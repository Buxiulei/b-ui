# v4 P0 基础子项目实施计划（workspace + `bui-schema`）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立 Cargo workspace，并交付 `bui-schema` crate：v4 期望态模型、用户权益 → 节点集合、四种内核配置与三种订阅 + 客户端配置的渲染器、上游 URL 与节点 URI 解析、v3 状态导入，全部以 v3 golden 样本与真实内核校验为测试。

**Architecture:** `bui-schema` 是唯一知道端口、标签、规则与格式的 crate；`bui`（P1–P3）与 `bui-c`（P4）只调用它。渲染用类型化结构体 + serde 序列化，不用文本模板。golden 样本由 v3 的 `web/server.js` 在本机以合成数据生成（无生产秘密）。

**Tech Stack:** Rust 1.93 stable（edition 2021）、serde / serde_json / serde_yaml 0.9、uuid 1（v4 + serde）、base64 0.22、percent-encoding 2、url 2、sha2 + hex、thiserror 2；dev：pretty_assertions、tempfile。集成校验用本机 `/usr/bin/sing-box`（1.13.19）、`/usr/local/bin/xray`（26.3.27）、`/usr/local/bin/hysteria`（2.12.2）、Node 24（生成 fixtures）。

**Spec:** `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`（§1、§2.1、§2.3、§3.1、§4.1、§4.4、§5.1、§5.4、§6）；总纲 `docs/superpowers/plans/2026-09-11-v4-master.md` C1 契约。

## Global Constraints

