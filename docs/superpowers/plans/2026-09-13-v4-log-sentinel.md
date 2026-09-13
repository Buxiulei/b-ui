# v4 日志哨兵与预案实施计划（spec §5.7）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 主理人 2026-09-13 原话：「bui 需要监听后台日志，如果出现报错，要立马处理，比如某个住宅代理 ip 连不通了，就要启用预案，立马分配新的 ip」。本计划在守护进程里加一个**日志哨兵**：每 5 秒增量读受管单元的 journald，按签名表识别故障，经去抖 / 冷却后执行预案（带外快探 → 该槽立即借用健康 IP / 用户同步重试 / 告警），每次触发写一条事件（`runtime.incidents`，环形 200 条），`bui status` 显示最近 5 条、`bui incidents` 查全量、面板新增「事件」卡；外加一个在生产机上用 iptables 断一条上游的真机演练脚本。

**Architecture:** 新模块 `crates/bui/src/modules/sentinel/`，实现 P1 的 `reconcile::Module`（空 `render`、一个后台任务、一条管理员路由）。采集走新增的 `Host::journal_read` 原语（`journalctl -o json --after-cursor` / `--since @<now>`，解析是 `sys` 里的纯函数，`FakeHost` 按队列可编程返回）；签名匹配（`signature.rs`）与去抖（`engine.rs`）都是纯函数；预案分两块——住宅（`resi.rs`：复用 `health` 的探测与 `slots` 的借用语义，新增 `probe_quick` / `mark_unhealthy` / `borrow_now` 三个小函数）与系统（`system.rs`：hysteria 鉴权告警、bind / 崩溃循环只记事件、caddy 证书告警、xray gRPC 不可用时立即跑一轮 `users::sync_now`）；主循环（`run.rs`）把它们串起来并落盘。哨兵只做「探测 → 借用 / 重试 / 告警」，**不增删池成员、不做切回**（切回仍由巡检的 `drive_slots` 连续 3 轮负责）。

**Tech Stack:** Rust 1.93 stable（edition 2021）、tokio 1、axum 0.8、serde / serde_json、time 0.3、uuid 1、clap 4（derive）、tracing / tracing-journald 0.3.2、anyhow；dev：pretty_assertions、tempfile。前端只用原生 DOM API（`web/app.js` 无构建步骤）。演练脚本：bash + python3 + iptables（真机）/ PATH stub（自测）。**不新增任何 crate 依赖。**

**Spec:** `docs/superpowers/specs/2026-09-11-v4-architecture-design.md` §5.7（日志哨兵与预案，已随本计划的 commit 按下文设计裁决修订；§3.2 末尾「哨兵」一句同步改为指向 `modules::sentinel`）为主，外加 §3.2（Hysteria2 http 鉴权）、§3.4（watchdog）、§5.3（健康迟滞与切换）、§5.4（黑名单的日志候选，与哨兵分界）、§5.6（IP 池与槽位、按槽驱动与借用）、§8（测试策略）；总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1 / C2 / C3 / C5 契约、裁决记录）；前置计划 `docs/superpowers/plans/2026-09-13-v4-p3-ip-pool.md`（D1–D11，尤其 D8 的按槽驱动）。

## Global Constraints

抄自 spec §0 与总纲全局约束 / C1–C5 的硬约束，**每个任务的要求都隐含包含本节**：

- 协议集不变：TCP/443 车道 = VLESS + REALITY + Vision（Xray-core）；UDP 车道 = Hysteria2 + Salamander + 端口跳跃（apernet/hysteria）。协议内核保留成熟实现，不自实现协议；Rust 只覆盖控制面、住宅模块、Linux 客户端。
- 单机先做扎实，为多机与外部通知预留接口、不实现（本计划的 Telegram / Webhook 只留 `Notifier` trait + 空实现）。
- Rust stable ≥ 1.85（本机 1.93），edition 2021，`cargo clippy --workspace --all-targets -- -D warnings` 零告警，`cargo fmt --all -- --check` 通过。test 构建严格拦 `dead_code`（`main.rs` 的 `#![cfg_attr(not(test), allow(dead_code))]`），所以**每个任务只定义它自己或它的测试用得到的项**。
- 发布目标 `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`；依赖优先纯 Rust；TLS 用 `rustls` + `ring`，禁止 openssl。本计划不新增依赖。
- 端口、标签、UUID、密码与 v3 一致；订阅输出与 v3 golden 逐项相等——本计划**不碰任何渲染器**，`crates/bui-schema/` 一个字节都不改。
- **凭据、密钥、密码不进 argv、不进日志**（`tracing` 字段一律脱敏）；state 与快照文件 0600。哨兵写进事件、告警、日志的任何原文都先过 `redact::line`（本计划 Task 2 新增），**上游只以 `host:port` 或体检学到的出口 IP 指称，绝不带用户名 / 密码**；日志里出现的上游 URL 一律脱敏。
- `state.json` / `runtime.json` 只经 P1 的 `Store` / `Runtime` 读写（临时文件 + rename）；哨兵的运行时数据放 `runtime.extra`（P1 的扩展位约定，不改 `RuntimeData` 的类型）。
- **测试不碰真实系统**：单元测试只接触 `tempfile` 的临时目录；journald、systemctl、监听端口一律经 `Host`（注入 `FakeHost`），网络探测经 `Prober` / `Clash` / `XrayApi`（注入 `FakeProber` / `FakeClash` / `FakeXray`）。`scripts/tests/` 禁止访问网络，需要 bui / iptables / curl / journalctl 的地方一律用 PATH 前置的 stub。
- 生产主机只用别名 `bwg-rick` / `bwg-tizi` / `baiyi`；公开仓库里不出现真实 IP、域名、供应商凭据。测试与文档只用合成值：域名 `example.com` / `panel.example.com`、VPS `203.0.113.10`、住宅出口 `198.51.100.7` / `198.51.100.8` / `198.51.100.9`、上游 `isp<N>.example.net:10007`、用户名 `user1`、密码 `pw1`。
- 面板 / CLI 的用户可见文字是中文；Shell 脚本带 `#!/usr/bin/env bash` + `LC_ALL=C`，采样/等待循环用 `set -uo pipefail`；过 `bash -n` 与 `shellcheck -S error`。
- 前端 `web/{index.html,app.js,style.css}` 保留，**只加一张卡，不重构**（spec §0「只改必要的 4 处」的一次例外，已在总纲裁决记录登记）。
- 分支基线 `v4`。**执行前先 rebase 到 `v4@606b697`（或更新）**：本计划写于 a8c7b53，此后 v4 合入了小修 A（`residential/slots.rs` 的 `converge_xray` gRPC 退避重试、`residential/state.rs` 的 `remove_alerts_with_prefix`）与小修 B（`residential/{api,cli}.rs`、`panel/fakes.rs`）；Task 4 的替换锚点（`drive_slots` 里从 `let mut switched = false;` 到 PUT 那一段、`state.rs` 的 `apply_hysteresis`）在 606b697 上未变，rebase 无冲突。实施分支 `v4-sentinel-t<N>`，**每个任务一个 commit**，前缀 `feat(sentinel):` / `feat(sys):` / `feat(residential):` / `test(sentinel):` / `docs(sentinel):`，正文末尾两行：
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv`
  不 `git commit -a`；不裸 `git stash`；不推送。
- **每个任务先写失败测试再实现**，跑到红了才动实现代码。`bui` 是 bin-only crate，测试一律写成源文件内 `#[cfg(test)] mod tests`。

---

## 文件结构

