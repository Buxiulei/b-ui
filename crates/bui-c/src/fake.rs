//! 内存执行器：[`FakeSys`] / [`FakeNet`]。单元测试全程只用它们，
//! 于是不 `systemctl`、不写 `/etc`、不发网络请求（Global Constraints）。
//!
//! 它不是 `#[cfg(test)]`——未来的集成测试（`tests/`）也要能拿到同一套 fake。

use crate::net::{Download, Net, Probe, ProbeError, Via};
use crate::sys::{Output, Sys};
use crate::{Error, Result};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use time::macros::datetime;

/// 内存执行器：记录调用、按 `"prog arg1 arg2"` 精确匹配预置输出。
/// 未登记的命令按 [`default_code`] 给退出码：查询类默认失败，变更类默认成功。
#[derive(Debug)]
pub struct FakeSys {
    files: RefCell<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
    calls: RefCell<Vec<String>>,
    replies: RefCell<BTreeMap<String, Output>>,
    envs: RefCell<BTreeMap<String, String>>,
    now: RefCell<time::OffsetDateTime>,
    sleeps: RefCell<Vec<u64>>,
    /// 终端尺寸 `(列, 行)`；默认 `None`，等于「stdout 不是终端」。
    term: Cell<Option<(u16, u16)>>,
    /// 注入写失败的路径（[`FakeSys::fail_write`]）。
    write_fails: RefCell<BTreeSet<PathBuf>>,
    /// 每次 [`Sys::write`] 的目标路径，按顺序（失败的也记）。
    written: RefCell<Vec<PathBuf>>,
}

impl Default for FakeSys {
    fn default() -> Self {
        Self {
            files: RefCell::default(),
            calls: RefCell::default(),
            replies: RefCell::default(),
            envs: RefCell::default(),
            now: RefCell::new(datetime!(2026-09-11 00:00:00 UTC)),
            sleeps: RefCell::default(),
            term: Cell::new(None),
            write_fails: RefCell::default(),
            written: RefCell::default(),
        }
    }
}

fn key(prog: &str, args: &[&str]) -> String {
    if args.is_empty() {
        prog.to_string()
    } else {
        format!("{prog} {}", args.join(" "))
    }
}

/// 未登记命令的默认退出码。
///
/// 查询类**必须**默认失败：`systemctl is-active/is-enabled`、`ip … show`、`ufw status`、
/// `sysctl -n`、`sing-box version` 在没登记时表示「没起 / 不存在 / 没装 / 读不到」。
/// 若它们默认成功，测试里的「干净机器」会被实现读成「什么都装好了」——
/// `apply` 不去 enable timer、`uninstall` 在没装 ufw 的机器上撤规则、`status` 把没有的
/// TUN 报成 up、`import-v3` 把每台机器都判成本来在用 TUN。
///
/// 变更类默认成功（0）：`daemon-reload` / `enable` / `restart` / `stop` / `disable` /
/// `reset-failed` / `ip link delete` / `sysctl -w` / `ufw allow|delete` / `tar` /
/// `sing-box check`，省得每个测试逐条登记幂等动作。
pub fn default_code(prog: &str, args: &[&str]) -> i32 {
    let first = args.first().copied().unwrap_or("");
    match (prog, first) {
        ("systemctl", "is-active") => 3,
        ("systemctl", "is-enabled") => 1,
        ("ufw", "status") => 127,
        ("sysctl", "-n") => 1,
        ("ip", _) if args.contains(&"show") => 1,
        (p, "version") if p.ends_with("sing-box") => 1,
        _ => 0,
    }
}