- 分支 `v4`，任务分支 `v4-p0-t<N>`，每任务一个 commit，尾部附 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`。
- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿才算完成。
- 端口、标签、UUID、密码、参数顺序与 v3 一致：订阅输出必须与 `crates/bui-schema/tests/fixtures/v3/expected/` 逐字节相等（base64 解码后按行比较；JSON 按语义相等；YAML 按语义相等）。
- sing-box 配置兼容 1.12–1.14：不用 `rule_set` / `download_detour`；DNS servers 用 typed 形式；TUN 用 `address` 数组；rule action `sniff` / `hijack-dns` / `reject`；`route.default_domain_resolver`。
- 真实内核校验测试：二进制不存在时打印 `skipped` 并返回，不失败。
- 测试与 fixture 只用合成值：域名 `example.com`、IP `203.0.113.10`、伪装域 `www.bing.com`；密钥由 `xray x25519` 现场生成后写入 fixture（可提交，不是生产密钥）。
- 不修改 `server/`、`web/server.js`、`b-ui-client.sh`（只读参照）。

---

## 文件结构

```
Cargo.toml                                  workspace（members: crates/*）
rust-toolchain.toml                         channel = "stable"
.gitignore                                  + target/
crates/bui-schema/Cargo.toml
crates/bui-schema/src/lib.rs                pub mod model, nodes, parse, keywords, render, v3, paths
crates/bui-schema/src/model.rs              State 与全部类型（C1）
crates/bui-schema/src/paths.rs              Paths { base_dir, certs_dir, bin_dir }（渲染器需要的绝对路径）
crates/bui-schema/src/nodes.rs              Node / NodeKind / Transport / nodes_for
crates/bui-schema/src/keywords.rs           DEFAULT_KEYWORDS
crates/bui-schema/src/parse/mod.rs          ParseError
crates/bui-schema/src/parse/upstream.rs     upstream_url
crates/bui-schema/src/parse/node_uri.rs     node_uri（hysteria2:// / vless://）
crates/bui-schema/src/render/mod.rs         SplitRules / RelayOpts / ClientOpts
crates/bui-schema/src/render/hysteria.rs    direct_yaml / residential_yaml
crates/bui-schema/src/render/xray.rs        config / structural_hash
crates/bui-schema/src/render/relay.rs       config
crates/bui-schema/src/render/subscription.rs uri_list / singbox / clash
crates/bui-schema/src/render/client.rs      tun_config / mixed_config
crates/bui-schema/src/v3.rs                 import
crates/bui-schema/tests/fixtures/v3/src/    合成 v3 状态文件（users.json、config.yaml …）
crates/bui-schema/tests/fixtures/v3/expected/  v3 生成的三种订阅输出（按用户 × 模式）
crates/bui-schema/tests/golden_subscription.rs
crates/bui-schema/tests/kernel_check.rs     真实内核校验
crates/bui-schema/tests/common/mod.rs       fixture 加载与 skip 辅助
scripts/gen-v3-fixtures.sh                  用 v3 server.js 生成 expected/
crates/bui/Cargo.toml + src/main.rs         占位（println!("bui")），P1 填
crates/bui-c/Cargo.toml + src/main.rs       占位，P4 填
```

---

### Task 1: Workspace 脚手架

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `crates/bui-schema/Cargo.toml`, `crates/bui-schema/src/lib.rs`, `crates/bui/Cargo.toml`, `crates/bui/src/main.rs`, `crates/bui-c/Cargo.toml`, `crates/bui-c/src/main.rs`
- Modify: `.gitignore`

**Interfaces:**
- Produces: workspace 可 `cargo build --workspace`；`bui_schema` crate 存在且导出空模块骨架。

- [ ] **Step 1: 写 workspace 与三个 crate**

`Cargo.toml`：
```toml
[workspace]
resolver = "2"
members = ["crates/bui-schema", "crates/bui", "crates/bui-c"]

[workspace.package]
edition = "2021"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
uuid = { version = "1", features = ["v4", "serde"] }
base64 = "0.22"
percent-encoding = "2"
url = "2"
sha2 = "0.10"
hex = "0.4"
thiserror = "2"
pretty_assertions = "1"
tempfile = "3"

[profile.release]
lto = "fat"
codegen-units = 1
strip = true
```
`rust-toolchain.toml`：`[toolchain]\nchannel = "stable"`。
`crates/bui-schema/Cargo.toml`：
```toml
[package]
name = "bui-schema"
version = "4.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
serde_yaml.workspace = true
uuid.workspace = true
base64.workspace = true
percent-encoding.workspace = true
url.workspace = true
sha2.workspace = true
hex.workspace = true
thiserror.workspace = true

[dev-dependencies]
pretty_assertions.workspace = true
tempfile.workspace = true
```
`crates/bui-schema/src/lib.rs`：
```rust
//! b-ui v4 shared schema: state model, node set, config/subscription renderers, parsers, v3 import.
pub mod keywords;
pub mod model;
pub mod nodes;
pub mod parse;
pub mod paths;
pub mod render;
pub mod v3;
```
先给每个模块建空文件（`pub mod` 需要文件存在）：`keywords.rs`、`model.rs`、`nodes.rs`、`parse/mod.rs`、`paths.rs`、`render/mod.rs`、`v3.rs`，内容只有 `//! placeholder filled by later tasks`。
`crates/bui/Cargo.toml` 与 `crates/bui-c/Cargo.toml`：`[package] name = "bui"`（另一个 `"bui-c"`），`version = "4.0.0"`，`edition.workspace = true`，`[dependencies] bui-schema = { path = "../bui-schema" }`；`src/main.rs`：`fn main() { println!("bui v4 placeholder"); }`（bui-c 同理）。
`.gitignore` 追加 `target/`。

- [ ] **Step 2: 构建与 lint**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: 成功，无输出告警。

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore crates
git commit -m "feat(workspace): v4 Cargo workspace 脚手架（bui-schema / bui / bui-c）"
```

---

### Task 2: 期望态模型（C1 契约）

**Files:**
- Create: `crates/bui-schema/src/model.rs`, `crates/bui-schema/src/paths.rs`
- Test: `crates/bui-schema/src/model.rs`（单元测试模块）

**Interfaces:**
- Produces: 下面全部类型，字段名即 JSON 字段名（`serde(rename_all = "snake_case")` 只用于枚举）；所有 `Option`/`Vec` 字段 `#[serde(default)]`，保证前向兼容。

- [ ] **Step 1: 写失败测试（spec §2.1 的 JSON 能 round-trip）**

在 `model.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE: &str = r#"{
      "schema_version": 1,
      "node": { "id": "8d5a1a1e-3b2c-4d1e-9f00-000000000001", "name": "bwg-rick", "domain": "example.com", "public_ip": "203.0.113.10",
                "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                           "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
                "reality": { "private_key": "priv", "public_key": "pub", "short_ids": ["0123abcd"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
                "obfs": { "enabled": false, "password": "" } },
      "admin": { "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA", "jwt_secret": "00ff" },
      "users": [ { "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa", "username": "alice", "note": "", "created_at": "2026-09-11T00:00:00Z", "disabled": false,
                   "credentials": { "hy2_password": "pw-alice", "vless_uuid": "11111111-1111-4111-8111-111111111111" },
                   "entitlements": { "protocols": ["hysteria2", "reality"], "direct": true, "residential": { "group_id": "default" },
                                     "expires_at": null, "traffic_limit": { "total_bytes": null, "monthly_bytes": null } },
                   "usage": { "total_bytes": 0, "monthly_bytes": 0, "month_key": "2026-09", "last_seen_at": null },
                   "portal_auth": { "password_hash": null, "tokens": [] },
                   "billing": { "currency": "CNY", "balance_minor": 0, "orders": [] } } ],
      "residential": { "groups": { "default": { "enabled": true, "mode": "global", "keywords": null,
                        "upstreams": [ { "id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb", "name": "url-1", "kind": "http", "host": "isp.example.net", "port": 10007,
                                         "username": "u", "password": "p", "priority": 10, "provider": "decodo", "region": "US",
                                         "ports_allowed": [80, 443], "verified": { "ip": "198.51.100.7", "asn": 33667, "org": "Comcast", "country": "US", "at": "2026-09-11T00:00:00Z" } } ],
                        "selected_upstream_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb",
                        "blacklist": { "pins": [ { "rule": { "kind": "domain_suffix", "value": "pay.google.com" }, "note": "", "created_at": "2026-09-11T00:00:00Z" } ],
                                       "auto": [ { "upstream_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb", "rule": { "kind": "port", "value": 5228 },
                                                   "hits": 5, "confirmed_at": "2026-09-11T00:00:00Z", "last_verified_at": "2026-09-11T00:00:00Z", "passes": 0 } ] } } } },
      "system": { "ssh_hardening": true, "static_dns": true, "sysctl_profile": "auto", "firewall": "auto" },
      "versions": { "bui": "4.0.0", "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2", "client_sing_box": "1.13.19" },
      "catalog": [ { "sku": "resi-ip-us", "title": "美国住宅 IP", "kind": "residential_ip", "region": "US", "price_minor": 1500, "period_days": 30 } ]
    }"#;

    #[test]
    fn state_round_trips() {
        let s: State = serde_json::from_str(SAMPLE).expect("parse");
        assert_eq!(s.users[0].entitlements.protocols, vec![Protocol::Hysteria2, Protocol::Reality]);
        assert_eq!(s.residential.groups["default"].upstreams[0].kind, UpstreamKind::Http);
        assert_eq!(s.residential.groups["default"].blacklist.auto[0].rule, Rule::Port(5228));
        let back: serde_json::Value = serde_json::to_value(&s).unwrap();
        let orig: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(back, orig);
    }

    #[test]
    fn missing_optional_sections_default() {
        let minimal = r#"{"schema_version":1,"node":{"id":"8d5a1a1e-3b2c-4d1e-9f00-000000000001","name":"n","domain":"example.com","public_ip":"203.0.113.10",
          "ports":{"hy2":10000,"hy2_hop":null,"hy2_resi":40000,"hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080},
          "reality":{"private_key":"a","public_key":"b","short_ids":["c"],"dest":"www.bing.com:443","server_names":["www.bing.com"]},"obfs":{"enabled":false,"password":""}},
          "admin":{"password_hash":"h","jwt_secret":"s"}}"#;
        let s: State = serde_json::from_str(minimal).unwrap();
        assert!(s.users.is_empty());
        assert!(s.residential.groups.contains_key("default"));
        assert_eq!(s.residential.groups["default"].mode, ResiMode::Split);
        assert!(s.system.static_dns);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui-schema model::` — Expected: 编译失败（类型不存在）。

- [ ] **Step 3: 写模型**

`crates/bui-schema/src/paths.rs`：
```rust
use std::path::PathBuf;

/// Absolute paths the renderers embed into kernel configs.
#[derive(Debug, Clone)]
pub struct Paths {
    pub base_dir: PathBuf,   // /opt/b-ui
    pub certs_dir: PathBuf,  // /opt/b-ui/certs
    pub bin_dir: PathBuf,    // /opt/b-ui/bin
}
impl Paths {
    pub fn default_server() -> Self {
        Self { base_dir: "/opt/b-ui".into(), certs_dir: "/opt/b-ui/certs".into(), bin_dir: "/opt/b-ui/bin".into() }
    }
}
```
`crates/bui-schema/src/model.rs`（完整）：
```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_GROUP: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub schema_version: u32,
    pub node: NodeParams,
    pub admin: Admin,
    #[serde(default)] pub users: Vec<User>,
    #[serde(default)] pub residential: Residential,
    #[serde(default)] pub system: SystemSettings,
    #[serde(default)] pub versions: Versions,
    #[serde(default)] pub catalog: Vec<CatalogItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeParams {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub public_ip: String,
    pub ports: Ports,
    pub reality: Reality,
    #[serde(default)] pub obfs: Obfs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ports {
    pub hy2: u16,
    #[serde(default)] pub hy2_hop: Option<(u16, u16)>,
    pub hy2_resi: u16,
    pub hy2_resi_hop: (u16, u16),
    pub reality_direct: u16,
    pub reality_resi: u16,
    pub admin: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reality {
    pub private_key: String,
    pub public_key: String,
    pub short_ids: Vec<String>,
    pub dest: String,          // "www.bing.com:443"
    pub server_names: Vec<String>,
}
impl Reality {
    /// SNI = dest 去掉端口（v3 getConfig 语义）
    pub fn sni(&self) -> &str { self.dest.split(':').next().unwrap_or(&self.dest) }
    pub fn short_id(&self) -> &str { self.short_ids.first().map(String::as_str).unwrap_or("") }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Obfs { #[serde(default)] pub enabled: bool, #[serde(default)] pub password: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Admin { pub password_hash: String, pub jwt_secret: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub user_id: Uuid,
    pub username: String,
    #[serde(default)] pub note: String,
    pub created_at: String,
    #[serde(default)] pub disabled: bool,
    pub credentials: Credentials,
    pub entitlements: Entitlements,
    #[serde(default)] pub usage: Usage,
    #[serde(default)] pub portal_auth: PortalAuth,
    #[serde(default)] pub billing: Billing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials { pub hy2_password: String, pub vless_uuid: Uuid }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol { Hysteria2, Reality }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entitlements {
    pub protocols: Vec<Protocol>,
    #[serde(default = "default_true")] pub direct: bool,
    #[serde(default)] pub residential: Option<ResidentialEntitlement>,
    #[serde(default)] pub expires_at: Option<String>,
    #[serde(default)] pub traffic_limit: TrafficLimit,
}
fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResidentialEntitlement { pub group_id: String }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrafficLimit { #[serde(default)] pub total_bytes: Option<u64>, #[serde(default)] pub monthly_bytes: Option<u64> }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default)] pub total_bytes: u64,
    #[serde(default)] pub monthly_bytes: u64,
    #[serde(default)] pub month_key: String,
    #[serde(default)] pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PortalAuth { #[serde(default)] pub password_hash: Option<String>, #[serde(default)] pub tokens: Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Billing { pub currency: String, #[serde(default)] pub balance_minor: i64, #[serde(default)] pub orders: Vec<Order> }
impl Default for Billing { fn default() -> Self { Self { currency: "CNY".into(), balance_minor: 0, orders: vec![] } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub order_id: Uuid, pub sku: String, pub amount_minor: i64, pub status: OrderStatus,
    #[serde(default)] pub external_ref: Option<String>, pub created_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus { Pending, Paid, Fulfilled, Cancelled }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Residential { #[serde(default)] pub groups: BTreeMap<String, ResidentialGroup> }
impl Default for Residential {
    fn default() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert(DEFAULT_GROUP.to_string(), ResidentialGroup::default());
        Self { groups }
    }
}
impl Residential { pub fn default_group(&self) -> Option<&ResidentialGroup> { self.groups.get(DEFAULT_GROUP) } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResidentialGroup {
    #[serde(default)] pub enabled: bool,
    #[serde(default)] pub mode: ResiMode,
    #[serde(default)] pub keywords: Option<Vec<String>>,
    #[serde(default)] pub upstreams: Vec<Upstream>,
    #[serde(default)] pub selected_upstream_id: Option<Uuid>,
    #[serde(default)] pub blacklist: Blacklist,
}
impl Default for ResidentialGroup {
    fn default() -> Self { Self { enabled: false, mode: ResiMode::Split, keywords: None, upstreams: vec![], selected_upstream_id: None, blacklist: Blacklist::default() } }
}
impl ResidentialGroup {
    /// 池有效 = enabled 且至少一个上游（fail-open 判据）
    pub fn pool_active(&self) -> bool { self.enabled && !self.upstreams.is_empty() }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResiMode { Global, #[default] Split }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Upstream {
    pub id: Uuid,
    pub name: String,
    pub kind: UpstreamKind,
    pub host: String,
    pub port: u16,
    #[serde(default)] pub username: String,
    #[serde(default)] pub password: String,
    #[serde(default = "default_priority")] pub priority: u32,
    #[serde(default)] pub provider: Option<String>,
    #[serde(default)] pub region: Option<String>,
    #[serde(default)] pub ports_allowed: Option<Vec<u16>>,
    #[serde(default)] pub verified: Option<Verified>,
}
fn default_priority() -> u32 { 100 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamKind { Socks5, Http }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verified {
    pub ip: String, #[serde(default)] pub asn: Option<u32>, #[serde(default)] pub org: Option<String>,
    #[serde(default)] pub country: Option<String>, pub at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Blacklist { #[serde(default)] pub pins: Vec<Pin>, #[serde(default)] pub auto: Vec<AutoEntry> }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Rule { DomainSuffix(String), Domain(String), Port(u16) }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin { pub rule: Rule, #[serde(default)] pub note: String, pub created_at: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoEntry {
    pub upstream_id: Uuid, pub rule: Rule, #[serde(default)] pub hits: u64,
    pub confirmed_at: String, pub last_verified_at: String, #[serde(default)] pub passes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemSettings {
    #[serde(default = "default_true")] pub ssh_hardening: bool,
    #[serde(default = "default_true")] pub static_dns: bool,
    #[serde(default = "default_auto")] pub sysctl_profile: String,
    #[serde(default = "default_auto")] pub firewall: String,
}
fn default_auto() -> String { "auto".into() }
impl Default for SystemSettings { fn default() -> Self { Self { ssh_hardening: true, static_dns: true, sysctl_profile: "auto".into(), firewall: "auto".into() } } }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Versions {
    #[serde(default)] pub bui: String, #[serde(default)] pub hysteria: String, #[serde(default)] pub xray: String,
    #[serde(default)] pub sing_box: String, #[serde(default)] pub caddy: String, #[serde(default)] pub client_sing_box: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogItem {
    pub sku: String, pub title: String, pub kind: CatalogKind,
    #[serde(default)] pub region: Option<String>, pub price_minor: i64, #[serde(default)] pub period_days: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind { ResidentialIp, Plan }
```
注意 `Rule` 的 serde 表示：`{"kind":"port","value":5228}` —— `#[serde(tag="kind", content="value")]` 恰好产生这个形状。

- [ ] **Step 4: 测试通过**

Run: `cargo test -p bui-schema model::` — Expected: 2 passed。

- [ ] **Step 5: Commit**

```bash
git add crates/bui-schema/src/model.rs crates/bui-schema/src/paths.rs
git commit -m "feat(schema): v4 期望态模型（State/User/Entitlements/Residential/Blacklist）"
```

---

### Task 3: v3 状态导入

**Files:**
- Create: `crates/bui-schema/src/v3.rs`, `crates/bui-schema/tests/fixtures/v3/src/{users.json,config.yaml,config-residential.yaml,reality-keys.json,xray-config.json,residential-proxy.json,admin.env,certs/.domain}`
- Test: `crates/bui-schema/tests/v3_import.rs`

**Interfaces:**
- Consumes: `model::*`
- Produces: `pub fn import(dir: &Path) -> Result<State, ImportError>`；`pub enum ImportError { Io(std::io::Error), Json(serde_json::Error), Missing(&'static str), Invalid(String) }`（`thiserror`）。

- [ ] **Step 1: 写合成 v3 fixture（与 Task 5 共用）**

`tests/fixtures/v3/src/users.json`（v3 真实字段：`limits.{expiresAt,trafficLimit,monthlyLimit,speedLimit}`，`usage.{total, monthly:{"YYYY-MM":bytes}}`，`residential` 布尔，`protocol`）：
```json
[
 {"username":"alice","password":"pw-alice-01","uuid":"11111111-1111-4111-8111-111111111111","sni":"www.bing.com","protocol":"fusion","residential":true,"createdAt":"2026-05-01T00:00:00.000Z","limits":{"speedLimit":100000000},"usage":{"total":123456,"monthly":{"2026-09":2345}}},
 {"username":"bob","password":"pw-bob-02","uuid":"22222222-2222-4222-8222-222222222222","sni":"www.bing.com","protocol":"hysteria2","residential":true,"createdAt":"2026-05-02T00:00:00.000Z","limits":{"expiresAt":"2027-01-01T00:00:00.000Z","trafficLimit":107374182400},"usage":{"total":0,"monthly":{}}},
 {"username":"carol","password":"pw-carol-03","uuid":"33333333-3333-4333-8333-333333333333","sni":"www.bing.com","protocol":"vless-reality","residential":true,"createdAt":"2026-05-03T00:00:00.000Z","limits":{"monthlyLimit":53687091200},"usage":{"total":10,"monthly":{"2026-08":5,"2026-09":7}}},
 {"username":"dave","password":"pw-dave-04","uuid":"44444444-4444-4444-8444-444444444444","sni":"www.bing.com","protocol":"fusion","residential":false,"createdAt":"2026-05-04T00:00:00.000Z","limits":{},"usage":{"total":0,"monthly":{}}}
]
```
`config.yaml`：从 `server/core.sh:527-580` 的模板抄，`listen: :10000,20000-30000`，证书路径 `/opt/b-ui/certs/…`，**不含** obfs 段；`config-residential.yaml` 从 `core.sh:595-660` 抄，`listen: :40000,41000-50000`。
`reality-keys.json`：先执行 `xray x25519` 取一对密钥，`shortId` 用 `0123456789abcdef`：`{"privateKey":"<priv>","publicKey":"<pub>","shortId":"0123456789abcdef"}`。
`xray-config.json`：从 `core.sh:775-841` 模板抄，四个用户的 `clients` 都填进两个 inbound，`dest` `www.bing.com:443`，`privateKey` 用上面的私钥，`shortIds` `["0123456789abcdef"]`。
`residential-proxy.json`：`{"enabled":true,"global":true,"domains":null,"urls":[{"host":"isp.example.net","port":10007,"username":"u1","password":"p1","type":"http","name":"url-1","lastVerifiedIp":"198.51.100.7"},{"host":"socks.example.net","port":1080,"username":"u2","password":"p2","type":"socks5","name":"url-2","lastVerifiedIp":"198.51.100.8"}]}`。
`admin.env`：`ADMIN_PASSWORD=test123`。`certs/.domain`：`example.com`。

- [ ] **Step 2: 写失败测试**

`tests/v3_import.rs`：
```rust
use bui_schema::model::*;
use std::path::Path;

fn fixture() -> &'static Path { Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v3/src")) }

#[test]
fn imports_node_params() {
    let s = bui_schema::v3::import(fixture()).unwrap();
    assert_eq!(s.schema_version, SCHEMA_VERSION);
    assert_eq!(s.node.domain, "example.com");
    assert_eq!(s.node.ports.hy2, 10000);
    assert_eq!(s.node.ports.hy2_hop, Some((20000, 30000)));
    assert_eq!(s.node.ports.hy2_resi, 40000);
    assert_eq!(s.node.ports.hy2_resi_hop, (41000, 50000));
    assert_eq!(s.node.reality.dest, "www.bing.com:443");
    assert_eq!(s.node.reality.short_ids, vec!["0123456789abcdef".to_string()]);
    assert!(!s.node.obfs.enabled);
}

#[test]
fn maps_users_and_limits() {
    let s = bui_schema::v3::import(fixture()).unwrap();
    let alice = s.users.iter().find(|u| u.username == "alice").unwrap();
    assert_eq!(alice.entitlements.protocols, vec![Protocol::Hysteria2, Protocol::Reality]);
    assert!(alice.entitlements.direct);
    assert_eq!(alice.entitlements.residential.as_ref().unwrap().group_id, "default");
    assert_eq!(alice.usage.total_bytes, 123456);
    assert_eq!(alice.usage.month_key, "2026-09");
    assert_eq!(alice.usage.monthly_bytes, 2345);
    let bob = s.users.iter().find(|u| u.username == "bob").unwrap();
    assert_eq!(bob.entitlements.protocols, vec![Protocol::Hysteria2]);
    assert_eq!(bob.entitlements.expires_at.as_deref(), Some("2027-01-01T00:00:00.000Z"));
    assert_eq!(bob.entitlements.traffic_limit.total_bytes, Some(107374182400));
    let carol = s.users.iter().find(|u| u.username == "carol").unwrap();
    assert_eq!(carol.entitlements.protocols, vec![Protocol::Reality]);
    assert_eq!(carol.entitlements.traffic_limit.monthly_bytes, Some(53687091200));
    let dave = s.users.iter().find(|u| u.username == "dave").unwrap();
    assert!(dave.entitlements.residential.is_none());
    assert_eq!(s.users.len(), 4);
}

#[test]
fn maps_residential_pool() {
    let s = bui_schema::v3::import(fixture()).unwrap();
    let g = s.residential.default_group().unwrap();
    assert!(g.enabled);
    assert_eq!(g.mode, ResiMode::Global);
    assert_eq!(g.keywords, None);
    assert_eq!(g.upstreams.len(), 2);
    assert_eq!(g.upstreams[0].kind, UpstreamKind::Http);
    assert_eq!(g.upstreams[1].kind, UpstreamKind::Socks5);
    assert_eq!(g.upstreams[0].verified.as_ref().unwrap().ip, "198.51.100.7");
    assert_eq!(g.selected_upstream_id, Some(g.upstreams[0].id));
}

#[test]
fn admin_password_is_hashed_not_stored() {
    let s = bui_schema::v3::import(fixture()).unwrap();
    assert!(s.admin.password_hash.starts_with("$argon2id$"));
    assert_ne!(s.admin.jwt_secret, "");
}
```
`month_key` 的规则：取 `usage.monthly` 里**字典序最大**的键（导入时最近的月份）；没有则空串、0。

- [ ] **Step 3: 运行确认失败** — `cargo test -p bui-schema --test v3_import`，Expected：`v3::import` 未定义。

- [ ] **Step 4: 实现 `v3.rs`**

依赖：在 `bui-schema/Cargo.toml` 加 `argon2 = "0.5"`、`rand = "0.8"`（argon2 盐与 jwt_secret）。要点：
- `listen` 行解析：正则语义 `^listen:\s*:?(\d+)(?:,(\d+)-(\d+))?\s*$`（与 `web/server.js:445-448` 一致），用手写解析（`split(',')`、`split('-')`），不引入 regex。
- obfs：`config.yaml` 顶层 `obfs:` 块内 `type: salamander` 与 `password:`（`server.js:449-465` 语义）。
- `protocol` 映射：`fusion`→`[Hysteria2, Reality]`；`hysteria2`→`[Hysteria2]`；`vless-reality`→`[Reality]`；其它（含 `vless-ws-tls`）→ `Err(Invalid)` 并在错误里带用户名。
- `residential !== false` → `Some(ResidentialEntitlement{group_id:"default"})`。
- `limits.expiresAt` → `expires_at`；`trafficLimit` → `total_bytes`；`monthlyLimit` → `monthly_bytes`；`speedLimit` 丢弃。
- `usage.total` → `total_bytes`；`usage.monthly` 最大键 → `month_key`/`monthly_bytes`。
- `user_id` = `Uuid::new_v4()`；`created_at` = `createdAt`，缺则 `"1970-01-01T00:00:00Z"`。
- 住宅：`enabled`、`global` → `ResiMode::Global` 否则 `Split`；`domains` null 或空数组 → `keywords: None`，非空 → `Some`；`urls[]` → `Upstream{ id: new_v4, name, kind: type=="http"?Http:Socks5（缺省 socks5）, host, port, username, password, priority: 100, verified: lastVerifiedIp.map(...) }`；`selected_upstream_id` = 第一个上游。
- `admin.env` 的 `ADMIN_PASSWORD` → argon2id 哈希（`Argon2::default().hash_password(pw, &SaltString::generate(&mut OsRng))`）；`jwt_secret` = 32 字节随机 hex。
- `versions` 留空由 P1 填；`catalog` 空。
- 缺文件：`users.json`/`config.yaml`/`reality-keys.json`/`xray-config.json`/`certs/.domain` 任一缺失 → `Missing`；`residential-proxy.json`、`admin.env`、`config-residential.yaml` 可缺（分别默认：空组、随机管理员密码并在返回值旁打印警告——用 `ImportReport { state, warnings: Vec<String> }` 返回，测试断言 `warnings` 为空）。

- [ ] **Step 5: 测试通过** — `cargo test -p bui-schema --test v3_import`，Expected：4 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui-schema/src/v3.rs crates/bui-schema/tests/v3_import.rs crates/bui-schema/tests/fixtures/v3/src crates/bui-schema/Cargo.toml Cargo.lock
git commit -m "feat(schema): v3 状态导入（users/config/reality/residential/admin → State）"
```

---

### Task 4: 权益 → 节点集合 `nodes_for`

**Files:**
- Create: `crates/bui-schema/src/nodes.rs`
- Test: 同文件单元测试

**Interfaces:**
- Consumes: `model::{User, NodeParams, Residential, Protocol}`
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum NodeKind { RealityDirect, RealityResidential, Hy2Direct, Hy2Residential }
#[derive(Debug, Clone, PartialEq)] pub enum Transport {
    Hysteria2 { username: String, password: String, sni: String, obfs_password: Option<String> },
    Reality   { uuid: uuid::Uuid, public_key: String, short_id: String, server_name: String, fingerprint: String /* "chrome" */, flow: String /* "xtls-rprx-vision" */ },
}
#[derive(Debug, Clone, PartialEq)] pub struct Node { pub kind: NodeKind, pub label: String, pub host: String, pub port: u16, pub hop: Option<(u16,u16)>, pub transport: Transport }
impl Node { pub fn name(&self, username: &str) -> String /* "{username}-{label}" */ }
pub fn nodes_for(user: &User, node: &NodeParams, resi: &Residential) -> Vec<Node>
```
顺序固定为 v3 `/api/sub` 的顺序：Reality直连、Reality住宅、HY2直连、HY2住宅（其它渲染器若需要别的顺序自行重排）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    fn node() -> NodeParams { serde_json::from_str(r#"{"id":"8d5a1a1e-3b2c-4d1e-9f00-000000000001","name":"n","domain":"example.com","public_ip":"203.0.113.10",
        "ports":{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080},
        "reality":{"private_key":"a","public_key":"PUB","short_ids":["0123456789abcdef"],"dest":"www.bing.com:443","server_names":["www.bing.com"]},
        "obfs":{"enabled":true,"password":"obfs-pw"}}"#).unwrap() }
    fn user(protocols: Vec<Protocol>, resi: bool) -> User {
        let mut u: User = serde_json::from_str(r#"{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000aa","username":"alice","created_at":"2026-09-11T00:00:00Z",
            "credentials":{"hy2_password":"pw","vless_uuid":"11111111-1111-4111-8111-111111111111"},
            "entitlements":{"protocols":[],"direct":true,"residential":{"group_id":"default"}}}"#).unwrap();
        u.entitlements.protocols = protocols;
        if !resi { u.entitlements.residential = None; }
        u
    }
    #[test] fn fusion_gives_four_in_v3_order() {
        let ns = nodes_for(&user(vec![Protocol::Hysteria2, Protocol::Reality], true), &node(), &Residential::default());
        let kinds: Vec<_> = ns.iter().map(|n| n.kind).collect();
        assert_eq!(kinds, vec![NodeKind::RealityDirect, NodeKind::RealityResidential, NodeKind::Hy2Direct, NodeKind::Hy2Residential]);
        assert_eq!(ns.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["Reality直连", "Reality住宅", "HY2直连", "HY2住宅"]);
        assert_eq!(ns[2].port, 10000); assert_eq!(ns[2].hop, Some((20000, 30000)));
        assert_eq!(ns[3].port, 40000); assert_eq!(ns[3].hop, Some((41000, 50000)));
        match &ns[2].transport { Transport::Hysteria2 { obfs_password, sni, .. } => { assert_eq!(obfs_password.as_deref(), Some("obfs-pw")); assert_eq!(sni, "example.com"); } _ => panic!() }
        match &ns[3].transport { Transport::Hysteria2 { obfs_password, .. } => assert_eq!(obfs_password, &None), _ => panic!() } // v3: 住宅 HY2 不带 obfs
        match &ns[0].transport { Transport::Reality { server_name, short_id, fingerprint, flow, .. } => { assert_eq!(server_name, "www.bing.com"); assert_eq!(short_id, "0123456789abcdef"); assert_eq!(fingerprint, "chrome"); assert_eq!(flow, "xtls-rprx-vision"); } _ => panic!() }
        assert_eq!(ns[0].name("alice"), "alice-Reality直连");
    }
    #[test] fn hysteria2_only_with_residential_gives_two() {
        let ns = nodes_for(&user(vec![Protocol::Hysteria2], true), &node(), &Residential::default());
        assert_eq!(ns.iter().map(|n| n.kind).collect::<Vec<_>>(), vec![NodeKind::Hy2Direct, NodeKind::Hy2Residential]);
    }
    #[test] fn no_residential_entitlement_gives_direct_only() {
        let ns = nodes_for(&user(vec![Protocol::Hysteria2, Protocol::Reality], false), &node(), &Residential::default());
        assert_eq!(ns.iter().map(|n| n.kind).collect::<Vec<_>>(), vec![NodeKind::RealityDirect, NodeKind::Hy2Direct]);
    }
    #[test] fn direct_false_gives_residential_only() {
        let mut u = user(vec![Protocol::Reality], true); u.entitlements.direct = false;
        let ns = nodes_for(&u, &node(), &Residential::default());
        assert_eq!(ns.iter().map(|n| n.kind).collect::<Vec<_>>(), vec![NodeKind::RealityResidential]);
    }
    #[test] fn no_hop_when_disabled() {
        let mut n = node(); n.ports.hy2_hop = None; n.obfs.enabled = false;
        let ns = nodes_for(&user(vec![Protocol::Hysteria2], false), &n, &Residential::default());
        assert_eq!(ns[0].hop, None);
        match &ns[0].transport { Transport::Hysteria2 { obfs_password, .. } => assert_eq!(obfs_password, &None), _ => panic!() }
    }
}
```
注意 v3 差异：v3 单协议 `hysteria2`/`vless-reality` 且 `residential=true` 时**只给住宅版**一个节点（`server.js:1857-1862`、`1893-1896`）。v4 权益模型下 `direct=true` 表示直连也给；导入时 v3 单协议用户的 `direct` 统一设为 `true`（多给一个直连节点是能力扩展，不影响原节点），**但 golden 比对只针对 v3 有的节点**——见 Task 6 的比对规则。

