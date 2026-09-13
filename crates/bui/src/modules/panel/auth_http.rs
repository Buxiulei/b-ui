//! Hysteria2 `auth.type: http` 的服务端：守护进程**进程内**应答鉴权，不再每条连接 fork 一次钩子。
//!
//! 背景（2026-09-13 主理人裁决）：bwg-rick 压测 200 登录/秒 p99 37ms，判据是 < 20ms，
//! 瓶颈是 `auth.type: command` 每次登录 fork 一个进程（本机 strace 里 clone + futex +
//! 线程栈 mmap/munmap 就占 ≈300µs/次，加上 exec 与动态链接总计 ≈15ms 固定开销）。
//! 于是默认改成 http，[`crate::modules::panel::auth_hook`] 保留为退路开关
//! （`bui set hy2-auth command`）。
//!
//! 三条硬约束：
//! - **独立监听 `127.0.0.1:AUTH_HTTP_PORT`**，绝不挂在面板的 8080 上：面板经 Caddy 对外，
//!   挂上去等于把鉴权面暴露到公网。
//! - **fail-closed**：请求读不动、JSON 解析不了、判定超时，一律 `{"ok": false}`。
//! - **判定逻辑与钩子同一份** [`auth_hook::decide`]（常量时间比密码、`blocked`、`expires_at`），
//!   日志也走同一个 [`auth_hook::log_line`]，格式仍是 `<RFC3339> <addr> <用户名> <结果>`，
//!   所以 m1 / m3 验收脚本对 `auth-hook.log` 的断言一个字都不用改。
//!
//! 数据源是**进程内**的鉴权快照，与 `auth-snapshot.json` 同源（都是
//! `Snapshot::from_state(state, blocked)`），由 [`refresh_loop`] 在 state 变化时刷新 ——
//! 请求路径上不读盘。

use crate::api::Event;
use crate::modules::panel::auth_hook::{self, Decision};
use crate::modules::panel::snapshot::Snapshot;
use crate::modules::panel::{users, Shared};
use crate::reconcile::DaemonCtx;
use crate::sys::Host;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::paths::Paths;
use bui_schema::render::hysteria::AUTH_HTTP_PORT;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// 服务端自己掐的硬超时：内核那边一条连接卡住就是一个用户连不上，宁可当场拒。
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(1);
/// 内存快照的兜底刷新间隔，与 [`users::SYNC_INTERVAL_SECS`]（重写 `auth-snapshot.json`
/// 的那一条安全网）同值 —— 两边的新鲜度必须一致，否则切一次模式就会改变限额生效的快慢。
pub const REFRESH_INTERVAL_SECS: u64 = users::SYNC_INTERVAL_SECS;

/// Hysteria2 发来的请求体（v2 官方格式）。字段全部 `default`：少一个字段也要能判成拒绝
/// 而不是 422 —— 内核收到非 2xx 只会记一行错误，用户看到的是同样的失败，但我们就少了一行日志。
#[derive(Debug, Default, Deserialize)]
pub struct AuthRequest {
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub tx: u64,
}

/// 响应体：放行 `{"ok":true,"id":"<user_id>"}`，拒绝 `{"ok":false}`。
/// `id` 就是 `/traffic`、`/online`、`/kick` 的键（spec §3.2）。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AuthReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl AuthReply {
    pub fn deny() -> Self {
        Self {
            ok: false,
            id: None,
        }
    }
}

/// 鉴权服务的进程内状态：一份快照 + 写日志要的 base 目录 + 时钟。
pub struct AuthHttp {
    paths: Paths,
    host: Arc<dyn Host>,
    snapshot: RwLock<Arc<Snapshot>>,
}

impl AuthHttp {
    pub fn new(paths: Paths, host: Arc<dyn Host>) -> Self {
        Self {
            paths,
            host,
            snapshot: RwLock::new(Arc::new(Snapshot::empty())),
        }
    }

    /// 读一份当前快照。锁被 poison 也不能让鉴权崩：退化成空快照 ⇒ 谁都进不来（fail-closed）。
    pub fn snapshot(&self) -> Arc<Snapshot> {
        match self.snapshot.read() {
            Ok(g) => g.clone(),
            Err(_) => Arc::new(Snapshot::empty()),
        }
    }

    pub fn set_snapshot(&self, snap: Snapshot) {
        if let Ok(mut g) = self.snapshot.write() {
            *g = Arc::new(snap);
        }
    }

