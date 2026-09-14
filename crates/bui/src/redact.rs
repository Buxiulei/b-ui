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

/// 四个免鉴权订阅端点的路径前缀。末段就在它们后面，见 [`sub_path`]。
const SUB_PREFIXES: [&str; 4] = [
    "/api/sub/",
    "/api/subscription/",
    "/api/clash/",
    "/api/nodes/",
];

/// 路径末段里还算「同一段」的字节（用户名允许字母/数字/中文/`_`/`-`/`.`，token 是十六进制，
/// 百分号编码的用户名带 `%`）。其余（`/`、`?`、`#`、空格、引号、逗号……）一律当分隔符。
fn is_segment_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, b'_' | b'-' | b'.' | b'~' | b'%' | b'+' | b':')
        || b >= 0x80
}

/// 把 `/api/{sub,subscription,clash,nodes}/<段>` 里的 `<段>` 换成 `***`。
///
/// 那四个端点**免鉴权**、响应体里就是 hy2 明文密码与 vless uuid，所以路径末段本身就是凭据
/// （随机订阅 token，或宽限期内的用户名）。整条 URI 进日志之前必须过这里：`TraceLayer` 的
/// span 会把 URI 原样记进 `--log debug` 的每一行（见 `crate::api::router`），日志哨兵也会把
/// 受管单元的日志原文放进事件的 `sample`。客户端侧早有同类实现（`bui-c` 的 `error::redact_url`），
/// 服务端这一侧补在这里（2026-09-14 裁决）。
///
/// 一行里出现多条就全换。前缀匹配对 ASCII 大小写不敏感：Caddy 记的是客户端原样的大小写，
/// 而它的 `path` 匹配器（渲染出的 `log_skip`）本身也不分大小写。末段照原样换成 `***`，
/// 查询串与后面的路径都保留。
pub fn sub_path(s: &str) -> String {
    // 只有 ASCII 字节会变，字节长度与字符边界都不动 ⇒ 下面两个串可以用同一套下标
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match SUB_PREFIXES.iter().find(|p| lower[i..].starts_with(**p)) {
            Some(p) => {
                out.push_str(&s[i..i + p.len()]);
                i += p.len();
                let seg = s.as_bytes()[i..]
                    .iter()
                    .take_while(|b| is_segment_byte(**b))
                    .count();
                if seg > 0 {
                    out.push_str("***");
                    i += seg;
                }
            }
            None => {
                // 按字符推进：`i` 必须始终落在 UTF-8 边界上
                let c = s[i..].chars().next().expect("i 在字符边界上");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// 整行日志脱敏：先过 [`sub_path`]（订阅链接的末段就是凭据），再按空格切分、含 `@` 的片段
/// 逐个过 [`url_credentials`]（`socks5://u:p@h:port`、`"http://u:p@h/x"`、`u:p@h:port`
/// 都能认）。日志哨兵把日志原文放进事件的 `sample` 之前必须过它。
pub fn line(s: &str) -> String {
    sub_path(s)
        .split(' ')
        .map(|tok| {
            if tok.contains('@') {
                url_credentials(tok)
            } else {
                tok.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    /// 四个免鉴权端点的路径末段就是凭据：token 与用户名都必须换掉，路径前缀与查询串留着
    /// （否则日志里连「谁在打哪个端点」都看不出来，排障成本换不来更多安全）。
    #[test]
    fn sub_path_masks_the_last_segment_of_all_four_endpoints() {
        let tok = "0123456789abcdef0123456789abcdef";
        assert_eq!(sub_path(&format!("/api/sub/{tok}")), "/api/sub/***");
        assert_eq!(
            sub_path(&format!("/api/subscription/{tok}")),
            "/api/subscription/***"
        );
        assert_eq!(sub_path(&format!("/api/clash/{tok}")), "/api/clash/***");
        assert_eq!(sub_path(&format!("/api/nodes/{tok}")), "/api/nodes/***");
        // 宽限期内的用户名链接同样是凭据
        assert_eq!(sub_path("/api/sub/alice"), "/api/sub/***");
        // 查询串、后续路径、百分号编码的用户名
        assert_eq!(sub_path("/api/sub/alice?x=1"), "/api/sub/***?x=1");
        assert_eq!(sub_path("/api/clash/_E5_BC_A0"), "/api/clash/***");
        assert_eq!(sub_path("/api/sub/%E5%BC%A0"), "/api/sub/***");
        assert_eq!(sub_path("/api/sub/张三"), "/api/sub/***");
    }

    /// 大小写：Caddy 记的是客户端原样的大小写（它的 path 匹配器不分大小写，所以那些请求
    /// 照样命中面板块），`is_sub_token` 只认小写 hex 不等于日志里只会出现小写。
    #[test]
    fn sub_path_is_case_insensitive_on_the_prefix_and_the_segment() {
        assert_eq!(sub_path("/API/SUB/ALICE"), "/API/SUB/***");
        assert_eq!(
            sub_path("/Api/Subscription/0123456789ABCDEF0123456789abcdef"),
            "/Api/Subscription/***"
        );
    }

    /// 一行里多条 URL 全换；不是这四个端点的路径一个字不动。
    #[test]
    fn sub_path_handles_multiple_urls_and_leaves_other_paths_alone() {
        let out = sub_path(
            "GET \"https://example.com/api/sub/alice\" then https://example.com/api/nodes/bob?v=2 done",
        );
        assert_eq!(
            out,
            "GET \"https://example.com/api/sub/***\" then https://example.com/api/nodes/***?v=2 done"
        );
        assert!(!out.contains("alice") && !out.contains("bob"), "{out}");
        // 前缀相近但不是那四个端点
        assert_eq!(sub_path("/api/users/alice"), "/api/users/alice");
        assert_eq!(sub_path("/api/subs/alice"), "/api/subs/alice");
        // 末段本来就空：只留前缀，不凭空造一个 ***
        assert_eq!(sub_path("/api/sub/"), "/api/sub/");
        assert_eq!(sub_path("/api/sub/?x=1"), "/api/sub/?x=1");
        assert_eq!(sub_path("plain text, no creds"), "plain text, no creds");
    }

    /// `line` 是哨兵与 `TraceLayer` 共用的入口：userinfo 与订阅末段都得在这一道里掉。
    #[test]
    fn line_also_masks_subscription_segments() {
        let out = line("GET /api/subscription/alice via socks5://u:pw1@isp.example.net:10007");
        assert_eq!(
            out,
            "GET /api/subscription/*** via socks5://***:***@isp.example.net:10007"
        );
    }

    /// 一整行日志里每个带 userinfo 的片段都要脱敏（哨兵把日志原文放进事件的 `sample`）
    #[test]
    fn line_redacts_every_credential_bearing_token() {
        // 密码夹具用唯一串：写成 `p2` 会误中脱敏后照样保留的 `isp2.example.net`
        let s = "dial socks5://user1:pw1@isp1.example.net:10007 failed, retry \
                 \"http://u2:pw2@isp2.example.net:44445/x\" and user1:pw1@isp3.example.net:10007";
        let out = line(s);
        assert!(!out.contains("pw1") && !out.contains("pw2"), "{out}");
        assert!(
            out.contains("socks5://***:***@isp1.example.net:10007"),
            "{out}"
        );
        assert!(
            out.contains("\"http://***:***@isp2.example.net:44445/x\""),
            "{out}"
        );
        assert!(out.contains("***:***@isp3.example.net:10007"), "{out}");
        assert_eq!(line("plain text, no creds"), "plain text, no creds");
    }
}