```
crates/bui/src/util.rs                          strip_ansi 从 residential::journal 挪来（sys 与 residential 共用）。T1
crates/bui/src/sys/mod.rs                       JournalFrom / JournalRecord / JOURNALCTL_MISSING / journal_args /
                                                parse_journal_json；Host::journal_read。T1
crates/bui/src/sys/real.rs                      RealHost::journal_read（which + run + 解析）。T1
crates/bui/src/sys/fake.rs                      FakeInner.journal 队列 + `journal:<units>:<from>` 流水。T1
crates/bui/src/modules/residential/journal.rs   strip_ansi 改为 `pub use crate::util::strip_ansi`。T1
crates/bui/src/redact.rs                        line()：整行日志里带 userinfo 的片段逐段脱敏。T2
crates/bui/src/modules/watchdog.rs              抽出 is_auth_http_failure（T2）；删掉 http 鉴权日志分支（迁到哨兵，T7）。
crates/bui/src/modules/panel/users.rs           USER_SYNC_FAILED_LOG 常量（哨兵按它认 xray gRPC 失败）。T2
crates/bui/src/modules/mod.rs                   `pub mod sentinel;`。T2
crates/bui/src/modules/sentinel/mod.rs          模块文档、常量、Deps、Notifier、SentinelModule。T2 建，T3/T5/T6/T7/T8 追加
crates/bui/src/modules/sentinel/signature.rs    签名表（纯函数）：Sig / Action / Rule / Match / classify。T2
crates/bui/src/modules/sentinel/engine.rs       去抖（Engine）、冷却（in_cooldown / prune_acted）、SentinelRuntime。T3
crates/bui/src/modules/sentinel/incidents.rs    Incident / Level / 环形 push（T3）；Outcome（T5）；格式化与 load_recent（T8）。
crates/bui/src/modules/residential/health.rs    probe_quick：哨兵用的带外快探。T4
crates/bui/src/modules/residential/state.rs     mark_unhealthy：带外确认不可达 ⇒ 立即判不健康。T4
crates/bui/src/modules/residential/slots.rs     put_slot（drive_slots 抽出的 PUT 助手）+ borrow_now。T4
crates/bui/src/modules/sentinel/testkit.rs      `#[cfg(test)]` 支架：三上游三槽的假机器、面板 Shared、日志记录。T5 建，T6/T7 追加
crates/bui/src/modules/sentinel/resi.rs         住宅预案：on_upstream_error / on_google_blocked / sweep_long_unreachable。T5
crates/bui/src/modules/sentinel/system.rs       系统预案：on_hy2_auth / on_kernel / on_caddy_cert / on_xray_grpc。T6
crates/bui/src/modules/sentinel/run.rs          主循环：tick（读 → 匹配 → 去抖 → 冷却 → 预案 → 落盘）+ sentinel_loop。T7
crates/bui/src/serve.rs                         modules() 注册 SentinelModule；注册子集断言加 "sentinel"。T7
crates/bui/src/modules/sentinel/api.rs          GET /api/incidents（管理员鉴权）。T8
crates/bui/src/commands/incidents.rs            `bui incidents [--json] [-n N]`。T8
crates/bui/src/commands/mod.rs / cli.rs / main.rs   子命令注册与派发。T8
crates/bui/src/commands/status.rs               `bui status` 末尾「最近事件」5 条。T8
web/index.html / web/app.js                     「事件」卡 + loadIncidents()。T9
crates/bui/src/modules/panel/assets.rs          守门测试：卡片与脚本已内嵌、接到 /api/incidents。T9
scripts/ops/sentinel-drill.sh                   真机演练（root，iptables 断一条上游，trap 兜底恢复）+ --self-test。T10
scripts/tests/test-sentinel-drill.sh            跑 --self-test 并核对摘要 / 退出码。T10
docs/superpowers/specs/2026-09-11-v4-architecture-design.md   §5.7 按落地口径修订。T11
docs/superpowers/plans/2026-09-11-v4-master.md  裁决记录加一行。T11
CLAUDE.md / CHANGELOG.md                        进程职责一句 + 新增条目。T11
```

---

## 设计裁决（相对 spec §5.7 原文与主理人口径的落地调整，已同步修订 spec §5.7）

- **D1 采集 = 增量轮询，不是 `-f`**：与黑名单的 `residential::journal::collect` 同一思路（契约决策 §E），每 `POLL_SECS = 5` 秒跑一次 `journalctl --no-pager -q -o json -u <每个单元> (--after-cursor <c> | --since @<unix 秒>)`。游标**以内存为准**：每轮都从上一轮最后一条的 `__CURSOR` 续读；落盘到 `runtime.extra["sentinel"].cursor` 有节流（有事件 / 冷却表变化时立即落；只是游标前进则至少隔 `CURSOR_PERSIST_SECS = 60` 秒落一次）——hysteria 在 info 级每条连接都写日志，若每 5 秒 fsync 一次 `runtime.json` 一天就是一万七千次写盘。代价：守护进程重启后最多重读 60 秒日志，由 D12（窗口按日志时间戳）与持久化的冷却表兜住，不会重复动作。**首次启动（没有游标也没有 `since`）从「现在」读起，不回放历史**；游标失效（日志轮转、`journalctl` 非零退出）同样丢游标、`since = 现在`。
- **D2 单元集合 = `reconcile::managed_units(&state)`**：六个固定单元（`b-ui` / `hysteria-server` / `hysteria-residential` / `xray` / `b-ui-relay` / `caddy`）+ 槽 1.. 的 `hysteria-residential-<i>`。**包括 `b-ui` 自己**：xray 的 gRPC `Unavailable` 不出现在 xray 的日志里，而是出现在守护进程自己「用户同步有失败项」那一行（见 D10）。
- **D3 JSON 字段解码**：sing-box 往 journald 写带 ANSI 色码的输出，含 ESC 的 `MESSAGE` 在 `-o json` 里是**字节数组**而不是字符串；解析器两种都认，再过 `strip_ansi`。单元名先看 `UNIT` 再看 `_SYSTEMD_UNIT`（systemd 自己关于某单元的「Start request repeated too quickly」是 `_SYSTEMD_UNIT=init.scope`、`UNIT=xray.service`）。`b-ui` 的 tracing 事件经 tracing-journald 0.3.2 写入，结构化字段带默认前缀 `F_`：`error = %e` 落在 `F_ERROR`，解析器把它拼成 `MESSAGE + " error=" + F_ERROR`。
- **D4 relay 签名的边界**：`open connection to <host>:<port> using outbound/<http|socks>[resi-N]: <reason>` 里，`reason` 是 `unexpected status: 4xx/5xx`（407 除外）或 `socks5:` 开头的 REP 拒绝 ⇒ 这是「上游拒绝了**这个目标**」，归黑名单（spec §5.4），哨兵**不计**；`connection refused` / `i/o timeout` / `deadline exceeded` / `no route to host` / `network is unreachable` / `unexpected status: 407` / `proxy authentication` / `incorrect user name or password` ⇒ 「上游本身不能用」，计 `relay_upstream_error`；`403` 且（含 `serp` 或目标是 Google 搜索域名）⇒ `relay_google_blocked`。成员 tag 是位置键，匹配当场经 `clash::id_of_tag` 换成 uuid 再计数（契约决策 §C），换不出来的丢弃。误判的代价只是一次带外探测（D5 是仲裁者）。
- **D5 带外探测用快探 `health::probe_quick`，不用巡检的 `probe_member`**：`probe_member` 在「上游被丢包」时要走完 `timed_get`（8s）+ `get`（10s）+ 407 补判 CONNECT（10s）+ Google（8s）+ STUN（5s），约 40 秒，达不到演练的 15 秒。快探先量到网关的 TCP 建连（哨兵用 `ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS = 5)`），**连不上直接判不可达**；连得上再走一轮与巡检同口径的 `probe_reachable`（含 407 补判）。不测 Google / UDP / 测速。
- **D6 带外确认不可达 ⇒ 立即判不健康**（`state::mark_unhealthy`：`active = false`、`failstreak ≥ FAIL_TO_UNHEALTHY`、记一条失败样本），不等巡检的 2 轮迟滞。否则下一轮巡检里这条 IP 仍是 `active`，`drive_slots` 会把「还坏着的一轮」当成「恢复第 1 轮」。**切回口径因此是**：IP 恢复后，巡检先要连续 `OK_TO_HEALTHY = 2` 轮探通才重新判健康（第 2 轮同时算切回第 1 轮），再攒满 `SLOT_BACK_ROUNDS = 3` 轮切回 ⇒ **恢复后第 4 轮巡检切回（约 8 分钟）**。主理人口径「恢复后 3 轮内切回」指的是 `SLOT_BACK_ROUNDS` 这一段；演练脚本按「≤ OK_TO_HEALTHY + SLOT_BACK_ROUNDS = 5 轮」判（多给一轮对齐巡检周期），见 Task 10。**已裁决（2026-09-13 主会话，总纲裁决记录已写）：保持现有切回语义**——迟滞 `OK_TO_HEALTHY = 2` 轮 + `SLOT_BACK_ROUNDS = 3` 轮，即恢复后第 4 轮巡检切回，实现不改；演练 `DRILL_BACK_WAIT` 缺省 660 秒（见文末「切回口径裁决」）。
- **D7 借用 = `slots::borrow_now`**：与 `drive_slots` 同一口径、只做「借出」一半——只动**当前出口就是故障 IP** 的槽（本槽 IP 就是它、或正借用它；`current_upstream_id = None` 视为停在本槽 IP 上）；手动 pin 的槽不动；目标 = 本槽 IP 不是故障 IP 且健康、Google 未封 ⇒ 回本槽，否则 `health::rank_healthy` 里排名最高的非故障健康 IP；一个都没有 ⇒ 保持现状（fail-open）；被挪的槽 `back_rounds` 归零。**不推进任何槽的 `back_rounds`**——直接再调一次 `drive_slots` 会让恢复中的槽多数一轮，把「连续 3 轮」的防抖打穿。PUT 助手 `put_slot` 从 `drive_slots` 抽出、两处共用。**与巡检互斥**：`drive_slots` 整轮「读快照 → 逐槽 PUT → 按快照写回 `current_upstream_id`」，`borrow_now` 若落在中间，它写的记录会被那轮写回盖成旧值（relay 的 selector 已经切走，面板与演练脚本看到的却是没借用，要等下一轮巡检才自愈），所以两者共用 `slots.rs` 的私有锁 `SLOT_SWITCH`（`tokio::sync::Mutex<()>`，各自整段持锁；持锁期间只有毫秒级的 Clash PUT，不含探测）。**不动全局 `resi-pool`**：哨兵只挪每槽的 `slot-<i>-pool`；全局 selector（relay 里 `dns_resi` 的 detour 与 global 模式的 `route.final` 用它）仍由巡检驱动。故障 IP 恰好是全局选择时，要等下一轮巡检（≤ 2 分钟）才切走；这段时间里受影响的主要是经 `dns_resi` 解析的关键字域名查询，各槽入站的连接走本槽 selector，已经被哨兵挪走。
- **D8 Google 被封**：日志里的 `403 serp` 触发一次带外 `google_search` 复核：复核 `Some(true)` ⇒ 不动作；`Some(false)`（403 / 429 / sorry 页 / unusual traffic，判据是 `proxy::google_ok_of` 那唯一一份）或没结论 ⇒ 按日志里的明确策略处理：`record_google(false)` + `borrow_now`。告警走全局 `push_alert`（上游级告警会在下一轮巡检「探通即清」，Google 被封时它是通的）。
- **D9 hysteria 鉴权失败只告警**：主理人本次口径改掉了 spec 原表「失败则重启 b-ui」——守护进程自身由 systemd `Restart=always` 拉起，自己 restart 自己只会把正在收敛的对账拦腰砍断。原来 watchdog 里每 60 秒 `journalctl -n 50` 的鉴权哨兵分支（`AUTH_HTTP_KEY` / `AuthHttpAlert` / `should_alert`，仓库里无人读取）**迁到哨兵**（Task 7 删除 watchdog 分支，判据 `is_auth_http_failure` 与门槛 `AUTH_HTTP_FAIL_THRESHOLD` 留在 watchdog 复用），避免同一故障两处各报一次。`hy2_auth = command` 时忽略该签名。
- **D10 xray gRPC 不可用**：信号源是 `b-ui` 自己的日志（`users::report` 的 `USER_SYNC_FAILED_LOG` 行，错误在 `F_ERROR`；tonic 0.14 的 `Status` 显示为 `code: 'The service is currently unavailable'`），匹配子串 `unavailable`。安全网 60 秒一轮，「连续两轮」落在 150 秒窗口里 ⇒ 门槛 `2 条 / 150 秒`。动作：xray 的 API 端口（`panel::XRAY_API_ADDR` 的 10085）在听 ⇒ 立即 `users::sync_now` 一轮；不在听 ⇒ 只记事件（xray 回来后 `sync_users` 的 `NRestarts` 侦测会自己重放）。**不纳入** v4@606b697 小修 A 的两个信号：`converge_xray` 的「xray gRPC 暂时连不上（多半刚重启、还没起监听），退避后重试」是 info 级，对账每次正常重启 xray 都会打，而且它自己已在 `GRPC_RETRY_BUDGET`（15 秒）内退避重试；「Xray 槽路由 gRPC 失败」是 `restart_fallback` **已经**重启过 xray 之后写的 runtime 告警（不是日志行），处置已经做完，收敛成功时 `mark_converged` 会自己认领清掉。这两个信号针对的是 RoutingService 槽路由，哨兵的 `retry_user_sync` 修的是 HandlerService 用户同步，纳入只会把同一次重启重复报一遍。
- **D11 caddy**：`"level":"error"` 且含 `obtaining certificate` 或 `could not get certificate` ⇒ 告警，对象是 `identifier` 字段或 `error` 里 `: obtaining certificate` 前的域名；不自动动作。
- **D12 去抖与冷却**：同签名同对象在各自窗口内达门槛才触发，触发后 `DEBOUNCE_SECS = 60` 秒内不再触发（内存）；同「动作 + 对象」`ACTION_COOLDOWN_SECS = 600` 秒内不重复执行（`runtime.extra["sentinel"].acted`，**持久化**，重启不失忆）；冷却中的触发不记事件（只 `debug` 日志），免得每 60 秒刷一条。**时间戳早于窗口的日志不计数**（重启后续读到的积压描述的是过去，不该此刻触发动作）。
- **D13 事件存放**：`runtime.extra["incidents"]`（P1 的扩展位约定，JSON 形状即 `runtime.json` 顶层的 `incidents` 数组），新的在前，`INCIDENTS_MAX = 200`；字段 `at / unit / signature / subject / action / result / level / sample`（`at` 在预案做完后才盖，即借用已生效的时刻，不是本轮开头，见 Task 7 的 `tick`）。`bui status` 显示最近 5 条（`--json` 不变：`/api/health` 的形状是 P2 锁死的回归面）；`bui incidents` 默认 20 条；面板卡片 20 条；`GET /api/incidents?limit=N`（缺省 50，夹到 1..=200）。守护进程没跑时 CLI 直接读 `runtime.json`。
- **D14 外部通知只留口**：`trait Notifier { fn notify(&self, &Incident) }` + `NoopNotifier`，每条事件落盘后调一次；Telegram / Webhook 本期不做。
- **D15 预案边界**：不改 `state` 里的池成员。住宅 IP 被判不健康连续 `LONG_UNREACHABLE_MINS = 30` 分钟 ⇒ 一次性写一条上游级告警 + 事件，建议管理员替换（面板删上游后添加，或 `bui residential remove <host:port>` + `bui residential add -`）；替换后 §5.6 的重分配自动完成。恢复（`active` 回来）即清「已建议」标记。
- **D16 对 C2 契约零改动**：`Module` / `Host` 之外不动任何契约面；`Host` 新增的一个方法按总纲「Host trait 同步 + spawn_blocking」口径实现，Task 11 在总纲 C2 的 Host 清单里补上它。

---
## Task 1: `Host::journal_read`——journald 增量读取原语

**Files:**
- Modify: `crates/bui/src/util.rs`（新增 `strip_ansi`）
- Modify: `crates/bui/src/modules/residential/journal.rs`（`strip_ansi` 改为 re-export）
- Modify: `crates/bui/src/sys/mod.rs`（类型、纯函数、trait 方法、测试）
- Modify: `crates/bui/src/sys/real.rs`（`RealHost::journal_read`）
- Modify: `crates/bui/src/sys/fake.rs`（`FakeInner.journal` 队列 + 实现）

**Interfaces:**
- Consumes: 既有 `sys::unit_full`（私有，同模块可用）、`util::fmt_rfc3339`。
- Produces:
  - `crate::util::strip_ansi(&str) -> String`
  - `crate::sys::JournalFrom { Cursor(String), Since(time::OffsetDateTime) }`（`Clone, PartialEq, Eq, Debug`）
  - `crate::sys::JournalRecord { cursor: String, unit: String /*裸名*/, ts: OffsetDateTime, message: String }`（`Clone, PartialEq, Eq, Debug`）
  - `crate::sys::JOURNALCTL_MISSING: &str`
  - `crate::sys::journal_args(&[String], &JournalFrom) -> Vec<String>`
  - `crate::sys::parse_journal_json(&str, &[String]) -> Vec<JournalRecord>`
  - `trait Host { fn journal_read(&self, units: &[String], from: &JournalFrom) -> anyhow::Result<Vec<JournalRecord>>; }`
  - `FakeInner.journal: VecDeque<Result<Vec<JournalRecord>, String>>`；流水 `journal:<units 以逗号连接>:cursor=<c>` / `journal:<units>:since=<RFC3339>`

- [ ] **Step 1: 写失败测试**（追加到 `crates/bui/src/sys/mod.rs` 的 `mod tests` 末尾）

```rust
    fn j0() -> time::OffsetDateTime {
        time::macros::datetime!(2026-09-11 00:00:00 UTC)
    }

    /// `__REALTIME_TIMESTAMP` 是**微秒**的十进制串
    fn us(secs: i64) -> String {
        ((j0().unix_timestamp() + secs) * 1_000_000).to_string()
    }

    /// 真机三种形态：① sing-box 带色输出（含 ESC ⇒ journald 把 MESSAGE 编成字节数组）
    /// ② systemd 关于某单元的消息（`_SYSTEMD_UNIT=init.scope`，单元名在 `UNIT`）
    /// ③ 守护进程自己的 tracing 事件（tracing-journald 0.3.2 默认前缀：`error` 字段落在 `F_ERROR`）
    #[test]
    fn journal_json_decodes_byte_arrays_systemd_messages_and_tracing_error_fields() {
        let units: Vec<String> = ["b-ui-relay", "xray", "b-ui"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let colored = "\x1b[31mERROR\x1b[0m[4006] [\x1b[38;5;48m2302991392\x1b[0m 6.42s] \
                       connection: open connection to www.gstatic.com:443 using \
                       outbound/socks[resi-2]: dial tcp 198.51.100.8:10007: i/o timeout";
        let l1 = serde_json::json!({"__CURSOR": "s=a;i=1", "__REALTIME_TIMESTAMP": us(1),
            "_SYSTEMD_UNIT": "b-ui-relay.service", "MESSAGE": colored.as_bytes()})
        .to_string();
        let l2 = serde_json::json!({"__CURSOR": "s=a;i=2", "__REALTIME_TIMESTAMP": us(2),
            "_SYSTEMD_UNIT": "init.scope", "UNIT": "xray.service",
            "MESSAGE": "xray.service: Start request repeated too quickly."})
        .to_string();
        let l3 = serde_json::json!({"__CURSOR": "s=a;i=3", "__REALTIME_TIMESTAMP": us(3),
            "_SYSTEMD_UNIT": "b-ui.service", "MESSAGE": "用户同步有失败项，下一轮安全网会重试",
            "F_ERROR": "AddUser vless-direct 失败：code: 'The service is currently unavailable'"})
        .to_string();
        // 不在单元集合里 / 非 JSON / 缺 __CURSOR：一律丢掉
        let l4 = serde_json::json!({"__CURSOR": "s=a;i=4", "__REALTIME_TIMESTAMP": us(4),
            "_SYSTEMD_UNIT": "sshd.service", "MESSAGE": "Accepted publickey"})
        .to_string();
        let l6 = serde_json::json!({"__REALTIME_TIMESTAMP": us(5),
            "_SYSTEMD_UNIT": "xray.service", "MESSAGE": "x"})
        .to_string();
        let text = [l1, l2, l3, l4, "-- No entries --".to_string(), l6].join("\n");
        let out = parse_journal_json(&text, &units);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].unit, "b-ui-relay");
        assert_eq!(out[0].cursor, "s=a;i=1");
        assert_eq!(out[0].ts, j0() + time::Duration::seconds(1));
        assert!(
            out[0].message.starts_with(
                "ERROR[4006] [2302991392 6.42s] connection: open connection to www.gstatic.com:443"
            ),
            "{}",
            out[0].message
        );
        assert!(!out[0].message.contains('\x1b'), "色码必须剥掉");
        assert_eq!(out[1].unit, "xray", "systemd 自己的消息按 UNIT 归属");
        assert_eq!(
            out[2].message,
            "用户同步有失败项，下一轮安全网会重试 error=AddUser vless-direct 失败：\
             code: 'The service is currently unavailable'"
        );
    }

    #[test]
    fn journal_args_use_after_cursor_or_an_epoch_since() {
        let units = vec![
            "b-ui-relay".to_string(),
            "hysteria-residential-1".to_string(),
        ];
        assert_eq!(
            journal_args(&units, &JournalFrom::Cursor("s=abc;i=9".into())),
            [
                "--no-pager",
                "-q",
                "-o",
                "json",
                "-u",
                "b-ui-relay.service",
                "-u",
                "hysteria-residential-1.service",
                "--after-cursor",
                "s=abc;i=9"
            ]
            .map(String::from)
            .to_vec()
        );
        let since = journal_args(&units, &JournalFrom::Since(j0()));
        assert_eq!(
            since[since.len() - 2..].to_vec(),
            vec!["--since".to_string(), format!("@{}", j0().unix_timestamp())],
            "systemd.time(7) 的 @<unix 秒> 写法，与时区无关"
        );
    }

    #[test]
    fn the_fake_journal_pops_scripted_batches_and_records_where_it_read_from() {
        let h = fake::FakeHost::new();
        let r = JournalRecord {
            cursor: "c1".into(),
            unit: "xray".into(),
            ts: j0(),
            message: "m".into(),
        };
        h.with(|i| {
            i.journal.push_back(Ok(vec![r.clone()]));
            i.journal.push_back(Err("Failed to seek to cursor".into()));
        });
        let units = vec!["xray".to_string(), "caddy".to_string()];
        assert_eq!(
            h.journal_read(&units, &JournalFrom::Since(j0())).unwrap(),
            vec![r]
        );
        assert!(h
            .journal_read(&units, &JournalFrom::Cursor("c1".into()))
            .is_err());
        assert!(
            h.journal_read(&units, &JournalFrom::Cursor("c1".into()))
                .unwrap()
                .is_empty(),
            "弹空后返回空批"
        );
        assert_eq!(
            h.ops(),
            vec![
                "journal:xray,caddy:since=2026-09-11T00:00:00Z",
                "journal:xray,caddy:cursor=c1",
                "journal:xray,caddy:cursor=c1"
            ]
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui sys::tests:: 2>&1 | tail -5`
Expected: 编译失败——`cannot find function parse_journal_json` / `JournalFrom` / `no field journal on FakeInner`。

- [ ] **Step 3: 把 `strip_ansi` 挪进 `util.rs`**

在 `crates/bui/src/util.rs` 的 `human_duration` 之后追加：

```rust
/// 去掉 ANSI 色码（sing-box 往 journald 写带色输出，R13 §6.2 踩过）。
/// 黑名单的日志候选（`residential::journal`）与日志哨兵的 journald 解析（`sys`）共用这一份。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI 序列：ESC [ 参数… 终止字母
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}
```

在 `crates/bui/src/modules/residential/journal.rs` 里把整个 `pub fn strip_ansi(...) { ... }`（连同它上面那行 `/// 去掉 ANSI 色码…` 文档注释）替换为：

```rust
/// 去掉 ANSI 色码：实现挪到 `crate::util`（日志哨兵的 journald 解析也要用），这里转出来，
/// 既有调用点与测试（`strip_ansi_removes_color_codes_only`）不变。
pub use crate::util::strip_ansi;
```

- [ ] **Step 4: 在 `crates/bui/src/sys/mod.rs` 加类型、纯函数与 trait 方法**

`Host` trait 的最后一个方法 `fn now(&self) -> time::OffsetDateTime;` 之后加：

```rust
    /// 读 `units` 在 `from` 之后的新日志（日志哨兵，spec §5.7）。journalctl 不存在 →
    /// `Err(JOURNALCTL_MISSING)`；游标失效等非零退出 → `Err`（调用方丢游标、从「现在」重来）。
    fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>>;
```

在 `fn cmd_line(...)` 之后加：

```rust
/// 日志哨兵（spec §5.7）读 journald 的起点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalFrom {
    /// 上一轮读到的最后一条的 `__CURSOR`：只读它之后的新条目
    Cursor(String),
    /// 没有游标（首次启动 / 游标失效）：从这一刻读起，**不回放历史**
    Since(time::OffsetDateTime),
}

/// `journalctl -o json` 的一条记录，只留哨兵要的四样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// `__CURSOR`：下一轮 `--after-cursor` 的参数
    pub cursor: String,
    /// 裸单元名（`xray`，不带 `.service`）
    pub unit: String,
    /// `__REALTIME_TIMESTAMP`（微秒）换成的 UTC 时刻
    pub ts: time::OffsetDateTime,
    /// 去掉 ANSI 色码的 `MESSAGE`；带 tracing-journald 的 `F_ERROR` 字段时追加 ` error=<值>`
    pub message: String,
}

/// `journal_read` 在机器上找不到 journalctl 时的错误文案。
pub const JOURNALCTL_MISSING: &str = "机器上没有 journalctl";

/// `journalctl` 的参数：`-o json` 每行一条、`-q` 不打「-- No entries --」、每个单元一个 `-u`；
/// 有游标用 `--after-cursor`，否则 `--since @<unix 秒>`（systemd.time(7) 的 `@` 写法，与时区无关）。
pub fn journal_args(units: &[String], from: &JournalFrom) -> Vec<String> {
    let mut v: Vec<String> = ["--no-pager", "-q", "-o", "json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for u in units {
        v.push("-u".into());
        v.push(unit_full(u));
    }
    match from {
        JournalFrom::Cursor(c) => {
            v.push("--after-cursor".into());
            v.push(c.clone());
        }
        JournalFrom::Since(t) => {
            v.push("--since".into());
            v.push(format!("@{}", t.unix_timestamp()));
        }
    }
    v
}

/// journald 的 JSON 字段值 → 文本。字段里含不可打印字节（sing-box 的 ANSI 色码就是 ESC）时，
/// `journalctl -o json` 把它编成**字节数组**而不是字符串。
fn journal_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(a) => {
            let bytes = a
                .iter()
                .map(|b| b.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect::<Option<Vec<u8>>>()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

fn bare_unit(u: &str) -> String {
    u.strip_suffix(".service").unwrap_or(u).to_string()
}

/// 解析 `journalctl -o json` 的输出（每行一个 JSON 对象）。不在 `units` 里的记录、缺字段的行、
/// 非 JSON 行一律丢掉。单元名先看 `UNIT` 再看 `_SYSTEMD_UNIT`：systemd 自己关于某单元的消息
/// （「Start request repeated too quickly」「restart counter is at 5」）的 `_SYSTEMD_UNIT` 是
/// `init.scope`，单元名在 `UNIT` 里。
pub fn parse_journal_json(stdout: &str, units: &[String]) -> Vec<JournalRecord> {
    let want: BTreeSet<String> = units.iter().map(|u| bare_unit(u)).collect();
    stdout
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let cursor = v.get("__CURSOR")?.as_str()?.to_string();
            let us: i128 = v.get("__REALTIME_TIMESTAMP")?.as_str()?.parse().ok()?;
            let ts = time::OffsetDateTime::from_unix_timestamp_nanos(us * 1000).ok()?;
            let unit = ["UNIT", "_SYSTEMD_UNIT"]
                .iter()
                .filter_map(|k| v.get(*k).and_then(|x| x.as_str()))
                .map(bare_unit)
                .find(|u| want.contains(u))?;
            let mut message = journal_text(v.get("MESSAGE")?)?;
            if let Some(e) = v.get("F_ERROR").and_then(journal_text) {
                message.push_str(" error=");
                message.push_str(&e);
            }
            Some(JournalRecord {
                cursor,
                unit,
                ts,
                message: crate::util::strip_ansi(&message),
            })
        })
        .collect()
}
```

- [ ] **Step 5: `RealHost::journal_read`**（`crates/bui/src/sys/real.rs`，`impl Host for RealHost` 的 `fn now` 之后）

```rust
    fn journal_read(
        &self,
        units: &[String],
        from: &super::JournalFrom,
    ) -> Result<Vec<super::JournalRecord>> {
        if !self.which("journalctl") {
            bail!(super::JOURNALCTL_MISSING);
        }
        let args = super::journal_args(units, from);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run("journalctl", &refs)?;
        if !out.ok() {
            // 游标失效（日志轮转 / vacuum 之后）就是这一支：调用方丢游标、从「现在」重来
            bail!("journalctl 退出码 {}：{}", out.status, out.stderr.trim());
        }
        Ok(super::parse_journal_json(&out.stdout, units))
    }
```

- [ ] **Step 6: `FakeHost` 的队列**（`crates/bui/src/sys/fake.rs`）

`use super::{cmd_line, unit_full, CmdOut, Host, Proto};` 改为：

```rust
use super::{cmd_line, unit_full, CmdOut, Host, JournalFrom, JournalRecord, Proto};
```

`FakeInner` 的 `pub ops: Vec<String>,` 之前加字段：

```rust
    /// `journal_read` 的脚本化返回，按调用顺序逐个弹出；弹空后返回 `Ok(vec![])`。
    /// `Err(文案)` 模拟 journalctl 失败（游标失效 / 没装）。
    pub journal: std::collections::VecDeque<Result<Vec<JournalRecord>, String>>,
```

手写的 `impl Default for FakeInner` 里 `ops: Vec::new(),` 之前加 `journal: std::collections::VecDeque::new(),`。

模块文档的流水清单末尾那一行（`` `run:<program> <args 以空格连接>`。 ``）改为：

```rust
//! `run:<program> <args 以空格连接>`、`journal:<units 以逗号连接>:cursor=<c>` /
//! `journal:<units>:since=<RFC3339>`。
```

`impl Host for FakeHost` 的 `fn now` 之后加：

```rust
    fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>> {
        let from = match from {
            JournalFrom::Cursor(c) => format!("cursor={c}"),
            JournalFrom::Since(t) => format!("since={}", crate::util::fmt_rfc3339(*t)),
        };
        let mut i = self.lock();
        i.ops.push(format!("journal:{}:{from}", units.join(",")));
        match i.journal.pop_front() {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(anyhow::anyhow!(e)),
            None => Ok(Vec::new()),
        }
    }
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p bui sys:: 2>&1 | tail -5 && cargo test -p bui residential::journal 2>&1 | tail -3`
Expected: 全部 PASS（`strip_ansi_removes_color_codes_only` 经 re-export 照样通过）。

- [ ] **Step 8: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 9: Commit**

```bash
git add crates/bui/src/util.rs crates/bui/src/modules/residential/journal.rs \
        crates/bui/src/sys/mod.rs crates/bui/src/sys/real.rs crates/bui/src/sys/fake.rs
git commit -m "$(cat <<'EOF'
feat(sys): Host::journal_read——journald 增量读取原语（日志哨兵 §5.7）

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 2: 签名表（纯函数）+ 行级脱敏 + 两处判据抽出

**Files:**
- Create: `crates/bui/src/modules/sentinel/mod.rs`
- Create: `crates/bui/src/modules/sentinel/signature.rs`
- Modify: `crates/bui/src/modules/mod.rs`（`pub mod sentinel;`）
- Modify: `crates/bui/src/redact.rs`（`line()` + 测试）
- Modify: `crates/bui/src/modules/watchdog.rs`（抽出 `is_auth_http_failure`，`count_auth_http_failures` 改为调用它）
- Modify: `crates/bui/src/modules/panel/users.rs`（`USER_SYNC_FAILED_LOG` 常量，`report` 用它）

**Interfaces:**
- Consumes: `crate::modules::residential::MEMBER_PREFIX`（`"resi-"`）、`bui_schema::render::hysteria::AUTH_HTTP_PORT`。
- Produces:
  - `crate::redact::line(&str) -> String`
  - `crate::modules::watchdog::is_auth_http_failure(&str) -> bool`
  - `crate::modules::panel::users::USER_SYNC_FAILED_LOG: &str = "用户同步有失败项，下一轮安全网会重试"`
  - `crate::modules::sentinel::signature::{Sig, Action, Rule, Match, classify, parse_relay, is_crash_loop, detail_of, DETAIL_MAX, CRASH_LOOP_RESTARTS, BIND_IN_USE}`
  - `Sig::{id() -> &'static str, action() -> Action, rule() -> Rule}`；`Action::id() -> &'static str`
  - `Sig` 的 `id()`：`relay_upstream_error` / `relay_google_blocked` / `hy2_auth_http_failed` / `kernel_bind_in_use` / `kernel_crash_loop` / `xray_grpc_unavailable` / `caddy_cert_failed`
  - `Action` 的 `id()`：`probe_and_borrow` / `verify_google_and_borrow` / `alert` / `delegate_watchdog` / `retry_user_sync`
  - `Match { sig: Sig, subject: String, detail: String }`（relay 的 `subject` 是成员 tag，调用方换 uuid）

- [ ] **Step 1: 写 `redact::line` 的失败测试**（`crates/bui/src/redact.rs` 的 `mod tests` 末尾）

```rust
    /// 一整行日志里每个带 userinfo 的片段都要脱敏（哨兵把日志原文放进事件的 `sample`）
    #[test]
    fn line_redacts_every_credential_bearing_token() {
        // 密码夹具用唯一串：写成 `p2` 会误中脱敏后照样保留的 `isp2.example.net`
        let s = "dial socks5://user1:pw1@isp1.example.net:10007 failed, retry \
                 \"http://u2:pw2@isp2.example.net:44445/x\" and user1:pw1@isp3.example.net:10007";
        let out = line(s);
        assert!(!out.contains("pw1") && !out.contains("pw2"), "{out}");
        assert!(out.contains("socks5://***:***@isp1.example.net:10007"), "{out}");
        assert!(out.contains("\"http://***:***@isp2.example.net:44445/x\""), "{out}");
        assert!(out.contains("***:***@isp3.example.net:10007"), "{out}");
        assert_eq!(line("plain text, no creds"), "plain text, no creds");
    }
```

- [ ] **Step 2: 写签名表的失败测试**

新建 `crates/bui/src/modules/sentinel/signature.rs`，先只放测试模块（实现留到 Step 5）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// relay 行的真实前缀（R13 §6.2 夹具，bwg-tizi `journalctl -u b-ui-relay -o cat`，色码已剥）
    fn relay(target: &str, out: &str, reason: &str) -> String {
        format!(
            "ERROR[4006] [2302991392 6.42s] connection: open connection to {target} using \
             outbound/{out}: {reason}"
        )
    }

    fn sig_of(unit: &str, msg: &str) -> Option<(Sig, String)> {
        classify(unit, msg).map(|m| (m.sig, m.subject))
    }

    #[test]
    fn upstream_side_failures_are_relay_upstream_errors_keyed_by_member_tag() {
        for (out, reason) in [
            ("socks[resi-2]", "dial tcp 198.51.100.8:10007: connect: connection refused"),
            ("socks[resi-2]", "dial tcp 198.51.100.8:10007: i/o timeout"),
            ("http[resi-3]", "context deadline exceeded"),
            ("http[resi-1]", "unexpected status: 407 Proxy Authentication Required"),
            ("socks[resi-1]", "socks5: incorrect user name or password"),
            ("http[resi-1]", "dial tcp: lookup isp1.example.net: no route to host"),
        ] {
            let msg = relay("www.gstatic.com:443", out, reason);
            let tag = out.split_once('[').unwrap().1.trim_end_matches(']');
            assert_eq!(
                sig_of("b-ui-relay", &msg),
                Some((Sig::RelayUpstreamError, tag.to_string())),
                "{msg}"
            );
        }
        // 端口号里含 407 的超时仍是连不上（不是凭据失效），但签名相同、对象相同
        let msg = relay("a.example.com:443", "http[resi-2]", "dial tcp 198.51.100.8:4070: i/o timeout");
        assert_eq!(sig_of("b-ui-relay", &msg), Some((Sig::RelayUpstreamError, "resi-2".into())));
    }

    /// 「上游拒绝了**这个目标**」归黑名单（spec §5.4），哨兵不计：否则一个被拒的支付域名
    /// 就能把整条上游判死。
    #[test]
    fn target_side_rejections_belong_to_the_blacklist_not_the_sentinel() {
        for (target, out, reason) in [
            ("gateway.icloud.com:443", "http[resi-1]", "unexpected status: 403 Forbidden"),
            ("198.51.100.9:5228", "http[resi-1]", "unexpected status: 403 Forbidden"),
            ("a.example.com:443", "http[resi-1]", "unexpected status: 502 Bad Gateway"),
            ("x.com:443", "socks[resi-2]", "socks5: connection not allowed by ruleset"),
            ("x.com:443", "socks[resi-2]", "socks5: connection refused"),
            // direct 出站与住宅无关
            ("a.example.com:443", "direct[direct]", "dial tcp 198.51.100.9:443: i/o timeout"),
        ] {
            let msg = relay(target, out, reason);
            assert_eq!(sig_of("b-ui-relay", &msg), None, "{msg}");
        }
        assert_eq!(sig_of("b-ui-relay", "inbound/socks[slot-1]: inbound connection from 127.0.0.1:41234"), None);
    }

    #[test]
    fn a_serp_403_or_a_403_on_google_search_is_google_blocked() {
        let serp = relay("www.google.com:443", "http[resi-1]", "unexpected status: 403 Forbidden serp domain");
        assert_eq!(sig_of("b-ui-relay", &serp), Some((Sig::RelayGoogleBlocked, "resi-1".into())));
        let bare = relay("www.google.com.hk:443", "socks[resi-3]", "unexpected status: 403 Forbidden");
        assert_eq!(sig_of("b-ui-relay", &bare), Some((Sig::RelayGoogleBlocked, "resi-3".into())));
        let gateway = relay("www.google.com:443", "http[resi-1]", "unexpected status: 502 Bad Gateway");
        assert_eq!(sig_of("b-ui-relay", &gateway), None, "5xx 不是封 Google");
    }

    #[test]
    fn hysteria_auth_endpoint_failures_are_their_own_signature() {
        let line = "hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": \
                    dial tcp 127.0.0.1:18789: connect: connection refused\"}";
        assert_eq!(sig_of("hysteria-server", line), Some((Sig::Hy2AuthHttpFailed, "hysteria-server".into())));
        assert_eq!(
            sig_of("hysteria-residential-2", line),
            Some((Sig::Hy2AuthHttpFailed, "hysteria-residential-2".into()))
        );
        // hysteria 自己的出站错误（住宅实例连 relay）不是鉴权失败
        let outbound = "hysteria[1]: TCP error {\"error\": \"dial tcp 127.0.0.1:2081: connect: connection refused\"}";
        assert_eq!(sig_of("hysteria-residential-1", outbound), None);
        assert_eq!(sig_of("xray", line), None, "鉴权签名只认 hysteria 单元");
    }

    #[test]
    fn bind_conflicts_and_crash_loops_are_recognised_on_kernel_units_only() {
        let hy = "FATAL\tfailed to load server config\t{\"error\": \"invalid config: listen: \
                  listen udp :10000: bind: address already in use\"}";
        assert_eq!(sig_of("hysteria-server", hy), Some((Sig::KernelBindInUse, "hysteria-server".into())));
        let xr = "Failed to start: main: failed to start server > app/proxyman/inbound: failed to \
                  listen TCP on 10001 > listen tcp 0.0.0.0:10001: bind: address already in use";
        assert_eq!(sig_of("xray", xr), Some((Sig::KernelBindInUse, "xray".into())));
        assert_eq!(
            sig_of("xray", "xray.service: Start request repeated too quickly."),
            Some((Sig::KernelCrashLoop, "xray".into()))
        );
        assert_eq!(
            sig_of("b-ui-relay", "b-ui-relay.service: Scheduled restart job, restart counter is at 5."),
            Some((Sig::KernelCrashLoop, "b-ui-relay".into()))
        );
        assert_eq!(
            sig_of("hysteria-server", "hysteria-server.service: Scheduled restart job, restart counter is at 2."),
            None,
            "偶发一两次重启不算崩溃循环"
        );
        assert_eq!(sig_of("caddy", hy), None, "bind 签名只认内核单元");
    }

    #[test]
    fn a_failed_user_sync_with_unavailable_is_the_xray_grpc_signature() {
        use crate::modules::panel::users::USER_SYNC_FAILED_LOG;
        let hit = format!(
            "{USER_SYNC_FAILED_LOG} error=AddUser vless-direct 失败：code: 'The service is \
             currently unavailable', message: \"tcp connect error\""
        );
        assert_eq!(sig_of("b-ui", &hit), Some((Sig::XrayGrpcUnavailable, "xray".into())));
        let other = format!("{USER_SYNC_FAILED_LOG} error=AddUser vless-direct 失败：fake");
        assert_eq!(sig_of("b-ui", &other), None, "不是 Unavailable 的同步失败不算");
        assert_eq!(sig_of("b-ui", "Clash API unavailable, retry later"), None, "别的 b-ui 行不算");
    }

    #[test]
    fn caddy_certificate_failures_name_the_domain() {
        let a = r#"{"level":"error","ts":1757548800.1,"logger":"tls.obtain","msg":"could not get certificate from issuer","identifier":"panel.example.com","issuer":"acme-v02.api.letsencrypt.org-directory","error":"HTTP 429 urn:ietf:params:acme:error:rateLimited"}"#;
        assert_eq!(sig_of("caddy", a), Some((Sig::CaddyCertFailed, "panel.example.com".into())));
        let b = r#"{"level":"error","ts":1757548800.2,"logger":"tls","msg":"job failed","error":"panel.example.com: obtaining certificate: [panel.example.com] Obtain: solving challenge"}"#;
        assert_eq!(sig_of("caddy", b), Some((Sig::CaddyCertFailed, "panel.example.com".into())));
        let info = r#"{"level":"info","ts":1757548800.3,"logger":"tls.obtain","msg":"obtaining certificate","identifier":"panel.example.com"}"#;
        assert_eq!(sig_of("caddy", info), None, "info 级的「开始签」不是失败");
    }

    #[test]
    fn rules_and_actions_match_the_spec() {
        assert_eq!(Sig::RelayUpstreamError.rule(), Rule { threshold: 3, window_secs: 60 });
        assert_eq!(Sig::Hy2AuthHttpFailed.rule(), Rule { threshold: 3, window_secs: 60 });
        assert_eq!(Sig::XrayGrpcUnavailable.rule(), Rule { threshold: 2, window_secs: 150 });
        for s in [Sig::RelayGoogleBlocked, Sig::KernelBindInUse, Sig::KernelCrashLoop, Sig::CaddyCertFailed] {
            assert_eq!(s.rule().threshold, 1, "{s:?}");
        }
        assert_eq!(Sig::RelayUpstreamError.action().id(), "probe_and_borrow");
        assert_eq!(Sig::RelayGoogleBlocked.action().id(), "verify_google_and_borrow");
        assert_eq!(Sig::Hy2AuthHttpFailed.action().id(), "alert");
        assert_eq!(Sig::CaddyCertFailed.action().id(), "alert");
        assert_eq!(Sig::KernelBindInUse.action().id(), "delegate_watchdog");
        assert_eq!(Sig::KernelCrashLoop.action().id(), "delegate_watchdog");
        assert_eq!(Sig::XrayGrpcUnavailable.action().id(), "retry_user_sync");
        assert_eq!(Sig::RelayUpstreamError.id(), "relay_upstream_error", "演练脚本按这个串认事件");
    }

    #[test]
    fn details_are_redacted_before_they_are_clipped() {
        let msg = format!(
            "{} socks5://user1:pw1@isp1.example.net:10007",
            relay("a.example.com:443", "socks[resi-1]", "dial tcp 198.51.100.7:10007: i/o timeout")
        );
        let m = classify("b-ui-relay", &msg).unwrap();
        assert!(!m.detail.contains("pw1"), "{}", m.detail);
        let long = relay(&format!("{}.example.com:443", "a".repeat(600)), "http[resi-1]", "context deadline exceeded");
        assert!(classify("b-ui-relay", &long).unwrap().detail.chars().count() <= DETAIL_MAX);
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

先把模块挂上：`crates/bui/src/modules/mod.rs` 按字母序在 `pub mod residential;` 之后加 `pub mod sentinel;`；新建 `crates/bui/src/modules/sentinel/mod.rs`：

```rust
//! 日志哨兵与预案（spec §5.7，2026-09-13 主理人：「bui 需要监听后台日志，如果出现报错，要立马处理，
//! 比如某个住宅代理 ip 连不通了，就要启用预案，立马分配新的 ip」）。
//!
//! 结构：`signature`（一行日志 → 签名，纯函数）→ `engine`（去抖 / 冷却）→ 预案（`resi` 住宅、
//! `system` 系统）→ `incidents`（事件落 `runtime.extra["incidents"]`）；`run` 是主循环。
//! 边界：只做「探测 → 借用 / 重试 / 告警」，**不增删池成员、不做切回**（切回归巡检的 `drive_slots`）。

pub mod signature;
```

Run: `cargo test -p bui sentinel::signature 2>&1 | tail -5; cargo test -p bui redact 2>&1 | tail -5`
Expected: 编译失败——`cannot find function classify` / `line` / `USER_SYNC_FAILED_LOG`。

- [ ] **Step 4: 实现 `redact::line`、`is_auth_http_failure`、`USER_SYNC_FAILED_LOG`**

`crates/bui/src/redact.rs`，`pub fn secret` 之前加：

```rust
/// 整行日志脱敏：按空格切分，含 `@` 的片段逐个过 [`url_credentials`]（`socks5://u:p@h:port`、
/// `"http://u:p@h/x"`、`u:p@h:port` 都能认）。日志哨兵把日志原文放进事件的 `sample` 之前必须过它。
pub fn line(s: &str) -> String {
    s.split(' ')
        .map(|tok| {
            if tok.contains('@') {
                url_credentials(tok)
            } else {
                tok.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
```

`crates/bui/src/modules/watchdog.rs`：把现有的 `count_auth_http_failures` 整个函数（含文档注释）替换为：

```rust
/// 一行 journal 是不是「鉴权请求连不上 / 超时」（纯函数）。日志哨兵（`modules::sentinel`）逐行用它。
///
/// 两类标记都要命中才算：只匹配 "connection refused" 会把住宅上游、relay 的连接错误
/// 一起数进来；只匹配 "/auth" 会把正常的鉴权日志数进来。
pub fn is_auth_http_failure(line: &str) -> bool {
    let port = format!(":{}", bui_schema::render::hysteria::AUTH_HTTP_PORT);
    let lower = line.to_ascii_lowercase();
    (lower.contains(AUTH_HTTP_PATH_MARKER) || lower.contains(&port))
        && AUTH_HTTP_FAIL_MARKERS.iter().any(|m| lower.contains(m))
}

/// 数一段 journal 里「鉴权请求连不上 / 超时」的行数（纯函数，便于单测）。
pub fn count_auth_http_failures(log: &str) -> u32 {
    log.lines().filter(|l| is_auth_http_failure(l)).count() as u32
}
```

`crates/bui/src/modules/panel/users.rs`：在 `pub const SYNC_INTERVAL_SECS: u64 = 60;` 之后加：

```rust
/// 用户同步有失败项时那一行日志的固定文案。日志哨兵（`modules::sentinel::signature`）按它加
/// tracing-journald 的 `F_ERROR` 字段认「xray gRPC 连续不可用」——改文案会让哨兵失明，
/// 所以它是常量，哨兵的测试直接引用它。
pub const USER_SYNC_FAILED_LOG: &str = "用户同步有失败项，下一轮安全网会重试";
```

并把 `fn report` 里的

```rust
        tracing::warn!(error = %e, "用户同步有失败项，下一轮安全网会重试");
```

改为

```rust
        tracing::warn!(error = %e, "{}", USER_SYNC_FAILED_LOG);
```

- [ ] **Step 5: 实现签名表**（`crates/bui/src/modules/sentinel/signature.rs`，写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 日志哨兵的签名表（spec §5.7）：一行日志 → 哪一类故障、落在哪个对象上、触发门槛多少。
//! **纯函数**，不碰机器；fixtures 用真机原文。
//!
//! 边界（设计裁决 D4）：relay 里「上游拒绝了**这个目标**」（`unexpected status: 4xx/5xx`、SOCKS5
//! 的 REP 拒绝）归黑名单（spec §5.4，`residential::journal::parse_line`），哨兵不碰；哨兵只认
//! 「**上游本身**不能用了」（连不上 / 超时 / 凭据失效）与「上游对 Google 搜索整域拒绝」。

use crate::modules::residential::MEMBER_PREFIX;

/// 签名。`id()` 是事件、面板与演练脚本共用的稳定字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sig {
    /// relay → 某上游：connection refused / i/o timeout / deadline exceeded / 407 / SOCKS5 认证被拒
    RelayUpstreamError,
    /// relay → 某上游：`unexpected status: 403 … serp …`，或对 Google 搜索域名的 403
    RelayGoogleBlocked,
    /// hysteria 连不上 http 鉴权端口（spec §3.2）
    Hy2AuthHttpFailed,
    /// hysteria / xray：`bind: address already in use`
    KernelBindInUse,
    /// systemd：`Start request repeated too quickly` / `restart counter is at N`（N ≥ [`CRASH_LOOP_RESTARTS`]）
    KernelCrashLoop,
    /// 守护进程自己的日志：用户同步的 xray gRPC 调用 Unavailable（设计裁决 D10）
    XrayGrpcUnavailable,
    /// caddy：签证书失败
    CaddyCertFailed,
}

/// 签名对应的预案（冷却按「动作 + 对象」计，设计裁决 D12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ProbeAndBorrow,
    VerifyGoogleAndBorrow,
    Alert,
    DelegateWatchdog,
    RetryUserSync,
}

/// 触发门槛：`window_secs` 秒内同签名同对象累计 `threshold` 条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub threshold: usize,
    pub window_secs: i64,
}

impl Sig {
    pub fn id(self) -> &'static str {
        match self {
            Sig::RelayUpstreamError => "relay_upstream_error",
            Sig::RelayGoogleBlocked => "relay_google_blocked",
            Sig::Hy2AuthHttpFailed => "hy2_auth_http_failed",
            Sig::KernelBindInUse => "kernel_bind_in_use",
            Sig::KernelCrashLoop => "kernel_crash_loop",
            Sig::XrayGrpcUnavailable => "xray_grpc_unavailable",
            Sig::CaddyCertFailed => "caddy_cert_failed",
        }
    }

    pub fn action(self) -> Action {
        match self {
            Sig::RelayUpstreamError => Action::ProbeAndBorrow,
            Sig::RelayGoogleBlocked => Action::VerifyGoogleAndBorrow,
            Sig::Hy2AuthHttpFailed | Sig::CaddyCertFailed => Action::Alert,
            Sig::KernelBindInUse | Sig::KernelCrashLoop => Action::DelegateWatchdog,
            Sig::XrayGrpcUnavailable => Action::RetryUserSync,
        }
    }

    pub fn rule(self) -> Rule {
        match self {
            Sig::RelayUpstreamError => Rule { threshold: 3, window_secs: 60 },
            Sig::Hy2AuthHttpFailed => Rule {
                threshold: crate::modules::watchdog::AUTH_HTTP_FAIL_THRESHOLD as usize,
                window_secs: 60,
            },
            // 用户同步的安全网 60 秒一轮：「连续两轮失败」落在 150 秒窗口里（设计裁决 D10）
            Sig::XrayGrpcUnavailable => Rule { threshold: 2, window_secs: 150 },
            // 一条就说明问题：403 serp 是明确的上游策略，bind / 崩溃循环 / 证书失败都不会「偶发」
            Sig::RelayGoogleBlocked
            | Sig::KernelBindInUse
            | Sig::KernelCrashLoop
            | Sig::CaddyCertFailed => Rule { threshold: 1, window_secs: 60 },
        }
    }
}

impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Action::ProbeAndBorrow => "probe_and_borrow",
            Action::VerifyGoogleAndBorrow => "verify_google_and_borrow",
            Action::Alert => "alert",
            Action::DelegateWatchdog => "delegate_watchdog",
            Action::RetryUserSync => "retry_user_sync",
        }
    }
}

/// 一行日志的匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub sig: Sig,
    /// 对象在日志里的名字：relay 是成员 tag（`resi-2`，**位置键**，调用方当场经
    /// `clash::id_of_tag` 换成 uuid 再计数）、caddy 是域名、xray gRPC 是 `xray`、其余是单元名
    pub subject: String,
    /// 先脱敏再截断的原文，进事件的 `sample`
    pub detail: String,
}

/// `sample` 的长度上限（按字符）
pub const DETAIL_MAX: usize = 240;
/// `restart counter is at N` 里 N 到多少算崩溃循环
pub const CRASH_LOOP_RESTARTS: u32 = 5;
pub const BIND_IN_USE: &str = "bind: address already in use";
/// 「凭据失效」（reason 小写）。`407` 只认状态行开头，免得把端口 4070 当成 407
const AUTH_MARKERS: [&str; 3] = [
    "proxy authentication",
    "incorrect user name or password",
    "username/password authentication failed",
];
/// 「到上游本身连不上 / 超时」（reason 小写）
const UNREACHABLE_MARKERS: [&str; 5] = [
    "connection refused",
    "i/o timeout",
    "deadline exceeded",
    "no route to host",
    "network is unreachable",
];

/// 一行日志（`unit` 是裸单元名，`message` 已剥色码）→ 签名。不认识的一律 `None`。
pub fn classify(unit: &str, message: &str) -> Option<Match> {
    let kernel = unit == "xray" || unit == "b-ui-relay" || unit.starts_with("hysteria-");
    if kernel && is_crash_loop(message) {
        return Some(hit(Sig::KernelCrashLoop, unit, message));
    }
    match unit {
        "b-ui-relay" => relay(message),
        "caddy" => caddy(message),
        "b-ui" => xray_grpc(message),
        _ if kernel => {
            if unit.starts_with("hysteria-")
                && crate::modules::watchdog::is_auth_http_failure(message)
            {
                return Some(hit(Sig::Hy2AuthHttpFailed, unit, message));
            }
            message
                .contains(BIND_IN_USE)
                .then(|| hit(Sig::KernelBindInUse, unit, message))
        }
        _ => None,
    }
}

fn hit(sig: Sig, subject: &str, message: &str) -> Match {
    Match {
        sig,
        subject: subject.to_string(),
        detail: detail_of(message),
    }
}

/// 进事件的原文：**先脱敏再截断**（先截断可能把 `socks5://user1:pw1@…` 截成不带 `@` 的半截，
/// 脱敏就认不出来了）。
pub fn detail_of(message: &str) -> String {
    crate::redact::line(message)
        .chars()
        .take(DETAIL_MAX)
        .collect()
}

/// systemd 的崩溃循环判据
pub fn is_crash_loop(message: &str) -> bool {
    if message.contains("Start request repeated too quickly") {
        return true;
    }
    message
        .split_once("restart counter is at ")
        .and_then(|(_, n)| n.trim().trim_end_matches('.').parse::<u32>().ok())
        .is_some_and(|n| n >= CRASH_LOOP_RESTARTS)
}

/// `open connection to <host>:<port> using outbound/<http|socks>[resi-N]: <reason>` →
/// `(tag, host, port, reason)`；不是住宅出站的行一律 `None`（与 `residential::journal::parse_line`
/// 同一形态，但那边只收「上游拒绝了目标」）。
pub fn parse_relay(message: &str) -> Option<(String, String, u16, String)> {
    let rest = message.split_once("open connection to ")?.1;
    let (target, rest) = rest.split_once(" using outbound/")?;
    let (host, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let (kind_tag, reason) = rest.split_once("]: ")?;
    let (kind, tag) = kind_tag.split_once('[')?;
    if !matches!(kind, "http" | "socks") || !tag.starts_with(MEMBER_PREFIX) || host.is_empty() {
        return None;
    }
    Some((tag.to_string(), host.to_string(), port, reason.to_string()))
}

fn is_google_search_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "google.com" || h.starts_with("www.google.")
}

fn relay(message: &str) -> Option<Match> {
    let (tag, host, _port, reason) = parse_relay(message)?;
    let lower = reason.to_ascii_lowercase();
    let m = |sig: Sig| Some(hit(sig, &tag, message));
    // ① 凭据失效：整条上游都不能用（调研 §D），与连不上同一个预案
    if lower.starts_with("unexpected status: 407") || AUTH_MARKERS.iter().any(|k| lower.contains(k)) {
        return m(Sig::RelayUpstreamError);
    }
    if let Some(s) = lower.strip_prefix("unexpected status: ") {
        // ② Google 搜索被上游整域拒绝（Bright Data 的 `403 Forbidden serp domain`）
        if s.starts_with("403") && (s.contains("serp") || is_google_search_host(&host)) {
            return m(Sig::RelayGoogleBlocked);
        }
        // 其余状态码：上游拒绝了这个目标，黑名单的地盘
        return None;
    }
    // ③ SOCKS5 的 REP 拒绝同样是目标级（凭据失效已在 ① 截走）
    if lower.starts_with("socks5:") {
        return None;
    }
    // ④ 到上游本身连不上 / 超时
    if UNREACHABLE_MARKERS.iter().any(|k| lower.contains(k)) {
        return m(Sig::RelayUpstreamError);
    }
    None
}

fn caddy(message: &str) -> Option<Match> {
    let lower = message.to_ascii_lowercase();
    let about_cert =
        lower.contains("obtaining certificate") || lower.contains("could not get certificate");
    if !about_cert || !lower.contains("\"level\":\"error\"") {
        return None;
    }
    let v: Option<serde_json::Value> = serde_json::from_str(message).ok();
    let field = |k: &str| {
        v.as_ref()
            .and_then(|v| v.get(k))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };
    let domain = field("identifier")
        .or_else(|| {
            field("error").and_then(|e| {
                e.split_once(": obtaining certificate")
                    .map(|(d, _)| d.trim().to_string())
            })
        })
        .unwrap_or_else(|| "caddy".into());
    Some(Match {
        sig: Sig::CaddyCertFailed,
        subject: domain,
        detail: detail_of(message),
    })
}

fn xray_grpc(message: &str) -> Option<Match> {
    if !message.contains(crate::modules::panel::users::USER_SYNC_FAILED_LOG) {
        return None;
    }
    // tonic 0.14：`code: 'The service is currently unavailable'`；旧格式是 `status: Unavailable`
    message
        .to_ascii_lowercase()
        .contains("unavailable")
        .then(|| hit(Sig::XrayGrpcUnavailable, "xray", message))
}
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p bui sentinel::signature 2>&1 | tail -5 && cargo test -p bui redact 2>&1 | tail -3 && cargo test -p bui watchdog 2>&1 | tail -3 && cargo test -p bui panel::users 2>&1 | tail -3`
Expected: 全部 PASS（`only_lines_about_the_auth_endpoint_failing_are_counted` 不改照过）。

- [ ] **Step 7: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 8: Commit**

```bash
git add crates/bui/src/modules/mod.rs crates/bui/src/modules/sentinel/mod.rs \
        crates/bui/src/modules/sentinel/signature.rs crates/bui/src/redact.rs \
        crates/bui/src/modules/watchdog.rs crates/bui/src/modules/panel/users.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 日志签名表（纯函数）与行级脱敏

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 3: 去抖 / 冷却状态机 + 事件环形存储

**Files:**
- Create: `crates/bui/src/modules/sentinel/engine.rs`
- Create: `crates/bui/src/modules/sentinel/incidents.rs`
- Modify: `crates/bui/src/modules/sentinel/mod.rs`（两个 `pub mod` + 三个常量）

**Interfaces:**
- Consumes: Task 2 的 `signature::Sig`（`id()` / `rule()` / `action()`）；P1 的 `state::runtime::{Runtime, RuntimeData}`（`extra` 扩展位）；`util::{fmt_rfc3339, parse_rfc3339}`。
- Produces:
  - `sentinel::{DEBOUNCE_SECS: i64 = 60, ACTION_COOLDOWN_SECS: i64 = 600, INCIDENTS_MAX: usize = 200}`
  - `engine::Engine`（`Default`）：`observe(&mut self, Sig, subject: &str, ts: OffsetDateTime, now: OffsetDateTime) -> bool`、`prune(&mut self, now)`
  - `engine::{sig_key(Sig, &str) -> String, action_key(Sig, &str) -> String, in_cooldown(&BTreeMap<String, String>, &str, OffsetDateTime) -> bool, prune_acted(&mut BTreeMap<String, String>, OffsetDateTime)}`
  - `engine::SentinelRuntime { cursor: Option<String>, since: Option<String>, acted: BTreeMap<String, String>, down_since: BTreeMap<Uuid, String>, suggested: BTreeSet<Uuid> }`（`Clone, Default, PartialEq, Serialize, Deserialize, Debug`）
  - `engine::{SENTINEL_KEY = "sentinel", sentinel_of(&RuntimeData) -> SentinelRuntime, put_sentinel(&mut RuntimeData, &SentinelRuntime)}`
  - `incidents::{INCIDENTS_KEY = "incidents", Level::{Info, Warn, Error}, Incident { at, unit, signature, subject, action, result, level, sample: Option<String> }, from_runtime(&RuntimeData) -> Vec<Incident>, push(&mut RuntimeData, Incident)}`

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/sentinel/engine.rs`（先只放测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn at(s: i64) -> OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC) + Duration::seconds(s)
    }

    /// 喂一条、当下时刻就是日志时刻（实时流的常态）
    fn feed(e: &mut Engine, sig: Sig, subject: &str, s: i64) -> bool {
        e.observe(sig, subject, at(s), at(s))
    }

    #[test]
    fn three_hits_inside_the_window_fire_exactly_once() {
        let mut e = Engine::default();
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", 0));
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", 1));
        assert!(feed(&mut e, Sig::RelayUpstreamError, "u2", 2), "第 3 条触发");
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", 3), "触发后计数清零");
    }

    #[test]
    fn hits_spread_wider_than_the_window_never_add_up() {
        let mut e = Engine::default();
        for s in [0, 40, 80, 120, 160] {
            assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", s), "t={s}");
        }
    }

    #[test]
    fn a_fired_signature_is_debounced_for_sixty_seconds() {
        let mut e = Engine::default();
        for s in 0..3 {
            feed(&mut e, Sig::RelayUpstreamError, "u2", s);
        }
        for s in 10..13 {
            assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", s), "去抖窗口内 t={s}");
        }
        assert!(
            feed(&mut e, Sig::RelayUpstreamError, "u2", 63),
            "距上次触发 ≥ 60 秒、窗口内仍有 ≥3 条 ⇒ 再触发"
        );
    }

    /// 重启后续读到的积压（设计裁决 D12）：描述的是过去，不许在此刻触发动作
    #[test]
    fn stale_backlog_older_than_the_window_is_ignored() {
        let mut e = Engine::default();
        for s in [-300, -299, -298] {
            assert!(!e.observe(Sig::RelayUpstreamError, "u2", at(s), at(0)));
        }
        assert!(!e.observe(Sig::KernelBindInUse, "xray", at(-61), at(0)), "门槛 1 的签名也一样");
        assert!(e.observe(Sig::KernelBindInUse, "xray", at(-5), at(0)));
    }

    #[test]
    fn subjects_and_signatures_count_separately() {
        let mut e = Engine::default();
        feed(&mut e, Sig::RelayUpstreamError, "u2", 0);
        feed(&mut e, Sig::RelayUpstreamError, "u2", 1);
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u3", 2), "别的上游不帮 u2 凑数");
        assert!(!feed(&mut e, Sig::Hy2AuthHttpFailed, "u2", 2), "别的签名也不凑数");
        assert!(feed(&mut e, Sig::CaddyCertFailed, "panel.example.com", 3), "门槛 1：一条即触发");
    }

    #[test]
    fn xray_grpc_needs_two_within_one_hundred_fifty_seconds() {
        let mut e = Engine::default();
        assert!(!feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 0));
        assert!(feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 60), "安全网连续两轮失败");
        let mut e = Engine::default();
        feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 0);
        assert!(!feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 200));
    }

    #[test]
    fn prune_forgets_old_counts_and_expired_debounces() {
        let mut e = Engine::default();
        feed(&mut e, Sig::RelayUpstreamError, "u2", 0);
        feed(&mut e, Sig::CaddyCertFailed, "panel.example.com", 0);
        e.prune(at(1000));
        assert!(e.hits.is_empty() && e.fired.is_empty(), "常驻进程里不许越攒越多");
    }

    #[test]
    fn cooldown_lasts_ten_minutes_and_survives_a_clock_step_back() {
        let key = action_key(Sig::RelayUpstreamError, "u2");
        assert_eq!(key, "probe_and_borrow|u2");
        let mut acted = BTreeMap::new();
        acted.insert(key.clone(), fmt_rfc3339(at(0)));
        assert!(in_cooldown(&acted, &key, at(599)));
        assert!(!in_cooldown(&acted, &key, at(600)));
        assert!(!in_cooldown(&acted, &key, at(-30)), "时钟回跳不能把冷却锁死");
        assert!(!in_cooldown(&acted, "alert|xray", at(1)));
        prune_acted(&mut acted, at(700));
        assert!(acted.is_empty());
    }

    #[tokio::test]
    async fn the_sentinel_section_round_trips_through_runtime_json() {
        let d = tempfile::tempdir().unwrap();
        let rt = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        let mut s = SentinelRuntime {
            cursor: Some("s=abc;i=9".into()),
            since: Some(fmt_rfc3339(at(0))),
            ..Default::default()
        };
        s.acted.insert("alert|xray".into(), fmt_rfc3339(at(1)));
        s.down_since.insert(Uuid::from_u128(2), fmt_rfc3339(at(2)));
        s.suggested.insert(Uuid::from_u128(2));
        let snap = s.clone();
        rt.update(move |r| put_sentinel(r, &snap)).await;
        let back = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        assert_eq!(sentinel_of(&back.read().await), s);
        assert_eq!(
            sentinel_of(&crate::state::runtime::RuntimeData::default()),
            SentinelRuntime::default()
        );
    }
}
```

`crates/bui/src/modules/sentinel/incidents.rs`（先只放测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn inc(n: usize) -> Incident {
        Incident {
            at: format!("2026-09-11T00:00:{:02}Z", n % 60),
            unit: "b-ui-relay".into(),
            signature: "relay_upstream_error".into(),
            subject: "isp2.example.net:10007".into(),
            action: "probe_and_borrow".into(),
            result: format!("第 {n} 条"),
            level: Level::Error,
            sample: None,
        }
    }

    #[test]
    fn push_keeps_the_newest_first_and_caps_the_ring() {
        let mut rt = RuntimeData::default();
        for n in 0..(INCIDENTS_MAX + 5) {
            push(&mut rt, inc(n));
        }
        let v = from_runtime(&rt);
        assert_eq!(v.len(), INCIDENTS_MAX);
        assert_eq!(v[0].result, format!("第 {} 条", INCIDENTS_MAX + 4), "新的在前");
        assert_eq!(v[INCIDENTS_MAX - 1].result, "第 5 条", "最旧的 5 条被挤掉");
    }

    #[test]
    fn a_corrupt_section_reads_as_no_incidents() {
        let mut rt = RuntimeData::default();
        rt.extra
            .insert(INCIDENTS_KEY.into(), serde_json::json!({"not": "a list"}));
        assert!(from_runtime(&rt).is_empty());
    }

    #[test]
    fn the_wire_shape_has_a_lowercase_level_and_omits_an_empty_sample() {
        let v = serde_json::to_value(inc(1)).unwrap();
        assert_eq!(v["level"], "error");
        for (l, s) in [(Level::Info, "info"), (Level::Warn, "warn"), (Level::Error, "error")] {
            assert_eq!(serde_json::to_value(l).unwrap(), s);
            assert_eq!(serde_json::from_value::<Level>(serde_json::json!(s)).unwrap(), l);
        }
        assert!(v.get("sample").is_none());
        let mut with = inc(2);
        with.sample = Some("dial tcp 198.51.100.8:10007: i/o timeout".into());
        assert_eq!(
            serde_json::to_value(&with).unwrap()["sample"],
            "dial tcp 198.51.100.8:10007: i/o timeout"
        );
    }
}
```

`crates/bui/src/modules/sentinel/mod.rs` 在 `pub mod signature;` 前后改为：

```rust
pub mod engine;
pub mod incidents;
pub mod signature;

/// 同签名同对象 60 秒内只触发一次（spec §5.7）
pub const DEBOUNCE_SECS: i64 = 60;
/// 同动作同对象 10 分钟内不重复执行（spec §5.7）；冷却表持久化在 `runtime.extra["sentinel"]`
pub const ACTION_COOLDOWN_SECS: i64 = 600;
/// `runtime.incidents` 的环形上限（新的在前）
pub const INCIDENTS_MAX: usize = 200;
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui sentinel:: 2>&1 | tail -5`
Expected: 编译失败——`cannot find type Engine` / `Incident` / `Level`。

- [ ] **Step 3: 实现 `engine.rs`**（写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 去抖与冷却（spec §5.7，设计裁决 D12）+ 哨兵自己的运行时段（`runtime.extra["sentinel"]`）。
//! [`Engine`] 是纯状态机（时钟由调用方传入），只活在内存里；冷却表与游标在 [`SentinelRuntime`]，落盘。

use super::signature::Sig;
use super::{ACTION_COOLDOWN_SECS, DEBOUNCE_SECS};
use crate::state::runtime::RuntimeData;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// `RuntimeData.extra` 里哨兵独占的键
pub const SENTINEL_KEY: &str = "sentinel";
/// [`Engine::prune`] 丢掉「最后一条命中早于这么久」的计数：比最长的签名窗口（150 秒）宽
const PRUNE_AFTER_SECS: i64 = 300;

/// 哨兵的持久化运行时段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SentinelRuntime {
    /// 上一次落盘时读到的最后一条 `__CURSOR`（内存里的更新，落盘有节流，设计裁决 D1）
    pub cursor: Option<String>,
    /// 没有游标时从这一刻读起（首次启动 / 游标失效时写成「当时的现在」，不回放历史）
    pub since: Option<String>,
    /// 冷却表：`<动作 id>|<对象>` → 上次执行时刻（RFC3339）
    pub acted: BTreeMap<String, String>,
    /// 住宅上游被判不健康的起点（30 分钟建议替换，设计裁决 D15）
    pub down_since: BTreeMap<Uuid, String>,
    /// 这一段不可达已经建议过替换的上游（恢复即清）
    pub suggested: BTreeSet<Uuid>,
}

/// 读哨兵段；缺失或解析失败按空值重建（`runtime.json` 可丢可重建）
pub fn sentinel_of(rt: &RuntimeData) -> SentinelRuntime {
    rt.extra
        .get(SENTINEL_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

pub fn put_sentinel(rt: &mut RuntimeData, s: &SentinelRuntime) {
    // to_value 只会在自定义 Serialize 里失败，这里全是 derive
    if let Ok(v) = serde_json::to_value(s) {
        rt.extra.insert(SENTINEL_KEY.to_string(), v);
    }
}

/// 同签名同对象的计数键
pub fn sig_key(sig: Sig, subject: &str) -> String {
    format!("{}|{subject}", sig.id())
}

/// 冷却键：同一个**动作**落在同一个对象上（两种签名共用一个动作时共享冷却）
pub fn action_key(sig: Sig, subject: &str) -> String {
    format!("{}|{subject}", sig.action().id())
}

/// 去抖计数器：每个 `sig|subject` 一串命中时刻 + 上次触发时刻。
#[derive(Debug, Default)]
pub struct Engine {
    hits: BTreeMap<String, VecDeque<OffsetDateTime>>,
    fired: BTreeMap<String, OffsetDateTime>,
}

impl Engine {
    /// 喂一条命中（`ts` = 日志时刻，`now` = 本轮时刻）。返回 `true` = 它让该签名在窗口内
    /// 达到门槛、且距上次触发已过 [`DEBOUNCE_SECS`] ⇒ **本轮触发一次**（触发后计数清零）。
    /// 早于窗口的日志直接不计（重启后续读的积压描述的是过去）。
    pub fn observe(&mut self, sig: Sig, subject: &str, ts: OffsetDateTime, now: OffsetDateTime) -> bool {
        let rule = sig.rule();
        let window = Duration::seconds(rule.window_secs);
        if now - ts > window {
            return false;
        }
        let key = sig_key(sig, subject);
        let q = self.hits.entry(key.clone()).or_default();
        q.push_back(ts);
        while q.front().is_some_and(|t| now - *t > window) {
            q.pop_front();
        }
        if q.len() < rule.threshold {
            return false;
        }
        if self
            .fired
            .get(&key)
            .is_some_and(|t| now - *t < Duration::seconds(DEBOUNCE_SECS))
        {
            return false;
        }
        self.fired.insert(key.clone(), now);
        self.hits.remove(&key);
        true
    }

    /// 丢掉陈旧的计数与已过期的去抖记录（每轮调一次）
    pub fn prune(&mut self, now: OffsetDateTime) {
        let keep = Duration::seconds(PRUNE_AFTER_SECS);
        self.hits
            .retain(|_, q| q.back().is_some_and(|t| now - *t <= keep));
        self.fired
            .retain(|_, t| now - *t < Duration::seconds(DEBOUNCE_SECS));
    }
}

/// 冷却表里这一条是否还「新鲜」。时钟回跳（记录时刻晚于现在）按不在冷却处理，不能把动作锁死
fn fresh(at: &str, now: OffsetDateTime) -> bool {
    parse_rfc3339(at)
        .is_some_and(|t| t <= now && now - t < Duration::seconds(ACTION_COOLDOWN_SECS))
}

/// 同动作同对象 [`ACTION_COOLDOWN_SECS`] 内是否执行过
pub fn in_cooldown(acted: &BTreeMap<String, String>, key: &str, now: OffsetDateTime) -> bool {
    acted.get(key).is_some_and(|at| fresh(at, now))
}

/// 冷却表只留还在冷却期里的条目（它随 `runtime.json` 落盘，不能只进不出）
pub fn prune_acted(acted: &mut BTreeMap<String, String>, now: OffsetDateTime) {
    acted.retain(|_, at| fresh(at, now));
}
```

测试里 `feed` 用了 `fmt_rfc3339`（`use super::*` 带进来）；`e.hits` / `e.fired` 是私有字段，同文件内的测试可见。

- [ ] **Step 4: 实现 `incidents.rs`**（写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 事件（spec §5.7，设计裁决 D13）：`runtime.extra["incidents"]` 环形保留最近
//! [`INCIDENTS_MAX`] 条，**新的在前**。面板、`bui status`、`bui incidents` 都读这一份。

use super::INCIDENTS_MAX;
use crate::state::runtime::RuntimeData;
use serde::{Deserialize, Serialize};

/// `RuntimeData.extra` 里的键（落盘后就是 `runtime.json` 顶层的 `incidents` 数组）
pub const INCIDENTS_KEY: &str = "incidents";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 探测通过 / 冷却外的复核结论：记一笔，不算故障
    Info,
    /// 已自动处置或只影响部分功能
    Warn,
    /// 需要管理员关注（上游不可达、鉴权全员失败、证书签不下来）
    Error,
}

/// 一条事件。字段即 spec §5.7 的「时间、单元、签名、对象、动作、结果」+ 等级 + 原文样本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub at: String,
    pub unit: String,
    pub signature: String,
    /// 对象的人读名：住宅上游是 `host:port`（**不带凭据**）、单元名、域名
    pub subject: String,
    pub action: String,
    pub result: String,
    pub level: Level,
    /// 触发它的那行日志（已脱敏、截断）；巡查类事件没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

/// 读全部事件（新的在前）；缺失或解析失败按空
pub fn from_runtime(rt: &RuntimeData) -> Vec<Incident> {
    rt.extra
        .get(INCIDENTS_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// 追加一条到最前，截到 [`INCIDENTS_MAX`]
pub fn push(rt: &mut RuntimeData, inc: Incident) {
    let mut v = from_runtime(rt);
    v.insert(0, inc);
    v.truncate(INCIDENTS_MAX);
    if let Ok(x) = serde_json::to_value(&v) {
        rt.extra.insert(INCIDENTS_KEY.to_string(), x);
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p bui sentinel:: 2>&1 | tail -5`
Expected: PASS（`engine` 9 条 + `incidents` 3 条 + Task 2 的签名表）。

- [ ] **Step 6: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/sentinel/mod.rs crates/bui/src/modules/sentinel/engine.rs \
        crates/bui/src/modules/sentinel/incidents.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 去抖/冷却状态机与事件环形存储

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 4: 住宅侧三个小函数——快探、立即判不健康、立即借用

**Files:**
- Modify: `crates/bui/src/modules/residential/health.rs`（`probe_quick` + 测试）
- Modify: `crates/bui/src/modules/residential/state.rs`（`mark_unhealthy` + 测试）
- Modify: `crates/bui/src/modules/residential/slots.rs`（`PutFail` / `put_slot` 从 `drive_slots` 抽出；`borrow_now` + 测试）

**Interfaces:**
- Consumes: 既有 `health::{probe_reachable（私有）, rank_healthy, MemberProbe}`、`state::{record_probe, HealthState, SlotRuntime, FAIL_TO_UNHEALTHY}`、`slots::{drive_slots, SlotOutcome}`、`clash::{Clash, tag_of}`、`residential::slot_selector`。
- Produces:
  - `health::probe_quick(&dyn Prober, &Upstream) -> MemberProbe`
  - `state::mark_unhealthy(&mut HealthState, OffsetDateTime)`
  - `slots::borrow_now(&DaemonCtx, Arc<dyn Clash>, failed: Uuid, now: OffsetDateTime) -> Vec<SlotOutcome>`
  - `drive_slots` 行为不变（既有测试全部照过），内部改用私有 `put_slot`
  - 私有 `static SLOT_SWITCH: tokio::sync::Mutex<()>`：`drive_slots` 与 `borrow_now` 各自整段持锁（设计裁决 D7：`borrow_now` 不会落在巡检那一轮的读快照与写回之间）

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/residential/health.rs` 的 `mod tests` 末尾：

```rust
    // ── 哨兵的带外快探（spec §5.7，设计裁决 D5）──

    #[test]
    fn the_quick_probe_gives_up_at_once_when_the_gateway_is_unreachable() {
        let p = crate::modules::residential::proxy::FakeProber::new(); // tcp_ms 缺省 None = 连不上
        let r = probe_quick(&p, &upstream(2, 10));
        assert!(!r.ok && !r.auth_failed);
        assert_eq!(p.calls(), vec!["tcp"], "网关都连不上就不再发 HTTP（丢包时每次都要等满超时）");
    }

    #[test]
    fn the_quick_probe_tunnels_once_when_the_gateway_answers() {
        let p = crate::modules::residential::proxy::FakeProber::new();
        p.with(|i| {
            i.tcp_ms = Some(30);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        let r = probe_quick(&p, &upstream(2, 10));
        assert!(r.ok);
        assert_eq!(r.tcp_ms, Some(30));
        assert_eq!(
            p.calls(),
            vec![
                "tcp".to_string(),
                format!("timed:{}", crate::modules::residential::LATENCY_PROBE_URL)
            ],
            "快探不测 Google / UDP / 测速"
        );
    }

    #[test]
    fn the_quick_probe_still_tells_a_credential_failure_apart() {
        let p = crate::modules::residential::proxy::FakeProber::new();
        p.with(|i| {
            i.tcp_ms = Some(30);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Err("__auth_failed__".into()),
            );
        });
        let r = probe_quick(&p, &upstream(2, 10));
        assert!(!r.ok && r.auth_failed);
    }
```

`crates/bui/src/modules/residential/state.rs` 的 `mod tests` 末尾：

```rust
    /// 哨兵带外确认不可达 ⇒ 立即不健康；恢复仍走巡检的 2 轮迟滞（设计裁决 D6）
    #[test]
    fn an_out_of_band_failure_marks_a_member_down_at_once_but_recovery_still_needs_two_rounds() {
        let mut h = HealthState::default();
        mark_unhealthy(&mut h, t0());
        assert!(!h.active);
        assert_eq!((h.okstreak, h.failstreak), (0, FAIL_TO_UNHEALTHY));
        assert_eq!(h.samples.len(), 1);
        assert!(!h.samples[0].ok, "带外失败也进 24h 成功率");
        assert!(!apply_hysteresis(&mut h, true), "第 1 轮探通还不恢复");
        assert!(apply_hysteresis(&mut h, true), "第 2 轮才恢复");
    }
```

`crates/bui/src/modules/residential/slots.rs` 的 `mod tests` 末尾（紧接 `switching_back_needs_three_consecutive_good_rounds` 之后，复用 `three_slot_ctx` / `seed_health`）：

```rust
    // ── 哨兵的立即借用（spec §5.7，设计裁决 D7）──────────────────────────────

    #[tokio::test]
    async fn borrow_now_moves_only_the_slots_sitting_on_the_failed_ip() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        state::update(&ctx.runtime, |r| {
            r.slots.entry("1".into()).or_default().back_rounds = 2;
        })
        .await;
        let out = borrow_now(&ctx, clash.clone(), Uuid::from_u128(2), OffsetDateTime::UNIX_EPOCH).await;
        assert_eq!(out.len(), 1, "只有槽 1 压在故障 IP 上：{out:?}");
        let o = &out[0];
        assert_eq!(
            (o.index, o.target, o.borrowed, o.switched),
            (1, Uuid::from_u128(3), true, true),
            "借排名最高的健康 IP（延迟最低的 resi-3）"
        );
        assert_eq!(clash.calls(), vec!["put:slot-1-pool:resi-3"], "槽 0 / 槽 2 一次 PUT 都不许有");
        let r = state::read(&ctx.runtime).await;
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(3)));
        assert_eq!(r.slots["1"].back_rounds, 0, "被挪走的槽从头数切回轮数");
    }

    #[tokio::test]
    async fn a_slot_borrowing_the_failed_ip_goes_home_when_its_own_ip_is_fine() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        // 槽 2 此刻借用着 IP 1
        state::update(&ctx.runtime, |r| {
            r.slots.entry("2".into()).or_default().current_upstream_id = Some(Uuid::from_u128(1));
        })
        .await;
        let out = borrow_now(&ctx, clash.clone(), Uuid::from_u128(1), OffsetDateTime::UNIX_EPOCH).await;
        let s0 = out.iter().find(|o| o.index == 0).unwrap();
        assert_eq!(s0.target, Uuid::from_u128(3), "槽 0 的本槽 IP 就是故障 IP ⇒ 借排名最高的");
        let s2 = out.iter().find(|o| o.index == 2).unwrap();
        assert_eq!((s2.target, s2.borrowed), (Uuid::from_u128(3), false), "槽 2 自己的 IP 好好的 ⇒ 回本槽");
        assert!(out.iter().all(|o| o.index != 1), "槽 1 没压在 IP 1 上");
    }

    #[tokio::test]
    async fn borrow_now_leaves_a_pinned_slot_alone() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        state::update(&ctx.runtime, |r| {
            let e = r.slots.entry("1".into()).or_default();
            e.pinned_upstream_id = Some(Uuid::from_u128(2));
            e.current_upstream_id = Some(Uuid::from_u128(2));
        })
        .await;
        let out = borrow_now(&ctx, clash.clone(), Uuid::from_u128(2), OffsetDateTime::UNIX_EPOCH).await;
        assert!(out.is_empty(), "管理员的 pin 压过哨兵：{out:?}");
        assert!(clash.calls().is_empty());
    }

    #[tokio::test]
    async fn with_nothing_healthy_to_borrow_the_slot_holds_still() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, false),
                (2, Some(true), 100, true),
                (3, Some(true), 100, false),
            ],
        )
        .await;
        let out = borrow_now(&ctx, clash.clone(), Uuid::from_u128(2), OffsetDateTime::UNIX_EPOCH).await;
        assert_eq!(out.len(), 1);
        assert!(!out[0].switched);
        assert_eq!(out[0].note.as_deref(), Some("没有可借用的健康 IP，保持现状"));
        assert!(clash.calls().is_empty(), "fail-open：不许 PUT 一条不健康的 IP");
        assert_eq!(
            state::read(&ctx.runtime).await.slots.get("1").and_then(|s| s.current_upstream_id),
            None
        );
    }

    #[tokio::test]
    async fn a_rejected_put_leaves_the_runtime_untouched() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        clash.with(|i| i.reject = true);
        let out = borrow_now(&ctx, clash.clone(), Uuid::from_u128(2), OffsetDateTime::UNIX_EPOCH).await;
        assert!(!out[0].switched);
        assert!(out[0].note.as_deref().unwrap().starts_with("切到 resi-3 失败"), "{:?}", out[0].note);
        assert_eq!(
            state::read(&ctx.runtime).await.slots.get("1").and_then(|s| s.current_upstream_id),
            None,
            "没切成就别记成切了"
        );
    }

    /// 设计裁决 D7：`borrow_now` 与 `drive_slots` 共用 `SLOT_SWITCH`。锁被占着（另一方正在
    /// 「读快照 → PUT → 写回」）时两者都得等，一次 PUT 都不发；放开后各自照常做完。
    #[tokio::test]
    async fn borrow_now_and_drive_slots_wait_for_each_other_on_the_slot_switch_lock() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let grp = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        let wait = std::time::Duration::from_millis(50);

        let held = SLOT_SWITCH.lock().await;
        let borrow = borrow_now(&ctx, clash.clone(), Uuid::from_u128(2), OffsetDateTime::UNIX_EPOCH);
        tokio::pin!(borrow);
        assert!(tokio::time::timeout(wait, &mut borrow).await.is_err(), "锁被占着，借用必须等");
        let drive = drive_slots(&ctx, clash.clone(), &grp, &healthy, OffsetDateTime::UNIX_EPOCH);
        tokio::pin!(drive);
        assert!(tokio::time::timeout(wait, &mut drive).await.is_err(), "锁被占着，巡检也得等");
        assert!(clash.calls().is_empty(), "等锁期间一次 PUT 都不许发");
        drop(held);
        assert_eq!(borrow.await.len(), 1, "放开后借用照常做完（只有槽 1 压在 IP 2 上）");
        assert_eq!(drive.await.len(), 3, "放开后巡检照常做完三个槽");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui residential:: 2>&1 | tail -5`
Expected: 编译失败——`cannot find function probe_quick` / `mark_unhealthy` / `borrow_now`、`cannot find value SLOT_SWITCH`。

- [ ] **Step 3: 实现 `probe_quick`**（`health.rs`，紧接 `probe_member` 之后）

```rust
/// 哨兵（spec §5.7，设计裁决 D5）的带外快探：先量到上游网关的 TCP 建连，**连不上直接判不可达**
/// ——网关都连不上，隧道不可能通，再等一次 HTTP 超时只会拖慢预案（上游被丢包时巡检那套
/// [`probe_member`] 要走 ~40 秒）；连得上再走一轮与巡检同口径的 [`probe_reachable`]（含 407 补判）。
/// **不测 Google / UDP / 测速**：那些是巡检的指标，预案只关心「这条上游此刻还能不能用」。
/// 超时由调用方的 `Prober` 决定（哨兵用 `ReqwestProber::with_timeout(5)`）。
pub fn probe_quick(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    let Some(tcp_ms) = p.gateway_tcp_ms(up) else {
        return MemberProbe::default();
    };
    let mut probe = probe_reachable(p, up);
    probe.tcp_ms = Some(tcp_ms);
    probe
}
```

- [ ] **Step 4: 实现 `mark_unhealthy`**（`state.rs`，紧接 `apply_hysteresis` 之后）

```rust
/// 哨兵带外探测确认不可达（spec §5.7，设计裁决 D6）：**立即**判不健康（不等巡检的 2 轮迟滞），
/// 并记一条失败样本。否则下一轮巡检里它仍是 `active`，按槽驱动会把「还坏着的一轮」当成
/// 「恢复第 1 轮」。恢复照旧走 [`apply_hysteresis`]（连续 [`OK_TO_HEALTHY`] 轮）。
pub fn mark_unhealthy(h: &mut HealthState, now: OffsetDateTime) {
    record_probe(h, false, now);
    h.active = false;
    h.okstreak = 0;
    h.failstreak = h.failstreak.max(FAIL_TO_UNHEALTHY);
}
```

- [ ] **Step 5: `slots.rs` 抽出 `put_slot` 并实现 `borrow_now`**

在 `pub async fn drive_slots` 之前加：

```rust
/// 按槽切换的互斥（设计裁决 D7）：[`drive_slots`] 整轮「读快照 → 逐槽 PUT → 按快照写回
/// `current_upstream_id`」，[`borrow_now`] 若落在中间，它写的记录会被那轮写回盖成旧值（relay 的
/// selector 已经切走，runtime 还记着旧值）。两者各自整段持锁；持锁期间只有毫秒级的 Clash PUT。
static SLOT_SWITCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// [`put_slot`] 没切成的原因。文案与抽出前的 `drive_slots` 逐字相同（面板与 CLI 直接显示）。
enum PutFail {
    /// 目标已不在池里
    Gone,
    /// Clash API 拒绝 / 不可达
    Rejected(String),
    /// `spawn_blocking` 任务异常
    Task(String),
}

impl PutFail {
    fn note(&self) -> String {
        match self {
            PutFail::Gone => "目标已不在池里，本轮不切".into(),
            PutFail::Rejected(s) | PutFail::Task(s) => s.clone(),
        }
    }
}

/// 把槽 `index` 的 selector 切到 `target`：成功返回目标 tag。[`drive_slots`] 与 [`borrow_now`]
/// 共用，「切一槽」只有这一份实现。
async fn put_slot(
    c: Arc<dyn Clash>,
    g: &ResidentialGroup,
    index: u16,
    target: Uuid,
) -> Result<String, PutFail> {
    let Some(tag) = clash::tag_of(g, target) else {
        return Err(PutFail::Gone);
    };
    let (sel, t) = (super::slot_selector(index), tag.clone());
    match tokio::task::spawn_blocking(move || c.select(&sel, &t)).await {
        Ok(Ok(())) => Ok(tag),
        Ok(Err(e)) => Err(PutFail::Rejected(format!("切到 {tag} 失败：{e}"))),
        Err(e) => Err(PutFail::Task(format!("切换任务异常：{e}"))),
    }
}
```

把 `drive_slots` 里从 `let mut switched = false;` 到它对应的 `if !hold && current != Some(target) { … }` 结束的整段替换为：

```rust
        let mut switched = false;
        if !hold && current != Some(target) {
            match put_slot(c.clone(), g, s.index, target).await {
                Ok(tag) => {
                    switched = true;
                    tracing::info!(slot = s.index, to = %tag, "按槽切换住宅出口");
                }
                Err(f) => {
                    if matches!(f, PutFail::Rejected(_)) {
                        rounds = sr.back_rounds; // 没切成就别把轮数清掉
                    }
                    note = Some(f.note());
                }
            }
        }
```

`drive_slots` 函数体第一行（`let view = …` 之前）加：

```rust
    let _switch = SLOT_SWITCH.lock().await;
```

在 `drive_slots` 之后（`pin_slot` 之前）加：

```rust
/// 哨兵的立即借用（spec §5.7，设计裁决 D7）：`failed` 已被带外探测确认不可用（不可达 /
/// Google 被封），把**此刻正压在它身上**的槽立刻挪走，不等下一轮巡检。
///
/// 与 [`drive_slots`] 同一口径、只做「借出」这一半：
/// - 只动「当前出口 == `failed`」的槽：本槽 IP 就是它（`current_upstream_id = None` 时 selector
///   停在配置里的 default = 本槽 IP），或正借用它；
/// - 手动 pin 的槽不动（管理员的判断压过哨兵，同 [`drive_slots`] 规则 1）；
/// - 目标：本槽 IP 不是 `failed` 且健康、Google 未被封 ⇒ 回本槽；否则
///   [`health::rank_healthy`] 里排名最高的非 `failed` 健康 IP；一个都没有 ⇒ 保持现状（fail-open）；
/// - 被挪动的槽 `back_rounds` 归零；**不推进任何槽的 `back_rounds`，切回永远只由巡检的
///   [`drive_slots`] 负责**（连续 [`SLOT_BACK_ROUNDS`] 轮）。
///
/// 「健康」= runtime 里 `active` 的成员（调用方先 `mark_unhealthy` 再调本函数）。
/// 与 [`drive_slots`] 共用 `SLOT_SWITCH`：不会落在巡检那一轮的读快照与写回之间。
pub async fn borrow_now(
    ctx: &DaemonCtx,
    c: Arc<dyn Clash>,
    failed: Uuid,
    now: OffsetDateTime,
) -> Vec<SlotOutcome> {
    let _switch = SLOT_SWITCH.lock().await;
    let s = ctx.store.read().await;
    let view = slots::sorted(&s.residential);
    let g = state::group_of(&s);
    drop(s);
    let rt = state::read(&ctx.runtime).await;
    let healthy: Vec<Uuid> = g
        .upstreams
        .iter()
        .map(|u| u.id)
        .filter(|id| {
            *id != failed
                && rt
                    .health
                    .get(&id.to_string())
                    .map(|h| h.active)
                    .unwrap_or(true)
        })
        .collect();
    let ranked = health::rank_healthy(&g, &rt, &healthy, now);
    let google_ok =
        |id: Uuid| rt.health.get(&id.to_string()).and_then(|h| h.google_ok) != Some(false);
    let in_pool = |id: Uuid| g.upstreams.iter().any(|u| u.id == id);

    let mut out = Vec::new();
    let mut writes: Vec<(u16, Uuid)> = Vec::new();
    for sl in &view {
        if !in_pool(sl.upstream_id) {
            continue;
        }
        let sr = rt.slots.get(&sl.index.to_string()).cloned().unwrap_or_default();
        if sr.pinned_upstream_id.is_some_and(in_pool) {
            continue;
        }
        let own = sl.upstream_id;
        let current = sr.current_upstream_id.unwrap_or(own);
        if current != failed {
            continue;
        }
        let target = if own != failed && healthy.contains(&own) && google_ok(own) {
            Some(own)
        } else {
            ranked.first().copied()
        };
        let stay = |note: String| SlotOutcome {
            index: sl.index,
            own,
            target: current,
            borrowed: current != own,
            switched: false,
            note: Some(note),
        };
        let Some(target) = target else {
            out.push(stay("没有可借用的健康 IP，保持现状".into()));
            continue;
        };
        match put_slot(c.clone(), &g, sl.index, target).await {
            Ok(tag) => {
                tracing::warn!(slot = sl.index, to = %tag, "哨兵：本槽当前出口不可用，立即借用");
                writes.push((sl.index, target));
                out.push(SlotOutcome {
                    index: sl.index,
                    own,
                    target,
                    borrowed: target != own,
                    switched: true,
                    note: None,
                });
            }
            Err(f) => out.push(stay(f.note())),
        }
    }
    if !writes.is_empty() {
        state::update(&ctx.runtime, move |r| {
            for (index, target) in writes {
                let e = r.slots.entry(index.to_string()).or_default();
                e.current_upstream_id = Some(target);
                e.back_rounds = 0;
            }
        })
        .await;
    }
    out
}
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p bui residential:: 2>&1 | tail -5`
Expected: PASS——新增 10 条，且 `drive_slots` 的既有测试（`a_healthy_slot_uses_its_own_ip` / `an_unhealthy_slot_borrows_the_top_ranked_healthy_ip_immediately` / `switching_back_needs_three_consecutive_good_rounds` 等）不改照过。

- [ ] **Step 7: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 8: Commit**

```bash
git add crates/bui/src/modules/residential/health.rs crates/bui/src/modules/residential/state.rs \
        crates/bui/src/modules/residential/slots.rs
git commit -m "$(cat <<'EOF'
feat(residential): 哨兵用的带外快探、立即判不健康与按槽立即借用

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 5: 住宅预案——带外探测 → 立即借用 → 告警；30 分钟建议替换

**Files:**
- Create: `crates/bui/src/modules/sentinel/resi.rs`
- Create: `crates/bui/src/modules/sentinel/testkit.rs`（`#[cfg(test)]` 支架）
- Modify: `crates/bui/src/modules/sentinel/incidents.rs`（`Outcome`）
- Modify: `crates/bui/src/modules/sentinel/mod.rs`（`pub mod resi;`、`#[cfg(test)] pub mod testkit;`、`LONG_UNREACHABLE_MINS`）

**Interfaces:**
- Consumes: Task 3 的 `engine::SentinelRuntime`、`incidents::Level`；Task 4 的 `health::probe_quick`、`state::mark_unhealthy`、`slots::{borrow_now, SlotOutcome}`；既有 `proxy::{Prober, google_ok_of}`、`clash::Clash`、`state::{group_of, read, update, set_upstream_alert, push_alert, record_google}`、`residential::slots::migrate_on_start`。
- Produces:
  - `incidents::Outcome { subject: String, result: String, level: Level }`（`Debug, Clone, PartialEq`）
  - `sentinel::LONG_UNREACHABLE_MINS: i64 = 30`
  - `resi::subject_of(&Upstream) -> String`（`host:port`）、`resi::ip_of(&Upstream) -> String`
  - `resi::on_upstream_error(&DaemonCtx, Arc<dyn Prober>, Arc<dyn Clash>, Uuid, OffsetDateTime) -> Outcome`
  - `resi::on_google_blocked(&DaemonCtx, Arc<dyn Prober>, Arc<dyn Clash>, Uuid, OffsetDateTime) -> Outcome`
  - `resi::sweep_long_unreachable(&DaemonCtx, &mut SentinelRuntime, OffsetDateTime) -> Vec<Outcome>`
  - `testkit::{upstream(u128) -> Upstream, pool_ctx(&Path) -> (DaemonCtx, Arc<FakeHost>)}`：三条 socks5 上游 `isp{1,2,3}.example.net:10007`，体检出口 `198.51.100.{7,8,9}`，三个槽（上游 N 在槽 N-1）

- [ ] **Step 1: 建测试支架**

`crates/bui/src/modules/sentinel/mod.rs` 加：

```rust
pub mod resi;
#[cfg(test)]
pub mod testkit;

/// 住宅 IP 被判不健康连续这么久 ⇒ 告警里建议管理员替换（设计裁决 D15）
pub const LONG_UNREACHABLE_MINS: i64 = 30;
```

新建 `crates/bui/src/modules/sentinel/testkit.rs`：

```rust
//! 哨兵各任务共用的测试支架（`#[cfg(test)]`）：一台装了三条上游、三个槽的假机器。
use crate::api::EventBus;
use crate::reconcile::DaemonCtx;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::fake::FakeHost;
use bui_schema::model::{ResiMode, Upstream, UpstreamKind, Verified, DEFAULT_GROUP};
use bui_schema::paths::Paths;
use std::sync::Arc;
use uuid::Uuid;

/// 上游 N：`ispN.example.net:10007`，体检学到的出口 `198.51.100.(6+N)`（N = 1..=3 ⇒ .7/.8/.9）
pub fn upstream(n: u128) -> Upstream {
    Upstream {
        id: Uuid::from_u128(n),
        name: format!("url-{n}"),
        kind: UpstreamKind::Socks5,
        host: format!("isp{n}.example.net"),
        port: 10007,
        username: "user1".into(),
        password: "pw1".into(),
        priority: 100,
        provider: None,
        region: None,
        ports_allowed: None,
        verified: Some(Verified {
            ip: format!("198.51.100.{}", 6 + n),
            asn: None,
            org: None,
            country: None,
            at: "2026-09-11T00:00:00Z".into(),
        }),
    }
}

/// 三条上游 + 三个槽（`migrate_on_start` 按池序落槽：上游 N 在槽 N-1）；state / runtime /
/// 快照都在 `dir` 里，时钟是 FakeHost 的 2026-09-11T00:00:00Z。
pub async fn pool_ctx(dir: &std::path::Path) -> (DaemonCtx, Arc<FakeHost>) {
    let mut st = crate::testutil::sample_state();
    let g = st
        .residential
        .groups
        .get_mut(DEFAULT_GROUP)
        .expect("sample_state 自带 default 组");
    g.enabled = true;
    g.mode = ResiMode::Global;
    g.upstreams = (1..=3).map(upstream).collect();
    g.selected_upstream_id = Some(Uuid::from_u128(1));
    let paths = Paths {
        base_dir: dir.to_path_buf(),
        certs_dir: dir.join("certs"),
        bin_dir: dir.join("bin"),
    };
    let store = Store::create(crate::paths::state_file(&paths), st)
        .await
        .unwrap();
    let bus = EventBus::new();
    crate::modules::residential::slots::migrate_on_start(&store, &bus)
        .await
        .unwrap();
    let host = Arc::new(FakeHost::new());
    (
        DaemonCtx {
            store,
            runtime: Runtime::load(crate::paths::runtime_file(&paths)),
            bus,
            host: host.clone(),
            paths,
        },
        host,
    )
}
```

`crates/bui/src/modules/sentinel/incidents.rs`，`pub fn from_runtime` 之前加：

```rust
/// 一个预案的结论：事件的 `subject` / `result` / `level` 由处理它的那一方决定
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub subject: String,
    pub result: String,
    pub level: Level,
}
```

- [ ] **Step 2: 写失败测试**

新建 `crates/bui/src/modules/sentinel/resi.rs`，先只放测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::clash::{Clash, FakeClash};
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::sentinel::testkit::pool_ctx;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn fakes() -> (Arc<FakeProber>, Arc<FakeClash>) {
        (Arc::new(FakeProber::new()), Arc::new(FakeClash::new(Some("resi-1"))))
    }

    #[tokio::test]
    async fn an_unreachable_upstream_is_marked_down_and_its_slot_borrows_at_once() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes(); // FakeProber 缺省：网关连不上
        let o = on_upstream_error(&ctx, p.clone(), c.clone(), u(2), t0()).await;
        assert_eq!(o.subject, "isp2.example.net:10007", "对象只写 host:port，不带凭据");
        assert_eq!(o.result, "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7");
        assert_eq!(o.level, Level::Error);
        assert_eq!(p.calls(), vec!["tcp"], "快探：网关连不上就不再发 HTTP");
        assert_eq!(c.selected("slot-1-pool").as_deref(), Some("resi-1"));
        let r = state::read(&ctx.runtime).await;
        assert!(!r.health[&u(2).to_string()].active, "带外确认 ⇒ 立即不健康");
        assert_eq!(r.slots["1"].current_upstream_id, Some(u(1)));
        assert_eq!(
            r.upstream_alerts.get(&u(2)),
            Some(&o.result),
            "上游级告警：面板住宅卡看得到，巡检探通即清"
        );
    }

    #[tokio::test]
    async fn a_passing_out_of_band_probe_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        let o = on_upstream_error(&ctx, p.clone(), c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Info);
        assert!(o.result.contains("带外探测通过"), "{}", o.result);
        assert!(c.calls().is_empty(), "不许借用");
        let r = state::read(&ctx.runtime).await;
        assert!(r.health.get(&u(2).to_string()).is_none_or(|h| h.active));
        assert!(r.upstream_alerts.is_empty());
    }

    #[tokio::test]
    async fn an_auth_failure_is_reported_as_credentials_not_reachability() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Err("__auth_failed__".into()),
            );
        });
        let o = on_upstream_error(&ctx, p, c, u(2), t0()).await;
        assert_eq!(
            o.result,
            "IP 198.51.100.8 凭据失效（407 / SOCKS5 认证被拒），槽 1 已临时切到 198.51.100.7"
        );
    }

    #[tokio::test]
    async fn a_sorry_page_on_recheck_marks_google_blocked_and_borrows() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 200,
                body: "<a href=\"https://www.google.com/sorry/index?continue=x\">".into(),
            }))
        });
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(
            o.result,
            "IP 198.51.100.8 的 Google 被封（serp 403 / sorry 页），槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(o.level, Level::Warn);
        assert_eq!(c.selected("slot-1-pool").as_deref(), Some("resi-1"));
        let r = state::read(&ctx.runtime).await;
        assert_eq!(r.health[&u(2).to_string()].google_ok, Some(false));
        assert!(r.health[&u(2).to_string()].active, "Google 被封不等于不可达");
        assert_eq!(r.alerts.first(), Some(&o.result), "全局告警（上游级的会被下一轮探通清掉）");
    }

    #[tokio::test]
    async fn an_inconclusive_recheck_trusts_the_explicit_serp_403() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes(); // google 缺省：连不上 ⇒ google_ok_of = None
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Warn);
        assert!(!c.calls().is_empty(), "日志里的 403 serp 是明确策略，复核没结论也要借");
    }

    #[tokio::test]
    async fn google_fine_on_recheck_means_no_action() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 200,
                body: "<html>results</html>".into(),
            }))
        });
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Info);
        assert!(c.calls().is_empty());
        let r = state::read(&ctx.runtime).await;
        assert_eq!(r.health.get(&u(2).to_string()).and_then(|h| h.google_ok), None);
    }

    #[tokio::test]
    async fn an_upstream_down_for_thirty_minutes_gets_exactly_one_replace_suggestion() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        state::update(&ctx.runtime, |r| {
            r.health.entry(u(2).to_string()).or_default().active = false;
        })
        .await;
        let min = |m: i64| t0() + time::Duration::minutes(m);
        let mut sr = SentinelRuntime::default();
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(0)).await.is_empty());
        assert_eq!(sr.down_since.get(&u(2)), Some(&fmt_rfc3339(min(0))));
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(29)).await.is_empty());
        let out = sweep_long_unreachable(&ctx, &mut sr, min(30)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].subject, "isp2.example.net:10007");
        assert!(
            out[0].result.starts_with("IP 198.51.100.8 已连续不可达 30 分钟，建议替换"),
            "{}",
            out[0].result
        );
        assert!(out[0].result.contains("bui residential remove isp2.example.net:10007"));
        assert_eq!(
            state::read(&ctx.runtime).await.upstream_alerts.get(&u(2)),
            Some(&out[0].result)
        );
        assert!(
            sweep_long_unreachable(&ctx, &mut sr, min(31)).await.is_empty(),
            "同一段不可达只建议一次"
        );
        // 恢复 ⇒ 两张表都清掉，下一段不可达重新计时
        state::update(&ctx.runtime, |r| {
            r.health.get_mut(&u(2).to_string()).unwrap().active = true;
        })
        .await;
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(32)).await.is_empty());
        assert!(sr.down_since.is_empty() && sr.suggested.is_empty());
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p bui sentinel::resi 2>&1 | tail -5`
Expected: 编译失败——`cannot find function on_upstream_error` 等。

- [ ] **Step 4: 实现**（`resi.rs`，写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 住宅预案（spec §5.7 × §5.6）：带外探测 → 立即判不健康 → 按槽借用 → 告警。
//! 边界（设计裁决 D15）：**不增删池成员、不做切回**（切回归巡检的 `drive_slots`）；
//! 长时间不可达只建议管理员替换。

use super::engine::SentinelRuntime;
use super::incidents::{Level, Outcome};
use super::LONG_UNREACHABLE_MINS;
use crate::modules::residential::clash::Clash;
use crate::modules::residential::proxy::{self, Prober};
use crate::modules::residential::slots::{self, SlotOutcome};
use crate::modules::residential::{health, state};
use crate::reconcile::DaemonCtx;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, Upstream};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

/// 事件与告警里的上游对象名：`host:port`（**绝不带凭据**），演练脚本按它认事件
pub fn subject_of(up: &Upstream) -> String {
    format!("{}:{}", up.host, up.port)
}

/// 告警里的「IP X」：体检学到的出口 IP，没有就退回 `host:port`
pub fn ip_of(up: &Upstream) -> String {
    up.verified
        .as_ref()
        .map(|v| v.ip.clone())
        .unwrap_or_else(|| subject_of(up))
}

/// `borrow_now` 的结果 → 「槽 1 已临时切到 Y；槽 2：没有可借用的健康 IP，保持现状」
fn borrow_text(g: &ResidentialGroup, moved: &[SlotOutcome]) -> String {
    if moved.is_empty() {
        return "当前没有槽经它出网".into();
    }
    moved
        .iter()
        .map(|o| {
            if o.switched {
                let to = g
                    .upstreams
                    .iter()
                    .find(|u| u.id == o.target)
                    .map(ip_of)
                    .unwrap_or_else(|| o.target.to_string());
                format!("槽 {} 已临时切到 {to}", o.index)
            } else {
                format!("槽 {}：{}", o.index, o.note.clone().unwrap_or_default())
            }
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn gone(id: Uuid) -> Outcome {
    Outcome {
        subject: id.to_string(),
        result: "上游已不在池里（刚被删），忽略".into(),
        level: Level::Info,
    }
}

/// relay 日志里同一上游 60 秒 ≥3 条连接错误之后：带外快探（[`health::probe_quick`]）；
/// 不可用 ⇒ 立即判不健康 + [`slots::borrow_now`] + 上游级告警「IP X 不可达，槽 i 已临时切到 Y」。
pub async fn on_upstream_error(
    ctx: &DaemonCtx,
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    id: Uuid,
    now: OffsetDateTime,
) -> Outcome {
    let g = state::group_of(&*ctx.store.read().await);
    let Some(up) = g.upstreams.iter().find(|u| u.id == id).cloned() else {
        return gone(id);
    };
    let u2 = up.clone();
    let probe = match tokio::task::spawn_blocking(move || health::probe_quick(prober.as_ref(), &u2)).await {
        Ok(p) => p,
        Err(e) => {
            return Outcome {
                subject: subject_of(&up),
                result: format!("带外探测任务异常（{e}），本次不动作"),
                level: Level::Warn,
            }
        }
    };
    if probe.ok {
        return Outcome {
            subject: subject_of(&up),
            result: "带外探测通过（日志里的错误来自目标侧或已自愈），不动作".into(),
            level: Level::Info,
        };
    }
    let why = if probe.auth_failed {
        "凭据失效（407 / SOCKS5 认证被拒）"
    } else {
        "不可达"
    };
    state::update(&ctx.runtime, move |r| {
        state::mark_unhealthy(r.health.entry(id.to_string()).or_default(), now);
    })
    .await;
    let moved = slots::borrow_now(ctx, clash, id, now).await;
    let msg = format!("IP {} {why}，{}", ip_of(&up), borrow_text(&g, &moved));
    let alert = msg.clone();
    state::update(&ctx.runtime, move |r| state::set_upstream_alert(r, id, alert)).await;
    Outcome {
        subject: subject_of(&up),
        result: msg,
        level: Level::Error,
    }
}

/// relay 日志里某上游对 Google 搜索 403（serp）之后：带外 `google_search` 复核（判据是
/// `proxy::google_ok_of` 那唯一一份，含 sorry 页）。复核通 ⇒ 不动作；封 / 没结论 ⇒
/// `google_ok = false` + [`slots::borrow_now`] + 全局告警（设计裁决 D8）。
pub async fn on_google_blocked(
    ctx: &DaemonCtx,
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    id: Uuid,
    now: OffsetDateTime,
) -> Outcome {
    let g = state::group_of(&*ctx.store.read().await);
    let Some(up) = g.upstreams.iter().find(|u| u.id == id).cloned() else {
        return gone(id);
    };
    let u2 = up.clone();
    let verdict = tokio::task::spawn_blocking(move || proxy::google_ok_of(&prober.google_search(&u2)))
        .await
        .ok()
        .flatten();
    if verdict == Some(true) {
        return Outcome {
            subject: subject_of(&up),
            result: "带外复核 Google 可用（日志可能来自已解封之前），不动作".into(),
            level: Level::Info,
        };
    }
    state::update(&ctx.runtime, move |r| {
        state::record_google(r.health.entry(id.to_string()).or_default(), Some(false), now);
    })
    .await;
    let moved = slots::borrow_now(ctx, clash, id, now).await;
    let msg = format!(
        "IP {} 的 Google 被封（serp 403 / sorry 页），{}",
        ip_of(&up),
        borrow_text(&g, &moved)
    );
    let alert = msg.clone();
    state::update(&ctx.runtime, move |r| state::push_alert(r, alert)).await;
    Outcome {
        subject: subject_of(&up),
        result: msg,
        level: Level::Warn,
    }
}

/// 每轮巡查：住宅上游被判不健康连续 [`LONG_UNREACHABLE_MINS`] 分钟 ⇒ 一次性建议替换
/// （上游级告警 + 事件）。`sr.down_since` / `sr.suggested` 随哨兵段落盘；恢复即清。
/// **只建议、不动池**：替换由管理员做，§5.6 的重分配随之自动完成。
pub async fn sweep_long_unreachable(
    ctx: &DaemonCtx,
    sr: &mut SentinelRuntime,
    now: OffsetDateTime,
) -> Vec<Outcome> {
    let g = state::group_of(&*ctx.store.read().await);
    let rt = state::read(&ctx.runtime).await;
    let in_pool = |id: &Uuid| g.upstreams.iter().any(|u| u.id == *id);
    sr.down_since.retain(|id, _| in_pool(id));
    sr.suggested.retain(|id| in_pool(id));
    let mut out = Vec::new();
    let mut alerts: Vec<(Uuid, String)> = Vec::new();
    for up in &g.upstreams {
        let active = rt
            .health
            .get(&up.id.to_string())
            .map(|h| h.active)
            .unwrap_or(true);
        if active {
            sr.down_since.remove(&up.id);
            sr.suggested.remove(&up.id);
            continue;
        }
        let since = sr
            .down_since
            .entry(up.id)
            .or_insert_with(|| fmt_rfc3339(now))
            .clone();
        let Some(t) = parse_rfc3339(&since) else {
            continue;
        };
        let mins = (now - t).whole_minutes();
        if mins < LONG_UNREACHABLE_MINS || sr.suggested.contains(&up.id) {
            continue;
        }
        let msg = format!(
            "IP {} 已连续不可达 {mins} 分钟，建议替换：面板删掉这条上游后添加新 IP，或 \
             `bui residential remove {}` 再 `bui residential add -`（替换后该槽用户自动重分配）",
            ip_of(up),
            subject_of(up)
        );
        sr.suggested.insert(up.id);
        alerts.push((up.id, msg.clone()));
        out.push(Outcome {
            subject: subject_of(up),
            result: msg,
            level: Level::Error,
        });
    }
    if !alerts.is_empty() {
        state::update(&ctx.runtime, move |r| {
            for (id, m) in alerts {
                state::set_upstream_alert(r, id, m);
            }
        })
        .await;
    }
    out
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p bui sentinel::resi 2>&1 | tail -5`
Expected: 7 条 PASS。

- [ ] **Step 6: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/sentinel/mod.rs crates/bui/src/modules/sentinel/resi.rs \
        crates/bui/src/modules/sentinel/testkit.rs crates/bui/src/modules/sentinel/incidents.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 住宅预案——带外快探、立即借用、30 分钟建议替换

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 6: 系统预案——鉴权告警、bind / 崩溃循环记事件、证书告警、xray gRPC 立即重试同步

**Files:**
- Create: `crates/bui/src/modules/sentinel/system.rs`
- Modify: `crates/bui/src/modules/sentinel/testkit.rs`（`panel_shared`）
- Modify: `crates/bui/src/modules/sentinel/mod.rs`（`pub mod system;`）

**Interfaces:**
- Consumes: Task 2 的 `signature::Sig`；Task 5 的 `incidents::Outcome`、`testkit::pool_ctx`；既有 `panel::{Shared, XRAY_API_ADDR, users::sync_now}`、`panel::fakes::{FakeXray, FakeHy2}`、`sys::{Host, Proto}`、`bui_schema::render::hysteria::AUTH_HTTP_PORT`。
- Produces:
  - `system::on_hy2_auth(&DaemonCtx, unit: &str) -> Outcome`（Error）
  - `system::on_kernel(unit: &str, Sig) -> Outcome`（Warn，只记事件）
  - `system::on_caddy_cert(domain: &str) -> Outcome`（Error，只告警）
  - `system::on_xray_grpc(&DaemonCtx, &Shared) -> Outcome`（端口在听 ⇒ `sync_now` 一轮）
  - `testkit::panel_shared(&DaemonCtx) -> (Arc<Shared>, FakeXray)`

- [ ] **Step 1: 支架与失败测试**

`crates/bui/src/modules/sentinel/mod.rs` 在 `pub mod signature;` 之后加 `pub mod system;`。

`crates/bui/src/modules/sentinel/testkit.rs` 末尾加：

```rust
/// 面板的共享句柄（gRPC / hysteria HTTP 全是内存 fake），快照写进 `ctx.paths` 的临时目录
pub fn panel_shared(
    ctx: &DaemonCtx,
) -> (
    Arc<crate::modules::panel::Shared>,
    crate::modules::panel::fakes::FakeXray,
) {
    let xray = crate::modules::panel::fakes::FakeXray::new();
    let shared = Arc::new(crate::modules::panel::Shared::new(
        Box::new(xray.clone()),
        Box::new(crate::modules::panel::fakes::FakeHy2::new()),
    ));
    shared.set_paths(&ctx.paths);
    (shared, xray)
}
```

新建 `crates/bui/src/modules/sentinel/system.rs`，先只放测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::sentinel::testkit::{panel_shared, pool_ctx};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn hy2_auth_alert_says_whether_the_daemon_listens_and_restarts_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(d.path()).await;
        let o = on_hy2_auth(&ctx, "hysteria-server").await;
        assert_eq!((o.subject.as_str(), o.level), ("hysteria-server", Level::Error));
        assert!(o.result.contains("127.0.0.1:18789") && o.result.contains("没在听"), "{}", o.result);
        host.with(|i| {
            i.listening
                .insert(Proto::Tcp, [AUTH_HTTP_PORT].into_iter().collect());
        });
        assert!(on_hy2_auth(&ctx, "hysteria-server").await.result.contains("应答超时"));
        assert!(
            host.ops().iter().all(|o| !o.starts_with("systemd:")),
            "守护进程由 systemd 拉起，哨兵不重启任何东西：{:?}",
            host.ops()
        );
    }

    #[test]
    fn kernel_and_certificate_signatures_only_record() {
        let o = on_kernel("xray", Sig::KernelBindInUse);
        assert_eq!(o.level, Level::Warn);
        assert!(o.result.contains("端口被占") && o.result.contains("交给看门狗"), "{}", o.result);
        assert!(on_kernel("hysteria-server", Sig::KernelCrashLoop).result.contains("崩溃循环"));
        let c = on_caddy_cert("panel.example.com");
        assert_eq!((c.subject.as_str(), c.level), ("panel.example.com", Level::Error));
        assert!(c.result.contains("不动作"));
    }

    #[tokio::test]
    async fn xray_grpc_trouble_runs_one_user_sync_when_the_api_port_is_up() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(d.path()).await;
        let (shared, _xray) = panel_shared(&ctx);
        host.with(|i| {
            i.listening.insert(Proto::Tcp, [10085].into_iter().collect());
        });
        let o = on_xray_grpc(&ctx, &shared).await;
        assert!(o.result.starts_with("已立即重试一轮用户同步"), "{}", o.result);
        assert_eq!(o.level, Level::Info);
        assert!(shared.snapshot_path().exists(), "sync_now 真跑了一轮（快照已重写）");
    }

    #[tokio::test]
    async fn xray_grpc_trouble_waits_when_the_api_port_is_down() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (shared, xray) = panel_shared(&ctx);
        let o = on_xray_grpc(&ctx, &shared).await;
        assert_eq!(o.level, Level::Warn);
        assert!(o.result.contains("没在听"), "{}", o.result);
        assert!(xray.calls().is_empty(), "端口都不在听，gRPC 一次都不许打");
        assert!(!shared.snapshot_path().exists());
    }

    #[test]
    fn the_xray_api_port_is_parsed_from_the_shared_constant() {
        assert_eq!(xray_api_port(), 10085);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui sentinel::system 2>&1 | tail -5`
Expected: 编译失败——`cannot find function on_hy2_auth` 等。

- [ ] **Step 3: 实现**（`system.rs`，写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 系统预案（spec §5.7）：hysteria 鉴权失败告警、bind 冲突 / 崩溃循环只记事件、caddy 证书告警、
//! xray gRPC 不可用时立即跑一轮用户同步。**不重启任何单元**：重启归 systemd 与看门狗
//! （设计裁决 D9 / D10 / D11）。

use super::incidents::{Level, Outcome};
use super::signature::Sig;
use crate::modules::panel::{users, Shared, XRAY_API_ADDR};
use crate::reconcile::DaemonCtx;
use crate::sys::{Host, Proto};
use bui_schema::render::hysteria::AUTH_HTTP_PORT;

/// `XRAY_API_ADDR`（`127.0.0.1:10085`）的端口。常量写死在面板模块里，这里只解析、不另立一份
fn xray_api_port() -> u16 {
    XRAY_API_ADDR
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or_default()
}

async fn tcp_listening(ctx: &DaemonCtx, port: u16) -> bool {
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || {
        host.listening_ports(Proto::Tcp)
            .unwrap_or_default()
            .contains(&port)
    })
    .await
    .unwrap_or(false)
}

/// hysteria 连不上 http 鉴权端口（60 秒内 ≥3 条）：记事件 + 告警，看一眼守护进程还在不在听。
/// **不重启 b-ui**：它由 systemd `Restart=always` 拉起，自己 restart 自己只会把正在收敛的对账拦腰砍断。
pub async fn on_hy2_auth(ctx: &DaemonCtx, unit: &str) -> Outcome {
    let tail = if tcp_listening(ctx, AUTH_HTTP_PORT).await {
        "守护进程在听该端口，多半是应答超时（看 b-ui 日志）"
    } else {
        "守护进程没在听该端口；b-ui 由 systemd 拉起，哨兵不重启它"
    };
    Outcome {
        subject: unit.to_string(),
        result: format!(
            "{unit} 连不上 http 鉴权端口 127.0.0.1:{AUTH_HTTP_PORT}，本实例正在全员拒绝登录；{tail}"
        ),
        level: Level::Error,
    }
}

/// bind 冲突 / 崩溃循环：交给看门狗（退避重启、孤儿链自愈）与 systemd，哨兵只记事件
pub fn on_kernel(unit: &str, sig: Sig) -> Outcome {
    let what = if sig == Sig::KernelBindInUse {
        "端口被占（bind: address already in use）"
    } else {
        "崩溃循环"
    };
    Outcome {
        subject: unit.to_string(),
        result: format!("{unit} {what}：交给看门狗（退避重启、孤儿链自愈）与 systemd，哨兵只记事件"),
        level: Level::Warn,
    }
}

/// caddy 签证书失败：只告警（caddy 自己会重试）
pub fn on_caddy_cert(domain: &str) -> Outcome {
    Outcome {
        subject: domain.to_string(),
        result: format!(
            "caddy 签 {domain} 的证书失败：caddy 会自行重试，哨兵不动作；持续失败请查 DNS 解析与 80/443 放行"
        ),
        level: Level::Error,
    }
}

/// xray gRPC 连续不可用：API 端口在听 ⇒ 立即跑一轮用户同步安全网（不等 60 秒）；不在听 ⇒ 只记事件，
/// xray 被拉起后 `users::sync_users` 的 `NRestarts` 侦测与 60 秒安全网会自己补齐。
pub async fn on_xray_grpc(ctx: &DaemonCtx, panel: &Shared) -> Outcome {
    let subject = "xray".to_string();
    if !tcp_listening(ctx, xray_api_port()).await {
        return Outcome {
            subject,
            result: format!(
                "xray 的 gRPC 端口 {XRAY_API_ADDR} 没在听：等 xray 被 systemd / 看门狗拉起后由 60 秒安全网补齐，本次不重试"
            ),
            level: Level::Warn,
        };
    }
    let out = users::sync_now(ctx, panel).await;
    if out.errors.is_empty() {
        Outcome {
            subject,
            result: format!(
                "已立即重试一轮用户同步：新增 {}、移除 {}",
                out.added.len(),
                out.removed.len()
            ),
            level: Level::Info,
        }
    } else {
        Outcome {
            subject,
            result: format!(
                "立即重试仍有 {} 项失败（{}），交给 60 秒安全网",
                out.errors.len(),
                out.errors[0]
            ),
            level: Level::Warn,
        }
    }
}
```

注意：这里写进事件与日志的文案**不许**包含 `users::USER_SYNC_FAILED_LOG`（哨兵读自己的日志，那样会自触发）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p bui sentinel::system 2>&1 | tail -5`
Expected: 5 条 PASS。

- [ ] **Step 5: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/sentinel/mod.rs crates/bui/src/modules/sentinel/system.rs \
        crates/bui/src/modules/sentinel/testkit.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 系统预案——鉴权告警、内核冲突记事件、证书告警、gRPC 不可用立即重试同步

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 7: 主循环、模块注册，watchdog 的鉴权日志分支迁到哨兵

**Files:**
- Create: `crates/bui/src/modules/sentinel/run.rs`
- Modify: `crates/bui/src/modules/sentinel/mod.rs`（`Deps` / `Notifier` / `NoopNotifier` / `SentinelModule` + 三个常量 + `pub mod run;`）
- Modify: `crates/bui/src/modules/sentinel/testkit.rs`（`rec`）
- Modify: `crates/bui/src/serve.rs`（`modules()` 注册；子集断言加 `"sentinel"`）
- Modify: `crates/bui/src/modules/watchdog.rs`（删除 http 鉴权日志分支及其孤儿项，设计裁决 D9）

**Interfaces:**
- Consumes: Task 1 `Host::journal_read` / `JournalFrom` / `JournalRecord`；Task 2 `signature::{classify, Sig, Match}`；Task 3 `engine::*`、`incidents::{push, Incident, Level}`；Task 5 `resi::*`；Task 6 `system::*`；既有 `reconcile::managed_units`、`clash::id_of_tag`、`residential::state::group_of`、`redact::line`。
- Produces:
  - `sentinel::{POLL_SECS: u64 = 5, CURSOR_PERSIST_SECS: i64 = 60, PROBE_TIMEOUT_SECS: u64 = 5}`
  - `sentinel::Notifier`（`fn notify(&self, &Incident)`）、`sentinel::NoopNotifier`
  - `sentinel::Deps { prober: Arc<dyn Prober>, clash: Arc<dyn Clash>, panel: Arc<Shared>, notifier: Arc<dyn Notifier> }`（`Clone`）
  - `sentinel::SentinelModule::{new(Arc<Shared>), with(Deps)}`，`Module::name() == "sentinel"`，`spawn` 一个任务
  - `run::{units(&State) -> Vec<String>, Sentinel（Default）, TickReport { read: usize, incidents: Vec<Incident> }, tick(&DaemonCtx, &Deps, &mut Sentinel) -> TickReport, sentinel_loop(DaemonCtx, Deps)}`
  - `testkit::rec(unit, secs, message) -> JournalRecord`（cursor = `c-<unit>-<secs>`）
  - `run` 的私有常量 `LONG_UNREACHABLE_SIG = "upstream_long_unreachable"` / `SUGGEST_REPLACE_ACTION = "suggest_replace"`：巡查事件的签名与动作串。首个使用者是本任务的 `tick`，所以在这里定义而不是 Task 5（test 构建拦 `dead_code`，bin-only crate 的 `pub const` 没人引用照样报 never used）
  - watchdog：删除 `AUTH_HTTP_KEY` / `AuthHttpAlert` / `AUTH_HTTP_COOLDOWN_MINUTES` / `should_alert` 与 `check_once` 里的鉴权分支；保留 `AUTH_HTTP_FAIL_THRESHOLD` / `AUTH_HTTP_PATH_MARKER` / `AUTH_HTTP_FAIL_MARKERS` / `is_auth_http_failure` / `count_auth_http_failures`

- [ ] **Step 1: 支架与失败测试**

`crates/bui/src/modules/sentinel/testkit.rs` 末尾加：

```rust
/// 一条日志记录：`secs` = 相对 FakeHost 默认时钟 2026-09-11T00:00:00Z 的秒数，游标 `c-<unit>-<secs>`
pub fn rec(unit: &str, secs: i64, message: &str) -> crate::sys::JournalRecord {
    crate::sys::JournalRecord {
        cursor: format!("c-{unit}-{secs}"),
        unit: unit.to_string(),
        ts: time::macros::datetime!(2026-09-11 00:00:00 UTC) + time::Duration::seconds(secs),
        message: message.to_string(),
    }
}
```

新建 `crates/bui/src/modules/sentinel/run.rs`，先只放测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::clash::{Clash, FakeClash};
    use crate::modules::residential::proxy::FakeProber;
    use crate::modules::sentinel::incidents::Level;
    use crate::modules::sentinel::testkit::{panel_shared, pool_ctx, rec};
    use crate::modules::sentinel::{Notifier, SentinelModule};
    use crate::reconcile::Module;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<Incident>>);

    impl Notifier for Recorder {
        fn notify(&self, i: &Incident) {
            self.0.lock().unwrap().push(i.clone());
        }
    }

    struct Kit {
        ctx: DaemonCtx,
        host: Arc<FakeHost>,
        deps: Deps,
        prober: Arc<FakeProber>,
        clash: Arc<FakeClash>,
        notes: Arc<Recorder>,
        _dir: tempfile::TempDir,
    }

    async fn kit() -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(dir.path()).await;
        let (panel, _xray) = panel_shared(&ctx);
        let prober = Arc::new(FakeProber::new()); // 缺省：网关连不上
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let notes = Arc::new(Recorder::default());
        let deps = Deps {
            prober: prober.clone(),
            clash: clash.clone(),
            panel,
            notifier: notes.clone(),
        };
        Kit { ctx, host, deps, prober, clash, notes, _dir: dir }
    }

    fn feed(k: &Kit, recs: Vec<crate::sys::JournalRecord>) {
        k.host.with(|i| i.journal.push_back(Ok(recs)));
    }

    fn timeout(tag: &str) -> String {
        format!(
            "ERROR[4006] [2302991392 6.42s] connection: open connection to www.gstatic.com:443 \
             using outbound/socks[{tag}]: dial tcp 198.51.100.8:10007: i/o timeout"
        )
    }

    fn journal_op(host: &FakeHost) -> String {
        host.ops()
            .into_iter()
            .rfind(|o| o.starts_with("journal:"))
            .expect("本轮读过 journald")
    }

    #[tokio::test]
    async fn the_first_round_reads_from_now_over_every_managed_unit() {
        let k = kit().await;
        let mut s = Sentinel::default();
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.read, 0);
        let op = journal_op(&k.host);
        assert!(op.ends_with(":since=2026-09-11T00:00:00Z"), "首次从现在读起、不回放：{op}");
        let units: Vec<&str> = op.split(':').nth(1).unwrap().split(',').collect();
        for u in [
            "b-ui",
            "b-ui-relay",
            "xray",
            "caddy",
            "hysteria-server",
            "hysteria-residential",
            "hysteria-residential-1",
            "hysteria-residential-2",
        ] {
            assert!(units.contains(&u), "{u} 不在 {units:?}");
        }
        assert_eq!(
            engine::sentinel_of(&k.ctx.runtime.read().await).since.as_deref(),
            Some("2026-09-11T00:00:00Z"),
            "起点落盘：重启后不会回头读更早的日志"
        );
    }

    #[tokio::test]
    async fn reading_continues_after_the_last_cursor_and_survives_a_restart() {
        let k = kit().await;
        feed(&k, vec![rec("xray", 0, "Xray 26.3.27 started")]);
        let mut s = Sentinel::default();
        assert_eq!(tick(&k.ctx, &k.deps, &mut s).await.read, 1);
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await;
        assert!(journal_op(&k.host).ends_with(":cursor=c-xray-0"), "{}", journal_op(&k.host));
        // 守护进程重启：新的 Sentinel 从 runtime 里续读
        let mut fresh = Sentinel::default();
        k.host.clear_ops();
        tick(&k.ctx, &k.deps, &mut fresh).await;
        assert!(journal_op(&k.host).ends_with(":cursor=c-xray-0"));
    }

    #[tokio::test]
    async fn a_failed_read_drops_the_cursor_and_restarts_from_now() {
        let k = kit().await;
        k.host.with(|i| {
            i.journal.push_back(Ok(vec![rec("xray", 0, "x")]));
            i.journal
                .push_back(Err("Failed to seek to cursor: Invalid argument".into()));
        });
        let mut s = Sentinel::default();
        tick(&k.ctx, &k.deps, &mut s).await;
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await; // 读失败
        k.host.advance(5);
        tick(&k.ctx, &k.deps, &mut s).await;
        assert!(
            journal_op(&k.host).ends_with(":since=2026-09-11T00:00:05Z"),
            "游标失效 ⇒ 从失败那一刻读起，不回放：{}",
            journal_op(&k.host)
        );
    }

    #[tokio::test]
    async fn three_timeouts_to_one_upstream_give_one_probe_one_borrow_one_incident() {
        let k = kit().await;
        k.host.advance(3);
        feed(&k, (0..3).map(|s| rec("b-ui-relay", s, &timeout("resi-2"))).collect());
        let mut s = Sentinel::default();
        let rep = tick(&k.ctx, &k.deps, &mut s).await;
        assert_eq!(rep.incidents.len(), 1, "{:?}", rep.incidents);
        let inc = &rep.incidents[0];
        assert_eq!(
            (
                inc.unit.as_str(),
                inc.signature.as_str(),
                inc.subject.as_str(),
                inc.action.as_str()
            ),
            (
                "b-ui-relay",
                "relay_upstream_error",
                "isp2.example.net:10007",
                "probe_and_borrow"
            )
        );
        assert_eq!(inc.level, Level::Error);
        assert_eq!(inc.result, "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7");
        assert_eq!(inc.at, "2026-09-11T00:00:03Z");
        assert!(inc.sample.as_deref().unwrap().contains("i/o timeout"));
        assert_eq!(k.clash.selected("slot-1-pool").as_deref(), Some("resi-1"));
        assert_eq!(
            incidents::from_runtime(&k.ctx.runtime.read().await),
            rep.incidents,
            "事件落盘"
        );
        assert_eq!(k.notes.0.lock().unwrap().len(), 1, "外部通知口收到一次");

        // 10 秒后又来 3 条：去抖窗口内，不触发
        k.host.advance(10);
        feed(&k, (13..16).map(|s| rec("b-ui-relay", s, &timeout("resi-2"))).collect());
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty());
        // 再过 61 秒又来 3 条：去抖已过，但同动作同对象 10 分钟冷却
        k.host.advance(61);
        feed(&k, (74..77).map(|s| rec("b-ui-relay", s, &timeout("resi-2"))).collect());
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty());
        assert_eq!(k.prober.calls(), vec!["tcp"], "整段只探了一次");
    }

    #[tokio::test]
    async fn two_timeouts_unknown_tags_and_stale_backlog_do_nothing() {
        let k = kit().await;
        let mut s = Sentinel::default();
        feed(&k, (0..2).map(|s| rec("b-ui-relay", s, &timeout("resi-2"))).collect());
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(), "不到 3 条");
        feed(&k, (0..3).map(|s| rec("b-ui-relay", s, &timeout("resi-9"))).collect());
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(), "池里没有 resi-9");
        feed(&k, (-300..-297).map(|s| rec("b-ui-relay", s, &timeout("resi-3"))).collect());
        assert!(tick(&k.ctx, &k.deps, &mut s).await.incidents.is_empty(), "5 分钟前的积压");
        assert!(k.prober.calls().is_empty() && k.clash.calls().is_empty());
    }

    const AUTH_FAIL: &str = "hysteria[1]: authentication error {\"error\": \"Post \
        \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: connect: connection refused\"}";

    #[tokio::test]
    async fn hysteria_auth_failures_alert_in_http_mode() {
        let k = kit().await;
        feed(&k, (0..3).map(|s| rec("hysteria-residential-1", s, AUTH_FAIL)).collect());
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1);
        assert_eq!(rep.incidents[0].signature, "hy2_auth_http_failed");
        assert_eq!(rep.incidents[0].subject, "hysteria-residential-1");
        assert_eq!(rep.incidents[0].action, "alert");
        assert!(k.host.ops().iter().all(|o| !o.starts_with("systemd:")));
    }

    #[tokio::test]
    async fn hysteria_auth_lines_are_ignored_in_command_mode() {
        let k = kit().await;
        k.ctx
            .store
            .update(|s| s.system.hy2_auth = bui_schema::model::Hy2Auth::Command)
            .await
            .unwrap();
        feed(&k, (0..3).map(|s| rec("hysteria-server", s, AUTH_FAIL)).collect());
        assert!(tick(&k.ctx, &k.deps, &mut Sentinel::default())
            .await
            .incidents
            .is_empty());
    }

    #[tokio::test]
    async fn a_bind_conflict_is_recorded_and_left_to_the_watchdog() {
        let k = kit().await;
        feed(
            &k,
            vec![rec(
                "xray",
                0,
                "Failed to start: main: failed to start server > listen tcp 0.0.0.0:10001: \
                 bind: address already in use",
            )],
        );
        let rep = tick(&k.ctx, &k.deps, &mut Sentinel::default()).await;
        assert_eq!(rep.incidents.len(), 1);
        assert_eq!(
            (rep.incidents[0].signature.as_str(), rep.incidents[0].action.as_str()),
            ("kernel_bind_in_use", "delegate_watchdog")
        );
        assert!(k.host.ops().iter().all(|o| !o.starts_with("systemd:")));
    }

    #[tokio::test]
    async fn the_module_renders_nothing_and_spawns_one_task() {
        let k = kit().await;
        let m = SentinelModule::with(k.deps.clone());
        assert_eq!(m.name(), "sentinel");
        let handles = m.spawn(k.ctx.clone());
        assert_eq!(handles.len(), 1);
        for h in handles {
            h.abort();
        }
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui sentinel::run 2>&1 | tail -5`
Expected: 编译失败——`cannot find struct Sentinel` / `Deps` / `fn tick`。

- [ ] **Step 3: `mod.rs` 的模块、依赖与通知口**

`crates/bui/src/modules/sentinel/mod.rs` 的 `pub mod` 列表加 `pub mod run;`，并在常量后追加：

```rust
/// 每轮增量读 journald 的间隔（设计裁决 D1）
pub const POLL_SECS: u64 = 5;
/// 只有游标前进时，至少隔这么久才把游标落盘一次（hysteria 在 info 级每条连接都写日志）
pub const CURSOR_PERSIST_SECS: i64 = 60;
/// 带外快探的超时（设计裁决 D5：演练要求 ≤15 秒出事件）
pub const PROBE_TIMEOUT_SECS: u64 = 5;

use crate::modules::panel::Shared;
use crate::modules::residential::clash::{Clash, HttpClash};
use crate::modules::residential::proxy::{Prober, ReqwestProber};
use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use bui_schema::model::State;
use std::sync::Arc;

/// 外部通知（Telegram / Webhook）的预留口（spec §5.7：本期不做）。每条事件落盘后调一次。
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, incident: &incidents::Incident);
}

