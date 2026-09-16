//! 对账的类型契约：期望项 [`Artifact`]、模块接口 [`Module`]、一轮对账的系统事实
//! [`Facts`]，以及 v4 受管/ v3 遗留两份单元名单。
//!
//! 这里只定义「期望是什么」，不碰机器：比对在 [`diff::plan`]、落地在 [`apply::apply`]、
//! 非受管项的漂移在 [`drift`]。`AppState` / `EventBus` 在 [`crate::api::state`]（同属本任务，
//! 因为 [`Module::routes`] 与 [`DaemonCtx`] 都要引用它们）。

pub mod apply;
pub mod diff;
pub mod drift;

/// v4 受管的六个单元；health 端点、`/api/services` 白名单、漂移的 drop-in 扫描都用这一份。
///
/// 4.1 起 `hysteria-residential` 是**唯一**的住宅实例（一个 sing-box hysteria2 入站
/// `:40000`，整段跳跃由 [`Artifact::NftTable`] 的 `inet bui` 表 REDIRECT 过来），
/// 所以这份名单就是全部受管单元：`hysteria-residential-1..7` 进了 [`LEGACY_UNITS`]。
pub const MANAGED_UNITS: [&str; 6] = [
    "b-ui",
    "hysteria-server",
    "hysteria-residential",
    "xray",
    "b-ui-relay",
    "caddy",
];

/// v3 遗留的单元与定时器 + 4.0 的按槽住宅实例（v4.1 一概不生成）：units 模块产出
/// `UnitState{false,false}` + `Absent`，漂移扫描对「删不掉还在的」再报一次。
/// **唯一一份**，Task 5 与 Task 8 都引用它。
///
/// `hysteria-residential-1..7` 是 4.0 的「每住宅槽位一个 apernet hysteria 实例」
/// （spec §2.5）：4.1 只剩 `hysteria-residential` 这一个 sing-box 实例，带后缀的七个
/// 因此从受管变成遗留 —— 停掉、禁掉、删单元文件。它们留下的端口跳跃 NAT 规则由
/// [`crate::modules::portjump::cleanup_legacy_residential`] 在落 nft 表之前清掉。
pub const LEGACY_UNITS: [&str; 16] = [
    "hy2-watchdog.timer",
    "hy2-watchdog.service",
    "b-ui-cert-sync.timer",
    "b-ui-cert-sync.service",
    "b-ui-resi-health.timer",
    "b-ui-resi-health.service",
    "b-ui-admin.service",
    "hysteria-server@.service",
    "xray@.service",
    "hysteria-residential-1.service",
    "hysteria-residential-2.service",
    "hysteria-residential-3.service",
    "hysteria-residential-4.service",
    "hysteria-residential-5.service",
    "hysteria-residential-6.service",
    "hysteria-residential-7.service",
];

/// 住宅槽位上限（= `bui_schema::slots::MAX_SLOTS`）：4.1 只用它枚举**遗留**项
/// （`hysteria-residential-<i>` 与 `config-residential-<i>.yaml`），受管单元不再按它展开。
pub const MAX_RESI_SLOTS: u16 = bui_schema::slots::MAX_SLOTS;

/// 这个名字是不是 v4 的受管单元。**词法判定**（不读期望态）：就是那六个固定名字。
///
/// 4.1 起带后缀的 `hysteria-residential-<i>` 一律返回 false（它们在 [`LEGACY_UNITS`] 里，
/// spec §2.5）：`POST /api/services/{unit}/{action}` 的白名单不该再放行一个不存在的实例，
/// 而 `apply::is_managed_dropin_dir` 也不该把它的 drop-in 目录当成自己的地盘 ——
/// 那个目录里的残留由遗留清理整条删掉。
///
/// 给「我能不能动这个单元 / 这个 drop-in 目录是不是我的地盘」这类判断用
/// （`apply::is_managed_dropin_dir`、`/api/services` 白名单、漂移扫描）。
pub fn is_managed_unit(name: &str) -> bool {
    MANAGED_UNITS.contains(&name)
}

/// 按期望态枚举**此刻**该跑的受管单元。4.1 起住宅只有一个实例，所以它退化成
/// [`MANAGED_UNITS`] 的那六个；**签名保留**（体检的 services 表、`/api/services` 枚举、
/// `bui status`、自检、假机器播种都按它调）。
pub fn managed_units(_s: &State) -> Vec<String> {
    MANAGED_UNITS.iter().map(|x| x.to_string()).collect()
}

