//! 内核二进制管理：`manifest.json`（总纲 C4）是版本与校验和的**唯一**来源，v4 不再轮询
//! GitHub API（spec §3.1、审计 §3.2「GitHub release 轮询 ×4 合并」）。
//!
//! 本模块负责：解析 manifest、按架构查资产、下载 + sha256 校验 + 落盘（实现
//! [`BinaryInstaller`]），以及从已装二进制的 `version` 输出反解版本号。
//!
//! **`reqwest::blocking` 的铁律**：reqwest 文档明写 `reqwest::blocking` 不得在 async 运行时里
//! 执行，否则 block 的时候直接 panic。因此 [`Fetcher`] 是**同步** trait，[`HttpFetcher`] 只允许
//! 在 `tokio::task::spawn_blocking` 的闭包里或纯同步的 CLI 路径里调用。
//!
//! manifest 地址只在 [`resolve_manifest_url`] / [`manifest_url`] 决定一次（总纲 C4「manifest
//! 来源与覆盖」）：`--manifest-url` > `--version`（套模板）> `$BUI_MANIFEST_URL` > 内置 latest。

use crate::reconcile::apply::BinaryInstaller;
use crate::sys::Host;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 服务端自带的四个内核二进制（键 = 二进制文件名）。
pub const KERNELS: [&str; 4] = ["hysteria", "xray", "sing-box", "caddy"];

/// 默认 manifest：GitHub Releases 的 `latest`。
pub const MANIFEST_URL: &str =
    "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json";

/// 指定版本的 manifest：GitHub 的形状是 `releases/download/<tag>/<asset>`（**不是**
/// `releases/<tag>/download/`）。
pub const MANIFEST_URL_TEMPLATE: &str =
    "https://github.com/Buxiulei/b-ui/releases/download/v{version}/manifest.json";

/// 环境变量覆盖（总纲 C4「manifest 来源与覆盖」明写由 P1 实现）。
pub const MANIFEST_URL_ENV: &str = "BUI_MANIFEST_URL";

/// 总纲 C4 的形状：`version` 是 bui 自己的版本，`kernels` 是版本表（键 = state `versions` 的
/// 字段名，下划线），`artifacts` 的键固定 `<name>-linux-<amd64|arm64>`（name = 二进制名，
/// 连字符）。未知字段一律忽略（serde 默认），P5 往 manifest 里加字段不会打死 P1。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub kernels: BTreeMap<String, String>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, Asset>,
    /// 总纲 C4 的可选字段：低于此版本的 `bui` 必须先升到该版本（Task 17 的 `plan_upgrade` 消费）。
    #[serde(default)]
    pub min_upgrade_from: Option<String>,
}

/// 一个发布资产：裸二进制的下载地址与 sha256（P5 的 Actions 负责解包上游压缩包后重新上传）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

/// **同步** trait：实现会阻塞线程，所以每个调用点都必须在 `tokio::task::spawn_blocking` 里
/// （或在纯 CLI 的同步上下文里）执行；`Send + Sync + 'static` 是为了能以 `Arc<dyn Fetcher>`
/// 跨 `spawn_blocking` 边界传递。
///
/// `url` 允许是 `file://<abs path>` 或**不含 `://` 的本地路径**：[`HttpFetcher`] 直接读文件，
/// 于是升级/回滚演练既能用本机 `python3 -m http.server`，也能直接给一个文件路径
/// （裁决：M5 前不创建任何 Release/tag，演练不得依赖公开 Release）。
pub trait Fetcher: Send + Sync + 'static {
    fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>>;
}

