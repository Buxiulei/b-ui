//! 渲染器写进内核配置的绝对路径。
use std::path::PathBuf;

/// 渲染器嵌入内核配置的绝对路径集合。
#[derive(Debug, Clone)]
pub struct Paths {
    /// 安装根目录，如 `/opt/b-ui`
    pub base_dir: PathBuf,
    /// 证书目录，如 `/opt/b-ui/certs`
    pub certs_dir: PathBuf,
    /// 内核二进制目录，如 `/opt/b-ui/bin`
    pub bin_dir: PathBuf,
}

/// Hysteria2 `auth.command` 指向的钩子入口名（调研 H15）。
///
/// 它是 `<bin>/bui` 的符号链接，**同名即参数**：内核的 `exec.Command(a.Cmd, addr, auth, tx)`
/// 不过 shell、不拆空格，所以 `auth.command` 只能是一个不带参数的可执行路径；`bui` 按
/// argv[0] 认出这个名字就直接进钩子。
pub const AUTH_HOOK_BIN: &str = "bui-auth-hook";

impl Paths {
    /// 服务端默认布局（C3 约定）。
    pub fn default_server() -> Self {
        Self {
            base_dir: "/opt/b-ui".into(),
            certs_dir: "/opt/b-ui/certs".into(),
            bin_dir: "/opt/b-ui/bin".into(),
        }
    }

    /// `<bin>/bui-auth-hook`：写进两份 Hysteria2 配置的 `auth.command`，
    /// 同时也是对账渲染的符号链接路径（目标是同目录的相对 `bui`）。
    pub fn auth_hook_bin(&self) -> PathBuf {
        self.bin_dir.join(AUTH_HOOK_BIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_hook_entry_lives_next_to_bui_and_takes_no_arguments() {
        let p = Paths::default_server();
        assert_eq!(
            p.auth_hook_bin(),
            PathBuf::from("/opt/b-ui/bin/bui-auth-hook")
        );
        assert_eq!(p.auth_hook_bin().parent(), Some(p.bin_dir.as_path()));
        assert!(!AUTH_HOOK_BIN.contains(char::is_whitespace));
    }
}
