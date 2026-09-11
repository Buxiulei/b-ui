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

impl Paths {
    /// 服务端默认布局（C3 约定）。
    pub fn default_server() -> Self {
        Self {
            base_dir: "/opt/b-ui".into(),
            certs_dir: "/opt/b-ui/certs".into(),
            bin_dir: "/opt/b-ui/bin".into(),
        }
    }
}