use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::{Host, Proto};
use anyhow::Result;
use bui_schema::model::State;
use bui_schema::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;

/// 配置变更后对单元做什么（spec §2.2 的重启映射：Caddy 是 reload，其余是 restart）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitAction {
    Restart,
    Reload,
}

/// 一个 systemd 单元 + 变更后要对它做的动作。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unit {
    pub name: String,
    pub action: UnitAction,
}

impl Unit {
    pub fn restart(name: &str) -> Self {
        Self {
            name: name.to_string(),
            action: UnitAction::Restart,
        }
    }

    pub fn reload(name: &str) -> Self {
        Self {
            name: name.to_string(),
            action: UnitAction::Reload,
        }
    }

    /// 补全成 systemd 的全名：`hysteria-server` → `hysteria-server.service`。
    pub fn service(&self) -> String {
        if self.name.contains('.') {
            self.name.clone()
        } else {
            format!("{}.service", self.name)
        }
    }
}

/// 写盘前用哪个内核校验候选内容（spec §2.2「验证再重启」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    Xray,
    SingBox,
    Caddy,
    Sshd,
}

/// sysctl 的值一律**按 ASCII 空白切分成 token 序列**再比：多值键在 `/proc` 里是**制表符**
/// 分隔（`sysctl -n net.ipv4.tcp_rmem` → `4096\t262144\t16777216`），而我们的 conf 与
/// `sysctl -w` 用空格。逐字节比会让 `net.ipv4.tcp_rmem` / `tcp_wmem` / `udp_mem` /
/// `ip_local_port_range` 这四个多值键每轮都进 `changed`（bwg-rick 真机 M1 step2 的
/// 「二次对账零变更」FAIL；单值键不复现）。
pub fn sysctl_tokens_eq(a: &str, b: &str) -> bool {
    a.split_ascii_whitespace().eq(b.split_ascii_whitespace())
}

/// 记账分隔符：sysctl 的值只有数字、字母、`,`、`-`、`.`，不会出现 `=>`。
const SYSCTL_CLAMP_SEP: &str = " => ";

/// 「写入值 → 内核实际生效值」的记账，存进 `runtime.restart_keys["sysctl:<key>"]`。
/// 切分后仍不等（内核钳制或重排，例如把 `tcp_rmem` 的最小值抬到一个页）时由 apply 写下，
/// 下一轮 [`diff::plan`] 拿它认账：读回值就是已生效值，不再每轮报 changed。
pub fn sysctl_clamped_record(want: &str, got: &str) -> String {
    format!(
        "{}{SYSCTL_CLAMP_SEP}{}",
        norm_sysctl(want),
        norm_sysctl(got)
    )
}

/// 记账是否仍然成立：期望值没变、且当前读回值就是记账里的已生效值
/// （期望值改了或值被人手改了都要重新写）。
pub fn sysctl_clamp_settled(record: &str, want: &str, current: &str) -> bool {
    record
        .split_once(SYSCTL_CLAMP_SEP)
        .is_some_and(|(w, g)| sysctl_tokens_eq(w, want) && sysctl_tokens_eq(g, current))
}

fn norm_sysctl(v: &str) -> String {
    v.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// 一条要放行的端口（单端口 = `from == to`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    pub proto: Proto,
    pub from: u16,
    pub to: u16,
}

impl PortSpec {
    pub fn one(proto: Proto, port: u16) -> Self {
        Self {
            proto,
            from: port,
            to: port,
        }
    }

    pub fn range(proto: Proto, from: u16, to: u16) -> Self {
        Self { proto, from, to }
    }

    /// ufw 语法：`22/tcp` 或 `20000:30000/udp`
    pub fn ufw(&self) -> String {
        let p = if self.proto == Proto::Tcp {
            "tcp"
        } else {
            "udp"
        };
        if self.from == self.to {
            format!("{}/{p}", self.from)
        } else {
            format!("{}:{}/{p}", self.from, self.to)
        }
    }

    /// firewalld 语法：`22/tcp` 或 `20000-30000/udp`
    pub fn firewalld(&self) -> String {
        let p = if self.proto == Proto::Tcp {
            "tcp"
        } else {
            "udp"
        };
        if self.from == self.to {
            format!("{}/{p}", self.from)
        } else {
            format!("{}-{}/{p}", self.from, self.to)
        }
    }
}

