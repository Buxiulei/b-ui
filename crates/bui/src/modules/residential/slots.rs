//! `bui` 侧的槽位入口（spec §5.6）：把 `bui_schema::slots` 的纯函数包进一次
//! `Store::update_as`，顺带同步槽位表、重分配孤儿用户，再发一次
//! `Event::StateChanged("residential")` 交给 P1 的去抖对账重渲染。
//!
//! 本模块**不渲染任何内核配置**（与 `residential/mod.rs` 同一条边界）：
//! `config-residential[-<i>].yaml`、`singbox-relay.json`、`xray-config.json` 都由
//! `crate::modules::core_files` / `units` 从期望态渲染。
use crate::api::{Event, EventBus};
use crate::state::store::Store;
use bui_schema::model::{ResidentialGroup, Slot, State, DEFAULT_GROUP};
use bui_schema::slots;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::{state, MAX_UPSTREAMS};
    use crate::state::store::Store;
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
}
