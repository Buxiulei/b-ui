//! 测试专用夹具（`#[cfg(test)]`，不进生产构建）。
use bui_schema::model::State;

/// spec §2.1 的期望态样例（合成值，无生产秘密）。argon2 哈希是 `test123` 的固定串。
pub fn sample_state() -> State {
    serde_json::from_str(SAMPLE).expect("sample_state 必须能解析")
}

const SAMPLE: &str = r#"{
  "schema_version": 1,
  "node": { "id": "8d5a1a1e-3b2c-4d1e-9f00-000000000001", "name": "node-a", "domain": "example.com", "public_ip": "203.0.113.10",
            "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                       "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
            "reality": { "private_key": "CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4",
                         "public_key": "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c",
                         "short_ids": ["0123456789abcdef"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
            "obfs": { "enabled": false, "password": "" } },
  "admin": { "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Q2g5cWFrZXN0aGFzaHZhbHVl", "jwt_secret": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff" },
  "users": [ { "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa", "username": "alice", "note": "", "created_at": "2026-09-11T00:00:00Z", "disabled": false,
               "credentials": { "hy2_password": "pw-alice-01", "vless_uuid": "11111111-1111-4111-8111-111111111111" },
               "entitlements": { "protocols": ["hysteria2", "reality"], "direct": true, "residential": { "group_id": "default" },
                                 "expires_at": null, "traffic_limit": { "total_bytes": null, "monthly_bytes": null } },
               "usage": { "total_bytes": 0, "monthly_bytes": 0, "month_key": "2026-09", "last_seen_at": null },
               "portal_auth": { "password_hash": null, "tokens": [] },
               "billing": { "currency": "CNY", "balance_minor": 0, "orders": [] } } ],
  "residential": { "groups": { "default": { "enabled": false, "mode": "split", "keywords": null, "upstreams": [],
                    "selected_upstream_id": null, "blacklist": { "pins": [], "auto": [] } } } },
  "system": { "ssh_hardening": true, "static_dns": true, "sysctl_profile": "auto", "firewall": "auto" },
  "versions": { "bui": "4.0.0", "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2", "client_sing_box": "1.13.19" },
  "catalog": []
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_state_parses() {
        let s = sample_state();
        assert_eq!(s.node.ports.hy2, 10000);
        assert_eq!(s.users.len(), 1);
    }
}
