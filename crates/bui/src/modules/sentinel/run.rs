//! 哨兵主循环（spec §5.7）：每 [`POLL_SECS`] 秒增量读一次 journald → 签名匹配 → 去抖 → 冷却 →
//! 预案 → 事件落盘 + 通知口。游标以内存为准，落盘有节流（设计裁决 D1）。

use super::engine::{self, Engine, SentinelRuntime};
use super::incidents::{self, Incident, Level, Outcome};
use super::signature::{self, Sig};
use super::{resi, system, Deps, CURSOR_PERSIST_SECS, POLL_SECS};
use crate::modules::residential::{clash, state as rstate};
use crate::reconcile::DaemonCtx;
use crate::sys::{JournalFrom, JournalRecord};
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{Hy2Auth, State};
use time::OffsetDateTime;
use uuid::Uuid;

/// 30 分钟建议替换不是日志签名，是巡查产出（[`resi::sweep_long_unreachable`]）；事件里用这两个串
const LONG_UNREACHABLE_SIG: &str = "upstream_long_unreachable";
const SUGGEST_REPLACE_ACTION: &str = "suggest_replace";

/// 哨兵要跟的单元 = 受管单元全集：relay / hysteria-server / 每个住宅槽的 hysteria / xray / caddy，
/// 外加 b-ui 自己——xray gRPC 的失败只出现在守护进程自己的日志里（设计裁决 D2 / D10）。
pub fn units(state: &State) -> Vec<String> {
    crate::reconcile::managed_units(state)
}

/// 最长的签名窗口（今天是 `XrayGrpcUnavailable` 的 150 秒）
fn longest_window_secs() -> i64 {
    [
        Sig::RelayUpstreamError,
        Sig::RelayUpstreamAuthFailed,
        Sig::RelayGoogleBlocked,
        Sig::Hy2AuthHttpFailed,
        Sig::KernelBindInUse,
        Sig::KernelCrashLoop,
        Sig::XrayGrpcUnavailable,
        Sig::CaddyCertFailed,
    ]
    .into_iter()
    .map(|s| s.rule().window_secs)
    .max()
    .unwrap_or_default()
}

/// 重启后第一轮：持久化的读起点（有游标看 `cursor_at`，否则看 `since`）早于最长签名窗口、或年龄不明
/// ⇒ 丢掉游标、从现在读起。那段积压里的每一条都会被去抖器当陈旧日志丢掉（D12），读它只是把
/// 停机期间的大量日志一次读进内存。
fn drop_stale_start(sr: &mut SentinelRuntime, now: OffsetDateTime) {
    if sr.cursor.is_none() && sr.since.is_none() {
        return; // 首次启动：下面 ① 自会从现在读起
    }
    let at = if sr.cursor.is_some() {
        sr.cursor_at.as_deref()
    } else {
        sr.since.as_deref()
    };
    let window = time::Duration::seconds(longest_window_secs());
    if at
        .and_then(parse_rfc3339)
        .is_some_and(|t| now - t <= window)
    {
        return;
    }
    tracing::info!(from = ?at, "哨兵持久化的读起点早于最长签名窗口：丢掉游标，从现在读起（积压都是陈旧日志）");
    sr.cursor = None;
    sr.cursor_at = None;
    sr.since = Some(fmt_rfc3339(now));
}

/// 执行了动作（借用 / 告警 / 重试 / 交看门狗）才盖 [`super::ACTION_COOLDOWN_SECS`] 冷却。住宅两类预案
/// 的 Info 是「带外探测 / 复核通过，或上游刚被删」——什么都没做，只受 60 秒去抖约束：否则一次误报
/// （HTTP 上游对慢目标报 deadline exceeded）会让哨兵对该上游失明 10 分钟，真故障只能等巡检。
fn takes_cooldown(sig: Sig, level: Level) -> bool {
    !(sig.on_upstream() && level == Level::Info)
}

/// 常驻在循环里的哨兵状态
#[derive(Default)]
pub struct Sentinel {
    engine: Engine,
    /// 首轮从 runtime 读出，此后**以内存为准**（落盘有节流；每轮都从 runtime 读会重复读同一段日志）
    sr: Option<SentinelRuntime>,
    last_persist: Option<OffsetDateTime>,
    /// 上一次读 journald 的错误文案：同样的错误只打一次日志（没装 journalctl 时每轮一条是刷屏）
    last_error: Option<String>,
}

/// 一轮的报告（测试断言用）
#[derive(Debug, Default)]
pub struct TickReport {
    pub read: usize,
    pub incidents: Vec<Incident>,
}

/// 一次触发：签名、计数键（relay 是上游 uuid 串）、上游 uuid（仅 relay 签名）、那条日志、匹配结果
type Fired = (Sig, String, Option<Uuid>, JournalRecord, signature::Match);

