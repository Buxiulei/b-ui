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
//! manifest 地址只在 [`resolve_manifest_url`] / [`fetch_manifest_with`] 决定一次（总纲 C4「manifest
//! 来源与覆盖」）：`--manifest-url` > `--version`（套模板）> `$BUI_MANIFEST_URL` > 内置 latest。
//! 「什么都没指定」这一支还有一步回退：GitHub 的 `releases/latest` 不解析预发布，所以仓库里
//! 只有 rc 时 `latest/download/manifest.json` 必然 404 —— [`fetch_manifest_with`] 这时**无条件**
//! 去 releases 列表取最新的 `v<x.y.z>-rcN`（裁决记录「发布：预发布与首推（2026-09-12）」）。
//! 回退**不看本机**：`scripts/release/check-version.sh` 强制 workspace version 是纯 semver，
//! `release.yml` 的 verify 又先把 `-rcN` 从 tag 上剥掉再校验，所以 `v4.0.0-rc1` 构建出来的二进制
//! `CARGO_PKG_VERSION` 与 manifest 的 `version` 都是 `4.0.0`，永远不含 `-rc`——「本机是不是
//! 预发布」在运行期根本没有可靠信号，拿它当回退前提只会让 rc 机器永远停在 404 上
//! （2026-09-12 真机：bwg-rick 上 `bui upgrade` 就是这么直接退出的）。
//! 反过来 latest **存在**（仓库已有正式版）时绝不回退到 rc：正式版机器不跟 rc。
//!
//! **事故复盘（2026-09-15，两台 rc1 服务器 bwg-tizi / bwg-rick）**：03:33Z / 03:34Z 两台都用
//! `bui upgrade --manifest-url …/v4.0.1-rc1/manifest.json` 升到 rc1（缓存 manifest 的 `tag` =
//! `v4.0.1-rc1`，sing-box 1.14.1）。04:00:31Z 守护进程的每日自检照 [`MANIFEST_URL`]
//! （`releases/latest`，GitHub **不把预发布算 latest**）拿回 v4.0.0 的 manifest，**无条件**
//! 覆盖了缓存 `/opt/b-ui/manifest.json`（version 4.0.0 / tag v4.0.0 / client_sing_box 1.14.0），
//! 于是同一分钟里：日志打出「每日自检：有新版 bui」（4.0.0 比在跑的 4.0.1 还旧）、对账照新缓存
//! 把 relay 的 sing-box 从 1.14.1 **降回** 1.14.0（`bin/sing-box` 的 sha 变回 `.prev` 那一份）并
//! 重启 `b-ui-relay`、面板 `/packages/manifest.json`（客户端自更新的来源）跟着宣告 4.0.0。
//! `bui` 二进制本身没被换掉（自检从不换 bui），但任何 rc 上线一天之内内核就被拉回稳定版集合，
//! 而且此时谁跑一次无参 `bui upgrade` 就会真的装上 4.0.0。
//!
//! 修法：给「一份发布」定义可比大小的 rank [`release_rank`]（`(x, y, z, rc)`，稳定版 rc 位取
//! `u32::MAX`，所以同一个 x.y.z 下 stable > 任何 rcN），每日自检
//! （[`pick_selfcheck_manifest`]）只接受 rank **严格高于**本机当前 rank（[`installed_rank`]：
//! manifest 缓存，缺失时取运行中的 bui 版本 + 稳定版）的候选；rc 机器另把 [`latest_rc_tag`]
//! 那一份也纳入候选，稳定版机器绝不自动移到预发布。显式路径（`--version` / `--manifest-url` /
//! `$BUI_MANIFEST_URL` / `--rollback` / `install`）是操作者意图，不受守卫影响；只有无参
//! `bui upgrade` 会在解析到更旧的 manifest 时拒绝执行（`commands::upgrade::refuse_downgrade`，
//! 退出码 2）。

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
/// 本地 `cargo build` 没有这个变量 → `None`（cargo 不跟踪 `option_env!` 的变量：本地改了它要
/// `touch` 本文件或 `cargo clean` 才会重编；CI 每次都是全新的 `target/`，不受影响）。
///
/// **只用于诊断**：回退不再看它（见模块头），但排障时得知道盘上这份 bui 是哪个 tag 构建的，
/// 所以切换预发布通道那一行日志带上它。
pub const BUILD_TAG: Option<&str> = option_env!("BUI_BUILD_TAG");

/// GitHub 的 releases 列表（**无凭据**，一页取满 100 条——API 的上限）：`releases/latest` 不解析
/// 预发布，预发布通道靠它找最新的 rc tag。这个列表不按创建时间排序（见 [`latest_rc_tag`]），
/// 页小了新 rc 可能落在第一页之外。仓库与 [`MANIFEST_URL`] 必须是同一个。
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=100";

/// 总纲 C4 的形状：`version` 是 bui 自己的版本，`kernels` 是版本表（键 = state `versions` 的
/// 字段名，下划线），`artifacts` 的键固定 `<name>-linux-<amd64|arm64>`（name = 二进制名，
/// 连字符）。未知字段一律忽略（serde 默认），P5 往 manifest 里加字段不会打死 P1。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// 发布这份 manifest 的 Release tag（`gen-manifest.sh --tag` 写入，预发布是
    /// `v4.0.0-rc1`）。C4 要求 `version` 是纯 semver，所以「这一份是不是 rc」只能从这里看出来
    /// ——只用于人读缓存/排障，回退不依赖它。老 manifest 没有这个字段 → `None`。
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
    /// 整个响应体读进内存。**只许用于小文件**（`manifest.json`、GitHub 的 releases 列表）。
    fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>>;

    /// 边下边算：响应体按 64 KiB 一块同时喂 sha256 与 `sink`，返回算出的 sha256（小写十六
    /// 进制）。**二进制一律走这一条**，`sink` 用 [`crate::sys::Host::stage_file`] 开的写入槽，
    /// 于是「校验过再 rename」，sha 不匹配时目标路径一个字节不动。
    ///
    /// **为什么存在**：内核二进制单笔约 81 MB（sing-box，自建带 `with_v2ray_api` 的更大）、
    /// 四个合计约 190 MB，而 `b-ui.service` 的 `MemoryMax=200M` 是**硬上限**
    /// （`modules/units.rs`）。`get_bytes` + `sha256_hex(&bytes)` + `write_file(&bytes)` 在一次
    /// 安装/升级里就有约 81 MB 常驻加一份写盘副本 ⇒ systemd 把守护进程杀在下载半路，形状是
    /// 「升级时 OOM、装到一半停下」。**别改回一次性读入**（判据：`FakeFetcher` 记的流式流水
    /// 与 `FakeHost::staged`；真实现每次 `write` 的长度由 `HttpFetcher` 那两条用例钉住）。
    ///
    /// **失败必须有终态**：实现自己兜「整个响应体」的总预算与字节上限（[`DOWNLOAD_BUDGET`]
    /// / [`DOWNLOAD_MAX_BYTES`]）——`reqwest::blocking` 的请求 timeout 在流式循环里退化成
    /// 「每块一次 read 的预算」，涓流镜像会把守护进程里唯一那条对账消费者永久卡住。写
    /// `sink` 失败（磁盘满）与读响应体失败在类型上分开，见 [`SinkFailed`]。
    fn download_to(&self, url: &str, sink: &mut dyn std::io::Write) -> anyhow::Result<String>;
}

/// 流式 sha256 + 落盘的那口缓冲：与 [`crate::sys::Host::file_sha256`] 同一大小，峰值内存就是
/// 它，与文件多大无关。
const STREAM_BUF: usize = 64 * 1024;

/// **整个响应体**的总预算，值就是 `get_bytes` 时代的 300 秒（语义保持，不引入新的失败面）。
///
/// `Client::builder().timeout(300s)` 在一次性 `bytes()` 上就是「整个请求 300 秒」，但
/// `reqwest::blocking` 的 `impl Read for Response` 每次 read 都重新 `Instant::now() + timeout`
/// （`blocking::wait::timeout`）⇒ 一进流式循环，300 秒就从「整个响应体的总预算」退化成
/// 「每 64 KiB 一次 read 的预算」，涓流/半死的镜像再也不会报错。`read_timeout` 只有 async
/// builder 有，blocking 拿不到，所以这条总预算只能自己兜。
///
/// **为什么是硬要求**：守护进程里只有**一条**对账消费者（`serve.rs` 的 reconcile mpsc，跑在
/// `spawn_blocking` 里、外面没有任何 `tokio::time::timeout`）。它一次卡死之后，去抖触发
/// （面板改动）、10 分钟漂移巡检、每日自检就全在那条队列里排队，再不会有下一轮对账：配置
/// 不写、单元不重启、漂移不报、也不产 incident。失败路径必须有终态。
const DOWNLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(300);

/// 响应体字节上限：镜像坏掉或被替换后回一个无限/超大 body 时，别一路写到 ENOSPC（盘撑满
/// 期间 `state.json` 写入、证书续签、journald 会一起失败）。sha 校验只在下载**之后**才有
/// 机会拒绝，所以上限必须在循环里。最大的真资产约 81 MB（sing-box，自建带 `with_v2ray_api`
/// 的更大），512 MiB 是六倍余量；客户端那侧的同形上限是 `bui-c::net::download_via` 的
/// `max_bytes`。
const DOWNLOAD_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// [`stream_sha256`] 的三种失败：首层文案与归类各不相同，所以在类型上就分开——读来源归
/// 「下载失败」、写落盘槽归「写盘失败」（[`SinkFailed`]）、主动中断是预算/上限。
enum StreamErr {
    /// 读来源（HTTP 响应体或本地文件）失败。
    Read(std::io::Error),
    /// 写落盘槽失败（ENOSPC / EIO / 只读文件系统）。
    Write(std::io::Error),
    /// 自己中断：总预算到点或超过字节上限。串里**绝不含 URL**（调用方补脱敏地址）。
    Aborted(String),
}

