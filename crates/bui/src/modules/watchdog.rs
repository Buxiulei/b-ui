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
use crate::sys::{Host, Proto};
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use serde::{Deserialize, Serialize};
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

/// 两个 hysteria 实例与各自的配置文件名。端口跳跃的 nat 链按**实例**定位（base 端口 +
/// 跳跃区间都在这份配置的 `listen:` 行里），所以自愈时要把对应的那一份传给
/// [`crate::modules::portjump::cleanup`]。
pub const HY2_CONFIGS: [(&str, &str); 2] = [
    ("hysteria-server", "config.yaml"),
    ("hysteria-residential", "config-residential.yaml"),
];

/// 崩溃循环的判据：日志里的这句话（真机实录「ip6tables: Chain already exists」）。
pub const CHAIN_MARKER: &str = "Chain already exists";

/// 每轮看多少行日志。
pub const JOURNAL_LINES: &str = "50";

/// 自愈事件落 `runtime.extra` 的键：`{ "<单元>": ChainHeal }`。
/// 面板与 `bui status` 从 `runtime.json` 原样读得到。
pub const HEAL_KEY: &str = "hy2_chain_heal";

/// 同一个单元两次自愈之间的最短间隔：清完链还起不来说明另有原因，
/// 不能每 60 秒无脑 restart 一次（那就是自己制造崩溃循环）。
pub const HEAL_COOLDOWN_MINUTES: i64 = 10;

/// http 鉴权连不上的判据（spec §3.2）：`auth.type: http` 下内核每条
/// 连接都要打一次 `127.0.0.1:AUTH_HTTP_PORT`，守护进程没在听（或应答超时）就是全员登录失败。
/// **检测与告警在日志哨兵**（`modules::sentinel`，5 秒增量读 journald）；这里只留判据与门槛，
/// 哨兵的签名表引用它们。
/// 多少条鉴权连接失败（60 秒内）才算一次事件。
pub const AUTH_HTTP_FAIL_THRESHOLD: u32 = 3;
/// 「这一行说的是鉴权请求」的判据：内核把整个 URL 打进错误里，所以路径或端口任一命中即可。
pub const AUTH_HTTP_PATH_MARKER: &str = "/auth";
/// 「这一行说的是连不上 / 超时」的判据。
pub const AUTH_HTTP_FAIL_MARKERS: [&str; 4] = [
    "connection refused",
    "deadline exceeded",
    "timeout",
    "no route to host",
];

/// 一行 journal 是不是「鉴权请求连不上 / 超时」（纯函数）。日志哨兵（`modules::sentinel`）逐行用它。
///
/// 两类标记都要命中才算：只匹配 "connection refused" 会把住宅上游、relay 的连接错误
/// 一起数进来；只匹配 "/auth" 会把正常的鉴权日志数进来。
pub fn is_auth_http_failure(line: &str) -> bool {
    let port = format!(":{}", bui_schema::render::hysteria::AUTH_HTTP_PORT);
    let lower = line.to_ascii_lowercase();
    (lower.contains(AUTH_HTTP_PATH_MARKER) || lower.contains(&port))
        && AUTH_HTTP_FAIL_MARKERS.iter().any(|m| lower.contains(m))
}

/// 数一段 journal 里「鉴权请求连不上 / 超时」的行数（纯函数，便于单测）。
pub fn count_auth_http_failures(log: &str) -> u32 {
    log.lines().filter(|l| is_auth_http_failure(l)).count() as u32
}

/// 一个单元的孤儿链自愈记录（累计次数 + 最近一次的时刻与清掉的链）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChainHeal {
    pub at: String,
    pub count: u32,
    /// 最近一次清理做过的事（`modules::portjump::cleanup` 的返回值，每行一条）
    pub done: Vec<String>,
}

/// 冷却判据：`last` 是上次自愈时刻（`None` = 没治过）。
pub fn should_heal(last: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    match last {
        None => true,
        Some(t) => now - t >= time::Duration::minutes(HEAL_COOLDOWN_MINUTES),
    }
}