- [ ] **Step 2: 运行确认失败** — `cargo test -p bui-schema nodes::`。

- [ ] **Step 3: 实现**

```rust
pub fn nodes_for(user: &User, node: &NodeParams, resi: &Residential) -> Vec<Node> {
    let e = &user.entitlements;
    let has = |p: Protocol| e.protocols.contains(&p);
    let resi_ok = e.residential.as_ref().map(|r| resi.groups.contains_key(&r.group_id)).unwrap_or(false);
    let sni = node.domain.clone();
    let obfs = if node.obfs.enabled && !node.obfs.password.is_empty() { Some(node.obfs.password.clone()) } else { None };
    let reality = |port: u16, kind: NodeKind, label: &str| Node {
        kind, label: label.into(), host: node.domain.clone(), port, hop: None,
        transport: Transport::Reality { uuid: user.credentials.vless_uuid, public_key: node.reality.public_key.clone(),
            short_id: node.reality.short_id().to_string(), server_name: node.reality.sni().to_string(),
            fingerprint: "chrome".into(), flow: "xtls-rprx-vision".into() },
    };
    let hy2 = |port: u16, hop: Option<(u16,u16)>, kind: NodeKind, label: &str, obfs_password: Option<String>| Node {
        kind, label: label.into(), host: node.domain.clone(), port, hop,
        transport: Transport::Hysteria2 { username: user.username.clone(), password: user.credentials.hy2_password.clone(), sni: sni.clone(), obfs_password },
    };
    let mut out = Vec::with_capacity(4);
    if has(Protocol::Reality) {
        if e.direct { out.push(reality(node.ports.reality_direct, NodeKind::RealityDirect, "Reality直连")); }
        if resi_ok { out.push(reality(node.ports.reality_resi, NodeKind::RealityResidential, "Reality住宅")); }
    }
    if has(Protocol::Hysteria2) {
        if e.direct { out.push(hy2(node.ports.hy2, node.ports.hy2_hop, NodeKind::Hy2Direct, "HY2直连", obfs.clone())); }
        if resi_ok { out.push(hy2(node.ports.hy2_resi, Some(node.ports.hy2_resi_hop), NodeKind::Hy2Residential, "HY2住宅", None)); }
    }
    out
}
```