/// 本期的实现：什么都不做
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn notify(&self, _incident: &incidents::Incident) {}
}

/// 哨兵的外部依赖（测试注入 fake）
#[derive(Clone)]
pub struct Deps {
    /// 带外快探用（生产：`ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS)`）
    pub prober: Arc<dyn Prober>,
    /// 按槽借用用（relay 的 Clash API）
    pub clash: Arc<dyn Clash>,
    /// 用户同步重试用（面板模块的共享句柄）
    pub panel: Arc<Shared>,
    pub notifier: Arc<dyn Notifier>,
}

pub struct SentinelModule {
    deps: Deps,
}

impl SentinelModule {
    /// 生产构造：`panel` 是 `PanelModule::shared()`（`serve::modules` 里传）
    pub fn new(panel: Arc<Shared>) -> Self {
        Self::with(Deps {
            prober: Arc::new(ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS)),
            clash: Arc::new(HttpClash::new()),
            panel,
            notifier: Arc::new(NoopNotifier),
        })
    }

    pub fn with(deps: Deps) -> Self {
        Self { deps }
    }
}

impl Module for SentinelModule {
    fn name(&self) -> &'static str {
        "sentinel"
    }

    /// 没有期望项，只有后台任务（与 watchdog 同）
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(run::sentinel_loop(ctx, self.deps.clone()))]
    }
}
```

- [ ] **Step 4: 实现主循环**（`run.rs`，写在 `#[cfg(test)] mod tests` 之前）

