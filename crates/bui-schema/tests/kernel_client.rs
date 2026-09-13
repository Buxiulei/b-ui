//! 客户端 sing-box 配置渲染（TUN schema 8 / mixed）+ 真实内核校验。
mod common;

use bui_schema::nodes::{Node, NodeKind, Transport};
use bui_schema::render::client::{self, ClientMode, ClientOpts};
use bui_schema::render::SplitRules;

fn copts(mode: ClientMode, v6: bool, split: SplitRules) -> ClientOpts {
    ClientOpts {
        mode,
        socks_port: 1080,
        http_port: 8080,
        host_has_ipv6: v6,
        split,
    }
}

#[test]
fn tun_config_matches_schema8_and_checks() {
    let s = common::state("global");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    assert_eq!(nodes.len(), 4, "alice 是 fusion + 住宅");
    for n in &nodes {
        let cfg = client::tun_config(n, &copts(ClientMode::Tun, true, split.clone()));
        let tun = &cfg["inbounds"][0];
        assert_eq!(tun["type"], "tun");
        assert_eq!(tun["interface_name"], "bui-tun");
        assert_eq!(tun["stack"], "mixed");
        assert_eq!(tun["auto_route"], true);
        assert_eq!(tun["strict_route"], true);
        assert!(
            tun["address"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a.as_str().unwrap().contains(':')),
            "v6 主机接管 ::/0"
        );
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["action"] == "hijack-dns"));
        assert!(rules
            .iter()
            .any(|r| r["ip_version"] == 6 && r["action"] == "reject"));
        assert_eq!(cfg["route"]["final"], "proxy-out");
        assert!(cfg
            .get("route")
            .unwrap()
            .get("default_domain_resolver")
            .is_some());
        assert_eq!(cfg["dns"]["strategy"], "ipv4_only");
        assert!(!serde_json::to_string(&cfg).unwrap().contains("rule_set"));
        common::check_singbox(&cfg);

        let cfg4 = client::tun_config(n, &copts(ClientMode::Tun, false, split.clone()));
        assert!(cfg4["inbounds"][0]["address"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| !a.as_str().unwrap().contains(':')));
        common::check_singbox(&cfg4);
    }
}

#[test]
fn tun_outbound_follows_node_transport() {
    let s = common::state("obfs");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    let cfg = client::tun_config(&nodes[0], &copts(ClientMode::Tun, false, split.clone()));
    let o = &cfg["outbounds"][0];
    assert_eq!(o["type"], "vless");
    assert_eq!(o["tag"], "proxy-out");
    assert_eq!(o["server"], "example.com");
    assert_eq!(o["server_port"], 10001);
    assert_eq!(o["flow"], "xtls-rprx-vision");
    assert_eq!(o["tls"]["server_name"], "www.bing.com");
    assert_eq!(o["tls"]["utls"]["fingerprint"], "chrome");
    assert_eq!(o["tls"]["reality"]["short_id"], "0123456789abcdef");
    assert_eq!(o["domain_resolver"], "local-dns");

    // HY2 直连：密码 = username:password，端口跳跃 + salamander obfs
    let cfg = client::tun_config(&nodes[2], &copts(ClientMode::Tun, false, split.clone()));
    let o = &cfg["outbounds"][0];
    assert_eq!(o["type"], "hysteria2");
    assert_eq!(o["server_port"], 10000);
    assert_eq!(o["password"], "alice:pw-alice-01");
    assert_eq!(o["server_ports"], serde_json::json!(["20000:30000"]));
    assert_eq!(o["hop_interval"], "30s");
    assert_eq!(o["obfs"]["type"], "salamander");
    assert_eq!(o["obfs"]["password"], "obfs-pw-test");
    assert_eq!(o["tls"]["server_name"], "example.com");
    assert_eq!(o["tls"]["insecure"], false);
    common::check_singbox(&cfg);

    // HY2 住宅：v3 语义不带 obfs
    let cfg = client::tun_config(&nodes[3], &copts(ClientMode::Tun, false, split));
    assert!(cfg["outbounds"][0].get("obfs").is_none());
    assert_eq!(
        cfg["outbounds"][0]["server_ports"],
        serde_json::json!(["41000:50000"])
    );
    common::check_singbox(&cfg);
}

