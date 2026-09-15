//! `bui import-v3`（只生成 `state.json`）与 `bui install --import-v3` 用的 v3 卸载流程。
//!
//! 卸载的顺序是硬要求（2026-09-12 裁决）：先把发行版 Caddy 的 ACME 账号与证书搬进 v4 的数据
//! 目录，再停它；整个 `uninstall_v3` 又必须排在**对账之前**（v3 的 `b-ui-admin` 还占着 `:8080`
//! 时，对账 start 的 `b-ui.service` 首启 bind 失败会进 `Restart=always` 循环）。
//!
//! 2026-09-13 裁决又在最前面加了一步：把 `/etc/caddy/Caddyfile` 里**除 b-ui 面板块以外**的
//! 顶层站点块原样搬进 `<base>/caddy/sites/imported-from-v3.caddy`，并对渲染出的新 Caddyfile
//! 跑一次 `caddy validate`。这一步过不了就整体中止（发行版 caddy 照旧在跑、`/etc/caddy`
//! 一个字节没动），生产机上那些托管着别人站点的 Caddy 才不会因为升级 v4 而集体掉线。

use crate::sys::Host;
use bui_schema::paths::Paths;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 要停用并删除的 v3 单元。**不含 `b-ui-relay.service`**：v4 原地重写同名单元，
/// 由内容 diff 决定是否重启；把它列进来会在装好 v4 之后又把 relay 卸掉，住宅两个节点一起断。
pub const V3_UNITS: [&str; 7] = [
    "b-ui-admin.service",
    "b-ui-cert-sync.timer",
    "b-ui-cert-sync.service",
    "hy2-watchdog.timer",
    "hy2-watchdog.service",
    "b-ui-resi-health.timer",
    "b-ui-resi-health.service",
];

/// v3 的 shell / Node / 引导文件与它自带的 sing-box 二进制：直接删。
/// 每一项都是从仓库里的 v3 脚本核实过的，不是凭记忆列的：
/// `sing-box` —— v3 的 relay 二进制在 **`${BASE_DIR}/sing-box`**（顶层，不在 `bin/`；
///   `server/residential-helper.sh:35`），v4 的那份在 `<base>/bin/sing-box`，所以删顶层这个不影响 v4；
/// `cert-check.sh` —— `server/core.sh:1170-1217` 写出并 `chmod +x`，`update.sh:2044-2066` 还给它挂了
///   一条 12 小时 cron（cron 行由 [`filter_cron`] 一并清掉）。
/// 少一项就会在 `<base>` 顶层留一个永久 `stray_file`：每 10 分钟一条漂移 → `/api/health` 恒
/// `degraded` → M1 的「体检无漂移」不可达（spec §2.3 + §9）。
pub const V3_FILES: [&str; 13] = [
    "core.sh",
    "update.sh",
    "b-ui-cli.sh",
    "residential-helper.sh",
    "resi-health.sh",
    "hy2-watchdog.sh",
    "cert-sync.sh",
    "cert-check.sh",
    "hy2-portjump-cleanup.sh",
    "install-key.txt",
    "b-ui-client.sh",
    "version.json",
    "sing-box",
];

/// v3 的状态文件：移进 `<base>/v3-backup/`（0700）而不是删——出问题要能回查，
/// 留在原地则被 §2.2 的漂移扫描永久报告。同样逐条核实过：
/// `port-hopping.json`（`core.sh:1422` 写，`update.sh:801-808/1315-1321`、`web/server.js:434` 读）、
/// `masquerade.json`（`core.sh:773` 写，`web/server.js:2567` 读）、
/// `server_ip.txt`（`web/server.js:73` 读，P0 的 v3 fixture 就带一个）。
/// 前两个不含秘密但含节点配置，第三个是公网 IP：一起归档，既不丢线索也不留漂移。
pub const V3_STATE_FILES: [&str; 9] = [
    "users.json",
    "reality-keys.json",
    "residential-proxy.json",
    "admin.env",
    ".resi-health-state.json",
    ".relay.lock",
    "port-hopping.json",
    "masquerade.json",
    "server_ip.txt",
];

/// v3 的迁移块与 CLI 留在 `<base>` 顶层的**备份 / 临时文件**的基名。只认这六个基名，
/// 别人的文件一概不碰。核实来源：
/// `server/update.sh:682/703/734`（`*.bak.v360.<ts>`，三个 config）、`:774`（`*.bak.<ts>`）、
/// `:858`（`*.bak.broken-<ts>`）、`:1660`（`config.yaml.bak.v357.<ts>`）、
/// `:1674`（`xray-config.json.bak.<ts>`）、`:1709`（`xray-config.json.bak.v359.<ts>`）、
/// `server/b-ui-cli.sh:939/963`（`config.yaml.bak.obfs.<ts>`）；
/// `.tmp` 来自 `core.sh`/`update.sh` 的 `config.yaml.tmp` / `config.yaml.v357.tmp` /
/// `xray-config.json.tmp` 与 `residential-helper.sh:455/500/565` 的原子写中断残留。
/// `*.bak.*` 里有 HY2 明文密码与 UUID → **归档**进 `v3-backup/`；`.tmp` 是半成品 → **删除**。
pub const V3_LEFTOVER_PREFIXES: [&str; 6] = [
    "config.yaml",
    "config-residential.yaml",
    "xray-config.json",
    "singbox-relay.json",
    "residential-proxy.json",
    ".resi-health-state.json",
];

/// 发行版 Caddy（v3 用的那个）的数据目录：ACME 账号与已签证书都在它下面。
pub const V3_CADDY_DATA: &str = "/var/lib/caddy/.local/share/caddy";

/// 发行版 Caddy 的主配置（v3 `server/core.sh:864` 写它）。**只读**：导入全程不改、不删它。
pub const V3_CADDYFILE: &str = "/etc/caddy/Caddyfile";

/// 导出文件开头加的说明（之后是一字节未改的外部站点块）。
pub const SITES_EXPORT_HEADER: &str = "\
# 由 bui import-v3 从 /etc/caddy/Caddyfile 原样导出（除 b-ui 面板站点块以外的全部顶层块）。
# 证书等绝对路径一个字节都没改；/etc/caddy 下的原文件也没删。
# 这个文件归你管：bui 不会再改写、覆盖或删除它。
";

