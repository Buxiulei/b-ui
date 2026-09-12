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
#[allow(clippy::all)]
pub mod pb {
    // prost 生成的嵌套模块树；不要改成平铺的 `include_proto!`，跨包引用会解析失败。
    include!(concat!(env!("OUT_DIR"), "/xray.rs"));
}

use super::{TxRx, XrayApi};
use bui_schema::paths::Paths;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;

    fn uid() -> Uuid {
        Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa").unwrap()
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
        assert_eq!(n, 10, "调研 X8：闭包正好 10 个 .proto");
    }
}
