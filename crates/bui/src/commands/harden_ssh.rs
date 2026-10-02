//! `bui harden-ssh`：只跑 ssh 模块的一次对账（spec §3.4 的提示语「加好公钥后运行
//! `b-ui harden-ssh`」指向它）。
//!
//! 在线经 root UDS 交给 daemon 的控制 owner；离线持机器 lease 后才读状态与写配置。
//! SSH-only apply 不运行住宅网络清理，避免越过这个命令的资源范围。

use crate::modules::ssh::SshModule;
use crate::reconcile::apply::{apply, ApplyInput, BinaryInstaller};
use crate::reconcile::diff::{plan, PlanInput};
use crate::reconcile::{Facts, Module, RenderCtx};
use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub async fn run(paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_at_socket(paths, host, std::path::Path::new(crate::paths::SOCKET_PATH)).await
}

async fn run_at_socket(paths: Paths, host: Arc<dyn Host>, socket: &Path) -> Result<()> {
    let client = crate::ipc::Client::new(socket);
    if client.available().await {
        let (status, body) = client.request("POST", "/api/harden-ssh", None).await?;
        anyhow::ensure!(
            (200..300).contains(&status),
            "daemon SSH hardening failed (HTTP {status}): {body}"
        );
        println!("{body}");
        return Ok(());
    }
    let lease_paths = paths.clone();
    crate::residential_lifecycle::offline(&lease_paths, async move {
        let store = Store::open(crate::paths::state_file(&paths)).await?;
        run_with(store, paths, host).await
    })
    .await
}

pub async fn run_with(store: Store, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    let state = store.read().await;
    let h = host.clone();
    // Host 的接口是同步的，整轮对账在 spawn_blocking 里跑（总纲裁决）。
    let out = tokio::task::spawn_blocking(move || -> Result<_> {
        let facts = Facts::probe(h.as_ref())?;
        let ctx = RenderCtx {
            account_blocked: Default::default(),
            paths: paths.clone(),
            facts,
        };
        let arts = SshModule.render(&state, &ctx);
        // ssh 模块既没有 restart_key，也没有二进制版本，两张表都是空的。
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let p = plan(
            PlanInput {
                artifacts: &arts,
                paths: &paths,
                keys: &keys,
                installed_versions: &versions,
                facts: &ctx.facts,
            },
            h.as_ref(),
        )?;
        Ok(apply(
            ApplyInput {
                legacy_residential_cleanup: false,
                plan: p,
                paths: &paths,
                facts: &ctx.facts,
                installer: &NoBinaries,
                dry_run: false,
            },
            h.as_ref(),
        ))
    })
    .await??;
    for line in out
        .notes
        .iter()
        .chain(out.verify_failures.iter())
        .chain(out.errors.iter())
    {
        tracing::warn!("{line}");
        println!("{line}");
    }
    if out.changed.is_empty() {
        println!("SSH 加固已是最新（无改动）");
    } else {
        println!("SSH 加固已写入 {}", crate::modules::ssh::HARDENING_CONF);
    }
    Ok(())
}

/// ssh 模块不产出二进制 artifact。
struct NoBinaries;
impl BinaryInstaller for NoBinaries {
    fn install(&self, name: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> Result<()> {
        anyhow::bail!("ssh 模块不应产出二进制 artifact：{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use bui_schema::paths::Paths;

    fn scratch(d: &tempfile::TempDir) -> Paths {
        Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    #[tokio::test]
    async fn busy_owner_and_unavailable_uds_refuses_before_reading_state() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let _lease = crate::residential_lifecycle::ControlLease::acquire(&paths).unwrap();
        let host = Arc::new(FakeHost::new());
        let error = run_at_socket(paths, host.clone(), &d.path().join("missing.sock"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("owner unavailable"));
        assert!(host.ops().is_empty());
        assert!(!d.path().join("state.json").exists());
    }

    #[tokio::test]
    async fn writes_the_conf_and_reloads_sshd() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
        });
        let store = Store::create(
            crate::paths::state_file(&paths),
            crate::testutil::sample_state(),
        )
        .await
        .unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(host
            .text(crate::modules::ssh::HARDENING_CONF)
            .unwrap()
            .contains("PasswordAuthentication no"));
        assert!(host.ops().contains(&"systemd:reload:sshd".to_string()));
    }

    #[tokio::test]
    async fn sshd_test_failure_rolls_back_and_does_not_reload() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
            i.scripted.push((
                "sshd -t".into(),
                CmdOut::failure(255, "bad configuration option"),
            ));
        });
        let store = Store::create(
            crate::paths::state_file(&paths),
            crate::testutil::sample_state(),
        )
        .await
        .unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(
            host.text(crate::modules::ssh::HARDENING_CONF).is_none(),
            "sshd -t 失败要回滚"
        );
        assert!(!host.ops().iter().any(|o| o.starts_with("systemd:reload")));
    }

    #[tokio::test]
    async fn ssh_only_apply_does_not_touch_legacy_residential_network_state() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = Arc::new(FakeHost::new());
        host.write_file(
            &paths.base_dir.join("config-residential.yaml"),
            b"listen: :40000\n",
            0o600,
        )
        .unwrap();
        host.with(|i| {
            i.which.insert("iptables".into());
        });
        let store = Store::create(
            crate::paths::state_file(&paths),
            crate::testutil::sample_state(),
        )
        .await
        .unwrap();
        host.clear_ops();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(
            !host.ops().iter().any(|op| op.contains("iptables")
                || op.contains("nft")
                || op.contains("hysteria-residential")),
            "{:?}",
            host.ops()
        );
    }

    #[tokio::test]
    async fn without_a_pubkey_nothing_is_written() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        let store = Store::create(
            crate::paths::state_file(&paths),
            crate::testutil::sample_state(),
        )
        .await
        .unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(host.ops().is_empty());
    }
}
