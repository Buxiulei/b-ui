//! 证书同步：Caddy 签发的证书 → `<base>/certs/{fullchain,privkey}.pem`，变了就更新两个
//! hysteria（spec §3.4）。
//!
//! 移植 `server/core.sh:923-1040`（`setup_cert_sync` 写出的 `cert-sync.sh` + timer + cron 三重触发）。
//! v4 的变化（审计 §3.1「证书同步三重触发 → 合并」）：既不写 `cert-sync.sh`、也不建 timer/cron，
//! 改成守护进程里的 inotify 任务 + 兜底轮询任务；比对从 `cmp` 改成 sha256 指纹记在
//! `runtime.cert_sha256`。
//!
//! 两个实例读证书的方式不同（T3 §12 第 9 项，2026-09-18 实测）：
//! - `hysteria-server` 是 apernet hysteria，只在启动时读证书（`CanReload=no`），换证必须重启；
//! - `hysteria-residential` 是自建 sing-box（1.14），hysteria2 入站会 watch 证书文件热加载，
//!   写完亚秒级打出 `inbound/hysteria2[hy2-resi]: reloaded TLS certificate`，既有连接不受影响。
//!   所以住宅实例改成「确认热加载、超时才回退重启」（[`confirm_reload`]），不再无条件重启。

use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use crate::sys::{Host, JournalFrom};
use bui_schema::model::State;
use std::path::{Path, PathBuf};

/// 住宅实例写完证书后确认热加载的时限：超过它没在 journald 看到 `reloaded TLS certificate`
/// 就回退重启（spec §3.4 原本是「两个 hysteria 错峰重启」的 10 秒间隔；4.1 住宅改热加载后，
/// 只剩直连一个实例要重启、错峰无意义，这个常量转用作住宅实例的热加载确认时限）。
pub const RESTART_GAP_SECS: u64 = 10;
/// 住宅实例（自建 sing-box）hysteria2 入站热加载证书成功时逐字打出的日志片段。
const RELOAD_MARK: &str = "reloaded TLS certificate";
/// 已经有证书之后的兜底轮询间隔（inotify 漏事件时的保险）。
pub const FALLBACK_POLL_SECS: u64 = 21600; // 6h
/// **还没拿到首张证书**时的轮询间隔：全新装机时 Caddy 的 `certificates/` 目录还不存在，
/// inotify 完全收不到事件，这时必须是秒级而不是 6h（B6）。
pub const PENDING_POLL_SECS: u64 = 30;

pub struct CertsModule;

