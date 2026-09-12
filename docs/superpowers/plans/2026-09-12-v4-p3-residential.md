# v4 P3 住宅模块实施计划（`crates/bui/src/modules/residential/`）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 `bui` 的住宅模块：上游池增删改与类型自动探测、出口体检（三方交叉分类 + 搜索/AI/支付/端口集/UDP ASSOCIATE）、每 2 分钟的健康巡检与 Clash API 热切换（含 relay 重启后重放选择）、自动黑名单（journald 候选学习 → 硬拒确认 → 每日批量生效 → 每日复核移除）、13 个面板端点与 `bui residential` CLI 子命令——全部作为一个 `Module` 挂进 P1 的对账器、Router 与后台任务组，不重复渲染任何内核配置。

**Architecture:** `ResidentialModule` 实现 P1 Task 4 的 `Module`。`render()` **返回空** `Vec<Artifact>`：`singbox-relay.json` 已由 P1 Task 10 的 `CoreFilesModule` 从 `state.residential.default_group()` 渲染，P3 只改 `state` 再发 `Event::StateChanged("residential")`，由 P1 的 500ms 去抖对账完成重渲染、`sing-box check` 与 `b-ui-relay` 重启（渲染边界见「依赖与契约决策 §B」）。所有碰网络的探测走**同步** `Prober` / `Clash` trait（真实实现用 `reqwest::blocking` + `std::net::TcpStream`，与 P1 的 `Host`/`Fetcher` 同一形态），调用点一律包在 `tokio::task::spawn_blocking` 里、并发上限 4；测试只注入 `FakeProber` / `FakeClash` / P1 的 `FakeHost`，不联网、不碰真实系统。运行时数据（健康迟滞、24h 成功率桶、黑名单候选计数、待生效条目、journald 游标、体检结果、当前生效上游 tag）全部落在 P1 `RuntimeData.extra["residential"]`。

**Tech Stack:** Rust 1.93 stable（edition 2021）、tokio 1（`JoinSet` / `Semaphore` / `broadcast` / `time`）、axum 0.8、reqwest 0.12（`blocking` + `rustls-tls` + `socks`，只用于探测）、serde / serde_json、base64 0.22（`Proxy-Authorization: Basic`）、uuid 1、time 0.3、clap 4（derive，CLI 子命令）、tracing、anyhow / thiserror；dev：pretty_assertions、tempfile、tokio `test-util`（`start_paused`）。消费 `bui-schema`（P0）与 `bui` 的 P1 骨架。

**Spec:** `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`（§5 全节、§4.3 面板 API、§2.1 运行时数据、§2.2 对账与重启映射、§3.1 relay 拓扑）；`docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`（R13，v4 只取其日志形态、候选表与探测判定表，状态文件形态改用 v4 模型）；总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1 消费契约、C2 模块边界、C3 路径、裁决记录）；调研 `docs/superpowers/research/2026-09-11-research-review.md` §D（Decodo 实测：硬拒形态 = CONNECT 403、407 = 凭据失效、端口只放行 80/443）；审计 `docs/superpowers/audits/2026-09-11-architecture-audit.md` §3.4。

## Global Constraints

- Rust stable ≥ 1.85（本机 1.93），edition 2021，`cargo clippy --workspace --all-targets -- -D warnings` 零告警，`cargo fmt --check` 通过。
- 发布目标 `x86_64-unknown-linux-musl` 与 `aarch64-unknown-linux-musl`；依赖优先纯 Rust；TLS 用 `rustls` + `ring`，**禁止 openssl**（`reqwest` 一律 `default-features = false`）。
- sing-box 配置兼容 1.12–1.14：typed DNS servers、TUN `address` 数组、rule action（`sniff`/`hijack-dns`/`reject`）、`route.default_domain_resolver`、**不用** `rule_set` / `download_detour`。**P3 不渲染任何 sing-box 配置**，这条约束由 `bui_schema::render::relay`（P0）与 P1 Task 10 保证；P3 只改 `ResidentialGroup`，改完必须能过 `sing-box check`（由 P1 的 `Verify::SingBox` 在对账时把关）。
- 凭据、密钥、密码不进 argv、不进日志：探测一律经 `reqwest::Proxy::basic_auth` 或自建 SOCKS5/CONNECT 握手（**不 exec curl**）；`tracing` 里出现的 URL 一律过 `crate::redact::url_credentials`，密码一律过 `crate::redact::secret`；面板响应只回 `username` 的前两字符 + `***`，**永不回 `password`**。
- `state.json`、`runtime.json` 0600（由 P1 的 `Store` / `Runtime` 保证，P3 不自己写这两个文件，只经 `Store::update` / `Runtime::update`）。
- 生产主机只用别名 bwg-rick / bwg-tizi / baiyi；公开仓库里不出现真实 IP、域名、供应商凭据。测试与示例只用合成值：域名 `example.com`、IP `203.0.113.10`（VPS）/ `198.51.100.7`（住宅出口）、上游 `isp.example.net:10007`、用户名 `user1`、密码 `pw1`。
- 分支 `v4`，任务分支 `v4-p3-t<N>`，每任务一个 commit，格式 `feat(resi): …` / `test(resi): …` / `fix(resi): …`，尾部附 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`；不 `git commit -a`；不裸 `git stash`；不推送。
- 每个任务先写失败测试再实现。
- **测试位置约定**（同 P1）：`bui` 是 bin-only crate，P3 全部测试都是 crate 内单元测试（`#[cfg(test)] mod tests`），这样 `FakeHost` / `testutil::sample_state()` 能复用；不建 `crates/bui/tests/`。`FakeProber` / `FakeClash` 用 `#[cfg(test)]` 声明，避免 `-D warnings` 下的 dead_code。
- **不碰真实系统的铁律**：单元测试只允许接触 `tempfile` 给的临时目录；任何 `systemctl` / `journalctl` 必须经 P1 的 `Host`，任何网络探测必须经 `Prober` / `Clash`，测试里注入 fake。真机验证留给 M2 里程碑验收（bwg-rick）。**唯一豁免**：T3 的 CONNECT / SOCKS5 握手与 T4 的 `HttpClash` 是协议层字节序列，注入 fake 覆盖不到，允许在测试进程内用 `std::net::TcpListener::bind("127.0.0.1:0")` 起一个假代理 / 假 Clash API（端口由内核分配故不冲突，不出网、不碰 systemd、不写 `/etc`），见 T3 Step 5 与 T4 Step 4。
- v3 的 `server/residential-helper.sh`、`server/resi-health.sh`、`web/server.js`、`web/app.js` 在 P3 期间**只读**，作为移植参照与前端契约真源，不修改、不删除（P5 最后一个任务才删）。**P3 不改 `web/` 下任何文件**（前端 4 处改动归 P2，改同一文件会与 P2 冲突）——这是 §A「v3 路径别名」存在的唯一理由。
- `#![allow(dead_code)]`：`bui` 是 bin crate，`pub` **不能**消除 dead_code（P1 `main.rs` 顶部的注释已说明这件事），`cargo clippy --all-targets` 还会编一次不带 `cfg(test)` 的 bin 目标，所以「在本任务的测试里至少调用一次」也压不住告警——T1 的常量与 `fanout`、T2–T9 的全部 `pub fn`，在 T11 把 `routes`/`spawn` 接上之前都是真死代码。因此：**T1 在 `crates/bui/src/modules/residential/mod.rs` 顶部加一行模块级 `#![allow(dead_code)]`**（lint 等级按词法作用域继承，`mod state;` 等子模块的条目一并覆盖），**T12 删掉它**——T12 之后 `cli.rs`、`commands/menu.rs`、`api/health.rs` 三处接线完成，模块内已无死代码。不在 T11 删的原因：`cli::{ResidentialCmd, run, menu_items}` 要等 T12 的 `main.rs` / `menu.rs` 补丁才有调用方。P1 `main.rs` 顶部那行 crate 级 `allow`（Task 17 收口删）与本行互不依赖，P3 排在 Task 17 前后合并行为都一样。
- **合并顺序**：T1–T11 只依赖 P1 Task 1–15（即 `crates/bui/src` 已合并的部分），**排在 P1 Task 17 之前合并**；只有 T12 必须等 P1 Task 17（要往 `commands::menu::items()` 插一项）。

---

## 文件结构

```
crates/bui/src/modules/residential/mod.rs         ResidentialModule（Module impl）、全模块常量、fanout 并发助手、#[cfg(test)] 住宅夹具
crates/bui/src/modules/residential/state.rs       ResiRuntime（runtime.extra["residential"]）、健康迟滞与 24h 桶、组读写助手
crates/bui/src/modules/residential/proxy.rs       Prober trait / ConnectVerdict / ReqwestProber（CONNECT 状态、SOCKS5 REP、UDP ASSOCIATE、经代理 GET/JSON、直连 TCP）/ FakeProber
crates/bui/src/modules/residential/clash.rs       Clash trait（GET/PUT /proxies/resi-pool）/ HttpClash / FakeClash / tag↔upstream 映射
crates/bui/src/modules/residential/journal.rs     b-ui-relay 日志行解析（403 / SOCKS 拒绝）与按游标增量收集（经 Host::run）
crates/bui/src/modules/residential/check.rs       出口三方交叉分类 + 搜索/AI/支付/端口集/UDP 体检 → CheckReport / ports_allowed
crates/bui/src/modules/residential/upstream.rs    上游增删（四种格式解析、类型自动探测整轮重试一次、verified）、总开关/模式/关键字/优先级
crates/bui/src/modules/residential/health.rs      2 分钟巡检、迟滞、切换决策（priority → 24h 成功率）、≥60s 限速、成员集变化本轮不切、RelayRestarted 重放
crates/bui/src/modules/residential/blacklist.rs   候选计数（只计白名单内端口）、每 5 分钟的硬拒确认状态机、每日 04:00（服务器本地时间）批量生效、每日复核移除、pins
crates/bui/src/modules/residential/api.rs         18 条路由（spec §4.3 的 12 条 + 3 条 spec 未列 + enable + 3 条 v3 别名）+ 全部请求/响应 DTO
crates/bui/src/modules/residential/cli.rs         bui residential 子命令定义与派发（经 unix socket 调自己的端点）、住宅子菜单
crates/bui/src/modules/mod.rs                     追加一行 `pub mod residential;`（P1 文件，仅此一行）
crates/bui/src/serve.rs                           `modules()` 追加一行 `Arc::new(ResidentialModule::new()),`；同文件 `p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 的 `names` 断言改为子集断言并追加 `"residential"`（P1 文件，一行实现 + 一条测试断言，裁决 D14，Task 1）
crates/bui/Cargo.toml                             追加 `base64`，`reqwest` 加 `socks` feature（P1 文件，仅 `[dependencies]` 内追加）
crates/bui/src/cli.rs                             追加 `Command::Residential { … }` 一个变体（P1 文件，Task 12）
crates/bui/src/main.rs                            追加一个 dispatch arm（P1 文件，Task 12）
crates/bui/src/commands/menu.rs                   追加 `MenuAction::Residential` 与一个 `MenuItem`（P1 文件，Task 12）
crates/bui/src/api/health.rs                      `residential:` 字段由 `None` 改为读 `runtime.extra["residential"]`（P1 文件，一行，Task 12）
```

`mod.rs` 在 **Task 1** 一次建齐全部 11 个桩文件（`//! placeholder filled by Task N`）并写全 `pub mod` 行，后续任务只填自己的文件——并行合并零冲突，任何单个任务合并后 workspace 都能编译。

---

## 依赖与契约决策

### §A 面板端点：规范路径 + v3 路径别名（**需 Fable 裁决 D1**）

spec §4.3 把住宅端点写成 `/api/residential/{status,add,remove,global,domains,health,restore-default}` 再加五个新端点；但仓库里 `web/app.js` 实际调用的是 v3 的另一套路径（逐行核对 `web/app.js:790,802,809,827,843,848,875,964,982,992,1067`）：

| v3 实际调用（`web/app.js`） | 语义 | 规范路径（spec §4.3） |
|---|---|---|
| `GET /api/residential` | 状态 | `GET /api/residential/status` |
| `POST /api/residential` `{url}` | 加上游 | `POST /api/residential/add` |
| `POST /api/residential` `{domains:[…]}` | 设关键字 | `POST /api/residential/domains` |
| `POST /api/residential` `{reset:true}` | 回到跟随默认 | `POST /api/residential/restore-default` |
| `DELETE /api/residential` | 总开关关 | `POST /api/residential/enable` `{enabled:false}` |
| `POST /api/residential/enable`（空体） | 总开关开 | `POST /api/residential/enable` `{enabled:true}` |
| `POST /api/residential/urls` `{url}` | 加上游 | `POST /api/residential/add` |
| `DELETE /api/residential/urls/<host:port>` | 删上游 | `POST /api/residential/remove` |
| `POST /api/residential/global` `{global}` | 切模式 | 同名，保留 |
| `GET /api/residential/health` | 体检卡 | 同名，保留 |

spec §4.3 又明写前端「**四处改动**」且四处都不涉及住宅。两者只能二选一。**本计划的选择**：规范路径为准，同时在同一个 `Router` 上注册上表左列的别名，指向同一批 handler——`POST /api/residential/enable` 只存在于 v3 一侧（spec §4.3 的规范集合里没有它），原样保留即可、不需要第二条路径，所以别名只多出 3 条路由 / 5 个「方法 + 路径」对（`GET|POST|DELETE /api/residential`、`POST /api/residential/urls`、`DELETE /api/residential/urls/{host_port}`；完整路由表见 Task 10）。理由：① 前端零改动，spec §4.3 的「四处」不被打破；② P3 因此完全不碰 `web/`，与 P2 的前端改动零文件重叠（Global Constraints 的硬要求）；③ 代价是 `api.rs` 里 3 行 `.route(...)` 与一个 `v3_post` 多路分发 handler，可在 P5 删前端兼容层时一并删掉。**替代方案**（Fable 若选它，Task 10 删掉别名、并在 P2 的前端任务里追加 11 处 `fetch` 路径改名）：只留规范路径。

`status` 与 `health` 的**字段名逐字照 v3**（`web/server.js:2158-2196` 与 `:2413-2513`），因为 `app.js` 直接读它们：`enabled` / `global` / `urls[].{host,port,username,name,type,lastVerifiedIp,displayUrl}` / `domains` / `domainsFollowDefault` / `lastVerifiedIp` / `lastVerifiedIspInfo`；`health` 的 `enabled` / `urls` / `domains_count` / `mode` / `selected` / `members[].{tag,type,host,port,active,failstreak,okstreak,egress}` / `current_egress_ip_test` / `egress_ip_type` / `via_proxy_isp`。**`egress_ip_type` 的取值必须是 v3 的中文串**（`家庭宽带 IP` / `IDC机房 IP` / `代理 IP` / `移动网络 IP` / `unknown`）——`app.js:1095` 用 `/IDC|机房/i` 正则判色，换成英文枚举会让「非住宅」告警永久失效。新增字段（`mode`、`selected_upstream_id`、`blacklist`、`alerts`、`members[].blacklist_count` 等）一律**追加**，不改也不删任何 v3 字段。`members[].blacklist_count` 与 `upstreams[].blacklist_count` 的语义**定死为「只数这一条上游自己的 `auto` 条目」**（`auto[].upstream_id == 该上游 id`），**不含 `pins`**——pins 是全局强制直连规则、不属于任何上游，计进去会让面板上每个上游都凭空多出 N 条。全局计数看 `status.blacklist`（`{pins,auto,pending,candidates}`）。两处同名字段只经 `api::auto_count` 这一个函数算，永远相等（Task 10）。

### §B 渲染边界：P3 **不**渲染 relay 配置

P1 Task 10 的 `CoreFilesModule::render` 已经产出 `<base>/singbox-relay.json`：

```rust
let group = s.residential.default_group().cloned().unwrap_or_default();
let relay = bui_schema::render::relay::config(&group, &relay_opts(s, p));
Artifact::file(p.base_dir.join("singbox-relay.json"), serde_json::to_vec_pretty(&relay)?)
    .verify(Verify::SingBox)
    .restart(Unit::restart("b-ui-relay"))      // 无 restart_key ⇒ 内容变即重启
```

所以：
- `ResidentialModule::render(&self, _s, _ctx) -> Vec<Artifact> { Vec::new() }`，并在函数上方写死这条注释（Task 1 的测试 `render_is_empty_because_core_files_owns_the_relay_config` 锁住它）。P3 **绝不**产出 `singbox-relay.json`，否则同一路径两个 artifact、diff 结果取决于模块注册顺序。
- P3 改住宅状态的唯一路径是 `state::update_group(&store, &bus, |g| …)`：`Store::update` 写 `state.json` → `bus.send(Event::StateChanged("residential"))` → P1 `serve::debounce_loop` 500ms 去抖 → 单 consumer 调 `reconcile_from_ctx` → diff 发现 `singbox-relay.json` 内容变 → `sing-box check` → 写盘 → 重启 `b-ui-relay` → `Event::RelayRestarted` → P3 的 `health::replay_loop` 重放当前生效的上游（`runtime.selected_upstream_id`）。
- **`relay 重启是唯一掐连接的动作`（spec §5.4）的落地推论**：凡是会改 relay 配置内容的操作都必须是「管理员显式动作」或「每日 04:00 窗口」。因此自动健康切换与手动切换**都不写 state**，只经 Clash API + `runtime.selected_upstream_id`（见 §C）；`auto` 黑名单条目确认后先进 `runtime.pending`，到 04:00 才批量写进 `state.blacklist.auto`（见 §D）。

### §C `selected_upstream_id`：state 里的「配置落点」与 runtime 里的「当前生效」（**需 Fable 裁决 D3/D4**）

`bui_schema::render::relay::config` 用 `g.selected_upstream_id` 决定三件事：selector 的 `default`、`ports_allowed` 取反的那条 `port_range` 规则、以及 `auto` 黑名单按 `upstream_id` 的过滤。因此**每次把切换结果写进 state，都会重写 relay 配置并重启 relay**，与 spec §5.3「经 Clash API 切换」+ `interrupt_exist_connections: false` 的用意（不掐既有连接）正相反。

本计划的分工：

| 字段 | 含义 | 谁写 | 是否触发 relay 重启 |
|---|---|---|---|
| `state…selected_upstream_id` | **配置里的落点**：selector `default`、`port_range` 取反依据、`auto` 过滤依据 | 增删上游（Task 7）、每日 04:00 批量（Task 9）、`blacklist/apply`（Task 10） | 是（这三处本来就在重启窗口内） |
| `runtime…selected_upstream_id` | **当前实际生效的上游**（uuid；给 Clash API 用的 `resi-N` 由 `clash::tag_of` 现算） | 自动健康切换与手动 `select`（Task 8/10）、relay 重启后的重放（Task 8） | 否 |

- 自动切换与手动 `select` 只发 `PUT /proxies/resi-pool` 并记 `runtime.selected_upstream_id`（D3）。
- **运行时主键一律是 uuid，不是 tag**：`resi-N` 是「`upstreams` 里的第 N 条」这种**位置**键，而 T7 的 `add` / `remove` 会 `renumber` 并触发 relay 重渲染——同一个 `resi-2` 在增删前后可能指向两条不同的上游。若 runtime 存 tag，会同时坏三件事：`replay_loop` 把重启后的选择重放到错的上游（或 404）、每日 04:00 把错的 uuid 写回 `state.selected_upstream_id`（`ports_allowed` 取反与 `auto` 过滤随之全错）、健康 streak 与 24h 成功率错位。所以 `ResiRuntime.selected_upstream_id: Option<Uuid>` 与 `ResiRuntime.health: BTreeMap<String, HealthState>`（key = `Uuid::to_string()`）都以 uuid 为主键，tag **只在调 Clash API 的那一瞬**由 `clash::tag_of(&g, id)` 现算；`upstream::remove` 同步清掉 `health` / `selected_upstream_id` 里该 uuid 的痕迹（T7）。
- 两者漂移期间 relay 的 `ports_allowed` / `auto` 过滤仍按旧落点——同供应商各上游的 `ports_allowed` 与被拒目标集实测高度一致（调研 §D：Decodo 10 个 IP 一律只放行 80/443），所以漂移的实际影响是「黑名单可能少几条」，fail-open 方向，不会误伤。
- **每日 04:00 批量时把漂移持久化**：若 `runtime.selected_upstream_id` 与 `state.selected_upstream_id` 不同，就在同一次 `Store::update` 里一起写回（D4）——两边都是 uuid，全程不需要任何 tag 换算。这样最坏 24 小时内落点追上现实，且不额外增加任何一次重启。`status` 响应用 `selected_pending_persist: bool` 把漂移状态摆给运维看。
- Fable 若否决 D4：删掉 Task 9 第 6 步里那三行，`selected_pending_persist` 永远为 `true` 直到管理员手动 `blacklist/apply`；此时必须在面板说明里写清「自动切换后端口白名单与黑名单仍按上一个上游」。
- **relay 有三条重启来路，只有一条会发 `Event::RelayRestarted`**（逐条按已合并代码核对）：① 对账重启——`crate::serve::reconcile_from_ctx`（`crates/bui/src/serve.rs:202`）在 relay 被重启后 `bus.send(Event::RelayRestarted)`，这条会触发 T8 的 `replay_loop`；② 看门狗重启——`crates/bui/src/modules/watchdog.rs:141` 的 `host.systemd("restart", &t.unit)`，**不发事件**；③ 面板/CLI 重启——`POST /api/services/b-ui-relay/restart`（`crates/bui/src/api/mod.rs:23` → `system::service_action`，`commands/menu.rs:229` 的「重启数据面」也走它），**不发事件**。relay 一重启，selector 就回落到配置里的 `default`（= `state.selected_upstream_id`，为空则池首），于是 ②③ 之后「实际生效」与 `runtime.selected_upstream_id` 会漂移。
  本计划**不**要求 P1 给 ②③ 补事件（那是改 P1 的第八处文件，超出 §H 的授权），而是让 **T8 的巡检规则 6 自己兜住**：每轮（≤ 120 秒）把 `runtime.selected_upstream_id` 当真源**重放**回 Clash，绝不反过来采纳 Clash 的 `now`。代价是 ②③ 之后最坏 120 秒的出口漂移（relay 仍在一条池内成员上，不断网）。**若 Fable 愿意向 P1 提一条改动**（裁决 D11），在 `watchdog.rs` 的重启分支与 `system::service_action` 的 `unit == "b-ui-relay"` 分支各加一行 `bus.send(Event::RelayRestarted)`，漂移窗口就从 120 秒降到事件延迟；规则 6 的兜底照旧保留（两者不冲突，重放是幂等的）。

### §D 黑名单的三段式（spec §5.4 的落地）

| 段 | 存放位置 | 触发 | 生效方式 |
|---|---|---|---|
| 候选 | `runtime.candidates`（key = `<upstream_id>\|<host>\|<port>`） | ① 每 `JOURNAL_POLL_SECS`（5 分钟）按 journald 游标增量读 `b-ui-relay` 日志并计数（Task 5/9）；② 每日固定探针集 + 端口集（Task 9）。两条路都**只对 `port ∈ upstream.ports_allowed`（`None` 时视为 `[80, 443]`）的拒绝计候选**（裁决「端口类拒绝不进域名黑名单」，Task 9 `port_allowed`） | 不生效，只计数（阈值 `CANDIDATE_THRESHOLD = 3`）；**确认进度也记在候选上**（`confirms` / `last_confirm_at`） |
| 待生效 | `runtime.pending` | `journal_loop` 每 `JOURNAL_POLL_SECS` 先 `learn_from_journal` 再 `confirm_round`（Task 9）：候选攒满「硬拒 + 直连 TCP 可达、间隔 ≥ `CONFIRM_MIN_GAP_SECS`（10 分钟）、连续 `CONFIRM_NEEDED` 次」即升上来 ⇒ **≥10 分钟就能进 `pending`，不必等 04:00**（裁决「黑名单确认节奏」） | 不生效；但进了 `pending` 就等于「确认完毕」，所以 `flush_pending` / `apply_now` 无条件全量写 |
| 生效 | `state.blacklist.{pins,auto}` | `pins` 与 `blacklist/apply` 立即；`auto` 每日 04:00 **批量生效**（04:00 窗口只做批量生效 + spec §5.4 (b) 的每日探针集/端口集与每日复核，**不再承担确认节奏**） | 写 state → 对账重渲染 relay → 重启 |

- 规则一律 `Rule::DomainSuffix(<被拒的完整主机名>)`，**不**泛化到注册域名（`gateway.icloud.com` 不会变成 `icloud.com`）。
- 裸 IP 目标与端口类拒绝**不进黑名单**：前者 `blacklist::is_bare_ip` 直接丢弃；后者由 `blacklist::port_allowed` 在计候选前就滤掉（白名单外端口的硬拒是**端口策略**，已由上游的 `ports_allowed` 表达，体检学得，Task 6），因此 `courier.push.apple.com:5228` 被拒不会变成一条域名规则。
- 复核：每日对 `state.blacklist.auto` 每条复探一次，连续 3 次不再被拒（`passes >= 3`）→ 移除（同一次 04:00 的 `Store::update`）。
- 软拦截（返回 200 拦截页）识别不了 → `status` 响应带 `notes: ["软封锁（返回 200 拦截页）无法自动识别，请手动钉住（pins）"]`，面板说明沿用该文案。
- **每日窗口的时区（需 Fable 裁决 D5）**：spec §5.4 写「每日 04:00（**服务器本地时间**）」，R13 §7 写「北京时间 04:00（`OnCalendar=… Asia/Shanghai`）」，两台生产机时区是 UTC——两份文档差 8 小时。本计划按 **v4 spec** 实现：`DAILY_HOUR: u8 = 4`，判据是 `host.now().hour()`（P1 `RealHost::now()` 返回 `OffsetDateTime::now_utc()`，生产机 UTC ⇒ 04:00 UTC = 服务器本地 04:00）。不引入 tz 数据库（`time-tz` 会把 musl 静态构建拖进 IANA 数据）。Fable 若选北京时间，改 `DAILY_HOUR = 20` 一个常量即可（Task 1），Task 9 的两条测试跟着改小时数。

### §E journald 读取方式：游标增量轮询，不用 `-f`（**需 Fable 裁决 D2**）

spec §5.4 写「跟随 `journalctl -u b-ui-relay -f -o json`」。P1 的 `Host` trait 只有同步的 `run(program, args) -> CmdOut`（一次性收全输出），`-f` 永不返回，无法经 `Host` 调用；绕过 `Host` 直接 `tokio::process` 会让「测试不碰真实系统」失守，还要新造一个流式 trait。

改为：每 `JOURNAL_POLL_SECS = 300` 秒调一次
`journalctl -u b-ui-relay --no-pager -o cat --show-cursor --after-cursor <游标>`
（首次或游标失效时用 `--since -25h` 代替 `--after-cursor`，与 R13 §6.2 同窗口）。`--show-cursor` 的最后一行是 `-- cursor: s=…`，解析后存 `runtime.journal_cursor`，**不重不漏**。全部经 `Host::run`，`FakeHost.scripted` 即可完整测试。`host.which("journalctl") == false` 时跳过日志学习、只靠每日探针集，并往 `runtime.alerts` 记一行。

同一个 `JOURNAL_POLL_SECS` 轮次里紧接着跑一次 `confirm_round`（裁决「黑名单确认节奏」）：`learn_from_journal` 先把本轮的拒绝计进候选，`confirm_round` 再对「已达阈值、距上次确认 ≥ 10 分钟」的候选做一次确认。于是一条候选最快 `CONFIRM_MIN_GAP_SECS`（10 分钟：第 1 轮确认一次，t=600 秒的那一轮确认第二次）就能进 `pending`，写进 `state.blacklist.auto` 仍等 04:00 窗口（或面板「立即应用」）；缺 `journalctl` 时本轮只是学不到新候选，确认轮照跑。

### §F 消费 P1 的签名（以 `2026-09-11-v4-p1-daemon-core.md` 的 Task 1/2/3/4/5/9/13/15 为契约，逐字照用，不得改写）

```rust
// Task 2
crate::state::store::Store            // async fn read() -> Arc<State>
                                      // async fn update(f: impl FnOnce(&mut State)) -> anyhow::Result<Arc<State>>
                                      //   ⚠ 零变更不写盘：测试要断言「真写了」就必须改出不同的值
crate::state::runtime::Runtime        // async fn read() -> RuntimeData
                                      // async fn update(f: impl FnOnce(&mut RuntimeData)) -> RuntimeData
crate::state::runtime::RuntimeData    // 字段 extra: BTreeMap<String, serde_json::Value>（#[serde(flatten)]，P2/P3 的运行时数据都放这里）
// Task 3
crate::sys::{Host, CmdOut, Proto}     // Host::run(&self, program:&str, args:&[&str]) -> Result<CmdOut>；CmdOut{status,stdout,stderr}、CmdOut::ok()
crate::sys::fake::FakeHost            // #[cfg(test)]；with(|i| …)、ops()、scripted: Vec<(String, CmdOut)>（前缀匹配）、which: BTreeSet<String>
// Task 4
crate::reconcile::{Artifact, Module, RenderCtx, DaemonCtx}
// trait Module { fn name(&self)->&'static str; fn render(&self,&State,&RenderCtx)->Vec<Artifact>;
//                fn routes(&self)->axum::Router<crate::api::AppState> { Router::new() }
//                fn spawn(&self,_ctx:DaemonCtx)->Vec<tokio::task::JoinHandle<()>> { Vec::new() } }
// struct DaemonCtx { store: Store, runtime: Runtime, bus: crate::api::EventBus, host: Arc<dyn Host>, paths: Paths }  // Clone
crate::api::{AppState, Event, EventBus}
// enum Event { StateChanged(&'static str), RelayRestarted, ReconcileRequested{force:bool} }
// EventBus::{new, send(Event), subscribe() -> broadcast::Receiver<Event>}
// struct AppState { store, bus, runtime, host: Arc<dyn Host>, started_at: OffsetDateTime, version: &'static str, login: LoginLimiter }  // Clone
// Task 13
crate::api::auth::require_admin       // 由 P1 的 router() 统一套在所有模块路由上，P3 的路由无需自己加中间件
crate::api::router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router   // protected.merge(m.routes()) 后统一 layer(require_admin)
// Task 15
crate::serve::modules(manifest: Option<Manifest>) -> Registry                     // P3 在 modules 数组里追加一行
// Task 1 的工具
crate::redact::{url_credentials, secret}
crate::util::{fmt_rfc3339, parse_rfc3339}
crate::paths::SOCKET_PATH
crate::ipc::Client                     // new(path)；async fn available() -> bool
                                       // async fn request(method, path, body) -> Result<(u16, Value)>
crate::testutil::sample_state()        // #[cfg(test)]
```

### §G 消费 P0 `bui-schema` 的签名（以 `crates/bui-schema/src` 真实代码为准）

```rust
bui_schema::model::{State, Residential, ResidentialGroup, ResiMode, Upstream, UpstreamKind, Verified,
                    Blacklist, Rule, Pin, AutoEntry, DEFAULT_GROUP}
// ResidentialGroup { enabled: bool, mode: ResiMode, keywords: Option<Vec<String>>, upstreams: Vec<Upstream>,
//                    selected_upstream_id: Option<Uuid>, blacklist: Blacklist }  + pool_active() -> bool
// Upstream { id: Uuid, name: String, kind: UpstreamKind, host: String, port: u16, username: String, password: String,
//            priority: u32 /* serde default 100 */, provider: Option<String>, region: Option<String>,
//            ports_allowed: Option<Vec<u16>>, verified: Option<Verified> }
// UpstreamKind::{Socks5, Http}（serde snake_case）；ResiMode::{Global, Split}（默认 Split）
// Verified { ip: String, asn: Option<u32>, org: Option<String>, country: Option<String>, at: String }
// Rule::{DomainSuffix(String), Domain(String), Port(u16)}（serde tag="kind" content="value" snake_case）
// Pin { rule: Rule, note: String, created_at: String }
// AutoEntry { upstream_id: Uuid, rule: Rule, hits: u64, confirmed_at: String, last_verified_at: String, passes: u32 }
// Residential::default_group(&self) -> Option<&ResidentialGroup>
bui_schema::parse::{upstream_url(&str) -> Result<UpstreamInput, ParseError>, UpstreamInput, ParseError}
// UpstreamInput { kind: Option<UpstreamKind>, host: String, port: u16, username: String, password: String }
// ParseError::{Scheme(String), Format, Port(String), Other(String)}（thiserror，Display 即面板文案）
bui_schema::keywords::DEFAULT_KEYWORDS: &[&str]                 // 67 条
bui_schema::render::SplitRules::from_group(&ResidentialGroup) -> SplitRules   // { enabled, global, keywords }
```

`DEFAULT_GROUP` 是 `bui-schema` 里的常量（值 `"default"`）；P3 的 `GROUP_DEFAULT` 直接 `pub use` 它，不另立一个字符串。

### §H 对 P1 文件的改动清单（**需 Fable 裁决 D0**）

Global Constraints 允许的两处（`modules/mod.rs` 追加 `pub mod`、`serve.rs` 的 `modules()` 追加一行）不够，本计划另需下表**七处**小改：两处在 Task 1（`Cargo.toml` 的依赖行、`serve.rs` 的注册测试断言，后者**已由裁决 D14 批准**），五处在 Task 12。除 `serve.rs` / `api/mod.rs` 两条**测试断言**是「改一个已有断言」外，其余全是「在段内追加」。每处都在对应任务里写了逐字补丁，且全部是「在段内追加」，与 P2 的同文件改动是机械可解的 `git` 冲突。

**建议的批准口径**：`Cargo.toml` 那行是**硬需求**（经 SOCKS5 上游发 HTTPS 必须有 `reqwest` 的 `socks` feature；`base64` 已在 workspace 依赖表里，只是本 crate 还没引），建议直接纳入允许清单；T12 那五处是「要 CLI 与数字菜单、要 `/api/health` 带住宅摘要」的必然代价，建议按「**追加式** + 批准时回写总纲 C5」放行。

| 文件 | 改动 | 任务 | 为什么不能避免 |
|---|---|---|---|
| `crates/bui/Cargo.toml` | `[dependencies]` 追加 `base64.workspace = true`；`reqwest` 的 features 追加 `"socks"` | T1 | CONNECT 的 `Proxy-Authorization: Basic` 要 base64；经 SOCKS5 上游发 HTTPS 要 reqwest 的 socks feature，无替代。P2 也会在同一段追加依赖，冲突是机械可解的 |
| `crates/bui/src/serve.rs`（**测试断言**，**裁决 D14 已批准**） | `p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 的 `names` 全等断言改成「包含这些模块名」的子集断言，名单末尾追加 `"residential"`；**函数名不改**（P2 的 T13 会在同一张名单里追加 `"panel"`，两边都只追加一行字符串） | T1 | `modules()` 一追加注册行，这条 P1 已合并的测试（`crates/bui/src/serve.rs:1017-1023`）就必红：`assert_eq!(names, vec!["core-files","units","system","ssh","certs","watchdog"])` 是**全等**断言而不是 `contains`。逐字新断言见 T1 Step 3 |
| `crates/bui/src/cli.rs` | `Command` 追加一个变体 `Residential { #[command(subcommand)] cmd: ResidentialCmd }` | T12 | clap derive 的子命令必须挂在 `Command` 上；总纲 C5 的 CLI 清单由此扩一项，批准时回写 C5 |
| `crates/bui/src/main.rs` | `dispatch` 追加一个 arm | T12 | 同上 |
| `crates/bui/src/commands/menu.rs` | `MenuAction` 追加 `Residential`、`items()` 追加一项 | T12 | spec §2.4「数字菜单」；P1 Task 17 的菜单是唯一入口 |
| `crates/bui/src/api/health.rs` | `residential: None` → 读 `runtime` 的住宅段并过 `state::health_summary`（逐字补丁见 T12 Step 4） | T12 | spec §4.3 第 ③ 条「`/api/health` 增加上游体检结果」；P1 Task 13 的注释就写着「P3 填」 |
| `crates/bui/src/api/mod.rs`（**测试断言**） | `assert_eq!(h.residential, None, "P3 才填")` → 断言 `is_some()` 且含 `enabled` / `alerts` 键 | T12 | 上一行一改，这条 P1 已合并的测试（`crates/bui/src/api/mod.rs` 的 `health_reports_services_drift_watchdog_and_reconcile`）必红。它与 `commands/menu.rs` 的菜单项数、`serve.rs` 的模块名清单一起，构成**允许改 P1 测试断言的唯三处**（逐字新断言分别在 T12 Step 4、T12 Step 5 与 T1 Step 3 写死） |

---

## 并行编排

依赖是**编译依赖**（下游任务代码里出现了上游任务定义的类型/常量/函数），已逐条按本计划的代码核对：

| 阶段 | 任务 | 依赖 | 依赖的具体符号 |
|---|---|---|---|
| 串行 | **T1** 骨架 / 常量 / `fanout` / 渲染边界 | P1 T1–T15 全部合并 | `Module`/`Artifact`/`RenderCtx`/`DaemonCtx`、`serve::modules`、`modules/mod.rs` |
| 并行 | **T2** `state.rs` | T1 | `mod.rs` 的 `RUNTIME_KEY`/`FAIL_TO_UNHEALTHY`/`OK_TO_HEALTHY`/`GROUP_DEFAULT` 与 `#[cfg(test)] sample_state_with_pool()`；P1 `Runtime`/`RuntimeData::extra`/`Store`/`EventBus` |
| 并行 | **T3** `proxy.rs` | T1 | `mod.rs` 的 `PROBE_TIMEOUT_SECS`；`bui_schema::model::{Upstream, UpstreamKind}`；`crate::redact` |
| 并行 | **T4** `clash.rs` | T1 | `mod.rs` 的 `POOL`/`CLASH_TIMEOUT_SECS`；`bui_schema::model::ResidentialGroup` |
| 并行 | **T5** `journal.rs` | T1 | `mod.rs` 的 `JOURNAL_UNIT`；P1 `sys::{Host, CmdOut}` |
| 串行 | **T6** `check.rs` | T2、T3 | `proxy::{Prober, ConnectVerdict, HttpProbe}`、`state::ResiRuntime`（存 `checks`）、`mod::fanout` |
| 并行 | **T7** `upstream.rs` | T3、T6 | `proxy::{Prober, proxy_url}`、`check::{probe_exit, to_verified, ExitClass}`、`state::{group_of, update_group, update}`；`bui_schema::parse::{upstream_url, UpstreamInput, ParseError}` |
| 并行 | **T8** `health.rs` | T2、T3、T4 | `state::{ResiRuntime, HealthState, apply_hysteresis, record_probe, success_rate_24h}`、`proxy::Prober`、`clash::{Clash, tag_of, id_of_tag}` |
| 并行 | **T9** `blacklist.rs` | T2、T3、T5、T6 | `journal::{RejectLine, collect}`、`proxy::{Prober, ConnectVerdict}`、`state::{Candidate, PendingEntry, update_group}`、`check::derive_ports_allowed`（每日端口集重探，spec §5.4 (b)） |
| 串行 | **T10** `api.rs` | T6、T7、T8、T9 | 上述四个模块的全部 `pub fn` + `state::health_summary` + T1 的 `#[cfg(test)] sample_state_with_pool()` |
| 串行 | **T11** `mod.rs` 收口（`routes`/`spawn` 装配 + 端到端） | T10 | `api::routes`、`health::{health_loop, replay_loop}`、`blacklist::{journal_loop, daily_loop}` |
| 串行 | **T12** `cli.rs` + P1 四处挂接 | T11、**P1 T17** | `crate::ipc::Client`（P1 T14）、`crate::cli::Command`（P1 T1）、`commands::menu::{MenuItem, MenuAction, items, render_with}`（P1 T17）、`api::health::HealthResponse.residential`（P1 T13） |

**对 P1 的启动门槛**（与总纲「P1 的 state 存储 + API 框架两个任务完成后 → {P2, P3} 并行」的对齐）：本计划**可以**在 P1 Task 4（`Module`/`Artifact`/`AppState`/`EventBus`）与 Task 13（`router` / `require_admin`）合并后就开始**评审与起草实现**，但落地顺序有两个硬门槛：① **T1 要等 P1 Task 15 合并**，因为它要在 `serve.rs` 的 `modules()` 里追加注册行（Task 15 才创建该函数），且 `CoreFilesModule` 渲染 relay 配置这件事（Task 10）是契约决策 §B 的前提；② **T12 要等 P1 Task 17 合并**，因为它要在 `commands::menu::items()` 里插一项。T2–T11 之间没有任何对 P1 未合并任务的依赖，因此 **T1–T11 排在 P1 Task 17 之前合并**，模块级 `#![allow(dead_code)]` 由 T1 加、T12 删（见 Global Constraints）。若要更早开工，可行的切法是把 T1 拆成「建桩文件 + 常量 + fanout」（只依赖 P1 Task 4）与「注册进 `modules()`」（等 Task 15）两个 commit——本计划不这么拆，因为 T1 的渲染边界测试要引用 `core_files` 才有意义。

**并行组内文件零重叠**：T2/T3/T4/T5 各只写一个文件；T7/T8/T9 各只写一个文件。T1 建的桩文件在后续任务里只被各自的主人填写，没有第二个任务追加 `mod` 行。

**与 P2 的关系**：P3 只写 `crates/bui/src/modules/residential/**`，加上 §H 的五处追加式小改。P2 写 `crates/bui/src/modules/users*`、`api/users.rs` 一类的新文件，两者唯一可能撞的是 `Cargo.toml` 的 `[dependencies]` 与 `serve.rs` 的 `modules()`——都是追加行，`git` 冲突可机械解决。

---

### Task 1: 住宅模块骨架（11 个桩文件）、全模块常量、`fanout` 并发助手、渲染边界

**Files:**
- Create: `crates/bui/src/modules/residential/mod.rs`
- Create（桩，内容一行 `//! placeholder filled by Task N`）：`state.rs`（T2）、`proxy.rs`（T3）、`clash.rs`（T4）、`journal.rs`（T5）、`check.rs`（T6）、`upstream.rs`（T7）、`health.rs`（T8）、`blacklist.rs`（T9）、`api.rs`（T10）、`cli.rs`（T12），均在 `crates/bui/src/modules/residential/` 下
- Modify: `crates/bui/src/modules/mod.rs`（在现有 `pub mod` 行后追加一行 `pub mod residential;`）
- Modify: `crates/bui/src/serve.rs`（两处：① `modules()` 的数组里把 P1 留的注释行 `// P3 在此追加 ResidentialModule…` 换成 `Arc::new(crate::modules::residential::ResidentialModule::new()),`；② 同文件 `crates/bui/src/serve.rs:1017-1023` 的 `p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 把 `names` 的**全等**断言改成**子集**断言并追加 `"residential"`——裁决 D14 已批准这条改法，逐字补丁见 Step 3）
- Modify: `crates/bui/Cargo.toml`（`[dependencies]` 追加 `base64.workspace = true`；`reqwest` 的 `features` 追加 `"socks"`）

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Module, RenderCtx}`；`crate::modules::core_files::RELAY_CLASH_API`；`bui_schema::model::{State, DEFAULT_GROUP}`
- Produces:
```rust
// crate::modules::residential
// 文件顶部（紧跟 //! 文档注释）一行；T1 加、T12 删，理由见 Global Constraints
#![allow(dead_code)]

pub mod api; pub mod blacklist; pub mod check; pub mod clash; pub mod cli;
pub mod health; pub mod journal; pub mod proxy; pub mod state; pub mod upstream;

/// `RuntimeData.extra` 里本模块独占的键（P1 Task 2 的 `#[serde(flatten)] extra`）
pub const RUNTIME_KEY: &str = "residential";
/// 住宅分组（v4 只有一个；直接用 `bui-schema` 的常量，不另立字符串）
pub const GROUP_DEFAULT: &str = DEFAULT_GROUP;
/// relay 里 selector 的 tag（`bui_schema::render::relay` 写死同名）
pub const POOL: &str = "resi-pool";
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
/// 体检的 AI 可达性目标（spec §5.2）
pub const AI_HOSTS: [&str; 3] = ["gemini.google.com", "api.openai.com", "api.anthropic.com"];
/// 体检的支付可达性目标（spec §5.2；调研 §D：Decodo 上 pay.google.com / www.paypal.com 是 403）
pub const PAY_HOSTS: [&str; 3] = ["checkout.stripe.com", "pay.google.com", "www.paypal.com"];
/// 取出口 IP 的纯文本接口（v3 `residential-helper.sh:215` 同源）
pub const EXIT_IP_URL: &str = "https://api.ipify.org";
/// [`EXIT_IP_URL`] 的主机名（同 [`HEALTH_PROBE_HOST`]，给 `confirm_auth_failure` 用）
pub const EXIT_IP_HOST: &str = "api.ipify.org";

pub struct ResidentialModule;      // T11 给它加 prober / clash 两个 Arc 字段
impl ResidentialModule { pub fn new() -> Self; }
impl Default for ResidentialModule { fn default() -> Self { Self::new() } }
impl Module for ResidentialModule {
    fn name(&self) -> &'static str { "residential" }
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> { Vec::new() }
    // routes() / spawn() 用 trait 默认实现，T11 覆盖
}

/// 把一批同步探测并发跑完：并发上限 [`PROBE_CONCURRENCY`]，每个闭包在
/// `tokio::task::spawn_blocking` 里执行（`reqwest::blocking` 在 async 上下文会 panic，
/// 与 P1 的 `Fetcher` 同一条铁律）。返回长度与输入**严格相等**、顺序与输入一致；
/// `None` = 该项的阻塞任务 panic 了（调用方记一条 alert，不让一个坏上游带崩整轮巡检）。
pub async fn fanout<I, R, F>(items: Vec<I>, f: F) -> Vec<Option<R>>
where I: Send + 'static, R: Send + 'static, F: Fn(I) -> R + Send + Sync + 'static;

/// 住宅段的测试夹具。**不要拿 `crate::testutil::sample_state()` 的住宅段当池用**：
/// P1 已合并的那份是空池（`enabled:false, mode:split, upstreams:[], selected_upstream_id:null,
/// pins:[], auto:[]`），T2 的 `health_summary` 断言与 T10 的全部 harness 测试都需要
/// 「1 条 HTTP 上游 + 1 条 pin + 1 条 auto」的池，所以夹具在本模块里自己造，不改 P1 的文件。
#[cfg(test)] pub fn sample_group() -> bui_schema::model::ResidentialGroup;
/// `crate::testutil::sample_state()`，但 `residential.groups["default"]` 换成 [`sample_group`]
#[cfg(test)] pub fn sample_state_with_pool() -> State;
```

**为什么 `render` 返回空**（契约决策 §B，这段注释要逐字进代码）：`<base>/singbox-relay.json` 由 P1 Task 10 的 `CoreFilesModule` 从 `state.residential.default_group()` 渲染，并带 `Verify::SingBox` + `Unit::restart("b-ui-relay")`。P3 若也产出同一路径的 `Artifact`，`reconcile::diff::plan` 会拿到两个同路径项，写盘与重启取决于模块注册顺序——这是个静默的、按注册顺序变脸的 bug。P3 改住宅状态的唯一手段是 `state::update_group`（T2）。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/residential/mod.rs` 末尾：
```rust
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
                mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
                has_ufw: false, ufw_active: false, has_firewalld: false, firewalld_active: false,
                ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: false,
            },
        }
    }

    #[test]
    fn render_is_empty_because_core_files_owns_the_relay_config() {
        // 锁住契约决策 §B：singbox-relay.json 只由 P1 Task 10 产出。一旦有人在这里补一个
        // Artifact::File，同路径两个 artifact 的 diff 就会随模块注册顺序变脸。
        let arts = ResidentialModule::new().render(&sample_state(), &ctx());
        assert!(arts.is_empty(), "P3 不产出任何 artifact，实际 {}", arts.len());
        assert_eq!(ResidentialModule::new().name(), "residential");
    }

    #[test]
    fn constants_match_the_spec() {
        assert_eq!(GROUP_DEFAULT, "default");
        assert_eq!(POOL, "resi-pool");
        assert_eq!(CLASH_API, "127.0.0.1:9091");
        assert_eq!((HEALTH_INTERVAL_SECS, HEALTH_TRIES), (120, 2));
        assert_eq!((FAIL_TO_UNHEALTHY, OK_TO_HEALTHY), (2, 2));
        assert_eq!((SWITCH_MIN_INTERVAL_SECS, MANUAL_ROUND_MIN_GAP_SECS), (60, 60));
        assert_eq!((PROBE_TIMEOUT_SECS, PROBE_CONCURRENCY), (10, 4));
        assert_eq!((CONFIRM_MIN_GAP_SECS, CONFIRM_NEEDED), (600, 2));
        assert_eq!(REVIEW_PASSES_TO_REMOVE, 3);
        assert_eq!(DAILY_HOUR, 4, "spec §5.4 的 04:00 是服务器本地时间（决策 D5）");
        assert_eq!(PROBE_PORTS, [5228, 5223, 993, 22, 8080, 853]);
        assert_eq!(AI_HOSTS[2], "api.anthropic.com");
        assert_eq!(PAY_HOSTS[1], "pay.google.com");
        // 这两个 host 必须与对应 URL 的主机名严格一致：CONNECT 补判（T3
        // confirm_auth_failure）拿它们去开隧道，写歪了就补判到别的站点上去了
        assert!(HEALTH_PROBE_URL.starts_with(&format!("https://{HEALTH_PROBE_HOST}/")));
        assert_eq!(EXIT_IP_URL, format!("https://{EXIT_IP_HOST}"));
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
        assert_eq!(s.node.public_ip, "203.0.113.10", "T7/T10 的「出口 IP == 本机」判据吃它");
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
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential`
Expected: 编译失败，`file not found for module `residential`` / `cannot find function `fanout``。

- [ ] **Step 3: 建 11 个文件与三处挂接**

`crates/bui/src/modules/residential/` 下十个桩文件各一行，例如 `state.rs`：
```rust
//! placeholder filled by Task 2
```
（`proxy.rs` → Task 3、`clash.rs` → Task 4、`journal.rs` → Task 5、`check.rs` → Task 6、`upstream.rs` → Task 7、`health.rs` → Task 8、`blacklist.rs` → Task 9、`api.rs` → Task 10、`cli.rs` → Task 12。）

`crates/bui/src/modules/mod.rs` 追加一行（P1 文件，仅此一行）：
```rust
pub mod residential;
```

`crates/bui/src/modules/residential/mod.rs` 的第一行（紧跟 `//!` 文档注释）写死这行，**T12 删除**：
```rust
// bin crate 里 `pub` 不消除 dead_code，而 T2–T9 的 pub fn 要到 T11/T12 才接上调用方；
// 这一行在 Task 12 收口（CLI + 菜单 + /api/health 都接上）时删除。
#![allow(dead_code)]
```

`crates/bui/src/serve.rs` 的 `modules()`：把 P1 留的注释行换成真实注册（P1 文件，一行）：
```rust
            Arc::new(WatchdogModule),
            // P2 在此追加 UsersModule（面板 API + 采样 + auth 快照）
            Arc::new(crate::modules::residential::ResidentialModule::new()),
```

`crates/bui/src/serve.rs` 的 **P1 已合并测试**（`crates/bui/src/serve.rs:1017-1023`）：上面那行注册一加，这条断言必红——它是全等断言，不是 `contains`。按**裁决 D14**（「改为『包含 P1 六个模块名』的子集断言，P2 加自己的模块名，P3 加 `residential`，不再精确计数」）改成子集断言（契约决策 §H 的第三处、也是全计划最后一处允许改的 P1 测试断言）：

```rust
    // 改前
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            vec!["core-files", "units", "system", "ssh", "certs", "watchdog"]
        );

    // 改后（函数名与函数体其余部分一个字不动：manifest 句柄那段与 reg.modules[0].render 照旧）
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        // 裁决 D14：子集断言，不再精确计数——P2 在这张表里加自己的模块名、P3 加
        // "residential"，两条车道各追加一行字符串，`git` 冲突机械可解。注册顺序仍
        // 由 modules() 自己保证：residential 的 render 返回空，同路径 artifact 的
        // 覆盖顺序不受它影响（见契约决策 §B）
        for want in [
            "core-files", "units", "system", "ssh", "certs", "watchdog", "residential",
        ] {
            assert!(names.contains(&want), "模块 {want} 必须注册，实际 {names:?}");
        }
```

**为什么函数名不改**：P2 的 T13 会在同一条断言里追加 `"panel"`（P2 计划 §D14 明确写「函数名不改」）。两条车道若各自改名，这一处就从「机械可解的追加冲突」变成「同一函数两个名字」的真冲突。名字里的 “exactly six” 由上面那条注释就地说明已改为子集断言，不再由函数名承担。

`crates/bui/Cargo.toml` 的 `[dependencies]`（P1 文件，只在段内追加/改一行）：
```toml
base64.workspace = true
reqwest = { version = "0.12", default-features = false, features = ["blocking", "rustls-tls", "socks", "json"] }
```
`reqwest` 那行是把 P1 已有的同名行补上 `"socks"`（经 SOCKS5 上游发 HTTPS 要它）；`base64` 用于 CONNECT 的 `Proxy-Authorization: Basic`。

- [ ] **Step 4: 实现常量、`fanout` 与 `Module`**

`mod.rs`：
```rust
//! 住宅出口模块（spec §5）：上游池、出口体检、健康巡检与热切换、自动黑名单、面板端点与 CLI。
//!
//! 边界（契约决策 §B）：本模块**不渲染任何内核配置**。`singbox-relay.json` 由
//! `crate::modules::core_files`（P1 Task 10）从 `state.residential.groups["default"]`
//! 渲染；P3 只改 `state`，由 P1 的对账器完成重渲染、`sing-box check` 与 relay 重启。
use crate::reconcile::{Artifact, Module, RenderCtx};
use bui_schema::model::{State, DEFAULT_GROUP};
use std::sync::Arc;

pub mod api;
pub mod blacklist;
pub mod check;
pub mod clash;
pub mod cli;
pub mod health;
pub mod journal;
pub mod proxy;
pub mod state;
pub mod upstream;

pub const RUNTIME_KEY: &str = "residential";
pub const GROUP_DEFAULT: &str = DEFAULT_GROUP;
// …（Interfaces 里列出的其余常量逐条照写，注释一并带上）

pub struct ResidentialModule;

impl ResidentialModule {
    pub fn new() -> Self {
        Self
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
}

/// 测试夹具：住宅段的期望态样例（合成值，无生产秘密）。字段取值与 `bui-schema`
/// `model.rs` 的 SAMPLE 一致，便于两边的断言互相印证。
#[cfg(test)]
pub fn sample_group() -> bui_schema::model::ResidentialGroup {
    use bui_schema::model::{
        AutoEntry, Blacklist, Pin, ResiMode, ResidentialGroup, Rule, Upstream, UpstreamKind, Verified,
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

#[cfg(test)]
pub fn sample_state_with_pool() -> State {
    let mut s = crate::testutil::sample_state();
    s.residential
        .groups
        .insert(GROUP_DEFAULT.to_string(), sample_group());
    s
}

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
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential && cargo test -p bui serve:: && cargo clippy -p bui --all-targets -- -D warnings && cargo fmt --check`
Expected: `modules::residential` 5 passed；`serve::` 全绿——注意 `p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 的断言是本步按 D14 **改过**的（改前它必红：全等断言里 `names` 会多出 `"residential"`）。其余 `serve::` 测试不受影响：新模块的 `render` 返回空，对账结果一字不变。clippy 零告警（模块级 `#![allow(dead_code)]` 已在位）。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential crates/bui/src/modules/mod.rs crates/bui/src/serve.rs crates/bui/Cargo.toml Cargo.lock
git commit -m "feat(resi): 住宅模块骨架（11 桩文件、全模块常量、fanout 并发助手、渲染边界为空）"
```

---

### Task 2: `ResiRuntime`（运行时数据）、健康迟滞与 24h 成功率、状态读写单一入口

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/state.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{RUNTIME_KEY, GROUP_DEFAULT, FAIL_TO_UNHEALTHY, OK_TO_HEALTHY, RATE_WINDOW_SECS, RATE_SAMPLES_MAX}` 与 `#[cfg(test)] sample_state_with_pool`；`crate::state::runtime::{Runtime, RuntimeData}`；`crate::state::store::Store`；`crate::api::{Event, EventBus}`；`crate::util::{fmt_rfc3339, parse_rfc3339}`；`bui_schema::model::{ResidentialGroup, State}`
- Produces:
```rust
// crate::modules::residential::state
/// `runtime.json` 的 `extra["residential"]`（spec §2.1：健康 streak、黑名单候选计数、
/// 上次选中的上游、采样游标都放 runtime.json，丢了也能从零重建）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResiRuntime {
    /// key = **上游 uuid 的字符串形式**（`Uuid::to_string()`），不是 `resi-N`：tag 是位置键，
    /// 增删上游会 renumber，用它当主键会让 streak 与成功率错位（契约决策 §C）
    pub health: BTreeMap<String, HealthState>,
    /// 当前实际生效的上游（Clash API 真源快照；tag 由 `clash::tag_of` 现算，契约决策 §C）
    pub selected_upstream_id: Option<Uuid>,
    pub last_switch_at: Option<String>,
    /// `state.selected_upstream_id` 与 `selected_upstream_id` 已漂移，等下一个 04:00 窗口写回
    pub selected_pending_persist: bool,
    /// 面板「立即巡检一轮」按钮的限速游标（T10 的 `POST /api/residential/health/check`）
    pub last_manual_round_at: Option<String>,
    /// 黑名单候选，key = `candidate_key(upstream_id, host, port)`
    pub candidates: BTreeMap<String, Candidate>,
    /// **已确认完毕**（`confirms >= CONFIRM_NEEDED`）、等 04:00 批量写进
    /// `state.blacklist.auto` 的条目。进了这里就不再需要判 `confirms`
    pub pending: Vec<PendingEntry>,
    pub journal_cursor: Option<String>,
    /// key = upstream_id 的字符串形式，值 = `check::CheckReport` 的 JSON（避免 state.rs 反向依赖 check.rs）
    pub checks: BTreeMap<String, serde_json::Value>,
    /// 正在跑的体检（面板据此显示「检测中…」，对应 R13 §4 的 `checking`）
    pub checking: Option<Checking>,
    /// 最近告警（全不健康、凭据失效、journalctl 缺失…），上限 `ALERTS_MAX`，新的在前
    pub alerts: Vec<String>,
    pub last_daily_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthState { pub active: bool, pub okstreak: u32, pub failstreak: u32, pub samples: Vec<ProbeSample> }
impl Default for HealthState { /* active = true，其余 0 */ }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeSample { pub at: String, pub ok: bool }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate { pub upstream_id: Uuid, pub host: String, pub port: u16, pub hits: u64,
                       pub first_seen: String, pub last_seen: String,
                       /// 确认进度记在候选上（spec §5.4「间隔 ≥10 分钟连续 2 次」）：
                       /// 只有攒到 `CONFIRM_NEEDED` 才升进 `pending`
                       pub confirms: u32, pub last_confirm_at: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingEntry { pub upstream_id: Uuid, pub host: String, pub port: u16, pub hits: u64,
                          pub confirms: u32, pub last_confirm_at: String }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checking { pub upstream_id: Uuid, pub started_at: String }

pub const ALERTS_MAX: usize = 20;

pub fn from_runtime(rt: &RuntimeData) -> ResiRuntime;                       // 解析失败 → Default + warn
pub async fn read(runtime: &Runtime) -> ResiRuntime;
pub async fn update(runtime: &Runtime, f: impl FnOnce(&mut ResiRuntime)) -> ResiRuntime;
pub fn push_alert(r: &mut ResiRuntime, msg: impl Into<String>);             // 去重 + 截断到 ALERTS_MAX

/// 读当前住宅分组（不存在时给 `Default`，等价于「池未启用」→ relay fail-open 直连）
pub fn group_of(s: &State) -> ResidentialGroup;
/// **改住宅状态的唯一入口**：写 state → 发 `Event::StateChanged("residential")`
/// → P1 的 500ms 去抖对账重渲染 relay（契约决策 §B）
pub async fn update_group(store: &Store, bus: &EventBus,
                          f: impl FnOnce(&mut ResidentialGroup)) -> anyhow::Result<()>;

/// 迟滞状态机（spec §5.3）：返回本轮之后该成员是否算健康
pub fn apply_hysteresis(h: &mut HealthState, ok: bool) -> bool;
/// 记一条样本并裁掉窗口外/超量的（`now` 由 `Host::now()` 给，测试可推进）
pub fn record_probe(h: &mut HealthState, ok: bool, now: OffsetDateTime);
/// 近 24h 成功率；无样本返回 0.0（「没数据」不该赢过「有数据且全成功」）
pub fn success_rate_24h(h: &HealthState, now: OffsetDateTime) -> f64;
/// `/api/health` 的 `residential` 字段（P1 Task 13 的 `HealthResponse.residential`）
pub fn health_summary(g: &ResidentialGroup, r: &ResiRuntime) -> serde_json::Value;
```
`ResiRuntime` 整体作为 `RuntimeData.extra["residential"]` 的一个 JSON 对象存取——P1 Task 2 的 `extra` 是 `#[serde(flatten)] BTreeMap<String, Value>`，所以 P3 不必回头改 P1 的文件（那份计划的字段注释就是这么写的）。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/residential/state.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Event, EventBus};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-12 00:00:00 UTC)
    }

    #[test]
    fn hysteresis_needs_two_rounds_in_each_direction() {
        let mut h = HealthState::default();
        assert!(h.active, "新成员默认健康（没探过不等于坏）");
        assert!(apply_hysteresis(&mut h, false), "第 1 轮失败还不剔除");
        assert_eq!((h.failstreak, h.okstreak), (1, 0));
        assert!(!apply_hysteresis(&mut h, false), "第 2 轮失败才剔除");
        assert!(!h.active);
        assert!(!apply_hysteresis(&mut h, true), "第 1 轮成功还不恢复");
        assert!(apply_hysteresis(&mut h, true), "第 2 轮成功才恢复");
        assert_eq!((h.failstreak, h.okstreak), (0, 2));
    }

    #[test]
    fn success_rate_only_counts_the_last_24h_and_caps_samples() {
        let mut h = HealthState::default();
        record_probe(&mut h, false, t0());
        record_probe(&mut h, true, t0() + time::Duration::hours(1));
        assert_eq!(success_rate_24h(&h, t0() + time::Duration::hours(2)), 0.5);
        // 25 小时后那条失败样本已出窗，且在写入时就被裁掉
        record_probe(&mut h, true, t0() + time::Duration::hours(25));
        assert_eq!(h.samples.len(), 2, "窗口外的样本在写入时裁掉");
        assert_eq!(success_rate_24h(&h, t0() + time::Duration::hours(25)), 1.0);
        assert_eq!(
            success_rate_24h(&HealthState::default(), t0()),
            0.0,
            "无样本记 0：不能让没探过的成员赢过全成功的成员"
        );
        for i in 0..(RATE_SAMPLES_MAX + 50) {
            record_probe(
                &mut h,
                true,
                t0() + time::Duration::hours(25) + time::Duration::seconds(i as i64),
            );
        }
        assert_eq!(h.samples.len(), RATE_SAMPLES_MAX, "样本数有上限，runtime.json 不会无限长");
    }

    #[tokio::test]
    async fn runtime_round_trips_through_the_extra_map() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let id = Uuid::from_u128(2);
        update(&runtime, |r| {
            r.selected_upstream_id = Some(id);
            r.health.insert(id.to_string(), HealthState::default());
        })
        .await;
        // extra 是 flatten，所以落盘后顶层就有一个 "residential" 键
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.path().join("runtime.json")).unwrap()).unwrap();
        assert_eq!(raw["residential"]["selected_upstream_id"], id.to_string());
        assert!(raw["residential"]["health"].get(id.to_string()).is_some(), "health 以 uuid 为键");
        assert_eq!(read(&runtime).await.selected_upstream_id, Some(id));
        assert!(raw.get("restart_keys").is_some(), "P1 自己的字段不被本模块碰掉");
    }

    #[tokio::test]
    async fn alerts_dedupe_and_cap() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let r = update(&runtime, |r| {
            push_alert(r, "全部上游探测不达标，保持当前出口");
            push_alert(r, "全部上游探测不达标，保持当前出口");
            for i in 0..30 {
                push_alert(r, format!("告警 {i}"));
            }
        })
        .await;
        assert_eq!(r.alerts.len(), ALERTS_MAX);
        assert_eq!(r.alerts[0], "告警 29", "最新的在最前");
    }

    #[tokio::test]
    async fn update_group_writes_state_and_fires_state_changed() {
        let d = tempfile::tempdir().unwrap();
        // sample_state() 的住宅段是 mode=split，所以这里改成 Global 才真的产生一次写盘
        // （`Store::update` 零变更不写盘，改成同值等于什么都没测）
        let store = Store::create(d.path().join("state.json"), sample_state()).await.unwrap();
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        update_group(&store, &bus, |g| g.mode = bui_schema::model::ResiMode::Global)
            .await
            .unwrap();
        assert_eq!(
            store.read().await.residential.groups[GROUP_DEFAULT].mode,
            bui_schema::model::ResiMode::Global
        );
        // 从磁盘读回，确认真的落盘了（不是只改了内存缓存）
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.path().join("state.json")).unwrap()).unwrap();
        assert_eq!(raw["residential"]["groups"]["default"]["mode"], "global");
        assert_eq!(rx.try_recv().unwrap(), Event::StateChanged("residential"));
    }

    #[test]
    fn health_summary_is_json_the_panel_can_read() {
        // 用本模块自己的夹具：P1 的 sample_state() 住宅段是空池（upstreams/pins 都是 0）
        let s = crate::modules::residential::sample_state_with_pool();
        let g = group_of(&s);
        let id = g.upstreams[0].id;
        let r = ResiRuntime {
            selected_upstream_id: Some(id),
            alerts: vec!["x".into()],
            ..Default::default()
        };
        let v = health_summary(&g, &r);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["upstreams"], 1);
        assert_eq!(v["selected_upstream_id"], id.to_string());
        assert_eq!(v["unhealthy"], 0, "没探过的成员默认健康");
        assert_eq!(v["blacklist"]["pins"], 1);
        assert_eq!(v["blacklist"]["auto"], 1);
        assert_eq!(v["alerts"][0], "x");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::state`
Expected: 编译失败，`cannot find type `HealthState``。

- [ ] **Step 3: 实现类型与运行时读写**

```rust
//! 住宅模块的运行时数据（`runtime.json` 的 `extra["residential"]`）与状态读写助手。
use super::{FAIL_TO_UNHEALTHY, GROUP_DEFAULT, OK_TO_HEALTHY, RATE_SAMPLES_MAX, RATE_WINDOW_SECS, RUNTIME_KEY};
use crate::api::{Event, EventBus};
use crate::state::runtime::{Runtime, RuntimeData};
use crate::state::store::Store;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::OffsetDateTime;
use uuid::Uuid;

pub const ALERTS_MAX: usize = 20;

// …（Interfaces 里的结构体逐条照写，字段一律 #[serde(default)]）

impl Default for HealthState {
    fn default() -> Self {
        // 新成员默认健康：没探过 ≠ 坏。与 v3 `resi-health.sh` 的 `.active // true` 同义。
        Self { active: true, okstreak: 0, failstreak: 0, samples: Vec::new() }
    }
}

pub fn from_runtime(rt: &RuntimeData) -> ResiRuntime {
    match rt.extra.get(RUNTIME_KEY) {
        None => ResiRuntime::default(),
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "runtime.json 的 residential 段解析失败，按空值重建");
            ResiRuntime::default()
        }),
    }
}

pub async fn read(runtime: &Runtime) -> ResiRuntime {
    from_runtime(&runtime.read().await)
}

pub async fn update(runtime: &Runtime, f: impl FnOnce(&mut ResiRuntime)) -> ResiRuntime {
    let mut out = ResiRuntime::default();
    runtime
        .update(|rt| {
            let mut r = from_runtime(rt);
            f(&mut r);
            // to_value 只会在自定义 Serialize 里失败，这里全是 derive
            if let Ok(v) = serde_json::to_value(&r) {
                rt.extra.insert(RUNTIME_KEY.to_string(), v);
            }
            out = r;
        })
        .await;
    out
}

pub fn push_alert(r: &mut ResiRuntime, msg: impl Into<String>) {
    let msg = msg.into();
    r.alerts.retain(|a| a != &msg);
    r.alerts.insert(0, msg);
    r.alerts.truncate(ALERTS_MAX);
}

pub fn group_of(s: &State) -> ResidentialGroup {
    s.residential.groups.get(GROUP_DEFAULT).cloned().unwrap_or_default()
}

pub async fn update_group(
    store: &Store,
    bus: &EventBus,
    f: impl FnOnce(&mut ResidentialGroup),
) -> anyhow::Result<()> {
    store
        .update(|s| {
            let g = s.residential.groups.entry(GROUP_DEFAULT.to_string()).or_default();
            f(g);
        })
        .await?;
    // 对账由 P1 的去抖消费者跑：重渲染 singbox-relay.json → sing-box check → 重启 b-ui-relay
    bus.send(Event::StateChanged("residential"));
    Ok(())
}
```

- [ ] **Step 4: 实现迟滞、24h 成功率与摘要**

```rust
pub fn apply_hysteresis(h: &mut HealthState, ok: bool) -> bool {
    if ok {
        h.okstreak += 1;
        h.failstreak = 0;
        if !h.active && h.okstreak >= OK_TO_HEALTHY {
            h.active = true;
        }
    } else {
        h.failstreak += 1;
        h.okstreak = 0;
        if h.active && h.failstreak >= FAIL_TO_UNHEALTHY {
            h.active = false;
        }
    }
    h.active
}

pub fn record_probe(h: &mut HealthState, ok: bool, now: OffsetDateTime) {
    h.samples.push(ProbeSample { at: fmt_rfc3339(now), ok });
    let cutoff = now - time::Duration::seconds(RATE_WINDOW_SECS);
    // 解析不出时间戳的旧样本一并丢掉（换过格式或文件被手改）
    h.samples.retain(|s| parse_rfc3339(&s.at).map(|t| t >= cutoff).unwrap_or(false));
    if h.samples.len() > RATE_SAMPLES_MAX {
        let drop = h.samples.len() - RATE_SAMPLES_MAX;
        h.samples.drain(..drop);
    }
}

pub fn success_rate_24h(h: &HealthState, now: OffsetDateTime) -> f64 {
    let cutoff = now - time::Duration::seconds(RATE_WINDOW_SECS);
    let (mut total, mut ok) = (0u32, 0u32);
    for s in h
        .samples
        .iter()
        .filter(|s| parse_rfc3339(&s.at).map(|t| t >= cutoff).unwrap_or(false))
    {
        total += 1;
        if s.ok {
            ok += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    f64::from(ok) / f64::from(total)
}

pub fn health_summary(g: &ResidentialGroup, r: &ResiRuntime) -> serde_json::Value {
    serde_json::json!({
        "enabled": g.pool_active(),
        "mode": g.mode,
        "upstreams": g.upstreams.len(),
        "selected_upstream_id": r.selected_upstream_id,
        "selected_pending_persist": r.selected_pending_persist,
        "unhealthy": r.health.values().filter(|h| !h.active).count(),
        "blacklist": { "pins": g.blacklist.pins.len(), "auto": g.blacklist.auto.len(),
                       "pending": r.pending.len(), "candidates": r.candidates.len() },
        "checking": r.checking,
        "last_daily_at": r.last_daily_at,
        "alerts": r.alerts,
    })
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::state && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 6 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/state.rs
git commit -m "feat(resi): 运行时数据、健康迟滞与 24h 成功率、update_group 单一写入口"
```

---

### Task 3: `Prober`（经上游的 CONNECT / SOCKS5 / HTTP / UDP ASSOCIATE）与直连对照

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/proxy.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::PROBE_TIMEOUT_SECS`；`crate::redact`；`bui_schema::model::{Upstream, UpstreamKind}`
- Produces:
```rust
// crate::modules::residential::proxy
/// 一次「经该上游对某个 host:port 开隧道」的结果。这四个变体是黑名单判定与体检的全部依据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectVerdict {
    /// 隧道建立：HTTP 上游 CONNECT 2xx，或 SOCKS5 REP = 0x00
    Open,
    /// **硬拒**：HTTP 上游 CONNECT 4xx/5xx（不含 407），或 SOCKS5 REP ≠ 0x00。
    /// `code` = HTTP 状态码，或 SOCKS5 的 REP 值（0x02 = not allowed by ruleset）
    Refused { code: u16 },
    /// 凭据失效：CONNECT 407，或 SOCKS5 用户名密码认证被拒（RFC1929 STATUS ≠ 0）。
    /// 调研 §D 的 407 场景：**整条上游不可用**，不是「这个目标被拒」
    AuthFailed,
    /// 连不上上游本身 / 超时 / 协议对不上
    Unreachable { detail: String },
}
impl ConnectVerdict {
    pub fn is_hard_reject(&self) -> bool;    // 只有 Refused 为 true
    pub fn label(&self) -> String;           // "open" / "refused:403" / "auth_failed" / "unreachable"
}
/// 经上游发一次 HTTPS GET 的结果（不跟随重定向，只要状态码与前 64KB 正文）
#[derive(Debug, Clone, PartialEq)]
pub struct HttpProbe { pub status: u16, pub body: String }
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("上游凭据失效（CONNECT 407 / SOCKS5 认证被拒）")]
    AuthFailed,
    #[error("上游不可用：{0}")]
    Unreachable(String),
}

/// 所有碰网络的操作都走这里；**同步** trait，调用点一律在 `tokio::task::spawn_blocking`
/// 里（`reqwest::blocking` 在 async 上下文会 panic，与 P1 的 `Fetcher` 同一条铁律）。
pub trait Prober: Send + Sync + 'static {
    /// 经上游对 `host:port` 开一次隧道（HTTP → CONNECT；SOCKS5 → CMD=CONNECT、ATYP=域名）
    fn connect(&self, up: &Upstream, host: &str, port: u16) -> ConnectVerdict;
    /// 经上游发一次 HTTPS GET
    fn get(&self, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError>;
    /// 经上游做一次 SOCKS5 UDP ASSOCIATE；HTTP 上游一律 `Ok(false)`（协议里没这回事）
    fn udp_associate(&self, up: &Upstream) -> Result<bool, ProbeError>;
    /// **不经上游**的直连 TCP 对照（黑名单确认的第二条件：直连同目标可达）
    fn direct_tcp(&self, host: &str, port: u16) -> bool;
}

pub struct ReqwestProber { timeout: std::time::Duration }
impl ReqwestProber { pub fn new() -> Self; pub fn with_timeout(secs: u64) -> Self; }
impl Default for ReqwestProber { fn default() -> Self { Self::new() } }
/// `reqwest::Proxy` 的 URL（**不含凭据**，凭据走 `Proxy::basic_auth`）
pub fn proxy_url(up: &Upstream) -> String;                 // "http://h:port" / "socks5h://h:port"
/// CONNECT 请求首部（`Proxy-Authorization: Basic` 用 base64）
pub fn connect_request(up: &Upstream, host: &str, port: u16) -> Vec<u8>;
/// CONNECT 响应首行 → 判定（`HTTP/1.1 403 Forbidden serp domain` → `Refused{403}`）
pub fn parse_connect_status(first_line: &str) -> ConnectVerdict;
/// SOCKS5 CONNECT 回复的 REP 字节 → 判定
pub fn socks_verdict(rep: u8) -> ConnectVerdict;
/// 目标主机名合法性（`\r\n` / 引号 / 空格 / 空串一律拒，绝不让它进请求行）
pub fn valid_target_host(host: &str) -> bool;
/// 「这条 `reqwest` 错误链的文字里有没有代理鉴权失败的迹象」（纯函数，喂整条
/// `source` 链拼出来的字符串）：命中 `407` / `proxy authentication`（大小写不敏感）
/// 即为真。`ReqwestProber::get` 用它做**第一层**判定
pub fn looks_like_proxy_auth(msg: &str) -> bool;
/// `get` 失败后的**补判**：经该上游对 `host:443` 自写一次 CONNECT / SOCKS5 握手，
/// 只为把「凭据失效」从「不可达」里分出来。返回 `true` = 上游拒绝鉴权。
///
/// **为什么必须补判**：所有探测 URL 都是 `https://`，经 HTTP 上游走的是 CONNECT 隧道，
/// 407 出现在**隧道建立阶段**——`reqwest` 把它作为 `Err(reqwest::Error)` 返回，
/// 调用方永远拿不到一个「状态码 = 407」的 `Response`（`get` 里 `status == 407` 那条
/// 分支只在「明文 HTTP 目标」时才可达，而本模块没有这种目标）。若只看 `get` 的返回值，
/// 生产上凭据失效会一律报成 `Unreachable`：巡检仍判不健康（spec §5.4 的「407 视为不健康」
/// 成立），但「凭据失效，请更新凭据」这条告警**永远不会出现**，运维只能看到「上游挂了」。
/// `connect()` 是本模块唯一能读到 CONNECT 状态码与 SOCKS5 认证 STATUS 的路径，所以补判走它。
pub fn confirm_auth_failure(p: &dyn Prober, up: &Upstream, host: &str) -> bool;
#[cfg(test)] pub struct FakeProber { /* Mutex<FakeProberInner> */ }
#[cfg(test)]
#[derive(Default)]
pub struct FakeProberInner {
    /// key = `"<host>:<port>"`，缺省 `Open`
    pub connects: BTreeMap<String, ConnectVerdict>,
    /// key = URL，缺省 `Err(Unreachable("no route"))`。**错误哨兵约定**（跨任务契约，
    /// T6/T7/T8 的测试都依赖它）：值为 `Err("__auth_failed__")` 时 `get` 返回
    /// `ProbeError::AuthFailed`，其余字符串返回 `ProbeError::Unreachable(那个字符串)`
    pub gets: BTreeMap<String, Result<HttpProbe, String>>,
    pub udp: bool,
    /// 直连可达的 `"<host>:<port>"`；不在集合里即不可达
    pub direct: BTreeSet<String>,
    pub calls: Vec<String>,
}
#[cfg(test)]
impl FakeProber {
    pub fn new() -> Self;
    pub fn with(&self, f: impl FnOnce(&mut FakeProberInner)) -> &Self;
    pub fn calls(&self) -> Vec<String>;
    pub fn clear_calls(&self);
}
```
`FakeProber` 的 `calls` 字符串格式（后续任务的断言依赖它，不得更改）：`connect:<host>:<port>`、`get:<url>`、`udp`、`direct:<host>:<port>`。

**为什么自己写握手、不 exec curl、也不只用 reqwest**：`reqwest` 不暴露 CONNECT 的响应状态码（失败只给一个 `reqwest::Error`），而 CONNECT 的 4xx/5xx 与 407 恰恰是「目标被硬拒」与「凭据失效」的**唯一**区分依据（调研 §D）。所以 `connect()` / `udp_associate()` 用 `std::net::TcpStream` 自己发 CONNECT / SOCKS5 握手；`get()` 不需要这层细节，用 `reqwest::blocking` + `Proxy::basic_auth`。Global Constraints 禁止 exec curl（v3 三处 `curl -K -` 构造是审计 resi-C5/C6 点名要合并的重复）。

**这同一件事也决定了 `get()` 怎么判 407**：本模块的探测 URL 全是 `https://`，经 HTTP 上游一律走 CONNECT 隧道，407 发生在隧道建立阶段 ⇒ `reqwest` 返回的是 `Err`，**不会**给出一个状态码为 407 的 `Response`。所以 `get()` 里 `status == 407` 那条分支在本模块的生产路径上不可达（留着只为兜住「明文 HTTP 目标」的将来用法），真正的判定靠两层：① `looks_like_proxy_auth` 扫 `reqwest::Error` 的整条 `source` 链文字；② 调用方（T7 `detect_kind`、T8 `probe_member`）在 `get` 失败后用 `confirm_auth_failure` 经 `connect()` 补判一次 CONNECT 状态码 / SOCKS5 认证 STATUS。两层都不命中才算 `Unreachable`。

- [ ] **Step 1: 写失败测试（纯判定 + FakeProber 契约）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    fn up(kind: UpstreamKind) -> Upstream {
        Upstream {
            id: Uuid::nil(), name: "url-1".into(), kind,
            host: "isp.example.net".into(), port: 10007,
            username: "user1".into(), password: "pw1".into(),
            priority: 10, provider: None, region: None, ports_allowed: None, verified: None,
        }
    }

    #[test]
    fn connect_status_line_maps_to_the_four_verdicts() {
        assert_eq!(parse_connect_status("HTTP/1.1 200 Connection established"), ConnectVerdict::Open);
        // 调研 §D 实测形态：Decodo 的硬拒就是 CONNECT 403
        assert_eq!(
            parse_connect_status("HTTP/1.1 403 Forbidden serp domain"),
            ConnectVerdict::Refused { code: 403 }
        );
        assert_eq!(parse_connect_status("HTTP/1.1 502 Bad Gateway"), ConnectVerdict::Refused { code: 502 });
        // 407 是凭据失效，不是目标被拒：不能进黑名单，要把整条上游判不健康
        assert_eq!(
            parse_connect_status("HTTP/1.1 407 Proxy Authentication Required"),
            ConnectVerdict::AuthFailed
        );
        assert!(matches!(parse_connect_status("garbage"), ConnectVerdict::Unreachable { .. }));
        assert!(matches!(parse_connect_status(""), ConnectVerdict::Unreachable { .. }));
    }

    #[test]
    fn socks5_reply_codes_map_to_the_four_verdicts() {
        assert_eq!(socks_verdict(0x00), ConnectVerdict::Open);
        assert_eq!(socks_verdict(0x02), ConnectVerdict::Refused { code: 2 }, "not allowed by ruleset");
        assert_eq!(socks_verdict(0x05), ConnectVerdict::Refused { code: 5 }, "connection refused");
        assert!(ConnectVerdict::Refused { code: 2 }.is_hard_reject());
        assert!(!ConnectVerdict::AuthFailed.is_hard_reject(), "凭据失效不是目标被拒");
        assert!(!ConnectVerdict::Unreachable { detail: "x".into() }.is_hard_reject());
        assert_eq!(ConnectVerdict::Refused { code: 403 }.label(), "refused:403");
        assert_eq!(ConnectVerdict::AuthFailed.label(), "auth_failed");
    }

    #[test]
    fn proxy_url_never_carries_credentials() {
        assert_eq!(proxy_url(&up(UpstreamKind::Http)), "http://isp.example.net:10007");
        // socks5h = 远端解析域名（目标域名原样交上游，v3.6.0 R10 的结论）
        assert_eq!(proxy_url(&up(UpstreamKind::Socks5)), "socks5h://isp.example.net:10007");
        assert!(!proxy_url(&up(UpstreamKind::Http)).contains("pw1"));
    }

    #[test]
    fn connect_request_has_basic_auth_and_a_clean_request_line() {
        let req = String::from_utf8(connect_request(&up(UpstreamKind::Http), "pay.google.com", 443)).unwrap();
        assert!(req.starts_with("CONNECT pay.google.com:443 HTTP/1.1\r\n"), "实际 {req:?}");
        assert!(req.contains("Host: pay.google.com:443\r\n"));
        // base64("user1:pw1")
        assert!(req.contains("Proxy-Authorization: Basic dXNlcjE6cHcx\r\n"), "实际 {req:?}");
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn target_host_validation_blocks_request_line_injection() {
        assert!(valid_target_host("gateway.icloud.com"));
        assert!(valid_target_host("198.51.100.7"));
        assert!(!valid_target_host(""));
        assert!(!valid_target_host("a.com\r\nX-Evil: 1"));
        assert!(!valid_target_host("a.com b.com"));
        assert!(!valid_target_host("a\"b.com"));
    }

    #[test]
    fn fake_prober_defaults_are_open_and_calls_are_recorded() {
        let p = FakeProber::new();
        p.with(|i| {
            i.connects.insert("pay.google.com:443".into(), ConnectVerdict::Refused { code: 403 });
            i.direct.insert("pay.google.com:443".into());
        });
        assert_eq!(p.connect(&up(UpstreamKind::Http), "www.google.com", 443), ConnectVerdict::Open);
        assert_eq!(
            p.connect(&up(UpstreamKind::Http), "pay.google.com", 443),
            ConnectVerdict::Refused { code: 403 }
        );
        assert!(p.direct_tcp("pay.google.com", 443));
        assert!(!p.direct_tcp("www.google.com", 443));
        assert_eq!(
            p.calls(),
            vec![
                "connect:www.google.com:443",
                "connect:pay.google.com:443",
                "direct:pay.google.com:443",
                "direct:www.google.com:443",
            ]
        );
        // 错误哨兵约定（跨任务契约）：T7 的 `add_rejects_a_407…` 与 T8 的 407 用例都吃它
        p.with(|i| {
            i.gets.insert("https://auth.example.com".into(), Err("__auth_failed__".into()));
            i.gets.insert("https://boom.example.com".into(), Err("boom".into()));
        });
        assert!(matches!(
            p.get(&up(UpstreamKind::Http), "https://auth.example.com"),
            Err(ProbeError::AuthFailed)
        ));
        assert!(matches!(
            p.get(&up(UpstreamKind::Http), "https://boom.example.com"),
            Err(ProbeError::Unreachable(ref s)) if s == "boom"
        ));
        assert!(
            matches!(
                p.get(&up(UpstreamKind::Http), "https://unmapped.example.com"),
                Err(ProbeError::Unreachable(_))
            ),
            "未登记的 URL 缺省不可达"
        );
        assert!(!p.udp_associate(&up(UpstreamKind::Socks5)).unwrap(), "inner.udp 默认 false");
    }

    #[test]
    fn a_407_is_recognised_from_the_error_text_and_confirmed_by_a_connect_probe() {
        // 第一层：reqwest 在 CONNECT 阶段的 407 只留下文字（没有 Response 可读状态码）
        assert!(looks_like_proxy_auth("error following redirect: HTTP/1.1 407 Proxy Authentication Required"));
        assert!(looks_like_proxy_auth("tunnel failed: Proxy Authentication Required"));
        assert!(looks_like_proxy_auth("PROXY AUTHENTICATION required"), "大小写不敏感");
        assert!(!looks_like_proxy_auth("dns error: failed to lookup address"));
        assert!(!looks_like_proxy_auth("connection refused"));
        // 第二层：文字里没有线索时，经上游对 host:443 自写一次 CONNECT 补判
        let p = FakeProber::new();
        p.with(|i| {
            i.connects.insert("www.gstatic.com:443".into(), ConnectVerdict::AuthFailed);
            i.connects.insert("api.ipify.org:443".into(), ConnectVerdict::Refused { code: 403 });
        });
        let u = up(UpstreamKind::Http);
        assert!(confirm_auth_failure(&p, &u, "www.gstatic.com"), "CONNECT 407 ⇒ 凭据失效");
        assert!(!confirm_auth_failure(&p, &u, "api.ipify.org"), "403 是目标被硬拒，不是凭据问题");
        assert!(!confirm_auth_failure(&p, &u, "open.example.com"), "缺省 Open ⇒ 不是凭据问题");
        assert_eq!(
            p.calls(),
            vec!["connect:www.gstatic.com:443", "connect:api.ipify.org:443", "connect:open.example.com:443"],
            "补判必须走 443：探测 URL 全是 https，换端口会撞上游的端口白名单"
        );
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::proxy`
Expected: 编译失败，`cannot find function `parse_connect_status``。

- [ ] **Step 3: 实现纯判定函数与请求构造**

```rust
impl ConnectVerdict {
    pub fn is_hard_reject(&self) -> bool {
        matches!(self, ConnectVerdict::Refused { .. })
    }

    pub fn label(&self) -> String {
        match self {
            ConnectVerdict::Open => "open".into(),
            ConnectVerdict::Refused { code } => format!("refused:{code}"),
            ConnectVerdict::AuthFailed => "auth_failed".into(),
            ConnectVerdict::Unreachable { .. } => "unreachable".into(),
        }
    }
}

pub fn valid_target_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']'))
}

pub fn proxy_url(up: &Upstream) -> String {
    let scheme = match up.kind {
        UpstreamKind::Http => "http",
        // socks5h = 远端解析域名（目标域名原样交上游，v3.6.0 R10 的结论）
        UpstreamKind::Socks5 => "socks5h",
    };
    format!("{scheme}://{}:{}", up.host, up.port)
}

pub fn connect_request(up: &Upstream, host: &str, port: u16) -> Vec<u8> {
    use base64::Engine as _;
    let cred = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", up.username, up.password));
    format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Proxy-Authorization: Basic {cred}\r\nProxy-Connection: keep-alive\r\n\r\n"
    )
    .into_bytes()
}

pub fn parse_connect_status(first_line: &str) -> ConnectVerdict {
    let mut parts = first_line.split_whitespace();
    let ver = parts.next().unwrap_or_default();
    let code: u16 = match parts.next().and_then(|c| c.parse().ok()) {
        Some(c) if ver.starts_with("HTTP/") => c,
        _ => {
            return ConnectVerdict::Unreachable { detail: "CONNECT 响应不是 HTTP 状态行".into() }
        }
    };
    match code {
        200..=299 => ConnectVerdict::Open,
        // 调研 §D：407 是凭据失效，整条上游不可用；绝不能当成「这个目标被拒」进黑名单
        407 => ConnectVerdict::AuthFailed,
        400..=599 => ConnectVerdict::Refused { code },
        _ => ConnectVerdict::Unreachable { detail: format!("CONNECT 返回 {code}") },
    }
}

pub fn socks_verdict(rep: u8) -> ConnectVerdict {
    match rep {
        0x00 => ConnectVerdict::Open,
        // 0x02 not allowed by ruleset / 0x05 connection refused / 0x03 network unreachable…
        // 一律算硬拒：这是「上游明确回绝了这个目标」，与 TCP 层连不上（Unreachable）不同
        r => ConnectVerdict::Refused { code: u16::from(r) },
    }
}

pub fn looks_like_proxy_auth(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    // reqwest / hyper 在 CONNECT 阶段拿到 407 时，错误链里带的是状态行或这句标准原因短语
    m.contains("407") || m.contains("proxy authentication")
}

pub fn confirm_auth_failure(p: &dyn Prober, up: &Upstream, host: &str) -> bool {
    // 443：探测 URL 全是 https，补判必须走同一个端口，否则上游的端口白名单
    // （调研 §D：只放行 80/443）会把补判本身判成硬拒，得出「不是凭据问题」的错结论
    matches!(p.connect(up, host, 443), ConnectVerdict::AuthFailed)
}
```

- [ ] **Step 4: 实现 `ReqwestProber`**

```rust
impl ReqwestProber {
    pub fn new() -> Self {
        Self::with_timeout(super::PROBE_TIMEOUT_SECS)
    }
    pub fn with_timeout(secs: u64) -> Self {
        Self { timeout: std::time::Duration::from_secs(secs) }
    }

    fn dial(&self, up: &Upstream) -> std::io::Result<std::net::TcpStream> {
        use std::net::ToSocketAddrs;
        let mut last = std::io::Error::other("上游地址解析为空");
        for a in (up.host.as_str(), up.port).to_socket_addrs()? {
            match std::net::TcpStream::connect_timeout(&a, self.timeout) {
                Ok(s) => {
                    s.set_read_timeout(Some(self.timeout))?;
                    s.set_write_timeout(Some(self.timeout))?;
                    return Ok(s);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// RFC7231 §4.3.6：CONNECT + `Proxy-Authorization`，只读响应首行
    fn http_connect(&self, up: &Upstream, host: &str, port: u16) -> std::io::Result<ConnectVerdict> {
        use std::io::{BufRead, BufReader, Write};
        let mut s = self.dial(up)?;
        s.write_all(&connect_request(up, host, port))?;
        s.flush()?;
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line)?;
        Ok(parse_connect_status(line.trim_end()))
    }

    /// RFC1928 + RFC1929：greeting(05 01 02) → 用户名密码认证 → CMD=01、ATYP=03（域名）
    fn socks5_connect(&self, up: &Upstream, host: &str, port: u16) -> std::io::Result<ConnectVerdict> {
        let mut s = self.dial(up)?;
        match socks5_handshake(&mut s, up)? {
            ConnectVerdict::Open => {}
            other => return Ok(other),
        }
        let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        req.extend_from_slice(host.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        socks5_request(&mut s, &req)
    }

    /// RFC1928 §4：CMD=03（UDP ASSOCIATE），绑定地址 0.0.0.0:0
    fn socks5_udp(&self, up: &Upstream) -> Result<bool, ProbeError> {
        let mut s = self
            .dial(up)
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        let v = socks5_handshake(&mut s, up).map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        if v == ConnectVerdict::AuthFailed {
            return Err(ProbeError::AuthFailed);
        }
        let req = [0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
        match socks5_request(&mut s, &req).map_err(|e| ProbeError::Unreachable(e.to_string()))? {
            ConnectVerdict::Open => Ok(true),
            ConnectVerdict::AuthFailed => Err(ProbeError::AuthFailed),
            _ => Ok(false),
        }
    }
}

/// greeting + RFC1929 认证；认证被拒 → `AuthFailed`（凭据失效，不是目标被拒）
fn socks5_handshake(s: &mut std::net::TcpStream, up: &Upstream) -> std::io::Result<ConnectVerdict> {
    use std::io::{Read, Write};
    // 只声明 0x02（用户名密码）：住宅上游一律要鉴权，声明 0x00 只会让错配更难发现
    s.write_all(&[0x05, 0x01, 0x02])?;
    let mut sel = [0u8; 2];
    s.read_exact(&mut sel)?;
    if sel[1] != 0x02 {
        return Ok(ConnectVerdict::Unreachable {
            detail: format!("上游不接受用户名密码认证（METHOD=0x{:02x}）", sel[1]),
        });
    }
    let (u, p) = (up.username.as_bytes(), up.password.as_bytes());
    let mut auth = vec![0x01, u.len() as u8];
    auth.extend_from_slice(u);
    auth.push(p.len() as u8);
    auth.extend_from_slice(p);
    s.write_all(&auth)?;
    let mut st = [0u8; 2];
    s.read_exact(&mut st)?;
    Ok(if st[1] == 0 { ConnectVerdict::Open } else { ConnectVerdict::AuthFailed })
}

/// 发一条 SOCKS5 请求并读回 REP 字节（前 4 字节即够判定，BND 地址不读）
fn socks5_request(s: &mut std::net::TcpStream, req: &[u8]) -> std::io::Result<ConnectVerdict> {
    use std::io::{Read, Write};
    s.write_all(req)?;
    s.flush()?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head)?;
    Ok(socks_verdict(head[1]))
}

impl Prober for ReqwestProber {
    fn connect(&self, up: &Upstream, host: &str, port: u16) -> ConnectVerdict {
        if !valid_target_host(host) {
            return ConnectVerdict::Unreachable { detail: "目标主机名非法".into() };
        }
        let r = match up.kind {
            UpstreamKind::Http => self.http_connect(up, host, port),
            UpstreamKind::Socks5 => self.socks5_connect(up, host, port),
        };
        match r {
            Ok(v) => v,
            Err(e) => {
                // url 里本来只有 host:port，仍统一过 redact，防后人把凭据拼进来
                tracing::debug!(upstream = %crate::redact::url_credentials(&proxy_url(up)),
                                target = %host, error = %e, "隧道探测失败");
                ConnectVerdict::Unreachable { detail: e.to_string() }
            }
        }
    }

    fn get(&self, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
        let proxy = reqwest::Proxy::all(proxy_url(up))
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?
            .basic_auth(&up.username, &up.password);
        let client = reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        let resp = client.get(url).send().map_err(|e| {
            tracing::debug!(url = %crate::redact::url_credentials(url), error = %e, "经上游 GET 失败");
            // https 目标经 HTTP 上游走 CONNECT 隧道，407 在隧道建立阶段就失败了 ⇒
            // reqwest 只给一个 Err，拿不到状态码。把整条 source 链的文字拼出来扫一遍，
            // 这是 `get` 自己唯一能识别凭据失效的办法（第二层补判在调用方，见
            // `confirm_auth_failure` 的注释）。
            let mut chain = e.to_string();
            let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
            while let Some(s) = src {
                chain.push_str("; ");
                chain.push_str(&s.to_string());
                src = s.source();
            }
            if looks_like_proxy_auth(&chain) {
                ProbeError::AuthFailed
            } else {
                ProbeError::Unreachable(e.to_string())
            }
        })?;
        let status = resp.status().as_u16();
        // 只有「明文 HTTP 目标」才会走到这里（本模块的探测 URL 全是 https，407 走上面
        // 那条 Err 分支）。留着是为了将来真加明文目标时不必再想一遍。
        if status == 407 {
            return Err(ProbeError::AuthFailed);
        }
        let body: String = resp.text().unwrap_or_default().chars().take(65_536).collect();
        Ok(HttpProbe { status, body })
    }

    fn udp_associate(&self, up: &Upstream) -> Result<bool, ProbeError> {
        if up.kind == UpstreamKind::Http {
            return Ok(false); // HTTP 代理协议里没有 UDP ASSOCIATE
        }
        self.socks5_udp(up)
    }

    fn direct_tcp(&self, host: &str, port: u16) -> bool {
        use std::net::ToSocketAddrs;
        let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
        addrs.into_iter().any(|a| std::net::TcpStream::connect_timeout(&a, self.timeout).is_ok())
    }
}
```
`FakeProber`（`#[cfg(test)]`）：`connect` 查 `connects`（缺省 `Open`）并记 `connect:<host>:<port>`；`get` 查 `gets`（缺省 `Err(Unreachable("no route"))`）并记 `get:<url>`；`udp_associate` 返回 `Ok(inner.udp)` 并记 `udp`；`direct_tcp` 查 `direct` 并记 `direct:<host>:<port>`。

`get` 的错误映射**必须**按哨兵约定写死（T6/T7/T8 的测试都依赖，改了它们全红）：
```rust
fn get(&self, _up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
    let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
    i.calls.push(format!("get:{url}"));
    match i.gets.get(url) {
        Some(Ok(hp)) => Ok(hp.clone()),
        // 哨兵：让测试能构造「上游凭据失效」这一条路径（T7 的 407 用例）
        Some(Err(e)) if e == "__auth_failed__" => Err(ProbeError::AuthFailed),
        Some(Err(e)) => Err(ProbeError::Unreachable(e.clone())),
        None => Err(ProbeError::Unreachable("no route".into())),
    }
}
```

- [ ] **Step 5: 写握手的进程内假代理测试**

握手的字节序列是这个模块唯一无法用 fake 覆盖的部分（`FakeProber` 从定义上就跳过了它），
而它一旦写错，整个黑名单判定的输入就全错。用绑 `127.0.0.1:0` 的进程内假代理测它：端口由
内核分配（不冲突）、不出网、不碰 systemd 与文件系统——这是 Global Constraints 里写明的唯一豁免。

在 `mod tests` 里追加：
```rust
    /// 进程内假代理：绑 `127.0.0.1:0`，接一条连接，按 `script` 逐段「读一段 → 回一段」，
    /// 返回它收到的全部字节供断言。
    fn fake_proxy(script: Vec<Vec<u8>>) -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut seen = Vec::new();
            for chunk in script {
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap_or(0);
                seen.extend_from_slice(&buf[..n]);
                if s.write_all(&chunk).is_err() {
                    break;
                }
                let _ = s.flush();
            }
            seen
        });
        (port, h)
    }

    fn local(kind: UpstreamKind, port: u16) -> Upstream {
        let mut u = up(kind);
        u.host = "127.0.0.1".into();
        u.port = port;
        u
    }

    #[test]
    fn the_http_connect_handshake_reads_the_real_status_line() {
        let (port, h) = fake_proxy(vec![b"HTTP/1.1 403 Forbidden serp domain\r\n\r\n".to_vec()]);
        let p = ReqwestProber::with_timeout(2);
        assert_eq!(
            p.connect(&local(UpstreamKind::Http, port), "pay.google.com", 443),
            ConnectVerdict::Refused { code: 403 },
            "调研 §D 的硬拒形态"
        );
        let req = String::from_utf8(h.join().unwrap()).unwrap();
        assert!(req.starts_with("CONNECT pay.google.com:443 HTTP/1.1\r\n"), "实际 {req:?}");
        assert!(req.contains("Proxy-Authorization: Basic dXNlcjE6cHcx\r\n"), "实际 {req:?}");
    }

    #[test]
    fn the_socks5_handshake_maps_rep_bytes_and_auth_rejection() {
        // greeting → (05,02)；RFC1929 认证 → (01,00)；CONNECT → REP=0x02
        let (port, h) = fake_proxy(vec![
            vec![0x05, 0x02],
            vec![0x01, 0x00],
            vec![0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
        ]);
        let p = ReqwestProber::with_timeout(2);
        assert_eq!(
            p.connect(&local(UpstreamKind::Socks5, port), "gateway.icloud.com", 443),
            ConnectVerdict::Refused { code: 2 },
            "REP=0x02 not allowed by ruleset ⇒ 硬拒"
        );
        let seen = h.join().unwrap();
        assert_eq!(&seen[..3], &[0x05, 0x01, 0x02], "只声明用户名密码认证");
        assert!(seen.windows(5).any(|w| w == b"user1"), "RFC1929 里带了用户名");
        assert!(seen.windows(18).any(|w| w == b"gateway.icloud.com"), "ATYP=03 把域名原样交上游");

        // 认证被拒（STATUS ≠ 0）⇒ AuthFailed，不是「这个目标被拒」
        let (port2, h2) = fake_proxy(vec![vec![0x05, 0x02], vec![0x01, 0x01]]);
        assert_eq!(
            p.connect(&local(UpstreamKind::Socks5, port2), "gateway.icloud.com", 443),
            ConnectVerdict::AuthFailed
        );
        h2.join().unwrap();

        // UDP ASSOCIATE 成功（CMD=03、REP=0x00）
        let (port3, h3) = fake_proxy(vec![
            vec![0x05, 0x02],
            vec![0x01, 0x00],
            vec![0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x04, 0x38],
        ]);
        assert!(p.udp_associate(&local(UpstreamKind::Socks5, port3)).unwrap());
        let seen3 = h3.join().unwrap();
        assert!(seen3.windows(2).any(|w| w == [0x05, 0x03]), "CMD=03 才是 UDP ASSOCIATE");
    }

    #[test]
    fn an_upstream_that_never_answers_is_unreachable_not_refused() {
        // 绑了但不 accept：连上后读超时 ⇒ Unreachable。这条必须区分清楚，
        // 否则网络抖动会被当成硬拒写进黑名单。
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let p = ReqwestProber::with_timeout(1);
        let v = p.connect(&local(UpstreamKind::Http, port), "gateway.icloud.com", 443);
        assert!(matches!(v, ConnectVerdict::Unreachable { .. }), "实际 {v:?}");
        assert!(!v.is_hard_reject());
        drop(l);
    }
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui modules::residential::proxy && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 10 passed（7 条纯判定 / FakeProber 契约 + 3 条握手）。

**仍然只能留给 M2 真机的部分**：`get()` 经真实 SOCKS5/HTTP 上游发 HTTPS（要 TLS 与真实供应商）、以及调研 §D 那张表的复现（80/443 Open、六个端口 `refused:403`、SOCKS5 UDP ASSOCIATE 通）。上面三条假代理测试锁住的是「字节序列与状态码 → 判定」这一层。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/residential/proxy.rs
git commit -m "feat(resi): Prober（CONNECT/SOCKS5 判定、经上游 GET、UDP ASSOCIATE、直连对照）与 FakeProber"
```

---

### Task 4: Clash API 客户端（读/切 `resi-pool`）与 tag ↔ upstream 映射

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/clash.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{CLASH_API, CLASH_TIMEOUT_SECS, MEMBER_PREFIX, POOL}`；`bui_schema::model::ResidentialGroup`
- Produces:
```rust
// crate::modules::residential::clash
/// relay 的 Clash API（`127.0.0.1:9091`，只监听回环）。**同步** trait，调用点在
/// `spawn_blocking` 里（与 `Prober` 同一条铁律）。
pub trait Clash: Send + Sync + 'static {
    /// `GET /proxies/resi-pool` → `.now`；relay 未运行 / 无 clash_api → `None`
    fn selected(&self) -> Option<String>;
    /// `PUT /proxies/resi-pool` `{"name":"<tag>"}`；与配置切换走同一条 `SelectOutbound`
    /// 路径（调研 S4），`interrupt_exist_connections: false` 保证不掐既有连接
    fn select(&self, tag: &str) -> Result<(), ClashError>;
}
#[derive(Debug, thiserror::Error)]
pub enum ClashError {
    #[error("Clash API 不可达：{0}")]
    Unreachable(String),
    #[error("Clash API 拒绝切换到 {tag}（HTTP {status}）")]
    Rejected { tag: String, status: u16 },
}
pub struct HttpClash { api: String, timeout: std::time::Duration }
impl HttpClash { pub fn new() -> Self; pub fn with_api(api: impl Into<String>, secs: u64) -> Self; }
impl Default for HttpClash { fn default() -> Self { Self::new() } }

/// 上游 id → relay 里的成员 tag（`resi-{下标+1}`，与 `bui_schema::render::relay` 的
/// `tag()` 同规则；**唯一**一处做这个换算）
pub fn tag_of(g: &ResidentialGroup, id: Uuid) -> Option<String>;
/// 成员 tag → 上游 id
pub fn id_of_tag(g: &ResidentialGroup, tag: &str) -> Option<Uuid>;
/// 当前池的全部成员 tag，顺序与 `g.upstreams` 一致
pub fn tags(g: &ResidentialGroup) -> Vec<String>;
#[cfg(test)] pub struct FakeClash { /* Mutex<FakeClashInner> */ }
#[cfg(test)]
#[derive(Default)]
pub struct FakeClashInner {
    pub now: Option<String>,
    /// 令 `select` 失败（测「切换失败只告警不改 runtime」）
    pub reject: bool,
    pub calls: Vec<String>,
}
#[cfg(test)]
impl FakeClash {
    pub fn new(now: Option<&str>) -> Self;
    pub fn with(&self, f: impl FnOnce(&mut FakeClashInner)) -> &Self;
    pub fn calls(&self) -> Vec<String>;         // "get" / "put:<tag>"
}
```
`tag_of` / `id_of_tag` 必须与 P0 `render/relay.rs` 的 `let tag = |i: usize| format!("resi-{}", i + 1);` 逐字对应——这是 state（uuid）与 relay/Clash API（tag）之间的**唯一**桥。`render/relay.rs` 若改 tag 规则，本文件的两条测试会立刻红（它们把 `resi-1`/`resi-2` 写死）。

- [ ] **Step 1: 写失败测试**

```rust
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
        ResidentialGroup { enabled: true, upstreams, ..Default::default() }
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
    fn fake_clash_records_calls() {
        let c = FakeClash::new(Some("resi-1"));
        assert_eq!(c.selected().as_deref(), Some("resi-1"));
        c.select("resi-2").unwrap();
        assert_eq!(c.selected().as_deref(), Some("resi-2"), "切换后 now 跟着变");
        c.with(|i| i.reject = true);
        assert!(matches!(c.select("resi-1"), Err(ClashError::Rejected { .. })));
        assert_eq!(c.selected().as_deref(), Some("resi-2"), "失败不改 now");
        assert_eq!(c.calls(), vec!["get", "put:resi-2", "get", "put:resi-1", "get"]);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::clash`
Expected: 编译失败，`cannot find function `tag_of``。

- [ ] **Step 3: 实现映射与 `HttpClash`**

```rust
pub fn tags(g: &ResidentialGroup) -> Vec<String> {
    (0..g.upstreams.len()).map(|i| format!("{MEMBER_PREFIX}{}", i + 1)).collect()
}

pub fn tag_of(g: &ResidentialGroup, id: Uuid) -> Option<String> {
    g.upstreams
        .iter()
        .position(|u| u.id == id)
        .map(|i| format!("{MEMBER_PREFIX}{}", i + 1))
}

pub fn id_of_tag(g: &ResidentialGroup, tag: &str) -> Option<Uuid> {
    let n: usize = tag.strip_prefix(MEMBER_PREFIX)?.parse().ok()?;
    // tag 从 1 开始；0 与越界都返回 None
    g.upstreams.get(n.checked_sub(1)?).map(|u| u.id)
}

impl HttpClash {
    pub fn new() -> Self {
        Self::with_api(CLASH_API, CLASH_TIMEOUT_SECS)
    }
    pub fn with_api(api: impl Into<String>, secs: u64) -> Self {
        Self { api: api.into(), timeout: std::time::Duration::from_secs(secs) }
    }
    fn client(&self) -> Result<reqwest::blocking::Client, ClashError> {
        reqwest::blocking::Client::builder()
            // 回环地址，绝不能走系统代理（住宅上游本身就是代理，绕回去会死锁）
            .no_proxy()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ClashError::Unreachable(e.to_string()))
    }
    fn url(&self) -> String {
        format!("http://{}/proxies/{POOL}", self.api)
    }
}

impl Clash for HttpClash {
    fn selected(&self) -> Option<String> {
        let v: serde_json::Value = self.client().ok()?.get(self.url()).send().ok()?.json().ok()?;
        v.get("now")?.as_str().map(str::to_string)
    }

    fn select(&self, tag: &str) -> Result<(), ClashError> {
        let resp = self
            .client()?
            .put(self.url())
            .json(&serde_json::json!({ "name": tag }))
            .send()
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(ClashError::Rejected { tag: tag.to_string(), status })
        }
    }
}
```
`FakeClash`（`#[cfg(test)]`）：`selected` 记 `get` 后返回 `inner.now.clone()`；`select` 记 `put:<tag>`，`reject` 为真时返回 `Rejected{status:404}` 且不改 `now`，否则把 `now` 设为该 tag。

- [ ] **Step 4: 写 `HttpClash` 的进程内假 Clash API 测试**

`FakeClash` 只覆盖调用约定，不覆盖 `HttpClash` 自己拼的 URL、`no_proxy()`、状态码映射。
用绑 `127.0.0.1:0` 的进程内假服务器测这一层（Global Constraints 里写明的唯一豁免：
不出网、不碰 systemd 与文件系统，端口由内核分配）。在 `mod tests` 里追加：

```rust
    // 都带 `Connection: close` 且不写 Content-Length：服务端写完即关，客户端读到 EOF 为止，
    // 免得测试里手算长度
    const OK_NOW: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"now\":\"resi-2\"}";
    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n{}";

    /// 进程内假 Clash API：按顺序对每个来访连接回 `responses[i]`，返回收到的请求原文
    /// （小请求一次 read 就能读全首行 + 首部 + body，够断言用）。
    fn fake_clash_api(responses: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
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
        assert_eq!(c.selected().as_deref(), Some("resi-2"));
        c.select("resi-3").unwrap();
        assert!(matches!(
            c.select("resi-9"),
            Err(ClashError::Rejected { status: 404, .. })
        ));
        let reqs = h.join().unwrap();
        assert!(reqs[0].starts_with("GET /proxies/resi-pool HTTP/1.1\r\n"), "实际 {:?}", reqs[0]);
        assert!(reqs[1].starts_with("PUT /proxies/resi-pool HTTP/1.1\r\n"), "实际 {:?}", reqs[1]);
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
        assert_eq!(c.selected(), None, "读不到当前选择 ⇒ 巡检本轮不切（T8 规则 5）");
        assert!(matches!(c.select("resi-1"), Err(ClashError::Unreachable(_))));
    }
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::clash && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 4 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/clash.rs
git commit -m "feat(resi): Clash API 客户端（读/切 resi-pool）与 tag↔upstream 唯一换算"
```

---

### Task 5: `b-ui-relay` 日志解析与游标增量收集

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/journal.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::JOURNAL_UNIT`；`crate::sys::{CmdOut, Host}`
- Produces:
```rust
// crate::modules::residential::journal
/// relay 日志里一条「上游拒绝了这个目标」的记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectLine {
    /// 成员 tag（`resi-1`…），来自 `outbound/http[resi-1]`
    pub tag: String,
    pub host: String,
    pub port: u16,
    /// HTTP 上游的 CONNECT 状态码；SOCKS 拒绝行没有状态码 → `None`
    pub status: Option<u16>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBatch { pub lines: Vec<RejectLine>, pub cursor: Option<String> }

/// 去掉 ANSI 色码（sing-box 往 journald 写带色输出，R13 §6.2 踩过）
pub fn strip_ansi(s: &str) -> String;
/// 解析一行；不是拒绝行（或不含状态/拒绝语义）→ `None`
pub fn parse_line(line: &str) -> Option<RejectLine>;
/// 增量读日志：有游标用 `--after-cursor`，否则 `--since -25h`（与 R13 §6.2 同窗口）。
/// `journalctl` 不存在 → `Ok(JournalBatch::default())`（调用方据此记一条 alert）。
pub fn collect(host: &dyn Host, cursor: Option<&str>) -> anyhow::Result<JournalBatch>;
```
**为什么是游标增量而不是 `-f`**：见契约决策 §E。命令形态写死为
`journalctl -u b-ui-relay --no-pager -o cat --show-cursor {--after-cursor <c> | --since -25h}`，`--show-cursor` 的最后一行是 `-- cursor: s=…`，解析后存 `runtime.journal_cursor`，不重不漏。全部经 `Host::run`，`FakeHost.scripted` 就能完整测试。

- [ ] **Step 1: 写失败测试（用真实日志形态）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    // bwg-tizi 2026-09-11 的真实形态（R13 §1 与调研 §D）；第一行带 ANSI 色码
    const HTTP_403: &str = "\x1b[31mERROR\x1b[0m open connection to gateway.icloud.com:443 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
    const HTTP_403_PORT: &str = "open connection to 198.51.100.9:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden serp domain";
    const SOCKS_DENY: &str = "open connection to x.com:443 using outbound/socks[resi-2]: socks5: connection not allowed by ruleset";
    const UNRELATED: &str = "inbound/socks[socks-in]: inbound connection from 127.0.0.1:41234";
    const TIMEOUT: &str = "open connection to a.example.com:443 using outbound/http[resi-1]: context deadline exceeded";

    #[test]
    fn parses_the_three_real_rejection_shapes() {
        assert_eq!(
            parse_line(HTTP_403),
            Some(RejectLine { tag: "resi-1".into(), host: "gateway.icloud.com".into(), port: 443, status: Some(403) })
        );
        assert_eq!(
            parse_line(HTTP_403_PORT),
            Some(RejectLine { tag: "resi-1".into(), host: "198.51.100.9".into(), port: 5228, status: Some(403) })
        );
        assert_eq!(
            parse_line(SOCKS_DENY),
            Some(RejectLine { tag: "resi-2".into(), host: "x.com".into(), port: 443, status: None })
        );
    }

    #[test]
    fn ignores_lines_that_are_not_upstream_rejections() {
        assert_eq!(parse_line(UNRELATED), None);
        // 超时不是拒绝：算进候选会把网络抖动学成黑名单
        assert_eq!(parse_line(TIMEOUT), None);
        // 2xx 不是拒绝
        assert_eq!(
            parse_line("open connection to a.com:443 using outbound/http[resi-1]: unexpected status: 200 OK"),
            None
        );
        // direct 出站的行与住宅无关
        assert_eq!(
            parse_line("open connection to a.com:443 using outbound/direct[direct]: unexpected status: 403 Forbidden"),
            None
        );
        assert_eq!(parse_line(""), None);
        // 端口不是数字
        assert_eq!(
            parse_line("open connection to a.com:https using outbound/http[resi-1]: unexpected status: 403 x"),
            None
        );
    }

    #[test]
    fn strip_ansi_removes_color_codes_only() {
        assert_eq!(strip_ansi("\x1b[31mERROR\x1b[0m x"), "ERROR x");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn collect_uses_since_on_the_first_run_and_after_cursor_afterwards() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("journalctl".into());
            i.scripted.push((
                format!("journalctl -u {JOURNAL_UNIT} --no-pager -o cat --show-cursor --since -25h"),
                CmdOut::success(&format!("{HTTP_403}\n{SOCKS_DENY}\n{UNRELATED}\n-- cursor: s=abc;i=1;b=2\n")),
            ));
            i.scripted.push((
                format!("journalctl -u {JOURNAL_UNIT} --no-pager -o cat --show-cursor --after-cursor s=abc;i=1;b=2"),
                CmdOut::success(&format!("{HTTP_403_PORT}\n-- cursor: s=abc;i=9;b=2\n")),
            ));
        });
        let first = collect(&h, None).unwrap();
        assert_eq!(first.lines.len(), 2, "两条拒绝行，无关行被丢掉");
        assert_eq!(first.cursor.as_deref(), Some("s=abc;i=1;b=2"));
        let second = collect(&h, first.cursor.as_deref()).unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].port, 5228);
        assert_eq!(second.cursor.as_deref(), Some("s=abc;i=9;b=2"));
    }

    #[test]
    fn collect_is_a_no_op_when_journalctl_is_missing() {
        let h = FakeHost::new(); // which 里没有 journalctl
        assert_eq!(collect(&h, None).unwrap(), JournalBatch::default());
        assert!(h.ops().is_empty(), "不该尝试执行不存在的命令");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::journal`
Expected: 编译失败，`cannot find function `parse_line``。

- [ ] **Step 3: 实现解析（手写扫描，不引 regex）**

```rust
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

pub fn parse_line(line: &str) -> Option<RejectLine> {
    let line = strip_ansi(line);
    // open connection to <host>:<port> using outbound/<kind>[<tag>]: <reason>
    let rest = line.split_once("open connection to ")?.1;
    let (target, rest) = rest.split_once(" using outbound/")?;
    let (host, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    let (kind_tag, reason) = rest.split_once("]: ")?;
    let (kind, tag) = kind_tag.split_once('[')?;
    // 只学住宅出站的拒绝；direct 出站与住宅无关
    if !matches!(kind, "http" | "socks") || !tag.starts_with(super::MEMBER_PREFIX) {
        return None;
    }
    if let Some(s) = reason.strip_prefix("unexpected status: ") {
        let code: u16 = s.split_whitespace().next()?.parse().ok()?;
        // 只有 4xx/5xx 才是拒绝（2xx 出现在这条 error 行里只可能是上游的怪行为）
        if !(400..600).contains(&code) {
            return None;
        }
        return Some(RejectLine { tag: tag.to_string(), host: host.to_string(), port, status: Some(code) });
    }
    // SOCKS5 的拒绝形态（REP ≠ 0 的文字化）。超时/EOF 一类不是拒绝，不学。
    let denied = ["not allowed", "refused", "rejected", "forbidden", "denied"]
        .iter()
        .any(|k| reason.to_ascii_lowercase().contains(k));
    denied.then(|| RejectLine { tag: tag.to_string(), host: host.to_string(), port, status: None })
}
```

- [ ] **Step 4: 实现 `collect`**

```rust
pub fn collect(host: &dyn Host, cursor: Option<&str>) -> anyhow::Result<JournalBatch> {
    if !host.which("journalctl") {
        return Ok(JournalBatch::default());
    }
    let mut args: Vec<&str> = vec!["-u", JOURNAL_UNIT, "--no-pager", "-o", "cat", "--show-cursor"];
    match cursor {
        Some(c) => {
            args.push("--after-cursor");
            args.push(c);
        }
        // 首次或游标失效：与 R13 §6.2 同窗口
        None => {
            args.push("--since");
            args.push("-25h");
        }
    }
    let out = host.run("journalctl", &args)?;
    if !out.ok() {
        // 游标失效时 journalctl 会报错；调用方下一轮会用 None 重来（见 Task 9）
        anyhow::bail!("journalctl 退出码 {}", out.status);
    }
    let mut batch = JournalBatch::default();
    for line in out.stdout.lines() {
        if let Some(c) = line.trim().strip_prefix("-- cursor: ") {
            batch.cursor = Some(c.trim().to_string());
            continue;
        }
        if let Some(r) = parse_line(line) {
            batch.lines.push(r);
        }
    }
    Ok(batch)
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::journal && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 5 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/journal.rs
git commit -m "feat(resi): b-ui-relay 日志拒绝行解析与 journald 游标增量收集"
```

---

### Task 6: 出口体检（三方交叉分类、搜索/AI/支付、端口集、UDP ASSOCIATE、`ports_allowed`）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/check.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{fanout, AI_HOSTS, BASE_PORTS, PAY_HOSTS, PROBE_PORTS}`；`crate::modules::residential::proxy::{ConnectVerdict, HttpProbe, ProbeError, Prober}`；`crate::modules::residential::state`；`crate::util::fmt_rfc3339`；`bui_schema::model::{Upstream, Verified}`
- Produces:
```rust
// crate::modules::residential::check
/// 出口类型。`label()` 返回的**中文串**是面板契约（`web/app.js:1095` 用 `/IDC|机房/i`
/// 判色），不得改成英文枚举，否则「非住宅」告警永久失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass { Residential, Datacenter, Proxy, Mobile, Unknown }
impl ExitClass { pub fn label(&self) -> &'static str; }   // 家庭宽带 IP / IDC机房 IP / 代理 IP / 移动网络 IP / unknown

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExitInfo {
    pub ip: Option<String>, pub asn: Option<u32>, pub org: Option<String>,
    pub country: Option<String>, pub city: Option<String>,
}
/// 一次体检的完整结果（存 `runtime.checks[<upstream_id>]`，面板整体回显）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub at: String,
    pub upstream_id: Uuid,
    pub class: ExitClass,
    /// 面板直接显示这一串（v3 `egress_ip_type` 的取值集合）
    pub class_label: String,
    pub exit: ExitInfo,
    /// 参与交叉的数据源与各自的判定（`ippure` / `ipquery` / `ipinfo`）
    pub sources: BTreeMap<String, String>,
    /// Google 搜索页是否落到 `/sorry/`；`None` = 没探到结论
    pub google_sorry: Option<bool>,
    pub ai: BTreeMap<String, bool>,
    pub payments: BTreeMap<String, bool>,
    /// 端口 → `ConnectVerdict::label()`
    pub ports: BTreeMap<u16, String>,
    pub udp_associate: Option<bool>,
    /// 学到的端口白名单（`None` = 不限或学不到）
    pub ports_allowed: Option<Vec<u16>>,
    /// 本轮遇到 407 / SOCKS5 认证被拒 —— 结果不可信，不写 `ports_allowed`
    pub auth_failed: bool,
    pub notes: Vec<String>,
}

/// 三方出口画像的数据源（v3 `web/server.js:546-629` 同源、同顺序）
pub const EXIT_SOURCES: [(&str, &str); 3] = [
    ("ippure",  "https://my.ippure.com/v1/info"),
    ("ipquery", "https://api.ipquery.io/?format=json"),
    ("ipinfo",  "https://ipinfo.io/json"),
];
/// Google 搜索探测目标：命中 `/sorry/` 即被判为机器人
pub const GOOGLE_SEARCH_URL: &str = "https://www.google.com/search?q=hello";

/// Cloudflare 挑战页识别（spec §5.2：识别为「未知」而非失败）
pub fn is_cloudflare_challenge(p: &HttpProbe) -> bool;
pub fn classify_ippure(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)>;
pub fn classify_ipquery(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)>;
pub fn classify_ipinfo(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)>;
/// 交叉：多数票；平票取先出现的（数据源顺序 = 可信度顺序）；无票 `Unknown`
pub fn cross_class(votes: &[ExitClass]) -> ExitClass;
/// **唯一**一份三源交叉逻辑：三个源各自的 GET 结果（`None` = 该项探测任务 panic 了）
/// → (分类, 归属, `sources` 表, 本轮是否遇到凭据失效, notes)。
/// `probe_exit`（顺序版）与 `run`（`fanout` 并发版）都调它，判定逻辑没有第二份。
pub fn fold_sources(results: &[Option<Result<HttpProbe, ProbeError>>])
    -> (ExitClass, ExitInfo, BTreeMap<String, String>, bool, Vec<String>);
/// 三源出口画像的**同步、顺序**版：给 T7 的 `add` 用（那条路径已经在 `spawn_blocking`
/// 里，且只需要这一项）。内部就是「三次 `p.get` → `fold_sources`」。
pub fn probe_exit(p: &dyn Prober, up: &Upstream) -> (ExitClass, ExitInfo, BTreeMap<String, String>, Vec<String>);
/// 搜索页 GET 结果 → 是否被判机器人（正文含 `/sorry/` 或状态码 429）；
/// `None` = 没探到结论（别把网络抖动显示成「被判机器人」）
pub fn sorry_of(r: Option<&Result<HttpProbe, ProbeError>>) -> Option<bool>;
/// `/sorry/` 检测（同步版；`run` 里用 `sorry_of` 读 `fanout` 的那一项）
pub fn google_sorry(p: &dyn Prober, up: &Upstream) -> Option<bool>;
/// `ports` 表 →（`ports_allowed`, `auth_failed`）：全通 → `None`（不限）；
/// 有通有拒 → `Some(通的那些)`；遇到 407 → 不学
pub fn derive_ports_allowed(ports: &BTreeMap<u16, String>, auth_failed: bool) -> Option<Vec<u16>>;
/// `ExitInfo` → `Verified`（写回 `state` 的那个结构；`ip` 为 `None` 时返回 `None`）
pub fn to_verified(e: &ExitInfo, at: &str) -> Option<Verified>;

/// 一次完整体检：三源出口 + Google sorry + AI + 支付 + 端口集 + UDP ASSOCIATE。
/// **全部**探测经 [`fanout`] 并发跑（上限 4），整个函数是 `async`（因为 `fanout` 是），
/// 每个探测项自身在 `spawn_blocking` 里。
pub async fn run(p: Arc<dyn Prober>, up: &Upstream, now: OffsetDateTime) -> CheckReport;
/// 跑一次体检并把结果写进 state（`verified` / `ports_allowed`）与
/// `runtime.checks[<id>]`；期间 `runtime.checking` 为 `Some`（面板显示「检测中…」）
pub async fn run_and_store(ctx: &DaemonCtx, p: Arc<dyn Prober>, id: Uuid) -> anyhow::Result<CheckReport>;
```

**`derive_ports_allowed` 的判据**（spec §5.2「端口集里只有 80/443 通 → `[80,443]`」）：探测集合 = `BASE_PORTS` ∪ `PROBE_PORTS`（8 个）。① `auth_failed` 为真 → `None`（凭据都失效了，拒绝不代表端口策略）；② 有任何 `unreachable` → `None`（网络抖动，别把抖动学成白名单，宁可不限）；③ 全部 `open` → `None`（不限）；④ 否则 → `Some(全部 open 的端口，升序)`。`Some(vec![])` 永不产生：若连 80/443 都拒，说明上游整体有问题，落到 ②/① 或记一条 note 后返回 `None`——空白名单会让 `render/relay.rs` 的 `inverted_port_ranges` 把**所有**端口判成直连，等于静默关掉住宅。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::proxy::{ConnectVerdict, FakeProber, HttpProbe};
    use bui_schema::model::UpstreamKind;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime { datetime!(2026-09-12 00:00:00 UTC) }

    fn up() -> Upstream {
        Upstream {
            id: Uuid::from_u128(1), name: "url-1".into(), kind: UpstreamKind::Http,
            host: "isp.example.net".into(), port: 10007,
            username: "user1".into(), password: "pw1".into(),
            priority: 10, provider: Some("decodo".into()), region: Some("US".into()),
            ports_allowed: None, verified: None,
        }
    }

    fn json(body: serde_json::Value) -> Result<HttpProbe, String> {
        Ok(HttpProbe { status: 200, body: body.to_string() })
    }

    #[test]
    fn exit_class_labels_are_the_v3_chinese_strings() {
        // web/app.js:1095 用 /IDC|机房/i 判色，改成英文会让「非住宅」告警永久失效
        assert_eq!(ExitClass::Residential.label(), "家庭宽带 IP");
        assert_eq!(ExitClass::Datacenter.label(), "IDC机房 IP");
        assert_eq!(ExitClass::Proxy.label(), "代理 IP");
        assert_eq!(ExitClass::Mobile.label(), "移动网络 IP");
        assert_eq!(ExitClass::Unknown.label(), "unknown");
    }

    #[test]
    fn classifiers_read_each_providers_real_shape() {
        // ippure（2026-09-10 实测形状）
        let (c, e) = classify_ippure(&serde_json::json!({
            "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
            "country": "US", "city": "Denver", "fraudScore": 2, "isResidential": true
        }))
        .unwrap();
        assert_eq!(c, ExitClass::Residential);
        assert_eq!((e.asn, e.org.as_deref()), (Some(33667), Some("Comcast")));
        assert_eq!(
            classify_ippure(&serde_json::json!({"ip": "203.0.113.10", "isResidential": false})).unwrap().0,
            ExitClass::Datacenter
        );
        // isResidential 缺失 → 只有归属，类型未知（不能猜）
        assert_eq!(
            classify_ippure(&serde_json::json!({"ip": "203.0.113.10"})).unwrap().0,
            ExitClass::Unknown
        );
        // ipquery
        assert_eq!(
            classify_ipquery(&serde_json::json!({
                "ip": "198.51.100.7",
                "isp": {"asn": "AS33667", "org": "Comcast"},
                "location": {"country": "US", "city": "Denver"},
                "risk": {"is_datacenter": false, "is_vpn": false, "is_proxy": false, "is_mobile": false}
            }))
            .unwrap()
            .0,
            ExitClass::Residential
        );
        assert_eq!(
            classify_ipquery(&serde_json::json!({"ip": "1.2.3.4", "risk": {"is_mobile": true}})).unwrap().0,
            ExitClass::Mobile
        );
        assert_eq!(
            classify_ipquery(&serde_json::json!({"ip": "1.2.3.4", "risk": {"is_vpn": true}})).unwrap().0,
            ExitClass::Proxy
        );
        // ipinfo 只有归属
        let (c, e) = classify_ipinfo(&serde_json::json!({
            "ip": "198.51.100.7", "org": "AS33667 Comcast", "country": "US", "city": "Denver"
        }))
        .unwrap();
        assert_eq!(c, ExitClass::Unknown, "ipinfo 没有类型字段，只能记 unknown");
        assert_eq!(e.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(classify_ipinfo(&serde_json::json!({})), None, "没有 ip 就不算一票");
    }

    #[test]
    fn cloudflare_challenge_is_unknown_not_failure() {
        // spec §5.2：识别 Cloudflare 挑战页为「未知」而非失败
        assert!(is_cloudflare_challenge(&HttpProbe {
            status: 403,
            body: "<title>Just a moment...</title><div id=\"cf-challenge-running\">".into()
        }));
        assert!(is_cloudflare_challenge(&HttpProbe {
            status: 503,
            body: "Attention Required! | Cloudflare".into()
        }));
        assert!(!is_cloudflare_challenge(&HttpProbe { status: 200, body: "{\"ip\":\"1.2.3.4\"}".into() }));
    }

    #[test]
    fn cross_class_takes_the_majority_then_source_order() {
        use ExitClass::*;
        assert_eq!(cross_class(&[Residential, Residential, Unknown]), Residential);
        assert_eq!(cross_class(&[Datacenter, Residential, Unknown]), Datacenter, "平票取先出现的（源顺序即可信度）");
        assert_eq!(cross_class(&[Unknown, Unknown, Unknown]), Unknown);
        assert_eq!(cross_class(&[]), Unknown);
        assert_eq!(cross_class(&[Unknown, Residential]), Residential, "Unknown 不参与投票");
    }

    #[test]
    fn ports_allowed_is_learned_only_from_a_clean_round() {
        let p = |pairs: &[(u16, &str)]| -> BTreeMap<u16, String> {
            pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
        };
        // 调研 §D 的 Decodo 实测：只放行 80/443，其余六个 403
        let decodo = p(&[
            (22, "refused:403"), (80, "open"), (443, "open"), (853, "refused:403"),
            (993, "refused:403"), (5223, "refused:403"), (5228, "refused:403"), (8080, "refused:403"),
        ]);
        assert_eq!(derive_ports_allowed(&decodo, false), Some(vec![80, 443]));
        // 全通 → 不限
        let all_open: BTreeMap<u16, String> =
            [22u16, 80, 443, 853, 993, 5223, 5228, 8080].iter().map(|k| (*k, "open".into())).collect();
        assert_eq!(derive_ports_allowed(&all_open, false), None);
        // 有一项探测失败 → 不学（宁可不限，别把抖动学成白名单）
        let mut flaky = decodo.clone();
        flaky.insert(993, "unreachable".into());
        assert_eq!(derive_ports_allowed(&flaky, false), None);
        // 凭据失效 → 不学
        assert_eq!(derive_ports_allowed(&decodo, true), None);
        // 连 80/443 都拒 → 不学（空白名单会让 relay 把所有端口判成直连，等于静默关住宅）
        let dead: BTreeMap<u16, String> =
            [22u16, 80, 443, 853, 993, 5223, 5228, 8080].iter().map(|k| (*k, "refused:403".into())).collect();
        assert_eq!(derive_ports_allowed(&dead, false), None);
    }

    #[tokio::test]
    async fn full_check_reproduces_the_decodo_measurement() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets.insert(EXIT_SOURCES[0].1.into(), json(serde_json::json!({
                "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
                "country": "US", "city": "Denver", "isResidential": true
            })));
            i.gets.insert(EXIT_SOURCES[1].1.into(), json(serde_json::json!({
                "ip": "198.51.100.7", "isp": {"asn": "AS33667", "org": "Comcast"},
                "location": {"country": "US"}, "risk": {"is_datacenter": false}
            })));
            i.gets.insert(GOOGLE_SEARCH_URL.into(), Ok(HttpProbe { status: 200, body: "<html>results".into() }));
            for h in AI_HOSTS.iter().chain(PAY_HOSTS.iter()) {
                i.connects.insert(format!("{h}:443"), ConnectVerdict::Open);
            }
            // 调研 §D：pay.google.com 与 www.paypal.com 在 Decodo 上 403
            i.connects.insert("pay.google.com:443".into(), ConnectVerdict::Refused { code: 403 });
            i.connects.insert("www.paypal.com:443".into(), ConnectVerdict::Refused { code: 403 });
            for port in PROBE_PORTS {
                i.connects.insert(format!("www.google.com:{port}"), ConnectVerdict::Refused { code: 403 });
            }
            i.udp = true;
        });
        let r = run(p, &up(), t0()).await;
        assert_eq!(r.class, ExitClass::Residential);
        assert_eq!(r.class_label, "家庭宽带 IP");
        assert_eq!(r.exit.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(r.google_sorry, Some(false));
        assert!(r.ai["api.anthropic.com"], "AI 可达性");
        assert!(!r.payments["pay.google.com"], "调研 §D：Decodo 上 403");
        assert!(r.payments["checkout.stripe.com"]);
        assert_eq!(r.ports[&5228], "refused:403");
        assert_eq!(r.ports[&443], "open");
        assert_eq!(r.ports_allowed, Some(vec![80, 443]));
        assert_eq!(r.udp_associate, Some(true));
        assert!(!r.auth_failed);
        assert_eq!(r.at, "2026-09-12T00:00:00Z");
    }

    #[test]
    fn probe_exit_crosses_three_sources_in_one_pass_and_to_verified_needs_an_ip() {
        // T7 的 `add` 只吃这一条（60 秒预算装不下整轮体检），所以它必须独立可测
        let p = FakeProber::new();
        p.with(|i| {
            i.gets.insert(EXIT_SOURCES[0].1.into(), json(serde_json::json!({
                "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
                "country": "US", "city": "Denver", "isResidential": true})));
            i.gets.insert(EXIT_SOURCES[1].1.into(), json(serde_json::json!({
                "ip": "198.51.100.7", "isp": {"asn": "AS33667", "org": "Comcast"},
                "location": {"country": "US"}, "risk": {"is_datacenter": false}})));
        });
        let (class, exit, sources, notes) = probe_exit(&p, &up());
        assert_eq!(class, ExitClass::Residential);
        assert_eq!(exit.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!((exit.asn, exit.org.as_deref()), (Some(33667), Some("Comcast")));
        assert!(sources["ipinfo"].starts_with("error:"), "第三个源没登记 ⇒ 记错误、不算票");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            p.calls(),
            vec![
                format!("get:{}", EXIT_SOURCES[0].1),
                format!("get:{}", EXIT_SOURCES[1].1),
                format!("get:{}", EXIT_SOURCES[2].1),
            ],
            "顺序版：三个源各一次，顺序照 EXIT_SOURCES（= 可信度顺序）"
        );
        let v = to_verified(&exit, "2026-09-12T00:00:00Z").unwrap();
        assert_eq!(
            (v.ip.as_str(), v.asn, v.org.as_deref(), v.country.as_deref(), v.at.as_str()),
            ("198.51.100.7", Some(33667), Some("Comcast"), Some("US"), "2026-09-12T00:00:00Z")
        );
        assert!(
            to_verified(&ExitInfo::default(), "2026-09-12T00:00:00Z").is_none(),
            "没 IP 就不写 verified（宁可空着等下次体检）"
        );
        // 三源全哑 ⇒ Unknown + 一条 note，不猜
        let (class2, exit2, _s2, notes2) = probe_exit(&FakeProber::new(), &up());
        assert_eq!(class2, ExitClass::Unknown);
        assert!(exit2.ip.is_none());
        assert_eq!(notes2.len(), 1, "{notes2:?}");
    }

    #[test]
    fn google_sorry_reads_the_search_page_and_stays_none_when_unknown() {
        let p = FakeProber::new();
        p.with(|i| {
            i.gets.insert(
                GOOGLE_SEARCH_URL.into(),
                Ok(HttpProbe { status: 429, body: "<title>https://www.google.com/sorry/index".into() }),
            );
        });
        assert_eq!(google_sorry(&p, &up()), Some(true));
        let p2 = FakeProber::new();
        p2.with(|i| {
            i.gets.insert(
                GOOGLE_SEARCH_URL.into(),
                Ok(HttpProbe { status: 200, body: "<html>results".into() }),
            );
        });
        assert_eq!(google_sorry(&p2, &up()), Some(false));
        assert_eq!(google_sorry(&FakeProber::new(), &up()), None, "探不到就不下结论");
        assert_eq!(sorry_of(None), None, "探测任务 panic 也是「未知」");
    }

    #[tokio::test]
    async fn a_407_marks_the_whole_round_untrustworthy() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            for port in PROBE_PORTS.iter().chain(BASE_PORTS.iter()) {
                i.connects.insert(format!("www.google.com:{port}"), ConnectVerdict::AuthFailed);
            }
        });
        let r = run(p, &up(), t0()).await;
        assert!(r.auth_failed, "调研 §D 的 407 场景");
        assert_eq!(r.ports_allowed, None, "凭据失效时绝不写端口白名单");
        assert!(r.notes.iter().any(|n| n.contains("凭据")), "notes 要指名凭据失效：{:?}", r.notes);
    }

    #[tokio::test]
    async fn google_sorry_is_detected_and_cloudflare_is_not_a_failure() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets.insert(
                GOOGLE_SEARCH_URL.into(),
                Ok(HttpProbe { status: 429, body: "<title>https://www.google.com/sorry/index".into() }),
            );
            i.gets.insert(
                EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe { status: 403, body: "<title>Just a moment...</title>".into() }),
            );
        });
        let r = run(p, &up(), t0()).await;
        assert_eq!(r.google_sorry, Some(true));
        assert_eq!(r.class, ExitClass::Unknown, "挑战页记未知，不是失败");
        assert_eq!(r.sources["ippure"], "cloudflare_challenge");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::check`
Expected: 编译失败，`cannot find type `ExitClass``。

- [ ] **Step 3: 实现分类器与交叉**

```rust
impl ExitClass {
    pub fn label(&self) -> &'static str {
        // 这些中文串是面板契约（v3 `egress_ip_type`），不要改
        match self {
            ExitClass::Residential => "家庭宽带 IP",
            ExitClass::Datacenter => "IDC机房 IP",
            ExitClass::Proxy => "代理 IP",
            ExitClass::Mobile => "移动网络 IP",
            ExitClass::Unknown => "unknown",
        }
    }
}

pub fn is_cloudflare_challenge(p: &HttpProbe) -> bool {
    if p.status == 200 {
        return false;
    }
    let b = p.body.to_ascii_lowercase();
    ["just a moment", "cf-challenge", "attention required! | cloudflare", "cf-browser-verification"]
        .iter()
        .any(|k| b.contains(k))
}

pub fn classify_ippure(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let class = match v.get("isResidential").and_then(serde_json::Value::as_bool) {
        Some(true) => ExitClass::Residential,
        Some(false) => ExitClass::Datacenter,
        None => ExitClass::Unknown, // 只剩归属字段时不猜类型
    };
    Some((
        class,
        ExitInfo {
            ip: Some(ip),
            asn: v.get("asn").and_then(as_asn),
            org: v.get("asOrganization").and_then(|x| x.as_str()).map(str::to_string),
            country: v.get("country").and_then(|x| x.as_str()).map(str::to_string),
            city: v.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

pub fn classify_ipquery(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let risk = v.get("risk").cloned().unwrap_or(serde_json::Value::Null);
    let b = |k: &str| risk.get(k).and_then(serde_json::Value::as_bool);
    let any_known = ["is_datacenter", "is_proxy", "is_vpn", "is_tor", "is_mobile"]
        .iter()
        .any(|k| b(k).is_some());
    // 判定顺序照 v3 `ipqueryEgress`：机房 > 代理/VPN/Tor > 移动 > 家宽
    let class = if !any_known {
        ExitClass::Unknown
    } else if b("is_datacenter") == Some(true) {
        ExitClass::Datacenter
    } else if [b("is_proxy"), b("is_vpn"), b("is_tor")].contains(&Some(true)) {
        ExitClass::Proxy
    } else if b("is_mobile") == Some(true) {
        ExitClass::Mobile
    } else {
        ExitClass::Residential
    };
    let isp = v.get("isp").cloned().unwrap_or(serde_json::Value::Null);
    let loc = v.get("location").cloned().unwrap_or(serde_json::Value::Null);
    Some((
        class,
        ExitInfo {
            ip: Some(ip),
            asn: isp.get("asn").and_then(as_asn),
            org: isp.get("org").or_else(|| isp.get("isp")).and_then(|x| x.as_str()).map(str::to_string),
            country: loc.get("country").and_then(|x| x.as_str()).map(str::to_string),
            city: loc.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

pub fn classify_ipinfo(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let org = v.get("org").and_then(|x| x.as_str()).map(str::to_string);
    Some((
        ExitClass::Unknown, // 匿名层没有类型字段
        ExitInfo {
            ip: Some(ip),
            asn: org.as_deref().and_then(|o| o.strip_prefix("AS")).and_then(|o| {
                o.split_whitespace().next().and_then(|n| n.parse().ok())
            }),
            org,
            country: v.get("country").and_then(|x| x.as_str()).map(str::to_string),
            city: v.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

/// `asn` 在 ippure 是数字、在 ipquery 是 `"AS33667"` 字符串
fn as_asn(v: &serde_json::Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_str()?.trim_start_matches("AS").parse().ok())
}

pub fn cross_class(votes: &[ExitClass]) -> ExitClass {
    let mut best = (ExitClass::Unknown, 0usize);
    for (i, v) in votes.iter().enumerate() {
        if *v == ExitClass::Unknown {
            continue; // Unknown 不投票
        }
        let n = votes.iter().skip(i).filter(|x| *x == v).count();
        if n > best.1 {
            best = (*v, n); // 平票时先出现的胜出（数据源顺序即可信度顺序）
        }
    }
    best.0
}

pub fn derive_ports_allowed(ports: &BTreeMap<u16, String>, auth_failed: bool) -> Option<Vec<u16>> {
    if auth_failed || ports.is_empty() {
        return None;
    }
    if ports.values().any(|v| v == "unreachable") {
        return None; // 抖动不该学成白名单
    }
    let open: Vec<u16> = ports.iter().filter(|(_, v)| *v == "open").map(|(k, _)| *k).collect();
    if open.len() == ports.len() {
        return None; // 全通 = 不限
    }
    // 连基准端口都不通 → 不是端口策略问题，别写空白名单（会把所有端口判成直连）
    if BASE_PORTS.iter().any(|p| !open.contains(p)) {
        return None;
    }
    Some(open)
}

pub fn fold_sources(
    results: &[Option<Result<HttpProbe, ProbeError>>],
) -> (ExitClass, ExitInfo, BTreeMap<String, String>, bool, Vec<String>) {
    let mut sources = BTreeMap::new();
    let mut votes = Vec::new();
    let mut exit = ExitInfo::default();
    let mut auth_failed = false;
    let mut notes = Vec::new();
    for (i, (name, _)) in EXIT_SOURCES.iter().enumerate() {
        let (class, detail) = match results.get(i).and_then(|x| x.as_ref()) {
            // fanout 的 None = 该项的阻塞任务 panic 了
            None => (ExitClass::Unknown, "probe_panicked".to_string()),
            Some(Err(ProbeError::AuthFailed)) => {
                auth_failed = true;
                (ExitClass::Unknown, "auth_failed".to_string())
            }
            Some(Err(e)) => (ExitClass::Unknown, format!("error:{e}")),
            Some(Ok(hp)) if is_cloudflare_challenge(hp) => {
                // spec §5.2：挑战页是「未知」，不是失败
                (ExitClass::Unknown, "cloudflare_challenge".to_string())
            }
            Some(Ok(hp)) => match serde_json::from_str::<serde_json::Value>(&hp.body) {
                Err(_) => (ExitClass::Unknown, format!("non_json:{}", hp.status)),
                Ok(v) => {
                    let parsed = match *name {
                        "ippure" => classify_ippure(&v),
                        "ipquery" => classify_ipquery(&v),
                        _ => classify_ipinfo(&v),
                    };
                    match parsed {
                        None => (ExitClass::Unknown, "no_ip".to_string()),
                        Some((c, e)) => {
                            // 先出现的源填空缺字段（EXIT_SOURCES 顺序 = 可信度顺序）
                            if exit.ip.is_none() {
                                exit = e;
                            }
                            (c, c.label().to_string())
                        }
                    }
                }
            },
        };
        sources.insert((*name).to_string(), detail);
        votes.push(class);
    }
    if exit.ip.is_none() {
        notes.push("三个出口画像数据源都没给出 IP，出口分类未知".into());
    }
    (cross_class(&votes), exit, sources, auth_failed, notes)
}

pub fn probe_exit(
    p: &dyn Prober,
    up: &Upstream,
) -> (ExitClass, ExitInfo, BTreeMap<String, String>, Vec<String>) {
    // 顺序跑（调用方已经在 spawn_blocking 里），结果交给与 `run` 同一个 fold_sources
    let results: Vec<Option<Result<HttpProbe, ProbeError>>> =
        EXIT_SOURCES.iter().map(|(_, url)| Some(p.get(up, url))).collect();
    let (class, exit, sources, _auth_failed, notes) = fold_sources(&results);
    (class, exit, sources, notes)
}

pub fn sorry_of(r: Option<&Result<HttpProbe, ProbeError>>) -> Option<bool> {
    match r {
        // 正文含 /sorry/ 或状态码 429 都算「被判机器人」（v3 同判据）
        Some(Ok(hp)) => Some(hp.body.contains("/sorry/") || hp.status == 429),
        // 探不到就不下结论：把抖动显示成「被判机器人」会误导运维换上游
        _ => None,
    }
}

pub fn google_sorry(p: &dyn Prober, up: &Upstream) -> Option<bool> {
    sorry_of(Some(&p.get(up, GOOGLE_SEARCH_URL)))
}

pub fn to_verified(e: &ExitInfo, at: &str) -> Option<Verified> {
    // 没 IP 就不写：`verified` 的语义是「上次确认到的出口」，空 IP 毫无意义
    Some(Verified {
        ip: e.ip.clone()?,
        asn: e.asn,
        org: e.org.clone(),
        country: e.country.clone(),
        at: at.to_string(),
    })
}
```

- [ ] **Step 4: 实现 `run` 与 `run_and_store`**

```rust
pub async fn run(p: Arc<dyn Prober>, up: &Upstream, now: OffsetDateTime) -> CheckReport {
    let mut notes = Vec::new();
    let mut auth_failed = false;

    // ① 三源出口画像 + ② Google /sorry/：四个 GET，一起 fanout
    let mut urls: Vec<String> = EXIT_SOURCES.iter().map(|(_, u)| (*u).to_string()).collect();
    urls.push(GOOGLE_SEARCH_URL.to_string());
    let (pp, u2) = (p.clone(), up.clone());
    let gets = super::fanout(urls.clone(), move |url| pp.get(&u2, &url)).await;

    // 三源交叉：判定逻辑只有 `fold_sources` 一份（`probe_exit` 走的也是它）
    let (class, exit, sources, src_auth_failed, src_notes) = fold_sources(&gets[..3]);
    if src_auth_failed {
        auth_failed = true;
    }
    notes.extend(src_notes);

    let google_sorry = sorry_of(gets[3].as_ref());
    if matches!(&gets[3], Some(Err(ProbeError::AuthFailed))) {
        auth_failed = true;
    }

    // ③ AI 与支付可达性：CONNECT 到 443 即算可达（TLS 之后的业务码不是我们的判据）
    let hosts: Vec<String> = AI_HOSTS.iter().chain(PAY_HOSTS.iter()).map(|h| (*h).to_string()).collect();
    let (pp, u2) = (p.clone(), up.clone());
    let hv = super::fanout(hosts.clone(), move |h| pp.connect(&u2, &h, 443)).await;
    let (mut ai, mut payments) = (BTreeMap::new(), BTreeMap::new());
    for (i, h) in hosts.iter().enumerate() {
        let open = matches!(hv[i], Some(ConnectVerdict::Open));
        if matches!(hv[i], Some(ConnectVerdict::AuthFailed)) {
            auth_failed = true;
        }
        if AI_HOSTS.contains(&h.as_str()) { ai.insert(h.clone(), open); } else { payments.insert(h.clone(), open); }
    }

    // ④ 固定端口集：目标主机固定用 www.google.com，只看隧道能不能开（端口策略与目标无关）
    let plist: Vec<u16> = BASE_PORTS.iter().chain(PROBE_PORTS.iter()).copied().collect();
    let (pp, u2) = (p.clone(), up.clone());
    let pv = super::fanout(plist.clone(), move |port| pp.connect(&u2, "www.google.com", port)).await;
    let mut ports = BTreeMap::new();
    for (i, port) in plist.iter().enumerate() {
        let label = match &pv[i] {
            None => "unreachable".to_string(),
            Some(v) => {
                if *v == ConnectVerdict::AuthFailed { auth_failed = true; }
                v.label()
            }
        };
        ports.insert(*port, label);
    }

    // ⑤ SOCKS5 UDP ASSOCIATE
    let (pp, u2) = (p.clone(), up.clone());
    let udp = tokio::task::spawn_blocking(move || pp.udp_associate(&u2)).await;
    let udp_associate = match udp {
        Ok(Ok(v)) => Some(v),
        Ok(Err(ProbeError::AuthFailed)) => { auth_failed = true; None }
        _ => None,
    };

    if auth_failed {
        notes.push("本轮出现 407 / SOCKS5 认证被拒：上游凭据可能已失效，端口白名单不予采信".into());
    }
    let ports_allowed = derive_ports_allowed(&ports, auth_failed);
    CheckReport {
        at: crate::util::fmt_rfc3339(now),
        upstream_id: up.id,
        class,
        class_label: class.label().to_string(),
        exit,
        sources,
        google_sorry,
        ai,
        payments,
        ports,
        udp_associate,
        ports_allowed,
        auth_failed,
        notes,
    }
}

pub async fn run_and_store(ctx: &DaemonCtx, p: Arc<dyn Prober>, id: Uuid) -> anyhow::Result<CheckReport> {
    let s = ctx.store.read().await;
    let g = state::group_of(&s);
    let up = g
        .upstreams
        .iter()
        .find(|u| u.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("上游不存在"))?;
    drop(s);
    let now = ctx.host.now();
    state::update(&ctx.runtime, |r| {
        r.checking = Some(state::Checking { upstream_id: id, started_at: crate::util::fmt_rfc3339(now) });
    })
    .await;
    let report = run(p, &up, now).await;
    // 体检学到的两项写回 state（会触发一次对账；ports_allowed 变了 relay 规则就得改，本来就要重启）
    let (allowed, verified) = (report.ports_allowed.clone(), to_verified(&report.exit, &report.at));
    state::update_group(&ctx.store, &ctx.bus, |g| {
        if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == id) {
            if !report.auth_failed {
                u.ports_allowed = allowed;
            }
            if let Some(v) = verified {
                u.verified = Some(v);
            }
        }
    })
    .await?;
    let json = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    state::update(&ctx.runtime, |r| {
        r.checks.insert(id.to_string(), json);
        r.checking = None;
        if report.auth_failed {
            state::push_alert(r, format!("上游 {} 凭据失效（407），请更新凭据", up.name));
        }
    })
    .await;
    Ok(report)
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::check && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 10 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/check.rs
git commit -m "feat(resi): 出口体检（三方交叉分类、/sorry/、AI/支付、端口集、UDP ASSOCIATE、ports_allowed）"
```

---

### Task 7: 上游增删改（四种格式、类型自动探测、`verified`）与总开关/模式/关键字

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/upstream.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{state, check, clash, proxy, EXIT_IP_HOST, EXIT_IP_URL, MAX_UPSTREAMS}`；`crate::modules::residential::proxy::{ProbeError, Prober}`；`crate::reconcile::DaemonCtx`；`crate::util::fmt_rfc3339`；`bui_schema::parse::{upstream_url, ParseError, UpstreamInput}`；`bui_schema::model::{ResiMode, Upstream, UpstreamKind}`
- Produces:
```rust
// crate::modules::residential::upstream
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddOutcome {
    pub id: Uuid, pub name: String, pub kind: UpstreamKind,
    pub exit_ip: Option<String>, pub isp: Option<String>, pub class_label: String,
}
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("{0}")] Parse(#[from] ParseError),
    #[error("SOCKS5 与 HTTP 两种协议都连不上 ({host}:{port}) —— 请核对凭据与端口（供应商的 SOCKS5 与 HTTP 端口通常不同）")]
    Unverifiable { host: String, port: u16 },
    #[error("上游凭据失效（407 / SOCKS5 认证被拒）")]
    AuthFailed,
    #[error("出口 IP 与本机相同（{0}），代理未生效")]
    NotProxied(String),
    #[error("代理节点池已满（上限 {MAX_UPSTREAMS} 个）")]
    PoolFull,
    #[error("未找到匹配的上游")]
    NotFound,
    #[error("代理节点池为空，请先添加至少 1 个住宅上游")]
    PoolEmpty,
    #[error(transparent)] Other(#[from] anyhow::Error),
}

/// 上游定位：面板用 `host:port`（v3 的 `DELETE /api/residential/urls/<host:port>`），
/// 规范端点与 CLI 用 id
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSel { Id(Uuid), HostPort { host: String, port: u16 } }
impl UpstreamSel { pub fn parse_host_port(s: &str) -> Option<Self>; }

/// 类型自动探测：`socks5` → `http`，**整轮重试一次**（v3.6.2 R12：实测有效的端口也会偶发
/// 抽风，同一行立刻重试就成）。`kind` 已指定时只试那一种、不重试。
/// 全都失败、且 `get` 没能识别出凭据失效时，再对每个候选类型用
/// [`crate::modules::residential::proxy::confirm_auth_failure`] 补判一次
/// [`EXIT_IP_HOST`]`:443`——`get` 在 https 目标上判不出 407（见 T3 `confirm_auth_failure`
/// 的注释），不补判的话真机上凭据写错只会得到 `Unverifiable`（「协议都连不上」）这条
/// 误导性文案，运维会去改端口而不是换凭据。
pub fn detect_kind(p: &dyn Prober, probe: &Upstream, want: Option<UpstreamKind>)
    -> Result<(UpstreamKind, String), UpstreamError>;                   // (可用类型, 出口 IP)
/// 加一条上游：解析 → 类型探测 → 出口画像 → 写 state。**凭据只进 state**，
/// 日志与错误信息里一律脱敏。同 `host:port` 是**覆盖**语义（v3 同），所以覆盖时
/// 不占新名额、池满也允许，并且**沿用既有条目的 `id`**（绝不换新 uuid）：`id` 是
/// 全模块的运行时主键（契约决策 §C），换掉它会让 `runtime.selected_upstream_id`、
/// `runtime.health[id]`、`runtime.checks[id]` 与 `state.blacklist.auto[].upstream_id`
/// 里的旧 uuid 一起悬空——旧 `auto` 条目既不会被 relay 渲染（按 selected 过滤）也永远
/// 不进每日复核、`remove_auto` 还删不到；`replay_loop` 因 `tag_of` 为 `None` 跳过重放，
/// 健康 streak 与 24h 成功率从零重来。
pub async fn add(ctx: &DaemonCtx, p: Arc<dyn Prober>, raw: &str) -> Result<AddOutcome, UpstreamError>;
/// 删一条上游；删掉最后一条时顺带 `enabled = false`（v3 `enable --remove` 同语义）
pub async fn remove(ctx: &DaemonCtx, sel: &UpstreamSel) -> Result<(), UpstreamError>;
/// 总开关。开启要求池非空（v3 `POST /api/residential/enable` 的 400 分支）
pub async fn set_enabled(ctx: &DaemonCtx, on: bool) -> Result<(), UpstreamError>;
/// global / split（v3 `POST /api/residential/global`）
pub async fn set_mode(ctx: &DaemonCtx, global: bool) -> Result<(), UpstreamError>;
/// 分流关键字：`None` = 回到跟随默认表（v3 的 `set-domains null`，R12 的语义）
pub async fn set_keywords(ctx: &DaemonCtx, kw: Option<Vec<String>>) -> Result<(), UpstreamError>;
/// 调优先级（切换目标的第一排序键）
pub async fn set_priority(ctx: &DaemonCtx, id: Uuid, priority: u32) -> Result<(), UpstreamError>;
/// `url-1..url-N` 稠密重排（v3 `add_url_to_config` 的 `to_entries|map(name:…)`）。
/// 名字**必须**跟着下标重排，否则删掉中间一条后 `url-3` 与 `resi-2` 对不上，面板表与
/// relay 成员就错位了。
pub fn renumber(ups: &mut [Upstream]);
```

**为什么 `add` 只做出口画像、不做端口集**：面板的添加请求预算是 60 秒（v3 `RESI_ADD_TIMEOUT_MS`）。类型探测最坏 2 轮 × 2 协议 × 10 秒 = 40 秒，再加三源画像就超了。所以 `add` 返回后由调用方（T10 的 handler）异步起一次 `check::run_and_store`（面板轮询 `checking` 消失即刷新，R13 §7 的「录入即探测」同语义）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;

    async fn ctx(d: &tempfile::TempDir) -> DaemonCtx {
        let mut s = sample_state();
        s.node.public_ip = "203.0.113.10".into();
        // 从空池起步，便于断言添加路径
        s.residential.groups.get_mut("default").unwrap().upstreams.clear();
        s.residential.groups.get_mut("default").unwrap().selected_upstream_id = None;
        DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: std::sync::Arc::new(FakeHost::new()),
            paths: bui_schema::paths::Paths::default_server(),
        }
    }

    fn prober_ok(exit: &str) -> std::sync::Arc<FakeProber> {
        let p = std::sync::Arc::new(FakeProber::new());
        let exit = exit.to_string();
        p.with(|i| {
            i.gets.insert(EXIT_IP_URL.into(), Ok(HttpProbe { status: 200, body: exit.clone() }));
            i.gets.insert(
                crate::modules::residential::check::EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: serde_json::json!({"ip": exit, "asn": 33667, "asOrganization": "Comcast",
                                             "country": "US", "isResidential": true}).to_string(),
                }),
            );
        });
        p
    }

    #[tokio::test]
    async fn add_accepts_all_four_paste_formats_and_records_verified() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for raw in [
            "socks5://user1:pw1@isp1.example.net:1080",
            "http://user1:pw1@isp2.example.net:10007",
            "isp3.example.net:1084:user1:pw1",
            "user1:pw1@isp4.example.net:1080",
        ] {
            add(&c, p.clone(), raw).await.unwrap();
        }
        let g = rstate::group_of(&c.store.read().await);
        assert_eq!(g.upstreams.len(), 4);
        assert_eq!(g.upstreams[0].kind, UpstreamKind::Socks5, "scheme 指定了就不探测");
        assert_eq!(g.upstreams[1].kind, UpstreamKind::Http);
        assert_eq!(g.upstreams[2].host, "isp3.example.net");
        assert_eq!(g.upstreams[2].password, "pw1");
        assert_eq!(
            g.upstreams.iter().map(|u| u.name.clone()).collect::<Vec<_>>(),
            vec!["url-1", "url-2", "url-3", "url-4"]
        );
        let v = g.upstreams[0].verified.clone().unwrap();
        assert_eq!((v.ip.as_str(), v.asn, v.org.as_deref()), ("198.51.100.7", Some(33667), Some("Comcast")));
        assert!(g.enabled, "第一条添加成功即启用（v3 save_config true）");
        assert_eq!(g.selected_upstream_id, Some(g.upstreams[0].id), "首条成为落点");
    }

    #[tokio::test]
    async fn auto_detection_falls_back_to_http_and_retries_the_whole_round_once() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        // 没写 scheme → auto：先 SOCKS5 再 HTTP，整轮重试一次
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            // FakeProber 按 (url) 查表，不区分上游类型，所以用调用流水断言顺序与次数
            i.gets.insert(EXIT_IP_URL.into(), Err("connection refused".into()));
        });
        let e = add(&c, p.clone(), "user1:pw1@isp.example.net:1080").await.unwrap_err();
        assert!(matches!(e, UpstreamError::Unverifiable { .. }), "实际 {e}");
        assert_eq!(
            p.calls().iter().filter(|c| c.starts_with("get:")).count(),
            4,
            "socks5 / http 各两轮：整轮重试一次（v3.6.2 R12）"
        );
        assert!(rstate::group_of(&c.store.read().await).upstreams.is_empty(), "失败不写 state");
    }

    #[tokio::test]
    async fn add_rejects_a_407_and_an_exit_ip_equal_to_the_vps() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets.insert(EXIT_IP_URL.into(), Err("__auth_failed__".into()));
        });
        assert!(matches!(
            add(&c, p, "http://user1:pw1@isp.example.net:10007").await,
            Err(UpstreamError::AuthFailed)
        ));
        // 出口 IP == 本机公网 IP ⇒ 代理没生效（v3 verify 的那条判据）
        let p2 = prober_ok("203.0.113.10");
        assert!(matches!(
            add(&c, p2, "http://user1:pw1@isp.example.net:10007").await,
            Err(UpstreamError::NotProxied(_))
        ));
    }

    #[tokio::test]
    async fn add_rejects_bad_pastes_with_the_parse_message_and_enforces_the_pool_cap() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        assert!(matches!(add(&c, p.clone(), "https://u:p@h:1").await, Err(UpstreamError::Parse(_))));
        assert!(matches!(add(&c, p.clone(), "socks5://u@h:1").await, Err(UpstreamError::Parse(_))));
        for i in 0..MAX_UPSTREAMS {
            add(&c, p.clone(), &format!("http://user1:pw1@isp{i}.example.net:10007")).await.unwrap();
        }
        assert!(matches!(
            add(&c, p.clone(), "http://user1:pw1@overflow.example.net:10007").await,
            Err(UpstreamError::PoolFull)
        ));
        // 池满时「更新一条已有上游」不该被 400 拒：同 host:port 是覆盖，不占新名额
        add(&c, p, "socks5://user1:pw1@isp0.example.net:10007").await.unwrap();
        let g = rstate::group_of(&c.store.read().await);
        assert_eq!(g.upstreams.len(), MAX_UPSTREAMS, "覆盖不增长");
        assert_eq!(
            g.upstreams.iter().find(|u| u.host == "isp0.example.net").unwrap().kind,
            UpstreamKind::Socks5,
            "覆盖后 kind 跟着新粘贴的那一行走"
        );
    }

    #[tokio::test]
    async fn overwriting_the_same_host_port_keeps_the_id_so_runtime_and_blacklist_keys_survive() {
        // 覆盖必须沿用既有 id（契约决策 §C：id 是运行时主键）。换新 uuid 的话，
        // 下面这四处引用会一起悬空：state.selected_upstream_id / blacklist.auto[].upstream_id
        // / runtime.selected_upstream_id / runtime.health[id]。
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let first = add(&c, p.clone(), "http://user1:pw1@isp1.example.net:10007").await.unwrap();
        rstate::update_group(&c.store, &c.bus, |g| {
            g.blacklist.auto.push(bui_schema::model::AutoEntry {
                upstream_id: first.id,
                rule: bui_schema::model::Rule::DomainSuffix("gateway.icloud.com".into()),
                hits: 9, confirmed_at: "2026-09-12T00:00:00Z".into(),
                last_verified_at: "2026-09-12T00:00:00Z".into(), passes: 0,
            });
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(first.id);
            r.health.insert(first.id.to_string(), rstate::HealthState::default());
        })
        .await;

        // 同 host:port、换协议与凭据重新粘贴一次
        let again = add(&c, p, "socks5://user2:pw2@isp1.example.net:10007").await.unwrap();
        assert_eq!(again.id, first.id, "覆盖沿用既有 id，不换 uuid");
        let g = rstate::group_of(&c.store.read().await);
        assert_eq!(g.upstreams.len(), 1, "覆盖不增长");
        assert_eq!(g.upstreams[0].id, first.id);
        assert_eq!(g.upstreams[0].kind, UpstreamKind::Socks5, "类型跟着新粘贴的那一行走");
        assert_eq!(g.upstreams[0].password, "pw2", "凭据也跟着走");
        assert_eq!(g.upstreams[0].name, "url-1");
        assert_eq!(g.selected_upstream_id, Some(first.id), "state 落点没被改成悬空 uuid");
        assert_eq!(
            g.blacklist.auto.iter().map(|a| a.upstream_id).collect::<Vec<_>>(),
            vec![first.id],
            "该上游学到的 auto 条目仍挂在同一个 id 上（换 uuid 会让它永远不进复核、也删不到）"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.selected_upstream_id, Some(first.id), "replay_loop 还能重放到它");
        assert!(r.health.contains_key(&first.id.to_string()), "健康 streak 与 24h 成功率不清零");
    }

    #[tokio::test]
    async fn remove_by_host_port_renumbers_and_disables_when_the_pool_empties() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for i in 1..=3 {
            add(&c, p.clone(), &format!("http://user1:pw1@isp{i}.example.net:10007")).await.unwrap();
        }
        remove(&c, &UpstreamSel::parse_host_port("isp2.example.net:10007").unwrap()).await.unwrap();
        let g = rstate::group_of(&c.store.read().await);
        assert_eq!(
            g.upstreams.iter().map(|u| (u.host.clone(), u.name.clone())).collect::<Vec<_>>(),
            vec![
                ("isp1.example.net".to_string(), "url-1".to_string()),
                ("isp3.example.net".to_string(), "url-2".to_string()),
            ],
            "名字跟着下标稠密重排，否则 url-N 与 resi-N 会错位"
        );
        assert!(matches!(
            remove(&c, &UpstreamSel::parse_host_port("nope.example.net:1").unwrap()).await,
            Err(UpstreamError::NotFound)
        ));
        for host in ["isp1.example.net", "isp3.example.net"] {
            remove(&c, &UpstreamSel::parse_host_port(&format!("{host}:10007")).unwrap()).await.unwrap();
        }
        let g = rstate::group_of(&c.store.read().await);
        assert!(g.upstreams.is_empty());
        assert!(!g.enabled, "最后一条被移除即关总开关（relay 回落 fail-open 直连）");
        assert_eq!(g.selected_upstream_id, None);
    }

    #[tokio::test]
    async fn remove_also_drops_that_upstreams_auto_blacklist_and_runtime_traces() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let a = add(&c, p.clone(), "http://user1:pw1@isp1.example.net:10007").await.unwrap();
        add(&c, p, "http://user1:pw1@isp2.example.net:10007").await.unwrap();
        rstate::update_group(&c.store, &c.bus, |g| {
            g.blacklist.auto.push(bui_schema::model::AutoEntry {
                upstream_id: a.id,
                rule: bui_schema::model::Rule::DomainSuffix("gateway.icloud.com".into()),
                hits: 9, confirmed_at: "2026-09-12T00:00:00Z".into(),
                last_verified_at: "2026-09-12T00:00:00Z".into(), passes: 0,
            });
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.checks.insert(a.id.to_string(), serde_json::json!({"x": 1}));
            // 运行时主键是 uuid（契约决策 §C），这两处也必须跟着清
            r.health.insert(a.id.to_string(), rstate::HealthState::default());
            r.selected_upstream_id = Some(a.id);
            r.selected_pending_persist = true;
        })
        .await;
        remove(&c, &UpstreamSel::Id(a.id)).await.unwrap();
        let g = rstate::group_of(&c.store.read().await);
        assert!(g.blacklist.auto.is_empty(), "该上游的 auto 条目一起删（不然永远没人复核它）");
        let r = rstate::read(&c.runtime).await;
        assert!(!r.checks.contains_key(&a.id.to_string()));
        assert!(!r.health.contains_key(&a.id.to_string()), "健康 streak 跟着 uuid 一起清");
        assert_eq!(r.selected_upstream_id, None, "别让 replay_loop 去重放一条已删的上游");
        assert!(!r.selected_pending_persist);
    }

    #[tokio::test]
    async fn toggles_follow_the_v3_semantics() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        // 空池不许开总开关（v3 的 400 分支）
        assert!(matches!(set_enabled(&c, true).await, Err(UpstreamError::PoolEmpty)));
        let p = prober_ok("198.51.100.7");
        let a = add(&c, p, "http://user1:pw1@isp1.example.net:10007").await.unwrap();
        set_mode(&c, true).await.unwrap();
        assert_eq!(rstate::group_of(&c.store.read().await).mode, ResiMode::Global);
        set_keywords(&c, Some(vec!["openai.com".into(), " ".into(), "openai.com".into()])).await.unwrap();
        assert_eq!(
            rstate::group_of(&c.store.read().await).keywords,
            Some(vec!["openai.com".to_string()]),
            "去空白、去重"
        );
        // None = 回到跟随默认表（R12：不把当时的默认表固化成自定义）
        set_keywords(&c, None).await.unwrap();
        assert_eq!(rstate::group_of(&c.store.read().await).keywords, None);
        set_priority(&c, a.id, 5).await.unwrap();
        assert_eq!(rstate::group_of(&c.store.read().await).upstreams[0].priority, 5);
        set_enabled(&c, false).await.unwrap();
        assert!(!rstate::group_of(&c.store.read().await).enabled);
    }
}
```
`FakeProber` 里 `Err("__auth_failed__")` 映射到 `ProbeError::AuthFailed`（其余字符串映射到 `Unreachable`）——这条约定写在 Task 3 的 `FakeProber::get` 实现里，本任务的测试依赖它。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::upstream`
Expected: 编译失败，`cannot find function `add``。

- [ ] **Step 3: 实现类型探测与 `add`**

```rust
pub fn renumber(ups: &mut [Upstream]) {
    for (i, u) in ups.iter_mut().enumerate() {
        u.name = format!("url-{}", i + 1);
    }
}

impl UpstreamSel {
    pub fn parse_host_port(s: &str) -> Option<Self> {
        let (host, port) = s.rsplit_once(':')?;
        (!host.is_empty()).then_some(())?;
        Some(Self::HostPort { host: host.to_string(), port: port.parse().ok()? })
    }
}

pub fn detect_kind(
    p: &dyn Prober,
    probe: &Upstream,
    want: Option<UpstreamKind>,
) -> Result<(UpstreamKind, String), UpstreamError> {
    // v3.6.2 R12：auto 整轮重试一次（实测有效的端口也会偶发抽风，
    // 同一行立刻重试就成，不该让用户以为凭据写错了）
    let (candidates, rounds): (Vec<UpstreamKind>, usize) = match want {
        Some(k) => (vec![k], 1),
        None => (vec![UpstreamKind::Socks5, UpstreamKind::Http], 2),
    };
    let mut auth_failed = false;
    for _round in 0..rounds {
        for kind in &candidates {
            let mut probe = probe.clone();
            probe.kind = *kind;
            match p.get(&probe, EXIT_IP_URL) {
                Ok(hp) => {
                    let ip = hp.body.trim().to_string();
                    if !ip.is_empty() {
                        return Ok((*kind, ip));
                    }
                }
                Err(ProbeError::AuthFailed) => auth_failed = true,
                Err(_) => {}
            }
        }
    }
    if !auth_failed {
        // `get` 分不出「凭据失效」与「连不上」：https 目标经 HTTP 上游走 CONNECT 隧道，
        // 407 发生在隧道建立阶段，reqwest 只给一个 Err（T3 `confirm_auth_failure` 的注释）。
        // 逐个候选类型补判一次，否则真机上凭据写错报的是 Unverifiable（「端口/协议不对」），
        // 把运维引到错误的排查方向。
        for kind in &candidates {
            let mut probe = probe.clone();
            probe.kind = *kind;
            if proxy::confirm_auth_failure(p, &probe, EXIT_IP_HOST) {
                auth_failed = true;
                break;
            }
        }
    }
    if auth_failed {
        return Err(UpstreamError::AuthFailed);
    }
    Err(UpstreamError::Unverifiable { host: probe.host.clone(), port: probe.port })
}

pub async fn add(ctx: &DaemonCtx, p: Arc<dyn Prober>, raw: &str) -> Result<AddOutcome, UpstreamError> {
    let input: UpstreamInput = upstream_url(raw)?;
    let s = ctx.store.read().await;
    let g = state::group_of(&s);
    // 同 host:port 是**覆盖**既有条目（下面 update_group 里先 retain 再 push），不占新名额：
    // 池满时改一条已有上游的凭据/类型不该被 400 拒
    let existing_id = g
        .upstreams
        .iter()
        .find(|u| u.host == input.host && u.port == input.port)
        .map(|u| u.id);
    if existing_id.is_none() && g.upstreams.len() >= MAX_UPSTREAMS {
        return Err(UpstreamError::PoolFull);
    }
    let vps_ip = s.node.public_ip.clone();
    drop(s);

    // **覆盖时沿用既有 id**：id 是全模块的运行时主键（契约决策 §C）。换新 uuid 会让
    // runtime.selected_upstream_id / runtime.health[id] / runtime.checks[id] 与
    // state.blacklist.auto[].upstream_id 里的旧 uuid 全部悬空：旧 auto 条目不再被渲染、
    // 永远不进复核、remove_auto 也删不到，replay_loop 因 tag_of 为 None 跳过重放。
    let id = existing_id.unwrap_or_else(Uuid::new_v4);
    let mut up = Upstream {
        id,
        name: String::new(), // renumber 里填
        kind: input.kind.unwrap_or(UpstreamKind::Socks5),
        host: input.host,
        port: input.port,
        username: input.username,
        password: input.password,
        priority: 100,
        provider: None,
        region: None,
        ports_allowed: None,
        verified: None,
    };

    let (kind, exit_ip) = {
        let (pp, probe, want) = (p.clone(), up.clone(), input.kind);
        tokio::task::spawn_blocking(move || detect_kind(pp.as_ref(), &probe, want))
            .await
            .map_err(|e| UpstreamError::Other(anyhow::anyhow!(e)))??
    };
    if exit_ip == vps_ip {
        // v3 verify 的那条判据：出口与本机相同说明流量根本没经代理
        return Err(UpstreamError::NotProxied(vps_ip));
    }
    up.kind = kind;

    // 出口画像（三源交叉）；失败不阻塞添加，verified 留空由后续体检补
    let now = ctx.host.now();
    let (class, exit, sources, _notes) = {
        let (pp, u2) = (p.clone(), up.clone());
        tokio::task::spawn_blocking(move || check::probe_exit(pp.as_ref(), &u2))
            .await
            .map_err(|e| UpstreamError::Other(anyhow::anyhow!(e)))?
    };
    let at = crate::util::fmt_rfc3339(now);
    up.verified = check::to_verified(&exit, &at);
    up.region = exit.country.clone();
    tracing::info!(
        upstream = %crate::modules::residential::proxy::proxy_url(&up),
        kind = ?kind, class = ?class, sources = ?sources, "新增住宅上游"
    );

    let isp = exit
        .asn
        .map(|a| format!("AS{a}"))
        .into_iter()
        .chain(exit.org.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let (name, added) = {
        let up2 = up.clone();
        state::update_group(&ctx.store, &ctx.bus, move |g| {
            // 同 host:port 视为同一上游，覆盖（v3 add_url_to_config 的 map(select(…)) 同语义）
            g.upstreams.retain(|u| !(u.host == up2.host && u.port == up2.port));
            g.upstreams.push(up2);
            renumber(&mut g.upstreams);
            g.enabled = true;
            if g.selected_upstream_id.is_none() {
                g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
            }
        })
        .await?;
        let g = state::group_of(&ctx.store.read().await);
        let added = g.upstreams.iter().find(|u| u.id == id).cloned().ok_or(UpstreamError::NotFound)?;
        (added.name.clone(), added)
    };
    Ok(AddOutcome {
        id: added.id,
        name,
        kind,
        exit_ip: Some(exit_ip),
        isp: (!isp.is_empty()).then_some(isp),
        class_label: class.label().to_string(),
    })
}
```

- [ ] **Step 4: 实现 `remove` 与四个开关**

```rust
pub async fn remove(ctx: &DaemonCtx, sel: &UpstreamSel) -> Result<(), UpstreamError> {
    let g = state::group_of(&ctx.store.read().await);
    let target = g
        .upstreams
        .iter()
        .find(|u| match sel {
            UpstreamSel::Id(id) => u.id == *id,
            UpstreamSel::HostPort { host, port } => u.host == *host && u.port == *port,
        })
        .cloned()
        .ok_or(UpstreamError::NotFound)?;
    let id = target.id;
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        g.upstreams.retain(|u| u.id != id);
        renumber(&mut g.upstreams);
        // 该上游的 auto 条目一起删：留着没人复核，还会被 render/relay 的
        // `upstream_id == selected` 过滤悄悄忽略
        g.blacklist.auto.retain(|e| e.upstream_id != id);
        if g.selected_upstream_id == Some(id) {
            g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        }
        if g.upstreams.is_empty() {
            g.enabled = false; // relay 回落 fail-open 直连（v3 `enable --remove` 同语义）
            g.selected_upstream_id = None;
        }
    })
    .await?;
    state::update(&ctx.runtime, |r| {
        r.checks.remove(&id.to_string());
        r.candidates.retain(|_, c| c.upstream_id != id);
        r.pending.retain(|e| e.upstream_id != id);
        // 运行时主键是 uuid（契约决策 §C）：健康 streak 与「当前生效」都得跟着清，
        // 否则 replay_loop 会去重放一条已经不存在的上游，24h 成功率也会留着僵尸样本
        r.health.remove(&id.to_string());
        if r.selected_upstream_id == Some(id) {
            r.selected_upstream_id = None;
            r.selected_pending_persist = false;
        }
    })
    .await;
    Ok(())
}

pub async fn set_enabled(ctx: &DaemonCtx, on: bool) -> Result<(), UpstreamError> {
    if on && state::group_of(&ctx.store.read().await).upstreams.is_empty() {
        return Err(UpstreamError::PoolEmpty);
    }
    state::update_group(&ctx.store, &ctx.bus, |g| g.enabled = on).await?;
    Ok(())
}

pub async fn set_mode(ctx: &DaemonCtx, global: bool) -> Result<(), UpstreamError> {
    let mode = if global { ResiMode::Global } else { ResiMode::Split };
    state::update_group(&ctx.store, &ctx.bus, |g| g.mode = mode).await?;
    Ok(())
}

pub async fn set_keywords(ctx: &DaemonCtx, kw: Option<Vec<String>>) -> Result<(), UpstreamError> {
    // None / 空列表 = 回到跟随 DEFAULT_KEYWORDS（R12：不把当时的默认表固化成自定义，
    // 否则后续版本扩充默认表这台机器永远跟不上）
    let cleaned = kw.and_then(|list| {
        let mut v: Vec<String> = list.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        v.dedup_by(|a, b| a == b);
        let mut seen = std::collections::BTreeSet::new();
        v.retain(|s| seen.insert(s.clone()));
        (!v.is_empty()).then_some(v)
    });
    state::update_group(&ctx.store, &ctx.bus, |g| g.keywords = cleaned).await?;
    Ok(())
}

pub async fn set_priority(ctx: &DaemonCtx, id: Uuid, priority: u32) -> Result<(), UpstreamError> {
    let g = state::group_of(&ctx.store.read().await);
    if !g.upstreams.iter().any(|u| u.id == id) {
        return Err(UpstreamError::NotFound);
    }
    state::update_group(&ctx.store, &ctx.bus, |g| {
        if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == id) {
            u.priority = priority;
        }
    })
    .await?;
    Ok(())
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::upstream && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 8 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/upstream.rs
git commit -m "feat(resi): 上游增删（四种格式、类型自动探测整轮重试、verified）与总开关/模式/关键字/优先级"
```

---

### Task 8: 健康巡检、切换决策与 relay 重启后的选择重放

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/health.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{clash, fanout, proxy, state, HEALTH_INTERVAL_SECS, HEALTH_PROBE_HOST, HEALTH_PROBE_URL, HEALTH_TRIES, SWITCH_MIN_INTERVAL_SECS}`；`crate::modules::residential::proxy::{ProbeError, Prober}`；`crate::modules::residential::clash::{Clash, ClashError}`；`crate::api::Event`；`crate::reconcile::DaemonCtx`；`crate::util::{fmt_rfc3339, parse_rfc3339}`
- Produces:
```rust
// crate::modules::residential::health
/// 一轮巡检的结论（返回给测试与 `/api/residential/health`）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoundOutcome {
    /// 本轮判定健康的成员 tag，顺序与池一致
    pub healthy: Vec<String>,
    /// 本轮探测结果，`(tag, 本轮是否达标)`
    pub probed: Vec<(String, bool)>,
    /// 因健康决策**切换**到的 tag（`None` = 没切）
    pub switched_to: Option<String>,
    /// 因 relay 重启后 selector 回落、把 `runtime.selected_upstream_id` **重放**回去的
    /// tag（`None` = 没重放）。与 `switched_to` 分开记：重放不是一次新的选择，它不算
    /// 切换、不吃 60 秒限速、不动 `last_switch_at`，测试也必须能把两者区分开
    pub replayed_to: Option<String>,
    pub notes: Vec<String>,
}

/// 一个成员一轮探测的结果。`auth_failed` 单独带出来，是因为凭据失效要**单独告警**
/// （管理员必须换凭据，不是等它自愈），而 `check_once` 是 async、不能为了补判一次
/// 就再同步调一次 `Prober`（`reqwest::blocking` 在 async 上下文会 panic）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemberProbe { pub ok: bool, pub auth_failed: bool }
/// 单成员一轮探测：对 [`HEALTH_PROBE_URL`] 最多 [`HEALTH_TRIES`] 次，任一成功即本轮健康。
/// **407 / SOCKS5 认证被拒一律算不健康**（调研 §D：凭据失效的上游会把每条连接都拒掉，
/// 它「可达」但不可用），并且立刻停止重试。
/// 全都失败、且 `get` 没能识别出凭据失效时，再用
/// [`crate::modules::residential::proxy::confirm_auth_failure`] 对
/// [`HEALTH_PROBE_HOST`]`:443` 补判一次 CONNECT——`get` 在 https 目标上判不出 407
/// （原因见 T3 `confirm_auth_failure` 的注释），不补判的话生产上「凭据失效」告警永不触发。
pub fn probe_member(p: &dyn Prober, up: &Upstream) -> MemberProbe;
/// 当前池的成员 id，顺序与 `g.upstreams` 一致（成员集比较用它，不用位置键 tag）
pub fn ids_of(g: &ResidentialGroup) -> Vec<Uuid>;
/// 切换目标：健康成员里 `priority` 最小者；同优先级按近 24h 成功率降序；再同按池内下标升序（稳定）。
/// 进出都是 **uuid**（契约决策 §C：运行时主键不用位置键 tag）
pub fn pick_target(g: &ResidentialGroup, r: &ResiRuntime, healthy: &[Uuid], now: OffsetDateTime) -> Option<Uuid>;
/// 一轮完整巡检（spec §5.3 的全部规则）：自己取「本轮开始时的成员集快照」再转调 [`check_round`]
pub async fn check_once(ctx: &DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>) -> anyhow::Result<RoundOutcome>;
/// 同上，但成员集快照由调用方给。**测试用它注入一份「本轮开始时成员集不同」的快照**来
/// 触发规则 4（探测中途改池的真实时序无法在单元测试里可靠复现）。生产只走 `check_once`。
pub async fn check_round(ctx: &DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>, before: Vec<Uuid>)
    -> anyhow::Result<RoundOutcome>;
/// 每 2 分钟一轮
pub async fn health_loop(ctx: DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>);
/// 重放 `runtime.selected_upstream_id`（spec §5.3 最后一句）。
/// **`rx` 由调用方先 `bus.subscribe()` 拿到再传进来**：broadcast 会丢弃「发送时还没有
/// 订阅者」的事件，若在本函数里 subscribe，调用方 spawn 之后立刻 send 的那条必丢
/// （current_thread 运行时下 100% 丢）。T11 的 `spawn` 与 T8 的测试都按这个顺序写。
pub async fn replay_loop(ctx: DaemonCtx, c: Arc<dyn Clash>, rx: tokio::sync::broadcast::Receiver<Event>);
/// 管理员手动切换（`POST /api/residential/select`）：只走 Clash API，不写 state（契约决策 §C）
pub async fn select_manual(ctx: &DaemonCtx, c: Arc<dyn Clash>, id: Uuid) -> anyhow::Result<String>;
```

`check_once` 的规则与执行顺序（逐条对应 spec §5.3，实现时按这个顺序写，评审按这张表核对）：

| # | 规则 | 实现 |
|---|---|---|
| 1 | 池无效（`!pool_active()`）→ 什么都不做 | 直接返回带一条 note 的空 `RoundOutcome` |
| 2 | 成员并行探测，每成员 2 次、任一成功即本轮健康 | `fanout`（并发 4）+ `probe_member` |
| 3 | 连续 2 轮不达标→不健康；连续 2 轮达标→恢复 | `state::apply_hysteresis`，并 `record_probe` 记 24h 样本 |
| 4 | **成员集在本轮内变化 → 本轮不切** | 探测前的 `ids_of(&g)` 快照（由 `check_once` 取、`check_round` 接收）与探测后的再取一次比较，不等则只写 runtime、记 note 后返回 |
| 5 | Clash API 读不到当前选择 → 本轮不切 | `c.selected()` 为 `None` 时记 note 返回（relay 没起来或旧配置） |
| 6a | **`runtime.selected_upstream_id` 是「该生效的出口」的真源**：它还在池里且健康 → 粘住；若 Clash 的 `now` 与它不一致（relay 刚被看门狗或 `/api/services/b-ui-relay/restart` 重启过，selector 回落到配置里的 `default`）→ **把 runtime 的选择重放回 Clash**，记 `replayed_to` 后返回。**绝不反过来把 `now` 写进 runtime** | `clash.select(tag_of(&g, want))`；成功记 `replayed_to`、失败记 alert。不吃规则 9 的限速、不动 `last_switch_at`（重放不是新选择；巡检间隔 120s 本身就 ≥ 60s，不会有重放风暴） |
| 6b | `runtime.selected_upstream_id` 为空（守护进程首次启动、从没切过）或已不在池里，而 Clash 的 `now` 健康 → 用 `now` 初始化 runtime 并粘住 | `healthy.contains(sel_id)` 则写一次 runtime 后返回 |
| 7 | 全不健康 → 保持并告警 | `state::push_alert` + note，不切 |
| 8 | 切换目标按 `priority` 再按 24h 成功率 | `pick_target` |
| 9 | 两次切换间隔 ≥ 60s | `last_switch_at` + `SWITCH_MIN_INTERVAL_SECS`；未到点记 note 返回 |
| 10 | 切换成功 → 记 `selected_upstream_id` / `last_switch_at`；与 state 落点不同则置 `selected_pending_persist` | `clash.select` 成功后一次 `state::update`（uuid，不是 tag） |
| 11 | 切换失败 → 只告警，不改 runtime | `ClashError` 分支记 alert |

**为什么规则 6 不采纳 Clash 的 `now`**（这是本任务最容易写反的一处）：relay 的 selector `default` 由 `state.selected_upstream_id` 渲染，所以 relay **任何**一次重启都会把实际生效的出口拽回配置落点（或池首）。而契约决策 §C 明确把「当前实际生效的出口」的真源放在 `runtime.selected_upstream_id`——自动切换与手动 `select` 都只写它、不写 state，正是为了不重启 relay。若规则 6 反过来「拿 Clash 的 `now` 回写 runtime」，那么一次看门狗重启（`watchdog.rs:141`，不发 `RelayRestarted`）或一次面板「重启数据面」就会：① 让 selector 回到池首；② 下一轮巡检把 runtime 也改成池首——管理员刚做的手动切换、上一轮的自动避障，**全被静默撤销，且面板与日志上看不出发生过什么**。所以方向必须是「runtime → Clash」的重放，只有 runtime 为空/失效时才从 Clash 初始化（规则 6b）。这条规则同时是 spec §5.3 最后一句「relay 任何重启后立即重放 `selected_upstream_id`」在**不发事件的那两条重启来路**上的唯一落点（§C 末段，裁决 D11）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::clash::FakeClash;
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    fn upstream(i: u128, priority: u32) -> Upstream {
        Upstream {
            id: Uuid::from_u128(i), name: format!("url-{i}"), kind: UpstreamKind::Http,
            host: format!("isp{i}.example.net"), port: 10007,
            username: "user1".into(), password: "pw1".into(),
            priority, provider: None, region: None, ports_allowed: None, verified: None,
        }
    }

    async fn ctx(d: &tempfile::TempDir, prios: &[u32]) -> (DaemonCtx, Arc<FakeHost>) {
        let mut s = sample_state();
        let g = s.residential.groups.get_mut("default").unwrap();
        g.enabled = true;
        g.upstreams = prios.iter().enumerate().map(|(i, p)| upstream(i as u128 + 1, *p)).collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        g.blacklist.auto.clear();
        let host = Arc::new(FakeHost::new());
        let c = DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: host.clone(),
            paths: bui_schema::paths::Paths::default_server(),
        };
        (c, host)
    }

    /// `FakeProber` 按 URL 查表，本任务要按**上游**区分好坏，所以用一个按 host 分派的 prober。
    /// `connect` 也按上游分派：`probe_member` 全失败后会用它做 CONNECT 补判，
    /// `auth_failed` 集合里的上游必须在补判里也回 `AuthFailed`（真机上 407 只有这条路能认出来）
    struct ByHost { bad: std::collections::BTreeSet<String>, auth_failed: std::collections::BTreeSet<String> }
    impl Prober for ByHost {
        fn connect(&self, u: &Upstream, _h: &str, _p: u16) -> crate::modules::residential::proxy::ConnectVerdict {
            if self.auth_failed.contains(&u.host) {
                return crate::modules::residential::proxy::ConnectVerdict::AuthFailed;
            }
            crate::modules::residential::proxy::ConnectVerdict::Open
        }
        fn get(&self, up: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
            if self.auth_failed.contains(&up.host) {
                return Err(ProbeError::AuthFailed);
            }
            if self.bad.contains(&up.host) {
                return Err(ProbeError::Unreachable("refused".into()));
            }
            Ok(HttpProbe { status: 204, body: String::new() })
        }
        fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> { Ok(true) }
        fn direct_tcp(&self, _h: &str, _p: u16) -> bool { true }
    }
    fn by_host(bad: &[&str], auth_failed: &[&str]) -> Arc<dyn Prober> {
        Arc::new(ByHost {
            bad: bad.iter().map(|s| s.to_string()).collect(),
            auth_failed: auth_failed.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[tokio::test]
    async fn a_healthy_selection_sticks() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone()).await.unwrap();
        assert_eq!(out.healthy, vec!["resi-1", "resi-2"]);
        assert_eq!(out.switched_to, None, "当前健康就粘住");
        assert_eq!(out.replayed_to, None, "两边一致，没什么可重放");
        assert_eq!(clash.calls(), vec!["get"], "不发 PUT");
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(1)),
            "规则 6b：runtime 还没有选择（守护进程首次启动），才拿 Clash 的 now 初始化它"
        );
    }

    #[tokio::test]
    async fn a_relay_restart_outside_reconcile_is_healed_by_replaying_the_runtime_choice() {
        // 看门狗（watchdog.rs:141）与 POST /api/services/b-ui-relay/restart 都不发
        // Event::RelayRestarted（§C 末段），于是 replay_loop 收不到通知；relay 重启后
        // selector 回落到配置里的 default（池首 resi-1），而运行时选的是 resi-2。
        // 规则 6a 必须把 runtime 的选择**重放**回去，而不是采纳 Clash 的 now。
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| r.selected_upstream_id = Some(Uuid::from_u128(2))).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone()).await.unwrap();
        assert_eq!(out.replayed_to.as_deref(), Some("resi-2"), "重放，不是采纳 now");
        assert_eq!(out.switched_to, None, "重放不是切换");
        assert!(out.notes.iter().any(|n| n.contains("重放")), "{:?}", out.notes);
        assert_eq!(clash.selected().as_deref(), Some("resi-2"));
        let r = rstate::read(&c.runtime).await;
        assert_eq!(
            r.selected_upstream_id,
            Some(Uuid::from_u128(2)),
            "runtime 是真源：绝不能被 Clash 的 now 改回池首（否则一次看门狗重启就静默撤销了手动切换）"
        );
        assert_eq!(r.last_switch_at, None, "重放不写 last_switch_at，不占 60s 限速额度");
    }

    #[tokio::test]
    async fn a_stale_runtime_selection_falls_back_to_the_clash_now() {
        // runtime 里记着一条**已不在池里**的 uuid（删上游与巡检竞态的残留）：规则 6a 的
        // `filter(|id| ids.contains(id))` 必须把它滤掉，走规则 6b 从 Clash 的 now 初始化，
        // 而不是拿一个 tag_of 为 None 的 uuid 去重放（那会 panic 或发一个空 PUT）。
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| r.selected_upstream_id = Some(Uuid::from_u128(99))).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone()).await.unwrap();
        assert_eq!(out.replayed_to, None, "悬空 uuid 不重放");
        assert_eq!(out.switched_to, None);
        assert!(!clash.calls().iter().any(|x| x.starts_with("put:")), "一个 PUT 都不发");
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(1)),
            "规则 6b：runtime 的选择失效 ⇒ 用 Clash 的 now 重新初始化"
        );
    }

    #[tokio::test]
    async fn two_bad_rounds_switch_to_the_lowest_priority_healthy_member() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20, 5]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net"], &[]);
        let first = check_once(&c, p.clone(), clash.clone()).await.unwrap();
        assert_eq!(first.switched_to, None, "第 1 轮失败还在迟滞里，不切");
        assert!(first.healthy.contains(&"resi-1".to_string()));
        host.advance(120);
        let second = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(second.switched_to.as_deref(), Some("resi-3"), "priority 5 < 10 < 20");
        assert_eq!(clash.calls().last().unwrap(), "put:resi-3");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.selected_upstream_id, Some(Uuid::from_u128(3)), "runtime 记 uuid，不记位置键 tag");
        assert!(r.selected_pending_persist, "state 落点还是 resi-1，标记待持久化（契约决策 §C）");
        // state 没被改 → relay 不重启（spec §5.3 的用意）
        assert_eq!(
            rstate::group_of(&c.store.read().await).selected_upstream_id,
            Some(Uuid::from_u128(1))
        );
    }

    #[tokio::test]
    async fn a_407_upstream_counts_as_unhealthy() {
        // 调研 §D：凭据失效的上游「可达」但每条连接都被拒，必须判不健康。
        // `ByHost::get` 直接回 ProbeError::AuthFailed（模拟 `looks_like_proxy_auth` 命中）；
        // 下一条测试覆盖「文字里没线索、只能靠 CONNECT 补判」的真机形态。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&[], &["isp1.example.net"]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.healthy, vec!["resi-2"]);
        assert_eq!(out.switched_to.as_deref(), Some("resi-2"));
        let r = rstate::read(&c.runtime).await;
        assert!(r.alerts.iter().any(|a| a.contains("凭据")), "要点名凭据失效：{:?}", r.alerts);
    }

    #[tokio::test]
    async fn a_407_that_only_the_connect_probe_can_see_still_raises_the_credential_alert() {
        // 真机形态：https 目标经 HTTP 上游走 CONNECT 隧道，407 让 reqwest 直接 Err，
        // 文字里也可能没有 407 字样 ⇒ `get` 只能给 Unreachable。此时唯一的识别路径是
        // `probe_member` 末尾那次 CONNECT 补判（T3 `confirm_auth_failure`）。
        struct Opaque;
        impl Prober for Opaque {
            fn connect(&self, _u: &Upstream, _h: &str, _p: u16)
                -> crate::modules::residential::proxy::ConnectVerdict {
                crate::modules::residential::proxy::ConnectVerdict::AuthFailed
            }
            fn get(&self, _up: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
                // 隧道建立阶段就失败了，reqwest 给的就是这种不带线索的错误
                Err(ProbeError::Unreachable("error trying to connect".into()))
            }
            fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> { Ok(true) }
            fn direct_tcp(&self, _h: &str, _p: u16) -> bool { true }
        }
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p: Arc<dyn Prober> = Arc::new(Opaque);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert!(out.healthy.is_empty());
        let r = rstate::read(&c.runtime).await;
        assert!(
            r.alerts.iter().any(|a| a.contains("凭据")),
            "没有 CONNECT 补判，这条告警在真机上永远不会出现：{:?}",
            r.alerts
        );
    }

    #[tokio::test]
    async fn all_unhealthy_keeps_the_current_member_and_alerts() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net", "isp2.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert!(out.healthy.is_empty());
        assert_eq!(out.switched_to, None, "全不健康时保持（降级总比乱切好）");
        assert!(!clash.calls().iter().any(|x| x.starts_with("put:")));
        assert!(rstate::read(&c.runtime).await.alerts.iter().any(|a| a.contains("全部")));
    }

    #[tokio::test]
    async fn a_switch_is_rate_limited_to_one_per_minute() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        rstate::update(&c.runtime, |r| {
            r.last_switch_at = Some(crate::util::fmt_rfc3339(host.now()));
        })
        .await;
        let p = by_host(&["isp1.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(30);
        let out = check_once(&c, p.clone(), clash.clone()).await.unwrap();
        assert_eq!(out.switched_to, None);
        assert!(out.notes.iter().any(|n| n.contains("限速")), "{:?}", out.notes);
        host.advance(31); // 距上次切换 61s
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to.as_deref(), Some("resi-2"));
    }

    #[tokio::test]
    async fn a_membership_change_during_the_round_skips_switching() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net"], &[]);
        let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        // 先攒满迟滞，让「本来该切」成立
        check_round(&c, p.clone(), clash.clone(), ids.clone()).await.unwrap();
        host.advance(120);
        // 注入一份「本轮开始时池里有 3 个成员」的快照 = 探测中途管理员删了一条上游。
        // 直接在 check_once 前后改池是测不到这条规则的（那样前后快照相同，会落进规则 7）。
        let mut stale = ids.clone();
        stale.push(Uuid::from_u128(3));
        let out = check_round(&c, p.clone(), clash.clone(), stale).await.unwrap();
        assert_eq!(out.switched_to, None);
        assert!(out.notes.iter().any(|n| n.contains("成员集")), "{:?}", out.notes);
        assert!(!clash.calls().iter().any(|x| x.starts_with("put:")), "本轮一个 PUT 都不发");
        // 对照组：成员集没变的同一轮是会切的
        host.advance(120);
        let out2 = check_round(&c, p, clash.clone(), ids).await.unwrap();
        assert_eq!(out2.switched_to.as_deref(), Some("resi-2"));
    }

    #[test]
    fn pick_target_breaks_priority_ties_by_the_24h_success_rate() {
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 10), upstream(2, 10), upstream(3, 10)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let mut r = ResiRuntime::default();
        // health 以 uuid 的字符串形式为键（契约决策 §C）
        for (i, oks) in [(1u128, 1u32), (2, 3), (3, 2)] {
            let mut h = rstate::HealthState::default();
            for k in 0..4 {
                rstate::record_probe(&mut h, k < oks, now - time::Duration::hours(1));
            }
            r.health.insert(Uuid::from_u128(i).to_string(), h);
        }
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        assert_eq!(pick_target(&g, &r, &healthy, now), Some(Uuid::from_u128(2)), "3/4 > 2/4 > 1/4");
        // 优先级压过成功率
        g.upstreams[0].priority = 1;
        assert_eq!(pick_target(&g, &r, &healthy, now), Some(Uuid::from_u128(1)));
        assert_eq!(pick_target(&g, &r, &[], now), None);
        // 不在当前池里的 uuid 直接忽略（删上游与巡检并发时会出现）
        assert_eq!(pick_target(&g, &r, &[Uuid::from_u128(99)], now), None);
    }

    #[tokio::test]
    async fn a_relay_restart_replays_the_selected_upstream() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| r.selected_upstream_id = Some(Uuid::from_u128(2))).await;
        // relay 重启后 selector 回到配置里的 default（池首），必须把运行时选择重放回去
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // **先 subscribe 再 spawn**：broadcast 丢弃「发送时还没有订阅者」的事件，
        // 若让 replay_loop 自己 subscribe，spawn 之后立刻 send 的这条必丢
        let rx = c.bus.subscribe();
        let task = tokio::spawn(replay_loop(c.clone(), clash.clone(), rx));
        c.bus.send(Event::RelayRestarted);
        for _ in 0..50 {
            if clash.selected().as_deref() == Some("resi-2") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        task.abort();
        assert_eq!(clash.selected().as_deref(), Some("resi-2"), "spec §5.3：relay 重启后重放选择");
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_loop_runs_a_round_every_two_minutes() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let task = tokio::spawn(health_loop(c.clone(), by_host(&[], &[]), clash.clone()));
        // 用「等到出现」而不是精确计时：一轮里有 spawn_blocking，与假时钟没有确定的先后
        for _ in 0..500 {
            if !clash.calls().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(!clash.calls().is_empty(), "interval 第一 tick 立即完成 ⇒ 起来就跑一轮");
        let first = clash.calls().len();
        tokio::time::advance(std::time::Duration::from_secs(HEALTH_INTERVAL_SECS + 1)).await;
        for _ in 0..500 {
            if clash.calls().len() > first {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(clash.calls().len() > first, "每 {HEALTH_INTERVAL_SECS} 秒再跑一轮");
        task.abort();
    }

    #[tokio::test]
    async fn manual_select_uses_clash_only_and_rejects_unknown_ids() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let tag = select_manual(&c, clash.clone(), Uuid::from_u128(2)).await.unwrap();
        assert_eq!(tag, "resi-2");
        assert_eq!(clash.selected().as_deref(), Some("resi-2"));
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(2))
        );
        // 不写 state ⇒ 不重启 relay
        assert_eq!(
            rstate::group_of(&c.store.read().await).selected_upstream_id,
            Some(Uuid::from_u128(1))
        );
        assert!(select_manual(&c, clash, Uuid::from_u128(99)).await.is_err());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::health`
Expected: 编译失败，`cannot find function `check_once``。

- [ ] **Step 3: 实现 `probe_member` 与 `pick_target`**

```rust
pub fn probe_member(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    for _ in 0..HEALTH_TRIES {
        match p.get(up, HEALTH_PROBE_URL) {
            // generate_204 正常回 204；任何 2xx/3xx 都说明隧道通了
            Ok(hp) if hp.status < 400 => return MemberProbe { ok: true, auth_failed: false },
            // 调研 §D：407 的上游「可达」但每条连接都被拒 —— 一律不健康，且不必再试
            Err(ProbeError::AuthFailed) => return MemberProbe { ok: false, auth_failed: true },
            _ => {}
        }
    }
    // 全失败：`get` 在 https 目标上分不出「凭据失效」与「连不上」（407 发生在 CONNECT
    // 隧道建立阶段，reqwest 只给一个 Err，见 T3 `confirm_auth_failure` 的注释），
    // 所以这里经上游自写一次 CONNECT 把两者分开。不补判的后果是生产上凭据失效
    // 只会报「上游挂了」，spec §5.2 要求的「凭据失效」告警永远不出现。
    let auth_failed = proxy::confirm_auth_failure(p, up, HEALTH_PROBE_HOST);
    MemberProbe { ok: false, auth_failed }
}

pub fn ids_of(g: &ResidentialGroup) -> Vec<Uuid> {
    g.upstreams.iter().map(|u| u.id).collect()
}

pub fn pick_target(
    g: &ResidentialGroup,
    r: &ResiRuntime,
    healthy: &[Uuid],
    now: OffsetDateTime,
) -> Option<Uuid> {
    let mut cands: Vec<(u32, i64, usize, Uuid)> = healthy
        .iter()
        .filter_map(|id| {
            // 不在当前池里的 uuid 直接忽略（删上游与巡检并发时会出现）
            let idx = g.upstreams.iter().position(|u| u.id == *id)?;
            let rate = r
                .health
                .get(&id.to_string())
                .map(|h| state::success_rate_24h(h, now))
                .unwrap_or(0.0);
            // 成功率降序 = 放大后取负（f64 不能直接排序，也不想引 total_cmp 的歧义）；
            // 第三项 idx 让同优先级同成功率时按池内下标稳定排序
            Some((g.upstreams[idx].priority, -((rate * 1_000_000.0) as i64), idx, *id))
        })
        .collect();
    cands.sort();
    cands.into_iter().next().map(|(_, _, _, id)| id)
}
```

- [ ] **Step 4: 实现 `check_once` / `check_round`**

```rust
pub async fn check_once(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
) -> anyhow::Result<RoundOutcome> {
    // 规则 4 的「本轮开始时的成员集」快照在这里取；check_round 只负责比较
    let before = ids_of(&state::group_of(&ctx.store.read().await));
    check_round(ctx, p, c, before).await
}

pub async fn check_round(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
    before: Vec<Uuid>,
) -> anyhow::Result<RoundOutcome> {
    let mut out = RoundOutcome::default();
    let g = state::group_of(&ctx.store.read().await);
    if !g.pool_active() {
        out.notes.push("住宅池未启用或为空，跳过巡检（relay 已 fail-open 直连）".into());
        return Ok(out);
    }
    let tags = clash::tags(&g); // 只用于展示与 Clash API，绝不当运行时主键（§C）
    let now = ctx.host.now();

    // 规则 2：成员并行探测（并发上限 4）
    let ups = g.upstreams.clone();
    let pp = p.clone();
    let results = super::fanout(ups.clone(), move |up| probe_member(pp.as_ref(), &up)).await;

    let mut auth_alerts = Vec::new();
    let mut probed: Vec<(Uuid, bool)> = Vec::new();
    for (i, up) in ups.iter().enumerate() {
        let probe = results.get(i).copied().flatten();
        let ok = probe.map(|x| x.ok).unwrap_or(false);
        if probe.is_none() {
            out.notes
                .push(format!("{} 的探测任务异常结束，本轮按不达标处理", up.name));
        }
        // 凭据失效要单独告警：不是网络抖动，管理员必须换凭据（探测结果里直接带出来，
        // 不再为了补判而在 async 上下文里同步调一次 Prober）
        if probe.map(|x| x.auth_failed).unwrap_or(false) {
            auth_alerts.push(format!(
                "上游 {} 凭据失效（407 / SOCKS5 认证被拒），请更新凭据",
                up.name
            ));
        }
        out.probed.push((tags[i].clone(), ok));
        probed.push((up.id, ok));
    }

    // 规则 3：迟滞 + 24h 样本。**一轮只写一次 runtime**：每次 state::update 都是
    // tmp + fsync + rename，按成员各写一次等于一轮 N 次落盘。
    let samples = probed.clone();
    let rt = state::update(&ctx.runtime, move |r| {
        for (id, ok) in samples {
            let h = r.health.entry(id.to_string()).or_default();
            state::record_probe(h, ok, now);
            let _ = state::apply_hysteresis(h, ok);
        }
    })
    .await;
    let healthy: Vec<Uuid> = probed
        .iter()
        .map(|(id, _)| *id)
        // 没探过的成员默认健康（HealthState::default().active = true）
        .filter(|id| rt.health.get(&id.to_string()).map(|h| h.active).unwrap_or(true))
        .collect();
    out.healthy = healthy.iter().filter_map(|id| clash::tag_of(&g, *id)).collect();

    // 规则 4：成员集在本轮内变化 → 只写 runtime，不切
    let after = ids_of(&state::group_of(&ctx.store.read().await));
    if after != before {
        out.notes.push(format!(
            "住宅池成员集在本轮探测期间变化（{} → {} 个成员），本轮不切换",
            before.len(),
            after.len()
        ));
        persist_alerts(ctx, &auth_alerts).await;
        return Ok(out);
    }

    // 规则 5：读不到当前选择（relay 没起来或旧配置）→ 不切
    let cc = c.clone();
    let sel_tag = tokio::task::spawn_blocking(move || cc.selected()).await?;
    let Some(sel_tag) = sel_tag else {
        out.notes
            .push("relay 的 Clash API 读不到当前选择（未运行或旧配置），本轮不切换".into());
        persist_alerts(ctx, &auth_alerts).await;
        return Ok(out);
    };
    // Clash 的 now 只说明「relay 此刻在用哪条」，**不是**「该用哪条」的真源（§C）
    let sel_id = clash::id_of_tag(&g, &sel_tag);

    // 规则 6a：runtime 记的选择还在池里且健康 → 它就是该生效的出口。
    // 与 Clash 的 now 不一致 = relay 刚被看门狗（watchdog.rs:141）或
    // `/api/services/b-ui-relay/restart` 重启过、selector 回落到配置里的 default，
    // 这两条来路都不发 Event::RelayRestarted（§C 末段），所以在这里重放。
    // **方向只能是 runtime → Clash**：反过来把 now 写进 runtime 会让一次重启静默
    // 撤销管理员的手动切换与上一轮的自动避障。
    let ids = ids_of(&g);
    let want = rt.selected_upstream_id.filter(|id| ids.contains(id));
    if let Some(want) = want {
        if healthy.contains(&want) {
            let mut alerts = auth_alerts.clone();
            if Some(want) != sel_id {
                // tag 现算（位置键，不做主键）；want 来自当前池，tag_of 必有值
                let tag = clash::tag_of(&g, want).expect("want 取自当前池");
                let (cc, t2) = (c.clone(), tag.clone());
                match tokio::task::spawn_blocking(move || cc.select(&t2)).await? {
                    Ok(()) => {
                        tracing::info!(from = %sel_tag, to = %tag, "relay 重启后重放住宅出口选择");
                        out.notes.push(format!(
                            "relay 的当前选择 {sel_tag} 与运行时记录的 {tag} 不一致（relay 刚重启过），已重放运行时的选择"
                        ));
                        // 重放不是切换：不写 last_switch_at、不吃 60s 限速
                        out.replayed_to = Some(tag);
                    }
                    Err(e) => {
                        alerts.push(format!("重放住宅出口选择到 {tag} 失败：{e}"));
                        out.notes.push(format!("重放到 {tag} 失败：{e}"));
                    }
                }
            }
            let pending = Some(want) != g.selected_upstream_id;
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(want);
                r.selected_pending_persist = pending;
                for a in alerts.drain(..) {
                    state::push_alert(r, a);
                }
            })
            .await;
            return Ok(out);
        }
    } else if let Some(sel_id) = sel_id {
        // 规则 6b：runtime 没有选择（守护进程首次启动、从没切过）或它已被删出池 ——
        // 此时才拿 Clash 的 now 初始化 runtime，并粘在它上面
        if healthy.contains(&sel_id) {
            let pending = Some(sel_id) != g.selected_upstream_id;
            let mut alerts = auth_alerts.clone();
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(sel_id);
                r.selected_pending_persist = pending;
                for a in alerts.drain(..) {
                    state::push_alert(r, a);
                }
            })
            .await;
            return Ok(out);
        }
    }
    // 规则 7：全不健康 → 保持并告警
    if healthy.is_empty() {
        out.notes.push(format!(
            "全部上游探测不达标，保持当前出口 {sel_tag}（降级总比乱切好）"
        ));
        let mut alerts = auth_alerts.clone();
        alerts.push("全部住宅上游探测不达标，出口已降级但未切换".into());
        persist_alerts(ctx, &alerts).await;
        return Ok(out);
    }

    // 规则 8：选目标（uuid 进、uuid 出；tag 只在调 Clash API 时现算）
    let Some(target_id) = pick_target(&g, &rt, &healthy, now) else {
        persist_alerts(ctx, &auth_alerts).await;
        return Ok(out);
    };
    let Some(target_tag) = clash::tag_of(&g, target_id) else {
        // healthy 全部来自当前池，走不到这里；真走到了说明池刚变过，按规则 4 处理
        out.notes
            .push("切换目标已不在池里（成员集刚变过），本轮不切换".into());
        persist_alerts(ctx, &auth_alerts).await;
        return Ok(out);
    };
    // 规则 9：≥ 60 秒
    let since = rt
        .last_switch_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|t| (now - t).whole_seconds())
        // 时钟回跳（NTP 校时）不该把限速锁死
        .map(|s| if s < 0 { i64::MAX } else { s })
        .unwrap_or(i64::MAX);
    if since < SWITCH_MIN_INTERVAL_SECS {
        out.notes.push(format!(
            "当前 {sel_tag} 不健康，需切到 {target_tag}，但切换限速中（剩余 {}s）",
            SWITCH_MIN_INTERVAL_SECS - since
        ));
        persist_alerts(ctx, &auth_alerts).await;
        return Ok(out);
    }

    // 规则 10/11：切换
    let (cc, t2) = (c.clone(), target_tag.clone());
    match tokio::task::spawn_blocking(move || cc.select(&t2)).await? {
        Ok(()) => {
            let pending = Some(target_id) != g.selected_upstream_id;
            let mut alerts = auth_alerts.clone();
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(target_id);
                r.last_switch_at = Some(fmt_rfc3339(now));
                r.selected_pending_persist = pending;
                for a in alerts.drain(..) {
                    state::push_alert(r, a);
                }
            })
            .await;
            tracing::info!(from = %sel_tag, to = %target_tag, "住宅出口切换");
            out.switched_to = Some(target_tag);
        }
        Err(e) => {
            // 切换失败不改 runtime：下一轮重新评估（别把没生效的选择记成生效）
            let mut alerts = auth_alerts.clone();
            alerts.push(format!("切换住宅出口到 {target_tag} 失败：{e}"));
            persist_alerts(ctx, &alerts).await;
            out.notes.push(format!("切换到 {target_tag} 失败：{e}"));
        }
    }
    Ok(out)
}

async fn persist_alerts(ctx: &DaemonCtx, alerts: &[String]) {
    if alerts.is_empty() {
        return;
    }
    let alerts = alerts.to_vec();
    state::update(&ctx.runtime, move |r| {
        for a in alerts {
            state::push_alert(r, a);
        }
    })
    .await;
}
```

- [ ] **Step 5: 实现两个循环与手动切换**

```rust
pub async fn health_loop(ctx: DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(HEALTH_INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = check_once(&ctx, p.clone(), c.clone()).await {
            tracing::warn!(error = %e, "住宅巡检一轮失败");
        }
    }
}

pub async fn replay_loop(
    ctx: DaemonCtx,
    c: Arc<dyn Clash>,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
) {
    // rx 由调用方先 subscribe 后传入：broadcast 丢弃「发送时还没有订阅者」的事件，
    // 在这里 subscribe 会漏掉调用方 spawn 之后立刻发的那条（见 Interfaces）
    loop {
        match rx.recv().await {
            Ok(Event::RelayRestarted) => {
                // relay 重启后 selector 回到配置里的 default（池首），把运行时选择重放回去。
                // 没有这一步，每次黑名单批量或 pin 都会把出口悄悄换回池首。
                // tag 现算：runtime 存的是 uuid，池增删后同一个 resi-N 可能已指向别人（§C）
                let Some(id) = state::read(&ctx.runtime).await.selected_upstream_id else { continue };
                let g = state::group_of(&ctx.store.read().await);
                let Some(tag) = clash::tag_of(&g, id) else {
                    tracing::warn!(%id, "运行时选中的上游已不在池里，跳过重放");
                    continue;
                };
                let (cc, t2) = (c.clone(), tag.clone());
                match tokio::task::spawn_blocking(move || cc.select(&t2)).await {
                    Ok(Ok(())) => tracing::info!(tag = %tag, "relay 重启后已重放住宅出口选择"),
                    other => tracing::warn!(tag = %tag, result = ?other, "重放住宅出口选择失败"),
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
    }
}

pub async fn select_manual(ctx: &DaemonCtx, c: Arc<dyn Clash>, id: Uuid) -> anyhow::Result<String> {
    let g = state::group_of(&ctx.store.read().await);
    let tag = clash::tag_of(&g, id).ok_or_else(|| anyhow::anyhow!("上游不在当前池里"))?;
    let (cc, t2) = (c.clone(), tag.clone());
    tokio::task::spawn_blocking(move || cc.select(&t2)).await??;
    let now = ctx.host.now();
    let pending = Some(id) != g.selected_upstream_id;
    state::update(&ctx.runtime, move |r| {
        r.selected_upstream_id = Some(id);
        r.last_switch_at = Some(fmt_rfc3339(now));
        r.selected_pending_persist = pending;
    })
    .await;
    Ok(tag)
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui modules::residential::health && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 10 passed。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/residential/health.rs
git commit -m "feat(resi): 健康巡检（2 轮迟滞、粘住、priority+24h 成功率、≥60s 限速）与 relay 重启重放"
```

---

### Task 9: 自动黑名单（候选学习 → 硬拒确认 → 每日批量生效 → 每日复核移除）与 pins

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/blacklist.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{check, clash, fanout, journal, state, BASE_PORTS, CANDIDATE_THRESHOLD, CONFIRM_MIN_GAP_SECS, CONFIRM_NEEDED, DAILY_HOUR, DAILY_TICK_SECS, JOURNAL_POLL_SECS, PAY_HOSTS, PROBE_PORTS, REVIEW_PASSES_TO_REMOVE}`；`crate::modules::residential::proxy::{ConnectVerdict, Prober}`；`crate::reconcile::DaemonCtx`；`crate::util::{fmt_rfc3339, parse_rfc3339}`；`bui_schema::model::{AutoEntry, Pin, Rule}`
- Produces:
```rust
// crate::modules::residential::blacklist
/// 每日固定探针集（spec §5.4 (b)）：支付域名集 + R13 §6.1 内置候选里 2026-09-11 实测被拒的那些。
/// **只是「值得一探」的候选**，进不进黑名单完全由探测结果决定（R13 §6.1 的硬要求：
/// 新上游的黑名单从空开始，没有任何预置条目）。
pub const PROBE_SET: [&str; 12] = [
    "checkout.stripe.com", "pay.google.com", "www.paypal.com", "api.stripe.com",
    "gateway.icloud.com", "query.ess.apple.com", "courier.push.apple.com",
    "x.com", "api.x.com", "www.google.com", "www.tiktok.com", "www.instagram.com",
];
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DailyReport {
    pub learned: usize,        // 本轮从日志新增/累加的候选数
    pub probed: usize,         // 本轮被探针集推成候选的条目数（确认交给 journal_loop 的确认轮）
    pub applied: usize,        // 写进 state.blacklist.auto 的条目数
    pub removed: usize,        // 复核通过被移除的条目数
    pub ports_learned: usize,  // 本轮端口集重探后更新了 ports_allowed 的上游数（spec §5.4 (b)）
    pub persisted_selection: bool,   // 是否把运行时落点的漂移写回了 state（契约决策 §C 的 D4）
    pub notes: Vec<String>,
}

/// 每日端口集重探对一个上游的结论
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortLearn {
    /// 本轮干净：把该上游的 `ports_allowed` 写成这个值（`None` = 不限）
    Learned(Option<Vec<u16>>),
    /// 本轮不可信（407 / 有探测抖动）：保留上一次学到的白名单，一个字节都不动
    Keep,
}

/// 裸 IP 目标不进黑名单（spec §5.4：规则是 `domain_suffix` = 完整主机名；端口类由
/// `ports_allowed` 表达）
pub fn is_bare_ip(host: &str) -> bool;
/// **端口类拒绝不进域名黑名单**（裁决）：只有落在该上游 `ports_allowed` 白名单里的端口
/// 被拒，才算「这个上游代理不了这个域名」；白名单外端口的硬拒是**端口策略**，已由
/// `ports_allowed` 表达（T6 体检学得），再生成一条 `domain_suffix` 规则就是把整个域名
/// 误判成不可代理。`None` = 还没学到白名单 ⇒ 按基准端口 [`BASE_PORTS`]（`[80, 443]`）看待。
pub fn port_allowed(up: &Upstream, port: u16) -> bool;
pub fn candidate_key(upstream_id: Uuid, host: &str, port: u16) -> String;   // "<uuid>|<host>|<port>"

/// 从 journald 增量学候选（每 [`JOURNAL_POLL_SECS`] 秒一次）。返回新增/累加的条目数。
/// 只计 [`port_allowed`] 为真的拒绝。
pub async fn learn_from_journal(ctx: &DaemonCtx) -> anyhow::Result<usize>;
/// 候选学习 + 确认的同一个循环（裁决「黑名单确认节奏」）：每 [`JOURNAL_POLL_SECS`] 秒
/// **先** [`learn_from_journal`] **再** [`confirm_round`]，所以一条候选攒够「间隔 ≥ 10
/// 分钟、连续 [`CONFIRM_NEEDED`] 次」就能进 `pending`（≥10 分钟即可），不必等 04:00。
pub async fn journal_loop(ctx: DaemonCtx, p: Arc<dyn Prober>);

/// 单条确认：经该上游**硬拒** 且 直连同目标 TCP 可达 → `Some(true)`；上游说通 →
/// `Some(false)`（复核路径据此累计 `passes`）；其余一律 `None`（未知，不改状态）——
/// 包括「硬拒但直连也不通」：那既不能算代理的错，也绝不能当成 pass，否则 3 天后会
/// 误删本该保留的 `auto` 条目。
pub fn confirm_once(p: &dyn Prober, up: &Upstream, host: &str, port: u16) -> Option<bool>;
/// 把达到阈值的候选推进一步：**确认进度记在候选上**（`Candidate.confirms` /
/// `last_confirm_at`），只有攒到「间隔 ≥ 10 分钟、连续 `CONFIRM_NEEDED` 次」才升进
/// `runtime.pending`（并出候选池）。返回本轮新升进 `pending` 的条目数。
pub async fn confirm_round(ctx: &DaemonCtx, p: Arc<dyn Prober>) -> anyhow::Result<usize>;
/// `ports` 表 →「本轮该不该改 `ports_allowed`」（纯函数）
pub fn port_learn(ports: &BTreeMap<u16, String>, auth_failed: bool) -> PortLearn;
/// spec §5.4 (b) 的**端口集**部分：每个上游对 `BASE_PORTS ∪ PROBE_PORTS` 重探一遍。
/// 结论在每日那一次 `Store::update` 里一起写回，不额外多一次 relay 重启。
pub async fn probe_ports_daily(p: Arc<dyn Prober>, ups: &[Upstream]) -> Vec<(Uuid, PortLearn)>;

/// 每日窗口（服务器本地 04:00）：learn → 探针集（只探 `port_allowed` 为真的上游）→
/// **端口集重探** → 批量写 state（含 `ports_allowed` 与落点漂移）→ 复核移除。
/// **确认不在这里**：确认节奏归 [`journal_loop`] 的每 5 分钟一轮（裁决「黑名单确认节奏」），
/// 04:00 只做批量生效 + spec §5.4 (b) 的每日探针/端口集 + 每日复核。
/// 这是**唯一**会因 `auto` 变化而重启 relay 的时机（spec §5.4「relay 重启是唯一掐连接的动作」）。
pub async fn daily_round(ctx: &DaemonCtx, p: Arc<dyn Prober>) -> anyhow::Result<DailyReport>;
pub async fn daily_loop(ctx: DaemonCtx, p: Arc<dyn Prober>);

/// 管理员「立即应用」：把 `runtime.pending` 全部写进 `state.blacklist.auto`（立刻重启 relay）
pub async fn apply_now(ctx: &DaemonCtx) -> anyhow::Result<usize>;
/// pins 增删（立即生效）
pub async fn add_pin(ctx: &DaemonCtx, rule: Rule, note: String) -> anyhow::Result<()>;
pub async fn remove_pin(ctx: &DaemonCtx, rule: &Rule) -> anyhow::Result<bool>;
/// 手动删一条 `auto`（面板「误伤了，删掉」）
pub async fn remove_auto(ctx: &DaemonCtx, upstream_id: Uuid, rule: &Rule) -> anyhow::Result<bool>;
```

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::proxy::{ConnectVerdict, HttpProbe, ProbeError};
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::{fake::FakeHost, CmdOut};
    use crate::testutil::sample_state;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    const HTTP_403: &str = "open connection to gateway.icloud.com:443 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
    const IP_403: &str = "open connection to 198.51.100.9:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden";

    async fn ctx(d: &tempfile::TempDir) -> (DaemonCtx, Arc<FakeHost>) {
        let mut s = sample_state();
        let g = s.residential.groups.get_mut("default").unwrap();
        g.enabled = true;
        g.blacklist.auto.clear();
        g.blacklist.pins.clear();
        g.upstreams = vec![Upstream {
            id: Uuid::from_u128(1), name: "url-1".into(), kind: UpstreamKind::Http,
            host: "isp1.example.net".into(), port: 10007,
            username: "user1".into(), password: "pw1".into(),
            priority: 10, provider: None, region: None, ports_allowed: None, verified: None,
        }];
        g.selected_upstream_id = Some(Uuid::from_u128(1));
        let host = Arc::new(FakeHost::new());
        host.with(|i| i.which.insert("journalctl".into()));
        let c = DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: host.clone(),
            paths: bui_schema::paths::Paths::default_server(),
        };
        (c, host)
    }

    /// 指定哪些 `host:port` 被上游硬拒、哪些直连可达
    struct Rejector { refused: Vec<String>, direct: Vec<String> }
    impl Prober for Rejector {
        fn connect(&self, _u: &Upstream, h: &str, p: u16) -> ConnectVerdict {
            if self.refused.contains(&format!("{h}:{p}")) {
                ConnectVerdict::Refused { code: 403 }
            } else {
                ConnectVerdict::Open
            }
        }
        fn get(&self, _u: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
            Ok(HttpProbe { status: 204, body: String::new() })
        }
        fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> { Ok(true) }
        fn direct_tcp(&self, h: &str, p: u16) -> bool { self.direct.contains(&format!("{h}:{p}")) }
    }
    fn rejector(refused: &[&str], direct: &[&str]) -> Arc<dyn Prober> {
        Arc::new(Rejector {
            refused: refused.iter().map(|s| s.to_string()).collect(),
            direct: direct.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn bare_ip_targets_never_become_rules() {
        // spec §5.4：端口类拒绝由 ports_allowed 表达，不进黑名单
        assert!(is_bare_ip("198.51.100.9"));
        assert!(is_bare_ip("2001:db8::1"));
        assert!(!is_bare_ip("gateway.icloud.com"));
        assert_eq!(candidate_key(Uuid::nil(), "a.com", 443), format!("{}|a.com|443", Uuid::nil()));
    }

    #[tokio::test]
    async fn journal_learning_counts_per_upstream_host_port_and_drops_bare_ips() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let cmd = format!(
            "journalctl -u {} --no-pager -o cat --show-cursor --since -25h",
            crate::modules::residential::JOURNAL_UNIT
        );
        host.with(|i| {
            i.scripted.push((
                cmd,
                CmdOut::success(&format!("{HTTP_403}\n{HTTP_403}\n{IP_403}\n-- cursor: s=1\n")),
            ));
        });
        assert_eq!(learn_from_journal(&c).await.unwrap(), 2, "两条域名拒绝（裸 IP 被丢）");
        let r = rstate::read(&c.runtime).await;
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        assert_eq!(r.candidates[&key].hits, 2);
        assert_eq!(r.candidates.len(), 1, "裸 IP 目标不进候选");
        assert_eq!(r.journal_cursor.as_deref(), Some("s=1"));
    }

    #[tokio::test]
    async fn a_refused_non_whitelisted_port_never_becomes_a_domain_rule() {
        // 裁决「端口类拒绝不进域名黑名单」：只有 port ∈ ports_allowed（None ⇒ [80,443]）
        // 的拒绝才计候选。5228 上的 403 是端口策略（ports_allowed 已表达），把
        // courier.push.apple.com 整域拉黑等于把该上游能代理的流量也推回直连。
        const PUSH_403: &str = "open connection to courier.push.apple.com:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let cmd = format!(
            "journalctl -u {} --no-pager",  // scripted 前缀匹配，两种游标形态共用
            crate::modules::residential::JOURNAL_UNIT
        );
        host.with(|i| {
            i.scripted.push((
                cmd,
                CmdOut::success(&format!(
                    "{PUSH_403}\n{PUSH_403}\n{PUSH_403}\n{HTTP_403}\n-- cursor: s=1\n"
                )),
            ));
        });
        // ports_allowed 还没学到（None）⇒ 按 BASE_PORTS = [80, 443] 看待
        assert!(rstate::group_of(&c.store.read().await).upstreams[0].ports_allowed.is_none());
        assert_eq!(learn_from_journal(&c).await.unwrap(), 1, "只有 443 那条计候选");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.candidates.len(), 1);
        assert!(r.candidates
            .contains_key(&candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443)));
        // 学到白名单以后同理：5228 不在 [80,443] 里 ⇒ 永远不计候选
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].ports_allowed = Some(vec![80, 443]);
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.candidates.clear();
            r.journal_cursor = None;
        })
        .await;
        assert_eq!(learn_from_journal(&c).await.unwrap(), 1);
        assert!(!rstate::read(&c.runtime)
            .await
            .candidates
            .contains_key(&candidate_key(Uuid::from_u128(1), "courier.push.apple.com", 5228)));
        // 白名单里没有 443 的上游，每日探针集（固定打 443）整条跳过
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].ports_allowed = Some(vec![80]);
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.candidates.clear();
            r.journal_cursor = None;
        })
        .await;
        let p = rejector(&["pay.google.com:443"], &["pay.google.com:443"]);
        let rep = daily_round(&c, p).await.unwrap();
        assert_eq!(rep.probed, 0, "443 不在白名单里，探针集不推候选");
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        assert!(rstate::group_of(&c.store.read().await).blacklist.auto.is_empty());
    }

    #[tokio::test]
    async fn a_missing_journalctl_only_alerts() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.which.remove("journalctl");
        });
        assert_eq!(learn_from_journal(&c).await.unwrap(), 0);
        assert!(rstate::read(&c.runtime).await.alerts.iter().any(|a| a.contains("journalctl")));
    }

    #[tokio::test]
    async fn confirmation_needs_two_hard_rejects_ten_minutes_apart_plus_a_direct_hit() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        rstate::update(&c.runtime, |r| {
            r.candidates.insert(
                key.clone(),
                rstate::Candidate {
                    upstream_id: Uuid::from_u128(1), host: "gateway.icloud.com".into(), port: 443,
                    hits: CANDIDATE_THRESHOLD,
                    first_seen: crate::util::fmt_rfc3339(host.now()),
                    last_seen: crate::util::fmt_rfc3339(host.now()),
                    confirms: 0, last_confirm_at: None,
                },
            );
        })
        .await;
        let p = rejector(&["gateway.icloud.com:443"], &["gateway.icloud.com:443"]);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 0, "第 1 次确认还不进 pending");
        let r1 = rstate::read(&c.runtime).await;
        assert_eq!(r1.pending.len(), 0, "确认进度记在候选上，不是一确认就进 pending");
        assert_eq!(r1.candidates[&key].confirms, 1);
        // 10 分钟内的第二次不算（防同一轮抖动连中两次）
        host.advance(300);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 0);
        assert_eq!(
            rstate::read(&c.runtime).await.candidates[&key].confirms,
            1,
            "间隔不够就不该被算作一次确认"
        );
        host.advance(301);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 1, "间隔 ≥10 分钟的第 2 次才进 pending");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.pending[0].host, "gateway.icloud.com");
        assert_eq!(r.pending[0].confirms, CONFIRM_NEEDED);
        assert!(!r.candidates.contains_key(&key), "进了 pending 就出候选池");
        // pending 不生效：state 还没动 ⇒ relay 不重启
        assert!(rstate::group_of(&c.store.read().await).blacklist.auto.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_journal_loop_learns_then_confirms_so_ten_minutes_are_enough() {
        // 裁决「黑名单确认节奏」：同一个 JOURNAL_POLL_SECS 轮次里先 learn 再 confirm，
        // 所以两次确认（≥ CONFIRM_MIN_GAP_SECS）之后候选就进 pending——不再是「每天
        // 04:00 才确认一次 ⇒ 一条规则要两天」。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            // scripted 是前缀匹配：--since -25h 与 --after-cursor 两种形态共用这一条
            i.scripted.push((
                format!("journalctl -u {} --no-pager", crate::modules::residential::JOURNAL_UNIT),
                CmdOut::success(&format!("{HTTP_403}\n{HTTP_403}\n{HTTP_403}\n-- cursor: s=1\n")),
            ));
        });
        let p = rejector(&["gateway.icloud.com:443"], &["gateway.icloud.com:443"]);
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        let task = tokio::spawn(journal_loop(c.clone(), p));
        // 第 1 轮（`interval` 首 tick 立即触发）：3 条日志攒满阈值，同一轮里确认第 1 次
        for _ in 0..500 {
            if rstate::read(&c.runtime).await.candidates.get(&key).map(|x| x.confirms) == Some(1) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let r1 = rstate::read(&c.runtime).await;
        // 本轮刚攒满阈值就在同一轮里被确认了一次 ⇒ learn 一定排在 confirm 之前
        // （`start_paused` 的自动推进可能又跑了几轮，所以只断言 ≥ 阈值）
        assert!(r1.candidates[&key].hits >= CANDIDATE_THRESHOLD);
        assert_eq!(r1.candidates[&key].confirms, 1, "间隔没到，后续轮次不加第 2 次");
        assert!(r1.pending.is_empty(), "一轮不够");
        // 往前推两轮：t=300 那轮间隔不够（<10 分钟）不算，t=600 那轮才是第 2 次确认
        host.advance(JOURNAL_POLL_SECS as i64 * 2);
        tokio::time::advance(std::time::Duration::from_secs(JOURNAL_POLL_SECS * 2 + 1)).await;
        for _ in 0..500 {
            if !rstate::read(&c.runtime).await.pending.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let r2 = rstate::read(&c.runtime).await;
        assert_eq!(r2.pending.len(), 1, "≥10 分钟的两次确认就够，不必等 04:00");
        assert_eq!(r2.pending[0].host, "gateway.icloud.com");
        assert_eq!(r2.pending[0].confirms, CONFIRM_NEEDED);
        // 「进了 pending 就出候选池」由 confirmation_needs_two_hard_rejects… 断言：
        // 这里 loop 还在跑，同一条会被后续轮次重新学成候选，不能在这里断言它不存在
        // pending 不生效：state 一个字没动 ⇒ relay 不重启，写入等 04:00 或「立即应用」
        assert!(rstate::group_of(&c.store.read().await).blacklist.auto.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn a_target_that_is_also_unreachable_directly_is_never_blacklisted() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        rstate::update(&c.runtime, |r| {
            r.candidates.insert(
                candidate_key(Uuid::from_u128(1), "dead.example.com", 443),
                rstate::Candidate {
                    upstream_id: Uuid::from_u128(1), host: "dead.example.com".into(), port: 443,
                    hits: CANDIDATE_THRESHOLD,
                    first_seen: crate::util::fmt_rfc3339(host.now()),
                    last_seen: crate::util::fmt_rfc3339(host.now()),
                    confirms: 0, last_confirm_at: None,
                },
            );
        })
        .await;
        // 上游拒 + 直连也不通 ⇒ 不是代理的错（R13 §5.3），confirm_once 返回 None：
        // 既不累计确认，也不当成「通了」去累计复核 pass
        let p = rejector(&["dead.example.com:443"], &[]);
        confirm_round(&c, p.clone()).await.unwrap();
        host.advance(601);
        assert_eq!(confirm_round(&c, p).await.unwrap(), 0);
        let r = rstate::read(&c.runtime).await;
        assert!(r.pending.is_empty());
        assert_eq!(
            r.candidates[&candidate_key(Uuid::from_u128(1), "dead.example.com", 443)].confirms,
            0,
            "未知不推进确认"
        );
    }

    #[test]
    fn port_learn_only_rewrites_the_whitelist_after_a_clean_round() {
        let p = |pairs: &[(u16, &str)]| -> BTreeMap<u16, String> {
            pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
        };
        // 调研 §D 的 Decodo 形态：只放行 80/443
        let decodo = p(&[
            (22, "refused:403"), (80, "open"), (443, "open"), (853, "refused:403"),
            (993, "refused:403"), (5223, "refused:403"), (5228, "refused:403"), (8080, "refused:403"),
        ]);
        assert_eq!(port_learn(&decodo, false), PortLearn::Learned(Some(vec![80, 443])));
        // 407 / 抖动 / 空表 ⇒ 保留上次学到的白名单（改成「不限」会静默放开端口策略）
        assert_eq!(port_learn(&decodo, true), PortLearn::Keep);
        let mut flaky = decodo.clone();
        flaky.insert(993, "unreachable".into());
        assert_eq!(port_learn(&flaky, false), PortLearn::Keep);
        let mut bad_auth = decodo.clone();
        bad_auth.insert(993, "auth_failed".into());
        assert_eq!(port_learn(&bad_auth, false), PortLearn::Keep);
        assert_eq!(port_learn(&BTreeMap::new(), false), PortLearn::Keep);
        // 全通 ⇒ 不限
        let all_open: BTreeMap<u16, String> =
            [22u16, 80, 443, 853, 993, 5223, 5228, 8080].iter().map(|k| (*k, "open".into())).collect();
        assert_eq!(port_learn(&all_open, false), PortLearn::Learned(None));
    }

    #[tokio::test]
    async fn a_failing_journalctl_resets_the_cursor_so_the_next_round_rescans() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        rstate::update(&c.runtime, |r| r.journal_cursor = Some("s=stale".into())).await;
        host.with(|i| {
            i.scripted.push((
                format!(
                    "journalctl -u {} --no-pager -o cat --show-cursor --after-cursor s=stale",
                    crate::modules::residential::JOURNAL_UNIT
                ),
                CmdOut::failure(1, "Failed to seek to cursor: Invalid argument"),
            ));
        });
        assert_eq!(learn_from_journal(&c).await.unwrap(), 0);
        assert_eq!(
            rstate::read(&c.runtime).await.journal_cursor,
            None,
            "游标失效就丢掉，下一轮用 --since -25h 重扫（T5 的 collect 对非零退出返回 Err）"
        );
    }

    #[tokio::test]
    async fn the_daily_round_applies_pending_reviews_autos_and_persists_the_selection() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        // 一条待生效、一条已生效且已连过两次复核的 auto
        rstate::update(&c.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: Uuid::from_u128(1), host: "gateway.icloud.com".into(), port: 443,
                hits: 9, confirms: CONFIRM_NEEDED,
                last_confirm_at: crate::util::fmt_rfc3339(host.now()),
            });
            // runtime 记的是 uuid（契约决策 §C），04:00 写回不需要任何 tag 换算
            r.selected_upstream_id = Some(Uuid::from_u128(1));
            r.selected_pending_persist = true;
        })
        .await;
        rstate::update_group(&c.store, &c.bus, |g| {
            g.selected_upstream_id = None;     // 与 runtime 的「当前生效」漂移
            g.blacklist.auto.push(AutoEntry {
                upstream_id: Uuid::from_u128(1),
                rule: Rule::DomainSuffix("x.com".into()),
                hits: 3, confirmed_at: "2026-09-10T00:00:00Z".into(),
                last_verified_at: "2026-09-11T00:00:00Z".into(),
                passes: REVIEW_PASSES_TO_REMOVE - 1,
            });
        })
        .await
        .unwrap();
        // x.com 现在通了（第 3 次 pass ⇒ 移除）；探针集全通
        let p = rejector(&[], &[]);
        let rep = daily_round(&c, p).await.unwrap();
        assert_eq!(rep.applied, 1);
        assert_eq!(rep.removed, 1);
        assert_eq!(rep.ports_learned, 1, "spec §5.4 (b)：端口集也每日重探一遍");
        assert!(rep.persisted_selection);
        let g = rstate::group_of(&c.store.read().await);
        assert_eq!(
            g.blacklist.auto.iter().map(|e| e.rule.clone()).collect::<Vec<_>>(),
            vec![Rule::DomainSuffix("gateway.icloud.com".into())],
            "规则值就是被拒的完整主机名，不泛化到注册域名"
        );
        assert_eq!(g.selected_upstream_id, Some(Uuid::from_u128(1)), "漂移在 04:00 窗口写回（D4）");
        assert_eq!(g.upstreams[0].ports_allowed, None, "端口集全通 ⇒ 学成「不限」，同一次写盘落地");
        let r = rstate::read(&c.runtime).await;
        assert!(r.pending.is_empty());
        assert!(!r.selected_pending_persist);
        assert_eq!(r.last_daily_at.as_deref(), Some(&*crate::util::fmt_rfc3339(host.now())));
    }

    #[tokio::test]
    async fn the_daily_probe_set_seeds_candidates_without_any_log_evidence() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.scripted.push((
                format!("journalctl -u {} --no-pager", crate::modules::residential::JOURNAL_UNIT),
                CmdOut::success("-- cursor: s=1\n"),
            ));
        });
        let p = rejector(&["pay.google.com:443"], &["pay.google.com:443"]);
        // 第一轮 04:00：探针集把它推成候选（日志里一条证据都没有也行，spec §5.4 (b)）
        let first = daily_round(&c, p.clone()).await.unwrap();
        assert_eq!(first.probed, 1);
        assert_eq!(first.applied, 0, "04:00 只批量生效已确认的；确认归 journal_loop");
        let key = candidate_key(Uuid::from_u128(1), "pay.google.com", 443);
        assert_eq!(rstate::read(&c.runtime).await.candidates[&key].hits, CANDIDATE_THRESHOLD);
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        // journal_loop 的两轮确认（间隔 ≥ 10 分钟）把它升进 pending
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 0);
        host.advance(601);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 1);
        // 下一个 04:00 窗口只做批量生效
        let second = daily_round(&c, p).await.unwrap();
        assert_eq!(second.applied, 1);
        assert_eq!(
            rstate::group_of(&c.store.read().await).blacklist.auto[0].rule,
            Rule::DomainSuffix("pay.google.com".into())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_daily_loop_fires_only_inside_the_window_hour_and_only_once_per_hour() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.now = time::macros::datetime!(2026-09-12 03:50:00 UTC);
            // scripted 是前缀匹配，一条就够覆盖两种游标形态
            i.scripted.push((
                format!("journalctl -u {} --no-pager", crate::modules::residential::JOURNAL_UNIT),
                CmdOut::success("-- cursor: s=1\n"),
            ));
        });
        let task = tokio::spawn(daily_loop(c.clone(), rejector(&[], &[])));
        // 03:50 不在窗口里：推两个 tick 也不该跑
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS * 2 + 1)).await;
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
        assert_eq!(rstate::read(&c.runtime).await.last_daily_at, None, "不是 04 点就不跑");
        // 进入窗口
        host.with(|i| i.now = time::macros::datetime!(2026-09-12 04:05:00 UTC));
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS + 1)).await;
        for _ in 0..500 {
            if rstate::read(&c.runtime).await.last_daily_at.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let first = rstate::read(&c.runtime).await.last_daily_at;
        assert!(first.is_some(), "04:05 跑了一轮");
        // 同一小时内再到点也只跑这一次（否则一小时会重启 relay 六次）
        host.with(|i| i.now = time::macros::datetime!(2026-09-12 04:40:00 UTC));
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS + 1)).await;
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
        assert_eq!(rstate::read(&c.runtime).await.last_daily_at, first, "同一小时只跑一次");
        task.abort();
    }

    #[tokio::test]
    async fn pins_and_apply_now_take_effect_immediately() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let mut rx = c.bus.subscribe();
        add_pin(&c, Rule::DomainSuffix("pay.google.com".into()), "支付必须直连".into()).await.unwrap();
        assert_eq!(
            rstate::group_of(&c.store.read().await).blacklist.pins[0].rule,
            Rule::DomainSuffix("pay.google.com".into())
        );
        assert_eq!(rx.try_recv().unwrap(), crate::api::Event::StateChanged("residential"),
                   "pin 立刻触发对账 → 重渲染 relay → 重启");
        // 同一条 pin 再加一次是幂等的
        add_pin(&c, Rule::DomainSuffix("pay.google.com".into()), "".into()).await.unwrap();
        assert_eq!(rstate::group_of(&c.store.read().await).blacklist.pins.len(), 1);
        assert!(remove_pin(&c, &Rule::DomainSuffix("pay.google.com".into())).await.unwrap());
        assert!(!remove_pin(&c, &Rule::DomainSuffix("pay.google.com".into())).await.unwrap());
        // apply_now：pending → auto
        rstate::update(&c.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: Uuid::from_u128(1), host: "gateway.icloud.com".into(), port: 443,
                hits: 3, confirms: CONFIRM_NEEDED,
                last_confirm_at: crate::util::fmt_rfc3339(host.now()),
            });
        })
        .await;
        assert_eq!(apply_now(&c).await.unwrap(), 1);
        assert_eq!(rstate::group_of(&c.store.read().await).blacklist.auto.len(), 1);
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        // 手动删一条 auto（面板「误伤了」）
        assert!(remove_auto(&c, Uuid::from_u128(1), &Rule::DomainSuffix("gateway.icloud.com".into()))
            .await
            .unwrap());
        assert!(rstate::group_of(&c.store.read().await).blacklist.auto.is_empty());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::blacklist`
Expected: 编译失败，`cannot find function `learn_from_journal``。

- [ ] **Step 3: 实现候选学习**

```rust
pub fn is_bare_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

pub fn candidate_key(upstream_id: Uuid, host: &str, port: u16) -> String {
    format!("{upstream_id}|{host}|{port}")
}

pub fn port_allowed(up: &Upstream, port: u16) -> bool {
    // 白名单外端口的硬拒是**端口策略**（ports_allowed 已经表达了它），不是「这个域名
    // 代理不了」。把 courier.push.apple.com:5228 的 403 变成一条 domain_suffix 规则，
    // 会把该上游本来能代理的 443 流量也推回直连（裁决「端口类拒绝不进域名黑名单」）。
    match up.ports_allowed.as_deref() {
        Some(list) => list.contains(&port),
        // 还没体检过 ⇒ 只认基准端口（spec §5.2 的 80/443）
        None => BASE_PORTS.contains(&port),
    }
}

pub async fn learn_from_journal(ctx: &DaemonCtx) -> anyhow::Result<usize> {
    let cursor = state::read(&ctx.runtime).await.journal_cursor;
    let host = ctx.host.clone();
    // **碰 `Host` 一律 spawn_blocking**（与 P1 的 `Fetcher`/`Facts::probe` 同一条铁律）：
    // `RealHost::which` 会同步扫 PATH、`collect` 会 exec journalctl，两件事放进同一次
    // 阻塞任务里，async 上下文里一次 Host 调用都不留。
    let probed = tokio::task::spawn_blocking(move || {
        if !host.which("journalctl") {
            return None;
        }
        Some(journal::collect(host.as_ref(), cursor.as_deref()))
    })
    .await?;
    let Some(collected) = probed else {
        state::update(&ctx.runtime, |r| {
            state::push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
        })
        .await;
        return Ok(0);
    };
    let batch = match collected {
        Ok(b) => b,
        Err(e) => {
            // 游标失效（日志轮转/重启）→ 丢掉游标，下一轮用 --since -25h 重来
            tracing::warn!(error = %e, "journalctl 读取失败，重置游标");
            state::update(&ctx.runtime, |r| r.journal_cursor = None).await;
            return Ok(0);
        }
    };
    let g = state::group_of(&ctx.store.read().await);
    let now = crate::util::fmt_rfc3339(ctx.host.now());
    let mut learned = 0usize;
    let lines = batch.lines.clone();
    state::update(&ctx.runtime, |r| {
        for l in &lines {
            // 裸 IP 目标（推送、非白名单端口）由 ports_allowed 表达，不进黑名单（spec §5.4）
            if is_bare_ip(&l.host) {
                continue;
            }
            let Some(id) = clash::id_of_tag(&g, &l.tag) else { continue };
            // 端口类拒绝不进域名黑名单（裁决）：白名单外端口一律不计候选
            let Some(up) = g.upstreams.iter().find(|u| u.id == id) else { continue };
            if !port_allowed(up, l.port) {
                continue;
            }
            let key = candidate_key(id, &l.host, l.port);
            let e = r.candidates.entry(key).or_insert_with(|| state::Candidate {
                upstream_id: id,
                host: l.host.clone(),
                port: l.port,
                hits: 0,
                first_seen: now.clone(),
                last_seen: now.clone(),
                confirms: 0,
                last_confirm_at: None,
            });
            e.hits += 1;
            e.last_seen = now.clone();
            learned += 1;
        }
        if batch.cursor.is_some() {
            r.journal_cursor = batch.cursor.clone();
        }
    })
    .await;
    Ok(learned)
}

pub async fn journal_loop(ctx: DaemonCtx, p: Arc<dyn Prober>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(JOURNAL_POLL_SECS));
    loop {
        tick.tick().await;
        // 顺序是裁决的一部分：**先 learn 再 confirm**。本轮新攒到阈值的候选立刻就能
        // 做第 1 次确认，于是「间隔 ≥ CONFIRM_MIN_GAP_SECS、连续 CONFIRM_NEEDED 次」
        // 最快 10 分钟走完（第 1 轮 + t=600 秒那轮），而不是一天一次、要两天。
        if let Err(e) = learn_from_journal(&ctx).await {
            tracing::warn!(error = %e, "黑名单候选学习失败");
        }
        match confirm_round(&ctx, p.clone()).await {
            Ok(n) if n > 0 => tracing::info!(promoted = n, "黑名单候选确认完毕，等 04:00 批量生效"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "黑名单候选确认失败"),
        }
    }
}
```

- [ ] **Step 4: 实现确认状态机与端口集学习**

```rust
pub fn confirm_once(p: &dyn Prober, up: &Upstream, host: &str, port: u16) -> Option<bool> {
    match p.connect(up, host, port) {
        // 硬拒 + 直连可达 ⇒ 确认是「这个上游代理不了它」（R13 §5.3）
        ConnectVerdict::Refused { .. } if p.direct_tcp(host, port) => Some(true),
        // 硬拒但直连也不通 ⇒ 未知。**不能返回 Some(false)**：复核路径会把它当 pass 累计，
        // 连续 3 天目标自己挂着就会误删一条本该保留的 auto 条目
        ConnectVerdict::Refused { .. } => None,
        ConnectVerdict::Open => Some(false),
        // 凭据失效 / 连不上 ⇒ 未知，不改状态（绝不因为一轮失败清空或误加）
        ConnectVerdict::AuthFailed | ConnectVerdict::Unreachable { .. } => None,
    }
}

pub async fn confirm_round(ctx: &DaemonCtx, p: Arc<dyn Prober>) -> anyhow::Result<usize> {
    let g = state::group_of(&ctx.store.read().await);
    let r = state::read(&ctx.runtime).await;
    let now = ctx.host.now();
    // 只探「达到阈值」且「距上次确认 ≥ 10 分钟」的候选：间隔是 spec §5.4 的硬要求，
    // 没有它同一波抖动会在一轮里连中两次直接进黑名单
    let due: Vec<(String, state::Candidate, Upstream)> = r
        .candidates
        .iter()
        .filter(|(_, c)| c.hits >= CANDIDATE_THRESHOLD)
        .filter(|(_, c)| match c.last_confirm_at.as_deref().and_then(parse_rfc3339) {
            Some(t) => (now - t).whole_seconds() >= CONFIRM_MIN_GAP_SECS,
            None => true,
        })
        .filter_map(|(k, c)| {
            let up = g.upstreams.iter().find(|u| u.id == c.upstream_id)?.clone();
            Some((k.clone(), c.clone(), up))
        })
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    let pp = p.clone();
    let verdicts = super::fanout(due, move |(k, c, up)| {
        (k, confirm_once(pp.as_ref(), &up, &c.host, c.port))
    })
    .await;

    let mut promoted = 0usize;
    let now_s = crate::util::fmt_rfc3339(now);
    state::update(&ctx.runtime, |r| {
        for (key, verdict) in verdicts.into_iter().flatten() {
            let Some(c) = r.candidates.get_mut(&key) else { continue };
            match verdict {
                // 未知（凭据失效 / 连不上 / 硬拒但直连也不通）：一个字段都不动
                None => {}
                Some(false) => {
                    // 现在通了：确认进度与计数一起归零，别把一次抖动攒成黑名单
                    c.confirms = 0;
                    c.hits = 0;
                    c.last_confirm_at = Some(now_s.clone());
                }
                Some(true) => {
                    c.confirms += 1;
                    c.last_confirm_at = Some(now_s.clone());
                }
            }
        }
        // 攒满 CONFIRM_NEEDED 次的候选升进 pending 并出候选池，等 04:00 批量生效。
        // 进了 pending 就等于「确认完毕」，所以 flush_pending / apply_now 不再判 confirms。
        let done: Vec<String> = r
            .candidates
            .iter()
            .filter(|(_, c)| c.confirms >= CONFIRM_NEEDED)
            .map(|(k, _)| k.clone())
            .collect();
        for key in done {
            let Some(c) = r.candidates.remove(&key) else { continue };
            r.pending
                .retain(|e| !(e.upstream_id == c.upstream_id && e.host == c.host && e.port == c.port));
            r.pending.push(state::PendingEntry {
                upstream_id: c.upstream_id,
                host: c.host,
                port: c.port,
                hits: c.hits,
                confirms: c.confirms,
                last_confirm_at: now_s.clone(),
            });
            promoted += 1;
        }
    })
    .await;
    Ok(promoted)
}

pub fn port_learn(ports: &BTreeMap<u16, String>, auth_failed: bool) -> PortLearn {
    // 凭据失效、任一项探测失败、或一个端口都没探到 ⇒ 本轮结论不可信，保留上次的白名单。
    // 这里若改成「写 None（不限）」，一次网络抖动就会静默放开端口策略。
    if auth_failed
        || ports.is_empty()
        || ports.values().any(|v| v == "unreachable" || v == "auth_failed")
    {
        return PortLearn::Keep;
    }
    PortLearn::Learned(check::derive_ports_allowed(ports, false))
}

pub async fn probe_ports_daily(p: Arc<dyn Prober>, ups: &[Upstream]) -> Vec<(Uuid, PortLearn)> {
    let plist: Vec<u16> = BASE_PORTS.iter().chain(PROBE_PORTS.iter()).copied().collect();
    let jobs: Vec<(Upstream, u16)> = ups
        .iter()
        .flat_map(|u| plist.iter().map(move |port| (u.clone(), *port)))
        .collect();
    let pp = p.clone();
    // 目标主机固定 www.google.com：端口策略与目标无关，只看隧道能不能开（与 T6 同判据）
    let probed = super::fanout(jobs.clone(), move |(up, port)| {
        pp.connect(&up, "www.google.com", port)
    })
    .await;
    let mut per_up: BTreeMap<Uuid, (BTreeMap<u16, String>, bool)> = BTreeMap::new();
    for (i, (up, port)) in jobs.iter().enumerate() {
        let e = per_up.entry(up.id).or_default();
        match probed.get(i).and_then(|x| x.as_ref()) {
            // fanout 的 None = 该项的阻塞任务 panic 了
            None => {
                e.0.insert(*port, "unreachable".into());
            }
            Some(v) => {
                if *v == ConnectVerdict::AuthFailed {
                    e.1 = true;
                }
                e.0.insert(*port, v.label());
            }
        }
    }
    per_up
        .into_iter()
        .map(|(id, (ports, af))| (id, port_learn(&ports, af)))
        .collect()
}
```

- [ ] **Step 5: 实现每日窗口、`apply_now` 与 pins**

```rust
/// `pending` → `state.blacklist.auto`（同一条重复出现时只累加 hits），返回写入条数
async fn flush_pending(ctx: &DaemonCtx, extra: impl FnOnce(&mut bui_schema::model::ResidentialGroup))
    -> anyhow::Result<usize>
{
    let pending = state::read(&ctx.runtime).await.pending;
    if pending.is_empty() {
        // 仍要执行 extra（复核移除 / 落点写回也走这条路径）
        state::update_group(&ctx.store, &ctx.bus, extra).await?;
        return Ok(0);
    }
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    let n = pending.len();
    let items = pending.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        for e in items {
            // 规则值就是被拒的完整主机名；domain_suffix 已覆盖其子域，不泛化到注册域名
            let rule = Rule::DomainSuffix(e.host.clone());
            match g
                .blacklist
                .auto
                .iter_mut()
                .find(|a| a.upstream_id == e.upstream_id && a.rule == rule)
            {
                Some(a) => {
                    a.hits += e.hits;
                    a.last_verified_at = at.clone();
                    a.passes = 0;
                }
                None => g.blacklist.auto.push(AutoEntry {
                    upstream_id: e.upstream_id,
                    rule,
                    hits: e.hits,
                    confirmed_at: at.clone(),
                    last_verified_at: at.clone(),
                    passes: 0,
                }),
            }
        }
        extra(g);
    })
    .await?;
    state::update(&ctx.runtime, |r| r.pending.clear()).await;
    Ok(n)
}

pub async fn apply_now(ctx: &DaemonCtx) -> anyhow::Result<usize> {
    flush_pending(ctx, |_| {}).await
}

pub async fn daily_round(ctx: &DaemonCtx, p: Arc<dyn Prober>) -> anyhow::Result<DailyReport> {
    let mut rep = DailyReport::default();
    let g = state::group_of(&ctx.store.read().await);
    if !g.pool_active() {
        rep.notes.push("住宅池未启用，跳过每日黑名单轮次".into());
        return Ok(rep);
    }
    // ① 从日志学
    rep.learned = learn_from_journal(ctx).await?;
    // ② 固定探针集：每个上游 × PROBE_SET，把被拒的推成候选（≥ 阈值，等 journal_loop
    // 的确认轮接手）。探针集固定打 443，所以 **443 不在该上游 ports_allowed 白名单里
    // 的上游整条跳过**（裁决「端口类拒绝不进域名黑名单」：那种上游的 443 全被拒是端口
    // 策略，不能变成一堆域名规则）
    let now_s = crate::util::fmt_rfc3339(ctx.host.now());
    let jobs: Vec<(Upstream, String)> = g
        .upstreams
        .iter()
        .filter(|u| port_allowed(u, 443))
        .flat_map(|u| PROBE_SET.iter().map(move |h| (u.clone(), (*h).to_string())))
        .collect();
    let pp = p.clone();
    let probed = super::fanout(jobs.clone(), move |(up, h)| {
        (up.id, h.clone(), confirm_once(pp.as_ref(), &up, &h, 443))
    })
    .await;
    let mut seeded = 0usize;
    state::update(&ctx.runtime, |r| {
        for (id, host, verdict) in probed.into_iter().flatten() {
            if verdict != Some(true) {
                continue;
            }
            seeded += 1;
            let key = candidate_key(id, &host, 443);
            let e = r.candidates.entry(key).or_insert_with(|| state::Candidate {
                upstream_id: id,
                host: host.clone(),
                port: 443,
                hits: 0,
                first_seen: now_s.clone(),
                last_seen: now_s.clone(),
                confirms: 0,
                last_confirm_at: None,
            });
            // 探针集是主动证据，一次就够阈值（阈值只用来过滤日志噪声）
            e.hits = e.hits.max(CANDIDATE_THRESHOLD);
            e.last_seen = now_s.clone();
        }
    })
    .await;
    rep.probed = seeded;
    // ②b spec §5.4 (b) 的端口集部分：每日对 BASE_PORTS ∪ PROBE_PORTS 重探一遍。
    // 手动「体检」按钮也会学 ports_allowed，但没人保证运维每天点它，所以每日轮次必须自己探。
    let ports_learned = probe_ports_daily(p.clone(), &g.upstreams).await;
    rep.ports_learned = ports_learned
        .iter()
        .filter(|(_, l)| matches!(l, PortLearn::Learned(_)))
        .count();
    // ③ **没有确认这一步**：确认归 `journal_loop` 的每 JOURNAL_POLL_SECS 一轮（裁决
    // 「黑名单确认节奏」）。04:00 只做批量生效 + 每日探针/端口集 + 复核；②
    // 刚推成的候选会在接下来的两轮确认（≥10 分钟）里升进 pending，等下一个窗口生效。
    // ④ 复核已生效的 auto：连续 3 次不再被拒 → 移除
    let autos: Vec<(AutoEntry, Upstream)> = g
        .blacklist
        .auto
        .iter()
        .filter_map(|a| {
            let up = g.upstreams.iter().find(|u| u.id == a.upstream_id)?.clone();
            let Rule::DomainSuffix(h) | Rule::Domain(h) = &a.rule else { return None };
            let _ = h;
            Some((a.clone(), up))
        })
        .collect();
    let pp = p.clone();
    let reviewed = super::fanout(autos.clone(), move |(a, up)| {
        let host = match &a.rule {
            Rule::DomainSuffix(h) | Rule::Domain(h) => h.clone(),
            Rule::Port(_) => return (a, None),
        };
        (a.clone(), confirm_once(pp.as_ref(), &up, &host, 443))
    })
    .await;
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    let mut removed = 0usize;
    // ⑤ 一次 Store::update 里完成：批量写入 + 复核更新/移除 + 落点漂移写回（只重启一次 relay）
    let r = state::read(&ctx.runtime).await;
    // 契约决策 §C 的 D4：runtime 存的就是 uuid，不需要任何 tag 换算；上游已被删掉就别写回
    let persist_id = r
        .selected_pending_persist
        .then_some(r.selected_upstream_id)
        .flatten()
        .filter(|id| g.upstreams.iter().any(|u| u.id == *id));
    rep.applied = flush_pending(ctx, |g| {
        for (a, verdict) in reviewed.into_iter().flatten() {
            let Some(cur) = g
                .blacklist
                .auto
                .iter_mut()
                .find(|x| x.upstream_id == a.upstream_id && x.rule == a.rule)
            else { continue };
            match verdict {
                Some(false) => {
                    cur.passes += 1;
                    cur.last_verified_at = at.clone();
                }
                Some(true) => {
                    cur.passes = 0;
                    cur.last_verified_at = at.clone();
                }
                None => {}
            }
        }
        let before = g.blacklist.auto.len();
        g.blacklist.auto.retain(|a| a.passes < REVIEW_PASSES_TO_REMOVE);
        removed = before - g.blacklist.auto.len();
        // ②b 学到的端口白名单在同一次写盘里落地（PortLearn::Keep 的一律不动）
        for (id, learn) in &ports_learned {
            if let PortLearn::Learned(allowed) = learn {
                if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == *id) {
                    u.ports_allowed = allowed.clone();
                }
            }
        }
        // 契约决策 §C 的 D4：自动切换造成的落点漂移在这个窗口写回，不额外多一次重启
        if let Some(id) = persist_id {
            g.selected_upstream_id = Some(id);
        }
    })
    .await?;
    rep.removed = removed;
    rep.persisted_selection = persist_id.is_some();
    let done_at = at.clone();
    state::update(&ctx.runtime, move |r| {
        r.last_daily_at = Some(done_at);
        if persist_id.is_some() {
            r.selected_pending_persist = false;
        }
    })
    .await;
    Ok(rep)
}

pub async fn daily_loop(ctx: DaemonCtx, p: Arc<dyn Prober>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(DAILY_TICK_SECS));
    loop {
        tick.tick().await;
        let now = ctx.host.now();
        // `OffsetDateTime::hour()` 已经是 u8，套 u8::from 会被 clippy::useless_conversion 拦下
        if now.hour() != DAILY_HOUR {
            continue;
        }
        // 同一小时内只跑一次
        let last = state::read(&ctx.runtime).await.last_daily_at.as_deref().and_then(parse_rfc3339);
        if last.map(|t| (now - t).whole_seconds() < 3_600).unwrap_or(false) {
            continue;
        }
        match daily_round(&ctx, p.clone()).await {
            Ok(rep) => tracing::info!(
                learned = rep.learned, probed = rep.probed, applied = rep.applied,
                removed = rep.removed, ports_learned = rep.ports_learned,
                "住宅黑名单每日轮次完成"
            ),
            Err(e) => tracing::warn!(error = %e, "住宅黑名单每日轮次失败"),
        }
    }
}

pub async fn add_pin(ctx: &DaemonCtx, rule: Rule, note: String) -> anyhow::Result<()> {
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        if g.blacklist.pins.iter().any(|p| p.rule == rule) {
            return; // 幂等
        }
        g.blacklist.pins.push(Pin { rule, note, created_at: at });
    })
    .await
}

pub async fn remove_pin(ctx: &DaemonCtx, rule: &Rule) -> anyhow::Result<bool> {
    let g = state::group_of(&ctx.store.read().await);
    if !g.blacklist.pins.iter().any(|p| p.rule == *rule) {
        return Ok(false);
    }
    let rule = rule.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| g.blacklist.pins.retain(|p| p.rule != rule)).await?;
    Ok(true)
}

pub async fn remove_auto(ctx: &DaemonCtx, upstream_id: Uuid, rule: &Rule) -> anyhow::Result<bool> {
    let g = state::group_of(&ctx.store.read().await);
    if !g.blacklist.auto.iter().any(|a| a.upstream_id == upstream_id && a.rule == *rule) {
        return Ok(false);
    }
    let rule = rule.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        g.blacklist.auto.retain(|a| !(a.upstream_id == upstream_id && a.rule == rule));
    })
    .await?;
    Ok(true)
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui modules::residential::blacklist && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 13 passed（含裁决新增的两条：`the_journal_loop_learns_then_confirms_so_ten_minutes_are_enough`、`a_refused_non_whitelisted_port_never_becomes_a_domain_rule`）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/residential/blacklist.rs
git commit -m "feat(resi): 自动黑名单（日志候选、每 5 分钟硬拒确认、04:00 批量生效、每日复核移除）与 pins"
```

---

### Task 10: 面板端点（共 18 条路由：15 条规范 / v3 同名 + 3 条 v3 别名）与全部请求/响应 DTO

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/api.rs`

**Interfaces:**
- Consumes: `crate::modules::residential::{blacklist, check, clash, health, state, upstream, MANUAL_ROUND_MIN_GAP_SECS}` 与 `#[cfg(test)] sample_state_with_pool`；`crate::modules::residential::proxy::Prober`；`crate::modules::residential::clash::Clash`；`crate::api::AppState`；`crate::reconcile::DaemonCtx`；`crate::util::{fmt_rfc3339, parse_rfc3339}`；`axum::{Extension, extract::{State, Path}, routing::{get, post, delete}, Json}`；`bui_schema::model::{Rule, ResiMode, ResidentialGroup}`；`bui_schema::render::SplitRules`
- Produces:
```rust
// crate::modules::residential::api
/// 把 `AppState` 补齐成后台任务用的 `DaemonCtx`（两者字段同源，`paths` 由模块持有）
pub fn ctx_of(app: &AppState, paths: &Paths) -> DaemonCtx;
/// 本模块的全部路由。P1 的 `api::router` 会把它 merge 进 protected 分支并统一
/// `layer(require_admin)`，所以这里**不加**任何鉴权中间件。
pub fn routes(prober: Arc<dyn Prober>, clash: Arc<dyn Clash>, paths: Paths) -> axum::Router<AppState>;
/// 同上，但可关掉 handler 里的后台任务（`post_add` / `post_check` 的 `tokio::spawn`）。
/// **测试一律用 `background = false`**：FakeProber 秒回，后台体检会与下一个请求抢
/// `runtime.checking` 与 `state`，断言随调度时序飘。生产即 `routes(..) = routes_with(.., true)`。
pub fn routes_with(prober: Arc<dyn Prober>, clash: Arc<dyn Clash>, paths: Paths, background: bool)
    -> axum::Router<AppState>;

// ── 请求体 ───────────────────────────────────────────────────────────────
#[derive(Debug, Deserialize)] pub struct AddRequest { pub url: String }
#[derive(Debug, Deserialize)] pub struct RemoveRequest {
    #[serde(default)] pub id: Option<Uuid>,
    /// v3 的定位方式 `"host:port"`
    #[serde(default)] pub host_port: Option<String>,
}
#[derive(Debug, Deserialize)] pub struct GlobalRequest { pub global: bool }
/// v3 的 `POST /api/residential/enable` 是**空体、且不带 `Content-Type`**，所以 handler 的
/// 提取器必须是 `Option<Json<EnableRequest>>`（`None` ⇒ `enabled = true`）；`#[serde(default)]`
/// 只救得了 `{}` 这种「有 JSON 但缺字段」的请求，救不了「没有 body」的 415
#[derive(Debug, Deserialize)] pub struct EnableRequest { #[serde(default = "default_true")] pub enabled: bool }
fn default_true() -> bool { true }   // 本文件私有，不往 P1 的 util.rs 加东西
#[derive(Debug, Deserialize)] pub struct DomainsRequest {
    /// `null` 或缺省 = 回到跟随默认表
    #[serde(default)] pub domains: Option<Vec<String>>,
    #[serde(default)] pub reset: bool,
}
#[derive(Debug, Deserialize)] pub struct CheckRequest { #[serde(default)] pub id: Option<Uuid> }
#[derive(Debug, Deserialize)] pub struct SelectRequest { pub id: Uuid }
#[derive(Debug, Deserialize)] pub struct PinRequest {
    /// `domain_suffix`（缺省）| `domain` | `port`
    #[serde(default)] pub kind: Option<String>,
    pub value: String,
    #[serde(default)] pub note: String,
}
#[derive(Debug, Deserialize)] pub struct ForgetAutoRequest { pub upstream_id: Uuid, pub kind: Option<String>, pub value: String }
#[derive(Debug, Deserialize)] pub struct PriorityRequest { pub id: Uuid, pub priority: u32 }
/// v3 的 `POST /api/residential`（一条路径三种语义：加上游 / 设关键字 / 恢复默认）
#[derive(Debug, Deserialize)] pub struct V3PostRequest {
    #[serde(default)] pub url: Option<String>,
    #[serde(default)] pub domains: Option<Vec<String>>,
    #[serde(default)] pub reset: bool,
}

// ── 响应体 ───────────────────────────────────────────────────────────────
/// `GET /api/residential/status`（= v3 `GET /api/residential`）。
/// **前 8 个字段名逐字照 v3**（`web/app.js:827/875/964` 直接读），其余是 v4 追加项。
#[derive(Debug, Serialize, PartialEq)]
pub struct StatusResponse {
    pub enabled: bool,
    pub global: bool,
    pub urls: Vec<UrlRow>,
    pub domains: Vec<String>,
    #[serde(rename = "domainsFollowDefault")] pub domains_follow_default: bool,
    #[serde(rename = "lastVerifiedIp")] pub last_verified_ip: String,
    #[serde(rename = "lastVerifiedIspInfo")] pub last_verified_isp_info: String,
    // v4 追加（只追加，不改不删任何 v3 字段）
    pub mode: ResiMode,
    /// **state 里的配置落点**（selector 的 default、`ports_allowed` 取反与 `auto` 过滤的依据）
    pub selected_upstream_id: Option<Uuid>,
    /// **当前实际生效的上游**（`runtime.selected_upstream_id`，契约决策 §C）
    pub active_upstream_id: Option<Uuid>,
    /// 上一行对应的成员 tag，由 `clash::tag_of` 现算（面板显示用，不做主键）
    pub active_tag: Option<String>,
    pub selected_pending_persist: bool,
    pub checking: Option<state::Checking>,
    pub blacklist: BlacklistCounts,
    pub upstreams: Vec<UpstreamRow>,
    pub notes: Vec<String>,
    pub alerts: Vec<String>,
}
/// v3 的 `urls[]` 行（`web/app.js:665-699 renderResidentialUrls` 逐字段读）
#[derive(Debug, Serialize, PartialEq)]
pub struct UrlRow {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub name: String,
    /// `"socks5"` | `"http"`（v3 的取值，不是 `UpstreamKind` 的 serde 名 —— 它俩恰好一致，
    /// 但这里显式转，免得以后改了 model 打死前端）
    #[serde(rename = "type")] pub kind: String,
    #[serde(rename = "lastVerifiedIp")] pub last_verified_ip: String,
    #[serde(rename = "displayUrl")] pub display_url: String,
    // v4 追加
    pub id: Uuid,
    pub priority: u32,
}
/// v4 追加的上游明细（面板新卡片用；**永不含 password**）
#[derive(Debug, Serialize, PartialEq)]
pub struct UpstreamRow {
    pub id: Uuid, pub name: String, pub kind: String, pub host: String, pub port: u16,
    pub username_masked: String, pub priority: u32,
    pub provider: Option<String>, pub region: Option<String>,
    pub ports_allowed: Option<Vec<u16>>,
    pub verified: Option<bui_schema::model::Verified>,
    /// **只数这一条上游自己的 `auto` 条目**（`auto[].upstream_id == id`），一律经
    /// [`auto_count`]。**不含 `pins`**：pins 是全局强制直连规则，不属于任何上游，
    /// 计进去会让面板上每个上游都凭空多出 `pins.len()` 条。全局计数看
    /// [`StatusResponse::blacklist`]（`BlacklistCounts{pins,auto,pending,candidates}`）
    pub blacklist_count: usize,
    /// 最近一次体检报告（`check::CheckReport` 的 JSON）
    pub check: Option<serde_json::Value>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct BlacklistCounts { pub pins: usize, pub auto: usize, pub pending: usize, pub candidates: usize }
/// `GET /api/residential/health`（= v3 同名端点）。前 10 个字段名逐字照 v3。
#[derive(Debug, Serialize, PartialEq)]
pub struct HealthResponse {
    pub enabled: bool,
    pub urls: Vec<HealthUrlRow>,
    pub domains_count: usize,
    /// `"selector"`（池有效）| `"none"`
    pub mode: String,
    pub selected: Option<String>,
    pub members: Vec<MemberRow>,
    pub current_egress_ip_test: Option<String>,
    pub egress_ip_type: String,
    pub via_proxy_isp: Option<String>,
    // v4 追加
    pub alerts: Vec<String>,
    pub last_daily_at: Option<String>,
    pub notes: Vec<String>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct HealthUrlRow {
    pub host: String, pub port: u16,
    pub last_verified_at: Option<String>, pub last_verified_ip: Option<String>, pub last_verified_isp: Option<String>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct MemberRow {
    pub tag: String, #[serde(rename = "type")] pub kind: String,
    pub host: String, pub port: u16,
    pub active: bool, pub failstreak: u32, pub okstreak: u32,
    pub egress: Option<Egress>,
    // v4 追加
    pub priority: u32, pub success_rate_24h: f64,
    /// 与 [`UpstreamRow::blacklist_count`] **同一语义、同一函数**（[`auto_count`]）：
    /// 只数 `auto[].upstream_id == upstream_id` 的条目，不含 `pins`。`status` 与
    /// `health` 的同名字段必须永远相等，所以两处都不许自己写过滤式
    pub blacklist_count: usize,
    /// 最近一次巡检样本的结果（`None` = 还没探过）
    pub probe_ok: Option<bool>,
    pub upstream_id: Uuid,
}
/// v3 `members[].egress`：`type` 是中文串（`web/app.js:1095` 用 `/IDC|机房/i` 判色）
#[derive(Debug, Serialize, PartialEq)]
pub struct Egress {
    pub ip: Option<String>, #[serde(rename = "type")] pub kind: String,
    pub isp: Option<String>, pub country: Option<String>, pub city: Option<String>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct BlacklistResponse {
    pub pins: Vec<PinRow>,
    pub auto: Vec<AutoRow>,
    pub pending: Vec<PendingRow>,
    pub candidates: Vec<CandidateRow>,
    pub checking: Option<state::Checking>,
    pub last_daily_at: Option<String>,
    /// 面板说明文案（spec §5.4「局限」，逐字照下面的实现）
    pub notes: Vec<String>,
}
#[derive(Debug, Serialize, PartialEq)] pub struct PinRow { pub kind: String, pub value: String, pub note: String, pub created_at: String }
#[derive(Debug, Serialize, PartialEq)] pub struct AutoRow { pub upstream_id: Uuid, pub upstream_name: String, pub kind: String, pub value: String,
                                                            pub hits: u64, pub confirmed_at: String, pub last_verified_at: String, pub passes: u32 }
#[derive(Debug, Serialize, PartialEq)] pub struct PendingRow { pub upstream_id: Uuid, pub host: String, pub port: u16, pub confirms: u32, pub last_confirm_at: String }
#[derive(Debug, Serialize, PartialEq)] pub struct CandidateRow { pub upstream_id: Uuid, pub host: String, pub port: u16, pub hits: u64, pub last_seen: String }
/// 出错时的统一形状，与 v3 一致：`{"error":"…"}`（`web/app.js` 各处读 `r.error`）
#[derive(Debug, Serialize)] pub struct ErrorBody { pub error: String }

/// 一条上游自己的 `auto` 黑名单条目数。**不含 `pins`**（pins 是全局规则，不属于任何
/// 上游）。`UpstreamRow.blacklist_count` 与 `MemberRow.blacklist_count` 都只经这个
/// 函数，免得两处算法漂移——它们在面板上是同一个数字。
pub fn auto_count(g: &ResidentialGroup, id: Uuid) -> usize;
pub fn status_of(s: &State, r: &state::ResiRuntime) -> StatusResponse;
pub fn blacklist_of(s: &State, r: &state::ResiRuntime) -> BlacklistResponse;
/// `"domain_suffix"|"domain"|"port"` + 值 → `Rule`；非法返回 `None`（handler 回 400）
pub fn rule_of(kind: Option<&str>, value: &str) -> Option<Rule>;
/// `Rule` → `(kind, value)`（响应用）
pub fn rule_parts(r: &Rule) -> (String, String);
```

**路由表**（规范路径 + v3 兼容别名；契约决策 §A）。`POST /api/residential/enable` 只存在于 v3 一侧、原样保留，所以别名只多出 **3 条路由 / 5 个「方法 + 路径」对**（与 §A 的口径一致）：

| 方法 + 路径 | handler | 说明 |
|---|---|---|
| `GET /api/residential/status` | `get_status` | 规范 |
| `POST /api/residential/add` | `post_add` | 规范 |
| `POST /api/residential/remove` | `post_remove` | 规范 |
| `POST /api/residential/enable` | `post_enable` | **v3 路径（保留）**——spec §4.3 的规范集合是 `status`/`add`/`remove`/`global`/`domains`/`health`/`restore-default` + `check`/`select`/`blacklist`(GET)/`blacklist/pins`/`blacklist/apply`，里面**没有** `enable`；它只是 v3 的总开关路径（`web/app.js:843`），本计划原样保留、不另起规范名。提取器必须是 `Option<Json<EnableRequest>>`（axum 0.8 的 `OptionalFromRequest`）：v3 前端 `api("/residential/enable",{method:"POST"})` **既没有 body 也没有 `Content-Type`**（`web/app.js:37-50` 只在有 string body 时才加），用 `Json<EnableRequest>` 会被 axum 直接回 **415**，`#[serde(default)]` 只救得了 `{}`。`None` ⇒ `enabled = true` |
| `POST /api/residential/global` | `post_global` | 规范 + v3 同名 |
| `POST /api/residential/domains` | `post_domains` | 规范 |
| `POST /api/residential/restore-default` | `post_restore_default` | 规范 |
| `GET /api/residential/health` | `get_health` | 规范 + v3 同名。**只读 runtime**（裁决 D6），不现探、不推进迟滞 |
| `POST /api/residential/health/check` | `post_health_check` | **需 Fable 裁决 D10**：面板「立即巡检一轮」。跑一次 `health::check_once`（会推进迟滞、可能切换），`MANUAL_ROUND_MIN_GAP_SECS` 内重复调用回 **429**。spec §4.3 未列；否决就删这一行与它的测试，成员状态只由后台每 2 分钟的巡检刷新 |
| `POST /api/residential/check` | `post_check` | 规范（新） |
| `POST /api/residential/select` | `post_select` | 规范（新） |
| `GET /api/residential/blacklist` | `get_blacklist` | 规范（新） |
| `POST /api/residential/blacklist/pins` | `post_pin` | 规范（新） |
| `DELETE /api/residential/blacklist/pins` | `delete_pin` | 规范（新） |
| `POST /api/residential/blacklist/apply` | `post_apply` | 规范（新） |
| `DELETE /api/residential/blacklist` | `delete_auto` | **需 Fable 裁决 D8**：面板「这条误伤了，删掉」。spec §4.3 只列了 `blacklist` 的 GET；删一条 `auto` 是 spec §5.4「面板可删条目」的落点，没有别的路径可挂。Fable 若否决，删掉这一行与它的测试，面板只能等每日复核自动移除 |
| `POST /api/residential/priority` | `post_priority` | **需 Fable 裁决 D7**：`priority` 是切换目标的第一排序键（spec §5.3），不给端点就只能手改 `state.json`。spec §4.3 未列 |
| `GET /api/residential` | `get_status` | v3 别名 |
| `POST /api/residential` | `v3_post`（按体分派：`url` → add、`domains` → domains、`reset` → restore-default） | v3 别名 |
| `DELETE /api/residential` | `post_enable(enabled=false)` | v3 别名（v3 的「禁用」） |
| `POST /api/residential/urls` | `post_add` | v3 别名 |
| `DELETE /api/residential/urls/{host_port}` | `delete_url` → `remove(HostPort)` | v3 别名（路径段是 URL 编码的 `host:port`） |

错误映射（与 v3 的状态码一致，`web/app.js` 只看 `r.error` 文案）：`UpstreamError::{Parse, Unverifiable, AuthFailed, NotProxied, PoolFull, PoolEmpty}` → **400**；`NotFound` → **404**；其余 → **500**。`post_check` 返回 **202** `{started:true, upstream_id}`，已有体检在跑时 **409**（R13 §8.4 同语义）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AppState, EventBus};
    use crate::modules::residential::clash::FakeClash;
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    struct Harness {
        app: axum::Router,
        ctx: DaemonCtx,
        /// 假时钟的句柄（`ctx.host` 是 `Arc<dyn Host>`，没有 `advance`）
        host: Arc<FakeHost>,
        prober: Arc<FakeProber>,
        clash: Arc<FakeClash>,
    }

    async fn harness(d: &tempfile::TempDir) -> Harness {
        // **必须**用本模块的夹具：P1 的 crate::testutil::sample_state() 住宅段是空池
        // （enabled:false / upstreams:[] / pins:[] / auto:[]），下面每一条断言都会崩
        let s = crate::modules::residential::sample_state_with_pool();
        let host = Arc::new(FakeHost::new());
        let store = Store::create(d.path().join("state.json"), s).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let bus = EventBus::new();
        let paths = bui_schema::paths::Paths::default_server();
        let app_state = AppState {
            store: store.clone(), bus: bus.clone(), runtime: runtime.clone(),
            host: host.clone(), started_at: host.now(), version: "4.0.0",
            login: Default::default(),
        };
        let prober = Arc::new(FakeProber::new());
        prober.with(|i| {
            i.gets.insert(
                crate::modules::residential::EXIT_IP_URL.into(),
                Ok(HttpProbe { status: 200, body: "198.51.100.7".into() }),
            );
            i.gets.insert(
                crate::modules::residential::check::EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: serde_json::json!({"ip":"198.51.100.7","asn":33667,
                        "asOrganization":"Comcast","country":"US","isResidential":true}).to_string(),
                }),
            );
            // `POST /api/residential/health/check` 会真探一轮（裁决 D10），这条不给就探不通
            i.gets.insert(
                crate::modules::residential::HEALTH_PROBE_URL.into(),
                Ok(HttpProbe { status: 204, body: String::new() }),
            );
        });
        // 出口画像读缓存（裁决 D6）：预置一份体检报告，否则 egress 只能退回 verified 的 unknown
        let up_id = rstate::group_of(&store.read().await).upstreams[0].id;
        rstate::update(&runtime, |r| {
            // runtime 的「当前生效」记 uuid（契约决策 §C）
            r.selected_upstream_id = Some(up_id);
            r.checks.insert(
                up_id.to_string(),
                serde_json::json!({
                    "class_label": "家庭宽带 IP",
                    "exit": {"ip": "198.51.100.7", "org": "AS33667 Comcast", "country": "US", "city": "Denver"}
                }),
            );
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // 直接用本模块的 routes_with()：P1 的 require_admin 在 api::router 里统一套，
        // 这里单测 handler 本身，不再套一层鉴权（鉴权由 P1 Task 13 的测试覆盖）。
        // `background = false`：FakeProber 秒回，后台体检会与下一个请求抢 runtime.checking，
        // 不关掉 `check_is_202_and_409…` 会随调度时序飘（S4）
        let app =
            routes_with(prober.clone(), clash.clone(), paths.clone(), false).with_state(app_state);
        Harness {
            app,
            ctx: DaemonCtx { store, runtime, bus, host: host.clone(), paths },
            host,
            prober,
            clash,
        }
    }

    async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<serde_json::Value>)
        -> (StatusCode, serde_json::Value)
    {
        let req = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => req.header("content-type", "application/json").body(Body::from(v.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn status_keeps_every_v3_field_name_the_panel_reads() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(st, StatusCode::OK);
        // web/app.js:827-930 读的全部字段
        assert_eq!(v["enabled"], true);
        assert_eq!(v["global"], true);
        assert_eq!(v["urls"][0]["host"], "isp.example.net");
        assert_eq!(v["urls"][0]["port"], 10007);
        assert_eq!(v["urls"][0]["name"], "url-1");
        assert_eq!(v["urls"][0]["type"], "http");
        assert_eq!(v["urls"][0]["username"], "u");
        assert_eq!(v["urls"][0]["lastVerifiedIp"], "198.51.100.7");
        assert_eq!(v["urls"][0]["displayUrl"], "http://u***@isp.example.net:10007");
        assert!(v["domains"].as_array().unwrap().len() >= 67, "keywords=null ⇒ 回生效默认表");
        assert_eq!(v["domainsFollowDefault"], true);
        assert_eq!(v["lastVerifiedIp"], "198.51.100.7", "顶层旧字段由当前落点派生");
        // 密码绝不外泄
        assert!(!serde_json::to_string(&v).unwrap().contains("\"p\""), "响应里不能出现密码");
        // v4 追加项
        assert_eq!(v["mode"], "global");
        let id = rstate::group_of(&h.ctx.store.read().await).upstreams[0].id;
        assert_eq!(v["selected_upstream_id"], id.to_string(), "state 的配置落点");
        assert_eq!(v["active_upstream_id"], id.to_string(), "runtime 的当前生效（uuid，§C）");
        assert_eq!(v["active_tag"], "resi-1", "tag 是现算出来给面板看的，不是主键");
        assert_eq!(v["selected_pending_persist"], false);
        assert_eq!(v["blacklist"]["pins"], 1);
        // blacklist_count 只数这条上游自己的 auto（夹具里恰好 1 条）。夹具同时有 1 条
        // 全局 pin，若实现里手滑加上 pins.len() 这里就会是 2 —— 这条断言专治那个手滑。
        assert_eq!(
            v["upstreams"][0]["blacklist_count"], 1,
            "pins 是全局规则，不计入单个上游：{v}"
        );
        assert_eq!(v["upstreams"][0]["username_masked"], "u***");
        assert!(v["upstreams"][0].get("password").is_none());
        // v3 别名同结果
        let (st2, v2) = call(&h.app, "GET", "/api/residential", None).await;
        assert_eq!((st2, v2), (StatusCode::OK, v));
    }

    #[tokio::test]
    async fn health_keeps_every_v3_field_and_the_chinese_egress_type() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["mode"], "selector");
        assert_eq!(v["selected"], "resi-1");
        assert!(v["domains_count"].as_u64().unwrap() >= 67);
        assert_eq!(v["members"][0]["tag"], "resi-1");
        assert_eq!(v["members"][0]["type"], "http");
        assert_eq!(v["members"][0]["host"], "isp.example.net");
        assert_eq!(v["members"][0]["active"], true);
        assert_eq!(v["members"][0]["failstreak"], 0);
        // GET 只读（裁决 D6）：没探过就是 0，绝不因为「刷新了一下面板」而推进迟滞
        assert_eq!(v["members"][0]["okstreak"], 0);
        assert_eq!(v["members"][0]["probe_ok"], serde_json::Value::Null, "还没探过");
        assert!(h.prober.calls().is_empty(), "GET /health 一次探测都不该发");
        assert!(
            v["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("立即巡检")),
            "面板要知道这些数字是什么时候的：{v}"
        );
        // egress.type 必须是中文串：app.js:1095 用 /IDC|机房/i 判色
        assert_eq!(v["members"][0]["egress"]["ip"], "198.51.100.7");
        assert_eq!(v["egress_ip_type"], "家庭宽带 IP");
        assert_eq!(v["current_egress_ip_test"], "198.51.100.7");
        assert!(v["via_proxy_isp"].as_str().unwrap().contains("Comcast"));
        // v4 追加。与 status 的 upstreams[0].blacklist_count 同源（都走 auto_count）：
        // 夹具里这条上游有 1 条 auto、全局有 1 条 pin，pins 不计入 ⇒ 1 而不是 2
        assert_eq!(v["members"][0]["blacklist_count"], 1);
        let (_, sv) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(
            v["members"][0]["blacklist_count"], sv["upstreams"][0]["blacklist_count"],
            "status 与 health 的同名字段必须永远相等（同一个 auto_count）"
        );
        assert!(v["members"][0]["priority"].is_number());
    }

    #[tokio::test]
    async fn the_manual_round_endpoint_probes_once_and_is_rate_limited() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["healthy"][0], "resi-1");
        assert!(
            h.prober.calls().iter().any(|c| c.starts_with("get:")),
            "这条端点才真的探：{:?}",
            h.prober.calls()
        );
        // 迟滞被推进了（这正是它不能挂在 GET 上的原因）
        let (_, hv) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(hv["members"][0]["okstreak"], 1);
        // 60 秒内再点一次 → 429
        let (st2, v2) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st2, StatusCode::TOO_MANY_REQUESTS);
        assert!(v2["error"].as_str().unwrap().contains("限速"), "{v2}");
        // 过了限速窗口就放行
        h.host.advance(61);
        let (st3, _) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st3, StatusCode::OK);
    }

    #[tokio::test]
    async fn health_on_a_disabled_pool_returns_the_v3_shape() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| g.enabled = false).await.unwrap();
        let (st, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["enabled"], false);
        assert_eq!(v["mode"], "none");
        assert_eq!(v["members"].as_array().unwrap().len(), 0);
        assert_eq!(v["egress_ip_type"], "unknown");
    }

    #[tokio::test]
    async fn add_and_remove_work_on_both_the_canonical_and_the_v3_paths() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        // 规范路径
        let (st, v) = call(&h.app, "POST", "/api/residential/add",
                           Some(serde_json::json!({"url": "http://user1:pw1@isp2.example.net:10007"}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["type"], "http", "v3 的 add 响应字段：success/exitIp/ispInfo/type");
        assert_eq!(v["exitIp"], "198.51.100.7");
        // v3 别名
        let (st, _) = call(&h.app, "POST", "/api/residential/urls",
                           Some(serde_json::json!({"url": "socks5://user1:pw1@isp3.example.net:1080"}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).upstreams.len(), 3);
        // v3 的删除定位方式（URL 编码的 host:port）
        let (st, v) = call(&h.app, "DELETE", "/api/residential/urls/isp3.example.net%3A1080", None).await;
        assert_eq!((st, v["success"].clone()), (StatusCode::OK, serde_json::json!(true)));
        let (st, v) = call(&h.app, "DELETE", "/api/residential/urls/nope.example.net%3A1", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(v["error"].is_string());
        // 规范路径按 id 删
        let id = rstate::group_of(&h.ctx.store.read().await).upstreams[1].id;
        let (st, _) = call(&h.app, "POST", "/api/residential/remove",
                           Some(serde_json::json!({"id": id}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).upstreams.len(), 1);
    }

    #[tokio::test]
    async fn a_bad_paste_returns_400_with_the_parser_message() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "POST", "/api/residential/add",
                           Some(serde_json::json!({"url": "https://u:p@h:1"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("https"), "文案要点名问题：{v}");
        let (st, _) = call(&h.app, "POST", "/api/residential/add",
                           Some(serde_json::json!({"url": "   "}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_v3_post_route_dispatches_by_body() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        // {domains:[…]} → 设关键字
        let (st, v) = call(&h.app, "POST", "/api/residential",
                           Some(serde_json::json!({"domains": ["openai.com"]}))).await;
        assert_eq!((st, v["success"].clone()), (StatusCode::OK, serde_json::json!(true)));
        assert_eq!(
            rstate::group_of(&h.ctx.store.read().await).keywords,
            Some(vec!["openai.com".to_string()])
        );
        // {reset:true} → 回到跟随默认
        let (st, v) = call(&h.app, "POST", "/api/residential", Some(serde_json::json!({"reset": true}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["domainsFollowDefault"], true);
        assert!(v["domains"].as_array().unwrap().len() >= 67);
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).keywords, None);
        // {url:…} → 加上游
        let (st, v) = call(&h.app, "POST", "/api/residential",
                           Some(serde_json::json!({"url": "http://user1:pw1@isp9.example.net:10007"}))).await;
        assert_eq!((st, v["success"].clone()), (StatusCode::OK, serde_json::json!(true)));
        // 空体 → 400（没有语义）
        let (st, _) = call(&h.app, "POST", "/api/residential", Some(serde_json::json!({}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn global_enable_and_disable_match_v3() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "POST", "/api/residential/global",
                           Some(serde_json::json!({"global": false}))).await;
        assert_eq!((st, v["global"].clone()), (StatusCode::OK, serde_json::json!(false)));
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).mode, ResiMode::Split);
        let (st, _) = call(&h.app, "POST", "/api/residential/global", Some(serde_json::json!({}))).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "缺 global 字段由 axum 的 Json 提取器挡掉");
        // v3 的禁用：DELETE /api/residential
        let (st, _) = call(&h.app, "DELETE", "/api/residential", None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(!rstate::group_of(&h.ctx.store.read().await).enabled);
        // v3 的启用：POST /api/residential/enable —— **既没有 body 也没有 Content-Type**
        // （web/app.js:37-50 只在有 string body 时才加），所以 handler 必须收
        // Option<Json<EnableRequest>>；用 Json<..> 这里会是 415（B7）
        let (st, v) = call(&h.app, "POST", "/api/residential/enable", None).await;
        assert_eq!(st, StatusCode::OK, "空体启用不能是 415：{v}");
        assert_eq!(v["enabled"], true);
        assert!(rstate::group_of(&h.ctx.store.read().await).enabled);
        // 显式关
        let (st, v) = call(&h.app, "POST", "/api/residential/enable",
                           Some(serde_json::json!({"enabled": false}))).await;
        assert_eq!((st, v["enabled"].clone()), (StatusCode::OK, serde_json::json!(false)));
        let (st, _) = call(&h.app, "POST", "/api/residential/enable", None).await;
        assert_eq!(st, StatusCode::OK);
        // 空池启用 → 400（v3 同）
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| g.upstreams.clear()).await.unwrap();
        let (st, v) = call(&h.app, "POST", "/api/residential/enable", Some(serde_json::json!({"enabled": true}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("为空"));
    }

    #[tokio::test]
    async fn check_is_202_and_409_while_running_then_select_switches_via_clash() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&h.ctx.store.read().await).upstreams[0].id;
        let (st, v) = call(&h.app, "POST", "/api/residential/check", Some(serde_json::json!({"id": id}))).await;
        assert_eq!(st, StatusCode::ACCEPTED);
        assert_eq!(v["started"], true);
        assert_eq!(v["upstream_id"], id.to_string());
        // harness 用 background = false，所以这里没有后台体检来抢 runtime.checking（S4）：
        // 409 分支靠下面手动置 checking 触发，结果与调度时序无关
        assert_eq!(rstate::read(&h.ctx.runtime).await.checking, None);
        // 人为把 checking 置上，第二次要 409（R13 §8.4）
        rstate::update(&h.ctx.runtime, |r| {
            r.checking = Some(rstate::Checking {
                upstream_id: id,
                started_at: crate::util::fmt_rfc3339(h.ctx.host.now()),
            });
        })
        .await;
        let (st, _) = call(&h.app, "POST", "/api/residential/check", Some(serde_json::json!({"id": id}))).await;
        assert_eq!(st, StatusCode::CONFLICT);
        // select
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| {
            let mut u = g.upstreams[0].clone();
            u.id = Uuid::from_u128(77);
            u.host = "isp2.example.net".into();
            g.upstreams.push(u);
            crate::modules::residential::upstream::renumber(&mut g.upstreams);
        })
        .await
        .unwrap();
        let (st, v) = call(&h.app, "POST", "/api/residential/select",
                           Some(serde_json::json!({"id": Uuid::from_u128(77)}))).await;
        assert_eq!((st, v["tag"].clone()), (StatusCode::OK, serde_json::json!("resi-2")));
        assert_eq!(h.clash.selected().as_deref(), Some("resi-2"));
        let (st, _) = call(&h.app, "POST", "/api/residential/select",
                           Some(serde_json::json!({"id": Uuid::from_u128(999)}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn blacklist_endpoints_read_pins_auto_pending_and_candidates() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/blacklist", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["pins"][0]["kind"], "domain_suffix");
        assert_eq!(v["pins"][0]["value"], "pay.google.com");
        assert!(
            v["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains("软封锁")),
            "spec §5.4 的局限说明要出现在面板里：{v}"
        );
        // 加 pin
        let (st, _) = call(&h.app, "POST", "/api/residential/blacklist/pins",
                           Some(serde_json::json!({"value": "www.paypal.com", "note": "支付直连"}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).blacklist.pins.len(), 2);
        // 非法 kind → 400
        let (st, _) = call(&h.app, "POST", "/api/residential/blacklist/pins",
                           Some(serde_json::json!({"kind": "regex", "value": ".*"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // 端口类 pin
        let (st, _) = call(&h.app, "POST", "/api/residential/blacklist/pins",
                           Some(serde_json::json!({"kind": "port", "value": "5228"}))).await;
        assert_eq!(st, StatusCode::OK);
        // 删 pin
        let (st, _) = call(&h.app, "DELETE", "/api/residential/blacklist/pins",
                           Some(serde_json::json!({"value": "www.paypal.com"}))).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(&h.app, "DELETE", "/api/residential/blacklist/pins",
                           Some(serde_json::json!({"value": "www.paypal.com"}))).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // apply：pending → auto
        let up_id = rstate::group_of(&h.ctx.store.read().await).upstreams[0].id;
        rstate::update(&h.ctx.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: up_id, host: "gateway.icloud.com".into(), port: 443,
                hits: 5, confirms: 2,
                last_confirm_at: crate::util::fmt_rfc3339(h.ctx.host.now()),
            });
        })
        .await;
        let (st, v) = call(&h.app, "POST", "/api/residential/blacklist/apply", None).await;
        assert_eq!((st, v["applied"].clone()), (StatusCode::OK, serde_json::json!(1)));
        let (_, v) = call(&h.app, "GET", "/api/residential/blacklist", None).await;
        assert_eq!(v["auto"].as_array().unwrap().len(), 2);
        assert_eq!(v["auto"][1]["value"], "gateway.icloud.com");
        assert_eq!(v["auto"][1]["upstream_name"], "url-1");
        // 删一条 auto（裁决 D8）
        let (st, _) = call(&h.app, "DELETE", "/api/residential/blacklist",
                           Some(serde_json::json!({"upstream_id": up_id, "value": "gateway.icloud.com"}))).await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn priority_endpoint_updates_the_switch_ordering_key() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&h.ctx.store.read().await).upstreams[0].id;
        let (st, _) = call(&h.app, "POST", "/api/residential/priority",
                           Some(serde_json::json!({"id": id, "priority": 3}))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rstate::group_of(&h.ctx.store.read().await).upstreams[0].priority, 3);
    }

    #[test]
    fn rule_conversion_covers_the_three_kinds_and_rejects_the_rest() {
        assert_eq!(rule_of(None, "a.com"), Some(Rule::DomainSuffix("a.com".into())), "缺省是 domain_suffix");
        assert_eq!(rule_of(Some("domain"), "a.com"), Some(Rule::Domain("a.com".into())));
        assert_eq!(rule_of(Some("port"), "5228"), Some(Rule::Port(5228)));
        assert_eq!(rule_of(Some("port"), "abc"), None);
        assert_eq!(rule_of(Some("regex"), ".*"), None);
        // 主机名校验（v3 R13 §8.4 的 `^[a-z0-9.-]{1,253}$`）
        assert_eq!(rule_of(None, "a b.com"), None);
        assert_eq!(rule_of(None, ""), None);
        assert_eq!(rule_parts(&Rule::Port(853)), ("port".to_string(), "853".to_string()));
        assert_eq!(rule_parts(&Rule::DomainSuffix("a.com".into())), ("domain_suffix".to_string(), "a.com".to_string()));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::api`
Expected: 编译失败，`cannot find function `routes``。

- [ ] **Step 3: 实现 DTO 组装（`status_of` / `blacklist_of` / `rule_of`）**

```rust
pub fn rule_of(kind: Option<&str>, value: &str) -> Option<Rule> {
    let ok_host = |v: &str| {
        !v.is_empty()
            && v.len() <= 253
            && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
    };
    match kind.unwrap_or("domain_suffix") {
        "domain_suffix" => ok_host(value).then(|| Rule::DomainSuffix(value.to_string())),
        "domain" => ok_host(value).then(|| Rule::Domain(value.to_string())),
        "port" => value.parse().ok().map(Rule::Port),
        _ => None,
    }
}

pub fn rule_parts(r: &Rule) -> (String, String) {
    match r {
        Rule::DomainSuffix(v) => ("domain_suffix".into(), v.clone()),
        Rule::Domain(v) => ("domain".into(), v.clone()),
        Rule::Port(p) => ("port".into(), p.to_string()),
    }
}

/// 用户名打码：前两字符 + `***`（v3 `resiRow` 的 `slice(0,2) + "***"`）
fn mask(u: &str) -> String {
    format!("{}***", u.chars().take(2).collect::<String>())
}

/// 一条上游自己的 `auto` 条目数。**`pins` 不计入**：pins 是管理员钉的全局强制直连
/// 规则，跟哪条上游都没关系；把 `pins.len()` 加进来会让面板上每个上游都凭空多出
/// 同样多的条数，运维会以为是这条上游自己学到的。全局计数走 `BlacklistCounts`。
/// `UpstreamRow`（status）与 `MemberRow`（health）都只调这一个函数。
pub fn auto_count(g: &ResidentialGroup, id: Uuid) -> usize {
    g.blacklist.auto.iter().filter(|a| a.upstream_id == id).count()
}

pub fn status_of(s: &State, r: &state::ResiRuntime) -> StatusResponse {
    let g = state::group_of(s);
    // keywords = null/空 ⇒ 回生效默认表，并把 domainsFollowDefault 置 true。
    // 面板要能区分「跟随默认」与「自定义」，否则用户一保存就把当时的默认表固化了（R12）。
    let split = SplitRules::from_group(&g);
    let follow_default = g.keywords.as_ref().map(|k| k.is_empty()).unwrap_or(true);
    let selected = g
        .selected_upstream_id
        .and_then(|id| g.upstreams.iter().find(|u| u.id == id))
        .or_else(|| g.upstreams.first());
    let kind_str = |u: &Upstream| match u.kind {
        UpstreamKind::Http => "http".to_string(),
        UpstreamKind::Socks5 => "socks5".to_string(),
    };
    let urls = g
        .upstreams
        .iter()
        .map(|u| UrlRow {
            host: u.host.clone(),
            port: u.port,
            username: u.username.clone(),
            name: u.name.clone(),
            kind: kind_str(u),
            last_verified_ip: u.verified.as_ref().map(|v| v.ip.clone()).unwrap_or_default(),
            display_url: format!("{}://{}@{}:{}", kind_str(u), mask(&u.username), u.host, u.port),
            id: u.id,
            priority: u.priority,
        })
        .collect();
    let upstreams = g
        .upstreams
        .iter()
        .map(|u| UpstreamRow {
            id: u.id,
            name: u.name.clone(),
            kind: kind_str(u),
            host: u.host.clone(),
            port: u.port,
            username_masked: mask(&u.username),
            priority: u.priority,
            provider: u.provider.clone(),
            region: u.region.clone(),
            ports_allowed: u.ports_allowed.clone(),
            verified: u.verified.clone(),
            blacklist_count: auto_count(&g, u.id),
            check: r.checks.get(&u.id.to_string()).cloned(),
        })
        .collect();
    let v = selected.and_then(|u| u.verified.clone());
    StatusResponse {
        enabled: g.pool_active(),
        global: matches!(g.mode, ResiMode::Global),
        urls,
        domains: split.keywords,
        domains_follow_default: follow_default,
        last_verified_ip: v.as_ref().map(|v| v.ip.clone()).unwrap_or_default(),
        last_verified_isp_info: v
            .as_ref()
            .map(|v| {
                [v.asn.map(|a| format!("AS{a}")), v.org.clone(), v.country.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        mode: g.mode,
        selected_upstream_id: g.selected_upstream_id,
        active_upstream_id: r.selected_upstream_id,
        // tag 现算：runtime 存的是 uuid，池增删后同一个 resi-N 可能已指向别人（§C）
        active_tag: r.selected_upstream_id.and_then(|id| clash::tag_of(&g, id)),
        selected_pending_persist: r.selected_pending_persist,
        checking: r.checking.clone(),
        blacklist: BlacklistCounts {
            pins: g.blacklist.pins.len(),
            auto: g.blacklist.auto.len(),
            pending: r.pending.len(),
            candidates: r.candidates.len(),
        },
        upstreams,
        notes: notes(),
        alerts: r.alerts.clone(),
    }
}

/// 面板说明文案（spec §5.4「局限」；`blacklist` 与 `status` 共用）
fn notes() -> Vec<String> {
    vec![
        "软封锁（上游返回 200 拦截页）无法自动识别，请手动钉住（pins）".into(),
        "端口类拒绝不进黑名单，由该上游体检学到的端口白名单（ports_allowed）表达".into(),
        "自动条目每日 04:00 批量生效；钉住与「立即应用」即时生效，会重启 b-ui-relay 并掐断住宅连接".into(),
    ]
}

pub fn blacklist_of(s: &State, r: &state::ResiRuntime) -> BlacklistResponse {
    let g = state::group_of(s);
    let name_of = |id: Uuid| {
        g.upstreams
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "(已移除)".into())
    };
    BlacklistResponse {
        pins: g
            .blacklist
            .pins
            .iter()
            .map(|p| {
                let (kind, value) = rule_parts(&p.rule);
                PinRow { kind, value, note: p.note.clone(), created_at: p.created_at.clone() }
            })
            .collect(),
        auto: g
            .blacklist
            .auto
            .iter()
            .map(|a| {
                let (kind, value) = rule_parts(&a.rule);
                AutoRow {
                    upstream_id: a.upstream_id,
                    upstream_name: name_of(a.upstream_id),
                    kind,
                    value,
                    hits: a.hits,
                    confirmed_at: a.confirmed_at.clone(),
                    last_verified_at: a.last_verified_at.clone(),
                    passes: a.passes,
                }
            })
            .collect(),
        pending: r
            .pending
            .iter()
            .map(|e| PendingRow {
                upstream_id: e.upstream_id,
                host: e.host.clone(),
                port: e.port,
                confirms: e.confirms,
                last_confirm_at: e.last_confirm_at.clone(),
            })
            .collect(),
        candidates: r
            .candidates
            .values()
            .map(|c| CandidateRow {
                upstream_id: c.upstream_id,
                host: c.host.clone(),
                port: c.port,
                hits: c.hits,
                last_seen: c.last_seen.clone(),
            })
            .collect(),
        checking: r.checking.clone(),
        last_daily_at: r.last_daily_at.clone(),
        notes: notes(),
    }
}
```

- [ ] **Step 4: 实现 handler 与路由表**

`get_health` 的数据来源（**需 Fable 裁决 D6**）：v3 每次请求都对每个成员现拨三个出口画像接口（最多 8 × 3 次 HTTPS），面板一刷新就是十几秒。v4 改成 **`GET` 全部只读 `runtime` + `state`**：成员状态读 `runtime.health`（由后台每 2 分钟的巡检维护），出口画像读 `upstream.verified` 与 `runtime.checks`（由添加上游与「体检」按钮刷新）。

**为什么 `GET` 不现探**：`health::check_once` 会推进迟滞、写 `runtime`、并可能真的切换出口。若 `GET /api/residential/health` 去调它，「刷新面板」就成了一个有副作用的动作——连刷两次就能把一个成员从 healthy 判成 unhealthy 并切走，把 spec §5.3 的「2 分钟一轮 × 2 轮」时间尺度压成「两次点击」，还会与后台 `health_loop` 并发写同一份 runtime。所以「立刻探一轮」独立成 `POST /api/residential/health/check`（裁决 D10），并按 `MANUAL_ROUND_MIN_GAP_SECS = 60` 限速（超频回 429）。代价是 `GET` 里的出口 IP 可能是上一次体检时的值，面板 `notes` 已写明。Fable 若否决 D6，把 `get_health` 里的 `egress_of` 换成现拨 `check::probe_exit`（最多 8×3 次 HTTPS），并把响应时长写进面板说明；若否决 D10，删掉那条路由与它的测试。

```rust
type ApiResult = Result<axum::response::Response, (StatusCode, Json<ErrorBody>)>;

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (code, Json(ErrorBody { error: msg.into() }))
}

/// `UpstreamError` → 状态码（与 v3 一致：校验类 400、找不到 404、其余 500）
fn map_upstream_err(e: upstream::UpstreamError) -> (StatusCode, Json<ErrorBody>) {
    use upstream::UpstreamError as E;
    let code = match &e {
        E::Parse(_) | E::Unverifiable { .. } | E::AuthFailed | E::NotProxied(_) | E::PoolFull | E::PoolEmpty => {
            StatusCode::BAD_REQUEST
        }
        E::NotFound => StatusCode::NOT_FOUND,
        E::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(code, e.to_string())
}

pub fn routes(prober: Arc<dyn Prober>, clash: Arc<dyn Clash>, paths: Paths) -> axum::Router<AppState> {
    routes_with(prober, clash, paths, true)
}

pub fn routes_with(
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    paths: Paths,
    background: bool,
) -> axum::Router<AppState> {
    let d = Deps { prober, clash, paths, background };
    axum::Router::new()
        // ── 规范路径（外加 v3 的 enable：spec §4.3 没有它，原样保留，见 §A）──
        .route("/api/residential/status", get(get_status))
        .route("/api/residential/add", post(post_add))
        .route("/api/residential/remove", post(post_remove))
        .route("/api/residential/enable", post(post_enable))
        .route("/api/residential/global", post(post_global))
        .route("/api/residential/domains", post(post_domains))
        .route("/api/residential/restore-default", post(post_restore_default))
        .route("/api/residential/health", get(get_health))
        .route("/api/residential/health/check", post(post_health_check))
        .route("/api/residential/check", post(post_check))
        .route("/api/residential/select", post(post_select))
        .route("/api/residential/priority", post(post_priority))
        .route("/api/residential/blacklist", get(get_blacklist).delete(delete_auto))
        .route("/api/residential/blacklist/pins", post(post_pin).delete(delete_pin))
        .route("/api/residential/blacklist/apply", post(post_apply))
        // ── v3 路径别名（契约决策 §A；前端零改动，P5 删兼容层时一并删掉本段）──
        .route(
            "/api/residential",
            get(get_status).post(v3_post).delete(post_enable_off),
        )
        .route("/api/residential/urls", post(post_add))
        .route("/api/residential/urls/{host_port}", delete(delete_url))
        // 四个句柄经请求扩展下发给全部 handler。`Router::layer` 只包住**此刻已注册**的
        // 这些路由，`api::router` 后面的 `merge` 与统一 `layer(require_admin)` 都不影响它。
        .layer(axum::Extension(d))
}
```
`Deps` 是 `#[derive(Clone)] struct Deps { prober: Arc<dyn Prober>, clash: Arc<dyn Clash>, paths: Paths, background: bool }`。

**为什么用 `Extension` 而不是「闭包捕获 `Deps`」**：`get({ let d = d.clone(); move |s: State<AppState>| get_status(s, d) })` 这种写法**编译不过**——闭包体把 `d` 整个 move 进了被调函数，于是闭包只能调一次，推出来是 `FnOnce`，而 axum 0.8 的 `Handler` 要求 `Fn + Clone + Send + 'static`（每个请求都要克隆一次调一次）。硬要用闭包就得写成 `move |s: State<AppState>| get_status(s, d.clone())`（闭包内再克隆），那是 18 条路由（其中 3 条挂了两三个方法）× 每个「方法 + 路径」一次 `let d = d.clone()` 加一次 `d.clone()`，共 40 多处样板。`Extension` 只要一行 `.layer(...)`，handler 签名多一个提取器，样板归零。
**提取器顺序**：`Extension<Deps>` 是 `FromRequestParts`，必须排在 `Json`（`FromRequest`，消费 body、只能做最后一个参数）之前——本任务全部 handler 都按 `State<AppState>` → `Extension<Deps>` → `Json<…>` 的顺序写。`post_enable` 的 body 提取器是 `Option<Json<EnableRequest>>`（v3 前端 `api("/residential/enable",{method:"POST"})` 既没有 body 也没有 `Content-Type`，用 `Json<EnableRequest>` 会被 axum 直接回 415，`#[serde(default)]` 只救得了 `{}`）。
不需要 `Deps` 的 handler（`get_status` / `get_blacklist`）**不写**这个提取器，免得 `-D warnings` 下多一个未使用绑定。

关键 handler：
```rust
async fn get_status(State(app): State<AppState>) -> ApiResult {
    // 纯读 state + runtime，不需要 Deps ⇒ 不写那个提取器
    let s = app.store.read().await;
    let r = state::read(&app.runtime).await;
    Ok(Json(status_of(&s, &r)).into_response())
}

async fn post_add(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<AddRequest>,
) -> ApiResult {
    let raw = b.url.trim().to_string();
    if raw.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "请粘贴供应商给的代理，如 socks5://user:pass@host:port 或 host:port:user:pass",
        ));
    }
    let ctx = ctx_of(&app, &d.paths);
    let out = upstream::add(&ctx, d.prober.clone(), &raw).await.map_err(map_upstream_err)?;
    // 录入即探测（R13 §7）：端口白名单与支付/AI 可达性在后台补，面板轮询 checking 消失。
    // `background` 为假时不起（测试里 FakeProber 秒回，后台体检会与下一个请求抢 runtime/state）
    if d.background {
        let (c2, p2, id) = (ctx.clone(), d.prober.clone(), out.id);
        tokio::spawn(async move {
            if let Err(e) = check::run_and_store(&c2, p2, id).await {
                tracing::warn!(error = %e, "新增上游后的体检失败");
            }
        });
    }
    // 响应字段照 v3 的 add：success / exitIp / ispInfo / type
    Ok(Json(serde_json::json!({
        "success": true, "exitIp": out.exit_ip, "ispInfo": out.isp,
        "type": match out.kind { UpstreamKind::Http => "http", UpstreamKind::Socks5 => "socks5" },
        "id": out.id, "name": out.name, "class": out.class_label,
    }))
    .into_response())
}

async fn get_health(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    // **只读**：不碰 Prober / Clash，不推进迟滞，不切换（裁决 D6/D10 的理由见上一段）
    let _ = &d;
    let s = app.store.read().await;
    let g = state::group_of(&s);
    let r = state::read(&app.runtime).await;
    let split = SplitRules::from_group(&g);
    let now = app.host.now();
    // active tag 现算：runtime 存 uuid（§C）
    let active_tag = r.selected_upstream_id.and_then(|id| clash::tag_of(&g, id));
    let mut resp = HealthResponse {
        enabled: g.pool_active(),
        urls: vec![],
        domains_count: split.keywords.len(),
        mode: if g.pool_active() { "selector".into() } else { "none".into() },
        selected: active_tag.clone(),
        members: vec![],
        current_egress_ip_test: None,
        egress_ip_type: "unknown".into(),
        via_proxy_isp: None,
        alerts: r.alerts.clone(),
        last_daily_at: r.last_daily_at.clone(),
        notes: health_notes(),
    };
    if !g.pool_active() {
        return Ok(Json(resp).into_response());
    }
    resp.selected = active_tag.or_else(|| clash::tags(&g).first().cloned());
    for (i, tag) in clash::tags(&g).iter().enumerate() {
        let u = &g.upstreams[i];
        // runtime.health 以 uuid 的字符串形式为键（契约决策 §C），不是 tag
        let h = r.health.get(&u.id.to_string()).cloned().unwrap_or_default();
        resp.members.push(MemberRow {
            tag: tag.clone(),
            kind: match u.kind { UpstreamKind::Http => "http".into(), UpstreamKind::Socks5 => "socks5".into() },
            host: u.host.clone(),
            port: u.port,
            active: h.active,
            failstreak: h.failstreak,
            okstreak: h.okstreak,
            egress: egress_of(u, &r),
            priority: u.priority,
            success_rate_24h: state::success_rate_24h(&h, now),
            // 与 status_of 的 UpstreamRow 同一个函数：面板上是同一个数字
            blacklist_count: auto_count(&g, u.id),
            probe_ok: h.samples.last().map(|x| x.ok),
            upstream_id: u.id,
        });
        resp.urls.push(HealthUrlRow {
            host: u.host.clone(),
            port: u.port,
            last_verified_at: u.verified.as_ref().map(|v| v.at.clone()),
            last_verified_ip: u.verified.as_ref().map(|v| v.ip.clone()),
            last_verified_isp: u.verified.as_ref().and_then(|v| v.org.clone()),
        });
    }
    // 顶层旧字段由「当前生效成员」派生（v3 同语义，面板旧渲染继续可用）
    if let Some(cur) = resp
        .members
        .iter()
        .find(|m| Some(&m.tag) == resp.selected.as_ref())
        .or_else(|| resp.members.first())
    {
        if let Some(e) = &cur.egress {
            resp.current_egress_ip_test = e.ip.clone();
            resp.egress_ip_type = e.kind.clone();
            let place = match (&e.city, &e.country) {
                (Some(c), Some(n)) => format!(" ({c}, {n})"),
                (None, Some(n)) => format!(" ({n})"),
                _ => String::new(),
            };
            let isp = format!("{}{}", e.isp.clone().unwrap_or_default(), place).trim().to_string();
            resp.via_proxy_isp = (!isp.is_empty()).then_some(isp);
        }
    }
    Ok(Json(resp).into_response())
}

/// `GET /api/residential/health` 的面板说明：把「这些数字是什么时候的」讲清楚，
/// 免得运维以为刷新一次就重新探过了（裁决 D6）
fn health_notes() -> Vec<String> {
    vec![
        "成员状态来自最近一轮健康巡检（后台每 2 分钟一轮）；要立刻探一轮请点「立即巡检」（POST /api/residential/health/check，60 秒内限一次）".into(),
        "出口 IP 与归属来自最近一次体检（添加上游或点「体检」时刷新），不是本次请求现拨".into(),
    ]
}

/// 面板「立即巡检一轮」（裁决 D10）：跑一次 `health::check_once`（会推进迟滞、可能切换），
/// 所以按 `MANUAL_ROUND_MIN_GAP_SECS` 限速，超频回 429。
async fn post_health_check(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    let now = app.host.now();
    let r = state::read(&app.runtime).await;
    if let Some(t) = r.last_manual_round_at.as_deref().and_then(crate::util::parse_rfc3339) {
        let gap = (now - t).whole_seconds();
        // 时钟回跳（NTP 校时）不该把按钮永久锁死，所以只在 [0, 上限) 区间内拦
        if (0..MANUAL_ROUND_MIN_GAP_SECS).contains(&gap) {
            return Err(err(
                StatusCode::TOO_MANY_REQUESTS,
                format!("巡检限速中，请 {} 秒后再试", MANUAL_ROUND_MIN_GAP_SECS - gap),
            ));
        }
    }
    let stamp = crate::util::fmt_rfc3339(now);
    state::update(&app.runtime, move |r| r.last_manual_round_at = Some(stamp)).await;
    let ctx = ctx_of(&app, &d.paths);
    let out = health::check_once(&ctx, d.prober.clone(), d.clash.clone())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "healthy": out.healthy,
        "probed": out.probed,
        "switched_to": out.switched_to,
        // 重放与切换是两件事（T8 规则 6a）：面板要能区分「relay 刚重启过，已把你选的出口放回去」
        // 与「你选的出口坏了，已自动切走」
        "replayed_to": out.replayed_to,
        "notes": out.notes,
    }))
    .into_response())
}

/// v3 的空体启用请求（`web/app.js:37-50` 不带 `Content-Type`）：提取器必须是
/// `Option<Json<..>>`，否则 axum 0.8 直接回 415（B7）
async fn post_enable(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    body: Option<Json<EnableRequest>>,
) -> ApiResult {
    let enabled = body.map(|Json(b)| b.enabled).unwrap_or(true);
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_enabled(&ctx, enabled).await.map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true, "enabled": enabled })).into_response())
}

async fn post_enable_off(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    // v3 的「禁用」是 DELETE /api/residential（无 body）
    post_enable(State(app), Extension(d), None).await
}

/// 出口画像来自缓存：优先上一次体检报告，退回 `verified`（裁决 D6）
fn egress_of(u: &Upstream, r: &state::ResiRuntime) -> Option<Egress> {
    if let Some(rep) = r.checks.get(&u.id.to_string()) {
        let ip = rep.pointer("/exit/ip").and_then(|v| v.as_str()).map(str::to_string);
        if ip.is_some() {
            return Some(Egress {
                ip,
                kind: rep.get("class_label").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
                isp: rep.pointer("/exit/org").and_then(|v| v.as_str()).map(str::to_string),
                country: rep.pointer("/exit/country").and_then(|v| v.as_str()).map(str::to_string),
                city: rep.pointer("/exit/city").and_then(|v| v.as_str()).map(str::to_string),
            });
        }
    }
    let v = u.verified.as_ref()?;
    Some(Egress {
        ip: Some(v.ip.clone()),
        // verified 只记归属，没有分类结论 —— 明确写 unknown，不猜
        kind: check::ExitClass::Unknown.label().to_string(),
        isp: v.org.clone(),
        country: v.country.clone(),
        city: None,
    })
}

async fn post_check(State(app): State<AppState>, Extension(d): Extension<Deps>, Json(b): Json<CheckRequest>) -> ApiResult {
    let s = app.store.read().await;
    let g = state::group_of(&s);
    let id = match b.id {
        Some(id) => id,
        None => g.selected_upstream_id.or_else(|| g.upstreams.first().map(|u| u.id))
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "代理节点池为空"))?,
    };
    if !g.upstreams.iter().any(|u| u.id == id) {
        return Err(err(StatusCode::NOT_FOUND, "未找到匹配的上游"));
    }
    drop(s);
    let r = state::read(&app.runtime).await;
    if let Some(c) = r.checking {
        // 超过 10 分钟视为过期（R13 §4），否则一次卡住的体检会永久挡住按钮
        let stale = crate::util::parse_rfc3339(&c.started_at)
            .map(|t| (app.host.now() - t).whole_seconds() > 600)
            .unwrap_or(true);
        if !stale {
            return Err(err(StatusCode::CONFLICT, "已有体检在进行中，请稍候"));
        }
    }
    if d.background {
        let ctx = ctx_of(&app, &d.paths);
        let p = d.prober.clone();
        tokio::spawn(async move {
            if let Err(e) = check::run_and_store(&ctx, p, id).await {
                tracing::warn!(error = %e, "体检失败");
            }
        });
    }
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({ "started": true, "upstream_id": id }))).into_response())
}

/// v3 的 `POST /api/residential`：一条路径三种语义，按体分派（契约决策 §A）
async fn v3_post(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<V3PostRequest>,
) -> ApiResult {
    if let Some(url) = b.url {
        return post_add(State(app), Extension(d), Json(AddRequest { url })).await;
    }
    if b.reset || b.domains.as_ref().map(|x| x.is_empty()).unwrap_or(false) {
        return post_restore_default(State(app), Extension(d)).await;
    }
    match b.domains {
        Some(domains) => {
            post_domains(
                State(app),
                Extension(d),
                Json(DomainsRequest { domains: Some(domains), reset: false }),
            )
            .await
        }
        None => Err(err(StatusCode::BAD_REQUEST, "url 或 domains 字段必填")),
    }
}
```
三个「404 不能靠错误文案判」的 handler 必须逐字照写——`health::select_manual` / `blacklist::remove_pin` / `blacklist::remove_auto` 返回的是 `anyhow::Error` 与 `Ok(bool)`，**没有**可 `downcast` 的错误类型，靠 `e.to_string().contains("不在池里")` 判 404 是个一改文案就坏的隐式契约：

```rust
/// `POST /api/residential/select`（手动切当前出口，只走 Clash API，不写 state）
async fn post_select(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<SelectRequest>,
) -> ApiResult {
    // 404 靠**预检**：select_manual 返回 anyhow::Error（文案「上游不在当前池里」），
    // 没有类型可匹配。用 tag_of 判「在不在池里」，与 select_manual 内部同一判据。
    let g = state::group_of(&app.store.read().await);
    if clash::tag_of(&g, b.id).is_none() {
        return Err(err(StatusCode::NOT_FOUND, "未找到匹配的上游"));
    }
    let ctx = ctx_of(&app, &d.paths);
    let tag = health::select_manual(&ctx, d.clash.clone(), b.id)
        .await
        // 走到这里只剩「Clash API 调用失败」一种可能（池内判据已预检过）
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true, "tag": tag })).into_response())
}

/// `DELETE /api/residential/blacklist/pins`（取消钉住，立即生效 ⇒ 会重启 relay）
async fn delete_pin(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<PinRequest>,
) -> ApiResult {
    let rule = rule_of(b.kind.as_deref(), &b.value)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "规则类型或取值非法"))?;
    let ctx = ctx_of(&app, &d.paths);
    let removed = blacklist::remove_pin(&ctx, &rule)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // `Ok(false)` = 本来就没这条 ⇒ 404，别回 200 让面板以为删掉了
    if !removed {
        return Err(err(StatusCode::NOT_FOUND, "未找到该钉住规则"));
    }
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// `DELETE /api/residential/blacklist`（删一条 auto，裁决 D8；立即生效 ⇒ 会重启 relay）
async fn delete_auto(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<ForgetAutoRequest>,
) -> ApiResult {
    let rule = rule_of(b.kind.as_deref(), &b.value)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "规则类型或取值非法"))?;
    let ctx = ctx_of(&app, &d.paths);
    let removed = blacklist::remove_auto(&ctx, b.upstream_id, &rule)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !removed {
        return Err(err(StatusCode::NOT_FOUND, "未找到该自动黑名单条目"));
    }
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}
```

其余 handler 一律「取 `ctx_of` → 调 T7/T8/T9 的函数 → `map_upstream_err` 映射错误 → 回 v3 形状的 JSON」，签名一律 `State<AppState>` →（需要时）`Extension<Deps>` →（有 body 时）`Json<…>`：`post_add`（回 `AddOutcome`；`d.background` 为真时再 `tokio::spawn` 一次 `check::run_and_store`）、`post_remove`（`id` 优先，否则 `host_port`，两者都缺 400）、`delete_url`（`Path<String>` 解码后 `UpstreamSel::parse_host_port`，解析不出来 400）、`post_global`（回 `{success, global}`）、`post_domains`（回 `{success, domains, domainsFollowDefault}`）、`post_restore_default`（`set_keywords(None)` 后回生效默认表）、`post_priority`（回 `{success, id, priority}`）、`get_blacklist`（不需要 `Deps`，同 `get_status`）、`post_pin`（`rule_of` 为 `None` → 400，回 `{success:true}`）、`post_apply`（回 `{applied: n}`）。

`ctx_of`：
```rust
pub fn ctx_of(app: &AppState, paths: &Paths) -> DaemonCtx {
    DaemonCtx {
        store: app.store.clone(),
        runtime: app.runtime.clone(),
        bus: app.bus.clone(),
        host: app.host.clone(),
        paths: paths.clone(),
    }
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::residential::api && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 12 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/api.rs
git commit -m "feat(resi): 面板端点（13 条规范路径 + v3 别名）与逐字段对齐 v3 的 status/health"
```

---

### Task 11: `ResidentialModule` 收口（`routes` / `spawn` 装配）与端到端串联

**Files:**
- Modify: `crates/bui/src/modules/residential/mod.rs`（把 Task 1 的 `ResidentialModule` 补上两个 `Arc` 字段与 `routes`/`spawn`；`render` 一行不动）

**Interfaces:**
- Consumes: `crate::modules::residential::{api, blacklist, clash::HttpClash, health, proxy::ReqwestProber}`；`crate::reconcile::DaemonCtx`
- Produces:
```rust
pub struct ResidentialModule {
    prober: Arc<dyn proxy::Prober>,
    clash: Arc<dyn clash::Clash>,
    paths: Paths,
}
impl ResidentialModule {
    /// 生产构造：`ReqwestProber` + `HttpClash` + `Paths::default_server()`
    pub fn new() -> Self;
    /// 测试与 M2 演练用：注入 fake
    pub fn with(prober: Arc<dyn proxy::Prober>, clash: Arc<dyn clash::Clash>, paths: Paths) -> Self;
}
impl Module for ResidentialModule {
    fn name(&self) -> &'static str { "residential" }
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> { Vec::new() }   // 不变，见契约决策 §B
    fn routes(&self) -> axum::Router<crate::api::AppState>;                          // = api::routes(...)
    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>>;             // 四个后台任务
}
```
`spawn` 起四个任务，一一对应 spec §5.3/§5.4：

| 任务 | 函数 | 周期 |
|---|---|---|
| 健康巡检 + 切换 | `health::health_loop` | 每 120 秒 |
| relay 重启后重放选择 | `health::replay_loop` | 事件驱动（`Event::RelayRestarted`；`spawn` 先 `bus.subscribe()` 再把 `rx` 传进去） |
| 黑名单候选学习 **+ 确认** | `blacklist::journal_loop(ctx, prober)` | 每 300 秒：先 `learn_from_journal` 再 `confirm_round`（裁决「黑名单确认节奏」） |
| 每日批量生效 + 探针/端口集 + 复核 | `blacklist::daily_loop` | 每 600 秒查一次钟点，命中 04:00 才跑（**不做确认**） |

- [ ] **Step 1: 写失败测试（端到端，只用 fake）**

在 Task 1 的 `mod tests` 里追加：
```rust
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
            store: Store::create(d.path().join("state.json"), sample_state()).await.unwrap(),
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
        // routes() 能被 P1 的 router 形状接住（with_state 编译通过即证明类型对齐）
        let _router: axum::Router<crate::api::AppState> = m.routes();
    }

    #[tokio::test]
    async fn a_state_change_reaches_the_relay_render_through_p1() {
        // 端到端：P3 改 state → P1 的 core_files 渲染出的 relay 配置随之变化。
        // 这条锁住契约决策 §B：P3 不渲染，但它的改动必须真的落到 relay 配置里。
        use crate::modules::core_files::CoreFilesModule;
        use crate::modules::residential::state as rstate;
        use crate::reconcile::{Artifact, Facts, Module as _, RenderCtx};
        // 用带池的夹具：空池时 relay 渲染的是 fail-open 直连形态，加 pin 也看不出差别
        let mut s = sample_state_with_pool();
        let render = |s: &State| -> String {
            let ctx = RenderCtx {
                paths: Paths::default_server(),
                facts: Facts {
                    mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
                    has_ufw: false, ufw_active: false, has_firewalld: false, firewalld_active: false,
                    ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: false,
                },
            };
            CoreFilesModule::new(None)
                .render(s, &ctx)
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
        let g = s.residential.groups.get_mut("default").unwrap();
        g.blacklist.pins.push(bui_schema::model::Pin {
            rule: bui_schema::model::Rule::DomainSuffix("www.paypal.com".into()),
            note: String::new(),
            created_at: "2026-09-12T00:00:00Z".into(),
        });
        let after = render(&s);
        assert!(after.contains("www.paypal.com"), "pin 必须出现在 relay 的 domain_suffix → direct 规则里");
        assert_ne!(before, after, "内容变了 ⇒ P1 的 diff 会写盘并重启 b-ui-relay");
        let _ = rstate::group_of(&s);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::tests`
Expected: 编译失败，`no function or associated item named `with``。

- [ ] **Step 3: 实现 `with` / `routes` / `spawn`**

```rust
impl ResidentialModule {
    pub fn new() -> Self {
        Self::with(
            Arc::new(proxy::ReqwestProber::new()),
            Arc::new(clash::HttpClash::new()),
            Paths::default_server(),
        )
    }

    pub fn with(prober: Arc<dyn proxy::Prober>, clash: Arc<dyn clash::Clash>, paths: Paths) -> Self {
        Self { prober, clash, paths }
    }
}

impl Module for ResidentialModule {
    fn name(&self) -> &'static str {
        "residential"
    }

    /// **空**：见文件头与契约决策 §B（`singbox-relay.json` 归 `core_files`）
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn routes(&self) -> axum::Router<crate::api::AppState> {
        api::routes(self.prober.clone(), self.clash.clone(), self.paths.clone())
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        let (p, c) = (self.prober.clone(), self.clash.clone());
        // **先 subscribe 再 spawn**：broadcast 丢弃「发送时还没有订阅者」的事件，
        // 若让 replay_loop 自己 subscribe，紧随 spawn 的第一次对账重启就可能漏掉（T8 Interfaces）
        let rx = ctx.bus.subscribe();
        vec![
            // spec §5.3：每 2 分钟一轮巡检 + 切换
            tokio::spawn(health::health_loop(ctx.clone(), p.clone(), c.clone())),
            // spec §5.3 最后一句：relay 任何重启后立即重放 runtime.selected_upstream_id
            tokio::spawn(health::replay_loop(ctx.clone(), c, rx)),
            // spec §5.4 (a)：跟随 relay 日志学候选（契约决策 §E：游标增量，不用 -f），
            // 同一轮里紧接着做一次确认 ⇒ ≥10 分钟就能进 pending（裁决「黑名单确认节奏」）
            tokio::spawn(blacklist::journal_loop(ctx.clone(), p.clone())),
            // spec §5.4：每日 04:00 批量生效 + 每日探针/端口集 + 复核移除（不做确认）
            tokio::spawn(blacklist::daily_loop(ctx, p)),
        ]
    }
}
```

- [ ] **Step 4: 运行全量测试**

Run: `cargo test -p bui && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿；`modules::residential` 共 7（T1 的 5 条 + 本任务追加的 2 条）+ 6（T2）+ 10（T3）+ 4（T4）+ 5（T5）+ 10（T6）+ 8（T7）+ 13（T8）+ 13（T9）+ 12（T10）= **88** passed。

- [ ] **Step 5: Commit**

```bash
git add crates/bui/src/modules/residential/mod.rs
git commit -m "feat(resi): 模块收口（routes 装配、四个后台任务、state→relay 端到端断言）"
```

---

### Task 12: `bui residential` CLI 子命令、数字菜单项与 P1 的四处挂接

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/residential/cli.rs`
- Modify: `crates/bui/src/modules/residential/mod.rs`（**删掉** T1 加的那行模块级 `#![allow(dead_code)]`，见 Global Constraints）
- Modify: `crates/bui/src/cli.rs`（`Command` 追加一个变体）
- Modify: `crates/bui/src/main.rs`（`dispatch` 追加一个 arm）
- Modify: `crates/bui/src/commands/menu.rs`（`MenuAction` 追加 `Residential`、`items()` 追加一项；同文件的 `menu_keys_are_stable_and_unique` 断言跟着加 `"8"`）
- Modify: `crates/bui/src/api/health.rs`（`residential: None` → 读 `runtime` 的住宅段）
- Modify: `crates/bui/src/api/mod.rs`（P1 已合并的 `assert_eq!(h.residential, None, "P3 才填")` 换成新断言）

（这四处 P1 文件的改动全是**追加式**，理由见契约决策 §H；批准本计划时把 `bui residential …` 回写总纲 C5 的 CLI 清单。）

**Interfaces:**
- Consumes: `crate::ipc::Client`（`new` / `available` / `request`，P1 Task 14）；`crate::paths::SOCKET_PATH`；`crate::commands::menu::{MenuAction, MenuItem}`；`crate::modules::residential::state`
- Produces:
```rust
// crate::modules::residential::cli
/// `bui residential <子命令>`。**全部**子命令经 unix socket 调本模块自己的 HTTP 端点
/// （spec §2.4「所有菜单项通过 /run/b-ui.sock 调守护进程 API」），不在 CLI 进程里直接
/// 碰 state —— 否则会与守护进程的 `Store` 并发写同一个 `state.json`。
#[derive(Debug, Clone, clap::Subcommand, PartialEq)]
pub enum ResidentialCmd {
    /// 查看住宅池状态（默认子命令）
    Status {
        #[arg(long)] json: bool,
    },
    /// 加一条上游；`-` 表示从 stdin 读一行（凭据不进 argv，`ps` 看不到）
    Add { url: String },
    /// 删一条上游（`<id>` 或 `<host:port>`）
    Remove { target: String },
    /// 总开关
    Enable,
    Disable,
    /// 分流模式
    Global {
        #[arg(value_parser = ["on", "off"])] on_off: String,
    },
    /// 打印生效的分流关键字（JSON 数组，供脚本消费；等价于 v3 `residential-helper.sh domains`）
    Domains,
    /// 分流关键字回到跟随默认表
    RestoreDefault,
    /// 体检（不带 id 则体检当前选中的上游）
    Check {
        #[arg(long)] id: Option<Uuid>,
    },
    /// 手动切换当前出口
    Select { id: Uuid },
    /// 巡检与出口画像
    Health {
        #[arg(long)] json: bool,
    },
    /// 黑名单
    Blacklist {
        #[command(subcommand)] cmd: BlacklistCmd,
    },
}
#[derive(Debug, Clone, clap::Subcommand, PartialEq)]
pub enum BlacklistCmd {
    List { #[arg(long)] json: bool },
    /// 钉住（强制直连）
    Pin { value: String, #[arg(long, default_value = "domain_suffix")] kind: String, #[arg(long, default_value = "")] note: String },
    Unpin { value: String, #[arg(long, default_value = "domain_suffix")] kind: String },
    /// 把已确认的待生效条目立刻写进黑名单（会重启 b-ui-relay）
    Apply,
}

/// 把一个子命令翻译成一次 socket 请求：`(method, path, body)`。
/// **纯函数**，因此 CLI 的全部路由/载荷都能在不起守护进程的情况下单测。
pub fn to_request(cmd: &ResidentialCmd) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)>;
pub fn blacklist_request(cmd: &BlacklistCmd) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)>;
/// `add -` 时从 stdin 读一行（凭据不进 argv）
pub fn resolve_url(raw: &str, stdin: &mut impl std::io::BufRead) -> anyhow::Result<String>;
/// 人类可读输出（`--json` 时直接打印原始 JSON）
pub fn format_status(v: &serde_json::Value) -> String;
pub fn format_health(v: &serde_json::Value) -> String;
pub fn format_blacklist(v: &serde_json::Value) -> String;
/// 派发：`Client::available()` 为假时报「守护进程未运行，请先 `systemctl start b-ui`」
pub async fn run(cmd: ResidentialCmd, socket: PathBuf) -> anyhow::Result<()>;
/// 菜单入口：进住宅子菜单（数字两列，沿用 P1 Task 17 的 `render_with`）
pub async fn menu(socket: PathBuf) -> anyhow::Result<()>;
pub fn menu_items() -> Vec<MenuItem>;
```

**与 P1 菜单的接口**（P1 Task 17 的 `items()` 是唯一入口）：在 `MenuAction` 追加一个变体、`items()` 在 `"7" SSH 硬化` 之后插一项 `"8" 住宅出口`，`run` 的派发里该分支调 `crate::modules::residential::cli::menu(socket)`。

**本任务消费的是 P1 Task 17 尚未合并的代码**（`crates/bui/src/commands/menu.rs` 里的 `MenuItem` / `MenuAction` / `items()` / `render_with()`），签名照 `docs/superpowers/plans/2026-09-11-v4-p1-daemon-core.md`（`MenuItem` 与 `MenuAction` 都 `#[derive(Debug, Clone, PartialEq, Eq)]`；`render_with(items: &[MenuItem], daemon_up: bool) -> String`，`render` 转调 `render_with(items, true)`）。下面 Step 1 的测试用 `{p1:?}` 打印和 `action == MenuAction::Residential` 比较，**直接依赖那两个 derive**；`menu_items()` 的渲染依赖 `render_with` 的第二个参数。**落地前先按真实合并代码复核一遍这三件事**（derive 是否都在、`render_with` 的签名、`items()` 里 `"7"` 那项的 key），不一致就以合并代码为准调整本任务的测试与 `menu_items()`，不要反过来改 P1。住宅子菜单自己再列 8 项（状态 / 加上游 / 删上游 / 体检 / 切换出口 / 分流模式 / 黑名单 / 返回），**需守护进程**（socket 不可用时整项标注并拒绝进入，与 P1 对 4/5 项的处理一致）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn every_subcommand_maps_to_one_endpoint_call() {
        let id = Uuid::from_u128(7);
        let cases: Vec<(ResidentialCmd, (&str, &str, Option<serde_json::Value>))> = vec![
            (ResidentialCmd::Status { json: false }, ("GET", "/api/residential/status", None)),
            (
                ResidentialCmd::Add { url: "socks5://u:p@h:1".into() },
                ("POST", "/api/residential/add", Some(serde_json::json!({"url": "socks5://u:p@h:1"}))),
            ),
            (
                ResidentialCmd::Remove { target: "isp.example.net:10007".into() },
                ("POST", "/api/residential/remove", Some(serde_json::json!({"host_port": "isp.example.net:10007"}))),
            ),
            (
                ResidentialCmd::Remove { target: id.to_string() },
                ("POST", "/api/residential/remove", Some(serde_json::json!({"id": id}))),
            ),
            (ResidentialCmd::Enable, ("POST", "/api/residential/enable", Some(serde_json::json!({"enabled": true})))),
            (ResidentialCmd::Disable, ("POST", "/api/residential/enable", Some(serde_json::json!({"enabled": false})))),
            (
                ResidentialCmd::Global { on_off: "on".into() },
                ("POST", "/api/residential/global", Some(serde_json::json!({"global": true}))),
            ),
            (ResidentialCmd::Domains, ("GET", "/api/residential/status", None)),
            (ResidentialCmd::RestoreDefault, ("POST", "/api/residential/restore-default", None)),
            (ResidentialCmd::Check { id: Some(id) }, ("POST", "/api/residential/check", Some(serde_json::json!({"id": id})))),
            (ResidentialCmd::Check { id: None }, ("POST", "/api/residential/check", Some(serde_json::json!({})))),
            (ResidentialCmd::Select { id }, ("POST", "/api/residential/select", Some(serde_json::json!({"id": id})))),
            (ResidentialCmd::Health { json: true }, ("GET", "/api/residential/health", None)),
        ];
        for (cmd, want) in cases {
            let (m, p, b) = to_request(&cmd).unwrap();
            assert_eq!((m, p.as_str(), b), (want.0, want.1, want.2), "子命令 {cmd:?}");
        }
    }

    #[test]
    fn blacklist_subcommands_map_too() {
        assert_eq!(
            blacklist_request(&BlacklistCmd::List { json: false }).unwrap(),
            ("GET", "/api/residential/blacklist".to_string(), None)
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Pin {
                value: "pay.google.com".into(),
                kind: "domain_suffix".into(),
                note: "支付直连".into()
            })
            .unwrap(),
            (
                "POST",
                "/api/residential/blacklist/pins".to_string(),
                Some(serde_json::json!({"kind": "domain_suffix", "value": "pay.google.com", "note": "支付直连"}))
            )
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Unpin { value: "pay.google.com".into(), kind: "domain_suffix".into() })
                .unwrap()
                .0,
            "DELETE"
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Apply).unwrap(),
            ("POST", "/api/residential/blacklist/apply".to_string(), None)
        );
        // 非法 kind 在 CLI 侧就挡掉，不必往服务端跑一趟
        assert!(blacklist_request(&BlacklistCmd::Pin {
            value: ".*".into(),
            kind: "regex".into(),
            note: String::new()
        })
        .is_err());
    }

    #[test]
    fn add_dash_reads_the_url_from_stdin_so_it_never_hits_argv() {
        let mut input = std::io::Cursor::new(b"socks5://user1:pw1@isp.example.net:1080\n".to_vec());
        assert_eq!(
            resolve_url("-", &mut input).unwrap(),
            "socks5://user1:pw1@isp.example.net:1080"
        );
        let mut empty = std::io::Cursor::new(Vec::new());
        assert!(resolve_url("-", &mut empty).is_err());
        let mut unused = std::io::Cursor::new(Vec::new());
        assert_eq!(resolve_url("http://u:p@h:1", &mut unused).unwrap(), "http://u:p@h:1");
    }

    #[test]
    fn formatters_never_print_a_password_and_cover_the_key_fields() {
        let v = serde_json::json!({
            "enabled": true, "global": true,
            "urls": [{"host": "isp.example.net", "port": 10007, "name": "url-1", "type": "http",
                      "username": "user1", "lastVerifiedIp": "198.51.100.7",
                      "displayUrl": "http://us***@isp.example.net:10007"}],
            "domains": ["openai.com"], "domainsFollowDefault": true,
            "active_tag": "resi-1", "active_upstream_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb",
            "selected_pending_persist": false,
            "blacklist": {"pins": 1, "auto": 2, "pending": 0, "candidates": 3},
            "alerts": ["全部住宅上游探测不达标"]
        });
        let out = format_status(&v);
        assert!(out.contains("已启用"));
        assert!(out.contains("global"));
        assert!(out.contains("url-1"));
        assert!(out.contains("198.51.100.7"));
        assert!(out.contains("跟随默认"));
        assert!(out.contains("resi-1"));
        assert!(out.contains("全部住宅上游探测不达标"));
        assert!(!out.contains("pw1"), "凭据绝不进 CLI 输出");
        let h = serde_json::json!({
            "enabled": true, "mode": "selector", "selected": "resi-1", "domains_count": 67,
            "members": [{"tag": "resi-1", "type": "http", "host": "isp.example.net", "port": 10007,
                         "active": true, "failstreak": 0, "okstreak": 3, "priority": 10,
                         "success_rate_24h": 0.97, "blacklist_count": 2,
                         "egress": {"ip": "198.51.100.7", "type": "家庭宽带 IP", "isp": "AS33667 Comcast"}}],
            "current_egress_ip_test": "198.51.100.7", "egress_ip_type": "家庭宽带 IP", "alerts": []
        });
        let out = format_health(&h);
        assert!(out.contains("resi-1"));
        assert!(out.contains("家庭宽带 IP"));
        assert!(out.contains("97"), "成功率按百分比显示：{out}");
        let b = serde_json::json!({
            "pins": [{"kind": "domain_suffix", "value": "pay.google.com", "note": "", "created_at": "2026-09-12T00:00:00Z"}],
            "auto": [{"upstream_id": "00000000-0000-0000-0000-000000000001", "upstream_name": "url-1",
                      "kind": "domain_suffix", "value": "gateway.icloud.com", "hits": 74,
                      "confirmed_at": "2026-09-12T00:00:00Z", "last_verified_at": "2026-09-12T00:00:00Z", "passes": 0}],
            "pending": [], "candidates": [], "notes": ["软封锁（上游返回 200 拦截页）无法自动识别，请手动钉住（pins）"]
        });
        let out = format_blacklist(&b);
        assert!(out.contains("pay.google.com"));
        assert!(out.contains("gateway.icloud.com"));
        assert!(out.contains("url-1"));
        assert!(out.contains("软封锁"), "局限说明要出现在 CLI 输出里（spec §5.4）");
    }

    #[test]
    fn the_menu_lists_eight_items_and_the_p1_menu_gains_one() {
        let items = menu_items();
        assert_eq!(items.len(), 8);
        assert_eq!(items.last().unwrap().action, MenuAction::Quit);
        assert!(items.iter().any(|i| i.title.contains("黑名单")));
        // P1 的主菜单必须有住宅入口
        let p1 = crate::commands::menu::items();
        assert!(
            p1.iter().any(|i| i.action == MenuAction::Residential),
            "P1 Task 17 的 items() 里要有住宅项：{p1:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_daemon_gives_an_actionable_error_instead_of_touching_state() {
        let d = tempfile::tempdir().unwrap();
        let e = run(ResidentialCmd::Status { json: false }, d.path().join("absent.sock"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("systemctl start b-ui"), "实际 {e}");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::residential::cli`
Expected: 编译失败，`cannot find type `ResidentialCmd``。

- [ ] **Step 3: 实现映射与格式化**

```rust
pub fn to_request(cmd: &ResidentialCmd) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)> {
    use ResidentialCmd as C;
    Ok(match cmd {
        // domains 只是 status 的一个投影（服务端不必为它单开端点）
        C::Status { .. } | C::Domains => ("GET", "/api/residential/status".into(), None),
        C::Health { .. } => ("GET", "/api/residential/health".into(), None),
        C::Add { url } => ("POST", "/api/residential/add".into(), Some(serde_json::json!({"url": url}))),
        C::Remove { target } => {
            // 先按 uuid 解，不是 uuid 就当 host:port（与面板两种定位方式一致）
            let body = match Uuid::parse_str(target) {
                Ok(id) => serde_json::json!({"id": id}),
                Err(_) => serde_json::json!({"host_port": target}),
            };
            ("POST", "/api/residential/remove".into(), Some(body))
        }
        C::Enable => ("POST", "/api/residential/enable".into(), Some(serde_json::json!({"enabled": true}))),
        C::Disable => ("POST", "/api/residential/enable".into(), Some(serde_json::json!({"enabled": false}))),
        C::Global { on_off } => (
            "POST",
            "/api/residential/global".into(),
            Some(serde_json::json!({"global": on_off == "on"})),
        ),
        C::RestoreDefault => ("POST", "/api/residential/restore-default".into(), None),
        C::Check { id } => (
            "POST",
            "/api/residential/check".into(),
            Some(match id {
                Some(id) => serde_json::json!({"id": id}),
                None => serde_json::json!({}),
            }),
        ),
        C::Select { id } => ("POST", "/api/residential/select".into(), Some(serde_json::json!({"id": id}))),
        C::Blacklist { cmd } => return blacklist_request(cmd),
    })
}

pub fn blacklist_request(cmd: &BlacklistCmd) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)> {
    let check_kind = |k: &str| -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(k, "domain_suffix" | "domain" | "port"),
            "--kind 只能是 domain_suffix / domain / port，实际 {k}"
        );
        Ok(())
    };
    Ok(match cmd {
        BlacklistCmd::List { .. } => ("GET", "/api/residential/blacklist".into(), None),
        BlacklistCmd::Apply => ("POST", "/api/residential/blacklist/apply".into(), None),
        BlacklistCmd::Pin { value, kind, note } => {
            check_kind(kind)?;
            (
                "POST",
                "/api/residential/blacklist/pins".into(),
                Some(serde_json::json!({"kind": kind, "value": value, "note": note})),
            )
        }
        BlacklistCmd::Unpin { value, kind } => {
            check_kind(kind)?;
            (
                "DELETE",
                "/api/residential/blacklist/pins".into(),
                Some(serde_json::json!({"kind": kind, "value": value})),
            )
        }
    })
}

pub fn resolve_url(raw: &str, stdin: &mut impl std::io::BufRead) -> anyhow::Result<String> {
    if raw != "-" {
        return Ok(raw.trim().to_string());
    }
    // 凭据经 stdin 传入，不进 argv（`ps` 看得到 argv）
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let line = line.trim().to_string();
    anyhow::ensure!(!line.is_empty(), "stdin 未读到代理 URL");
    Ok(line)
}
```
`format_status` / `format_health` / `format_blacklist` 都是「读 JSON → 拼多行中文文本」的纯函数：`format_status` 打印总开关、模式、关键字来源（跟随默认 / 自定义 + 条数）、当前落点与运行时选择（漂移时加一行「自动切换后的落点尚未持久化，将在每日 04:00 写回」）、上游表（`name / type / host:port / lastVerifiedIp / priority`）、黑名单计数、alerts；`format_health` 打印成员表（`tag / 上游 / 巡检状态 / 连续轮次 / 24h 成功率（百分比）/ 出口 IP / 类型 / 黑名单条数`）并高亮当前选中行；`format_blacklist` 分三段打印 pins / auto / pending+candidates，末尾附 `notes`。三者都**只读传入的 JSON 字段**，绝不打印 `password`（服务端也不回它）。

- [ ] **Step 4: 实现派发、菜单与 P1 四处挂接**

```rust
pub async fn run(cmd: ResidentialCmd, socket: PathBuf) -> anyhow::Result<()> {
    let client = crate::ipc::Client::new(&socket);
    anyhow::ensure!(
        client.available().await,
        "守护进程未运行（{}），请先 `systemctl start b-ui`",
        socket.display()
    );
    // add - 的 stdin 读取放在请求组装之前，凭据不进 argv
    let cmd = match cmd {
        ResidentialCmd::Add { url } => {
            let mut stdin = std::io::BufReader::new(std::io::stdin());
            ResidentialCmd::Add { url: resolve_url(&url, &mut stdin)? }
        }
        other => other,
    };
    let (method, path, body) = to_request(&cmd)?;
    let (status, v) = client.request(method, &path, body).await?;
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "{}",
            v.get("error").and_then(|e| e.as_str()).unwrap_or("请求失败").to_string()
        );
    }
    match &cmd {
        ResidentialCmd::Status { json } => print_or(*json, &v, format_status),
        ResidentialCmd::Health { json } => print_or(*json, &v, format_health),
        ResidentialCmd::Blacklist { cmd: BlacklistCmd::List { json } } => print_or(*json, &v, format_blacklist),
        // 供脚本消费：只打印生效关键字数组（等价于 v3 `residential-helper.sh domains`）
        ResidentialCmd::Domains => println!("{}", v.get("domains").cloned().unwrap_or(serde_json::json!([]))),
        _ => println!("{}", v),
    }
    Ok(())
}

fn print_or(json: bool, v: &serde_json::Value, f: impl Fn(&serde_json::Value) -> String) {
    if json {
        println!("{v}");
    } else {
        println!("{}", f(v));
    }
}

pub fn menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem { key: "1", title: "住宅池状态", action: MenuAction::Residential },
        MenuItem { key: "2", title: "添加上游（粘贴供应商那一行）", action: MenuAction::Residential },
        MenuItem { key: "3", title: "移除上游", action: MenuAction::Residential },
        MenuItem { key: "4", title: "体检当前上游", action: MenuAction::Residential },
        MenuItem { key: "5", title: "手动切换出口", action: MenuAction::Residential },
        MenuItem { key: "6", title: "分流模式（global / 关键字）", action: MenuAction::Residential },
        MenuItem { key: "7", title: "黑名单（查看 / 钉住 / 立即应用）", action: MenuAction::Residential },
        MenuItem { key: "0", title: "返回", action: MenuAction::Quit },
    ]
}
```
`menu` 用 `crate::commands::menu::render_with(&menu_items(), true)` 渲染，读一行 → 按 `key` 派发到对应 `ResidentialCmd`（2/3/5 再交互读一行输入：上游那一行**不回显**提示「粘贴后回车」并直接当 `Add{url}` 的值，不经 argv；`0` 返回）。

P1 四处挂接的逐字补丁：
```rust
// crates/bui/src/cli.rs：Command 追加一个变体
    /// 住宅出口（上游池 / 体检 / 切换 / 黑名单）
    Residential {
        #[command(subcommand)]
        cmd: crate::modules::residential::cli::ResidentialCmd,
    },

// crates/bui/src/main.rs：dispatch 追加一个 arm。
// 注意：已合并的 P1 代码里 `dispatch` 的签名是 `async fn dispatch(command: Command) -> Result<()>`，
// 函数体内**没有** `runtime` 变量（tokio runtime 在调用方建），所以这里直接 `.await`。
// `use std::path::PathBuf;` 已在 main.rs 第 26 行，无需新增 use。
        Command::Residential { cmd } => {
            crate::modules::residential::cli::run(cmd, PathBuf::from(crate::paths::SOCKET_PATH)).await
        }

// crates/bui/src/commands/menu.rs：MenuAction 追加一个变体
pub enum MenuAction { Status, Reconcile, ReconcileForce, Service(&'static str), Logs, Upgrade, HardenSsh, Residential, Quit }
// items() 在 "7" 之后插一项（"0" 退出保持在最后）
        MenuItem { key: "8", title: "住宅出口（池 / 体检 / 黑名单）", action: MenuAction::Residential },
// run() 的派发追加一支（socket 不可用时与 4/5 同处理：标注「(需守护进程)」并拒绝进入）
            MenuAction::Residential => {
                crate::modules::residential::cli::menu(PathBuf::from(crate::paths::SOCKET_PATH)).await?
            }

// crates/bui/src/api/health.rs：把 P1 留的 `residential: None` 换成真值。
// `get()` 里已有 `let state = app.store.read().await;` 与 `let rt = app.runtime.read().await;`
// 两个局部变量，且 `rt` 在这一行之后仍被使用，所以直接用它们，不再重复 read。
        residential: Some(crate::modules::residential::state::health_summary(
            &crate::modules::residential::state::group_of(&state),
            &crate::modules::residential::state::from_runtime(&rt),
        )),

// crates/bui/src/api/mod.rs：上一行一改，P1 已合并的这条断言必红，按下面改
// （`health_reports_services_drift_watchdog_and_reconcile` 里那一行）
        let resi = h.residential.clone().expect("P3 起 /api/health 一定带住宅摘要");
        assert_eq!(resi["enabled"], false, "夹具是空池");
        assert!(resi.get("alerts").is_some());
        assert!(resi.get("blacklist").is_some());

// crates/bui/src/modules/residential/mod.rs：删掉 T1 加的那两行注释 + 一行属性
// （到这一步 cli.rs / menu.rs / api/health.rs 都接上了，模块内已无死代码）
-// bin crate 里 `pub` 不消除 dead_code，而 T2–T9 的 pub fn 要到 T11/T12 才接上调用方；
-// 这一行在 Task 12 收口（CLI + 菜单 + /api/health 都接上）时删除。
-#![allow(dead_code)]
```

- [ ] **Step 5: 运行全量测试**

Run: `cargo test -p bui && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: 全绿。本任务要改 **P1 已合并/已计划的两处测试断言**（第三处是 T1 改 `serve.rs` 的模块名清单；全计划共三处，见契约决策 §H）：
1. `crates/bui/src/commands/menu.rs` 的 `menu_keys_are_stable_and_unique`：`keys` 从
   `vec!["1","2","3","4","5","6","7","0"]` 改成 `vec!["1","2","3","4","5","6","7","8","0"]`
   （`render_is_two_columns_and_mentions_every_item` 用 `items.len().div_ceil(2)`，9 项自动成立，不用改）；
2. `crates/bui/src/api/mod.rs` 的 `health_reports_services_drift_watchdog_and_reconcile`：
   `assert_eq!(h.residential, None, "P3 才填")` 换成 Step 4 里那四行断言。
另外确认删掉模块级 `#![allow(dead_code)]` 之后 `cargo clippy --workspace --all-targets -- -D warnings` 仍然零告警；若报某个符号 dead，说明它确实没有调用方，按「删掉它或补上调用方」处理，**不要**把 allow 加回来。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/residential/cli.rs crates/bui/src/modules/residential/mod.rs \
        crates/bui/src/cli.rs crates/bui/src/main.rs crates/bui/src/commands/menu.rs \
        crates/bui/src/api/health.rs crates/bui/src/api/mod.rs
git commit -m "feat(resi): bui residential 子命令、住宅数字菜单与 /api/health 的住宅摘要"
```

---

## 自查

### 1. spec 覆盖

| spec §5 条目 | 落在哪 |
|---|---|
| §5.1 `groups.default` 的 `enabled` / `mode` | T7 `set_enabled` / `set_mode`；T10 `/enable`、`/global` |
| §5.1 `keywords`（`null` = 跟随默认） | T7 `set_keywords`；T10 `/domains`、`/restore-default`（`domainsFollowDefault`） |
| §5.1 `upstreams` 增删改 | T7 `add` / `remove` / `set_priority` / `renumber` |
| §5.1 `selected_upstream_id` | T8（runtime 侧）+ T9 每日窗口写回（契约决策 §C） |
| §5.1 `blacklist.pins` / `.auto` | T9 `add_pin` / `remove_pin` / `flush_pending` / `remove_auto` |
| §5.1 `Upstream.provider` | **本计划从不写入**（T7 `add` 只从 `exit.country` 填 `region`）。三源画像给的是 ASN / org / country，推不出「供应商」（同一家住宅代理商会跨几十个 ASN），瞎猜出来的值会进面板、进 v3 兼容字段、还会被运维当真。v4 的口径：**该字段由管理员手填或暂空**，`status` / `health` 只原样回显（T10 `UpstreamRow.provider`），P3 不写第二个推断规则。夹具 `sample_group()` 里给的 `Some("decodo")` 只是「手填过」的样例值 |
| §5.2 四种格式 | T7 经 `bui_schema::parse::upstream_url`（P0 已实现四种，T7 只消费） |
| §5.2 类型自动探测 SOCKS5→HTTP、整轮重试一次 | T7 `detect_kind`（测试断言 4 次 GET） |
| §5.2 记 `verified` | T7 调 `check::to_verified`（实现与测试都在 T6 Step 3/1） |
| §5.2 三方交叉 + Cloudflare 挑战页判「未知」 | T6 `fold_sources`（唯一一份交叉逻辑，`probe_exit` 与 `run` 共用）/ `cross_class` / `is_cloudflare_challenge` |
| §5.2 Google `/sorry/` | T6 `sorry_of`（唯一一份判定；同步包装 `google_sorry` 无调用点，终审已删） |
| §5.2 gemini / api.openai.com / api.anthropic.com | T6 `AI_HOSTS` |
| §5.2 checkout.stripe.com / pay.google.com / www.paypal.com | T6 `PAY_HOSTS` |
| §5.2 固定端口集 5228/5223/993/22/8080/853 的 CONNECT 状态 | T6 `PROBE_PORTS` + T3 `parse_connect_status`；目标主机由 T1 `port_probe_host` 给（基准 80/443 打中性主机 `www.gstatic.com`，固定端口集打各自真在该端口监听的主机） |
| §5.2 SOCKS5 UDP ASSOCIATE | T3 `udp_associate`（RFC1928 CMD=03）+ T6 |
| §5.2 写 `verified` 与 `ports_allowed` | T6 `run_and_store` |
| §5.2 reqwest 走代理、两种上游都带鉴权 | T3（`Proxy::basic_auth` / CONNECT Basic / RFC1929） |
| §5.2 超时 10s、并发 4、`spawn_blocking`、凭据不进 argv/日志 | T1 `PROBE_TIMEOUT_SECS` / `PROBE_CONCURRENCY` / `fanout`；T3 `redact` |
| §5.3 每 2 分钟、gstatic 204 ×2 | T1 常量 + T8 `probe_member` / `health_loop` |
| §5.3 2 轮迟滞 | T2 `apply_hysteresis` |
| §5.3 粘住 | T8 规则 6 |
| §5.3 切换目标 priority → 24h 成功率 | T8 `pick_target`；T2 `success_rate_24h` |
| §5.3 ≥ 60s 间隔 | T8 规则 9 |
| §5.3 全不健康保持并告警 | T8 规则 7 |
| §5.3 成员集变化本轮不切 | T8 规则 4 |
| §5.3 Clash API `PUT /proxies/resi-pool` | T4 `HttpClash::select` |
| §5.3 relay **任何**重启后重放 `selected_upstream_id` | 两条路都覆盖：① 对账重启（发 `Event::RelayRestarted`）→ T8 `replay_loop`（读 `runtime.selected_upstream_id` 这个 uuid，调 Clash API 前用 `clash::tag_of` 现算 tag；`rx` 由 T11 的 `spawn` 先 `subscribe()` 再传入）；② 看门狗重启与 `POST /api/services/b-ui-relay/restart`（都**不发**事件）→ T8 规则 6a 每轮 ≤ 120 秒重放（测试 `a_relay_restart_outside_reconcile_is_healed_by_replaying_the_runtime_choice`）。裁决 D11 记录了「让 P1 给那两条来路也发事件」的可选改法 |
| §5.3 健康 streak 存 `runtime.json` | T2 `ResiRuntime.health` |
| §5.4 候选（journald 解析 403 与 SOCKS 拒绝、按 (upstream, host, port) 计数） | T5 `parse_line`；T9 `learn_from_journal`（只计 `port_allowed` 为真的拒绝，裁决「端口类拒绝不进域名黑名单」） |
| §5.4 候选（每日固定探针集） | T9 `PROBE_SET` + `daily_round` ②（443 不在该上游白名单里的上游整条跳过） |
| §5.4 (b) 每日**端口集**主动探测（5228/5223/993/22/8080/853 + 80/443） | T9 `probe_ports_daily` / `port_learn` + `daily_round` ②b（学到的 `ports_allowed` 在同一次 04:00 写盘里落地，不额外重启 relay） |
| §5.4 确认（CONNECT 4xx/5xx、SOCKS REP≠0、直连可达、≥10 分钟连续 2 次） | T3 判定 + T9 `confirm_once` / `confirm_round`，由 `journal_loop` 每 `JOURNAL_POLL_SECS` 驱动（裁决「黑名单确认节奏」：learn → confirm，≥10 分钟即进 `pending`；04:00 不做确认）。测试 `the_journal_loop_learns_then_confirms_so_ten_minutes_are_enough`（`start_paused`）锁住这条节奏 |
| §5.4 规则 `domain_suffix` = 完整主机名 | T9 `flush_pending`（测试断言不泛化） |
| §5.4 端口类由 `ports_allowed` 表达 | T9 `is_bare_ip` 丢弃裸 IP、`port_allowed` 滤掉白名单外端口的拒绝（`None` ⇒ `BASE_PORTS`；测试 `a_refused_non_whitelisted_port_never_becomes_a_domain_rule`）；T6 `derive_ports_allowed` |
| §5.4 pins 与手动应用立即生效、auto 每日 04:00 批量生效 | T9 `add_pin` / `apply_now` / `daily_loop`（04:00 只做批量生效 + 每日探针/端口集 + 复核） |
| §5.4 每日复核连续 3 次不再被拒移除 | T9 `daily_round` ④ |
| §5.4 软拦截识别不了 → 面板说明 | T10 `notes()`；T12 `format_blacklist` |
| §5.4 渲染由 `bui_schema::render::relay` 完成，P3 不重复渲染 | 契约决策 §B；T1 的 `render_is_empty_because_core_files_owns_the_relay_config`；T11 的 `a_state_change_reaches_the_relay_render_through_p1` |
| §5.5 多组预留只体现在数据结构 | 全程只读写 `groups[GROUP_DEFAULT]`（T2 `group_of` / `update_group` 是唯一入口），不新增任何多组逻辑 |
| §4.3 面板端点（spec 列的 12 条 + 3 条 spec 未列：`priority`（D7）/ `DELETE blacklist`（D8）/ `health/check`（D10）；`enable` 与另外 3 条别名属 v3 兼容层，见 §A） | T10 路由表 |
| §4.3 第 ③ 条 `/api/health` 加上游体检结果 | T12 的 `api/health.rs` 一行 |
| §2.4 CLI 与数字菜单 | T12 |

**spec 之外、但计划里做了的两件事**，都在契约决策里给了理由与 Fable 的否决路径：v3 路径别名（§A）、state 落点与 runtime「当前生效」的分工（§C）。**spec 里有、但本计划有意不做的**：无。

### 2. 占位符扫描

无 “TBD / TODO / 稍后实现 / 加上适当的错误处理 / 为以上写测试 / 同 Task N”。每个 Step 3/4/5 都带可编译的真实代码；`mod.rs` 的常量段与三个 `format_*` 的正文用「逐条照写 Interfaces / 只读这些字段」描述而非重复粘贴，但两处都给了完整字段清单与约束（不打印 password、中文标签不变），不是「细节待补」。

### 3. 类型一致性（跨任务逐个核对）

- `ConnectVerdict` 四变体：T3 定义 → T6 `port_matrix` / T9 `confirm_once` 消费，「硬拒」只有 `Refused` 一种（谓词 `is_hard_reject()` 无调用点，终审已删，判定就地 `match`），三处一致。
- `ProbeError::AuthFailed`：T3 定义 → T6（`auth_failed` 标记）、T7（`UpstreamError::AuthFailed`）、T8（判不健康）三处都按「整条上游不可用」处理，无第二种解读。
- `HealthState` / `Candidate` / `PendingEntry` / `Checking`：T2 定义，T8/T9/T10 只按字段名读写，无重复定义。`Candidate` 带 `confirms` / `last_confirm_at`（确认进度，由 T9 `journal_loop` 每 `JOURNAL_POLL_SECS` 一轮推进），`PendingEntry` 里的同名字段只是「已确认完毕」的留档，`flush_pending` / `apply_now` 不再判它。
- **端口白名单判定只有一处**：T9 `port_allowed(up, port)`（`ports_allowed` 为 `None` 时用 T1 的 `BASE_PORTS`），`learn_from_journal` 与 `daily_round` ② 都调它；T6 只负责**学**出 `ports_allowed`（`derive_ports_allowed`），不做这个判定。
- `RoundOutcome.switched_to` 与 `RoundOutcome.replayed_to` 语义互斥：前者是「健康决策切走」（写 `last_switch_at`、吃 60s 限速），后者是「relay 重启后把 runtime 的选择放回去」（不写、不吃）。T8 规则 6a / 10 各只写一个，T10 的 `POST /health/check` 两个都回给面板。
- **运行时主键一律 uuid**：`ResiRuntime.selected_upstream_id: Option<Uuid>`、`ResiRuntime.health` 的 key = `Uuid::to_string()`；T7 的 `remove` 清这两处，T8 的巡检/重放/手动切换与 T9 的 04:00 写回都只读写 uuid，T10 的 `active_tag` 是现算出来给面板看的。
- tag 换算只有一处：T4 `tag_of` / `id_of_tag` / `tags`，与 `bui-schema` 的 `format!("resi-{}", i+1)` 对齐；只有「调 Clash API」与「解析 journald 日志行里的 `outbound/http[resi-1]`」两类场景经它，绝不做主键。
- `FakeProber` 的错误哨兵（`Err("__auth_failed__")` → `ProbeError::AuthFailed`）在 T3 的 Interfaces、Step 1 断言与 Step 4 实现里三处写死，T6/T7/T8 的测试依赖它。
- 三源交叉逻辑只有一份：T6 `fold_sources`，`probe_exit`（顺序）与 `run`（`fanout` 并发）都调它。
- `PortLearn` / `port_learn` 在 T9 定义并消费 T6 的 `derive_ports_allowed`；`Learned(None)` = 不限、`Keep` = 不改，两者在 `daily_round` 与测试里语义一致。
- `state::update_group` 是唯一写 state 的函数（T2），T6/T7/T9 都经它；T8/T10 的切换路径**不**写 state（契约决策 §C）。
- `check::CheckReport` 在 `runtime.checks` 里存成 `serde_json::Value`（T2 的字段类型），T6 写、T10 `egress_of` 用 `pointer("/exit/ip")` 读，两端一致，也避免 `state.rs` 反向依赖 `check.rs`。
- `blacklist_count` **只有一份算法**：T10 的 `auto_count(g, id)`（只数 `auto[].upstream_id == id`，**不含 `pins`**），`UpstreamRow`（status）与 `MemberRow`（health）都调它。`pins` 的全局条数只出现在 `BlacklistCounts.pins` 与 `BlacklistResponse.pins`。夹具（1 条 auto + 1 条 pin）下两处都断言 `1`，且互相断言相等。
- `ExitClass::label()` 的中文串在 T6 定义，T10 的 `egress.type` / `egress_ip_type` 与 T12 的 `format_health` 都只转发它，没有第二份字符串表。
- 消费 P1 的签名（`Store::update` / `Runtime::update` / `Host::run` / `Module::{name,render,routes,spawn}` / `DaemonCtx` / `AppState` / `Event` / `EventBus` / `ipc::Client` / `MenuItem`）逐条照 P1 计划的 Interfaces 段，未改写任何一个。
- 消费 P0 的签名（`upstream_url` / `UpstreamInput` / `ParseError` / `ResidentialGroup` / `Upstream` / `Rule` / `Pin` / `AutoEntry` / `Verified` / `SplitRules::from_group` / `DEFAULT_KEYWORDS` / `DEFAULT_GROUP`）逐条照 `crates/bui-schema/src` 的真实代码。

### 4. 并行组的文件不重叠

T2/T3/T4/T5 各写一个文件；T7/T8/T9 各写一个文件；T1 一次建齐桩与 `pub mod` 行，后续任务不互相追加声明。唯一的多任务文件是 `mod.rs`（T1 建、T11 收口、T12 删掉那行 `#![allow(dead_code)]`，三者都在串行段）与 P1 的六处追加（T1 两处 + Cargo.toml、T12 五处），以及 T12 要改的两条 P1 测试断言（`commands/menu.rs` 的菜单项数、`api/mod.rs` 的 `residential == None`），也都在串行段。

### 5. 单元测试覆盖不到、必须留给 M2 真机（bwg-rick）的部分

「不碰真实系统、不联网」是 Global Constraints 的铁律，所以下面这些只有真机能验，逐条给出**真机验收动作**与**万一它们不成立会坏掉什么**。M2 验收清单直接照这张表跑。

| # | 单测覆盖不到的部分 | 真机验收动作（bwg-rick） | 不成立的后果 |
|---|---|---|---|
| M2-1 | spec §5.2 的**真实网络路径**：经真实供应商的 SOCKS5 / HTTP 上游做 TLS GET、SOCKS5 UDP ASSOCIATE。单测里只有 T3 Step 5 那段进程内假代理的字节序列 | `bui residential add -`（凭据走 stdin）加一条真实上游 → `bui residential check --id <id>` → 看 `runtime.checks[id]` 里 `exit.ip` / `udp_associate` / `ports_allowed` 是否都有值 | 握手字节序列写错 ⇒ 全部体检与黑名单判定的输入都错（这也是 T3 Step 5 那段假代理测试存在的唯一理由） |
| M2-2 | 调研 §D 的形态复现：CONNECT 403 = 硬拒、只放行 80/443、`PROBE_PORTS` 六个端口全 403 | 同一次 `check` 的报告里核对 `ports.{5228,5223,993,22,8080,853}` 是否都是 `refused:403`、`ports_allowed == [80,443]` | `derive_ports_allowed` 学出的白名单不对 ⇒ relay 的 `port_range` 取反规则把该走代理的流量放直连（或反之） |
| M2-3 | **407 的真机链路**：`get` 在 https 目标上判不出 407（T3 `confirm_auth_failure` 的注释），生产上只有 `connect()` 能识别 | 故意把一条上游的密码改错 → `bui residential health` 应出现「凭据失效」告警而不是只有「上游挂了」；`bui residential add` 同一条错凭据应回 `AuthFailed` 而不是 `Unverifiable` | 补判失效 ⇒ 凭据失效永远只报「不可达」，运维去查端口而不是换凭据（这正是 T3/T7/T8 加 `confirm_auth_failure` 的原因） |
| M2-4 | 决策 D2 的**延迟**：journald 改成每 `JOURNAL_POLL_SECS = 300` 秒 `--after-cursor` 增量读、`-o cat` 而不是 spec §5.4 (a) 写的 `-f -o json`；同一轮里紧接着跑确认（裁决「黑名单确认节奏」） | 制造一次真实硬拒（对 `pay.google.com` 发一次请求）→ 最多 5 分钟后 `bui residential blacklist list` 的 `candidates` 里出现该条；再等 ≤ 15 分钟该条进 `pending`（`bui residential blacklist list` 的待生效段）；`bui residential blacklist apply` 或次日 04:00 后进 `auto` | 候选入账最晚晚 5 分钟，进 `pending` 最快 ~10 分钟、最晚 ~15 分钟（确认要「间隔 ≥ 10 分钟连续 2 次」），写进 `state` 仍等 04:00 或面板「立即应用」——不影响正确性，只影响时延。Fable 否决 D2 则要给 `Host` 加流式 trait |
| M2-5 | 决策 D5 的**时区**：`DAILY_HOUR = 4` 判据是 `host.now().hour()`（UTC 机器 ⇒ 04:00 UTC），与 R13 §7 的北京时间 04:00 差 8 小时 | `timedatectl` 确认机器仍是 UTC；观察 `runtime.last_daily_at` 落在 04:0x UTC | 若某台机器改成了 `Asia/Shanghai`，每日窗口会跑在北京 12:00（白天重启 relay 掐连接）。裁决 D5 若选北京时间，改 `DAILY_HOUR = 20` 一个常量 |
| M2-6 | relay 的 ②③ 两条**重启来路**（看门狗 / `POST /api/services/b-ui-relay/restart`，都不发 `RelayRestarted`，§C 末段） | `bui residential select --id <B>` 切到 B → `systemctl restart b-ui-relay` → 等 ≤ 120 秒 → `bui residential health` 的 `selected` 应回到 B（而不是停在池首 A） | 规则 6a 写反（采纳 Clash 的 `now`）⇒ 一次看门狗重启静默撤销手动/自动切换。裁决 D11 记录了让 P1 给这两条来路补发事件的可选改法 |
| M2-7 | `Upstream.provider` 无写入路径（自查 §1 已注明「管理员手填 / 暂空」） | 无需动作；确认面板对 `provider: null` 显示为空而不是 `undefined` | 无（有意不做推断） |

### 6. 需要 Fable 裁决的决策清单

| # | 议题 | 本计划的选择 | 否决时的改法 |
|---|---|---|---|
| D0 | 改 P1 文件超出「两处追加行」的授权（Cargo.toml、cli.rs、main.rs、menu.rs、api/health.rs 共 5 处追加式小改，契约决策 §H） | 按 §H 的逐字补丁执行，批准时把 `bui residential …` 回写总纲 C5 | 砍掉 CLI 与菜单（T12 只剩 health.rs 一行），住宅只能从面板操作 |
| D1 | v3 路径别名（契约决策 §A） | 规范路径 + 4 条 v3 别名路由，前端零改动、P3 不碰 `web/` | 只留规范路径，并在 P2 的前端任务里追加 11 处 `fetch` 路径改名 |
| D2 | journald 用游标增量轮询而非 `-f`（契约决策 §E） | 每 300 秒 `--after-cursor`，全部经 `Host::run`；同一轮 learn → confirm（裁决「黑名单确认节奏」已批准） | 给 `Host` 加流式 trait 或另起 `tokio::process`，测试需要新的 fake 层 |
| D3 | 自动/手动切换只写 `runtime.selected_upstream_id`（uuid），不写 state（契约决策 §C） | 切换零重启，符合 `interrupt_exist_connections: false` 的用意；运行时主键用 uuid 而非位置键 `resi-N`，增删上游后不会指错人 | 每次切换写 state → 每次健康切换都重启 relay、掐断全部住宅连接 |
| D4 | 落点漂移在每日 04:00 窗口写回 state | `selected_pending_persist` 最坏 24h 追上 | 永不自动写回，面板说明写清「自动切换后端口白名单与黑名单仍按上一个上游」 |
| D5 | 每日窗口用**服务器本地时间** 04:00（`DAILY_HOUR = 4`），与 R13 的北京时间差 8 小时 | 照 v4 spec §5.4，不引 tz 数据库 | 改 `DAILY_HOUR = 20`（UTC 机器上等于北京 04:00），Task 9 两条测试跟着改 |
| D6 | `GET /api/residential/health` **全部只读** runtime/state（Task 10 Step 4） | 面板响应 2 秒内；「刷新面板」不再是有副作用的动作（否则连刷两次就能把成员判死并切走，把 spec §5.3 的「2 分钟 × 2 轮」压成两次点击，还与 `health_loop` 并发写 runtime） | `egress_of` 换成现拨 `check::probe_exit`（最多 8×3 次 HTTPS），并在面板说明写响应时长 |
| D10 | 新增 `POST /api/residential/health/check`（spec §4.3 未列）：面板「立即巡检一轮」，`MANUAL_ROUND_MIN_GAP_SECS = 60` 限速、超频 429 | D6 之后需要一个显式的「现在就探」入口，且它有副作用（推进迟滞、可能切换），必须限速 | 删该路由与它的测试，成员状态只由后台每 2 分钟的巡检刷新 |
| D7 | `POST /api/residential/priority`（spec §4.3 未列） | 给端点，否则 `priority` 只能手改 `state.json` | 删该路由与测试，优先级由 `bui residential` 之外的手段维护 |
| D8 | `DELETE /api/residential/blacklist`（删一条 `auto`，spec §4.3 只列了 GET） | 给端点，落实 spec §5.4「面板可删条目」 | 删该方法与测试，误伤只能等每日复核自动移除（最快 3 天） |
| D9 | 池上限 `MAX_UPSTREAMS = 8`（spec 未规定，取 v3 面板 `slice(0,8)`） | 超过回 400 | 调大常量；面板成员表与 Clash API 的可用性需重新评估 |
| D11 | spec §5.3「relay **任何**重启后立即重放」：relay 有三条重启来路，只有对账那条发 `Event::RelayRestarted`（看门狗 `watchdog.rs:141` 与 `POST /api/services/b-ui-relay/restart` 都不发，§C 末段） | **P3 自己兜住**：T8 巡检规则 6a 每轮（≤ 120 秒）把 `runtime.selected_upstream_id` 重放回 Clash，绝不采纳 Clash 的 `now`。不改 P1 的第八处文件；代价是那两条来路最坏 120 秒漂移（不断网，只是出口可能回到池首） | 向 P1 提一条追加改动：`watchdog.rs` 的重启分支与 `system::service_action` 的 `unit == "b-ui-relay"` 分支各加一行 `bus.send(Event::RelayRestarted)`，漂移窗口降到事件延迟。规则 6a 照旧保留（重放幂等，两者不冲突） |
| D12 | `GET /api/residential/status` 的 `urls[].username` 回**明文**完整用户名（v3 `web/app.js:665-699 renderResidentialUrls` 逐字段读） | **保留 v3 兼容的完整用户名**：整条管理员域在 JWT 之后，面板本来就是唯一读者；`password` 永不出现在任何响应里（`UpstreamRow` 与 `display_url` 都只给 `mask()` 过的用户名）。新前端用 `upstreams[].username_masked`，不要再读 `urls[].username` | 两处都只回打码值，并同批改 `web/app.js` 的 `renderResidentialUrls`（v3 面板会显示成 `u***`，运维核对凭据要改去看 `state.json`） |
