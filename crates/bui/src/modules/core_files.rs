//! 四个内核二进制 + 它们的五份配置（`config.yaml` / `config-residential.yaml` /
//! `xray-config.json` / `singbox-relay.json` / `Caddyfile`），外加外部站点目录的占位
//! `caddy/sites/README.txt`（2026-09-13 裁决：目录由对账器建，里面的 `*.caddy` 归用户管）。
//!
//! 除 Caddyfile 之外的每份配置都**原样**落 `bui-schema` 的渲染结果（总纲 C1）：P1 绝不自己
//! 拼内核配置，订阅与配置的一致性由 P0 的 golden 测试兜底。
//!
//! `auth-snapshot.json` 不是 artifact：`bui install` 写初版，P2 的用户模块接手后在每次用户
//! 变更时原子重写，对账不比对它的内容。

use crate::kernels::{Manifest, KERNELS};
use crate::reconcile::{Artifact, Module, RenderCtx, Unit, Verify};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use bui_schema::render::relay::RelayOpts;
use std::sync::{Arc, RwLock};

/// 本地 relay 的 socks 入站端口（两个数据面内核的住宅出口都指向它）。
pub const RELAY_LISTEN_PORT: u16 = 2080;
/// 本地 relay 的 Clash API（巡检热切换上游用，不重启进程）。
pub const RELAY_CLASH_API: &str = "127.0.0.1:9091";

/// 槽 `index` 的住宅配置路径：槽 0 是 v3 的 `config-residential.yaml`（兼容面），
/// 其余是 `config-residential-<i>.yaml`。
pub fn resi_config_path(p: &Paths, index: u16) -> std::path::PathBuf {
    if index == 0 {
        p.base_dir.join("config-residential.yaml")
    } else {
        p.base_dir.join(format!("config-residential-{index}.yaml"))
    }
}

/// 四个内核二进制 + 它们的五份配置（唯一来源是 bui-schema 的渲染器）+ 站点目录占位文件。
/// manifest 放共享锁里：守护进程每日自检拉到新 manifest 后直接写这个锁，
/// 下一轮对账就按新版本装内核，不需要重启进程（spec §7）。
pub struct CoreFilesModule {
    manifest: Arc<RwLock<Option<Manifest>>>,
}

impl CoreFilesModule {
    pub fn new(manifest: Option<Manifest>) -> Self {
        Self {
            manifest: Arc::new(RwLock::new(manifest)),
        }
    }

    pub fn manifest_handle(&self) -> Arc<RwLock<Option<Manifest>>> {
        self.manifest.clone()
    }
}

/// relay 的渲染参数：两个本地端口固定，公网 IP 只在探到时传（直连例外用）。
pub fn relay_opts(state: &State, paths: &Paths) -> RelayOpts {
    RelayOpts {
        listen_port: RELAY_LISTEN_PORT,
        api: RELAY_CLASH_API.to_string(),
        cache_path: paths.base_dir.join("relay-cache.db").display().to_string(),
        server_ip: if state.node.public_ip.is_empty() {
            None
        } else {
            Some(state.node.public_ip.clone())
        },
    }
}

/// 站点目录的占位说明：写它顺带把目录建出来（`write_file` 会建父目录），
/// 后缀不是 `.caddy` 所以不会被 import 进配置。
pub const SITES_README: &str = "\
# 外部站点通道（B-UI v4）
#
# 这个目录里的 *.caddy 文件会被 <base>/Caddyfile 末尾的 import 行整体引入，
# 内容**完全归你管**：bui 只负责把这个目录建出来，从不改写、覆盖或删除里面的文件，
# 漂移扫描也不会报告它们。
#
# 一个文件可以放任意多个顶层站点块，写法与 /etc/caddy/Caddyfile 里一样，例如：
#
#   blog.example.com {
#       root * /srv/blog
#       file_server
#   }
#
# 注意：
# - 不要在这里写 Caddy 的全局选项块（开头那个没有站点地址的 `{ ... }`）——
#   被 import 的文件里不允许出现它，caddy validate 会直接报错；
# - 改完用 `bui reconcile` 或 `systemctl reload caddy` 生效；语法错会让整份配置
#   校验失败（对账会把它报成 verify_failures，旧配置保持不变）。
";

