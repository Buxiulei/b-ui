//! 住宅出口模块（spec §5）：上游池、出口体检、健康巡检与热切换、自动黑名单、面板端点与 CLI。
//!
//! 边界（契约决策 §B）：本模块**不渲染任何内核配置**。`singbox-relay.json` 由
//! `crate::modules::core_files`（P1 Task 10）从 `state.residential.groups["default"]`
//! 渲染；P3 只改 `state`，由 P1 的对账器完成重渲染、`sing-box check` 与 relay 重启。

use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use bui_schema::model::{State, DEFAULT_GROUP};
use bui_schema::paths::Paths;
use std::sync::Arc;

pub mod api;
pub mod blacklist;
pub mod check;
pub mod clash;
pub mod cli;
pub mod health;
pub mod journal;
pub mod proxy;
pub mod slots;
pub mod state;
pub mod upstream;

/// `RuntimeData.extra` 里本模块独占的键（P1 Task 2 的 `#[serde(flatten)] extra`）
pub const RUNTIME_KEY: &str = "residential";
/// 住宅分组（v4 只有一个；直接用 `bui-schema` 的常量，不另立字符串）
pub const GROUP_DEFAULT: &str = DEFAULT_GROUP;
/// relay 里全局 selector 的 tag（真源在 `bui_schema::render::relay`，这里只是转出来给
/// 巡检与 Clash 客户端用）
pub const POOL: &str = bui_schema::render::relay::POOL;
/// 成员 tag 前缀，`resi-1..resi-N`，N = `upstreams` 的下标 +1（与 render/relay.rs 的 `tag()` 同规则）
pub const MEMBER_PREFIX: &str = "resi-";
/// 池上限：relay 每个成员一个出站，再多面板表格与 Clash API 都不好用了（v3 面板 `slice(0,8)`）
pub const MAX_UPSTREAMS: usize = 8;
/// 黑名单候选要跟的 journald 单元
pub const JOURNAL_UNIT: &str = "b-ui-relay";
/// 单次探测硬超时（spec §5.2）
pub const PROBE_TIMEOUT_SECS: u64 = 10;
/// 探测并发上限（spec §5.2）
pub const PROBE_CONCURRENCY: usize = 4;
/// Clash API 地址与超时（地址与 P1 Task 10 的 relay 渲染同值，不另立常量）
pub const CLASH_API: &str = crate::modules::core_files::RELAY_CLASH_API;
pub const CLASH_TIMEOUT_SECS: u64 = 2;
/// 健康巡检：每 2 分钟一轮、每成员 2 次探测、任一成功即本轮健康（spec §5.3）
pub const HEALTH_INTERVAL_SECS: u64 = 120;
pub const HEALTH_PROBE_URL: &str = "https://www.gstatic.com/generate_204";
/// [`HEALTH_PROBE_URL`] 的主机名。`get` 失败后要经上游自写一次 CONNECT 才能把
/// 「凭据失效（407）」从「不可达」里分出来（T3 `confirm_auth_failure` 的注释讲了原因），
/// 那一步只要 host，不要 URL
pub const HEALTH_PROBE_HOST: &str = "www.gstatic.com";
pub const HEALTH_TRIES: u32 = 2;
/// Google 可达性探测（主理人硬要求「住宅上游不封 Google」，R2 ②）：巡检每轮每成员
/// 多打这一次，体检也带这一项。**必须打真实搜索路径**：Bright Data 这类上游对 serp
/// 域名整域硬拒（`403 Forbidden serp domain`），只探首页看不出来。
pub const GOOGLE_PROBE_URL: &str = "https://www.google.com/search?q=weather";
/// 浏览器 UA：无 UA / 脚本 UA 会被 Google 自己判机器人（回 `/sorry/`），
/// 那测出来的是「我们像机器人」，不是「这条上游封了 Google」
pub const GOOGLE_PROBE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// Google 探测的硬超时：比 [`PROBE_TIMEOUT_SECS`] 短，一轮巡检多这一次请求不能拖垮整轮
pub const GOOGLE_PROBE_TIMEOUT_SECS: u64 = 8;
/// 每轮延迟样本的 HTTP 目标（主理人 2026-09-12：「巡检要给出延迟」）。204 空响应，
/// 正文 0 字节 —— 每 2 分钟每成员一次，用带正文的目标等于白烧住宅流量。
/// **这一次请求同时充当本轮的第一次连通性探测**（`probe_reachable` 的 try #1），
/// 所以健康成员每轮的 HTTP 请求数与加这一项之前一样是 2 次（本项 + Google 搜索）
pub const LATENCY_PROBE_URL: &str = "https://www.google.com/generate_204";
/// 延迟探测的超时：与 [`GOOGLE_PROBE_TIMEOUT_SECS`] 同值（主理人口径 `--max-time 8`）
pub const LATENCY_PROBE_TIMEOUT_SECS: u64 = 8;
/// 每成员保留的延迟样本数（环形），p50/p95 从这些样本算。30 × 2 分钟 = 近 1 小时
pub const LATENCY_SAMPLES_MAX: usize = 30;

