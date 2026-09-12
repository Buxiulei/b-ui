//! 装机前的环境探测（2026-09-12 裁决「安装：一行命令与零手工配置」）。
//!
//! 主理人的要求是「安装我需要能自动检测环境，完成所有的配置安装」：除面板域名外一个问题都不问，
//! 所以凡是过去靠人肉确认的事实（发行版、SELinux、时间同步、端口占用、域名解析、IPv6 出口）
//! 都要在这里自己探出来，打成一张表给运维看。
//!
//! 全部经 [`Host`] 原语，于是 [`crate::sys::fake::FakeHost`] 里可编程、单元测试既不 `systemctl`
//! 也不联网。两处**顺手修好**（不是只报告）：SELinux Enforcing 时给 `bin/*` 打标签、时钟没同步时
//! 开 `systemd-timesyncd`——两者失败都只警告，不阻塞装机。
//!
//! 唯一会**中止装机**的一项是关键端口被非本栈进程占用（见 [`EnvReport::blocking`]）。
//! 中止的时机在**写任何配置/单元与 `uninstall_v3` 之前**，但**在内核下载之后**（`run_with`
//! 第 3/4 步先拉 manifest 装内核，4.2 才探测）：所以文案说的是「配置与 v3 一字未动」，
//! 而不是「一个字节都没改」——`bin/` 里那 ~100MB 内核确实已经落盘了（2026-09-12 审查 nit）。
//! 装下去
//! 只会得到一堆起不来的单元，而端口是人肉才能腾的。

use crate::sys::{Host, Proto};
use bui_schema::model::Ports;
use std::path::Path;

/// 本栈（v4 自己 + 被它替换的 v3）的进程名：这些进程占着关键端口不算冲突。
/// `node` 是 v3 的 `b-ui-admin`（Node 面板）。
pub const STACK_PROCESSES: [&str; 7] = [
    "hysteria", "xray", "sing-box", "caddy", "bui", "b-ui", "node",
];

/// 占着某个关键端口的进程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHolder {
    pub proto: Proto,
    pub port: u16,
    /// 进程名；`ss` 不在或没有 `-p` 权限时是「未知进程」。
    pub process: String,
}

/// 一次环境探测的结果；[`table`] 负责渲染。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvReport {
    /// `debian 12` 这种「ID VERSION_ID」；`/etc/os-release` 读不到时是空串。
    pub distro: String,
    pub pkg_manager: String,
    pub systemd: bool,
    /// `Enforcing` / `Permissive` / `Disabled` / 空串（没装 SELinux）。
    pub selinux: String,
    /// `已同步` / `未同步` / `未知`。
    pub time_sync: String,
    pub ipv6_egress: bool,
    /// `getent ahosts <域名>` 解析出的地址（去重，保序）。
    pub domain_ips: Vec<String>,
    /// 被非本栈进程占着的关键端口。
    pub conflicts: Vec<PortHolder>,
    pub warnings: Vec<String>,
}

