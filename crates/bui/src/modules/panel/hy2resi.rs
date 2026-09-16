//! 住宅 HY2（自建 sing-box）的两个控制面客户端（spec §2.3、§5.1、§5.2）。
//!
//! 住宅入站从 apernet hysteria 换成 sing-box 之后，「计量」与「在线 / 踢人」分别落在
//! 两个只监听回环、都不设 secret 的面上（[`super::HY2_RESI_V2RAY_API`] /
//! [`super::HY2_RESI_CLASH_API`]）：
//!
//! - **计量**走 v2ray_api 的 `StatsService.QueryStats`，计数器名是
//!   `user>>>{凭据 name}>>>traffic>>>uplink|downlink` —— 与 Xray 那条路**同名同义**，
//!   所以解析复用 [`super::xray::parse_user_counter`]，不重写。
//!   两个上游事实（v1.14.1 源码，见 [`QUERY_STATS_PATH`] 与 [`Hy2ResiApi::query_user_deltas`]）：
//!   ① 服务名在 `init()` 里被覆盖过；② `QueryStats` 只读 `patterns`，deprecated 的 `pattern`
//!   一个字都不看。
//! - **在线数与踢人**走 Clash API：`GET /connections` 里**没有** user 字段，唯一能把连接
//!   归到用户的线索是 `rule` 字符串里的 `auth_user=<name>`（[`user_of_rule`]）；踢用户 =
//!   门（selector）切 `deny` + 逐条 `DELETE /connections/{id}` 兜底（spec §5.2）。
//!
//! 计数器不跨 sing-box 重启存活（与 apernet trafficStats 的内存计数器同一种丢失窗口），
//! 这里**不做**任何降级或补偿。

// 生成代码归 prost/tonic 管，clippy 的意见对它没有意义；理由同 `panel::xray::pb`。
#[allow(clippy::all, dead_code)]
pub mod pb {
    // prost 生成的嵌套模块树；`v2ray.rs` 由 build.rs 的第二次 `configure()` 产出。
    include!(concat!(env!("OUT_DIR"), "/v2ray.rs"));
}

use super::xray::{parse_user_counter, Direction, CALL_TIMEOUT, CONNECT_TIMEOUT, STATS_PATTERN};
use super::{Hy2ResiApi, TxRx};
use crate::modules::residential::clash::ClashError;
use bui_schema::render::hy2_singbox::gate_tag;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use tonic::transport::{Channel, Endpoint};

use pb::v2ray::core::app::stats::command::{
    stats_service_client::StatsServiceClient, QueryStatsRequest, Stat,
};

/// Clash API 的超时：与直连那条 trafficStats HTTP 路同值（[`super::hy2::REQ_TIMEOUT`]）。
/// 采样与生命周期动作都**同步等**它，所以宁短不长 —— 住宅面挂了要的是快速报错，
/// 不是把整轮采样拖住（gRPC 那条路另有 `CONNECT_TIMEOUT` / `CALL_TIMEOUT`）。
pub const REQ_TIMEOUT: std::time::Duration = super::hy2::REQ_TIMEOUT;

/// `StatsService.QueryStats` 在线上的**真实**方法路径。
///
/// sing-box 生成代码里的路径是 `/experimental.v2rayapi.StatsService/QueryStats`
/// （proto 的 package 就是 `experimental.v2rayapi`），但
/// `experimental/v2rayapi/stats.go:22` 的 `init()` 把注册名覆盖成
/// `"v2ray.core.app.stats.command.StatsService"`，而 grpc-go 是用
/// `ServiceDesc.ServiceName` 拼方法路径的 ⇒ 线上认的是下面这一个。
/// 所以 `proto-v2ray/app/stats/command/command.proto` 的 package 写成
/// `v2ray.core.app.stats.command`，让 tonic 生成的客户端天然打对路径；
/// 本常量与那份 proto 的 package 行由守门测试钉在一起。
pub const QUERY_STATS_PATH: &str = "/v2ray.core.app.stats.command.StatsService/QueryStats";

/// `GET /connections` 里的一条连接。`metadata` 里**没有** user 字段，所以只留
/// 归组要用的三项（`rule` 归人、`id` 踢人、`chains` 供自检看门位是否真生效）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hy2ResiConn {
    pub id: String,
    pub rule: String,
    pub chains: Vec<String>,
}

