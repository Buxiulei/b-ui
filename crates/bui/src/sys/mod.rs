//! 所有「碰真实系统」的能力都收在 [`Host`] 一个 trait 后面：读写文件、符号链接、
//! immutable 位、跑外部命令、systemd、sysctl、内核模块、主机事实、监听端口、时钟。
//! 生产用 [`real::RealHost`]，单元测试注入 [`fake::FakeHost`]，于是测试既不
//! `systemctl`、也不写 `/etc`、更不联网。
//!
//! 接口一律**同步**（总纲裁决「`Host` trait 同步 + `spawn_blocking`」）：对账整体在
//! `tokio::task::spawn_blocking` 里跑，async 运行时不会被这些阻塞调用拖住。

pub mod env_probe;
#[cfg(test)]
pub mod fake;
pub mod real;

use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 一次外部命令的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOut {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    pub fn success(stdout: &str) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn failure(code: i32, stderr: &str) -> Self {
        Self {
            status: code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// 监听端口探测的协议（watchdog 与防火墙都要按协议分别看）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proto {
    Tcp,
    Udp,
}

/// 所有碰真实系统的操作都走这里；同步接口，对账在 `spawn_blocking` 里跑。
pub trait Host: Send + Sync {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>>;
    fn write_file(&self, path: &Path, content: &[u8], mode: u32) -> Result<()>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    /// **只返回直接子项**（文件与目录都返回，绝对路径，按路径名升序）；目录不存在 → `Ok(vec![])`，不是错误。
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    fn is_dir(&self, path: &Path) -> Result<bool>;
    /// 递归删除一个目录（`uninstall_v3` 删 v3 的 `admin/`、漂移清理删陌生目录）。
    fn remove_dir_all(&self, path: &Path) -> Result<()>;
    fn is_symlink(&self, path: &Path) -> Result<bool>;
    fn read_link(&self, path: &Path) -> Result<Option<PathBuf>>;
    /// 建/改符号链接（`/usr/local/bin/{bui,b-ui}`）：目标存在则先删再建。
    fn symlink(&self, target: &Path, link: &Path) -> Result<()>;
    fn set_immutable(&self, path: &Path, on: bool) -> Result<()>;
    fn is_immutable(&self, path: &Path) -> Result<bool>;
    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut>;
    fn which(&self, program: &str) -> bool;
    fn systemd_daemon_reload(&self) -> Result<()>;
    fn systemd(&self, verb: &str, unit: &str) -> Result<CmdOut>;
    fn unit_is_active(&self, unit: &str) -> Result<bool>;
    fn unit_is_enabled(&self, unit: &str) -> Result<bool>;
    fn unit_exists(&self, unit: &str) -> Result<bool>;
    fn unit_property(&self, unit: &str, prop: &str) -> Result<Option<String>>;
    fn sysctl_get(&self, key: &str) -> Result<Option<String>>;
    fn sysctl_set(&self, key: &str, value: &str) -> Result<()>;
    fn modprobe(&self, module: &str) -> Result<()>;
    fn mem_mb(&self) -> Result<u64>;
    fn arch(&self) -> Result<String>;
    fn hostname(&self) -> Result<String>;
    fn listening_ports(&self, proto: Proto) -> Result<BTreeSet<u16>>;
    fn now(&self) -> time::OffsetDateTime;
    /// 读 `units` 在 `from` 之后的新日志（日志哨兵，spec §5.7）。journalctl 不存在 →
    /// `Err(JOURNALCTL_MISSING)`；游标失效等非零退出 → `Err`（调用方丢游标、从「现在」重来）。
    fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>>;
}

/// 单元名归一化：不含 `.` 的裸名补 `.service`，已带后缀（`.service` / `.timer`）原样返回。
/// 真假两套实现共用，保证 `unit_is_active("hysteria-server")` 在两边语义一致。
fn unit_full(unit: &str) -> String {
    if unit.contains('.') {
        unit.to_string()
    } else {
        format!("{unit}.service")
    }
}

/// `run:` 流水与 `scripted` 前缀匹配共用的命令行拼法：`<program> <args 以空格连接>`。
fn cmd_line(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

/// 日志哨兵（spec §5.7）读 journald 的起点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalFrom {
    /// 上一轮读到的最后一条的 `__CURSOR`：只读它之后的新条目
    Cursor(String),
    /// 没有游标（首次启动 / 游标失效）：从这一刻读起，**不回放历史**
    Since(time::OffsetDateTime),
}

/// `journalctl -o json` 的一条记录，只留哨兵要的四样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// `__CURSOR`：下一轮 `--after-cursor` 的参数
    pub cursor: String,
    /// 裸单元名（`xray`，不带 `.service`）
    pub unit: String,
    /// `__REALTIME_TIMESTAMP`（微秒）换成的 UTC 时刻
    pub ts: time::OffsetDateTime,
    /// 去掉 ANSI 色码的 `MESSAGE`；带 tracing-journald 的 `F_ERROR` 字段时追加 ` error=<值>`
    pub message: String,
}

/// `journal_read` 在机器上找不到 journalctl 时的错误文案。
pub const JOURNALCTL_MISSING: &str = "机器上没有 journalctl";

/// `journalctl` 的参数：`-o json` 每行一条、`-q` 不打「-- No entries --」、每个单元一个 `-u`；
/// 有游标用 `--after-cursor`，否则 `--since @<unix 秒>`（systemd.time(7) 的 `@` 写法，与时区无关）。
pub fn journal_args(units: &[String], from: &JournalFrom) -> Vec<String> {
    let mut v: Vec<String> = ["--no-pager", "-q", "-o", "json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for u in units {
        v.push("-u".into());
        v.push(unit_full(u));
    }
    match from {
        JournalFrom::Cursor(c) => {
            v.push("--after-cursor".into());
            v.push(c.clone());
        }
        JournalFrom::Since(t) => {
            v.push("--since".into());
            v.push(format!("@{}", t.unix_timestamp()));
        }
    }
    v
}

/// journald 的 JSON 字段值 → 文本。字段里含不可打印字节（sing-box 的 ANSI 色码就是 ESC）时，
/// `journalctl -o json` 把它编成**字节数组**而不是字符串。
fn journal_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(a) => {
            let bytes = a
                .iter()
                .map(|b| b.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect::<Option<Vec<u8>>>()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

fn bare_unit(u: &str) -> String {
    u.strip_suffix(".service").unwrap_or(u).to_string()
}

/// 解析 `journalctl -o json` 的输出（每行一个 JSON 对象）。不在 `units` 里的记录、缺字段的行、
/// 非 JSON 行一律丢掉。单元名先看 `UNIT` 再看 `_SYSTEMD_UNIT`：systemd 自己关于某单元的消息
/// （「Start request repeated too quickly」「restart counter is at 5」）的 `_SYSTEMD_UNIT` 是
/// `init.scope`，单元名在 `UNIT` 里。
pub fn parse_journal_json(stdout: &str, units: &[String]) -> Vec<JournalRecord> {
    let want: BTreeSet<String> = units.iter().map(|u| bare_unit(u)).collect();
    stdout
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let cursor = v.get("__CURSOR")?.as_str()?.to_string();
            let us: i128 = v.get("__REALTIME_TIMESTAMP")?.as_str()?.parse().ok()?;
            let ts = time::OffsetDateTime::from_unix_timestamp_nanos(us * 1000).ok()?;
            let unit = ["UNIT", "_SYSTEMD_UNIT"]
                .iter()
                .filter_map(|k| v.get(*k).and_then(|x| x.as_str()))
                .map(bare_unit)
                .find(|u| want.contains(u))?;
            let mut message = journal_text(v.get("MESSAGE")?)?;
            if let Some(e) = v.get("F_ERROR").and_then(journal_text) {
                message.push_str(" error=");
                message.push_str(&e);
            }
            Some(JournalRecord {
                cursor,
                unit,
                ts,
                message: crate::util::strip_ansi(&message),
            })
        })
        .collect()
}

/// `/proc/net/{tcp,udp,tcp6,udp6}` 的本地端口解析。
/// 第二列是 `HEXIP:HEXPORT`；TCP 的 LISTEN 状态是 `0A`（第四列），`listening_only=true` 时只取它。
pub fn parse_proc_net(text: &str, listening_only: bool) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        if listening_only && cols[3] != "0A" {
            continue;
        }
        let Some((_, port_hex)) = cols[1].split_once(':') else {
            continue;
        };
        if let Ok(port) = u16::from_str_radix(port_hex, 16) {
            out.insert(port);
        }
    }
    out
}

