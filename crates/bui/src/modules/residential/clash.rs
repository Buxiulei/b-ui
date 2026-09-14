//! relay 的 Clash API 客户端（读/切每个 selector）与 tag ↔ upstream 的唯一换算。
//!
//! 切换只经 Clash API，不写 state、不重启 relay（契约决策 §C）：relay 的
//! `interrupt_exist_connections: false` 保证换选择不掐既有连接。

use super::{CLASH_API, CLASH_TIMEOUT_SECS, MEMBER_PREFIX};
// 生产实现按调用方传进来的 selector 拼路径，不再自己引用全局池的 tag；
// `FakeClash` 与测试仍要用它当默认 selector
#[cfg(test)]
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
