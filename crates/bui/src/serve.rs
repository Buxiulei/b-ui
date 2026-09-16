//! 守护进程装配：模块注册、一次完整对账、对账触发器（启动 / 500ms 去抖 / 10 分钟巡检 /
//! 每日带抖动自检）、`bui reconcile` 的 CLI 路径。
//!
//! 触发点（spec §2.2、§7）：启动时立即一次；`EventBus` 上任何 `StateChanged` /
//! `ReconcileRequested` 经 500ms 去抖；每 10 分钟一次漂移检查；每日一次带抖动的 manifest 自检；
//! `bui reconcile` 手动。守护进程里**只有一个** consumer 会调 [`reconcile_from_ctx`]，
//! 所有触发都经同一条 mpsc 排队，天然互斥。

use crate::api::{AppState, Event, EventBus};
use crate::kernels::{Fetcher, HttpFetcher, KernelInstaller, Manifest};
use crate::modules::certs::CertsModule;
use crate::modules::core_files::CoreFilesModule;
use crate::modules::ssh::SshModule;
use crate::modules::system::SystemModule;
use crate::modules::units::UnitsModule;
use crate::modules::watchdog::WatchdogModule;
use crate::reconcile::apply::{apply, ApplyInput, BinaryInstaller};
use crate::reconcile::diff::{plan, PlanInput};
use crate::reconcile::{DaemonCtx, Facts, Module, RenderCtx};
use crate::state::runtime::{ReconcileReport, Runtime};
use crate::state::store::Store;
use crate::sys::Host;
use bui_schema::model::State;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 事件到对账之间的去抖窗口：一串变更压成一次对账。
pub const DEBOUNCE_MS: u64 = 500;
/// 漂移巡检间隔（10 分钟往去抖队列投一次触发）。
pub const DRIFT_INTERVAL_SECS: u64 = 600;
/// 每日自检间隔（spec §7「守护进程每日带抖动自检」）
pub const SELFCHECK_INTERVAL_SECS: u64 = 86400;

/// P1 注册的六个模块 + 它们共享的 manifest 句柄；P2/P3 在 [`modules`] 里各追加自己的 Module
pub struct Registry {
    pub modules: Vec<Arc<dyn Module>>,
    pub manifest: Arc<RwLock<Option<Manifest>>>,
    /// 对账末尾收敛 Xray 槽路由要用它的 `XrayApi`（D7）
    pub panel: Arc<crate::modules::panel::Shared>,
}

pub fn modules(manifest: Option<Manifest>) -> Registry {
    let core = CoreFilesModule::new(manifest);
    let handle = core.manifest_handle();
    let panel = crate::modules::panel::PanelModule::new();
    let shared = panel.shared();
    Registry {
        modules: vec![
            Arc::new(core),
            Arc::new(UnitsModule),
            Arc::new(SystemModule),
            Arc::new(SshModule),
            Arc::new(CertsModule),
            Arc::new(WatchdogModule),
            // P2：面板 API + 采样 + auth 快照 + Xray gRPC
            Arc::new(panel),
            Arc::new(crate::modules::residential::ResidentialModule::new()),
            // spec §5.7：日志哨兵（xray gRPC 预案要借面板的 Shared 重试用户同步）
            Arc::new(crate::modules::sentinel::SentinelModule::new(
                shared.clone(),
            )),
        ],
        manifest: handle,
        panel: shared,
    }
}

pub struct ReconcileInput<'a> {
    pub state: &'a State,
    pub modules: &'a [Arc<dyn Module>],
    pub paths: &'a Paths,
    pub keys: &'a BTreeMap<String, String>,
    pub installer: &'a dyn BinaryInstaller,
    pub force: bool,
    pub dry_run: bool,
}

/// 一次完整对账：探事实 → 收集 artifacts → diff → apply → 漂移扫描 → 报告
pub fn reconcile_once(
    input: ReconcileInput<'_>,
    host: &dyn Host,
) -> anyhow::Result<(ReconcileReport, BTreeMap<String, String>)> {
    let facts = Facts::probe(host)?;
    let ctx = RenderCtx {
        paths: input.paths.clone(),
        facts,
    };
    let mut artifacts = Vec::new();
    for m in input.modules {
        artifacts.extend(m.render(input.state, &ctx));
    }
    let installed = crate::kernels::installed_versions(host, &input.paths.bin_dir);
    let p = plan(
        PlanInput {
            artifacts: &artifacts,
            paths: input.paths,
            keys: input.keys,
            installed_versions: &installed,
            facts: &ctx.facts,
        },
        host,
    )?;
    let mut keys = input.keys.clone();
    let out = apply(
        ApplyInput {
            plan: p,
            paths: input.paths,
            facts: &ctx.facts,
            installer: input.installer,
            dry_run: input.dry_run,
        },
        host,
    );
    keys.extend(out.keys.clone());
    let drift = crate::reconcile::drift::scan(host, &artifacts, input.paths);
    let mut notes = out.notes.clone();
    if input.force && !drift.is_empty() && !input.dry_run {
        notes.extend(crate::reconcile::drift::clean(host, &drift));
    }
    if input.state.system.ssh_hardening && ctx.facts.ssh_pubkeys == 0 {
        notes.push(
            "未在 /root/.ssh/authorized_keys 检测到公钥，已跳过 SSH 硬化；加好公钥后运行 `b-ui harden-ssh`"
                .into(),
        );
    }
    // spec §2.2「防火墙：没装就在体检里提示」。判据与 `SystemModule::render`、apply 的两支
    // 完全一致（`*_active`）。放在这里而不是产出 `FirewallPorts` artifact：notes 不进 `changed`、
    // 不影响 `/api/health` 的 degraded 判定，于是「每轮提醒」不等于「每轮有改动」——
    // 没有防火墙的机器仍然满足 M1 的「二次对账零变更」（第三轮审查 C1）。
    if input.state.system.firewall != "off" && !ctx.facts.ufw_active && !ctx.facts.firewalld_active
    {
        let specs: Vec<String> = crate::modules::system::firewall_ports(
            &input.state.node.ports,
            input.state.system.hy2_resi_compat_ports,
        )
        .iter()
        .map(|p| p.ufw())
        .collect();
        notes.push(format!(
            "未检测到 ufw/firewalld，请在云厂商安全组放行：{}",
            specs.join(", ")
        ));
    }
    let report = ReconcileReport {
        at: crate::util::fmt_rfc3339(host.now()),
        changed: out.changed,
        restarted: out.restarted,
        notes,
        verify_failures: out.verify_failures,
        errors: out.errors,
        drift: if input.force && !input.dry_run {
            Vec::new()
        } else {
            drift
        },
        dry_run: input.dry_run,
        self_restart_required: out.self_restart_required && !input.dry_run,
    };
    if out.relay_restarted {
        tracing::info!("b-ui-relay 已重启，需要重放选中的住宅上游");
    }
    Ok((report, keys))
}

/// 从 [`DaemonCtx`] 跑一轮并把报告 / 漂移 / 重启键写进 runtime；relay 重启时发 [`Event::RelayRestarted`]。
///
/// `fetcher` 用 `Arc<dyn Fetcher>` 传入并在 `spawn_blocking` 里使用（`reqwest::blocking`
/// 不能在 async 上下文里跑）。**不重启 `b-ui` 自己**（apply 也不会）——调用方拿到报告后调
/// [`finish_self_restart`]。
pub async fn reconcile_from_ctx(
    ctx: &DaemonCtx,
    modules: &[Arc<dyn Module>],
    fetcher: Arc<dyn Fetcher>,
    force: bool,
    dry_run: bool,
) -> anyhow::Result<ReconcileReport> {
    // spec §3.1 第三道防线：这一轮本来就要重写 `hy2-residential.json`（§3.5 四件事之一）
    // ⇒ 落盘前先把空闲凭据的 secret 重随机。放在读期望态之前：它自己也改期望态。
    // `--dry-run` 一个字节都不许写。
    if !dry_run {
        crate::modules::residential::slots::reroll_idle_hy2_secrets(ctx).await;
    }
    let state = ctx.store.read().await;
    let keys = ctx.runtime.read().await.restart_keys;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let mods = modules.to_vec();
    // 整轮对账（含 reqwest::blocking 下载内核）都在阻塞线程里跑：
    // reqwest 文档明确 blocking 客户端在 async runtime 里会 panic。
    let (report, keys) = tokio::task::spawn_blocking(move || {
        let installer = KernelInstaller {
            fetcher: fetcher.as_ref(),
            host: host.as_ref(),
        };
        reconcile_once(
            ReconcileInput {
                state: &state,
                modules: &mods,
                paths: &paths,
                keys: &keys,
                installer: &installer,
                force,
                dry_run,
            },
            host.as_ref(),
        )
    })
    .await??;
    if !dry_run {
        ctx.runtime
            .update(|r| {
                r.restart_keys = keys;
                r.drift = report.drift.clone();
                r.last_reconcile = Some(report.clone());
            })
            .await;
    }
    if report.restarted.iter().any(|u| u == "b-ui-relay") {
        ctx.bus.send(Event::RelayRestarted);
    }
    // spec §3.4：住宅入站重启后每个门回到 default = deny（不开 `cache_file`），
    // 订阅者立刻重放真实门位；不重放的话全体住宅 HY2 用户一直被拒到 60 秒安全网那一轮。
    if hy2_resi_restarted(&report) {
        ctx.bus.send(Event::Hy2ResiRestarted);
    }
    Ok(report)
}

