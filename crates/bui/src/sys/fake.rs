//! 内存实现（只在 test 构建里存在）：文件系统、单元表、sysctl、内核模块、监听端口、
//! 时钟全在一个 `Mutex<FakeInner>` 里，每个改动操作都往 `ops` 里记一条可断言的流水。
//!
//! `ops` 的字符串格式是后续任务断言的契约，**不得更改**：
//! `write:<path>:<mode 八进制三位>`、`remove:<path>`、`rmdir:<path>`、
//! `symlink:<link>-><target>`、`chattr:+i:<path>` / `chattr:-i:<path>`、`daemon-reload`、
//! `systemd:<verb>:<unit>`、`sysctl:<key>=<value>`、`modprobe:<module>`、
//! `run:<program> <args 以空格连接>`（`run_stdin` 同格式，stdin 另记在 `stdins` 里）、
//! `journal:<units 以逗号连接>:cursor=<c>` /
//! `journal:<units>:since=<RFC3339>`。

use super::{cmd_line, unit_full, CmdOut, Host, JournalFrom, JournalRecord, Proto};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct FakeInner {
    pub files: BTreeMap<PathBuf, (Vec<u8>, u32)>,
    pub immutable: BTreeSet<PathBuf>,
    /// 符号链接：link → target
    pub symlinks: BTreeMap<PathBuf, PathBuf>,
    /// 显式存在的空目录（有文件的目录由 files 的路径前缀隐式存在）
    pub dirs: BTreeSet<PathBuf>,
    pub units_active: BTreeSet<String>,
    pub units_enabled: BTreeSet<String>,
    pub units_exist: BTreeSet<String>,
    /// 键是 `(单元**全名**, 属性名)`，如 `("hysteria-server.service", "NRestarts")`
    pub unit_props: BTreeMap<(String, String), String>,
    pub sysctl: BTreeMap<String, String>,
    /// 「这个键被内核钳制/重排」：`sysctl_set` 之后读回的就是这里的值，不是写入值
    pub sysctl_clamp: BTreeMap<String, String>,
    pub which: BTreeSet<String>,
    pub modules: BTreeSet<String>,
    /// 前缀匹配的脚本化命令结果："xray run -test" → CmdOut
    pub scripted: Vec<(String, CmdOut)>,
    /// 令某单元的 systemd 动作失败（测回滚）
    pub fail_units: BTreeSet<String>,
    /// 令某个路径的 `write_file` 失败（盘满 / 只读挂载 / 目录建不出来）：真实机器上写盘是会
    /// 失败的，而对账把「带 restart 的 WriteFile 写失败」也算作搁置该单元的理由
    /// （`reconcile::apply` 第 1 步），不给假机器造出写失败就没法钉住那一半。
    pub fail_writes: BTreeSet<PathBuf>,
    /// 令某单元**永远不 active**：`systemctl start/restart` 照样退 0，单元却起不来
    /// （203/EXEC、start-limit-hit 的真实形态）。裸名与全名两种键各查一次。
    pub never_active: BTreeSet<String>,
    pub listening: BTreeMap<Proto, BTreeSet<u16>>,
    pub mem_mb: u64,
    pub arch: String,
    pub hostname: String,
    pub now: time::OffsetDateTime,
    /// `journal_read` 的脚本化返回，按调用顺序逐个弹出；弹空后返回 `Ok(vec![])`。
    /// `Err(文案)` 模拟 journalctl 失败（游标失效 / 没装）。
    pub journal: std::collections::VecDeque<Result<Vec<JournalRecord>, String>>,
    /// `unit_property` 的查询流水（单元全名, 属性名）：纯查询不进 `ops`，
    /// 「一次 systemd 都没查」这类断言靠它证明（`bui hy2-prestart` 的启动关键路径）。
    pub unit_prop_reads: Vec<(String, String)>,
    /// `run_stdin` 的载荷流水（命令行, stdin）：`nft -f -` 喂进去的规则集按它断言。
    pub stdins: Vec<(String, String)>,
    /// `file_sha256` 的查询流水：纯查询不进 `ops`，「二进制身份是流式算的、没被整文件读进
    /// 内存」这类断言靠它证明（`b-ui.service` 的 `MemoryMax=200M` 是硬上限）。
    pub sha_reads: Vec<PathBuf>,
    /// 操作流水（顺序可断言）
    pub ops: Vec<String>,
}

