//! sing-box 住宅 HY2 入站（`hysteria-residential`，spec §8.1 的八条）的日志原文夹具。
//!
//! **不许凭记忆编**：改动这里等于改哨兵判据，要重采。每条常量的出处逐条标在它头上，
//! 三种来源：
//!
//! 1. **本机实跑**（2026-09-17，`sing-box 1.14.0`（官方归档，= `kernels.lock` 的
//!    `singbox` 轨道版本）跑 spec §2.3 形状的配置 —— `log.timestamp: true`、入站
//!    `hysteria2[hy2-resi]:40000`、`clash_api 127.0.0.1:9092`、出站 `socks[deny]` →
//!    `127.0.0.1:1` 与 `socks[slot-0-out]` → `127.0.0.1:2080`，另一个 sing-box 当客户端
//!    按 `名:密码` 握手、逐条打真流量）。**色码已剥**（journald 那一步剥，
//!    `sys::parse_journal_json`），公网源地址换成 `203.0.113.10`、UDP 目标换成
//!    `198.51.100.53`，其余**逐字原文**。
//! 2. **源码确证**（`sing-box` v1.14.1 的 `log/format.go`、`box.go` 的 logger tag 与
//!    对应模块的日志语句）：本机官方归档缺 `with_v2ray_api` 跑不出来的那一条。
//! 3. **systemd 文案**：与 4.0 同源（`signature.rs` 既有用例里的真机原文），4.1 只换单元名。
//!
//! 与 apernet（4.0 的 `hysteria-residential-<i>`）的日志形态差异，凡哨兵判据用得上的：
//! - 前缀从 `hysteria[1]: ` 变成 `+0800 <日期> <级别> [<流 id> <耗时>] <tag>: `
//!   （`log.timestamp: true` ⇒ `log/format.go` 的 `FullTimestamp` 分支 + 每条流的 id 前缀）。
//! - 出站失败的原文由 `route/conn.go` 拼：TCP 是 `open connection to <目标> using
//!   outbound/socks[<tag>]: <原因>`，UDP 是 `listen packet connection using  using
//!   outbound/socks[<tag>]: <原因>`（`using ` 重复两次是上游的原文，
//!   `E.Cause(err, "listen packet connection using ", dialerString)`）⇒ 判据**不许**
//!   只认 `open connection to `（那样 UDP 那一半全漏），只认 `outbound/socks[slot-` 加
//!   不可达标记。
//! - 鉴权失败**不打任何日志**（auth 不命中时 hysteria2 入站走 masquerade），所以
//!   `Hy2AuthHttpFailed` 在住宅单元上失去对象（spec §6、§8.1）。
//!
//! **待 T3（staging 复验）复核**：`V2RAY_API_LISTEN` 是三类来源里唯一没在真机上跑出来的
//! 一条（要自建 `with_v2ray_api` 的二进制）。它只被「健康行不许误报」那一条用例用，
//! 形态错了不会让判据失效，但 T3 采到原文后请原样替换。

/// ① 启动行：入站起监听（本机实跑）
pub const START_LINE: &str =
    "+0800 2026-09-17 03:47:55 INFO inbound/hysteria2[hy2-resi]: udp server started at [::]:40000";

/// ② 成功连接两行之一：握手来源（本机实跑，源地址换成合成公网 IP）
pub const CONN_FROM: &str = "+0800 2026-09-17 03:48:11 INFO [3493080625 0ms] \
                             inbound/hysteria2[hy2-resi]: inbound connection from 203.0.113.10:55076";

/// ② 成功连接两行之二：带**用户名**的目标行 —— spec §6 的排查线索，就是它逼着
/// `hy2-residential.json` 的 `log.level` 留在 `info`（本机实跑）
pub const CONN_TO_USER: &str = "+0800 2026-09-17 03:48:11 INFO [3493080625 0ms] \
                                inbound/hysteria2[hy2-resi]: [alice] inbound connection to www.example.com:443";

