//! IP 池的槽位（spec §5.6）：端口资源与分配器，全是纯函数。
//!
//! 槽位语义：池内每个上游 IP 是一个槽，键是上游 uuid，序号 [`Slot::index`] 决定它的
//! 全部端口。多个用户共用一个槽（用户数 > IP 数），同一用户稳定走同一个 IP（粘性）。
//!
//! 三条不变量，全靠 [`sync_slots`] 维护：
//! 1. 每条上游恰好一个槽，每个槽恰好指向一条在池里的上游；
//! 2. 序号取 `0..MAX_SLOTS` 的最小空闲值；
//! 3. **池非空 ⇒ 序号 0 的槽存在**（relay 的 `2080` 与 xray 的 `relay-slot-0` 是兼容面，
//!    不许悬空）。4.1 起这条不变量**只保护这两个名字**：`40000` 归整个住宅 HY2 入站、
//!    `9998` 已消失、`hysteria-residential.service` 归那个入站，三者都不再与槽 0 绑定
//!    （spec §4.2 末段）。
use crate::model::{Residential, Slot, State, User, DEFAULT_GROUP};
use uuid::Uuid;

/// relay 每槽一个 socks 入站的基准端口：槽 i 监听 `127.0.0.1:(2080 + i)`。
pub const RELAY_SOCKS_BASE: u16 = 2080;
/// 槽位上限，与住宅池上限同值（D5）。
pub const MAX_SLOTS: u16 = 8;

/// 一个槽位的端口资源。每一项都是序号的纯函数。
///
/// 4.1 起只剩一项：**槽位与住宅 HY2 的对外端口彻底脱钩**（住宅 HY2 是一个 sing-box 入站
/// `ports.hy2_resi`，整段跳跃由 `table inet bui` 送进去），所以这里只服务 relay 的 socks
/// 入站与 xray 住宅出站。4.0.x 的 `hy2_port` / `stats_port` / `hop` 三项连同
/// `hop_slice` / `slot_span` / `HY2_STATS_RESI_BASE` / `resources_of` 一起删掉了
/// （spec §4.2、§14 裁决 6）——按槽算对外端口正是 2026-09-15 回归事故的入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRes {
    pub index: u16,
    /// relay 的 socks 入站端口，也是该槽 xray 住宅出站的目标
    pub relay_port: u16,
}

/// 全部槽位，按序号升序（`state.residential.slots` 的规范视图）。
pub fn sorted(r: &Residential) -> Vec<Slot> {
    let mut v = r.slots.clone();
    v.sort_by_key(|s| s.index);
    v
}

/// 要渲染几个住宅实例、各自的槽序号。池空 / 旧 state 没有槽位时 = 只有槽 0，
/// 于是渲染结果与 v3 单实例逐字等价。
pub fn indices(r: &Residential) -> Vec<u16> {
    let v: Vec<u16> = sorted(r).iter().map(|s| s.index).collect();
    if v.is_empty() {
        vec![0]
    } else {
        v
    }
}

/// 兜底槽的序号：序号最小的那个槽（不变量 3 保证池非空时就是 0）。
/// 未分配、或分配指向了已消失槽位的用户都落在它上面。
pub fn fallback_index(r: &Residential) -> u16 {
    indices(r)[0]
}

/// 槽 `index` 的端口资源。序号越界（> [`MAX_SLOTS`] - 1）夹到最后一个槽。
pub fn resources(index: u16) -> SlotRes {
    let index = index.min(MAX_SLOTS - 1);
    SlotRes {
        index,
        relay_port: RELAY_SOCKS_BASE + index,
    }
}

/// 用户粘住的槽位键（上游 uuid）。没有住宅权益 / 没分过槽 ⇒ `None`。
pub fn slot_id_of_user(u: &User) -> Option<Uuid> {
    u.entitlements.residential.as_ref().and_then(|e| e.slot_id)
}

