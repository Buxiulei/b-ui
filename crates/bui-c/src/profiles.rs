//! `profiles.json`（0600）：多节点、活动节点、模式与本地端口、面板坐标。
//!
//! 只存「用户意图」。运行期状态（失败计数、上次重启、上次自更新）在
//! `runtime.json`，两者不混——否则配置与状态纠缠、坏一个丢两个（裁决记录 10）。

use crate::paths::Paths;
use crate::sys::Sys;
use crate::{Error, Result};
use bui_schema::nodes::{Node, NodeKind, Transport};
use bui_schema::render::SplitRules;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;

/// 本地代理形态：`socks` 只开本地 SOCKS/HTTP，`tun` 全局接管。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Socks,
    Tun,
}

/// 节点是怎么进来的，决定 `update` 能不能自动刷新它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    ApiNodes,
    Subscription,
    Paste,
    V3,
}

/// 面板坐标。`base_url` 不带尾斜杠，`username` 是订阅路径的最后一段（等价凭据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    pub base_url: String,
    pub username: String,
}

/// 能当 [`Panel::base_url`] 的地址：`https://<主机>`，去掉首尾空白与尾斜杠后原样返回；
/// 别的一律 `None`。
///
/// panel 是 root 每日自更新的 manifest 与二进制首选来源，sha256 也出自同一份 manifest、
/// 没有签名——明文 http 在路上谁都能改，所以只收 https。
pub fn https_base(url: &str) -> Option<String> {
    let base = url.trim().trim_end_matches('/');
    let host = base
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &base[8..])?;
    if host.is_empty() || host.starts_with('/') {
        return None;
    }
    Some(base.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub node: Node,
    pub split: SplitRules,
    pub source: Source,
    /// RFC3339（秒精度）
    pub imported_at: String,
}

/// 一条墓碑：删掉过的节点，重新导入时先跳过、再问一句要不要加回（spec §5.7）。
///
/// `key` 是账号级指纹（[`tombstone_key`]，不含明文凭据、不含端口），`name` 只用在那一问里
/// 显示——就是用户删它时在列表上看到的名字，`at` 是 unix 秒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub key: String,
    pub name: String,
    pub at: i64,
}

/// 墓碑最多留几条（spec §5.7）：满了丢最旧的。
pub const TOMBSTONE_CAP: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profiles {
    pub schema_version: u32,
    pub active: Option<String>,
    pub mode: Mode,
    pub socks_port: u16,
    pub http_port: u16,
    pub auto_update: bool,
    pub panel: Option<Panel>,
    pub profiles: Vec<Profile>,
    /// 删掉过的节点（spec §5.7）。**放在最后、且没有墓碑时不写进文件**：这样
    /// `SCHEMA_VERSION` 不必动，没墓碑的机器上 `profiles.json` 与改动前逐字节相同。
    /// [`Profiles`] 永远不加 `deny_unknown_fields`，旧版本读到它直接忽略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Tombstone>,
}

/// [`Profiles::upsert`] 的结果，菜单据此决定提示语。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Replaced,
    Unchanged,
}

pub fn kind_slug(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::RealityDirect => "reality-direct",
        NodeKind::RealityResidential => "reality-resi",
        NodeKind::Hy2Direct => "hy2-direct",
        NodeKind::Hy2Residential => "hy2-resi",
    }
}

/// profile 名要能当命令行参数打：只留 `[A-Za-z0-9._]`，其余折成 `-`，收尾去掉分隔符。
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `<用户名>-<kind slug>`；用户名 sanitize 后为空则回落成 `<主机名>-<kind slug>`。
///
/// 回落是必须的，不能只剩 kind slug：这套部署的面板用户名全是中文
/// （`示例用户甲`、`示例专用名-小组` …），[`sanitize`] 只留 ASCII，它们一律为空，
/// 于是所有服务器的同类节点都会叫 `hy2-direct`。而 profile 名是
/// [`Profiles::upsert`] 的主键，同名即「同一个节点位」——不同服务器的节点会互相覆盖。
/// 主机名把它们分开：同一主机的同一 kind 本来就是同一个节点位。
/// 主机名 sanitize 后也为空（理论上不会，主机名本就是 ASCII）才退到光秃秃的 kind slug。
pub fn profile_name(user: &str, node: &Node) -> String {
    let kind = kind_slug(node.kind);
    let prefix = match sanitize(user) {
        u if !u.is_empty() => u,
        _ => sanitize(&node.host),
    };
    if prefix.is_empty() {
        kind.to_string()
    } else {
        format!("{prefix}-{kind}")
    }
}