- [ ] **Step 4: 测试通过**；**Step 5: Commit** `feat(schema): 权益 → 节点集合 nodes_for`。

---

### Task 5: 用 v3 生成 golden fixtures

**Files:**
- Create: `scripts/gen-v3-fixtures.sh`, `crates/bui-schema/tests/fixtures/v3/expected/**`
- Test: 无（产物供 Task 6 使用）

**Interfaces:**
- Produces: `expected/<mode>/<user>.{sub.txt,singbox.json,clash.yaml}`，`mode ∈ {global, split, obfs}`；`expected/README.md` 记录生成命令与 v3 commit。

- [ ] **Step 1: 写脚本**

```bash
#!/usr/bin/env bash
# 用 v3 的 web/server.js 以合成数据生成订阅 golden 样本。只读 v3 源码，不动生产。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FX="$ROOT/crates/bui-schema/tests/fixtures/v3"
SRC="$FX/src"; OUT="$FX/expected"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"; kill ${PID:-} 2>/dev/null || true' EXIT
PORT=18090
run_mode() {  # $1 = mode name; 调用前已把 $WORK/base 准备好
  local mode="$1"
  ( cd "$ROOT/web" && BASE_DIR="$WORK/base" ADMIN_DIR="$ROOT/web" ADMIN_PORT=$PORT ADMIN_PASSWORD=test123 SERVER_IP=203.0.113.10 node server.js >"$WORK/server-$mode.log" 2>&1 ) & PID=$!
  for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/api/sub/alice" >/dev/null 2>&1 && break; sleep 0.2; done
  mkdir -p "$OUT/$mode"
  for u in alice bob carol dave; do
    curl -sf "http://127.0.0.1:$PORT/api/sub/$u"          > "$OUT/$mode/$u.sub.txt"
    curl -sf "http://127.0.0.1:$PORT/api/subscription/$u" | python3 -m json.tool > "$OUT/$mode/$u.singbox.json"
    curl -sf "http://127.0.0.1:$PORT/api/clash/$u"        > "$OUT/$mode/$u.clash.yaml"
  done
  kill $PID; wait $PID 2>/dev/null || true; PID=
}
prep() {  # 复制 src 到 $WORK/base，server.js 需要 residential-helper.sh 同目录
  rm -rf "$WORK/base"; mkdir -p "$WORK/base/certs"
  cp "$SRC"/users.json "$SRC"/config.yaml "$SRC"/config-residential.yaml "$SRC"/reality-keys.json "$SRC"/xray-config.json "$WORK/base/"
  cp "$SRC/certs/.domain" "$WORK/base/certs/.domain"
  cp "$ROOT/server/residential-helper.sh" "$WORK/base/residential-helper.sh"; chmod +x "$WORK/base/residential-helper.sh"
}
# mode global：residential-proxy.json 原样（global=true）
prep; cp "$SRC/residential-proxy.json" "$WORK/base/"; run_mode global
# mode split：global=false，domains=null（跟随默认表）
prep; python3 - "$SRC/residential-proxy.json" "$WORK/base/residential-proxy.json" <<'PY'
import json,sys; d=json.load(open(sys.argv[1])); d["global"]=False; d["domains"]=None; json.dump(d,open(sys.argv[2],"w"))
PY
run_mode split
# mode obfs：global=true + config.yaml 顶部插入 obfs 段（与 b-ui-cli.sh cmd_obfs 相同格式）
prep; cp "$SRC/residential-proxy.json" "$WORK/base/"
{ printf 'obfs:\n  type: salamander\n  salamander:\n    password: obfs-pw-test\n'; cat "$SRC/config.yaml"; } > "$WORK/base/config.yaml"
run_mode obfs
{ echo "# v3 golden fixtures"; echo "生成命令: scripts/gen-v3-fixtures.sh"; echo "v3 commit: $(git -C "$ROOT" rev-parse --short HEAD)"; echo "生成时间: $(date -u +%FT%TZ)"; } > "$OUT/README.md"
echo "OK: $OUT"
```
说明：`server.js` 用 `residential-helper.sh domains` 取默认关键字表（`server.js:492-506`），所以要拷 helper；`obfs` 段格式见 `server/b-ui-cli.sh:940-953`（如与上面 `printf` 不同，以 cli 为准并改脚本）。

