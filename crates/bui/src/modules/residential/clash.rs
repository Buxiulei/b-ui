//! relay 的 Clash API 客户端（读/切每个 selector）与 tag ↔ upstream 的唯一换算。
//!
//! 切换只经 Clash API，不写 state、不重启 relay（契约决策 §C）：relay 的
//! `interrupt_exist_connections: false` 保证换选择不掐既有连接。

use super::{CLASH_API, CLASH_TIMEOUT_SECS, MEMBER_PREFIX};
// 生产实现按调用方传进来的 selector 拼路径，不再自己引用全局池的 tag；
// `unstable_pools` 要用它区分「全局池 vs. 槽池」，`FakeClash` 与测试用它当默认 selector
use super::journal::RelaySubject;
use super::state::ResiRuntime;
use super::POOL;
use bui_schema::model::ResidentialGroup;
use uuid::Uuid;

/// relay 的 Clash API（`127.0.0.1:9091`，只监听回环）。**同步** trait，调用点在
/// `spawn_blocking` 里（与 `Prober` 同一条铁律）。
///
/// `selector` 是 selector 出站的 tag：全局池是 [`POOL`]（`resi-pool`），每槽是
/// `slot-<i>-pool`（[`super::slot_selector`]）。两层 selector 的语义见总纲裁决 D8。
pub trait Clash: Send + Sync + 'static {
    /// Clash API 此刻能不能应答（`GET /version` 回 2xx）。relay 刚重启时它还没起监听，
    /// 重放选择前先探这一下（`health::replay_after_restart`）
    fn ready(&self) -> bool;
    /// `GET /proxies/<selector>` → `.now`；relay 未运行 / 无此 selector → `None`
    fn selected(&self, selector: &str) -> Option<String>;
    /// `PUT /proxies/<selector>` `{"name":"<tag>"}`；与配置切换走同一条 `SelectOutbound`
    /// 路径（调研 S4），`interrupt_exist_connections: false` 保证不掐既有连接
    fn select(&self, selector: &str, tag: &str) -> Result<(), ClashError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ClashError {
    #[error("Clash API 不可达：{0}")]
    Unreachable(String),
    #[error("Clash API 拒绝切换到 {tag}（HTTP {status}）")]
    Rejected { tag: String, status: u16 },
}

pub struct HttpClash {
    api: String,
    timeout: std::time::Duration,
}

impl HttpClash {
    pub fn new() -> Self {
        Self::with_api(CLASH_API, CLASH_TIMEOUT_SECS)
    }

    pub fn with_api(api: impl Into<String>, secs: u64) -> Self {
        Self {
            api: api.into(),
            timeout: std::time::Duration::from_secs(secs),
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, ClashError> {
        reqwest::blocking::Client::builder()
            // 回环地址，绝不能走系统代理（住宅上游本身就是代理，绕回去会死锁）
            .no_proxy()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ClashError::Unreachable(e.to_string()))
    }

    fn url(&self, selector: &str) -> String {
        format!("http://{}/proxies/{selector}", self.api)
    }
}

impl Default for HttpClash {
    fn default() -> Self {
        Self::new()
    }
}

impl Clash for HttpClash {
    fn ready(&self) -> bool {
        self.client()
            .ok()
            .and_then(|c| c.get(format!("http://{}/version", self.api)).send().ok())
            .is_some_and(|r| r.status().is_success())
    }

    fn selected(&self, selector: &str) -> Option<String> {
        let v: serde_json::Value = self
            .client()
            .ok()?
            .get(self.url(selector))
            .send()
            .ok()?
            .json()
            .ok()?;
        v.get("now")?.as_str().map(str::to_string)
    }

    fn select(&self, selector: &str, tag: &str) -> Result<(), ClashError> {
        let resp = self
            .client()?
            .put(self.url(selector))
            .json(&serde_json::json!({ "name": tag }))
            .send()
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(ClashError::Rejected {
                tag: tag.to_string(),
                status,
            })
        }
    }
}

/// 当前池的全部成员 tag，顺序与 `g.upstreams` 一致
pub fn tags(g: &ResidentialGroup) -> Vec<String> {
    (0..g.upstreams.len())
        .map(|i| format!("{MEMBER_PREFIX}{}", i + 1))
        .collect()
}

/// 上游 id → relay 里的成员 tag（`resi-{下标+1}`，与 `bui_schema::render::relay` 的
/// `tag()` 同规则；**唯一**一处做这个换算）
pub fn tag_of(g: &ResidentialGroup, id: Uuid) -> Option<String> {
    g.upstreams
        .iter()
        .position(|u| u.id == id)
        .map(|i| format!("{MEMBER_PREFIX}{}", i + 1))
}

