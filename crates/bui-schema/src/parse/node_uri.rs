//! 节点 URI 解析：`hysteria2://` / `vless://`（REALITY）→ `Node`。
//!
//! 格式对照 v3 `web/server.js:1826-1884`（`buildVlessUrl` / `buildHy2Url`）。
use super::ParseError;
use crate::nodes::{Node, NodeKind, Transport};
use percent_encoding::percent_decode_str;
use url::Url;
use uuid::Uuid;

/// 解析一条节点 URI；`label` 取 fragment 解码后的原文（含 `{username}-` 前缀）。
///
/// 直连 / 住宅按 v3 标签语义判定：label 含「住宅」即住宅通路，其余算直连。
pub fn node_uri(raw: &str) -> Result<Node, ParseError> {
    let u =
        Url::parse(raw.trim()).map_err(|e| ParseError::Other(format!("节点 URI 解析失败：{e}")))?;
    let host = u
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ParseError::Other("节点 URI 缺少主机".into()))?
        .to_string();
    let port = u
        .port()
        .ok_or_else(|| ParseError::Other("节点 URI 缺少端口".into()))?;
    let label = decode(u.fragment().unwrap_or(""))?;
    let resi = label.contains("住宅");
    // query_pairs 按 form-urlencoded 解码；v3 用 encodeURIComponent，字面 + 一律写成 %2B，不会误解
    let query: Vec<(String, String)> = u
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };

    match u.scheme() {
        "hysteria2" | "hy2" => {
            // v3 早期（≤3.4）把整段 `user:pass` 当一个 token 做 encodeURIComponent 再放进
            // userinfo（`alice%3Apw@host`）：url crate 看不到未编码的 `:`，password() 是 None，
            // 解码后再按第一个 `:` 拆一次——与 hysteria 服务端 userpass 的拆法一致。
            let (username, password) = match u.password() {
                Some(p) => (decode(u.username())?, decode(p)?),
                None => {
                    let whole = decode(u.username())?;
                    match whole.split_once(':') {
                        Some((a, b)) => (a.to_string(), b.to_string()),
                        None => (whole, String::new()),
                    }
                }
            };
            if username.is_empty() || password.is_empty() {
                return Err(ParseError::Other("hysteria2 URI 缺少用户名或密码".into()));
            }
            Ok(Node {
                kind: if resi {
                    NodeKind::Hy2Residential
                } else {
                    NodeKind::Hy2Direct
                },
                label,
                hop: get("mport").map(hop_range).transpose()?,
                port,
                transport: Transport::Hysteria2 {
                    username,
                    password,
                    sni: get("sni").unwrap_or(&host).to_string(),
                    obfs_password: get("obfs-password").map(str::to_string),
                },
                host,
            })
        }
        "vless" => {
            let uuid = Uuid::parse_str(&decode(u.username())?)
                .map_err(|e| ParseError::Other(format!("vless UUID 非法：{e}")))?;
            Ok(Node {
                kind: if resi {
                    NodeKind::RealityResidential
                } else {
                    NodeKind::RealityDirect
                },
                label,
                port,
                hop: None,
                transport: Transport::Reality {
                    uuid,
                    public_key: get("pbk")
                        .ok_or_else(|| ParseError::Other("vless URI 缺少 pbk".into()))?
                        .to_string(),
                    short_id: get("sid").unwrap_or("").to_string(),
                    server_name: get("sni").unwrap_or(&host).to_string(),
                    fingerprint: get("fp").unwrap_or("chrome").to_string(),
                    flow: get("flow").unwrap_or("xtls-rprx-vision").to_string(),
                },
                host,
            })
        }
        other => Err(ParseError::Other(format!(
            "不支持的节点协议 {other}://，只支持 hysteria2:// 与 vless://"
        ))),
    }
}

/// 百分号解码。
fn decode(s: &str) -> Result<String, ParseError> {
    percent_decode_str(s)
        .decode_utf8()
        .map(|c| c.into_owned())
        .map_err(|e| ParseError::Other(format!("百分号编码解码失败：{e}")))
}

