//! 流量采样、用量累加、限额执行（spec §4.2）。
//!
//! 与 v3 的差别（审计 eff-C1~C4、web-C2~C5）：
//! - 一次 `QueryStats(pattern="user>>>", reset=true)` 拉全量，不再每用户两次 `execSync`（O(N) 阻塞）。
//! - 住宅实例的 `:9998` 也读（v3 的常量从没被用过）。
//! - `clear=1` / `reset=true` 让每轮拿到的就是增量，重启守护进程不会把累计值当增量重复计。
//! - 限额真的执行（v3 的 `checkUserLimits` 哪里都没被调用）。
//! - `/api/stats`、`/api/online` 读同一份缓存，面板开着不增加采样。

use super::users::{self, month_key};
use super::{Shared, TxRx, HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI};
use crate::reconcile::DaemonCtx;
use bui_schema::model::State;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// spec §4.2「守护进程内 10 秒任务」
pub const SAMPLE_INTERVAL_SECS: u64 = 10;
/// spec §4.2「最多每 30 秒合并落盘一次」
pub const FLUSH_INTERVAL_SECS: i64 = 30;
/// spec §4.2「最近 30 秒有 Xray 增量的用户」算在线
pub const XRAY_ONLINE_WINDOW_SECS: i64 = 30;

/// 一轮采样的原始结果（键都是 `user_id` 的字符串）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub deltas: BTreeMap<String, TxRx>,
    pub online: BTreeMap<String, u32>,
    /// 本轮有 Xray 增量的用户（在线判定用）
    pub xray_ids: BTreeSet<String>,
    pub errors: Vec<String>,
}

/// 打两个 hysteria 的 `/traffic?clear=1` 与 `/online`，再打一次 `QueryStats(reset=true)`。
/// 单个来源失败只记 error，不影响其余来源（spec §4.2；审计 web-C3：住宅 9998 也要读）。
pub async fn sample_once(shared: &Shared) -> Sample {
    let mut s = Sample::default();
    // 顺序固定（9999 → 9998 → online 9999 → online 9998 → Xray），测试按这个顺序断言 calls()
    for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
        match shared.hy2().traffic_clear(port).await {
            Ok(m) => {
                for (id, d) in m {
                    s.deltas.entry(id).or_default().add(d);
                }
            }
            Err(e) => s
                .errors
                .push(format!("hysteria :{port} /traffic 失败：{e}")),
        }
    }
    for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
        match shared.hy2().online(port).await {
            Ok(m) => {
                for (id, n) in m {
                    *s.online.entry(id).or_insert(0) += n;
                }
            }
            Err(e) => s.errors.push(format!("hysteria :{port} /online 失败：{e}")),
        }
    }
    match shared.xray().query_user_deltas().await {
        Ok(m) => {
            for (id, d) in m {
                s.xray_ids.insert(id.clone());
                s.deltas.entry(id).or_default().add(d);
            }
        }
        Err(e) => s.errors.push(format!("Xray QueryStats 失败：{e}")),
    }
    s
}

/// `user_id` 字符串表 → `Uuid` 表；认不出的键丢弃（并不报错：内核里可能还留着已删用户的计数器）。
pub fn to_uuid_map(raw: &BTreeMap<String, TxRx>) -> BTreeMap<Uuid, TxRx> {
    raw.iter()
        .filter_map(|(k, v)| Uuid::parse_str(k).ok().map(|id| (id, *v)))
        .collect()
}

/// 用 state 把 `user_id` 键的表翻译成用户名键的表（`/api/stats`、`/api/online` 要用户名）。
pub fn by_username<T: Copy>(state: &State, raw: &BTreeMap<Uuid, T>) -> BTreeMap<String, T> {
    let names: BTreeMap<Uuid, &str> = state
        .users
        .iter()
        .map(|u| (u.user_id, u.username.as_str()))
        .collect();
    raw.iter()
        .filter_map(|(id, v)| names.get(id).map(|n| (n.to_string(), *v)))
        .collect()
}

/// 把一轮增量并进期望态（纯函数）：月度重置 → 累加 → `last_seen_at`。返回被改动的用户数。
pub fn apply_sample(
    state: &mut State,
    deltas: &BTreeMap<Uuid, TxRx>,
    now: OffsetDateTime,
) -> usize {
    let mk = month_key(now);
    let ts = crate::util::fmt_rfc3339(now);
    let mut changed = 0;
    for u in state.users.iter_mut() {
        let d = deltas.get(&u.user_id).copied().unwrap_or_default();
        // 月度重置：即使本轮没有流量也要翻月份，否则 `is_blocked` 会拿上个月的用量判这个月
        let rolled = u.usage.month_key != mk;
        if !rolled && d.is_zero() {
            continue;
        }
        if rolled {
            u.usage.month_key = mk.clone();
            u.usage.monthly_bytes = 0;
        }
        let total = d.total();
        if total > 0 {
            u.usage.total_bytes = u.usage.total_bytes.saturating_add(total);
            u.usage.monthly_bytes = u.usage.monthly_bytes.saturating_add(total);
            u.usage.last_seen_at = Some(ts.clone());
        }
        changed += 1;
    }
    changed
}

