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
//! 「什么都没指定」这一支还有一步回退：GitHub 的 `releases/latest` 不解析预发布，所以 rc 阶段
//! `latest/download/manifest.json` 必然 404 —— [`fetch_manifest_with`] 在**本机自己就是预发布**
//! 时改跟 releases 列表里最新的 `v*-rcN`（裁决记录「发布：预发布与首推（2026-09-12）」）。
//! 「本机是预发布」只认 **tag**（[`on_prerelease_channel`]）：编译进二进制的发布 tag
//! （[`BUILD_TAG`]）或 manifest 缓存里记的 [`Manifest::tag`]。**版本号不是信号**——
//! `scripts/release/check-version.sh` 强制 workspace version 是纯 semver，`release.yml` 的
//! verify 又先把 `-rcN` 从 tag 上剥掉再校验，所以 `v4.0.0-rc1` 构建出来的二进制
//! `CARGO_PKG_VERSION` 与 manifest 的 `version` 都是 `4.0.0`，永远不含 `-rc`。

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

/// 发布时**编译进**二进制的真实 tag：`release.yml` 的 build 作业经 `BUI_BUILD_TAG` 传入
/// （`needs.verify.outputs.tag`，预发布是 `v4.0.0-rc1`；cross 走容器，靠
/// `CROSS_BUILD_ENV_PASSTHROUGH` 透传，build 作业有一步断言它真编进去了）。
/// 本地 `cargo build` 没有这个变量 → `None`，于是本地构建永远不在预发布通道上
/// （cargo 不跟踪 `option_env!` 的变量：本地改了它要 `touch` 本文件或 `cargo clean` 才会
/// 重编；CI 每次都是全新的 `target/`，不受影响）。
///
/// 这是「本机是预发布」**唯一**可靠的运行期信号：见模块头，版本号里永远不会有 `-rc`。
pub const BUILD_TAG: Option<&str> = option_env!("BUI_BUILD_TAG");

/// GitHub 的 releases 列表（**无凭据**，只取最近 10 条）：`releases/latest` 不解析预发布，
/// 预发布通道靠它找最新的 rc tag。仓库与 [`MANIFEST_URL`] 必须是同一个。
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=10";

/// 总纲 C4 的形状：`version` 是 bui 自己的版本，`kernels` 是版本表（键 = state `versions` 的
/// 字段名，下划线），`artifacts` 的键固定 `<name>-linux-<amd64|arm64>`（name = 二进制名，
/// 连字符）。未知字段一律忽略（serde 默认），P5 往 manifest 里加字段不会打死 P1。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// 发布这份 manifest 的 Release tag（`gen-manifest.sh --tag` 写入，预发布是
    /// `v4.0.0-rc1`）。C4 要求 `version` 是纯 semver，所以 rc 只能从这里看出来：装机/升级
    /// 把 manifest 落盘成缓存后，它就是「本机在预发布通道上」的信号（[`on_prerelease_channel`]）。
    /// 老 manifest 没有这个字段 → `None`（等于「不知道，按正式版算」）。
    #[serde(default)]
    pub tag: Option<String>,
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

/// 一次 404（或 `releases/latest` 还没有这个资产）。预发布回退必须把「没这个文件」与
/// 「网络断了 / JSON 坏了」分开，所以 404 单独成型，判断靠 `anyhow` 的 downcast
/// （[`is_not_found`]），不靠匹配错误串。
#[derive(Debug, thiserror::Error)]
#[error("下载 {0} 失败：HTTP 404")]
pub struct NotFound(pub String);

/// 错误链里有没有 [`NotFound`]。
pub fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.is::<NotFound>())
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
        // 404 单独成型：预发布通道回退要认出它（见 [`fetch_manifest_with`]）
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(NotFound(crate::redact::url_credentials(url)).into());
        }
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

/// 指定 **tag** 的 manifest：预发布回退拿到的是 tag（`v4.0.0-rc1`），不是纯版本号。
/// [`MANIFEST_URL_TEMPLATE`] 里的 `v{version}` 就是 tag 那一段。
pub fn manifest_url_for_tag(tag: &str) -> String {
    MANIFEST_URL_TEMPLATE.replace("v{version}", tag)
}

/// `MANIFEST_URL_TEMPLATE` 代入版本号（tag = `v<version>`）。
pub fn manifest_url_for_version(version: &str) -> String {
    manifest_url_for_tag(&format!("v{}", version.trim_start_matches('v')))
}

