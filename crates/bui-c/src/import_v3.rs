//! 从 v3 客户端（`/opt/hysteria-client`）原地升级：导入节点与端口、卸载旧单元与残留。
//!
//! 只在交互路径触发（菜单首次运行的一问、`bui-c import-v3`）；`check` / `update`
//! 这类非交互路径不自动导入——那等于让 timer 悄悄改用户配置（决策 10）。
//!
//! 不删 `<base>` 目录：v3 的 `uri.txt` 是回滚素材，清理交给 `uninstall --purge-v3`。
//!
//! profile 名沿用 v3 的目录名（`hysteria2-1785892136`、`HY2`、`reality-Reality` …）：
//! v3 用户在 v3 里就是拿目录名切节点的，原地升级后名字不变，认得出哪个是哪个。

use crate::engine::Engine;
use crate::net::Net;
use crate::paths::{Paths, TUN_IFACE, UNIT_MAIN};
use crate::profiles::{
    default_split, https_base, profile_name, rfc3339, sanitize, Mode, Panel, Profile, Profiles,
    Source,
};
use crate::sys::{systemd, Sys};
use crate::{update, Error, Result};
use std::path::Path;

pub const V3_BASE: &str = "/opt/hysteria-client";
pub const V3_UNITS: [&str; 3] = [
    "hysteria-client.service",
    "xray-client.service",
    "bui-tun.service",
];
pub const V3_AUX_UNITS: [&str; 2] = ["hysteria-health.timer", "hysteria-health.service"];
const V3_SYSCTL: &str = "/etc/sysctl.d/99-hysteria.conf";

/// 一次导入 / 卸载的结果，菜单据此打印人话。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// 新建的 profile 名
    pub imported: Vec<String>,
    /// 解析失败的 v3 目录名
    pub skipped: Vec<String>,
    /// v3 目录里的节点已经在 profiles 里（同一个连接，见
    /// [`same_endpoint`](crate::profiles::same_endpoint)），这次原样跳过的 profile 名
    pub existing: Vec<String>,
    /// 命中墓碑、这次没导入的节点名（墓碑里记的那个名字，spec §5.7）。菜单据此问一句、
    /// 命令行据此打一行；[`RunOpts::with_deleted`] 为真时永远是空的
    pub buried: Vec<String>,
    pub active: Option<String>,
    pub removed_units: Vec<String>,
    pub ufw_restored: bool,
    /// 这次真下载并落了 `bin/sing-box`（机器上本来就有内核时是 false）
    pub kernel_installed: bool,
}

/// [`run`] 的可选覆盖项，都来自 `bui-c import-v3` 的命令行（或 `BUI_C_PANEL`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOpts {
    /// `--panel`：覆盖从 v3 `server_address` 推导的面板地址。v3 面板没有 `/packages`，
    /// 预发布期得给个显式出口。只换 `base_url`，`username` 仍用推导值。只收 https：
    /// 它会落成 root 自更新的来源。
    pub panel: Option<String>,
    /// `--mode`：覆盖「v3 的 bui-tun 是否 enabled」推导出的模式。
    pub mode: Option<Mode>,
    /// `--with-deleted`：连删过的节点一起导（spec §5.7）。默认假：命中墓碑的先跳过、
    /// 收进 [`Report::buried`]，菜单问一句、命令行打一行。
    pub with_deleted: bool,
}

/// `<base>/configs` 存在就认为这机器装过 v3 客户端。
pub fn detect<S: Sys>(sys: &S, base: &Path) -> bool {
    sys.exists(&base.join("configs"))
}

fn read_str<S: Sys>(sys: &S, p: &Path) -> Option<String> {
    sys.read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

fn meta_u16(meta: &serde_json::Value, key: &str) -> Option<u16> {
    meta.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u16::try_from(n).ok())
}

/// v3 的 fragment 是 `<用户名>-<标签>`（server.js 的订阅生成规则），取前缀当用户名。
fn user_from_label(label: &str) -> String {
    match label.split_once('-') {
        Some((u, _)) if !u.is_empty() => u.to_string(),
        _ => String::new(),
    }
}

