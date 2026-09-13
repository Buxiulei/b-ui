//! 订阅链接的随机 token 与旧「用户名链接」的宽限期（2026-09-14 裁决）。
//!
//! 四个免鉴权端点（`/api/sub`、`/api/subscription`、`/api/clash`、`/api/nodes`）的响应体里
//! 有 hy2 明文密码与 vless uuid，所以**路径末段本身就是凭据**。仓库是公开的、git 历史里有
//! 真实域名与真实用户名，而证书透明日志本来就公开所有子域 —— 域名那一半补不回来，于是把
//! 末段换成每用户一个随机 token（[`crate::model::User::sub_token`]，32 位小写十六进制 =
//! 16 字节随机）：不可猜、可轮换。
//!
//! 旧的「用户名链接」只在全局宽限期（[`crate::model::SystemSettings::legacy_sub_until`]）内
//! 还认，且那个用户没被轮换过（[`crate::model::User::legacy_sub_disabled`]）。全新装机不设
//! 宽限期 ⇒ 用户名链接从来不可用；v3 导入与老 v4 安装补 token 时给
//! [`LEGACY_SUB_GRACE_DAYS`] 天。

use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// 旧「用户名链接」的宽限期天数。v3 导入（[`crate::v3::import`]）与老 v4 安装的启动补齐
/// 都用它算截止时刻，运维可以用 `bui set legacy-sub off` 提前收口。
pub const LEGACY_SUB_GRACE_DAYS: i64 = 7;

/// 订阅 token 的十六进制长度（16 字节随机 → 32 字符）。
pub const SUB_TOKEN_HEX_LEN: usize = 32;

/// 一个新的随机订阅 token：16 字节随机 → 32 位小写十六进制。
pub fn new_sub_token() -> String {
    hex::encode(rand::random::<[u8; SUB_TOKEN_HEX_LEN / 2]>())
}

/// 「这一段是不是订阅 token」的严格判据：长度 [`SUB_TOKEN_HEX_LEN`]、只含 `0-9a-f`。
///
/// 大写十六进制**不算**：[`new_sub_token`] 只产小写，放宽等于让同一个 token 有两种写法，
/// 而四个端点要按这个判据在「token」与「用户名」两条路之间分流。
pub fn is_sub_token(s: &str) -> bool {
    s.len() == SUB_TOKEN_HEX_LEN
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 宽限期截止时刻 = `now` + [`LEGACY_SUB_GRACE_DAYS`] 天，RFC3339（UTC，秒级）。
pub fn legacy_sub_deadline(now: OffsetDateTime) -> String {
    let t = now + Duration::days(LEGACY_SUB_GRACE_DAYS);
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// 一个用户的四条订阅地址（[`sub_urls`] 的产物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubUrls {
    /// `/api/sub/<token>` —— v2rayN 用的 base64 URI 列表
    pub uri: String,
    /// `/api/subscription/<token>` —— 完整 sing-box 配置
    pub singbox: String,
    /// `/api/clash/<token>` —— mihomo YAML
    pub clash: String,
    /// `/api/nodes/<token>` —— 节点集合 + 分流规则 JSON
    pub nodes: String,
}

/// 把「域名 + token」拼成那四条订阅地址（装机收尾输出与面板共用，免得两边各拼一遍）。
pub fn sub_urls(domain: &str, token: &str) -> SubUrls {
    let at = |path: &str| format!("https://{domain}/api/{path}/{token}");
    SubUrls {
        uri: at("sub"),
        singbox: at("subscription"),
        clash: at("clash"),
        nodes: at("nodes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    #[test]
    fn a_new_token_is_32_lowercase_hex_and_never_repeats() {
        let t = new_sub_token();
        assert_eq!(t.len(), SUB_TOKEN_HEX_LEN);
        assert!(is_sub_token(&t), "{t}");
        assert_eq!(t, t.to_lowercase(), "只产小写：{t}");
        assert_ne!(new_sub_token(), new_sub_token(), "每次都要是新的随机值");
    }

    /// 四个端点按这个判据在「token」与「用户名」之间分流，所以边界必须是严格的：
    /// 31/33 位、大写、非 hex、以及任何长得像用户名的段都不算 token。
    #[test]
    fn is_sub_token_is_strict_about_length_case_and_alphabet() {
        assert!(is_sub_token("0123456789abcdef0123456789abcdef"));
        assert!(!is_sub_token("0123456789abcdef0123456789abcde"), "31 位");
        assert!(!is_sub_token("0123456789abcdef0123456789abcdef0"), "33 位");
        assert!(!is_sub_token("0123456789ABCDEF0123456789abcdef"), "大写");
        assert!(!is_sub_token("0123456789abcdef0123456789abcdeg"), "非 hex");
        assert!(!is_sub_token("0123456789abcdef 123456789abcdef"), "空格");
        assert!(!is_sub_token(""), "空段");
        assert!(!is_sub_token("alice"), "用户名");
    }

    #[test]
    fn the_grace_deadline_is_seven_days_out_in_rfc3339() {
        assert_eq!(LEGACY_SUB_GRACE_DAYS, 7);
        assert_eq!(
            legacy_sub_deadline(datetime!(2026-09-14 08:30:00 UTC)),
            "2026-09-21T08:30:00Z"
        );
        // 纳秒被抹平（与 `bui` 的 `util::fmt_rfc3339` 同口径，秒级）
        assert_eq!(
            legacy_sub_deadline(datetime!(2026-09-14 08:30:00.123456 UTC)),
            "2026-09-21T08:30:00Z"
        );
    }

    #[test]
    fn sub_urls_are_the_four_public_endpoints() {
        let u = sub_urls("example.com", "0123456789abcdef0123456789abcdef");
        assert_eq!(
            u,
            SubUrls {
                uri: "https://example.com/api/sub/0123456789abcdef0123456789abcdef".into(),
                singbox: "https://example.com/api/subscription/0123456789abcdef0123456789abcdef"
                    .into(),
                clash: "https://example.com/api/clash/0123456789abcdef0123456789abcdef".into(),
                nodes: "https://example.com/api/nodes/0123456789abcdef0123456789abcdef".into(),
            }
        );
    }
}
