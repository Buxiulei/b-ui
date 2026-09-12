//! 节点来源三种：面板 `/api/nodes/<user>`（首选，带服务端真实分流）、
//! 订阅 `/api/sub/<user>`（base64 URI 列表）、用户粘贴的 URI 行。
//!
//! 订阅与粘贴都拿不到住宅分流信息，统一回落 [`crate::profiles::default_split`]。

use crate::net::Net;
use crate::profiles::default_split;
use crate::{Error, Result};
use bui_schema::nodes::Node;
use bui_schema::render::SplitRules;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 面板与订阅的取文本超时；两个源顺序探测，最坏 2×15s（决策 1）。
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// `/api/nodes/<user>` 的响应体：P2 直接 serde 序列化 `nodes_for()` 与
/// `SplitRules::from_group()` 的结果（总纲裁决记录 2026-09-12），这里照样反序列化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodesPayload {
    pub user: String,
    pub split: SplitRules,
    pub nodes: Vec<Node>,
}

/// 一次取节点的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub user: String,
    pub split: SplitRules,
    pub nodes: Vec<Node>,
    /// 无法解析的行，已脱敏（只留前 24 字符）。
    pub skipped: Vec<String>,
}

pub fn nodes_url(base_url: &str, user: &str) -> String {
    format!("{}/api/nodes/{user}", base_url.trim_end_matches('/'))
}

/// 取 URL 最后一个非空路径段；订阅 URL 的末段就是用户名。
pub fn username_from_url(url: &str) -> String {
    let body = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path = body.split(['?', '#']).next().unwrap_or(body);
    match path.split_once('/') {
        Some((_host, rest)) => rest
            .rsplit('/')
            .find(|s: &&str| !s.is_empty())
            .unwrap_or("")
            .to_string(),
        None => String::new(),
    }
}

/// 从订阅 URL 取 `scheme://host[:port]`，回填 `Panel::base_url`。
///
/// 不回填的话每日自更新会跳过面板源、只打 GitHub（T12 `Cmd::Import` 的 sub 分支用它）。
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if scheme.is_empty() || host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// 首选来源：只有 `/api/nodes` 能拿到服务端真实的住宅分流规则。
pub fn from_panel<N: Net>(net: &N, base_url: &str, user: &str) -> Result<Fetched> {
    let url = nodes_url(base_url, user);
    let body = net.text(&url, FETCH_TIMEOUT)?;
    let p: NodesPayload =
        serde_json::from_str(&body).map_err(|e| Error::parse("/api/nodes 响应", e.to_string()))?;
    if p.nodes.is_empty() {
        return Err(Error::msg("面板返回的节点列表为空：该用户可能没有任何权益"));
    }
    Ok(Fetched {
        user: p.user,
        split: p.split,
        nodes: p.nodes,
        skipped: Vec::new(),
    })
}

/// 订阅来源：base64 的 URI 列表，拿不到分流信息，回落 [`default_split`]。
pub fn from_subscription<N: Net>(net: &N, url: &str) -> Result<Fetched> {
    let body = net.text(url, FETCH_TIMEOUT)?;
    let text = decode_base64_body(&body)?;
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut f = collect(&lines)?;
    f.user = username_from_url(url);
    Ok(f)
}

/// 粘贴来源：用户手抄的节点链接，没有用户名。
pub fn from_uris(lines: &[String]) -> Result<Fetched> {
    for l in lines {
        let t = l.trim();
        if t.starts_with("http://") || t.starts_with("https://") {
            return Err(Error::msg(
                "这是订阅地址，不是节点链接：用 `bui-c import --sub <url>`",
            ));
        }
    }
    collect(lines)
}

