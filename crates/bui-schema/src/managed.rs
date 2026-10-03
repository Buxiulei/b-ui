//! 受管配置的纯策略模型：明确出口身份、路径绑定与族/协议能力的时效判据。
//!
//! 证据由调用方提供，时间由调用方传入；本模块不探测网络、不授予账户权益。

use crate::nodes::{Node, NodeKind};
use serde::{Deserialize, Serialize};
use std::fmt;

/// 管理员明确选定的出口身份；身份之间不自动回落。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressIdentity {
    Vps,
    Residential,
}

/// 明确的客户端适配目标；枚举值不代表运行权限或真机能力已验收。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientTarget {
    V2raynMacOs,
    BuiCLinux,
}

/// 一个路径上的实际能力证据，时间为调用方提供的 Unix 秒。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub path_fingerprint: String,
    pub observed_at: i64,
    pub expires_at: i64,
    pub observed_identity: String,
}

impl fmt::Debug for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 字段可能来自外部输入，不能假定 fingerprint/identity 不含敏感数据。
        f.debug_struct("Evidence")
            .field("path_fingerprint", &"[redacted]")
            .field("observed_at", &self.observed_at)
            .field("expires_at", &self.expires_at)
            .field("observed_identity", &"[redacted]")
            .finish()
    }
}

/// 未知与明确不支持均不能作为转发许可；Verified 仍须检查路径和时间。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    #[default]
    Unknown,
    Unsupported,
    Verified(Evidence),
}

/// IPv4/IPv6 × TCP/UDP 分别保留证据，缺失字段保持未知。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EgressCapabilities {
    pub v4_tcp: EvidenceStatus,
    pub v4_udp: EvidenceStatus,
    pub v6_tcp: EvidenceStatus,
    pub v6_udp: EvidenceStatus,
}

/// 发布策略的纯快照；证据到期只影响判据，不自动改变发布 revision。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedPolicy {
    pub revision: u64,
    pub selected_identity: EgressIdentity,
    pub path_fingerprint: String,
    #[serde(default)]
    pub capabilities: EgressCapabilities,
}

impl fmt::Debug for ManagedPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedPolicy")
            .field("revision", &self.revision)
            .field("selected_identity", &self.selected_identity)
            .field("path_fingerprint", &"[redacted]")
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

/// 只允许路径精确匹配、已观测且未到期的证据：`observed_at <= now < expires_at`。
pub fn allows(status: &EvidenceStatus, path: &str, now: i64) -> bool {
    matches!(status, EvidenceStatus::Verified(e)
        if e.path_fingerprint == path && e.observed_at <= now && now < e.expires_at)
}

/// 从调用方已授权的节点中保留指定身份，维持原有顺序与全部节点/凭据字段。
///
/// 本函数不验证账户权益，不创建节点，也不补齐另一个协议；输入必须来自授权真源。
pub fn select_nodes(nodes: &[Node], identity: EgressIdentity) -> Vec<Node> {
    nodes
        .iter()
        .filter(|node| match identity {
            EgressIdentity::Residential => matches!(
                node.kind,
                NodeKind::Hy2Residential | NodeKind::RealityResidential
            ),
            EgressIdentity::Vps => {
                matches!(node.kind, NodeKind::Hy2Direct | NodeKind::RealityDirect)
            }
        })
        .cloned()
        .collect()
}
