//! 管理员鉴权：argon2id 口令、JWT（24h，密钥持久化在 state）、按 IP 的登录限速，
//! 以及 unix socket 连接的 `SO_PEERCRED` 放行（spec §4.3）。
//!
//! 契约对齐 v3（`web/server.js:1693-1702`）：`POST /api/login` 请求体 `{"password":"…"}`，
//! 成功 `200 {"token":"…"}`，失败 `401 {"error":"Auth failed"}`，
//! 限速 `429 {"error":"Too many attempts. Try again later."}`。与 v3 的两处差异按 spec §4.3：
//! ① 限速窗口是 5 次/分钟/IP（v3 是 5 分钟）；② JWT 密钥取 `state.admin.jwt_secret`（持久化，
//! 修 web-C17：v3 每次重启随机，重启即把所有会话踢下线）。

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

pub const TOKEN_HOURS: i64 = 24;
pub const LOGIN_MAX_ATTEMPTS: u32 = 5;
pub const LOGIN_WINDOW_SECS: i64 = 60;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub admin: bool,
    pub exp: i64,
}

/// unix socket 连接注入的扩展（Task 14 的 `ipc.rs` 按 `SO_PEERCRED` 填）；uid 命中即视为管理员。
#[derive(Debug, Clone, Copy)]
pub struct UdsPeer {
    pub uid: u32,
}

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut OsRng);
    Ok(argon2::Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 哈希失败：{e}"))?
        .to_string())
}

pub fn verify_password(hash: &str, pw: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    match PasswordHash::new(hash) {
        Ok(parsed) => argon2::Argon2::default()
            .verify_password(pw.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 返回 `(token, expires_at)`；`expires_at` 是给前端看的 RFC3339 串。
pub fn issue_token(secret: &str, now: OffsetDateTime) -> anyhow::Result<(String, String)> {
    let exp = now + time::Duration::hours(TOKEN_HOURS);
    let claims = Claims {
        sub: "admin".into(),
        admin: true,
        exp: exp.unix_timestamp(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, crate::util::fmt_rfc3339(exp)))
}

pub fn decode_token(secret: &str, token: &str, now: OffsetDateTime) -> Option<Claims> {
    let mut v = jsonwebtoken::Validation::default();
    v.validate_exp = false; // 自己按传入的 now 判断，便于测试
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &v,
    )
    .ok()?;
    (data.claims.exp > now.unix_timestamp() && data.claims.admin).then_some(data.claims)
}

/// 登录限速器：**每个 router 实例一份**，作为 `AppState` 的字段随 `AppState` 克隆。
///
/// 生产上它等于「每进程一份」（一个进程只建一个 router）；写成进程级 `OnceLock` 会让同一个测试
/// 二进制里的多个 `#[tokio::test]` 共享同一张计数表——`FakeHost` 的时钟固定（窗口永不过期）、
/// `oneshot` 请求没有 `ConnectInfo`（所有测试共用 IP 键 `"unknown"`），谁后跑谁拿到错位的 429/401。
#[derive(Clone, Default)]
pub struct LoginLimiter(Arc<Mutex<HashMap<String, (OffsetDateTime, u32)>>>);

impl LoginLimiter {
    /// 锁被毒化时放行：宁可某一刻不限速，也不要把登录端点变成 500。
    pub fn allow(&self, ip: &str, now: OffsetDateTime) -> bool {
        let Ok(mut g) = self.0.lock() else {
            return true;
        };
        if g.get(ip)
            .is_some_and(|(first, _)| (now - *first).whole_seconds() > LOGIN_WINDOW_SECS)
        {
            g.remove(ip);
        }
        g.get(ip)
            .map(|(_, count)| *count < LOGIN_MAX_ATTEMPTS)
            .unwrap_or(true)
    }

    pub fn record(&self, ip: &str, ok: bool, now: OffsetDateTime) {
        let Ok(mut g) = self.0.lock() else {
            return;
        };
        if ok {
            g.remove(ip);
            return;
        }
        match g.get_mut(ip) {
            Some((first, count)) => {
                if (now - *first).whole_seconds() > LOGIN_WINDOW_SECS {
                    *first = now;
                    *count = 1;
                } else {
                    *count += 1;
                }
            }
            None => {
                g.insert(ip.to_string(), (now, 1));
            }
        }
    }
}

/// 取限速与日志用的客户端 IP。
///
/// 信任边界：面板只经 Caddy 反代暴露（`reverse_proxy 127.0.0.1:8080`），TCP 对端永远是
/// `127.0.0.1`，按 `ConnectInfo` 限速等于全站共用一个桶。Caddy 把真实客户端 IP **追加**到
/// `X-Forwarded-For` 末尾，所以：
/// 1. 有 `UdsPeer` → `"unix"`（socket 是 0600，本来就只有 root 能连，不限速）；
/// 2. 否则取 `X-Forwarded-For` 的**最后一跳**——那是我们自己的 Caddy 追加的，客户端伪造的前缀项忽略；
/// 3. 没有该头 → `ConnectInfo<SocketAddr>`；连它也没有（`oneshot` 测试）→ `"unknown"`。
///
/// 第 3 条要求 `axum::serve` 用 `into_make_service_with_connect_info::<SocketAddr>()`（Task 15）。
pub fn client_ip(req: &axum::extract::Request) -> String {
    if req.extensions().get::<UdsPeer>().is_some() {
        return "unix".to_string();
    }
    if let Some(xff) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(last) = xff.rsplit(',').next() {
            let last = last.trim();
            if !last.is_empty() {
                return last.to_string();
            }
        }
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `POST /api/login`（公开端点）。不用 `Json` 提取器：要先读 header 取 IP 再读 body。
pub async fn login(State(app): State<AppState>, req: axum::extract::Request) -> Response {
    let ip = client_ip(&req);
    let now = app.host.now();
    if !app.login.allow(&ip, now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(serde_json::json!({"error": "Too many attempts. Try again later."})),
        )
            .into_response();
    }
    let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"error": "Bad request"})),
            )
                .into_response()
        }
    };
    let password = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| {
            v.get("password")
                .and_then(|p| p.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let state = app.store.read().await;
    if !verify_password(&state.admin.password_hash, &password) {
        app.login.record(&ip, false, now);
        tracing::warn!(ip = %ip, "管理员登录失败");
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Auth failed"})),
        )
            .into_response();
    }
    app.login.record(&ip, true, now);
    match issue_token(&state.admin.jwt_secret, now) {
        Ok((token, expires_at)) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({"token": token, "expires_at": expires_at})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "签发 JWT 失败");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "Internal error"})),
            )
                .into_response()
        }
    }
}