/// 下载期的**写盘**失败。装机/升级要连写四个内核（约 190 MB），磁盘满是最现实的失败，把
/// 操作者指向网络是误导，所以与「下载失败」分开归类，判断靠 `anyhow` 的 downcast
/// （[`sink_failure`]），不靠匹配错误串。文案里只有 io 错误，没有下载地址。
#[derive(Debug, thiserror::Error)]
#[error("写临时文件失败：{0}")]
pub struct SinkFailed(pub std::io::Error);

/// 错误链里的 [`SinkFailed`]：有就是写盘炸的，不是下载炸的。
pub fn sink_failure(err: &anyhow::Error) -> Option<&std::io::Error> {
    err.chain()
        .find_map(|c| c.downcast_ref::<SinkFailed>())
        .map(|w| &w.0)
}

/// `reader` → `sink`，边搬边算 sha256（小写十六进制）。峰值 = [`STREAM_BUF`]。
///
/// `budget` 是整个搬运的总时长预算、`max_bytes` 是字节上限，两条都在循环里查（理由见
/// [`DOWNLOAD_BUDGET`] 与 [`DOWNLOAD_MAX_BYTES`]）：**没有终态的失败路径比慢的失败更坏**。
fn stream_sha256(
    reader: &mut dyn std::io::Read,
    sink: &mut dyn std::io::Write,
    budget: std::time::Duration,
    max_bytes: u64,
) -> Result<String, StreamErr> {
    use sha2::{Digest as _, Sha256};
    let start = std::time::Instant::now();
    let mut buf = vec![0u8; STREAM_BUF];
    let mut h = Sha256::new();
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(StreamErr::Read)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            return Err(StreamErr::Aborted(format!(
                "超过字节上限（{max_bytes} 字节）"
            )));
        }
        h.update(&buf[..n]);
        sink.write_all(&buf[..n]).map_err(StreamErr::Write)?;
        if start.elapsed() > budget {
            return Err(StreamErr::Aborted(format!(
                "总预算 {budget:?} 到点还没搬完"
            )));
        }
    }
    sink.flush().map_err(StreamErr::Write)?;
    Ok(hex::encode(h.finalize()))
}

/// 读响应体时的 io 错误：里面包着 reqwest 的错误就照样先 `without_url()` 剥掉 URL（模块头
/// 的铁律：**不许**把「reqwest 0.12 会把 userinfo 挪进 Authorization 头」当成「凭据不进
/// 日志」的唯一依靠——`blocking::Response` 是 `.map_err(Error::into_io)` 把 `reqwest::Error`
/// 包进 `io::Error` 的，anyhow 的 `{:#?}` 会把它连 source 一起摊开）；不是 reqwest 的错误
/// 就只留错误种类（`ErrorKind` 的描述里没有 URL）。口径与 `bui-c::net::read_error` 一致。
fn body_error(url: &str, e: std::io::Error) -> anyhow::Error {
    let kind = e.kind();
    let inner = match e.into_inner().map(|i| i.downcast::<reqwest::Error>()) {
        Some(Ok(re)) => anyhow::Error::new(re.without_url()),
        _ => anyhow::anyhow!("{kind}"),
    };
    inner.context(format!(
        "读取 {} 的响应体失败",
        crate::redact::url_credentials(url)
    ))
}