pub async fn tick(ctx: &DaemonCtx, deps: &Deps, s: &mut Sentinel) -> TickReport {
    let now = ctx.host.now();
    let state = ctx.store.read().await;
    let units = units(&state);
    let hy2_http = state.system.hy2_auth == Hy2Auth::Http;
    let g = rstate::group_of(&state);
    drop(state);
    let first = s.sr.is_none();
    if first {
        s.sr = Some(engine::sentinel_of(&ctx.runtime.read().await));
    }
    let mut sr = s.sr.clone().unwrap_or_default();
    let before = sr.clone();
    if first {
        drop_stale_start(&mut sr, now);
    }

    // ① 读：有游标续读；没有就从 `since`（首次 = 现在）读起，不回放历史
    let from = match (&sr.cursor, sr.since.as_deref().and_then(parse_rfc3339)) {
        (Some(c), _) => JournalFrom::Cursor(c.clone()),
        (None, Some(t)) => JournalFrom::Since(t),
        (None, None) => {
            sr.since = Some(fmt_rfc3339(now));
            JournalFrom::Since(now)
        }
    };
    let host = ctx.host.clone();
    let (u2, f2) = (units.clone(), from.clone());
    let records = match tokio::task::spawn_blocking(move || host.journal_read(&u2, &f2)).await {
        Ok(Ok(v)) => {
            s.last_error = None;
            if let Some(last) = v.last() {
                sr.cursor = Some(last.cursor.clone());
            }
            if sr.cursor.is_some() {
                sr.cursor_at = Some(fmt_rfc3339(now));
            }
            v
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            if s.last_error.as_deref() != Some(msg.as_str()) {
                tracing::warn!(error = %msg, "哨兵读 journald 失败：丢掉游标，从现在读起（不回放）");
            }
            s.last_error = Some(msg);
            sr.cursor = None;
            sr.cursor_at = None;
            sr.since = Some(fmt_rfc3339(now));
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "哨兵读 journald 的任务异常");
            Vec::new()
        }
    };

    // ② 匹配 + 去抖。relay 的对象当场从成员 tag（位置键）换成 uuid，换不出来的丢弃
    let mut fired: Vec<Fired> = Vec::new();
    for r in &records {
        let Some(m) = signature::classify(&r.unit, &r.message) else {
            continue;
        };
        if m.sig == Sig::Hy2AuthHttpFailed && !hy2_http {
            continue;
        }
        let upstream = if m.sig.on_upstream() {
            match clash::id_of_tag(&g, &m.subject) {
                Some(id) => Some(id),
                None => continue,
            }
        } else {
            None
        };
        let key = upstream.map_or_else(|| m.subject.clone(), |id| id.to_string());
        if s.engine.observe(m.sig, &key, r.ts, now) {
            fired.push((m.sig, key, upstream, r.clone(), m));
        }
    }
    s.engine.prune(now);
    engine::prune_acted(&mut sr.acted, now);

    // ③ 冷却 + 预案
    let mut out: Vec<Incident> = Vec::new();
    for (sig, key, upstream, r, m) in fired {
        let akey = engine::action_key(sig, &key);
        if engine::in_cooldown(&sr.acted, &akey, now) {
            tracing::debug!(signature = sig.id(), subject = %key, "同动作冷却中，跳过");
            continue;
        }
        let o = dispatch(ctx, deps, sig, &key, upstream, &r, now).await;
        if takes_cooldown(sig, o.level) {
            sr.acted.insert(akey, fmt_rfc3339(now));
        }
        out.push(Incident {
            // 预案做完（网关不通时快探最长 PROBE_TCP_TIMEOUT_SECS + 借用的 PUT）之后才盖时间戳：
            // 事件时刻 = 该槽已借用的时刻。演练判据①「首条错误 → 记事件并借用 ≤15 秒」量的是它
            at: fmt_rfc3339(ctx.host.now()),
            unit: r.unit.clone(),
            signature: sig.id().to_string(),
            subject: o.subject,
            action: sig.action().id().to_string(),
            result: crate::redact::line(&o.result),
            level: o.level,
            sample: Some(m.detail),
        });
    }
    // ④ 巡查：长时间不可达建议替换
    for o in resi::sweep_long_unreachable(ctx, &mut sr, now).await {
        out.push(Incident {
            at: fmt_rfc3339(now),
            unit: "b-ui-relay".into(),
            signature: LONG_UNREACHABLE_SIG.into(),
            subject: o.subject,
            action: SUGGEST_REPLACE_ACTION.into(),
            result: o.result,
            level: o.level,
            sample: None,
        });
    }

    // ⑤ 落盘：有事件 / 冷却表或起点变了 ⇒ 立即；只有游标或它的读取时刻前进 ⇒ 至少隔
    // CURSOR_PERSIST_SECS。日志安静时游标不动，读取时刻也照样按节流落盘：重启时靠它判断积压是否过旧
    let others_changed = {
        let (mut a, mut b) = (sr.clone(), before.clone());
        (a.cursor, a.cursor_at) = (None, None);
        (b.cursor, b.cursor_at) = (None, None);
        a != b
    };
    let cursor_due = (sr.cursor != before.cursor || sr.cursor_at != before.cursor_at)
        && s.last_persist
            .is_none_or(|t| now - t >= time::Duration::seconds(CURSOR_PERSIST_SECS));
    if !out.is_empty() || others_changed || cursor_due {
        let (incs, snap) = (out.clone(), sr.clone());
        ctx.runtime
            .update(move |rt| {
                for i in incs {
                    incidents::push(rt, i);
                }
                engine::put_sentinel(rt, &snap);
            })
            .await;
        s.last_persist = Some(now);
    }
    for i in &out {
        match i.level {
            Level::Info => {
                tracing::info!(signature = %i.signature, subject = %i.subject, "哨兵事件：{}", i.result)
            }
            _ => {
                tracing::warn!(signature = %i.signature, subject = %i.subject, action = %i.action, "哨兵事件：{}", i.result)
            }
        }
        deps.notifier.notify(i);
    }
    s.sr = Some(sr);
    TickReport {
        read: records.len(),
        incidents: out,
    }
}

