//! Hysteria2 两个实例的配置渲染（模板移植自 v3 `server/core.sh:527-580` 直连、`595-660` 住宅）。
//!
//! 与 v3 的差别：`auth` 段从 `http` / `userpass` 改为 `command`（钩子 `bin/bui-auth-hook`，见 spec §3.2）；
//! `masquerade.proxy.url` 由 REALITY 伪装域推导；`obfs` 段只在启用时输出。
use crate::model::NodeParams;
use crate::paths::Paths;
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

/// 住宅实例 `config-residential.yaml`：出站 socks5 → 本地 relay，`acl: relay(all)`。
pub fn residential_yaml(node: &NodeParams, paths: &Paths) -> String {
    let (start, end) = node.ports.hy2_resi_hop;
    let listen = format!(":{},{}-{}", node.ports.hy2_resi, start, end);
    let mut doc = common_doc(node, paths, &listen, 9998);

    let mut relay = Mapping::new();
    relay.insert(key("name"), str_val("relay"));
    relay.insert(key("type"), str_val("socks5"));
    relay.insert(
        key("socks5"),
        map(vec![("addr", str_val("127.0.0.1:2080"))]),
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
