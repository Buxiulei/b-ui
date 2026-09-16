//! 流量采样、用量累加、限额执行（spec §4.2）。
//!
//! 与 v3 的差别（审计 eff-C1~C4、web-C2~C5）：
//! - 一次 `QueryStats(pattern="user>>>", reset=true)` 拉全量，不再每用户两次 `execSync`（O(N) 阻塞）。
//! - `clear=1` / `reset=true` 让每轮拿到的就是增量，重启守护进程不会把累计值当增量重复计。
//! - 限额真的执行（v3 的 `checkUserLimits` 哪里都没被调用）。
//! - `/api/stats`、`/api/online` 读同一份缓存，面板开着不增加采样。
//!
//! 4.1 起**直连与住宅走两条不同的控制面**（spec §5.1、§5.2）：
//! - **直连一个字不变**：`hysteria-server` 的 trafficStats `:9999` 上打 `/traffic?clear=1`
//!   + `/online`，踢人打 `/kick`（[`stats_ports`] 因此只剩这一个端口）。
//! - **住宅**（自建 sing-box）计量走 v2ray_api 的 `QueryStats`、在线走 Clash API 的
//!   `/connections`，踢人 = 门切 `deny` + 逐条 `DELETE /connections/{id}`，未封用户再把门
//!   切回他自己的槽出站（[`super::Hy2ResiApi`]、[`kick_residential`]）。这两条的返回键是
//!   **凭据 name** 而不是 `user_id`，要经 [`resi_name_to_user`] 换键才能入账。
//!
//! 住宅的计数器同样不跨 sing-box 重启存活（与 apernet 那个内存 trafficStats 计数器
//! 是同一种丢失窗口：重启丢掉「上次采样到重启」这一段），**这里不做任何补偿或降级**。
//!
//! ## 在线数的量纲：**每人 0 / 1**（T14 裁决，第五波复核）
//!
//! 三个来源报回来的东西量纲根本不同：
//! - 直连 hysteria2 的 `/online` 是**会话数**（apernet 按已鉴权会话计，一人多设备 = N）；
//! - 住宅 sing-box 的 Clash `/connections` 是**连接条数**（[`online_of`]，一个只在刷网页
//!   的住宅用户轻易到 30 条）；
//! - Xray 压根没有连接数接口，只能「最近 30 秒有增量就算 1」。
//!
//! 直接相加，面板那个「在线设备」卡就是把会话数、连接条数与常数 1 加在一起 —— 它等于
//! 任何东西。所以 [`apply_sample`] 把每个来源都收成「**这个人现在有没有在线**」：任一
//! 来源 > 0 ⇒ 这个人记 1。于是 `/api/online` 的值恒为 `1`（不在线的人压根不进表）、
//! 面板那个卡是「在线用户数」，全站一个量纲。
//!
//! 为什么不是另一个选项（面板把住宅那一列单独标成「连接数」）：住宅那条路**取不到会话
//! 概念** —— sing-box 的 hysteria2 入站不在 Clash API 里暴露会话，只有连接；两列不同量纲
//! 的数字也没法喂给同一个汇总卡。踢人（`on ? 断开按钮 : 无`）与限额判定都只看「在不在线」
//! 这个布尔，一个都不需要基数。**代价说清**：直连用户的多设备数不再显示（只显示「在线」），
//! 要看会话基数请打 `/online`（`bui` 不再转述它）。

use super::hy2resi::online_of;
use super::users::{self, month_key};
use super::{Shared, TxRx, HY2_STATS_PORT_DIRECT};
use crate::reconcile::DaemonCtx;
use bui_schema::model::State;
use bui_schema::render::hy2_singbox::{gate_tag, slot_out_tag, DENY_TAG};
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

/// 要采样 / kick 的 hysteria trafficStats 端口：**只有直连实例那一个**。
///
/// 4.1 起住宅是一个 sing-box 入站（计量走 v2ray_api、踢人走 Clash API），apernet 的
/// `trafficStats` 端口连带槽位换算一起消失；签名保留 `&State` 是为了让调用点不动。
pub fn stats_ports(_s: &State) -> Vec<u16> {
    vec![HY2_STATS_PORT_DIRECT]
}

/// 凭据 `name` → `user_id`：住宅两条控制面（`QueryStats` 的计数器名、`/connections` 的
/// `auth_user=`）都以凭据 name 为键，采样与在线共用这张表换键。
///
/// 空闲凭据与已删用户的凭据不在表里 ⇒ 它们的增量与连接一律丢弃（口径同
/// [`to_uuid_map`]：认不出的键不入账，绝不猜）。
pub fn resi_name_to_user(s: &State) -> BTreeMap<String, Uuid> {
    s.users
        .iter()
        .filter_map(|u| {
            bui_schema::hy2pool::cred_of(u, &s.residential).map(|c| (c.name.clone(), u.user_id))
        })
        .collect()
}

