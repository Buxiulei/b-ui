//! `update`：多源取 manifest（面板 → GitHub latest）、多源取产物（面板 → manifest 的
//! `url` → `manifest.mirrors`），sha256 校验后自替换 `/usr/local/bin/bui-c` 与
//! `bin/sing-box`。manifest 形状 = 总纲 C4：`artifacts` 是扁平表，每个 `url` 指向
//! **裸二进制**（上游 tar.gz 由 P5 的 Actions 解包后重新上传），所以客户端不解包、不调 `tar`。

use crate::lock::{self, How, LockGuard};
use crate::net::Net;
use crate::paths::{Paths, SELF_BIN, UNIT_MAIN};
use crate::profiles::{https_base, Panel, Profiles};
use crate::sys::{systemd, Sys};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GITHUB_REPO: &str = "Buxiulei/b-ui";
/// GitHub 的 releases 列表（无凭据，一页取满 100 条——API 的上限）：`releases/latest` 不解析预发布，
/// 预发布通道靠它找最新的 rc tag。这个列表不按创建时间排序（见 `latest_rc_tag`），页小了新 rc
/// 可能落在第一页之外。与服务端 `crates/bui/src/kernels/mod.rs::RELEASES_API_URL` 同口径。
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=100";
pub const DL_TIMEOUT: Duration = Duration::from_secs(120);
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kernels {
    /// 客户端要用的 sing-box 版本（总纲 C4 的 `kernels.client_sing_box`）。**不是** `sing_box`：
    /// 服务端 state 里 `versions.sing_box` 是 relay 的版本，两者可以不同。
    pub client_sing_box: String,
}

/// 总纲 C4 的一个 artifact：`url` 指向**裸二进制**（上游的 tar.gz/zip 由 P5 的 Actions 解包后重新上传）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub kernels: Kernels,
    /// artifact 直链的前缀镜像，由 manifest 下发（缺省空）。客户端不硬编码域名。
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// 键固定 `<name>-linux-<amd64|arm64>`；客户端只取 `bui-c-linux-*` 与 `sing-box-linux-*`，
    /// 其余键（`bui-linux-*` / `hysteria-*` / `xray-*` / `caddy-*`）照收不用。
    pub artifacts: BTreeMap<String, Artifact>,
}

impl Manifest {
    fn artifact(&self, file: &str) -> Result<&Artifact> {
        self.artifacts
            .get(file)
            .ok_or_else(|| Error::msg(format!("manifest 里没有 {file} 这个产物")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub manifest_source: String,
    pub manifest_version: String,
    /// manifest 里的 bui-c 与已装的不是同一份构建（[`self_build_differs`]）；`check_only` 也会填。
    /// 恒等于 `self_reason != SelfReason::Current`
    pub self_outdated: bool,
    /// `/usr/local/bin/bui-c` 被替换
    pub self_updated: bool,
    /// `bin/sing-box` 被替换
    pub kernel_updated: bool,
    pub restarted: bool,
    /// 自身为什么要换（或为什么换不了），[`build_differs`] 的结论；`check_only` 也会填
    pub self_reason: SelfReason,
    /// 本机内核不是 manifest 要的版本（含读不到版本）；`check_only` 也会填
    pub kernel_outdated: bool,
    /// manifest 要的客户端内核版本（`kernels.client_sing_box`）
    pub kernel_wanted: String,
    /// 本机 `bin/sing-box version` 报的版本；没装或跑不通是 `None`。[`install`] 进锁发现内核已被
    /// 别处换过时改成那时读到的
    pub kernel_local: Option<String>,
    /// [`install`] 进锁时发现下载期间盘上已被别的操作换过，至少有一项没盖。这时 `self_reason` 与
    /// `kernel_outdated` 已按盘上现在的样子改过：别处装的正是 manifest 那一份就是「已是最新」
    pub superseded: bool,
}

impl Report {
    /// 这次要不要记成「更新过」（`runtime.last_update_at`，每日自更新据此隔 23 小时再查）：换上了东西，
    /// 或本来就没有要换的。进锁发现下载期间已被别处换过、这次一样都没换上的不算——那是别处那次
    /// 更新，它自己会记；别处装的不是 manifest 那一份时也不该推迟 23 小时（审查 T12b r2 I1）。
    pub fn counts_as_update(&self) -> bool {
        self.self_updated || self.kernel_updated || !self.superseded
    }
}

/// manifest 里的 bui-c 与本机的比出来是什么情况（spec §8.1）。菜单与 `--check-only` 按它分开说：
/// 以前一个布尔把「读不到本机二进制」「manifest 缺本机架构」也说成「有同版本的新构建」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelfReason {
    /// 版本相同、sha256 也相同：就是 manifest 里那一份构建。
    #[default]
    Current,
    /// 版本号不同。
    NewVersion,
    /// 版本号相同、sha256 不同：rc 通道的同版本重建。
    Rebuild,
    /// 版本号相同，但盘上的 bui-c 读不到：更新会重新装一份。
    Unreadable,
    /// manifest 里没有本机架构的 `bui-c-linux-<arch>`：没有可下载的，这次换不了自身。
    MissingAsset,
}

pub fn arch_suffix() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

/// 来源顺序：面板 `/packages/` → manifest 给的 `url`（C4 指向 GitHub Release 资产）→ `manifest.mirrors`
/// 前缀（spec §6 的 update 条目）。URL 一律来自 manifest，客户端不自己拼 Release 路径。
pub fn sources(panel: Option<&Panel>, m: &Manifest, file: &str) -> Result<Vec<(String, String)>> {
    let url = m.artifact(file)?.url.clone();
    let mut out = Vec::with_capacity(2 + m.mirrors.len());
    if let Some(base) = trusted_panel(panel) {
        out.push(("面板".to_string(), format!("{base}/packages/{file}")));
    }
    out.push(("GitHub".to_string(), url.clone()));
    for (i, mirror) in m.mirrors.iter().enumerate() {
        out.push((
            format!("镜像 {}", i + 1),
            format!("{}/{url}", mirror.trim_end_matches('/')),
        ));
    }
    Ok(out)
}

/// manifest 自身只有两个源：镜像列表在 manifest 里，拿不到 manifest 就没有镜像可用。
/// 两个源各 15s 超时，最坏 30s；离线机器由 `check` 的自更新退避（1 小时）兜住。
/// 自更新只认 https 的 panel。导入时已经只写 https（K1），这里再挡一层：旧版本经 `--sub`
/// 或明文面板写进 profiles.json 的地址照样会被读出来，而它决定 root 替换哪份二进制。
fn trusted_panel(panel: Option<&Panel>) -> Option<String> {
    let p = panel?;
    let base = https_base(&p.base_url);
    if base.is_none() {
        tracing::warn!(
            panel = %crate::error::redact_url(&p.base_url),
            "profiles.json 里的面板不是 https，自更新跳过它"
        );
    }
    base
}

fn manifest_sources(panel: Option<&Panel>) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(2);
    if let Some(base) = trusted_panel(panel) {
        out.push(("面板".to_string(), format!("{base}/packages/manifest.json")));
    }
    out.push((
        "GitHub".to_string(),
        format!("https://github.com/{GITHUB_REPO}/releases/latest/download/manifest.json"),
    ));
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// tag 是不是 `^v\d+\.\d+\.\d+-rc\d+$`（整串锚定；不引 regex crate，手写，与服务端同一份逻辑）。
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
/// （超长 / 溢出）就 `None`，调用方跳过这条。与服务端同一份逻辑。
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
/// rc9 → rc8 → rc7 → rc10 → rc6，最新的 rc10 排第 4，取第一个会停在 rc9 上。没有就 `None`。
fn latest_rc_tag<N: Net>(net: &N) -> Result<Option<String>> {
    let body = net.text(RELEASES_API_URL, MANIFEST_TIMEOUT)?;
    let list: Vec<GhRelease> = serde_json::from_str(&body)
        .map_err(|e| Error::parse("GitHub releases 列表", e.to_string()))?;
    Ok(list
        .into_iter()
        .filter(|r| r.prerelease)
        .filter_map(|r| rc_version(&r.tag_name).map(|v| (v, r.tag_name)))
        .max_by_key(|(v, _)| *v)
        .map(|(_, tag)| tag))
}

/// `Net::text` 对非 2xx 报 `HTTP <code>`；预发布回退只认 404（「没有正式版」），
/// 断网 / 502 不是回退的理由。
fn is_http_404(e: &Error) -> bool {
    matches!(e, Error::Net { detail, .. } if detail == "HTTP 404")
}

/// 来源顺序：面板 `/packages/manifest.json` → GitHub `releases/latest` →（latest 404 时）
/// GitHub releases 列表里最新的预发布。仓库里只有 `v4.0.0-rcN` 时 latest 必然 404
/// （GitHub 不把预发布算 latest），服务端 `kernels::fetch_manifest_with` 已实现同样的回退。
pub fn fetch_manifest<N: Net>(net: &N, panel: Option<&Panel>) -> Result<(String, Manifest)> {
    let mut last = String::new();
    for (name, url) in manifest_sources(panel) {
        match net.text(&url, MANIFEST_TIMEOUT) {
            Ok(body) => match serde_json::from_str::<Manifest>(&body) {
                Ok(m) => return Ok((name, m)),
                Err(e) => last = format!("{name}: manifest 解析失败（{e}）"),
            },
            Err(e) if name == "GitHub" && is_http_404(&e) => {
                last = match latest_rc_tag(net) {
                    Ok(Some(tag)) => {
                        let rc_url = format!(
                            "https://github.com/{GITHUB_REPO}/releases/download/{tag}/manifest.json"
                        );
                        let rc_name = format!("GitHub 预发布 {tag}");
                        match net.text(&rc_url, MANIFEST_TIMEOUT) {
                            Ok(body) => match serde_json::from_str::<Manifest>(&body) {
                                Ok(m) => return Ok((rc_name, m)),
                                Err(e) => format!("{rc_name}: manifest 解析失败（{e}）"),
                            },
                            Err(e) => format!("{rc_name}: {e}"),
                        }
                    }
                    Ok(None) => format!("{name}: {e}（releases 列表里也没有预发布）"),
                    Err(le) => format!("{name}: {e}；查 GitHub releases 列表也失败：{le}"),
                };
            }
            Err(e) => last = format!("{name}: {e}"),
        }
    }
    Err(Error::msg(format!(
        "manifest.json 所有来源都失败（最后一个错误：{last}）"
    )))
}

pub fn fetch_verified<N: Net>(
    net: &N,
    srcs: &[(String, String)],
    sha256: &str,
) -> Result<(String, Vec<u8>)> {
    let mut last = String::new();
    for (name, url) in srcs {
        match net.bytes(url, DL_TIMEOUT) {
            Ok(data) => {
                let got = sha256_hex(&data);
                if got == sha256 {
                    return Ok((name.clone(), data));
                }
                last = format!("{name}: sha256 不符（期望 {sha256}，实际 {got}）");
            }
            Err(e) => last = format!("{name}: {e}"),
        }
    }
    Err(Error::Verify(format!(
        "下载与校验全部失败（最后一个错误：{last}）"
    )))
}

/// `bin/sing-box version` 首行的版本号；二进制不存在或跑不通都是 `None`
/// （= 读不到 = 该装内核，不会被静默当成「已是最新」）。
pub fn kernel_version<S: Sys>(sys: &S, paths: &Paths) -> Option<String> {
    if !sys.exists(&paths.singbox()) {
        return None;
    }
    let out = sys
        .run(&paths.singbox().display().to_string(), &["version"])
        .ok()?;
    if !out.ok() {
        return None;
    }
    out.stdout
        .lines()
        .next()?
        .split_whitespace()
        .nth(2)
        .map(|s| s.to_string())
}

fn replace_self<S: Sys>(sys: &S, data: &[u8]) -> Result<()> {
    let target = PathBuf::from(SELF_BIN);
    let dir = target.parent().unwrap_or(Path::new("/usr/local/bin"));
    let tmp = dir.join(".bui-c.tmp");
    sys.write(&tmp, data, 0o755)?;
    sys.rename(&tmp, &target)
}

/// 装 manifest 指定的 sing-box：C4 的 artifact 就是裸二进制，下载校验完直接落 `bin/sing-box`。
pub fn install_kernel<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    panel: Option<&Panel>,
    m: &Manifest,
) -> Result<bool> {
    let file = format!("sing-box-linux-{}", arch_suffix());
    let srcs = sources(panel, m, &file)?;
    let (_src, bin) = fetch_verified(net, &srcs, &m.artifact(&file)?.sha256)?;
    sys.mkdir_p(&paths.bin_dir())?;
    sys.write(&paths.singbox(), &bin, 0o755)?;
    Ok(true)
}

/// 首次运行没有内核时装一份；已有则原样返回（不联网）。
pub fn ensure_kernel<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    prof: &Profiles,
) -> Result<bool> {
    if sys.exists(&paths.singbox()) {
        return Ok(false);
    }
    let (_src, m) = fetch_manifest(net, prof.panel.as_ref())?;
    install_kernel(sys, net, paths, prof.panel.as_ref(), &m)
}