/// 一份 Caddyfile 里的一个顶层段落。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaddyBlock {
    /// 站点地址：`{` 之前的代码文本（剥掉注释、首尾空白），全局选项块是空串。
    pub header: String,
    /// 块内代码（剥掉注释），只用来判定；写文件用的是 [`CaddyBlock::text`]。
    pub body: String,
    /// 这一段的**原样字节**：前导注释 / 空行 + 站点块本身 + 行尾换行。
    /// 所有段落的 `text` 顺序拼起来 == 原文。
    pub text: String,
    /// 这一段里出现过配对的花括号块体（末尾纯注释 / 空行的尾巴没有）。
    pub braced: bool,
}

impl CaddyBlock {
    /// 要不要搬家：有块体的、或者没块体但有站点地址的（宁可多搬，绝不丢别人的站点）。
    fn is_site(&self) -> bool {
        self.braced || !self.header.is_empty()
    }

    /// 全局选项块：有块体但没有站点地址。
    fn is_global_options(&self) -> bool {
        self.braced && self.header.is_empty()
    }

    /// 站点地址列表（`a.com, b.com` / 空白分隔都认）。
    fn addresses(&self) -> Vec<&str> {
        self.header
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// 剥掉一行里的注释：`#` 只有在行首或前面是空白、且不在引号里时才起注释作用（Caddyfile 语义）。
fn code_part(line: &str) -> &str {
    let b = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'`' {
                    quote = Some(c);
                } else if c == b'#' && (i == 0 || b[i - 1].is_ascii_whitespace()) {
                    return &line[..i];
                }
            }
        }
        i += 1;
    }
    line
}

/// 把一份 Caddyfile 切成顶层段落（2026-09-13 裁决的 import 通道要按块搬家）。
///
/// 规则：段落是**连续**的——前一段结束的下一行就是后一段的开始，所以前导注释与空行归后面那个块，
/// 拼回来一定等于原文（这就是「字节不改」的实现方式）。花括号只在剥掉注释与引号之后才计数，
/// 段落只在**行尾**且深度回到 0 时收尾（一行写多个块时宁可整行留在一段里，也不切断任何人的站点）。
pub fn split_caddy_blocks(text: &str) -> Vec<CaddyBlock> {
    let mut out = Vec::new();
    let (mut chunk, mut header, mut body) = (String::new(), String::new(), String::new());
    let mut depth = 0usize;
    let mut opened = false;
    for line in text.split_inclusive('\n') {
        chunk.push_str(line);
        for ch in code_part(line).chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                    if depth == 1 {
                        continue; // 最外层的花括号本身不进 header / body
                    }
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        continue;
                    }
                }
                _ => {}
            }
            if depth == 0 {
                header.push(ch);
            } else {
                body.push(ch);
            }
        }
        if opened && depth == 0 {
            out.push(CaddyBlock {
                header: header.trim().to_string(),
                body: std::mem::take(&mut body),
                text: std::mem::take(&mut chunk),
                braced: true,
            });
            header.clear();
            opened = false;
        }
    }
    // 尾巴：文件末尾的注释 / 空行，或没闭合的残句（原样保留，不丢字节）
    if !chunk.is_empty() {
        out.push(CaddyBlock {
            header: header.trim().to_string(),
            body,
            text: chunk,
            braced: false,
        });
    }
    out
}

/// 是不是 b-ui 的面板站点块？判定口径（2026-09-13 裁决）：
/// **站点地址里有一个等于 `domain`**（`https://` 前缀与 `:443` 之类的端口后缀忽略）
/// **且块内有一条 `reverse_proxy … :<admin_port>`**。两条都满足才算，避免把运营自己反代
/// 别的服务的同域块误吞。
pub fn is_bui_panel_block(block: &CaddyBlock, domain: &str, admin_port: u16) -> bool {
    let addr_hit = block.addresses().into_iter().any(|a| {
        let a = a
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        a == domain || a.rsplit_once(':').map(|(h, _)| h) == Some(domain)
    });
    if !addr_hit {
        return false;
    }
    let port = format!(":{admin_port}");
    block.body.lines().any(|l| {
        let mut w = l.split_whitespace();
        w.next() == Some("reverse_proxy") && w.any(|t| t.ends_with(&port))
    })
}

/// [`external_caddy_sites`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SitesExport {
    /// 要写进 `<base>/caddy/sites/imported-from-v3.caddy` 的内容
    /// （[`SITES_EXPORT_HEADER`] + 原样的外部块）；没有外部站点时是空串。
    pub text: String,
    /// 导出的站点地址行（日志用）。
    pub addresses: Vec<String>,
    /// 丢掉了全局选项块：被 import 的文件里不允许出现它，照搬会让 `caddy validate` 直接报错。
    pub dropped_global: bool,
}

/// 从一份 Caddyfile 文本里取出**除 b-ui 面板块以外**的所有顶层站点块，原样拼接。
pub fn external_caddy_sites(text: &str, domain: &str, admin_port: u16) -> SitesExport {
    let mut out = SitesExport::default();
    let mut kept = String::new();
    for b in split_caddy_blocks(text) {
        if !b.is_site() {
            continue; // 纯注释 / 空行的尾巴
        }
        if b.is_global_options() {
            out.dropped_global = true;
            continue;
        }
        if is_bui_panel_block(&b, domain, admin_port) {
            continue;
        }
        kept.push_str(&b.text);
        out.addresses.push(b.header.clone());
    }
    // 首尾的空行是块之间的分隔，不是谁的内容：只在拼好的整体两端修掉
    let kept = kept.trim_matches('\n');
    if !kept.is_empty() {
        out.text = format!("{SITES_EXPORT_HEADER}{kept}\n");
    }
    out
}