/// 这一轮对账重启过住宅 HY2 入站吗（判据只有这一处）。
///
/// 启动那一轮的结论不能只靠 [`Event::Hy2ResiRestarted`]：`EventBus` 是裸
/// `broadcast::Sender`，`send` 在「还没有订阅者」时静默丢弃，而 `gates::replay_loop`
/// 要到 `Module::spawn` 才订阅 —— 启动对账早于它。所以 [`run`] 拿这个判据在启动路径上
/// 自己补一次重放（见那里的注释）。
pub fn hy2_resi_restarted(report: &ReconcileReport) -> bool {
    report.restarted.iter().any(|u| u == "hysteria-residential")
}

/// 报告与 `restart_keys` 已落盘之后才重启守护进程自己：`in_daemon = true` 用
/// `systemctl restart --no-block b-ui.service`（systemd 排队、本轮先返回；单元有 `Restart=always`），
/// CLI 路径用同步 `systemctl restart b-ui.service`。`report.self_restart_required` 为 false 时什么都不做。
pub async fn finish_self_restart(ctx: &DaemonCtx, report: &ReconcileReport, in_daemon: bool) {
    if !report.self_restart_required || report.dry_run {
        return;
    }
    let host = ctx.host.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if in_daemon {
            // 本进程就是 b-ui：--no-block 让 systemd 排队，本轮的收尾代码先跑完
            host.run("systemctl", &["restart", "--no-block", "b-ui.service"])
                .map(|o| o.ok())
                .unwrap_or(false)
        } else {
            host.systemd("restart", "b-ui")
                .map(|o| o.ok())
                .unwrap_or(false)
        }
    })
    .await;
    tracing::info!(in_daemon, "b-ui.service 自身需要重启，已提交");
}

/// 500ms 去抖：把一串 Event 压成一次触发
pub async fn debounce_loop(bus: EventBus, tx: tokio::sync::mpsc::Sender<bool>) {
    let mut rx = bus.subscribe();
    loop {
        let force = match rx.recv().await {
            Ok(Event::StateChanged(_)) => false,
            Ok(Event::ReconcileRequested { force }) => force,
            // 两条「内核刚重启」的事件都不该触发对账：它们自有重放订阅者
            // （`health::replay_loop` / `gates::replay_loop`），这里白跑一轮只会多一次
            // 全量对账
            Ok(Event::RelayRestarted) | Ok(Event::Hy2ResiRestarted) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
        };
        // 去抖窗口内把后续事件吞掉，force 取或
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(DEBOUNCE_MS);
        let mut force = force;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(Event::ReconcileRequested { force: f })) => force |= f,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        if tx.send(force).await.is_err() {
            return;
        }
    }
}

/// 每日自检：延 [`jitter_secs`] 秒后每 24h 挑一份 manifest
/// （[`crate::kernels::pick_selfcheck_manifest`]）→ 写 `<base>/manifest.json` → 刷新共享
/// manifest 句柄 → 记录可升级版本 → 请求一次对账（内核随 manifest 升级）。
///
/// 只刷内核、**不自动换 bui 自己**：`bui upgrade` 仍由面板 / CLI 手动触发。
///
/// `url_override` = `$BUI_MANIFEST_URL`（守护进程没有命令行开关）。没覆盖就跟 `latest`，
/// 404 时无条件回退到 releases 列表里最新的预发布——与 `bui upgrade` 用的是
/// [`crate::kernels::fetch_manifest_with`] 这同一套解析；两边都拿不到就只 info 一行。
///
/// **绝不自动降级**（事故 2026-09-15，两台 rc1 服务器，复盘在 `kernels` 模块头）：候选必须比
/// 本机当前 rank（manifest 缓存，缺失时取运行中的 bui 版本 + 稳定版）**严格更高**，否则本轮
/// 一个字节都不写 —— 不换缓存、不请求对账、不提示新版。`releases/latest` 不含预发布，所以
/// 预发布机器的候选里多一份 rc 通道的 manifest（[`crate::kernels::pick_selfcheck_manifest`]）。
pub async fn selfcheck_loop(
    ctx: DaemonCtx,
    manifest: Arc<RwLock<Option<Manifest>>>,
    fetcher: Arc<dyn Fetcher>,
    url_override: Option<String>,
) {
    let node_id = ctx.store.read().await.node.id;
    // 按节点 id 定死的抖动，避免所有机器同一秒打 GitHub（spec §7）
    tokio::time::sleep(std::time::Duration::from_secs(jitter_secs(node_id))).await;
    loop {
        // 「本机停在哪一版」以 manifest 缓存为准（对账正照它装内核），每轮重读一次：中途的
        // 显式 `bui upgrade` 写下的那一份也算得上（事故 2026-09-15）
        let (h, p) = (ctx.host.clone(), ctx.paths.clone());
        let cached = tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p))
            .await
            .ok()
            .flatten();
        let f = fetcher.clone();
        let u = url_override.clone();
        match tokio::task::spawn_blocking(move || {
            crate::kernels::pick_selfcheck_manifest(
                f.as_ref(),
                u.as_deref(),
                cached.as_ref(),
                env!("CARGO_PKG_VERSION"),
            )
        })
        .await
        {
            Ok(Ok(Some((_url, m)))) => {
                // 写缓存：下次启动直接用，拉不到网也能装内核
                let path = crate::paths::manifest_file(&ctx.paths);
                match serde_json::to_vec_pretty(&m) {
                    Ok(bytes) => {
                        let h = ctx.host.clone();
                        let _ =
                            tokio::task::spawn_blocking(move || h.write_file(&path, &bytes, 0o644))
                                .await;
                    }
                    Err(e) => tracing::warn!(error = %e, "manifest 序列化失败"),
                }
                // 「有没有新的 bui」不能只比版本号：rc 通道下 rc1 / rc2 / 正式版的 Cargo 版本号
                // 都是同一个，同版本重建要靠 sha256 才认得出（`kernels::bui_build_differs`）。
                // 读盘是阻塞调用，照本文件的铁律放进 spawn_blocking。
                let (h, bin, mm) = (ctx.host.clone(), ctx.paths.bin_dir.clone(), m.clone());
                let differs = tokio::task::spawn_blocking(move || {
                    let arch = h.arch().unwrap_or_default();
                    crate::kernels::bui_build_differs(
                        h.as_ref(),
                        &bin,
                        &mm,
                        env!("CARGO_PKG_VERSION"),
                        &arch,
                    )
                })
                .await
                .unwrap_or(false);
                let rebuild = differs && m.version == env!("CARGO_PKG_VERSION");
                // 比运行中的版本更**低**的候选一律不算「可升级」：事故 2026-09-15 里 latest 的
                // 4.0.0 就是这么被 4.0.1 的机器当成「有新版 bui」报出来的（守卫在
                // `pick_selfcheck_manifest`，这里再钉一道，别让提示与 runtime 说反话）
                let newer = differs.then(|| m.version.clone()).filter(|v| {
                    v.as_str() == env!("CARGO_PKG_VERSION")
                        || crate::kernels::version_is_newer(v, env!("CARGO_PKG_VERSION"))
                });
                if let Ok(mut guard) = manifest.write() {
                    *guard = Some(m);
                }
                ctx.runtime
                    .update(|r| r.upgrade_available = newer.clone())
                    .await;
                match (&newer, rebuild) {
                    (Some(_), true) => tracing::info!(
                        "每日自检：有同版本的新构建（rc 通道），可运行 `b-ui upgrade`"
                    ),
                    (Some(v), false) => {
                        tracing::info!(version = %v, "每日自检：有新版 bui，可运行 `b-ui upgrade`")
                    }
                    (None, _) => {}
                }
                // 内核版本随 manifest 走：请求一次对账（不 force）
                ctx.bus.send(Event::ReconcileRequested { force: false });
            }
            // 候选不比本机当前版本新（`pick_selfcheck_manifest` 已记一行）：缓存、内核、提示
            // 全不动，顺带把可能留下的假「可升级」清掉（事故里它被写成了更旧的 4.0.0）
            Ok(Ok(None)) => {
                ctx.runtime.update(|r| r.upgrade_available = None).await;
            }
            // rc 阶段（或正式版还没发）latest 就是 404，而且没有可回退的预发布通道：
            // 这是预期状态，只记一行 info，不用 warn/error 每天刷一条（裁决记录
            // 「发布：预发布与首推（2026-09-12）」）
            Ok(Err(e)) if crate::kernels::is_not_found(&e) => {
                tracing::info!(error = %e, "每日自检：还没有可用的 manifest，跳过本轮")
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "每日自检拉取 manifest 失败"),
            Err(e) => tracing::warn!(error = %e, "每日自检任务 panic"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(SELFCHECK_INTERVAL_SECS)).await;
    }
}

/// `<base>/manifest.json`（由 install / upgrade / 每日自检写入）；没有就返回 `None`，对账跳过 Binary
pub fn load_cached_manifest(host: &dyn Host, paths: &Paths) -> Option<Manifest> {
    let bytes = host
        .read_file(&crate::paths::manifest_file(paths))
        .ok()
        .flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// 每日自检的抖动：sha256(node_id) 取前 8 字节对 3600 取模，同一台机器永远同一个偏移。
///
/// 定义在这里而不是 `commands::upgrade`，是为了不让本模块反向依赖 Task 17。
pub fn jitter_secs(node_id: uuid::Uuid) -> u64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(node_id.as_bytes());
    let n = u64::from_be_bytes(digest[..8].try_into().expect("sha256 至少 8 字节"));
    n % 3600
}