/// 除 `/api/login` 外的所有端点都过这一层。
pub async fn require_admin(
    State(app): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // socket 是 0600，能连上的只有守护进程所属用户（生产下就是 root）；Task 14 按 SO_PEERCRED 填 uid
    if let Some(peer) = req.extensions().get::<UdsPeer>() {
        if peer.uid == 0 || peer.uid == nix::unistd::geteuid().as_raw() {
            return next.run(req).await;
        }
    }
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim()
        .to_string();
    let secret = app.store.read().await.admin.jwt_secret.clone();
    if token.is_empty() || decode_token(&secret, &token, app.host.now()).is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }
    const SECRET: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn password_hash_round_trips_and_rejects_wrong_input() {
        let h = hash_password("test123").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "test123"));
        assert!(!verify_password(&h, "test124"));
        assert!(!verify_password("not-a-hash", "test123"));
    }

    #[test]
    fn token_round_trips_and_expires_after_24h() {
        let (token, expires_at) = issue_token(SECRET, t0()).unwrap();
        assert_eq!(expires_at, "2026-09-12T00:00:00Z");
        let claims = decode_token(SECRET, &token, t0() + time::Duration::hours(23)).unwrap();
        assert!(claims.admin);
        assert_eq!(claims.sub, "admin");
        assert!(decode_token(SECRET, &token, t0() + time::Duration::hours(25)).is_none());
        assert!(decode_token("another-secret", &token, t0()).is_none());
        assert!(decode_token(SECRET, "garbage", t0()).is_none());
    }

    #[test]
    fn limiter_allows_five_failures_per_minute_per_ip() {
        let l = LoginLimiter::default();
        for i in 0..5 {
            assert!(l.allow("203.0.113.10", t0()), "第 {} 次应放行", i + 1);
            l.record("203.0.113.10", false, t0());
        }
        assert!(!l.allow("203.0.113.10", t0()), "第 6 次被限速");
        assert!(l.allow("203.0.113.11", t0()), "限速按 IP 隔离");
        assert!(
            l.allow("203.0.113.10", t0() + time::Duration::seconds(61)),
            "窗口过期后恢复"
        );
    }

    #[test]
    fn a_successful_login_clears_the_counter() {
        let l = LoginLimiter::default();
        for _ in 0..4 {
            l.record("203.0.113.10", false, t0());
        }
        l.record("203.0.113.10", true, t0());
        for _ in 0..5 {
            assert!(l.allow("203.0.113.10", t0()));
            l.record("203.0.113.10", false, t0());
        }
        assert!(!l.allow("203.0.113.10", t0()));
    }

    #[test]
    fn two_limiters_do_not_share_counters() {
        // 这条锁住「限速器在 AppState 里」的设计：两个 router 实例 = 两张计数表
        let (a, b) = (LoginLimiter::default(), LoginLimiter::default());
        for _ in 0..5 {
            a.record("203.0.113.10", false, t0());
        }
        assert!(!a.allow("203.0.113.10", t0()));
        assert!(b.allow("203.0.113.10", t0()));
    }

    #[test]
    fn client_ip_takes_the_last_forwarded_hop() {
        use axum::body::Body;
        use axum::http::Request as HttpRequest;
        // Caddy 追加的最后一跳才可信；客户端伪造的前缀项忽略
        let req = HttpRequest::builder()
            .uri("/api/login")
            .header("x-forwarded-for", "1.1.1.1, 203.0.113.10")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "203.0.113.10");
        let bare = HttpRequest::builder()
            .uri("/api/login")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&bare), "unknown");
        let mut uds = HttpRequest::builder()
            .uri("/api/login")
            .body(Body::empty())
            .unwrap();
        uds.extensions_mut().insert(UdsPeer { uid: 0 });
        assert_eq!(client_ip(&uds), "unix");
    }
}
