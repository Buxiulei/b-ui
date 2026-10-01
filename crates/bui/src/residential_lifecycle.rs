//! Exclusive residential publication, activation and durable acknowledgement.
//! Receipts describe requested files and an observed systemd instance; the stock
//! Clash API cannot attest a loaded configuration hash.
use crate::modules::panel::{gates, Shared};
use crate::reconcile::{diff::Change, DaemonCtx};
use crate::sys::{Host, Proto};
use anyhow::{Context, Result};
use bui_schema::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, OwnedMutexGuard};

pub const UNIT: &str = "hysteria-residential";
const BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// A real flock, never inferred from a socket or pid file. Do not unlink it.
pub struct ControlLease {
    _lock: nix::fcntl::Flock<std::fs::File>,
}
impl ControlLease {
    pub fn acquire(paths: &Paths) -> Result<Self> {
        let path = if paths.base_dir == Path::new("/opt/b-ui") {
            PathBuf::from("/run/b-ui-control.lock")
        } else {
            paths.base_dir.join(".residential-control.lock")
        };
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| anyhow::anyhow!("control owner unavailable ({}): {error}; use the daemon UDS or stop it before offline mutation", path.display()))?;
        Ok(Self { _lock: lock })
    }
}

/// The lease travels with the actual mutation task, not its cancellable waiter.
pub async fn offline<T, F>(paths: &Paths, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T>> + Send + 'static,
{
    let lease = ControlLease::acquire(paths)?;
    tokio::spawn(async move {
        let _lease = lease;
        operation.await
    })
    .await?
}

#[derive(Debug, Default, PartialEq)]
pub struct Deferred {
    pub changes: Vec<Change>,
    pub keys: BTreeMap<String, String>,
}
pub fn is_residential_change(c: &Change, paths: &Paths) -> bool {
    let config = crate::modules::core_files::hy2_resi_config_path(paths);
    match c {
        Change::WriteFile { path, restart, .. } => {
            path == &config || restart.as_ref().is_some_and(|unit| unit.name == UNIT)
        }
        Change::RemoveFile { path } | Change::SetImmutable { path, .. } => path == &config,
        Change::WriteUnit { unit, .. } => unit.name == UNIT,
        Change::SetUnitState { unit, .. } => unit == UNIT,
        Change::InstallBinary { name, .. } => name == "sing-box",
        _ => false,
    }
}

#[derive(Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
#[error("residential activation is awaiting its first TLS certificate")]
pub struct AwaitingCertificate;

pub struct Published {
    pub receipt: Receipt,
    config: Vec<u8>,
    pub action: Action,
    pub binary_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub invocation: String,
    pub pid: u32,
    pub started: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Running(Instance),
    Stopped,
    Undeployed,
}
pub fn observe(host: &dyn Host) -> Result<Observation> {
    let out = host.run("systemctl", &["show", "hysteria-residential.service", "--property=InvocationID,MainPID,ExecMainStartTimestampMonotonic,ActiveState,SubState,LoadState", "--no-pager"])?;
    anyhow::ensure!(
        out.ok(),
        "residential instance observation failed: systemctl status {}",
        out.status
    );
    parse_observation(&out.stdout)
}
pub fn parse_observation(text: &str) -> Result<Observation> {
    let fields: BTreeMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let get = |key| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("instance observation missing {key}"))
    };
    let active = get("ActiveState")?;
    let sub = get("SubState")?;
    let loaded = get("LoadState")?;
    let pid: u32 = get("MainPID")?.parse().context("invalid MainPID")?;
    if active == "inactive" && sub == "dead" && pid == 0 {
        if loaded == "not-found" && get("InvocationID")?.is_empty() {
            return Ok(Observation::Undeployed);
        }
        if loaded == "loaded" {
            return Ok(Observation::Stopped);
        }
    }
    anyhow::ensure!(loaded == "loaded", "residential unit is not loaded");
    anyhow::ensure!(
        active == "active" && sub == "running" && pid > 0,
        "residential state is unknown/non-running"
    );
    let invocation = get("InvocationID")?;
    anyhow::ensure!(
        invocation.len() == 32
            && invocation.bytes().all(|b| b.is_ascii_hexdigit())
            && invocation.bytes().any(|b| b != b'0'),
        "invalid InvocationID"
    );
    let started: u64 = get("ExecMainStartTimestampMonotonic")?
        .parse()
        .context("invalid process start identity")?;
    anyhow::ensure!(started > 0, "missing process start identity");
    Ok(Observation::Running(Instance {
        invocation: invocation.into(),
        pid,
        started,
    }))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Reconcile,
    Manual,
    Watchdog,
    Certificate,
    Recovery,
}
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Start,
    Restart,
    Stop,
    Observe,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    path: PathBuf,
    previous: Option<PathBuf>,
    previous_sha: Option<String>,
    published_sha: Option<String>,
    mode: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub operation_id: String,
    pub source: Source,
    pub phase: String,
    pub requested_config_sha: String,
    pub topology_sha: String,
    pub maintenance: bool,
    pub previous_instance: Option<Instance>,
    pub before: Option<Instance>,
    pub after: Option<Instance>,
    pub authorization_sha: Option<String>,
    pub gate_count: usize,
    pub pending_keys: BTreeMap<String, String>,
    pub error: Option<String>,
    pub cert_sha: Option<String>,
    pub relay_recovery_confirmed: Option<bool>,
    backups: Vec<Backup>,
}

