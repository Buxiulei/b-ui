//! CLI 通过 unix socket 调的五个系统端点（spec §2.4）：触发一轮对账、动一个受管单元、
//! 切 Hysteria2 的鉴权方式、改旧订阅链接的宽限期、开关 HY2 混淆。
//!
//! 这两个端点是「菜单与 CLI 不自己动手」的唯一出口：`sudo b-ui` 的重启/停止都经过这里，
//! 于是三条对账路径（启动、去抖、10 分钟巡检）仍只有守护进程里那一个 consumer 在跑（S6）。

use crate::api::{AppState, Event};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ReconcileRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/reconcile` → 200 + 最近一份 `ReconcileReport`（已有报告）/ 202 `{"queued":true}`。
pub async fn reconcile(State(app): State<AppState>, Json(req): Json<ReconcileRequest>) -> Response {
    let _ = req.dry_run; // 真正的 dry_run 由 `bui reconcile --dry-run` 走本地路径（Task 15）
    app.bus.send(Event::ReconcileRequested { force: req.force });
    // 真正的对账由 serve.rs 的去抖触发器执行；这里只排队并回最近一份报告
    match app.runtime.read().await.last_reconcile {
        Some(r) => (StatusCode::OK, Json(r)).into_response(),
        None => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({"queued": true})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct Hy2AuthRequest {
    pub mode: String,
}

/// `POST /api/system/hy2-auth`：切 Hysteria2 的鉴权方式（`bui set hy2-auth` 的落点）。
///
/// 只写期望态 + 发一次 `StateChanged`：重渲染 `config.yaml`、重启 `hysteria-server`
/// 都由那一轮对账做（4.1 起只作用于直连），CLI 进程绝不自己碰 `state.json`
/// （否则会与守护进程的 `Store` 并发写）。
pub async fn set_hy2_auth(
    State(app): State<AppState>,
    Json(req): Json<Hy2AuthRequest>,
) -> Response {
    let mode = match req.mode.parse::<bui_schema::model::Hy2Auth>() {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };
    match app.store.update(|s| s.system.hy2_auth = mode).await {
        Ok(_) => {
            app.bus.send(Event::StateChanged("system"));
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "hy2_auth": mode.as_str()})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct LegacySubRequest {
    /// `null` = 停用全部「用户名订阅链接」；否则 RFC3339 截止时刻。
    #[serde(default)]
    pub until: Option<String>,
}

/// `POST /api/system/legacy-sub`：改旧「用户名订阅链接」的全局宽限期
/// （`bui set legacy-sub` 的落点，2026-09-14 裁决）。
///
/// 只写期望态、**不发 `StateChanged`**：四个免鉴权端点每次请求都现读 `state.json` 的内存
/// 副本，没有任何渲染产物依赖这一位，不必为它跑一轮对账。
pub async fn set_legacy_sub(
    State(app): State<AppState>,
    Json(req): Json<LegacySubRequest>,
) -> Response {
    // 校验与 CLI 同一处（`commands::config::parse_legacy_sub`）：`null` 直接就是停用
    let until = match req.until.as_deref() {
        None => None,
        Some(raw) => match crate::commands::config::parse_legacy_sub(raw) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        },
    };
    let stored = until.clone();
    match app
        .store
        .update(|s| s.system.legacy_sub_until = stored)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "legacy_sub_until": until})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ObfsRequest {
    /// `on` / `off`；合法性由 `commands::config::parse_obfs` 判。
    pub value: String,
}

/// `POST /api/system/obfs`：HY2 混淆（salamander）开关（`bui set obfs` 的落点，2026-09-15 裁决）。
///
/// 覆盖直连与全部住宅 HY2 实例。状态真变化时写期望态 + 发一次 `StateChanged`：重渲染与重启
/// 受影响的 HY2 实例都由那一轮对账做；已经是这个状态就什么都不写、不发事件（不让在线客户端
/// 白断一次）。判定与密码生成都在 `commands::config::obfs_switched`，且在 `Store` 的写锁里做。
/// 回包只有开关与是否变化，**不带 obfs 密码**。
pub async fn set_obfs(State(app): State<AppState>, Json(req): Json<ObfsRequest>) -> Response {
    let on = match crate::commands::config::parse_obfs(&req.value) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let mut changed = false;
    let res = app
        .store
        .update(|s| {
            if let Some(next) = crate::commands::config::obfs_switched(&s.node.obfs, on) {
                s.node.obfs = next;
                changed = true;
            }
        })
        .await;
    match res {
        Ok(_) => {
            if changed {
                app.bus.send(Event::StateChanged("obfs"));
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "enabled": on, "changed": changed})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/services/{unit}/{action}`；`unit` 只接受 `reconcile::is_managed_unit` 认的名字
/// （4.1 起就是那六个固定名字：带序号的住宅实例已退役进 `LEGACY_UNITS`），其余 400。
pub async fn service_action(
    State(app): State<AppState>,
    Path((unit, action)): Path<(String, String)>,
) -> Response {
    if !crate::reconcile::is_managed_unit(unit.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("不受管的单元：{unit}")})),
        )
            .into_response();
    }
    if !["restart", "stop", "start", "reload"].contains(&action.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("不支持的动作：{action}")})),
        )
            .into_response();
    }
    let host = app.host.clone();
    // 住宅入站被这个端点拉起来 / 重启之后，每个 `gate-<id>` selector 都回到
    // `default = deny`（不开 `cache_file`，spec §14 裁决 1）⇒ 必须广播，让
    // `gates::replay_loop` 立刻重放真实门位；少了它就是全体住宅 HY2 用户被拒到下一轮
    // 60 秒安全网，且没有任何告警说明原因。`stop` 不发（门跟着内核一起没了）。
    let announce = unit == "hysteria-residential" && matches!(action.as_str(), "restart" | "start");
    let out = tokio::task::spawn_blocking(move || host.systemd(&action, &unit)).await;
    match out {
        Ok(Ok(o)) => {
            if announce && o.ok() {
                app.bus.send(Event::Hy2ResiRestarted);
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": o.ok(), "detail": o.stderr})),
            )
                .into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "systemctl 调用失败"})),
        )
            .into_response(),
    }
}