    /// 判一次并落一行日志。纯粹到可以直接单测：不碰网络、不读盘（日志是追加写）。
    pub fn decide_and_log(&self, req: &AuthRequest) -> AuthReply {
        let now = self.host.now();
        let decision = auth_hook::decide(&self.snapshot(), &req.auth, now);
        let username = req.auth.split_once(':').map(|(u, _)| u).unwrap_or("-");
        let addr = if req.addr.is_empty() { "-" } else { &req.addr };
        auth_hook::log_line(&self.paths, now, addr, username, decision.label());
        match decision {
            Decision::Allow { user_id } => AuthReply {
                ok: true,
                id: Some(user_id),
            },
            Decision::Deny { .. } => AuthReply::deny(),
        }
    }
}

/// `POST /auth`。
///
/// 收 `axum::extract::Json` 而不是自己解 `Bytes`：解析失败时 axum 回 400/422，而内核把任何
/// 非 2xx 都当成拒绝 —— 与 `{"ok":false}` 同效，也仍然 fail-closed。真正需要自己兜的是
/// **能解析但字段缺失**的那一类，靠 [`AuthRequest`] 的 `#[serde(default)]` 兜住。
async fn auth(State(st): State<Arc<AuthHttp>>, Json(req): Json<AuthRequest>) -> Response {
    let _ = req.tx; // 内核给的是本连接已用上行字节数，v4 的限额走采样，不在这里判
    Json(st.decide_and_log(&req)).into_response()
}

/// 1 秒服务端超时：到点一律 `{"ok": false}`（fail-closed）。
async fn timeout(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(AUTH_TIMEOUT, next.run(req)).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!("鉴权请求超过 {:?}，按拒绝返回", AUTH_TIMEOUT);
            Json(AuthReply::deny()).into_response()
        }
    }
}

pub fn router(st: Arc<AuthHttp>) -> axum::Router {
    axum::Router::new()
        .route("/auth", axum::routing::post(auth))
        .layer(axum::middleware::from_fn(timeout))
        .with_state(st)
}

/// 监听 `127.0.0.1:AUTH_HTTP_PORT` 并一直服务。绑不上就记 error 返回：
/// hysteria 那边随即全员拒绝（fail-closed），watchdog 会在日志里看到连不上并记一条事件。
pub async fn serve(st: Arc<AuthHttp>) -> anyhow::Result<()> {
    let bind = format!("127.0.0.1:{AUTH_HTTP_PORT}");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(bind = %bind, "Hysteria2 http 鉴权就绪（只监听回环）");
    axum::serve(listener, router(st)).await?;
    Ok(())
}

/// 后台任务：绑定 + 服务，失败只记 error（不能让整个守护进程退出）。
pub async fn serve_loop(st: Arc<AuthHttp>) {
    if let Err(e) = serve(st).await {
        tracing::error!(error = %e, port = AUTH_HTTP_PORT, "http 鉴权监听失败");
    }
}

/// 把「期望态 + 本轮拒绝集合」刷进内存快照。与 `users::sync_now` 算的是同一件事，
/// 所以两边永远给出同一个判定。
pub async fn refresh(ctx: &DaemonCtx, shared: &Shared, st: &AuthHttp) {
    let now = ctx.host.now();
    let pending = shared.pending().await.clone();
    let state = ctx.store.read().await;
    let blocked = users::blocked_set(&state, &pending, now);
    st.set_snapshot(Snapshot::from_state(&state, &blocked));
}

