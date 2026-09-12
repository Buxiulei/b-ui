//! 用户域（自助门户）API 的预留桩。spec §4.3：**v4 全部返回 501**
//! `{"error":"not_implemented"}`；请求与响应结构由 spec 附录 A 定义，照抄如下，
//! 将来实现时不必再回头查 spec：
//!
//! - `POST /api/me/login` `{ "username", "password" }` → `{ "token", "expires_at" }`
//! - `GET  /api/me` → `{ "user_id", "username", "entitlements", "usage", "expires_at" }`
//! - `GET  /api/me/subscription-links` → `{ "sub", "singbox", "clash", "nodes" }`（四个 URL）
//! - `GET  /api/me/entitlements` → 同 spec §4.1 的 `entitlements`
//! - `GET  /api/me/billing` → `{ "currency", "balance_minor", "orders": [...] }`
//! - `POST /api/me/orders` `{ "sku", "quantity", "region" }` → `{ "order_id", "status": "pending", "amount_minor" }`
//! - `GET  /api/me/orders/{id}` → 订单记录（spec §4.1 的 `Order`）
//!
//! 这些端点故意挂在 `public_routes()`（`require_admin` 外面）：用户域的凭据是
//! `user.portal_auth`，不是管理员 JWT；放在里面将来接门户还得再搬一次。

use super::Shared;
use crate::api::AppState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

/// 七个端点的路径与方法，`bui status` 与 P5 的验收脚本按它核对
pub const ME_ENDPOINTS: [(&str, &str); 7] = [
    ("POST", "/api/me/login"),
    ("GET", "/api/me"),
    ("GET", "/api/me/subscription-links"),
    ("GET", "/api/me/entitlements"),
    ("GET", "/api/me/billing"),
    ("POST", "/api/me/orders"),
    ("GET", "/api/me/orders/{id}"),
];

async fn not_implemented() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "not_implemented"})),
    )
        .into_response()
}

/// spec §4.3：用户域**全部返回 501** `{"error":"not_implemented"}`；请求/响应结构见附录 A。
/// 挂在 `public_routes()` 里（`POST /api/me/login` 本来就不该要管理员 token）。
pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/me/login", post(not_implemented))
        .route("/api/me", get(not_implemented))
        .route("/api/me/subscription-links", get(not_implemented))
        .route("/api/me/entitlements", get(not_implemented))
        .route("/api/me/billing", get(not_implemented))
        .route("/api/me/orders", post(not_implemented))
        .route("/api/me/orders/{id}", get(not_implemented))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, send};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn every_user_domain_endpoint_is_501_with_the_documented_body() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for (m, p) in ME_ENDPOINTS {
            // 路由里的 `{id}` 换成一个具体值再请求
            let uri = p.replace("{id}", "ord-1");
            let (s, v) = send(&r, m, &uri, None, Some(serde_json::json!({}))).await;
            assert_eq!(s, axum::http::StatusCode::NOT_IMPLEMENTED, "{m} {uri}");
            assert_eq!(
                v,
                serde_json::json!({"error": "not_implemented"}),
                "{m} {uri}"
            );
        }
        assert_eq!(ME_ENDPOINTS.len(), 7, "spec 附录 A 正好七个端点");
    }

    #[tokio::test]
    async fn the_user_domain_does_not_require_an_admin_token() {
        let h = harness().await;
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        // 501 而不是 401：它们在 require_admin 外面（否则将来接自助门户还要再搬一次）
        let (s, _) = send(
            &router,
            "POST",
            "/api/me/login",
            None,
            Some(serde_json::json!({"username": "a", "password": "b"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::NOT_IMPLEMENTED);
        let (s2, _) = send(&router, "GET", "/api/me", None, None).await;
        assert_eq!(s2, axum::http::StatusCode::NOT_IMPLEMENTED);
    }
}
