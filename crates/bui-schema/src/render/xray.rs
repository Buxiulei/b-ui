//! Xray 配置渲染：双 REALITY inbound（`vless-direct` 直连 / `vless-residential` 走本地 relay）。
//!
//! 字段逐条对齐 v3 `server/core.sh` 的 `xray-config.json` 模板。

use crate::model::{NodeParams, Protocol, Residential, User};
use crate::paths::Paths;
use crate::slots;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 住宅槽路由规则的 `ruleTag` 前缀：gRPC 侧按它认「这条规则是槽位模块的」。
/// 改它等于把线上已经装载的旧规则变成认不出来的孤儿（收敛会把它们当多余项删掉，
/// 一轮之内自愈，但别没事乱改）。
pub const USER_RULE_PREFIX: &str = "resi-u-";
/// 兜底规则的 `ruleTag`：永远排在表尾，兜住「一条规则都没有的 email」。
pub const FALLBACK_RULE_TAG: &str = "resi-fallback";

/// 用户 `user_id`（= xray 侧的 email）对应的规则 tag。
pub fn user_rule_tag(user_id: uuid::Uuid) -> String {
    format!("{USER_RULE_PREFIX}{user_id}")
}

/// 一条住宅槽路由规则 —— **渲染（写 `xray-config.json`）与 gRPC 增删共用同一个形状**，
/// 两边不可能各算一份（D7）。`emails` 为空 = 兜底规则（不带 `user`，匹配该 inbound 的全部流量）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRule {
    pub rule_tag: String,
    pub inbound_tag: String,
    pub emails: Vec<String>,
    pub outbound_tag: String,
}

/// API inbound 的本地端口（v3 固定值）。
const API_PORT: u16 = 10085;

/// 住宅入站的 tag（规则与 `XRAY_INBOUND_TAGS[1]` 同一个字面量）。
const RESI_INBOUND_TAG: &str = "vless-residential";

/// 渲染整份 `xray-config.json`。
///
/// `users` 里只有开通了 [`Protocol::Reality`] 且未停用的用户进 `clients`，两个 inbound 共用同一份。
///
/// `resi` 提供槽位表（spec §5.6）：每槽一个 `relay-slot-<i>` 出站（socks →
/// `127.0.0.1:(2080+i)`），住宅入站**每个用户一条规则**分到自己的槽（`ruleTag`
/// = `resi-u-<user_id>`），末尾一条 `resi-fallback` 兜住还没有规则的 email。
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

    let mut outbounds = vec![
        json!({"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "ForceIPv4"}}),
    ];
    for i in slots::indices(resi) {
        let res = slots::resources_of(&node.ports, resi, i);
        outbounds.push(json!({
            "tag": relay_tag(i),
            "protocol": "socks",
            "settings": {"servers": [{"address": "127.0.0.1", "port": res.relay_port}]}
        }));
    }

    let mut rules = vec![
        json!({"type": "field", "inboundTag": ["api"], "outboundTag": "api"}),
        json!({"type": "field", "inboundTag": ["vless-direct"], "outboundTag": "direct"}),
    ];
    // 每个住宅用户一条、末尾兜底一条（顺序即 `slot_rules` 的顺序）。带 `user` 的那些
    // **不进结构哈希**（[`structural_hash`]，D7），所以加用户不重启 xray。
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
            vless_inbound("vless-direct", node.ports.reality_direct, node, &clients),
            vless_inbound("vless-residential", node.ports.reality_resi, node, &clients),
        ],
        "outbounds": outbounds,
        "routing": {"rules": rules}
    })
}

/// 期望态的住宅槽规则表：每个住宅用户一条（按 `user_id` 升序），**末尾一条兜底**。
///
/// 渲染（写文件）与 gRPC 收敛（`residential::slots::converge_xray`）共用这一个函数，
/// 两边不可能各算出一份不同的表（D7）。
///
/// 被封（超限/到期）的用户**留着**他的规则：他的凭据已经被 `sync_users` 从 `clients`
/// 里摘掉了，规则匹配不到任何连接，留着能让「渲染 = runtime」这条不变量保持简单。
pub fn slot_rules(users: &[User], resi: &Residential) -> Vec<SlotRule> {
    let mut out: Vec<SlotRule> = slot_users(users)
        .into_iter()
        .map(|u| SlotRule {
            rule_tag: user_rule_tag(u.user_id),
            inbound_tag: RESI_INBOUND_TAG.to_string(),
            emails: vec![u.user_id.to_string()],
            outbound_tag: relay_tag(slots::index_of_user(u, resi)),
        })
        .collect();
    out.push(SlotRule {
        rule_tag: FALLBACK_RULE_TAG.to_string(),
        inbound_tag: RESI_INBOUND_TAG.to_string(),
        emails: vec![],
        outbound_tag: relay_tag(slots::fallback_index(resi)),
    });
    out
}

/// 一条 [`SlotRule`] 的 JSON 形态（`emails` 为空就不写 `user` 字段 —— 写成 `[]`
/// 会变成「匹配空用户列表」，内核语义与「不过滤」不同）。
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
    v
}

/// 槽 `i` 的住宅出站 tag。
fn relay_tag(i: u16) -> String {
    format!("relay-slot-{i}")
}

