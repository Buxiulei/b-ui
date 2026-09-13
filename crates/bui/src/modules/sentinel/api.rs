//! `GET /api/incidents?limit=N`：最近的哨兵事件，新的在前（spec §5.7）。管理员鉴权由
//! `api::router` 统一挂（模块路由都在 `require_admin` 里面），本模块不加中间件。

use super::incidents::{self, Incident};
use super::INCIDENTS_MAX;
use crate::api::AppState;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::Json;
use serde::{Deserialize, Serialize};

/// 不带 `limit` 时给多少条
pub const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize)]
pub struct IncidentsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct IncidentsResponse {
    pub incidents: Vec<Incident>,
    /// 环里一共有几条（≤ INCIDENTS_MAX）
    pub total: usize,
}

pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/api/incidents", get(get_incidents))
}

pub async fn get_incidents(
    State(app): State<AppState>,
    Query(q): Query<IncidentsQuery>,
) -> Json<IncidentsResponse> {
    let all = incidents::from_runtime(&app.runtime.read().await);
    let n = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, INCIDENTS_MAX);
    Json(IncidentsResponse {
        total: all.len(),
        incidents: all.into_iter().take(n).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::sentinel::incidents::{push, Level};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn inc(n: usize) -> Incident {
        Incident {
            at: format!("2026-09-11T00:00:{:02}Z", n),
            unit: "xray".into(),
            signature: "kernel_bind_in_use".into(),
            subject: "xray".into(),
            action: "delegate_watchdog".into(),
            result: format!("第 {n} 条"),
            level: Level::Warn,
            sample: None,
        }
    }

    async fn app() -> (AppState, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state())
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        runtime
            .update(|r| {
                for n in 0..5 {
                    push(r, inc(n));
                }
            })
            .await;
        let host = Arc::new(FakeHost::new());
        let started_at = host.now();
        (
            AppState {
                store,
                bus: EventBus::new(),
                runtime,
                host,
                started_at,
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            d,
        )
    }

    #[tokio::test]
    async fn incidents_come_newest_first_and_the_limit_is_clamped() {
        let (a, _d) = app().await;
        let Json(r) =
            get_incidents(State(a.clone()), Query(IncidentsQuery { limit: Some(2) })).await;
        assert_eq!(r.total, 5);
        assert_eq!(
            r.incidents
                .iter()
                .map(|i| i.result.as_str())
                .collect::<Vec<_>>(),
            vec!["第 4 条", "第 3 条"]
        );
        let Json(r) =
            get_incidents(State(a.clone()), Query(IncidentsQuery { limit: Some(0) })).await;
        assert_eq!(r.incidents.len(), 1, "下限 1");
        let Json(r) = get_incidents(State(a), Query(IncidentsQuery { limit: None })).await;
        assert_eq!(r.incidents.len(), 5, "缺省 50，不足全给");
    }

    #[tokio::test]
    async fn the_endpoint_sits_behind_admin_auth() {
        let (a, _d) = app().await;
        let panel = Arc::new(crate::modules::panel::Shared::new(
            Box::new(crate::modules::panel::fakes::FakeXray::new()),
            Box::new(crate::modules::panel::fakes::FakeHy2::new()),
        ));
        let m: Arc<dyn crate::reconcile::Module> =
            Arc::new(crate::modules::sentinel::SentinelModule::new(panel));
        let router = crate::api::router(a.clone(), &[m]);
        let res = router
            .oneshot(Request::get("/api/incidents").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