/// 同一个账号位：同一台服务器（`host`）上 `kind` 相同、凭据主体相同
/// （Hysteria2 的 `username`、Reality 的 `uuid`）。
///
/// 不看 port / 密码：同一台服务器上同一账号换了密码或端口是凭据轮换，该原地替换同名节点。
/// 必须看 host：ASCII 用户名（`alice`）在两台服务器上各有一个账号时 [`profile_name`] 都是
/// `alice-<kind>`，那是两个账号，第二台不能把第一台的节点换掉（换了主机名的同一账号
/// 顶多多出一个 `-2`，不丢东西）。不同账号（家人账号）哪怕撞名也不是同一个账号位。
pub fn same_account(a: &Node, b: &Node) -> bool {
    a.kind == b.kind
        && a.host == b.host
        && match (&a.transport, &b.transport) {
            (
                Transport::Hysteria2 { username: ua, .. },
                Transport::Hysteria2 { username: ub, .. },
            ) => ua == ub,
            (Transport::Reality { uuid: ua, .. }, Transport::Reality { uuid: ub, .. }) => ua == ub,
            _ => false,
        }
}

/// 墓碑的 key：`"{kind_slug}|{host 小写}|{账号指纹}"`，账号指纹 = 凭据主体
/// （Hysteria2 的 `username`、Reality 的 `uuid`）的 sha256 取前 16 个十六进制字符。
///
/// 与 [`same_account`] 同一层级，但**不存明文凭据、也不存端口**（spec §5.7）：HY2 换密码、
/// 住宅换槽位（端口变）之后仍认得出是那个被删的节点；同一台服务器上家人的账号指纹不同，
/// 不会被误伤。
///
/// 区分度来自 `kind` 而不是端口：真机上面板导入的 Reality 直连（:10001）与 Reality 住宅
/// （:10002）共用同一个 uuid、同一个 host，两个 HY2（:10000 与 :40000）也共用同一个账号，
/// 只有 kind 不同——所以 key 里必须有 kind，也正因为如此才敢把端口留在外面。
///
/// 已知限制（spec §0.2 R16 第三条）：Reality 轮换 uuid 之后指纹跟着变，墓碑失效，删掉过的
/// 节点会被当成全新节点加回来（不误删、不出错，只是「又回来了」）。
pub fn tombstone_key(node: &Node) -> String {
    let subject = match &node.transport {
        Transport::Hysteria2 { username, .. } => username.clone(),
        Transport::Reality { uuid, .. } => uuid.to_string(),
    };
    let fingerprint = hex::encode(Sha256::digest(subject.as_bytes()));
    format!(
        "{}|{}|{}",
        kind_slug(node.kind),
        node.host.to_lowercase(),
        &fingerprint[..16]
    )
}

/// 同一个连接：[`same_account`]，且 `host`、`port` 相同，Hysteria2 的 `password` 也相同
/// （Reality 的 uuid 已经是凭据全部）。
///
/// 故意不比 `label`、`hop`、`sni` / `server_name`、`public_key`、`short_id`、
/// `obfs_password`：这些是面板能改的展示或参数，改了应原地更新，不是新节点——
/// 否则 import-v3 带 v3 备注进来的节点，经面板导入一次就在列表里出现两份。
pub fn same_endpoint(a: &Node, b: &Node) -> bool {
    same_account(a, b)
        && a.host == b.host
        && a.port == b.port
        && match (&a.transport, &b.transport) {
            (
                Transport::Hysteria2 { password: pa, .. },
                Transport::Hysteria2 { password: pb, .. },
            ) => pa == pb,
            _ => true,
        }
}