/// 用户落在哪个槽序号。未分配、或指向已消失的槽 ⇒ 兜底槽。
pub fn index_of_user(u: &User, r: &Residential) -> u16 {
    slot_id_of_user(u)
        .and_then(|id| r.slots.iter().find(|s| s.upstream_id == id))
        .map(|s| s.index)
        .unwrap_or_else(|| fallback_index(r))
}

/// `0..MAX_SLOTS` 里第一个没被占的序号（池满则 `None`）。
pub fn free_index(r: &Residential) -> Option<u16> {
    (0..MAX_SLOTS).find(|i| !r.slots.iter().any(|s| s.index == *i))
}

/// 按池现状同步槽位表，维护模块文档里的三条不变量。返回**被释放**的槽位
/// （它们的用户要重新分配，见 [`migrate_unassigned`]）。
pub fn sync_slots(r: &mut Residential) -> Vec<Slot> {
    let ids: Vec<Uuid> = r
        .groups
        .get(DEFAULT_GROUP)
        .map(|g| g.upstreams.iter().map(|u| u.id).collect())
        .unwrap_or_default();
    let released: Vec<Slot> = r
        .slots
        .iter()
        .copied()
        .filter(|s| !ids.contains(&s.upstream_id))
        .collect();
    r.slots.retain(|s| ids.contains(&s.upstream_id));
    for id in ids {
        if r.slots.iter().any(|s| s.upstream_id == id) {
            continue;
        }
        let Some(index) = free_index(r) else { break };
        r.slots.push(Slot {
            index,
            upstream_id: id,
        });
    }
    r.slots.sort_by_key(|s| s.index);
    // 不变量 3：池非空时序号 0 必须有人（D2）。序号 0 被释放掉时，把现存序号
    // 最小的那个槽搬到 0 —— 4.1 起这不动任何人的订阅（住宅 HY2 的端口与槽无关），
    // 只是让 relay 的 `2080` 与 xray 的 `relay-slot-0` 这两个兼容面不悬空。
    if let Some(first) = r.slots.first_mut() {
        if first.index != 0 {
            first.index = 0;
        }
    }
    released
}

/// 某个槽上的全部用户。
pub fn users_of_slot(s: &State, slot_upstream_id: Uuid) -> Vec<&User> {
    s.users
        .iter()
        .filter(|u| slot_id_of_user(u) == Some(slot_upstream_id))
        .collect()
}

/// 用户数最少的槽（平手取序号最小，序号也平手取 uuid —— 完全确定性）。
/// 空池 ⇒ `None`。
pub fn least_loaded(s: &State) -> Option<Uuid> {
    sorted(&s.residential)
        .iter()
        .map(|sl| {
            (
                users_of_slot(s, sl.upstream_id).len(),
                sl.index,
                sl.upstream_id,
            )
        })
        .min()
        .map(|(_, _, id)| id)
}

/// 把一个用户钉到指定槽位。用户不存在 / 没有住宅权益 / 槽位键不在池里 ⇒ `false`（不改动）。
pub fn assign(s: &mut State, user_id: Uuid, slot_upstream_id: Uuid) -> bool {
    if !s
        .residential
        .slots
        .iter()
        .any(|x| x.upstream_id == slot_upstream_id)
    {
        return false;
    }
    match s
        .users
        .iter_mut()
        .find(|u| u.user_id == user_id)
        .and_then(|u| u.entitlements.residential.as_mut())
    {
        Some(e) => {
            e.slot_id = Some(slot_upstream_id);
            true
        }
        None => false,
    }
}

/// 规则 1：把一个用户分到用户数最少的槽。
pub fn assign_least_loaded(s: &mut State, user_id: Uuid) -> bool {
    match least_loaded(s) {
        Some(id) => assign(s, user_id, id),
        None => false,
    }
}

/// 住宅用户的 uuid，按 `created_at` 升序（同时刻按 uuid，保证确定性）。
fn residential_users_in_creation_order(s: &State) -> Vec<Uuid> {
    let mut v: Vec<(&str, Uuid)> = s
        .users
        .iter()
        .filter(|u| u.entitlements.residential.is_some())
        .map(|u| (u.created_at.as_str(), u.user_id))
        .collect();
    v.sort_unstable();
    v.into_iter().map(|(_, id)| id).collect()
}

