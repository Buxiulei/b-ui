//! Hysteria2 两个实例的配置渲染（模板移植自 v3 `server/core.sh:527-580` 直连、`595-660` 住宅）。
//!
//! `auth` 段有两种形态（spec §3.2，2026-09-13 裁决）：默认 [`Hy2Auth::Http`] 指向守护进程
//! 自己监听的 [`AUTH_HTTP_PORT`]（进程内应答，不 fork）；[`Hy2Auth::Command`] 是退路开关，
//! 回到钩子 `bin/bui-auth-hook`。`masquerade.proxy.url` 由 REALITY 伪装域推导；
//! `obfs` 段只在启用时输出。
use crate::model::{Hy2Auth, NodeParams};
use crate::paths::Paths;
use crate::slots::{self, SlotRes};
use serde_yaml::{Mapping, Value};

/// 守护进程的鉴权端口：**只监听 127.0.0.1**，不挂在面板端口上（面板经 Caddy 对外，
/// 鉴权面不能跟着暴露）。选 18789 是为了避开已占用的 8080 / 9991–9999 / 10001 /
/// 10002 / 10085 / 2080+ 与住宅跳跃段 41000–50000。
pub const AUTH_HTTP_PORT: u16 = 18789;

/// 写进两份配置的 `auth.http.url`。
pub fn auth_http_url() -> String {
    format!("http://127.0.0.1:{AUTH_HTTP_PORT}/auth")
}

