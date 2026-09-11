//! 默认住宅分流关键字表。
//!
//! 移植来源：`server/residential-helper.sh` 的 `DEFAULT_DOMAINS`（v3.6.x，88-111 行）。
//! 语义是 sing-box `domain_keyword` / Clash `DOMAIN-KEYWORD` 的**子串**匹配，所以：
//! 不写裸 `google`（会把被拒的 `www.google.com` 也送进住宅腿），改按子域枚举；
//! 不写裸 `x.ai`（子串会误伤大量域名），用 `api.x.ai`；不写裸 `oai`（过宽），
//! 用 `oaistatic` + `oaiusercontent`；数字串写成 `api.123169` / `cf.999831`
//! 而不是裸数字（裸数字会命中任何含这串数字的域名）。
//! 支付处理商（Stripe / PayPal）故意不进表：进表反而让 ChatGPT Plus / Claude Pro
//! 的付款页打不开。
//!
//! 表的顺序与 v3 一致，改动必须同步 v3 侧，测试逐项比对。

/// 住宅分流的默认关键字（67 条）。[`ResidentialGroup::keywords`] 为 `None` 时跟随本表。
///
/// [`ResidentialGroup::keywords`]: crate::model::ResidentialGroup::keywords
pub const DEFAULT_KEYWORDS: &[&str] = &[
    // OpenAI
    "openai",
    "chatgpt",
    "oaistatic",
    "oaiusercontent",
    "sora.com",
    // Anthropic
    "anthropic",
    "claude",
    // Google AI + 账号 + 支付（不含裸 google）
    "gemini.google",
    "aistudio",
    "generativelanguage",
    "makersuite",
    "notebooklm",
    "jules.google",
    "labs.google",
    "antigravity",
    "idx.google",
    "ai.google.dev",
    "deepmind",
    "cloudcode",
    "googleapis",
    "gstatic",
    "googleusercontent",
    "clients6.google",
    "accounts.google",
    "myaccount.google",
    "apis.google",
    "ogs.google",
    "pay.google",
    "payments.google",
    "wallet.google",
    "one.google",
    // 其它 AI
    "grok",
    "api.x.ai",
    "githubcopilot",
    "cursor",
    "perplexity",
    "mistral",
    "cohere",
    "huggingface",
    "replicate",
    "together",
    "groq",
    "statsig",
    "featuregates",
    // 住宅 IP 检测站（面板体检与用户自查都得经住宅腿才准）
    "ippure",
    "ipquery",
    "ipinfo",
    "ip-api",
    "ping0",
    "ip.sb",
    "browserleaks",
    "whoer",
    "ipleak",
    "scamalytics",
    "ipqualityscore",
    "ip2location",
    "iplocation",
    "whatismyipaddress",
    "ipdata",
    "ipapi",
    "ipregistry",
    "ip.skk.moe",
    "ping.pe",
    // ippure.com 网页的后端域（签名 API 在 api.123169.xyz，辅助 cf.999831.xyz / myip.ipip.net）
    "api.123169",
    "ipip.net",
    "cf.999831",
    // 既有（非 AI）
    "tiktok",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 v3 `server/residential-helper.sh` 的 `DEFAULT_DOMAINS` 逐项（含顺序）一致。
    #[test]
    fn matches_v3_helper_table() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../server/residential-helper.sh"
        ))
        .unwrap();
        let start = src.find("DEFAULT_DOMAINS=(").unwrap();
        let end = src[start..].find(')').unwrap() + start;
        let v3: Vec<&str> = src[start..end].split('"').skip(1).step_by(2).collect();
        assert_eq!(
            DEFAULT_KEYWORDS.to_vec(),
            v3,
            "与 v3 表逐项一致（顺序也一致）"
        );
        assert_eq!(DEFAULT_KEYWORDS.len(), 67);
    }

    /// 关键字是 `domain_keyword` 子串语义，重复项只会让规则表变长。
    #[test]
    fn no_duplicates() {
        let mut s = DEFAULT_KEYWORDS.to_vec();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), DEFAULT_KEYWORDS.len());
    }
}
