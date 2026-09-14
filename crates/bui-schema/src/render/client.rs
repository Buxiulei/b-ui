//! 客户端（`bui-c`）的 sing-box 配置渲染：TUN 与 mixed 两种形态。
//!
//! 模板移植 v3 `b-ui-client.sh` 的 `generate_singbox_tun_config`（TUN schema 8）：
//! `interface_name: bui-tun`、`address` 数组、`stack: mixed`、CN 域名直连 DNS、
//! `sniff` + `hijack-dns`、cloudflared QUIC 例外、裸 IPv6 就地 reject。
//! mixed 形态用同一套出站与路由，只把 TUN inbound 换成两个 `mixed` 监听、去掉 DNS 劫持。
//! 另有 [`probe_config`]：`bui-c` 节点测速用的多入站配置，出站与主配置同源（同一个私有 `outbound`）。

use crate::nodes::{Node, NodeKind, Transport};
use crate::render::SplitRules;
use serde_json::{json, Value};

/// 客户端运行形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientMode {
    /// `tun` inbound 全局接管
    Tun,
    /// 两个本地 `mixed` 监听
    Mixed,
}

/// 渲染客户端配置需要的参数。
#[derive(Debug, Clone)]
pub struct ClientOpts {
    /// 当前形态（由调用方记在 profile 里）
    pub mode: ClientMode,
    /// 本地 SOCKS 端口（默认 1080）
    pub socks_port: u16,
    /// 本地 HTTP 端口（默认 8080）
    pub http_port: u16,
    /// 主机有真实 IPv6：TUN 才加 v6 地址（加了 `auto_route` 才会装 `::/0`）
    pub host_has_ipv6: bool,
    /// 住宅分流规则（住宅节点在 split 模式下用）
    pub split: SplitRules,
}

/// TUN 形态配置（schema 8）。
pub fn tun_config(node: &Node, opts: &ClientOpts) -> Value {
    config(node, opts, true)
}

/// mixed 形态配置（无 TUN、无 DNS 劫持）。
pub fn mixed_config(node: &Node, opts: &ClientOpts) -> Value {
    config(node, opts, false)
}

/// 测速的一个目标：节点 + 本地 socks 入站端口 + 这个入站的凭据（调用方随机生成）。
///
/// 没有 tag 字段：标签由 [`probe_config`] 按下标生成，调用方按 `listen_port` 认目标。
/// `Debug` 是手写的脱敏版：只打 `node.label` 与 `listen_port`，`user` / `pass` 一律 `***`
/// （节点自己的服务器口令也不经这里露出来）。
pub struct ProbeTarget<'a> {
    /// 要测的节点（同一个节点、同名节点都可以出现多次）
    pub node: &'a Node,
    /// 本地 socks 入站端口（只监听 127.0.0.1；由调用方分配，各目标互不相同）
    pub listen_port: u16,
    /// 入站认证的用户名（调用方随机生成）
    pub user: String,
    /// 入站认证的密码（调用方随机生成）
    pub pass: String,
}

/// 脱敏的 `Debug`：凭据打 `***`，节点只打标签（`Node` 自己 derive 了 `Debug`，
/// 整个打出来会带上服务器口令）。手写而不是干脆不实现，是为了让调用方带
/// `#[derive(Debug)]` 的结构体装得下它。
impl std::fmt::Debug for ProbeTarget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeTarget")
            .field("node", &self.node.label)
            .field("listen_port", &self.listen_port)
            .field("user", &"***")
            .field("pass", &"***")
            .finish()
    }
}