/// 一个期望项。模块只产出它，不自己动机器。
#[derive(Debug, Clone, PartialEq)]
pub enum Artifact {
    File {
        path: PathBuf,
        content: Vec<u8>,
        mode: u32,
        immutable: bool,
        restart: Option<Unit>,
        /// 只有这个 key 变了才重启（xray 的 `clients` 变化不重启，spec §3.3）
        restart_key: Option<String>,
        verify: Option<Verify>,
    },
    Unit {
        name: Unit,
        dropin: Option<String>,
        content: String,
    },
    UnitState {
        name: String,
        enabled: bool,
        active: bool,
    },
    Sysctl {
        key: String,
        value: String,
    },
    /// 内核模块必须**现在**加载（spec §2.2 的「nf_conntrack 模块」）；
    /// `/etc/modules-load.d` 只管下次开机，不写这条的话首装当轮 `sysctl -w net.netfilter.*` 全 ENOENT。
    Modprobe {
        module: String,
    },
    Binary {
        name: String,
        version: String,
        sha256: String,
        url: String,
    },
    /// CLI 入口符号链接（`/usr/local/bin/{bui,b-ui}` → `<base>/bin/bui`，spec §1、§2.4）
    Symlink {
        path: PathBuf,
        target: PathBuf,
    },
    FirewallPorts {
        ports: Vec<PortSpec>,
    },
    /// 一张由 b-ui 自管的 nft 表（C2 的第 10 个变体，4.1）。`ruleset` 是
    /// [`bui_schema::render::nft::ruleset`] 的整份产出：`table` 声明 + `flush table` +
    /// 完整定义，一个 `nft -f -` 事务原子替换，重放幂等。
    ///
    /// 目前只有住宅 HY2 端口跳跃那一张 `inet bui`：整段 `41000-50000`（+ 4.0 兼容段
    /// `40001-40007`）REDIRECT 到 `:40000`。**缺 `nft` 不是「跳跃失效」而是住宅 HY2 对
    /// 全体带 `mport` 的现役订阅全断**（客户端只往跳跃段发、从不发 `:40000`），所以
    /// `bui install` / `bui upgrade` 把 `nft` 当硬前置（退出码 2、一个字不落盘）。
    NftTable {
        family: String,
        name: String,
        ruleset: String,
    },
    Absent {
        path: PathBuf,
    },
}