/// 住宅侧踢一个人要的东西：门的 tag 由凭据 `id` 算、连接归组按凭据 `name`。
/// **不带 `secret`** —— 踢人这条路一个凭据字节都不需要，也就不会被日志带出去。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResiKickTarget {
    pub cred_id: String,
    pub name: String,
    /// 踢完把门 PUT 回这里：`Some("slot-<i>-out")` = 未封用户（spec §5.2 的
    /// 「未封者下一请求即通」）；`None` = 他本来就该停在 `deny`（限额封禁 / 到期 /
    /// 禁用 / 没有住宅 hysteria2 权益）。
    pub restore_to: Option<String>,
}

/// 这些用户在住宅侧的踢人目标；没有凭据的用户（没住宅权益、或池还没分过）不在表里。
///
/// `open` = 这些人里「踢完还该放行」的那些（手动踢人时 = 没被 `users::blocked_set` 判拒
/// 的），他们的 [`ResiKickTarget::restore_to`] 是自己槽的出站 tag；限额封禁那条路传空集
/// ⇒ 全部停在 `deny`。撤掉了住宅 hysteria2 权益的用户即使在 `open` 里也不回切 —— 他的门
/// 本来就该是 `deny`，踢一下不许把它开回去。
pub fn resi_kick_targets(s: &State, ids: &[Uuid], open: &BTreeSet<Uuid>) -> Vec<ResiKickTarget> {
    ids.iter()
        .filter_map(|id| {
            let u = s.users.iter().find(|u| u.user_id == *id)?;
            let c = bui_schema::hy2pool::cred_of(u, &s.residential)?;
            let restore_to = (open.contains(id) && super::gates::has_resi_hy2(u, &s.residential))
                .then(|| slot_out_tag(bui_schema::slots::index_of_user(u, &s.residential)));
            Some(ResiKickTarget {
                cred_id: c.id.clone(),
                name: c.name.clone(),
                restore_to,
            })
        })
        .collect()
}

/// 住宅侧的「踢下线」（spec §5.2）：门切 `deny` —— 门是 `interrupt_exist_connections`
/// 的 selector，切过去存量流当场断 —— 再逐条 `DELETE /connections/{id}` 兜底，最后把
/// [`ResiKickTarget::restore_to`] 非空的（未封）用户的门 PUT 回他自己的槽出站。返回关掉
/// 的连接条数。
///
/// 三步都是 best-effort（失败只记 warn）：快照拒绝与门位收敛是主保障，踢不动只是旧会话
/// 多活一会儿。门位在这里切成 `deny` 与后续的门位收敛同向，重复切是幂等的。
///
/// **回切不能省**：限额封禁那条路的人本来就该停在 `deny`，但面板「断开」按钮踢的多数是
/// 正常用户 —— 少了这一次 PUT，他会一直断到下一次门位收敛（最长 60 秒的安全网），而
/// spec §5.2 的判据是「未封者下一请求即通」。
pub async fn kick_residential(shared: &Shared, targets: &[ResiKickTarget]) -> usize {
    if targets.is_empty() {
        return 0;
    }
    for t in targets {
        let gate = gate_tag(&t.cred_id);
        if let Err(e) = shared.hy2resi().select(&gate, DENY_TAG).await {
            tracing::warn!(gate = %gate, error = %e, "住宅 HY2 门切 deny 失败；快照拒绝仍然生效");
        }
    }
    let closed = close_conns_of(shared, targets).await;
    for t in targets {
        let Some(slot) = t.restore_to.as_deref() else {
            continue;
        };
        let gate = gate_tag(&t.cred_id);
        if let Err(e) = shared.hy2resi().select(&gate, slot).await {
            tracing::warn!(gate = %gate, error = %e, "住宅 HY2 门切回槽出站失败；门位收敛会补上");
        }
    }
    closed
}

/// `kick_residential` 的逐条 DELETE 兜底：只关归到这些凭据 `name` 的连接。
async fn close_conns_of(shared: &Shared, targets: &[ResiKickTarget]) -> usize {
    let wanted: BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    let conns = match shared.hy2resi().connections().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "住宅 HY2 /connections 读不到；跳过逐条 DELETE");
            return 0;
        }
    };
    let mut closed = 0;
    for c in conns {
        let Some(name) = super::hy2resi::user_of_rule(&c.rule) else {
            continue;
        };
        if !wanted.contains(name) {
            continue;
        }
        match shared.hy2resi().close_connection(&c.id).await {
            Ok(()) => closed += 1,
            Err(e) => tracing::warn!(error = %e, "住宅 HY2 关连接失败"),
        }
    }
    closed
}

