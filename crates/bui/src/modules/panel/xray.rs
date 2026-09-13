//! Xray gRPC 客户端（spec §3.3、调研 X1–X11）。
//!
//! - 增删用户：`HandlerService.AlterInbound{tag, operation}`，`operation` 是**两层** `TypedMessage`
//!   （外层 `AddUserOperation` 包 `User`，`User.account` 再包 `vless.Account`）；对两个 inbound 各调一次。
//! - `email` 是 gRPC 侧唯一键，取 `user_id`（`bui_schema::render::xray::clients` 里也是它）。
//! - 统计：`StatsService.QueryStats(pattern="user>>>", reset=true)` 一次拉全量增量；
//!   `reset` 对每个计数器原子交换清零，不丢不重（X7）。
//! - `RemoveUser` **只阻止新握手**，已建立的 REALITY 连接会活到客户端自己断开（X10，spec §4.2 已接受该窗口）。

// 生成代码归 prost/tonic 管，clippy 的意见对它没有意义；不挂这个 allow，
// `cargo clippy --all-targets -- -D warnings` 大概率直接失败。
// 生成代码里有十来个本项目用不到的消息（`xray.core::Config`、`common.net::PortRange`…），
// 它们是被用到的那几个 proto `import` 进来的，删不掉也没法「补调用方」⇒ 这条 allow 只盖
// 生成代码，不盖手写代码（Task 13 已删掉整个 panel 子树的 allow(dead_code)）。
#[allow(clippy::all, dead_code)]
pub mod pb {
    // prost 生成的嵌套模块树；不要改成平铺的 `include_proto!`，跨包引用会解析失败。
    include!(concat!(env!("OUT_DIR"), "/xray.rs"));
}

use super::{TxRx, XrayApi};
use bui_schema::paths::Paths;
use bui_schema::render::xray::SlotRule;
use prost::Message;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;

use pb::xray::app::proxyman::command::{
    handler_service_client::HandlerServiceClient, AddUserOperation, AlterInboundRequest,
    RemoveUserOperation,
};
use pb::xray::app::router::command::{
    routing_service_client::RoutingServiceClient, AddRuleRequest, ListRuleItem, ListRuleRequest,
    RemoveRuleRequest,
};
use pb::xray::app::router::{routing_rule::TargetTag, Config as RouterConfig, RoutingRule};
use pb::xray::app::stats::command::{
    stats_service_client::StatsServiceClient, QueryStatsRequest, Stat,
};
use pb::xray::common::protocol::User as PbUser;
use pb::xray::common::serial::TypedMessage;
use pb::xray::proxy::vless::Account;

/// 两层 `TypedMessage` 的类型名（调研 X2；必须是 proto 消息全名，服务端按它做反射解码）
pub const TYPE_ADD_USER: &str = "xray.app.proxyman.command.AddUserOperation";
pub const TYPE_REMOVE_USER: &str = "xray.app.proxyman.command.RemoveUserOperation";
pub const TYPE_VLESS_ACCOUNT: &str = "xray.proxy.vless.Account";
/// `AddRule` 载荷内层消息的全名（同样是 proto 消息全名，服务端按它反射解码）
pub const TYPE_ROUTER_CONFIG: &str = "xray.app.router.Config";
/// `QueryStats` 的 pattern：纯子串匹配（X6），`user>>>` 命中所有用户计数器
pub const STATS_PATTERN: &str = "user>>>";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

fn typed(type_name: &str, msg: &impl Message) -> TypedMessage {
    TypedMessage {
        r#type: type_name.to_string(),
        value: msg.encode_to_vec(),
    }
}

/// 组装 `AlterInbound` 的 AddUser 请求（两层 TypedMessage，X1–X3）。纯函数，可单测。
pub fn add_user_request(tag: &str, user_id: Uuid, vless_uuid: Uuid) -> AlterInboundRequest {
    let account = Account {
        id: vless_uuid.to_string(),
        flow: super::VLESS_FLOW.to_string(),
        ..Default::default()
    };
    let user = PbUser {
        level: 0,
        email: user_id.to_string(),
        account: Some(typed(TYPE_VLESS_ACCOUNT, &account)),
    };
    let op = AddUserOperation { user: Some(user) };
    AlterInboundRequest {
        tag: tag.to_string(),
        operation: Some(typed(TYPE_ADD_USER, &op)),
    }
}