/// `rule` 字符串 → 这条连接属于哪个凭据 name。
///
/// 形态是 sing-box 自己拼的：`F.ToString(c.Rule, " => ", c.Rule.Action())`
/// （`experimental/clashapi/connections.go`），`auth_user` 那一项的 `String()` 是
/// `"auth_user=" + users[0]`，`route` 动作的是 `"route(" + 出站 + ")"`
/// ⇒ `auth_user=r000 => route(gate-r000)`。没有规则命中时 sing-box 写的是 `"final"`。
///
/// 多用户形态（`auth_user=[a b]`，规则里写了多个 user）归不到单个用户，返回 `None`
/// —— 我们每条规则只写一个用户（spec §3.2），出现它就是渲染出了错，宁可不计。
pub fn user_of_rule(rule: &str) -> Option<&str> {
    let cond = rule.split(" => ").next()?;
    let name = cond
        .split_whitespace()
        .find_map(|t| t.strip_prefix("auth_user="))?;
    (!name.is_empty() && !name.starts_with('[')).then_some(name)
}

/// 凭据 name → 当前连接条数（一条连接算一次，spec §5.2）
pub fn online_of(conns: &[Hy2ResiConn]) -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    for c in conns {
        if let Some(name) = user_of_rule(&c.rule) {
            *out.entry(name.to_string()).or_default() += 1;
        }
    }
    out
}