pub struct Lifecycle {
    mutation: Arc<tokio::sync::Mutex<()>>,
    shared: Arc<Shared>,
    jobs: Mutex<Jobs>,
}
#[derive(Default)]
struct Jobs {
    closing: bool,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
/// Only the independently owned task can create a transaction token.
pub struct Transaction {
    owner: Arc<Lifecycle>,
    _guard: Arc<OwnedMutexGuard<()>>,
}
impl Lifecycle {
    pub fn new(shared: Arc<Shared>) -> Arc<Self> {
        Arc::new(Self {
            mutation: Arc::new(tokio::sync::Mutex::new(())),
            shared,
            jobs: Mutex::new(Jobs::default()),
        })
    }
    pub async fn execute<T, F, Fut>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Transaction) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let owner = self.clone();
        {
            let mut jobs = self.jobs.lock().expect("jobs mutex");
            anyhow::ensure!(!jobs.closing, "residential owner is shutting down");
            let job = tokio::spawn(async move {
                let guard = Arc::new(owner.mutation.clone().lock_owned().await);
                let result = f(Transaction {
                    owner,
                    _guard: guard.clone(),
                })
                .await;
                drop(guard);
                let _ = tx.send(result);
            });
            jobs.tasks.retain(|j| !j.is_finished());
            jobs.tasks.push(job);
        }
        rx.await
            .context("residential owner task ended without a result")?
    }
    pub async fn drain(&self) {
        loop {
            let jobs = {
                let mut jobs = self.jobs.lock().expect("jobs mutex");
                jobs.closing = true;
                std::mem::take(&mut jobs.tasks)
            };
            if jobs.is_empty() {
                return;
            }
            for job in jobs {
                let _ = job.await;
            }
        }
    }
    pub async fn manual(self: &Arc<Self>, ctx: DaemonCtx, action: Action) -> Result<Receipt> {
        self.execute(move |tx| async move { tx.activate(&ctx, Source::Manual, action, None).await })
            .await
    }
    pub async fn repair(self: &Arc<Self>, ctx: DaemonCtx) -> Result<Option<Receipt>> {
        self.execute(move |tx| async move {
            if maintenance(&ctx)? {
                return Ok(None);
            }
            let observed = observe_async(&ctx).await?;
            let Observation::Running(instance) = observed else {
                return Ok(None);
            };
            let _ = instance; // every daemon observation revalidates authorization and readback
            tx.activate(&ctx, Source::Recovery, Action::Observe, None)
                .await
                .map(Some)
        })
        .await
    }
    pub async fn watchdog(
        self: &Arc<Self>,
        ctx: DaemonCtx,
        proto: Proto,
        port: u16,
    ) -> Result<bool> {
        self.execute(move |tx| async move {
            if maintenance(&ctx)? {
                return Ok(false);
            }
            let host = ctx.host.clone();
            let needs = tokio::task::spawn_blocking(move || -> Result<bool> {
                Ok(matches!(observe(host.as_ref())?, Observation::Running(_))
                    && !host.listening_ports(proto)?.contains(&port))
            })
            .await??;
            if !needs {
                return Ok(false);
            }
            tx.activate(&ctx, Source::Watchdog, Action::Restart, None)
                .await?;
            Ok(true)
        })
        .await
    }
}

pub fn config_bytes(s: &bui_schema::model::State, paths: &Paths) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(
        &bui_schema::render::hy2_singbox::config(&s.node, paths, &s.residential.hy2_pool),
    )?)
}
fn topology(s: &bui_schema::model::State, paths: &Paths) -> Result<String> {
    let mut bytes = config_bytes(s, paths)?;
    bytes.extend_from_slice(if s.system.hy2_resi_compat_ports {
        b"compat:on"
    } else {
        b"compat:off"
    });
    Ok(crate::kernels::sha256_hex(&bytes))
}
fn directory(ctx: &DaemonCtx) -> PathBuf {
    ctx.store.directory().join(".residential-lifecycle")
}
fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub fn read_record(ctx: &DaemonCtx) -> Result<Option<Receipt>> {
    match std::fs::read(directory(ctx).join("record.json")) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("invalid residential recovery record")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn maintenance(ctx: &DaemonCtx) -> Result<bool> {
    Ok(read_record(ctx)?.is_some_and(|r| r.maintenance))
}
fn persist(ctx: &DaemonCtx, r: &Receipt) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = directory(ctx);
    private_dir(&dir)?;
    let path = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(&serde_json::to_vec_pretty(r)?)?;
        f.sync_all()?;
        std::fs::rename(&path, dir.join("record.json"))?;
        std::fs::File::open(&dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}
async fn observe_async(ctx: &DaemonCtx) -> Result<Observation> {
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || observe(host.as_ref())).await?
}
async fn systemd(ctx: &DaemonCtx, action: &'static str) -> Result<()> {
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if action == "restart" || action == "start" {
            let _ = host.systemd("reset-failed", UNIT);
        }
        anyhow::ensure!(
            host.systemd(action, UNIT)?.ok(),
            "residential {action} command failed"
        );
        Ok(())
    })
    .await?
}

impl Transaction {
    /// The relay shares the published sing-box inode with residential ingress.
    pub async fn relay(&self, ctx: &DaemonCtx, action: String) -> Result<crate::sys::CmdOut> {
        let restarted = matches!(action.as_str(), "start" | "restart");
        let host = ctx.host.clone();
        let out =
            tokio::task::spawn_blocking(move || host.systemd(&action, "b-ui-relay")).await??;
        if out.ok() && restarted {
            let pools =
                crate::modules::residential::state::all_pool_selectors(&*ctx.store.read().await);
            crate::modules::residential::state::mark_pools_switch(
                &ctx.runtime,
                &pools,
                ctx.host.now(),
            )
            .await;
            ctx.bus.send(crate::api::Event::RelayRestarted);
        }
        Ok(out)
    }