```rust
//! 哨兵主循环（spec §5.7）：每 [`POLL_SECS`] 秒增量读一次 journald → 签名匹配 → 去抖 → 冷却 →
//! 预案 → 事件落盘 + 通知口。游标以内存为准，落盘有节流（设计裁决 D1）。

use super::engine::{self, Engine, SentinelRuntime};
use super::incidents::{self, Incident, Level, Outcome};
use super::signature::{self, Sig};
use super::{resi, system, Deps, CURSOR_PERSIST_SECS, POLL_SECS};
use crate::modules::residential::{clash, state as rstate};
use crate::reconcile::DaemonCtx;
use crate::sys::{Host, JournalFrom, JournalRecord};
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{Hy2Auth, State};
use time::OffsetDateTime;
use uuid::Uuid;

/// 30 分钟建议替换不是日志签名，是巡查产出（[`resi::sweep_long_unreachable`]）；事件里用这两个串
const LONG_UNREACHABLE_SIG: &str = "upstream_long_unreachable";
const SUGGEST_REPLACE_ACTION: &str = "suggest_replace";

/// 哨兵要跟的单元 = 受管单元全集：relay / hysteria-server / 每个住宅槽的 hysteria / xray / caddy，
/// 外加 b-ui 自己——xray gRPC 的失败只出现在守护进程自己的日志里（设计裁决 D2 / D10）。
pub fn units(state: &State) -> Vec<String> {
    crate::reconcile::managed_units(state)
}

/// 常驻在循环里的哨兵状态
#[derive(Default)]
pub struct Sentinel {
    engine: Engine,
    /// 首轮从 runtime 读出，此后**以内存为准**（落盘有节流；每轮都从 runtime 读会重复读同一段日志）
    sr: Option<SentinelRuntime>,
    last_persist: Option<OffsetDateTime>,
    /// 上一次读 journald 的错误文案：同样的错误只打一次日志（没装 journalctl 时每 5 秒一条是刷屏）
    last_error: Option<String>,
}

/// 一轮的报告（测试断言用）
#[derive(Debug, Default)]
pub struct TickReport {
    pub read: usize,
    pub incidents: Vec<Incident>,
}

/// 一次触发：签名、计数键（relay 是上游 uuid 串）、上游 uuid（仅 relay 签名）、那条日志、匹配结果
type Fired = (Sig, String, Option<Uuid>, JournalRecord, signature::Match);

pub async fn tick(ctx: &DaemonCtx, deps: &Deps, s: &mut Sentinel) -> TickReport {
    let now = ctx.host.now();
    let state = ctx.store.read().await;
    let units = units(&state);
    let hy2_http = state.system.hy2_auth == Hy2Auth::Http;
    let g = rstate::group_of(&state);
    drop(state);
    if s.sr.is_none() {
        s.sr = Some(engine::sentinel_of(&ctx.runtime.read().await));
    }
    let mut sr = s.sr.clone().unwrap_or_default();
    let before = sr.clone();

    // ① 读：有游标续读；没有就从 `since`（首次 = 现在）读起，不回放历史
    let from = match (&sr.cursor, sr.since.as_deref().and_then(parse_rfc3339)) {
        (Some(c), _) => JournalFrom::Cursor(c.clone()),
        (None, Some(t)) => JournalFrom::Since(t),
        (None, None) => {
            sr.since = Some(fmt_rfc3339(now));
            JournalFrom::Since(now)
        }
    };
    let host = ctx.host.clone();
    let (u2, f2) = (units.clone(), from.clone());
    let records = match tokio::task::spawn_blocking(move || host.journal_read(&u2, &f2)).await {
        Ok(Ok(v)) => {
            s.last_error = None;
            v
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            if s.last_error.as_deref() != Some(msg.as_str()) {
                tracing::warn!(error = %msg, "哨兵读 journald 失败：丢掉游标，从现在读起（不回放）");
            }
            s.last_error = Some(msg);
            sr.cursor = None;
            sr.since = Some(fmt_rfc3339(now));
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "哨兵读 journald 的任务异常");
            Vec::new()
        }
    };
    if let Some(last) = records.last() {
        sr.cursor = Some(last.cursor.clone());
    }

    // ② 匹配 + 去抖。relay 的对象当场从成员 tag（位置键）换成 uuid，换不出来的丢弃
    let mut fired: Vec<Fired> = Vec::new();
    for r in &records {
        let Some(m) = signature::classify(&r.unit, &r.message) else {
            continue;
        };
        if m.sig == Sig::Hy2AuthHttpFailed && !hy2_http {
            continue;
        }
        let upstream = match m.sig {
            Sig::RelayUpstreamError | Sig::RelayGoogleBlocked => {
                match clash::id_of_tag(&g, &m.subject) {
                    Some(id) => Some(id),
                    None => continue,
                }
            }
            _ => None,
        };
        let key = upstream.map_or_else(|| m.subject.clone(), |id| id.to_string());
        if s.engine.observe(m.sig, &key, r.ts, now) {
            fired.push((m.sig, key, upstream, r.clone(), m));
        }
    }
    s.engine.prune(now);
    engine::prune_acted(&mut sr.acted, now);

    // ③ 冷却 + 预案
    let mut out: Vec<Incident> = Vec::new();
    for (sig, key, upstream, r, m) in fired {
        let akey = engine::action_key(sig, &key);
        if engine::in_cooldown(&sr.acted, &akey, now) {
            tracing::debug!(signature = sig.id(), subject = %key, "同动作冷却中，跳过");
            continue;
        }
        let o = dispatch(ctx, deps, sig, &key, upstream, &r, now).await;
        sr.acted.insert(akey, fmt_rfc3339(now));
        out.push(Incident {
            // 预案做完（快探最长 PROBE_TIMEOUT_SECS + 借用的 PUT）之后才盖时间戳：事件时刻 = 该槽
            // 已借用的时刻。演练判据①「首条错误 → 记事件并借用 ≤15 秒」量的是它，不是本轮开头
            at: fmt_rfc3339(ctx.host.now()),
            unit: r.unit.clone(),
            signature: sig.id().to_string(),
            subject: o.subject,
            action: sig.action().id().to_string(),
            result: crate::redact::line(&o.result),
            level: o.level,
            sample: Some(m.detail),
        });
    }
    // ④ 巡查：长时间不可达建议替换
    for o in resi::sweep_long_unreachable(ctx, &mut sr, now).await {
        out.push(Incident {
            at: fmt_rfc3339(now),
            unit: "b-ui-relay".into(),
            signature: LONG_UNREACHABLE_SIG.into(),
            subject: o.subject,
            action: SUGGEST_REPLACE_ACTION.into(),
            result: o.result,
            level: o.level,
            sample: None,
        });
    }

    // ⑤ 落盘：有事件 / 冷却表或起点变了 ⇒ 立即；只有游标前进 ⇒ 至少隔 CURSOR_PERSIST_SECS
    let others_changed = {
        let (mut a, mut b) = (sr.clone(), before.clone());
        a.cursor = None;
        b.cursor = None;
        a != b
    };
    let cursor_due = sr.cursor != before.cursor
        && s
            .last_persist
            .is_none_or(|t| now - t >= time::Duration::seconds(CURSOR_PERSIST_SECS));
    if !out.is_empty() || others_changed || cursor_due {
        let (incs, snap) = (out.clone(), sr.clone());
        ctx.runtime
            .update(move |rt| {
                for i in incs {
                    incidents::push(rt, i);
                }
                engine::put_sentinel(rt, &snap);
            })
            .await;
        s.last_persist = Some(now);
    }
    for i in &out {
        match i.level {
            Level::Info => tracing::info!(signature = %i.signature, subject = %i.subject, "哨兵事件：{}", i.result),
            _ => tracing::warn!(signature = %i.signature, subject = %i.subject, action = %i.action, "哨兵事件：{}", i.result),
        }
        deps.notifier.notify(i);
    }
    s.sr = Some(sr);
    TickReport {
        read: records.len(),
        incidents: out,
    }
}

async fn dispatch(
    ctx: &DaemonCtx,
    deps: &Deps,
    sig: Sig,
    key: &str,
    upstream: Option<Uuid>,
    r: &JournalRecord,
    now: OffsetDateTime,
) -> Outcome {
    match (sig, upstream) {
        (Sig::RelayUpstreamError, Some(id)) => {
            resi::on_upstream_error(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        (Sig::RelayGoogleBlocked, Some(id)) => {
            resi::on_google_blocked(ctx, deps.prober.clone(), deps.clash.clone(), id, now).await
        }
        // relay 签名的对象在匹配那一步就换成了 uuid（换不出来的已丢弃）
        (Sig::RelayUpstreamError | Sig::RelayGoogleBlocked, None) => {
            unreachable!("relay 签名必带上游 uuid")
        }
        (Sig::Hy2AuthHttpFailed, _) => system::on_hy2_auth(ctx, &r.unit).await,
        (Sig::KernelBindInUse | Sig::KernelCrashLoop, _) => system::on_kernel(&r.unit, sig),
        (Sig::XrayGrpcUnavailable, _) => system::on_xray_grpc(ctx, &deps.panel).await,
        (Sig::CaddyCertFailed, _) => system::on_caddy_cert(key),
    }
}

/// 每 [`POLL_SECS`] 秒一轮；一轮里的预案（带外探测最长 ~5 秒）拖长了就顺延，不补跑
pub async fn sentinel_loop(ctx: DaemonCtx, deps: Deps) {
    let mut s = Sentinel::default();
    let mut every = tokio::time::interval(std::time::Duration::from_secs(POLL_SECS));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        tick(&ctx, &deps, &mut s).await;
    }
}
```

