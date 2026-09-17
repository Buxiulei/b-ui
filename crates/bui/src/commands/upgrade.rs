//! `bui upgrade` / `bui upgrade --rollback`（spec §7）。
//!
//! manifest（总纲 C4）是版本的唯一来源：`m.version`（版本号相同时再比 bui 资产的 sha256，
//! 见 [`crate::kernels::bui_build_differs`]）决定要不要换 `bui` 自己，`m.kernels` 决定四个
//! 内核；资产一律按架构从 `m.artifacts` 查，没有 Rust target 三元组那一层。
//!
//! 下载全部经 [`Fetcher`]，而 [`crate::kernels::HttpFetcher`] 用的是 `reqwest::blocking`，
//! 在 async 上下文里会 panic——所以本文件的每个下载调用点都在 `tokio::task::spawn_blocking` 里。

use crate::kernels::{Asset, Fetcher, HttpFetcher, Manifest};
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 每日自检的抖动实现在 `serve.rs`（那边不能反向依赖本模块），这里再导出一层，
/// 让 CLI 侧也有一处稳定入口。
///
/// 计划里写的是 `pub use crate::serve::jitter_secs;`，但 `bui` 是 bin-only crate：`pub use` 出不了
/// crate，非 test 构建里没人用它就是 `unused_imports`（与 dead_code 分属两条 lint，`-D warnings`
/// 下直接失败）。改成等价的转调函数，测试断言的「两条路径行为一致」照旧成立。
pub fn jitter_secs(node_id: uuid::Uuid) -> u64 {
    crate::serve::jitter_secs(node_id)
}

/// 一次升级要做的事：换不换 `bui` 自己 + 哪些内核要换版本。
#[derive(Debug, PartialEq)]
pub struct UpgradePlan {
    pub self_from: String,
    pub self_to: Option<String>,
    /// (二进制名, 现装版本, 目标版本)
    pub kernels: Vec<(String, String, String)>,
}

/// 点分版本号比较（只比数字段；段数不等时缺位当 0，非数字段当 0）。不引新依赖。
pub fn version_lt(a: &str, b: &str) -> bool {
    let seg = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (x, y) = (seg(a), seg(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (
            x.get(i).copied().unwrap_or(0),
            y.get(i).copied().unwrap_or(0),
        );
        if l != r {
            return l < r;
        }
    }
    false
}

/// 无参 `bui upgrade` 撞上降级时的专属错误：`main` 据它打印一行并以**退出码 2** 结束
/// （`install` 的自检 FAIL 同一口径）；`bui menu` 的「检查升级」只当普通错误打印一行，
/// 不会被 `exit` 带走。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DowngradeRefused(pub String);

/// 降级守卫（事故 2026-09-15，复盘在 [`crate::kernels`] 模块头）：`releases/latest` 不含预发布，
/// 所以预发布机器上**无参** `bui upgrade` 解析到的就是一份更旧的稳定版 manifest。那是降级，
/// 必须由操作者显式点名（`--version` / `--manifest-url`），不许当成「升级」默默执行。
///
/// 判据与每日自检同一套 rank：目标 rank 严格低于本机当前 rank
/// （[`crate::kernels::installed_rank`]：manifest 缓存，缺失时取运行中的 bui 版本 + 稳定版）
/// 就拒绝。任一边的 rank 认不出来就放行 —— 不知道不拦。
pub fn refuse_downgrade(
    cached: Option<&Manifest>,
    target: &Manifest,
    running_bui: &str,
) -> anyhow::Result<()> {
    let (Some(to), Some(from)) = (
        crate::kernels::manifest_rank(target),
        crate::kernels::installed_rank(cached, running_bui),
    ) else {
        return Ok(()); // 有一边的 rank 认不出来：不知道不拦
    };
    if to >= from {
        return Ok(());
    }
    // 人读的标签优先用 tag（`v4.0.1-rc1` 才看得出是预发布），没有 tag 就给版本号补个 v
    let label = |version: &str, tag: Option<&str>| {
        tag.filter(|t| !t.is_empty())
            .map_or_else(|| format!("v{version}"), str::to_string)
    };
    let now = cached.map_or_else(
        || label(running_bui, None),
        |m| label(&m.version, m.tag.as_deref()),
    );
    Err(DowngradeRefused(format!(
        "目标版本低于当前（{now} → {}），这是降级；确认请加 --version 或 --manifest-url 显式指定",
        label(&target.version, target.tag.as_deref())
    ))
    .into())
}

/// 比对 manifest 与现装版本，产出升级计划。
///
/// `bui` 自己的判据不止版本号：版本相同但 manifest 里 `bui-linux-<arch>` 的 sha256 与盘上
/// `bin/bui` 的不同（rc 通道的同版本重建）也要换，详见
/// [`crate::kernels::bui_build_differs`]。四个内核同一判法
/// （[`crate::kernels::kernel_build_differs`]）：自建 sing-box 与官方归档同版本号，
/// 只比版本号的话它永远装不上去。
///
/// 顺带消费总纲 C4 的可选字段 `min_upgrade_from`：当前版本低于它就直接报错，
/// 提示先升到那个中间版本（消费方规则在 P1，不在 P5）。
pub fn plan_upgrade(
    host: &dyn Host,
    bin_dir: &Path,
    m: &Manifest,
    current_bui: &str,
    installed: &BTreeMap<String, String>,
    arch: &str,
) -> anyhow::Result<UpgradePlan> {
    if let Some(min) = m.min_upgrade_from.as_deref() {
        if version_lt(current_bui, min) {
            anyhow::bail!(
                "当前 {current_bui} 低于 manifest 要求的 min_upgrade_from {min}：请先 `bui upgrade --version {min}`，再升到 {}",
                m.version
            );
        }
    }
    // 资产缺失就当场报错，别等下载才发现
    let _ = m.bui_asset(arch)?;
    let self_to = crate::kernels::bui_build_differs(host, bin_dir, m, current_bui, arch)
        .then(|| m.version.clone());
    let mut kernels = Vec::new();
    for name in crate::kernels::KERNELS {
        // `kernel_asset` 同时给出 `kernels` 表里的版本（键是下划线的 `sing_box`）与本架构的
        // `artifacts` 条目；缺哪一样都没法装，跳过（对账侧同样只 warn 跳过）。
        let Ok((want, asset)) = m.kernel_asset(name, arch) else {
            continue;
        };
        if crate::kernels::kernel_build_differs(host, bin_dir, installed, name, want, &asset.sha256)
        {
            let from = installed.get(name).cloned().unwrap_or_default();
            kernels.push((name.to_string(), from, want.to_string()));
        }
    }
    Ok(UpgradePlan {
        self_from: current_bui.to_string(),
        self_to,
        kernels,
    })
}

/// 下载 → sha256 → 旧版另存 `bin/bui.prev` → 写 `bin/bui`。校验失败不动现装二进制。
pub fn apply_self(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    asset: &Asset,
    bin_dir: &Path,
) -> anyhow::Result<()> {
    let bytes = fetcher.get_bytes(&asset.url)?;
    let got = crate::kernels::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        anyhow::bail!(
            "bui 二进制 sha256 不匹配：期望 {}，实际 {got}",
            asset.sha256
        );
    }
    let current = bin_dir.join("bui");
    if let Some(old) = host.read_file(&current)? {
        host.write_file(&bin_dir.join("bui.prev"), &old, 0o755)?;
    }
    host.write_file(&current, &bytes, 0o755)?;
    Ok(())
}

/// `bin/<kernel>.prev`：内核二进制的上一版快照（`bin/bui.prev` 由 [`apply_self`] 写）。
fn kernel_prev(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(format!("{name}.prev"))
}

/// 住宅 HY2 那个单元名（4.1 = sing-box，4.0 = 槽 0 的 apernet；名字两代相同，spec §2.5）。
/// [`rollback`] 停它、并在恢复 sing-box 时跳过它的 restart。
const RESI_UNIT: &str = "hysteria-residential";