- [ ] **Step 2: 运行并检查产物**

Run: `bash scripts/gen-v3-fixtures.sh && ls crates/bui-schema/tests/fixtures/v3/expected/*/ && base64 -d crates/bui-schema/tests/fixtures/v3/expected/global/alice.sub.txt`
Expected: 3 个模式 × 4 用户 × 3 文件；alice 解码后 4 行：`vless://…#alice-Reality%E7%9B%B4%E8%BF%9E`、`vless://…10002…`、`hysteria2://alice:pw-alice-01@example.com:10000?sni=example.com&insecure=0&mport=20000-30000#…`、`hysteria2://…40000…mport=41000-50000…`；obfs 模式的 10000 那行多 `&obfs=salamander&obfs-password=obfs-pw-test`，40000 那行没有。`sing-box check -c expected/global/alice.singbox.json` 通过。

- [ ] **Step 3: Commit**

```bash
git add scripts/gen-v3-fixtures.sh crates/bui-schema/tests/fixtures/v3/expected
git commit -m "test(schema): 用 v3 server.js 生成订阅 golden 样本（global/split/obfs × 4 用户）"
```

---

### Task 6: 订阅渲染器（三种）+ golden 比对

**Files:**
- Create: `crates/bui-schema/src/render/mod.rs`, `crates/bui-schema/src/render/subscription.rs`, `crates/bui-schema/tests/common/mod.rs`, `crates/bui-schema/tests/golden_subscription.rs`
- Modify: `crates/bui-schema/src/lib.rs`（已 `pub mod render`）

**Interfaces:**
- Consumes: `nodes::{Node, nodes_for}`, `model::*`, `keywords::DEFAULT_KEYWORDS`（Task 12）
- Produces:
```rust
// render/mod.rs
pub struct SplitRules { pub enabled: bool /* 住宅池有效 */, pub global: bool, pub keywords: Vec<String> }
impl SplitRules { pub fn from_group(g: &ResidentialGroup) -> Self }   // keywords: g.keywords 或 DEFAULT_KEYWORDS；enabled = g.pool_active()
// render/subscription.rs
pub fn uri_list(nodes: &[Node], username: &str) -> String         // base64(lines.join("\n"))，行格式与 v3 一致
pub fn singbox(nodes: &[Node], username: &str, split: &SplitRules) -> serde_json::Value
pub fn clash(nodes: &[Node], username: &str, split: &SplitRules) -> String
```

- [ ] **Step 1: 写 golden 测试**

