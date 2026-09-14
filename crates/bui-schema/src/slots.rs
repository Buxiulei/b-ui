//! IP 池的槽位（spec §5.6）：端口资源与分配器，全是纯函数。
//!
//! 槽位语义：池内每个上游 IP 是一个槽，键是上游 uuid，序号 [`Slot::index`] 决定它的
//! 全部端口。多个用户共用一个槽（用户数 > IP 数），同一用户稳定走同一个 IP（粘性）。
//!
//! 三条不变量，全靠 [`sync_slots`] 维护：
//! 1. 每条上游恰好一个槽，每个槽恰好指向一条在池里的上游；
//! 2. 序号取 `0..MAX_SLOTS` 的最小空闲值；
//! 3. **池非空 ⇒ 序号 0 的槽存在**（`40000` / `2080` / `9998` /
//!    `hysteria-residential.service` 是 v3 兼容面，不许悬空）。
use crate::model::{Ports, Residential, Slot, State, User, DEFAULT_GROUP};
use serde::Serialize;
use uuid::Uuid;

/// relay 每槽一个 socks 入站的基准端口：槽 i 监听 `127.0.0.1:(2080 + i)`。
pub const RELAY_SOCKS_BASE: u16 = 2080;
/// 住宅 hysteria 实例 `trafficStats` 的基准端口：槽 i 用 `9998 - i`。
/// **只能递减**：`9999` 是直连实例（`HY2_STATS_PORT_DIRECT`）。
pub const HY2_STATS_RESI_BASE: u16 = 9998;
/// 槽位上限，与住宅池上限同值（D5）。
pub const MAX_SLOTS: u16 = 8;

/// 一个槽位的全部端口资源。每一项都是序号的纯函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRes {
    pub index: u16,
    /// relay 的 socks 入站端口，也是该槽 hysteria / xray 出站的目标
    pub relay_port: u16,
    /// 该槽 hysteria 住宅实例的监听端口
    pub hy2_port: u16,
    /// 该槽 hysteria 住宅实例的 `trafficStats.listen` 端口
    pub stats_port: u16,
    /// 该槽分到的端口跳跃区间（闭区间）
    pub hop: (u16, u16),
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

/// 槽位空间的宽度 = 最高序号 + 1，也就是「把跳跃区间切成几片」（D3）。
/// 空池按 1 算 ⇒ 槽 0 拿到完整区间。
pub fn slot_span(r: &Residential) -> u16 {
    r.slots
        .iter()
        .map(|s| s.index + 1)
        .max()
        .unwrap_or(1)
        .clamp(1, MAX_SLOTS)
}

/// 把闭区间 `range` 切成 `span` 段连续切片，返回第 `index` 段；最后一段吃掉余数。
/// `span <= 1`、区间退化、或区间比 `span` 还短时一律返回整个区间（**绝不返回空区间**：
/// 空的 `mport=` 会让客户端连不上）。
pub fn hop_slice(range: (u16, u16), index: u16, span: u16) -> (u16, u16) {
    let (start, end) = range;
    let span = span.clamp(1, MAX_SLOTS);
    let index = index.min(span - 1);
    if span == 1 || end <= start {
        return (start, end);
    }
    let total = u32::from(end - start) + 1;
    let width = total / u32::from(span);
    if width == 0 {
        return (start, end);
    }
    let lo = u32::from(start) + width * u32::from(index);
    let hi = if index + 1 >= span {
        u32::from(end)
    } else {
        lo + width - 1
    };
    (lo as u16, hi as u16)
}

/// 槽 `index` 在 `span` 宽的槽位空间里的端口资源。
pub fn resources(ports: &Ports, index: u16, span: u16) -> SlotRes {
    let span = span.clamp(1, MAX_SLOTS);
    let index = index.min(span - 1);
    SlotRes {
        index,
        relay_port: RELAY_SOCKS_BASE + index,
        hy2_port: ports.hy2_resi + index,
        stats_port: HY2_STATS_RESI_BASE - index,
        hop: hop_slice(ports.hy2_resi_hop, index, span),
    }
}

/// 同 [`resources`]，但槽位空间直接从期望态取（调用方少算一次 [`slot_span`]）。
pub fn resources_of(ports: &Ports, r: &Residential, index: u16) -> SlotRes {
    resources(ports, index, slot_span(r))
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
    // 最小的那个槽搬到 0 —— 代价是那一槽的用户端口下移一次，但 40000 / 2080 /
    // 9998 / hysteria-residential.service 这四个兼容面绝不能悬空。
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

/// 删一条上游会让哪些用户的 HY2 住宅端口变化（按成因分两组，**只有用户名**）。
///
/// 线上形态就是 derive 出来的两个数组（面板回包的 `port_changed`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PortChangeImpact {
    /// 被删槽上的用户：那个 `hy2_resi + 序号` 删完没人监听，他们会被重分配到别的槽
    pub removed_slot: Vec<String>,
    /// 只在**删 0 号槽**时非空：被不变量 3 搬到序号 0 的那一槽的用户，端口随之下移
    pub moved_to_zero: Vec<String>,
}

