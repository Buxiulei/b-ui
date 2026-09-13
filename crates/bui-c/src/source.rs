//! 节点来源三种：面板 `/api/nodes/<user>`（首选，带服务端真实分流）、
//! 订阅 `/api/sub/<user>`（base64 URI 列表）、用户粘贴的 URI 行。
//!
//! 订阅与粘贴都拿不到住宅分流信息，统一回落 [`crate::profiles::default_split`]。

use crate::net::Net;
use crate::profiles::default_split;
use crate::{Error, Result};
use bui_schema::nodes::Node;
use bui_schema::render::SplitRules;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 路径段里要编码的字符：RFC 3986 unreserved（字母数字与 `-._~`）以外一律编码。
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

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

/// 跳过项里没有 `://` 时的占位：这种行拿不到 scheme，为免带出凭据一个字符都不留。
pub const NO_SCHEME: &str = "(无 scheme)";

/// 一次取节点的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub user: String,
    pub split: SplitRules,
    pub nodes: Vec<Node>,
    /// 无法解析的行，已脱敏（只留到 `://` 为止的 scheme，见 [`scheme_only`]）。
    pub skipped: Vec<String>,
}

/// 脱敏：只留到 `://` 为止。`hysteria2://` 的 userinfo 段就是密码，
/// 留前 24 字符会把 `hysteria2://alice:hy2-pw` 原样打进日志。
pub fn scheme_only(line: &str) -> String {
    match line.split_once("://") {
        Some((scheme, _)) => format!("{scheme}://"),
        None => NO_SCHEME.to_string(),
    }
}

/// 用户名按一个路径段百分号编码：`#` `?` `/` 空格会把 URL 拆坏；中文编码后面板
/// （axum 解码路径）照样认。
pub fn nodes_url(base_url: &str, user: &str) -> String {
    format!(
        "{}/api/nodes/{}",
        base_url.trim_end_matches('/'),
        utf8_percent_encode(user, PATH_SEGMENT)
    )
}

/// 百分号解码一个路径段；解出来不是 UTF-8 就有损替换（用户名只用于显示与拼 URL）。
fn decode_segment(seg: &str) -> String {
    percent_decode_str(seg).decode_utf8_lossy().into_owned()
}

/// 取 URL 最后一个非空路径段（百分号解码后）；订阅 URL 的末段就是用户名。
///
/// 要解码：面板给的订阅地址里中文用户名是编码过的，不解码的话 `%E5%BC%A0` 会被
/// 当成用户名落进 `Panel::username` 与节点名。
pub fn username_from_url(url: &str) -> String {
    let body = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path = body.split(['?', '#']).next().unwrap_or(body);
    match path.split_once('/') {
        Some((_host, rest)) => decode_segment(
            rest.rsplit('/')
                .find(|s: &&str| !s.is_empty())
                .unwrap_or(""),
        ),
        None => String::new(),
    }
}

/// 以 `http://` / `https://` 开头（忽略大小写与首尾空白）。
pub fn is_http_url(line: &str) -> bool {
    let head: String = line.trim_start().chars().take(8).collect();
    let head = head.to_ascii_lowercase();
    head.starts_with("http://") || head.starts_with("https://")
}

/// 面板发给用户的四种按用户地址。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPath {
    /// `/api/sub/<user>`：base64 URI 列表，v3 面板也有
    Sub,
    /// `/api/subscription/<user>`：sing-box 配置
    Subscription,
    /// `/api/clash/<user>`：mihomo YAML
    Clash,
    /// `/api/nodes/<user>`：节点 + 分流规则（v4）
    Nodes,
}

/// 从一条面板地址里认出来的面板与用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelLink {
    /// `scheme://host[:port]`
    pub base_url: String,
    /// 百分号解码后的用户名
    pub user: String,
    pub path: PanelPath,
}

