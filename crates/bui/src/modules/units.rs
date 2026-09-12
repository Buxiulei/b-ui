//! systemd 单元模块：六个 v4 单元的完整单元文件、两个 CLI 入口符号链接、
//! 六个单元的启用态，以及 v3 遗留单元 / 遗留文件的删除项。
//!
//! 移植参照：`server/core.sh:197-274`（两个 hysteria 的资源限制与内存调优）、
//! `server/core.sh:1722-1790`（面板单元 + xray drop-in）、
//! `server/residential-helper.sh:507-522`（relay 单元）、`server/core.sh:884-891`（caddy drop-in）。
//! v4 的变化（spec §3.4、审计 §3.1/§3.2）：六个单元都写**完整单元文件**（四个内核二进制都在
//! `<base>/bin/`，不再 drop-in 覆盖发行版单元）；删掉两个 hysteria 的
//! `ExecStartPre=…hy2-portjump-cleanup.sh`（v4 无任何 iptables/nft 规则）；`xray` 与 `b-ui-relay`
//! 补上 v3 缺的 `MemoryHigh/MemoryMax`；全部 `LimitNOFILE=1048576`，四个数据面单元 `Nice=-5`；
//! `b-ui.service` 自身 `MemoryMax=200M`；不生成任何 timer。

use crate::paths::{caddy_xdg, CLI_LINKS};
use crate::reconcile::{Artifact, Module, RenderCtx, Unit, LEGACY_UNITS, MANAGED_UNITS};
use bui_schema::model::State;

/// v3 留下的、不属于任何 v4 单元的散落文件。
///
/// `/etc/sysctl.d/99-hysteria-bbr.conf` 是 v3 的 BBR 文件（`server/core.sh:1349` 写
/// `net.core.default_qdisc` + `net.ipv4.tcp_congestion_control=<algo>`，`server/update.sh:1200-1211`
/// 的 D2 块还会把 `<algo>` 升成 `bbr3`/`bbrv3`/`bbr_v3`）。v4 把这两个键并进 system 模块的
/// `99-b-ui-network.conf`（值 `bbr`），而 `sysctl --system` 按文件名升序加载——
/// `99-hysteria-bbr.conf` 排在 `99-b-ui-network.conf` **之后**，留着它就等于 v3 的值每次开机
/// 覆盖 v4 的期望值（对账只 `sysctl -w` 当前值，看不出下次开机会被翻回去）。它的文件名不带
/// `b-ui` 前缀，`stray_conf` 那类漂移也扫不到，只能在这里显式删。
///
/// 反过来，`99-b-ui-memory.conf` **不在**这份名单里：那是 system 模块在 ≤2G 机器上接管的同名
/// 文件，放进来会让两个模块每轮互斗（system 写、units 删），对账永不收敛。
pub const LEGACY_FILES: [&str; 9] = [
    "/etc/systemd/system/hysteria-server.service.d/override.conf",
    "/etc/systemd/system/hysteria-server.service.d/priority.conf",
    "/etc/systemd/system/xray.service.d/99-b-ui-override.conf",
    "/etc/systemd/system/caddy.service.d/override.conf",
    "/etc/sysctl.d/99-hysteria-perf.conf",
    "/etc/sysctl.d/99-hysteria-bbr.conf",
    "/opt/b-ui/hy2-portjump-cleanup.sh",
    "/opt/b-ui/hy2-watchdog.sh",
    "/opt/b-ui/cert-sync.sh",
];

pub struct UnitsModule;

