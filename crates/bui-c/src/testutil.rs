//! 测试样例：合成域名 panel.example.com、合成密码，与生产无关。
//!
//! 不加 `#[cfg(test)]`——同一套样例给 `src/**` 的单元测试与未来的集成测试共用。

use bui_schema::nodes::{Node, NodeKind, Transport};
use bui_schema::render::SplitRules;

pub fn hy2_direct_node() -> Node {
    Node {
        kind: NodeKind::Hy2Direct,
        label: "HY2直连".into(),
        host: "panel.example.com".into(),
        port: 10000,
        hop: Some((20000, 30000)),
        transport: Transport::Hysteria2 {
            username: "alice".into(),
            password: "hy2-pw".into(),
            sni: "panel.example.com".into(),
            obfs_password: None,
        },
    }
}

pub fn hy2_resi_node() -> Node {
    Node {
        kind: NodeKind::Hy2Residential,
        label: "HY2住宅".into(),
        port: 40000,
        hop: Some((41000, 50000)),
        ..hy2_direct_node()
    }
}

pub fn reality_direct_node() -> Node {
    Node {
        kind: NodeKind::RealityDirect,
        label: "Reality直连".into(),
        host: "panel.example.com".into(),
        port: 10001,
        hop: None,
        transport: Transport::Reality {
            uuid: uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            public_key: "PUB".into(),
            short_id: "0123456789abcdef".into(),
            server_name: "www.bing.com".into(),
            fingerprint: "chrome".into(),
            flow: "xtls-rprx-vision".into(),
        },
    }
}

pub fn split_global() -> SplitRules {
    SplitRules {
        enabled: true,
        global: true,
        keywords: bui_schema::keywords::DEFAULT_KEYWORDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

pub fn split_keywords() -> SplitRules {
    SplitRules {
        enabled: true,
        global: false,
        keywords: vec!["openai.com".into(), "anthropic.com".into()],
    }
}
