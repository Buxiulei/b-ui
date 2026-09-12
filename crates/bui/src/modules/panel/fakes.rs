//! 测试用的内存 fake（`#[cfg(test)]`）：把 gRPC 与 hysteria HTTP 全挡在进程内。

use super::{Hy2Api, TxRx, XrayApi};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct FakeXrayInner {
    /// `"add:<tag>:<user_id>:<uuid>"` / `"remove:<tag>:<user_id>"` / `"query"`
    pub calls: Vec<String>,
    /// `query_user_deltas` 的下一次返回值（返回后清空，对应 `reset=true` 语义）
    pub deltas: BTreeMap<String, TxRx>,
    /// 命中就返回 `Err`，用来测退路
    pub fail_on: BTreeSet<String>,
    /// 指定某次失败的错误串（键同 `fail_on`），不给就用默认串。
    /// 真实 xray 在「email 已存在」与「email 不存在」时都报错，而这两种错误 Task 6
    /// 按成功处理——错误串不可配就没法给那条容错写正向测试。
    pub error_text: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
pub struct FakeXray(Arc<Mutex<FakeXrayInner>>);

impl FakeXray {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeXrayInner)) -> &Self {
        f(&mut self.0.lock().unwrap());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.0.lock().unwrap().calls.clear();
    }
}

#[async_trait::async_trait]
impl XrayApi for FakeXray {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()> {
        let key = format!("add:{tag}:{user_id}:{vless_uuid}");
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake AddUser 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        Ok(())
    }

    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()> {
        let key = format!("remove:{tag}:{user_id}");
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake RemoveUser 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        Ok(())
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push("query".into());
        if i.fail_on.contains("query") {
            anyhow::bail!("fake QueryStats 失败");
        }
        Ok(std::mem::take(&mut i.deltas))
    }
}

#[derive(Default)]
pub struct FakeHy2Inner {
    /// `"traffic:<port>"` / `"online:<port>"` / `"kick:<port>:<ids 逗号连接>"`
    pub calls: Vec<String>,
    /// 端口 → 下一次 `/traffic` 的返回值（返回后清空，对应 `clear=1` 语义）
    pub traffic: BTreeMap<u16, BTreeMap<String, TxRx>>,
    pub online: BTreeMap<u16, BTreeMap<String, u32>>,
    pub fail_ports: BTreeSet<u16>,
}

#[derive(Clone, Default)]
pub struct FakeHy2(Arc<Mutex<FakeHy2Inner>>);

impl FakeHy2 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeHy2Inner)) -> &Self {
        f(&mut self.0.lock().unwrap());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.0.lock().unwrap().calls.clear();
    }
}

#[async_trait::async_trait]
impl Hy2Api for FakeHy2 {
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("traffic:{port}"));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /traffic 失败：{port}");
        }
        Ok(i.traffic.remove(&port).unwrap_or_default())
    }

    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("online:{port}"));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /online 失败：{port}");
        }
        Ok(i.online.get(&port).cloned().unwrap_or_default())
    }

    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("kick:{port}:{}", ids.join(",")));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /kick 失败：{port}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::{Hy2Api, TxRx, XrayApi};
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    #[tokio::test]
    async fn fake_xray_records_calls_and_drains_deltas() {
        let x = FakeXray::new();
        x.with(|i| {
            i.deltas.insert("u-1".into(), TxRx { tx: 10, rx: 20 });
        });
        let uid = Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa").unwrap();
        let vid = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        x.add_user("vless-direct", uid, vid).await.unwrap();
        x.remove_user("vless-residential", uid).await.unwrap();
        assert_eq!(x.query_user_deltas().await.unwrap().len(), 1);
        assert!(
            x.query_user_deltas().await.unwrap().is_empty(),
            "增量取走即清空（reset=true 语义）"
        );
        assert_eq!(
            x.calls(),
            vec![
                format!("add:vless-direct:{uid}:{vid}"),
                format!("remove:vless-residential:{uid}"),
                "query".to_string(),
                "query".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn fake_xray_can_be_made_to_fail_one_call_with_a_chosen_message() {
        let x = FakeXray::new();
        let uid = Uuid::nil();
        let key = format!("remove:vless-direct:{uid}");
        x.with(|i| {
            i.fail_on.insert(key.clone());
        });
        let default_err = x
            .remove_user("vless-direct", uid)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            default_err.contains("fake RemoveUser"),
            "默认错误串：{default_err}"
        );
        assert!(x.remove_user("vless-residential", uid).await.is_ok());
        // 错误串可配：Task 6 要靠它测 xray 的「已存在 / 不存在」容错（那两种错误按成功处理）
        x.with(|i| {
            i.error_text.insert(key, "User 0 not found.".into());
        });
        assert_eq!(
            x.remove_user("vless-direct", uid)
                .await
                .unwrap_err()
                .to_string(),
            "User 0 not found."
        );
    }

    #[tokio::test]
    async fn fake_hy2_serves_per_port_replies_and_records_kicks() {
        let k = FakeHy2::new();
        k.with(|i| {
            i.traffic.insert(
                9999,
                BTreeMap::from([("u-1".to_string(), TxRx { tx: 1, rx: 2 })]),
            );
            i.online
                .insert(9998, BTreeMap::from([("u-1".to_string(), 3u32)]));
            i.fail_ports.insert(1234);
        });
        assert_eq!(k.traffic_clear(9999).await.unwrap().len(), 1);
        assert!(
            k.traffic_clear(9999).await.unwrap().is_empty(),
            "clear=1 语义：取走即清空"
        );
        assert_eq!(k.online(9998).await.unwrap()["u-1"], 3);
        assert!(
            k.online(9999).await.unwrap().is_empty(),
            "没播种的端口返回空表，不报错"
        );
        assert!(k.traffic_clear(1234).await.is_err());
        k.kick(9999, &["u-1".to_string(), "u-2".to_string()])
            .await
            .unwrap();
        assert!(k.calls().contains(&"kick:9999:u-1,u-2".to_string()));
    }
}
