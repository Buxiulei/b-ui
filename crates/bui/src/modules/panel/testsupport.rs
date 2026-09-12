//! P2 各任务共用的测试支架（`#[cfg(test)]`）：tempdir 上的一台假机器 + 一个 `AppState`。

use super::fakes::{FakeHy2, FakeXray};
use super::{PanelModule, Shared};
use crate::api::{AppState, EventBus};
use crate::reconcile::Module;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::fake::FakeHost;
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::sync::Arc;

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub paths: Paths,
    pub host: Arc<FakeHost>,
    pub store: Store,
    pub runtime: Runtime,
    pub app: AppState,
    pub shared: Arc<Shared>,
    pub xray: FakeXray,
    pub hy2: FakeHy2,
}

impl Harness {
    /// 只挂 P2 自己的模块的整套 Router（含 `require_admin` 与公开路由）。
    ///
    /// **只给 T13 用**：T1…T12 期间 `PanelModule::routes()` / `public_routes()` 还是空的，
    /// 这条路会让任何端点都返回 404。那些任务请用 [`full_with`]。
    pub fn router(&self) -> axum::Router {
        full(
            &self.app,
            Arc::new(PanelModule::with_shared(self.shared.clone())),
        )
    }
}

pub async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let paths = Paths {
        base_dir: base.clone(),
        certs_dir: base.join("certs"),
        bin_dir: base.join("bin"),
    };
    let host = Arc::new(FakeHost::new());
    let store = Store::create(
        crate::paths::state_file(&paths),
        crate::testutil::sample_state(),
    )
    .await
    .unwrap();
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let (xray, hy2) = (FakeXray::new(), FakeHy2::new());
    let shared = Arc::new(Shared::new(Box::new(xray.clone()), Box::new(hy2.clone())));
    shared.set_paths(&paths);
    let app = AppState {
        store: store.clone(),
        bus: EventBus::new(),
        runtime: runtime.clone(),
        host: host.clone(),
        started_at: host.now(),
        version: env!("CARGO_PKG_VERSION"),
        login: crate::api::auth::LoginLimiter::default(),
    };
    Harness {
        dir,
        paths,
        host,
        store,
        runtime,
        app,
        shared,
        xray,
        hy2,
    }
}

/// 用 state 里的 `jwt_secret` 现签一个管理员 token
pub async fn token(h: &Harness) -> String {
    let secret = h.store.read().await.admin.jwt_secret.clone();
    crate::api::auth::issue_token(&secret, h.host.now())
        .unwrap()
        .0
}

pub fn mount(app: &AppState, r: axum::Router<AppState>) -> axum::Router {
    r.with_state(app.clone())
}

pub fn full(app: &AppState, m: Arc<dyn Module>) -> axum::Router {
    crate::api::router(app.clone(), &[m])
}

/// 把**指定的**两棵子路由挂成一套完整 Router（`public` 在 `require_admin` 外面、
/// `routes` 在里面）。
///
/// T1…T12 期间 `PanelModule::routes()` / `public_routes()` 还是空的（T13 才接线），
/// 用 [`Harness::router`] 去验「401 / 无鉴权可达」会一律拿到 **404**（axum 的 `layer`
/// 只包裹已挂上的路由，未命中就走外层 fallback）。所以那些测试用本函数挂自己的子树。
pub fn full_with(
    app: &AppState,
    routes: axum::Router<AppState>,
    public: axum::Router<AppState>,
) -> axum::Router {
    struct Adhoc {
        routes: axum::Router<AppState>,
        public: axum::Router<AppState>,
    }

    impl Module for Adhoc {
        fn name(&self) -> &'static str {
            "adhoc"
        }

        fn render(
            &self,
            _s: &bui_schema::model::State,
            _c: &crate::reconcile::RenderCtx,
        ) -> Vec<crate::reconcile::Artifact> {
            Vec::new()
        }

        fn routes(&self) -> axum::Router<AppState> {
            self.routes.clone()
        }

        fn public_routes(&self) -> axum::Router<AppState> {
            self.public.clone()
        }
    }

    crate::api::router(app.clone(), &[Arc::new(Adhoc { routes, public })])
}

pub async fn raw(
    router: &axum::Router,
    method: &str,
    uri: &str,
    tok: Option<&str>,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
    use tower::ServiceExt;
    let mut b = axum::http::Request::builder().method(method).uri(uri);
    if let Some(t) = tok {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => b.body(axum::body::Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

pub async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    tok: Option<&str>,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let (status, _h, bytes) = raw(router, method, uri, tok, body).await;
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

pub async fn text(router: &axum::Router, uri: &str) -> (axum::http::StatusCode, String) {
    let (status, _h, bytes) = raw(router, "GET", uri, None, None).await;
    (status, String::from_utf8_lossy(&bytes).to_string())
}
