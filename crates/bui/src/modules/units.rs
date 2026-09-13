//! systemd 单元模块：六个 v4 单元的完整单元文件、三条入口符号链接（两个 CLI 入口 +
//! Hysteria2 鉴权钩子 `bin/bui-auth-hook`）、六个单元的启用态，以及 v3 遗留单元 /
//! 遗留文件的删除项。
//!
//! 移植参照：`server/core.sh:197-274`（两个 hysteria 的资源限制与内存调优）、
//! `server/core.sh:1722-1790`（面板单元 + xray drop-in）、
//! `server/residential-helper.sh:507-522`（relay 单元）、`server/core.sh:884-891`（caddy drop-in）。
//! v4 的变化（spec §3.4、审计 §3.1/§3.2）：六个单元都写**完整单元文件**（四个内核二进制都在
//! `<base>/bin/`，不再 drop-in 覆盖发行版单元）；两个 hysteria 的
//! `ExecStartPre=…hy2-portjump-cleanup.sh` 换成内置子命令
//! `ExecStartPre=-{bin}/bui hy2-prestart <该实例的配置>`；`xray` 与 `b-ui-relay`
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
/// `/etc/systemd/system/xray.service.d/10-donot_touch_single_conf.conf` 是**官方 Xray 安装器**
/// （`install-release.sh`）写的 drop-in，内容是 `ExecStart=`（清空）+
/// `ExecStart=/usr/local/bin/xray run -config /usr/local/etc/xray/config.json`。drop-in 的优先级
/// 高于单元文件本体，所以它留在机器上就等于 v4 单元里那条 `ExecStart={bin}/xray run -config
/// {base}/xray-config.json` 形同虚设：2026-09-12 bwg-rick 的 v3→v4 首切现场，xray 显示 active 却跑着
/// v3 的二进制和 v3 的配置，10001/10002 一个端口都没监听，而单元文件本身对账起来毫无差异。
/// 它不带 `b-ui` 前缀、也不是受管 artifact，漂移扫描扫不到，只能在这里显式删。
///
/// 反过来，`99-b-ui-memory.conf` **不在**这份名单里：那是 system 模块在 ≤2G 机器上接管的同名
/// 文件，放进来会让两个模块每轮互斗（system 写、units 删），对账永不收敛。
pub const LEGACY_FILES: [&str; 10] = [
    "/etc/systemd/system/hysteria-server.service.d/override.conf",
    "/etc/systemd/system/hysteria-server.service.d/priority.conf",
    "/etc/systemd/system/xray.service.d/99-b-ui-override.conf",
    "/etc/systemd/system/xray.service.d/10-donot_touch_single_conf.conf",
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

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::new();

        // 六个单元文件。`name` 一律 `Unit::restart`，caddy 也不例外：单元文件本身变了必须
        // `systemctl restart`，`reload` 不会让新的 ExecStart / Environment / MemoryMax 生效。
        // （`Unit::reload` 只用在两个 `Artifact::File` 上：`<base>/Caddyfile` 与
        // `/etc/ssh/sshd_config.d/00-b-ui-hardening.conf`。）
        // `hysteria-residential` 是槽 0，正文由 `resi_unit_text` 出（与槽 1.. 同一模板）；
        // 它必须留在 MANAGED_UNITS 的原位，`renders_exactly_six_units_and_no_timers`
        // 按这个顺序断言。
        for name in MANAGED_UNITS {
            let content = if name == "hysteria-residential" {
                resi_unit_text(0, ctx)
            } else {
                unit_text(name, ctx)
            };
            out.push(Artifact::Unit {
                name: Unit::restart(name),
                dropin: None,
                content,
            });
        }
        // 槽 1.. 的住宅实例（槽 0 已在上面那轮里）
        let live = bui_schema::slots::indices(&s.residential);
        for i in live.iter().copied().filter(|i| *i != 0) {
            let name = crate::reconcile::resi_unit(i);
            out.push(Artifact::Unit {
                name: Unit::restart(&name),
                dropin: None,
                content: resi_unit_text(i, ctx),
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

        // 第三个入口：Hysteria2 `auth.command` 指向的 `<base>/bin/bui-auth-hook`
        // （事故 2026-09-12 / 调研 H15：`auth.command` 只接受单个不带参数的可执行路径，
        // `exec.Command(a.Cmd, addr, auth, tx)` 不过 shell、不拆空格）。
        //
        // 归 units 而不是 core_files：core_files 管的是四个内核二进制与它们的配置，而这条链接
        // 与上面两条 CLI 入口是同一件事——把 `<base>/bin/bui` 暴露成另一个入口名，靠 argv[0]
        // 分发；三条一起渲染，`bui reconcile` 才能一并自愈被误删的入口。
        //
        // 目标写**相对**的 `bui`（同目录）：`bui upgrade` 与 `--rollback` 都是原地换掉
        // `bin/bui` 这个文件，相对链接因此天然指向换上来的那一版。
        out.push(Artifact::Symlink {
            path: ctx.paths.auth_hook_bin(),
            target: "bui".into(),
        });

        // 六个单元都 enable + start。
        for name in MANAGED_UNITS {
            out.push(Artifact::UnitState {
                name: name.to_string(),
                enabled: true,
                active: true,
            });
        }
        for i in live.iter().copied().filter(|i| *i != 0) {
            out.push(Artifact::UnitState {
                name: crate::reconcile::resi_unit(i),
                enabled: true,
                active: true,
            });
        }
        // 池缩小之后多出来的实例：先停用再删单元文件（与 LEGACY_UNITS 同一套手法）
        for i in 1..crate::reconcile::MAX_RESI_SLOTS {
            if live.contains(&i) {
                continue;
            }
            let name = crate::reconcile::resi_unit(i);
            out.push(Artifact::UnitState {
                name: name.clone(),
                enabled: false,
                active: false,
            });
            out.push(Artifact::Absent {
                path: format!("/etc/systemd/system/{name}.service").into(),
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

/// 渲染一个受管单元的完整正文。`name` 必须是 [`MANAGED_UNITS`] 里的一个，
/// **`hysteria-residential` 除外**——住宅实例（含槽 0）一律走 [`resi_unit_text`]。
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
ExecStartPre=-{bin}/bui hy2-prestart {base}/config.yaml
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
        // `User=root` + 零沙箱指令是硬要求（2026-09-13 裁决「Caddy 外部站点通道」）：
        // 从 v3 导过来的外部站点块可能 `tls /etc/caddy/certs/*.crt`，发行版单元的
        // `ProtectSystem=full` 那一套会让这些绝对路径读不到 → 外部站点 443 全挂。
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
User=root
Group=root
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

/// 槽 `index` 的住宅 hysteria 单元正文。与直连实例的差别只有 `--config` 指向的文件
/// 与 Description；资源限制沿用 v3 的住宅档（`GOMEMLIMIT=200MiB` / `MemoryHigh=300M` /
/// `MemoryMax=500M` / `LimitNPROC=512`）。
///
/// **每槽独立的资源上限而不是共享一份**：一条上游抖起来只该影响它自己那个实例。
/// `ExecStartPre` 同样按槽传**自己**那份配置——端口跳跃的 nat 链按实例的
/// base 端口 + 跳跃区间定位，跨实例清理正是 v3.5.1 翻车的写法。
pub fn resi_unit_text(index: u16, ctx: &RenderCtx) -> String {
    let bin = ctx.paths.bin_dir.display();
    let cfg = crate::modules::core_files::resi_config_path(&ctx.paths, index);
    let cfg = cfg.display();
    let slot = if index == 0 {
        String::new()
    } else {
        format!(" slot {index}")
    };
    format!(
        "[Unit]
Description=Hysteria Server (Residential{slot})
Documentation=https://v2.hysteria.network/
After=network-online.target b-ui-relay.service
Wants=network-online.target

[Service]
Type=simple
ExecStartPre=-{bin}/bui hy2-prestart {cfg}
ExecStart={bin}/hysteria server --config {cfg}
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::apply::{apply, ApplyInput, ApplyOutcome, BinaryInstaller};
    use crate::reconcile::diff::{plan, PlanInput};
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx};
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::path::Path;

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
    fn hysteria_units_keep_v3_memory_tuning() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let direct = unit_of(&arts, "hysteria-server");
        assert!(direct
            .contains("ExecStart=/opt/b-ui/bin/hysteria server --config /opt/b-ui/config.yaml"));
        assert!(direct.contains("Environment=GOMEMLIMIT=400MiB"));
        assert!(direct.contains("Environment=HYSTERIA_LOG_LEVEL=warn"));
        assert!(direct.contains("MemoryHigh=500M"));
        assert!(direct.contains("MemoryMax=700M"));
        assert!(direct.contains("TimeoutStopSec=15"));
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

    /// 事故回归（2026-09-12 20:33 UTC bwg-rick，M5 回滚演练之后）：`hysteria-residential`
    /// 崩 52 次，日志「invalid config: listen: ip6tables [-w -t nat -N HYSTERIA-PR-c66a02d9]:
    /// exit status 1: ip6tables: Chain already exists」——内置端口跳跃建的 nat 链在进程被
    /// 非正常终止后残留，新进程 `-N` 同名链即 FATAL。所以两个 hysteria 单元都要在启动前跑
    /// 一次**本实例**的孤儿链清理（逻辑见 `modules::portjump`）。
    ///
    /// 三个点是硬要求：① `ExecStartPre=-` 的 `-` 前缀（清理失败不得阻塞内核启动）；
    /// ② 每个单元只传**自己**那份配置（链按该实例的 base 端口 / 跳跃区间定位，绝不跨实例误删，
    /// 这正是 v3.5.1 共享 cleanup 翻车的修法）；③ 钩子是 `bui` 自己的子命令，不是 shell 脚本、
    /// 单元里也不出现 `iptables`（v3 的 `hy2-portjump-cleanup.sh` 仍在 `LEGACY_FILES` 里被删）。
    #[test]
    fn both_hysteria_units_clean_their_own_portjump_chains_before_start() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let direct = unit_of(&arts, "hysteria-server");
        assert!(
            direct.contains("ExecStartPre=-/opt/b-ui/bin/bui hy2-prestart /opt/b-ui/config.yaml\n"),
            "{direct}"
        );
        let resi = unit_of(&arts, "hysteria-residential");
        assert!(
            resi.contains(
                "ExecStartPre=-/opt/b-ui/bin/bui hy2-prestart /opt/b-ui/config-residential.yaml\n"
            ),
            "{resi}"
        );
        for (name, t) in [
            ("hysteria-server", &direct),
            ("hysteria-residential", &resi),
        ] {
            assert_eq!(
                t.matches("ExecStartPre").count(),
                1,
                "{name} 只该有一条启动前钩子"
            );
            let low = t.to_lowercase();
            assert!(
                !low.contains("iptables") && !low.contains("nft") && !low.contains(".sh"),
                "{name} 的钩子必须是 bui 子命令，不是脚本：{t}"
            );
        }
        // 另外四个单元不该有任何启动前钩子
        for name in ["b-ui", "xray", "b-ui-relay", "caddy"] {
            assert!(
                !unit_of(&arts, name).contains("ExecStartPre"),
                "{name} 不需要端口跳跃清理"
            );
        }
        assert!(LEGACY_FILES.contains(&"/opt/b-ui/hy2-portjump-cleanup.sh"));
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

    /// 2026-09-13 裁决「P1：Caddy 外部站点通道」：导入的站点块可能 `tls /etc/caddy/certs/x.crt …`
    /// （发行版 caddy 以 `caddy` 用户跑、证书就放在那儿）。v4 的 caddy 必须以 **root** 运行且
    /// **不带任何沙箱指令**，否则那些绝对路径读不到 → 外部站点 443 全挂。
    #[test]
    fn caddy_unit_runs_as_root_so_external_site_certs_stay_readable() {
        let t = unit_of(&UnitsModule.render(&sample_state(), &ctx()), "caddy");
        assert!(t.contains("\nUser=root\n"), "必须显式 root：{t}");
        assert!(t.contains("\nGroup=root\n"));
        for sandbox in [
            "ProtectSystem",
            "ProtectHome",
            "ReadOnlyPaths",
            "ReadWritePaths",
            "PrivateTmp",
            "DynamicUser",
            "InaccessiblePaths",
            "TemporaryFileSystem",
        ] {
            assert!(
                !t.contains(sandbox),
                "{sandbox} 会挡住 /etc/caddy/certs（发行版单元的 ProtectSystem=full 就是 v3 的老坑）：{t}"
            );
        }
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

    /// 事故回归（2026-09-12 bwg-rick）：Hysteria2 的 `auth.command` 只接受单个可执行路径
    /// （`exec.Command(a.Cmd, addr, auth, tx)`，不过 shell、不拆空格），所以钩子的入口是
    /// `<base>/bin/bui-auth-hook` 这条符号链接，由 `bui` 按 argv[0] 分发。
    ///
    /// 目标必须是**相对**的 `bui`（同目录）：`bui upgrade` 与 `--rollback` 都是原地替换
    /// `bin/bui` 这个文件名，相对链接因此天然指向换上来的那一版，不需要重建
    /// （绝对路径同样指得到，但相对目标连 `<base>` 整体搬迁也活得下来）。
    #[test]
    fn the_hysteria_auth_hook_entry_is_a_relative_symlink_to_bui() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        assert!(
            arts.contains(&Artifact::Symlink {
                path: "/opt/b-ui/bin/bui-auth-hook".into(),
                target: "bui".into(),
            }),
            "缺 bin/bui-auth-hook 链接（或目标不是同目录的相对 `bui`）：{:?}",
            arts.iter()
                .filter(|a| matches!(a, Artifact::Symlink { .. }))
                .collect::<Vec<_>>()
        );
        // 路径由 Paths 派生，不是第二处硬编码
        let p = Paths::default_server();
        assert!(arts.contains(&Artifact::Symlink {
            path: p.auth_hook_bin(),
            target: "bui".into(),
        }));
        // 渲染进配置的那条路径与这条链接必须是同一个（否则内核指向一个不存在的文件）
        let yaml = bui_schema::render::hysteria::direct_yaml(
            &sample_state().node,
            &p,
            bui_schema::model::Hy2Auth::Command,
        );
        assert!(
            yaml.contains(&format!("command: {}\n", p.auth_hook_bin().display())),
            "config.yaml 的 auth.command 与链接不一致：\n{yaml}"
        );
    }

    #[test]
    fn render_is_pure_so_the_diff_does_not_flap() {
        let a = UnitsModule.render(&sample_state(), &ctx());
        let b = UnitsModule.render(&sample_state(), &ctx());
        assert_eq!(a, b);
    }

    struct NoopInstaller;
    impl BinaryInstaller for NoopInstaller {
        fn install(&self, _n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// 只跑 units 模块的 render → plan → apply（本模块的遗留清理要真的落到机器上才算数）。
    fn reconcile_units(h: &FakeHost) -> ApplyOutcome {
        let ctx = ctx();
        let arts = UnitsModule.render(&sample_state(), &ctx);
        let p = plan(
            PlanInput {
                artifacts: &arts,
                paths: &ctx.paths,
                keys: &BTreeMap::new(),
                installed_versions: &BTreeMap::new(),
            },
            h,
        )
        .unwrap();
        apply(
            ApplyInput {
                plan: p,
                paths: &ctx.paths,
                facts: &ctx.facts,
                installer: &NoopInstaller,
                dry_run: false,
            },
            h,
        )
    }

    const XRAY_DROPIN_DIR: &str = "/etc/systemd/system/xray.service.d";
    const OFFICIAL_XRAY_DROPIN: &str =
        "/etc/systemd/system/xray.service.d/10-donot_touch_single_conf.conf";

    /// 官方 Xray 安装器（`install-release.sh`）写的
    /// `10-donot_touch_single_conf.conf` 内容是 `ExecStart=`（清空）+
    /// `ExecStart=/usr/local/bin/xray run -config /usr/local/etc/xray/config.json`。
    /// drop-in 的优先级高于单元文件本体，所以留着它 = v4 写的完整单元的 ExecStart 被整条替换成
    /// **旧二进制 + 旧配置**（2026-09-12 bwg-rick 首切实录：xray 跑 v3 二进制、10001/10002 一个都没监听，
    /// 而 `systemctl status` 显示 active，健康检查看不出来）。清掉文件后空掉的 `.service.d/`
    /// 也一并删：留着它下次 `bui reconcile` 的漂移扫描要报、官方脚本再跑一次又能悄悄塞回来。
    #[test]
    fn official_xray_installer_dropin_is_cleaned_up_with_its_empty_dir() {
        let h = FakeHost::new();
        h.with(|i| {
            i.dirs.insert(XRAY_DROPIN_DIR.into());
            i.files.insert(
                OFFICIAL_XRAY_DROPIN.into(),
                (
                    b"[Service]\nExecStart=\nExecStart=/usr/local/bin/xray run -config /usr/local/etc/xray/config.json\n".to_vec(),
                    0o644,
                ),
            );
        });
        let out = reconcile_units(&h);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(
            h.text(OFFICIAL_XRAY_DROPIN).is_none(),
            "官方 drop-in 必须被清掉，否则 v4 的 ExecStart 形同虚设"
        );
        assert!(
            !h.is_dir(Path::new(XRAY_DROPIN_DIR)).unwrap(),
            "清空后的 .service.d 目录要一并删掉"
        );
        assert!(out
            .changed
            .iter()
            .any(|c| c == OFFICIAL_XRAY_DROPIN || c == XRAY_DROPIN_DIR));
    }

    /// 只删**空**目录：同一个 `.service.d/` 里还有别人的 drop-in（运维手写的
    /// `ExecStartPost=`、云厂商 agent 塞的限速片段）时，目录必须留着，否则连带删掉别人的文件。
    #[test]
    fn a_dropin_dir_with_other_files_left_in_it_is_kept() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                OFFICIAL_XRAY_DROPIN.into(),
                (b"ExecStart=\n".to_vec(), 0o644),
            );
            i.files.insert(
                format!("{XRAY_DROPIN_DIR}/50-operator.conf").into(),
                (b"[Service]\nExecStartPost=/bin/true\n".to_vec(), 0o644),
            );
        });
        reconcile_units(&h);
        assert!(h.text(OFFICIAL_XRAY_DROPIN).is_none());
        assert_eq!(
            h.text(&format!("{XRAY_DROPIN_DIR}/50-operator.conf"))
                .as_deref(),
            Some("[Service]\nExecStartPost=/bin/true\n"),
            "别人的 drop-in 不许动"
        );
        assert!(
            h.is_dir(Path::new(XRAY_DROPIN_DIR)).unwrap(),
            "非空目录不动"
        );
    }

    #[test]
    fn a_single_slot_renders_the_v3_named_residential_unit_only() {
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
        // 1..MAX 的实例：停用 + 删单元文件
        for i in 1..crate::reconcile::MAX_RESI_SLOTS {
            let u = crate::reconcile::resi_unit(i);
            assert!(arts.iter().any(
                |a| matches!(a, Artifact::UnitState { name, enabled: false, active: false } if *name == u)
            ));
            assert!(arts.iter().any(|a| matches!(a, Artifact::Absent { path }
                if path.to_str() == Some(&format!("/etc/systemd/system/{u}.service")))));
        }
    }

    #[test]
    fn three_slots_render_three_residential_units_each_with_its_own_config() {
        let mut s = sample_state();
        s.residential.slots = (0..3)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        let arts = UnitsModule.render(&s, &ctx());
        for i in 0..3u16 {
            let name = crate::reconcile::resi_unit(i);
            let t = unit_of(&arts, &name);
            let cfg = if i == 0 {
                "/opt/b-ui/config-residential.yaml".to_string()
            } else {
                format!("/opt/b-ui/config-residential-{i}.yaml")
            };
            assert!(
                t.contains(&format!(
                    "ExecStart=/opt/b-ui/bin/hysteria server --config {cfg}"
                )),
                "{name} 的 ExecStart 不对：\n{t}"
            );
            assert!(t.contains("Environment=GOMEMLIMIT=200MiB"));
            assert!(t.contains("LimitNOFILE=1048576"));
            assert!(t.contains("Nice=-5"));
            assert!(t.contains("After=network-online.target b-ui-relay.service"));
            assert!(arts.iter().any(
                |a| matches!(a, Artifact::UnitState { name: n, enabled: true, active: true } if *n == name)
            ));
        }
        // 3..MAX 才是清理项
        assert!(arts.iter().any(
            |a| matches!(a, Artifact::UnitState { name, enabled: false, active: false }
            if name == "hysteria-residential-3")
        ));
        assert!(!arts.iter().any(
            |a| matches!(a, Artifact::UnitState { name, enabled: false, active: false }
            if name == "hysteria-residential-2")
        ));
    }
}