/// 外部站点通道的 import 通配：`<base>/caddy/sites/*.caddy`（路径经 [`crate::paths`] 派生）。
pub fn sites_glob(paths: &Paths) -> String {
    crate::paths::caddy_sites(paths)
        .join("*.caddy")
        .display()
        .to_string()
}

/// 四个免鉴权订阅端点的 path 匹配器（`log_skip` 用）。末段是订阅 token 或宽限期内的
/// 用户名，两者都是凭据（2026-09-14 裁决）。Caddy 的 `path` 匹配器**不分大小写**，
/// 所以 `/API/SUB/<token>` 这种请求也一并跳过。
const SUB_PATHS_MATCHER: &str = "/api/sub/* /api/subscription/* /api/clash/* /api/nodes/*";

/// `format filter` 的 `regexp` 参数：把订阅路径的末段换成 `***`（口径同 [`crate::redact::sub_path`]）。
///
/// `log_skip` 只挡站点路由树里的**访问**日志，挡不住这两条真会漏的路（2026-09-14 用本机
/// caddy 2.10.2 复现）：
/// - `reverse_proxy` 连不上上游时的错误日志（`http.log.error.log0`）落到 **default** logger，
///   `"uri":"/api/sub/<token>?x=1"` 原样进 journald。面板的上游就是本机 `:admin_port`，
///   `b-ui` 每次重启 / 升级 / watchdog 拉起的窗口里客户端拉订阅都会 502，一条一个 token；
/// - `:80` 的 HTTP→HTTPS 跳转服务器不走站点路由树（`log_skip` 挂在那棵树上），它的访问日志
///   与 308 的 `Location` 头各带一份末段；Host 跟站点不匹配时这条还会落到 default logger。
///
/// 所以 default 与站点两个 logger 都得挂，`request>uri` 与 `resp_headers>Location` 两个字段
/// 都得过（`regexp` 过滤器对数组字段逐项生效，实测 `Location` 那一项被换掉）。`(?i)` 是因为
/// Caddy 的 `path` 匹配器不分大小写，`/API/SUB/<token>` 也照样被服务到。
const SUB_SEG_REGEXP: &str = "\"(?i)(/api/(?:sub|subscription|clash|nodes)/)[^/?]+\" \"${1}***\"";

/// Caddyfile：只反代面板端口，日志进 stderr（journald 收），不再写 `/var/log/caddy`。
///
/// 面板块的访问日志会把完整 URI 写进 journald，而四个免鉴权订阅端点的路径末段本身就是
/// 凭据 ⇒ 给它们加一条 `log_skip`（[`SUB_PATHS_MATCHER`]，2026-09-14 裁决），其余请求
/// 照旧记日志；`log_skip` 挡不到的那两条路由由 [`SUB_SEG_REGEXP`] 兜住。守护进程自己那一侧
/// 的脱敏在 `crate::redact::sub_path`。
///
/// default logger 的 `wrap` 必须是 **json**：哨兵按 JSON 解 caddy 的证书失败行
/// （`crate::modules::sentinel` 的 `signature::caddy` 先认 `"level":"error"` 再 `serde_json`
/// 解 `identifier`），换成 console 那条告警就静默失效（2026-09-14 实测：`wrap console` 之后
/// 整份 stderr 里 `"level":"error"` 出现 0 次）。站点 logger 照旧 console（访问日志给人看）。
///
/// `sites_glob` 是外部站点通道的 import 通配（2026-09-13 裁决），由 [`crate::paths`] 派生传进来，
/// **必须排在面板站点块之后**：写进块里就变成站点内指令了。glob 一个文件都没匹配到时
/// caddy 不报错（2.10 实测），所以目录空着也照样 `Valid configuration`。
pub fn caddyfile_text(domain: &str, admin_port: u16, sites_glob: &str) -> String {
    format!(
        "\
# B-UI v4 —— 由 bui 对账器生成，手改会被覆盖
{{
\t# caddy 自己的日志（含 reverse_proxy 的错误日志与 :80 跳转）里也有订阅链接的末段；
\t# wrap 必须留 json，哨兵按 JSON 解证书失败行
\tlog default {{
\t\toutput stderr
\t\tformat filter {{
\t\t\twrap json
\t\t\tfields {{
\t\t\t\trequest>uri regexp {SUB_SEG_REGEXP}
\t\t\t\tresp_headers>Location regexp {SUB_SEG_REGEXP}
\t\t\t}}
\t\t}}
\t}}
}}

{domain} {{
\t# 订阅链接的末段就是凭据，不进访问日志
\t@sub path {SUB_PATHS_MATCHER}
\tlog_skip @sub
\treverse_proxy 127.0.0.1:{admin_port}
\tlog {{
\t\toutput stderr
\t\tformat filter {{
\t\t\twrap console
\t\t\tfields {{
\t\t\t\trequest>uri regexp {SUB_SEG_REGEXP}
\t\t\t\tresp_headers>Location regexp {SUB_SEG_REGEXP}
\t\t\t}}
\t\t}}
\t}}
}}

# 外部站点：{sites_glob} 里的文件归用户管，bui 不覆盖也不删除
import {sites_glob}
"
    )
}

