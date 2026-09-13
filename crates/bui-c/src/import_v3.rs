//! 从 v3 客户端（`/opt/hysteria-client`）原地升级：导入节点与端口、卸载旧单元与残留。
//!
//! 只在交互路径触发（菜单首次运行的一问、`bui-c import-v3`）；`check` / `update`
//! 这类非交互路径不自动导入——那等于让 timer 悄悄改用户配置（决策 10）。
//!
//! 不删 `<base>` 目录：v3 的 `uri.txt` 是回滚素材，清理交给 `uninstall --purge-v3`。

use crate::engine::Engine;
use crate::net::Net;
use crate::paths::{Paths, TUN_IFACE};
use crate::profiles::{
    default_split, profile_name, rfc3339, Mode, Panel, Profile, Profiles, Source,
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
    /// 预发布期得给个显式出口。只换 `base_url`，`username` 仍用推导值。
    pub panel: Option<String>,
    /// `--mode`：覆盖「v3 的 bui-tun 是否 enabled」推导出的模式。
    pub mode: Option<Mode>,
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
pub fn import<S: Sys>(sys: &S, base: &Path, prof: &mut Profiles) -> Result<Report> {
    let mut r = Report::default();
    let v3_active = read_str(sys, &base.join("active")).unwrap_or_default();
    let mut active_ports: Option<(u16, u16)> = None;

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
        // free_name 对不冲突的名字原样返回，同 kind 的第二个节点自动变 `…-2`
        let name = prof.free_name(&profile_name(&user, &node));

        if dir_name == v3_active {
            r.active = Some(name.clone());
            active_ports = Some((
                meta_u16(&meta, "socks_port").unwrap_or(1080),
                meta_u16(&meta, "http_port").unwrap_or(8080),
            ));
        }
        if prof.panel.is_none() {
            if let Some(host) =
                read_str(sys, &base.join("server_address")).filter(|h| !h.is_empty())
            {
                prof.panel = Some(Panel {
                    base_url: format!("https://{host}"),
                    username: user.clone(),
                });
            }
        }
        prof.upsert(Profile {
            name: name.clone(),
            node,
            split: default_split(),
            source: Source::V3,
            imported_at: rfc3339(sys),
        });
        r.imported.push(name);
    }

    if r.imported.is_empty() {
        return Err(Error::msg(format!("{} 下没有可导入的节点", base.display())));
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
    for iface in [TUN_IFACE, "hystun"] {
        let _ = sys.run("ip", &["link", "delete", iface]);
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
    let mut r = import(sys, base, prof)?;
    if let Some(url) = &opts.panel {
        prof.panel = Some(Panel {
            base_url: url.trim().trim_end_matches('/').to_string(),
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
        let r = import(&s, Path::new(V3_BASE), &mut prof).unwrap();
        assert_eq!(
            r.imported,
            vec![
                "alice-hy2-direct".to_string(),
                "alice-reality-direct".to_string()
            ]
        );
        assert!(r.skipped.is_empty());
        assert_eq!(
            r.active.as_deref(),
            Some("alice-reality-direct"),
            "v3 的 active 目录映射到新 profile 名"
        );
        assert_eq!(prof.active.as_deref(), Some("alice-reality-direct"));
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
        import(&s, Path::new(V3_BASE), &mut prof).unwrap();
        assert_eq!(prof.mode, Mode::Tun, "v3 用 TUN 的机器升级后仍是 TUN");

        let s2 = v3_machine();
        s2.reply("systemctl is-enabled --quiet bui-tun.service", 1, "");
        let mut prof2 = Profiles::new_default();
        import(&s2, Path::new(V3_BASE), &mut prof2).unwrap();
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
        let r = import(&s, Path::new(V3_BASE), &mut prof).unwrap();
        assert_eq!(r.imported.len(), 2);
        assert_eq!(r.skipped, vec!["broken".to_string(), "no-uri".to_string()]);
    }

    #[test]
    fn import_without_any_node_errors() {
        let s = FakeSys::new();
        s.put("/opt/hysteria-client/configs/x/meta.json", "{}");
        let mut prof = Profiles::new_default();
        let e = import(&s, Path::new(V3_BASE), &mut prof).unwrap_err();
        assert!(e.to_string().contains("没有可导入的节点"), "{e}");
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
        assert_eq!(saved.active.as_deref(), Some("alice-reality-direct"));
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
        let r = import(&s, Path::new(V3_BASE), &mut prof).unwrap();
        assert_eq!(r.imported.len(), 5, "五个目录一个都不能丢：{r:?}");
        assert!(r.skipped.is_empty(), "不该有跳过的目录：{:?}", r.skipped);
        let mut names = r.imported.clone();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 5, "同 kind 的重名要自动让位：{:?}", r.imported);
        assert_eq!(prof.profiles.len(), 5);
    }
}
