//! `bui` 侧的槽位入口（spec §5.6）：把 `bui_schema::slots` 的纯函数包进一次
//! `Store::update_as`，顺带同步槽位表、重分配孤儿用户，再发一次
//! `Event::StateChanged("residential")` 交给 P1 的去抖对账重渲染。
//!
//! 本模块**不渲染任何内核配置**（与 `residential/mod.rs` 同一条边界）：
//! `config-residential[-<i>].yaml`、`singbox-relay.json`、`xray-config.json` 都由
//! `crate::modules::core_files` / `units` 从期望态渲染。
use crate::api::{Event, EventBus};
use crate::modules::panel::XrayApi;
use crate::modules::residential::state;
use crate::reconcile::DaemonCtx;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use bui_schema::model::{ResidentialGroup, Slot, State, DEFAULT_GROUP};
use bui_schema::render::xray as xray_render;
use bui_schema::render::xray::SlotRule;
use bui_schema::slots;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// 一次带槽位同步的写入之后，槽位表与用户分配发生了什么。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotSync {
    /// 本次被释放的槽位（它们的上游已不在池里）
    pub released: Vec<Slot>,
    /// 因此被重新分配的用户数
    pub reassigned: usize,
}

/// **改住宅池并同步槽位的唯一入口**：在同一次 `Store::update_as` 里改组、同步槽位表、
/// 重分配孤儿用户，写成功后发 `Event::StateChanged("residential")`。
///
/// `caller` 透传给 `Store::update_as` 的拒写防线（只有
/// [`crate::state::store::CALLER_RESI_REMOVE`] 能让上游池变短，R3 ①）。
/// 只改模式 / 关键字 / 优先级这类**不动池成员**的写入继续用
/// `state::update_group`，不必经过这里。
pub async fn update_group_slots_as(
    store: &Store,
    bus: &EventBus,
    caller: &'static str,
    f: impl FnOnce(&mut ResidentialGroup),
) -> anyhow::Result<SlotSync> {
    let mut sync = SlotSync::default();
    let out = &mut sync;
    store
        .update_as(caller, |s| {
            let g = s
                .residential
                .groups
                .entry(DEFAULT_GROUP.to_string())
                .or_default();
            f(g);
            out.released = slots::sync_slots(&mut s.residential);
            out.reassigned = slots::migrate_unassigned(s);
        })
        .await?;
    bus.send(Event::StateChanged("residential"));
    Ok(sync)
}

/// 新建用户时分槽（spec §5.6 规则 1）。**在 `Store::update` 的闭包里调**，
/// 与 `users.push(...)` 同一次写盘 —— 否则会出现「用户已存在但没有槽位」的中间态，
/// 那一瞬间他的 Reality 住宅会落到兜底槽。
pub fn assign_new_user(s: &mut State, user_id: Uuid) -> bool {
    slots::assign_least_loaded(s, user_id)
}

/// 守护进程启动时跑一次槽位迁移（spec §5.6 规则 4）：
/// 旧 `state.json` 没有 `slots` 字段就补齐，既有住宅用户按创建时间轮流落槽。
/// 返回被分配的用户数；**幂等**，所以每次启动都可以无条件调用（零变更不写盘）。
pub async fn migrate_on_start(store: &Store, bus: &EventBus) -> anyhow::Result<usize> {
    let mut assigned = 0usize;
    let n = &mut assigned;
    store
        .update(|s| {
            slots::sync_slots(&mut s.residential);
            *n = slots::migrate_unassigned(s);
        })
        .await?;
    if assigned > 0 {
        tracing::info!(
            users = assigned,
            "按创建时间把既有住宅用户轮流落槽（spec §5.6 规则 4）"
        );
        bus.send(Event::StateChanged("residential"));
    }
    Ok(assigned)
}

