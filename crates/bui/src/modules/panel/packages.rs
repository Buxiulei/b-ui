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
use axum::http::{header, HeaderMap, StatusCode};
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
            if host
                .read_file(&dest)
                .ok()
                .flatten()
                .map(|b| sha256_hex(&b) == asset.sha256)
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

async fn get_install_command(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    let host = if host.is_empty() {
        app.store.read().await.node.domain.clone()
    } else {
        host
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

async fn get_package(AxPath(name): AxPath<String>, shared: Arc<Shared>) -> Response {
    if !safe_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid filename"})),
        )
            .into_response();
    }
    let ct = assets::content_type(&name);
    if let Ok(bytes) = std::fs::read(shared.packages_dir().join(&name)) {
        return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response();
    }
    // 引导脚本不在盘上：直接发嵌进二进制的那一份（永远与本机 bui 同版本）
    if name == INSTALL_SCRIPT {
        if let Some(bytes) = assets::install_script() {
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
            get(move |p: AxPath<String>| get_package(p, s_p.clone())),
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
    async fn install_command_uses_the_host_header_then_the_state_domain() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", "/api/install-command", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        // oneshot 的请求没有 Host 头 ⇒ 回落 state.node.domain
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

    #[tokio::test]
    async fn the_installer_script_comes_from_the_embed_when_it_is_not_on_disk() {
        let h = harness().await;
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) =
            raw(&router, "GET", "/packages/bui-c-install.sh", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/x-shellscript; charset=utf-8");
        assert_eq!(
            bytes,
            crate::modules::panel::assets::install_script().unwrap()
        );
    }
}
