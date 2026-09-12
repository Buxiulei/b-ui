//! Hysteria2 两个实例的配置渲染（模板移植自 v3 `server/core.sh:527-580` 直连、`595-660` 住宅）。
//!
//! 与 v3 的差别：`auth` 段从 `http` / `userpass` 改为 `command`（钩子 `bin/bui-auth-hook`，见 spec §3.2）；
//! `masquerade.proxy.url` 由 REALITY 伪装域推导；`obfs` 段只在启用时输出。
use crate::model::NodeParams;
use crate::paths::Paths;
use crate::slots::{self, SlotRes};
use serde_yaml::{Mapping, Value};

/// 直连实例 `config.yaml`。
pub fn direct_yaml(node: &NodeParams, paths: &Paths) -> String {
    let listen = match node.ports.hy2_hop {
        Some((start, end)) => format!(":{},{}-{}", node.ports.hy2, start, end),
        None => format!(":{}", node.ports.hy2),
    };
    let mut doc = common_doc(node, paths, &listen, 9999);
    doc.insert(
        key("outbounds"),
        Value::Sequence(vec![Value::Mapping(direct_outbound())]),
    );
    push_obfs(&mut doc, node);
    to_yaml(doc)
}

/// 住宅实例 `config-residential[-<i>].yaml`：出站 socks5 → 本槽的 relay 入站，`acl: relay(all)`。
///
/// 每个槽一个实例（spec §5.6）：监听 `:{hy2_port},{hop.0}-{hop.1}`，出站
/// `127.0.0.1:{relay_port}`，`trafficStats` 监听 `127.0.0.1:{stats_port}`。
/// 端口全部来自 [`SlotRes`]，本函数不自己算任何端口。
pub fn residential_slot_yaml(node: &NodeParams, paths: &Paths, res: &SlotRes) -> String {
    let listen = format!(":{},{}-{}", res.hy2_port, res.hop.0, res.hop.1);
    let mut doc = common_doc(node, paths, &listen, res.stats_port);

    let mut relay = Mapping::new();
    relay.insert(key("name"), str_val("relay"));
    relay.insert(key("type"), str_val("socks5"));
    relay.insert(
        key("socks5"),
        map(vec![(
            "addr",
            str_val(&format!("127.0.0.1:{}", res.relay_port)),
        )]),
    );
    doc.insert(
        key("outbounds"),
        Value::Sequence(vec![
            Value::Mapping(relay),
            Value::Mapping(direct_outbound()),
        ]),
    );
    doc.insert(
        key("acl"),
        map(vec![(
            "inline",
            Value::Sequence(vec![str_val("relay(all)")]),
        )]),
    );
    // 住宅实例不带 obfs：v3 的订阅只给直连节点 obfs 参数，两边必须一致。
    to_yaml(doc)
}

/// 单槽（槽 0）的住宅配置。保留这个签名给 golden 与 `import-v3`：
/// 池空 / 只有一条上游时，输出与 v3 单实例逐字节相同。
pub fn residential_yaml(node: &NodeParams, paths: &Paths) -> String {
    residential_slot_yaml(node, paths, &slots::resources(&node.ports, 0, 1))
}

/// 两个实例共有的字段（顺序按 v3 模板）。
fn common_doc(node: &NodeParams, paths: &Paths, listen: &str, traffic_port: u16) -> Mapping {
    let certs = paths.certs_dir.display();
    let mut doc = Mapping::new();
    doc.insert(key("listen"), str_val(listen));
    doc.insert(
        key("tls"),
        map(vec![
            ("sniGuard", str_val("disable")),
            ("cert", str_val(&format!("{certs}/fullchain.pem"))),
            ("key", str_val(&format!("{certs}/privkey.pem"))),
        ]),
    );
    doc.insert(key("quic"), map(vec![("maxIdleTimeout", str_val("60s"))]));
    doc.insert(key("ignoreClientBandwidth"), Value::Bool(true));
    doc.insert(
        key("resolver"),
        map(vec![
            ("type", str_val("https")),
            (
                "https",
                map(vec![
                    ("addr", str_val("1.1.1.1:443")),
                    ("sni", str_val("cloudflare-dns.com")),
                ]),
            ),
        ]),
    );
    doc.insert(
        key("auth"),
        map(vec![
            ("type", str_val("command")),
            // 必须是**单个不带参数**的可执行路径：内核 `exec.Command(a.Cmd, addr, auth, tx)`
            // 不过 shell、不按空格拆参数（调研 H15）。`bin/bui-auth-hook` 是 `bin/bui` 的
            // 符号链接，`bui` 按 argv[0] 认出这个名字就直接进钩子。
            ("command", str_val(&paths.auth_hook_bin().to_string_lossy())),
        ]),
    );
    doc.insert(
        key("trafficStats"),
        map(vec![
            ("listen", str_val(&format!("127.0.0.1:{traffic_port}"))),
            ("secret", str_val("")),
        ]),
    );
    doc.insert(
        key("masquerade"),
        map(vec![
            ("type", str_val("proxy")),
            (
                "proxy",
                map(vec![
                    ("url", str_val(&format!("https://{}", node.reality.sni()))),
                    ("rewriteHost", Value::Bool(true)),
                ]),
            ),
        ]),
    );
    doc.insert(
        key("sniff"),
        map(vec![
            ("enable", Value::Bool(true)),
            ("timeout", str_val("2s")),
            ("rewriteDomain", Value::Bool(true)),
            ("tcpPorts", str_val("80,443,8000-9000")),
            ("udpPorts", str_val("443,53")),
        ]),
    );
    doc
}