/// `state.node.public_ip` 为空时探测一次并写回 `state.json`。
///
/// 2026-09-12 真机：`--import-v3` 时 `api.ipify.org` 返回空串，`node.public_ip` 就此留空，
/// 之后谁也不会再补（relay 的本机 IP 直连例外、订阅里的地址都靠它）。启动时补一次最省事。
/// **不阻塞启动**：探测失败或写盘失败只 warn。
pub async fn backfill_public_ip(store: &Store, host: Arc<dyn Host>) {
    if !store.read().await.node.public_ip.is_empty() {
        return;
    }
    let h = host.clone();
    let ip =
        match tokio::task::spawn_blocking(move || crate::sys::probe_public_ip(h.as_ref())).await {
            Ok(ip) => ip,
            Err(e) => {
                tracing::warn!(error = %e, "公网 IP 探测任务 panic");
                return;
            }
        };
    if ip.is_empty() {
        tracing::warn!("state.node.public_ip 为空且探测失败，请在面板里补填");
        return;
    }
    let value = ip.clone();
    match store.update(|s| s.node.public_ip = value).await {
        Ok(_) => tracing::info!(public_ip = %ip, "已回填 state.node.public_ip"),
        Err(e) => tracing::warn!(error = %e, "回填 state.node.public_ip 写盘失败"),
    }
}

/// `bui serve`：装好 Router 与全部后台任务，监听面板 HTTP 与 unix socket。
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()> {
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    // 在启动对账之前补：渲染出来的配置与订阅都读 `node.public_ip`
    backfill_public_ip(&store, host.clone()).await;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let bus = EventBus::new();
    let cached = {
        let h = host.clone();
        let p = paths.clone();
        tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p)).await?
    };
    let reg = modules(cached);
    let mods = reg.modules.clone();
    let panel = reg.panel.clone();
    let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new());
    let ctx = DaemonCtx {
        store: store.clone(),
        runtime: runtime.clone(),
        bus: bus.clone(),
        host: host.clone(),
        paths: paths.clone(),
    };
    let app_state = AppState {
        store,
        bus: bus.clone(),
        runtime: runtime.clone(),
        host: host.clone(),
        started_at: host.now(),
        version: env!("CARGO_PKG_VERSION"),
        login: crate::api::auth::LoginLimiter::default(),
    };
    runtime
        .update(|r| r.started_at = Some(crate::util::fmt_rfc3339(host.now())))
        .await;
    let app = crate::api::router(app_state, &mods);
    // spec §5.6 规则 4：旧 state 没有 slots 字段时补齐，既有住宅用户按创建时间轮流落槽。
    // 幂等，所以每次启动无条件跑一次；失败只告警（对账仍能按单槽视图渲染，行为退回 v3）。
    match crate::modules::residential::slots::migrate_on_start(&ctx.store, &bus).await {
        // spec §5.6 + D7：迁移动过分槽 ⇒ 槽规则要跟着收敛，这里只置脏，
        // 收口交给紧接着那一轮对账末尾的 converge_xray
        Ok(n) if n > 0 => {
            crate::modules::residential::slots::mark_xray_rules_dirty(&runtime).await;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "住宅槽位迁移失败，本次启动按单槽渲染"),
    }
    // spec §4.3 + §3.1：把住宅 HY2 凭据池拉到期望态（先治悬空指针、再给没有凭据的人补发）。
    // **必须在下面那轮启动对账之前**：`hy2-residential.json` 的凭据、门与 `auth_user` 规则
    // 全从池里渲染，池空就等于全体住宅 HY2 用户没有节点、没有门。
    // 幂等、零变更不写盘；失败只告警（下次启动重试，本轮按现有池渲染）。
    if let Err(e) = crate::modules::residential::slots::migrate_hy2_pool_on_start(&ctx).await {
        tracing::warn!(error = %e, "住宅 HY2 凭据池迁移失败，下次启动重试");
    }
    // 2026-09-14 裁决：升级上来的老 `state.json` 里的用户没有随机订阅 token，启动时补齐，
    // 并给他们手里那条「用户名链接」开宽限期。幂等、零变更不写盘，所以无条件跑一次；
    // 失败只告警、下次启动重试——写盘失败时挡住启动只会让面板与四个订阅端点一起停摆。
    if let Err(e) = crate::modules::panel::users::backfill_sub_tokens(&ctx.store, host.now()).await
    {
        tracing::warn!(error = %e, "补随机订阅 token 失败，下次启动重试");
    }
    // 启动时先对账一次，再拉起后台任务
    let mut startup_restarted_hy2_resi = false;
    match reconcile_from_ctx(&ctx, &mods, fetcher.clone(), false, false).await {
        Ok(r) => {
            tracing::info!(
                changed = r.changed.len(),
                restarted = r.restarted.len(),
                "启动对账完成"
            );
            startup_restarted_hy2_resi = hy2_resi_restarted(&r);
            // 报告已落盘，这时才允许重启自己（B5）
            finish_self_restart(&ctx, &r, true).await;
        }
        Err(e) => tracing::error!(error = %e, "启动对账失败"),
    }
    // spec §5.6 + D7：对账刚把新的 xray-config.json 落盘，这时收敛住宅槽路由 ——
    // 走 RoutingService gRPC 增删受影响用户的规则，**不重启 xray**；只有 gRPC 失败且
    // 文件已落地才退回一次重启。干净时是零成本 no-op，所以无条件调。
    crate::modules::residential::slots::converge_xray(&ctx, panel.xray()).await;
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for m in &mods {
        tasks.extend(m.spawn(ctx.clone()));
    }
    // spec §3.4：启动那一轮对账重启了住宅入站 ⇒ 启动路径**自己**补一次门位重放。
    // 上面那一轮发的 `Event::Hy2ResiRestarted` 必然被丢弃：那一刻总线上一个订阅者都没有
    // （`EventBus` 是裸 `broadcast::Sender`，`send` 吞 Err），`gates::replay_loop` 要到上面
    // 那行 `m.spawn(..)` 才订阅。少了这一次重放，「重启后按 `ready()` 探测 + 15 秒退避重放」
    // 这条路在**最常发生的场景**（升级 / 首装 / 任何改了住宅配置的重启）根本不走，兜底只剩
    // `sync_loop` 起来时那一次 `sync_now` —— 它没有 `ready()` 探测，sing-box 刚重启时
    // `GET /proxies` 大概率还连不上 ⇒ 全体住宅 HY2 用户停在 `deny`（握手成功、每个请求被拒）。
    // 不走总线而直接调：这样它不依赖「补发必须晚于 spawn」这个顺序，挪到哪里都成立。
    if startup_restarted_hy2_resi {
        let (c, sh) = (ctx.clone(), panel.clone());
        tasks.push(tokio::spawn(async move {
            crate::modules::panel::gates::replay_after_restart(&c, &sh).await;
        }));
    }
    // Hysteria2 的 http 鉴权（spec §3.2）：**独立**监听 127.0.0.1:AUTH_HTTP_PORT，
    // 绝不挂在下面那个面板监听上 —— 面板经 Caddy 对外，挂上去等于把鉴权面暴露到公网。
    // 无条件起：`hy2_auth=command` 时它只是没人来敲，换回 http 就不必重启守护进程。
    {
        let auth = Arc::new(crate::modules::panel::auth_http::AuthHttp::new(
            paths.clone(),
            host.clone(),
        ));
        tasks.push(tokio::spawn(
            crate::modules::panel::auth_http::refresh_loop(
                ctx.clone(),
                panel.clone(),
                auth.clone(),
            ),
        ));
        tasks.push(tokio::spawn(crate::modules::panel::auth_http::serve_loop(
            auth,
        )));
    }
    // 守护进程里**只有这一个** consumer 会调 reconcile_from_ctx（启动那一轮在它之前、串行跑完）：
    // 去抖触发、10 分钟巡检、每日自检（经 bus → 去抖）全部经这条 mpsc 排队，天然互斥。
    // 若让 10 分钟 tick 自己起一个任务直接对账，就会与去抖触发的那一轮并发——两轮同时
    // restart 同一个单元、`.verify/<file>` 候选文件互相覆盖、`runtime.json` 交叉写。
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    tasks.push(tokio::spawn(debounce_loop(bus.clone(), tx.clone())));
    {
        let (ctx2, mods2, f2) = (ctx.clone(), mods.clone(), fetcher.clone());
        let panel2 = panel.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(force) = rx.recv().await {
                match reconcile_from_ctx(&ctx2, &mods2, f2.clone(), force, false).await {
                    Ok(r) => finish_self_restart(&ctx2, &r, true).await,
                    Err(e) => tracing::error!(error = %e, "对账失败"),
                }
                // spec §5.6 + D7：同上，对账落盘之后收敛住宅槽路由（gRPC 增删，不重启）
                crate::modules::residential::slots::converge_xray(&ctx2, panel2.xray()).await;
            }
        }));
    }
    {
        // 10 分钟巡检只往同一条队列里投一次触发，不自己对账
        let tick_tx = tx.clone();
        tasks.push(tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(DRIFT_INTERVAL_SECS));
            tick.tick().await;
            loop {
                tick.tick().await;
                if tick_tx.send(false).await.is_err() {
                    return;
                }
            }
        }));
    }
    // spec §7：守护进程每日带抖动自检（刷新 manifest → 下一轮对账按新版本装内核）
    tasks.push(tokio::spawn(selfcheck_loop(
        ctx.clone(),
        reg.manifest.clone(),
        fetcher.clone(),
        // 总纲 C4：`$BUI_MANIFEST_URL` 可覆盖（M1 时内置 URL 必然 404，演练/装机都靠它）；
        // 没覆盖就跟 latest，404 时按预发布通道回退（见 `selfcheck_loop`）
        std::env::var(crate::kernels::MANIFEST_URL_ENV).ok(),
    )));
    let sock = PathBuf::from(crate::paths::SOCKET_PATH);
    let admin_bind = format!("127.0.0.1:{}", ctx.store.read().await.node.ports.admin);
    let http = tokio::net::TcpListener::bind(&admin_bind).await?;
    tracing::info!(bind = %admin_bind, "面板 HTTP 就绪（Caddy 反代）");
    let app_uds = app.clone();
    tasks.push(tokio::spawn(async move {
        if let Err(e) = crate::ipc::serve_uds(&sock, app_uds).await {
            tracing::error!(error = %e, "socket 服务退出");
        }
    }));
    // ConnectInfo 必须挂上，否则 auth::client_ip 拿不到对端地址（限速退化成一个桶）
    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    // systemd stop / restart 发的是 SIGTERM，不是 SIGINT：只 select ctrl_c 的话
    // `systemctl restart b-ui` 时后台任务不会走 abort 路径（socket 文件也不会被清掉）。
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = axum::serve(http, service).into_future() => { r?; }
        _ = sigterm.recv() => tracing::info!("收到 SIGTERM，退出"),
        _ = tokio::signal::ctrl_c() => tracing::info!("收到中断，退出"),
    }
    for t in tasks {
        t.abort();
    }
    Ok(())
}