- [ ] **Step 5: 在守护进程里注册**（`crates/bui/src/serve.rs`）

`modules()` 的 `modules: vec![ … ]` 在 `Arc::new(crate::modules::residential::ResidentialModule::new()),` 之后加一行：

```rust
            // spec §5.7：日志哨兵（xray gRPC 预案要借面板的 Shared 重试用户同步）
            Arc::new(crate::modules::sentinel::SentinelModule::new(shared.clone())),
```

测试 `p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 的名单数组末尾加 `"sentinel",`。

- [ ] **Step 6: watchdog 的鉴权日志分支迁走（设计裁决 D9）**

`crates/bui/src/modules/watchdog.rs`：

1. 删除 `pub const AUTH_HTTP_KEY`、`pub const AUTH_HTTP_COOLDOWN_MINUTES`、`pub struct AuthHttpAlert`（连同各自的文档注释）与 `pub fn should_alert`。
2. 把 `AUTH_HTTP_KEY` 上方那段文档注释（「http 鉴权连不上的哨兵…事件落 `runtime.extra` 的这个键」）改写到 `AUTH_HTTP_FAIL_THRESHOLD` 上：

```rust
/// http 鉴权连不上的判据（spec §3.2）：`auth.type: http` 下内核每条连接都要打一次
/// `127.0.0.1:AUTH_HTTP_PORT`，守护进程没在听（或应答超时）就是全员登录失败。
/// **检测与告警在日志哨兵**（`modules::sentinel`，5 秒增量读 journald）；这里只留判据与门槛，
/// 哨兵的签名表引用它们。
/// 多少条鉴权连接失败（60 秒内）才算一次事件。
pub const AUTH_HTTP_FAIL_THRESHOLD: u32 = 3;
```

3. `check_once`：删掉 `let auth_http = …;` 一行、`let mut alerts: … = rt.extra.get(AUTH_HTTP_KEY)…;` 整段、`if auth_http { … }` 整段、`update` 闭包里 `if !alerts.is_empty() { … }` 整段；元组 `(decisions, records, heals, alerts, now)` 改为 `(decisions, records, heals, now)`，`Ok((out, records, heals, alerts, now))` 改为 `Ok((out, records, heals, now))`。
4. 测试：删除 `repeated_auth_endpoint_failures_are_recorded_without_touching_anything` 与 `the_sentinel_is_silent_in_command_mode`（两者的行为由 Task 7 的 `hysteria_auth_failures_alert_in_http_mode` / `hysteria_auth_lines_are_ignored_in_command_mode` 接管）；`only_lines_about_the_auth_endpoint_failing_are_counted` 末尾三条 `should_alert` 断言删掉；`acting_ops` 的文档注释改为「只看真正动了机器的操作：孤儿链自愈会先读一次崩溃单元的 journal，这些只读调用不属于任何一条动作断言」。
5. 追加回归测试：

```rust
    /// 鉴权日志由哨兵负责（5 秒增量读，设计裁决 D9）：watchdog 不再每 60 秒自己翻 hysteria 的日志，
    /// 也不再写 `hy2_auth_http` 键——同一故障不许两处各报一次。
    #[tokio::test]
    async fn the_watchdog_leaves_the_auth_log_to_the_sentinel() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in ["hysteria-server", "hysteria-residential", "xray", "b-ui-relay"] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening
                .insert(Proto::Udp, [10000, 40000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            !host.ops().iter().any(|o| o.contains("journalctl")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key("hy2_auth_http"));
    }
```

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p bui sentinel:: 2>&1 | tail -5 && cargo test -p bui watchdog 2>&1 | tail -3 && cargo test -p bui serve 2>&1 | tail -3`
Expected: 全部 PASS。

- [ ] **Step 8: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。另 `grep -rn "AUTH_HTTP_KEY\|AuthHttpAlert\|should_alert" crates/` 无输出。

- [ ] **Step 9: Commit**

```bash
git add crates/bui/src/modules/sentinel/mod.rs crates/bui/src/modules/sentinel/run.rs \
        crates/bui/src/modules/sentinel/testkit.rs crates/bui/src/serve.rs \
        crates/bui/src/modules/watchdog.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 主循环与模块注册，鉴权日志检测从 watchdog 迁到哨兵

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 8: `GET /api/incidents`、`bui incidents`、`bui status` 最近 5 条

**Files:**
- Create: `crates/bui/src/modules/sentinel/api.rs`
- Create: `crates/bui/src/commands/incidents.rs`
- Modify: `crates/bui/src/modules/sentinel/mod.rs`（`pub mod api;` + `Module::routes`）
- Modify: `crates/bui/src/modules/sentinel/incidents.rs`（`format_line` / `format_list` / `load_recent`）
- Modify: `crates/bui/src/commands/mod.rs`、`crates/bui/src/cli.rs`、`crates/bui/src/main.rs`
- Modify: `crates/bui/src/commands/status.rs`

**Interfaces:**
- Consumes: Task 3 `incidents::{from_runtime, Incident, Level}`、`INCIDENTS_MAX`；Task 7 `SentinelModule`；既有 `api::AppState`、`ipc::Client`、`state::runtime::Runtime`、`paths::{runtime_file, SOCKET_PATH}`。
- Produces:
  - `GET /api/incidents?limit=N` → `{"incidents": [Incident…], "total": <usize>}`（缺省 50，夹到 1..=200，管理员鉴权）
  - `sentinel::api::{routes() -> Router<AppState>, IncidentsQuery, IncidentsResponse { incidents, total }, DEFAULT_LIMIT = 50}`
  - `incidents::{format_line(&Incident) -> String, format_list(&[Incident]) -> String, load_recent(&Path, &Paths, usize) -> (Vec<Incident>, bool /*来自守护进程*/)}`
  - CLI：`bui incidents [--json] [-n N]`（`Command::Incidents { json: bool, n: usize }`，`-n` 缺省 20）；`--json` 输出 `{"incidents": […], "source": "daemon" | "runtime.json"}`
  - `commands::status::{STATUS_INCIDENTS = 5, format_recent_incidents(&[Incident]) -> String}`

- [ ] **Step 1: 写失败测试**

新建 `crates/bui/src/modules/sentinel/api.rs`，先只放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::sentinel::incidents::{push, Level};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn inc(n: usize) -> Incident {
        Incident {
            at: format!("2026-09-11T00:00:{:02}Z", n),
            unit: "xray".into(),
            signature: "kernel_bind_in_use".into(),
            subject: "xray".into(),
            action: "delegate_watchdog".into(),
            result: format!("第 {n} 条"),
            level: Level::Warn,
            sample: None,
        }
    }

    async fn app() -> (AppState, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state())
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        runtime
            .update(|r| {
                for n in 0..5 {
                    push(r, inc(n));
                }
            })
            .await;
        let host = Arc::new(FakeHost::new());
        let started_at = host.now();
        (
            AppState {
                store,
                bus: EventBus::new(),
                runtime,
                host,
                started_at,
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            d,
        )
    }

    #[tokio::test]
    async fn incidents_come_newest_first_and_the_limit_is_clamped() {
        let (a, _d) = app().await;
        let Json(r) = get_incidents(State(a.clone()), Query(IncidentsQuery { limit: Some(2) })).await;
        assert_eq!(r.total, 5);
        assert_eq!(
            r.incidents.iter().map(|i| i.result.as_str()).collect::<Vec<_>>(),
            vec!["第 4 条", "第 3 条"]
        );
        let Json(r) = get_incidents(State(a.clone()), Query(IncidentsQuery { limit: Some(0) })).await;
        assert_eq!(r.incidents.len(), 1, "下限 1");
        let Json(r) = get_incidents(State(a), Query(IncidentsQuery { limit: None })).await;
        assert_eq!(r.incidents.len(), 5, "缺省 50，不足全给");
    }

    #[tokio::test]
    async fn the_endpoint_sits_behind_admin_auth() {
        let (a, _d) = app().await;
        let panel = Arc::new(crate::modules::panel::Shared::new(
            Box::new(crate::modules::panel::fakes::FakeXray::new()),
            Box::new(crate::modules::panel::fakes::FakeHy2::new()),
        ));
        let m: Arc<dyn crate::reconcile::Module> =
            Arc::new(crate::modules::sentinel::SentinelModule::new(panel));
        let router = crate::api::router(a.clone(), &[m]);
        let res = router
            .oneshot(Request::get("/api/incidents").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
```

`crates/bui/src/modules/sentinel/incidents.rs` 的 `mod tests` 末尾加：

```rust
    #[test]
    fn one_line_per_incident_for_the_terminal() {
        let mut i = inc(7);
        i.result = "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7".into();
        assert_eq!(
            format_line(&i),
            "2026-09-11T00:00:07Z [告警] b-ui-relay relay_upstream_error isp2.example.net:10007 \
             → probe_and_borrow：IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(format_list(&[]), "（暂无事件）");
        assert_eq!(format_list(&[inc(1), inc(2)]).lines().count(), 2);
    }

    /// 守护进程没跑（socket 连不上）⇒ 直接读 runtime.json，并如实报告来源
    #[tokio::test]
    async fn load_recent_falls_back_to_runtime_json_when_the_daemon_is_down() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let rt = crate::state::runtime::Runtime::load(crate::paths::runtime_file(&paths));
        rt.update(|r| {
            for n in 0..8 {
                push(r, inc(n));
            }
        })
        .await;
        let (v, live) = load_recent(&d.path().join("no.sock"), &paths, 5).await;
        assert!(!live);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0].result, "第 7 条");
    }
```

`crates/bui/src/commands/status.rs` 的 `mod tests` 末尾加：

```rust
    #[test]
    fn status_ends_with_the_latest_incidents() {
        use crate::modules::sentinel::incidents::{Incident, Level};
        assert_eq!(
            format_recent_incidents(&[]),
            "最近事件    无（`bui incidents` 查看全量）"
        );
        let i = Incident {
            at: "2026-09-11T00:00:03Z".into(),
            unit: "b-ui-relay".into(),
            signature: "relay_upstream_error".into(),
            subject: "isp2.example.net:10007".into(),
            action: "probe_and_borrow".into(),
            result: "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7".into(),
            level: Level::Error,
            sample: None,
        };
        let t = format_recent_incidents(&[i.clone(), i]);
        assert!(t.starts_with("最近事件    （`bui incidents` 查看全量）"), "{t}");
        assert_eq!(t.lines().count(), 3);
        assert!(t.contains("槽 1 已临时切到 198.51.100.7"));
        assert_eq!(STATUS_INCIDENTS, 5, "spec §5.7：status 只显示最近 5 条");
    }
```

`crates/bui/src/cli.rs` 的 `mod tests` 末尾加：

```rust
    #[test]
    fn parses_incidents_with_a_count_and_json() {
        assert_eq!(
            Cli::try_parse_from(["bui", "incidents"]).unwrap().command,
            Some(Command::Incidents { json: false, n: 20 })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "incidents", "-n", "5", "--json"])
                .unwrap()
                .command,
            Some(Command::Incidents { json: true, n: 5 })
        );
        assert!(Cli::try_parse_from(["bui", "incidents", "-n", "x"]).is_err());
    }
```

- [ ] **Step 2: 跑测试确认失败**

先在 `crates/bui/src/modules/sentinel/mod.rs` 的 `pub mod` 列表加 `pub mod api;`。
Run: `cargo test -p bui incidents 2>&1 | tail -5`
Expected: 编译失败——`cannot find function get_incidents` / `format_line` / `Command::Incidents`。

- [ ] **Step 3: 端点**（`api.rs`，写在测试之前）

```rust
//! `GET /api/incidents?limit=N`：最近的哨兵事件，新的在前（spec §5.7）。管理员鉴权由
//! `api::router` 统一挂（模块路由都在 `require_admin` 里面），本模块不加中间件。

use super::incidents::{self, Incident};
use super::INCIDENTS_MAX;
use crate::api::AppState;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::Json;
use serde::{Deserialize, Serialize};

/// 不带 `limit` 时给多少条
pub const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize)]
pub struct IncidentsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct IncidentsResponse {
    pub incidents: Vec<Incident>,
    /// 环里一共有几条（≤ INCIDENTS_MAX）
    pub total: usize,
}

pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/api/incidents", get(get_incidents))
}

pub async fn get_incidents(
    State(app): State<AppState>,
    Query(q): Query<IncidentsQuery>,
) -> Json<IncidentsResponse> {
    let all = incidents::from_runtime(&app.runtime.read().await);
    let n = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, INCIDENTS_MAX);
    Json(IncidentsResponse {
        total: all.len(),
        incidents: all.into_iter().take(n).collect(),
    })
}
```

`mod.rs` 的 `impl Module for SentinelModule` 里加：

```rust
    fn routes(&self) -> axum::Router<crate::api::AppState> {
        api::routes()
    }
```

- [ ] **Step 4: 格式化与 CLI 取数**（`incidents.rs`，`push` 之后）

```rust
impl Level {
    /// 终端与面板上的中文等级
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "信息",
            Level::Warn => "警告",
            Level::Error => "告警",
        }
    }
}

/// 一行人读：`<时间> [<等级>] <单元> <签名> <对象> → <动作>：<结果>`
pub fn format_line(i: &Incident) -> String {
    format!(
        "{} [{}] {} {} {} → {}：{}",
        i.at,
        i.level.label(),
        i.unit,
        i.signature,
        i.subject,
        i.action,
        i.result
    )
}

pub fn format_list(v: &[Incident]) -> String {
    if v.is_empty() {
        return "（暂无事件）".into();
    }
    v.iter().map(format_line).collect::<Vec<_>>().join("\n")
}

/// CLI 取最近 `n` 条：守护进程在跑就经 socket 调 `/api/incidents`，否则直接读 `runtime.json`。
/// 返回 `(事件, 是否来自守护进程)`。
pub async fn load_recent(
    socket: &std::path::Path,
    paths: &bui_schema::paths::Paths,
    n: usize,
) -> (Vec<Incident>, bool) {
    let client = crate::ipc::Client::new(socket);
    if client.available().await {
        if let Ok((200, v)) = client
            .request("GET", &format!("/api/incidents?limit={n}"), None)
            .await
        {
            if let Ok(r) = serde_json::from_value::<super::api::IncidentsResponse>(v) {
                return (r.incidents, true);
            }
        }
    }
    let rt = crate::state::runtime::Runtime::load(crate::paths::runtime_file(paths))
        .read()
        .await;
    (from_runtime(&rt).into_iter().take(n).collect(), false)
}
```

- [ ] **Step 5: `bui incidents` 子命令**

新建 `crates/bui/src/commands/incidents.rs`：

```rust
//! `bui incidents [--json] [-n N]`：日志哨兵的事件，新的在前（spec §5.7）。
//! 守护进程在跑就经 socket 读，否则直接读 `runtime.json`（与 `bui status` 同一退化口径）。

use crate::modules::sentinel::incidents;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::path::PathBuf;

pub async fn run(json: bool, n: usize, paths: Paths, socket: PathBuf) -> Result<()> {
    let (list, live) = incidents::load_recent(&socket, &paths, n).await;
    if json {
        let v = serde_json::json!({
            "incidents": list,
            "source": if live { "daemon" } else { "runtime.json" },
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        if !live {
            eprintln!("守护进程未运行，以下读自 runtime.json");
        }
        println!("{}", incidents::format_list(&list));
    }
    Ok(())
}
```

`crates/bui/src/commands/mod.rs` 按字母序加 `pub mod incidents;`（在 `pub mod import_v3;` 之后）。

`crates/bui/src/cli.rs` 的 `enum Command`，`Status { … },` 之后加：

```rust
    /// 日志哨兵的事件（新的在前）
    Incidents {
        /// 输出 JSON（`{"incidents": [...], "source": "daemon" | "runtime.json"}`）
        #[arg(long)]
        json: bool,
        /// 显示最近几条
        #[arg(short = 'n', long = "count", default_value_t = 20)]
        n: usize,
    },
```

`crates/bui/src/main.rs` 的 `dispatch`，`Command::Status { json } => { … }` 之后加：

```rust
        Command::Incidents { json, n } => {
            commands::incidents::run(
                json,
                n,
                bui_schema::paths::Paths::default_server(),
                PathBuf::from(paths::SOCKET_PATH),
            )
            .await
        }
```

- [ ] **Step 6: `bui status` 末尾的最近事件**（`crates/bui/src/commands/status.rs`）

`format_status` 之后加：

```rust
/// `bui status` 末尾显示几条事件（spec §5.7；全量看 `bui incidents`）
pub const STATUS_INCIDENTS: usize = 5;

/// `bui status` 的「最近事件」段；`--json` 不带它（`/api/health` 的形状是 P2 锁死的回归面）
pub fn format_recent_incidents(v: &[crate::modules::sentinel::incidents::Incident]) -> String {
    if v.is_empty() {
        return "最近事件    无（`bui incidents` 查看全量）".into();
    }
    let mut out = vec!["最近事件    （`bui incidents` 查看全量）".to_string()];
    out.extend(v.iter().map(|i| {
        format!(
            "            {}",
            crate::modules::sentinel::incidents::format_line(i)
        )
    }));
    out.join("\n")
}
```

`run_with` 里把

```rust
    } else {
        println!("{}", format_status(&health, hy2_auth));
    }
```

改为

```rust
    } else {
        println!("{}", format_status(&health, hy2_auth));
        let (recent, _) =
            crate::modules::sentinel::incidents::load_recent(&socket, &paths, STATUS_INCIDENTS)
                .await;
        println!("{}", format_recent_incidents(&recent));
    }
```

（`socket` 与 `paths` 在这之前只被借用过，所有权仍在 `run_with` 手里。）

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p bui incidents 2>&1 | tail -5 && cargo test -p bui status 2>&1 | tail -3 && cargo test -p bui cli 2>&1 | tail -3`
Expected: 全部 PASS。

- [ ] **Step 8: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 9: Commit**

```bash
git add crates/bui/src/modules/sentinel/api.rs crates/bui/src/modules/sentinel/mod.rs \
        crates/bui/src/modules/sentinel/incidents.rs crates/bui/src/commands/incidents.rs \
        crates/bui/src/commands/mod.rs crates/bui/src/cli.rs crates/bui/src/main.rs \
        crates/bui/src/commands/status.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): GET /api/incidents、bui incidents 与 bui status 最近事件

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 9: 面板「事件」卡（只加卡，不重构）

