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
    /// 粘贴的内容一行节点都解析不出来。`schemes` 已脱敏去重（只留到 `://`），
    /// `skipped` 是无法解析的行数（去重前）。
    #[error("{}", no_nodes_display(*.skipped, .schemes, *.has_http))]
    NoNodes {
        skipped: usize,
        schemes: Vec<String>,
        has_http: bool,
    },
    /// 用法错误：非终端下没加 `-y`、`--json` 没带 `-y` 这类。`finish()` 映射到退出码 2，
    /// 与 clap 的用法错误同码（spec §0.2 R15）。
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Msg(String),
}

/// [`Error::NoNodes`] 两个出口共用的主句（不带任何「下一步去哪」的指引）。
/// 菜单里前面还有「  失败：」，常见的一两种 scheme 时整行要在 80 列内。
pub fn no_nodes_summary(skipped: usize, schemes: &[String]) -> String {
    format!(
        "{skipped} 行都不是 hysteria2:// 或 vless:// 链接（{}）",
        schemes.join("、")
    )
}

/// 命令行（`bui-c import -`）看到的全文：有 http(s) 行时指到 `--sub`。
/// 菜单 `[3]` 不用它，自己拿变体组织两行（不提命令行）。
fn no_nodes_display(skipped: usize, schemes: &[String], has_http: bool) -> String {
    let mut msg = no_nodes_summary(skipped, schemes);
    if has_http {
        msg.push_str("；订阅地址请用 bui-c import --sub <url>");
    }
    msg
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
    pub fn usage(m: impl Into<String>) -> Self {
        Error::Usage(m.into())
    }
    /// 退出码（spec §0.2 R15）：用法错误 2，其余执行失败 1。「不是 root」在 `run()` 里
    /// 也返回 2，不经这里。
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Usage(_) => 2,
            _ => 1,
        }
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
    fn usage_errors_map_to_exit_code_two() {
        // spec §0.2 R15：2 = 用法错误（与 clap 同码），1 = 执行失败
        assert_eq!(Error::usage("--json 要和 -y 一起用").exit_code(), 2);
        assert_eq!(
            Error::usage("x").to_string(),
            "x",
            "用法错误的文案不加前缀，finish 自己写「错误：」"
        );
        assert_eq!(Error::msg("节点 nope 不存在").exit_code(), 1);
        assert_eq!(Error::Verify("check 不通过".into()).exit_code(), 1);
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
