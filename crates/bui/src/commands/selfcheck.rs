//! 装完自检（2026-09-12 裁决「安装：一行命令与零手工配置」的「装完自检打印 PASS/FAIL 表」）。
//!
//! 判据照 `scripts/m1-acceptance.sh`：六单元在跑、三个内核校验器过、关键端口在监听、漂移为空，
//! 再加一条真正的**端到端**验证——用自带的 hysteria 客户端拿首个用户的凭据打一次外网
//! （m1-acceptance 没有这条，而「装完能不能用」只有它能回答）。
//!
//! 任一 FAIL 让 `bui install` 退 2，但**不回滚**：配置已经落盘、v3 已经卸掉，回滚只会把机器
//! 推到更糟的中间态；运维要的是「哪一项没过、怎么查」。
//!
//! 全新装机有一段**必然的**未就绪窗口（2026-09-12 审查 blocking）：`config.yaml` 的
//! `tls.cert` 指向 `<certs_dir>/fullchain.pem`，而那张证书要等 Caddy 跑完 ACME、再由守护进程的
//! 证书同步（[`crate::modules::certs`]）复制过来才存在；在那之前两个 hysteria 一起来就 FATAL
//! （`failed to load server config … tls.cert: no such file or directory`）进 auto-restart，
//! 对应的 UDP 端口也没在听。所以自检**先有界等一等**（[`Wait`]：≤120s、每 3s 一轮），
//! 等不到证书就把「两个 hysteria 单元 / HY2 的 UDP 端口 / HY2 回环鉴权」判成 SKIP 而不是 FAIL
//! ——否则一行命令装完必然退 2，与「一行命令完成新服务器的所有安装」直接冲突。
//! 同一段等待顺带消掉 `finish_self_restart` 刚 restart 过 `b-ui` 时 admin 口的竞态。

use crate::reconcile::MANAGED_UNITS;
use crate::state::runtime::DriftItem;
use crate::sys::{Host, Proto};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use std::time::Duration;

/// HY2 回环探测用的本机 socks5 端口：只在自检那十几秒里存在，选一个没人用的高位端口。
pub const PROBE_SOCKS_PORT: u16 = 45899;
/// 回环探测打的外网地址（与 [`crate::sys::IP_PROBE_URLS`] 同源，200 即证明出网与鉴权都通）。
pub const PROBE_URL: &str = "https://api.ipify.org";

/// 自检前那段有界等待的预算。`Default` 是装机真跑的那一组；测试用 [`Wait::NONE`]（只查一轮，不睡）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub budget: Duration,
    pub poll: Duration,
}

impl Default for Wait {
    fn default() -> Self {
        // 120s 是「Caddy 签到 + 证书同步一轮（30s 兜底轮询）+ 两个 hysteria 重启」的宽裕上限；
        // 等不到就 SKIP 往下走，不在最后一屏无限期干等。
        Wait {
            budget: Duration::from_secs(120),
            poll: Duration::from_secs(3),
        }
    }
}

impl Wait {
    /// 一轮都不等（`poll` 为零即「查一次就返回」）。
    pub const NONE: Wait = Wait {
        budget: Duration::ZERO,
        poll: Duration::ZERO,
    };
}

/// 证书还没到位时那三行的说明。
pub const CERT_PENDING: &str =
    "待证书（Caddy 尚未签到；证书同步到位后 systemd 会自动拉起，`bui status` 复检）";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Skip,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Skip => "SKIP",
        }
    }
}

/// 自检表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub verdict: Verdict,
    pub detail: String,
}

fn pass(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Pass,
        detail: detail.into(),
    }
}

fn fail(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Fail,
        detail: detail.into(),
    }
}

fn skip(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Skip,
        detail: detail.into(),
    }
}

/// FAIL 的条数（SKIP 不算）。
pub fn failures(rows: &[Check]) -> usize {
    rows.iter().filter(|c| c.verdict == Verdict::Fail).count()
}

/// 文件在不在。用 `list_dir` 而不是 `read_file`：内核动辄几十 MB，只为测存在把整个文件读进内存
/// 太亏（2026-09-12 审查 nit）。
fn file_exists(host: &dyn Host, path: &std::path::Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    host.list_dir(parent)
        .map(|kids| kids.iter().any(|k| k == path))
        .unwrap_or(false)
}

