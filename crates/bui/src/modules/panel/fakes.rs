//! 测试用的内存 fake（`#[cfg(test)]`）：把 gRPC 与 hysteria HTTP 全挡在进程内。

use super::hy2resi::Hy2ResiConn;
use super::{Hy2Api, Hy2ResiApi, TxRx, XrayApi};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct FakeXrayInner {
    /// **写**与统计的调用：`"add:<tag>:<user_id>:<uuid>"` / `"remove:<tag>:<user_id>"` /
    /// `"query"` / `"add-rule:…"` / `"remove-rule:…"` / `"list-rules"`。
    /// `inbound_user_uuid` 那种纯读调用记在 [`FakeXrayInner::gets`]，不混进来 ——
    /// 断言「这一轮没动内核」看的就是本列表。
    pub calls: Vec<String>,
    /// `inbound_user_uuid` 的调用：`"get:<tag>:<user_id>"`
    pub gets: Vec<String>,
    /// 内核里「现在挂着」的用户：`(tag, user_id)` → vless uuid。`add_user` / `remove_user`
    /// 成功时跟着变，`inbound_user_uuid` 从这里读 —— 测试要模拟「内核里挂着旧 uuid」
    /// 直接往这里塞（`sync_users` 先读后写，光靠 `error_text` 已经描述不了内核状态）。
    pub users: BTreeMap<(String, Uuid), Uuid>,
    /// `query_user_deltas` 的下一次返回值（返回后清空，对应 `reset=true` 语义）
    pub deltas: BTreeMap<String, TxRx>,
    /// 命中就返回 `Err`，用来测退路
    pub fail_on: BTreeSet<String>,
    /// 只 `add_user` 认：命中就返回 `Err` **一次**（命中即移除），用来模拟
    /// 「第一次 AddUser 撞上同名 email，摘掉占位的那个再发一次就成」——
    /// 真 xray 的应答（`users::sync_users` 的「摘掉再加」路径，2026-09-14 裁决）。
    pub fail_once: BTreeSet<String>,
    /// 指定某次失败的错误串（键同 `fail_on` / `fail_once`），不给就用默认串。
    /// 真实 xray 在「email 已存在」与「email 不存在」时都报错，而这两种错误各有各的
    /// 处理口径（remove 按成功、add 摘掉再加）——错误串不可配就没法给它们写正向测试。
    pub error_text: BTreeMap<String, String>,
    /// 进程里「正在跑」的规则表，按表序：`(ruleTag, outboundTag)`
    pub rules: Vec<(String, String)>,
    /// `ListRule` 接下来这么多次返回 **`tonic::Status::unavailable`**（文案仿真机的
    /// `tcp connect error`），每次减一、用完即恢复：模拟 xray 刚重启、还没起 10085 监听的
    /// 那一两秒（2026-09-13 bwg-rick）。`u32::MAX` ≈ 一直连不上。
    pub list_rules_unavailable: u32,
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
        let mut i = self.0.lock().unwrap();
        i.calls.clear();
        i.gets.clear();
    }

    pub fn gets(&self) -> Vec<String> {
        self.0.lock().unwrap().gets.clone()
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
        if i.fail_on.contains(&key) || i.fail_once.remove(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake AddUser 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        i.users.insert((tag.to_string(), user_id), vless_uuid);
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
        i.users.remove(&(tag.to_string(), user_id));
        Ok(())
    }

    async fn inbound_user_uuid(&self, tag: &str, user_id: Uuid) -> anyhow::Result<Option<Uuid>> {
        let key = format!("get:{tag}:{user_id}");
        let mut i = self.0.lock().unwrap();
        i.gets.push(key.clone());
        if i.fail_on.contains(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake GetInboundUsers 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        Ok(i.users.get(&(tag.to_string(), user_id)).copied())
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
        if i.list_rules_unavailable > 0 {
            i.list_rules_unavailable -= 1;
            return Err(tonic::Status::unavailable(
                "tcp connect error: Connection refused (os error 111)",
            )
            .into());
        }
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

pub struct FakeHy2ResiInner {
    /// `"query"` / `"connections"` / `"close:<id>"` / `"selected_all"` /
    /// `"select:<selector>:<tag>"` / `"ready"`
    /// （记法是契约：T11 的用例按这些字面量数调用次数）
    pub calls: Vec<String>,
    /// `query_user_deltas` 的下一次返回值（返回后清空，对应 `reset=true` 语义）
    pub deltas: BTreeMap<String, TxRx>,
    /// `GET /connections` 的返回值
    pub conns: Vec<Hy2ResiConn>,
    /// 门位现状：selector tag → 当前成员。`select` 成功会改这里，`selected_all` 从这里读
    /// —— T11 的门位收敛要靠「写完能读回来」才测得出幂等。
    pub selected: BTreeMap<String, String>,
    /// `ready()` 的返回值（默认 true；置 false 模拟 sing-box 刚重启还没起监听）
    pub ready: bool,
    /// 命中就返回 `Err`（键同 `calls` 的记法），用来测某一条调用长期失败
    pub fail_on: BTreeSet<String>,
    /// 下一次**任何**调用失败一次（命中即清空），错误串取这里 ——
    /// T11 的 fail-closed 测试要的就是「这一次切门没成」。
    pub fail_next: Option<String>,
}

impl Default for FakeHy2ResiInner {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            deltas: BTreeMap::new(),
            conns: Vec::new(),
            selected: BTreeMap::new(),
            ready: true,
            fail_on: BTreeSet::new(),
            fail_next: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct FakeHy2Resi(Arc<Mutex<FakeHy2ResiInner>>);

impl FakeHy2Resi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeHy2ResiInner)) -> &Self {
        f(&mut self.0.lock().unwrap());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.0.lock().unwrap().calls.clear();
    }

    pub fn set_deltas(&self, deltas: BTreeMap<String, TxRx>) {
        self.0.lock().unwrap().deltas = deltas;
    }

    /// `(id, rule)` 对 → 连接表；`rule` 就是 sing-box 那个
    /// `auth_user=<name> => route(gate-<id>)` 字符串。
    pub fn set_conns(&self, conns: Vec<(&str, &str)>) {
        self.0.lock().unwrap().conns = conns
            .into_iter()
            .map(|(id, rule)| Hy2ResiConn {
                id: id.to_string(),
                rule: rule.to_string(),
                chains: Vec::new(),
            })
            .collect();
    }

    pub fn set_selected(&self, selected: BTreeMap<String, String>) {
        self.0.lock().unwrap().selected = selected;
    }

    pub fn selected(&self) -> BTreeMap<String, String> {
        self.0.lock().unwrap().selected.clone()
    }

    pub fn set_ready(&self, ready: bool) {
        self.0.lock().unwrap().ready = ready;
    }

    /// `ready()` 永远为假：住宅 sing-box 的 Clash API 一直起不来，重放只能停在 `deny`
    /// （T11 的「重放预算用完」用例）。
    pub fn set_never_ready(&self) {
        self.set_ready(false);
    }

    /// 下一次任何调用失败一次（错误串是 `msg`）
    pub fn fail_next(&self, msg: &str) {
        self.0.lock().unwrap().fail_next = Some(msg.to_string());
    }

    /// 记一次调用；该失败就返回 `Err`。
    fn record(&self, key: String) -> anyhow::Result<()> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if let Some(msg) = i.fail_next.take() {
            anyhow::bail!("{msg}");
        }
        if i.fail_on.contains(&key) {
            anyhow::bail!("fake Hy2ResiApi 失败：{key}");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Hy2ResiApi for FakeHy2Resi {
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        self.record("query".into())?;
        Ok(std::mem::take(&mut self.0.lock().unwrap().deltas))
    }

    async fn connections(&self) -> anyhow::Result<Vec<Hy2ResiConn>> {
        self.record("connections".into())?;
        Ok(self.0.lock().unwrap().conns.clone())
    }

    async fn close_connection(&self, id: &str) -> anyhow::Result<()> {
        self.record(format!("close:{id}"))?;
        self.0.lock().unwrap().conns.retain(|c| c.id != id);
        Ok(())
    }

    async fn selected_all(&self) -> anyhow::Result<BTreeMap<String, String>> {
        self.record("selected_all".into())?;
        Ok(self.0.lock().unwrap().selected.clone())
    }

    async fn select(&self, selector: &str, tag: &str) -> anyhow::Result<()> {
        self.record(format!("select:{selector}:{tag}"))?;
        self.0
            .lock()
            .unwrap()
            .selected
            .insert(selector.to_string(), tag.to_string());
        Ok(())
    }

    async fn ready(&self) -> bool {
        let mut i = self.0.lock().unwrap();
        i.calls.push("ready".into());
        i.ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::{Hy2Api, Hy2ResiApi, TxRx, XrayApi};
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

    /// 住宅这条路的 fake 必须记账 + 可注入失败：T10 的采样与 T11 的门位收敛
    /// 全靠「调用序列」与「下一次失败」这两件事写测试。
    #[tokio::test]
    async fn fake_hy2resi_records_calls_replays_selections_and_can_fail_once() {
        let r = FakeHy2Resi::new();
        assert!(r.ready().await, "默认可用");
        r.set_never_ready();
        assert!(!r.ready().await);
        r.set_ready(true);
        assert!(r.ready().await);

        r.set_deltas(BTreeMap::from([(
            "r000".to_string(),
            TxRx { tx: 1, rx: 2 },
        )]));
        assert_eq!(r.query_user_deltas().await.unwrap().len(), 1);
        assert!(
            r.query_user_deltas().await.unwrap().is_empty(),
            "增量取走即清空（reset=true 语义）"
        );

        r.set_conns(vec![
            ("c1", "auth_user=r000 => route(gate-r000)"),
            ("c2", "final"),
        ]);
        assert_eq!(r.connections().await.unwrap().len(), 2);
        r.close_connection("c1").await.unwrap();
        assert_eq!(
            r.connections().await.unwrap()[0].id,
            "c2",
            "关掉的连接不再出现在 /connections 里"
        );

        // 切门写完能读回来（门位收敛的幂等判据）
        r.select("gate-r000", "slot-0-out").await.unwrap();
        assert_eq!(
            r.selected_all().await.unwrap(),
            BTreeMap::from([("gate-r000".to_string(), "slot-0-out".to_string())])
        );

        assert_eq!(
            r.calls(),
            vec![
                "ready",
                "ready",
                "ready",
                "query",
                "query",
                "connections",
                "close:c1",
                "connections",
                "select:gate-r000:slot-0-out",
                "selected_all",
            ]
        );

        // 下一次调用失败一次，之后自愈
        r.fail_next("v2ray_api 不可达");
        assert_eq!(
            r.query_user_deltas().await.unwrap_err().to_string(),
            "v2ray_api 不可达"
        );
        assert!(r.query_user_deltas().await.is_ok());

        // 指定某一条调用长期失败
        r.with(|i| {
            i.fail_on.insert("select:gate-r001:deny".into());
        });
        assert!(r.select("gate-r001", "deny").await.is_err());
        assert!(r.select("gate-r000", "deny").await.is_ok());
        assert_eq!(
            r.selected().get("gate-r001"),
            None,
            "失败的切门不许留下痕迹"
        );
    }
}