#[test]
fn residential_node_gets_keyword_split_before_cn_direct() {
    let s = common::state("split");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    let kw = serde_json::json!({
        "domain_keyword": split.keywords.clone(),
        "outbound": "proxy-out",
    });

    // 住宅节点：关键字规则存在且排在国内域名直连之前
    let cfg = client::tun_config(&nodes[1], &copts(ClientMode::Tun, false, split.clone()));
    let rules = cfg["route"]["rules"].as_array().unwrap();
    let at = rules
        .iter()
        .position(|r| *r == kw)
        .expect("住宅关键字分流规则");
    let cn = rules
        .iter()
        .position(|r| r["outbound"] == "direct-out" && r.get("domain_suffix").is_some())
        .expect("国内域名直连规则");
    assert!(at < cn, "住宅关键字优先于 cn 直连");
    common::check_singbox(&cfg);

    // 直连节点：不加该规则
    let cfg = client::tun_config(&nodes[0], &copts(ClientMode::Tun, false, split));
    assert!(!cfg["route"]["rules"].as_array().unwrap().contains(&kw));
}

#[test]
fn mixed_config_has_two_mixed_inbounds() {
    let s = common::state("global");
    let u = &s.users[0];
    let n = &bui_schema::nodes::nodes_for(u, &s.node, &s.residential)[2];
    let cfg = client::mixed_config(
        n,
        &copts(
            ClientMode::Mixed,
            true,
            SplitRules::from_group(s.residential.default_group().unwrap()),
        ),
    );
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(inb.len(), 2);
    assert_eq!(inb[0]["type"], "mixed");
    assert_eq!(inb[0]["listen_port"], 1080);
    assert_eq!(inb[1]["listen_port"], 8080);
    assert!(inb.iter().all(|i| i["type"] != "tun"));
    // 无 DNS 劫持
    assert!(!cfg["route"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["action"] == "hijack-dns"));
    assert_eq!(cfg["route"]["final"], "proxy-out");
    common::check_singbox(&cfg);
}

// ---- golden：客户端主配置（tun_config / mixed_config）的完整输出 ----
//
// 节点形状照 bui-c `testutil`（合成域名、合成密码，与生产无关）；数据自带、不走 v3 fixture，
// 这组 golden 只随 `render::client` 变。模板有意改动（例如域名表加一个后缀）时，
// 用 `rerecord_client_main_config_golden` 重录文件末尾的常量。

