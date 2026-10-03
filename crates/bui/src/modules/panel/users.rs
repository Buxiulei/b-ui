//! 用户域：面板投影（v3 兼容）、CRUD 域逻辑、限额判定、同步反应器。
//!
//! 移植参照：`web/server.js:1157-1226`（`handleManage` 的 create/update/delete）、
//! `web/server.js:1945-2067`（`/api/users` 的 GET/POST/PUT/DELETE）、
//! `web/server.js:246-257`（两个校验函数）、`web/server.js:1146-1155`（`checkUserLimits`）。

use super::snapshot::{self, Snapshot};
use super::xray::{rmu_args, xray_program};
use super::{Shared, TxRx, XRAY_INBOUND_TAGS};
use crate::api::Event;
use crate::reconcile::DaemonCtx;
use crate::state::store::Store;
use bui_schema::egress::{access_for, AuthorizedEgress, RequestedEgress};
use bui_schema::model::{
    Billing, Credentials, Entitlements, NodeParams, PortalAuth, Protocol, Residential,
    ResidentialEntitlement, State, TrafficLimit, Usage, User, DEFAULT_GROUP,
};
use bui_schema::nodes::{nodes_for, NodeKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

pub const GIB: u64 = 1_073_741_824;
pub const SYNC_INTERVAL_SECS: u64 = 60;
/// 用户同步有失败项时那一行日志的固定文案。日志哨兵（`modules::sentinel::signature`）按它加
/// tracing-journald 的 `F_ERROR` 字段认「xray gRPC 连续不可用」——改文案会让哨兵失明，
/// 所以它是常量，哨兵的测试直接引用它。
pub const USER_SYNC_FAILED_LOG: &str = "用户同步有失败项，下一轮安全网会重试";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelLimits {
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(rename = "trafficLimit", skip_serializing_if = "Option::is_none")]
    pub traffic_limit: Option<u64>,
    #[serde(rename = "monthlyLimit", skip_serializing_if = "Option::is_none")]
    pub monthly_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUsage {
    pub total: u64,
    pub monthly: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUser {
    pub username: String,
    pub protocol: &'static str,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub limits: PanelLimits,
    pub usage: PanelUsage,
    pub password: String,
    pub uuid: Uuid,
    /// 该用户四个免鉴权订阅端点的路径末段（`User::sub_token`，2026-09-14 裁决）：
    /// 前端据此拼订阅链接，不再拿用户名拼。老 `state.json` 还没补齐 token 时不出现
    /// （补齐由守护进程启动时的 [`backfill_sub_tokens`] 做，所以这一档只在那之前可见）。
    #[serde(rename = "subToken", skip_serializing_if = "Option::is_none")]
    pub sub_token: Option<String>,
    /// 单协议账户当前可交付的规范节点 URI，直接来自同一份授权节点与订阅格式器。
    /// 融合账户使用订阅 token 地址；不可交付时省略，不让前端猜凭据、端口或 SNI。
    #[serde(rename = "nodeUri", skip_serializing_if = "Option::is_none")]
    pub node_uri: Option<String>,
    pub sni: String,
    pub residential: bool,
    /// 当前住宅通路不可交付的原因；不会含凭据或猜测的出口地址。
    #[serde(
        rename = "residentialUnavailable",
        skip_serializing_if = "Option::is_none"
    )]
    pub residential_unavailable: Option<String>,
    /// 该用户所在的 IP 槽位序号（spec §5.6；没有住宅权益 ⇒ `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u16>,
    /// 该槽位 IP 的出口地址（未体检过 ⇒ `None`）
    #[serde(rename = "slotIp", skip_serializing_if = "Option::is_none")]
    pub slot_ip: Option<String>,
    /// 该用户订阅里 HY2 住宅节点的端口
    #[serde(rename = "slotPort", skip_serializing_if = "Option::is_none")]
    pub slot_port: Option<u16>,
    /// 该用户订阅里 HY2 住宅节点的跳跃区间（闭区间）
    #[serde(rename = "slotHop", skip_serializing_if = "Option::is_none")]
    pub slot_hop: Option<(u16, u16)>,
    /// 该用户住宅 HY2 的**门位**：`slot-<i>-out`（能出网、走第 i 槽的住宅 IP）/ `deny`
    /// （握手照旧成功、**每个请求被拒**，spec §6 的到期 / 封禁语义）/ `未分配`（还没拿到
    /// 凭据 ⇒ 压根没有门，订阅里也没有住宅 HY2 节点）。
    ///
    /// 没有住宅 hysteria2 权益的用户不出现这个字段（`None`）。值与门位收敛**同一份口径**
    /// （[`gates::expected`](super::gates::expected)）—— 面板与 CLI 各算一遍必然漂移成
    /// 「面板说放行、内核里是 deny」。
    #[serde(rename = "hy2ResiGate", skip_serializing_if = "Option::is_none")]
    pub hy2_resi_gate: Option<String>,
    pub disabled: bool,
    pub blocked: bool,
    #[serde(rename = "managedProfile", skip_serializing_if = "Option::is_none")]
    pub managed_profile: Option<ManagedProfileView>,
}

/// v3 的 `protocol` 字段，映射方向与 `bui_schema::v3::import` 相反（spec §4.1）。
pub fn protocol_label(e: &Entitlements) -> &'static str {
    let hy2 = e.protocols.contains(&Protocol::Hysteria2);
    let reality = e.protocols.contains(&Protocol::Reality);
    match (hy2, reality) {
        (true, true) => "fusion",
        (false, true) => "vless-reality",
        // 空权益按 v3 的默认值（`handleManage` 的 `protocol || "hysteria2"`）
        _ => "hysteria2",
    }
}

/// 面板投影。`gates` 是 [`gates::expected`](super::gates::expected) 的结果
/// （凭据 `id` → 出站 tag）：**住宅 HY2 的门位只有那一处口径**，面板照它显示，不自己重算
/// 「谁该放行」。传空表 ⇒ 有凭据的人显示 `未分配`（等下一轮收敛把门位算出来）。
pub fn project(
    u: &User,
    node: &NodeParams,
    resi: &Residential,
    blocked: &BTreeSet<Uuid>,
    gates: &BTreeMap<String, String>,
) -> PanelUser {
    // 使用用户实际拥有的协议检查精确绑定；fusion 的 REALITY 不依赖 HY2 凭据预留。
    // 暂时不可交付时不猜槽 0 / 池首 IP，面板显示明确原因。
    let residential_access = u.entitlements.residential.as_ref().map(|_| {
        let protocol = if u.entitlements.protocols.contains(&Protocol::Reality) {
            Protocol::Reality
        } else {
            Protocol::Hysteria2
        };
        access_for(
            u,
            resi,
            protocol,
            RequestedEgress::RequiredResidential,
            blocked.contains(&u.user_id),
        )
    });
    let residential_unavailable = residential_access
        .as_ref()
        .and_then(|a| a.as_ref().err())
        .map(ToString::to_string);
    let single_kind = match (
        u.entitlements.protocols.contains(&Protocol::Hysteria2),
        u.entitlements.protocols.contains(&Protocol::Reality),
        u.entitlements.residential.is_some(),
    ) {
        (true, false, true) => Some(NodeKind::Hy2Residential),
        (true, false, false) => Some(NodeKind::Hy2Direct),
        (false, true, true) => Some(NodeKind::RealityResidential),
        (false, true, false) => Some(NodeKind::RealityDirect),
        _ => None,
    };
    let node_uri =
        if u.disabled || blocked.contains(&u.user_id) || residential_unavailable.is_some() {
            None
        } else {
            single_kind.and_then(|kind| {
                nodes_for(u, node, resi)
                    .into_iter()
                    .find(|node| node.kind == kind)
                    .map(|node| bui_schema::render::subscription::node_uri(&node, &u.username))
            })
        };
    let slot = residential_access.and_then(Result::ok).and_then(|access| {
        let AuthorizedEgress::Residential(binding) = access else {
            return None;
        };
        let ip = resi
            .default_group()
            .and_then(|g| g.upstreams.iter().find(|x| x.id == binding.upstream_id))
            .and_then(|x| x.verified.as_ref())
            .map(|v| v.ip.clone());
        Some((binding.slot_index, ip))
    });
    // 门位：有住宅 hysteria2 权益才有这一档（口径同 `gates::expected`，判据是
    // `bui_schema::hy2pool::is_resi_hy2`）。
    // 持有的凭据 id 不在 `gates` 里（悬空指针、或池刚扩容还没收敛）一律按「未分配」显示，
    // 不猜一个放行值。
    let hy2_resi_gate = bui_schema::hy2pool::is_resi_hy2(u, resi).then(|| {
        u.credentials
            .hy2_resi_cred
            .as_deref()
            .and_then(|id| gates.get(id).cloned())
            .unwrap_or_else(|| "未分配".to_string())
    });
    PanelUser {
        username: u.username.clone(),
        protocol: protocol_label(&u.entitlements),
        created_at: u.created_at.clone(),
        limits: PanelLimits {
            expires_at: u.entitlements.expires_at.clone(),
            traffic_limit: u.entitlements.traffic_limit.total_bytes,
            monthly_limit: u.entitlements.traffic_limit.monthly_bytes,
        },
        usage: PanelUsage {
            total: u.usage.total_bytes,
            monthly: if u.usage.month_key.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([(u.usage.month_key.clone(), u.usage.monthly_bytes)])
            },
        },
        password: u.credentials.hy2_password.clone(),
        uuid: u.credentials.vless_uuid,
        sub_token: u.sub_token.clone(),
        node_uri,
        // v4 没有 per-user sni：面板与订阅都用全局 REALITY 伪装域（v3.5.13 的修复口径）
        sni: node.reality.sni().to_string(),
        residential: u.entitlements.residential.is_some(),
        residential_unavailable,
        slot: slot.as_ref().map(|(i, _)| *i),
        slot_ip: slot.as_ref().and_then(|(_, ip)| ip.clone()),
        slot_port: slot.as_ref().map(|_| node.ports.hy2_resi),
        slot_hop: slot.as_ref().map(|_| node.ports.hy2_resi_hop),
        hy2_resi_gate,
        disabled: u.disabled,
        blocked: u.disabled || blocked.contains(&u.user_id),
        managed_profile: None,
    }
}

pub fn project_all(state: &State, blocked: &BTreeSet<Uuid>) -> Vec<PanelUser> {
    // 门位算一次给全表用（`expected` 遍历整池，逐用户调会是 O(用户 × 凭据)）
    let gates = super::gates::expected(state, blocked);
    state
        .users
        .iter()
        .map(|u| project(u, &state.node, &state.residential, blocked, &gates))
        .collect()
}

/// Sanitized display projection only; never serialized policy/proof/path or admission input.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedProfileView {
    selected_egress: Option<bui_schema::managed::EgressIdentity>,
    allowed_choices: Vec<bui_schema::managed::EgressIdentity>,
    revision: Option<u64>,
    delivery: &'static str,
    capabilities: ManagedCapabilityView,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedCapabilityView {
    v4_tcp: &'static str,
    v4_udp: &'static str,
    v6_tcp: &'static str,
    v6_udp: &'static str,
}

pub fn project_managed_all(
    state: &State,
    blocked: &BTreeSet<Uuid>,
    now: OffsetDateTime,
) -> Vec<PanelUser> {
    let mut rows = project_all(state, blocked);
    for (row, user) in rows.iter_mut().zip(&state.users) {
        row.managed_profile = Some(managed_profile_view(
            state,
            user,
            blocked,
            now.unix_timestamp(),
        ));
    }
    rows
}

fn managed_profile_view(
    state: &State,
    user: &User,
    blocked: &BTreeSet<Uuid>,
    now: i64,
) -> ManagedProfileView {
    use bui_schema::managed::{allows, EgressIdentity, EvidenceStatus};
    use bui_schema::managed_binding::{path_fingerprint, selection_granted, semantic_stamp};
    let allowed_choices = [EgressIdentity::Vps, EgressIdentity::Residential]
        .into_iter()
        .filter(|identity| selection_granted(user, state, *identity))
        .collect::<Vec<_>>();
    let path = path_fingerprint(state, user).ok();
    let caps = path
        .as_ref()
        .and_then(|p| state.managed_egress_capabilities.get(p))
        .cloned()
        .unwrap_or_default();
    let statuses = [&caps.v4_tcp, &caps.v4_udp, &caps.v6_tcp, &caps.v6_udp];
    let display = statuses.map(|status| match status {
        EvidenceStatus::Unsupported => "unsupported",
        EvidenceStatus::Verified(_) if path.as_ref().is_some_and(|p| allows(status, p, now)) => {
            "verified"
        }
        _ => "unknown",
    });
    let invalid_proof = statuses.iter().any(|status| {
        matches!(status, EvidenceStatus::Verified(_))
            && !path.as_ref().is_some_and(|p| allows(status, p, now))
    });
    let revision = user.managed_profile_revision.filter(|r| *r > 0);
    let delivery = if user.disabled || blocked.contains(&user.user_id) {
        "account_unavailable"
    } else if state
        .users
        .iter()
        .filter(|u| u.user_id == user.user_id)
        .count()
        != 1
    {
        "route_unavailable"
    } else if let Some(identity) = user.managed_egress {
        if !allowed_choices.contains(&identity) {
            "account_unavailable"
        } else if path.is_none()
            || revision.is_none()
            || invalid_proof
            || semantic_stamp(state, user).is_err()
        {
            "route_unavailable"
        } else {
            "available"
        }
    } else {
        "missing_selection"
    };
    ManagedProfileView {
        selected_egress: user.managed_egress,
        allowed_choices,
        revision,
        delivery,
        capabilities: ManagedCapabilityView {
            v4_tcp: display[0],
            v4_udp: display[1],
            v6_tcp: display[2],
            v6_udp: display[3],
        },
    }
}

/// 移植 `web/server.js:246-251`。正则 `^[\p{L}\p{N}_\-.]+$` 用 `char::is_alphanumeric`
/// （Unicode Alphabetic + Nd/Nl/No）等价实现，不引 `regex`。
/// 注意 v3 允许 `..` 这种名字：v4 的用户名从不参与拼路径（路由参数只用来在 `state.users`
/// 里做精确匹配），所以照抄 v3 的规则不引入路径穿越。
pub fn validate_username(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("username 不能为空".into());
    }
    if name.chars().count() > 64 {
        return Err("username 长度不能超过 64 字符".into());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err("username 仅允许字母/数字/中文/下划线/连字符/点".into());
    }
    Ok(())
}

/// 移植 `web/server.js:253-257`。
pub fn validate_password(pw: &str) -> Result<(), String> {
    if pw.is_empty() {
        return Err("password 不能为空".into());
    }
    if pw.chars().count() > 256 {
        return Err("password 长度不能超过 256 字符".into());
    }
    Ok(())
}