/// `<bin>/<program>` 优先，其次 PATH（与 apply 的校验器查找同一套规则）；都没有 `None`。
fn kernel_bin(host: &dyn Host, paths: &Paths, program: &str) -> Option<String> {
    let bundled = paths.bin_dir.join(program);
    if file_exists(host, &bundled) {
        return Some(bundled.display().to_string());
    }
    host.which(program).then(|| program.to_string())
}

/// 证书（`tls.cert`，两个 hysteria 起不来的唯一常见原因）在不在。
fn cert_present(host: &dyn Host, paths: &Paths) -> bool {
    file_exists(host, &paths.certs_dir.join("fullchain.pem"))
}

/// 这个单元起不来是不是「在等证书」。两个 hysteria 的 `config*.yaml` 都写着 `tls.cert`；
/// xray（REALITY）、relay、caddy、b-ui 都不读那张证书。
fn needs_cert(unit: &str) -> bool {
    unit.starts_with("hysteria")
}

/// 等证书的那两个 UDP 端口（HY2 直连与住宅）。
fn cert_bound_port(state: &State, proto: Proto, port: u16) -> bool {
    proto == Proto::Udp && (port == state.node.ports.hy2 || port == state.node.ports.hy2_resi)
}

/// 自检前的有界等待：等到「证书在位 + 六单元 active + 关键端口在听」，或耗尽预算。
/// 返回值是**证书到底有没有到位**——那三行是判 SKIP 还是 FAIL 全看它。
fn wait_ready(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    wait: Wait,
    sleep: &dyn Fn(Duration),
) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        let cert = cert_present(host, paths);
        let units = MANAGED_UNITS
            .iter()
            .all(|u| host.unit_is_active(u).unwrap_or(false));
        let ports = {
            let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
            let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
            listen_ports(state)
                .into_iter()
                .all(|(proto, port)| match proto {
                    Proto::Tcp => tcp.contains(&port),
                    Proto::Udp => udp.contains(&port),
                })
        };
        if cert && units && ports {
            return true;
        }
        if wait.poll.is_zero() || waited >= wait.budget {
            return cert;
        }
        sleep(wait.poll);
        waited += wait.poll;
    }
}

/// 关键端口：面板与证书的 443、REALITY 两个入站、HY2 直连与住宅、面板自己的 admin 口。
/// 端口跳跃区间不查（nft redirect 过来的，本身不监听）。
pub fn listen_ports(state: &State) -> Vec<(Proto, u16)> {
    let p = &state.node.ports;
    vec![
        (Proto::Tcp, 443),
        (Proto::Tcp, p.reality_direct),
        (Proto::Tcp, p.reality_resi),
        (Proto::Tcp, p.admin),
        (Proto::Udp, p.hy2),
        (Proto::Udp, p.hy2_resi),
    ]
}

/// hysteria 客户端配置（凭据只落这里，0600，用完即删）。
pub fn hy2_client_yaml(state: &State, username: &str, hy2_password: &str) -> String {
    let n = &state.node;
    let mut y = format!(
        "server: 127.0.0.1:{}\nauth: {username}:{hy2_password}\ntls:\n  sni: {}\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{PROBE_SOCKS_PORT}\n",
        n.ports.hy2, n.domain
    );
    if n.obfs.enabled {
        y.push_str(&format!(
            "obfs:\n  type: salamander\n  salamander:\n    password: {}\n",
            n.obfs.password
        ));
    }
    y
}

/// 回环探测最多试几轮、每轮 curl 最多等几秒：最坏 6×(1s+4s)≈30s 就放弃。
/// （连不上时这一项 FAIL，运维照 detail 去看 journalctl，不值得在最后一屏干等两分钟。）
const PROBE_ROUNDS: u32 = 6;
const PROBE_TIMEOUT: u32 = 4;