pub struct HttpFetcher {
    client: std::sync::OnceLock<reqwest::blocking::Client>,
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self {
            client: std::sync::OnceLock::new(),
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for HttpFetcher {
    /// 只能在 `spawn_blocking` 线程或同步 CLI 路径里调用（见模块头的铁律）。
    fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        // `file://…` 或不含 `://` 的入参按本地文件读（总纲 C4 的覆盖方式之一；M5 演练不依赖
        // 公开 Release）。这一步必须在建 client 之前。
        if let Some(path) = url
            .strip_prefix("file://")
            .or_else(|| (!url.contains("://")).then_some(url))
        {
            return std::fs::read(path).map_err(|e| anyhow::anyhow!("读取 {path} 失败：{e}"));
        }
        // 客户端用 `OnceLock` 复用，避免每次下载都重建连接池。
        let client = self.client.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .user_agent(concat!("b-ui/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("rustls 客户端构建失败")
        });
        // Global Constraints：凭据不进日志。`BUI_MANIFEST_URL` / `--manifest-url` 允许带 basic
        // auth（`https://user:token@host/manifest.json`，私有 Release 或跳板机常这么给），所以
        // 凡是把 url 放进 tracing 字段或错误串的地方都过 `crate::redact::url_credentials`。
        //
        // reqwest 的 `Error` Display 会追加 ` for url (<完整 URL>)`，而 `url::Url` 的序列化含
        // userinfo。0.12 的 `RequestBuilder::new` 确实会先把 userinfo 挪进 Authorization 头
        // （所以本路径当下不泄漏），但那是 reqwest 的内部实现细节：**不能**让它成为「凭据不
        // 进日志」的唯一依靠。因此两处 reqwest 错误一律先 `without_url()` 剥掉 URL，再由本
        // 函数补一个脱敏地址——journal 与上抛给 Task 15/16/17 的错误串都只可能见到 `***:***`。
        let resp = client.get(url).send().map_err(|e| {
            let e = e.without_url();
            tracing::warn!(url = %crate::redact::url_credentials(url), error = %e, "下载失败");
            anyhow::Error::new(e)
                .context(format!("下载 {} 失败", crate::redact::url_credentials(url)))
        })?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "下载 {} 失败：HTTP {}",
                crate::redact::url_credentials(url),
                resp.status().as_u16()
            );
        }
        Ok(resp
            .bytes()
            .map_err(|e| e.without_url())
            .with_context(|| format!("读取 {} 的响应体失败", crate::redact::url_credentials(url)))?
            .to_vec())
    }
}

impl Manifest {
    pub fn from_url(fetcher: &dyn Fetcher, url: &str) -> anyhow::Result<Manifest> {
        let bytes = fetcher.get_bytes(url)?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("解析 manifest {} 失败", crate::redact::url_credentials(url)))
    }

    /// 返回 (`kernels` 表里记的版本, 该架构的 `artifacts` 条目)；`name` 传二进制名
    /// （`sing-box`），内部换成 `kernels` 的键（`sing_box`）与 `artifacts` 的键
    /// （`sing-box-linux-amd64`）。
    pub fn kernel_asset(&self, name: &str, arch: &str) -> anyhow::Result<(&str, &Asset)> {
        let key = artifact_key(name, arch)?;
        let version = self
            .kernels
            .get(&kernels_key(name))
            .ok_or_else(|| anyhow::anyhow!("manifest 的 kernels 表里没有 {name}"))?;
        let asset = self
            .artifacts
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("manifest 的 artifacts 表里没有 {key}"))?;
        Ok((version.as_str(), asset))
    }

    /// `bui` 自己的资产 = `artifacts["bui-linux-<amd64|arm64>"]`（按架构查，不是 target 三元组）。
    pub fn bui_asset(&self, arch: &str) -> anyhow::Result<&Asset> {
        let key = artifact_key("bui", arch)?;
        self.artifacts
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("manifest 的 artifacts 表里没有 {key}"))
    }
}

/// `kernels` 表的键：二进制名里的连字符换成下划线（`sing-box` → `sing_box`）。
pub fn kernels_key(name: &str) -> String {
    name.replace('-', "_")
}

/// `artifacts` 表的键：`<name>-linux-<amd64|arm64>`（架构不支持则报错）。
pub fn artifact_key(name: &str, arch: &str) -> anyhow::Result<String> {
    Ok(format!("{name}-linux-{}", asset_arch(arch)?))
}

/// `MANIFEST_URL_TEMPLATE` 代入版本号。
pub fn manifest_url_for_version(version: &str) -> String {
    MANIFEST_URL_TEMPLATE.replace("{version}", version.trim_start_matches('v'))
}