impl Module for CoreFilesModule {
    fn name(&self) -> &'static str {
        "core-files"
    }

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::with_capacity(9);
        // 锁被别的线程 poison 也不能让对账崩：退化成「没有 manifest」
        let guard = self.manifest.read().ok();
        if let Some(m) = guard.as_ref().and_then(|g| g.as_ref()) {
            for name in KERNELS {
                match m.kernel_asset(name, &ctx.facts.arch) {
                    Ok((version, asset)) => out.push(Artifact::Binary {
                        name: name.to_string(),
                        version: version.to_string(),
                        sha256: asset.sha256.clone(),
                        url: asset.url.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(kernel = name, error = %e, "manifest 缺该内核资产，跳过")
                    }
                }
            }
        }
        let p = &ctx.paths;
        out.push(
            Artifact::file(
                p.base_dir.join("config.yaml"),
                bui_schema::render::hysteria::direct_yaml(&s.node, p, s.system.hy2_auth),
            )
            .restart(Unit::restart("hysteria-server")),
        );
        // 每槽一个住宅实例的配置（spec §5.6）；改了哪一槽只重启那一槽的单元
        let live = bui_schema::slots::indices(&s.residential);
        for i in live.iter().copied() {
            let res = bui_schema::slots::resources_of(&s.node.ports, &s.residential, i);
            out.push(
                Artifact::file(
                    resi_config_path(p, i),
                    bui_schema::render::hysteria::residential_slot_yaml(
                        &s.node,
                        p,
                        &res,
                        s.system.hy2_auth,
                    ),
                )
                .restart(Unit::restart(&crate::reconcile::resi_unit(i))),
            );
        }
        // 池缩小之后留下的配置文件要删掉：留着不会被加载，但会被漂移扫描
        // 报成「受管目录里的陌生文件」，体检永久 degraded
        for i in 1..crate::reconcile::MAX_RESI_SLOTS {
            if !live.contains(&i) {
                out.push(Artifact::Absent {
                    path: resi_config_path(p, i),
                });
            }
        }
        let xray = bui_schema::render::xray::config(&s.node, &s.users, &s.residential, p);
        let hash = bui_schema::render::xray::structural_hash(&xray);
        out.push(
            Artifact::file(
                p.base_dir.join("xray-config.json"),
                serde_json::to_vec_pretty(&xray).expect("xray 配置必须可序列化"),
            )
            .verify(Verify::Xray)
            .restart(Unit::restart("xray"))
            .restart_key(hash),
        );
        let group = s.residential.default_group().cloned().unwrap_or_default();
        let relay = bui_schema::render::relay::config(
            &group,
            &bui_schema::slots::sorted(&s.residential),
            &relay_opts(s, p),
        );
        out.push(
            Artifact::file(
                p.base_dir.join("singbox-relay.json"),
                serde_json::to_vec_pretty(&relay).expect("relay 配置必须可序列化"),
            )
            .verify(Verify::SingBox)
            .restart(Unit::restart("b-ui-relay")),
        );
        // 外部站点通道（2026-09-13 裁决）：占位 README 只为把目录建出来（写文件会建父目录），
        // 排在 Caddyfile 之前，这样首装当轮 caddy validate 时目录已经在了。
        out.push(Artifact::file(crate::paths::caddy_sites_readme(p), SITES_README).mode(0o644));
        out.push(
            Artifact::file(
                crate::paths::caddyfile(p),
                caddyfile_text(&s.node.domain, s.node.ports.admin, &sites_glob(p)),
            )
            .mode(0o644)
            .verify(Verify::Caddy)
            .restart(Unit::reload("caddy")),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx, Unit, Verify};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    fn ctx() -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 1,
                systemd_resolved: false,
            },
        }
    }

    /// 总纲 C4 形状（`kernels` 用下划线键、`artifacts` 用 `<name>-linux-<arch>` 键）
    fn manifest() -> Manifest {
        let asset = |u: &str| Asset {
            url: u.into(),
            sha256: "00".into(),
        };
        Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([
                ("hysteria".to_string(), "2.12.2".to_string()),
                ("xray".to_string(), "26.3.27".to_string()),
                ("sing_box".to_string(), "1.13.19".to_string()),
                ("caddy".to_string(), "2.10.2".to_string()),
            ]),
            artifacts: BTreeMap::from([
                ("bui-linux-amd64".to_string(), asset("https://x/bui")),
                ("hysteria-linux-amd64".to_string(), asset("https://x/hy")),
                ("xray-linux-amd64".to_string(), asset("https://x/xray")),
                ("sing-box-linux-amd64".to_string(), asset("https://x/sb")),
                ("caddy-linux-amd64".to_string(), asset("https://x/caddy")),
            ]),
            min_upgrade_from: None,
            tag: None,
        }
    }

    fn find_file(arts: &[Artifact], want: &str) -> Artifact {
        arts.iter()
            .find(|a| matches!(a, Artifact::File { path, .. } if path.to_str() == Some(want)))
            .cloned()
            .unwrap_or_else(|| panic!("没有渲染 {want}"))
    }

    #[test]
    fn renders_four_binaries_then_six_files() {
        let arts = CoreFilesModule::new(Some(manifest())).render(&sample_state(), &ctx());
        let bins: Vec<(String, String, String)> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Binary {
                    name, version, url, ..
                } => Some((name.clone(), version.clone(), url.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            bins,
            vec![
                (
                    "hysteria".to_string(),
                    "2.12.2".to_string(),
                    "https://x/hy".to_string()
                ),
                (
                    "xray".to_string(),
                    "26.3.27".to_string(),
                    "https://x/xray".to_string()
                ),
                (
                    "sing-box".to_string(),
                    "1.13.19".to_string(),
                    "https://x/sb".to_string()
                ),
                (
                    "caddy".to_string(),
                    "2.10.2".to_string(),
                    "https://x/caddy".to_string()
                ),
            ]
        );
        let files: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::File { path, .. } => Some(path.display().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            files,
            vec![
                "/opt/b-ui/config.yaml",
                "/opt/b-ui/config-residential.yaml",
                "/opt/b-ui/xray-config.json",
                "/opt/b-ui/singbox-relay.json",
                "/opt/b-ui/caddy/sites/README.txt",
                "/opt/b-ui/Caddyfile",
            ]
        );
    }

    #[test]
    fn without_a_manifest_only_the_files_are_rendered() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Binary { .. })));
        // 六份配置文件 + 空槽位（1..MAX）的配置清理项
        assert_eq!(
            arts.iter()
                .filter(|a| matches!(a, Artifact::File { .. }))
                .count(),
            6
        );
        assert_eq!(
            arts.len(),
            6 + usize::from(crate::reconcile::MAX_RESI_SLOTS - 1)
        );
    }

    #[test]
    fn hysteria_files_carry_the_schema_output_and_the_right_restart() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let expect = bui_schema::render::hysteria::direct_yaml(
            &s.node,
            &Paths::default_server(),
            s.system.hy2_auth,
        );
        match find_file(&arts, "/opt/b-ui/config.yaml") {
            Artifact::File {
                content,
                mode,
                restart,
                verify,
                restart_key,
                immutable,
                ..
            } => {
                assert_eq!(
                    String::from_utf8(content).unwrap(),
                    expect,
                    "必须原样用 bui-schema 的渲染结果"
                );
                assert_eq!(mode, 0o600);
                assert_eq!(restart, Some(Unit::restart("hysteria-server")));
                assert_eq!(verify, None);
                assert_eq!(restart_key, None);
                assert!(!immutable);
            }
            other => panic!("{other:?}"),
        }
        match find_file(&arts, "/opt/b-ui/config-residential.yaml") {
            Artifact::File {
                content, restart, ..
            } => {
                assert_eq!(
                    String::from_utf8(content).unwrap(),
                    bui_schema::render::hysteria::residential_yaml(
                        &s.node,
                        &Paths::default_server(),
                        s.system.hy2_auth,
                    )
                );
                assert_eq!(restart, Some(Unit::restart("hysteria-residential")));
            }
            other => panic!("{other:?}"),
        }
    }

    /// `state.system.hy2_auth` 是两份 hysteria 配置里 `auth` 段的唯一来源（2026-09-13 裁决）：
    /// 默认 http，切成 command 时两份都跟着变，于是对账各重启一次实例。
    #[test]
    fn the_auth_mode_from_state_reaches_both_hysteria_configs() {
        let text = |s: &State, path: &str| match find_file(
            &CoreFilesModule::new(None).render(s, &ctx()),
            path,
        ) {
            Artifact::File { content, .. } => String::from_utf8(content).unwrap(),
            other => panic!("{other:?}"),
        };
        let mut s = sample_state();
        assert_eq!(s.system.hy2_auth, bui_schema::model::Hy2Auth::Http);
        for p in ["/opt/b-ui/config.yaml", "/opt/b-ui/config-residential.yaml"] {
            let t = text(&s, p);
            assert!(t.contains("url: http://127.0.0.1:18789/auth"), "{p}:\n{t}");
            assert!(!t.contains("bui-auth-hook"), "{p}:\n{t}");
        }
        s.system.hy2_auth = bui_schema::model::Hy2Auth::Command;
        for p in ["/opt/b-ui/config.yaml", "/opt/b-ui/config-residential.yaml"] {
            let t = text(&s, p);
            assert!(
                t.contains("command: /opt/b-ui/bin/bui-auth-hook"),
                "{p}:\n{t}"
            );
            assert!(!t.contains("18789"), "{p}:\n{t}");
        }
    }

    #[test]
    fn xray_file_uses_the_structural_hash_as_its_restart_key() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let cfg = bui_schema::render::xray::config(
            &s.node,
            &s.users,
            &s.residential,
            &Paths::default_server(),
        );
        let hash = bui_schema::render::xray::structural_hash(&cfg);
        match find_file(&arts, "/opt/b-ui/xray-config.json") {
            Artifact::File {
                content,
                restart,
                restart_key,
                verify,
                ..
            } => {
                let got: serde_json::Value = serde_json::from_slice(&content).unwrap();
                assert_eq!(got, cfg);
                assert_eq!(restart, Some(Unit::restart("xray")));
                assert_eq!(restart_key, Some(hash));
                assert_eq!(verify, Some(Verify::Xray));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn adding_a_user_changes_the_content_but_not_the_restart_key() {
        let mut s = sample_state();
        let before = CoreFilesModule::new(None).render(&s, &ctx());
        let mut bob = s.users[0].clone();
        bob.username = "bob".into();
        bob.user_id = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000bb").unwrap();
        bob.credentials.vless_uuid =
            uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        s.users.push(bob);
        let after = CoreFilesModule::new(None).render(&s, &ctx());
        let key = |arts: &[Artifact]| match find_file(arts, "/opt/b-ui/xray-config.json") {
            Artifact::File {
                restart_key,
                content,
                ..
            } => (restart_key, content),
            other => panic!("{other:?}"),
        };
        let (k1, c1) = key(&before);
        let (k2, c2) = key(&after);
        assert_ne!(c1, c2, "clients 变了");
        assert_eq!(k1, k2, "结构没变 → 不重启 xray（eff-C7 的正解）");
    }

    #[test]
    fn relay_file_uses_the_fixed_local_ports_and_singbox_verify() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let opts = relay_opts(&s, &Paths::default_server());
        assert_eq!(opts.listen_port, RELAY_LISTEN_PORT);
        assert_eq!(opts.api, RELAY_CLASH_API);
        assert_eq!(opts.cache_path, "/opt/b-ui/relay-cache.db");
        assert_eq!(opts.server_ip.as_deref(), Some("203.0.113.10"));
        let expect = bui_schema::render::relay::config(
            s.residential.default_group().unwrap(),
            &bui_schema::slots::sorted(&s.residential),
            &opts,
        );
        match find_file(&arts, "/opt/b-ui/singbox-relay.json") {
            Artifact::File {
                content,
                restart,
                verify,
                mode,
                ..
            } => {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&content).unwrap(),
                    expect
                );
                assert_eq!(restart, Some(Unit::restart("b-ui-relay")));
                assert_eq!(verify, Some(Verify::SingBox));
                assert_eq!(mode, 0o600);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn caddyfile_reverse_proxies_the_admin_port_and_reloads() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        match find_file(&arts, "/opt/b-ui/Caddyfile") {
            Artifact::File {
                content,
                mode,
                restart,
                verify,
                ..
            } => {
                let text = String::from_utf8(content).unwrap();
                assert!(text.contains("example.com {"));
                assert!(text.contains("reverse_proxy 127.0.0.1:8080"));
                assert!(
                    text.contains("output stderr"),
                    "日志进 journald，不再写 /var/log/caddy"
                );
                assert_eq!(mode, 0o644);
                assert_eq!(restart, Some(Unit::reload("caddy")));
                assert_eq!(verify, Some(Verify::Caddy));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            caddyfile_text("example.com", 8080, "/opt/b-ui/caddy/sites/*.caddy"),
            caddyfile_text("example.com", 8080, "/opt/b-ui/caddy/sites/*.caddy")
        );
    }

    /// 2026-09-14 裁决「每用户随机订阅 token」：四个免鉴权订阅端点的末段是凭据，
    /// 面板块的访问日志（`output stderr` → journald）不许收它们；其余请求照旧记。
    #[test]
    fn caddyfile_skips_the_access_log_for_the_four_subscription_paths() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        let text = match find_file(&arts, "/opt/b-ui/Caddyfile") {
            Artifact::File { content, .. } => String::from_utf8(content).unwrap(),
            other => panic!("{other:?}"),
        };
        assert!(
            text.contains("\t@sub path /api/sub/* /api/subscription/* /api/clash/* /api/nodes/*\n"),
            "四条路径一条都不能少：\n{text}"
        );
        assert!(text.contains("\tlog_skip @sub\n"), "\n{text}");
        // 必须在面板站点块**里面**（块外的 log_skip 不是合法的顶层指令）。
        // 顶格的 `\n}\n` 现在第一处是全局选项块的，所以从站点块开头往后找
        let block_start = text.find("\nexample.com {\n").expect("面板站点块的开头");
        let block_end = block_start
            + 1
            + text[block_start + 1..]
                .find("\n}\n")
                .expect("面板块顶格的右花括号");
        let at = text.find("log_skip @sub").expect("log_skip 行");
        assert!(
            at > block_start && at < block_end,
            "log_skip 得落在面板站点块里：\n{text}"
        );
        // 只跳这四条，别把整个面板的访问日志一起关掉
        assert!(text.contains("output stderr"), "\n{text}");
    }

    /// 2026-09-14 审查意见 1/2：`log_skip` 只挡站点路由树里的访问日志，`reverse_proxy` 的
    /// 错误日志（落 default logger）与 `:80` 跳转服务器的访问日志 + 308 的 `Location` 头
    /// 照旧把末段写进 journald ⇒ 两个 logger、两个字段都得挂 `format filter` 的 `regexp`。
    /// default 的 `wrap` 必须留 json：哨兵按 JSON 解 caddy 的证书失败行。
    #[test]
    fn caddyfile_masks_the_subscription_segment_in_both_loggers() {
        let text = caddyfile_text("example.com", 8080, "/opt/b-ui/caddy/sites/*.caddy");
        let field = |f: &str| format!("\t\t\t\t{f} regexp {SUB_SEG_REGEXP}\n");
        assert_eq!(
            text.matches(&field("request>uri")).count(),
            2,
            "uri 过滤要挂在 default 与站点两个 logger 上：\n{text}"
        );
        assert_eq!(
            text.matches(&field("resp_headers>Location")).count(),
            2,
            "308 跳转的 Location 头里也有一份末段：\n{text}"
        );
        // 全局选项块（default logger）在最前，站点块在后
        let global = text.find("\n{\n").expect("全局选项块");
        let site = text.find("\nexample.com {\n").expect("面板站点块");
        assert!(global < site, "全局选项块必须是第一个块：\n{text}");
        assert!(
            text[global..site].contains("\t\t\twrap json\n"),
            "default logger 换成 console 会让哨兵的 caddy 证书告警静默失效：\n{text}"
        );
        assert!(
            text[site..].contains("\t\t\twrap console\n"),
            "站点的访问日志照旧 console：\n{text}"
        );
    }

    /// 2026-09-13 裁决「P1：Caddy 外部站点通道」：面板块之后追加 import 行，
    /// 目录由对账器建出来（`README.txt` 占位），目录里的 `*.caddy` 归用户管。
    #[test]
    fn caddyfile_imports_the_external_sites_dir_after_the_panel_block() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        let text = match find_file(&arts, "/opt/b-ui/Caddyfile") {
            Artifact::File { content, .. } => String::from_utf8(content).unwrap(),
            other => panic!("{other:?}"),
        };
        let import = "import /opt/b-ui/caddy/sites/*.caddy";
        let at = text
            .find(import)
            .unwrap_or_else(|| panic!("Caddyfile 缺 import 行：\n{text}"));
        assert!(
            at > text.rfind('}').expect("面板块的右花括号"),
            "import 必须在面板站点块**之后**（块内的 import 会被当成站点内指令）：\n{text}"
        );
        // 目录得有人建：渲染一个占位 README（0644、不是 *.caddy 所以不会被 import 进去）
        match find_file(&arts, "/opt/b-ui/caddy/sites/README.txt") {
            Artifact::File {
                mode,
                restart,
                verify,
                content,
                ..
            } => {
                assert_eq!(mode, 0o644);
                assert_eq!(restart, None, "占位文件变了不该 reload caddy");
                assert_eq!(verify, None);
                assert!(String::from_utf8(content).unwrap().contains(".caddy"));
            }
            other => panic!("{other:?}"),
        }
        // 用户的站点文件永远不是 artifact（不会被覆盖），也不会被 Absent 删掉
        assert!(
            !arts.iter().any(|a| match a {
                Artifact::File { path, .. } | Artifact::Absent { path } =>
                    path.to_str().is_some_and(|p| p.ends_with(".caddy")),
                _ => false,
            }),
            "bui 不许把任何 *.caddy 纳入受管"
        );
    }

    #[test]
    fn render_is_pure() {
        let s = sample_state();
        assert_eq!(
            CoreFilesModule::new(Some(manifest())).render(&s, &ctx()),
            CoreFilesModule::new(Some(manifest())).render(&s, &ctx())
        );
    }

    #[test]
    fn refreshing_the_shared_manifest_changes_the_next_render() {
        // 每日自检写这个锁，下一轮对账就按新版本装内核（spec §7）
        let m = CoreFilesModule::new(None);
        let s = sample_state();
        assert!(!m
            .render(&s, &ctx())
            .iter()
            .any(|a| matches!(a, Artifact::Binary { .. })));
        *m.manifest_handle().write().unwrap() = Some(manifest());
        let bins = m
            .render(&s, &ctx())
            .into_iter()
            .filter(|a| matches!(a, Artifact::Binary { .. }))
            .count();
        assert_eq!(bins, 4);
    }

    /// P1 唯一自己渲染（不经 P0 golden 覆盖）的内核输入就是 Caddyfile，
    /// 所以拿真实 caddy 过一次 `validate`；二进制不在就 skip（Global Constraints）。
    #[test]
    fn caddyfile_passes_a_real_caddy_validate() {
        let caddy = [
            "/opt/b-ui/bin/caddy",
            "/usr/bin/caddy",
            "/usr/local/bin/caddy",
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists());
        let Some(caddy) = caddy else {
            eprintln!("skipped: 本机没有 caddy 二进制");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        // 连外部站点通道一起过：sites/ 里放一个真实的外部站点块（裁决 P1）
        let p = Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        std::fs::create_dir_all(crate::paths::caddy_sites(&p)).unwrap();
        std::fs::write(crate::paths::caddy_sites_readme(&p), SITES_README).unwrap();
        std::fs::write(
            crate::paths::caddy_sites(&p).join("blog.caddy"),
            "blog.example.com {\n\trespond \"hi\"\n}\n",
        )
        .unwrap();
        let f = d.path().join("Caddyfile");
        std::fs::write(&f, caddyfile_text("example.com", 8080, &sites_glob(&p))).unwrap();
        // caddy validate 会 provision tls 模块，可能往 ~/.local/share/caddy 建目录；
        // 把两个 XDG 目录指到 tempdir，保持测试封闭（不碰开发机的家目录）
        let out = std::process::Command::new(caddy)
            .args([
                "validate",
                "--config",
                f.to_str().unwrap(),
                "--adapter",
                "caddyfile",
            ])
            .env("XDG_DATA_HOME", d.path())
            .env("XDG_CONFIG_HOME", d.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "caddy validate 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 造一份 n 槽的期望态（上游 uuid = index+1，与 T1 的夹具同规则）。
    fn state_with_slots(n: u16) -> bui_schema::model::State {
        let mut s = sample_state();
        let g = s
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        g.enabled = true;
        g.upstreams = (0..n)
            .map(|i| bui_schema::model::Upstream {
                id: uuid::Uuid::from_u128(u128::from(i) + 1),
                name: format!("url-{}", i + 1),
                kind: bui_schema::model::UpstreamKind::Socks5,
                host: format!("isp{}.example.net", i + 1),
                port: 10007,
                username: "user1".into(),
                password: "pw1".into(),
                priority: 100,
                provider: None,
                region: None,
                ports_allowed: None,
                verified: None,
            })
            .collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        s.residential.slots = (0..n)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        s
    }

    #[test]
    fn a_single_slot_renders_exactly_one_residential_config_named_like_v3() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        let files: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::File { path, .. } => Some(path.display().to_string()),
                _ => None,
            })
            .collect();
        assert!(files.contains(&"/opt/b-ui/config-residential.yaml".to_string()));
        assert!(!files.iter().any(|f| f.contains("config-residential-")));
        // 空槽位的清理项覆盖 1..MAX
        for i in 1..crate::reconcile::MAX_RESI_SLOTS {
            assert!(
                arts.iter().any(|a| matches!(a, Artifact::Absent { path }
                    if path.to_str() == Some(&format!("/opt/b-ui/config-residential-{i}.yaml")))),
                "槽 {i} 的配置没有清理项"
            );
        }
    }

    #[test]
    fn three_slots_render_three_residential_configs_with_their_own_restart_targets() {
        let s = state_with_slots(3);
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let p = Paths::default_server();
        for i in 0..3u16 {
            let want = bui_schema::render::hysteria::residential_slot_yaml(
                &s.node,
                &p,
                &bui_schema::slots::resources_of(&s.node.ports, &s.residential, i),
                s.system.hy2_auth,
            );
            let path = crate::modules::core_files::resi_config_path(&p, i);
            match find_file(&arts, path.to_str().unwrap()) {
                Artifact::File {
                    content, restart, ..
                } => {
                    assert_eq!(String::from_utf8(content).unwrap(), want, "槽 {i} 内容");
                    assert_eq!(
                        restart,
                        Some(Unit::restart(&crate::reconcile::resi_unit(i))),
                        "槽 {i} 只重启自己那个实例"
                    );
                }
                other => panic!("{other:?}"),
            }
        }
        // 只剩 3..MAX 需要清理
        assert!(arts.iter().any(|a| matches!(a, Artifact::Absent { path }
            if path.to_str() == Some("/opt/b-ui/config-residential-3.yaml"))));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Absent { path }
            if path.to_str() == Some("/opt/b-ui/config-residential-2.yaml"))));
    }
}
