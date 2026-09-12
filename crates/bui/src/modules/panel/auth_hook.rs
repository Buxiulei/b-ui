//! `bui auth-hook <addr> <auth> <tx>`：Hysteria2 `auth.type: command` 的钩子。
//!
//! **极简路径**（spec §3.2 + 调研 H1–H9）：不初始化 tokio、不初始化 tracing、不加载 state，
//! 只读一个小 JSON 文件。内核对钩子既不设超时也不限流，每条新 QUIC 连接 fork 一次本进程，
//! 所以这里不允许出现任何重量级初始化；`main.rs` 把这一支放在建 runtime 之前。
//!
//! 判定失败一律 fail-closed（退出码 1）。stderr 不会进 `hysteria-server` 的 journal（H5），
//! 诊断写 `<base>/auth-hook.log`（0600，只记用户名与结果，**不记密码**）。

use crate::modules::panel::snapshot::{self, Snapshot};
use bui_schema::paths::Paths;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use subtle::ConstantTimeEq;

/// 钩子自设的硬超时（spec §3.2：内核**不设**超时，调研 H8）。
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(2);
/// 日志超限的阈值。超限时**原地截断**（只保留尾部 [`LOG_KEEP_BYTES`] 重写同一个文件），
/// **绝不产生 `auth-hook.log.1` 之类的兄弟文件** —— 理由见 [`truncate_in_place`]。
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;
/// 原地截断后保留的尾部字节数。
pub const LOG_KEEP_BYTES: u64 = 256 * 1024;

/// `<base>/auth-hook.log`（在 P1 的 `reconcile::drift::BASE_WHITELIST` 里）。
pub fn log_path(paths: &Paths) -> PathBuf {
    paths.base_dir.join("auth-hook.log")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { user_id: String },
    Deny { reason: &'static str },
}

impl Decision {
    fn label(&self) -> &'static str {
        match self {
            Decision::Allow { .. } => "allow",
            Decision::Deny { reason } => reason,
        }
    }
}

/// 纯函数：解析 `auth` → 查快照 → 常量时间比对密码 → 校 `blocked` 与期限。
pub fn decide(snap: &Snapshot, auth: &str, now: time::OffsetDateTime) -> Decision {
    // H1：auth 是 `user:pass` 原串，按**第一个**冒号拆分（密码里可以再有冒号）
    let Some((username, password)) = auth.split_once(':') else {
        return Decision::Deny {
            reason: "malformed-auth",
        };
    };
    if username.is_empty() || password.is_empty() {
        return Decision::Deny {
            reason: "malformed-auth",
        };
    }
    let Some(u) = snap.users.get(username) else {
        return Decision::Deny {
            reason: "no-such-user",
        };
    };
    // 常量时间比对（spec §3.2）
    if u.hy2_password
        .as_bytes()
        .ct_eq(password.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Decision::Deny {
            reason: "bad-password",
        };
    }
    if u.blocked {
        return Decision::Deny { reason: "blocked" };
    }
    if let Some(exp) = &u.expires_at {
        // 解析失败也算到期：fail-closed
        match crate::util::parse_rfc3339(exp) {
            Some(t) if t > now => {}
            _ => return Decision::Deny { reason: "expired" },
        }
    }
    Decision::Allow {
        user_id: u.user_id.clone(),
    }
}

/// 进程入口：返回**进程退出码**（0 = 放行），放行时先把 `user_id` 打到 stdout。
pub fn run(args: &[String]) -> i32 {
    let (code, out) = run_with(
        &Paths::default_server(),
        args,
        time::OffsetDateTime::now_utc(),
    );
    if let Some(id) = out {
        println!("{id}");
    }
    code
}

