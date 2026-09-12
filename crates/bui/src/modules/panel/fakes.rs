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
    /// 进程里「正在跑」的规则表，按表序：`(ruleTag, outboundTag)`
    pub rules: Vec<(String, String)>,
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

    pub fn rules(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().rules.clone()
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

    async fn add_rule(&self, rule: &bui_schema::render::xray::SlotRule) -> anyhow::Result<()> {
        let key = format!("add-rule:{}:{}", rule.rule_tag, rule.outbound_tag);
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) || i.fail_on.contains("add-rule") {
            anyhow::bail!("fake AddRule 失败：{key}");
        }
        // 真实内核：重名 ruleTag ⇒ 整条请求报错（D7 事实①）
        if i.rules.iter().any(|(t, _)| *t == rule.rule_tag) {
            anyhow::bail!("duplicate ruleTag {}", rule.rule_tag);
        }
        // 真实内核：shouldAppend=true ⇒ 落在表尾（D7 事实③）
        i.rules
            .push((rule.rule_tag.clone(), rule.outbound_tag.clone()));
        Ok(())
    }

    async fn remove_rule(&self, rule_tag: &str) -> anyhow::Result<()> {
        let key = format!("remove-rule:{rule_tag}");
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) || i.fail_on.contains("remove-rule") {
            anyhow::bail!("fake RemoveRule 失败：{key}");
        }
        // 真实内核：tag 不存在也算成功（D7 事实②）
        i.rules.retain(|(t, _)| t != rule_tag);
        Ok(())
    }

    async fn list_rules(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push("list-rules".into());
        if i.fail_on.contains("list-rules") {
            anyhow::bail!("fake ListRule 失败");
        }
        Ok(i.rules.clone())
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

    /// fake 的规则表必须与真实内核同语义（D7 的三条事实），否则 T4 的收敛测试是在测假东西。
    /// 真身由 `panel::xray::tests::routing_rules_round_trip_against_a_real_xray` 打在真 xray 上。
    #[tokio::test]
    async fn fake_xray_mirrors_the_three_routing_rule_facts() {
        use bui_schema::render::xray::SlotRule;
        let rule = |tag: &str, out: &str| SlotRule {
            rule_tag: tag.to_string(),
            inbound_tag: "vless-residential".into(),
            emails: vec![],
            outbound_tag: out.to_string(),
        };
        let x = FakeXray::new();
        x.add_rule(&rule("resi-fallback", "relay-slot-0"))
            .await
            .unwrap();
        x.add_rule(&rule("resi-u-a", "relay-slot-1")).await.unwrap();
        // 事实③：追加即表尾
        assert_eq!(
            x.rules(),
            vec![
                ("resi-fallback".to_string(), "relay-slot-0".to_string()),
                ("resi-u-a".to_string(), "relay-slot-1".to_string()),
            ]
        );
        // 事实①：重名 ruleTag 报错
        assert!(x.add_rule(&rule("resi-u-a", "relay-slot-2")).await.is_err());
        // 事实②：删不存在的 tag 也算成功
        x.remove_rule("resi-u-nobody").await.unwrap();
        // 删+追加把兜底挪回表尾
        x.remove_rule("resi-fallback").await.unwrap();
        x.add_rule(&rule("resi-fallback", "relay-slot-0"))
            .await
            .unwrap();
        assert_eq!(
            x.list_rules().await.unwrap(),
            vec![
                ("resi-u-a".to_string(), "relay-slot-1".to_string()),
                ("resi-fallback".to_string(), "relay-slot-0".to_string()),
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