impl FakeSys {
    pub fn new() -> Self {
        Self::default()
    }
    /// 登记一条命令的输出，key 是 `"prog arg1 arg2"`。显式登记永远盖过 [`default_code`]。
    pub fn reply(&self, cmd: &str, code: i32, stdout: &str) {
        self.replies.borrow_mut().insert(
            cmd.to_string(),
            Output {
                code,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        );
    }
    /// 预置一个文件（权限按 0644，模拟外部落下的文件）。
    pub fn put(&self, path: &str, data: &str) {
        self.files
            .borrow_mut()
            .insert(PathBuf::from(path), (data.as_bytes().to_vec(), 0o644));
    }
    pub fn get(&self, path: &str) -> Option<String> {
        self.files
            .borrow()
            .get(Path::new(path))
            .map(|(d, _)| String::from_utf8_lossy(d).into_owned())
    }
    pub fn mode(&self, path: &str) -> Option<u32> {
        self.files.borrow().get(Path::new(path)).map(|(_, m)| *m)
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
    pub fn called(&self, cmd: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == cmd)
    }
    /// 虚拟时间前进，用于测退避与「今天该不该自更新」。
    pub fn advance(&self, seconds: i64) {
        let mut n = self.now.borrow_mut();
        *n += time::Duration::seconds(seconds);
    }
    /// 记录到的 [`Sys::sleep`] 毫秒数。
    pub fn sleeps(&self) -> Vec<u64> {
        self.sleeps.borrow().clone()
    }
    /// 注入环境变量，不碰进程环境（决策 11）。
    pub fn set_env(&self, key: &str, value: &str) {
        self.envs
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
    }
    /// 注入终端尺寸 `(列, 行)`。默认 `None`（不是终端）：没注入的测试按 80×24 排版、
    /// 不清屏，与加这个能力之前的输出一致。
    pub fn set_term_size(&self, v: Option<(u16, u16)>) {
        self.term.set(v);
    }
    /// 注入写失败：之后对这个路径的 [`Sys::write`] 一律返回 `Err`（磁盘满、只读文件系统）。
    /// 删除流程的回滚分支靠它测：写 `config.json` 或 `profiles.json` 失败时要换回原配置。
    pub fn fail_write(&self, path: &str) {
        self.write_fails.borrow_mut().insert(PathBuf::from(path));
    }
    /// 对这个路径调用过几次 [`Sys::write`]（失败的也算）。用来钉住「profiles 与 active
    /// 同一次 save」这类断言。
    pub fn writes(&self, path: &str) -> usize {
        let want = PathBuf::from(path);
        self.written.borrow().iter().filter(|p| **p == want).count()
    }
}

impl Sys for FakeSys {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output> {
        let k = key(prog, args);
        self.calls.borrow_mut().push(k.clone());
        Ok(self
            .replies
            .borrow()
            .get(&k)
            .cloned()
            .unwrap_or_else(|| Output {
                code: default_code(prog, args),
                stdout: String::new(),
                stderr: String::new(),
            }))
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.files
            .borrow()
            .get(path)
            .map(|(d, _)| d.clone())
            .ok_or_else(|| Error::io(path, std::io::Error::from(std::io::ErrorKind::NotFound)))
    }
    fn write(&self, path: &Path, data: &[u8], mode: u32) -> Result<()> {
        self.written.borrow_mut().push(path.to_path_buf());
        if self.write_fails.borrow().contains(path) {
            return Err(Error::io(
                path,
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        }
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), (data.to_vec(), mode));
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let v = self.files.borrow_mut().remove(from);
        match v {
            Some(x) => {
                self.files.borrow_mut().insert(to.to_path_buf(), x);
                Ok(())
            }
            None => Err(Error::io(
                from,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            )),
        }
    }
    fn remove_file(&self, path: &Path) -> Result<()> {
        self.files.borrow_mut().remove(path);
        Ok(())
    }
    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        self.files.borrow_mut().retain(|k, _| !k.starts_with(path));
        Ok(())
    }
    fn mkdir_p(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn exists(&self, path: &Path) -> bool {
        let f = self.files.borrow();
        f.contains_key(path) || f.keys().any(|k| k.starts_with(path) && k != path)
    }
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = self
            .files
            .borrow()
            .keys()
            .filter_map(|k| {
                k.strip_prefix(path)
                    .ok()
                    .and_then(|r| r.iter().next())
                    .map(|first| path.join(first))
            })
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
    fn now(&self) -> time::OffsetDateTime {
        *self.now.borrow()
    }
    fn sleep(&self, d: Duration) {
        self.sleeps.borrow_mut().push(d.as_millis() as u64)
    }
    fn env(&self, key: &str) -> Option<String> {
        self.envs.borrow().get(key).cloned()
    }
    fn term_size(&self) -> Option<(u16, u16)> {
        self.term.get()
    }
}

/// 一个 URL 的预置响应。`Timeout` / `Refused` / `Dns` 模拟三种网络失败：
/// [`Net::probe`] 给出对应的 [`ProbeError`]，其余方法一律报错。
#[derive(Debug, Clone)]
pub enum FakeReply {
    Status(u16),
    Text(String),
    Bytes(Vec<u8>),
    Fail(String),
    Timeout,
    Refused,
    Dns,
}

/// 内存 HTTP 客户端：按 URL 精确匹配，未登记的 URL 报错（等价「不可达」）。
/// 内部用 `Mutex` 不用 `RefCell`：`Net: Sync`，测速会在 `std::thread::scope` 里并发调用。
#[derive(Debug, Default)]
pub struct FakeNet {
    routes: Mutex<BTreeMap<String, FakeReply>>,
    delays: Mutex<BTreeMap<String, u64>>,
    log: Mutex<Vec<String>>,
}

/// 拿锁；别的测试线程 panic 过也照样拿（里面只是登记表与流水，不怕半截状态）。
fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 非正文的响应在要正文的方法里报的错，URL 照真实实现脱敏。
fn failure(url: &str, reply: FakeReply) -> Error {
    let detail = match reply {
        FakeReply::Status(c) => format!("HTTP {c}"),
        FakeReply::Fail(m) => m,
        FakeReply::Timeout => "超时".into(),
        FakeReply::Refused => "连接被拒绝".into(),
        FakeReply::Dns => "域名解析失败".into(),
        FakeReply::Text(_) | FakeReply::Bytes(_) => unreachable!("有正文的响应不算失败"),
    };
    Error::Net {
        url: crate::error::redact_url(url),
        detail,
    }
}

impl FakeNet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn route(&self, url: &str, reply: FakeReply) {
        locked(&self.routes).insert(url.to_string(), reply);
    }
    /// 登记这个 URL 的用时（毫秒），决定 `probe` / `download_via` 带回的 `elapsed`，
    /// 不登记就是 0。只报这个数，不真的睡。
    pub fn delay(&self, url: &str, ms: u64) {
        locked(&self.delays).insert(url.to_string(), ms);
    }
    /// 请求流水：`"GET <url> via <Direct|socks5:1080>"`。
    pub fn log(&self) -> Vec<String> {
        locked(&self.log).clone()
    }
    fn hit(&self, url: &str, via: Via) -> Result<FakeReply> {
        let v = match via {
            Via::Direct => "Direct".to_string(),
            Via::Socks5 { port } => format!("socks5:{port}"),
        };
        locked(&self.log).push(format!("GET {url} via {v}"));
        locked(&self.routes)
            .get(url)
            .cloned()
            .ok_or_else(|| Error::Net {
                url: crate::error::redact_url(url),
                detail: "fake 未登记该 URL".into(),
            })
    }
    fn elapsed(&self, url: &str) -> Duration {
        Duration::from_millis(locked(&self.delays).get(url).copied().unwrap_or(0))
    }
}

