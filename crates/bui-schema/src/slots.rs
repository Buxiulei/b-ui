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
use crate::model::{NodeParams, Ports, Residential, Slot, State, User, DEFAULT_GROUP};
use crate::nodes::{self, NodeKind};
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

/// 两份期望态之间，**必须重新获取订阅**的用户名（升序、只有用户名）。
///
/// HY2 住宅节点的端口（`hy2_resi + 槽序号`）与端口跳跃区间（`mport=`，把 `hy2_resi_hop`
/// 按 [`slot_span`] 等分给各槽）都写死在用户手里那份订阅里。客户端不会主动发现它们变了，
/// 要等下一次订阅更新（`bui-c` 的每日 timer、v2rayN 的定时更新，或人工重新获取）；在那
/// 之前这个节点连不上。区间被重切时更隐蔽：挪给了别的槽的那一段会打到**另一个 hysteria
/// 进程**，它不认这条 QUIC 连接（鉴权 URL 全实例共用，重握手后就从别的槽的 IP 出去）。
/// 所以池成员一变就得把名单打给操作者。
///
/// 判据是「手里那份订阅还能不能用」，不是「有没有变」：端口必须一样，且旧的跳跃区间整段
/// 仍落在本槽的新区间里 —— 区间被切成前缀时（`41000-43999` ⊆ `41000-45499`）每个端口照旧
/// 打到本槽实例，这种人不上名单。节点取自 [`nodes::nodes_for`]（订阅内容的唯一真源），
/// 所以没开 hysteria2 协议、没有住宅权益、权益指向的分组不存在的用户自动不在名单里。
///
/// `before` / `after` 必须是**同一次写入**的前后两份期望态（`after` 是 [`sync_slots`] 与
/// [`migrate_unassigned`] 跑完之后的），否则名单是过期的。
pub fn resubscribe_impact(before: &State, after: &State) -> Vec<String> {
    let mut out = Vec::new();
    for u in &before.users {
        let Some(old) = hy2_resi_node(u, &before.node, &before.residential) else {
            continue;
        };
        let Some(new) = after
            .users
            .iter()
            .find(|x| x.user_id == u.user_id)
            .and_then(|a| hy2_resi_node(a, &after.node, &after.residential))
        else {
            continue;
        };
        if !still_usable(&old, &new) {
            out.push(u.username.clone());
        }
    }
    // 输出要确定性。**不含凭据、不含订阅 token。**
    out.sort_unstable();
    out
}

/// 用户订阅里的 HY2 住宅节点；没有这个节点 ⇒ `None`。
///
/// 真源是 [`nodes::nodes_for`]：只开了 vless-reality 的住宅用户订阅里压根没有 HY2 住宅
/// 节点（Reality 住宅固定 `:10002`，换槽在服务端的 xray 路由里完成，订阅内容不变），
/// 改池动不到他们。
fn hy2_resi_node(u: &User, node: &NodeParams, r: &Residential) -> Option<nodes::Node> {
    nodes::nodes_for(u, node, r)
        .into_iter()
        .find(|n| n.kind == NodeKind::Hy2Residential)
}