/// 把 `<base>/configs/*` 里能解析的节点收进 `prof`，端口与面板取 v3 的记录。
///
/// 单个目录坏掉只记进 `skipped`，不打断整次导入；一个都没成功才报错。
///
/// profile 名取 v3 的目录名（见模块文档），目录名 sanitize 后为空时才回落到
/// [`profile_name`]。[`user_from_label`] 仍用来推导 [`Panel::username`]。
///
/// `with_deleted` 为假时认墓碑（spec §5.7）：删掉过的节点不导入，名字收进
/// [`Report::buried`]；为真时照常导入并把墓碑清掉。
pub fn import<S: Sys>(
    sys: &S,
    base: &Path,
    prof: &mut Profiles,
    with_deleted: bool,
) -> Result<Report> {
    let mut r = Report::default();
    let v3_active = read_str(sys, &base.join("active")).unwrap_or_default();
    let mut active_ports: Option<(u16, u16)> = None;
    // 面板候选值先攒着：一个新节点都没导入时连它都不能落（见函数尾部）
    let mut pending_panel: Option<Panel> = None;

    for dir in sys.read_dir(&base.join("configs")).unwrap_or_default() {
        let dir_name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let uri = match read_str(sys, &dir.join("uri.txt")) {
            Some(u) if !u.is_empty() => u,
            _ => {
                r.skipped.push(dir_name);
                continue;
            }
        };
        let node = match bui_schema::parse::node_uri(&uri) {
            Ok(n) => n,
            Err(_) => {
                r.skipped.push(dir_name);
                continue;
            }
        };
        let meta: serde_json::Value = read_str(sys, &dir.join("meta.json"))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let user = user_from_label(&node.label);
        if prof.panel.is_none() && pending_panel.is_none() {
            if let Some(host) =
                read_str(sys, &base.join("server_address")).filter(|h| !h.is_empty())
            {
                pending_panel = Some(Panel {
                    base_url: format!("https://{host}"),
                    username: user.clone(),
                });
            }
        }
        // 重跑幂等：同一个连接已经在 profiles 里就原样跳过——不新建、不改名、不回写。
        // v3 的 `<base>/configs` 按约定保留着（`detect` 恒为真），少了这一步，
        // 在已迁移的机器上按一次菜单 [7] 就多出一批 `-2` 重复节点。按连接身份认而不是
        // 整个 `Node` 相等：面板导入过一次后 label 已是 `HY2直连`，不再是 v3 的备注。
        if let Some(existing) = prof.find_same_endpoint(&node) {
            let name = existing.name.clone();
            if dir_name == v3_active {
                r.active = Some(name.clone());
            }
            r.existing.push(name);
            continue;
        }
        // 删过的节点默认不带回来（spec §5.7）：v3 目录按约定留着，不记墓碑的话按一次
        // [7]→[3] 就把删掉的又导回来了。名字取墓碑里记的那个
        if !with_deleted {
            if let Some(t) = prof.tombstone_of(&node) {
                r.buried.push(t.name.clone());
                continue;
            }
        }
        // 明确要它：墓碑清掉，下次导入不再跳过
        prof.forget(&node);
        // profile 名沿用 v3 的目录名：用户在 v3 里就是拿它 `switch` 的，原地升级后
        // 名字不变，一眼认得出哪个是哪个。目录名 sanitize 后为空（全非 ASCII）才回落
        // 到 profile_name。free_name 对不冲突的名字原样返回。
        let name_base = match sanitize(&dir_name) {
            d if !d.is_empty() => d,
            _ => profile_name(&user, &node),
        };
        let name = prof.free_name(&name_base);

        if dir_name == v3_active {
            r.active = Some(name.clone());
            active_ports = Some((
                meta_u16(&meta, "socks_port").unwrap_or(1080),
                meta_u16(&meta, "http_port").unwrap_or(8080),
            ));
        }
        prof.upsert(Profile {
            name: name.clone(),
            node,
            split: default_split(),
            source: Source::V3,
            imported_at: rfc3339(sys),
            extra: Default::default(),
        });
        r.imported.push(name);
    }

    if r.imported.is_empty() {
        // 全被墓碑挡下也是「有东西可导」：调用方要打那一行 / 问那一句，不能报成「没有节点」
        if r.existing.is_empty() && r.buried.is_empty() {
            return Err(Error::msg(format!("{} 下没有可导入的节点", base.display())));
        }
        // 一个新节点都没有 → `prof` 一个字段都不动（active / mode / 端口 / 面板）：
        // 用户早就可能在 v4 里换过节点，重跑一次 import 不该把他拽回 v3 的选择。
        return Ok(r);
    }
    if let Some(p) = pending_panel {
        prof.panel = Some(p);
    }
    if let Some((s_port, h_port)) = active_ports {
        prof.socks_port = s_port;
        prof.http_port = h_port;
    }
    // v3 的 bui-tun 是 enable 状态 → 用户本来在用 TUN，升级后保持 TUN
    prof.mode = if systemd::is_enabled(sys, "bui-tun.service") {
        Mode::Tun
    } else {
        Mode::Socks
    };
    if r.active.is_none() {
        prof.active = r.imported.first().cloned();
        r.active = prof.active.clone();
    } else {
        prof.active = r.active.clone();
    }
    Ok(r)
}