/// 多节点测速配置（`bui-c` 菜单的 `[9]`）：每个目标一个带认证的 `127.0.0.1` socks 入站，
/// 按入站分流到各自节点的出站，其余流量 `final: direct-out`。
///
/// - 标签按目标在 `targets` 里的下标 `i`（从 0 开始）生成：出站 `probe-<i>`、入站 `probe-in-<i>`，
///   路由规则 `probe-in-<i>` → `probe-<i>`。标签与节点名无关：同一个节点测两次、两台服务器上的
///   同名节点都不会撞 tag，也碰不到 `direct-out`。
/// - `route.auto_detect_interface = true` 必需：不设的话出站连接会被本机正在跑的 TUN 截走。
/// - DNS 只有 `local-dns`（udp 223.5.5.5），出站的 `domain_resolver` 写死引用它；
///   `default_domain_resolver` 与 `strategy: ipv4_only` 同主配置（服务端没有 IPv6 出口，
///   测速也必须只查 A 记录，否则整轮全判不通）。
/// - 没有 tun、hijack-dns、sniff、rule_set、download_detour、cache_file；
///   `log.level = error`（sing-box 的日志可能带出服务器地址，调用方也不读）。
/// - `targets` 为空时产出没有入站的配置，调用方不要这样调。
/// - `listen_port` 撞上本机已占用的端口时，sing-box 在入站 bind 阶段就整体起不来（一个都不测），
///   与「节点不通」是两回事：调用方要把这种失败单独报，别算成网络问题。
pub fn probe_config(targets: &[ProbeTarget<'_>]) -> Value {
    let mut inbounds = Vec::with_capacity(targets.len());
    let mut outbounds = Vec::with_capacity(targets.len() + 1);
    let mut rules = Vec::with_capacity(targets.len());
    for (i, t) in targets.iter().enumerate() {
        let in_tag = format!("probe-in-{i}");
        let out_tag = format!("probe-{i}");
        inbounds.push(json!({
            "type": "socks",
            "tag": in_tag,
            "listen": "127.0.0.1",
            "listen_port": t.listen_port,
            "users": [{ "username": t.user, "password": t.pass }],
        }));
        outbounds.push(outbound(t.node, &out_tag));
        rules.push(json!({ "inbound": [in_tag], "outbound": out_tag }));
    }
    outbounds.push(json!({ "type": "direct", "tag": "direct-out" }));

    json!({
        "log": { "level": "error" },
        "dns": {
            "servers": [
                { "tag": "local-dns", "type": "udp", "server": "223.5.5.5" },
            ],
            "strategy": "ipv4_only",
        },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": "direct-out",
            "auto_detect_interface": true,
            "default_domain_resolver": "local-dns",
        },
    })
}

/// DNS 走国内递归的域名后缀（v3 `b-ui-client.sh` dns.rules）。
const CN_DNS_SUFFIXES: &[&str] = &[
    ".qq.com",
    ".wechat.com",
    ".tencent.com",
    ".myqcloud.com",
    ".xiaohongshu.com",
    ".douyin.com",
    ".bytedance.com",
    ".toutiao.com",
    ".kuaishou.com",
    ".bilibili.com",
    ".taobao.com",
    ".alibaba.com",
    ".alipay.com",
    ".aliyuncs.com",
    ".tmall.com",
    ".jd.com",
    ".baidu.com",
    ".cn",
];

/// 直连的国内域名后缀（v3 `b-ui-client.sh` route.rules）。
const CN_DIRECT_SUFFIXES: &[&str] = &[
    ".qq.com",
    ".qpic.cn",
    ".qlogo.cn",
    ".wechat.com",
    ".weixin.qq.com",
    ".wx.qq.com",
    ".tencent.com",
    ".tencent-cloud.net",
    ".myqcloud.com",
    ".gtimg.com",
    ".xiaohongshu.com",
    ".xhscdn.com",
    ".douyin.com",
    ".douyincdn.com",
    ".amemv.com",
    ".bytedance.com",
    ".bytecdntp.com",
    ".toutiao.com",
    ".iesdouyin.com",
    ".kuaishou.com",
    ".ksapisrv.com",
    ".bilibili.com",
    ".bilivideo.com",
    ".hdslb.com",
    ".taobao.com",
    ".tbcdn.cn",
    ".alicdn.com",
    ".aliyuncs.com",
    ".alipay.com",
    ".alipayobjects.com",
    ".alibaba.com",
    ".alibabacloud.com",
    ".tmall.com",
    ".tmall.hk",
    ".jd.com",
    ".360buyimg.com",
    ".jdcdn.com",
    ".baidu.com",
    ".bdstatic.com",
    ".bdimg.com",
    ".cn",
];