/// [`run`] 的可测版本：显式给 base 目录与「现在」，不读进程环境、不打印。
pub fn run_with(
    paths: &Paths,
    args: &[String],
    now: time::OffsetDateTime,
) -> (i32, Option<String>) {
    let addr = args.first().cloned().unwrap_or_default();
    let Some(auth) = args.get(1).cloned() else {
        log_line(paths, now, &addr, "-", "missing-args");
        return (1, None);
    };
    // 内核不设超时（H8）：把「读文件 + 判定」放子线程，主线程自己掐 2 秒
    let (tx, rx) = std::sync::mpsc::channel::<Decision>();
    let file = crate::paths::auth_snapshot_file(paths);
    let auth_for_thread = auth.clone();
    std::thread::spawn(move || {
        let snap = snapshot::read(&file);
        // 空快照（文件缺失或坏 JSON）里查不到任何用户 ⇒ no-such-user ⇒ 拒绝
        let _ = tx.send(decide(&snap, &auth_for_thread, now));
    });
    let decision = rx
        .recv_timeout(HOOK_TIMEOUT)
        .unwrap_or(Decision::Deny { reason: "timeout" });
    let username = auth.split_once(':').map(|(u, _)| u).unwrap_or("-");
    log_line(paths, now, &addr, username, decision.label());
    match decision {
        Decision::Allow { user_id } => (0, Some(user_id)),
        Decision::Deny { .. } => (1, None),
    }
}

/// 追加一行 `<RFC3339> <addr> <username> <结果>`。失败一律忽略——钩子不能因为写不了日志
/// 就拒绝合法用户。
fn log_line(paths: &Paths, now: time::OffsetDateTime, addr: &str, username: &str, result: &str) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = log_path(paths);
    if std::fs::metadata(&path)
        .map(|m| m.len() > LOG_MAX_BYTES)
        .unwrap_or(false)
    {
        truncate_in_place(&path);
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
    else {
        return;
    };
    let ts = crate::util::fmt_rfc3339(now);
    let _ = writeln!(f, "{ts} {addr} {username} {result}");
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
}