/// 升级前的快照：`manifest.json` → `manifest.prev.json`，四个内核 → 各自 `.prev`。
///
/// 由 [`prepare`] 在**算出计划、确认真有东西要换之后、写新 manifest 缓存与替换内核之前**调：
/// 早了快照里存的就是新版，无替换时动它会把上一版快照冲掉。缺的文件（全新装机、某个内核还没
/// 装）一概跳过；已有的 `.prev` 直接覆盖——回滚只回一步，不留上上一版。
///
/// 裁决（2026-09-11 v4-master「P1：bui upgrade --rollback 范围」）：`--rollback` 要能连内核
/// 一起回退，而内核版本是 manifest 缓存说了算，所以两样必须一起快照、一起恢复。
pub fn snapshot_prev(host: &dyn Host, paths: &Paths) -> anyhow::Result<()> {
    if let Some(bytes) = host.read_file(&crate::paths::manifest_file(paths))? {
        host.write_file(&crate::paths::manifest_prev_file(paths), &bytes, 0o644)?;
    }
    for name in crate::kernels::KERNELS {
        if let Some(bytes) = host.read_file(&paths.bin_dir.join(name))? {
            host.write_file(&kernel_prev(&paths.bin_dir, name), &bytes, 0o755)?;
        }
    }
    Ok(())
}

/// 「要不要换 + 换之前先留快照」一步到位：先算计划，**只在真有东西要换时**才快照并写新的
/// manifest 缓存。
///
/// 顺序是裁决的要求，不是风格问题：
/// - `plan_upgrade` 会 bail（`min_upgrade_from`），bail 之后一个字都不该落盘，否则留下一份
///   「新版 manifest 缓存 + 旧内核」的不可用状态；
/// - 已是最新（菜单里的「检查升级」那一下）时也绝不能碰 `.prev`，否则升级过一次后再跑一次
///   就把五个 `.prev` 与 `manifest.prev.json` 全覆盖成**当前**版，而 `bin/bui.prev` 仍是上一版
///   （它只由 [`apply_self`] 写）⇒ `--rollback` 只退 bui 不退内核，正是裁决要消灭的混合态。
pub fn prepare(
    host: &dyn Host,
    paths: &Paths,
    m: &Manifest,
    current_bui: &str,
    arch: &str,
) -> anyhow::Result<(UpgradePlan, Asset)> {
    let installed = crate::kernels::installed_versions(host, &paths.bin_dir);
    let plan = plan_upgrade(host, &paths.bin_dir, m, current_bui, &installed, arch)?;
    let asset = m.bui_asset(arch)?.clone();
    if plan.self_to.is_some() || !plan.kernels.is_empty() {
        // 换任何东西之前先留一份回滚快照（manifest 缓存 + 四个内核）：`--rollback` 靠它
        snapshot_prev(host, paths)?;
        // 缓存给对账用：内核版本随 manifest 落地
        let bytes = serde_json::to_vec_pretty(m)?;
        host.write_file(&crate::paths::manifest_file(paths), &bytes, 0o644)?;
    }
    Ok((plan, asset))
}

/// **先停 `hysteria-residential` 并删掉 `table inet bui`**（spec §9.1），再
/// `bin/bui.prev` → `bin/bui`、四个内核 ← 各自 `.prev`、`manifest.json` ←
/// `manifest.prev.json`、`state.json` ← 最近一份备份，最后重启 `b-ui`（重启后守护进程自己
/// 对账）。
///
/// 前两步的顺序不能换、也不能省（4.1 → 4.0.1 的回滚判据）：4.1 的住宅 HY2 是一个
/// sing-box 入站 `:40000` + 那张表把整段 `41000-50000`（开着兼容段时还有 `40001-40007`）
/// REDIRECT 进去。表留着回到 4.0.1，整段就全被 REDIRECT 到槽 0 的 apernet 实例 ——
/// 全体住宅用户从槽 0 那个 IP 出去、`40000+i` 无人应答，比 2026-09-15 那次切片跨进程的
/// 回归事故更糟；反过来先删表后停单元，那一瞬 sing-box 还独占着 `:40000`。
/// 停完**不再重启它**：`units_for_binary("sing-box")` 含 `hysteria-residential`（P-D），
/// 恢复 sing-box 时若跟着 restart 一遍，4.1 的单元又起来、在没有表的情况下只听单端口，
/// 得等 4.0.1 的下一轮对账才改回按槽的 apernet —— 那一段时间住宅整段仍然不通。
/// 起回来的活交给恢复后的 `b-ui`：它重启后第一轮对账重渲染
/// `config-residential*.yaml` 与单元，再把它启起来。
///
/// 恢复后的 manifest 与恢复后的内核二进制是同一版，对账时 Binary 的比对（实际探测版本 vs
/// manifest 记的版本）必然相等 ⇒ 零变更、不下载。正因为零变更，对账不会替我们重启内核单元，
/// 所以每恢复一个内核就自己 restart 它的单元（`hysteria` 两个）。缺 `.prev` 的内核只记一条
/// note 跳过、不碰它的单元；连 `manifest.prev.json` 都没有时**删掉**当前缓存，否则对账会照新版
/// manifest 把内核又拉上去。
///
/// 边界（记在这里以免误判）：`.prev` 只有显式 `bui upgrade` 会留。守护进程每日自检
/// （`serve.rs` 的 `selfcheck_loop`：拉 manifest → 写缓存 → 对账）换内核不经本模块、不写 `.prev`，
/// 所以 `--rollback` 能退到的是「最近一次显式 `bui upgrade` 之前」，不是「任意一次换内核之前」。
pub fn rollback(host: &dyn Host, paths: &Paths) -> anyhow::Result<Vec<String>> {
    let prev = paths.bin_dir.join("bui.prev");
    let bytes = host
        .read_file(&prev)?
        .ok_or_else(|| anyhow::anyhow!("没有 {}，无法回滚二进制", prev.display()))?;
    // 第 0 步（spec §9.1）：停住宅单元 → 删 nft 表。两步都只记 note，失败不中断回滚。
    let mut done = Vec::new();
    let _ = host.systemd("stop", RESI_UNIT);
    done.push(format!(
        "已停 {RESI_UNIT}（回滚后由恢复的 bui 对账重渲染为 4.0 的 apernet 实例并启回来）"
    ));
    match crate::commands::nft::delete(host, paths) {
        Ok(line) => done.push(format!("nft 表：{line}")),
        Err(e) => done.push(format!("nft 表删除失败（回滚继续）：{e}")),
    }
    host.write_file(&paths.bin_dir.join("bui"), &bytes, 0o755)?;
    done.push(format!("已恢复上一版 bui（{}）", prev.display()));
    for name in crate::kernels::KERNELS {
        let prev = kernel_prev(&paths.bin_dir, name);
        match host.read_file(&prev)? {
            Some(bytes) => {
                host.write_file(&paths.bin_dir.join(name), &bytes, 0o755)?;
                done.push(format!("已恢复上一版 {name}（{}）", prev.display()));
                // 盘上换回旧字节 ≠ 回滚生效：单元还在内存里跑新版二进制，而恢复后的 manifest
                // 与盘上的版本、sha 都一致 ⇒ 对账零变更 ⇒ apply 第 0 步（只在 InstallBinary
                // 成功时才收单元）不会替我们重启。所以这里自己重启；跳过的内核不碰它的单元。
                for unit in crate::reconcile::apply::units_for_binary(name) {
                    // 住宅单元上面已经停了，这里不许再 restart 一遍（见函数文档末段）
                    if unit.name == RESI_UNIT {
                        continue;
                    }
                    let _ = host.systemd("restart", &unit.name);
                    done.push(format!("已重启 {}", unit.name));
                }
            }
            None => done.push(format!("没有 {}，{name} 保持现状", prev.display())),
        }
    }
    let manifest = crate::paths::manifest_file(paths);
    let manifest_prev = crate::paths::manifest_prev_file(paths);
    match host.read_file(&manifest_prev)? {
        Some(bytes) => {
            host.write_file(&manifest, &bytes, 0o644)?;
            done.push(format!(
                "已恢复上一版 manifest（{}）",
                manifest_prev.display()
            ));
        }
        None if host.read_file(&manifest)?.is_some() => {
            host.remove_file(&manifest)?;
            done.push(format!(
                "没有 {}，已删掉 {}（免得对账又按新版装内核）",
                manifest_prev.display(),
                manifest.display()
            ));
        }
        None => {}
    }
    // 备份名是 `state-<stamp>-<nnn>.json`（零填充，Task 2）⇒ 字典序 == 时间序，而 `list_dir`
    // 的契约就是「直接子项、按路径名升序」⇒ 最后一项就是最近一份。目录不存在时它给空表。
    if let Some(newest) = host.list_dir(&crate::paths::backups_dir(paths))?.pop() {
        let bytes = host
            .read_file(&newest)?
            .ok_or_else(|| anyhow::anyhow!("读取备份 {} 失败", newest.display()))?;
        host.write_file(&crate::paths::state_file(paths), &bytes, 0o600)?;
        done.push(format!("已恢复期望态备份 {}", newest.display()));
    }
    let _ = host.systemd("restart", "b-ui");
    done.push("已重启 b-ui".into());
    Ok(done)
}