/// 停用删除 v3 的五个单元、sysctl 片段与残留 TUN 接口，必要时把 UFW 拉回来。
///
/// 单元目录一律取 `paths.unit_dir`（`BUI_C_UNIT_DIR` 覆盖对它同样有效）。
pub fn teardown<S: Sys>(sys: &S, paths: &Paths, base: &Path) -> Result<Report> {
    let mut r = Report::default();
    for u in V3_UNITS.iter().chain(V3_AUX_UNITS.iter()) {
        systemd::stop_quiet(sys, u);
        systemd::disable_quiet(sys, u);
        systemd::reset_failed_quiet(sys, u);
        let f = paths.unit(u);
        if sys.exists(&f) {
            sys.remove_file(&f)?;
            r.removed_units.push((*u).to_string());
        }
    }
    systemd::daemon_reload(sys)?;
    sys.remove_file(Path::new(V3_SYSCTL))?;
    // bui-tun 在 v4 里就是 `bui-c.service` 自己的接口：它在跑的时候删接口 = 断网，
    // 而重跑 import 渲出的 config 字节不变不会触发重启，得等 bui-c.timer 一分钟后
    // 的巡检才把接口救回来。只有 v4 数据面没在跑时，它才确实是 v3 的残留。
    if !systemd::is_active(sys, UNIT_MAIN) {
        for iface in [TUN_IFACE, "hystun"] {
            let _ = sys.run("ip", &["link", "delete", iface]);
        }
    }
    // v3 的 TUN 流程会 `ufw disable` 整墙；留下这个标记说明墙本来是开的
    let marker = base.join(".ufw_state");
    if let Some(txt) = read_str(sys, &marker) {
        let was_active = txt.lines().any(|l| l.trim() == "ufw_was_active=true");
        let now_inactive = sys
            .run("ufw", &["status"])
            .map(|o| o.stdout.contains("Status: inactive"))
            .unwrap_or(false);
        if was_active && now_inactive {
            let _ = sys.run("ufw", &["--force", "enable"]);
            r.ufw_restored = true;
        }
        sys.remove_file(&marker)?;
    }
    Ok(r)
}

