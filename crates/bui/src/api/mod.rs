//! HTTP API。Task 4 追加 `pub use state::{AppState, Event, EventBus};`，Task 13 追加 `router()`。
pub mod auth;
pub mod health;
pub mod state;
pub mod system;

// Task 17 收口：三个名字都已有调用方（`AppState` / `EventBus` 在 `Module::routes` 与 `DaemonCtx`，
// `Event` 在 `POST /api/reconcile` 与 `serve::debounce_loop`），Task 4 临时加的
// `#[allow(unused_imports)]` 随之删掉。
pub use state::{AppState, Event, EventBus};

use crate::reconcile::Module;
use std::sync::Arc;

/// 基础 Router：公开 `/api/login`，其余全部走 `require_admin`；再 merge 各模块的 `routes()`。
///
/// 模块路由挂在 `protected` 上，所以 P2/P3 的端点天然带鉴权，不必自己加中间件。
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router {
    let protected = axum::Router::new()
        .route("/api/health", axum::routing::get(health::get))
        .route("/api/reconcile", axum::routing::post(system::reconcile))
        .route(
            "/api/services/{unit}/{action}",
            axum::routing::post(system::service_action),
        );
    let protected = modules
        .iter()
        .fold(protected, |acc, m| acc.merge(m.routes()));
    axum::Router::new()
        .route("/api/login", axum::routing::post(auth::login))
        .merge(protected.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        )))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::health::HealthResponse;
    use crate::state::runtime::{ReconcileReport, Runtime};
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 一台「六个单元都在跑」的假机器 + 一个独立 AppState（限速器随之独立）
    async fn app_with_runtime() -> (axum::Router, tempfile::TempDir, Arc<FakeHost>, Runtime) {
        let d = tempfile::tempdir().unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = crate::api::auth::hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in crate::reconcile::MANAGED_UNITS {
                i.units_active.insert(format!("{u}.service"));
                i.units_enabled.insert(format!("{u}.service"));
            }
            // 键必须是单元**全名**：`unit_property` 的查询键会补 `.service`（Task 3 的归一化），
            // 播成裸名查不到 → `n_restarts` 静默变 0，下面的 `== 3` 必失败
            i.unit_props.insert(
                ("hysteria-server.service".into(), "NRestarts".into()),
                "3".into(),
            );
        });
        let app_state = AppState {
            store,
            bus: EventBus::new(),
            runtime: runtime.clone(),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: crate::api::auth::LoginLimiter::default(),
        };
        (router(app_state, &[]), d, host, runtime)
    }

    async fn app() -> (axum::Router, tempfile::TempDir, Arc<FakeHost>) {
        let (app, d, host, _rt) = app_with_runtime().await;
        (app, d, host)
    }

    async fn json(res: axum::response::Response) -> serde_json::Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    fn post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn with_token(mut req: Request<Body>, token: &str) -> Request<Body> {
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        req
    }

    /// 登录拿 token（成功登录会清掉本 router 的失败计数）
    async fn login(app: &axum::Router) -> String {
        let res = app
            .clone()
            .oneshot(post(
                "/api/login",
                serde_json::json!({"password": "test123"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        json(res).await["token"].as_str().unwrap().to_string()
    }

    async fn health(app: &axum::Router, token: &str) -> HealthResponse {
        let res = app
            .clone()
            .oneshot(with_token(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
                token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        serde_json::from_value(json(res).await).unwrap()
    }

    #[tokio::test]
    async fn login_rejects_a_wrong_password_like_v3() {
        let (app, _d, _h) = app().await;
        let res = app
            .oneshot(post("/api/login", serde_json::json!({"password": "nope"})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json(res).await, serde_json::json!({"error": "Auth failed"}));
    }

    #[tokio::test]
    async fn login_returns_a_token_on_success() {
        let (app, _d, _h) = app().await;
        let res = app
            .oneshot(post(
                "/api/login",
                serde_json::json!({"password": "test123"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json(res).await;
        assert_eq!(
            body["token"].as_str().unwrap().split('.').count(),
            3,
            "JWT 三段"
        );
        assert!(body["expires_at"].is_string());
    }

    #[tokio::test]
    async fn sixth_failed_login_within_a_minute_is_rate_limited() {
        let (app, _d, _h) = app().await;
        for _ in 0..5 {
            let res = app
                .clone()
                .oneshot(post("/api/login", serde_json::json!({"password": "nope"})))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
        let res = app
            .oneshot(post("/api/login", serde_json::json!({"password": "nope"})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            json(res).await["error"],
            "Too many attempts. Try again later."
        );
    }

    #[tokio::test]
    async fn health_requires_a_bearer_token() {
        let (app, _d, _h) = app().await;
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_reports_services_drift_watchdog_and_reconcile() {
        let (app, _d, _h) = app().await;
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "ok");
        assert_eq!(h.version, "4.0.0");
        assert_eq!(h.node, "node-a");
        assert_eq!(h.services.len(), 6, "六个受管单元都要报");
        let hy = h
            .services
            .iter()
            .find(|s| s.unit == "hysteria-server")
            .unwrap();
        assert!(hy.active && hy.enabled);
        assert_eq!(hy.n_restarts, 3);
        assert_eq!(h.drift, vec![]);
        assert!(h.watchdog.is_empty());
        assert_eq!(h.reconcile, None);
        assert_eq!(h.upgrade_available, None);
        assert_eq!(h.residential, None, "P3 才填");
    }

    #[tokio::test]
    async fn health_is_degraded_when_the_last_reconcile_had_errors() {
        let (app, _d, _h, runtime) = app_with_runtime().await;
        runtime
            .update(|r| {
                r.last_reconcile = Some(ReconcileReport {
                    at: "2026-09-11T00:00:00Z".into(),
                    errors: vec!["hysteria-server 重启失败".into()],
                    ..Default::default()
                });
                r.upgrade_available = Some("4.0.1".into());
            })
            .await;
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "degraded");
        assert_eq!(h.reconcile.unwrap().errors.len(), 1);
        assert_eq!(h.upgrade_available.as_deref(), Some("4.0.1"));
    }

    #[tokio::test]
    async fn health_is_degraded_when_a_managed_unit_is_down() {
        let (app, _d, host) = app().await;
        host.with(|i| {
            i.units_active.remove("xray.service");
        });
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "degraded");
        assert!(!h.services.iter().find(|s| s.unit == "xray").unwrap().active);
    }

    #[tokio::test]
    async fn reconcile_endpoint_queues_and_returns_the_last_report() {
        let (app, _d, _h, runtime) = app_with_runtime().await;
        let token = login(&app).await;
        // 还没有任何报告 → 202 排队
        let res = app
            .clone()
            .oneshot(with_token(
                post(
                    "/api/reconcile",
                    serde_json::json!({"force": false, "dry_run": false}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert_eq!(json(res).await["queued"], true);
        // 有报告后 → 200 + 报告本体
        runtime
            .update(|r| {
                r.last_reconcile = Some(ReconcileReport {
                    at: "2026-09-11T00:00:00Z".into(),
                    ..Default::default()
                })
            })
            .await;
        let res = app
            .oneshot(with_token(
                post(
                    "/api/reconcile",
                    serde_json::json!({"force": true, "dry_run": false}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json(res).await["at"], "2026-09-11T00:00:00Z");
    }

    #[tokio::test]
    async fn service_action_rejects_unmanaged_units() {
        let (app, _d, host) = app().await;
        let token = login(&app).await;
        let call = |unit: &str, action: &str| {
            let token = token.clone();
            let app = app.clone();
            let uri = format!("/api/services/{unit}/{action}");
            async move {
                app.oneshot(with_token(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                    &token,
                ))
                .await
                .unwrap()
            }
        };
        assert_eq!(
            call("hysteria-server", "restart").await.status(),
            StatusCode::OK
        );
        assert!(host
            .ops()
            .contains(&"systemd:restart:hysteria-server".to_string()));
        assert_eq!(call("sshd", "stop").await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            call("hysteria-server", "chown").await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}
