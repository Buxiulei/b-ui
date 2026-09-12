//! 进程内 watchdog：60 秒一轮，探四个内核的「单元存活 + 监听端口」，只治「进程还在、
//! 端口却不 listen」这一类僵死，重启带 1/2/4 分钟退避（spec §3.4）。
//!
//! 移植参照 `server/core.sh:1050-1130`（`setup_hy2_watchdog`）。v4 的变化（审计 §3.1/§3.2
//! 与 web-C12）：不再生成脚本、不再建 timer、不再用 `/tmp/hy2-watchdog-*` 计数文件（面板还
//! 在读那个文件），改成守护进程里的内存状态机 + `runtime.json` 持久化；覆盖面从两个
//! hysteria 扩到四个内核；阈值 2 次（60s×2）+ 退避 1/2/4 分钟。
//!
//! 本模块不产出任何 `Artifact`（`render` 返回空 `Vec`），只有后台任务。

use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use crate::state::runtime::WatchdogRecord;
use crate::sys::Proto;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::State;
use time::OffsetDateTime;

/// 一轮检查的间隔。
pub const INTERVAL_SECS: u64 = 60;
/// 连续多少轮「进程在、端口不 listen」才重启。
pub const FAIL_THRESHOLD: u32 = 2;
/// 退避 1/2/4 分钟（spec §3.4），第四次及以后停在 4 分钟。
pub const BACKOFF_MINUTES: [i64; 3] = [1, 2, 4];
/// 每轮跑完把「这一轮的时刻 / 下一轮的时刻」记到 `runtime.extra` 的这个键下
/// （`RuntimeData::extra` 是 flatten 的扩展位，不必为两个字段改 `runtime.rs`）。
/// 面板 `GET /api/hy2/watchdog/status` 原样透出这两个字段。
pub const RUN_KEY: &str = "watchdog_run";

/// 一个探测目标：单元名 + 协议 + 监听端口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub unit: String,
    pub proto: Proto,
    pub port: u16,
}

/// 状态机对一轮探测的裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Healthy,
    Failing { fails: u32 },
    Restart,
    Backoff,
}

pub struct WatchdogModule;

impl Module for WatchdogModule {
    fn name(&self) -> &'static str {
        "watchdog"
    }

    /// watchdog 没有期望项，只有后台任务。
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(watch_loop(ctx))]
    }
}

/// 四个内核 + 各自的监听端口：relay 与 xray 是 TCP，两个 hysteria 是 UDP。
/// 端口取自期望态（`state.node.ports`），relay 的入站端口是常量。
pub fn targets(state: &State) -> Vec<Target> {
    vec![
        Target {
            unit: "hysteria-server".into(),
            proto: Proto::Udp,
            port: state.node.ports.hy2,
        },
        Target {
            unit: "hysteria-residential".into(),
            proto: Proto::Udp,
            port: state.node.ports.hy2_resi,
        },
        Target {
            unit: "xray".into(),
            proto: Proto::Tcp,
            port: state.node.ports.reality_direct,
        },
        Target {
            unit: "b-ui-relay".into(),
            proto: Proto::Tcp,
            port: crate::modules::core_files::RELAY_LISTEN_PORT,
        },
    ]
}

/// 纯状态机：返回裁决并就地更新记录（不碰机器、不落盘，便于单测）。
pub fn decide(
    rec: &mut WatchdogRecord,
    alive: bool,
    listening: bool,
    now: OffsetDateTime,
) -> Decision {
    // 进程不在 → systemd 的 Restart=always 负责，watchdog 不插手（core.sh:1065-1068）。
    // **这与 spec §3.4「检查四个内核进程存活 + 监听端口」的字面读法不同，是有意为之**（v3 同语义）：
    // watchdog 只解决「进程还在、端口却不 listen」这一类僵死；`!alive` 交给 systemd，重复插手会和
    // `Restart=always` 抢着重启。单元不 active 这件事本身由 `/api/health` 的 services 报 degraded，
    // 不会被吞掉。M5 验收时按这一段口径核对，不要按 spec 字面要求 watchdog 去 start 单元。
    if !alive || listening {
        rec.fails = 0;
        return Decision::Healthy;
    }
    rec.fails += 1;
    if rec.fails < FAIL_THRESHOLD {
        return Decision::Failing { fails: rec.fails };
    }
    if let Some(until) = rec.backoff_until.as_deref().and_then(parse_rfc3339) {
        if now < until {
            return Decision::Backoff;
        }
    }
    rec.fails = 0;
    rec.restarts += 1;
    // restarts 是累计值（不清零），所以退避取 1/2/4 后停在 4。
    let minutes = BACKOFF_MINUTES[(rec.restarts as usize - 1).min(BACKOFF_MINUTES.len() - 1)];
    rec.last_restart_at = Some(fmt_rfc3339(now));
    rec.backoff_until = Some(fmt_rfc3339(now + time::Duration::minutes(minutes)));
    Decision::Restart
}