/// `GET /connections` 的响应体 → 连接表；坏条目跳过，整轮不失败（口径同 `hy2::parse_traffic`）。
pub fn parse_connections(body: &str) -> Vec<Hy2ResiConn> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(list) = v.get("connections").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|c| {
            Some(Hy2ResiConn {
                id: c.get("id")?.as_str()?.to_string(),
                rule: c.get("rule")?.as_str()?.to_string(),
                chains: c
                    .get("chains")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// `GET /proxies` 的响应体 → **门位** tag → 当前成员（`now`）。
///
/// 只有出站组才有 `now`（`experimental/clashapi/proxies.go` 的 `proxyInfo`），
/// 非组出站（`deny`、各槽 socks）没有这个键 ⇒ 不进表。
///
/// 除此之外还要按 `gate-` 前缀筛一道：sing-box 为了让 clash dashboard 能用，在
/// `getProxies` 里**无条件**多塞一个伪组 `GLOBAL`（`now = route.final`，对我们就是
/// `"deny"`），它不是 Selector，`PUT` 过去会被回 400 "Must be a Selector"。生产上必然有
/// 这一项而 `FakeHy2Resi` 的表里永远没有，不筛掉就是一处 fake / 生产不对等：任何
/// 「遍历读回来的键去收敛 / 投影 / 自检」的写法都会在测试里全绿、在生产上对 `GLOBAL`
/// 动手。前缀取自渲染器的 [`gate_tag`]，门位的唯一形态由它决定。
pub fn parse_proxies_now(body: &str) -> BTreeMap<String, String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeMap::new();
    };
    let Some(obj) = v.get("proxies").and_then(|p| p.as_object()) else {
        return BTreeMap::new();
    };
    let prefix = gate_tag("");
    obj.iter()
        .filter(|(tag, _)| tag.starts_with(&prefix))
        .filter_map(|(tag, info)| {
            let now = info.get("now")?.as_str()?;
            Some((tag.clone(), now.to_string()))
        })
        .collect()
}

/// `QueryStatsResponse.stat` → 凭据 name → 增量。
///
/// 与 [`super::xray::deltas_from_stats`] **同一套语义**（uplink 计 tx、downlink 计 rx、
/// 负值与 0 丢弃），只是 prost 生成的 `Stat` 是另一个包的另一个类型，泛型化不值当
/// ⇒ 两份实现的一致性由 `deltas_from_stats_agrees_with_the_xray_twin` 钉住。
pub fn deltas_from_stats(stats: &[Stat]) -> BTreeMap<String, TxRx> {
    let mut out: BTreeMap<String, TxRx> = BTreeMap::new();
    for s in stats {
        let Some((name, dir)) = parse_user_counter(&s.name) else {
            continue;
        };
        // `reset=true` 之后返回的是增量；负值理论上不可能，出现就丢弃
        // （绝不 `as u64` 回绕成天文数字，那会瞬间把用户判成超限）
        let Ok(v) = u64::try_from(s.value) else {
            continue;
        };
        if v == 0 {
            continue;
        }
        let e = out.entry(name).or_default();
        match dir {
            Direction::Up => e.add(TxRx { tx: v, rx: 0 }),
            Direction::Down => e.add(TxRx { tx: 0, rx: v }),
        }
    }
    out
}

/// 发给 sing-box 的 `QueryStats` 请求（每轮采样都是这一个常量请求）。
///
/// **`patterns`（repeated，字段 3）填、单数 `pattern`（字段 1）必须留空** —— 上游
/// `experimental/v2rayapi/stats.go:199-208`（v1.14.1）：
///
/// ```text
/// if len(request.Patterns) == 0 { for name, counter := range s.counters { … counter.Swap(0) … } }
/// ```
///
/// 即 `patterns` 为空时它**无过滤地返回并清零全部计数器**；整个 `QueryStats` 里
/// `request.Pattern` 一次都没被读（`:210/:211/:233` 只读 `Patterns`）。所以照 Xray 那条路
/// 写 `pattern = "user>>>"` 会变成「每轮把 `inbound>>>` / `outbound>>>` 的计数器一起
/// `Swap(0)`、并把非用户计数器混进返回体」——**两边不能照抄**（Xray 侧仍是
/// `pattern`，见 `panel::xray` 的 `query_user_deltas`）。非 regexp 分支是
/// `strings.Contains(name, matcher)`（`:231-244`），子串语义与 [`STATS_PATTERN`] 一致。
///
/// 提成函数只为让这条裁决性偏离被测试钉住（`the_query_stats_request_fills_patterns_not_pattern`）。
pub fn query_stats_request() -> QueryStatsRequest {
    QueryStatsRequest {
        pattern: String::new(),
        reset: true,
        patterns: vec![STATS_PATTERN.to_string()],
        regexp: false,
    }
}

pub struct Hy2ResiClient {
    clash_api: String,
    v2ray_api: String,
    http: reqwest::Client,
    channel: OnceLock<Channel>,
}

impl Hy2ResiClient {
    pub fn new() -> Self {
        Self::with_apis(super::HY2_RESI_CLASH_API, super::HY2_RESI_V2RAY_API)
    }

    pub fn with_apis(clash_api: impl Into<String>, v2ray_api: impl Into<String>) -> Self {
        Self {
            clash_api: clash_api.into(),
            v2ray_api: v2ray_api.into(),
            http: reqwest::Client::builder()
                // 回环地址，绝不能走系统代理（住宅上游本身就是代理，绕回去会死锁）
                .no_proxy()
                .timeout(REQ_TIMEOUT)
                .build()
                .expect("reqwest 客户端构造不会失败"),
            channel: OnceLock::new(),
        }
    }

    pub fn clash_api(&self) -> &str {
        &self.clash_api
    }

    pub fn v2ray_api(&self) -> &str {
        &self.v2ray_api
    }

    /// Clash API 的 URL。`selector` / 连接 id 只可能是渲染器产的
    /// `gate-r000` 一类 tag 或 sing-box 自己发的 UUID，不含要转义的字符。
    pub fn clash_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.clash_api)
    }

    pub fn v2ray_endpoint_url(&self) -> String {
        format!("http://{}", self.v2ray_api)
    }

    /// `connect_lazy` 的通道：进程启动时住宅 sing-box 可能还没起来，所以不在构造时连；
    /// 通道自己会重连，掉线不需要我们重建（同 `panel::xray::XrayClient::channel`）。
    fn channel(&self) -> anyhow::Result<Channel> {
        if let Some(c) = self.channel.get() {
            return Ok(c.clone());
        }
        let ep = Endpoint::from_shared(self.v2ray_endpoint_url())?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CALL_TIMEOUT);
        let c = ep.connect_lazy();
        let _ = self.channel.set(c.clone());
        Ok(c)
    }

    /// 非 2xx 的**读**接口失败：措辞不许冒用 `Rejected` 的「拒绝切换到 {tag}」——
    /// 当时并没有在切任何门，那句话是 [`Hy2ResiApi::select`] 的签名文案（T13 的哨兵
    /// 按它认 `hy2_resi_gate_sync_failed`）。仍走 `Unreachable`，所以「Clash API 不可达」
    /// 这半个签名照旧命中，运维文案也是事实。
    fn http_failed(method: &str, path: &str, status: u16) -> ClashError {
        ClashError::Unreachable(format!("{method} {path} 回 HTTP {status}"))
    }

    async fn clash_get(&self, path: &str) -> Result<String, ClashError> {
        let resp = self
            .http
            .get(self.clash_url(path))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Self::http_failed("GET", path, status));
        }
        resp.text()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))
    }
}