pub fn protocols_from_label(label: &str) -> Result<Vec<Protocol>, String> {
    match label {
        "fusion" => Ok(vec![Protocol::Hysteria2, Protocol::Reality]),
        "hysteria2" => Ok(vec![Protocol::Hysteria2]),
        "vless-reality" => Ok(vec![Protocol::Reality]),
        // v3 还有 `vless-ws-tls`，spec §0 已把它删掉：明确报错而不是静默降级
        other => Err(format!(
            "不支持的协议：{other}（可选 fusion / hysteria2 / vless-reality）"
        )),
    }
}

/// 与 v3 的 `crypto.randomBytes(8).toString("hex")` 同形（16 个 hex 字符）。
pub fn random_hy2_password() -> String {
    hex::encode(rand::random::<[u8; 8]>())
}

fn gb_to_bytes(gb: Option<f64>) -> Option<u64> {
    match gb {
        Some(g) if g > 0.0 => Some((g * GIB as f64) as u64),
        _ => None,
    }
}

fn expires_from_days(days: Option<f64>, now: OffsetDateTime) -> Option<String> {
    match days {
        Some(d) if d > 0.0 => Some(crate::util::fmt_rfc3339(
            now + Duration::from_secs_f64(d * 86400.0),
        )),
        _ => None,
    }
}

pub fn new_user(req: &CreateRequest, now: OffsetDateTime) -> Result<User, String> {
    validate_username(&req.username)?;
    if let Some(pw) = &req.password {
        validate_password(pw)?;
    }
    let label = req.protocol.as_deref().unwrap_or("hysteria2");
    let protocols = protocols_from_label(label)?;
    // v3.5.5：`residential` 缺省 true
    let residential = req.residential.unwrap_or(true);
    Ok(User {
        user_id: Uuid::new_v4(),
        username: req.username.clone(),
        note: String::new(),
        created_at: crate::util::fmt_rfc3339(now),
        disabled: false,
        credentials: Credentials {
            hy2_password: req.password.clone().unwrap_or_else(random_hy2_password),
            vless_uuid: Uuid::new_v4(),
            // 住宅凭据由建用户路径在分槽之后分配（spec §3.3）
            hy2_resi_cred: None,
        },
        entitlements: Entitlements {
            protocols,
            // 单协议开住宅只发住宅版（与 v3 订阅逐项等价，口径同 v3 导入）
            direct: bui_schema::v3::direct_entitlement(label, residential),
            residential: residential.then(|| ResidentialEntitlement {
                group_id: DEFAULT_GROUP.to_string(),
                // 分槽在 `create_user` 的那一次 `store.update` 里做（spec §5.6 规则 1）
                slot_id: None,
            }),
            expires_at: expires_from_days(req.days, now),
            traffic_limit: TrafficLimit {
                total_bytes: gb_to_bytes(req.traffic),
                monthly_bytes: gb_to_bytes(req.monthly),
            },
        },
        usage: Usage::default(),
        portal_auth: PortalAuth::default(),
        billing: Billing::default(),
        // 2026-09-14 裁决：建出来就带随机订阅 token（四个免鉴权端点的路径末段）。
        // 装机首用户也走这里（`commands::install::first_user`），所以三条建用户路径里
        // 有两条由这一行负责，第三条是 `bui_schema::v3::user_from_v3`。
        sub_token: Some(bui_schema::sub::new_sub_token()),
        managed_egress: req.managed_egress,
        managed_profile_revision: None,
        legacy_sub_disabled: false,
    })
}

/// 守护进程启动时把订阅 token 补齐（2026-09-14 裁决），形状照
/// `residential::slots::migrate_on_start`：**幂等**、零变更不写盘，所以每次启动无条件调。
///
/// 三条建用户路径都自带 token，所以这里只管「升级上来的老 `state.json`」：
/// - 给每个 `sub_token == None` 的用户生成一个；
/// - **补出过 token** 且全局宽限期还没设过时，把宽限期设成「本次启动 +
///   `sub::LEGACY_SUB_GRACE_DAYS` 天」—— 那些用户手里拿的是用户名链接，不给宽限期
///   等于升级那一刻把他们全掐了。全新装机一个都补不出来 ⇒ 不设宽限期 ⇒ 用户名链接
///   从来不可用；已经设过（v3 导入、或运维自己设的）就不覆盖。
///
/// 返回补齐的用户数。日志**只写个数**：token 是凭据，一个字都不许进 journal。
pub async fn backfill_sub_tokens(store: &Store, now: OffsetDateTime) -> anyhow::Result<usize> {
    let mut filled = 0usize;
    let n = &mut filled;
    store
        .update(|s| {
            for u in s.users.iter_mut().filter(|u| u.sub_token.is_none()) {
                u.sub_token = Some(bui_schema::sub::new_sub_token());
                *n += 1;
            }
            if *n > 0 && s.system.legacy_sub_until.is_none() {
                s.system.legacy_sub_until = Some(bui_schema::sub::legacy_sub_deadline(now));
            }
        })
        .await?;
    if filled > 0 {
        tracing::info!(
            users = filled,
            grace_days = bui_schema::sub::LEGACY_SUB_GRACE_DAYS,
            "已给既有用户补随机订阅 token，旧用户名链接进入宽限期"
        );
    }
    Ok(filled)
}

/// 轮换一个用户的订阅凭据（2026-09-14 裁决，`POST /api/users/{name}/rotate`）：
/// 订阅 token、hy2 密码、vless uuid **同时**换掉，并停用他的「用户名链接」。
///
/// 三样一起换是这个接口的全部意义：泄露的链接里同时有 token（路径）、hy2 明文密码与
/// vless uuid（响应体），只换其中一样等于没换。停用用户名链接则是为了让轮换在全局宽限期
/// （`system.legacy_sub_until`）还没到期时也立刻生效 —— 否则旧链接照样能取到新凭据。
///
/// hy2 密码沿用 [`random_hy2_password`]（16 个 hex 字符，v3 同形），因此鉴权快照一重写
/// 旧密码当场被拒；uuid 的生效靠 [`sync_users`] 的换 uuid 路径（先 RemoveUser 再 AddUser）。
pub fn rotate(u: &mut User) {
    u.sub_token = Some(bui_schema::sub::new_sub_token());
    u.credentials.hy2_password = random_hy2_password();
    u.credentials.vless_uuid = Uuid::new_v4();
    u.legacy_sub_disabled = true;
}

pub fn apply_update(u: &mut User, req: &UpdateRequest, now: OffsetDateTime) -> Result<(), String> {
    if let Some(name) = &req.username {
        validate_username(name)?;
        u.username = name.clone();
    }
    if let Some(pw) = &req.password {
        validate_password(pw)?;
        // 移植 v3：只有 Reality 权益的用户，这个字段改的是 UUID
        if protocol_label(&u.entitlements) == "vless-reality" {
            u.credentials.vless_uuid = Uuid::parse_str(pw)
                .map_err(|_| "password 必须是合法 UUID（该用户只有 Reality 权益）".to_string())?;
        } else {
            u.credentials.hy2_password = pw.clone();
        }
    }
    if req.days.is_some() {
        u.entitlements.expires_at = expires_from_days(req.days, now);
    }
    if req.traffic.is_some() {
        u.entitlements.traffic_limit.total_bytes = gb_to_bytes(req.traffic);
    }
    if req.monthly.is_some() {
        u.entitlements.traffic_limit.monthly_bytes = gb_to_bytes(req.monthly);
    }
    if let Some(d) = req.disabled {
        u.disabled = d;
    }
    if let Some(identity) = req.managed_egress {
        u.managed_egress = Some(identity);
    }
    // req.speed 接受但忽略（决策 D13：spec §0「按用户限速…删除」）
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Disabled,
    Expired,
    TotalExceeded,
    MonthlyExceeded,
}

/// UTC 月键（决策 D7：`RealHost::now()` 是 `now_utc()`）。
pub fn month_key(now: OffsetDateTime) -> String {
    format!("{:04}-{:02}", now.year(), now.month() as u8)
}

/// `extra` 是尚未落盘的内存增量；判定按 `usage + extra`。
pub fn is_blocked(u: &User, extra: TxRx, now: OffsetDateTime) -> Option<BlockReason> {
    if u.disabled {
        return Some(BlockReason::Disabled);
    }
    if let Some(exp) = &u.entitlements.expires_at {
        match crate::util::parse_rfc3339(exp) {
            Some(t) if t > now => {}
            // 解析失败也算到期：与 auth-hook 的 fail-closed 口径一致
            _ => return Some(BlockReason::Expired),
        }
    }
    let extra = extra.total();
    if let Some(limit) = u.entitlements.traffic_limit.total_bytes {
        if u.usage.total_bytes.saturating_add(extra) >= limit {
            return Some(BlockReason::TotalExceeded);
        }
    }
    if let Some(limit) = u.entitlements.traffic_limit.monthly_bytes {
        let base = if u.usage.month_key == month_key(now) {
            u.usage.monthly_bytes
        } else {
            0
        };
        if base.saturating_add(extra) >= limit {
            return Some(BlockReason::MonthlyExceeded);
        }
    }
    None
}