impl Module for CertsModule {
    fn name(&self) -> &'static str {
        "certs"
    }

    /// 证书不是渲染产物（Caddy 签、我们只复制），所以没有任何期望项。
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    /// 两个任务：inotify 监听 + 兜底轮询。两个循环各自从 `ctx.store` 现读域名，
    /// 所以这个同步函数里不需要 await。
    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        vec![
            tokio::spawn(watch_loop(ctx.clone())),
            tokio::spawn(poll_loop(ctx)),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertPair {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertAction {
    NotReady,
    UpToDate,
    Copy { sha256: String },
}

/// 在 Caddy 数据目录里找 `<domain>` 的证书对（移植 `core.sh:952-963` 的两段查找：
/// 先试 Let's Encrypt 的 issuer 目录，再退回 `certificates/<domain>`）。
pub fn find_cert(host: &dyn Host, caddy_data: &Path, domain: &str) -> Option<CertPair> {
    let certs = caddy_data.join("certificates");
    let candidates = [
        certs
            .join("acme-v02.api.letsencrypt.org-directory")
            .join(domain),
        certs.join(domain),
    ];
    for dir in candidates {
        let cert = dir.join(format!("{domain}.crt"));
        let key = dir.join(format!("{domain}.key"));
        let have = |p: &Path| host.read_file(p).ok().flatten().is_some();
        if have(&cert) && have(&key) {
            return Some(CertPair { cert, key });
        }
    }
    None
}

/// 与 runtime 里记的指纹比较，决定要不要复制。
pub fn decide(host: &dyn Host, pair: &CertPair, known: Option<&str>) -> anyhow::Result<CertAction> {
    let Some(bytes) = host.read_file(&pair.cert)? else {
        return Ok(CertAction::NotReady);
    };
    let sha256 = crate::kernels::sha256_hex(&bytes);
    Ok(if known == Some(sha256.as_str()) {
        CertAction::UpToDate
    } else {
        CertAction::Copy { sha256 }
    })
}

/// `fullchain.pem` 0644 / `privkey.pem` 0600（移植 `core.sh:983-986`）。
pub fn copy_pair(host: &dyn Host, pair: &CertPair, certs_dir: &Path) -> anyhow::Result<()> {
    let cert = host
        .read_file(&pair.cert)?
        .ok_or_else(|| anyhow::anyhow!("证书消失了"))?;
    let key = host
        .read_file(&pair.key)?
        .ok_or_else(|| anyhow::anyhow!("私钥消失了"))?;
    host.write_file(&certs_dir.join("fullchain.pem"), &cert, 0o644)?;
    host.write_file(&certs_dir.join("privkey.pem"), &key, 0o600)?;
    Ok(())
}

/// 同步一轮：找证书 → 判断 → 复制 → 更新两个 hysteria（直连重启、住宅确认热加载）。
/// 返回新指纹（无变化则 `None`）。
pub async fn sync_once(ctx: &DaemonCtx, domain: &str) -> anyhow::Result<Option<String>> {
    let known = ctx.runtime.read().await.cert_sha256;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let domain_owned = domain.to_string();
    // 读住宅实例 journald 的起点必须在写证书之前（热加载行随写入而生）；抓在复制动作前一刻。
    let since = ctx.host.now();
    let action = tokio::task::spawn_blocking(move || -> anyhow::Result<CertAction> {
        let Some(pair) = find_cert(
            host.as_ref(),
            &crate::paths::caddy_data(&paths),
            &domain_owned,
        ) else {
            return Ok(CertAction::NotReady);
        };
        let action = decide(host.as_ref(), &pair, known.as_deref())?;
        if let CertAction::Copy { .. } = &action {
            copy_pair(host.as_ref(), &pair, &paths.certs_dir)?;
        }
        Ok(action)
    })
    .await??;
    let CertAction::Copy { sha256 } = action else {
        return Ok(None);
    };
    ctx.runtime
        .update(|r| r.cert_sha256 = Some(sha256.clone()))
        .await;
    // 直连 hysteria（apernet）只在启动时读证书（CanReload=no），换证必须重启。
    // 没在跑的实例交给 systemd，证书同步不负责拉起（移植 core.sh:1000-1006）。
    if unit_active(ctx, "hysteria-server").await? && restart(ctx, "hysteria-server").await? {
        tracing::info!(unit = "hysteria-server", "证书更新后已重启");
    }
    // 住宅实例（自建 sing-box）会 watch 证书文件热加载：先确认，超时才回退重启（fail-safe，
    // 行为不比 4.0 差）。没在跑就同样交给 systemd（下次启动读新证书），既不读 journald 也不重启。
    if unit_active(ctx, "hysteria-residential").await? {
        if confirm_reload(ctx, "hysteria-residential", since).await {
            tracing::info!(unit = "hysteria-residential", "证书已热加载，无需重启");
        } else if restart(ctx, "hysteria-residential").await? {
            tracing::warn!(
                unit = "hysteria-residential",
                secs = RESTART_GAP_SECS,
                "证书热加载未在时限内确认，已回退重启"
            );
        }
    }
    Ok(Some(sha256))
}

/// 单元是否在跑（`ctx.host` 的阻塞调用挪到 blocking 线程）。
async fn unit_active(ctx: &DaemonCtx, unit: &'static str) -> anyhow::Result<bool> {
    let host = ctx.host.clone();
    Ok(tokio::task::spawn_blocking(move || host.unit_is_active(unit).unwrap_or(false)).await?)
}

/// 重启单元，返回是否成功（调用方已确认它在跑）。
async fn restart(ctx: &DaemonCtx, unit: &'static str) -> anyhow::Result<bool> {
    let host = ctx.host.clone();
    Ok(tokio::task::spawn_blocking(move || {
        host.systemd("restart", unit)
            .map(|o| o.ok())
            .unwrap_or(false)
    })
    .await?)
}

/// 写完证书后确认住宅实例（自建 sing-box）已热加载：在 [`RESTART_GAP_SECS`] 秒内反复读它的
/// journald（复用哨兵那条 [`Host::journal_read`]，从写证书前一刻 [`JournalFrom::Since`] 读起、
/// 只看新行），看到 [`RELOAD_MARK`] 即确认返回 `true`。
///
/// `copy_pair` 先写 fullchain 再写 privkey（两次 rename），中间态必然先出一条
/// `reload key pair: tls: private key does not match public key` ERROR 再成功，所以只认最终
/// 那行 `reloaded TLS certificate`；超时没看到返回 `false`，由调用方回退重启。
async fn confirm_reload(ctx: &DaemonCtx, unit: &'static str, since: time::OffsetDateTime) -> bool {
    let from = JournalFrom::Since(since);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(RESTART_GAP_SECS);
    loop {
        let host = ctx.host.clone();
        let units = vec![unit.to_string()];
        let from2 = from.clone();
        if let Ok(Ok(recs)) =
            tokio::task::spawn_blocking(move || host.journal_read(&units, &from2)).await
        {
            if recs.iter().any(|r| r.message.contains(RELOAD_MARK)) {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// 本轮兜底轮询该等多久：没证书 30s，有证书回到 6h。
pub fn poll_interval(cert_ready: bool) -> std::time::Duration {
    std::time::Duration::from_secs(if cert_ready {
        FALLBACK_POLL_SECS
    } else {
        PENDING_POLL_SECS
    })
}

/// 兜底轮询：没证书时 30 秒一轮（全新装机的首张证书全靠它），拿到后 6 小时一轮。
/// 与 inotify 无关——inotify 在目录还不存在或子目录后建时都可能一个事件都收不到。
pub async fn poll_loop(ctx: DaemonCtx) {
    loop {
        let ready = ctx.runtime.read().await.cert_sha256.is_some();
        tokio::time::sleep(poll_interval(ready)).await;
        let domain = ctx.store.read().await.node.domain.clone();
        if let Err(e) = sync_once(&ctx, &domain).await {
            tracing::warn!(error = %e, "兜底轮询的证书同步失败");
        }
    }
}

pub async fn watch_loop(ctx: DaemonCtx) {
    let domain = ctx.store.read().await.node.domain.clone();
    if let Err(e) = sync_once(&ctx, &domain).await {
        tracing::warn!(error = %e, "启动时证书同步失败");
    }
    let dir = crate::paths::caddy_data(&ctx.paths).join("certificates");
    // 全新装机时这个目录还不存在（Caddy 签到证书才建）；不先建出来，watches.add 会 ENOENT，
    // 整个监听退化成兜底轮询（B6）。目录在我们自己的 XDG 数据目录里，先建无害。
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "创建证书目录失败");
    }
    let mut watcher = match CertWatcher::open(&dir) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "inotify 建立失败，只靠兜底轮询");
            None
        }
    };
    loop {
        match watcher.as_mut() {
            Some(w) => {
                use futures_util::StreamExt;
                if w.stream.next().await.is_none() {
                    tracing::warn!("inotify 流结束，只靠兜底轮询");
                    watcher = None;
                    continue;
                }
                // Caddy 事后才建 issuer/domain 子目录，新目录要补进 watch，否则里面的写入无事件
                w.rescan();
            }
            // inotify 用不了：这条任务直接退出，证书同步交给 poll_loop（30s / 6h）
            None => return,
        }
        // Caddy 写证书是多文件操作，等 2 秒收敛再同步
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let domain = ctx.store.read().await.node.domain.clone();
        if let Err(e) = sync_once(&ctx, &domain).await {
            tracing::warn!(error = %e, "证书同步失败");
        }
    }
}

/// 持有 inotify 句柄与 watch 集合，`rescan` 把新出现的子目录补进去。
struct CertWatcher {
    root: PathBuf,
    watches: inotify::Watches,
    stream: inotify::EventStream<[u8; 1024]>,
    watched: std::collections::BTreeSet<PathBuf>,
}

impl CertWatcher {
    fn open(dir: &Path) -> anyhow::Result<Self> {
        let inotify = inotify::Inotify::init()?;
        let watches = inotify.watches();
        let mut me = Self {
            root: dir.to_path_buf(),
            watches,
            stream: inotify.into_event_stream([0u8; 1024])?,
            watched: std::collections::BTreeSet::new(),
        };
        let root = me.root.clone();
        me.add(&root)?;
        me.rescan();
        Ok(me)
    }

    fn mask() -> inotify::WatchMask {
        inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO | inotify::WatchMask::CREATE
    }

    fn add(&mut self, dir: &Path) -> anyhow::Result<()> {
        self.watches.add(dir, Self::mask())?;
        self.watched.insert(dir.to_path_buf());
        Ok(())
    }

    fn rescan(&mut self) {
        for d in walk_dirs(&self.root) {
            if !self.watched.contains(&d) {
                if let Err(e) = self.add(&d) {
                    tracing::warn!(error = %e, dir = %d.display(), "补加 watch 失败");
                }
            }
        }
    }
}

/// `root` 下深度 2 的子目录（足够覆盖 `certificates/<issuer>/<domain>`）。
/// 这里直接用 `std::fs`：inotify 本身就只能对真实目录工作，Host 抽象在这条路径上没有意义。
fn walk_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(level1) = std::fs::read_dir(root) else {
        return out;
    };
    for e in level1.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        if let Ok(level2) = std::fs::read_dir(e.path()) {
            for e2 in level2.flatten() {
                if e2.path().is_dir() {
                    out.push(e2.path());
                }
            }
        }
        out.push(e.path());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::reconcile::{DaemonCtx, Module, RenderCtx};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    const ACME: &str =
        "/opt/b-ui/caddy/caddy/certificates/acme-v02.api.letsencrypt.org-directory/example.com";

    fn seed(h: &FakeHost, cert: &str, key: &str) {
        h.with(|i| {
            i.files.insert(
                format!("{ACME}/example.com.crt").into(),
                (cert.as_bytes().to_vec(), 0o600),
            );
            i.files.insert(
                format!("{ACME}/example.com.key").into(),
                (key.as_bytes().to_vec(), 0o600),
            );
        });
    }

    #[test]
    fn finds_the_cert_pair_under_the_acme_directory() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let pair = find_cert(
            &h,
            &crate::paths::caddy_data(&Paths::default_server()),
            "example.com",
        )
        .unwrap();
        assert_eq!(
            pair.cert.to_str().unwrap(),
            format!("{ACME}/example.com.crt")
        );
        assert_eq!(
            pair.key.to_str().unwrap(),
            format!("{ACME}/example.com.key")
        );
    }

    #[test]
    fn missing_cert_is_not_an_error() {
        let h = FakeHost::new();
        assert_eq!(
            find_cert(
                &h,
                &crate::paths::caddy_data(&Paths::default_server()),
                "example.com"
            ),
            None
        );
    }

    #[test]
    fn decide_compares_against_the_known_fingerprint() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let pair = find_cert(
            &h,
            &crate::paths::caddy_data(&Paths::default_server()),
            "example.com",
        )
        .unwrap();
        let sum = crate::kernels::sha256_hex(b"CERT");
        assert_eq!(
            decide(&h, &pair, None).unwrap(),
            CertAction::Copy {
                sha256: sum.clone()
            }
        );
        assert_eq!(decide(&h, &pair, Some(&sum)).unwrap(), CertAction::UpToDate);
        assert_eq!(
            decide(&h, &pair, Some("stale")).unwrap(),
            CertAction::Copy { sha256: sum }
        );
    }

    #[test]
    fn copy_pair_writes_644_and_600() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let paths = Paths::default_server();
        let pair = find_cert(&h, &crate::paths::caddy_data(&paths), "example.com").unwrap();
        copy_pair(&h, &pair, &paths.certs_dir).unwrap();
        assert_eq!(h.text("/opt/b-ui/certs/fullchain.pem").unwrap(), "CERT");
        assert_eq!(h.text("/opt/b-ui/certs/privkey.pem").unwrap(), "KEY");
        assert_eq!(h.mode("/opt/b-ui/certs/fullchain.pem"), Some(0o644));
        assert_eq!(h.mode("/opt/b-ui/certs/privkey.pem"), Some(0o600));
    }

    async fn ctx_with(host: Arc<FakeHost>) -> (DaemonCtx, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::default_server();
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state())
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        (
            DaemonCtx {
                store,
                runtime,
                bus: EventBus::new(),
                host,
                paths,
            },
            d,
        )
    }

    /// 住宅实例热加载证书成功那行（前面那条 key-mismatch ERROR 略去，confirm_reload 只认它）。
    fn reload_rec() -> crate::sys::JournalRecord {
        crate::sys::JournalRecord {
            cursor: "c-resi-0".into(),
            unit: "hysteria-residential".into(),
            ts: time::macros::datetime!(2026-09-11 00:00:01 UTC),
            message: "inbound/hysteria2[hy2-resi]: reloaded TLS certificate".into(),
        }
    }

    /// 热加载行出现 → 只重启直连、不重启住宅（T3 §12 第 9 项）。
    #[tokio::test(start_paused = true)]
    async fn residential_hot_reload_is_confirmed_and_skips_the_restart() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
            i.journal.push_back(Ok(vec![reload_rec()]));
        });
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        let sum = sync_once(&ctx, "example.com").await.unwrap().unwrap();
        assert_eq!(sum, crate::kernels::sha256_hex(b"CERT"));
        let ops = host.ops();
        assert!(
            ops.iter().any(|o| o == "systemd:restart:hysteria-server"),
            "直连 hysteria 始终重启：{ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|o| o == "systemd:restart:hysteria-residential"),
            "住宅实例热加载已确认，绝不重启：{ops:?}"
        );
        assert_eq!(
            ctx.runtime.read().await.cert_sha256.as_deref(),
            Some(sum.as_str())
        );
    }

    /// journald 里等不到 reloaded 行 → 回退重启住宅（fail-safe），且必然在确认时限之后。
    #[tokio::test(start_paused = true)]
    async fn residential_falls_back_to_restart_when_reload_is_not_seen() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
            // journal 队列不塞 reloaded 行：每轮读回 Ok(vec![])，确认必然超时
        });
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        let start = tokio::time::Instant::now();
        sync_once(&ctx, "example.com").await.unwrap().unwrap();
        assert!(
            tokio::time::Instant::now().duration_since(start).as_secs() >= RESTART_GAP_SECS,
            "回退重启必须等满确认时限"
        );
        let ops = host.ops();
        let i1 = ops
            .iter()
            .position(|o| o == "systemd:restart:hysteria-server")
            .unwrap();
        let i2 = ops
            .iter()
            .position(|o| o == "systemd:restart:hysteria-residential")
            .expect("热加载超时必须回退重启住宅");
        assert!(i1 < i2, "先直连、住宅在确认超时后");
    }

    /// 无论住宅实例是热加载还是回退重启，直连 hysteria 都必须重启（apernet 只在启动时读证书）。
    #[tokio::test(start_paused = true)]
    async fn the_direct_hysteria_is_always_restarted() {
        for journal in [Some(vec![reload_rec()]), None] {
            let host = Arc::new(FakeHost::new());
            host.with(|i| {
                i.units_active.insert("hysteria-server.service".into());
                i.units_active.insert("hysteria-residential.service".into());
                if let Some(recs) = journal.clone() {
                    i.journal.push_back(Ok(recs));
                }
            });
            seed(&host, "CERT", "KEY");
            let (ctx, _d) = ctx_with(host.clone()).await;
            sync_once(&ctx, "example.com").await.unwrap().unwrap();
            assert!(
                host.ops()
                    .iter()
                    .any(|o| o == "systemd:restart:hysteria-server"),
                "直连 hysteria 始终重启（journal={journal:?}）"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn second_sync_is_a_no_op() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
        });
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        sync_once(&ctx, "example.com").await.unwrap();
        host.clear_ops();
        assert_eq!(sync_once(&ctx, "example.com").await.unwrap(), None);
        assert!(host.ops().is_empty(), "证书没变不重启");
    }

    #[tokio::test(start_paused = true)]
    async fn inactive_instances_are_not_started_by_the_cert_sync() {
        let host = Arc::new(FakeHost::new()); // 两个单元都不 active
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        sync_once(&ctx, "example.com").await.unwrap().unwrap();
        assert!(
            !host.ops().iter().any(|o| o.starts_with("systemd:restart")),
            "没在跑的实例交给 systemd，不由证书同步拉起（移植 core.sh:1000-1006）"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_certificate_is_picked_up_in_seconds_not_six_hours() {
        // B6：全新装机时 certificates/ 目录都还不存在，inotify 收不到任何事件；
        // 兜底轮询必须是 30 秒级，否则两个 hysteria 因缺 fullchain.pem 崩溃循环最长 6 小时。
        assert_eq!(
            poll_interval(false),
            std::time::Duration::from_secs(PENDING_POLL_SECS)
        );
        assert_eq!(
            poll_interval(true),
            std::time::Duration::from_secs(FALLBACK_POLL_SECS)
        );
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
        });
        let (ctx, _d) = ctx_with(host.clone()).await;
        let task = tokio::spawn(poll_loop(ctx.clone())); // sample_state 的域名就是 example.com
        tokio::time::sleep(std::time::Duration::from_secs(PENDING_POLL_SECS + 5)).await;
        assert!(
            ctx.runtime.read().await.cert_sha256.is_none(),
            "证书还没签出来，这一轮什么都不做"
        );
        seed(&host, "CERT", "KEY"); // Caddy 这时才把证书写出来
        tokio::time::sleep(std::time::Duration::from_secs(
            PENDING_POLL_SECS + RESTART_GAP_SECS + 5,
        ))
        .await;
        assert_eq!(
            ctx.runtime.read().await.cert_sha256.as_deref(),
            Some(crate::kernels::sha256_hex(b"CERT").as_str()),
            "下一轮 30 秒轮询就该复制并记指纹"
        );
        assert!(host
            .ops()
            .iter()
            .any(|o| o == "write:/opt/b-ui/certs/fullchain.pem:644"));
        task.abort();
    }

    /// 证书文件不是渲染产物（Caddy 签、我们只复制），所以 render 必须为空；
    /// 本文件自带一个 RenderCtx 构造器，不跨模块借用别的任务的测试辅助。
    #[test]
    fn module_renders_nothing() {
        let ctx = RenderCtx {
            paths: Paths::default_server(),
            facts: crate::reconcile::Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 0,
                systemd_resolved: false,
                nft_tables: Default::default(),
            },
        };
        assert_eq!(
            CertsModule.render(&crate::testutil::sample_state(), &ctx),
            vec![]
        );
    }
}
