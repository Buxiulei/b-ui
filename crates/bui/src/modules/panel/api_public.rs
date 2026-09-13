//! 三种订阅 + `/api/nodes`（spec §4.3 的「无鉴权」组、§4.4）。
//!
//! 四个端点的节点集合都来自 `bui_schema::nodes::nodes_for`，渲染全部交给
//! `bui_schema::render::subscription`：P2 一行拼装逻辑都不写（总纲 C1、审计 web-C15）。
//!
//! 路径末段从 2026-09-14 裁决起是**每用户的随机订阅 token**（`bui_schema::sub`），旧的
//! 「用户名链接」只在全局宽限期内还认 —— 四个端点同一口径，见 [`resolve`]。

use super::Shared;
use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::model::{ResidentialGroup, State as BuiState, User};
use bui_schema::nodes::{nodes_for, Node};
use bui_schema::render::subscription::{clash, singbox, uri_list};
use bui_schema::render::SplitRules;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

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
    Some(nodes_and_split_of(state, user))
}

/// [`nodes_and_split`] 的按用户版（四个 handler 走 [`lookup`] 进这里，省一次按名重查）。
fn nodes_and_split_of(state: &BuiState, user: &User) -> (Vec<Node>, SplitRules) {
    let nodes = nodes_for(user, &state.node, &state.residential);
    let fallback = ResidentialGroup::default();
    let group = state.residential.default_group().unwrap_or(&fallback);
    (nodes, SplitRules::from_group(group))
}

/// 按路径末段取用户（2026-09-14 裁决「每用户随机订阅 token」），四个端点同一口径：
///
/// 1. 末段是 32 位小写十六进制（[`bui_schema::sub::is_sub_token`]）⇒ 按 `sub_token` 找，
///    比较走常量时间（`subtle`，与 `auth_hook::decide` 比 hy2 密码同口径）；
/// 2. 否则 ⇒ 只有全局宽限期（`system.legacy_sub_until`）还没到、且这个用户没被轮换过
///    （`legacy_sub_disabled == false`）时，才按用户名精确找；
/// 3. 其余一律 `None`。
///
/// 三条路都收敛到同一个 404（[`not_found`]）：**不给** 410 之类能区分「没这个用户」与
/// 「链接已过期」的回应，那等于白送一个免鉴权的用户名探测器。宽限期时刻解析不出来也当
/// 过期（fail-closed，与 `auth_hook` 处理 `expires_at` 同口径）。
pub fn resolve<'a>(state: &'a BuiState, seg: &str, now: OffsetDateTime) -> Option<&'a User> {
    if bui_schema::sub::is_sub_token(seg) {
        return state.users.iter().find(|u| {
            u.sub_token
                .as_deref()
                .is_some_and(|t| t.as_bytes().ct_eq(seg.as_bytes()).unwrap_u8() == 1)
        });
    }
    let until = crate::util::parse_rfc3339(state.system.legacy_sub_until.as_deref()?)?;
    if now >= until {
        return None;
    }
    state
        .users
        .iter()
        .find(|u| u.username == seg && !u.legacy_sub_disabled)
}

/// 四个 handler 的共用前半段：[`resolve`] → 节点集合 + 分流规则 + **查到的**用户名。
///
/// 返回的用户名一律来自期望态，不是路径末段：节点标签、`/api/nodes` 的 `user` 与
/// [`safe_filename`] 都用它，否则 token 链接下载到的文件名就是那个 token。
fn lookup(
    state: &BuiState,
    seg: &str,
    now: OffsetDateTime,
) -> Option<(String, Vec<Node>, SplitRules)> {
    let user = resolve(state, seg, now)?;
    let (nodes, split) = nodes_and_split_of(state, user);
    Some((user.username.clone(), nodes, split))
}

/// 移植 `web/server.js:1800`：`encodeURIComponent(username).replace(/%/g, "_") + ".json"`。
/// `encodeURIComponent` 不编码的集合是 `A-Za-z0-9-_.!~*'()`。
///
/// 入参必须是 [`resolve`] 查到的 `u.username`，**不是**路径末段：token 链接也得下载到
/// `<用户名>.json`。
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

async fn get_sub(State(app): State<AppState>, Path(seg): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((user, nodes, _split)) = lookup(&state, &seg, app.host.now()) else {
        return not_found();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        uri_list(&nodes, &user),
    )
        .into_response()
}