/// 直连实例 `config.yaml`。
pub fn direct_yaml(node: &NodeParams, paths: &Paths, auth: Hy2Auth) -> String {
    let listen = match node.ports.hy2_hop {
        Some((start, end)) => format!(":{},{}-{}", node.ports.hy2, start, end),
        None => format!(":{}", node.ports.hy2),
    };
    let mut doc = common_doc(node, paths, &listen, 9999, auth);
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
pub fn residential_slot_yaml(
    node: &NodeParams,
    paths: &Paths,
    res: &SlotRes,
    auth: Hy2Auth,
) -> String {
    let listen = format!(":{},{}-{}", res.hy2_port, res.hop.0, res.hop.1);
    let mut doc = common_doc(node, paths, &listen, res.stats_port, auth);

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
    // 混淆覆盖全部 HY2 实例（2026-09-15 裁决）：订阅给住宅 HY2 节点同样带 obfs 参数，两边必须一致。
    push_obfs(&mut doc, node);
    to_yaml(doc)
}

/// 单槽（槽 0）的住宅配置。保留这个签名给 golden 与 `import-v3`：
/// 池空 / 只有一条上游时，输出与 v3 单实例逐字节相同。
pub fn residential_yaml(node: &NodeParams, paths: &Paths, auth: Hy2Auth) -> String {
    residential_slot_yaml(node, paths, &slots::resources(&node.ports, 0, 1), auth)
}

/// 两个实例共有的字段（顺序按 v3 模板）。
fn common_doc(
    node: &NodeParams,
    paths: &Paths,
    listen: &str,
    traffic_port: u16,
    auth: Hy2Auth,
) -> Mapping {
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
    doc.insert(key("auth"), auth_section(paths, auth));
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

/// `auth` 段：http 模式指向守护进程的回环端口，command 模式回到钩子。
///
/// command 那一支的 `command` 必须是**单个不带参数**的可执行路径：内核
/// `exec.Command(a.Cmd, addr, auth, tx)` 不过 shell、不按空格拆参数（调研 H15）。
/// `bin/bui-auth-hook` 是 `bin/bui` 的符号链接，`bui` 按 argv[0] 认出这个名字就直接进钩子。
fn auth_section(paths: &Paths, auth: Hy2Auth) -> Value {
    match auth {
        Hy2Auth::Http => map(vec![
            ("type", str_val("http")),
            (
                "http",
                map(vec![
                    ("url", str_val(&auth_http_url())),
                    // 明文回环，没有证书可校验；写 false 是为了让这一位在配置里显式可见
                    ("insecure", Value::Bool(false)),
                ]),
            ),
        ]),
        Hy2Auth::Command => map(vec![
            ("type", str_val("command")),
            ("command", str_val(&paths.auth_hook_bin().to_string_lossy())),
        ]),
    }
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
            residential_slot_yaml(&n, &p, &one, Hy2Auth::Http),
            residential_yaml(&n, &p, Hy2Auth::Http)
        );
        let text = residential_yaml(&n, &p, Hy2Auth::Http);
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
            .map(|i| {
                residential_slot_yaml(&n, &p, &slots::resources(&n.ports, i, 3), Hy2Auth::Command)
            })
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
        // node() 的 obfs 是关闭的：住宅实例不输出 obfs 段
        for t in &texts {
            assert!(!t.contains("salamander"));
            assert!(t.contains("relay(all)"));
            assert!(t.contains("/opt/b-ui/bin/bui-auth-hook"));
        }
    }

    /// 混淆覆盖全部 HY2 实例（2026-09-15 裁决）：开启时直连与每个住宅槽都带同一段 salamander，
    /// 关闭时一律不带 —— 订阅给直连与住宅 HY2 节点都带 obfs 参数，服务端必须一致。
    #[test]
    fn obfs_follows_the_switch_on_direct_and_every_residential_slot() {
        let (mut n, p) = (node(), Paths::default_server());
        let render_all = |n: &NodeParams| -> Vec<String> {
            let mut v = vec![direct_yaml(n, &p, Hy2Auth::Http)];
            v.extend((0..3).map(|i| {
                residential_slot_yaml(n, &p, &slots::resources(&n.ports, i, 3), Hy2Auth::Http)
            }));
            v
        };
        n.obfs = crate::model::Obfs {
            enabled: true,
            password: "obfs-pw".into(),
        };
        for t in render_all(&n) {
            let y: Value = serde_yaml::from_str(&t).unwrap();
            assert_eq!(y["obfs"]["type"].as_str(), Some("salamander"), "{t}");
            assert_eq!(
                y["obfs"]["salamander"]["password"].as_str(),
                Some("obfs-pw"),
                "{t}"
            );
        }
        n.obfs.enabled = false;
        for t in render_all(&n) {
            let y: Value = serde_yaml::from_str(&t).unwrap();
            assert!(y.get("obfs").is_none(), "{t}");
        }
    }

    /// 两种鉴权模式各自的 `auth` 段（2026-09-13 裁决）。http 是默认，command 是退路：
    /// 切模式**只能**改这一段，别的字段一个字都不许动（否则切换会顺带重排端口/伪装）。
    #[test]
    fn the_auth_section_is_the_only_difference_between_the_two_modes() {
        let (n, p) = (node(), Paths::default_server());
        for text in [
            direct_yaml(&n, &p, Hy2Auth::Http),
            residential_yaml(&n, &p, Hy2Auth::Http),
        ] {
            let y: Value = serde_yaml::from_str(&text).unwrap();
            assert_eq!(y["auth"]["type"].as_str(), Some("http"));
            assert_eq!(
                y["auth"]["http"]["url"].as_str(),
                Some("http://127.0.0.1:18789/auth")
            );
            assert_eq!(y["auth"]["http"]["insecure"].as_bool(), Some(false));
            assert!(y["auth"].get("command").is_none());
        }
        for text in [
            direct_yaml(&n, &p, Hy2Auth::Command),
            residential_yaml(&n, &p, Hy2Auth::Command),
        ] {
            let y: Value = serde_yaml::from_str(&text).unwrap();
            assert_eq!(y["auth"]["type"].as_str(), Some("command"));
            assert_eq!(
                y["auth"]["command"].as_str(),
                Some("/opt/b-ui/bin/bui-auth-hook")
            );
            assert!(y["auth"].get("http").is_none());
        }
        // 除 auth 之外逐键相等
        let mut a: Mapping = serde_yaml::from_str(&direct_yaml(&n, &p, Hy2Auth::Http)).unwrap();
        let mut b: Mapping = serde_yaml::from_str(&direct_yaml(&n, &p, Hy2Auth::Command)).unwrap();
        a.remove(key("auth"));
        b.remove(key("auth"));
        assert_eq!(a, b);
    }

    /// 端口是常量，且必须避开已被占用的那些（改了它就要同步改守护进程的监听与文档）。
    #[test]
    fn the_auth_http_port_avoids_every_other_listener() {
        assert_eq!(AUTH_HTTP_PORT, 18789);
        assert_eq!(auth_http_url(), "http://127.0.0.1:18789/auth");
        for taken in [8080u16, 9991, 9998, 9999, 10001, 10002, 10085, 2080, 2087] {
            assert_ne!(AUTH_HTTP_PORT, taken);
        }
        assert!(
            !(41000..=50000).contains(&AUTH_HTTP_PORT),
            "不能落进住宅跳跃段"
        );
    }
}
