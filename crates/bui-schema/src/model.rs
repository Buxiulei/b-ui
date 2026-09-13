//! v4 期望态模型（C1 契约）：整台机器的全部状态都在 [`State`] 里。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// 当前状态文件的 schema 版本。
pub const SCHEMA_VERSION: u32 = 1;
/// 默认住宅分组名。
pub const DEFAULT_GROUP: &str = "default";

/// 期望态根对象（`/opt/b-ui/state.json`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub schema_version: u32,
    pub node: NodeParams,
    pub admin: Admin,
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default)]
    pub residential: Residential,
    #[serde(default)]
    pub system: SystemSettings,
    #[serde(default)]
    pub versions: Versions,
    #[serde(default)]
    pub catalog: Vec<CatalogItem>,
}

/// 本节点的身份与监听参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeParams {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub public_ip: String,
    pub ports: Ports,
    pub reality: Reality,
    #[serde(default)]
    pub obfs: Obfs,
}

/// 四条通路的端口与端口跳跃区间。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ports {
    pub hy2: u16,
    #[serde(default)]
    pub hy2_hop: Option<(u16, u16)>,
    pub hy2_resi: u16,
    pub hy2_resi_hop: (u16, u16),
    pub reality_direct: u16,
    pub reality_resi: u16,
    pub admin: u16,
}

/// REALITY 密钥与伪装参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reality {
    pub private_key: String,
    pub public_key: String,
    pub short_ids: Vec<String>,
    /// 形如 `www.bing.com:443`
    pub dest: String,
    pub server_names: Vec<String>,
}

impl Reality {
    /// SNI = dest 去掉端口（v3 `getConfig` 语义）
    pub fn sni(&self) -> &str {
        self.dest.split(':').next().unwrap_or(&self.dest)
    }

    /// 订阅里用的第一个 shortId，缺失时为空串。
    pub fn short_id(&self) -> &str {
        self.short_ids.first().map(String::as_str).unwrap_or("")
    }
}

/// Hysteria2 salamander 混淆。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Obfs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub password: String,
}

/// 面板管理员凭据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Admin {
    pub password_hash: String,
    pub jwt_secret: String,
}

/// 一个订阅用户。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub user_id: Uuid,
    pub username: String,
    #[serde(default)]
    pub note: String,
    pub created_at: String,
    #[serde(default)]
    pub disabled: bool,
    pub credentials: Credentials,
    pub entitlements: Entitlements,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub portal_auth: PortalAuth,
    #[serde(default)]
    pub billing: Billing,
}

/// 用户在两种协议上的凭据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    pub hy2_password: String,
    pub vless_uuid: Uuid,
}

/// 可开通的协议。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Hysteria2,
    Reality,
}

/// 用户权益：决定 `nodes_for` 生成哪些节点。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entitlements {
    pub protocols: Vec<Protocol>,
    #[serde(default = "default_true")]
    pub direct: bool,
    #[serde(default)]
    pub residential: Option<ResidentialEntitlement>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub traffic_limit: TrafficLimit,
}

fn default_true() -> bool {
    true
}

/// 住宅权益：指向某个住宅分组，以及该用户粘住的 IP 槽位。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResidentialEntitlement {
    pub group_id: String,
    /// 该用户粘住的槽位键 = 上游 uuid（spec §5.6「槽，键为上游 uuid」）。
    /// `None` = 还没分配（升级迁移前的老用户、或池为空），出口走兜底槽。
    /// **不存槽序号**：序号会在删上游时被回收复用，存序号等于把用户绑到「第 i 个位置」
    /// 而不是「那个 IP」，换一条上游进来就悄悄换了人家的出口。
    ///
    /// `skip_serializing_if`（D10）：没分过槽就一个字节都不进 `state.json` —— 与同文件
    /// 三个 `speedtest_*` 同风格，也让 `tests::state_round_trips` 的 `to_value(&s) == SAMPLE`
    /// 继续成立。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<Uuid>,
}

/// 流量上限（`None` = 不限）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrafficLimit {
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub monthly_bytes: Option<u64>,
}