/// 海外 CDN 关键字：必须走代理（直连会落到国内节点被墙/超时）。
const CDN_PROXY_KEYWORDS: &[&str] = &[
    "akamai",
    "akamaized",
    "akamaihd",
    "edgekey",
    "edgesuite",
    "fastly",
    "cloudfront",
];

/// 强制走代理的关键字（开发与 AI 服务）。
const PROXY_KEYWORDS: &[&str] = &[
    "github",
    "google",
    "googleapis",
    "googlevideo",
    "gstatic",
    "gmail",
    "gemini",
    "generativelanguage",
    "anthropic",
    "openai",
    "chatgpt",
    "antigravity",
    "cloudcode",
    "visualstudio",
    "vscode",
];

/// 强制走代理的域名后缀。
const PROXY_SUFFIXES: &[&str] = &[
    ".github.com",
    ".github.io",
    ".githubusercontent.com",
    ".githubassets.com",
    ".google.com",
    ".google.com.hk",
    ".google.co.jp",
    ".goog",
    ".youtube.com",
    ".ytimg.com",
    ".googlesyndication.com",
    ".googleusercontent.com",
    ".ggpht.com",
    ".gemini.google.com",
    ".antigravity.google",
    ".antigravity-unleash.goog",
    ".run.app",
    ".visualstudio.com",
    ".vscode-cdn.net",
    ".aka.ms",
];

fn config(node: &Node, opts: &ClientOpts, tun: bool) -> Value {
    let mut inbounds = Vec::with_capacity(3);
    if tun {
        let mut address = vec!["172.19.0.1/30"];
        if opts.host_has_ipv6 {
            // 有 v6 地址 auto_route 才会把 ::/0 装进 TUN 路由表；
            // 主机无 v6 时加地址会让 TUN 创建失败，宁缺勿滥。
            address.push("fdfe:dcba:9876::1/126");
        }
        inbounds.push(json!({
            "type": "tun",
            "tag": "tun-in",
            "interface_name": "bui-tun",
            "address": address,
            "auto_route": true,
            "strict_route": true,
            "stack": "mixed",
        }));
    }
    inbounds.push(json!({
        "type": "mixed",
        "tag": "socks-in",
        "listen": "127.0.0.1",
        "listen_port": opts.socks_port,
    }));
    inbounds.push(json!({
        "type": "mixed",
        "tag": "http-in",
        "listen": "127.0.0.1",
        "listen_port": opts.http_port,
    }));

    json!({
        "log": { "level": "info" },
        "dns": {
            "servers": [
                { "tag": "proxy-dns", "type": "https", "server": "1.1.1.1", "detour": "proxy-out" },
                { "tag": "local-dns", "type": "udp", "server": "223.5.5.5" },
            ],
            "rules": [
                { "domain_suffix": CN_DNS_SUFFIXES, "server": "local-dns" },
            ],
            "final": "proxy-dns",
            "strategy": "ipv4_only",
        },
        "inbounds": inbounds,
        "outbounds": [ outbound(node, "proxy-out"), { "type": "direct", "tag": "direct-out" } ],
        "route": {
            "rules": route_rules(node, opts, tun),
            "final": "proxy-out",
            "auto_detect_interface": true,
            "default_domain_resolver": "local-dns",
        },
    })
}