/// 一个完整周期：采样 → 累加到内存 → 到点落盘 → 限额执行（含 `/kick`）→ 刷新缓存。
///
/// 落盘节奏：`pending` 累加每一轮的增量，距上次落盘 ≥ [`FLUSH_INTERVAL_SECS`] 时在一次
/// `store.update` 里全部并进 `usage` 并清空。限额判定用 `usage + pending`
/// （`users::is_blocked` 的 `extra` 参数），所以「还没落盘」不会让超限用户多跑 30 秒。
///
/// 已知降级：`pending` 只在内存里，`systemctl restart b-ui` / SIGTERM 最多丢 30 秒的计数
/// （`SampleCache` 与 `Applied` 同样要下一轮重建，两者都是幂等的）。
pub async fn tick(ctx: &DaemonCtx, shared: &Shared) -> anyhow::Result<()> {
    let now = ctx.host.now();
    let sample = sample_once(shared).await;
    let deltas = to_uuid_map(&sample.deltas);

    // ① 内存累加
    {
        let mut pending = shared.pending().await;
        for (id, d) in &deltas {
            pending.entry(*id).or_default().add(*d);
        }
    }
    // ② Xray 在线窗口
    {
        let mut seen = shared.xray_seen().await;
        for id in &sample.xray_ids {
            if let Ok(uid) = Uuid::parse_str(id) {
                seen.insert(uid, now);
            }
        }
        seen.retain(|_, t| (now - *t).whole_seconds() < XRAY_ONLINE_WINDOW_SECS);
    }

    // ③ 到点落盘（spec §4.2「最多每 30 秒合并落盘一次」）
    let due = {
        let cache = shared.cache().await;
        match cache.last_flush_at {
            Some(t) => (now - t).whole_seconds() >= FLUSH_INTERVAL_SECS,
            None => false,
        }
    };
    if due {
        let pending = std::mem::take(&mut *shared.pending().await);
        ctx.store
            .update(|s| {
                apply_sample(s, &pending, now);
            })
            .await?;
    }

    // ④ 限额执行与恢复（快照拒绝 + Xray 增删 + kick）
    let out = users::sync_now(ctx, shared).await;
    if !out.newly_blocked.is_empty() {
        let ids: Vec<String> = out.newly_blocked.iter().map(Uuid::to_string).collect();
        for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
            if let Err(e) = shared.hy2().kick(port, &ids).await {
                tracing::warn!(port, error = %e, "kick 失败；快照拒绝仍然生效");
            }
        }
    }

    // ⑤ 刷新面板缓存
    let state = ctx.store.read().await;
    let mut online: BTreeMap<Uuid, u32> = BTreeMap::new();
    for (id, n) in &sample.online {
        if let Ok(uid) = Uuid::parse_str(id) {
            *online.entry(uid).or_insert(0) += *n;
        }
    }
    // Xray 没有连接数接口：最近 30 秒有增量就按 1 计（spec §4.2 的并集）
    for uid in shared.xray_seen().await.keys() {
        online.entry(*uid).or_insert(1);
    }
    let mut cache = shared.cache_mut().await;
    for (id, d) in &deltas {
        if let Some(name) = state
            .users
            .iter()
            .find(|u| u.user_id == *id)
            .map(|u| u.username.clone())
        {
            cache.stats.entry(name).or_default().add(*d);
        }
    }
    cache.online = by_username(&state, &online);
    cache.last_sample_at = Some(crate::util::fmt_rfc3339(now));
    // 第一轮把 `last_flush_at` 设成当轮时间 ⇒ 第 31 秒起才开始落盘
    if due || cache.last_flush_at.is_none() {
        cache.last_flush_at = Some(now);
    }
    cache.errors = sample.errors;
    Ok(())
}

