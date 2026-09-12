//! 四个内核二进制 + 它们的五份配置（`config.yaml` / `config-residential.yaml` /
//! `xray-config.json` / `singbox-relay.json` / `Caddyfile`）。
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

/// 四个内核二进制 + 它们的五份配置（唯一来源是 bui-schema 的渲染器）。
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

/// Caddyfile：只反代面板端口，日志进 stderr（journald 收），不再写 `/var/log/caddy`。
pub fn caddyfile_text(domain: &str, admin_port: u16) -> String {
    format!(
        "\
# B-UI v4 —— 由 bui 对账器生成，手改会被覆盖
{domain} {{
\treverse_proxy 127.0.0.1:{admin_port}
\tlog {{
\t\toutput stderr
\t\tformat console
\t}}
}}
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
                bui_schema::render::hysteria::direct_yaml(&s.node, p),
            )
            .restart(Unit::restart("hysteria-server")),
        );
        out.push(
            Artifact::file(
                p.base_dir.join("config-residential.yaml"),
                bui_schema::render::hysteria::residential_yaml(&s.node, p),
            )
            .restart(Unit::restart("hysteria-residential")),
        );
        let xray = bui_schema::render::xray::config(&s.node, &s.users, p);
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
        let relay = bui_schema::render::relay::config(&group, &relay_opts(s, p));
        out.push(
            Artifact::file(
                p.base_dir.join("singbox-relay.json"),
                serde_json::to_vec_pretty(&relay).expect("relay 配置必须可序列化"),
            )
            .verify(Verify::SingBox)
            .restart(Unit::restart("b-ui-relay")),
        );
        out.push(
            Artifact::file(
                crate::paths::caddyfile(p),
                caddyfile_text(&s.node.domain, s.node.ports.admin),
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
        }
    }

    fn find_file(arts: &[Artifact], want: &str) -> Artifact {
        arts.iter()
            .find(|a| matches!(a, Artifact::File { path, .. } if path.to_str() == Some(want)))
            .cloned()
            .unwrap_or_else(|| panic!("没有渲染 {want}"))
    }

    #[test]
    fn renders_four_binaries_then_five_files() {
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
                "/opt/b-ui/Caddyfile",
            ]
        );
    }

    #[test]
    fn without_a_manifest_only_the_files_are_rendered() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Binary { .. })));
        assert_eq!(arts.len(), 5);
    }

    #[test]
    fn hysteria_files_carry_the_schema_output_and_the_right_restart() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let expect = bui_schema::render::hysteria::direct_yaml(&s.node, &Paths::default_server());
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
                        &Paths::default_server()
                    )
                );
                assert_eq!(restart, Some(Unit::restart("hysteria-residential")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn xray_file_uses_the_structural_hash_as_its_restart_key() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let cfg = bui_schema::render::xray::config(&s.node, &s.users, &Paths::default_server());
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
        let expect =
            bui_schema::render::relay::config(s.residential.default_group().unwrap(), &opts);
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
            caddyfile_text("example.com", 8080),
            caddyfile_text("example.com", 8080)
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
        let f = d.path().join("Caddyfile");
        std::fs::write(&f, caddyfile_text("example.com", 8080)).unwrap();
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
}
