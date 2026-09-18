//! sing-box 住宅 HY2 入站（`hysteria-residential`，spec §8.1 的八条）的日志原文夹具。
//!
//! **不许凭记忆编**：改动这里等于改哨兵判据，要重采。每条常量的出处逐条标在它头上，
//! 三种来源：
//!
//! 1. **2026-09-18 tizi sidecar 自建 1.14.1 真机**（`with_v2ray_api`，生产形状配置：整段
//!    跳跃 + 每凭据一个 `gate-<id>` selector 门 + `auth_user` 路由到门）。落这里的是
//!    RESULT-up.md「八条夹具」一节（T3 阶段 0），**色码已剥**（journald 那一步剥，
//!    `sys::parse_journal_json`）。隔离端口改回生产值（`40100→40000`、`9192→9092`、
//!    `10186→10086`），公网源地址 / 测试目标域名换成 `203.0.113.10` / `www.example.com`，
//!    用户名保留两条**合成**名做解析考察（中文 `测试用户`、含空格 + U+00B7 中点的
//!    `低 空·飞行`，都是本次测试造的合成名、不是真实用户），其余用 `alice` / `r000`；
//!    其余**逐字原文**（时间戳保留真机的 `+0000 2026-09-18`）。
//! 2. **2026-09-17 本机实跑**（`sing-box 1.14.0` 官方归档，= `kernels.lock` 的 `singbox`
//!    轨道版本；配置**没有门 selector**）：保留成员出站形态的两条
//!    （`RELAY_DIAL_FAIL_SOCKS_FORM` / `DENY_NOISE_SOCKS_FORM` 及其 UDP 变体），只用来证明
//!    判据**两种形态都认**——生产不会再打这种形态。`BIND_IN_USE` 也来自这一版（1.14.1 的
//!    dup 单元复现了同类 FATAL，见 RESULT-up.md 步骤 6，但没采到整行）。UDP 的 selector
//!    形态真机未采到，按 1.14.0 实录 + selector 替换推断（见 `RELAY_DIAL_FAIL_UDP` 注）。
//! 3. **systemd 文案**：与 4.0 同源（`signature.rs` 既有用例里的真机原文），4.1 只换单元名。
//!
//! **生产形态的关键点**：一旦成员出站拨号失败，refused 的 ERROR 行挂在最外层 **selector 门**
//! `outbound/selector[gate-<id>]` 上（成员 tag 只出现在它自己的 INFO `outbound connection to`
//! 行里）——所以判据**不能**只认 `outbound/socks[slot-`，那样生产上永远探不到（RESULT-up.md
//! 「与预期不符」第 1 条）。deny 与槽的区分改看 ERROR 行 `dial tcp 127.0.0.1:<port>` 里的回环
//! 端口：`1` = deny 噪音、`2080..` = 槽拨不通。
//!
//! 与 apernet（4.0 的 `hysteria-residential-<i>`）的日志形态差异，凡哨兵判据用得上的：
//! - 前缀从 `hysteria[1]: ` 变成 `<tzoffset> <日期> <级别> [<流 id> <耗时>] <tag>: `
//!   （`log.timestamp: true` ⇒ `log/format.go` 的 `FullTimestamp` 分支 + 每条流的 id 前缀）。
//! - 出站失败的原文由 `route/conn.go` 拼：TCP 是 `open connection to <目标> using
//!   outbound/<kind>[<tag>]: <原因>`，UDP 是 `listen packet connection using  using
//!   outbound/<kind>[<tag>]: <原因>`（`using ` 重复两次是上游的原文，
//!   `E.Cause(err, "listen packet connection using ", dialerString)`）⇒ 判据**不许**
//!   只认 `open connection to `（那样 UDP 那一半全漏）。
//! - 鉴权失败**不打任何日志**（auth 不命中时 hysteria2 入站走 masquerade），所以
//!   `Hy2AuthHttpFailed` 在住宅单元上失去对象（spec §6、§8.1）。

/// ① 启动行：入站起监听（2026-09-18 真机，端口 40100→40000）
pub const START_LINE: &str =
    "+0000 2026-09-18 02:48:21 INFO inbound/hysteria2[hy2-resi]: udp server started at [::]:40000";

/// ② 成功连接两行之一：握手来源（2026-09-18 真机，回环源地址换成合成公网 IP）
pub const CONN_FROM: &str = "+0000 2026-09-18 02:52:06 INFO [3044954400 0ms] \
                             inbound/hysteria2[hy2-resi]: inbound connection from 203.0.113.10:30990";