/// ② 的 UDP 形态（QUIC 目标走的就是这条，本机实跑）
pub const CONN_TO_USER_UDP: &str = "+0800 2026-09-17 03:48:50 INFO [620844294 0ms] \
                                    inbound/hysteria2[hy2-resi]: [alice] inbound packet connection to 198.51.100.53:53";

/// 槽出站的正常拨号行（`INFO`，本机实跑）：带 `outbound/socks[slot-0-out]` 但**没有**
/// 不可达标记 ⇒ 不许误报成 [`crate::modules::sentinel::signature::Sig::Hy2ResiRelayUnreachable`]
pub const SLOT_OUT_CONN: &str = "+0800 2026-09-17 03:48:11 INFO [3493080625 1ms] \
                                 outbound/socks[slot-0-out]: outbound connection to www.example.com:443";

/// ③ `bind: address already in use` 的 FATAL 行（本机实跑：同端口起第二个实例）。
/// 注意它走的是 `cmd` 那个 logger ⇒ **没有** `log.timestamp` 的完整时间戳、是
/// `FATAL[0000] ` 这种「级别 + 运行秒数」前缀
pub const BIND_IN_USE: &str = "FATAL[0000] start service: start inbound/hysteria2[hy2-resi]: \
                               listen udp 0.0.0.0:40000: bind: address already in use";

/// ④ systemd 崩溃循环两行之一（systemd 文案，4.0 起未变，只换单元名）
pub const CRASH_LOOP_A: &str =
    "hysteria-residential.service: Scheduled restart job, restart counter is at 5.";

/// ④ systemd 崩溃循环两行之二
pub const CRASH_LOOP_B: &str = "hysteria-residential.service: Start request repeated too quickly.";

/// ⑤ 拨不通 relay 的槽入站（本机实跑：`127.0.0.1:2080` 上没有 `b-ui-relay`）
pub const RELAY_DIAL_FAIL: &str = "+0800 2026-09-17 03:48:11 ERROR [3493080625 1ms] connection: \
                                   open connection to www.example.com:443 using outbound/socks[slot-0-out]: \
                                   dial tcp 127.0.0.1:2080: connect: connection refused";

/// ⑤ 的 UDP 形态（本机实跑，`using ` 重复两次是上游原文，见模块文档）
pub const RELAY_DIAL_FAIL_UDP: &str = "+0800 2026-09-17 03:48:50 ERROR [620844294 0ms] connection: \
                                       listen packet connection using  using outbound/socks[slot-0-out]: \
                                       dial tcp 127.0.0.1:2080: connect: connection refused";

/// ⑥ `deny` 噪音：被封 / 到期用户持续请求，每条流一条 ERROR（本机实跑）。
/// **必须判 `None`**：它是设计正常工作的证据，不是故障
pub const DENY_NOISE: &str = "+0800 2026-09-17 03:48:11 ERROR [2717321387 0ms] connection: \
                              open connection to www.example.com:443 using outbound/socks[deny]: \
                              dial tcp 127.0.0.1:1: connect: connection refused";

/// ⑥ 噪音的 `INFO` 前半条（本机实跑）
pub const DENY_NOISE_CONN: &str = "+0800 2026-09-17 03:48:11 INFO [2717321387 0ms] \
                                   outbound/socks[deny]: outbound connection to www.example.com:443";

/// ⑦ Clash API 起监听（本机实跑）
pub const CLASH_API_LISTEN: &str =
    "+0800 2026-09-17 03:47:55 INFO clash-api: restful api listening at 127.0.0.1:9092";

/// ⑧ v2ray_api 起监听。**源码确证、未在真机跑出**（本机的 1.14.0 官方归档没有
/// `with_v2ray_api`）：tag 来自 `box.go:438` 的 `logFactory.NewLogger("v2ray-api")`，
/// 正文来自 `experimental/v2rayapi/server.go:59` 的
/// `s.logger.Info("grpc server started at ", listener.Addr())`，前缀与 ⑦ 同源。
/// **待 T3 用自建二进制采到原文后替换**
pub const V2RAY_API_LISTEN: &str =
    "+0800 2026-09-17 03:47:55 INFO v2ray-api: grpc server started at 127.0.0.1:10086";