/// 节点 → sing-box 出站 JSON。客户端主配置用 `proxy-out`，[`probe_config`] 用 `probe-<i>`。
///
/// 私有：`domain_resolver` 写死引用 tag 为 `local-dns` 的 DNS 服务器，
/// 放进哪份配置那份就得自带它（主配置与 [`probe_config`] 都有）——这个前置条件签名里看不出来，
/// 所以片段只在本模块内部拼。
fn outbound(node: &Node, tag: &str) -> Value {
    match &node.transport {
        Transport::Hysteria2 {
            username,
            password,
            sni,
            obfs_password,
        } => {
            let mut o = json!({
                "type": "hysteria2",
                "tag": tag,
                "server": node.host,
                "server_port": node.port,
                "password": format!("{username}:{password}"),
                "domain_resolver": "local-dns",
                "tls": { "enabled": true, "server_name": sni, "insecure": false },
            });
            let m = o.as_object_mut().unwrap();
            if let Some((start, end)) = node.hop {
                m.insert("server_ports".into(), json!([format!("{start}:{end}")]));
                m.insert("hop_interval".into(), json!("30s"));
            }
            if let Some(pw) = obfs_password {
                m.insert(
                    "obfs".into(),
                    json!({ "type": "salamander", "password": pw }),
                );
            }
            o
        }
        Transport::Reality {
            uuid,
            public_key,
            short_id,
            server_name,
            fingerprint,
            flow,
        } => json!({
            "type": "vless",
            "tag": tag,
            "server": node.host,
            "server_port": node.port,
            "uuid": uuid.to_string(),
            "flow": flow,
            "domain_resolver": "local-dns",
            "tls": {
                "enabled": true,
                "server_name": server_name,
                "utls": { "enabled": true, "fingerprint": fingerprint },
                "reality": { "enabled": true, "public_key": public_key, "short_id": short_id },
            },
        }),
    }
}

