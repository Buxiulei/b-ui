//! `b-ui-relay`（本地中继 sing-box）出站失败行的日志原文夹具。
//!
//! **不许凭记忆编**：改动这里等于改哨兵判据与黑名单学习的判据，要重采。
//!
//! 出处（两处，逐条标在常量头上）：
//!
//! 1. **2026-09-18 调研采样**（`docs` 外的调研件 `relay-blind/RESEARCH.md` §2）：两台生产机
//!    （SSH 别名 `bwg-rick` / `bwg-tizi`）各取 14 天窗口的 `journalctl -u b-ui-relay -o cat`
//!    里全部 ERROR/WARN 行，按 `using outbound/<kind>[<tag>]: <reason>` 归并去重。落这里的是
//!    **归并后每一类形状各一条**，`<reason>` 逐字原文。采样命令把行首前缀 `grep -oE` 掉了，
//!    所以本文件的行首前缀沿用 R13 §6.2 那条真机原文的形态（`ERROR[…] […] connection: `，
//!    与 `signature.rs` 既有用例同源）。
//! 2. **R13 §6.2 真机原文**（`bwg-tizi` 2026-09-11，成员出站形状）：`MEMBER_*` 那几条。
//!    当前渲染器已不再产生这种形状（见下），保留是为了钉住「两种形状都认」。
//!
//! **脱敏**：上游主机名 → `isp.example.net`、上游/目标 IP → `203.0.113.x`、
//! 目标域名 → `www.example.com`（Google 那两条保留真实的 `www.google.com`：判据按它分类）。
//!
//! **关键点（RESEARCH §3，sing-box `route/conn.go` + `protocol/group/{selector,urltest}.go`）**：
//! 路由规则的 `outbound` 指向一个 group（4.1 的 relay 只有这一种：每槽 `slot-<i>-pool`、
//! DNS 与 global 模式 `resi-pool`）时，group 的 `NewConnection` 把**自己**当 dialer 传给
//! `E.Cause(err, "open connection to ", …)`，于是 ERROR 行里只有 group 的 `Type()`/`Tag()`，
//! **选中的成员 tag 一个字都不出现**。两台真机 2026-09-15 之后再没打过一条成员形状的行。
//! 所以「哪个上游」只能由调用方归因（`dial tcp` 里的上游地址，或 Clash API 的 `now`）。

/// 行首前缀：R13 §6.2 真机原文的形态（色码已剥，journald 那一步剥）
macro_rules! line {
    ($tail:expr) => {
        concat!(
            "ERROR[4006] [2302991392 6.42s] connection: open connection to ",
            $tail
        )
    };
}

// ───────────────────────── 池形状（当前唯一会出现的形状） ─────────────────────────

/// rick/tizi 两台合计 45579 条、**样本里量最大的一类**：全局池 `urltest[resi-pool]` 上的
/// SOCKS5 REP 拒绝。REP=2 ⇒ 黑名单候选（目标级），哨兵不碰
pub const POOL_URLTEST_SOCKS_CODE2: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: socks5: request rejected, code=2"
);

/// 同上，REP=4（主机不可达）：目标/网络的问题，**不学**进黑名单
pub const POOL_URLTEST_SOCKS_CODE4: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: socks5: request rejected, code=4"
);

/// 每槽 selector 上的 SOCKS5 REP=2（rick 741 条 / tizi 241 条）：4.1 新拓扑的主形状
pub const POOL_SELECTOR_SOCKS_CODE2: &str = line!(
    "www.example.com:443 using outbound/selector[slot-3-pool]: socks5: request rejected, code=2"
);

/// 每槽 selector 上的 SOCKS5 凭据被拒（tizi 10 条）⇒ 整条上游不能用
pub const POOL_SELECTOR_SOCKS_AUTH: &str = line!(
    "www.example.com:443 using outbound/selector[slot-0-pool]: socks5: incorrect user name or password"
);

/// http 上游的鉴权失败形状（rick 2650 条，成员形状采到；这里是它的池形态）。
/// **`AUTH_MARKERS` 原先没有这一条**，加它是本次修法的一部分
pub const POOL_SELECTOR_AUTH_REQUIRED: &str =
    line!("www.example.com:443 using outbound/selector[slot-1-pool]: authentication required");

/// `context canceled`（tizi 7 条）：客户端自己断的，既不是上游故障也不是拒绝 ⇒ 不归类
pub const POOL_SELECTOR_CONTEXT_CANCELED: &str =
    line!("www.example.com:443 using outbound/selector[slot-2-pool]: context canceled");

/// `context deadline exceeded`：TCP 已连上、握手阶段超时，reason **不带上游地址**
/// （路径 A 归不了因，只能靠 Clash `now`）
pub const POOL_SELECTOR_DEADLINE: &str =
    line!("www.example.com:443 using outbound/selector[slot-4-pool]: context deadline exceeded");

/// [`POOL_SELECTOR_DEADLINE`] 的 `slot-1-pool` 变体：`pool_ctx` 的假机器只有 0/1/2 三个槽，
/// 哨兵的归因用例要打在真存在的槽上
pub const POOL_SELECTOR_DEADLINE_SLOT1: &str =
    line!("www.example.com:443 using outbound/selector[slot-1-pool]: context deadline exceeded");