/// 内置 direct 出站，`mode: 4` = 只拨 IPv4（VPS 无 IPv6 出口）。
fn direct_outbound() -> Mapping {
    let mut o = Mapping::new();
    o.insert(key("name"), str_val("direct"));
    o.insert(key("type"), str_val("direct"));
    o.insert(key("direct"), map(vec![("mode", Value::Number(4.into()))]));
    o
}

/// 启用混淆时追加 salamander 段（v3 由 CLI 插在文件顶部，位置无关）。
fn push_obfs(doc: &mut Mapping, node: &NodeParams) {
    if !node.obfs.enabled {
        return;
    }
    doc.insert(
        key("obfs"),
        map(vec![
            ("type", str_val("salamander")),
            (
                "salamander",
                map(vec![("password", str_val(&node.obfs.password))]),
            ),
        ]),
    );
}

fn key(k: &str) -> Value {
    str_val(k)
}

fn str_val(s: &str) -> Value {
    Value::String(s.to_string())
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Mapping::new();
    for (k, v) in pairs {
        m.insert(key(k), v);
    }
    Value::Mapping(m)
}

fn to_yaml(doc: Mapping) -> String {
    serde_yaml::to_string(&Value::Mapping(doc)).expect("hysteria 配置序列化不会失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots;

    fn node() -> NodeParams {
        serde_json::from_str(
            r#"{"id":"8d5a1a1e-3b2c-4d1e-9f00-000000000001","name":"node-a","domain":"example.com","public_ip":"203.0.113.10",
            "ports":{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],
                     "reality_direct":10001,"reality_resi":10002,"admin":8080},
            "reality":{"private_key":"a","public_key":"b","short_ids":["0123456789abcdef"],"dest":"www.bing.com:443","server_names":["www.bing.com"]},
            "obfs":{"enabled":false,"password":""}}"#,
        )
        .unwrap()
    }

    /// 槽 0 单槽的输出必须与今天的 `residential_yaml` **逐字节相同**：
    /// golden、M1 验收与 `import-v3` 都按它写死。
    #[test]
    fn slot_zero_of_a_single_slot_pool_is_byte_identical_to_the_old_output() {
        let (n, p) = (node(), Paths::default_server());
        let one = slots::resources(&n.ports, 0, 1);
        assert_eq!(
            residential_slot_yaml(&n, &p, &one),
            residential_yaml(&n, &p)
        );
        let text = residential_yaml(&n, &p);
        // serde_yaml 对 `:40000,…` 这种以冒号开头的标量不加引号，原样输出
        assert!(
            text.lines().any(|l| l == "listen: :40000,41000-50000"),
            "{text}"
        );
        assert!(text.contains("127.0.0.1:9998"));
        assert!(text.contains("127.0.0.1:2080"));
    }

    #[test]
    fn each_slot_gets_its_own_listen_relay_and_stats_port() {
        let (n, p) = (node(), Paths::default_server());
        let texts: Vec<String> = (0..3)
            .map(|i| residential_slot_yaml(&n, &p, &slots::resources(&n.ports, i, 3)))
            .collect();
        let listen = |t: &str| -> String {
            t.lines()
                .find(|l| l.starts_with("listen:"))
                .unwrap()
                .to_string()
        };
        assert!(listen(&texts[0]).contains(":40000,41000-43999"));
        assert!(listen(&texts[1]).contains(":40001,44000-46999"));
        assert!(listen(&texts[2]).contains(":40002,47000-50000"));
        assert!(
            texts[1].contains("127.0.0.1:2081"),
            "出站指向本槽的 relay 入站"
        );
        assert!(texts[1].contains("127.0.0.1:9997"), "trafficStats 按槽递减");
        assert!(texts[2].contains("127.0.0.1:2082"));
        assert!(texts[2].contains("127.0.0.1:9996"));
        // 住宅实例一律不带 obfs（v3 语义：订阅只给直连节点 obfs 参数）
        for t in &texts {
            assert!(!t.contains("salamander"));
            assert!(t.contains("relay(all)"));
            assert!(t.contains("/opt/b-ui/bin/bui-auth-hook"));
        }
    }
}