/// [`converge_xray`] 这一轮做了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergeOutcome {
    /// 没脏、或脏但哈希已一致（清脏、不动 xray）
    Clean,
    /// 走 gRPC 增删收敛成功，带发出的 `AddRule`/`RemoveRule` 条数
    Applied(usize),
    /// gRPC 失败、新配置已落盘 ⇒ 退回了一次 `systemctl restart xray`
    Restarted,
    /// 什么都没做，脏标记留着等下一轮
    Deferred,
}

/// 标记「渲染出的 Xray 槽路由与 xray 进程里跑的那一份可能已经不一致」（D7）。
/// 用户增删、改分槽、`rebalance`、删上游后的重分配之后都要置位。
pub async fn mark_xray_rules_dirty(runtime: &Runtime) {
    state::update(runtime, |r| r.xray_slot_rules_dirty = true).await;
}

/// 让 Xray 的住宅槽路由与期望态一致（D7）。**正常路径不重启 xray**。
///
/// 四道门（顺序即语义，别调整）：
/// 1. `runtime.residential.xray_slot_rules_dirty` 没置位 ⇒ 什么都不做；
/// 2. 置位但 `xray_slot_rules_hash` 已等于当前渲染的 `slot_rules_hash` ⇒ 清脏、不动 xray
///    （这一轮的变化与槽路由无关，例如加了个没有住宅权益的用户）；
/// 3. 否则 `ListRule()` 读回**进程里正在跑的**那张表，与期望态求差，只对差集调
///    `RemoveRule` / `AddRule`（每个用户一条规则，`ruleTag` = `resi-u-<user_id>`），
///    最后把兜底规则挪回表尾；全成功 ⇒ 记哈希、清脏；
/// 4. 任一步 gRPC 失败 ⇒ [`restart_fallback`]：只有磁盘上那份 `xray-config.json` 已经
///    是新规则时才重启一次并记事件，否则什么都不做、脏标记留着。
///
/// 调用点只有对账 consumer 的末尾（`serve.rs` 启动那一轮 + 去抖那一轮）：那时新的
/// `xray-config.json` 刚落盘，第 4 步的判据才可能成立。干净时是个零成本 no-op，
/// 可以无条件调。**不在这里发 `ReconcileRequested`**：那会与「对账末尾调本函数」
/// 组成自触发环；脏标记留着等 10 分钟巡检 tick / 下一次 `StateChanged` / 每日自检更安全。
pub async fn converge_xray(ctx: &DaemonCtx, xray: &dyn XrayApi) -> ConvergeOutcome {
    if !state::read(&ctx.runtime).await.xray_slot_rules_dirty {
        return ConvergeOutcome::Clean;
    }
    let (want, want_hash) = {
        let s = ctx.store.read().await;
        let cfg = xray_render::config(&s.node, &s.users, &s.residential, &ctx.paths);
        (
            xray_render::slot_rules(&s.users, &s.residential),
            xray_render::slot_rules_hash(&cfg),
        )
    };
    if state::read(&ctx.runtime)
        .await
        .xray_slot_rules_hash
        .as_deref()
        == Some(want_hash.as_str())
    {
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        return ConvergeOutcome::Clean;
    }
    match apply_slot_rules(xray, &want).await {
        Ok(calls) => {
            state::update(&ctx.runtime, move |r| {
                r.xray_slot_rules_dirty = false;
                r.xray_slot_rules_hash = Some(want_hash);
            })
            .await;
            if calls > 0 {
                tracing::info!(
                    calls,
                    "Xray 住宅槽路由已按用户增删收敛（未重启，spec §5.6）"
                );
            }
            ConvergeOutcome::Applied(calls)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Xray RoutingService 收敛失败，看磁盘配置是否已落盘");
            restart_fallback(ctx, &want_hash, &e.to_string()).await
        }
    }
}