/// 组装 `AlterInbound` 的 RemoveUser 请求：只带 email。
pub fn remove_user_request(tag: &str, user_id: Uuid) -> AlterInboundRequest {
    let op = RemoveUserOperation {
        email: user_id.to_string(),
    };
    AlterInboundRequest {
        tag: tag.to_string(),
        operation: Some(typed(TYPE_REMOVE_USER, &op)),
    }
}

/// 组装 `AddRule` 请求：外层 `TypedMessage` 包 `xray.app.router.Config`，里面正好一条规则。
/// 纯函数，可单测。
pub fn add_rule_request(rule: &SlotRule) -> AddRuleRequest {
    let r = RoutingRule {
        rule_tag: rule.rule_tag.clone(),
        inbound_tag: vec![rule.inbound_tag.clone()],
        user_email: rule.emails.clone(),
        target_tag: Some(TargetTag::Tag(rule.outbound_tag.clone())),
        ..Default::default()
    };
    AddRuleRequest {
        config: Some(typed(
            TYPE_ROUTER_CONFIG,
            &RouterConfig {
                rule: vec![r],
                ..Default::default()
            },
        )),
        // 永远 true：`false` 的语义是「清空 rules + balancers 再全量装载」（D7）
        should_append: true,
    }
}

pub fn remove_rule_request(rule_tag: &str) -> RemoveRuleRequest {
    RemoveRuleRequest {
        rule_tag: rule_tag.to_string(),
    }
}

/// `ListRuleResponse.rules` → `(ruleTag, outboundTag)`；没有 `ruleTag` 的规则
/// （渲染里的 api / 直连两条）我们既不认也不删，直接丢掉。表序原样保留。
pub fn rule_pairs(items: &[ListRuleItem]) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|i| !i.rule_tag.is_empty())
        .map(|i| (i.rule_tag.clone(), i.tag.clone()))
        .collect()
}

/// `user>>><email>>>>traffic>>>uplink|downlink` → (email, 方向)；不认的名字返回 None
pub fn parse_user_counter(name: &str) -> Option<(String, Direction)> {
    let rest = name.strip_prefix("user>>>")?;
    let (email, dir) = rest.rsplit_once(">>>traffic>>>")?;
    let d = match dir {
        "uplink" => Direction::Up,
        "downlink" => Direction::Down,
        _ => return None,
    };
    if email.is_empty() {
        return None;
    }
    Some((email.to_string(), d))
}

/// `QueryStatsResponse.stat` → email → 增量（uplink 计 tx、downlink 计 rx，与 v3 一致）
pub fn deltas_from_stats(stats: &[Stat]) -> BTreeMap<String, TxRx> {
    let mut out: BTreeMap<String, TxRx> = BTreeMap::new();
    for s in stats {
        let Some((email, dir)) = parse_user_counter(&s.name) else {
            continue;
        };
        // 计数器是单调累加的 u64，`reset=true` 之后返回的是增量；负值理论上不可能，
        // 出现就丢弃（绝不 `as u64` 回绕成天文数字，那会瞬间把用户判成超限）
        let Ok(v) = u64::try_from(s.value) else {
            continue;
        };
        if v == 0 {
            continue;
        }
        let e = out.entry(email).or_default();
        match dir {
            Direction::Up => e.add(TxRx { tx: v, rx: 0 }),
            Direction::Down => e.add(TxRx { tx: 0, rx: v }),
        }
    }
    out
}

/// CLI 退路（X9）：`xray api rmu --server=<addr> -tag=<tag> <email>`
pub fn rmu_args(addr: &str, tag: &str, email: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "rmu".to_string(),
        format!("--server={addr}"),
        format!("-tag={tag}"),
        email.to_string(),
    ]
}

/// 找 xray 可执行文件：先 `<bin>/xray`，再 PATH 上的 `xray`；都没有返回 None
pub fn xray_program(host: &dyn crate::sys::Host, paths: &Paths) -> Option<PathBuf> {
    let owned = paths.bin_dir.join("xray");
    if host.read_file(&owned).ok().flatten().is_some() {
        return Some(owned);
    }
    host.which("xray").then(|| PathBuf::from("xray"))
}

pub struct XrayClient {
    addr: String,
    channel: OnceLock<Channel>,
}