fn hy2_direct_node() -> Node {
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

fn hy2_resi_node() -> Node {
    Node {
        kind: NodeKind::Hy2Residential,
        label: "HY2住宅".into(),
        port: 40000,
        hop: Some((41000, 50000)),
        ..hy2_direct_node()
    }
}

fn reality_direct_node() -> Node {
    Node {
        kind: NodeKind::RealityDirect,
        label: "Reality直连".into(),
        host: "panel.example.com".into(),
        port: 10001,
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

/// 关键字写死一张小表，不跟 `DEFAULT_KEYWORDS` 走，免得改默认表连带改 golden。
fn split(global: bool) -> SplitRules {
    SplitRules {
        enabled: true,
        global,
        keywords: vec!["openai.com".into(), "anthropic.com".into()],
    }
}

/// 四个场景合起来走遍出站的每个分支（HY2 跳跃 + obfs、HY2 无跳跃无 obfs、Reality）
/// 与 `config` 的每个分支（tun 带 v6 / 不带 v6、mixed、住宅关键字分流）。
fn golden_cases() -> Vec<(&'static str, serde_json::Value, &'static str)> {
    let mut hy2_obfs = hy2_direct_node();
    if let Transport::Hysteria2 { obfs_password, .. } = &mut hy2_obfs.transport {
        *obfs_password = Some("obfs-pw".into());
    }
    let mut hy2_no_hop = hy2_direct_node();
    hy2_no_hop.hop = None;
    vec![
        (
            "tun_hy2_obfs_v6",
            client::tun_config(&hy2_obfs, &copts(ClientMode::Tun, true, split(true))),
            GOLDEN_TUN_HY2_OBFS_V6,
        ),
        (
            "tun_hy2_resi_split",
            client::tun_config(
                &hy2_resi_node(),
                &copts(ClientMode::Tun, false, split(false)),
            ),
            GOLDEN_TUN_HY2_RESI_SPLIT,
        ),
        (
            "mixed_reality",
            client::mixed_config(
                &reality_direct_node(),
                &copts(ClientMode::Mixed, false, split(true)),
            ),
            GOLDEN_MIXED_REALITY,
        ),
        (
            "mixed_hy2_no_hop",
            client::mixed_config(&hy2_no_hop, &copts(ClientMode::Mixed, false, split(false))),
            GOLDEN_MIXED_HY2_NO_HOP,
        ),
    ]
}

#[test]
fn client_main_configs_match_golden() {
    for (what, cfg, want) in golden_cases() {
        // serde_json 没开 preserve_order，键按字典序输出，字符串比较就是逐字节比较
        assert_eq!(serde_json::to_string(&cfg).unwrap(), want, "{what}");
    }
}

// 重录 golden：有意改动客户端模板之后跑
//   cargo test -p bui-schema --test kernel_client -- --ignored rerecord_client_main_config_golden --nocapture
// 把打印出的四行常量整段替换文件末尾的同名常量，再跑 client_main_configs_match_golden 确认变绿，
// 并逐行看一遍 diff，确认只变了有意改的地方。
#[test]
#[ignore = "重录 golden 用，平时不跑"]
fn rerecord_client_main_config_golden() {
    for (what, cfg, _) in golden_cases() {
        let json = serde_json::to_string(&cfg).unwrap();
        // 常量是 r#"…"# 原始字符串，输出里出现 "# 就得改用 r##"…"##
        assert!(
            !json.contains("\"#"),
            "{what} 的输出含 \"#，改用 r##\"…\"## 再贴"
        );
        println!(
            "const GOLDEN_{}: &str = r#\"{json}\"#;",
            what.to_uppercase()
        );
    }
}

// ---- 测速探测配置（bui-c [9]）----

/// 每个节点一个目标（同一个节点可以出现多次）；端口与凭据都是示例值，端口不真正监听。
fn probe_targets<'a>(nodes: impl IntoIterator<Item = &'a Node>) -> Vec<client::ProbeTarget<'a>> {
    nodes
        .into_iter()
        .enumerate()
        .map(|(i, n)| client::ProbeTarget {
            node: n,
            listen_port: 20800 + i as u16,
            user: format!("probe-user-{i}"),
            pass: format!("probe-pass-{i}"),
        })
        .collect()
}

/// 标签按下标生成：第 i 个目标的入站 `probe-in-<i>`、出站 `probe-<i>`，第 i 条规则把前者送进后者；
/// 入站之间、出站之间（含最后的 direct-out）都不重复，否则 sing-box 起不来。
fn assert_probe_tags(cfg: &serde_json::Value, n: usize) {
    let inbounds = cfg["inbounds"].as_array().unwrap();
    let outbounds = cfg["outbounds"].as_array().unwrap();
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(inbounds.len(), n);
    assert_eq!(outbounds.len(), n + 1);
    assert_eq!(rules.len(), n);
    for (i, ((inb, out), rule)) in inbounds.iter().zip(outbounds).zip(rules).enumerate() {
        assert_eq!(inb["tag"], format!("probe-in-{i}"));
        assert_eq!(out["tag"], format!("probe-{i}"));
        assert_eq!(
            rule,
            &serde_json::json!({
                "inbound": [format!("probe-in-{i}")],
                "outbound": format!("probe-{i}"),
            })
        );
    }
    for (what, list) in [("入站", inbounds), ("出站", outbounds)] {
        let mut tags: Vec<&str> = list.iter().map(|x| x["tag"].as_str().unwrap()).collect();
        let total = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), total, "{what} tag 互不相同");
    }
}

