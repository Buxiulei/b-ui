//! 住宅 HY2 的 sing-box 单入站配置 `hy2-residential.json` 的**唯一**渲染器
//! （spec §2.3 形状、§3.2 门契约）。
//!
//! 取代 v4 的「每槽一个 apernet hysteria 实例」：一个 hysteria2 入站 `:40000`
//! （整段 `41000-50000` 由 [`crate::render::nft`] 的表 REDIRECT 过来，因为 sing-box 的
//! `ListenOptions` 只有单个 `listen_port`、没有端口范围字段 ——
//! <https://sing-box.sagernet.org/configuration/shared/listen/>）、`users[]` 是静态凭据池
//! （[`crate::hy2pool`]）、每条凭据一个 `gate-<id>` selector 门、`auth_user` 路由到门。
//!
//! **文件内容与用户无关**：只含凭据池、8 个槽出站、门、规则、两个回环端点、obfs、
//! 证书路径与监听端口。用户的任何生命周期动作只经 Clash API 切门，一个字节都不许改这里
//! （spec §3.5，守门测试在 `bui` 的 `core_files.rs`）。所以本文件**不用 `restart_key`**。
//!
//! 字段出处：
//! - `users[].name` / `users[].password`、`obfs`、`masquerade`、`ignore_client_bandwidth`、
//!   `tls` 必填 —— <https://sing-box.sagernet.org/configuration/inbound/hysteria2/> 。
//!   `bbr_profile`（1.14 起，缺省 `standard`）**不写**。
//! - selector 的 `outbounds` 必填、`default` 缺省取第一个、`interrupt_exist_connections` =
//!   "Interrupt existing connections when the selected outbound has changed" ——
//!   <https://sing-box.sagernet.org/configuration/outbound/selector/> 。
//! - `v2ray_api.stats.users` = "User list to count traffic"，且 "V2Ray API is not included
//!   by default"（所以这份配置只在自建二进制上跑）——
//!   <https://sing-box.sagernet.org/configuration/experimental/v2ray-api/> 。
//!
//! 与今天 apernet 配置（[`crate::render::hysteria`] 的 `common_doc`）的结构性差异：
//! `sniGuard` / `resolver` / `trafficStats` / `auth` / `acl` / `quic` 在 sing-box 的入站
//! schema 里根本不存在 —— `trafficStats` 的位置由 `experimental.v2ray_api` 接，`auth` 由
//! `users[]` 接，**`sniff` 这一项没有对应物（见下）**，目标域名仍不在本机解析（**不要
//! `dns` 段**），原样经 socks 交给 relay。
//!
//! # 为什么这份配置不 sniff（`route.rules` 里没有 `{"action":"sniff"}`）
//!
//! 曾经有过一条裸 `{"action":"sniff"}` 在 `rules[0]`。**它只会给每条 TCP 流加满一个
//! 嗅探超时（缺省 300 ms），而且永远嗅不到东西** —— 成因是 Hysteria2 协议与 sing-box
//! 实现的次序死锁：
//!
//! - apernet 客户端（v2rayN 里那个、`hysteria` CLI）缺省 `fastOpen: false`，
//!   要等服务端回 `TCPResponse` 之后才发首包；
//! - 而 sing-box 是在 **route（sniff 动作在 `route/route.go` 的规则匹配里跑）走完、
//!   出站也拨通之后**才 `N.ReportConnHandshakeSuccess`（`route/conn.go:122`），
//!   那一步才触达 sing-quic 的 `hysteria2.serverConn.HandshakeSuccess`
//!   （`service.go:403`）把 `TCPResponse` 写回去。
//!
//! 于是 sniff 在等客户端首包、客户端在等服务端的 `TCPResponse`，双方对等到嗅探超时
//! 到点为止：嗅探必然空手而归，代价是每条流白等一个超时。T3 2026-09-18 的五格实测
//! （25 次/格，`scratchpad/t3/item-300ms/results/matrix.txt`）：
//!
//! | 配置 | apernet 客户端（fastOpen 关）TTFB 中位数 | 嗅到域名 |
//! |---|---|---|
//! | `{"action":"sniff"}`（缺省 300 ms） | 307.1 ms | 0 / 25 |
//! | `{"action":"sniff","timeout":"50ms"}` | 56.6 ms | 0 / 25 |
//! | `{"action":"sniff","timeout":"1s"}` | 1010.6 ms | 0 / 25 |
//! | **不 sniff（现状）** | **5.6 ms** | 0 / 25 |
//!
//! 即：延迟 ≈ 超时值，缩短超时只是缩短白等；嗅探成功率恒为 0，无论超时多大。
//! （sing-box 当客户端时先发首包、不受影响：同一格 5.9 ms、25/25 嗅到域名；
//! 但**服务端不能假设客户端是谁**——v2rayN 用户走的就是 apernet 客户端那条路。
//! 对照格 E：apernet **服务端** + `sniff.rewriteDomain` 两种客户端都是 5 ms 级，
//! 所以这不是协议本身的代价，是 sing-box 这个实现的次序。）
//!
//! 删掉它不影响任何功能：
//!
//! - **分流**：住宅路径的 split 关键字匹配靠的是 relay 自己那条 `rules[0]` 无过滤
//!   sniff（[`crate::render::relay`]，`kernel_relay.rs` 有守门断言）。sing-box 1.14 的
//!   sniff 动作**不改写连接目标**，这一侧嗅到的域名原本就传不到 relay；relay 会在自己
//!   那一跳重新嗅一次，`domain_keyword` 优先匹配 `metadata.Domain`。T3 item12 的对照
//!   实验已经证明：摘掉 relay 那条会静默直连，摘掉这一条不会。
//! - **面板 / CLI**：`/connections` 的归户只读 `rule` / `chains` / `id`
//!   （`bui` 的 `panel::hy2resi::Hy2ResiConn`），`metadata.host` 一处都没读；失去它
//!   不改变任何显示。`inbound connection to …` 那条日志行在 route 之前打，照旧。
use crate::model::{Hy2Pool, NodeParams};
use crate::paths::Paths;
use crate::slots::{MAX_SLOTS, RELAY_SOCKS_BASE};
use serde_json::{json, Value};