/// manifest 里的 bui-c 与盘上 `bin` 是不是**两个不同的构建**，是的话为什么（与服务端
/// `kernels::bui_build_differs` 同一口径，2026-09-13 服务端裁决；版本、架构、路径都由参数给，
/// 与服务端一样参数化）。
///
/// 版本号不是充分判据：rc 通道下 `v4.0.0-rc1` / `rc2` / 正式版的 Cargo 版本号都是同一个
/// `4.0.0`，只按版本号判断的话，已装的 rc6 / rc7 永远收不到同版本的新构建。所以版本相同时
/// 再比一次 sha256：manifest 里 `bui-c-linux-<arch>` 的 vs 盘上二进制的（大小写不敏感）。
///
/// 先看 manifest 有没有本机架构的产物：没有就是 [`SelfReason::MissingAsset`]，版本号再新也换不了。
/// 盘上二进制读不到是 [`SelfReason::Unreadable`]——本来就该给它装一份。
pub fn build_differs<S: Sys>(
    sys: &S,
    m: &Manifest,
    version: &str,
    arch: &str,
    bin: &Path,
) -> SelfReason {
    let Ok(want) = m.artifact(&format!("bui-c-linux-{arch}")) else {
        return SelfReason::MissingAsset;
    };
    if m.version != version {
        return SelfReason::NewVersion;
    }
    match sys.read(bin) {
        Ok(bytes) if sha256_hex(&bytes).eq_ignore_ascii_case(&want.sha256) => SelfReason::Current,
        Ok(_) => SelfReason::Rebuild,
        Err(_) => SelfReason::Unreadable,
    }
}

/// [`build_differs`] 取本机版本、本机架构、`/usr/local/bin/bui-c`，不是 [`SelfReason::Current`]
/// 就算要换。口径与 500c786 相同：读不到本机二进制、manifest 缺本机架构的资产都返回真。
pub fn self_build_differs<S: Sys>(sys: &S, m: &Manifest) -> bool {
    build_differs(sys, m, crate::VERSION, arch_suffix(), Path::new(SELF_BIN)) != SelfReason::Current
}

/// 拿 manifest，与本机比出这次要换什么（只读：不下载产物、不写盘、不拿锁）。
fn assess<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    prof: &Profiles,
) -> Result<(Manifest, Report)> {
    let (src, m) = fetch_manifest(net, prof.panel.as_ref())?;
    let self_reason = build_differs(sys, &m, crate::VERSION, arch_suffix(), Path::new(SELF_BIN));
    let kernel_local = kernel_version(sys, paths);
    let r = Report {
        manifest_source: src,
        manifest_version: m.version.clone(),
        self_outdated: self_reason != SelfReason::Current,
        self_reason,
        kernel_outdated: kernel_local.as_deref() != Some(m.kernels.client_sing_box.as_str()),
        kernel_wanted: m.kernels.client_sing_box.clone(),
        kernel_local,
        ..Report::default()
    };
    Ok((m, r))
}