#[test]
fn probe_config_routes_each_inbound_to_its_node_with_auth_and_auto_detect() {
    let nodes = [hy2_direct_node(), hy2_resi_node(), reality_direct_node()];
    let targets = probe_targets(&nodes);
    let cfg = client::probe_config(&targets);

    // 顶层只有这五块：没有 experimental（cache_file 在它下面）
    let keys: Vec<&str> = cfg
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["dns", "inbounds", "log", "outbounds", "route"]);
    assert_eq!(cfg["log"]["level"], "error");
    // DNS 只有 local-dns：node_outbound 的 domain_resolver 写死引用它
    assert_eq!(
        cfg["dns"]["servers"],
        serde_json::json!([{ "tag": "local-dns", "type": "udp", "server": "223.5.5.5" }])
    );

    // 标签由 probe_config 按下标生成（调用方不给）：probe-in-<i> → probe-<i>，互不重复
    assert_probe_tags(&cfg, targets.len());
    let inbounds = cfg["inbounds"].as_array().unwrap();
    let outbounds = cfg["outbounds"].as_array().unwrap();
    assert_eq!(
        outbounds.last().unwrap(),
        &serde_json::json!({ "type": "direct", "tag": "direct-out" })
    );

    for (i, t) in targets.iter().enumerate() {
        // 调用方按 listen_port 认目标：127.0.0.1 上的 socks，带这个目标自己的凭据
        let inb = inbounds
            .iter()
            .find(|x| x["listen_port"] == t.listen_port)
            .expect("每个目标一个入站");
        assert_eq!(inb["tag"], format!("probe-in-{i}"));
        assert_eq!(inb["type"], "socks");
        assert_eq!(inb["listen"], "127.0.0.1");
        assert_eq!(
            inb["users"],
            serde_json::json!([{ "username": t.user, "password": t.pass }])
        );
        // 这个入站的规则指向 probe-<i>（assert_probe_tags 已核对），它就是这个节点的出站
        let out = &outbounds[i];
        assert_eq!(out, &client::node_outbound(t.node, &format!("probe-{i}")));
        assert_eq!(out["server_port"], t.node.port);
        assert_eq!(out["domain_resolver"], "local-dns");
    }

    // 兜底直连，并且自己选网卡：不设 auto_detect_interface 会被本机正在跑的 TUN 截走
    assert_eq!(cfg["route"]["final"], "direct-out");
    assert_eq!(cfg["route"]["auto_detect_interface"], true);
    assert_eq!(cfg["route"]["default_domain_resolver"], "local-dns");

    let s = serde_json::to_string(&cfg).unwrap();
    for banned in [
        "\"tun\"",
        "cache_file",
        "rule_set",
        "download_detour",
        "hijack-dns",
        "sniff",
        "proxy-out",
    ] {
        assert!(!s.contains(banned), "测速配置里不该有 {banned}");
    }

    // 主配置的出站就是 node_outbound(node, "proxy-out")
    for n in &nodes {
        let main = client::tun_config(n, &copts(ClientMode::Tun, false, split(false)));
        assert_eq!(main["outbounds"][0], client::node_outbound(n, "proxy-out"));
    }
}

#[test]
fn probe_config_tags_stay_unique_for_same_named_nodes() {
    // 同一个节点测两次、另一台服务器上同名的节点、label 恰好是 direct-out 的节点：
    // 调用方自带 tag（拿节点名当 tag）时这些都会撞 tag，sing-box 整轮起不来
    let a = hy2_direct_node();
    let b = Node {
        host: "b.example.com".into(),
        ..hy2_direct_node()
    };
    let c = Node {
        label: "direct-out".into(),
        ..hy2_direct_node()
    };
    assert_eq!(a.label, b.label);
    let targets = probe_targets([&a, &a, &b, &c]);
    let cfg = client::probe_config(&targets);
    assert_probe_tags(&cfg, targets.len());
    let outbounds = cfg["outbounds"].as_array().unwrap();
    for (i, (t, out)) in targets.iter().zip(outbounds).enumerate() {
        assert_eq!(out, &client::node_outbound(t.node, &format!("probe-{i}")));
    }
    assert_eq!(outbounds[2]["server"], "b.example.com");
}

