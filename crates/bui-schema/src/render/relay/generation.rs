//! 可独立验证的 relay 前端与不可变出口代际配置编译器。
//!
//! 仅面向 sing-box 1.14.2；当前生产 renderer 尚未调用本模块。
//! 从既有纯 renderer 按入站所有权投影，而不复制公共分流和 UUID 策略规则：
//! 前端仅保留 slot 入站及其 DNS/路由，后端仅保留 policy 入站、其 logical 子规则及
//! 真实供应商出口。这样 pins、UDP 保护、嗅探和解析顺序仍由同一份规则定义。
use super::RelayOpts;
use crate::model::{ResidentialGroup, Slot, UpstreamKind};
use crate::relay_generation::Bank;
use crate::relay_policy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct FrontOpts {
    pub relay: RelayOpts,
    pub bank: Bank,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendOpts {
    pub bank: Bank,
    pub generation: u64,
    pub server_ip: Option<String>,
}

/// 稳定入口：原槽 selector 选择 UUID，`resi-N` 再选择那个 UUID 的 A/B 后端。
/// 两层 selector 不增加网络跳数；仅 bank 成员出站建立一次 SOCKS 回环。
pub fn front_config(
    group: &ResidentialGroup,
    slots: &[Slot],
    opts: &FrontOpts,
) -> Result<Value, String> {
    validate_group(group)?;
    if slots.iter().any(|slot| {
        slot.index >= crate::slots::MAX_SLOTS
            || opts.relay.listen_port.checked_add(slot.index).is_none()
    }) {
        return Err("relay slot index or listening port is outside the supported range".into());
    }

    let policies = policy_inbounds(group);
    let mut cfg = super::config(group, slots, &opts.relay);
    cfg["inbounds"]
        .as_array_mut()
        .expect("relay renderer always emits an inbounds array")
        .retain(|inbound| !policies.contains(inbound["tag"].as_str().unwrap_or_default()));
    cfg["route"]["rules"]
        .as_array_mut()
        .expect("relay renderer always emits route rules")
        .retain(|rule| !targets_policy(rule, &policies));

    if group.pool_active() {
        let mut outbounds = Vec::new();
        for (index, upstream) in group.upstreams.iter().enumerate() {
            for bank in [Bank::A, Bank::B] {
                let mut member = json!({
                    "type": "socks",
                    "tag": bank.member_tag(upstream.id),
                    "server": "127.0.0.1",
                    "server_port": bank.policy_port(index).expect("group capacity was validated"),
                    "version": "5"
                });
                // Each supplier's protocol is a front structure boundary, even if another HTTP
                // member already caused the shared mixed-pool UDP guard to reject UDP.
                if upstream.kind == UpstreamKind::Http {
                    member["network"] = json!("tcp");
                }
                outbounds.push(member);
            }
        }
        outbounds.extend(group.upstreams.iter().enumerate().map(|(index, upstream)| {
            json!({
                "type": "selector",
                "tag": super::member_tag(index),
                "outbounds": [Bank::A.member_tag(upstream.id), Bank::B.member_tag(upstream.id)],
                "default": opts.bank.member_tag(upstream.id),
                "interrupt_exist_connections": false
            })
        }));
        // Keep slot preference, global selection and direct semantics from the same renderer;
        // discard every original loopback member and raw supplier credential.
        outbounds.extend(
            cfg["outbounds"]
                .as_array()
                .expect("relay renderer always emits an outbounds array")
                .iter()
                .filter(|outbound| outbound["type"] == "selector" || outbound["type"] == "direct")
                .cloned(),
        );
        cfg["outbounds"] = json!(outbounds);
    }
    Ok(cfg)
}

/// 不可变后端：只拥有实际 UUID 策略，不拥有公共规则、选择器或共享 cache。
pub fn backend_config(group: &ResidentialGroup, opts: &BackendOpts) -> Result<Value, String> {
    validate_group(group)?;
    if opts.generation == 0 {
        return Err("relay generation must be nonzero".into());
    }
    let policies = policy_inbounds(group);
    let inbound_names: BTreeMap<_, _> = group
        .upstreams
        .iter()
        .map(|upstream| {
            (
                relay_policy::inbound_tag(upstream.id),
                format!("resi-policy-g{}-{}", opts.generation, upstream.id),
            )
        })
        .collect();
    let raw_names: BTreeMap<_, _> = group
        .upstreams
        .iter()
        .map(|upstream| {
            Ok((
                relay_policy::egress_tag(upstream.id),
                egress_tag(opts.generation, upstream.id)?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let mut cfg = super::config(
        group,
        &[],
        &RelayOpts {
            listen_port: crate::slots::RELAY_SOCKS_BASE,
            api: opts.bank.api().into(),
            cache_path: String::new(),
            server_ip: opts.server_ip.clone(),
        },
    );
    let inbounds = cfg["inbounds"]
        .as_array_mut()
        .expect("relay renderer always emits an inbounds array");
    inbounds.retain(|inbound| policies.contains(inbound["tag"].as_str().unwrap_or_default()));
    for (index, inbound) in inbounds.iter_mut().enumerate() {
        let old = inbound["tag"].as_str().expect("policy inbound tag");
        inbound["tag"] = json!(inbound_names[old]);
        inbound["listen_port"] = json!(opts
            .bank
            .policy_port(index)
            .expect("group capacity was validated"));
    }
    let rules = cfg["route"]["rules"]
        .as_array_mut()
        .expect("relay renderer always emits route rules");
    rules.retain(|rule| targets_policy(rule, &policies));
    for rule in rules {
        rename_rule(rule, &inbound_names, &raw_names);
    }
    cfg["route"]["final"] = json!("direct");
    let outbounds = cfg["outbounds"]
        .as_array_mut()
        .expect("relay renderer always emits an outbounds array");
    outbounds.retain(|outbound| {
        outbound["tag"] == "direct"
            || raw_names.contains_key(outbound["tag"].as_str().unwrap_or_default())
    });
    for outbound in outbounds {
        if let Some(tag) = outbound["tag"].as_str().and_then(|tag| raw_names.get(tag)) {
            outbound["tag"] = json!(tag);
        }
    }
    cfg["dns"]["servers"]
        .as_array_mut()
        .expect("relay renderer always emits DNS servers")
        .retain(|server| server["tag"] == "dns_direct");
    cfg["dns"]["rules"] = json!([]);
    cfg["dns"]["final"] = json!("dns_direct");
    cfg["experimental"]
        .as_object_mut()
        .expect("relay renderer always emits experimental options")
        .remove("cache_file");
    Ok(cfg)
}

/// Only saved global selection and well-formed A/B member defaults are mutable selections.
/// Slot defaults, member identities, public rules, DNS and every other field remain structural.
pub fn front_structural_hash(cfg: &Value) -> String {
    let mut structure = cfg.clone();
    if let Some(outbounds) = structure.get_mut("outbounds").and_then(Value::as_array_mut) {
        for outbound in outbounds {
            if outbound["type"] == "selector"
                && (outbound["tag"] == super::POOL || is_bank_selector(outbound))
            {
                if let Some(fields) = outbound.as_object_mut() {
                    fields.remove("default");
                }
            }
        }
    }
    let bytes = serde_json::to_vec(&structure).expect("Value serialization cannot fail");
    hex::encode(Sha256::digest(bytes))
}

/// Canonical generation + UUID identity used for raw supplier error attribution.
pub fn egress_tag(generation: u64, id: Uuid) -> Result<String, String> {
    if generation == 0 || id.is_nil() {
        return Err("relay generation and supplier UUID must be nonzero".into());
    }
    Ok(format!("resi-egress-g{generation}-{id}"))
}

pub fn parse_egress_tag(tag: &str) -> Option<(u64, Uuid)> {
    let (number, uuid) = tag.strip_prefix("resi-egress-g")?.split_once('-')?;
    let generation = number.parse::<u64>().ok()?;
    let id = Uuid::parse_str(uuid).ok()?;
    (egress_tag(generation, id).ok()?.as_str() == tag).then_some((generation, id))
}

fn validate_group(group: &ResidentialGroup) -> Result<(), String> {
    relay_policy::validate_group(group)?;
    let mut ids = BTreeSet::new();
    if group
        .upstreams
        .iter()
        .any(|upstream| upstream.id.is_nil() || !ids.insert(upstream.id))
    {
        return Err("relay supplier UUIDs must be nonzero and unique".into());
    }
    Ok(())
}

fn policy_inbounds(group: &ResidentialGroup) -> BTreeSet<String> {
    group
        .upstreams
        .iter()
        .map(|upstream| relay_policy::inbound_tag(upstream.id))
        .collect()
}

fn targets_policy(rule: &Value, policies: &BTreeSet<String>) -> bool {
    rule["inbound"].as_array().is_some_and(|inbounds| {
        inbounds
            .iter()
            .any(|inbound| inbound.as_str().is_some_and(|tag| policies.contains(tag)))
    }) || rule["rules"]
        .as_array()
        .is_some_and(|rules| rules.iter().any(|rule| targets_policy(rule, policies)))
}

fn rename_rule(
    rule: &mut Value,
    inbound_names: &BTreeMap<String, String>,
    raw_names: &BTreeMap<String, String>,
) {
    if let Some(inbounds) = rule.get_mut("inbound").and_then(Value::as_array_mut) {
        for inbound in inbounds {
            if let Some(tag) = inbound.as_str().and_then(|tag| inbound_names.get(tag)) {
                *inbound = json!(tag);
            }
        }
    }
    if let Some(tag) = rule["outbound"].as_str().and_then(|tag| raw_names.get(tag)) {
        rule["outbound"] = json!(tag);
    }
    if let Some(rules) = rule.get_mut("rules").and_then(Value::as_array_mut) {
        for rule in rules {
            rename_rule(rule, inbound_names, raw_names);
        }
    }
}

fn is_bank_selector(outbound: &Value) -> bool {
    let Some(number) = outbound["tag"]
        .as_str()
        .and_then(|tag| tag.strip_prefix("resi-"))
    else {
        return false;
    };
    let Ok(index) = number.parse::<u16>() else {
        return false;
    };
    if !(1..=crate::slots::MAX_SLOTS).contains(&index) || index.to_string() != number {
        return false;
    }
    let Some(members) = outbound["outbounds"]
        .as_array()
        .filter(|members| members.len() == 2)
    else {
        return false;
    };
    let Some(id) = members[0]
        .as_str()
        .and_then(|tag| tag.strip_prefix("resi-policy-bank-a-"))
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil())
    else {
        return false;
    };
    members[0] == Bank::A.member_tag(id)
        && members[1] == Bank::B.member_tag(id)
        && members.contains(&outbound["default"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AutoEntry, Pin, ResiMode, Rule, Upstream, UpstreamKind};
    use serde_json::json;
    use std::collections::BTreeSet;

    fn group(n: u16) -> ResidentialGroup {
        ResidentialGroup {
            enabled: n > 0,
            mode: ResiMode::Global,
            keywords: Some(vec!["example".into()]),
            upstreams: (1..=n)
                .map(|i| Upstream {
                    id: Uuid::from_u128(u128::from(i)),
                    name: format!("test-{i}"),
                    kind: UpstreamKind::Socks5,
                    host: format!("isp{i}.example.test"),
                    port: 10007,
                    username: format!("user-{i}"),
                    password: format!("secret-{i}"),
                    priority: 100,
                    provider: None,
                    region: None,
                    ports_allowed: None,
                    verified: None,
                })
                .collect(),
            selected_upstream_id: (n > 0).then(|| Uuid::from_u128(1)),
            blacklist: Default::default(),
        }
    }

    fn slots(g: &ResidentialGroup) -> Vec<Slot> {
        g.upstreams
            .iter()
            .enumerate()
            .map(|(i, u)| Slot {
                index: i as u16,
                upstream_id: u.id,
            })
            .collect()
    }

    fn front(bank: Bank) -> FrontOpts {
        FrontOpts {
            relay: RelayOpts {
                listen_port: 2080,
                api: "127.0.0.1:9091".into(),
                cache_path: "/tmp/test-relay-cache.db".into(),
                server_ip: Some("203.0.113.10".into()),
            },
            bank,
        }
    }

    fn backend(bank: Bank) -> BackendOpts {
        BackendOpts {
            bank,
            generation: 42,
            server_ip: Some("203.0.113.10".into()),
        }
    }

    fn outbound<'a>(cfg: &'a Value, tag: &str) -> &'a Value {
        cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == tag)
            .unwrap_or_else(|| panic!("missing outbound {tag}"))
    }

    fn auto(id: Uuid, rule: Rule) -> AutoEntry {
        AutoEntry {
            upstream_id: id,
            rule,
            hits: 3,
            confirmed_at: "2026-09-30T00:00:00Z".into(),
            last_verified_at: "2026-09-30T00:00:00Z".into(),
            passes: 0,
        }
    }

    fn assert_no_dangling_outbounds(cfg: &Value) {
        let tags: BTreeSet<_> = cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap())
            .collect();
        for outbound in cfg["outbounds"].as_array().unwrap() {
            if let Some(members) = outbound["outbounds"].as_array() {
                for member in members {
                    assert!(tags.contains(member.as_str().unwrap()), "{member}");
                }
                if let Some(default) = outbound["default"].as_str() {
                    assert!(tags.contains(default), "{default}");
                }
            }
        }
        for rule in cfg["route"]["rules"].as_array().unwrap() {
            if let Some(target) = rule["outbound"].as_str() {
                assert!(tags.contains(target), "{target}");
            }
        }
        assert!(tags.contains(cfg["route"]["final"].as_str().unwrap()));
        for server in cfg["dns"]["servers"].as_array().unwrap() {
            if let Some(target) = server["detour"].as_str() {
                assert!(tags.contains(target), "{target}");
            }
        }
    }

    #[test]
    fn front_keeps_stable_slots_and_exactly_one_loopback_hop_to_either_bank() {
        let g = group(2);
        let cfg = front_config(&g, &slots(&g), &front(Bank::A)).unwrap();
        assert_eq!(
            cfg["inbounds"],
            json!([
                {"type":"socks","tag":"slot-0","listen":"127.0.0.1","listen_port":2080},
                {"type":"socks","tag":"slot-1","listen":"127.0.0.1","listen_port":2081}
            ])
        );
        let selector = outbound(&cfg, "resi-1");
        assert_eq!(selector["type"], "selector");
        assert_eq!(
            selector["outbounds"],
            json!([
                "resi-policy-bank-a-00000000-0000-0000-0000-000000000001",
                "resi-policy-bank-b-00000000-0000-0000-0000-000000000001"
            ])
        );
        assert_eq!(
            selector["default"],
            "resi-policy-bank-a-00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(selector["interrupt_exist_connections"], false);
        assert_eq!(outbound(&cfg, "slot-1-pool")["default"], "resi-2");
        assert_eq!(
            outbound(&cfg, "slot-1-pool")["outbounds"],
            json!(["resi-2", "resi-1"])
        );
        assert_eq!(outbound(&cfg, "resi-pool")["default"], "resi-1");
        let socks: Vec<_> = cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["type"] == "socks")
            .collect();
        assert_eq!(socks.len(), 4);
        let ports: BTreeSet<_> = socks
            .iter()
            .map(|o| o["server_port"].as_u64().unwrap())
            .collect();
        assert_eq!(ports, BTreeSet::from([2180, 2181, 2280, 2281]));
        assert!(socks.iter().all(|o| o["server"] == "127.0.0.1"));
        assert!(!cfg.to_string().contains("secret-"));
        assert_eq!(
            cfg["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:9091"
        );
        assert_no_dangling_outbounds(&cfg);
    }

    #[test]
    fn private_credentials_and_actual_upstream_policy_do_not_change_front_structure() {
        let g = group(2);
        let before_front = front_config(&g, &slots(&g), &front(Bank::A)).unwrap();
        let before_back = backend_config(&g, &backend(Bank::A)).unwrap();
        let mut changed = g.clone();
        changed.upstreams[1].host = "rotated.example.test".into();
        changed.upstreams[1].port = 10008;
        changed.upstreams[1].username = "rotated".into();
        changed.upstreams[1].password = "rotated-secret".into();
        changed.upstreams[1].ports_allowed = Some(vec![80, 443]);
        changed.blacklist.auto.push(auto(
            changed.upstreams[1].id,
            Rule::Domain("failed.example.test".into()),
        ));
        let after_front = front_config(&changed, &slots(&changed), &front(Bank::A)).unwrap();
        let after_back = backend_config(&changed, &backend(Bank::A)).unwrap();
        assert_eq!(before_front, after_front);
        assert_eq!(
            front_structural_hash(&before_front),
            front_structural_hash(&after_front)
        );
        assert_ne!(before_back, after_back);
        assert_eq!(
            outbound(
                &after_back,
                "resi-egress-g42-00000000-0000-0000-0000-000000000002"
            )["password"],
            "rotated-secret"
        );
    }

    #[test]
    fn shared_routing_and_identity_changes_require_a_different_front_structure() {
        let g = group(2);
        let opts = front(Bank::A);
        let before = front_structural_hash(&front_config(&g, &slots(&g), &opts).unwrap());
        let mut cases = Vec::new();
        let mut changed = g.clone();
        changed.blacklist.pins.push(Pin {
            rule: Rule::Domain("public.example.test".into()),
            note: String::new(),
            created_at: "2026-09-30T00:00:00Z".into(),
        });
        cases.push(("public pin", changed));
        let mut changed = g.clone();
        changed.mode = ResiMode::Split;
        cases.push(("split mode", changed));
        let mut changed = g.clone();
        changed.upstreams[0].kind = UpstreamKind::Http;
        cases.push(("UDP capability", changed));
        let mut changed = g.clone();
        changed.upstreams.swap(0, 1);
        cases.push(("UUID order", changed));
        let mut changed = g.clone();
        changed.upstreams[0].id = Uuid::from_u128(99);
        cases.push(("UUID identity", changed));
        for (name, changed) in cases {
            assert_ne!(
                before,
                front_structural_hash(&front_config(&changed, &slots(&g), &opts).unwrap()),
                "{name}"
            );
        }
        for (name, mut changed_opts) in [("listen port", opts.clone()), ("public IP", opts.clone())]
        {
            if name == "listen port" {
                changed_opts.relay.listen_port = 2090;
            } else {
                changed_opts.relay.server_ip = Some("203.0.113.11".into());
            }
            assert_ne!(
                before,
                front_structural_hash(&front_config(&g, &slots(&g), &changed_opts).unwrap()),
                "{name}"
            );
        }
        let mut changed_slots = slots(&g);
        changed_slots[1].index = 3;
        assert_ne!(
            before,
            front_structural_hash(&front_config(&g, &changed_slots, &opts).unwrap())
        );
        let mut split = g.clone();
        split.mode = ResiMode::Split;
        let split_before = front_structural_hash(&front_config(&split, &slots(&g), &opts).unwrap());
        split.keywords = Some(vec!["another".into()]);
        assert_ne!(
            split_before,
            front_structural_hash(&front_config(&split, &slots(&g), &opts).unwrap())
        );
        let mut mixed = g.clone();
        mixed.upstreams[1].kind = UpstreamKind::Http;
        let mixed_before = front_structural_hash(&front_config(&mixed, &slots(&g), &opts).unwrap());
        mixed.upstreams[0].kind = UpstreamKind::Http;
        assert_ne!(
            mixed_before,
            front_structural_hash(&front_config(&mixed, &slots(&g), &opts).unwrap()),
            "kind must be structural even if the pool already rejected UDP"
        );
    }

    #[test]
    fn front_hash_excludes_only_valid_bank_defaults_and_global_selection() {
        let mut g = group(2);
        let a = front_config(&g, &slots(&g), &front(Bank::A)).unwrap();
        g.selected_upstream_id = Some(g.upstreams[1].id);
        let b = front_config(&g, &slots(&g), &front(Bank::B)).unwrap();
        assert_ne!(a, b);
        assert_eq!(front_structural_hash(&a), front_structural_hash(&b));
        let mut changed = a.clone();
        let slot = changed["outbounds"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|o| o["tag"] == "slot-0-pool")
            .unwrap();
        slot["default"] = json!("resi-2");
        assert_ne!(front_structural_hash(&a), front_structural_hash(&changed));
        for pointer in [
            "/outbounds/0/server_port",
            "/inbounds/0/listen_port",
            "/dns/servers/0/server",
            "/route/rules/0/action",
        ] {
            let mut changed = a.clone();
            *changed
                .pointer_mut(pointer)
                .expect("rendered network field") = json!("changed");
            assert_ne!(
                front_structural_hash(&a),
                front_structural_hash(&changed),
                "{pointer}"
            );
        }
        for tag in ["resi-0", "resi-01", "resi-9", "resi-unknown", "slot-0-pool"] {
            let unusual = json!({"outbounds":[{"type":"selector","tag":tag,"default":"first"}]});
            let mut changed = unusual.clone();
            changed["outbounds"][0]["default"] = json!("second");
            assert_ne!(
                front_structural_hash(&unusual),
                front_structural_hash(&changed),
                "{tag}"
            );
        }
        let unusual = json!({"outbounds":[{"type":"socks","tag":"resi-1","default":"first"}]});
        let mut changed = unusual.clone();
        changed["outbounds"][0]["default"] = json!("second");
        assert_ne!(
            front_structural_hash(&unusual),
            front_structural_hash(&changed)
        );
        let unusual = json!({"outbounds":[{
            "type":"selector", "tag":"resi-1", "outbounds":["unrelated-a","unrelated-b"], "default":"unrelated-a"
        }]});
        let mut changed = unusual.clone();
        changed["outbounds"][0]["default"] = json!("unrelated-b");
        assert_ne!(
            front_structural_hash(&unusual),
            front_structural_hash(&changed)
        );
    }

    #[test]
    fn backend_tags_every_supplier_with_generation_and_has_no_front_state_or_cache() {
        let g = group(2);
        let a = backend_config(&g, &backend(Bank::A)).unwrap();
        let b = backend_config(&g, &backend(Bank::B)).unwrap();
        assert_eq!(
            a["inbounds"][0]["tag"],
            "resi-policy-g42-00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(a["inbounds"][0]["listen_port"], 2180);
        assert_eq!(b["inbounds"][0]["listen_port"], 2280);
        assert_eq!(
            a["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:9093"
        );
        assert_eq!(
            b["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:9094"
        );
        assert!(a["experimental"].get("cache_file").is_none());
        assert!(a["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["type"] != "selector"));
        assert!(a["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| !i["tag"].as_str().unwrap().starts_with("slot-")));
        for (i, u) in g.upstreams.iter().enumerate() {
            let raw = outbound(&a, &format!("resi-egress-g42-{}", u.id));
            assert_eq!(raw["type"], "socks");
            assert_eq!(raw["version"], "5");
            assert_eq!(raw["server"], u.host);
            assert_eq!(raw["password"], u.password);
            assert_eq!(a["inbounds"][i]["listen"], "127.0.0.1");
        }
        assert_no_dangling_outbounds(&a);
        assert_no_dangling_outbounds(&b);
    }

    #[test]
    fn backend_applies_only_its_uuid_policy_in_dns_sniff_resolve_guard_order() {
        let mut g = group(2);
        g.blacklist.pins.push(Pin {
            rule: Rule::Domain("public.example.test".into()),
            note: String::new(),
            created_at: String::new(),
        });
        g.blacklist.auto.push(auto(
            g.upstreams[1].id,
            Rule::DomainSuffix("failed.example.test".into()),
        ));
        g.blacklist
            .auto
            .push(auto(g.upstreams[1].id, Rule::Port(8443)));
        g.upstreams[1].ports_allowed = Some(vec![443]);
        let cfg = backend_config(&g, &backend(Bank::A)).unwrap();
        let routes = cfg["route"]["rules"].as_array().unwrap();
        let second = "resi-policy-g42-00000000-0000-0000-0000-000000000002";
        let second_routes: Vec<_> = routes
            .iter()
            .filter(|r| {
                r["inbound"] == json!([second]) || r["rules"][0]["inbound"] == json!([second])
            })
            .collect();
        assert_eq!(second_routes.len(), 8);
        assert_eq!(
            second_routes[0],
            &json!({"inbound":[second],"network":"udp","port":53,"outbound":"resi-egress-g42-00000000-0000-0000-0000-000000000002"})
        );
        assert_eq!(
            second_routes[1],
            &json!({"type":"logical","mode":"and","rules":[{"inbound":[second]},{"domain_regex":[".+"],"invert":true}],"action":"sniff"})
        );
        assert_eq!(
            second_routes[2]["domain_suffix"],
            json!(["failed.example.test"])
        );
        assert_eq!(second_routes[2]["outbound"], "direct");
        assert_eq!(second_routes[3]["port"], json!([8443]));
        assert_eq!(
            second_routes[4],
            &json!({"inbound":[second],"network":"udp","action":"resolve","server":"dns_direct","strategy":"ipv4_only"})
        );
        assert!(second_routes[5]["ip_cidr"]
            .as_array()
            .unwrap()
            .contains(&json!("203.0.113.10/32")));
        assert_eq!(
            second_routes[6]["port_range"],
            json!(["1:442", "444:65535"])
        );
        assert_eq!(
            second_routes[7]["outbound"],
            "resi-egress-g42-00000000-0000-0000-0000-000000000002"
        );
        assert!(!cfg.to_string().contains("public.example.test"));
        assert!(routes
            .iter()
            .filter(
                |r| r["inbound"] == json!(["resi-policy-g42-00000000-0000-0000-0000-000000000001"])
            )
            .all(|r| r.get("domain_suffix").is_none() && r.get("port_range").is_none()));
        assert_eq!(
            cfg["dns"]["servers"],
            json!([{"tag":"dns_direct","type":"udp","server":"1.1.1.1"}])
        );
    }

    #[test]
    fn front_keeps_public_pins_split_and_mixed_http_udp_guards_before_selectors() {
        let mut g = group(2);
        g.mode = ResiMode::Split;
        g.upstreams[1].kind = UpstreamKind::Http;
        g.blacklist.pins.push(Pin {
            rule: Rule::DomainSuffix("public.example.test".into()),
            note: String::new(),
            created_at: String::new(),
        });
        g.blacklist.pins.push(Pin {
            rule: Rule::Port(8443),
            note: String::new(),
            created_at: String::new(),
        });
        let cfg = front_config(&g, &slots(&g), &front(Bank::A)).unwrap();
        let r = cfg["route"]["rules"].as_array().unwrap();
        assert_eq!(
            r[0],
            json!({"inbound":["slot-0","slot-1"],"action":"sniff"})
        );
        assert_eq!(r[1]["domain_suffix"], json!(["public.example.test"]));
        assert_eq!(r[2]["port"], json!([8443]));
        assert_eq!(
            r[3],
            json!({"inbound":["slot-0","slot-1"],"network":"udp","port":53,"outbound":"direct"})
        );
        assert_eq!(
            r[4],
            json!({"inbound":["slot-0","slot-1"],"network":"udp","port":443,"action":"reject"})
        );
        assert_eq!(
            r[5],
            json!({"inbound":["slot-0","slot-1"],"network":"udp","outbound":"direct"})
        );
        assert!(r[6]["ip_cidr"]
            .as_array()
            .unwrap()
            .contains(&json!("127.0.0.0/8")));
        assert_eq!(
            r[7],
            json!({"inbound":["slot-0"],"domain_keyword":["example"],"outbound":"slot-0-pool"})
        );
        assert_eq!(
            r[8],
            json!({"inbound":["slot-1"],"domain_keyword":["example"],"outbound":"slot-1-pool"})
        );
        assert_eq!(cfg["route"]["final"], "direct");
        assert_eq!(
            cfg["dns"]["rules"],
            json!([
                {"domain_suffix":["public.example.test"],"server":"dns_direct"},
                {"domain_keyword":["example"],"server":"dns_resi"}
            ])
        );
        let back = backend_config(&g, &backend(Bank::A)).unwrap();
        assert!(back["route"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["action"] != "resolve"));
        assert_eq!(
            outbound(
                &back,
                "resi-egress-g42-00000000-0000-0000-0000-000000000002"
            )["type"],
            "http"
        );
    }

    #[test]
    fn disabled_or_empty_pools_keep_direct_front_and_no_backend_references() {
        let mut disabled = group(2);
        disabled.enabled = false;
        for g in [disabled, group(0)] {
            let front = front_config(&g, &slots(&g), &front(Bank::A)).unwrap();
            let back = backend_config(&g, &backend(Bank::A)).unwrap();
            assert_eq!(
                front["outbounds"],
                json!([{"type":"direct","tag":"direct"}])
            );
            assert_eq!(front["route"]["final"], "direct");
            assert!(front["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["listen_port"] == 2080));
            assert_eq!(back["inbounds"], json!([]));
            assert_eq!(back["outbounds"], json!([{"type":"direct","tag":"direct"}]));
            assert_no_dangling_outbounds(&front);
            assert_no_dangling_outbounds(&back);
        }
    }

    #[test]
    fn oversized_pools_and_zero_generations_are_rejected_before_rendering() {
        let mut g = group(9);
        assert!(front_config(&g, &slots(&g), &front(Bank::A)).is_err());
        assert!(backend_config(&g, &backend(Bank::A)).is_err());
        g = group(1);
        g.upstreams[0].id = Uuid::nil();
        assert!(front_config(&g, &slots(&g), &front(Bank::A)).is_err());
        assert!(backend_config(&g, &backend(Bank::A)).is_err());
        assert!(egress_tag(42, Uuid::nil()).is_err());
        let g = group(8);
        let a = backend_config(&g, &backend(Bank::A)).unwrap();
        assert_eq!(a["inbounds"][7]["listen_port"], 2187);
        let mut invalid = backend(Bank::A);
        invalid.generation = 0;
        assert!(backend_config(&g, &invalid).is_err());
        assert!(egress_tag(0, Uuid::from_u128(1)).is_err());
    }

    #[test]
    fn invalid_slot_ranges_ports_or_duplicate_supplier_identity_are_not_rendered() {
        let mut g = group(2);
        let mut bad_slots = slots(&g);
        bad_slots[1].index = 8;
        assert!(front_config(&g, &bad_slots, &front(Bank::A)).is_err());
        let mut bad_opts = front(Bank::A);
        bad_opts.relay.listen_port = u16::MAX;
        assert!(front_config(&g, &slots(&g), &bad_opts).is_err());
        g.upstreams[1].id = g.upstreams[0].id;
        assert!(front_config(&g, &slots(&g), &front(Bank::A)).is_err());
        assert!(backend_config(&g, &backend(Bank::A)).is_err());
    }

    #[test]
    fn generation_tags_roundtrip_only_complete_canonical_raw_identity() {
        let id = Uuid::from_u128(0xabcdef);
        let tag = "resi-egress-g42-00000000-0000-0000-0000-000000abcdef";
        assert_eq!(egress_tag(42, id).unwrap(), tag);
        assert_eq!(parse_egress_tag(tag), Some((42, id)));
        assert_eq!(
            parse_egress_tag(&egress_tag(u64::MAX, id).unwrap()),
            Some((u64::MAX, id))
        );
        for invalid in [
            tag.replace("g42", "g0"),
            tag.replace("g42", "g042"),
            tag.replace("g42", "g+42"),
            tag.replace("g42", "g18446744073709551616"),
            tag.to_uppercase(),
            format!("{tag} extra"),
            format!("resi-egress-g42-{}", id.simple()),
            tag.replace("resi-egress", "resi-policy"),
            "resi-1".into(),
            format!("resi-egress-{id}"),
            "resi-egress-g42-".into(),
            "resi-egress-g42-00000000-0000-0000-0000-000000000000".into(),
        ] {
            assert_eq!(parse_egress_tag(&invalid), None, "{invalid}");
        }
    }
}
