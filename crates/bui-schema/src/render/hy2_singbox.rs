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
//! `users[]` 接，`sniff` 由 route 的 `sniff` action 接，目标域名仍不在本机解析（**不要
//! `dns` 段**），原样经 socks 交给 relay。
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
    let mut outbounds = vec![socks_out(DENY_TAG, 1)];
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
    // （spec §5.2），合并规则就丢了它
    let mut rules = vec![json!({ "action": "sniff" })];
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
        assert_eq!(rules[0], serde_json::json!({ "action": "sniff" }));
        assert_eq!(
            rules[1],
            serde_json::json!({ "auth_user": ["alice"], "outbound": "gate-r000" }),
            "route 规则用传统 outbound 字段即可（tizi PoC 已验证）"
        );
        assert_eq!(rules[2]["auth_user"], serde_json::json!(["r001"]));
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

    /// 文件内容与用户无关：只有 §3.5 的四件事能改它
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
