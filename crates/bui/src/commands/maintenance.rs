//! CLI ownership boundary for managed firewall maintenance.
//!
//! Manual commands take the same real lease as the daemon before any managed
//! state/config/active-instance read, and retain it through the synchronous write.
//! Existing owner-held rollback and watchdog calls keep using their low-level APIs.
//!
//! Only the two direct ExecStartPre commands in `modules::units` may avoid
//! re-entry. Those shipped system services run directly under PID 1, in their
//! exact system.slice cgroup, with a systemd invocation ID. No systemctl query is
//! added to the startup path. Missing/foreign observations use the manual path.
//! This attributes the shipped commands, not privileged arbitrary unit overrides.
use crate::{cli::NftCmd, modules::portjump, residential_lifecycle::ControlLease, sys::Host};
use anyhow::Result;
use bui_schema::paths::Paths;
use std::path::Path;

#[derive(Default)]
pub struct Invocation {
    invocation_id: Option<String>,
    parent_pid: Option<u32>,
    manager_comm: Option<String>,
    cgroup: Option<String>,
}

impl Invocation {
    /// These are process-context observations only, never managed mutation basis.
    pub fn current(host: &dyn Host) -> Self {
        Self::read(host, std::env::var(portjump::SYSTEMD_INVOCATION_ENV).ok())
    }

    fn read(host: &dyn Host, invocation_id: Option<String>) -> Self {
        if invocation_id.is_none() {
            return Self::default();
        }
        let read = |path: &str| {
            host.read_file(Path::new(path))
                .ok()
                .flatten()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        };
        Self {
            invocation_id,
            parent_pid: read("/proc/self/status").and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("PPid:")?.trim().parse().ok())
            }),
            manager_comm: read("/proc/1/comm"),
            cgroup: read("/proc/self/cgroup"),
        }
    }

    fn is_direct_prestart(&self, unit: &str) -> bool {
        if self.parent_pid != Some(1)
            || self.manager_comm.as_deref().map(str::trim) != Some("systemd")
            || !self
                .invocation_id
                .as_deref()
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return false;
        }
        let Some(cgroup) = &self.cgroup else {
            return false;
        };
        let expected = format!("/system.slice/{unit}.service");
        let mut found = false;
        for line in cgroup.lines() {
            let mut fields = line.splitn(3, ':');
            let (Some(id), Some(controllers), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return false;
            };
            // The systemd hierarchy: unified v2, or the named v1 hierarchy.
            // Reject conflicting observations, child cgroups and lookalike units.
            if (id == "0" && controllers.is_empty()) || controllers == "name=systemd" {
                if id.parse::<u32>().is_err() || path != expected {
                    return false;
                }
                found = true;
            }
        }
        found
    }
}

pub fn nft_command(
    host: &dyn Host,
    paths: &Paths,
    cmd: NftCmd,
    invocation: &Invocation,
) -> Result<String> {
    if matches!(cmd, NftCmd::Status) {
        return super::nft::status(host, paths);
    }
    let prestart = matches!(cmd, NftCmd::Apply)
        && invocation.is_direct_prestart(crate::residential_lifecycle::UNIT);
    let _lease = if prestart {
        None
    } else {
        Some(ControlLease::acquire(paths)?)
    };
    match cmd {
        NftCmd::Apply => super::nft::apply(host, paths),
        NftCmd::Delete => super::nft::delete(host, paths),
        NftCmd::Status => unreachable!("read-only status returned before ownership"),
    }
}