/// 外部站点通道的迁移（2026-09-13 裁决），**必须在停发行版 caddy 之前**跑：
/// 读 `/etc/caddy/Caddyfile` → 把非 b-ui 的顶层块原样写进 `<base>/caddy/sites/imported-from-v3.caddy`
/// （0644）→ 对**渲染出的新 Caddyfile**（带 import 行）跑 `caddy validate`。
///
/// validate 失败 → `Err`：调用方中止 import，发行版 caddy 照旧在跑，`/etc/caddy` 一个字节没动。
/// bundled caddy 还没装（离线装机）→ 只记一行「跳过」，不因此卡住 import（与
/// `reconcile::apply` 里「校验器不存在就跳过」同一口径）。
pub fn migrate_external_caddy_sites(
    host: &dyn Host,
    paths: &Paths,
    domain: &str,
    admin_port: u16,
) -> anyhow::Result<Vec<String>> {
    let Ok(Some(raw)) = host.read_file(Path::new(V3_CADDYFILE)) else {
        return Ok(Vec::new()); // 没装过发行版 caddy
    };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let export = external_caddy_sites(&text, domain, admin_port);
    let mut done = Vec::new();
    if export.dropped_global {
        done.push(format!(
            "提示：{V3_CADDYFILE} 的全局选项块没有导出（被 import 的文件里不允许出现它），需要的话请自己并进 {}",
            crate::paths::caddyfile(paths).display()
        ));
    }
    if export.text.is_empty() {
        done.push(format!(
            "{V3_CADDYFILE} 里只有 b-ui 面板块，没有外部站点要迁移"
        ));
    } else {
        let dest = crate::paths::caddy_sites_imported(paths);
        host.write_file(&dest, export.text.as_bytes(), 0o644)?;
        done.push(format!(
            "已把 {} 个外部站点块原样导出到 {}：{}",
            export.addresses.len(),
            dest.display(),
            export.addresses.join(" / ")
        ));
    }
    // 停发行版 caddy 之前的闸门：新 Caddyfile（含 import 行）必须先过 caddy validate
    match validate_rendered_caddyfile(host, paths, domain, admin_port)? {
        Some(note) => done.push(note),
        None => done.push("新 Caddyfile（含外部站点 import）已过 caddy validate".into()),
    }
    Ok(done)
}

/// 用 bundled caddy 校验渲染出的新 Caddyfile。`Ok(None)` = 校验通过，
/// `Ok(Some(note))` = 没有可用的 caddy 二进制、跳过校验，`Err` = 校验失败（中止 import）。
fn validate_rendered_caddyfile(
    host: &dyn Host,
    paths: &Paths,
    domain: &str,
    admin_port: u16,
) -> anyhow::Result<Option<String>> {
    let bin = paths.bin_dir.join("caddy");
    if host.read_file(&bin).ok().flatten().is_none() {
        return Ok(Some(format!(
            "没有 {}，已跳过新 Caddyfile 的 caddy validate",
            bin.display()
        )));
    }
    let candidate = crate::paths::verify_dir(paths).join("Caddyfile");
    let text = crate::modules::core_files::caddyfile_text(
        domain,
        admin_port,
        &crate::modules::core_files::sites_glob(paths),
    );
    host.write_file(&candidate, text.as_bytes(), 0o600)?;
    let cand = candidate.display().to_string();
    let out = host.run(
        &bin.display().to_string(),
        &["validate", "--config", &cand, "--adapter", "caddyfile"],
    );
    let _ = host.remove_file(&candidate);
    match out {
        Ok(o) if o.ok() => Ok(None),
        Ok(o) => anyhow::bail!(
            "新 Caddyfile 过不了 caddy validate，已中止导入（发行版 caddy 仍在运行，{V3_CADDYFILE} 未改动）：{}",
            if o.stderr.is_empty() { o.stdout } else { o.stderr }.trim()
        ),
        Err(e) => anyhow::bail!("跑 caddy validate 失败，已中止导入：{e}"),
    }
}

