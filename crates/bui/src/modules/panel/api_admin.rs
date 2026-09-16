//! 管理员 API（spec §4.3）。路径与响应形状沿用 v3（`web/app.js` 不改），四处改动见计划的契约表。
//!
//! 所有 handler 只改 `state` 并发 `Event::StateChanged(..)`：快照重写与 Xray gRPC 差分由
//! `users::sync_loop` 统一做（幂等 + 60 秒安全网），内核配置重渲染与重启映射由 P1 的对账做。

use super::users;
use super::Shared;
use crate::api::{AppState, Event};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::model::State as BuiState;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

fn ok_json<T: Serialize>(v: T) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

/// v3 对坏请求体回 `400 {"error":"请求格式错误（JSON 解析失败）"}`（`web/server.js:1948-1955`），
/// 而 axum 的 `Json<T>` 提取器回 422 + 自己的文案，所以这里手工解析。
///
/// 返回 `Option` 而不是 `Result<T, Response>`：`Response` 有 128 字节，
/// 塞进 `Err` 会打红 `clippy::result_large_err`。空体与坏 JSON 的回包完全一样，
/// 所以区分二者没有意义，失败一律由 [`bad_json`] 生成回包。
fn parse_json<T: DeserializeOwned>(body: &Bytes) -> Option<T> {
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice(body).ok()
}

fn bad_json() -> Response {
    fail(StatusCode::BAD_REQUEST, "请求格式错误（JSON 解析失败）")
}

/// 这个用户该有住宅 HY2 凭据却没拿到（池的 id 域 256 条用尽）⇒ 记一条 Error 级事件。
///
/// spec §3.1：空闲耗尽时建用户 / 轮换**不拒绝**，当场扩容；扩到上限还分不出来就只能如实
/// 告警 —— 静默下去的表现是「有住宅权益但订阅里没有住宅 HY2 节点」，从面板上看不出原因。
async fn report_pool_exhausted_for(app: &AppState, user_id: uuid::Uuid) {
    let missing = {
        let s = app.store.read().await;
        s.users
            .iter()
            .find(|u| u.user_id == user_id)
            .filter(|u| super::gates::has_resi_hy2(u))
            .filter(|u| bui_schema::hy2pool::cred_of(u, &s.residential).is_none())
            .map(|u| u.username.clone())
    };
    let Some(username) = missing else {
        return;
    };
    tracing::error!(
        user = %username,
        "住宅 HY2 凭据池分不出凭据，该用户渲染不出住宅 HY2 节点"
    );
    let inc = crate::modules::sentinel::incidents::Incident {
        at: crate::util::fmt_rfc3339(app.host.now()),
        unit: "b-ui".into(),
        signature: crate::modules::residential::slots::POOL_EXHAUSTED_SIG.into(),
        subject: username,
        action: "分配住宅 HY2 凭据".into(),
        result: format!(
            "凭据池上限 {} 条已用尽，该用户暂无住宅 HY2 节点",
            bui_schema::hy2pool::POOL_MAX
        ),
        level: crate::modules::sentinel::incidents::Level::Error,
        sample: None,
    };
    app.runtime
        .update(move |rt| crate::modules::sentinel::incidents::push(rt, inc))
        .await;
}

/// `GET /api/config` 的响应（形状逐字段照 v3 `getConfig`，`web/server.js:392-489`）
pub fn config_payload(s: &BuiState) -> serde_json::Value {
    let ph = match s.node.ports.hy2_hop {
        Some((start, end)) => json!({"enabled": true, "start": start, "end": end}),
        // v3 在没有区间时给的也是这对默认值（`getConfig` 的 portHopping 初值）
        None => json!({"enabled": false, "start": 20000, "end": 30000}),
    };
    json!({
        "domain": s.node.domain,
        // v3 的 `port` 来自正则捕获，是**字符串**；前端直接拼进 URL，这里保持同型
        "port": s.node.ports.hy2.to_string(),
        "xrayPort": s.node.ports.reality_direct,
        "pubKey": s.node.reality.public_key,
        "shortId": s.node.reality.short_id(),
        "sni": s.node.reality.sni(),
        "portHopping": ph,
        "obfs": {
            "enabled": s.node.obfs.enabled,
            "type": if s.node.obfs.enabled { "salamander" } else { "" },
            "password": s.node.obfs.password,
        },
    })
}

/// `GET /api/masquerade` 的响应
pub fn masquerade_payload(s: &BuiState) -> serde_json::Value {
    let d = s.node.reality.sni();
    json!({"masqueradeUrl": format!("https://{d}/"), "masqueradeDomain": d})
}

/// 移植 v3 的 `b.url.replace(/https?:\/\/([^/:]+).*/, "$1")`（`web/server.js:2588`）。
pub fn host_of(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', ':']).next().unwrap_or("").to_string()
}