/// 汇总进 `runtime.extra["users"]`（由 T8 的 `GET /api/users/health` 读出来；
/// 裁决 D2 不批准把这段塞进 `/api/health`）。
pub async fn write_health_summary(ctx: &DaemonCtx, shared: &Shared) {
    let now = ctx.host.now();
    let summary = {
        let state = ctx.store.read().await;
        let pending = shared.pending().await.clone();
        let blocked = users::blocked_set(&state, &pending, now);
        let cache = shared.cache().await;
        let xray_synced = shared.applied().await.xray_users.len();
        serde_json::json!({
            "total": state.users.len(),
            "disabled": state.users.iter().filter(|u| u.disabled).count(),
            "blocked": blocked.len(),
            "online": cache.online.len(),
            "xray_synced": xray_synced,
            "month_key": month_key(now),
            "last_sample_at": cache.last_sample_at,
            "pending_users": pending.len(),
            "sample_errors": cache.errors,
        })
    };
    ctx.runtime
        .update(|r| {
            r.extra.insert("users".into(), summary);
        })
        .await;
}

pub async fn sampling_loop(ctx: DaemonCtx, shared: Arc<Shared>) {
    let mut iv = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        if let Err(e) = tick(&ctx, &shared).await {
            tracing::warn!(error = %e, "采样周期失败");
        }
        write_health_summary(&ctx, &shared).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{harness, Harness};
    use crate::modules::panel::users;
    use crate::reconcile::DaemonCtx;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn ctx_of(h: &Harness) -> DaemonCtx {
        DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        }
    }

    #[test]
    fn apply_sample_accumulates_total_and_monthly_and_sets_last_seen() {
        let mut s = sample_state();
        let id = s.users[0].user_id;
        s.users[0].usage = bui_schema::model::Usage {
            total_bytes: 100,
            monthly_bytes: 40,
            month_key: "2026-09".into(),
            last_seen_at: None,
        };
        let n = apply_sample(&mut s, &BTreeMap::from([(id, TxRx { tx: 3, rx: 4 })]), t0());
        assert_eq!(n, 1);
        assert_eq!(s.users[0].usage.total_bytes, 107);
        assert_eq!(s.users[0].usage.monthly_bytes, 47);
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(
            s.users[0].usage.last_seen_at.as_deref(),
            Some("2026-09-11T00:00:00Z")
        );
    }

    #[test]
    fn a_new_month_resets_monthly_but_not_total() {
        let mut s = sample_state();
        let id = s.users[0].user_id;
        s.users[0].usage = bui_schema::model::Usage {
            total_bytes: 100,
            monthly_bytes: 90,
            month_key: "2026-08".into(),
            last_seen_at: None,
        };
        apply_sample(&mut s, &BTreeMap::from([(id, TxRx { tx: 5, rx: 0 })]), t0());
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(s.users[0].usage.monthly_bytes, 5, "跨月先清零再加本轮");
        assert_eq!(s.users[0].usage.total_bytes, 105, "总量不清零");
    }

    #[test]
    fn a_month_rollover_with_no_traffic_still_resets() {
        // 月初第一轮采样可能一个字节都没有，`month_key` 也必须翻过去，
        // 否则 `is_blocked` 会拿上个月的用量继续判这个月的月度上限
        let mut s = sample_state();
        s.users[0].usage.month_key = "2026-08".into();
        s.users[0].usage.monthly_bytes = 999;
        let n = apply_sample(&mut s, &BTreeMap::new(), t0());
        assert_eq!(n, 1, "只是重置月份也算改动，要落盘");
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(s.users[0].usage.monthly_bytes, 0);
    }

    #[test]
    fn unknown_ids_are_dropped_and_usernames_are_resolved() {
        let s = sample_state();
        let id = s.users[0].user_id;
        let raw = BTreeMap::from([
            (id.to_string(), TxRx { tx: 1, rx: 2 }),
            ("not-a-uuid".to_string(), TxRx { tx: 9, rx: 9 }),
        ]);
        let m = to_uuid_map(&raw);
        assert_eq!(m, BTreeMap::from([(id, TxRx { tx: 1, rx: 2 })]));
        assert_eq!(
            by_username(&s, &m),
            BTreeMap::from([("alice".to_string(), TxRx { tx: 1, rx: 2 })])
        );
        // state 里没有的 user_id（刚删的用户，内核里计数器还在）也丢掉
        let ghost = BTreeMap::from([(uuid::Uuid::nil(), 1u32)]);
        assert!(by_username(&s, &ghost).is_empty());
    }

    #[tokio::test]
    async fn sample_once_reads_both_hysteria_ports_and_xray() {
        let h = harness().await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.traffic
                .insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 10, rx: 0 })]));
            i.traffic
                .insert(9998, BTreeMap::from([(id.clone(), TxRx { tx: 1, rx: 2 })]));
            i.online.insert(9999, BTreeMap::from([(id.clone(), 1u32)]));
            i.online.insert(9998, BTreeMap::from([(id.clone(), 2u32)]));
        });
        h.xray.with(|i| {
            i.deltas.insert(id.clone(), TxRx { tx: 0, rx: 100 });
        });
        let s = sample_once(&h.shared).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 11, rx: 102 }, "三个来源相加");
        assert_eq!(s.online[&id], 3, "两个 /online 的值相加");
        assert!(s.xray_ids.contains(&id));
        assert!(s.errors.is_empty());
        assert_eq!(
            h.hy2.calls(),
            vec![
                "traffic:9999".to_string(),
                "traffic:9998".to_string(),
                "online:9999".to_string(),
                "online:9998".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn one_dead_source_does_not_lose_the_others() {
        let h = harness().await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.fail_ports.insert(9998);
            i.traffic
                .insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 7, rx: 0 })]));
        });
        h.xray.with(|i| {
            i.fail_on.insert("query".into());
        });
        let s = sample_once(&h.shared).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 7, rx: 0 });
        assert_eq!(
            s.errors.len(),
            3,
            "住宅的 traffic 与 online 各一条 + Xray 一条：{:?}",
            s.errors
        );
        assert!(s.errors.iter().any(|e| e.contains("9998")));
    }

    #[tokio::test]
    async fn tick_holds_the_delta_in_memory_then_flushes_after_30s() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 5, rx: 5 })]),
            );
        });
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.store.read().await.users[0].usage.total_bytes,
            0,
            "第一轮只进内存"
        );
        assert_eq!(h.shared.pending().await[&id], TxRx { tx: 5, rx: 5 });
        // 面板读的是缓存，缓存里已经有了
        assert_eq!(h.shared.cache().await.stats["alice"], TxRx { tx: 5, rx: 5 });

        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 1, rx: 0 })]),
            );
        });
        h.host.advance(31);
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.store.read().await.users[0].usage.total_bytes,
            11,
            "到点一次性落盘"
        );
        assert!(h.shared.pending().await.is_empty());
        assert_eq!(
            h.shared.cache().await.stats["alice"],
            TxRx { tx: 6, rx: 5 },
            "缓存是累计值"
        );
    }

    #[tokio::test]
    async fn online_is_the_union_of_hysteria_and_recent_xray_traffic() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.xray.with(|i| {
            i.deltas.insert(id.to_string(), TxRx { tx: 1, rx: 0 });
        });
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.shared.cache().await.online["alice"],
            1,
            "只有 Xray 增量也算在线"
        );
        // 30 秒窗口过去、又没有新增量 ⇒ 下线
        h.host.advance(31);
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!h.shared.cache().await.online.contains_key("alice"));
    }

    #[tokio::test]
    async fn exceeding_the_quota_kicks_both_instances_and_blocks_the_snapshot() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
            })
            .await
            .unwrap();
        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 20, rx: 0 })]),
            );
        });
        tick(&ctx, &h.shared).await.unwrap();
        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert!(snap.users["alice"].blocked, "快照拒绝是主保障（spec §4.2）");
        assert!(
            h.hy2.calls().contains(&format!("kick:9999:{id}"))
                && h.hy2.calls().contains(&format!("kick:9998:{id}")),
            "两个实例都要 kick：{:?}",
            h.hy2.calls()
        );
        assert!(h
            .xray
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("remove:vless-direct:{id}"))));
        // 第二轮不重复 kick
        h.hy2.clear_calls();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!h.hy2.calls().iter().any(|c| c.starts_with("kick:")));
    }

    #[tokio::test]
    async fn raising_the_quota_restores_the_user() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
                s.users[0].usage.total_bytes = 50;
            })
            .await
            .unwrap();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(
            crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked
        );
        h.xray.clear_calls();
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(100 * users::GIB);
            })
            .await
            .unwrap();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(
            !crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"]
                .blocked
        );
        assert!(
            h.xray
                .calls()
                .iter()
                .any(|c| c.starts_with(&format!("add:vless-direct:{id}"))),
            "恢复要反向做一次 AddUser：{:?}",
            h.xray.calls()
        );
    }

    #[tokio::test]
    async fn the_health_summary_lands_in_runtime_extra() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.hy2.with(|i| {
            i.fail_ports.insert(9998);
        });
        tick(&ctx, &h.shared).await.unwrap();
        write_health_summary(&ctx, &h.shared).await;
        let v = h.runtime.read().await.extra["users"].clone();
        assert_eq!(v["total"], 1);
        assert_eq!(v["blocked"], 0);
        assert_eq!(v["disabled"], 0);
        assert_eq!(v["month_key"], "2026-09");
        assert_eq!(v["last_sample_at"], "2026-09-11T00:00:00Z");
        assert!(
            v["sample_errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.as_str().unwrap().contains("9998")),
            "采样错误要能在 `/api/users/health` 里看见：{v}"
        );
    }
}
