//! `bui harden-ssh`：只跑 ssh 模块的一次对账（spec §3.4 的提示语「加好公钥后运行
//! `b-ui harden-ssh`」指向它）。
//!
//! 守护进程未运行时也放行：它只渲染 [`SshModule`] 的一个 artifact——不写 `.verify/`
//! （`Verify::Sshd` 是「先写目标文件再 `sshd -t`」的特例，不落候选文件）、不写
//! `runtime.json`、不碰任何内核单元，因此与守护进程正在跑的对账没有交叉写。

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
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    run_with(store, paths, host).await
}

pub async fn run_with(store: Store, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    let state = store.read().await;
    let h = host.clone();
    // Host 的接口是同步的，整轮对账在 spawn_blocking 里跑（总纲裁决）。
    let out = tokio::task::spawn_blocking(move || -> Result<_> {
        let facts = Facts::probe(h.as_ref())?;
        let ctx = RenderCtx {
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
            },
            h.as_ref(),
        )?;
        Ok(apply(
            ApplyInput {
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
