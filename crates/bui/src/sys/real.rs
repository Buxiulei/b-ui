//! 生产实现：`std::fs` + `std::process::Command` + `/proc`。每个方法都只是薄封装，
//! 不含任何业务判断——判断全在对账器里，这样 [`super::fake::FakeHost`] 才能等价替换。

use super::{parse_proc_net, unit_full, CmdOut, Host, Proto};
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use tracing::warn;

pub struct RealHost;

impl RealHost {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for RealHost {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("读 {} 失败", path.display())),
        }
    }

    fn write_file(&self, path: &Path, content: &[u8], mode: u32) -> Result<()> {
        let parent = path
            .parent()
            .with_context(|| format!("{} 没有父目录", path.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("建目录 {} 失败", parent.display()))?;
        let name = path
            .file_name()
            .with_context(|| format!("{} 没有文件名", path.display()))?
            .to_string_lossy()
            .into_owned();
        let tmp = parent.join(format!(".{name}.tmp"));
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("建临时文件 {} 失败", tmp.display()))?;
            f.write_all(content)
                .with_context(|| format!("写 {} 失败", tmp.display()))?;
            f.sync_all()
                .with_context(|| format!("落盘 {} 失败", tmp.display()))?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("设 {} 权限失败", tmp.display()))?;
        // 目标是符号链接时必须先删：`/etc/resolv.conf` 在 systemd-resolved 机器上就是一条
        // 指向 stub 的链接，直接 rename 会把链接换成文件（可接受），但先删更贴近 v3 的行为，
        // 也避免某些内核/文件系统上 rename 覆盖链接的差异。
        if std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            std::fs::remove_file(path)
                .with_context(|| format!("删旧链接 {} 失败", path.display()))?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("重命名到 {} 失败", path.display()))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("删 {} 失败", path.display())),
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let rd = match std::fs::read_dir(path) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e).with_context(|| format!("列 {} 失败", path.display())),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.with_context(|| format!("列 {} 失败", path.display()))?;
            out.push(entry.path());
        }
        out.sort();
        Ok(out)
    }

    fn is_dir(&self, path: &Path) -> Result<bool> {
        Ok(std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false))
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("删目录 {} 失败", path.display())),
        }
    }

    fn is_symlink(&self, path: &Path) -> Result<bool> {
        Ok(std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false))
    }

    fn read_link(&self, path: &Path) -> Result<Option<PathBuf>> {
        Ok(std::fs::read_link(path).ok())
    }

    fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("建目录 {} 失败", parent.display()))?;
        }
        // 这里**不能**用 `Path::exists()`：它跟随符号链接，对**悬空链接**返回 false。
        // `uninstall_v3` 删掉 `<base>/b-ui-cli.sh` 之后，v3 留下的
        // `/usr/local/bin/b-ui → /opt/hysteria/b-ui-cli.sh` 正是一条悬空链接；判 false 就不删，
        // 紧接着的 `symlink()` 直接 EEXIST，CLI 入口永远建不起来（spec §2.4 的 `sudo b-ui`）。
        if std::fs::symlink_metadata(link).is_ok() {
            std::fs::remove_file(link)
                .with_context(|| format!("删旧的 {} 失败", link.display()))?;
        }
        std::os::unix::fs::symlink(target, link).with_context(|| {
            format!("建符号链接 {} -> {} 失败", link.display(), target.display())
        })?;
        Ok(())
    }

    fn set_immutable(&self, path: &Path, on: bool) -> Result<()> {
        let flag = if on { "+i" } else { "-i" };
        let p = path.to_string_lossy().into_owned();
        // 文件系统可能不支持 immutable 位（overlayfs、部分 VPS 模板），与 v3 `core.sh:1317`
        // 一致：只告警，不让整轮对账失败。
        match self.run("chattr", &[flag, &p]) {
            Ok(out) if out.ok() => {}
            Ok(out) => warn!(path = %p, flag, status = out.status, "chattr 失败，跳过 immutable"),
            Err(e) => warn!(path = %p, flag, error = %e, "chattr 不可用，跳过 immutable"),
        }
        Ok(())
    }

    fn is_immutable(&self, path: &Path) -> Result<bool> {
        let p = path.to_string_lossy().into_owned();
        let Ok(out) = self.run("lsattr", &["-d", &p]) else {
            return Ok(false);
        };
        if !out.ok() {
            return Ok(false);
        }
        // `----i---------e----- /etc/resolv.conf`
        Ok(out
            .stdout
            .split_whitespace()
            .next()
            .is_some_and(|attrs| attrs.contains('i')))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut> {
        // 不把 args 写进日志：域名无妨，但凭据绝不能进 journal（凭据只经 stdin）。
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("执行 {program} 失败"))?;
        Ok(CmdOut {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn which(&self, program: &str) -> bool {
        if program.contains('/') {
            return std::fs::metadata(program).is_ok_and(|m| m.is_file());
        }
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    }

    fn systemd_daemon_reload(&self) -> Result<()> {
        let out = self.run("systemctl", &["daemon-reload"])?;
        if !out.ok() {
            bail!("systemctl daemon-reload 失败：{}", out.stderr.trim());
        }
        Ok(())
    }

    fn systemd(&self, verb: &str, unit: &str) -> Result<CmdOut> {
        let full = unit_full(unit);
        self.run("systemctl", &[verb, &full])
    }

    fn unit_is_active(&self, unit: &str) -> Result<bool> {
        Ok(self.systemd("is-active", unit)?.ok())
    }

    fn unit_is_enabled(&self, unit: &str) -> Result<bool> {
        Ok(self.systemd("is-enabled", unit)?.ok())
    }

    fn unit_exists(&self, unit: &str) -> Result<bool> {
        let full = unit_full(unit);
        let out = self.run("systemctl", &["list-unit-files", &full])?;
        Ok(out.stdout.contains(&full))
    }

    fn unit_property(&self, unit: &str, prop: &str) -> Result<Option<String>> {
        let full = unit_full(unit);
        let out = self.run("systemctl", &["show", "-p", prop, "--value", &full])?;
        let v = out.stdout.trim();
        if v.is_empty() {
            Ok(None)
        } else {
            Ok(Some(v.to_string()))
        }
    }

    fn sysctl_get(&self, key: &str) -> Result<Option<String>> {
        let Ok(out) = self.run("sysctl", &["-n", key]) else {
            return Ok(None);
        };
        if !out.ok() {
            return Ok(None);
        }
        Ok(Some(out.stdout.trim().to_string()))
    }

    fn sysctl_set(&self, key: &str, value: &str) -> Result<()> {
        // 带空格的值（`net.ipv4.tcp_rmem=4096 262144 16777216`）作为**一个**参数传，天然正确。
        let out = self.run("sysctl", &["-w", &format!("{key}={value}")])?;
        if !out.ok() {
            bail!("sysctl -w {key} 失败：{}", out.stderr.trim());
        }
        Ok(())
    }

    fn modprobe(&self, module: &str) -> Result<()> {
        let out = self.run("modprobe", &[module])?;
        if !out.ok() {
            bail!("modprobe {module} 失败：{}", out.stderr.trim());
        }
        Ok(())
    }

    fn mem_mb(&self) -> Result<u64> {
        let text = std::fs::read_to_string("/proc/meminfo").context("读 /proc/meminfo 失败")?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb) = rest.split_whitespace().next() {
                    let kb: u64 = kb.parse().context("解析 MemTotal 失败")?;
                    return Ok(kb / 1024);
                }
            }
        }
        bail!("/proc/meminfo 里没有 MemTotal")
    }

    fn arch(&self) -> Result<String> {
        Ok(std::env::consts::ARCH.to_string())
    }

    fn hostname(&self) -> Result<String> {
        let h = nix::unistd::gethostname().context("gethostname 失败")?;
        Ok(h.to_string_lossy().into_owned())
    }

    fn listening_ports(&self, proto: Proto) -> Result<BTreeSet<u16>> {
        // UDP 没有 LISTEN 状态，只能看「有本地端口绑定」。
        let (files, listening_only) = match proto {
            Proto::Tcp => (["/proc/net/tcp", "/proc/net/tcp6"], true),
            Proto::Udp => (["/proc/net/udp", "/proc/net/udp6"], false),
        };
        let mut out = BTreeSet::new();
        for f in files {
            if let Ok(text) = std::fs::read_to_string(f) {
                out.extend(parse_proc_net(&text, listening_only));
            }
        }
        Ok(out)
    }

    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 只碰 tempfile 给的临时目录：原子写 + 权限 + 只列直接子项 + 悬空链接可被顶替。
    #[test]
    fn writes_atomically_and_lists_only_direct_children() {
        let d = tempfile::tempdir().unwrap();
        let h = RealHost::new();
        let f = d.path().join("sub/state.json");
        h.write_file(&f, b"{}", 0o600).unwrap();
        assert_eq!(h.read_file(&f).unwrap().unwrap(), b"{}");
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // 临时文件不留痕
        assert_eq!(
            h.list_dir(&d.path().join("sub")).unwrap(),
            vec![d.path().join("sub/state.json")]
        );
        assert_eq!(
            h.list_dir(&d.path().join("nope")).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert!(h.is_dir(&d.path().join("sub")).unwrap());
        assert!(!h.is_dir(&f).unwrap());
        assert_eq!(h.read_file(&d.path().join("nope")).unwrap(), None);
        h.remove_file(&d.path().join("nope")).unwrap();
    }

    #[test]
    fn symlink_replaces_a_dangling_link() {
        let d = tempfile::tempdir().unwrap();
        let h = RealHost::new();
        let link = d.path().join("bin/b-ui");
        // 先做一条悬空链接（v3 卸载后的真实形态）
        h.symlink(Path::new("/opt/hysteria/b-ui-cli.sh"), &link)
            .unwrap();
        assert!(h.is_symlink(&link).unwrap());
        assert!(!link.exists(), "悬空链接：exists() 为 false");
        let target = d.path().join("bin/bui");
        h.write_file(&target, b"ELF", 0o755).unwrap();
        h.symlink(&target, &link).unwrap();
        assert_eq!(h.read_link(&link).unwrap(), Some(target));
        assert_eq!(h.read_link(&d.path().join("nope")).unwrap(), None);
    }

    #[test]
    fn remove_dir_all_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let h = RealHost::new();
        h.write_file(&d.path().join("admin/server.js"), b"node", 0o644)
            .unwrap();
        h.remove_dir_all(&d.path().join("admin")).unwrap();
        h.remove_dir_all(&d.path().join("admin")).unwrap();
        assert!(!h.is_dir(&d.path().join("admin")).unwrap());
    }
}