/// `mport=20000-30000` → 端口跳跃区间。
fn hop_range(raw: &str) -> Result<(u16, u16), ParseError> {
    let (start, end) = raw
        .split_once('-')
        .ok_or_else(|| ParseError::Other(format!("mport 格式非法：{raw}")))?;
    let one = |s: &str| {
        s.parse::<u16>()
            .map_err(|_| ParseError::Port(raw.to_string()))
    };
    Ok((one(start)?, one(end)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // 以下样例按 v3 /api/sub 的输出手写（Task 6 会补 fixture 版逐行比对）
    const HY2_DIRECT: &str = "hysteria2://alice:pw-alice@example.com:10000?sni=example.com&insecure=0&mport=20000-30000&obfs=salamander&obfs-password=obfs-pw#alice-HY2%E7%9B%B4%E8%BF%9E";
    const HY2_RESI: &str = "hysteria2://alice:pw-alice@example.com:40000?sni=example.com&insecure=0&mport=41000-50000#alice-HY2%E4%BD%8F%E5%AE%85";
    const REALITY_DIRECT: &str = "vless://11111111-1111-4111-8111-111111111111@example.com:10001?security=reality&encryption=none&pbk=PUB&headerType=&fp=chrome&spx=%2F&type=tcp&flow=xtls-rprx-vision&sni=www.bing.com&sid=0123456789abcdef#alice-Reality%E7%9B%B4%E8%BF%9E";
    const REALITY_RESI: &str = "vless://11111111-1111-4111-8111-111111111111@example.com:10002?security=reality&encryption=none&pbk=PUB&headerType=&fp=chrome&spx=%2F&type=tcp&flow=xtls-rprx-vision&sni=www.bing.com&sid=0123456789abcdef#alice-Reality%E4%BD%8F%E5%AE%85";

    fn hy2(port: u16, hop: (u16, u16), label: &str, obfs: Option<&str>, kind: NodeKind) -> Node {
        Node {
            kind,
            label: label.into(),
            host: "example.com".into(),
            port,
            hop: Some(hop),
            transport: Transport::Hysteria2 {
                username: "alice".into(),
                password: "pw-alice".into(),
                sni: "example.com".into(),
                obfs_password: obfs.map(str::to_string),
            },
        }
    }

    fn reality(port: u16, label: &str, kind: NodeKind) -> Node {
        Node {
            kind,
            label: label.into(),
            host: "example.com".into(),
            port,
            hop: None,
            transport: Transport::Reality {
                uuid: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
                public_key: "PUB".into(),
                short_id: "0123456789abcdef".into(),
                server_name: "www.bing.com".into(),
                fingerprint: "chrome".into(),
                flow: "xtls-rprx-vision".into(),
            },
        }
    }

    #[test]
    fn hysteria2_direct_round_trip() {
        assert_eq!(
            node_uri(HY2_DIRECT).unwrap(),
            hy2(
                10000,
                (20000, 30000),
                "alice-HY2直连",
                Some("obfs-pw"),
                NodeKind::Hy2Direct
            )
        );
    }

    #[test]
    fn hysteria2_residential_has_no_obfs() {
        assert_eq!(
            node_uri(HY2_RESI).unwrap(),
            hy2(
                40000,
                (41000, 50000),
                "alice-HY2住宅",
                None,
                NodeKind::Hy2Residential
            )
        );
    }

    #[test]
    fn reality_direct_round_trip() {
        assert_eq!(
            node_uri(REALITY_DIRECT).unwrap(),
            reality(10001, "alice-Reality直连", NodeKind::RealityDirect)
        );
    }

    #[test]
    fn reality_residential_round_trip() {
        assert_eq!(
            node_uri(REALITY_RESI).unwrap(),
            reality(10002, "alice-Reality住宅", NodeKind::RealityResidential)
        );
    }

    #[test]
    fn no_mport_means_no_hop() {
        let n =
            node_uri("hysteria2://bob:p%2B1@example.com:10000?sni=example.com&insecure=0").unwrap();
        assert_eq!(n.hop, None);
        assert_eq!(n.label, "");
        match n.transport {
            Transport::Hysteria2 {
                username,
                password,
                obfs_password,
                ..
            } => {
                assert_eq!(username, "bob");
                assert_eq!(password, "p+1");
                assert_eq!(obfs_password, None);
            }
            _ => panic!("应为 hysteria2"),
        }
    }

    #[test]
    fn hysteria2_accepts_v3_userinfo_with_a_percent_encoded_colon() {
        // v3 早期（≤3.4）把整段 `user:pass` 当一个 token 做 encodeURIComponent 再放进 userinfo：
        // `hysteria2://alice%3Apw-alice@…`。url crate 看不到未编码的 `:`，password() 是 None。
        // 2026-09-12 baiyi 真机 import-v3 三个 hysteria2-* 目录全被「跳过」就是这个原因。
        let raw = "hysteria2://alice%3Apw-alice@example.com:10000?sni=example.com&insecure=0&allowInsecure=0&mport=20000-30000#%E7%A4%BA%E4%BE%8B%E4%B8%93%E7%94%A8%E5%90%8D";
        let n = node_uri(raw).unwrap();
        assert_eq!(n.label, "示例专用名");
        assert_eq!(n.kind, NodeKind::Hy2Direct);
        assert_eq!(n.hop, Some((20000, 30000)));
        match n.transport {
            Transport::Hysteria2 {
                username, password, ..
            } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "pw-alice");
            }
            _ => panic!("应为 hysteria2"),
        }
        // 密码本身含冒号时只拆第一个：`u:p:x` → ("u", "p:x")，与 hysteria 服务端 userpass 的拆法一致
        let n2 = node_uri("hysteria2://alice%3Apw%3Ax@example.com:10000").unwrap();
        match n2.transport {
            Transport::Hysteria2 {
                username, password, ..
            } => assert_eq!((username.as_str(), password.as_str()), ("alice", "pw:x")),
            _ => panic!("应为 hysteria2"),
        }
        // 没有冒号的单段 userinfo 仍然拒绝：拿不出用户名，服务端 userpass 鉴权必然失败
        assert!(node_uri("hysteria2://onlypassword@example.com:10000").is_err());
    }

    #[test]
    fn rejects_other_schemes_and_broken_uris() {
        assert!(matches!(
            node_uri("trojan://p@example.com:443#x"),
            Err(ParseError::Other(_))
        ));
        // vless 的用户名必须是 UUID
        assert!(
            node_uri(&REALITY_DIRECT.replace("11111111-1111-4111-8111-111111111111", "abc"))
                .is_err()
        );
        // 缺端口
        assert!(node_uri("hysteria2://alice:pw@example.com?sni=example.com").is_err());
        // 缺密码
        assert!(node_uri("hysteria2://alice@example.com:10000?sni=example.com").is_err());
        assert!(node_uri("not-a-uri").is_err());
    }
}
