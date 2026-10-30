//! Xray 配置渲染：**单个** REALITY inbound（`tag: "vless"`，客户端端口恒 443），
//! 直连 / 住宅用 VLESS `vlessRoute`（UUID 第 6/7 字节）区分。
//!
//! 2026-10-30 裁决（REALITY 443 + self-steal）：`vless-direct` / `vless-residential` 两个
//! inbound 合并成一个，未认证连接回落到本机 Caddy（`dest = 127.0.0.1:9443`）；住宅用户
//! 的路由按 `user` + `vlessRoute: 1` 落到自己的槽，route 0 落到最终兜底 `direct`。
//!
//! 4.2.1 安全修订：**没有住宅权益的 Reality 用户也会有一条 `user + vlessRoute: 1 → block`
//! 的 per-user 规则**，通用 `resi-route` 也从「兜底槽」改成 `block` —— 非住宅用户自己把
//! UUID 第 6/7 字节改成 `0001` 只会被拒，不会借到住宅出口；新住宅用户的规则尚未经 gRPC
//! 落地时同样被拒（fail-closed 从「落到兜底槽」收紧为「block」）。

use crate::model::{NodeParams, Protocol, RealityFront, Residential, User, CADDY_LOCAL_PORT};
use crate::nodes::RESIDENTIAL_ROUTE;
use crate::paths::Paths;
use crate::slots;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 字面 IPv6 的 blackhole 出站 tag。路由首条把 `::/0` 送到它，服务端绝不新建 IPv6 出站。
/// 4.2.1 起它同时是「route 1 未授权」的落点（非住宅用户与通用兜底都指它）。
pub const BLOCK_TAG: &str = "block";

/// 唯一 REALITY inbound 的 tag（`XRAY_INBOUND_TAGS` 与用户同步共用同一个字面量）。
pub const INBOUND_TAG: &str = "vless";

/// 住宅槽路由规则的 `ruleTag` 前缀：gRPC 侧按它认「这条规则是槽位模块的」。
/// 改它等于把线上已经装载的旧规则变成认不出来的孤儿（收敛会把它们当多余项删掉，
/// 一轮之内自愈，但别没事乱改）。
pub const USER_RULE_PREFIX: &str = "resi-u-";
/// 通用 route-1 规则（4.2.1 起**拒绝**）：没有 `user` 条件、只匹配 `vlessRoute: 1`，
/// 排在全部用户规则之后、最终兜底之前。每个 Reality 用户都有 per-user 规则（住宅 → 自己的
/// 槽，非住宅 → `block`），所以这一条只兜「规则尚未经 gRPC 落地」的窗口——落点是
/// [`BLOCK_TAG`]，既不漏到直连、也不借住宅出口。**不能**以 [`USER_RULE_PREFIX`] 开头
/// （否则收敛会把线上规则当孤儿删掉）。
pub const RESI_ROUTE_RULE_TAG: &str = "resi-route";
/// 最终兜底规则的 `ruleTag`：无 `user`、无 `vlessRoute`，永远排在表尾（`direct`）。
pub const FALLBACK_RULE_TAG: &str = "resi-fallback";

/// 用户 `user_id`（= xray 侧的 email）对应的规则 tag。
pub fn user_rule_tag(user_id: uuid::Uuid) -> String {
    format!("{USER_RULE_PREFIX}{user_id}")
}

/// 一条住宅槽路由规则 —— **渲染（写 `xray-config.json`）与 gRPC 增删共用同一个形状**，
/// 两边不可能各算一份（D7）。`emails` 为空 = 不带 `user` 条件；`vless_route` 有值 =
/// 只匹配该 `vlessRoute`（proto 的 `vless_route_list`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRule {
    pub rule_tag: String,
    pub inbound_tag: String,
    pub emails: Vec<String>,
    pub vless_route: Option<u16>,
    pub outbound_tag: String,
}

/// API inbound 的本地端口（v3 固定值）。
const API_PORT: u16 = 10085;