// `time::OffsetDateTime` 没有 `Default`，所以 `FakeInner` 的 `Default` 手写（计划里写的是
// `#[derive(Default)]`，derive 编不过）；默认的机器事实与假时钟一并在这里定下。
impl Default for FakeInner {
    fn default() -> Self {
        Self {
            files: BTreeMap::new(),
            immutable: BTreeSet::new(),
            symlinks: BTreeMap::new(),
            dirs: BTreeSet::new(),
            units_active: BTreeSet::new(),
            units_enabled: BTreeSet::new(),
            units_exist: BTreeSet::new(),
            unit_props: BTreeMap::new(),
            sysctl: BTreeMap::new(),
            sysctl_clamp: BTreeMap::new(),
            which: BTreeSet::new(),
            modules: BTreeSet::new(),
            scripted: Vec::new(),
            fail_units: BTreeSet::new(),
            fail_writes: BTreeSet::new(),
            never_active: BTreeSet::new(),
            listening: BTreeMap::new(),
            mem_mb: 2048,
            arch: "x86_64".into(),
            hostname: "node-a".into(),
            now: time::macros::datetime!(2026-09-11 00:00:00 UTC),
            journal: std::collections::VecDeque::new(),
            unit_prop_reads: Vec::new(),
            stdins: Vec::new(),
            sha_reads: Vec::new(),
            ops: Vec::new(),
        }
    }
}

pub struct FakeHost {
    inner: Mutex<FakeInner>,
}