/// 流量用量统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub monthly_bytes: u64,
    #[serde(default)]
    pub month_key: String,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

/// 用户自助门户的登录凭据。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PortalAuth {
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub tokens: Vec<String>,
}

/// 用户账务。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Billing {
    pub currency: String,
    #[serde(default)]
    pub balance_minor: i64,
    #[serde(default)]
    pub orders: Vec<Order>,
}

impl Default for Billing {
    fn default() -> Self {
        Self {
            currency: "CNY".into(),
            balance_minor: 0,
            orders: vec![],
        }
    }
}

/// 一笔订单。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub order_id: Uuid,
    pub sku: String,
    pub amount_minor: i64,
    pub status: OrderStatus,
    #[serde(default)]
    pub external_ref: Option<String>,
    pub created_at: String,
}

/// 订单状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Paid,
    Fulfilled,
    Cancelled,
}

/// 全部住宅分组。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Residential {
    #[serde(default)]
    pub groups: BTreeMap<String, ResidentialGroup>,
    /// IP 池的槽位表（spec §5.6）。**跨版本可缺**：旧 `state.json` 没有这个字段时
    /// default 成空表，由 `bui` 启动时的一次迁移补齐（`migrate_on_start`）。
    ///
    /// `skip_serializing_if`（D10）：空表不落盘 —— 既让 `tests::state_round_trips`
    /// 的 `to_value(&s) == SAMPLE` 继续成立（`SAMPLE` 里没有这个字段，**不许改它**），
    /// 也让单槽 / 空池的机器升级后 `state.json` 一个字节都不变，`Store::update_as`
    /// 的零变更比对不会空写一次盘。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
    /// 巡检测速的用量与周期覆盖（`None` = 用代码里的默认 4MB / 1MB / 60 分钟）。
    /// **`skip_serializing_if`**：不设就一个字节都不进 `state.json` —— 这三项是给
    /// 「流量吃紧想调小」的运维留的旋钮，不该让每台机器的 state 都多三个 null。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speedtest_down_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speedtest_up_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speedtest_interval_mins: Option<i64>,
}

impl Default for Residential {
    fn default() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert(DEFAULT_GROUP.to_string(), ResidentialGroup::default());
        Self {
            groups,
            slots: Vec::new(),
            speedtest_down_bytes: None,
            speedtest_up_bytes: None,
            speedtest_interval_mins: None,
        }
    }
}

impl Residential {
    /// 默认分组（不存在时返回 `None`）。
    pub fn default_group(&self) -> Option<&ResidentialGroup> {
        self.groups.get(DEFAULT_GROUP)
    }
}

/// 一组住宅上游及其分流与黑名单。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResidentialGroup {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: ResiMode,
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    #[serde(default)]
    pub upstreams: Vec<Upstream>,
    #[serde(default)]
    pub selected_upstream_id: Option<Uuid>,
    #[serde(default)]
    pub blacklist: Blacklist,
}

impl ResidentialGroup {
    /// 池有效 = enabled 且至少一个上游（fail-open 判据）
    pub fn pool_active(&self) -> bool {
        self.enabled && !self.upstreams.is_empty()
    }

    /// UDP 能不能经住宅出口：池有效且**每一条**上游都是 socks5。
    ///
    /// socks5 上游的 UDP ASSOCIATE 可用（2026-09-12 实测 Decodo Dedicated ISP：
    /// YouTube / Google / Cloudflare 的 HTTP/3 握手全通，STUN 看到的源 IP 是住宅出口；
    /// 唯一约束是一次关联只服务第一个目标地址，而 sing-box 的 socks 出站每个 packet
    /// conn 一次关联，QUIC 单目标无影响）。http 出站在 sing-box 里没有 UDP 能力，
    /// selector 选到它会直接报错，所以池里只要混进一条 http 就整池不走 UDP。
    pub fn udp_via_pool(&self) -> bool {
        self.pool_active()
            && self
                .upstreams
                .iter()
                .all(|u| matches!(u.kind, UpstreamKind::Socks5))
    }
}

/// 住宅分流模式。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResiMode {
    Global,
    #[default]
    Split,
}

