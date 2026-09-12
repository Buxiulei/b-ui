//! SSH 硬化：v4 **唯一**一处实现（审计 §3.2「SSH 硬化 ×3 → 合并为一份」），
//! `bui install` / `bui reconcile` / `bui harden-ssh` 都走它。
//!
//! 移植 `server/core.sh:1867-1935`，v3 的两条血泪经验必须保留：
//! 1. 文件名用 `00-` 前缀——OpenSSH 的 `Include sshd_config.d/*.conf` 按字典序读、**首个匹配生效**，
//!    v3 用 `99-` 时被云镜像自带的 `50-cloud-init.conf` 抢先，加固形同虚设；
//! 2. `/root/.ssh/authorized_keys` 里没有公钥时**一个 artifact 都不产**，
//!    否则禁用密码登录等于把自己锁在门外。

use crate::reconcile::{Artifact, Module, RenderCtx, Unit, Verify};
use bui_schema::model::State;

pub const HARDENING_CONF: &str = "/etc/ssh/sshd_config.d/00-b-ui-hardening.conf";
/// v3 早期用过 99- 前缀，反被 50-cloud-init.conf 抢先生效（core.sh:1893 的踩坑记录）。
pub const LEGACY_CONF: &str = "/etc/ssh/sshd_config.d/99-b-ui-hardening.conf";

pub struct SshModule;

pub fn hardening_body() -> &'static str {
    "\
# B-UI SSH 加固（00- 前缀确保先于 50-cloud-init.conf 生效；OpenSSH 首个匹配生效）
PasswordAuthentication no
PermitRootLogin prohibit-password
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
"
}

impl Module for SshModule {
    fn name(&self) -> &'static str {
        "ssh"
    }

    // 「没有公钥」的提示不在 `render` 里发（它必须是纯函数）：由 Task 15 的 `reconcile_once`
    // 在 `state.system.ssh_hardening && facts.ssh_pubkeys == 0` 时往 `report.notes` 追加。
    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        if !s.system.ssh_hardening || ctx.facts.ssh_pubkeys == 0 {
            return Vec::new();
        }
        vec![
            Artifact::file(HARDENING_CONF, hardening_body())
                .mode(0o644)
                .verify(Verify::Sshd)
                .restart(Unit::reload(&ctx.facts.ssh_unit)),
            Artifact::Absent {
                path: LEGACY_CONF.into(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx, Unit, Verify};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn ctx(pubkeys: u32, ssh_unit: &str) -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: ssh_unit.into(),
                ssh_pubkeys: pubkeys,
                systemd_resolved: false,
            },
        }
    }

    #[test]
    fn hardening_file_is_00_prefixed_verified_and_reloads_the_right_unit() {
        let arts = SshModule.render(&sample_state(), &ctx(2, "ssh"));
        assert_eq!(arts.len(), 2, "硬化文件 + 删除 99- 老文件");
        match &arts[0] {
            Artifact::File {
                path,
                content,
                mode,
                verify,
                restart,
                immutable,
                restart_key,
            } => {
                assert_eq!(path.to_str(), Some(HARDENING_CONF));
                assert_eq!(*mode, 0o644);
                assert_eq!(*verify, Some(Verify::Sshd));
                assert_eq!(*restart, Some(Unit::reload("ssh")));
                assert!(!*immutable);
                assert_eq!(*restart_key, None);
                let text = String::from_utf8(content.clone()).unwrap();
                assert!(text.contains("PasswordAuthentication no"));
                assert!(text.contains("PermitRootLogin prohibit-password"));
                assert!(text.contains("KbdInteractiveAuthentication no"));
                assert!(text.contains("ChallengeResponseAuthentication no"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            arts[1],
            Artifact::Absent {
                path: LEGACY_CONF.into()
            }
        );
    }

    #[test]
    fn uses_sshd_unit_when_that_is_what_the_distro_has() {
        let arts = SshModule.render(&sample_state(), &ctx(1, "sshd"));
        match &arts[0] {
            Artifact::File { restart, .. } => assert_eq!(*restart, Some(Unit::reload("sshd"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_pubkey_means_no_hardening_at_all() {
        let arts = SshModule.render(&sample_state(), &ctx(0, "sshd"));
        assert_eq!(arts, vec![], "没有公钥时绝不禁用密码登录");
    }

    #[test]
    fn disabled_in_state_means_no_artifacts() {
        let mut s = sample_state();
        s.system.ssh_hardening = false;
        assert_eq!(SshModule.render(&s, &ctx(3, "sshd")), vec![]);
    }

    #[test]
    fn counts_only_real_pubkey_lines() {
        assert_eq!(
            crate::reconcile::count_pubkeys(
                "# 注释\n\nssh-ed25519 AAAA a@b\n  ecdsa-sha2-nistp256 BBB c@d\nnot-a-key xxx\nssh-rsa CCC e@f\n"
            ),
            3
        );
        assert_eq!(crate::reconcile::count_pubkeys(""), 0);
        assert_eq!(
            crate::reconcile::count_pubkeys("#ssh-rsa AAA commented\n"),
            0
        );
    }
}
