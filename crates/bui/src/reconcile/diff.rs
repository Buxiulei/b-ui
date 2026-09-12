//! 期望态 → 变更集：按内容/状态逐项比对，只产出有差异的项（spec §2.2「按内容哈希比对，
//! 只写有差异的」）。这里只读机器、不改机器；落地在 [`super::apply`]。
use super::{Artifact, PortSpec, Unit, Verify};
use crate::sys::Host;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 一条具体的改动。`apply` 按固定顺序执行它们（见 Task 5）。
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    WriteFile {
        path: PathBuf,
        content: Vec<u8>,
        mode: u32,
        verify: Option<Verify>,
        restart: Option<Unit>,
    },
    SetImmutable {
        path: PathBuf,
        on: bool,
    },
    RemoveFile {
        path: PathBuf,
    },
    WriteUnit {
        path: PathBuf,
        content: String,
        unit: Unit,
    },
    SetUnitState {
        unit: String,
        enabled: bool,
        active: bool,
    },
    SetSysctl {
        key: String,
        value: String,
    },
    LoadModule {
        module: String,
    },
    InstallBinary {
        name: String,
        version: String,
        sha256: String,
        url: String,
        path: PathBuf,
    },
    WriteSymlink {
        path: PathBuf,
        target: PathBuf,
    },
    OpenPorts {
        ports: Vec<PortSpec>,
    },
}

/// 一轮比对的结果。`keys` 只是**候选** `restart_key`：真正落盘由 apply 在写盘成功后
/// 搬进 `ApplyOutcome.keys`（校验失败 / 写失败 / 没有防火墙都不搬）。
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub keys: BTreeMap<String, String>,
    pub unchanged: usize,
}

pub struct PlanInput<'a> {
    pub artifacts: &'a [Artifact],
    pub paths: &'a Paths,
    /// `runtime.restart_keys`
    pub keys: &'a BTreeMap<String, String>,
    pub installed_versions: &'a BTreeMap<String, String>,
}