impl XrayClient {
    pub fn new() -> Self {
        Self::with_addr(super::XRAY_API_ADDR)
    }

    pub fn with_addr(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            channel: OnceLock::new(),
        }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn endpoint_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `connect_lazy` 的通道：进程启动时 xray 可能还没起来，所以不在构造时连；
    /// 通道自己会重连，掉线不需要我们重建。
    fn channel(&self) -> anyhow::Result<Channel> {
        if let Some(c) = self.channel.get() {
            return Ok(c.clone());
        }
        let ep = Endpoint::from_shared(self.endpoint_url())?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CALL_TIMEOUT);
        let c = ep.connect_lazy();
        let _ = self.channel.set(c.clone());
        Ok(c)
    }
}

impl Default for XrayClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl XrayApi for XrayClient {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()> {
        let mut c = HandlerServiceClient::new(self.channel()?);
        c.alter_inbound(add_user_request(tag, user_id, vless_uuid))
            .await?;
        Ok(())
    }

    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()> {
        let mut c = HandlerServiceClient::new(self.channel()?);
        c.alter_inbound(remove_user_request(tag, user_id)).await?;
        Ok(())
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut c = StatsServiceClient::new(self.channel()?);
        let resp = c
            .query_stats(QueryStatsRequest {
                pattern: STATS_PATTERN.to_string(),
                reset: true,
            })
            .await?;
        Ok(deltas_from_stats(&resp.into_inner().stat))
    }

    async fn add_rule(&self, rule: &SlotRule) -> anyhow::Result<()> {
        let mut c = RoutingServiceClient::new(self.channel()?);
        c.add_rule(add_rule_request(rule)).await?;
        Ok(())
    }

    async fn remove_rule(&self, rule_tag: &str) -> anyhow::Result<()> {
        let mut c = RoutingServiceClient::new(self.channel()?);
        c.remove_rule(remove_rule_request(rule_tag)).await?;
        Ok(())
    }

