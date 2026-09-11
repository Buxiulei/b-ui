use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("命令 {prog} 执行失败（退出码 {code}）：{stderr}")]
    Command {
        prog: String,
        code: i32,
        stderr: String,
    },
    #[error("读写 {path} 失败：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("解析 {what} 失败：{detail}")]
    Parse { what: String, detail: String },
    #[error("请求 {url} 失败：{detail}")]
    Net { url: String, detail: String },
    #[error("校验失败：{0}")]
    Verify(String),
    #[error("{0}")]
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }
    pub fn parse(what: &str, detail: impl Into<String>) -> Self {
        Error::Parse {
            what: what.to_string(),
            detail: detail.into(),
        }
    }
    pub fn msg(m: impl Into<String>) -> Self {
        Error::Msg(m.into())
    }
}

/// 日志与错误里只保留 scheme://host 与路径首段。
/// 订阅 URL 的最后一段是用户名（等价凭据），绝不能进日志。
pub fn redact_url(raw: &str) -> String {
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) if !s.is_empty() && !r.is_empty() => (s, r),
        _ => return "<非法 URL>".to_string(),
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut it = rest.splitn(3, '/');
    let host = it.next().unwrap_or("");
    if host.is_empty() {
        return "<非法 URL>".to_string();
    }
    match it.next() {
        None => format!("{scheme}://{host}"),
        Some(first) => format!("{scheme}://{host}/{first}/…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redact_keeps_host_drops_secret_path() {
        assert_eq!(
            redact_url("https://panel.example.com/api/sub/alice"),
            "https://panel.example.com/api/…"
        );
        assert_eq!(
            redact_url("https://panel.example.com/api/nodes/alice?x=1"),
            "https://panel.example.com/api/…"
        );
        assert_eq!(
            redact_url("https://panel.example.com/packages/manifest.json"),
            "https://panel.example.com/packages/…"
        );
        assert_eq!(
            redact_url("https://panel.example.com"),
            "https://panel.example.com"
        );
        assert_eq!(redact_url("not a url"), "<非法 URL>");
    }
    #[test]
    fn command_error_message_is_chinese_and_has_code() {
        let e = Error::Command {
            prog: "systemctl".into(),
            code: 5,
            stderr: "boom".into(),
        };
        assert_eq!(e.to_string(), "命令 systemctl 执行失败（退出码 5）：boom");
    }
}
