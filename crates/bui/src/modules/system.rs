//! 系统模块：sysctl 调优、conntrack 容量与模块加载、静态 DNS、防火墙端口。
//!
//! 移植参照（只读 v3）：`server/core.sh:1443-1521`（三个 sysctl 文件 + conntrack 分档 +
//! modprobe + BBR）、`server/core.sh:283-288` 与 `server/update.sh:987-996`（≤2G 机器的
//! `99-b-ui-memory.conf`）、`server/core.sh:1237-1283`（`configure_firewall`）、
//! `server/update.sh:1738-1752`（静态 DNS）、`server/update.sh:1754-1767`（防火墙端口）。
//!
//! v4 的变化：三个 sysctl 文件合并成 `99-b-ui-network.conf` + `99-b-ui-conntrack.conf`
//! （不再写 `99-hysteria-perf.conf`，也不再写 `hysteria-server.service.d/priority.conf` 的
//! RT 优先级——单元里统一 `Nice=-5`）；每个 sysctl 键额外产出一条 [`Artifact::Sysctl`]
//! 以便立即生效（不再 `sysctl --system`）。

use crate::reconcile::{Artifact, Module, PortSpec, RenderCtx};
use crate::sys::Proto;
use bui_schema::model::{Ports, State};

pub const RESOLV_CONF: &str = "/etc/resolv.conf";
/// conntrack 的 sysctl 键必须在模块加载后才存在（spec §2.2 的「nf_conntrack 模块」）。
pub const CONNTRACK_MODULE: &str = "nf_conntrack";
/// 小内存机器（≤2G）的 swappiness 文件：**与 v3 同名**（v3 `server/core.sh:283-288`、
/// `server/update.sh:987-996` 都写这一个），>2G 时产出 `Absent` 删掉它。
pub const MEMORY_CONF: &str = "/etc/sysctl.d/99-b-ui-memory.conf";

pub struct SystemModule;

/// 与 v3 core.sh:1504-1508 同值（>4G → 524288，>2G → 262144，否则 131072）。
pub fn conntrack_max(mem_mb: u64) -> u32 {
    if mem_mb > 4096 {
        524288
    } else if mem_mb > 2048 {
        262144
    } else {
        131072
    }
}

