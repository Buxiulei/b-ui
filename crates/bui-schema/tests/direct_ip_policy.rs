//! 直连出口 IP 策略的行为契约；全部样本来自既有的公开合成 v3 fixture。
//! 用 JSON 注入新字段，让旧模型也能编译并在缺失行为上产生真实 RED。
mod common;

use bui_schema::model::{Hy2Auth, NodeParams, ResiMode, State};
use bui_schema::paths::Paths;
use bui_schema::render::{hy2_singbox, hysteria, relay, xray};
use serde_json::{json, Value};

/// 保留既有 fixture，只在内存里建立两个槽和用户绑定，使配置比较覆盖住宅路由。
fn legacy_state_json() -> Value {
    let mut state = common::state("split");
    bui_schema::slots::sync_slots(&mut state.residential);
    let ids: Vec<_> = state
        .residential
        .default_group()
        .unwrap()
        .upstreams
        .iter()
        .map(|upstream| upstream.id)
        .collect();
    assert_eq!(ids.len(), 2, "公开 fixture 必须覆盖多个住宅上游");
    assert_eq!(state.residential.slots.len(), 2);
    for (index, user) in state.users.iter_mut().enumerate() {
        if let Some(entitlement) = &mut user.entitlements.residential {
            entitlement.slot_id = Some(ids[index % ids.len()]);
        }
    }
    assert!(!state.residential.hy2_pool.creds.is_empty());
    let mut legacy = serde_json::to_value(state).unwrap();
    legacy["node"]
        .as_object_mut()
        .unwrap()
        .remove("direct_ip_policy");
    legacy
}

fn state_with_policy(legacy: &Value, policy: &str) -> State {
    let mut value = legacy.clone();
    value["node"]["direct_ip_policy"] = json!(policy);
    serde_json::from_value(value).unwrap()
}

fn direct_hy2(node: &NodeParams, auth: Hy2Auth) -> serde_yaml::Value {
    serde_yaml::from_str(&hysteria::direct_yaml(node, &Paths::default_server(), auth)).unwrap()
}

fn xray_config(state: &State) -> Value {
    xray::config(
        &state.node,
        &state.users,
        &state.residential,
        &Paths::default_server(),
    )
}

fn direct_xray(cfg: &Value) -> &Value {
    cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outbound| outbound["tag"] == "direct")
        .expect("直连 freedom 出站必须存在")
}

fn assert_ipv4_outputs(state: &State) {
    for auth in [Hy2Auth::Http, Hy2Auth::Command] {
        let hy2 = direct_hy2(&state.node, auth);
        assert_eq!(
            hy2["outbounds"][0]["direct"]["mode"],
            serde_yaml::Value::Number(4.into()),
            "旧状态的直连 HY2 必须保持数字 mode: 4"
        );
    }
    let cfg = xray_config(state);
    assert_eq!(direct_xray(&cfg)["settings"]["domainStrategy"], "ForceIPv4");
    assert_eq!(cfg["dns"]["queryStrategy"], "UseIPv4");
}

#[test]
fn legacy_state_defaults_to_ipv4_and_omits_the_default_policy() {
    let legacy = legacy_state_json();
    let state: State = serde_json::from_value(legacy.clone()).unwrap();
    assert_ipv4_outputs(&state);
    let serialized = serde_json::to_value(&state).unwrap();
    assert!(serialized["node"].get("direct_ip_policy").is_none());
    assert_eq!(serialized, legacy, "读写旧 state 不能新增默认字段");
    assert_eq!(state, state_with_policy(&legacy, "ipv4_only"));
}