/// `bui reconcile`：socket 可用就走 API，否则进程内直接跑。
///
/// `socket` 由调用方传入而不是在函数体里读常量——否则任何测到这条路径的单元测试都会真的
/// `connect("/run/b-ui.sock")`，在跑着 v4 守护进程的机器上会真发一次 `POST /api/reconcile`。
pub async fn reconcile_cli(
    paths: Paths,
    host: Arc<dyn Host>,
    socket: PathBuf,
    force: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::Client::new(&socket);
    if client.available().await && !dry_run {
        let (status, body) = client
            .request(
                "POST",
                "/api/reconcile",
                Some(serde_json::json!({"force": force, "dry_run": false})),
            )
            .await?;
        println!("已提交给守护进程（HTTP {status}）：{body}");
        return Ok(());
    }
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let ctx = DaemonCtx {
        store,
        runtime,
        bus: EventBus::new(),
        host: host.clone(),
        paths: paths.clone(),
    };
    let cached = {
        let h = host.clone();
        let p = paths.clone();
        tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p)).await?
    };
    let reg = modules(cached);
    let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new());
    let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, force, dry_run).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    // CLI 路径：报告与 restart_keys 已落盘，这时同步重启守护进程（B5）
    finish_self_restart(&ctx, &report, false).await;
    if !report.errors.is_empty() || !report.verify_failures.is_empty() {
        anyhow::bail!("对账有失败项，见上面的报告");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Event, EventBus};
    use crate::kernels::{Asset, Fetcher, Manifest};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct NoopInstaller;
    impl crate::reconcile::apply::BinaryInstaller for NoopInstaller {
        fn install(&self, _n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FakeFetcher(Mutex<Vec<(String, Vec<u8>)>>);
    impl FakeFetcher {
        fn take(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| crate::kernels::NotFound(url.to_string()).into())
        }
    }
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.take(url)
        }

        fn download_to(&self, url: &str, sink: &mut dyn std::io::Write) -> anyhow::Result<String> {
            let bytes = self.take(url)?;
            sink.write_all(&bytes)?;
            sink.flush()?;
            Ok(crate::kernels::sha256_hex(&bytes))
        }
    }

    /// 六个受管单元都 enable + active。这一步是必需的，而且 `b-ui` 自己那一条最关键：
    /// `UnitsModule::render` 给六个单元各产一条 `UnitState{true,true}`，而 apply 第 12 步
    /// 有意不 start/restart `b-ui`（不许在对账中途杀掉自己）。真机上守护进程正在跑，
    /// `unit_is_active("b-ui")` 为真、diff 不出这一条；假机器上不播种就永远差这一条，
    /// 于是「二次对账零变更」恒 FAIL。
    fn mark_units_up(i: &mut crate::sys::fake::FakeInner, s: &State) {
        for u in crate::reconcile::managed_units(s) {
            i.units_active.insert(format!("{u}.service"));
            i.units_enabled.insert(format!("{u}.service"));
        }
    }

    /// 一台「装好了 v4」的假机器：六个单元在跑、四个内核版本正确、公钥在位、**ufw 已启用**。
    /// ufw 那两行不是装饰：`SystemModule::render` 只在 `facts.ufw_active || facts.firewalld_active`
    /// 为真时才产出 `FirewallPorts`（Task 6），所以少了它，下面 `first_pass…` 的
    /// `keys.contains_key("firewall")` 就无从成立。
    fn ready_host() -> Arc<FakeHost> {
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            mark_units_up(i, &crate::testutil::sample_state());
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
            i.which.insert("ufw".into());
            i.scripted
                .push(("ufw status".into(), CmdOut::success("Status: active\n")));
            for (bin, out) in [
                ("hysteria", "Version:\tv2.12.2\n"),
                ("xray", "Xray 26.3.27 (Xray) abc (go1.26.1 linux/amd64)\n"),
                ("sing-box", "sing-box version 1.13.19\n"),
                ("caddy", "v2.10.2 h1:xxx\n"),
            ] {
                i.files.insert(
                    format!("/opt/b-ui/bin/{bin}").into(),
                    (b"ELF".to_vec(), 0o755),
                );
                i.scripted
                    .push((format!("/opt/b-ui/bin/{bin} version"), CmdOut::success(out)));
            }
        });
        h
    }

    fn input<'a>(
        state: &'a bui_schema::model::State,
        mods: &'a [Arc<dyn Module>],
        paths: &'a bui_schema::paths::Paths,
        keys: &'a BTreeMap<String, String>,
        installer: &'a dyn crate::reconcile::apply::BinaryInstaller,
    ) -> ReconcileInput<'a> {
        ReconcileInput {
            state,
            modules: mods,
            paths,
            keys,
            installer,
            force: false,
            dry_run: false,
        }
    }

    #[test]
    fn first_pass_writes_everything_second_pass_is_a_no_op() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let keys = BTreeMap::new();
        let (first, keys) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            !first.changed.is_empty(),
            "首轮要写配置、单元、sysctl、符号链接"
        );
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(
            first.verify_failures.is_empty(),
            "{:?}",
            first.verify_failures
        );
        assert!(first.changed.iter().any(|c| c.contains("config.yaml")));
        assert!(first
            .changed
            .iter()
            .any(|c| c.contains("hysteria-server.service")));
        assert!(first
            .changed
            .iter()
            .any(|c| c.contains("99-b-ui-network.conf")));
        assert!(
            first.changed.iter().any(|c| c == "nf_conntrack"),
            "首轮要加载 conntrack 模块"
        );
        assert!(
            first.changed.iter().any(|c| c == "/usr/local/bin/b-ui"),
            "首轮要建 CLI 符号链接"
        );
        assert!(
            first.self_restart_required,
            "首轮写了 b-ui.service，自身重启交给调用方"
        );
        // B4：apply 必须把 plan.keys 搬进 outcome.keys，reconcile_once 再 extend 进 restart_keys。
        assert!(keys.contains_key("firewall"), "{keys:?}");
        assert!(
            keys.contains_key("file:/opt/b-ui/xray-config.json"),
            "{keys:?}"
        );
        host.clear_ops();
        let (second, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            second.is_clean(),
            "二次对账必须零变更（M1 验收项）：{second:?}"
        );
        assert!(
            second.drift.is_empty(),
            "二次对账也不能报漂移：{:?}",
            second.drift
        );
        assert!(
            host.ops()
                .iter()
                .all(|o| o.starts_with("run:") || o.starts_with("write:/opt/b-ui/.verify/")),
            "只允许剩下只读探测与校验落地：{:?}",
            host.ops()
        );
    }

    /// T8 的机器化验收（4.1）：一台从 4.0 升上来的假机器跑一轮对账之后 ——
    /// ① 槽 1..7 的住宅实例被停用、禁用、单元文件删掉；② `nft -f` 发了**一次**；
    /// ③ `hysteria-residential` 的单元正文换成 sing-box；④ 第二轮零变更。
    ///
    /// ④ 要成立，假机器必须让 `nft list tables` 答得出 `table inet bui`：规则集哈希相同
    /// **且**表在才算已落地（表被人刷掉时哈希看不出来，只能靠这份事实）。
    #[test]
    fn a_host_upgraded_from_four_zero_retires_the_slot_units_and_lands_the_nft_table() {
        let host = ready_host();
        host.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                CmdOut::success("table inet bui\n"),
            ));
            // 4.0 的槽 1 还在跑、配置与单元文件都在盘上
            i.units_active
                .insert("hysteria-residential-1.service".into());
            i.units_enabled
                .insert("hysteria-residential-1.service".into());
            i.files.insert(
                "/etc/systemd/system/hysteria-residential-1.service".into(),
                (
                    b"[Service]\nExecStart=/opt/b-ui/bin/hysteria server\n".to_vec(),
                    0o644,
                ),
            );
            i.files.insert(
                "/opt/b-ui/config-residential-1.yaml".into(),
                (b"listen: :40001,45500-50000\n".to_vec(), 0o600),
            );
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (b"listen: :40000,41000-45499\n".to_vec(), 0o600),
            );
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (first, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(
            first.verify_failures.is_empty(),
            "{:?}",
            first.verify_failures
        );
        let ops = host.ops();
        // ①
        for op in [
            "systemd:stop:hysteria-residential-1.service",
            "systemd:disable:hysteria-residential-1.service",
            "remove:/etc/systemd/system/hysteria-residential-1.service",
            "remove:/opt/b-ui/config-residential-1.yaml",
            "remove:/opt/b-ui/config-residential.yaml",
        ] {
            assert!(ops.iter().any(|o| o == op), "缺 {op}：{ops:?}");
        }
        // ②
        assert_eq!(
            ops.iter().filter(|o| o.as_str() == "run:nft -f -").count(),
            1,
            "{ops:?}"
        );
        assert_eq!(
            host.stdins()
                .iter()
                .filter(|(cmd, _)| cmd == "nft -f -")
                .map(|(_, body)| body.clone())
                .collect::<Vec<_>>(),
            vec![bui_schema::render::nft::ruleset(&state.node.ports, true)],
            "喂进去的必须是 bui-schema 渲染的那一份"
        );
        assert_eq!(
            keys.get("nft:inet:bui").map(String::as_str),
            Some(crate::reconcile::diff::ruleset_key(
                &bui_schema::render::nft::ruleset(&state.node.ports, true)
            ))
            .as_deref(),
            "{keys:?}"
        );
        // ③
        assert_eq!(
            host.text("/etc/systemd/system/hysteria-residential.service")
                .map(|t| t.contains("sing-box run -c /opt/b-ui/hy2-residential.json")),
            Some(true)
        );
        assert!(host.text("/opt/b-ui/hy2-residential.json").is_some());
        // ④
        host.clear_ops();
        let (second, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            second.is_clean(),
            "二次对账必须零变更（M1 验收项）：{second:?}"
        );
        assert!(
            second.drift.is_empty(),
            "二次对账也不能报漂移：{:?}",
            second.drift
        );
        assert!(
            !host.ops().iter().any(|o| o == "run:nft -f -"),
            "表在且哈希相同就不该重放：{:?}",
            host.ops()
        );
    }

    /// 没有 `nft` 的机器：**不算 apply 失败**（装包不是对账能决定的），但每轮都留一条按
    /// 真实量级写的提示，而且二次对账仍然零变更（notes 不进 `changed`，与「没有防火墙」同款）。
    #[test]
    fn a_host_without_nft_still_reconciles_clean_but_keeps_warning() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (first, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(
            first.notes.iter().any(|n| n.contains("现役订阅")),
            "{:?}",
            first.notes
        );
        assert!(!keys.contains_key("nft:inet:bui"), "没落地就不许记账");
        let (second, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(second.is_clean(), "{second:?}");
        assert!(second.notes.iter().any(|n| n.contains("现役订阅")));
    }

    #[test]
    fn second_pass_is_clean_even_when_the_kernel_clamps_a_multi_value_sysctl() {
        // bwg-rick 真机 M1 step2 的两种成因都在这里兜住：多值键读回制表符（FakeHost 默认行为）
        // 与内核钳制（`sysctl_clamp`）—— 两轮之后 changed 必须为空。
        let host = ready_host();
        host.with(|i| {
            i.sysctl_clamp
                .insert("net.ipv4.udp_mem".into(), "8192\t524288\t1048576".into());
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (first, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(first.changed.iter().any(|c| c == "net.ipv4.tcp_rmem"));
        assert!(
            first.notes.iter().any(|n| n.contains("net.ipv4.udp_mem")),
            "被钳制的键要留一条提示：{:?}",
            first.notes
        );
        let (second, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            second.is_clean(),
            "多值键的制表符与内核钳制都不该让二次对账有改动：{:?}",
            second.changed
        );
    }

    #[test]
    fn a_hand_edited_config_is_rewritten_and_only_its_unit_restarts() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (_, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        host.write_file(Path::new("/opt/b-ui/config.yaml"), b"listen: :1\n", 0o600)
            .unwrap();
        host.clear_ops();
        let (report, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert_eq!(report.changed, vec!["/opt/b-ui/config.yaml".to_string()]);
        assert_eq!(report.restarted, vec!["hysteria-server".to_string()]);
    }

    #[test]
    fn drift_is_reported_but_untouched_until_force() {
        let host = ready_host();
        host.with(|i| {
            i.files.insert(
                "/etc/systemd/system/xray.service.d/50-manual.conf".into(),
                (b"x".to_vec(), 0o644),
            );
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (report, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        // `bin/` 里的四个内核二进制不会各报一条：`list_dir` 只返回直接子项，`bin` 在白名单里
        assert_eq!(
            report.drift.len(),
            1,
            "只有手工 drop-in 一条：{:?}",
            report.drift
        );
        assert_eq!(report.drift[0].kind, "unit_dropin");
        assert!(
            host.text("/etc/systemd/system/xray.service.d/50-manual.conf")
                .is_some(),
            "只报不改"
        );
        let mut forced = input(&state, &reg.modules, &paths, &keys, &NoopInstaller);
        forced.force = true;
        let (report2, _) = reconcile_once(forced, host.as_ref()).unwrap();
        assert!(
            host.text("/etc/systemd/system/xray.service.d/50-manual.conf")
                .is_none(),
            "--force 才清理"
        );
        assert!(report2.notes.iter().any(|n| n.contains("50-manual.conf")));
    }

    #[test]
    fn a_host_without_a_firewall_gets_a_note_and_still_reconciles_clean() {
        // 第三轮审查 C1 的回归：没有 ufw/firewalld 的机器上，「放行端口」只能落进 `notes`，
        // 绝不能落进 `changed`。
        let host = Arc::new(FakeHost::new()); // 没有 ufw、没有 firewalld
        host.with(|i| {
            mark_units_up(i, &crate::testutil::sample_state());
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (first, keys) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            first
                .notes
                .iter()
                .any(|n| n.contains("安全组") && n.contains("40000/udp")),
            "{:?}",
            first.notes
        );
        assert!(
            !first.changed.iter().any(|c| c.starts_with("firewall")),
            "没有防火墙时不该有 firewall 这条改动：{:?}",
            first.changed
        );
        assert!(
            !keys.contains_key("firewall"),
            "没改过防火墙就不该记 key：{keys:?}"
        );
        let (second, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &keys, &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(second.is_clean(), "第二轮必须零变更：{second:?}");
        assert!(
            second.notes.iter().any(|n| n.contains("安全组")),
            "提示要每轮都在（只报不改）"
        );
    }

    #[test]
    fn missing_pubkey_becomes_a_note_not_a_failure() {
        let host = Arc::new(FakeHost::new()); // 没有 authorized_keys
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (report, _) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            report.notes.iter().any(|n| n.contains("harden-ssh")),
            "{:?}",
            report.notes
        );
        assert!(report.errors.is_empty());
        assert!(host
            .text("/etc/ssh/sshd_config.d/00-b-ui-hardening.conf")
            .is_none());
    }

    #[test]
    fn binaries_come_from_the_manifest_when_versions_differ() {
        let host = ready_host();
        host.with(|i| {
            // 装的是旧 sing-box
            i.scripted.insert(
                0,
                (
                    "/opt/b-ui/bin/sing-box version".into(),
                    CmdOut::success("sing-box version 1.12.0\n"),
                ),
            );
        });
        let m = Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([("sing_box".to_string(), "1.13.19".to_string())]),
            artifacts: BTreeMap::from([(
                "sing-box-linux-amd64".to_string(),
                Asset {
                    url: "https://x/sb".into(),
                    sha256: "00".into(),
                },
            )]),
            min_upgrade_from: None,
            tag: None,
        };
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(Some(m));
        let (report, _) = reconcile_once(
            input(
                &state,
                &reg.modules,
                &paths,
                &BTreeMap::new(),
                &NoopInstaller,
            ),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            report.changed.iter().any(|c| c.contains("sing-box")),
            "{:?}",
            report.changed
        );
    }

    #[test]
    fn dry_run_changes_nothing_on_disk() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let keys = BTreeMap::new();
        let mut i = input(&state, &reg.modules, &paths, &keys, &NoopInstaller);
        i.dry_run = true;
        let (report, _) = reconcile_once(i, host.as_ref()).unwrap();
        assert!(!report.changed.is_empty());
        assert!(report.dry_run);
        assert!(host.text("/opt/b-ui/config.yaml").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn debounce_collapses_a_burst_into_one_trigger() {
        let bus = EventBus::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(debounce_loop(bus.clone(), tx));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        for _ in 0..5 {
            bus.send(Event::StateChanged("test"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert_eq!(
            rx.try_recv().ok(),
            Some(false),
            "五次变更压成一次（force=false）"
        );
        assert!(rx.try_recv().is_err());
        bus.send(Event::ReconcileRequested { force: true });
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert_eq!(rx.try_recv().ok(), Some(true), "force 要透传");
    }

    /// 两条「内核刚重启」的事件都不许触发对账：它们各有自己的重放订阅者
    /// （`health::replay_loop` / `gates::replay_loop`），在这里白跑一轮只会多一次全量对账。
    #[tokio::test(start_paused = true)]
    async fn a_kernel_restart_event_does_not_trigger_a_reconcile() {
        let bus = EventBus::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(debounce_loop(bus.clone(), tx));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        bus.send(Event::RelayRestarted);
        bus.send(Event::Hy2ResiRestarted);
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert!(rx.try_recv().is_err(), "重启事件不该投触发");
        // 同一个循环还活着：随后的 StateChanged 照旧要触发
        bus.send(Event::StateChanged("test"));
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert_eq!(rx.try_recv().ok(), Some(false));
    }

    /// 比本机（`CARGO_PKG_VERSION`）严格高一档的版本号（`x.(y+1).0`）。每日自检的降级守卫只
    /// 放 rank 更高的候选过，所以「有新版」这类用例不能把版本号写死 —— workspace 版本一升就
    /// 会失效。
    fn newer_version() -> String {
        let mut seg = env!("CARGO_PKG_VERSION").split('.');
        let x: u32 = seg.next().unwrap().parse().unwrap();
        let y: u32 = seg.next().unwrap().parse().unwrap();
        format!("{x}.{}.0", y + 1)
    }

    async fn ctx_for(host: Arc<FakeHost>, d: &tempfile::TempDir) -> DaemonCtx {
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state())
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        DaemonCtx {
            store,
            runtime,
            bus: EventBus::new(),
            host,
            paths: bui_schema::paths::Paths {
                base_dir: d.path().into(),
                certs_dir: d.path().join("certs"),
                bin_dir: d.path().join("bin"),
            },
        }
    }

    #[tokio::test(start_paused = true)]
    async fn selfcheck_waits_for_its_jitter_then_refreshes_the_manifest() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let node_id = ctx.store.read().await.node.id;
        let jitter = jitter_secs(node_id);
        // 版本必须严格高于本机：自检只接受 rank 更高的候选（`pick_selfcheck_manifest`）
        let newer = newer_version();
        let manifest_json = serde_json::json!({
            "version": newer.clone(),
            "kernels": { "sing_box": "1.14.2" },
            "artifacts": { "sing-box-linux-amd64": { "url": "https://x/sb", "sha256": "00" } }
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![(
            "https://x/manifest.json".into(),
            manifest_json.into_bytes(),
        )])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let mut events = ctx.bus.subscribe();
        let task = tokio::spawn(selfcheck_loop(
            ctx.clone(),
            handle.clone(),
            fetcher,
            Some("https://x/manifest.json".to_string()),
        ));
        // 抖动窗口内什么都不该发生
        tokio::time::sleep(std::time::Duration::from_secs(
            jitter.saturating_sub(1).max(1) - 1,
        ))
        .await;
        assert!(
            handle.read().unwrap().is_none(),
            "抖动没到就不该拉 manifest"
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert_eq!(
            handle.read().unwrap().as_ref().map(|m| m.version.clone()),
            Some(newer.clone()),
            "共享句柄要被刷新，否则内核永远跟不上 manifest"
        );
        assert!(
            host.text(d.path().join("manifest.json").to_str().unwrap())
                .is_some(),
            "manifest 要落盘给下次启动用"
        );
        assert_eq!(
            ctx.runtime.read().await.upgrade_available.as_deref(),
            Some(newer.as_str())
        );
        assert_eq!(
            events.recv().await.unwrap(),
            Event::ReconcileRequested { force: false }
        );
        task.abort();
    }

    /// rc 通道（2026-09-12 bwg-rick）：rc1 / rc2 / 正式版的 Cargo 版本号都是同一个，所以每日
    /// 自检只比版本号就会一直报「已是最新」。同版本但 manifest 里 bui 资产的 sha256 与盘上
    /// `bin/bui` 不同时必须报成「有新构建」；sha 一致才是真的没东西可升。
    ///
    /// 2026-09-15 起「同一版的另一份构建」由 rank 的 rc 位分辨（缓存 `tag` = rc1，候选 = rc2）：
    /// 降级守卫只放 rank 更高的候选过，光换 sha256 已经不足以让自检去动缓存。
    #[tokio::test(start_paused = true)]
    async fn selfcheck_reports_a_same_version_rebuild_as_upgradable() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let bui = ctx.paths.bin_dir.join("bui");
        host.write_file(&bui, b"BUI-rc1", 0o755).unwrap();
        let ver = env!("CARGO_PKG_VERSION");
        // 缓存 = 这一版的 rc1（一次显式 `bui upgrade` 写下的那一份）
        host.write_file(
            &crate::paths::manifest_file(&ctx.paths),
            serde_json::json!({
                "version": ver, "tag": format!("v{ver}-rc1"), "kernels": {}, "artifacts": {}
            })
            .to_string()
            .as_bytes(),
            0o644,
        )
        .unwrap();
        // 版本号与本进程一致，只有 tag 进了一档、bui 资产换成了另一份构建
        let manifest_json = serde_json::json!({
            "version": ver,
            "tag": format!("v{ver}-rc2"),
            "kernels": {},
            "artifacts": { "bui-linux-amd64": {
                "url": "https://x/bui",
                "sha256": crate::kernels::sha256_hex(b"BUI-rc2"),
            } }
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![(
            "https://x/manifest.json".into(),
            manifest_json.into_bytes(),
        )])));
        let task = tokio::spawn(selfcheck_loop(
            ctx.clone(),
            Arc::new(std::sync::RwLock::new(None)),
            fetcher,
            Some("https://x/manifest.json".to_string()),
        ));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        assert_eq!(
            ctx.runtime.read().await.upgrade_available.as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "同版本的新构建也要报可升级"
        );
        // 换上那一份之后（sha 一致、且缓存已经是 rc2 ⇒ 候选 rank 不再更高）下一轮就该回到
        // 「没东西可升」
        host.write_file(&bui, b"BUI-rc2", 0o755).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(SELFCHECK_INTERVAL_SECS + 3)).await;
        assert_eq!(
            ctx.runtime.read().await.upgrade_available,
            None,
            "版本相同且 sha 相同才是已最新"
        );
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn selfcheck_follows_the_prerelease_channel_when_latest_is_404() {
        // 裁决记录「发布：预发布与首推（2026-09-12）」：仓库里只有预发布时 releases/latest
        // 必然 404，每日自检必须走 kernels 里那同一套**无条件**回退（本机没有任何缓存、
        // 版本号是纯 semver，认不出 rc），而不是每天 warn 一条「拉取失败」。
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let rc_url = crate::kernels::manifest_url_for_tag("v4.0.0-rc2");
        let newer = newer_version();
        let rc_manifest = serde_json::json!({
            "version": newer.clone(),
            "kernels": { "sing_box": "1.14.2" },
            "artifacts": { "sing-box-linux-amd64": { "url": "https://x/sb", "sha256": "00" } }
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![
            (
                crate::kernels::RELEASES_API_URL.into(),
                br#"[{"tag_name":"v4.0.0-rc2","prerelease":true}]"#.to_vec(),
            ),
            (rc_url, rc_manifest.into_bytes()),
        ])));
        // bwg-rick 的真机形状：跑着 4.0.0（纯 semver），还没有任何 manifest 缓存
        let handle = Arc::new(std::sync::RwLock::new(None));
        let task = tokio::spawn(selfcheck_loop(ctx.clone(), handle.clone(), fetcher, None));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        assert_eq!(
            handle.read().unwrap().as_ref().map(|m| m.version.clone()),
            Some(newer),
            "latest 404 之后应当跟上预发布通道里最新的 rc"
        );
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn selfcheck_survives_a_404_with_nothing_to_fall_back_to() {
        // latest 404 且 releases 列表也拉不到（什么都没发 / 断网）：只记一行 info，
        // 不写缓存、不请求对账、更不会退出循环
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let task = tokio::spawn(selfcheck_loop(ctx.clone(), handle.clone(), fetcher, None));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        assert!(handle.read().unwrap().is_none());
        assert!(
            host.text(d.path().join("manifest.json").to_str().unwrap())
                .is_none(),
            "拉不到就不该写缓存"
        );
        assert!(!task.is_finished(), "404 不许让自检循环退出");
        task.abort();
    }

    /// 事故回归（2026-09-15，两台 rc1 服务器）：`releases/latest` 不含预发布，所以 rc1 机器的
    /// 每日自检从 latest 拿回来的是**更旧**的稳定版 manifest（4.0.0）。照它刷缓存就把 relay 的
    /// sing-box 从 1.14.1 降回 1.14.0，还打出「每日自检：有新版 bui」。缓存 rank 更高时这一轮
    /// 必须一动不动：缓存不变、共享句柄不变、不请求对账、不报可升级。
    #[tokio::test(start_paused = true)]
    async fn selfcheck_never_walks_a_prerelease_machine_back_to_stable() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let ver = env!("CARGO_PKG_VERSION");
        let rc1_tag = format!("v{ver}-rc1");
        // 缓存 = rc1（升级时写下的那一份，sing-box 1.14.1）
        let cached = serde_json::json!({
            "version": ver,
            "tag": rc1_tag.clone(),
            "kernels": { "sing_box": "1.14.1" },
            "artifacts": {}
        })
        .to_string();
        let cache_file = crate::paths::manifest_file(&ctx.paths);
        host.write_file(&cache_file, cached.as_bytes(), 0o644)
            .unwrap();
        // latest 是更旧的稳定版 4.0.0（sing-box 1.14.0），rc 列表里仍只有 rc1
        let latest = serde_json::json!({
            "version": "4.0.0",
            "tag": "v4.0.0",
            "kernels": { "sing_box": "1.14.0" },
            "artifacts": {}
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![
            (crate::kernels::MANIFEST_URL.into(), latest.into_bytes()),
            (
                crate::kernels::RELEASES_API_URL.into(),
                format!(r#"[{{"tag_name":"{rc1_tag}","prerelease":true}}]"#).into_bytes(),
            ),
            (
                crate::kernels::manifest_url_for_tag(&rc1_tag),
                cached.clone().into_bytes(),
            ),
        ])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let mut events = ctx.bus.subscribe();
        let task = tokio::spawn(selfcheck_loop(ctx.clone(), handle.clone(), fetcher, None));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        assert_eq!(
            host.text(cache_file.to_str().unwrap()).as_deref(),
            Some(cached.as_str()),
            "缓存必须一个字节都不变：对账照它决定内核版本"
        );
        assert!(
            handle.read().unwrap().is_none(),
            "共享句柄不许被更旧的 manifest 覆盖"
        );
        assert_eq!(
            ctx.runtime.read().await.upgrade_available,
            None,
            "更旧的版本不是「新版」"
        );
        assert!(
            events.try_recv().is_err(),
            "不许因此请求对账：那一步会把内核降回去"
        );
        assert!(!task.is_finished(), "跳过一轮不许让自检循环退出");
        task.abort();
    }

    /// 被降过的机器要能自己爬回来：缓存已经是稳定版 4.0.0（事故留下的现状），而运行中的 bui
    /// 比它新 ⇒ 这一轮去问 rc 通道，把缓存恢复成本版的 rc1（内核也随之回到 1.14.1）。
    #[tokio::test(start_paused = true)]
    async fn selfcheck_heals_a_cache_that_was_already_walked_back() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let ver = env!("CARGO_PKG_VERSION");
        let rc1_tag = format!("v{ver}-rc1");
        let bui = ctx.paths.bin_dir.join("bui");
        host.write_file(&bui, b"BUI-rc1", 0o755).unwrap();
        let cache_file = crate::paths::manifest_file(&ctx.paths);
        let stale = serde_json::json!({
            "version": "4.0.0",
            "tag": "v4.0.0",
            "kernels": { "sing_box": "1.14.0" },
            "artifacts": {}
        })
        .to_string();
        host.write_file(&cache_file, stale.as_bytes(), 0o644)
            .unwrap();
        // rc1 的 manifest：bui 资产就是盘上这一份（所以不该顺带报「可升级」）
        let rc1 = serde_json::json!({
            "version": ver,
            "tag": rc1_tag.clone(),
            "kernels": { "sing_box": "1.14.1" },
            "artifacts": { "bui-linux-amd64": {
                "url": "https://x/bui",
                "sha256": crate::kernels::sha256_hex(b"BUI-rc1"),
            } }
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![
            (
                crate::kernels::MANIFEST_URL.into(),
                stale.clone().into_bytes(),
            ),
            (
                crate::kernels::RELEASES_API_URL.into(),
                format!(r#"[{{"tag_name":"{rc1_tag}","prerelease":true}}]"#).into_bytes(),
            ),
            (
                crate::kernels::manifest_url_for_tag(&rc1_tag),
                rc1.clone().into_bytes(),
            ),
        ])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let mut events = ctx.bus.subscribe();
        let task = tokio::spawn(selfcheck_loop(ctx.clone(), handle.clone(), fetcher, None));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        // 缓存是 `to_vec_pretty` 重新序列化后落盘的，所以比语义不比字节
        let on_disk: Manifest = serde_json::from_str(
            &host
                .text(cache_file.to_str().unwrap())
                .expect("缓存必须被重写"),
        )
        .unwrap();
        assert_eq!(
            on_disk,
            serde_json::from_str::<Manifest>(&rc1).unwrap(),
            "缓存要被恢复成 rc 那一份，否则对账继续按 4.0.0 装内核"
        );
        assert_eq!(
            handle.read().unwrap().as_ref().and_then(|m| m.tag.clone()),
            Some(rc1_tag),
        );
        assert_eq!(
            ctx.runtime.read().await.upgrade_available,
            None,
            "盘上的 bui 就是 rc1 那一份：自愈不该顺带报「可升级」"
        );
        assert_eq!(
            events.try_recv().ok(),
            Some(Event::ReconcileRequested { force: false }),
            "换了缓存就要对账一次，把内核拉回 1.14.1"
        );
        task.abort();
    }

    /// 稳定版机器绝不被自动移到预发布：缓存与 latest 都是本机这一版，releases 列表里有个更高
    /// 版本的 rc 也不许跟过去（跟了就等于把正式版机器推上预发布通道）。
    #[tokio::test(start_paused = true)]
    async fn selfcheck_keeps_a_stable_machine_off_the_prerelease_channel() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let jitter = jitter_secs(ctx.store.read().await.node.id);
        let ver = env!("CARGO_PKG_VERSION");
        let stable = serde_json::json!({
            "version": ver, "tag": format!("v{ver}"),
            "kernels": { "sing_box": "1.14.1" }, "artifacts": {}
        })
        .to_string();
        let cache_file = crate::paths::manifest_file(&ctx.paths);
        host.write_file(&cache_file, stable.as_bytes(), 0o644)
            .unwrap();
        let next = newer_version();
        let rc_tag = format!("v{next}-rc1");
        let rc = serde_json::json!({
            "version": next, "tag": rc_tag.clone(),
            "kernels": { "sing_box": "1.15.0" }, "artifacts": {}
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![
            (
                crate::kernels::MANIFEST_URL.into(),
                stable.clone().into_bytes(),
            ),
            (
                crate::kernels::RELEASES_API_URL.into(),
                format!(r#"[{{"tag_name":"{rc_tag}","prerelease":true}}]"#).into_bytes(),
            ),
            (
                crate::kernels::manifest_url_for_tag(&rc_tag),
                rc.into_bytes(),
            ),
        ])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let task = tokio::spawn(selfcheck_loop(ctx.clone(), handle.clone(), fetcher, None));
        tokio::time::sleep(std::time::Duration::from_secs(jitter + 3)).await;
        assert_eq!(
            host.text(cache_file.to_str().unwrap()).as_deref(),
            Some(stable.as_str()),
            "缓存不变：正式版机器不跟 rc"
        );
        assert_eq!(ctx.runtime.read().await.upgrade_available, None);
        task.abort();
    }

    /// spec §3.4：对账重启住宅入站之后必须发 `Event::Hy2ResiRestarted` —— 不开 `cache_file`，
    /// 重启会把每个门打回 `default = deny`，不重放就是全体住宅 HY2 用户被拒到 60 秒安全网
    /// 那一轮。
    #[tokio::test]
    async fn restarting_the_residential_inbound_announces_it_on_the_bus() {
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let mut rx = ctx.bus.subscribe();
        let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false)
            .await
            .unwrap();
        assert!(
            report.restarted.iter().any(|u| u == "hysteria-residential"),
            "前提：首轮对账会重启住宅入站：{:?}",
            report.restarted
        );
        let mut seen = Vec::new();
        while let Ok(e) = rx.try_recv() {
            seen.push(e);
        }
        assert!(
            seen.contains(&Event::Hy2ResiRestarted),
            "住宅入站重启没有广播出去：{seen:?}"
        );
    }

    /// 守门（第七波复核 important）：启动那一轮对账广播的 `Event::Hy2ResiRestarted`
    /// **必然被丢弃** —— `EventBus` 是裸 `broadcast::Sender`（`send` 吞 Err），而唯一的
    /// 订阅者 `gates::replay_loop` 要到 `Module::spawn` 才起、`spawn` 又晚于启动对账。
    /// 所以 [`run`] 不能只靠广播：它拿 [`hy2_resi_restarted`] 的判据在 spawn 之后自己调一次
    /// `gates::replay_after_restart`。少了那一次，升级 / 首装 / 任何改了住宅配置的重启之后
    /// 全体住宅 HY2 用户都停在 `deny`，只能等 `sync_loop` 那一次没有 `ready()` 探测的
    /// `sync_now` 去碰运气。
    #[tokio::test]
    async fn the_startup_reconciles_restart_event_is_dropped_before_anyone_subscribes() {
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        // 生产顺序：启动对账在前，后台任务（唯一的订阅者）在后
        let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false)
            .await
            .unwrap();
        let mut rx = ctx.bus.subscribe();
        assert!(
            hy2_resi_restarted(&report),
            "前提：首轮对账会重启住宅入站：{:?}",
            report.restarted
        );
        let mut only_direct = report.clone();
        only_direct.restarted = vec!["hysteria-server".into(), "xray".into()];
        assert!(
            !hy2_resi_restarted(&only_direct),
            "判据只认住宅入站：别的内核重启不该触发门位重放"
        );
        assert!(
            rx.try_recv().is_err(),
            "spawn 之前广播的那一条一定收不到 ⇒ 启动路径必须自己补一次门位重放"
        );
    }

    /// 守门（第七波复核 important）：`reconcile_from_ctx` 必须在落盘前把**空闲**凭据的
    /// secret 重随机（spec §3.1 第三道防线，`hy2pool::regenerate_idle_secrets`）。
    /// 接不上的话被 `release` 掉的凭据 secret 终生不变：24 小时冷却期一过，同 id 同 secret
    /// 原样发给下一个人，前任持有人手里的旧订阅直接连上新人的门。
    ///
    /// 三面一起钉：①文件不变的那一轮一个字节都不许动（否则每轮对账都重写 + 重启住宅内核）；
    /// ②`--dry-run` 不许改期望态；③池扩容那一轮空闲全换、**在用的一条不动**。
    #[tokio::test]
    async fn a_reconcile_that_rewrites_the_residential_config_rerolls_idle_secrets() {
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let f: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let run = |dry: bool| {
            let (c, mods, f) = (ctx.clone(), reg.modules.clone(), f.clone());
            async move {
                reconcile_from_ctx(&c, &mods, f, false, dry).await.unwrap();
            }
        };
        crate::modules::residential::slots::migrate_hy2_pool_on_start(&ctx)
            .await
            .unwrap();
        run(false).await; // 池落盘：此后文件与期望态一致
        let snapshot = || {
            let c = ctx.clone();
            async move {
                let s = c.store.read().await;
                s.residential
                    .hy2_pool
                    .creds
                    .iter()
                    .map(|c| (c.id.clone(), c.secret.clone()))
                    .collect::<Vec<_>>()
            }
        };
        let used = ctx.store.read().await.users[0]
            .credentials
            .hy2_resi_cred
            .clone()
            .expect("前提：存量住宅用户占着一条");
        let stable = snapshot().await;
        run(false).await;
        assert_eq!(
            snapshot().await,
            stable,
            "文件不变的那一轮不许换 secret —— 换了就是每轮对账都重启一次住宅内核"
        );

        // 池扩容（spec §3.5 四件事之一）⇒ 这一轮本来就要重写 `hy2-residential.json`
        ctx.store
            .update(|s| {
                s.residential
                    .hy2_pool
                    .creds
                    .push(bui_schema::model::ReservedCred {
                        id: "r099".into(),
                        name: "r099".into(),
                        secret: "x".repeat(22),
                        released_at: None,
                    });
            })
            .await
            .unwrap();
        let grown = snapshot().await;
        run(true).await;
        assert_eq!(snapshot().await, grown, "`--dry-run` 一个字节都不许写");
        run(false).await;
        let after = snapshot().await;
        for (id, secret) in &grown {
            let now = &after.iter().find(|(i, _)| i == id).expect("凭据不会消失").1;
            if *id == used {
                assert_eq!(now, secret, "在用的凭据 secret 一个字节都不许动");
            } else {
                assert_ne!(now, secret, "空闲凭据 {id} 的 secret 没换");
            }
        }
    }

    /// 守门（第七波复核 minor）：`slots::migrate_hy2_pool_on_start` **必须早于启动那一轮
    /// 对账**。顺序破了的后果是 4.0.x 升上来的机器第一轮拿空池渲染 `hy2-residential.json`
    /// （`users: []`，一条 `auth_user` 规则、一个门都没有）并重启内核，全体住宅 HY2 用户掉线，
    /// 直到池迁移触发第二轮对账才恢复。这一条钉住「两个顺序渲染出来的东西真的不一样」。
    #[tokio::test]
    async fn the_credential_pool_must_be_migrated_before_the_first_reconcile() {
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let auth_users = || -> usize {
            let path = crate::modules::core_files::hy2_resi_config_path(&ctx.paths);
            let text = host
                .text(path.to_str().unwrap())
                .expect("对账必须落下住宅 HY2 配置");
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            v["inbounds"][0]["users"].as_array().map_or(0, Vec::len)
        };
        // 反面（顺序破了）：池还没迁移就对账
        reconcile_from_ctx(&ctx, &reg.modules, fetcher.clone(), false, false)
            .await
            .unwrap();
        assert_eq!(auth_users(), 0, "空池 ⇒ 谁都通不过住宅入站的鉴权");
        // 生产顺序：先迁移池，再对账
        crate::modules::residential::slots::migrate_hy2_pool_on_start(&ctx)
            .await
            .unwrap();
        reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false)
            .await
            .unwrap();
        assert!(
            auth_users() > 0,
            "迁移之后这一轮必须把存量住宅用户的凭据写进配置"
        );
    }

    #[tokio::test]
    async fn the_daemon_restarts_itself_only_after_the_report_is_persisted() {
        // B5：apply 里不重启 b-ui；守护进程路径改成报告落盘后 `restart --no-block`。
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false)
            .await
            .unwrap();
        assert!(report.self_restart_required);
        assert!(
            !host
                .ops()
                .iter()
                .any(|o| o.contains("restart") && o.contains("b-ui.service")),
            "对账本身不许动 b-ui：{:?}",
            host.ops()
        );
        assert!(
            ctx.runtime.read().await.last_reconcile.is_some(),
            "报告先落盘"
        );
        finish_self_restart(&ctx, &report, true).await;
        assert_eq!(
            host.ops().last().map(String::as_str),
            Some("run:systemctl restart --no-block b-ui.service"),
            "守护进程内不能同步 restart 自己"
        );
    }

    #[test]
    fn jitter_is_deterministic_per_node_and_within_an_hour() {
        let a = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000001").unwrap();
        let b = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000002").unwrap();
        assert_eq!(jitter_secs(a), jitter_secs(a));
        assert_ne!(jitter_secs(a), jitter_secs(b));
        assert!(jitter_secs(a) < 3600 && jitter_secs(b) < 3600);
    }

    #[test]
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        // 裁决 D14：子集断言，不再精确计数——P2 在这张表里加自己的模块名、P3 加
        // "residential"，两条车道各追加一行字符串，`git` 冲突机械可解。注册顺序仍
        // 由 modules() 自己保证：residential 的 render 返回空，同路径 artifact 的
        // 覆盖顺序不受它影响（见契约决策 §B）
        for want in [
            "core-files",
            "units",
            "system",
            "ssh",
            "certs",
            "watchdog",
            "panel",
            "residential",
            "sentinel",
        ] {
            assert!(
                names.contains(&want),
                "模块 {want} 必须注册，实际 {names:?}"
            );
        }
        *reg.manifest.write().unwrap() = Some(Manifest {
            version: "4.0.1".into(),
            kernels: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            min_upgrade_from: None,
            tag: None,
        });
        // 句柄与 core-files 模块里的是同一个锁：写进去以后 render 会看到
        let state = crate::testutil::sample_state();
        let ctx = crate::reconcile::RenderCtx {
            paths: bui_schema::paths::Paths::default_server(),
            facts: crate::reconcile::Facts::probe(&FakeHost::new()).unwrap(),
        };
        let _ = reg.modules[0].render(&state, &ctx); // 不 panic 即证明锁未被 poison
        assert!(reg.manifest.read().unwrap().is_some());
    }

    /// 2026-09-12 真机：`--import-v3` 时 ipify 返回空串 → `node.public_ip` 留空，之后没人再补。
    /// 守护进程启动时补一次；已有值就一次探测都不发。
    #[tokio::test]
    async fn serve_backfills_an_empty_public_ip_on_startup() {
        let d = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            // 前两个源不灵（真机形态：ipify 回空串），第三个才给出答案
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", crate::sys::IP_PROBE_URLS[0]),
                CmdOut::success(""),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", crate::sys::IP_PROBE_URLS[1]),
                CmdOut::failure(7, "couldn't connect"),
            ));
            i.scripted.push((
                format!("curl -sS --max-time 5 {}", crate::sys::IP_PROBE_URLS[2]),
                CmdOut::success("198.51.100.7\n"),
            ));
        });
        let mut empty = crate::testutil::sample_state();
        empty.node.public_ip = String::new();
        let path = d.path().join("state.json");
        let store = Store::create(&path, empty).await.unwrap();
        backfill_public_ip(&store, host.clone()).await;
        assert_eq!(store.read().await.node.public_ip, "198.51.100.7");
        // 写回了磁盘，不只是内存缓存
        let on_disk: State = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.node.public_ip, "198.51.100.7");
        // 已有值时：一次探测都不发
        host.clear_ops();
        backfill_public_ip(&store, host.clone()).await;
        assert_eq!(host.ops(), Vec::<String>::new(), "{:?}", host.ops());
    }

    /// 探测全失败不许阻塞启动，也不许把空串写进 state（那是一次无谓的备份 + 写盘）。
    #[tokio::test]
    async fn a_failed_public_ip_probe_does_not_block_startup() {
        let d = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::new());
        host.with(|i| i.scripted.push(("curl".into(), CmdOut::failure(6, "dns"))));
        let mut empty = crate::testutil::sample_state();
        empty.node.public_ip = String::new();
        let path = d.path().join("state.json");
        let store = Store::create(&path, empty).await.unwrap();
        backfill_public_ip(&store, host.clone()).await;
        assert_eq!(store.read().await.node.public_ip, "");
        assert!(
            !d.path().join("state.backups").exists(),
            "没探到就别写盘（写盘会顺带备份一份）"
        );
    }
}