/// `http(s)://host[:port]/api/(sub|subscription|clash|nodes)/<用户名>`（可带尾斜杠与 query）
/// → 面板地址 + 解码后的用户名；别的形状一律 `None`（当普通订阅地址处理）。
pub fn panel_link(url: &str) -> Option<PanelLink> {
    let url = url.trim();
    if !is_http_url(url) {
        return None;
    }
    let base_url = origin(url)?;
    let (_, rest) = url.split_once("://")?;
    let path = match rest.find(['/', '?', '#']) {
        Some(i) => rest[i..].split(['?', '#']).next().unwrap_or(""),
        None => "",
    };
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let (kind, user) = match segs.as_slice() {
        ["", "api", kind, user] if !user.is_empty() => (*kind, decode_segment(user)),
        _ => return None,
    };
    let path = match kind {
        "sub" => PanelPath::Sub,
        "subscription" => PanelPath::Subscription,
        "clash" => PanelPath::Clash,
        "nodes" => PanelPath::Nodes,
        _ => return None,
    };
    if user.trim().is_empty() {
        return None;
    }
    Some(PanelLink {
        base_url,
        user,
        path,
    })
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
    let mut f = collect(&lines);
    if f.nodes.is_empty() {
        return Err(Error::msg(format!(
            "没有可用节点（{} 行无法解析，只支持 hysteria2:// 与 vless://）",
            f.skipped.len()
        )));
    }
    f.user = username_from_url(url);
    Ok(f)
}

/// 粘贴来源：用户手抄的节点链接，没有用户名。
///
/// http(s) 行不再整批拒绝：与不支持的 scheme 一样记进 `skipped`，同一次粘贴里能用的节点
/// 照样导入。一个都解析不出来时，报错里列出全部（去重的）脱敏 scheme。
pub fn from_uris(lines: &[String]) -> Result<Fetched> {
    let f = collect(lines);
    if !f.nodes.is_empty() {
        return Ok(f);
    }
    if f.skipped.is_empty() {
        return Err(Error::msg("没有输入任何链接"));
    }
    let mut schemes: Vec<&str> = Vec::new();
    for s in &f.skipped {
        if !schemes.contains(&s.as_str()) {
            schemes.push(s);
        }
    }
    let mut msg = format!(
        "没有可用节点：{} 行无法解析（{}），只支持 hysteria2:// 与 vless://",
        f.skipped.len(),
        schemes.join("、")
    );
    if lines.iter().any(|l| is_http_url(l)) {
        // 命令行 `bui-c import -` 与菜单 [3] 都会走到这里：说法得两边都成立
        msg.push_str("。订阅地址请用 `bui-c import --sub <url>`，或在菜单 [3] 里单独粘贴一行");
    }
    Err(Error::msg(msg))
}