/// 一轮跑完的时间戳：`last_run_at` = 这一轮的时刻，`next_run_at` = 再加一个间隔。
pub fn run_stamp(now: OffsetDateTime) -> serde_json::Value {
    serde_json::json!({
        "last_run_at": fmt_rfc3339(now),
        "next_run_at": fmt_rfc3339(now + time::Duration::seconds(INTERVAL_SECS as i64)),
    })
}

/// 跑一轮：读单元状态与监听端口（都经 `Host`，时钟也取 `host.now()`），按裁决重启，落 `runtime.json`。
pub async fn check_once(ctx: &DaemonCtx) -> anyhow::Result<Vec<(String, Decision)>> {
    let state = ctx.store.read().await;
    let targets = targets(&state);
    let mut records = ctx.runtime.read().await.watchdog;
    let host = ctx.host.clone();
    let (decisions, records, now) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let now = host.now();
        let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
        let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
        let mut out = Vec::with_capacity(targets.len());
        for t in &targets {
            let rec = records.entry(t.unit.clone()).or_default();
            let alive = host.unit_is_active(&t.unit).unwrap_or(false);
            let listening = match t.proto {
                Proto::Udp => udp.contains(&t.port),
                Proto::Tcp => tcp.contains(&t.port),
            };
            let d = decide(rec, alive, listening, now);
            if d == Decision::Restart {
                tracing::warn!(unit = %t.unit, port = t.port, "监听失活连续 2 轮，重启");
                let _ = host.systemd("restart", &t.unit);
            }
            out.push((t.unit.clone(), d));
        }
        Ok((out, records, now))
    })
    .await??;
    ctx.runtime
        .update(|r| {
            r.watchdog = records;
            r.extra.insert(RUN_KEY.into(), run_stamp(now));
        })
        .await;
    Ok(decisions)
}

