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
}
