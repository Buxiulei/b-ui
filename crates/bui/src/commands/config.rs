//! `bui set <项> <值>`：改期望态里的单项开关（Hysteria2 鉴权方式、旧订阅链接宽限期、HY2 混淆）。
//!
//! 与 `bui reconcile` 同一口径（spec §2.4「所有菜单项通过 unix socket 调守护进程 API」）：
//! socket 通就交给守护进程写 `state.json`，守护进程没跑才在本进程里直接改。**不能**两边都
//! 直接写 —— 守护进程的 `Store` 在内存里持着一份 state，CLI 绕过它写盘会被下一次守护进程
//! 的写盘整份覆盖掉，改动无声消失。

use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::model::{Hy2Auth, Obfs};
use bui_schema::paths::Paths;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn run_hy2_auth(mode: &str, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_hy2_auth_with(mode, paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// socket 路径由调用方传入（与 install / reconcile / status 同一口径：单元测试不碰真实 socket）。
pub async fn run_hy2_auth_with(
    mode: &str,
    paths: Paths,
    _host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    let mode: Hy2Auth = mode.parse().map_err(anyhow::Error::msg)?;
    let client = crate::ipc::Client::new(&socket);
    if client.available().await {
        let (status, body) = client
            .request(
                "POST",
                "/api/system/hy2-auth",
                Some(serde_json::json!({"mode": mode.as_str()})),
            )
            .await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("守护进程拒绝了这次修改（HTTP {status}）：{body}");
        }
        println!("Hysteria2 鉴权已切到 {mode}；对账会重渲染 config.yaml 并重启 hysteria-server（4.1 起只作用于直连）。");
        return Ok(());
    }
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    if store.read().await.system.hy2_auth == mode {
        println!("Hysteria2 鉴权已经是 {mode}，无需改动。");
        return Ok(());
    }
    store.update(|s| s.system.hy2_auth = mode).await?;
    println!("Hysteria2 鉴权已切到 {mode}；守护进程没在跑，请执行 `bui reconcile` 让它生效。");
    Ok(())
}

/// `bui set legacy-sub <值>` 的取值校验：`off`（不分大小写）⇒ `None`，也就是立刻停用
/// 全部「用户名订阅链接」；否则必须是 RFC3339 时刻，归一成秒级再落盘。
///
/// **唯一一处**：`POST /api/system/legacy-sub` 也调它，免得 CLI 与 API 各有一套判据。
pub fn parse_legacy_sub(value: &str) -> Result<Option<String>> {
    if value.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let t = crate::util::parse_rfc3339(value).ok_or_else(|| {
        anyhow::anyhow!(
            "取值只能是 off 或 RFC3339 时刻（如 2026-09-21T00:00:00Z），收到「{value}」"
        )
    })?;
    Ok(Some(crate::util::fmt_rfc3339(t)))
}

/// 那一行「改完之后现在是什么状态」。
fn legacy_sub_notice(until: Option<&str>) -> String {
    match until {
        None => "旧「用户名订阅链接」已全部停用，此后只认每用户的随机 token 链接。".into(),
        Some(t) => format!("旧「用户名订阅链接」的宽限期改到 {t}，到点后只认随机 token 链接。"),
    }
}

pub async fn run_legacy_sub(value: &str, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_legacy_sub_with(value, paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// 口径与 [`run_hy2_auth_with`] 完全一致：socket 通就交给守护进程写，没跑才自己写。
pub async fn run_legacy_sub_with(
    value: &str,
    paths: Paths,
    _host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    let until = parse_legacy_sub(value)?;
    let client = crate::ipc::Client::new(&socket);
    if client.available().await {
        let (status, body) = client
            .request(
                "POST",
                "/api/system/legacy-sub",
                Some(serde_json::json!({"until": until})),
            )
            .await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("守护进程拒绝了这次修改（HTTP {status}）：{body}");
        }
        println!("{}", legacy_sub_notice(until.as_deref()));
        return Ok(());
    }
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    if store.read().await.system.legacy_sub_until == until {
        println!("宽限期已经是这个值，无需改动。");
        return Ok(());
    }
    let value = until.clone();
    store.update(|s| s.system.legacy_sub_until = value).await?;
    println!("{}", legacy_sub_notice(until.as_deref()));
    Ok(())
}

/// obfs 密码的十六进制位数：16 字节随机，与订阅 token（`bui_schema::sub`）同量级。
pub const OBFS_PASSWORD_HEX_LEN: usize = 32;

/// `bui set obfs <值>` 的取值校验：`on` / `off`（不分大小写）⇒ 开 / 关，别的一律报错。
///
/// **唯一一处**：`POST /api/system/obfs` 也调它，免得 CLI 与 API 各有一套判据。
pub fn parse_obfs(value: &str) -> Result<bool> {
    if value.eq_ignore_ascii_case("on") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("off") {
        Ok(false)
    } else {
        anyhow::bail!("取值只能是 on 或 off，收到「{value}」")
    }
}

/// 开关落到 `node.obfs` 之后的样子；`None` = 已经是这个状态（不写盘、不触发对账、不重启实例）。
///
/// - `on`：密码为空才生成随机密码，**不为空就沿用** —— 重复 on 不能让所有客户端再断一次；
/// - `off`：保留密码，再 on 回来时已下发的订阅仍然有效。
///
/// 混淆覆盖直连与全部住宅 HY2 实例（2026-09-15 裁决，渲染在 `bui_schema::render::hysteria`）。
/// CLI 的本地路径与 `POST /api/system/obfs` 共用这一处。
pub fn obfs_switched(current: &Obfs, on: bool) -> Option<Obfs> {
    let password = if on && current.password.is_empty() {
        crate::commands::install::random_hex(OBFS_PASSWORD_HEX_LEN / 2)
    } else {
        current.password.clone()
    };
    let next = Obfs {
        enabled: on,
        password,
    };
    (next != *current).then_some(next)
}

/// 改完之后的那段说明（状态 + 下一步）。**不含 obfs 密码**。
fn obfs_notice(enabled: bool, changed: bool, daemon: bool) -> String {
    if !changed {
        return if enabled {
            "HY2 混淆（salamander）已经是开启状态，无需改动。".into()
        } else {
            "HY2 混淆已经是关闭状态，无需改动。".into()
        };
    }
    let head = if enabled {
        "HY2 混淆（salamander）已开启，覆盖直连与全部住宅 HY2 实例。"
    } else {
        "HY2 混淆已关闭（密码保留，再开启时沿用）。"
    };
    let apply = if daemon {
        "对账会重渲染 HY2 配置并重启直连与住宅 HY2 实例。"
    } else {
        "守护进程没在跑，请执行 `bui reconcile` 让它生效。"
    };
    format!("{head}\n所有用户的 HY2 节点要更新一次订阅才能连上；Reality 节点不受影响。\n{apply}")
}

pub async fn run_obfs(value: &str, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_obfs_with(value, paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// 口径与 [`run_legacy_sub_with`] 完全一致：socket 通就交给守护进程写，没跑才自己写。
pub async fn run_obfs_with(
    value: &str,
    paths: Paths,
    _host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    println!("{}", switch_obfs(value, &paths, &socket).await?);
    Ok(())
}

/// [`run_obfs_with`] 的本体：返回要打印的那段话（单元测试直接断言它）。
async fn switch_obfs(value: &str, paths: &Paths, socket: &Path) -> Result<String> {
    let on = parse_obfs(value)?;
    let client = crate::ipc::Client::new(socket);
    if client.available().await {
        let (status, body) = client
            .request(
                "POST",
                "/api/system/obfs",
                Some(serde_json::json!({"value": if on { "on" } else { "off" }})),
            )
            .await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("守护进程拒绝了这次修改（HTTP {status}）：{body}");
        }
        let changed = body["changed"].as_bool().unwrap_or(true);
        return Ok(obfs_notice(on, changed, true));
    }
    let store = Store::open(crate::paths::state_file(paths)).await?;
    let Some(next) = obfs_switched(&store.read().await.node.obfs, on) else {
        return Ok(obfs_notice(on, false, false));
    };
    store.update(|s| s.node.obfs = next).await?;
    Ok(obfs_notice(on, true, false))
}

// ── `bui set hy2-resi-compat on|off`（4.1，spec §2.4）─────────────────────────

/// 兼容段下线的**门禁判据**（2026-09-17 裁决）：满足它才允许不带 `--force` 关掉。
///
/// 判据是「**一次都没命中过** 且静默满 30 天」。为什么不能只看静默时长：兼容段的 counter
/// 是 **nat 链**计数，只计每条 conntrack 流的首包（`man nft`），一个 24×7 不断线的 4.0
/// 客户端只在建连那一刻记 1 次，之后几十天一动不动 ⇒ 只看静默就会把**正在用**的兼容段
/// 判成闲置并关掉，那批还没刷订阅的 4.0 住宅用户当场断联。所以命中过就只能人工 `--force`。
///
/// `None`（统计还没就绪）一律返回 `false` —— **fail-closed**，误判方向是危险的那一侧。
///
/// **T12 依赖**：这三个值由 `modules::watchdog` 每轮采样写进 `runtime.json`
/// （`CompatHits`），T12 合并之前这里恒为 `None` ⇒ `off` 一律要 `--force`。T12 合并后把
/// 本函数体换成 `hits.is_some_and(|h| h.idle_for_takedown(now))` 即可（判据一处、两边同源）。
pub fn compat_idle_for_takedown(
    hits: Option<&crate::commands::nft::CompatHits>,
    now: time::OffsetDateTime,
) -> bool {
    let Some(h) = hits else { return false };
    if h.total > 0 {
        return false;
    }
    match crate::util::parse_rfc3339(h.quiet_since()) {
        // 起算时刻读不出来（字段被人改坏）⇒ 不判闲置
        None => false,
        Some(t) => now - t >= time::Duration::days(COMPAT_IDLE_DAYS),
    }
}

/// 兼容段自动判闲置要静默多少天（spec §2.4：「连续 30 天为 0」）。
pub const COMPAT_IDLE_DAYS: i64 = 30;

/// 拒绝下线时那段话：把人判断需要的三个值**全部**打出来（spec §2.4）。
fn compat_refusal(hits: Option<&crate::commands::nft::CompatHits>) -> String {
    let detail = match hits {
        Some(h) => format!(
            "累计命中 {} 次；最近一次 {}；起算时刻 {}",
            h.total,
            h.last_hit_at.as_deref().unwrap_or("从未"),
            h.since
        ),
        None => "命中统计未就绪（守护进程还没采过一轮）".to_string(),
    };
    format!(
        "拒绝关闭住宅 HY2 的 4.0 兼容段：{detail}。\n         关掉它 = `inet bui` 删掉 {} 那两条 REDIRECT 规则 + 防火墙收口，\n         **全部还没刷订阅的 4.0 住宅用户当场断联**（他们的订阅里是裸 `40000+槽序号`）。\n         判据是连续 {COMPAT_IDLE_DAYS} 天累计命中为 0；确认无人在用请加 --force。",
        "40001-40007"
    )
}

/// 改完之后的那段说明（状态 + 下一步）。
fn compat_notice(on: bool, changed: bool, daemon: bool, forced: bool) -> String {
    if !changed {
        return if on {
            "住宅 HY2 的 4.0 兼容段已经是开启状态，无需改动。".into()
        } else {
            "住宅 HY2 的 4.0 兼容段已经是关闭状态，无需改动。".into()
        };
    }
    let head = if on {
        "住宅 HY2 的 4.0 兼容段已开启（40001-40007 一并 REDIRECT 到单一监听端口）。"
    } else {
        "住宅 HY2 的 4.0 兼容段已关闭（`inet bui` 只剩两条规则，防火墙同步收口）。"
    };
    let warn = if !on && forced {
        "\n**这是 --force 下线**：还在用裸 `40000+槽序号` 订阅的 4.0 用户会立刻断联，让他们重新获取一次订阅。"
    } else {
        ""
    };
    let apply = if daemon {
        "对账会重放 `inet bui` 并同步防火墙端口。"
    } else {
        "守护进程没在跑，请执行 `bui reconcile`（或 `bui nft apply`）让它生效。"
    };
    format!("{head}{warn}\n{apply}")
}

pub async fn run_hy2_resi_compat(
    value: &str,
    force: bool,
    paths: Paths,
    host: Arc<dyn Host>,
) -> Result<()> {
    run_hy2_resi_compat_with(
        value,
        force,
        paths,
        host,
        PathBuf::from(crate::paths::SOCKET_PATH),
    )
    .await
}

/// 口径与 [`run_obfs_with`] 完全一致：socket 通就交给守护进程写，没跑才自己写。
pub async fn run_hy2_resi_compat_with(
    value: &str,
    force: bool,
    paths: Paths,
    host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    println!(
        "{}",
        switch_hy2_resi_compat(value, force, &paths, &socket, host.now()).await?
    );
    Ok(())
}

/// [`run_hy2_resi_compat_with`] 的本体：返回要打印的那段话（单元测试直接断言它）。
///
/// 门禁在**这一处**（不在守护进程那一侧）：`runtime.json` 本地就读得到，而且要在改任何
/// 东西之前就拒掉。
async fn switch_hy2_resi_compat(
    value: &str,
    force: bool,
    paths: &Paths,
    socket: &Path,
    now: time::OffsetDateTime,
) -> Result<String> {
    let on = parse_obfs(value)?; // 同一套 on/off 判据
    let hits = crate::commands::nft::compat_hits(
        &crate::state::runtime::Runtime::load(crate::paths::runtime_file(paths))
            .read()
            .await,
    );
    if !on && !force && !compat_idle_for_takedown(hits.as_ref(), now) {
        anyhow::bail!("{}", compat_refusal(hits.as_ref()));
    }
    let client = crate::ipc::Client::new(socket);
    if client.available().await {
        let (status, body) = client
            .request(
                "POST",
                "/api/system/hy2-resi-compat",
                Some(serde_json::json!({"value": if on { "on" } else { "off" }})),
            )
            .await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("守护进程拒绝了这次修改（HTTP {status}）：{body}");
        }
        let changed = body["changed"].as_bool().unwrap_or(true);
        return Ok(compat_notice(on, changed, true, force));
    }
    let store = Store::open(crate::paths::state_file(paths)).await?;
    if store.read().await.system.hy2_resi_compat_ports == on {
        return Ok(compat_notice(on, false, false, force));
    }
    store
        .update(|s| s.system.hy2_resi_compat_ports = on)
        .await?;
    Ok(compat_notice(on, true, false, force))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;

    async fn scratch() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        Store::create(crate::paths::state_file(&paths), sample_state())
            .await
            .unwrap();
        (d, paths)
    }

    fn t0() -> time::OffsetDateTime {
        time::macros::datetime!(2026-10-20 00:00:00 UTC)
    }

    fn hits(total: u64, last: Option<&str>, since: &str) -> crate::commands::nft::CompatHits {
        crate::commands::nft::CompatHits {
            total,
            last_hit_at: last.map(str::to_string),
            since: since.to_string(),
        }
    }

    /// 兼容段下线门禁（spec §2.4 / 2026-09-17 裁决）：**只有「一次都没命中过 + 静默满 30
    /// 天」才允许不带 `--force`**，其余一律拒。
    ///
    /// 误判方向是危险的那一侧：兼容段的 counter 是 nat 链计数，只计每条 conntrack 流的
    /// 首包，一个 24×7 不断线的 4.0 客户端只在建连那一刻记 1 次 —— 只看静默时长就会把
    /// **正在用**的兼容段判成闲置并关掉，那批还没刷订阅的 4.0 住宅用户当场断联。
    #[test]
    fn the_takedown_gate_fails_closed_on_everything_but_a_provably_idle_range() {
        // 统计未就绪（T12 的采样还没落地 / 守护进程刚起）⇒ 不判闲置
        assert!(!compat_idle_for_takedown(None, t0()));
        // 一次都没命中 + 静默 30 天 ⇒ 放行
        let idle = hits(0, None, "2026-09-15T00:00:00Z");
        assert!(compat_idle_for_takedown(Some(&idle), t0()));
        // 差一天都不行
        assert!(!compat_idle_for_takedown(
            Some(&idle),
            time::macros::datetime!(2026-10-14 23:59:59 UTC)
        ));
        // **命中过就永不自动判闲置**，哪怕最近那一次已经过了一年
        let hit_long_ago = hits(1, Some("2025-01-01T00:00:00Z"), "2024-01-01T00:00:00Z");
        assert!(
            !compat_idle_for_takedown(Some(&hit_long_ago), t0()),
            "nat 链只计首包：一个常连的 4.0 客户端几十天只记 1 次"
        );
        // 起算时刻被人改坏 ⇒ 不判闲置
        assert!(!compat_idle_for_takedown(
            Some(&hits(0, None, "下周")),
            t0()
        ));
        assert_eq!(COMPAT_IDLE_DAYS, 30, "spec §2.4：连续 30 天为 0");
    }

    /// `bui set hy2-resi-compat off` 在门禁不放行时**必须拒绝并打出三个值**（total /
    /// 最近一次 / 起算时刻），由人判断；`--force` 才放过，且警告那批人会断联。
    #[tokio::test]
    async fn turning_the_compat_range_off_needs_force_until_it_is_provably_idle() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        // 统计未就绪 ⇒ 拒绝，state 一个字节不动
        let e = switch_hy2_resi_compat("off", false, &paths, &sock, t0())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("拒绝关闭"), "{e}");
        assert!(e.contains("命中统计未就绪"), "{e}");
        assert!(e.contains("--force"), "要说清下一步怎么做：{e}");
        assert!(e.contains("当场断联"), "要说清代价：{e}");
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert!(
            store.read().await.system.hy2_resi_compat_ports,
            "被拒的那次不许改期望态"
        );

        // 命中过 ⇒ 拒绝，且三个值都要打出来给人判断
        let rt = crate::state::runtime::Runtime::load(crate::paths::runtime_file(&paths));
        rt.update(|r| {
            r.extra.insert(
                crate::commands::nft::COMPAT_HITS_KEY.into(),
                serde_json::json!({
                    "total": 12, "seen": 12,
                    "last_hit_at": "2026-10-19T10:00:00Z",
                    "since": "2026-09-01T00:00:00Z",
                }),
            );
        })
        .await;
        let e = switch_hy2_resi_compat("off", false, &paths, &sock, t0())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("累计命中 12 次"), "{e}");
        assert!(e.contains("最近一次 2026-10-19T10:00:00Z"), "{e}");
        assert!(e.contains("起算时刻 2026-09-01T00:00:00Z"), "{e}");

        // `--force` 放过，并警告那批人会断联
        let out = switch_hy2_resi_compat("off", true, &paths, &sock, t0())
            .await
            .unwrap();
        assert!(out.contains("4.0 兼容段已关闭"), "{out}");
        assert!(out.contains("--force 下线"), "{out}");
        assert!(out.contains("会立刻断联"), "{out}");
        // 每次都重开：`Store` 缓存自己那份内存副本，写盘的是 switch 里另开的那个句柄
        let reopen = || async {
            Store::open(crate::paths::state_file(&paths))
                .await
                .unwrap()
                .read()
                .await
                .system
                .hy2_resi_compat_ports
        };
        assert!(!reopen().await);

        // 再 off 一次：已经是这个状态 ⇒ 零改动（门禁照旧先过，所以还得带 --force）
        assert!(switch_hy2_resi_compat("off", true, &paths, &sock, t0())
            .await
            .unwrap()
            .contains("已经是关闭状态"));
        // 开回来不需要 --force（开兼容段没有风险）
        let out = switch_hy2_resi_compat("on", false, &paths, &sock, t0())
            .await
            .unwrap();
        assert!(out.contains("4.0 兼容段已开启"), "{out}");
        assert!(reopen().await);
    }

    /// 真正闲置（一次都没命中 + 静默 30 天）⇒ 不用 `--force` 也能关。
    #[tokio::test]
    async fn a_provably_idle_compat_range_comes_down_without_force() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        crate::state::runtime::Runtime::load(crate::paths::runtime_file(&paths))
            .update(|r| {
                r.extra.insert(
                    crate::commands::nft::COMPAT_HITS_KEY.into(),
                    serde_json::json!({
                        "total": 0, "seen": 0, "since": "2026-09-15T00:00:00Z",
                    }),
                );
            })
            .await;
        let out = switch_hy2_resi_compat("off", false, &paths, &sock, t0())
            .await
            .unwrap();
        assert!(out.contains("4.0 兼容段已关闭"), "{out}");
        assert!(!out.contains("--force 下线"), "不是强制下线：{out}");
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert!(!store.read().await.system.hy2_resi_compat_ports);
        drop(store);
    }

    /// 守护进程没跑时（socket 指向一个不存在的路径）就地改 `state.json`。
    #[tokio::test]
    async fn without_a_daemon_it_writes_the_state_file_itself() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        run_hy2_auth_with("command", paths.clone(), Arc::new(FakeHost::new()), sock)
            .await
            .unwrap();
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert_eq!(store.read().await.system.hy2_auth, Hy2Auth::Command);
    }

    /// `off` ⇒ 停用（`None`）；RFC3339 归一成秒级；别的一律报错。
    #[test]
    fn legacy_sub_takes_off_or_an_rfc3339_instant() {
        assert_eq!(parse_legacy_sub("off").unwrap(), None);
        assert_eq!(parse_legacy_sub("OFF").unwrap(), None);
        assert_eq!(
            parse_legacy_sub("2026-09-21T00:00:00Z").unwrap().as_deref(),
            Some("2026-09-21T00:00:00Z")
        );
        assert_eq!(
            parse_legacy_sub("2026-09-21T00:00:00.500Z")
                .unwrap()
                .as_deref(),
            Some("2026-09-21T00:00:00Z"),
            "归一成秒级"
        );
        for bad in ["", "下周", "7d", "2026-09-21"] {
            assert!(parse_legacy_sub(bad).is_err(), "{bad} 该被挡掉");
        }
    }

    /// 守护进程没跑时就地改 `state.json`：`off` 之后 `legacy_sub_until` 必须是 `None`
    /// （而不是留着一个过去的时刻），非法值一个字都不许写。
    #[tokio::test]
    async fn legacy_sub_off_clears_the_deadline_in_the_state_file() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        store
            .update(|s| s.system.legacy_sub_until = Some("2026-09-21T00:00:00Z".into()))
            .await
            .unwrap();
        drop(store);

        run_legacy_sub_with(
            "off",
            paths.clone(),
            Arc::new(FakeHost::new()),
            sock.clone(),
        )
        .await
        .unwrap();
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert_eq!(store.read().await.system.legacy_sub_until, None);
        drop(store);

        // 反向：设一个时刻
        run_legacy_sub_with(
            "2026-10-01T08:00:00Z",
            paths.clone(),
            Arc::new(FakeHost::new()),
            sock.clone(),
        )
        .await
        .unwrap();
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert_eq!(
            store.read().await.system.legacy_sub_until.as_deref(),
            Some("2026-10-01T08:00:00Z")
        );
        drop(store);

        let err = run_legacy_sub_with("下周", paths.clone(), Arc::new(FakeHost::new()), sock)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("RFC3339"), "{err}");
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert_eq!(
            store.read().await.system.legacy_sub_until.as_deref(),
            Some("2026-10-01T08:00:00Z"),
            "非法值不落盘"
        );
    }

    #[tokio::test]
    async fn an_unknown_mode_is_refused_before_anything_is_written() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        let err = run_hy2_auth_with("userpass", paths.clone(), Arc::new(FakeHost::new()), sock)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("http"), "{err}");
        let store = Store::open(crate::paths::state_file(&paths)).await.unwrap();
        assert_eq!(store.read().await.system.hy2_auth, Hy2Auth::Http, "没被改");
    }

    fn is_obfs_password(pw: &str) -> bool {
        pw.len() == 32 && pw.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    }

    fn obfs_in(state_json: &std::path::Path) -> Obfs {
        let bytes = std::fs::read(state_json).unwrap();
        serde_json::from_slice::<bui_schema::model::State>(&bytes)
            .unwrap()
            .node
            .obfs
    }

    /// state.json 的字节 + `state.backups` 里的文件数：「不写盘」就是这两样都不变。
    fn disk_snapshot(paths: &Paths) -> (Vec<u8>, usize) {
        let state = std::fs::read(crate::paths::state_file(paths)).unwrap();
        let backups = std::fs::read_dir(paths.base_dir.join("state.backups"))
            .map(|d| d.count())
            .unwrap_or(0);
        (state, backups)
    }

    /// `on` / `off` 不分大小写；别的一律报错（CLI 与 `POST /api/system/obfs` 共用这一处）。
    #[test]
    fn obfs_takes_on_or_off_only() {
        assert!(parse_obfs("on").unwrap());
        assert!(parse_obfs("ON").unwrap());
        assert!(!parse_obfs("off").unwrap());
        for bad in ["", "true", "1", "enable", "salamander"] {
            let err = parse_obfs(bad).unwrap_err();
            assert!(err.to_string().contains("on 或 off"), "{bad}: {err}");
        }
    }

    /// 守护进程没跑时就地改 `state.json`：首次 on 生成 32 位十六进制密码；重复 on 密码不变且
    /// 一个字节都不写；off 保留密码；重复 off 也不写盘；再 on 沿用原密码。输出只断言
    /// 「要更新订阅」那半句。
    #[tokio::test]
    async fn obfs_without_a_daemon_switches_the_state_file_itself() {
        let (d, paths) = scratch().await;
        let sock = d.path().join("nope.sock");
        let file = crate::paths::state_file(&paths);
        assert!(!obfs_in(&file).enabled, "sample_state 默认关闭");

        let out = switch_obfs("on", &paths, &sock).await.unwrap();
        assert!(out.contains("更新一次订阅"), "{out}");
        let first = obfs_in(&file);
        assert!(first.enabled);
        assert!(is_obfs_password(&first.password), "密码形状不对");
        assert!(!out.contains(&first.password), "输出不许带 obfs 密码");
        let before = disk_snapshot(&paths);

        let out = switch_obfs("on", &paths, &sock).await.unwrap();
        assert!(out.contains("已经是开启状态"), "{out}");
        assert_eq!(obfs_in(&file), first, "重复 on 密码不变");
        assert_eq!(disk_snapshot(&paths), before, "重复 on 不写盘");

        let out = switch_obfs("off", &paths, &sock).await.unwrap();
        assert!(out.contains("更新一次订阅"), "{out}");
        let off = obfs_in(&file);
        assert!(!off.enabled);
        assert_eq!(off.password, first.password, "off 保留密码");
        let before = disk_snapshot(&paths);
        let out = switch_obfs("OFF", &paths, &sock).await.unwrap();
        assert!(out.contains("已经是关闭状态"), "{out}");
        assert_eq!(disk_snapshot(&paths), before, "重复 off 不写盘");

        switch_obfs("on", &paths, &sock).await.unwrap();
        assert_eq!(
            obfs_in(&file),
            first,
            "再开沿用原密码，已下发的订阅继续有效"
        );

        let err = run_obfs_with("maybe", paths.clone(), Arc::new(FakeHost::new()), sock)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("on 或 off"), "{err}");
        assert_eq!(obfs_in(&file), first, "非法值不落盘");
    }

    /// 起一个只挂基础 router 的守护进程 socket（同 `ipc` 的测试），交出事件总线。
    async fn spawn_daemon(dir: &std::path::Path) -> (PathBuf, crate::api::EventBus) {
        let sock = dir.join("b-ui.sock");
        let store = Store::create(dir.join("state.json"), sample_state())
            .await
            .unwrap();
        let host = Arc::new(FakeHost::new());
        let bus = crate::api::EventBus::new();
        let app = crate::api::router(
            crate::api::AppState {
                store,
                bus: bus.clone(),
                runtime: crate::state::runtime::Runtime::load(dir.join("runtime.json")),
                host: host.clone(),
                started_at: host.now(),
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            &[],
        );
        let s = sock.clone();
        tokio::spawn(async move {
            let _ = crate::ipc::serve_uds(&s, app).await;
        });
        for _ in 0..100 {
            if crate::ipc::Client::new(&sock).available().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (sock, bus)
    }

    /// 守护进程在跑：CLI 只经 socket 交给 `POST /api/system/obfs`，自己**不碰**本地 state.json
    /// （给 CLI 的 `paths` 指向一个没有 state.json 的空目录，一旦走本地路径就会报错）。
    /// 变化时守护进程发 `StateChanged`，没变化不发。
    #[tokio::test]
    async fn obfs_with_a_daemon_goes_through_the_socket() {
        let daemon = tempfile::tempdir().unwrap();
        let (sock, bus) = spawn_daemon(daemon.path()).await;
        let mut rx = bus.subscribe();
        let file = daemon.path().join("state.json");
        let cli_dir = tempfile::tempdir().unwrap();
        let cli_paths = Paths {
            base_dir: cli_dir.path().to_path_buf(),
            certs_dir: cli_dir.path().join("certs"),
            bin_dir: cli_dir.path().join("bin"),
        };

        let out = switch_obfs("on", &cli_paths, &sock).await.unwrap();
        assert!(out.contains("更新一次订阅"), "{out}");
        let first = obfs_in(&file);
        assert!(first.enabled && is_obfs_password(&first.password));
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::api::Event::StateChanged(_))
        ));
        assert!(
            !crate::paths::state_file(&cli_paths).exists(),
            "CLI 没自己写"
        );

        let out = switch_obfs("on", &cli_paths, &sock).await.unwrap();
        assert!(out.contains("已经是开启状态"), "{out}");
        assert_eq!(obfs_in(&file), first, "重复 on 密码不变");
        assert!(rx.try_recv().is_err(), "没变化不发 StateChanged");

        let out = switch_obfs("off", &cli_paths, &sock).await.unwrap();
        assert!(out.contains("更新一次订阅"), "{out}");
        assert_eq!(
            obfs_in(&file),
            Obfs {
                enabled: false,
                password: first.password.clone()
            },
            "off 保留密码"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::api::Event::StateChanged(_))
        ));

        let err = switch_obfs("maybe", &cli_paths, &sock).await.unwrap_err();
        assert!(err.to_string().contains("on 或 off"), "{err}");
        assert!(!obfs_in(&file).enabled, "非法值不落盘");
        assert!(rx.try_recv().is_err());
    }
}