/// 「这个单元正卡在崩溃循环里」：`ActiveState` 是 `failed`（已撞上 systemd 的 start limit）
/// 或 `activating`（`auto-restart` 间隙）。
///
/// **不看 `inactive`**：运维 `systemctl stop` 停掉的单元就是 inactive，自愈不该把它拉起来。
pub fn is_crash_looping(state: Option<&str>) -> bool {
    matches!(state, Some("failed") | Some("activating"))
}

/// 一个 hysteria 单元的自愈：`ActiveState` 判崩溃循环 → 日志里找 [`CHAIN_MARKER`] →
/// 清本实例的孤儿链 → `reset-failed` + `restart`。返回 `Some(清理做过的事)` 表示治过一次。
///
/// `reset-failed` 是必须的：52 次崩溃早已撞上 `StartLimitBurst`，不清计数直接 `restart`
/// 会被 systemd 以 "start request repeated too quickly" 挡掉。
fn heal_chain_conflict(
    host: &dyn Host,
    paths: &Paths,
    unit: &str,
    conf: &str,
) -> Option<Vec<String>> {
    let active_state = host.unit_property(unit, "ActiveState").ok().flatten();
    if !is_crash_looping(active_state.as_deref()) {
        return None;
    }
    let log = host
        .run(
            "journalctl",
            &["-u", unit, "-n", JOURNAL_LINES, "--no-pager"],
        )
        .ok()?;
    if !log.ok() || !log.stdout.contains(CHAIN_MARKER) {
        return None;
    }
    tracing::warn!(
        unit = %unit,
        "日志含「{CHAIN_MARKER}」：清本实例的端口跳跃孤儿链后重启"
    );
    let done = crate::modules::portjump::cleanup(host, &paths.base_dir.join(conf));
    let _ = host.systemd("reset-failed", unit);
    let _ = host.systemd("restart", unit);
    Some(done)
}

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