/// 成员 tag → 上游 id
pub fn id_of_tag(g: &ResidentialGroup, tag: &str) -> Option<Uuid> {
    let n: usize = tag.strip_prefix(MEMBER_PREFIX)?.parse().ok()?;
    // tag 从 1 开始；0 与越界都返回 None
    g.upstreams.get(n.checked_sub(1)?).map(|u| u.id)
}

/// 归因**路径 A**（零 I/O）：relay 日志 reason 里 `dial tcp <ip>:<port>` 的那个地址就是上游
/// 自己的 host:port（Go `net.Dialer` 的错误原文自带）→ 上游 id。
///
/// 只认**唯一**命中（host 大小写不敏感 + 端口相等）：一个都不中、或多个上游共用同一个
/// host:port（同 IP 不同凭据）⇒ `None`，交给路径 B，**绝不猜**。上游配的是域名时，报错里
/// 已经是解析后的 IP、与 [`Upstream::host`] 不字面相等 ⇒ 这里也匹配不上，同样由路径 B 兜住
/// （**不做 DNS 解析**：那要 I/O，而路径 B 本来就更准）。
///
/// **已知边角**（不修，代价可接受）：池里一条上游按 IP 录入、另一条按域名录入，而那个域名
/// 恰好解析到同一个 `IP:port` 时，报错原文里的地址是解析后的 IP ⇒ 本函数会**自信地**归到
/// 按 IP 录入的那一条，另一条的失败就记错了人。判据只看字面，看不出这种重合；要看出来得做
/// DNS 解析（I/O，而且解析结果随时会变）。真正的防线是别把同一个出口重复录两遍。
///
/// [`Upstream::host`]: bui_schema::model::Upstream::host
pub fn id_of_dial_addr(g: &ResidentialGroup, host: &str, port: u16) -> Option<Uuid> {
    let mut hit = g
        .upstreams
        .iter()
        .filter(|u| u.port == port && u.host.eq_ignore_ascii_case(host));
    let first = hit.next()?;
    hit.next().is_none().then_some(first.id)
}

/// 一批池 selector 此刻各自选中谁（`None` = relay 没起 / 没这个 selector / 读不到）
pub type PoolNow = std::collections::BTreeMap<String, Option<String>>;