`tests/common/mod.rs`：
```rust
use bui_schema::model::*;
use std::path::{Path, PathBuf};
pub fn fx() -> PathBuf { PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v3")) }
pub fn state(mode: &str) -> State {
    let mut s = bui_schema::v3::import(&fx().join("src")).unwrap().state;
    let g = s.residential.groups.get_mut("default").unwrap();
    match mode { "global" | "obfs" => g.mode = ResiMode::Global, "split" => g.mode = ResiMode::Split, _ => unreachable!() }
    if mode == "obfs" { s.node.obfs = Obfs { enabled: true, password: "obfs-pw-test".into() }; }
    s
}
pub fn expected(mode: &str, user: &str, kind: &str) -> String { std::fs::read_to_string(fx().join("expected").join(mode).join(format!("{user}.{kind}"))).unwrap() }
pub fn have(bin: &str) -> bool { std::process::Command::new(bin).arg("version").output().map(|o| o.status.success()).unwrap_or(false) }
pub fn v3_nodes_only(nodes: Vec<bui_schema::nodes::Node>, user: &User) -> Vec<bui_schema::nodes::Node> {
    // v3 单协议 + residential=true 只有住宅版；v4 多给直连版。golden 比对只看 v3 有的节点。
    let single = user.entitlements.protocols.len() == 1 && user.entitlements.residential.is_some();
    if single { nodes.into_iter().filter(|n| matches!(n.kind, bui_schema::nodes::NodeKind::RealityResidential | bui_schema::nodes::NodeKind::Hy2Residential)).collect() } else { nodes }
}
```
`tests/golden_subscription.rs`：
```rust
mod common;
use base64::Engine;
use bui_schema::{nodes::nodes_for, render::{subscription, SplitRules}};
use pretty_assertions::assert_eq;

const USERS: [&str; 4] = ["alice", "bob", "carol", "dave"];
const MODES: [&str; 3] = ["global", "split", "obfs"];

#[test]
fn uri_list_matches_v3() {
    for mode in MODES { let s = common::state(mode); for u in USERS {
        let user = s.users.iter().find(|x| x.username == u).unwrap();
        let nodes = common::v3_nodes_only(nodes_for(user, &s.node, &s.residential), user);
        let got = base64::engine::general_purpose::STANDARD.decode(subscription::uri_list(&nodes, u)).unwrap();
        let want = base64::engine::general_purpose::STANDARD.decode(common::expected(mode, u, "sub.txt").trim()).unwrap();
        assert_eq!(String::from_utf8(got).unwrap(), String::from_utf8(want).unwrap(), "mode={mode} user={u}");
    } }
}

#[test]
fn singbox_matches_v3() {
    for mode in MODES { let s = common::state(mode); let split = SplitRules::from_group(s.residential.default_group().unwrap()); for u in USERS {
        let user = s.users.iter().find(|x| x.username == u).unwrap();
        let nodes = common::v3_nodes_only(nodes_for(user, &s.node, &s.residential), user);
        let got = subscription::singbox(&nodes, u, &split);
        let want: serde_json::Value = serde_json::from_str(&common::expected(mode, u, "singbox.json")).unwrap();
        assert_eq!(got, want, "mode={mode} user={u}");
    } }
}

#[test]
fn clash_matches_v3() {
    for mode in MODES { let s = common::state(mode); let split = SplitRules::from_group(s.residential.default_group().unwrap()); for u in USERS {
        let user = s.users.iter().find(|x| x.username == u).unwrap();
        let nodes = common::v3_nodes_only(nodes_for(user, &s.node, &s.residential), user);
        let got: serde_yaml::Value = serde_yaml::from_str(&subscription::clash(&nodes, u, &split)).unwrap();
        let want: serde_yaml::Value = serde_yaml::from_str(&common::expected(mode, u, "clash.yaml")).unwrap();
        assert_eq!(got, want, "mode={mode} user={u}");
    } }
}
```
`base64`、`serde_yaml` 已是依赖。

- [ ] **Step 2: 运行确认失败** — `cargo test -p bui-schema --test golden_subscription`。

- [ ] **Step 3: 实现三种渲染器（移植 v3）**

移植来源（逐行对照，字段顺序也照抄以便 JSON 语义相等）：
- URI：`web/server.js:1807-1907`（`buildVlessUrl` 的参数串在 1820-1840，`buildHy2Url` 在 1868-1882）。`percent-encoding` 用 `NON_ALPHANUMERIC` 之外与 JS `encodeURIComponent` 相同的保留集：不编码 `A-Za-z0-9-_.!~*'()`。
- sing-box：`web/server.js:650-849 generateSingboxConfig(user, cfg, host)`。其中 `residential-pool` / `direct-pool` 出站、`route.rules` 的关键字规则（`server.js:776-784`）、DNS 段、TUN 段。
- Clash：`web/server.js:850-934 generateClashConfig`。
- `SplitRules`：`enabled=false`（池空/禁用）时 v3 的行为以 fixture 为准（fixture 三个模式池都非空；再补一个断言：`enabled=false` 时 singbox 输出里没有 `residential-pool` 出站——这一条按 `server.js:770-790` 的 `if (resi.enabled)` 分支实现）。
实现时先跑测试看 diff，逐字段对齐；不要为通过测试而在测试里放宽比较。

- [ ] **Step 4: 测试通过** — 3 个测试全绿。

- [ ] **Step 5: Commit** `feat(schema): 三种订阅渲染器与 v3 golden 逐项等价`。

---

### Task 7: Hysteria2 配置渲染器

**Files:**
- Create: `crates/bui-schema/src/render/hysteria.rs`
- Test: `crates/bui-schema/tests/kernel_check.rs`（本任务新建，后续任务追加）

**Interfaces:**
- Produces: `pub fn direct_yaml(node: &NodeParams, paths: &Paths) -> String`、`pub fn residential_yaml(node: &NodeParams, paths: &Paths) -> String`。用 `serde_yaml` 从 `serde_json::json!` 构造的 Value 序列化（字段顺序按 v3 模板）。

- [ ] **Step 1: 写失败测试**

```rust
mod common;
use bui_schema::{paths::Paths, render::hysteria};

#[test]
fn hysteria_direct_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::direct_yaml(&s.node, &Paths::default_server())).unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":10000,20000-30000");
    assert_eq!(y["auth"]["type"].as_str().unwrap(), "command");
    assert_eq!(y["auth"]["command"].as_str().unwrap(), "/opt/b-ui/bin/bui auth-hook");
    assert_eq!(y["trafficStats"]["listen"].as_str().unwrap(), "127.0.0.1:9999");
    assert_eq!(y["outbounds"][0]["direct"]["mode"].as_u64().unwrap(), 4);
    assert_eq!(y["tls"]["cert"].as_str().unwrap(), "/opt/b-ui/certs/fullchain.pem");
    assert_eq!(y["obfs"]["type"].as_str().unwrap(), "salamander");
    assert_eq!(y["obfs"]["salamander"]["password"].as_str().unwrap(), "obfs-pw-test");
    assert!(y["resolver"]["https"]["addr"].as_str().unwrap().contains("1.1.1.1"));
    assert_eq!(y["masquerade"]["type"].as_str().unwrap(), "proxy");
}

#[test]
fn hysteria_direct_without_obfs_or_hop() {
    let mut s = common::state("global"); s.node.ports.hy2_hop = None;
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::direct_yaml(&s.node, &Paths::default_server())).unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":10000");
    assert!(y.get("obfs").is_none());
}

#[test]
fn hysteria_residential_shape() {
    let s = common::state("obfs");
    let y: serde_yaml::Value = serde_yaml::from_str(&hysteria::residential_yaml(&s.node, &Paths::default_server())).unwrap();
    assert_eq!(y["listen"].as_str().unwrap(), ":40000,41000-50000");
    assert_eq!(y["trafficStats"]["listen"].as_str().unwrap(), "127.0.0.1:9998");
    assert_eq!(y["outbounds"][0]["name"].as_str().unwrap(), "relay");
    assert_eq!(y["outbounds"][0]["socks5"]["addr"].as_str().unwrap(), "127.0.0.1:2080");
    assert_eq!(y["acl"]["inline"][0].as_str().unwrap(), "relay(all)");
    assert!(y.get("obfs").is_none(), "v3 住宅实例不带 obfs，与订阅一致");
}
```

- [ ] **Step 2: 运行确认失败**；**Step 3: 实现**：模板来源 `server/core.sh:527-580`（直连）与 `595-660`（住宅）；区别：`auth` 段改为 `command`；`masquerade.proxy.url` 取 `https://{node.reality.sni()}`（v3 用 `MASQUERADE_URL`，装机时就是 `https://<masq_domain>`）；`obfs` 段仅 `node.obfs.enabled` 时输出到顶层（v3 是 cli 插在文件顶部，位置无关）。

- [ ] **Step 4: 测试通过**；**Step 5: Commit** `feat(schema): Hysteria2 直连/住宅配置渲染（auth.command）`。

---

### Task 8: Xray 配置渲染器 + 结构哈希

