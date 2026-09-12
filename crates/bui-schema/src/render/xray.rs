//! Xray 配置渲染：双 REALITY inbound（`vless-direct` 直连 / `vless-residential` 走本地 relay）。
//!
//! 字段逐条对齐 v3 `server/core.sh` 的 `xray-config.json` 模板。

use crate::model::{NodeParams, Protocol, User};
use crate::paths::Paths;
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
/// 住宅出口指向的本地 sing-box relay。
const RELAY_PORT: u16 = 2080;

/// 渲染整份 `xray-config.json`。
///
/// `users` 里只有开通了 [`Protocol::Reality`] 且未停用的用户进 `clients`，两个 inbound 共用同一份。
///
/// **调用方必须只传入当前有效（未到期、未超限）的用户**：本函数只看
/// `disabled` 与权益，不做期限、流量配额判断；把过期或超限用户传进来
/// 就等于给他们签发可用的 REALITY 凭据。
///
/// 当前模板不需要磁盘路径，`_paths` 只为满足 C1 契约的统一签名。
pub fn config(node: &NodeParams, users: &[User], _paths: &Paths) -> Value {
    let clients = clients(users);
    json!({
        "log": {"loglevel": "warning"},
        "stats": {},
        "api": {"tag": "api", "services": ["StatsService", "HandlerService"]},
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
        "outbounds": [
            {"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "ForceIPv4"}},
            {"tag": "relay", "protocol": "socks",
             "settings": {"servers": [{"address": "127.0.0.1", "port": RELAY_PORT}]}}
        ],
        "routing": {
            "rules": [
                {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
                {"type": "field", "inboundTag": ["vless-direct"], "outboundTag": "direct"},
                {"type": "field", "inboundTag": ["vless-residential"], "outboundTag": "relay"}
            ]
        }
    })
}

/// 配置的结构哈希：去掉每个 inbound 的 `settings.clients` 后取 sha256。
///
/// 用户增删走 gRPC，不该触发 xray 重启，所以 `clients` 不进哈希。
pub fn structural_hash(cfg: &Value) -> String {
    let mut stripped = cfg.clone();
    if let Some(inbounds) = stripped.get_mut("inbounds").and_then(Value::as_array_mut) {
        for inbound in inbounds {
            if let Some(settings) = inbound.get_mut("settings").and_then(Value::as_object_mut) {
                settings.remove("clients");
            }
        }
    }
    let bytes = serde_json::to_vec(&stripped).expect("Value 序列化不会失败");
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
}