impl Artifact {
    /// 默认 0600、不校验、不重启（最保守的一档；秘密文件占多数）。
    pub fn file(path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> Self {
        Artifact::File {
            path: path.into(),
            content: content.into(),
            mode: 0o600,
            immutable: false,
            restart: None,
            restart_key: None,
            verify: None,
        }
    }

    pub fn mode(mut self, mode: u32) -> Self {
        if let Artifact::File { mode: m, .. } = &mut self {
            *m = mode;
        }
        self
    }

    pub fn restart(mut self, unit: Unit) -> Self {
        if let Artifact::File { restart, .. } = &mut self {
            *restart = Some(unit);
        }
        self
    }

    pub fn restart_key(mut self, key: impl Into<String>) -> Self {
        if let Artifact::File { restart_key, .. } = &mut self {
            *restart_key = Some(key.into());
        }
        self
    }

    pub fn verify(mut self, v: Verify) -> Self {
        if let Artifact::File { verify, .. } = &mut self {
            *verify = Some(v);
        }
        self
    }

    pub fn immutable(mut self) -> Self {
        if let Artifact::File { immutable, .. } = &mut self {
            *immutable = true;
        }
        self
    }

    /// 稳定标识，用作 `runtime.restart_keys` 的键。
    pub fn id(&self) -> String {
        match self {
            Artifact::File { path, .. } => format!("file:{}", path.display()),
            Artifact::Unit { name, dropin, .. } => match dropin {
                Some(d) => format!("unit:{}:{d}", name.name),
                None => format!("unit:{}", name.name),
            },
            Artifact::UnitState { name, .. } => format!("unitstate:{name}"),
            Artifact::Sysctl { key, .. } => format!("sysctl:{key}"),
            Artifact::Modprobe { module } => format!("modprobe:{module}"),
            Artifact::Binary { name, .. } => format!("binary:{name}"),
            Artifact::Symlink { path, .. } => format!("symlink:{}", path.display()),
            Artifact::FirewallPorts { .. } => "firewall".to_string(),
            // 也是 `runtime.restart_keys` 里那条记账的键：`nft:inet:bui`
            Artifact::NftTable { family, name, .. } => format!("nft:{family}:{name}"),
            Artifact::Absent { path } => format!("absent:{}", path.display()),
        }
    }

    /// `/etc/systemd/system/<unit>` 或 `/etc/systemd/system/<unit>.d/<dropin>`。
    pub fn unit_path(name: &str, dropin: Option<&str>) -> PathBuf {
        let base = PathBuf::from("/etc/systemd/system");
        let unit = if name.contains('.') {
            name.to_string()
        } else {
            format!("{name}.service")
        };
        match dropin {
            Some(d) => base.join(format!("{unit}.d")).join(d),
            None => base.join(unit),
        }
    }
}

/// 一轮对账开始时探一次的系统事实（`render` 里不再碰 [`Host`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub mem_mb: u64,
    pub arch: String,
    pub hostname: String,
    pub has_ufw: bool,
    pub ufw_active: bool,
    pub has_firewalld: bool,
    pub firewalld_active: bool,
    pub ssh_unit: String,
    pub ssh_pubkeys: u32,
    pub systemd_resolved: bool,
    /// `nft list tables` 探到的表标识符（`"inet bui"` 这种「族 + 空格 + 表名」，与
    /// [`bui_schema::render::nft::TABLE`] 同一写法）。[`diff::plan`] 是纯函数、不许跑命令，
    /// 所以「`inet bui` 这张表还在不在」这一问由这里一次性探好带进去 ——
    /// 有人 `nft flush ruleset`（或 `nftables.service` 重启）把表刷掉时，
    /// 规则集哈希没变但表没了，只有这份事实能让下一轮重放它。
    pub nft_tables: std::collections::BTreeSet<String>,
}

impl Facts {
    pub fn probe(host: &dyn Host) -> Result<Facts> {
        let has_ufw = host.which("ufw");
        let ufw_active = has_ufw
            && host
                .run("ufw", &["status"])
                .map(|o| o.stdout.contains("Status: active"))
                .unwrap_or(false);
        let has_firewalld = host.which("firewall-cmd");
        let firewalld_active = has_firewalld && host.unit_is_active("firewalld").unwrap_or(false);
        let ssh_unit = if host.unit_exists("ssh.service").unwrap_or(false) {
            "ssh"
        } else {
            "sshd"
        };
        let keys = host
            .read_file(std::path::Path::new("/root/.ssh/authorized_keys"))?
            .map(|b| count_pubkeys(&String::from_utf8_lossy(&b)))
            .unwrap_or(0);
        Ok(Facts {
            mem_mb: host.mem_mb()?,
            arch: host.arch()?,
            hostname: host.hostname()?,
            has_ufw,
            ufw_active,
            has_firewalld,
            firewalld_active,
            ssh_unit: ssh_unit.to_string(),
            ssh_pubkeys: keys,
            systemd_resolved: host.unit_is_active("systemd-resolved").unwrap_or(false)
                || host
                    .unit_exists("systemd-resolved.service")
                    .unwrap_or(false),
            nft_tables: nft_tables(host),
        })
    }
}

/// `nft list tables` 的输出 → 表标识符集合（`table inet bui` → `"inet bui"`）。
/// 没有 `nft`、命令失败或输出认不出来都返回空集（那就等于「表不在」，下一轮重放一次，
/// 幂等无副作用；反过来误判成「在」会让被刷掉的表永远补不回来）。
fn nft_tables(host: &dyn Host) -> std::collections::BTreeSet<String> {
    if !host.which("nft") {
        return Default::default();
    }
    match host.run("nft", &["list", "tables"]) {
        Ok(o) if o.ok() => o.stdout.lines().filter_map(parse_nft_table_line).collect(),
        _ => Default::default(),
    }
}

/// `table inet bui` → `Some("inet bui")`（多余的空白归一，别的行返回 `None`）。
pub fn parse_nft_table_line(line: &str) -> Option<String> {
    let mut w = line.split_whitespace();
    (w.next()? == "table").then_some(())?;
    let family = w.next()?;
    let name = w.next()?;
    w.next().is_none().then(|| format!("{family} {name}"))
}

