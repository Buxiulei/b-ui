//! 两个 hysteria 的 trafficStats HTTP API 客户端。**Task 5 填实**（reqwest 异步）。
//!
//! Task 1 只给 `Hy2Client` 的空壳（签名此后不再变动），每个方法都返回 `Err`。

use super::{Hy2Api, TxRx};
use std::collections::BTreeMap;

pub struct Hy2Client {
    base: String,
    secret: String,
}

impl Hy2Client {
    pub fn new() -> Self {
        Self::with_base("http://127.0.0.1")
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            secret: super::HY2_STATS_SECRET.to_string(),
        }
    }

    pub fn url(&self, port: u16, path: &str) -> String {
        format!("{}:{}{}", self.base.trim_end_matches('/'), port, path)
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl Default for Hy2Client {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hy2Api for Hy2Client {
    async fn traffic_clear(&self, _port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }

    async fn online(&self, _port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }

    async fn kick(&self, _port: u16, _ids: &[String]) -> anyhow::Result<()> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }
}
