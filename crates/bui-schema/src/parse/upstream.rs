//! 住宅上游 URL 解析：四种格式 → `UpstreamInput`。
//!
//! 判定顺序逐条移植 v3 `server/residential-helper.sh:132-198 parse_url`。
use super::ParseError;
use crate::model::UpstreamKind;

/// 一条上游的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInput {
    /// `None` = 没写 scheme，需要自动探测（v3 的 `RESI_TYPE=auto`）
    pub kind: Option<UpstreamKind>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

/// 解析一条上游 URL。
///
/// 支持 `socks5://u:p@h:port`、`socks5h://…`、`http://u:p@h:port`、
/// `host:port:user:pass`（供应商 IP 列表整行）、`u:p@h:port`（不带 scheme）。
pub fn upstream_url(raw: &str) -> Result<UpstreamInput, ParseError> {
    // 供应商邮件与 CSV 常带首尾空白和成对引号，先剥掉
    let s = strip_quotes(raw.trim()).trim();
    let (kind, s) = split_scheme(s)?;

    // 换行无法安全写进 curl 配置文件（会注入额外指令），视为非法（v3 R3）
    if s.contains('\n') || s.contains('\r') {
        return Err(ParseError::Other("凭据含换行符，非法".into()));
    }

    // 两种形态的密码都可以含 : 与 @，只靠字符判不出来，所以按"有没有写 scheme"定优先级：
    //   没写 scheme → 供应商 IP 列表整行（host:port:user:pass）优先，其密码含 @ 很常见
    //   写了 scheme → 按 URL 语义，先试 user:pass@host:port（以最后一个 @ 切分）
    let csv = csv_form(s);
    let at = at_form(s);
    // decode = 走的是 URL 形态（写了 scheme 且按 user:pass@host:port 切开），
    // 此时 userinfo 按 RFC 3986 是百分号编码的；两种粘贴形态原样不解码
    let ((host, port, username, password), decode) = match csv {
        Some(v) if kind.is_none() || at.is_none() => (v, false),
        _ => match at {
            Some(v) => (v, kind.is_some()),
            None => return Err(ParseError::Format),
        },
    };

    if host.is_empty() || username.is_empty() || password.is_empty() {
        return Err(ParseError::Other("解析结果包含空字段".into()));
    }
    let (username, password) = if decode {
        (percent_decode(username), percent_decode(password))
    } else {
        (username.to_string(), password.to_string())
    };
    // `%0A` 解出来就是换行，上面那次检查挡不住，解码后再挡一次
    if [&username, &password]
        .iter()
        .any(|v| v.contains('\n') || v.contains('\r'))
    {
        return Err(ParseError::Other("凭据含换行符，非法".into()));
    }

    Ok(UpstreamInput {
        kind,
        host: host.to_string(),
        port: parse_port(port)?,
        username,
        password,
    })
}

/// userinfo 百分号解码：没有 `%XX` 序列时原样返回（孤立的 `%` 也原样，
/// `percent_decode_str` 对非法序列就是照抄），解不出 UTF-8 时退回原串。
fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

/// 去掉成对的首尾引号。
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// 剥掉 scheme；https:// / socks4:// 等一律拒绝，别把 scheme 当用户名静默解析。
fn split_scheme(url: &str) -> Result<(Option<UpstreamKind>, &str), ParseError> {
    let lower = url.to_ascii_lowercase();
    // 大小写不敏感；socks5h:// 是 socks5:// 的别名（供应商文档普遍写 socks5h）
    let (kind, rest) = if lower.starts_with("socks5h://") {
        (Some(UpstreamKind::Socks5), &url[10..])
    } else if lower.starts_with("socks5://") {
        (Some(UpstreamKind::Socks5), &url[9..])
    } else if lower.starts_with("http://") {
        (Some(UpstreamKind::Http), &url[7..])
    } else {
        (None, url)
    };
    if let Some(scheme) = leading_scheme(rest) {
        return Err(ParseError::Scheme(scheme.to_string()));
    }
    Ok((kind, rest))
}

/// 取开头的 `scheme://`（对照 v3 的 `^[A-Za-z][A-Za-z0-9+.-]*://`）。
fn leading_scheme(s: &str) -> Option<&str> {
    let scheme = &s[..s.find("://")?];
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        .then_some(scheme)
}

/// `host:port:user:pass`（对照 `^[^:@]+:[0-9]+:[^:]+:.+$`）。
fn csv_form(s: &str) -> Option<(&str, &str, &str, &str)> {
    let (host, rest) = s.split_once(':')?;
    if host.is_empty() || host.contains('@') {
        return None;
    }
    let (port, rest) = rest.split_once(':')?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (user, pass) = rest.split_once(':')?;
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    Some((host, port, user, pass))
}

/// `user:pass@host:port`，以最后一个 `@` 切分（host:port 对照 `^[^:@/]+:[0-9]+$`）。
fn at_form(s: &str) -> Option<(&str, &str, &str, &str)> {
    let (userpass, hostport) = s.rsplit_once('@')?;
    let (host, port) = hostport.split_once(':')?;
    if host.is_empty() || host.contains('/') {
        return None;
    }
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // 缺密码（没有 `:`）在 v3 里是单独一条报错，这里统一落到 Format
    let (user, pass) = userpass.split_once(':')?;
    Some((host, port, user, pass))
}