async fn dispatch(
    ctx: &DaemonCtx,
    deps: &Deps,
    sig: Sig,
    key: &str,
    upstream: Option<Uuid>,
    r: &JournalRecord,
    now: OffsetDateTime,
) -> Outcome {
    match (sig, upstream) {
        (Sig::RelayUpstreamError | Sig::RelayUpstreamAuthFailed, Some(id)) => {
            resi::on_upstream_error(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        (Sig::RelayGoogleBlocked, Some(id)) => {
            resi::on_google_blocked(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        // relay 签名的对象在匹配那一步就换成了 uuid（换不出来的已丢弃）
        (
            Sig::RelayUpstreamError | Sig::RelayUpstreamAuthFailed | Sig::RelayGoogleBlocked,
            None,
        ) => {
            unreachable!("relay 签名必带上游 uuid")
        }
        (Sig::Hy2AuthHttpFailed, _) => system::on_hy2_auth(ctx, &r.unit).await,
        (Sig::KernelBindInUse | Sig::KernelCrashLoop, _) => system::on_kernel(&r.unit, sig),
        (Sig::XrayGrpcUnavailable, _) => system::on_xray_grpc(ctx, &deps.panel).await,
        (Sig::CaddyCertFailed, _) => system::on_caddy_cert(key),
    }
}

/// 每 [`POLL_SECS`] 秒一轮；一轮里的预案（网关不通时带外探测 ≤ [`super::PROBE_TCP_TIMEOUT_SECS`]
/// 秒，网关通时完整探测更久）拖长了就顺延，不补跑
pub async fn sentinel_loop(ctx: DaemonCtx, deps: Deps) {
    let mut s = Sentinel::default();
    let mut every = tokio::time::interval(std::time::Duration::from_secs(POLL_SECS));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        tick(&ctx, &deps, &mut s).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::clash::{Clash, FakeClash};
    use crate::modules::residential::proxy::FakeProber;
    use crate::modules::sentinel::incidents::Level;
    use crate::modules::sentinel::testkit::{panel_shared, pool_ctx, rec};
    use crate::modules::sentinel::{Notifier, SentinelModule};
    use crate::reconcile::Module;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<Incident>>);

    impl Notifier for Recorder {
        fn notify(&self, i: &Incident) {
            self.0.lock().unwrap().push(i.clone());
        }
    }

    struct Kit {
        ctx: DaemonCtx,
        host: Arc<FakeHost>,
        deps: Deps,
        prober: Arc<FakeProber>,
        clash: Arc<FakeClash>,
        notes: Arc<Recorder>,
        _dir: tempfile::TempDir,
    }

    async fn kit() -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(dir.path()).await;
        let (panel, _xray) = panel_shared(&ctx);
        let prober = Arc::new(FakeProber::new()); // 缺省：网关连不上
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let notes = Arc::new(Recorder::default());
        let deps = Deps {
            prober: prober.clone(),
            clash: clash.clone(),
            panel,
            notifier: notes.clone(),
        };
        Kit {
            ctx,
            host,
            deps,
            prober,
            clash,
            notes,
            _dir: dir,
        }
    }

    fn feed(k: &Kit, recs: Vec<crate::sys::JournalRecord>) {
        k.host.with(|i| i.journal.push_back(Ok(recs)));
    }

    fn timeout(tag: &str) -> String {
        format!(
            "ERROR[4006] [2302991392 6.42s] connection: open connection to www.gstatic.com:443 \
             using outbound/socks[{tag}]: dial tcp 198.51.100.8:10007: i/o timeout"
        )
    }

    fn journal_op(host: &FakeHost) -> String {
        host.ops()
            .into_iter()
            .rfind(|o| o.starts_with("journal:"))
            .expect("本轮读过 journald")
    }

    #[tokio::test]
    async fn the_first_round_reads_from_now_over_every_managed_unit() {
        let k = kit().await;
        let mut s = Sentinel::default();
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.read, 0);
        let op = journal_op(&k.host);
        assert!(
            op.ends_with(":since=2026-09-11T00:00:00Z"),
            "首次从现在读起、不回放：{op}"
        );
        let units: Vec<&str> = op.split(':').nth(1).unwrap().split(',').collect();
        for u in [
            "b-ui",
            "b-ui-relay",
            "xray",
            "caddy",
            "hysteria-server",
            "hysteria-residential",
            "hysteria-residential-1",
            "hysteria-residential-2",
        ] {
            assert!(units.contains(&u), "{u} 不在 {units:?}");
        }
        assert_eq!(
            engine::sentinel_of(&k.ctx.runtime.read().await)
                .since
                .as_deref(),
            Some("2026-09-11T00:00:00Z"),
            "起点落盘：重启后不会回头读更早的日志"
        );
    }

    #[tokio::test]
    async fn reading_continues_after_the_last_cursor_and_survives_a_restart() {
        let k = kit().await;
        feed(&k, vec![rec("xray", 0, "Xray 26.3.27 started")]);
        let mut s = Sentinel::default();
        assert_eq!(tick(&k.ctx, &k.deps, &mut s).await.read, 1);
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await;
        assert!(
            journal_op(&k.host).ends_with(":cursor=c-xray-0"),
            "{}",
            journal_op(&k.host)
        );
        // 守护进程重启：新的 Sentinel 从 runtime 里续读
        let mut fresh = Sentinel::default();
        k.host.clear_ops();
        tick(&k.ctx, &k.deps, &mut fresh).await;
        assert!(journal_op(&k.host).ends_with(":cursor=c-xray-0"));
    }

    #[tokio::test]
    async fn a_failed_read_drops_the_cursor_and_restarts_from_now() {
        let k = kit().await;
        k.host.with(|i| {
            i.journal.push_back(Ok(vec![rec("xray", 0, "x")]));
            i.journal
                .push_back(Err("Failed to seek to cursor: Invalid argument".into()));
        });
        let mut s = Sentinel::default();
        tick(&k.ctx, &k.deps, &mut s).await;
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await; // 读失败
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await;
        assert!(
            journal_op(&k.host).ends_with(":since=2026-09-11T00:00:05Z"),
            "游标失效 ⇒ 从失败那一刻读起，不回放：{}",
            journal_op(&k.host)
        );
    }

    #[tokio::test]
    async fn two_timeouts_to_one_upstream_give_one_probe_one_borrow_one_incident() {
        let k = kit().await;
        // 借用目标（resi-1）探得通：借用后要用同一个快探验证它（spec §5.7）
        k.prober.with_gateways_up(&["isp1.example.net:10007"]);
        k.host.advance(3);
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        let mut s = Sentinel::default();
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        let inc = &rep.incidents[0];
        assert_eq!(
            (
                inc.unit.as_str(),
                inc.signature.as_str(),
                inc.subject.as_str(),
                inc.action.as_str()
            ),
            (
                "b-ui-relay",
                "relay_upstream_error",
                "isp2.example.net:10007",
                "probe_and_borrow"
            )
        );
        assert_eq!(inc.level, Level::Error);
        assert_eq!(
            inc.result,
            "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(inc.at, "2026-09-11T00:00:03Z");
        assert!(inc.sample.as_deref().unwrap().contains("i/o timeout"));
        assert_eq!(k.clash.selected("slot-1-pool").as_deref(), Some("resi-1"));
        assert_eq!(
            incidents::from_runtime(&k.ctx.runtime.read().await),
            rep.incidents,
            "事件落盘"
        );
        assert_eq!(k.notes.0.lock().unwrap().len(), 1, "外部通知口收到一次");
        assert!(
            engine::sentinel_of(&k.ctx.runtime.read().await)
                .acted
                .contains_key(&engine::action_key(
                    Sig::RelayUpstreamError,
                    &Uuid::from_u128(2).to_string()
                )),
            "真借用了 ⇒ 盖 10 分钟冷却"
        );

        // 10 秒后又来 3 条：去抖窗口内，不触发
        k.host.advance(10);
        feed(
            &k,
            (13..16)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty());
        // 再过 61 秒又来 3 条：去抖已过，但同动作同对象 10 分钟冷却
        k.host.advance(61);
        feed(
            &k,
            (74..77)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty());
        assert_eq!(
            k.prober.calls().iter().filter(|c| *c == "tcp").count(),
            2,
            "整段只探了两次：故障 IP 一次 + 借用目标的验证一次（{:?}）",
            k.prober.calls()
        );
    }

    #[tokio::test]
    async fn one_timeout_unknown_tags_and_stale_backlog_do_nothing() {
        let k = kit().await;
        let mut s = Sentinel::default();
        feed(&k, vec![rec("b-ui-relay", 0, &timeout("resi-2"))]);
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "1 条不触发（连接类门槛 2 条）"
        );
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-9")))
                .collect(),
        );
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "池里没有 resi-9"
        );
        feed(
            &k,
            (-300..-298)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-3")))
                .collect(),
        );
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "5 分钟前的积压"
        );
        assert!(k.prober.calls().is_empty() && k.clash.calls().is_empty());
    }

    const AUTH_FAIL: &str = "hysteria[1]: authentication error {\"error\": \"Post \
        \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: connect: connection refused\"}";

    #[tokio::test]
    async fn hysteria_auth_failures_alert_in_http_mode() {
        let k = kit().await;
        feed(
            &k,
            (0..3)
                .map(|s| rec("hysteria-residential-1", s, AUTH_FAIL))
                .collect(),
        );
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1);
        assert_eq!(rep.incidents[0].signature, "hy2_auth_http_failed");
        assert_eq!(rep.incidents[0].subject, "hysteria-residential-1");
        assert_eq!(rep.incidents[0].action, "alert");
        assert!(k.host.ops().iter().all(|o| !o.starts_with("systemd:")));
    }

    #[tokio::test]
    async fn hysteria_auth_lines_are_ignored_in_command_mode() {
        let k = kit().await;
        k.ctx
            .store
            .update(|s| s.system.hy2_auth = bui_schema::model::Hy2Auth::Command)
            .await
            .unwrap();
        feed(
            &k,
            (0..3)
                .map(|s| rec("hysteria-server", s, AUTH_FAIL))
                .collect(),
        );
        assert!(tick(&k.ctx, &k.deps, &mut Sentinel::default())
            .await
            .incidents
            .is_empty());
    }

    #[tokio::test]
    async fn a_bind_conflict_is_recorded_and_left_to_the_watchdog() {
        let k = kit().await;
        feed(
            &k,
            vec![rec(
                "xray",
                0,
                "Failed to start: main: failed to start server > listen tcp 0.0.0.0:10001: \
                 bind: address already in use",
            )],
        );
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1);
        assert_eq!(
            (
                rep.incidents[0].signature.as_str(),
                rep.incidents[0].action.as_str()
            ),
            ("kernel_bind_in_use", "delegate_watchdog")
        );
        assert!(k.host.ops().iter().all(|o| !o.starts_with("systemd:")));
    }

    /// 带外探测通过 = 什么都没做：不占 10 分钟动作冷却，只剩 60 秒去抖。否则一次误报（HTTP 上游对慢
    /// 目标报 deadline exceeded）会让哨兵对这条上游失明 10 分钟，真故障只能等巡检
    #[tokio::test]
    async fn a_passing_probe_takes_no_cooldown_so_the_next_burst_is_probed_again() {
        let k = kit().await;
        k.prober.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Ok(crate::modules::residential::proxy::HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        k.host.advance(3);
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        let mut s = Sentinel::default();
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        assert_eq!(rep.incidents[0].level, Level::Info);
        assert!(
            engine::sentinel_of(&k.ctx.runtime.read().await)
                .acted
                .is_empty(),
            "探测通过不占动作冷却"
        );
        // 61 秒后同一上游又攒满 2 条：去抖已过 ⇒ 再探一次
        k.host.advance(61);
        feed(
            &k,
            (61..63)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        assert_eq!(
            k.prober.calls().iter().filter(|c| *c == "tcp").count(),
            2,
            "{:?}",
            k.prober.calls()
        );
        assert!(k.clash.calls().is_empty(), "两次都探通，一次都不借");
    }

    fn auth_fail(tag: &str) -> String {
        format!(
            "ERROR[4007] [2302991393 0.31s] connection: open connection to www.gstatic.com:443 \
             using outbound/http[{tag}]: unexpected status: 407 Proxy Authentication Required"
        )
    }

    /// 丢包时 SYN 没有回音：探网关的 TCP 要等满给它的时限。把这段时间记到假时钟上，
    /// 事件的 `at`（预案做完才盖）才量得出预案的真实耗时
    fn gateway_dropped(k: &Kit) {
        let host = k.host.clone();
        k.prober.with(|i| {
            i.on_tcp_fail = Some(Box::new(move |within| {
                host.advance(within.as_secs() as i64)
            }))
        });
    }

    fn secs_until(from: OffsetDateTime, at: &str) -> i64 {
        (parse_rfc3339(at).expect("at 是 RFC3339") - from).whole_seconds()
    }

    /// 演练判据①的延迟预算（2026-09-13 真机 30.3 秒、SLA 15 秒之后）：达到门槛的那条错误最晚在它
    /// 之后一个轮询周期被读到；带外探测 TCP 不通最多等 PROBE_TCP_TIMEOUT_SECS 就判不可达，不再跑
    /// 完整探测；借用 = 一次 Clash PUT 加借到那条的带外验证 —— 这条快路上验证的目标网关是通的
    /// （只丢了一个端口），一次经隧道的 GET 远小于 1 秒，所以留 1 秒。**验证走不通的那条慢路**
    /// （网关活着、出口 IP 死了）由 `slots::BORROW_VERIFY_BUDGET_SECS` 的整体时限兜住，见
    /// `with_the_whole_gateway_down_the_event_lands_within_the_no_exit_sla` 与
    /// `slots::tests::a_verification_that_outruns_its_budget_keeps_the_candidate_and_says_so`
    #[tokio::test]
    async fn a_dropped_gateway_is_borrowed_within_one_poll_plus_the_tcp_timeout() {
        assert_eq!(
            (POLL_SECS, super::super::PROBE_TCP_TIMEOUT_SECS),
            (2, 3),
            "spec §5.7 的数字"
        );
        let k = kit().await;
        gateway_dropped(&k);
        // 借用目标还活着（演练只丢弃**一个**端口 = 一个出口 IP），验证它不吃时限
        k.prober.with_gateways_up(&["isp1.example.net:10007"]);
        // 两条并发连接同时报错（演练就是并发三个请求），刚好落在上一轮之后：一个轮询周期后才读到
        k.host.advance(POLL_SECS as i64);
        let reached = rec("b-ui-relay", 0, &timeout("resi-2"));
        let ts = reached.ts;
        feed(&k, vec![rec("b-ui-relay", 0, &timeout("resi-2")), reached]);
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        assert_eq!(rep.incidents[0].level, Level::Error);
        assert_eq!(k.clash.selected("slot-1-pool").as_deref(), Some("resi-1"));
        assert_eq!(
            k.prober.calls(),
            vec![
                "tcp".to_string(),
                "tcp".to_string(),
                format!("timed:{}", crate::modules::residential::LATENCY_PROBE_URL),
            ],
            "TCP 不通即判不可达，不再跑完整探测；借用目标再验证一次"
        );
        let budget = POLL_SECS as i64 + super::super::PROBE_TCP_TIMEOUT_SECS as i64 + 1;
        let took = secs_until(ts, &rep.incidents[0].at);
        assert!(
            took <= budget,
            "达到门槛的错误 → 事件 {took} 秒，预算 {budget} 秒"
        );
    }

    /// **整个网关不可用**（池里各条都在同一个网关上）时最慢的那条路：原上游快探 3 秒 +
    /// 每个候选「PUT + 快探」+ 收尾 PUT，事件才落地。演练判据拆成两条正是为此：
    /// 有可用出口 ≤15 秒，无可用出口 ≤25 秒（`DRILL_NOEXIT_SLA`）。
    /// 这里的假时钟只推得动「网关 TCP 连不上」那一段（`on_tcp_fail`），与真机丢包一样 ——
    /// 网关活着、HTTP 卡住那一段的上界靠 `slots::BORROW_VERIFY_BUDGET_SECS` 的 `timeout`，
    /// 在 `slots` 那边用假件单独测（丢包量不到它）。
    /// 候选数上限是**单次调用**的 `slots::BORROW_PROBES_PER_CALL`，所以这里的算式
    /// `POLL + (1 + 候选数) × PROBE_TCP_TIMEOUT_SECS` 在 8 条上游的生产池上同样成立
    #[tokio::test]
    async fn with_the_whole_gateway_down_the_event_lands_within_the_no_exit_sla() {
        const DRILL_NOEXIT_SLA: i64 = 25;
        let k = kit().await;
        gateway_dropped(&k); // 池里每一条都连不上，各等满 PROBE_TCP_TIMEOUT_SECS
        k.host.advance(POLL_SECS as i64);
        let reached = rec("b-ui-relay", 0, &timeout("resi-2"));
        let ts = reached.ts;
        feed(&k, vec![rec("b-ui-relay", 0, &timeout("resi-2")), reached]);
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        let inc = &rep.incidents[0];
        assert_eq!(
            inc.result,
            "IP 198.51.100.8 不可达，槽 1：候选 198.51.100.7、198.51.100.9 都探不通，\
             当前指向本槽 IP 198.51.100.8，当前无可用出口"
        );
        assert!(!inc.result.contains("已临时切到"), "{}", inc.result);
        assert_eq!(
            k.clash.selected("slot-1-pool").as_deref(),
            Some("resi-2"),
            "终态 = 放回本槽自己的上游，不许停在死候选上"
        );
        assert_eq!(
            crate::modules::residential::state::read(&k.ctx.runtime)
                .await
                .slots["1"]
                .current_upstream_id,
            Some(Uuid::from_u128(2))
        );
        let took = secs_until(ts, &inc.at);
        let budget = POLL_SECS as i64 + 3 * super::super::PROBE_TCP_TIMEOUT_SECS as i64;
        assert_eq!(took, budget, "原上游 + 两个候选各一次快探");
        assert!(took <= DRILL_NOEXIT_SLA, "首条错误 → 事件 {took} 秒");
    }

    /// 最坏情况（客户端串行、错误一条一条来）：第 2 条错误要等 relay 再拨一次上游、超时才出现。
    /// relay 的拨号超时 = sing-box 的 `C.TCPConnectTimeout` 5 秒（constant/timeout.go；relay 出站
    /// 不设 `connect_timeout`，见 bui-schema `render::relay`）。首条错误 → 事件 = 5 + 2 + 3 = 10 秒
    #[tokio::test]
    async fn errors_one_relay_dial_timeout_apart_still_meet_the_drill_sla() {
        const RELAY_DIAL_SECS: i64 = 5;
        const DRILL_DETECT_SLA: i64 = 15;
        let k = kit().await;
        gateway_dropped(&k);
        k.prober.with_gateways_up(&["isp1.example.net:10007"]);
        let mut s = Sentinel::default();
        k.host.advance(POLL_SECS as i64);
        let first = rec("b-ui-relay", 0, &timeout("resi-2"));
        let t0 = first.ts;
        feed(&k, vec![first]);
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "1 条不触发"
        );
        // 第 2 条在第 5 秒出现，一个轮询周期后读到（此刻 = 7）
        k.host.advance(RELAY_DIAL_SECS);
        feed(
            &k,
            vec![rec("b-ui-relay", RELAY_DIAL_SECS, &timeout("resi-2"))],
        );
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        let took = secs_until(t0, &rep.incidents[0].at);
        assert_eq!(
            took,
            RELAY_DIAL_SECS + POLL_SECS as i64 + super::super::PROBE_TCP_TIMEOUT_SECS as i64
        );
        assert!(took <= DRILL_DETECT_SLA, "首条错误 → 事件 {took} 秒");
    }

    /// TCP 连得上 ⇒ 仍走原来的完整探测（经隧道两次请求 + 407 补判），不能因为「网关在」就放过
    #[tokio::test]
    async fn a_reachable_gateway_still_gets_the_full_probe() {
        use crate::modules::residential::{HEALTH_PROBE_HOST, HEALTH_PROBE_URL, LATENCY_PROBE_URL};
        let k = kit().await;
        k.prober.with(|i| {
            // 网关通；经隧道的请求缺省全失败。借用目标那条经隧道通（按上游限定的一格），验证得过
            i.tcp_ms = Some(20);
            i.gets.insert(
                format!("isp1.example.net:10007 {LATENCY_PROBE_URL}"),
                Ok(crate::modules::residential::proxy::HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        k.host.advance(2);
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        assert_eq!(rep.incidents[0].level, Level::Error);
        assert_eq!(
            k.prober.calls(),
            vec![
                "tcp".to_string(),
                format!("timed:{LATENCY_PROBE_URL}"),
                format!("get:{HEALTH_PROBE_URL}"),
                format!("connect:{HEALTH_PROBE_HOST}:443"),
                // 借用目标的验证：同一个快探，网关通就走一轮可达性
                "tcp".to_string(),
                format!("timed:{LATENCY_PROBE_URL}"),
            ]
        );
        assert_eq!(k.clash.selected("slot-1-pool").as_deref(), Some("resi-1"));
    }

    /// 凭据类签名门槛不变（60 秒 3 条），与连接类共用同一个预案与动作冷却
    #[tokio::test]
    async fn credential_failures_still_need_three() {
        let k = kit().await;
        k.prober.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Err("__auth_failed__".into()),
            );
            // 凭据失效的只有这一条：借用目标（resi-1）验证得过
            i.gets.insert(
                format!(
                    "isp1.example.net:10007 {}",
                    crate::modules::residential::LATENCY_PROBE_URL
                ),
                Ok(crate::modules::residential::proxy::HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        k.host.advance(3);
        let mut s = Sentinel::default();
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &auth_fail("resi-2")))
                .collect(),
        );
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "2 条凭据失效不触发"
        );
        feed(&k, vec![rec("b-ui-relay", 2, &auth_fail("resi-2"))]);
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        let inc = &rep.incidents[0];
        assert_eq!(
            (inc.signature.as_str(), inc.action.as_str()),
            ("relay_upstream_auth_failed", "probe_and_borrow")
        );
        assert_eq!(
            inc.result,
            "IP 198.51.100.8 凭据失效（407 / SOCKS5 认证被拒），槽 1 已临时切到 198.51.100.7"
        );
        assert!(
            engine::sentinel_of(&k.ctx.runtime.read().await)
                .acted
                .contains_key(&engine::action_key(
                    Sig::RelayUpstreamError,
                    &Uuid::from_u128(2).to_string()
                )),
            "与连接类共用一条动作冷却"
        );
    }

    /// 守护进程停机太久：持久化的读起点早于最长签名窗口（150 秒）⇒ 丢掉游标、从现在读起。那段积压里的
    /// 每一条都会被去抖器当陈旧日志丢掉（D12），读它只是把大量日志一次读进内存
    #[tokio::test]
    async fn a_persisted_start_older_than_the_longest_window_restarts_from_now() {
        let now = ":since=2026-09-11T00:00:00Z";
        for (seed, want, why) in [
            (
                serde_json::json!({"cursor": "c-old", "cursor_at": "2026-09-10T23:57:29Z"}),
                now,
                "游标读到 151 秒前",
            ),
            (
                serde_json::json!({"cursor": "c-old"}),
                now,
                "旧构建落的游标没有读取时刻：年龄不明，按过旧处理",
            ),
            (
                serde_json::json!({"since": "2026-09-10T20:00:00Z"}),
                now,
                "没有游标、起点是 4 小时前",
            ),
            (
                serde_json::json!({"cursor": "c-old", "cursor_at": "2026-09-10T23:57:40Z"}),
                ":cursor=c-old",
                "停机 140 秒：还在窗口内，照常续读",
            ),
        ] {
            let k = kit().await;
            k.ctx
                .runtime
                .update(move |rt| {
                    rt.extra.insert(engine::SENTINEL_KEY.into(), seed);
                })
                .await;
            tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
            let op = journal_op(&k.host);
            assert!(op.ends_with(want), "{why}：{op}");
            if want == now {
                let rt = k.ctx.runtime.read().await;
                let sr = &rt.extra[engine::SENTINEL_KEY];
                assert_eq!(
                    (sr["cursor"].as_str(), sr["since"].as_str()),
                    (None, Some("2026-09-11T00:00:00Z")),
                    "{why}：新起点落盘"
                );
            }
        }
    }

    /// 日志安静时游标不动，但「读到哪一刻」照样随节流落盘：否则安静 10 分钟后重启一次，
    /// 游标看着就像 10 分钟前的，会被当成过旧丢掉
    #[tokio::test]
    async fn quiet_logs_still_refresh_the_persisted_cursor_time() {
        let k = kit().await;
        feed(&k, vec![rec("xray", 0, "Xray 26.3.27 started")]);
        let mut s = Sentinel::default();
        tick(&k.ctx, &k.deps, &mut s).await;
        k.host.advance(61);
        tick(&k.ctx, &k.deps, &mut s).await; // 没有新日志
        assert_eq!(
            k.ctx.runtime.read().await.extra[engine::SENTINEL_KEY]["cursor_at"].as_str(),
            Some("2026-09-11T00:01:01Z")
        );
        // 停机 100 秒后重启：离上次落盘的读取时刻还在窗口内 ⇒ 续读
        k.host.advance(100);
        tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert!(
            journal_op(&k.host).ends_with(":cursor=c-xray-0"),
            "{}",
            journal_op(&k.host)
        );
    }

    #[tokio::test]
    async fn the_module_renders_nothing_and_spawns_one_task() {
        let k = kit().await;
        let m = SentinelModule::with(k.deps.clone());
        assert_eq!(m.name(), "sentinel");
        let handles = m.spawn(k.ctx.clone());
        assert_eq!(handles.len(), 1);
        for h in handles {
            h.abort();
        }
    }
}