/// **唯一**一处决定 manifest 地址（纯函数，便于测试）：
/// `cli_override` > `version`（套模板）> `env`（`$BUI_MANIFEST_URL`）> [`MANIFEST_URL`]。
pub fn resolve_manifest_url(
    cli_override: Option<&str>,
    version: Option<&str>,
    env: Option<&str>,
) -> String {
    match (cli_override, version, env) {
        (Some(u), _, _) if !u.is_empty() => u.to_string(),
        (_, Some(v), _) if !v.is_empty() => manifest_url_for_version(v),
        (_, _, Some(u)) if !u.is_empty() => u.to_string(),
        _ => MANIFEST_URL.to_string(),
    }
}

/// 读进程环境后调 [`resolve_manifest_url`]；Task 15/16/17 一律用它，不再各自拼 URL。
pub fn manifest_url(cli_override: Option<&str>, version: Option<&str>) -> String {
    let env = std::env::var(MANIFEST_URL_ENV).ok();
    resolve_manifest_url(cli_override, version, env.as_deref())
}

/// 从内核的 `version` 输出里反解版本号（每个内核的格式都不一样，注释里是本机实测的第一行）。
pub fn parse_version(kind: &str, stdout: &str) -> Option<String> {
    let strip = |s: &str| s.trim().trim_start_matches('v').to_string();
    match kind {
        // "sing-box version 1.13.19"
        "sing-box" => stdout.lines().next()?.split_whitespace().nth(2).map(strip),
        // "Xray 26.3.27 (Xray, Penetrates Everything.) …"
        "xray" => stdout.lines().next()?.split_whitespace().nth(1).map(strip),
        // banner 多行，取 "Version:\tv2.12.2"
        "hysteria" => stdout
            .lines()
            .find(|l| l.trim_start().starts_with("Version:"))
            .and_then(|l| l.split(':').nth(1))
            .map(strip),
        // "v2.10.2 h1:…"
        "caddy" => stdout.lines().next()?.split_whitespace().next().map(strip),
        _ => None,
    }
    .filter(|v| !v.is_empty())
}

/// 探测已装二进制的版本；文件不存在就返回 `None`（diff 会判成「要装」）。
pub fn installed_version(host: &dyn Host, bin_dir: &Path, kind: &str) -> Option<String> {
    let bin = bin_dir.join(kind);
    // 文件不存在（或读不出来）就直接返回 None，不去 run 一个不存在的程序。
    // 计划里写的是 `if … is_none() { return None }`，clippy 的 `question_mark` 要求用 `?`。
    host.read_file(&bin).ok().flatten()?;
    let out = host.run(&bin.display().to_string(), &["version"]).ok()?;
    parse_version(
        kind,
        if out.stdout.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        },
    )
}

/// 四个内核的已装版本表（缺的二进制不进表）。
pub fn installed_versions(host: &dyn Host, bin_dir: &Path) -> BTreeMap<String, String> {
    KERNELS
        .iter()
        .filter_map(|k| installed_version(host, bin_dir, k).map(|v| (k.to_string(), v)))
        .collect()
}

/// [`KERNELS`] 里「`bin/` 下没有该二进制、或有但探不出版本」的那些（顺序同 `KERNELS`）。
/// 探不出版本与不存在同等对待：真机上 203/EXEC 的单元就是「文件在、但根本跑不起来」。
pub fn missing_kernels(host: &dyn Host, bin_dir: &Path) -> Vec<&'static str> {
    let have = installed_versions(host, bin_dir);
    KERNELS
        .iter()
        .copied()
        .filter(|k| !have.contains_key(*k))
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// `x86_64`/`amd64` → `amd64`，`aarch64`/`arm64` → `arm64`，其余报错。
pub fn asset_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => anyhow::bail!("不支持的架构：{other}（只支持 x86_64 / aarch64）"),
    }
}

/// 下载 → sha256 → `host.write_file(dest, bytes, 0o755)`。
pub struct KernelInstaller<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub host: &'a dyn Host,
}