/// 打直连 hysteria 的 `/traffic?clear=1` 与 `/online`，再打住宅 sing-box 的
/// `QueryStats(reset=true)` 与 `/connections`，最后打一次 Xray 的 `QueryStats(reset=true)`。
///
/// 单个来源失败只记 error，不影响其余来源（spec §4.2）。`resi_names` 是
/// [`resi_name_to_user`] 的结果：住宅那两条返回的是凭据 name，认不出的键丢弃。
pub async fn sample_once(
    shared: &Shared,
    ports: &[u16],
    resi_names: &BTreeMap<String, Uuid>,
) -> Sample {
    let mut s = Sample::default();
    // 顺序固定（直连 traffic 按 ports 升序 → 直连 online 同序 → 住宅 QueryStats →
    // 住宅 /connections → Xray），测试按这个顺序断言 calls()
    for port in ports.iter().copied() {
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
    for port in ports.iter().copied() {
        match shared.hy2().online(port).await {
            Ok(m) => {
                for (id, n) in m {
                    *s.online.entry(id).or_insert(0) += n;
                }
            }
            Err(e) => s.errors.push(format!("hysteria :{port} /online 失败：{e}")),
        }
    }
    // 住宅：v2ray_api 的 `QueryStats`（`reset=true` ⇒ 拿到的就是增量）+ Clash API 的
    // `/connections`（在线数 = 按 `auth_user=` 归组的连接条数）。两条的键都是凭据 name。
    match shared.hy2resi().query_user_deltas().await {
        Ok(m) => {
            for (name, d) in m {
                if let Some(uid) = resi_names.get(&name) {
                    s.deltas.entry(uid.to_string()).or_default().add(d);
                }
            }
        }
        Err(e) => s.errors.push(format!("住宅 HY2 QueryStats 失败：{e}")),
    }
    match shared.hy2resi().connections().await {
        Ok(conns) => {
            for (name, n) in online_of(&conns) {
                if let Some(uid) = resi_names.get(&name) {
                    *s.online.entry(uid.to_string()).or_insert(0) += n;
                }
            }
        }
        Err(e) => s.errors.push(format!("住宅 HY2 /connections 失败：{e}")),
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
/// `store.update` 里全部并进 `usage`；**只有 `update` 成功才算清空**，失败就把取走的增量
/// 合并回 `pending`，下一轮重试。限额判定用 `usage + pending`
/// （`users::is_blocked` 的 `extra` 参数），所以「还没落盘」不会让超限用户多跑 30 秒。
///
/// 已知降级：`pending` 只在内存里，`systemctl restart b-ui` / SIGTERM 最多丢 30 秒的计数
/// （`SampleCache` 与 `Applied` 同样要下一轮重建，两者都是幂等的）。
pub async fn tick(ctx: &DaemonCtx, shared: &Shared) -> anyhow::Result<()> {
    let now = ctx.host.now();
    // 一次读出这一轮要用的两张表：直连的端口集（只有 `:9999`）与住宅的「凭据 name → user_id」
    let (ports, resi_names) = {
        let state = ctx.store.read().await;
        (
            stats_ports(state.as_ref()),
            resi_name_to_user(state.as_ref()),
        )
    };
    let sample = sample_once(shared, &ports, &resi_names).await;
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
        let taken = std::mem::take(&mut *shared.pending().await);
        if let Err(e) = ctx
            .store
            .update(|s| {
                apply_sample(s, &taken, now);
            })
            .await
        {
            // 落盘失败（磁盘满、备份目录写不动…）不能把本轮取走的增量丢掉：按用户键累加回
            // `pending`，下轮再试。`last_flush_at` 也没被推进（下面那一段在 `?` 之后），
            // 所以下一轮仍然判「到点该落盘了」。
            let mut pending = shared.pending().await;
            for (id, d) in &taken {
                pending.entry(*id).or_default().add(*d);
            }
            return Err(e);
        }
    }

    // ④ 限额执行与恢复（快照拒绝 + Xray 增删 + kick）
    let out = users::sync_now(ctx, shared).await;
    if !out.newly_blocked.is_empty() {
        let ids: Vec<String> = out.newly_blocked.iter().map(Uuid::to_string).collect();
        for port in ports.iter().copied() {
            if let Err(e) = shared.hy2().kick(port, &ids).await {
                tracing::warn!(port, error = %e, "kick 失败；快照拒绝仍然生效");
            }
        }
        // 住宅那条路没有 `/kick`：门切 `deny` + 逐条 DELETE（spec §5.2）。
        // 回切集合是**空的**：这一路踢的全是刚被判拒的人，他们就该停在 `deny`。
        let targets = resi_kick_targets(
            ctx.store.read().await.as_ref(),
            &out.newly_blocked,
            &BTreeSet::new(),
        );
        kick_residential(shared, &targets).await;
    }

    // ⑤ 刷新面板缓存
    let state = ctx.store.read().await;
    let mut online: BTreeMap<Uuid, u32> = BTreeMap::new();
    for (id, n) in &sample.online {
        if let Ok(uid) = Uuid::parse_str(id) {
            *online.entry(uid).or_insert(0) += *n;
        }
    }
    // Xray 没有连接数接口：最近 30 秒有增量就算在线（spec §4.2 的并集）
    for uid in shared.xray_seen().await.keys() {
        online.entry(*uid).or_insert(1);
    }
    // **归一成每人 0 / 1**（见模块文档「在线数的量纲」）：上面三个来源分别是直连会话数、
    // 住宅连接条数、Xray 的常数 1，相加等于任何东西。收成布尔就一个量纲，面板那个卡是
    // 「在线用户数」。**这两行是唯一的归一点** —— 删了它就回到 4.0.x 那个混量纲的和。
    online.retain(|_, n| *n > 0);
    online.values_mut().for_each(|n| *n = 1);
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

    /// 给 state 建住宅凭据池（迁移口径：alice 拿 `r000`，凭据 `name` = 用户名）。
    /// 住宅那两条控制面返回的键就是这个 `name`，采样要能换回 `user_id`。
    async fn with_pool(h: &Harness) {
        h.store
            .update(|s| {
                bui_schema::hy2pool::migrate(s, t0());
            })
            .await
            .unwrap();
    }

    /// 当前期望态里的「采样端口 + 凭据 name → user_id」——`tick` 里那一读的测试版。
    async fn sample_args(h: &Harness) -> (Vec<u16>, BTreeMap<String, Uuid>) {
        let st = h.store.read().await;
        (stats_ports(st.as_ref()), resi_name_to_user(st.as_ref()))
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

    /// 直连**一个字不变**（仍是 `/traffic?clear=1` + `/online`，只剩 `:9999` 一个端口）；
    /// 住宅改走自建 sing-box 的两个回环面：计量 v2ray_api 的 `QueryStats`、在线
    /// Clash API 的 `/connections`。住宅那两条的返回键是**凭据 name**，必须经凭据池
    /// 换成 `user_id` 才能入账（spec §5.1、§5.2）。
    #[tokio::test]
    async fn sampling_reads_the_direct_instance_over_http_and_the_residential_one_over_grpc() {
        let h = harness().await;
        with_pool(&h).await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.traffic
                .insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 10, rx: 0 })]));
            i.online.insert(9999, BTreeMap::from([(id.clone(), 1u32)]));
        });
        h.hy2resi.set_deltas(BTreeMap::from([(
            "alice".to_string(),
            TxRx { tx: 0, rx: 7 },
        )]));
        h.hy2resi.set_conns(vec![
            ("c1", "auth_user=alice => route(gate-r000)"),
            ("c2", "auth_user=alice => route(gate-r000)"),
            // 空闲凭据上的连接（生产上不该有）归不到任何用户 ⇒ 不入账、也不报错
            ("c9", "auth_user=r001 => route(gate-r001)"),
        ]);
        h.xray.with(|i| {
            i.deltas.insert(id.clone(), TxRx { tx: 0, rx: 100 });
        });

        let (ports, names) = sample_args(&h).await;
        assert_eq!(ports, vec![9999], "住宅实例不再有 trafficStats 端口");
        assert_eq!(names["alice"].to_string(), id, "凭据 name → user_id");
        let s = sample_once(&h.shared, &ports, &names).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 10, rx: 107 }, "三个来源相加");
        assert_eq!(
            s.online[&id], 3,
            "直连 1 条 + 住宅 /connections 归到他的 2 条"
        );
        assert!(s.xray_ids.contains(&id));
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(
            h.hy2.calls(),
            vec!["traffic:9999".to_string(), "online:9999".to_string()],
            "直连仍是那两个调用，且只打 :9999"
        );
        assert_eq!(
            h.hy2resi.calls(),
            vec!["query".to_string(), "connections".to_string()],
            "住宅只有 QueryStats 与 /connections 两次调用"
        );
    }

    /// 住宅那条 gRPC 挂了只记一条 error，**它前后两边的采样都不能丢一个字节**
    /// （spec §4.2 既有口径）：前面是直连的 `/traffic`，后面是同一轮的住宅
    /// `/connections` 与 Xray 的 `QueryStats`。
    ///
    /// 「后面」这半是本用例的守门点：把住宅 `QueryStats` 的 `Err` 分支改成整轮
    /// 提前返回（`?` 化重构最容易写成这样），Reality 的流量与住宅在线数会连带丢掉，
    /// 而 T8 落地前住宅那个面在每台机上都是不可达的 ⇒ 每一轮都丢。
    #[tokio::test]
    async fn a_grpc_failure_does_not_lose_the_samples_before_or_after_it() {
        let h = harness().await;
        with_pool(&h).await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.traffic
                .insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 5, rx: 0 })]));
        });
        // 住宅的 QueryStats（住宅那两条里的第一条）失败一次；它之后的 `/connections`
        // 与 Xray 照常应答
        h.hy2resi.fail_next("v2ray_api 不可达");
        h.hy2resi
            .set_conns(vec![("c1", "auth_user=alice => route(gate-r000)")]);
        h.xray.with(|i| {
            i.deltas.insert(id.clone(), TxRx { tx: 0, rx: 9 });
        });
        let (_, names) = sample_args(&h).await;
        let s = sample_once(&h.shared, &[9999], &names).await;
        assert_eq!(s.deltas.len(), 1);
        assert_eq!(
            s.deltas[&id],
            TxRx { tx: 5, rx: 9 },
            "它之前的直连 5 字节 + 它之后的 Xray 9 字节都要在"
        );
        assert!(s.xray_ids.contains(&id), "Xray 的在线窗口也不能丢");
        assert_eq!(s.online[&id], 1, "它之后的住宅 /connections 照样归组");
        assert_eq!(s.errors.len(), 1, "{:?}", s.errors);
        assert!(s.errors[0].contains("住宅 HY2"), "{:?}", s.errors);
        assert!(
            h.hy2resi.calls().contains(&"connections".to_string()),
            "住宅计量挂了不许跳过同轮的 /connections：{:?}",
            h.hy2resi.calls()
        );
    }

    /// T14 的量纲判据（第五波复核）：三个来源（直连会话数 / 住宅连接条数 / Xray「有增量」）
    /// 同时报回来，面板缓存里这个人仍然只是 **1 个在线**。
    ///
    /// 4.0.x 是三者相加：一个只在刷网页、住宅开了 30 条连接的用户会显示成「30 在线」，
    /// 而面板顶上那个「在线设备」卡把全表这种数加起来 —— 那个数不代表任何东西。
    /// 原始采样（[`Sample::online`]）照旧是各来源的原值，归一只在 [`apply_sample`] 里。
    #[tokio::test]
    async fn the_panel_counts_one_online_user_not_the_sum_of_three_dimensions() {
        let h = harness().await;
        with_pool(&h).await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id.to_string();
        // 直连：3 个会话
        h.hy2.with(|i| {
            i.online.insert(9999, BTreeMap::from([(id.clone(), 3u32)]));
        });
        // 住宅：30 条连接
        let conns: Vec<(String, String)> = (0..30)
            .map(|n| {
                (
                    format!("c{n}"),
                    "auth_user=alice => route(gate-r000)".to_string(),
                )
            })
            .collect();
        h.hy2resi.set_conns(
            conns
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect(),
        );
        // **先不给 Xray 流量**：Xray 那一档写的是常数 1，有它在就会把「直连会话数 + 住宅
        // 连接条数」这个混量纲的和掩盖掉（第一版用例就是被它掩盖的，变异验证抓出来的）
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.shared.cache().await.online.get("alice").copied(),
            Some(1),
            "3 个会话 + 30 条连接 = 1 个在线用户（4.0.x 这里是 33）"
        );
        // 再加上 Xray 的那一档，还是 1
        h.xray.with(|i| {
            i.deltas.insert(id.clone(), TxRx { tx: 1, rx: 1 });
        });
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.shared.cache().await.online.get("alice").copied(),
            Some(1),
            "3 个会话 + 30 条连接 + Xray 有量 = 1 个在线用户"
        );

        // 全部来源归零 ⇒ 这个人压根不进表（前端按 falsy 显示「离线」、不给断开按钮）
        h.hy2.with(|i| {
            i.online.insert(9999, BTreeMap::new());
        });
        h.hy2resi.set_conns(vec![]);
        h.xray.with(|i| i.deltas.clear());
        h.shared.xray_seen().await.clear();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(
            !h.shared.cache().await.online.contains_key("alice"),
            "不在线的人不进表：{:?}",
            h.shared.cache().await.online
        );
    }

    /// 住宅的字节与在线数要真的落进**生产路径**：`tick` 自己那一次
    /// `resi_name_to_user` 读出的换键表。
    ///
    /// 三条 `sample_once` 用例都自带换键表（`sample_args` 是 `tick` 那一读的复制品），
    /// 所以 `tick` 里那一段是零覆盖：把它换成空表，住宅流量与在线会被整条丢掉（用户
    /// 跑住宅永不计费、超限永不触发），而全套用例照旧全绿。本用例就是钉这一段。
    #[tokio::test]
    async fn tick_books_the_residential_bytes_and_connections_with_its_own_name_table() {
        let h = harness().await;
        with_pool(&h).await;
        let ctx = ctx_of(&h);
        // 住宅是**唯一**的来源：直连与 Xray 一个字节都不给，落盘的数只可能来自住宅
        h.hy2resi.set_deltas(BTreeMap::from([(
            "alice".to_string(),
            TxRx { tx: 3, rx: 4 },
        )]));
        h.hy2resi.set_conns(vec![
            ("c1", "auth_user=alice => route(gate-r000)"),
            ("c2", "auth_user=alice => route(gate-r000)"),
        ]);
        tick(&ctx, &h.shared).await.unwrap();
        let id = h.store.read().await.users[0].user_id;
        assert_eq!(
            h.shared.pending().await.get(&id).copied(),
            Some(TxRx { tx: 3, rx: 4 }),
            "住宅增量经 tick 的换键表进了内存账"
        );
        assert_eq!(
            h.shared.cache().await.stats.get("alice").copied(),
            Some(TxRx { tx: 3, rx: 4 }),
            "面板缓存里也是这笔"
        );
        assert_eq!(
            h.shared.cache().await.online.get("alice").copied(),
            Some(1),
            "面板缓存的在线是**每人 0/1**：住宅那两条连接归到他，他就是 1 个在线用户，\
             不是 2（量纲见模块文档）"
        );
        assert!(h.shared.cache().await.errors.is_empty());

        // 第二轮到点落盘：两轮的住宅字节一起进 `usage`
        h.hy2resi.set_deltas(BTreeMap::from([(
            "alice".to_string(),
            TxRx { tx: 0, rx: 5 },
        )]));
        h.host.advance(31);
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.store.read().await.users[0].usage.total_bytes,
            12,
            "3 + 4 + 5 全部落进期望态（住宅计量的唯一生产路径）"
        );
        assert!(h.shared.pending().await.is_empty());
    }

    #[tokio::test]
    async fn one_dead_source_does_not_lose_the_others() {
        let h = harness().await;
        with_pool(&h).await;
        let id = h.store.read().await.users[0].user_id.to_string();
        // 直连的 /traffic 与 /online 双双失败、住宅的 /connections 失败、Xray 失败，
        // 只剩住宅的 QueryStats 活着 —— 它那 7 字节必须照旧入账。
        h.hy2.with(|i| {
            i.fail_ports.insert(9999);
        });
        h.hy2resi.set_deltas(BTreeMap::from([(
            "alice".to_string(),
            TxRx { tx: 0, rx: 7 },
        )]));
        h.hy2resi.with(|i| {
            i.fail_on.insert("connections".into());
        });
        h.xray.with(|i| {
            i.fail_on.insert("query".into());
        });
        let (_, names) = sample_args(&h).await;
        let s = sample_once(&h.shared, &[9999], &names).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 0, rx: 7 });
        assert_eq!(
            s.errors.len(),
            4,
            "直连 traffic / online + 住宅 /connections + Xray 各一条：{:?}",
            s.errors
        );
        assert_eq!(
            s.errors.iter().filter(|e| e.contains("住宅 HY2")).count(),
            1,
            "住宅那条错误要认得出是住宅的：{:?}",
            s.errors
        );
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

    /// 终审收尾：落盘失败不许把本轮取走的增量丢掉。
    ///
    /// 制造失败的办法：把 `state.backups` 变成一个**普通文件** ⇒ `Store::update` 写盘前的
    /// 「备份上一版」那步 `create_dir_all` 必然失败（与 uid 无关，root 下也一样失败）。
    #[tokio::test]
    async fn a_failed_flush_keeps_the_delta_and_the_next_round_lands_the_sum() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 5, rx: 0 })]),
            );
        });
        // 第一轮只进内存，同时把 `last_flush_at` 设成当轮时间
        tick(&ctx, &h.shared).await.unwrap();

        let blocker = crate::paths::backups_dir(&h.paths);
        std::fs::write(&blocker, b"not a directory").unwrap();
        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 2, rx: 0 })]),
            );
        });
        h.host.advance(31);
        assert!(
            tick(&ctx, &h.shared).await.is_err(),
            "落盘失败要报上去（sampling_loop 记 warn 后继续）"
        );
        assert_eq!(
            h.shared.pending().await[&id],
            TxRx { tx: 7, rx: 0 },
            "取走的增量必须合并回 pending：5 + 2"
        );
        assert_eq!(
            h.store.read().await.users[0].usage.total_bytes,
            0,
            "失败的那一轮一个字节都没落盘"
        );

        std::fs::remove_file(&blocker).unwrap();
        h.hy2.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([(id.to_string(), TxRx { tx: 1, rx: 0 })]),
            );
        });
        h.host.advance(1);
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(
            h.store.read().await.users[0].usage.total_bytes,
            8,
            "5 + 2 + 1：重试那轮把三轮的增量一次落全"
        );
        assert!(h.shared.pending().await.is_empty());
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

    /// 限额触发的踢人：直连仍打 `:9999` 的 `/kick`；住宅没有这个接口，改成
    /// **门切 `deny`**（`interrupt_exist_connections` 当场断掉存量流）+ 逐条
    /// `DELETE /connections/{id}` 兜底（spec §5.2）。
    #[tokio::test]
    async fn exceeding_the_quota_kicks_the_direct_port_and_denies_the_residential_gate() {
        let h = harness().await;
        with_pool(&h).await;
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
        h.hy2resi.set_conns(vec![
            ("c1", "auth_user=alice => route(gate-r000)"),
            ("c9", "auth_user=r001 => route(gate-r001)"),
        ]);
        tick(&ctx, &h.shared).await.unwrap();
        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert!(snap.users["alice"].blocked, "快照拒绝是主保障（spec §4.2）");
        assert!(
            h.hy2.calls().contains(&format!("kick:9999:{id}")),
            "{:?}",
            h.hy2.calls()
        );
        assert!(
            !h.hy2.calls().iter().any(|c| c.starts_with("kick:9998")),
            "住宅不再有 trafficStats 端口：{:?}",
            h.hy2.calls()
        );
        let calls = h.hy2resi.calls();
        assert!(
            calls.contains(&"select:gate-r000:deny".to_string()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"close:c1".to_string()),
            "逐条 DELETE 兜底：{calls:?}"
        );
        assert!(
            !calls.contains(&"close:c9".to_string()),
            "别人的连接一条都不许动：{calls:?}"
        );
        // 第二轮不重复 kick（`newly_blocked` 是差分）
        h.hy2.clear_calls();
        h.hy2resi.clear_calls();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!h.hy2.calls().iter().any(|c| c.starts_with("kick:")));
        let calls = h.hy2resi.calls();
        assert!(!calls.iter().any(|c| c.starts_with("select:")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("close:")), "{calls:?}");
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
        h.hy2resi.fail_next("v2ray_api 不可达");
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
                .any(|e| e.as_str().unwrap().contains("住宅 HY2")),
            "采样错误要能在 `/api/users/health` 里看见：{v}"
        );
    }

    /// 住宅的计量从 trafficStats 搬走之后，采样端口表就只剩直连那一个 ——
    /// **增删槽位不再影响它**（spec §5.1）。
    #[test]
    fn stats_ports_is_only_the_direct_instance() {
        let mut s = crate::testutil::sample_state();
        assert_eq!(stats_ports(&s), vec![9999]);
        s.residential.slots = (0..3)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        assert_eq!(
            stats_ports(&s),
            vec![9999],
            "住宅槽位不再带 trafficStats 端口"
        );
    }

    /// 住宅两条控制面的返回键是**凭据 name**（迁移用户 = 用户名、新发 = 凭据 id），
    /// 采样与在线都得经这张表换成 `user_id`；空闲凭据不属于任何人。
    #[test]
    fn residential_keys_are_credential_names_and_map_back_to_user_ids() {
        let mut s = crate::testutil::sample_state();
        assert!(
            resi_name_to_user(&s).is_empty(),
            "还没分过凭据 ⇒ 表是空的（住宅那条路的键一个都认不出）"
        );
        bui_schema::hy2pool::migrate(&mut s, t0());
        let id = s.users[0].user_id;
        let m = resi_name_to_user(&s);
        assert_eq!(
            m.get("alice").copied(),
            Some(id),
            "迁移用户的 name = 用户名"
        );
        assert_eq!(m.len(), 1, "空闲凭据不属于任何用户：{m:?}");
        assert!(!m.contains_key("r001"));
        // 新发凭据的 name = 凭据 id：键必须跟着凭据走，不是跟着用户名走
        let cred = s.residential.hy2_pool.creds[1].clone();
        assert_eq!(cred.name, cred.id);
        s.users[0].credentials.hy2_resi_cred = Some(cred.id.clone());
        let m = resi_name_to_user(&s);
        assert_eq!(m.get(cred.name.as_str()).copied(), Some(id));
        assert!(!m.contains_key("alice"));
    }

    #[test]
    fn a_user_without_a_credential_has_no_gate_to_deny() {
        let mut s = crate::testutil::sample_state();
        let id = s.users[0].user_id;
        let none = BTreeSet::new();
        assert!(
            resi_kick_targets(&s, &[id], &none).is_empty(),
            "没分过凭据 ⇒ 没有门、也没有连接要关"
        );
        bui_schema::hy2pool::migrate(&mut s, t0());
        assert_eq!(
            resi_kick_targets(&s, &[id], &none),
            vec![ResiKickTarget {
                cred_id: "r000".to_string(),
                name: "alice".to_string(),
                restore_to: None,
            }],
            "不在回切集合里 ⇒ 踢完停在 deny（限额封禁那条路）"
        );
        assert!(
            resi_kick_targets(&s, &[uuid::Uuid::nil()], &none).is_empty(),
            "认不出的 user_id 不许拼出门 tag"
        );
    }

    /// 手动踢人的后半截（spec §5.2）：未封用户的目标要带回切 tag = 他自己那个槽的出站。
    #[test]
    fn an_unblocked_kick_target_carries_its_slot_outbound_to_restore() {
        let mut s = crate::testutil::sample_state();
        bui_schema::hy2pool::migrate(&mut s, t0());
        let id = s.users[0].user_id;
        let open = BTreeSet::from([id]);
        assert_eq!(
            resi_kick_targets(&s, &[id], &open)[0].restore_to.as_deref(),
            Some("slot-0-out"),
            "未封用户踢完要回到自己的槽出站"
        );
        // 槽 3 上的用户回切到 slot-3-out（回切 tag 跟着他粘的那个 IP 走）
        let up = uuid::Uuid::from_u128(7);
        s.residential.slots = vec![bui_schema::model::Slot {
            index: 3,
            upstream_id: up,
        }];
        s.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(up);
        assert_eq!(
            resi_kick_targets(&s, &[id], &open)[0].restore_to.as_deref(),
            Some("slot-3-out")
        );
        // 住宅权益被撤掉（凭据还没被回收）⇒ 门本来就该是 deny，踢一下不许把它开回去
        s.users[0].entitlements.residential = None;
        assert_eq!(
            resi_kick_targets(&s, &[id], &open)[0].restore_to,
            None,
            "没有住宅 hysteria2 权益的人不回切"
        );
    }

    /// 没有连接的被封用户也要把门切 `deny`（新握手仍会成功、流必须被拒，spec §6）。
    #[tokio::test]
    async fn denying_a_gate_happens_even_with_no_live_connection() {
        let h = harness().await;
        with_pool(&h).await;
        let targets = {
            let st = h.store.read().await;
            let id = st.users[0].user_id;
            resi_kick_targets(st.as_ref(), &[id], &BTreeSet::new())
        };
        let closed = kick_residential(&h.shared, &targets).await;
        assert_eq!(closed, 0, "一条连接都没有");
        assert_eq!(
            h.hy2resi.selected().get("gate-r000").map(String::as_str),
            Some("deny"),
            "门位必须落在 deny"
        );
    }

    /// 未封用户被踢：`deny` 掐断存量流 → 逐条 DELETE → **再切回他的槽出站**
    /// （spec §5.2 的判据是「未封者下一请求即通」，不能等下一次门位收敛）。
    /// `/connections` 读不到时回切也不许被跳过 —— 否则他会一直断着。
    #[tokio::test]
    async fn kicking_an_unblocked_user_puts_his_gate_back_on_his_slot() {
        let h = harness().await;
        with_pool(&h).await;
        let targets = {
            let st = h.store.read().await;
            let id = st.users[0].user_id;
            resi_kick_targets(st.as_ref(), &[id], &BTreeSet::from([id]))
        };
        h.hy2resi
            .set_conns(vec![("c1", "auth_user=alice => route(gate-r000)")]);
        assert_eq!(kick_residential(&h.shared, &targets).await, 1);
        assert_eq!(
            h.hy2resi.calls(),
            vec![
                "select:gate-r000:deny".to_string(),
                "connections".to_string(),
                "close:c1".to_string(),
                "select:gate-r000:slot-0-out".to_string(),
            ],
            "顺序是「切 deny → 关连接 → 回切」"
        );
        assert_eq!(
            h.hy2resi.selected().get("gate-r000").map(String::as_str),
            Some("slot-0-out"),
            "门位最终停在他自己的槽出站"
        );

        // `/connections` 挂了也要回切（早返回会让被踢的正常用户一直断到门位收敛）
        h.hy2resi.clear_calls();
        h.hy2resi.set_selected(BTreeMap::new());
        h.hy2resi.with(|i| {
            i.fail_on.insert("connections".into());
        });
        assert_eq!(kick_residential(&h.shared, &targets).await, 0);
        assert_eq!(
            h.hy2resi.selected().get("gate-r000").map(String::as_str),
            Some("slot-0-out"),
            "{:?}",
            h.hy2resi.calls()
        );
    }
}