/// 渲染整份 `xray-config.json`。
///
/// `users` 里只有开通了 [`Protocol::Reality`] 且未停用的用户进 `clients`。
///
/// `resi` 提供槽位表（spec §5.6）：每槽一个 `relay-slot-<i>` 出站（socks →
/// `127.0.0.1:(2080+i)`）。路由表（顺序即语义，spec §3.3）：
/// 1. 字面 IPv6 → blackhole；
/// 2. api inbound → api；
/// 3. 每个未停用、有 Reality 权益的用户一条：
///    有住宅权益 → `user + vlessRoute: 1 → relay-slot-<自己的槽>`；
///    没有住宅权益 → `user + vlessRoute: 1 → block`（4.2.1 安全修订）；
/// 4. `resi-route`：无 `user`，`vlessRoute: 1 → block`（规则尚未收敛的窗口，拒绝）；
/// 5. `resi-fallback`：无 `user`、无 route → `direct`（直连节点走这一条）。
///
/// `email` 就是 `user_id`（spec §3.3，gRPC 侧唯一键）。
///
/// 这份文件只在 xray **启动**时被读；运行中的增删由
/// `residential::slots::converge_xray` 走 `RoutingService` gRPC 完成（D7）。
///
/// **调用方必须只传入当前有效（未到期、未超限）的用户**：本函数只看
/// `disabled` 与权益，不做期限、流量配额判断；把过期或超限用户传进来
/// 就等于给他们签发可用的 REALITY 凭据。
///
/// 当前模板不需要磁盘路径，`_paths` 只为满足 C1 契约的统一签名。
pub fn config(node: &NodeParams, users: &[User], resi: &Residential, _paths: &Paths) -> Value {
    let clients = clients(users);

    // 服务端出站一律 IPv4（2026-10-27 裁决）：
    // - `ForceIPv4` / `UseIPv4` 只管域名解析，字面 `[::1]` 仍会被 freedom 拨出去；
    // - 所以路由首条把字面 IPv6（`::/0`）丢进 blackhole，`domainStrategy: AsIs`
    //   保证路由阶段不解析域名（域名交给 freedom 按 ForceIPv4 取 A，AAAA-only 域名失败）。
    // 住宅仍把目标交给原 SOCKS relay，不改变供应商路径与出口身份。
    let mut outbounds = vec![
        json!({"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "ForceIPv4"}}),
        json!({"tag": BLOCK_TAG, "protocol": "blackhole", "settings": {}}),
    ];
    for i in slots::indices(resi) {
        let res = slots::resources(i);
        outbounds.push(json!({
            "tag": relay_tag(i),
            "protocol": "socks",
            "settings": {"servers": [{"address": "127.0.0.1", "port": res.relay_port}]}
        }));
    }

    let mut rules = vec![
        // 字面 IPv6 一律 blackhole（见上面 outbounds 的注释）。必须在所有入站规则之前：
        // 不论流量来自哪条路由，都不允许从本机新建 IPv6 出站。
        json!({"type": "field", "ip": ["::/0"], "outboundTag": BLOCK_TAG}),
        json!({"type": "field", "inboundTag": ["api"], "outboundTag": "api"}),
    ];
    // 用户规则 + `resi-route` + `resi-fallback`（顺序即 `slot_rules` 的顺序）。带 `user`
    // 的那些**不进结构哈希**（[`structural_hash`]，D7），所以加用户不重启 xray。
    rules.extend(slot_rules(users, resi).iter().map(rule_json));

    json!({
        "log": {"loglevel": "warning"},
        "stats": {},
        // RoutingService 是 D7 的槽路由增删（AddRule / RemoveRule / ListRule）所必需，
        // 不声明就是 gRPC Unimplemented（D11）
        "api": {"tag": "api", "services": ["StatsService", "HandlerService", "RoutingService"]},
        "policy": {
            "levels": {"0": {"statsUserUplink": true, "statsUserDownlink": true}},
            "system": {"statsInboundUplink": true, "statsInboundDownlink": true}
        },
        "dns": {
            "servers": ["https+local://1.1.1.1/dns-query", "8.8.8.8"],
            "queryStrategy": "UseIPv4"
        },
        "inbounds": [
            {
                "tag": "api",
                "port": API_PORT,
                "listen": "127.0.0.1",
                "protocol": "dokodemo-door",
                "settings": {"address": "127.0.0.1"}
            },
            vless_inbound(node, &clients),
        ],
        "outbounds": outbounds,
        "routing": {"domainStrategy": "AsIs", "rules": rules}
    })
}

/// 期望态的槽规则表：**每个未停用、有 Reality 权益的用户一条**（按 `user_id` 升序），
/// **末尾两条通用规则**（`resi-route` → `block`，`resi-fallback` → `direct`）。
///
/// 渲染（写文件）与 gRPC 收敛（`residential::slots::converge_xray`）共用这一个函数，
/// 两边不可能各算出一份不同的表（D7）。
///
/// 有住宅权益 → 自己的槽；没有住宅权益 → `block`（4.2.1 安全修订：不再靠「没有规则」
/// 让它落到通用规则上，通用规则本身也改成 `block`）。
///
/// 被封（超限/到期）的用户**留着**他的规则：他的凭据已经被 `sync_users` 从 `clients`
/// 里摘掉了，规则匹配不到任何连接，留着能让「渲染 = runtime」这条不变量保持简单。
pub fn slot_rules(users: &[User], resi: &Residential) -> Vec<SlotRule> {
    let mut out: Vec<SlotRule> = slot_users(users)
        .into_iter()
        .map(|u| SlotRule {
            rule_tag: user_rule_tag(u.user_id),
            inbound_tag: INBOUND_TAG.to_string(),
            emails: vec![u.user_id.to_string()],
            vless_route: Some(RESIDENTIAL_ROUTE),
            outbound_tag: if u.entitlements.residential.is_some() {
                relay_tag(slots::index_of_user(u, resi))
            } else {
                BLOCK_TAG.to_string()
            },
        })
        .collect();
    // 通用 route-1 兜底（4.2.1）：落点从「兜底槽」改成 `block`。per-user 规则尚未经
    // gRPC 落地的窗口里 route 1 会被拒 —— 既不漏到直连，也不借住宅出口。
    out.push(SlotRule {
        rule_tag: RESI_ROUTE_RULE_TAG.to_string(),
        inbound_tag: INBOUND_TAG.to_string(),
        emails: vec![],
        vless_route: Some(RESIDENTIAL_ROUTE),
        outbound_tag: BLOCK_TAG.to_string(),
    });
    // 最终兜底：直连节点（route 0）落到 direct。
    out.push(SlotRule {
        rule_tag: FALLBACK_RULE_TAG.to_string(),
        inbound_tag: INBOUND_TAG.to_string(),
        emails: vec![],
        vless_route: None,
        outbound_tag: "direct".to_string(),
    });
    out
}

/// 一条 [`SlotRule`] 的 JSON 形态（`emails` 为空就不写 `user` 字段 —— 写成 `[]`
/// 会变成「匹配空用户列表」，内核语义与「不过滤」不同）。`vless_route` 有值时写字符串
/// `"vlessRoute": "<n>"`（与 proto 的 `vless_route_list` 同语义）。
fn rule_json(r: &SlotRule) -> Value {
    let mut v = json!({
        "type": "field",
        "ruleTag": r.rule_tag,
        "inboundTag": [r.inbound_tag],
        "outboundTag": r.outbound_tag
    });
    if !r.emails.is_empty() {
        v["user"] = json!(r.emails);
    }
    if let Some(route) = r.vless_route {
        v["vlessRoute"] = json!(route.to_string());
    }
    v
}

/// 槽 `i` 的住宅出站 tag。
fn relay_tag(i: u16) -> String {
    format!("relay-slot-{i}")
}

/// 会出现在槽规则里的用户：**未停用、有 Reality 权益**（不论有没有住宅权益）。
///
/// 4.2.1 安全修订：没有住宅权益的用户也必须有一条 `→ block` 的 route-1 规则，否则他可以
/// 自己把 UUID 第 6/7 字节改成 `0001`，直接落进通用 `resi-route`（旧语义是兜底住宅槽）。
/// 按 `user_id` 升序 —— 渲染必须确定性，顺序抖动会让文件与两个哈希无谓地变。
fn slot_users(users: &[User]) -> Vec<&User> {
    let mut v: Vec<&User> = users
        .iter()
        .filter(|u| !u.disabled && u.entitlements.protocols.contains(&Protocol::Reality))
        .collect();
    v.sort_unstable_by_key(|u| u.user_id);
    v
}

/// 配置的结构哈希：去掉每个 inbound 的 `settings.clients`、**以及 `routing.rules` 里
/// 带 `user` 字段的规则**后取 sha256。
///
/// 用户增删、用户改槽位、用户权益变化（住宅 ↔ 非住宅，per-user 规则在 relay 与 block
/// 之间切换）都只改这些 `user` 规则（走 gRPC 收敛），不该触发 xray 重启
/// （D7 / spec §3.3；重启会掐断全部 REALITY 连接）。
///
/// 4.2.1 起两条通用规则都是常量（`resi-route → block`、`resi-fallback → direct`），
/// 它们与槽位表无关；结构调整只剩「槽位表本身 + front/入站/出站」在哈希里。
pub fn structural_hash(cfg: &Value) -> String {
    let mut stripped = cfg.clone();
    if let Some(inbounds) = stripped.get_mut("inbounds").and_then(Value::as_array_mut) {
        for inbound in inbounds {
            if let Some(settings) = inbound.get_mut("settings").and_then(Value::as_object_mut) {
                settings.remove("clients");
            }
        }
    }
    if let Some(rules) = stripped
        .pointer_mut("/routing/rules")
        .and_then(Value::as_array_mut)
    {
        rules.retain(|r| r.get("user").is_none());
    }
    let bytes = serde_json::to_vec(&stripped).expect("Value 序列化不会失败");
    hex::encode(Sha256::digest(bytes))
}

/// 槽路由的哈希：只看 `routing.rules` 里带 `user` 字段的那些规则。
///
/// 它是「xray 进程里正在跑的那一份」与「期望的那一份」的比对键：
/// `runtime.residential.xray_slot_rules_hash` 记最近一次成功收敛的值，
/// `converge_xray`（D7）拿它做第 2 步的快速判等，拿磁盘上那份算出来的同一个哈希
/// 做第 4 步「文件落地了没」的判据。
pub fn slot_rules_hash(cfg: &Value) -> String {
    let rules: Vec<&Value> = cfg
        .pointer("/routing/rules")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|r| r.get("user").is_some()).collect())
        .unwrap_or_default();
    let bytes = serde_json::to_vec(&rules).expect("Value 序列化不会失败");
    hex::encode(Sha256::digest(bytes))
}