/// ② 成功连接两行之二：带**用户名**的目标行 —— spec §6 的排查线索，就是它逼着
/// `hy2-residential.json` 的 `log.level` 留在 `info`（ASCII 用户名，2026-09-18 真机同形，
/// 用户名用合成 `alice`、目标域名换 `www.example.com`）
pub const CONN_TO_USER: &str = "+0000 2026-09-18 02:52:06 INFO [3044954400 0ms] \
                                inbound/hysteria2[hy2-resi]: [alice] inbound connection to www.example.com:443";

/// ②（③a）中文用户名的目标行（2026-09-18 真机 `测试用户`，目标域名换 `www.example.com`）：
/// 判据不解析用户名，这条只考察非 ASCII 用户名不误报、不崩
pub const CONN_TO_USER_ZH: &str = "+0000 2026-09-18 02:52:06 INFO [3044954400 0ms] \
                                    inbound/hysteria2[hy2-resi]: [测试用户] inbound connection to www.example.com:443";

/// ②（③a2）含空格 + U+00B7 中点的中文用户名目标行（2026-09-18 真机 `低 空·飞行`，
/// 目标域名换 `www.example.com`）：考察「空格 + 非 ASCII 标点」的用户名不误报、不崩
pub const CONN_TO_USER_ZH_SPACE: &str = "+0000 2026-09-18 02:52:07 INFO [1663292108 0ms] \
                                          inbound/hysteria2[hy2-resi]: [低 空·飞行] inbound connection to www.example.com:443";

/// ② 的 UDP 形态（QUIC 目标走的就是这条，2026-09-17 本机实跑）
pub const CONN_TO_USER_UDP: &str = "+0800 2026-09-17 03:48:50 INFO [620844294 0ms] \
                                    inbound/hysteria2[hy2-resi]: [alice] inbound packet connection to 198.51.100.53:53";

/// ③b 槽出站的正常拨号行（`INFO`，2026-09-18 真机 slot-0-out，目标域名换 `www.example.com`）：
/// 带 `outbound/socks[slot-0-out]` 但**没有**不可达标记 ⇒ 不许误报成
/// [`crate::modules::sentinel::signature::Sig::Hy2ResiRelayUnreachable`]
pub const SLOT_OUT_CONN: &str = "+0000 2026-09-18 02:52:06 INFO [2880481059 300ms] \
                                 outbound/socks[slot-0-out]: outbound connection to www.example.com:443";

/// `bind: address already in use` 的 FATAL 行（2026-09-17 本机实跑：同端口起第二个实例；
/// 1.14.1 的 dup 单元复现同类 FATAL，见 RESULT-up.md 步骤 6）。注意它走的是 `cmd` 那个
/// logger ⇒ **没有** `log.timestamp` 的完整时间戳、是 `FATAL[0000] `「级别 + 运行秒数」前缀
pub const BIND_IN_USE: &str = "FATAL[0000] start service: start inbound/hysteria2[hy2-resi]: \
                               listen udp 0.0.0.0:40000: bind: address already in use";

/// systemd 崩溃循环两行之一（systemd 文案，4.0 起未变，只换单元名）
pub const CRASH_LOOP_A: &str =
    "hysteria-residential.service: Scheduled restart job, restart counter is at 5.";

/// systemd 崩溃循环两行之二
pub const CRASH_LOOP_B: &str = "hysteria-residential.service: Start request repeated too quickly.";

/// ⑤b 拨不通 relay 的槽入站 —— **生产形态**（2026-09-18 真机）：门 selector 拨 slot-7 的
/// `127.0.0.1:2087` 被拒，ERROR 挂在 `outbound/selector[gate-r000]` 上，槽序号只在
/// `dial tcp 127.0.0.1:<port>` 里。判据靠这个回环端口（2080..）认它是槽拨不通
pub const RELAY_DIAL_FAIL: &str = "+0000 2026-09-18 02:52:23 ERROR [1393264390 301ms] connection: \
                                   open connection to www.example.com:443 using outbound/selector[gate-r000]: \
                                   dial tcp 127.0.0.1:2087: connect: connection refused";

/// ⑤b 的 UDP 形态：**真机未采到**，按 2026-09-17 的 1.14.0 UDP 实录（`using ` 重复两次是
/// 上游原文，见模块文档）+ selector 替换推断。UDP 经 socks5 出站先拨 TCP 控制连接到
/// `127.0.0.1:2080`，拨不通同样落成 `dial tcp 127.0.0.1:<port>`
pub const RELAY_DIAL_FAIL_UDP: &str = "+0000 2026-09-18 02:52:24 ERROR [620844295 0ms] connection: \
                                       listen packet connection using  using outbound/selector[gate-r000]: \
                                       dial tcp 127.0.0.1:2080: connect: connection refused";