/// 公网 IP 探测源，按顺序试。**必须多源**：2026-09-12 bwg-rick 的 v3→v4 首切时
/// `api.ipify.org` 返回了空串（事后手动 curl 同一地址正常），单源探测直接让
/// `state.node.public_ip` 留空。
pub const IP_PROBE_URLS: [&str; 3] = [
    "https://api.ipify.org",
    "https://api.ip.sb/ip",
    "https://ifconfig.me/ip",
];

/// 依次探测 [`IP_PROBE_URLS`]，返回第一个**能解析成 IPv4** 的回答（空串、HTML 错误页、
/// IPv6 都不算）；全都不行返回空串。
///
/// **阻塞**（`curl --max-time 5`）：只能在 `tokio::task::spawn_blocking` 里或纯同步的
/// CLI 路径里调用。
pub fn probe_public_ip(host: &dyn Host) -> String {
    for url in IP_PROBE_URLS {
        let Ok(out) = host.run("curl", &["-sS", "--max-time", "5", url]) else {
            continue;
        };
        if !out.ok() {
            continue;
        }
        let text = out.stdout.trim();
        if text.parse::<std::net::Ipv4Addr>().is_ok() {
            return text.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const UDP: &str = "\
   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  654: 00000000:D3A1 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 201613515 2 0000000000000000 0
 1858: 00000000:D855 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 201624164 2 0000000000000000 0
";

    const TCP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:7235 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5931149 1 0000000000000000 100 0 0 10 0
   1: 0100007F:7751 00000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 35608872 1 0000000000000000 100 0 0 10 0
";

    #[test]
    fn parses_udp_local_ports() {
        assert_eq!(parse_proc_net(UDP, false), BTreeSet::from([0xD3A1, 0xD855]));
    }

    #[test]
    fn parses_only_listening_tcp_ports() {
        // 0x7235 是 LISTEN(0A)，0x7751 是 ESTABLISHED(01)
        assert_eq!(parse_proc_net(TCP, true), BTreeSet::from([0x7235]));
        assert_eq!(parse_proc_net(TCP, false), BTreeSet::from([0x7235, 0x7751]));
    }

    #[test]
    fn ignores_garbage_lines() {
        assert_eq!(
            parse_proc_net("nonsense\n\n   sl  local_address\n", false),
            BTreeSet::new()
        );
    }

    /// 2026-09-12 真机：ipify 返回空串（curl 退出码 0）→ 必须继续试下一个源。
    #[test]
    fn public_ip_probe_falls_through_to_the_third_source() {
        let h = fake::FakeHost::new();
        h.with(|i| {
            // ① 连不上 ② 200 但回了个空串（真机形态）③ 才是真答案
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[0]),
                CmdOut::failure(7, "couldn't connect"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[1]),
                CmdOut::success("\n"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[2]),
                CmdOut::success("203.0.113.10\n"),
            ));
        });
        assert_eq!(probe_public_ip(&h), "203.0.113.10");
        assert_eq!(
            h.ops(),
            IP_PROBE_URLS
                .iter()
                .map(|u| format!("run:curl -sS --max-time 5 {u}"))
                .collect::<Vec<_>>(),
            "三个源按顺序试"
        );
    }

    #[test]
    fn public_ip_probe_rejects_non_ipv4_answers_and_stops_at_the_first_good_one() {
        let h = fake::FakeHost::new();
        h.with(|i| {
            // 运营商的劫持页 / IPv6 都不算 IPv4
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[0]),
                CmdOut::success("<html>error</html>"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[1]),
                CmdOut::success("2001:db8::1"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", IP_PROBE_URLS[2]),
                CmdOut::success("198.51.100.7"),
            ));
        });
        assert_eq!(probe_public_ip(&h), "198.51.100.7");
        // 第一个源就成功时不再探后面两个
        let h2 = fake::FakeHost::new();
        h2.with(|i| {
            i.scripted
                .push(("curl".into(), CmdOut::success("203.0.113.10")))
        });
        assert_eq!(probe_public_ip(&h2), "203.0.113.10");
        assert_eq!(h2.ops().len(), 1, "{:?}", h2.ops());
        // 三个源全失败 → 空串（调用方据此提示「稍后在面板里补填」）
        let h3 = fake::FakeHost::new();
        h3.with(|i| i.scripted.push(("curl".into(), CmdOut::failure(7, "x"))));
        assert_eq!(probe_public_ip(&h3), "");
    }

    fn j0() -> time::OffsetDateTime {
        time::macros::datetime!(2026-09-11 00:00:00 UTC)
    }

    /// `__REALTIME_TIMESTAMP` 是**微秒**的十进制串
    fn us(secs: i64) -> String {
        ((j0().unix_timestamp() + secs) * 1_000_000).to_string()
    }

    /// 真机三种形态：① sing-box 带色输出（含 ESC ⇒ journald 把 MESSAGE 编成字节数组）
    /// ② systemd 关于某单元的消息（`_SYSTEMD_UNIT=init.scope`，单元名在 `UNIT`）
    /// ③ 守护进程自己的 tracing 事件（tracing-journald 0.3.2 默认前缀：`error` 字段落在 `F_ERROR`）
    #[test]
    fn journal_json_decodes_byte_arrays_systemd_messages_and_tracing_error_fields() {
        let units: Vec<String> = ["b-ui-relay", "xray", "b-ui"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let colored = "\x1b[31mERROR\x1b[0m[4006] [\x1b[38;5;48m2302991392\x1b[0m 6.42s] \
                       connection: open connection to www.gstatic.com:443 using \
                       outbound/socks[resi-2]: dial tcp 198.51.100.8:10007: i/o timeout";
        let l1 = serde_json::json!({"__CURSOR": "s=a;i=1", "__REALTIME_TIMESTAMP": us(1),
            "_SYSTEMD_UNIT": "b-ui-relay.service", "MESSAGE": colored.as_bytes()})
        .to_string();
        let l2 = serde_json::json!({"__CURSOR": "s=a;i=2", "__REALTIME_TIMESTAMP": us(2),
            "_SYSTEMD_UNIT": "init.scope", "UNIT": "xray.service",
            "MESSAGE": "xray.service: Start request repeated too quickly."})
        .to_string();
        let l3 = serde_json::json!({"__CURSOR": "s=a;i=3", "__REALTIME_TIMESTAMP": us(3),
            "_SYSTEMD_UNIT": "b-ui.service", "MESSAGE": "用户同步有失败项，下一轮安全网会重试",
            "F_ERROR": "AddUser vless-direct 失败：code: 'The service is currently unavailable'"})
        .to_string();
        // 不在单元集合里 / 非 JSON / 缺 __CURSOR：一律丢掉
        let l4 = serde_json::json!({"__CURSOR": "s=a;i=4", "__REALTIME_TIMESTAMP": us(4),
            "_SYSTEMD_UNIT": "sshd.service", "MESSAGE": "Accepted publickey"})
        .to_string();
        let l6 = serde_json::json!({"__REALTIME_TIMESTAMP": us(5),
            "_SYSTEMD_UNIT": "xray.service", "MESSAGE": "x"})
        .to_string();
        let text = [l1, l2, l3, l4, "-- No entries --".to_string(), l6].join("\n");
        let out = parse_journal_json(&text, &units);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].unit, "b-ui-relay");
        assert_eq!(out[0].cursor, "s=a;i=1");
        assert_eq!(out[0].ts, j0() + time::Duration::seconds(1));
        assert!(
            out[0].message.starts_with(
                "ERROR[4006] [2302991392 6.42s] connection: open connection to www.gstatic.com:443"
            ),
            "{}",
            out[0].message
        );
        assert!(!out[0].message.contains('\x1b'), "色码必须剥掉");
        assert_eq!(out[1].unit, "xray", "systemd 自己的消息按 UNIT 归属");
        assert_eq!(
            out[2].message,
            "用户同步有失败项，下一轮安全网会重试 error=AddUser vless-direct 失败：\
             code: 'The service is currently unavailable'"
        );
    }

    #[test]
    fn journal_args_use_after_cursor_or_an_epoch_since() {
        let units = vec![
            "b-ui-relay".to_string(),
            "hysteria-residential-1".to_string(),
        ];
        assert_eq!(
            journal_args(&units, &JournalFrom::Cursor("s=abc;i=9".into())),
            [
                "--no-pager",
                "-q",
                "-o",
                "json",
                "-u",
                "b-ui-relay.service",
                "-u",
                "hysteria-residential-1.service",
                "--after-cursor",
                "s=abc;i=9"
            ]
            .map(String::from)
            .to_vec()
        );
        let since = journal_args(&units, &JournalFrom::Since(j0()));
        assert_eq!(
            since[since.len() - 2..].to_vec(),
            vec!["--since".to_string(), format!("@{}", j0().unix_timestamp())],
            "systemd.time(7) 的 @<unix 秒> 写法，与时区无关"
        );
    }

    #[test]
    fn the_fake_journal_pops_scripted_batches_and_records_where_it_read_from() {
        let h = fake::FakeHost::new();
        let r = JournalRecord {
            cursor: "c1".into(),
            unit: "xray".into(),
            ts: j0(),
            message: "m".into(),
        };
        h.with(|i| {
            i.journal.push_back(Ok(vec![r.clone()]));
            i.journal.push_back(Err("Failed to seek to cursor".into()));
        });
        let units = vec!["xray".to_string(), "caddy".to_string()];
        assert_eq!(
            h.journal_read(&units, &JournalFrom::Since(j0())).unwrap(),
            vec![r]
        );
        assert!(h
            .journal_read(&units, &JournalFrom::Cursor("c1".into()))
            .is_err());
        assert!(
            h.journal_read(&units, &JournalFrom::Cursor("c1".into()))
                .unwrap()
                .is_empty(),
            "弹空后返回空批"
        );
        assert_eq!(
            h.ops(),
            vec![
                "journal:xray,caddy:since=2026-09-11T00:00:00Z",
                "journal:xray,caddy:cursor=c1",
                "journal:xray,caddy:cursor=c1"
            ]
        );
    }
}
