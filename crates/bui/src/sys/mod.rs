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
}