/// 用 `ListRule` 读回进程里真正在跑的那张表，只对差集调 `RemoveRule` / `AddRule`。
/// 返回发出的增删条数（0 = 本来就一致）。任一调用失败立刻返回 `Err`，**不回滚** ——
/// 下一轮 `ListRule` 会看到真实状态再补差分（幂等）。
async fn apply_slot_rules(xray: &dyn XrayApi, want: &[SlotRule]) -> anyhow::Result<usize> {
    let live: BTreeMap<String, String> = xray
        .list_rules()
        .await?
        .into_iter()
        .filter(|(tag, _)| {
            tag.starts_with(xray_render::USER_RULE_PREFIX) || tag == xray_render::FALLBACK_RULE_TAG
        })
        .collect();
    let wanted: BTreeSet<&str> = want.iter().map(|r| r.rule_tag.as_str()).collect();
    let mut calls = 0usize;
    // ① 多出来的（用户删了 / 没了住宅权益 / 换了 tag 写法）
    for tag in live.keys() {
        if !wanted.contains(tag.as_str()) {
            xray.remove_rule(tag).await?;
            calls += 1;
        }
    }
    // ② 缺的或指错槽的：**先删再加** —— 重名 ruleTag 会让整条 AddRule 报错（D7 事实①）
    let fallback = want.last().expect("slot_rules 末尾一定是兜底规则").clone();
    let mut appended = false;
    for r in want.iter().filter(|r| r.rule_tag != fallback.rule_tag) {
        if live.get(&r.rule_tag).map(String::as_str) == Some(r.outbound_tag.as_str()) {
            continue;
        }
        if live.contains_key(&r.rule_tag) {
            xray.remove_rule(&r.rule_tag).await?;
            calls += 1;
        }
        xray.add_rule(r).await?;
        calls += 1;
        appended = true;
    }
    // ③ `AddRule` 只能追加到表尾 ⇒ 这一轮追加过（或兜底本身缺了/指错了）就把兜底
    //    删掉再追加一次，让它回到全部用户规则之后。两次调用之间有个亚毫秒窗口，
    //    期间「一条规则都没有的 email」落到首个出站 direct（D7 已接受）。
    if appended
        || live.get(&fallback.rule_tag).map(String::as_str) != Some(fallback.outbound_tag.as_str())
    {
        if live.contains_key(&fallback.rule_tag) {
            xray.remove_rule(&fallback.rule_tag).await?;
            calls += 1;
        }
        xray.add_rule(&fallback).await?;
        calls += 1;
    }
    Ok(calls)
}

/// gRPC 收敛失败时的唯一退路：重启一次 xray，让它把磁盘上那份**完整**规则表读回来。
/// 只有磁盘哈希已等于期望哈希才动手 —— 否则重启只是把旧规则重新加载一遍。
async fn restart_fallback(ctx: &DaemonCtx, want_hash: &str, err: &str) -> ConvergeOutcome {
    let path = ctx.paths.base_dir.join("xray-config.json");
    let host = ctx.host.clone();
    let h2 = host.clone();
    let landed = tokio::task::spawn_blocking(move || h2.read_file(&path))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| xray_render::slot_rules_hash(&v));
    if landed.as_deref() != Some(want_hash) {
        tracing::debug!(
            landed = ?landed,
            want = %want_hash,
            "xray-config.json 还没落到新的槽路由，本轮不重启（脏标记留着）"
        );
        return ConvergeOutcome::Deferred;
    }
    let out = tokio::task::spawn_blocking(move || host.systemd("restart", "xray.service")).await;
    if !matches!(out, Ok(Ok(ref o)) if o.status == 0) {
        tracing::warn!("重启 xray 失败，槽路由等下一轮对账再收敛");
        return ConvergeOutcome::Deferred;
    }
    // 清脏 ⇒ 这次脏事件最多退回一次重启，不会每 10 分钟掐一遍连接
    let hash = want_hash.to_string();
    // 错误串是 gRPC 状态文本，不含凭据；仍然只取首行，避免把多行 tonic 报文塞进面板
    let msg = format!(
        "Xray 槽路由 gRPC 失败（{}），已重启 xray 收敛一次",
        err.lines().next().unwrap_or("")
    );
    state::update(&ctx.runtime, move |r| {
        r.xray_slot_rules_dirty = false;
        r.xray_slot_rules_hash = Some(hash);
        state::push_alert(r, msg);
    })
    .await;
    tracing::info!("已重启 xray 使住宅槽路由生效（gRPC 退路，spec §5.6）");
    ConvergeOutcome::Restarted
}