impl PortChangeImpact {
    pub fn is_empty(&self) -> bool {
        self.removed_slot.is_empty() && self.moved_to_zero.is_empty()
    }
}

/// 删上游**之前**算出 [`PortChangeImpact`]，`upstream_id` 是要删的那条。
///
/// HY2 住宅节点的端口 = `hy2_resi + 槽序号`，已经写进用户手里的订阅（客户端快照），
/// 客户端没有任何自愈手段（连「订阅过期」都检测不到）；缩池只有显式删上游这一条路，
/// 所以受影响名单在这里算、由调用方打给操作者。
///
/// 两组的成因不同，文案要分开说：
/// - [`PortChangeImpact::removed_slot`]：被删槽上的用户，端口删完没人听；
/// - [`PortChangeImpact::moved_to_zero`]：删 0 号槽时，[`sync_slots`] 的不变量 3 会把现存
///   序号最小的槽搬到 0（保住 `hy2_resi` 与 `hysteria-residential.service` 不悬空），
///   那一槽的用户端口跟着下移 —— 所以删 0 号槽会同时打到两批人。
///
/// 槽位表只剩这一个槽时返回空：清空后渲染退回单槽（[`indices`] 给 `[0]`），`hy2_resi`
/// 照旧有人监听，谁的端口都不变（出口回落 fail-open 直连，是另一回事）。
pub fn port_change_impact(s: &State, upstream_id: Uuid) -> PortChangeImpact {
    let empty = PortChangeImpact::default();
    if s.residential.slots.len() <= 1 {
        return empty;
    }
    let Some(removed) = s
        .residential
        .slots
        .iter()
        .find(|x| x.upstream_id == upstream_id)
    else {
        return empty;
    };
    PortChangeImpact {
        removed_slot: usernames_of_slot(s, upstream_id),
        moved_to_zero: if removed.index == 0 {
            sorted(&s.residential)
                .into_iter()
                .find(|x| x.upstream_id != upstream_id)
                .map(|next| usernames_of_slot(s, next.upstream_id))
                .unwrap_or_default()
        } else {
            Vec::new()
        },
    }
}

