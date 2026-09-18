//! `b-ui-relay`（本地中继 sing-box）出站失败行的日志原文夹具。
//!
//! **不许凭记忆编**：改动这里等于改哨兵判据与黑名单学习的判据，要重采。
//!
//! 每条常量头上都标着出处，**只有三档，别把后两档当真机原文用**：
//!
//! - **【采样】**：2026-09-18 两台生产机（SSH 别名 `bwg-rick` / `bwg-tizi`）各取 14 天窗口的
//!   `journalctl -u b-ui-relay -o cat`，ERROR/WARN 行按 `using outbound/<kind>[<tag>]: <reason>`
//!   归并去重（调研件 `relay-blind/RESEARCH.md` §2）。`<reason>` 逐字，只按下面的规则脱敏。
//! - **【转写】**：形状的前半截采到了、**尾巴没有**。§2 的采样命令用 `sed` 把
//!   `unexpected status: <三位码><后续>` 整段折叠成了 `unexpected status: <code> ...`，所以凡是带
//!   具体状态码 / 状态短语的常量，尾巴都**不是**采样原文，沿用 6e3f914 既有用例（或另行标注的
//!   设计文档）的文本；「成员形状采到、池形态按 §3 推」的那几条也在这一档。
//! - **【合成】**：采样里根本没出现过的形状，或有意造的负例（tag 越界、门 selector…）。§3 的
//!   结论（group 只换 tag、不换 reason）保证这种拼法在语法上成立，但**没有真机证据**。
//!
//! 采样命令的 `grep -oE 'using outbound/…'` 把行首前缀切掉了，所以本文件所有常量的行首前缀统一
//! 用 R13 §6.2 那条真机原文的形态（`ERROR[…] […] connection: `，与 `signature.rs` 既有用例同源）
//! ——**只有前缀这一段**能追到 R13 §6.2，`<reason>` 的出处各看各的标注。
//!
//! **脱敏**：上游主机名 → `isp.example.net`、上游/目标 IP → `203.0.113.x`、
//! 目标域名 → `www.example.com`（Google 那条保留真实的 `www.google.com`：判据按它分类）。
//! `dial tcp <ip>:<port>` 的地址在采样命令里就被 `sed` 归一掉了，本文件里的地址一律是脱敏值。
//! **槽序号也一样**：§2 的归并把 `slot-<i>-pool` 归一成了 `slot-N-pool`，本文件里每条常量上
//! 的那个序号是**为用例挑的**（各用例要打在假机器真存在的槽上），不是采到那条行时的真实槽号。
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

/// 【采样】rick/tizi 两台合计 45579 条、**样本里量最大的一类**：全局池 `urltest[resi-pool]` 上的
/// SOCKS5 REP 拒绝。REP=2 ⇒ 黑名单候选（目标级），哨兵不碰。
/// （§2 的汇总把 REP 值写成 `code=<n>`，`code=2` 是量最大那一类，由 §4 点名）
pub const POOL_URLTEST_SOCKS_CODE2: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: socks5: request rejected, code=2"
);

/// 【合成】REP=4（主机不可达）的变体：§2 的汇总没拆 REP 值，这条是按 SOCKS5 REP 语义造的负例
/// ——目标/网络的问题，**不学**进黑名单
pub const POOL_URLTEST_SOCKS_CODE4: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: socks5: request rejected, code=4"
);

/// 【采样】每槽 selector 上的 SOCKS5 REP 拒绝（rick 741 条 / tizi 241 条）：4.1 新拓扑的主形状。
/// REP 值同 [`POOL_URLTEST_SOCKS_CODE2`]
pub const POOL_SELECTOR_SOCKS_CODE2: &str = line!(
    "www.example.com:443 using outbound/selector[slot-3-pool]: socks5: request rejected, code=2"
);

/// 【采样】每槽 selector 上的 SOCKS5 凭据被拒（tizi 10 条），逐字 ⇒ 整条上游不能用。
/// 池形状里**唯一**采到原文的「上游本身坏了」形状，哨兵归因层的端到端用例打的就是它
pub const POOL_SELECTOR_SOCKS_AUTH: &str = line!(
    "www.example.com:443 using outbound/selector[slot-0-pool]: socks5: incorrect user name or password"
);

/// 【转写】http 上游的鉴权失败：**成员形状**采到（rick 2650 条，见
/// [`MEMBER_AUTH_REQUIRED`]），这里是按 §3 推出来的池形态。
/// `AUTH_MARKERS` 原先没有这一条，加它是本次修法的一部分
pub const POOL_SELECTOR_AUTH_REQUIRED: &str =
    line!("www.example.com:443 using outbound/selector[slot-1-pool]: authentication required");

/// 【采样】`context canceled`（tizi 7 条）：客户端自己断的，既不是上游故障也不是拒绝 ⇒ 不归类
pub const POOL_SELECTOR_CONTEXT_CANCELED: &str =
    line!("www.example.com:443 using outbound/selector[slot-2-pool]: context canceled");

