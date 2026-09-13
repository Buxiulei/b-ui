//! `bui set <项> <值>`：改期望态里的单项开关。目前只有 Hysteria2 的鉴权方式。
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
