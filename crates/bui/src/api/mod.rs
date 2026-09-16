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

/// 基础 Router：公开 `/api/login` 与各模块的 `public_routes()`，其余全部走 `require_admin`；
/// 再 merge 各模块的 `routes()`。
///
/// 模块路由挂在 `protected` 上，所以 P2/P3 的端点天然带鉴权，不必自己加中间件。
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router {
    let protected = axum::Router::new()
        .route("/api/health", axum::routing::get(health::get))
        .route("/api/reconcile", axum::routing::post(system::reconcile))
        .route(
            "/api/services/{unit}/{action}",
            axum::routing::post(system::service_action),
        )
        .route(
            "/api/system/hy2-auth",
            axum::routing::post(system::set_hy2_auth),
        )
        .route(
            "/api/system/legacy-sub",
            axum::routing::post(system::set_legacy_sub),
        )
        .route("/api/system/obfs", axum::routing::post(system::set_obfs))
        .route(
            "/api/system/hy2-resi-compat",
            axum::routing::post(system::set_hy2_resi_compat),
        );
    let protected = modules
        .iter()
        .fold(protected, |acc, m| acc.merge(m.routes()));
    // 裁决 D1：公开路由必须落在 require_admin 外面
    let public = modules
        .iter()
        .fold(axum::Router::new(), |acc, m| acc.merge(m.public_routes()));
    axum::Router::new()
        .route("/api/login", axum::routing::post(auth::login))
        .merge(public)
        .merge(protected.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        )))
        // `DefaultMakeSpan` 把整条 URI 记进 span ⇒ `--log debug` 一开，四个免鉴权订阅端点的
        // 路径末段（订阅 token，或宽限期内的用户名）就进了 journald。那一段本身就是凭据
        // （2026-09-14 裁决），所以自己造 span，URI 先过 `redact::sub_path`。
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                |req: &axum::http::Request<axum::body::Body>| {
                    tracing::debug_span!(
                        "request",
                        method = %req.method(),
                        uri = %crate::redact::sub_path(&req.uri().to_string()),
                        version = ?req.version(),
                    )
                },
            ),
        )
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
        let (app, d, host, runtime, _bus) = app_with_bus().await;
        (app, d, host, runtime)
    }

    /// 同 [`app_with_runtime`]，另外交出事件总线（断言 `StateChanged` 发没发）。
    async fn app_with_bus() -> (
        axum::Router,
        tempfile::TempDir,
        Arc<FakeHost>,
        Runtime,
        EventBus,
    ) {
        let d = tempfile::tempdir().unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = crate::api::auth::hash_password("test123").unwrap();
        let units = crate::reconcile::managed_units(&state);
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in &units {
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
        let bus = EventBus::new();
        let app_state = AppState {
            store,
            bus: bus.clone(),
            runtime: runtime.clone(),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: crate::api::auth::LoginLimiter::default(),
        };
        (router(app_state, &[]), d, host, runtime, bus)
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
        let resi = h
            .residential
            .clone()
            .expect("P3 起 /api/health 一定带住宅摘要");
        assert_eq!(resi["enabled"], false, "夹具是空池");
        assert!(resi.get("alerts").is_some());
        assert!(resi.get("blacklist").is_some());
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

    /// `POST /api/system/hy2-auth`（`bui set hy2-auth` 的落点）：只写期望态 + 发一次
    /// `StateChanged`，重渲染与重启都交给那一轮对账；非法值 400 且什么都不写。
    #[tokio::test]
    async fn the_hy2_auth_endpoint_writes_the_state_and_asks_for_a_reconcile() {
        let (app, d, _h, _rt) = app_with_runtime().await;
        let token = login(&app).await;
        let mode = || -> bui_schema::model::Hy2Auth {
            let bytes = std::fs::read(d.path().join("state.json")).unwrap();
            serde_json::from_slice::<bui_schema::model::State>(&bytes)
                .unwrap()
                .system
                .hy2_auth
        };
        assert_eq!(mode(), bui_schema::model::Hy2Auth::Http, "默认就是 http");

        let res = app
            .clone()
            .oneshot(with_token(
                post(
                    "/api/system/hy2-auth",
                    serde_json::json!({"mode": "command"}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json(res).await["hy2_auth"], "command");
        assert_eq!(mode(), bui_schema::model::Hy2Auth::Command);

        let res = app
            .oneshot(with_token(
                post(
                    "/api/system/hy2-auth",
                    serde_json::json!({"mode": "userpass"}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(mode(), bui_schema::model::Hy2Auth::Command, "非法值不落盘");
    }

    /// `POST /api/system/legacy-sub`（`bui set legacy-sub` 的落点）：`until: null` 停用、
    /// RFC3339 改期、垃圾值 400 且什么都不写。
    #[tokio::test]
    async fn the_legacy_sub_endpoint_writes_the_deadline_or_clears_it() {
        let (app, d, _h, _rt) = app_with_runtime().await;
        let token = login(&app).await;
        let until = || -> Option<String> {
            let bytes = std::fs::read(d.path().join("state.json")).unwrap();
            serde_json::from_slice::<bui_schema::model::State>(&bytes)
                .unwrap()
                .system
                .legacy_sub_until
        };
        assert_eq!(until(), None, "sample_state 没有宽限期");

        let res = app
            .clone()
            .oneshot(with_token(
                post(
                    "/api/system/legacy-sub",
                    serde_json::json!({"until": "2026-09-21T00:00:00Z"}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json(res).await["legacy_sub_until"], "2026-09-21T00:00:00Z");
        assert_eq!(until().as_deref(), Some("2026-09-21T00:00:00Z"));

        let res = app
            .clone()
            .oneshot(with_token(
                post(
                    "/api/system/legacy-sub",
                    serde_json::json!({"until": "下周"}),
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            until().as_deref(),
            Some("2026-09-21T00:00:00Z"),
            "非法值不落盘"
        );

        let res = app
            .oneshot(with_token(
                post("/api/system/legacy-sub", serde_json::json!({"until": null})),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(until(), None, "null ⇒ 立刻停用全部用户名链接");
    }

    /// `POST /api/system/obfs`（`bui set obfs` 的落点，2026-09-15 裁决）：未登录 401；非法 JSON
    /// 与非法取值 400 且什么都不写；on 首次生成 32 位十六进制密码并发 `StateChanged`；重复 on
    /// 密码不变、不写盘、不发事件；off 保留密码。回包不带 obfs 密码。
    #[tokio::test]
    async fn the_obfs_endpoint_switches_the_state_and_asks_for_a_reconcile() {
        let (app, d, _h, _rt, bus) = app_with_bus().await;
        let token = login(&app).await;
        let mut rx = bus.subscribe();
        let read = || -> (bui_schema::model::Obfs, Vec<u8>) {
            let bytes = std::fs::read(d.path().join("state.json")).unwrap();
            let s: bui_schema::model::State = serde_json::from_slice(&bytes).unwrap();
            (s.node.obfs, bytes)
        };
        let (initial, _) = read();
        assert!(!initial.enabled, "sample_state 默认关闭");
        let send = |req: Request<Body>| app.clone().oneshot(req);

        let res = send(post("/api/system/obfs", serde_json::json!({"value": "on"})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "未登录");
        let res = send(with_token(
            Request::builder()
                .method("POST")
                .uri("/api/system/obfs")
                .header("content-type", "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "非法 JSON");
        let res = send(with_token(
            post("/api/system/obfs", serde_json::json!({"value": "maybe"})),
            &token,
        ))
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "非法取值");
        assert_eq!(read().0, initial, "未登录 / 非法请求不落盘");
        assert!(rx.try_recv().is_err(), "也不发事件");

        let on_req = || {
            with_token(
                post("/api/system/obfs", serde_json::json!({"value": "on"})),
                &token,
            )
        };
        let res = send(on_req()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json(res).await;
        assert_eq!(body["enabled"], true);
        assert_eq!(body["changed"], true);
        let (on, bytes) = read();
        assert!(on.enabled);
        assert!(
            on.password.len() == 32
                && on
                    .password
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "密码形状不对"
        );
        assert!(
            !body.to_string().contains(&on.password),
            "回包不带 obfs 密码"
        );
        assert_eq!(rx.try_recv().unwrap(), Event::StateChanged("obfs"));

        let res = send(on_req()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json(res).await["changed"], false);
        assert_eq!(read(), (on.clone(), bytes), "重复 on 密码不变、不写盘");
        assert!(rx.try_recv().is_err(), "没变化不发 StateChanged");

        let res = send(with_token(
            post("/api/system/obfs", serde_json::json!({"value": "off"})),
            &token,
        ))
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json(res).await;
        assert_eq!(
            (body["enabled"].clone(), body["changed"].clone()),
            (false.into(), true.into())
        );
        assert_eq!(
            read().0,
            bui_schema::model::Obfs {
                enabled: false,
                password: on.password.clone()
            },
            "off 保留密码"
        );
        assert_eq!(rx.try_recv().unwrap(), Event::StateChanged("obfs"));
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

    /// 面板 / CLI 手动重启住宅入站之后必须广播 `Event::Hy2ResiRestarted`（第七波复核）：
    /// 不开 `cache_file`（spec §14 裁决 1）⇒ 重启把每个 `gate-<id>` selector 打回
    /// `default = deny`，没人重放就是全体住宅 HY2 用户被拒到下一轮 60 秒安全网。
    /// 别的单元、以及 `stop`（门跟着内核一起没了）都不广播 —— 白重放一轮是多余的 HTTP。
    #[tokio::test]
    async fn restarting_the_residential_inbound_announces_it_for_the_gate_replay() {
        let (app, _d, _h, _rt, bus) = app_with_bus().await;
        let token = login(&app).await;
        let mut rx = bus.subscribe();
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
            call("hysteria-residential", "restart").await.status(),
            StatusCode::OK
        );
        assert_eq!(rx.try_recv().ok(), Some(Event::Hy2ResiRestarted));
        assert_eq!(
            call("hysteria-residential", "start").await.status(),
            StatusCode::OK
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Event::Hy2ResiRestarted),
            "`start` 之后门同样是 default = deny"
        );
        assert_eq!(
            call("hysteria-residential", "stop").await.status(),
            StatusCode::OK
        );
        assert!(rx.try_recv().is_err(), "stop 不广播：门跟着内核一起没了");
        assert_eq!(
            call("hysteria-server", "restart").await.status(),
            StatusCode::OK
        );
        assert!(rx.try_recv().is_err(), "直连实例与住宅门位无关");
    }

    /// 一个只为「公开路由不过鉴权、受保护路由仍要 Bearer」而存在的模块（裁决 D1 的回归锁）
    struct ProbeModule;
    impl Module for ProbeModule {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn render(
            &self,
            _s: &bui_schema::model::State,
            _c: &crate::reconcile::RenderCtx,
        ) -> Vec<crate::reconcile::Artifact> {
            Vec::new()
        }
        fn routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/admin", axum::routing::get(|| async { "admin" }))
        }
        fn public_routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/open", axum::routing::get(|| async { "open" }))
        }
    }

    /// 与 `app_with_runtime()` 同一套 AppState，只是把模块挂进 `router()`
    async fn app_with_modules(modules: Vec<Arc<dyn Module>>) -> (axum::Router, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = crate::api::auth::hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let host = Arc::new(FakeHost::new());
        let app_state = AppState {
            store,
            bus: EventBus::new(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: crate::api::auth::LoginLimiter::default(),
        };
        (router(app_state, &modules), d)
    }

    async fn get_status(app: &axum::Router, uri: &str, token: Option<&str>) -> StatusCode {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let req = match token {
            Some(t) => with_token(req, t),
            None => req,
        };
        app.clone().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn public_module_routes_skip_require_admin() {
        let (app, _d) = app_with_modules(vec![Arc::new(ProbeModule)]).await;
        assert_eq!(
            get_status(&app, "/probe/open", None).await,
            StatusCode::OK,
            "public_routes() 必须落在 require_admin 外面（裁决 D1）：订阅 / 嵌入前端 / /packages/* 靠它"
        );
    }

    #[tokio::test]
    async fn protected_module_routes_still_require_a_bearer_token() {
        let (app, _d) = app_with_modules(vec![Arc::new(ProbeModule)]).await;
        assert_eq!(
            get_status(&app, "/probe/admin", None).await,
            StatusCode::UNAUTHORIZED,
            "模块的 routes() 仍然整棵套在 require_admin 里"
        );
        let token = login(&app).await;
        assert_eq!(
            get_status(&app, "/probe/admin", Some(&token)).await,
            StatusCode::OK
        );
        // 公开通道不许顺手把 P1 自己的受保护端点漏出去
        assert_eq!(
            get_status(&app, "/api/health", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// 收 `TraceLayer` 造出来的 span 字段（`--log debug` 会把它们打进 journald）。
    #[derive(Clone, Default)]
    struct SpanFields(Arc<std::sync::Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanFields {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Visitor<'a>(&'a mut Vec<String>);
            impl tracing::field::Visit for Visitor<'_> {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.push(format!("{}={v:?}", f.name()));
                }
            }
            attrs.record(&mut Visitor(&mut self.0.lock().unwrap()));
        }
    }

    /// 2026-09-14 裁决：四个免鉴权订阅端点的路径末段就是凭据，`--log debug` 也不许把它打出来。
    /// 这里真的挂一个 DEBUG 订阅者跑一次请求，核对 span 里落下的是 `/api/sub/***`。
    #[tokio::test]
    async fn the_request_span_masks_the_subscription_token_even_at_debug() {
        use tracing_subscriber::layer::SubscriberExt;
        let cap = SpanFields::default();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::registry()
                .with(tracing_subscriber::filter::LevelFilter::DEBUG)
                .with(cap.clone()),
        );
        let (app, _d, _h) = app().await;
        // `router()` 那条 span 的 callsite 是整个测试进程共用的：谁第一次打到它，兴趣就按
        // **那个线程**当时的订阅者缓存下来。别的用例（在没有订阅者的线程上）先打到就会缓存成
        // `Interest::never()`，而 `set_default` 不重算缓存 ⇒ 不处理的话这个用例单跑绿、
        // 全量并行跑红。所以先空跑一次把 callsite 注册掉，再显式重算一次兴趣。
        let warmup = Request::builder()
            .uri("/api/login")
            .body(Body::empty())
            .unwrap();
        let _ = app.clone().oneshot(warmup).await.unwrap();
        tracing::callsite::rebuild_interest_cache();
        cap.0.lock().unwrap().clear();

        let token = "0123456789abcdef0123456789abcdef";
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sub/{token}?x=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let fields = cap.0.lock().unwrap().join(" | ");
        assert!(!fields.contains(token), "token 进了 span：{fields}");
        assert!(
            fields.contains("/api/sub/***?x=1"),
            "URI 该留着路径与查询串、只换末段：{fields}"
        );
    }
}