/// `check_only = true`：只取 manifest 与本机比，不下载、不写盘、不拿锁。
///
/// `check_only = false`：[`fetch`]（锁外下载校验）→ 拿锁（最多等 [`INSTALL_LOCK_WAIT`]）→ [`install`]。
/// 等不到锁时什么都没换。菜单与命令行的安装不走这一支：它们要在等锁时先说一句，所以自己
/// fetch → `cli::take_lock` → install；每日自更新只试一次锁（spec §8.3）。
///
/// 自身按 [`build_differs`]（版本不同，或同版本不同构建，或读不到本机二进制）决定换不换；manifest
/// 缺本机架构的 bui-c 时跳过自身、内核照换（spec §8.1）。内核维持只按版本比对。
/// 版本不做 semver 排序：manifest 是唯一权威，降级也由主理人改 manifest 完成
/// （`bui upgrade --rollback` 是服务端能力，见 C5）。
pub fn run<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    prof: &Profiles,
    check_only: bool,
) -> Result<Report> {
    if check_only {
        return assess(sys, net, paths, prof).map(|(_, r)| r);
    }
    let staged = fetch(sys, net, paths, prof)?;
    let Some(g) = lock::acquire(sys, paths, How::Wait(INSTALL_LOCK_WAIT))? else {
        return Err(Error::msg(
            "另一个 bui-c 操作还没结束，这次什么都没改，稍后再试",
        ));
    };
    install(sys, paths, prof, staged, &g)
}

/// [`run`] 拿锁最多等多久：与菜单、命令行同一个 15 秒（spec §8.3）。
pub const INSTALL_LOCK_WAIT: Duration = Duration::from_secs(15);

/// [`fetch`] 下载并校验好、还没装的这次更新。二进制只在内存里；`report` 的 `*_updated` 与
/// `restarted` 都还是假，由 [`install`] 填。
pub struct Staged {
    /// 与 `check_only` 同一份结论（原因、内核要不要换、版本）
    pub report: Report,
    /// 要换上的 bui-c（manifest 缺本机架构、或已是那一份构建时为 `None`）
    self_bin: Option<Vec<u8>>,
    /// manifest 里本机架构 bui-c 的 sha256（`self_bin` 为 `None` 时也是 `None`）：install 跳过自身时
    /// 拿它看别处装上的是不是同一份
    self_want: Option<String>,
    /// 要换上的 sing-box（内核已是 manifest 要的版本时为 `None`）
    kernel_bin: Option<Vec<u8>>,
    /// 这份下载出自哪个 manifest 版本（install 记日志用）
    manifest_version: String,
    /// 开始 fetch 时（取 manifest、下载之前）盘上 bui-c 的 sha256（读不到为 `None`）：install 进锁后
    /// 比一次，变了就不盖
    self_sha: Option<String>,
}

/// 手写：两份二进制几十 MB，派生的 Debug 会把字节全打出来。
impl std::fmt::Debug for Staged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staged")
            .field("report", &self.report)
            .field("self_bin", &self.self_bin.as_ref().map(Vec::len))
            .field("self_want", &self.self_want)
            .field("kernel_bin", &self.kernel_bin.as_ref().map(Vec::len))
            .field("manifest_version", &self.manifest_version)
            .field("self_sha", &self.self_sha)
            .finish()
    }
}

/// 更新的第一段（spec §8.3「update 拆成两段」）：取 manifest，把要换的 bui-c 与 sing-box 下载、
/// 校验到内存。**不写盘、不持锁、不碰 systemd**——这一段最长要等几分钟（每个来源 120 秒），
/// 巡检或菜单拿着锁等它，另一个会话就要等锁超时。
///
/// 两份都下载校验通过才返回：内核那份失败时自身也不换（以前是先换上自身、再报内核失败）。
pub fn fetch<S: Sys, N: Net>(sys: &S, net: &N, paths: &Paths, prof: &Profiles) -> Result<Staged> {
    // 取样在取 manifest 与下载**之前**：install 进锁后比的是「开始这次更新时」与「进锁时」，
    // 下载那几分钟里别处装上的也算换过（内核那道闸的 `kernel_local` 同样在下载之前读）。
    // 放到下载之后取样，取到的就是别处刚装上的那份，比对相等，旧下载照盖（审查 T12b I1）。
    let self_sha = disk_sha(sys, Path::new(SELF_BIN));
    let (m, report) = assess(sys, net, paths, prof)?;
    let panel = prof.panel.as_ref();
    let self_file = format!("bui-c-linux-{}", arch_suffix());
    let (self_bin, self_want) = match report.self_reason {
        SelfReason::Current => (None, None),
        // 没有可下载的：不在这里报错，内核照换。命令行 `bui-c update` 事后仍以失败退出（cli.rs）
        SelfReason::MissingAsset => {
            tracing::warn!(
                file = %self_file,
                "manifest 里没有本机架构的 bui-c，这次跳过 bui-c、只看内核"
            );
            (None, None)
        }
        SelfReason::NewVersion | SelfReason::Rebuild | SelfReason::Unreadable => {
            let srcs = sources(panel, &m, &self_file)?;
            let want = m.artifact(&self_file)?.sha256.clone();
            (Some(fetch_verified(net, &srcs, &want)?.1), Some(want))
        }
    };
    let kernel_bin = if report.kernel_outdated {
        let file = format!("sing-box-linux-{}", arch_suffix());
        let srcs = sources(panel, &m, &file)?;
        Some(fetch_verified(net, &srcs, &m.artifact(&file)?.sha256)?.1)
    } else {
        None
    };
    Ok(Staged {
        report,
        self_bin,
        self_want,
        kernel_bin,
        manifest_version: m.version,
        self_sha,
    })
}

fn disk_sha<S: Sys>(sys: &S, path: &Path) -> Option<String> {
    sys.read(path).ok().map(|b| sha256_hex(&b))
}

/// 换了内核之后要不要重启代理（D16）：主单元文件在，**并且**有活动节点。只看单元文件的话，删光节点
/// 时单元文件没删干净，就会用旧的 `config.json` 把删掉的节点拉起来；新机器还没导入过节点，单元文件
/// 都没写，第一次 apply 会把它拉起来。菜单提示「代理重启几秒」与 [`install`] 共用这一个判断。
pub fn restarts_on_kernel_swap<S: Sys>(sys: &S, paths: &Paths, prof: &Profiles) -> bool {
    sys.exists(&paths.unit(UNIT_MAIN)) && prof.active_profile().is_some()
}

