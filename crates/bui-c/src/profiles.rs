//! `profiles.json`（0600）：多节点、活动节点、模式与本地端口、面板坐标。
//!
//! 只存「用户意图」。运行期状态（失败计数、上次重启、上次自更新）在
//! `runtime.json`，两者不混——否则配置与状态纠缠、坏一个丢两个（裁决记录 10）。

use crate::paths::Paths;
use crate::sys::Sys;
use crate::{Error, Result};
use bui_schema::nodes::{Node, NodeKind};
use bui_schema::render::SplitRules;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub node: Node,
    pub split: SplitRules,
    pub source: Source,
    /// RFC3339（秒精度）
    pub imported_at: String,
}

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
    use crate::testutil::{hy2_direct_node, hy2_resi_node, split_global};
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
    fn rfc3339_uses_injected_clock() {
        let s = FakeSys::new();
        assert_eq!(rfc3339(&s), "2026-09-11T00:00:00Z");
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
