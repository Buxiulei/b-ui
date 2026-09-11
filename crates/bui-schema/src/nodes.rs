//! 权益 → 节点集合：把一个用户的 [`Entitlements`](crate::model::Entitlements) 展开成他订阅里该出现的节点。

use crate::model::{NodeParams, Protocol, Residential, User};
use uuid::Uuid;

/// 四条通路之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    RealityDirect,
    RealityResidential,
    Hy2Direct,
    Hy2Residential,
}

/// 节点的协议参数。
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
/// obfs 按 v3 语义只加在 HY2 直连节点上。
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
                obfs,
            ));
        }
        if resi_ok {
            out.push(hy2(
                node.ports.hy2_resi,
                Some(node.ports.hy2_resi_hop),
                NodeKind::Hy2Residential,
                "HY2住宅",
                None,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

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
        // v3: 住宅 HY2 不带 obfs
        match &ns[3].transport {
            Transport::Hysteria2 { obfs_password, .. } => assert_eq!(obfs_password, &None),
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
