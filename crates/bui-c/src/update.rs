//! `update`：多源取 manifest（面板 → GitHub latest）、多源取产物（面板 → manifest 的
//! `url` → `manifest.mirrors`），sha256 校验后自替换 `/usr/local/bin/bui-c` 与
//! `bin/sing-box`。manifest 形状 = 总纲 C4：`artifacts` 是扁平表，每个 `url` 指向
//! **裸二进制**（上游 tar.gz 由 P5 的 Actions 解包后重新上传），所以客户端不解包、不调 `tar`。

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
/// GitHub 的 releases 列表（无凭据，只取最近 10 条）：`releases/latest` 不解析预发布，
/// 预发布通道靠它找最新的 rc tag。与服务端 `crates/bui/src/kernels/mod.rs::RELEASES_API_URL` 同口径。
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=10";
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
    /// manifest 里的 bui-c 与已装的不是同一份构建（[`self_build_differs`]）；`check_only` 也会填
    pub self_outdated: bool,
    /// `/usr/local/bin/bui-c` 被替换
    pub self_updated: bool,
    /// `bin/sing-box` 被替换
    pub kernel_updated: bool,
    pub restarted: bool,
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

/// releases 列表里最新的预发布 rc tag：列表按创建时间倒序，取第一个 `prerelease=true`
/// 且 tag 形如 `v<x.y.z>-rcN` 的。没有就 `None`。
fn latest_rc_tag<N: Net>(net: &N) -> Result<Option<String>> {
    let body = net.text(RELEASES_API_URL, MANIFEST_TIMEOUT)?;
    let list: Vec<GhRelease> = serde_json::from_str(&body)
        .map_err(|e| Error::parse("GitHub releases 列表", e.to_string()))?;
    Ok(list
        .into_iter()
        .find(|r| r.prerelease && is_rc_tag(&r.tag_name))
        .map(|r| r.tag_name))
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

/// manifest 里的 bui-c 与盘上 `/usr/local/bin/bui-c` 是不是**两个不同的构建**（与服务端
/// `kernels::bui_build_differs` 同一口径，2026-09-13 服务端裁决）。
///
/// 版本号不是充分判据：rc 通道下 `v4.0.0-rc1` / `rc2` / 正式版的 Cargo 版本号都是同一个
/// `4.0.0`，只按版本号判断的话，已装的 rc6 / rc7 永远收不到同版本的新构建。所以版本相同时
/// 再比一次 sha256：manifest 里 `bui-c-linux-<arch>` 的 vs 盘上二进制的（大小写不敏感）。
///
/// 盘上二进制读不到按「要升级」处理——本来就该给它装一份；manifest 缺本机架构的资产同样按
/// 要升级返回，真正的报错留给下载那一步（[`sources`] 会说清缺哪个产物）。
pub fn self_build_differs<S: Sys>(sys: &S, m: &Manifest) -> bool {
    if m.version != crate::VERSION {
        return true;
    }
    let Ok(want) = m.artifact(&format!("bui-c-linux-{}", arch_suffix())) else {
        return true;
    };
    match sys.read(Path::new(SELF_BIN)) {
        Ok(bytes) => !sha256_hex(&bytes).eq_ignore_ascii_case(&want.sha256),
        Err(_) => true,
    }
}

/// 自身按 [`self_build_differs`]（版本不同，或同版本不同构建）决定换不换；内核维持只按版本比对。
/// 版本不做 semver 排序：manifest 是唯一权威，降级也由主理人改 manifest 完成
/// （`bui upgrade --rollback` 是服务端能力，见 C5）。
pub fn run<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    prof: &Profiles,
    check_only: bool,
) -> Result<Report> {
    let (src, m) = fetch_manifest(net, prof.panel.as_ref())?;
    let mut r = Report {
        manifest_source: src,
        manifest_version: m.version.clone(),
        self_outdated: self_build_differs(sys, &m),
        ..Report::default()
    };
    if check_only {
        return Ok(r);
    }

    if r.self_outdated {
        let file = format!("bui-c-linux-{}", arch_suffix());
        let srcs = sources(prof.panel.as_ref(), &m, &file)?;
        let (_s, data) = fetch_verified(net, &srcs, &m.artifact(&file)?.sha256)?;
        replace_self(sys, &data)?;
        r.self_updated = true;
    }

    if kernel_version(sys, paths).as_deref() != Some(m.kernels.client_sing_box.as_str()) {
        install_kernel(sys, net, paths, prof.panel.as_ref(), &m)?;
        r.kernel_updated = true;
        // 新机器还没导入过节点，单元文件都没写：没有可重启的，第一次 apply 会把它拉起来
        if sys.exists(&paths.unit(UNIT_MAIN)) {
            systemd::restart(sys, UNIT_MAIN)?;
            r.restarted = true;
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys};
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
        // 列表按创建时间倒序；第 1 条是 rc 形状但 prerelease=false（跳过），第 2 条 tag 不匹配（跳过）
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
}