/// 手动把一个用户钉到某个槽（spec §5.6 规则 3 的 `assign`）。
/// 成功返回 `true`、发 `StateChanged("residential")` 并置脏标记 —— 收尾（gRPC 增删）由
/// 那次 `StateChanged` 触发的对账末尾的 [`converge_xray`] 负责，**调用方不要自己去调 gRPC**。
pub async fn assign_user(
    ctx: &DaemonCtx,
    user_id: Uuid,
    slot_upstream_id: Uuid,
) -> anyhow::Result<bool> {
    let mut ok = false;
    let flag = &mut ok;
    ctx.store
        .update(|s| *flag = slots::assign(s, user_id, slot_upstream_id))
        .await?;
    if ok {
        ctx.bus.send(Event::StateChanged("residential"));
        mark_xray_rules_dirty(&ctx.runtime).await;
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::panel::fakes::FakeXray;
    use crate::modules::residential::{state, MAX_UPSTREAMS};
    use crate::reconcile::DaemonCtx;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use bui_schema::model::{ResiMode, Upstream, UpstreamKind};
    use bui_schema::slots;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    /// D5 的对偶断言：`bui` 侧的池上限必须等于 `bui-schema` 的槽位上限。
    #[test]
    fn the_pool_cap_and_the_slot_cap_are_the_same_number() {
        assert_eq!(MAX_UPSTREAMS, slots::MAX_SLOTS as usize);
    }

    fn upstream(i: u16) -> Upstream {
        Upstream {
            id: Uuid::from_u128(u128::from(i) + 1),
            name: format!("url-{}", i + 1),
            kind: UpstreamKind::Socks5,
            host: format!("isp{}.example.net", i + 1),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 100,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        }
    }

    /// 一个装了 n 条上游、m 个住宅用户（created_at 递增）的 Store。
    async fn store_with(dir: &std::path::Path, n: u16, m: u32) -> (Store, EventBus) {
        let mut st = crate::testutil::sample_state();
        let proto = st.users[0].clone();
        st.users = (0..m)
            .map(|i| {
                let mut u = proto.clone();
                u.user_id = Uuid::from_u128(0x1000 + u128::from(i));
                u.username = format!("u{}", i + 1);
                u.created_at = format!("2026-09-11T00:0{}:00Z", i);
                u
            })
            .collect();
        let g = st
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        g.enabled = n > 0;
        g.mode = ResiMode::Global;
        g.upstreams = (0..n).map(upstream).collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        (
            Store::create(dir.join("state.json"), st).await.unwrap(),
            EventBus::new(),
        )
    }

    #[tokio::test]
    async fn migrate_on_start_fills_in_slots_and_assignments_once() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 3, 5).await;
        // 旧 state：既没有 slots，也没有任何 slot_id
        assert!(store.read().await.residential.slots.is_empty());

        assert_eq!(migrate_on_start(&store, &bus).await.unwrap(), 5);
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1, 2]);
        let mut load = vec![0usize; 3];
        for u in &s.users {
            load[slots::index_of_user(u, &s.residential) as usize] += 1;
        }
        assert_eq!(load, vec![2, 2, 1], "spec §5.6 规则 4");

        // 幂等：第二次启动不再改动任何东西（也就不会多写一次 state.json）
        assert_eq!(migrate_on_start(&store, &bus).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn adding_an_upstream_creates_its_slot_without_moving_anyone() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 4).await;
        migrate_on_start(&store, &bus).await.unwrap();
        let before: Vec<Option<Uuid>> = store
            .read()
            .await
            .users
            .iter()
            .map(slots::slot_id_of_user)
            .collect();
        let sync =
            update_group_slots_as(&store, &bus, crate::state::store::CALLER_UNLABELED, |g| {
                g.upstreams.push(upstream(2))
            })
            .await
            .unwrap();
        assert!(sync.released.is_empty());
        assert_eq!(sync.reassigned, 0, "规则 2：新增上游不搬既有用户");
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1, 2]);
        assert_eq!(
            s.users
                .iter()
                .map(slots::slot_id_of_user)
                .collect::<Vec<_>>(),
            before
        );
    }

    #[tokio::test]
    async fn removing_an_upstream_releases_its_slot_and_reassigns_its_users() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 3, 5).await;
        migrate_on_start(&store, &bus).await.unwrap();
        let gone = Uuid::from_u128(3); // 槽 2，上面是 u3
        let sync = update_group_slots_as(
            &store,
            &bus,
            crate::state::store::CALLER_RESI_REMOVE,
            move |g| g.upstreams.retain(|u| u.id != gone),
        )
        .await
        .unwrap();
        assert_eq!(
            sync.released.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(sync.reassigned, 1, "只有那个槽的用户被重分配");
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1]);
        assert!(
            s.users
                .iter()
                .all(|u| slots::slot_id_of_user(u).is_some_and(|id| id != gone)),
            "没人还指着被删掉的槽"
        );
    }

    #[tokio::test]
    async fn a_slot_runtime_entry_survives_a_runtime_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let rt = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        state::update(&rt, |r| {
            r.slots.insert(
                "1".into(),
                state::SlotRuntime {
                    current_upstream_id: Some(Uuid::from_u128(2)),
                    pinned_upstream_id: None,
                    back_rounds: 2,
                },
            );
            r.xray_slot_rules_dirty = true;
            r.xray_slot_rules_hash = Some("deadbeef".into());
        })
        .await;
        let back = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        let r = state::read(&back).await;
        assert_eq!(r.slots["1"].back_rounds, 2);
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(2)));
        assert!(r.xray_slot_rules_dirty);
        assert_eq!(r.xray_slot_rules_hash.as_deref(), Some("deadbeef"));
    }

    // ── D7：Xray 槽路由的 gRPC 收敛 ────────────────────────────────────────────

    async fn ctx_of(
        dir: &std::path::Path,
        store: Store,
        bus: EventBus,
    ) -> (DaemonCtx, std::sync::Arc<FakeHost>) {
        let host = std::sync::Arc::new(FakeHost::new());
        (
            DaemonCtx {
                store,
                runtime: crate::state::runtime::Runtime::load(dir.join("runtime.json")),
                bus,
                host: host.clone(),
                paths: bui_schema::paths::Paths::default_server(),
            },
            host,
        )
    }

    /// 期望态的 `(ruleTag, 出站)` 表，末尾一定是兜底那条。
    async fn want_rules(ctx: &DaemonCtx) -> Vec<(String, String)> {
        let s = ctx.store.read().await;
        bui_schema::render::xray::slot_rules(&s.users, &s.residential)
            .into_iter()
            .map(|r| (r.rule_tag, r.outbound_tag))
            .collect()
    }

    /// 把**当前期望态**渲染出的 xray 配置写进 FakeHost 的假文件系统，
    /// 模拟「对账已经把新的 `xray-config.json` 落盘了」（只有退回重启那条路要它）。
    async fn land_xray_config(ctx: &DaemonCtx, host: &FakeHost) {
        let s = ctx.store.read().await;
        let cfg = bui_schema::render::xray::config(&s.node, &s.users, &s.residential, &ctx.paths);
        let bytes = serde_json::to_vec_pretty(&cfg).unwrap();
        let path = ctx.paths.base_dir.join("xray-config.json");
        host.with(|i| {
            i.files.insert(path, (bytes, 0o600));
        });
    }

    fn restarts_of_xray(host: &FakeHost) -> usize {
        host.ops()
            .iter()
            .filter(|o| *o == "systemd:restart:xray.service")
            .count()
    }

    /// 「表内容一致 + 兜底在表尾」。用户规则**彼此之间的先后顺序无所谓** —— 一个 email 只
    /// 出现在一条规则里，谁先谁后都匹配同一结果；而删+加会把被改的那条挪到表尾，所以
    /// 这里比集合、只对兜底那条断言位置（它没有 `user`，排在谁前面就会把谁抢掉）。
    fn assert_same_rules(live: &[(String, String)], want: &[(String, String)]) {
        use std::collections::BTreeSet;
        assert_eq!(
            live.iter().cloned().collect::<BTreeSet<_>>(),
            want.iter().cloned().collect::<BTreeSet<_>>(),
            "规则表内容与期望态不一致"
        );
        assert_eq!(
            live.last().map(|(tag, _)| tag.as_str()),
            Some("resi-fallback"),
            "兜底必须在表尾：{live:?}"
        );
    }

    /// 第二个槽的槽位键（上游 uuid）。
    async fn second_slot_id(ctx: &DaemonCtx) -> Uuid {
        let s = ctx.store.read().await;
        slots::sorted(&s.residential)[1].upstream_id
    }

    #[tokio::test]
    async fn converge_xray_is_a_no_op_when_nothing_is_dirty() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        let x = FakeXray::new();
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert!(
            x.calls().is_empty(),
            "干净时连 ListRule 都不该发：{:?}",
            x.calls()
        );
        assert_eq!(restarts_of_xray(&host), 0);
    }

    #[tokio::test]
    async fn converge_xray_installs_one_rule_per_user_without_restarting() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();

        let n = match converge_xray(&ctx, &x).await {
            ConvergeOutcome::Applied(n) => n,
            other => panic!("期望 Applied，实际 {other:?}"),
        };
        assert!(n > 0);
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(
            restarts_of_xray(&host),
            0,
            "槽路由靠 gRPC 生效，**不许**重启 xray（M3 判据①）：{:?}",
            host.ops()
        );
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty, "收敛成功后清脏");
        assert!(r.xray_slot_rules_hash.is_some(), "记下已收敛的槽路由哈希");
        // 幂等：再置一次脏也只会 ListRule 一次、什么都不改
        x.clear_calls();
        mark_xray_rules_dirty(&ctx.runtime).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert_eq!(
            x.calls(),
            Vec::<String>::new(),
            "哈希没变 ⇒ 清脏不动（D7 第 2 步）"
        );
    }

    /// **裁决的核心**：改一个人的槽位只碰他那一条规则（外加把兜底挪回表尾），
    /// 邻居的规则一个字都不动，xray 不重启。
    #[tokio::test]
    async fn assigning_a_user_touches_only_his_own_rule() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        converge_xray(&ctx, &x).await;

        let (moved, other) = {
            let s = ctx.store.read().await;
            (s.users[0].user_id, s.users[1].user_id)
        };
        let slot1 = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, moved, slot1).await.unwrap());
        x.clear_calls();
        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            x.calls(),
            vec![
                "list-rules".to_string(),
                format!("remove-rule:resi-u-{moved}"),
                format!("add-rule:resi-u-{moved}:relay-slot-1"),
                "remove-rule:resi-fallback".to_string(),
                "add-rule:resi-fallback:relay-slot-0".to_string(),
            ],
            "先删再加（重名会让整条 AddRule 报错），最后把兜底挪回表尾"
        );
        assert!(
            !x.calls().iter().any(|c| c.contains(&other.to_string())),
            "邻居的规则不许动：{:?}",
            x.calls()
        );
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
    }

    /// 删用户只发一次 `RemoveRule`，连兜底都不用动（删规则不会打乱顺序）。
    #[tokio::test]
    async fn deleting_a_user_only_removes_his_rule() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        converge_xray(&ctx, &x).await;

        let gone = ctx.store.read().await.users[0].user_id;
        ctx.store
            .update(|s| s.users.retain(|u| u.user_id != gone))
            .await
            .unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        x.clear_calls();
        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            x.calls(),
            vec![
                "list-rules".to_string(),
                format!("remove-rule:resi-u-{gone}")
            ]
        );
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0);
    }

    /// gRPC 挂了（xray 没在听 / RoutingService 没开）且**新配置已落盘** ⇒
    /// 退回一次 restart（重启会把磁盘上那份完整规则表读回来），记一条面板可见的事件，
    /// 并且**只退一次**：清脏之后不会每 10 分钟再重启一遍。
    #[tokio::test]
    async fn a_grpc_failure_falls_back_to_exactly_one_restart_and_records_an_event() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });

        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(restarts_of_xray(&host), 1, "{:?}", host.ops());
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty);
        assert!(r.xray_slot_rules_hash.is_some());
        assert!(
            r.alerts.iter().any(|a| a.contains("槽路由")),
            "面板要看得见这次退回：{:?}",
            r.alerts
        );
        // 再来一轮：已经不脏了 ⇒ 不再重启（「退回一次」的含义）
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    /// gRPC 挂了、而且磁盘上还是旧配置（对账在 500ms 之后，或全新装机还没跑过第一轮）：
    /// **不重启、不清脏**。这时重启只会把旧规则重新加载一遍，脏标记一旦被清掉，
    /// 新槽路由就要等下一次别的脏事件才生效。
    #[tokio::test]
    async fn a_grpc_failure_defers_when_the_config_file_has_not_landed() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        // 先落一份「分槽变更之前」的配置，再改分槽 ⇒ 磁盘哈希 != 期望哈希
        land_xray_config(&ctx, &host).await;
        let uid = ctx.store.read().await.users[0].user_id;
        let slot1 = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, uid, slot1).await.unwrap());
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });

        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Deferred);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
        assert!(
            state::read(&ctx.runtime).await.xray_slot_rules_dirty,
            "文件没落地时**绝不**清脏标记，否则这台机器的槽路由永远不收敛"
        );
        // 对账把新文件写下去之后（serve.rs 的每轮收口），同一条路才走到重启
        land_xray_config(&ctx, &host).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    #[tokio::test]
    async fn a_failed_restart_keeps_the_dirty_flag_for_the_next_round() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 1).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        // FakeHost 的 systemd 失败注入走 `fail_units`（裸名或全名都认，见 sys/fake.rs）
        host.with(|i| {
            i.fail_units.insert("xray".into());
        });
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Deferred);
        assert_eq!(restarts_of_xray(&host), 1, "重启确实发了一次，只是失败了");
        assert!(
            state::read(&ctx.runtime).await.xray_slot_rules_dirty,
            "重启失败不许清标记，否则这台机器的槽路由永远不收敛"
        );
    }

    /// 一条 `AddRule` 失败（比如内核报 duplicate ruleTag）⇒ 整轮算失败，走同一条退路；
    /// 已经发出去的那几条不回滚（下一轮 `ListRule` 会看到真实状态再补差分）。
    #[tokio::test]
    async fn a_failing_add_rule_also_falls_back() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("add-rule".into());
        });
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    #[tokio::test]
    async fn assigning_a_user_marks_the_slot_rules_dirty() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, _host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        let uid = ctx.store.read().await.users[0].user_id;
        let target = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, uid, target).await.unwrap());
        assert!(state::read(&ctx.runtime).await.xray_slot_rules_dirty);
    }
}