/// 每小时测速（主理人 2026-09-12：「要给出上下行速度，目的是选最快的住宅代理」）。
/// Cloudflare 的 speedtest 端点：`__down?bytes=N` 回 N 字节，`__up` 收任意 POST 正文
pub const SPEEDTEST_DOWN_URL: &str = "https://speed.cloudflare.com/__down?bytes=";
pub const SPEEDTEST_UP_URL: &str = "https://speed.cloudflare.com/__up";
/// 下载 4 MB / 上传 1 MB / 每 60 分钟一轮（可由 `state.residential` 的同名可选字段覆盖）。
/// 三条上游按此口径约 11 GB/月，见 spec §5.3
pub const SPEEDTEST_DOWN_BYTES: u64 = 4 * 1024 * 1024;
pub const SPEEDTEST_UP_BYTES: u64 = 1024 * 1024;
pub const SPEEDTEST_INTERVAL_MINS: i64 = 60;
/// 每成员保留的测速次数（环形），报中位数
pub const SPEEDTEST_SAMPLES_MAX: usize = 6;
/// 测速的硬超时：4 MB 在慢上游上会超过 [`PROBE_TIMEOUT_SECS`]，不能用探测的超时
pub const SPEEDTEST_TIMEOUT_SECS: u64 = 60;
/// 单次测速用量的上限：覆盖字段是人手填的，多打一个 0 就是几十 GB 住宅流量。
/// 上传还要在内存里备齐这么多随机字节，不夹住等于给自己留一个 OOM
pub const SPEEDTEST_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// UDP 探测（主理人 2026-09-12：「巡检也要测 UDP」）：经 socks5 上游做 UDP ASSOCIATE，
/// 向 Google 的公共 STUN 发一个 Binding Request，回包里的 XOR-MAPPED-ADDRESS 就是
/// 「UDP 出口 IP」。**每次探测新建关联**：实测 Decodo 的一次 UDP ASSOCIATE 只服务
/// 第一个目标地址（同 `ResidentialGroup::udp_via_pool` 的注释）
pub const STUN_HOST: &str = "stun.l.google.com";
pub const STUN_PORT: u16 = 19302;
pub const STUN_TIMEOUT_SECS: u64 = 5;
/// RFC 5389 §6 的 magic cookie
pub const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// 「更优候选」防抖（主理人 2026-09-12）：当前出口**健康**时，只有连续
/// [`SWITCH_IMPROVE_ROUNDS`] 轮满足「延迟 p50 低 ≥ [`SWITCH_LATENCY_GAIN`] 或
/// 下行快 ≥ [`SWITCH_SPEED_GAIN`]」才切。当前出口不健康 / Google 封仍立即切（既有规则）
pub const SWITCH_IMPROVE_ROUNDS: u32 = 3;
pub const SWITCH_LATENCY_GAIN: f64 = 0.20;
pub const SWITCH_SPEED_GAIN: f64 = 0.30;
/// 按槽驱动的「切回防抖」（spec §5.6）：本槽自己的 IP 连续这么多轮健康且 Google 通，
/// 才从借用状态切回本槽。**借用方向不设额外轮数**：`HealthState.active` 本身已经带
/// 2 轮迟滞（[`FAIL_TO_UNHEALTHY`]），再叠一层只会让「本槽坏了还在往坏 IP 上打」多撑几分钟。
pub const SLOT_BACK_ROUNDS: u32 = 3;
/// 迟滞：连续 2 轮不达标 → 不健康；连续 2 轮达标 → 恢复（spec §5.3）
pub const FAIL_TO_UNHEALTHY: u32 = 2;
pub const OK_TO_HEALTHY: u32 = 2;
/// 两次切换的最小间隔（spec §5.3）
pub const SWITCH_MIN_INTERVAL_SECS: i64 = 60;
/// 面板「立即巡检一轮」按钮的最小间隔（T10 的 `POST /api/residential/health/check`）：
/// 一轮巡检会推进迟滞、可能触发切换，不限速的话连点两下就能把成员判死并切走
pub const MANUAL_ROUND_MIN_GAP_SECS: i64 = 60;
/// 24h 成功率的采样窗口与每成员样本上限（同优先级时的排序依据，spec §5.3）
pub const RATE_WINDOW_SECS: i64 = 86_400;
pub const RATE_SAMPLES_MAX: usize = 1024;
/// journald 增量轮询间隔（契约决策 §E）
pub const JOURNAL_POLL_SECS: u64 = 300;
/// 候选阈值：同一 (上游, 主机, 端口) 累计被拒次数（R13 §6.2）
pub const CANDIDATE_THRESHOLD: u64 = 3;
/// 确认：两次确认之间至少间隔 10 分钟、连续 2 次（spec §5.4）
pub const CONFIRM_MIN_GAP_SECS: i64 = 600;
pub const CONFIRM_NEEDED: u32 = 2;
/// 复核：连续 3 次不再被拒 → 移除（spec §5.4）
pub const REVIEW_PASSES_TO_REMOVE: u32 = 3;
/// 每日批量窗口的小时（服务器本地时间，契约决策 §D 的 D5）
pub const DAILY_HOUR: u8 = 4;
/// 每日窗口的检查间隔：到点判定靠 `host.now().hour()`，只要比一小时细就够
pub const DAILY_TICK_SECS: u64 = 600;
/// 体检的固定端口集（spec §5.2；调研 §D：Decodo 这六个全 CONNECT 403）
pub const PROBE_PORTS: [u16; 6] = [5228, 5223, 993, 22, 8080, 853];
/// 体检必测的基准端口：它们通不通决定 `ports_allowed` 有没有意义
pub const BASE_PORTS: [u16; 2] = [80, 443];
/// 端口集探测的**中性**目标主机：经任何上游 CONNECT 到它的 80/443 都应该通。
/// **绝不能用 `www.google.com`**：Bright Data 对搜索域名整域硬拒（policy_20110），
/// 基准端口会被判成不通，于是 [`check::derive_ports_allowed`] / [`blacklist::port_learn`]
/// 一条白名单都学不出来（返回 `None` = 不限），端口策略静默失效。
pub const PORT_PROBE_HOST: &str = "www.gstatic.com";
/// 固定端口集各自的真实目标主机：必须是**真在那个端口上监听**的站点，否则上游放行了
/// 该端口、目标却不监听时上游回 502/504，会被记成「端口被拒」。
/// 没有公认公共监听点的端口（8080）打 `portquiz.net`：它在所有 TCP 端口监听，专为出站
/// 端口测试设计。打中性主机不行——`www.gstatic.com` 的 8080 没监听，恒判 unreachable，
/// 抖动保护于是让任何上游都学不出结论（2026-09-13 真机）。
pub const PORT_PROBE_HOSTS: [(u16, &str); 6] = [
    (5228, "mtalk.google.com"),       // FCM（调研 §8 的实测目标）
    (5223, "courier.push.apple.com"), // APNs（同上）
    (993, "imap.gmail.com"),          // IMAPS
    (22, "github.com"),               // SSH
    (853, "dns.google"),              // DoT
    (8080, "portquiz.net"),           // 全端口监听的出站端口测试站
];