impl Module for UnitsModule {
    fn name(&self) -> &'static str {
        "units"
    }

    fn render(&self, _s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::new();

        // 六个单元文件。`name` 一律 `Unit::restart`，caddy 也不例外：单元文件本身变了必须
        // `systemctl restart`，`reload` 不会让新的 ExecStart / Environment / MemoryMax 生效。
        // （`Unit::reload` 只用在两个 `Artifact::File` 上：`<base>/Caddyfile` 与
        // `/etc/ssh/sshd_config.d/00-b-ui-hardening.conf`。）
        for name in MANAGED_UNITS {
            out.push(Artifact::Unit {
                name: Unit::restart(name),
                dropin: None,
                content: unit_text(name, ctx),
            });
        }

        // 两个 CLI 入口符号链接：`bui reconcile` 能自愈被误删的入口，`sudo b-ui` 由此落地。
        let target = ctx.paths.bin_dir.join("bui");
        for link in CLI_LINKS {
            out.push(Artifact::Symlink {
                path: link.into(),
                target: target.clone(),
            });
        }

        // 六个单元都 enable + start。
        for name in MANAGED_UNITS {
            out.push(Artifact::UnitState {
                name: name.to_string(),
                enabled: true,
                active: true,
            });
        }

        // v3 遗留单元：先全部停用，再删单元文件。
        for legacy in LEGACY_UNITS {
            out.push(Artifact::UnitState {
                name: legacy.to_string(),
                enabled: false,
                active: false,
            });
        }
        for legacy in LEGACY_UNITS {
            out.push(Artifact::Absent {
                path: format!("/etc/systemd/system/{legacy}").into(),
            });
        }

        for f in LEGACY_FILES {
            out.push(Artifact::Absent { path: f.into() });
        }

        out
    }
}