pub fn blocked_set(
    state: &State,
    pending: &BTreeMap<Uuid, TxRx>,
    now: OffsetDateTime,
) -> BTreeSet<Uuid> {
    state
        .users
        .iter()
        .filter(|u| {
            is_blocked(u, pending.get(&u.user_id).copied().unwrap_or_default(), now).is_some()
        })
        .map(|u| u.user_id)
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateRequest {
    /// Explicit administrator intent; never accepts evidence or revision.
    #[serde(default)]
    pub managed_egress: Option<bui_schema::managed::EgressIdentity>,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub days: Option<f64>,
    #[serde(default)]
    pub traffic: Option<f64>,
    #[serde(default)]
    pub monthly: Option<f64>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub residential: Option<bool>,
    /// 接受但忽略：v4 的 REALITY 伪装域全局唯一（决策 D13）
    #[serde(default)]
    pub sni: Option<String>,
    /// 接受但忽略：spec §0「按用户限速（内核不支持）」（决策 D13）
    #[serde(default)]
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRequest {
    /// Explicit administrator intent; never accepts evidence or revision.
    #[serde(default)]
    pub managed_egress: Option<bui_schema::managed::EgressIdentity>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub days: Option<f64>,
    #[serde(default)]
    pub traffic: Option<f64>,
    #[serde(default)]
    pub monthly: Option<f64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncOutcome {
    pub snapshot_written: bool,
    pub added: Vec<Uuid>,
    pub removed: Vec<Uuid>,
    pub newly_blocked: Vec<Uuid>,
    pub errors: Vec<String>,
}

/// 幂等：把「期望态 + 拒绝集合」同步到内核 —— 重写快照 + 对两个 inbound 做 gRPC 差分。
pub async fn sync_users(
    ctx: &DaemonCtx,
    shared: &Shared,
    caller_blocked: &BTreeSet<Uuid>,
) -> SyncOutcome {
    let _turn = shared.sync_guard().await;
    // Keep the actual Store writer fence until all Xray side effects are accounted for.
    // A cancelled RPC can have reached the remote kernel; touched identities survive cancellation.
    let publication = ctx.store.publication_permit().await;
    let state = publication.state().clone();
    let pending = shared.pending().await.clone();
    let mut blocked = blocked_set(&state, &pending, ctx.host.now());
    blocked.extend(caller_blocked);
    let mut out = SyncOutcome::default();
    let snap = Snapshot::from_state(&state, &blocked);
    let path = shared.snapshot_path();
    match snapshot::write_if_changed(&path, &snap) {
        Ok(written) => out.snapshot_written = written,
        Err(e) => out.errors.push(format!("写 auth-snapshot.json 失败：{e}")),
    }
    let host = ctx.host.clone();
    let restarts =
        tokio::task::spawn_blocking(move || host.unit_property("xray", "NRestarts").ok().flatten())
            .await
            .ok()
            .flatten();
    let desired = xray_grants(&state, &blocked);
    let mut live = BTreeMap::new();
    let mut readable = BTreeSet::new();
    for tag in XRAY_INBOUND_TAGS {
        match shared.xray().inbound_users(tag).await {
            Ok(users) => {
                readable.insert(tag);
                live.extend(users.into_iter().map(|(id, vid)| ((tag, id), vid)));
            }
            Err(e) => out
                .errors
                .push(format!("Xray inventory {tag} 读取失败：{e}")),
        }
    }
    let mut want_removed = BTreeSet::new();
    // Revoke denied identities regardless of in-memory success markers: a stale startup
    // configuration can contain a user this daemon never added, including removed protocols.
    for u in &state.users {
        for tag in XRAY_INBOUND_TAGS {
            if !desired.contains_key(&(tag, u.user_id)) {
                want_removed.insert((tag, u.user_id));
            }
        }
    }
    // Running named-user inventory survives daemon restarts; deleted accounts are
    // absent from both State and a fresh Applied ledger, but still must be revoked.
    want_removed.extend(
        live.keys()
            .filter(|key| !desired.contains_key(key))
            .copied(),
    );
    let (to_add, to_remove, grant_set_changed) = {
        let mut applied = shared.applied().await;
        applied.snapshot_sha = Some(snap.sha256());
        if applied.xray_restarts != restarts {
            if applied.xray_restarts.is_some() {
                tracing::info!(?restarts, "xray 重启过，重放 gRPC 用户差分");
            }
            applied.xray_restarts = restarts;
            let previous: Vec<_> = applied.xray_inbound_users.keys().copied().collect();
            applied.xray_touched.extend(previous);
            applied.xray_users.clear();
            applied.xray_inbound_users.clear();
            applied.xray_inbound_removed.clear();
        }
        for key in applied
            .xray_inbound_users
            .keys()
            .chain(applied.xray_touched.iter())
        {
            if !desired.contains_key(key) {
                want_removed.insert(*key);
            }
        }
        let grant_set_changed = desired != applied.xray_inbound_users;
        let to_add: Vec<_> = desired
            .iter()
            .filter(|(key, vid)| {
                readable.contains(key.0)
                    && (applied.xray_inbound_users.get(key) != Some(vid)
                        || live.get(key) != Some(vid))
            })
            .map(|(key, vid)| (*key, *vid))
            .collect();
        let to_remove: Vec<_> = want_removed
            .iter()
            .filter(|key| live.contains_key(key) || !applied.xray_inbound_removed.contains(key))
            .copied()
            .collect();
        // This is an attempt ledger, not success evidence. Record before any awaited RPC.
        for (key, _) in &to_add {
            applied.xray_touched.insert(*key);
            // An in-flight grant invalidates earlier evidence that the same inlet is empty.
            applied.xray_inbound_removed.remove(key);
        }
        out.newly_blocked = blocked.difference(&applied.blocked).copied().collect();
        applied.blocked = blocked.clone();
        (to_add, to_remove, grant_set_changed)
    };
    let mut removed = Vec::new();
    // Close old grants first; a residential grant never keeps the direct inlet authorized.
    for (tag, id) in to_remove {
        match free_the_email(ctx, shared, tag, id).await {
            Ok(()) => removed.push((tag, id)),
            Err(e) => out.errors.push(format!("RemoveUser {tag} 失败：{e}")),
        }
    }
    let mut added = Vec::new();
    let by_id: BTreeMap<_, _> = state.users.iter().map(|u| (u.user_id, u)).collect();
    for ((tag, id), vid) in to_add {
        let user = by_id[&id];
        if !xray_grant_current(ctx, shared, &state, user, tag, caller_blocked).await {
            continue;
        }
        let mut ok = true;
        match shared.xray().inbound_user_uuid(tag, id).await {
            // 挂的就是期望的 uuid ⇒ 目标已达成，不写
            Ok(Some(cur)) if cur == vid => {
                added.push(((tag, id), vid));
                continue;
            }
            // 挂着别的 uuid（轮换、或面板给只有 Reality 权益的用户改 UUID）⇒ 摘掉腾位置：
            // AddUser 撞同名 email 只会报「已存在」，新 uuid 挂不上
            Ok(Some(_)) => {
                if let Err(e) = free_the_email(ctx, shared, tag, id).await {
                    // 位置没腾出来，AddUser 只会再撞一次「已存在」：这一轮不加，
                    // 留给 60 秒安全网重试（幂等）
                    out.errors
                        .push(format!("换 uuid 前 RemoveUser {tag} 失败：{e}"));
                    continue;
                }
            }
            // 位置空着 ⇒ 直接加
            Ok(None) => {}
            // 读不到（xray 掉线、inbound 还没起、或内核老到没这个 RPC）⇒ 退回下面那条
            // 只靠错误文案的老路：AddUser 撞「已存在」就摘掉再加
            Err(e) => {
                tracing::debug!(tag, %id, error = %e, "GetInboundUsers 读不到，退回 AddUser 那一路")
            }
        }
        if !xray_grant_current(ctx, shared, &state, user, tag, caller_blocked).await {
            continue;
        }
        match shared.xray().add_user(tag, id, vid).await {
            Ok(()) => {}
            // xray 说这个 email 已经在 inbound 里 = 位置被占。**绝不**把它记成已达目标：
            // 上面的读失败了才走到这儿，挂着的可能正是要换掉的旧 uuid（吃下这条错误就等于
            // 把旧 uuid 记成新 uuid —— 泄露的旧凭据一直有效到 xray 下次重启，而
            // `render::xray::structural_hash` 剥掉了 clients，对账也不会重启它）。
            Err(e) if email_taken(&e.to_string()) => {
                match free_the_email(ctx, shared, tag, id).await {
                    Err(e2) => {
                        ok = false;
                        out.errors.push(format!(
                            "AddUser {tag} 报已存在、腾位置的 RemoveUser 又失败：{e2}"
                        ));
                    }
                    Ok(()) => {
                        if !xray_grant_current(ctx, shared, &state, user, tag, caller_blocked).await
                        {
                            continue;
                        }
                        if let Err(e2) = shared.xray().add_user(tag, id, vid).await {
                            // 已经摘出去了、又没加回来 ⇒ 这个用户此刻**在 `tag` 上握不了新手**，
                            // 要等 60 秒安全网补回。单独写清楚，别让运维以为只是加不上。
                            ok = false;
                            out.errors.push(format!(
                                "AddUser {tag} 重试失败，该用户已被摘出 {tag}、等下一轮补回：{e2}"
                            ));
                        } else {
                            tracing::info!(tag, %id, "AddUser 报已存在，摘掉旧的重加一次");
                        }
                    }
                }
            }
            Err(e) => {
                ok = false;
                out.errors.push(format!("AddUser {tag} 失败：{e}"));
            }
        }
        if ok {
            added.push(((tag, id), vid));
        }
    }
    // Time and pending quota can advance while RPCs are in flight even with Store publication
    // fenced. Close every grant that became invalid, including users skipped as already applied.
    let pending = shared.pending().await.clone();
    blocked = blocked_set(&state, &pending, ctx.host.now());
    blocked.extend(caller_blocked);
    let mut final_desired = xray_grants(&state, &blocked);
    for (tag, id) in desired
        .keys()
        .filter(|key| !final_desired.contains_key(key))
        .copied()
    {
        match free_the_email(ctx, shared, tag, id).await {
            Ok(()) => removed.push((tag, id)),
            Err(e) => out
                .errors
                .push(format!("授权在同步期间失效，RemoveUser {tag} 失败：{e}")),
        }
    }
    let mut final_snapshot = Snapshot::from_state(&state, &blocked);
    match snapshot::write_if_changed(&path, &final_snapshot) {
        Ok(written) => out.snapshot_written |= written,
        Err(e) => out
            .errors
            .push(format!("更新 auth-snapshot.json 失败：{e}")),
    }
    let mut final_live = BTreeMap::new();
    let mut final_readable = BTreeSet::new();
    let mut verified = BTreeSet::new();
    // Final observation is part of authorization publication: quota and clock can
    // advance during these RPCs too. One repair-and-readback round is bounded; further
    // changes are reported as unsettled, never committed as synchronized authority.
    for attempt in 0..2 {
        final_live.clear();
        final_readable.clear();
        verified.clear();
        for tag in XRAY_INBOUND_TAGS {
            match shared.xray().inbound_users(tag).await {
                Ok(users) => {
                    final_readable.insert(tag);
                    final_live.extend(users.into_iter().map(|(id, vid)| ((tag, id), vid)));
                }
                Err(e) => out
                    .errors
                    .push(format!("Xray inventory {tag} 最终读回失败：{e}")),
            }
        }
        let pending = shared.pending().await.clone();
        let mut observed_blocked = blocked_set(&state, &pending, ctx.host.now());
        observed_blocked.extend(caller_blocked);
        let observed_desired = xray_grants(&state, &observed_blocked);
        blocked = observed_blocked;
        final_snapshot = Snapshot::from_state(&state, &blocked);
        match snapshot::write_if_changed(&path, &final_snapshot) {
            Ok(written) => out.snapshot_written |= written,
            Err(e) => out
                .errors
                .push(format!("更新 auth-snapshot.json 失败：{e}")),
        }
        if observed_desired != final_desired {
            for (tag, id) in final_desired
                .keys()
                .filter(|key| !observed_desired.contains_key(key))
                .copied()
            {
                match free_the_email(ctx, shared, tag, id).await {
                    Ok(()) => removed.push((tag, id)),
                    Err(e) => out.errors.push(format!(
                        "授权在最终读回期间失效，RemoveUser {tag} 失败：{e}"
                    )),
                }
            }
            final_desired = observed_desired;
            if attempt == 1 {
                final_readable.clear();
                out.errors
                    .push("Xray inventory 授权在最终观察期间继续变化，未记为已收敛".into());
                break;
            }
            continue;
        }
        for tag in &final_readable {
            let expected: BTreeMap<_, _> = final_desired
                .iter()
                .filter(|((inbound, _), _)| inbound == tag)
                .map(|((_, id), vid)| (*id, *vid))
                .collect();
            let observed: BTreeMap<_, _> = final_live
                .iter()
                .filter(|((inbound, _), _)| inbound == tag)
                .map(|((_, id), vid)| (*id, *vid))
                .collect();
            if observed == expected {
                verified.insert(*tag);
            } else {
                out.errors
                    .push(format!("Xray inventory {tag} 未达到当前授权集合"));
            }
        }
        break;
    }
    {
        let mut applied = shared.applied().await;
        applied.snapshot_sha = Some(final_snapshot.sha256());
        let added_ids: BTreeSet<_> = added.iter().map(|((_, id), _)| *id).collect();
        let removed_ids: BTreeSet<_> = removed.iter().map(|(_, id)| *id).collect();
        for (key, vid) in added {
            if final_desired.get(&key) == Some(&vid) {
                applied.xray_inbound_users.insert(key, vid);
                applied.xray_inbound_removed.remove(&key);
            }
        }
        for key in removed {
            applied.xray_inbound_users.remove(&key);
            applied.xray_inbound_removed.insert(key);
            applied.xray_touched.remove(&key);
        }
        // Follow final kernel truth: an RPC acknowledgement is insufficient evidence
        // after reload, and cannot hide an unknown user whose removal failed.
        applied
            .xray_inbound_users
            .retain(|key, vid| !final_readable.contains(key.0) || final_live.get(key) == Some(vid));
        for (key, vid) in &final_live {
            if final_desired.get(key) == Some(vid) {
                applied.xray_inbound_users.insert(*key, *vid);
                applied.xray_inbound_removed.remove(key);
            }
        }
        applied.xray_users = state
            .users
            .iter()
            .filter(|u| {
                let mut has_grant = false;
                for tag in XRAY_INBOUND_TAGS {
                    if !verified.contains(tag) {
                        return false;
                    }
                    let key = (tag, u.user_id);
                    if let Some(vid) = final_desired.get(&key) {
                        has_grant = true;
                        if applied.xray_inbound_users.get(&key) != Some(vid) {
                            return false;
                        }
                    } else if !applied.xray_inbound_removed.contains(&key) {
                        return false;
                    }
                }
                has_grant
            })
            .map(|u| (u.user_id, u.credentials.vless_uuid))
            .collect();
        out.added = added_ids
            .into_iter()
            .filter(|id| applied.xray_users.contains_key(id))
            .collect();
        out.removed = removed_ids
            .into_iter()
            .filter(|id| {
                XRAY_INBOUND_TAGS
                    .into_iter()
                    .filter(|tag| !final_desired.contains_key(&(*tag, *id)))
                    .all(|tag| applied.xray_inbound_removed.contains(&(tag, *id)))
            })
            .collect();
        out.newly_blocked
            .extend(blocked.difference(&applied.blocked).copied());
        out.newly_blocked.sort_unstable();
        out.newly_blocked.dedup();
        applied.blocked = blocked.clone();
        let known: BTreeSet<_> = state.users.iter().map(|u| u.user_id).collect();
        applied
            .xray_inbound_removed
            .retain(|(_, id)| known.contains(id));
    }
    drop(publication); // Gate convergence takes its own Store fence; never nest it.
    if grant_set_changed || desired != final_desired || !out.removed.is_empty() {
        // Authorization revocation changes slot routes even when an RPC failure prevents
        // the aggregate user count from being reported as fully synchronized.
        crate::modules::residential::slots::mark_xray_rules_dirty(&ctx.runtime).await;
    }
    let gates = super::gates::converge(ctx, shared, &blocked).await;
    out.errors.extend(gates.errors);
    out
}

/// Exact identities for each separately authorized REALITY inlet.
fn xray_grants(state: &State, blocked: &BTreeSet<Uuid>) -> BTreeMap<(&'static str, Uuid), Uuid> {
    let mut grants = BTreeMap::new();
    for user in &state.users {
        for (tag, request) in [
            (XRAY_INBOUND_TAGS[0], RequestedEgress::Direct),
            (XRAY_INBOUND_TAGS[1], RequestedEgress::RequiredResidential),
        ] {
            if access_for(
                user,
                &state.residential,
                Protocol::Reality,
                request,
                blocked.contains(&user.user_id),
            )
            .is_ok()
            {
                grants.insert((tag, user.user_id), user.credentials.vless_uuid);
            }
        }
    }
    grants
}

/// Recheck the owning accounting policy after awaited readback/removal, before a grant.
async fn xray_grant_current(
    ctx: &DaemonCtx,
    shared: &Shared,
    state: &State,
    user: &User,
    tag: &str,
    forced_blocked: &BTreeSet<Uuid>,
) -> bool {
    let delta = shared
        .pending()
        .await
        .get(&user.user_id)
        .copied()
        .unwrap_or_default();
    let account_blocked =
        forced_blocked.contains(&user.user_id) || is_blocked(user, delta, ctx.host.now()).is_some();
    let requested = if tag == XRAY_INBOUND_TAGS[0] {
        RequestedEgress::Direct
    } else {
        RequestedEgress::RequiredResidential
    };
    access_for(
        user,
        &state.residential,
        Protocol::Reality,
        requested,
        account_blocked,
    )
    .is_ok()
}

/// `RemoveUser` 撞上「email 不存在」时 xray 返回错误，而这时目标状态其实已经达成
/// （那个 email 本来就不在 inbound 里），所以按成功处理。
///
/// **只给 RemoveUser 这个方向用**：`AddUser` 的「已存在」不代表目标达成 ——
/// 挂着的可能是旧 uuid，判据见 [`email_taken`]（2026-09-14 裁决）。
///
/// 文案已在真内核上核对（xray 26.3.27 与 26.9.9，2026-09-14，见
/// `super::xray::tests::user_alter_facts_against_a_real_xray`）：`proxy/vless: User <email> not found.`。
/// 仍宽匹配三个子串以容其它版本的措辞 —— 匹配失败的后果只是多记一条 error +
/// 多跑一次 CLI 退路（幂等），不会误判成功。
pub(super) fn target_already_reached(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("already") || m.contains("not found") || m.contains("not exist")
}

/// `AddUser` 失败是不是「这个 email 已经在 inbound 里」= 位置被占。
///
/// 真内核文案（xray 26.3.27 与 26.9.9，2026-09-14 实测）：
/// `proxy/vless: User <email> already exists.`。
/// **只认 `already`**，不像 [`target_already_reached`] 那样连 `not found` 一起认：
/// AddUser 打到还没起来的 inbound 报的是 `handler not found: <tag>`，那条也带 `not found`，
/// 认下去就等于把「内核根本没收到这个用户」记成同步成功（同一个真内核用例钉住这一条）。
///
/// **只是退路**：`sync_users` 正常走 `XrayApi::inbound_user_uuid`
/// （`GetInboundUsers` 读回当前挂的 uuid），这条文案判据只在那次读失败时才用得上，
/// 所以内核换措辞也不会让轮换静默失效。判成占位的后果是「摘掉再加一次」（幂等）；
/// 判不出来的后果是记一条 error + 60 秒后重试 —— 两边都不会把没换成的 uuid 记成换成了。
pub(super) fn email_taken(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("already")
}

/// 把 xray 里占着这个 email 的用户摘掉，好让 `AddUser` 能把新 uuid 挂上去。
///
/// 与 `to_remove` 那一路同口径（决策 D12 / 调研 X9）：gRPC 报「不存在」= 位置本来就是空的，
/// 其余失败走 `xray api rmu` CLI 退路 —— RemoveUser 这个方向不自愈，而位置腾不出来
/// 这一轮连 AddUser 都发不出去（新 uuid 一直不生效，审查意见③）。
async fn free_the_email(
    ctx: &DaemonCtx,
    shared: &Shared,
    tag: &str,
    id: Uuid,
) -> Result<(), String> {
    match shared.xray().remove_user(tag, id).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if target_already_reached(&msg) {
                tracing::debug!(tag, %id, "腾位置的 RemoveUser 报不存在，直接 AddUser");
                Ok(())
            } else if cli_remove(ctx, tag, &id.to_string()).await {
                tracing::info!(tag, %id, "腾位置的 RemoveUser 走了 CLI 退路");
                Ok(())
            } else {
                Err(msg)
            }
        }
    }
}

/// `xray api rmu --server=<addr> -tag=<tag> <email>`；找不到可执行文件或非零退出返回 false。
async fn cli_remove(ctx: &DaemonCtx, tag: &str, email: &str) -> bool {
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let (tag, email) = (tag.to_string(), email.to_string());
    tokio::task::spawn_blocking(move || {
        let Some(prog) = xray_program(host.as_ref(), &paths) else {
            return false;
        };
        let args = rmu_args(super::XRAY_API_ADDR, &tag, &email);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        host.run(&prog.to_string_lossy(), &refs)
            .map(|o| o.ok())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// 读当前 state 与内存增量、算拒绝集合、调 [`sync_users`]。
pub async fn sync_now(ctx: &DaemonCtx, shared: &Shared) -> SyncOutcome {
    let now = ctx.host.now();
    let pending = shared.pending().await.clone();
    let blocked = {
        let state = ctx.store.read().await;
        blocked_set(&state, &pending, now)
    };
    sync_users(ctx, shared, &blocked).await
}

/// 订阅 `EventBus`：任何 `StateChanged` 立刻同步一次；另有每 60 秒的安全网。
pub async fn sync_loop(ctx: DaemonCtx, shared: Arc<Shared>) {
    let mut rx = ctx.bus.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(SYNC_INTERVAL_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 启动时先收敛一次：`bui install` 写的初版快照收的是全量用户（含只有 Reality 权益的），
    // 而且 P2 合并前它也不带限额判定。
    report(sync_now(&ctx, &shared).await);
    loop {
        tokio::select! {
            _ = tick.tick() => report(sync_now(&ctx, &shared).await),
            ev = rx.recv() => match ev {
                Ok(Event::StateChanged(what)) => {
                    tracing::debug!(what, "期望态变化，同步用户");
                    report(sync_now(&ctx, &shared).await);
                }
                Ok(_) => {}
                // 落后就当「有过变化」，直接全量对一次（幂等）
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    report(sync_now(&ctx, &shared).await)
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

/// 失败原因拼进文案：`tracing_journald` 把 `error` 字段写成独立的 `F_ERROR`，`journalctl -o cat`
/// 只看得到 `MESSAGE`。常量打头不变——哨兵（`sentinel::signature::xray_grpc`）按包含匹配。
pub fn sync_failed_message(e: impl std::fmt::Display) -> String {
    format!("{USER_SYNC_FAILED_LOG}：{e}")
}

fn report(out: SyncOutcome) {
    for e in &out.errors {
        tracing::warn!(error = %e, "{}", sync_failed_message(e));
    }
    if out.snapshot_written || !out.added.is_empty() || !out.removed.is_empty() {
        tracing::info!(
            snapshot = out.snapshot_written,
            added = out.added.len(),
            removed = out.removed.len(),
            "用户同步完成"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{harness as bare_harness, Harness};
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn alice(s: &State) -> &User {
        s.users.iter().find(|u| u.username == "alice").unwrap()
    }

    #[test]
    fn protocol_labels_mirror_the_v3_mapping() {
        let mut e = sample_state().users[0].entitlements.clone();
        assert_eq!(protocol_label(&e), "fusion");
        e.protocols = vec![Protocol::Hysteria2];
        assert_eq!(protocol_label(&e), "hysteria2");
        e.protocols = vec![Protocol::Reality];
        assert_eq!(protocol_label(&e), "vless-reality");
        e.protocols = vec![];
        assert_eq!(protocol_label(&e), "hysteria2", "空权益按 v3 的默认值兜底");
    }

    #[test]
    fn projection_keeps_every_v3_field_name() {
        let mut s = sample_state();
        s.users[0].entitlements.expires_at = Some("2026-10-01T00:00:00Z".into());
        s.users[0].entitlements.traffic_limit.total_bytes = Some(5 * GIB);
        s.users[0].usage = Usage {
            total_bytes: 1234,
            monthly_bytes: 234,
            month_key: "2026-09".into(),
            last_seen_at: None,
        };
        let v = serde_json::to_value(project(
            alice(&s),
            &s.node,
            &s.residential,
            &BTreeSet::new(),
            &Default::default(),
        ))
        .unwrap();
        assert_eq!(v["username"], "alice");
        assert_eq!(v["protocol"], "fusion");
        assert_eq!(v["createdAt"], "2026-09-11T00:00:00Z");
        assert_eq!(v["limits"]["expiresAt"], "2026-10-01T00:00:00Z");
        assert_eq!(v["limits"]["trafficLimit"], 5 * GIB);
        assert!(
            v["limits"].get("monthlyLimit").is_none(),
            "不限的项不出现（v3 也是这样）"
        );
        assert!(
            v["limits"].get("speedLimit").is_none(),
            "决策 D13：speedLimit 已删"
        );
        assert_eq!(v["usage"]["total"], 1234);
        assert_eq!(v["usage"]["monthly"]["2026-09"], 234);
        assert_eq!(v["password"], "pw-alice-01");
        assert_eq!(v["uuid"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(
            v["sni"], "www.bing.com",
            "全局 REALITY 伪装域，不是建户时的旧拷贝"
        );
        assert_eq!(v["residential"], true);
        assert_eq!(v["disabled"], false);
        assert_eq!(v["blocked"], false);
    }

    #[test]
    fn the_panel_user_carries_its_slot_ip_and_residential_port() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        // 扩到 2 槽，把 alice 放到槽 1
        let g = s
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        let mut second = g.upstreams[0].clone();
        second.id = Uuid::from_u128(0xa001);
        second.host = "isp2.example.net".into();
        second.verified = Some(bui_schema::model::Verified {
            ip: "198.51.100.8".into(),
            asn: None,
            org: None,
            country: None,
            at: "2026-09-13T00:00:00Z".into(),
        });
        g.upstreams.push(second);
        bui_schema::slots::sync_slots(&mut s.residential);
        let uid = s.users[0].user_id;
        assert!(bui_schema::slots::assign(
            &mut s,
            uid,
            Uuid::from_u128(0xa001)
        ));

        let rows = project_all(&s, &BTreeSet::new());
        assert_eq!(rows.len(), 1);
        let v = serde_json::to_value(&rows[0]).unwrap();
        // 槽位与 IP 的投影不变：换槽换的就是这个出口 IP
        assert_eq!(v["slot"], 1);
        assert_eq!(v["slotIp"], "198.51.100.8");
        // 4.1：端口与跳跃区间与槽序号无关（4.0.x 这里是 40001 + 45500-50000）
        assert_eq!(v["slotPort"], 40000);
        assert_eq!(v["slotHop"], serde_json::json!([41000, 50000]));
        // v3 字段一个都不许动（前端按它们渲染）
        assert_eq!(v["username"], "alice");
        assert_eq!(v["protocol"], "fusion");
        assert_eq!(v["residential"], true);
    }

    #[test]
    fn a_user_without_the_residential_entitlement_has_no_slot_fields() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        s.users[0].entitlements.residential = None;
        let v = serde_json::to_value(&project_all(&s, &BTreeSet::new())[0]).unwrap();
        assert!(v["slot"].is_null());
        assert!(v["slotIp"].is_null());
        assert!(v["slotPort"].is_null());
    }

    /// 槽位表没有建立时，展示待绑定原因，不签出一个猜测的池首地址。
    #[test]
    fn an_empty_slot_table_reports_missing_binding_without_a_fallback_ip() {
        let s = crate::modules::residential::sample_state_with_pool();
        assert!(
            s.residential.slots.is_empty(),
            "sample 没有槽位表，正是要测的那一档"
        );
        let v = serde_json::to_value(&project_all(&s, &BTreeSet::new())[0]).unwrap();
        assert!(v["slot"].is_null());
        assert!(v["slotPort"].is_null());
        assert!(v["slotHop"].is_null());
        assert!(v["slotIp"].is_null());
        assert_eq!(v["residentialUnavailable"], "residential slot unassigned");
    }

    /// 空池没有住宅通路可交付，不能继续显示可用端口和猜测的槽位。
    #[test]
    fn an_empty_pool_reports_unavailability_without_a_connectable_port() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        s.residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap()
            .upstreams
            .clear();
        let v = serde_json::to_value(&project_all(&s, &BTreeSet::new())[0]).unwrap();
        assert!(v["slot"].is_null());
        assert!(v["slotPort"].is_null());
        assert!(v["slotIp"].is_null());
        assert_eq!(v["residentialUnavailable"], "residential pool empty");
    }

    /// 绑定已经消失时，不把其它供应商的地址显示成该用户的出口。
    #[test]
    fn a_stale_slot_id_reports_missing_slot_without_using_another_suppliers_ip() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        bui_schema::slots::sync_slots(&mut s.residential);
        assert_eq!(s.residential.slots.len(), 1);
        let uid = s.users[0].user_id;
        s.users
            .iter_mut()
            .find(|u| u.user_id == uid)
            .unwrap()
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(Uuid::from_u128(0xdead));
        let v = serde_json::to_value(&project_all(&s, &BTreeSet::new())[0]).unwrap();
        assert!(v["slot"].is_null());
        assert!(v["slotIp"].is_null());
        assert_eq!(v["residentialUnavailable"], "residential slot missing");
    }

    /// 面板投影（T14）：**端口与槽位解耦** + 门位那一档。
    ///
    /// 4.0.x 的面板显示 `40000 + 槽序号` 与那一槽的跳跃切片；4.1 起两样都固定
    /// （`ports.hy2_resi` / `ports.hy2_resi_hop`），与 `nodes_for` 逐字一致。门位是
    /// spec §6 的到期 / 封禁语义的**唯一**可见解释：`deny` = 客户端仍会显示已连接，
    /// 但每个请求都被拒。
    #[test]
    fn the_panel_shows_a_slot_independent_port_and_the_gate() {
        // **必须是非 0 槽**：4.0.x 的 `40000 + 槽序号` 在槽 0 上恰好等于 4.1 的固定端口，
        // 拿槽 0 的用户断言等于什么都没验（第一版就是这个洞，变异验证抓出来的）。
        let mut s = crate::modules::residential::sample_state_with_pool();
        let g = s
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        let mut second = g.upstreams[0].clone();
        second.id = Uuid::from_u128(0xb001);
        second.host = "isp2.example.net".into();
        g.upstreams.push(second);
        bui_schema::slots::sync_slots(&mut s.residential);
        let uid = s.users[0].user_id;
        assert!(bui_schema::slots::assign(
            &mut s,
            uid,
            Uuid::from_u128(0xb001)
        ));
        bui_schema::hy2pool::migrate(&mut s, t0());
        let gates = super::super::gates::expected(&s, &BTreeSet::new());
        let p = project(alice(&s), &s.node, &s.residential, &BTreeSet::new(), &gates);
        assert_eq!(p.slot, Some(1), "槽位投影仍跟着槽走");
        assert_eq!(p.slot_port, Some(40000), "4.0.x 这里是 40001");
        assert_eq!(p.slot_hop, Some((41000, 50000)), "4.0.x 这里是那一槽的切片");
        assert_eq!(p.hy2_resi_gate.as_deref(), Some("slot-1-out"));

        // 到期 / 封禁：门位是 deny —— UI 据此解释「显示已连接但请求全被拒」
        let mut expired = s.clone();
        expired.users[0].disabled = true;
        let b2 = blocked_set(&expired, &BTreeMap::new(), t0());
        let g2 = super::super::gates::expected(&expired, &b2);
        assert_eq!(
            project(
                alice(&expired),
                &expired.node,
                &expired.residential,
                &b2,
                &g2
            )
            .hy2_resi_gate
            .as_deref(),
            Some("deny")
        );

        // 还没拿到凭据 ⇒ 压根没有门（订阅里也没有住宅 HY2 节点）
        let mut fresh = crate::modules::residential::sample_state_with_pool();
        fresh.users[0].credentials.hy2_resi_cred = None;
        assert_eq!(
            project(
                alice(&fresh),
                &fresh.node,
                &fresh.residential,
                &BTreeSet::new(),
                &Default::default()
            )
            .hy2_resi_gate
            .as_deref(),
            Some("未分配")
        );

        // 没有住宅 hysteria2 权益 ⇒ 这一档整个不出现
        let mut direct = crate::modules::residential::sample_state_with_pool();
        direct.users[0].entitlements.residential = None;
        let v = serde_json::to_value(project(
            alice(&direct),
            &direct.node,
            &direct.residential,
            &BTreeSet::new(),
            &Default::default(),
        ))
        .unwrap();
        assert!(v.get("hy2ResiGate").is_none(), "{v}");

        // 面板显示的端口就是订阅里的端口（两处必须同源）
        let n = bui_schema::nodes::nodes_for(alice(&s), &s.node, &s.residential)
            .into_iter()
            .find(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential)
            .expect("有凭据 ⇒ 有住宅 HY2 节点");
        assert_eq!((Some(n.port), n.hop), (p.slot_port, p.slot_hop));
    }

    #[test]
    fn projection_marks_blocked_users() {
        let s = sample_state();
        let id = alice(&s).user_id;
        let p = project(
            alice(&s),
            &s.node,
            &s.residential,
            &BTreeSet::from([id]),
            &Default::default(),
        );
        assert!(p.blocked);
        assert_eq!(project_all(&s, &BTreeSet::from([id])).len(), 1);
    }

    fn canonical_single_node_state() -> State {
        let mut state = crate::modules::residential::sample_state_with_pool();
        state.node.domain = "edge.example".into();
        state.node.ports.hy2 = 12345;
        state.node.ports.hy2_resi = 42345;
        state.node.ports.hy2_resi_hop = (43000, 43999);
        state.node.obfs.enabled = true;
        state.node.obfs.password = "obfs/pw?".into();
        state.users = vec![new_user(
            &CreateRequest {
                username: "alice".into(),
                password: Some("native-only-secret".into()),
                protocol: Some("hysteria2".into()),
                residential: Some(true),
                ..Default::default()
            },
            t0(),
        )
        .unwrap()];
        bui_schema::slots::sync_slots(&mut state.residential);
        let uid = state.users[0].user_id;
        let upstream = state.residential.default_group().unwrap().upstreams[0].id;
        assert!(bui_schema::slots::assign(&mut state, uid, upstream));
        bui_schema::hy2pool::grow(&mut state.residential.hy2_pool, 32, &BTreeSet::new());
        assert_eq!(
            bui_schema::hy2pool::assign_at(&mut state, uid, t0()).as_deref(),
            Some("r000")
        );
        state.residential.hy2_pool.creds[0].secret = "reserved-create".into();
        state
    }

    #[test]
    fn canonical_node_uri_uses_reserved_identity_after_create_and_rotate() {
        let mut state = canonical_single_node_state();
        for (id, secret) in [("r000", "reserved-create"), ("r001", "reserved-rotated")] {
            let cred = bui_schema::hy2pool::cred_of(&state.users[0], &state.residential).unwrap();
            assert_eq!(cred.name, id);
            assert_eq!(cred.secret, secret);
            assert_ne!(cred.name, state.users[0].username);
            assert_ne!(cred.secret, state.users[0].credentials.hy2_password);
            let expected = match id {
                "r000" => "hysteria2://r000:reserved-create@edge.example:42345?sni=edge.example&insecure=0&mport=43000-43999&obfs=salamander&obfs-password=obfs%2Fpw%3F#alice-HY2%E4%BD%8F%E5%AE%85",
                _ => "hysteria2://r001:reserved-rotated@edge.example:42345?sni=edge.example&insecure=0&mport=43000-43999&obfs=salamander&obfs-password=obfs%2Fpw%3F#alice-HY2%E4%BD%8F%E5%AE%85",
            };
            let payload = serde_json::to_value(&project_all(&state, &BTreeSet::new())[0]).unwrap();
            assert_eq!(payload["nodeUri"], expected, "the product projection must publish the reserved identity, not the direct password");
            let parsed = bui_schema::parse::node_uri(payload["nodeUri"].as_str().unwrap()).unwrap();
            assert_eq!(
                (parsed.host.as_str(), parsed.port, parsed.hop),
                ("edge.example", 42345, Some((43000, 43999)))
            );
            assert_eq!(
                parsed.transport,
                bui_schema::nodes::Transport::Hysteria2 {
                    username: id.into(),
                    password: secret.into(),
                    sni: "edge.example".into(),
                    obfs_password: Some("obfs/pw?".into()),
                }
            );
            let stock = bui_schema::render::client::probe_config(&[
                bui_schema::render::client::ProbeTarget {
                    node: &parsed,
                    listen_port: 12001,
                    user: "fixture-user".into(),
                    pass: "fixture-password".into(),
                },
            ]);
            let outbound = &stock["outbounds"][0];
            assert_eq!(outbound["type"], "hysteria2");
            assert_eq!(outbound["server"], "edge.example");
            assert_eq!(outbound["server_port"], 42345);
            assert_eq!(outbound["server_ports"], serde_json::json!(["43000:43999"]));
            assert_eq!(outbound["password"], format!("{id}:{secret}"));
            assert_eq!(outbound["tls"]["server_name"], "edge.example");
            assert_eq!(outbound["obfs"]["password"], "obfs/pw?");
            if let Some(directory) = std::env::var_os("BUI_TEST_PANEL_NODE_URI_FIXTURE_DIR") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(
                    directory.join(format!("{id}-panel.json")),
                    serde_json::to_vec_pretty(&payload).unwrap(),
                )
                .unwrap();
                std::fs::write(
                    directory.join(format!("{id}-stock-client.json")),
                    serde_json::to_vec_pretty(&stock).unwrap(),
                )
                .unwrap();
            }
            if id == "r000" {
                let uid = state.users[0].user_id;
                rotate(&mut state.users[0]);
                assert_eq!(
                    bui_schema::hy2pool::release(&mut state, uid, t0()).as_deref(),
                    Some("r000")
                );
                assert_eq!(
                    bui_schema::hy2pool::assign_at(&mut state, uid, t0()).as_deref(),
                    Some("r001")
                );
                state.residential.hy2_pool.creds[1].secret = "reserved-rotated".into();
            }
        }
    }

    #[test]
    fn canonical_node_uri_preserves_authorized_direct_and_reality_parameters() {
        let mut state = canonical_single_node_state();
        state.users[0].entitlements.residential = None;
        state.users[0].entitlements.direct = true;
        let payload = serde_json::to_value(&project_all(&state, &BTreeSet::new())[0]).unwrap();
        assert_eq!(payload["nodeUri"], "hysteria2://alice:native-only-secret@edge.example:12345?sni=edge.example&insecure=0&mport=20000-30000&obfs=salamander&obfs-password=obfs%2Fpw%3F#alice-HY2%E7%9B%B4%E8%BF%9E");
        state.users[0].entitlements.protocols = vec![Protocol::Reality];
        state.users[0].credentials.vless_uuid = Uuid::from_u128(0x222);
        state.node.reality.public_key = "CANONICAL-PUB".into();
        state.node.reality.dest = "masked.example:443".into();
        state.node.reality.short_ids = vec!["0123456789abcdef".into()];
        state.node.ports.reality_direct = 22345;
        let payload = serde_json::to_value(&project_all(&state, &BTreeSet::new())[0]).unwrap();
        assert_eq!(payload["nodeUri"], "vless://00000000-0000-0000-0000-000000000222@edge.example:22345?security=reality&encryption=none&pbk=CANONICAL-PUB&headerType=&fp=chrome&spx=%2F&type=tcp&flow=xtls-rprx-vision&sni=masked.example&sid=0123456789abcdef#alice-Reality%E7%9B%B4%E8%BF%9E");
    }

    #[test]
    fn canonical_node_uri_is_absent_for_denied_or_unsupported_single_nodes() {
        let control = canonical_single_node_state();
        let mut cases = Vec::new();
        let mut state = control.clone();
        state.users[0].disabled = true;
        cases.push(("disabled", state, BTreeSet::new()));
        cases.push((
            "blocked",
            control.clone(),
            BTreeSet::from([control.users[0].user_id]),
        ));
        let mut state = control.clone();
        state.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(Uuid::from_u128(0xdead));
        cases.push(("bad slot", state, BTreeSet::new()));
        let mut state = control.clone();
        state
            .residential
            .groups
            .get_mut(DEFAULT_GROUP)
            .unwrap()
            .enabled = false;
        cases.push(("disabled pool", state, BTreeSet::new()));
        let mut state = control.clone();
        state.users[0].credentials.hy2_resi_cred = None;
        cases.push(("missing reserved identity", state, BTreeSet::new()));
        let mut state = control.clone();
        state.users[0].entitlements.protocols.clear();
        cases.push(("no supported protocol", state, BTreeSet::new()));
        let mut state = control.clone();
        state.users[0].entitlements.residential = None;
        state.users[0].entitlements.direct = false;
        cases.push(("no path grant", state, BTreeSet::new()));
        let mut state = control;
        state.users[0]
            .entitlements
            .protocols
            .push(Protocol::Reality);
        cases.push(("fusion uses feed URL", state, BTreeSet::new()));
        for (reason, state, blocked) in cases {
            let payload = serde_json::to_value(&project_all(&state, &blocked)[0]).unwrap();
            assert!(
                payload.get("nodeUri").is_none(),
                "{reason} must not publish a guessed single-node URI"
            );
        }
    }

    #[test]
    fn usernames_and_passwords_follow_the_v3_rules() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("张三_a-b.c").is_ok());
        assert_eq!(validate_username("").unwrap_err(), "username 不能为空");
        assert_eq!(
            validate_username(&"a".repeat(65)).unwrap_err(),
            "username 长度不能超过 64 字符"
        );
        assert_eq!(
            validate_username("a/b").unwrap_err(),
            "username 仅允许字母/数字/中文/下划线/连字符/点"
        );
        assert!(validate_username("a b").is_err());
        assert!(validate_password("x").is_ok());
        assert_eq!(validate_password("").unwrap_err(), "password 不能为空");
        assert_eq!(
            validate_password(&"a".repeat(257)).unwrap_err(),
            "password 长度不能超过 256 字符"
        );
    }

    /// 决策 D13：v3 前端仍会传 `sni` 与 `speed`，两者**接受但忽略**——反序列化必须收下
    /// （不能因为多出字段就 400），而 `new_user` / `apply_update` 一个字都不看它们。
    #[test]
    fn sni_and_speed_are_accepted_and_then_ignored() {
        let req: CreateRequest =
            serde_json::from_str(r#"{"username":"bob","sni":"ignored.example.com","speed":100}"#)
                .unwrap();
        assert_eq!(req.sni.as_deref(), Some("ignored.example.com"));
        assert_eq!(req.speed, Some(100.0));
        let plain = new_user(
            &CreateRequest {
                username: "bob".into(),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        let ignored = new_user(&req, t0()).unwrap();
        assert_eq!(
            ignored.entitlements, plain.entitlements,
            "sni / speed 不许影响任何权益"
        );

        let upd: UpdateRequest = serde_json::from_str(r#"{"speed":50}"#).unwrap();
        assert_eq!(upd.speed, Some(50.0));
        let mut u = ignored.clone();
        apply_update(&mut u, &upd, t0()).unwrap();
        assert_eq!(u, ignored, "只传 speed 时用户一个字段都不该变");
    }

    /// 2026-09-14 裁决：面板建用户（装机首用户走同一个函数）一律带随机订阅 token，
    /// 且不停用用户名链接（停用只由轮换做）。
    #[test]
    fn new_user_carries_a_random_sub_token() {
        let make = || {
            new_user(
                &CreateRequest {
                    username: "bob".into(),
                    ..Default::default()
                },
                t0(),
            )
            .unwrap()
        };
        let u = make();
        let t = u.sub_token.as_deref().expect("建出来就该有 token");
        assert!(bui_schema::sub::is_sub_token(t), "{t}");
        assert!(!u.legacy_sub_disabled);
        assert_ne!(make().sub_token, u.sub_token, "每个用户一个新随机值");
    }

    /// 升级上来的老 `state.json`（用户都没有 token）：一次补齐所有人 + 开 7 天宽限期，
    /// 第二次启动零变更（连备份都不该多一份）。
    #[tokio::test]
    async fn backfill_fills_missing_tokens_once_and_opens_the_grace_window() {
        let d = tempfile::tempdir().unwrap();
        let mut st = sample_state();
        let proto = st.users[0].clone();
        st.users = vec![proto.clone(), proto];
        st.users[1].username = "bob".into();
        st.users[1].user_id = uuid::Uuid::from_u128(0x2000);
        let store = Store::create(d.path().join("state.json"), st)
            .await
            .unwrap();

        assert_eq!(backfill_sub_tokens(&store, t0()).await.unwrap(), 2);
        let s = store.read().await;
        let tokens: Vec<String> = s
            .users
            .iter()
            .map(|u| u.sub_token.clone().unwrap())
            .collect();
        assert!(
            tokens.iter().all(|t| bui_schema::sub::is_sub_token(t)),
            "{tokens:?}"
        );
        assert_ne!(tokens[0], tokens[1], "每人一个独立 token");
        assert_eq!(
            s.system.legacy_sub_until.as_deref(),
            Some("2026-09-18T00:00:00Z"),
            "补出过 token ⇒ 宽限期 = 启动时刻 + 7 天"
        );
        let backups = || {
            std::fs::read_dir(d.path().join("state.backups"))
                .map(|it| it.count())
                .unwrap_or(0)
        };
        let after_first = backups();

        // 幂等：第二遍一个字段都不动，Store 的零变更比对因此不写盘
        assert_eq!(backfill_sub_tokens(&store, t0()).await.unwrap(), 0);
        let s2 = store.read().await;
        assert_eq!(
            s2.users
                .iter()
                .map(|u| u.sub_token.clone())
                .collect::<Vec<_>>(),
            s.users
                .iter()
                .map(|u| u.sub_token.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(s2.system.legacy_sub_until, s.system.legacy_sub_until);
        assert_eq!(
            backups(),
            after_first,
            "零变更不许再写一次盘（不该多一份备份）"
        );
    }

    /// 已经有宽限期（v3 导入设的、或运维自己改的）时不覆盖：补 token 不该把
    /// 运维刚收紧的截止时刻又推后 7 天。
    #[tokio::test]
    async fn backfill_never_overwrites_an_existing_grace_window() {
        let d = tempfile::tempdir().unwrap();
        let mut st = sample_state();
        st.system.legacy_sub_until = Some("2026-09-15T12:00:00Z".into());
        let store = Store::create(d.path().join("state.json"), st)
            .await
            .unwrap();

        assert_eq!(backfill_sub_tokens(&store, t0()).await.unwrap(), 1);
        assert_eq!(
            store.read().await.system.legacy_sub_until.as_deref(),
            Some("2026-09-15T12:00:00Z")
        );
    }

    /// 全新装机：首用户建号时就有 token ⇒ 一个都补不出来 ⇒ **不设**宽限期，
    /// 用户名链接在这台机器上从来不可用。
    #[tokio::test]
    async fn a_fresh_install_never_opens_the_grace_window() {
        let d = tempfile::tempdir().unwrap();
        let mut st = sample_state();
        st.users = vec![new_user(
            &CreateRequest {
                username: "alice".into(),
                ..Default::default()
            },
            t0(),
        )
        .unwrap()];
        let store = Store::create(d.path().join("state.json"), st)
            .await
            .unwrap();

        assert_eq!(backfill_sub_tokens(&store, t0()).await.unwrap(), 0);
        assert_eq!(store.read().await.system.legacy_sub_until, None);
    }

    /// 2026-09-14 裁决：轮换把订阅 token、hy2 密码、vless uuid **一起**换掉
    /// （只换一样等于没换：泄露的链接里三样都有），并停用这个用户的「用户名链接」。
    #[test]
    fn rotate_swaps_all_three_credentials_and_disables_the_username_link() {
        let mut s = sample_state();
        let before = s.users[0].clone();
        rotate(&mut s.users[0]);
        let after = s.users[0].clone();

        let token = after.sub_token.as_deref().expect("轮换后必须有 token");
        assert!(bui_schema::sub::is_sub_token(token), "{token}");
        assert_ne!(after.sub_token, before.sub_token);
        assert_ne!(
            after.credentials.hy2_password,
            before.credentials.hy2_password
        );
        assert_eq!(
            after.credentials.hy2_password.len(),
            16,
            "与 v3 同形的 16 个 hex 字符"
        );
        assert_ne!(after.credentials.vless_uuid, before.credentials.vless_uuid);
        assert!(after.legacy_sub_disabled, "旧用户名链接必须立刻失效");

        // 别的字段一个都不许动（权益、用量、账务、槽位都在里面）
        let mut restored = after.clone();
        restored.sub_token = before.sub_token.clone();
        restored.credentials = before.credentials.clone();
        restored.legacy_sub_disabled = before.legacy_sub_disabled;
        assert_eq!(restored, before);

        // 每次轮换都是新随机值
        rotate(&mut s.users[0]);
        assert_ne!(s.users[0].sub_token, after.sub_token);
        assert_ne!(
            s.users[0].credentials.vless_uuid,
            after.credentials.vless_uuid
        );
    }

    /// 前端要拿 `subToken` 拼订阅链接（用户名链接只在宽限期内可用），所以投影必须带它。
    #[test]
    fn the_panel_user_carries_the_sub_token() {
        let mut s = sample_state();
        let v = serde_json::to_value(project(
            alice(&s),
            &s.node,
            &s.residential,
            &BTreeSet::new(),
            &Default::default(),
        ))
        .unwrap();
        assert!(
            v.get("subToken").is_none(),
            "老 state 还没补齐 token ⇒ 字段不出现"
        );
        s.users[0].sub_token = Some("0123456789abcdef0123456789abcdef".into());
        let v2 = serde_json::to_value(project(
            alice(&s),
            &s.node,
            &s.residential,
            &BTreeSet::new(),
            &Default::default(),
        ))
        .unwrap();
        assert_eq!(v2["subToken"], "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn new_user_converts_days_and_gigabytes_like_v3() {
        let req = CreateRequest {
            managed_egress: None,
            username: "bob".into(),
            password: None,
            days: Some(30.0),
            traffic: Some(1.5),
            monthly: Some(0.0),
            protocol: Some("fusion".into()),
            residential: Some(false),
            sni: Some("ignored.example.com".into()),
            speed: Some(100.0),
        };
        let u = new_user(&req, t0()).unwrap();
        assert_eq!(u.username, "bob");
        assert_eq!(u.created_at, "2026-09-11T00:00:00Z");
        assert_eq!(
            u.entitlements.expires_at.as_deref(),
            Some("2026-10-11T00:00:00Z")
        );
        assert_eq!(
            u.entitlements.traffic_limit.total_bytes,
            Some(1_610_612_736),
            "1.5 GiB"
        );
        assert_eq!(u.entitlements.traffic_limit.monthly_bytes, None, "0 = 不限");
        assert_eq!(
            u.entitlements.protocols,
            vec![Protocol::Hysteria2, Protocol::Reality]
        );
        assert!(u.entitlements.direct);
        assert!(
            u.entitlements.residential.is_none(),
            "residential=false ⇒ 不给住宅权益"
        );
        assert_eq!(
            u.credentials.hy2_password.len(),
            16,
            "没给密码就随机 16 个 hex 字符"
        );
        assert_eq!(u.usage, Usage::default());
        assert!(!u.disabled);
        // 显式给密码时原样用
        let req2 = CreateRequest {
            username: "carol".into(),
            password: Some("pw".into()),
            ..Default::default()
        };
        assert_eq!(
            new_user(&req2, t0()).unwrap().credentials.hy2_password,
            "pw"
        );
        // 默认协议与住宅（v3：protocol 缺省 hysteria2、residential 缺省 true）
        let d = new_user(
            &CreateRequest {
                username: "dave".into(),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        assert_eq!(d.entitlements.protocols, vec![Protocol::Hysteria2]);
        assert_eq!(
            d.entitlements.residential.map(|r| r.group_id),
            Some("default".to_string())
        );
        // 不认的协议名报错，不静默兜底
        assert!(new_user(
            &CreateRequest {
                username: "e".into(),
                protocol: Some("vless-ws-tls".into()),
                ..Default::default()
            },
            t0()
        )
        .is_err());
        assert!(new_user(
            &CreateRequest {
                username: "a/b".into(),
                ..Default::default()
            },
            t0()
        )
        .is_err());
    }

    /// 直连权益与 v3 订阅逐项等价（2026-09-13 裁决）：单协议（hysteria2 / vless-reality）开住宅
    /// 只发住宅版 ⇒ `direct=false`；单协议不开住宅只发直连版、fusion 直连照给 ⇒ `direct=true`。
    /// 与 `bui_schema::v3::import` 同一个判定（`v3::direct_entitlement`）。
    #[test]
    fn a_single_protocol_user_with_residential_gets_no_direct_entitlement() {
        let make = |protocol: Option<&str>, residential: Option<bool>| {
            new_user(
                &CreateRequest {
                    username: "bob".into(),
                    protocol: protocol.map(str::to_string),
                    residential,
                    ..Default::default()
                },
                t0(),
            )
            .unwrap()
        };
        let hy2 = make(Some("hysteria2"), Some(true));
        assert!(!hy2.entitlements.direct);
        assert!(hy2.entitlements.residential.is_some());
        let reality = make(Some("vless-reality"), Some(true));
        assert!(!reality.entitlements.direct);
        assert!(
            !make(None, None).entitlements.direct,
            "缺省（v3：protocol 缺省 hysteria2、residential 缺省 true）就是单协议 + 住宅"
        );
        let hy2_direct = make(Some("hysteria2"), Some(false));
        assert!(
            hy2_direct.entitlements.direct,
            "单协议不开住宅 ⇒ 只有直连版"
        );
        assert!(hy2_direct.entitlements.residential.is_none());
        assert!(make(Some("fusion"), Some(true)).entitlements.direct);
        assert!(make(Some("fusion"), Some(false)).entitlements.direct);

        // 创建权益保留住宅意图；默认池未启用时，面板不能猜出一个可用槽位。
        let s = sample_state();
        for (u, label) in [(&hy2, "hysteria2"), (&reality, "vless-reality")] {
            let v = serde_json::to_value(project(
                u,
                &s.node,
                &s.residential,
                &BTreeSet::new(),
                &Default::default(),
            ))
            .unwrap();
            assert_eq!(v["protocol"], label);
            assert_eq!(v["residential"], true);
            assert!(v["slot"].is_null());
            assert!(v["slotPort"].is_null());
            assert_eq!(v["residentialUnavailable"], "residential pool disabled");
        }
    }

    /// 面板编辑（`PUT /api/users/:u` → `apply_update`）改不了协议与住宅：`UpdateRequest` 没有这两个
    /// 字段（v3 的 PUT 同样不收，前端编辑框也不发），请求体里带了也被忽略。所以 direct 只在新建时
    /// 算一次，编辑哪个字段都不动它；将来给 `UpdateRequest` 加 protocol / residential 时会先撞上
    /// 这里的第二段断言，届时必须按变更后的值重算 direct（`bui_schema::v3::direct_entitlement`）。
    #[test]
    fn editing_a_user_never_touches_the_direct_entitlement() {
        let mut u = new_user(
            &CreateRequest {
                username: "bob".into(),
                protocol: Some("hysteria2".into()),
                residential: Some(true),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        assert!(!u.entitlements.direct);
        let protocols = u.entitlements.protocols.clone();
        let residential = u.entitlements.residential.clone();
        apply_update(
            &mut u,
            &UpdateRequest {
                managed_egress: None,
                username: Some("bob2".into()),
                password: Some("pw2".into()),
                days: Some(3.0),
                traffic: Some(1.0),
                monthly: Some(1.0),
                speed: Some(10.0),
                disabled: Some(true),
            },
            t0(),
        )
        .unwrap();
        assert!(!u.entitlements.direct, "只改别的字段 ⇒ direct 不变");
        assert_eq!(u.entitlements.protocols, protocols);
        assert_eq!(u.entitlements.residential, residential);

        let before = u.clone();
        let upd: UpdateRequest =
            serde_json::from_str(r#"{"residential":false,"protocol":"fusion"}"#).unwrap();
        apply_update(&mut u, &upd, t0()).unwrap();
        assert_eq!(
            u, before,
            "PUT 不收 protocol / residential（与 v3 相同），用户一个字段都不变"
        );
    }

    #[test]
    fn apply_update_follows_v3_semantics() {
        let mut s = sample_state();
        let u = &mut s.users[0];
        apply_update(
            u,
            &UpdateRequest {
                managed_egress: None,
                username: Some("alice2".into()),
                password: Some("new-pw".into()),
                days: Some(10.0),
                traffic: Some(2.0),
                monthly: Some(1.0),
                speed: Some(50.0),
                disabled: None,
            },
            t0(),
        )
        .unwrap();
        assert_eq!(u.username, "alice2");
        assert_eq!(u.credentials.hy2_password, "new-pw");
        assert_eq!(
            u.entitlements.expires_at.as_deref(),
            Some("2026-09-21T00:00:00Z")
        );
        assert_eq!(u.entitlements.traffic_limit.total_bytes, Some(2 * GIB));
        assert_eq!(u.entitlements.traffic_limit.monthly_bytes, Some(GIB));
        // 0 清除限制（v3：`> 0` 才设，否则 delete）
        apply_update(
            u,
            &UpdateRequest {
                days: Some(0.0),
                traffic: Some(0.0),
                monthly: Some(0.0),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        assert_eq!(u.entitlements.expires_at, None);
        assert_eq!(u.entitlements.traffic_limit, TrafficLimit::default());
        // 只有 Reality 权益的用户，password 改的是 uuid（v3 同逻辑）
        u.entitlements.protocols = vec![Protocol::Reality];
        apply_update(
            u,
            &UpdateRequest {
                password: Some("22222222-2222-4222-8222-222222222222".into()),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        assert_eq!(
            u.credentials.vless_uuid.to_string(),
            "22222222-2222-4222-8222-222222222222"
        );
        assert_eq!(u.credentials.hy2_password, "new-pw", "hy2 密码没被动");
        // 不是合法 UUID 就报错
        assert!(apply_update(
            u,
            &UpdateRequest {
                password: Some("not-a-uuid".into()),
                ..Default::default()
            },
            t0()
        )
        .is_err());
        // disabled 是 v4 新增的显式开关
        apply_update(
            u,
            &UpdateRequest {
                disabled: Some(true),
                ..Default::default()
            },
            t0(),
        )
        .unwrap();
        assert!(u.disabled);
    }

    #[test]
    fn month_key_and_limit_checks() {
        assert_eq!(month_key(t0()), "2026-09");
        assert_eq!(month_key(datetime!(2026-01-01 00:00:00 UTC)), "2026-01");
        let mut s = sample_state();
        let u = &mut s.users[0];
        assert_eq!(is_blocked(u, TxRx::default(), t0()), None);
        u.disabled = true;
        assert_eq!(
            is_blocked(u, TxRx::default(), t0()),
            Some(BlockReason::Disabled)
        );
        u.disabled = false;
        u.entitlements.expires_at = Some("2026-09-10T00:00:00Z".into());
        assert_eq!(
            is_blocked(u, TxRx::default(), t0()),
            Some(BlockReason::Expired)
        );
        u.entitlements.expires_at = Some("坏时间".into());
        assert_eq!(
            is_blocked(u, TxRx::default(), t0()),
            Some(BlockReason::Expired),
            "解析失败 fail-closed"
        );
        u.entitlements.expires_at = None;
        u.entitlements.traffic_limit.total_bytes = Some(100);
        u.usage.total_bytes = 60;
        assert_eq!(is_blocked(u, TxRx::default(), t0()), None);
        assert_eq!(
            is_blocked(u, TxRx { tx: 20, rx: 20 }, t0()),
            Some(BlockReason::TotalExceeded),
            "判定要带上未落盘的内存增量"
        );
        u.entitlements.traffic_limit.total_bytes = None;
        u.entitlements.traffic_limit.monthly_bytes = Some(100);
        u.usage.monthly_bytes = 150;
        u.usage.month_key = "2026-09".into();
        assert_eq!(
            is_blocked(u, TxRx::default(), t0()),
            Some(BlockReason::MonthlyExceeded)
        );
        u.usage.month_key = "2026-08".into();
        assert_eq!(
            is_blocked(u, TxRx::default(), t0()),
            None,
            "跨月了，上个月的用量不算这个月"
        );
    }

    #[test]
    fn blocked_set_covers_every_user() {
        let mut s = sample_state();
        s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
        let id = s.users[0].user_id;
        let pending = BTreeMap::from([(id, TxRx { tx: 20, rx: 0 })]);
        assert_eq!(blocked_set(&s, &pending, t0()), BTreeSet::from([id]));
        assert!(blocked_set(&s, &BTreeMap::new(), t0()).is_empty());
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

    async fn residential_access_harness() -> Harness {
        let h = bare_harness().await;
        h.store
            .update(|s| {
                let pool = crate::modules::residential::sample_state_with_pool();
                s.residential = pool.residential;
                bui_schema::slots::sync_slots(&mut s.residential);
                bui_schema::slots::migrate_unassigned(s);
                bui_schema::hy2pool::migrate(s, t0());
            })
            .await
            .unwrap();
        h
    }

    // Positive sync scenarios exercise two genuinely provisioned entry points.
    // A disabled/empty legacy pool no longer authorizes the residential inlet.
    async fn harness() -> Harness {
        residential_access_harness().await
    }

    struct PausingXray {
        inner: super::super::fakes::FakeXray,
        entered: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Semaphore>,
        pause_once: std::sync::atomic::AtomicBool,
        pause_in_add: bool,
        pause_inventory_at: Option<usize>,
        inventory_reads: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl super::super::XrayApi for PausingXray {
        async fn inbound_users(&self, tag: &str) -> anyhow::Result<BTreeMap<Uuid, Uuid>> {
            let n = self
                .inventory_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if self.pause_inventory_at == Some(n) {
                self.entered.notify_one();
                self.resume.acquire().await.unwrap().forget();
            }
            super::super::XrayApi::inbound_users(&self.inner, tag).await
        }
        async fn add_user(&self, tag: &str, id: Uuid, vid: Uuid) -> anyhow::Result<()> {
            super::super::XrayApi::add_user(&self.inner, tag, id, vid).await?;
            if self.pause_in_add
                && self
                    .pause_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.notify_one();
                self.resume.acquire().await.unwrap().forget();
            }
            Ok(())
        }
        async fn remove_user(&self, tag: &str, id: Uuid) -> anyhow::Result<()> {
            super::super::XrayApi::remove_user(&self.inner, tag, id).await
        }
        async fn inbound_user_uuid(&self, tag: &str, id: Uuid) -> anyhow::Result<Option<Uuid>> {
            if self.pause_inventory_at.is_none()
                && !self.pause_in_add
                && self
                    .pause_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.notify_one();
                self.resume.acquire().await.unwrap().forget();
            }
            super::super::XrayApi::inbound_user_uuid(&self.inner, tag, id).await
        }
        async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
            super::super::XrayApi::query_user_deltas(&self.inner).await
        }
        async fn add_rule(&self, r: &bui_schema::render::xray::SlotRule) -> anyhow::Result<()> {
            super::super::XrayApi::add_rule(&self.inner, r).await
        }
        async fn remove_rule(&self, tag: &str) -> anyhow::Result<()> {
            super::super::XrayApi::remove_rule(&self.inner, tag).await
        }
        async fn list_rules(&self) -> anyhow::Result<Vec<(String, String)>> {
            super::super::XrayApi::list_rules(&self.inner).await
        }
    }

    #[tokio::test]
    async fn xray_authorization_publication_is_fenced_and_cancellation_releases_it() {
        let h = residential_access_harness().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Semaphore::new(0));
        let shared = Arc::new(
            Shared::new(
                Box::new(PausingXray {
                    inner: h.xray.clone(),
                    entered: entered.clone(),
                    resume,
                    pause_once: std::sync::atomic::AtomicBool::new(true),
                    pause_in_add: false,
                    pause_inventory_at: None,
                    inventory_reads: std::sync::atomic::AtomicUsize::new(0),
                }),
                Box::new(h.hy2.clone()),
            )
            .with_hy2resi(Box::new(h.hy2resi.clone())),
        );
        shared.set_paths(&h.paths);
        let sync_ctx = ctx_of(&h);
        let sync_shared = shared.clone();
        let sync =
            tokio::spawn(
                async move { sync_users(&sync_ctx, &sync_shared, &BTreeSet::new()).await },
            );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        let store = h.store.clone();
        let mut writer =
            tokio::spawn(async move { store.update(|s| s.users[0].disabled = true).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut writer)
                .await
                .is_err(),
            "revoke cannot publish between the authorization read and an in-flight Xray grant"
        );
        sync.abort();
        assert!(sync.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            h.store.read().await.users[0].disabled,
            "cancel must release the existing Store writer fence"
        );
        let outcome = sync_now(&ctx_of(&h), &shared).await;
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(
            !h.xray.calls().iter().any(|c| c.starts_with("add:")),
            "the cancelled round cannot leave a delayed authorized grant behind"
        );
    }

    #[tokio::test]
    async fn a_cancelled_remote_grant_is_revoked_even_after_the_user_is_deleted() {
        let h = residential_access_harness().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let shared = Arc::new(
            Shared::new(
                Box::new(PausingXray {
                    inner: h.xray.clone(),
                    entered: entered.clone(),
                    resume: Arc::new(tokio::sync::Semaphore::new(0)),
                    pause_once: std::sync::atomic::AtomicBool::new(true),
                    pause_in_add: true,
                    pause_inventory_at: None,
                    inventory_reads: std::sync::atomic::AtomicUsize::new(0),
                }),
                Box::new(h.hy2.clone()),
            )
            .with_hy2resi(Box::new(h.hy2resi.clone())),
        );
        shared.set_paths(&h.paths);
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        // Establish previous revoke markers first: the subsequent grant attempt must
        // invalidate them even when the remote acknowledgement is lost to cancellation.
        let denied = sync_users(&ctx, &shared, &BTreeSet::from([id])).await;
        assert!(denied.errors.is_empty(), "{:?}", denied.errors);
        h.xray.with(|i| assert!(i.users.is_empty()));
        h.xray.clear_calls();
        let sync_ctx = ctx.clone();
        let sync_shared = shared.clone();
        let sync =
            tokio::spawn(
                async move { sync_users(&sync_ctx, &sync_shared, &BTreeSet::new()).await },
            );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        h.xray.with(|i| {
            assert!(
                i.users.contains_key(&("vless-direct".into(), id)),
                "remote side effect must have happened before cancellation"
            )
        });
        sync.abort();
        assert!(sync.await.unwrap_err().is_cancelled());
        h.store.update(|s| s.users.clear()).await.unwrap();
        let out = sync_now(&ctx, &shared).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        h.xray.with(|i| {
            assert!(
                i.users.is_empty(),
                "an unacknowledged remote grant must not outlive its deleted account"
            )
        });
        h.xray.clear_calls();
        sync_now(&ctx, &shared).await;
        assert!(
            h.xray.calls().is_empty(),
            "successful orphan cleanup must converge to no writes"
        );
    }

    #[tokio::test]
    async fn pending_quota_crossed_during_final_inventory_is_revoked_before_completion() {
        let h = residential_access_harness().await;
        h.store
            .update(|s| s.users[0].entitlements.traffic_limit.total_bytes = Some(10))
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Semaphore::new(0));
        let shared = Arc::new(
            Shared::new(
                Box::new(PausingXray {
                    inner: h.xray.clone(),
                    entered: entered.clone(),
                    resume: resume.clone(),
                    pause_once: std::sync::atomic::AtomicBool::new(false),
                    pause_in_add: false,
                    pause_inventory_at: Some(3),
                    inventory_reads: std::sync::atomic::AtomicUsize::new(0),
                }),
                Box::new(h.hy2.clone()),
            )
            .with_hy2resi(Box::new(h.hy2resi.clone())),
        );
        shared.set_paths(&h.paths);
        let id = h.store.read().await.users[0].user_id;
        let ctx = ctx_of(&h);
        let worker_shared = shared.clone();
        let worker = tokio::spawn(async move { sync_now(&ctx, &worker_shared).await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        h.xray.with(|i| {
            assert_eq!(
                i.users.len(),
                2,
                "positive grants must have reached both inlets before the quota transition"
            )
        });
        shared.pending().await.insert(id, TxRx { tx: 11, rx: 0 });
        resume.add_permits(1);
        let out = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        h.xray.with(|i| {
            assert!(
                i.users.is_empty(),
                "final observations cannot commit a grant after live quota revocation"
            )
        });
        assert!(snapshot::read(&shared.snapshot_path()).users["alice"].blocked);
        assert!(!shared.applied().await.xray_users.contains_key(&id));
        assert!(out.newly_blocked.contains(&id));
    }

    #[tokio::test]
    async fn a_fresh_daemon_removes_unknown_live_identities_without_touching_healthy_users() {
        let h = residential_access_harness().await;
        let u = h.store.read().await.users[0].clone();
        let orphan = Uuid::from_u128(0xbad);
        h.xray.with(|i| {
            for tag in XRAY_INBOUND_TAGS {
                i.users
                    .insert((tag.into(), u.user_id), u.credentials.vless_uuid);
                i.users
                    .insert((tag.into(), orphan), Uuid::from_u128(0xcafe));
            }
        });
        let fresh = Shared::new(Box::new(h.xray.clone()), Box::new(h.hy2.clone()))
            .with_hy2resi(Box::new(h.hy2resi.clone()));
        fresh.set_paths(&h.paths);
        let out = sync_now(&ctx_of(&h), &fresh).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{orphan}"),
                format!("remove:vless-residential:{orphan}")
            ]
        );
        h.xray.with(|i| {
            assert_eq!(
                i.users.len(),
                2,
                "only the two authorized inlet identities remain"
            );
            for tag in XRAY_INBOUND_TAGS {
                assert_eq!(
                    i.users.get(&(tag.into(), u.user_id)),
                    Some(&u.credentials.vless_uuid)
                );
            }
        });
        h.xray.clear_calls();
        let again = sync_now(&ctx_of(&h), &fresh).await;
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert!(
            h.xray.calls().is_empty(),
            "inventory reconciliation is a no-op after actual cleanup"
        );
    }

    #[tokio::test]
    async fn an_unreadable_xray_inventory_cannot_be_reported_as_a_converged_grant() {
        let h = residential_access_harness().await;
        let id = h.store.read().await.users[0].user_id;
        h.xray.with(|i| {
            i.fail_on.insert("inventory:vless-direct".into());
        });
        let out = sync_now(&ctx_of(&h), &h.shared).await;
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains("inventory") && e.contains("vless-direct")),
            "inventory failure must be visible as an authorization observation failure: {:?}",
            out.errors
        );
        assert!(
            !h.xray
                .calls()
                .iter()
                .any(|c| c.starts_with("add:vless-direct:")),
            "an unreadable inlet must not receive new grants"
        );
        assert!(
            !h.shared.applied().await.xray_users.contains_key(&id),
            "missing kernel truth cannot be counted as fully synchronized"
        );
    }

    #[tokio::test]
    async fn residential_only_credentials_cannot_be_kept_on_the_direct_inbound() {
        let h = residential_access_harness().await;
        h.store
            .update(|s| s.users[0].entitlements.direct = false)
            .await
            .unwrap();
        let u = h.store.read().await.users[0].clone();
        h.xray.with(|i| {
            i.users
                .insert(("vless-direct".into(), u.user_id), u.credentials.vless_uuid);
            i.users.insert(
                ("vless-residential".into(), u.user_id),
                u.credentials.vless_uuid,
            );
        });
        let out = sync_users(&ctx_of(&h), &h.shared, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-direct:{}", u.user_id)]
        );
        h.xray.with(|i| {
            assert!(!i.users.contains_key(&("vless-direct".into(), u.user_id)));
            assert_eq!(
                i.users.get(&("vless-residential".into(), u.user_id)),
                Some(&u.credentials.vless_uuid)
            );
        });
    }

    #[tokio::test]
    async fn direct_only_credentials_cannot_be_kept_on_the_residential_inbound() {
        let h = harness().await;
        h.store
            .update(|s| s.users[0].entitlements.residential = None)
            .await
            .unwrap();
        let u = h.store.read().await.users[0].clone();
        h.xray.with(|i| {
            i.users
                .insert(("vless-direct".into(), u.user_id), u.credentials.vless_uuid);
            i.users.insert(
                ("vless-residential".into(), u.user_id),
                u.credentials.vless_uuid,
            );
        });
        let out = sync_users(&ctx_of(&h), &h.shared, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-residential:{}", u.user_id)]
        );
        h.xray.with(|i| {
            assert_eq!(
                i.users.get(&("vless-direct".into(), u.user_id)),
                Some(&u.credentials.vless_uuid)
            );
            assert!(!i
                .users
                .contains_key(&("vless-residential".into(), u.user_id)));
        });
    }

    #[tokio::test]
    async fn residential_pool_loss_revokes_only_that_inbound_and_recovery_is_idempotent() {
        let h = residential_access_harness().await;
        let ctx = ctx_of(&h);
        let u = h.store.read().await.users[0].clone();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        h.store
            .update(|s| s.residential.groups.get_mut(DEFAULT_GROUP).unwrap().enabled = false)
            .await
            .unwrap();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-residential:{}", u.user_id)]
        );
        h.xray.clear_calls();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            h.xray.calls().is_empty(),
            "repeated deny must not remove the authorized direct user"
        );
        h.store
            .update(|s| s.residential.groups.get_mut(DEFAULT_GROUP).unwrap().enabled = true)
            .await
            .unwrap();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(
            h.xray.calls(),
            vec![format!(
                "add:vless-residential:{}:{}",
                u.user_id, u.credentials.vless_uuid
            )]
        );
        h.xray.clear_calls();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            h.xray.calls().is_empty(),
            "restored access must converge to a no-op"
        );
        assert_eq!(
            h.shared.applied().await.xray_users.len(),
            1,
            "the health count is users, not inbounds"
        );
    }

    #[test]
    fn missing_residential_binding_is_not_projected_as_a_real_slot_or_ip() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        bui_schema::slots::sync_slots(&mut s.residential);
        s.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(Uuid::from_u128(0xdead));
        let p = project_all(&s, &BTreeSet::new());
        assert_eq!(p[0].slot, None);
        assert_eq!(p[0].slot_ip, None);
        assert_eq!(p[0].slot_port, None);
    }

    #[tokio::test]
    async fn sync_writes_the_snapshot_and_adds_reality_users_to_both_inbounds() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.snapshot_written);
        assert_eq!(out.added, vec![id]);
        assert!(out.removed.is_empty());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("add:vless-direct:{id}:{vid}"),
                format!("add:vless-residential:{id}:{vid}")
            ]
        );
        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert_eq!(snap.users["alice"].blocked, false);
    }

    #[tokio::test]
    async fn a_second_sync_touches_neither_the_snapshot_nor_grpc() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(!out.snapshot_written, "内容没变就不该写盘");
        assert!(out.added.is_empty());
        assert!(h.xray.calls().is_empty(), "幂等：第二轮零 gRPC 调用");
    }

    #[tokio::test]
    async fn blocking_a_user_removes_him_from_both_inbounds_and_marks_the_snapshot() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id]);
        assert_eq!(out.newly_blocked, vec![id], "供采样任务去 kick");
        assert!(out.snapshot_written);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("remove:vless-residential:{id}")
            ]
        );
        assert!(
            crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked
        );
        // 再同步一次不重复报 newly_blocked
        h.xray.clear_calls();
        let again = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert!(again.newly_blocked.is_empty());
        assert!(h.xray.calls().is_empty());
    }

    /// 决策 D6 的核心回归：`Applied` 只活在进程内，守护进程重启后它是空的，
    /// 而 `xray-config.json` 里还带着被封用户的 clients ⇒ 第一轮同步就必须无条件 RemoveUser。
    #[tokio::test]
    async fn a_blocked_user_is_removed_even_if_this_process_never_added_him() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id], "没 AddUser 过也要删（spec §4.2）");
        assert!(out.added.is_empty());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("remove:vless-residential:{id}")
            ]
        );
        // `applied.xray_removed` 去重：第二轮零 gRPC
        h.xray.clear_calls();
        let again = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert!(again.removed.is_empty());
        assert!(h.xray.calls().is_empty(), "{:?}", h.xray.calls());
    }

    #[tokio::test]
    async fn a_disabled_user_is_removed_on_the_first_sync_too() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].disabled = true;
            })
            .await
            .unwrap();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.removed, vec![id], "禁用与超限同样处理（spec §4.2）");
        assert!(
            crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked
        );
    }

    #[tokio::test]
    async fn a_failed_remove_falls_back_to_the_xray_cli() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        h.host.with(|i| {
            i.which.insert("xray".into());
        });
        h.xray.with(|i| {
            i.fail_on.insert(format!("remove:vless-direct:{id}"));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id], "退路成功就算删掉了");
        assert!(
            h.host.ops().contains(&format!(
                "run:xray api rmu --server=127.0.0.1:10085 -tag=vless-direct {id}"
            )),
            "没走 CLI 退路：{:?}",
            h.host.ops()
        );
    }

    #[tokio::test]
    async fn a_remove_that_reports_not_found_needs_no_cli_fallback() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.host.with(|i| {
            i.which.insert("xray".into());
        });
        // xray 侧本来就没有这个 email（刚重启 / 从没 AddUser 过）⇒ 目标已达成
        h.xray.with(|i| {
            for tag in XRAY_INBOUND_TAGS {
                let key = format!("remove:{tag}:{id}");
                i.fail_on.insert(key.clone());
                i.error_text.insert(key, format!("User {id} not found."));
            }
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id]);
        assert!(
            out.errors.is_empty(),
            "「不存在」不算失败：{:?}",
            out.errors
        );
        assert!(
            !h.host.ops().iter().any(|o| o.contains("api rmu")),
            "「不存在」不该再跑 CLI 退路：{:?}",
            h.host.ops()
        );
    }

    /// **`GetInboundUsers` 读不到时**才走的那条退路：xray 报「已存在」= 位置被占，
    /// 一律摘掉再加一次 —— 不报 error，但也**不**把它当成「目标已达成」
    /// （位置上挂的可能正是要换掉的旧 uuid）。
    #[tokio::test]
    async fn an_add_that_reports_already_exists_is_healed_by_removing_the_squatter() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        // 读不到内核状态（xray 掉线 / 老内核没这个 RPC）⇒ 退回错误串那一路；
        // 而 xray 侧其实已有同 email（`applied` 只活在进程内）
        let key = format!("add:vless-direct:{id}:{vid}");
        h.xray.with(|i| {
            i.fail_on.insert(format!("get:vless-direct:{id}"));
            i.fail_once.insert(key.clone());
            i.error_text
                .insert(key, format!("User {id} already exists."));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors.is_empty(),
            "摘掉再加成功了就不记 error：{:?}",
            out.errors
        );
        assert_eq!(out.added, vec![id], "记进 applied，第二轮就不再 AddUser");
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("add:vless-direct:{id}:{vid}"),
                format!("remove:vless-direct:{id}"),
                format!("add:vless-direct:{id}:{vid}"),
                format!("add:vless-residential:{id}:{vid}"),
            ],
            "只有报「已存在」的那个 inbound 需要摘掉再加"
        );
    }

    /// 回归：轮换在 gRPC 推送前遇上 b-ui 重启 —— `applied` 只活在进程内
    /// （`panel/mod.rs` 的 `Applied` 注释自己写了这件事），新进程里是空的，
    /// 而 xray 进程没重启、里面仍挂着**旧** uuid。
    ///
    /// 判据是内核自己（`GetInboundUsers`），不是记账：读回来是旧 uuid 就照样先摘再加。
    /// 一旦哪天又靠记账判，这一轮只会发 AddUser、撞「已存在」；把那条错误吃成成功
    /// 就等于把旧 uuid 记成新 uuid —— 泄露的旧凭据一直有效到 xray 下次重启
    /// （`render::xray::structural_hash` 剥掉了 clients，对账不会重启它），
    /// journal 里一条错误都没有，而面板与 rotate 回包都声称换过了。
    #[tokio::test]
    async fn a_rotation_lost_to_a_daemon_restart_still_reaches_xray() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        // 第一轮：xray 里挂上旧 uuid
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let new = h.store.read().await.users[0].credentials.vless_uuid;

        // 「b-ui 重启」：换一份全新的 `Shared`（记账清空），xray 与 hysteria 还是同两个假内核
        // （假内核里仍挂着旧 uuid —— 它就是这一步唯一的事实来源）
        let restarted = Arc::new(Shared::new(
            Box::new(h.xray.clone()),
            Box::new(h.hy2.clone()),
        ));
        restarted.set_paths(&h.paths);
        h.xray.clear_calls();

        let out = sync_users(&ctx, &restarted, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.added, vec![id]);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("add:vless-direct:{id}:{new}"),
                format!("remove:vless-residential:{id}"),
                format!("add:vless-residential:{id}:{new}"),
            ],
            "两个 inbound 都要把旧 uuid 摘掉再挂新的"
        );
        // 新进程的记账指向新 uuid ⇒ 下一轮零请求（不会每 60 秒 remove+add 抖一次）
        h.xray.clear_calls();
        let out2 = sync_users(&ctx, &restarted, &BTreeSet::new()).await;
        assert!(out2.errors.is_empty(), "{:?}", out2.errors);
        assert!(h.xray.calls().is_empty(), "{:?}", h.xray.calls());
    }

    /// 审查意见③：腾位置的 RemoveUser 与 `to_remove` 那一路同口径 —— gRPC 失败要走
    /// `xray api rmu` CLI 退路（决策 D12 / 调研 X9），退路成功就照常把新 uuid 挂上去。
    /// 不走退路的后果是这一轮连 AddUser 都发不出去，而那条 error 大概率连 incident
    /// 都不会变成（`XrayGrpcUnavailable` 签名要求日志里同时出现 `unavailable`）。
    #[tokio::test]
    async fn freeing_the_email_falls_back_to_the_xray_cli() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let new = h.store.read().await.users[0].credentials.vless_uuid;
        h.host.with(|i| {
            i.which.insert("xray".into());
        });
        h.xray.with(|i| {
            i.fail_on.insert(format!("remove:vless-direct:{id}"));
        });
        h.xray.clear_calls();

        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors.is_empty(),
            "退路成功就不记 error：{:?}",
            out.errors
        );
        assert_eq!(out.added, vec![id]);
        assert!(
            h.host.ops().contains(&format!(
                "run:xray api rmu --server=127.0.0.1:10085 -tag=vless-direct {id}"
            )),
            "没走 CLI 退路：{:?}",
            h.host.ops()
        );
        assert!(
            h.xray
                .calls()
                .contains(&format!("add:vless-direct:{id}:{new}")),
            "位置腾出来了就要把新 uuid 挂上去：{:?}",
            h.xray.calls()
        );
    }

    /// 2026-09-14 裁决的回归：uuid 变了（轮换、或面板给 Reality 用户改 UUID）就必须
    /// **先 RemoveUser 再 AddUser**。原来只发 AddUser，xray 报「已存在」又被
    /// `target_already_reached` 记成成功 ⇒ 新 uuid 要等 xray 重启才生效，而
    /// `render::xray::structural_hash` 剥掉了 clients，对账也不会重启它。
    #[tokio::test]
    async fn rotating_a_uuid_removes_the_old_one_before_adding_the_new_one() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let old = h.store.read().await.users[0].credentials.vless_uuid;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;

        // uuid 没变 ⇒ 一个 gRPC 请求都不发
        h.xray.clear_calls();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(h.xray.calls().is_empty(), "{:?}", h.xray.calls());

        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let new = h.store.read().await.users[0].credentials.vless_uuid;
        assert_ne!(new, old);
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.added, vec![id]);
        assert!(out.removed.is_empty(), "换 uuid 不是「删用户」");
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("add:vless-direct:{id}:{new}"),
                format!("remove:vless-residential:{id}"),
                format!("add:vless-residential:{id}:{new}"),
            ],
            "每个 inbound 都要先 remove 再 add"
        );

        // 记账已经指向新 uuid ⇒ 再同步一轮零请求
        h.xray.clear_calls();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(h.xray.calls().is_empty(), "{:?}", h.xray.calls());
    }

    /// 位置腾过了、AddUser 还是报「已存在」（摘掉再加也没成）= 目标根本没达成：
    /// 必须记成失败交给 60 秒安全网重试，**绝不**记账。
    ///
    /// 审查意见②的另一半：这一刻这个用户已经被摘出 `vless-direct`、还没加回去，
    /// 60 秒内他在这个 inbound 上握不了新手 —— 错误串要把这件事说出来，
    /// 否则运维只看到「加不上」，不知道人已经掉了。
    #[tokio::test]
    async fn an_already_exists_while_replacing_a_uuid_is_a_real_error() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let new = h.store.read().await.users[0].credentials.vless_uuid;

        let add_key = format!("add:vless-direct:{id}:{new}");
        h.xray.with(|i| {
            i.fail_on.insert(add_key.clone());
            i.error_text
                .insert(add_key, format!("User {id} already exists."));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains("AddUser vless-direct") && e.contains("已被摘出 vless-direct")),
            "错误串要点明用户已被摘出内核、等下一轮补回：{:?}",
            out.errors
        );
        assert!(out.added.is_empty(), "没换成就不许记账，等安全网重试");
    }

    /// 腾位置的那次 RemoveUser 自己失败（gRPC 失败 + CLI 退路也不可用）：
    /// 这一轮连 AddUser 都不发（发了也只会再撞一次「已存在」），另一个 inbound 照旧推进。
    #[tokio::test]
    async fn a_failed_remove_while_replacing_a_uuid_skips_the_add() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let new = h.store.read().await.users[0].credentials.vless_uuid;
        h.xray.with(|i| {
            i.fail_on.insert(format!("remove:vless-direct:{id}"));
        });
        h.xray.clear_calls();

        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors
                .iter()
                .any(|e| e.contains("换 uuid 前 RemoveUser vless-direct")),
            "{:?}",
            out.errors
        );
        assert!(out.added.is_empty(), "没换成就不许记账");
        assert!(
            !h.xray
                .calls()
                .contains(&format!("add:vless-direct:{id}:{new}")),
            "{:?}",
            h.xray.calls()
        );
        assert!(
            h.xray
                .calls()
                .contains(&format!("add:vless-residential:{id}:{new}")),
            "另一个 inbound 照旧推进：{:?}",
            h.xray.calls()
        );
    }

    /// 审查意见②【必改】的回归：b-ui 或 xray 一重启 `applied` 就清空，全部健康 Reality
    /// 用户都会重新走一遍 add 那一路。uuid 一个字节没变时必须**零写** —— 盲目「摘掉再加」
    /// 会给每个人开一个「已摘出、还没加回」的窗口，那一步失败（inbound 正在重载、
    /// 或事实④的 `handler not found`）就把一个本来好着的用户摘下线，
    /// 要等 60 秒安全网才补回来，而旧实现在这个方向上零风险。
    #[tokio::test]
    async fn a_restart_does_not_touch_a_user_whose_uuid_has_not_changed() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let (id, vid) = {
            let s = h.store.read().await;
            (s.users[0].user_id, s.users[0].credentials.vless_uuid)
        };
        // 第一轮：假内核里挂上这个用户
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;

        // 「b-ui 重启」：记账清空，内核状态不变
        let restarted = Arc::new(Shared::new(
            Box::new(h.xray.clone()),
            Box::new(h.hy2.clone()),
        ));
        restarted.set_paths(&h.paths);
        h.xray.clear_calls();
        // 审查者那条序列的下半截，当陷阱布在这里：万一真发了 AddUser，它会撞「已存在」
        // （真应答），于是走「摘掉再加」，而重加那次同样失败（inbound 正在重载）——
        // 旧实现就是在这里把一个本来好着的用户摘出内核的。先读后写根本不发这次 AddUser，
        // 所以这个陷阱必须一次都踩不到。
        let add_key = format!("add:vless-direct:{id}:{vid}");
        h.xray.with(|i| {
            i.fail_on.insert(add_key.clone());
            i.error_text
                .insert(add_key, format!("User {id} already exists."));
        });

        let out = sync_users(&ctx, &restarted, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.added, vec![id], "读到内核里已经是对的 ⇒ 照样补上记账");
        assert!(
            h.xray.calls().is_empty(),
            "uuid 没变就一个写请求都不许发：{:?}",
            h.xray.calls()
        );
        assert_eq!(
            h.xray.gets(),
            vec![
                format!("get:vless-direct:{id}"),
                format!("get:vless-residential:{id}"),
            ],
            "只读，每个 inbound 各一次"
        );
        // 最要紧的一条断言：这个健康用户此刻还挂在内核上。旧实现走到这里时他已经被
        // RemoveUser 摘出去、重加又失败，要等 60 秒安全网才补回来。
        let mut mounted = None;
        h.xray
            .with(|i| mounted = i.users.get(&("vless-direct".to_string(), id)).copied());
        assert_eq!(mounted, Some(vid), "uuid 没变的健康用户绝不许被摘出内核");
    }

    /// 轮换后鉴权快照重写：新 hy2 密码通过、旧密码被拒。判定走的是钩子与 http 鉴权
    /// 共用的那一份 `auth_hook::decide`，所以这条就是真机上「旧密码还能连吗」的答案。
    #[tokio::test]
    async fn a_rotated_hy2_password_is_the_only_one_the_auth_snapshot_accepts() {
        use crate::modules::panel::auth_hook::{decide, Decision};
        let h = harness().await;
        let ctx = ctx_of(&h);
        let old_pw = h.store.read().await.users[0]
            .credentials
            .hy2_password
            .clone();
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;

        h.store.update(|s| rotate(&mut s.users[0])).await.unwrap();
        let user = h.store.read().await.users[0].clone();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.snapshot_written, "凭据变了必须重写鉴权快照");

        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert_eq!(
            decide(
                &snap,
                &format!("alice:{}", user.credentials.hy2_password),
                t0()
            ),
            Decision::Allow {
                user_id: user.user_id.to_string()
            }
        );
        assert_eq!(
            decide(&snap, &format!("alice:{old_pw}"), t0()),
            Decision::Deny {
                reason: "bad-password"
            },
            "旧密码必须当场被拒"
        );
    }

    #[tokio::test]
    async fn an_add_that_fails_for_real_is_recorded_as_an_error() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        // 默认错误串（不含 already / not found）：真失败
        h.xray.with(|i| {
            i.fail_on.insert(format!("add:vless-direct:{id}:{vid}"));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors.iter().any(|e| e.contains("vless-direct")),
            "真失败要记 error：{:?}",
            out.errors
        );
        assert!(
            out.added.is_empty(),
            "有 inbound 没加成功就不能记成已同步，留给 60 秒安全网重试"
        );
    }

    /// 决策 D6：xray 重启会从 `xray-config.json` 把全部 clients（含被封的）读回来，
    /// 所以记账要清空、差分要重放。重放的是**读**：配置里已经是对的 uuid 就零写
    /// （审查意见②），落后的 uuid 才摘掉换新。
    #[tokio::test]
    async fn an_xray_restart_replays_the_whole_add_set() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "0".into());
        });
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "1".into());
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.added.len(), 1, "重启后必须重放一遍");
        assert_eq!(h.xray.gets().len(), 2, "两个 inbound 各读一次");
        assert!(
            h.xray.calls().is_empty(),
            "配置里的 uuid 已经是对的 ⇒ 不许摘挂一遍：{:?}",
            h.xray.calls()
        );

        // 配置落后（`xray-config.json` 还是轮换前那份）⇒ 重放时把旧 uuid 换掉
        h.xray.with(|i| {
            i.users
                .insert(("vless-direct".into(), id), Uuid::from_u128(1));
        });
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "2".into());
        });
        h.xray.clear_calls();
        let out2 = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out2.added, vec![id]);
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("add:vless-direct:{id}:{vid}"),
            ],
            "只有落后的那个 inbound 要动"
        );
    }

    #[tokio::test]
    async fn an_xray_restart_replays_the_removal_of_a_blocked_user() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "0".into());
        });
        sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        h.xray.clear_calls();
        // 决策 D6：重启把 xray-config.json 里被封用户的 clients 又读回来了
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "1".into());
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(
            out.removed,
            vec![id],
            "重启后 RemoveUser 也要重放，不只是 AddUser"
        );
        assert_eq!(
            h.xray.calls(),
            vec![
                format!("remove:vless-direct:{id}"),
                format!("remove:vless-residential:{id}")
            ]
        );
    }

    #[tokio::test]
    async fn sync_now_derives_the_blocked_set_from_state_and_pending() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
            })
            .await
            .unwrap();
        h.shared.pending().await.insert(id, TxRx { tx: 50, rx: 0 });
        let out = sync_now(&ctx, &h.shared).await;
        assert_eq!(out.newly_blocked, vec![id]);
        assert!(
            crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked
        );
    }

    #[tokio::test]
    async fn reality_only_users_never_reach_the_snapshot_but_do_reach_xray() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.store
            .update(|s| {
                s.users[0].entitlements.protocols = vec![Protocol::Reality];
            })
            .await
            .unwrap();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.added.len(), 1);
        assert!(
            crate::modules::panel::snapshot::read(&h.shared.snapshot_path())
                .users
                .is_empty()
        );
    }
}