pub async fn watch_loop(ctx: DaemonCtx) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = check_once(&ctx).await {
            tracing::warn!(error = %e, "watchdog 一轮检查失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::state::runtime::{Runtime, WatchdogRecord};
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    #[test]
    fn targets_cover_four_kernels_with_the_right_protocol() {
        assert_eq!(
            targets(&sample_state()),
            vec![
                Target {
                    unit: "hysteria-server".into(),
                    proto: Proto::Udp,
                    port: 10000
                },
                Target {
                    unit: "hysteria-residential".into(),
                    proto: Proto::Udp,
                    port: 40000
                },
                Target {
                    unit: "xray".into(),
                    proto: Proto::Tcp,
                    port: 10001
                },
                Target {
                    unit: "b-ui-relay".into(),
                    proto: Proto::Tcp,
                    port: 2080
                },
            ]
        );
    }

    #[test]
    fn healthy_resets_the_counter() {
        let mut rec = WatchdogRecord {
            fails: 1,
            restarts: 3,
            last_restart_at: None,
            backoff_until: None,
        };
        assert_eq!(decide(&mut rec, true, true, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.restarts, 3, "重启次数是累计值，不清零");
    }

    #[test]
    fn dead_process_is_left_to_systemd() {
        let mut rec = WatchdogRecord::default();
        // 进程不在 → systemd Restart=always 会管，watchdog 只清计数（移植 core.sh:1065-1068）
        assert_eq!(decide(&mut rec, false, false, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
    }

    #[test]
    fn two_consecutive_half_dead_rounds_trigger_a_restart_then_backoff() {
        let mut rec = WatchdogRecord::default();
        assert_eq!(
            decide(&mut rec, true, false, t0()),
            Decision::Failing { fails: 1 }
        );
        let r = decide(&mut rec, true, false, t0() + time::Duration::seconds(60));
        assert_eq!(r, Decision::Restart);
        assert_eq!(rec.restarts, 1);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.last_restart_at.as_deref(), Some("2026-09-11T00:01:00Z"));
        assert_eq!(
            rec.backoff_until.as_deref(),
            Some("2026-09-11T00:02:00Z"),
            "第一次退避 1 分钟"
        );
        // 退避窗口内即使再连续两轮失败也不重启
        assert_eq!(
            decide(&mut rec, true, false, t0() + time::Duration::seconds(70)),
            Decision::Failing { fails: 1 }
        );
        assert_eq!(
            decide(&mut rec, true, false, t0() + time::Duration::seconds(80)),
            Decision::Backoff
        );
        assert_eq!(rec.restarts, 1);
    }

    #[test]
    fn backoff_grows_one_two_four_then_stays_at_four() {
        let mut rec = WatchdogRecord::default();
        let mut now = t0();
        let mut seen = Vec::new();
        for _ in 0..4 {
            // 每轮：两次失败触发一次重启，然后跳过退避窗口
            decide(&mut rec, true, false, now);
            now += time::Duration::seconds(60);
            assert_eq!(decide(&mut rec, true, false, now), Decision::Restart);
            let until = time::OffsetDateTime::parse(
                rec.backoff_until.as_deref().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            seen.push((until - now).whole_minutes());
            now = until + time::Duration::seconds(1);
        }
        assert_eq!(seen, vec![1, 2, 4, 4]);
        assert_eq!(rec.restarts, 4);
    }

    async fn ctx(host: Arc<FakeHost>) -> (crate::reconcile::DaemonCtx, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(d.path().join("state.json"), sample_state())
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        (
            crate::reconcile::DaemonCtx {
                store,
                runtime,
                bus: EventBus::new(),
                host,
                paths: Paths::default_server(),
            },
            d,
        )
    }

    #[tokio::test]
    async fn check_once_restarts_only_the_half_dead_unit() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening
                .insert(Proto::Udp, [40000].into_iter().collect()); // 10000 失活
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        let first = check_once(&c).await.unwrap();
        assert_eq!(
            first[0],
            (
                "hysteria-server".to_string(),
                Decision::Failing { fails: 1 }
            )
        );
        assert_eq!(
            first[1],
            ("hysteria-residential".to_string(), Decision::Healthy)
        );
        assert!(!host.ops().iter().any(|o| o.starts_with("systemd:restart")));
        host.advance(60);
        let second = check_once(&c).await.unwrap();
        assert_eq!(
            second[0],
            ("hysteria-server".to_string(), Decision::Restart)
        );
        assert_eq!(host.ops(), vec!["systemd:restart:hysteria-server"]);
        let rt = c.runtime.read().await;
        assert_eq!(rt.watchdog["hysteria-server"].restarts, 1);
        assert_eq!(rt.watchdog["xray"].fails, 0);
    }

    #[tokio::test]
    async fn each_round_stamps_last_run_at_and_next_run_at() {
        let host = Arc::new(FakeHost::new());
        let (c, _d) = ctx(host.clone()).await;
        assert!(
            !c.runtime.read().await.extra.contains_key(RUN_KEY),
            "没跑过一轮时没有时间戳（接口那头就是 null）"
        );

        check_once(&c).await.unwrap();
        let stamp = c.runtime.read().await.extra[RUN_KEY].clone();
        assert_eq!(stamp["last_run_at"], "2026-09-11T00:00:00Z");
        assert_eq!(
            stamp["next_run_at"], "2026-09-11T00:01:00Z",
            "next = last + INTERVAL_SECS"
        );

        // 时钟推进一轮：两个字段跟着走
        host.advance(INTERVAL_SECS as i64);
        check_once(&c).await.unwrap();
        let stamp = c.runtime.read().await.extra[RUN_KEY].clone();
        assert_eq!(stamp["last_run_at"], "2026-09-11T00:01:00Z");
        assert_eq!(stamp["next_run_at"], "2026-09-11T00:02:00Z");
    }
}