/// `runtime` → v3 `/api/hy2/watchdog/status` 的形状（兼容 shim，见契约表 #21）。
///
/// `last_run_at` / `next_run_at` 原样取 watchdog 每轮写进 `runtime.extra[RUN_KEY]` 的时间戳
/// （上一轮的时刻 + 一个间隔），没跑过一轮时两者都是 `null`。
pub fn watchdog_payload(rt: &crate::state::runtime::RuntimeData) -> serde_json::Value {
    let w = &rt.watchdog;
    let fail_count: u32 = w.values().map(|r| r.fails).sum();
    let run = rt.extra.get(crate::modules::watchdog::RUN_KEY);
    let stamp = |k: &str| {
        run.and_then(|v| v.get(k))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let mut lines: Vec<String> = w
        .iter()
        .filter_map(|(unit, r)| {
            r.last_restart_at.as_ref().map(|t| {
                format!(
                    "{t} {unit} 已重启 {} 次（连续失败 {}）",
                    r.restarts, r.fails
                )
            })
        })
        .collect();
    lines.sort();
    lines.reverse();
    lines.truncate(5);
    json!({
        // v4 的 watchdog 是守护进程内的常驻任务，没有 timer；只要面板答得出这一问，它就在跑
        "watchdog_active": true,
        "next_run_at": stamp("next_run_at"),
        "last_run_at": stamp("last_run_at"),
        "fail_count": fail_count,
        "log_recent_lines": lines,
    })
}

async fn blocked_now(app: &AppState, shared: &Shared) -> std::collections::BTreeSet<uuid::Uuid> {
    let now = app.host.now();
    let pending = shared.pending().await.clone();
    let state = app.store.read().await;
    users::blocked_set(&state, &pending, now)
}

async fn list_users(State(app): State<AppState>, shared: Arc<Shared>) -> Response {
    let blocked = blocked_now(&app, &shared).await;
    let state = app.store.read().await;
    ok_json(users::project_all(&state, &blocked))
}

async fn create_user(State(app): State<AppState>, body: Bytes) -> Response {
    let Some(req) = parse_json::<users::CreateRequest>(&body) else {
        return bad_json();
    };
    let user = match users::new_user(&req, app.host.now()) {
        Ok(u) => u,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    let mut dup = false;
    let to_push = user.clone();
    let uid = to_push.user_id;
    if let Err(e) = app
        .store
        .update(|s| {
            if s.users.iter().any(|u| u.username == to_push.username) {
                dup = true;
                return;
            }
            s.users.push(to_push);
            // spec §5.6 规则 1：新建用户分到用户数最少的槽，与 push 同一次写盘
            crate::modules::residential::slots::assign_new_user(s, uid);
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    if dup {
        return fail(StatusCode::BAD_REQUEST, "用户名已存在");
    }
    // spec §3.1：池的 id 域用尽（256 条）时**不拒绝建用户**，但要记一条 Error 级事件 ——
    // 没有凭据就渲染不出他的住宅 HY2 节点，运维必须看得见。
    report_pool_exhausted_for(&app, uid).await;
    // 新用户要有自己那条 `resi-u-<user_id>` 槽规则（D7），置脏交给对账末尾收敛
    crate::modules::residential::slots::mark_xray_rules_dirty(&app.runtime).await;
    app.bus.send(Event::StateChanged("users"));
    let sni = app.store.read().await.node.reality.sni().to_string();
    ok_json(json!({
        "success": true,
        "user": user.username,
        // 建号即生成（`users::new_user`）；与 `rotate_user` 同口径一起回包，
        // 省掉前端「建完再 GET /api/users 才拿得到订阅链接末段」那一次往返（审查意见④）
        "subToken": user.sub_token,
        "password": user.credentials.hy2_password,
        "uuid": user.credentials.vless_uuid,
        // 回显真正生效的 SNI（v4 全局唯一），请求里的 `sni` 被忽略（决策 D13）
        "sni": sni,
    }))
}

async fn update_user(
    State(app): State<AppState>,
    Path(username): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(e) = users::validate_username(&username) {
        return fail(StatusCode::BAD_REQUEST, format!("URL 中的 {e}"));
    }
    let Some(req) = parse_json::<users::UpdateRequest>(&body) else {
        return bad_json();
    };
    let now = app.host.now();
    let mut missing = false;
    let mut problem: Option<String> = None;
    let mut new_name = username.clone();
    if let Err(e) = app
        .store
        .update(|s| {
            if let Some(n) = &req.username {
                if n != &username && s.users.iter().any(|u| &u.username == n) {
                    problem = Some("Username already exists".into());
                    return;
                }
            }
            match s.users.iter().position(|u| u.username == username) {
                None => missing = true,
                Some(i) => {
                    // 改在副本上，成功才写回：`apply_update` 中途报错不能留下半改的用户
                    let mut copy = s.users[i].clone();
                    match users::apply_update(&mut copy, &req, now) {
                        Ok(()) => {
                            new_name = copy.username.clone();
                            s.users[i] = copy;
                        }
                        Err(e) => problem = Some(e),
                    }
                }
            }
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    if missing {
        return fail(StatusCode::NOT_FOUND, "User not found");
    }
    if let Some(e) = problem {
        return fail(StatusCode::BAD_REQUEST, e);
    }
    app.bus.send(Event::StateChanged("users"));
    ok_json(json!({"success": true, "user": new_name}))
}

/// `POST /api/users/{username}/rotate`（2026-09-14 裁决）：换订阅 token + hy2 密码 + vless uuid，
/// 并停用这个用户的「用户名链接」。请求体是空对象（没有可调项，所以一个字节都不读）。
///
/// 回包直接给出新凭据 —— 与 `create_user` 同口径：这条路由在 `require_admin` 里面，
/// 面板本来就在显示每个用户的密码与 uuid（`PanelUser`）。
///
/// 顺手把这个用户**已经建好**的 hysteria2 会话踢下线（审查意见④）：两条鉴权路径都只在
/// **握手时**过 `auth_hook::decide`，xray 的 RemoveUser/AddUser 同样只影响新握手，
/// 所以不踢的话拿着泄露凭据的那一方照旧有流量，直到连接自己断 —— 那就不叫轮换了。
/// 踢是 best-effort（失败只记 warn）：新凭据已经落盘生效，踢不动只是旧会话多活一会儿。
///
/// URL 里的用户名**只用来查**，不跑 `validate_username`（审查意见③，与 `delete_user` 同口径）：
/// `v3.rs` 的导入原样照抄 v3 的 `username`、v3 的 `server/core.sh` 也能直接往用户表里塞人，
/// 所以存量里可能有不合今天规则的名字。卡在校验上会让这种用户能改、能删、能取订阅，
/// 却永远轮换不了 —— 而他恰恰是最该轮换的那个。查不到一律 404。
///
/// 两个已知边界，都不在本次范围内：
/// - Reality 那条**已建立**的连接没有对应手段（xray 没有 kick），要等它自己断；
/// - 踢的是「这一刻的期望态里该有的」全部 hy2 实例（`traffic::stats_ports`），
///   对内核没起来的实例会记一条 warn。
async fn rotate_user(
    State(app): State<AppState>,
    Path(username): Path<String>,
    shared: Arc<Shared>,
) -> Response {
    let now = app.host.now();
    let mut rotated: Option<bui_schema::model::User> = None;
    // 住宅 HY2 的旧 / 新凭据 id：换凭据必须显式「先 release 再 assign」（`hy2pool::assign`
    // 是幂等的，不 release 就会原样拿回旧凭据 ⇒ 轮换等于没换）
    let mut old_cred: Option<String> = None;
    let mut new_cred: Option<String> = None;
    if let Err(e) = app
        .store
        .update(|s| {
            let Some(idx) = s.users.iter().position(|u| u.username == username) else {
                return;
            };
            users::rotate(&mut s.users[idx]);
            let uid = s.users[idx].user_id;
            let resi = super::gates::has_resi_hy2(&s.users[idx]);
            // 释放记 `released_at` ⇒ 24 小时冷却期，旧凭据不会立刻发给下一个人
            old_cred = bui_schema::hy2pool::release(s, uid, now);
            if resi {
                new_cred = crate::modules::residential::slots::assign_hy2_cred(s, uid);
            }
            rotated = Some(s.users[idx].clone());
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    let Some(u) = rotated else {
        return fail(StatusCode::NOT_FOUND, "User not found");
    };
    // 两次 PUT（spec §3.3）：旧凭据的门当场切 `deny`（`interrupt_exist_connections` ⇒ 拿着
    // 旧订阅的那一方存量流立刻断），新凭据的门开到他自己那一槽。只靠 60 秒收敛不行 ——
    // 轮换的整个理由就是「现在就让旧凭据失效」。
    if let Some(old) = old_cred.as_deref() {
        super::gates::put_gate(&shared, old, bui_schema::render::hy2_singbox::DENY_TAG).await;
    }
    if let Some(new) = new_cred.as_deref() {
        let tag = {
            let s = app.store.read().await;
            bui_schema::render::hy2_singbox::slot_out_tag(bui_schema::slots::index_of_user(
                &u,
                &s.residential,
            ))
        };
        super::gates::put_gate(&shared, new, &tag).await;
    }
    // 该有凭据却没拿到（id 域用尽）⇒ Error 级事件（spec §3.1）
    report_pool_exhausted_for(&app, u.user_id).await;
    // 凭据变了 ⇒ 重写鉴权快照 + 把 xray 里的旧 uuid 换掉，都由 `users::sync_loop` 收敛。
    // 事件先发、再踢：两条鉴权路径据此刷新，被踢的客户端拿旧密码重连时已经会被拒。
    app.bus.send(Event::StateChanged("users"));
    let ports = super::traffic::stats_ports(app.store.read().await.as_ref());
    let ids = [u.user_id.to_string()];
    for port in ports {
        if let Err(e) = shared.hy2().kick(port, &ids).await {
            tracing::warn!(port, error = %e, "轮换后踢下线失败（旧会话可能还在跑）");
        }
    }
    tracing::info!(user = %u.username, "已轮换订阅凭据并停用该用户的用户名链接");
    ok_json(json!({
        "success": true,
        "user": u.username,
        "subToken": u.sub_token,
        "password": u.credentials.hy2_password,
        "uuid": u.credentials.vless_uuid,
    }))
}

async fn delete_user(State(app): State<AppState>, Path(username): Path<String>) -> Response {
    let mut removed = false;
    if let Err(e) = app
        .store
        .update(|s| {
            let before = s.users.len();
            s.users.retain(|u| u.username != username);
            removed = s.users.len() != before;
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    if !removed {
        return fail(StatusCode::NOT_FOUND, "User not found");
    }
    // 他那条 `resi-u-<user_id>` 槽规则要删掉（D7），置脏交给对账末尾收敛
    crate::modules::residential::slots::mark_xray_rules_dirty(&app.runtime).await;
    app.bus.send(Event::StateChanged("users"));
    ok_json(json!({"success": true}))
}

async fn get_stats(shared: Arc<Shared>) -> Response {
    ok_json(shared.cache().await.stats.clone())
}

async fn get_online(shared: Arc<Shared>) -> Response {
    ok_json(shared.cache().await.online.clone())
}

/// 面板的「断开」按钮（`web/app.js` 的 `kickUsers`）。
///
/// 直连仍是 `POST /kick` 打 `traffic::stats_ports`（4.1 起只有 `:9999`，住宅那个
/// `trafficStats` 端口没人监听了）；住宅走门 + `/connections`：切 `deny` 掐断存量流、
/// 逐条 DELETE 兜底，**未被判拒的人再把门切回他自己的槽**（spec §5.2「未封者下一请求
/// 即通」）。`success` 只报直连那半的结果 —— 住宅那条是 best-effort，主保障是快照拒绝
/// 与门位收敛。
async fn kick(State(app): State<AppState>, shared: Arc<Shared>, body: Bytes) -> Response {
    let Some(names) = parse_json::<Vec<String>>(&body) else {
        return bad_json();
    };
    let blocked = blocked_now(&app, &shared).await;
    let (ports, ids, targets) = {
        let state = app.store.read().await;
        let picked: Vec<uuid::Uuid> = state
            .users
            .iter()
            .filter(|u| names.contains(&u.username))
            .map(|u| u.user_id)
            .collect();
        let open = picked
            .iter()
            .copied()
            .filter(|id| !blocked.contains(id))
            .collect();
        let targets = super::traffic::resi_kick_targets(state.as_ref(), &picked, &open);
        (
            super::traffic::stats_ports(state.as_ref()),
            picked.iter().map(uuid::Uuid::to_string).collect::<Vec<_>>(),
            targets,
        )
    };
    let mut all_ok = true;
    for port in ports {
        if let Err(e) = shared.hy2().kick(port, &ids).await {
            tracing::warn!(port, error = %e, "kick 失败");
            all_ok = false;
        }
    }
    super::traffic::kick_residential(&shared, &targets).await;
    ok_json(json!({"success": all_ok, "kicked": ids.len()}))
}

async fn get_config(State(app): State<AppState>) -> Response {
    let s = app.store.read().await;
    ok_json(config_payload(&s))
}

async fn set_password(State(app): State<AppState>, body: Bytes) -> Response {
    let Some(v) = parse_json::<serde_json::Value>(&body) else {
        return bad_json();
    };
    let pw = v.get("newPassword").and_then(|x| x.as_str()).unwrap_or("");
    if pw.chars().count() < 6 {
        return fail(StatusCode::BAD_REQUEST, "密码至少6位");
    }
    let hash = match crate::api::auth::hash_password(pw) {
        Ok(h) => h,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("哈希失败：{e}")),
    };
    // 决策 D11：轮换 JWT 密钥，让旧 token 立即失效（v3 靠重启进程达到同样效果）
    let secret = hex::encode(rand::random::<[u8; 32]>());
    // 日志里关于密码一个字都不留（连长度也不留：`redact::secret` 会漏出字符数），
    // 只记「这件事发生过」。
    tracing::info!("管理员密码已更新并轮换 JWT 密钥");
    if let Err(e) = app
        .store
        .update(|s| {
            s.admin.password_hash = hash;
            s.admin.jwt_secret = secret;
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    ok_json(json!({"success": true, "message": "密码已更新，请重新登录"}))
}

async fn get_masquerade(State(app): State<AppState>) -> Response {
    let s = app.store.read().await;
    ok_json(masquerade_payload(&s))
}

async fn set_masquerade(State(app): State<AppState>, body: Bytes) -> Response {
    let Some(v) = parse_json::<serde_json::Value>(&body) else {
        return bad_json();
    };
    let Some(url) = v.get("url").and_then(|x| x.as_str()) else {
        return fail(StatusCode::BAD_REQUEST, "URL required");
    };
    let domain = host_of(url);
    if domain.is_empty() || !domain.contains('.') {
        return fail(StatusCode::BAD_REQUEST, "URL required");
    }
    let d = domain.clone();
    if let Err(e) = app
        .store
        .update(|s| {
            s.node.reality.dest = format!("{d}:443");
            s.node.reality.server_names = vec![d];
        })
        .await
    {
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Save failed: {e}"),
        );
    }
    // 对账会重写 xray-config.json（结构哈希变 ⇒ 重启 xray）与两份 hysteria 配置
    // （`masquerade.proxy.url` 由 `reality.sni()` 推导 ⇒ 重启两个 hysteria）
    app.bus.send(Event::StateChanged("masquerade"));
    ok_json(json!({"success": true, "domain": domain}))
}

async fn get_bandwidth() -> Response {
    // 决策 D5：v4 的 hysteria 配置固定 `ignoreClientBandwidth: true`，没有服务端带宽设置
    ok_json(json!({"up": 0, "down": 0}))
}

async fn set_bandwidth() -> Response {
    fail(
        StatusCode::NOT_IMPLEMENTED,
        "v4 不再设置服务端带宽：config.yaml 固定 ignoreClientBandwidth: true",
    )
}

async fn get_port_hopping(State(app): State<AppState>) -> Response {
    let s = app.store.read().await;
    match s.node.ports.hy2_hop {
        Some((start, end)) => ok_json(json!({"enabled": true, "start": start, "end": end})),
        None => ok_json(json!({"enabled": false, "start": 20000, "end": 30000})),
    }
}

async fn set_port_hopping(State(app): State<AppState>, body: Bytes) -> Response {
    let Some(v) = parse_json::<serde_json::Value>(&body) else {
        return bad_json();
    };
    let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
    let start = v.get("start").and_then(|x| x.as_u64()).unwrap_or(20000) as u16;
    let end = v.get("end").and_then(|x| x.as_u64()).unwrap_or(30000) as u16;
    if enabled && start >= end {
        return fail(StatusCode::BAD_REQUEST, "起始端口必须小于结束端口");
    }
    let want = if enabled { Some((start, end)) } else { None };
    // 值没变就不发事件：`StateChanged("ports")` 会触发一轮去抖对账，而对账把 `listen:` 行
    // 判成「要改」时会**重启 hysteria-server**。前端的「关掉→再关掉」不该踢掉所有连接。
    let changed = app.store.read().await.node.ports.hy2_hop != want;
    if changed {
        if let Err(e) = app.store.update(|s| s.node.ports.hy2_hop = want).await {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Save failed: {e}"),
            );
        }
        // 审计 web-C13：v3 在这里写 iptables REDIRECT；v4 改 `listen:` 行 + 重启，零 iptables 规则
        app.bus.send(Event::StateChanged("ports"));
    }
    ok_json(json!({"success": true, "enabled": enabled, "start": start, "end": end}))
}

async fn watchdog_status(State(app): State<AppState>) -> Response {
    ok_json(watchdog_payload(&app.runtime.read().await))
}

/// 裁决 D2：`/api/health` 不加用户段，摘要由这条管理员端点透出（写入侧是 T7 的 `write_health_summary`）
pub fn users_health_payload(rt: &crate::state::runtime::RuntimeData) -> serde_json::Value {
    rt.extra
        .get("users")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

async fn users_health(State(app): State<AppState>) -> Response {
    ok_json(users_health_payload(&app.runtime.read().await))
}

/// 全部端点都要求 JWT（由 P1 的 `require_admin` 统一拦），路径与响应形状照 v3
pub fn routes(shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    let s_list = shared.clone();
    let s_stats = shared.clone();
    let s_online = shared.clone();
    let s_kick = shared.clone();
    let s_rotate = shared.clone();
    axum::Router::new()
        .route(
            "/api/users",
            get(move |st: State<AppState>| list_users(st, s_list.clone())).post(create_user),
        )
        .route(
            "/api/users/{username}",
            axum::routing::put(update_user).delete(delete_user),
        )
        .route(
            "/api/users/{username}/rotate",
            post(move |st: State<AppState>, p: Path<String>| rotate_user(st, p, s_rotate.clone())),
        )
        .route("/api/stats", get(move || get_stats(s_stats.clone())))
        .route("/api/online", get(move || get_online(s_online.clone())))
        .route(
            "/api/kick",
            post(move |st: State<AppState>, body: Bytes| kick(st, s_kick.clone(), body)),
        )
        .route("/api/config", get(get_config))
        .route("/api/password", post(set_password))
        .route("/api/masquerade", get(get_masquerade).post(set_masquerade))
        .route("/api/bandwidth", get(get_bandwidth).post(set_bandwidth))
        .route(
            "/api/port-hopping",
            get(get_port_hopping).post(set_port_hopping),
        )
        .route("/api/hy2/watchdog/status", get(watchdog_status))
        // 静态段优先于 `/api/users/{username}`（matchit 的静态优先规则），所以它不会被
        // 路径参数吃掉；`{username}` 只挂 PUT / DELETE，GET 也不冲突
        .route("/api/users/health", get(users_health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, send, token, Harness};
    use crate::modules::panel::TxRx;
    use pretty_assertions::assert_eq;

    async fn app(h: &Harness) -> (axum::Router, String) {
        (mount(&h.app, routes(h.shared.clone())), token(h).await)
    }

    /// 给 state 建住宅凭据池（迁移口径：alice 拿 `r000`，凭据 `name` = 用户名），
    /// 住宅那条踢人路径要靠它才有门 tag 可算。
    async fn with_pool(h: &Harness) {
        let now = crate::sys::Host::now(h.host.as_ref());
        h.store
            .update(|s| {
                bui_schema::hy2pool::migrate(s, now);
            })
            .await
            .unwrap();
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(host_of("https://www.bing.com/"), "www.bing.com");
        assert_eq!(host_of("http://www.bing.com"), "www.bing.com");
        assert_eq!(host_of("https://a.example.com:8443/x?y=1"), "a.example.com");
        assert_eq!(host_of("www.bing.com"), "www.bing.com");
        assert_eq!(host_of(""), "");
    }

    #[tokio::test]
    async fn config_matches_the_v3_shape() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/config", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["domain"], "example.com");
        assert_eq!(v["port"], "10000", "v3 的 port 是字符串");
        assert_eq!(v["xrayPort"], 10001);
        assert_eq!(v["pubKey"], "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c");
        assert_eq!(v["shortId"], "0123456789abcdef");
        assert_eq!(v["sni"], "www.bing.com");
        assert_eq!(
            v["portHopping"],
            serde_json::json!({"enabled": true, "start": 20000, "end": 30000})
        );
        assert_eq!(
            v["obfs"],
            serde_json::json!({"enabled": false, "type": "", "password": ""})
        );
    }

    #[tokio::test]
    async fn users_list_is_the_v3_projection() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/users", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["username"], "alice");
        assert_eq!(arr[0]["protocol"], "fusion");
        assert_eq!(arr[0]["usage"]["total"], 0);
    }

    #[tokio::test]
    async fn creating_a_user_uses_post_with_jwt_and_emits_a_state_change() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let mut rx = h.app.bus.subscribe();
        let (s, v) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({
                "username": "bob", "days": 30, "traffic": 1.5, "monthly": 0,
                "protocol": "fusion", "residential": true, "sni": "ignored", "speed": 100
            })),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["user"], "bob");
        assert_eq!(v["password"].as_str().unwrap().len(), 16);
        assert_eq!(
            v["sni"], "www.bing.com",
            "回显真正生效的 SNI，不是请求里那个（决策 D13）"
        );
        assert!(v["uuid"].as_str().is_some());
        // 审查意见④：建号即生成 token，回包就带上，前端不必再 GET /api/users 拼链接
        let tok = v["subToken"].as_str().expect("回包必须带 subToken");
        assert!(bui_schema::sub::is_sub_token(tok), "{tok}");
        assert_eq!(
            h.store.read().await.users[1].sub_token.as_deref(),
            Some(tok),
            "回包给的就是落盘的那一个"
        );
        assert_eq!(h.store.read().await.users.len(), 2);
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("users")
        );
    }

    /// 面板新建用户当场拿一条住宅 HY2 凭据（spec §3.3 建用户那一行）：少了它，
    /// 他刷出来的订阅里压根没有住宅 HY2 节点，而且没有任何提示。
    #[tokio::test]
    async fn creating_a_user_mints_his_residential_cred_right_away() {
        let h = harness().await;
        with_pool(&h).await;
        let (r, t) = app(&h).await;
        let (s, _) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({
                "username": "bob", "days": 30, "protocol": "fusion", "residential": true
            })),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let st = h.store.read().await;
        let bob = st.users.iter().find(|u| u.username == "bob").unwrap();
        let c = bui_schema::hy2pool::cred_of(bob, &st.residential).expect("建号即分凭据");
        assert_eq!(c.name, c.id, "新发凭据的 name = id（spec §3.1）");
        assert!(
            bui_schema::nodes::nodes_for(bob, &st.node, &st.residential)
                .iter()
                .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential),
            "新用户的订阅里要有住宅 HY2 节点"
        );
        // 门位由下一轮 `sync_users` 收敛（`hy2-residential.json` 一个字节都不动）
        assert!(
            h.hy2resi.calls().is_empty(),
            "建用户不该在 handler 里直接动内核：{:?}",
            h.hy2resi.calls()
        );
    }

    /// 池的 id 域用尽时**不拒绝建用户**，但要留一条 Error 级事件（spec §3.1）
    #[tokio::test]
    async fn creating_a_user_when_the_pool_is_exhausted_still_succeeds_but_alerts() {
        let h = harness().await;
        h.store
            .update(|s| {
                // 256 条全被占满：id 域用尽 ⇒ `grow` 一条都补不出来
                s.residential.hy2_pool.creds = (0..bui_schema::hy2pool::POOL_MAX)
                    .map(|i| bui_schema::model::ReservedCred {
                        id: format!("r{i:03}"),
                        name: format!("r{i:03}"),
                        secret: "x".repeat(22),
                        released_at: Some("2099-01-01T00:00:00Z".into()),
                    })
                    .collect();
            })
            .await
            .unwrap();
        let (r, t) = app(&h).await;
        let (s, v) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({
                "username": "bob", "days": 30, "protocol": "fusion", "residential": true
            })),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK, "建用户不许因为池满被拒");
        assert_eq!(v["success"], true);
        let inc = crate::modules::sentinel::incidents::from_runtime(&h.runtime.read().await);
        let err = inc
            .iter()
            .find(|i| i.signature == crate::modules::residential::slots::POOL_EXHAUSTED_SIG)
            .expect("池耗尽必须留一条 Error 级事件");
        assert_eq!(err.level, crate::modules::sentinel::incidents::Level::Error);
        assert_eq!(err.subject, "bob");
    }

    #[tokio::test]
    async fn create_rejects_duplicates_bad_names_bad_protocols_and_broken_json() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({"username": "alice"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "用户名已存在");
        let (s2, v2) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({"username": "a/b"})),
        )
        .await;
        assert_eq!(s2, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            v2["error"],
            "username 仅允许字母/数字/中文/下划线/连字符/点"
        );
        let (s3, _) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({"username": "c", "protocol": "vless-ws-tls"})),
        )
        .await;
        assert_eq!(s3, axum::http::StatusCode::BAD_REQUEST);
        // 空 / 坏请求体要回 v3 的 400 文案，不是 axum 默认的 422
        let (s4, v4) = send(&r, "POST", "/api/users", Some(&t), None).await;
        assert_eq!(s4, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v4["error"], "请求格式错误（JSON 解析失败）");
        assert_eq!(h.store.read().await.users.len(), 1, "失败请求不能改 state");
    }

    #[tokio::test]
    async fn update_and_delete_follow_the_v3_contract() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(
            &r,
            "PUT",
            "/api/users/alice",
            Some(&t),
            Some(
                serde_json::json!({"username": "alice2", "days": 10, "traffic": 2, "monthly": 0, "speed": 50}),
            ),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"success": true, "user": "alice2"}));
        assert_eq!(h.store.read().await.users[0].username, "alice2");
        let (s404, v404) = send(
            &r,
            "PUT",
            "/api/users/nope",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s404, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(v404["error"], "User not found");
        let (sd, vd) = send(&r, "DELETE", "/api/users/alice2", Some(&t), None).await;
        assert_eq!(sd, axum::http::StatusCode::OK);
        assert_eq!(vd, serde_json::json!({"success": true}));
        assert!(h.store.read().await.users.is_empty());
        assert_eq!(
            send(&r, "DELETE", "/api/users/alice2", Some(&t), None)
                .await
                .0,
            axum::http::StatusCode::NOT_FOUND
        );
    }

    /// `POST /api/users/{u}/rotate`（2026-09-14 裁决）：三样凭据一起换、回包给出新值、
    /// 停用这个用户的「用户名链接」，并发一次 `StateChanged` 让快照与 xray 收敛。
    #[tokio::test]
    async fn rotating_a_user_swaps_all_three_credentials_and_kills_his_username_link() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let before = h.store.read().await.users[0].clone();
        let mut rx = h.app.bus.subscribe();
        let (s, v) = send(
            &r,
            "POST",
            "/api/users/alice/rotate",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["user"], "alice");
        let after = h.store.read().await.users[0].clone();
        let token_now = after.sub_token.clone().expect("轮换后必须有 token");
        assert!(bui_schema::sub::is_sub_token(&token_now), "{token_now}");
        assert_ne!(after.sub_token, before.sub_token);
        assert_ne!(
            after.credentials.hy2_password,
            before.credentials.hy2_password
        );
        assert_ne!(after.credentials.vless_uuid, before.credentials.vless_uuid);
        assert!(
            after.legacy_sub_disabled,
            "轮换必须立刻停用用户名链接，否则旧链接还能取到新凭据"
        );
        // 回包就是落盘的那三样（与 create_user 同口径：这条路由在 require_admin 里面）
        assert_eq!(v["subToken"], token_now);
        assert_eq!(v["password"], after.credentials.hy2_password);
        assert_eq!(v["uuid"], after.credentials.vless_uuid.to_string());
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("users")
        );
        // 审查意见④：已经建好的 hy2 会话当场踢掉（hy2 只在握手时判凭据），
        // 踢的是这一刻期望态里的全部 trafficStats 实例、只带这一个 user_id ——
        // 4.1 起住宅不再是 apernet 实例（计量与踢人都在 sing-box 的两个回环面上，
        // `traffic::stats_ports` 只剩直连那一个端口），住宅侧的轮换是 T11 的事。
        assert_eq!(
            h.hy2.calls(),
            vec![format!("kick:9999:{}", before.user_id)],
            "轮换必须把旧会话踢下线，否则拿着泄露凭据的那一方照旧有流量"
        );
        // 面板列表跟着给出新 token（前端据此拼订阅链接）
        let (_, list) = send(&r, "GET", "/api/users", Some(&t), None).await;
        assert_eq!(list[0]["subToken"], token_now);
    }

    /// 轮换的住宅那半（spec §3.3）：释放旧凭据（记 `released_at` ⇒ 24 小时冷却期）、
    /// 分一条新的，并当场两次 PUT —— 旧门切 `deny`、新门开到他自己那一槽。
    /// 只靠 60 秒门位收敛不行：轮换的整个理由就是「现在就让旧凭据失效」。
    #[tokio::test]
    async fn rotate_releases_the_old_cred_and_assigns_a_new_one() {
        let h = harness().await;
        with_pool(&h).await;
        let (r, t) = app(&h).await;
        let before = {
            let s = h.store.read().await;
            s.users[0].credentials.hy2_resi_cred.clone().unwrap()
        };
        let (s, _) = send(
            &r,
            "POST",
            "/api/users/alice/rotate",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let st = h.store.read().await;
        let alice = &st.users[0];
        let after = alice.credentials.hy2_resi_cred.clone().unwrap();
        assert_ne!(
            before, after,
            "凭据必须真的换掉（assign 是幂等的，不 release 就白换）"
        );
        let calls = h.hy2resi.calls();
        assert!(
            calls.contains(&format!("select:gate-{before}:deny")),
            "{calls:?}"
        );
        assert!(
            calls.contains(&format!("select:gate-{after}:slot-0-out")),
            "{calls:?}"
        );
        // 旧凭据进了冷却期，不会立刻发给下一个人
        let old = st
            .residential
            .hy2_pool
            .creds
            .iter()
            .find(|c| c.id == before)
            .unwrap();
        assert!(old.released_at.is_some(), "释放必记 released_at");
        let c = bui_schema::hy2pool::cred_of(alice, &st.residential).unwrap();
        assert_eq!(c.name, c.id, "新发凭据的 name = id");
        assert_ne!(
            c.secret, alice.credentials.hy2_password,
            "住宅凭据与直连密码此后各走各的"
        );
        // 有凭据 ⇒ 订阅里仍有住宅 HY2 节点（换的是凭据，不是节点）
        assert!(
            bui_schema::nodes::nodes_for(alice, &st.node, &st.residential)
                .iter()
                .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential)
        );
    }

    /// 没有住宅 hysteria2 权益的用户轮换时不许白拿一条凭据（池是有限的）
    #[tokio::test]
    async fn rotating_a_direct_only_user_mints_no_residential_cred() {
        let h = harness().await;
        h.store
            .update(|s| {
                s.users[0].entitlements.residential = None;
            })
            .await
            .unwrap();
        with_pool(&h).await;
        let (r, t) = app(&h).await;
        send(
            &r,
            "POST",
            "/api/users/alice/rotate",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        let st = h.store.read().await;
        assert_eq!(st.users[0].credentials.hy2_resi_cred, None);
        assert!(
            !h.hy2resi.calls().iter().any(|c| c.contains("slot-")),
            "{:?}",
            h.hy2resi.calls()
        );
    }

    #[tokio::test]
    async fn rotating_an_unknown_username_changes_nothing() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let before = h.store.read().await.users[0].clone();
        let (s, v) = send(
            &r,
            "POST",
            "/api/users/ghost/rotate",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(v["error"], "User not found");
        // 不合今天规则的名字也只是「查不到」⇒ 404，不是 400（审查意见③）
        let (s2, v2) = send(&r, "POST", "/api/users/a%2Fb/rotate", Some(&t), None).await;
        assert_eq!(s2, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(v2["error"], "User not found");
        assert_eq!(h.store.read().await.users[0], before, "一个字段都不许动");
        assert!(h.hy2.calls().is_empty(), "也不许踢任何人下线");
    }

    /// 审查意见③：v3 导入 / v3 的 `server/core.sh` 都能留下不合今天规则的用户名
    /// （`validate_username` 只放过字母数字与 `_-.`）。这种用户能改、能删、能取订阅，
    /// 所以也必须能轮换 —— 他恰恰是最该轮换的那个。
    #[tokio::test]
    async fn a_legacy_username_that_fails_todays_rules_can_still_be_rotated() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let legacy = "alice@old";
        assert!(
            users::validate_username(legacy).is_err(),
            "这个名字今天建不出来，只可能是存量"
        );
        h.store
            .update(|s| s.users[0].username = legacy.into())
            .await
            .unwrap();
        let before = h.store.read().await.users[0].clone();
        let (s, v) = send(
            &r,
            "POST",
            "/api/users/alice%40old/rotate",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK, "{v}");
        assert_eq!(v["user"], legacy);
        let after = h.store.read().await.users[0].clone();
        assert_ne!(after.sub_token, before.sub_token);
        assert_ne!(after.credentials.vless_uuid, before.credentials.vless_uuid);
    }

    #[tokio::test]
    async fn renaming_onto_an_existing_username_is_rejected_and_changes_nothing() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({"username": "bob"})),
        )
        .await;
        let (s, v) = send(
            &r,
            "PUT",
            "/api/users/bob",
            Some(&t),
            Some(serde_json::json!({"username": "alice"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "Username already exists");
        let names: Vec<String> = h
            .store
            .read()
            .await
            .users
            .iter()
            .map(|u| u.username.clone())
            .collect();
        assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[tokio::test]
    async fn stats_and_online_come_from_the_shared_cache() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        {
            let mut c = h.shared.cache_mut().await;
            c.stats.insert("alice".into(), TxRx { tx: 7, rx: 8 });
            c.online.insert("alice".into(), 2);
        }
        let (_, v) = send(&r, "GET", "/api/stats", Some(&t), None).await;
        assert_eq!(v, serde_json::json!({"alice": {"tx": 7, "rx": 8}}));
        let (_, o) = send(&r, "GET", "/api/online", Some(&t), None).await;
        assert_eq!(o, serde_json::json!({"alice": 2}));
    }

    /// 面板「断开」按钮（`POST /api/kick`）：用户名换成 `user_id`；直连打
    /// `traffic::stats_ports`（**只剩 `:9999`** —— 4.1 的住宅是 sing-box，`:9998`
    /// 没人监听，再打它只会让接口恒回 `success: false`）；住宅走门 + `/connections`，
    /// **未封用户踢完门要回到他自己的槽**（spec §5.2「未封者下一请求即通」）。
    #[tokio::test]
    async fn kicking_an_unblocked_user_hits_the_direct_port_and_cycles_his_residential_gate() {
        let h = harness().await;
        with_pool(&h).await;
        let (r, t) = app(&h).await;
        let id = h.store.read().await.users[0].user_id;
        h.hy2resi.set_conns(vec![
            ("c1", "auth_user=alice => route(gate-r000)"),
            ("c9", "auth_user=r001 => route(gate-r001)"),
        ]);
        let (s, v) = send(
            &r,
            "POST",
            "/api/kick",
            Some(&t),
            Some(serde_json::json!(["alice", "ghost"])),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"success": true, "kicked": 1}));
        assert_eq!(
            h.hy2.calls(),
            vec![format!("kick:9999:{id}")],
            "住宅那个 trafficStats 端口不再被打"
        );
        assert_eq!(
            h.hy2resi.calls(),
            vec![
                "select:gate-r000:deny".to_string(),
                "connections".to_string(),
                "close:c1".to_string(),
                "select:gate-r000:slot-0-out".to_string(),
            ],
            "切 deny → 只关他自己那条连接 → 回切到他的槽"
        );
        assert_eq!(
            h.hy2resi.selected().get("gate-r000").map(String::as_str),
            Some("slot-0-out"),
            "未封用户被踢后门位必须回到 slot-<i>-out"
        );
    }

    /// 已被判拒的用户手动踢完**停在 `deny`**（spec §3.3 那一格：「已封者已在 deny」）——
    /// 踢一下不许把他的门开回去。
    #[tokio::test]
    async fn kicking_a_blocked_user_leaves_his_residential_gate_denied() {
        let h = harness().await;
        with_pool(&h).await;
        h.store
            .update(|s| s.users[0].disabled = true)
            .await
            .unwrap();
        let (r, t) = app(&h).await;
        let (s, _) = send(
            &r,
            "POST",
            "/api/kick",
            Some(&t),
            Some(serde_json::json!(["alice"])),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert!(
            !h.hy2resi
                .calls()
                .iter()
                .any(|c| c.starts_with("select:gate-r000:slot-")),
            "被封用户不许回切：{:?}",
            h.hy2resi.calls()
        );
        assert_eq!(
            h.hy2resi.selected().get("gate-r000").map(String::as_str),
            Some("deny")
        );
    }

    #[tokio::test]
    async fn changing_the_admin_password_rehashes_and_rotates_the_jwt_secret() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let before = h.store.read().await.admin.jwt_secret.clone();
        let (s, v) = send(
            &r,
            "POST",
            "/api/password",
            Some(&t),
            Some(serde_json::json!({"newPassword": "short"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "密码至少6位");
        let (s2, v2) = send(
            &r,
            "POST",
            "/api/password",
            Some(&t),
            Some(serde_json::json!({"newPassword": "newpass123"})),
        )
        .await;
        assert_eq!(s2, axum::http::StatusCode::OK);
        assert_eq!(
            v2,
            serde_json::json!({"success": true, "message": "密码已更新，请重新登录"})
        );
        let st = h.store.read().await;
        assert!(crate::api::auth::verify_password(
            &st.admin.password_hash,
            "newpass123"
        ));
        assert_ne!(st.admin.jwt_secret, before, "决策 D11：旧 token 立即失效");
    }

    /// 终审收尾：换密码时**日志与回包都不许出现密码的任何信息，连长度都不许**
    /// （`redact::secret` 会漏出字符数）；只留「管理员密码已更新」这件事本身可审计。
    ///
    /// 为什么锁源码而不是拿测试订阅器抓日志：tracing 的回调点 `Interest` 是**全局**缓存，
    /// 并行跑的别的测试先命中同一个 `info!`（当时没有任何订阅器）就会把它缓存成
    /// 「永不感兴趣」，本线程后装的订阅器再也收不到 —— 实测在整套 panel 测试里必然 flaky。
    #[test]
    fn set_password_logs_nothing_about_the_password() {
        let src = include_str!("api_admin.rs");
        let after = src
            .split_once("async fn set_password(")
            .expect("set_password 还在吧？")
            .1;
        let body = after
            .split_once("\nasync fn ")
            .map(|(b, _)| b)
            .unwrap_or(after);
        let logs: Vec<&str> = body.lines().filter(|l| l.contains("tracing::")).collect();
        assert_eq!(logs.len(), 1, "set_password 只该有那一条日志：{logs:?}");
        assert!(
            logs[0].contains("管理员密码已更新"),
            "换密码这件事仍要留痕：{logs:?}"
        );
        // `pw` / `redact::secret(pw)` / 任何字符数都不许进这一行
        for leak in ["pw", "secret", "redact", "count()", "len()"] {
            assert!(
                !logs[0].contains(leak),
                "这条日志里不许出现 {leak:?}（长度也算信息）：{}",
                logs[0]
            );
        }
    }

    #[tokio::test]
    async fn changing_the_admin_password_answers_without_echoing_it() {
        const PW: &str = "hunter2-correct-horse";
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(
            &r,
            "POST",
            "/api/password",
            Some(&t),
            Some(serde_json::json!({"newPassword": PW})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let body = v.to_string();
        let len = PW.chars().count().to_string();
        assert!(
            !body.contains(PW) && !body.contains(len.as_str()) && !body.contains("***"),
            "回包不许带密码的任何信息（含长度）：{body}"
        );
    }

    #[tokio::test]
    async fn masquerade_reads_and_writes_the_reality_dest() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (_, v) = send(&r, "GET", "/api/masquerade", Some(&t), None).await;
        assert_eq!(
            v,
            serde_json::json!({"masqueradeUrl": "https://www.bing.com/", "masqueradeDomain": "www.bing.com"})
        );
        let mut rx = h.app.bus.subscribe();
        let (s, out) = send(
            &r,
            "POST",
            "/api/masquerade",
            Some(&t),
            Some(serde_json::json!({"url": "https://www.apple.com/"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(
            out,
            serde_json::json!({"success": true, "domain": "www.apple.com"})
        );
        let st = h.store.read().await;
        assert_eq!(st.node.reality.dest, "www.apple.com:443");
        assert_eq!(
            st.node.reality.server_names,
            vec!["www.apple.com".to_string()]
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("masquerade")
        );
        let (bad, _) = send(
            &r,
            "POST",
            "/api/masquerade",
            Some(&t),
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(bad, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn port_hopping_writes_the_listen_range_instead_of_iptables() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (_, v) = send(&r, "GET", "/api/port-hopping", Some(&t), None).await;
        assert_eq!(
            v,
            serde_json::json!({"enabled": true, "start": 20000, "end": 30000})
        );
        let mut rx = h.app.bus.subscribe();
        let (s, out) = send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": true, "start": 21000, "end": 22000})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(
            out,
            serde_json::json!({"success": true, "enabled": true, "start": 21000, "end": 22000})
        );
        assert_eq!(
            h.store.read().await.node.ports.hy2_hop,
            Some((21000, 22000))
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("ports")
        );
        // 关掉 = 清空区间（对账会把 `listen:` 写回单端口并重启 hysteria-server）
        send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": false})),
        )
        .await;
        assert_eq!(h.store.read().await.node.ports.hy2_hop, None);
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("ports")
        );
        // 再关一次：值没变 ⇒ 不发事件。否则「关掉→关掉」会白触发一轮对账，
        // 而对账认为 `listen:` 要改时会重启 hysteria-server、踢掉所有在线连接。
        let (again, _) = send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": false})),
        )
        .await;
        assert_eq!(again, axum::http::StatusCode::OK, "幂等请求仍然回 200");
        assert!(
            rx.try_recv().is_err(),
            "值没变还发事件 ⇒ 白重启一次 hysteria-server"
        );
        // 起点不小于终点直接拒绝
        let (bad, bv) = send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": true, "start": 30000, "end": 20000})),
        )
        .await;
        assert_eq!(bad, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(bv["error"], "起始端口必须小于结束端口");
        assert!(
            h.host.ops().iter().all(|o| !o.starts_with("run:iptables")),
            "v4 不写任何 iptables 规则"
        );
    }

    #[tokio::test]
    async fn bandwidth_is_read_only_in_v4() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/bandwidth", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"up": 0, "down": 0}));
        let (s2, v2) = send(
            &r,
            "POST",
            "/api/bandwidth",
            Some(&t),
            Some(serde_json::json!({"up": 100, "down": 100})),
        )
        .await;
        assert_eq!(s2, axum::http::StatusCode::NOT_IMPLEMENTED);
        assert!(
            v2["error"]
                .as_str()
                .unwrap()
                .contains("ignoreClientBandwidth"),
            "{v2}"
        );
    }

    #[tokio::test]
    async fn the_watchdog_shim_keeps_the_v3_keys() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        h.runtime
            .update(|rt| {
                rt.watchdog.insert(
                    "hysteria-server".into(),
                    crate::state::runtime::WatchdogRecord {
                        fails: 2,
                        restarts: 1,
                        last_restart_at: Some("2026-09-11T00:00:00Z".into()),
                        backoff_until: None,
                    },
                );
                rt.extra.insert(
                    crate::modules::watchdog::RUN_KEY.into(),
                    crate::modules::watchdog::run_stamp(
                        time::macros::datetime!(2026-09-11 00:02:00 UTC),
                    ),
                );
            })
            .await;
        let (s, v) = send(&r, "GET", "/api/hy2/watchdog/status", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["watchdog_active"], true);
        assert_eq!(v["last_run_at"], "2026-09-11T00:02:00Z");
        assert_eq!(
            v["next_run_at"], "2026-09-11T00:03:00Z",
            "next_run_at = 上一轮 + 一个间隔，不再是 null"
        );
        assert_eq!(v["fail_count"], 2);
        let lines = v["log_recent_lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].as_str().unwrap().contains("hysteria-server"),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn the_watchdog_shim_reports_null_before_the_first_round() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/hy2/watchdog/status", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["last_run_at"], serde_json::Value::Null);
        assert_eq!(v["next_run_at"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn the_user_health_endpoint_serves_the_runtime_summary() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        // 还没采样过：空对象，不是 404、也不是 null（裁决 D2：摘要只在这条端点上）
        let (s0, v0) = send(&r, "GET", "/api/users/health", Some(&t), None).await;
        assert_eq!(s0, axum::http::StatusCode::OK);
        assert_eq!(v0, serde_json::json!({}));
        h.runtime
            .update(|rt| {
                rt.extra.insert(
                    "users".into(),
                    serde_json::json!({"total": 2, "blocked": 1}),
                );
            })
            .await;
        let (s, v) = send(&r, "GET", "/api/users/health", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"total": 2, "blocked": 1}));
    }

    #[tokio::test]
    async fn every_admin_endpoint_is_401_without_a_token() {
        let h = harness().await;
        // 必须用 full_with 挂**本文件的** routes()：`h.router()` 挂的是 PanelModule，
        // 而它的 routes() 要到 T13 才接线，这里会一律拿到 404 而不是 401。
        let router = full_with(&h.app, routes(h.shared.clone()), axum::Router::new());
        for (m, p) in [
            ("GET", "/api/users"),
            ("POST", "/api/users"),
            ("POST", "/api/users/alice/rotate"),
            ("GET", "/api/stats"),
            ("GET", "/api/online"),
            ("POST", "/api/kick"),
            ("GET", "/api/config"),
            ("POST", "/api/password"),
            ("GET", "/api/masquerade"),
            ("GET", "/api/bandwidth"),
            ("GET", "/api/port-hopping"),
            ("GET", "/api/hy2/watchdog/status"),
            ("GET", "/api/users/health"),
        ] {
            let (s, _) = send(&router, m, p, None, None).await;
            assert_eq!(
                s,
                axum::http::StatusCode::UNAUTHORIZED,
                "{m} {p} 居然不需要鉴权"
            );
        }
    }

    #[tokio::test]
    async fn the_deleted_v3_endpoints_are_gone() {
        let h = harness().await;
        // 同上：挂本文件的 routes()，确认这些路径**在管理员这棵子树里**确实不存在
        let router = full_with(&h.app, routes(h.shared.clone()), axum::Router::new());
        let t = token(&h).await;
        for p in [
            "/api/manage?key=x&action=list",
            "/api/version",
            "/api/kernel-versions",
            "/api/kernel-downloads",
            "/packages",
            "/install-client?key=x",
        ] {
            let (s, _) = send(&router, "GET", p, Some(&t), None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "{p} 应该已被删除");
        }
        // 带 token 发：P1 的 `require_admin` 套在整棵受保护子树（含它的 fallback）上，
        // 无 token 的未命中路径先拿 401 再谈 404，那证不出「路由不存在」。
        let (s, _) = send(
            &router,
            "POST",
            "/auth/hysteria",
            Some(&t),
            Some(serde_json::json!({"auth": "a:b"})),
        )
        .await;
        assert_eq!(
            s,
            axum::http::StatusCode::NOT_FOUND,
            "/auth/hysteria 已由 auth-hook 取代"
        );
    }
}