**Files:**
- Modify: `web/index.html`（系统状态栅格里追加一张卡）
- Modify: `web/app.js`（`loadIncidents()` + `initSysStatusOnce` 追加一行）
- Modify: `crates/bui/src/modules/panel/assets.rs`（守门测试）

**Interfaces:**
- Consumes: Task 8 的 `GET /api/incidents?limit=20` → `{incidents: [{at, unit, signature, subject, action, result, level, sample?}], total}`；既有前端助手 `api()` / `_sysShimmer` / `_sysErr` / `_sysClear` / `_sysKv` / `_sysTag` / `_sysFmt`。
- Produces: DOM `#sys-inc-card` / `#sys-inc-body` / `#inc-refresh`；全局函数 `loadIncidents()`。样式全部复用 `.sysstat-*`（`.sysstat-grid` 是 `auto-fit, minmax(320px, 1fr)`，第三张卡自动换行），`style.css` 不改。

- [ ] **Step 1: 写失败测试**（`crates/bui/src/modules/panel/assets.rs` 的 `mod tests` 末尾）

```rust
    /// spec §5.7：面板「事件」卡。只加一张卡，接到 `/api/incidents`，文本一律 textContent。
    #[test]
    fn the_incidents_card_is_embedded_and_wired_to_the_api() {
        let html = String::from_utf8(web_file("index.html").unwrap()).unwrap();
        for id in ["id=\"sys-inc-card\"", "id=\"sys-inc-body\"", "id=\"inc-refresh\""] {
            assert!(html.contains(id), "index.html 缺 {id}");
        }
        assert!(html.contains("onclick=\"loadIncidents()\""));
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        assert!(js.contains("function loadIncidents()"));
        assert!(js.contains("api(\"/incidents?limit=20\")"));
        assert!(
            js.contains("if (document.getElementById(\"sys-inc-body\")) loadIncidents();"),
            "登录后自动加载一次"
        );
        let body = js.split("function loadIncidents()").nth(1).unwrap();
        let body = body.split("\nfunction ").next().unwrap();
        assert!(!body.contains("innerHTML"), "事件文本来自日志，只许 textContent");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p bui the_incidents_card 2>&1 | tail -5`
