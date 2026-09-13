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

/// 常驻在循环里的哨兵状态
#[derive(Default)]
pub struct Sentinel {
    engine: Engine,
    /// 首轮从 runtime 读出，此后**以内存为准**（落盘有节流；每轮都从 runtime 读会重复读同一段日志）
    sr: Option<SentinelRuntime>,
    last_persist: Option<OffsetDateTime>,
    /// 上一次读 journald 的错误文案：同样的错误只打一次日志（没装 journalctl 时每 5 秒一条是刷屏）
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
    if s.sr.is_none() {
        s.sr = Some(engine::sentinel_of(&ctx.runtime.read().await));
    }
    let mut sr = s.sr.clone().unwrap_or_default();
    let before = sr.clone();

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
            v
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            if s.last_error.as_deref() != Some(msg.as_str()) {
                tracing::warn!(error = %msg, "哨兵读 journald 失败：丢掉游标，从现在读起（不回放）");
            }
            s.last_error = Some(msg);
            sr.cursor = None;
            sr.since = Some(fmt_rfc3339(now));
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "哨兵读 journald 的任务异常");
            Vec::new()
        }
    };
    if let Some(last) = records.last() {
        sr.cursor = Some(last.cursor.clone());
    }

    // ② 匹配 + 去抖。relay 的对象当场从成员 tag（位置键）换成 uuid，换不出来的丢弃
    let mut fired: Vec<Fired> = Vec::new();
    for r in &records {
        let Some(m) = signature::classify(&r.unit, &r.message) else {
            continue;
        };
        if m.sig == Sig::Hy2AuthHttpFailed && !hy2_http {
            continue;
        }
        let upstream = match m.sig {
            Sig::RelayUpstreamError | Sig::RelayGoogleBlocked => {
                match clash::id_of_tag(&g, &m.subject) {
                    Some(id) => Some(id),
                    None => continue,
                }
            }
            _ => None,
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
        sr.acted.insert(akey, fmt_rfc3339(now));
        out.push(Incident {
            // 预案做完（快探最长 PROBE_TIMEOUT_SECS + 借用的 PUT）之后才盖时间戳：事件时刻 = 该槽
            // 已借用的时刻。演练判据①「首条错误 → 记事件并借用 ≤15 秒」量的是它，不是本轮开头
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

    // ⑤ 落盘：有事件 / 冷却表或起点变了 ⇒ 立即；只有游标前进 ⇒ 至少隔 CURSOR_PERSIST_SECS
    let others_changed = {
        let (mut a, mut b) = (sr.clone(), before.clone());
        a.cursor = None;
        b.cursor = None;
        a != b
    };
    let cursor_due = sr.cursor != before.cursor
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
        (Sig::RelayUpstreamError, Some(id)) => {
            resi::on_upstream_error(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        (Sig::RelayGoogleBlocked, Some(id)) => {
            resi::on_google_blocked(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        // relay 签名的对象在匹配那一步就换成了 uuid（换不出来的已丢弃）
        (Sig::RelayUpstreamError | Sig::RelayGoogleBlocked, None) => {
            unreachable!("relay 签名必带上游 uuid")
        }
        (Sig::Hy2AuthHttpFailed, _) => system::on_hy2_auth(ctx, &r.unit).await,
        (Sig::KernelBindInUse | Sig::KernelCrashLoop, _) => system::on_kernel(&r.unit, sig),
        (Sig::XrayGrpcUnavailable, _) => system::on_xray_grpc(ctx, &deps.panel).await,
        (Sig::CaddyCertFailed, _) => system::on_caddy_cert(key),
    }
}

/// 每 [`POLL_SECS`] 秒一轮；一轮里的预案（带外探测最长 ~5 秒）拖长了就顺延，不补跑
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
    async fn three_timeouts_to_one_upstream_give_one_probe_one_borrow_one_incident() {
        let k = kit().await;
        k.host.advance(3);
        feed(
            &k,
            (0..3)
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
        assert_eq!(k.prober.calls(), vec!["tcp"], "整段只探了一次");
    }

    #[tokio::test]
    async fn two_timeouts_unknown_tags_and_stale_backlog_do_nothing() {
        let k = kit().await;
        let mut s = Sentinel::default();
        feed(
            &k,
            (0..2)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-2")))
                .collect(),
        );
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "不到 3 条"
        );
        feed(
            &k,
            (0..3)
                .map(|s| rec("b-ui-relay", s, &timeout("resi-9")))
                .collect(),
        );
        assert!(
            tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(),
            "池里没有 resi-9"
        );
        feed(
            &k,
            (-300..-297)
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
