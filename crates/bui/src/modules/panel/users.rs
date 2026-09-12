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
use bui_schema::model::{
    Billing, Credentials, Entitlements, NodeParams, PortalAuth, Protocol, ResidentialEntitlement,
    State, TrafficLimit, Usage, User, DEFAULT_GROUP,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

pub const GIB: u64 = 1_073_741_824;
pub const SYNC_INTERVAL_SECS: u64 = 60;

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
    pub sni: String,
    pub residential: bool,
    pub disabled: bool,
    pub blocked: bool,
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

pub fn project(u: &User, node: &NodeParams, blocked: &BTreeSet<Uuid>) -> PanelUser {
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
        // v4 没有 per-user sni：面板与订阅都用全局 REALITY 伪装域（v3.5.13 的修复口径）
        sni: node.reality.sni().to_string(),
        residential: u.entitlements.residential.is_some(),
        disabled: u.disabled,
        blocked: u.disabled || blocked.contains(&u.user_id),
    }
}

pub fn project_all(state: &State, blocked: &BTreeSet<Uuid>) -> Vec<PanelUser> {
    state
        .users
        .iter()
        .map(|u| project(u, &state.node, blocked))
        .collect()
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
    let protocols = protocols_from_label(req.protocol.as_deref().unwrap_or("hysteria2"))?;
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
        },
        entitlements: Entitlements {
            protocols,
            direct: true,
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
    })
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
pub async fn sync_users(ctx: &DaemonCtx, shared: &Shared, blocked: &BTreeSet<Uuid>) -> SyncOutcome {
    // 轮次锁：采样任务与事件反应器都会调本函数，同一时刻只许一轮。
    // 注意它与 `applied` 是两把锁 —— `applied` 只在算差分与写回记账时短暂加锁，
    // 绝不跨 gRPC await 持有（否则 xray 掉线时 `/api/users/health` 会跟着卡）。
    let _turn = shared.sync_guard().await;
    let state = ctx.store.read().await;
    let mut out = SyncOutcome::default();

    // ① 快照（hysteria 侧的主保障）
    let snap = Snapshot::from_state(&state, blocked);
    let path = shared.snapshot_path();
    match snapshot::write_if_changed(&path, &snap) {
        Ok(written) => out.snapshot_written = written,
        Err(e) => out.errors.push(format!("写 auth-snapshot.json 失败：{e}")),
    }

    // ② xray 重启侦测（决策 D6）：重启会把 xray-config.json 里的全部 clients 读回来，
    //    包括被封的那些，所以必须清空两份记账、重放差分。
    let host = ctx.host.clone();
    let restarts =
        tokio::task::spawn_blocking(move || host.unit_property("xray", "NRestarts").ok().flatten())
            .await
            .ok()
            .flatten();

    // ③ 期望态：只有「未禁用、未被封、有 Reality 权益」的用户该留在两个 inbound 里
    let desired: BTreeMap<Uuid, Uuid> = state
        .users
        .iter()
        .filter(|u| {
            !u.disabled
                && !blocked.contains(&u.user_id)
                && u.entitlements.protocols.contains(&Protocol::Reality)
        })
        .map(|u| (u.user_id, u.credentials.vless_uuid))
        .collect();
    // 该被拒的 Reality 用户：**无条件**删（决策 D6）。不看 `applied.xray_users`——
    // 那份记账只活在本进程内，守护进程重启后是空的，而 `xray-config.json` 里
    // 还带着他们的凭据（P1 Task 10 传的是全量 `state.users`）。
    let mut want_removed: BTreeSet<Uuid> = state
        .users
        .iter()
        .filter(|u| {
            u.entitlements.protocols.contains(&Protocol::Reality)
                && !desired.contains_key(&u.user_id)
        })
        .map(|u| u.user_id)
        .collect();

    // ④ 短暂加锁算差分
    let (to_add, to_remove) = {
        let mut applied = shared.applied().await;
        applied.snapshot_sha = Some(snap.sha256());
        if applied.xray_restarts != restarts {
            if applied.xray_restarts.is_some() {
                tracing::info!(?restarts, "xray 重启过，重放 gRPC 用户差分");
            }
            applied.xray_restarts = restarts;
            applied.xray_users.clear();
            applied.xray_removed.clear();
        }
        // 已从面板删掉的用户不在 `state` 里，只能靠 `applied.xray_users` 发现
        for id in applied.xray_users.keys() {
            if !desired.contains_key(id) {
                want_removed.insert(*id);
            }
        }
        let to_add: Vec<(Uuid, Uuid)> = desired
            .iter()
            .filter(|(id, vid)| applied.xray_users.get(id) != Some(vid))
            .map(|(id, vid)| (*id, *vid))
            .collect();
        let to_remove: Vec<Uuid> = want_removed
            .iter()
            .filter(|id| !applied.xray_removed.contains(id))
            .copied()
            .collect();
        out.newly_blocked = blocked.difference(&applied.blocked).copied().collect();
        applied.blocked = blocked.clone();
        (to_add, to_remove)
    };

    // ⑤ gRPC 差分（不持 `applied`）。两个 inbound 都成功才记账，否则留给 60 秒安全网重试。
    let mut added: Vec<(Uuid, Uuid)> = Vec::new();
    for (id, vid) in to_add {
        let mut ok = true;
        for tag in XRAY_INBOUND_TAGS {
            if let Err(e) = shared.xray().add_user(tag, id, vid).await {
                let msg = e.to_string();
                if target_already_reached(&msg) {
                    tracing::debug!(tag, %id, "AddUser 报已存在，按成功处理");
                } else {
                    ok = false;
                    out.errors.push(format!("AddUser {tag} 失败：{msg}"));
                }
            }
        }
        if ok {
            added.push((id, vid));
            out.added.push(id);
        }
    }
    let mut removed: Vec<Uuid> = Vec::new();
    for id in to_remove {
        let mut ok = true;
        for tag in XRAY_INBOUND_TAGS {
            if let Err(e) = shared.xray().remove_user(tag, id).await {
                let msg = e.to_string();
                if target_already_reached(&msg) {
                    // xray 侧本来就没有这个 email ⇒ 目标已达成，不必跑 CLI 退路
                    tracing::debug!(tag, %id, "RemoveUser 报不存在，按成功处理");
                } else if !cli_remove(ctx, tag, &id.to_string()).await {
                    // CLI 退路（X9、决策 D12）：RemoveUser 这条方向不自愈，必须补上
                    ok = false;
                    out.errors.push(format!("RemoveUser {tag} 失败：{msg}"));
                }
            }
        }
        if ok {
            removed.push(id);
            out.removed.push(id);
        }
    }

    // ⑥ 短暂加锁写回记账
    {
        let mut applied = shared.applied().await;
        for (id, vid) in added {
            applied.xray_users.insert(id, vid);
            // 恢复过的用户将来再被封，还要能再删一次
            applied.xray_removed.remove(&id);
        }
        for id in removed {
            applied.xray_users.remove(&id);
            applied.xray_removed.insert(id);
        }
        // 已从 state 里删掉的用户不必再记账（同一个 user_id 不会回来），顺手别让这个集合长胖
        let known: BTreeSet<Uuid> = state.users.iter().map(|u| u.user_id).collect();
        applied.xray_removed.retain(|id| known.contains(id));
    }
    // 用户集合变了 ⇒ Xray 的槽规则表要跟着增删（D7），置脏交给对账末尾收敛
    if !out.added.is_empty() || !out.removed.is_empty() {
        crate::modules::residential::slots::mark_xray_rules_dirty(&ctx.runtime).await;
    }
    out
}