fn collect(lines: &[String]) -> Fetched {
    let mut nodes = Vec::new();
    let mut skipped = Vec::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        match bui_schema::parse::node_uri(t) {
            Ok(n) => nodes.push(n),
            Err(_) => skipped.push(scheme_only(t)),
        }
    }
    Fetched {
        user: String::new(),
        split: default_split(),
        nodes,
        skipped,
    }
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
        assert_eq!(f.skipped, vec!["ss://", NO_SCHEME]);
    }

    #[test]
    fn skipped_lines_keep_only_the_scheme_never_the_userinfo() {
        // hysteria2:// 的 userinfo 段就是密码：前 24 字符已经够带出 `hysteria2://alice:hy2-pw`
        let lines = vec![
            "hysteria2://alice:hy2-pw@panel.example.com?no-port=1".to_string(),
            "ss://not-supported@h:1#x".to_string(),
            "# 注释行".to_string(),
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#ok"
                .to_string(),
        ];
        let f = from_uris(&lines).unwrap();
        assert_eq!(f.nodes.len(), 1, "最后一行能解析");
        assert_eq!(
            f.skipped,
            vec!["hysteria2://", "ss://", NO_SCHEME],
            "只留 scheme，没有 scheme 的行连内容都不留"
        );
        for s in &f.skipped {
            assert!(!s.contains("hy2-pw"), "跳过项不能带凭据：{s}");
            assert!(!s.contains('@'), "跳过项不能带 userinfo：{s}");
        }
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
        let msg = e.to_string();
        assert!(
            msg.contains("--sub"),
            "订阅地址要指向 `bui-c import --sub <url>`：{msg}"
        );
        assert!(
            msg.contains("或在菜单 [3] 里单独粘贴一行"),
            "这条错误在命令行 `bui-c import -` 与菜单里都会出现，说法要两边都成立：{msg}"
        );
        assert!(msg.contains("https://"), "列出脱敏后的 scheme：{msg}");
        assert!(
            !msg.contains("alice"),
            "订阅路径里的用户名不能带出来：{msg}"
        );
    }

    #[test]
    fn from_uris_keeps_parsable_lines_when_an_http_url_is_mixed_in() {
        // 以前遇到 http(s) 行整批报错，同一次粘贴里能用的节点也一个不导
        let lines = vec![
            "ss://not-supported@h:1#x".to_string(),
            "https://panel.example.com/api/sub/alice".to_string(),
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#ok"
                .to_string(),
        ];
        let f = from_uris(&lines).unwrap();
        assert_eq!(f.nodes.len(), 1);
        assert_eq!(f.skipped, vec!["ss://", "https://"]);
    }

    #[test]
    fn from_uris_lists_every_redacted_scheme_when_nothing_parses() {
        let lines = vec![
            "ss://secret-a@h:1#x".to_string(),
            "trojan://secret-b@h:2#y".to_string(),
            "# 注释行".to_string(),
            "ss://secret-c@h:3#z".to_string(),
        ];
        let msg = from_uris(&lines).unwrap_err().to_string();
        for want in ["ss://", "trojan://", NO_SCHEME, "hysteria2://", "vless://"] {
            assert!(msg.contains(want), "缺 {want}：{msg}");
        }
        assert!(!msg.contains("secret"), "只列 scheme，不带内容：{msg}");
        assert!(!msg.contains("--sub"), "没有 http(s) 行就别提订阅：{msg}");
    }

    #[test]
    fn from_uris_with_only_blank_lines_says_nothing_was_entered() {
        let msg = from_uris(&[String::new(), "   ".to_string()])
            .unwrap_err()
            .to_string();
        assert!(msg.contains("没有输入任何链接"), "{msg}");
        assert!(!msg.contains("0 行"), "别报「0 行无法解析」：{msg}");
    }

    #[test]
    fn nodes_url_percent_encodes_the_username_as_one_path_segment() {
        let base = "https://panel.example.com";
        assert_eq!(
            nodes_url(base, "a b#c?d/e%f"),
            "https://panel.example.com/api/nodes/a%20b%23c%3Fd%2Fe%25f",
            "# ? / 空格 % 会把 URL 拆坏"
        );
        assert_eq!(
            nodes_url(base, "张三"),
            "https://panel.example.com/api/nodes/%E5%BC%A0%E4%B8%89",
            "中文按 UTF-8 编码，面板（axum）解码路径后照样认"
        );
        assert_eq!(
            nodes_url(base, "Al-ice_1.~"),
            "https://panel.example.com/api/nodes/Al-ice_1.~",
            "unreserved 字符原样保留"
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
    fn username_from_url_percent_decodes_the_segment() {
        assert_eq!(
            username_from_url("https://panel.example.com/api/sub/%E5%BC%A0%E4%B8%89"),
            "张三"
        );
        assert_eq!(
            username_from_url("https://panel.example.com/api/sub/a%20b?x=1"),
            "a b"
        );
    }

    #[test]
    fn panel_link_recognises_the_four_per_user_panel_paths() {
        for (url, path) in [
            ("https://panel.example.com/api/sub/alice", PanelPath::Sub),
            (
                "https://panel.example.com/api/subscription/alice/",
                PanelPath::Subscription,
            ),
            (
                "https://panel.example.com/api/clash/alice?x=1",
                PanelPath::Clash,
            ),
            (
                "https://panel.example.com/api/nodes/alice/?a=b#c",
                PanelPath::Nodes,
            ),
        ] {
            assert_eq!(
                panel_link(url),
                Some(PanelLink {
                    base_url: "https://panel.example.com".into(),
                    user: "alice".into(),
                    path,
                }),
                "{url}"
            );
        }
        let l = panel_link("  http://1.2.3.4:8443/api/sub/%E5%BC%A0%E4%B8%89  ").unwrap();
        assert_eq!(l.base_url, "http://1.2.3.4:8443", "端口保留");
        assert_eq!(l.user, "张三", "用户名百分号解码");
    }

    #[test]
    fn panel_link_rejects_other_shapes() {
        for url in [
            "https://panel.example.com",
            "https://panel.example.com/",
            "https://panel.example.com/api/sub/",
            "https://panel.example.com/api/sub",
            "https://panel.example.com/api/other/alice",
            "https://panel.example.com/prefix/api/sub/alice",
            "https://panel.example.com/api/sub/alice/extra",
            "https://sub.example.com/link/abc?token=1",
            "ftp://panel.example.com/api/sub/alice",
            "hysteria2://alice:pw@panel.example.com:10000/api/sub/alice",
        ] {
            assert_eq!(panel_link(url), None, "{url}");
        }
    }

    #[test]
    fn is_http_url_ignores_case_and_leading_space() {
        assert!(is_http_url("https://x"));
        assert!(is_http_url("  HTTP://x"));
        assert!(!is_http_url("hysteria2://x"));
        assert!(!is_http_url("http:/x"));
        assert!(!is_http_url("你好"));
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