/// 槽 `index` 的 selector tag，与 `bui_schema::render::relay` 的 `selector_tag` 同规则。
/// **唯一**一处做这个换算。
pub fn slot_selector(index: u16) -> String {
    format!("slot-{index}-pool")
}

/// 端口 → 探测目标主机。表里有就用真实主机，其余（基准 80/443）用中性主机。
pub fn port_probe_host(port: u16) -> &'static str {
    PORT_PROBE_HOSTS
        .iter()
        .find(|(p, _)| *p == port)
        .map_or(PORT_PROBE_HOST, |(_, h)| *h)
}
/// 体检的 AI 可达性目标（spec §5.2）
pub const AI_HOSTS: [&str; 3] = ["gemini.google.com", "api.openai.com", "api.anthropic.com"];
/// 体检的支付可达性目标（spec §5.2；调研 §D：Decodo 上 pay.google.com / www.paypal.com 是 403）
pub const PAY_HOSTS: [&str; 3] = ["checkout.stripe.com", "pay.google.com", "www.paypal.com"];
/// 取出口 IP 的纯文本接口（v3 `residential-helper.sh:215` 同源）
pub const EXIT_IP_URL: &str = "https://api.ipify.org";
/// [`EXIT_IP_URL`] 的主机名（同 [`HEALTH_PROBE_HOST`]，给 `confirm_auth_failure` 用）
pub const EXIT_IP_HOST: &str = "api.ipify.org";