Expected: FAIL——`index.html 缺 id="sys-inc-card"`。

- [ ] **Step 3: 加卡片**（`web/index.html`）

在 `<!-- hy2 watchdog -->` 那张卡（`id="sys-wd-card"`）的闭合 `</div>` 之后、`sysstat-grid` 的闭合 `</div>` 之前插入：

```html
                <!-- 日志哨兵事件（spec §5.7） -->
                <div class="table-card sysstat-card" id="sys-inc-card">
                    <div class="table-header">
                        <h2><svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="vertical-align:-3px;margin-right:6px"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>事件</h2>
                        <button class="btn btn-secondary sysstat-refresh" onclick="loadIncidents()" id="inc-refresh">刷新</button>
                    </div>
                    <div class="sysstat-body" id="sys-inc-body">
                        <div class="resi-shimmer-line" style="width:60%;margin-bottom:10px"></div>
                        <div class="resi-shimmer-line" style="width:40%"></div>
                    </div>
                </div>
```

- [ ] **Step 4: 加加载函数**（`web/app.js`）

在 `function loadWatchdogStatus()` 整个函数之后、`// 在 dashboard 初始化后自动加载一次` 之前插入：

```js
// 日志哨兵事件（spec §5.7）：最近 20 条，新的在前。文本全部来自日志与预案，一律 textContent
function loadIncidents() {
    const body = document.getElementById("sys-inc-body");
    const btn  = document.getElementById("inc-refresh");
    if (!body) return;
    _sysShimmer(body);
    if (btn) { btn.disabled = true; btn.textContent = "刷新中…"; }

    api("/incidents?limit=20").then(r => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        if (!r || r.error || !Array.isArray(r.incidents)) {
            _sysErr(body, "读取失败", loadIncidents);
            return;
        }
        _sysClear(body);
        if (!r.incidents.length) {
            const row = document.createElement("div");
            row.className = "sysstat-row";
            const lbl = document.createElement("span");
            lbl.className = "sysstat-label-dim";
            lbl.textContent = "暂无事件";
            row.appendChild(lbl);
            body.appendChild(row);
            return;
        }
        const kinds = { error: "bad", warn: "warn", info: "good" };
        const names = { error: "告警", warn: "警告", info: "信息" };
        r.incidents.forEach(i => {
            const wrap = document.createElement("span");
            const text = document.createElement("span");
            text.textContent = _sysFmt(i.subject) + " · " + _sysFmt(i.result);
            text.title = _sysFmt(i.signature) + " → " + _sysFmt(i.action);
            wrap.append(_sysTag(names[i.level] || _sysFmt(i.level), kinds[i.level] || ""),
                        document.createTextNode(" "), text);
            body.appendChild(_sysKv(_sysFmt(i.at).replace("T", " ").replace("Z", ""), wrap));
        });
        body.appendChild(_sysKv("总数", String(r.total)));
    }).catch(() => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        _sysErr(body, "读取失败", loadIncidents);
    });
}

```

`function initSysStatusOnce()` 里在 `if (document.getElementById("sys-wd-body"))   loadWatchdogStatus();` 之后加一行：

```js
    if (document.getElementById("sys-inc-body")) loadIncidents();
```

- [ ] **Step 5: 跑测试确认通过 + 语法检查**

Run: `cargo test -p bui the_incidents_card 2>&1 | tail -3 && (command -v node >/dev/null && node --check web/app.js && echo js-ok || echo "node 不在，跳过语法检查")`
Expected: PASS；有 node 时打印 `js-ok`。

- [ ] **Step 6: 目视核对（可选，本机）**

用 `run` skill 起 `bui serve` 的测试构建不现实（要 root 与内核），改为：`python3 -m http.server -d web 8099` 后浏览器打开 `http://127.0.0.1:8099/index.html`，在控制台执行
`_sysClear(document.getElementById("sys-inc-body"))` 确认卡片存在且与另两张卡同风格；数据渲染由 Task 10 的真机演练顺带核对（演练后面板的事件卡应出现 `relay_upstream_error` 一条）。

- [ ] **Step 7: 全量门禁**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3 && cargo test --workspace 2>&1 | grep -E "^test result" | head`
Expected: 零告警、全绿。

- [ ] **Step 8: Commit**

```bash
git add web/index.html web/app.js crates/bui/src/modules/panel/assets.rs
git commit -m "$(cat <<'EOF'
feat(sentinel): 面板新增「事件」卡

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 10: 真机演练脚本 `scripts/ops/sentinel-drill.sh`（+ `--self-test`）

**Files:**
- Create: `scripts/ops/sentinel-drill.sh`
- Create: `scripts/tests/test-sentinel-drill.sh`

**Interfaces:**
- Consumes: Task 8 的 `bui incidents --json -n 50`（`{"incidents": [{at, signature, subject, …}], …}`）；既有 `bui residential slots --json`（`{"slots": [{index, upstream_id, upstream_tag, host, port, relay_port, borrowed, pinned, active_upstream_id, active_tag, …}]}`，`residential::api::slot_rows`）；既有 `bui residential status --json`（`mode` = `global` / `split`、`domains` = 生效的分流关键字，`residential::api::StatusResponse`）；`journalctl -u b-ui-relay -o short-unix`；`iptables -w`；`getent ahostsv4`；`curl --socks5-hostname`。
- Produces: 真机判据 ①–④ 的 PASS/FAIL 行 + `N PASS / M FAIL` 摘要，退出码 = FAIL 数；`--self-test` 打 `PASS 自测 …` / `FAIL 自测 …` 行 + `自测：N PASS / M FAIL`。所有外部命令都可经环境变量换成 stub：`BUI` / `IPTABLES` / `CURL` / `JOURNALCTL` / `GETENT`；等待窗口 `DRILL_DETECT_SLA`（15）/ `DRILL_DETECT_WAIT`（90）/ `DRILL_BACK_WAIT`（660）/ `DRILL_POLL`（2）；出口探测 URL `DRILL_EGRESS_URL`（缺省 `https://chatgpt.com/cdn-cgi/trace`，取其中 `ip=` 那一行；返回裸 IP 的 URL 也认）。

判据口径（设计裁决 D6）：④ 的等待上限 660 秒 = `(OK_TO_HEALTHY 2 + SLOT_BACK_ROUNDS 3) × 120 秒 + 60 秒`——恢复后第 4 轮巡检切回，多给一轮对齐巡检相位。① 量的是「首条 relay 连接错误 → 事件的 `at`」，而 `at` 在快探与借用做完后才盖（Task 7），所以这段时长已含借用。

分流模式：经本槽中继入站 `127.0.0.1:(2080+i)` 的请求只有走到本槽 selector 才会拨上游。global 模式全走；split 模式（缺省——`bui-schema` `model.rs` 里 `ResiMode` 的 `#[default]` 是 `Split`）只有域名命中分流关键字的才走（`render::relay` 把 `domain_keyword` 挂在「本槽入站 → 本槽 selector」那条规则上），其余落到 `route.final = direct`。`DEFAULT_KEYWORDS` 里没有 ipify，所以探测 URL 若用 `api.ipify.org`，split 节点上三个造错误的请求根本不会拨上游：relay 不出 `[resi-N]` 的连接错误，哨兵不记事件，`ip_before` 与 `ip_during` 都是 VPS 自己的 IP，判据①–③ 必然 FAIL，演练会误报哨兵失效。脚本因此做两件事。一是缺省探测 URL 改用命中默认关键字 `chatgpt` 的 Cloudflare trace。二是开跑前读 `bui residential status --json`：split 模式下，探测 URL 的主机名只要不含任何一个生效关键字（sing-box 的 `domain_keyword` 是子串匹配），就打 FATAL 退 2，一条 iptables 规则都不插。`--self-test` 覆盖 split 命中、split 不命中、global 三个分支。

- [ ] **Step 1: 写失败测试**

新建 `scripts/tests/test-sentinel-drill.sh`：

```bash
#!/usr/bin/env bash
# sentinel-drill.sh 的 --self-test 必须全绿：四条判据的通过与失败分支、分流模式三个分支、trap 兜底删规则都在那里守着。
# 本测试只跑它并核对摘要、退出码，以及「每条判据的两个分支都有人断言」。全 stub，零网络。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

out=$(bash "$ROOT/scripts/ops/sentinel-drill.sh" --self-test 2>&1)
rc=$?
assert_eq "0" "$rc" "--self-test 退出码 0（退出码 = 自测失败数）"
assert_eq "0" "$(printf '%s\n' "$out" | grep -c '^FAIL 自测')" "自测没有失败项"
assert_contains "自测：" "$out" "打印了自测摘要"
assert_contains "丢包规则已删干净" "$out" "成功路径删规则有断言"
assert_contains "被 TERM 杀掉也删干净" "$out" "trap 兜底有断言"
assert_contains "PASS 自测 分流 split 分支：探测 URL 命中关键字才演练" "$out" "split 命中分支有断言"
assert_contains "PASS 自测 分流 split 分支：不命中关键字 ⇒ FATAL" "$out" "split 不命中分支有断言"
assert_contains "PASS 自测 分流 global 分支" "$out" "global 分支有断言"
for c in 判据① 判据② 判据③ 判据④; do
    p=$(printf '%s\n' "$out" | grep -c "^PASS 自测 $c 通过分支")
    f=$(printf '%s\n' "$out" | grep -c "^PASS 自测 $c 失败分支")
    assert_eq "1" "$([[ "$p" -ge 1 ]] && echo 1 || echo 0)" "$c 的通过分支有断言（实测 $p 条）"
    assert_eq "1" "$([[ "$f" -ge 1 ]] && echo 1 || echo 0)" "$c 的失败分支有断言（实测 $f 条）"
done

out=$(bash "$ROOT/scripts/ops/sentinel-drill.sh" --no-such-flag 2>&1)
rc=$?
assert_eq "2" "$rc" "未知参数退 2"
assert_contains "用法" "$out" "未知参数打印用法"
finish
```

- [ ] **Step 2: 跑测试确认失败**

Run: `bash scripts/tests/test-sentinel-drill.sh; echo rc=$?`
Expected: `not ok`（`sentinel-drill.sh` 不存在），`rc=1`。

- [ ] **Step 3: 写演练脚本**

新建 `scripts/ops/sentinel-drill.sh`：

```bash
#!/usr/bin/env bash
# b-ui v4 日志哨兵真机演练（spec §5.7）。在生产机（先 bwg-rick）上以 root 运行：临时丢弃发往
# **某一个**住宅上游（该上游的 IP:端口，只动 TCP）的流量，验证四条判据：
#   ① 哨兵 ≤ DRILL_DETECT_SLA（15）秒记事件——从该上游第一条 relay 连接错误的日志时刻算到事件的
#     at（哨兵在快探与借用做完后才盖时间戳，所以这段时长已含借用）
#   ② 该槽借用到其它 IP（`bui residential slots --json` 的 borrowed=true、active ≠ 本槽）
#   ③ 该槽用户回环出网 IP 改变（经本槽中继入站 127.0.0.1:(2080+i) 请求 DRILL_EGRESS_URL 取出口 IP）
#   ④ 删掉丢包规则后，巡检在 DRILL_BACK_WAIT（660）秒内切回本槽（恢复后第 4 轮，见计划 D6）
#
#   真机：sudo bash scripts/ops/sentinel-drill.sh [--slot N]      # 缺省槽 1
#   自测：bash scripts/ops/sentinel-drill.sh --self-test          # 不出网、不碰 iptables、不需要 root
#
# 丢包规则都带注释 bui-sentinel-drill；EXIT 的 trap 必定把它们删干净（INT/TERM/HUP 先转成 exit）。
# 分流模式：本槽入站的请求只有走到本槽 selector 才会拨上游。global 全走；split（缺省）只有域名
# 命中分流关键字的才走，其余 route.final = direct——探测 URL 不命中时请求根本不碰上游，判据①–③
# 必然误报失败。所以缺省探测 URL 用命中默认关键字 chatgpt 的 Cloudflare trace（取 ip= 行），
# 开跑前按 `bui residential status --json` 的 mode / domains 自查：split 且不命中 ⇒ FATAL 退 2。
# 演练期间该槽用户会断流约一分钟（直到哨兵借到别的 IP），请在低峰做。
# 退出码 = FAIL 数（0 = 全过）；前置条件不满足打 FATAL 退 2。凭据一律不经本脚本。
set -uo pipefail
LC_ALL=C

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
BASE=${BASE:-/opt/b-ui}
BUI=${BUI:-$BASE/bin/bui}
IPTABLES=${IPTABLES:-iptables}
CURL=${CURL:-curl}
JOURNALCTL=${JOURNALCTL:-journalctl}
GETENT=${GETENT:-getent}
DETECT_SLA=${DRILL_DETECT_SLA:-15}
DETECT_WAIT=${DRILL_DETECT_WAIT:-90}
BACK_WAIT=${DRILL_BACK_WAIT:-660}
POLL=${DRILL_POLL:-2}
EGRESS_URL=${DRILL_EGRESS_URL:-https://chatgpt.com/cdn-cgi/trace}
TAG=bui-sentinel-drill
SLOT=1
SELF_TEST=0
DROP_IPS=""
DROP_PORT=""
PASS=0
FAIL=0

usage() {
  printf '用法：%s [--slot N] [--base /opt/b-ui] | --self-test\n' "$0" >&2
  exit 2
}
pass() { PASS=$((PASS + 1)); printf 'PASS %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'FAIL %s\n' "$1"; }
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$1" >&2; }
fatal() { printf 'FATAL %s\n' "$1" >&2; exit 2; }

# slots JSON 里第 $2 槽的字段 $3（布尔打印成 true/false，缺失打印空）
slot_field() {
  python3 -c 'import json, sys
try:
    rows = (json.loads(sys.argv[1]) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    if r.get("index") == int(sys.argv[2]):
        v = r.get(sys.argv[3])
        print("" if v is None else (str(v).lower() if isinstance(v, bool) else v))
        break' "$1" "$2" "$3"
}

slot_count() {
  python3 -c 'import json, sys
try:
    print(len((json.loads(sys.argv[1]) or {}).get("slots") or []))
except Exception:
    print(0)' "$1"
}

# 经本槽中继入站取出口 IP（$1 = relay 端口）；取不到打印空。
# Cloudflare trace 取 ip= 那一行；返回裸 IP 的 URL（DRILL_EGRESS_URL 覆盖时）整行就是 IP
egress_ip() {
  "$CURL" -sS --max-time 20 --socks5-hostname "127.0.0.1:$1" "$EGRESS_URL" 2>/dev/null |
    awk -F= '$1 == "ip" { print $2; exit } NF == 1 && /^[0-9A-Fa-f:.]+$/ { print; exit }' | tr -d '[:space:]'
}

# 探测 URL 经本槽入站时会不会走本槽 selector（$1 = status JSON，$2 = URL）：global ⇒ 打印
# global；split ⇒ 主机名含某个生效关键字（sing-box domain_keyword 是子串匹配）就打印
# split:<关键字>，一个都不含打印空
egress_route() {
  python3 -c 'import json, sys, urllib.parse
try:
    st = json.loads(sys.argv[1]) or {}
except Exception:
    st = {}
host = (urllib.parse.urlsplit(sys.argv[2]).hostname or "").lower()
if st.get("mode") == "global":
    print("global")
else:
    hit = next((k for k in st.get("domains") or [] if k and k.lower() in host), None)
    print("" if hit is None else "split:" + hit)' "$1" "$2"
}

# 主机名 → IPv4（可能多个）；本身就是 IPv4 就原样返回
resolve_v4() {
  if [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf '%s\n' "$1"
    return
  fi
  "$GETENT" ahostsv4 "$1" | awk '{print $1}' | sort -u
}

drop_on() {
  local ip
  for ip in $DROP_IPS; do
    "$IPTABLES" -w -I OUTPUT -p tcp -d "$ip" --dport "$DROP_PORT" -m comment --comment "$TAG" -j DROP || return 1
  done
}

# 删到删不动为止（同一条插了几次就删几次）；没插过时是 no-op
drop_off() {
  local ip
  [[ -n "$DROP_PORT" ]] || return 0
  for ip in $DROP_IPS; do
    while "$IPTABLES" -w -D OUTPUT -p tcp -d "$ip" --dport "$DROP_PORT" -m comment --comment "$TAG" -j DROP 2>/dev/null; do :; done
  done
}

# relay 日志里 $2（epoch 秒）之后成员 $1 的第一条连接错误的时刻（short-unix 的第一列）
first_error_epoch() {
  "$JOURNALCTL" -u b-ui-relay --since "@$2" -o short-unix --no-pager 2>/dev/null |
    sed 's/\x1b\[[0-9;]*m//g' | grep -F "[$1]: " | grep -F 'open connection to' | head -n1 | awk '{print $1}'
}

# 对象 $1（host:port）在 $2（epoch 秒）之后最早一条 relay_upstream_error 事件的时刻（epoch 秒）
incident_epoch() {
  "$BUI" incidents --json -n 50 2>/dev/null | python3 -c 'import datetime, json, sys
subj, since = sys.argv[1], float(sys.argv[2])
try:
    rows = (json.load(sys.stdin) or {}).get("incidents") or []
except Exception:
    rows = []
best = None
for i in rows:
    if i.get("signature") != "relay_upstream_error" or i.get("subject") != subj:
        continue
    t = datetime.datetime.strptime(i["at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    if t >= since and (best is None or t < best):
        best = t
print("" if best is None else int(best))' "$1" "$2"
}

run_drill() {
  local slots status route n own_id tag host port relay ip_before ip_during t0 inc err delta borrowed active waited
  slots=$("$BUI" residential slots --json 2>/dev/null) || fatal "bui residential slots --json 失败（守护进程在跑吗？）"
  n=$(slot_count "$slots")
  [[ "$n" -ge 2 ]] || fatal "至少要 2 个槽才有别的 IP 可借（实有 $n）"
  own_id=$(slot_field "$slots" "$SLOT" upstream_id)
  [[ -n "$own_id" ]] || fatal "槽 $SLOT 不存在"
  [[ "$(slot_field "$slots" "$SLOT" pinned)" != "true" ]] || fatal "槽 $SLOT 被手动 pin，哨兵按设计不动它；先 bui residential slot-pin $SLOT --auto"
  [[ "$(slot_field "$slots" "$SLOT" borrowed)" != "true" ]] || fatal "槽 $SLOT 此刻正在借用，等它切回再演练"
  status=$("$BUI" residential status --json 2>/dev/null) || fatal "bui residential status --json 失败"
  route=$(egress_route "$status" "$EGRESS_URL")
  [[ -n "$route" ]] || fatal "分流模式是 split，探测 URL $EGRESS_URL 的主机不命中任何分流关键字：经本槽入站的请求会走 direct、根本不拨上游，判据①–③ 必然误报失败。换一个命中关键字的 DRILL_EGRESS_URL，或临时 bui residential global on"
  tag=$(slot_field "$slots" "$SLOT" upstream_tag)
  host=$(slot_field "$slots" "$SLOT" host)
  port=$(slot_field "$slots" "$SLOT" port)
  relay=$(slot_field "$slots" "$SLOT" relay_port)
  DROP_IPS=$(resolve_v4 "$host" | tr '\n' ' ')
  [[ -n "${DROP_IPS// /}" ]] || fatal "解析不出 $host 的 IPv4"
  DROP_PORT=$port
  ip_before=$(egress_ip "$relay")
  [[ -n "$ip_before" ]] || fatal "演练前经 127.0.0.1:$relay 取不到出口 IP"
  log "槽 $SLOT（$tag = $host:$port → ${DROP_IPS% }），分流 $route，演练前出口 $ip_before"

  t0=$(date +%s)
  drop_on || fatal "iptables 插规则失败"
  log "已丢弃发往 ${DROP_IPS% } 端口 $port 的 TCP（注释 $TAG）"
  # 造连接错误：三个并发请求经本槽出网，都会卡在 relay 连上游这一步（门槛 60 秒 ≥3 条）
  for _ in 1 2 3; do
    egress_ip "$relay" >/dev/null &
  done

  # ① 事件与时延
  inc=""
  waited=0
  while ((waited < DETECT_WAIT)); do
    inc=$(incident_epoch "$host:$port" "$t0")
    [[ -n "$inc" ]] && break
    sleep "$POLL"
    waited=$((waited + POLL))
  done
  err=$(first_error_epoch "$tag" "$t0")
  if [[ -z "$inc" ]]; then
    fail "判据① 哨兵事件：${DETECT_WAIT}s 内没有 $host:$port 的 relay_upstream_error"
  elif [[ -z "$err" ]]; then
    fail "判据① 哨兵事件：有事件，但 relay 日志里找不到 [$tag] 的连接错误，无法计时"
  else
    delta=$(python3 -c 'import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 1))' "$inc" "$err")
    if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= float(sys.argv[2]) else 1)' "$delta" "$DETECT_SLA"; then
      pass "判据① 哨兵事件：首条错误后 ${delta}s 记事件（≤${DETECT_SLA}s）"
    else
      fail "判据① 哨兵事件：首条错误后 ${delta}s 才记事件（>${DETECT_SLA}s）"
    fi
  fi

  # ② 该槽借用
  slots=$("$BUI" residential slots --json 2>/dev/null)
  borrowed=$(slot_field "$slots" "$SLOT" borrowed)
  active=$(slot_field "$slots" "$SLOT" active_upstream_id)
  if [[ "$borrowed" == "true" && -n "$active" && "$active" != "$own_id" ]]; then
    pass "判据② 槽 $SLOT 已借用 $(slot_field "$slots" "$SLOT" active_tag)"
  else
    fail "判据② 槽 $SLOT 没有借用（borrowed=${borrowed:-?} active=${active:-?}）"
  fi

  # ③ 回环出网 IP 改变
  ip_during=$(egress_ip "$relay")
  if [[ -n "$ip_during" && "$ip_during" != "$ip_before" ]]; then
    pass "判据③ 回环出网 IP $ip_before → $ip_during"
  else
    fail "判据③ 回环出网 IP 没变（前 $ip_before，后 ${ip_during:-取不到}）"
  fi

  # 恢复
  drop_off
  wait
  log "已删除丢包规则，等巡检切回（最多 ${BACK_WAIT}s）"

  # ④ 切回
  waited=0
  while ((waited < BACK_WAIT)); do
    slots=$("$BUI" residential slots --json 2>/dev/null)
    if [[ "$(slot_field "$slots" "$SLOT" borrowed)" == "false" &&
      "$(slot_field "$slots" "$SLOT" active_upstream_id)" == "$own_id" ]]; then
      break
    fi
    sleep "$POLL"
    waited=$((waited + POLL))
  done
  if ((waited < BACK_WAIT)); then
    pass "判据④ 恢复后 ${waited}s 切回本槽 IP"
  else
    fail "判据④ ${BACK_WAIT}s 内没有切回本槽"
  fi
  printf '%d PASS / %d FAIL\n' "$PASS" "$FAIL"
  return "$FAIL"
}

# ── 自测：五个 stub 模拟 bui / iptables / curl / journalctl / getent ─────────────
write_stubs() {
  local d="$1"
  cat >"$d/iptables" <<'STUB'
#!/usr/bin/env bash
# 记账：-I 追加一行，-D 删掉第一条相同的（没有就退 1，与真 iptables 一致）
f="$ST_DIR/rules"
touch "$f"
args="$*"
key="${args/-I OUTPUT/}"
key="${key/-D OUTPUT/}"
case " $* " in
  *" -I "*) printf '%s\n' "$key" >>"$f" ;;
  *" -D "*)
    grep -qxF -- "$key" "$f" || exit 1
    awk -v k="$key" '!d && $0 == k { d = 1; next } { print }' "$f" >"$f.tmp" && mv "$f.tmp" "$f"
    ;;
esac
STUB
  cat >"$d/curl" <<'STUB'
#!/usr/bin/env bash
# Cloudflare trace 的形状。借用后出口 .9；丢包期间没借用 ⇒ 记一条 relay 连接错误并超时；平时出口 .8
trace() { printf 'fl=1f1\nh=chatgpt.com\nip=%s\nts=1\n' "$1"; }
if [[ -f "$ST_DIR/borrowed" ]]; then trace 198.51.100.9; exit 0; fi
if [[ -s "$ST_DIR/rules" ]]; then
  printf '%s.000000 node-a sing-box[1]: ERROR[4006] [1 5.00s] connection: open connection to chatgpt.com:443 using outbound/socks[resi-2]: dial tcp 198.51.100.200:10007: i/o timeout\n' "$(date +%s)" >>"$ST_DIR/journal"
  exit 28
fi
trace 198.51.100.8
STUB
  cat >"$d/journalctl" <<'STUB'
#!/usr/bin/env bash
cat "$ST_DIR/journal" 2>/dev/null
STUB
  cat >"$d/getent" <<'STUB'
#!/usr/bin/env bash
printf '198.51.100.200  STREAM isp2.example.net\n198.51.100.200  DGRAM\n'
STUB
  cat >"$d/bui" <<'STUB'
#!/usr/bin/env bash
# 模拟守护进程：丢包 + 有连接错误 ⇒ 记事件（首条错误后 FAKE_DELAY 秒）并借用；
# 规则删掉后第 3 次查询切回（FAKE_NO_BACK=1 永不切回；FAKE_NO_INCIDENT=1 永不记事件）；
# residential status 按 FAKE_MODE（缺省 split）/ FAKE_DOMAINS（缺省 ["openai","chatgpt"]）作答
S="$ST_DIR"
rules_on() { [[ -s "$S/rules" ]]; }
case "$1 $2" in
  "residential status")
    dflt='["openai","chatgpt"]'
    printf '{"mode":"%s","domains":%s}\n' "${FAKE_MODE:-split}" "${FAKE_DOMAINS:-$dflt}"
    ;;
  "residential slots")
    if [[ -f "$S/borrowed" ]] && ! rules_on && [[ "${FAKE_NO_BACK:-0}" != 1 ]]; then
      n=$(($(cat "$S/back" 2>/dev/null || echo 0) + 1))
      echo "$n" >"$S/back"
      [[ "$n" -ge 3 ]] && rm -f "$S/borrowed"
    fi
    if [[ -f "$S/borrowed" ]]; then b=true; a=3; else b=false; a=2; fi
    printf '{"slots":[{"index":0,"upstream_id":"00000000-0000-0000-0000-000000000001","upstream_tag":"resi-1","host":"isp1.example.net","port":10007,"relay_port":2080,"borrowed":false,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-000000000001","active_tag":"resi-1"},{"index":1,"upstream_id":"00000000-0000-0000-0000-000000000002","upstream_tag":"resi-2","host":"isp2.example.net","port":10007,"relay_port":2081,"borrowed":%s,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-00000000000%s","active_tag":"resi-%s"}]}\n' "$b" "$a" "$a"
    ;;
  "incidents --json")
    if rules_on && [[ -s "$S/journal" && ! -f "$S/incident" && "${FAKE_NO_INCIDENT:-0}" != 1 ]]; then
      e=$(head -n1 "$S/journal" | awk '{printf "%d", $1}')
      date -u -d "@$((e + ${FAKE_DELAY:-3}))" +%FT%TZ >"$S/incident"
      touch "$S/borrowed"
    fi
    if [[ -f "$S/incident" ]]; then
      printf '{"incidents":[{"at":"%s","unit":"b-ui-relay","signature":"relay_upstream_error","subject":"isp2.example.net:10007","action":"probe_and_borrow","result":"stub","level":"error"}],"source":"daemon"}\n' "$(cat "$S/incident")"
    else
      printf '{"incidents":[],"source":"daemon"}\n'
    fi
    ;;
  *) echo "stub bui: $*" >&2; exit 1 ;;
esac
STUB
  chmod +x "$d"/*
}

self_test() {
  ST_DIR=$(mktemp -d) || exit 2
  export ST_DIR
  trap 'rm -rf "$ST_DIR"' EXIT
  mkdir -p "$ST_DIR/bin"
  write_stubs "$ST_DIR/bin"
  local out rc pid i st_pass=0 st_fail=0
  check() {
    if [[ "$2" == *"$1"* ]]; then
      st_pass=$((st_pass + 1))
      printf 'PASS 自测 %s\n' "$3"
    else
      st_fail=$((st_fail + 1))
      printf 'FAIL 自测 %s\n    缺：%s\n' "$3" "$1"
    fi
  }
  scenario() {
    rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" "$ST_DIR/back"
    env "$@" BUI="$ST_DIR/bin/bui" IPTABLES="$ST_DIR/bin/iptables" CURL="$ST_DIR/bin/curl" \
      JOURNALCTL="$ST_DIR/bin/journalctl" GETENT="$ST_DIR/bin/getent" DRILL_ALLOW_NONROOT=1 \
      DRILL_DETECT_WAIT="${WAIT_DETECT:-3}" DRILL_BACK_WAIT=6 DRILL_POLL=1 \
      bash "$SELF" --slot 1 2>&1
  }

  out=$(scenario)
  rc=$?
  check "PASS 判据① 哨兵事件：首条错误后 3.0s" "$out" "判据① 通过分支：3 秒记事件"
  check "PASS 判据② 槽 1 已借用 resi-3" "$out" "判据② 通过分支：借用"
  check "PASS 判据③ 回环出网 IP 198.51.100.8 → 198.51.100.9" "$out" "判据③ 通过分支：出口改变"
  check "PASS 判据④ 恢复后" "$out" "判据④ 通过分支：切回"
  check "4 PASS / 0 FAIL" "$out" "成功路径摘要"
  check "rc=0" "rc=$rc" "成功路径退出码 0"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "成功路径：丢包规则已删干净"
  check "分流 split:chatgpt" "$out" "分流 split 分支：探测 URL 命中关键字才演练"

  out=$(scenario FAKE_DOMAINS='["openai"]')
  rc=$?
  check "FATAL 分流模式是 split" "$out" "分流 split 分支：不命中关键字 ⇒ FATAL"
  check "rc=2" "rc=$rc" "分流 split 不命中：退出码 2"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "分流 split 不命中：一条丢包规则都没插"

  out=$(scenario FAKE_MODE=global FAKE_DOMAINS='[]')
  check "分流 global" "$out" "分流 global 分支：不看关键字"
  check "4 PASS / 0 FAIL" "$out" "分流 global 分支：照常全过"

  out=$(scenario FAKE_DELAY=30)
  check "FAIL 判据① 哨兵事件：首条错误后 30.0s 才记事件" "$out" "判据① 失败分支：超过 15 秒"

  out=$(scenario FAKE_NO_INCIDENT=1)
  rc=$?
  check "FAIL 判据① 哨兵事件：3s 内没有 isp2.example.net:10007" "$out" "判据① 失败分支：没有事件"
  check "FAIL 判据② 槽 1 没有借用" "$out" "判据② 失败分支：没借用"
  check "FAIL 判据③ 回环出网 IP 没变" "$out" "判据③ 失败分支：出口没变"
  check "rc=3" "rc=$rc" "没事件时退出码 = FAIL 数"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "失败路径：丢包规则同样删干净"

  out=$(scenario FAKE_NO_BACK=1)
  check "FAIL 判据④ 6s 内没有切回本槽" "$out" "判据④ 失败分支：不切回"

  # trap 兜底：演练卡在等事件时被 TERM 杀掉，规则也必须删干净
  rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" "$ST_DIR/back"
  env FAKE_NO_INCIDENT=1 BUI="$ST_DIR/bin/bui" IPTABLES="$ST_DIR/bin/iptables" CURL="$ST_DIR/bin/curl" \
    JOURNALCTL="$ST_DIR/bin/journalctl" GETENT="$ST_DIR/bin/getent" DRILL_ALLOW_NONROOT=1 \
    DRILL_DETECT_WAIT=60 DRILL_POLL=1 bash "$SELF" --slot 1 >/dev/null 2>&1 &
  pid=$!
  for i in $(seq 1 50); do
    [[ -s "$ST_DIR/rules" ]] && break
    sleep 0.1
  done
  kill -TERM "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "被 TERM 杀掉也删干净（等了 ${i} 个 0.1s 才见到规则）"

  printf '自测：%d PASS / %d FAIL\n' "$st_pass" "$st_fail"
  exit "$st_fail"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --slot) SLOT="${2:-}"; shift 2 ;;
    --base) BASE="${2:-}"; BUI="$BASE/bin/bui"; shift 2 ;;
    --self-test) SELF_TEST=1; shift ;;
    *) usage ;;
  esac
done
[[ "$SLOT" =~ ^[0-9]+$ ]] || usage

if [[ "$SELF_TEST" -eq 1 ]]; then
  self_test
fi

command -v python3 >/dev/null || fatal "需要 python3"
if [[ "${DRILL_ALLOW_NONROOT:-0}" != 1 ]]; then
  [[ "$EUID" -eq 0 ]] || fatal "需要 root（要动 iptables）"
  command -v "$IPTABLES" >/dev/null || fatal "找不到 $IPTABLES"
fi
trap 'drop_off' EXIT
trap 'exit 130' INT TERM HUP
run_drill
exit $?
```