/// 超限时只保留尾部 [`LOG_KEEP_BYTES`]，**重写同一个文件**。
///
/// 不要改成 `rename` 出 `auth-hook.log.1`：P1 的 `reconcile::drift::BASE_WHITELIST`
/// 只认 `auth-hook.log`（`TRANSIENT_SUFFIXES` 也只有 `.tmp` / `.new`），多出来的兄弟文件
/// 会让每 10 分钟的漂移巡检永久报 `stray_file`（`/api/health` 随之 degraded），
/// 并被 `bui reconcile --force` 删掉。
fn truncate_in_place(path: &std::path::Path) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    let keep_from = data.len().saturating_sub(LOG_KEEP_BYTES as usize);
    // 从下一个换行之后开始，避免文件头留下半行
    let start = match data[keep_from..].iter().position(|b| *b == b'\n') {
        Some(i) => keep_from + i + 1,
        None => keep_from,
    };
    let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    else {
        return;
    };
    let _ = f.write_all(&data[start..]);
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::snapshot::{self, Snapshot, SnapshotUser};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn paths(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            schema: 1,
            users: BTreeMap::from([
                (
                    "alice".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into(),
                        hy2_password: "pw:with:colons".into(),
                        expires_at: None,
                        blocked: false,
                    },
                ),
                (
                    "bob".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb".into(),
                        hy2_password: "pw-bob".into(),
                        expires_at: Some("2026-09-10T00:00:00Z".into()),
                        blocked: false,
                    },
                ),
                (
                    "carol".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000cc".into(),
                        hy2_password: "pw-carol".into(),
                        expires_at: None,
                        blocked: true,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn allows_a_correct_password_and_returns_the_user_id() {
        assert_eq!(
            decide(&snap(), "alice:pw:with:colons", t0()),
            Decision::Allow {
                user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into()
            },
            "auth 按第一个冒号拆分，密码里允许再有冒号"
        );
    }

    #[test]
    fn denies_wrong_password_unknown_user_blocked_and_expired() {
        assert_eq!(
            decide(&snap(), "alice:nope", t0()),
            Decision::Deny {
                reason: "bad-password"
            }
        );
        assert_eq!(
            decide(&snap(), "dave:x", t0()),
            Decision::Deny {
                reason: "no-such-user"
            }
        );
        assert_eq!(
            decide(&snap(), "carol:pw-carol", t0()),
            Decision::Deny { reason: "blocked" }
        );
        assert_eq!(
            decide(&snap(), "bob:pw-bob", t0()),
            Decision::Deny { reason: "expired" }
        );
        assert_eq!(
            decide(&snap(), "bob:pw-bob", datetime!(2026-09-09 23:59:59 UTC)),
            Decision::Allow {
                user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb".into()
            },
            "到期时间在未来就放行"
        );
    }

    #[test]
    fn denies_malformed_auth_payloads() {
        assert_eq!(
            decide(&snap(), "alice", t0()),
            Decision::Deny {
                reason: "malformed-auth"
            }
        );
        assert_eq!(
            decide(&snap(), "", t0()),
            Decision::Deny {
                reason: "malformed-auth"
            }
        );
        assert_eq!(
            decide(&snap(), ":pw", t0()),
            Decision::Deny {
                reason: "malformed-auth"
            }
        );
        assert_eq!(
            decide(&snap(), "alice:", t0()),
            Decision::Deny {
                reason: "malformed-auth"
            }
        );
        // 坏时间戳当成到期（fail-closed），不是当成不过期
        let mut s = snap();
        s.users.get_mut("alice").unwrap().expires_at = Some("not a time".into());
        assert_eq!(
            decide(&s, "alice:pw:with:colons", t0()),
            Decision::Deny { reason: "expired" }
        );
    }

    #[test]
    fn an_empty_snapshot_denies_everyone() {
        assert_eq!(
            decide(&Snapshot::empty(), "alice:pw", t0()),
            Decision::Deny {
                reason: "no-such-user"
            }
        );
    }

    #[test]
    fn run_with_returns_the_id_and_appends_a_redacted_log_line() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        snapshot::write_if_changed(&crate::paths::auth_snapshot_file(&p), &snap()).unwrap();
        let args = vec![
            "203.0.113.10:51820".to_string(),
            "alice:pw:with:colons".to_string(),
            "0".to_string(),
        ];
        let (code, out) = run_with(&p, &args, t0());
        assert_eq!(code, 0);
        assert_eq!(out.as_deref(), Some("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa"));
        let log = std::fs::read_to_string(log_path(&p)).unwrap();
        assert!(log.contains("alice"), "日志要能定位到用户：{log}");
        assert!(log.contains("allow"), "{log}");
        assert!(!log.contains("pw:with:colons"), "密码绝不能进日志：{log}");
        assert!(
            log.contains("203.0.113.10:51820"),
            "addr 进日志便于排查：{log}"
        );
    }

    #[test]
    fn run_with_is_fail_closed_without_a_snapshot_or_with_too_few_args() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let (code, out) = run_with(&p, &["addr".into(), "alice:pw".into(), "0".into()], t0());
        assert_eq!(code, 1, "快照不存在必须拒绝，不能放行");
        assert_eq!(out, None);
        assert_eq!(run_with(&p, &["only-addr".into()], t0()).0, 1);
        assert_eq!(run_with(&p, &[], t0()).0, 1);
    }

    #[test]
    fn the_log_is_0600_and_is_truncated_in_place_when_oversized() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        // 每行 51 字节 × 30000 ≈ 1.53 MiB > LOG_MAX_BYTES
        let mut big = String::new();
        for i in 0..30_000u32 {
            big.push_str(&format!(
                "2026-09-11T00:00:00Z 203.0.113.10:1 u{i:05} allow-x\n"
            ));
        }
        assert!(big.len() as u64 > LOG_MAX_BYTES, "先确认这份日志真的超限");
        std::fs::write(log_path(&p), &big).unwrap();
        let _ = run_with(&p, &["a".into(), "alice:pw".into(), "0".into()], t0());

        // ① 绝不产生兄弟文件：P1 的 drift::BASE_WHITELIST 只认 `auth-hook.log`，
        //    多一个 auth-hook.log.1 就是一条永久 stray_file（/api/health 判 degraded），
        //    还会被 `bui reconcile --force` 删掉。
        let mut top: Vec<String> = std::fs::read_dir(&p.base_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        top.sort();
        assert_eq!(
            top,
            vec!["auth-hook.log".to_string()],
            "<base> 顶层只许有这一个文件"
        );

        // ② 原地截断成尾部 + 本次新追加的一行
        let after = std::fs::read_to_string(log_path(&p)).unwrap();
        assert!(
            (after.len() as u64) <= LOG_KEEP_BYTES + 128,
            "截断后应只剩尾部 LOG_KEEP_BYTES：{}",
            after.len()
        );
        assert!(
            after.starts_with("2026-09-11T00:00:00Z"),
            "不能留半行：{:?}",
            &after[..40]
        );
        assert!(
            after.trim_end().ends_with("no-such-user"),
            "本次那一行要在末尾：{:?}",
            after.trim_end().lines().last()
        );
        assert_eq!(
            std::fs::metadata(log_path(&p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