/// 一个住宅上游。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Upstream {
    pub id: Uuid,
    pub name: String,
    pub kind: UpstreamKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub ports_allowed: Option<Vec<u16>>,
    #[serde(default)]
    pub verified: Option<Verified>,
}

fn default_priority() -> u32 {
    100
}

/// 一个 IP 槽位（spec §5.6）：池内每个上游 IP 占一个槽，多个用户共用一个槽。
/// `index` 决定该槽的全部端口（见 [`crate::slots`]），在增删上游时分配 / 释放，
/// 取 `0..MAX_SLOTS` 里的最小空闲值。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Slot {
    pub index: u16,
    pub upstream_id: Uuid,
}

/// 上游协议类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamKind {
    Socks5,
    Http,
}

/// 上游出口的一次体检结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verified {
    pub ip: String,
    #[serde(default)]
    pub asn: Option<u32>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    pub at: String,
}

/// 住宅黑名单：人工钉住的 + 自动学习的。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Blacklist {
    #[serde(default)]
    pub pins: Vec<Pin>,
    #[serde(default)]
    pub auto: Vec<AutoEntry>,
}

/// 一条黑名单规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Rule {
    DomainSuffix(String),
    Domain(String),
    Port(u16),
}

/// 人工钉住的黑名单条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin {
    pub rule: Rule,
    #[serde(default)]
    pub note: String,
    pub created_at: String,
}

/// 自动学习出的黑名单条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoEntry {
    pub upstream_id: Uuid,
    pub rule: Rule,
    #[serde(default)]
    pub hits: u64,
    pub confirmed_at: String,
    pub last_verified_at: String,
    #[serde(default)]
    pub passes: u32,
}

/// Hysteria2 的鉴权方式（2026-09-13 主理人裁决：默认 `http`，`command` 留作退路开关）。
///
/// - [`Hy2Auth::Http`]：内核 POST 到 `http://127.0.0.1:<AUTH_PORT>/auth`，由 `bui` 守护进程
///   **进程内**应答（`render::hysteria::AUTH_HTTP_PORT`）。
/// - [`Hy2Auth::Command`]：内核对每条新 QUIC 连接 fork 一次 `bin/bui-auth-hook`（v4 原行为）。
///
/// 旧 `state.json` 缺这个字段 ⇒ `http`（`#[serde(default)]` + [`Default`]）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Hy2Auth {
    #[default]
    Http,
    Command,
}

impl Hy2Auth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Hy2Auth::Http => "http",
            Hy2Auth::Command => "command",
        }
    }
}

impl std::fmt::Display for Hy2Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Hy2Auth {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http" => Ok(Hy2Auth::Http),
            "command" => Ok(Hy2Auth::Command),
            other => Err(format!("鉴权方式只能是 http 或 command，收到 {other}")),
        }
    }
}

/// 系统级硬化开关。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemSettings {
    #[serde(default = "default_true")]
    pub ssh_hardening: bool,
    #[serde(default = "default_true")]
    pub static_dns: bool,
    #[serde(default = "default_auto")]
    pub sysctl_profile: String,
    #[serde(default = "default_auto")]
    pub firewall: String,
    /// Hysteria2 鉴权方式（默认 http；旧 state 缺字段就是 http）
    #[serde(default)]
    pub hy2_auth: Hy2Auth,
}

fn default_auto() -> String {
    "auto".into()
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            ssh_hardening: true,
            static_dns: true,
            sysctl_profile: "auto".into(),
            firewall: "auto".into(),
            hy2_auth: Hy2Auth::Http,
        }
    }
}

/// 已安装的各内核版本。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Versions {
    #[serde(default)]
    pub bui: String,
    #[serde(default)]
    pub hysteria: String,
    #[serde(default)]
    pub xray: String,
    #[serde(default)]
    pub sing_box: String,
    #[serde(default)]
    pub caddy: String,
    #[serde(default)]
    pub client_sing_box: String,
}

/// 售卖目录里的一项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogItem {
    pub sku: String,
    pub title: String,
    pub kind: CatalogKind,
    #[serde(default)]
    pub region: Option<String>,
    pub price_minor: i64,
    #[serde(default)]
    pub period_days: Option<u32>,
}