/// `unexpected EOF`（rick 17217 条，成员形状采到；这里是池形态）。语义含糊——可能是上游
/// 掐了连接、也可能是目标掐的——**不归类**（既不进哨兵也不进黑名单），本常量就是钉这件事的
pub const POOL_SELECTOR_UNEXPECTED_EOF: &str =
    line!("www.example.com:443 using outbound/selector[slot-5-pool]: unexpected EOF");

/// 上游 CONNECT 回 407 ⇒ 凭据失效（整条上游不能用）
pub const POOL_SELECTOR_407: &str = line!(
    "www.example.com:443 using outbound/selector[slot-6-pool]: unexpected status: 407 Proxy Authentication Required"
);

/// Bright Data 的 SERP 整域拒绝：`403 … serp …` ⇒ [`Sig::RelayGoogleBlocked`]
///
/// [`Sig::RelayGoogleBlocked`]: crate::modules::sentinel::signature::Sig::RelayGoogleBlocked
pub const POOL_SELECTOR_403_SERP: &str = line!(
    "www.google.com:443 using outbound/selector[slot-7-pool]: unexpected status: 403 Forbidden serp domain"
);

/// 其余 4xx：上游拒绝了**这个目标** ⇒ 黑名单地盘，哨兵不碰
pub const POOL_SELECTOR_403: &str = line!(
    "www.example.com:443 using outbound/selector[slot-1-pool]: unexpected status: 403 Forbidden"
);

/// 5xx 变体（rick 39 条 / tizi 91 条的 server_error 一类）：同样是目标级
pub const POOL_SELECTOR_502: &str = line!(
    "www.example.com:443 using outbound/selector[slot-1-pool]: unexpected status: 502 Bad Gateway"
);

/// TCP 拨号被拒：reason 里带着**上游自己的 host:port** ⇒ 路径 A（零 I/O）能归因
pub const POOL_SELECTOR_DIAL_REFUSED: &str = line!(
    "www.example.com:443 using outbound/selector[slot-2-pool]: dial tcp 203.0.113.7:10007: connect: connection refused"
);

/// TCP 拨号超时（同样带上游地址）
pub const POOL_URLTEST_DIAL_TIMEOUT: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: dial tcp 203.0.113.7:10007: i/o timeout"
);

/// 上游主机名解析不出来：`dial tcp: lookup …`，**没有** `host:port` ⇒ 路径 A 归不了因
pub const POOL_SELECTOR_DIAL_NO_ROUTE: &str = line!(
    "www.example.com:443 using outbound/selector[slot-0-pool]: dial tcp: lookup isp.example.net: no route to host"
);

/// 目标是裸 IP 的池形态（黑名单侧要靠它验证「裸 IP 不进候选」仍然成立）
pub const POOL_SELECTOR_IP_TARGET_403: &str = line!(
    "203.0.113.9:5228 using outbound/selector[slot-1-pool]: unexpected status: 403 Forbidden"
);

/// 不是住宅池的 group tag（槽序号越界）：白名单必须把它挡掉，否则任何 group 都能冒充住宅池
pub const POOL_TAG_OUT_OF_RANGE: &str = line!(
    "www.example.com:443 using outbound/selector[slot-9-pool]: socks5: request rejected, code=2"
);

/// 住宅 HY2 的门 selector（`hysteria-residential` 那份 sing-box 的东西，relay 日志里不该有）：
/// tag 不是已知池名 ⇒ 一律不认
pub const GATE_SELECTOR: &str = line!(
    "www.example.com:443 using outbound/selector[gate-7f3a]: dial tcp 127.0.0.1:2081: connect: connection refused"
);

/// `direct` 出站与住宅无关
pub const DIRECT_DIAL_TIMEOUT: &str = line!(
    "www.example.com:443 using outbound/direct[direct]: dial tcp 203.0.113.9:443: i/o timeout"
);

// ───────────────────────── 成员形状（历史，仍双认） ─────────────────────────

/// R13 §6.2：成员出站的 TCP 拨号被拒
pub const MEMBER_DIAL_REFUSED: &str = line!(
    "www.example.com:443 using outbound/socks[resi-2]: dial tcp 203.0.113.7:10007: connect: connection refused"
);

/// R13 §6.2：成员出站的握手超时
pub const MEMBER_DEADLINE: &str =
    line!("www.example.com:443 using outbound/http[resi-3]: context deadline exceeded");

/// R13 §6.2：成员出站的 407
pub const MEMBER_407: &str = line!(
    "www.example.com:443 using outbound/http[resi-1]: unexpected status: 407 Proxy Authentication Required"
);

/// R13 §6.2：成员出站的 SOCKS5 REP=2（黑名单候选）
pub const MEMBER_SOCKS_CODE2: &str =
    line!("www.example.com:443 using outbound/socks[resi-1]: socks5: request rejected, code=2");

/// 2026-09-18 采样：成员出站的 `authentication required`（rick 2650 条的原形状）
pub const MEMBER_AUTH_REQUIRED: &str =
    line!("www.example.com:443 using outbound/http[resi-1]: authentication required");

/// 2026-09-18 采样：成员出站的 `unexpected EOF`（rick 17217 条的原形状）⇒ 不归类
pub const MEMBER_UNEXPECTED_EOF: &str =
    line!("www.example.com:443 using outbound/http[resi-1]: unexpected EOF");
