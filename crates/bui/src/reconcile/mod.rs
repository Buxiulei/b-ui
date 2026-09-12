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
pub const MANAGED_UNITS: [&str; 6] = [
    "b-ui",
    "hysteria-server",
    "hysteria-residential",
    "xray",
    "b-ui-relay",
    "caddy",
];

/// v3 遗留的单元与定时器（v4 一概不生成）：units 模块产出 `UnitState{false,false}` + `Absent`，
/// 漂移扫描对「删不掉还在的」再报一次。**唯一一份**，Task 5 与 Task 8 都引用它。
pub const LEGACY_UNITS: [&str; 9] = [
    "hy2-watchdog.timer",
    "hy2-watchdog.service",
    "b-ui-cert-sync.timer",
    "b-ui-cert-sync.service",
    "b-ui-resi-health.timer",
    "b-ui-resi-health.service",
    "b-ui-admin.service",
    "hysteria-server@.service",
    "xray@.service",
];

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
        })
    }
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
}