    async fn list_rules(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut c = RoutingServiceClient::new(self.channel()?);
        let resp = c.list_rule(ListRuleRequest {}).await?;
        Ok(rule_pairs(&resp.into_inner().rules))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use bui_schema::render::xray::SlotRule;
    use pretty_assertions::assert_eq;

    fn uid() -> Uuid {
        Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa").unwrap()
    }

    fn user_rule() -> SlotRule {
        SlotRule {
            rule_tag: "resi-u-8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into(),
            inbound_tag: "vless-residential".into(),
            emails: vec!["8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into()],
            outbound_tag: "relay-slot-1".into(),
        }
    }

    fn fallback_rule() -> SlotRule {
        SlotRule {
            rule_tag: "resi-fallback".into(),
            inbound_tag: "vless-residential".into(),
            emails: vec![],
            outbound_tag: "relay-slot-0".into(),
        }
    }

    #[test]
    fn add_rule_request_wraps_a_router_config_with_exactly_one_rule() {
        let req = add_rule_request(&user_rule());
        assert!(
            req.should_append,
            "只许追加：shouldAppend=false 会清空整张路由表（D7）"
        );
        let tm = req.config.expect("config 必须存在");
        assert_eq!(tm.r#type, TYPE_ROUTER_CONFIG);
        let cfg = pb::xray::app::router::Config::decode(tm.value.as_slice()).unwrap();
        assert!(cfg.balancing_rule.is_empty(), "不带 balancer");
        assert_eq!(cfg.rule.len(), 1, "一条请求只带一条规则");
        let r = &cfg.rule[0];
        assert_eq!(r.rule_tag, user_rule().rule_tag);
        assert_eq!(r.inbound_tag, vec!["vless-residential".to_string()]);
        assert_eq!(r.user_email, vec![user_rule().emails[0].clone()]);
        assert_eq!(
            r.target_tag,
            Some(pb::xray::app::router::routing_rule::TargetTag::Tag(
                "relay-slot-1".into()
            )),
            "出站走 oneof 的 tag 分支，不是 balancing_tag"
        );
    }

    #[test]
    fn a_fallback_rule_carries_no_user_email() {
        let req = add_rule_request(&fallback_rule());
        let cfg =
            pb::xray::app::router::Config::decode(req.config.unwrap().value.as_slice()).unwrap();
        assert!(
            cfg.rule[0].user_email.is_empty(),
            "兜底规则不按 user 过滤：它要兜住「一条规则都没有的 email」"
        );
        assert_eq!(cfg.rule[0].rule_tag, "resi-fallback");
    }

    #[test]
    fn remove_rule_request_carries_only_the_rule_tag() {
        assert_eq!(remove_rule_request("resi-u-x").rule_tag, "resi-u-x");
    }

    #[test]
    fn rule_pairs_drops_the_untagged_rules() {
        use pb::xray::app::router::command::ListRuleItem;
        let items = vec![
            ListRuleItem {
                tag: "api".into(),
                rule_tag: String::new(),
            },
            ListRuleItem {
                tag: "relay-slot-1".into(),
                rule_tag: "resi-u-a".into(),
            },
            ListRuleItem {
                tag: "relay-slot-0".into(),
                rule_tag: "resi-fallback".into(),
            },
        ];
        assert_eq!(
            rule_pairs(&items),
            vec![
                ("resi-u-a".to_string(), "relay-slot-1".to_string()),
                ("resi-fallback".to_string(), "relay-slot-0".to_string())
            ],
            "没有 ruleTag 的规则（渲染里的 api / 直连两条）既不认也不删，顺序保持表序"
        );
    }

    /// 真实 xray 的**用户增删**事实。与 [`routing_rules_round_trip_against_a_real_xray`]
    /// 同一档的有意例外（回环端口 + tempfile + 子进程在 `Drop` 里 kill，不碰 systemd / /opt），
    /// 只在 `xray` 在 PATH 上时跑。
    ///
    /// 四条事实，钉住 `panel::users` 里 add / remove 两个方向的判据
    /// （本机 xray 26.9.9 实测，2026-09-14）：
    /// 1. 同名 email 再 `AddUser` ⇒ `proxy/vless: User <email> already exists.`
    ///    —— `users::email_taken` 认它，于是「摘掉再加」那一路才会被触发；
    /// 2. 先 `RemoveUser` 再 `AddUser` ⇒ 新 uuid 挂得上（轮换靠这条成立）；
    /// 3. `RemoveUser` 撞不存在 ⇒ `proxy/vless: User <email> not found.`
    ///    —— `users::target_already_reached` 认它，删是幂等的；
    /// 4. `AddUser` 打到**不存在的 inbound tag** ⇒ `handler not found: <tag>`，
    ///    这条**也带 `not found`**。所以 add 那一路绝不能用
    ///    `users::target_already_reached` 判「已达成」：xray 还没起那个 inbound 时
    ///    会被记成同步成功，用户永远进不了内核而日志里一条错都没有。
    #[tokio::test]
    async fn user_alter_facts_against_a_real_xray() {
        use crate::modules::panel::users::{email_taken, target_already_reached};
        if !have_xray() {
            eprintln!("skipped: xray not found");
            return;
        }
        let free_port = || {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        };
        let (api_port, vless_port) = (free_port(), free_port());
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("xray.json");
        let seed = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        let cfg = serde_json::json!({
            "log": {"loglevel": "warning"},
            "api": {"tag": "api", "services": ["HandlerService", "RoutingService"]},
            "inbounds": [
                {"tag": "api", "port": api_port, "listen": "127.0.0.1",
                 "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}},
                // 渲染出的两个 REALITY 入站在这里用明文 vless 代替：本用例只问
                // HandlerService 对 clients 的增删语义，与传输层无关
                {"tag": "vless-direct", "port": vless_port, "listen": "127.0.0.1",
                 "protocol": "vless",
                 "settings": {"clients": [{"id": seed, "email": "seed"}], "decryption": "none"}}
            ],
            "outbounds": [{"tag": "direct", "protocol": "freedom"}],
            "routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
        });
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
        let _xrayd = Xrayd::spawn(&cfg_path);
        let c = XrayClient::with_addr(format!("127.0.0.1:{api_port}"));
        let mut ready = false;
        for _ in 0..50 {
            if c.list_rules().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "xray 没在 5 秒内接受 gRPC");

        let new = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        c.add_user("vless-direct", uid(), vid()).await.unwrap();

        // 事实①：同名 email 再 AddUser 报「已存在」——**换的 uuid 没生效**
        let dup = c
            .add_user("vless-direct", uid(), new)
            .await
            .expect_err("同名 email 必须报错")
            .to_string();
        assert!(email_taken(&dup), "{dup}");

        // 事实②：摘掉再加，新 uuid 就挂上了（轮换与「b-ui 重启丢了记账」都靠这条）
        c.remove_user("vless-direct", uid()).await.unwrap();
        c.add_user("vless-direct", uid(), new).await.unwrap();

        // 事实③：RemoveUser 撞不存在 = 目标已达成
        c.remove_user("vless-direct", uid()).await.unwrap();
        let gone = c
            .remove_user("vless-direct", uid())
            .await
            .expect_err("不存在的 email 必须报错")
            .to_string();
        assert!(target_already_reached(&gone), "{gone}");

        // 事实④：inbound 不存在时的 AddUser 也带「not found」，但目标根本没达成
        let no_tag = c
            .add_user("vless-residential", uid(), new)
            .await
            .expect_err("不存在的 inbound tag 必须报错")
            .to_string();
        assert!(
            target_already_reached(&no_tag),
            "宽匹配会把它当成已达成，所以 add 那一路不许用它：{no_tag}"
        );
        assert!(
            !email_taken(&no_tag),
            "add 那一路的判据必须把它排除在外：{no_tag}"
        );
    }

    /// 真实 xray 的 gRPC 往返。这是「两层 `TypedMessage` 的 type 名写对了没」的唯一硬证据 ——
    /// 编码错了服务端反射解码会直接报错，本地 fake 永远发现不了；D7 里那三条内核事实
    /// （重名报错 / 删不存在算成功 / 追加即表尾）也由它守住。
    ///
    /// **对 Global Constraints「测试不碰真实系统」的一次有意例外**（P3 计划批准）：
    /// 只在 `xray` 存在时跑，配置写在 `tempfile` 目录里，api inbound 绑 `127.0.0.1` 的
    /// 临时端口，子进程在 `Drop` 里 kill —— 与 `clash.rs` / `proxy.rs` / `hy2.rs` 里
    /// 既有的「起一个回环监听再打自己」是同一档次的动作，不碰 systemd、不碰 /opt。
    #[tokio::test]
    async fn routing_rules_round_trip_against_a_real_xray() {
        use bui_schema::render::xray::FALLBACK_RULE_TAG;
        if !have_xray() {
            eprintln!("skipped: xray not found");
            return;
        }
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("xray.json");
        let cfg = serde_json::json!({
            "log": {"loglevel": "warning"},
            // 少了 RoutingService 就是 gRPC Unimplemented（D11）
            "api": {"tag": "api", "services": ["HandlerService", "RoutingService"]},
            "inbounds": [{"tag": "api", "port": port, "listen": "127.0.0.1",
                          "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}}],
            "outbounds": [{"tag": "direct", "protocol": "freedom"},
                          {"tag": "relay-slot-0", "protocol": "freedom"},
                          {"tag": "relay-slot-1", "protocol": "freedom"}],
            "routing": {"rules": [
                {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
                {"type": "field", "ruleTag": "resi-fallback",
                 "inboundTag": ["vless-residential"], "outboundTag": "relay-slot-0"}
            ]}
        });
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
        let _xrayd = Xrayd::spawn(&cfg_path);
        let c = XrayClient::with_addr(format!("127.0.0.1:{port}"));
        // 起得来才继续（最多等 5 秒）
        let mut ready = false;
        for _ in 0..50 {
            if c.list_rules().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "xray 没在 5 秒内接受 gRPC");

        let fb = ("resi-fallback".to_string(), "relay-slot-0".to_string());
        assert_eq!(
            c.list_rules().await.unwrap(),
            vec![fb.clone()],
            "启动时的表来自配置文件；api 那条没有 ruleTag，不出现在这里"
        );

        let u = SlotRule {
            rule_tag: "resi-u-a".into(),
            inbound_tag: "vless-residential".into(),
            emails: vec!["a".into()],
            outbound_tag: "relay-slot-1".into(),
        };
        c.add_rule(&u).await.unwrap();
        // 事实：AddRule 只能追加到**表尾**（所以每轮加完要把兜底删了再追加，D7）
        assert_eq!(
            c.list_rules().await.unwrap(),
            vec![
                fb.clone(),
                ("resi-u-a".to_string(), "relay-slot-1".to_string())
            ]
        );
        // 事实：ruleTag 重名 ⇒ 整条请求报错（所以 converge 必须先删再加）
        let dup = c.add_rule(&u).await;
        assert!(dup.is_err(), "重名 ruleTag 必须报错，实际：{dup:?}");
        // 事实：删不存在的 tag 算成功（所以删是幂等的）
        c.remove_rule("resi-u-nobody").await.unwrap();
        // 把兜底挪回表尾
        c.remove_rule(FALLBACK_RULE_TAG).await.unwrap();
        c.add_rule(&SlotRule {
            rule_tag: FALLBACK_RULE_TAG.into(),
            inbound_tag: "vless-residential".into(),
            emails: vec![],
            outbound_tag: "relay-slot-0".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            c.list_rules().await.unwrap(),
            vec![("resi-u-a".to_string(), "relay-slot-1".to_string()), fb],
            "删+追加之后兜底回到表尾"
        );
        // 删掉用户那条，表回到初始形状
        c.remove_rule("resi-u-a").await.unwrap();
        assert_eq!(c.list_rules().await.unwrap().len(), 1);
    }

    fn have_xray() -> bool {
        std::process::Command::new("xray")
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 跑在回环上的 xray 子进程；`Drop` 里 kill + wait，测试 panic 也不留孤儿进程。
    struct Xrayd(std::process::Child);

    impl Xrayd {
        fn spawn(cfg: &std::path::Path) -> Self {
            Self(
                std::process::Command::new("xray")
                    .args(["run", "-c"])
                    .arg(cfg)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("起 xray 子进程"),
            )
        }
    }

    impl Drop for Xrayd {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn vid() -> Uuid {
        Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
    }

    #[test]
    fn add_user_request_nests_two_typed_messages() {
        let req = add_user_request("vless-direct", uid(), vid());
        assert_eq!(req.tag, "vless-direct");
        let op = req.operation.expect("operation 必须存在");
        assert_eq!(op.r#type, TYPE_ADD_USER);
        let add = pb::xray::app::proxyman::command::AddUserOperation::decode(op.value.as_slice())
            .unwrap();
        let user = add.user.expect("user 必须存在");
        assert_eq!(user.level, 0);
        assert_eq!(
            user.email,
            uid().to_string(),
            "email 就是 user_id（spec §3.3 的唯一键）"
        );
        let acct_tm = user.account.expect("account 必须存在");
        assert_eq!(acct_tm.r#type, TYPE_VLESS_ACCOUNT);
        let acct = pb::xray::proxy::vless::Account::decode(acct_tm.value.as_slice()).unwrap();
        assert_eq!(acct.id, vid().to_string());
        assert_eq!(acct.flow, "xtls-rprx-vision");
        assert_eq!(acct.encryption, "", "v4 不填 encryption（X3）");
        // 以下两条绑在 pinned 版本 v26.3.27 的 account.proto 字段 4–9 上（调研 X3）：
        // 换 pinned 版本时这几条断言要连同 `proto/SHA256SUMS` 一起重看，字段改名/挪位就会编译不过。
        assert_eq!(acct.xor_mode, 0);
        assert!(acct.reverse.is_none());
    }

    #[test]
    fn remove_user_request_carries_only_the_email() {
        let req = remove_user_request("vless-residential", uid());
        assert_eq!(req.tag, "vless-residential");
        let op = req.operation.unwrap();
        assert_eq!(op.r#type, TYPE_REMOVE_USER);
        let rm = pb::xray::app::proxyman::command::RemoveUserOperation::decode(op.value.as_slice())
            .unwrap();
        assert_eq!(rm.email, uid().to_string());
    }

    #[test]
    fn counter_names_parse_into_email_and_direction() {
        assert_eq!(
            parse_user_counter("user>>>alice-id>>>traffic>>>uplink"),
            Some(("alice-id".to_string(), Direction::Up))
        );
        assert_eq!(
            parse_user_counter("user>>>alice-id>>>traffic>>>downlink"),
            Some(("alice-id".to_string(), Direction::Down))
        );
        // email 里带 `>>>` 之外的任何字符都要原样保留
        assert_eq!(
            parse_user_counter("user>>>8d5a1a1e-3b2c-4d1e-9f00-0000000000aa>>>traffic>>>uplink"),
            Some((
                "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".to_string(),
                Direction::Up
            ))
        );
        assert_eq!(parse_user_counter("inbound>>>api>>>traffic>>>uplink"), None);
        assert_eq!(
            parse_user_counter("user>>>alice>>>traffic>>>sidelink"),
            None
        );
        assert_eq!(
            parse_user_counter("user>>>>>>traffic>>>uplink"),
            None,
            "空 email 不认"
        );
        assert_eq!(parse_user_counter(""), None);
    }

    #[test]
    fn stats_become_per_user_deltas_with_uplink_as_tx() {
        let stats = vec![
            Stat {
                name: "user>>>a>>>traffic>>>uplink".into(),
                value: 100,
            },
            Stat {
                name: "user>>>a>>>traffic>>>downlink".into(),
                value: 250,
            },
            Stat {
                name: "user>>>b>>>traffic>>>uplink".into(),
                value: 7,
            },
            Stat {
                name: "inbound>>>api>>>traffic>>>uplink".into(),
                value: 999,
            },
            Stat {
                name: "user>>>c>>>traffic>>>uplink".into(),
                value: -5,
            },
        ];
        let d = deltas_from_stats(&stats);
        assert_eq!(d["a"], TxRx { tx: 100, rx: 250 });
        assert_eq!(d["b"], TxRx { tx: 7, rx: 0 });
        assert!(!d.contains_key("api"), "非 user 计数器不进表");
        assert_eq!(
            d.get("c"),
            None,
            "负值（不该出现）当 0 丢弃，绝不回绕成天文数字"
        );
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn rmu_args_match_the_cli_contract() {
        assert_eq!(
            rmu_args("127.0.0.1:10085", "vless-direct", "alice-id"),
            vec![
                "api".to_string(),
                "rmu".to_string(),
                "--server=127.0.0.1:10085".to_string(),
                "-tag=vless-direct".to_string(),
                "alice-id".to_string(),
            ]
        );
    }

    #[test]
    fn xray_program_prefers_bin_dir_then_path() {
        let paths = bui_schema::paths::Paths::default_server();
        let h = FakeHost::new();
        assert_eq!(xray_program(&h, &paths), None, "两处都没有就返回 None");
        h.with(|i| {
            i.which.insert("xray".into());
        });
        assert_eq!(
            xray_program(&h, &paths),
            Some(std::path::PathBuf::from("xray"))
        );
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
        });
        assert_eq!(
            xray_program(&h, &paths),
            Some(std::path::PathBuf::from("/opt/b-ui/bin/xray"))
        );
    }

    #[test]
    fn endpoint_url_is_plain_http_on_the_api_inbound() {
        assert_eq!(XrayClient::new().addr(), super::super::XRAY_API_ADDR);
        assert_eq!(XrayClient::new().endpoint_url(), "http://127.0.0.1:10085");
        assert_eq!(
            XrayClient::with_addr("127.0.0.1:1").endpoint_url(),
            "http://127.0.0.1:1"
        );
    }

    #[tokio::test]
    async fn calls_fail_fast_when_nothing_listens_on_the_api_port() {
        // 端口 1 上不会有 xray：lazy channel 在第一次调用时才连，连不上就是 Err，
        // 不会 panic、不会挂住（CONNECT_TIMEOUT 3 秒）
        let c = XrayClient::with_addr("127.0.0.1:1");
        assert!(c.remove_user("vless-direct", uid()).await.is_err());
    }

    #[test]
    fn vendored_protos_match_the_recorded_checksums() {
        // 换 pinned 版本时必须同步更新 SHA256SUMS，否则这条测试会告诉你 proto 变了
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proto");
        let sums =
            std::fs::read_to_string(root.join("SHA256SUMS")).expect("proto/SHA256SUMS 必须存在");
        let mut n = 0;
        for line in sums.lines().filter(|l| !l.trim().is_empty()) {
            let (want, rel) = line
                .split_once("  ")
                .expect("格式是 `<sha256>  <相对路径>`");
            let bytes = std::fs::read(root.join(rel)).unwrap_or_else(|_| panic!("缺少 {rel}"));
            assert_eq!(
                crate::kernels::sha256_hex(&bytes),
                want,
                "{rel} 的内容与记录不符"
            );
            n += 1;
        }
        assert_eq!(
            n, 13,
            "10 个用户/统计面的 proto + RoutingService 闭包的 3 个（T0）"
        );
    }
}