async fn get_subscription(State(app): State<AppState>, Path(seg): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((user, nodes, split)) = lookup(&state, &seg, app.host.now()) else {
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

async fn get_clash(State(app): State<AppState>, Path(seg): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((user, nodes, split)) = lookup(&state, &seg, app.host.now()) else {
        return not_found();
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        clash(&nodes, &user, &split),
    )
        .into_response()
}

/// `bui-c` 的「面板导入」靠它，所以它和三种订阅一样必须认 token —— 不认就退化成订阅导入、
/// 丢掉分流规则（2026-09-14 裁决第 5 条）。
async fn get_nodes(State(app): State<AppState>, Path(seg): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((user, nodes, split)) = lookup(&state, &seg, app.host.now()) else {
        return not_found();
    };
    (StatusCode::OK, Json(NodesPayload { user, split, nodes })).into_response()
}

/// 四个端点都无鉴权（spec §4.3），挂在 `public_routes()` 里；末段的解析口径见 [`resolve`]。
///
/// `_shared` 参数是**有意留的**：四个 handler 只需要 `AppState`，但 Task 13 把所有子路由按
/// 同一个签名 `fn(Arc<Shared>) -> Router<AppState>` 合并，签名统一比省一个下划线更值。
pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/sub/{seg}", get(get_sub))
        .route("/api/subscription/{seg}", get(get_subscription))
        .route("/api/clash/{seg}", get(get_clash))
        .route("/api/nodes/{seg}", get(get_nodes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, raw, send, text, Harness};
    use pretty_assertions::assert_eq;

    /// 夹具 token：`sample_state` 的 alice 没有 `sub_token`（= 升级上来还没补齐的老 state），
    /// 四个端点的正路一律走 token，所以每个用例先给她一个。
    const TOK: &str = "0123456789abcdef0123456789abcdef";
    /// 另一个用户的 token（比对必须逐字，不能张冠李戴）。
    const OTHER_TOK: &str = "fedcba9876543210fedcba9876543210";

    async fn with_token(h: &Harness) {
        h.store
            .update(|s| s.users[0].sub_token = Some(TOK.into()))
            .await
            .unwrap();
    }

    /// 开一段还没到期的宽限期（假时钟停在 2026-09-11，截止时刻给到 09-18）。
    async fn open_grace(h: &Harness) {
        h.store
            .update(|s| s.system.legacy_sub_until = Some("2026-09-18T00:00:00Z".into()))
            .await
            .unwrap();
    }

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
        with_token(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", &format!("/api/nodes/{TOK}"), None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["user"], "alice", "载荷里是用户名，不是路径末段的 token");
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
        with_token(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", &format!("/api/sub/{TOK}"), None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            bui_schema::render::subscription::uri_list(&nodes, "alice"),
            "节点标签用查到的用户名，不是 token"
        );
    }

    #[tokio::test]
    async fn subscription_is_the_singbox_config_dialed_by_public_ip() {
        let h = harness().await;
        with_token(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) =
            raw(&r, "GET", &format!("/api/subscription/{TOK}"), None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "application/json; charset=utf-8");
        assert_eq!(
            headers["content-disposition"], "inline; filename=\"alice.json\"",
            "文件名来自查到的用户名 —— 用路径末段就会让人下载到一个叫 token 的文件"
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
        with_token(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", &format!("/api/clash/{TOK}"), None, None).await;
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

    /// 陌生末段一律 404，token 形态与用户名形态都一样：不给任何能区分「有没有这个用户」
    /// 的回应（那等于一个免鉴权的用户名探测器）。
    #[tokio::test]
    async fn an_unknown_segment_is_404_on_all_four_endpoints() {
        let h = harness().await;
        with_token(&h).await;
        open_grace(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for seg in ["ghost", OTHER_TOK, "not-hex-and-not-a-user"] {
            for p in ["sub", "subscription", "clash", "nodes"] {
                let (s, v) = send(&r, "GET", &format!("/api/{p}/{seg}"), None, None).await;
                assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "/api/{p}/{seg}");
                assert_eq!(v["error"], "User not found", "/api/{p}/{seg}");
            }
        }
    }

    #[tokio::test]
    async fn all_four_endpoints_need_no_admin_auth_in_the_full_router() {
        let h = harness().await;
        with_token(&h).await;
        // 整套装配（含 require_admin），但公开路由挂的是**本文件的** public_routes()：
        // `h.router()` 走 PanelModule，它要到 T13 才接线 ⇒ 这里会拿到 404 而不是 200。
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        for p in ["sub", "clash", "nodes", "subscription"] {
            let (s, _) = text(&router, &format!("/api/{p}/{TOK}")).await;
            assert_eq!(
                s,
                axum::http::StatusCode::OK,
                "/api/{p} 必须无鉴权可达（spec §4.3）"
            );
        }
    }

    /// 全新装机不开宽限期（`legacy_sub_until = None`）⇒ 用户名链接从来不可用。
    #[tokio::test]
    async fn a_fresh_install_never_honours_the_username_link() {
        let h = harness().await;
        with_token(&h).await;
        assert_eq!(h.store.read().await.system.legacy_sub_until, None);
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for p in ["sub", "subscription", "clash", "nodes"] {
            let (s, _) = send(&r, "GET", &format!("/api/{p}/alice"), None, None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "/api/{p}/alice");
        }
    }

    /// 宽限期内用户名链接照旧能用（v3 导入 / 老 v4 升级上来的用户手里全是这种链接），
    /// 到期之后四个端点一起变 404 —— 而 token 链接不受影响。
    #[tokio::test]
    async fn a_username_link_works_only_inside_the_grace_window() {
        let h = harness().await;
        with_token(&h).await;
        open_grace(&h).await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for p in ["sub", "subscription", "clash", "nodes"] {
            let (s, _) = text(&r, &format!("/api/{p}/alice")).await;
            assert_eq!(s, axum::http::StatusCode::OK, "宽限期内 /api/{p}/alice");
        }
        // 假时钟推过截止时刻（09-11 + 8 天 > 09-18）
        h.host.advance(8 * 86400);
        for p in ["sub", "subscription", "clash", "nodes"] {
            let (s, v) = send(&r, "GET", &format!("/api/{p}/alice"), None, None).await;
            assert_eq!(
                s,
                axum::http::StatusCode::NOT_FOUND,
                "宽限期过后 /api/{p}/alice"
            );
            assert_eq!(
                v["error"], "User not found",
                "不许给出「过期」这种可区分回应"
            );
        }
        let (s, _) = text(&r, &format!("/api/sub/{TOK}")).await;
        assert_eq!(s, axum::http::StatusCode::OK, "token 链接不受宽限期影响");
    }

    /// 轮换过的用户（`legacy_sub_disabled`）在宽限期内也不认用户名链接：不然换了凭据，
    /// 旧链接还能取到新凭据，轮换就是空转。
    #[tokio::test]
    async fn a_rotated_user_loses_the_username_link_inside_the_grace_window() {
        let h = harness().await;
        with_token(&h).await;
        open_grace(&h).await;
        h.store
            .update(|s| s.users[0].legacy_sub_disabled = true)
            .await
            .unwrap();
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for p in ["sub", "subscription", "clash", "nodes"] {
            let (s, _) = send(&r, "GET", &format!("/api/{p}/alice"), None, None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "/api/{p}/alice");
        }
        let (s, _) = text(&r, &format!("/api/sub/{TOK}")).await;
        assert_eq!(s, axum::http::StatusCode::OK, "token 链接照旧");
    }

    /// [`resolve`] 的判定矩阵（纯函数，比四个端点各跑一遍便宜）。
    #[tokio::test]
    async fn resolve_covers_the_whole_decision_matrix() {
        let h = harness().await;
        with_token(&h).await;
        let inside = crate::util::parse_rfc3339("2026-09-11T00:00:00Z").unwrap();
        let after = crate::util::parse_rfc3339("2026-09-30T00:00:00Z").unwrap();

        // 没开宽限期：token 认，用户名不认
        let s = h.store.read().await;
        assert_eq!(
            resolve(&s, TOK, inside).map(|u| &u.username[..]),
            Some("alice")
        );
        assert!(resolve(&s, "alice", inside).is_none());
        assert!(
            resolve(&s, OTHER_TOK, inside).is_none(),
            "别人的 token 不认"
        );
        drop(s);

        open_grace(&h).await;
        let s = h.store.read().await;
        assert_eq!(
            resolve(&s, "alice", inside).map(|u| &u.username[..]),
            Some("alice"),
            "宽限期内按用户名"
        );
        assert!(resolve(&s, "alice", after).is_none(), "过了截止时刻不认");
        // 边界：`now == 截止时刻` 已经算过期
        let edge = crate::util::parse_rfc3339("2026-09-18T00:00:00Z").unwrap();
        assert!(resolve(&s, "alice", edge).is_none(), "截止那一刻就不认了");
        drop(s);

        // 截止时刻解析不出来 ⇒ fail-closed（当过期），不是「永久有效」
        h.store
            .update(|s| s.system.legacy_sub_until = Some("下周三".into()))
            .await
            .unwrap();
        let s = h.store.read().await;
        assert!(
            resolve(&s, "alice", inside).is_none(),
            "解析失败要 fail-closed"
        );
        assert_eq!(
            resolve(&s, TOK, inside).map(|u| &u.username[..]),
            Some("alice"),
            "token 这条路和宽限期无关"
        );
    }

    /// token 形态的末段**只**走 token 那条路：就算某人的用户名刚好长成 32 位小写十六进制，
    /// 也不会在宽限期里被当用户名认出来（[`resolve`] 第 1 步优先，是设计裁决）。
    #[tokio::test]
    async fn a_hex_looking_username_still_goes_down_the_token_path() {
        let h = harness().await;
        h.store
            .update(|s| {
                s.users[0].username = OTHER_TOK.into();
                s.users[0].sub_token = Some(TOK.into());
                s.system.legacy_sub_until = Some("2026-09-18T00:00:00Z".into());
            })
            .await
            .unwrap();
        let s = h.store.read().await;
        let inside = crate::util::parse_rfc3339("2026-09-11T00:00:00Z").unwrap();
        assert!(resolve(&s, OTHER_TOK, inside).is_none());
        assert!(resolve(&s, TOK, inside).is_some());
    }

    #[tokio::test]
    async fn a_residential_pool_turns_the_split_rules_on() {
        let h = harness().await;
        with_token(&h).await;
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
        let (_, v) = send(&r, "GET", &format!("/api/nodes/{TOK}"), None, None).await;
        assert_eq!(v["split"]["enabled"], true);
        assert_eq!(v["split"]["global"], true);
        assert!(
            !v["split"]["keywords"].as_array().unwrap().is_empty(),
            "keywords=null ⇒ 跟随 DEFAULT_KEYWORDS（67 条）"
        );
    }
}
