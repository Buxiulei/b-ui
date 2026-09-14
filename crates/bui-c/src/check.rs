//! `bui-c check`：每分钟由 `bui-c.timer` 拉起的一次性巡检。
//!
//! 三件事：204 探测（socks 模式经本地 inbound，TUN 模式直连）+ TUN 接口与默认路由核对、
//! 失败时按 1/2/4 分钟退避重启数据面单元（只有重启这一段持进程锁，拿不到就跳过本轮）、
//! 判定「今天该不该自更新」；
//! 外加 TUN 模式下幂等重放 `bui-tun` 的两条 UFW 规则（`ufw reset` 会把它们清掉）。
//! 状态落在 `/opt/bui-c/runtime.json`（0600），丢了能从零重建。

use crate::delete;
use crate::engine::Engine;
use crate::lock::{self, How, LockGuard};
use crate::net::{Net, Via};
use crate::paths::{Paths, TUN_IFACE, UNIT_MAIN};
use crate::profiles::{Mode, Profiles};
use crate::sys::{systemd, Sys};
use crate::ufw;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
pub const UPDATE_INTERVAL_S: i64 = 23 * 3600;
/// 自更新失败后的重试退避：面板与 GitHub 都不可达时别每分钟白等两个源各 15s。
pub const UPDATE_RETRY_S: i64 = 3600;
const JITTER_SPAN_S: i64 = 7200;

/// 运行时数据：丢了能从零重建，所以解析失败按默认值处理（spec §2.1 的客户端版）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Runtime {
    /// 已经连着重启过几次（成功探测清零）。
    pub fail_streak: u32,
    /// 上次重启的 epoch 秒。
    pub last_restart_at: Option<i64>,
    /// 上一次「成功」自更新。
    pub last_update_at: Option<i64>,
    /// 上一次「尝试」自更新（失败也记，用于 1h 退避）。
    pub last_update_attempt_at: Option<i64>,
    /// 已加过 bui-tun 放行规则（T12 写，本模块只读写字段）。
    pub ufw_rules: bool,
    /// 上一次检查更新的结论：有能装的新东西（`cli::new_version_pending`：自身有新版或同版本新构建，
    /// 或内核要换）。主菜单 `[7] 更新与维护 ★` 读它，不为了渲染一屏菜单去联网。由 `update` 子命令、
    /// 菜单 [7] → [1] 与巡检里的自更新写。
    pub update_available: bool,
    /// 上一次检查更新的时间（epoch 秒），与 `update_available` 同时写。
    pub update_checked_at: Option<i64>,
    /// 上一次检查更新时 manifest 的版本号，与 `update_available` 同时写；[7] 子页的
    /// 「上次检查   有新版 4.0.1（2 小时前）」读它（spec §8.1）。旧版本写的文件没有这个键。
    #[serde(default)]
    pub update_version: Option<String>,
}

impl Runtime {
    pub fn load<S: Sys>(sys: &S, paths: &Paths) -> Self {
        sys.read(&paths.runtime())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save<S: Sys>(&self, sys: &S, paths: &Paths) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(self)
            .map_err(|e| crate::Error::parse("runtime.json", e.to_string()))?;
        data.push(b'\n');
        sys.mkdir_p(&paths.base)?;
        sys.write(&paths.runtime(), &data, 0o600)
    }
}

/// 一项巡检失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    UnitDown,
    Probe { got: Option<u16> },
    TunMissing,
    TunNoDefaultRoute,
}

/// 巡检日志（journald）与 `bui-c check` 输出里的中文说法；不打 Rust 的 Debug 名。
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 探测目标只写主机与路径：scheme 对读日志的人没有信息量
        let target = PROBE_URL.trim_start_matches("https://");
        match self {
            Failure::UnitDown => write!(f, "{UNIT_MAIN} 没在运行"),
            Failure::Probe { got: Some(code) } => write!(f, "探测 {target} 返回 HTTP {code}"),
            Failure::Probe { got: None } => write!(f, "探测 {target} 超时或连不上"),
            Failure::TunMissing => write!(f, "{TUN_IFACE} 接口不存在"),
            Failure::TunNoDefaultRoute => write!(f, "默认路由没有指向 {TUN_IFACE}"),
        }
    }
}