/// 路由规则：顺序即优先级（移植 v3 TUN schema 8）。
fn route_rules(node: &Node, opts: &ClientOpts, tun: bool) -> Vec<Value> {
    let mut rules = vec![
        // cloudflared 的 QUIC tunnel 被 sniff 误判成 DNS，进程与端口两道例外
        json!({ "process_name": ["cloudflared"], "outbound": "direct-out" }),
        json!({ "network": "udp", "port": [7844], "outbound": "direct-out" }),
        json!({ "action": "sniff" }),
    ];
    if tun {
        rules.push(json!({ "protocol": "dns", "action": "hijack-dns" }));
    }
    rules.push(json!({ "port": [22, 2222], "outbound": "direct-out" }));
    rules.push(json!({ "source_port": [22, 2222], "outbound": "direct-out" }));
    rules.push(json!({ "ip_is_private": true, "outbound": "direct-out" }));
    // 服务端无 IPv6 出口：裸 v6 目标就地 reject，应用立刻回退 IPv4
    rules.push(json!({ "ip_version": 6, "action": "reject" }));
    // 住宅节点 + split 模式：关键字域名必须走住宅出口，优先于国内域名直连
    let residential = matches!(
        node.kind,
        NodeKind::RealityResidential | NodeKind::Hy2Residential
    );
    if residential && opts.split.enabled && !opts.split.global && !opts.split.keywords.is_empty() {
        rules.push(json!({ "domain_keyword": opts.split.keywords, "outbound": "proxy-out" }));
    }
    rules.push(json!({ "domain_suffix": CN_DIRECT_SUFFIXES, "outbound": "direct-out" }));
    rules.push(json!({ "domain_keyword": CDN_PROXY_KEYWORDS, "outbound": "proxy-out" }));
    rules.push(json!({ "domain_keyword": PROXY_KEYWORDS, "outbound": "proxy-out" }));
    rules.push(json!({ "domain_suffix": PROXY_SUFFIXES, "outbound": "proxy-out" }));
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 出站分支最全的示例节点：HY2 带端口跳跃与 obfs。
    fn hy2_node() -> Node {
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
                obfs_password: Some("obfs-pw".into()),
            },
        }
    }

    fn reality_node() -> Node {
        Node {
            kind: NodeKind::RealityResidential,
            label: "Reality住宅".into(),
            host: "panel.example.com".into(),
            port: 10002,
            hop: None,
            transport: Transport::Reality {
                uuid: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
                public_key: "PUB".into(),
                short_id: "0123456789abcdef".into(),
                server_name: "www.bing.com".into(),
                fingerprint: "chrome".into(),
                flow: "xtls-rprx-vision".into(),
            },
        }
    }

    fn opts(mode: ClientMode) -> ClientOpts {
        ClientOpts {
            mode,
            socks_port: 1080,
            http_port: 8080,
            host_has_ipv6: false,
            split: SplitRules {
                enabled: true,
                global: false,
                keywords: vec!["openai.com".into()],
            },
        }
    }

    /// 每个节点一个目标；端口与凭据都是示例值，这里不真的监听。
    fn probe_targets(nodes: &[Node]) -> Vec<ProbeTarget<'_>> {
        nodes
            .iter()
            .enumerate()
            .map(|(i, n)| ProbeTarget {
                node: n,
                listen_port: 20800 + i as u16,
                user: format!("probe-user-{i}"),
                pass: format!("probe-pass-{i}"),
            })
            .collect()
    }

    /// 测速出站与主配置出站同源：同一个 `outbound`，只有 tag 不同。
    /// 这条断言原先在 `tests/kernel_client.rs`，为了写它才把 `outbound` 公开成 `node_outbound`；
    /// 搬进同文件测试之后私有函数照样可见，公开项不必留。
    #[test]
    fn probe_and_main_outbounds_come_from_one_renderer() {
        let nodes = [hy2_node(), reality_node()];
        let targets = probe_targets(&nodes);
        let cfg = probe_config(&targets);
        let outbounds = cfg["outbounds"].as_array().unwrap();
        assert_eq!(outbounds.len(), nodes.len() + 1);
        for (i, t) in targets.iter().enumerate() {
            assert_eq!(outbounds[i], outbound(t.node, &format!("probe-{i}")));
        }
        for n in &nodes {
            let tun = tun_config(n, &opts(ClientMode::Tun));
            assert_eq!(tun["outbounds"][0], outbound(n, "proxy-out"));
            let mixed = mixed_config(n, &opts(ClientMode::Mixed));
            assert_eq!(mixed["outbounds"][0], outbound(n, "proxy-out"));
        }
    }

    /// IPv6 接管是项目级不变量（服务端没有 IPv6 出口）：测速配置一旦回退成默认 DNS 策略，
    /// 现象是测速走 AAAA、所有节点被判不通，而主配置照常可用——静默错测，最难查。
    #[test]
    fn probe_config_pins_ipv4_only_dns() {
        let nodes = [hy2_node(), reality_node()];
        let cfg = probe_config(&probe_targets(&nodes));
        assert_eq!(cfg["dns"]["strategy"], "ipv4_only");
        // domain_resolver 指名 local-dns：解析器与这份配置里唯一的 DNS 服务器必须对得上
        assert_eq!(cfg["route"]["default_domain_resolver"], "local-dns");
        assert_eq!(cfg["dns"]["servers"][0]["tag"], "local-dns");
        for out in cfg["outbounds"].as_array().unwrap() {
            if out["type"] != "direct" {
                assert_eq!(out["domain_resolver"], "local-dns");
            }
        }
    }

    /// 手写的脱敏 `Debug`：只打标签与端口，入站凭据与节点里的服务器口令都不露。
    #[test]
    fn probe_target_debug_redacts_credentials() {
        let nodes = [hy2_node()];
        let targets = probe_targets(&nodes);
        let s = format!("{:?}", targets[0]);
        assert!(s.contains("HY2直连"), "{s}");
        assert!(s.contains("20800"), "{s}");
        // host 也不能露：真机上 label 带面板用户名，域名 + 用户名就等于订阅凭据
        for secret in [
            "probe-user-0",
            "probe-pass-0",
            "hy2-pw",
            "obfs-pw",
            "panel.example.com",
        ] {
            assert!(!s.contains(secret), "{secret} 不该出现在 {s}");
        }
    }
}
