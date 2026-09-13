//! 嵌入的面板前端与客户端引导脚本（spec §1、§4.3 改动 1）。
//!
//! v3 是从 `ADMIN_DIR` 读盘（`web/server.js:1476-1513`）；v4 编进二进制，
//! 于是 `/opt/b-ui/admin/` 整棵目录连同 Node 一起消失。

use super::Shared;
use crate::api::AppState;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::sync::Arc;

/// 嵌入的面板前端（spec §1：`web/` 前端由 `rust-embed` 嵌入 `bui`；实际是 5 个文件）
#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/"]
#[include = "index.html"]
#[include = "app.js"]
#[include = "style.css"]
#[include = "qrcode.min.js"]
#[include = "logo.jpg"]
pub struct Web;

/// 嵌入的客户端首次安装脚本（P4 Task 13 交付，Task 12 的 `/packages/bui-c-install.sh` 用它）
#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../scripts/"]
#[include = "bui-c-install.sh"]
pub struct Scripts;

/// URL 路径 → 嵌入文件名（`/` 与 `/index.html` 都给 index.html）
pub const WEB_ROUTES: [(&str, &str); 6] = [
    ("/", "index.html"),
    ("/index.html", "index.html"),
    ("/app.js", "app.js"),
    ("/style.css", "style.css"),
    ("/qrcode.min.js", "qrcode.min.js"),
    ("/logo.jpg", "logo.jpg"),
];

pub fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("json") => "application/json; charset=utf-8",
        Some("sh") => "text/x-shellscript; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `index.html` 里的版本号占位符（v3 `web/server.js:1234` 的 `loadHTML()` 同样在发出前替换）
pub const VERSION_PLACEHOLDER: &str = "${VERSION}";