/// `ip_local_reserved_ports` 的值（v3 core.sh:1485 的硬编码在 v4 从 state 推导）：
/// 监听端口里连续的合成段（10000-10002）+ 直连跳跃段 + **住宅基础端口段**（每槽一个）+
/// 住宅跳跃段。`slots` 是槽位空间宽度（[`bui_schema::slots::slot_span`]），单槽时输出与
/// v3 逐字相同。
pub fn reserved_ports(p: &Ports, slots: u16) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut singles = vec![p.hy2, p.reality_direct, p.reality_resi];
    singles.sort_unstable();
    singles.dedup();
    let mut i = 0;
    while i < singles.len() {
        let start = singles[i];
        let mut end = start;
        while i + 1 < singles.len() && singles[i + 1] == end + 1 {
            i += 1;
            end = singles[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        i += 1;
    }
    if let Some((a, b)) = p.hy2_hop {
        parts.push(format!("{a}-{b}"));
    }
    let slots = slots.max(1);
    parts.push(if slots == 1 {
        p.hy2_resi.to_string()
    } else {
        format!("{}-{}", p.hy2_resi, p.hy2_resi + slots - 1)
    });
    parts.push(format!("{}-{}", p.hy2_resi_hop.0, p.hy2_resi_hop.1));
    parts.join(",")
}

/// 防火墙要放行的端口集（v3 core.sh:1237-1283 + update.sh 块 E，外加 spec §5.6 的槽位）。
/// `render` 只在机器上确实有活防火墙时把它包成 [`Artifact::FirewallPorts`]；
/// `serve::reconcile_once` 在没有防火墙时也调它，用来拼「请在云厂商安全组放行」的提示，
/// 所以它是 `pub`。
///
/// `slots` 是槽位空间宽度（[`bui_schema::slots::slot_span`]，= 最高序号 + 1 而不是槽位
/// **个数**）：住宅基础端口放行 `hy2_resi .. hy2_resi + slots - 1`（每槽一个实例），跳跃
/// 区间**整段**放行（各槽只用其中一片，多放的那些没人监听）。序号有空洞时会多放一个空
/// 端口，可接受（D3 的连带效果）。单槽时 `from == to`，`ufw()` / `firewalld()` 打出来就是
/// 单端口，与 v3 逐字相同。
pub fn firewall_ports(p: &Ports, slots: u16) -> Vec<PortSpec> {
    let slots = slots.max(1);
    let mut v = vec![
        PortSpec::one(Proto::Tcp, 22),
        PortSpec::one(Proto::Tcp, 80),
        PortSpec::one(Proto::Tcp, 443),
        PortSpec::one(Proto::Tcp, p.reality_direct),
        PortSpec::one(Proto::Tcp, p.reality_resi),
        PortSpec::one(Proto::Udp, p.hy2),
    ];
    if let Some((a, b)) = p.hy2_hop {
        v.push(PortSpec::range(Proto::Udp, a, b));
    }
    v.push(PortSpec::range(
        Proto::Udp,
        p.hy2_resi,
        p.hy2_resi + slots - 1,
    ));
    v.push(PortSpec::range(
        Proto::Udp,
        p.hy2_resi_hop.0,
        p.hy2_resi_hop.1,
    ));
    v
}

impl Module for SystemModule {
    fn name(&self) -> &'static str {
        "system"
    }

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::new();
        if s.system.sysctl_profile != "off" {
            let ct = conntrack_max(ctx.facts.mem_mb);
            let network = network_conf(&s.node.ports, bui_schema::slots::slot_span(&s.residential));
            let conntrack = conntrack_conf(ct);
            out.push(
                Artifact::file("/etc/sysctl.d/99-b-ui-network.conf", network.clone()).mode(0o644),
            );
            out.push(
                Artifact::file("/etc/sysctl.d/99-b-ui-conntrack.conf", conntrack.clone())
                    .mode(0o644),
            );
            // ≤2G 机器接管 v3 的同名文件（v3 core.sh:283-288 / update.sh:987-996），>2G 删掉它
            let small_mem = ctx.facts.mem_mb <= 2048;
            if small_mem {
                out.push(Artifact::file(MEMORY_CONF, MEMORY_BODY).mode(0o644));
            } else {
                out.push(Artifact::Absent {
                    path: MEMORY_CONF.into(),
                });
            }
            out.push(
                Artifact::file(
                    "/etc/modprobe.d/b-ui-nf_conntrack.conf",
                    format!("options nf_conntrack hashsize={}\n", ct / 4),
                )
                .mode(0o644),
            );
            out.push(
                Artifact::file("/etc/modules-load.d/b-ui-conntrack.conf", "nf_conntrack\n")
                    .mode(0o644),
            );
            // modules-load.d 只管下次开机；本轮就要加载，否则 net.netfilter.* 三个键
            // `sysctl -w` 报 ENOENT
            out.push(Artifact::Modprobe {
                module: CONNTRACK_MODULE.to_string(),
            });
            let mut confs: Vec<&str> = vec![&network, &conntrack];
            if small_mem {
                confs.push(MEMORY_BODY);
            }
            for text in confs {
                for (key, value) in parse_conf(text) {
                    out.push(Artifact::Sysctl { key, value });
                }
            }
        }
        if s.system.static_dns {
            out.push(
                Artifact::file(RESOLV_CONF, RESOLV_BODY)
                    .mode(0o644)
                    .immutable(),
            );
            if ctx.facts.systemd_resolved {
                out.push(Artifact::UnitState {
                    name: "systemd-resolved".into(),
                    enabled: false,
                    active: false,
                });
            }
        }
        // 只在机器上确实有**活的**防火墙时才产出这个 artifact。判据必须与 apply 第 10 步的两支
        // 完全一致（`*_active`，不是 `has_*`）：装了 ufw 但没 enable 的机器上 `ufw allow` 不会生效，
        // apply 会走「两者都没有」的守卫支、不搬 `firewall` key，于是 diff 规则 8 每轮重新产出
        // 一条 `OpenPorts` —— `changed` 永远非空、`keys["firewall"]` 永不出现，
        // M1 的「二次对账零变更」在这类机器上永远不可达（第三轮审查 C1）。
        // 没有防火墙时的提示由 `serve::reconcile_once` 从 `facts` 追加进 `notes`（notes 不影响
        // `/api/health` 的 degraded 判定，所以它是「每轮提醒」而不是「每轮改动」）。
        if s.system.firewall != "off" && (ctx.facts.ufw_active || ctx.facts.firewalld_active) {
            out.push(Artifact::FirewallPorts {
                ports: firewall_ports(&s.node.ports, bui_schema::slots::slot_span(&s.residential)),
            });
        }
        out
    }
}