/// 住宅 HY2 实例的 Clash API（relay 是 9091）。**只监听回环、不设 secret**。
pub const HY2_RESI_CLASH_API: &str = "127.0.0.1:9092";
/// 住宅 HY2 实例的 v2ray_api（Xray 的 api 是 10085）。**只监听回环、不设 secret**。
pub const HY2_RESI_V2RAY_API: &str = "127.0.0.1:10086";
/// 唯一的 hysteria2 入站 tag。
pub const INBOUND_TAG: &str = "hy2-resi";
/// 拒绝出站的 tag：门的 `default`、`route.final` 都指它。
pub const DENY_TAG: &str = "deny";

/// `deny` 出站拨的回环端口：本机永不监听 ⇒ 每条流当场 connection refused。门 selector 上
/// 的拨号失败靠这个端口把 deny 噪音（到期 / 封禁用户的正常拒绝）和「拨不通 relay 的槽入站」
/// 分开（`crate::modules::sentinel` 无法从 `gate-<id>` selector tag 看出选中的是 deny 还是槽，
/// 只能看 `dial tcp 127.0.0.1:<port>` 里的这个端口）。**唯一一处**定义。
pub const DENY_DIAL_PORT: u16 = 1;

/// 凭据 `id` 的门（selector）tag。
pub fn gate_tag(cred_id: &str) -> String {
    format!("gate-{cred_id}")
}

/// 槽 `index` 的出站 tag（目标是 relay 的 `slot-<index>` 入站）。
pub fn slot_out_tag(index: u16) -> String {
    format!("slot-{index}-out")
}