/// 一次巡检的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    NoProfile,
    Ok,
    Restarted {
        failures: Vec<Failure>,
        next_backoff_min: u64,
        /// TUN 模式下重启后等接口的结果（同一把锁里等的）；SOCKS 模式没有接口可等，为 `None`。
        tun_ready: Option<bool>,
    },
    Waiting {
        failures: Vec<Failure>,
        remaining_s: i64,
    },
    /// 要重启时锁被别的 bui-c 占着，或拿到锁后发现节点设置已经被改过：本轮跳过，
    /// 不重启、不写 `runtime.json`（spec §8.3、§5.9）。
    Busy,
}

/// 退避档位（分钟）：第 0/1/≥2 档分别 1/2/4。
pub fn backoff_minutes(fail_streak: u32) -> u64 {
    match fail_streak {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

/// 距上次重启至少要等多久才允许再重启。
///
/// `fail_streak` 是「已经重启过几次」，所以用 k-1 取档：重启 1 次后等 1 分钟、
/// 2 次后 2 分钟、3 次起 4 分钟。直接用 k 会让 spec §6 的「1 分钟」档永远不出现。
pub fn wait_seconds(fail_streak: u32) -> i64 {
    backoff_minutes(fail_streak.saturating_sub(1)) as i64 * 60
}

/// 探测走哪条腿：TUN 已接管全局路由，直连探测就是经隧道；SOCKS 模式必须经本地 inbound
/// 才验证得到隧道。巡检与菜单 `[5]`（[`crate::nettest`]）共用这一处（spec §6.6）。
pub fn via_for(prof: &Profiles) -> Via {
    match prof.mode {
        Mode::Tun => Via::Direct,
        Mode::Socks => Via::Socks5 {
            port: prof.socks_port,
        },
    }
}

/// 逐项探测，收集**全部**失败项（不短路：日志里要能一眼看出是单元没起还是路由没接管）。
pub fn probe<S: Sys, N: Net>(sys: &S, net: &N, paths: &Paths, prof: &Profiles) -> Vec<Failure> {
    let mut out = Vec::new();
    if !systemd::is_active(sys, UNIT_MAIN) {
        out.push(Failure::UnitDown);
    }
    match net.status(PROBE_URL, via_for(prof), PROBE_TIMEOUT) {
        Ok(204) => {}
        Ok(code) => out.push(Failure::Probe { got: Some(code) }),
        Err(_) => out.push(Failure::Probe { got: None }),
    }
    if prof.mode == Mode::Tun {
        let e = Engine::new(sys, paths);
        if !e.tun_up() {
            out.push(Failure::TunMissing);
        }
        if !e.tun_default_route() {
            out.push(Failure::TunNoDefaultRoute);
        }
    }
    out
}

/// 探测之后怎么办（[`decide`] 的结论）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 各项都好：清零连击（有的话），不拿锁。
    Healthy,
    /// 还在退避窗口里：等，不拿锁。
    Wait {
        failures: Vec<Failure>,
        remaining_s: i64,
    },
    /// 该重启了：只试一次锁，拿到了才重启。
    Restart { failures: Vec<Failure> },
}

/// 纯函数：探测结果 + 运行时记账 → 这一轮做什么。退避窗口按 [`wait_seconds`] 算，从上次重启起。
pub fn decide(rt: &Runtime, now: i64, failures: Vec<Failure>) -> Decision {
    if failures.is_empty() {
        return Decision::Healthy;
    }
    if let Some(last) = rt.last_restart_at {
        let (elapsed, wait_s) = (now - last, wait_seconds(rt.fail_streak));
        if elapsed < wait_s {
            return Decision::Wait {
                failures,
                remaining_s: wait_s - elapsed,
            };
        }
    }
    Decision::Restart { failures }
}

/// 一次巡检（`bui-c.timer`，spec §8.3、§5.9）：探测（锁外）→ [`decide`] → 要重启时**只试一次锁**。
///
/// 拿不到锁返回 [`Verdict::Busy`]，不重启、不写 `runtime.json`。拿到锁后重读 `profiles.json`，
/// 与探测时的完整快照（节点名、active、mode、两个端口）比，任何一项变了也是 `Busy`：别人刚 apply
/// 好的服务，不能拿旧设置的探测结果再重启一次（§0.2 R11）。重启与等 TUN 就绪都在这把锁里。
pub fn run<S: Sys, N: Net>(sys: &S, net: &N, paths: &Paths) -> Result<Verdict> {
    let prof = Profiles::load(sys, paths)?;
    if prof.active_profile().is_none() {
        return Ok(Verdict::NoProfile);
    }
    let seen = delete::snapshot(&prof);
    let mut rt = Runtime::load(sys, paths);
    let now = sys.now().unix_timestamp();

    // 幂等重放 bui-tun 的两条 UFW 规则：`ufw reset` / 重装 ufw 会把它们清掉，
    // 而 UFW 默认 FORWARD DROP 会掐掉隧道转发的 TCP。放在探测之前，好让这一轮就恢复。
    // 失败只记日志：巡检的结论是「隧道通不通」，不该被防火墙报错顶掉。
    if rt.ufw_rules && prof.mode == Mode::Tun {
        match ufw::ensure_tun(sys) {
            Ok(true) => tracing::info!("已重放 bui-tun 的 UFW 放行规则"),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "UFW 规则重放失败"),
        }
    }

    let failures = match decide(&rt, now, probe(sys, net, paths, &prof)) {
        Decision::Healthy => {
            if rt.fail_streak != 0 {
                rt.fail_streak = 0;
                rt.save(sys, paths)?;
            }
            return Ok(Verdict::Ok);
        }
        Decision::Wait {
            failures,
            remaining_s,
        } => {
            return Ok(Verdict::Waiting {
                failures,
                remaining_s,
            })
        }
        Decision::Restart { failures } => failures,
    };

    let Some(g) = lock::acquire(sys, paths, How::Once)? else {
        tracing::info!("另一个 bui-c 操作进行中，本轮巡检跳过");
        return Ok(Verdict::Busy);
    };
    let fresh = Profiles::load(sys, paths)?;
    if delete::snapshot(&fresh) != seen {
        tracing::info!("节点设置在探测之后被改过，本轮巡检跳过");
        return Ok(Verdict::Busy);
    }
    let mut v = restart(sys, paths, failures, &g)?;
    if fresh.mode == Mode::Tun {
        // is-active 在 exec 之后立刻为真，接口还没起来：在同一把锁里等（最多 5 秒）
        let ready = Engine::new(sys, paths).wait_tun_ready();
        if let Verdict::Restarted { tun_ready, .. } = &mut v {
            *tun_ready = Some(ready);
        }
    }
    Ok(v)
}