/// xray 的 `AddUser` 撞上「email 已存在」、`RemoveUser` 撞上「email 不存在」时都返回错误，
/// 而这两种情形下目标状态其实已经达成，所以按成功处理。
///
/// 错误文案本身**没有实机核对过**（调研 X1–X11 只覆盖 proto 与服务端调用链，没记错误串），
/// 所以这里宽匹配三个子串；M3 真内核联调（bwg-rick）时按 xray 的实际文案核对一次，
/// 对不上就把子串补齐 —— 匹配失败的后果只是多记一条 error + 多跑一次 CLI 退路（幂等），不会误判成功。
fn target_already_reached(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("already") || m.contains("not found") || m.contains("not exist")
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

fn report(out: SyncOutcome) {
    for e in &out.errors {
        tracing::warn!(error = %e, "用户同步有失败项，下一轮安全网会重试");
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
    use crate::modules::panel::testsupport::{harness, Harness};
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
        let v = serde_json::to_value(project(alice(&s), &s.node, &BTreeSet::new())).unwrap();
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
    fn projection_marks_blocked_users() {
        let s = sample_state();
        let id = alice(&s).user_id;
        let p = project(alice(&s), &s.node, &BTreeSet::from([id]));
        assert!(p.blocked);
        assert_eq!(project_all(&s, &BTreeSet::from([id])).len(), 1);
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

    #[test]
    fn new_user_converts_days_and_gigabytes_like_v3() {
        let req = CreateRequest {
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

    #[test]
    fn apply_update_follows_v3_semantics() {
        let mut s = sample_state();
        let u = &mut s.users[0];
        apply_update(
            u,
            &UpdateRequest {
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

    #[tokio::test]
    async fn an_add_that_reports_already_exists_is_not_an_error() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        // 装机后第一轮的真实情形：xray 侧已有同 email（`applied` 只活在进程内）
        let key = format!("add:vless-direct:{id}:{vid}");
        h.xray.with(|i| {
            i.fail_on.insert(key.clone());
            i.error_text
                .insert(key, format!("User {id} already exists."));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors.is_empty(),
            "「已存在」按成功处理：{:?}",
            out.errors
        );
        assert_eq!(out.added, vec![id], "记进 applied，第二轮就不再 AddUser");
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

    #[tokio::test]
    async fn an_xray_restart_replays_the_whole_add_set() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "0".into());
        });
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        // 决策 D6：xray 重启会从 xray-config.json 把全部 clients（含被封的）读回来
        h.host.with(|i| {
            i.unit_props
                .insert(("xray.service".into(), "NRestarts".into()), "1".into());
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.added.len(), 1, "重启后必须重放一遍");
        assert_eq!(h.xray.calls().len(), 2);
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
