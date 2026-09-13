//! 所有「碰真实系统」的能力都收在 [`Sys`] 一个 trait 后面：执行命令、读写文件、
//! 取时间、睡眠、读环境变量、取终端尺寸。生产用 [`RealSys`]，单元测试注入
//! [`FakeSys`](crate::fake::FakeSys)，于是测试既不 `systemctl` 也不写 `/etc`。

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 一次命令执行的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// 可注入的系统执行器。
pub trait Sys {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output>;
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
    /// 原子写：临时文件（一开始就是目标权限）+ fsync + rename。
    fn write(&self, path: &Path, data: &[u8], mode: u32) -> Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    /// 文件不存在也返回 `Ok(())`。
    fn remove_file(&self, path: &Path) -> Result<()>;
    /// 目录不存在也返回 `Ok(())`。
    fn remove_dir_all(&self, path: &Path) -> Result<()>;
    fn mkdir_p(&self, path: &Path) -> Result<()>;
    fn exists(&self, path: &Path) -> bool;
    /// 只列直接子项，按路径排序。
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    fn now(&self) -> time::OffsetDateTime;
    fn sleep(&self, d: Duration);
    /// 环境变量也从这里出去：测试注入，生产读真实环境（决策 11）。
    fn env(&self, key: &str) -> Option<String>;
    /// stdout 所在终端的尺寸 `(列, 行)`，同一次查询取出。stdout 不是终端（`| tee`、
    /// `| head`、timer 写 journald）、查询失败或列数为 0 时为 `None`：菜单据此回落
    /// 80×24、不发清屏序列。不缓存，每次画屏前重新取，窗口缩放与手机转屏立刻生效。
    fn term_size(&self) -> Option<(u16, u16)>;
}

/// 生产实现。
pub struct RealSys;

impl Sys for RealSys {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output> {
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .map_err(|e| Error::Command {
                prog: prog.to_string(),
                code: -1,
                stderr: e.to_string(),
            })?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(|e| Error::io(path, e))
    }

    /// 临时文件**一开始就用目标权限创建**，再 fsync + rename。
    /// 不能先 `File::create`（umask 下是 0644）后 chmod：`profiles.json` / `config.json`
    /// 带凭据，那样会有一个短暂的全局可读窗口。
    fn write(&self, path: &Path, data: &[u8], mode: u32) -> Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let dir = path
            .parent()
            .ok_or_else(|| Error::msg(format!("{} 没有父目录", path.display())))?;
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("f");
        let tmp = dir.join(format!(".{name}.tmp"));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(mode)
                .open(&tmp)
                .map_err(|e| Error::io(&tmp, e))?;
            f.write_all(data).map_err(|e| Error::io(&tmp, e))?;
            f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        }
        // `.mode()` 只在「这次创建了文件」时生效；上一轮崩溃留下的 tmp 要补一刀
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::rename(from, to).map_err(|e| Error::io(to, e))
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    fn mkdir_p(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path).map_err(|e| Error::io(path, e))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            out.push(e.map_err(|e| Error::io(path, e))?.path());
        }
        out.sort();
        Ok(out)
    }

    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    /// 查 stdout，不查 stdin 或 `/dev/tty`：要知道的是「我们的输出画在多宽的地方」，
    /// stdout 被重定向时就该回落，清屏序列也不能混进管道。请求号用 libc 的常量，
    /// 别写字面量：glibc 上 `ioctl` 的请求参数是 `c_ulong`，musl（发布构建）上是 `c_int`。
    /// 读不到终端是常态（管道、timer），不报错、不打日志。
    fn term_size(&self) -> Option<(u16, u16)> {
        use nix::libc;
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: 本 crate 唯一的 unsafe。`ws` 是本函数的局部可写 POD（四个 u16），
        // `&mut ws` 在调用期间独占且有效；fd 是本进程自己的 stdout；TIOCGWINSZ 只往
        // ws 里写一个 winsize，fd 不是终端或已关闭时返回 -1，不碰别的内存。
        let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
        (rc == 0 && ws.ws_col > 0).then_some((ws.ws_col, ws.ws_row))
    }
}

/// systemd 动作。查询类吞错返回 false，变更类失败要报错，
/// 幂等收尾类（stop/disable/reset-failed）一律 quiet——v3 里这三类到处 `|| true`。
pub mod systemd {
    use super::{Output, Sys};
    use crate::{Error, Result};

    fn quiet_ok<S: Sys>(sys: &S, args: &[&str]) -> Option<Output> {
        sys.run("systemctl", args).ok()
    }

    fn must<S: Sys>(sys: &S, args: &[&str]) -> Result<()> {
        let o = sys.run("systemctl", args)?;
        if o.ok() {
            Ok(())
        } else {
            Err(Error::Command {
                prog: format!("systemctl {}", args.join(" ")),
                code: o.code,
                stderr: o.stderr,
            })
        }
    }

    pub fn is_active<S: Sys>(sys: &S, unit: &str) -> bool {
        quiet_ok(sys, &["is-active", "--quiet", unit])
            .map(|o| o.ok())
            .unwrap_or(false)
    }
    pub fn is_enabled<S: Sys>(sys: &S, unit: &str) -> bool {
        quiet_ok(sys, &["is-enabled", "--quiet", unit])
            .map(|o| o.ok())
            .unwrap_or(false)
    }
    pub fn daemon_reload<S: Sys>(sys: &S) -> Result<()> {
        must(sys, &["daemon-reload"])
    }
    pub fn enable_now<S: Sys>(sys: &S, unit: &str) -> Result<()> {
        must(sys, &["enable", "--now", unit])
    }
    /// 只补开机自启，不动运行状态：单元已经在跑时 `enable --now` 不会重载配置，
    /// 「在跑 + 配置变了」要的是 `restart`（engine::apply 的重启语义）。
    pub fn enable<S: Sys>(sys: &S, unit: &str) -> Result<()> {
        must(sys, &["enable", unit])
    }
    pub fn restart<S: Sys>(sys: &S, unit: &str) -> Result<()> {
        must(sys, &["restart", unit])
    }
    pub fn start<S: Sys>(sys: &S, unit: &str) -> Result<()> {
        must(sys, &["start", unit])
    }
    pub fn stop_quiet<S: Sys>(sys: &S, unit: &str) {
        let _ = quiet_ok(sys, &["stop", unit]);
    }
    pub fn disable_quiet<S: Sys>(sys: &S, unit: &str) {
        let _ = quiet_ok(sys, &["disable", unit]);
    }
    pub fn reset_failed_quiet<S: Sys>(sys: &S, unit: &str) {
        let _ = quiet_ok(sys, &["reset-failed", unit]);
    }
}