impl EnvReport {
    /// 阻塞装机的理由（`None` = 可以继续）。端口冲突是唯一一条：其余全是警告。
    pub fn blocking(&self) -> Option<String> {
        if self.conflicts.is_empty() {
            return None;
        }
        let list = self
            .conflicts
            .iter()
            .map(|c| {
                format!(
                    "{}/{} 被 {} 占用",
                    c.port,
                    match c.proto {
                        Proto::Tcp => "tcp",
                        Proto::Udp => "udp",
                    },
                    c.process
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        Some(format!(
            "关键端口已被占用：{list}。请先停掉这些进程（或换端口）再重跑 bui install；\
             已中止：配置、单元与 v3 一字未动（此前下载的内核留在 bin/ 里，重跑会直接复用）"
        ))
    }
}

/// `/etc/os-release` → `("debian 12", "apt")`。`ID` 缺失返回空发行版名。
pub fn parse_os_release(text: &str) -> (String, String) {
    let get = |key: &str| -> String {
        text.lines()
            .filter_map(|l| l.trim().split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let id = get("ID");
    let version = get("VERSION_ID");
    let distro = match (id.is_empty(), version.is_empty()) {
        (true, _) => String::new(),
        (false, true) => id.clone(),
        (false, false) => format!("{id} {version}"),
    };
    (distro, pkg_manager_for(&id, &get("ID_LIKE")).to_string())
}

/// 包管理器按 `ID` / `ID_LIKE` 判定（探不出来返回「未知」；v4 自带全部内核，包管理器只用于提示）。
pub fn pkg_manager_for(id: &str, id_like: &str) -> &'static str {
    let hay = format!("{id} {id_like}");
    let has = |k: &str| hay.split_whitespace().any(|w| w == k);
    if has("debian") || has("ubuntu") {
        "apt"
    } else if has("fedora") || has("rhel") || has("centos") {
        "dnf"
    } else if has("suse") || has("opensuse") {
        "zypper"
    } else if has("arch") {
        "pacman"
    } else if has("alpine") {
        "apk"
    } else {
        "未知"
    }
}

/// `ss -lntupH` 的一行 → `(协议, 本地端口, 进程名)`。
///
/// 列序是 `Netid State Recv-Q Send-Q Local Peer [Process]`；本地地址形如 `0.0.0.0:443`、
/// `*:80`、`[::]:443`，所以端口取最后一个冒号之后。进程名取 `users:(("<名字>",pid=…` 的第一个。
pub fn parse_ss(text: &str) -> Vec<(Proto, u16, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let proto = match cols[0] {
            "tcp" => Proto::Tcp,
            "udp" => Proto::Udp,
            _ => continue,
        };
        let Some((_, port)) = cols[4].rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        let process = line
            .split_once("users:((\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| "未知进程".to_string());
        out.push((proto, port, process));
    }
    out
}

/// 必须空着的关键端口：Caddy 的 80/443、HY2 直连与住宅、REALITY 两个入站。
/// 端口跳跃区间不查（它是一整段，v3 机器上本来就在 nft 的 redirect 里）。
pub fn key_ports(ports: &Ports) -> Vec<(Proto, u16)> {
    vec![
        (Proto::Tcp, 80),
        (Proto::Tcp, 443),
        (Proto::Tcp, ports.reality_direct),
        (Proto::Tcp, ports.reality_resi),
        (Proto::Udp, ports.hy2),
        (Proto::Udp, ports.hy2_resi),
    ]
}

/// `getent ahosts` 的输出 → 去重保序的地址表（第一列是地址，后面是 `STREAM <名字>` 之类）。
pub fn parse_ahosts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(ip) = line.split_whitespace().next() {
            if !ip.is_empty() && !out.iter().any(|o| o == ip) {
                out.push(ip.to_string());
            }
        }
    }
    out
}

/// 探一遍环境；`skip_port_check` 为真（v3 迁移）时不查端口占用——那台机器上 80/443/10000
/// 正被 v3 的 caddy 与 hysteria 占着，那是**要被替换掉的自己**，不是冲突。
pub fn probe(
    host: &dyn Host,
    domain: &str,
    public_ip: &str,
    ports: &Ports,
    bin_dir: &Path,
    skip_port_check: bool,
) -> EnvReport {
    let mut r = EnvReport::default();

    if let Ok(Some(bytes)) = host.read_file(Path::new("/etc/os-release")) {
        let (distro, pkg) = parse_os_release(&String::from_utf8_lossy(&bytes));
        r.distro = distro;
        r.pkg_manager = pkg;
    }
    if r.distro.is_empty() {
        r.distro = "未知".into();
        r.pkg_manager = "未知".into();
        r.warnings
            .push("读不到 /etc/os-release，无法识别发行版（v4 自带全部内核，通常不影响）".into());
    }

    r.systemd = host.which("systemctl");
    if !r.systemd {
        r.warnings
            .push("PATH 上没有 systemctl：没有 systemd 的系统装不了 v4".into());
    }

    // SELinux：Enforcing 时自己给 bin/* 打上 bin_t，否则 systemd 起内核会 203/EXEC
    r.selinux = host
        .run("getenforce", &[])
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default();
    if r.selinux == "Enforcing" {
        for f in host.list_dir(bin_dir).unwrap_or_default() {
            let p = f.display().to_string();
            let ok = host
                .run("chcon", &["-t", "bin_t", &p])
                .map(|o| o.ok())
                .unwrap_or(false)
                || host
                    .run("restorecon", &["-F", &p])
                    .map(|o| o.ok())
                    .unwrap_or(false);
            if !ok {
                r.warnings.push(format!(
                    "SELinux 是 Enforcing，但给 {p} 打 bin_t 标签失败：内核起不来时请手动 chcon -t bin_t"
                ));
            }
        }
    }

    // 时间同步：证书签发与 QUIC 握手都吃时钟；明确「未同步」才动手开 timesyncd
    let ntp = host
        .run("timedatectl", &["show", "-p", "NTPSynchronized"])
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default();
    r.time_sync = if ntp.contains("=yes") {
        "已同步".into()
    } else if ntp.contains("=no") {
        let started = host
            .run("systemctl", &["enable", "--now", "systemd-timesyncd"])
            .map(|o| o.ok())
            .unwrap_or(false);
        r.warnings.push(if started {
            "系统时钟未与 NTP 同步，已启用 systemd-timesyncd；证书签发失败时请等它同步完再重试"
                .into()
        } else {
            "系统时钟未与 NTP 同步，且启用 systemd-timesyncd 失败：请手动校时，否则证书签发会失败"
                .into()
        });
        "未同步".into()
    } else {
        "未知".into()
    };

    // IPv6 出口：没有也没关系（服务端出站一律钉死 IPv4），但客户端 IPv6 接管的判据要记一笔
    r.ipv6_egress = host
        .run("ip", &["-6", "route", "show", "default"])
        .map(|o| o.ok() && !o.stdout.trim().is_empty())
        .unwrap_or(false);

    // 域名解析核对：不一致只警告（DNS 还没生效、或走 Cloudflare 代理都属正常）
    if !domain.is_empty() {
        r.domain_ips = host
            .run("getent", &["ahosts", domain])
            .ok()
            .filter(|o| o.ok())
            .map(|o| parse_ahosts(&o.stdout))
            .unwrap_or_default();
        if r.domain_ips.is_empty() {
            r.warnings.push(format!(
                "{domain} 还解析不出地址：证书会签不下来，请先把 A 记录指到本机"
            ));
        } else if !public_ip.is_empty() && !r.domain_ips.iter().any(|ip| ip == public_ip) {
            r.warnings.push(format!(
                "{domain} 解析到 {}，与本机公网 IP {public_ip} 不一致（用了 CDN/代理则可忽略）",
                r.domain_ips.join("、")
            ));
        }
    }

    if !skip_port_check {
        let seen = host
            .run("ss", &["-lntupH"])
            .ok()
            .filter(|o| o.ok())
            .map(|o| parse_ss(&o.stdout))
            .unwrap_or_default();
        for (proto, port) in key_ports(ports) {
            for (p, n, process) in seen.iter() {
                if *p == proto && *n == port && !STACK_PROCESSES.contains(&process.as_str()) {
                    r.conflicts.push(PortHolder {
                        proto,
                        port,
                        process: process.clone(),
                    });
                }
            }
        }
    }
    r
}

/// 字符串在终端里占的列数（非 ASCII 一律按两列算——装机的表里只有中文与 ASCII）。
pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// 按**显示宽度**右补空格：中文标签用 `{:<12}` 会对不齐（`chars()` 数 1，终端占 2 列）。
/// 装机的两张表（这里的「环境」与 [`crate::commands::selfcheck::table`]）共用这一个。
pub fn pad_display(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// 「环境」表（装机日志里给运维看的那一张）。
pub fn table(r: &EnvReport, ports: &Ports) -> String {
    let row = |k: &str, v: &str| format!("  {}{v}", pad_display(k, 14));
    let mut out = vec!["环境".to_string()];
    out.push(row(
        "发行版",
        &format!("{}（包管理器 {}）", r.distro, r.pkg_manager),
    ));
    out.push(row("systemd", if r.systemd { "在位" } else { "缺失" }));
    out.push(row(
        "SELinux",
        if r.selinux.is_empty() {
            "未启用"
        } else {
            &r.selinux
        },
    ));
    out.push(row("时间同步", &r.time_sync));
    out.push(row(
        "IPv6 出口",
        if r.ipv6_egress {
            "有"
        } else {
            "无（服务端出站一律钉死 IPv4）"
        },
    ));
    out.push(row(
        "域名解析",
        &if r.domain_ips.is_empty() {
            "未解析".to_string()
        } else {
            r.domain_ips.join("、")
        },
    ));
    let ports_txt: Vec<String> = key_ports(ports)
        .iter()
        .map(|(_, p)| p.to_string())
        .collect();
    out.push(row(
        "关键端口",
        &if r.conflicts.is_empty() {
            format!("{} 均空闲", ports_txt.join("/"))
        } else {
            r.conflicts
                .iter()
                .map(|c| format!("{} 被 {} 占用", c.port, c.process))
                .collect::<Vec<_>>()
                .join("；")
        },
    ));
    for w in &r.warnings {
        out.push(format!("  ⚠ {w}"));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    fn ports() -> Ports {
        crate::commands::install::Answers::defaults("node-a", "203.0.113.10").ports
    }

    /// 一台「除本测关心的那一项之外都正常」的假机器：`/etc/os-release` 与 systemctl 在位，
    /// 于是 `warnings` 里不会混进无关的两条基线警告。
    fn sane() -> FakeHost {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("systemctl".into());
            i.files.insert(
                "/etc/os-release".into(),
                (b"ID=debian\nVERSION_ID=\"12\"\n".to_vec(), 0o644),
            );
        });
        h
    }

    #[test]
    fn reads_distro_and_package_manager_from_os_release() {
        let (d, p) = parse_os_release(
            "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nID=debian\nVERSION_ID=\"12\"\n",
        );
        assert_eq!((d.as_str(), p.as_str()), ("debian 12", "apt"));
        let (d, p) =
            parse_os_release("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"\n");
        assert_eq!((d.as_str(), p.as_str()), ("rocky 9.4", "dnf"));
        let (d, p) = parse_os_release("ID=ubuntu\nVERSION_ID=\"24.04\"\n");
        assert_eq!((d.as_str(), p.as_str()), ("ubuntu 24.04", "apt"));
        // 认不出的发行版不该猜错包管理器
        assert_eq!(parse_os_release("ID=voidlinux\n").1, "未知");
        assert_eq!(parse_os_release("").0, "");
        // ID_LIKE 的子串不算命中（`rhel` 不能被 `xrhelper` 之类蹭到）
        assert_eq!(pkg_manager_for("xrhel", ""), "未知");
    }

    #[test]
    fn parses_ss_lines_into_port_holders() {
        let text = "\
udp   UNCONN 0      0            0.0.0.0:10000      0.0.0.0:*    users:((\"hysteria\",pid=811,fd=8))
tcp   LISTEN 0      4096         0.0.0.0:443        0.0.0.0:*    users:((\"nginx\",pid=7,fd=6),(\"nginx\",pid=8,fd=6))
tcp   LISTEN 0      511               *:80          *:*
tcp   LISTEN 0      128            [::]:22          [::]:*       users:((\"sshd\",pid=900,fd=3))
nonsense
";
        assert_eq!(
            parse_ss(text),
            vec![
                (Proto::Udp, 10000, "hysteria".to_string()),
                (Proto::Tcp, 443, "nginx".to_string()),
                // 没有 users:(( 段（非 root 跑 ss）时也要认出端口，只是进程名未知
                (Proto::Tcp, 80, "未知进程".to_string()),
                (Proto::Tcp, 22, "sshd".to_string()),
            ]
        );
    }

    #[test]
    fn key_ports_cover_the_six_that_must_be_free() {
        let ps: Vec<u16> = key_ports(&ports()).iter().map(|(_, p)| *p).collect();
        assert_eq!(ps, vec![80, 443, 10001, 10002, 10000, 40000]);
    }

    /// 装机前的闸门：nginx 占着 443 就必须中止（装下去 caddy 起不来、证书签不下来），
    /// 而 v3 自己的 caddy/hysteria 占着同一批端口不算冲突。
    #[test]
    fn foreign_process_on_a_key_port_blocks_the_install() {
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push((
                "ss -lntupH".into(),
                CmdOut::success(
                    "tcp LISTEN 0 511 0.0.0.0:443 0.0.0.0:* users:((\"nginx\",pid=7,fd=6))\n\
                     udp UNCONN 0 0 0.0.0.0:10000 0.0.0.0:* users:((\"hysteria\",pid=8,fd=9))\n",
                ),
            ));
        });
        let r = probe(&h, "", "", &ports(), Path::new("/opt/b-ui/bin"), false);
        assert_eq!(
            r.conflicts,
            vec![PortHolder {
                proto: Proto::Tcp,
                port: 443,
                process: "nginx".into()
            }],
            "只有非本栈进程算冲突"
        );
        let msg = r.blocking().expect("必须中止");
        assert!(msg.contains("443/tcp 被 nginx 占用"), "{msg}");
        assert!(msg.contains("已中止"), "{msg}");
        // 探测跑在内核下载**之后**：别宣称「一个字节都没改」（bin/ 里已有 ~100MB 内核）
        assert!(
            msg.contains("配置、单元与 v3 一字未动") && msg.contains("内核留在 bin/"),
            "{msg}"
        );
        assert!(!msg.contains("一个字节都没改"), "{msg}");
        // v3 迁移场景：同一份 ss 输出一条冲突都不报
        let r = probe(&h, "", "", &ports(), Path::new("/opt/b-ui/bin"), true);
        assert_eq!(r.conflicts, vec![]);
        assert_eq!(r.blocking(), None);
    }

    #[test]
    fn selinux_enforcing_labels_the_kernels_and_only_warns_on_failure() {
        let h = sane();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files
                .insert("/opt/b-ui/bin/caddy".into(), (b"ELF".to_vec(), 0o755));
            i.scripted
                .push(("getenforce".into(), CmdOut::success("Enforcing\n")));
            // chcon 不在（精简系统）→ 退回 restorecon；caddy 那次两条都失败 → 只警告
            i.scripted.push((
                "chcon".into(),
                CmdOut::failure(127, "chcon: command not found"),
            ));
            i.scripted.push((
                "restorecon -F /opt/b-ui/bin/caddy".into(),
                CmdOut::failure(1, "boom"),
            ));
        });
        let r = probe(&h, "", "", &ports(), Path::new("/opt/b-ui/bin"), true);
        assert_eq!(r.selinux, "Enforcing");
        assert!(h
            .ops()
            .contains(&"run:chcon -t bin_t /opt/b-ui/bin/xray".to_string()));
        assert!(h
            .ops()
            .contains(&"run:restorecon -F /opt/b-ui/bin/xray".to_string()));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(
            r.warnings[0].contains("/opt/b-ui/bin/caddy"),
            "{:?}",
            r.warnings
        );
        assert_eq!(r.blocking(), None, "标签失败不阻塞装机");
        // 没装 SELinux 的机器（getenforce 不存在）：一条 chcon 都不该跑
        let h2 = sane();
        h2.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
        });
        let r2 = probe(&h2, "", "", &ports(), Path::new("/opt/b-ui/bin"), true);
        assert_eq!(r2.selinux, "");
        assert!(!h2.ops().iter().any(|o| o.starts_with("run:chcon")));
    }

    #[test]
    fn unsynced_clock_starts_timesyncd_and_warns() {
        let h = sane();
        h.with(|i| {
            i.scripted.push((
                "timedatectl show -p NTPSynchronized".into(),
                CmdOut::success("NTPSynchronized=no\n"),
            ));
        });
        let r = probe(&h, "", "", &ports(), Path::new("/opt/b-ui/bin"), true);
        assert_eq!(r.time_sync, "未同步");
        assert!(h
            .ops()
            .contains(&"run:systemctl enable --now systemd-timesyncd".to_string()));
        assert!(r.warnings.iter().any(|w| w.contains("systemd-timesyncd")));
        // 已同步：不碰 timesyncd
        let h2 = sane();
        h2.with(|i| {
            i.scripted.push((
                "timedatectl show -p NTPSynchronized".into(),
                CmdOut::success("NTPSynchronized=yes\n"),
            ));
        });
        let r2 = probe(&h2, "", "", &ports(), Path::new("/opt/b-ui/bin"), true);
        assert_eq!(r2.time_sync, "已同步");
        assert!(!h2.ops().iter().any(|o| o.contains("timesyncd")));
        assert!(r2.warnings.is_empty(), "{:?}", r2.warnings);
        // timedatectl 不在（容器）：只报「未知」，不擅自装 NTP
        let h3 = sane();
        h3.with(|i| {
            i.scripted
                .push(("timedatectl".into(), CmdOut::failure(127, "not found")));
        });
        assert_eq!(
            probe(&h3, "", "", &ports(), Path::new("/opt/b-ui/bin"), true).time_sync,
            "未知"
        );
    }

    #[test]
    fn domain_mismatch_warns_but_never_blocks() {
        let h = sane();
        h.with(|i| {
            i.scripted.push((
                "getent ahosts panel.example.com".into(),
                CmdOut::success(
                    "198.51.100.9  STREAM panel.example.com\n198.51.100.9  DGRAM\n198.51.100.9  RAW\n",
                ),
            ));
        });
        let r = probe(
            &h,
            "panel.example.com",
            "203.0.113.10",
            &ports(),
            Path::new("/opt/b-ui/bin"),
            true,
        );
        assert_eq!(
            r.domain_ips,
            vec!["198.51.100.9".to_string()],
            "同一地址去重"
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("203.0.113.10")),
            "{:?}",
            r.warnings
        );
        assert_eq!(
            r.blocking(),
            None,
            "域名不一致不阻塞（CDN/DNS 未生效都正常）"
        );
        // 一致就不警告
        let h2 = sane();
        h2.with(|i| {
            i.scripted.push((
                "getent ahosts panel.example.com".into(),
                CmdOut::success("203.0.113.10 STREAM panel.example.com\n"),
            ));
        });
        let r2 = probe(
            &h2,
            "panel.example.com",
            "203.0.113.10",
            &ports(),
            Path::new("/opt/b-ui/bin"),
            true,
        );
        assert!(r2.warnings.is_empty(), "{:?}", r2.warnings);
        // 完全解析不出来：警告点明证书会签不下来
        let h3 = sane();
        h3.with(|i| {
            i.scripted
                .push(("getent".into(), CmdOut::failure(2, "not found")));
        });
        let r3 = probe(
            &h3,
            "panel.example.com",
            "203.0.113.10",
            &ports(),
            Path::new("/opt/b-ui/bin"),
            true,
        );
        assert!(
            r3.warnings.iter().any(|w| w.contains("A 记录")),
            "{:?}",
            r3.warnings
        );
    }

    #[test]
    fn the_table_shows_every_probed_fact() {
        let h = sane();
        h.with(|i| {
            i.scripted.push((
                "timedatectl show -p NTPSynchronized".into(),
                CmdOut::success("NTPSynchronized=yes\n"),
            ));
            i.scripted.push((
                "ip -6 route show default".into(),
                CmdOut::success("default via 2001:db8::1 dev eth0\n"),
            ));
            i.scripted.push((
                "getent ahosts panel.example.com".into(),
                CmdOut::success("203.0.113.10 STREAM panel.example.com\n"),
            ));
        });
        let r = probe(
            &h,
            "panel.example.com",
            "203.0.113.10",
            &ports(),
            Path::new("/opt/b-ui/bin"),
            false,
        );
        assert!(r.systemd && r.ipv6_egress);
        let t = table(&r, &ports());
        for want in [
            "环境",
            "debian 12（包管理器 apt）",
            "systemd",
            "在位",
            "未启用",
            "已同步",
            "203.0.113.10",
            "80/443/10001/10002/10000/40000 均空闲",
        ] {
            assert!(t.contains(want), "表里缺「{want}」：\n{t}");
        }
        assert!(!t.contains('⚠'), "干净环境不该有警告：\n{t}");
    }
}