/// 把一批池的 `now` 一次读回来，**每个池只查一次**。`Clash` 是同步 trait ⇒ 整批放进一次
/// `spawn_blocking`（与 `Prober` 同一条铁律）。池数上限是 `MAX_SLOTS + 1`，量很小。
pub async fn pool_now(clash: &std::sync::Arc<dyn Clash>, pools: &[String]) -> PoolNow {
    if pools.is_empty() {
        return PoolNow::new();
    }
    let (c, ps) = (clash.clone(), pools.to_vec());
    tokio::task::spawn_blocking(move || {
        ps.into_iter()
            .map(|p| {
                let now = c.selected(&p);
                (p, now)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// 每个池 selector 最近一次成功切换的时刻（RFC3339），真源是
/// [`super::state::ResiRuntime::pool_switch_at`]
pub type PoolSwitchAt = std::collections::BTreeMap<String, String>;

/// 这条日志行落在该池切换的宽限窗里吗（`ts < last_switch_at[pool] + grace_secs`）。
///
/// 只对**路径 B** 有意义：`now` 说的是「此刻选中谁」，切换之前产生的行说的是老成员。
/// 没记过切换（进程刚起）、或时刻解析不出来 ⇒ `false`（不丢弃：宁可少丢，也不因为读不到
/// 记录就把整条链停掉）。
///
/// `grace_secs` 由调用方按这条行的 reason 现算（[`switch_grace_secs`]）：超时类的行的
/// 时间戳比路由决策晚一个完整超时，1 秒的门拦不住。
pub fn within_switch_grace(
    switch_at: &PoolSwitchAt,
    pool: &str,
    ts: time::OffsetDateTime,
    grace_secs: i64,
) -> bool {
    let Some(t0) = switch_at
        .get(pool)
        .and_then(|s| crate::util::parse_rfc3339(s))
    else {
        return false;
    };
    ts < t0 + time::Duration::seconds(grace_secs)
}

/// 这条 relay 错误行的路径 B 宽限窗该取多少秒（2026-09-18 第四次裁决 ④）。
///
/// 命中「连不上上游 / 超时」那一组**既有**标记
/// （[`crate::modules::sentinel::signature::is_unreachable_reason`]，= 哨兵
/// `Sig::RelayUpstreamError` 的判据）⇒ [`super::SWITCH_ATTRIB_GRACE_TIMEOUT_SECS`]；
/// 其余 ⇒ [`super::SWITCH_ATTRIB_GRACE_SECS`]。
///
/// `text` 可以是整条 message，也可以只是 reason —— 判据是 `contains`，两者同结论。
pub fn switch_grace_secs(text: &str) -> i64 {
    if crate::modules::sentinel::signature::is_unreachable_reason(text) {
        super::SWITCH_ATTRIB_GRACE_TIMEOUT_SECS
    } else {
        super::SWITCH_ATTRIB_GRACE_SECS
    }
}

/// 一行 relay 错误行的归因结论
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attrib {
    /// 归到这条上游
    Upstream(Uuid),
    /// 路径 B 的行落在该池切换的宽限窗内：本批证据放弃、等下次复发
    Switched,
    /// 两条路径都归不了因：丢弃，**绝不猜**
    Unknown,
}

/// relay 一行日志的出站主体 → 上游 id。
///
/// - **路径 A**（零 I/O，不受时间戳门约束）：成员 tag 直接换算（sing-box 写的就是当时失败的
///   那个成员），或 reason 里 `dial tcp <ip>:<port>` 的上游地址（[`id_of_dial_addr`]，写的就是
///   当时实际拨的那个上游）——两者说的都是**过去**，与此刻选中谁无关；
/// - **路径 B**（本轮缓存的 `now` → [`id_of_tag`]）：`now` 说的是**此刻**，所以先过时间戳门
///   （[`within_switch_grace`]），落在切换宽限窗里的行判 [`Attrib::Switched`]。
///
/// 两条都不成 ⇒ [`Attrib::Unknown`]，调用方丢弃该行并记一行 debug。
///
/// `grace_secs` 是这条行自己的宽限窗（[`switch_grace_secs`]）：超时类 reason 要宽得多。
pub fn attribute(
    g: &ResidentialGroup,
    s: &RelaySubject,
    now: &PoolNow,
    switch_at: &PoolSwitchAt,
    ts: time::OffsetDateTime,
    grace_secs: i64,
) -> Attrib {
    let unknown = |o: Option<Uuid>| o.map_or(Attrib::Unknown, Attrib::Upstream);
    match s {
        RelaySubject::Member(tag) => unknown(id_of_tag(g, tag)),
        RelaySubject::Pool { pool, dial_addr } => {
            if let Some(id) = dial_addr
                .as_ref()
                .and_then(|(h, p)| id_of_dial_addr(g, h, *p))
            {
                return Attrib::Upstream(id);
            }
            if within_switch_grace(switch_at, pool, ts, grace_secs) {
                return Attrib::Switched;
            }
            unknown(now.get(pool).and_then(|t| id_of_tag(g, t.as_deref()?)))
        }
    }
}

/// 池形态的行属于哪个池（成员形态 → `None`）：[`unstable_pools`] 按池整批丢弃时要用
pub fn pool_of(s: &RelaySubject) -> Option<&str> {
    match s {
        RelaySubject::Pool { pool, .. } => Some(pool),
        RelaySubject::Member(_) => None,
    }
}

/// 某池此刻 relay 里选中的上游（`None` = `now` 读不到、或那个 tag 已不在当前池里）
fn now_id(g: &ResidentialGroup, now: &PoolNow, pool: &str) -> Option<Uuid> {
    id_of_tag(g, now.get(pool)?.as_deref()?)
}

/// runtime 记下的「这个池当前该生效的上游」：全局池是
/// [`ResiRuntime::selected_upstream_id`]，槽池是该槽的
/// [`super::state::SlotRuntime::current_upstream_id`]。`None` = 没记过（首轮 / 刚被清零），
/// 无从比较。**换算只经 [`super::slot_selector`]**，不在这里再拼一遍 `slot-<i>-pool`。
fn recorded_id(r: &ResiRuntime, pool: &str) -> Option<Uuid> {
    if pool == POOL {
        return r.selected_upstream_id;
    }
    r.slots
        .iter()
        .find(|(k, _)| {
            k.parse::<u16>()
                .is_ok_and(|i| super::slot_selector(i) == pool)
        })
        .and_then(|(_, s)| s.current_upstream_id)
}

/// 归因前的**通用兜底**（2026-09-18 第三次裁决 ③）：某池此刻的 `now` 与 runtime 记下的
/// 「该池当前选择」不一致 ⇒ 中间发生过一次**没记账**的切换（relay 重启把 selector 打回
/// 配置里的 default、有人手动 `curl` 过 Clash API、某条落账路径漏写…）。这些池本批的
/// **路径 B** 行整批丢弃，调用方还要当场 `note_pool_switch(now)` 把账补上 —— 否则这一批
/// 之后的每一批都会继续把老成员的失败记到新成员头上。
///
/// 只在两边**都读得到**时才下结论：`now` 读不到（relay 没起）时路径 B 本来就归不了因；
/// runtime 没记过该池的选择时无从比较 —— 宁可少丢，也不因为读不到记录就把整条链停掉
/// （与 [`within_switch_grace`] 同口径）。
pub fn unstable_pools(
    g: &ResidentialGroup,
    now: &PoolNow,
    r: &ResiRuntime,
) -> std::collections::BTreeSet<String> {
    now.keys()
        .filter(
            |p| match (now_id(g, now, p.as_str()), recorded_id(r, p.as_str())) {
                (Some(n), Some(rec)) => n != rec,
                _ => false,
            },
        )
        .cloned()
        .collect()
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeClashInner {
    /// 每个 selector 各记一份当前选择（键 = selector tag）
    pub now: std::collections::BTreeMap<String, String>,
    /// 令 `select` 失败（测「切换失败只告警不改 runtime」）
    pub reject: bool,
    /// 只拒绝切到这些 tag（`reject` 是全拒）：测「候选都不通、连放回本槽 IP 都失败」
    pub reject_tags: std::collections::BTreeSet<String>,
    /// 接下来这么多次调用（`ready` / `selected` / `select` 都算）按「连接被拒」处理：
    /// relay 刚重启、Clash API 还没起监听时的形态
    pub refuse: u32,
    pub calls: Vec<String>,
    /// 每次**成功**的 `select` 把这台假主机的时钟往前拨这么多秒：真实世界里「轮首取
    /// `now` → 发 PUT → select 返回」之间是有时间差的，归因时间戳门的用例要把它复现出来
    pub advance_on_select: Option<(std::sync::Arc<crate::sys::fake::FakeHost>, i64)>,
    /// 每次 `selected`（= `clash::pool_now` 问 `now` 的那一刻）先跑一下这个钩子：
    /// 用来把「采日志 → 读 `now`」之间发生的切换注入进去（黑名单读序的用例）
    pub on_selected: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

#[cfg(test)]
pub struct FakeClash {
    inner: std::sync::Mutex<FakeClashInner>,
}

#[cfg(test)]
impl FakeClash {
    /// `now` 播种在全局 selector [`POOL`] 上（每槽 selector 一开始都读不到）
    pub fn new(now: Option<&str>) -> Self {
        Self {
            inner: std::sync::Mutex::new(FakeClashInner {
                now: now
                    .map(|t| [(POOL.to_string(), t.to_string())].into_iter().collect())
                    .unwrap_or_default(),
                ..Default::default()
            }),
        }
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeClashInner)) -> &Self {
        f(&mut self.inner.lock().unwrap());
        self
    }

    /// `ready` / `get:<selector>` / `put:<selector>:<tag>`
    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// 读某个 selector 的当前选择，**不记调用、不吃 `refuse`**（断言用）
    pub fn peek(&self, selector: &str) -> Option<String> {
        self.inner.lock().unwrap().now.get(selector).cloned()
    }
}

/// 这一次调用该不该按「连接被拒」处理（顺带把 `refuse` 减一）
#[cfg(test)]
fn refused(i: &mut FakeClashInner) -> bool {
    if i.refuse == 0 {
        return false;
    }
    i.refuse -= 1;
    true
}

#[cfg(test)]
impl Clash for FakeClash {
    fn ready(&self) -> bool {
        let mut i = self.inner.lock().unwrap();
        i.calls.push("ready".into());
        !refused(&mut i)
    }

    fn selected(&self, selector: &str) -> Option<String> {
        // 钩子在拿锁之前跑：它自己可能回头读 `FakeClash`
        let hook = self.inner.lock().unwrap().on_selected.clone();
        if let Some(f) = hook {
            f();
        }
        let mut i = self.inner.lock().unwrap();
        i.calls.push(format!("get:{selector}"));
        if refused(&mut i) {
            return None;
        }
        i.now.get(selector).cloned()
    }

    fn select(&self, selector: &str, tag: &str) -> Result<(), ClashError> {
        let mut i = self.inner.lock().unwrap();
        i.calls.push(format!("put:{selector}:{tag}"));
        if refused(&mut i) {
            return Err(ClashError::Unreachable("connection refused".into()));
        }
        if i.reject || i.reject_tags.contains(tag) {
            return Err(ClashError::Rejected {
                tag: tag.to_string(),
                status: 404,
            });
        }
        i.now.insert(selector.to_string(), tag.to_string());
        // 切成功才走时钟：切失败什么都没变，时刻也不该动
        if let Some((h, secs)) = i.advance_on_select.clone() {
            h.advance(secs);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;

    fn group(n: usize) -> ResidentialGroup {
        let upstreams = (0..n)
            .map(|i| Upstream {
                id: Uuid::from_u128(i as u128 + 1),
                name: format!("url-{}", i + 1),
                kind: UpstreamKind::Http,
                host: format!("isp{}.example.net", i + 1),
                port: 10007,
                username: "user1".into(),
                password: "pw1".into(),
                priority: 10,
                provider: None,
                region: None,
                ports_allowed: None,
                verified: None,
            })
            .collect();
        ResidentialGroup {
            enabled: true,
            upstreams,
            ..Default::default()
        }
    }

    #[test]
    fn tag_mapping_matches_render_relay() {
        // 与 crates/bui-schema/src/render/relay.rs 的 `format!("resi-{}", i + 1)` 逐字对应
        let g = group(3);
        assert_eq!(tag_of(&g, Uuid::from_u128(1)).as_deref(), Some("resi-1"));
        assert_eq!(tag_of(&g, Uuid::from_u128(3)).as_deref(), Some("resi-3"));
        assert_eq!(tag_of(&g, Uuid::from_u128(9)), None, "不在池里就没有 tag");
        assert_eq!(id_of_tag(&g, "resi-2"), Some(Uuid::from_u128(2)));
        assert_eq!(id_of_tag(&g, "resi-4"), None, "越界");
        assert_eq!(id_of_tag(&g, "resi-0"), None, "tag 从 1 开始");
        assert_eq!(id_of_tag(&g, "direct"), None, "非成员 tag");
        assert_eq!(id_of_tag(&g, "resi-x"), None);
        assert_eq!(tags(&g), vec!["resi-1", "resi-2", "resi-3"]);
        assert!(tags(&ResidentialGroup::default()).is_empty());
    }

    #[test]
    fn fake_clash_records_calls_per_selector() {
        let c = FakeClash::new(Some("resi-1"));
        assert_eq!(c.selected(POOL).as_deref(), Some("resi-1"));
        // 每槽 selector 各自独立记账：一个槽切走不影响别的槽
        assert_eq!(
            c.selected("slot-1-pool"),
            None,
            "没播种过的 selector 读不到 now"
        );
        c.select(POOL, "resi-2").unwrap();
        c.select("slot-1-pool", "resi-3").unwrap();
        assert_eq!(c.selected(POOL).as_deref(), Some("resi-2"));
        assert_eq!(c.selected("slot-1-pool").as_deref(), Some("resi-3"));
        c.with(|i| i.reject = true);
        assert!(matches!(
            c.select(POOL, "resi-1"),
            Err(ClashError::Rejected { .. })
        ));
        assert_eq!(c.selected(POOL).as_deref(), Some("resi-2"), "失败不改 now");
        assert_eq!(
            c.calls(),
            vec![
                "get:resi-pool",
                "get:slot-1-pool",
                "put:resi-pool:resi-2",
                "put:slot-1-pool:resi-3",
                "get:resi-pool",
                "get:slot-1-pool",
                "put:resi-pool:resi-1",
                "get:resi-pool",
            ]
        );
    }

    // 都带 `Connection: close` 且不写 Content-Length：服务端写完即关，客户端读到 EOF 为止，
    // 免得测试里手算长度
    const OK_NOW: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"now\":\"resi-2\"}";
    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n{}";

    /// 进程内假 Clash API：按顺序对每个来访连接回 `responses[i]`，返回收到的请求原文
    /// （小请求一次 read 就能读全首行 + 首部 + body，够断言用）。
    fn fake_clash_api(
        responses: Vec<&'static str>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for resp in responses {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            }
            seen
        });
        (api, h)
    }

    #[test]
    fn http_clash_reads_now_puts_the_tag_and_maps_a_rejection() {
        let (api, h) = fake_clash_api(vec![OK_NOW, NO_CONTENT, NOT_FOUND]);
        let c = HttpClash::with_api(api, 2);
        assert_eq!(c.selected(POOL).as_deref(), Some("resi-2"));
        c.select(POOL, "resi-3").unwrap();
        assert!(matches!(
            c.select(POOL, "resi-9"),
            Err(ClashError::Rejected { status: 404, .. })
        ));
        let reqs = h.join().unwrap();
        assert!(
            reqs[0].starts_with("GET /proxies/resi-pool HTTP/1.1\r\n"),
            "实际 {:?}",
            reqs[0]
        );
        assert!(
            reqs[1].starts_with("PUT /proxies/resi-pool HTTP/1.1\r\n"),
            "实际 {:?}",
            reqs[1]
        );
        assert!(
            reqs[1].contains(r#"{"name":"resi-3"}"#),
            "PUT 的 body 就是 name 字段：{:?}",
            reqs[1]
        );
    }

    #[test]
    fn an_unreachable_clash_api_is_none_not_a_panic() {
        // 没人监听的回环端口 = relay 还没起来 / 旧配置没有 clash_api 时的形态
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = l.local_addr().unwrap().to_string();
        drop(l);
        let c = HttpClash::with_api(api, 1);
        assert!(!c.ready(), "没人监听 ⇒ 未就绪");
        assert_eq!(
            c.selected(POOL),
            None,
            "读不到当前选择 ⇒ 巡检本轮不切（T8 规则 5）"
        );
        assert!(matches!(
            c.select(POOL, "resi-1"),
            Err(ClashError::Unreachable(_))
        ));
    }

    #[test]
    fn http_clash_ready_probes_the_version_endpoint() {
        let (api, h) = fake_clash_api(vec![OK_NOW, NOT_FOUND]);
        let c = HttpClash::with_api(api, 2);
        assert!(c.ready(), "2xx ⇒ 就绪");
        assert!(!c.ready(), "非 2xx ⇒ 未就绪");
        let reqs = h.join().unwrap();
        assert!(
            reqs[0].starts_with("GET /version HTTP/1.1\r\n"),
            "实际 {:?}",
            reqs[0]
        );
    }

    /// 路径 A：`dial tcp <ip>:<port>` 唯一命中才算，**绝不猜**
    #[test]
    fn the_dial_address_attributes_only_on_a_unique_hit() {
        let mut g = group(2);
        g.upstreams[0].host = "203.0.113.7".into();
        assert_eq!(
            id_of_dial_addr(&g, "203.0.113.7", 10007),
            Some(Uuid::from_u128(1))
        );
        assert_eq!(id_of_dial_addr(&g, "203.0.113.7", 10008), None, "端口不对");
        assert_eq!(
            id_of_dial_addr(&g, "203.0.113.8", 10007),
            None,
            "池里没有这个地址；上游配的是域名时报错里是解析后的 IP，也走这一支（交给路径 B）"
        );
        // 同一个 IP:端口被两条凭据共用 ⇒ 分不清是哪条，不猜
        g.upstreams[1].host = "203.0.113.7".into();
        assert_eq!(id_of_dial_addr(&g, "203.0.113.7", 10007), None);
    }

    /// 归因：成员 tag 直接换；池先走路径 A 再走路径 B；两条都不成 ⇒ [`Attrib::Unknown`]
    #[test]
    fn subject_attribution_prefers_the_dial_address_over_the_clash_now() {
        let mut g = group(2);
        g.upstreams[1].host = "203.0.113.8".into();
        let now: PoolNow = [("slot-0-pool".to_string(), Some("resi-1".to_string()))]
            .into_iter()
            .collect();
        let never = PoolSwitchAt::new();
        let pool = |dial: Option<(&str, u16)>| RelaySubject::Pool {
            pool: "slot-0-pool".into(),
            dial_addr: dial.map(|(h, p)| (h.to_string(), p)),
        };
        let at = |s: &RelaySubject| attribute(&g, s, &now, &never, T1, GRACE);
        assert_eq!(
            at(&RelaySubject::Member("resi-2".into())),
            Attrib::Upstream(Uuid::from_u128(2))
        );
        assert_eq!(
            at(&pool(Some(("203.0.113.8", 10007)))),
            Attrib::Upstream(Uuid::from_u128(2)),
            "路径 A 说的是『刚才拨的是谁』，优先于 `now` 的『此刻选中谁』"
        );
        assert_eq!(
            at(&pool(None)),
            Attrib::Upstream(Uuid::from_u128(1)),
            "没有地址线索 ⇒ 路径 B"
        );
        let empty = PoolNow::new();
        assert_eq!(
            attribute(&g, &pool(None), &empty, &never, T1, GRACE),
            Attrib::Unknown,
            "两条都不成"
        );
        assert_eq!(
            at(&RelaySubject::Member("resi-9".into())),
            Attrib::Unknown,
            "成员 tag 不在池里"
        );
    }

    const T0: time::OffsetDateTime = time::macros::datetime!(2026-09-18 12:00:00 UTC);
    const T1: time::OffsetDateTime = time::macros::datetime!(2026-09-18 12:00:01 UTC);
    /// 非超时类 reason 的宽限窗（`SWITCH_ATTRIB_GRACE_SECS`）
    const GRACE: i64 = crate::modules::residential::SWITCH_ATTRIB_GRACE_SECS;
    /// 超时类 reason 的宽限窗（`SWITCH_ATTRIB_GRACE_TIMEOUT_SECS`）
    const GRACE_T: i64 = crate::modules::residential::SWITCH_ATTRIB_GRACE_TIMEOUT_SECS;

    /// 时间戳门（2026-09-18 第二次裁决）：池在 `t0` 被切过，`ts < t0 + GRACE` 的行落在窗里
    #[test]
    fn the_grace_window_covers_everything_up_to_one_second_after_the_switch() {
        let sw: PoolSwitchAt = [(
            "slot-0-pool".to_string(),
            "2026-09-18T12:00:00Z".to_string(),
        )]
        .into_iter()
        .collect();
        assert!(within_switch_grace(
            &sw,
            "slot-0-pool",
            T0 - time::Duration::seconds(30),
            GRACE
        ));
        assert!(within_switch_grace(&sw, "slot-0-pool", T0, GRACE));
        assert!(
            within_switch_grace(
                &sw,
                "slot-0-pool",
                T1 - time::Duration::milliseconds(1),
                GRACE
            ),
            "GRACE = 1 秒，边界之前仍在窗内"
        );
        assert!(
            !within_switch_grace(&sw, "slot-0-pool", T1, GRACE),
            "t0 + GRACE 起放行"
        );
        assert!(
            !within_switch_grace(&sw, "resi-pool", T0, GRACE),
            "别的池没被切过，不受牵连"
        );
        assert!(
            !within_switch_grace(&PoolSwitchAt::new(), "slot-0-pool", T0, GRACE),
            "没记过切换（进程刚起）⇒ 不丢弃"
        );
        let bad: PoolSwitchAt = [("slot-0-pool".to_string(), "不是时刻".to_string())]
            .into_iter()
            .collect();
        assert!(
            !within_switch_grace(&bad, "slot-0-pool", T0, GRACE),
            "解析不出来 ⇒ 不丢弃"
        );
    }

    /// 门只管路径 B：路径 A（`dial tcp` 地址 / 成员 tag）写的就是当时实际拨的那个上游
    #[test]
    fn the_switch_gate_drops_path_b_only() {
        let mut g = group(2);
        g.upstreams[1].host = "203.0.113.8".into();
        let now: PoolNow = [("slot-0-pool".to_string(), Some("resi-1".to_string()))]
            .into_iter()
            .collect();
        let sw: PoolSwitchAt = [(
            "slot-0-pool".to_string(),
            "2026-09-18T12:00:00Z".to_string(),
        )]
        .into_iter()
        .collect();
        let pool = |dial: Option<(&str, u16)>| RelaySubject::Pool {
            pool: "slot-0-pool".into(),
            dial_addr: dial.map(|(h, p)| (h.to_string(), p)),
        };
        // 切换之前产生的路径 B 行：`now` 说的是新成员，这条行说的是老成员 ⇒ 丢
        assert_eq!(
            attribute(
                &g,
                &pool(None),
                &now,
                &sw,
                T0 - time::Duration::seconds(5),
                GRACE
            ),
            Attrib::Switched
        );
        // 同一批里的路径 A 行不受门影响
        assert_eq!(
            attribute(
                &g,
                &pool(Some(("203.0.113.8", 10007))),
                &now,
                &sw,
                T0 - time::Duration::seconds(5),
                GRACE
            ),
            Attrib::Upstream(Uuid::from_u128(2))
        );
        assert_eq!(
            attribute(
                &g,
                &RelaySubject::Member("resi-2".into()),
                &now,
                &sw,
                T0 - time::Duration::seconds(5),
                GRACE
            ),
            Attrib::Upstream(Uuid::from_u128(2)),
            "成员 tag 是 sing-box 自己写的那个成员，同样不受门约束"
        );
        // 宽限窗之后的路径 B 行照常归到新成员
        assert_eq!(
            attribute(&g, &pool(None), &now, &sw, T1, GRACE),
            Attrib::Upstream(Uuid::from_u128(1))
        );
    }

    /// **裁决 ④**（2026-09-18 第四次）：超时类 reason 的日志时刻比**路由决策时刻**晚一个
    /// 完整的拨号 / 握手超时（选中成员 → 拨 → 等超时 → 才写日志），所以切换之后才落盘的
    /// 那条行说的可能还是**老**成员。1 秒的门拦不住，这类行改用
    /// [`super::SWITCH_ATTRIB_GRACE_TIMEOUT_SECS`]；其余 reason 照旧 1 秒。
    #[test]
    fn timeout_class_reasons_ride_the_wide_grace_window() {
        // 判据只复用哨兵那一组既有标记，没有新造
        assert_eq!(
            switch_grace_secs("dial tcp 203.0.113.7:10007: i/o timeout"),
            GRACE_T
        );
        assert_eq!(switch_grace_secs("context deadline exceeded"), GRACE_T);
        assert_eq!(
            switch_grace_secs(
                "open connection to www.example.com:443 using outbound/selector[slot-0-pool]: \
                 context deadline exceeded"
            ),
            GRACE_T,
            "喂整条 message 与只喂 reason 同结论（判据是 contains）"
        );
        assert_eq!(
            switch_grace_secs("socks5: request rejected, code=2"),
            GRACE,
            "上游拒绝目标是当场应答，没有超时延迟"
        );
        assert_eq!(switch_grace_secs("unexpected status: 403 Forbidden"), GRACE);

        let g = group(2);
        let now: PoolNow = [("slot-0-pool".to_string(), Some("resi-1".to_string()))]
            .into_iter()
            .collect();
        let sw: PoolSwitchAt = [(
            "slot-0-pool".to_string(),
            "2026-09-18T12:00:00Z".to_string(),
        )]
        .into_iter()
        .collect();
        let line = RelaySubject::Pool {
            pool: "slot-0-pool".into(),
            dial_addr: None,
        };
        let at = |secs: i64, reason: &str| {
            attribute(
                &g,
                &line,
                &now,
                &sw,
                T0 + time::Duration::seconds(secs),
                switch_grace_secs(reason),
            )
        };
        assert_eq!(
            at(10, "context deadline exceeded"),
            Attrib::Switched,
            "切换后 10 秒打出的超时行：拨号是切换之前发起的，说的是老成员 ⇒ 本批放弃"
        );
        assert_eq!(
            at(5, "socks5: request rejected, code=2"),
            Attrib::Upstream(Uuid::from_u128(1)),
            "切换后 5 秒的拒绝行没有超时延迟，照常归到此刻选中的成员"
        );
        assert_eq!(
            at(GRACE_T, "context deadline exceeded"),
            Attrib::Upstream(Uuid::from_u128(1)),
            "超时类也不是永远丢：t0 + 30 秒起放行"
        );
    }

    #[test]
    fn a_refusing_fake_clash_answers_like_a_relay_that_is_not_listening_yet() {
        let c = FakeClash::new(Some("resi-1"));
        c.with(|i| i.refuse = 3);
        assert!(!c.ready());
        assert_eq!(c.selected(POOL), None);
        assert!(matches!(
            c.select(POOL, "resi-2"),
            Err(ClashError::Unreachable(_))
        ));
        assert!(c.ready(), "拒绝次数用完就恢复");
        assert_eq!(
            c.peek(POOL).as_deref(),
            Some("resi-1"),
            "被拒的 PUT 不改 now"
        );
        assert_eq!(
            c.calls(),
            vec!["ready", "get:resi-pool", "put:resi-pool:resi-2", "ready"],
            "peek 不记调用"
        );
    }

    #[test]
    fn http_clash_addresses_the_selector_in_the_path() {
        let (api, h) = fake_clash_api(vec![OK_NOW, NO_CONTENT]);
        let c = HttpClash::with_api(api, 2);
        assert_eq!(c.selected("slot-2-pool").as_deref(), Some("resi-2"));
        c.select("slot-2-pool", "resi-3").unwrap();
        let reqs = h.join().unwrap();
        assert!(
            reqs[0].starts_with("GET /proxies/slot-2-pool HTTP/1.1\r\n"),
            "实际 {:?}",
            reqs[0]
        );
        assert!(reqs[1].starts_with("PUT /proxies/slot-2-pool HTTP/1.1\r\n"));
        assert!(reqs[1].contains(r#"{"name":"resi-3"}"#));
    }
}