pub struct HttpFetcher {
    client: std::sync::OnceLock<reqwest::blocking::Client>,
    /// 整个响应体的总预算与字节上限，默认 [`DOWNLOAD_BUDGET`] / [`DOWNLOAD_MAX_BYTES`]；
    /// 只有用例会调小（涓流服务器不能等 300 秒，单测也不该灌 512 MiB）。
    budget: std::time::Duration,
    max_bytes: u64,
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self {
            client: std::sync::OnceLock::new(),
            budget: DOWNLOAD_BUDGET,
            max_bytes: DOWNLOAD_MAX_BYTES,
        }
    }

    /// 只给用例：把两条限额调小，好在秒级内跑出「涓流」与「超大 body」两种终态。
    #[cfg(test)]
    fn with_limits(budget: std::time::Duration, max_bytes: u64) -> Self {
        Self {
            budget,
            max_bytes,
            ..Self::new()
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpFetcher {
    /// `file://…` 或不含 `://` 的入参按本地文件读（总纲 C4 的覆盖方式之一；M5 演练不依赖
    /// 公开 Release）。这一步必须在建 client 之前。
    fn local_path(url: &str) -> Option<&str> {
        url.strip_prefix("file://")
            .or_else(|| (!url.contains("://")).then_some(url))
    }

    /// GET 到「状态码已确认成功」的响应（响应体还没读）：两个取法共用，于是 404 成型、
    /// 非 2xx 文案与凭据脱敏只有一处实现。
    fn send_get(&self, url: &str) -> anyhow::Result<reqwest::blocking::Response> {
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
            tracing::warn!(url = %crate::redact::url_credentials(url), error = %e, "下载失败：{e}");
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
        Ok(resp)
    }
}

impl Fetcher for HttpFetcher {
    /// 只能在 `spawn_blocking` 线程或同步 CLI 路径里调用（见模块头的铁律）。
    fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        if let Some(path) = Self::local_path(url) {
            return std::fs::read(path).map_err(|e| anyhow::anyhow!("读取 {path} 失败：{e}"));
        }
        Ok(self
            .send_get(url)?
            .bytes()
            .map_err(|e| e.without_url())
            .with_context(|| format!("读取 {} 的响应体失败", crate::redact::url_credentials(url)))?
            .to_vec())
    }

    /// 只能在 `spawn_blocking` 线程或同步 CLI 路径里调用（见模块头的铁律）。
    /// 响应体**不进内存**：按 64 KiB 一块同时喂 sha256 与 `sink`（理由见 trait 上的注释）。
    fn download_to(&self, url: &str, sink: &mut dyn std::io::Write) -> anyhow::Result<String> {
        if let Some(path) = Self::local_path(url) {
            let mut f =
                std::fs::File::open(path).map_err(|e| anyhow::anyhow!("读取 {path} 失败：{e}"))?;
            return stream_sha256(&mut f, sink, self.budget, self.max_bytes).map_err(|e| match e {
                StreamErr::Read(e) => anyhow::anyhow!("读取 {path} 失败：{e}"),
                StreamErr::Write(e) => SinkFailed(e).into(),
                StreamErr::Aborted(why) => anyhow::anyhow!("读取 {path} 失败：{why}"),
            });
        }
        let mut resp = self.send_get(url)?;
        stream_sha256(&mut resp, sink, self.budget, self.max_bytes).map_err(|e| match e {
            // 读响应体的 io 错误里包着 reqwest 的错误 ⇒ 先剥 URL（见 [`body_error`]）
            StreamErr::Read(e) => body_error(url, e),
            // 磁盘满不是「下载失败」：文案与归类都不许指向网络
            StreamErr::Write(e) => SinkFailed(e).into(),
            StreamErr::Aborted(why) => {
                anyhow::anyhow!("下载 {} 失败：{why}", crate::redact::url_credentials(url))
            }
        })
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

/// GitHub releases 列表里的一条（只取用得上的两个字段，其余忽略）。
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
}

/// rc tag 的 (x, y, z, N)，按数值比大小用。不是 rc tag（[`is_rc_tag`]）或任一段超出 `u32`
/// （超长 / 溢出）就 `None`，调用方跳过这条。
fn rc_version(tag: &str) -> Option<(u32, u32, u32, u32)> {
    if !is_rc_tag(tag) {
        return None;
    }
    let (core, rc) = tag.strip_prefix('v')?.split_once("-rc")?;
    let mut seg = core.split('.').map(str::parse::<u32>);
    Some((
        seg.next()?.ok()?,
        seg.next()?.ok()?,
        seg.next()?.ok()?,
        rc.parse().ok()?,
    ))
}

/// releases 列表里版本号最大的预发布 rc tag：所有 `prerelease=true` 且 tag 形如
/// `v<x.y.z>-rcN` 的条目按 (x, y, z, N) 数值取最大（v4.0.1-rc1 > v4.0.0-rc10 > v4.0.0-rc9），
/// **不看列表顺序**。这个 API 的返回不按创建时间倒序：2026-09-13 实测 `?per_page=5` 返回
/// rc9 → rc8 → rc7 → rc10 → rc6，最新的 rc10 排第 4；取第一个就让 bwg-rick 在 rc9 上
/// `bui upgrade` 拿到 rc9 的 manifest、打印「已最新」。没有就 `None`（调用方把原来的 404 上抛）。
pub fn latest_rc_tag(fetcher: &dyn Fetcher) -> anyhow::Result<Option<String>> {
    let bytes = fetcher.get_bytes(RELEASES_API_URL)?;
    let list: Vec<GhRelease> =
        serde_json::from_slice(&bytes).context("解析 GitHub releases 列表失败")?;
    Ok(list
        .into_iter()
        .filter(|r| r.prerelease)
        .filter_map(|r| rc_version(&r.tag_name).map(|v| (v, r.tag_name)))
        .max_by_key(|(v, _)| *v)
        .map(|(_, tag)| tag))
}

/// 一次发布的可比大小的序号：`(x, y, z, rc)`，rc 位稳定版取 `u32::MAX`。
pub type ReleaseRank = (u32, u32, u32, u32);

/// 一份发布（manifest 的 `version` + `tag`）的 rank ——「谁更新」只由它回答。
///
/// `version` 按 C4 必须是纯 semver（`4.0.1`），预发布只写在 `tag` 上（`v4.0.1-rc1`），所以
/// x.y.z 一律取自 `version`、rc 位只取自 `tag`：
/// - `tag` 形如 `v<x.y.z>-rcN`（[`is_rc_tag`]）⇒ rc 位 = N，rcN 之间按数值排；
/// - 其余（正式版 tag、老 manifest 没有 tag、认不出的 tag）⇒ rc 位 = `u32::MAX`，于是同一个
///   x.y.z 下 **稳定版 > 任何 rcN**；
/// - `version` 不是三段纯数字（空串、`4.0`、`4.0.1-rc1`、溢出 `u32`）⇒ `None`：调用方一律按
///   「不知道」处理，**不许**当成 `0.0.0` 去比。
///
/// 事故 2026-09-15 的那一对：`(4,0,1,1)`（4.0.1-rc1）> `(4,0,0,MAX)`（稳定版 4.0.0）。
pub fn release_rank(version: &str, tag: &str) -> Option<ReleaseRank> {
    let mut seg = version.trim().trim_start_matches('v').split('.');
    let (x, y, z) = (seg.next()?, seg.next()?, seg.next()?);
    if seg.next().is_some() {
        return None; // 四段以上不是 C4 的纯 semver
    }
    // 稳定版（含没有 tag 的老 manifest、认不出的 tag）rc 位取 u32::MAX ⇒ 同版本下 stable > rcN
    let rc = rc_version(tag).map_or(u32::MAX, |(_, _, _, n)| n);
    Some((x.parse().ok()?, y.parse().ok()?, z.parse().ok()?, rc))
}

/// 一份 manifest 的 [`release_rank`]（没有 `tag` 的老 manifest 按稳定版算）。
pub fn manifest_rank(m: &Manifest) -> Option<ReleaseRank> {
    release_rank(&m.version, m.tag.as_deref().unwrap_or_default())
}

/// 本机「现在停在哪一版」的 rank：优先取 **manifest 缓存**（install / upgrade / 每日自检写下的
/// 那一份，对账正照它装内核），缓存缺失或版本号认不出时退回「运行中的 bui 版本 + 稳定版」。
pub fn installed_rank(cached: Option<&Manifest>, running_bui: &str) -> Option<ReleaseRank> {
    cached
        .and_then(manifest_rank)
        .or_else(|| release_rank(running_bui, ""))
}

/// 版本号 `candidate` 是否**严格高于** `current`（只比 x.y.z，不看 rc）：两边都按稳定版算 rank，
/// 元组比较就等价于按数值比 x.y.z。任一边认不出就返回 false —— 不知道不算更高。
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (release_rank(candidate, ""), release_rank(current, "")) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// 这份 manifest 是不是一份预发布（`tag` = `v<x.y.z>-rcN`）。
pub fn is_rc_manifest(m: &Manifest) -> bool {
    m.tag.as_deref().is_some_and(is_rc_tag)
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

/// C4 那三个覆盖是否**都没给**（空串按没给算，与 [`resolve_manifest_url`] 同一口径）。
/// 预发布回退只在这种「跟着 latest 走」的情况下才允许发生；无参 `bui upgrade` 的降级守卫
/// （`commands::upgrade::refuse_downgrade`）用的也是这个判据。
pub fn nothing_specified(
    cli_override: Option<&str>,
    version: Option<&str>,
    env: Option<&str>,
) -> bool {
    [cli_override, version, env]
        .iter()
        .all(|o| !o.is_some_and(|s| !s.is_empty()))
}

/// 按 C4 的顺序算地址并把 manifest 拉回来，返回 **(实际用的 url, manifest)**。
///
/// 比 [`resolve_manifest_url`] 多的只有一件事：**未指定版本**（没有 `--manifest-url`、没有
/// `--version`、没有 `$BUI_MANIFEST_URL`，也就是跟着 `latest` 走）时若拿到 404，就**无条件**
/// 去 GitHub releases 列表取最新的 `v<x.y.z>-rcN`（裁决记录「发布：预发布与首推
/// （2026-09-12）」：仓库里只有预发布时 `releases/latest/download/manifest.json` 必然 404）。
///
/// 三条边界：
/// - **指定了地址或版本就只认那一个**，404 原样上抛——不许把人从指定源拽回预发布通道。
///   判据是「三个覆盖都没给」（[`nothing_specified`]），不是「算出来的 url 恰好等于
///   [`MANIFEST_URL`]」：后者会让把 `$BUI_MANIFEST_URL` 设成内置 latest 同一串的人也被回退；
/// - **latest 拿到了就不回退**：仓库一有正式版，latest 就解析得出来，正式版机器不跟 rc；
/// - 回退**不看本机是不是 rc**：运行期没有这个信号（见模块头），拿它当前提等于回退永远
///   不发生。
pub fn fetch_manifest_with(
    fetcher: &dyn Fetcher,
    cli_override: Option<&str>,
    version: Option<&str>,
    env: Option<&str>,
) -> anyhow::Result<(String, Manifest)> {
    let url = resolve_manifest_url(cli_override, version, env);
    match Manifest::from_url(fetcher, &url) {
        Ok(m) => Ok((url, m)),
        Err(e) if nothing_specified(cli_override, version, env) && is_not_found(&e) => {
            let tag = match latest_rc_tag(fetcher) {
                Ok(Some(t)) => t,
                // 列表里也没有预发布：正式版还没发而已，把原来的 404 上抛（调用方按
                // 「还没有可用的 manifest」处理，只记一行）
                Ok(None) => return Err(e),
                // 列表也拉不动（断网 / GitHub 限流 / JSON 坏）：两件事一起说清楚，
                // 别让人只看见一个 404
                Err(le) => return Err(e.context(format!("查 GitHub releases 列表也失败：{le:#}"))),
            };
            let rc_url = manifest_url_for_tag(&tag);
            tracing::info!(tag = %tag, build = ?BUILD_TAG, "latest 里没有 manifest（GitHub 不把预发布算 latest），改跟预发布通道");
            let m = Manifest::from_url(fetcher, &rc_url)?;
            Ok((rc_url, m))
        }
        Err(e) => Err(e),
    }
}

/// 每日自检（`serve::selfcheck_loop`）这一轮要不要换 manifest：只有返回 `Some((url, manifest))`
/// 才允许刷缓存 / 换内核 / 提示新版，`None` = 本轮一个字节都不动。
///
/// 事故 2026-09-15（见模块头）：候选**必须**比本机当前 rank（[`installed_rank`]）严格更高，
/// 否则装上 rc 的机器一天之内就被 `releases/latest`（不含预发布）拽回稳定版内核。
///
/// 候选最多两份：
/// 1. latest（或 `$BUI_MANIFEST_URL` 指定的那一个）—— 沿用 [`fetch_manifest_with`]，含
///    「latest 404 就无条件跟最新 rc」那一支；
/// 2. rc 通道那一份 —— 只在「跟着 latest 走」（[`nothing_specified`]）且本机确实在预发布上
///    （缓存是 rc，或运行中的 bui 比 latest 的版本还新 ⇒ 缓存刚被降过，自愈）时才多问一次
///    [`latest_rc_tag`]。稳定版机器（bui 版本 = latest 版本、缓存是稳定版）绝不自动移到预发布。
///
/// 两份里取 rank 大的那一份再与本机比。rank 是 `Option`（`None` < `Some`）：候选认不出而本机
/// 认得出 ⇒ 不动；本机认不出（缓存坏了）⇒ 跟候选走。
pub fn pick_selfcheck_manifest(
    fetcher: &dyn Fetcher,
    env: Option<&str>,
    cached: Option<&Manifest>,
    running_bui: &str,
) -> anyhow::Result<Option<(String, Manifest)>> {
    let current = installed_rank(cached, running_bui);
    let (mut url, mut best) = fetch_manifest_with(fetcher, None, None, env)?;
    // latest 自己 404 回退到 rc 的那一支已经取过最新的 rc，不必再问一遍（`is_rc_manifest`）
    let follow_rc = nothing_specified(None, None, env)
        && !is_rc_manifest(&best)
        && (cached.is_some_and(is_rc_manifest) || version_is_newer(running_bui, &best.version));
    if follow_rc {
        match latest_rc_tag(fetcher) {
            Ok(Some(tag)) => {
                let rc_url = manifest_url_for_tag(&tag);
                match Manifest::from_url(fetcher, &rc_url) {
                    Ok(rc) if manifest_rank(&rc) > manifest_rank(&best) => {
                        (url, best) = (rc_url, rc);
                    }
                    Ok(_) => {}
                    // rc 那一份拉不动不影响 latest 那一份：记一行，接着比
                    Err(e) => tracing::info!(
                        tag = %tag,
                        error = %format!("{e:#}"),
                        "每日自检：预发布 manifest 拉不到，本轮只看 latest"
                    ),
                }
            }
            Ok(None) => {}
            Err(e) => tracing::info!(
                error = %format!("{e:#}"),
                "每日自检：查 GitHub releases 列表失败，本轮只看 latest"
            ),
        }
    }
    if manifest_rank(&best) > current {
        return Ok(Some((url, best)));
    }
    tracing::info!(
        candidate = %best.tag.as_deref().unwrap_or(&best.version),
        current = ?current,
        "每日自检：候选不比本机当前版本新，本轮不动缓存与内核（不许自动降级）"
    );
    Ok(None)
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
///
/// 盘上那份的 sha 走 [`Host::file_sha256`]（流式）而不是 `read_file`：这个判断在守护进程的
/// 升级巡检里周期性跑，而 `b-ui.service` 的 `MemoryMax=200M` 是硬上限。
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
    match host.file_sha256(&bin_dir.join("bui")).ok().flatten() {
        Some(sum) => !sum.eq_ignore_ascii_case(&asset.sha256),
        None => true,
    }
}

/// 期望的那个内核构建与盘上 `bin/<name>` 是不是**两个不同的构建**。判法与
/// [`bui_build_differs`] 一致：先比版本号，版本号相同再比 sha256。
///
/// **版本号不是充分判据**（2026-09-16 裁决，发布阻断级）：自建的 sing-box 与官方同版本归档
/// 打的是同一个版本号（ldflags 里就写着 `constant.Version=1.14.1`），差别只在构建标签
/// （自建带 `with_v2ray_api`）。只比版本号的话，已装官方 1.14.1 的机器上 `InstallBinary`
/// 永远不会被计划、自建二进制装不上去，`bui upgrade` 还会打印「已最新」；于是依赖
/// `v2ray_api` 的那份配置每轮 `sing-box check` 必然 FATAL、永不落盘 —— 我们给自家二进制
/// 做对了这件事，给内核漏了。
///
/// `want_sha256` 取 manifest 的 `artifacts.<name>-linux-<arch>.sha256`（见
/// [`Manifest::kernel_asset`]）。盘上二进制读不到（还没装 / 读失败）按「要装」处理。
///
/// 盘上那份的 sha 必须走 [`Host::file_sha256`]（流式）而不是 `read_file`：这个判断在**每轮
/// 稳态对账**里对四个内核各跑一次，合计约 190 MB、单个最大约 81 MB，而 `b-ui.service` 的
/// `MemoryMax=200M` 是硬上限、对账 600 秒一轮 —— 整文件读进内存就是每 10 分钟复现一次的
/// OOM/重启循环。判据不变，只是「读 190 MB」换成「流 190 MB」。
pub fn kernel_build_differs(
    host: &dyn Host,
    bin_dir: &Path,
    installed: &BTreeMap<String, String>,
    name: &str,
    want_version: &str,
    want_sha256: &str,
) -> bool {
    if installed.get(name).map(String::as_str) != Some(want_version) {
        return true;
    }
    match host.file_sha256(&bin_dir.join(name)).ok().flatten() {
        Some(sum) => !sum.eq_ignore_ascii_case(want_sha256),
        None => true,
    }
}

/// 边下边 hash 边写临时文件 → sha256 校验 → `commit`（0755 rename 到 `dest`）。
///
/// **不许改回 `get_bytes` + `write_file`**：内核二进制单笔约 81 MB（sing-box，自建更大），
/// 而 `b-ui.service` 的 `MemoryMax=200M` 是硬上限 ⇒ 一次性读入就是升级期的 OOM 面
/// （systemd 把守护进程杀在下载半路）。详见 [`Fetcher::download_to`] 与
/// [`crate::sys::StagedWrite`]。
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
        // 校验不过或下载失败 ⇒ `slot` 就这么 drop 掉：临时文件删掉、`dest` 上的现装二进制
        // 一个字节不动（语义与一次性读入那版逐字一致）。
        let mut slot = self.host.stage_file(dest, 0o755)?;
        let got = self.fetcher.download_to(url, &mut slot)?;
        if !got.eq_ignore_ascii_case(sha256) {
            anyhow::bail!("{name} {version} 的 sha256 不匹配：期望 {sha256}，实际 {got}");
        }
        slot.commit()?;
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

    /// 记下每次 `write` 的长度（外加全部字节）的 sink。峰值 RSS 在单测里钉不住，但
    /// **分块搬运**钉得住：一次性读入（`bytes()` / `fs::read` 之后一次 `write_all`）会写出
    /// 一整块 150 KiB，而流式实现每块都 ≤ [`STREAM_BUF`]。复核（2026-09-17）实测：sink 只是
    /// `Vec<u8>` 时，把 `download_to` 整段改回一次性读入，全量门禁照样全绿。
    #[derive(Default)]
    struct CountingSink {
        writes: Vec<usize>,
        bytes: Vec<u8>,
    }

    impl std::io::Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes.push(buf.len());
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CountingSink {
        /// 「字节一位不少 + 每块 ≤ 缓冲 + 至少切成 ⌈len/缓冲⌉ 块」：一条断言同时杀掉
        /// 「HTTP 分支改回 `bytes()`」「本地分支改回 `fs::read`」「两条都换成 `get_bytes`」。
        fn assert_streamed(&self, body: &[u8], what: &str) {
            assert_eq!(self.bytes, body, "{what} 的字节流不完整");
            assert!(
                self.writes.iter().all(|n| *n <= STREAM_BUF),
                "{what} 有一次写了 {:?} 字节（> {STREAM_BUF}）：这就是一次性读入，\
                 峰值内存跟文件大小成正比",
                self.writes.iter().max()
            );
            assert!(
                self.writes.len() >= body.len().div_ceil(STREAM_BUF),
                "{what} 只写了 {} 次，{} 字节至少该切成 {} 块",
                self.writes.len(),
                body.len(),
                body.len().div_ceil(STREAM_BUF)
            );
        }
    }

    /// 回环上的一次性 HTTP 服务器（**不出网**）：接一条连接，把 `head` 与 `chunks` 依次写
    /// 出去、每块之间停 `gap`，写完就关。线程里所有失败都忽略——客户端先走一步（预算到点、
    /// 超限）是有些用例的预期，那时写 socket 会 EPIPE。
    fn serve_once(head: &str, chunks: Vec<Vec<u8>>, gap: std::time::Duration) -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("回环监听");
        let port = l.local_addr().expect("本地地址").port();
        let head = head.to_string();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let Ok((mut s, _)) = l.accept() else { return };
            // 请求头先读掉：不读就关连接会发 RST，客户端看到的就不是「半截 body」了
            let _ = s.read(&mut [0u8; 2048]);
            if s.write_all(head.as_bytes()).is_err() {
                return;
            }
            for c in chunks {
                if s.write_all(&c).is_err() || s.flush().is_err() {
                    return;
                }
                std::thread::sleep(gap);
            }
        });
        port
    }

    struct FakeFetcher {
        files: Mutex<Vec<(String, Vec<u8>)>>,
        seen: Mutex<Vec<String>>,
        /// 走**流式**那条口子的流水（url, 喂进 sink 的字节数）：`install` 改回
        /// `get_bytes` + 一次性 digest 时它会是空的（见 `install_streams_…` 用例）。
        streamed: Mutex<Vec<(String, usize)>>,
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
                streamed: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }

        fn streamed(&self) -> Vec<(String, usize)> {
            self.streamed.lock().unwrap().clone()
        }

        fn take(&self, url: &str) -> anyhow::Result<Vec<u8>> {
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

    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.seen.lock().unwrap().push(url.to_string());
            self.take(url)
        }

        fn download_to(&self, url: &str, sink: &mut dyn std::io::Write) -> anyhow::Result<String> {
            let bytes = self.take(url)?;
            sink.write_all(&bytes)?;
            sink.flush()?;
            self.streamed
                .lock()
                .unwrap()
                .push((url.to_string(), bytes.len()));
            Ok(sha256_hex(&bytes))
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
            streamed: Mutex::new(Vec::new()),
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

    /// 发布阻断级回归（2026-09-16）：自建 sing-box 与官方归档**同版本号**（都打 1.14.1），
    /// 只比版本号的话自建那份永远装不上去 ⇒ 依赖 `with_v2ray_api` 的配置每轮 `check` 必然
    /// FATAL、永不落盘。内核与 `bui` 同一判法：版本号相同再比盘上二进制的 sha256。
    #[test]
    fn kernel_build_differs_falls_back_to_sha_when_the_version_is_unchanged() {
        let bin = Path::new("/opt/b-ui/bin");
        let official = b"SB-1.14.1-official";
        let ours = b"SB-1.14.1-with_v2ray_api";
        let installed = BTreeMap::from([("sing-box".to_string(), "1.14.1".to_string())]);
        let h = FakeHost::new();
        h.write_file(&bin.join("sing-box"), official, 0o755)
            .unwrap();
        let want = sha256_hex(ours);
        assert!(
            kernel_build_differs(&h, bin, &installed, "sing-box", "1.14.2", &want),
            "版本不同：一眼就要装"
        );
        assert!(
            kernel_build_differs(&h, bin, &installed, "sing-box", "1.14.1", &want),
            "同版本号但盘上是官方那一份：也要装"
        );
        h.write_file(&bin.join("sing-box"), ours, 0o755).unwrap();
        assert!(
            !kernel_build_differs(&h, bin, &installed, "sing-box", "1.14.1", &want),
            "同版本同 sha 才是已最新"
        );
        assert!(
            kernel_build_differs(
                &FakeHost::new(),
                bin,
                &installed,
                "sing-box",
                "1.14.1",
                &want
            ),
            "盘上读不到 bin/sing-box：按要装处理"
        );
        assert!(
            kernel_build_differs(&h, bin, &BTreeMap::new(), "sing-box", "1.14.1", &want),
            "探不出已装版本：按要装处理"
        );
    }

    /// 钉住「内核身份不再把二进制整文件读进内存」。
    ///
    /// 这个判断在**每轮稳态对账**里对四个内核各跑一次：实测四个 stripped 内核合计 190 MB、
    /// 单笔最大约 81 MB（sing-box，自建更大），等价进程峰值 RSS 132 MB；而 `b-ui.service`
    /// 的 `MemoryMax=200M` 是**硬上限**、对账 600 秒一轮 —— 用 `read_file` 算 sha 就是每
    /// 10 分钟复现一次的 OOM/重启循环面。判据不变，只把「读 190 MB」换成「流 190 MB」，
    /// 所以这里断言的是**用了哪条接口**：`Host::file_sha256`（流式，峰值几 KiB）被查过。
    /// 改回 `read_file` 这条就转红（`sha_reads` 会是空的）。真实实现是不是真流式，由
    /// `sys::real` 那边的 `file_sha256` 用例按跨块非整数倍的文件钉住。
    #[test]
    fn kernel_identity_streams_the_binary_instead_of_reading_it_whole() {
        let bin = Path::new("/opt/b-ui/bin");
        let sb = bin.join("sing-box");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.14.1".to_string())]);
        let h = FakeHost::new();
        h.write_file(&sb, b"SB-1.14.1-with_v2ray_api", 0o755)
            .unwrap();
        let want = sha256_hex(b"SB-1.14.1-with_v2ray_api");
        h.with(|i| i.sha_reads.clear());
        assert!(
            !kernel_build_differs(&h, bin, &installed, "sing-box", "1.14.1", &want),
            "同版本同 sha：判据不因为换了接口而变"
        );
        assert_eq!(h.sha_reads(), vec![sb], "sha 必须流式算");
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
    /// 第 1 条是 rc 形状、版本号最大但 `prerelease=false`（必须跳过），第 2 条是 prerelease 但
    /// tag 不匹配（必须跳过），剩下的里取版本号最大的 rc2。
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

    /// 2026-09-13 实测 `GET …/releases?per_page=5` 的返回顺序：最新的 rc10（13:54Z 创建）排在
    /// rc9（13:07Z）、rc8（11:39Z）、rc7（04:02Z）之后——这个列表**不**按创建时间倒序。
    const RELEASES_JSON_OBSERVED: &str = r#"[
      {"tag_name": "v4.0.0-rc9",  "prerelease": true},
      {"tag_name": "v4.0.0-rc8",  "prerelease": true},
      {"tag_name": "v4.0.0-rc7",  "prerelease": true},
      {"tag_name": "v4.0.0-rc10", "prerelease": true},
      {"tag_name": "v4.0.0-rc6",  "prerelease": true}
    ]"#;

    fn rc_from(list: &str) -> Option<String> {
        latest_rc_tag(&FakeFetcher::new(vec![(RELEASES_API_URL, list)])).unwrap()
    }

    #[test]
    fn the_rc_channel_picks_the_highest_version_not_the_first_listed() {
        // bwg-rick 在 rc9 上跑 `bui upgrade`：取「第一个」拿到的是 rc9 的 manifest，打印「已最新」
        assert_eq!(
            rc_from(RELEASES_JSON_OBSERVED).as_deref(),
            Some("v4.0.0-rc10")
        );
        let rc10 = manifest_url_for_tag("v4.0.0-rc10");
        let f = FakeFetcher::new(vec![
            (RELEASES_API_URL, RELEASES_JSON_OBSERVED),
            (&rc10, RC_MANIFEST),
        ]);
        let (url, _) = fetch_manifest_with(&f, None, None, None).unwrap();
        assert_eq!(url, rc10, "latest 404 之后跟的是 rc10 的 manifest");
    }

    #[test]
    fn rc_tags_compare_numerically_across_versions() {
        // 按数值比 (x, y, z, N)：rc10 > rc9（不是字典序），v4.0.1-rc1 > v4.0.0-rc10（跨 patch）
        let list = r#"[
          {"tag_name": "v4.0.0-rc9",  "prerelease": true},
          {"tag_name": "v4.0.0-rc10", "prerelease": true},
          {"tag_name": "v4.0.1-rc1",  "prerelease": true}
        ]"#;
        assert_eq!(rc_from(list).as_deref(), Some("v4.0.1-rc1"));
        let list = r#"[
          {"tag_name": "v9.9.9-rc9",  "prerelease": true},
          {"tag_name": "v4.1.0-rc1",  "prerelease": true},
          {"tag_name": "v10.0.0-rc1", "prerelease": true},
          {"tag_name": "v4.0.9-rc99", "prerelease": true}
        ]"#;
        assert_eq!(rc_from(list).as_deref(), Some("v10.0.0-rc1"));
    }

    #[test]
    fn rc_shaped_tags_that_are_not_prereleases_are_skipped() {
        // 版本号最大的两条都不是预发布（一条显式 false，一条缺字段按 false 算）
        let list = r#"[
          {"tag_name": "v4.0.2-rc1",  "prerelease": false},
          {"tag_name": "v4.0.1-rc3"},
          {"tag_name": "v4.0.0-rc9",  "prerelease": true},
          {"tag_name": "v4.0.0-rc10", "prerelease": true}
        ]"#;
        assert_eq!(rc_from(list).as_deref(), Some("v4.0.0-rc10"));
        assert_eq!(
            rc_from(r#"[{"tag_name":"v4.0.2-rc1","prerelease":false}]"#),
            None
        );
    }

    #[test]
    fn malformed_or_overflowing_rc_tags_are_skipped_without_panicking() {
        // 每一段按 u32 解析，解析不了（超长 / 溢出）就跳过；形状不对的本来就不是 rc tag
        let list = r#"[
          {"tag_name": "v4.0.0-rc4294967296",            "prerelease": true},
          {"tag_name": "v4.0.99999999999999999999-rc1",  "prerelease": true},
          {"tag_name": "v99999999999999999999.0.0-rc1",  "prerelease": true},
          {"tag_name": "v5.0.0-rc1x",                    "prerelease": true},
          {"tag_name": "v5.0-rc1",                       "prerelease": true},
          {"tag_name": "v5.0.0-rc",                      "prerelease": true},
          {"tag_name": "v5.0.0",                         "prerelease": true},
          {"tag_name": "nightly",                        "prerelease": true},
          {"tag_name": "v4.0.0-rc9",                     "prerelease": true},
          {"tag_name": "v4.0.0-rc10",                    "prerelease": true}
        ]"#;
        assert_eq!(rc_from(list).as_deref(), Some("v4.0.0-rc10"));
        // u32::MAX 本身还解析得了（shell 那两份按同一个上界比）
        let list = r#"[
          {"tag_name": "v4.0.0-rc10",         "prerelease": true},
          {"tag_name": "v4.0.0-rc4294967295", "prerelease": true}
        ]"#;
        assert_eq!(rc_from(list).as_deref(), Some("v4.0.0-rc4294967295"));
        assert_eq!(
            rc_from(r#"[{"tag_name":"v4.0.0-rc4294967296","prerelease":true}]"#),
            None
        );
    }

    /// 两条都是 prerelease 的列表，给下面按 tag 对拍的用例用（test-install-sh.sh 的 pick2 同一形状）。
    fn two_prereleases(a: &str, b: &str) -> String {
        format!(
            r#"[{{"tag_name":"{a}","prerelease":true}},{{"tag_name":"{b}","prerelease":true}}]"#
        )
    }

    #[test]
    fn leading_zeros_compare_by_value_not_lexically() {
        // release.yml 的 tag 正则 `^v[0-9]+\.[0-9]+\.[0-9]+-rc[0-9]+$` 放前导零过，这种 tag 发得出去、
        // 会被标成 prerelease。每段按数值比：rc010 = 10 > 9，v4.010.0 = v4.10.0 > v4.9.0（字典序
        // 正好反过来）。两种排列取到同一个，与列表顺序无关。
        for (a, b, want) in [
            ("v4.0.0-rc9", "v4.0.0-rc010", "v4.0.0-rc010"),
            ("v4.0.0-rc010", "v4.0.0-rc9", "v4.0.0-rc010"),
            ("v4.9.0-rc1", "v4.010.0-rc1", "v4.010.0-rc1"),
            ("v4.010.0-rc1", "v4.9.0-rc1", "v4.010.0-rc1"),
        ] {
            assert_eq!(
                rc_from(&two_prereleases(a, b)).as_deref(),
                Some(want),
                "[{a}, {b}]"
            );
        }
    }

    #[test]
    fn equal_versions_resolve_to_the_later_listed_tag() {
        // rc1 与 rc01 数值相同：取列表里后出现的那个。这边靠 max_by_key 并列时返回最后一个，
        // install.sh / bui-c-install.sh 的 awk 靠 `i > 4 ||` 那一支替换；test-install-sh.sh 与
        // test-bui-c-install.sh 断言同样的结果，谁改了任一边的比较分支，Rust 与 shell 就在这里分叉。
        for (a, b, want) in [
            ("v4.0.0-rc1", "v4.0.0-rc01", "v4.0.0-rc01"),
            ("v4.0.0-rc01", "v4.0.0-rc1", "v4.0.0-rc1"),
        ] {
            assert_eq!(
                rc_from(&two_prereleases(a, b)).as_deref(),
                Some(want),
                "[{a}, {b}]"
            );
        }
    }

    #[test]
    fn the_running_version_is_never_a_prerelease_signal() {
        // 回退不看本机版本，正因为版本号不可能带 -rc：`check-version.sh` 只放纯 semver 过，
        // `release.yml` 的 verify 又把 `-rcN` 从 tag 上剥掉才校验。faf337f 把它当信号，
        // 于是 rc 机器（bwg-rick，4.0.0 + 无缓存）永远回退不了。
        assert!(
            !env!("CARGO_PKG_VERSION").contains("-rc"),
            "workspace version 必须是纯 semver（check-version.sh 强制），所以它不能当预发布信号"
        );
    }

    #[test]
    fn unspecified_version_follows_latest_when_it_exists() {
        let f = FakeFetcher::new(vec![(MANIFEST_URL, PLAIN_MANIFEST)]);
        let (url, m) = fetch_manifest_with(&f, None, None, None).unwrap();
        assert_eq!(url, MANIFEST_URL);
        assert_eq!(m.version, "4.0.0");
        assert_eq!(m.tag.as_deref(), Some("v4.0.0"));
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL],
            "latest 拿到了就不该再问 GitHub 的 releases 列表——有正式版就绝不跟 rc"
        );
    }

    #[test]
    fn latest_404_always_falls_back_to_the_newest_rc() {
        // GitHub 的 releases/latest 不解析预发布：仓库里只有 rc 时 latest/download/… 必然 404。
        // 回退是无条件的：本机是 4.0.0 正式版号、没有任何 manifest 缓存（bwg-rick 的真机形状）。
        let rc_url = manifest_url_for_tag("v4.0.0-rc2");
        let f = FakeFetcher::new(vec![
            (RELEASES_API_URL, RELEASES_JSON),
            (&rc_url, RC_MANIFEST),
        ]);
        let (url, m) = fetch_manifest_with(&f, None, None, None).unwrap();
        assert_eq!(url, rc_url, "prerelease=true 且 tag 匹配的里取版本号最大的");
        assert_eq!(m.version, "4.0.0");
        assert_eq!(m.tag.as_deref(), Some("v4.0.0-rc2"));
        assert_eq!(
            f.seen(),
            vec![
                MANIFEST_URL.to_string(),
                RELEASES_API_URL.to_string(),
                rc_url.clone()
            ],
            "顺序必须是 latest → releases 列表 → rc 的 manifest"
        );
        // 指定了地址或版本就只认那一个，404 原样上抛（不许把人从指定源拽回预发布通道）
        let e = fetch_manifest_with(&f, None, None, Some("https://x/nope.json")).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        let e = fetch_manifest_with(&f, None, Some("4.9.9"), None).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        let e = fetch_manifest_with(&f, Some("/tmp/nope.json"), None, None).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
    }

    #[test]
    fn a_404_with_no_prerelease_to_fall_back_to_stays_one_clear_error() {
        let rc_url = manifest_url_for_tag("v4.0.0-rc2");
        // 列表里没有预发布（正式版也还没发）：404 原样上抛，只问了 latest + 列表两次
        let f = FakeFetcher::new(vec![
            (
                RELEASES_API_URL,
                r#"[{"tag_name":"v4.0.0","prerelease":false}]"#,
            ),
            (&rc_url, RC_MANIFEST),
        ]);
        let e = fetch_manifest_with(&f, None, None, None).unwrap_err();
        assert!(is_not_found(&e), "404 原样上抛：{e:#}");
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL.to_string(), RELEASES_API_URL.to_string()],
            "不刷屏：latest 与列表各问一次就停"
        );
        // 列表也拉不动（断网 / 限流）：错误里两件事都要说清楚，且仍是 NotFound 家族，
        // 好让每日自检走「只 info 一行」那一支
        let f = FakeFetcher::new(vec![(&rc_url, RC_MANIFEST)]);
        let e = fetch_manifest_with(&f, None, None, None).unwrap_err();
        let msg = format!("{e:#}");
        assert!(
            msg.contains("releases/latest/download/manifest.json"),
            "{msg}"
        );
        assert!(msg.contains("查 GitHub releases 列表也失败"), "{msg}");
        assert!(is_not_found(&e), "{msg}");
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL.to_string(), RELEASES_API_URL.to_string()]
        );
        // 把 $BUI_MANIFEST_URL 设成与内置 latest 逐字相同的串也算「指定了」：只认那一个
        let f = FakeFetcher::new(vec![
            (RELEASES_API_URL, RELEASES_JSON),
            (&rc_url, RC_MANIFEST),
        ]);
        let e = fetch_manifest_with(&f, None, None, Some(MANIFEST_URL)).unwrap_err();
        assert!(is_not_found(&e), "{e:#}");
        assert_eq!(f.seen(), vec![MANIFEST_URL], "指定了就只认那一个");
    }

    /// [`pick_selfcheck_manifest`] 用的 manifest：`tag` 决定 rank 的 rc 位（C4 要求
    /// `version` 是纯 semver，预发布只写在 `tag` 上）。
    fn ranked(version: &str, tag: &str) -> String {
        format!(r#"{{"version":"{version}","tag":"{tag}","kernels":{{}},"artifacts":{{}}}}"#)
    }

    /// GitHub releases 列表：给的 tag 全按 `prerelease=true` 列出。
    fn prereleases(tags: &[&str]) -> String {
        let items: Vec<String> = tags
            .iter()
            .map(|t| format!(r#"{{"tag_name":"{t}","prerelease":true}}"#))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn release_rank_ranks_stable_above_every_rc_of_the_same_version() {
        let r = |v: &str, t: &str| release_rank(v, t).expect("应当解析得出");
        assert_eq!(release_rank("4.0.1", "v4.0.1-rc3"), Some((4, 0, 1, 3)));
        assert_eq!(release_rank("4.0.0", "v4.0.0"), Some((4, 0, 0, u32::MAX)));
        assert!(
            r("4.0.1", "v4.0.1") > r("4.0.1", "v4.0.1-rc9"),
            "同一个版本：稳定版高于任何 rc"
        );
        assert!(
            r("4.0.1", "") > r("4.0.1", "v4.0.1-rc1"),
            "老 manifest 没有 tag：按稳定版算"
        );
        assert!(
            r("4.0.1", "nightly") > r("4.0.1", "v4.0.1-rc1"),
            "认不出的 tag 同样按稳定版算"
        );
        assert!(
            r("4.0.1", "v4.0.1-rc2") > r("4.0.1", "v4.0.1-rc1"),
            "rc 按数值排"
        );
        assert!(
            r("4.0.1", "v4.0.1-rc10") > r("4.0.1", "v4.0.1-rc9"),
            "rc10 > rc9，不是字典序"
        );
        // 事故 2026-09-15 的那一对：4.0.1-rc1 的机器比稳定版 4.0.0 新
        assert!(r("4.0.1", "v4.0.1-rc1") > r("4.0.0", "v4.0.0"));
        // 坏输入一律 None：调用方按「不知道」处理，不许当成 0.0.0 去比
        for bad in [
            "",
            "4.0",
            "4.0.0.1",
            "4.0.x",
            "4.0.1-rc1",
            "99999999999999999999.0.0",
        ] {
            assert_eq!(release_rank(bad, ""), None, "{bad}");
        }
        // 版本号只比 x.y.z，不看 rc
        assert!(version_is_newer("4.0.1", "4.0.0"));
        assert!(version_is_newer("4.1.0", "4.0.9"));
        assert!(!version_is_newer("4.0.0", "4.0.1"));
        assert!(!version_is_newer("4.0.1", "4.0.1"));
        assert!(!version_is_newer("4.0.1", "坏版本号"), "认不出就不算更高");
    }

    #[test]
    fn installed_rank_prefers_the_cached_manifest_then_the_running_binary() {
        let rc1: Manifest = serde_json::from_str(&ranked("4.0.1", "v4.0.1-rc1")).unwrap();
        assert_eq!(installed_rank(Some(&rc1), "4.0.1"), Some((4, 0, 1, 1)));
        assert!(is_rc_manifest(&rc1));
        // 没有缓存（全新装机 / 缓存被删）：按运行中的 bui 版本 + 稳定版算
        assert_eq!(installed_rank(None, "4.0.1"), Some((4, 0, 1, u32::MAX)));
        // 缓存的版本号坏了也退回运行中的版本
        let bad: Manifest = serde_json::from_str(&ranked("4.0", "v4.0-rc1")).unwrap();
        assert!(!is_rc_manifest(&bad), "v4.0-rc1 少一段，不是 rc tag");
        assert_eq!(
            installed_rank(Some(&bad), "4.0.0"),
            Some((4, 0, 0, u32::MAX))
        );
    }

    /// 事故回归（2026-09-15，两台 rc1 服务器）：缓存是 4.0.1-rc1，而 `releases/latest` 不含
    /// 预发布、给回来的是更旧的稳定版 4.0.0 —— 本轮一个字节都不许动。
    #[test]
    fn the_daily_selfcheck_never_walks_back_from_a_prerelease_to_stable() {
        let rc1 = ranked("4.0.1", "v4.0.1-rc1");
        let stable = ranked("4.0.0", "v4.0.0");
        let rc_url = manifest_url_for_tag("v4.0.1-rc1");
        let f = FakeFetcher::new(vec![
            (MANIFEST_URL, &stable),
            (RELEASES_API_URL, &prereleases(&["v4.0.1-rc1"])),
            (&rc_url, &rc1),
        ]);
        let cached: Manifest = serde_json::from_str(&rc1).unwrap();
        assert_eq!(
            pick_selfcheck_manifest(&f, None, Some(&cached), "4.0.1").unwrap(),
            None,
            "两个候选（稳定版 4.0.0、同一份 rc1）都不比本机新：不换缓存、不动内核、不提示"
        );
    }

    /// 自愈：缓存已被稳定版 4.0.0 覆盖（事故留下的现状），但运行中的 bui 是 4.0.1 ⇒ 去问 rc
    /// 通道，把缓存恢复成 4.0.1-rc1。
    #[test]
    fn the_daily_selfcheck_heals_a_cache_that_was_already_walked_back() {
        let rc1 = ranked("4.0.1", "v4.0.1-rc1");
        let stable = ranked("4.0.0", "v4.0.0");
        let rc_url = manifest_url_for_tag("v4.0.1-rc1");
        let f = FakeFetcher::new(vec![
            (MANIFEST_URL, &stable),
            (RELEASES_API_URL, &prereleases(&["v4.0.1-rc1"])),
            (&rc_url, &rc1),
        ]);
        let cached: Manifest = serde_json::from_str(&stable).unwrap();
        let (url, m) = pick_selfcheck_manifest(&f, None, Some(&cached), "4.0.1")
            .unwrap()
            .expect("运行中的 bui 比 latest 新 ⇒ 必须回到 rc 通道");
        assert_eq!(url, rc_url);
        assert_eq!(m.tag.as_deref(), Some("v4.0.1-rc1"));
        assert_eq!(
            f.seen(),
            vec![
                MANIFEST_URL.to_string(),
                RELEASES_API_URL.to_string(),
                rc_url
            ],
            "顺序必须是 latest → releases 列表 → rc 的 manifest"
        );
    }

    /// 稳定版机器（bui 版本 = latest 版本、缓存是稳定版）绝不自动移到预发布：连 releases
    /// 列表都不该去问。
    #[test]
    fn a_stable_machine_is_never_moved_onto_the_prerelease_channel() {
        let stable = ranked("4.0.0", "v4.0.0");
        let rc = ranked("4.0.1", "v4.0.1-rc1");
        let rc_url = manifest_url_for_tag("v4.0.1-rc1");
        let f = FakeFetcher::new(vec![
            (MANIFEST_URL, &stable),
            (RELEASES_API_URL, &prereleases(&["v4.0.1-rc1"])),
            (&rc_url, &rc),
        ]);
        let cached: Manifest = serde_json::from_str(&stable).unwrap();
        assert_eq!(
            pick_selfcheck_manifest(&f, None, Some(&cached), "4.0.0").unwrap(),
            None,
            "latest 就是本机这一版：没有更新的候选"
        );
        assert_eq!(
            f.seen(),
            vec![MANIFEST_URL],
            "一次都不许问 releases 列表：正式版机器不跟 rc"
        );
    }

    /// rc 机器等的就是同版本的正式版：rank 规则里 4.0.1 > 4.0.1-rc1，换过去。
    #[test]
    fn a_prerelease_machine_moves_up_to_the_stable_release_of_the_same_version() {
        let rc1 = ranked("4.0.1", "v4.0.1-rc1");
        let stable = ranked("4.0.1", "v4.0.1");
        let rc_url = manifest_url_for_tag("v4.0.1-rc1");
        let f = FakeFetcher::new(vec![
            (MANIFEST_URL, &stable),
            (RELEASES_API_URL, &prereleases(&["v4.0.1-rc1"])),
            (&rc_url, &rc1),
        ]);
        let cached: Manifest = serde_json::from_str(&rc1).unwrap();
        let (url, m) = pick_selfcheck_manifest(&f, None, Some(&cached), "4.0.1")
            .unwrap()
            .expect("同版本的正式版是升级");
        assert_eq!(url, MANIFEST_URL);
        assert_eq!(m.tag.as_deref(), Some("v4.0.1"));
    }

    /// rc 机器继续跟 rc：列表里出现 rc2 就换过去（latest 仍是更旧的稳定版 4.0.0）。
    #[test]
    fn a_prerelease_machine_follows_the_next_rc() {
        let rc1 = ranked("4.0.1", "v4.0.1-rc1");
        let rc2 = ranked("4.0.1", "v4.0.1-rc2");
        let stable = ranked("4.0.0", "v4.0.0");
        let rc2_url = manifest_url_for_tag("v4.0.1-rc2");
        let f = FakeFetcher::new(vec![
            (MANIFEST_URL, &stable),
            (
                RELEASES_API_URL,
                &prereleases(&["v4.0.1-rc1", "v4.0.1-rc2"]),
            ),
            (&rc2_url, &rc2),
        ]);
        let cached: Manifest = serde_json::from_str(&rc1).unwrap();
        let (url, m) = pick_selfcheck_manifest(&f, None, Some(&cached), "4.0.1")
            .unwrap()
            .expect("rc2 比 rc1 新");
        assert_eq!(url, rc2_url);
        assert_eq!(m.tag.as_deref(), Some("v4.0.1-rc2"));
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

    /// 钉住「安装写那一半也不再把二进制整段读进内存」（2026-09-17 裁决）。
    ///
    /// 上一轮只改了「稳态对账算内核 sha」那一半；下载写盘这一半仍是
    /// `get_bytes` → `sha256_hex(&bytes)` → `write_file(dest, &bytes)`，一次安装/升级里单笔
    /// 就有约 81 MB 常驻（sing-box，自建带 `with_v2ray_api` 的更大）加一份写盘副本，而
    /// `b-ui.service` 的 `MemoryMax=200M` 是**硬上限**（`modules/units.rs`）⇒ 升级期 OOM、
    /// 被 systemd 杀在下载半路。峰值 RSS 在单测里钉不住，所以这里钉**用了哪条接口**：
    /// `Fetcher::download_to` + `Host::stage_file`，`get_bytes` 一次都没碰。改回一次性读入
    /// 这条就转红。真实现是不是真流式，由 `download_to_streams_in_chunks_…` 按跨块非整数倍
    /// 的长度对照一次性 digest 钉住。
    #[test]
    fn install_streams_the_body_instead_of_reading_it_whole() {
        let payload = b"binary-bytes";
        let (m, f) = fixture(payload);
        let h = FakeHost::new();
        let dest = std::path::Path::new("/opt/b-ui/bin/hysteria");
        let (ver, asset) = m.kernel_asset("hysteria", "x86_64").unwrap();
        KernelInstaller {
            fetcher: &f,
            host: &h,
        }
        .install("hysteria", ver, &asset.sha256, &asset.url, dest)
        .unwrap();
        assert_eq!(
            f.streamed(),
            vec![(asset.url.clone(), payload.len())],
            "二进制必须走 download_to（边下边 hash 边写临时文件）"
        );
        assert!(
            f.seen().is_empty(),
            "二进制不许经 get_bytes 整段读进内存（get_bytes 只给 manifest 这类小文件）"
        );
        assert_eq!(
            h.staged(),
            vec![dest.to_path_buf()],
            "先开临时写入槽，校验过才 commit"
        );
        assert_eq!(
            h.ops(),
            vec!["write:/opt/b-ui/bin/hysteria:755"],
            "落盘仍是一次 0755 写入，ops 契约不变"
        );
    }

    /// 真实文件系统上的语义（假机器钉不住 rename 与临时文件）：sha 不匹配 ⇒ 现装二进制
    /// 一个字节不动、`bin/` 里不留 `.sing-box.download` 残骸；匹配 ⇒ 0755 就位。
    /// 「下载源」用本地路径（`HttpFetcher` 的 file 分支与 HTTP 分支共用同一条流式循环），
    /// 于是这条用例不出网。
    #[test]
    fn install_on_a_real_filesystem_never_touches_the_installed_binary_until_the_sha_matches() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().unwrap();
        let host = crate::sys::real::RealHost::new();
        let bin = d.path().join("bin");
        let dest = bin.join("sing-box");
        host.write_file(&dest, b"SB-1.14.1-official", 0o755)
            .unwrap();
        // 跨块且非整数倍：把流式循环写错就会算出另一个 sha
        let body: Vec<u8> = (0..(150 * 1024 + 123)).map(|i| (i % 251) as u8).collect();
        let src = d.path().join("payload");
        std::fs::write(&src, &body).unwrap();
        let f = HttpFetcher::new();
        let inst = KernelInstaller {
            fetcher: &f,
            host: &host,
        };
        let err = inst
            .install(
                "sing-box",
                "1.14.1",
                &sha256_hex(b"another-build"),
                src.to_str().unwrap(),
                &dest,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256 不匹配"), "{err}");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"SB-1.14.1-official",
            "校验不过绝不动现装二进制"
        );
        assert_eq!(
            host.list_dir(&bin).unwrap(),
            vec![dest.clone()],
            "临时文件必须删掉（每次校验失败留一个 81 MB 残骸会把盘撑爆）"
        );
        inst.install(
            "sing-box",
            "1.14.1",
            &sha256_hex(&body),
            src.to_str().unwrap(),
            &dest,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            body,
            "校验过才 rename 到目标"
        );
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(host.list_dir(&bin).unwrap(), vec![dest]);
    }

    /// 真流式的正确性判据：分块搬运的结果与一次性 digest 逐位相同（0 / 单块内 / 正好一块 /
    /// 跨两块且非整数倍，本地路径与 `file://` 两种写法各来一遍），**而且真的是分块的**
    /// ——sink 记下每次 write 的长度，一次性读入会写出一整块（见 [`CountingSink`]）。
    #[test]
    fn download_to_streams_in_chunks_and_matches_the_one_shot_digest() {
        let d = tempfile::tempdir().unwrap();
        let f = HttpFetcher::new();
        for len in [0usize, 7, 64 * 1024, 64 * 1024 + 1, 150 * 1024 + 123] {
            let body: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let p = d.path().join(format!("payload-{len}"));
            std::fs::write(&p, &body).unwrap();
            for url in [p.display().to_string(), format!("file://{}", p.display())] {
                let mut sink = CountingSink::default();
                let got = f.download_to(&url, &mut sink).unwrap();
                assert_eq!(
                    got,
                    sha256_hex(&body),
                    "长度 {len} 的流式 sha 与一次性 digest 不符（{url}）"
                );
                sink.assert_streamed(&body, &format!("长度 {len} 的本地源（{url}）"));
            }
        }
        assert!(
            f.download_to(
                &d.path().join("nope").display().to_string(),
                &mut Vec::new()
            )
            .is_err(),
            "源不存在必须报错，不能静默产出空文件的 sha"
        );
    }

    /// HTTP 分支自己的分块判据 + 状态码成型。上一条只覆盖本地路径 / `file://`，所以复核
    /// （2026-09-17）实测「只把 HTTP 分支改回 `send_get().bytes()`」是全绿的，`send_get` 里
    /// 404 成型那三行删掉也全绿。回环 `TcpListener` 手写响应，不出网。
    #[test]
    fn download_to_over_http_streams_in_chunks_and_shapes_status_errors() {
        let f = HttpFetcher::new();
        // 跨块且非整数倍；服务器按 11 KiB 一片发，客户端怎么攒都不该出现 > 64 KiB 的写入
        let body: Vec<u8> = (0..(150 * 1024 + 123)).map(|i| (i % 251) as u8).collect();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let chunks: Vec<Vec<u8>> = body.chunks(11 * 1024).map(|c| c.to_vec()).collect();
        let port = serve_once(&head, chunks, std::time::Duration::ZERO);
        let mut sink = CountingSink::default();
        let got = f
            .download_to(&format!("http://127.0.0.1:{port}/sing-box"), &mut sink)
            .expect("200 必须收完");
        assert_eq!(
            got,
            sha256_hex(&body),
            "HTTP 分支的流式 sha 与一次性 digest 不符"
        );
        sink.assert_streamed(&body, "HTTP 200 的响应体");

        // 404 单独成型：预发布通道回退靠它判断（见 `fetch_manifest_with`）
        let port = serve_once(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            vec![],
            std::time::Duration::ZERO,
        );
        let e = f
            .download_to(&format!("http://127.0.0.1:{port}/nope"), &mut Vec::new())
            .unwrap_err();
        assert!(is_not_found(&e), "{e:#}");

        // 其余非 2xx 带状态码
        let port = serve_once(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            vec![],
            std::time::Duration::ZERO,
        );
        let e = format!(
            "{:#}",
            f.download_to(&format!("http://127.0.0.1:{port}/boom"), &mut Vec::new())
                .unwrap_err()
        );
        assert!(e.contains("HTTP 500"), "{e}");
    }

    /// 复核 2026-09-17 的 blocking 项：`Client::builder().timeout(300s)` 在
    /// `reqwest::blocking` 的流式 `Read` 上是**每次 read** 重新计时的
    /// （`blocking::wait::timeout` 每次都 `Instant::now() + d`），于是「整个请求 300 秒」
    /// 退化成「每 64 KiB 一次 read 的预算」——涓流/半死的镜像不再报错。而守护进程里只有
    /// **一条**对账消费者（`serve.rs` 的 reconcile mpsc，外面没有 `tokio::time::timeout`），
    /// 一次卡死之后去抖触发、10 分钟漂移巡检、每日自检就全在队列里排队：配置不写、单元
    /// 不重启、漂移不报、也不产 incident。这条用例钉「涓流有终态」。
    #[test]
    fn a_trickling_mirror_ends_on_the_total_budget_instead_of_blocking_forever() {
        assert_eq!(
            DOWNLOAD_BUDGET,
            std::time::Duration::from_secs(300),
            "预算就是 `get_bytes` 时代的语义（整个请求 300 秒），不许放大"
        );
        assert_eq!(
            HttpFetcher::new().budget,
            DOWNLOAD_BUDGET,
            "默认实现必须带着预算，不是只有用例才有"
        );
        // Content-Length 报 1 MB，实际每 5 ms 挤一个字节（400 个之后撒手）。预算调成 100 ms：
        // 不看总预算的实现要等服务器撒手才报错，那时错误串里是「响应体失败」而不是「总预算」。
        let port = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n",
            vec![vec![b'x'; 1]; 400],
            std::time::Duration::from_millis(5),
        );
        let f = HttpFetcher::with_limits(std::time::Duration::from_millis(100), DOWNLOAD_MAX_BYTES);
        let url = format!("http://user:s3cr3t-token@127.0.0.1:{port}/sing-box");
        let start = std::time::Instant::now();
        let e = format!("{:#}", f.download_to(&url, &mut Vec::new()).unwrap_err());
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(10),
            "{took:?} 才退出：失败路径没有终态"
        );
        assert!(e.contains("总预算"), "错误要说清是预算到点：{e}");
        assert!(!e.contains("s3cr3t-token"), "{e}");
        assert!(e.contains("***:***@127.0.0.1"), "{e}");
    }

    /// 字节上限：镜像坏掉或被替换后回一个无限/超大 body 时别一路写到 ENOSPC（盘撑满期间
    /// `state.json` 写入、证书续签、journald 会一起失败）。sha 校验只在下载**之后**才有机会
    /// 拒绝，所以上限必须在循环里。
    #[test]
    fn an_oversized_body_is_refused_before_it_fills_the_disk() {
        assert_eq!(
            DOWNLOAD_MAX_BYTES,
            512 * 1024 * 1024,
            "上限不许放大到实际等于没有（最大的真资产约 81 MB）"
        );
        assert_eq!(HttpFetcher::new().max_bytes, DOWNLOAD_MAX_BYTES);
        let body = vec![b'y'; 40 * 1024];
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        let port = serve_once(&head, vec![body.clone()], std::time::Duration::ZERO);
        let f = HttpFetcher::with_limits(DOWNLOAD_BUDGET, 8 * 1024);
        let mut sink = CountingSink::default();
        let e = format!(
            "{:#}",
            f.download_to(&format!("http://127.0.0.1:{port}/huge"), &mut sink)
                .unwrap_err()
        );
        assert!(e.contains("上限"), "{e}");
        assert!(
            sink.bytes.len() < body.len(),
            "超限之后不许继续往盘上写：已写 {} 字节",
            sink.bytes.len()
        );
    }

    /// 磁盘满不是「下载失败」：写 sink 的 io 错误在类型上与读来源分开（[`SinkFailed`]），
    /// 首层文案里也不许出现下载地址——装机/升级要连写约 190 MB，磁盘满是最现实的失败，
    /// 把操作者指向网络就是误导（`panel::packages::sync_once` 的 `{key} 写盘失败：` 靠它）。
    #[test]
    fn a_full_disk_is_reported_as_a_write_failure_not_a_download_failure() {
        struct FullDisk;
        impl std::io::Write for FullDisk {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(28))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("payload");
        std::fs::write(&p, b"binary-bytes").unwrap();
        let e = HttpFetcher::new()
            .download_to(p.to_str().unwrap(), &mut FullDisk)
            .unwrap_err();
        let io = sink_failure(&e).unwrap_or_else(|| panic!("写盘失败必须认得出来：{e:#}"));
        assert_eq!(io.raw_os_error(), Some(28));
        let msg = format!("{e:#}");
        assert!(msg.contains("写临时文件失败"), "{msg}");
        assert!(
            !msg.contains(&p.display().to_string()),
            "写盘失败的文案不许点名下载来源：{msg}"
        );
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
        assert_eq!(
            h.staged(),
            vec![std::path::PathBuf::from("/opt/b-ui/bin/xray")],
            "临时写入槽开过、但没 commit：目标路径上的现装二进制不动"
        );
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

        // 流式那条口子同一条铁律（二进制的下载地址也可能带 basic auth）。注意这一段
        // 与上一段一样只走到 `send_get`（1 端口必定拒连），读正文那条口子在下面单独试。
        let chain = format!("{:#?}", f.download_to(URL, &mut Vec::new()).unwrap_err());
        assert!(!chain.contains("s3cr3t-token"), "{chain}");
        assert!(!chain.contains("user:"), "{chain}");
        assert!(chain.contains("***:***@127.0.0.1:1"), "{chain}");

        // 「响应头 200、读正文中途断」那一支（复核 2026-09-17：原来的断言一次都没走到这里）：
        // 回环监听报 Content-Length: 1000 只发 5 字节就关，错误来自 `stream_sha256` 的 Read。
        let port = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n",
            vec![b"short".to_vec()],
            std::time::Duration::ZERO,
        );
        let url = format!("http://user:s3cr3t-token@127.0.0.1:{port}/sing-box");
        let chain = format!("{:#?}", f.download_to(&url, &mut Vec::new()).unwrap_err());
        assert!(!chain.contains("s3cr3t-token"), "{chain}");
        assert!(!chain.contains("user:"), "{chain}");
        assert!(chain.contains("***:***@127.0.0.1"), "{chain}");

        // 上面那条今天**挡不住** `without_url()` 被删掉：这一版 reqwest（0.12.28）的 body
        // 错误恰好不带 url。而模块头的铁律明写不许依赖这种内部实现细节（换一版、换一种
        // body 错误就漏），所以直接拿一条**确实带 url** 的 reqwest 错误（`send()` 拒连那种）
        // 包进 `io::Error` 喂给 `body_error`，钉住「URL 有没有被剥掉」本身：URL 里那段路径
        // 只有 reqwest 的错误知道，脱敏上下文里不会出现它。userinfo 这一版已被 reqwest 挪进
        // Authorization 头（实测 `e.url()` 的 username 是空的），所以只能这么钉。
        let with_url = reqwest::blocking::Client::new()
            .get("http://127.0.0.1:1/only-reqwest-knows-this-path")
            .send()
            .unwrap_err();
        assert!(
            format!("{with_url}").contains("only-reqwest-knows-this-path"),
            "夹具前提：这条 reqwest 错误本来就带 url，否则这段断言什么都没验"
        );
        let e = body_error(URL, std::io::Error::other(with_url));
        // `{:#?}` 把 url 字段摊出来，`{:#}` 带 ` for url (…)`：两种都不许留下
        for chain in [format!("{e:#?}"), format!("{e:#}")] {
            assert!(
                !chain.contains("only-reqwest-knows-this-path"),
                "reqwest 的错误必须先剥掉 URL：{chain}"
            );
            assert!(chain.contains("***:***@127.0.0.1:1"), "{chain}");
        }

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