#[test]
fn probe_config_passes_sing_box_check() {
    // 四种节点（HY2 直连带跳跃与 obfs），单个目标与四个目标各过一遍；
    // 再过一遍同名的：四种节点各测两次，外加另一台服务器上 label 相同的四个节点。
    // PATH 上的 sing-box / sing-box-1.12 / -1.13 / -1.14 缺哪个跳哪个
    let s = common::state("obfs");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    assert_eq!(nodes.len(), 4, "alice 是 fusion + 住宅");
    let targets = probe_targets(&nodes);
    for t in &targets {
        common::check_singbox_all(&client::probe_config(std::slice::from_ref(t)));
    }
    common::check_singbox_all(&client::probe_config(&targets));

    let twins: Vec<Node> = nodes
        .iter()
        .map(|n| Node {
            host: "b.example.com".into(),
            ..n.clone()
        })
        .collect();
    let same_named = probe_targets(nodes.iter().chain(&nodes).chain(&twins));
    let cfg = client::probe_config(&same_named);
    // 先过内核再核对标签：标签重复时 sing-box 自己就会报出来
    common::check_singbox_all(&cfg);
    assert_probe_tags(&cfg, same_named.len());
}

const GOLDEN_TUN_HY2_OBFS_V6: &str = r#"{"dns":{"final":"proxy-dns","rules":[{"domain_suffix":[".qq.com",".wechat.com",".tencent.com",".myqcloud.com",".xiaohongshu.com",".douyin.com",".bytedance.com",".toutiao.com",".kuaishou.com",".bilibili.com",".taobao.com",".alibaba.com",".alipay.com",".aliyuncs.com",".tmall.com",".jd.com",".baidu.com",".cn"],"server":"local-dns"}],"servers":[{"detour":"proxy-out","server":"1.1.1.1","tag":"proxy-dns","type":"https"},{"server":"223.5.5.5","tag":"local-dns","type":"udp"}],"strategy":"ipv4_only"},"inbounds":[{"address":["172.19.0.1/30","fdfe:dcba:9876::1/126"],"auto_route":true,"interface_name":"bui-tun","stack":"mixed","strict_route":true,"tag":"tun-in","type":"tun"},{"listen":"127.0.0.1","listen_port":1080,"tag":"socks-in","type":"mixed"},{"listen":"127.0.0.1","listen_port":8080,"tag":"http-in","type":"mixed"}],"log":{"level":"info"},"outbounds":[{"domain_resolver":"local-dns","hop_interval":"30s","obfs":{"password":"obfs-pw","type":"salamander"},"password":"alice:hy2-pw","server":"panel.example.com","server_port":10000,"server_ports":["20000:30000"],"tag":"proxy-out","tls":{"enabled":true,"insecure":false,"server_name":"panel.example.com"},"type":"hysteria2"},{"tag":"direct-out","type":"direct"}],"route":{"auto_detect_interface":true,"default_domain_resolver":"local-dns","final":"proxy-out","rules":[{"outbound":"direct-out","process_name":["cloudflared"]},{"network":"udp","outbound":"direct-out","port":[7844]},{"action":"sniff"},{"action":"hijack-dns","protocol":"dns"},{"outbound":"direct-out","port":[22,2222]},{"outbound":"direct-out","source_port":[22,2222]},{"ip_is_private":true,"outbound":"direct-out"},{"action":"reject","ip_version":6},{"domain_suffix":[".qq.com",".qpic.cn",".qlogo.cn",".wechat.com",".weixin.qq.com",".wx.qq.com",".tencent.com",".tencent-cloud.net",".myqcloud.com",".gtimg.com",".xiaohongshu.com",".xhscdn.com",".douyin.com",".douyincdn.com",".amemv.com",".bytedance.com",".bytecdntp.com",".toutiao.com",".iesdouyin.com",".kuaishou.com",".ksapisrv.com",".bilibili.com",".bilivideo.com",".hdslb.com",".taobao.com",".tbcdn.cn",".alicdn.com",".aliyuncs.com",".alipay.com",".alipayobjects.com",".alibaba.com",".alibabacloud.com",".tmall.com",".tmall.hk",".jd.com",".360buyimg.com",".jdcdn.com",".baidu.com",".bdstatic.com",".bdimg.com",".cn"],"outbound":"direct-out"},{"domain_keyword":["akamai","akamaized","akamaihd","edgekey","edgesuite","fastly","cloudfront"],"outbound":"proxy-out"},{"domain_keyword":["github","google","googleapis","googlevideo","gstatic","gmail","gemini","generativelanguage","anthropic","openai","chatgpt","antigravity","cloudcode","visualstudio","vscode"],"outbound":"proxy-out"},{"domain_suffix":[".github.com",".github.io",".githubusercontent.com",".githubassets.com",".google.com",".google.com.hk",".google.co.jp",".goog",".youtube.com",".ytimg.com",".googlesyndication.com",".googleusercontent.com",".ggpht.com",".gemini.google.com",".antigravity.google",".antigravity-unleash.goog",".run.app",".visualstudio.com",".vscode-cdn.net",".aka.ms"],"outbound":"proxy-out"}]}}"#;
const GOLDEN_TUN_HY2_RESI_SPLIT: &str = r#"{"dns":{"final":"proxy-dns","rules":[{"domain_suffix":[".qq.com",".wechat.com",".tencent.com",".myqcloud.com",".xiaohongshu.com",".douyin.com",".bytedance.com",".toutiao.com",".kuaishou.com",".bilibili.com",".taobao.com",".alibaba.com",".alipay.com",".aliyuncs.com",".tmall.com",".jd.com",".baidu.com",".cn"],"server":"local-dns"}],"servers":[{"detour":"proxy-out","server":"1.1.1.1","tag":"proxy-dns","type":"https"},{"server":"223.5.5.5","tag":"local-dns","type":"udp"}],"strategy":"ipv4_only"},"inbounds":[{"address":["172.19.0.1/30"],"auto_route":true,"interface_name":"bui-tun","stack":"mixed","strict_route":true,"tag":"tun-in","type":"tun"},{"listen":"127.0.0.1","listen_port":1080,"tag":"socks-in","type":"mixed"},{"listen":"127.0.0.1","listen_port":8080,"tag":"http-in","type":"mixed"}],"log":{"level":"info"},"outbounds":[{"domain_resolver":"local-dns","hop_interval":"30s","password":"alice:hy2-pw","server":"panel.example.com","server_port":40000,"server_ports":["41000:50000"],"tag":"proxy-out","tls":{"enabled":true,"insecure":false,"server_name":"panel.example.com"},"type":"hysteria2"},{"tag":"direct-out","type":"direct"}],"route":{"auto_detect_interface":true,"default_domain_resolver":"local-dns","final":"proxy-out","rules":[{"outbound":"direct-out","process_name":["cloudflared"]},{"network":"udp","outbound":"direct-out","port":[7844]},{"action":"sniff"},{"action":"hijack-dns","protocol":"dns"},{"outbound":"direct-out","port":[22,2222]},{"outbound":"direct-out","source_port":[22,2222]},{"ip_is_private":true,"outbound":"direct-out"},{"action":"reject","ip_version":6},{"domain_keyword":["openai.com","anthropic.com"],"outbound":"proxy-out"},{"domain_suffix":[".qq.com",".qpic.cn",".qlogo.cn",".wechat.com",".weixin.qq.com",".wx.qq.com",".tencent.com",".tencent-cloud.net",".myqcloud.com",".gtimg.com",".xiaohongshu.com",".xhscdn.com",".douyin.com",".douyincdn.com",".amemv.com",".bytedance.com",".bytecdntp.com",".toutiao.com",".iesdouyin.com",".kuaishou.com",".ksapisrv.com",".bilibili.com",".bilivideo.com",".hdslb.com",".taobao.com",".tbcdn.cn",".alicdn.com",".aliyuncs.com",".alipay.com",".alipayobjects.com",".alibaba.com",".alibabacloud.com",".tmall.com",".tmall.hk",".jd.com",".360buyimg.com",".jdcdn.com",".baidu.com",".bdstatic.com",".bdimg.com",".cn"],"outbound":"direct-out"},{"domain_keyword":["akamai","akamaized","akamaihd","edgekey","edgesuite","fastly","cloudfront"],"outbound":"proxy-out"},{"domain_keyword":["github","google","googleapis","googlevideo","gstatic","gmail","gemini","generativelanguage","anthropic","openai","chatgpt","antigravity","cloudcode","visualstudio","vscode"],"outbound":"proxy-out"},{"domain_suffix":[".github.com",".github.io",".githubusercontent.com",".githubassets.com",".google.com",".google.com.hk",".google.co.jp",".goog",".youtube.com",".ytimg.com",".googlesyndication.com",".googleusercontent.com",".ggpht.com",".gemini.google.com",".antigravity.google",".antigravity-unleash.goog",".run.app",".visualstudio.com",".vscode-cdn.net",".aka.ms"],"outbound":"proxy-out"}]}}"#;
const GOLDEN_MIXED_REALITY: &str = r#"{"dns":{"final":"proxy-dns","rules":[{"domain_suffix":[".qq.com",".wechat.com",".tencent.com",".myqcloud.com",".xiaohongshu.com",".douyin.com",".bytedance.com",".toutiao.com",".kuaishou.com",".bilibili.com",".taobao.com",".alibaba.com",".alipay.com",".aliyuncs.com",".tmall.com",".jd.com",".baidu.com",".cn"],"server":"local-dns"}],"servers":[{"detour":"proxy-out","server":"1.1.1.1","tag":"proxy-dns","type":"https"},{"server":"223.5.5.5","tag":"local-dns","type":"udp"}],"strategy":"ipv4_only"},"inbounds":[{"listen":"127.0.0.1","listen_port":1080,"tag":"socks-in","type":"mixed"},{"listen":"127.0.0.1","listen_port":8080,"tag":"http-in","type":"mixed"}],"log":{"level":"info"},"outbounds":[{"domain_resolver":"local-dns","flow":"xtls-rprx-vision","server":"panel.example.com","server_port":10001,"tag":"proxy-out","tls":{"enabled":true,"reality":{"enabled":true,"public_key":"PUB","short_id":"0123456789abcdef"},"server_name":"www.bing.com","utls":{"enabled":true,"fingerprint":"chrome"}},"type":"vless","uuid":"11111111-1111-4111-8111-111111111111"},{"tag":"direct-out","type":"direct"}],"route":{"auto_detect_interface":true,"default_domain_resolver":"local-dns","final":"proxy-out","rules":[{"outbound":"direct-out","process_name":["cloudflared"]},{"network":"udp","outbound":"direct-out","port":[7844]},{"action":"sniff"},{"outbound":"direct-out","port":[22,2222]},{"outbound":"direct-out","source_port":[22,2222]},{"ip_is_private":true,"outbound":"direct-out"},{"action":"reject","ip_version":6},{"domain_suffix":[".qq.com",".qpic.cn",".qlogo.cn",".wechat.com",".weixin.qq.com",".wx.qq.com",".tencent.com",".tencent-cloud.net",".myqcloud.com",".gtimg.com",".xiaohongshu.com",".xhscdn.com",".douyin.com",".douyincdn.com",".amemv.com",".bytedance.com",".bytecdntp.com",".toutiao.com",".iesdouyin.com",".kuaishou.com",".ksapisrv.com",".bilibili.com",".bilivideo.com",".hdslb.com",".taobao.com",".tbcdn.cn",".alicdn.com",".aliyuncs.com",".alipay.com",".alipayobjects.com",".alibaba.com",".alibabacloud.com",".tmall.com",".tmall.hk",".jd.com",".360buyimg.com",".jdcdn.com",".baidu.com",".bdstatic.com",".bdimg.com",".cn"],"outbound":"direct-out"},{"domain_keyword":["akamai","akamaized","akamaihd","edgekey","edgesuite","fastly","cloudfront"],"outbound":"proxy-out"},{"domain_keyword":["github","google","googleapis","googlevideo","gstatic","gmail","gemini","generativelanguage","anthropic","openai","chatgpt","antigravity","cloudcode","visualstudio","vscode"],"outbound":"proxy-out"},{"domain_suffix":[".github.com",".github.io",".githubusercontent.com",".githubassets.com",".google.com",".google.com.hk",".google.co.jp",".goog",".youtube.com",".ytimg.com",".googlesyndication.com",".googleusercontent.com",".ggpht.com",".gemini.google.com",".antigravity.google",".antigravity-unleash.goog",".run.app",".visualstudio.com",".vscode-cdn.net",".aka.ms"],"outbound":"proxy-out"}]}}"#;
const GOLDEN_MIXED_HY2_NO_HOP: &str = r#"{"dns":{"final":"proxy-dns","rules":[{"domain_suffix":[".qq.com",".wechat.com",".tencent.com",".myqcloud.com",".xiaohongshu.com",".douyin.com",".bytedance.com",".toutiao.com",".kuaishou.com",".bilibili.com",".taobao.com",".alibaba.com",".alipay.com",".aliyuncs.com",".tmall.com",".jd.com",".baidu.com",".cn"],"server":"local-dns"}],"servers":[{"detour":"proxy-out","server":"1.1.1.1","tag":"proxy-dns","type":"https"},{"server":"223.5.5.5","tag":"local-dns","type":"udp"}],"strategy":"ipv4_only"},"inbounds":[{"listen":"127.0.0.1","listen_port":1080,"tag":"socks-in","type":"mixed"},{"listen":"127.0.0.1","listen_port":8080,"tag":"http-in","type":"mixed"}],"log":{"level":"info"},"outbounds":[{"domain_resolver":"local-dns","password":"alice:hy2-pw","server":"panel.example.com","server_port":10000,"tag":"proxy-out","tls":{"enabled":true,"insecure":false,"server_name":"panel.example.com"},"type":"hysteria2"},{"tag":"direct-out","type":"direct"}],"route":{"auto_detect_interface":true,"default_domain_resolver":"local-dns","final":"proxy-out","rules":[{"outbound":"direct-out","process_name":["cloudflared"]},{"network":"udp","outbound":"direct-out","port":[7844]},{"action":"sniff"},{"outbound":"direct-out","port":[22,2222]},{"outbound":"direct-out","source_port":[22,2222]},{"ip_is_private":true,"outbound":"direct-out"},{"action":"reject","ip_version":6},{"domain_suffix":[".qq.com",".qpic.cn",".qlogo.cn",".wechat.com",".weixin.qq.com",".wx.qq.com",".tencent.com",".tencent-cloud.net",".myqcloud.com",".gtimg.com",".xiaohongshu.com",".xhscdn.com",".douyin.com",".douyincdn.com",".amemv.com",".bytedance.com",".bytecdntp.com",".toutiao.com",".iesdouyin.com",".kuaishou.com",".ksapisrv.com",".bilibili.com",".bilivideo.com",".hdslb.com",".taobao.com",".tbcdn.cn",".alicdn.com",".aliyuncs.com",".alipay.com",".alipayobjects.com",".alibaba.com",".alibabacloud.com",".tmall.com",".tmall.hk",".jd.com",".360buyimg.com",".jdcdn.com",".baidu.com",".bdstatic.com",".bdimg.com",".cn"],"outbound":"direct-out"},{"domain_keyword":["akamai","akamaized","akamaihd","edgekey","edgesuite","fastly","cloudfront"],"outbound":"proxy-out"},{"domain_keyword":["github","google","googleapis","googlevideo","gstatic","gmail","gemini","generativelanguage","anthropic","openai","chatgpt","antigravity","cloudcode","visualstudio","vscode"],"outbound":"proxy-out"},{"domain_suffix":[".github.com",".github.io",".githubusercontent.com",".githubassets.com",".google.com",".google.com.hk",".google.co.jp",".goog",".youtube.com",".ytimg.com",".googlesyndication.com",".googleusercontent.com",".ggpht.com",".gemini.google.com",".antigravity.google",".antigravity-unleash.goog",".run.app",".visualstudio.com",".vscode-cdn.net",".aka.ms"],"outbound":"proxy-out"}]}}"#;