impl Net for FakeNet {
    fn status(&self, url: &str, via: Via, _t: Duration) -> Result<u16> {
        match self.hit(url, via)? {
            FakeReply::Status(c) => Ok(c),
            FakeReply::Text(_) | FakeReply::Bytes(_) => Ok(200),
            other => Err(failure(url, other)),
        }
    }
    fn text(&self, url: &str, t: Duration) -> Result<String> {
        self.text_via(url, Via::Direct, t)
    }
    fn bytes(&self, url: &str, _t: Duration) -> Result<Vec<u8>> {
        match self.hit(url, Via::Direct)? {
            FakeReply::Bytes(b) => Ok(b),
            FakeReply::Text(s) => Ok(s.into_bytes()),
            other => Err(failure(url, other)),
        }
    }
    fn text_via(&self, url: &str, via: Via, _t: Duration) -> Result<String> {
        match self.hit(url, via)? {
            FakeReply::Text(s) => Ok(s),
            FakeReply::Bytes(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
            other => Err(failure(url, other)),
        }
    }
    /// `Status(n)` 给 `code = n`，有正文的给 200；`Fail(m)` 给 `Other(m)`，
    /// 未登记的 URL 给 `Other("other")`。`delay` 不短于时限时一律超时：真实实现到了
    /// 时限只会报超时，不会带回那么长的 elapsed。
    fn probe(&self, url: &str, via: Via, t: Duration) -> std::result::Result<Probe, ProbeError> {
        let reply = self
            .hit(url, via)
            .map_err(|_| ProbeError::Other("other".into()))?;
        let elapsed = self.elapsed(url);
        if elapsed >= t {
            return Err(ProbeError::Timeout);
        }
        match reply {
            FakeReply::Status(code) => Ok(Probe { code, elapsed }),
            FakeReply::Text(_) | FakeReply::Bytes(_) => Ok(Probe { code: 200, elapsed }),
            FakeReply::Timeout => Err(ProbeError::Timeout),
            FakeReply::Refused => Err(ProbeError::Refused),
            FakeReply::Dns => Err(ProbeError::Dns),
            FakeReply::Fail(m) => Err(ProbeError::Other(m)),
        }
    }
    /// 读到的字节 = min(正文长度, `max_bytes`)，读满才算 `complete`；
    /// `elapsed` 截到 `cap` 为止（真实实现 cap 到点就停）。
    fn download_via(&self, url: &str, via: Via, max_bytes: u64, cap: Duration) -> Result<Download> {
        let len = match self.hit(url, via)? {
            FakeReply::Bytes(b) => b.len() as u64,
            FakeReply::Text(s) => s.len() as u64,
            other => return Err(failure(url, other)),
        };
        let bytes = len.min(max_bytes);
        Ok(Download {
            bytes,
            elapsed: self.elapsed(url).min(cap),
            complete: bytes >= max_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Net, ProbeError, Via};
    use crate::sys::{systemd, Sys};
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn fake_sys_records_commands_and_returns_canned_output() {
        let s = FakeSys::new();
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("ip link show bui-tun", 1, "");
        assert!(systemd::is_active(&s, "bui-c.service"));
        assert_eq!(s.run("ip", &["link", "show", "bui-tun"]).unwrap().code, 1);
        // 未登记的「变更类」命令默认成功、空输出（幂等的 systemctl 收尾动作不必逐条登记）
        assert!(s.run("systemctl", &["daemon-reload"]).unwrap().ok());
        assert!(s.called("systemctl daemon-reload"));
        assert_eq!(s.calls().len(), 3);
    }

    #[test]
    fn injected_write_failures_keep_the_old_bytes_and_still_count() {
        let s = FakeSys::new();
        let p = Path::new("/opt/bui-c/profiles.json");
        s.write(p, b"old", 0o600).unwrap();
        s.fail_write("/opt/bui-c/profiles.json");
        let e = s.write(p, b"new", 0o600).unwrap_err();
        assert!(e.to_string().contains("profiles.json"), "{e}");
        assert_eq!(s.read(p).unwrap(), b"old", "写失败不能改动原文件");
        assert_eq!(s.writes("/opt/bui-c/profiles.json"), 2, "失败的也算一次");
        assert_eq!(s.writes("/opt/bui-c/config.json"), 0);
        // 只影响登记过的那个路径
        s.write(Path::new("/opt/bui-c/config.json"), b"{}", 0o600)
            .unwrap();
    }

    #[test]
    fn fake_sys_query_commands_default_to_failure() {
        // 「未登记」必须等于「没起 / 不存在 / 没装」，不能等于「一切正常」：
        // 否则 apply 会以为 timer 已经 enable 过（不去 enable），uninstall 会在干净机器上
        // 以为装了 ufw 去撤规则，status 会把没有的 TUN 报成 up。
        let s = FakeSys::new();
        assert!(!systemd::is_active(&s, "bui-c.timer"));
        assert!(!systemd::is_enabled(&s, "bui-c.timer"));
        assert_eq!(s.run("ip", &["link", "show", "bui-tun"]).unwrap().code, 1);
        assert_eq!(
            s.run("ip", &["-4", "route", "show", "table", "all"])
                .unwrap()
                .code,
            1
        );
        assert_eq!(
            s.run("ip", &["-6", "route", "show", "default"])
                .unwrap()
                .code,
            1
        );
        assert_eq!(s.run("ufw", &["status"]).unwrap().code, 127);
        assert_eq!(
            s.run("sysctl", &["-n", "net.ipv6.conf.all.disable_ipv6"])
                .unwrap()
                .code,
            1
        );
        assert_eq!(
            s.run("/opt/bui-c/bin/sing-box", &["version"]).unwrap().code,
            1
        );
        // 变更类仍然默认成功
        assert!(s.run("ip", &["link", "delete", "bui-tun"]).unwrap().ok());
        assert!(s
            .run("sysctl", &["-w", "net.ipv4.ip_forward=1"])
            .unwrap()
            .ok());
        assert!(s
            .run("ufw", &["allow", "in", "on", "bui-tun"])
            .unwrap()
            .ok());
        assert!(s
            .run("/opt/bui-c/bin/sing-box", &["check", "-c", "/x.json"])
            .unwrap()
            .ok());
        // 显式登记盖过默认值
        s.reply("ufw status", 0, "Status: active\n");
        assert_eq!(
            s.run("ufw", &["status"]).unwrap().stdout,
            "Status: active\n"
        );
    }

    #[test]
    fn fake_sys_write_keeps_mode_and_read_back() {
        let s = FakeSys::new();
        s.write(Path::new("/opt/bui-c/profiles.json"), b"{}", 0o600)
            .unwrap();
        assert_eq!(s.get("/opt/bui-c/profiles.json").unwrap(), "{}");
        assert_eq!(s.mode("/opt/bui-c/profiles.json"), Some(0o600));
        assert!(s.exists(Path::new("/opt/bui-c/profiles.json")));
        assert!(s.exists(Path::new("/opt/bui-c")), "父目录视为存在");
        assert!(!s.exists(Path::new("/opt/bui-c/config.json")));
        s.remove_file(Path::new("/opt/bui-c/profiles.json"))
            .unwrap();
        assert!(!s.exists(Path::new("/opt/bui-c/profiles.json")));
        s.remove_file(Path::new("/nope")).unwrap(); // 不存在也 Ok
    }

    #[test]
    fn fake_sys_read_dir_lists_direct_children_sorted() {
        let s = FakeSys::new();
        s.put("/v3/configs/b/uri.txt", "x");
        s.put("/v3/configs/a/uri.txt", "y");
        s.put("/v3/active", "a");
        let got: Vec<String> = s
            .read_dir(Path::new("/v3/configs"))
            .unwrap()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            got,
            vec!["/v3/configs/a".to_string(), "/v3/configs/b".to_string()]
        );
    }

    #[test]
    fn fake_sys_time_and_sleep_are_virtual() {
        let s = FakeSys::new();
        let t0 = s.now();
        s.sleep(Duration::from_millis(500));
        s.advance(60);
        assert_eq!((s.now() - t0).whole_seconds(), 60);
        assert_eq!(s.sleeps(), vec![500]);
    }

    #[test]
    fn fake_sys_env_is_injected_not_read_from_the_process() {
        let s = FakeSys::new();
        assert_eq!(s.env("BUI_FORCE_IPV6"), None);
        s.set_env("BUI_FORCE_IPV6", "1");
        assert_eq!(s.env("BUI_FORCE_IPV6").as_deref(), Some("1"));
        s.set_env("BUI_FORCE_IPV6", "0");
        assert_eq!(s.env("BUI_FORCE_IPV6").as_deref(), Some("0"));
    }

    #[test]
    fn fake_net_routes_and_logs() {
        let n = FakeNet::new();
        n.route(
            "https://www.gstatic.com/generate_204",
            FakeReply::Status(204),
        );
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text("{}".into()),
        );
        n.route(
            "https://panel.example.com/x",
            FakeReply::Fail("timeout".into()),
        );
        assert_eq!(
            n.status(
                "https://www.gstatic.com/generate_204",
                Via::Socks5 { port: 1080 },
                Duration::from_secs(5)
            )
            .unwrap(),
            204
        );
        assert_eq!(
            n.text(
                "https://panel.example.com/packages/manifest.json",
                Duration::from_secs(5)
            )
            .unwrap(),
            "{}"
        );
        assert!(n
            .bytes("https://panel.example.com/x", Duration::from_secs(5))
            .is_err());
        assert!(n
            .status(
                "https://unknown.example.com/",
                Via::Direct,
                Duration::from_secs(1)
            )
            .is_err());
        assert_eq!(n.log().len(), 4);
        assert_eq!(
            n.log()[0],
            "GET https://www.gstatic.com/generate_204 via socks5:1080"
        );
    }

    #[test]
    fn fake_probe_reports_code_elapsed_and_failure_kinds() {
        let n = FakeNet::new();
        n.route("https://a.example.com/", FakeReply::Status(204));
        n.delay("https://a.example.com/", 312);
        let p = n
            .probe(
                "https://a.example.com/",
                Via::Direct,
                Duration::from_secs(8),
            )
            .unwrap();
        assert_eq!((p.code, p.elapsed.as_millis()), (204, 312));
        n.route("https://b.example.com/", FakeReply::Dns);
        assert_eq!(
            n.probe(
                "https://b.example.com/",
                Via::Direct,
                Duration::from_secs(1)
            ),
            Err(ProbeError::Dns)
        );
        n.route("https://c.example.com/", FakeReply::Bytes(vec![0; 600_000]));
        let d = n
            .download_via(
                "https://c.example.com/",
                Via::Socks5 { port: 1080 },
                1_000_000,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!((d.bytes, d.complete), (600_000, false));
        assert!(n
            .log()
            .iter()
            .any(|l| l == "GET https://c.example.com/ via socks5:1080"));
    }

    #[test]
    fn fake_net_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FakeNet>();
    }

    #[test]
    fn fake_probe_times_out_once_the_delay_reaches_the_timeout() {
        // 真实实现里，用时到了时限只会报超时，不会带回一个不短于时限的 elapsed
        let n = FakeNet::new();
        let url = "https://slow.example.com/";
        n.route(url, FakeReply::Status(204));
        n.delay(url, 6_000);
        assert_eq!(
            n.probe(url, Via::Direct, Duration::from_secs(6)),
            Err(ProbeError::Timeout)
        );
        let p = n
            .probe(url, Via::Direct, Duration::from_millis(6_001))
            .unwrap();
        assert_eq!((p.code, p.elapsed.as_millis()), (204, 6_000));
    }

    #[test]
    fn fake_download_elapsed_stops_at_the_cap() {
        // 真实实现 cap 到点就停，elapsed 不会超过 cap
        let n = FakeNet::new();
        let url = "https://speed.example.com/";
        n.route(url, FakeReply::Bytes(vec![0; 400_000]));
        n.delay(url, 9_000);
        let d = n
            .download_via(url, Via::Direct, 1_000_000, Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            (d.bytes, d.complete, d.elapsed),
            (400_000, false, Duration::from_secs(5))
        );
    }

    #[test]
    fn fake_new_methods_follow_the_canned_reply() {
        let n = FakeNet::new();
        let t = Duration::from_secs(5);
        // text_via 记下走的哪条腿；没设 delay 的 elapsed 是 0
        n.route(
            "https://ip.example.com/",
            FakeReply::Text("203.0.113.7".into()),
        );
        assert_eq!(
            n.text_via("https://ip.example.com/", Via::Socks5 { port: 1080 }, t)
                .unwrap(),
            "203.0.113.7"
        );
        assert_eq!(
            n.log().last().unwrap(),
            "GET https://ip.example.com/ via socks5:1080"
        );
        let p = n.probe("https://ip.example.com/", Via::Direct, t).unwrap();
        assert_eq!((p.code, p.elapsed), (200, Duration::ZERO));
        // 正文超过上限：只算到上限，算读满
        n.route("https://d.example.com/", FakeReply::Bytes(vec![0; 2_000]));
        n.delay("https://d.example.com/", 900);
        let d = n
            .download_via("https://d.example.com/", Via::Direct, 1_000, t)
            .unwrap();
        assert_eq!(
            (d.bytes, d.complete, d.elapsed.as_millis()),
            (1_000, true, 900)
        );
        // 三种网络失败：probe 给对应类别，其余方法报错，错误里没有路径末段
        n.route("https://e.example.com/api/nodes/alice", FakeReply::Timeout);
        n.route("https://f.example.com/api/nodes/alice", FakeReply::Refused);
        let e = "https://e.example.com/api/nodes/alice";
        let f = "https://f.example.com/api/nodes/alice";
        assert_eq!(n.probe(e, Via::Direct, t), Err(ProbeError::Timeout));
        assert_eq!(n.probe(f, Via::Direct, t), Err(ProbeError::Refused));
        for msg in [
            n.download_via(f, Via::Direct, 1_000, t)
                .unwrap_err()
                .to_string(),
            n.text_via(e, Via::Direct, t).unwrap_err().to_string(),
        ] {
            assert!(!msg.contains("alice"), "错误里带出了用户名：{msg}");
        }
    }
}