/// 会出现在槽规则里的用户：未停用、有 Reality 权益、**且有住宅权益**（其余人没有住宅
/// 节点，列进 `user` 只会匹配一个用不到的 email）。按 `user_id` 升序 —— 渲染必须确定性，
/// 顺序抖动会让文件与两个哈希无谓地变。
fn slot_users(users: &[User]) -> Vec<&User> {
    let mut v: Vec<&User> = users
        .iter()
        .filter(|u| {
            !u.disabled
                && u.entitlements.protocols.contains(&Protocol::Reality)
                && u.entitlements.residential.is_some()
        })
        .collect();
    v.sort_unstable_by_key(|u| u.user_id);
    v
}

/// 配置的结构哈希：去掉每个 inbound 的 `settings.clients`、**以及 `routing.rules` 里
/// 带 `user` 字段的规则**后取 sha256。
///
/// 用户增删走 gRPC、用户改槽位只改这些 `user` 规则（也走 gRPC），两者都不该触发
/// xray 重启（D7 / spec §3.3；重启会掐断全部 REALITY 连接）。兜底规则没有 `user`，
/// 留在哈希里 —— 它只在兜底槽序号变化时变，那一刻本来就该重启。
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

/// 一个 REALITY inbound（两条通路只差 tag 与端口）。
fn vless_inbound(tag: &str, port: u16, node: &NodeParams, clients: &[Value]) -> Value {
    json!({
        "tag": tag,
        "port": port,
        "protocol": "vless",
        "settings": {"clients": clients, "decryption": "none"},
        "streamSettings": {
            "network": "tcp",
            "security": "reality",
            "realitySettings": {
                "dest": node.reality.dest,
                "serverNames": node.reality.server_names,
                "privateKey": node.reality.private_key,
                "shortIds": node.reality.short_ids
            }
        },
        "sniffing": {"enabled": true, "destOverride": ["http", "tls"]}
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
        assert_eq!(tags, vec!["direct", "relay-slot-0"]);
        assert_eq!(cfg["outbounds"][1]["settings"]["servers"][0]["port"], 2080);
        let r = rules(&cfg);
        assert_eq!(r.len(), 4, "api / 直连 / 这个用户一条 / 住宅兜底");
        // 未分配的用户也有自己的一条规则，指向兜底槽（D7：一人一条，追加顺序才无所谓）
        assert_eq!(
            user_rules(&cfg),
            vec![(
                format!("resi-u-{}", u.user_id),
                "relay-slot-0".to_string(),
                u.user_id.to_string()
            )]
        );
        let last = r.last().unwrap();
        assert_eq!(last["ruleTag"], "resi-fallback");
        assert_eq!(last["outboundTag"], "relay-slot-0");
        assert_eq!(last["inboundTag"], serde_json::json!(["vless-residential"]));
        assert!(last.get("user").is_none(), "兜底规则不带 user");
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
        assert_eq!(want.len(), 3, "两个用户各一条 + 兜底一条");
        assert_eq!(want.last().unwrap().rule_tag, FALLBACK_RULE_TAG);
        assert!(want.last().unwrap().emails.is_empty());
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
        assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 3);
        assert!(
            !rules(&cfg)
                .iter()
                .any(|r| r["outboundTag"] == "relay-slot-1"),
            "槽 1 没人 ⇒ 没有规则指向它（出站照留，xray 启动时要能解析）"
        );
    }

    /// 停用 / 没有 Reality 权益 / 没有住宅权益的用户都不进槽规则 —— 前两种在 `clients`
    /// 里也不存在，第三种根本没有住宅节点，列进 `user` 只会匹配一个用不到的 email。
    #[test]
    fn disabled_non_reality_and_direct_only_users_are_left_out_of_the_slot_rules() {
        let mut disabled = user(2, Some(Uuid::from_u128(2)));
        disabled.disabled = true;
        let mut hy2_only = user(3, Some(Uuid::from_u128(2)));
        hy2_only.entitlements.protocols = vec![Protocol::Hysteria2];
        let mut direct_only = user(4, None);
        direct_only.entitlements.residential = None;
        let cfg = config(
            &node(),
            &[disabled, hy2_only, direct_only],
            &resi(2),
            &Paths::default_server(),
        );
        assert!(user_rules(&cfg).is_empty());
        assert_eq!(rules(&cfg).len(), 3, "只剩 api / 直连 / 兜底");
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

    /// 兜底规则没有 `user` ⇒ **留在**结构哈希里：它只在兜底槽序号变化时变，
    /// 那一刻本来就该重启（D7）。
    #[test]
    fn the_fallback_rule_stays_inside_the_structural_hash() {
        let p = Paths::default_server();
        let mut r = resi(2);
        let a = config(&node(), &[], &r, &p);
        // 删掉序号 0 那个槽 ⇒ 兜底槽变成槽 1
        r.slots.retain(|s| s.index != 0);
        let b = config(&node(), &[], &r, &p);
        assert_eq!(rules(&b).last().unwrap()["outboundTag"], "relay-slot-1");
        assert_ne!(structural_hash(&a), structural_hash(&b));
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