/// 总纲 C5：`bui upgrade [--version <x.y.z>] [--manifest-url <url|file>] [--rollback]`。
pub async fn run(
    rollback_flag: bool,
    version: Option<String>,
    manifest_url: Option<String>,
    paths: Paths,
    host: Arc<dyn Host>,
) -> anyhow::Result<()> {
    run_with(
        rollback_flag,
        version,
        manifest_url,
        paths,
        host,
        Arc::new(HttpFetcher::new()),
    )
    .await
}

/// 注入 `fetcher` 的版本；每个下载调用点都在 `spawn_blocking` 里。
pub async fn run_with(
    rollback_flag: bool,
    version: Option<String>,
    manifest_url: Option<String>,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
) -> anyhow::Result<()> {
    if rollback_flag {
        let (h, p) = (host.clone(), paths.clone());
        let done = tokio::task::spawn_blocking(move || rollback(h.as_ref(), &p)).await??;
        for line in done {
            println!("{line}");
        }
        return Ok(());
    }
    let (h, f, p) = (host.clone(), fetcher.clone(), paths.clone());
    let want = version.clone();
    let cli = manifest_url.clone();
    let (plan, asset) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        // 总纲 C4 的解析顺序在 kernels 里一处实现：--manifest-url > --version（模板）
        // > $BUI_MANIFEST_URL > latest；什么都没指定且 latest 404 时无条件改跟 releases
        // 列表里最新的 rc（仓库里还没有正式版）
        let env = std::env::var(crate::kernels::MANIFEST_URL_ENV).ok();
        let (_, m) = crate::kernels::fetch_manifest_with(
            f.as_ref(),
            cli.as_deref(),
            want.as_deref(),
            env.as_deref(),
        )?;
        if let Some(v) = want.as_deref() {
            // 指定了版本就必须拿到那一版（本地文件或 $BUI_MANIFEST_URL 里可能是别的版本）
            anyhow::ensure!(
                m.version == v.trim_start_matches('v'),
                "manifest 里是 {}，不是请求的 {v}",
                m.version
            );
        }
        // 三个覆盖都没给（= 跟着 latest 走）时才守降级：显式点名的目标是操作者意图
        // （含 M5 演练故意装旧版），照办不拦
        if crate::kernels::nothing_specified(cli.as_deref(), want.as_deref(), env.as_deref()) {
            refuse_downgrade(
                crate::serve::load_cached_manifest(h.as_ref(), &p).as_ref(),
                &m,
                env!("CARGO_PKG_VERSION"),
            )?;
        }
        // 4.1 的硬前置（2026-09-16 裁决）：缺 `nft`（或内核低于 inet nat 要求的 5.2）
        // 就**硬性拒绝**升级 —— 退出码 2、一个字不落盘、不装系统包。判据与文案同
        // `bui install` 的 `env.blocking()` 共用 `env_probe::nft_blocking`。
        // 位置在 `prepare` 之前：那之后就会写 `.prev` 快照与新 manifest 缓存。
        //
        // **注意这道闸门拦不住 4.0.1 → 4.1 那一跳**：`bui upgrade` 由**旧**二进制执行，
        // 旧的那版没有这段代码。那一跳由 T18 的演练 preflight 与 T19 闸门 5 守。
        if let Some(msg) = crate::sys::env_probe::nft_blocking_for(h.as_ref()) {
            return Err(crate::sys::env_probe::EnvBlocked(msg).into());
        }
        let arch = h.arch()?;
        // 计划先算、快照与新缓存只在真要换东西时才写（见 `prepare` 的说明）
        prepare(h.as_ref(), &p, &m, env!("CARGO_PKG_VERSION"), &arch)
    })
    .await??;
    println!("{}", format_plan(&plan));
    if plan.self_to.is_none() && plan.kernels.is_empty() {
        return Ok(());
    }
    if plan.self_to.is_some() {
        let (h, f, bin) = (host.clone(), fetcher.clone(), paths.bin_dir.clone());
        tokio::task::spawn_blocking(move || apply_self(h.as_ref(), f.as_ref(), &asset, &bin))
            .await??;
    }
    // 守护进程重启后自己会对账（内核随 manifest 落地）；它没在跑就本进程内跑一次
    let socket = PathBuf::from(crate::paths::SOCKET_PATH);
    if crate::ipc::Client::new(&socket).available().await {
        let h = host.clone();
        let _ = tokio::task::spawn_blocking(move || h.systemd("restart", "b-ui")).await;
        println!("已重启 b-ui，升级后的对账由守护进程完成");
        return Ok(());
    }
    println!("守护进程未运行，改为本进程内对账一次");
    crate::serve::reconcile_cli(paths, host, socket, false, false).await
}