/// tag 是不是 `^v\d+\.\d+\.\d+-rc\d+$`（整串锚定；不引 regex crate，手写）。
pub fn is_rc_tag(tag: &str) -> bool {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    let Some((core, rc)) = tag.strip_prefix('v').and_then(|r| r.split_once("-rc")) else {
        return false;
    };
    let mut seg = core.split('.');
    let three = [seg.next(), seg.next(), seg.next()];
    seg.next().is_none() && three.iter().all(|s| s.is_some_and(digits)) && digits(rc)
}

/// 本机是不是在预发布通道上：编译进来的发布 tag 是 rc，或 manifest 缓存里记的 tag 是 rc。
/// 两个入参都只接受 **tag**（`v<x.y.z>[-rcN]`），不接受版本号——版本号认不出 rc（见模块头）。
pub fn on_prerelease_channel(build_tag: Option<&str>, cached_tag: Option<&str>) -> bool {
    build_tag.is_some_and(is_rc_tag) || cached_tag.is_some_and(is_rc_tag)
}

/// GitHub releases 列表里的一条（只取用得上的两个字段，其余忽略）。
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
}

/// releases 列表里最新的预发布 rc tag：列表按创建时间倒序，取第一个 `prerelease=true`
/// 且 tag 形如 `v<x.y.z>-rcN` 的。没有就 `None`（调用方把原来的 404 上抛）。
pub fn latest_rc_tag(fetcher: &dyn Fetcher) -> anyhow::Result<Option<String>> {
    let bytes = fetcher.get_bytes(RELEASES_API_URL)?;
    let list: Vec<GhRelease> =
        serde_json::from_slice(&bytes).context("解析 GitHub releases 列表失败")?;
    Ok(list
        .into_iter()
        .find(|r| r.prerelease && is_rc_tag(&r.tag_name))
        .map(|r| r.tag_name))
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

/// C4 那三个覆盖是否**都没给**（空串按没给算，与 [`resolve_manifest_url`] 同一口径）。
/// 预发布回退只在这种「跟着 latest 走」的情况下才允许发生。
fn nothing_specified(cli_override: Option<&str>, version: Option<&str>, env: Option<&str>) -> bool {
    [cli_override, version, env]
        .iter()
        .all(|o| !o.is_some_and(|s| !s.is_empty()))
}

/// 按 C4 的顺序算地址并把 manifest 拉回来，返回 **(实际用的 url, manifest)**。
///
/// 比 [`resolve_manifest_url`] 多的只有一件事：**未指定版本**（没有 `--manifest-url`、没有
/// `--version`、没有 `$BUI_MANIFEST_URL`，也就是跟着 `latest` 走）时若拿到 404，且本机自己
/// 就在预发布通道上，就改跟 GitHub releases 列表里最新的 `v<x.y.z>-rcN`
/// （裁决记录「发布：预发布与首推（2026-09-12）」：rc1 发布时仓库里还没有正式版，
/// `releases/latest/download/manifest.json` 必然 404）。
///
/// 两条边界：
/// - **指定了地址或版本就只认那一个**，404 原样上抛——不许把人从指定源拽回预发布通道。
///   判据是「三个覆盖都没给」（[`nothing_specified`]），不是「算出来的 url 恰好等于
///   [`MANIFEST_URL`]」：后者会让把 `$BUI_MANIFEST_URL` 设成内置 latest 同一串的人也被回退；
/// - **正式版不回退**（只跟 latest），否则一次 rc 就能把所有正式版机器拽上预发布轨道。
///
/// 「本机是预发布」的信号见 [`on_prerelease_channel`]：`build_tag` 传 [`BUILD_TAG`]，
/// `cached_tag` 传 manifest 缓存里的 [`Manifest::tag`]。
pub fn fetch_manifest_with(
    fetcher: &dyn Fetcher,
    cli_override: Option<&str>,
    version: Option<&str>,
    env: Option<&str>,
    build_tag: Option<&str>,
    cached_tag: Option<&str>,
) -> anyhow::Result<(String, Manifest)> {
    let url = resolve_manifest_url(cli_override, version, env);
    match Manifest::from_url(fetcher, &url) {
        Ok(m) => Ok((url, m)),
        Err(e) if nothing_specified(cli_override, version, env) && is_not_found(&e) => {
            if !on_prerelease_channel(build_tag, cached_tag) {
                return Err(e);
            }
            let Some(tag) = latest_rc_tag(fetcher)? else {
                return Err(e);
            };
            let rc_url = manifest_url_for_tag(&tag);
            tracing::info!(tag = %tag, "latest 里没有 manifest（GitHub 不把预发布算 latest），改跟预发布通道");
            let m = Manifest::from_url(fetcher, &rc_url)?;
            Ok((rc_url, m))
        }
        Err(e) => Err(e),
    }
}

/// 读进程环境与编译期 tag 后调 [`fetch_manifest_with`]；`cached_tag` 传 manifest 缓存
/// （`<base>/manifest.json`）里记的 [`Manifest::tag`]，没有缓存或老 manifest 就传 `None`。
pub fn fetch_manifest(
    fetcher: &dyn Fetcher,
    cli_override: Option<&str>,
    version: Option<&str>,
    cached_tag: Option<&str>,
) -> anyhow::Result<(String, Manifest)> {
    let env = std::env::var(MANIFEST_URL_ENV).ok();
    fetch_manifest_with(
        fetcher,
        cli_override,
        version,
        env.as_deref(),
        BUILD_TAG,
        cached_tag,
    )
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

/// manifest 里的 `bui` 与盘上 `bin/bui` 是不是**两个不同的构建**。
///
/// 版本号不是充分判据：rc 通道下 `v4.0.0-rc1` / `rc2` / 正式版的 Cargo 版本号都是同一个
/// `4.0.0`（release.yml 的 check-version 把 `-rcN` 去掉后才比对），只按版本号判断的话
/// 「同版本重建」永远不会被升级——2026-09-12 bwg-rick 上 `bui upgrade` 走不到新构建就是这个。
/// 所以版本相同时再比一次 sha256：manifest 里 `bui-linux-<arch>` 的 vs 盘上 `bin/bui` 的。
///
/// 盘上二进制读不到（还没装 / 读失败）按「要升级」处理——本来就该给它装一份；manifest 缺该
/// 架构资产同样按要升级返回，真正的报错留给 `plan_upgrade`（它在出计划前就会 bail）。
pub fn bui_build_differs(
    host: &dyn Host,
    bin_dir: &Path,
    m: &Manifest,
    current_version: &str,
    arch: &str,
) -> bool {
    if m.version != current_version {
        return true;
    }
    let Ok(asset) = m.bui_asset(arch) else {
        return true;
    };
    match host.read_file(&bin_dir.join("bui")).ok().flatten() {
        Some(bytes) => !sha256_hex(&bytes).eq_ignore_ascii_case(&asset.sha256),
        None => true,
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
        seen: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        fn new(files: Vec<(&str, &str)>) -> Self {
            Self {
                files: Mutex::new(
                    files
                        .into_iter()
                        .map(|(u, b)| (u.to_string(), b.as_bytes().to_vec()))
                        .collect(),
                ),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.seen.lock().unwrap().push(url.to_string());
            self.files
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                // 真 HttpFetcher 的 404 是 `NotFound`，预发布回退靠它判断：假的必须一致
                .ok_or_else(|| NotFound(url.to_string()).into())
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
            seen: Mutex::new(Vec::new()),
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

    /// rc 通道的回归：版本号相同、sha256 不同也算「有新构建」。
    #[test]
    fn bui_build_differs_falls_back_to_sha_when_the_version_is_unchanged() {
        let (m, _) = fixture(b"BUI-rc2"); // manifest 版本 4.0.1，bui 资产 = sha256("BUI-rc2")
        let bin = Path::new("/opt/b-ui/bin");
        let h = FakeHost::new();
        h.write_file(&bin.join("bui"), b"BUI-rc1", 0o755).unwrap();
        assert!(
            bui_build_differs(&h, bin, &m, "4.0.0", "x86_64"),
            "版本不同：一眼就要升"
        );
        assert!(
            bui_build_differs(&h, bin, &m, "4.0.1", "x86_64"),
            "同版本但盘上是另一份构建（rc1 vs rc2）：也要升"
        );
        h.write_file(&bin.join("bui"), b"BUI-rc2", 0o755).unwrap();
        assert!(
            !bui_build_differs(&h, bin, &m, "4.0.1", "x86_64"),
            "同版本同 sha 才是已最新"
        );
        assert!(
            bui_build_differs(&FakeHost::new(), bin, &m, "4.0.1", "x86_64"),
            "盘上读不到 bin/bui：按要升级处理"
        );
        assert!(
            bui_build_differs(&h, bin, &m, "4.0.1", "aarch64"),
            "manifest 没有该架构的 bui 资产：交给 plan_upgrade 去报错，这里按要升级算"
        );
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
        assert_eq!(
            manifest_url_for_tag("v4.0.0-rc1"),
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc1/manifest.json",
            "预发布回退拿到的是 tag，不是纯版本号"
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
        assert_eq!(m.tag, None, "老 manifest 没有 tag：按「不知道」算");
        assert!(f
            .get_bytes(&d.path().join("nope.json").display().to_string())
            .is_err());
    }

    /// 预发布通道回退用的 fixture（裁决记录「发布：预发布与首推（2026-09-12）」）。
    /// **这是 rc 机器的真实形状**：C4 要求 `version` 是纯 semver，`check-version.sh` 与
    /// `release.yml` 的 verify 也一起把 `-rcN` 从版本号上剥掉，所以 rc 只写在 `tag` 上。
    const RC_MANIFEST: &str =
        r#"{"version":"4.0.0","tag":"v4.0.0-rc2","kernels":{},"artifacts":{}}"#;
    const PLAIN_MANIFEST: &str =
        r#"{"version":"4.0.0","tag":"v4.0.0","kernels":{},"artifacts":{}}"#;
    /// GitHub 的 releases 列表按创建时间倒序。第 1 条是 rc 形状但 `prerelease=false`
    /// （必须跳过），第 2 条是 prerelease 但 tag 不匹配（必须跳过）。
    const RELEASES_JSON: &str = r#"[
      {"tag_name": "v4.0.1-rc1", "prerelease": false},
      {"tag_name": "nightly",    "prerelease": true},
      {"tag_name": "v4.0.0-rc2", "prerelease": true},
      {"tag_name": "v4.0.0-rc1", "prerelease": true},
      {"tag_name": "v3.9.9",     "prerelease": false}
    ]"#;

    #[test]
    fn rc_tag_matching_is_anchored() {
        assert!(is_rc_tag("v4.0.0-rc1"));
        assert!(is_rc_tag("v10.2.30-rc12"));
        assert!(!is_rc_tag("v4.0.0"), "正式版不是 rc");
        assert!(!is_rc_tag("4.0.0-rc1"), "少 v 前缀");
        assert!(!is_rc_tag("v4.0-rc1"), "少一段");
        assert!(!is_rc_tag("v4.0.0-rc"), "rc 后面必须有数字");
        assert!(!is_rc_tag("v4.0.0-rc1x"), "整串锚定：后面不许有尾巴");
        assert!(!is_rc_tag("v4.0.0-rc1-rc2"), "整串锚定：不许接第二个 rc");
    }

    #[test]
    fn the_prerelease_signal_is_the_tag_never_the_version() {
        // 版本号不可能带 -rc：`check-version.sh` 只放纯 semver 过，`release.yml` 的 verify
        // 又把 `-rcN` 从 tag 上剥掉才校验。谁要是把这条当信号，rc 机器就永远回退不了。
        assert!(
            !env!("CARGO_PKG_VERSION").contains("-rc"),
            "workspace version 必须是纯 semver（check-version.sh 强制），所以它不能当预发布信号"
        );
        // 信号一：编译进二进制的发布 tag（rc1 装机后没有任何缓存，只能靠它）
        assert!(on_prerelease_channel(Some("v4.0.0-rc1"), None));
        // 信号二：manifest 缓存里记的 tag（二进制没带 tag 时的兜底）
        assert!(on_prerelease_channel(None, Some("v4.0.0-rc1")));
        // 正式版的两种形状与「什么都不知道」都不算预发布
        assert!(!on_prerelease_channel(Some("v4.0.0"), Some("v4.0.0")));
        assert!(!on_prerelease_channel(None, None));
        // 版本号（没有 v 前缀）即便真带了 -rc 也不是 tag，不认
        assert!(!on_prerelease_channel(Some("4.0.0-rc1"), Some("4.0.0-rc1")));
    }

    #[test]
    fn unspecified_version_follows_latest_when_it_exists() {
        let f = FakeFetcher::new(vec![(MANIFEST_URL, RC_MANIFEST)]);
        let (url, m) = fetch_manifest_with(&f, None, None, None, Some("v4.0.0-rc1"), None).unwrap();
        assert_eq!(url, MANIFEST_URL);
        assert_eq!(m.version, "4.0.0");
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL],
            "latest 拿到了就不该再问 GitHub 的 releases 列表"
        );
        // 正式版机器同样只跟 latest（转正之后 rc 机器走的就是这一支）
        let g = FakeFetcher::new(vec![(MANIFEST_URL, PLAIN_MANIFEST)]);
        let (url, m) = fetch_manifest_with(&g, None, None, None, Some("v4.0.0"), None).unwrap();
        assert_eq!(url, MANIFEST_URL);
        assert_eq!(m.tag.as_deref(), Some("v4.0.0"));
    }

    #[test]
    fn a_prerelease_build_falls_back_to_the_newest_rc_when_latest_is_404() {
        // GitHub 的 releases/latest 不解析预发布：rc1 发布时 latest/download/… 必然 404
        let rc_url = manifest_url_for_tag("v4.0.0-rc2");
        let f = FakeFetcher::new(vec![
            (RELEASES_API_URL, RELEASES_JSON),
            (&rc_url, RC_MANIFEST),
        ]);
        // 信号一：二进制是 rc1 构建的（一台刚用一行命令装好的 rc1 机器：没有 manifest 缓存）
        let (url, m) = fetch_manifest_with(&f, None, None, None, Some("v4.0.0-rc1"), None).unwrap();
        assert_eq!(url, rc_url, "取列表里第一个 prerelease=true 且 tag 匹配的");
        assert_eq!(m.version, "4.0.0");
        assert_eq!(
            m.tag.as_deref(),
            Some("v4.0.0-rc2"),
            "缓存下来就是下一轮的信号"
        );
        assert_eq!(
            f.seen(),
            vec![
                MANIFEST_URL.to_string(),
                RELEASES_API_URL.to_string(),
                rc_url.clone()
            ],
            "顺序必须是 latest → releases 列表 → rc 的 manifest"
        );
        // 信号二：二进制没带 tag（本地构建 / 手搓），但 manifest 缓存里记的 tag 是 rc
        let (url, _) = fetch_manifest_with(&f, None, None, None, None, Some("v4.0.0-rc1")).unwrap();
        assert_eq!(url, rc_url);
        // 指定了地址或版本就只认那一个，404 原样上抛（不许把人从指定源拽回预发布通道）
        let e = fetch_manifest_with(
            &f,
            None,
            None,
            Some("https://x/nope.json"),
            Some("v4.0.0-rc1"),
            None,
        )
        .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        let e = fetch_manifest_with(&f, None, Some("4.9.9"), None, Some("v4.0.0-rc1"), None)
            .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        let e = fetch_manifest_with(
            &f,
            Some("/tmp/nope.json"),
            None,
            None,
            Some("v4.0.0-rc1"),
            None,
        )
        .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
    }

    #[test]
    fn a_formal_build_never_falls_back_to_a_prerelease() {
        let rc_url = manifest_url_for_tag("v4.0.0-rc2");
        let files = || {
            vec![
                (RELEASES_API_URL.to_string(), RELEASES_JSON.to_string()),
                (rc_url.clone(), RC_MANIFEST.to_string()),
            ]
        };
        let fake = |v: Vec<(String, String)>| {
            FakeFetcher::new(v.iter().map(|(u, b)| (u.as_str(), b.as_str())).collect())
        };
        // 正式版二进制 + 正式版缓存
        let f = fake(files());
        let e =
            fetch_manifest_with(&f, None, None, None, Some("v4.0.0"), Some("v4.0.0")).unwrap_err();
        assert!(is_not_found(&e), "404 原样上抛：{e:#}");
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL],
            "正式版只跟 latest：连 releases 列表都不许问，否则一次 rc 能把正式版机器拽走"
        );
        // 什么都不知道（本地构建、没缓存）也按正式版算
        let f = fake(files());
        let e = fetch_manifest_with(&f, None, None, None, None, None).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert_eq!(f.seen(), vec![MANIFEST_URL]);
        // 把 $BUI_MANIFEST_URL 设成与内置 latest 逐字相同的串也算「指定了」：只认那一个
        let f = fake(files());
        let e = fetch_manifest_with(&f, None, None, Some(MANIFEST_URL), Some("v4.0.0-rc1"), None)
            .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert_eq!(f.seen(), vec![MANIFEST_URL], "指定了就只认那一个");
        // 预发布通道但列表里没有 rc：仍然只是把 404 上抛
        let g = FakeFetcher::new(vec![(
            RELEASES_API_URL,
            r#"[{"tag_name":"v4.0.0","prerelease":false}]"#,
        )]);
        let e = fetch_manifest_with(&g, None, None, None, Some("v4.0.0-rc1"), None).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
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