/// 某个槽上的用户名，字典序（输出要确定性）。**不含凭据、不含订阅 token。**
fn usernames_of_slot(s: &State, slot_upstream_id: Uuid) -> Vec<String> {
    let mut v: Vec<String> = users_of_slot(s, slot_upstream_id)
        .iter()
        .map(|u| u.username.clone())
        .collect();
    v.sort_unstable();
    v
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

    fn ports() -> Ports {
        serde_json::from_str(
            r#"{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],
                "reality_direct":10001,"reality_resi":10002,"admin":8080}"#,
        )
        .unwrap()
    }

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
        assert_eq!(HY2_STATS_RESI_BASE, 9998);
    }

    /// 单槽（含空池）必须与 v3 单实例逐字等价 —— golden 的生命线。
    #[test]
    fn a_single_slot_keeps_the_v3_ports_exactly() {
        let empty = Residential::default();
        assert_eq!(indices(&empty), vec![0]);
        assert_eq!(slot_span(&empty), 1);
        assert_eq!(fallback_index(&empty), 0);
        let r = resources_of(&ports(), &empty, 0);
        assert_eq!(
            r,
            SlotRes {
                index: 0,
                relay_port: 2080,
                hy2_port: 40000,
                stats_port: 9998,
                hop: (41000, 50000),
            }
        );
    }

    #[test]
    fn three_slots_split_the_hop_range_into_contiguous_slices() {
        let r = resi(&[0, 1, 2]);
        assert_eq!(slot_span(&r), 3);
        let p = ports();
        let s: Vec<SlotRes> = (0..3).map(|i| resources_of(&p, &r, i)).collect();
        assert_eq!(s[0].hop, (41000, 43999));
        assert_eq!(s[1].hop, (44000, 46999));
        // 最后一段吃掉余数，右端必须正好落在 50000（否则客户端跳到没人监听的端口）
        assert_eq!(s[2].hop, (47000, 50000));
        assert_eq!(
            s.iter().map(|x| x.hy2_port).collect::<Vec<_>>(),
            vec![40000, 40001, 40002]
        );
        assert_eq!(
            s.iter().map(|x| x.relay_port).collect::<Vec<_>>(),
            vec![2080, 2081, 2082]
        );
        assert_eq!(
            s.iter().map(|x| x.stats_port).collect::<Vec<_>>(),
            vec![9998, 9997, 9996],
            "9999 归直连实例，住宅只能往下走"
        );
        // 切片必须首尾相接、不重叠
        assert_eq!(s[0].hop.1 + 1, s[1].hop.0);
        assert_eq!(s[1].hop.1 + 1, s[2].hop.0);
    }

    /// D3：序号有空洞时闲置那一片，**不重切** —— 否则删一条上游就要全员刷订阅。
    #[test]
    fn a_hole_in_the_index_space_leaves_its_slice_idle() {
        let r = resi(&[0, 2]);
        assert_eq!(slot_span(&r), 3, "空间宽度按最高序号算，不按槽位个数");
        let p = ports();
        assert_eq!(resources_of(&p, &r, 0).hop, (41000, 43999), "槽 0 的片没变");
        assert_eq!(resources_of(&p, &r, 2).hop, (47000, 50000));
    }

    #[test]
    fn hop_slice_never_returns_an_empty_range() {
        // 区间比槽数还短：全部共用整个区间
        assert_eq!(hop_slice((41000, 41001), 1, 8), (41000, 41001));
        // 退化区间
        assert_eq!(hop_slice((41000, 41000), 0, 3), (41000, 41000));
        // 越界序号夹到最后一段
        assert_eq!(hop_slice((41000, 50000), 9, 2), (45500, 50000));
        for span in 1..=MAX_SLOTS {
            for i in 0..span {
                let (lo, hi) = hop_slice((41000, 50000), i, span);
                assert!(lo <= hi, "span={span} i={i} 切出了空区间");
                assert!((41000..=50000).contains(&lo) && (41000..=50000).contains(&hi));
            }
        }
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

    /// 删非 0 号槽：只有那个槽上的用户端口会变。
    #[test]
    fn port_change_impact_lists_only_the_removed_slots_users() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s);
        let i = port_change_impact(&s, Uuid::from_u128(2)); // 槽 1，上面是 u2
        assert_eq!(i.removed_slot, vec!["u2".to_string()]);
        assert!(
            i.moved_to_zero.is_empty(),
            "删非 0 号槽不触发不变量 3，没人被搬"
        );
        assert!(!i.is_empty());
    }

    /// 删 0 号槽同时打到两批人：被删槽的用户，以及被不变量 3 搬到 0 的那一槽的用户。
    #[test]
    fn removing_slot_zero_also_hits_the_slot_that_gets_moved_to_zero() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s);
        let gone = Uuid::from_u128(1); // 槽 0，上面是 u1
        let i = port_change_impact(&s, gone);
        assert_eq!(i.removed_slot, vec!["u1".to_string()]);
        assert_eq!(
            i.moved_to_zero,
            vec!["u2".to_string()],
            "槽 1（u2）是现存序号最小的那个，它会被搬到 0"
        );
        // 预测必须与 sync_slots 的实际行为一致：真删一次，看谁落在了序号 0
        let g = s.residential.groups.get_mut(DEFAULT_GROUP).unwrap();
        g.upstreams.retain(|u| u.id != gone);
        sync_slots(&mut s.residential);
        assert_eq!(sorted(&s.residential)[0].upstream_id, Uuid::from_u128(2));
    }

    /// 空名单的三种来源：槽上没有用户、槽位表只剩一个、uuid 不在槽位表里。
    #[test]
    fn port_change_impact_is_empty_when_nobody_moves() {
        let mut s = state(3, 2);
        migrate_unassigned(&mut s); // u1 → 槽 0，u2 → 槽 1；槽 2 空着
        assert!(
            port_change_impact(&s, Uuid::from_u128(3)).is_empty(),
            "槽上没有用户 ⇒ 不打名单"
        );
        assert!(
            port_change_impact(&s, Uuid::from_u128(99)).is_empty(),
            "不在槽位表里的 uuid ⇒ 空"
        );
        // 池里只剩一条：删完槽位表清空，渲染退回单槽，hy2_resi 照旧有人听 ⇒ 端口不变
        let mut one = state(1, 2);
        migrate_unassigned(&mut one);
        assert_eq!(one.residential.slots.len(), 1);
        assert!(port_change_impact(&one, Uuid::from_u128(1)).is_empty());
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