/// 渲染 `hy2-residential.json`。
///
/// 8 个槽出站**一律预声明**（与 [`MAX_SLOTS`] 同源）：槽不存在时它只是拨不通的回环端口，
/// 于是住宅 IP 池增删不改本文件。`deny` 是指向 `127.0.0.1:1` 的 socks 出站而不是 `block`
/// （`block` 出站 1.11 弃用、1.13 删除，而门的成员必须是**出站**）；端口 1 本机永不监听，
/// 每条流当场 connection refused。
///
/// **不开 `cache_file`**（spec §14 裁决 1）：门是授权决定，「陈旧但放行」比「短暂拒绝」更糟。
/// sing-box 重启后每个门回到 `default` = `deny`，由 `bui` 重放真实门位（fail-closed）。
pub fn config(node: &NodeParams, paths: &Paths, pool: &Hy2Pool) -> Value {
    let certs = paths.certs_dir.display();
    let mut inbound = json!({
        "type": "hysteria2",
        "tag": INBOUND_TAG,
        "listen": "::",
        "listen_port": node.ports.hy2_resi,
        "ignore_client_bandwidth": true,
        "users": pool.creds.iter().map(|c| json!({
            // 客户端发的整个 auth 串就是 `名:密码`（旧订阅兼容不变量 ①，spec §7.5）
            "name": c.name,
            "password": format!("{}:{}", c.name, c.secret)
        })).collect::<Vec<Value>>(),
        "masquerade": {
            "type": "proxy",
            // 伪装域与直连实例同源（`Reality::sni`），auth 不命中的握手落到它
            "url": format!("https://{}", node.reality.sni()),
            "rewrite_host": true
        },
        "tls": {
            "enabled": true,
            "certificate_path": format!("{certs}/fullchain.pem"),
            "key_path": format!("{certs}/privkey.pem")
        }
    });
    // 混淆覆盖全部 HY2 实例：订阅给住宅 HY2 节点同样带 obfs 参数，两边必须一致
    if node.obfs.enabled {
        inbound.as_object_mut().expect("json object").insert(
            "obfs".into(),
            json!({ "type": "salamander", "password": node.obfs.password }),
        );
    }

    // 成员表对全部门逐字相同：deny 在首位（`default` 也是它），其后 8 个槽出站
    let members: Vec<String> = std::iter::once(DENY_TAG.to_string())
        .chain((0..MAX_SLOTS).map(slot_out_tag))
        .collect();
    let mut outbounds = vec![socks_out(DENY_TAG, DENY_DIAL_PORT)];
    outbounds.extend((0..MAX_SLOTS).map(|i| socks_out(&slot_out_tag(i), RELAY_SOCKS_BASE + i)));
    outbounds.extend(pool.creds.iter().map(|c| {
        json!({
            "type": "selector",
            "tag": gate_tag(&c.id),
            "outbounds": members,
            "default": DENY_TAG,
            // 与 relay 的 slot-<i>-pool 固定 false 刻意相反：门只在用户自己的生命周期
            // 动作上切换（到期 / 封禁 / 换槽 / 踢），掐断既有流正是目的
            "interrupt_exist_connections": true
        })
    }));

    // 每凭据**一条**规则：`/connections` 的 `rule` 字段是唯一能把连接归到用户的线索
    // （spec §5.2），合并规则就丢了它。
    // **这张表里没有 `{"action":"sniff"}`**，理由见模块文档「为什么这份配置不 sniff」。
    let mut rules: Vec<Value> = Vec::with_capacity(pool.creds.len());
    rules.extend(
        pool.creds
            .iter()
            .map(|c| json!({ "auth_user": [&c.name], "outbound": gate_tag(&c.id) })),
    );

    json!({
        // level 不降到 warn：spec §6 那条带用户名的 inbound connection 行是排查线索
        "log": { "level": "info", "timestamp": true },
        "inbounds": [inbound],
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            // 理论上不可达：auth 串不命中时 hysteria2 入站走 masquerade，不进路由
            "final": DENY_TAG
        },
        "experimental": {
            "clash_api": { "external_controller": HY2_RESI_CLASH_API },
            "v2ray_api": {
                "listen": HY2_RESI_V2RAY_API,
                "stats": {
                    "enabled": true,
                    "users": pool.creds.iter().map(|c| &c.name).collect::<Vec<&String>>()
                }
            }
        }
    })
}