pub fn hy2_prestart(
    host: &dyn Host,
    paths: &Paths,
    config: &Path,
    force: bool,
    invocation: &Invocation,
) -> Result<()> {
    let (unit, file) = crate::modules::watchdog::HY2_CONFIGS[0];
    let prestart =
        !force && config == paths.base_dir.join(file) && invocation.is_direct_prestart(unit);
    let _lease = if prestart {
        None
    } else {
        Some(ControlLease::acquire(paths)?)
    };
    portjump::run(host, config, force, prestart)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::residential_lifecycle::ControlLease;
    use crate::sys::{fake::FakeHost, CmdOut, JournalFrom, JournalRecord, Proto, StagedWrite};
    use std::{
        collections::BTreeSet,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    // Record real wrapper observations and optionally pause an external-operation boundary.
    // FakeHost remains the external system; the actual flock and binding file are real.
    type Checkpoint = Box<dyn Fn(&str) + Send + Sync>;

    #[derive(Default)]
    struct AuditHost {
        fake: FakeHost,
        reads: Mutex<Vec<String>>,
        before: Option<Checkpoint>,
    }
    impl AuditHost {
        fn observe(&self, what: String) {
            self.reads.lock().unwrap().push(what.clone());
            if let Some(before) = &self.before {
                before(&what);
            }
        }
    }
    macro_rules! delegate {
        ($(fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
            $(fn $name(&self $(, $arg: $ty)*) -> $ret { self.fake.$name($($arg),*) })*
        };
    }
    impl Host for AuditHost {
        fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
            self.observe(format!("read:{}", path.display()));
            self.fake.read_file(path)
        }
        fn which(&self, program: &str) -> bool {
            self.observe(format!("which:{program}"));
            self.fake.which(program)
        }
        fn unit_property(&self, unit: &str, prop: &str) -> Result<Option<String>> {
            self.observe(format!("unit:{unit}:{prop}"));
            self.fake.unit_property(unit, prop)
        }
        fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut> {
            self.observe(format!("run:{program} {}", args.join(" ")));
            self.fake.run(program, args)
        }
        fn run_stdin(&self, program: &str, args: &[&str], input: &str) -> Result<CmdOut> {
            self.observe(format!("run:{program} {}", args.join(" ")));
            self.fake.run_stdin(program, args, input)
        }
        fn stage_file<'a>(&'a self, dest: &Path, mode: u32) -> Result<Box<dyn StagedWrite + 'a>> {
            self.fake.stage_file(dest, mode)
        }
        delegate! {
            fn file_sha256(&self, path: &Path) -> Result<Option<String>>;
            fn write_file(&self, path: &Path, content: &[u8], mode: u32) -> Result<()>;
            fn rename_file(&self, from: &Path, to: &Path) -> Result<()>;
            fn sync_parent(&self, path: &Path) -> Result<()>;
            fn set_file_mode(&self, path: &Path, mode: u32) -> Result<()>;
            fn remove_file(&self, path: &Path) -> Result<()>;
            fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
            fn is_dir(&self, path: &Path) -> Result<bool>;
            fn remove_dir_all(&self, path: &Path) -> Result<()>;
            fn is_symlink(&self, path: &Path) -> Result<bool>;
            fn read_link(&self, path: &Path) -> Result<Option<PathBuf>>;
            fn symlink(&self, target: &Path, link: &Path) -> Result<()>;
            fn set_immutable(&self, path: &Path, on: bool) -> Result<()>;
            fn is_immutable(&self, path: &Path) -> Result<bool>;
            fn run_journalctl(&self, args: &[&str]) -> Result<CmdOut>;
            fn systemd_daemon_reload(&self) -> Result<()>;
            fn systemd(&self, verb: &str, unit: &str) -> Result<CmdOut>;
            fn unit_is_active(&self, unit: &str) -> Result<bool>;
            fn unit_is_enabled(&self, unit: &str) -> Result<bool>;
            fn unit_exists(&self, unit: &str) -> Result<bool>;
            fn sysctl_get(&self, key: &str) -> Result<Option<String>>;
            fn sysctl_set(&self, key: &str, value: &str) -> Result<()>;
            fn modprobe(&self, module: &str) -> Result<()>;
            fn mem_mb(&self) -> Result<u64>;
            fn arch(&self) -> Result<String>;
            fn hostname(&self) -> Result<String>;
            fn listening_ports(&self, proto: Proto) -> Result<BTreeSet<u16>>;
            fn now(&self) -> time::OffsetDateTime;
            fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>>;
        }
    }

    fn seeded() -> (tempfile::TempDir, Paths, AuditHost) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: dir.path().into(),
            bin_dir: dir.path().join("bin"),
            certs_dir: dir.path().join("certs"),
        };
        let host = AuditHost::default();
        let state = crate::testutil::sample_state();
        crate::residential_lifecycle::seed_applied_fixture(dir.path(), &state, &paths);
        host.fake
            .write_file(
                &crate::paths::state_file(&paths),
                &serde_json::to_vec(&state).unwrap(),
                0o600,
            )
            .unwrap();
        host.fake
            .write_file(
                &paths.base_dir.join("config.yaml"),
                b"listen: :10000,20000-30000\n",
                0o600,
            )
            .unwrap();
        host.fake.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                CmdOut::success("table ip hysteria_12345678\n"),
            ));
            i.scripted.push((
                "nft list table ip hysteria_12345678".into(),
                CmdOut::success("udp dport 20000-30000 redirect to :10000\n"),
            ));
        });
        host.fake.clear_ops();
        (dir, paths, host)
    }
    fn invocation(unit: &str) -> Invocation {
        Invocation {
            invocation_id: Some("0123456789abcdef0123456789abcdef".into()),
            parent_pid: Some(1),
            manager_comm: Some("systemd\n".into()),
            cgroup: Some(format!("0::/system.slice/{unit}.service\n")),
        }
    }
    fn assert_busy<T: std::fmt::Debug>(result: Result<T>, host: &AuditHost) {
        let error = result.expect_err("manual writer must refuse the occupied machine lease");
        assert!(
            error.to_string().contains("control owner unavailable"),
            "{error:#}"
        );
        assert!(
            host.reads.lock().unwrap().is_empty(),
            "basis/external access before ownership: {:?}",
            host.reads
        );
        assert!(host.fake.ops().is_empty());
    }

    #[test]
    fn manual_nft_apply_refuses_before_state_or_binding_read() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        // A missing binding would fail apply if sampling were reached.
        std::fs::remove_file(paths.base_dir.join(".residential-lifecycle/topology.json")).unwrap();
        assert_busy(
            nft_command(&host, &paths, NftCmd::Apply, &Invocation::default()),
            &host,
        );
    }
    #[test]
    fn manual_nft_delete_refuses_before_nft_probe_or_delete() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        assert_busy(
            nft_command(&host, &paths, NftCmd::Delete, &Invocation::default()),
            &host,
        );
    }
    #[test]
    fn manual_native_prestart_refuses_before_inactive_guard_or_cleanup() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        assert_busy(
            hy2_prestart(
                &host,
                &paths,
                &paths.base_dir.join("config.yaml"),
                false,
                &Invocation::default(),
            ),
            &host,
        );
    }
    #[test]
    fn manual_force_refuses_before_config_read_or_cleanup() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        assert_busy(
            hy2_prestart(
                &host,
                &paths,
                &paths.base_dir.join("config.yaml"),
                true,
                &Invocation::default(),
            ),
            &host,
        );
    }
    #[test]
    fn genuine_prestart_uses_published_binding_without_lease_reentry() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        let mut desired = crate::testutil::sample_state();
        desired.node.ports.hy2_resi = 53000;
        host.fake
            .write_file(
                &crate::paths::state_file(&paths),
                &serde_json::to_vec(&desired).unwrap(),
                0o600,
            )
            .unwrap();
        host.fake.clear_ops();
        nft_command(
            &host,
            &paths,
            NftCmd::Apply,
            &invocation("hysteria-residential"),
        )
        .unwrap();
        let inputs = host.fake.stdins();
        assert_eq!(inputs.len(), 2);
        assert!(inputs[1].1.contains("redirect to :40000"));
        assert!(!inputs[1].1.contains("53000"));
        assert_eq!(host.fake.ops(), ["run:nft -c -f -", "run:nft -f -"]);
    }
    #[test]
    fn genuine_native_prestart_keeps_cleanup_without_active_query_or_reentry() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        host.fake.with(|i| {
            i.unit_props.insert(
                ("hysteria-server.service".into(), "ActiveState".into()),
                "active".into(),
            );
        });
        hy2_prestart(
            &host,
            &paths,
            &paths.base_dir.join("config.yaml"),
            false,
            &invocation("hysteria-server"),
        )
        .unwrap();
        assert!(host.fake.unit_prop_reads().is_empty());
        assert!(host
            .fake
            .ops()
            .contains(&"run:nft delete table ip hysteria_12345678".into()));
        assert!(!host.fake.ops().iter().any(|op| op.contains("inet bui")));
    }
    #[test]
    fn inherited_or_unknown_invocation_never_bypasses_the_owner() {
        let mut contexts = vec![
            Invocation::default(),
            invocation("unrelated"),
            invocation("hysteria-server"),
        ];
        let mut child = invocation("hysteria-residential");
        child.parent_pid = Some(42);
        contexts.push(child);
        let mut unknown = invocation("hysteria-residential");
        unknown.cgroup = None;
        contexts.push(unknown);
        let mut non_manager = invocation("hysteria-residential");
        non_manager.manager_comm = Some("init\n".into());
        contexts.push(non_manager);
        let mut missing_id = invocation("hysteria-residential");
        missing_id.invocation_id = None;
        contexts.push(missing_id);
        let mut malformed_id = invocation("hysteria-residential");
        malformed_id.invocation_id = Some("inherited".into());
        contexts.push(malformed_id);
        let mut child_cgroup = invocation("hysteria-residential");
        child_cgroup.cgroup = Some("0::/system.slice/hysteria-residential.service/child\n".into());
        contexts.push(child_cgroup);
        for context in contexts {
            let (_dir, paths, host) = seeded();
            let _owner = ControlLease::acquire(&paths).unwrap();
            assert_busy(nft_command(&host, &paths, NftCmd::Apply, &context), &host);
        }
    }
    #[test]
    fn prestart_identity_does_not_exempt_delete_force_or_another_config() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        assert_busy(
            nft_command(
                &host,
                &paths,
                NftCmd::Delete,
                &invocation("hysteria-residential"),
            ),
            &host,
        );
        assert_busy(
            hy2_prestart(
                &host,
                &paths,
                &paths.base_dir.join("config.yaml"),
                true,
                &invocation("hysteria-server"),
            ),
            &host,
        );
        assert_busy(
            hy2_prestart(
                &host,
                &paths,
                Path::new("/other/config.yaml"),
                false,
                &invocation("hysteria-server"),
            ),
            &host,
        );
        assert_busy(
            hy2_prestart(
                &host,
                &paths,
                &paths.base_dir.join("config.yaml"),
                false,
                &invocation("unrelated"),
            ),
            &host,
        );
    }
    #[test]
    fn status_and_already_owned_low_level_delete_do_not_reenter() {
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        nft_command(&host, &paths, NftCmd::Status, &Invocation::default()).unwrap();
        assert_eq!(host.fake.ops(), ["run:nft list table inet bui"]);
        super::super::nft::delete(&host, &paths).unwrap();
        assert!(host
            .fake
            .ops()
            .contains(&"run:nft delete table inet bui".into()));
    }
    #[test]
    fn manual_lease_is_held_at_every_basis_and_effect_boundary() {
        for operation in 0..4 {
            let (_dir, paths, mut host) = seeded();
            let checking_paths = paths.clone();
            host.before = Some(Box::new(move |boundary| {
                assert!(
                    ControlLease::acquire(&checking_paths).is_err(),
                    "owner may enter while manual operation is at {boundary}"
                );
            }));
            match operation {
                0 => {
                    nft_command(&host, &paths, NftCmd::Apply, &Invocation::default()).unwrap();
                }
                1 => {
                    nft_command(&host, &paths, NftCmd::Delete, &Invocation::default()).unwrap();
                }
                _ => {
                    hy2_prestart(
                        &host,
                        &paths,
                        &paths.base_dir.join("config.yaml"),
                        operation == 3,
                        &Invocation::default(),
                    )
                    .unwrap();
                }
            }
            assert!(!host.reads.lock().unwrap().is_empty());
            ControlLease::acquire(&paths).expect("completion must release manual ownership");
        }
    }
    #[test]
    fn delayed_old_binding_apply_excludes_a_new_owner_until_its_actual_write_finishes() {
        let (_dir, paths, mut host) = seeded();
        let (arrived_tx, arrived_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
        let resume_rx = Mutex::new(resume_rx);
        host.before = Some(Box::new(move |boundary| {
            if boundary == "run:nft -c -f -" {
                arrived_tx.send(()).unwrap();
                resume_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
        }));
        let host = Arc::new(host);
        let thread_host = host.clone();
        let thread_paths = paths.clone();
        let worker = std::thread::spawn(move || {
            nft_command(
                &*thread_host,
                &thread_paths,
                NftCmd::Apply,
                &Invocation::default(),
            )
        });
        arrived_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Old binding is sampled and precheck paused. An owner must not be able to
        // publish a new active topology in this interval.
        let next_owner = ControlLease::acquire(&paths);
        let refused = next_owner.is_err();
        drop(next_owner);
        resume_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(
            refused,
            "new owner entered after manual old-binding sample but before nft write"
        );
        let _owner = ControlLease::acquire(&paths).unwrap();
        let mut state = crate::testutil::sample_state();
        state.node.ports.hy2_resi = 53000;
        crate::residential_lifecycle::seed_applied_fixture(&paths.base_dir, &state, &paths);
        let fresh = AuditHost {
            fake: FakeHost::new(),
            ..Default::default()
        };
        fresh
            .fake
            .write_file(
                &crate::paths::state_file(&paths),
                &serde_json::to_vec(&state).unwrap(),
                0o600,
            )
            .unwrap();
        fresh.fake.with(|i| {
            i.which.insert("nft".into());
        });
        nft_command(
            &fresh,
            &paths,
            NftCmd::Apply,
            &invocation("hysteria-residential"),
        )
        .unwrap();
        assert!(fresh.fake.stdins()[1].1.contains("redirect to :53000"));
    }
    #[test]
    fn proc_identity_requires_the_exact_manager_hierarchy() {
        for (cgroup, allowed) in [
            ("0::/system.slice/hysteria-residential.service\n", true),
            ("7:memory:/system.slice/hysteria-residential.service\n1:name=systemd:/system.slice/hysteria-residential.service\n", true),
            ("0::/system.slice/hysteria-residential.service\n1:name=systemd:/system.slice/other.service\n", false),
            ("0::/system.slice/hysteria-residential.service-extra\n", false),
            ("0::/user.slice/user-0.slice/hysteria-residential.service\n", false),
            ("7:memory:/system.slice/hysteria-residential.service\n", false),
            ("0::/\n", false),
            ("malformed", false),
        ] {
            let (_dir, paths, host) = seeded();
            let _owner = ControlLease::acquire(&paths).unwrap();
            for (path, content) in [("/proc/self/status", "Name:\tbui\nPPid:\t1\n"), ("/proc/1/comm", "systemd\n"), ("/proc/self/cgroup", cgroup)] {
                host.fake.write_file(Path::new(path), content.as_bytes(), 0o400).unwrap();
            }
            let context = Invocation::read(&host, Some("0123456789abcdef0123456789abcdef".into()));
            assert_eq!(*host.reads.lock().unwrap(), ["read:/proc/self/status", "read:/proc/1/comm", "read:/proc/self/cgroup"]);
            host.reads.lock().unwrap().clear(); host.fake.clear_ops();
            let result = nft_command(&host, &paths, NftCmd::Apply, &context);
            if allowed { result.unwrap(); assert_eq!(host.fake.stdins().len(), 2); }
            else { assert_busy(result, &host); }
        }
        let (_dir, paths, host) = seeded();
        let _owner = ControlLease::acquire(&paths).unwrap();
        // No proc observations must never be promoted to an owned prestart.
        let context = Invocation::read(&host, Some("0123456789abcdef0123456789abcdef".into()));
        host.reads.lock().unwrap().clear();
        assert_busy(nft_command(&host, &paths, NftCmd::Apply, &context), &host);
    }

    #[test]
    fn offline_native_active_guard_and_force_keep_their_existing_meaning() {
        let (_dir, paths, host) = seeded();
        host.fake.with(|i| {
            i.unit_props.insert(
                ("hysteria-server.service".into(), "ActiveState".into()),
                "active".into(),
            );
        });
        let error = hy2_prestart(
            &host,
            &paths,
            &paths.base_dir.join("config.yaml"),
            false,
            &Invocation::default(),
        )
        .unwrap_err();
        assert!(error.is::<portjump::InstanceRunning>());
        assert!(host.fake.ops().is_empty());
        ControlLease::acquire(&paths).expect("active guard error releases ownership");
        host.fake.with(|i| {
            i.unit_prop_reads.clear();
        });
        hy2_prestart(
            &host,
            &paths,
            &paths.base_dir.join("config.yaml"),
            true,
            &Invocation::default(),
        )
        .unwrap();
        assert!(host.fake.unit_prop_reads().is_empty());
        assert!(host
            .fake
            .ops()
            .contains(&"run:nft delete table ip hysteria_12345678".into()));
    }

    #[test]
    fn inactive_guard_to_cleanup_interval_excludes_a_new_daemon_owner() {
        let (_dir, paths, mut host) = seeded();
        let config = paths.base_dir.join("config.yaml");
        let pause_at = format!("read:{}", config.display());
        let (arrived_tx, arrived_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
        let resume_rx = Mutex::new(resume_rx);
        host.before = Some(Box::new(move |boundary| {
            if boundary == pause_at {
                arrived_tx.send(()).unwrap();
                resume_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
        }));
        let host = Arc::new(host);
        let thread_host = host.clone();
        let thread_paths = paths.clone();
        let worker = std::thread::spawn(move || {
            hy2_prestart(
                &*thread_host,
                &thread_paths,
                &config,
                false,
                &Invocation::default(),
            )
        });
        arrived_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            host.fake.unit_prop_reads(),
            [("hysteria-server.service".into(), "ActiveState".into())]
        );
        // Inactive was sampled, cleanup not reached: the daemon cannot obtain
        // machine ownership and start new live native rules in this interval.
        let next_owner = ControlLease::acquire(&paths);
        let refused = next_owner.is_err();
        drop(next_owner);
        resume_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(
            refused,
            "daemon could start a native instance between inactive check and cleanup"
        );
        assert!(host
            .fake
            .ops()
            .contains(&"run:nft delete table ip hysteria_12345678".into()));
        ControlLease::acquire(&paths).expect("actual cleanup completion releases ownership");
    }
}