/// ⑤ 拨不通槽入站的**成员出站形态**（2026-09-17 本机实跑，1.14.0 无门配置）：生产不会再
/// 出现，仅证明判据对 `outbound/socks[slot-<i>-out]` 这种旧形态也认
pub const RELAY_DIAL_FAIL_SOCKS_FORM: &str = "+0800 2026-09-17 03:48:11 ERROR [3493080625 1ms] connection: \
                                              open connection to www.example.com:443 using outbound/socks[slot-0-out]: \
                                              dial tcp 127.0.0.1:2080: connect: connection refused";

/// ⑤ 成员出站形态的 UDP 变体（2026-09-17 本机实跑，1.14.0 无门配置）
pub const RELAY_DIAL_FAIL_UDP_SOCKS_FORM: &str = "+0800 2026-09-17 03:48:50 ERROR [620844294 0ms] connection: \
                                                  listen packet connection using  using outbound/socks[slot-0-out]: \
                                                  dial tcp 127.0.0.1:2080: connect: connection refused";

/// ⑥b `deny` 噪音 —— **生产形态**（2026-09-18 真机）：到期用户（门 default=deny）持续请求，
/// 门 selector 拨 `127.0.0.1:1` 被拒，ERROR 挂在 `outbound/selector[gate-r003]` 上。
/// **必须判 `None`**：它是门在正常拒绝的证据，判据靠回环端口 `1` 把它和槽拨不通分开
pub const DENY_NOISE: &str = "+0000 2026-09-18 02:52:09 ERROR [3173138189 301ms] connection: \
                              open connection to www.example.com:443 using outbound/selector[gate-r003]: \
                              dial tcp 127.0.0.1:1: connect: connection refused";

/// ⑥a 噪音的 `INFO` 前半条（2026-09-18 真机 socks[deny]，目标域名换 `www.example.com`）
pub const DENY_NOISE_CONN: &str = "+0000 2026-09-18 02:52:09 INFO [3173138189 300ms] \
                                   outbound/socks[deny]: outbound connection to www.example.com:443";

/// ⑥ `deny` 噪音的**成员出站形态**（2026-09-17 本机实跑，1.14.0 无门配置）：生产不会再
/// 出现，仅证明 `outbound/socks[deny]` 这条旧 guard 仍把它判 `None`
pub const DENY_NOISE_SOCKS_FORM: &str = "+0800 2026-09-17 03:48:11 ERROR [2717321387 0ms] connection: \
                                         open connection to www.example.com:443 using outbound/socks[deny]: \
                                         dial tcp 127.0.0.1:1: connect: connection refused";

/// ⑦ Clash API 起监听（2026-09-18 真机，端口 9192→9092）
pub const CLASH_API_LISTEN: &str =
    "+0000 2026-09-18 02:48:21 INFO clash-api: restful api listening at 127.0.0.1:9092";

/// ⑧ v2ray_api 起监听（2026-09-18 tizi sidecar 自建 `with_v2ray_api` 真机，端口 10186→10086）。
/// 4.1 起随发布分发的 sing-box 自带 `with_v2ray_api`（住宅 HY2 的按用户计量靠它），这条终于
/// 能在真机上采到，替换掉先前的源码推断版
pub const V2RAY_API_LISTEN: &str =
    "+0000 2026-09-18 02:48:21 INFO v2ray-api: grpc server started at 127.0.0.1:10086";

/// ⑨a 证书热加载的中间态 ERROR（2026-09-18 本机自建 1.14.1 换证实测，item09/logs/sidecar.log，
/// `+0800` 是那次本机运行的真时区、逐字原文；行里无端口 / 路径 / 用户名，无需归一）。
/// b-ui 写盘是 tmp+rename、先 cert 后 key，每次轮换都必然先出这条「cert 已换 key 未换那几毫秒」
/// 的 ERROR（§3.5 第 3 件、§12 第 9 项）。**没人给它加白**：`classify` 是签名白名单，这条匹配不到
/// 任何签名 ⇒ 默认忽略、判 `None`；本常量是把这条「默认忽略」钉死的守门夹具，防止将来有人加一条
/// 泛 ERROR 签名把它当异常报出来
pub const CERT_RELOAD_MISMATCH: &str = "+0800 2026-09-18 10:45:22 ERROR inbound/hysteria2[hy2-resi]: \
                                        reload certificate: reload key pair: tls: private key does not match public key";

/// ⑨b 证书热加载成功那条 INFO（同一次实测，紧跟 [`CERT_RELOAD_MISMATCH`] 之后）：key 落盘后第二次
/// reload 成功。同样匹配不到任何签名 ⇒ 判 `None`
pub const CERT_RELOADED: &str =
    "+0800 2026-09-18 10:45:22 INFO inbound/hysteria2[hy2-resi]: reloaded TLS certificate";