impl FakeHost {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FakeInner::default()),
        }
    }

    /// 播种：`h.with(|i| { i.units_active.insert("xray.service".into()); })`。
    pub fn with(&self, f: impl FnOnce(&mut FakeInner)) -> &Self {
        f(&mut self.lock());
        self
    }

    pub fn ops(&self) -> Vec<String> {
        self.lock().ops.clone()
    }

    pub fn clear_ops(&self) {
        self.lock().ops.clear();
    }

    /// 读回 `unit_property` 被查过的 (单元全名, 属性名)：断言「一次都没查」用。
    pub fn unit_prop_reads(&self) -> Vec<(String, String)> {
        self.lock().unit_prop_reads.clone()
    }

    /// 读回 [`Host::run_stdin`] 的载荷（命令行, stdin）。
    pub fn stdins(&self) -> Vec<(String, String)> {
        self.lock().stdins.clone()
    }

    /// 读回 [`Host::file_sha256`] 查过的路径：断言「二进制的 sha 是流式算的」用。
    pub fn sha_reads(&self) -> Vec<PathBuf> {
        self.lock().sha_reads.clone()
    }

    /// 读回写入的文件内容。
    pub fn text(&self, path: &str) -> Option<String> {
        self.lock()
            .files
            .get(Path::new(path))
            .map(|(c, _)| String::from_utf8_lossy(c).into_owned())
    }

    pub fn mode(&self, path: &str) -> Option<u32> {
        self.lock().files.get(Path::new(path)).map(|(_, m)| *m)
    }

    /// 推进假时钟。
    pub fn advance(&self, secs: i64) {
        let mut i = self.lock();
        i.now += time::Duration::seconds(secs);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("FakeHost 锁被毒化")
    }

    fn push_op(&self, op: String) {
        self.lock().ops.push(op);
    }
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for FakeHost {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        Ok(self.lock().files.get(path).map(|(c, _)| c.clone()))
    }

    fn file_sha256(&self, path: &Path) -> Result<Option<String>> {
        use sha2::{Digest as _, Sha256};
        let mut i = self.lock();
        i.sha_reads.push(path.to_path_buf());
        Ok(i.files
            .get(path)
            .map(|(c, _)| hex::encode(Sha256::digest(c))))
    }

    fn write_file(&self, path: &Path, content: &[u8], mode: u32) -> Result<()> {
        let mut i = self.lock();
        if i.fail_writes.contains(path) {
            anyhow::bail!("写 {} 失败：假机器播了 fail_writes", path.display());
        }
        i.files.insert(path.to_path_buf(), (content.to_vec(), mode));
        i.ops.push(format!("write:{}:{:03o}", path.display(), mode));
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let mut i = self.lock();
        i.files.remove(path);
        // 真实 `remove_file` 也能删掉符号链接本身
        i.symlinks.remove(path);
        i.ops.push(format!("remove:{}", path.display()));
        Ok(())
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let i = self.lock();
        let mut out: BTreeSet<PathBuf> = BTreeSet::new();
        for key in i.files.keys().chain(i.dirs.iter()) {
            let Ok(rest) = key.strip_prefix(path) else {
                continue;
            };
            // 只取第一段拼回 path：`/opt/b-ui/bin/xray` 在 `/opt/b-ui` 下只贡献 `bin`
            if let Some(first) = rest.components().next() {
                out.insert(path.join(first));
            }
        }
        Ok(out.into_iter().collect())
    }

    fn is_dir(&self, path: &Path) -> Result<bool> {
        let i = self.lock();
        // `/sys/module/<m>` 按 modules 集合回答：modprobe 之后第二轮对账判成已加载，幂等成立。
        if let Ok(rest) = path.strip_prefix("/sys/module") {
            let mut it = rest.components();
            if let (Some(m), None) = (it.next(), it.next()) {
                return Ok(i
                    .modules
                    .contains(&m.as_os_str().to_string_lossy().into_owned()));
            }
        }
        if i.dirs.contains(path) {
            return Ok(true);
        }
        Ok(i.files.keys().chain(i.dirs.iter()).any(|k| {
            k.strip_prefix(path)
                .is_ok_and(|rest| rest.iter().count() > 0)
        }))
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        let mut i = self.lock();
        i.files
            .retain(|k, _| k != path && k.strip_prefix(path).is_err());
        i.dirs
            .retain(|k| k != path && k.strip_prefix(path).is_err());
        i.ops.push(format!("rmdir:{}", path.display()));
        Ok(())
    }

    fn is_symlink(&self, path: &Path) -> Result<bool> {
        Ok(self.lock().symlinks.contains_key(path))
    }

    fn read_link(&self, path: &Path) -> Result<Option<PathBuf>> {
        Ok(self.lock().symlinks.get(path).cloned())
    }

    fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        let mut i = self.lock();
        i.symlinks.insert(link.to_path_buf(), target.to_path_buf());
        i.ops
            .push(format!("symlink:{}->{}", link.display(), target.display()));
        Ok(())
    }

    fn set_immutable(&self, path: &Path, on: bool) -> Result<()> {
        let mut i = self.lock();
        if on {
            i.immutable.insert(path.to_path_buf());
        } else {
            i.immutable.remove(path);
        }
        let flag = if on { "+i" } else { "-i" };
        i.ops.push(format!("chattr:{flag}:{}", path.display()));
        Ok(())
    }

    fn is_immutable(&self, path: &Path) -> Result<bool> {
        Ok(self.lock().immutable.contains(path))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut> {
        let line = cmd_line(program, args);
        self.push_op(format!("run:{line}"));
        let i = self.lock();
        for (prefix, out) in &i.scripted {
            if line.starts_with(prefix.as_str()) {
                return Ok(out.clone());
            }
        }
        Ok(CmdOut::success(""))
    }

    fn run_stdin(&self, program: &str, args: &[&str], stdin: &str) -> Result<CmdOut> {
        let line = cmd_line(program, args);
        self.push_op(format!("run:{line}"));
        let mut i = self.lock();
        i.stdins.push((line.clone(), stdin.to_string()));
        for (prefix, out) in &i.scripted {
            if line.starts_with(prefix.as_str()) {
                return Ok(out.clone());
            }
        }
        Ok(CmdOut::success(""))
    }

    fn which(&self, program: &str) -> bool {
        self.lock().which.contains(program)
    }

    fn systemd_daemon_reload(&self) -> Result<()> {
        self.push_op("daemon-reload".into());
        Ok(())
    }

    fn systemd(&self, verb: &str, unit: &str) -> Result<CmdOut> {
        let full = unit_full(unit);
        let bare = full.strip_suffix(".service").unwrap_or(&full).to_string();
        let mut i = self.lock();
        // ops 里记的是**传进来的原样名字**
        i.ops.push(format!("systemd:{verb}:{unit}"));
        // fail_units 裸名与全名两种键各查一次，任一命中即失败（Task 5 的回滚测试按裸名播种）
        if i.fail_units.contains(&full) || i.fail_units.contains(&bare) {
            return Ok(CmdOut::failure(1, "Job failed"));
        }
        // 语义跟真实 systemd 对齐：`disable` 不停服务，`stop` 不改 enable 状态。
        match verb {
            "start" | "restart" | "reload-or-restart" => {
                if !(i.never_active.contains(&full) || i.never_active.contains(&bare)) {
                    i.units_active.insert(full);
                }
            }
            "stop" => {
                i.units_active.remove(&full);
            }
            "enable" => {
                i.units_enabled.insert(full);
            }
            "disable" => {
                i.units_enabled.remove(&full);
            }
            _ => {}
        }
        Ok(CmdOut::success(""))
    }

    fn unit_is_active(&self, unit: &str) -> Result<bool> {
        Ok(self.lock().units_active.contains(&unit_full(unit)))
    }

    fn unit_is_enabled(&self, unit: &str) -> Result<bool> {
        Ok(self.lock().units_enabled.contains(&unit_full(unit)))
    }

    fn unit_exists(&self, unit: &str) -> Result<bool> {
        Ok(self.lock().units_exist.contains(&unit_full(unit)))
    }

    fn unit_property(&self, unit: &str, prop: &str) -> Result<Option<String>> {
        // unit_props **只认全名**：播成裸名查不到、返回 None（`fail_units` 的宽容不适用于它）。
        let key = (unit_full(unit), prop.to_string());
        let mut i = self.lock();
        i.unit_prop_reads.push(key.clone());
        Ok(i.unit_props.get(&key).cloned())
    }

    fn sysctl_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.lock().sysctl.get(key).cloned())
    }

    fn sysctl_set(&self, key: &str, value: &str) -> Result<()> {
        let mut i = self.lock();
        // 真机语义：`sysctl -w` 写进去的是空格分隔，但 `/proc` 里多值键存的是**制表符**分隔，
        // 下一轮 `sysctl -n` 原样读回制表符（`4096\t262144\t16777216`）。假机器照抄这条，
        // 否则「二次对账零变更」在测试里恒绿、在真机上恒红（bwg-rick M1 step2）。
        let stored = match i.sysctl_clamp.get(key) {
            // 内核钳制/重排：写什么都读回这个值
            Some(clamped) => clamped.clone(),
            None => value
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join("\t"),
        };
        i.sysctl.insert(key.to_string(), stored);
        i.ops.push(format!("sysctl:{key}={value}"));
        Ok(())
    }

    fn modprobe(&self, module: &str) -> Result<()> {
        let mut i = self.lock();
        i.modules.insert(module.to_string());
        i.ops.push(format!("modprobe:{module}"));
        Ok(())
    }

    fn mem_mb(&self) -> Result<u64> {
        Ok(self.lock().mem_mb)
    }

    fn arch(&self) -> Result<String> {
        Ok(self.lock().arch.clone())
    }

    fn hostname(&self) -> Result<String> {
        Ok(self.lock().hostname.clone())
    }

    fn listening_ports(&self, proto: Proto) -> Result<BTreeSet<u16>> {
        Ok(self
            .lock()
            .listening
            .get(&proto)
            .cloned()
            .unwrap_or_default())
    }

    fn now(&self) -> time::OffsetDateTime {
        self.lock().now
    }

    fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>> {
        let from = match from {
            JournalFrom::Cursor(c) => format!("cursor={c}"),
            JournalFrom::Since(t) => format!("since={}", crate::util::fmt_rfc3339(*t)),
        };
        let mut i = self.lock();
        i.ops.push(format!("journal:{}:{from}", units.join(",")));
        match i.journal.pop_front() {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(anyhow::anyhow!(e)),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Host;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[test]
    fn records_file_writes_with_mode() {
        let h = FakeHost::new();
        h.write_file(Path::new("/opt/b-ui/config.yaml"), b"listen: :10000", 0o600)
            .unwrap();
        assert_eq!(h.text("/opt/b-ui/config.yaml").unwrap(), "listen: :10000");
        assert_eq!(h.mode("/opt/b-ui/config.yaml"), Some(0o600));
        assert_eq!(h.ops(), vec!["write:/opt/b-ui/config.yaml:600"]);
        assert_eq!(h.read_file(Path::new("/nope")).unwrap(), None);
    }

    #[test]
    fn records_systemd_and_sysctl_ops_in_order() {
        let h = FakeHost::new();
        h.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.sysctl.insert("net.ipv4.tcp_retries2".into(), "15".into());
        });
        assert!(h.unit_is_active("hysteria-server").unwrap());
        h.systemd_daemon_reload().unwrap();
        h.systemd("restart", "hysteria-server").unwrap();
        h.sysctl_set("net.ipv4.tcp_retries2", "8").unwrap();
        assert_eq!(
            h.sysctl_get("net.ipv4.tcp_retries2").unwrap().as_deref(),
            Some("8")
        );
        assert_eq!(
            h.ops(),
            vec![
                "daemon-reload",
                "systemd:restart:hysteria-server",
                "sysctl:net.ipv4.tcp_retries2=8"
            ]
        );
    }

    #[test]
    fn multi_value_sysctl_reads_back_tab_separated_and_clamps_win() {
        // 真机行为：`sysctl -w net.ipv4.tcp_rmem="4096 262144 16777216"` 之后
        // `sysctl -n net.ipv4.tcp_rmem` 输出的是制表符分隔；被内核钳制的键读回的还不是写入值。
        let h = FakeHost::new();
        h.sysctl_set("net.ipv4.tcp_rmem", "4096 262144 16777216")
            .unwrap();
        assert_eq!(
            h.sysctl_get("net.ipv4.tcp_rmem").unwrap().as_deref(),
            Some("4096\t262144\t16777216")
        );
        h.with(|i| {
            i.sysctl_clamp
                .insert("net.ipv4.udp_mem".into(), "8192\t524288\t1048576".into());
        });
        h.sysctl_set("net.ipv4.udp_mem", "262144 524288 1048576")
            .unwrap();
        assert_eq!(
            h.sysctl_get("net.ipv4.udp_mem").unwrap().as_deref(),
            Some("8192\t524288\t1048576")
        );
    }

    #[test]
    fn scripted_commands_and_failures() {
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push((
                "xray run -test".into(),
                CmdOut::failure(1, "invalid config"),
            ));
            i.fail_units.insert("b-ui-relay".into());
        });
        let out = h
            .run(
                "xray",
                &["run", "-test", "-c", "/opt/b-ui/.verify/xray-config.json"],
            )
            .unwrap();
        assert!(!out.ok());
        assert_eq!(out.stderr, "invalid config");
        // 未脚本化的命令默认成功
        assert!(h.run("sshd", &["-t"]).unwrap().ok());
        assert!(!h.systemd("restart", "b-ui-relay").unwrap().ok());
        assert!(h.systemd("restart", "xray").unwrap().ok());
    }

    #[test]
    fn never_active_units_stay_down_even_though_restart_returns_zero() {
        let h = FakeHost::new();
        h.with(|i| {
            i.never_active.insert("caddy".into());
        });
        assert!(
            h.systemd("restart", "caddy").unwrap().ok(),
            "退出码照样是 0"
        );
        assert!(!h.unit_is_active("caddy").unwrap(), "单元其实没起来");
        assert!(h.systemd("restart", "xray").unwrap().ok());
        assert!(h.unit_is_active("xray").unwrap());
    }

    #[test]
    fn immutable_flag_and_clock() {
        let h = FakeHost::new();
        let p = Path::new("/etc/resolv.conf");
        assert!(!h.is_immutable(p).unwrap());
        h.set_immutable(p, true).unwrap();
        assert!(h.is_immutable(p).unwrap());
        let t0 = h.now();
        h.advance(600);
        assert_eq!((h.now() - t0).whole_seconds(), 600);
        assert_eq!(h.ops(), vec!["chattr:+i:/etc/resolv.conf"]);
    }

    #[test]
    fn list_dir_returns_only_direct_children_including_implicit_dirs() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files
                .insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert(
                "/opt/b-ui/admin/node_modules/x/index.js".into(),
                (b"x".to_vec(), 0o644),
            );
            i.dirs.insert("/opt/b-ui/certs".into());
        });
        assert_eq!(
            h.list_dir(Path::new("/opt/b-ui")).unwrap(),
            vec![
                std::path::PathBuf::from("/opt/b-ui/admin"),
                std::path::PathBuf::from("/opt/b-ui/bin"),
                std::path::PathBuf::from("/opt/b-ui/certs"),
                std::path::PathBuf::from("/opt/b-ui/state.json"),
            ]
        );
        assert!(h.is_dir(Path::new("/opt/b-ui/bin")).unwrap());
        assert!(h.is_dir(Path::new("/opt/b-ui/certs")).unwrap());
        assert!(!h.is_dir(Path::new("/opt/b-ui/state.json")).unwrap());
        assert_eq!(
            h.list_dir(Path::new("/nope")).unwrap(),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[test]
    fn remove_dir_all_takes_the_whole_subtree() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/admin/server.js".into(),
                (b"node".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/admin/node_modules/x/index.js".into(),
                (b"x".to_vec(), 0o644),
            );
            i.files
                .insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
        });
        h.clear_ops();
        h.remove_dir_all(Path::new("/opt/b-ui/admin")).unwrap();
        assert!(h.text("/opt/b-ui/admin/node_modules/x/index.js").is_none());
        assert!(h.text("/opt/b-ui/state.json").is_some(), "只删指定子树");
        assert_eq!(h.ops(), vec!["rmdir:/opt/b-ui/admin"]);
    }

    #[test]
    fn symlinks_are_readable_and_replaceable() {
        let h = FakeHost::new();
        let link = Path::new("/usr/local/bin/b-ui");
        assert_eq!(h.read_link(link).unwrap(), None);
        h.symlink(Path::new("/opt/b-ui/bin/bui"), link).unwrap();
        assert_eq!(
            h.read_link(link).unwrap(),
            Some(std::path::PathBuf::from("/opt/b-ui/bin/bui"))
        );
        assert!(h.is_symlink(link).unwrap());
        h.symlink(Path::new("/opt/b-ui/bin/bui2"), link).unwrap();
        assert_eq!(
            h.read_link(link).unwrap(),
            Some(std::path::PathBuf::from("/opt/b-ui/bin/bui2"))
        );
        assert_eq!(
            h.ops(),
            vec![
                "symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui",
                "symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui2"
            ]
        );
    }

    #[test]
    fn modprobe_makes_sys_module_appear() {
        let h = FakeHost::new();
        assert!(!h.is_dir(Path::new("/sys/module/nf_conntrack")).unwrap());
        h.modprobe("nf_conntrack").unwrap();
        assert!(
            h.is_dir(Path::new("/sys/module/nf_conntrack")).unwrap(),
            "加载后第二轮对账要判成已加载"
        );
        assert_eq!(h.ops(), vec!["modprobe:nf_conntrack"]);
    }
}