/// `bui-c import-v3` 的整条动作：导入 → 备引擎并自检 → 落盘 → 最后才卸 v3。
///
/// 顺序是这个函数的全部要点（缺陷 1，与服务端 `bui install` 的「缺内核即中止」同一原则）：
/// v3 的单元在最后一步才被停掉，在那之前任何一步失败都 `Err` 返回，**既不落盘也不动 v3**，
/// 机器仍由 v3 客户端代理着。先落盘再卸载则是第二层保险：卸载途中出错时节点已经在
/// `profiles.json` 里了。
pub fn run<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    base: &Path,
    prof: &mut Profiles,
    opts: &RunOpts,
) -> Result<Report> {
    if !detect(sys, base) {
        return Err(Error::msg(format!(
            "没找到 v3 客户端目录 {}",
            base.display()
        )));
    }
    // 覆盖的面板就是 manifest 与内核的来源，也会落成 root 自更新的 panel：只收 https。
    // 在动任何东西之前查，错了 v3 一字不动。
    let panel_override = match opts.panel.as_deref() {
        None => None,
        Some(raw) => Some(https_base(raw).ok_or_else(|| {
            Error::msg(format!(
                "面板地址 {} 不是 https，不能作为 manifest 与内核来源，v3 客户端原样保留；\
                 请改用 https:// 开头的面板地址",
                raw.trim()
            ))
        })?),
    };
    let mut r = import(sys, base, prof, opts.with_deleted)?;
    // 重跑幂等：新节点一个没有、v3 单元也一个不剩 → 无事可做，直接回。
    // 继续往下是有害的：`ensure_kernel` / 自检 / 落盘全是空转，而 `teardown` 会去删
    // bui-tun——那正是 v4 数据面正在用的接口。
    let leftovers = V3_UNITS
        .iter()
        .chain(V3_AUX_UNITS.iter())
        .any(|u| sys.exists(&paths.unit(u)));
    // 新节点一个没有、v3 的节点又全被墓碑挡下、手上也没有活动节点（删光之后，spec §9）：
    // 同样无事可做，残留单元在也一样。继续往下只会在「没有可用的活动节点」上报错，命令行的
    // 跳过行与菜单那一问都到不了（审查 T7b r2 I1）。残留单元留给答 y / `--with-deleted`
    // 那一趟清——那一趟 `imported` 非空，照常 teardown。`existing` 非空而没有活动节点在 v4
    // 里到不了，不在这里吞掉：留给下面那句错误当断言
    let nothing_to_apply = r.existing.is_empty() && prof.active_profile().is_none();
    if r.imported.is_empty() && (!leftovers || nothing_to_apply) {
        return Ok(r);
    }
    if let Some(base_url) = panel_override {
        prof.panel = Some(Panel {
            base_url,
            username: prof
                .panel
                .as_ref()
                .map(|p| p.username.clone())
                .unwrap_or_default(),
        });
    }
    if let Some(m) = opts.mode {
        prof.mode = m;
    }

    // ① 引擎：拿不到内核就此中止——此时 profiles.json 还没写、v3 单元一个没动
    let kernel_installed = update::ensure_kernel(sys, net, paths, prof).map_err(|e| {
        Error::msg(format!(
            "拿不到 sing-box 内核，v3 客户端原样保留：{e}。\
             可用 `bui-c import-v3 --panel <面板地址>` 指定 manifest 来源"
        ))
    })?;

    // ② 自检：用刚备好的内核把活动节点的配置渲一遍、`sing-box check` 过一遍
    let engine = Engine::new(sys, paths);
    let active = prof
        .active_profile()
        .ok_or_else(|| Error::msg("导入后没有可用的活动节点，v3 客户端原样保留".to_string()))?;
    let cfg = engine
        .render(prof, active)
        .map_err(|e| Error::msg(format!("渲染客户端配置失败，v3 客户端原样保留：{e}")))?;
    engine
        .verify(&cfg)
        .map_err(|e| Error::msg(format!("sing-box 自检不通过，v3 客户端原样保留：{e}")))?;

    // ③ 引擎已就绪，这时候落盘、卸 v3 才是安全的
    prof.save(sys, paths)?;
    let t = teardown(sys, paths, base)?;
    r.removed_units = t.removed_units;
    r.ufw_restored = t.ufw_restored;
    r.kernel_installed = kernel_installed;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys};
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    /// C4 形状里客户端用得上的那部分（对齐 `update.rs` 测试里的 `manifest_json`）：
    /// 版本 + `client_sing_box` + 两个 sing-box 裸二进制。
    fn manifest_json(sb_sha: &str) -> String {
        let dl = "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0";
        format!(
            r#"{{"version":"{v}","kernels":{{"client_sing_box":"1.14.5"}},
                 "artifacts":{{
                 "sing-box-linux-amd64":{{"url":"{dl}/sing-box-linux-amd64","sha256":"{sb_sha}"}},
                 "sing-box-linux-arm64":{{"url":"{dl}/sing-box-linux-arm64","sha256":"{sb_sha}"}}}}}}"#,
            v = crate::VERSION
        )
    }

    /// 让 `<panel>/packages/` 这一路能拿到 manifest 与内核裸二进制。
    fn serve_kernel(n: &FakeNet, panel: &str, bin: &[u8]) {
        n.route(
            &format!("{panel}/packages/manifest.json"),
            FakeReply::Text(manifest_json(&crate::update::sha256_hex(bin))),
        );
        n.route(
            &format!(
                "{panel}/packages/sing-box-linux-{}",
                crate::update::arch_suffix()
            ),
            FakeReply::Bytes(bin.to_vec()),
        );
    }

    /// 把 v3 五个单元文件都摆上，好断言「中止时一个都没被动」。
    fn with_v3_units(s: &FakeSys) {
        for u in V3_UNITS.iter().chain(V3_AUX_UNITS.iter()) {
            s.put(&format!("/etc/systemd/system/{u}"), "[Unit]");
        }
    }

    /// 中止路径的共同断言：没落盘、没停/禁用任何单元、v3 单元文件都还在。
    fn assert_v3_untouched(s: &FakeSys) {
        assert!(
            !s.exists(Path::new("/opt/bui-c/profiles.json")),
            "引擎没备好就不该落盘"
        );
        for c in s.calls() {
            assert!(
                !c.starts_with("systemctl stop") && !c.starts_with("systemctl disable"),
                "中止路径不该动任何单元，却跑了：{c}"
            );
        }
        for u in V3_UNITS.iter().chain(V3_AUX_UNITS.iter()) {
            assert!(
                s.exists(Path::new(&format!("/etc/systemd/system/{u}"))),
                "{u} 应原样保留"
            );
        }
    }

    fn v3_machine() -> FakeSys {
        let s = FakeSys::new();
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1757000000/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1757000000/meta.json",
            r#"{"name":"hysteria2-1757000000","alias":"alice-HY2直连","protocol":"hysteria2","server":"panel.example.com:10000","socks_port":1080,"http_port":8080}"#,
        );
        s.put(
            "/opt/hysteria-client/configs/vless-1757000001/uri.txt",
            "vless://11111111-1111-4111-8111-111111111111@panel.example.com:10001?encryption=none&security=reality&sni=www.bing.com&fp=chrome&pbk=PUB&sid=0123456789abcdef&flow=xtls-rprx-vision&type=tcp#alice-Reality%E7%9B%B4%E8%BF%9E",
        );
        s.put(
            "/opt/hysteria-client/configs/vless-1757000001/meta.json",
            r#"{"name":"vless-1757000001","protocol":"vless-reality","server":"panel.example.com:10001","socks_port":11080,"http_port":18080}"#,
        );
        s.put("/opt/hysteria-client/active", "vless-1757000001\n");
        s.put("/opt/hysteria-client/server_address", "panel.example.com");
        s
    }

    #[test]
    fn detect_only_fires_when_v3_configs_exist() {
        let s = v3_machine();
        assert!(detect(&s, Path::new(V3_BASE)));
        assert!(!detect(&FakeSys::new(), Path::new(V3_BASE)));
    }

    #[test]
    fn import_maps_nodes_ports_panel_and_active() {
        let s = v3_machine();
        let mut prof = Profiles::new_default();
        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(
            r.imported,
            vec![
                "hysteria2-1757000000".to_string(),
                "vless-1757000001".to_string()
            ],
            "profile 名沿用 v3 目录名：用户在 v3 里就是拿它 switch 的"
        );
        assert!(r.skipped.is_empty());
        assert_eq!(
            r.active.as_deref(),
            Some("vless-1757000001"),
            "v3 的 active 目录映射到新 profile 名"
        );
        assert_eq!(prof.active.as_deref(), Some("vless-1757000001"));
        assert_eq!(prof.profiles.len(), 2);
        assert!(prof.profiles.iter().all(|p| p.source == Source::V3));
        // 端口取「active 那个节点」的 meta（v4 的端口是全局设置，不再按节点存）
        assert_eq!((prof.socks_port, prof.http_port), (11080, 18080));
        assert_eq!(
            prof.panel.as_ref().map(|p| p.base_url.as_str()),
            Some("https://panel.example.com")
        );
        assert_eq!(
            prof.panel.as_ref().map(|p| p.username.as_str()),
            Some("alice"),
            "用户名从 URI fragment 前缀推导：依赖 node_uri 的 label 是完整 fragment（决策 8）"
        );
    }

    #[test]
    fn import_defaults_mode_from_v3_tun_state() {
        let s = v3_machine();
        s.reply("systemctl is-enabled --quiet bui-tun.service", 0, "");
        let mut prof = Profiles::new_default();
        import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(prof.mode, Mode::Tun, "v3 用 TUN 的机器升级后仍是 TUN");

        let s2 = v3_machine();
        s2.reply("systemctl is-enabled --quiet bui-tun.service", 1, "");
        let mut prof2 = Profiles::new_default();
        import(&s2, Path::new(V3_BASE), &mut prof2, false).unwrap();
        assert_eq!(prof2.mode, Mode::Socks);
    }

    #[test]
    fn import_skips_broken_dirs_but_keeps_going() {
        let s = v3_machine();
        s.put(
            "/opt/hysteria-client/configs/broken/uri.txt",
            "ss://nope@h:1#x",
        );
        s.put("/opt/hysteria-client/configs/no-uri/meta.json", "{}");
        let mut prof = Profiles::new_default();
        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(r.imported.len(), 2);
        assert_eq!(r.skipped, vec!["broken".to_string(), "no-uri".to_string()]);
    }

    #[test]
    fn import_without_any_node_errors() {
        let s = FakeSys::new();
        s.put("/opt/hysteria-client/configs/x/meta.json", "{}");
        let mut prof = Profiles::new_default();
        let e = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap_err();
        assert!(e.to_string().contains("没有可导入的节点"), "{e}");
    }

    /// v3 的节点全被删过（spec §5.7）：不是「没有可导入的节点」，要如实报成 `buried`，
    /// 调用方才能打那一行 / 问那一句；`with_deleted` 则照常导入并清墓碑。
    #[test]
    fn import_reports_buried_nodes_instead_of_failing() {
        let s = v3_machine();
        let mut prof = Profiles::new_default();
        let first = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(first.imported.len(), 2);
        // 两个都删掉（只记墓碑，节点从列表里拿走）
        let mut buried = Profiles::new_default();
        for name in &first.imported {
            let p = prof.profiles.iter().find(|p| &p.name == name).unwrap();
            buried.bury(p, 7);
        }

        let mut prof = buried.clone();
        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert!(r.imported.is_empty() && r.existing.is_empty(), "{r:?}");
        assert_eq!(r.buried, first.imported, "{r:?}");
        assert!(prof.profiles.is_empty(), "一个都不该导回来");
        assert_eq!(prof.deleted.len(), 2, "墓碑原样留着");

        let mut prof = buried;
        let r = import(&s, Path::new(V3_BASE), &mut prof, true).unwrap();
        assert_eq!(r.imported, first.imported);
        assert!(r.buried.is_empty(), "{r:?}");
        assert!(prof.deleted.is_empty(), "加回来了就清墓碑");
    }

    #[test]
    fn teardown_removes_all_v3_units_and_leftovers() {
        let s = v3_machine();
        for u in V3_UNITS.iter().chain(V3_AUX_UNITS.iter()) {
            s.put(&format!("/etc/systemd/system/{u}"), "[Unit]");
        }
        s.put(
            "/etc/sysctl.d/99-hysteria.conf",
            "net.ipv4.conf.all.rp_filter=2",
        );
        let r = teardown(&s, &paths(), Path::new(V3_BASE)).unwrap();
        assert_eq!(r.removed_units.len(), 5);
        for u in V3_UNITS.iter().chain(V3_AUX_UNITS.iter()) {
            assert!(s.called(&format!("systemctl stop {u}")));
            assert!(s.called(&format!("systemctl disable {u}")));
            assert!(!s.exists(Path::new(&format!("/etc/systemd/system/{u}"))));
        }
        assert!(s.called("systemctl daemon-reload"));
        assert!(!s.exists(Path::new("/etc/sysctl.d/99-hysteria.conf")));
        assert!(s.called("ip link delete bui-tun"));
        assert!(s.called("ip link delete hystun"));
        // v3 的节点目录保留，作为回滚素材
        assert!(s.exists(Path::new(
            "/opt/hysteria-client/configs/vless-1757000001/uri.txt"
        )));
    }

    #[test]
    fn teardown_restores_ufw_when_v3_had_disabled_it() {
        let s = v3_machine();
        s.put("/opt/hysteria-client/.ufw_state", "ufw_was_active=true\n");
        s.reply("ufw status", 0, "Status: inactive\n");
        let r = teardown(&s, &paths(), Path::new(V3_BASE)).unwrap();
        assert!(r.ufw_restored);
        assert!(s.called("ufw --force enable"));
        assert!(!s.exists(Path::new("/opt/hysteria-client/.ufw_state")));
    }

    #[test]
    fn teardown_leaves_an_already_active_ufw_alone() {
        let s = v3_machine();
        s.put("/opt/hysteria-client/.ufw_state", "ufw_was_active=true\n");
        s.reply("ufw status", 0, "Status: active\n");
        let r = teardown(&s, &paths(), Path::new(V3_BASE)).unwrap();
        assert!(!r.ufw_restored);
        assert!(!s.called("ufw --force enable"));
    }

    #[test]
    fn teardown_uses_the_injected_unit_dir() {
        // BUI_C_UNIT_DIR 覆盖后必须生效：硬编码 /etc/systemd/system 会让测试去删真实目录里的文件
        let s = v3_machine();
        let p = Paths::new("/opt/bui-c", "/run/units");
        s.put("/run/units/hysteria-client.service", "[Unit]");
        s.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        let r = teardown(&s, &p, Path::new(V3_BASE)).unwrap();
        assert_eq!(r.removed_units, vec!["hysteria-client.service".to_string()]);
        assert!(!s.exists(Path::new("/run/units/hysteria-client.service")));
        assert!(
            s.exists(Path::new("/etc/systemd/system/hysteria-client.service")),
            "没碰真实单元目录"
        );
    }

    #[test]
    fn run_imports_then_tears_down_and_persists() {
        let s = v3_machine();
        // 机器上已有内核 → `ensure_kernel` 是空操作，这条用例仍只看「导入 → 落盘 → 卸载」
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        s.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        let r = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();
        assert_eq!(r.imported.len(), 2);
        assert!(!r.kernel_installed, "内核本来就在，不该再下一次");
        assert_eq!(
            r.removed_units,
            vec!["hysteria-client.service".to_string()],
            "只有实际存在的单元文件才计入删除"
        );
        assert!(s.called("systemctl daemon-reload"));
        let saved = Profiles::load(&s, &paths()).unwrap();
        assert_eq!(saved.profiles.len(), 2);
        assert_eq!(saved.active.as_deref(), Some("vless-1757000001"));
        assert_eq!(s.mode("/opt/bui-c/profiles.json"), Some(0o600));
    }

    #[test]
    fn run_aborts_before_touching_v3_when_the_kernel_cannot_be_fetched() {
        let s = v3_machine();
        with_v3_units(&s);
        // 没有内核，且一个 URL 都没登记 = 面板与 GitHub 都不可达
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        let e = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("v3 客户端原样保留"), "{e}");
        assert_v3_untouched(&s);
    }

    #[test]
    fn run_aborts_when_sing_box_check_rejects_the_config() {
        let s = v3_machine();
        with_v3_units(&s);
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply(
            "/opt/bui-c/bin/sing-box check -c /opt/bui-c/.config.json.new",
            1,
            "",
        );
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        let e = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("v3 客户端原样保留"), "{e}");
        assert_v3_untouched(&s);
    }

    #[test]
    fn run_installs_the_kernel_before_stopping_v3_units() {
        let s = v3_machine();
        s.put("/etc/systemd/system/bui-tun.service", "[Unit]");
        let n = FakeNet::new();
        let bin = b"ELF-sing-box".to_vec();
        serve_kernel(&n, "https://panel.example.com", &bin);
        let mut prof = Profiles::new_default();
        let r = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();
        assert!(r.kernel_installed);
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), "ELF-sing-box");
        let calls = s.calls();
        let checked = calls
            .iter()
            .position(|c| c.contains("sing-box check"))
            .expect("应当跑过 sing-box 自检");
        let stopped = calls
            .iter()
            .position(|c| c == "systemctl stop bui-tun.service")
            .expect("应当卸过 v3 的 bui-tun");
        assert!(
            checked < stopped,
            "自检必须排在卸 v3 之前（拿不到内核时机器要还有代理）：{calls:?}"
        );
        assert!(s.exists(Path::new("/opt/bui-c/profiles.json")));
        assert_eq!(s.mode("/opt/bui-c/profiles.json"), Some(0o600));
    }

    #[test]
    fn run_honours_panel_and_mode_overrides() {
        let s = v3_machine();
        // v3 这台机器在用 TUN，`--mode socks` 要压过这个推导
        s.reply("systemctl is-enabled --quiet bui-tun.service", 0, "");
        let n = FakeNet::new();
        let bin = b"ELF-sing-box".to_vec();
        serve_kernel(&n, "https://other.example.com", &bin);
        let mut prof = Profiles::new_default();
        let opts = RunOpts {
            panel: Some("https://other.example.com/".into()),
            mode: Some(Mode::Socks),
            with_deleted: false,
        };
        run(&s, &n, &paths(), Path::new(V3_BASE), &mut prof, &opts).unwrap();
        let saved = Profiles::load(&s, &paths()).unwrap();
        assert_eq!(
            saved.panel.as_ref().map(|p| p.base_url.as_str()),
            Some("https://other.example.com"),
            "尾斜杠要去掉"
        );
        assert_eq!(
            saved.panel.as_ref().map(|p| p.username.as_str()),
            Some("alice"),
            "用户名仍取 v3 推导出的那个"
        );
        assert_eq!(saved.mode, Mode::Socks);
        assert!(
            n.log()
                .iter()
                .any(|l| l.contains("https://other.example.com/packages/manifest.json")),
            "manifest 应当去 --panel 指定的面板取：{:?}",
            n.log()
        );
    }

    /// `--panel` / `BUI_C_PANEL` 指定的面板是 root 自更新的 manifest 与二进制来源（sha256 也来自
    /// 同一份 manifest，没有签名）：明文 http 谁都能在路上改，不能要。报错时 v3 一字不动。
    #[test]
    fn run_refuses_a_non_https_panel_override_and_leaves_v3_alone() {
        let s = v3_machine();
        with_v3_units(&s);
        let n = FakeNet::new();
        let bin = b"ELF-sing-box".to_vec();
        serve_kernel(&n, "http://other.example.com", &bin);
        let mut prof = Profiles::new_default();
        let opts = RunOpts {
            panel: Some("http://other.example.com".into()),
            mode: None,
            with_deleted: false,
        };
        let e = run(&s, &n, &paths(), Path::new(V3_BASE), &mut prof, &opts).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("https"), "要说清楚得用 https：{msg}");
        assert!(msg.contains("v3 客户端原样保留"), "{msg}");
        assert_v3_untouched(&s);
        assert!(
            n.log().is_empty(),
            "不该去 http 源取 manifest：{:?}",
            n.log()
        );
        assert!(!s.exists(Path::new("/opt/bui-c/bin/sing-box")));
    }

    /// 回归保护（缺陷 3，解析层已由 688f88a 的 `parse::node_uri` 修好，这条只是钉住它）：
    /// baiyi 真机上的五个 v3 目录——早期 `hysteria2-<ts>` 把 userinfo 写成 `user%3Apass@`，
    /// fragment 还常常没有 `-<标签>` 后缀——必须一个不落地导进来。
    #[test]
    fn import_keeps_early_v3_hysteria2_dirs() {
        let s = FakeSys::new();
        s.put(
            "/opt/hysteria-client/configs/HY2/uri.txt",
            "hysteria2://alice:pw@h0.example.com:40000?sni=h0.example.com&insecure=0&mport=41000-50000#%E7%A4%BA%E4%BE%8B%E4%B8%93%E7%94%A8%E5%90%8D-HY2%E4%BD%8F%E5%AE%85",
        );
        for (dir, host, frag) in [
            (
                "hysteria2-1757000001",
                "h1.example.com",
                "%E7%A4%BA%E4%BE%8B%E4%B8%93%E7%94%A8%E5%90%8D",
            ),
            (
                "hysteria2-1757000002",
                "h2.example.com",
                "%E7%A4%BA%E4%BE%8B%E4%B8%B4%E6%97%B6%E5%90%8D",
            ),
            (
                "hysteria2-1757000003",
                "h3.example.com",
                "%E7%A4%BA%E4%BE%8B%E4%B8%93%E7%94%A8%E5%90%8D-%E5%B0%8F%E7%BB%84",
            ),
        ] {
            s.put(
                &format!("/opt/hysteria-client/configs/{dir}/uri.txt"),
                &format!(
                    "hysteria2://alice%3Apw@{host}:10000?sni={host}&insecure=0&allowInsecure=0&mport=20000-30000#{frag}"
                ),
            );
        }
        s.put(
            "/opt/hysteria-client/configs/reality-Reality/uri.txt",
            "vless://11111111-1111-4111-8111-111111111111@h0.example.com:10001?encryption=none&security=reality&sni=www.bing.com&fp=chrome&pbk=PUB&sid=0123456789abcdef&flow=xtls-rprx-vision&type=tcp#alice-Reality%E7%9B%B4%E8%BF%9E",
        );
        s.put("/opt/hysteria-client/active", "HY2\n");

        let mut prof = Profiles::new_default();
        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert!(r.skipped.is_empty(), "不该有跳过的目录：{:?}", r.skipped);
        // 名字精确等于 v3 目录名（read_dir 已排序），不再是 `hy2-direct` / `-2` / `-3`：
        // 五个节点的用户名都是中文，旧规则 sanitize 后全塌成同一个 kind slug。
        assert_eq!(
            r.imported,
            vec![
                "HY2".to_string(),
                "hysteria2-1757000001".to_string(),
                "hysteria2-1757000002".to_string(),
                "hysteria2-1757000003".to_string(),
                "reality-Reality".to_string(),
            ]
        );
        assert_eq!(prof.profiles.len(), 5);
        assert_eq!(prof.active.as_deref(), Some("HY2"));
    }

    /// 目录名全是非 ASCII（sanitize 后为空）时才回落到 [`profile_name`]。
    #[test]
    fn dir_names_that_sanitize_to_nothing_fall_back_to_profile_name() {
        let s = FakeSys::new();
        s.put(
            "/opt/hysteria-client/configs/中文目录/uri.txt",
            "hysteria2://alice:hy2-pw@h9.example.com:10000/?sni=h9.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        let mut prof = Profiles::new_default();
        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(r.imported, vec!["alice-hy2-direct".to_string()]);
    }

    /// 重跑幂等（缺陷：`/opt/hysteria-client/configs` 按约定保留着，`detect` 恒为真，
    /// 于是 baiyi 上按一次菜单 [7] 就多出五个 `-2` 重复节点，active 还被拽回 v3 的选择）。
    #[test]
    fn import_reports_existing_nodes_without_duplicating() {
        let s = v3_machine();
        let mut prof = Profiles::new_default();
        let first = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert_eq!(first.imported.len(), 2);
        assert!(first.existing.is_empty(), "第一次全是新节点");

        // 升级到 v4 之后用户自己换了节点、开了 TUN：重跑 import 不该把他拽回 v3 的选择
        prof.active = Some("hysteria2-1757000000".to_string());
        prof.mode = Mode::Tun;

        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert!(r.imported.is_empty(), "一个新节点都没有：{:?}", r.imported);
        assert_eq!(
            r.existing,
            vec![
                "hysteria2-1757000000".to_string(),
                "vless-1757000001".to_string()
            ],
            "按 profile 名报告已存在的节点，不改名、不新建"
        );
        assert_eq!(prof.profiles.len(), 2, "不该冒出 `-2` 后缀的重复条目");
        assert_eq!(
            prof.active.as_deref(),
            Some("hysteria2-1757000000"),
            "没导入任何新节点就不动 active"
        );
        assert_eq!(prof.mode, Mode::Tun, "没导入任何新节点就不动 mode");
        assert_eq!(
            (prof.socks_port, prof.http_port),
            (11080, 18080),
            "端口同理保持原样"
        );
    }

    /// 面板刷新过 label（`HY2直连` 取代 v3 备注）之后重跑 import-v3：整个 `Node` 已经不等，
    /// 但仍是同一个连接——计入 `existing`，不新建 `-2`，也不拿 v3 的旧数据回写它。
    #[test]
    fn rerun_recognises_nodes_whose_label_was_updated_by_the_panel() {
        let s = v3_machine();
        let mut prof = Profiles::new_default();
        import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        let hy2 = prof
            .profiles
            .iter_mut()
            .find(|p| p.name == "hysteria2-1757000000")
            .unwrap();
        hy2.node.label = "HY2直连".into();
        hy2.source = Source::ApiNodes;

        let r = import(&s, Path::new(V3_BASE), &mut prof, false).unwrap();
        assert!(r.imported.is_empty(), "{:?}", r.imported);
        assert_eq!(
            r.existing,
            vec![
                "hysteria2-1757000000".to_string(),
                "vless-1757000001".to_string()
            ]
        );
        assert_eq!(prof.profiles.len(), 2, "不该冒出 `-2` 后缀的重复条目");
        assert_eq!(
            prof.profiles[0].node.label, "HY2直连",
            "面板刷新过的节点不被回写"
        );
    }

    #[test]
    fn run_twice_is_a_no_op_and_keeps_the_live_tun() {
        let s = v3_machine();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        let first = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();
        assert_eq!(first.imported.len(), 2);
        let saved_before = s.get("/opt/bui-c/profiles.json").unwrap();

        // v4 数据面已经起来了：bui-tun 这时是 bui-c.service 自己的接口
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        let before = s.calls().len();

        let r = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();
        assert!(r.imported.is_empty(), "{:?}", r.imported);
        assert_eq!(r.existing.len(), 2);
        assert!(r.removed_units.is_empty(), "v3 单元上一轮就删干净了");

        let second: Vec<String> = s.calls().into_iter().skip(before).collect();
        for c in &second {
            assert!(
                !c.starts_with("ip link delete"),
                "v4 在跑时删 bui-tun 就是断网：{second:?}"
            );
            assert!(!c.starts_with("systemctl stop"), "没东西可停：{second:?}");
            assert!(
                !c.contains("sing-box check"),
                "无事可做就别自检：{second:?}"
            );
        }
        assert_eq!(
            s.get("/opt/bui-c/profiles.json").unwrap(),
            saved_before,
            "重跑不该改写 profiles.json"
        );
    }

    #[test]
    fn teardown_leaves_the_interface_alone_while_v4_is_running() {
        let s = v3_machine();
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        teardown(&s, &paths(), Path::new(V3_BASE)).unwrap();
        assert!(
            !s.called("ip link delete bui-tun"),
            "bui-tun 这时归 v4 数据面：{:?}",
            s.calls()
        );
        assert!(!s.called("ip link delete hystun"));

        let s2 = v3_machine();
        s2.reply("systemctl is-active --quiet bui-c.service", 3, "");
        teardown(&s2, &paths(), Path::new(V3_BASE)).unwrap();
        assert!(s2.called("ip link delete bui-tun"), "v4 没跑就照旧清残留");
        assert!(s2.called("ip link delete hystun"));
    }

    #[test]
    fn partial_rerun_still_tears_down_leftover_units() {
        let s = v3_machine();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        let n = FakeNet::new();
        let mut prof = Profiles::new_default();
        run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();

        // 模拟上一次卸载中途失败：单元文件又回来了（或压根没删掉）
        s.put("/etc/systemd/system/bui-tun.service", "[Unit]");
        let r = run(
            &s,
            &n,
            &paths(),
            Path::new(V3_BASE),
            &mut prof,
            &RunOpts::default(),
        )
        .unwrap();
        assert!(r.imported.is_empty(), "{:?}", r.imported);
        assert_eq!(r.existing.len(), 2);
        assert_eq!(
            r.removed_units,
            vec!["bui-tun.service".to_string()],
            "还有 v3 残留就仍旧走 teardown"
        );
    }
}
