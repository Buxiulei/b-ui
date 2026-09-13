//! `bui auth-hook <addr> <auth> <tx>`：Hysteria2 `auth.type: command` 的钩子。
//!
//! **极简路径**（spec §3.2 + 调研 H1–H9）：不初始化 tokio、不初始化 tracing、不加载 state，
//! 只读一个小 JSON 文件。内核对钩子既不设超时也不限流，每条新 QUIC 连接 fork 一次本进程，
//! 所以这里不允许出现任何重量级初始化；`main.rs` 把这一支放在建 runtime 之前。
//!
//! 判定失败一律 fail-closed（退出码 1）。stderr 不会进 `hysteria-server` 的 journal（H5），
//! 诊断写 `<base>/auth-hook.log`（0600，只记用户名与结果，**不记密码**）。

use crate::modules::panel::snapshot::Snapshot;
use bui_schema::paths::Paths;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use subtle::ConstantTimeEq;

/// 钩子自设的硬超时（spec §3.2：内核**不设**超时，调研 H8）。
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(2);
/// 日志超限的阈值。超限时**原地截断**（只保留尾部 [`LOG_KEEP_BYTES`] 重写同一个文件），
/// **绝不产生 `auth-hook.log.1` 之类的兄弟文件** —— 理由见 [`truncate_in_place`]。
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;
/// 原地截断后保留的尾部字节数。
pub const LOG_KEEP_BYTES: u64 = 256 * 1024;
/// 非阻塞 fd 上 `EAGAIN` 的重试间隔（只有快照不是普通文件时才会走到，见 [`read_snapshot`]）。
const READ_POLL: Duration = Duration::from_millis(2);

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
    /// 落日志用的结果字段（`allow` 或拒绝原因）。`auth_http` 也用它，两条鉴权路径的
    /// `auth-hook.log` 因此逐字同格式。
    pub fn label(&self) -> &'static str {
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

/// 只为**本机微基准**留的 base 覆盖（`scripts/ops/authhook-microbench.sh`）。
///
/// 生产上钩子的环境由 systemd 给的 `hysteria-server` / `hysteria-residential` 决定，能改它
/// 的环境就已经是 root，所以这条覆盖不放大攻击面；但它确实只为测量存在，别写进任何单元文件。
const BASE_DIR_ENV: &str = "BUI_BASE_DIR";

fn paths_from_env() -> Paths {
    match std::env::var_os(BASE_DIR_ENV) {
        Some(d) if !d.is_empty() => {
            let base = PathBuf::from(d);
            Paths {
                certs_dir: base.join("certs"),
                bin_dir: base.join("bin"),
                base_dir: base,
            }
        }
        _ => Paths::default_server(),
    }
}

/// 进程入口：返回**进程退出码**（0 = 放行），放行时先把 `user_id` 打到 stdout。
pub fn run(args: &[String]) -> i32 {
    let (code, out) = run_with(&paths_from_env(), args, time::OffsetDateTime::now_utc());
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
    // 内核不设超时（H8），所以钩子自己掐 2 秒 —— 但**不起线程**：2026-09-13 bwg-rick 实测
    // 每次调用有 ≈15ms 的固定开销，本机 strace -c 里 clone + futex + sigaltstack + 线程栈的
    // mmap/munmap 是最大的一块（约 300µs / 次）。超时改由 [`read_snapshot`] 自己看表。
    let file = crate::paths::auth_snapshot_file(paths);
    let decision = match read_snapshot(&file, std::time::Instant::now() + HOOK_TIMEOUT) {
        // 空快照（文件缺失或坏 JSON）里查不到任何用户 ⇒ no-such-user ⇒ 拒绝
        Some(snap) => decide(&snap, &auth, now),
        None => Decision::Deny { reason: "timeout" },
    };
    let username = auth.split_once(':').map(|(u, _)| u).unwrap_or("-");
    log_line(paths, now, &addr, username, decision.label());
    match decision {
        Decision::Allow { user_id } => (0, Some(user_id)),
        Decision::Deny { .. } => (1, None),
    }
}