/// 【合成】`context deadline exceeded`：**§2 两台 14 天窗口里一条都没有**（成员形状也没有）。
/// reason 文本是 `UNREACHABLE_MARKERS` 里 R13 时代就有的判据，池形态按 §3 拼出来的。
///
/// 它在 `run.rs` 里当归因层的测试向量用——归因层只看 [`RelaySubject`]、与 reason 无关，所以
/// 用哪条形状都一样；**采样原文**的端到端用例走 [`POOL_SELECTOR_SOCKS_AUTH`]。
///
/// [`RelaySubject`]: crate::modules::residential::journal::RelaySubject
pub const POOL_SELECTOR_DEADLINE: &str =
    line!("www.example.com:443 using outbound/selector[slot-4-pool]: context deadline exceeded");

/// 【合成】[`POOL_SELECTOR_DEADLINE`] 的 `slot-1-pool` 变体：`pool_ctx` 的假机器只有 0/1/2
/// 三个槽，哨兵的归因用例要打在真存在的槽上
pub const POOL_SELECTOR_DEADLINE_SLOT1: &str =
    line!("www.example.com:443 using outbound/selector[slot-1-pool]: context deadline exceeded");

/// 【转写】`unexpected EOF`：**成员形状**采到（rick 17217 条，见 [`MEMBER_UNEXPECTED_EOF`]），
/// 这里是按 §3 推的池形态。语义含糊——可能是上游掐了连接、也可能是目标掐的——**不归类**
///（既不进哨兵也不进黑名单），本常量就是钉这件事的
pub const POOL_SELECTOR_UNEXPECTED_EOF: &str =
    line!("www.example.com:443 using outbound/selector[slot-5-pool]: unexpected EOF");

/// 【转写】上游 CONNECT 回 407 ⇒ 凭据失效（整条上游不能用）。
/// 两处转写：① `unexpected status:` 这个前缀只在**成员**形状采到（rick 5030 / tizi 3589），
/// 池形态按 §3 推；② §2 的 `sed` 把状态码之后整段折叠成了 `<code> ...`，
/// `407 Proxy Authentication Required` 这句沿用 6e3f914 既有用例的文本
pub const POOL_SELECTOR_407: &str = line!(
    "www.example.com:443 using outbound/selector[slot-6-pool]: unexpected status: 407 Proxy Authentication Required"
);

/// 【转写】Bright Data 的 SERP 整域拒绝：`403 … serp …` ⇒ [`Sig::RelayGoogleBlocked`]。
/// 转写同 [`POOL_SELECTOR_407`]；另外 `403 Forbidden serp domain` 这句的出处是
/// `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md` 里记的
/// Bright Data CONNECT 状态行，**不是** relay 的 journald 原文
///
/// [`Sig::RelayGoogleBlocked`]: crate::modules::sentinel::signature::Sig::RelayGoogleBlocked
pub const POOL_SELECTOR_403_SERP: &str = line!(
    "www.google.com:443 using outbound/selector[slot-7-pool]: unexpected status: 403 Forbidden serp domain"
);

/// 【转写】其余 4xx：上游拒绝了**这个目标** ⇒ 黑名单地盘，哨兵不碰。转写同 [`POOL_SELECTOR_407`]
pub const POOL_SELECTOR_403: &str = line!(
    "www.example.com:443 using outbound/selector[slot-1-pool]: unexpected status: 403 Forbidden"
);

/// 【转写】5xx 变体（rick 39 条 / tizi 91 条的 server_error 一类，采样里同样被折叠成
/// `<code> ...`）：同样是目标级
pub const POOL_SELECTOR_502: &str = line!(
    "www.example.com:443 using outbound/selector[slot-1-pool]: unexpected status: 502 Bad Gateway"
);

/// 【转写】TCP 拨号被拒：reason 里带着**上游自己的 host:port** ⇒ 路径 A（零 I/O）能归因。
/// §2 采到的 dial 变体只列到 `i/o timeout` / `EOF` / `context canceled`，
/// `connect: connection refused` 这个尾巴沿用 6e3f914 既有用例
pub const POOL_SELECTOR_DIAL_REFUSED: &str = line!(
    "www.example.com:443 using outbound/selector[slot-2-pool]: dial tcp 203.0.113.7:10007: connect: connection refused"
);

/// 【转写】TCP 拨号超时：`urltest[resi-pool]` 上有 dial 变体（tizi）、`dial tcp <ip>:<port>:
/// i/o timeout` 这个尾巴也采到了（rick），但**两者不在同一条采样行里** —— 本常量是把 rick 的
/// 尾巴接到 tizi 的池 tag 上拼出来的，地址那一段还被 §2 的 `sed` 归一过。§3 的结论（group 只
/// 换 tag、不换 reason）保证这种拼法成立，但它不是哪一条采样行的逐字原文
pub const POOL_URLTEST_DIAL_TIMEOUT: &str = line!(
    "www.example.com:443 using outbound/urltest[resi-pool]: dial tcp 203.0.113.7:10007: i/o timeout"
);