impl Default for Hy2ResiClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hy2ResiApi for Hy2ResiClient {
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut c = StatsServiceClient::new(self.channel()?);
        // 请求体是 `query_stats_request()`（`patterns` 填、`pattern` 留空，理由与上游
        // 源码行号写在那个函数的文档里）
        let resp = c.query_stats(query_stats_request()).await?;
        Ok(deltas_from_stats(&resp.into_inner().stat))
    }

    async fn connections(&self) -> anyhow::Result<Vec<Hy2ResiConn>> {
        Ok(parse_connections(&self.clash_get("/connections").await?))
    }

    async fn close_connection(&self, id: &str) -> anyhow::Result<()> {
        let path = format!("/connections/{id}");
        let resp = self
            .http
            .delete(self.clash_url(&path))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            // 踢连接也不是切门 ⇒ 同样不用「拒绝切换」那句话
            return Err(Hy2ResiClient::http_failed("DELETE", &path, status).into());
        }
        Ok(())
    }

    async fn selected_all(&self) -> anyhow::Result<BTreeMap<String, String>> {
        Ok(parse_proxies_now(&self.clash_get("/proxies").await?))
    }

    async fn select(&self, selector: &str, tag: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(self.clash_url(&format!("/proxies/{selector}")))
            .json(&serde_json::json!({ "name": tag }))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ClashError::Rejected {
                tag: tag.to_string(),
                status,
            }
            .into());
        }
        Ok(())
    }

    async fn ready(&self) -> bool {
        self.http
            .get(self.clash_url("/version"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// `/connections` 的 rule 字段是唯一能把连接归到用户的线索（metadata 里没有 user 字段）。
    /// 形态来自 tizi PoC 实测：`auth_user=r000 => route(gate-r000)`
    #[test]
    fn connections_are_grouped_by_the_auth_user_in_the_rule_string() {
        assert_eq!(
            user_of_rule("auth_user=r000 => route(gate-r000)"),
            Some("r000")
        );
        assert_eq!(
            user_of_rule("auth_user=alice => route(gate-r000)"),
            Some("alice")
        );
        assert_eq!(user_of_rule("final => route(deny)"), None);
        assert_eq!(user_of_rule(""), None);
        // 中文用户名（生产上真实存在）
        assert_eq!(
            user_of_rule("auth_user=张三 => route(gate-r007)"),
            Some("张三")
        );
        // 没有规则命中时 sing-box 写的就是裸 "final"（connections.go 的 else 分支）
        assert_eq!(user_of_rule("final"), None);
        // 多用户形态归不到单个用户（rule_item_auth_user.go:36）
        assert_eq!(
            user_of_rule("auth_user=[alice bob] => route(gate-r000)"),
            None
        );
    }

    #[test]
    fn online_counts_are_one_per_connection_of_that_user() {
        let conns = vec![
            Hy2ResiConn {
                id: "a".into(),
                rule: "auth_user=alice => route(gate-r000)".into(),
                chains: vec!["gate-r000".into()],
            },
            Hy2ResiConn {
                id: "b".into(),
                rule: "auth_user=alice => route(gate-r000)".into(),
                chains: vec![],
            },
            Hy2ResiConn {
                id: "c".into(),
                rule: "auth_user=r001 => route(gate-r001)".into(),
                chains: vec![],
            },
        ];
        let m = online_of(&conns);
        assert_eq!(m.get("alice").copied(), Some(2));
        assert_eq!(m.get("r001").copied(), Some(1));
    }

    /// 计数器名的解析与 Xray 那条路同构（复用 panel::xray::parse_user_counter）
    #[test]
    fn counter_names_are_parsed_by_the_same_function_as_xray() {
        use crate::modules::panel::xray::{parse_user_counter, Direction};
        assert_eq!(
            parse_user_counter("user>>>alice>>>traffic>>>uplink"),
            Some(("alice".to_string(), Direction::Up))
        );
        assert_eq!(
            parse_user_counter("user>>>r001>>>traffic>>>downlink"),
            Some(("r001".to_string(), Direction::Down))
        );
    }

    /// 两个回环端点的地址只有**一处**来源：写出 `hy2-residential.json` 的那个渲染器。
    ///
    /// 断言比的是「`panel` 侧的常量 == `bui_schema` 侧的常量」而不是字面量 ——
    /// 比字面量的话，`panel` 这边另定一份值、与渲染器分叉，测试照样全绿，而生产上
    /// 计量（v2ray_api）与门位（Clash API）会双双打到没人听的端口上（静默故障）。
    #[test]
    fn the_endpoints_come_from_the_renderer() {
        use bui_schema::render::hy2_singbox as renderer;
        assert_eq!(
            super::super::HY2_RESI_CLASH_API,
            renderer::HY2_RESI_CLASH_API,
            "panel 侧的 Clash API 必须就是渲染器写进配置的那一个"
        );
        assert_eq!(
            super::super::HY2_RESI_V2RAY_API,
            renderer::HY2_RESI_V2RAY_API,
            "panel 侧的 v2ray_api 必须就是渲染器写进配置的那一个"
        );
        // 客户端不许自己拼地址：默认构造的两个端点就是上面那两个常量
        assert_eq!(
            Hy2ResiClient::new().clash_api(),
            renderer::HY2_RESI_CLASH_API
        );
        assert_eq!(
            Hy2ResiClient::new().v2ray_api(),
            renderer::HY2_RESI_V2RAY_API
        );
        assert_eq!(
            Hy2ResiClient::new().clash_url("/connections"),
            format!("http://{}/connections", renderer::HY2_RESI_CLASH_API)
        );
        assert_eq!(
            Hy2ResiClient::new().v2ray_endpoint_url(),
            format!("http://{}", renderer::HY2_RESI_V2RAY_API)
        );
    }

    /// `QueryStats` 的请求体：**`patterns` 填、单数 `pattern` 留空、`reset` 开**。
    ///
    /// 这是一条裁决性偏离（计划与 spec 原先写的是 `pattern="user>>>"`，2026-09-16 按上游
    /// 源码订正），所以必须有断言钉住：sing-box 的 `QueryStats` 只读 `patterns`
    /// （`experimental/v2rayapi/stats.go:199-208`、`:210/:211/:233`，v1.14.1），`patterns`
    /// 为空时**无过滤地返回并清零全部计数器**，而 `request.Pattern` 一次都没被读。
    /// 把这里改回 `pattern=` 编译照样过、别处的测试照样全绿，后果却是每轮把
    /// `inbound>>>` / `outbound>>>` 一起 `Swap(0)`、并把非用户计数器混进返回体。
    #[test]
    fn the_query_stats_request_fills_patterns_not_pattern() {
        let req = query_stats_request();
        assert_eq!(req.patterns, vec!["user>>>".to_string()]);
        assert!(
            req.pattern.is_empty(),
            "单数 pattern 必须留空：上游一个字都不读它"
        );
        assert!(req.reset, "reset=true 才是原子交换清零的增量语义");
        assert!(!req.regexp, "子串匹配，不是正则");
        // 子串与 Xray 那条路同一个常量（计数器名同名同义）
        assert_eq!(req.patterns, vec![STATS_PATTERN.to_string()]);
    }

    fn proto_v2ray_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proto-v2ray")
    }

    // `proto-v2ray/` 的 checksum 与条目数守门与 Xray 那棵树合在一条测试里遍历两棵树：
    // `panel::xray::tests::vendored_protos_match_the_recorded_checksums`（计划 T9 的 Files）。

    /// 编译的那份 proto 是上游原文的**逐字转写，只改 package 行**。
    ///
    /// 这条测试就是「为什么允许手改 vendor 文件」的全部理由：消息、字段号、service 与
    /// 方法名逐行必须与上游一致，唯一的差异只许是 `package` / `option go_package`。
    /// 上游 `init()` 把注册名覆盖成 `v2ray.core.app.stats.command.StatsService`
    /// （`experimental/v2rayapi/stats.go:22`），tonic 只能从 package 推路径 ⇒ 必须改这一行。
    #[test]
    fn the_compiled_proto_only_renames_the_package_of_the_upstream_one() {
        let root = proto_v2ray_dir();
        let body = |rel: &str| -> Vec<String> {
            std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|_| panic!("缺少 {rel}"))
                .lines()
                .map(str::trim_end)
                .filter(|l| {
                    !l.trim().is_empty()
                        && !l.trim_start().starts_with("//")
                        && !l.starts_with("package ")
                        && !l.starts_with("option go_package")
                })
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            body("app/stats/command/command.proto"),
            body("experimental/v2rayapi/stats.proto"),
            "除 package 行以外，编译的那份必须与上游原文逐行一致"
        );
        // package 行本身钉死：它决定 tonic 打哪条方法路径
        let compiled =
            std::fs::read_to_string(root.join("app/stats/command/command.proto")).unwrap();
        let pkg = compiled
            .lines()
            .find_map(|l| l.strip_prefix("package ").and_then(|p| p.strip_suffix(';')))
            .expect("必须有 package 行");
        assert_eq!(pkg, "v2ray.core.app.stats.command");
        assert_eq!(QUERY_STATS_PATH, format!("/{pkg}.StatsService/QueryStats"));
        // 生成代码真的落在这个模块树上（编译期就证明 package 没被改回去）
        let _ = std::any::type_name::<QueryStatsRequest>();
    }

    /// 两条计量路的取数语义必须一致：同样的计数器名与值，两个 `deltas_from_stats` 出同一张表。
    /// 一边漂了就是住宅或直连的流量被少记 / 多记，而这种错要到月底对账才看得出来。
    #[test]
    fn deltas_from_stats_agrees_with_the_xray_twin() {
        let names = [
            ("user>>>alice>>>traffic>>>uplink", 10i64),
            ("user>>>alice>>>traffic>>>downlink", 20),
            ("user>>>r001>>>traffic>>>uplink", 0),
            ("user>>>r002>>>traffic>>>downlink", -1),
            ("inbound>>>hy2-resi>>>traffic>>>uplink", 99),
            ("junk", 7),
        ];
        let mine = deltas_from_stats(
            &names
                .iter()
                .map(|(n, v)| Stat {
                    name: n.to_string(),
                    value: *v,
                })
                .collect::<Vec<_>>(),
        );
        let theirs = super::super::xray::deltas_from_stats(
            &names
                .iter()
                .map(
                    |(n, v)| super::super::xray::pb::xray::app::stats::command::Stat {
                        name: n.to_string(),
                        value: *v,
                    },
                )
                .collect::<Vec<_>>(),
        );
        assert_eq!(mine, theirs);
        assert_eq!(mine[&"alice".to_string()], TxRx { tx: 10, rx: 20 });
        assert!(!mine.contains_key("r001"), "0 不进表");
        assert!(!mine.contains_key("r002"), "负值丢弃，绝不回绕");
        assert_eq!(mine.len(), 1, "inbound>>> 与不认的名字都不进表");
    }

    #[test]
    fn connections_and_proxies_bodies_parse_and_tolerate_junk() {
        // 形状抄自 `experimental/clashapi/connections.go` 的 MarshalJSON
        let conns = parse_connections(
            r#"{"downloadTotal":1,"uploadTotal":2,"connections":[
                {"id":"c1","metadata":{"network":"udp","host":"example.com"},
                 "upload":1,"download":2,"chains":["gate-r000","slot-0-out"],
                 "rule":"auth_user=r000 => route(gate-r000)","rulePayload":""},
                {"id":"c2","rule":"final","chains":[]},
                {"id":"c3"},
                {"rule":"auth_user=r001 => route(gate-r001)"}
            ],"memory":3}"#,
        );
        assert_eq!(
            conns,
            vec![
                Hy2ResiConn {
                    id: "c1".into(),
                    rule: "auth_user=r000 => route(gate-r000)".into(),
                    chains: vec!["gate-r000".into(), "slot-0-out".into()],
                },
                Hy2ResiConn {
                    id: "c2".into(),
                    rule: "final".into(),
                    chains: vec![],
                },
            ],
            "缺 id 或缺 rule 的条目跳过，不让整轮采样失败"
        );
        assert!(parse_connections("not json").is_empty());
        assert!(parse_connections("{}").is_empty());

        // 形状抄自 `experimental/clashapi/proxies.go` 的 proxyInfo：只有出站组才有 `now`
        let now = parse_proxies_now(
            r#"{"proxies":{
                "GLOBAL":{"type":"Fallback","now":"deny","all":["gate-r000"]},
                "gate-r000":{"type":"Selector","now":"slot-0-out","all":["deny","slot-0-out"]},
                "gate-r001":{"type":"Selector","now":"deny","all":["deny"]},
                "deny":{"type":"Socks","udp":true}
            }}"#,
        );
        assert_eq!(now.get("gate-r000").map(String::as_str), Some("slot-0-out"));
        assert_eq!(now.get("gate-r001").map(String::as_str), Some("deny"));
        assert!(!now.contains_key("deny"), "非组出站没有 now ⇒ 不进表");
        // sing-box 无条件塞的伪组不许混进门位表（proxies.go 的 getProxies），否则
        // 「遍历读回来的键」的写法会在生产上对它发 PUT（它不是 Selector，回 400）
        assert!(!now.contains_key("GLOBAL"), "GLOBAL 不是门位：{now:?}");
        assert_eq!(now.len(), 2, "只有 gate- 前缀的才是门位：{now:?}");
        assert!(parse_proxies_now("not json").is_empty());
    }

    /// 切门失败的两种文案与 relay 那条路**逐字一致**（T13 的哨兵签名表按这两句话认）。
    #[tokio::test]
    async fn select_errors_use_the_same_wording_as_the_relay_clash_client() {
        assert_eq!(
            ClashError::Rejected {
                tag: "deny".into(),
                status: 400
            }
            .to_string(),
            "Clash API 拒绝切换到 deny（HTTP 400）"
        );
        assert!(ClashError::Unreachable("x".into())
            .to_string()
            .starts_with("Clash API 不可达："));
        // 端口 1 本机永不监听（spec §2.2 的 deny 出站用的就是它）⇒ 必是「不可达」
        let c = Hy2ResiClient::with_apis("127.0.0.1:1", "127.0.0.1:1");
        let err = c.select("gate-r000", "deny").await.unwrap_err().to_string();
        assert!(err.starts_with("Clash API 不可达："), "{err}");
        assert!(!c.ready().await, "没人听 ⇒ ready() 为假，不是挂住");
        assert!(c.connections().await.is_err());
        assert!(c.selected_all().await.is_err());
        assert!(c.close_connection("c1").await.is_err());
        assert!(
            c.query_user_deltas().await.is_err(),
            "gRPC 也是 Err 不是 panic"
        );
    }

    /// 假 Clash API 的应答口径。
    ///
    /// `AllBad` 那一档是**非 2xx 的判据**专用：`select` / `clash_get` / `close_connection`
    /// 的 `if !(200..300)` 与 `ready()` 的 `status().is_success()` 四处，删掉任何一处
    /// 都会让「门根本没切动」被当成成功——到期 / 封禁用户静默 fail-open（spec §3.4 禁止），
    /// 而这正是 T11 fail-closed 收敛唯一的判据。只靠手工构造 `ClashError` 比字符串测不到
    /// 这件事：客户端得**真的**产出一次。
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ClashMode {
        /// 正常回；约定的坏 tag（`gate-bad` / 连接 `bad`）回非 2xx
        Ok,
        /// 一律回 500，`GET /version` 回 404（sing-box 对未知 selector 也是 404）
        AllBad,
    }

    /// 起一个**进程内的回环** HTTP 服务当假 Clash API：把「方法 + 路径 + 请求体 + 状态码」
    /// 这几件只在 HTTP 线上才成立的事真的验一遍（纯函数测不到，又最容易写错 ——
    /// `PUT /proxies/<门>` 的体漏了 `name` 就是门永远切不动、全员断网）。
    async fn fake_clash(mode: ClashMode) -> (u16, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                // 读满：hyper 常把请求头与请求体分两次写（口径同 `hy2::tests::fake_hysteria`）
                let mut raw: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 4096];
                let head_end = loop {
                    match raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        Some(i) => break i + 4,
                        None => {
                            let n = s.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break raw.len();
                            }
                            raw.extend_from_slice(&chunk[..n]);
                        }
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + want {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                let _ = tx.send(req.clone()).await;
                // 约定的坏 tag：真 sing-box 对「切到非成员 tag」回 400
                // `Selector update error: not found`（`experimental/clashapi/proxies.go:178-184`），
                // 对未知 selector / 未知连接 id 回 404（`:50-53`）。
                let bad = match mode {
                    ClashMode::AllBad => Some(if req.starts_with("GET /version") {
                        404
                    } else {
                        500
                    }),
                    ClashMode::Ok if req.starts_with("PUT /proxies/gate-bad") => Some(400),
                    ClashMode::Ok if req.starts_with("DELETE /connections/bad") => Some(404),
                    ClashMode::Ok => None,
                };
                // 只有 GET 才有响应体，PUT / DELETE 学 sing-box 回 204（render.NoContent）
                let body = if bad.is_some() {
                    ""
                } else if req.starts_with("GET /connections") {
                    r#"{"connections":[{"id":"c1","rule":"auth_user=r000 => route(gate-r000)","chains":[]}]}"#
                } else if req.starts_with("GET /proxies") {
                    r#"{"proxies":{"gate-r000":{"type":"Selector","now":"deny"}}}"#
                } else if req.starts_with("GET /version") {
                    r#"{"version":"sing-box 1.14.1","premium":true,"meta":true}"#
                } else {
                    ""
                };
                let resp = if let Some(status) = bad {
                    format!("HTTP/1.1 {status} Nope\r\nconnection: close\r\n\r\n")
                } else if body.is_empty() {
                    "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn the_clash_calls_use_the_documented_methods_paths_and_bodies() {
        let (port, mut rx) = fake_clash(ClashMode::Ok).await;
        let c = Hy2ResiClient::with_apis(format!("127.0.0.1:{port}"), "127.0.0.1:1");

        assert!(c.ready().await);
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /version HTTP/1.1"));

        assert_eq!(
            c.connections().await.unwrap(),
            vec![Hy2ResiConn {
                id: "c1".into(),
                rule: "auth_user=r000 => route(gate-r000)".into(),
                chains: vec![],
            }]
        );
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /connections HTTP/1.1"));

        assert_eq!(
            c.selected_all().await.unwrap(),
            BTreeMap::from([("gate-r000".to_string(), "deny".to_string())])
        );
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /proxies HTTP/1.1"));

        c.select("gate-r000", "slot-0-out").await.unwrap();
        let req = rx.recv().await.unwrap();
        assert!(
            req.starts_with("PUT /proxies/gate-r000 HTTP/1.1"),
            "请求行不对：{req}"
        );
        assert!(
            req.ends_with(r#"{"name":"slot-0-out"}"#),
            "体必须是 {{\"name\":\"<成员 tag>\"}}：{req}"
        );

        c.close_connection("c1").await.unwrap();
        let req = rx.recv().await.unwrap();
        assert!(
            req.starts_with("DELETE /connections/c1 HTTP/1.1"),
            "请求行不对：{req}"
        );
        // 两个面都不设 secret（spec §2.3）⇒ 一条也不发 Authorization
        assert!(
            !req.to_ascii_lowercase().contains("authorization:"),
            "{req}"
        );
    }

    /// **非 2xx 一律是失败，绝不当成静默的成功。**
    ///
    /// 这是 T11 fail-closed 门位收敛唯一的判据：真 sing-box 在「切到非成员 tag」时回 400
    /// `Selector update error: not found`（`experimental/clashapi/proxies.go:178-184`）、
    /// 对未知 selector 回 404（`:50-53`）。把 `select` 的 `if !(200..300)` 删掉，就会把
    /// 「门根本没切动」当成切门成功 —— 到期 / 封禁用户静默 fail-open，正是 spec §3.4
    /// 禁止的那一种错。四处判据（`select` / `clash_get` / `close_connection` / `ready`）
    /// 都必须由客户端**真的**产出一次错误来证明，手工构造 `ClashError` 比字符串证明不了。
    #[tokio::test]
    async fn a_non_2xx_answer_is_always_a_failure_never_a_silent_ok() {
        // ① select 的 400：文案与 relay 那条路逐字一致（T13 的哨兵按它认）
        let (port, _rx) = fake_clash(ClashMode::Ok).await;
        let c = Hy2ResiClient::with_apis(format!("127.0.0.1:{port}"), "127.0.0.1:1");
        let err = c.select("gate-bad", "deny").await.unwrap_err().to_string();
        assert_eq!(err, "Clash API 拒绝切换到 deny（HTTP 400）");
        // ② DELETE 的 404 也是失败；措辞不冒用「拒绝切换」（当时没在切门）
        let err = c.close_connection("bad").await.unwrap_err().to_string();
        assert_eq!(err, "Clash API 不可达：DELETE /connections/bad 回 HTTP 404");
        // 正常路照旧成功，证明上面两条是状态码判出来的、不是路径写错
        c.select("gate-r000", "deny").await.unwrap();
        c.close_connection("c1").await.unwrap();

        // ③ 两个读接口（clash_get）的 500：整轮采样 / 收敛必须失败，不许回空表当真源
        let (port, _rx) = fake_clash(ClashMode::AllBad).await;
        let c = Hy2ResiClient::with_apis(format!("127.0.0.1:{port}"), "127.0.0.1:1");
        assert_eq!(
            c.connections().await.unwrap_err().to_string(),
            "Clash API 不可达：GET /connections 回 HTTP 500",
            "回空表就是「谁都不在线」，踢人与计量会一起打空"
        );
        assert_eq!(
            c.selected_all().await.unwrap_err().to_string(),
            "Clash API 不可达：GET /proxies 回 HTTP 500",
            "回空表会被收敛当成「全部门位都不对」而全员重切"
        );
        // ④ ready() 的 404：有人听但答非 2xx ⇒ 仍然不算就绪（重放要等真起来）
        assert!(
            !c.ready().await,
            "答了 404 也不算就绪：重放会在门位还没生效时以为自己成了"
        );
    }
}