/// 渲染一个受管单元的完整正文。`name` 必须是 [`MANAGED_UNITS`] 里的一个。
pub fn unit_text(name: &str, ctx: &RenderCtx) -> String {
    let bin = ctx.paths.bin_dir.display();
    let base = ctx.paths.base_dir.display();
    match name {
        "b-ui" => format!(
            "[Unit]
Description=B-UI v4 Controller
Documentation=https://github.com/Buxiulei/b-ui
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/bui serve
Restart=always
RestartSec=3
LimitNOFILE=1048576
MemoryMax=200M
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
"
        ),
        "hysteria-server" => format!(
            "[Unit]
Description=Hysteria Server (Direct)
Documentation=https://v2.hysteria.network/
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/hysteria server --config {base}/config.yaml
User=root
Group=root
Restart=always
RestartSec=3
TimeoutStopSec=15
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=400MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=500M
MemoryMax=700M

[Install]
WantedBy=multi-user.target
"
        ),
        "hysteria-residential" => format!(
            "[Unit]
Description=Hysteria Server (Residential)
Documentation=https://v2.hysteria.network/
After=network-online.target b-ui-relay.service
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/hysteria server --config {base}/config-residential.yaml
User=root
Group=root
Restart=always
RestartSec=3
TimeoutStopSec=15
LimitNOFILE=1048576
LimitNPROC=512
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=200MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=300M
MemoryMax=500M

[Install]
WantedBy=multi-user.target
"
        ),
        "xray" => format!(
            "[Unit]
Description=Xray Service (VLESS-REALITY)
Documentation=https://xtls.github.io/
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/xray run -config {base}/xray-config.json
Restart=always
RestartSec=3
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
MemoryHigh=300M
MemoryMax=500M

[Install]
WantedBy=multi-user.target
"
        ),
        "b-ui-relay" => format!(
            "[Unit]
Description=B-UI Outbound Relay (sing-box)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/sing-box run -c {base}/singbox-relay.json
Restart=always
RestartSec=3
LimitNOFILE=1048576
Nice=-5
MemoryHigh=200M
MemoryMax=300M
LogRateLimitIntervalSec=10s
LogRateLimitBurst=200

[Install]
WantedBy=multi-user.target
"
        ),
        "caddy" => {
            let xdg = caddy_xdg(&ctx.paths);
            let xdg = xdg.display();
            format!(
                "[Unit]
Description=Caddy (B-UI managed)
Documentation=https://caddyserver.com/docs/
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart={bin}/caddy run --config {base}/Caddyfile --adapter caddyfile
ExecReload={bin}/caddy reload --config {base}/Caddyfile --adapter caddyfile --force
Restart=always
RestartSec=5
LimitNOFILE=1048576
Environment=XDG_DATA_HOME={xdg}
Environment=XDG_CONFIG_HOME={xdg}

[Install]
WantedBy=multi-user.target
"
            )
        }
        other => unreachable!("unit_text 只认受管单元，收到 {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

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

    fn unit_of(arts: &[Artifact], name: &str) -> String {
        arts.iter()
            .find_map(|a| match a {
                Artifact::Unit {
                    name: u,
                    content,
                    dropin: None,
                } if u.name == name => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("没有渲染单元 {name}"))
    }

    #[test]
    fn renders_exactly_six_units_and_no_timers() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let names: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Unit { name, .. } => Some(name.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            MANAGED_UNITS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert!(!arts
            .iter()
            .any(|a| matches!(a, Artifact::Unit { name, .. } if name.name.contains(".timer"))));
    }

    #[test]
    fn every_unit_has_the_required_limits() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            let t = unit_of(&arts, name);
            assert!(t.contains("LimitNOFILE=1048576"), "{name} 缺 LimitNOFILE");
            assert!(t.contains("Restart=always"), "{name} 缺 Restart=always");
            assert!(
                t.contains("WantedBy=multi-user.target"),
                "{name} 缺 Install 段"
            );
        }
        for name in [
            "hysteria-server",
            "hysteria-residential",
            "xray",
            "b-ui-relay",
        ] {
            assert!(
                unit_of(&arts, name).contains("Nice=-5"),
                "{name} 缺 Nice=-5"
            );
        }
    }

    #[test]
    fn hysteria_units_keep_v3_memory_tuning_and_drop_the_portjump_hook() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let direct = unit_of(&arts, "hysteria-server");
        assert!(direct
            .contains("ExecStart=/opt/b-ui/bin/hysteria server --config /opt/b-ui/config.yaml"));
        assert!(direct.contains("Environment=GOMEMLIMIT=400MiB"));
        assert!(direct.contains("Environment=HYSTERIA_LOG_LEVEL=warn"));
        assert!(direct.contains("MemoryHigh=500M"));
        assert!(direct.contains("MemoryMax=700M"));
        assert!(direct.contains("TimeoutStopSec=15"));
        assert!(
            !direct.contains("ExecStartPre"),
            "v4 没有端口跳跃 NAT 链，不需要清理钩子"
        );
        let low = direct.to_lowercase();
        assert!(!low.contains("iptables") && !low.contains("nft"));
        let resi = unit_of(&arts, "hysteria-residential");
        assert!(resi.contains(
            "ExecStart=/opt/b-ui/bin/hysteria server --config /opt/b-ui/config-residential.yaml"
        ));
        assert!(resi.contains("Environment=GOMEMLIMIT=200MiB"));
        assert!(resi.contains("MemoryHigh=300M"));
        assert!(resi.contains("MemoryMax=500M"));
        assert!(resi.contains("LimitNPROC=512"));
        assert!(resi.contains("After=network-online.target b-ui-relay.service"));
    }

    #[test]
    fn xray_and_relay_get_the_memory_caps_v3_was_missing() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let xray = unit_of(&arts, "xray");
        assert!(
            xray.contains("ExecStart=/opt/b-ui/bin/xray run -config /opt/b-ui/xray-config.json")
        );
        assert!(xray.contains("MemoryHigh=300M") && xray.contains("MemoryMax=500M"));
        let relay = unit_of(&arts, "b-ui-relay");
        assert!(
            relay.contains("ExecStart=/opt/b-ui/bin/sing-box run -c /opt/b-ui/singbox-relay.json")
        );
        assert!(relay.contains("MemoryHigh=200M") && relay.contains("MemoryMax=300M"));
        assert!(
            relay.contains("LogRateLimitIntervalSec=10s")
                && relay.contains("LogRateLimitBurst=200")
        );
    }

    #[test]
    fn daemon_unit_caps_itself_at_200m() {
        let t = unit_of(&UnitsModule.render(&sample_state(), &ctx()), "b-ui");
        assert!(t.contains("ExecStart=/opt/b-ui/bin/bui serve"));
        assert!(t.contains("MemoryMax=200M"));
        assert!(t.contains("Environment=RUST_LOG=info"));
    }

    #[test]
    fn caddy_unit_uses_the_bundled_binary_with_its_own_data_dir() {
        let t = unit_of(&UnitsModule.render(&sample_state(), &ctx()), "caddy");
        assert!(t.contains(
            "ExecStart=/opt/b-ui/bin/caddy run --config /opt/b-ui/Caddyfile --adapter caddyfile"
        ));
        assert!(t.contains("ExecReload=/opt/b-ui/bin/caddy reload --config /opt/b-ui/Caddyfile --adapter caddyfile --force"));
        assert!(t.contains("Environment=XDG_DATA_HOME=/opt/b-ui/caddy"));
        assert!(t.contains("Environment=XDG_CONFIG_HOME=/opt/b-ui/caddy"));
    }

    #[test]
    fn enables_all_six_and_removes_v3_leftovers() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            assert!(
                arts.contains(&Artifact::UnitState {
                    name: name.into(),
                    enabled: true,
                    active: true
                }),
                "{name} 要 enable+start"
            );
        }
        for legacy in LEGACY_UNITS {
            assert!(
                arts.contains(&Artifact::UnitState {
                    name: legacy.into(),
                    enabled: false,
                    active: false
                }),
                "{legacy} 要停用"
            );
            assert!(
                arts.contains(&Artifact::Absent {
                    path: format!("/etc/systemd/system/{legacy}").into()
                }),
                "{legacy} 的单元文件要删"
            );
        }
        for f in LEGACY_FILES {
            assert!(
                arts.contains(&Artifact::Absent { path: f.into() }),
                "{f} 要删"
            );
        }
    }

    #[test]
    fn v3_sysctl_leftovers_are_deleted_but_the_memory_conf_is_not() {
        // `99-hysteria-bbr.conf`（v3 core.sh:1349 / update.sh:1200-1211 的 D2 块，可能写成 bbr3）
        // 按文件名排在 `99-b-ui-network.conf` 之后 → 不删就每次开机覆盖 v4 的拥塞算法期望值；
        // 而 `99-b-ui-memory.conf` 是 Task 6 在 ≤2G 机器上**接管**的同名文件，
        // 误放进这里会让两个模块每轮互斗（system 写、units 删），对账永不收敛。
        let arts = UnitsModule.render(&sample_state(), &ctx());
        assert!(LEGACY_FILES.contains(&"/etc/sysctl.d/99-hysteria-bbr.conf"));
        assert!(arts.contains(&Artifact::Absent {
            path: "/etc/sysctl.d/99-hysteria-bbr.conf".into()
        }));
        // 路径写字面量、不引 `system::MEMORY_CONF`：Task 6 与本任务是并行的两支，
        // 引过去就凭空多一条编译依赖
        assert!(
            !arts.iter().any(|a| matches!(a, Artifact::Absent { path }
                if path.to_str() == Some("/etc/sysctl.d/99-b-ui-memory.conf"))),
            "99-b-ui-memory.conf 归 system 模块管，不是遗留文件"
        );
    }

    #[test]
    fn v3_leftovers_never_include_a_v4_managed_unit() {
        // b-ui-relay.service 是 v4 自己的单元：如果它出现在遗留清理里，
        // 对账刚写完就会被 disable/stop/删文件，住宅两个节点一起断
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            assert!(
                !arts.contains(&Artifact::UnitState {
                    name: name.into(),
                    enabled: false,
                    active: false
                }),
                "{name} 被当成遗留单元停用了"
            );
            assert!(
                !arts.contains(&Artifact::Absent {
                    path: format!("/etc/systemd/system/{name}.service").into()
                }),
                "{name} 的单元文件被当成遗留文件删了"
            );
        }
    }

    #[test]
    fn both_cli_entry_symlinks_point_at_the_bundled_binary() {
        // spec §1/§2.4：sudo b-ui 是 bui menu 的符号链接
        let arts = UnitsModule.render(&sample_state(), &ctx());
        assert!(arts.contains(&Artifact::Symlink {
            path: "/usr/local/bin/bui".into(),
            target: "/opt/b-ui/bin/bui".into()
        }));
        assert!(arts.contains(&Artifact::Symlink {
            path: "/usr/local/bin/b-ui".into(),
            target: "/opt/b-ui/bin/bui".into()
        }));
    }

    #[test]
    fn render_is_pure_so_the_diff_does_not_flap() {
        let a = UnitsModule.render(&sample_state(), &ctx());
        let b = UnitsModule.render(&sample_state(), &ctx());
        assert_eq!(a, b);
    }
}