/// 更新的第二段：替换自身、写内核，换了内核且 [`restarts_on_kernel_swap`] 才重启。持锁调用：
/// `LockGuard` 由顶层入口拿，这里只收下凭证（spec §0.2 R11）；`prof` 要是拿锁之后重读的那份。
///
/// 下载在锁外，从 fetch 开始到拿到锁之间别的 bui-c 可能已经换过（菜单里刚装完一版、巡检的自更新）：
/// 盘上的 bui-c 已不是 fetch 开始下载前那一份、内核版本已不是那时读到的，就不拿这份旧下载去盖。
/// 跳过时结论按盘上现在的样子改（`superseded`）：别处装上的正是 manifest 那一份，就是「已是最新」，
/// 否则照旧算要换——调用方拿这份结论落 runtime，★ 与结果行才对得上。
///
/// 失败即返回：先换自身、再写内核，写内核失败时自身已经换上了，不重启（下次检查只剩内核要换）。
pub fn install<S: Sys>(
    sys: &S,
    paths: &Paths,
    prof: &Profiles,
    staged: Staged,
    _: &LockGuard,
) -> Result<Report> {
    let Staged {
        mut report,
        self_bin,
        self_want,
        kernel_bin,
        manifest_version,
        self_sha,
    } = staged;
    tracing::info!(manifest = %manifest_version, "安装更新");
    if let Some(data) = self_bin {
        let on_disk = disk_sha(sys, Path::new(SELF_BIN));
        if on_disk == self_sha {
            replace_self(sys, &data)?;
            report.self_updated = true;
        } else {
            report.superseded = true;
            let same =
                matches!((&on_disk, &self_want), (Some(d), Some(w)) if d.eq_ignore_ascii_case(w));
            if same {
                report.self_reason = SelfReason::Current;
                report.self_outdated = false;
            }
            tracing::warn!(
                same_as_manifest = same,
                "下载期间 bui-c 已被别的操作换过，这次不替换"
            );
        }
    }
    if let Some(data) = kernel_bin {
        let on_disk = kernel_version(sys, paths);
        if on_disk == report.kernel_local {
            sys.mkdir_p(&paths.bin_dir())?;
            sys.write(&paths.singbox(), &data, 0o755)?;
            report.kernel_updated = true;
            if restarts_on_kernel_swap(sys, paths, prof) {
                systemd::restart(sys, UNIT_MAIN)?;
                report.restarted = true;
            }
        } else {
            report.superseded = true;
            report.kernel_outdated = on_disk.as_deref() != Some(report.kernel_wanted.as_str());
            tracing::warn!(
                local = ?on_disk,
                "下载期间 sing-box 内核已被别的操作换过，这次不替换"
            );
            report.kernel_local = on_disk;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys, LandsDuringDownload};
    use crate::paths::Paths;
    use crate::profiles::Panel;
    use crate::testutil::profiles_socks;
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }
    fn panel() -> Panel {
        Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        }
    }

    /// C4 形状：`kernels` 五个键、`artifacts` 是裸二进制的 {url, sha256}，外加两个 C4 的可选字段
    /// 与两个客户端用不到的 artifact 键——都必须被忽略而不是报错。
    fn manifest_json(ver: &str, bui_sha: &str, sb_sha: &str) -> String {
        let dl = format!("https://github.com/{GITHUB_REPO}/releases/download/v{ver}");
        format!(
            r#"{{"version":"{ver}","released":"2026-09-12T00:00:00Z","min_upgrade_from":"4.0.0",
                 "kernels":{{"hysteria":"2.12.2","xray":"26.3.27","sing_box":"1.14.5","caddy":"2.10.2","client_sing_box":"1.14.5"}},
                 "mirrors":["https://mirror.example.com"],
                 "artifacts":{{
                 "bui-linux-amd64":{{"url":"{dl}/bui-linux-amd64","sha256":"{bui_sha}"}},
                 "bui-c-linux-amd64":{{"url":"{dl}/bui-c-linux-amd64","sha256":"{bui_sha}"}},
                 "bui-c-linux-arm64":{{"url":"{dl}/bui-c-linux-arm64","sha256":"{bui_sha}"}},
                 "sing-box-linux-amd64":{{"url":"{dl}/sing-box-linux-amd64","sha256":"{sb_sha}"}},
                 "sing-box-linux-arm64":{{"url":"{dl}/sing-box-linux-arm64","sha256":"{sb_sha}"}}}}}}"#
        )
    }

    fn manifest(ver: &str) -> Manifest {
        serde_json::from_str(&manifest_json(ver, &"a".repeat(64), &"b".repeat(64))).unwrap()
    }

    #[test]
    fn source_order_is_panel_then_manifest_url_then_mirrors() {
        let m = manifest("4.0.1");
        let s = sources(Some(&panel()), &m, "bui-c-linux-amd64").unwrap();
        let names: Vec<&str> = s.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["面板", "GitHub", "镜像 1"]);
        assert_eq!(
            s[0].1,
            "https://panel.example.com/packages/bui-c-linux-amd64"
        );
        // 第二个源就是 manifest 给的 url，客户端不自己拼 Release 路径
        assert_eq!(
            s[1].1,
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/bui-c-linux-amd64"
        );
        assert_eq!(s[2].1, format!("https://mirror.example.com/{}", s[1].1));
        // 没有面板时只剩 manifest url + 镜像
        assert_eq!(sources(None, &m, "bui-c-linux-amd64").unwrap().len(), 2);
        // manifest 没给镜像 → 不编造域名（v3 硬编码的 ghproxy 系已失效）
        let mut m2 = m.clone();
        m2.mirrors.clear();
        assert_eq!(
            sources(Some(&panel()), &m2, "bui-c-linux-amd64")
                .unwrap()
                .len(),
            2
        );
        // 产物不在 manifest 里 → 报错，不去猜 URL
        assert!(sources(Some(&panel()), &m, "bui-c-linux-riscv64").is_err());
    }

    #[test]
    fn a_persisted_plain_http_panel_is_never_an_update_source() {
        // 旧版本（K1 之前）经 `--sub` 或 http 面板导入时会把明文地址写进 profiles.json。
        // 这是 root 自更新的首选来源：明文就等于把替换 /usr/local/bin/bui-c 的权力交给中间人。
        let http = Panel {
            base_url: "http://panel.example.com".into(),
            username: "alice".into(),
        };
        let m = manifest("4.0.1");
        let names: Vec<String> = sources(Some(&http), &m, "bui-c-linux-amd64")
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(!names.iter().any(|n| n == "面板"), "{names:?}");
        let n = FakeNet::new();
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Text(manifest_json("4.0.1", &"a".repeat(64), &"b".repeat(64))),
        );
        let (src, _) = fetch_manifest(&n, Some(&http)).unwrap();
        assert_eq!(src, "GitHub");
        assert!(
            !n.log()
                .iter()
                .any(|l| l.contains("http://panel.example.com")),
            "明文面板一个请求都不该发：{:?}",
            n.log()
        );
    }

    #[test]
    fn manifest_falls_back_to_the_next_source() {
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Fail("502".into()),
        );
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Text(manifest_json("4.0.1", &"a".repeat(64), &"b".repeat(64))),
        );
        let (src, m) = fetch_manifest(&n, Some(&panel())).unwrap();
        assert_eq!(src, "GitHub");
        assert_eq!(m.version, "4.0.1");
        assert_eq!(
            m.kernels.client_sing_box, "1.14.5",
            "只认 client_sing_box，不认 kernels.sing_box"
        );
        assert_eq!(
            m.artifacts.len(),
            5,
            "C4 里客户端用不到的键也照收进 BTreeMap"
        );
        assert_eq!(
            m.artifacts["sing-box-linux-amd64"].url,
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/sing-box-linux-amd64"
        );
        assert_eq!(m.mirrors, vec!["https://mirror.example.com".to_string()]);
    }

    fn releases_list(json: &str) -> FakeReply {
        FakeReply::Text(json.to_string())
    }

    #[test]
    fn github_latest_404_falls_back_to_the_newest_rc_prerelease() {
        // GitHub 的 releases/latest 不解析预发布：仓库里只有 v4.0.0-rcN 时 latest/download/… 必然 404
        // （2026-09-12 baiyi 真机：v3 面板没有 /packages/manifest.json，回退 latest 又 404，装不了内核）。
        // 服务端 crates/bui/src/kernels/mod.rs::fetch_manifest_with 已实现同样的回退，这里照抄口径。
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Status(404),
        );
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Status(404),
        );
        // 第 1 条是 rc 形状、版本号最大但 prerelease=false（跳过），第 2 条 tag 不匹配（跳过），
        // 剩下的里取版本号最大的 rc6
        n.route(
            RELEASES_API_URL,
            releases_list(
                r#"[{"tag_name":"v4.0.1-rc1","prerelease":false},
                    {"tag_name":"nightly","prerelease":true},
                    {"tag_name":"v4.0.0-rc6","prerelease":true},
                    {"tag_name":"v4.0.0-rc5","prerelease":true}]"#,
            ),
        );
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc6/manifest.json",
            FakeReply::Text(manifest_json("4.0.0", &"a".repeat(64), &"b".repeat(64))),
        );
        let (src, m) = fetch_manifest(&n, Some(&panel())).unwrap();
        assert_eq!(src, "GitHub 预发布 v4.0.0-rc6");
        assert_eq!(m.version, "4.0.0");
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
        let n = FakeNet::new();
        n.route(RELEASES_API_URL, releases_list(list));
        latest_rc_tag(&n).unwrap()
    }

    #[test]
    fn the_rc_channel_picks_the_highest_version_not_the_first_listed() {
        // 取「第一个」会拿到 rc9：rc9 上的机器 `bui-c update` 永远说「已最新」
        assert_eq!(
            rc_from(RELEASES_JSON_OBSERVED).as_deref(),
            Some("v4.0.0-rc10")
        );
        let n = FakeNet::new();
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Status(404),
        );
        n.route(RELEASES_API_URL, releases_list(RELEASES_JSON_OBSERVED));
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc10/manifest.json",
            FakeReply::Text(manifest_json("4.0.0", &"a".repeat(64), &"b".repeat(64))),
        );
        let (src, _) = fetch_manifest(&n, None).unwrap();
        assert_eq!(src, "GitHub 预发布 v4.0.0-rc10");
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
    fn github_latest_404_without_any_prerelease_stays_a_clear_error() {
        let n = FakeNet::new();
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Status(404),
        );
        n.route(
            RELEASES_API_URL,
            releases_list(r#"[{"tag_name":"v3.9.9","prerelease":false}]"#),
        );
        let e = fetch_manifest(&n, None).unwrap_err();
        assert!(e.to_string().contains("HTTP 404"), "{e}");
        assert!(
            e.to_string().contains("预发布"),
            "要说清楚列表里也没有预发布：{e}"
        );
    }

    #[test]
    fn github_latest_404_and_releases_list_down_mentions_both() {
        let n = FakeNet::new();
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Status(404),
        );
        n.route(RELEASES_API_URL, FakeReply::Fail("timeout".into()));
        let e = fetch_manifest(&n, None).unwrap_err();
        assert!(e.to_string().contains("HTTP 404"), "{e}");
        assert!(e.to_string().contains("timeout"), "{e}");
    }

    #[test]
    fn a_non_404_github_failure_does_not_consult_the_releases_list() {
        // 断网 / 502 不是「没有正式版」，不该去问列表白等
        let n = FakeNet::new();
        n.route(
            "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json",
            FakeReply::Fail("502".into()),
        );
        assert!(fetch_manifest(&n, None).is_err());
        assert!(
            !n.log().iter().any(|l| l.contains("api.github.com")),
            "{:?}",
            n.log()
        );
    }

    #[test]
    fn is_rc_tag_is_anchored() {
        assert!(is_rc_tag("v4.0.0-rc6"));
        assert!(!is_rc_tag("v4.0.0"));
        assert!(!is_rc_tag("4.0.0-rc1"));
        assert!(!is_rc_tag("v4.0.0-rc"));
        assert!(!is_rc_tag("v4.0.0-rc1x"));
        assert!(!is_rc_tag("nightly"));
    }

    #[test]
    fn manifest_all_sources_down_is_an_error() {
        let n = FakeNet::new();
        let e = fetch_manifest(&n, None).unwrap_err();
        assert!(e.to_string().contains("所有来源"), "{e}");
    }

    #[test]
    fn fetch_verified_rejects_wrong_hash_and_tries_next_source() {
        let good = b"real-binary".to_vec();
        let sha = sha256_hex(&good);
        let n = FakeNet::new();
        n.route(
            "https://a.example.com/x",
            FakeReply::Bytes(b"tampered".to_vec()),
        );
        n.route("https://b.example.com/x", FakeReply::Bytes(good.clone()));
        let srcs = vec![
            ("A".to_string(), "https://a.example.com/x".to_string()),
            ("B".to_string(), "https://b.example.com/x".to_string()),
        ];
        let (src, data) = fetch_verified(&n, &srcs, &sha).unwrap();
        assert_eq!((src.as_str(), data), ("B", good));
        // 全部不匹配 → 报错且不返回任何数据
        let e = fetch_verified(&n, &srcs[..1], &sha).unwrap_err();
        assert!(matches!(e, crate::Error::Verify(_)), "{e}");
    }

    #[test]
    fn sha256_hex_is_lowercase_64_chars() {
        let h = sha256_hex(b"");
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn check_only_reports_without_touching_disk() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json("4.9.9", &"a".repeat(64), &"b".repeat(64))),
        );
        let r = run(&s, &n, &paths(), &prof, true).unwrap();
        assert_eq!(r.manifest_version, "4.9.9");
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false)
        );
        assert!(s.calls().is_empty(), "check_only 不该跑任何命令");
    }

    #[test]
    fn run_replaces_self_atomically_and_keeps_mode_0755() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let bin = b"new-bui-c".to_vec();
        let sha = sha256_hex(&bin);
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json("4.9.9", &sha, &"b".repeat(64))),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/bui-c-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(bin.clone()),
        );
        // 内核已是 manifest 指定版本 → 不重装
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n\nEnvironment: go1.24\n",
        );

        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert!(r.self_updated && !r.kernel_updated);
        assert_eq!(s.get("/usr/local/bin/bui-c").unwrap(), "new-bui-c");
        assert_eq!(s.mode("/usr/local/bin/bui-c"), Some(0o755));
        assert!(
            !s.exists(std::path::Path::new("/usr/local/bin/.bui-c.tmp")),
            "临时文件 rename 走掉了"
        );
    }

    #[test]
    fn run_installs_the_bare_kernel_binary_and_restarts() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let bin = b"ELF-new".to_vec();
        let sha = sha256_hex(&bin);
        // 自身已是最新（版本相同，盘上也正是 manifest 里那份构建）→ 只升内核
        let self_sha = installed_self(&s, "bui-c-current");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(crate::VERSION, &self_sha, &sha)),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/sing-box-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(bin),
        );
        s.put("/opt/bui-c/bin/sing-box", "ELF-old");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.13.19\n",
        );
        // 单元已经装好的机器（没有单元时不重启，见下一条）
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");

        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert!(r.kernel_updated && !r.self_updated);
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), "ELF-new");
        assert_eq!(s.mode("/opt/bui-c/bin/sing-box"), Some(0o755));
        assert!(
            !s.calls().iter().any(|c| c.starts_with("tar")),
            "C4 的产物是裸二进制，客户端不解包"
        );
        assert!(s.called("systemctl restart bui-c.service"));
        assert!(r.restarted);
    }

    /// 新机器（还没导入过节点）上跑 `bui-c update`：内核照装，但没有 bui-c.service 可重启——
    /// 以前在这里以「Unit bui-c.service not found」失败。
    #[test]
    fn run_on_a_machine_without_the_unit_installs_the_kernel_but_does_not_restart() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        prof.panel = Some(panel());
        let bin = b"ELF-new".to_vec();
        let self_sha = installed_self(&s, "bui-c-current");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(crate::VERSION, &self_sha, &sha256_hex(&bin))),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/sing-box-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(bin),
        );
        // 真机上 restart 一个不存在的单元会失败
        s.reply(
            "systemctl restart bui-c.service",
            5,
            "Failed to restart bui-c.service: Unit bui-c.service not found.",
        );

        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert!(r.kernel_updated);
        assert!(!r.restarted);
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), "ELF-new");
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "{:?}",
            s.calls()
        );
    }

    #[test]
    fn run_is_a_no_op_when_everything_matches() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let self_sha = installed_self(&s, "bui-c-current");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(crate::VERSION, &self_sha, &"b".repeat(64))),
        );
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n",
        );
        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false)
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn ensure_kernel_only_installs_when_missing() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let bin = b"ELF-new".to_vec();
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(
                crate::VERSION,
                &"a".repeat(64),
                &sha256_hex(&bin),
            )),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/sing-box-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(bin),
        );
        assert!(ensure_kernel(&s, &n, &paths(), &prof).unwrap());
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), "ELF-new");
        // 已存在 → 直接返回 false，不联网
        let before = n.log().len();
        assert!(!ensure_kernel(&s, &n, &paths(), &prof).unwrap());
        assert_eq!(n.log().len(), before);
    }

    #[test]
    fn kernel_version_parses_singbox_output() {
        let s = FakeSys::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n\nEnvironment: go1.24\n",
        );
        assert_eq!(kernel_version(&s, &paths()).as_deref(), Some("1.14.5"));
        let s2 = FakeSys::new();
        assert_eq!(kernel_version(&s2, &paths()), None, "二进制不存在");
    }

    /// 把盘上的 `/usr/local/bin/bui-c` 摆成 `bytes`，返回它的 sha256——manifest 里写这个值，
    /// 就表示「已装的正是 manifest 里那一份构建」。
    fn installed_self(s: &FakeSys, bytes: &str) -> String {
        s.put(SELF_BIN, bytes);
        sha256_hex(bytes.as_bytes())
    }

    /// 回归（与服务端 `kernels::bui_build_differs` 同口径）：rc 通道下 `v4.0.0-rc1` / `rc2` /
    /// 正式版的 Cargo 版本号都是 `4.0.0`，光比版本号 ⇒ 已装的 rc6 / rc7 永远收不到同版本的新构建。
    /// 版本相同时再比 manifest 里 `bui-c-linux-<arch>` 的 sha256 与盘上 `/usr/local/bin/bui-c` 的。
    #[test]
    fn self_build_differs_falls_back_to_sha_when_the_version_is_unchanged() {
        let s = FakeSys::new();
        let rc2 = sha256_hex(b"BUI-C-rc2");
        let m: Manifest =
            serde_json::from_str(&manifest_json(crate::VERSION, &rc2, &"b".repeat(64))).unwrap();
        installed_self(&s, "BUI-C-rc1");
        assert!(
            self_build_differs(&s, &manifest("9.9.9")),
            "版本不同：一眼就要升"
        );
        assert!(
            self_build_differs(&s, &m),
            "同版本但盘上是另一份构建（rc1 vs rc2）：也要升"
        );
        installed_self(&s, "BUI-C-rc2");
        assert!(!self_build_differs(&s, &m), "同版本同 sha 才是已最新");
        let mut upper = m.clone();
        for a in upper.artifacts.values_mut() {
            a.sha256 = a.sha256.to_uppercase();
        }
        assert!(!self_build_differs(&s, &upper), "sha256 比较不分大小写");
        assert!(
            self_build_differs(&FakeSys::new(), &m),
            "盘上读不到 /usr/local/bin/bui-c：按要升级处理"
        );
        let mut no_asset = m.clone();
        no_asset.artifacts.retain(|k, _| !k.starts_with("bui-c-"));
        assert!(
            self_build_differs(&s, &no_asset),
            "manifest 没有本机架构的 bui-c：按要升级算，报错留给下载那一步"
        );
    }

    #[test]
    fn a_same_version_rebuild_replaces_self_but_leaves_the_kernel_alone() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        installed_self(&s, "bui-c-rc6");
        let rc7 = b"bui-c-rc7".to_vec();
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(
                crate::VERSION,
                &sha256_hex(&rc7),
                &"b".repeat(64),
            )),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/bui-c-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(rc7),
        );
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n",
        );
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");

        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert!(
            r.self_outdated && r.self_updated,
            "同版本新构建也要换：{r:?}"
        );
        assert!(!r.kernel_updated, "内核维持按版本比对：版本没变就不动");
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-rc7");

        // 换完再跑一次：同版本同 sha ⇒ 什么都不做
        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert_eq!(
            (r.self_outdated, r.self_updated, r.kernel_updated),
            (false, false, false)
        );
    }

    #[test]
    fn check_only_flags_a_same_version_rebuild_without_touching_disk() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        installed_self(&s, "bui-c-rc6");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(
                crate::VERSION,
                &sha256_hex(b"bui-c-rc7"),
                &"b".repeat(64),
            )),
        );
        let r = run(&s, &n, &paths(), &prof, true).unwrap();
        assert!(r.self_outdated && !r.self_updated, "{r:?}");
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-rc6", "只检查不换");
        assert!(s.calls().is_empty(), "check_only 不该跑任何命令");
    }

    /// `self_build_differs` 的布尔扩成原因（spec §8.1，服务端跟进 a / b）：版本、架构、盘上路径都由
    /// 参数给，arm64 与 amd64 同一套判定。manifest 缺本机架构的资产优先报 `MissingAsset`——
    /// 说成「有新版」再问 y/N，下载那一步必然失败。
    #[test]
    fn build_differs_names_the_reason_and_covers_arm64() {
        let s = FakeSys::new();
        let bin = Path::new(SELF_BIN);
        s.put(SELF_BIN, "BUI-C-rc6");
        let m: Manifest = serde_json::from_str(&manifest_json(
            "4.0.0",
            &sha256_hex(b"BUI-C-rc7"),
            &"b".repeat(64),
        ))
        .unwrap();
        assert_eq!(
            build_differs(&s, &m, "3.9.9", "amd64", bin),
            SelfReason::NewVersion
        );
        assert_eq!(
            build_differs(&s, &m, "4.0.0", "amd64", bin),
            SelfReason::Rebuild
        );
        assert_eq!(
            build_differs(&s, &m, "4.0.0", "arm64", bin),
            SelfReason::Rebuild
        );
        s.put(SELF_BIN, "BUI-C-rc7");
        assert_eq!(
            build_differs(&s, &m, "4.0.0", "arm64", bin),
            SelfReason::Current
        );
        assert_eq!(
            build_differs(&FakeSys::new(), &m, "4.0.0", "amd64", bin),
            SelfReason::Unreadable
        );
        let mut no = m.clone();
        no.artifacts.retain(|k, _| k != "bui-c-linux-arm64");
        assert_eq!(
            build_differs(&s, &no, "4.0.0", "arm64", bin),
            SelfReason::MissingAsset
        );
        assert_eq!(
            build_differs(&s, &no, "3.9.9", "arm64", bin),
            SelfReason::MissingAsset,
            "版本不同也先看有没有产物：没有就没法换"
        );
        assert_eq!(
            build_differs(&s, &no, "4.0.0", "amd64", bin),
            SelfReason::Current,
            "只缺别的架构不影响本机"
        );
        // 布尔封装的口径不变：不是 Current 就算要换
        assert!(self_build_differs(&FakeSys::new(), &m));
    }

    /// `check_only` 也把原因与「内核要不要换」填上（spec §8.1）：菜单要按它们分开说，
    /// 只换内核的 manifest 也要挂 ★。只读：不写盘。
    #[test]
    fn check_only_fills_the_reason_and_whether_the_kernel_is_outdated() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let self_sha = installed_self(&s, "bui-c-current");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(crate::VERSION, &self_sha, &"b".repeat(64))),
        );
        s.put("/opt/bui-c/bin/sing-box", "ELF-old");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.13.19\n",
        );
        let r = run(&s, &n, &paths(), &prof, true).unwrap();
        assert_eq!(r.self_reason, SelfReason::Current, "{r:?}");
        assert!(!r.self_outdated, "{r:?}");
        assert!(r.kernel_outdated, "{r:?}");
        assert_eq!(r.kernel_wanted, "1.14.5");
        assert_eq!(r.kernel_local.as_deref(), Some("1.13.19"));
        assert_eq!((r.self_updated, r.kernel_updated), (false, false));
        assert_eq!(s.writes("/opt/bui-c/bin/sing-box"), 0, "只检查不装");

        // 盘上没有内核：本机版本读不到，照样算要换
        let s = FakeSys::new();
        installed_self(&s, "bui-c-current");
        let r = run(&s, &n, &paths(), &prof, true).unwrap();
        assert!(r.kernel_outdated && r.kernel_local.is_none(), "{r:?}");
    }

    /// manifest 缺本机架构的 bui-c：没有可下载的，自身跳过；内核照换（spec §8.1：内核也要换时
    /// 只换内核）。以前在自身那一步就报错退出，内核也跟着永远换不了。
    #[test]
    fn run_skips_a_missing_self_asset_but_still_replaces_the_kernel() {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        installed_self(&s, "bui-c-old");
        let kernel = b"ELF-new".to_vec();
        let mut m: Manifest = serde_json::from_str(&manifest_json(
            "9.9.9",
            &"a".repeat(64),
            &sha256_hex(&kernel),
        ))
        .unwrap();
        m.artifacts.retain(|k, _| !k.starts_with("bui-c-"));
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(serde_json::to_string(&m).unwrap()),
        );
        n.route(
            &format!(
                "https://panel.example.com/packages/sing-box-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(kernel),
        );
        s.put("/opt/bui-c/bin/sing-box", "ELF-old");
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.13.19\n",
        );
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");

        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert_eq!(r.self_reason, SelfReason::MissingAsset, "{r:?}");
        assert!(!r.self_updated, "{r:?}");
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-old", "自身原样");
        assert!(r.kernel_updated && r.restarted, "{r:?}");
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), "ELF-new");
        assert!(
            !n.log().iter().any(|l| l.contains("bui-c-linux-")),
            "没有产物就不去下载：{:?}",
            n.log()
        );
    }

    const KERNEL: &str = "/opt/bui-c/bin/sing-box";
    const UNIT: &str = "/etc/systemd/system/bui-c.service";
    const SELF_TMP: &str = "/usr/local/bin/.bui-c.tmp";

    /// fetch / install 用例共用的机器：面板 profile（SOCKS，有活动节点）、盘上 bui-c 是 `bui-c-old`、
    /// 内核 1.13.19、主单元文件在；manifest 9.9.9 自身与内核都要换（`bui-c-new` / `ELF-new`），
    /// 两个产物都能从面板下载。
    fn staged_machine() -> (FakeSys, FakeNet, Profiles) {
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        installed_self(&s, "bui-c-old");
        let (me, kernel) = (b"bui-c-new".to_vec(), b"ELF-new".to_vec());
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(
                "9.9.9",
                &sha256_hex(&me),
                &sha256_hex(&kernel),
            )),
        );
        let arch = arch_suffix();
        n.route(
            &format!("https://panel.example.com/packages/bui-c-linux-{arch}"),
            FakeReply::Bytes(me),
        );
        n.route(
            &format!("https://panel.example.com/packages/sing-box-linux-{arch}"),
            FakeReply::Bytes(kernel),
        );
        s.put(KERNEL, "ELF-old");
        s.reply(
            &format!("{KERNEL} version"),
            0,
            "sing-box version 1.13.19\n",
        );
        s.put(UNIT, "[Unit]");
        (s, n, prof)
    }

    /// 自身、临时文件、内核三处一次都没写过（失败的写也算写过）。
    fn assert_untouched(s: &FakeSys, case: &str) {
        for p in [SELF_BIN, SELF_TMP, KERNEL] {
            assert_eq!(s.writes(p), 0, "{case}：{p} 被写过");
        }
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-old", "{case}");
        assert_eq!(s.get(KERNEL).unwrap(), "ELF-old", "{case}");
    }

    /// spec §8.3「update 拆成两段」：`fetch` 取 manifest、把要换的两份下载校验到内存，**不写盘、不拿锁、
    /// 不碰 systemd**——巡检拿着锁等最长 120 秒的下载，菜单等锁 15 秒就会超时。
    #[test]
    fn fetch_writes_nothing() {
        let (s, n, prof) = staged_machine();
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        let r = &staged.report;
        assert_eq!(
            (r.self_reason, r.kernel_outdated),
            (SelfReason::NewVersion, true),
            "{r:?}"
        );
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false),
            "还没装：{r:?}"
        );
        assert_eq!(r.manifest_version, "9.9.9");
        assert_eq!(staged.self_bin.as_deref(), Some(&b"bui-c-new"[..]));
        assert_eq!(staged.kernel_bin.as_deref(), Some(&b"ELF-new"[..]));
        assert_untouched(&s, "下载成功");
        assert_eq!(
            s.calls(),
            vec![format!("{KERNEL} version")],
            "只读了一次内核版本：不拿锁、不动 systemd"
        );
        let log = n.log();
        assert!(log.iter().any(|l| l.contains("bui-c-linux-")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("sing-box-linux-")), "{log:?}");

        // 内核校验不过（自身那份已经校验通过）：整个 fetch 失败，自身也不能先换上
        let (s, n, prof) = staged_machine();
        n.route(
            &format!(
                "https://panel.example.com/packages/sing-box-linux-{}",
                arch_suffix()
            ),
            FakeReply::Bytes(b"tampered".to_vec()),
        );
        let e = fetch(&s, &n, &paths(), &prof).unwrap_err();
        assert!(matches!(e, Error::Verify(_)), "{e}");
        assert_untouched(&s, "内核校验失败");
        assert!(!s.called("lock"), "{:?}", s.calls());

        // 都是最新：什么都不下载
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut prof = profiles_socks();
        prof.panel = Some(panel());
        let self_sha = installed_self(&s, "bui-c-current");
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(manifest_json(crate::VERSION, &self_sha, &"b".repeat(64))),
        );
        s.put(KERNEL, "ELF");
        s.reply(&format!("{KERNEL} version"), 0, "sing-box version 1.14.5\n");
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        assert!(staged.self_bin.is_none() && staged.kernel_bin.is_none());
        assert_eq!(n.log().len(), 1, "只取了 manifest：{:?}", n.log());
    }

    /// D16：换了内核之后，**主单元文件在、并且有活动节点**才重启。只看单元文件的话，删光节点时
    /// 单元文件没删干净（或并发的删除刚删完 profiles）就会把删掉的节点拉起来。替换本身不受影响。
    #[test]
    fn install_restarts_only_with_a_unit_and_an_active_node() {
        for (unit, active, want) in [
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, false, false),
        ] {
            let case = format!("单元文件在={unit} 有活动节点={active}");
            let (s, n, mut prof) = staged_machine();
            if !unit {
                s.remove_file(Path::new(UNIT)).unwrap();
            }
            if !active {
                prof.active = None;
            }
            assert_eq!(restarts_on_kernel_swap(&s, &paths(), &prof), want, "{case}");
            let staged = fetch(&s, &n, &paths(), &prof).unwrap();
            let r = install(&s, &paths(), &prof, staged, &crate::lock::LockGuard::stub()).unwrap();
            assert!(r.self_updated && r.kernel_updated, "{case}：{r:?}");
            assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-new", "{case}");
            assert_eq!(s.mode(SELF_BIN), Some(0o755), "{case}");
            assert_eq!(s.get(KERNEL).unwrap(), "ELF-new", "{case}");
            assert_eq!(r.restarted, want, "{case}：{r:?}");
            assert_eq!(
                s.called("systemctl restart bui-c.service"),
                want,
                "{case}：{:?}",
                s.calls()
            );
        }

        // 删光之后的 profiles（一个节点都没有）：单元文件还在也不重启
        let (s, n, _) = staged_machine();
        let mut empty = Profiles::new_default();
        empty.panel = Some(panel());
        let staged = fetch(&s, &n, &paths(), &empty).unwrap();
        let r = install(
            &s,
            &paths(),
            &empty,
            staged,
            &crate::lock::LockGuard::stub(),
        )
        .unwrap();
        assert!(r.kernel_updated && !r.restarted, "{r:?}");
        assert!(!s.called("systemctl restart bui-c.service"));

        // 只换自身、内核不动：跑着的 sing-box 没变，有单元有节点也不重启
        let (s, n, prof) = staged_machine();
        s.reply(&format!("{KERNEL} version"), 0, "sing-box version 1.14.5\n");
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        let r = install(&s, &paths(), &prof, staged, &crate::lock::LockGuard::stub()).unwrap();
        assert!(r.self_updated && !r.kernel_updated && !r.restarted, "{r:?}");
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    /// 下载在锁外，下载与拿到锁之间别的 bui-c 可能已经换过（菜单里刚装完一版、巡检的自更新）。
    /// install 进锁后重看一眼：盘上已经不是 fetch 时那一份，就不拿手里这份旧下载去盖。
    #[test]
    fn install_leaves_alone_what_changed_on_disk_after_the_fetch() {
        let (s, n, prof) = staged_machine();
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        s.put(SELF_BIN, "bui-c-newer");
        s.put(KERNEL, "ELF-newer");
        s.reply(&format!("{KERNEL} version"), 0, "sing-box version 1.14.9\n");
        let r = install(&s, &paths(), &prof, staged, &crate::lock::LockGuard::stub()).unwrap();
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-newer", "{r:?}");
        assert_eq!(s.get(KERNEL).unwrap(), "ELF-newer", "{r:?}");
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false),
            "{r:?}"
        );
        assert!(!s.called("systemctl restart bui-c.service"));
        assert_eq!(s.writes(SELF_TMP), 0);
    }

    /// 审查 T12b I1：「盘上已被别处换过就不盖」的两道闸都要覆盖**整个下载窗口**。timer 在 T0 看过、
    /// 开始下载 → 人在菜单里 T1 装好更新的一版 → timer 下完进锁：盘上的 bui-c 与内核都已不是它开始
    /// 下载时的那一份，手里这份旧下载一样都不能盖上去。以前自身那道闸在下载**之后**才取样，取到的
    /// 就是 T1 装上的那份，比对相等，照盖。
    #[test]
    fn install_leaves_alone_what_another_install_put_on_disk_during_the_download() {
        let (s, n, prof) = staged_machine();
        let land = |s: &FakeSys| {
            s.put(SELF_BIN, "bui-c-newer");
            s.put(KERNEL, "ELF-newer");
            s.reply(&format!("{KERNEL} version"), 0, "sing-box version 1.14.9\n");
        };
        let race = LandsDuringDownload::new(&s, &n, "/sing-box-linux-", &land);
        let staged = fetch(&race, &n, &paths(), &prof).unwrap();
        assert!(
            staged.self_bin.is_some() && staged.kernel_bin.is_some(),
            "两份都下载了：{staged:?}"
        );
        let r = install(
            &race,
            &paths(),
            &prof,
            staged,
            &crate::lock::LockGuard::stub(),
        )
        .unwrap();
        assert!(race.landed(), "别处的安装没落下来，用例没测到东西");
        assert_eq!(s.get(SELF_BIN).unwrap(), "bui-c-newer", "{r:?}");
        assert_eq!(s.get(KERNEL).unwrap(), "ELF-newer", "{r:?}");
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false),
            "{r:?}"
        );
        assert_eq!(s.writes(SELF_TMP), 0);
        assert_eq!(s.writes(KERNEL), 0);
        assert!(!s.called("systemctl restart bui-c.service"));
        // 别处装上的不是 manifest 要的那一份：结论照旧是「要换」，★ 该挂；内核按现在盘上的版本说
        assert_eq!(
            (r.self_reason, r.self_outdated, r.kernel_outdated),
            (SelfReason::NewVersion, true, true),
            "{r:?}"
        );
        assert_eq!(r.kernel_local.as_deref(), Some("1.14.9"), "{r:?}");
    }

    /// 审查 T12b r2 I1：下载期间别处装上的**正是**这次要装的那一份（菜单里刚装完同一版）。两道闸照样
    /// 不盖，但结论要跟着改成「已是最新」——还说 NewVersion / 内核过期的话，三个入口拿它落 runtime，
    /// 会把 ★ 重新点亮，与结果行「没有需要更新的」自相矛盾。
    #[test]
    fn install_skipping_the_same_update_reports_nothing_pending() {
        let (s, n, prof) = staged_machine();
        let land = |s: &FakeSys| {
            s.put(SELF_BIN, "bui-c-new");
            s.put(KERNEL, "ELF-new");
            s.reply(&format!("{KERNEL} version"), 0, "sing-box version 1.14.5\n");
        };
        let race = LandsDuringDownload::new(&s, &n, "/sing-box-linux-", &land);
        let staged = fetch(&race, &n, &paths(), &prof).unwrap();
        let r = install(
            &race,
            &paths(),
            &prof,
            staged,
            &crate::lock::LockGuard::stub(),
        )
        .unwrap();
        assert!(race.landed(), "别处的安装没落下来，用例没测到东西");
        assert_eq!(s.writes(SELF_TMP), 0, "{r:?}");
        assert_eq!(s.writes(KERNEL), 0, "{r:?}");
        assert!(!s.called("systemctl restart bui-c.service"));
        assert_eq!(
            (r.self_updated, r.kernel_updated, r.restarted),
            (false, false, false),
            "{r:?}"
        );
        assert_eq!(
            (r.self_reason, r.self_outdated, r.kernel_outdated),
            (SelfReason::Current, false, false),
            "{r:?}"
        );
        assert_eq!(r.kernel_local.as_deref(), Some("1.14.5"), "{r:?}");
        assert!(r.superseded && !r.counts_as_update(), "{r:?}");

        // manifest 里的 sha256 是大写：与 `build_differs` 同一口径，大小写不敏感
        let (s, _, prof) = staged_machine();
        s.put(SELF_BIN, "bui-c-new");
        let staged = Staged {
            report: Report {
                self_reason: SelfReason::NewVersion,
                self_outdated: true,
                ..Report::default()
            },
            self_bin: Some(b"bui-c-new".to_vec()),
            self_want: Some(sha256_hex(b"bui-c-new").to_ascii_uppercase()),
            kernel_bin: None,
            manifest_version: "9.9.9".into(),
            self_sha: Some(sha256_hex(b"bui-c-old")),
        };
        let r = install(&s, &paths(), &prof, staged, &crate::lock::LockGuard::stub()).unwrap();
        assert_eq!(s.writes(SELF_TMP), 0, "{r:?}");
        assert_eq!(
            (r.self_reason, r.self_outdated, r.superseded),
            (SelfReason::Current, false, true),
            "{r:?}"
        );

        // 什么都没跳过：换上了、或本来就没什么要换，都算更新过
        let (s, n, prof) = staged_machine();
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        let r = install(&s, &paths(), &prof, staged, &crate::lock::LockGuard::stub()).unwrap();
        assert!(
            r.self_updated && !r.superseded && r.counts_as_update(),
            "{r:?}"
        );
        let idle = Report::default();
        assert!(idle.counts_as_update(), "{idle:?}");
        let half = Report {
            kernel_updated: true,
            superseded: true,
            ..Report::default()
        };
        assert!(
            half.counts_as_update(),
            "自身被别处换过、内核换上了：{half:?}"
        );
    }

    /// 审查 T12b M5：排查「为什么没盖」时要看 fetch 那一刻盘上 bui-c 的 sha；两份二进制只打长度。
    #[test]
    fn staged_debug_shows_the_sampled_self_sha_but_not_the_bytes() {
        let (s, n, prof) = staged_machine();
        let staged = fetch(&s, &n, &paths(), &prof).unwrap();
        let dbg = format!("{staged:?}");
        let old = sha256_hex(b"bui-c-old");
        assert!(dbg.contains("self_sha") && dbg.contains(&old), "{dbg}");
        assert!(!dbg.contains("98, 117, 105"), "二进制按字节打出来了：{dbg}");
    }

    /// `run(check_only = false)` = fetch + 拿锁（等 15 秒）+ install：下载在拿锁之前，重启在锁里。
    /// 锁一直被占：下载照做，等满 15 秒放弃，盘上什么都没换。
    #[test]
    fn run_downloads_before_the_lock_and_restarts_inside_it() {
        let (s, n, prof) = staged_machine();
        let r = run(&s, &n, &paths(), &prof, false).unwrap();
        assert!(r.self_updated && r.kernel_updated && r.restarted, "{r:?}");
        let calls = s.calls();
        let at = |c: &str| {
            calls
                .iter()
                .position(|x| x == c)
                .unwrap_or_else(|| panic!("没有 {c}：{calls:?}"))
        };
        assert!(at(&format!("{KERNEL} version")) < at("lock"), "{calls:?}");
        assert!(
            at("lock") < at("systemctl restart bui-c.service"),
            "{calls:?}"
        );
        assert!(
            at("systemctl restart bui-c.service") < at("unlock"),
            "{calls:?}"
        );
        assert!(s.sleeps().is_empty(), "{:?}", s.sleeps());

        let (s, n, prof) = staged_machine();
        s.lock_busy(u32::MAX);
        let e = run(&s, &n, &paths(), &prof, false).unwrap_err();
        assert!(e.to_string().contains("稍后再试"), "{e}");
        assert!(
            n.log().iter().any(|l| l.contains("sing-box-linux-")),
            "下载在锁外：{:?}",
            n.log()
        );
        assert_untouched(&s, "锁一直被占");
        assert!(!s.called("systemctl restart bui-c.service"));
        assert_eq!(s.sleeps().iter().sum::<u64>(), 15_000);
    }
}
