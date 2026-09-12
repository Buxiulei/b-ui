//! CLI 通过 unix socket 调的两个系统端点（spec §2.4）：触发一轮对账、动一个受管单元。
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

/// `POST /api/services/{unit}/{action}`；`unit` 只接受 `MANAGED_UNITS` 里的六个名字，其余 400。
pub async fn service_action(
    State(app): State<AppState>,
    Path((unit, action)): Path<(String, String)>,
) -> Response {
    if !crate::reconcile::MANAGED_UNITS.contains(&unit.as_str()) {
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
    let out = tokio::task::spawn_blocking(move || host.systemd(&action, &unit)).await;
    match out {
        Ok(Ok(o)) => (
            StatusCode::OK,
            Json(serde_json::json!({"ok": o.ok(), "detail": o.stderr})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "systemctl 调用失败"})),
        )
            .into_response(),
    }
}
