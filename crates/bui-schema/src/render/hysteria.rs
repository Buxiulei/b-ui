//! Hysteria2 **直连**实例的配置渲染（模板移植自 v3 `server/core.sh:527-580`）。
//!
//! 4.1 起这个文件只管直连一个实例：住宅 HY2 换成了 sing-box 的单入站 + 凭据池
//! （[`crate::render::hy2_singbox`]），4.0.x 那两个渲染器
//! （`residential_yaml` / `residential_slot_yaml`）连签名一起删了（spec §4.2、§13 C1）。
//!
//! `auth` 段有两种形态（spec §3.2，2026-09-13 裁决）：默认 [`Hy2Auth::Http`] 指向守护进程
//! 自己监听的 [`AUTH_HTTP_PORT`]（进程内应答，不 fork）；[`Hy2Auth::Command`] 是退路开关，
//! 回到钩子 `bin/bui-auth-hook`。**两种形态都只作用于直连**（住宅侧是 sing-box 的静态
//! `users` 列表 + 门，配置里没有 `auth` 段）。`masquerade.proxy.url` 由 REALITY 伪装域推导；
//! `obfs` 段只在启用时输出。
use crate::model::{Hy2Auth, NodeParams};
use crate::paths::Paths;
use serde_yaml::{Mapping, Value};

/// 守护进程的鉴权端口：**只监听 127.0.0.1**，不挂在面板端口上（面板经 Caddy 对外，
/// 鉴权面不能跟着暴露）。选 18789 是为了避开已占用的 8080 / 9991–9999 / 10001 /
/// 10002 / 10085 / 2080+ 与住宅跳跃段 41000–50000。
pub const AUTH_HTTP_PORT: u16 = 18789;

/// 写进直连配置的 `auth.http.url`（4.1：只有 `config.yaml` 还有 `auth` 段）。
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

/// 直连实例的字段（顺序按 v3 模板；4.0.x 的住宅实例也共用过这一份）。
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

    /// 混淆跟着开关走（2026-09-15 裁决：直连与住宅 HY2 同一段 salamander）——
    /// 这里只管直连，住宅那半在 `render::hy2_singbox` 的用例里。
    #[test]
    fn obfs_follows_the_switch_on_direct() {
        let (mut n, p) = (node(), Paths::default_server());
        n.obfs = crate::model::Obfs {
            enabled: true,
            password: "obfs-pw".into(),
        };
        let t = direct_yaml(&n, &p, Hy2Auth::Http);
        let y: Value = serde_yaml::from_str(&t).unwrap();
        assert_eq!(y["obfs"]["type"].as_str(), Some("salamander"), "{t}");
        assert_eq!(
            y["obfs"]["salamander"]["password"].as_str(),
            Some("obfs-pw"),
            "{t}"
        );
        n.obfs.enabled = false;
        let t = direct_yaml(&n, &p, Hy2Auth::Http);
        let y: Value = serde_yaml::from_str(&t).unwrap();
        assert!(y.get("obfs").is_none(), "{t}");
    }

    /// 两种鉴权模式各自的 `auth` 段（2026-09-13 裁决）。http 是默认，command 是退路：
    /// 切模式**只能**改这一段，别的字段一个字都不许动（否则切换会顺带重排端口/伪装）。
    #[test]
    fn the_auth_section_is_the_only_difference_between_the_two_modes() {
        let (n, p) = (node(), Paths::default_server());
        let y: Value = serde_yaml::from_str(&direct_yaml(&n, &p, Hy2Auth::Http)).unwrap();
        assert_eq!(y["auth"]["type"].as_str(), Some("http"));
        assert_eq!(
            y["auth"]["http"]["url"].as_str(),
            Some("http://127.0.0.1:18789/auth")
        );
        assert_eq!(y["auth"]["http"]["insecure"].as_bool(), Some(false));
        assert!(y["auth"].get("command").is_none());

        let y: Value = serde_yaml::from_str(&direct_yaml(&n, &p, Hy2Auth::Command)).unwrap();
        assert_eq!(y["auth"]["type"].as_str(), Some("command"));
        assert_eq!(
            y["auth"]["command"].as_str(),
            Some("/opt/b-ui/bin/bui-auth-hook")
        );
        assert!(y["auth"].get("http").is_none());
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

    /// `"127.0.0.1:9092"` 这种监听地址里的端口号。
    fn port_of(addr: &str) -> u16 {
        addr.rsplit(':').next().unwrap().parse().unwrap()
    }

    /// 全部回环管理面与监听端口互不相撞。加新面时**必须**加进这张表，且**端口要从那一面
    /// 自己的常量取**（写字面量的话，改坏常量这张表照样全绿 —— 它就只是在自证）。
    #[test]
    fn every_loopback_management_port_is_distinct() {
        let ports: Vec<(u16, &str)> = vec![
            (AUTH_HTTP_PORT, "hy2 直连的 http 鉴权"),
            (9999, "hy2 直连 trafficStats"),
            (9091, "relay Clash API"),
            (
                port_of(crate::render::hy2_singbox::HY2_RESI_CLASH_API),
                "住宅 HY2 Clash API",
            ),
            (10085, "Xray api"),
            (
                port_of(crate::render::hy2_singbox::HY2_RESI_V2RAY_API),
                "住宅 HY2 v2ray_api",
            ),
            (10000, "hy2 直连"),
            (10001, "reality 直连"),
            (10002, "reality 住宅"),
            (40000, "住宅 HY2"),
            (8080, "面板"),
        ];
        let mut seen = std::collections::BTreeMap::new();
        for (p, who) in ports {
            if let Some(prev) = seen.insert(p, who) {
                panic!("端口 {p} 被 {prev} 与 {who} 同时占用");
            }
        }
    }
}