#[test]
fn legacy_standalone_node_defaults_to_ipv4_and_omits_the_default_policy() {
    let legacy = legacy_state_json();
    let node: NodeParams = serde_json::from_value(legacy["node"].clone()).unwrap();
    let mut explicit = legacy["node"].clone();
    explicit["direct_ip_policy"] = json!("ipv4_only");
    let ipv4: NodeParams = serde_json::from_value(explicit).unwrap();
    assert_eq!(node, ipv4);
    assert_eq!(serde_json::to_value(&node).unwrap(), legacy["node"]);
    assert_eq!(
        direct_hy2(&node, Hy2Auth::Http)["outbounds"][0]["direct"]["mode"],
        serde_yaml::Value::Number(4.into())
    );
}

#[test]
fn explicit_ipv4_only_renders_ipv4_and_skips_default_serialization() {
    let legacy = legacy_state_json();
    let state = state_with_policy(&legacy, "ipv4_only");
    assert_ipv4_outputs(&state);
    assert_eq!(serde_json::to_value(&state).unwrap(), legacy);
    assert_eq!(serde_json::to_value(&state.node).unwrap(), legacy["node"]);
}

#[test]
fn explicit_dual_stack_survives_state_json_round_trip() {
    let legacy = legacy_state_json();
    let dual = state_with_policy(&legacy, "dual_stack");
    let mut expected = legacy;
    expected["node"]["direct_ip_policy"] = json!("dual_stack");
    assert_eq!(
        serde_json::to_value(&dual).unwrap(),
        expected,
        "双栈策略必须落盘，旧模型忽略该字段会在这里失败"
    );
}

#[test]
fn explicit_dual_stack_survives_standalone_node_json_round_trip() {
    let mut expected = legacy_state_json()["node"].clone();
    expected["direct_ip_policy"] = json!("dual_stack");
    let node: NodeParams = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(node).unwrap(), expected);
}

#[test]
fn invalid_direct_ip_policy_is_rejected_in_state_and_standalone_node_json() {
    let legacy = legacy_state_json();
    for invalid in [
        json!("dual_stak"),
        json!("ipv6_only"),
        Value::Null,
        json!(4),
    ] {
        let mut value = legacy.clone();
        value["node"]["direct_ip_policy"] = invalid.clone();
        assert!(
            serde_json::from_value::<State>(value.clone()).is_err(),
            "非法策略 {invalid} 不能被 State 静默丢弃或回落默认"
        );
        assert!(
            serde_json::from_value::<NodeParams>(value["node"].clone()).is_err(),
            "非法策略 {invalid} 不能被 NodeParams 静默丢弃或回落默认"
        );
    }
}

/// 2026-10-27：旧 state 里的 `dual_stack` 仍能读，但渲染器不再读它 —— HY2 恒 `mode: 4`。
#[test]
fn dual_stack_state_still_renders_hy2_ipv4_only() {
    let legacy = legacy_state_json();
    let ipv4 = state_with_policy(&legacy, "ipv4_only");
    let dual = state_with_policy(&legacy, "dual_stack");
    for auth in [Hy2Auth::Http, Hy2Auth::Command] {
        let expected = direct_hy2(&ipv4.node, auth);
        assert_eq!(
            expected["outbounds"][0]["direct"]["mode"],
            serde_yaml::Value::Number(4.into())
        );
        assert_eq!(
            direct_hy2(&dual.node, auth),
            expected,
            "旧双栈 state 也只渲染 IPv4"
        );
    }
}

