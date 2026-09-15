//! 权益 → 节点集合：把一个用户的 [`Entitlements`](crate::model::Entitlements) 展开成他订阅里该出现的节点。

use crate::model::{NodeParams, Protocol, Residential, User};
use crate::slots;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 四条通路之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    RealityDirect,
    RealityResidential,
    Hy2Direct,
    Hy2Residential,
}

/// 节点的协议参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Transport {
    Hysteria2 {
        username: String,
        password: String,
        sni: String,
        obfs_password: Option<String>,
    },
    Reality {
        uuid: Uuid,
        public_key: String,
        short_id: String,
        server_name: String,
        /// 固定 `chrome`
        fingerprint: String,
        /// 固定 `xtls-rprx-vision`
        flow: String,
    },
}

/// 订阅里的一个节点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    pub label: String,
    pub host: String,
    pub port: u16,
    /// 端口跳跃区间（闭区间），`None` = 未启用。
    pub hop: Option<(u16, u16)>,
    pub transport: Transport,
}

impl Node {
    /// 订阅里显示的节点名，与 v3 一致：`{username}-{label}`。
    pub fn name(&self, username: &str) -> String {
        format!("{}-{}", username, self.label)
    }
}

/// 按权益生成节点集合，顺序固定为 v3 `/api/sub` 的顺序：
/// Reality直连、Reality住宅、HY2直连、HY2住宅。
///
/// 住宅节点只在用户有住宅权益、且权益指向的分组真实存在时才给出；
/// obfs 加在直连与住宅两种 HY2 节点上（混淆覆盖全部 HY2 实例，2026-09-15 裁决）。
pub fn nodes_for(user: &User, node: &NodeParams, resi: &Residential) -> Vec<Node> {
    let e = &user.entitlements;
    let has = |p: Protocol| e.protocols.contains(&p);
    let resi_ok = e
        .residential
        .as_ref()
        .map(|r| resi.groups.contains_key(&r.group_id))
        .unwrap_or(false);
    let sni = node.domain.clone();
    let obfs = if node.obfs.enabled && !node.obfs.password.is_empty() {
        Some(node.obfs.password.clone())
    } else {
        None
    };
    let reality = |port: u16, kind: NodeKind, label: &str| Node {
        kind,
        label: label.into(),
        host: node.domain.clone(),
        port,
        hop: None,
        transport: Transport::Reality {
            uuid: user.credentials.vless_uuid,
            public_key: node.reality.public_key.clone(),
            short_id: node.reality.short_id().to_string(),
            server_name: node.reality.sni().to_string(),
            fingerprint: "chrome".into(),
            flow: "xtls-rprx-vision".into(),
        },
    };
    let hy2 = |port: u16,
               hop: Option<(u16, u16)>,
               kind: NodeKind,
               label: &str,
               obfs_password: Option<String>| Node {
        kind,
        label: label.into(),
        host: node.domain.clone(),
        port,
        hop,
        transport: Transport::Hysteria2 {
            username: user.username.clone(),
            password: user.credentials.hy2_password.clone(),
            sni: sni.clone(),
            obfs_password,
        },
    };
    let mut out = Vec::with_capacity(4);
    if has(Protocol::Reality) {
        if e.direct {
            out.push(reality(
                node.ports.reality_direct,
                NodeKind::RealityDirect,
                "Reality直连",
            ));
        }
        if resi_ok {
            out.push(reality(
                node.ports.reality_resi,
                NodeKind::RealityResidential,
                "Reality住宅",
            ));
        }
    }
    if has(Protocol::Hysteria2) {
        if e.direct {
            out.push(hy2(
                node.ports.hy2,
                node.ports.hy2_hop,
                NodeKind::Hy2Direct,
                "HY2直连",
                obfs.clone(),
            ));
        }
        if resi_ok {
            // spec §5.6：住宅 HY2 节点用**该用户槽位**的端口与跳跃区间。
            // 单槽（含空池、旧 state）时它就是 40000 + 41000-50000，与 v3 逐字相同。
            let res = slots::resources_of(&node.ports, resi, slots::index_of_user(user, resi));
            out.push(hy2(
                res.hy2_port,
                Some(res.hop),
                NodeKind::Hy2Residential,
                "HY2住宅",
                obfs,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use uuid::Uuid;

    fn node() -> NodeParams {
        serde_json::from_str(
            r#"{"id":"8d5a1a1e-3b2c-4d1e-9f00-000000000001","name":"n","domain":"example.com","public_ip":"203.0.113.10",
        "ports":{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080},
        "reality":{"private_key":"a","public_key":"PUB","short_ids":["0123456789abcdef"],"dest":"www.bing.com:443","server_names":["www.bing.com"]},
        "obfs":{"enabled":true,"password":"obfs-pw"}}"#,
        )
        .unwrap()
    }

    fn user(protocols: Vec<Protocol>, resi: bool) -> User {
        let mut u: User = serde_json::from_str(
            r#"{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"alice","created_at":"2026-09-11T00:00:00Z",
            "credentials":{"hy2_password":"pw","vless_uuid":"11111111-1111-4111-8111-111111111111"},
            "entitlements":{"protocols":[],"direct":true,"residential":{"group_id":"default"}}}"#,
        )
        .unwrap();
        u.entitlements.protocols = protocols;
        if !resi {
            u.entitlements.residential = None;
        }
        u
    }

    #[test]
    fn fusion_gives_four_in_v3_order() {
        let ns = nodes_for(
            &user(vec![Protocol::Hysteria2, Protocol::Reality], true),
            &node(),
            &Residential::default(),
        );
        let kinds: Vec<_> = ns.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            vec![
                NodeKind::RealityDirect,
                NodeKind::RealityResidential,
                NodeKind::Hy2Direct,
                NodeKind::Hy2Residential
            ]
        );
        assert_eq!(
            ns.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            vec!["Reality直连", "Reality住宅", "HY2直连", "HY2住宅"]
        );
        assert_eq!(ns[2].port, 10000);
        assert_eq!(ns[2].hop, Some((20000, 30000)));
        assert_eq!(ns[3].port, 40000);
        assert_eq!(ns[3].hop, Some((41000, 50000)));
        match &ns[2].transport {
            Transport::Hysteria2 {
                obfs_password, sni, ..
            } => {
                assert_eq!(obfs_password.as_deref(), Some("obfs-pw"));
                assert_eq!(sni, "example.com");
            }
            _ => panic!(),
        }
        // 混淆覆盖全部 HY2 实例：住宅 HY2 与直连带同一个 obfs 密码
        match &ns[3].transport {
            Transport::Hysteria2 { obfs_password, .. } => {
                assert_eq!(obfs_password.as_deref(), Some("obfs-pw"))
            }
            _ => panic!(),
        }
        match &ns[0].transport {
            Transport::Reality {
                server_name,
                short_id,
                fingerprint,
                flow,
                ..
            } => {
                assert_eq!(server_name, "www.bing.com");
                assert_eq!(short_id, "0123456789abcdef");
                assert_eq!(fingerprint, "chrome");
                assert_eq!(flow, "xtls-rprx-vision");
            }
            _ => panic!(),
        }
        assert_eq!(ns[0].name("alice"), "alice-Reality直连");
    }

    #[test]
    fn hysteria2_only_with_residential_gives_two() {
        let ns = nodes_for(
            &user(vec![Protocol::Hysteria2], true),
            &node(),
            &Residential::default(),
        );
        assert_eq!(
            ns.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![NodeKind::Hy2Direct, NodeKind::Hy2Residential]
        );
    }

    #[test]
    fn no_residential_entitlement_gives_direct_only() {
        let ns = nodes_for(
            &user(vec![Protocol::Hysteria2, Protocol::Reality], false),
            &node(),
            &Residential::default(),
        );
        assert_eq!(
            ns.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![NodeKind::RealityDirect, NodeKind::Hy2Direct]
        );
    }

    #[test]
    fn direct_false_gives_residential_only() {
        let mut u = user(vec![Protocol::Reality], true);
        u.entitlements.direct = false;
        let ns = nodes_for(&u, &node(), &Residential::default());
        assert_eq!(
            ns.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![NodeKind::RealityResidential]
        );
    }

    /// 造一份 n 槽的住宅段（上游 uuid = index+1）。
    fn resi_with_slots(n: u16) -> Residential {
        let mut r = Residential::default();
        let g = r.groups.get_mut(crate::model::DEFAULT_GROUP).unwrap();
        g.enabled = true;
        g.upstreams = (0..n)
            .map(|i| crate::model::Upstream {
                id: Uuid::from_u128(u128::from(i) + 1),
                name: format!("url-{}", i + 1),
                kind: crate::model::UpstreamKind::Socks5,
                host: format!("isp{}.example.net", i + 1),
                port: 10007,
                username: "user1".into(),
                password: "pw1".into(),
                priority: 100,
                provider: None,
                region: None,
                ports_allowed: None,
                verified: None,
            })
            .collect();
        r.slots = (0..n)
            .map(|i| crate::model::Slot {
                index: i,
                upstream_id: Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        r
    }

    /// 单槽（含空池）时 HY2 住宅节点必须还是 40000 + 41000-50000 —— golden 的生命线。
    #[test]
    fn a_single_slot_keeps_the_v3_residential_hy2_port() {
        let ns = nodes_for(
            &user(vec![Protocol::Hysteria2], true),
            &node(),
            &Residential::default(),
        );
        let r = ns
            .iter()
            .find(|n| n.kind == NodeKind::Hy2Residential)
            .unwrap();
        assert_eq!(r.port, 40000);
        assert_eq!(r.hop, Some((41000, 50000)));
    }

    #[test]
    fn the_residential_hy2_node_follows_the_users_slot() {
        let resi = resi_with_slots(3);
        let mut u = user(vec![Protocol::Hysteria2, Protocol::Reality], true);
        // 槽 1
        u.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(2));
        let ns = nodes_for(&u, &node(), &resi);
        let r = ns
            .iter()
            .find(|n| n.kind == NodeKind::Hy2Residential)
            .unwrap();
        assert_eq!(r.port, 40001);
        assert_eq!(r.hop, Some((44000, 46999)));
        // 槽 2
        u.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(3));
        let ns = nodes_for(&u, &node(), &resi);
        let r = ns
            .iter()
            .find(|n| n.kind == NodeKind::Hy2Residential)
            .unwrap();
        assert_eq!(r.port, 40002);
        assert_eq!(r.hop, Some((47000, 50000)));
        // 未分配 ⇒ 兜底槽（槽 0）
        u.entitlements.residential.as_mut().unwrap().slot_id = None;
        let ns = nodes_for(&u, &node(), &resi);
        let r = ns
            .iter()
            .find(|n| n.kind == NodeKind::Hy2Residential)
            .unwrap();
        assert_eq!(r.port, 40000);
        assert_eq!(r.hop, Some((41000, 43999)));
    }

    /// 槽位只动 HY2 住宅那一个节点：直连两条与 Reality 住宅（:10002）都不许变。
    #[test]
    fn slots_do_not_touch_any_other_node() {
        let resi = resi_with_slots(3);
        let mut u = user(vec![Protocol::Hysteria2, Protocol::Reality], true);
        u.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(3));
        let ns = nodes_for(&u, &node(), &resi);
        let by = |k: NodeKind| ns.iter().find(|n| n.kind == k).unwrap().clone();
        assert_eq!(by(NodeKind::RealityDirect).port, 10001);
        assert_eq!(by(NodeKind::RealityResidential).port, 10002);
        assert_eq!(by(NodeKind::RealityResidential).hop, None);
        assert_eq!(by(NodeKind::Hy2Direct).port, 10000);
        assert_eq!(by(NodeKind::Hy2Direct).hop, Some((20000, 30000)));
        // 标签与顺序也不许变（v2rayN 里的节点名）
        assert_eq!(
            ns.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
            vec!["Reality直连", "Reality住宅", "HY2直连", "HY2住宅"]
        );
    }

    #[test]
    fn no_hop_when_disabled() {
        let mut n = node();
        n.ports.hy2_hop = None;
        n.obfs.enabled = false;
        let ns = nodes_for(
            &user(vec![Protocol::Hysteria2], false),
            &n,
            &Residential::default(),
        );
        assert_eq!(ns[0].hop, None);
        match &ns[0].transport {
            Transport::Hysteria2 { obfs_password, .. } => assert_eq!(obfs_password, &None),
            _ => panic!(),
        }
    }
}