/// 有 Reality 权益且未停用的用户 → `clients` 条目。
///
/// 同上：不做期限/超限判断，调用方须只传入当前有效的用户。
fn clients(users: &[User]) -> Vec<Value> {
    users
        .iter()
        .filter(|u| !u.disabled && u.entitlements.protocols.contains(&Protocol::Reality))
        .map(|u| {
            json!({
                "id": u.credentials.vless_uuid,
                "flow": "xtls-rprx-vision",
                // spec §3.3：email 是 gRPC 侧唯一键（AddUser/RemoveUser/QueryStats 都用它），与 state 的 user_id 一一对应
                "email": u.user_id.to_string(),
            })
        })
        .collect()
}

/// 唯一的 REALITY inbound（self-steal）：未认证连接回落到本机 Caddy。
///
/// - `front = public`：监听 `0.0.0.0:443`；`front = l4`：监听 `127.0.0.1:10443`
///   （前置 L4 把公网 443 转进来）。
/// - `dest` 恒为 [`CADDY_LOCAL_PORT`]；`serverNames` = `[node.domain]`（订阅里客户端
///   用的 SNI 与它同源）；`xver: 1` 让 Xray 给 Caddy 发 PROXY protocol（Caddy 的
///   `listener_wrappers { proxy_protocol tls }` 接住，客户端真实 IP 不丢）。
/// - `sniffing.routeOnly: true`：只把嗅探结果用于路由，不重写目标的域名（spec §2.1 第 4 条）。
fn vless_inbound(node: &NodeParams, clients: &[Value]) -> Value {
    let (listen, port) = match node.reality.front {
        RealityFront::Public => ("0.0.0.0", crate::model::REALITY_PORT),
        RealityFront::L4 => ("127.0.0.1", crate::model::L4_XRAY_PORT),
    };
    json!({
        "tag": INBOUND_TAG,
        "listen": listen,
        "port": port,
        "protocol": "vless",
        "settings": {"clients": clients, "decryption": "none"},
        "streamSettings": {
            "network": "tcp",
            "security": "reality",
            "realitySettings": {
                "dest": format!("127.0.0.1:{CADDY_LOCAL_PORT}"),
                "serverNames": [node.domain],
                "privateKey": node.reality.private_key,
                "shortIds": node.reality.short_ids,
                "xver": 1
            }
        },
        "sniffing": {"enabled": true, "destOverride": ["http", "tls"], "routeOnly": true}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ResidentialEntitlement, ResidentialGroup, Slot, Upstream, UpstreamKind, DEFAULT_GROUP,
    };
    use crate::paths::Paths;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    #[test]
    fn rule_tags_are_stable_and_prefixed() {
        let id = Uuid::from_u128(0xaa);
        assert_eq!(
            user_rule_tag(id),
            format!("resi-u-{id}"),
            "gRPC 侧按前缀认「这条规则是我们的」，改前缀等于把旧规则变成孤儿"
        );
        assert!(user_rule_tag(id).starts_with(USER_RULE_PREFIX));
        assert_eq!(RESI_ROUTE_RULE_TAG, "resi-route");
        assert!(
            !RESI_ROUTE_RULE_TAG.starts_with(USER_RULE_PREFIX),
            "通用住宅兜底也不能以用户前缀开头（否则收敛会把它当孤儿删掉）"
        );
        assert_eq!(FALLBACK_RULE_TAG, "resi-fallback");
        assert!(
            !FALLBACK_RULE_TAG.starts_with(USER_RULE_PREFIX),
            "兜底 tag 不能被当成某个用户的规则（收敛时会把它当多余项删掉）"
        );
    }

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

    /// n 个槽（上游 uuid = index+1）。
    fn resi(n: u16) -> Residential {
        let mut r = Residential::default();
        let g = r.groups.get_mut(DEFAULT_GROUP).unwrap();
        *g = ResidentialGroup {
            enabled: n > 0,
            upstreams: (0..n)
                .map(|i| Upstream {
                    id: Uuid::from_u128(u128::from(i) + 1),
                    name: format!("url-{}", i + 1),
                    kind: UpstreamKind::Socks5,
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
                .collect(),
            ..Default::default()
        };
        r.slots = (0..n)
            .map(|i| Slot {
                index: i,
                upstream_id: Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        r
    }

    /// 一个有 Reality 权益的住宅用户，粘在 `slot` 指的那条上游上。
    fn user(n: u128, slot: Option<Uuid>) -> User {
        let mut u: User = serde_json::from_str(
            r#"{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"x","created_at":"2026-09-11T00:00:00Z",
            "credentials":{"hy2_password":"pw1","vless_uuid":"11111111-1111-4111-8111-111111111111"},
            "entitlements":{"protocols":["hysteria2","reality"],"direct":true,"residential":{"group_id":"default"}}}"#,
        )
        .unwrap();
        u.user_id = Uuid::from_u128(0x1000 + n);
        u.username = format!("u{n}");
        u.entitlements.residential = Some(ResidentialEntitlement {
            group_id: DEFAULT_GROUP.into(),
            slot_id: slot,
        });
        u
    }

    fn rules(cfg: &Value) -> Vec<Value> {
        cfg["routing"]["rules"].as_array().unwrap().clone()
    }

    /// 带 `user` 的那些规则：`(ruleTag, 出站, 第一个 email)`
    fn user_rules(cfg: &Value) -> Vec<(String, String, String)> {
        rules(cfg)
            .iter()
            .filter(|r| r.get("user").is_some())
            .map(|r| {
                (
                    r["ruleTag"].as_str().unwrap().to_string(),
                    r["outboundTag"].as_str().unwrap().to_string(),
                    r["user"][0].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn the_api_inbound_declares_routing_service() {
        let cfg = config(&node(), &[], &resi(1), &Paths::default_server());
        assert_eq!(
            cfg["api"]["services"],
            serde_json::json!(["StatsService", "HandlerService", "RoutingService"]),
            "少了 RoutingService，AddRule/RemoveRule/ListRule 就是 gRPC Unimplemented（D11）"
        );
    }

    /// 服务端出站一律 IPv4（2026-10-27）：字面 `::/0` 在路由首条进 blackhole，
    /// 且必须显式 `AsIs`（默认 `AsIs` 也好，但写死才不会被上游默认值带着走）。
    #[test]
    fn literal_ipv6_is_blackholed_before_every_other_route() {
        let cfg = config(&node(), &[], &resi(1), &Paths::default_server());
        assert_eq!(cfg["routing"]["domainStrategy"], "AsIs");
        assert_eq!(
            rules(&cfg)[0],
            serde_json::json!({"type": "field", "ip": ["::/0"], "outboundTag": "block"})
        );
        assert!(cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["tag"] == "block" && o["protocol"] == "blackhole"));
    }

    #[test]
    fn a_single_slot_routes_its_one_user_to_slot_zero() {
        let u = user(1, None);
        let cfg = config(
            &node(),
            std::slice::from_ref(&u),
            &resi(1),
            &Paths::default_server(),
        );
        let tags: Vec<String> = cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(tags, vec!["direct", "block", "relay-slot-0"]);
        assert_eq!(cfg["outbounds"][2]["settings"]["servers"][0]["port"], 2080);
        let r = rules(&cfg);
        assert_eq!(
            r.len(),
            5,
            "IPv6 block / api / 这个用户一条 / 通用住宅兜底 / 最终兜底"
        );
        // 未分配的用户也有自己的一条规则，指向兜底槽（D7：一人一条，追加顺序才无所谓）
        assert_eq!(
            user_rules(&cfg),
            vec![(
                format!("resi-u-{}", u.user_id),
                "relay-slot-0".to_string(),
                u.user_id.to_string()
            )]
        );
        // 通用 route-1 兜底（4.2.1 起 → block）排在用户规则之后、最终兜底之前：
        // 新用户的 per-user 规则还没经 gRPC 落地时，route 1 被拒（不漏直连、不借住宅）
        let route = &r[3];
        assert_eq!(route["ruleTag"], "resi-route");
        assert_eq!(route["outboundTag"], "block");
        assert_eq!(route["vlessRoute"], "1");
        assert!(route.get("user").is_none(), "通用 route-1 兜底不带 user");
        let last = r.last().unwrap();
        assert_eq!(last["ruleTag"], "resi-fallback");
        assert_eq!(
            last["outboundTag"], "direct",
            "route 0（直连节点）落到 direct"
        );
        assert_eq!(last["inboundTag"], serde_json::json!(["vless"]));
        assert!(last.get("user").is_none(), "最终兜底不带 user");
        assert!(last.get("vlessRoute").is_none(), "最终兜底不按 route 过滤");
    }

    #[test]
    fn every_slot_gets_an_outbound_and_every_user_gets_his_own_rule() {
        let r3 = resi(3);
        let users = vec![
            user(1, Some(Uuid::from_u128(1))), // 槽 0
            user(2, Some(Uuid::from_u128(2))), // 槽 1
            user(3, Some(Uuid::from_u128(3))), // 槽 2
            user(4, Some(Uuid::from_u128(2))), // 槽 1
            user(5, None),                     // 未分配 ⇒ 兜底槽（槽 0）
        ];
        let cfg = config(&node(), &users, &r3, &Paths::default_server());
        assert_eq!(
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| (
                    o["tag"].as_str().unwrap().to_string(),
                    o["settings"]["servers"][0]["port"].as_u64()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("direct".to_string(), None),
                ("block".to_string(), None),
                ("relay-slot-0".to_string(), Some(2080)),
                ("relay-slot-1".to_string(), Some(2081)),
                ("relay-slot-2".to_string(), Some(2082)),
            ]
        );
        // 一人一条，按 user_id 升序（渲染必须确定性：顺序抖动会让文件与哈希无谓地变）
        assert_eq!(
            user_rules(&cfg),
            users
                .iter()
                .map(|u| (
                    format!("resi-u-{}", u.user_id),
                    format!("relay-slot-{}", crate::slots::index_of_user(u, &r3)),
                    u.user_id.to_string()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            user_rules(&cfg)
                .iter()
                .map(|(_, o, _)| o.as_str())
                .collect::<Vec<_>>(),
            vec![
                "relay-slot-0",
                "relay-slot-1",
                "relay-slot-2",
                "relay-slot-1",
                "relay-slot-0"
            ]
        );
        // 兜底必须是最后一条（前面每个人的规则先匹配）
        assert_eq!(rules(&cfg).last().unwrap()["ruleTag"], "resi-fallback");
    }

    /// `slot_rules` 与渲染出的规则是同一份 —— 收敛拿它当期望态，渲染拿它写文件，
    /// 两边不可能各算一份（D7）。
    #[test]
    fn slot_rules_is_exactly_what_gets_rendered() {
        let r = resi(2);
        let users = vec![user(1, Some(Uuid::from_u128(2))), user(2, None)];
        let cfg = config(&node(), &users, &r, &Paths::default_server());
        let want = slot_rules(&users, &r);
        assert_eq!(want.len(), 4, "两个用户各一条 + resi-route + 最终兜底");
        assert_eq!(want[want.len() - 2].rule_tag, RESI_ROUTE_RULE_TAG);
        assert_eq!(want.last().unwrap().rule_tag, FALLBACK_RULE_TAG);
        assert!(want.last().unwrap().emails.is_empty());
        assert_eq!(want.last().unwrap().vless_route, None);
        let rendered: Vec<(String, String)> = rules(&cfg)
            .iter()
            .filter(|x| x.get("ruleTag").is_some())
            .map(|x| {
                (
                    x["ruleTag"].as_str().unwrap().to_string(),
                    x["outboundTag"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            rendered,
            want.iter()
                .map(|s| (s.rule_tag.clone(), s.outbound_tag.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_slot_with_no_users_keeps_its_outbound_but_gets_no_rule() {
        let cfg = config(
            &node(),
            &[user(1, Some(Uuid::from_u128(1)))],
            &resi(2),
            &Paths::default_server(),
        );
        assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 4);
        assert!(
            !rules(&cfg)
                .iter()
                .any(|r| r["outboundTag"] == "relay-slot-1"),
            "槽 1 没人 ⇒ 没有规则指向它（出站照留，xray 启动时要能解析）"
        );
    }

    /// 停用 / 没有 Reality 权益的用户不进槽规则（前两种在 `clients` 里也不存在）；
    /// **没有住宅权益的 Reality 用户仍要有一条 `→ block` 的 per-user 规则**（4.2.1）。
    #[test]
    fn disabled_and_non_reality_users_get_no_rule_but_direct_only_users_are_blocked() {
        let mut disabled = user(2, Some(Uuid::from_u128(2)));
        disabled.disabled = true;
        let mut hy2_only = user(3, Some(Uuid::from_u128(2)));
        hy2_only.entitlements.protocols = vec![Protocol::Hysteria2];
        let mut direct_only = user(4, None);
        direct_only.entitlements.residential = None;
        let direct_id = direct_only.user_id;
        let cfg = config(
            &node(),
            &[disabled, hy2_only, direct_only],
            &resi(2),
            &Paths::default_server(),
        );
        assert_eq!(
            user_rules(&cfg),
            vec![(
                user_rule_tag(direct_id),
                "block".to_string(),
                direct_id.to_string()
            )],
            "非住宅 Reality 用户的 route 1 必须被拒"
        );
        assert_eq!(
            rules(&cfg).len(),
            5,
            "IPv6 block / api / 非住宅用户 → block / resi-route → block / resi-fallback → direct"
        );
    }

    /// 4.2.1 安全修订的核心：非住宅 Reality 用户自己把 UUID 第 6/7 字节改成 `0001`，
    /// 也只会被自己的 per-user 规则 `→ block` 拦住；通用 `resi-route` 同样是 block。
    /// 权益从「无住宅」变「有住宅」只改他的 user 规则（relay vs block）⇒ gRPC 收敛，
    /// 不改结构哈希。
    #[test]
    fn a_direct_only_reality_user_is_blocked_on_route_1() {
        let p = Paths::default_server();
        let r = resi(1);
        let mut direct_only = user(1, None);
        direct_only.entitlements.residential = None;
        let blocked = config(&node(), std::slice::from_ref(&direct_only), &r, &p);
        assert_eq!(
            user_rules(&blocked),
            vec![(
                user_rule_tag(direct_only.user_id),
                "block".to_string(),
                direct_only.user_id.to_string()
            )]
        );
        let tail = &rules(&blocked)[rules(&blocked).len() - 2];
        assert_eq!(tail["ruleTag"], RESI_ROUTE_RULE_TAG);
        assert_eq!(tail["outboundTag"], "block");

        let entitled = user(1, None); // 同一个 user_id，带住宅权益
        assert_eq!(entitled.user_id, direct_only.user_id);
        let with_relay = config(&node(), std::slice::from_ref(&entitled), &r, &p);
        assert_eq!(user_rules(&with_relay)[0].1, "relay-slot-0");
        assert_eq!(
            structural_hash(&blocked),
            structural_hash(&with_relay),
            "权益变化只改 user 规则，不重启 xray"
        );
        assert_ne!(
            slot_rules_hash(&blocked),
            slot_rules_hash(&with_relay),
            "权益变化必须触发 gRPC 收敛"
        );
    }

    /// D7：加减用户、改用户槽位都**不**改结构哈希（否则 xray 每次加用户都重启，
    /// M3 验收「加用户 NRestarts 不变」当场回归）。
    #[test]
    fn changing_who_is_on_which_slot_does_not_change_the_structural_hash() {
        let r = resi(3);
        let p = Paths::default_server();
        let a = config(&node(), &[user(1, Some(Uuid::from_u128(2)))], &r, &p);
        let b = config(
            &node(),
            &[
                user(1, Some(Uuid::from_u128(3))),
                user(2, Some(Uuid::from_u128(2))),
            ],
            &r,
            &p,
        );
        assert_ne!(a, b, "配置内容变了（clients + 槽规则）");
        assert_eq!(
            structural_hash(&a),
            structural_hash(&b),
            "结构没变 ⇒ 不重启 xray"
        );
        assert_ne!(
            slot_rules_hash(&a),
            slot_rules_hash(&b),
            "槽规则变了 ⇒ 脏标记要置位，等对账末尾的 converge_xray 走 gRPC 收敛"
        );
    }

    /// 槽位数变了才算结构变化（出站表与兜底规则都变了，必须重启才能生效）。
    #[test]
    fn adding_a_slot_does_change_the_structural_hash() {
        let p = Paths::default_server();
        let a = config(&node(), &[], &resi(1), &p);
        let b = config(&node(), &[], &resi(2), &p);
        assert_ne!(structural_hash(&a), structural_hash(&b));
    }

    /// 4.2.1：两条通用规则都是常量（`resi-route → block`、`resi-fallback → direct`），
    /// **不再随兜底槽序号变化**；它们留在结构哈希里也无害。用户增删 / 分槽变化只改被剥离
    /// 的 `user` 规则 ⇒ 结构哈希不变（gRPC 收敛）。槽位表本身增删仍会改出站表 ⇒ 仍要重启，
    /// 那是出站表的变化，不是这两条规则。
    #[test]
    fn the_generic_tail_rules_are_constant_across_slot_and_user_changes() {
        let p = Paths::default_server();
        let r = resi(2);
        let empty = config(&node(), &[], &r, &p);
        let tail = rules(&empty)[rules(&empty).len() - 2..].to_vec();
        assert_eq!(tail[0]["ruleTag"], RESI_ROUTE_RULE_TAG);
        assert_eq!(tail[0]["outboundTag"], "block");
        assert_eq!(tail[0]["vlessRoute"], "1");
        assert_eq!(tail[1]["ruleTag"], FALLBACK_RULE_TAG);
        assert_eq!(tail[1]["outboundTag"], "direct");
        assert!(tail[1].get("vlessRoute").is_none());

        // 加用户 / 改分槽：表尾一字不差，结构哈希不变
        let with_user = config(&node(), &[user(1, Some(Uuid::from_u128(2)))], &r, &p);
        assert_eq!(&rules(&with_user)[rules(&with_user).len() - 2..], &tail[..]);
        assert_eq!(structural_hash(&empty), structural_hash(&with_user));

        // 槽位表本身变化：出站表变了 ⇒ 仍要重启（不是因为表尾规则）
        let mut shrunk = r.clone();
        shrunk.slots.retain(|s| s.index != 0);
        let one_slot = config(&node(), &[], &shrunk, &p);
        assert_eq!(
            &rules(&one_slot)[rules(&one_slot).len() - 2..],
            &tail[..],
            "表尾仍不引用任何槽"
        );
        assert_ne!(structural_hash(&empty), structural_hash(&one_slot));
    }

    #[test]
    fn slot_rules_hash_ignores_everything_but_the_user_rules() {
        let p = Paths::default_server();
        let r = resi(2);
        let a = config(&node(), &[user(1, Some(Uuid::from_u128(2)))], &r, &p);
        let mut b = a.clone();
        // 改 clients（加用户走 gRPC 的那一半）不该动槽规则哈希
        b["inbounds"][1]["settings"]["clients"] = serde_json::json!([]);
        assert_eq!(slot_rules_hash(&a), slot_rules_hash(&b));
    }
}