/// 取 conf 里的 `key=value` 行（跳过注释与空行）。
fn parse_conf(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// v3 写的是 `vm.swappiness = 10`（带空格），v4 用统一的 `key=value` 写法——**值同为 10**
/// （spec §3.4「与 v3 同值」）。import-v3 后的首轮对账会因内容哈希不同重写这一个文件一次，
/// 第二轮起零变更，M1 的「二次对账零变更」不受影响。
const MEMORY_BODY: &str = "\
# B-UI v4 内存策略：小内存机器（≤2G）降低 swap 倾向（移植 v3 core.sh:283-288）
# 配合两个 hysteria 的 GOMEMLIMIT/MemoryHigh/MemoryMax 一起生效
vm.swappiness=10
";

const RESOLV_BODY: &str = "\
# B-UI v4 静态 DNS（绕 systemd-resolved，防 GFW UDP 投毒兜底）
# hy2/xray/sing-box 各自用 DoH 解析；这里只给 apt/curl 等次要进程用
nameserver 1.1.1.1
nameserver 8.8.8.8
options edns0 timeout:2 attempts:2 single-request
";

fn network_conf(p: &Ports, slots: u16) -> String {
    format!(
        "\
# B-UI v4 网络栈调优（移植 v3 core.sh:1447-1494，合并原 99-hysteria-perf.conf）
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=1048576
net.core.wmem_default=1048576
net.ipv4.tcp_retries2=8
net.ipv4.tcp_mtu_probing=1
net.ipv4.tcp_keepalive_time=600
net.ipv4.tcp_keepalive_intvl=30
net.ipv4.tcp_keepalive_probes=3
net.ipv4.tcp_no_metrics_save=1
net.ipv4.tcp_slow_start_after_idle=0
net.ipv4.tcp_notsent_lowat=131072
net.ipv4.tcp_rmem=4096 262144 16777216
net.ipv4.tcp_wmem=4096 65536 16777216
net.ipv4.udp_mem=262144 524288 1048576
net.ipv4.ip_local_port_range=10000 65535
# 把监听端口与跳跃段从临时端口池排除，避免出向连接抢占跳跃段端口
net.ipv4.ip_local_reserved_ports={reserved}
net.core.netdev_max_backlog=5000
net.ipv4.tcp_max_syn_backlog=8192
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=bbr
",
        reserved = reserved_ports(p, slots)
    )
}

fn conntrack_conf(ct: u32) -> String {
    format!(
        "\
# B-UI v4 conntrack 容量（两个 hy2 + xray + relay 共享一张表）
net.netfilter.nf_conntrack_max={ct}
net.netfilter.nf_conntrack_udp_timeout=20
net.netfilter.nf_conntrack_udp_timeout_stream=60
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx};
    use crate::sys::Proto;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn ctx(mem_mb: u64, ufw: bool, firewalld: bool, resolved: bool) -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: ufw,
                ufw_active: ufw,
                has_firewalld: firewalld,
                firewalld_active: firewalld,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 1,
                systemd_resolved: resolved,
            },
        }
    }

    fn files(arts: &[Artifact]) -> Vec<String> {
        arts.iter()
            .filter_map(|a| match a {
                Artifact::File { path, .. } => Some(path.display().to_string()),
                _ => None,
            })
            .collect()
    }

    fn file_text(arts: &[Artifact], want: &str) -> String {
        arts.iter()
            .find_map(|a| match a {
                Artifact::File { path, content, .. } if path.to_str() == Some(want) => {
                    Some(String::from_utf8(content.clone()).unwrap())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("没有渲染 {want}"))
    }

    #[test]
    fn conntrack_tiers_match_v3() {
        assert_eq!(conntrack_max(1024), 131072);
        assert_eq!(conntrack_max(2048), 131072);
        assert_eq!(conntrack_max(3072), 262144);
        assert_eq!(conntrack_max(4096), 262144);
        assert_eq!(conntrack_max(8192), 524288);
    }

    #[test]
    fn reserved_ports_are_derived_from_state() {
        let s = sample_state();
        assert_eq!(
            reserved_ports(&s.node.ports, 1),
            "10000-10002,20000-30000,40000,41000-50000"
        );
        let mut p = s.node.ports.clone();
        p.hy2_hop = None;
        assert_eq!(reserved_ports(&p, 1), "10000-10002,40000,41000-50000");
    }

    #[test]
    fn firewall_ports_cover_every_listener_and_hop_range() {
        let s = sample_state();
        let want = vec![
            PortSpec::one(Proto::Tcp, 22),
            PortSpec::one(Proto::Tcp, 80),
            PortSpec::one(Proto::Tcp, 443),
            PortSpec::one(Proto::Tcp, 10001),
            PortSpec::one(Proto::Tcp, 10002),
            PortSpec::one(Proto::Udp, 10000),
            PortSpec::range(Proto::Udp, 20000, 30000),
            PortSpec::one(Proto::Udp, 40000),
            PortSpec::range(Proto::Udp, 41000, 50000),
        ];
        assert_eq!(firewall_ports(&s.node.ports, 1), want);
    }

    #[test]
    fn a_single_slot_opens_exactly_the_v3_port_set() {
        let p = crate::testutil::sample_state().node.ports;
        let specs: Vec<String> = firewall_ports(&p, 1).iter().map(PortSpec::ufw).collect();
        assert_eq!(
            specs,
            vec![
                "22/tcp",
                "80/tcp",
                "443/tcp",
                "10001/tcp",
                "10002/tcp",
                "10000/udp",
                "20000:30000/udp",
                "40000/udp",
                "41000:50000/udp",
            ],
            "单槽的端口集必须与 v3 逐字相同"
        );
        assert_eq!(
            reserved_ports(&p, 1),
            "10000-10002,20000-30000,40000,41000-50000"
        );
    }

    #[test]
    fn three_slots_open_the_whole_residential_base_range() {
        let p = crate::testutil::sample_state().node.ports;
        let specs: Vec<String> = firewall_ports(&p, 3).iter().map(PortSpec::ufw).collect();
        assert!(
            specs.contains(&"40000:40002/udp".to_string()),
            "spec §5.6：放行 40000..40000+N-1/udp，实际 {specs:?}"
        );
        assert!(
            specs.contains(&"41000:50000/udp".to_string()),
            "跳跃区间整段放行（各槽只用其中一片，多放不会有人监听）"
        );
        let fw: Vec<String> = firewall_ports(&p, 3)
            .iter()
            .map(PortSpec::firewalld)
            .collect();
        assert!(fw.contains(&"40000-40002/udp".to_string()));
        assert_eq!(
            reserved_ports(&p, 3),
            "10000-10002,20000-30000,40000-40002,41000-50000"
        );
    }

    #[test]
    fn render_opens_the_slot_range_when_the_pool_has_slots() {
        let mut s = crate::testutil::sample_state();
        s.residential.slots = (0..2)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        // `ctx(mem_mb, ufw, firewalld, resolved)`：`has_ufw` 与 `ufw_active` 由第二个参数
        // 一起给，所以「有活防火墙」就是 `ctx(1024, true, false, true)`
        let arts = SystemModule.render(&s, &ctx(1024, true, false, true));
        let ports = arts
            .iter()
            .find_map(|a| match a {
                Artifact::FirewallPorts { ports } => Some(ports.clone()),
                _ => None,
            })
            .expect("有活防火墙就该产出 FirewallPorts");
        assert!(
            ports.iter().any(|p| p.ufw() == "40000:40001/udp"),
            "实际 {:?}",
            ports.iter().map(PortSpec::ufw).collect::<Vec<_>>()
        );
    }

    #[test]
    fn renders_conf_files_plus_one_live_sysctl_per_key() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert_eq!(
            files(&arts),
            vec![
                "/etc/sysctl.d/99-b-ui-network.conf",
                "/etc/sysctl.d/99-b-ui-conntrack.conf",
                "/etc/sysctl.d/99-b-ui-memory.conf",
                "/etc/modprobe.d/b-ui-nf_conntrack.conf",
                "/etc/modules-load.d/b-ui-conntrack.conf",
                "/etc/resolv.conf",
            ]
        );
        let net = file_text(&arts, "/etc/sysctl.d/99-b-ui-network.conf");
        assert!(net.contains("net.ipv4.tcp_congestion_control=bbr"));
        assert!(net.contains("net.core.default_qdisc=fq"));
        assert!(net.contains(
            "net.ipv4.ip_local_reserved_ports=10000-10002,20000-30000,40000,41000-50000"
        ));
        assert!(net.contains("net.core.rmem_max=16777216"));
        assert!(net.contains("net.ipv4.tcp_retries2=8"));
        assert!(net.contains("net.ipv4.tcp_rmem=4096 262144 16777216"));
        let ct = file_text(&arts, "/etc/sysctl.d/99-b-ui-conntrack.conf");
        assert!(ct.contains("net.netfilter.nf_conntrack_max=131072"));
        assert!(ct.contains("net.netfilter.nf_conntrack_udp_timeout=20"));
        assert!(
            file_text(&arts, "/etc/modprobe.d/b-ui-nf_conntrack.conf").contains("hashsize=32768")
        );
        assert_eq!(
            file_text(&arts, "/etc/modules-load.d/b-ui-conntrack.conf"),
            "nf_conntrack\n"
        );
        let keys: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Sysctl { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"net.ipv4.tcp_congestion_control".to_string()));
        assert!(keys.contains(&"net.netfilter.nf_conntrack_max".to_string()));
        let conf_keys = |t: &str| {
            t.lines()
                .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
                .count()
        };
        // ctx 的 mem_mb=2048（≤2G）→ 还多渲染一个 99-b-ui-memory.conf，它的键也要有 Sysctl
        let mem = file_text(&arts, MEMORY_CONF);
        assert_eq!(
            keys.len(),
            conf_keys(&net) + conf_keys(&ct) + conf_keys(&mem),
            "每个 conf 里的键都要有一条 Sysctl artifact"
        );
    }

    #[test]
    fn small_memory_hosts_keep_the_v3_swappiness_conf() {
        // v3 在 ≤2G 机器上写 /etc/sysctl.d/99-b-ui-memory.conf（`server/core.sh:283-288`、
        // `server/update.sh:987-996`，内容 `vm.swappiness = 10`）。v4 不渲染同名文件的话，
        // `bui install --import-v3` 之后这个文件既不在本轮 artifact 的路径集合里、
        // 名字又带 `99-b-ui` 前缀 → Task 5 的 `stray_conf` 每 10 分钟报一条、
        // `/api/health` 恒 degraded，M1 的「体检无漂移」不可达。spec §3.4「与 v3 同值」。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(files(&arts).contains(&MEMORY_CONF.to_string()));
        assert!(file_text(&arts, MEMORY_CONF).contains("vm.swappiness=10"));
        assert!(arts.contains(&Artifact::Sysctl {
            key: "vm.swappiness".into(),
            value: "10".into()
        }));
        // 1024MB 也是同一档
        let arts = SystemModule.render(&s, &ctx(1024, true, false, false));
        assert!(files(&arts).contains(&MEMORY_CONF.to_string()));
    }

    #[test]
    fn big_memory_hosts_get_an_absent_for_it_instead_of_a_stray_conf() {
        // >2G：v3 本来就不写这个文件，v4 也不要它——但机器可能是从 2G 升配上来的（v3 写过），
        // 所以要产出 `Absent` 让对账删掉，而不是放着不管（放着就是一条永久 `stray_conf`，
        // 且 swappiness 被钉在 10）。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(4096, true, false, false));
        assert!(!files(&arts).contains(&MEMORY_CONF.to_string()));
        assert!(arts.contains(&Artifact::Absent {
            path: MEMORY_CONF.into()
        }));
        assert!(!arts
            .iter()
            .any(|a| matches!(a, Artifact::Sysctl { key, .. } if key == "vm.swappiness")));
    }

    #[test]
    fn conntrack_module_is_loaded_now_not_only_on_next_boot() {
        // modules-load.d 只管下次开机；不产出 Modprobe 的话首装当轮
        // `sysctl -w net.netfilter.nf_conntrack_max` 直接 ENOENT，健康永久 degraded
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        let modprobe_at = arts
            .iter()
            .position(|a| {
                a == &Artifact::Modprobe {
                    module: CONNTRACK_MODULE.into(),
                }
            })
            .expect("必须产出 Modprobe{nf_conntrack}");
        let first_sysctl = arts
            .iter()
            .position(|a| matches!(a, Artifact::Sysctl { .. }))
            .unwrap();
        assert!(
            modprobe_at < first_sysctl,
            "render 里也把 Modprobe 排在 Sysctl 前（apply 的第 5/6 步已保证执行顺序，这里保证 dry-run 报告的可读顺序一致）"
        );
    }

    #[test]
    fn resolv_conf_is_immutable_0644_and_disables_systemd_resolved() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, true));
        let resolv = arts
            .iter()
            .find(
                |a| matches!(a, Artifact::File { path, .. } if path.to_str() == Some(RESOLV_CONF)),
            )
            .unwrap();
        match resolv {
            Artifact::File {
                mode,
                immutable,
                content,
                restart,
                ..
            } => {
                assert_eq!(*mode, 0o644);
                assert!(*immutable);
                assert_eq!(*restart, None);
                let text = String::from_utf8(content.clone()).unwrap();
                assert!(text.contains("nameserver 1.1.1.1"));
                assert!(text.contains("nameserver 8.8.8.8"));
                assert!(text.contains("options edns0 timeout:2 attempts:2 single-request"));
            }
            other => panic!("{other:?}"),
        }
        assert!(arts.contains(&Artifact::UnitState {
            name: "systemd-resolved".into(),
            enabled: false,
            active: false
        }));
    }

    #[test]
    fn static_dns_off_drops_resolv_conf_and_the_unit_state() {
        let mut s = sample_state();
        s.system.static_dns = false;
        let arts = SystemModule.render(&s, &ctx(2048, true, false, true));
        assert!(!files(&arts).contains(&RESOLV_CONF.to_string()));
        assert!(!arts
            .iter()
            .any(|a| matches!(a, Artifact::UnitState { name, .. } if name == "systemd-resolved")));
    }

    #[test]
    fn sysctl_profile_off_drops_every_sysctl_and_conf_file() {
        let mut s = sample_state();
        s.system.sysctl_profile = "off".into();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Sysctl { .. })));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Modprobe { .. })));
        assert_eq!(files(&arts), vec!["/etc/resolv.conf"]);
        // `99-b-ui-memory.conf` 的两支都在 `sysctl_profile != "off"` 里面：关了就连 Absent 也不产出
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Absent { .. })));
    }

    #[test]
    fn firewall_off_drops_the_artifact() {
        let mut s = sample_state();
        s.system.firewall = "off".into();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(!arts
            .iter()
            .any(|a| matches!(a, Artifact::FirewallPorts { .. })));
    }

    #[test]
    fn firewall_artifact_is_emitted_when_ufw_is_active() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(arts
            .iter()
            .any(|a| matches!(a, Artifact::FirewallPorts { .. })));
        // firewalld 单独在也算
        let arts = SystemModule.render(&s, &ctx(2048, false, true, false));
        assert!(arts
            .iter()
            .any(|a| matches!(a, Artifact::FirewallPorts { .. })));
    }

    #[test]
    fn no_firewall_means_no_artifact_only_a_reconcile_note() {
        // spec §2.2「防火墙：没装就在体检里提示」——提示走 `notes`，**不能**走 artifact。
        // 产出 artifact 的话 apply 什么都改不了、不搬 `firewall` key，diff 规则 8 就会每轮
        // 重新产出一条 `OpenPorts`：`changed` 永远非空，M1 的「二次对账零变更」不可达
        // （第三轮审查 C1）。提示的生成点在 `serve::reconcile_once`，那里有一条
        // `a_host_without_a_firewall_gets_a_note_and_still_reconciles_clean` 覆盖。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, false, false, false));
        assert!(
            !arts
                .iter()
                .any(|a| matches!(a, Artifact::FirewallPorts { .. })),
            "没有活防火墙时不许产出 FirewallPorts"
        );
        // 但端口集本身照样可算——提示文案要用它
        assert_eq!(firewall_ports(&s.node.ports, 1).len(), 9);
    }

    #[test]
    fn v4_sysctl_values_match_the_v3_block_verbatim() {
        // spec §3.4「sysctl … 与 v3 同值」：不自证，直接**只读**比对仓库里的 v3 实现
        // （`server/core.sh` 的三个 sysctl heredoc + BBR 那两行，共 24 条 `net.*=`）。
        // P5 删掉 server/core.sh 之后本测试自动 skip，与 v3 fixture 的处理方式一致。
        let core =
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../server/core.sh"));
        if !core.exists() {
            eprintln!("skipped: 仓库里已没有 server/core.sh（P5 删除后正常）");
            return;
        }
        let text = std::fs::read_to_string(core).unwrap();
        let v3: Vec<(String, String)> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("net.") && l.contains('='))
            .filter_map(|l| {
                l.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        assert_eq!(
            v3.len(),
            24,
            "v3 的 sysctl 行数变了，先核对 core.sh 再改本测试：{v3:?}"
        );
        let s = sample_state();
        let v4: std::collections::BTreeMap<String, String> =
            parse_conf(&network_conf(&s.node.ports, 1))
                .into_iter()
                .chain(parse_conf(&conntrack_conf(conntrack_max(2048))))
                .collect();
        for (k, v) in &v3 {
            // v3 这两条的值是 shell 变量（`${ct_max}` / `${algo}`），只能比键在不在，值单独断言
            if k == "net.netfilter.nf_conntrack_max" || k == "net.ipv4.tcp_congestion_control" {
                assert!(v4.contains_key(k), "{k} 在 v4 里丢了");
                continue;
            }
            assert_eq!(
                v4.get(k).map(String::as_str),
                Some(v.as_str()),
                "{k} 与 v3 不同值"
            );
        }
        assert_eq!(
            v4.get("net.netfilter.nf_conntrack_max").map(String::as_str),
            Some("131072")
        );
        assert_eq!(
            v4.get("net.ipv4.tcp_congestion_control")
                .map(String::as_str),
            Some("bbr")
        );
        // v4 只多不少：v3 的 enable_bbr 是独立函数，v4 把它并进 99-b-ui-network.conf
        assert_eq!(v4.len(), 24, "v4 的键集与 v3 不是一一对应：{v4:?}");
    }
}