/// 非注释行且以 ssh-rsa/ssh-ed25519/ssh-dss/ecdsa-sha2- 开头（移植 `core.sh:1871`）。
/// 放在这里而不是 `modules::ssh`：`Facts::probe` 要用它，而 ssh 模块在后面的任务才落地。
pub fn count_pubkeys(text: &str) -> u32 {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter(|l| {
            l.starts_with("ssh-rsa")
                || l.starts_with("ssh-ed25519")
                || l.starts_with("ssh-dss")
                || l.starts_with("ecdsa-sha2-")
        })
        .count() as u32
}

/// 渲染上下文：路径布局 + 本轮探到的事实。
pub struct RenderCtx {
    pub paths: Paths,
    pub facts: Facts,
}

/// P2/P3 各实现一个，注册在 `serve.rs` 的 `modules()`。
pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;
    /// 期望项；纯函数，不碰机器。
    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact>;
    /// 该模块的 HTTP 路由（默认没有）。
    fn routes(&self) -> axum::Router<crate::api::AppState> {
        axum::Router::new()
    }
    /// 无鉴权路由：订阅、`/api/nodes`、嵌入前端、`/packages/*`、用户域桩。
    /// `api::router()` 把它合并在 `require_admin` **外面**（裁决 D1）。
    fn public_routes(&self) -> axum::Router<crate::api::AppState> {
        axum::Router::new()
    }
    /// 该模块的后台任务（默认没有）。
    fn spawn(&self, _ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        Vec::new()
    }
}

/// 传给 [`Module::spawn`] 的守护进程句柄。
#[derive(Clone)]
pub struct DaemonCtx {
    pub store: Store,
    pub runtime: Runtime,
    pub bus: crate::api::EventBus,
    pub host: Arc<dyn Host>,
    pub paths: Paths,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;

