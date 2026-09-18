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
        app_with_state(crate::testutil::sample_state()).await
    }

    /// 同 [`app_with_bus`]，但期望态由调用方给（住宅池要有上游 / 槽位的用例用它）
    /// 同 [`app_with_bus`]，但期望态由调用方给，并连 [`Store`] 一起交出来
    /// （要自己拼一个 `DaemonCtx` 的用例用它：`Store` 是 `DaemonCtx` 的第一个字段）
    #[allow(clippy::type_complexity)]
    async fn app_with_store(
        mut state: bui_schema::model::State,
    ) -> (
        axum::Router,
        tempfile::TempDir,
        Arc<FakeHost>,
        Runtime,
        EventBus,
        Store,
    ) {
        let d = tempfile::tempdir().unwrap();
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
        (
            router(app_state.clone(), &[]),
            d,
            host,
            runtime,
            bus,
            app_state.store,
        )
    }

    /// 同 [`app_with_store`]，丢掉 `Store`（绝大多数用例只要路由）
    async fn app_with_state(
        state: bui_schema::model::State,
    ) -> (
        axum::Router,
        tempfile::TempDir,
        Arc<FakeHost>,
        Runtime,
        EventBus,
    ) {
        let (app, d, host, rt, bus, _store) = app_with_store(state).await;
        (app, d, host, rt, bus)
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

    /// `POST /api/system/hy2-resi-compat`（T14 第六波复核收口）：**30 天门禁在端点这一侧
    /// 同样成立**，判据与 CLI 是同一个函数（`commands::config::check_compat_takedown`）。
    ///
    /// 为什么端点也必须拦：`runtime.json` 的累计命中守护进程自己就持有（`AppState.runtime`），
    /// 「只有本地读得到」不成立；不拦的话面板、curl、任何走 socket / HTTP 的运维脚本都能
    /// 零检查关掉兼容段 —— 而那正是 2026-09-17 裁决要防的那件事（把仍在用的兼容段关掉，
    /// 全部还没刷订阅的 4.0 住宅用户当场断联）。
    #[tokio::test]
    async fn the_compat_endpoint_enforces_the_same_thirty_day_gate_as_the_cli() {
        let (app, d, _h, rt, bus) = app_with_bus().await;
        let token = login(&app).await;
        let mut rx = bus.subscribe();
        let read = || -> bool {
            let bytes = std::fs::read(d.path().join("state.json")).unwrap();
            serde_json::from_slice::<bui_schema::model::State>(&bytes)
                .unwrap()
                .system
                .hy2_resi_compat_ports
        };
        let switch = |value: &str, force: bool| {
            let token = token.clone();
            let app = app.clone();
            let body = serde_json::json!({"value": value, "force": force});
            async move {
                let res = app
                    .oneshot(with_token(
                        post("/api/system/hy2-resi-compat", body),
                        &token,
                    ))
                    .await
                    .unwrap();
                (res.status(), json(res).await)
            }
        };
        assert!(read(), "sample_state 默认开着兼容段");

        // ① 命中统计未就绪 ⇒ fail-closed，一个字节都不写、也不发事件
        let (code, body) = switch("off", false).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let why = body["error"].as_str().unwrap().to_string();
        assert!(why.contains("命中统计未就绪"), "{why}");
        assert!(read(), "被拒的那次不许改期望态");
        assert!(rx.try_recv().is_err(), "也不发事件");

        // ② 命中过 ⇒ 拒，且把人判断需要的三个值 + 区间都回给调用方
        //    （区间由 `nft::compat_range(&ports)` 算，不是写死的字面量）
        rt.update(|r| {
            r.extra.insert(
                crate::modules::watchdog::COMPAT_HITS_KEY.into(),
                serde_json::json!({
                    "total": 12, "seen": 12,
                    "last_hit_at": "2026-09-10T10:00:00Z",
                    "since": "2026-08-01T00:00:00Z",
                }),
            );
        })
        .await;
        let (code, body) = switch("off", false).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let why = body["error"].as_str().unwrap().to_string();
        for must in [
            "累计命中 12 次",
            "最近一次 2026-09-10T10:00:00Z",
            "起算时刻 2026-08-01T00:00:00Z",
            "40001-40007",
            "当场断联",
            "--force",
        ] {
            assert!(why.contains(must), "回包缺「{must}」：{why}");
        }
        assert!(read(), "被拒的那次不许改期望态");
        assert!(rx.try_recv().is_err());

        // ③ 开兼容段从不过门禁（危险方向只有关）：已经是 on ⇒ 200 + changed=false
        let (code, body) = switch("on", false).await;
        assert_eq!(
            (code, body["changed"].clone()),
            (StatusCode::OK, false.into())
        );

        // ④ 显式 force ⇒ 放行并落盘
        let (code, body) = switch("off", true).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            (body["enabled"].clone(), body["changed"].clone()),
            (false.into(), true.into())
        );
        assert!(!read());
        assert_eq!(
            rx.try_recv().unwrap(),
            Event::StateChanged("hy2-resi-compat")
        );

        // ⑤ 一次都没命中 + 静默满 30 天 ⇒ 不带 force 也放行（开回来同样不用 force）
        let (code, _) = switch("on", false).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            rx.try_recv().unwrap(),
            Event::StateChanged("hy2-resi-compat")
        );
        rt.update(|r| {
            r.extra.insert(
                crate::modules::watchdog::COMPAT_HITS_KEY.into(),
                serde_json::json!({"total": 0, "seen": 0, "since": "2026-08-01T00:00:00Z"}),
            );
        })
        .await;
        let (code, body) = switch("off", false).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(!read());
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

    /// 池有效 + 两条上游 + 两个槽（槽 i 的 IP = 第 i 条）：全池 = 全局池 + slot-0/1-pool，
    /// 而且有可借的第二条，槽能 pin 到别人身上
    fn relay_pool_state() -> bui_schema::model::State {
        let mut s = crate::modules::residential::sample_state_with_pool();
        let g = s.residential.groups.get_mut("default").unwrap();
        let proto = g.upstreams[0].clone();
        g.upstreams = (0..2u16)
            .map(|i| bui_schema::model::Upstream {
                id: uuid::Uuid::from_u128(i as u128 + 1),
                name: format!("url-{}", i + 1),
                host: format!("isp{}.example.net", i + 1),
                ..proto.clone()
            })
            .collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        s.residential.slots = (0..2u16)
            .map(|index| bui_schema::model::Slot {
                index,
                upstream_id: uuid::Uuid::from_u128(index as u128 + 1),
            })
            .collect();
        s
    }

    /// 2026-09-18 第三次裁决 ③ + **第四次裁决 ①**：`POST /api/services/b-ui-relay/restart|start`
    /// = 一次**全池**切换。不开 `cache_file` ⇒ 每个池 selector 的 `now` 都回落到配置里的
    /// default，所以归因的时间戳门必须在这里当场记上 —— 否则这一刻之前产生的 relay 错误行
    /// 会被路径 B 整批记到重启后 default 的那条上游头上。
    ///
    /// 记的时刻必须取自 `systemctl` **返回之后**（裁决 ①）：`systemctl` 是阻塞的，
    /// selector 回落 default 就发生在「发命令 → 返回」那段里，记发命令之前等于把门开在
    /// 事件之前。用 `FakeHost::advance_on_systemd` 在 systemd stub 里拨表复现这段耗时。
    ///
    /// `stop` 不记：relay 停着时一个 selector 都没有，路径 B 本来就归不了因。
    #[tokio::test]
    async fn restarting_the_relay_stamps_every_pool_after_the_systemctl_returns() {
        use crate::modules::residential::{slot_selector, state as rstate, POOL};
        let (app, _d, host, rt, _bus) = app_with_state(relay_pool_state()).await;
        // 「发 systemctl → 返回」之间走掉 30 秒（真机上是内核起不起来那一段）
        host.with(|i| i.advance_on_systemd = 30);
        let token = login(&app).await;
        let call = |action: &str| {
            let (token, app) = (token.clone(), app.clone());
            let uri = format!("/api/services/b-ui-relay/{action}");
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
        assert_eq!(call("stop").await.status(), StatusCode::OK);
        assert!(
            rstate::read(&rt).await.pool_switch_at.is_empty(),
            "stop 不记：relay 停着时一个 selector 都没有"
        );
        let before = host.now(); // 发 restart 之前
        assert_eq!(call("restart").await.status(), StatusCode::OK);
        let returned = host.now(); // systemctl 返回之后
        assert_eq!(
            returned,
            before + time::Duration::seconds(30),
            "stub 拨过表：这一段就是 systemctl 的阻塞时间"
        );
        let at = rstate::read(&rt).await;
        assert_eq!(
            at.pool_switch_at.keys().cloned().collect::<Vec<_>>(),
            vec![POOL.to_string(), slot_selector(0), slot_selector(1)],
            "relay 重启 = 全池切换：每个池 selector 都要记一笔"
        );
        for (pool, raw) in &at.pool_switch_at {
            let t = crate::util::parse_rfc3339(raw).expect("记下的时刻要能解析");
            assert!(
                t >= returned,
                "{pool} 记的是 {t}，比 systemctl 返回时刻 {returned} 早 ⇒ 门开在事件之前"
            );
        }
    }

    /// **第四次裁决 ②**：`POST /api/services/b-ui-relay/restart` 也要发
    /// `Event::RelayRestarted`（与同端点重启 `hysteria-residential` 那条口径一致）。
    /// 少了它，`health::replay_loop` 不跑，借用 / pin 中的槽就停在配置里的 default ——
    /// 靠 `drive_slots` 下一轮兜底意味着最坏 2 分钟里借槽的用户都被打回坏 IP。
    /// `stop` 仍不发：relay 停着时没有 selector 可重放。
    #[tokio::test]
    async fn restarting_the_relay_is_announced_so_the_selection_gets_replayed() {
        use crate::modules::residential::clash::{Clash, FakeClash};
        use crate::modules::residential::health::replay_loop;
        use crate::modules::residential::{slot_selector, state as rstate};

        let (app, _d, host, rt, bus, store) = app_with_store(relay_pool_state()).await;
        let token = login(&app).await;
        // 槽 0 被管理员 pin 在第二条上游上（≠ 本槽自己的 IP）⇒ 重启后必须被重放回去
        rstate::update(&rt, |r| {
            r.slots.entry("0".into()).or_default().pinned_upstream_id =
                Some(uuid::Uuid::from_u128(2));
        })
        .await;
        let clash = Arc::new(FakeClash::new(None));
        // relay 重启后 selector 停在配置里的 default（本槽自己的 IP）
        clash.with(|i| {
            i.now.insert(slot_selector(0), "resi-1".into());
        });
        let ctx = crate::reconcile::DaemonCtx {
            store,
            runtime: rt.clone(),
            bus: bus.clone(),
            host: host.clone(),
            paths: bui_schema::paths::Paths::default_server(),
        };
        let rx = bus.subscribe();
        let mut watch = bus.subscribe();
        let replay = tokio::spawn(replay_loop(ctx, clash.clone() as Arc<dyn Clash>, rx));
        let call = |action: &str| {
            let (token, app) = (token.clone(), app.clone());
            let uri = format!("/api/services/b-ui-relay/{action}");
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
        assert_eq!(call("stop").await.status(), StatusCode::OK);
        assert!(watch.try_recv().is_err(), "stop 不发：没有 selector 可重放");
        assert_eq!(call("restart").await.status(), StatusCode::OK);
        assert_eq!(
            watch.try_recv().ok(),
            Some(Event::RelayRestarted),
            "重启了 relay 却不广播 ⇒ 借用 / pin 的槽全停在 default、没人重放"
        );
        let sel = slot_selector(0);
        for _ in 0..200 {
            if clash.peek(&sel).as_deref() == Some("resi-2") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        replay.abort();
        assert_eq!(
            clash.peek(&sel).as_deref(),
            Some("resi-2"),
            "重放跑过：pin 的槽被重新 PUT 回去，不再停在 default"
        );
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
