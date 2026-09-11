//! 上游 URL 与节点 URI 的解析器。
pub mod node_uri;
pub mod upstream;

pub use node_uri::node_uri;
pub use upstream::{upstream_url, UpstreamInput};

/// 解析失败原因（文案对照 v3 `server/residential-helper.sh` 的 `parse_url`）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// 写了 scheme 但不是 socks5 / socks5h / http
    #[error("不支持的代理协议 {0}://，只支持 socks5:// 与 http://")]
    Scheme(String),
    /// 四种支持的凭据格式都对不上
    #[error("无法解析凭据格式")]
    Format,
    /// 端口不是 1-65535 的十进制数
    #[error("端口必须在 1-65535：{0}")]
    Port(String),
    /// 其它（换行、空字段、节点 URI 字段缺失等）
    #[error("{0}")]
    Other(String),
}