/// 手里那份 `old` 节点在新期望态下还连得上吗。
fn still_usable(old: &nodes::Node, new: &nodes::Node) -> bool {
    old.port == new.port
        && match (old.hop, new.hop) {
            // 旧订阅没有 mport ⇒ 只用基础端口，端口相同就还能用
            (None, _) => true,
            (Some(_), None) => false,
            // 旧区间 ⊆ 新区间：每个端口照旧落在本槽实例上
            (Some((a, b)), Some((c, d))) => c <= a && b <= d,
        }
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

    /// 某用户订阅里 HY2 住宅节点的 `(端口, 跳跃区间)`。
    fn node_of(s: &State, name: &str) -> (u16, Option<(u16, u16)>) {
        let u = s.users.iter().find(|u| u.username == name).unwrap();
        let n = hy2_resi_node(u, &s.node, &s.residential).unwrap();
        (n.port, n.hop)
    }

    /// 删掉序号最高的槽：那个槽上的人端口变了，**存活槽里区间被重切的人也一样要重取订阅**。
    /// 槽位空间 3 → 2 让槽 1 的区间从 `44000-46999` 挪成 `45500-50000`，u2 手里那份订阅
    /// 会把 44000-45499 的包打到槽 0 那个**独立进程**上（它不认 u2 的连接）；槽 0 的 u1
    /// 反而无害 —— 他的旧区间是新区间的前缀子集，每个端口照旧落在本槽实例。
    #[test]
    fn removing_the_top_slot_also_lists_whoever_had_his_hop_range_resliced() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s); // u1 → 槽 0，u2 → 槽 1，u3 → 槽 2
        let after = after_remove(&s, Uuid::from_u128(3));
        assert_eq!(
            resubscribe_impact(&s, &after),
            vec!["u2".to_string(), "u3".to_string()]
        );
        // 三个人各自变了什么：只有 u1 那份订阅还能照旧用
        assert_eq!(node_of(&s, "u1"), (40000, Some((41000, 43999))));
        assert_eq!(node_of(&after, "u1"), (40000, Some((41000, 45499))));
        assert_eq!(node_of(&s, "u2"), (40001, Some((44000, 46999))));
        assert_eq!(node_of(&after, "u2"), (40001, Some((45500, 50000))));
        assert_eq!(node_of(&s, "u3"), (40002, Some((47000, 50000))));
        assert_eq!(node_of(&after, "u3"), (40000, Some((41000, 45499))));
    }

    /// 被删的槽上**一个用户都没有**也要打名单：槽位空间一缩，存活槽的区间就被重切。
    /// 这条是「只算端口」那版实现的漏报 —— 它会回一句「没有用户受影响」。
    #[test]
    fn removing_an_empty_top_slot_still_lists_the_survivors_whose_range_moved() {
        let mut s = state(3, 2);
        migrate_unassigned(&mut s); // u1 → 槽 0，u2 → 槽 1；槽 2 空着
        let after = after_remove(&s, Uuid::from_u128(3));
        assert_eq!(
            resubscribe_impact(&s, &after),
            vec!["u2".to_string()],
            "u2 的区间 44000-46999 → 45500-50000（不是子集）；u1 的 41000-43999 是新区间前缀"
        );
    }

    /// 删 0 号槽：只列手里那份订阅**真的**不能用了的人。不变量 3 把槽 1 搬到 0（端口下移
    /// ⇒ 上名单），而被删槽上的 u1 常常被重新分配回序号 0（`40000` → `40000`，区间也没动
    /// ⇒ 不上名单）—— 照「被删槽上的人」列名单就是误报。
    #[test]
    fn removing_slot_zero_lists_only_the_users_whose_node_really_moves() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s); // u1 → 槽 0，u2 → 槽 1，u3 → 槽 2
        let after = after_remove(&s, Uuid::from_u128(1));
        assert_eq!(
            resubscribe_impact(&s, &after),
            vec!["u2".to_string()],
            "槽 1（u2）被搬到 0：40001 → 40000"
        );
        // 名单与删后逐用户核对的结果逐字一致
        assert_eq!(node_of(&after, "u1"), node_of(&s, "u1"));
        assert_eq!(node_of(&after, "u2"), (40000, Some((41000, 43999))));
        assert_eq!(node_of(&after, "u3"), node_of(&s, "u3"));
    }

    /// 删 0 号槽可以同时打到两批人：被重新分配到别的槽的，以及槽序号被搬到 0 的。
    #[test]
    fn removing_slot_zero_can_hit_both_the_reassigned_and_the_moved() {
        let mut s = state(3, 5);
        migrate_unassigned(&mut s); // 槽 0：u1、u4；槽 1：u2、u5；槽 2：u3
        assert_eq!(load(&s), vec![2, 2, 1]);
        let after = after_remove(&s, Uuid::from_u128(1));
        assert_eq!(
            resubscribe_impact(&s, &after),
            vec!["u1".to_string(), "u2".to_string(), "u5".to_string()],
            "u1 被重新分配到槽 2（40000 → 40002）、u2 与 u5 随槽 1 被搬到 0；\
             u4 落回序号 0，端口与区间都没动"
        );
        assert_eq!(node_of(&after, "u4"), node_of(&s, "u4"));
    }

    /// 只开 vless-reality 的住宅用户永不进名单：他的订阅里压根没有 HY2 住宅节点
    /// （Reality 住宅固定 `:10002`，换槽在服务端的 xray 路由里完成）。
    #[test]
    fn a_reality_only_user_is_never_listed() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s);
        s.users[1].entitlements.protocols = vec![crate::model::Protocol::Reality];
        // 删槽 1（上面只有 u2）：槽 0 / 槽 2 的序号与区间都不动，u2 自己没有 HY2 住宅节点
        assert!(resubscribe_impact(&s, &after_remove(&s, Uuid::from_u128(2))).is_empty());
    }

    /// 空名单：期望态没变，以及池里只剩一条时删掉它（槽位表清空 ⇒ 渲染退回单槽，
    /// `40000` + 完整区间照旧有人听）。
    #[test]
    fn nobody_is_listed_when_the_subscription_still_works() {
        let mut s = state(3, 3);
        migrate_unassigned(&mut s);
        assert!(resubscribe_impact(&s, &s).is_empty(), "同一份期望态 ⇒ 空");
        let mut one = state(1, 2);
        migrate_unassigned(&mut one);
        let after = after_remove(&one, Uuid::from_u128(1));
        assert!(after.residential.slots.is_empty());
        assert_eq!(node_of(&after, "u1"), (40000, Some((41000, 50000))));
        assert!(resubscribe_impact(&one, &after).is_empty());
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