**Files:**
- Create: `crates/bui-schema/src/render/xray.rs`
- Test: 追加到 `tests/kernel_check.rs`

**Interfaces:**
- Produces: `pub fn config(node: &NodeParams, users: &[User], paths: &Paths) -> serde_json::Value`；`pub fn structural_hash(cfg: &serde_json::Value) -> String`（去掉每个 inbound 的 `settings.clients` 后 `serde_json::to_vec` 的 sha256 hex）。
- 只有 `entitlements.protocols` 含 `Reality` 且 `!disabled` 的用户进 `clients`（`{"id": uuid, "flow": "xtls-rprx-vision", "email": username}`），两个 inbound 同一份。

- [ ] **Step 1: 写失败测试**

```rust
use bui_schema::render::xray;
#[test]
fn xray_config_has_two_reality_inbounds_and_passes_xray_test() {
    let s = common::state("global");
    let cfg = xray::config(&s.node, &s.users, &Paths::default_server());
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(inb.iter().map(|i| i["tag"].as_str().unwrap()).collect::<Vec<_>>(), vec!["api", "vless-direct", "vless-residential"]);
    let clients = inb[1]["settings"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 3, "bob 是 hysteria2-only，不进 xray"); 
    assert_eq!(inb[1]["streamSettings"]["realitySettings"]["dest"].as_str().unwrap(), "www.bing.com:443");
    assert_eq!(cfg["routing"]["rules"][2]["outboundTag"].as_str().unwrap(), "relay");
    if !common::have("xray") { eprintln!("skipped: xray not found"); return; }
    let f = tempfile::NamedTempFile::new().unwrap(); std::fs::write(f.path(), serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let out = std::process::Command::new("xray").args(["run", "-test", "-c"]).arg(f.path()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}
#[test]
fn structural_hash_ignores_clients() {
    let s = common::state("global");
    let a = xray::config(&s.node, &s.users, &Paths::default_server());
    let b = xray::config(&s.node, &s.users[..1], &Paths::default_server());
    assert_eq!(xray::structural_hash(&a), xray::structural_hash(&b));
    let mut s2 = s.clone(); s2.node.reality.dest = "www.apple.com:443".into();
    assert_ne!(xray::structural_hash(&a), xray::structural_hash(&xray::config(&s2.node, &s2.users, &Paths::default_server())));
}
```

- [ ] **Step 2–5**：实现（模板 `server/core.sh:775-841`，逐字段抄，`privateKey` 来自 `node.reality.private_key`，`serverNames` 来自 `node.reality.server_names`）→ 测试通过 → Commit `feat(schema): Xray 双 REALITY inbound 渲染与结构哈希`。

---

### Task 9: relay（sing-box）配置渲染器

**Files:**
- Create: `crates/bui-schema/src/render/relay.rs`
- Test: 追加到 `tests/kernel_check.rs`

**Interfaces:**
- Consumes: `model::{ResidentialGroup, Upstream, Rule}`, `keywords`
- Produces: `pub struct RelayOpts { pub listen_port: u16 /*2080*/, pub api: String /*"127.0.0.1:9091"*/, pub cache_path: String, pub server_ip: Option<String> }`；`pub fn config(g: &ResidentialGroup, opts: &RelayOpts) -> serde_json::Value`。
- 规则顺序（spec §5.4）：`sniff` → 黑名单 `domain_suffix`/`domain` → direct → `ports_allowed` 取反 `port_range` → direct → `udp/53 direct`、`udp/443 reject`、其余 udp direct → 私网与本机 IP direct → split 模式 `domain_keyword → resi-pool`；`final` global=`resi-pool`、split=`direct`；池无效 → 无 resi 出站、`final=direct`、DNS 全 direct。黑名单生效集合 = `pins` ∪ `auto` 中 `upstream_id == selected_upstream_id` 的条目（`Rule::Port` 类进 `port` 规则）。DNS 规则镜像：黑名单域名 → `dns_direct`。

- [ ] **Step 1: 写失败测试**

```rust
use bui_schema::render::relay::{self, RelayOpts};
fn opts() -> RelayOpts { RelayOpts { listen_port: 2080, api: "127.0.0.1:9091".into(), cache_path: "/opt/b-ui/relay-cache.db".into(), server_ip: Some("203.0.113.10".into()) } }
fn check_singbox(cfg: &serde_json::Value) {
    if !common::have("sing-box") { eprintln!("skipped: sing-box not found"); return; }
    let f = tempfile::NamedTempFile::new().unwrap(); std::fs::write(f.path(), serde_json::to_vec_pretty(cfg).unwrap()).unwrap();
    let out = std::process::Command::new("sing-box").args(["check", "-c"]).arg(f.path()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
#[test]
fn relay_global_with_blacklist_and_ports_allowed() {
    let s = common::state("global"); let mut g = s.residential.default_group().unwrap().clone();
    g.upstreams[0].ports_allowed = Some(vec![80, 443]);
    g.blacklist.pins.push(bui_schema::model::Pin { rule: bui_schema::model::Rule::DomainSuffix("pay.google.com".into()), note: "".into(), created_at: "2026-09-11T00:00:00Z".into() });
    let cfg = relay::config(&g, &opts());
    let tags: Vec<_> = cfg["outbounds"].as_array().unwrap().iter().map(|o| o["tag"].as_str().unwrap().to_string()).collect();
    assert_eq!(tags, vec!["resi-1", "resi-2", "resi-pool", "direct"]);
    assert_eq!(cfg["outbounds"][0]["type"], "http"); assert_eq!(cfg["outbounds"][1]["type"], "socks"); assert_eq!(cfg["outbounds"][1]["version"], "5");
    assert_eq!(cfg["route"]["final"], "resi-pool");
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(rules[0]["action"], "sniff");
    assert_eq!(rules[1]["domain_suffix"][0], "pay.google.com"); assert_eq!(rules[1]["outbound"], "direct");
    assert_eq!(rules[2]["port_range"], serde_json::json!(["1:79", "81:442", "444:65535"])); assert_eq!(rules[2]["outbound"], "direct");
    assert!(rules.iter().any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    assert_eq!(cfg["dns"]["rules"][0]["domain_suffix"][0], "pay.google.com"); assert_eq!(cfg["dns"]["rules"][0]["server"], "dns_direct");
    assert_eq!(cfg["dns"]["final"], "dns_resi");
    check_singbox(&cfg);
}
#[test]
fn relay_split_uses_keywords_and_direct_final() {
    let s = common::state("split"); let g = s.residential.default_group().unwrap().clone();
    let cfg = relay::config(&g, &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    let last = cfg["route"]["rules"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["outbound"], "resi-pool"); assert_eq!(last["domain_keyword"].as_array().unwrap().len(), bui_schema::keywords::DEFAULT_KEYWORDS.len());
    check_singbox(&cfg);
}
#[test]
fn relay_fail_open_when_pool_empty() {
    let mut g = bui_schema::model::ResidentialGroup::default(); g.enabled = true;
    let cfg = relay::config(&g, &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert!(cfg["outbounds"].as_array().unwrap().iter().all(|o| o["tag"] != "resi-pool"));
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    check_singbox(&cfg);
}
```

- [ ] **Step 2–5**：实现（移植 `server/residential-helper.sh:357-460`（池模式）与 `462-505`（直连模式），`PRIVATE_CIDRS` 见 helper 顶部定义）→ 测试通过 → Commit `feat(schema): relay 配置渲染（黑名单/端口白名单/分流/fail-open）`。

---

### Task 10: 客户端配置渲染器（TUN / mixed）

**Files:**
- Create: `crates/bui-schema/src/render/client.rs`
- Test: 追加到 `tests/kernel_check.rs`

**Interfaces:**
- Produces: `pub struct ClientOpts { pub mode: ClientMode /* Tun | Mixed */, pub socks_port: u16 /*1080*/, pub http_port: u16 /*8080*/, pub host_has_ipv6: bool, pub split: SplitRules }`；`pub fn tun_config(node: &Node, opts: &ClientOpts) -> Value`；`pub fn mixed_config(node: &Node, opts: &ClientOpts) -> Value`。
- TUN 模板移植 `b-ui-client.sh:1377-1651 generate_singbox_tun_config`（schema 8：`interface_name: "bui-tun"`、`address` 数组、`host_has_ipv6` 决定是否接管 `::/0` 与裸 v6 拒绝规则、`stack: "mixed"`、CN 域名直连 DNS、`sniff` + `hijack-dns`、cloudflared QUIC 例外、住宅节点的关键字分流）。mixed 模板：同样的出站与路由，inbound 换成两个 `mixed`（1080、8080），无 TUN、无 DNS 劫持。

- [ ] **Step 1: 写失败测试**

