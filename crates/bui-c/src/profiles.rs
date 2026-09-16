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

impl Source {
    /// 来源等级：`ApiNodes` 3 > `Subscription` 2 > `Paste` 1 = `V3` 1（spec §4）。
    ///
    /// 只用于「只升不降」（spec §5.5）：粘贴与 v3 同级，互相之间不改来源——
    /// 两者都是用户手里的副本，谁都不比谁新。
    pub fn rank(self) -> u8 {
        match self {
            Source::ApiNodes => 3,
            Source::Subscription => 2,
            Source::Paste | Source::V3 => 1,
        }
    }
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
    /// 以后的版本写进来、本版不认识的字段（spec §6.3）：原样读进来、原样写回去。
    /// 空时 flatten 不产生任何 JSON 键。`node` 有意不加——它由服务端下发、每次导入整条覆盖，
    /// 未知字段下次导入就回来了，加了反而削弱 `bui-schema` 的 C1 契约。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
    /// 同 [`Profile::extra`]：以后的版本写进来、本版不认识的字段（spec §6.3）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
    /// 删掉过的节点（spec §5.7）。**放在已知字段的最后（其后只有 flatten 的 `extra`，
    /// 为空时不产生键）、且没有墓碑时不写进文件**：这样
    /// `SCHEMA_VERSION` 不必动，没墓碑的机器上 `profiles.json` 与改动前逐字节相同。
    /// [`Profiles`] 永远不加 `deny_unknown_fields`，旧版本读到它直接忽略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Tombstone>,
    /// 同 [`Profile::extra`]：以后的版本写进来、本版不认识的字段（spec §6.3）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// [`Profiles::upsert`] 的结果，菜单据此决定提示语。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Replaced,
    Unchanged,
}

/// 同账号条目为什么没进账号组（spec §5.1、§5.3）。
///
/// 每一条的原因**先判是否受保护，门槛次之，两条都报**（A1）：
/// [`protected`] 且不 [`same_params`] → 条目来源是 `ApiNodes` 则 [`Blocked::PanelEntry`]、
/// 否则 [`Blocked::ActiveEntry`]，两者都带 `kind_unsure: !gate_ok(..)`，**不论门槛内外**；
/// 其余（必然在门槛外、且不受保护）→ [`Blocked::KindUnsure`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// 受保护、且条目来源是 `ApiNodes`（不管是不是活动节点）：出路是从面板重新导入。
    ///
    /// `kind_unsure` 为真表示它同时在门槛外（端口不同、且来件的 kind 是按备注猜的——
    /// 条目自己来源是 `ApiNodes`，kind 恒可信），说明句按 §5.3 补那半句。
    PanelEntry { kind_unsure: bool },
    /// 受保护、且条目是非面板来源的活动节点：出路是确认新节点能用后切换过去。
    /// `kind_unsure` 同上（端口不同，且两边 kind 至少一边是按备注猜的）。
    ActiveEntry { kind_unsure: bool },
    /// 不受保护、且在门槛外：kind 至少一边是按备注猜的、端口不同。
    KindUnsure,
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

/// 名字里带 4.0.0 的订阅 token：按 `-` 切开后有一段恰好是 32 位十六进制
/// （判据同 [`crate::source::looks_like_token`]，spec §4、§8）。
pub fn name_has_token(name: &str) -> bool {
    name.split('-').any(crate::source::looks_like_token)
}

