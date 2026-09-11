//! 日志与错误信息脱敏。凭据、密钥、密码一律不进 journal。

/// 把 URL（或 `u:p@host:port` 形态）里的 userinfo 换成 `***:***`。
pub fn url_credentials(s: &str) -> String {
    let (scheme, rest) = match s.find("://") {
        Some(i) => (&s[..i + 3], &s[i + 3..]),
        None => ("", s),
    };
    // userinfo 只可能出现在第一个 '/' 之前
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];
    match authority.rfind('@') {
        Some(at) => {
            let userinfo = &authority[..at];
            let masked = if userinfo.contains(':') {
                "***:***"
            } else {
                "***"
            };
            format!("{scheme}{masked}@{}{tail}", &authority[at + 1..])
        }
        None => s.to_string(),
    }
}

/// 只暴露长度，不暴露内容。
pub fn secret(s: &str) -> String {
    if s.is_empty() {
        "<empty>".to_string()
    } else {
        format!("***({} 字符)", s.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_socks_url() {
        assert_eq!(
            url_credentials("socks5://user:pa:ss@isp.example.net:10007"),
            "socks5://***:***@isp.example.net:10007"
        );
    }

    #[test]
    fn redacts_http_url_with_path() {
        assert_eq!(
            url_credentials("http://u-x-ip-1:bz@isp.example.net:44445/x?y=1"),
            "http://***:***@isp.example.net:44445/x?y=1"
        );
    }

    #[test]
    fn redacts_at_form_without_scheme() {
        assert_eq!(
            url_credentials("u:p@h.example:1080"),
            "***:***@h.example:1080"
        );
    }

    #[test]
    fn leaves_credential_free_url_untouched() {
        assert_eq!(
            url_credentials("https://example.com/a?b=1"),
            "https://example.com/a?b=1"
        );
        assert_eq!(url_credentials("127.0.0.1:2080"), "127.0.0.1:2080");
    }

    #[test]
    fn secret_never_leaks_the_value() {
        assert_eq!(secret("hunter2hunter2"), "***(14 字符)");
        assert_eq!(secret(""), "<empty>");
        assert!(!secret("pw-alice-01").contains("alice"));
    }
}