/// 探测脚本：起客户端 → 经它的 socks5 打一次外网 → 打印 HTTP 状态码 → 收掉客户端。
/// 凭据在配置文件里，**不进 argv**（`ps` 会泄露）。
pub fn hy2_probe_sh(hysteria: &str, cfg: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # bui install 自检生成，用完即删。\n\
         # 精简系统只有 wget（install.sh 也支持那种机器）：curl 不在就报 nocurl，让这一项 SKIP，\n\
         # 不是 FAIL —— wget 不会 socks5，这里没有等价替代。\n\
         command -v curl > /dev/null 2>&1 || {{ printf 'nocurl\\n'; exit 0; }}\n\
         \"{hysteria}\" client -c \"{cfg}\" > /dev/null 2>&1 &\n\
         pid=$!\n\
         code=000\n\
         i=0\n\
         while [ $i -lt {PROBE_ROUNDS} ]; do\n\
         \x20 i=$((i+1))\n\
         \x20 sleep 1\n\
         \x20 code=$(curl -s -o /dev/null -w '%{{http_code}}' --max-time {PROBE_TIMEOUT} \
         --socks5-hostname 127.0.0.1:{PROBE_SOCKS_PORT} '{PROBE_URL}')\n\
         \x20 [ \"$code\" != \"000\" ] && break\n\
         done\n\
         kill \"$pid\" > /dev/null 2>&1\n\
         printf '%s\\n' \"$code\"\n"
    )
}

/// HY2 端到端：首个可用用户的凭据经回环打一次外网，期望 200。
/// 没有用户（全新装机）或没有 hysteria 二进制 ⇒ SKIP，不算失败。
fn hy2_loopback(host: &dyn Host, paths: &Paths, state: &State, cert_ready: bool) -> Check {
    let name = "HY2 回环鉴权";
    if !cert_ready {
        // 证书还没到 ⇒ hysteria 根本没在跑，打过去必然 000：这不是「装坏了」，别染红。
        return skip(name, CERT_PENDING);
    }
    let Some(user) = state.users.iter().find(|u| !u.disabled) else {
        return skip(name, "还没有用户（全新装机）；加完用户后 `bui status` 复检");
    };
    let Some(hysteria) = kernel_bin(host, paths, "hysteria") else {
        return skip(name, "找不到 hysteria 二进制，跳过");
    };
    let dir = crate::paths::verify_dir(paths);
    let (cfg, script) = (dir.join("selfcheck-hy2.yaml"), dir.join("selfcheck-hy2.sh"));
    let yaml = hy2_client_yaml(state, &user.username, &user.credentials.hy2_password);
    let sh = hy2_probe_sh(&hysteria, &cfg.display().to_string());
    let out = match host
        .write_file(&cfg, yaml.as_bytes(), 0o600)
        .and_then(|()| host.write_file(&script, sh.as_bytes(), 0o700))
    {
        // 凭据在 0600 的临时文件里，无论成败都要删掉
        Ok(()) => host.run("sh", &[&script.display().to_string()]),
        Err(e) => Err(e),
    };
    let _ = host.remove_file(&cfg);
    let _ = host.remove_file(&script);
    match out {
        Ok(o) => {
            let code = o.stdout.trim().lines().last().unwrap_or_default().trim();
            if code == "200" {
                pass(name, format!("{} 经本机 HY2 出网 200", user.username))
            } else if code == "nocurl" {
                skip(
                    name,
                    "系统没有 curl（wget 不支持 socks5），跳过；装上 curl 后 `bui status` 复检",
                )
            } else {
                fail(
                    name,
                    format!(
                        "{} 经本机 HY2 打 {PROBE_URL} 得到 {}（0/000 = 连不上：看 journalctl -u hysteria-server）",
                        user.username,
                        if code.is_empty() { "空" } else { code }
                    ),
                )
            }
        }
        Err(e) => fail(name, format!("探测脚本跑不起来：{e}")),
    }
}

/// 跑一遍自检。`drift` 直接用这一轮对账报告里的漂移（不再重扫一遍）。
pub fn run(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    drift: &[DriftItem],
    wait: Wait,
) -> Vec<Check> {
    run_with_clock(host, paths, state, drift, wait, &std::thread::sleep)
}