#[cfg(test)]
mod serde_tests {
    use super::*;

    fn hy2() -> Node {
        Node {
            kind: NodeKind::Hy2Direct,
            label: "HY2直连".into(),
            host: "panel.example.com".into(),
            port: 10000,
            hop: Some((20000, 30000)),
            transport: Transport::Hysteria2 {
                username: "alice".into(),
                password: "pw".into(),
                sni: "panel.example.com".into(),
                obfs_password: None,
            },
        }
    }

    #[test]
    fn node_wire_format_is_snake_case_and_internally_tagged() {
        let v = serde_json::to_value(hy2()).unwrap();
        assert_eq!(v["kind"], "hy2_direct");
        assert_eq!(v["hop"], serde_json::json!([20000, 30000]));
        assert_eq!(v["transport"]["type"], "hysteria2");
        assert_eq!(v["transport"]["username"], "alice");
        assert!(v["transport"]["obfs_password"].is_null());
    }

    #[test]
    fn node_round_trips() {
        let n = hy2();
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn reality_node_round_trips() {
        let n = Node {
            kind: NodeKind::RealityResidential,
            label: "Reality住宅".into(),
            host: "panel.example.com".into(),
            port: 10002,
            hop: None,
            transport: Transport::Reality {
                uuid: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
                public_key: "PUB".into(),
                short_id: "0123456789abcdef".into(),
                server_name: "www.bing.com".into(),
                fingerprint: "chrome".into(),
                flow: "xtls-rprx-vision".into(),
            },
        };
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["kind"], "reality_residential");
        assert_eq!(v["transport"]["type"], "reality");
        assert_eq!(
            v["transport"]["uuid"],
            "11111111-1111-4111-8111-111111111111"
        );
        assert!(v["hop"].is_null());
        let back: Node = serde_json::from_value(v).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn split_rules_round_trips() {
        let s = crate::render::SplitRules {
            enabled: true,
            global: false,
            keywords: vec!["openai".into()],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"enabled": true, "global": false, "keywords": ["openai"]})
        );
        let back: crate::render::SplitRules = serde_json::from_value(v).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn node_uri_label_is_the_whole_decoded_fragment() {
        // 守门测试：node_uri 的 label 必须是解码后的完整 fragment（含 "alice-" 前缀）。
        // P4 的 import-v3 用 `label.split_once('-')` 推导用户名；一旦有人改成「先剥前缀」，
        // 用户名就是空串，profile 名会从 alice-hy2-direct 退化成 hy2-direct，P4 T9 三个断言全红。
        // （`kind` 的判定规则由 node_uri.rs 自己的测试覆盖，这里不重复断言。）
        let n = crate::parse::node_uri(
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        )
        .unwrap();
        assert_eq!(n.label, "alice-HY2直连");
        assert_eq!(n.host, "panel.example.com");
        assert_eq!(n.port, 10000);
        assert_eq!(n.hop, Some((20000, 30000)));
    }

    #[test]
    fn client_opts_is_importable_from_render_client() {
        // 守门测试：ClientOpts / ClientMode 现在定义在 render/client.rs，P4 的 engine.rs 按
        // 这条路径导入。谁把定义搬去 render/mod.rs 而不留 re-export，这里先编译不过。
        let o = crate::render::client::ClientOpts {
            mode: crate::render::client::ClientMode::Mixed,
            socks_port: 1080,
            http_port: 8080,
            host_has_ipv6: false,
            split: crate::render::SplitRules {
                enabled: false,
                global: false,
                keywords: vec![],
            },
        };
        assert_eq!((o.socks_port, o.http_port), (1080, 8080));
    }
}