```rust
use bui_schema::render::client::{self, ClientMode, ClientOpts};
fn copts(mode: ClientMode, v6: bool, split: bui_schema::render::SplitRules) -> ClientOpts { ClientOpts { mode, socks_port: 1080, http_port: 8080, host_has_ipv6: v6, split } }
#[test]
fn tun_config_matches_schema8_and_checks() {
    let s = common::state("global"); let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = bui_schema::render::SplitRules::from_group(s.residential.default_group().unwrap());
    for n in &nodes {
        let cfg = client::tun_config(n, &copts(ClientMode::Tun, true, split.clone()));
        let tun = &cfg["inbounds"][0];
        assert_eq!(tun["type"], "tun"); assert_eq!(tun["interface_name"], "bui-tun"); assert_eq!(tun["stack"], "mixed");
        assert!(tun["address"].as_array().unwrap().iter().any(|a| a.as_str().unwrap().contains(':')), "v6 主机接管 ::/0");
        assert!(cfg["route"]["rules"].as_array().unwrap().iter().any(|r| r["action"] == "hijack-dns"));
        assert!(cfg.get("route").unwrap().get("default_domain_resolver").is_some());
        assert!(serde_json::to_string(&cfg).unwrap().contains("rule_set") == false);
        check_singbox(&cfg);
        let cfg4 = client::tun_config(n, &copts(ClientMode::Tun, false, split.clone()));
        assert!(cfg4["inbounds"][0]["address"].as_array().unwrap().iter().all(|a| !a.as_str().unwrap().contains(':')));
        check_singbox(&cfg4);
    }
}
#[test]
fn mixed_config_has_two_mixed_inbounds() {
    let s = common::state("global"); let u = &s.users[0];
    let n = &bui_schema::nodes::nodes_for(u, &s.node, &s.residential)[2];
    let cfg = client::mixed_config(n, &copts(ClientMode::Mixed, true, bui_schema::render::SplitRules::from_group(s.residential.default_group().unwrap())));
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(inb.len(), 2); assert_eq!(inb[0]["type"], "mixed"); assert_eq!(inb[0]["listen_port"], 1080); assert_eq!(inb[1]["listen_port"], 8080);
    assert!(inb.iter().all(|i| i["type"] != "tun"));
    check_singbox(&cfg);
}
```

- [ ] **Step 2–5**：实现 → 通过 → Commit `feat(schema): 客户端 sing-box 配置渲染（TUN schema 8 / mixed）`。

---

### Task 11: 解析器（上游 URL、节点 URI）

**Files:**
- Create: `crates/bui-schema/src/parse/mod.rs`, `crates/bui-schema/src/parse/upstream.rs`, `crates/bui-schema/src/parse/node_uri.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Debug, thiserror::Error, PartialEq)] pub enum ParseError { #[error("不支持的代理协议 {0}://，只支持 socks5:// 与 http://")] Scheme(String), #[error("无法解析凭据格式")] Format, #[error("端口必须在 1-65535：{0}")] Port(String), #[error("{0}")] Other(String) }
pub struct UpstreamInput { pub kind: Option<UpstreamKind> /* None = 自动探测 */, pub host: String, pub port: u16, pub username: String, pub password: String }
pub fn upstream_url(raw: &str) -> Result<UpstreamInput, ParseError>
pub fn node_uri(raw: &str) -> Result<Node, ParseError>   // hysteria2:// 与 vless://（REALITY），label 取 fragment 解码
```

- [ ] **Step 1: 写失败测试（行为对照 `server/residential-helper.sh:132-198 parse_url`）**

```rust
#[cfg(test)] mod tests {
    use super::*; use crate::model::UpstreamKind;
    #[test] fn socks5_url() { let u = upstream_url("socks5://u:p@h.example:1080").unwrap(); assert_eq!(u.kind, Some(UpstreamKind::Socks5)); assert_eq!((u.host.as_str(), u.port, u.username.as_str(), u.password.as_str()), ("h.example", 1080, "u", "p")); }
    #[test] fn socks5h_alias_and_case() { assert_eq!(upstream_url("SOCKS5H://u:p@h:1").unwrap().kind, Some(UpstreamKind::Socks5)); }
    #[test] fn http_url_with_plus_and_hyphen_password() { let u = upstream_url("http://user-x-ip-1.2.3.4:+bz/x@isp.example:10007").unwrap(); assert_eq!(u.kind, Some(UpstreamKind::Http)); assert_eq!(u.username, "user-x-ip-1.2.3.4"); assert_eq!(u.password, "+bz/x"); }
    #[test] fn csv_form_password_with_at() { let u = upstream_url("h:1084:u:p@x:5").unwrap(); assert_eq!(u.kind, None); assert_eq!(u.host, "h"); assert_eq!(u.port, 1084); assert_eq!(u.username, "u"); assert_eq!(u.password, "p@x:5"); }
    #[test] fn at_form_without_scheme() { let u = upstream_url("u:p@h:1080").unwrap(); assert_eq!(u.kind, None); assert_eq!(u.host, "h"); }
    #[test] fn quoted_and_padded() { assert_eq!(upstream_url("  \"socks5://u:p@h:1\"  ").unwrap().port, 1); }
    #[test] fn rejects_https_and_socks4() { assert_eq!(upstream_url("https://u:p@h:1"), Err(ParseError::Scheme("https".into()))); assert_eq!(upstream_url("socks4://u:p@h:1"), Err(ParseError::Scheme("socks4".into()))); }
    #[test] fn rejects_bad_port_and_missing_password() { assert!(matches!(upstream_url("socks5://u:p@h:99999"), Err(ParseError::Port(_)))); assert!(matches!(upstream_url("socks5://u:p@h:08080"), Err(ParseError::Port(_)))); assert_eq!(upstream_url("socks5://u@h:1"), Err(ParseError::Format)); }
    #[test] fn rejects_newline_in_credentials() { assert!(upstream_url("socks5://u:p\nq@h:1").is_err()); }
}
```
`node_uri` 测试：把 Task 5 生成的 `expected/obfs/alice.sub.txt` 解码后的 4 行逐行 `node_uri()`，得到的 `Node` 与 `nodes_for(alice)` 的对应节点 `==`（`label` 从 fragment 去掉 `alice-` 前缀）。

- [ ] **Step 2–5**：实现（`upstream_url` 逐条复刻 helper 的判定顺序：去空白与引号 → scheme → 拒绝其它 scheme → `csv_ok`/`at_ok` 判定与优先级 → 端口位数 ≤ 5 且不允许前导 0 → 1..=65535；`node_uri` 用 `url` crate 解析，`hysteria2://` 取 `sni`、`mport`、`obfs-password`，`vless://` 取 `pbk`、`sid`、`sni`、`fp`、`flow`）→ 通过 → Commit `feat(schema): 上游 URL 与节点 URI 解析器`。

---

### Task 12: 默认关键字表

**Files:**
- Create: `crates/bui-schema/src/keywords.rs`
- Test: 同文件

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)] mod tests { use super::*;
    #[test] fn matches_v3_helper_table() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../server/residential-helper.sh")).unwrap();
        let start = src.find("DEFAULT_DOMAINS=(").unwrap(); let end = src[start..].find(')').unwrap() + start;
        let v3: Vec<&str> = src[start..end].split('"').skip(1).step_by(2).collect();
        assert_eq!(DEFAULT_KEYWORDS.to_vec(), v3, "与 v3 表逐项一致（顺序也一致）");
        assert_eq!(DEFAULT_KEYWORDS.len(), 67);
    }
    #[test] fn no_duplicates() { let mut s = DEFAULT_KEYWORDS.to_vec(); s.sort(); s.dedup(); assert_eq!(s.len(), DEFAULT_KEYWORDS.len()); }
}
```

- [ ] **Step 2–5**：从 `server/residential-helper.sh:88-111` 抄成 `pub const DEFAULT_KEYWORDS: &[&str] = &[ … ];` → 通过 → Commit `feat(schema): 默认住宅分流关键字表（67 条，与 v3 一致）`。

---

### Task 13: 集成收口

**Files:**
- Modify: `crates/bui-schema/src/lib.rs`（crate 级文档与 re-export）、`README` 无

- [ ] **Step 1**：`cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` 全绿；`cargo doc -p bui-schema --no-deps` 无警告。
- [ ] **Step 2**：`lib.rs` 加 `pub use model::State;`，crate 文档列出 C1 契约中的每个公共函数并链接。
- [ ] **Step 3: Commit** `chore(schema): P0 收口（文档、re-export、全量检查）`。

---

## 自查

- **Spec 覆盖**：§2.1（Task 2）、§2.3（Task 3）、§3.1（Task 7/8/9）、§4.1（Task 2/3/4）、§4.4（Task 5/6）、§5.1/§5.4 渲染部分（Task 9）、§6 渲染部分（Task 10）、§8 golden/集成（Task 5/6/7-10）。§4.2 采样、§5.2/5.3 探测与切换、§2.2 对账器不在 P0（属 P1–P3）。
- **占位扫描**：无 TBD；「移植来源」均给出文件与行号。
- **类型一致**：`SplitRules::from_group`（Task 6）被 Task 9/10 复用；`Node`/`NodeKind`/`Transport`（Task 4）被 6/10/11 使用；`Rule`（Task 2）被 9 使用；`Paths`（Task 2）被 7/8 使用。