fn collect(lines: &[String]) -> Result<Fetched> {
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match bui_schema::parse::node_uri(t) {
            Ok(n) => nodes.push(n),
            // 跳过项只留前 24 字符：hysteria2:// 的 userinfo 段就是密码
            Err(_) => skipped.push(t.chars().take(24).collect()),
        }
    }
    if nodes.is_empty() {
        return Err(Error::msg(format!(
            "没有可用节点（{} 行无法解析，只支持 hysteria2:// 与 vless://）",
            skipped.len()
        )));
    }
    Ok(Fetched {
        user: String::new(),
        split: default_split(),
        nodes,
        skipped,
    })
}

/// 面板的 `/api/sub` 返回 base64；也容忍面板直接给明文 URI 列表。
///
/// base64 的四种口径（标准/urlsafe × 有无 padding）逐个试。用泛型 `try_one` 而不是
/// `&dyn base64::Engine` 的数组：`Engine` 带关联类型（`Config` / `DecodeEstimate`），
/// 不是 object-safe 的，写成 trait object 编译不过。
pub fn decode_base64_body(body: &str) -> Result<String> {
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("://") {
        return Ok(body.trim().to_string());
    }
    fn try_one<E: base64::Engine>(engine: &E, compact: &str) -> Option<String> {
        engine
            .decode(compact.as_bytes())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    }
    use base64::engine::general_purpose as gp;
    try_one(&gp::STANDARD, &compact)
        .or_else(|| try_one(&gp::STANDARD_NO_PAD, &compact))
        .or_else(|| try_one(&gp::URL_SAFE, &compact))
        .or_else(|| try_one(&gp::URL_SAFE_NO_PAD, &compact))
        .ok_or_else(|| Error::parse("订阅响应", "既不是 base64 也不是 URI 列表"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, FakeReply};
    use crate::profiles::default_split;
    use crate::testutil::{hy2_direct_node, reality_direct_node, split_keywords};
    use base64::Engine as _;
    use pretty_assertions::assert_eq;

    fn payload() -> NodesPayload {
        NodesPayload {
            user: "alice".into(),
            split: split_keywords(),
            nodes: vec![reality_direct_node(), hy2_direct_node()],
        }
    }

    #[test]
    fn nodes_url_normalises_trailing_slash() {
        assert_eq!(
            nodes_url("https://panel.example.com", "alice"),
            "https://panel.example.com/api/nodes/alice"
        );
        assert_eq!(
            nodes_url("https://panel.example.com/", "alice"),
            "https://panel.example.com/api/nodes/alice"
        );
    }

    #[test]
    fn from_panel_parses_payload_verbatim() {
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/nodes/alice",
            FakeReply::Text(serde_json::to_string(&payload()).unwrap()),
        );
        let f = from_panel(&n, "https://panel.example.com/", "alice").unwrap();
        assert_eq!(f.user, "alice");
        assert_eq!(f.nodes, vec![reality_direct_node(), hy2_direct_node()]);
        assert_eq!(
            f.split,
            split_keywords(),
            "住宅分流规则来自服务端，不用默认表"
        );
        assert!(f.skipped.is_empty());
    }

    #[test]
    fn from_panel_reports_bad_json_without_leaking_url() {
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/nodes/alice",
            FakeReply::Text("<html>502</html>".into()),
        );
        let e = from_panel(&n, "https://panel.example.com", "alice").unwrap_err();
        assert!(matches!(e, crate::Error::Parse { .. }), "{e}");
        assert!(
            !e.to_string().contains("alice"),
            "错误信息不能带用户名：{e}"
        );
    }

    #[test]
    fn from_subscription_decodes_base64_uri_list() {
        let uris = "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E\n\
                    vless://11111111-1111-4111-8111-111111111111@panel.example.com:10001?encryption=none&security=reality&sni=www.bing.com&fp=chrome&pbk=PUB&sid=0123456789abcdef&flow=xtls-rprx-vision&type=tcp#alice-Reality%E7%9B%B4%E8%BF%9E";
        let n = FakeNet::new();
        // 真实面板返回不带换行的一整段 base64；这里额外插换行验证容忍度
        let b64 = base64::engine::general_purpose::STANDARD.encode(uris);
        let wrapped = format!("{}\n{}\n", &b64[..20], &b64[20..]);
        n.route(
            "https://panel.example.com/api/sub/alice",
            FakeReply::Text(wrapped),
        );
        let f = from_subscription(&n, "https://panel.example.com/api/sub/alice").unwrap();
        assert_eq!(f.user, "alice", "用户名从订阅 URL 末段取");
        assert_eq!(f.nodes.len(), 2);
        assert_eq!(f.split, default_split());
        assert!(f.skipped.is_empty());
    }

    #[test]
    fn from_subscription_skips_unparsable_lines_and_keeps_the_rest() {
        let body = "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#a\n\
                    \n\
                    ss://not-supported@h:1#x\n\
                    # 注释行\n";
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/sub/alice",
            FakeReply::Text(base64::engine::general_purpose::STANDARD.encode(body)),
        );
        let f = from_subscription(&n, "https://panel.example.com/api/sub/alice").unwrap();
        assert_eq!(f.nodes.len(), 1);
        assert_eq!(f.skipped.len(), 2, "空行忽略不计，ss:// 与注释行计为跳过");
        assert!(f.skipped[0].len() <= 24);
    }

    #[test]
    fn from_subscription_errors_when_nothing_parses() {
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/sub/alice",
            FakeReply::Text(base64::engine::general_purpose::STANDARD.encode("ss://x@h:1#a")),
        );
        let e = from_subscription(&n, "https://panel.example.com/api/sub/alice").unwrap_err();
        assert!(e.to_string().contains("没有可用节点"), "{e}");
    }

    #[test]
    fn from_uris_accepts_paste_and_ignores_blank_lines() {
        let lines = vec![
            "  hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#a  "
                .to_string(),
            String::new(),
        ];
        let f = from_uris(&lines).unwrap();
        assert_eq!(f.nodes.len(), 1);
        assert_eq!(f.user, "", "粘贴来源没有用户名，profile 名回落到 kind slug");
        assert_eq!(f.split, default_split());
    }

    #[test]
    fn from_uris_rejects_http_urls_with_a_pointer_to_the_right_flag() {
        // v3 的 tui_import_node 在这里指向一个已经死掉的订阅导入分支（审计 client-C7）
        let e = from_uris(&["https://panel.example.com/api/sub/alice".to_string()]).unwrap_err();
        assert!(
            e.to_string().contains("--sub"),
            "订阅地址要指向 `bui-c import --sub <url>`：{e}"
        );
    }

    #[test]
    fn decode_base64_handles_urlsafe_and_missing_padding() {
        let raw = "hysteria2://pw@h:1#a";
        let urlsafe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        assert_eq!(decode_base64_body(&urlsafe).unwrap(), raw);
        // 已经是明文 URI 列表（有些面板不做 base64）时原样返回
        assert_eq!(decode_base64_body(raw).unwrap(), raw);
    }

    #[test]
    fn username_from_url_takes_last_segment() {
        assert_eq!(
            username_from_url("https://panel.example.com/api/sub/alice"),
            "alice"
        );
        assert_eq!(
            username_from_url("https://panel.example.com/api/sub/alice/"),
            "alice"
        );
        assert_eq!(username_from_url("https://panel.example.com"), "");
    }

    #[test]
    fn origin_keeps_scheme_host_and_port() {
        assert_eq!(
            origin("https://panel.example.com/api/sub/alice").as_deref(),
            Some("https://panel.example.com")
        );
        assert_eq!(
            origin("http://panel.example.com:8443/api/sub/alice?x=1").as_deref(),
            Some("http://panel.example.com:8443")
        );
        assert_eq!(
            origin("https://panel.example.com").as_deref(),
            Some("https://panel.example.com")
        );
        assert_eq!(origin("panel.example.com/api/sub/alice"), None);
        assert_eq!(origin("https://"), None);
    }
}
