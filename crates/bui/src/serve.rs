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
}

pub fn modules(manifest: Option<Manifest>) -> Registry {
    let core = CoreFilesModule::new(manifest);
    let handle = core.manifest_handle();
    Registry {
        modules: vec![
            Arc::new(core),
            Arc::new(UnitsModule),
            Arc::new(SystemModule),
            Arc::new(SshModule),
            Arc::new(CertsModule),
            Arc::new(WatchdogModule),
            // P2 在此追加 UsersModule（面板 API + 采样 + auth 快照）
            // P3 在此追加 ResidentialModule（体检 + 健康切换 + 黑名单）
        ],
        manifest: handle,
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
        let specs: Vec<String> = crate::modules::system::firewall_ports(&input.state.node.ports)
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
    Ok(report)
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
            Ok(Event::RelayRestarted) => continue,
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

/// 每日自检：延 [`jitter_secs`] 秒后每 24h 拉一次 manifest → 写 `<base>/manifest.json`
/// → 刷新共享 manifest 句柄 → 记录可升级版本 → 请求一次对账（内核随 manifest 升级）。
///
/// 只刷内核、**不自动换 bui 自己**：`bui upgrade` 仍由面板 / CLI 手动触发。
pub async fn selfcheck_loop(
    ctx: DaemonCtx,
    manifest: Arc<RwLock<Option<Manifest>>>,
    fetcher: Arc<dyn Fetcher>,
    url: String,
) {
    let node_id = ctx.store.read().await.node.id;
    // 按节点 id 定死的抖动，避免所有机器同一秒打 GitHub（spec §7）
    tokio::time::sleep(std::time::Duration::from_secs(jitter_secs(node_id))).await;
    loop {
        let f = fetcher.clone();
        let u = url.clone();
        match tokio::task::spawn_blocking(move || Manifest::from_url(f.as_ref(), &u)).await {
            Ok(Ok(m)) => {
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
                let newer = (m.version != env!("CARGO_PKG_VERSION")).then(|| m.version.clone());
                if let Ok(mut guard) = manifest.write() {
                    *guard = Some(m);
                }
                ctx.runtime
                    .update(|r| r.upgrade_available = newer.clone())
                    .await;
                if let Some(v) = &newer {
                    tracing::info!(version = %v, "每日自检：有新版 bui，可运行 `b-ui upgrade`");
                }
                // 内核版本随 manifest 走：请求一次对账（不 force）
                ctx.bus.send(Event::ReconcileRequested { force: false });
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

/// `bui serve`：装好 Router 与全部后台任务，监听面板 HTTP 与 unix socket。
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()> {
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let bus = EventBus::new();
    let cached = {
        let h = host.clone();
        let p = paths.clone();
        tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p)).await?
    };
    let reg = modules(cached);
    let mods = reg.modules.clone();
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
    // 启动时先对账一次，再拉起后台任务
    match reconcile_from_ctx(&ctx, &mods, fetcher.clone(), false, false).await {
        Ok(r) => {
            tracing::info!(
                changed = r.changed.len(),
                restarted = r.restarted.len(),
                "启动对账完成"
            );
            // 报告已落盘，这时才允许重启自己（B5）
            finish_self_restart(&ctx, &r, true).await;
        }
        Err(e) => tracing::error!(error = %e, "启动对账失败"),
    }
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for m in &mods {
        tasks.extend(m.spawn(ctx.clone()));
    }
    // 守护进程里**只有这一个** consumer 会调 reconcile_from_ctx（启动那一轮在它之前、串行跑完）：
    // 去抖触发、10 分钟巡检、每日自检（经 bus → 去抖）全部经这条 mpsc 排队，天然互斥。
    // 若让 10 分钟 tick 自己起一个任务直接对账，就会与去抖触发的那一轮并发——两轮同时
    // restart 同一个单元、`.verify/<file>` 候选文件互相覆盖、`runtime.json` 交叉写。
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    tasks.push(tokio::spawn(debounce_loop(bus.clone(), tx.clone())));
    {
        let (ctx2, mods2, f2) = (ctx.clone(), mods.clone(), fetcher.clone());
        tasks.push(tokio::spawn(async move {
            while let Some(force) = rx.recv().await {
                match reconcile_from_ctx(&ctx2, &mods2, f2.clone(), force, false).await {
                    Ok(r) => finish_self_restart(&ctx2, &r, true).await,
                    Err(e) => tracing::error!(error = %e, "对账失败"),
                }
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
        // 总纲 C4：`$BUI_MANIFEST_URL` 可覆盖（M1 时内置 URL 必然 404，演练/装机都靠它）
        crate::kernels::manifest_url(None, None),
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
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    /// 六个受管单元都 enable + active。这一步是必需的，而且 `b-ui` 自己那一条最关键：
    /// `UnitsModule::render` 给六个单元各产一条 `UnitState{true,true}`，而 apply 第 12 步
    /// 有意不 start/restart `b-ui`（不许在对账中途杀掉自己）。真机上守护进程正在跑，
    /// `unit_is_active("b-ui")` 为真、diff 不出这一条；假机器上不播种就永远差这一条，
    /// 于是「二次对账零变更」恒 FAIL。
    fn mark_units_up(i: &mut crate::sys::fake::FakeInner) {
        for u in crate::reconcile::MANAGED_UNITS {
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
            mark_units_up(i);
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
            mark_units_up(i);
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
        let manifest_json = serde_json::json!({
            "version": "4.0.1",
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
            "https://x/manifest.json".to_string(),
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
            Some("4.0.1".to_string()),
            "共享句柄要被刷新，否则内核永远跟不上 manifest"
        );
        assert!(
            host.text(d.path().join("manifest.json").to_str().unwrap())
                .is_some(),
            "manifest 要落盘给下次启动用"
        );
        assert_eq!(
            ctx.runtime.read().await.upgrade_available.as_deref(),
            Some("4.0.1")
        );
        assert_eq!(
            events.recv().await.unwrap(),
            Event::ReconcileRequested { force: false }
        );
        task.abort();
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
        assert_eq!(
            names,
            vec!["core-files", "units", "system", "ssh", "certs", "watchdog"]
        );
        *reg.manifest.write().unwrap() = Some(Manifest {
            version: "4.0.1".into(),
            kernels: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            min_upgrade_from: None,
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
}