    pub async fn publish(
        &self,
        ctx: &DaemonCtx,
        work: Deferred,
        candidate: Option<Candidate>,
    ) -> Result<Published> {
        let desired = ctx.store.read().await;
        let config = work
            .changes
            .iter()
            .find_map(|c| match c {
                Change::WriteFile { path, content, .. }
                    if path == &crate::modules::core_files::hy2_resi_config_path(&ctx.paths) =>
                {
                    Some(content.clone())
                }
                _ => None,
            })
            .unwrap_or(config_bytes(&desired, &ctx.paths)?);
        anyhow::ensure!(
            config == config_bytes(&desired, &ctx.paths)?,
            "residential candidate superseded before publication"
        );
        let was_maintenance = maintenance(ctx)?;
        let prior = read_record(ctx)?;
        let observed = observe_async(ctx).await;
        let live_config = ctx
            .host
            .read_file(&crate::modules::core_files::hy2_resi_config_path(
                &ctx.paths,
            ))?;
        let already_published = live_config.as_deref() == Some(config.as_slice());
        if live_config.is_none()
            && matches!(observed, Ok(Observation::Stopped | Observation::Undeployed))
        {
            let candidate_json: serde_json::Value = serde_json::from_slice(&config)?;
            let tls = &candidate_json["inbounds"][0]["tls"];
            let cert = ctx.host.read_file(Path::new(
                tls["certificate_path"]
                    .as_str()
                    .context("candidate certificate path missing")?,
            ))?;
            let key = ctx.host.read_file(Path::new(
                tls["key_path"]
                    .as_str()
                    .context("candidate key path missing")?,
            ))?;
            if cert.is_none() || key.is_none() {
                let mut receipt = self
                    .prepared(ctx, Source::Reconcile, &config, was_maintenance)
                    .await?;
                receipt.phase = "awaiting_certificate".into();
                receipt.pending_keys = work.keys;
                persist(ctx, &receipt)?;
                return Err(AwaitingCertificate.into());
            }
        }
        // A prior failed gate check does not require another restart of this same instance.
        let repair_only = already_published && prior.as_ref().is_some_and(|r| {
            r.requested_config_sha == crate::kernels::sha256_hex(&config)
                && matches!(&observed, Ok(Observation::Running(i)) if r.after.as_ref() == Some(i))
        }) && candidate.is_none();
        let mut r = self
            .prepared(ctx, Source::Reconcile, &config, was_maintenance)
            .await?;
        r.pending_keys = work.keys;
        let mut writes = Vec::new();
        let binary_changed = candidate.is_some();
        let mut restart = binary_changed;
        let mut stop = false;
        let mut enable = None;
        for change in work.changes {
            match change {
                Change::WriteFile {
                    path,
                    content,
                    mode,
                    ..
                } => {
                    restart |= ctx.host.read_file(&path)?.as_deref() != Some(content.as_slice());
                    writes.push((path, content, mode));
                }
                Change::WriteUnit { path, content, .. } => {
                    restart |= ctx.host.read_file(&path)?.as_deref() != Some(content.as_bytes());
                    writes.push((path, content.into_bytes(), 0o644));
                }
                Change::SetUnitState {
                    enabled, active, ..
                } => {
                    enable = Some(enabled);
                    stop |= !active;
                }
                Change::InstallBinary { .. } => {}
                _ => anyhow::bail!("unsupported residential publication change"),
            }
        }
        let op_dir = directory(ctx).join(&r.operation_id);
        private_dir(&op_dir)?;
        // Configuration validation uses the exact candidate binary before any live publication.
        let verify = op_dir.join("candidate.json");
        let host = ctx.host.clone();
        let bin = candidate
            .as_ref()
            .map(|c| c.path.clone())
            .unwrap_or_else(|| ctx.paths.bin_dir.join("sing-box"));
        let config2 = config.clone();
        let validated = tokio::task::spawn_blocking(move || -> Result<()> {
            host.write_file(&verify, &config2, 0o600)?;
            let out = host.run(
                &bin.display().to_string(),
                &["check", "-c", &verify.display().to_string()],
            )?;
            anyhow::ensure!(out.ok(), "residential candidate validation failed");
            Ok(())
        })
        .await?;
        if let Err(error) = validated {
            r.phase = "failed".into();
            r.error = Some(crate::redact::url_credentials(&error.to_string()));
            persist(ctx, &r)?;
            return Err(error);
        }
        for (index, (path, content, mode)) in writes.iter().enumerate() {
            let previous = if let Some(old) = ctx.host.read_file(path)? {
                let backup = op_dir.join(format!("previous-{index}"));
                ctx.host.write_file(&backup, &old, 0o600)?;
                ctx.host.sync_parent(&backup)?;
                Some(backup)
            } else {
                None
            };
            r.backups.push(Backup {
                path: path.clone(),
                previous,
                previous_sha: ctx.host.file_sha256(path)?,
                published_sha: Some(crate::kernels::sha256_hex(content)),
                mode: *mode,
            });
        }
        if let Some(c) = &candidate {
            anyhow::ensure!(
                ctx.host.file_sha256(&c.path)?.as_deref() == Some(c.sha256.as_str()),
                "candidate binary changed"
            );
            let backup_dir = ctx
                .paths
                .bin_dir
                .join(format!(".residential-backup-{}", r.operation_id));
            private_dir(&backup_dir)?;
            r.backups.push(Backup {
                path: ctx.paths.bin_dir.join("sing-box"),
                previous_sha: ctx.host.file_sha256(&ctx.paths.bin_dir.join("sing-box"))?,
                previous: ctx
                    .host
                    .file_sha256(&ctx.paths.bin_dir.join("sing-box"))?
                    .map(|_| backup_dir.join("previous-sing-box")),
                published_sha: Some(c.sha256.clone()),
                mode: 0o755,
            });
        }
        std::fs::File::open(&op_dir)?.sync_all()?;
        persist(ctx, &r)?; // write ahead before any live file mutation
        let publish = (|| -> Result<()> {
            if let Some(enabled) = enable {
                let out = ctx
                    .host
                    .systemd(if enabled { "enable" } else { "disable" }, UNIT)?;
                anyhow::ensure!(out.ok(), "residential unit enablement failed");
            }
            if let Some(c) = candidate {
                let live = ctx.paths.bin_dir.join("sing-box");
                if let Some(previous) = r.backups.last().and_then(|b| b.previous.as_ref()) {
                    ctx.host.rename_file(&live, previous)?;
                    ctx.host.set_file_mode(previous, 0o600)?;
                }
                ctx.host.rename_file(&c.path, &live)?;
            }
            for (path, content, mode) in &writes {
                ctx.host.write_file(path, content, *mode)?;
                ctx.host.sync_parent(path)?;
            }
            if writes
                .iter()
                .any(|(p, _, _)| p.starts_with("/etc/systemd/system"))
            {
                ctx.host.systemd_daemon_reload()?;
            }
            Ok(())
        })();
        if let Err(e) = publish {
            let error = self
                .rollback(ctx, &mut r, e)
                .await
                .expect_err("failed publication is never acknowledged");
            return Err(error);
        }
        let action = if was_maintenance || stop {
            Action::Stop
        } else if repair_only {
            Action::Observe
        } else if restart {
            Action::Restart
        } else if matches!(observed, Ok(Observation::Stopped | Observation::Undeployed)) {
            Action::Start
        } else {
            Action::Observe
        };
        Ok(Published {
            receipt: r,
            config,
            action,
            binary_changed,
        })
    }
    pub async fn publish_certificate(
        &self,
        ctx: &DaemonCtx,
        cert: Vec<u8>,
        key: Vec<u8>,
        sha: String,
    ) -> Result<Published> {
        let config = ctx
            .host
            .read_file(&crate::modules::core_files::hy2_resi_config_path(
                &ctx.paths,
            ))?
            .unwrap_or(config_bytes(ctx.store.read().await.as_ref(), &ctx.paths)?);
        let mut receipt = self
            .prepared(ctx, Source::Certificate, &config, maintenance(ctx)?)
            .await?;
        receipt.cert_sha = Some(sha);
        let dir = directory(ctx).join(&receipt.operation_id);
        private_dir(&dir)?;
        let files = [
            (ctx.paths.certs_dir.join("fullchain.pem"), cert, 0o644),
            (ctx.paths.certs_dir.join("privkey.pem"), key, 0o600),
        ];
        for (index, (path, bytes, mode)) in files.iter().enumerate() {
            let previous_sha = ctx.host.file_sha256(path)?;
            let previous = if let Some(old) = ctx.host.read_file(path)? {
                let backup = dir.join(format!("tls-{index}"));
                ctx.host.write_file(&backup, &old, 0o600)?;
                ctx.host.sync_parent(&backup)?;
                Some(backup)
            } else {
                None
            };
            receipt.backups.push(Backup {
                path: path.clone(),
                previous,
                previous_sha,
                published_sha: Some(crate::kernels::sha256_hex(bytes)),
                mode: *mode,
            });
        }
        persist(ctx, &receipt)?;
        for (path, bytes, mode) in files {
            if let Err(error) = ctx
                .host
                .write_file(&path, &bytes, mode)
                .and_then(|()| ctx.host.sync_parent(&path))
            {
                return Err(self
                    .rollback(ctx, &mut receipt, error)
                    .await
                    .expect_err("failed certificate publication"));
            }
        }
        Ok(Published {
            receipt,
            config,
            action: Action::Observe,
            binary_changed: false,
        })
    }