- [ ] **Step 4: 跑测试确认通过**

Run: `bash scripts/tests/test-sentinel-drill.sh`
Expected: 全部 `ok`，末行 `1..N` 且无 `not ok`。

- [ ] **Step 5: shell 门禁**

Run: `bash -n scripts/ops/sentinel-drill.sh scripts/tests/test-sentinel-drill.sh && shellcheck -S error scripts/ops/sentinel-drill.sh scripts/tests/test-sentinel-drill.sh && bash scripts/tests/run-all.sh | tail -2`
Expected: 无输出错误；`# all script tests passed`。

- [ ] **Step 6: 真机演练（Fable 验收时执行，不在 CI）**

**前置（缺一不跑）**：
1. 切回轮数已裁决（文末「切回口径裁决」）：维持 D6，判据④ 按 `DRILL_BACK_WAIT` 缺省 660 秒判。
2. 先看 `bui residential status --json` 的 `mode`。split（缺省）时生效关键字必须含 `chatgpt`（缺省探测 URL 的主机），否则脚本会 FATAL；关键字被改过，就用 `DRILL_EGRESS_URL` 指一个命中关键字、且返回 Cloudflare trace 或裸 IP 的 URL。

在 bwg-rick 以 root：`nohup bash /opt/b-ui/ops/sentinel-drill.sh --slot 1 >/var/log/bui-sentinel-drill.log 2>&1 & echo $! > /var/log/bui-sentinel-drill.pid`（脚本随 Task 11 之后的发版一同进 `/opt/b-ui/ops/`；在那之前用 `scp` 拷过去跑），用 `Monitor` 等 `/var/log/bui-sentinel-drill.log` 出现 `PASS / ` 摘要行。通过标准：`4 PASS / 0 FAIL`；另外人工核对面板「事件」卡与 `bui status` 末尾都出现这条 `relay_upstream_error`，且 `iptables -S OUTPUT | grep -c bui-sentinel-drill` 为 0。

- [ ] **Step 7: Commit**

```bash
git add scripts/ops/sentinel-drill.sh scripts/tests/test-sentinel-drill.sh
git commit -m "$(cat <<'EOF'
test(sentinel): 真机演练脚本（iptables 断一条上游，trap 兜底恢复）与自测

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

---

## Task 11: 文档收口（CLAUDE.md、CHANGELOG、总纲 C2 的 Host 清单）

spec §5.7、§3.2 末尾「哨兵」那一句与总纲裁决记录已随本计划的 commit 按设计裁决 D1–D16 修订；本任务在实现合并后把**实现侧**的三处文档跟上，并核对 spec §5.7 里的常量与实现一致。

**Files:**
- Modify: `CLAUDE.md`
- Modify: `CHANGELOG.md`
- Modify: `docs/superpowers/plans/2026-09-11-v4-master.md`（C2 的 `Host` trait 清单）
- Modify（仅当 Step 4 发现不一致）: `docs/superpowers/specs/2026-09-11-v4-architecture-design.md` §5.7

**Interfaces:**
- Consumes: Task 1–10 的全部实现（常量名与值以代码为准）。
- Produces: 文档与代码一致；无代码变更。

- [ ] **Step 1: CLAUDE.md**

`## Architecture` 第 2 条里，子命令清单 `` `install / upgrade / serve / reconcile / status / import-v3 / set / auth-hook / residential / menu / harden-ssh` `` 改为：

```
`install / upgrade / serve / reconcile / status / incidents / import-v3 / set / auth-hook / residential / menu / harden-ssh`
```

同一条里 `面板 API + 内嵌前端 + 住宅巡检 + 黑名单 + 证书监听 + watchdog + 升级都在这一个进程里` 改为：

```
面板 API + 内嵌前端 + 住宅巡检 + 黑名单 + 证书监听 + watchdog + 日志哨兵（spec §5.7，`modules/sentinel`：5 秒增量读受管单元的 journald，按签名表探测 → 按槽借用 / 重试用户同步 / 告警，事件落 `runtime.json` 的 `incidents`，`bui incidents` 查看）+ 升级都在这一个进程里
```

- [ ] **Step 2: CHANGELOG.md**

`## [4.0.0] - 未发布` 的 `### 新增` 末尾追加一条：

```markdown
- 日志哨兵（spec §5.7）：守护进程每 5 秒增量读 relay / hysteria / xray / caddy / 自身的日志，住宅上游连不上（60 秒内 ≥3 条连接错误）时立即带外探测，确认不可用就让压在它上面的槽**当场**借用别的健康 IP（不等 2 分钟巡检；恢复后仍由巡检连续 3 轮切回），并告警「IP X 不可达，槽 i 已临时切到 Y」；上游封 Google、hysteria 鉴权端口连不上、端口冲突 / 崩溃循环、xray gRPC 不可用、证书签不下来各有对应处置或告警。每次触发记一条事件（保留最近 200 条）：`bui status` 末尾显示最近 5 条，`bui incidents [--json] [-n N]` 查询，面板新增「事件」卡。不可达持续 30 分钟会建议替换该 IP；哨兵从不增删池成员。真机演练脚本 `scripts/ops/sentinel-drill.sh`。
```

- [ ] **Step 3: 总纲 C2 的 Host 清单**

`docs/superpowers/plans/2026-09-11-v4-master.md` 的 C2 代码块里，`pub trait Host` 的 `fn now(&self) -> time::OffsetDateTime;` 之后加一行：

```rust
    fn journal_read(&self, units: &[String], from: &JournalFrom) -> Result<Vec<JournalRecord>>;   // 日志哨兵（spec §5.7）：`journalctl -o json` 增量读；JournalFrom::{Cursor(String), Since(OffsetDateTime)}
```

- [ ] **Step 4: 核对 spec §5.7 与实现一致**

Run:

```bash
grep -n "POLL_SECS\|CURSOR_PERSIST_SECS\|PROBE_TIMEOUT_SECS\|DEBOUNCE_SECS\|ACTION_COOLDOWN_SECS\|INCIDENTS_MAX\|LONG_UNREACHABLE_MINS" crates/bui/src/modules/sentinel/mod.rs
grep -n "threshold: \|window_secs: " crates/bui/src/modules/sentinel/signature.rs
grep -n "STATUS_INCIDENTS\|DEFAULT_LIMIT\|default_value_t = 20" crates/bui/src/commands/status.rs crates/bui/src/modules/sentinel/api.rs crates/bui/src/cli.rs
```

Expected：5 / 60 / 5 / 60 / 600 / 200 / 30；门槛 3/60、3/60、2/150、其余 1/60；status 5、API 缺省 50、CLI 缺省 20——与 spec §5.7 的文字逐项一致。有任何一项不一致，以代码为准改 spec §5.7 对应那句。

再核对 §3.2 末尾的「哨兵」一句。本计划的 commit 已把它改为指向 `modules::sentinel`，Task 7 删掉 watchdog 分支之后仍须成立：

```bash
grep -n '^- \*\*哨兵\*\*' docs/superpowers/specs/2026-09-11-v4-architecture-design.md
grep -n 'AUTH_HTTP_KEY\|AuthHttpAlert\|should_alert' crates/bui/src/modules/watchdog.rs
```

Expected：第一条那一行含 `modules::sentinel`，不再说「watchdog 每轮读两个 hysteria 单元的 journal」；第二条无输出。

- [ ] **Step 5: 验证**

Run: `bash scripts/tests/run-all.sh | tail -2 && bash scripts/release/check-version.sh v4.0.0; echo rc=$?`
Expected：`# all script tests passed`；`check-version` 与改动前的结果相同（本任务不改版本号）。

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md CHANGELOG.md docs/superpowers/plans/2026-09-11-v4-master.md \
        docs/superpowers/specs/2026-09-11-v4-architecture-design.md
git commit -m "$(cat <<'EOF'
docs(sentinel): CLAUDE.md、CHANGELOG 与总纲 C2 跟上日志哨兵

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013DzumWvZofwAtRbiq15cEv
EOF
)"
```

（spec 若 Step 4 无改动，`git add` 它是 no-op。）

---

## 执行顺序与并行度

```
T1（sys：journal_read）──────────────────────────────┐
T2（签名表）── T3（去抖/冷却 + 事件环）──┐            │
T4（residential：快探/判不健康/借用）────┴── T5（住宅预案）── T6（系统预案）── T7（主循环 + 注册 + watchdog 迁移）── T8（API / CLI / status）──┬── T9（面板卡）
                                                                                                                                              └── T10（演练脚本）── T11（文档收口）
```

- **第一批并行：T1 ∥ T2 ∥ T4**。三者的文件互不相交：T1 = `util.rs` / `sys/{mod,real,fake}.rs` / `residential/journal.rs`；T2 = `redact.rs` / `watchdog.rs` / `panel/users.rs` / `modules/mod.rs` / 新建 `sentinel/{mod,signature}.rs`；T4 = `residential/{health,state,slots}.rs`。各自在 `v4-sentinel-t1` / `-t2` / `-t4` worktree 里做。
- **T3 在 T2 之后**：它往 T2 建的 `sentinel/mod.rs` 里加 `pub mod` 与常量。
- **T5 需要 T3 + T4**（`SentinelRuntime`、`Level`，`probe_quick` / `mark_unhealthy` / `borrow_now`）；**T6 在 T5 之后**（复用 T5 建的 `testkit.rs`）。
- **T7 需要 T1 + T6**（`journal_read` 与全部预案）。T7 与 T2 都动 `watchdog.rs`，T2 先合、T7 在其上改。
- **T8 在 T7 之后**（`SentinelModule` 的 `routes`、`incidents` 的格式化与取数）。
- **第二批并行：T9 ∥ T10**，都只依赖 T8：T9 = `web/*` + `panel/assets.rs`，T10 = `scripts/*`，互不相交。
- **T11 最后**：文档以合并后的代码为准。

**合并顺序（硬约束）**：T1 → T2 → T4 → T3 → T5 → T6 → T7 → T8 → T9 → T10 → T11。第一批三者谁先做完都按这个顺序合（文件不相交，rebase 无冲突）；`sentinel/mod.rs` 被 T2/T3/T5/T6/T7/T8 依次追加，严格串行合并即不会冲突。每次合并后在 `v4` 上跑一遍 `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && bash scripts/tests/run-all.sh`，全绿才合下一个。

**真机验收（T10 合并后，Fable 在 bwg-rick 执行）**：
1. 升级到含本计划的构建后 `journalctl -u b-ui -n 50` 无「哨兵读 journald 失败」；`bui incidents` 输出「（暂无事件）」或只有真实事件；`jq '.sentinel.since' /opt/b-ui/runtime.json` 是升级时刻（首次不回放历史）。
2. （前置同 Task 10 Step 6：切回口径已裁决；split 模式下探测 URL 命中分流关键字）`sudo bash scripts/ops/sentinel-drill.sh --slot 1` → `4 PASS / 0 FAIL`；面板「事件」卡与 `bui status` 末尾出现这条 `relay_upstream_error`；`iptables -S OUTPUT | grep -c bui-sentinel-drill` 为 0。
3. 连跑两轮巡检（4 分钟）确认 `bui residential slots` 各槽 `borrowed=false`，`systemctl show xray b-ui-relay -p NRestarts` 与演练前相同（借用只经 Clash API，不重启内核）。
4. 通过后在总纲裁决记录补一行「日志哨兵真机验收通过」，再进 bwg-tizi。

---

## 自审记录（writing-plans Self-Review）

**1. 需求覆盖**（主理人口径 (1)–(7) 与 spec §5.7 逐条 → 任务）：

| 要求 | 落点 |
|---|---|
| (1) 5 秒一轮、`--after-cursor`、cursor 存 runtime 重启续读、首次从现在不回放、`-o json`、按 `managed_units` 枚举 relay / hysteria* / xray / caddy、解析成 (unit, ts, message) 交纯函数；Host 原语 + FakeHost 可编程 | T1（原语、解析、FakeHost 队列）、T7（`tick` 的游标 / since / 节流落盘、`units()`） |
| (2) relay 连接错误 60 秒 ≥3 条 ⇒ 按 tag 反查上游 ⇒ 带外探测（复用 health 的探测函数）⇒ 失败则按 §5.6 借用（复用借用语义、防抖 / 手动锁定）+ 告警「IP X 不可达，槽 i 已临时切到 Y」 | T2（签名）、T4（`probe_quick` 复用 `probe_reachable`；`borrow_now` 复用 `rank_healthy` / pin 语义 / `put_slot`）、T5（`on_upstream_error`）、T7（tag → uuid、派发） |
| (2) `403 … serp domain` / sorry 页 ⇒ google_ok=false + 借用 | T2、T5（`on_google_blocked`，sorry 页经 `google_ok_of` 带外复核认出） |
| (2) hysteria auth http 失败 ≥3 条 ⇒ 事件 + 告警，不自杀 | T2、T6（`on_hy2_auth`）、T7（command 模式忽略；watchdog 旧分支迁入） |
| (2) bind 冲突 / 崩溃循环 ⇒ 交给 watchdog，只记事件 | T2、T6（`on_kernel`） |
| (2) xray gRPC Unavailable 连续 ⇒ 立即触发用户同步安全网 | T2（信号取自 b-ui 自身日志，`USER_SYNC_FAILED_LOG`）、T6（`on_xray_grpc` → `users::sync_now`） |
| (2) caddy 签证书失败 ⇒ 告警不动作 | T2、T6（`on_caddy_cert`） |
| (3) 同签名同对象 60 秒一次；同动作 10 分钟冷却；切回归巡检（连续 3 轮），哨兵不切回 | T3（`Engine` / `in_cooldown`）、T4（`borrow_now` 不推进 `back_rounds`）、D6 |
| (4) `runtime.incidents` 环形 200 条（时间、单元、签名、对象、动作、结果）；`bui status` 最近 5 条；`bui incidents [--json] [-n N]`；`GET /api/incidents`（管理员鉴权）；app.js「事件」卡只加不重构 | T3（环）、T8（API / CLI / status）、T9（卡片） |
| (5) 不增删池成员；30 分钟不可达建议替换；替换后 §5.6 重分配；外部通知只留接口 | T5（`sweep_long_unreachable`）、T7（`Notifier` / `NoopNotifier`）、D15 |
| (6) scripts/ops 真机演练：iptables 只针对该上游 IP:端口、trap 兜底恢复；≤15 秒记事件、该槽借用、回环出网 IP 改变、恢复后切回；`--self-test` 用 stub | T10（「恢复后 3 轮内」按 D6 落成「恢复后第 4 轮巡检，脚本上限 660 秒」，见下方切回口径裁决） |
| (7) spec §5.7 按实现修订、总纲裁决记录加一行；Global Constraints 抄 spec §0 与 C1–C5；凭据不进 argv / 日志、测试不碰真实系统、上游 URL 脱敏 | 本计划的 commit（spec §5.7 + 总纲一行）、T11（CLAUDE.md / CHANGELOG / C2）、Global Constraints、T2 的 `redact::line` 与 `detail_of`（先脱敏后截断） |

**2. 占位符扫描**：全文无 TBD / TODO / 「类似 Task N」；每个代码步骤都给了完整代码；唯一「按需」的是 T11 Step 4（spec 与代码核对，给了具体 grep 与期望值）。

**3. 类型与命名一致性**（跨任务核对过的名字）：
- `JournalFrom::{Cursor, Since}` / `JournalRecord { cursor, unit, ts, message }`：T1 定义，T7 使用，testkit `rec()` 构造一致。
- `Sig` 七个变体与 `id()` 串：T2 定义；T7 的 `dispatch` 覆盖全部七个；T10 脚本认 `relay_upstream_error`；T8/T9 只透传字符串。
- `Action::id()`：`probe_and_borrow` / `verify_google_and_borrow` / `alert` / `delegate_watchdog` / `retry_user_sync`（T2），T7 的冷却键 `engine::action_key` 用它；巡查事件的 `SUGGEST_REPLACE_ACTION = "suggest_replace"`、`LONG_UNREACHABLE_SIG = "upstream_long_unreachable"` 是 T7 `run.rs` 的私有常量（首个使用者是 T7 的 `tick`；T5 的 `sweep_long_unreachable` 只产 `Outcome`）。
- `Incident { at, unit, signature, subject, action, result, level, sample }` / `Level::{Info, Warn, Error}`（T3），`Outcome { subject, result, level }`（T5 加进 `incidents.rs`），T6/T7 一致；`Level::label()` 在 T8 加（T3 未用到，避免 dead_code）。
- `SentinelRuntime { cursor, since, acted, down_since, suggested }`（T3），T5 用 `down_since` / `suggested`，T7 用其余。
- `borrow_now(&DaemonCtx, Arc<dyn Clash>, Uuid, OffsetDateTime) -> Vec<SlotOutcome>`（T4），T5 两处调用参数顺序一致。
- `Deps { prober, clash, panel, notifier }`（T7），测试 `Kit` 与 `SentinelModule::new` 一致。
- `IncidentsResponse { incidents, total }`（T8），`load_recent` 反序列化它；前端读 `r.incidents` / `r.total`（T9）；演练脚本读 `incidents[].{signature, subject, at}`（T10）。
- 常量：`DEBOUNCE_SECS` / `ACTION_COOLDOWN_SECS` / `INCIDENTS_MAX`（T3）、`LONG_UNREACHABLE_MINS`（T5）、`POLL_SECS` / `CURSOR_PERSIST_SECS` / `PROBE_TIMEOUT_SECS`（T7）、`LONG_UNREACHABLE_SIG` / `SUGGEST_REPLACE_ACTION`（T7）——每个都在首个使用它的任务里定义。

**切回口径裁决**（原「待主理人确认的一处口径」；2026-09-13 主会话已裁决，总纲裁决记录已写）：主理人原话「恢复后 3 轮内切回」，裁决为**保持现有切回语义**——带外确认不可达的 IP 被立即判不健康（D6），恢复后巡检先连续 `OK_TO_HEALTHY = 2` 轮探通才重新算健康（第 2 轮同时计切回第 1 轮），再攒满 `SLOT_BACK_ROUNDS = 3` 轮切回，即**恢复后第 4 轮巡检（约 8 分钟）切回**；「3 轮内」指的是 `SLOT_BACK_ROUNDS` 这一段，实现不改。演练判据④ 按 `DRILL_BACK_WAIT` 缺省 660 秒（`(2 + 3) × 120 + 60`）判。曾列出的两种严格「3 轮」改法（`mark_unhealthy` 换成只借不改健康位 / 切回计数把迟滞 2 轮算进去）都改 §5.6 既有语义，不做。