/// [`run`] 的可注入时钟版本（测试用假 `sleep` 把那 120s 压成零）。
pub fn run_with_clock(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    drift: &[DriftItem],
    wait: Wait,
    sleep: &dyn Fn(Duration),
) -> Vec<Check> {
    // 先等：全新装机的证书、刚被 restart 的 b-ui 的 admin 口，都要几十秒才到位。
    let cert_ready = wait_ready(host, paths, state, wait, sleep);
    let mut rows = Vec::new();
    for unit in MANAGED_UNITS {
        let active = host.unit_is_active(unit).unwrap_or(false);
        rows.push(if active {
            pass(format!("单元 {unit}"), "running")
        } else if !cert_ready && needs_cert(unit) {
            skip(format!("单元 {unit}"), CERT_PENDING)
        } else {
            fail(
                format!("单元 {unit}"),
                format!("未运行：journalctl -u {unit} -n 50"),
            )
        });
    }
    // 三个校验器跑的是**已落盘**的配置（不是 .verify/ 里的候选）
    for (program, name, args) in [
        (
            "xray",
            "xray run -test",
            vec![
                "run".to_string(),
                "-test".to_string(),
                "-c".to_string(),
                paths
                    .base_dir
                    .join("xray-config.json")
                    .display()
                    .to_string(),
            ],
        ),
        (
            "sing-box",
            "sing-box check",
            vec![
                "check".to_string(),
                "-c".to_string(),
                paths
                    .base_dir
                    .join("singbox-relay.json")
                    .display()
                    .to_string(),
            ],
        ),
        (
            "caddy",
            "caddy validate",
            vec![
                "validate".to_string(),
                "--config".to_string(),
                crate::paths::caddyfile(paths).display().to_string(),
                "--adapter".to_string(),
                "caddyfile".to_string(),
            ],
        ),
    ] {
        let Some(bin) = kernel_bin(host, paths, program) else {
            rows.push(skip(name, format!("{program} 二进制不在，跳过")));
            continue;
        };
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        rows.push(match host.run(&bin, &argv) {
            Ok(o) if o.ok() => pass(name, "配置合法"),
            Ok(o) => fail(
                name,
                if o.stderr.trim().is_empty() {
                    o.stdout.trim().to_string()
                } else {
                    o.stderr.trim().to_string()
                },
            ),
            Err(e) => fail(name, format!("跑不起来：{e}")),
        });
    }
    // 关键端口在监听
    let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
    let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
    let label = |proto: Proto, port: u16| {
        format!(
            "{port}/{}",
            match proto {
                Proto::Tcp => "tcp",
                Proto::Udp => "udp",
            }
        )
    };
    // 没在听的端口分两堆：等证书的那两个 UDP 口（SKIP）与真有问题的（FAIL）
    let (mut pending, mut missing) = (Vec::new(), Vec::new());
    for (proto, port) in listen_ports(state) {
        let listening = match proto {
            Proto::Tcp => tcp.contains(&port),
            Proto::Udp => udp.contains(&port),
        };
        if listening {
            continue;
        }
        if !cert_ready && cert_bound_port(state, proto, port) {
            pending.push(label(proto, port));
        } else {
            missing.push(label(proto, port));
        }
    }
    rows.push(if !missing.is_empty() {
        fail("关键端口", format!("没在监听：{}", missing.join("、")))
    } else if !pending.is_empty() {
        skip("关键端口", format!("{} {CERT_PENDING}", pending.join("、")))
    } else {
        pass("关键端口", "443/REALITY/HY2/面板 全部在监听")
    });
    // 漂移为空
    rows.push(if drift.is_empty() {
        pass("漂移", "无漂移")
    } else {
        fail(
            "漂移",
            drift
                .iter()
                .map(|d| format!("{} {}", d.kind, d.path))
                .collect::<Vec<_>>()
                .join("；"),
        )
    });
    rows.push(hy2_loopback(host, paths, state, cert_ready));
    rows
}