/// 订阅与粘贴来源拿不到服务端的住宅分流信息：按「活动节点承担全部流量」处理。
/// 放在 profiles.rs 而不是 source.rs——T6 与 T9 同波并行，两边都要用它。
pub fn default_split() -> SplitRules {
    SplitRules {
        enabled: true,
        global: true,
        keywords: bui_schema::keywords::DEFAULT_KEYWORDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// 注入时钟的 RFC3339（秒精度，尾部 `Z`）。
pub fn rfc3339<S: Sys>(sys: &S) -> String {
    let t = sys.now();
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

impl Profiles {
    pub fn new_default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            active: None,
            mode: Mode::Socks,
            socks_port: 1080,
            http_port: 8080,
            auto_update: true,
            panel: None,
            profiles: Vec::new(),
            deleted: Vec::new(),
        }
    }

    /// 文件不存在 → 默认值；文件存在但坏了 → 报错，绝不静默清空用户节点。
    pub fn load<S: Sys>(sys: &S, paths: &Paths) -> Result<Self> {
        let p = paths.profiles();
        if !sys.exists(&p) {
            return Ok(Self::new_default());
        }
        let raw = sys.read(&p)?;
        serde_json::from_slice(&raw).map_err(|e| Error::parse("profiles.json", e.to_string()))
    }

    /// 0600 原子写，pretty JSON + 末尾换行（带凭据，不能给别的用户读）。
    pub fn save<S: Sys>(&self, sys: &S, paths: &Paths) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::parse("profiles.json", e.to_string()))?;
        data.push(b'\n');
        sys.mkdir_p(&paths.base)?;
        sys.write(&paths.profiles(), &data, 0o600)
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        let name = self.active.as_deref()?;
        self.profiles.iter().find(|p| p.name == name)
    }

    /// active 缺失或指向不存在的 profile → 取第一个（v3 的自愈行为，但不再靠 mtime 排序）。
    pub fn heal_active(&mut self) -> bool {
        let ok = self
            .active
            .as_deref()
            .is_some_and(|n| self.profiles.iter().any(|p| p.name == n));
        if ok {
            return false;
        }
        self.active = self.profiles.first().map(|p| p.name.clone());
        true
    }

    pub fn find_by_node(&self, node: &Node) -> Option<&Profile> {
        self.profiles.iter().find(|p| &p.node == node)
    }

    /// 按连接身份（[`same_endpoint`]）找已有 profile：导入去重用它，不用整个 `Node` 相等。
    pub fn find_same_endpoint(&self, node: &Node) -> Option<&Profile> {
        self.profiles.iter().find(|p| same_endpoint(&p.node, node))
    }

    /// `base` / `base-2` / `base-3` …
    pub fn free_name(&self, base: &str) -> String {
        if !self.profiles.iter().any(|p| p.name == base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|c| !self.profiles.iter().any(|p| &p.name == c))
            .unwrap_or_else(|| base.to_string())
    }

    /// 按 name 覆盖；节点与分流都没变就不动（保住原 `imported_at`，菜单也少一句噪音）。
    pub fn upsert(&mut self, p: Profile) -> Upsert {
        match self.profiles.iter_mut().find(|x| x.name == p.name) {
            Some(existing) if existing.node == p.node && existing.split == p.split => {
                Upsert::Unchanged
            }
            Some(existing) => {
                *existing = p;
                Upsert::Replaced
            }
            None => {
                self.profiles.push(p);
                Upsert::Added
            }
        }
    }

    /// 这个节点的墓碑（[`tombstone_key`] 命中的那一条）。名字要用墓碑里记的那个：
    /// 用户删它时在列表上看到的就是它。
    pub fn tombstone_of(&self, node: &Node) -> Option<&Tombstone> {
        let key = tombstone_key(node);
        self.deleted.iter().find(|t| t.key == key)
    }

    /// 这个节点删过吗（spec §5.7）。
    pub fn is_deleted(&self, node: &Node) -> bool {
        self.tombstone_of(node).is_some()
    }

    /// 记一条墓碑：同 key 先去重（名字与时间跟着这次更新），超过 [`TOMBSTONE_CAP`] 丢最旧的。
    ///
    /// 调用点只有删除成功落盘那一处，**和 `profiles` 同一次 `save`**（spec §5.7、§0.2 R10）。
    pub fn bury(&mut self, p: &Profile, at: i64) {
        let key = tombstone_key(&p.node);
        self.deleted.retain(|t| t.key != key);
        self.deleted.push(Tombstone {
            key,
            name: p.name.clone(),
            at,
        });
        // 先去重再压上限：一批删除里同一个账号位只会占一条
        let over = self.deleted.len().saturating_sub(TOMBSTONE_CAP);
        self.deleted.drain(..over);
    }

    /// 把这个节点的墓碑清掉（加回来了）：清掉过返回 `true`。
    pub fn forget(&mut self, node: &Node) -> bool {
        let key = tombstone_key(node);
        let before = self.deleted.len();
        self.deleted.retain(|t| t.key != key);
        self.deleted.len() != before
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() == before {
            return false;
        }
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use crate::testutil::{hy2_account_node, hy2_direct_node, hy2_resi_node, split_global};
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    fn prof(name: &str, node: bui_schema::nodes::Node) -> Profile {
        Profile {
            name: name.into(),
            node,
            split: split_global(),
            source: Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        }
    }

    #[test]
    fn missing_file_yields_defaults() {
        let s = FakeSys::new();
        let p = Profiles::load(&s, &paths()).unwrap();
        assert_eq!(p, Profiles::new_default());
        assert_eq!(
            (
                p.schema_version,
                p.mode,
                p.socks_port,
                p.http_port,
                p.auto_update
            ),
            (1, Mode::Socks, 1080, 8080, true)
        );
        assert!(p.active.is_none() && p.profiles.is_empty() && p.panel.is_none());
    }

    #[test]
    fn save_is_0600_and_round_trips() {
        let s = FakeSys::new();
        let mut p = Profiles::new_default();
        p.upsert(prof("alice-hy2-direct", hy2_direct_node()));
        p.active = Some("alice-hy2-direct".into());
        p.panel = Some(Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        p.save(&s, &paths()).unwrap();
        assert_eq!(s.mode("/opt/bui-c/profiles.json"), Some(0o600));
        let raw = s.get("/opt/bui-c/profiles.json").unwrap();
        assert!(raw.ends_with('\n'), "末尾要有换行，便于 diff");
        assert_eq!(Profiles::load(&s, &paths()).unwrap(), p);
    }

    #[test]
    fn corrupt_file_is_a_parse_error_not_a_silent_reset() {
        let s = FakeSys::new();
        s.put("/opt/bui-c/profiles.json", "{ not json");
        let e = Profiles::load(&s, &paths()).unwrap_err();
        assert!(
            matches!(e, crate::Error::Parse { .. }),
            "损坏文件必须报错，不能静默清空用户节点：{e}"
        );
    }

    #[test]
    fn upsert_replaces_by_name_and_skips_identical_node() {
        let mut p = Profiles::new_default();
        assert_eq!(p.upsert(prof("n", hy2_direct_node())), Upsert::Added);
        assert_eq!(p.upsert(prof("n", hy2_direct_node())), Upsert::Unchanged);
        assert_eq!(p.upsert(prof("n", hy2_resi_node())), Upsert::Replaced);
        assert_eq!(p.profiles.len(), 1);
        assert_eq!(p.profiles[0].node, hy2_resi_node());
    }

    #[test]
    fn find_by_node_and_free_name() {
        let mut p = Profiles::new_default();
        p.upsert(prof("hy2-direct", hy2_direct_node()));
        assert_eq!(
            p.find_by_node(&hy2_direct_node()).map(|x| x.name.as_str()),
            Some("hy2-direct")
        );
        assert!(p.find_by_node(&hy2_resi_node()).is_none());
        assert_eq!(p.free_name("hy2-direct"), "hy2-direct-2");
        p.upsert(prof("hy2-direct-2", hy2_resi_node()));
        assert_eq!(p.free_name("hy2-direct"), "hy2-direct-3");
        assert_eq!(p.free_name("fresh"), "fresh");
    }

    /// 连接身份 = kind + 凭据主体 + host/port（+ HY2 密码）。label / hop / sni 与 Reality 的
    /// public_key / short_id 是面板能改的展示或参数：改了应原地更新，不是新节点。
    #[test]
    fn same_endpoint_ignores_label_hop_and_sni_but_not_credentials() {
        use bui_schema::nodes::Transport;
        let base = hy2_direct_node();
        let with_hy2 = |f: &dyn Fn(&mut String, &mut String, &mut String)| {
            let mut n = hy2_direct_node();
            if let Transport::Hysteria2 {
                username,
                password,
                sni,
                ..
            } = &mut n.transport
            {
                f(username, password, sni);
            }
            n
        };

        let cosmetic = Node {
            label: "示例专用名-小组".into(),
            hop: None,
            ..with_hy2(&|_, _, sni| *sni = "other.example.com".into())
        };
        assert!(
            same_endpoint(&base, &cosmetic),
            "只改 label/hop/sni 仍是同一个连接"
        );
        assert!(same_account(&base, &cosmetic));

        let rotated = with_hy2(&|_, pw, _| *pw = "rotated".into());
        assert!(!same_endpoint(&base, &rotated), "换了密码不是同一个连接");
        assert!(
            same_account(&base, &rotated),
            "但仍是同一个账号（凭据轮换）"
        );

        let other_user = with_hy2(&|u, _, _| *u = "bob".into());
        assert!(!same_endpoint(&base, &other_user));
        assert!(
            !same_account(&base, &other_user),
            "username 不同就是另一个账号"
        );

        let moved_host = Node {
            host: "b.example.com".into(),
            ..hy2_direct_node()
        };
        let moved_port = Node {
            port: 10009,
            ..hy2_direct_node()
        };
        assert!(!same_endpoint(&base, &moved_host));
        assert!(!same_endpoint(&base, &moved_port));
        assert!(
            !same_account(&base, &moved_host),
            "换了主机就当另一个账号：同一个 ASCII 用户名在两台服务器上是两个账号，不能互相覆盖"
        );
        assert!(
            same_account(&base, &moved_port),
            "同一台服务器换端口仍是同一个账号"
        );

        assert!(
            !same_account(&base, &hy2_resi_node()),
            "kind 不同就不是同一个账号位"
        );
        assert!(!same_endpoint(
            &base,
            &crate::testutil::reality_direct_node()
        ));

        let reality = crate::testutil::reality_direct_node();
        let with_reality = |f: &dyn Fn(&mut uuid::Uuid, &mut String)| {
            let mut n = crate::testutil::reality_direct_node();
            if let Transport::Reality { uuid, short_id, .. } = &mut n.transport {
                f(uuid, short_id);
            }
            n
        };
        let new_uuid = with_reality(&|u, _| {
            *u = uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap()
        });
        assert!(
            !same_endpoint(&reality, &new_uuid),
            "Reality 的 uuid 就是凭据全部"
        );
        assert!(!same_account(&reality, &new_uuid));
        let new_sid = with_reality(&|_, sid| *sid = "fedcba9876543210".into());
        assert!(
            same_endpoint(&reality, &new_sid),
            "short_id 是服务端参数，不是身份"
        );

        let mut p = Profiles::new_default();
        p.upsert(prof("hysteria2-1785892136", cosmetic));
        assert_eq!(
            p.find_same_endpoint(&base).map(|x| x.name.as_str()),
            Some("hysteria2-1785892136")
        );
        assert!(p.find_same_endpoint(&rotated).is_none());
    }

    #[test]
    fn heal_active_picks_first_when_dangling() {
        let mut p = Profiles::new_default();
        p.upsert(prof("a", hy2_direct_node()));
        p.upsert(prof("b", hy2_resi_node()));
        p.active = Some("gone".into());
        assert!(p.heal_active());
        assert_eq!(p.active.as_deref(), Some("a"));
        assert!(!p.heal_active(), "已一致时不报改动");
        p.profiles.clear();
        assert!(p.heal_active());
        assert!(p.active.is_none(), "没有节点时 active 清空");
    }

    #[test]
    fn remove_clears_active_when_it_was_active() {
        let mut p = Profiles::new_default();
        p.upsert(prof("a", hy2_direct_node()));
        p.active = Some("a".into());
        assert!(p.remove("a"));
        assert!(!p.remove("a"));
        assert!(p.active.is_none());
    }

    #[test]
    fn names_are_ascii_slugs_derived_from_kind() {
        assert_eq!(
            profile_name("alice", &hy2_direct_node()),
            "alice-hy2-direct"
        );
        assert_eq!(profile_name("alice", &hy2_resi_node()), "alice-hy2-resi");
        // 用户名 sanitize 后为空（这套部署的面板用户名全是中文）→ 回落到主机名，
        // 而不是只剩 kind slug：否则不同服务器的同类节点都叫 `hy2-direct`，
        // 而 profile 名是 upsert 的主键，它们会互相覆盖。
        assert_eq!(
            profile_name("", &crate::testutil::reality_direct_node()),
            "panel.example.com-reality-direct"
        );
        assert_eq!(
            profile_name("香港节点", &hy2_direct_node()),
            "panel.example.com-hy2-direct"
        );
        assert_eq!(sanitize(" My Node!! v2 "), "My-Node-v2");
        assert_eq!(sanitize("---"), "");
    }

    #[test]
    fn chinese_usernames_on_different_hosts_do_not_collide() {
        let a = Node {
            host: "a.example.com".into(),
            ..hy2_direct_node()
        };
        let b = Node {
            host: "b.example.com".into(),
            ..hy2_direct_node()
        };
        assert_eq!(profile_name("示例用户甲", &a), "a.example.com-hy2-direct");
        assert_eq!(profile_name("示例用户甲", &b), "b.example.com-hy2-direct");
        assert_ne!(
            profile_name("示例用户甲", &a),
            profile_name("示例用户甲", &b),
            "同 kind 不同主机必须是两个 profile，否则 store_fetched 会互相覆盖"
        );
    }

    #[test]
    fn https_base_only_accepts_https_with_a_host() {
        assert_eq!(
            https_base(" https://panel.example.com/ ").as_deref(),
            Some("https://panel.example.com")
        );
        assert_eq!(
            https_base("HTTPS://panel.example.com:8443").as_deref(),
            Some("HTTPS://panel.example.com:8443")
        );
        for bad in [
            "http://panel.example.com",
            "panel.example.com",
            "https://",
            "https:///x",
            "ftp://panel.example.com",
            "",
        ] {
            assert_eq!(https_base(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rfc3339_uses_injected_clock() {
        let s = FakeSys::new();
        assert_eq!(rfc3339(&s), "2026-09-11T00:00:00Z");
    }

    // ───────────── 墓碑（spec §5.7） ─────────────

    /// 墓碑的 key 是账号级的：凭据轮换（换密码）、住宅换槽位（换端口）之后仍认得出，
    /// 但 key 里既没有明文账号也没有端口；同一台服务器上另一个账号指纹不同。
    #[test]
    fn the_tombstone_key_is_account_level_and_holds_no_credentials() {
        let a = hy2_direct_node();
        let mut rotated = a.clone(); // 换密码、换端口：同一个账号
        if let bui_schema::nodes::Transport::Hysteria2 { password, .. } = &mut rotated.transport {
            *password = "rotated".into();
        }
        rotated.port = 10009;
        assert_eq!(tombstone_key(&a), tombstone_key(&rotated));
        let k = tombstone_key(&a);
        assert!(k.starts_with("hy2-direct|panel.example.com|"), "{k}");
        assert!(
            !k.contains("alice") && !k.contains("10000"),
            "不存明文账号、不存端口：{k}"
        );
        // 指纹是 sha256 的前 16 个十六进制字符
        let fp = k.rsplit('|').next().unwrap();
        assert_eq!(fp.len(), 16, "{k}");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "{k}");
        let other = hy2_account_node("bob"); // 同服务器另一个账号（家人账号）
        assert_ne!(tombstone_key(&a), tombstone_key(&other));
        // host 大小写不影响
        let upper = Node {
            host: "PANEL.EXAMPLE.COM".into(),
            ..a.clone()
        };
        assert_eq!(tombstone_key(&a), tombstone_key(&upper));
    }

    /// 区分度来自 kind：真机（baiyi 的 9 个 profile）上面板导入的 Reality 直连 :10001 与
    /// Reality 住宅 :10002 共用同一个 uuid 与 host，两个 HY2（:10000 / :40000）也共用同一个
    /// 账号——key 里不含端口，全靠 kind 把它们分开，否则删一个会连坐另一个。
    #[test]
    fn the_tombstone_key_separates_kinds_on_one_account() {
        let reality_direct = crate::testutil::reality_direct_node();
        let reality_resi = Node {
            kind: NodeKind::RealityResidential,
            port: 10002,
            ..reality_direct.clone()
        };
        assert!(
            same_account(&reality_direct, &reality_direct)
                && !same_account(&reality_direct, &reality_resi),
            "样例前提：同 uuid、同 host，只有 kind 不同"
        );
        assert_ne!(
            tombstone_key(&reality_direct),
            tombstone_key(&reality_resi),
            "同账号同主机、只有 kind 不同的两个节点，墓碑 key 必须不同"
        );
        assert_ne!(
            tombstone_key(&hy2_direct_node()),
            tombstone_key(&hy2_resi_node()),
        );
        // 指纹这一段反而相同：凭据主体是同一个
        let fp = |n: &Node| tombstone_key(n).rsplit('|').next().unwrap().to_string();
        assert_eq!(fp(&reality_direct), fp(&reality_resi));
        assert_eq!(fp(&hy2_direct_node()), fp(&hy2_resi_node()));
    }

    #[test]
    fn bury_dedups_and_caps_at_64() {
        let mut p = Profiles::new_default();
        p.bury(&prof("a", hy2_direct_node()), 1);
        p.bury(&prof("a-again", hy2_direct_node()), 2);
        assert_eq!(p.deleted.len(), 1, "同 key 只留一条：{:?}", p.deleted);
        assert_eq!(
            (p.deleted[0].name.as_str(), p.deleted[0].at),
            ("a-again", 2),
            "名字与时间跟着这次更新"
        );
        assert!(p.is_deleted(&hy2_direct_node()));

        // 再埋 72 个别的账号位：满 64 条丢最旧的（先进先出）
        for i in 0..TOMBSTONE_CAP + 8 {
            let n = Node {
                host: format!("h{i}.example.com"),
                ..hy2_direct_node()
            };
            p.bury(&prof(&format!("n{i}"), n), 100 + i as i64);
        }
        assert_eq!(p.deleted.len(), TOMBSTONE_CAP);
        let kept: Vec<&str> = p.deleted.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(kept.first(), Some(&"n8"), "最旧的 9 条（含 a-again）被丢掉");
        assert_eq!(kept.last(), Some(&"n71"));
        assert!(!p.is_deleted(&hy2_direct_node()), "挤出去的那条不再挡导入");
    }

    /// HY2 换密码之后墓碑仍认得出（指纹是 username，不随密码变）；加回来时 `forget` 清掉它。
    #[test]
    fn a_rotated_password_is_still_recognized_and_forget_clears_it() {
        let mut p = Profiles::new_default();
        p.bury(&prof("alice-hy2-direct", hy2_direct_node()), 7);
        let mut rotated = hy2_direct_node();
        if let Transport::Hysteria2 { password, .. } = &mut rotated.transport {
            *password = "rotated".into();
        }
        assert!(p.is_deleted(&rotated), "换了密码还是那个被删的节点");
        assert_eq!(
            p.tombstone_of(&rotated).map(|t| t.name.as_str()),
            Some("alice-hy2-direct"),
            "问那一句时用墓碑里记的名字"
        );
        assert!(!p.is_deleted(&hy2_resi_node()), "另一个 kind 不受影响");
        assert!(p.forget(&rotated));
        assert!(!p.forget(&rotated), "已经清过就不再报改动");
        assert!(p.deleted.is_empty());
    }

    /// 没有墓碑时 `deleted` 不写进文件，字节与改动前逐字相同（spec §5.7、§9）。
    /// 「改动前」这一份是按 v1 的字段表当场序列化出来的，不是抄下来的字面量。
    #[test]
    fn a_file_without_deleted_loads_and_saving_without_tombstones_is_byte_identical() {
        #[derive(Serialize)]
        struct V1<'a> {
            schema_version: u32,
            active: Option<&'a str>,
            mode: Mode,
            socks_port: u16,
            http_port: u16,
            auto_update: bool,
            panel: Option<&'a Panel>,
            profiles: &'a [Profile],
        }
        let panel = Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        };
        let profiles = vec![prof("alice-hy2-direct", hy2_direct_node())];
        let mut want = serde_json::to_vec_pretty(&V1 {
            schema_version: SCHEMA_VERSION,
            active: Some("alice-hy2-direct"),
            mode: Mode::Socks,
            socks_port: 1080,
            http_port: 8080,
            auto_update: true,
            panel: Some(&panel),
            profiles: &profiles,
        })
        .unwrap();
        want.push(b'\n');

        let s = FakeSys::new();
        s.put(
            "/opt/bui-c/profiles.json",
            std::str::from_utf8(&want).unwrap(),
        );
        // 旧文件没有 `deleted` 键：读得出来，墓碑为空
        let loaded = Profiles::load(&s, &paths()).unwrap();
        assert!(loaded.deleted.is_empty());
        // 写回去逐字节相同
        loaded.save(&s, &paths()).unwrap();
        assert_eq!(
            s.get("/opt/bui-c/profiles.json").unwrap().as_bytes(),
            want.as_slice(),
            "没有墓碑时文件与改动前逐字节相同"
        );
        assert!(!s
            .get("/opt/bui-c/profiles.json")
            .unwrap()
            .contains("deleted"));
        // 有墓碑才写出来，并且照样读得回去
        let mut with = loaded.clone();
        with.bury(&prof("gone", hy2_resi_node()), 11);
        with.save(&s, &paths()).unwrap();
        assert!(s
            .get("/opt/bui-c/profiles.json")
            .unwrap()
            .contains("deleted"));
        assert_eq!(Profiles::load(&s, &paths()).unwrap(), with);
    }

    /// 永不加 `deny_unknown_fields`：降级回旧版本时它必须还能读出这份文件（spec §5.7「兼容」）。
    #[test]
    fn an_old_v1_reader_still_parses_a_file_with_tombstones() {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct V1 {
            schema_version: u32,
            active: Option<String>,
            mode: Mode,
            socks_port: u16,
            http_port: u16,
            auto_update: bool,
            panel: Option<Panel>,
            profiles: Vec<Profile>,
        }
        let mut p = Profiles::new_default();
        p.bury(&prof("x", hy2_direct_node()), 1);
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"deleted\""));
        let _: V1 = serde_json::from_str(&json).expect("旧版本忽略未知字段");
    }

    #[test]
    fn default_split_is_global_with_the_default_keyword_table() {
        let d = default_split();
        assert!(
            d.enabled && d.global,
            "订阅/粘贴来源拿不到服务端分流信息，按「活动节点承担全部流量」"
        );
        assert_eq!(
            d.keywords.len(),
            bui_schema::keywords::DEFAULT_KEYWORDS.len()
        );
        // v3 的 DEFAULT_DOMAINS 是裸关键字（不是完整域名）
        assert!(d.keywords.iter().any(|k| k == "openai"));
        assert!(d.keywords.iter().any(|k| k == "claude"));
    }
}
