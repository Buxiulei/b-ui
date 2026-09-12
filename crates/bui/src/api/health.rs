//! `GET /api/health`：六个受管单元的状态 + 上一轮对账报告 + 漂移 + watchdog + 可升级版本。
//!
//! `degraded` 的判据只有三项：有受管单元不 active、有漂移、上一轮对账有 errors 或 verify_failures。
//! `notes` 不参与判定——它是「只报不改」的提示（没装防火墙、SSH 公钥数为 0 之类），
//! 常年存在却不代表机器坏了（C1 的落地口径）。

use crate::api::AppState;
use crate::state::runtime::{DriftItem, ReconcileReport, WatchdogRecord};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceStatus {
    pub unit: String,
    pub active: bool,
    pub enabled: bool,
    pub n_restarts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthResponse {
    /// "ok" | "degraded"
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
    pub node: String,
    pub services: Vec<ServiceStatus>,
    pub reconcile: Option<ReconcileReport>,
    pub drift: Vec<DriftItem>,
    pub watchdog: BTreeMap<String, WatchdogRecord>,
    /// 每日自检发现的新版本（spec §7）
    pub upgrade_available: Option<String>,
    /// P3 填
    pub residential: Option<serde_json::Value>,
}

pub async fn get(State(app): State<AppState>) -> Json<HealthResponse> {
    let state = app.store.read().await;
    let rt = app.runtime.read().await;
    let host = app.host.clone();
    // 三次 systemctl × 六个单元都是阻塞调用，必须离开 async 线程（总纲裁决：Host 同步 + spawn_blocking）
    let services = tokio::task::spawn_blocking(move || {
        crate::reconcile::MANAGED_UNITS
            .iter()
            .map(|u| ServiceStatus {
                unit: u.to_string(),
                active: host.unit_is_active(u).unwrap_or(false),
                enabled: host.unit_is_enabled(u).unwrap_or(false),
                n_restarts: host
                    .unit_property(u, "NRestarts")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let degraded = services.iter().any(|s| !s.active)
        || !rt.drift.is_empty()
        || rt
            .last_reconcile
            .as_ref()
            .is_some_and(|r| !r.errors.is_empty() || !r.verify_failures.is_empty());
    Json(HealthResponse {
        status: if degraded {
            "degraded".into()
        } else {
            "ok".into()
        },
        version: app.version.to_string(),
        uptime_secs: (app.host.now() - app.started_at).whole_seconds().max(0) as u64,
        node: state.node.name.clone(),
        services,
        reconcile: rt.last_reconcile.clone(),
        drift: rt.drift.clone(),
        watchdog: rt.watchdog.clone(),
        upgrade_available: rt.upgrade_available.clone(),
        residential: None,
    })
}
