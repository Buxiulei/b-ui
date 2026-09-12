//! 三种订阅 + `/api/nodes`（spec §4.3 的「无鉴权」组、§4.4）。
//!
//! 四个端点的节点集合都来自 `bui_schema::nodes::nodes_for`，渲染全部交给
//! `bui_schema::render::subscription`：P2 一行拼装逻辑都不写（总纲 C1、审计 web-C15）。

use super::Shared;
use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::model::{ResidentialGroup, State as BuiState};
use bui_schema::nodes::{nodes_for, Node};
use bui_schema::render::subscription::{clash, singbox, uri_list};
use bui_schema::render::SplitRules;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

/// `/api/nodes/<user>` 的载荷 = `nodes_for()` 与 `SplitRules` 的直接序列化
/// （总纲裁决记录 + P4 决策 6；**不得**自己拼 JSON）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodesPayload {
    pub user: String,
    pub split: SplitRules,
    pub nodes: Vec<Node>,
}

/// 按用户名取出「节点集合 + 分流规则」；用户不存在返回 `None`。
///
/// 分组取 `state.residential.default_group()`；理论上不会缺（`Residential::default()`
/// 就带一个 `default`），缺了就用 `ResidentialGroup::default()`——它 `enabled=false`
/// ⇒ `pool_active()` 为假 ⇒ 订阅里不出现住宅分流规则，与 v3「未启用不给关键字」一致。
pub fn nodes_and_split(state: &BuiState, username: &str) -> Option<(Vec<Node>, SplitRules)> {
    let user = state.users.iter().find(|u| u.username == username)?;
    let nodes = nodes_for(user, &state.node, &state.residential);
    let fallback = ResidentialGroup::default();
    let group = state.residential.default_group().unwrap_or(&fallback);
    Some((nodes, SplitRules::from_group(group)))
}

/// 移植 `web/server.js:1800`：`encodeURIComponent(username).replace(/%/g, "_") + ".json"`。
/// `encodeURIComponent` 不编码的集合是 `A-Za-z0-9-_.!~*'()`。
pub fn safe_filename(username: &str) -> String {
    const KEEP: &[u8] = b"-_.!~*'()";
    let mut out = String::with_capacity(username.len() + 5);
    for b in username.as_bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(b) {
            out.push(*b as char);
        } else {
            out.push('_');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out.push_str(".json");
    out
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "User not found"})),
    )
        .into_response()
}

async fn get_sub(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, _split)) = nodes_and_split(&state, &user) else {
        return not_found();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        uri_list(&nodes, &user),
    )
        .into_response()
}

async fn get_subscription(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else {
        return not_found();
    };
    // 总纲 C1：dial_ip = node.public_ip（IPv6 接管设计的「按 IP 拨号、SNI 用域名」）
    let cfg = singbox(&nodes, &split, &state.node.public_ip);
    let body = serde_json::to_vec_pretty(&cfg).unwrap_or_default();
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{}\"", safe_filename(&user)),
            ),
        ],
        body,
    )
        .into_response()
}

async fn get_clash(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else {
        return not_found();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        clash(&nodes, &user, &split),
    )
        .into_response()
}

async fn get_nodes(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else {
        return not_found();
    };
    (StatusCode::OK, Json(NodesPayload { user, split, nodes })).into_response()
}

