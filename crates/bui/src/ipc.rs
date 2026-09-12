//! unix socket 服务端（0600 + SO_PEERCRED）与 CLI 客户端。
//!
//! spec §2.4：`/run/b-ui.sock` 权限 0600、只有 root；`sudo b-ui` 菜单的每一项都经它调守护进程 API。
//! 鉴权不走 JWT——socket 是 0600 且属守护进程用户，能连上就说明是同一用户或 root，因此服务端把
//! `SO_PEERCRED` 拿到的 uid 注入连接扩展 `UdsPeer`，由 `api::auth::require_admin` 放行；公开端点
//! `/api/login` 不受影响（仍然校验密码）。

use std::path::{Path, PathBuf};

/// 绑定 socket（0600）并把同一个 Router 服务在上面；连接扩展里注入 `UdsPeer`。
pub async fn serve_uds(path: &Path, app: axum::Router) -> anyhow::Result<()> {
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 陈旧的 socket 文件（上次崩溃留下的）会让 bind 报 EADDRINUSE
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %path.display(), "CLI socket 就绪");
    loop {
        let (stream, _) = listener.accept().await?;
        let uid = peer_uid(&stream).unwrap_or(u32::MAX);
        // `UdsPeer` 注入用 `Router::layer` 而不是 `ServiceBuilder::service(app)`：后者产出的
        // `FromFn<_, (), Router, _>` 不实现 `Service<Request<Incoming>>`（axum 只为带提取器元组的
        // `FromFn` 实现），`serve_connection` 因此编译不过。`Router::layer` 返回的仍是 `Router`。
        let app = app.clone().layer(axum::middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                req.extensions_mut()
                    .insert(crate::api::auth::UdsPeer { uid });
                next.run(req).await
            },
        ));
        tokio::spawn(async move {
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                .await
            {
                tracing::debug!(error = %e, "socket 连接结束");
            }
        });
    }
}

fn peer_uid(stream: &tokio::net::UnixStream) -> Option<u32> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials).ok().map(|c| c.uid())
}

#[derive(Clone)]
pub struct Client {
    path: PathBuf,
}

impl Client {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// socket 存在且能连上
    pub async fn available(&self) -> bool {
        tokio::net::UnixStream::connect(&self.path).await.is_ok()
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<(u16, serde_json::Value)> {
        use http_body_util::BodyExt;
        use hyper_util::rt::TokioIo;

        let stream = tokio::net::UnixStream::connect(&self.path)
            .await
            .map_err(|e| anyhow::anyhow!("连不上守护进程（{}）：{e}", self.path.display()))?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let req = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(http_body_util::Full::new(hyper::body::Bytes::from(payload)))?;
        let res = sender.send_request(req).await?;
        let status = res.status().as_u16();
        let bytes = res.into_body().collect().await?.to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        Ok((status, json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{auth::hash_password, AppState, EventBus};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    async fn spawn_server(dir: &std::path::Path) -> (PathBuf, Arc<FakeHost>) {
        let sock = dir.join("b-ui.sock");
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = hash_password("test123").unwrap();
        let store = Store::create(dir.join("state.json"), state).await.unwrap();
        let runtime = Runtime::load(dir.join("runtime.json"));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in crate::reconcile::MANAGED_UNITS {
                i.units_active.insert(format!("{u}.service"));
                i.units_enabled.insert(format!("{u}.service"));
            }
        });
        let app = crate::api::router(
            AppState {
                store,
                bus: EventBus::new(),
                runtime,
                host: host.clone(),
                started_at: host.now(),
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            &[],
        );
        let s = sock.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&s, app).await;
        });
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (sock, host)
    }

    #[tokio::test]
    async fn socket_is_created_with_0600() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn local_peer_reaches_health_without_a_token() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let (status, body) = Client::new(&sock)
            .request("GET", "/api/health", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["version"], "4.0.0");
        assert_eq!(body["node"], "node-a");
    }

    #[tokio::test]
    async fn login_over_the_socket_still_checks_the_password() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let c = Client::new(&sock);
        let (status, body) = c
            .request(
                "POST",
                "/api/login",
                Some(serde_json::json!({"password": "wrong"})),
            )
            .await
            .unwrap();
        assert_eq!(status, 401);
        assert_eq!(body["error"], "Auth failed");
        let (status, body) = c
            .request(
                "POST",
                "/api/login",
                Some(serde_json::json!({"password": "test123"})),
            )
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(body["token"].is_string());
    }

    #[tokio::test]
    async fn service_action_over_the_socket_hits_systemctl() {
        let d = tempfile::tempdir().unwrap();
        let (sock, host) = spawn_server(d.path()).await;
        let (status, body) = Client::new(&sock)
            .request("POST", "/api/services/xray/restart", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert!(host.ops().contains(&"systemd:restart:xray".to_string()));
    }

    #[tokio::test]
    async fn available_is_false_when_nothing_is_listening() {
        let d = tempfile::tempdir().unwrap();
        assert!(!Client::new(d.path().join("absent.sock")).available().await);
        let (sock, _h) = spawn_server(d.path()).await;
        assert!(Client::new(&sock).available().await);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced_on_bind() {
        let d = tempfile::tempdir().unwrap();
        let sock = d.path().join("b-ui.sock");
        std::fs::write(&sock, b"stale").unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let host = Arc::new(FakeHost::new());
        let app = crate::api::router(
            AppState {
                store,
                bus: EventBus::new(),
                runtime: Runtime::load(d.path().join("runtime.json")),
                host: host.clone(),
                started_at: host.now(),
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            &[],
        );
        let s = sock.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&s, app).await;
        });
        for _ in 0..100 {
            if Client::new(&sock).available().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("陈旧 socket 文件没被替换");
    }
}
