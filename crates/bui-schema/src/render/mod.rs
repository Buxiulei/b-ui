//! 内核配置、订阅与客户端配置的渲染器。
pub mod client;
pub mod hysteria;
pub mod relay;
pub mod subscription;
pub mod xray;

use crate::keywords::DEFAULT_KEYWORDS;
use crate::model::ResidentialGroup;
use serde::{Deserialize, Serialize};

/// 订阅与客户端配置里的住宅分流规则（与 relay 同源）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitRules {
    /// 住宅池有效（enabled 且非空）
    pub enabled: bool,
    /// global 模式：全部走住宅
    pub global: bool,
    /// 分流关键字（跟随默认表或自定义）
    pub keywords: Vec<String>,
}

impl SplitRules {
    /// 从住宅分组推导分流规则。
    pub fn from_group(g: &ResidentialGroup) -> Self {
        let keywords = match &g.keywords {
            Some(k) if !k.is_empty() => k.clone(),
            _ => DEFAULT_KEYWORDS.iter().map(|s| s.to_string()).collect(),
        };
        Self {
            enabled: g.pool_active(),
            global: matches!(g.mode, crate::model::ResiMode::Global),
            keywords,
        }
    }
}