    #[test]
    fn probes_firewall_ssh_and_resolved_facts() {
        let h = FakeHost::new();
        h.with(|i| {
            i.mem_mb = 3072;
            i.which.insert("ufw".into());
            i.scripted.push((
                "ufw status".into(),
                crate::sys::CmdOut::success("Status: active\n"),
            ));
            i.units_exist.insert("ssh.service".into());
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (
                    b"# comment\nssh-ed25519 AAAA me@host\necdsa-sha2-nistp256 BBBB other\n"
                        .to_vec(),
                    0o600,
                ),
            );
            i.units_active.insert("systemd-resolved.service".into());
        });
        let f = Facts::probe(&h).unwrap();
        assert_eq!(f.mem_mb, 3072);
        assert!(f.has_ufw && f.ufw_active);
        assert!(!f.has_firewalld && !f.firewalld_active);
        assert_eq!(f.ssh_unit, "ssh", "有 ssh.service 就用 ssh，否则 sshd");
        assert_eq!(f.ssh_pubkeys, 2);
        assert!(f.systemd_resolved);
        assert_eq!(f.arch, "x86_64");
        assert_eq!(f.hostname, "node-a");
    }

    #[test]
    fn defaults_to_sshd_when_no_ssh_unit_and_zero_pubkeys() {
        let h = FakeHost::new();
        let f = Facts::probe(&h).unwrap();
        assert_eq!(f.ssh_unit, "sshd");
        assert_eq!(f.ssh_pubkeys, 0);
        assert!(!f.systemd_resolved);
    }

    #[test]
    fn unit_paths_cover_dropins() {
        assert_eq!(
            Artifact::unit_path("xray", None),
            std::path::PathBuf::from("/etc/systemd/system/xray.service")
        );
        assert_eq!(
            Artifact::unit_path("xray", Some("99-b-ui-override.conf")),
            std::path::PathBuf::from("/etc/systemd/system/xray.service.d/99-b-ui-override.conf")
        );
        assert_eq!(
            Unit::restart("hysteria-server").service(),
            "hysteria-server.service"
        );
    }

    #[test]
    fn managed_and_legacy_unit_lists_are_the_single_source_of_truth() {
        assert_eq!(MANAGED_UNITS.len(), 6);
        assert_eq!(
            LEGACY_UNITS.len(),
            16,
            "9 个 v3 遗留 + 7 个 4.0 的按槽住宅实例"
        );
        assert!(
            MANAGED_UNITS.contains(&"b-ui-relay"),
            "v4 自己的 relay 单元必须在受管列表里"
        );
        // v3 遗留列表绝不能含 v4 受管单元，否则对账刚写完就被卸载/清理掉
        for m in MANAGED_UNITS {
            assert!(
                !LEGACY_UNITS.contains(&format!("{m}.service").as_str()),
                "{m} 同时出现在受管与遗留列表里"
            );
        }
        assert!(LEGACY_UNITS.contains(&"hysteria-server@.service"));
        assert!(LEGACY_UNITS.contains(&"xray@.service"));
        assert!(LEGACY_UNITS.contains(&"b-ui-admin.service"));
    }

    /// 4.1（spec §2.5）：带后缀的住宅实例从受管变成遗留，`is_managed_unit` 对它们一律 false。
    /// 词法判定不许再放行一个不存在的实例 —— `/api/services` 的白名单与
    /// `apply::is_managed_dropin_dir` 都按它，放行等于允许动一个已退役的单元。
    #[test]
    fn is_managed_unit_accepts_exactly_the_six() {
        for m in MANAGED_UNITS {
            assert!(is_managed_unit(m), "{m}");
        }
        for i in 1..MAX_RESI_SLOTS {
            let n = format!("hysteria-residential-{i}");
            assert!(!is_managed_unit(&n), "{n} 是遗留单元，不再受管");
            assert!(
                LEGACY_UNITS.contains(&format!("{n}.service").as_str()),
                "{n} 不在遗留清单里"
            );
        }
        assert!(!is_managed_unit("hysteria-residential-8"));
        assert!(!is_managed_unit("hysteria-residential-x"));
        assert!(!is_managed_unit("hysteria-residential-01"));
        assert!(!is_managed_unit("ssh"));
        for l in LEGACY_UNITS {
            assert!(!is_managed_unit(
                l.trim_end_matches(".service").trim_end_matches(".timer")
            ));
        }
    }

    #[test]
    fn managed_units_is_always_the_six_fixed_names() {
        let mut s = crate::testutil::sample_state();
        let six: Vec<String> = MANAGED_UNITS.iter().map(|x| x.to_string()).collect();
        assert_eq!(managed_units(&s), six);
        // 4.1：槽位表不再影响受管单元数（住宅只有一个 sing-box 实例）
        s.residential.slots = (0..3)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        assert_eq!(managed_units(&s), six, "增槽不再多出受管单元");
        assert!(managed_units(&s).iter().all(|n| is_managed_unit(n)));
    }

    /// `diff` 是纯函数，「表还在不在」只能靠这份事实（本轮探一次）。
    #[test]
    fn probes_the_nft_table_list_when_nft_is_present() {
        let h = FakeHost::new();
        let f = Facts::probe(&h).unwrap();
        assert!(f.nft_tables.is_empty(), "PATH 上没有 nft 就是空集");
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                crate::sys::CmdOut::success(
                    "table inet bui
table ip hysteria_c66a02d9
table ip6 nat
",
                ),
            ));
        });
        let f = Facts::probe(&h).unwrap();
        assert!(f.nft_tables.contains(bui_schema::render::nft::TABLE));
        assert!(f.nft_tables.contains("ip hysteria_c66a02d9"));
        assert_eq!(f.nft_tables.len(), 3);
    }

    #[test]
    fn nft_table_lines_are_parsed_conservatively() {
        assert_eq!(
            parse_nft_table_line("table inet bui").as_deref(),
            Some("inet bui")
        );
        assert_eq!(
            parse_nft_table_line("  table   ip   hysteria_x  ").as_deref(),
            Some("ip hysteria_x")
        );
        // 多一列（`table inet bui { … }` 这种一行式）与缺列都不认
        assert_eq!(parse_nft_table_line("table inet bui {"), None);
        assert_eq!(parse_nft_table_line("table inet"), None);
        assert_eq!(parse_nft_table_line(""), None);
        assert_eq!(parse_nft_table_line("chain inet bui"), None);
    }

    #[test]
    fn the_nft_table_artifact_is_keyed_by_family_and_name() {
        assert_eq!(
            Artifact::NftTable {
                family: "inet".into(),
                name: "bui".into(),
                ruleset: "table inet bui
"
                .into(),
            }
            .id(),
            "nft:inet:bui"
        );
    }
}