/// 只删 b-ui 自己写的 cron 行。
pub fn filter_cron(text: &str) -> String {
    text.lines()
        .filter(|l| !l.contains("/opt/b-ui/"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// 递归复制目录：只用 [`Host`] 的原语，所以测试里注入 `FakeHost` 就能全程不碰真实系统。
/// 目标已存在同名文件 → 跳过（幂等，且绝不用旧数据盖掉新的）。
fn copy_tree(host: &dyn Host, src: &Path, dest: &Path, done: &mut Vec<String>) {
    let Ok(entries) = host.list_dir(src) else {
        return;
    };
    for e in entries {
        let Some(name) = e.file_name() else { continue };
        let target = dest.join(name);
        if host.is_dir(&e).unwrap_or(false) {
            copy_tree(host, &e, &target, done);
        } else if host.read_file(&target).ok().flatten().is_some() {
            continue;
        } else if let Ok(Some(bytes)) = host.read_file(&e) {
            // 里面有 ACME 账号私钥与证书私钥，一律 0600（v4 的 caddy 以 root 运行）
            if host.write_file(&target, &bytes, 0o600).is_ok() {
                done.push(format!("已复制 {} → {}", e.display(), target.display()));
            }
        }
    }
}

/// 把 [`V3_CADDY_DATA`] 整棵复制到 `<base>/caddy/caddy/`（目标已有同名文件则跳过，不覆盖），
/// **复制完才** `systemctl stop caddy`；[`uninstall_v3`] 的第一步。
pub fn migrate_caddy_data(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let src = Path::new(V3_CADDY_DATA);
    if !host.is_dir(src).unwrap_or(false) {
        return Vec::new(); // 没装过发行版 caddy（全新机器）：什么都不做
    }
    let dest = crate::paths::caddy_data(paths);
    let mut done = Vec::new();
    copy_tree(host, src, &dest, &mut done);
    // 复制完才停发行版 caddy：它还活着就会继续往旧目录写，停早了 443 上会出现无证书窗口。
    // 对账（install 第 9 步）随后写 v4 的 caddy.service 并把它拉起来，届时用的是新的 XDG 目录。
    let _ = host.systemd("stop", "caddy");
    done.push(format!(
        "已迁移 v3 Caddy 数据目录（ACME 账号与证书）到 {} 并停用发行版 caddy",
        dest.display()
    ));
    done
}

/// 扫 `<base>` **顶层**（`list_dir` 只给直接子项），把 [`V3_LEFTOVER_PREFIXES`] 里某个基名后面
/// 跟着 `.bak.…` 的文件归档进 `v3-backup/`（0600）、跟着 `.tmp` 结尾的删掉；文件名恰好等于基名
/// 本身的（就是 v4 自己在管的那四个配置）绝不碰。
///
/// 漂移扫描只跳过 `.tmp` / `.new` 后缀，**不跳过 `*.bak.*`**，所以不清掉这些备份，
/// 导入后的机器每 10 分钟就会报一串 `stray_file`（spec §2.3 + §9）。
pub fn sweep_v3_leftovers(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(entries) = host.list_dir(&paths.base_dir) else {
        return done;
    };
    for e in entries {
        let Some(name) = e.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // 只认「某个受管基名 + 后缀」，且不能等于基名本身（那是 v4 正在管的配置）
        let Some(rest) = V3_LEFTOVER_PREFIXES
            .iter()
            .find_map(|pre| name.strip_prefix(pre).filter(|r| !r.is_empty()))
        else {
            continue;
        };
        if host.is_dir(&e).unwrap_or(false) {
            continue; // 只处理文件
        }
        if rest.starts_with(".bak.") {
            // 里面有 HY2 明文密码与 UUID：归档而不是删，0600
            if let Ok(Some(bytes)) = host.read_file(&e) {
                let dest = crate::paths::v3_backup_dir(paths).join(name);
                if host.write_file(&dest, &bytes, 0o600).is_ok() {
                    let _ = host.remove_file(&e);
                    done.push(format!(
                        "已归档 v3 备份 {} → {}",
                        e.display(),
                        dest.display()
                    ));
                }
            }
        } else if rest.ends_with(".tmp") {
            let _ = host.remove_file(&e);
            done.push(format!("已删除 v3 临时文件 {}", e.display()));
        }
    }
    done
}

/// 顺序：[`migrate_external_caddy_sites`]（外部站点块 + caddy validate 闸门）→
/// [`migrate_caddy_data`] → 停 v3 单元 → 删 v3 shell/Node 文件 → 归档 v3 状态文件 →
/// [`sweep_v3_leftovers`]（`*.bak.*` 归档、`*.tmp` 删） → 删 v3 `admin/` → 删
/// `/tmp/hy2-watchdog-*` → 清 cron 行；保留 `certs/` 与 `packages/`。
///
/// **端口跳跃的孤儿规则不在这里清**（4.0.1 起）：v3 留下的只有直连与住宅两种形状，两个
/// base 端口由 `bui_schema::v3::import` 从 v3 自己的 `listen:` 行原样搬进期望态，所以对账
/// 起两个 hysteria 单元时，各自的 `ExecStartPre=- bui hy2-prestart <配置>`
/// （[`crate::modules::portjump`]）就会按 base 端口把它们连链带表一并认领——两种后端、两族
/// 都覆盖，而且每次启动都跑，不只在导入时跑一次。这里原先那一份按 `HYSTERIA-PR-` /
/// `hysteria_` **前缀整表删**，会把同机别的 hysteria 实例一起清掉（v3.5.1 的老反例）。
///
/// 返回 `Err` 只有一种情况：第一步的 `caddy validate` 没过。那时**什么破坏性动作都还没做**
/// （发行版 caddy 还在跑、v3 单元一个没停），调用方按「中止 import」处理。
pub fn uninstall_v3(
    host: &dyn Host,
    paths: &Paths,
    domain: &str,
    admin_port: u16,
) -> anyhow::Result<Vec<String>> {
    // 第一步（2026-09-13 裁决）：把 /etc/caddy/Caddyfile 里非 b-ui 的站点块搬进外部站点目录，
    // 并对渲染出的新 Caddyfile 跑 caddy validate。必须排在停发行版 caddy **之前**，
    // 失败就整体中止（下面一行都不执行）。
    let mut done = migrate_external_caddy_sites(host, paths, domain, admin_port)?;
    // 第二步（2026-09-12 裁决）：先把发行版 Caddy 的 ACME 账号与证书搬进 v4 的数据目录，
    // 再停它；顺序颠倒就会重新签发。必须排在所有删除动作之前。
    done.extend(migrate_caddy_data(host, paths));
    let mut touched_units = false;
    for u in V3_UNITS {
        let unit_file = PathBuf::from("/etc/systemd/system").join(u);
        if host.read_file(&unit_file).ok().flatten().is_none()
            && !host.unit_exists(u).unwrap_or(false)
        {
            continue;
        }
        let _ = host.systemd("disable", u);
        let _ = host.systemd("stop", u);
        let _ = host.remove_file(&unit_file);
        touched_units = true;
        done.push(format!("已移除 v3 单元 {u}"));
    }
    for f in V3_FILES {
        let p = paths.base_dir.join(f);
        if host.read_file(&p).ok().flatten().is_some() {
            let _ = host.remove_file(&p);
            done.push(format!("已删除 {}", p.display()));
        }
    }
    // v3 的状态文件（含秘密）归档到 v3-backup/，0600
    for f in V3_STATE_FILES {
        let src = paths.base_dir.join(f);
        if let Ok(Some(bytes)) = host.read_file(&src) {
            let dest = crate::paths::v3_backup_dir(paths).join(f);
            if host.write_file(&dest, &bytes, 0o600).is_ok() {
                let _ = host.remove_file(&src);
                done.push(format!("已归档 {} → {}", src.display(), dest.display()));
            }
        }
    }
    // v3 迁移块与 CLI 留下的备份 / 临时文件（`*.bak.v357.<ts>` 一类）。漂移扫描只跳过
    // `.tmp` / `.new`，不跳过 `*.bak.*`，不清就是一串永久 stray_file
    done.extend(sweep_v3_leftovers(host, paths));
    // v3 的 Node 面板整棵删（server.js + node_modules + 目录本身）
    let admin = paths.base_dir.join("admin");
    if host.is_dir(&admin).unwrap_or(false) {
        let _ = host.remove_dir_all(&admin);
        done.push(format!("已删除 v3 Node 面板目录 {}", admin.display()));
    }
    // spec §3.4：/tmp/hy2-watchdog-* 计数文件（v3 面板还在读）
    if let Ok(entries) = host.list_dir(Path::new("/tmp")) {
        for e in entries {
            let name = e.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with("hy2-watchdog-") {
                let _ = host.remove_file(&e);
                done.push(format!("已删除 {}", e.display()));
            }
        }
    }
    if let Ok(out) = host.run("crontab", &["-l"]) {
        if out.ok() && out.stdout.contains("/opt/b-ui/") {
            let kept = filter_cron(&out.stdout);
            let tmp = paths.base_dir.join(".crontab.new");
            if host.write_file(&tmp, kept.as_bytes(), 0o600).is_ok() {
                let _ = host.run("crontab", &[&tmp.display().to_string()]);
                let _ = host.remove_file(&tmp);
                done.push("已清理 b-ui 的 cron 行（其它行保留）".into());
            }
        }
    }
    if touched_units {
        let _ = host.systemd_daemon_reload();
    }
    Ok(done)
}

/// `bui import-v3`：**只**从 v3 目录生成 `state.json`，不卸载 v3、不对账
/// （命令语义就是「先人工 diff」）。
pub async fn run(
    dir: PathBuf,
    out: Option<PathBuf>,
    paths: Paths,
    host: Arc<dyn Host>,
) -> anyhow::Result<()> {
    let report = bui_schema::v3::import(&dir)?;
    for w in &report.warnings {
        println!("导入提示：{w}");
        tracing::warn!("{w}");
    }
    let mut state = report.state;
    let (hostname, probe_ip, versions) = {
        let h = host.clone();
        let p = paths.clone();
        // 只在导入值为空时才兜底：探测失败绝不覆盖已导入的值
        let need_name = state.node.name.is_empty();
        let need_ip = state.node.public_ip.is_empty();
        tokio::task::spawn_blocking(move || {
            let hostname = if need_name {
                h.hostname().unwrap_or_default()
            } else {
                String::new()
            };
            let ip = if need_ip {
                crate::sys::probe_public_ip(h.as_ref())
            } else {
                String::new()
            };
            (
                hostname,
                ip,
                crate::kernels::installed_versions(h.as_ref(), &p.bin_dir),
            )
        })
        .await?
    };
    if state.node.name.is_empty() {
        state.node.name = hostname;
    }
    if state.node.public_ip.is_empty() {
        state.node.public_ip = probe_ip;
        if state.node.public_ip.is_empty() {
            println!("提示：v3 目录里没有公网 IP，探测也失败了，请稍后在面板里补填");
        }
    }
    let g = |k: &str| versions.get(k).cloned().unwrap_or_default();
    state.versions = bui_schema::model::Versions {
        bui: env!("CARGO_PKG_VERSION").to_string(),
        hysteria: g("hysteria"),
        xray: g("xray"),
        sing_box: g("sing-box"),
        caddy: g("caddy"),
        client_sing_box: g("sing-box"),
    };
    let target = out.unwrap_or_else(|| crate::paths::state_file(&paths));
    crate::state::store::Store::create(&target, state).await?;
    println!("已写出 {}", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    const CRONTAB: &str = "\
# m h dom mon dow command
0 */6 * * * /opt/b-ui/update.sh auto >/dev/null 2>&1
0 */12 * * * /opt/b-ui/update.sh kernel >/dev/null 2>&1
30 3 * * * /usr/local/bin/backup-my-blog.sh
";

    fn scratch(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    #[test]
    fn filter_cron_only_drops_b_ui_lines() {
        let out = filter_cron(CRONTAB);
        assert!(!out.contains("/opt/b-ui/update.sh"));
        assert!(out.contains("backup-my-blog.sh"), "别人的 cron 行必须留着");
        assert!(out.contains("# m h dom mon dow command"));
    }

    /// bwg-rick 实况：v3 的面板块（`core.sh:864` 那份）+ 运营手工加的两个外部站点块，
    /// 带前导注释、行内注释、带 `}` 的注释、`/etc/caddy/certs` 的绝对路径。
    const V3_CADDYFILE_TEXT: &str = "\
# B-UI Web 管理面板 - 由 Caddy 自动管理 HTTPS 证书
example.com {
    # 反代到 Node.js 管理面板
    reverse_proxy 127.0.0.1:8080

    # 日志
    log {
        output file /var/log/caddy/b-ui-access.log {
            roll_size 10mb
            roll_keep 5
        }
    }
}

# 我的博客（手工加的，别动）
blog.example.com {
    root * /srv/blog
    file_server
    tls /etc/caddy/certs/blog.crt /etc/caddy/certs/blog.key
}

shop.example.com, www.shop.example.com {
    reverse_proxy 127.0.0.1:3000  # 这个注释里有个 } 别被当成收尾
}
";

    /// 上面那份里除 b-ui 面板块之外的部分，**逐字节**就是这些（前导注释也算块的一部分）
    const EXTERNAL_BLOCKS: &str = "\
# 我的博客（手工加的，别动）
blog.example.com {
    root * /srv/blog
    file_server
    tls /etc/caddy/certs/blog.crt /etc/caddy/certs/blog.key
}

shop.example.com, www.shop.example.com {
    reverse_proxy 127.0.0.1:3000  # 这个注释里有个 } 别被当成收尾
}
";

    #[test]
    fn splits_top_level_blocks_and_keeps_them_byte_exact() {
        let blocks = split_caddy_blocks(V3_CADDYFILE_TEXT);
        let headers: Vec<&str> = blocks.iter().map(|b| b.header.as_str()).collect();
        assert_eq!(
            headers,
            vec![
                "example.com",
                "blog.example.com",
                "shop.example.com, www.shop.example.com"
            ]
        );
        // 拼回来必须与原文一字不差（注释、空行、缩进全在）
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<String>(),
            V3_CADDYFILE_TEXT
        );
        assert!(
            is_bui_panel_block(&blocks[0], "example.com", 8080),
            "面板块要认出来"
        );
        for b in &blocks[1..] {
            assert!(!is_bui_panel_block(b, "example.com", 8080), "{b:?}");
        }
        // 域名对但反代端口不是面板端口（运营自己反代的别的服务）→ 不是面板块
        assert!(!is_bui_panel_block(&blocks[0], "example.com", 9090));
        assert!(!is_bui_panel_block(&blocks[0], "other.example.com", 8080));
    }

    #[test]
    fn exports_every_block_but_the_panel_one() {
        let e = external_caddy_sites(V3_CADDYFILE_TEXT, "example.com", 8080);
        assert!(e.text.starts_with(SITES_EXPORT_HEADER));
        assert_eq!(
            &e.text[SITES_EXPORT_HEADER.len()..],
            EXTERNAL_BLOCKS,
            "外部站点块必须逐字节原样（含注释与绝对路径）"
        );
        assert!(
            !e.text.contains("127.0.0.1:8080"),
            "b-ui 面板块不许被导出（会和 v4 的 Caddyfile 撞同一个站点地址）"
        );
        assert!(
            e.text.contains("/etc/caddy/certs/blog.crt"),
            "绝对路径不许改"
        );
        assert_eq!(
            e.addresses,
            vec![
                "blog.example.com".to_string(),
                "shop.example.com, www.shop.example.com".to_string()
            ]
        );
        assert!(!e.dropped_global);
    }

    #[test]
    fn a_caddyfile_with_only_the_panel_block_exports_nothing() {
        let only_panel = "example.com {\n\treverse_proxy 127.0.0.1:8080\n}\n";
        let e = external_caddy_sites(only_panel, "example.com", 8080);
        assert_eq!(e.text, "");
        assert!(e.addresses.is_empty());
    }

    #[test]
    fn the_global_options_block_is_dropped_with_a_warning() {
        // 被 import 的文件里不允许出现全局选项块，照搬过去 caddy validate 直接报错
        let text = "\
{
\temail me@example.com
}

example.com {
\treverse_proxy 127.0.0.1:8080
}

blog.example.com {
\tfile_server
}
";
        let e = external_caddy_sites(text, "example.com", 8080);
        assert!(e.dropped_global, "全局选项块要被丢掉并告知");
        assert!(!e.text.contains("email me@example.com"));
        assert!(e.text.contains("blog.example.com {"));
        assert_eq!(e.addresses, vec!["blog.example.com".to_string()]);
    }

    #[test]
    fn external_sites_are_exported_and_validated_before_the_distro_caddy_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                V3_CADDYFILE.into(),
                (V3_CADDYFILE_TEXT.as_bytes().to_vec(), 0o644),
            );
            // bundled caddy 已装好（install 第 3/4 步先装内核，第 7 步才卸 v3）
            i.files
                .insert(paths.bin_dir.join("caddy"), (b"ELF".to_vec(), 0o755));
            i.files.insert(
                format!("{V3_CADDY_DATA}/certificates/x/example.com.crt").into(),
                (b"CERT".to_vec(), 0o600),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let done = uninstall_v3(&h, &paths, "example.com", 8080).unwrap();
        let sites = crate::paths::caddy_sites_imported(&paths);
        let got = h.text(sites.to_str().unwrap()).expect("要写出导出文件");
        assert_eq!(&got[SITES_EXPORT_HEADER.len()..], EXTERNAL_BLOCKS);
        assert_eq!(h.mode(sites.to_str().unwrap()), Some(0o644));
        assert_eq!(
            h.text(V3_CADDYFILE).as_deref(),
            Some(V3_CADDYFILE_TEXT),
            "/etc/caddy 只读，一个字节都不许动"
        );
        let ops = h.ops();
        let validate = ops
            .iter()
            .position(|o| o.contains("caddy validate --config"))
            .expect("停 caddy 之前要对新 Caddyfile 跑 validate");
        let stop = ops
            .iter()
            .position(|o| o == "systemd:stop:caddy")
            .expect("要停发行版 caddy");
        let write = ops
            .iter()
            .position(|o| o.starts_with(&format!("write:{}", sites.display())))
            .expect("要写导出文件");
        assert!(write < validate && validate < stop, "{ops:?}");
        // validate 用的是渲染出的新 Caddyfile（带 import 行），不是 /etc/caddy 那份
        assert!(
            ops[validate].contains(
                crate::paths::verify_dir(&paths)
                    .join("Caddyfile")
                    .to_str()
                    .unwrap()
            ),
            "{:?}",
            ops[validate]
        );
        assert!(done.iter().any(|l| l.contains("blog.example.com")));
    }

    #[test]
    fn a_failing_validate_aborts_the_import_and_leaves_the_distro_caddy_running() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                V3_CADDYFILE.into(),
                (V3_CADDYFILE_TEXT.as_bytes().to_vec(), 0o644),
            );
            i.files
                .insert(paths.bin_dir.join("caddy"), (b"ELF".to_vec(), 0o755));
            i.files.insert(
                format!("{V3_CADDY_DATA}/certificates/x/example.com.crt").into(),
                (b"CERT".to_vec(), 0o600),
            );
            for u in V3_UNITS {
                i.files.insert(
                    format!("/etc/systemd/system/{u}").into(),
                    (b"x".to_vec(), 0o644),
                );
                i.units_active.insert(u.to_string());
            }
            i.scripted.push((
                format!("{} validate", paths.bin_dir.join("caddy").display()),
                CmdOut::failure(
                    1,
                    "Caddyfile:9 - Error during parsing: unrecognized directive",
                ),
            ));
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let err = uninstall_v3(&h, &paths, "example.com", 8080).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unrecognized directive"), "{msg}");
        let ops = h.ops();
        assert!(
            !ops.iter().any(|o| o == "systemd:stop:caddy"),
            "validate 失败必须保留发行版 caddy 在跑：{ops:?}"
        );
        let verify_prefix = format!("remove:{}", crate::paths::verify_dir(&paths).display());
        assert!(
            !ops.iter().any(|o| o.starts_with("systemd:disable:")
                || (o.starts_with("remove:") && !o.starts_with(&verify_prefix))),
            "还没开始卸 v3（只允许清掉 .verify/ 里的校验候选）：{ops:?}"
        );
        assert!(h.text(V3_CADDYFILE).is_some(), "/etc/caddy 不删");
        assert!(
            h.text(&format!("{V3_CADDY_DATA}/certificates/x/example.com.crt"))
                .is_some(),
            "发行版 caddy 的证书原地不动"
        );
    }

    #[test]
    fn a_machine_without_the_distro_caddyfile_skips_the_whole_channel() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        assert_eq!(
            migrate_external_caddy_sites(&h, &paths, "example.com", 8080).unwrap(),
            Vec::<String>::new()
        );
        assert!(!h.ops().iter().any(|o| o.contains("validate")));
    }

    #[test]
    fn a_caddyfile_without_the_bundled_caddy_binary_only_notes_the_skip() {
        // 离线装机 / 内核还没下载：validate 不了就只记一行提示，不能因此卡住 import
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                V3_CADDYFILE.into(),
                (V3_CADDYFILE_TEXT.as_bytes().to_vec(), 0o644),
            );
        });
        let done = migrate_external_caddy_sites(&h, &paths, "example.com", 8080).unwrap();
        assert!(done.iter().any(|l| l.contains("跳过")), "{done:?}");
        assert!(!h.ops().iter().any(|o| o.contains("validate")));
    }

    #[test]
    fn v3_units_never_touch_a_v4_managed_unit() {
        for u in V3_UNITS {
            let bare = u.trim_end_matches(".service").trim_end_matches(".timer");
            assert!(
                !crate::reconcile::MANAGED_UNITS.contains(&bare),
                "{u} 是 v4 受管单元，不能在卸载列表里"
            );
        }
        assert!(!V3_UNITS.contains(&"b-ui-relay.service"));
        // 两份 v3 单元表必须是包含关系：上一轮事故就是「units 与 drift 各有一份、内容还不一样」，
        // 结果 `hysteria-server@.service` / `xray@.service` 只有 --force 才清理。
        for u in V3_UNITS {
            assert!(
                crate::reconcile::LEGACY_UNITS.contains(&u),
                "{u} 不在 LEGACY_UNITS 里：uninstall 停了它、漂移扫描却不认它"
            );
        }
    }

    /// 4.0.1：卸载不再碰任何 NAT 规则。端口跳跃孤儿改由两个 hysteria 单元各自的
    /// `ExecStartPre=- bui hy2-prestart <配置>` 按 base 端口清（覆盖证明在
    /// `modules::portjump` 的 `every_v3_leftover_shape_is_claimed_by_the_matching_instance_prestart`）。
    /// 这里只守「卸载不越界」：机器上 iptables / nft 齐全、孤儿也在，卸载照样一条规则都不动。
    #[test]
    fn uninstall_never_touches_nat_rules() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            for c in ["iptables", "ip6tables", "nft"] {
                i.which.insert(c.into());
            }
            i.scripted.push((
                "iptables -t nat -S".into(),
                CmdOut::success(concat!(
                    "-N HYSTERIA-PR-abc123\n",
                    "-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-abc123\n",
                    "-A HYSTERIA-PR-abc123 -p udp -j REDIRECT --to-ports 10000\n",
                )),
            ));
            i.scripted.push((
                "nft list tables".into(),
                CmdOut::success("table ip hysteria_abc123\n"),
            ));
        });
        uninstall_v3(&h, &paths, "example.com", 8080).unwrap();
        assert!(
            h.ops()
                .iter()
                .all(|o| !o.contains("HYSTERIA-PR-") && !o.contains("nft")),
            "{:?}",
            h.ops()
        );
    }

    #[test]
    fn uninstall_stops_v3_units_archives_state_deletes_shell_and_keeps_certs() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            for u in V3_UNITS {
                i.files.insert(
                    format!("/etc/systemd/system/{u}").into(),
                    (b"x".to_vec(), 0o644),
                );
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            for f in V3_FILES {
                i.files
                    .insert(d.path().join(f), (b"#!/bin/bash".to_vec(), 0o755));
            }
            for f in V3_STATE_FILES {
                i.files
                    .insert(d.path().join(f), (b"secret".to_vec(), 0o600));
            }
            // v3 迁移块与 CLI 留下的真实残留
            i.files.insert(
                d.path().join("config.yaml.bak.v357.1757000000"),
                (b"listen: :10000".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("config.yaml.bak.obfs.20260901-120000"),
                (b"obfs".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("xray-config.json.bak.v359.1757000001"),
                (b"{}".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("config-residential.yaml.bak.v360.1757000002"),
                (b"resi".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("xray-config.json.tmp"),
                (b"half".to_vec(), 0o600),
            );
            // v4 自己在管的四个配置：同名文件绝不能被这一步碰到
            i.files.insert(
                d.path().join("config.yaml"),
                (b"listen: :10000".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("certs/fullchain.pem"),
                (b"CERT".to_vec(), 0o644),
            );
            i.files.insert(
                d.path().join("packages/versions.json"),
                (b"{}".to_vec(), 0o644),
            );
            i.files
                .insert(d.path().join("admin/server.js"), (b"node".to_vec(), 0o644));
            i.files.insert(
                d.path().join("admin/node_modules/y/index.js"),
                (b"y".to_vec(), 0o644),
            );
            i.files
                .insert("/tmp/hy2-watchdog-10000".into(), (b"2".to_vec(), 0o644));
            i.files
                .insert("/tmp/hy2-watchdog-40000".into(), (b"0".to_vec(), 0o644));
            i.files
                .insert("/tmp/unrelated.txt".into(), (b"keep".to_vec(), 0o644));
            i.scripted
                .push(("crontab -l".into(), CmdOut::success(CRONTAB)));
        });
        let done = uninstall_v3(&h, &paths, "example.com", 8080).unwrap();
        for u in V3_UNITS {
            assert!(
                h.ops().contains(&format!("systemd:disable:{u}")),
                "{u} 要停用"
            );
            assert!(
                h.text(&format!("/etc/systemd/system/{u}")).is_none(),
                "{u} 的单元文件要删"
            );
        }
        for f in V3_FILES {
            assert!(
                h.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 要删"
            );
        }
        for f in V3_STATE_FILES {
            assert!(
                h.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 要移走"
            );
            assert_eq!(
                h.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                )
                .as_deref(),
                Some("secret"),
                "{f} 要进 v3-backup"
            );
            assert_eq!(
                h.mode(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                ),
                Some(0o600)
            );
        }
        assert!(
            h.text(
                d.path()
                    .join("admin/node_modules/y/index.js")
                    .to_str()
                    .unwrap()
            )
            .is_none(),
            "Node 面板整棵删"
        );
        assert!(h
            .ops()
            .contains(&format!("rmdir:{}", d.path().join("admin").display())));
        assert_eq!(
            h.text(d.path().join("certs/fullchain.pem").to_str().unwrap())
                .as_deref(),
            Some("CERT"),
            "证书必须保留"
        );
        assert!(
            h.text(d.path().join("packages/versions.json").to_str().unwrap())
                .is_some(),
            "内核缓存保留"
        );
        assert!(
            h.text("/tmp/hy2-watchdog-10000").is_none()
                && h.text("/tmp/hy2-watchdog-40000").is_none()
        );
        assert_eq!(
            h.text("/tmp/unrelated.txt").as_deref(),
            Some("keep"),
            "只删自己的 /tmp 文件"
        );
        // v3 的迁移备份：归档（含明文密码）；临时文件：删掉；v4 在管的同名配置：不许碰
        for bak in [
            "config.yaml.bak.v357.1757000000",
            "config.yaml.bak.obfs.20260901-120000",
            "xray-config.json.bak.v359.1757000001",
            "config-residential.yaml.bak.v360.1757000002",
        ] {
            assert!(
                h.text(d.path().join(bak).to_str().unwrap()).is_none(),
                "{bak} 应移走"
            );
            assert!(
                h.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(bak)
                        .to_str()
                        .unwrap()
                )
                .is_some(),
                "{bak} 应进 v3-backup（漂移扫描不跳过 *.bak.*）"
            );
        }
        assert!(
            h.text(d.path().join("xray-config.json.tmp").to_str().unwrap())
                .is_none(),
            ".tmp 应删掉"
        );
        assert_eq!(
            h.text(d.path().join("config.yaml").to_str().unwrap())
                .as_deref(),
            Some("listen: :10000"),
            "v4 在管的配置本身不能被 sweep 碰到"
        );
        // cron 必须被**重写**：`crontab -l` 自己就会产生一条 `run:crontab -l`，所以断言
        // `starts_with("run:crontab")` 是恒真的空断言；要断言真正的写入命令。
        assert!(
            h.ops().contains(&format!(
                "run:crontab {}",
                d.path().join(".crontab.new").display()
            )),
            "cron 行要重写：{:?}",
            h.ops()
        );
        assert!(done.iter().any(|l| l.contains("已清理 b-ui 的 cron 行")));
        assert!(!done.is_empty());
    }

    #[test]
    fn caddy_acme_data_is_copied_before_the_distro_unit_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let certs = format!(
            "{V3_CADDY_DATA}/certificates/acme-v02.api.letsencrypt.org-directory/example.com"
        );
        let acct =
            format!("{V3_CADDY_DATA}/acme/acme-v02.api.letsencrypt.org-directory/users/default");
        h.with(|i| {
            i.files.insert(
                format!("{certs}/example.com.crt").into(),
                (b"CERT".to_vec(), 0o644),
            );
            i.files.insert(
                format!("{certs}/example.com.key").into(),
                (b"KEY".to_vec(), 0o600),
            );
            i.files.insert(
                format!("{acct}/default.key").into(),
                (b"ACCT".to_vec(), 0o600),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let done = uninstall_v3(&h, &paths, "example.com", 8080).unwrap();
        let dest = crate::paths::caddy_data(&paths);
        let key = dest.join(
            "certificates/acme-v02.api.letsencrypt.org-directory/example.com/example.com.key",
        );
        assert_eq!(
            h.text(key.to_str().unwrap()).as_deref(),
            Some("KEY"),
            "证书私钥要搬过来"
        );
        assert_eq!(h.mode(key.to_str().unwrap()), Some(0o600));
        assert_eq!(
            h.text(
                dest.join("acme/acme-v02.api.letsencrypt.org-directory/users/default/default.key")
                    .to_str()
                    .unwrap()
            )
            .as_deref(),
            Some("ACCT"),
            "ACME 账号私钥必须一起搬，否则 Caddy 重新注册账号并重签，撞 Let's Encrypt 速率限制"
        );
        let ops = h.ops();
        let stop = ops
            .iter()
            .position(|o| o == "systemd:stop:caddy")
            .expect("要停发行版 caddy");
        let last_copy = ops
            .iter()
            .rposition(|o| o.starts_with(&format!("write:{}", dest.display())))
            .expect("要复制文件");
        assert!(
            last_copy < stop,
            "必须先复制完再停 caddy，否则会出现无证书窗口：{ops:?}"
        );
        assert!(done.iter().any(|l| l.contains("Caddy 数据目录")));
    }

    #[test]
    fn caddy_migration_is_idempotent_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let dest = crate::paths::caddy_data(&paths).join("certificates/x/example.com.crt");
        h.with(|i| {
            i.files.insert(
                format!("{V3_CADDY_DATA}/certificates/x/example.com.crt").into(),
                (b"OLD".to_vec(), 0o600),
            );
            i.files.insert(dest.clone(), (b"NEW".to_vec(), 0o600));
        });
        let done = migrate_caddy_data(&h, &paths);
        assert_eq!(
            h.text(dest.to_str().unwrap()).as_deref(),
            Some("NEW"),
            "目标已有的文件不许被旧数据盖掉"
        );
        assert!(done.iter().all(|l| !l.contains("已复制")), "{done:?}");
    }

    #[test]
    fn uninstall_on_a_machine_without_v3_is_a_no_op() {
        let d = tempfile::tempdir().unwrap();
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ))
        });
        let done = uninstall_v3(&h, &scratch(&d), "example.com", 8080).unwrap();
        assert_eq!(done, Vec::<String>::new());
        assert!(!h
            .ops()
            .iter()
            .any(|o| o.starts_with("remove:") || o.starts_with("rmdir:")));
    }

    #[tokio::test]
    async fn import_writes_state_from_a_v3_directory() {
        // 复用 P0 的合成 v3 fixture（crates/bui-schema/tests/fixtures/v3/src）
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| i.hostname = "node-b".into());
        run(src.to_path_buf(), None, paths.clone(), host)
            .await
            .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(state.users.len(), 4);
        assert_eq!(state.node.domain, "example.com");
        assert_eq!(
            state.node.name, "example.com",
            "导入值优先，hostname 只在导入值为空时兜底"
        );
        assert_eq!(state.node.ports.hy2_hop, Some((20000, 30000)));
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(crate::paths::state_file(&paths))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn import_probes_the_public_ip_only_when_it_is_missing() {
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        // fixture 里有 server_ip.txt → 导入值非空 → 不该调 curl 覆盖它
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.scripted
                .push(("curl".into(), CmdOut::failure(7, "couldn't connect")))
        });
        run(src.to_path_buf(), None, paths.clone(), host.clone())
            .await
            .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(
            state.node.public_ip, "203.0.113.10",
            "导入到的公网 IP 不能被失败的探测清空"
        );
        assert!(
            !host.ops().iter().any(|o| o.starts_with("run:curl")),
            "导入值非空就别探测"
        );
    }

    /// 2026-09-12 真机：v3 目录里没有 IP 且 ipify 返回空串 → `node.public_ip` 留空。
    /// 探测必须过三个源（`crate::sys::probe_public_ip`），第一个不灵就换下一个。
    #[tokio::test]
    async fn import_falls_through_to_the_next_ip_source() {
        let Some(src) = fixture_without_server_ip() else {
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", crate::sys::IP_PROBE_URLS[0]),
                // 真机形态：HTTP 200 但回了个空串
                CmdOut::success("\n"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", crate::sys::IP_PROBE_URLS[1]),
                CmdOut::success("198.51.100.7\n"),
            ));
        });
        run(src.path().to_path_buf(), None, paths.clone(), host.clone())
            .await
            .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(state.node.public_ip, "198.51.100.7");
    }

    /// v3 fixture 的副本，去掉 `server_ip.txt`（导入值为空 → 走探测那一支）。
    fn fixture_without_server_ip() -> Option<tempfile::TempDir> {
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return None;
        }
        let dst = tempfile::tempdir().unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            if name == "server_ip.txt" {
                continue;
            }
            let (from, to) = (e.path(), dst.path().join(&name));
            if from.is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                for f in std::fs::read_dir(&from).unwrap() {
                    let f = f.unwrap();
                    std::fs::copy(f.path(), to.join(f.file_name())).unwrap();
                }
            } else {
                std::fs::copy(&from, &to).unwrap();
            }
        }
        Some(dst)
    }
}