    pub fn certificate_stopped(
        &self,
        ctx: &DaemonCtx,
        mut published: Published,
    ) -> Result<Receipt> {
        anyhow::ensure!(
            matches!(
                observe(ctx.host.as_ref())?,
                Observation::Stopped | Observation::Undeployed
            ),
            "certificate consumer changed while stopped"
        );
        published.receipt.phase = "stopped".into();
        persist(ctx, &published.receipt)?;
        Ok(published.receipt)
    }

    pub async fn reject(
        &self,
        ctx: &DaemonCtx,
        mut published: Published,
        error: anyhow::Error,
    ) -> Result<Receipt> {
        self.rollback(ctx, &mut published.receipt, error).await
    }

    pub async fn complete(&self, ctx: &DaemonCtx, mut published: Published) -> Result<Receipt> {
        let r = &mut published.receipt;
        if matches!(published.action, Action::Stop) {
            systemd(ctx, "stop").await?;
            anyhow::ensure!(
                observe_async(ctx).await? == Observation::Stopped,
                "stop not observed"
            );
            r.phase = "stopped".into();
            persist(ctx, r)?;
            return Ok(published.receipt);
        }
        match self
            .finish(ctx, r, &published.config, published.action)
            .await
        {
            Ok(()) => Ok(published.receipt),
            Err(e) if r.phase == "activating" => self.rollback(ctx, r, e).await,
            Err(e) => {
                r.phase = "repair_required".into();
                r.error = Some(crate::redact::url_credentials(&e.to_string()));
                persist(ctx, r)?;
                Err(e)
            }
        }
    }
    async fn prepared(
        &self,
        ctx: &DaemonCtx,
        source: Source,
        config: &[u8],
        maintenance: bool,
    ) -> Result<Receipt> {
        Ok(Receipt {
            operation_id: uuid::Uuid::new_v4().to_string(),
            source,
            phase: "prepared".into(),
            requested_config_sha: crate::kernels::sha256_hex(config),
            topology_sha: topology(ctx.store.read().await.as_ref(), &ctx.paths)?,
            maintenance,
            previous_instance: match observe_async(ctx).await? {
                Observation::Running(i) => Some(i),
                Observation::Stopped | Observation::Undeployed => None,
            },
            before: None,
            after: None,
            authorization_sha: None,
            gate_count: gates::GateManifest::from_config(config)?.gates().len(),
            pending_keys: BTreeMap::new(),
            error: None,
            cert_sha: None,
            relay_recovery_confirmed: None,
            backups: Vec::new(),
        })
    }
    pub async fn activate(
        &self,
        ctx: &DaemonCtx,
        source: Source,
        action: Action,
        cert_sha: Option<String>,
    ) -> Result<Receipt> {
        let config = match ctx
            .host
            .read_file(&crate::modules::core_files::hy2_resi_config_path(
                &ctx.paths,
            ))? {
            Some(bytes) => bytes,
            None if matches!(action, Action::Stop) => {
                config_bytes(ctx.store.read().await.as_ref(), &ctx.paths)?
            }
            None => anyhow::bail!("residential configuration missing"),
        };
        let explicit = matches!(source, Source::Manual);
        anyhow::ensure!(
            explicit || !maintenance(ctx)?,
            "residential service is in maintenance"
        );
        let mut r = self
            .prepared(ctx, source, &config, matches!(action, Action::Stop))
            .await?;
        r.cert_sha = cert_sha;
        persist(ctx, &r)?;
        if matches!(action, Action::Stop) {
            let result = async {
                systemd(ctx, "stop").await?;
                anyhow::ensure!(
                    observe_async(ctx).await? == Observation::Stopped,
                    "residential stop not observed"
                );
                r.phase = "stopped".into();
                persist(ctx, &r)
            }
            .await;
            if let Err(e) = result {
                r.phase = "recovery_required".into();
                r.error = Some(crate::redact::url_credentials(&e.to_string()));
                persist(ctx, &r)?;
                return Err(e);
            }
            return Ok(r);
        }
        if let Err(e) = self.finish(ctx, &mut r, &config, action).await {
            r.phase = "repair_required".into();
            r.error = Some(crate::redact::url_credentials(&e.to_string()));
            persist(ctx, &r)?;
            return Err(e);
        }
        Ok(r)
    }
    async fn finish(
        &self,
        ctx: &DaemonCtx,
        r: &mut Receipt,
        config: &[u8],
        action: Action,
    ) -> Result<()> {
        let current = ctx.store.read().await;
        anyhow::ensure!(
            config == config_bytes(&current, &ctx.paths)?,
            "live residential candidate differs from current desired topology"
        );
        anyhow::ensure!(
            r.topology_sha == topology(&current, &ctx.paths)?,
            "residential topology superseded before activation"
        );
        r.phase = "activating".into();
        persist(ctx, r)?;
        match action {
            Action::Restart => systemd(ctx, "restart").await?,
            Action::Start => systemd(ctx, "start").await?,
            Action::Observe => {}
            Action::Stop => anyhow::bail!("stop cannot activate"),
        }
        let Observation::Running(before) = observe_async(ctx).await? else {
            anyhow::bail!("residential service did not become active");
        };
        r.before = Some(before.clone());
        r.after = Some(before.clone());
        r.phase = "replaying".into();
        persist(ctx, r)?;
        let deadline = tokio::time::Instant::now() + BUDGET;
        let manifest = gates::GateManifest::from_config(config)?;
        let permit =
            gates::restore_and_verify(ctx, &self.owner.shared, &manifest, deadline).await?;
        let value: serde_json::Value = serde_json::from_slice(config)?;
        let inbounds = value["inbounds"]
            .as_array()
            .context("candidate inbounds missing")?;
        let ports: Vec<_> = inbounds
            .iter()
            .filter(|i| i["type"] == "hysteria2")
            .filter_map(|i| {
                i["listen_port"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|p| *p > 0)
            })
            .collect();
        anyhow::ensure!(
            ports.len() == 1,
            "candidate residential UDP listener is ambiguous"
        );
        let expected_port = ports[0];
        let host = ctx.host.clone();
        let listening = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || host.listening_ports(Proto::Udp)),
        )
        .await
        .context("listener observation deadline exhausted")???;
        anyhow::ensure!(
            listening.contains(&expected_port),
            "candidate residential UDP listener is absent"
        );
        anyhow::ensure!(
            tokio::time::timeout_at(deadline, observe_async(ctx))
                .await
                .context("instance observation deadline exhausted")??
                == Observation::Running(before),
            "residential instance changed during gate barrier"
        );
        anyhow::ensure!(
            r.topology_sha == topology(ctx.store.read().await.as_ref(), &ctx.paths)?,
            "residential topology superseded during activation"
        );
        anyhow::ensure!(
            ctx.host
                .file_sha256(&crate::modules::core_files::hy2_resi_config_path(
                    &ctx.paths
                ))?
                .as_deref()
                == Some(r.requested_config_sha.as_str()),
            "published residential file drifted during activation"
        );
        permit.validate_or_close(ctx.host.now(), deadline).await?;
        r.authorization_sha = Some(permit.projection_sha256().to_owned());
        r.gate_count = manifest.gates().len();
        r.phase = "active".into();
        r.error = None;
        persist(ctx, r)?;
        // Time is not frozen by the Store fence. Expiry across the fsync also
        // invalidates acknowledgement and closes revoked gates before releasing it.
        if let Err(e) = permit.validate_or_close(ctx.host.now(), deadline).await {
            r.phase = "repair_required".into();
            r.error = Some(crate::redact::url_credentials(&e.to_string()));
            persist(ctx, r)?;
            return Err(e);
        }
        ctx.runtime
            .update(|rt| rt.restart_keys.extend(r.pending_keys.clone()))
            .await;
        drop(permit);
        Ok(())
    }
    async fn rollback(
        &self,
        ctx: &DaemonCtx,
        r: &mut Receipt,
        original: anyhow::Error,
    ) -> Result<Receipt> {
        r.phase = "recovery_required".into();
        r.error = Some(crate::redact::url_credentials(&original.to_string()));
        persist(ctx, r)?;
        for backup in r.backups.iter().rev() {
            let current = ctx.host.file_sha256(&backup.path)?;
            if current == backup.previous_sha {
                continue;
            }
            let moved_binary = current.is_none()
                && backup.path == ctx.paths.bin_dir.join("sing-box")
                && backup
                    .previous
                    .as_ref()
                    .is_some_and(|p| ctx.host.file_sha256(p).ok().flatten() == backup.previous_sha);
            anyhow::ensure!(
                current == backup.published_sha || moved_binary,
                "foreign drift prevents residential rollback"
            );
            if let Some(previous) = &backup.previous {
                if backup.path == ctx.paths.bin_dir.join("sing-box") {
                    ctx.host.rename_file(previous, &backup.path)?;
                    ctx.host.set_file_mode(&backup.path, backup.mode)?;
                } else {
                    let bytes = ctx
                        .host
                        .read_file(previous)?
                        .context("residential rollback file missing")?;
                    ctx.host.write_file(&backup.path, &bytes, backup.mode)?;
                }
            } else {
                ctx.host.remove_file(&backup.path)?;
            }
            ctx.host.sync_parent(&backup.path)?;
        }
        ctx.host.systemd_daemon_reload()?;
        if r.backups
            .iter()
            .any(|b| b.path == ctx.paths.bin_dir.join("sing-box"))
        {
            let host = ctx.host.clone();
            let recovered = tokio::task::spawn_blocking(move || -> Result<bool> {
                Ok(host.systemd("restart", "b-ui-relay")?.ok()
                    && host.unit_is_active("b-ui-relay")?)
            })
            .await?;
            r.relay_recovery_confirmed = Some(recovered.unwrap_or(false));
            persist(ctx, r)?;
            anyhow::ensure!(
                r.relay_recovery_confirmed == Some(true),
                "shared binary rollback could not confirm relay recovery"
            );
        }
        if let Some(config) =
            ctx.host
                .read_file(&crate::modules::core_files::hy2_resi_config_path(
                    &ctx.paths,
                ))?
        {
            let mut previous = self.prepared(ctx, Source::Recovery, &config, false).await?;
            previous.pending_keys.clear();
            previous.relay_recovery_confirmed = r.relay_recovery_confirmed;
            match self
                .finish(ctx, &mut previous, &config, Action::Restart)
                .await
            {
                Ok(()) => {
                    previous.error = Some(crate::redact::url_credentials(&format!(
                        "candidate rejected; previous configuration recovered: {original}"
                    )));
                    persist(ctx, &previous)?;
                }
                Err(e) => {
                    previous.phase = "recovery_required".into();
                    previous.error = Some(crate::redact::url_credentials(&format!(
                        "candidate: {original}; rollback: {e}"
                    )));
                    persist(ctx, &previous)?;
                }
            }
        }
        Err(original)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::fakes::{FakeHy2, FakeHy2Resi, FakeXray};
    use crate::state::{runtime::Runtime, store::Store};
    use crate::sys::fake::FakeHost;

    async fn fixture() -> (tempfile::TempDir, DaemonCtx, Arc<FakeHost>, FakeHy2Resi) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: dir.path().into(),
            bin_dir: dir.path().join("bin"),
            certs_dir: dir.path().join("certs"),
        };
        let mut state = crate::testutil::sample_state();
        bui_schema::hy2pool::grow(&mut state.residential.hy2_pool, 32, &Default::default());
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.instances.insert(format!("{UNIT}.service"), 1);
            i.stock_singbox = true;
            i.listening
                .insert(Proto::Udp, [40000].into_iter().collect());
            i.units_active.insert(format!("{UNIT}.service"));
        });
        host.write_file(
            &crate::modules::core_files::hy2_resi_config_path(&paths),
            &config_bytes(&state, &paths).unwrap(),
            0o600,
        )
        .unwrap();
        let fake = FakeHy2Resi::new();
        let shared = Arc::new(
            Shared::new(Box::new(FakeXray::new()), Box::new(FakeHy2::new()))
                .with_hy2resi(Box::new(fake.clone())),
        );
        let ctx = DaemonCtx {
            store: Store::create(dir.path().join("state.json"), state)
                .await
                .unwrap(),
            runtime: Runtime::load(dir.path().join("runtime.json")),
            bus: crate::api::EventBus::new(),
            host: host.clone(),
            paths,
        };
        ctx.bus.bind_residential(shared);
        (dir, ctx, host, fake)
    }

    #[test]
    fn identity_requires_complete_consistent_systemd_observation() {
        assert!(parse_observation("").is_err());
        assert!(parse_observation("LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=0\nInvocationID=\nExecMainStartTimestampMonotonic=0").is_err());
        // Real systemd wire observation for a non-existent unit (exit status 0).
        assert_eq!(parse_observation("LoadState=not-found\nActiveState=inactive\nSubState=dead\nInvocationID=\nMainPID=0\n").unwrap(), Observation::Undeployed);
        assert!(parse_observation("LoadState=not-found\nActiveState=inactive\nSubState=dead\nInvocationID=unexpected\nMainPID=0\n").is_err());
        assert_eq!(
            parse_observation("LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0")
                .unwrap(),
            Observation::Stopped
        );
    }

    #[tokio::test]
    async fn successful_command_with_missing_gate_never_commits_active() {
        let (_dir, ctx, host, fake) = fixture().await;
        fake.with(|f| {
            f.selected.remove("gate-r000");
        });
        let result = ctx
            .bus
            .residential()
            .manual(ctx.clone(), Action::Restart)
            .await;
        assert!(result.is_err());
        assert_eq!(read_record(&ctx).unwrap().unwrap().phase, "repair_required");
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
        assert_eq!(
            host.ops()
                .iter()
                .filter(|o| o.as_str() == "systemd:restart:hysteria-residential")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn old_active_same_instance_is_revalidated_without_restart() {
        let (_dir, ctx, host, fake) = fixture().await;
        let owner = ctx.bus.residential();
        owner.repair(ctx.clone()).await.unwrap();
        fake.with(|f| {
            f.selected.insert("gate-r000".into(), "slot-1-out".into());
        });
        owner.repair(ctx.clone()).await.unwrap();
        assert_eq!(fake.selected()["gate-r000"], "deny");
        assert!(!host
            .ops()
            .iter()
            .any(|o| o == "systemd:restart:hysteria-residential"));
        let receipt = read_record(&ctx).unwrap().unwrap();
        assert_eq!(receipt.phase, "active");
        assert!(receipt.authorization_sha.is_some());
        assert_eq!(receipt.gate_count, 32);
    }

    #[tokio::test]
    async fn changed_state_cannot_adopt_an_old_live_candidate_as_active() {
        let (_dir, ctx, host, _fake) = fixture().await;
        ctx.store
            .update(|s| s.node.ports.hy2_resi = 45000)
            .await
            .unwrap();
        assert!(ctx.bus.residential().repair(ctx.clone()).await.is_err());
        assert_ne!(read_record(&ctx).unwrap().unwrap().phase, "active");
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
        assert!(!host
            .ops()
            .iter()
            .any(|op| op == "systemd:restart:hysteria-residential"));
    }

    #[tokio::test]
    async fn cancellation_keeps_the_actual_operation_inside_the_owner() {
        let (_dir, ctx, _host, _fake) = fixture().await;
        let owner = ctx.bus.residential();
        let (entered, seen) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let first_owner = owner.clone();
        let first = tokio::spawn(async move {
            first_owner
                .execute(move |_tx| async move {
                    let _ = entered.send(());
                    wait.await?;
                    Ok(())
                })
                .await
        });
        seen.await.unwrap();
        first.abort();
        let second_owner = owner.clone();
        let (second_entered, mut second_seen) = oneshot::channel();
        let second = tokio::spawn(async move {
            second_owner
                .execute(move |_tx| async move {
                    let _ = second_entered.send(());
                    Ok(())
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            second_seen.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        second.await.unwrap().unwrap();
        owner.drain().await;
    }

    #[tokio::test]
    async fn offline_lease_survives_waiter_cancellation_until_publication_finishes() {
        let (_dir, ctx, _host, _fake) = fixture().await;
        let paths = ctx.paths.clone();
        let (entered, seen) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let operation_paths = paths.clone();
        let task = tokio::spawn(async move {
            offline(&operation_paths, async move {
                let _ = entered.send(());
                wait.await?;
                ctx.store
                    .update(|s| s.node.name = "published".into())
                    .await?;
                Ok(())
            })
            .await
        });
        seen.await.unwrap();
        task.abort();
        assert!(ControlLease::acquire(&paths).is_err());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(lease) = ControlLease::acquire(&paths) {
                    drop(lease);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned offline writer did not finish");
        assert_eq!(
            Store::open(crate::paths::state_file(&paths))
                .await
                .unwrap()
                .read()
                .await
                .node
                .name,
            "published"
        );
    }

    #[tokio::test]
    async fn shutdown_drain_rejects_new_mutations() {
        let (_dir, ctx, _host, _fake) = fixture().await;
        let owner = ctx.bus.residential();
        let (entered, seen) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let first_owner = owner.clone();
        let first = tokio::spawn(async move {
            first_owner
                .execute(move |_tx| async move {
                    entered.send(()).unwrap();
                    wait.await?;
                    Ok(())
                })
                .await
        });
        seen.await.unwrap();
        let draining = owner.clone();
        let drain = tokio::spawn(async move { draining.drain().await });
        loop {
            if owner.jobs.lock().unwrap().closing {
                break;
            }
            tokio::task::yield_now().await;
        }
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        assert!(owner
            .execute(move |_tx| async move {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .await
            .is_err());
        assert!(
            !drain.is_finished(),
            "drain must wait for the already registered operation"
        );
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "rejected admission must never spawn its mutation"
        );
        release.send(()).unwrap();
        first.await.unwrap().unwrap();
        drain.await.unwrap();
        assert!(owner.execute(|_tx| async { Ok(()) }).await.is_err());
    }

    #[tokio::test]
    async fn candidate_validation_failure_preserves_live_binary_and_configuration() {
        let (_dir, ctx, host, _fake) = fixture().await;
        let live = ctx.paths.bin_dir.join("sing-box");
        let candidate = ctx.paths.bin_dir.join("candidate-sing-box");
        host.write_file(&live, b"old", 0o755).unwrap();
        host.write_file(&candidate, b"candidate", 0o755).unwrap();
        host.with(|i| {
            i.scripted.push((
                format!("{} check", candidate.display()),
                crate::sys::CmdOut {
                    status: 1,
                    stdout: String::new(),
                    stderr: "bad candidate".into(),
                },
            ))
        });
        let config_path = crate::modules::core_files::hy2_resi_config_path(&ctx.paths);
        let original = host.read_file(&config_path).unwrap();
        let c = ctx.clone();
        let result = ctx
            .bus
            .residential()
            .execute(move |tx| async move {
                tx.publish(
                    &c,
                    Deferred {
                        changes: vec![Change::SetUnitState {
                            unit: UNIT.into(),
                            enabled: true,
                            active: true,
                        }],
                        keys: BTreeMap::new(),
                    },
                    Some(Candidate {
                        path: candidate,
                        sha256: crate::kernels::sha256_hex(b"candidate"),
                    }),
                )
                .await
            })
            .await;
        assert!(result.is_err());
        assert_eq!(read_record(&ctx).unwrap().unwrap().phase, "failed");
        assert_eq!(host.read_file(&live).unwrap(), Some(b"old".to_vec()));
        assert_eq!(host.read_file(&config_path).unwrap(), original);
        assert!(!host.ops().iter().any(|op| op.starts_with("systemd:")));
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
    }

    #[tokio::test]
    async fn unknown_listener_cannot_produce_an_active_receipt() {
        let (_dir, ctx, host, _fake) = fixture().await;
        host.with(|i| {
            i.fail_listening.insert(Proto::Udp);
        });
        assert!(ctx
            .bus
            .residential()
            .manual(ctx.clone(), Action::Restart)
            .await
            .is_err());
        assert_ne!(read_record(&ctx).unwrap().unwrap().phase, "active");
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
    }

    #[tokio::test]
    async fn empty_or_wrong_udp_port_cannot_produce_active_receipt() {
        for ports in [vec![], vec![40001]] {
            let (_dir, ctx, host, _fake) = fixture().await;
            host.with(|i| {
                i.listening.insert(Proto::Udp, ports.into_iter().collect());
            });
            assert!(ctx.bus.residential().repair(ctx.clone()).await.is_err());
            assert_eq!(read_record(&ctx).unwrap().unwrap().phase, "repair_required");
            assert!(
                !host
                    .ops()
                    .iter()
                    .any(|op| op.starts_with("systemd:restart:")),
                "missing listener alone must not trigger recovery restart"
            );
        }
    }

    #[tokio::test]
    async fn instance_change_during_readback_prevents_active_acknowledgement() {
        let (_dir, ctx, host, fake) = fixture().await;
        let changed = host.clone();
        fake.with(|f| {
            f.on_inventory_read = Some(Arc::new(move |read| {
                if read == 2 {
                    changed.with(|i| {
                        i.instances
                            .insert("hysteria-residential.service".into(), 99);
                    });
                }
            }))
        });
        assert!(ctx
            .bus
            .residential()
            .manual(ctx.clone(), Action::Restart)
            .await
            .is_err());
        assert_eq!(read_record(&ctx).unwrap().unwrap().phase, "repair_required");
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
    }

    #[tokio::test]
    async fn active_record_publication_failure_never_acknowledges_keys() {
        let (_dir, ctx, _host, fake) = fixture().await;
        let record = directory(&ctx).join("record.json");
        fake.with(|f| {
            f.on_inventory_read = Some(Arc::new(move |read| {
                if read == 2 {
                    std::fs::remove_file(&record).unwrap();
                    std::fs::create_dir(&record).unwrap();
                }
            }))
        });
        assert!(ctx
            .bus
            .residential()
            .manual(ctx.clone(), Action::Restart)
            .await
            .is_err());
        assert!(
            read_record(&ctx).is_err(),
            "unwritable journal must not look active"
        );
        assert!(ctx.runtime.read().await.restart_keys.is_empty());
    }

    #[tokio::test]
    async fn busy_machine_lease_blocks_broken_uds_fallback_and_live_import() {
        let (_dir, ctx, host, _fake) = fixture().await;
        let before = std::fs::read(crate::paths::state_file(&ctx.paths)).unwrap();
        let _lease = ControlLease::acquire(&ctx.paths).unwrap();
        let err = crate::serve::reconcile_cli(
            ctx.paths.clone(),
            host.clone(),
            ctx.paths.base_dir.join("missing.sock"),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("owner unavailable"));
        let err = crate::commands::import_v3::run(
            ctx.paths.base_dir.join("missing-v3"),
            None,
            ctx.paths.clone(),
            host,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("owner unavailable"),
            "lease must precede source/state reads"
        );
        assert_eq!(
            std::fs::read(crate::paths::state_file(&ctx.paths)).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn partial_shared_binary_publication_restores_old_files_and_both_consumers() {
        for relay_fails in [false, true] {
            let (_dir, ctx, host, _fake) = fixture().await;
            let binary = ctx.paths.bin_dir.join("sing-box");
            let candidate = ctx.paths.bin_dir.join("candidate-sing-box");
            let first = ctx.paths.base_dir.join("associated.json");
            let unit = ctx.paths.base_dir.join("residential-unit");
            host.write_file(&binary, b"old binary", 0o755).unwrap();
            host.write_file(&candidate, b"new binary", 0o755).unwrap();
            host.write_file(&first, b"old file", 0o600).unwrap();
            host.write_file(&unit, b"old unit", 0o644).unwrap();
            host.with(|i| {
                i.fail_writes.insert(unit.clone());
                if relay_fails {
                    i.fail_units.insert("b-ui-relay".into());
                }
            });
            let work = Deferred {
                changes: vec![
                    Change::WriteFile {
                        path: first.clone(),
                        content: b"new file".to_vec(),
                        mode: 0o600,
                        verify: None,
                        restart: Some(crate::reconcile::Unit::restart(UNIT)),
                    },
                    Change::WriteUnit {
                        path: unit.clone(),
                        content: "new unit".into(),
                        unit: crate::reconcile::Unit::restart(UNIT),
                    },
                ],
                keys: BTreeMap::from([("candidate".into(), "new".into())]),
            };
            let c = ctx.clone();
            let result = ctx
                .bus
                .residential()
                .execute(move |tx| async move {
                    tx.publish(
                        &c,
                        work,
                        Some(Candidate {
                            path: candidate,
                            sha256: crate::kernels::sha256_hex(b"new binary"),
                        }),
                    )
                    .await
                })
                .await;
            assert!(result.is_err());
            assert_eq!(
                host.read_file(&binary).unwrap(),
                Some(b"old binary".to_vec())
            );
            assert_eq!(host.read_file(&first).unwrap(), Some(b"old file".to_vec()));
            assert_eq!(host.read_file(&unit).unwrap(), Some(b"old unit".to_vec()));
            assert!(ctx.runtime.read().await.restart_keys.is_empty());
            let record = read_record(&ctx).unwrap().unwrap();
            if relay_fails {
                assert_eq!(record.relay_recovery_confirmed, Some(false));
                assert_eq!(record.phase, "recovery_required");
                assert!(record.error.is_some());
                continue;
            }
            assert_eq!(record.relay_recovery_confirmed, Some(true));
            assert!(record.error.unwrap().contains("candidate rejected"));
            assert_eq!(
                host.ops()
                    .iter()
                    .filter(|o| o.as_str() == "systemd:restart:b-ui-relay")
                    .count(),
                1
            );
            assert_eq!(
                host.ops()
                    .iter()
                    .filter(|o| o.as_str() == "systemd:restart:hysteria-residential")
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn maintenance_stop_prevents_watchdog_and_repair_starting_it() {
        let (_dir, ctx, host, _fake) = fixture().await;
        let owner = ctx.bus.residential();
        owner.manual(ctx.clone(), Action::Stop).await.unwrap();
        assert!(maintenance(&ctx).unwrap());
        assert!(!owner
            .watchdog(ctx.clone(), Proto::Udp, 40000)
            .await
            .unwrap());
        assert!(owner.repair(ctx.clone()).await.unwrap().is_none());
        assert!(!host
            .ops()
            .iter()
            .any(|o| o == "systemd:restart:hysteria-residential"
                || o == "systemd:start:hysteria-residential"));
        owner.manual(ctx.clone(), Action::Start).await.unwrap();
        assert!(!maintenance(&ctx).unwrap());
    }
}