/// 巡检的 restart 段（持锁，spec §0.2 R11）：重启主单元，记一次连击与重启时间。不管退避——
/// 退避由调用方判（巡检经 [`decide`]；菜单 `[5]` 人就在跟前，点了就是要修）。`runtime.json`
/// 在锁里重读：刚才别的会话记下的重启也要算进连击，timer 之后的退避从这次算起。
///
/// 只重启、不等就绪：巡检在 [`run`] 里等 TUN，菜单 `[5]` 按模式等（SOCKS 等端口）。
pub fn restart<S: Sys>(
    sys: &S,
    paths: &Paths,
    failures: Vec<Failure>,
    _: &LockGuard,
) -> Result<Verdict> {
    systemd::restart(sys, UNIT_MAIN)?;
    let mut rt = Runtime::load(sys, paths);
    rt.fail_streak = rt.fail_streak.saturating_add(1);
    rt.last_restart_at = Some(sys.now().unix_timestamp());
    rt.save(sys, paths)?;
    // 报的就是「这次重启之后要等几分钟」，与 wait_seconds 同一个数
    Ok(Verdict::Restarted {
        failures,
        next_backoff_min: (wait_seconds(rt.fail_streak) / 60) as u64,
        tun_ready: None,
    })
}

/// 每日自更新判定：23 小时 + 机器码派生的抖动（避免同一批机器同时打面板）。
///
/// 另有 1 小时的失败退避：`last_update_attempt_at` 由调用方（T12 的 `Cmd::Check`）在
/// **发请求之前**写下，所以面板与 GitHub 都不可达时，不会每分钟白等两个源各 15s。
pub fn update_due<S: Sys>(sys: &S, rt: &Runtime, prof: &Profiles) -> bool {
    if !prof.auto_update {
        return false;
    }
    let now = sys.now().unix_timestamp();
    if let Some(t) = rt.last_update_attempt_at {
        if now - t < UPDATE_RETRY_S {
            return false;
        }
    }
    match rt.last_update_at {
        None => true,
        Some(last) => now - last >= UPDATE_INTERVAL_S + jitter_s(sys),
    }
}