/// 触发点与 `users::sync_loop` 逐条对齐（`StateChanged` + 每 60 秒安全网 + 落后即全量），
/// 这样内存快照与 `auth-snapshot.json` 的新鲜度一致。
pub async fn refresh_loop(ctx: DaemonCtx, shared: Arc<Shared>, st: Arc<AuthHttp>) {
    let mut rx = ctx.bus.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(REFRESH_INTERVAL_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    refresh(&ctx, &shared, &st).await;
    loop {
        tokio::select! {
            _ = tick.tick() => refresh(&ctx, &shared, &st).await,
            ev = rx.recv() => match ev {
                Ok(Event::StateChanged(_)) => refresh(&ctx, &shared, &st).await,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    refresh(&ctx, &shared, &st).await
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::snapshot::SnapshotUser;
    use crate::modules::panel::testsupport;
    use crate::sys::fake::FakeHost;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    fn snap() -> Snapshot {
        Snapshot {
            schema: 1,
            users: BTreeMap::from([
                (
                    "alice".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into(),
                        hy2_password: "pw:with:colons".into(),
                        expires_at: None,
                        blocked: false,
                    },
                ),
                (
                    "carol".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000cc".into(),
                        hy2_password: "pw-carol".into(),
                        expires_at: None,
                        blocked: true,
                    },
                ),
            ]),
        }
    }

    fn state(d: &tempfile::TempDir) -> (Arc<AuthHttp>, Paths) {
        let paths = Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let st = Arc::new(AuthHttp::new(paths.clone(), Arc::new(FakeHost::new())));
        st.set_snapshot(snap());
        (st, paths)
    }

    async fn post(router: &axum::Router, body: &str) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method("POST")
            .uri("/auth")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn a_correct_password_gets_ok_true_and_the_user_id() {
        let d = tempfile::tempdir().unwrap();
        let (st, paths) = state(&d);
        let (code, body) = post(
            &router(st),
            r#"{"addr":"203.0.113.10:51820","auth":"alice:pw:with:colons","tx":0}"#,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({"ok": true, "id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa"})
        );
        // 日志格式与钩子完全一致（m1 step6 / m3 的断言都按它写死）
        let log = std::fs::read_to_string(auth_hook::log_path(&paths)).unwrap();
        let last = log.trim_end().lines().last().unwrap();
        let f: Vec<&str> = last.split(' ').collect();
        assert_eq!(f.len(), 4, "四段：时间 addr 用户名 结果 —— {last}");
        assert_eq!(f[1], "203.0.113.10:51820");
        assert_eq!(f[2], "alice");
        assert_eq!(f[3], "allow");
        assert!(!log.contains("pw:with:colons"), "密码绝不能进日志：{log}");
    }

    #[tokio::test]
    async fn every_rejection_is_ok_false_without_an_id_and_still_logged() {
        let d = tempfile::tempdir().unwrap();
        let (st, paths) = state(&d);
        let r = router(st);
        for (body, want) in [
            (
                r#"{"addr":"a:1","auth":"alice:nope","tx":0}"#,
                "bad-password",
            ),
            (r#"{"addr":"a:1","auth":"dave:x","tx":0}"#, "no-such-user"),
            (
                r#"{"addr":"a:1","auth":"carol:pw-carol","tx":0}"#,
                "blocked",
            ),
            (r#"{"addr":"a:1","auth":"alice","tx":0}"#, "malformed-auth"),
            // 字段缺失也要判成拒绝，而不是 422
            (r#"{}"#, "malformed-auth"),
        ] {
            let (code, got) = post(&r, body).await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(got, serde_json::json!({"ok": false}), "{body}");
            let log = std::fs::read_to_string(auth_hook::log_path(&paths)).unwrap();
            assert!(
                log.trim_end().ends_with(want),
                "{body} 应记成 {want}：{}",
                log.trim_end().lines().last().unwrap()
            );
        }
    }

    /// 空快照（守护进程刚起、还没刷过）里谁都不在 ⇒ 全拒。
    #[tokio::test]
    async fn an_unrefreshed_snapshot_denies_everyone() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let st = Arc::new(AuthHttp::new(paths, Arc::new(FakeHost::new())));
        let (code, body) = post(&router(st), r#"{"addr":"a:1","auth":"alice:pw","tx":0}"#).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"ok": false}));
    }

    /// 只有 `POST /auth` 这一条路由：这个监听面不该多出任何别的东西。
    #[tokio::test]
    async fn nothing_but_post_auth_is_served() {
        let d = tempfile::tempdir().unwrap();
        let (st, _) = state(&d);
        let r = router(st);
        for (m, p) in [("GET", "/auth"), ("POST", "/"), ("GET", "/api/health")] {
            let res = r
                .clone()
                .oneshot(
                    Request::builder()
                        .method(m)
                        .uri(p)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(res.status(), StatusCode::OK, "{m} {p} 不该被服务");
        }
    }

    /// 判定跑过头一律拒绝：中间件对一条人为挂起的路由计时（`start_paused` 让时钟秒退）。
    #[tokio::test(start_paused = true)]
    async fn a_handler_that_hangs_is_denied_by_the_one_second_timeout() {
        let r = axum::Router::new()
            .route(
                "/auth",
                axum::routing::post(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Json(AuthReply {
                        ok: true,
                        id: Some("never".into()),
                    })
                }),
            )
            .layer(axum::middleware::from_fn(timeout));
        let (code, body) = post(&r, r#"{"addr":"a:1","auth":"alice:pw","tx":0}"#).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"ok": false}));
    }

    /// 内存快照与 `auth-snapshot.json` 同源：刷新之后两边给出同一个判定。
    #[tokio::test]
    async fn refresh_takes_the_same_users_as_the_on_disk_snapshot() {
        let h = testsupport::harness().await;
        let ctx = DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        };
        let st = AuthHttp::new(h.paths.clone(), h.host.clone());
        assert!(st.snapshot().users.is_empty(), "刷新前是空的");
        refresh(&ctx, &h.shared, &st).await;
        let mem = st.snapshot();
        let state = h.store.read().await;
        let disk = Snapshot::from_state(&state, &Default::default());
        assert_eq!(mem.as_ref(), &disk, "内存快照必须与写盘那份逐字段相同");
        assert!(mem.users.contains_key("alice"));
    }
}