/// 旧 `dual_stack` state 的 Xray 与 `ipv4_only` 逐字节相同：出站恒 `ForceIPv4` / `UseIPv4`，
/// 字面 IPv6 由首条 blackhole 规则挡住。
#[test]
fn dual_stack_state_renders_the_same_ipv4_xray_config() {
    let legacy = legacy_state_json();
    let ipv4 = state_with_policy(&legacy, "ipv4_only");
    let dual = state_with_policy(&legacy, "dual_stack");
    let expected = xray_config(&ipv4);
    assert_eq!(
        direct_xray(&expected)["settings"]["domainStrategy"],
        "ForceIPv4"
    );
    assert_eq!(expected["dns"]["queryStrategy"], "UseIPv4");
    assert_eq!(expected["routing"]["domainStrategy"], "AsIs");
    assert_eq!(
        expected["routing"]["rules"][0],
        json!({"type": "field", "ip": ["::/0"], "outboundTag": "block"})
    );
    assert!(expected["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["tag"] == "block" && o["protocol"] == "blackhole"));
    assert!(
        expected["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|inbound| {
                inbound["tag"] == bui_schema::render::xray::INBOUND_TAG
                    && !inbound["settings"]["clients"]
                        .as_array()
                        .unwrap()
                        .is_empty()
            }),
        "全 JSON 比较必须实际包含住宅账户"
    );
    assert!(expected["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| {
            rule.get("user").is_some()
                && rule["ruleTag"]
                    .as_str()
                    .is_some_and(|tag| tag.starts_with("resi-u-"))
        }));
    assert_eq!(
        xray_config(&dual),
        expected,
        "旧双栈 state 的账户、住宅 SOCKS、路由、REALITY 与其他 Xray 字段必须完全不变"
    );
}

/// CLI 与控制 API 共用的解析器：`dual-stack` 明确拒绝，`ipv4-only` 仍可写回默认。
#[test]
fn from_str_rejects_dual_stack_but_still_accepts_ipv4_only() {
    use bui_schema::model::DirectIpPolicy;
    use std::str::FromStr;
    assert_eq!(
        DirectIpPolicy::from_str("ipv4-only").unwrap(),
        DirectIpPolicy::Ipv4Only
    );
    let err = DirectIpPolicy::from_str("dual-stack").unwrap_err();
    assert!(err.contains("dual-stack 已停用"), "{err}");
    assert!(err.contains("IPv4"), "{err}");
    // 落盘名称仍是 snake_case；`as_str` 只服务 API 回显
    assert_eq!(DirectIpPolicy::DualStack.as_str(), "dual-stack");
}

fn relay_config(state: &State) -> Value {
    let group = state.residential.default_group().unwrap();
    assert!(group.pool_active(), "比较必须覆盖有效住宅池");
    assert!(!state.residential.slots.is_empty());
    let opts = relay::RelayOpts {
        listen_port: 2080,
        api: "127.0.0.1:9091".into(),
        cache_path: "/opt/b-ui/relay-cache.db".into(),
        server_ip: Some(state.node.public_ip.clone()),
    };
    let cfg = relay::config(group, &state.residential.slots, &opts);
    assert!(cfg["outbounds"].as_array().unwrap().iter().any(|outbound| {
        outbound["tag"]
            .as_str()
            .is_some_and(|tag| tag.starts_with("resi-egress-"))
    }));
    cfg
}

#[test]
fn direct_policy_preserves_complete_residential_hy2_and_relay_configs() {
    let legacy = legacy_state_json();
    for mode in [ResiMode::Split, ResiMode::Global] {
        for obfs in [false, true] {
            let mut ipv4 = state_with_policy(&legacy, "ipv4_only");
            let mut dual = state_with_policy(&legacy, "dual_stack");
            for state in [&mut ipv4, &mut dual] {
                state.residential.groups.get_mut("default").unwrap().mode = mode;
                state.node.obfs.enabled = obfs;
                state.node.obfs.password = "obfs-pw-test".into();
            }
            let paths = Paths::default_server();
            let residential = hy2_singbox::config(&ipv4.node, &paths, &ipv4.residential.hy2_pool);
            assert!(residential["route"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| {
                    rule["outbound"]
                        .as_str()
                        .is_some_and(|tag| tag.starts_with("gate-"))
                }));
            assert_eq!(
                residential,
                hy2_singbox::config(&dual.node, &paths, &dual.residential.hy2_pool),
                "直连策略不得改变住宅 HY2 的凭据、门、槽、监听或 obfs"
            );
            assert_eq!(
                relay_config(&ipv4),
                relay_config(&dual),
                "直连策略不得改变住宅 relay 的 DNS、真实出口或分流规则"
            );
        }
    }
}