/// 端口：位数按原串判（`065535` 这种"补零后合法"的怪输入一律拒），再看范围。
fn parse_port(raw: &str) -> Result<u16, ParseError> {
    let bad = || ParseError::Port(raw.to_string());
    if raw.is_empty() || raw.len() > 5 || (raw.len() > 1 && raw.starts_with('0')) {
        return Err(bad());
    }
    let n: u32 = raw.parse().map_err(|_| bad())?;
    if !(1..=65535).contains(&n) {
        return Err(bad());
    }
    Ok(n as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UpstreamKind;

    #[test]
    fn socks5_url() {
        let u = upstream_url("socks5://u:p@h.example:1080").unwrap();
        assert_eq!(u.kind, Some(UpstreamKind::Socks5));
        assert_eq!(
            (
                u.host.as_str(),
                u.port,
                u.username.as_str(),
                u.password.as_str()
            ),
            ("h.example", 1080, "u", "p")
        );
    }

    #[test]
    fn socks5h_alias_and_case() {
        assert_eq!(
            upstream_url("SOCKS5H://u:p@h:1").unwrap().kind,
            Some(UpstreamKind::Socks5)
        );
    }

    #[test]
    fn http_url_with_plus_and_hyphen_password() {
        let u = upstream_url("http://user-x-ip-1.2.3.4:+bz/x@isp.example:10007").unwrap();
        assert_eq!(u.kind, Some(UpstreamKind::Http));
        assert_eq!(u.username, "user-x-ip-1.2.3.4");
        assert_eq!(u.password, "+bz/x");
    }

    #[test]
    fn csv_form_password_with_at() {
        let u = upstream_url("h:1084:u:p@x:5").unwrap();
        assert_eq!(u.kind, None);
        assert_eq!(u.host, "h");
        assert_eq!(u.port, 1084);
        assert_eq!(u.username, "u");
        assert_eq!(u.password, "p@x:5");
    }

    #[test]
    fn at_form_without_scheme() {
        let u = upstream_url("u:p@h:1080").unwrap();
        assert_eq!(u.kind, None);
        assert_eq!(u.host, "h");
    }

    #[test]
    fn quoted_and_padded() {
        assert_eq!(upstream_url("  \"socks5://u:p@h:1\"  ").unwrap().port, 1);
    }

    #[test]
    fn rejects_https_and_socks4() {
        assert_eq!(
            upstream_url("https://u:p@h:1"),
            Err(ParseError::Scheme("https".into()))
        );
        assert_eq!(
            upstream_url("socks4://u:p@h:1"),
            Err(ParseError::Scheme("socks4".into()))
        );
    }

    #[test]
    fn rejects_bad_port_and_missing_password() {
        assert!(matches!(
            upstream_url("socks5://u:p@h:99999"),
            Err(ParseError::Port(_))
        ));
        assert!(matches!(
            upstream_url("socks5://u:p@h:08080"),
            Err(ParseError::Port(_))
        ));
        assert_eq!(upstream_url("socks5://u@h:1"), Err(ParseError::Format));
    }

    #[test]
    fn rejects_newline_in_credentials() {
        assert!(upstream_url("socks5://u:p\nq@h:1").is_err());
    }

    /// URL 形态的 userinfo 按 percent-encoding 解码：密码里的 `=` 必须写成 `%3D`，
    /// 不解码就会拿着字面量 `%3D` 去认证，上游回「incorrect user name or password」。
    #[test]
    fn url_form_percent_decodes_userinfo() {
        let u = upstream_url("socks5://u:p%3D@h:1080").unwrap();
        assert_eq!(u.password, "p=");
        let u = upstream_url("http://u:p%40x@h:1080").unwrap();
        assert_eq!(u.kind, Some(UpstreamKind::Http));
        assert_eq!(u.host, "h");
        assert_eq!(u.password, "p@x");
        // 用户名同样解码
        let u = upstream_url("socks5h://user%2Dname:p@h:1").unwrap();
        assert_eq!(u.username, "user-name");
    }

    /// 已存凭据不受影响：没有 `%XX` 序列时原样，孤立的 `%` 也原样（不报错、不吞字符）。
    #[test]
    fn percent_decoding_leaves_existing_credentials_alone() {
        let u = upstream_url("socks5://user-x-ip-1.2.3.4:+bz/x@isp.example:10007").unwrap();
        assert_eq!(u.username, "user-x-ip-1.2.3.4");
        assert_eq!(u.password, "+bz/x");
        let u = upstream_url("socks5://u:100%pw@h:1").unwrap();
        assert_eq!(u.password, "100%pw");
    }

    /// 两种非 URL 粘贴格式（供应商整行 / 无 scheme）原样不解码：那里的 `%` 是密码本身。
    #[test]
    fn paste_forms_are_not_percent_decoded() {
        let u = upstream_url("h:1084:u:p%3Dx").unwrap();
        assert_eq!(u.password, "p%3Dx");
        let u = upstream_url("u:p%3Dx@h:1080").unwrap();
        assert_eq!(u.password, "p%3Dx");
    }

    /// `%0A` 解出来是换行：解码后要再挡一次，否则 R3 的换行拒绝被绕开。
    #[test]
    fn rejects_percent_encoded_newline() {
        assert!(upstream_url("socks5://u:p%0Aq@h:1").is_err());
        assert!(upstream_url("socks5://u%0D:p@h:1").is_err());
    }
}