pub struct ResidentialModule {
    prober: Arc<dyn proxy::Prober>,
    clash: Arc<dyn clash::Clash>,
    paths: Paths,
}

impl ResidentialModule {
    /// 生产构造：`ReqwestProber` + `HttpClash` + `Paths::default_server()`
    pub fn new() -> Self {
        Self::with(
            Arc::new(proxy::ReqwestProber::new()),
            Arc::new(clash::HttpClash::new()),
            Paths::default_server(),
        )
    }

    /// 测试与 M2 演练用：注入 fake
    pub fn with(
        prober: Arc<dyn proxy::Prober>,
        clash: Arc<dyn clash::Clash>,
        paths: Paths,
    ) -> Self {
        Self {
            prober,
            clash,
            paths,
        }
    }
}

impl Default for ResidentialModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ResidentialModule {
    fn name(&self) -> &'static str {
        "residential"
    }

    /// **空**：`singbox-relay.json` 归 `core_files`（P1 Task 10）。这里若产出同路径的
    /// artifact，`reconcile::diff::plan` 会拿到两个同路径项，写盘与重启取决于模块注册
    /// 顺序。改住宅状态请走 `state::update_group`。
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn routes(&self) -> axum::Router<crate::api::AppState> {
        api::routes(self.prober.clone(), self.clash.clone(), self.paths.clone())
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        let (p, c) = (self.prober.clone(), self.clash.clone());
        // **先 subscribe 再 spawn**：broadcast 丢弃「发送时还没有订阅者」的事件，
        // 若让 replay_loop 自己 subscribe，紧随 spawn 的第一次对账重启就可能漏掉。
        let rx = ctx.bus.subscribe();
        vec![
            // spec §5.3：每 2 分钟一轮巡检 + 切换
            tokio::spawn(health::health_loop(ctx.clone(), p.clone(), c.clone())),
            // spec §5.3 最后一句：relay 任何重启后立即重放 runtime.selected_upstream_id
            tokio::spawn(health::replay_loop(ctx.clone(), c.clone(), rx)),
            // spec §5.4 (a)：跟随 relay 日志学候选（契约决策 §E：游标增量，不用 -f），
            // 同一轮里紧接着做一次确认 ⇒ ≥10 分钟就能进 pending（裁决「黑名单确认节奏」）
            tokio::spawn(blacklist::journal_loop(ctx.clone(), p.clone(), c.clone())),
            // spec §5.4：每日 04:00 批量生效 + 每日探针/端口集 + 复核移除（不做确认）
            tokio::spawn(blacklist::daily_loop(ctx, p, c)),
        ]
    }
}

/// 把一批同步探测并发跑完：并发上限 [`PROBE_CONCURRENCY`]，每个闭包在
/// `tokio::task::spawn_blocking` 里执行（`reqwest::blocking` 在 async 上下文会 panic，
/// 与 P1 的 `Fetcher` 同一条铁律）。返回长度与输入**严格相等**、顺序与输入一致；
/// `None` = 该项的阻塞任务 panic 了（调用方记一条 alert，不让一个坏上游带崩整轮巡检）。
pub async fn fanout<I, R, F>(items: Vec<I>, f: F) -> Vec<Option<R>>
where
    I: Send + 'static,
    R: Send + 'static,
    F: Fn(I) -> R + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let sem = Arc::new(tokio::sync::Semaphore::new(PROBE_CONCURRENCY));
    let total = items.len();
    let mut set = tokio::task::JoinSet::new();
    for (idx, item) in items.into_iter().enumerate() {
        let (f, sem) = (f.clone(), sem.clone());
        set.spawn(async move {
            // Semaphore 只在本函数内持有、永不 close，acquire 不会失败
            let _permit = sem.acquire_owned().await.expect("semaphore 未关闭");
            let r = tokio::task::spawn_blocking(move || f(item)).await;
            (idx, r.ok())
        });
    }
    let mut out: Vec<Option<R>> = (0..total).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        // 外层 async 任务只 await，不会 panic；真正可能 panic 的是 spawn_blocking 里的闭包
        if let Ok((idx, r)) = joined {
            out[idx] = r;
        }
    }
    out
}

