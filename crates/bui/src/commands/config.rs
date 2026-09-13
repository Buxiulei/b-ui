//! `bui set <项> <值>`：改期望态里的单项开关（Hysteria2 鉴权方式、旧订阅链接宽限期）。
//!
//! 与 `bui reconcile` 同一口径（spec §2.4「所有菜单项通过 unix socket 调守护进程 API」）：
//! socket 通就交给守护进程写 `state.json`，守护进程没跑才在本进程里直接改。**不能**两边都
//! 直接写 —— 守护进程的 `Store` 在内存里持着一份 state，CLI 绕过它写盘会被下一次守护进程
//! 的写盘整份覆盖掉，改动无声消失。

use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::model::Hy2Auth;
use bui_schema::paths::Paths;
use std::path::PathBuf;
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
        println!("Hysteria2 鉴权已切到 {mode}；对账会重渲染两份配置并各重启一次实例。");
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
}