/// 取一个嵌入的前端文件（不存在返回 None）。
///
/// `index.html` 的 `${VERSION}` **全部**替换成本二进制的版本号：v3 由
/// `web/server.js` 的 `loadHTML()` 在发出前 `replace(/\${VERSION}/g, VERSION)`，
/// v4 把前端编进二进制后这一步一度漏掉，面板首页于是字面显示 `v${VERSION}`。
/// 其余资源（`app.js` / `style.css` / `qrcode.min.js` / `logo.jpg`）原样返回。
pub fn web_file(name: &str) -> Option<Vec<u8>> {
    let bytes = Web::get(name).map(|f| f.data.into_owned())?;
    if name != "index.html" {
        return Some(bytes);
    }
    // index.html 是仓库里的 UTF-8 文本；真出现非法字节时宁可原样发出，也不要 500
    match String::from_utf8(bytes) {
        Ok(html) => Some(
            html.replace(VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"))
                .into_bytes(),
        ),
        Err(e) => Some(e.into_bytes()),
    }
}

/// 取嵌入的 `bui-c-install.sh`（P4 Task 13 未合并时返回 None）
pub fn install_script() -> Option<Vec<u8>> {
    Scripts::get("bui-c-install.sh").map(|f| f.data.into_owned())
}

fn serve(name: &'static str) -> Response {
    match web_file(name) {
        Some(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type(name))],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 六条前端路由，全部无鉴权
pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    let mut r = axum::Router::new();
    for (path, name) in WEB_ROUTES {
        r = r.route(path, get(move || async move { serve(name) }));
    }
    r
}

#[cfg(test)]
mod tests {
    // `Web::iter` / `Web::get` 要的 `rust_embed::Embed` trait 由本文件顶部的 `use` 经
    // `super::*` 一并带进来，这里再写一遍会被 clippy 判 unused_imports。
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, raw};
    use pretty_assertions::assert_eq;

    #[test]
    fn exactly_the_five_front_end_files_are_embedded() {
        let mut names: Vec<String> = Web::iter()
            .map(|c: std::borrow::Cow<str>| c.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "app.js".to_string(),
                "index.html".to_string(),
                "logo.jpg".to_string(),
                "qrcode.min.js".to_string(),
                "style.css".to_string(),
            ]
        );
        assert!(web_file("index.html").unwrap().starts_with(b"<!DOCTYPE"));
        assert!(
            web_file("server.js").is_none(),
            "v3 的 Node 服务端不能被嵌进来"
        );
    }

    #[test]
    fn content_types_cover_every_route() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("app.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("qrcode.min.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(content_type("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("logo.jpg"), "image/jpeg");
        assert_eq!(content_type("whatever.bin"), "application/octet-stream");
    }

    #[tokio::test]
    async fn the_panel_is_served_without_a_token() {
        let h = harness().await;
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        for (path, name) in WEB_ROUTES {
            let (s, headers, bytes) = raw(&router, "GET", path, None, None).await;
            assert_eq!(s, axum::http::StatusCode::OK, "{path}");
            assert_eq!(headers["content-type"], content_type(name), "{path}");
            assert_eq!(
                bytes,
                web_file(name).unwrap(),
                "{path} 的内容要与嵌入的一致"
            );
        }
        // 未命中的路径落到 P1 `api::router()` 里被 `require_admin` 包住的 fallback（T0 的装配，
        // 已被 `api::mod` 的两条回归测试锁住），所以是 401 而不是 404；本条要锁的是
        // 「它不会被当成嵌入前端发出去」。
        let (s_miss, _, _) = raw(&router, "GET", "/nope.html", None, None).await;
        assert_eq!(s_miss, axum::http::StatusCode::UNAUTHORIZED);
    }

    /// spec §4.3 改动 1 的回归锁：前端不能再走 `GET /api/manage`，也不能再往
    /// localStorage 存明文管理员密码（审计 web-C6）。改坏了这条会立刻红。
    #[test]
    fn app_js_creates_users_over_post_and_never_stores_the_admin_password() {
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        assert!(!js.contains("/api/manage"), "app.js 还在调 /api/manage");
        assert!(
            !js.contains("localStorage.setItem(\"ap\""),
            "app.js 还在存明文管理员密码"
        );
        assert!(
            !js.contains("localStorage.getItem(\"ap\")"),
            "app.js 还在读明文管理员密码"
        );
        assert!(
            js.contains("api(\"/users\", {"),
            "addUser 没改成 POST /api/users"
        );
    }

    /// 面板首页不能再字面显示 `v${VERSION}`（v3 `web/server.js` 的 `loadHTML()` 会替换，
    /// v4 把前端编进二进制后漏了这一步）。只对 index.html 替换，其余资源必须原样。
    #[tokio::test]
    async fn index_html_gets_the_real_version_and_other_assets_stay_byte_identical() {
        let html = String::from_utf8(web_file("index.html").unwrap()).unwrap();
        assert!(
            !html.contains(VERSION_PLACEHOLDER),
            "index.html 还留着 {VERSION_PLACEHOLDER} 占位符"
        );
        let want = format!("v{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            html.matches(&want).count(),
            2,
            "index.html 里两处版本号都要替换（登录副标题 + 顶部 ver-tag）"
        );
        for name in ["app.js", "style.css", "qrcode.min.js", "logo.jpg"] {
            assert_eq!(
                web_file(name).unwrap(),
                Web::get(name).unwrap().data.into_owned(),
                "{name} 不该被改一个字节"
            );
        }
        // 真发出去的那份也替换过，且 Content-Length 跟着替换后的长度走（axum 按 body 现算）
        let h = harness().await;
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&router, "GET", "/", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let served = String::from_utf8(bytes.clone()).unwrap();
        assert!(!served.contains(VERSION_PLACEHOLDER), "/ 发出的还是占位符");
        assert!(served.contains(&want));
        assert_eq!(
            headers["content-length"].to_str().unwrap(),
            bytes.len().to_string(),
            "Content-Length 要与替换后的正文一致"
        );
    }

    #[test]
    fn the_installer_script_is_embedded_from_the_scripts_folder() {
        // P4 Task 13 交付 scripts/bui-c-install.sh；它按 `$BUI_C_SOURCE/<artifact 键名>` 取二进制
        let s =
            install_script().expect("scripts/bui-c-install.sh 必须存在（前置：P4 Task 13 已合并）");
        let text = String::from_utf8(s).unwrap();
        assert!(text.starts_with("#!"), "引导脚本要有 shebang");
        assert!(
            text.contains("BUI_C_SOURCE"),
            "引导脚本要认 BUI_C_SOURCE（P4 决策 9）"
        );
    }

    /// spec §5.7：面板「事件」卡。只加一张卡，接到 `/api/incidents`，文本一律 textContent。
    #[test]
    fn the_incidents_card_is_embedded_and_wired_to_the_api() {
        let html = String::from_utf8(web_file("index.html").unwrap()).unwrap();
        for id in [
            "id=\"sys-inc-card\"",
            "id=\"sys-inc-body\"",
            "id=\"inc-refresh\"",
        ] {
            assert!(html.contains(id), "index.html 缺 {id}");
        }
        assert!(html.contains("onclick=\"loadIncidents()\""));
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        assert!(js.contains("function loadIncidents()"));
        assert!(js.contains("api(\"/incidents?limit=20\")"));
        assert!(
            js.contains("if (document.getElementById(\"sys-inc-body\")) loadIncidents();"),
            "登录后自动加载一次"
        );
        let body = js.split("function loadIncidents()").nth(1).unwrap();
        let body = body.split("\nfunction ").next().unwrap();
        assert!(
            !body.contains("innerHTML"),
            "事件文本来自日志，只许 textContent"
        );
    }

    /// 2026-09-14 裁决的回归锁：四个免鉴权订阅端点的路径末段是**每用户的随机订阅 token**，
    /// 面板不许再拿用户名拼 —— 公开仓库的 git 历史里有真实域名与用户名，旧口径下
    /// 「域名 + 用户名」就等于订阅凭据（响应体里有 hy2 明文密码与 vless uuid）。
    ///
    /// 撤销本阶段任一处安全语义（把 `genUri` 改回用户名拼链接、把重置入口藏掉、
    /// 把弹窗同步去掉）都必须让这一条红。
    #[test]
    fn app_js_builds_subscription_links_from_the_random_sub_token() {
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        // 路径一律由 subPath() 用 token 拼，所以 app.js 里不许再出现字面的 `/api/<kind>/`
        //（旧写法是 `"/api/sub/" + x.username`、`"/api/clash/" + encodeURIComponent(…)`）
        for kind in ["sub", "subscription", "clash", "nodes"] {
            let literal = format!("/api/{kind}/");
            assert!(
                !js.contains(&literal),
                "app.js 还在字面拼 {literal}<用户名>"
            );
        }
        assert!(
            js.contains("function subPath("),
            "缺 subPath()：拼订阅路径的唯一出口"
        );
        assert!(
            js.contains("\"/api/\" + kind + \"/\""),
            "subPath() 没在拼 /api/<kind>/<token>"
        );
        assert!(
            js.contains("x.subToken"),
            "subPath() 必须读面板投影的 subToken"
        );

        // 「重置订阅链接与凭据」：二次确认 + POST /api/users/{name}/rotate
        let rotate = js
            .split("function rotateSub()")
            .nth(1)
            .expect("app.js 缺 rotateSub()")
            .split("\nfunction ")
            .next()
            .unwrap();
        assert!(
            rotate.contains("confirm("),
            "轮换会让该用户现有客户端断连，必须二次确认"
        );
        assert!(rotate.contains("\"/rotate\""), "rotateSub 没打 /rotate");
        assert!(rotate.contains("method: \"POST\""), "rotate 端点是 POST");

        // 弹窗打开期间别的会话/CLI 轮换了同一用户 ⇒ 按新凭据重画，别把作废的那份
        // 复制或下载出去（端点对作废 token 一律回 404，管理员看不到任何提示）
        assert!(
            js.contains("syncOpenConfig(u);"),
            "load() 每轮刷新后要同步已打开的配置弹窗"
        );
        let sync = js
            .split("function syncOpenConfig(")
            .nth(1)
            .expect("app.js 缺 syncOpenConfig()")
            .split("\nfunction ")
            .next()
            .unwrap();
        for key in ["subToken", "password", "uuid"] {
            assert!(
                sync.contains(key),
                "syncOpenConfig 没比 {key}：它是可轮换的凭据"
            );
        }

        // 入口在配置弹窗里
        let html = String::from_utf8(web_file("index.html").unwrap()).unwrap();
        assert!(
            html.contains("id=\"cfg-rotate\""),
            "index.html 缺「重置订阅链接与凭据」按钮"
        );
        assert!(html.contains("onclick=\"rotateSub()\""));
    }

    /// 跨阶段的字段名契约：前端只认驼峰 `subToken`，而 [`super::super::users::PanelUser`]
    /// 的单词字段走 serde 默认名、多词字段才逐个 `rename`。轮换那一阶段若把
    /// `sub_token` 按默认名落下，投影发出的就是 `sub_token`，`subPath()` 对**每个**用户
    /// 都返回 null ⇒ 面板全量用户掉进「取不到订阅 token」分支、订阅链接与二维码全部消失，
    /// 而 fmt / clippy / 全量测试照样全绿（审查 2）。
    ///
    /// 所以把两侧钉在一起：`PanelUser` 声明 `sub_token` **当且仅当** users.rs 里写了
    /// `rename = "subToken"`。字段落地时漏了 rename、或改成别的名字，这一条立刻红。
    /// （投影那一侧还有一条正面断言 `the_panel_user_carries_the_sub_token`，
    /// 在声明字段的那个提交里。）
    #[test]
    fn the_panel_user_sub_token_key_is_exactly_what_app_js_reads() {
        let users_rs = include_str!("users.rs");
        let declares = users_rs.contains("pub sub_token:");
        let renamed = users_rs.contains("rename = \"subToken\"");
        assert_eq!(
            declares, renamed,
            "PanelUser 的 sub_token 必须带 #[serde(rename = \"subToken\")]：\
             前端 app.js 只读驼峰 subToken"
        );
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        assert!(
            js.contains("x.subToken"),
            "app.js 读的 key 变了，投影那一侧要一起改"
        );
    }
}