/// 升级计划的人读文本。
pub fn format_plan(p: &UpgradePlan) -> String {
    let mut out = Vec::new();
    match &p.self_to {
        // 版本号一样但 sha256 不一样：rc 通道的同版本重建（rc1 → rc2 → 正式版都是同一个版本号）
        Some(to) if *to == p.self_from => {
            out.push(format!("bui          {to}（同版本的新构建，rc 通道）"))
        }
        Some(to) => out.push(format!("bui          {} → {to}", p.self_from)),
        None => out.push(format!("bui          {}（已最新）", p.self_from)),
    }
    for (name, from, to) in &p.kernels {
        // 内核也有「同版本的另一份构建」：自建 sing-box 与官方归档打同一个版本号，判据是
        // 资产 sha256（`kernel_build_differs`）。不特判就会打印「1.14.1 → 1.14.1」。
        if from == to {
            out.push(format!("{name:<12} {to}（同版本的新构建）"));
            continue;
        }
        let from = if from.is_empty() {
            "（未装）"
        } else {
            from
        };
        out.push(format!("{name:<12} {from} → {to}"));
    }
    if p.self_to.is_none() && p.kernels.is_empty() {
        out.push("无需升级".into());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct F(Mutex<Vec<(String, Vec<u8>)>>);
    impl F {
        fn take(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }
    impl crate::kernels::Fetcher for F {
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

    /// `plan_upgrade` 直接单测用的 `bin/` 目录（与 `apply_self` 的单测同一口径）。
    const BIN: &str = "/opt/b-ui/bin";

    /// 盘上装着一份 `bin/bui` 的假机器（内容即构建指纹，sha256 由它算）。
    fn host_with_bui(body: &[u8]) -> FakeHost {
        let h = FakeHost::new();
        h.write_file(&std::path::Path::new(BIN).join("bui"), body, 0o755)
            .unwrap();
        h
    }

    /// 总纲 C4 形状；只放 amd64 资产，用来顺带测「缺架构资产直接报错」
    fn manifest(bui: &str, sb: &str, sum: &str) -> Manifest {
        let a = |u: &str| Asset {
            url: u.into(),
            sha256: sum.into(),
        };
        Manifest {
            version: bui.into(),
            kernels: BTreeMap::from([("sing_box".to_string(), sb.to_string())]),
            artifacts: BTreeMap::from([
                ("bui-linux-amd64".to_string(), a("https://x/bui")),
                ("sing-box-linux-amd64".to_string(), a("https://x/sb")),
            ]),
            min_upgrade_from: None,
            tag: None,
        }
    }

    #[test]
    fn bui_asset_is_keyed_by_arch_not_by_target_triple() {
        let m = manifest("4.0.1", "1.13.19", "00");
        assert_eq!(m.bui_asset("x86_64").unwrap().url, "https://x/bui");
        assert!(
            m.bui_asset("aarch64").is_err(),
            "manifest 没放 arm64 资产就该当场报错"
        );
        assert!(m.bui_asset("armv7l").is_err(), "架构本身不支持");
        assert!(
            plan_upgrade(
                &host_with_bui(b"BUI"),
                Path::new(BIN),
                &m,
                "4.0.0",
                &BTreeMap::new(),
                "aarch64"
            )
            .is_err(),
            "缺资产不许出计划"
        );
    }

    /// 事故回归（2026-09-15）：rc1 机器上无参 `bui upgrade` 从 `releases/latest`（不含预发布）
    /// 拿到的是更旧的稳定版 4.0.0。那是降级，必须当场拒绝并说清「这是降级」。
    #[test]
    fn a_no_argument_upgrade_refuses_a_downgrade_and_says_so() {
        let ranked = |v: &str, tag: &str| Manifest {
            version: v.into(),
            tag: Some(tag.into()),
            ..Default::default()
        };
        let rc1 = ranked("4.0.1", "v4.0.1-rc1");
        let stable400 = ranked("4.0.0", "v4.0.0");
        let err = refuse_downgrade(Some(&rc1), &stable400, "4.0.1").unwrap_err();
        assert!(err.is::<DowngradeRefused>(), "main 靠这个类型给退出码 2");
        let msg = err.to_string();
        assert!(msg.contains("这是降级"), "{msg}");
        assert!(msg.contains("v4.0.1-rc1 → v4.0.0"), "{msg}");
        assert!(msg.contains("--manifest-url"), "{msg}");
        // 同一版、下一个 rc、同版本的正式版都放行
        assert!(refuse_downgrade(Some(&rc1), &rc1, "4.0.1").is_ok());
        assert!(refuse_downgrade(Some(&rc1), &ranked("4.0.1", "v4.0.1-rc2"), "4.0.1").is_ok());
        assert!(refuse_downgrade(Some(&rc1), &ranked("4.0.1", "v4.0.1"), "4.0.1").is_ok());
        // 没有缓存时按运行中的 bui 版本比
        assert!(refuse_downgrade(None, &stable400, "4.0.1").is_err());
        assert!(refuse_downgrade(None, &stable400, "4.0.0").is_ok());
        // rank 认不出来就不拦
        assert!(refuse_downgrade(Some(&rc1), &ranked("4.0", "v4.0"), "4.0.1").is_ok());
    }

    /// 同一条路走整遍：无参 `bui upgrade` 解析到更旧的 manifest ⇒ 返回 [`DowngradeRefused`]
    /// （`main` 据此给退出码 2），而且**一个字都不落盘** —— 不许留下「新缓存 + 旧内核」，
    /// 也不许覆盖 `.prev`。
    ///
    /// 用例假定进程环境里没有 `$BUI_MANIFEST_URL`（设了就等于操作者显式指定了源，守卫放行）。
    #[tokio::test]
    async fn run_with_refuses_to_walk_back_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = Arc::new(FakeHost::new());
        // 缓存 = 4.0.1-rc1（一次显式 `bui upgrade --manifest-url` 写下的那一份）
        let cached = r#"{"version":"4.0.1","tag":"v4.0.1-rc1","kernels":{},"artifacts":{}}"#;
        h.write_file(
            &crate::paths::manifest_file(&paths),
            cached.as_bytes(),
            0o644,
        )
        .unwrap();
        h.clear_ops();
        let latest = manifest_json("4.0.0", OLD_KERNELS, "BUI-4.0.0");
        let f: Arc<dyn Fetcher> = Arc::new(F(Mutex::new(vec![(
            crate::kernels::MANIFEST_URL.to_string(),
            latest.into_bytes(),
        )])));
        let host: Arc<dyn Host> = h.clone();
        let err = run_with(false, None, None, paths.clone(), host, f)
            .await
            .unwrap_err();
        assert!(err.is::<DowngradeRefused>(), "{err:#}");
        assert!(err.to_string().contains("这是降级"), "{err}");
        assert_eq!(writes(&h), Vec::<String>::new(), "拒绝之后一个字都不写");
        assert_eq!(
            text(&h, &crate::paths::manifest_file(&paths)).as_deref(),
            Some(cached),
            "缓存必须还是 rc1 那一份"
        );
        assert!(text(&h, &crate::paths::manifest_prev_file(&paths)).is_none());
    }

    /// 4.1 的 nft 硬前置（2026-09-16 裁决）：缺 `nft` ⇒ 专属错误（`main` 据它给退出码 2）、
    /// **一个字都不落盘**（不许留下「新缓存 + 旧内核」，也不许覆盖 `.prev`）。
    ///
    /// 位置在降级守卫**之后**、`prepare` 之前：降级那条是「目标不对」，这条是「机器不够」。
    #[tokio::test]
    async fn run_with_refuses_to_upgrade_without_nft_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            i.files.insert(
                "/etc/os-release".into(),
                (b"ID=debian\nVERSION_ID=\"12\"\n".to_vec(), 0o644),
            );
        });
        h.clear_ops();
        let latest = manifest_json("9.9.9", OLD_KERNELS, "BUI-9.9.9");
        let f: Arc<dyn Fetcher> = Arc::new(F(Mutex::new(vec![(
            crate::kernels::MANIFEST_URL.to_string(),
            latest.into_bytes(),
        )])));
        let host: Arc<dyn Host> = h.clone();
        let err = run_with(false, None, None, paths.clone(), host, f)
            .await
            .unwrap_err();
        assert!(
            err.is::<crate::sys::env_probe::EnvBlocked>(),
            "main 靠这个类型给退出码 2：{err:#}"
        );
        let msg = err.to_string();
        assert!(msg.contains("apt-get install -y nftables"), "{msg}");
        assert!(msg.contains("现役订阅"), "{msg}");
        assert_eq!(writes(&h), Vec::<String>::new(), "拒绝之后一个字都不写");
        assert!(text(&h, &crate::paths::manifest_file(&paths)).is_none());
        assert!(text(&h, &crate::paths::manifest_prev_file(&paths)).is_none());
    }

    #[test]
    fn version_compare_handles_uneven_segments() {
        assert!(version_lt("4.0.0", "4.0.1"));
        assert!(version_lt("4.0", "4.0.1"));
        assert!(version_lt("3.9.9", "4.0.0"));
        assert!(!version_lt("4.0.1", "4.0.1"));
        assert!(!version_lt("4.1.0", "4.0.9"));
        assert!(!version_lt("4.0.10", "4.0.9"), "按数字比，不是字典序");
    }

    #[test]
    fn min_upgrade_from_refuses_to_skip_the_required_intermediate_version() {
        // 总纲 C4 的可选字段：低于此版本必须先升到它。消费方规则在 P1。
        let mut m = manifest("4.2.0", "1.13.19", "00");
        m.min_upgrade_from = Some("4.1.0".into());
        let h = host_with_bui(b"BUI");
        let plan = |m: &Manifest, cur: &str| {
            plan_upgrade(&h, Path::new(BIN), m, cur, &BTreeMap::new(), "x86_64")
        };
        let err = plan(&m, "4.0.0").unwrap_err().to_string();
        assert!(err.contains("4.1.0"), "{err}");
        // 已经到了门槛版本就放行
        assert!(plan(&m, "4.1.0").is_ok());
        // 没有这个字段时一切照旧
        let m2 = manifest("4.2.0", "1.13.19", "00");
        assert!(plan(&m2, "4.0.0").is_ok());
    }

    #[test]
    fn nothing_to_do_when_versions_match() {
        // 「已最新」= 版本相同**且** manifest 里 bui 资产的 sha256 与盘上 bin/bui 一致
        let m = manifest("4.0.0", "1.13.19", &crate::kernels::sha256_hex(b"BUI"));
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let h = host_with_bui(b"BUI");
        // 内核同一判法：版本相同还要 sha 相同才算「已最新」。本 fixture 里 bui 与 sing-box
        // 两个资产共用一个 sum，所以盘上那份 sing-box 的字节也写成同一份。
        h.write_file(&Path::new(BIN).join("sing-box"), b"BUI", 0o755)
            .unwrap();
        let p = plan_upgrade(&h, Path::new(BIN), &m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(
            p,
            UpgradePlan {
                self_from: "4.0.0".into(),
                self_to: None,
                kernels: vec![]
            }
        );
    }

    /// 发布阻断级回归（2026-09-16）：自建 sing-box 与官方归档同版本号，`bui upgrade` 只比
    /// 版本号就会打印「sing-box 1.14.1（已最新）」而永远不换那份二进制。
    #[test]
    fn plan_lists_a_kernel_whose_version_matches_but_whose_asset_sha_differs() {
        // 本 fixture 里 bui 与 sing-box 两个资产共用一个 sum，所以盘上那份 bui 的字节也写成
        // 同一份（本用例要说的只是内核那一半）
        let ours = b"SB-ours";
        let m = manifest("4.0.0", "1.14.1", &crate::kernels::sha256_hex(ours));
        let installed = BTreeMap::from([("sing-box".to_string(), "1.14.1".to_string())]);
        let h = host_with_bui(ours);
        // 盘上是官方那一份：版本号相同、字节不同
        h.write_file(&Path::new(BIN).join("sing-box"), b"SB-official", 0o755)
            .unwrap();
        let p = plan_upgrade(&h, Path::new(BIN), &m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(p.self_to, None, "bui 自己没变");
        assert_eq!(
            p.kernels,
            vec![(
                "sing-box".to_string(),
                "1.14.1".to_string(),
                "1.14.1".to_string()
            )],
            "同版本异 sha 也要列进升级计划"
        );
        assert!(
            format_plan(&p).contains("sing-box     1.14.1（同版本的新构建）"),
            "别打印「1.14.1 → 1.14.1」：{}",
            format_plan(&p)
        );
    }

    #[test]
    fn plan_lists_self_and_kernel_upgrades() {
        let m = manifest("4.0.1", "1.14.2", "00");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let h = host_with_bui(b"BUI");
        let p = plan_upgrade(&h, Path::new(BIN), &m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(p.self_to.as_deref(), Some("4.0.1"));
        assert_eq!(
            p.kernels,
            vec![(
                "sing-box".to_string(),
                "1.13.19".to_string(),
                "1.14.2".to_string()
            )]
        );
    }

    #[test]
    fn apply_self_keeps_the_previous_binary() {
        let payload = b"NEWBUI".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let m = manifest("4.0.1", "1.13.19", &sum);
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), payload)]));
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755)
            .unwrap();
        h.clear_ops();
        apply_self(
            &h,
            &f,
            m.bui_asset("x86_64").unwrap(),
            std::path::Path::new("/opt/b-ui/bin"),
        )
        .unwrap();
        assert_eq!(h.text("/opt/b-ui/bin/bui").as_deref(), Some("NEWBUI"));
        assert_eq!(h.text("/opt/b-ui/bin/bui.prev").as_deref(), Some("OLDBUI"));
        assert_eq!(h.mode("/opt/b-ui/bin/bui"), Some(0o755));
    }

    #[test]
    fn apply_self_refuses_a_bad_checksum() {
        let f = F(Mutex::new(vec![(
            "https://x/bui".to_string(),
            b"NEWBUI".to_vec(),
        )]));
        let m = manifest("4.0.1", "1.13.19", "deadbeef");
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755)
            .unwrap();
        assert!(apply_self(
            &h,
            &f,
            m.bui_asset("x86_64").unwrap(),
            std::path::Path::new("/opt/b-ui/bin")
        )
        .is_err());
        assert_eq!(
            h.text("/opt/b-ui/bin/bui").as_deref(),
            Some("OLDBUI"),
            "校验失败不替换"
        );
    }

    #[test]
    fn rollback_restores_binary_and_the_newest_state_backup() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        // 回滚的每一次读写都经 Host（与 upgrade 的其它路径同一口径），所以夹具全在 FakeHost 里，
        // tempdir 只用来给出一组互不冲突的绝对路径。
        let h = FakeHost::new();
        h.with(|i| {
            for (name, body) in [
                ("state-20260910T000000Z-001.json", b"{\"old\":1}".to_vec()),
                ("state-20260911T000000Z-001.json", b"{\"new\":1}".to_vec()),
            ] {
                i.files
                    .insert(crate::paths::backups_dir(&paths).join(name), (body, 0o600));
            }
            i.files.insert(
                crate::paths::state_file(&paths),
                (b"{\"current\":1}".to_vec(), 0o600),
            );
        });
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755)
            .unwrap();
        h.write_file(&paths.bin_dir.join("bui"), b"NEWBUI", 0o755)
            .unwrap();
        let done = rollback(&h, &paths).unwrap();
        assert_eq!(
            h.text(paths.bin_dir.join("bui").to_str().unwrap())
                .as_deref(),
            Some("OLDBUI")
        );
        let state = crate::paths::state_file(&paths).display().to_string();
        assert_eq!(
            h.text(&state).as_deref(),
            Some("{\"new\":1}"),
            "恢复最近一份备份"
        );
        assert_eq!(h.mode(&state), Some(0o600), "state.json 含秘密，必须 0600");
        assert!(
            !std::fs::exists(crate::paths::state_file(&paths)).unwrap(),
            "不许绕过 Host 直接写真实文件系统"
        );
        assert!(done
            .iter()
            .any(|l| l.contains("state-20260911T000000Z-001.json")));
        assert!(h.ops().contains(&"systemd:restart:b-ui".to_string()));
    }

    /// 回滚夹具：`bui.prev` + 四个内核的 `.prev` 都在盘上，PATH 上有 `nft`。
    fn host_with_prev_binaries(d: &tempfile::TempDir) -> (FakeHost, bui_schema::paths::Paths) {
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755)
            .unwrap();
        h.write_file(&paths.bin_dir.join("bui"), b"NEWBUI", 0o755)
            .unwrap();
        for name in crate::kernels::KERNELS {
            h.write_file(&kernel_prev(&paths.bin_dir, name), b"OLDK", 0o755)
                .unwrap();
        }
        h.with(|i| {
            i.which.insert("nft".into());
        });
        h.clear_ops();
        (h, paths)
    }

    /// 回滚必须**先停住宅单元、再删 nft 表**，而且整趟只停/起它一次（spec §9.1）。
    ///
    /// 表留着会把整段 `41000-50000` 与兼容段 `40001-40007` 全 REDIRECT 进 `:40000`，
    /// 而 4.0.1 在那个端口上跑的是槽 0 的 apernet 实例 —— 全体住宅用户从槽 0 的 IP 出去、
    /// `40000+i` 无人应答，比 2026-09-15 那次回归事故更糟。
    ///
    /// 顺序反了同样致命：先删表、后停单元的那一瞬，sing-box 还在 `:40000` 上接管着整段。
    #[test]
    fn rollback_stops_the_residential_unit_and_deletes_the_nft_table() {
        let d = tempfile::tempdir().unwrap();
        let (h, p) = host_with_prev_binaries(&d);
        let notes = rollback(&h, &p).unwrap();
        let calls = h.ops();
        let stop = calls
            .iter()
            .position(|c| c == "systemd:stop:hysteria-residential")
            .unwrap_or_else(|| panic!("先停单元：{calls:?}"));
        let del = calls
            .iter()
            .position(|c| c.starts_with("run:nft delete table"))
            .unwrap_or_else(|| panic!("再删表：{calls:?}"));
        assert!(stop < del, "顺序错了：{calls:?}");
        // 恢复二进制必须排在这两步之后（表还在的时候换 sing-box 等于把整段交给旧内核）
        let restore = calls
            .iter()
            .position(|c| c.starts_with(&format!("write:{}", p.bin_dir.join("bui").display())))
            .unwrap_or_else(|| panic!("恢复 bui：{calls:?}"));
        assert!(del < restore, "先删表再恢复二进制：{calls:?}");
        // 别重启两次：`units_for_binary("sing-box")` 含 `hysteria-residential`（P-D），
        // 恢复 sing-box 时不许再 restart 它一遍 —— 那会让 4.1 的 sing-box 单元又起来、
        // 在没有表的情况下只听 `:40000`，直到下一轮 4.0.1 的对账才改回 apernet。
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.contains(":hysteria-residential"))
                .collect::<Vec<_>>(),
            vec!["systemd:stop:hysteria-residential"],
            "住宅单元整趟只碰一次：{calls:?}"
        );
        assert!(
            calls.contains(&"systemd:restart:b-ui-relay".to_string()),
            "中继照旧重启：{calls:?}"
        );
        assert!(notes.iter().any(|n| n.contains("nft")), "{notes:?}");
        assert!(
            notes.iter().any(|n| n.contains("hysteria-residential")),
            "停单元要记一行：{notes:?}"
        );
    }

    /// 没有 `nft` 二进制 / 表不存在 ⇒ 只记 note，不算失败（回滚绝不能因此中断）。
    #[test]
    fn a_missing_nft_table_does_not_fail_the_rollback() {
        let d = tempfile::tempdir().unwrap();
        let (h, p) = host_with_prev_binaries(&d);
        h.with(|i| {
            i.which.remove("nft");
        });
        let notes = rollback(&h, &p).unwrap();
        assert!(notes.iter().any(|n| n.contains("nft")), "{notes:?}");
        assert!(
            notes.iter().any(|n| n.contains("已恢复上一版 bui")),
            "缺 nft 不许中断回滚：{notes:?}"
        );
    }

    /// 事故回归（2026-09-12 bwg-rick）：Hysteria2 的 `auth.command` 指的是
    /// `<base>/bin/bui-auth-hook`，而那是一条目标为**相对** `bui` 的符号链接。
    /// 升级与回滚都只是原地换掉 `bin/bui` 这个文件，所以链接天然指向换上来的那一版：
    /// 谁都不需要重建它，反过来谁也不许把它删了。
    ///
    /// 上半段走真实文件系统（相对链接的解析语义正是被测对象，FakeHost 的 symlink 表证明不了
    /// 这一点），只用 `apply_self`——它只读写文件，不碰 systemd；下半段拿 FakeHost 跑真正的
    /// `rollback()`，确认它换完二进制之后链接还在、目标没被改写。
    #[test]
    fn the_auth_hook_symlink_keeps_pointing_at_bui_across_upgrade_and_rollback() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let real = crate::sys::real::RealHost::new();
        let bui = paths.bin_dir.join("bui");
        let hook = paths.auth_hook_bin();
        real.write_file(&bui, b"OLDBUI", 0o755).unwrap();
        real.symlink(std::path::Path::new("bui"), &hook).unwrap();
        assert_eq!(std::fs::read(&hook).unwrap(), b"OLDBUI");

        let payload = b"NEWBUI".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let m = manifest("4.0.1", "1.13.19", &sum);
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), payload)]));
        apply_self(&real, &f, m.bui_asset("x86_64").unwrap(), &paths.bin_dir).unwrap();
        assert_eq!(
            std::fs::read(&hook).unwrap(),
            b"NEWBUI",
            "升级换掉 bin/bui 之后，钩子入口必须跟着走"
        );
        // 回滚那一步也只是把旧字节写回同一个文件名
        real.write_file(&bui, b"OLDBUI", 0o755).unwrap();
        assert_eq!(std::fs::read(&hook).unwrap(), b"OLDBUI");
        assert_eq!(
            real.read_link(&hook).unwrap().as_deref(),
            Some(std::path::Path::new("bui")),
            "目标必须一直是同目录的相对 `bui`，不许被重建成绝对路径"
        );

        // 真正的 rollback()：换完二进制不许动这条链接
        let h = FakeHost::new();
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755)
            .unwrap();
        h.write_file(&bui, b"NEWBUI", 0o755).unwrap();
        h.symlink(std::path::Path::new("bui"), &hook).unwrap();
        rollback(&h, &paths).unwrap();
        assert_eq!(h.text(bui.to_str().unwrap()).as_deref(), Some("OLDBUI"));
        assert_eq!(
            h.read_link(&hook).unwrap().as_deref(),
            Some(std::path::Path::new("bui")),
            "rollback 不许删掉或改写 bin/bui-auth-hook"
        );
    }

    #[test]
    fn rollback_without_a_previous_binary_is_an_explicit_error() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        let err = rollback(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("bui.prev"), "{err}");
    }

    /// 升级演练用的 manifest 缓存（总纲 C4 形状；只放 amd64 资产）。
    ///
    /// `bui_body` 是这份 manifest 所指的 bui 二进制内容（资产 sha256 由它算出）：传盘上现有那
    /// 一份就是「bui 已最新」，传别的就是「同版本的新构建」。四个内核的资产 sha256 同样从
    /// [`kernel_body`] 算——内核判据是「版本 + sha256」，写死的假 sha 会让每个内核恒判「要装」。
    fn manifest_json(bui: &str, kernels: [&str; 4], bui_body: &str) -> String {
        let [hy, xray, sb, caddy] = kernels;
        let bui_sum = crate::kernels::sha256_hex(bui_body.as_bytes());
        let sum = |name: &str, v: &str| crate::kernels::sha256_hex(kernel_body(name, v).as_bytes());
        let (hy_sum, xray_sum) = (sum("hysteria", hy), sum("xray", xray));
        let (sb_sum, caddy_sum) = (sum("sing-box", sb), sum("caddy", caddy));
        format!(
            r#"{{"version":"{bui}",
  "kernels":{{"hysteria":"{hy}","xray":"{xray}","sing_box":"{sb}","caddy":"{caddy}"}},
  "artifacts":{{
    "bui-linux-amd64":      {{"url":"https://x/bui","sha256":"{bui_sum}"}},
    "hysteria-linux-amd64": {{"url":"https://x/hy","sha256":"{hy_sum}"}},
    "xray-linux-amd64":     {{"url":"https://x/xray","sha256":"{xray_sum}"}},
    "sing-box-linux-amd64": {{"url":"https://x/sb","sha256":"{sb_sum}"}},
    "caddy-linux-amd64":    {{"url":"https://x/caddy","sha256":"{caddy_sum}"}}
  }}}}"#
        )
    }

    /// 「`bin/<name>` 装的是这一版」时它的字节（内容当版本指纹用）：`OLD_BINS` / `NEW_BINS`
    /// 与 [`manifest_json`] 的资产 sha256 都按这一份口径生成，两边才对得上。
    fn kernel_body(name: &str, v: &str) -> String {
        match name {
            "hysteria" => format!("HY-{v}"),
            "xray" => format!("XRAY-{v}"),
            "sing-box" => format!("SB-{v}"),
            _ => format!("CADDY-{v}"),
        }
    }

    const OLD_KERNELS: [&str; 4] = ["2.12.2", "26.3.27", "1.13.19", "2.10.2"];
    const NEW_KERNELS: [&str; 4] = ["2.13.0", "26.4.0", "1.14.2", "2.11.0"];
    /// 升级前盘上的五个二进制（内容当版本指纹用，回滚后逐字节比对）
    const OLD_BINS: [(&str, &str); 5] = [
        ("bui", "BUI-4.0.0"),
        ("hysteria", "HY-2.12.2"),
        ("xray", "XRAY-26.3.27"),
        ("sing-box", "SB-1.13.19"),
        ("caddy", "CADDY-2.10.2"),
    ];
    const NEW_BINS: [(&str, &str); 4] = [
        ("hysteria", "HY-2.13.0"),
        ("xray", "XRAY-26.4.0"),
        ("sing-box", "SB-1.14.2"),
        ("caddy", "CADDY-2.11.0"),
    ];

    fn scratch(d: &tempfile::TempDir) -> Paths {
        Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    /// 升级前的机器：manifest 缓存 + `bin/` 下五个二进制 + 四个内核的 `version` 输出。
    fn pre_upgrade(paths: &Paths) -> (FakeHost, String) {
        let h = FakeHost::new();
        let manifest = manifest_json("4.0.0", OLD_KERNELS, OLD_BINS[0].1);
        h.write_file(
            &crate::paths::manifest_file(paths),
            manifest.as_bytes(),
            0o644,
        )
        .unwrap();
        for (name, body) in OLD_BINS {
            h.write_file(&paths.bin_dir.join(name), body.as_bytes(), 0o755)
                .unwrap();
        }
        script_versions(&h, paths, OLD_KERNELS);
        h.clear_ops();
        (h, manifest)
    }

    /// FakeHost 的 `run` 只按命令行前缀匹配，与盘上的字节无关；插到脚本表**最前面**即代表
    /// 「`bin/<kernel>` 现在是这一版」，让 `installed_versions` 跟着升级/回滚走。
    fn script_versions(h: &FakeHost, paths: &Paths, versions: [&str; 4]) {
        let banner = |name: &str, v: &str| match name {
            "hysteria" => format!("Version:\tv{v}\n"),
            "xray" => format!("Xray {v} (Xray) a (go1 linux/amd64)\n"),
            "sing-box" => format!("sing-box version {v}\n"),
            _ => format!("v{v} h1:x\n"),
        };
        h.with(|i| {
            for (name, v) in crate::kernels::KERNELS.iter().zip(versions) {
                let line = format!("{} version", paths.bin_dir.join(name).display());
                i.scripted
                    .insert(0, (line, CmdOut::success(&banner(name, v))));
            }
        });
    }

    fn text(h: &FakeHost, path: &std::path::Path) -> Option<String> {
        h.text(path.to_str().unwrap())
    }

    /// 只挑落盘类的 op（`installed_versions` 的探测 `run` 不算写）。
    fn writes(h: &FakeHost) -> Vec<String> {
        h.ops()
            .into_iter()
            .filter(|o| o.starts_with("write:") || o.starts_with("remove:"))
            .collect()
    }

    #[test]
    fn snapshot_prev_copies_the_manifest_cache_and_all_four_kernels() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let (h, manifest) = pre_upgrade(&paths);
        // 上上一版的 .prev 必须被覆盖（回滚只回一步）
        h.write_file(&paths.bin_dir.join("xray.prev"), b"XRAY-ANCIENT", 0o755)
            .unwrap();
        snapshot_prev(&h, &paths).unwrap();
        let prev_manifest = crate::paths::manifest_prev_file(&paths);
        assert_eq!(text(&h, &prev_manifest).as_deref(), Some(manifest.as_str()));
        assert_eq!(h.mode(prev_manifest.to_str().unwrap()), Some(0o644));
        for name in crate::kernels::KERNELS {
            let body = OLD_BINS.iter().find(|(n, _)| *n == name).unwrap().1;
            let prev = paths.bin_dir.join(format!("{name}.prev"));
            assert_eq!(text(&h, &prev).as_deref(), Some(body), "{name}");
            assert_eq!(h.mode(prev.to_str().unwrap()), Some(0o755), "{name}");
        }
        assert!(
            text(&h, &paths.bin_dir.join("bui.prev")).is_none(),
            "bui 自己的 .prev 由 apply_self 在校验过 sha256 之后写，快照不碰"
        );
    }

    #[test]
    fn snapshot_prev_skips_what_is_not_installed_yet() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        snapshot_prev(&h, &paths).unwrap();
        assert!(h.ops().is_empty(), "全新装机没有可快照的东西，一个字都不写");
    }

    /// 已是最新时 `prepare` 一个字都不能写：否则升过一次后再跑一次 `bui upgrade`
    /// （菜单里的「检查升级」）会把五个 `.prev` 与 `manifest.prev.json` 全覆盖成当前版，
    /// `--rollback` 就变成「只退 bui、内核原地不动」的混合态。
    #[test]
    fn prepare_writes_nothing_when_there_is_nothing_to_upgrade() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let (h, old_manifest) = pre_upgrade(&paths);
        let m: Manifest =
            serde_json::from_str(&manifest_json("4.0.0", OLD_KERNELS, OLD_BINS[0].1)).unwrap();
        let (plan, _) = prepare(&h, &paths, &m, "4.0.0", "x86_64").unwrap();
        assert!(
            plan.self_to.is_none() && plan.kernels.is_empty(),
            "{plan:?}"
        );
        assert_eq!(writes(&h), Vec::<String>::new(), "无需升级就一个字都不写");
        assert_eq!(
            text(&h, &crate::paths::manifest_file(&paths)).as_deref(),
            Some(old_manifest.as_str()),
            "连 manifest 缓存都不该重写"
        );
        assert!(text(&h, &crate::paths::manifest_prev_file(&paths)).is_none());

        // min_upgrade_from 直接 bail 的那次同样不许留下任何东西（尤其不许留新版缓存）
        let mut blocked: Manifest =
            serde_json::from_str(&manifest_json("4.2.0", NEW_KERNELS, "BUI-4.2.0")).unwrap();
        blocked.min_upgrade_from = Some("4.1.0".into());
        assert!(prepare(&h, &paths, &blocked, "4.0.0", "x86_64").is_err());
        assert_eq!(writes(&h), Vec::<String>::new(), "bail 之后一个字都不写");
    }

    /// 回归（2026-09-12 bwg-rick）：rc 通道下 `v4.0.0-rc1` / `rc2` / 正式版的 Cargo 版本号都是
    /// 同一个 `4.0.0`，光比版本号 ⇒ 同版本重建永远升不上去。同版本但 manifest 里 bui 资产的
    /// sha256 与盘上 `bin/bui` 不同时必须照常走完整流程（快照 → 新缓存 → 换二进制）。
    ///
    /// 四个内核是**同一判法**（`kernel_build_differs`：版本不同、或盘上 sha 与 manifest 不符
    /// 即重装），这一轮它们不动是因为两项判据都不成立——版本没变，盘上字节又正是 manifest
    /// 那一份。别把它读成「内核只按版本比对」：那正是 2026-09-16 P-A 修掉的发布阻断级缺陷
    /// （自建 sing-box 与官方归档同版本号，只比版本号就永远装不上去）。
    #[test]
    fn a_same_version_rebuild_still_upgrades_bui_but_leaves_the_kernels_alone() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let (h, old_manifest) = pre_upgrade(&paths);
        // 版本还是 4.0.0、内核版本一个没动，只有 bui 资产换成了另一份构建
        let m: Manifest =
            serde_json::from_str(&manifest_json("4.0.0", OLD_KERNELS, "BUI-4.0.0-rc2")).unwrap();
        let (plan, asset) = prepare(&h, &paths, &m, "4.0.0", "x86_64").unwrap();
        assert_eq!(plan.self_to.as_deref(), Some("4.0.0"), "同版本新构建也要升");
        assert_eq!(
            plan.kernels,
            vec![],
            "内核同一判法：版本没变、盘上 sha 又与 manifest 一致 ⇒ 这一轮确实不动"
        );
        assert!(
            format_plan(&plan).contains("同版本"),
            "计划文本要说清这是同版本重建：{}",
            format_plan(&plan)
        );
        assert_eq!(
            text(&h, &crate::paths::manifest_prev_file(&paths)).as_deref(),
            Some(old_manifest.as_str()),
            "同版本重建一样要先留回滚快照"
        );

        // 换上来的就是 manifest 里那一份（sha 校验 + .prev 保留照旧）
        let f = F(Mutex::new(vec![(
            "https://x/bui".to_string(),
            b"BUI-4.0.0-rc2".to_vec(),
        )]));
        apply_self(&h, &f, &asset, &paths.bin_dir).unwrap();
        assert_eq!(
            text(&h, &paths.bin_dir.join("bui")).as_deref(),
            Some("BUI-4.0.0-rc2")
        );
        assert_eq!(
            text(&h, &paths.bin_dir.join("bui.prev")).as_deref(),
            Some("BUI-4.0.0")
        );

        // 换完再跑一次：同版本同 sha ⇒ 零计划（菜单里的「检查升级」不许把 .prev 冲掉）
        h.clear_ops();
        let (plan, _) = prepare(&h, &paths, &m, "4.0.0", "x86_64").unwrap();
        assert!(
            plan.self_to.is_none() && plan.kernels.is_empty(),
            "{plan:?}"
        );
        assert_eq!(writes(&h), Vec::<String>::new(), "无需升级就一个字都不写");
    }

    #[test]
    fn prepare_snapshots_prev_before_writing_the_new_manifest_cache() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let (h, old_manifest) = pre_upgrade(&paths);
        let m: Manifest =
            serde_json::from_str(&manifest_json("4.0.1", NEW_KERNELS, "BUI-4.0.1")).unwrap();
        let (plan, _) = prepare(&h, &paths, &m, "4.0.0", "x86_64").unwrap();
        assert_eq!(plan.self_to.as_deref(), Some("4.0.1"));

        let ops = h.ops();
        let pos = |p: &std::path::Path| {
            let needle = format!("write:{}:", p.display());
            ops.iter()
                .position(|o| o.starts_with(&needle))
                .unwrap_or_else(|| panic!("{} 没写：{ops:?}", p.display()))
        };
        let cache = pos(&crate::paths::manifest_file(&paths));
        assert!(
            pos(&crate::paths::manifest_prev_file(&paths)) < cache,
            "manifest 快照必须早于新缓存：{ops:?}"
        );
        for name in crate::kernels::KERNELS {
            assert!(
                pos(&kernel_prev(&paths.bin_dir, name)) < cache,
                "{name}.prev 必须早于新 manifest 缓存：{ops:?}"
            );
        }
        assert_eq!(
            text(&h, &crate::paths::manifest_prev_file(&paths)).as_deref(),
            Some(old_manifest.as_str())
        );
        let cached: Manifest =
            serde_json::from_str(&text(&h, &crate::paths::manifest_file(&paths)).unwrap()).unwrap();
        assert_eq!(cached.version, "4.0.1", "新缓存照 manifest 落地");
    }

    #[test]
    fn rollback_restores_the_four_kernels_and_the_manifest_cache_byte_for_byte() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let (h, old_manifest) = pre_upgrade(&paths);

        // ---- 升一次：快照 → 换 manifest 缓存 → 换四个内核 → 换 bui 自己（apply_self 写 bui.prev）
        snapshot_prev(&h, &paths).unwrap();
        h.write_file(
            &crate::paths::manifest_file(&paths),
            manifest_json("4.0.1", NEW_KERNELS, "BUI-4.0.1").as_bytes(),
            0o644,
        )
        .unwrap();
        for (name, body) in NEW_BINS {
            h.write_file(&paths.bin_dir.join(name), body.as_bytes(), 0o755)
                .unwrap();
        }
        script_versions(&h, &paths, NEW_KERNELS);
        let payload = b"BUI-4.0.1".to_vec();
        let asset = Asset {
            url: "https://x/bui".into(),
            sha256: crate::kernels::sha256_hex(&payload),
        };
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), payload)]));
        apply_self(&h, &f, &asset, &paths.bin_dir).unwrap();

        // 五个 .prev + manifest.prev.json 都在，且内容 == 升级前
        for (name, body) in OLD_BINS {
            assert_eq!(
                text(&h, &paths.bin_dir.join(format!("{name}.prev"))).as_deref(),
                Some(body),
                "{name}.prev"
            );
        }
        assert_eq!(
            text(&h, &crate::paths::manifest_prev_file(&paths)).as_deref(),
            Some(old_manifest.as_str())
        );

        // ---- 回滚
        h.clear_ops();
        let done = rollback(&h, &paths).unwrap();
        script_versions(&h, &paths, OLD_KERNELS); // 盘上换回旧二进制 ⇒ 版本探测给旧版
        for (name, body) in OLD_BINS {
            let bin = paths.bin_dir.join(name);
            assert_eq!(
                text(&h, &bin).as_deref(),
                Some(body),
                "{name} 要逐字节回到升级前"
            );
            assert_eq!(h.mode(bin.to_str().unwrap()), Some(0o755), "{name}");
        }
        assert_eq!(
            text(&h, &crate::paths::manifest_file(&paths)).as_deref(),
            Some(old_manifest.as_str()),
            "manifest 缓存也要回到升级前，否则对账又把内核拉成新版"
        );
        assert!(h.ops().contains(&"systemd:restart:b-ui".to_string()));
        // 光把字节写回盘上不算回滚：内核单元还在内存里跑新版二进制，而恢复后的
        // manifest 与盘上版本一致 ⇒ 对账零变更 ⇒ apply 第 0 步不会替我们重启任何一个。
        for unit in ["hysteria-server", "xray", "b-ui-relay", "caddy"] {
            assert!(
                h.ops().contains(&format!("systemd:restart:{unit}")),
                "{unit} 必须随内核回滚一起重启：{:?}",
                h.ops()
            );
        }
        // 住宅单元是唯一的例外（T15，spec §9.1）：回滚**先停它再删 nft 表**，之后不许再
        // restart 一遍（`units_for_binary("sing-box")` 里有它）——重启会让 4.1 的 sing-box
        // 单元在表已删的情况下又只听单端口，住宅整段要等 4.0.1 的下一轮对账才通。
        assert_eq!(
            h.ops()
                .iter()
                .filter(|c| c.contains(":hysteria-residential"))
                .cloned()
                .collect::<Vec<_>>(),
            vec!["systemd:stop:hysteria-residential".to_string()],
            "住宅单元整趟只停一次、不重启：{:?}",
            h.ops()
        );
        assert!(
            done.iter().any(|l| l.contains("manifest.prev.json")),
            "{done:?}"
        );

        // 漂移为空：manifest.prev.json 与 bin/*.prev 都不该被报成陌生文件
        assert_eq!(
            crate::reconcile::drift::scan(&h, &[], &paths),
            vec![],
            "回滚留下的快照文件不算漂移"
        );

        // 对账零变更（也就不会下载）：Binary 的版本以恢复后的实际探测为准，与恢复的 manifest 一致
        let m: Manifest = serde_json::from_str(&old_manifest).unwrap();
        let arts: Vec<crate::reconcile::Artifact> = crate::kernels::KERNELS
            .iter()
            .map(|name| {
                let (version, asset) = m.kernel_asset(name, "x86_64").unwrap();
                crate::reconcile::Artifact::Binary {
                    name: name.to_string(),
                    version: version.to_string(),
                    sha256: asset.sha256.clone(),
                    url: asset.url.clone(),
                }
            })
            .collect();
        let installed = crate::kernels::installed_versions(&h, &paths.bin_dir);
        let plan = crate::reconcile::diff::plan(
            crate::reconcile::diff::PlanInput {
                artifacts: &arts,
                paths: &paths,
                keys: &BTreeMap::new(),
                installed_versions: &installed,
                facts: &crate::reconcile::Facts::probe(&h).unwrap(),
            },
            &h,
        )
        .unwrap();
        assert_eq!(plan.changes, vec![], "回滚后对账不许再装一遍内核");
        assert_eq!(plan.unchanged, 4);
    }

    #[test]
    fn rollback_without_kernel_prev_files_only_notes_it() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        // 只升过 bui 自己、没升过内核的机器：只有 bin/bui.prev
        let h = FakeHost::new();
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755)
            .unwrap();
        h.write_file(&paths.bin_dir.join("bui"), b"NEWBUI", 0o755)
            .unwrap();
        h.write_file(
            &crate::paths::manifest_file(&paths),
            manifest_json("4.0.1", NEW_KERNELS, "BUI-4.0.1").as_bytes(),
            0o644,
        )
        .unwrap();
        let done = rollback(&h, &paths).unwrap();
        assert_eq!(
            text(&h, &paths.bin_dir.join("bui")).as_deref(),
            Some("OLDBUI")
        );
        for name in crate::kernels::KERNELS {
            assert!(
                done.iter().any(|l| l.contains(&format!("{name}.prev"))),
                "缺 {name}.prev 只记一条 note：{done:?}"
            );
        }
        assert!(
            text(&h, &crate::paths::manifest_file(&paths)).is_none(),
            "没有上一版 manifest 就删掉当前缓存，免得对账按新版把内核又拉上去"
        );
        let restarts: Vec<String> = h
            .ops()
            .into_iter()
            .filter(|o| o.starts_with("systemd:restart:"))
            .collect();
        assert_eq!(
            restarts,
            vec!["systemd:restart:b-ui".to_string()],
            "没恢复内核就不该重启内核服务"
        );
    }

    #[test]
    fn jitter_is_reexported_from_serve() {
        // 实现与单元测试在 Task 15；这里只保证 CLI 侧的路径可用且行为一致
        let a = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000001").unwrap();
        assert_eq!(jitter_secs(a), crate::serve::jitter_secs(a));
        assert!(jitter_secs(a) < 3600);
    }
}
