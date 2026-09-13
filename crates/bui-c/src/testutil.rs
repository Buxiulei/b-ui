//! 测试样例：合成域名 panel.example.com、合成密码，与生产无关。
//!
//! 不加 `#[cfg(test)]`——同一套样例给 `src/**` 的单元测试与未来的集成测试共用。

use crate::profiles::{Mode, Profile, Profiles, Source};
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

fn one(mode: Mode) -> Profiles {
    let mut p = Profiles::new_default();
    p.mode = mode;
    p.profiles.push(Profile {
        name: "alice-hy2-direct".into(),
        node: hy2_direct_node(),
        split: split_global(),
        source: Source::ApiNodes,
        imported_at: "2026-09-11T00:00:00Z".into(),
    });
    p.active = Some("alice-hy2-direct".into());
    p
}

/// 一个 hy2 直连节点，`mode = Socks`，active 已设。
pub fn profiles_socks() -> Profiles {
    one(Mode::Socks)
}

/// 同 [`profiles_socks`]，但 `mode = Tun`。
pub fn profiles_tun() -> Profiles {
    one(Mode::Tun)
}

/// 叫 `name` 的节点：全局分流，来自面板 `/api/nodes`。
pub fn named(name: &str, node: Node) -> Profile {
    Profile {
        name: name.into(),
        node,
        split: split_global(),
        source: Source::ApiNodes,
        imported_at: "2026-09-11T00:00:00Z".into(),
    }
}

/// 真机（baiyi）形态的 9 个节点：名字 3–38 列、label 最长 26 列、host:port 最长 29 列。
/// 端点与凭据是合成的，只有各字段的长度照抄真机。活动节点是第 2 个（短名）。
/// 菜单的宽度守门表与删除流程的测试都用它。
pub fn baiyi_like() -> Profiles {
    let node = |kind: NodeKind, label: &str, host: &str, port: u16| Node {
        kind,
        label: label.into(),
        host: host.into(),
        port,
        ..match kind {
            NodeKind::RealityDirect | NodeKind::RealityResidential => reality_direct_node(),
            NodeKind::Hy2Direct | NodeKind::Hy2Residential => hy2_direct_node(),
        }
    };
    let bwg = "tizi.example.test";
    let cl = "rick-node.example-a.net";
    let mut p = Profiles::new_default();
    for (name, n) in [
        (
            "HY2",
            node(NodeKind::Hy2Residential, "示例专用名-HY2住宅", bwg, 40000),
        ),
        (
            "hysteria2-1778329470",
            node(NodeKind::Hy2Direct, "示例专用名", bwg, 10000),
        ),
        (
            "reality-Reality",
            node(
                NodeKind::RealityDirect,
                "示例名-reality-Reality直连",
                bwg,
                10001,
            ),
        ),
        (
            "rick-node.example-a.net-reality-direct",
            node(NodeKind::RealityDirect, "Reality直连", cl, 10001),
        ),
        (
            "rick-node.example-a.net-reality-resi",
            node(NodeKind::RealityResidential, "Reality住宅", cl, 10002),
        ),
        (
            "rick-node.example-a.net-hy2-direct",
            node(NodeKind::Hy2Direct, "HY2直连", cl, 10000),
        ),
        (
            "rick-node.example-a.net-hy2-resi",
            node(NodeKind::Hy2Residential, "HY2住宅", cl, 40001),
        ),
        (
            "tizi.example.test-reality-resi",
            node(NodeKind::RealityResidential, "Reality住宅", bwg, 10002),
        ),
        (
            "tizi.example.test-hy2-resi",
            node(NodeKind::Hy2Residential, "HY2住宅", bwg, 40002),
        ),
    ] {
        p.profiles.push(named(name, n));
    }
    p.active = Some("hysteria2-1778329470".into());
    p
}