/// 目录项类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    ResidentialIp,
    Plan,
}

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
      "system": { "ssh_hardening": true, "static_dns": true, "sysctl_profile": "auto", "firewall": "auto", "hy2_auth": "http" },
      "versions": { "bui": "4.0.0", "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2", "client_sing_box": "1.13.19" },
      "catalog": [ { "sku": "resi-ip-us", "title": "美国住宅 IP", "kind": "residential_ip", "region": "US", "price_minor": 1500, "period_days": 30 } ]
    }"#;

    #[test]
    fn state_round_trips() {
        let s: State = serde_json::from_str(SAMPLE).expect("parse");
        assert_eq!(
            s.users[0].entitlements.protocols,
            vec![Protocol::Hysteria2, Protocol::Reality]
        );
        assert_eq!(
            s.residential.groups["default"].upstreams[0].kind,
            UpstreamKind::Http
        );
        assert_eq!(
            s.residential.groups["default"].blacklist.auto[0].rule,
            Rule::Port(5228)
        );
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

    /// 2026-09-13 裁决：Hysteria2 鉴权默认 `http`。旧 `state.json`（含只写了另外四个
    /// 开关的 `system` 段）读出来必须是 `http`，`command` 只有显式写进去才生效。
    #[test]
    fn hy2_auth_defaults_to_http_and_parses_both_modes() {
        assert_eq!(SystemSettings::default().hy2_auth, Hy2Auth::Http);
        let old: SystemSettings =
            serde_json::from_str(r#"{"ssh_hardening":true,"static_dns":true}"#).unwrap();
        assert_eq!(old.hy2_auth, Hy2Auth::Http, "旧 state 缺字段 ⇒ http");
        let switched: SystemSettings = serde_json::from_str(r#"{"hy2_auth":"command"}"#).unwrap();
        assert_eq!(switched.hy2_auth, Hy2Auth::Command);
        assert_eq!(
            serde_json::to_value(Hy2Auth::Command).unwrap(),
            serde_json::json!("command")
        );
        assert_eq!("http".parse::<Hy2Auth>().unwrap(), Hy2Auth::Http);
        assert_eq!("command".parse::<Hy2Auth>().unwrap(), Hy2Auth::Command);
        assert!("userpass".parse::<Hy2Auth>().is_err());
        assert_eq!(Hy2Auth::Http.to_string(), "http");
    }

    #[test]
    fn slots_and_user_slot_id_round_trip() {
        let s: State = serde_json::from_str(SAMPLE).expect("parse");
        // 旧 state 没有 slots / slot_id：必须 default 出来而不是解析失败
        assert!(s.residential.slots.is_empty());
        assert_eq!(
            s.users[0]
                .entitlements
                .residential
                .as_ref()
                .unwrap()
                .slot_id,
            None
        );
        // D10：空槽位 / 未分配一个字节都不进 `state.json`。既有的
        // `state_round_trips` 断言 `to_value(&s) == SAMPLE`，多出 `"slots": []`
        // 或 `"slot_id": null` 就会红 —— 这两条是那条测试的前哨。
        let plain = serde_json::to_value(&s).unwrap();
        assert!(plain["residential"].get("slots").is_none());
        assert!(plain["users"][0]["entitlements"]["residential"]
            .get("slot_id")
            .is_none());

        let up = s.residential.groups["default"].upstreams[0].id;
        let mut s2 = s.clone();
        s2.residential.slots = vec![Slot {
            index: 0,
            upstream_id: up,
        }];
        s2.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(up);
        let json = serde_json::to_value(&s2).unwrap();
        assert_eq!(json["residential"]["slots"][0]["index"], 0);
        assert_eq!(
            json["residential"]["slots"][0]["upstream_id"],
            up.to_string()
        );
        assert_eq!(
            json["users"][0]["entitlements"]["residential"]["slot_id"],
            up.to_string()
        );
        let back: State = serde_json::from_value(json).unwrap();
        assert_eq!(back, s2);
    }
}
