//! Xray gRPC 客户端。**Task 4 填实**（tonic + vendored proto）。
//!
//! Task 1 只给 `XrayClient` 的空壳，好让 `PanelModule::new()` 从 Task 1 起就能编译、
//! 且它的签名此后不再变动；空壳的每个方法都返回 `Err`（绝不假装成功）。

use super::{TxRx, XrayApi};
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct XrayClient {
    addr: String,
}

impl XrayClient {
    pub fn new() -> Self {
        Self::with_addr(super::XRAY_API_ADDR)
    }

    pub fn with_addr(addr: impl Into<String>) -> Self {
        Self { addr: addr.into() }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }
}

impl Default for XrayClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl XrayApi for XrayClient {
    async fn add_user(&self, _tag: &str, _user_id: Uuid, _vless_uuid: Uuid) -> anyhow::Result<()> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }

    async fn remove_user(&self, _tag: &str, _user_id: Uuid) -> anyhow::Result<()> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }
}