/// 规则 2 与规则 4 的**同一个实现**：把「没分过槽」和「指向已消失槽位」的住宅用户
/// 按创建时间顺序逐个分到当时用户数最少的槽。返回被改动的用户数。
///
/// 逐个而不是一次性平分：这样它同时是「新增上游后不搬既有用户、只承接后来者」
/// （规则 2 后半句）与「升级时轮流落槽」（规则 4）的正解 —— 5 人 3 IP 得到 2/2/1。
pub fn migrate_unassigned(s: &mut State) -> usize {
    if s.residential.slots.is_empty() {
        return 0;
    }
    let mut changed = 0;
    for id in residential_users_in_creation_order(s) {
        let stale = match s.users.iter().find(|u| u.user_id == id) {
            Some(u) => slot_id_of_user(u)
                .is_none_or(|sid| !s.residential.slots.iter().any(|x| x.upstream_id == sid)),
            None => false,
        };
        if stale && assign_least_loaded(s, id) {
            changed += 1;
        }
    }
    changed
}

/// 规则 3：把住宅用户在各槽间均匀重排（按创建时间稳定排序）。返回被改动的用户数。
///
/// 实现 = 清空全部分配再跑一次 [`migrate_unassigned`]，因此与规则 4 的迁移口径**同一个
/// 算法**，并且**幂等**：连调两次第二次返回 0。这就是「尽量少动」在确定性算法下的落地。
pub fn rebalance(s: &mut State) -> usize {
    if s.residential.slots.is_empty() {
        return 0;
    }
    let before: Vec<(Uuid, Option<Uuid>)> = s
        .users
        .iter()
        .map(|u| (u.user_id, slot_id_of_user(u)))
        .collect();
    for u in s.users.iter_mut() {
        if let Some(e) = u.entitlements.residential.as_mut() {
            e.slot_id = None;
        }
    }
    migrate_unassigned(s);
    s.users
        .iter()
        .filter(|u| {
            before
                .iter()
                .find(|(id, _)| *id == u.user_id)
                .map(|(_, old)| *old)
                != Some(slot_id_of_user(u))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Residential, Slot};
    use pretty_assertions::assert_eq;

    fn resi(indices: &[u16]) -> Residential {
        Residential {
            slots: indices
                .iter()
                .map(|i| Slot {
                    index: *i,
                    upstream_id: Uuid::from_u128(u128::from(*i) + 1),
                })
                .collect(),
            ..Residential::default()
        }
    }

    /// D5：槽位上限必须等于住宅池上限。谁改一边，这里立刻红。
    #[test]
    fn max_slots_is_eight() {
        assert_eq!(MAX_SLOTS, 8);
        assert_eq!(RELAY_SOCKS_BASE, 2080);
    }

    /// 单槽（含空池）：槽 0 的 relay 入站必须还是 `2080`（xray 的 `relay-slot-0`
    /// 与中继配置都按它走）。
    #[test]
    fn a_single_slot_keeps_the_v3_relay_port_exactly() {
        let empty = Residential::default();
        assert_eq!(indices(&empty), vec![0]);
        assert_eq!(fallback_index(&empty), 0);
        assert_eq!(
            resources(0),
            SlotRes {
                index: 0,
                relay_port: 2080,
            }
        );
    }

    /// `SlotRes` 只剩两项：端口换算只服务 relay 入站与 xray 住宅出站。
    ///
    /// 下面这几行必须**编译不过**（字段与函数都已删干净，spec §14 裁决 6）：
    /// `let _ = r.hy2_port;` / `let _ = r.stats_port;` / `let _ = r.hop;` /
    /// `slots::hop_slice(...)` / `slots::slot_span(&r)` / `slots::resources_of(&p, &r, 0)`。
    #[test]
    fn slot_resources_are_now_only_the_relay_port() {
        let r = resources(3);
        assert_eq!(r.index, 3);
        assert_eq!(r.relay_port, RELAY_SOCKS_BASE + 3);
    }

    /// 8 槽下每个槽的 relay 端口互不相同，且与 `MAX_SLOTS` 同源。
    #[test]
    fn eight_slots_get_eight_distinct_relay_ports() {
        let ports: std::collections::BTreeSet<u16> =
            (0..MAX_SLOTS).map(|i| resources(i).relay_port).collect();
        assert_eq!(ports.len(), usize::from(MAX_SLOTS));
        assert_eq!(*ports.iter().next().unwrap(), 2080);
        assert_eq!(*ports.iter().last().unwrap(), 2087);
    }

    /// 三个槽各拿自己那一个 relay 端口，**不再有跳跃段可切**（4.0.x 那三条「切片首尾
    /// 相接」的断言随 `hop_slice` 一起删了）。越界序号夹到最后一个槽，不 panic。
    #[test]
    fn three_slots_get_three_contiguous_relay_ports() {
        let s: Vec<SlotRes> = (0..3).map(resources).collect();
        assert_eq!(
            s.iter().map(|x| x.relay_port).collect::<Vec<_>>(),
            vec![2080, 2081, 2082]
        );
        assert_eq!(
            resources(MAX_SLOTS),
            resources(MAX_SLOTS - 1),
            "越界夹到顶槽"
        );
        assert_eq!(resources(99).relay_port, 2087);
    }

    #[test]
    fn a_user_without_an_assignment_falls_back_to_the_lowest_slot() {
        let r = resi(&[0, 1]);
        let mut u: User = serde_json::from_str(
            r#"{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"alice","created_at":"2026-09-11T00:00:00Z",
                "credentials":{"hy2_password":"pw1","vless_uuid":"11111111-1111-4111-8111-111111111111"},
                "entitlements":{"protocols":["hysteria2"],"direct":true,"residential":{"group_id":"default"}}}"#,
        )
        .unwrap();
        assert_eq!(index_of_user(&u, &r), 0, "没分过槽 ⇒ 兜底槽");
        u.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(2));
        assert_eq!(index_of_user(&u, &r), 1);
        // 指向已被删掉的槽 ⇒ 兜底槽（而不是 panic、也不是凭空造一个序号）
        u.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(99));
        assert_eq!(index_of_user(&u, &r), 0);
    }

    /// 造一份「n 条上游、m 个住宅用户」的期望态：用户 created_at 依次递增，
    /// 用户名 `u1..um`，上游 uuid = `Uuid::from_u128(i+1)`（与 `resi()` 同规则）。
    fn state(upstreams: u16, users: u32) -> State {
        let mut s: State = serde_json::from_str(SAMPLE_STATE).unwrap();
        let g = s.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        let proto = g.upstreams[0].clone();
        g.upstreams = (0..upstreams)
            .map(|i| crate::model::Upstream {
                id: Uuid::from_u128(u128::from(i) + 1),
                name: format!("url-{}", i + 1),
                host: format!("isp{}.example.net", i + 1),
                ..proto.clone()
            })
            .collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        let proto_user = s.users[0].clone();
        s.users = (0..users)
            .map(|i| {
                let mut u = proto_user.clone();
                u.user_id = Uuid::from_u128(0x1000 + u128::from(i));
                u.username = format!("u{}", i + 1);
                u.created_at = format!("2026-09-11T00:0{}:00Z", i);
                u.entitlements.residential = Some(crate::model::ResidentialEntitlement {
                    group_id: DEFAULT_GROUP.into(),
                    slot_id: None,
                });
                u
            })
            .collect();
        sync_slots(&mut s.residential);
        s
    }

    /// 每个槽上有几个用户，按槽序号升序。
    fn load(s: &State) -> Vec<usize> {
        sorted(&s.residential)
            .iter()
            .map(|sl| users_of_slot(s, sl.upstream_id).len())
            .collect()
    }

    #[test]
    fn sync_slots_gives_every_upstream_the_lowest_free_index() {
        let mut s = state(3, 0);
        assert_eq!(
            sorted(&s.residential)
                .iter()
                .map(|x| (x.index, x.upstream_id))
                .collect::<Vec<_>>(),
            vec![
                (0, Uuid::from_u128(1)),
                (1, Uuid::from_u128(2)),
                (2, Uuid::from_u128(3)),
            ]
        );
        // 删中间那条 ⇒ 序号 1 空出来，存活的槽序号不动（端口不churn）
        let g = s.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        g.upstreams.retain(|u| u.id != Uuid::from_u128(2));
        let released = sync_slots(&mut s.residential);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].index, 1);
        assert_eq!(indices(&s.residential), vec![0, 2]);
        assert_eq!(free_index(&s.residential), Some(1), "最小空闲序号");
        // 新加一条 ⇒ 补进空洞
        let mut nu = s.residential.groups[DEFAULT_GROUP].upstreams[0].clone();
        nu.id = Uuid::from_u128(4);
        nu.host = "isp4.example.net".into();
        s.residential
            .groups
            .get_mut(DEFAULT_GROUP)
            .unwrap()
            .upstreams
            .push(nu);
        assert!(sync_slots(&mut s.residential).is_empty());
        assert_eq!(indices(&s.residential), vec![0, 1, 2]);
        assert_eq!(
            sorted(&s.residential)[1].upstream_id,
            Uuid::from_u128(4),
            "新条目占了空洞 1"
        );
    }

    /// D2 不变量：删掉序号 0 那条上游后，必须有别的槽搬到 0。
    #[test]
    fn slot_zero_always_exists_while_the_pool_is_not_empty() {
        let mut s = state(3, 0);
        let g = s.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        g.upstreams.retain(|u| u.id != Uuid::from_u128(1));
        sync_slots(&mut s.residential);
        assert_eq!(indices(&s.residential), vec![0, 2]);
        assert_eq!(
            sorted(&s.residential)[0].upstream_id,
            Uuid::from_u128(2),
            "原来序号最小的槽搬到了 0"
        );
        // 池清空 ⇒ 槽位表空，渲染视图退回单槽
        s.residential
            .groups
            .get_mut(DEFAULT_GROUP)
            .unwrap()
            .upstreams
            .clear();
        sync_slots(&mut s.residential);
        assert!(s.residential.slots.is_empty());
        assert_eq!(indices(&s.residential), vec![0]);
    }

    /// 规则 1：新建用户分到用户数最少的槽，平手取序号最小。
    #[test]
    fn a_new_user_lands_on_the_least_loaded_slot_with_ties_by_index() {
        let mut s = state(3, 0);
        let mut u = s.users.clone(); // 空的
        assert!(u.is_empty());
        let ids: Vec<Uuid> = (0..4).map(|i| Uuid::from_u128(0x2000 + i)).collect();
        let proto: User = serde_json::from_str(SAMPLE_USER).unwrap();
        for id in &ids {
            let mut x = proto.clone();
            x.user_id = *id;
            u.push(x);
        }
        s.users = u;
        for id in &ids {
            assert!(assign_least_loaded(&mut s, *id));
        }
        assert_eq!(
            load(&s),
            vec![2, 1, 1],
            "四个人进三个槽：2/1/1，平手取序号最小"
        );
    }

    /// 规则 4：升级迁移 —— 5 人 3 IP ⇒ 2/2/1，且按 created_at 顺序落槽。
    #[test]
    fn migration_round_robins_existing_users_by_creation_time() {
        let mut s = state(3, 5);
        assert_eq!(migrate_unassigned(&mut s), 5);
        assert_eq!(load(&s), vec![2, 2, 1], "spec §5.6 规则 4 的算例");
        let slot_of = |name: &str| -> u16 {
            let u = s.users.iter().find(|u| u.username == name).unwrap();
            index_of_user(u, &s.residential)
        };
        assert_eq!(
            (
                slot_of("u1"),
                slot_of("u2"),
                slot_of("u3"),
                slot_of("u4"),
                slot_of("u5")
            ),
            (0, 1, 2, 0, 1)
        );
        assert_eq!(migrate_unassigned(&mut s), 0, "已分配的不再动");
    }

    /// 规则 2：删上游 ⇒ 只有那个槽的用户重分配，其余人一个都不动。
    #[test]
    fn removing_an_upstream_reassigns_only_its_own_users() {
        let mut s = state(3, 5);
        migrate_unassigned(&mut s);
        let before: Vec<(String, u16)> = s
            .users
            .iter()
            .map(|u| (u.username.clone(), index_of_user(u, &s.residential)))
            .collect();
        // 干掉槽 2（上游 uuid=3），它只有 u3
        let g = s.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        g.upstreams.retain(|u| u.id != Uuid::from_u128(3));
        sync_slots(&mut s.residential);
        assert_eq!(migrate_unassigned(&mut s), 1, "只有 u3 被重分配");
        let after: Vec<(String, u16)> = s
            .users
            .iter()
            .map(|u| (u.username.clone(), index_of_user(u, &s.residential)))
            .collect();
        for (name, idx) in &before {
            if name != "u3" {
                assert_eq!(
                    after.iter().find(|(n, _)| n == name).unwrap().1,
                    *idx,
                    "{name} 不该被搬动"
                );
            }
        }
        assert_eq!(load(&s), vec![3, 2]);
    }

    /// 照 `upstream::remove` 的同一条路径删一条上游（摘成员 → [`sync_slots`] →
    /// [`migrate_unassigned`]），返回删后的期望态。
    fn after_remove(s: &State, upstream_id: Uuid) -> State {
        let mut after = s.clone();
        after
            .residential
            .groups
            .get_mut(DEFAULT_GROUP)
            .unwrap()
            .upstreams
            .retain(|u| u.id != upstream_id);
        sync_slots(&mut after.residential);
        migrate_unassigned(&mut after);
        after
    }

    /// 某用户订阅里 HY2 住宅节点的 `(端口, 跳跃区间)`。订阅内容的真源只有
    /// [`nodes::nodes_for`] 一处。
    fn node_of(s: &State, name: &str) -> (u16, Option<(u16, u16)>) {
        let u = s.users.iter().find(|u| u.username == name).unwrap();
        let n = crate::nodes::nodes_for(u, &s.node, &s.residential)
            .into_iter()
            .find(|n| n.kind == crate::nodes::NodeKind::Hy2Residential)
            .expect("该用户订阅里没有 HY2 住宅节点");
        (n.port, n.hop)
    }

    /// 照 `upstream::add` 的同一条路径加**第 `n + 1` 条**上游（[`sync_slots`] →
    /// [`migrate_unassigned`]），返回加后的期望态。
    fn after_add(s: &State, n: u16) -> State {
        let mut after = s.clone();
        let g = after.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        let mut extra = g.upstreams[0].clone();
        extra.id = Uuid::from_u128(u128::from(n) + 1);
        extra.host = format!("isp{}.example.net", n + 1);
        g.upstreams.push(extra);
        sync_slots(&mut after.residential);
        migrate_unassigned(&mut after);
        after
    }

    /// 4.1 的退役判据（spec §1.2 目标 1）：**改槽不动订阅**。
    ///
    /// 4.0.x 里住宅 HY2 节点的端口是 `hy2_resi + 槽序号`、跳跃段是 `hy2_resi_hop` 按当下
    /// 槽数等分的第 i 片，于是增删上游、`assign`、`rebalance` 都会改掉已下发订阅里的端口
    /// 或段，受影响的用户必须重新获取订阅（那套「必须重新获取订阅」的名单机制就是为它存在
    /// 的）。4.1 起住宅 HY2 只有**一个**监听端口、整段 `41000-50000` 由 `table inet bui`
    /// 的 REDIRECT 送进去（[`nodes::nodes_for`](crate::nodes::nodes_for)），所以每个用户的
    /// 端口与区间完全相同、与槽位无关 —— 名单机制随之退役。
    ///
    /// 这条用例是那次退役的回归锁：只要哪天端口又跟槽序号挂上钩，它就红。
    ///
    /// 每条腿都**先断言槽位真的动过**（4.0.x 算端口用的那两个输入：每个用户的槽序号 +
    /// 槽位空间宽度）—— 否则某天 `sync_slots` / `rebalance` 退化成 no-op，「订阅没变」
    /// 会在「什么都没变」的情况下照旧全绿（第六波复核点名）。
    /// `assign` 那条路在 `bui` 侧覆盖（`residential::slots` 与 `residential::upstream`
    /// 各有一条带「他确实换了槽」断言的用例），所以这里是四条腿。
    #[test]
    fn changing_the_pool_never_moves_anybody_s_subscription() {
        /// 4.0.x 算端口/区间的两个输入：每人的槽序号 + 槽位空间宽度（当年 `hop_slice`
        /// 的分母 = 最高序号 + 1）。两个输入都已经不参与订阅了，这里就地算一遍只为
        /// 证明「槽位真的动过」—— 否则某天 `sync_slots` / `rebalance` 退化成 no-op，
        /// 「订阅没变」会在「什么都没变」的情况下照旧全绿。
        fn port_inputs(s: &State) -> (Vec<(String, u16)>, u16) {
            let mut per_user: Vec<(String, u16)> = s
                .users
                .iter()
                .map(|u| (u.username.clone(), index_of_user(u, &s.residential)))
                .collect();
            per_user.sort();
            let span = s
                .residential
                .slots
                .iter()
                .map(|x| x.index + 1)
                .max()
                .unwrap_or(1);
            (per_user, span)
        }

        let mut s = state(3, 5);
        migrate_unassigned(&mut s);
        crate::hy2pool::migrate(&mut s, time::OffsetDateTime::now_utc());
        let before: Vec<(u16, Option<(u16, u16)>)> =
            (1..=5).map(|n| node_of(&s, &format!("u{n}"))).collect();
        assert!(
            before.iter().all(|x| *x == (40000, Some((41000, 50000)))),
            "4.1：端口与整段跳跃对每个用户都一样，{before:?}"
        );
        // rebalance 在已经均匀的池上是 no-op，那样这条腿什么都没验 —— 先把所有人压到槽 0
        let squeezed = {
            let mut r = s.clone();
            let slot0 = sorted(&r.residential)[0].upstream_id;
            for u in r.users.iter_mut() {
                u.entitlements.residential.as_mut().unwrap().slot_id = Some(slot0);
            }
            r
        };
        let rebalanced = {
            let mut r = squeezed.clone();
            assert!(rebalance(&mut r) > 0, "rebalance 没搬人，这条腿什么都没验");
            r
        };
        // 删 0 号槽（不变量 3 会把槽 1 搬到 0）、删顶槽、加一条上游、rebalance —— 四条
        // 改槽路径挨个来一遍，订阅里那个节点一个字节都不许动
        for (path, from, after) in [
            ("删 0 号槽", &s, after_remove(&s, Uuid::from_u128(1))),
            ("删顶槽", &s, after_remove(&s, Uuid::from_u128(3))),
            ("加一条上游", &s, after_add(&s, 3)),
            ("rebalance", &squeezed, rebalanced),
        ] {
            assert_ne!(
                port_inputs(&after),
                port_inputs(from),
                "{path}：槽序号与槽位空间都没变 —— 这条腿证明不了「改槽不动订阅」"
            );
            let now: Vec<(u16, Option<(u16, u16)>)> =
                (1..=5).map(|n| node_of(&after, &format!("u{n}"))).collect();
            assert_eq!(now, before, "{path}：改槽动了订阅里的住宅 HY2 节点");
        }
    }

    /// 规则 3：rebalance 均匀重排，且**幂等**（连调两次第二次零改动）。
    #[test]
    fn rebalance_is_even_and_idempotent() {
        let mut s = state(3, 5);
        migrate_unassigned(&mut s);
        // 人为把所有人压到槽 0
        let slot0 = sorted(&s.residential)[0].upstream_id;
        for u in s.users.iter_mut() {
            u.entitlements.residential.as_mut().unwrap().slot_id = Some(slot0);
        }
        assert_eq!(load(&s), vec![5, 0, 0]);
        assert_eq!(rebalance(&mut s), 3, "5 人里有 3 个要搬走");
        assert_eq!(load(&s), vec![2, 2, 1]);
        assert_eq!(rebalance(&mut s), 0, "幂等 = 「尽量少动」的确定性落地");
    }

    /// 手动指定：只接受池里真实存在的槽位键。
    #[test]
    fn manual_assign_rejects_an_unknown_slot() {
        let mut s = state(2, 1);
        let uid = s.users[0].user_id;
        assert!(!assign(&mut s, uid, Uuid::from_u128(99)));
        assert!(assign(&mut s, uid, Uuid::from_u128(2)));
        assert_eq!(index_of_user(&s.users[0], &s.residential), 1);
    }

    /// 没有住宅权益的用户永不进分配器（否则订阅会凭空长出住宅节点）。
    #[test]
    fn a_user_without_the_residential_entitlement_is_never_assigned() {
        let mut s = state(2, 2);
        s.users[0].entitlements.residential = None;
        let uid = s.users[0].user_id;
        assert_eq!(migrate_unassigned(&mut s), 1);
        assert!(slot_id_of_user(&s.users[0]).is_none());
        assert!(!assign_least_loaded(&mut s, uid));
        assert_eq!(load(&s), vec![1, 0]);
    }

    /// 空池：分配器一律不动任何人（fail-open，出口回落直连）。
    #[test]
    fn an_empty_pool_assigns_nobody() {
        let mut s = state(0, 3);
        assert!(s.residential.slots.is_empty());
        let uid = s.users[0].user_id;
        assert_eq!(migrate_unassigned(&mut s), 0);
        assert_eq!(rebalance(&mut s), 0);
        assert!(!assign_least_loaded(&mut s, uid));
        assert_eq!(least_loaded(&s), None);
    }

    const SAMPLE_USER: &str = r#"{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"x","created_at":"2026-09-11T00:00:00Z",
        "credentials":{"hy2_password":"pw1","vless_uuid":"11111111-1111-4111-8111-111111111111"},
        "entitlements":{"protocols":["hysteria2"],"direct":true,"residential":{"group_id":"default"}}}"#;

    const SAMPLE_STATE: &str = r#"{
      "schema_version": 1,
      "node": { "id": "8d5a1a1e-3b2c-4d1e-9f00-000000000001", "name": "node-a", "domain": "example.com", "public_ip": "203.0.113.10",
                "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                           "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
                "reality": { "private_key": "a", "public_key": "b", "short_ids": ["0123456789abcdef"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
                "obfs": { "enabled": false, "password": "" } },
      "admin": { "password_hash": "h", "jwt_secret": "s" },
      "users": [ {"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"x","created_at":"2026-09-11T00:00:00Z",
        "credentials":{"hy2_password":"pw1","vless_uuid":"11111111-1111-4111-8111-111111111111"},
        "entitlements":{"protocols":["hysteria2"],"direct":true,"residential":{"group_id":"default"}}} ],
      "residential": { "groups": { "default": { "enabled": true, "mode": "global", "keywords": null,
                        "upstreams": [ { "id": "00000000-0000-0000-0000-000000000001", "name": "url-1", "kind": "socks5",
                                         "host": "isp.example.net", "port": 10007, "username": "user1", "password": "pw1",
                                         "priority": 100 } ],
                        "selected_upstream_id": null,
                        "blacklist": { "pins": [], "auto": [] } } } }
    }"#;
}