/// PASS/FAIL 表（装机最后一屏）。
pub fn table(rows: &[Check]) -> String {
    // 名字列按**显示宽度**对齐：「关键端口」占 8 列而 chars() 只数 4
    let width = rows
        .iter()
        .map(|c| crate::sys::env_probe::display_width(&c.name))
        .max()
        .unwrap_or(0);
    let mut out = vec![format!(
        "自检（{} 项，{} 项未通过）",
        rows.len(),
        failures(rows)
    )];
    for c in rows {
        out.push(format!(
            "  {}  {}  {}",
            c.verdict.label(),
            crate::sys::env_probe::pad_display(&c.name, width),
            c.detail,
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;

    /// 测试一律走不等待的时钟版：真 `run` 会睡满 120s。
    fn no_wait(h: &dyn Host, p: &Paths, state: &State, drift: &[DriftItem]) -> Vec<Check> {
        run_with_clock(h, p, state, drift, Wait::NONE, &|_| {})
    }

    fn paths(d: &tempfile::TempDir) -> Paths {
        Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    /// 一台「什么都对」的假机器：六单元在跑、四内核在位、证书已同步、关键端口在听、探测脚本回 200。
    fn healthy(p: &Paths, state: &State) -> FakeHost {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert(p.certs_dir.join("fullchain.pem"), (b"CERT".to_vec(), 0o644));
            for u in MANAGED_UNITS {
                i.units_active.insert(format!("{u}.service"));
            }
            for k in crate::kernels::KERNELS {
                i.files.insert(p.bin_dir.join(k), (b"ELF".to_vec(), 0o755));
            }
            let (mut tcp, mut udp) = (
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
            );
            for (proto, port) in listen_ports(state) {
                match proto {
                    Proto::Tcp => tcp.insert(port),
                    Proto::Udp => udp.insert(port),
                };
            }
            i.listening.insert(Proto::Tcp, tcp);
            i.listening.insert(Proto::Udp, udp);
            i.scripted.push(("sh ".into(), CmdOut::success("200\n")));
        });
        h
    }

    #[test]
    fn a_healthy_machine_passes_every_check() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        // 六单元 + 三校验器 + 端口 + 漂移 + HY2 回环
        assert_eq!(rows.len(), MANAGED_UNITS.len() + 6);
        let t = table(&rows);
        for want in [
            "单元 b-ui",
            "单元 caddy",
            "xray run -test",
            "sing-box check",
            "caddy validate",
            "关键端口",
            "无漂移",
            "HY2 回环鉴权",
        ] {
            assert!(t.contains(want), "自检表缺「{want}」：\n{t}");
        }
        assert!(!t.contains("FAIL"), "{t}");
    }

    #[test]
    fn every_kind_of_breakage_shows_up_as_fail() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.units_active.remove("caddy.service");
            i.listening.insert(Proto::Udp, Default::default());
            i.scripted.insert(
                0,
                (
                    format!("{} run -test", p.bin_dir.join("xray").display()),
                    CmdOut::failure(1, "bad inbound"),
                ),
            );
            // 回环探测：客户端起不来，脚本打印 000
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::success("000\n")));
        });
        let rows = no_wait(
            &h,
            &p,
            &state,
            &[DriftItem {
                kind: "cron".into(),
                path: "crontab".into(),
                detail: "0 */6 * * * /opt/b-ui/update.sh".into(),
            }],
        );
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(by("单元 caddy").verdict, Verdict::Fail);
        assert!(by("单元 caddy").detail.contains("journalctl -u caddy"));
        assert_eq!(by("单元 b-ui").verdict, Verdict::Pass);
        assert_eq!(by("xray run -test").verdict, Verdict::Fail);
        assert_eq!(by("xray run -test").detail, "bad inbound");
        assert_eq!(by("关键端口").verdict, Verdict::Fail);
        assert!(
            by("关键端口").detail.contains("10000/udp")
                && by("关键端口").detail.contains("40000/udp"),
            "{:?}",
            by("关键端口").detail
        );
        assert_eq!(by("漂移").verdict, Verdict::Fail);
        assert!(by("漂移").detail.contains("crontab"));
        assert_eq!(by("HY2 回环鉴权").verdict, Verdict::Fail);
        assert!(by("HY2 回环鉴权").detail.contains("000"));
        // caddy 没起 + xray 校验失败 + 端口没听 + 有漂移 + HY2 回环不通
        assert_eq!(failures(&rows), 5);
        assert!(table(&rows).contains("5 项未通过"));
    }

    /// 全新装机没有用户 ⇒ HY2 回环无从可试，SKIP 而不是 FAIL（否则一行命令装完必然退 2）。
    #[test]
    fn a_fresh_machine_skips_the_loopback_probe_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut state = crate::testutil::sample_state();
        state.users.clear();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        let probe = rows.iter().find(|c| c.name == "HY2 回环鉴权").unwrap();
        assert_eq!(probe.verdict, Verdict::Skip);
        assert!(probe.detail.contains("还没有用户"), "{}", probe.detail);
        assert_eq!(failures(&rows), 0);
        // 一条 sh 都不该跑（没有用户就不写临时配置）
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:sh")),
            "{:?}",
            h.ops()
        );
    }

    /// 凭据只进 0600 的临时配置、不进 argv，且无论成败都清理掉。
    #[test]
    fn the_probe_keeps_credentials_out_of_argv_and_always_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let pw = state.users[0].credentials.hy2_password.clone();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::failure(1, "boom")))
        });
        let rows = no_wait(&h, &p, &state, &[]);
        assert_eq!(
            rows.iter()
                .find(|c| c.name == "HY2 回环鉴权")
                .unwrap()
                .verdict,
            Verdict::Fail
        );
        let cfg = crate::paths::verify_dir(&p).join("selfcheck-hy2.yaml");
        let sh = crate::paths::verify_dir(&p).join("selfcheck-hy2.sh");
        assert_eq!(h.mode(&cfg.display().to_string()), None, "临时配置必须删掉");
        assert_eq!(h.mode(&sh.display().to_string()), None, "临时脚本必须删掉");
        let ops = h.ops();
        assert!(
            ops.contains(&format!("write:{}:600", cfg.display())),
            "配置要 0600：{ops:?}"
        );
        assert!(
            ops.contains(&format!("remove:{}", cfg.display())),
            "{ops:?}"
        );
        assert!(ops.contains(&format!("remove:{}", sh.display())), "{ops:?}");
        for op in &ops {
            assert!(!op.contains(&pw), "凭据进了命令行/流水：{op}");
        }
        // 配置里才有凭据，脚本里没有
        let yaml = hy2_client_yaml(&state, &state.users[0].username, &pw);
        assert!(
            yaml.contains(&format!("auth: {}:{pw}", state.users[0].username)),
            "{yaml}"
        );
        assert!(
            yaml.contains(&format!("listen: 127.0.0.1:{PROBE_SOCKS_PORT}")),
            "{yaml}"
        );
        let sh_text = hy2_probe_sh(
            "/opt/b-ui/bin/hysteria",
            "/opt/b-ui/.verify/selfcheck-hy2.yaml",
        );
        assert!(!sh_text.contains(&pw), "{sh_text}");
        assert!(
            sh_text.contains("--socks5-hostname 127.0.0.1:45899"),
            "{sh_text}"
        );
        assert!(sh_text.contains(PROBE_URL), "{sh_text}");
        assert!(sh_text.contains("%{http_code}"), "{sh_text}");
    }

    #[test]
    fn obfs_is_carried_into_the_probe_config_only_when_enabled() {
        let mut state = crate::testutil::sample_state();
        let u = state.users[0].username.clone();
        let pw = state.users[0].credentials.hy2_password.clone();
        assert!(!hy2_client_yaml(&state, &u, &pw).contains("obfs"));
        state.node.obfs = bui_schema::model::Obfs {
            enabled: true,
            password: "obfs-pw".into(),
        };
        let y = hy2_client_yaml(&state, &u, &pw);
        assert!(
            y.contains("type: salamander") && y.contains("password: obfs-pw"),
            "{y}"
        );
    }

    /// 精简系统（只装了 wget）：install.sh 刻意支持那种机器，带用户的 import-v3 路径不该因为
    /// 缺 curl 就把自检染红、退 2。探测脚本自己报 nocurl ⇒ 这一项 SKIP。
    #[test]
    fn a_system_without_curl_skips_the_loopback_probe_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::success("nocurl\n")));
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let c = rows.iter().find(|c| c.name == "HY2 回环鉴权").unwrap();
        assert_eq!(c.verdict, Verdict::Skip, "{:?}", c.detail);
        assert!(c.detail.contains("curl"), "{}", c.detail);
        assert_eq!(failures(&rows), 0, "缺 curl 不算失败");
        // 脚本里那一行就是 SKIP 的来源，顺带钉住「最后一屏不干等两分钟」的预算
        let sh = hy2_probe_sh("/opt/b-ui/bin/hysteria", "/x.yaml");
        assert!(sh.contains("command -v curl"), "{sh}");
        assert!(sh.contains("nocurl"), "{sh}");
        assert!(sh.contains("--max-time 4") && sh.contains("-lt 6"), "{sh}");
    }

    /// 全新装机的那台机器：一开始没证书、两个 hysteria 没起、HY2 的 UDP 口没在听
    /// （`config.yaml` 的 `tls.cert` 还不存在，hysteria 启动即 FATAL 进 auto-restart）；
    /// 几轮之后 Caddy 签到、证书同步复制过来、两个单元起稳。自检必须**等到那时**再判，
    /// 一项都不 FAIL —— 否则一行命令装完必然退 2。
    #[test]
    fn a_fresh_machine_waits_for_the_first_certificate_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        let cert = p.certs_dir.join("fullchain.pem");
        // 起始态：没证书、两个 hysteria 没 active、两个 HY2 的 UDP 口没在听
        h.with(|i| {
            i.files.remove(&cert);
            i.units_active.remove("hysteria-server.service");
            i.units_active.remove("hysteria-residential.service");
            i.listening.insert(Proto::Udp, Default::default());
        });
        let rounds = std::cell::Cell::new(0u32);
        let rows = run_with_clock(&h, &p, &state, &[], Wait::default(), &|dur| {
            assert_eq!(dur, Duration::from_secs(3), "每 3s 一轮");
            rounds.set(rounds.get() + 1);
            if rounds.get() == 4 {
                // 第 4 轮：证书到位 + 两个单元起稳 + UDP 口挂上
                h.with(|i| {
                    i.files.insert(cert.clone(), (b"CERT".to_vec(), 0o644));
                    i.units_active.insert("hysteria-server.service".into());
                    i.units_active.insert("hysteria-residential.service".into());
                    let mut udp = std::collections::BTreeSet::new();
                    for (proto, port) in listen_ports(&state) {
                        if proto == Proto::Udp {
                            udp.insert(port);
                        }
                    }
                    i.listening.insert(Proto::Udp, udp);
                });
            }
        });
        assert_eq!(rounds.get(), 4, "等到就绪就不再睡");
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        assert!(!table(&rows).contains("FAIL"), "{}", table(&rows));
    }

    /// 等满预算证书还是没来（DNS 没指过来 / ACME 限流）：那三行报 SKIP 而不是 FAIL，
    /// 装机不退 2；其余各项照旧判定（这里 xray 仍然要 PASS）。
    #[test]
    fn a_certificate_that_never_arrives_skips_those_three_rows() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.files.remove(&p.certs_dir.join("fullchain.pem"));
            i.units_active.remove("hysteria-server.service");
            i.units_active.remove("hysteria-residential.service");
            i.listening.insert(Proto::Udp, Default::default());
        });
        let slept = std::cell::Cell::new(Duration::ZERO);
        let rows = run_with_clock(&h, &p, &state, &[], Wait::default(), &|d| {
            slept.set(slept.get() + d)
        });
        assert_eq!(slept.get(), Duration::from_secs(120), "等待有界：最多 120s");
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        for n in ["单元 hysteria-server", "单元 hysteria-residential"] {
            assert_eq!(by(n).verdict, Verdict::Skip, "{n}");
            assert!(by(n).detail.contains("待证书"), "{}", by(n).detail);
        }
        assert_eq!(by("关键端口").verdict, Verdict::Skip);
        assert!(
            by("关键端口").detail.contains("10000/udp")
                && by("关键端口").detail.contains("40000/udp")
                && by("关键端口").detail.contains("待证书"),
            "{}",
            by("关键端口").detail
        );
        assert_eq!(by("HY2 回环鉴权").verdict, Verdict::Skip);
        assert_eq!(by("单元 caddy").verdict, Verdict::Pass, "caddy 不等证书");
        assert_eq!(by("xray run -test").verdict, Verdict::Pass);
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        // 证书没到就别去打回环（凭据都不用写）
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:sh")),
            "{:?}",
            h.ops()
        );
    }

    /// 证书在位时该 FAIL 的还得 FAIL：别把「等证书」变成万能免罪牌。
    #[test]
    fn units_still_fail_once_the_certificate_is_there() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.units_active.remove("hysteria-server.service");
            i.listening.insert(Proto::Udp, Default::default());
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(by("单元 hysteria-server").verdict, Verdict::Fail);
        assert_eq!(by("关键端口").verdict, Verdict::Fail);
        // 单元 + 端口两项；回环探测的 scripted 仍回 200（假机器上单元起没起与脚本无关）
        assert_eq!(failures(&rows), 2, "{}", table(&rows));
    }

    #[test]
    fn missing_kernels_are_skipped_not_failed() {
        // 离线装机 / manifest 拉不到：校验器不在不该把自检染红（内核闸门另有一条）
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.files.remove(&p.bin_dir.join("caddy"));
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let c = rows.iter().find(|c| c.name == "caddy validate").unwrap();
        assert_eq!(c.verdict, Verdict::Skip);
        assert_eq!(failures(&rows), 0);
    }
}