/// 测试夹具：住宅段的期望态样例（合成值，无生产秘密）。字段取值与 `bui-schema`
/// `model.rs` 的 SAMPLE 一致，便于两边的断言互相印证。
#[cfg(test)]
pub fn sample_group() -> bui_schema::model::ResidentialGroup {
    use bui_schema::model::{
        AutoEntry, Blacklist, Pin, ResiMode, ResidentialGroup, Rule, Upstream, UpstreamKind,
        Verified,
    };
    let id = uuid::Uuid::from_u128(0x8d5a1a1e_3b2c_4d1e_9f00_0000000000bb);
    ResidentialGroup {
        enabled: true,
        mode: ResiMode::Global,
        keywords: None,
        upstreams: vec![Upstream {
            id,
            name: "url-1".into(),
            kind: UpstreamKind::Http,
            host: "isp.example.net".into(),
            port: 10007,
            username: "u".into(),
            password: "p".into(),
            priority: 10,
            provider: Some("decodo".into()),
            region: Some("US".into()),
            ports_allowed: Some(vec![80, 443]),
            verified: Some(Verified {
                ip: "198.51.100.7".into(),
                asn: Some(33667),
                org: Some("Comcast".into()),
                country: Some("US".into()),
                at: "2026-09-11T00:00:00Z".into(),
            }),
        }],
        selected_upstream_id: Some(id),
        blacklist: Blacklist {
            pins: vec![Pin {
                rule: Rule::DomainSuffix("pay.google.com".into()),
                note: String::new(),
                created_at: "2026-09-11T00:00:00Z".into(),
            }],
            auto: vec![AutoEntry {
                upstream_id: id,
                rule: Rule::Port(5228),
                hits: 5,
                confirmed_at: "2026-09-11T00:00:00Z".into(),
                last_verified_at: "2026-09-11T00:00:00Z".into(),
                passes: 0,
            }],
        },
    }
}

