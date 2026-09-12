//! `bui-c check`：每分钟由 `bui-c.timer` 拉起的一次性巡检。
//!
//! 三件事：204 探测（socks 模式经本地 inbound，TUN 模式直连）+ TUN 接口与默认路由核对、
//! 失败时按 1/2/4 分钟退避重启数据面单元、判定「今天该不该自更新」。
//! 状态落在 `/opt/bui-c/runtime.json`（0600），丢了能从零重建。

use crate::engine::Engine;
use crate::net::{Net, Via};
use crate::paths::{Paths, UNIT_MAIN};
use crate::profiles::{Mode, Profiles};
use crate::sys::{systemd, Sys};
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

/// 一次巡检的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    NoProfile,
    Ok,
    Restarted {
        failures: Vec<Failure>,
        next_backoff_min: u64,
    },
    Waiting {
        failures: Vec<Failure>,
        remaining_s: i64,
    },
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

/// 逐项探测，收集**全部**失败项（不短路：日志里要能一眼看出是单元没起还是路由没接管）。
pub fn probe<S: Sys, N: Net>(sys: &S, net: &N, paths: &Paths, prof: &Profiles) -> Vec<Failure> {
    let mut out = Vec::new();
    if !systemd::is_active(sys, UNIT_MAIN) {
        out.push(Failure::UnitDown);
    }
    let via = match prof.mode {
        // TUN 已接管全局路由，直连探测就是经隧道；socks 模式必须经本地 inbound 才验证得到隧道
        Mode::Tun => Via::Direct,
        Mode::Socks => Via::Socks5 {
            port: prof.socks_port,
        },
    };
    match net.status(PROBE_URL, via, PROBE_TIMEOUT) {
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

/// 一次巡检：探测 → 清零或退避重启。
pub fn run<S: Sys, N: Net>(sys: &S, net: &N, paths: &Paths) -> Result<Verdict> {
    let prof = Profiles::load(sys, paths)?;
    if prof.active_profile().is_none() {
        return Ok(Verdict::NoProfile);
    }
    let mut rt = Runtime::load(sys, paths);
    let now = sys.now().unix_timestamp();

    let failures = probe(sys, net, paths, &prof);
    if failures.is_empty() {
        if rt.fail_streak != 0 {
            rt.fail_streak = 0;
            rt.save(sys, paths)?;
        }
        return Ok(Verdict::Ok);
    }

    let wait_s = wait_seconds(rt.fail_streak);
    if let Some(last) = rt.last_restart_at {
        let elapsed = now - last;
        if elapsed < wait_s {
            return Ok(Verdict::Waiting {
                failures,
                remaining_s: wait_s - elapsed,
            });
        }
    }
    systemd::restart(sys, UNIT_MAIN)?;
    rt.fail_streak = rt.fail_streak.saturating_add(1);
    rt.last_restart_at = Some(now);
    rt.save(sys, paths)?;
    // 报的就是「这次重启之后要等几分钟」，与 wait_seconds 同一个数
    Ok(Verdict::Restarted {
        failures,
        next_backoff_min: (wait_seconds(rt.fail_streak) / 60) as u64,
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