/// 一个指向本机回环端口的 socks5 出站（`deny` 与 8 个槽出站都是这个形状）。
fn socks_out(tag: &str, port: u16) -> Value {
    json!({
        "type": "socks",
        "tag": tag,
        "server": "127.0.0.1",
        "server_port": port,
        "version": "5"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use pretty_assertions::assert_eq;

    fn node() -> NodeParams {
        serde_json::from_str(
            r#"{"id":"8d5a1a1e-3b2c-4d1e-9f00-000000000001","name":"node-a","domain":"example.com",
            "public_ip":"203.0.113.10",
            "ports":{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],
                     "reality_direct":10001,"reality_resi":10002,"admin":8080},
            "reality":{"private_key":"a","public_key":"b","short_ids":["0123456789abcdef"],
                       "dest":"www.bing.com:443","server_names":["www.bing.com"]},
            "obfs":{"enabled":false,"password":""}}"#,
        )
        .unwrap()
    }

    fn pool() -> Hy2Pool {
        Hy2Pool {
            creds: vec![
                ReservedCred {
                    id: "r000".into(),
                    name: "alice".into(),
                    secret: "pw1".into(),
                    released_at: None,
                },
                ReservedCred {
                    id: "r001".into(),
                    name: "r001".into(),
                    secret: "s2".into(),
                    released_at: None,
                },
            ],
            generation: 3,
        }
    }

    /// `log` 段是哨兵判据的前提（spec §2.2 / §6、§8.1）：级别降到 `warn` 就吃掉
    /// `[<用户名>] inbound connection to <目标>` 那条排查线索，也吃掉哨兵认
    /// `outbound/socks[slot-<i>-out]` 拨号失败所需的上下文；`timestamp` 决定日志行的前缀形态，
    /// 而哨兵的夹具（`bui` 的 `sentinel::fixtures_hy2_resi`）就是按 `true` 采的。
    /// 谁把它改了，这条转红。
    #[test]
    fn the_log_section_stays_at_info_with_timestamps() {
        let v = config(&node(), &Paths::default_server(), &pool());
        assert_eq!(v["log"], json!({ "level": "info", "timestamp": true }));
    }

    /// 入站形状 = spec §2.3 里在 1.14.0 真机上过 check 的那一份
    #[test]
    fn the_inbound_is_the_shape_verified_on_the_tizi_sidecar() {
        let v = config(&node(), &Paths::default_server(), &pool());
        let i = &v["inbounds"][0];
        assert_eq!(i["type"], "hysteria2");
        assert_eq!(i["tag"], INBOUND_TAG);
        assert_eq!(i["listen"], "::");
        assert_eq!(i["listen_port"], 40000);
        assert_eq!(i["ignore_client_bandwidth"], true);
        // password 就是客户端发的整个 auth 串（本机 PoC：写 `p1` 握手 404、写 `r00:p1` 才通）
        assert_eq!(i["users"][0]["name"], "alice");
        assert_eq!(i["users"][0]["password"], "alice:pw1");
        assert_eq!(i["users"][1]["password"], "r001:s2");
        assert_eq!(i["masquerade"]["type"], "proxy");
        assert_eq!(i["masquerade"]["rewrite_host"], true);
        assert_eq!(i["tls"]["enabled"], true);
        assert_eq!(
            i["tls"]["certificate_path"],
            "/opt/b-ui/certs/fullchain.pem"
        );
        assert!(i.get("obfs").is_none(), "obfs 关着就一个字段都不写");
        // apernet 专属字段在 sing-box 的 schema 里根本不存在（配置模型的结构性差异）
        for k in [
            "sniGuard",
            "resolver",
            "trafficStats",
            "auth",
            "acl",
            "quic",
        ] {
            assert!(i.get(k).is_none() && v.get(k).is_none(), "{k} 不该出现");
        }
    }

    /// 门与出站：8 个槽预声明 + deny 指向 127.0.0.1:1 + default/interrupt 契约
    #[test]
    fn every_cred_gets_a_gate_whose_default_is_deny() {
        let v = config(&node(), &Paths::default_server(), &pool());
        let outs = v["outbounds"].as_array().unwrap();
        let deny = outs.iter().find(|o| o["tag"] == DENY_TAG).unwrap();
        assert_eq!(deny["type"], "socks");
        assert_eq!(deny["server"], "127.0.0.1");
        assert_eq!(
            deny["server_port"], 1,
            "本机永不监听端口 1 ⇒ 每条流 connection refused"
        );
        for i in 0..crate::slots::MAX_SLOTS {
            let t = slot_out_tag(i);
            let o = outs.iter().find(|o| o["tag"] == t.as_str()).unwrap();
            assert_eq!(
                o["server_port"],
                2080 + i,
                "槽出站端口 = RELAY_SOCKS_BASE + i"
            );
        }
        for id in ["r000", "r001"] {
            let g = outs
                .iter()
                .find(|o| o["tag"] == gate_tag(id).as_str())
                .unwrap();
            assert_eq!(g["type"], "selector");
            assert_eq!(
                g["default"], DENY_TAG,
                "到期语义的实现 + fail-closed 的地基"
            );
            assert_eq!(
                g["interrupt_exist_connections"], true,
                "与 relay 的 false 刻意相反"
            );
            assert_eq!(
                g["outbounds"].as_array().unwrap().len(),
                1 + usize::from(crate::slots::MAX_SLOTS)
            );
            assert_eq!(g["outbounds"][0], DENY_TAG);
        }
    }

    /// 每凭据一条 auth_user 规则（合并规则就丢了 /connections 里归组用的线索）
    #[test]
    fn routing_has_one_auth_user_rule_per_cred_and_finals_to_deny() {
        let v = config(&node(), &Paths::default_server(), &pool());
        let rules = v["route"]["rules"].as_array().unwrap();
        assert_eq!(
            rules[0],
            serde_json::json!({ "auth_user": ["alice"], "outbound": "gate-r000" }),
            "route 规则用传统 outbound 字段即可（tizi PoC 已验证）"
        );
        assert_eq!(rules[1]["auth_user"], serde_json::json!(["r001"]));
        assert_eq!(
            rules.len(),
            2,
            "每凭据一条，别的什么都没有 —— 特别是**不许**有 sniff（模块文档：\
             apernet 客户端等 TCPResponse、sing-box 在 route 后才写它 ⇒ sniff 必然\
             等满超时且嗅不到，每条 TCP 流白加 300 ms；分流靠 relay 那条 sniff）"
        );
        assert!(
            rules.iter().all(|r| r["action"] != "sniff"),
            "住宅 HY2 侧不许 sniff：{rules:#?}"
        );
        assert_eq!(v["route"]["final"], DENY_TAG);
        assert_eq!(
            v["experimental"]["clash_api"]["external_controller"],
            HY2_RESI_CLASH_API
        );
        assert_eq!(v["experimental"]["v2ray_api"]["listen"], HY2_RESI_V2RAY_API);
        assert_eq!(v["experimental"]["v2ray_api"]["stats"]["enabled"], true);
        assert_eq!(
            v["experimental"]["v2ray_api"]["stats"]["users"],
            serde_json::json!(["alice", "r001"]),
            "白名单 = 全部凭据的 name；不在表里的用户不计"
        );
        assert!(
            v["experimental"].get("cache_file").is_none(),
            "不开 cache_file（授权决定不许陈旧放行）"
        );
    }

    /// 文件内容与用户无关：只有 §3.5 的那五件事能改它
    #[test]
    fn obfs_and_the_pool_are_the_only_things_that_change_the_bytes() {
        let (n, p) = (node(), Paths::default_server());
        assert_eq!(config(&n, &p, &pool()), config(&n, &p, &pool()), "纯函数");
        let mut n2 = n.clone();
        n2.obfs = Obfs {
            enabled: true,
            password: "obfs-pw-test".into(),
        };
        let v = config(&n2, &p, &pool());
        assert_eq!(v["inbounds"][0]["obfs"]["type"], "salamander");
        assert_eq!(v["inbounds"][0]["obfs"]["password"], "obfs-pw-test");
        let mut grown = pool();
        grown.creds.push(ReservedCred {
            id: "r002".into(),
            name: "r002".into(),
            secret: "s3".into(),
            released_at: None,
        });
        assert_ne!(
            config(&n, &p, &grown),
            config(&n, &p, &pool()),
            "扩容改内容"
        );
    }
}