/// `crate::testutil::sample_state()`，但 `residential.groups["default"]` 换成 [`sample_group`]
#[cfg(test)]
pub fn sample_state_with_pool() -> State {
    let mut s = crate::testutil::sample_state();
    s.residential
        .groups
        .insert(GROUP_DEFAULT.to_string(), sample_group());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Facts, RenderCtx};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ctx() -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 1,
                systemd_resolved: false,
                nft_tables: Default::default(),
            },
        }
    }

    #[test]
    fn render_is_empty_because_core_files_owns_the_relay_config() {
        // 锁住契约决策 §B：singbox-relay.json 只由 P1 Task 10 产出。一旦有人在这里补一个
        // Artifact::File，同路径两个 artifact 的 diff 就会随模块注册顺序变脸。
        let arts = ResidentialModule::new().render(&sample_state(), &ctx());
        assert!(
            arts.is_empty(),
            "P3 不产出任何 artifact，实际 {}",
            arts.len()
        );
        assert_eq!(ResidentialModule::new().name(), "residential");
    }

    #[test]
    fn constants_match_the_spec() {
        assert_eq!(GROUP_DEFAULT, "default");
        assert_eq!(POOL, "resi-pool");
        assert_eq!(CLASH_API, "127.0.0.1:9091");
        assert_eq!((HEALTH_INTERVAL_SECS, HEALTH_TRIES), (120, 2));
        assert_eq!((FAIL_TO_UNHEALTHY, OK_TO_HEALTHY), (2, 2));
        assert_eq!(
            (SWITCH_MIN_INTERVAL_SECS, MANUAL_ROUND_MIN_GAP_SECS),
            (60, 60)
        );
        assert_eq!((PROBE_TIMEOUT_SECS, PROBE_CONCURRENCY), (10, 4));
        assert_eq!((CONFIRM_MIN_GAP_SECS, CONFIRM_NEEDED), (600, 2));
        assert_eq!(REVIEW_PASSES_TO_REMOVE, 3);
        assert_eq!(
            DAILY_HOUR, 4,
            "spec §5.4 的 04:00 是服务器本地时间（决策 D5）"
        );
        assert_eq!(PROBE_PORTS, [5228, 5223, 993, 22, 8080, 853]);
        assert_eq!(AI_HOSTS[2], "api.anthropic.com");
        // 基准端口打中性主机：Bright Data 整域硬拒 www.google.com，用它当基准会让
        // ports_allowed 永远学不出来（见 PORT_PROBE_HOST 的注释）
        for p in BASE_PORTS {
            assert_eq!(port_probe_host(p), "www.gstatic.com", "基准端口 {p}");
        }
        assert_ne!(
            port_probe_host(443),
            "www.google.com",
            "基准主机绝不能是搜索域名"
        );
        // 固定端口集打各自真实监听的主机
        assert_eq!(port_probe_host(5228), "mtalk.google.com");
        assert_eq!(port_probe_host(5223), "courier.push.apple.com");
        assert_eq!(port_probe_host(993), "imap.gmail.com");
        assert_eq!(port_probe_host(22), "github.com");
        assert_eq!(port_probe_host(853), "dns.google");
        // 8080 没有公认的公共监听点：打中性主机会恒 unreachable（2026-09-13 真机），
        // 抖动保护于是让任何上游都学不出结论。portquiz.net 在所有 TCP 端口监听
        assert_eq!(port_probe_host(8080), "portquiz.net");
        assert_eq!(PAY_HOSTS[1], "pay.google.com");
        // 这两个 host 必须与对应 URL 的主机名严格一致：CONNECT 补判（T3
        // confirm_auth_failure）拿它们去开隧道，写歪了就补判到别的站点上去了
        assert!(HEALTH_PROBE_URL.starts_with(&format!("https://{HEALTH_PROBE_HOST}/")));
        assert_eq!(EXIT_IP_URL, format!("https://{EXIT_IP_HOST}"));
        // R2 ②：打真实搜索路径（首页看不出 serp 整域硬拒），UA 得像浏览器
        assert_eq!(GOOGLE_PROBE_URL, "https://www.google.com/search?q=weather");
        assert!(GOOGLE_PROBE_UA.starts_with("Mozilla/5.0 "));
        // 巡检每轮每成员多这一次请求，所以超时要比普通探测短
        assert_eq!(
            (GOOGLE_PROBE_TIMEOUT_SECS, PROBE_TIMEOUT_SECS),
            (8, 10),
            "Google 探测的超时必须短于普通探测"
        );
    }

    #[test]
    fn the_shared_fixture_is_a_one_upstream_pool_with_one_pin_and_one_auto_entry() {
        // T2 的 health_summary 与 T10 的 harness 全吃这份夹具。P1 的
        // crate::testutil::sample_state() 住宅段是空池，直接用会让那些断言与
        // `upstreams[0]` 索引全部崩掉——这就是本夹具存在的唯一理由。
        let g = sample_group();
        assert!(g.enabled);
        assert_eq!(g.mode, bui_schema::model::ResiMode::Global);
        assert_eq!(g.upstreams.len(), 1);
        assert_eq!(g.upstreams[0].host, "isp.example.net");
        assert_eq!(g.upstreams[0].port, 10007);
        assert_eq!(g.upstreams[0].username, "u");
        assert_eq!(g.upstreams[0].kind, bui_schema::model::UpstreamKind::Http);
        assert_eq!(g.upstreams[0].name, "url-1");
        assert_eq!(g.upstreams[0].verified.as_ref().unwrap().ip, "198.51.100.7");
        assert_eq!(g.selected_upstream_id, Some(g.upstreams[0].id));
        assert_eq!(g.blacklist.pins.len(), 1);
        assert_eq!(g.blacklist.auto.len(), 1);
        assert!(g.pool_active());
        let s = sample_state_with_pool();
        assert_eq!(s.residential.groups[GROUP_DEFAULT], g);
        assert_eq!(
            s.node.public_ip, "203.0.113.10",
            "T7/T10 的「出口 IP == 本机」判据吃它"
        );
    }

    #[tokio::test]
    async fn fanout_preserves_order_and_caps_concurrency_at_four() {
        let live = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let (l, p) = (live.clone(), peak.clone());
        let out = fanout((0..20u32).collect::<Vec<_>>(), move |i| {
            let n = l.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            l.fetch_sub(1, Ordering::SeqCst);
            i * 2
        })
        .await;
        assert_eq!(out.len(), 20);
        assert_eq!(out[3], Some(6), "输出顺序与输入一致");
        assert_eq!(out[19], Some(38));
        assert!(
            peak.load(Ordering::SeqCst) <= PROBE_CONCURRENCY,
            "并发峰值 {} 超过上限",
            peak.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn fanout_reports_a_panicking_item_as_none_without_losing_the_others() {
        let out = fanout(vec![1u32, 2, 3], |i| {
            assert_ne!(i, 2, "这一项的探测炸了");
            i * 10
        })
        .await;
        assert_eq!(out, vec![Some(10), None, Some(30)]);
    }

    #[tokio::test]
    async fn the_module_exposes_routes_and_four_background_tasks() {
        use crate::api::EventBus;
        use crate::modules::residential::clash::FakeClash;
        use crate::modules::residential::proxy::FakeProber;
        use crate::state::runtime::Runtime;
        use crate::state::store::Store;
        use crate::sys::fake::FakeHost;
        let d = tempfile::tempdir().unwrap();
        let host = std::sync::Arc::new(FakeHost::new());
        let ctx = crate::reconcile::DaemonCtx {
            store: Store::create(d.path().join("state.json"), sample_state())
                .await
                .unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host,
            paths: Paths::default_server(),
        };
        let m = ResidentialModule::with(
            std::sync::Arc::new(FakeProber::new()),
            std::sync::Arc::new(FakeClash::new(Some("resi-1"))),
            Paths::default_server(),
        );
        let handles = m.spawn(ctx);
        assert_eq!(handles.len(), 4, "巡检 / 重放 / 日志学习 / 每日批量");
        for h in handles {
            h.abort();
        }
        // routes() 能被 P1 的 router 形状接住（类型对齐即编译通过）
        let _router: axum::Router<crate::api::AppState> = m.routes();
    }

    #[tokio::test]
    async fn a_state_change_reaches_the_relay_render_through_p1() {
        // 端到端：P3 改 state → P1 的 core_files 渲染出的 relay 配置随之变化。
        // 这条锁住契约决策 §B：P3 不渲染，但它的改动必须真的落到 relay 配置里。
        use crate::modules::core_files::CoreFilesModule;
        use crate::modules::residential::state as rstate;
        use crate::reconcile::Artifact;
        // 用带池的夹具：空池时 relay 渲染的是 fail-open 直连形态，加 pin 也看不出差别
        let mut s = sample_state_with_pool();
        let render = |s: &State| -> String {
            CoreFilesModule::new(None)
                .render(s, &ctx())
                .into_iter()
                .find_map(|a| match a {
                    Artifact::File { path, content, .. }
                        if path.ends_with("singbox-relay.json") =>
                    {
                        Some(String::from_utf8(content).unwrap())
                    }
                    _ => None,
                })
                .expect("core_files 必须渲染 singbox-relay.json")
        };
        let before = render(&s);
        assert!(!before.contains("www.paypal.com"));
        // 等价于 blacklist::add_pin 对 state 的那一步
        let g = s.residential.groups.get_mut(GROUP_DEFAULT).unwrap();
        g.blacklist.pins.push(bui_schema::model::Pin {
            rule: bui_schema::model::Rule::DomainSuffix("www.paypal.com".into()),
            note: String::new(),
            created_at: "2026-09-12T00:00:00Z".into(),
        });
        let after = render(&s);
        assert!(
            after.contains("www.paypal.com"),
            "pin 必须出现在 relay 的 domain_suffix → direct 规则里"
        );
        assert_ne!(
            before, after,
            "内容变了 ⇒ P1 的 diff 会写盘并重启 b-ui-relay"
        );
        assert_eq!(rstate::group_of(&s).blacklist.pins.len(), 2);
    }
}
