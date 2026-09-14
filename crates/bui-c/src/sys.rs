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
    /// 本机 `127.0.0.1:port` 有没有人在听：连一下，200ms 为限，连上立刻断开。连接检查的
    /// 「本地端口」一行用它，不去解析 `ss` 的文本输出（spec §6.1）。
    fn tcp_listening(&self, port: u16) -> bool;
    /// 用本机的解析器解析 `host`，返回用时；最多等 `timeout`。只给连接检查 SOCKS 模式的
    /// 「DNS」一行用，巡检路径不调用（spec §0.2 R3、R13）。
    fn resolve(&self, host: &str, timeout: Duration) -> Result<Duration>;
    /// 试一次进程锁（不等）。`Ok(None)` = 别人正持着；拿到的 [`LockGuard`] 扔掉就放锁。
    /// 要等就经 [`crate::lock::acquire`]，别直接循环调它（spec §8.3）。
    fn try_lock(&self, path: &Path) -> Result<Option<crate::lock::LockGuard>>;
}

/// [`RealSys::write`] 的临时文件名：`<目录>/.<文件名>.<pid>.tmp`。带 pid，两个进程同时写同一个
/// 文件（菜单与巡检都会写 `runtime.json`）时不会共用一个临时文件（spec §0.2 R10）；
/// `Engine` 清残留时按 `.<文件名>.*.tmp` 认它。
pub fn tmp_path(target: &Path, pid: u32) -> PathBuf {
    let name = target.file_name().and_then(|s| s.to_str()).unwrap_or("f");
    let file = format!(".{name}.{pid}.tmp");
    match target.parent() {
        Some(dir) => dir.join(file),
        None => PathBuf::from(file),
    }
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
        let tmp = tmp_path(path, std::process::id());
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
        // `.mode()` 只在「这次创建了文件」时生效；pid 复用时撞上崩溃留下的同名 tmp 要补一刀
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

    /// 真连本机回环，所以没有单元测试（单元测试不联网，回环也不行），留给真机验收。
    fn tcp_listening(&self, port: u16) -> bool {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
    }

    /// `ToSocketAddrs` 走 getaddrinfo：阻塞，也不认超时。放进一个线程，这边最多等 `timeout`；
    /// 到点就不等了，线程留在后台等系统解析器自己放弃（每次检查最多一个，进程随后就回菜单）。
    /// 真解析，所以没有单元测试，留给真机验收。
    fn resolve(&self, host: &str, timeout: Duration) -> Result<Duration> {
        use std::net::ToSocketAddrs as _;
        let (tx, rx) = std::sync::mpsc::channel();
        let name = host.to_string();
        let start = std::time::Instant::now();
        std::thread::spawn(move || {
            let found = (name.as_str(), 443)
                .to_socket_addrs()
                .is_ok_and(|mut addrs| addrs.next().is_some());
            let _ = tx.send(found);
        });
        match rx.recv_timeout(timeout) {
            Ok(true) => Ok(start.elapsed()),
            Ok(false) => Err(Error::msg(format!("{host} 解析不到地址"))),
            Err(_) => Err(Error::msg(format!(
                "{host} 解析超时（{} 秒）",
                timeout.as_secs()
            ))),
        }
    }

    /// `flock(LOCK_EX | LOCK_NB)`（spec §8.3）。打开时 `O_NOFOLLOW`（锁文件被换成符号链接就报错，
    /// 不跟过去）、`0600`，std 默认带 `O_CLOEXEC`：测速拉起的子进程不会继承锁 fd。
    ///
    /// 拿到后比对 fd 与路径的 inode：拿锁的间隙文件被删掉重建过，这把锁锁住的就是孤儿 inode，
    /// 与后来的进程互不排斥，放掉重开一次。进程死了由内核放锁，不会留下死锁。
    /// 真开文件、真加锁，所以没有单元测试，留给真机验收。
    fn try_lock(&self, path: &Path) -> Result<Option<crate::lock::LockGuard>> {
        use nix::errno::Errno;
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        for _ in 0..2 {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(path)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::PermissionDenied => Error::msg("需要 root：用 sudo bui-c"),
                    _ => Error::io(path, e),
                })?;
            let lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(l) => l,
                Err((_, e)) if e == Errno::EWOULDBLOCK => return Ok(None),
                Err((_, e)) => return Err(Error::io(path, std::io::Error::from(e))),
            };
            let same = match (lock.metadata(), std::fs::symlink_metadata(path)) {
                (Ok(held), Ok(now)) => held.dev() == now.dev() && held.ino() == now.ino(),
                _ => false,
            };
            if same {
                return Ok(Some(crate::lock::LockGuard::new(Box::new(move || {
                    release(lock)
                }))));
            }
            drop(lock);
        }
        // 连着两次都被换掉：当作别人正在折腾它，等下一拍再试
        Ok(None)
    }
}

/// 放锁。不靠 `Flock` 的 Drop：它在 `LOCK_UN` 失败时会 panic，把菜单连同终端状态一起带走。
/// 失败只记日志，然后直接关掉 fd——flock 跟着打开的文件走，关掉就释放了。
fn release(lock: nix::fcntl::Flock<std::fs::File>) {
    use std::os::fd::AsRawFd as _;
    if let Err((lock, e)) = lock.unlock() {
        tracing::warn!(error = %e, "放进程锁失败，直接关闭锁文件");
        let fd = lock.as_raw_fd();
        // 不 forget 的话 Flock 的 Drop 会再解一次锁并 panic；forget 之后 File 不会自己关，手动关
        std::mem::forget(lock);
        let _ = nix::unistd::close(fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_carry_the_pid() {
        let t = Path::new("/opt/bui-c/runtime.json");
        assert_eq!(
            tmp_path(t, 4242),
            PathBuf::from("/opt/bui-c/.runtime.json.4242.tmp")
        );
        assert_ne!(
            tmp_path(t, 4242),
            tmp_path(t, 4243),
            "两个进程同时写同一个文件，临时文件不能撞名"
        );
        // 与 Engine 清残留的认法一致：`.<文件名>.` 开头、`.tmp` 结尾，且和目标在同一目录（rename 不跨盘）
        let tmp = tmp_path(Path::new("/opt/bui-c/config.json"), 7);
        let name = tmp.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(
            name.starts_with(".config.json.") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_eq!(tmp.parent(), Some(Path::new("/opt/bui-c")));
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