/// 四个端点都无鉴权（spec §4.3「无鉴权（按用户名，沿用 v3）」），挂在 `public_routes()` 里。
///
/// `_shared` 参数是**有意留的**：四个 handler 只需要 `AppState`，但 Task 13 把所有子路由按
/// 同一个签名 `fn(Arc<Shared>) -> Router<AppState>` 合并，签名统一比省一个下划线更值。
pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/sub/{user}", get(get_sub))
        .route("/api/subscription/{user}", get(get_subscription))
        .route("/api/clash/{user}", get(get_clash))
        .route("/api/nodes/{user}", get(get_nodes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, raw, send, text};
    use pretty_assertions::assert_eq;

    #[test]
    fn filenames_follow_the_v3_encoding() {
        assert_eq!(safe_filename("alice"), "alice.json");
        assert_eq!(safe_filename("a-b_c.d"), "a-b_c.d.json");
        // encodeURIComponent("张") = "%E5%BC%A0"，把 % 换成 _ ⇒ "_E5_BC_A0"
        assert_eq!(safe_filename("张"), "_E5_BC_A0.json");
        assert_eq!(safe_filename("a b"), "a_20b.json");
    }

    #[tokio::test]
    async fn nodes_endpoint_serializes_nodes_for_and_split_rules_verbatim() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", "/api/nodes/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["user"], "alice");
        // 与 P4 决策 6 的线格式逐字对齐：kind 是 snake_case，transport 用内部标签 `type`
        let state = h.store.read().await;
        let expect = serde_json::to_value(NodesPayload {
            user: "alice".into(),
            split: bui_schema::render::SplitRules::from_group(
                state.residential.default_group().unwrap(),
            ),
            nodes: bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential),
        })
        .unwrap();
        assert_eq!(
            v, expect,
            "必须是 nodes_for + SplitRules 的直接序列化，不能自己拼"
        );
        assert_eq!(v["nodes"][0]["kind"], "reality_direct");
        assert_eq!(v["nodes"][0]["transport"]["type"], "reality");
        assert_eq!(v["split"]["enabled"], false, "sample_state 的住宅池没启用");
    }

    #[tokio::test]
    async fn sub_is_base64_of_the_uri_list() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/sub/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            bui_schema::render::subscription::uri_list(&nodes, "alice")
        );
    }

    #[tokio::test]
    async fn subscription_is_the_singbox_config_dialed_by_public_ip() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/subscription/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "application/json; charset=utf-8");
        assert_eq!(
            headers["content-disposition"],
            "inline; filename=\"alice.json\""
        );
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        let split =
            bui_schema::render::SplitRules::from_group(state.residential.default_group().unwrap());
        let want = bui_schema::render::subscription::singbox(&nodes, &split, &state.node.public_ip);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            want
        );
    }

    #[tokio::test]
    async fn clash_is_the_mihomo_yaml() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/clash/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/yaml; charset=utf-8");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        let split =
            bui_schema::render::SplitRules::from_group(state.residential.default_group().unwrap());
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            bui_schema::render::subscription::clash(&nodes, "alice", &split)
        );
    }

    #[tokio::test]
    async fn an_unknown_user_is_404_on_all_four_endpoints() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for p in [
            "/api/sub/ghost",
            "/api/subscription/ghost",
            "/api/clash/ghost",
            "/api/nodes/ghost",
        ] {
            let (s, v) = send(&r, "GET", p, None, None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "{p}");
            assert_eq!(v["error"], "User not found", "{p}");
        }
    }

    #[tokio::test]
    async fn all_four_endpoints_work_without_a_token_in_the_full_router() {
        let h = harness().await;
        // 整套装配（含 require_admin），但公开路由挂的是**本文件的** public_routes()：
        // `h.router()` 走 PanelModule，它要到 T13 才接线 ⇒ 这里会拿到 404 而不是 200。
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        for p in ["/api/sub/alice", "/api/clash/alice", "/api/nodes/alice"] {
            let (s, _) = text(&router, p).await;
            assert_eq!(
                s,
                axum::http::StatusCode::OK,
                "{p} 必须无鉴权可达（spec §4.3）"
            );
        }
        let (s, _) = text(&router, "/api/subscription/alice").await;
        assert_eq!(s, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn a_residential_pool_turns_the_split_rules_on() {
        let h = harness().await;
        h.store
            .update(|s| {
                let g = s.residential.groups.get_mut("default").unwrap();
                g.enabled = true;
                g.mode = bui_schema::model::ResiMode::Global;
                g.upstreams.push(bui_schema::model::Upstream {
                    id: uuid::Uuid::nil(),
                    name: "url-1".into(),
                    kind: bui_schema::model::UpstreamKind::Http,
                    host: "isp.example.net".into(),
                    port: 10007,
                    username: "u".into(),
                    password: "p".into(),
                    priority: 10,
                    provider: None,
                    region: None,
                    ports_allowed: None,
                    verified: None,
                });
            })
            .await
            .unwrap();
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (_, v) = send(&r, "GET", "/api/nodes/alice", None, None).await;
        assert_eq!(v["split"]["enabled"], true);
        assert_eq!(v["split"]["global"], true);
        assert!(
            !v["split"]["keywords"].as_array().unwrap().is_empty(),
            "keywords=null ⇒ 跟随 DEFAULT_KEYWORDS（67 条）"
        );
    }
}