/// 读快照并解析。只有**到点还没读完**才返回 `None`（= 超时）；文件缺失、权限不对、内容
/// 不是 JSON 一律返回 [`Snapshot::empty`]（里面查不到用户 ⇒ 调用方拒绝，fail-closed）。
///
/// 两处刻意的写法，都是为了在**不起线程**的前提下仍然守住 2 秒：
/// - `O_NONBLOCK` 打开。钩子唯一可能卡在 `open` 上的情形是快照不再是普通文件（被换成
///   FIFO / 字符设备），这个标志让 `open` 立刻返回；普通文件上它是空操作。
/// - 自己写 `read` 循环而不用 `std::fs::read`。后者的 `read_to_end` 在 `EINTR` 上无条件
///   重试、在非阻塞 fd 上直接报错，卡住就是永远卡住；这里每轮先看一眼表。
///
/// 守不住的那一类和老写法一样：快照躺在卡死的块设备上时 `open` 进不可中断睡眠，谁也叫不醒
/// 它——老写法的主线程虽然 2 秒就返回，可 `std::process::exit` 带不走那条卡在 D 状态的线程，
/// 内核仍要等 I/O 结束才让进程消失，Hysteria 那边照样在等。
fn read_snapshot(path: &Path, deadline: std::time::Instant) -> Option<Snapshot> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut buf = Vec::with_capacity(4096);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
    {
        let mut chunk = [0u8; 8192];
        loop {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            match f.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                // 只有非普通文件会走到 WouldBlock（普通文件的 read 不返回 EAGAIN）
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(READ_POLL)
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    buf.clear();
                    break;
                }
            }
        }
    }
    Some(serde_json::from_slice(&buf).unwrap_or_else(|_| Snapshot::empty()))
}

/// 追加一行 `<RFC3339> <addr> <username> <结果>`。失败一律忽略——钩子不能因为写不了日志
/// 就拒绝合法用户。
///
/// http 鉴权（[`crate::modules::panel::auth_http`]）调的是同一个函数：m1 step6 与 m3 验收
/// 都按这行格式断言，两条路径写出来的必须一模一样。
pub fn log_line(
    paths: &Paths,
    now: time::OffsetDateTime,
    addr: &str,
    username: &str,
    result: &str,
) {
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

    /// 超时兜底（原来是「子线程 + `recv_timeout`」，现在是 [`read_snapshot`] 自己看表）。
    ///
    /// Fake：把快照路径做成一条 FIFO，并在测试里一直**持着写端不写**（写端不存在时
    /// 非阻塞读会直接拿到 EOF，那是另一条路径）。钩子于是永远读不到数据，正是它唯一可能
    /// 卡住的那类情形。判据两条：2 秒左右必须返回，且必须是拒绝。这条用例自身要跑满 2 秒。
    #[test]
    fn a_snapshot_read_that_blocks_is_denied_within_the_timeout() {
        use std::os::unix::fs::OpenOptionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let fifo = crate::paths::auth_snapshot_file(&p);
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        // O_RDWR 打开 FIFO 是 Linux 扩展：一次拿到读写两端，不会像 O_WRONLY 那样在
        // 没有读端时报 ENXIO。这个 fd 活着 = 写端存在 = 钩子那边读到的是 EAGAIN。
        let _keep_writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
            .open(&fifo)
            .unwrap();

        let t = std::time::Instant::now();
        let (code, out) = run_with(&p, &["a".into(), "alice:pw".into(), "0".into()], t0());
        let took = t.elapsed();

        assert_eq!(code, 1, "读不到快照必须拒绝");
        assert_eq!(out, None);
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_secs(5),
            "应该在 2 秒左右兜底返回，实测 {took:?}"
        );
        let log = std::fs::read_to_string(log_path(&p)).unwrap();
        assert!(log.trim_end().ends_with("timeout"), "要记成超时：{log}");
        assert!(!log.contains(":pw"), "密码绝不能进日志：{log}");
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
