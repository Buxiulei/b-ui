use std::path::PathBuf;

pub const UNIT_MAIN: &str = "bui-c.service";
pub const UNIT_CHECK: &str = "bui-c-check.service";
pub const UNIT_TIMER: &str = "bui-c.timer";
pub const TUN_IFACE: &str = "bui-tun";
pub const SELF_BIN: &str = "/usr/local/bin/bui-c";

const DEFAULT_BASE: &str = "/opt/bui-c";
const DEFAULT_UNIT_DIR: &str = "/etc/systemd/system";
const DEFAULT_RUN_DIR: &str = "/run";

/// `/opt/bui-c` 布局（spec §6、总纲 C3）。测试用 `BUI_C_BASE` / `BUI_C_UNIT_DIR` /
/// `BUI_C_RUN_DIR` 改到 tempdir。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub base: PathBuf,
    pub unit_dir: PathBuf,
    /// 进程锁所在的目录，默认 `/run`（tmpfs，重启就清空）。不放 `base` 下：卸载会删掉整个
    /// `/opt/bui-c`，等锁的进程锁住的是已删除的 inode，后来的进程新建同名文件，两把锁互不
    /// 排斥（spec §8.3）。
    pub run_dir: PathBuf,
}

impl Paths {
    pub fn new(base: impl Into<PathBuf>, unit_dir: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            unit_dir: unit_dir.into(),
            run_dir: PathBuf::from(DEFAULT_RUN_DIR),
        }
    }
    /// 纯函数：`get` 就是「按名字取环境变量」这一个能力，测试直接喂闭包。
    pub fn from_map(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut p = Self::new(
            get("BUI_C_BASE").unwrap_or_else(|| DEFAULT_BASE.to_string()),
            get("BUI_C_UNIT_DIR").unwrap_or_else(|| DEFAULT_UNIT_DIR.to_string()),
        );
        if let Some(run) = get("BUI_C_RUN_DIR") {
            p.run_dir = PathBuf::from(run);
        }
        p
    }
    /// 生产入口（`cli::run()`）是全 crate 唯一读进程环境变量的地方。
    pub fn from_env() -> Self {
        Self::from_map(|k| std::env::var(k).ok())
    }
    pub fn profiles(&self) -> PathBuf {
        self.base.join("profiles.json")
    }
    pub fn config(&self) -> PathBuf {
        self.base.join("config.json")
    }
    pub fn runtime(&self) -> PathBuf {
        self.base.join("runtime.json")
    }
    pub fn bin_dir(&self) -> PathBuf {
        self.base.join("bin")
    }
    pub fn singbox(&self) -> PathBuf {
        self.bin_dir().join("sing-box")
    }
    pub fn unit(&self, name: &str) -> PathBuf {
        self.unit_dir.join(name)
    }
    /// 进程锁 `/run/bui-c.lock`（spec §8.3）：改配置的路径互斥，巡检遇锁跳过。
    pub fn lock(&self) -> PathBuf {
        self.run_dir.join("bui-c.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_layout_matches_spec() {
        let p = Paths::new("/opt/bui-c", "/etc/systemd/system");
        assert_eq!(p.profiles(), PathBuf::from("/opt/bui-c/profiles.json"));
        assert_eq!(p.config(), PathBuf::from("/opt/bui-c/config.json"));
        assert_eq!(p.runtime(), PathBuf::from("/opt/bui-c/runtime.json"));
        assert_eq!(p.singbox(), PathBuf::from("/opt/bui-c/bin/sing-box"));
        assert_eq!(
            p.unit(UNIT_TIMER),
            PathBuf::from("/etc/systemd/system/bui-c.timer")
        );
        assert_eq!(p.lock(), PathBuf::from("/run/bui-c.lock"));
    }
    #[test]
    fn overrides_come_from_the_injected_lookup() {
        // 不碰进程环境变量：cargo 默认多线程跑测试，set_var 会污染同进程的其它测试
        let p = Paths::from_map(|k| match k {
            "BUI_C_BASE" => Some("/run/bui-c-test".to_string()),
            "BUI_C_UNIT_DIR" => Some("/run/units".to_string()),
            "BUI_C_RUN_DIR" => Some("/run/bui-c-test-run".to_string()),
            _ => None,
        });
        assert_eq!(p.base, PathBuf::from("/run/bui-c-test"));
        assert_eq!(p.unit("x"), PathBuf::from("/run/units/x"));
        assert_eq!(p.lock(), PathBuf::from("/run/bui-c-test-run/bui-c.lock"));
        // 查不到就用默认值
        assert_eq!(
            Paths::from_map(|_| None),
            Paths::new("/opt/bui-c", "/etc/systemd/system")
        );
    }
}