/// 同一个账号位：同一台服务器（`host`）上 `kind` 相同、凭据主体相同
/// （Hysteria2 的 `username`、Reality 的 `uuid`）。
///
/// 不看 port / 密码：同一台服务器上同一账号换了密码或端口是凭据轮换，该原地替换同名节点。
/// 必须看 host：ASCII 用户名（`alice`）在两台服务器上各有一个账号时 [`profile_name`] 都是
/// `alice-<kind>`，那是两个账号，第二台不能把第一台的节点换掉（换了主机名的同一账号
/// 顶多多出一个 `-2`，不丢东西）。不同账号（家人账号）哪怕撞名也不是同一个账号位。
/// `host` 不区分大小写（D6，spec §5.2）：与 [`tombstone_key`] 的 `to_lowercase` 同口径，
/// 否则「墓碑挡得住」与「账号组命中」会因为大小写分叉。
pub fn same_account(a: &Node, b: &Node) -> bool {
    a.kind == b.kind
        && a.host.to_lowercase() == b.host.to_lowercase()
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

/// 从墓碑 key 推出可以展示的名字 `<sanitize(host)>-<kind_slug>`（spec §5.1）：
/// key 的第 0 段是 kind_slug、第 1 段是小写 host，`sanitize(host)` 为空就退回 kind_slug。
///
/// 墓碑记的名字可能带订阅 token（4.0.0 起过的名字），打码之前先用账号维度的 key 换一个
/// 干净名字——key 里本来就没有凭据（C7）。
///
/// 定稿 §5.1 写的就是私有函数：只给本文件的 `bury` / `heal_token_names` 用，不进 crate 的
/// 公共 API。T6 把这两处生产调用点接上之后，删掉下面这行 `allow`。
#[cfg_attr(not(test), allow(dead_code))]
fn key_display(key: &str) -> String {
    let mut parts = key.split('|');
    let kind = parts.next().unwrap_or_default();
    let host = sanitize(parts.next().unwrap_or_default());
    if host.is_empty() {
        kind.to_string()
    } else {
        format!("{host}-{kind}")
    }
}

/// 同一个连接：[`same_account`]（host 已在那里按小写比过，D6），且 `port` 相同，
/// Hysteria2 的 `password` 也相同（Reality 的 uuid 已经是凭据全部）。
///
/// 故意不比 `label`、`hop`、`sni` / `server_name`、`public_key`、`short_id`、
/// `obfs_password`：这些是面板能改的展示或参数，改了应原地更新，不是新节点——
/// 否则 import-v3 带 v3 备注进来的节点，经面板导入一次就在列表里出现两份。
pub fn same_endpoint(a: &Node, b: &Node) -> bool {
    same_account(a, b)
        && a.port == b.port
        && match (&a.transport, &b.transport) {
            (
                Transport::Hysteria2 { password: pa, .. },
                Transport::Hysteria2 { password: pb, .. },
            ) => pa == pb,
            _ => true,
        }
}

/// kind 是来源明说的，还是 [`node_uri`](bui_schema::parse) 按备注猜的（spec §4）。
///
/// 面板 `/api/nodes` 下发的 kind 是服务端给的，恒可信；URI 只看备注含不含「住宅」，
/// 所以备注点名「直连」或「住宅」的才算数。
pub fn kind_trusted(node: &Node, src: Source) -> bool {
    src == Source::ApiNodes || node.label.contains("直连") || node.label.contains("住宅")
}

/// 门槛内：同一个账号，且跨端口时两边 kind 都可信（spec §5.2）。
///
/// 端口相同就不必问 kind：猜错 kind 也换不到别的实例上去。端口不同才要两边都可信——
/// 否则一条备注被改成 `custom` 的住宅链接会把直连节点改写到住宅端口。
pub fn gate_ok(p: &Profile, node: &Node, src: Source) -> bool {
    same_account(&p.node, node)
        && (p.node.port == node.port
            || (kind_trusted(&p.node, p.source) && kind_trusted(node, src)))
}

/// 只升不降（D1，spec §5.3）：非面板来件不许覆盖面板来源的条目或活动节点。
///
/// 例外是「订阅来件遇订阅条目」：订阅是现拉的服务端数据，不是用户手里的过期副本，
/// 面板取失败、退回订阅路径的机器照常受益。
pub fn protected(p: &Profile, is_active: bool, src: Source) -> bool {
    src != Source::ApiNodes
        && (p.source == Source::ApiNodes
            || (is_active && !(src == Source::Subscription && p.source == Source::Subscription)))
}

/// 同参数：除 `label`、`hop` 以外 `node` 全等，`host` 按 [`same_account`] 的口径
/// 不区分大小写（C2，spec §5.3）。
///
/// 比「同一个连接」严：[`same_endpoint`] 有意不比 `obfs_password`、`sni`、`public_key`、
/// `short_id`，而面板开了混淆之后粘进来的一条旧链接端口与密码都相同——照 `same_endpoint`
/// 覆盖上去，活动节点的 obfs 密码被抹掉并立即 apply，TUN 下整机断网。
pub fn same_params(a: &Node, b: &Node) -> bool {
    same_account(a, b) && {
        let mut x = a.clone();
        x.label = b.label.clone();
        x.hop = b.hop;
        x.host = b.host.clone(); // same_account 已按小写比过
        x == *b
    }
}

/// 这条 profile 能不能被来件原地替换：门槛内，且不受保护或与来件同参数
/// （[`same_params`] 蕴含 [`same_endpoint`]）。
pub fn movable(p: &Profile, is_active: bool, node: &Node, src: Source) -> bool {
    gate_ok(p, node, src) && (!protected(p, is_active, src) || same_params(&p.node, node))
}

/// 来件、留存者、组内面板成员三者里等级最高的来源；同级时留条目原来的（spec §5.4、§5.5）。
///
/// `keep` 是留存者当前的来源（账号组为空、新建节点时 `None`），`has_panel_member` 为真表示
/// 合并前的账号组里有 `ApiNodes` 成员——等价于候选里多一个 `Source::ApiNodes`。
/// 同级留 `keep`，所以 `Paste` 与 `V3` 互遇谁都不动（spec §4 来源等级）。
///
/// 定稿把它写在 `cli.rs` 的伪代码之后，实现放在这里：§11.1 测试 17a 要它与
/// [`Profiles::raise_source`] 在同一张表上断言，`cli.rs` 引用即可。
pub fn best_source(src: Source, keep: Option<Source>, has_panel_member: bool) -> Source {
    let mut best = keep.unwrap_or(src);
    for cand in [src]
        .into_iter()
        .chain(has_panel_member.then_some(Source::ApiNodes))
    {
        if cand.rank() > best.rank() {
            best = cand;
        }
    }
    best
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
            extra: Default::default(),
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

    /// 这条 profile 是不是当前活动节点（[`movable`] 等一族纯函数的 `is_active` 参数）。
    fn is_active(&self, p: &Profile) -> bool {
        self.active.as_deref() == Some(p.name.as_str())
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

    /// 账号组（spec §5.1）：[`movable`] 命中的 profile 下标，按列表顺序。
    ///
    /// 扫全部 profile，**不看名字**——rc 靠「名字恰好等于 `profile_name`」才认得出同一个节点位，
    /// 名字对不上（v3 目录名、token 名、`-2` 后缀）就另起一条。同一批里先写入的节点也在其中。
    pub fn account_group(&self, node: &Node, src: Source) -> Vec<usize> {
        self.profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| movable(p, self.is_active(p), node, src))
            .map(|(i, _)| i)
            .collect()
    }

    /// 没进账号组的同账号条目（spec §5.1、§5.3）：返回下标与原因，按
    /// `PanelEntry` > `ActiveEntry` > `KindUnsure` 取，同级取列表最前，取舍与 `kind_unsure` 无关。
    ///
    /// 每条的原因**先判是否受保护，门槛次之，两条都报**（A1）：[`protected`] 且不
    /// [`same_params`] 的，不论门槛内外都报 `PanelEntry`（条目来源 `ApiNodes`）或
    /// `ActiveEntry`，同时在门槛外时 `kind_unsure` 为真；其余（必然门槛外、且不受保护）
    /// 才是 `KindUnsure`。
    pub fn blocked_same_account(&self, node: &Node, src: Source) -> Option<(usize, Blocked)> {
        /// 报哪一条：`PanelEntry` 0 > `ActiveEntry` 1 > `KindUnsure` 2。
        fn order(why: &Blocked) -> u8 {
            match why {
                Blocked::PanelEntry { .. } => 0,
                Blocked::ActiveEntry { .. } => 1,
                Blocked::KindUnsure => 2,
            }
        }
        self.profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| same_account(&p.node, node))
            .filter(|(_, p)| !movable(p, self.is_active(p), node, src))
            .map(|(i, p)| {
                // 保护优先、门槛次之、两条都报（A1）：受保护且不同参数的，门槛内外都报
                // PanelEntry / ActiveEntry，门槛外只是让 kind_unsure 为真
                let why = if protected(p, self.is_active(p), src) && !same_params(&p.node, node) {
                    let kind_unsure = !gate_ok(p, node, src);
                    match p.source {
                        Source::ApiNodes => Blocked::PanelEntry { kind_unsure },
                        _ => Blocked::ActiveEntry { kind_unsure },
                    }
                } else {
                    // 剩下的必然在门槛外：same_params 蕴含 same_endpoint、进而蕴含 gate_ok，
                    // 「受保护且同参数」根本到不了这里（它 movable，已经进了账号组）
                    Blocked::KindUnsure
                };
                (i, why)
            })
            .min_by_key(|(i, why)| (order(why), *i))
    }

    /// 账号组里留哪一条接收新数据（spec §5.1，`group` 非空）。
    pub fn pick_keeper(&self, group: &[usize], node: &Node, wanted: &str, known: usize) -> usize {
        group
            .iter()
            .copied()
            .min_by_key(|&i| {
                let p = &self.profiles[i];
                (
                    !self.is_active(p),            // 1. 正在用的那一条不动
                    i >= known,                    // 2. 导入前已存在的老名字不被本批顶掉
                    !same_endpoint(&p.node, node), // 3. 与来件同一连接
                    p.name != wanted,              // 4. 名字就是这次要起的那个
                    i,                             // 5. 列表位置靠前
                )
            })
            .expect("account_group 非空时才调 pick_keeper（spec §5.4 ①）")
    }

    /// `import-v3` 用（spec §5.1、§5.9）：只在 `self.profiles[..known]` 里找门槛内的同账号条目。
    pub fn find_account_before(&self, known: usize, node: &Node, src: Source) -> Option<&Profile> {
        let head = &self.profiles[..known.min(self.profiles.len())];
        let hits = || head.iter().filter(|p| gate_ok(p, node, src));
        hits().find(|p| self.is_active(p)).or_else(|| hits().next())
    }

    /// 把名为 `name` 的条目的来源升到 `src`（spec §5.5）：等级不高于现有来源时什么都不动。
    pub fn raise_source(&mut self, name: &str, src: Source) -> bool {
        match self.profiles.iter_mut().find(|p| p.name == name) {
            // `imported_at` 有意不动：来源升级不是内容变化（spec §14.2 T7）
            Some(p) if src.rank() > p.source.rank() => {
                p.source = src;
                true
            }
            _ => false,
        }
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
    pub fn upsert(&mut self, mut p: Profile) -> Upsert {
        match self.profiles.iter_mut().find(|x| x.name == p.name) {
            Some(existing) if existing.node == p.node && existing.split == p.split => {
                Upsert::Unchanged
            }
            Some(existing) => {
                // 整条替换会丢掉这条 profile 上以后版本写进来的未知字段，先搬过去（spec §6.3）
                p.extra = std::mem::take(&mut existing.extra);
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

    /// 记一条墓碑：同 key 先去重（名字与时间跟着这次更新，未知字段从旧墓碑搬过来），
    /// 超过 [`TOMBSTONE_CAP`] 丢最旧的。
    ///
    /// 调用点只有删除成功落盘那一处，**和 `profiles` 同一次 `save`**（spec §5.7、§0.2 R10）。
    pub fn bury(&mut self, p: &Profile, at: i64) {
        let key = tombstone_key(&p.node);
        // 同 key 的旧墓碑整条取出来，把它上面的未知字段搬到新墓碑（spec §6.3）
        let extra = self
            .deleted
            .iter()
            .position(|t| t.key == key)
            .map(|i| self.deleted.remove(i).extra)
            .unwrap_or_default();
        self.deleted.push(Tombstone {
            key,
            name: p.name.clone(),
            at,
            extra,
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
            extra: Default::default(),
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

    /// 整条替换要把旧条目上的未知字段搬过去（spec §6.3）：以后的版本给 `Profile` 加了字段，
    /// 降回本版再导入一次不该把它抹掉。
    #[test]
    fn upsert_replace_keeps_unknown_profile_fields() {
        let mut p = Profiles::new_default();
        let mut old = prof("n", hy2_direct_node());
        old.extra
            .insert("future_field".into(), serde_json::json!({"a": 1}));
        p.profiles.push(old);

        assert_eq!(p.upsert(prof("n", hy2_resi_node())), Upsert::Replaced);
        assert_eq!(p.profiles.len(), 1);
        assert_eq!(p.profiles[0].node, hy2_resi_node(), "节点换成来件的");
        assert_eq!(
            p.profiles[0].extra.get("future_field"),
            Some(&serde_json::json!({"a": 1})),
            "旧条目上的未知字段跟着留下"
        );
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

        let host_case = Node {
            host: "Panel.Example.com".into(),
            ..hy2_direct_node()
        };
        assert!(
            same_endpoint(&base, &host_case),
            "host 只差大小写仍是同一个连接（D6）"
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

    /// 换掉 hy2 直连节点的 Hysteria2 字段，`None` = 不动。
    fn hy2_with(
        username: Option<&str>,
        password: Option<&str>,
        sni: Option<&str>,
        obfs: Option<&str>,
    ) -> Node {
        let mut n = hy2_direct_node();
        if let Transport::Hysteria2 {
            username: u,
            password: pw,
            sni: s,
            obfs_password: o,
        } = &mut n.transport
        {
            if let Some(v) = username {
                *u = v.into();
            }
            if let Some(v) = password {
                *pw = v.into();
            }
            if let Some(v) = sni {
                *s = v.into();
            }
            if let Some(v) = obfs {
                *o = Some(v.into());
            }
        }
        n
    }

    /// 换掉 Reality 直连节点的服务端参数，`None` = 不动。
    fn reality_with(
        public_key: Option<&str>,
        short_id: Option<&str>,
        server_name: Option<&str>,
        flow: Option<&str>,
    ) -> Node {
        let mut n = crate::testutil::reality_direct_node();
        if let Transport::Reality {
            public_key: pk,
            short_id: sid,
            server_name: sn,
            flow: fl,
            ..
        } = &mut n.transport
        {
            if let Some(v) = public_key {
                *pk = v.into();
            }
            if let Some(v) = short_id {
                *sid = v.into();
            }
            if let Some(v) = server_name {
                *sn = v.into();
            }
            if let Some(v) = flow {
                *fl = v.into();
            }
        }
        n
    }

    /// 来源等级：粘贴与 v3 同级，谁都不压过谁（spec §4，只升不降的底座）。
    #[test]
    fn source_rank_puts_paste_and_v3_at_the_same_level() {
        assert_eq!(Source::Paste.rank(), Source::V3.rank());
        assert!(Source::Subscription.rank() > Source::Paste.rank());
        assert!(Source::ApiNodes.rank() > Source::Subscription.rank());
    }

    /// kind 可信 = 来源是面板，或备注里点名「直连」/「住宅」（spec §4、§5.1）。
    #[test]
    fn kind_trusted_only_for_panel_nodes_or_labels_naming_direct_or_residential() {
        let guessed = Node {
            label: "示例备注".into(),
            ..hy2_direct_node()
        };
        assert!(
            kind_trusted(&guessed, Source::ApiNodes),
            "面板明说了 kind，备注写什么都可信"
        );
        for src in [Source::Subscription, Source::Paste, Source::V3] {
            assert!(!kind_trusted(&guessed, src), "{src:?}：备注不含直连 / 住宅");
            assert!(
                kind_trusted(&hy2_direct_node(), src),
                "{src:?}：备注含「直连」"
            );
            assert!(
                kind_trusted(&hy2_resi_node(), src),
                "{src:?}：备注含「住宅」"
            );
            assert!(
                kind_trusted(
                    &Node {
                        label: "alice-HY2住宅-备用".into(),
                        ..hy2_resi_node()
                    },
                    src
                ),
                "{src:?}：备注里带「住宅」二字就够"
            );
            assert!(
                !kind_trusted(
                    &Node {
                        label: "custom".into(),
                        ..hy2_resi_node()
                    },
                    src
                ),
                "{src:?}：kind 是住宅但备注是自定义的，kind 就是猜的"
            );
        }
    }

    /// §5.1、§5.2：账号组按列表顺序收下标；端口相同一律进组，端口不同要**两边** kind 都可信。
    #[test]
    fn account_group_crosses_ports_only_when_both_kinds_are_trusted() {
        // 条目一律非活动、V3 来源：不受保护，挡得住它的只剩门槛（§11 门槛用例的夹具）
        let entry = |label: &str, port: u16| {
            let mut p = prof(
                "alice-hy2-direct",
                Node {
                    label: label.into(),
                    port,
                    ..hy2_direct_node()
                },
            );
            p.source = Source::V3;
            p
        };
        for (entry_label, entry_trusted) in [("HY2直连", true), ("示例备注", false)] {
            for (in_label, in_trusted) in [("alice-HY2直连", true), ("custom", false)] {
                for port in [10000_u16, 40003] {
                    let mut prs = Profiles::new_default();
                    prs.profiles.push(entry(entry_label, 10000));
                    let node = Node {
                        label: in_label.into(),
                        port,
                        ..hy2_direct_node()
                    };
                    let want: Vec<usize> = if port == 10000 || (entry_trusted && in_trusted) {
                        vec![0]
                    } else {
                        vec![]
                    };
                    assert_eq!(
                        prs.account_group(&node, Source::Paste),
                        want,
                        "条目备注 {entry_label} × 来件备注 {in_label} × 端口 {port}"
                    );
                }
            }
        }

        // 扫的是全部 profile、不看名字：同账号的两条都在组里，按列表顺序
        let mut prs = Profiles::new_default();
        prs.profiles.push(entry("HY2直连", 10000));
        let mut second = entry("HY2直连", 40003);
        second.name = "hysteria2-1785892136".into();
        prs.profiles.push(second);
        let node = Node {
            label: "alice-HY2直连".into(),
            port: 40007,
            ..hy2_direct_node()
        };
        assert_eq!(
            prs.account_group(&node, Source::Paste),
            vec![0, 1],
            "名字对不上也认得出同一个账号位"
        );
    }

    /// §5.1、§4：账号组不跨 kind、不跨 host、不跨凭据主体——共用 uuid / username 的两种 kind
    /// 只靠 kind 分开。
    #[test]
    fn account_group_never_crosses_kind_host_or_subject() {
        let mut prs = Profiles::new_default();
        prs.profiles.push(prof(
            "alice-reality-direct",
            crate::testutil::reality_direct_node(),
        ));
        prs.profiles
            .push(prof("alice-hy2-direct", hy2_direct_node()));

        // Reality 住宅与直连共用同一个 uuid、同一台 host，只有 kind 不同
        let reality_resi = Node {
            kind: NodeKind::RealityResidential,
            label: "Reality住宅".into(),
            port: 10002,
            ..crate::testutil::reality_direct_node()
        };
        assert!(
            prs.account_group(&reality_resi, Source::ApiNodes)
                .is_empty(),
            "Reality 住宅与直连共用 uuid，只有 kind 把它们分开"
        );
        // HY2 住宅与直连共用同一个 username
        assert!(
            prs.account_group(&hy2_resi_node(), Source::ApiNodes)
                .is_empty(),
            "HY2 住宅与直连共用 username"
        );
        // 另一台服务器上的同一个 ASCII 用户名是另一个账号
        let other_host = Node {
            host: "tizi.example.test".into(),
            ..hy2_direct_node()
        };
        assert!(
            prs.account_group(&other_host, Source::ApiNodes).is_empty(),
            "换了主机就是另一个账号"
        );
        // 同一台服务器上的家人账号：凭据主体不同
        assert!(
            prs.account_group(&hy2_account_node("bob"), Source::ApiNodes)
                .is_empty(),
            "撞名也不是同一个账号位"
        );
        // 前提：同 kind、同 host、同凭据主体的来件确实进得了组
        assert_eq!(
            prs.account_group(&hy2_direct_node(), Source::ApiNodes),
            vec![1]
        );
    }

    /// D6：`same_account` / `same_endpoint` 的 host 与 `tombstone_key` 同口径，不区分大小写。
    #[test]
    fn same_account_and_same_endpoint_ignore_host_case_like_the_tombstone_key() {
        let lower = hy2_direct_node();
        let upper = Node {
            host: "Panel.Example.com".into(),
            ..lower.clone()
        };
        assert!(same_account(&lower, &upper), "host 只差大小写是同一个账号");
        assert!(same_endpoint(&lower, &upper), "也是同一个连接");
        assert_eq!(
            tombstone_key(&lower),
            tombstone_key(&upper),
            "墓碑 key 本来就小写，三者从此一致"
        );

        let moved = Node {
            port: 10009,
            ..upper
        };
        assert!(same_account(&lower, &moved), "换端口仍是同一个账号");
        assert!(!same_endpoint(&lower, &moved), "但不是同一个连接");

        let resi = Node {
            kind: NodeKind::Hy2Residential,
            host: "PANEL.EXAMPLE.COM".into(),
            ..lower.clone()
        };
        assert!(!same_account(&lower, &resi), "kind 不同不是同一个账号");
        assert!(!same_endpoint(&lower, &resi));
        assert_ne!(tombstone_key(&lower), tombstone_key(&resi));

        let other_host = Node {
            host: "OTHER.example.com".into(),
            ..lower.clone()
        };
        assert!(
            !same_account(&lower, &other_host),
            "宽到大小写为止：换了主机仍是另一个账号"
        );
        assert!(!same_endpoint(&lower, &other_host));
    }

    /// §5.3 只升不降扩到节点（D1）：非面板来件不许覆盖面板条目或活动节点，
    /// 例外是「订阅来件遇订阅条目」；受保护的条目只接受同参数的来件。
    ///
    /// 函数名沿用定稿 §11.1 第 5 条的 r2 措辞（`same_endpoint`），断言按 r3 收紧后的
    /// `same_params` 写——改名会与 §11.1 的编号对不上，留给文档收尾轮更正那一行。
    #[test]
    fn protected_entries_need_the_same_endpoint_for_non_panel_sources() {
        let sources = [
            Source::ApiNodes,
            Source::Subscription,
            Source::Paste,
            Source::V3,
        ];
        // 同参数来件：只差 label 与 hop
        let same = Node {
            label: "面板改过的备注".into(),
            hop: None,
            ..hy2_direct_node()
        };
        // 不同参数来件：端口与 HY2 密码都相同（同一个连接），只多了 obfs 密码——
        // 受保护条目要挡的正是这一种（§5.3，C2）
        let differing = hy2_with(None, None, None, Some("obfs-pw"));
        assert!(same_params(&hy2_direct_node(), &same));
        assert!(!same_params(&hy2_direct_node(), &differing));
        assert!(
            same_endpoint(&hy2_direct_node(), &differing),
            "它是同一个连接，只有 same_params 挡得住"
        );

        for src in sources {
            for entry_src in sources {
                for is_active in [true, false] {
                    for (node, params_equal) in [(&same, true), (&differing, false)] {
                        let mut p = prof("alice-hy2-direct", hy2_direct_node());
                        p.source = entry_src;
                        assert!(
                            gate_ok(&p, node, src),
                            "同端口的同账号来件一律在门槛内：{src:?} × {entry_src:?}"
                        );
                        // §5.3 表：面板来件整行可替换；ApiNodes 条目与活动节点只接受同参数，
                        // 例外是订阅来件遇订阅条目；其余条目可替换
                        let only_same_params = match (src, entry_src, is_active) {
                            (Source::ApiNodes, _, _) => false,
                            (_, Source::ApiNodes, _) => true,
                            (Source::Subscription, Source::Subscription, true) => false,
                            (_, _, true) => true,
                            _ => false,
                        };
                        assert_eq!(
                            movable(&p, is_active, node, src),
                            !only_same_params || params_equal,
                            "来件 {src:?} × 条目 {entry_src:?} × active={is_active} × 同参数={params_equal}"
                        );
                        assert_eq!(
                            protected(&p, is_active, src),
                            only_same_params,
                            "受保护与否：{src:?} × {entry_src:?} × active={is_active}"
                        );
                    }
                }
            }
        }
    }

    /// §5.2 kind 门槛的跨端口那一支：端口变了的来件，**条目与来件两边**的 kind 都可信
    /// 才算在门槛内。
    ///
    /// 测试 5 的来件与条目同端口，`gate_ok` 走 `p.node.port == node.port` 短路，kind 分支
    /// 一格都没走到；这里专钉那一支，顺带钉住 `movable` 里 `gate_ok &&` 的前件不是恒真。
    #[test]
    fn the_kind_gate_needs_both_sides_trusted_once_the_port_moves() {
        // 条目：kind 是 Hy2Direct，但备注不点名、来源也不是面板——kind 是猜的
        let entry = |src: Source| {
            let mut p = prof(
                "alice-hy2-direct",
                Node {
                    label: "示例备注".into(),
                    ..hy2_direct_node()
                },
            );
            p.source = src;
            p
        };
        // 来件：同账号、同 kind（备注不含「住宅」，node_uri 一律猜直连），只换了端口
        let incoming = |label: &str| Node {
            label: label.into(),
            port: 40003,
            ..hy2_direct_node()
        };
        let named = incoming("alice-HY2直连");
        let unnamed = incoming("custom");
        assert!(
            same_account(&entry(Source::V3).node, &named)
                && !same_endpoint(&hy2_direct_node(), &named),
            "前提：同账号、跨端口，门槛之外没有别的东西拦着"
        );

        // (a) 条目侧是猜的：来件点名也进不了门槛（§5.2 表第二行的方向）
        let guessed = entry(Source::V3);
        assert!(
            !gate_ok(&guessed, &named, Source::Paste),
            "条目的 kind 是猜的，跨端口不给动"
        );
        assert!(
            !gate_ok(&guessed, &named, Source::ApiNodes),
            "来件是面板也一样：猜错 kind 的条目会被挪到另一个实例上去"
        );
        assert!(
            !protected(&guessed, false, Source::Paste),
            "前提：非活动的 v3 条目不受保护，这一格挡住它的只能是门槛"
        );
        assert!(
            !movable(&guessed, false, &named, Source::Paste),
            "门槛外就不可替换，与保护、同参数无关"
        );

        // (b) 条目来自面板：kind 是服务端给的，两边都可信
        let panel = entry(Source::ApiNodes);
        assert!(
            gate_ok(&panel, &named, Source::Paste),
            "条目 kind 面板明说、来件备注点名「直连」"
        );

        // (c) 来件侧是猜的：备注被改成 custom 的链接不许把节点挪到别的端口（§5.2 表第一行）
        assert!(
            !gate_ok(&panel, &unnamed, Source::Paste),
            "来件的 kind 是猜的，跨端口不给动"
        );

        // (d) 来件也来自面板：备注写什么都可信
        assert!(
            gate_ok(&panel, &unnamed, Source::ApiNodes),
            "面板明说了 kind，备注是 custom 也放行"
        );

        // 门槛内且不受保护时 movable 跟着放行：证明上面的 !movable 是门槛给的
        let named_entry = {
            let mut p = prof("alice-hy2-direct", hy2_direct_node());
            p.source = Source::V3;
            p
        };
        assert!(
            kind_trusted(&named_entry.node, named_entry.source),
            "前提：hy2_direct_node 的备注点名「直连」"
        );
        assert!(
            movable(&named_entry, false, &named, Source::Paste),
            "两边都点名了 kind，非活动的 v3 条目可以原地换端口"
        );
    }

    /// C2：同参数比「同一个连接」严——只放过 label、hop 与 host 大小写。
    #[test]
    fn same_params_ignores_only_label_and_hop() {
        let base = hy2_direct_node();
        let cosmetic = Node {
            label: "示例专用名-小组".into(),
            hop: None,
            host: "Panel.Example.com".into(),
            ..base.clone()
        };
        let cases: Vec<(&str, Node, bool)> = vec![
            ("label / hop / host 大小写", cosmetic, true),
            (
                "端口",
                Node {
                    port: 10009,
                    ..base.clone()
                },
                false,
            ),
            (
                "HY2 密码",
                hy2_with(None, Some("rotated"), None, None),
                false,
            ),
            (
                "obfs 密码（None → Some）",
                hy2_with(None, None, None, Some("obfs-pw")),
                false,
            ),
            (
                "sni",
                hy2_with(None, None, Some("other.example.com"), None),
                false,
            ),
            ("username", hy2_with(Some("bob"), None, None, None), false),
        ];
        for (what, other, want) in &cases {
            assert_eq!(same_params(&base, other), *want, "{what}");
            assert!(
                !same_params(&base, other) || same_endpoint(&base, other),
                "same_params 蕴含 same_endpoint：{what}"
            );
        }

        let reality = crate::testutil::reality_direct_node();
        let reality_cases: Vec<(&str, Node)> = vec![
            (
                "public_key",
                reality_with(Some("OTHER-PUB"), None, None, None),
            ),
            (
                "short_id",
                reality_with(None, Some("fedcba9876543210"), None, None),
            ),
            (
                "server_name",
                reality_with(None, None, Some("www.example.com"), None),
            ),
            ("flow", reality_with(None, None, None, Some(""))),
        ];
        for (what, other) in &reality_cases {
            assert!(
                !same_params(&reality, other),
                "Reality 的 {what} 是连接参数，不同就不是同参数"
            );
            assert!(
                same_endpoint(&reality, other),
                "但按「同一个连接」它们相同——同参数严在这里：{what}"
            );
        }
        assert!(
            same_params(
                &reality,
                &Node {
                    label: "改过的备注".into(),
                    ..reality.clone()
                }
            ),
            "Reality 也只放过 label 与 hop"
        );
    }

    /// §5.1、§9（A1）：被挡下的同账号条目，原因**先判是否受保护、门槛次之、两条都报**；
    /// 多条之间 `PanelEntry` > `ActiveEntry` > `KindUnsure`，同级取列表最前，取舍与
    /// `kind_unsure` 无关。
    ///
    /// 纯函数用例：`Node` 由 `testutil` 直接构造，两侧 kind 恒为 `Hy2Direct`
    /// （§11 通则在这里体现为「构造的 kind 相同」），label 只决定 `kind_trusted`。
    #[test]
    fn blocked_same_account_reports_panel_entries_before_active_ones_before_kind_unsure() {
        let entry = |name: &str, label: &str, src: Source| {
            let mut p = prof(
                name,
                Node {
                    label: label.into(),
                    ..hy2_direct_node()
                },
            );
            p.source = src;
            p
        };
        // 同端口、同 HY2 密码，只多了 obfs 密码：同一个连接但不同参数（§5.3，C2）
        let obfs = hy2_with(None, None, None, Some("obfs-pw"));
        // 跨端口来件：备注点名「直连」的可信，改成 custom 的是猜的
        let moved = |label: &str| Node {
            label: label.into(),
            port: 40003,
            ..hy2_direct_node()
        };
        // 每一格同时断言「不在账号组里」与「blocked_same_account 报的那一条」，挡住静默空过
        let case = |entries: Vec<Profile>, active: Option<&str>, node: &Node| {
            let mut prs = Profiles::new_default();
            prs.profiles = entries;
            prs.active = active.map(str::to_string);
            (
                prs.account_group(node, Source::Paste),
                prs.blocked_same_account(node, Source::Paste),
            )
        };

        // ① 不受保护、门槛外 → KindUnsure
        assert_eq!(
            case(
                vec![entry("v3-name", "示例备注", Source::V3)],
                None,
                &moved("alice-HY2直连"),
            ),
            (vec![], Some((0, Blocked::KindUnsure))),
            "非活动的 v3 条目不受保护，挡下它的只有门槛"
        );
        // ② 面板来源条目、门槛内、不同参数 → PanelEntry { kind_unsure: false }
        assert_eq!(
            case(
                vec![entry("alice-hy2-direct", "HY2直连", Source::ApiNodes)],
                None,
                &obfs,
            ),
            (
                vec![],
                Some((0, Blocked::PanelEntry { kind_unsure: false }))
            ),
            "面板开了混淆之前的旧链接：同一个连接，但参数不同"
        );
        // ③ 非面板来源的活动节点、门槛内、不同参数 → ActiveEntry { kind_unsure: false }
        assert_eq!(
            case(
                vec![entry("alice-hy2-direct", "HY2直连", Source::V3)],
                Some("alice-hy2-direct"),
                &obfs,
            ),
            (
                vec![],
                Some((0, Blocked::ActiveEntry { kind_unsure: false }))
            ),
        );
        // ④ 面板来源条目 + 猜 kind 的跨端口粘贴：受保护且门槛外 → PanelEntry { kind_unsure: true }
        assert_eq!(
            case(
                vec![entry("alice-hy2-direct", "HY2直连", Source::ApiNodes)],
                None,
                &moved("custom"),
            ),
            (vec![], Some((0, Blocked::PanelEntry { kind_unsure: true }))),
            "A1：保护优先、门槛次之，两条都报，不退化成 KindUnsure"
        );
        // ⑤ 猜 kind 的活动 V3 条目 + 备注可信的跨端口粘贴 → ActiveEntry { kind_unsure: true }
        assert_eq!(
            case(
                vec![entry("v3-name", "示例备注", Source::V3)],
                Some("v3-name"),
                &moved("alice-HY2直连"),
            ),
            (
                vec![],
                Some((0, Blocked::ActiveEntry { kind_unsure: true }))
            ),
            "A1：活动节点同时在门槛外，报的仍是 ActiveEntry"
        );
        // ⑥ 门槛内、受保护、同参数 → 它 movable、在账号组里，一个字都不报
        let same = Node {
            label: "面板改过的备注".into(),
            hop: None,
            ..hy2_direct_node()
        };
        assert_eq!(
            case(
                vec![entry("alice-hy2-direct", "HY2直连", Source::ApiNodes)],
                None,
                &same,
            ),
            (vec![0], None),
            "同参数的来件进得了组，组成员不算被挡下"
        );
        // ⑦ 面板条目与活动节点同时被挡下：只报 PanelEntry（面板条目排在后面也一样，R18）
        assert_eq!(
            case(
                vec![
                    entry("v3-name", "示例备注", Source::V3),
                    entry("alice-hy2-direct", "HY2直连", Source::ApiNodes),
                ],
                Some("v3-name"),
                &moved("alice-HY2直连"),
            ),
            (
                vec![],
                Some((1, Blocked::PanelEntry { kind_unsure: false }))
            ),
            "PanelEntry 排最前，面板条目排在列表后面也一样"
        );
        // ⑧ 同级取列表最前
        assert_eq!(
            case(
                vec![
                    entry("alice-hy2-direct", "HY2直连", Source::ApiNodes),
                    entry("alice-hy2-direct-2", "HY2直连", Source::ApiNodes),
                ],
                None,
                &obfs,
            ),
            (
                vec![],
                Some((0, Blocked::PanelEntry { kind_unsure: false }))
            ),
            "同级取列表最前"
        );
        // ⑨ 反向组合：活动条目在门槛内（kind_unsure 假）、面板条目在门槛外（kind_unsure 真），
        //    报的仍是 PanelEntry——⑦ 那格两者同向，钉不住「取舍与 kind_unsure 无关」
        let mut v3_active = prof(
            "v3-name",
            Node {
                label: "示例备注".into(),
                port: 40003,
                ..hy2_with(None, Some("hy2-pw-2"), None, None)
            },
        );
        v3_active.source = Source::V3;
        assert_eq!(
            case(
                vec![
                    v3_active,
                    entry("alice-hy2-direct", "HY2直连", Source::ApiNodes),
                ],
                Some("v3-name"),
                &moved("custom"),
            ),
            (vec![], Some((1, Blocked::PanelEntry { kind_unsure: true }))),
            "活动条目同端口（门槛内）、面板条目跨端口且来件不可信（门槛外），报的仍是 PanelEntry"
        );
    }

    /// §5.1：留存者的五级优先级，逐级各一组。
    #[test]
    fn pick_keeper_prefers_active_then_preexisting_then_same_endpoint_then_wanted_then_list_order()
    {
        let wanted = "alice-hy2-direct";
        let node = hy2_direct_node(); // :10000
        let moved = || Node {
            port: 40003,
            ..hy2_direct_node()
        };

        // 第 1 级：活动节点赢——它在后面、是本批刚写入的、不是同一连接、名字也不是
        // wanted（其余四级上全劣势，所以这一格钉的确实是第 1 级压过第 2 级）
        let mut prs = Profiles::new_default();
        prs.profiles = vec![prof(wanted, node.clone()), prof("v3-name", moved())];
        prs.active = Some("v3-name".into());
        assert_eq!(
            prs.pick_keeper(&[0, 1], &node, wanted, 1),
            1,
            "不打扰正在用的那一条：哪怕它是本批刚写入的"
        );

        // 第 2 级：导入前已存在的老名字压过本批刚写入的同端口条目
        let mut prs = Profiles::new_default();
        prs.profiles = vec![
            prof("hysteria2-1785892136", moved()),
            prof(wanted, node.clone()),
        ];
        assert_eq!(
            prs.pick_keeper(&[0, 1], &node, wanted, 1),
            0,
            "本批新条目（同一连接、名字还等于 wanted）顶不掉老名字"
        );

        // 第 3 级：同一连接
        let mut prs = Profiles::new_default();
        prs.profiles = vec![
            prof(wanted, moved()),
            prof("hysteria2-1785892136", node.clone()),
        ];
        assert_eq!(
            prs.pick_keeper(&[0, 1], &node, wanted, 2),
            1,
            "都是存量、都不是活动节点时，同一连接的那条接新数据"
        );

        // 第 4 级：名字等于 wanted
        let mut prs = Profiles::new_default();
        prs.profiles = vec![prof("hysteria2-1785892136", moved()), prof(wanted, moved())];
        assert_eq!(prs.pick_keeper(&[0, 1], &node, wanted, 2), 1);

        // 第 5 级：全部打平时取列表位置最前的（group 的给定顺序不算数）
        let mut prs = Profiles::new_default();
        prs.profiles = vec![
            prof("hysteria2-1785892136", moved()),
            prof("bob-hy2-direct-2", moved()),
        ];
        assert_eq!(prs.pick_keeper(&[1, 0], &node, wanted, 2), 0);
    }

    /// §5.1、§5.9：`find_account_before` 只看导入前已存在的那一段，且**不看** protected
    /// （import-v3 只跳过、不写，挡住冻结快照的回写正是要的）。
    #[test]
    fn find_account_before_ignores_profiles_added_in_this_run() {
        let mut prs = Profiles::new_default();
        // 0：面板来源、非活动——对 v3 来件受保护，但这里照样认得出它
        prs.profiles
            .push(prof("alice-hy2-direct", hy2_direct_node()));
        // 1：同账号的活动节点
        let mut act = prof("hysteria2-1785892136", hy2_direct_node());
        act.source = Source::V3;
        prs.profiles.push(act);
        // 2：本批刚写入的同账号条目
        prs.profiles
            .push(prof("alice-hy2-direct-2", hy2_direct_node()));
        prs.active = Some("hysteria2-1785892136".into());

        // 同账号、同端口、换了密码的来件：门槛内，但对 0 号条目不可替换
        let node = hy2_with(None, Some("rotated-pw"), None, None);
        assert!(
            protected(&prs.profiles[0], false, Source::V3)
                && !movable(&prs.profiles[0], false, &node, Source::V3),
            "前提：0 号条目对 v3 来件受保护、不可替换"
        );
        assert_eq!(
            prs.find_account_before(3, &node, Source::V3)
                .map(|p| p.name.as_str()),
            Some("hysteria2-1785892136"),
            "优先活动节点"
        );
        prs.active = None;
        assert_eq!(
            prs.find_account_before(3, &node, Source::V3)
                .map(|p| p.name.as_str()),
            Some("alice-hy2-direct"),
            "没有活动节点时取列表最前——受保护的条目照样算数"
        );
        assert_eq!(
            prs.find_account_before(0, &node, Source::V3)
                .map(|p| p.name.as_str()),
            None,
            "本批刚写入的一段不算"
        );
        prs.active = Some("alice-hy2-direct-2".into());
        assert_eq!(
            prs.find_account_before(1, &node, Source::V3)
                .map(|p| p.name.as_str()),
            Some("alice-hy2-direct"),
            "活动节点被 known 切在外面时只在前一段里找"
        );

        // 门槛照样管用：跨端口 + 猜 kind 的条目不算同一个账号位
        let mut guessed = Profiles::new_default();
        let mut g = prof(
            "v3-name",
            Node {
                label: "示例备注".into(),
                ..hy2_direct_node()
            },
        );
        g.source = Source::V3;
        guessed.profiles.push(g);
        assert_eq!(
            guessed
                .find_account_before(
                    1,
                    &Node {
                        label: "custom".into(),
                        port: 40003,
                        ..hy2_direct_node()
                    },
                    Source::V3,
                )
                .map(|p| p.name.as_str()),
            None,
            "门槛外不算"
        );
    }

    /// 名字里带 4.0.0 的订阅 token（spec §4、§8）。
    #[test]
    fn name_has_token_matches_4_0_0_token_names_only() {
        let token = "0123456789abcdef0123456789abcdef";
        assert!(name_has_token(&format!("{token}-hy2-resi")));
        assert!(name_has_token(&format!("{token}-hy2-resi-2")));
        assert!(name_has_token(&format!(
            "{}-hy2-resi",
            token.to_uppercase()
        )));
        assert!(name_has_token(token));
        assert!(
            !name_has_token(&format!("{}-hy2-resi", &token[..31])),
            "31 位不是 token"
        );
        assert!(!name_has_token("alice-hy2-resi"));
        assert!(!name_has_token("hysteria2-1785892136"));
        assert!(!name_has_token("panel.example.com-hy2-direct"));
    }

    /// 墓碑 key 推出的显示名：`<sanitize(host)>-<kind_slug>`，字段索引不写反。
    #[test]
    fn key_display_names_the_account_by_host_and_kind() {
        let key = tombstone_key(&hy2_resi_node());
        assert!(
            key.starts_with("hy2-resi|panel.example.com|"),
            "kind 在第 0 段、host 在第 1 段：{key}"
        );
        assert_eq!(key_display(&key), "panel.example.com-hy2-resi");
        assert_eq!(
            key_display(&tombstone_key(&crate::testutil::reality_direct_node())),
            "panel.example.com-reality-direct"
        );
        assert_eq!(
            key_display("hy2-direct|示例主机|0123456789abcdef"),
            "hy2-direct",
            "host 清洗后为空就退回 kind_slug"
        );
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

    /// 同一个账号位再埋一次墓碑：旧墓碑上的未知字段要搬到新墓碑（spec §6.3、§5.1 `bury`）。
    #[test]
    fn burying_the_same_account_twice_keeps_unknown_tombstone_fields() {
        let mut p = Profiles::new_default();
        p.bury(&prof("a", hy2_direct_node()), 1);
        p.deleted[0]
            .extra
            .insert("future_field".into(), serde_json::json!("keep"));
        // 另一个账号位垫在后面，才看得出新墓碑仍排末尾
        p.bury(&prof("other", hy2_resi_node()), 2);

        p.bury(&prof("a-again", hy2_direct_node()), 3);

        let key = tombstone_key(&hy2_direct_node());
        let hits: Vec<&Tombstone> = p.deleted.iter().filter(|t| t.key == key).collect();
        assert_eq!(hits.len(), 1, "同 key 只留一条：{:?}", p.deleted);
        assert_eq!(
            (hits[0].name.as_str(), hits[0].at),
            ("a-again", 3),
            "名字与时间跟着这次更新"
        );
        assert_eq!(
            hits[0].extra.get("future_field"),
            Some(&serde_json::json!("keep")),
            "旧墓碑上的未知字段搬到新墓碑"
        );
        assert_eq!(
            p.deleted.last().map(|t| t.key.as_str()),
            Some(key.as_str()),
            "新墓碑仍然移到末尾"
        );
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

    /// `Profiles` / `Profile` / `Tombstone` 三层的未知键读进来、写回去都逐字保留
    /// （spec §6.3 catch-all、§6.5）：以后的版本加了字段，降回本版不丢。
    #[test]
    fn unknown_fields_survive_a_round_trip_on_profiles_profile_and_tombstone() {
        let s = FakeSys::new();
        let mut p = Profiles::new_default();
        p.profiles.push(prof("alice-hy2-direct", hy2_direct_node()));
        p.active = Some("alice-hy2-direct".into());
        p.bury(&prof("gone", hy2_resi_node()), 11);
        p.extra.insert("future_top".into(), serde_json::json!(7));
        p.profiles[0]
            .extra
            .insert("future_profile".into(), serde_json::json!({"x": [1, 2]}));
        p.deleted[0]
            .extra
            .insert("future_tomb".into(), serde_json::json!("t"));
        p.save(&s, &paths()).unwrap();

        let loaded = Profiles::load(&s, &paths()).unwrap();
        assert_eq!(loaded, p, "读回来与写出去同构，未知键进了 extra");
        loaded.save(&s, &paths()).unwrap();

        let raw = s.get("/opt/bui-c/profiles.json").unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["future_top"], serde_json::json!(7));
        assert_eq!(
            v["profiles"][0]["future_profile"],
            serde_json::json!({"x": [1, 2]})
        );
        assert_eq!(v["deleted"][0]["future_tomb"], serde_json::json!("t"));
        assert!(!raw.contains("extra"), "未知键是平铺的，不套一层 extra");
    }

    /// §5.5、§14.2 T7：来源只升不降、同级不动，`imported_at` 不跟着变；
    /// `best_source` 与它同一张表。
    #[test]
    fn raise_source_only_goes_up_and_keeps_ties() {
        let all = [
            Source::ApiNodes,
            Source::Subscription,
            Source::Paste,
            Source::V3,
        ];
        for have in all {
            for want in all {
                let mut prs = Profiles::new_default();
                let mut p = prof("alice-hy2-direct", hy2_direct_node());
                p.source = have;
                prs.profiles.push(p);
                let up = want.rank() > have.rank();
                assert_eq!(
                    prs.raise_source("alice-hy2-direct", want),
                    up,
                    "条目 {have:?} 遇来源 {want:?}"
                );
                assert_eq!(
                    prs.profiles[0].source,
                    if up { want } else { have },
                    "只升不降，同级不动（Paste 与 V3 互遇也不动）"
                );
                assert_eq!(
                    prs.profiles[0].imported_at, "2026-09-11T00:00:00Z",
                    "来源升级不是内容变化，imported_at 不动（T7）"
                );
                // 同一张表：best_source 三者取等级最高，同级留条目原来的
                assert_eq!(
                    best_source(want, Some(have), false),
                    if up { want } else { have },
                    "best_source：条目 {have:?} 遇来件 {want:?}"
                );
                assert_eq!(
                    best_source(want, Some(have), true),
                    Source::ApiNodes,
                    "组里有面板成员，至少是 ApiNodes"
                );
            }
        }

        // 逐级升：V3 → Subscription → ApiNodes，升到顶就不再动
        let mut prs = Profiles::new_default();
        let mut p = prof("alice-hy2-direct", hy2_direct_node());
        p.source = Source::V3;
        prs.profiles.push(p);
        assert!(prs.raise_source("alice-hy2-direct", Source::Subscription));
        assert!(prs.raise_source("alice-hy2-direct", Source::ApiNodes));
        assert!(!prs.raise_source("alice-hy2-direct", Source::Subscription));
        assert_eq!(prs.profiles[0].source, Source::ApiNodes);
        assert!(
            !prs.raise_source("没有这个节点", Source::ApiNodes),
            "名字不存在：什么都不动"
        );

        // 账号组为空（新建节点）时没有留存者，来源就是来件的
        assert_eq!(best_source(Source::Paste, None, false), Source::Paste);
        assert_eq!(best_source(Source::Paste, None, true), Source::ApiNodes);
    }

    /// C2、§6.5：4.0.0 读一遍再写回去（墓碑与未知字段一起丢掉）之后，本版仍按账号
    /// 认得出同一条 profile——端口变了也是原地替换，不会另起一条。
    ///
    /// `Profile400` 五个字段、`Profiles400` 八个字段（不含 `deleted`），字段名逐字抄自
    /// `git show v4.0.0:crates/bui-c/src/profiles.rs`：`Profile` 在 57-65 行（五个字段在
    /// 59-64 行），`Profiles` 在 67-77 行（八个字段在 69-76 行）。
    #[test]
    fn a_4_0_0_shaped_rewrite_still_matches_the_same_node() {
        #[derive(Debug, Serialize, Deserialize)]
        struct Profile400 {
            name: String,
            node: Node,
            split: SplitRules,
            source: Source,
            imported_at: String,
        }
        #[derive(Debug, Serialize, Deserialize)]
        struct Profiles400 {
            schema_version: u32,
            active: Option<String>,
            mode: Mode,
            socks_port: u16,
            http_port: u16,
            auto_update: bool,
            panel: Option<Panel>,
            profiles: Vec<Profile400>,
        }

        // 第 1 步：本版写一份——token 名已经改成 `<主机>-<kind>`（§5.6），有墓碑，三层都有未知字段
        let s = FakeSys::new();
        let mut p = Profiles::new_default();
        let mut live = prof("panel.example.com-hy2-resi", hy2_resi_node());
        live.extra
            .insert("future_profile".into(), serde_json::json!("p"));
        p.profiles.push(live);
        p.active = Some("panel.example.com-hy2-resi".into());
        p.bury(&prof("gone", hy2_direct_node()), 11);
        p.deleted[0]
            .extra
            .insert("future_tomb".into(), serde_json::json!(true));
        p.extra.insert("future_top".into(), serde_json::json!(7));
        p.save(&s, &paths()).unwrap();

        // 第 2 步：以 4.0.0 的字段表读进来再写回去
        let raw = s.get("/opt/bui-c/profiles.json").unwrap();
        let old: Profiles400 = serde_json::from_str(&raw).unwrap();
        assert_eq!(old.profiles[0].name, "panel.example.com-hy2-resi");
        let mut back = serde_json::to_vec_pretty(&old).unwrap();
        back.push(b'\n');
        let back = String::from_utf8(back).unwrap();
        assert!(
            !back.contains("deleted"),
            "4.0.0 写回去必然丢掉墓碑，否则这个模拟不算数"
        );
        assert!(
            !back.contains("future_top")
                && !back.contains("future_profile")
                && !back.contains("future_tomb"),
            "三层（顶层 / profile / 墓碑）的未知字段也一起丢了"
        );
        s.put("/opt/bui-c/profiles.json", &back);

        // 第 3 步：本版再读——墓碑没了，但账号还认得出
        let after = Profiles::load(&s, &paths()).unwrap();
        assert!(after.deleted.is_empty());
        let moved = Node {
            port: 40009,
            hop: Some((41000, 50000)),
            ..hy2_resi_node()
        };
        assert_eq!(
            after.account_group(&moved, Source::ApiNodes),
            vec![0],
            "端口变了仍是同一个账号位"
        );
        assert_eq!(
            after.profiles[0].name, "panel.example.com-hy2-resi",
            "名字不变，原地替换"
        );
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