/// 【合成】上游主机名解析不出来：`dial tcp: lookup …`，**没有** `host:port` ⇒ 路径 A 归不了因。
/// §2 里没有这个形状，造它是为了钉住「没有 host:port 就别猜」
pub const POOL_SELECTOR_DIAL_NO_ROUTE: &str = line!(
    "www.example.com:443 using outbound/selector[slot-0-pool]: dial tcp: lookup isp.example.net: no route to host"
);

/// 【转写】目标是裸 IP 的池形态（黑名单侧要靠它验证「裸 IP 不进候选」仍然成立）。
/// 状态码尾部同 [`POOL_SELECTOR_407`]
pub const POOL_SELECTOR_IP_TARGET_403: &str = line!(
    "203.0.113.9:5228 using outbound/selector[slot-1-pool]: unexpected status: 403 Forbidden"
);

/// 【合成】负例：不是住宅池的 group tag（槽序号越界）。白名单必须把它挡掉，否则任何 group
/// 都能冒充住宅池
pub const POOL_TAG_OUT_OF_RANGE: &str = line!(
    "www.example.com:443 using outbound/selector[slot-9-pool]: socks5: request rejected, code=2"
);

/// 【合成】负例：住宅 HY2 的门 selector（`hysteria-residential` 那份 sing-box 的东西，
/// §2 在 relay 日志里采到 0 条）。tag 不是已知池名 ⇒ 一律不认
pub const GATE_SELECTOR: &str = line!(
    "www.example.com:443 using outbound/selector[gate-7f3a]: dial tcp 127.0.0.1:2081: connect: connection refused"
);

/// 【采样】`direct` 出站与住宅无关（rick 4 条）
pub const DIRECT_DIAL_TIMEOUT: &str = line!(
    "www.example.com:443 using outbound/direct[direct]: dial tcp 203.0.113.9:443: i/o timeout"
);

// ───────────────────────── 成员形状（历史，仍双认） ─────────────────────────

/// 【转写】成员出站的 TCP 拨号被拒：文本沿用 6e3f914 既有用例（前缀出自 R13 §6.2），
/// §2 采到的 dial 变体没有 `connect: connection refused` 这一条
pub const MEMBER_DIAL_REFUSED: &str = line!(
    "www.example.com:443 using outbound/socks[resi-2]: dial tcp 203.0.113.7:10007: connect: connection refused"
);

/// 【合成】成员出站的握手超时：`context deadline exceeded` **不在 §2 的任何一档**，
/// 文本沿用 6e3f914 既有用例（R13 时代就在 `UNREACHABLE_MARKERS` 里的判据）
pub const MEMBER_DEADLINE: &str =
    line!("www.example.com:443 using outbound/http[resi-3]: context deadline exceeded");

/// 【转写】成员出站的 407：前缀 `unexpected status:` 采到（rick 5030 / tizi 3589），
/// 状态码之后那截被 `sed` 折叠 ⇒ 沿用 6e3f914 既有用例
pub const MEMBER_407: &str = line!(
    "www.example.com:443 using outbound/http[resi-1]: unexpected status: 407 Proxy Authentication Required"
);

/// 【采样】成员出站的 SOCKS5 REP 拒绝（rick 2719 条 / tizi 290 条）⇒ 黑名单候选。
/// REP 值同 [`POOL_URLTEST_SOCKS_CODE2`]
pub const MEMBER_SOCKS_CODE2: &str =
    line!("www.example.com:443 using outbound/socks[resi-1]: socks5: request rejected, code=2");

/// 【采样】成员出站的 SOCKS5 凭据被拒（rick 308 条），逐字 ⇒ 凭据失效，不是目标级拒绝
pub const MEMBER_SOCKS_AUTH: &str = line!(
    "www.example.com:443 using outbound/socks[resi-2]: socks5: incorrect user name or password"
);

/// 【采样】成员出站的 `authentication required`（rick 2650 条），逐字
pub const MEMBER_AUTH_REQUIRED: &str =
    line!("www.example.com:443 using outbound/http[resi-1]: authentication required");

/// 【采样】成员出站的 `unexpected EOF`（rick 17217 条），逐字 ⇒ 不归类
pub const MEMBER_UNEXPECTED_EOF: &str =
    line!("www.example.com:443 using outbound/http[resi-1]: unexpected EOF");

/// 【转写】上游回了个不合 HTTP 的响应头（rick 15 条 / tizi 94 条）：前缀
/// `malformed MIME header line` 采到，冒号之后那截在 §2 的汇总里被省略成 `...` ⇒ 这里的尾巴
/// 是合成的。它是**负例**（哨兵与黑名单都必须判 `None`），尾巴本来就不参与判据
pub const MEMBER_MALFORMED_MIME: &str = line!(
    "www.example.com:443 using outbound/http[resi-1]: malformed MIME header line: X-Proxy-Session 7f3a"
);