impl BinaryInstaller for KernelInstaller<'_> {
    fn install(
        &self,
        name: &str,
        version: &str,
        sha256: &str,
        url: &str,
        dest: &Path,
    ) -> anyhow::Result<()> {
        let bytes = self.fetcher.get_bytes(url)?;
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(sha256) {
            anyhow::bail!("{name} {version} 的 sha256 不匹配：期望 {sha256}，实际 {got}");
        }
        self.host.write_file(dest, &bytes, 0o755)?;
        tracing::info!(kernel = name, version, "内核二进制已更新");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;
    use std::sync::Mutex;

    // 本机实测输出：sing-box 1.13.19 / xray 26.3.27 / hysteria 2.12.2 / caddy 2.10.2
    const SINGBOX: &str = "sing-box version 1.13.19\n\nEnvironment: go1.25.12 linux/amd64\nTags: with_gvisor,with_quic\n";
    const XRAY: &str = "Xray 26.3.27 (Xray, Penetrates Everything.) d2758a0 (go1.26.1 linux/amd64)\nA unified platform for anti-censorship.\n";
    const HYSTERIA: &str = "\n░█░█\n\na powerful, lightning fast and censorship resistant proxy\n\nVersion:\tv2.12.2\nBuildDate:\t2026-08-23T00:39:00Z\nBuildType:\trelease\n";
    const CADDY: &str = "v2.10.2 h1:g/gTYjGMD0dec+UgMw8SnfmJ3I9+M2TdvoRL/Ovu6U8=\n";

    #[test]
    fn parses_all_four_real_version_outputs() {
        assert_eq!(
            parse_version("sing-box", SINGBOX).as_deref(),
            Some("1.13.19")
        );
        assert_eq!(parse_version("xray", XRAY).as_deref(), Some("26.3.27"));
        assert_eq!(
            parse_version("hysteria", HYSTERIA).as_deref(),
            Some("2.12.2")
        );
        assert_eq!(parse_version("caddy", CADDY).as_deref(), Some("2.10.2"));
        assert_eq!(parse_version("xray", ""), None);
        assert_eq!(parse_version("unknown-kernel", "whatever 1.2.3"), None);
    }

    #[test]
    fn installed_versions_skips_missing_binaries() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files
                .insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.scripted
                .push(("/opt/b-ui/bin/xray version".into(), CmdOut::success(XRAY)));
            i.scripted.push((
                "/opt/b-ui/bin/sing-box version".into(),
                CmdOut::success(SINGBOX),
            ));
        });
        let v = installed_versions(&h, std::path::Path::new("/opt/b-ui/bin"));
        assert_eq!(v.get("xray").map(String::as_str), Some("26.3.27"));
        assert_eq!(v.get("sing-box").map(String::as_str), Some("1.13.19"));
        assert_eq!(
            v.get("hysteria"),
            None,
            "文件不存在就不进表，diff 会判成要装"
        );
        // 2026-09-12 真机：bin/ 里只有 bui，install 却照着往下跑把 v3 拆了。
        // 「缺哪些」必须能一口气问出来，且探不出版本与不存在同等对待。
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/caddy".into(), (b"garbage".to_vec(), 0o755));
            // 203/EXEC 的形态：文件在，但根本跑不出版本
            i.scripted.push((
                "/opt/b-ui/bin/caddy version".into(),
                CmdOut::failure(127, ""),
            ));
        });
        assert_eq!(
            missing_kernels(&h, std::path::Path::new("/opt/b-ui/bin")),
            vec!["hysteria", "caddy"],
        );
    }

    struct FakeFetcher {
        files: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    // 总纲 C4 的形状（只放 amd64，用来同时测「架构缺资产」这一支）
    const MANIFEST_JSON: &str = r#"{
      "version": "4.0.1",
      "kernels": { "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2",
                   "client_sing_box": "1.13.19" },
      "artifacts": {
        "bui-linux-amd64":      { "url": "https://x/bui-amd64", "sha256": "PLACE_SUM" },
        "hysteria-linux-amd64": { "url": "https://x/hysteria", "sha256": "PLACE_SUM" },
        "xray-linux-amd64":     { "url": "https://x/xray", "sha256": "aa" },
        "sing-box-linux-amd64": { "url": "https://x/sb", "sha256": "bb" },
        "caddy-linux-amd64":    { "url": "https://x/caddy", "sha256": "cc" }
      }
    }"#;

    fn fixture(payload: &[u8]) -> (Manifest, FakeFetcher) {
        let json = MANIFEST_JSON.replace("PLACE_SUM", &sha256_hex(payload));
        let m: Manifest = serde_json::from_str(&json).unwrap();
        let f = FakeFetcher {
            files: Mutex::new(vec![
                ("https://x/manifest.json".into(), json.into_bytes()),
                ("https://x/hysteria".into(), payload.to_vec()),
                ("https://x/bui-amd64".into(), payload.to_vec()),
                ("https://x/xray".into(), payload.to_vec()),
            ]),
        };
        (m, f)
    }

    #[test]
    fn manifest_round_trip_and_asset_lookup() {
        let (m, f) = fixture(b"binary-bytes");
        assert_eq!(
            Manifest::from_url(&f, "https://x/manifest.json").unwrap(),
            m
        );
        let (ver, asset) = m.kernel_asset("hysteria", "x86_64").unwrap();
        assert_eq!(ver, "2.12.2");
        assert_eq!(asset.url, "https://x/hysteria");
        // 连字符的二进制名 → 下划线的版本表键 + 连字符的资产键
        let (ver, asset) = m.kernel_asset("sing-box", "x86_64").unwrap();
        assert_eq!(ver, "1.13.19");
        assert_eq!(asset.url, "https://x/sb");
        assert_eq!(kernels_key("sing-box"), "sing_box");
        assert_eq!(
            artifact_key("sing-box", "aarch64").unwrap(),
            "sing-box-linux-arm64"
        );
        assert!(m.kernel_asset("hysteria", "riscv64").is_err(), "架构不支持");
        assert!(
            m.kernel_asset("hysteria", "aarch64").is_err(),
            "本 fixture 没有 arm64 资产"
        );
        assert!(
            m.kernel_asset("shadowsocks", "x86_64").is_err(),
            "版本表里没有这个内核"
        );
        assert_eq!(m.bui_asset("x86_64").unwrap().url, "https://x/bui-amd64");
        assert!(m.bui_asset("armv7l").is_err());
    }

    #[test]
    fn manifest_url_resolution_follows_the_c4_order() {
        // C4：--manifest-url > --version（模板）> $BUI_MANIFEST_URL > 内置 latest。
        // 测纯函数，不动进程环境（`std::env::set_var` 会影响并行跑的其它测试）。
        assert_eq!(resolve_manifest_url(None, None, None), MANIFEST_URL);
        assert_eq!(
            resolve_manifest_url(None, None, Some("http://127.0.0.1:8000/manifest.json")),
            "http://127.0.0.1:8000/manifest.json"
        );
        assert_eq!(
            resolve_manifest_url(
                None,
                Some("4.0.1"),
                Some("http://127.0.0.1:8000/manifest.json")
            ),
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/manifest.json",
            "--version 优先于环境变量，且必须是 releases/download/<tag>/ 的形状"
        );
        assert_eq!(
            resolve_manifest_url(
                Some("/tmp/dist/manifest.json"),
                Some("4.0.1"),
                Some("http://x/m.json")
            ),
            "/tmp/dist/manifest.json",
            "--manifest-url 最高优先"
        );
        assert_eq!(
            manifest_url_for_version("4.0.1"),
            resolve_manifest_url(None, Some("4.0.1"), None)
        );
    }

    #[test]
    fn the_http_fetcher_also_reads_local_paths_and_file_urls() {
        // M5 演练用：`--manifest-url /tmp/dist/manifest.json` 或 `file:///tmp/dist/manifest.json`
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("manifest.json");
        std::fs::write(&p, b"{\"version\":\"4.0.1\"}").unwrap();
        let f = HttpFetcher::new();
        assert_eq!(
            f.get_bytes(p.to_str().unwrap()).unwrap(),
            b"{\"version\":\"4.0.1\"}"
        );
        assert_eq!(
            f.get_bytes(&format!("file://{}", p.display())).unwrap(),
            b"{\"version\":\"4.0.1\"}"
        );
        let m = Manifest::from_url(&f, p.to_str().unwrap()).unwrap();
        assert_eq!(m.version, "4.0.1");
        assert_eq!(m.min_upgrade_from, None, "可选字段缺失不报错");
        assert!(f
            .get_bytes(&d.path().join("nope.json").display().to_string())
            .is_err());
    }

    #[test]
    fn asset_arch_maps_only_supported_arches() {
        assert_eq!(asset_arch("x86_64").unwrap(), "amd64");
        assert_eq!(asset_arch("aarch64").unwrap(), "arm64");
        assert!(asset_arch("armv7l").is_err());
    }

    #[test]
    fn install_writes_0755_after_verifying_sha256() {
        let payload = b"binary-bytes";
        let (m, f) = fixture(payload);
        let h = FakeHost::new();
        let (ver, asset) = m.kernel_asset("hysteria", "x86_64").unwrap();
        KernelInstaller {
            fetcher: &f,
            host: &h,
        }
        .install(
            "hysteria",
            ver,
            &asset.sha256,
            &asset.url,
            std::path::Path::new("/opt/b-ui/bin/hysteria"),
        )
        .unwrap();
        assert_eq!(h.text("/opt/b-ui/bin/hysteria").unwrap(), "binary-bytes");
        assert_eq!(h.mode("/opt/b-ui/bin/hysteria"), Some(0o755));
        assert_eq!(h.ops(), vec!["write:/opt/b-ui/bin/hysteria:755"]);
    }

    #[test]
    fn install_refuses_on_sha256_mismatch_and_writes_nothing() {
        let (m, f) = fixture(b"binary-bytes");
        let h = FakeHost::new();
        // xray 的 sha256 写的是 "aa"，与内容不符
        let (ver, asset) = m.kernel_asset("xray", "x86_64").unwrap();
        let err = KernelInstaller {
            fetcher: &f,
            host: &h,
        }
        .install(
            "xray",
            ver,
            &asset.sha256,
            &asset.url,
            std::path::Path::new("/opt/b-ui/bin/xray"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("sha256"), "{err}");
        assert!(h.ops().is_empty(), "校验不过不写盘");
    }

    #[test]
    fn install_propagates_download_failure() {
        let (_m, f) = fixture(b"x");
        let h = FakeHost::new();
        assert!(KernelInstaller {
            fetcher: &f,
            host: &h
        }
        .install(
            "caddy",
            "2.10.2",
            "cc",
            "https://x/missing",
            std::path::Path::new("/opt/b-ui/bin/caddy")
        )
        .is_err());
        assert!(h.ops().is_empty());
    }

    #[test]
    fn download_errors_never_leak_url_credentials() {
        // Global Constraints「凭据不进日志」：reqwest 的 `Error` Display 会追加 ` for url (<完整
        // URL>)`，而 URL 的序列化带 userinfo，`$BUI_MANIFEST_URL` / `--manifest-url` 明写允许
        // `https://user:token@host/manifest.json`。回环 1 端口必定拒连（不联网、无需 mock）。
        const URL: &str = "http://user:s3cr3t-token@127.0.0.1:1/manifest.json";
        let f = HttpFetcher::new();
        // `{:#?}` 把 anyhow 的 context 链连带 source 一路摊开，只查最外层会漏。
        let chain = format!("{:#?}", f.get_bytes(URL).unwrap_err());
        assert!(!chain.contains("s3cr3t-token"), "{chain}");
        assert!(!chain.contains("user:"), "{chain}");
        assert!(chain.contains("***:***@127.0.0.1:1"), "{chain}");

        // manifest 解析失败的错误串同样要带脱敏来源（Task 15「拉不到只 warn」靠它定位）。
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("manifest.json");
        std::fs::write(&p, b"not json").unwrap();
        let bad = format!(
            "{:#}",
            Manifest::from_url(&f, p.to_str().unwrap()).unwrap_err()
        );
        assert!(bad.contains("解析"), "{bad}");
        assert!(bad.contains("manifest.json"), "{bad}");
    }
}