/// 抖动秒数：`/etc/machine-id` 派生，同机稳定，读不到就 0。
pub fn jitter_s<S: Sys>(sys: &S) -> i64 {
    let id = match sys.read(std::path::Path::new("/etc/machine-id")) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let sum: i64 = id
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|b| *b as i64)
        .sum();
    sum % JITTER_SPAN_S
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys};
    use crate::testutil::{profiles_socks, profiles_tun};
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    fn healthy_socks(s: &FakeSys, n: &FakeNet) {
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        n.route(PROBE_URL, FakeReply::Status(204));
        profiles_socks().save(s, &paths()).unwrap();
    }

    #[test]
    fn failures_read_as_chinese_sentences() {
        assert_eq!(Failure::UnitDown.to_string(), "bui-c.service 没在运行");
        assert_eq!(
            Failure::Probe { got: Some(403) }.to_string(),
            "探测 www.gstatic.com/generate_204 返回 HTTP 403"
        );
        assert_eq!(
            Failure::Probe { got: None }.to_string(),
            "探测 www.gstatic.com/generate_204 超时或连不上"
        );
        assert_eq!(Failure::TunMissing.to_string(), "bui-tun 接口不存在");
        assert_eq!(
            Failure::TunNoDefaultRoute.to_string(),
            "默认路由没有指向 bui-tun"
        );
    }

    #[test]
    fn via_for_goes_direct_under_tun_and_through_the_local_socks_port_otherwise() {
        assert_eq!(via_for(&profiles_tun()), Via::Direct);
        let mut p = profiles_socks();
        p.socks_port = 10808;
        assert_eq!(via_for(&p), Via::Socks5 { port: 10808 });
    }

    #[test]
    fn no_profile_is_not_a_failure() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::NoProfile);
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "没配置节点时不该重启单元"
        );
    }

    #[test]
    fn healthy_socks_probe_is_ok_and_clears_streak() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_socks(&s, &n);
        Runtime {
            fail_streak: 2,
            last_restart_at: Some(0),
            ..Runtime::default()
        }
        .save(&s, &paths())
        .unwrap();
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert_eq!(Runtime::load(&s, &paths()).fail_streak, 0);
        assert_eq!(
            n.log()[0],
            format!("GET {PROBE_URL} via socks5:1080"),
            "socks 模式必须经本地 inbound 探测"
        );
    }

    #[test]
    fn tun_mode_probes_directly_and_checks_iface_plus_route() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        s.reply(
            "ip -4 route show table all",
            0,
            "default dev bui-tun table 2022\n",
        );
        n.route(PROBE_URL, FakeReply::Status(204));
        profiles_tun().save(&s, &paths()).unwrap();
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert_eq!(
            n.log()[0],
            format!("GET {PROBE_URL} via Direct"),
            "TUN 已接管，直连探测"
        );
    }

    /// TUN 模式下一切正常的机器（单元在跑、接口在、默认路由已接管、204 通）。
    fn healthy_tun(s: &FakeSys, n: &FakeNet) {
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        s.reply(
            "ip -4 route show table all",
            0,
            "default dev bui-tun table 2022\n",
        );
        n.route(PROBE_URL, FakeReply::Status(204));
        profiles_tun().save(s, &paths()).unwrap();
        Runtime {
            ufw_rules: true,
            ..Runtime::default()
        }
        .save(s, &paths())
        .unwrap();
    }

    const UFW_WITH_TUN_RULES: &str = "Status: active\n\n\
        To                         Action      From\n\
        --                         ------      ----\n\
        Anywhere on bui-tun        ALLOW IN    Anywhere\n\
        Anywhere                   ALLOW FWD   Anywhere on bui-tun\n";

    #[test]
    fn check_replays_the_bui_tun_ufw_rules_when_they_went_missing() {
        // 用户 `ufw reset` / 重装 ufw 之后两条规则会消失，FORWARD DROP 会掐掉隧道 TCP
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_tun(&s, &n);
        s.reply(
            "ufw status",
            0,
            "Status: active\n\nTo                Action      From\n",
        );
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert!(s.called("ufw allow in on bui-tun"), "{:?}", s.calls());
        assert!(s.called("ufw route allow in on bui-tun"), "{:?}", s.calls());
    }

    #[test]
    fn check_does_not_re_add_ufw_rules_that_are_already_there() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_tun(&s, &n);
        s.reply("ufw status", 0, UFW_WITH_TUN_RULES);
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert!(!s.called("ufw allow in on bui-tun"), "{:?}", s.calls());
        assert!(!s.called("ufw route allow in on bui-tun"));
    }

    #[test]
    fn check_leaves_ufw_alone_when_inactive_in_socks_mode_or_never_configured() {
        // 墙没启用：没有 FORWARD DROP 要绕，不去替用户开墙
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_tun(&s, &n);
        s.reply("ufw status", 0, "Status: inactive\n");
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert!(!s.called("ufw allow in on bui-tun"));

        // socks 模式：本来就不该有 bui-tun 规则
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_socks(&s, &n);
        Runtime {
            ufw_rules: true,
            ..Runtime::default()
        }
        .save(&s, &paths())
        .unwrap();
        s.reply("ufw status", 0, "Status: active\n");
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert!(!s.called("ufw allow in on bui-tun"));
        assert!(!s.calls().iter().any(|c| c == "ufw status"));

        // 从来没加过规则（ufw_rules=false）→ 连 ufw 都不问
        let s = FakeSys::new();
        let n = FakeNet::new();
        healthy_tun(&s, &n);
        Runtime::default().save(&s, &paths()).unwrap();
        s.reply("ufw status", 0, "Status: active\n");
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Ok);
        assert!(!s.calls().iter().any(|c| c == "ufw status"));
    }

    #[test]
    fn tun_without_default_route_is_a_failure() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        s.reply(
            "ip -4 route show table all",
            0,
            "default via 203.0.113.1 dev eth0\n",
        );
        n.route(PROBE_URL, FakeReply::Status(204));
        profiles_tun().save(&s, &paths()).unwrap();
        assert_eq!(
            probe(&s, &n, &paths(), &profiles_tun()),
            vec![Failure::TunNoDefaultRoute]
        );
    }

    #[test]
    fn probe_collects_all_failures() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.reply("ip link show bui-tun", 1, "");
        s.reply("ip -4 route show table all", 0, "");
        n.route(PROBE_URL, FakeReply::Status(403));
        let f = probe(&s, &n, &paths(), &profiles_tun());
        assert_eq!(
            f,
            vec![
                Failure::UnitDown,
                Failure::Probe { got: Some(403) },
                Failure::TunMissing,
                Failure::TunNoDefaultRoute
            ]
        );
    }

    #[test]
    fn probe_network_error_reports_none_status() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        n.route(PROBE_URL, FakeReply::Fail("connect timeout".into()));
        assert_eq!(
            probe(&s, &n, &paths(), &profiles_socks()),
            vec![Failure::Probe { got: None }]
        );
    }

    #[test]
    fn first_failure_restarts_immediately_then_backs_off_1_2_4() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        profiles_socks().save(&s, &paths()).unwrap();
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        n.route(PROBE_URL, FakeReply::Fail("no route".into()));
        let restarts = || {
            s.calls()
                .iter()
                .filter(|c| *c == "systemctl restart bui-c.service")
                .count()
        };

        // t=0：第一次失败，立刻重启；重启之后的等待档位是 1 分钟
        let v = run(&s, &n, &paths()).unwrap();
        assert!(
            matches!(
                v,
                Verdict::Restarted {
                    next_backoff_min: 1,
                    ..
                }
            ),
            "{v:?}"
        );
        assert!(s.called("systemctl restart bui-c.service"));
        assert_eq!(Runtime::load(&s, &paths()).fail_streak, 1);

        // t=30：还在 1 分钟窗口里 → 只等待，不重启
        s.advance(30);
        let v = run(&s, &n, &paths()).unwrap();
        assert_eq!(
            v,
            Verdict::Waiting {
                failures: vec![Failure::UnitDown, Failure::Probe { got: None }],
                remaining_s: 30
            }
        );
        assert_eq!(restarts(), 1);

        // t=70（距上次重启 70s ≥ 60s）→ 第二次重启，之后的档位是 2 分钟
        s.advance(40);
        let v = run(&s, &n, &paths()).unwrap();
        assert!(
            matches!(
                v,
                Verdict::Restarted {
                    next_backoff_min: 2,
                    ..
                }
            ),
            "{v:?}"
        );
        assert_eq!(restarts(), 2);
        assert_eq!(Runtime::load(&s, &paths()).fail_streak, 2);

        // t=130（距上次重启 60s < 120s）→ 等待，剩 60s
        s.advance(60);
        let v = run(&s, &n, &paths()).unwrap();
        assert!(
            matches!(
                v,
                Verdict::Waiting {
                    remaining_s: 60,
                    ..
                }
            ),
            "{v:?}"
        );
        assert_eq!(restarts(), 2);

        // t=190（距上次重启 120s ≥ 120s）→ 第三次重启，之后封顶 4 分钟
        s.advance(60);
        let v = run(&s, &n, &paths()).unwrap();
        assert!(
            matches!(
                v,
                Verdict::Restarted {
                    next_backoff_min: 4,
                    ..
                }
            ),
            "{v:?}"
        );
        assert_eq!(restarts(), 3);
        assert_eq!(Runtime::load(&s, &paths()).fail_streak, 3);
    }

    /// 菜单 `[5]` 的修复直接调 [`restart`]：人就在跟前，发现异常就重启，不理 timer 的退避窗口；
    /// 但照样记下这次重启，timer 接下来仍按连击退避。
    #[test]
    fn a_locked_restart_ignores_the_backoff_window_and_still_records_it() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        profiles_socks().save(&s, &paths()).unwrap();
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        n.route(PROBE_URL, FakeReply::Fail("no route".into()));
        let restarts = || {
            s.calls()
                .iter()
                .filter(|c| *c == "systemctl restart bui-c.service")
                .count()
        };

        assert!(matches!(
            run(&s, &n, &paths()).unwrap(),
            Verdict::Restarted { .. }
        ));
        s.advance(30); // 还在 1 分钟窗口里：timer 会等，手动不等
        let g = lock::acquire(&s, &paths(), How::Once).unwrap().unwrap();
        let v = restart(&s, &paths(), Vec::new(), &g).unwrap();
        drop(g);
        assert!(
            matches!(
                v,
                Verdict::Restarted {
                    next_backoff_min: 2,
                    ..
                }
            ),
            "{v:?}"
        );
        assert_eq!(restarts(), 2);
        let rt = Runtime::load(&s, &paths());
        assert_eq!(rt.fail_streak, 2, "手动重启也算一次连击");
        assert_eq!(rt.last_restart_at, Some(s.now().unix_timestamp()));

        // 紧接着 timer 巡检：从这次手动重启算退避
        s.advance(30);
        assert!(matches!(
            run(&s, &n, &paths()).unwrap(),
            Verdict::Waiting { .. }
        ));
        assert_eq!(restarts(), 2);

        // 重启本身失败：什么都不记，错误原样交回
        let s = FakeSys::new();
        s.reply("systemctl restart bui-c.service", 1, "");
        let g = lock::acquire(&s, &paths(), How::Once).unwrap().unwrap();
        assert!(restart(&s, &paths(), Vec::new(), &g).is_err());
        assert_eq!(Runtime::load(&s, &paths()), Runtime::default());
        assert_eq!(s.writes("/opt/bui-c/runtime.json"), 0);
    }

    #[test]
    fn decide_is_healthy_waits_inside_the_window_and_restarts_after_it() {
        let rt = Runtime {
            fail_streak: 1,
            last_restart_at: Some(1_000),
            ..Runtime::default()
        };
        let f = || vec![Failure::UnitDown];
        assert_eq!(decide(&rt, 1_030, Vec::new()), Decision::Healthy);
        assert_eq!(
            decide(&rt, 1_030, f()),
            Decision::Wait {
                failures: f(),
                remaining_s: 30
            }
        );
        assert_eq!(decide(&rt, 1_060, f()), Decision::Restart { failures: f() });
        assert_eq!(
            decide(&Runtime::default(), 0, f()),
            Decision::Restart { failures: f() },
            "从没重启过：第一次失败立刻重启"
        );
    }

    /// 一台要重启的 SOCKS 机器：单元没在跑、探测不通。
    fn broken_socks(s: &FakeSys, n: &FakeNet) {
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        n.route(PROBE_URL, FakeReply::Fail("no route".into()));
        profiles_socks().save(s, &paths()).unwrap();
    }

    #[test]
    fn the_timer_check_returns_busy_and_writes_nothing_when_locked() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        broken_socks(&s, &n);
        s.lock_busy(u32::MAX);
        let before = s.writes("/opt/bui-c/runtime.json");
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Busy);
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "锁被占着就不重启：{:?}",
            s.calls()
        );
        assert_eq!(
            s.writes("/opt/bui-c/runtime.json"),
            before,
            "不写 runtime.json、不动 fail_streak"
        );
        assert!(s.sleeps().is_empty(), "巡检只试一次，不等锁");
        // 锁放开之后的下一轮照常重启：Busy 没有留下任何会挡住它的记账
        s.lock_busy(0);
        assert!(matches!(
            run(&s, &n, &paths()).unwrap(),
            Verdict::Restarted { .. }
        ));
    }

    #[test]
    fn the_timer_check_skips_when_the_snapshot_changed_under_it() {
        // 探测时是 SOCKS；拿到锁之前另一个会话把它切成了 TUN（刚 apply 好）。拿旧模式的探测结果
        // 再重启一次，只会把刚切好的服务打断（spec §0.2 R11）
        let s = FakeSys::new();
        let n = FakeNet::new();
        broken_socks(&s, &n);
        let tun = String::from_utf8(serde_json::to_vec_pretty(&profiles_tun()).unwrap()).unwrap();
        s.stage_on_lock("/opt/bui-c/profiles.json", &tun);
        let before = s.writes("/opt/bui-c/runtime.json");
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Busy);
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "{:?}",
            s.calls()
        );
        assert_eq!(s.writes("/opt/bui-c/runtime.json"), before);
        let calls = s.calls();
        let lock = calls.iter().position(|c| c == "lock").expect("拿过锁");
        let unlock = calls.iter().position(|c| c == "unlock").expect("放了锁");
        assert!(lock < unlock, "{calls:?}");

        // 只改了端口也算变了：快照是完整的（节点名、active、mode、socks_port、http_port）
        let s = FakeSys::new();
        let n = FakeNet::new();
        broken_socks(&s, &n);
        let mut moved = profiles_socks();
        moved.http_port += 1;
        let moved = String::from_utf8(serde_json::to_vec_pretty(&moved).unwrap()).unwrap();
        s.stage_on_lock("/opt/bui-c/profiles.json", &moved);
        assert_eq!(run(&s, &n, &paths()).unwrap(), Verdict::Busy);
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn the_timer_restart_and_the_tun_wait_happen_inside_the_lock() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        n.route(PROBE_URL, FakeReply::Status(502));
        profiles_tun().save(&s, &paths()).unwrap();
        let v = run(&s, &n, &paths()).unwrap();
        assert!(
            matches!(
                v,
                Verdict::Restarted {
                    tun_ready: Some(true),
                    ..
                }
            ),
            "{v:?}"
        );
        let calls = s.calls();
        let at = |c: &str| calls.iter().rposition(|x| x == c).unwrap_or(usize::MAX);
        let lock = calls.iter().position(|c| c == "lock").expect("拿过锁");
        let restart = at("systemctl restart bui-c.service");
        let waited = at("ip link show bui-tun");
        let unlock = at("unlock");
        assert!(lock < restart, "{calls:?}");
        assert!(
            restart < waited && waited < unlock,
            "等 TUN 就绪也在锁里：{calls:?}"
        );
        assert_eq!(Runtime::load(&s, &paths()).fail_streak, 1);
    }

    #[test]
    fn backoff_ladder_is_1_2_4_capped() {
        assert_eq!(
            (
                backoff_minutes(0),
                backoff_minutes(1),
                backoff_minutes(2),
                backoff_minutes(9)
            ),
            (1, 2, 4, 4)
        );
    }

    #[test]
    fn wait_seconds_uses_the_streak_minus_one_so_the_1_minute_step_actually_happens() {
        // fail_streak 是「已经重启过几次」：重启 1 次之后等 60s，2 次之后等 120s，3 次起 240s
        assert_eq!(wait_seconds(1), 60);
        assert_eq!(wait_seconds(2), 120);
        assert_eq!(wait_seconds(3), 240);
        assert_eq!(wait_seconds(9), 240);
        // 从未重启过（成功探测会把 streak 清零）也按最小档，避免刚重启完又连着重启
        assert_eq!(wait_seconds(0), 60);
    }

    #[test]
    fn runtime_survives_a_corrupt_file() {
        let s = FakeSys::new();
        s.put("/opt/bui-c/runtime.json", "{oops");
        assert_eq!(
            Runtime::load(&s, &paths()),
            Runtime::default(),
            "运行时数据丢了要能从零重建，不是报错退出"
        );
    }

    #[test]
    fn runtime_is_0600() {
        let s = FakeSys::new();
        Runtime::default().save(&s, &paths()).unwrap();
        assert_eq!(s.mode("/opt/bui-c/runtime.json"), Some(0o600));
    }

    #[test]
    fn update_due_respects_switch_and_interval_and_jitter() {
        let s = FakeSys::new();
        s.put("/etc/machine-id", "0123456789abcdef0123456789abcdef\n");
        let mut prof = profiles_socks();
        let now = s.now().unix_timestamp();
        let j = jitter_s(&s);
        assert!((0..7200).contains(&j));

        // 从未更新过 → 该更新
        assert!(update_due(&s, &Runtime::default(), &prof));
        // 刚更新过 → 不更新
        let rt = Runtime {
            last_update_at: Some(now),
            ..Runtime::default()
        };
        assert!(!update_due(&s, &rt, &prof));
        // 超过 23h + 抖动 → 该更新
        let rt = Runtime {
            last_update_at: Some(now - UPDATE_INTERVAL_S - j - 1),
            ..Runtime::default()
        };
        assert!(update_due(&s, &rt, &prof));
        // 关掉自动更新 → 永不更新
        prof.auto_update = false;
        assert!(!update_due(&s, &Runtime::default(), &prof));
    }

    #[test]
    fn update_due_backs_off_an_hour_after_a_failed_attempt() {
        let s = FakeSys::new();
        let prof = profiles_socks();
        let now = s.now().unix_timestamp();
        // 尝试过但没成功（面板/GitHub 都不可达）→ 1 小时内不再试，
        // 否则 timer 每分钟都要白等两个源各 15s
        let rt = Runtime {
            last_update_attempt_at: Some(now - 59 * 60),
            ..Runtime::default()
        };
        assert!(!update_due(&s, &rt, &prof));
        let rt = Runtime {
            last_update_attempt_at: Some(now - UPDATE_RETRY_S),
            ..Runtime::default()
        };
        assert!(update_due(&s, &rt, &prof));
    }

    #[test]
    fn jitter_is_stable_for_the_same_machine_and_zero_without_machine_id() {
        let s = FakeSys::new();
        s.put("/etc/machine-id", "aaaa\n");
        assert_eq!(jitter_s(&s), jitter_s(&s));
        let s2 = FakeSys::new();
        assert_eq!(jitter_s(&s2), 0);
    }
}