pub fn plan(input: PlanInput<'_>, host: &dyn Host) -> Result<Plan> {
    let mut out = Plan::default();
    // 产出顺序必须与 artifacts 的输入顺序一致（apply 的步骤顺序与测试断言都依赖它）。
    for art in input.artifacts {
        match art {
            Artifact::File {
                path,
                content,
                mode,
                immutable,
                restart,
                restart_key,
                verify,
            } => {
                let current = host.read_file(path)?;
                if current.as_deref() != Some(content.as_slice()) {
                    let restart = match restart_key {
                        Some(k) => {
                            let id = art.id();
                            let landed =
                                input.keys.get(&id).map(String::as_str) == Some(k.as_str());
                            out.keys.insert(id, k.clone());
                            // 结构哈希没变（xray 只改了 clients）→ 写盘但不重启，spec §3.3
                            if landed {
                                None
                            } else {
                                restart.clone()
                            }
                        }
                        None => restart.clone(),
                    };
                    out.changes.push(Change::WriteFile {
                        path: path.clone(),
                        content: content.clone(),
                        mode: *mode,
                        verify: *verify,
                        restart,
                    });
                    // 写盘会清掉 immutable 位（apply 先 `chattr -i` 再写），所以要补回来。
                    if *immutable {
                        out.changes.push(Change::SetImmutable {
                            path: path.clone(),
                            on: true,
                        });
                    }
                } else if host.is_immutable(path)? != *immutable {
                    out.changes.push(Change::SetImmutable {
                        path: path.clone(),
                        on: *immutable,
                    });
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::Unit {
                name,
                dropin,
                content,
            } => {
                let path = Artifact::unit_path(&name.name, dropin.as_deref());
                if host.read_file(&path)?.as_deref() != Some(content.as_bytes()) {
                    out.changes.push(Change::WriteUnit {
                        path,
                        content: content.clone(),
                        unit: name.clone(),
                    });
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::UnitState {
                name,
                enabled,
                active,
            } => {
                if host.unit_is_enabled(name)? != *enabled || host.unit_is_active(name)? != *active
                {
                    out.changes.push(Change::SetUnitState {
                        unit: name.clone(),
                        enabled: *enabled,
                        active: *active,
                    });
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::Sysctl { key, value } => {
                // 比较按空白切分的 token 序列（`/proc` 的多值键是制表符分隔），切分后仍不等时
                // 再看 apply 留下的钳制记账（内核把写入值改过，读回值才是已生效值）。
                let current = host.sysctl_get(key)?;
                let settled = current.as_deref().is_some_and(|cur| {
                    super::sysctl_tokens_eq(cur, value)
                        || input
                            .keys
                            .get(&art.id())
                            .is_some_and(|rec| super::sysctl_clamp_settled(rec, value, cur))
                });
                if settled {
                    out.unchanged += 1;
                } else {
                    out.changes.push(Change::SetSysctl {
                        key: key.clone(),
                        value: value.clone(),
                    });
                }
            }
            Artifact::Modprobe { module } => {
                // 已加载 → 第二轮对账为零变更（`/sys/module/<m>` 是判据）
                if host.is_dir(&PathBuf::from(format!("/sys/module/{module}")))? {
                    out.unchanged += 1;
                } else {
                    out.changes.push(Change::LoadModule {
                        module: module.clone(),
                    });
                }
            }
            Artifact::Binary {
                name,
                version,
                sha256,
                url,
            } => {
                if input.installed_versions.get(name).map(String::as_str) != Some(version.as_str())
                {
                    out.changes.push(Change::InstallBinary {
                        name: name.clone(),
                        version: version.clone(),
                        sha256: sha256.clone(),
                        url: url.clone(),
                        path: input.paths.bin_dir.join(name),
                    });
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::Symlink { path, target } => {
                // 覆盖「路径是普通文件」「链接指错地方」「悬空链接」三种情况
                if host.read_link(path)?.as_deref() != Some(target.as_path()) {
                    out.changes.push(Change::WriteSymlink {
                        path: path.clone(),
                        target: target.clone(),
                    });
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::FirewallPorts { ports } => {
                // 产出这个 artifact 的模块必须先确认机器上有活的防火墙（Task 6 只在
                // `facts.ufw_active || facts.firewalld_active` 时产出）：否则 apply 什么都没改、
                // 不搬 key，本规则下一轮又出一条 OpenPorts，「二次对账零变更」永远不可达。
                let key = ports_key(ports);
                if input.keys.get("firewall").map(String::as_str) != Some(key.as_str()) {
                    out.changes.push(Change::OpenPorts {
                        ports: ports.clone(),
                    });
                    out.keys.insert("firewall".to_string(), key);
                } else {
                    out.unchanged += 1;
                }
            }
            Artifact::Absent { path } => {
                if host.read_file(path)?.is_some() {
                    out.changes.push(Change::RemoveFile { path: path.clone() });
                } else {
                    out.unchanged += 1;
                }
            }
        }
    }
    Ok(out)
}

/// 端口集的指纹：放行过一次就不再重复 `ufw allow`。
fn ports_key(ports: &[PortSpec]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in ports {
        h.update(p.ufw().as_bytes());
        h.update(b",");
    }
    hex::encode(h.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Unit, Verify};
    use crate::sys::{fake::FakeHost, Host, Proto};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    fn input<'a>(
        artifacts: &'a [Artifact],
        paths: &'a Paths,
        keys: &'a BTreeMap<String, String>,
        versions: &'a BTreeMap<String, String>,
    ) -> PlanInput<'a> {
        PlanInput {
            artifacts,
            paths,
            keys,
            installed_versions: versions,
        }
    }

    #[test]
    fn missing_file_is_written_and_restarts_its_unit() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/opt/b-ui/config.yaml", "listen: :10000")
            .restart(Unit::restart("hysteria-server"))];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(),
                content: b"listen: :10000".to_vec(),
                mode: 0o600,
                verify: None,
                restart: Some(Unit::restart("hysteria-server")),
            }]
        );
        assert_eq!(p.unchanged, 0);
    }

    #[test]
    fn identical_file_is_untouched() {
        let h = FakeHost::new();
        h.write_file(
            std::path::Path::new("/opt/b-ui/config.yaml"),
            b"listen: :10000",
            0o600,
        )
        .unwrap();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/opt/b-ui/config.yaml", "listen: :10000")];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert!(p.changes.is_empty());
        assert_eq!(p.unchanged, 1);
    }

    #[test]
    fn restart_key_suppresses_restart_when_structure_is_unchanged() {
        let h = FakeHost::new();
        h.write_file(
            std::path::Path::new("/opt/b-ui/xray-config.json"),
            b"{\"clients\":[1]}",
            0o600,
        )
        .unwrap();
        let paths = Paths::default_server();
        let mut keys = BTreeMap::new();
        keys.insert(
            "file:/opt/b-ui/xray-config.json".to_string(),
            "hash-A".to_string(),
        );
        let versions = BTreeMap::new();
        let arts = vec![
            Artifact::file("/opt/b-ui/xray-config.json", "{\"clients\":[1,2]}")
                .restart(Unit::restart("xray"))
                .restart_key("hash-A")
                .verify(Verify::Xray),
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        match &p.changes[0] {
            Change::WriteFile {
                restart, verify, ..
            } => {
                assert_eq!(*restart, None, "结构哈希未变 → 不重启 xray");
                assert_eq!(*verify, Some(Verify::Xray));
            }
            other => panic!("{other:?}"),
        }
        // 结构哈希变了就要重启
        let arts2 = vec![
            Artifact::file("/opt/b-ui/xray-config.json", "{\"clients\":[1,2]}")
                .restart(Unit::restart("xray"))
                .restart_key("hash-B"),
        ];
        let p2 = plan(input(&arts2, &paths, &keys, &versions), &h).unwrap();
        match &p2.changes[0] {
            Change::WriteFile { restart, .. } => {
                assert_eq!(*restart, Some(Unit::restart("xray")))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p2.keys
                .get("file:/opt/b-ui/xray-config.json")
                .map(String::as_str),
            Some("hash-B")
        );
    }

    #[test]
    fn immutable_flag_alone_produces_only_a_flag_change() {
        let h = FakeHost::new();
        h.write_file(
            std::path::Path::new("/etc/resolv.conf"),
            b"nameserver 1.1.1.1\n",
            0o644,
        )
        .unwrap();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/etc/resolv.conf", "nameserver 1.1.1.1\n")
            .mode(0o644)
            .immutable()];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![Change::SetImmutable {
                path: "/etc/resolv.conf".into(),
                on: true
            }]
        );
    }

    #[test]
    fn unit_sysctl_unitstate_binary_and_absent() {
        let h = FakeHost::new();
        h.with(|i| {
            i.sysctl.insert("net.ipv4.tcp_retries2".into(), "8".into());
            i.sysctl
                .insert("net.core.default_qdisc".into(), "pfifo_fast".into());
            i.units_enabled.insert("b-ui.service".into());
            i.files.insert(
                "/etc/systemd/system/hy2-watchdog.timer".into(),
                (b"x".to_vec(), 0o644),
            );
        });
        let paths = Paths::default_server();
        let (keys, mut versions) = (BTreeMap::new(), BTreeMap::new());
        versions.insert("xray".to_string(), "26.3.27".to_string());
        let arts = vec![
            Artifact::Unit {
                name: Unit::restart("b-ui"),
                dropin: None,
                content: "[Service]\n".into(),
            },
            Artifact::UnitState {
                name: "b-ui".into(),
                enabled: true,
                active: true,
            },
            Artifact::Sysctl {
                key: "net.ipv4.tcp_retries2".into(),
                value: "8".into(),
            },
            Artifact::Sysctl {
                key: "net.core.default_qdisc".into(),
                value: "fq".into(),
            },
            Artifact::Binary {
                name: "xray".into(),
                version: "26.3.27".into(),
                sha256: "aa".into(),
                url: "https://x/y".into(),
            },
            Artifact::Binary {
                name: "sing-box".into(),
                version: "1.13.19".into(),
                sha256: "bb".into(),
                url: "https://x/z".into(),
            },
            Artifact::Absent {
                path: "/etc/systemd/system/hy2-watchdog.timer".into(),
            },
            Artifact::Absent {
                path: "/etc/systemd/system/gone.timer".into(),
            },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("b-ui"),
                },
                Change::SetUnitState {
                    unit: "b-ui".into(),
                    enabled: true,
                    active: true,
                },
                Change::SetSysctl {
                    key: "net.core.default_qdisc".into(),
                    value: "fq".into(),
                },
                Change::InstallBinary {
                    name: "sing-box".into(),
                    version: "1.13.19".into(),
                    sha256: "bb".into(),
                    url: "https://x/z".into(),
                    path: "/opt/b-ui/bin/sing-box".into(),
                },
                Change::RemoveFile {
                    path: "/etc/systemd/system/hy2-watchdog.timer".into()
                },
            ]
        );
        assert_eq!(p.unchanged, 3, "tcp_retries2 / xray 版本 / 不存在的 Absent");
    }

    #[test]
    fn multi_value_sysctl_is_compared_token_wise_not_byte_wise() {
        // bwg-rick 真机 M1 step2：`/proc` 里多值键是**制表符**分隔，而 conf 与 `sysctl -w`
        // 用空格 —— 逐字节比会让 tcp_rmem / tcp_wmem / udp_mem / ip_local_port_range
        // 这四个键每轮都进 `changed`，「二次对账零变更」恒 FAIL。
        let h = FakeHost::new();
        h.with(|i| {
            i.sysctl
                .insert("net.ipv4.tcp_rmem".into(), "4096\t262144\t16777216".into());
            i.sysctl
                .insert("net.ipv4.ip_local_port_range".into(), "10000\t65535".into());
            // 单值键不受影响，但顺手覆盖「前后空白」这一种
            i.sysctl
                .insert("net.ipv4.tcp_retries2".into(), " 8 ".into());
        });
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![
            Artifact::Sysctl {
                key: "net.ipv4.tcp_rmem".into(),
                value: "4096 262144 16777216".into(),
            },
            Artifact::Sysctl {
                key: "net.ipv4.ip_local_port_range".into(),
                value: "10000 65535".into(),
            },
            Artifact::Sysctl {
                key: "net.ipv4.tcp_retries2".into(),
                value: "8".into(),
            },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes, vec![], "制表符/空格之差不是改动");
        assert_eq!(p.unchanged, 3);
        // token 序列真的不同（值变了 / 少一个 token）才算改动
        let arts = vec![
            Artifact::Sysctl {
                key: "net.ipv4.tcp_rmem".into(),
                value: "4096 524288 16777216".into(),
            },
            Artifact::Sysctl {
                key: "net.ipv4.ip_local_port_range".into(),
                value: "10000".into(),
            },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes.len(), 2, "{:?}", p.changes);
    }

    #[test]
    fn a_kernel_clamped_sysctl_settles_after_one_apply() {
        // 切分后仍不等 = 内核钳制/重排。apply 记下「写了什么 → 实际生效什么」之后，
        // 下一轮就把读回值当已生效值，不再每轮报 changed。
        let h = FakeHost::new();
        h.with(|i| {
            i.sysctl
                .insert("net.ipv4.udp_mem".into(), "8192\t524288\t1048576".into());
        });
        let paths = Paths::default_server();
        let versions = BTreeMap::new();
        let want = "262144 524288 1048576";
        let arts = vec![Artifact::Sysctl {
            key: "net.ipv4.udp_mem".into(),
            value: want.into(),
        }];
        // 没有记账 → 照旧写一次
        let keys = BTreeMap::new();
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes.len(), 1);
        // 记账之后 → 零变更
        let mut keys = BTreeMap::new();
        keys.insert(
            "sysctl:net.ipv4.udp_mem".to_string(),
            super::super::sysctl_clamped_record(want, "8192\t524288\t1048576"),
        );
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert!(p.changes.is_empty(), "{:?}", p.changes);
        assert_eq!(p.unchanged, 1);
        // 期望值改了 → 记账失效，重新写
        let other = vec![Artifact::Sysctl {
            key: "net.ipv4.udp_mem".into(),
            value: "262144 524288 2097152".into(),
        }];
        let p = plan(input(&other, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes.len(), 1, "期望值变了必须重新写");
        // 读回值被人手改了 → 记账失效，重新写
        h.with(|i| {
            i.sysctl
                .insert("net.ipv4.udp_mem".into(), "1024\t1024\t1024".into());
        });
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes.len(), 1, "读回值不再是记账里的已生效值");
    }

    #[test]
    fn modprobe_and_symlink_are_planned_only_when_missing_or_wrong() {
        let h = FakeHost::new();
        h.with(|i| {
            i.modules.insert("tcp_bbr".into());
            i.symlinks
                .insert("/usr/local/bin/bui".into(), "/opt/b-ui/bin/bui".into());
            i.symlinks.insert(
                "/usr/local/bin/b-ui".into(),
                "/opt/hysteria/b-ui-cli.sh".into(),
            );
        });
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![
            Artifact::Modprobe {
                module: "tcp_bbr".into(),
            },
            Artifact::Modprobe {
                module: "nf_conntrack".into(),
            },
            Artifact::Symlink {
                path: "/usr/local/bin/bui".into(),
                target: "/opt/b-ui/bin/bui".into(),
            },
            Artifact::Symlink {
                path: "/usr/local/bin/b-ui".into(),
                target: "/opt/b-ui/bin/bui".into(),
            },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![
                Change::LoadModule {
                    module: "nf_conntrack".into()
                },
                Change::WriteSymlink {
                    path: "/usr/local/bin/b-ui".into(),
                    target: "/opt/b-ui/bin/bui".into()
                },
            ]
        );
        assert_eq!(p.unchanged, 2, "已加载的模块与已正确的链接都不动");
    }

    #[test]
    fn firewall_ports_only_reapply_when_the_set_changes() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let versions = BTreeMap::new();
        let ports = vec![
            PortSpec::one(Proto::Tcp, 22),
            PortSpec::range(Proto::Udp, 20000, 30000),
        ];
        let arts = vec![Artifact::FirewallPorts {
            ports: ports.clone(),
        }];
        let first = plan(input(&arts, &paths, &BTreeMap::new(), &versions), &h).unwrap();
        assert_eq!(first.changes, vec![Change::OpenPorts { ports }]);
        let key = first.keys.get("firewall").cloned().unwrap();
        let mut keys = BTreeMap::new();
        keys.insert("firewall".to_string(), key);
        let second = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert!(second.changes.is_empty(), "端口集没变就不重复放行");
    }

    #[test]
    fn artifact_ids_are_stable_and_distinct() {
        assert_eq!(Artifact::file("/a", "x").id(), "file:/a");
        assert_eq!(
            Artifact::Unit {
                name: Unit::restart("xray"),
                dropin: None,
                content: String::new()
            }
            .id(),
            "unit:xray"
        );
        assert_eq!(
            Artifact::Unit {
                name: Unit::restart("xray"),
                dropin: Some("99-b-ui.conf".into()),
                content: String::new()
            }
            .id(),
            "unit:xray:99-b-ui.conf"
        );
        assert_eq!(
            Artifact::Sysctl {
                key: "a.b".into(),
                value: "1".into()
            }
            .id(),
            "sysctl:a.b"
        );
        assert_eq!(
            Artifact::Modprobe {
                module: "nf_conntrack".into()
            }
            .id(),
            "modprobe:nf_conntrack"
        );
        assert_eq!(
            Artifact::Symlink {
                path: "/usr/local/bin/b-ui".into(),
                target: "/x".into()
            }
            .id(),
            "symlink:/usr/local/bin/b-ui"
        );
        assert_eq!(Artifact::FirewallPorts { ports: vec![] }.id(), "firewall");
    }
}