/// 四类内核 + 每个住宅实例，各自的监听端口：relay 与 xray 是 TCP，hysteria 是 UDP。
/// 端口取自期望态（`state.node.ports` + 槽位表），relay 的入站端口是基准常量。
pub fn targets(state: &State) -> Vec<Target> {
    let mut v = vec![Target {
        unit: "hysteria-server".into(),
        proto: Proto::Udp,
        port: state.node.ports.hy2,
    }];
    for i in bui_schema::slots::indices(&state.residential) {
        v.push(Target {
            unit: crate::reconcile::resi_unit(i),
            proto: Proto::Udp,
            port: bui_schema::slots::resources_of(&state.node.ports, &state.residential, i)
                .hy2_port,
        });
    }
    v.push(Target {
        unit: "xray".into(),
        proto: Proto::Tcp,
        port: state.node.ports.reality_direct,
    });
    v.push(Target {
        unit: "b-ui-relay".into(),
        proto: Proto::Tcp,
        port: crate::modules::core_files::RELAY_LISTEN_PORT,
    });
    v
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

/// 跑一轮：先治 hysteria 的端口跳跃孤儿链崩溃循环（[`heal_chain_conflict`]），再读单元状态与
/// 监听端口（都经 `Host`，时钟也取 `host.now()`），按裁决重启，落 `runtime.json`。
pub async fn check_once(ctx: &DaemonCtx) -> anyhow::Result<Vec<(String, Decision)>> {
    let state = ctx.store.read().await;
    let targets = targets(&state);
    let rt = ctx.runtime.read().await;
    let mut heals: std::collections::BTreeMap<String, ChainHeal> = rt
        .extra
        .get(HEAL_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut records = rt.watchdog;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let (decisions, records, heals, now) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let now = host.now();
            // 孤儿链自愈排在监听探测之前：崩溃循环里的实例根本没进程，按端口判只会得出
            // `Healthy`（`!alive` 交给 systemd），而 systemd 的 `Restart=always` 在这个错误上
            // 永远治不好——链不清掉，下一次 `-N` 还是 "Chain already exists"。
            for (unit, conf) in HY2_CONFIGS {
                let last = heals.get(unit).and_then(|h| parse_rfc3339(&h.at));
                if !should_heal(last, now) {
                    continue;
                }
                if let Some(done) = heal_chain_conflict(&*host, &paths, unit, conf) {
                    let rec = heals.entry(unit.to_string()).or_default();
                    rec.at = fmt_rfc3339(now);
                    rec.count += 1;
                    rec.done = done;
                }
            }
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
            Ok((out, records, heals, now))
        })
        .await??;
    ctx.runtime
        .update(|r| {
            r.watchdog = records;
            r.extra.insert(RUN_KEY.into(), run_stamp(now));
            // 治过才写这个键：没治过的机器上 `runtime.json` 里连它都不该出现
            if !heals.is_empty() {
                if let Ok(v) = serde_json::to_value(&heals) {
                    r.extra.insert(HEAL_KEY.into(), v);
                }
            }
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

    /// 只看真正动了机器的操作：孤儿链自愈会先读一次崩溃单元的 journal，这些只读调用不属于任何一条动作断言
    fn acting_ops(host: &FakeHost) -> Vec<String> {
        host.ops()
            .into_iter()
            .filter(|o| !o.starts_with("run:journalctl"))
            .collect()
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
        assert_eq!(acting_ops(&host), vec!["systemd:restart:hysteria-server"]);
        let rt = c.runtime.read().await;
        assert_eq!(rt.watchdog["hysteria-server"].restarts, 1);
        assert_eq!(rt.watchdog["xray"].fails, 0);
    }

    #[test]
    fn only_a_failed_or_auto_restarting_unit_counts_as_a_crash_loop() {
        assert!(is_crash_looping(Some("failed")));
        assert!(is_crash_looping(Some("activating")));
        // 运维手动 stop 的单元不许被自愈拉起来
        assert!(!is_crash_looping(Some("inactive")));
        assert!(!is_crash_looping(Some("active")));
        assert!(!is_crash_looping(Some("deactivating")));
        assert!(!is_crash_looping(None));
    }

    #[test]
    fn healing_the_same_unit_again_waits_out_the_cooldown() {
        assert!(should_heal(None, t0()), "没治过就治");
        assert!(!should_heal(Some(t0()), t0() + time::Duration::minutes(9)));
        assert!(should_heal(
            Some(t0()),
            t0() + time::Duration::minutes(HEAL_COOLDOWN_MINUTES)
        ));
    }

    /// 播种真机事故现场（bwg-rick 2026-09-12 20:33 UTC）：`hysteria-residential` 在崩溃循环，
    /// journal 里是「ip6tables: Chain already exists」，`ip6tables` 的 nat 表里只剩 OUTPUT
    /// 一条跳转（链本身是空的）。
    fn crash_looping_host() -> Arc<FakeHost> {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("xray.service".into());
            i.units_active.insert("b-ui-relay.service".into());
            i.unit_props.insert(
                ("hysteria-server.service".into(), "ActiveState".into()),
                "active".into(),
            );
            i.unit_props.insert(
                ("hysteria-residential.service".into(), "ActiveState".into()),
                "failed".into(),
            );
            i.scripted.push((
                "journalctl -u hysteria-residential".into(),
                crate::sys::CmdOut::success(
                    "hysteria[1234]: invalid config: listen: ip6tables [-w -t nat -N \
                     HYSTERIA-PR-c66a02d9]: exit status 1: ip6tables: Chain already exists\n",
                ),
            ));
            i.which.insert("ip6tables".into());
            i.scripted.push((
                "ip6tables -t nat -S".into(),
                crate::sys::CmdOut::success(
                    "-N HYSTERIA-PR-c66a02d9\n-A OUTPUT -p udp -m udp --dport 41000:50000 \
                     -j HYSTERIA-PR-c66a02d9\n",
                ),
            ));
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (b"listen: :40000,41000-50000\n".to_vec(), 0o600),
            );
            i.listening
                .insert(Proto::Udp, [10000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        host
    }

    /// 事故回归：崩溃循环 + 日志含「Chain already exists」→ 先清本实例的孤儿链，
    /// 再 `reset-failed`（52 次重启早撞上 start limit，不清计数 restart 会被挡）+ `restart`，
    /// 并把事件记进 `runtime.json`。直连实例（active）一个命令都不许收到。
    #[tokio::test]
    async fn a_chain_conflict_crash_loop_is_healed_before_the_port_probe() {
        let host = crash_looping_host();
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        let ops = acting_ops(&host);
        let i = |needle: &str| {
            ops.iter()
                .position(|o| o.contains(needle))
                .unwrap_or_else(|| panic!("没有 {needle}：{ops:?}"))
        };
        assert!(i("-D OUTPUT") < i("-X HYSTERIA-PR-c66a02d9"), "{ops:?}");
        assert!(
            i("-X HYSTERIA-PR-c66a02d9") < i("systemd:reset-failed:hysteria-residential"),
            "清链必须在重启之前，否则起来照样撞同名链：{ops:?}"
        );
        assert!(
            i("systemd:reset-failed:hysteria-residential")
                < i("systemd:restart:hysteria-residential"),
            "{ops:?}"
        );
        assert!(
            !ops.iter().any(|o| o.contains("hysteria-server")),
            "直连实例是 active，不该被碰：{ops:?}"
        );
        // 事件落盘
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        let rec = &heals["hysteria-residential"];
        assert_eq!(rec.count, 1);
        assert_eq!(rec.at, "2026-09-11T00:00:00Z");
        assert_eq!(
            rec.done,
            vec!["已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）"]
        );
    }

    /// 自愈分支与 `bui hy2-prestart` 是同一个 [`crate::modules::portjump::cleanup`]：
    /// nft 后端的机器（tizi）上，崩溃循环的自愈同样要把本实例的 `hysteria_*` 表删掉，
    /// 而不是只清 iptables 链。
    #[tokio::test]
    async fn the_heal_branch_drops_this_instances_nft_table_too() {
        let host = crash_looping_host();
        host.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                crate::sys::CmdOut::success(
                    "table ip6 hysteria_390d4d8b\ntable ip hysteria_7c1e0f2a\n",
                ),
            ));
            i.scripted.push((
                "nft list table ip6 hysteria_390d4d8b".into(),
                crate::sys::CmdOut::success(
                    "table ip6 hysteria_390d4d8b {\n\tchain output {\n\t\tudp dport 41000-50000 \
                     redirect to :40000\n\t}\n}\n",
                ),
            ));
            i.scripted.push((
                "nft list table ip hysteria_7c1e0f2a".into(),
                crate::sys::CmdOut::success(
                    "table ip hysteria_7c1e0f2a {\n\tchain output {\n\t\tudp dport 45500-50000 \
                     redirect to :40001\n\t}\n}\n",
                ),
            ));
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        let ops = acting_ops(&host);
        assert!(
            ops.iter()
                .any(|o| o == "run:nft delete table ip6 hysteria_390d4d8b"),
            "{ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|o| o.contains("delete table ip hysteria_7c1e0f2a")),
            "别的槽（base 40001）的表不许碰：{ops:?}"
        );
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        assert_eq!(
            heals["hysteria-residential"].done,
            vec![
                "已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）",
                "已删除 nft 表 ip6 hysteria_390d4d8b（本实例端口跳跃孤儿）",
            ]
        );
    }

    /// 冷却窗口内不再治第二次（清完链还起不来说明另有原因，每 60 秒 restart 一次就是
    /// 自己造崩溃循环）；过了窗口再治，计数累加。
    #[tokio::test]
    async fn the_second_round_waits_for_the_cooldown_then_heals_again() {
        let host = crash_looping_host();
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        host.clear_ops();

        host.advance(60);
        check_once(&c).await.unwrap();
        assert!(
            acting_ops(&host)
                .iter()
                .all(|o| !o.contains("tables") && !o.contains("reset-failed")),
            "冷却期内不该再清链、也不该再走自愈的重启：{:?}",
            host.ops()
        );

        host.advance(HEAL_COOLDOWN_MINUTES * 60);
        check_once(&c).await.unwrap();
        assert!(host
            .ops()
            .iter()
            .any(|o| o == "systemd:restart:hysteria-residential"));
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        assert_eq!(heals["hysteria-residential"].count, 2);
    }

    /// 崩溃循环但日志里**不是**这个错误（证书过期、端口被占…）→ 不清链、不重启：
    /// 那些错误清 nat 链治不好，`Restart=always` 与体检各归各管。
    #[tokio::test]
    async fn a_crash_loop_with_another_error_is_left_alone() {
        let host = crash_looping_host();
        host.with(|i| {
            i.scripted.clear();
            i.scripted.push((
                "journalctl -u hysteria-residential".into(),
                crate::sys::CmdOut::success("hysteria: failed to load cert: no such file\n"),
            ));
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            host.ops()
                .iter()
                .all(|o| !o.starts_with("systemd:restart") && !o.contains("ip6tables")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key(HEAL_KEY));
    }

    /// 运维 `systemctl stop` 停掉的实例（`ActiveState=inactive`）即使日志里还留着那句错误，
    /// 也不许被自愈拉起来。
    #[tokio::test]
    async fn a_deliberately_stopped_unit_is_not_restarted() {
        let host = crash_looping_host();
        host.with(|i| {
            i.unit_props.insert(
                ("hysteria-residential.service".into(), "ActiveState".into()),
                "inactive".into(),
            );
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            acting_ops(&host)
                .iter()
                .all(|o| !o.contains("hysteria-residential")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key(HEAL_KEY));
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

    #[test]
    fn targets_cover_every_residential_slot_instance() {
        let mut s = crate::testutil::sample_state();
        assert_eq!(
            targets(&s)
                .iter()
                .map(|t| (t.unit.clone(), t.port))
                .collect::<Vec<_>>(),
            vec![
                ("hysteria-server".to_string(), 10000),
                ("hysteria-residential".to_string(), 40000),
                ("xray".to_string(), 10001),
                ("b-ui-relay".to_string(), 2080),
            ]
        );
        s.residential.slots = (0..3)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        let t = targets(&s);
        assert_eq!(t.len(), 6);
        assert!(t
            .iter()
            .any(|x| x.unit == "hysteria-residential-1" && x.port == 40001));
        assert!(t
            .iter()
            .any(|x| x.unit == "hysteria-residential-2" && x.port == 40002));
        assert!(t.iter().all(|x| match x.unit.as_str() {
            "xray" | "b-ui-relay" => x.proto == Proto::Tcp,
            _ => x.proto == Proto::Udp,
        }));
    }

    /// 只数「鉴权请求 + 连不上/超时」两类标记同时命中的行。住宅上游、relay 的连接错误
    /// 与正常的鉴权日志都不能被数进来，否则哨兵天天误报。
    #[test]
    fn only_lines_about_the_auth_endpoint_failing_are_counted() {
        let log = "\
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: connect: connection refused\"}
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": context deadline exceeded\"}
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: i/o timeout\"}
hysteria[1]: client connected {\"addr\": \"203.0.113.9:1\", \"id\": \"u-1\"}
hysteria[1]: outbound error {\"error\": \"dial tcp 198.51.100.7:10007: connect: connection refused\"}
";
        assert_eq!(count_auth_http_failures(log), 3);
        assert_eq!(count_auth_http_failures(""), 0);
        assert_eq!(
            count_auth_http_failures("hysteria[1]: client connected /auth ok\n"),
            0,
            "只有 /auth 没有失败标记 ⇒ 不算"
        );
    }

    /// 鉴权日志由哨兵负责（5 秒增量读，设计裁决 D9）：watchdog 不再每 60 秒自己翻 hysteria 的日志，
    /// 也不再写 `hy2_auth_http` 键——同一故障不许两处各报一次。
    #[tokio::test]
    async fn the_watchdog_leaves_the_auth_log_to_the_sentinel() {
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
                .insert(Proto::Udp, [10000, 40000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            !host.ops().iter().any(|o| o.contains("journalctl")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key("hy2_auth_http"));
    }
}
