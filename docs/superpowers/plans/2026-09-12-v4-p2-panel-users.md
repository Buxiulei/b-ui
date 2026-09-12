# v4 P2 面板与用户实施计划（`crates/bui` 的 panel 模块）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 v4 的面板与用户域：`bui auth-hook`（Hysteria2 `auth.type: command` 钩子，极简路径 + ≤2 秒硬超时 + fail-closed + 自写日志）与 `auth-snapshot.json` 的原子重写；Xray gRPC（tonic + vendored 10 个 proto）的 AddUser / RemoveUser / QueryStats；用户 CRUD（不重启内核）；10 秒流量采样、内存累加、≤30 秒落盘、在线并集、月度重置、到期/超限/禁用的执行与恢复；管理员 API（契约沿用 v3，四处改动）；三种订阅 + 新增 `/api/nodes/<user>`；用户域 `/api/me/*` 的 501 桩；`rust-embed` 嵌入的旧前端（`app.js` 只改两处：`login()` 删 localStorage `ap`、`addUser()` 改 POST）；客户端包缓存与 `/packages/*`；用户与流量摘要经 `GET /api/stats`（v3 既有契约）与新增的 `GET /api/users/health` 透出（**不改** `/api/health`，裁决 D2）。

**Architecture:** 全部代码落在 `crates/bui/src/modules/panel/` 一棵子树里，对外只是 P1 `reconcile::Module` 的**一个**实现 `PanelModule`：`render()` 返回空（xray 的 `clients` 由 P1 Task 10 的 `core_files` 从 `state.users` 渲染，`auth-snapshot.json` 不是 artifact，见「渲染边界」一节）、`routes()` 给出管理员端点、`public_routes()` 给出无鉴权端点（订阅 / 节点 / 前端 / `/packages` / `/api/me`）、`spawn()` 起两个后台任务（10 秒采样 + 用户同步反应器）。写机器的动作一律经 P1 的 `Host` / `crate::state::store::write_atomic`，网络一律经注入的 `XrayApi` / `Hy2Api` trait，于是全部逻辑都能在单元测试里用 `FakeHost` + 内存 fake 验证，不碰真实系统、不联网。用户变更走「改 state → 发 `Event::StateChanged("users")` → ①P1 的对账把新 `clients` 写进 `xray-config.json`（结构哈希不变 ⇒ 不重启 xray）②P2 的同步反应器重写快照并对两个 inbound 做 gRPC 差分」，因此加删用户既不重启内核也不掉线。

**Tech Stack:** 继续 P1 的 Rust 1.93 / tokio 1 / axum 0.8 / reqwest 0.12（本计划用它的**异步**客户端打两个 hysteria 的 trafficStats）/ serde / tracing / time 0.3 / argon2 0.5 / jsonwebtoken 9 / uuid 1 / sha2；新增 tonic 0.14 + tonic-prost 0.14 + prost 0.14（gRPC，build 期 `tonic-prost-build` 0.14 + `protoc-bin-vendored` 3）、`async-trait` 0.1（可注入的 gRPC / HTTP trait）、`rust-embed` 8（嵌入 `web/` 与 `scripts/bui-c-install.sh`）、`subtle` 2（常量时间比对）。消费 `bui-schema`（P0 已交付）与 `bui` 的 P1 骨架。

**Spec:** `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`（§3.2、§3.3、§4.1、§4.2、§4.3、§4.4、附录 A）；总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1 消费契约、C2 模块边界、C3 路径、C4 manifest、C5 CLI 与快照、裁决记录）；P1 计划 `2026-09-11-v4-p1-daemon-core.md`（Task 1/2/3/4/5/9/10/13/15 的签名即契约）；P4 计划 `2026-09-11-v4-p4-client.md`（决策 6 的 `/api/nodes` 载荷、Task 13 的引导脚本）；审计 `docs/superpowers/audits/2026-09-11-architecture-audit.md` §3.3 与 §6（每处移植的处置依据）；调研 `docs/superpowers/research/2026-09-11-v4-unknowns.md` §1（Hysteria2 API 语义 H1–H14）与 §3（Xray gRPC X1–X11）。

## Global Constraints

- Rust stable ≥ 1.85（本机 1.93），edition 2021，`cargo clippy --workspace --all-targets -- -D warnings` 零告警，`cargo fmt --check` 通过。
- 发布目标 `x86_64-unknown-linux-musl`（本机已装）与 `aarch64-unknown-linux-musl`（CI）；依赖优先纯 Rust；TLS 用 `rustls` + `ring`，禁止 openssl。
- 协议集、端口、标签、UUID、密码与 v3 一致；三种订阅的字节由 `bui-schema` 渲染，P2 **不得**自己拼节点、URI、sing-box JSON 或 Clash YAML（总纲 C1；审计 web-C15）。
- sing-box 配置兼容 1.12–1.14 的责任在 P0；P2 只负责把 `bui_schema::render::subscription::singbox` 的产出原样发出去。
- 凭据、密钥、密码不进 argv、不进日志（`tracing` 字段一律经 `crate::redact::` 脱敏）；`auth-snapshot.json` 与 `state.json` 一律 0600，`auth-hook.log` 0600。
- 生产主机只用别名 bwg-rick / bwg-tizi / baiyi；公开仓库里不出现真实 IP、域名。测试与示例只用合成值：域名 `example.com`、IP `203.0.113.10`、伪装域 `www.bing.com`、用户 `alice` / `bob`。
- 分支 `v4`，任务分支 `v4-p2-t<N>`，每任务一个 commit，格式 `feat(scope): …` / `test(scope): …` / `fix(scope): …`，尾部附 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`；不 `git commit -a`；不裸 `git stash`；不推送。
- 每个任务先写失败测试再实现。
- **测试位置约定**：`bui` 是 bin-only crate，P2 的全部测试都是 crate 内单元测试（`#[cfg(test)] mod tests`），共享的测试支架放 `modules/panel/testsupport.rs`（`#[cfg(test)]`）。不建 `crates/bui/tests/` 目录。
- **不碰真实系统、不出网的铁律**：单元测试只允许接触 `tempfile` 给的临时目录；`systemctl` / `sysctl` 走 `FakeHost`；gRPC 走 `FakeXray`；hysteria 的 HTTP API 走 `FakeHy2`；HTTP 端点优先用 `tower::ServiceExt::oneshot` 直接打 Router。口径是「**不出网、只允许进程内回环**」，不是「一律不许监听端口」——Task 5 第 1 步的假 hysteria 是进程内 `127.0.0.1:0`（内核分配随机端口）的 `tokio::net::TcpListener`，Task 4 与 Task 5 各有一条连 `127.0.0.1:1`（必然连不上）的报错/超时测试，Task 1 的 `the_clients_report_errors_instead_of_pretending_and_only_dial_a_dead_port` 同样只拨 `127.0.0.1:1`，这几处合法，**不得以本约束为由删掉**。反过来也是铁律：**任何测试都不许拨 `127.0.0.1:10085` / `:9999` / `:9998`**——`QueryStats(reset=true)` 会把线上 Xray 计数器清零，`/traffic?clear=1` 会把 hysteria 的流量账取走。真实内核联调留给 M1 / M3 里程碑验收（bwg-rick）。
- **总纲 C2 的裁决口径只许改 P1 两处**：`serve.rs` 的 `modules()` 追加一行（**T13** 做，与三个方法的接线同一个 commit；同时把那条六模块断言改成子集断言，见 D14）、`modules/mod.rs` 追加一行 `pub mod panel;`（T1）。总纲「裁决记录」（2026-09-12）另外批准了两处：`reconcile/mod.rs` 的 `Module` trait 追加带默认实现的 `public_routes()` 与 `api/mod.rs` 的 `router()` 在 `require_admin` 外合并它（**裁决 D1**，裁决原文「这是 P2 唯一可改的 P1 文件对，改动限于这两处」，由**独立的 Task 0** 落地）、`main.rs` 的 `Command::AuthHook` 那一支（D3，P1 Task 1 已在代码注释里预授权）。`api/health.rs` **一个字都不改**（裁决 D2 不批准）：用户与流量摘要走 `GET /api/stats`（v3 既有契约）与新增的 `GET /api/users/health`（T8）。详见「P1 边界」一节。
- v3 的 `server/`、`web/server.js`、`b-ui-client.sh` 在 P2 期间**只读**（移植参照），不修改、不删除（P5 最后一个任务才删）。唯一例外是 `web/app.js` 的两处（`login()` 删 localStorage `ap`、`addUser()` 改 POST；spec §4.3 改动 1，Task 11）。
- 前端 `web/{index.html,style.css,logo.jpg,qrcode.min.js}` 一个字都不改：**因此 `/api/users`、`/api/config`、`/api/stats`、`/api/online`、`/api/hy2/watchdog/status` 必须按 v3 的字段名与形状回包**（见「v3 端点逐个契约」表）。

---

## 渲染边界（P1 Task 10 `core_files` 与 P2 的分工，必须先读）

| 产物 | 谁渲染 | 依据 | P2 的动作 |
|---|---|---|---|
| `<base>/xray-config.json` 的 `inbounds[].settings.clients` | **P1 Task 10 的 `CoreFilesModule`**，调 `bui_schema::render::xray::config(&state.node, &state.users, &paths)` | P1 Task 10 的产出表；`restart_key` 用 `structural_hash`（排除 `clients`），所以 `clients` 变化不重启 xray | **绝不重复渲染**。P2 只改 `state.users` 并发 `Event::StateChanged("users")`，由 P1 的去抖对账把新 `clients` 落盘（持久化用，重启后生效） |
| `<base>/auth-snapshot.json` | **P2**（`snapshot::write_if_changed`，tmp + 0600 + rename） | P1 文首契约段：「它**不是** `Artifact`（P1 不对账它的内容），所以必须在 Task 5 的 `BASE_WHITELIST` 里」；`bui install` 写初版（P1 Task 16），P2 接手重写 | 同步反应器在每次 `StateChanged` 与每 60 秒重写一次（内容不变则不写） |
| `<base>/config.yaml` / `config-residential.yaml` / `singbox-relay.json` / `Caddyfile` | P1 Task 10 | 同上 | 不碰。`/api/masquerade`、`/api/port-hopping` 只改 `state`，重渲染与重启映射交给对账 |
| `<base>/packages/*` | **P2**（Task 12 的包缓存任务） | spec §6「服务端内核缓存继续维护 sing-box 与 `bui-c` 的 Linux 二进制」；`packages/` 已在 P1 Task 5 的 `BASE_WHITELIST` 里，漂移扫描不递归进去 | 直接写盘（不是 artifact：内容由 manifest 决定、体积大、丢了重下即可） |

**结论：`PanelModule::render()` 返回空 `Vec`。** 这不是偷懒——把它写成「也渲染一份 xray-config.json」会与 `core_files` 产生两份同路径 artifact，P1 Task 4 的 `plan()` 会按后一份覆盖前一份、且两份的 `restart_key` 不同 ⇒ 每轮对账都判「要改」，M1 的「二次对账零变更」直接失效。Task 13 有一条断言测试锁住这个边界。

**已知缺口（D6，必须让 Fable 看见）**：`bui_schema::render::xray::config` 的文档注释写明「**调用方必须只传入当前有效（未到期、未超限）的用户**：本函数只看 `disabled` 与权益」，而 P1 Task 10 传的是 `&state.users` 全量。于是到期 / 超限用户的 REALITY 凭据一直留在 `xray-config.json` 里，**xray 每次重启都会把他们放回来**。P2 的短期处置（Task 6 的 `sync_users`）是两条一起上：

1. **无条件 RemoveUser**：每一轮同步都把「有 Reality 权益但当前该被拒（被封 / 到期 / 禁用）」的用户挨个发 RemoveUser，**不看本进程有没有 AddUser 过他**——`Applied` 只活在进程内，守护进程重启后它是空的，若只从 `applied.xray_users` 里筛「该删谁」，这些人就永远不会被删（那样 spec §4.2「超限/到期 → Xray RemoveUser」与 M3「到期用户被拒」都落不了地）。xray 对「不存在的 email」报错，这种错按成功处理；去重靠 `Applied::xray_removed`（删成功才记账）。
2. **`NRestarts` 变化时连记账一起清空重放**：`host.unit_property("xray", "NRestarts")` 一变，`applied.xray_users` 与 `applied.xray_removed` 同时清空 ⇒ 下一轮既重放 AddUser 也重放 RemoveUser。再加每 60 秒一次的安全网兜住「本轮 gRPC 失败」。长期修法有两条（都超出 P2 的改动许可，需 Fable 裁决）：(a) P0 在 `User` 上加一个由 P2 维护的派生字段（如 `enforced_block: bool`），`render::xray::clients` 一并过滤；(b) P1 Task 10 改成只传 `state.users` 里当前有效的子集（需要它能拿到 P2 的限额判定，等于把判定挪进 `bui-schema`）。

---

## 文件结构

```
crates/bui/Cargo.toml                          **只由 T1 改一次**：6 个依赖 + [build-dependencies] 一次加齐（T4 / T11 只消费，不再动它，免得三个并行分支各改一次必撞 Cargo.lock）
crates/bui/build.rs                            （新）tonic-prost-build 生成 Xray gRPC 代码（T4）
crates/bui/proto/…                             （新）vendored Xray v26.3.27 的 10 个 .proto（T4，目录树照仓库根）
crates/bui/src/modules/mod.rs                  追加一行 `pub mod panel;`（T1）
crates/bui/src/serve.rs                        `modules()` 追加一行 `Arc::new(PanelModule::new()),` + 六模块断言改成子集断言（**T13**，裁决 D14）
crates/bui/src/reconcile/mod.rs                `Module` trait 追加 `public_routes()` 默认方法（**T0**，裁决 D1）
crates/bui/src/api/mod.rs                      `router()` 在 `require_admin` 外合并 `public_routes` + 两条回归测试（**T0**，裁决 D1）
crates/bui/src/main.rs                         `Command::AuthHook` 那一支改为调 `panel::auth_hook::run`（T3，决策 D3）
crates/bui/src/modules/panel/mod.rs            PanelModule / Shared / SampleCache / TxRx / XrayApi / Hy2Api / 常量；T13 接线
crates/bui/src/modules/panel/fakes.rs          #[cfg(test)] FakeXray / FakeHy2（T1）
crates/bui/src/modules/panel/testsupport.rs    #[cfg(test)] Harness / token / send / text（T1）
crates/bui/src/modules/panel/snapshot.rs       auth-snapshot.json 形状与原子重写（T2）
crates/bui/src/modules/panel/auth_hook.rs      bui auth-hook 极简路径（T3）
crates/bui/src/modules/panel/xray.rs           tonic 客户端：AddUser / RemoveUser / QueryStats + `xray api rmu` 退路（T4）
crates/bui/src/modules/panel/hy2.rs            两个 hysteria trafficStats 客户端：/traffic?clear=1 / /online / /kick（T5）
crates/bui/src/modules/panel/users.rs          v3 兼容投影、CRUD 域逻辑、同步反应器 sync_users（T6）
crates/bui/src/modules/panel/traffic.rs        采样、累加、落盘、月度重置、限额执行与恢复（T7）
crates/bui/src/modules/panel/api_admin.rs      管理员端点（T8）
crates/bui/src/modules/panel/api_public.rs     /api/sub /api/subscription /api/clash /api/nodes（T9）
crates/bui/src/modules/panel/api_me.rs         /api/me/* 的 501 桩（T10）
crates/bui/src/modules/panel/assets.rs         rust-embed 前端 5 文件 + 引导脚本（T11）
crates/bui/src/modules/panel/packages.rs       客户端包缓存 + /packages/*（T12）
web/app.js                                     两处：login() 删 localStorage `ap`、addUser() 改 POST（T11）
```

**T1 一次建齐 `modules/panel/` 下的全部桩文件**（照 P1 Task 1 的做法）：`panel/mod.rs` 把 `pub mod` 行一次写全，其余文件内容只有一行 `//! placeholder filled by Task N`。这样后续任务只填自己的文件，并行合并零冲突，任何单个任务合并后 workspace 都能编译。

---

## 依赖与契约决策

### 消费 P1 的签名（逐字照 P1 计划，不得改写）

```rust
// Task 2
crate::state::store::Store::{open, create, read, update, path}
//   read(&self) -> Arc<State>；update(&self, f: impl FnOnce(&mut State)) -> anyhow::Result<Arc<State>>
crate::state::store::write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()>   // pub(crate)，tmp + 0600 + rename
crate::state::runtime::{Runtime, RuntimeData, DriftItem, ReconcileReport, WatchdogRecord}
//   Runtime::read(&self) -> RuntimeData；Runtime::update(&self, f: impl FnOnce(&mut RuntimeData)) -> RuntimeData
//   RuntimeData.extra: BTreeMap<String, serde_json::Value>（#[serde(flatten)]，P2/P3 的运行时字段就放这里）
//   RuntimeData.watchdog: BTreeMap<String, WatchdogRecord>
//   WatchdogRecord { fails: u32, restarts: u32, last_restart_at: Option<String>, backoff_until: Option<String> }
// Task 3
crate::sys::{Host, CmdOut, Proto};  // Host 全同步；CmdOut { status, stdout, stderr } + ok()
//   host.run(program, args) -> Result<CmdOut>；host.which(p) -> bool
//   host.unit_property(unit, prop) -> Result<Option<String>>；host.now() -> time::OffsetDateTime（now_utc）
crate::sys::fake::FakeHost;         // #[cfg(test)]，with() / ops() / text() / mode() / advance()
// Task 4
crate::reconcile::{Artifact, Module, RenderCtx, DaemonCtx, MANAGED_UNITS};
//   trait Module { fn name(&self) -> &'static str; fn render(&self, &State, &RenderCtx) -> Vec<Artifact>;
//                  fn routes(&self) -> axum::Router<crate::api::AppState> { Router::new() }
//                  fn spawn(&self, DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> { Vec::new() } }
//   DaemonCtx { store: Store, runtime: Runtime, bus: crate::api::EventBus, host: Arc<dyn Host>, paths: Paths }
crate::api::{AppState, Event, EventBus};
//   Event::{StateChanged(&'static str), RelayRestarted, ReconcileRequested { force: bool }}
//   AppState { store, bus, runtime, host: Arc<dyn Host>, started_at: OffsetDateTime, version: &'static str, login: LoginLimiter }
// Task 9
crate::kernels::{Fetcher, HttpFetcher, Manifest, Asset, asset_arch, sha256_hex, manifest_url};
//   Manifest { version: String, kernels: BTreeMap<String,String>, artifacts: BTreeMap<String,Asset>, min_upgrade_from: Option<String> }
//   Asset { url: String, sha256: String }；trait Fetcher（**同步**，只能在 spawn_blocking 里调）
// Task 13
crate::api::auth::{issue_token, decode_token, hash_password, verify_password, require_admin, client_ip, UdsPeer};
crate::api::health::{HealthResponse, ServiceStatus};
// Task 15（**已合并**，`v4` = eb10eb1；只有 T12 / T13 用到）
crate::serve::modules(manifest: Option<Manifest>) -> Registry;   // T13 在 modules 向量里追加一行
//   Registry { modules: Vec<Arc<dyn Module>>, manifest: Arc<std::sync::RwLock<Option<Manifest>>> }
crate::serve::load_cached_manifest(host: &dyn Host, paths: &Paths) -> Option<Manifest>;  // T12 的 cache_loop
// Task 1
crate::paths::{auth_snapshot_file, manifest_file, verify_dir};
crate::util::{fmt_rfc3339, parse_rfc3339};
crate::redact::{url_credentials, secret};
crate::testutil::sample_state() -> bui_schema::model::State;     // #[cfg(test)]
```

### 消费 P0 `bui-schema` 的签名（以 `crates/bui-schema/src` 的真实代码为准，2026-09-12 核对）

```rust
bui_schema::paths::Paths { base_dir, certs_dir, bin_dir }     // 三个字段都 pub；Paths::default_server()
bui_schema::model::{State, NodeParams, User, Credentials, Protocol, Entitlements, ResidentialEntitlement,
                    TrafficLimit, Usage, PortalAuth, Billing, Admin, Residential, ResiMode};
//   User { user_id: Uuid, username: String, note: String, created_at: String, disabled: bool,
//          credentials: Credentials { hy2_password: String, vless_uuid: Uuid },
//          entitlements: Entitlements { protocols: Vec<Protocol>, direct: bool,
//                                       residential: Option<ResidentialEntitlement>, expires_at: Option<String>,
//                                       traffic_limit: TrafficLimit { total_bytes: Option<u64>, monthly_bytes: Option<u64> } },
//          usage: Usage { total_bytes: u64, monthly_bytes: u64, month_key: String, last_seen_at: Option<String> },
//          portal_auth, billing }
//   Reality::sni(&self) -> &str（dest 去端口）；Reality::short_id(&self) -> &str
//   Residential::default_group(&self) -> Option<&ResidentialGroup>
bui_schema::nodes::{Node, NodeKind, Transport, nodes_for(&User, &NodeParams, &Residential) -> Vec<Node>};
bui_schema::render::SplitRules { enabled, global, keywords };  // SplitRules::from_group(&ResidentialGroup) -> Self
bui_schema::render::subscription::{uri_list(&[Node], username: &str) -> String,
                                   singbox(&[Node], &SplitRules, dial_ip: &str) -> serde_json::Value,
                                   clash(&[Node], username: &str, &SplitRules) -> String};
```

`Node` / `NodeKind` / `Transport` / `SplitRules` 已在 P4 Task 2 补齐 serde derive（`kind` snake_case、`transport` 内部标签 `type`），**`/api/nodes` 直接序列化它们，P2 不得自己拼 JSON**（裁决记录原文）。

### 决策项（D1 / D2 / D14 已在总纲「裁决记录」落字，2026-09-12）

| # | 议题 | 本计划采用的做法 | 为什么必须动 P1 / 备选 |
|---|---|---|---|
| **D1**（**已批准**，落在 **T0**） | P1 的 `api::router()` 把**所有** `Module::routes()` 合并在 `require_admin` 里面，而 P2 的订阅（`/api/sub` 等）、`/api/nodes`、嵌入前端、`/packages/*`、`/api/me/login` 按 spec §4.3 必须**无鉴权** | `Module` trait 追加带默认实现的 `fn public_routes(&self) -> Router<AppState> { Router::new() }`（`reconcile/mod.rs` 加 4 行）；`router()` 把各模块的 `public_routes` 合并在中间件**外面**（`api/mod.rs` 加 3 行）。两处都是纯追加、不改名、不动既有语义 | 裁决原文：「这是 P2 唯一可改的 P1 文件对，改动限于这两处」。因此它单独成 **Task 0**（两个文件一个 commit + 两条回归测试），T1…T13 再也不碰 P1 的路由装配。不改则订阅端点一律 401，M1 的「v2rayN 四节点可连」不可达 |
| **D2**（**不批准**） | `HealthResponse` 只给 P3 留了 `residential: Option<Value>`，没有用户/流量位 | 裁决原文：「不批准改 `api/health.rs`；用户/流量摘要放 P2 自己的 `GET /api/stats`（现有契约）与 `GET /api/users/health`，删除 T1 第 4 步对 health 的改动」。落地：T7 的 `write_health_summary` 照旧写 `runtime.extra["users"]`（`RuntimeData.extra` 是 `#[serde(flatten)]`，P1 Task 2 明写 P2/P3 的运行时字段落这里），由 **T8 的 `GET /api/users/health`** 读出来；`api/health.rs` 一个字不改 | 代价：`bui status --json`（= `/api/health`）不含用户段，M3 验收与运维看 `/api/users/health` 与 `/api/stats` 两处。T8 因此多一条端点 + 一条测试（16 → 17） |
| **D3** | `main.rs` 现在的 `Command::AuthHook` 那一支是 `anyhow::bail!("auth-hook 由 P2 实现")` | 改成 `std::process::exit(panel::auth_hook::run(args))`，位置不变（仍在建 tokio runtime、初始化 tracing **之前**） | P1 Task 1 已预授权：「P2 落地钩子时只替换那一支，不必回头重构入口」。列在这里只为让裁决表完整 |
| **D4** | `bui` 的 `Cargo.toml` 要加 6 个依赖 + 2 个 build-dependencies | 只在 `[dependencies]` / 新增 `[build-dependencies]` 末尾**追加**，不改已有行、不改根 `Cargo.toml`（那是 P0 的文件）。**六个依赖与 `[build-dependencies]` 全部由 T1 一次加齐**：T1 / T4 / T11 是三个并行分支，各改一次 `Cargo.toml` 就等于 `Cargo.lock` 必冲突；`[build-dependencies]` 在还没有 `build.rs`（T4 才建）的阶段只影响依赖解析、不影响编译 | 机械合并，无语义冲突。`protoc-bin-vendored` 是为了不引入系统 `protoc` 依赖（本机没装，CI 也不该装），已实测可用 |
| **D5** | v3 的 `/api/bandwidth`（服务端上下行带宽）在 v4 无落点：`bui-schema` 渲染的 hysteria 配置固定 `ignoreClientBandwidth: true` 且没有 `bandwidth:` 段，`state` 里也没有对应字段 | GET 返回 `{"up":0,"down":0}`（= 不限）；POST 返回 `501 {"error":"v4 不再设置服务端带宽：config.yaml 固定 ignoreClientBandwidth: true"}`，前端会把它 toast 出来 | 要真支持就得在 P0 的 `NodeParams` 加字段 + 改 `render::hysteria`（P0 改动）。spec §0 已把「按用户限速」列为不做，服务端总带宽没被任何一条能力要求，因此建议就此删掉 |
| **D6** | `render::xray::config` 要求只传有效用户，P1 Task 10 传全量 ⇒ xray 重启后到期/超限用户复活（见「渲染边界」一节） | Task 6 的 `sync_users` 对**每个**被封 / 到期 / 禁用且有 Reality 权益的用户**无条件**发 RemoveUser（不看本进程有没有 AddUser 过，xray 报「不存在」当成功），用 `Applied::xray_removed` 去重；`NRestarts` 变化时把 `xray_users` 与 `xray_removed` 一起清空重放；再加每 60 秒安全网 | 长期修法 (a) P0 加派生字段、(b) P1 Task 10 过滤，两者都超出 P2 的改动许可，**仍待裁决**（见「待裁决与需落字」）。P2 这一版只兜住运行期，每次 xray 重启到下一轮同步之间仍有一个窗口 |
| **D7** | 月度重置的时区。spec §4.2 写「服务器本地时间每月 1 日 00:00」，而 P1 的 `RealHost::now()` 是 `OffsetDateTime::now_utc()` | 用 `host.now()`（UTC）算 `month_key`，即按 **UTC 月初**重置 | 要按本地时区就得给 `Host` 加一个 `local_offset()`（P1 改动）。两台 VPS 的系统时区本就是 UTC，实际差异为零；若主理人要按东八区，改一处 `month_key` 即可 |
| **D8** | C4 的 `artifacts` 只有一个 `sing-box-linux-<arch>` 键，而 `kernels` 同时有 `sing_box` 与 `client_sing_box` 两个版本号 | 客户端包缓存用 `sing-box-linux-<arch>`；当 `client_sing_box != sing_box` 时只 `warn!` 一条并仍缓存该 artifact | 要真支持两个版本，P5 得加 `sing-box-client-linux-<arch>` 键（C4 改动）。当前 C4 示例里两者相等 |
| **D9** | v3 的 `/api/version` 与 `/api/kernel-versions`（无鉴权，给老客户端查版本用） | 一并删除（不注册路由 ⇒ 404） | `web/app.js` 不调用它们，P4 的 `bui-c` 只读 `/packages/manifest.json` 与 GitHub。spec §4.3 改动 2 只点名了 `/auth/hysteria`、`/api/kernel-downloads`、install-key，所以这两条要单独批。**代价**：v3 `b-ui-client.sh` 第 357 / 4958 / 5650 行会调它们 ⇒ 先装 `bui-c` 再切服务端（见「P1 边界」一节末尾） |
| **D10** | v3 的 `GET /packages`（目录列表，返回 `{packages:[{name,size,modified}]}`） | 删除（前端不调用，`bui-c` 只按名字取文件） | 同上，属「顺带删」，请一并裁决。v3 客户端也不调这条（它按固定文件名取），但它取的 `b-ui-client.sh` / `hysteria-linux-*` / `xray-linux-*` 在 v4 的 `packages/` 里不再有 |
| **D11** | 改管理员密码是否同时轮换 `jwt_secret` | 轮换。v3 的行为是写 `admin.env` + 重启进程 ⇒ 随机 secret ⇒ 所有 token 失效，响应文案也是「密码已更新，请重新登录」 | 不轮换就等于「改了密码旧 token 还能用 24 小时」，与文案和 v3 行为都不符 |
| **D12** | `xray api adu` 退路（X9：需要一份完整 inbound JSON 片段） | **不实现**。只实现 `xray api rmu -tag=<tag> <email>` 退路 | AddUser 失败是自愈的：`state.users` 已落进 `xray-config.json`，xray 下次启动就带上该用户；再加 60 秒安全网重试 gRPC。RemoveUser 失败不自愈（超限用户会继续用 Reality），所以只给它退路 |
| **D13** | 创建用户时前端仍会传 `speed`（Mbps）与 `sni` | 两者都接受但忽略，响应里回显真正生效的值（`sni` 回显 `node.reality.sni()`，不回显 speed）；`/api/users` 的投影里不再有 `limits.speedLimit`（编辑框因此显示为空） | spec §0 明写「按用户限速（内核不支持，`speedLimit` 字段删除）」，v4 的 SNI 全局唯一。前端不改是硬约束，所以只能「接受并忽略」 |
| **D14**（**已批准**，落在 T13） | 注册 `PanelModule` 会打红 P1 既有测试 `serve::tests::p1_registers_exactly_six_modules_and_shares_the_manifest_handle`（它断言**精确**六模块名列表） | 裁决原文：「`serve.rs` 的精确六模块断言改为『包含 P1 六个模块名』的子集断言，P2 加自己的模块名」。T13 在**同一个 commit** 里把 `assert_eq!(names, vec![…])` 换成对「P1 六名 + `"panel"`」的 `contains` 子集断言（函数名不改） | 于是 `serve.rs` 的改动面是「注册一行 + 一处断言」；**P3 追加 `ResidentialModule` 时只在子集数组里加 `"residential"`**，不必再走一轮裁决 |

---

## P1 边界：六处改动的归属与裁决状态（事实陈述，2026-09-12）

总纲 C2 原文只许 P2 在 P1 动两处。本计划实际动**六处**，其中四处已在总纲「裁决记录」里逐字落字（D1、D3、D14 批准；D2 不批准）。**已按裁决记录批准，无需再等任何人**；下表只是让实现者一眼看清每处的边界。

| 改动处 | 裁决 | 现状（2026-09-12 对 `v4` 的真实代码核对） |
|---|---|---|
| `modules/mod.rs` 追加一行 `pub mod panel;`（T1） | C2 已许可 | — |
| `serve.rs` 的 `modules()` 追加注册一行（T13） | C2 已许可 | `pub fn modules(manifest: Option<Manifest>) -> Registry` 已在（P1 Task 15 已合并） |
| `serve.rs` 的 `serve::tests::p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 改成**子集断言**（T13） | **D14 已批准** | 真实 `crates/bui/src/serve.rs:1016-1023` 断言 `names == ["core-files","units","system","ssh","certs","watchdog"]`（精确列表）。裁决口径：改成「包含 P1 六个模块名」的子集断言，P2 在其中加自己的 `"panel"`；P3 后续加 `"residential"` 不必再裁 |
| `reconcile/mod.rs` 的 `Module` trait 追加带默认实现的 `public_routes()`（**T0**） | **D1 已批准** | 真实 `trait Module` 只有 `name` / `render` / `routes` / `spawn` 四个方法。裁决原文：`reconcile/mod.rs` 与 `api/mod.rs` 是**P2 唯一可改的 P1 文件对，改动限于这两处** |
| `api/mod.rs` 的 `router()` 在 `require_admin` **外**合并 `public_routes()`（**T0**） | **D1 已批准** | 真实 `router()` 把所有 `m.routes()` fold 进 `protected`，再整棵 `.layer(from_fn_with_state(state, auth::require_admin))`；axum 的 `layer` 套的是整棵子 Router，模块内部无法「打洞」 |
| `main.rs` 的 `Command::AuthHook` 那一支（T3） | D3（P1 Task 1 代码注释里已预授权） | 那一支带注释「P2 落地钩子时只替换那一支，不必回头重构入口」 |

**`api/health.rs` 一个字都不改**（裁决 D2 **不批准**，原文：「不批准改 `api/health.rs`；用户/流量摘要放 P2 自己的 `GET /api/stats`（现有契约）与 `GET /api/users/health`，删除 T1 第 4 步对 health 的改动」）。落地口径：T7 的 `write_health_summary` 仍把摘要写进 `runtime.extra["users"]`，由 **T8 的 `GET /api/users/health`**（管理员端点，走 `require_admin`）读出来；每用户的 tx/rx 仍走 v3 既有的 `GET /api/stats`。`/api/health` 因此不含用户段，M3 验收改看这两条端点。

**同一轮里仍请一起裁的三项**（不动 P1 代码，但都是删端点 / 改行为，spec 未明写）：D5（`POST /api/bandwidth` → 501）、D9（删 `/api/version`、`/api/kernel-versions`）、D10（删 `GET /packages` 目录列表）。

⚠️ **对旧客户端的影响**（D9 / D10 的真实代价，必须写进 M1 / M4 清单）：baiyi 上尚未迁移的 v3 `b-ui-client.sh` 会调 `/api/version`（第 357 行，自更新版本检查）与 `/api/kernel-versions`（第 4958、5650 行，内核版本比对），并从 `/packages/b-ui-client.sh`（第 397、453 行）与 `/packages/{hysteria,xray,sing-box}-linux-*`（第 5052、5076、5100 行）自更新。v4 的 `/packages/` 只缓存 `bui-c-linux-<arch>` 与 `sing-box-linux-<arch>`（T12），所以**服务端切 v4 之后旧客户端脚本的自更新与内核同步一律失效**。口径：**先给客户端装 `bui-c`（`/api/install-command` 给的命令），再切服务端**；M1 / M4 清单里写明这条顺序。

---

## 并行编排

依赖是**编译依赖**（下游任务的代码里出现了上游任务定义的类型 / 常量 / 函数），已逐条按代码核对：

| 阶段 | 任务 | 依赖 | 依赖的具体符号 |
|---|---|---|---|
| 前置（T0…T11 起） | — | P1 Task 4 与 Task 13 已合并到 `v4`；**裁决 D1 已批准**（T0 落地，见「P1 边界」） | `reconcile::{Artifact, Module, RenderCtx, DaemonCtx, MANAGED_UNITS}`、`api::{AppState, Event, EventBus}`、`api::auth::{issue_token, require_admin}`、`api::health::{get, HealthResponse}`、`state::store::{Store, write_atomic}`、`state::runtime::Runtime`、`sys::{Host, fake::FakeHost}` |
| 前置（**T12 / T13** 起） | — | **P1 Task 15 已合并到 `v4`（已满足）** | `serve::modules(Option<Manifest>) -> Registry`（T13 的注册那一行与注册回归测试）、`serve::load_cached_manifest(&dyn Host, &Paths) -> Option<Manifest>`（T12 的 `cache_loop`）。2026-09-12 核对：`v4` = `eb10eb1`（`merge: v4-p1-t15` + `merge: v4-p1-t16` 都已在），`crates/bui/src/serve.rs` 里两个函数都在 ⇒ **这条前置已结清，T12 / T13 的编排不必再等**（唯一未到位的前置是 T12 要的 P4 Task 13 `scripts/bui-c-install.sh`） |
| 串行 | **T0** P1 的公开路由通道 | 前置 | `reconcile/mod.rs` 的 `Module::public_routes()` 默认方法 + `api/mod.rs::router()` 在 `require_admin` 外合并它（裁决 D1，**P2 唯一可改的 P1 文件对**）。两个文件一个 commit，带两条回归测试（公开路由不经 `require_admin`、受保护路由仍需 Bearer） |
| 串行 | **T1** 骨架 | T0 | 建齐 `panel/` 桩文件；定义 `PanelModule` / `Shared` / `TxRx` / `SampleCache` / `Applied` / `XrayApi` / `Hy2Api` / 常量；`modules/mod.rs` 一行（**`serve.rs` 归 T13**：注册要与 `routes()`/`public_routes()`/`spawn()` 的接线同一个 commit，否则 T1…T12 期间守护进程会带着一个空模块跑；且 T13 还要把那条六模块断言改成子集断言，见 D14） |
| 并行 | **T2** 快照、**T4** Xray gRPC、**T5** hysteria API、**T9** 订阅端点、**T10** `/api/me` 桩、**T11** 前端嵌入 | 各自只依赖 T1 | T2 用 `write_atomic`；T4 实现 `panel::XrayApi`；T5 实现 `panel::Hy2Api`；T9/T10/T11 用 `testsupport::Harness` 与 `Shared` |
| 并行 | **T3** auth-hook、**T12** 包缓存 | T3 依赖 T2；T12 依赖 T1 + T11 + P1 Task 15（**已合并**）+ P4 Task 13 | T3 用 `snapshot::{Snapshot, SnapshotUser}`；T12 用 `kernels::{Manifest, Asset, Fetcher, sha256_hex}`、`serve::load_cached_manifest` 与 `assets::{Scripts, install_script, content_type}`（T11）——**T12 排在 T11 之后** |
| 串行 | **T6** 用户域与同步反应器 | T2、T4 | `snapshot::{Snapshot, write_if_changed}`、`XrayApi`、`Shared::applied` |
| 串行 | **T7** 采样与限额 | T5、T6 | `Hy2Api`、`users::{sync_now, sync_users, SyncOutcome, blocked_set, month_key}`、`Shared::{cache, pending, xray_seen}` |
| 串行 | **T8** 管理员 API | T6（+T7 的 `SampleCache` 读法由 T1 定义） | `users::{PanelUser, CreateRequest, UpdateRequest, create, update, delete}` |
| 串行 | **T13** 收口 | 全部（P1 Task 15 已合并） | `PanelModule::{routes, public_routes, spawn}` 接线 + `serve.rs` 的注册一行与六模块断言（D14） + `runtime.extra["users"]` |

**四条曾经互相打死的环，本计划的解法**：
1. `Shared` 要同时被 `routes()`（handler 闭包捕获）与 `spawn()`（后台任务）用，而 `AppState` 里没有它 —— 所以 `Shared` 归 **T1** 定义、由 `PanelModule` 持有 `Arc<Shared>`，每个 API 文件导出 `pub fn routes(shared: Arc<Shared>) -> Router<AppState>`，T13 只做 `merge`。
2. `XrayApi` / `Hy2Api` 的**真实实现**在 T4/T5，但 T6/T7 要在测试里注入 fake —— 所以 **trait 与 fake 都归 T1**（`panel/mod.rs` + `panel/fakes.rs`），T4/T5 只补 `impl`。
3. `AppState` 里没有 `paths`，而 `/packages/*` 的 handler 需要 `<base>/packages` —— `Shared` 用 `OnceLock<Paths>`，`render()` 与 `spawn()` 都调 `set_paths(&ctx.paths)`，handler 读 `shared.paths()`，未设置时回落 `Paths::default_server()`（生产下 P1 的 `serve::run` 传的就是它）。
4. T12 要把 `scripts/bui-c-install.sh` 发到 `/packages/bui-c-install.sh`，脚本由 **P4 Task 13** 交付 —— T11 建 `rust-embed` 的 `Scripts` 资产集（`include = "bui-c-install.sh"`），T12 只消费；**T12 的前置里写明「P4 Task 13 已合并」**，未合并时先做 T13 以外的其它任务。

---

## v3 端点逐个契约（spec §4.3「以 `web/app.js` 实际调用的集合为准，M1 前逐一列表」）

下表是 `grep -n "api(" web/app.js` 与 `grep -n 'if (r === ' web/server.js` 的全量交叉结果（2026-09-12 于 v3.6.3 基线）。**「v4 处置」列就是 T8/T9/T10/T11/T12 的验收清单。**

| # | v3 路径与方法 | app.js 调用点 | server.js 处理器 | v3 请求 / 响应 | v4 处置 |
|---|---|---|---|---|---|
| 1 | `POST /api/login` | `web/app.js:55` | `web/server.js:1693-1702` | `{"password":"…"}` → `200 {"token":"…"}` / `401 {"error":"Auth failed"}` / `429 {"error":"Too many attempts. Try again later."}` | **P1 Task 13 已实现**（限速改 5 次/分钟/IP、JWT 密钥持久化）。P2 不动；`app.js:61` 的 `localStorage.setItem("ap", pw)` 由 T11 删除 |
| 2 | `GET /api/manage?key=<管理员密码>&action=create&…` | `web/app.js:190-198` | `web/server.js:1157-1226`（`handleManage`） | query 明文管理员密码 + `action=create\|delete\|update\|list` → `{"success":true,"user","password","uuid","sni"}` | **删除整个端点**（审计 web-C6/C7、spec §4.3 改动 1）。`create` 的语义搬到 `POST /api/users`（T8），`delete` / `update` 已有 REST 对应，`list` 无人调用 |
| 3 | `GET /api/version` | 无 | `web/server.js:1707-1719` | → `version.json` 内容 | **删除**（决策 D9） |
| 4 | `GET /api/kernel-versions` | 无 | `web/server.js:1721-1737` | → `{"hysteria2","xray","singbox"}` | **删除**（决策 D9） |
| 5 | `GET /api/kernel-downloads` | 无 | `web/server.js:1739-1767` | → 每内核的 `{version,syncedAt,files:{amd64,arm64}}` | **删除**（spec §4.3 改动 2、审计 web-C11）。客户端改读 `/packages/manifest.json`（T12） |
| 6 | `GET /api/install-command` | `web/app.js:78` | `web/server.js:1770-1781` | → `{"command","key","server","note"}`，**在 Bearer 检查之前返回 install key**（审计 web-C8） | **保留但不带 key**（spec §4.3 改动 2）：`{"command","server","note"}`，command 指向 `/packages/bui-c-install.sh`（**T12**，与 `/packages/*` 同一个文件） |
| 7 | `GET /api/subscription/<user>` | `web/app.js:423`（下载按钮） | `web/server.js:1783-1806` | → sing-box 完整配置 JSON，`Content-Disposition: inline; filename="<user>.json"` | **保留**，改由 `bui_schema::render::subscription::singbox(&nodes, &split, dial_ip = node.public_ip)` 渲染（T9） |
| 8 | `GET /api/sub/<user>` | `web/app.js:298`（链接文本） | `web/server.js:1807-1920` | → base64 的 `vless://`/`hysteria2://` 列表，`text/plain` | **保留**，改由 `uri_list(&nodes, username)` 渲染（T9） |
| 9 | `GET /api/clash/<user>` | `web/app.js:431`（链接文本） | `web/server.js:1921-1941` | → mihomo YAML，`text/yaml` | **保留**，改由 `clash(&nodes, username, &split)` 渲染（T9） |
| 10 | `GET /api/users` | `web/app.js:114` | `web/server.js:1945-1946` | → `users.json` 数组原样：`[{username, protocol, createdAt, limits:{expiresAt,trafficLimit,monthlyLimit,speedLimit}, usage:{total, monthly:{"YYYY-MM":n}}, password, uuid, sni, residential}]` | **保留，形状不变**：`users::PanelUser` 从 `State.users` 投影出同名字段（`speedLimit` 除外，决策 D13）（T6/T8） |
| 11 | `POST /api/users` | 无（v3 前端走 #2） | `web/server.js:1946-1968` | `{"username","password"}` → `{"success":true}`（只写两个字段，是个半成品） | **重写**：接管 #2 的 `create` 全语义，JWT 保护（spec §4.3 改动 1）。请求 `{"username","password"?,"days"?,"traffic"?,"monthly"?,"protocol"?,"residential"?,"sni"?,"speed"?}` → `{"success":true,"user","password","uuid","sni"}`（T8）；`web/app.js` 的 `addUser()` 改调它（T11） |
| 12 | `DELETE /api/users/<user>` | `web/app.js:212` | `web/server.js:1970-1975` | → `{"success":true}` | **保留**（T8），并联动快照重写 + Xray `RemoveUser` |
| 13 | `PUT /api/users/<user>` | `web/app.js:271-281` | `web/server.js:1977-2066` | `{"username","password"?,"days","traffic","monthly","speed"}`（days = 天数、traffic/monthly = GB、speed = Mbps） → `{"success":true,"user"}` | **保留，语义不变**（`speed` 忽略，决策 D13）（T8） |
| 14 | `GET /api/stats` | `web/app.js:114` | `web/server.js:2068` | → `{"<用户名>":{"tx":n,"rx":n}}`（hysteria `/traffic` 与 `xray api stats` 合并） | **保留形状**，改为进程内共享缓存（审计 eff-C1~C4、web-C3/C4：住宅 9998 也读、fusion 用户也计）（T7/T8） |
| 15 | `GET /api/online` | `web/app.js:114` | `web/server.js:2069` | → `{"<用户名>":n}`（n = 连接数） | **保留形状**：两个 `/online` 并集 ∪ 30 秒内有 Xray 增量的用户（T7/T8） |
| 16 | `POST /api/kick` | `web/app.js:218` | `web/server.js:2070` | `["<用户名>"]` → `true`（v3 直接把布尔透传） | **保留**，改回 JSON：`{"success":bool,"kicked":n}`（`n` = 认得出的用户名个数，认不出的静默跳过）；内部把用户名映射成 `user_id` 再 POST 到两个 hysteria 的 `/kick`（体 `["<user_id>"]`，H12）（T8） |
| 17 | `GET /api/config` | `web/app.js:105`、`531` | `web/server.js:392-489`（`getConfig`）、`2071` | → `{"domain","port"(字符串),"xrayPort","pubKey","shortId","sni","portHopping":{"enabled","start","end"},"obfs":{"enabled","type","password"}}` | **保留，形状不变**，数据源从「解析 config.yaml / xray-config.json」改成读 `state`（T8） |
| 18 | `GET /api/port-hopping` | `web/app.js:507` | `web/server.js:2074-2092` | → `{"enabled","start","end"}` | **保留**，真源改为 `state.node.ports.hy2_hop`（T8） |
| 19 | `POST /api/port-hopping` | `web/app.js:523` | `web/server.js:2093-2152` | `{"enabled","start","end"}` → `{"success":true,"enabled","start","end"}`；**v3 写的是 iptables REDIRECT**（审计 web-C13） | **重写**：改 `state.node.ports.hy2_hop` + `Event::StateChanged("ports")`，由对账重写 `config.yaml` 的 `listen:` 行并重启 `hysteria-server`；**不产生任何 iptables 规则**（spec §3.1）（T8） |
| 20 | `GET /api/residential`、`POST/DELETE /api/residential`、`POST /api/residential/enable`、`POST /api/residential/global`、`POST /api/residential/urls`、`DELETE /api/residential/urls/<hostPort>`、`GET /api/residential/health` | `web/app.js:790-1067` | `web/server.js:2154-2516` | 见 P3 | **归 P3**（`ResidentialModule::routes`）。P2 一行都不写 |
| 21 | `GET /api/hy2/watchdog/status` | `web/app.js:1175` | `web/server.js:2517-2565` | → `{"watchdog_active","next_run_at","last_run_at","fail_count","log_recent_lines":[...]}` | **保留形状（兼容 shim）**，数据源改为 `runtime.watchdog`（P1 Task 12 写）：`watchdog_active = true`（守护进程内常驻，无 timer）、`next_run_at = null`、`last_run_at` = 最近一条 `last_restart_at`、`fail_count` = Σ`fails`、`log_recent_lines` = 由 records 生成的最近 5 行中文（T8） |
| 22 | `GET /api/masquerade` | `web/app.js:456` | `web/server.js:2566-2584` | → `{"masqueradeUrl","masqueradeDomain"}` | **保留**，从 `state.node.reality.dest` 推导（T8） |
| 23 | `POST /api/masquerade` | `web/app.js:465` | `web/server.js:2585-2602` | `{"url"}` → `{"success":true,"domain"}` | **保留**：改 `state.node.reality.{dest,server_names}` + `StateChanged("masquerade")`；由对账重写 xray（结构哈希变 ⇒ 重启 xray）与两份 hysteria 配置（`masquerade.proxy.url` 由 `reality.sni()` 推导）（T8） |
| 24 | `GET /api/bandwidth` | `web/app.js:478` | `web/server.js:2604-2617` | → `{"up","down"}`（Mbps） | **保留 GET，恒返回 `{"up":0,"down":0}`**（决策 D5）（T8） |
| 25 | `POST /api/bandwidth` | `web/app.js:491` | `web/server.js:2618-2641` | `{"up","down"}` → `{"success":true,"up","down"}` | **501**（决策 D5）（T8） |
| 26 | `POST /api/password` | `web/app.js:443` | `web/server.js:2642-2689` | `{"newPassword"}`（≥6 位）→ `{"success":true,"message":"密码已更新，请重新登录"}` | **保留**：argon2id 重哈希写 `state.admin.password_hash` + 轮换 `jwt_secret`（决策 D11），不重启进程（T8） |
| 27 | `POST /auth/hysteria` | 无（hysteria 内核调） | `web/server.js:2691-2712` | `{"auth":"user:pass"}` → `{"ok",id,rx?,tx?}` | **删除**（spec §4.3 改动 2）。鉴权改 `auth.type: command` + `bui auth-hook`（T3） |
| 28 | `GET /install-client?key=…` | 无（安装命令里） | `web/server.js:1514-1618` | → 带 key 校验的客户端安装脚本 | **删除**（install-key 机制取消）。改为 `GET /packages/bui-c-install.sh`（T12） |
| 29 | `GET /packages`、`GET /packages/` | 无 | `web/server.js:1619-1631` | → `{"packages":[{name,size,modified}]}` | **删除**（决策 D10） |
| 30 | `GET /packages/<file>` | 无 | `web/server.js:1633-1687` | → 文件字节 + `Content-Disposition: attachment`；`b-ui-client.sh` 特判注入版本号 | **保留并简化**：无 key、无版本注入；来源是 `<base>/packages/` 加嵌入的 `bui-c-install.sh`；另加 `GET /packages/manifest.json`（转发 `<base>/manifest.json`）（T12） |
| 31 | `GET /`、`/index.html`、`/style.css`、`/app.js`、`/qrcode.min.js`、`/logo.jpg` | 浏览器 | `web/server.js:1476-1513` | → 从 `ADMIN_DIR` 读盘 | **改为 `rust-embed` 嵌入**（spec §1「`web/` 前端三文件 `rust-embed` 嵌入 `bui`」，实际是 5 个文件）（T11） |

**v4 新增（无 v3 对应）**：`GET /api/users/health`（T8，管理员端点，读 `runtime.extra["users"]` 的用户与流量摘要——裁决 D2 不批准把这段塞进 `/api/health`）；`GET /api/nodes/<user>`（T9，载荷 = P4 决策 6）；`POST /api/me/login`、`GET /api/me`、`GET /api/me/subscription-links`、`GET /api/me/entitlements`、`GET /api/me/billing`、`POST /api/me/orders`、`GET /api/me/orders/{id}`（T10，一律 501）。

---

### Task 0: P1 的公开路由通道（`Module::public_routes()` + `api::router()`，裁决 D1）

**Files:**
- Modify: `crates/bui/src/reconcile/mod.rs`（`trait Module` 追加带默认实现的 `public_routes()`）
- Modify: `crates/bui/src/api/mod.rs`（`router()` 在 `require_admin` **外**合并各模块的 `public_routes()`；两条回归测试追加进该文件既有的 `#[cfg(test)] mod tests`）

**为什么单独成一个任务**：这两个文件是 P2 在 P1 代码里**唯一**可改的文件对（总纲「裁决记录」2026-09-12 的 D1 原文：「这是 P2 唯一可改的 P1 文件对，改动限于这两处」）。把它独立成第一个任务，是为了让这个越界面在一个只有两文件的 commit 里被一次审完；此后 T1…T13 再也不碰 P1 的路由装配。**已按裁决记录批准。**

**前置**：P1 Task 4 与 Task 13 已合并到 `v4`。2026-09-12 对真实代码核对：`crates/bui/src/reconcile/mod.rs` 的 `trait Module` 只有 `name` / `render` / `routes` / `spawn` 四个方法；`crates/bui/src/api/mod.rs::router()` 把基础三条路由与所有 `m.routes()` fold 进 `protected`，最后整棵 `protected.layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_admin))`，`/api/login` 是唯一挂在中间件外面的路由。

**Interfaces:**
- Consumes: `crate::reconcile::{Module, RenderCtx, Artifact}`、`crate::api::{AppState, EventBus}`、`crate::api::auth::{hash_password, require_admin, LoginLimiter}`、`crate::state::store::Store`、`crate::state::runtime::Runtime`、`crate::sys::{Host, fake::FakeHost}`、`crate::testutil::sample_state`
- Produces:
```rust
// crate::reconcile
pub trait Module: Send + Sync {
    // …既有的 name / render / routes / spawn 一个字都不改…
    /// 无鉴权路由：订阅、`/api/nodes`、嵌入前端、`/packages/*`、用户域桩。
    /// `api::router()` 把它合并在 `require_admin` **外面**（裁决 D1）。
    fn public_routes(&self) -> axum::Router<crate::api::AppState> {
        axum::Router::new()
    }
}
// crate::api（签名不变，只多合并一棵公开子 Router）
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router;
```

- [ ] **Step 1: 写失败测试**

追加到 `crates/bui/src/api/mod.rs` 既有的 `#[cfg(test)] mod tests` 末尾。该 `mod tests` 已有 `app_with_runtime()` / `app()` / `json()` / `post()` / `with_token()` / `login()` 与 `use super::*;`、`use std::sync::Arc;`、`use tower::ServiceExt;`、`use axum::http::{Request, StatusCode};`、`use crate::sys::Host;`（`host.now()` 要它）——**既有 helper 一行都不改**，下面只是同一套装配的「带模块」版本：
```rust
    /// 一个只为「公开路由不过鉴权、受保护路由仍要 Bearer」而存在的模块（裁决 D1 的回归锁）
    struct ProbeModule;
    impl Module for ProbeModule {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn render(
            &self,
            _s: &bui_schema::model::State,
            _c: &crate::reconcile::RenderCtx,
        ) -> Vec<crate::reconcile::Artifact> {
            Vec::new()
        }
        fn routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/admin", axum::routing::get(|| async { "admin" }))
        }
        fn public_routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/open", axum::routing::get(|| async { "open" }))
        }
    }

    /// 与 `app_with_runtime()` 同一套 AppState，只是把模块挂进 `router()`
    async fn app_with_modules(modules: Vec<Arc<dyn Module>>) -> (axum::Router, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = crate::api::auth::hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let host = Arc::new(FakeHost::new());
        let app_state = AppState {
            store,
            bus: EventBus::new(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: crate::api::auth::LoginLimiter::default(),
        };
        (router(app_state, &modules), d)
    }

    async fn get_status(app: &axum::Router, uri: &str, token: Option<&str>) -> StatusCode {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let req = match token {
            Some(t) => with_token(req, t),
            None => req,
        };
        app.clone().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn public_module_routes_skip_require_admin() {
        let (app, _d) = app_with_modules(vec![Arc::new(ProbeModule)]).await;
        assert_eq!(
            get_status(&app, "/probe/open", None).await,
            StatusCode::OK,
            "public_routes() 必须落在 require_admin 外面（裁决 D1）：订阅 / 嵌入前端 / /packages/* 靠它"
        );
    }

    #[tokio::test]
    async fn protected_module_routes_still_require_a_bearer_token() {
        let (app, _d) = app_with_modules(vec![Arc::new(ProbeModule)]).await;
        assert_eq!(
            get_status(&app, "/probe/admin", None).await,
            StatusCode::UNAUTHORIZED,
            "模块的 routes() 仍然整棵套在 require_admin 里"
        );
        let token = login(&app).await;
        assert_eq!(
            get_status(&app, "/probe/admin", Some(&token)).await,
            StatusCode::OK
        );
        // 公开通道不许顺手把 P1 自己的受保护端点漏出去
        assert_eq!(
            get_status(&app, "/api/health", None).await,
            StatusCode::UNAUTHORIZED
        );
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui api::`
Expected: 编译失败，`error[E0407]: method 'public_routes' is not a member of trait 'Module'`。

- [ ] **Step 3: 给 `Module` 加默认方法**

`crates/bui/src/reconcile/mod.rs` 的 `trait Module` 里，紧跟在 `routes` 之后、`spawn` 之前追加（带默认实现 ⇒ P1 的六个模块一个字都不改）：
```rust
    /// 无鉴权路由：订阅、`/api/nodes`、嵌入前端、`/packages/*`、用户域桩。
    /// `api::router()` 把它合并在 `require_admin` **外面**（裁决 D1）。
    fn public_routes(&self) -> axum::Router<crate::api::AppState> {
        axum::Router::new()
    }
```

- [ ] **Step 4: `router()` 在中间件外合并**

`crates/bui/src/api/mod.rs` 的 `router()`：**只插入下面两段（4 行 + 1 行 `.merge(public)`），既有行一个字都不改**（真实文件是 `rustfmt` 过的多行排版，照原样插，别顺手重排，否则 `cargo fmt --check` 与 diff 都变脏）。

在 `let protected = modules.iter().fold(...)` 那一段之后插入：
```rust
    // 裁决 D1：公开路由必须落在 require_admin 外面
    let public = modules
        .iter()
        .fold(axum::Router::new(), |acc, m| acc.merge(m.public_routes()));
```
在 `.route("/api/login", ...)` 之后、`.merge(protected.layer(...))` 之前插入一行：
```rust
        .merge(public)
```
插完的全貌（供核对，**不要照抄覆盖整个函数**）：
```rust
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router {
    let protected = axum::Router::new()
        .route("/api/health", axum::routing::get(health::get))
        .route("/api/reconcile", axum::routing::post(system::reconcile))
        .route(
            "/api/services/{unit}/{action}",
            axum::routing::post(system::service_action),
        );
    let protected = modules
        .iter()
        .fold(protected, |acc, m| acc.merge(m.routes()));
    // 裁决 D1：公开路由必须落在 require_admin 外面
    let public = modules
        .iter()
        .fold(axum::Router::new(), |acc, m| acc.merge(m.public_routes()));
    axum::Router::new()
        .route("/api/login", axum::routing::post(auth::login))
        .merge(public)
        .merge(protected.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        )))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
```
顺带把文件顶部那句文档注释补一句（它现在写「其余全部走 `require_admin`」，加了公开通道之后不再准确）：
```rust
/// 基础 Router：公开 `/api/login` 与各模块的 `public_routes()`，其余全部走 `require_admin`；
/// 再 merge 各模块的 `routes()`。
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui api:: && cargo test -p bui reconcile:: && cargo clippy -p bui --all-targets -- -D warnings && cargo fmt --check`
Expected: `api::` 原有测试全绿并多 2 条（`public_module_routes_skip_require_admin`、`protected_module_routes_still_require_a_bearer_token`）；`reconcile::` 全绿（默认方法不影响 P1 六个模块）；clippy / fmt 无输出。

- [ ] **Step 6: Commit**

```
git add crates/bui/src/reconcile/mod.rs crates/bui/src/api/mod.rs
git commit -m "feat(api): Module::public_routes 公开路由通道（裁决 D1）"
```

---

### Task 1: `panel` 子树骨架与共享类型

**Files:**
- Create（本任务填实）: `crates/bui/src/modules/panel/mod.rs`, `crates/bui/src/modules/panel/fakes.rs`, `crates/bui/src/modules/panel/testsupport.rs`
- Create（本任务只给「能编译的空壳」，Task 4 / 5 填实）: `crates/bui/src/modules/panel/xray.rs`, `crates/bui/src/modules/panel/hy2.rs`
- Create（本任务只建桩 `//! placeholder filled by Task N`）: `crates/bui/src/modules/panel/{snapshot,auth_hook,users,traffic,api_admin,api_public,api_me,assets,packages}.rs`
- Modify: `crates/bui/Cargo.toml`（`[dependencies]` 末尾一次追加**全部 6 个**依赖 + 新增 `[build-dependencies]`；T4 / T11 因此不必再动这个文件，见决策 D4）
- Modify: `crates/bui/src/modules/mod.rs`（追加一行 `pub mod panel;`）
- **不改** `crates/bui/src/reconcile/mod.rs` 与 `crates/bui/src/api/mod.rs`：`Module::public_routes()` 与 `router()` 的公开通道已由 **Task 0** 落地（裁决 D1，P2 唯一可改的 P1 文件对）；本任务只实现 `PanelModule::public_routes()` 的本体（T13 接线）
- **不改** `crates/bui/src/api/health.rs`：裁决 D2 不批准给 `HealthResponse` 加用户段，用户与流量摘要走 T8 的 `GET /api/users/health`
- **不改** `crates/bui/src/serve.rs`：`modules()` 里那一行归 **T13**。`v4` = `eb10eb1` 上这个函数已经有了（P1 Task 15 已合并），但注册必须与 `routes()` / `public_routes()` / `spawn()` 的接线落在同一个 commit——T1 就注册的话，T1…T12 期间守护进程会带着一个既没路由也没后台任务的模块跑；而且 T13 还要把 P1 那条六模块断言改成子集断言（裁决 D14），两处同一个文件放一起改才不打架

**前置**：①P1 的 Task 4 与 Task 13 已合并到 `v4`（`Module` / `AppState` / `api::router()` 存在；2026-09-12 核对 `v4` = `eb10eb1`，P1 Task 1–16 都已合并）；②**Task 0 已合并**——`Module::public_routes()` 的默认方法在，`PanelModule` 才能实现它（裁决 D1 已在总纲「裁决记录」批准）。本任务**不需要** P1 Task 15，也不碰 P1 的任何文件。

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Module, RenderCtx, DaemonCtx, Facts}`、`crate::api::{AppState, EventBus}`、`crate::api::auth::{issue_token, LoginLimiter}`、`crate::state::store::Store`、`crate::state::runtime::Runtime`、`crate::sys::{Host, fake::FakeHost}`、`crate::paths::{state_file, runtime_file, auth_snapshot_file}`、`crate::testutil::sample_state`、`bui_schema::paths::Paths`
- Produces:
```rust
// crate::modules::panel
pub const MODULE_NAME: &str = "panel";
/// 两个 hysteria 的 trafficStats 监听端口（由 bui_schema::render::hysteria 写死：直连 9999、住宅 9998）
pub const HY2_STATS_PORT_DIRECT: u16 = 9999;
pub const HY2_STATS_PORT_RESI: u16 = 9998;
/// bui-schema 渲染的 `trafficStats.secret` 是空串 ⇒ 不发 Authorization 头
/// （调研 H13：有 secret 时头值**精确等于** secret，无 `Bearer ` 前缀）
pub const HY2_STATS_SECRET: &str = "";
/// Xray 的 api inbound（bui_schema::render::xray 的 API_PORT = 10085）
pub const XRAY_API_ADDR: &str = "127.0.0.1:10085";
/// 两个 REALITY inbound 的 tag（bui_schema::render::xray 的 vless_inbound 调用处）
pub const XRAY_INBOUND_TAGS: [&str; 2] = ["vless-direct", "vless-residential"];
/// VLESS Account.flow（bui_schema::render::xray::clients 里同值）
pub const VLESS_FLOW: &str = "xtls-rprx-vision";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxRx { pub tx: u64, pub rx: u64 }
impl TxRx { pub fn total(&self) -> u64; pub fn add(&mut self, other: TxRx); pub fn is_zero(&self) -> bool; }

/// `/api/stats` 与 `/api/online` 读的同一份缓存（spec §4.2「读同一份缓存；面板开着不增加采样」）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SampleCache {
    pub stats: BTreeMap<String, TxRx>,        // 用户名 → 守护进程启动以来的累计增量
    pub online: BTreeMap<String, u32>,        // 用户名 → 当前连接数
    pub last_sample_at: Option<String>,
    /// 上次把内存增量并进 `state.json` 的时刻（Task 7 的 `tick` 用它判「到点该落盘了没」）
    pub last_flush_at: Option<time::OffsetDateTime>,
    pub errors: Vec<String>,                  // 本轮采样错误（已脱敏）
}

/// 已经同步到内核的状态（只在进程内，用来做差分）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub snapshot_sha: Option<String>,
    pub xray_users: BTreeMap<Uuid, Uuid>,     // 已 AddUser 的 user_id → vless_uuid
    pub xray_restarts: Option<String>,        // 上次同步时 xray 的 NRestarts
    /// 已经 RemoveUser 成功的用户（被封 / 到期 / 禁用）。被封用户要**无条件**删（决策 D6），
    /// 不能靠 `xray_users` 判「该不该删」，所以单独记一笔用来去重；
    /// `xray_restarts` 变化时与 `xray_users` 一起清空重放。
    pub xray_removed: BTreeSet<Uuid>,
    pub blocked: BTreeSet<Uuid>,              // 上次判定为拒绝的用户
}

#[async_trait::async_trait]
pub trait XrayApi: Send + Sync {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()>;
    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()>;
    /// `QueryStats(pattern="user>>>", reset=true)`：email（= `user_id` 的字符串）→ 本轮增量
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>>;
}

#[async_trait::async_trait]
pub trait Hy2Api: Send + Sync {
    /// `GET /traffic?clear=1`：`user_id` → 本轮增量（`clear` 与读取在同一把锁里，H10）
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>>;
    /// `GET /online`：`user_id` → 当前 QUIC 连接数（H11）
    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>>;
    /// `POST /kick`，体是 JSON 字符串数组（H12）
    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()>;
}

pub struct Shared { /* 私有字段见实现 */ }
impl Shared {
    pub fn new(xray: Box<dyn XrayApi>, hy2: Box<dyn Hy2Api>) -> Self;
    pub fn paths(&self) -> Paths;                     // 未 set 过就回落 Paths::default_server()
    pub fn set_paths(&self, p: &Paths);               // OnceLock，render() 与 spawn() 各调一次
    pub fn snapshot_path(&self) -> PathBuf;           // <base>/auth-snapshot.json
    pub fn packages_dir(&self) -> PathBuf;            // <base>/packages
    pub fn xray(&self) -> &dyn XrayApi;
    pub fn hy2(&self) -> &dyn Hy2Api;
    pub async fn cache(&self) -> tokio::sync::RwLockReadGuard<'_, SampleCache>;
    pub async fn cache_mut(&self) -> tokio::sync::RwLockWriteGuard<'_, SampleCache>;
    pub async fn applied(&self) -> tokio::sync::MutexGuard<'_, Applied>;
    /// 同步反应器的「轮次锁」：与 `applied` 分开，让 `sync_users` 在 2×N 次 gRPC await
    /// 期间不抱着 `applied`（否则 xray 掉线时 `write_health_summary` 会跟着卡 5 秒 × N，
    /// `/api/users/health` 随之卡住）。同一时刻只有一轮同步在跑。
    pub async fn sync_guard(&self) -> tokio::sync::MutexGuard<'_, ()>;
    pub async fn pending(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, TxRx>>;
    pub async fn xray_seen(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, time::OffsetDateTime>>;
}

pub struct PanelModule { /* Arc<Shared> */ }
impl PanelModule {
    pub fn new() -> Self;                             // 生产：XrayClient + Hy2Client
    pub fn with_shared(shared: Arc<Shared>) -> Self;
    pub fn shared(&self) -> Arc<Shared>;
}
impl Module for PanelModule {
    fn name(&self) -> &'static str;                    // "panel"
    fn render(&self, _s: &State, ctx: &RenderCtx) -> Vec<Artifact>;      // 只 set_paths，返回空 Vec
    fn routes(&self) -> axum::Router<AppState>;        // Task 13 接线
    fn public_routes(&self) -> axum::Router<AppState>; // Task 13 接线
    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>>;  // Task 13 接线
}
// crate::modules::panel::xray（Task 1 空壳，Task 4 填实）
pub struct XrayClient { /* addr */ }
impl XrayClient { pub fn new() -> Self; pub fn with_addr(addr: impl Into<String>) -> Self; pub fn addr(&self) -> &str; }
// crate::modules::panel::hy2（Task 1 空壳，Task 5 填实）
pub struct Hy2Client { /* base + secret */ }
impl Hy2Client { pub fn new() -> Self; pub fn with_base(base: impl Into<String>) -> Self;
                 pub fn url(&self, port: u16, path: &str) -> String; pub fn secret(&self) -> &str; }
// crate::modules::panel::fakes（#[cfg(test)]）
#[derive(Clone, Default)] pub struct FakeXray(/* Arc<Mutex<FakeXrayInner>> */);
#[derive(Default)] pub struct FakeXrayInner {
    pub calls: Vec<String>,                 // "add:<tag>:<user_id>:<uuid>" / "remove:<tag>:<user_id>" / "query"
    pub deltas: BTreeMap<String, TxRx>,     // query_user_deltas 的下一次返回值（返回后清空）
    pub fail_on: BTreeSet<String>,          // 命中就返回 Err，用来测退路
    pub error_text: BTreeMap<String, String>, // 指定某次失败的错误串（键同 fail_on）；
                                            // 不给就是默认串。xray 的「已存在 / 不存在」容错靠它测
}
impl FakeXray { pub fn new() -> Self; pub fn with(&self, f: impl FnOnce(&mut FakeXrayInner)) -> &Self;
                pub fn calls(&self) -> Vec<String>; pub fn clear_calls(&self); }
#[derive(Clone, Default)] pub struct FakeHy2(/* Arc<Mutex<FakeHy2Inner>> */);
#[derive(Default)] pub struct FakeHy2Inner {
    pub calls: Vec<String>,                              // "traffic:<port>" / "online:<port>" / "kick:<port>:<ids 逗号连接>"
    pub traffic: BTreeMap<u16, BTreeMap<String, TxRx>>,  // 端口 → 下一次 /traffic 的返回值（返回后清空）
    pub online: BTreeMap<u16, BTreeMap<String, u32>>,
    pub fail_ports: BTreeSet<u16>,
}
impl FakeHy2 { pub fn new() -> Self; pub fn with(&self, f: impl FnOnce(&mut FakeHy2Inner)) -> &Self;
               pub fn calls(&self) -> Vec<String>; pub fn clear_calls(&self); }
// crate::modules::panel::testsupport（#[cfg(test)]）
pub struct Harness {
    pub dir: tempfile::TempDir, pub paths: Paths, pub host: Arc<FakeHost>,
    pub store: Store, pub runtime: Runtime, pub app: AppState,
    pub shared: Arc<Shared>, pub xray: FakeXray, pub hy2: FakeHy2,
}
impl Harness { pub fn router(&self) -> axum::Router; }     // 整套装配，只挂 PanelModule。**只给 T13 用**
pub async fn harness() -> Harness;
pub async fn token(h: &Harness) -> String;
pub fn mount(app: &AppState, r: axum::Router<AppState>) -> axum::Router;   // 只挂子路由，不套 require_admin
pub fn full(app: &AppState, m: Arc<dyn Module>) -> axum::Router;           // 整套装配（含鉴权与公开路由）
/// T1…T12 里凡是要验「401 / 无鉴权可达」的测试都用这个：把**自己的**子路由挂成一套完整 Router。
/// 原因见下方「为什么 T1…T12 不能用 `Harness::router()`」。
pub fn full_with(app: &AppState, routes: axum::Router<AppState>, public: axum::Router<AppState>) -> axum::Router;
pub async fn raw(router: &axum::Router, method: &str, uri: &str, tok: Option<&str>, body: Option<serde_json::Value>)
    -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>);
pub async fn send(router: &axum::Router, method: &str, uri: &str, tok: Option<&str>, body: Option<serde_json::Value>)
    -> (axum::http::StatusCode, serde_json::Value);
pub async fn text(router: &axum::Router, uri: &str) -> (axum::http::StatusCode, String);
```

**为什么 T1…T12 不能用 `Harness::router()`（否则整套路由测试必挂）**：`Harness::router()` = `api::router(app, [PanelModule])`，而 `PanelModule::routes()` / `public_routes()` 在 T1 里写的是空 `Router::new()`，**要到 T13 才接线**。axum 的 `protected.layer(require_admin)` 只包裹**已经挂上的**路由；空 Router 套上中间件再 `merge` 进外层，未命中的路径走的是外层默认 fallback ⇒ 返回 **404，不是 401 / 200**。所以 T8…T12 里「整套路由」的那几条测试一律用 `full_with(&h.app, 自己的 routes, 自己的 public_routes)`，各自只挂自己那棵子树（各任务都只改自己的文件，并行合并仍然零冲突）；`h.router()` 只留给 T13 的收口测试。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/mod.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::testsupport::{full, harness, send};
    use super::*;
    use crate::reconcile::{Facts, Module, RenderCtx};
    use pretty_assertions::assert_eq;

    /// 一个只为测「公开路由不过鉴权、受保护路由过鉴权」而存在的模块（与 T0 的回归锁互补：
    /// T0 锁的是 P1 `api::router()` 的装配，这一条锁的是 `testsupport::full` 与生产装配一致）
    struct ProbeModule;
    impl Module for ProbeModule {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn render(&self, _s: &State, _c: &RenderCtx) -> Vec<Artifact> {
            Vec::new()
        }
        fn routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/admin", axum::routing::get(|| async { "admin" }))
        }
        fn public_routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/open", axum::routing::get(|| async { "open" }))
        }
    }

    fn ctx(paths: &Paths) -> RenderCtx {
        RenderCtx {
            paths: paths.clone(),
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
            },
        }
    }

    #[tokio::test]
    async fn module_is_named_panel() {
        let h = harness().await;
        assert_eq!(PanelModule::with_shared(h.shared.clone()).name(), MODULE_NAME);
        assert_eq!(MODULE_NAME, "panel");
    }

    #[tokio::test]
    async fn render_produces_nothing_so_core_files_stays_the_only_renderer() {
        // 渲染边界：xray-config.json 的 clients 由 P1 Task 10 的 core_files 渲染，
        // auth-snapshot.json 不是 artifact。这里多产出一个同路径 artifact 就会让
        // 「二次对账零变更」永久失效（见本计划「渲染边界」一节）。
        let h = harness().await;
        let m = PanelModule::with_shared(h.shared.clone());
        let arts = m.render(&h.store.read().await, &ctx(&h.paths));
        assert_eq!(arts, Vec::new());
        // render 顺带把 paths 交给 Shared（handler 要用 <base>/packages）
        assert_eq!(h.shared.paths().base_dir, h.paths.base_dir);
    }

    #[tokio::test]
    async fn public_routes_skip_require_admin_and_protected_routes_do_not() {
        let h = harness().await;
        let router = full(&h.app, std::sync::Arc::new(ProbeModule));
        let (open, _) = send(&router, "GET", "/probe/open", None, None).await;
        assert_eq!(open, axum::http::StatusCode::OK, "公开路由必须无 token 可达");
        let (denied, body) = send(&router, "GET", "/probe/admin", None, None).await;
        assert_eq!(denied, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Unauthorized");
        let tok = super::testsupport::token(&h).await;
        let (allowed, _) = send(&router, "GET", "/probe/admin", Some(&tok), None).await;
        assert_eq!(allowed, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn shared_paths_default_to_the_server_layout_and_are_set_once() {
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        );
        assert_eq!(s.paths().base_dir, PathBuf::from("/opt/b-ui"));
        let p = Paths {
            base_dir: "/tmp/x".into(),
            certs_dir: "/tmp/x/certs".into(),
            bin_dir: "/tmp/x/bin".into(),
        };
        s.set_paths(&p);
        assert_eq!(s.snapshot_path(), PathBuf::from("/tmp/x/auth-snapshot.json"));
        assert_eq!(s.packages_dir(), PathBuf::from("/tmp/x/packages"));
        // OnceLock：第二次 set 不生效，也不 panic
        s.set_paths(&Paths::default_server());
        assert_eq!(s.paths().base_dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn txrx_saturates_and_reports_totals() {
        let mut a = TxRx { tx: 3, rx: 4 };
        assert_eq!(a.total(), 7);
        a.add(TxRx { tx: u64::MAX, rx: 1 });
        assert_eq!(a.tx, u64::MAX, "饱和加，不 panic 也不回绕");
        assert!(!a.is_zero());
        assert!(TxRx::default().is_zero());
    }

    /// 空壳只为让 `PanelModule::new()` 从 Task 1 起就能编译；它们必须报错而不是假装成功。
    ///
    /// ⚠️ **T4 / T5 把两个客户端填实之后，这条测试仍然要绿，所以它只允许拨死端口
    /// `127.0.0.1:1`**（本机必然连不上，符合本计划铁律）：
    /// - `XrayApi::query_user_deltas` 的 `QueryStats` 带 `reset=true`，打到真实的
    ///   `127.0.0.1:10085` 会把线上 Xray 的用户计数器**原子清零** ——
    ///   在跑着内核的机器上执行 `cargo test` 就等于把流量账清零；
    /// - `Hy2Api::online` 打到真实的 9999 在有内核的机器上会**成功**，`is_err()` 当场失败。
    ///
    /// T4 / T5 的实现者：不要把下面两次真实调用改成 `XRAY_API_ADDR` /
    /// `HY2_STATS_PORT_DIRECT`，也不要因为「空壳已经填实了」就删掉这条测试。
    #[tokio::test]
    async fn the_clients_report_errors_instead_of_pretending_and_only_dial_a_dead_port() {
        // 默认地址只做纯字符串断言，不发起任何连接
        let c = super::xray::XrayClient::new();
        assert_eq!(c.addr(), XRAY_API_ADDR);
        let k = super::hy2::Hy2Client::new();
        assert_eq!(k.url(HY2_STATS_PORT_DIRECT, "/online"), "http://127.0.0.1:9999/online");
        // 真正发起的两次调用都只拨 127.0.0.1:1
        let dead_xray = super::xray::XrayClient::with_addr("127.0.0.1:1");
        assert!(XrayApi::query_user_deltas(&dead_xray).await.is_err());
        assert!(Hy2Api::online(&k, 1).await.is_err(), "端口 1 上没人听");
    }
}
```

`crates/bui/src/modules/panel/fakes.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::{Hy2Api, TxRx, XrayApi};
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    #[tokio::test]
    async fn fake_xray_records_calls_and_drains_deltas() {
        let x = FakeXray::new();
        x.with(|i| {
            i.deltas.insert("u-1".into(), TxRx { tx: 10, rx: 20 });
        });
        let uid = Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa").unwrap();
        let vid = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        x.add_user("vless-direct", uid, vid).await.unwrap();
        x.remove_user("vless-residential", uid).await.unwrap();
        assert_eq!(x.query_user_deltas().await.unwrap().len(), 1);
        assert!(x.query_user_deltas().await.unwrap().is_empty(), "增量取走即清空（reset=true 语义）");
        assert_eq!(
            x.calls(),
            vec![
                format!("add:vless-direct:{uid}:{vid}"),
                format!("remove:vless-residential:{uid}"),
                "query".to_string(),
                "query".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn fake_xray_can_be_made_to_fail_one_call_with_a_chosen_message() {
        let x = FakeXray::new();
        let uid = Uuid::nil();
        let key = format!("remove:vless-direct:{uid}");
        x.with(|i| {
            i.fail_on.insert(key.clone());
        });
        let default_err = x.remove_user("vless-direct", uid).await.unwrap_err().to_string();
        assert!(default_err.contains("fake RemoveUser"), "默认错误串：{default_err}");
        assert!(x.remove_user("vless-residential", uid).await.is_ok());
        // 错误串可配：Task 6 要靠它测 xray 的「已存在 / 不存在」容错（那两种错误按成功处理）
        x.with(|i| {
            i.error_text.insert(key, "User 0 not found.".into());
        });
        assert_eq!(
            x.remove_user("vless-direct", uid).await.unwrap_err().to_string(),
            "User 0 not found."
        );
    }

    #[tokio::test]
    async fn fake_hy2_serves_per_port_replies_and_records_kicks() {
        let k = FakeHy2::new();
        k.with(|i| {
            i.traffic.insert(9999, BTreeMap::from([("u-1".to_string(), TxRx { tx: 1, rx: 2 })]));
            i.online.insert(9998, BTreeMap::from([("u-1".to_string(), 3u32)]));
            i.fail_ports.insert(1234);
        });
        assert_eq!(k.traffic_clear(9999).await.unwrap().len(), 1);
        assert!(k.traffic_clear(9999).await.unwrap().is_empty(), "clear=1 语义：取走即清空");
        assert_eq!(k.online(9998).await.unwrap()["u-1"], 3);
        assert!(k.online(9999).await.unwrap().is_empty(), "没播种的端口返回空表，不报错");
        assert!(k.traffic_clear(1234).await.is_err());
        k.kick(9999, &["u-1".to_string(), "u-2".to_string()]).await.unwrap();
        assert!(k.calls().contains(&"kick:9999:u-1,u-2".to_string()));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::`
Expected: 编译失败，`error[E0433]: failed to resolve: could not find 'panel' in 'modules'`。

- [ ] **Step 3: Cargo.toml 追加依赖**

P2 的全部依赖**在这一步一次加齐**（T4 的 tonic 三件套与 T11 的 rust-embed 也在内）：T1 / T4 / T11 是三个并行分支，各改一次 `Cargo.toml` 就等于 `Cargo.lock` 三方冲突。`[build-dependencies]` 这一节现在还没有 `build.rs`（T4 才建）——**没有 `build.rs` 时它只参与依赖解析，不参与编译**，所以现在加进来完全无害。

在 `crates/bui/Cargo.toml` 的 `[dependencies]` **末尾**追加（不动已有行）：
```toml
# P2：可注入的 gRPC / hysteria HTTP trait 要 `dyn` 化，原生 async fn in trait 不是对象安全的
async-trait = "0.1"
# P2：auth-hook 的密码比对走常量时间（spec §3.2）
subtle = "2"
# P2 Task 4：Xray gRPC（只做客户端、只连 127.0.0.1 明文，不需要 TLS）
tonic = { version = "0.14", default-features = false, features = ["codegen", "transport"] }
tonic-prost = "0.14"
prost = "0.14"
# P2 Task 11：把 web/ 的五个前端文件与 scripts/bui-c-install.sh 编进二进制。
# 三个 feature 都是必需的：interpolate-folder-path 展开 $CARGO_MANIFEST_DIR、
# include-exclude 启用 #[include]、debug-embed 让 debug 构建也真嵌入（默认是运行时读盘）。
rust-embed = { version = "8", features = ["debug-embed", "include-exclude", "interpolate-folder-path"] }
```
文件**末尾**新增一节（本机没有系统 `protoc`，CI 也不该装，所以用 vendored 的；Task 4 的 `build.rs` 会用到）：
```toml
[build-dependencies]
tonic-prost-build = "0.14"
# 提供构建机自己架构的 protoc 二进制（纯 crates.io，不依赖系统包）；
# 交叉编译到 aarch64-musl 时它仍然给 host 的 protoc，正是需要的那个
protoc-bin-vendored = "3"
```

- [ ] **Step 4: `modules/mod.rs` 一行**

`crates/bui/src/modules/mod.rs`（按字母序插在 `core_files` 与 `ssh` 之间）：
```rust
pub mod panel;
```

**不要碰 `serve.rs`**：`v4` = `eb10eb1` 上 `modules()` 已经在（P1 Task 15 已合并），但注册那一行、它的回归测试 `the_panel_module_is_registered_in_serve_modules`、以及把 P1 那条六模块断言改成子集断言（裁决 D14）**全部归 T13**——三者必须与 `routes()` / `public_routes()` / `spawn()` 的接线落在同一个 commit，否则 T1…T12 期间守护进程会注册一个空模块，且 P1 的 `serve::tests` 会红一路。

- [ ] **Step 5: 写 `panel/mod.rs`**

```rust
//! P2 面板与用户域：管理员 API、订阅与节点端点、嵌入前端、`/packages`、
//! 采样与限额、`auth-snapshot.json`、Xray gRPC、`bui auth-hook`。
//!
//! 对 P1 只暴露一个 [`PanelModule`]（`reconcile::Module` 的实现）。

pub mod api_admin;
pub mod api_me;
pub mod api_public;
pub mod assets;
pub mod auth_hook;
pub mod hy2;
pub mod packages;
pub mod snapshot;
pub mod traffic;
pub mod users;
pub mod xray;

#[cfg(test)]
pub mod fakes;
#[cfg(test)]
pub mod testsupport;

use crate::api::AppState;
use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MODULE_NAME: &str = "panel";
pub const HY2_STATS_PORT_DIRECT: u16 = 9999;
pub const HY2_STATS_PORT_RESI: u16 = 9998;
/// `bui_schema::render::hysteria` 渲染的 `trafficStats.secret` 是空串 ⇒ 不发 Authorization 头。
/// 若将来改成非空，`hy2::Hy2Client` 按调研 H13 发 `Authorization: <secret>`（**无** `Bearer ` 前缀）。
pub const HY2_STATS_SECRET: &str = "";
pub const XRAY_API_ADDR: &str = "127.0.0.1:10085";
pub const XRAY_INBOUND_TAGS: [&str; 2] = ["vless-direct", "vless-residential"];
pub const VLESS_FLOW: &str = "xtls-rprx-vision";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxRx {
    pub tx: u64,
    pub rx: u64,
}

impl TxRx {
    pub fn total(&self) -> u64 {
        self.tx.saturating_add(self.rx)
    }

    pub fn add(&mut self, other: TxRx) {
        self.tx = self.tx.saturating_add(other.tx);
        self.rx = self.rx.saturating_add(other.rx);
    }

    pub fn is_zero(&self) -> bool {
        self.tx == 0 && self.rx == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SampleCache {
    pub stats: BTreeMap<String, TxRx>,
    pub online: BTreeMap<String, u32>,
    pub last_sample_at: Option<String>,
    /// 上次把内存增量并进 `state.json` 的时刻（Task 7 的 `tick` 用它判「到点该落盘了没」）
    pub last_flush_at: Option<OffsetDateTime>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub snapshot_sha: Option<String>,
    pub xray_users: BTreeMap<Uuid, Uuid>,
    pub xray_restarts: Option<String>,
    /// 已经 RemoveUser 成功的用户（被封 / 到期 / 禁用）。被封用户要**无条件**删（决策 D6），
    /// 不能靠 `xray_users` 判「该不该删」——守护进程重启后 `xray_users` 是空的，
    /// 而 `xray-config.json` 里还带着他们的凭据。`xray_restarts` 变化时与 `xray_users` 一起清空。
    pub xray_removed: BTreeSet<Uuid>,
    pub blocked: BTreeSet<Uuid>,
}

#[async_trait::async_trait]
pub trait XrayApi: Send + Sync {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()>;
    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()>;
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>>;
}

#[async_trait::async_trait]
pub trait Hy2Api: Send + Sync {
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>>;
    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>>;
    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()>;
}

pub struct Shared {
    paths: OnceLock<Paths>,
    cache: tokio::sync::RwLock<SampleCache>,
    applied: tokio::sync::Mutex<Applied>,
    /// 同步反应器的轮次锁（见 `sync_guard`）
    sync: tokio::sync::Mutex<()>,
    pending: tokio::sync::Mutex<BTreeMap<Uuid, TxRx>>,
    xray_seen: tokio::sync::Mutex<BTreeMap<Uuid, OffsetDateTime>>,
    xray: Box<dyn XrayApi>,
    hy2: Box<dyn Hy2Api>,
}

impl Shared {
    pub fn new(xray: Box<dyn XrayApi>, hy2: Box<dyn Hy2Api>) -> Self {
        Self {
            paths: OnceLock::new(),
            cache: tokio::sync::RwLock::new(SampleCache::default()),
            applied: tokio::sync::Mutex::new(Applied::default()),
            sync: tokio::sync::Mutex::new(()),
            pending: tokio::sync::Mutex::new(BTreeMap::new()),
            xray_seen: tokio::sync::Mutex::new(BTreeMap::new()),
            xray,
            hy2,
        }
    }

    /// `AppState` 里没有 `paths`，而 `/packages/*` 与快照重写都要它；`render()` 与 `spawn()`
    /// 各调一次 `set_paths`，两者都跑在处理第一个请求之前（P1 的 `serve::run` 先对账、
    /// 再起后台任务、最后才 listen）。
    pub fn paths(&self) -> Paths {
        self.paths.get().cloned().unwrap_or_else(Paths::default_server)
    }

    pub fn set_paths(&self, p: &Paths) {
        let _ = self.paths.set(p.clone());
    }

    pub fn snapshot_path(&self) -> PathBuf {
        crate::paths::auth_snapshot_file(&self.paths())
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.paths().base_dir.join("packages")
    }

    pub fn xray(&self) -> &dyn XrayApi {
        self.xray.as_ref()
    }

    pub fn hy2(&self) -> &dyn Hy2Api {
        self.hy2.as_ref()
    }

    pub async fn cache(&self) -> tokio::sync::RwLockReadGuard<'_, SampleCache> {
        self.cache.read().await
    }

    pub async fn cache_mut(&self) -> tokio::sync::RwLockWriteGuard<'_, SampleCache> {
        self.cache.write().await
    }

    pub async fn applied(&self) -> tokio::sync::MutexGuard<'_, Applied> {
        self.applied.lock().await
    }

    /// 同步反应器的轮次锁：`users::sync_users` 全程持有它（采样任务与事件反应器都会调它，
    /// 同一时刻只许一轮），但**不**全程持有 `applied`——`applied` 只在算差分与写回记账时
    /// 短暂加锁。否则 2×N 次 gRPC（每次最长 5 秒）都抱着 `applied`，xray 掉线时
    /// `traffic::write_health_summary` 会跟着卡住，`/api/users/health` 一起卡。
    pub async fn sync_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.sync.lock().await
    }

    pub async fn pending(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, TxRx>> {
        self.pending.lock().await
    }

    pub async fn xray_seen(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, OffsetDateTime>> {
        self.xray_seen.lock().await
    }
}

pub struct PanelModule {
    shared: Arc<Shared>,
}

impl PanelModule {
    pub fn new() -> Self {
        Self::with_shared(Arc::new(Shared::new(
            Box::new(xray::XrayClient::new()),
            Box::new(hy2::Hy2Client::new()),
        )))
    }

    pub fn with_shared(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}

impl Default for PanelModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PanelModule {
    fn name(&self) -> &'static str {
        MODULE_NAME
    }

    /// **空**。见本计划「渲染边界」一节：`xray-config.json` 的 `clients` 归 P1 Task 10 的
    /// `core_files`（`structural_hash` 排除 `clients` ⇒ 增删用户不重启 xray），
    /// `auth-snapshot.json` 不是 artifact（它在 P1 的 `BASE_WHITELIST` 里），由本模块原子重写。
    fn render(&self, _s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        self.shared.set_paths(&ctx.paths);
        Vec::new()
    }

    fn routes(&self) -> axum::Router<AppState> {
        // Task 13 接线
        axum::Router::new()
    }

    fn public_routes(&self) -> axum::Router<AppState> {
        // Task 13 接线
        axum::Router::new()
    }

    fn spawn(&self, _ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        // Task 13 接线
        Vec::new()
    }
}
```

- [ ] **Step 6: 写 `panel/xray.rs` 与 `panel/hy2.rs` 的空壳**

`crates/bui/src/modules/panel/xray.rs`：
```rust
//! Xray gRPC 客户端。**Task 4 填实**（tonic + vendored proto）。
//!
//! Task 1 只给 `XrayClient` 的空壳，好让 `PanelModule::new()` 从 Task 1 起就能编译、
//! 且它的签名此后不再变动；空壳的每个方法都返回 `Err`（绝不假装成功）。

use super::{TxRx, XrayApi};
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct XrayClient {
    addr: String,
}

impl XrayClient {
    pub fn new() -> Self {
        Self::with_addr(super::XRAY_API_ADDR)
    }

    pub fn with_addr(addr: impl Into<String>) -> Self {
        Self { addr: addr.into() }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }
}

impl Default for XrayClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl XrayApi for XrayClient {
    async fn add_user(&self, _tag: &str, _user_id: Uuid, _vless_uuid: Uuid) -> anyhow::Result<()> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }

    async fn remove_user(&self, _tag: &str, _user_id: Uuid) -> anyhow::Result<()> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        anyhow::bail!("Xray gRPC 由 P2 Task 4 实现")
    }
}
```

`crates/bui/src/modules/panel/hy2.rs`：
```rust
//! 两个 hysteria 的 trafficStats HTTP API 客户端。**Task 5 填实**（reqwest 异步）。

use super::{Hy2Api, TxRx};
use std::collections::BTreeMap;

pub struct Hy2Client {
    base: String,
    secret: String,
}

impl Hy2Client {
    pub fn new() -> Self {
        Self::with_base("http://127.0.0.1")
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self { base: base.into(), secret: super::HY2_STATS_SECRET.to_string() }
    }

    pub fn url(&self, port: u16, path: &str) -> String {
        format!("{}:{}{}", self.base.trim_end_matches('/'), port, path)
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl Default for Hy2Client {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hy2Api for Hy2Client {
    async fn traffic_clear(&self, _port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }

    async fn online(&self, _port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }

    async fn kick(&self, _port: u16, _ids: &[String]) -> anyhow::Result<()> {
        anyhow::bail!("hysteria trafficStats 客户端由 P2 Task 5 实现")
    }
}
```

- [ ] **Step 7: 写 `panel/fakes.rs`**

```rust
//! 测试用的内存 fake（`#[cfg(test)]`）：把 gRPC 与 hysteria HTTP 全挡在进程内。

use super::{Hy2Api, TxRx, XrayApi};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct FakeXrayInner {
    pub calls: Vec<String>,
    pub deltas: BTreeMap<String, TxRx>,
    pub fail_on: BTreeSet<String>,
    /// 指定某次失败的错误串（键同 `fail_on`），不给就用默认串。
    /// 真实 xray 在「email 已存在」与「email 不存在」时都报错，而这两种错误 Task 6
    /// 按成功处理——错误串不可配就没法给那条容错写正向测试。
    pub error_text: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
pub struct FakeXray(Arc<Mutex<FakeXrayInner>>);

impl FakeXray {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeXrayInner)) -> &Self {
        f(&mut self.0.lock().unwrap());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.0.lock().unwrap().calls.clear();
    }
}

#[async_trait::async_trait]
impl XrayApi for FakeXray {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()> {
        let key = format!("add:{tag}:{user_id}:{vless_uuid}");
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake AddUser 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        Ok(())
    }

    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()> {
        let key = format!("remove:{tag}:{user_id}");
        let mut i = self.0.lock().unwrap();
        i.calls.push(key.clone());
        if i.fail_on.contains(&key) {
            let msg = i
                .error_text
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("fake RemoveUser 失败：{key}"));
            anyhow::bail!("{msg}");
        }
        Ok(())
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push("query".into());
        if i.fail_on.contains("query") {
            anyhow::bail!("fake QueryStats 失败");
        }
        Ok(std::mem::take(&mut i.deltas))
    }
}

#[derive(Default)]
pub struct FakeHy2Inner {
    pub calls: Vec<String>,
    pub traffic: BTreeMap<u16, BTreeMap<String, TxRx>>,
    pub online: BTreeMap<u16, BTreeMap<String, u32>>,
    pub fail_ports: BTreeSet<u16>,
}

#[derive(Clone, Default)]
pub struct FakeHy2(Arc<Mutex<FakeHy2Inner>>);

impl FakeHy2 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeHy2Inner)) -> &Self {
        f(&mut self.0.lock().unwrap());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.0.lock().unwrap().calls.clear();
    }
}

#[async_trait::async_trait]
impl Hy2Api for FakeHy2 {
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("traffic:{port}"));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /traffic 失败：{port}");
        }
        Ok(i.traffic.remove(&port).unwrap_or_default())
    }

    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("online:{port}"));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /online 失败：{port}");
        }
        Ok(i.online.get(&port).cloned().unwrap_or_default())
    }

    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()> {
        let mut i = self.0.lock().unwrap();
        i.calls.push(format!("kick:{port}:{}", ids.join(",")));
        if i.fail_ports.contains(&port) {
            anyhow::bail!("fake /kick 失败：{port}");
        }
        Ok(())
    }
}
```

- [ ] **Step 8: 写 `panel/testsupport.rs`**

```rust
//! P2 各任务共用的测试支架（`#[cfg(test)]`）：tempdir 上的一台假机器 + 一个 `AppState`。

use super::fakes::{FakeHy2, FakeXray};
use super::{PanelModule, Shared};
use crate::api::{AppState, EventBus};
use crate::reconcile::Module;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::fake::FakeHost;
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::sync::Arc;

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub paths: Paths,
    pub host: Arc<FakeHost>,
    pub store: Store,
    pub runtime: Runtime,
    pub app: AppState,
    pub shared: Arc<Shared>,
    pub xray: FakeXray,
    pub hy2: FakeHy2,
}

impl Harness {
    /// 只挂 P2 自己的模块的整套 Router（含 require_admin 与公开路由）。
    ///
    /// **只给 T13 用**：T1…T12 期间 `PanelModule::routes()` / `public_routes()` 还是空的，
    /// 这条路会让任何端点都返回 404。那些任务请用 [`full_with`]。
    pub fn router(&self) -> axum::Router {
        full(&self.app, Arc::new(PanelModule::with_shared(self.shared.clone())))
    }
}

pub async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let paths = Paths {
        base_dir: base.clone(),
        certs_dir: base.join("certs"),
        bin_dir: base.join("bin"),
    };
    let host = Arc::new(FakeHost::new());
    let store = Store::create(crate::paths::state_file(&paths), crate::testutil::sample_state())
        .await
        .unwrap();
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let (xray, hy2) = (FakeXray::new(), FakeHy2::new());
    let shared = Arc::new(Shared::new(Box::new(xray.clone()), Box::new(hy2.clone())));
    shared.set_paths(&paths);
    let app = AppState {
        store: store.clone(),
        bus: EventBus::new(),
        runtime: runtime.clone(),
        host: host.clone(),
        started_at: host.now(),
        version: env!("CARGO_PKG_VERSION"),
        login: crate::api::auth::LoginLimiter::default(),
    };
    Harness { dir, paths, host, store, runtime, app, shared, xray, hy2 }
}

/// 用 state 里的 `jwt_secret` 现签一个管理员 token
pub async fn token(h: &Harness) -> String {
    let secret = h.store.read().await.admin.jwt_secret.clone();
    crate::api::auth::issue_token(&secret, h.host.now()).unwrap().0
}

pub fn mount(app: &AppState, r: axum::Router<AppState>) -> axum::Router {
    r.with_state(app.clone())
}

pub fn full(app: &AppState, m: Arc<dyn Module>) -> axum::Router {
    crate::api::router(app.clone(), &[m])
}

/// 把**指定的**两棵子路由挂成一套完整 Router（`public` 在 `require_admin` 外面、
/// `routes` 在里面）。
///
/// T1…T12 期间 `PanelModule::routes()` / `public_routes()` 还是空的（T13 才接线），
/// 用 [`Harness::router`] 去验「401 / 无鉴权可达」会一律拿到 **404**（axum 的 `layer`
/// 只包裹已挂上的路由，未命中就走外层 fallback）。所以那些测试用本函数挂自己的子树。
pub fn full_with(
    app: &AppState,
    routes: axum::Router<AppState>,
    public: axum::Router<AppState>,
) -> axum::Router {
    struct Adhoc {
        routes: axum::Router<AppState>,
        public: axum::Router<AppState>,
    }
    impl Module for Adhoc {
        fn name(&self) -> &'static str {
            "adhoc"
        }

        fn render(
            &self,
            _s: &bui_schema::model::State,
            _c: &crate::reconcile::RenderCtx,
        ) -> Vec<crate::reconcile::Artifact> {
            Vec::new()
        }

        fn routes(&self) -> axum::Router<AppState> {
            self.routes.clone()
        }

        fn public_routes(&self) -> axum::Router<AppState> {
            self.public.clone()
        }
    }
    crate::api::router(app.clone(), &[Arc::new(Adhoc { routes, public })])
}

pub async fn raw(
    router: &axum::Router,
    method: &str,
    uri: &str,
    tok: Option<&str>,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
    use tower::ServiceExt;
    let mut b = axum::http::Request::builder().method(method).uri(uri);
    if let Some(t) = tok {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => b.body(axum::body::Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024).await.unwrap().to_vec();
    (status, headers, bytes)
}

pub async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    tok: Option<&str>,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let (status, _h, bytes) = raw(router, method, uri, tok, body).await;
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

pub async fn text(router: &axum::Router, uri: &str) -> (axum::http::StatusCode, String) {
    let (status, _h, bytes) = raw(router, "GET", uri, None, None).await;
    (status, String::from_utf8_lossy(&bytes).to_string())
}
```

- [ ] **Step 9: 建其余九个桩文件**

`crates/bui/src/modules/panel/{snapshot,auth_hook,users,traffic,api_admin,api_public,api_me,assets,packages}.rs`，每个只一行注释，例如：
```rust
//! placeholder filled by Task 2
```
对应关系：`snapshot.rs` → Task 2、`auth_hook.rs` → Task 3、`users.rs` → Task 6、`traffic.rs` → Task 7、`api_admin.rs` → Task 8、`api_public.rs` → Task 9、`api_me.rs` → Task 10、`assets.rs` → Task 11、`packages.rs` → Task 12。

- [ ] **Step 10: 运行测试**

Run: `cargo test -p bui panel:: && cargo test -p bui api:: && cargo clippy -p bui --all-targets -- -D warnings && cargo fmt --check`
Expected: `panel::` 9 passed（mod 6 + fakes 3）；`api::` 与 T0 之后完全一致（本任务不碰 `api/`）；clippy / fmt 无输出。

- [ ] **Step 11: Commit**

```
git add crates/bui/Cargo.toml crates/bui/src/modules Cargo.lock
git commit -m "feat(panel): P2 骨架与共享类型（桩文件 + Shared / XrayApi / Hy2Api + 测试支架）"
```

---

### Task 2: `auth-snapshot.json`（总纲 C5 形状）与原子重写

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/snapshot.rs`

**Interfaces:**
- Consumes: `bui_schema::model::{State, Protocol}`、`crate::state::store::write_atomic`、`crate::util::parse_rfc3339`
- Produces:
```rust
pub const SNAPSHOT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot { pub schema: u32, pub users: BTreeMap<String, SnapshotUser> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotUser {
    pub user_id: String,
    pub hy2_password: String,
    #[serde(default)] pub expires_at: Option<String>,
    #[serde(default)] pub blocked: bool,
}
impl Snapshot {
    pub fn empty() -> Self;
    /// 期望态 + 本轮判定的拒绝集合 → 快照。只收有 `hysteria2` 权益的用户。
    pub fn from_state(state: &State, blocked: &BTreeSet<Uuid>) -> Self;
    pub fn to_bytes(&self) -> Vec<u8>;      // pretty JSON + 末尾换行
    pub fn sha256(&self) -> String;         // to_bytes() 的 sha256 十六进制（64 字符）
}
/// 读快照；文件缺失或解析失败 → `Snapshot::empty()`（钩子那边靠 fail-closed 兜底，不靠这里）
pub fn read(path: &Path) -> Snapshot;
/// 原子重写：内容与磁盘一致就**不写**（返回 `Ok(false)`）。写 = tmp + 0600 + rename。
pub fn write_if_changed(path: &Path, snap: &Snapshot) -> anyhow::Result<bool>;
```

**为什么只收 `hysteria2` 权益的用户**：快照是 hysteria 鉴权钩子唯一的输入；只有 Reality 权益的用户建不了 QUIC 连接，放进去只会让文件变大，也会让「快照里有他 ⇒ 他能用 HY2」这条读法出错。P1 Task 16 的初版快照收的是全量，但那只影响装机当轮——本任务合并后守护进程一启动就同步一次（Task 13 接线），立刻收敛。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/snapshot.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;

    fn uid(s: &State, name: &str) -> Uuid {
        s.users.iter().find(|x| x.username == name).unwrap().user_id
    }

    #[test]
    fn shape_is_exactly_c5() {
        let s = sample_state();
        let snap = Snapshot::from_state(&s, &BTreeSet::new());
        let v: serde_json::Value = serde_json::from_slice(&snap.to_bytes()).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(
            v["users"]["alice"],
            serde_json::json!({
                "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa",
                "hy2_password": "pw-alice-01",
                "expires_at": serde_json::Value::Null,
                "blocked": false
            })
        );
        assert_eq!(v.as_object().unwrap().len(), 2, "顶层只有 schema 与 users");
    }

    #[test]
    fn empty_snapshot_is_schema_plus_empty_users_not_an_empty_object() {
        let v: serde_json::Value = serde_json::from_slice(&Snapshot::empty().to_bytes()).unwrap();
        assert_eq!(v, serde_json::json!({"schema": 1, "users": {}}));
    }

    #[test]
    fn blocked_and_expiry_come_from_the_caller_and_the_entitlements() {
        let mut s = sample_state();
        s.users[0].entitlements.expires_at = Some("2026-10-01T00:00:00Z".into());
        let a = uid(&s, "alice");
        let snap = Snapshot::from_state(&s, &BTreeSet::from([a]));
        let u = &snap.users["alice"];
        assert_eq!(u.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert!(u.blocked, "拒绝集合里的用户必须标 blocked");
        // disabled 自己也要生效，不能只依赖调用方把它放进 blocked 集合
        let mut s2 = sample_state();
        s2.users[0].disabled = true;
        assert!(Snapshot::from_state(&s2, &BTreeSet::new()).users["alice"].blocked);
    }

    #[test]
    fn reality_only_users_are_not_in_the_snapshot() {
        let mut s = sample_state();
        s.users[0].entitlements.protocols = vec![bui_schema::model::Protocol::Reality];
        assert!(Snapshot::from_state(&s, &BTreeSet::new()).users.is_empty());
    }

    #[test]
    fn writes_0600_atomically_and_skips_an_unchanged_write() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("auth-snapshot.json");
        let snap = Snapshot::from_state(&sample_state(), &BTreeSet::new());
        assert!(write_if_changed(&p, &snap).unwrap(), "第一次必须写");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!write_if_changed(&p, &snap).unwrap(), "内容不变不写");
        // 没有残留的 .tmp（write_atomic 的 rename 语义）
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["auth-snapshot.json".to_string()]);
        let mut s2 = sample_state();
        s2.users[0].credentials.hy2_password = "pw-alice-02".into();
        assert!(write_if_changed(&p, &Snapshot::from_state(&s2, &BTreeSet::new())).unwrap());
        assert_eq!(read(&p).users["alice"].hy2_password, "pw-alice-02");
    }

    #[test]
    fn reading_a_missing_or_broken_file_yields_an_empty_snapshot() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read(&d.path().join("nope.json")), Snapshot::empty());
        let bad = d.path().join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert_eq!(read(&bad), Snapshot::empty());
    }

    #[test]
    fn sha256_changes_with_content_and_is_stable_across_calls() {
        let a = Snapshot::from_state(&sample_state(), &BTreeSet::new());
        let mut s = sample_state();
        s.users[0].disabled = true;
        let b = Snapshot::from_state(&s, &BTreeSet::new());
        assert_eq!(a.sha256(), a.sha256());
        assert_ne!(a.sha256(), b.sha256());
        assert_eq!(a.sha256().len(), 64);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::snapshot`
Expected: 编译失败，`cannot find type 'Snapshot' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! `auth-snapshot.json`：hysteria 鉴权钩子（`bui auth-hook`）唯一读取的文件。
//!
//! 形状**逐字照总纲 C5**：顶层只有 `schema` 与 `users`，`users` 的键是用户名。
//! 它不是对账的 `Artifact`（P1 不比对它的内容，只把它放进 `BASE_WHITELIST`），
//! 由本模块在每次用户 / 限额变化时原子重写（tmp + 0600 + rename）。

use bui_schema::model::{Protocol, State};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

pub const SNAPSHOT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub users: BTreeMap<String, SnapshotUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotUser {
    pub user_id: String,
    pub hy2_password: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub blocked: bool,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self { schema: SNAPSHOT_SCHEMA, users: BTreeMap::new() }
    }

    pub fn from_state(state: &State, blocked: &BTreeSet<Uuid>) -> Self {
        let users = state
            .users
            .iter()
            .filter(|u| u.entitlements.protocols.contains(&Protocol::Hysteria2))
            .map(|u| {
                (
                    u.username.clone(),
                    SnapshotUser {
                        user_id: u.user_id.to_string(),
                        hy2_password: u.credentials.hy2_password.clone(),
                        expires_at: u.entitlements.expires_at.clone(),
                        blocked: u.disabled || blocked.contains(&u.user_id),
                    },
                )
            })
            .collect();
        Self { schema: SNAPSHOT_SCHEMA, users }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = serde_json::to_vec_pretty(self).expect("快照序列化不会失败");
        v.push(b'\n');
        v
    }

    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.to_bytes()))
    }
}

pub fn read(path: &Path) -> Snapshot {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Snapshot::empty()),
        Err(_) => Snapshot::empty(),
    }
}

pub fn write_if_changed(path: &Path, snap: &Snapshot) -> anyhow::Result<bool> {
    let bytes = snap.to_bytes();
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(false);
    }
    // 与 state.json / runtime.json 同一条落盘路径：tmp + 0600 + rename
    crate::state::store::write_atomic(path, &bytes)?;
    Ok(true)
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::snapshot && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 7 passed，clippy 无输出。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/snapshot.rs
git commit -m "feat(panel): auth-snapshot.json 形状（C5）与原子重写"
```

---

### Task 3: `bui auth-hook`（极简路径、≤2 秒硬超时、fail-closed、自写日志）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/auth_hook.rs`
- Modify: `crates/bui/src/main.rs`（`Command::AuthHook` 那一支，**决策 D3**；P1 Task 1 已预授权「P2 落地钩子时只替换那一支」）

**Interfaces:**
- Consumes: `crate::modules::panel::snapshot::{self, Snapshot}`、`crate::util::{parse_rfc3339, fmt_rfc3339}`、`bui_schema::paths::Paths`、`crate::paths::auth_snapshot_file`
- Produces:
```rust
/// 钩子自设的硬超时（spec §3.2：内核**不设**超时，调研 H8）
pub const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// 日志超限的阈值。超限时**原地截断**（只保留尾部 [`LOG_KEEP_BYTES`] 重写同一个文件），
/// **绝不产生 `auth-hook.log.1` 之类的兄弟文件** —— 理由见下面「为什么不轮转出兄弟文件」。
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;
/// 原地截断后保留的尾部字节数
pub const LOG_KEEP_BYTES: u64 = 256 * 1024;
pub fn log_path(paths: &Paths) -> PathBuf;                 // <base>/auth-hook.log（在 P1 的 BASE_WHITELIST 里）

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision { Allow { user_id: String }, Deny { reason: &'static str } }
/// 纯函数：解析 `auth`（按**第一个** `:` 拆分，H1）→ 查快照 → 常量时间比对密码 → 校 blocked 与期限
pub fn decide(snap: &Snapshot, auth: &str, now: time::OffsetDateTime) -> Decision;
/// 进程入口：`bui auth-hook <addr> <auth> <tx>`。返回**进程退出码**（0 = 放行），
/// 放行时先 `println!("{user_id}")`。任何异常（参数不全、读文件失败、超时）一律 Deny。
pub fn run(args: &[String]) -> i32;
/// `run` 的可测版本：显式给 base 目录与「现在」，不读进程环境、不打印
pub fn run_with(paths: &Paths, args: &[String], now: time::OffsetDateTime) -> (i32, Option<String>);
```

**这条路径的硬约束（spec §3.2 + 调研 H1–H9），实现时逐条对照：**
- **不初始化 tokio、不初始化 tracing、不加载 state**：本文件只用 `std::{fs, io, sync::mpsc, thread, time}`、`serde_json`、`subtle`、`time`。`main.rs` 里这一支在建 runtime、`logging::init` **之前**返回。
- 参数形态固定 `<addr> <auth> <tx>`；`auth` 是 `hysteria2://user:pass@` 里的 `user:pass` **原串**，按第一个 `:` 拆分（密码里可以再有 `:`）。
- 放行 = stdout 打印 `user_id` + 退出码 0；**该字符串就是 `/traffic`、`/online`、`/kick` 的键**（H4）。
- stderr 不会进 `hysteria-server` 的 journal（H5），所以诊断只写 `<base>/auth-hook.log`，**只记用户名与结果，不记密码**。
- 内核不设超时（H8）：读快照放在子线程里，主线程 `recv_timeout(HOOK_TIMEOUT)`，超时 Deny。
- 密码比对用 `subtle::ConstantTimeEq`。
- fail-closed：快照缺失 / 坏 JSON / 用户不存在 / 密码不符 / `blocked` / 已过期 / 参数不足 —— 全部退出码 1。
- 日志超限**原地截断**，不产生兄弟文件（见下条）。

**为什么不轮转出兄弟文件**（本计划第三轮审查改掉的一处真实缺陷，实现时不要改回 `rename`）：
P1 的 `crates/bui/src/reconcile/drift.rs` 里 `BASE_WHITELIST` 只有 13 项（2026-09-12 对真实代码核对），涉及本文件的只有 **`auth-hook.log`** 这一个名字；`TRANSIENT_SUFFIXES` 也只有 `.tmp` / `.new`。于是一旦钩子把超限日志 `rename` 成 `<base>/auth-hook.log.1`：
1. P1 每 10 分钟一次的漂移巡检会永久报一条 `stray_file`（`api/health.rs` 以 `!rt.drift.is_empty()` 判 **degraded**）⇒ `/api/health` 从此再也不 healthy；
2. `bui reconcile --force` 会走 `drift::clean` 把它**删掉**。
所以本任务的做法是 **(a) 原地截断**：超限时读出尾部 `LOG_KEEP_BYTES`、从下一个换行之后起、以 `truncate(true)` 重写**同一个文件**。`<base>` 顶层因此不会多出任何 P2 自己造的文件，不需要请 P1 改 `BASE_WHITELIST`（那是被否掉的备选 (b)）。测试 `the_log_is_0600_and_is_truncated_in_place_when_oversized` 用「顶层文件名集合」锁住这一点。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/auth_hook.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::snapshot::{self, Snapshot, SnapshotUser};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn paths(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            schema: 1,
            users: BTreeMap::from([
                (
                    "alice".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into(),
                        hy2_password: "pw:with:colons".into(),
                        expires_at: None,
                        blocked: false,
                    },
                ),
                (
                    "bob".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb".into(),
                        hy2_password: "pw-bob".into(),
                        expires_at: Some("2026-09-10T00:00:00Z".into()),
                        blocked: false,
                    },
                ),
                (
                    "carol".to_string(),
                    SnapshotUser {
                        user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000cc".into(),
                        hy2_password: "pw-carol".into(),
                        expires_at: None,
                        blocked: true,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn allows_a_correct_password_and_returns_the_user_id() {
        assert_eq!(
            decide(&snap(), "alice:pw:with:colons", t0()),
            Decision::Allow { user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".into() },
            "auth 按第一个冒号拆分，密码里允许再有冒号"
        );
    }

    #[test]
    fn denies_wrong_password_unknown_user_blocked_and_expired() {
        assert_eq!(decide(&snap(), "alice:nope", t0()), Decision::Deny { reason: "bad-password" });
        assert_eq!(decide(&snap(), "dave:x", t0()), Decision::Deny { reason: "no-such-user" });
        assert_eq!(decide(&snap(), "carol:pw-carol", t0()), Decision::Deny { reason: "blocked" });
        assert_eq!(decide(&snap(), "bob:pw-bob", t0()), Decision::Deny { reason: "expired" });
        assert_eq!(
            decide(&snap(), "bob:pw-bob", datetime!(2026-09-09 23:59:59 UTC)),
            Decision::Allow { user_id: "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb".into() },
            "到期时间在未来就放行"
        );
    }

    #[test]
    fn denies_malformed_auth_payloads() {
        assert_eq!(decide(&snap(), "alice", t0()), Decision::Deny { reason: "malformed-auth" });
        assert_eq!(decide(&snap(), "", t0()), Decision::Deny { reason: "malformed-auth" });
        assert_eq!(decide(&snap(), ":pw", t0()), Decision::Deny { reason: "malformed-auth" });
        assert_eq!(decide(&snap(), "alice:", t0()), Decision::Deny { reason: "malformed-auth" });
        // 坏时间戳当成到期（fail-closed），不是当成不过期
        let mut s = snap();
        s.users.get_mut("alice").unwrap().expires_at = Some("not a time".into());
        assert_eq!(decide(&s, "alice:pw:with:colons", t0()), Decision::Deny { reason: "expired" });
    }

    #[test]
    fn an_empty_snapshot_denies_everyone() {
        assert_eq!(decide(&Snapshot::empty(), "alice:pw", t0()), Decision::Deny { reason: "no-such-user" });
    }

    #[test]
    fn run_with_returns_the_id_and_appends_a_redacted_log_line() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        snapshot::write_if_changed(&crate::paths::auth_snapshot_file(&p), &snap()).unwrap();
        let args = vec![
            "203.0.113.10:51820".to_string(),
            "alice:pw:with:colons".to_string(),
            "0".to_string(),
        ];
        let (code, out) = run_with(&p, &args, t0());
        assert_eq!(code, 0);
        assert_eq!(out.as_deref(), Some("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa"));
        let log = std::fs::read_to_string(log_path(&p)).unwrap();
        assert!(log.contains("alice"), "日志要能定位到用户：{log}");
        assert!(log.contains("allow"), "{log}");
        assert!(!log.contains("pw:with:colons"), "密码绝不能进日志：{log}");
        assert!(log.contains("203.0.113.10:51820"), "addr 进日志便于排查：{log}");
    }

    #[test]
    fn run_with_is_fail_closed_without_a_snapshot_or_with_too_few_args() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let (code, out) = run_with(&p, &["addr".into(), "alice:pw".into(), "0".into()], t0());
        assert_eq!(code, 1, "快照不存在必须拒绝，不能放行");
        assert_eq!(out, None);
        assert_eq!(run_with(&p, &["only-addr".into()], t0()).0, 1);
        assert_eq!(run_with(&p, &[], t0()).0, 1);
    }

    #[test]
    fn the_log_is_0600_and_is_truncated_in_place_when_oversized() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        // 每行 51 字节 × 30000 ≈ 1.53 MiB > LOG_MAX_BYTES
        let mut big = String::new();
        for i in 0..30_000u32 {
            big.push_str(&format!("2026-09-11T00:00:00Z 203.0.113.10:1 u{i:05} allow-x\n"));
        }
        assert!(big.len() as u64 > LOG_MAX_BYTES, "先确认这份日志真的超限");
        std::fs::write(log_path(&p), &big).unwrap();
        let _ = run_with(&p, &["a".into(), "alice:pw".into(), "0".into()], t0());

        // ① 绝不产生兄弟文件：P1 的 drift::BASE_WHITELIST 只认 `auth-hook.log`，
        //    多一个 auth-hook.log.1 就是一条永久 stray_file（/api/health 判 degraded），
        //    还会被 `bui reconcile --force` 删掉。
        let mut top: Vec<String> = std::fs::read_dir(&p.base_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        top.sort();
        assert_eq!(top, vec!["auth-hook.log".to_string()], "<base> 顶层只许有这一个文件");

        // ② 原地截断成尾部 + 本次新追加的一行
        let after = std::fs::read_to_string(log_path(&p)).unwrap();
        assert!(
            (after.len() as u64) <= LOG_KEEP_BYTES + 128,
            "截断后应只剩尾部 LOG_KEEP_BYTES：{}",
            after.len()
        );
        assert!(after.starts_with("2026-09-11T00:00:00Z"), "不能留半行：{:?}", &after[..40]);
        assert!(after.trim_end().ends_with("no-such-user"), "本次那一行要在末尾：{:?}", after.trim_end().lines().last());
        assert_eq!(std::fs::metadata(log_path(&p)).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::auth_hook`
Expected: 编译失败，`cannot find function 'decide' in this scope`。

- [ ] **Step 3: 实现 `auth_hook.rs`**

```rust
//! `bui auth-hook <addr> <auth> <tx>`：Hysteria2 `auth.type: command` 的钩子。
//!
//! **极简路径**（spec §3.2 + 调研 H1–H9）：不初始化 tokio、不初始化 tracing、不加载 state，
//! 只读一个小 JSON 文件。内核对钩子既不设超时也不限流，每条新 QUIC 连接 fork 一次本进程，
//! 所以这里不允许出现任何重量级初始化；`main.rs` 把这一支放在建 runtime 之前。
//!
//! 判定失败一律 fail-closed（退出码 1）。stderr 不会进 `hysteria-server` 的 journal（H5），
//! 诊断写 `<base>/auth-hook.log`（0600，只记用户名与结果，**不记密码**）。

use crate::modules::panel::snapshot::{self, Snapshot};
use bui_schema::paths::Paths;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use subtle::ConstantTimeEq;

pub const HOOK_TIMEOUT: Duration = Duration::from_secs(2);
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;

pub fn log_path(paths: &Paths) -> PathBuf {
    paths.base_dir.join("auth-hook.log")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow { user_id: String },
    Deny { reason: &'static str },
}

impl Decision {
    fn label(&self) -> &'static str {
        match self {
            Decision::Allow { .. } => "allow",
            Decision::Deny { reason } => reason,
        }
    }
}

pub fn decide(snap: &Snapshot, auth: &str, now: time::OffsetDateTime) -> Decision {
    // H1：auth 是 `user:pass` 原串，按**第一个**冒号拆分（密码里可以再有冒号）
    let Some((username, password)) = auth.split_once(':') else {
        return Decision::Deny { reason: "malformed-auth" };
    };
    if username.is_empty() || password.is_empty() {
        return Decision::Deny { reason: "malformed-auth" };
    }
    let Some(u) = snap.users.get(username) else {
        return Decision::Deny { reason: "no-such-user" };
    };
    // 常量时间比对（spec §3.2）
    if u.hy2_password.as_bytes().ct_eq(password.as_bytes()).unwrap_u8() != 1 {
        return Decision::Deny { reason: "bad-password" };
    }
    if u.blocked {
        return Decision::Deny { reason: "blocked" };
    }
    if let Some(exp) = &u.expires_at {
        // 解析失败也算到期：fail-closed
        match crate::util::parse_rfc3339(exp) {
            Some(t) if t > now => {}
            _ => return Decision::Deny { reason: "expired" },
        }
    }
    Decision::Allow { user_id: u.user_id.clone() }
}

pub fn run(args: &[String]) -> i32 {
    let (code, out) = run_with(&Paths::default_server(), args, time::OffsetDateTime::now_utc());
    if let Some(id) = out {
        println!("{id}");
    }
    code
}

pub fn run_with(paths: &Paths, args: &[String], now: time::OffsetDateTime) -> (i32, Option<String>) {
    let addr = args.first().cloned().unwrap_or_default();
    let Some(auth) = args.get(1).cloned() else {
        log_line(paths, &addr, "-", "missing-args");
        return (1, None);
    };
    // 内核不设超时（H8）：把「读文件 + 判定」放子线程，主线程自己掐 2 秒
    let (tx, rx) = std::sync::mpsc::channel::<Decision>();
    let file = crate::paths::auth_snapshot_file(paths);
    let auth_for_thread = auth.clone();
    std::thread::spawn(move || {
        let snap = snapshot::read(&file);
        // 空快照（文件缺失或坏 JSON）里查不到任何用户 ⇒ no-such-user ⇒ 拒绝
        let _ = tx.send(decide(&snap, &auth_for_thread, now));
    });
    let decision = rx.recv_timeout(HOOK_TIMEOUT).unwrap_or(Decision::Deny { reason: "timeout" });
    let username = auth.split_once(':').map(|(u, _)| u).unwrap_or("-");
    log_line(paths, &addr, username, decision.label());
    match decision {
        Decision::Allow { user_id } => (0, Some(user_id)),
        Decision::Deny { .. } => (1, None),
    }
}

/// 追加一行 `<RFC3339> <addr> <username> <结果>`。失败一律忽略——钩子不能因为写不了日志
/// 就拒绝合法用户。
fn log_line(paths: &Paths, addr: &str, username: &str, result: &str) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = log_path(paths);
    if std::fs::metadata(&path).map(|m| m.len() > LOG_MAX_BYTES).unwrap_or(false) {
        truncate_in_place(&path);
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path)
    else {
        return;
    };
    let ts = crate::util::fmt_rfc3339(time::OffsetDateTime::now_utc());
    let _ = writeln!(f, "{ts} {addr} {username} {result}");
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
}

/// 超限时只保留尾部 [`LOG_KEEP_BYTES`]，**重写同一个文件**。
///
/// 不要改成 `rename` 出 `auth-hook.log.1`：P1 的 `reconcile::drift::BASE_WHITELIST`
/// 只认 `auth-hook.log`（`TRANSIENT_SUFFIXES` 也只有 `.tmp` / `.new`），多出来的兄弟文件
/// 会让每 10 分钟的漂移巡检永久报 `stray_file`（`/api/health` 随之 degraded），
/// 并被 `bui reconcile --force` 删掉。
fn truncate_in_place(path: &std::path::Path) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    let keep_from = data.len().saturating_sub(LOG_KEEP_BYTES as usize);
    // 从下一个换行之后开始，避免文件头留下半行
    let start = match data[keep_from..].iter().position(|b| *b == b'\n') {
        Some(i) => keep_from + i + 1,
        None => keep_from,
    };
    let Ok(mut f) = std::fs::OpenOptions::new().write(true).truncate(true).mode(0o600).open(path)
    else {
        return;
    };
    let _ = f.write_all(&data[start..]);
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
```

- [ ] **Step 4: 改 `main.rs` 的那一支（决策 D3）**

把 P1 Task 1 留下的
```rust
    if let Command::AuthHook { args } = &command {
        let _ = args;
        // P2 的交付物；P1 在这里就返回，连 runtime 与日志都不初始化
        anyhow::bail!("auth-hook 由 P2 实现");
    }
```
换成
```rust
    if let Command::AuthHook { args } = &command {
        // spec §3.2：不建 tokio runtime、不初始化 tracing、不加载 state。
        // 退出码即判定结果（0 = 放行），放行时 stdout 已打印 user_id。
        std::process::exit(modules::panel::auth_hook::run(args));
    }
```
`dispatch()` 里那一支保持 `unreachable!("auth-hook 已在 main 里提前返回")`。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui panel::auth_hook && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 7 passed，clippy 无输出。再手验入口没有被重量级初始化污染：
```bash
cargo build -p bui
./target/debug/bui auth-hook 2>/dev/null; echo "exit=$?"    # 期望 exit=1，stdout 无输出
./target/debug/bui auth-hook a alice:pw 0 2>/dev/null; echo "exit=$?"   # 期望 exit=1（本机没有 /opt/b-ui/auth-snapshot.json）
```

- [ ] **Step 6: Commit**

```
git add crates/bui/src/modules/panel/auth_hook.rs crates/bui/src/main.rs
git commit -m "feat(panel): bui auth-hook（极简路径、2 秒硬超时、fail-closed、自写日志）"
```

---

### Task 4: Xray gRPC（vendored proto + tonic 客户端 + `xray api rmu` 退路）

**Files:**
- Create: `crates/bui/build.rs`
- Create: `crates/bui/proto/{app/proxyman/command/command.proto, app/proxyman/config.proto, app/stats/command/command.proto, common/net/address.proto, common/net/port.proto, common/protocol/user.proto, common/serial/typed_message.proto, core/config.proto, proxy/vless/account.proto, transport/internet/config.proto}`
- Create: `crates/bui/proto/SHA256SUMS`
- **不改** `crates/bui/Cargo.toml`：tonic / tonic-prost / prost 与 `[build-dependencies]` 已由 T1 一次加齐（决策 D4）
- Modify: `crates/bui/src/modules/panel/xray.rs`（把 Task 1 的空壳换成真实实现）

**Interfaces:**
- Consumes: `crate::modules::panel::{TxRx, XrayApi, XRAY_API_ADDR, XRAY_INBOUND_TAGS, VLESS_FLOW}`、`crate::sys::Host`、`bui_schema::paths::Paths`
- Produces:
```rust
/// prost / tonic 生成的代码。模块树必须与 proto 的 package 层级一致
/// （生成代码用 `super::super::…` 跨包引用），所以用 `include_file("xray.rs")` 的嵌套版本。
/// **必须挂 `#[allow(clippy::all)]`**：生成代码在本 crate 内，`cargo clippy --all-targets
/// -- -D warnings` 对它一样生效，而 proto 注释与 oneof 会稳定触发
/// `doc_lazy_continuation` / `large_enum_variant` / `enum_variant_names` 等。
#[allow(clippy::all)]
pub mod pb { include!(concat!(env!("OUT_DIR"), "/xray.rs")); }

/// 两层 `TypedMessage` 的类型名（调研 X2；必须是 proto 消息全名，服务端按它做反射解码）
pub const TYPE_ADD_USER: &str = "xray.app.proxyman.command.AddUserOperation";
pub const TYPE_REMOVE_USER: &str = "xray.app.proxyman.command.RemoveUserOperation";
pub const TYPE_VLESS_ACCOUNT: &str = "xray.proxy.vless.Account";
/// `QueryStats` 的 pattern：纯子串匹配（X6），`user>>>` 命中所有用户计数器
pub const STATS_PATTERN: &str = "user>>>";
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { Up, Down }

/// 组装 `AlterInbound` 的请求（两层 TypedMessage，X1–X3）。纯函数，可单测。
pub fn add_user_request(tag: &str, user_id: Uuid, vless_uuid: Uuid)
    -> pb::xray::app::proxyman::command::AlterInboundRequest;
pub fn remove_user_request(tag: &str, user_id: Uuid)
    -> pb::xray::app::proxyman::command::AlterInboundRequest;
/// `user>>><email>>>>traffic>>>uplink|downlink` → (email, 方向)；不认的名字返回 None
pub fn parse_user_counter(name: &str) -> Option<(String, Direction)>;
/// `QueryStatsResponse.stat` → email → 增量（uplink 计 tx、downlink 计 rx，与 v3 一致）
pub fn deltas_from_stats(stats: &[pb::xray::app::stats::command::Stat]) -> BTreeMap<String, TxRx>;
/// CLI 退路（X9）：`xray api rmu --server=<addr> -tag=<tag> <email>`
pub fn rmu_args(addr: &str, tag: &str, email: &str) -> Vec<String>;
/// 找 xray 可执行文件：先 `<bin>/xray`，再 PATH 上的 `xray`；都没有返回 None
pub fn xray_program(host: &dyn Host, paths: &Paths) -> Option<PathBuf>;

pub struct XrayClient { /* addr + OnceLock<Channel> */ }
impl XrayClient {
    pub fn new() -> Self;                                  // XRAY_API_ADDR
    pub fn with_addr(addr: impl Into<String>) -> Self;
    pub fn addr(&self) -> &str;
    pub fn endpoint_url(&self) -> String;                  // "http://<addr>"
}
impl XrayApi for XrayClient { /* add_user / remove_user / query_user_deltas */ }
```

**为什么不在单元测试里连真 xray**：全局约束「不碰真实系统、不联网」。本任务能机器化验证的是「请求字节组装对不对」（用 prost 解码回来逐层断言）与「计数器名解析对不对」；`AlterInbound` / `QueryStats` 在真内核上的行为由 **M3 里程碑验收**覆盖（spec §9：「加用户时 `NRestarts` 不变且在线会话不断」）。调研 X11 也标注了「未实机对 REALITY inbound 跑过 AddUser/RemoveUser」，M3 正是那一项的兜底。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/xray.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use prost::Message;

    fn uid() -> Uuid {
        Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa").unwrap()
    }

    fn vid() -> Uuid {
        Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
    }

    #[test]
    fn add_user_request_nests_two_typed_messages() {
        let req = add_user_request("vless-direct", uid(), vid());
        assert_eq!(req.tag, "vless-direct");
        let op = req.operation.expect("operation 必须存在");
        assert_eq!(op.r#type, TYPE_ADD_USER);
        let add = pb::xray::app::proxyman::command::AddUserOperation::decode(op.value.as_slice()).unwrap();
        let user = add.user.expect("user 必须存在");
        assert_eq!(user.level, 0);
        assert_eq!(user.email, uid().to_string(), "email 就是 user_id（spec §3.3 的唯一键）");
        let acct_tm = user.account.expect("account 必须存在");
        assert_eq!(acct_tm.r#type, TYPE_VLESS_ACCOUNT);
        let acct = pb::xray::proxy::vless::Account::decode(acct_tm.value.as_slice()).unwrap();
        assert_eq!(acct.id, vid().to_string());
        assert_eq!(acct.flow, "xtls-rprx-vision");
        assert_eq!(acct.encryption, "", "v4 不填 encryption（X3）");
        // 以下两条绑在 pinned 版本 v26.3.27 的 account.proto 字段 4–9 上（调研 X3）：
        // 换 pinned 版本时这三条断言要连同 `proto/SHA256SUMS` 一起重看，字段改名/挪位就会编译不过。
        assert_eq!(acct.xor_mode, 0);
        assert!(acct.reverse.is_none());
    }

    #[test]
    fn remove_user_request_carries_only_the_email() {
        let req = remove_user_request("vless-residential", uid());
        assert_eq!(req.tag, "vless-residential");
        let op = req.operation.unwrap();
        assert_eq!(op.r#type, TYPE_REMOVE_USER);
        let rm = pb::xray::app::proxyman::command::RemoveUserOperation::decode(op.value.as_slice()).unwrap();
        assert_eq!(rm.email, uid().to_string());
    }

    #[test]
    fn counter_names_parse_into_email_and_direction() {
        assert_eq!(
            parse_user_counter("user>>>alice-id>>>traffic>>>uplink"),
            Some(("alice-id".to_string(), Direction::Up))
        );
        assert_eq!(
            parse_user_counter("user>>>alice-id>>>traffic>>>downlink"),
            Some(("alice-id".to_string(), Direction::Down))
        );
        // email 里带 `>>>` 之外的任何字符都要原样保留
        assert_eq!(
            parse_user_counter("user>>>8d5a1a1e-3b2c-4d1e-9f00-0000000000aa>>>traffic>>>uplink"),
            Some(("8d5a1a1e-3b2c-4d1e-9f00-0000000000aa".to_string(), Direction::Up))
        );
        assert_eq!(parse_user_counter("inbound>>>api>>>traffic>>>uplink"), None);
        assert_eq!(parse_user_counter("user>>>alice>>>traffic>>>sidelink"), None);
        assert_eq!(parse_user_counter("user>>>>>>traffic>>>uplink"), None, "空 email 不认");
        assert_eq!(parse_user_counter(""), None);
    }

    #[test]
    fn stats_become_per_user_deltas_with_uplink_as_tx() {
        use pb::xray::app::stats::command::Stat;
        let stats = vec![
            Stat { name: "user>>>a>>>traffic>>>uplink".into(), value: 100 },
            Stat { name: "user>>>a>>>traffic>>>downlink".into(), value: 250 },
            Stat { name: "user>>>b>>>traffic>>>uplink".into(), value: 7 },
            Stat { name: "inbound>>>api>>>traffic>>>uplink".into(), value: 999 },
            Stat { name: "user>>>c>>>traffic>>>uplink".into(), value: -5 },
        ];
        let d = deltas_from_stats(&stats);
        assert_eq!(d["a"], TxRx { tx: 100, rx: 250 });
        assert_eq!(d["b"], TxRx { tx: 7, rx: 0 });
        assert!(!d.contains_key("api"), "非 user 计数器不进表");
        assert_eq!(d.get("c"), None, "负值（不该出现）当 0 丢弃，绝不回绕成天文数字");
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn rmu_args_match_the_cli_contract() {
        assert_eq!(
            rmu_args("127.0.0.1:10085", "vless-direct", "alice-id"),
            vec![
                "api".to_string(),
                "rmu".to_string(),
                "--server=127.0.0.1:10085".to_string(),
                "-tag=vless-direct".to_string(),
                "alice-id".to_string(),
            ]
        );
    }

    #[test]
    fn xray_program_prefers_bin_dir_then_path() {
        let paths = bui_schema::paths::Paths::default_server();
        let h = FakeHost::new();
        assert_eq!(xray_program(&h, &paths), None, "两处都没有就返回 None");
        h.with(|i| {
            i.which.insert("xray".into());
        });
        assert_eq!(xray_program(&h, &paths), Some(std::path::PathBuf::from("xray")));
        h.with(|i| {
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
        });
        assert_eq!(xray_program(&h, &paths), Some(std::path::PathBuf::from("/opt/b-ui/bin/xray")));
    }

    #[test]
    fn endpoint_url_is_plain_http_on_the_api_inbound() {
        assert_eq!(XrayClient::new().addr(), XRAY_API_ADDR);
        assert_eq!(XrayClient::new().endpoint_url(), "http://127.0.0.1:10085");
        assert_eq!(XrayClient::with_addr("127.0.0.1:1").endpoint_url(), "http://127.0.0.1:1");
    }

    #[tokio::test]
    async fn calls_fail_fast_when_nothing_listens_on_the_api_port() {
        // 端口 1 上不会有 xray：lazy channel 在第一次调用时才连，连不上就是 Err，
        // 不会 panic、不会挂住（CONNECT_TIMEOUT 3 秒）
        let c = XrayClient::with_addr("127.0.0.1:1");
        assert!(c.remove_user("vless-direct", uid()).await.is_err());
    }

    #[test]
    fn vendored_protos_match_the_recorded_checksums() {
        // 换 pinned 版本时必须同步更新 SHA256SUMS，否则这条测试会告诉你 proto 变了
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proto");
        let sums = std::fs::read_to_string(root.join("SHA256SUMS")).expect("proto/SHA256SUMS 必须存在");
        let mut n = 0;
        for line in sums.lines().filter(|l| !l.trim().is_empty()) {
            let (want, rel) = line.split_once("  ").expect("格式是 `<sha256>  <相对路径>`");
            let bytes = std::fs::read(root.join(rel)).unwrap_or_else(|_| panic!("缺少 {rel}"));
            assert_eq!(crate::kernels::sha256_hex(&bytes), want, "{rel} 的内容与记录不符");
            n += 1;
        }
        assert_eq!(n, 10, "调研 X8：闭包正好 10 个 .proto");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::xray`
Expected: 编译失败，`cannot find module 'pb' in this scope` / `failed to resolve: use of undeclared crate or module 'prost'`。

- [ ] **Step 3: vendor 10 个 proto 并记录校验和**

在仓库根执行（`v26.3.27` 是 pinned 版本，spec §3.3）：
```bash
cd crates/bui
for f in app/proxyman/command/command.proto app/proxyman/config.proto app/stats/command/command.proto \
         common/net/address.proto common/net/port.proto common/protocol/user.proto \
         common/serial/typed_message.proto core/config.proto proxy/vless/account.proto \
         transport/internet/config.proto; do
  mkdir -p "proto/$(dirname "$f")"
  curl -fsSL -o "proto/$f" "https://raw.githubusercontent.com/XTLS/Xray-core/v26.3.27/$f"
done
cd proto && find . -name '*.proto' | sort | xargs sha256sum | sed 's|\./||' > SHA256SUMS && cat SHA256SUMS
```
`SHA256SUMS` 必须与下面这份逐字节相同（2026-09-12 于 `v26.3.27` 实测）：
```
4b3e4e9ad1488af54429ef6a3948fc4d527033a526280d708e389883e264e7b0  app/proxyman/command/command.proto
65f95a5a1cefbd32c9b68240d3a69a1bbf61c543669e1291a49dacb4fbccfff2  app/proxyman/config.proto
0f37d863b0b13b7bdf08ced2c2e3a9007238a3d46841350baef5c555b0badda5  app/stats/command/command.proto
5d91454bf2f686b51a2eb6d3c2ef7590dcd1593efd990930bf47066d6b370db8  common/net/address.proto
e76c7e70f1ca2e2a0675b1e87bdb9cb75dce6527a2428567daff2445cf51aeaf  common/net/port.proto
33e5c5ea3cbf330e7c526c6b0c93bc8f07a3f8b4cad579bf50698e3c544270e5  common/protocol/user.proto
e0d8b7fdc4557eae7c628130d909ad648e89f2f7bab68f44bdf8ddae08e1c711  common/serial/typed_message.proto
312ef0a4a9add73bbc044eab8864025fd8452ede49e7687fe75cccb17a749394  core/config.proto
1e03384298211ee21e666cf8a80f50eba16a3c054fdc07a4da13fe67d9b71dac  proxy/vless/account.proto
6dd79df85a60f2be5a47ada464ebca70905dbbe1bc10e166220a799661aab49c  transport/internet/config.proto
```
对不上就是上游改了文件或下载被中转篡改：**停下来报给 Fable，不要改 SHA256SUMS 迁就**。

- [ ] **Step 4: 确认依赖已就位（不改 `Cargo.toml`）**

tonic / tonic-prost / prost 与 `[build-dependencies]`（`tonic-prost-build` + `protoc-bin-vendored`）都由 **T1 一次加齐**（决策 D4：T1 / T4 / T11 并行，各改一次 `Cargo.toml` 会让 `Cargo.lock` 三方冲突）。本步只核对，**不要动 `Cargo.toml`**：

```bash
grep -n 'tonic\|prost\|\[build-dependencies\]' crates/bui/Cargo.toml
# 期望：[dependencies] 里有 tonic / tonic-prost / prost，
#       文件末尾有 [build-dependencies] 且含 tonic-prost-build 与 protoc-bin-vendored
```
对不上就是 T1 没合并或加漏了：**回去补 T1，不要在本任务里加**（会撞 `Cargo.lock`）。

- [ ] **Step 5: 写 `crates/bui/build.rs`**

```rust
//! 生成 Xray 的 gRPC 客户端代码（P2 Task 4）。
//!
//! 两个要点，改动前先读：
//! 1. `protoc` 由 `protoc-bin-vendored` 提供，本机与 CI 都不需要装 protobuf-compiler。
//! 2. 必须用 `include_file`：prost 生成的跨包引用形如 `super::super::super::common::serial::TypedMessage`，
//!    只有把模块树按 proto 的 package 层级嵌套起来才解析得通。`include_file` 生成的
//!    `xray.rs` 就是那棵树，`xray.rs` 里再 `include!` 各包的实现文件。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .include_file("xray.rs")
        .compile_protos(
            &[
                "proto/app/proxyman/command/command.proto",
                "proto/app/stats/command/command.proto",
                "proto/common/protocol/user.proto",
                "proto/common/serial/typed_message.proto",
                "proto/core/config.proto",
                "proto/proxy/vless/account.proto",
                "proto/app/proxyman/config.proto",
                "proto/transport/internet/config.proto",
                "proto/common/net/address.proto",
                "proto/common/net/port.proto",
            ],
            &["proto"],
        )?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
```

- [ ] **Step 6: 实现 `panel/xray.rs`**

```rust
//! Xray gRPC 客户端（spec §3.3、调研 X1–X11）。
//!
//! - 增删用户：`HandlerService.AlterInbound{tag, operation}`，`operation` 是**两层** `TypedMessage`
//!   （外层 `AddUserOperation` 包 `User`，`User.account` 再包 `vless.Account`）；对两个 inbound 各调一次。
//! - `email` 是 gRPC 侧唯一键，取 `user_id`（`bui_schema::render::xray::clients` 里也是它）。
//! - 统计：`StatsService.QueryStats(pattern="user>>>", reset=true)` 一次拉全量增量；
//!   `reset` 对每个计数器原子交换清零，不丢不重（X7）。
//! - `RemoveUser` **只阻止新握手**，已建立的 REALITY 连接会活到客户端自己断开（X10，spec §4.2 已接受该窗口）。

// 生成代码归 prost/tonic 管，clippy 的意见对它没有意义；不挂这个 allow，
// Step 7 的 `cargo clippy --all-targets -- -D warnings` 大概率直接失败。
#[allow(clippy::all)]
pub mod pb {
    // prost 生成的嵌套模块树；不要改成平铺的 `include_proto!`，跨包引用会解析失败。
    include!(concat!(env!("OUT_DIR"), "/xray.rs"));
}

use super::{TxRx, XrayApi};
use bui_schema::paths::Paths;
use prost::Message;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;

use pb::xray::app::proxyman::command::{
    handler_service_client::HandlerServiceClient, AddUserOperation, AlterInboundRequest,
    RemoveUserOperation,
};
use pb::xray::app::stats::command::{
    stats_service_client::StatsServiceClient, QueryStatsRequest, Stat,
};
use pb::xray::common::protocol::User as PbUser;
use pb::xray::common::serial::TypedMessage;
use pb::xray::proxy::vless::Account;

pub const TYPE_ADD_USER: &str = "xray.app.proxyman.command.AddUserOperation";
pub const TYPE_REMOVE_USER: &str = "xray.app.proxyman.command.RemoveUserOperation";
pub const TYPE_VLESS_ACCOUNT: &str = "xray.proxy.vless.Account";
pub const STATS_PATTERN: &str = "user>>>";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

fn typed(type_name: &str, msg: &impl Message) -> TypedMessage {
    TypedMessage { r#type: type_name.to_string(), value: msg.encode_to_vec() }
}

pub fn add_user_request(tag: &str, user_id: Uuid, vless_uuid: Uuid) -> AlterInboundRequest {
    let account = Account {
        id: vless_uuid.to_string(),
        flow: super::VLESS_FLOW.to_string(),
        ..Default::default()
    };
    let user = PbUser {
        level: 0,
        email: user_id.to_string(),
        account: Some(typed(TYPE_VLESS_ACCOUNT, &account)),
    };
    let op = AddUserOperation { user: Some(user) };
    AlterInboundRequest { tag: tag.to_string(), operation: Some(typed(TYPE_ADD_USER, &op)) }
}

pub fn remove_user_request(tag: &str, user_id: Uuid) -> AlterInboundRequest {
    let op = RemoveUserOperation { email: user_id.to_string() };
    AlterInboundRequest { tag: tag.to_string(), operation: Some(typed(TYPE_REMOVE_USER, &op)) }
}

pub fn parse_user_counter(name: &str) -> Option<(String, Direction)> {
    let rest = name.strip_prefix("user>>>")?;
    let (email, dir) = rest.rsplit_once(">>>traffic>>>")?;
    let d = match dir {
        "uplink" => Direction::Up,
        "downlink" => Direction::Down,
        _ => return None,
    };
    if email.is_empty() {
        return None;
    }
    Some((email.to_string(), d))
}

pub fn deltas_from_stats(stats: &[Stat]) -> BTreeMap<String, TxRx> {
    let mut out: BTreeMap<String, TxRx> = BTreeMap::new();
    for s in stats {
        let Some((email, dir)) = parse_user_counter(&s.name) else { continue };
        // 计数器是单调累加的 u64，`reset=true` 之后返回的是增量；负值理论上不可能，
        // 出现就丢弃（绝不 `as u64` 回绕成天文数字，那会瞬间把用户判成超限）
        let Ok(v) = u64::try_from(s.value) else { continue };
        if v == 0 {
            continue;
        }
        let e = out.entry(email).or_default();
        match dir {
            Direction::Up => e.add(TxRx { tx: v, rx: 0 }),
            Direction::Down => e.add(TxRx { tx: 0, rx: v }),
        }
    }
    out
}

pub fn rmu_args(addr: &str, tag: &str, email: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "rmu".to_string(),
        format!("--server={addr}"),
        format!("-tag={tag}"),
        email.to_string(),
    ]
}

pub fn xray_program(host: &dyn crate::sys::Host, paths: &Paths) -> Option<PathBuf> {
    let owned = paths.bin_dir.join("xray");
    if host.read_file(&owned).ok().flatten().is_some() {
        return Some(owned);
    }
    host.which("xray").then(|| PathBuf::from("xray"))
}

pub struct XrayClient {
    addr: String,
    channel: OnceLock<Channel>,
}

impl XrayClient {
    pub fn new() -> Self {
        Self::with_addr(super::XRAY_API_ADDR)
    }

    pub fn with_addr(addr: impl Into<String>) -> Self {
        Self { addr: addr.into(), channel: OnceLock::new() }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn endpoint_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// `connect_lazy` 的通道：进程启动时 xray 可能还没起来，所以不在构造时连；
    /// 通道自己会重连，掉线不需要我们重建。
    fn channel(&self) -> anyhow::Result<Channel> {
        if let Some(c) = self.channel.get() {
            return Ok(c.clone());
        }
        let ep = Endpoint::from_shared(self.endpoint_url())?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CALL_TIMEOUT);
        let c = ep.connect_lazy();
        let _ = self.channel.set(c.clone());
        Ok(c)
    }
}

impl Default for XrayClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl XrayApi for XrayClient {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()> {
        let mut c = HandlerServiceClient::new(self.channel()?);
        c.alter_inbound(add_user_request(tag, user_id, vless_uuid)).await?;
        Ok(())
    }

    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()> {
        let mut c = HandlerServiceClient::new(self.channel()?);
        c.alter_inbound(remove_user_request(tag, user_id)).await?;
        Ok(())
    }

    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let mut c = StatsServiceClient::new(self.channel()?);
        let resp = c
            .query_stats(QueryStatsRequest {
                pattern: STATS_PATTERN.to_string(),
                reset: true,
                ..Default::default()
            })
            .await?;
        Ok(deltas_from_stats(&resp.into_inner().stat))
    }
}
```
- [ ] **Step 7: 运行测试**

Run: `cargo test -p bui panel::xray && cargo clippy -p bui --all-targets -- -D warnings && cargo fmt --check`
Expected: 9 passed，clippy / fmt 无输出。首次构建会额外编译 tonic / prost / protoc-bin-vendored（约 1 分钟）。

再复核 T1 那条「只拨死端口」的铁律测试：填实之后它是真的会去连接的，一旦被改成默认地址，在跑着内核的机器上执行 `cargo test` 就会把线上 Xray 的用户计数器 `reset` 清零。
```bash
cargo test -p bui panel::tests::the_clients_report_errors_instead_of_pretending_and_only_dial_a_dead_port
grep -n 'with_addr("127.0.0.1:1")' crates/bui/src/modules/panel/mod.rs   # 期望：命中
```
Expected: 1 passed；grep 命中。**不要**把它改成 `XRAY_API_ADDR`，也不要因为「空壳已经填实」就删掉它。

- [ ] **Step 8: Commit**

```
git add crates/bui/build.rs crates/bui/proto crates/bui/src/modules/panel/xray.rs
git commit -m "feat(panel): Xray gRPC（vendored proto、两层 TypedMessage、QueryStats reset）"
```

---

### Task 5: 两个 hysteria 的 trafficStats 客户端

**Files:**
- Modify: `crates/bui/src/modules/panel/hy2.rs`（把 Task 1 的空壳换成真实实现）

**Interfaces:**
- Consumes: `crate::modules::panel::{Hy2Api, TxRx, HY2_STATS_SECRET}`、`reqwest`（P1 的 Cargo.toml 里已有 `reqwest = { …, features = ["rustls-tls","json","blocking"] }`，本任务用它的**异步**客户端，不加依赖）
- Produces:
```rust
pub const REQ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub struct Hy2Client { /* base + secret + reqwest::Client */ }
impl Hy2Client {
    pub fn new() -> Self;                                  // base = "http://127.0.0.1"，secret = HY2_STATS_SECRET
    pub fn with_base(base: impl Into<String>) -> Self;
    pub fn with_secret(self, secret: impl Into<String>) -> Self;
    pub fn url(&self, port: u16, path: &str) -> String;    // "<base>:<port><path>"
    pub fn secret(&self) -> &str;
}
impl Hy2Api for Hy2Client { /* traffic_clear / online / kick */ }

/// `{"<id>":{"tx":n,"rx":n}}` → 表；坏条目跳过（H10 的返回体形状）
pub fn parse_traffic(body: &str) -> BTreeMap<String, TxRx>;
/// `{"<id>":n}` → 表；`n <= 0` 视为不在线、不进表（H11）
pub fn parse_online(body: &str) -> BTreeMap<String, u32>;
/// `/kick` 的请求体：JSON 字符串数组（H12）
pub fn kick_body(ids: &[String]) -> Vec<u8>;
/// `trafficStats.secret` 非空时要发的头；**头值精确等于 secret，没有 `Bearer ` 前缀**（H13）
pub fn auth_header(secret: &str) -> Option<(&'static str, String)>;
```

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/hy2.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn urls_are_built_from_base_and_port() {
        let c = Hy2Client::new();
        assert_eq!(c.url(9999, "/traffic?clear=1"), "http://127.0.0.1:9999/traffic?clear=1");
        assert_eq!(c.url(9998, "/online"), "http://127.0.0.1:9998/online");
        assert_eq!(Hy2Client::with_base("http://127.0.0.1/").url(9999, "/kick"), "http://127.0.0.1:9999/kick");
    }

    #[test]
    fn traffic_and_online_bodies_parse_and_tolerate_junk() {
        let t = parse_traffic(r#"{"u-1":{"tx":10,"rx":20},"u-2":{"tx":0,"rx":0},"u-3":"junk"}"#);
        assert_eq!(t["u-1"], TxRx { tx: 10, rx: 20 });
        assert_eq!(t["u-2"], TxRx { tx: 0, rx: 0 });
        assert!(!t.contains_key("u-3"), "坏条目跳过，不让整轮采样失败");
        assert!(parse_traffic("not json").is_empty());
        let o = parse_online(r#"{"u-1":2,"u-2":0,"u-3":-1,"u-4":"x"}"#);
        assert_eq!(o, BTreeMap::from([("u-1".to_string(), 2u32)]));
        assert!(parse_online("{}").is_empty());
    }

    #[test]
    fn kick_body_is_a_json_string_array() {
        assert_eq!(kick_body(&["u-1".to_string(), "u-2".to_string()]), br#"["u-1","u-2"]"#.to_vec());
        assert_eq!(kick_body(&[]), b"[]".to_vec());
    }

    #[test]
    fn the_auth_header_has_no_bearer_prefix_and_is_omitted_when_empty() {
        // 调研 H13：hysteria 要求头值**精确等于** secret
        assert_eq!(auth_header(""), None, "bui-schema 渲染的 secret 是空串 ⇒ 不发头");
        assert_eq!(auth_header("s3cr3t"), Some(("authorization", "s3cr3t".to_string())));
        assert_eq!(HY2_STATS_SECRET, "");
    }

    /// 起一个**进程内的回环** HTTP 服务（`127.0.0.1:0`，随机端口）当假 hysteria。
    /// 它不碰机器配置、不出网，只为把「方法 + 路径 + 查询串 + 头」这几件在 HTTP 线上的事
    /// 真的验一遍——这些恰恰是纯函数测不到、又最容易写错的地方（`?clear=1` 漏了就等于流量翻倍重复计）。
    async fn fake_hysteria(reply: &'static str) -> (u16, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                // 必须**读满**再回包：hyper 对 POST 常把请求头与请求体分两次写，
                // 只 read 一次 4 KiB 的话 `req.ends_with(r#"["u-1"]"#)` 会偶发失败。
                // 先读到空行拿完整头，再按 content-length 补齐请求体。
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
                let _ = tx.send(String::from_utf8_lossy(&raw).to_string()).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn traffic_clear_sends_get_with_clear_1_and_no_auth_header() {
        let (port, mut rx) = fake_hysteria(r#"{"u-1":{"tx":5,"rx":6}}"#).await;
        let c = Hy2Client::new();
        let got = c.traffic_clear(port).await.unwrap();
        assert_eq!(got["u-1"], TxRx { tx: 5, rx: 6 });
        let req = rx.recv().await.unwrap();
        assert!(req.starts_with("GET /traffic?clear=1 HTTP/1.1"), "请求行不对：{req}");
        assert!(
            !req.to_ascii_lowercase().contains("authorization:"),
            "secret 为空时不应发 Authorization：{req}"
        );
    }

    #[tokio::test]
    async fn online_and_kick_use_the_documented_shapes_and_a_bare_secret_header() {
        let (port, mut rx) = fake_hysteria(r#"{"u-1":3}"#).await;
        let c = Hy2Client::new().with_secret("s3cr3t");
        assert_eq!(c.online(port).await.unwrap()["u-1"], 3);
        let req = rx.recv().await.unwrap();
        assert!(req.starts_with("GET /online HTTP/1.1"), "{req}");
        assert!(req.contains("authorization: s3cr3t"), "头值必须是裸 secret：{req}");
        assert!(!req.contains("Bearer"), "绝不能带 Bearer 前缀：{req}");

        let (port2, mut rx2) = fake_hysteria("").await;
        c.kick(port2, &["u-1".to_string()]).await.unwrap();
        let req2 = rx2.recv().await.unwrap();
        assert!(req2.starts_with("POST /kick HTTP/1.1"), "{req2}");
        assert!(req2.ends_with(r#"["u-1"]"#), "请求体必须是 JSON 字符串数组：{req2}");
    }

    #[tokio::test]
    async fn a_dead_instance_is_an_error_not_a_hang() {
        // 端口 1 上没人听：3 秒超时之内必须返回 Err（采样任务靠它把错误记进 /api/health）
        let c = Hy2Client::new();
        assert!(c.online(1).await.is_err());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::hy2`
Expected: 编译失败，`cannot find function 'parse_traffic' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! 两个 hysteria 实例的 trafficStats HTTP API 客户端（spec §4.2、调研 H10–H13）。
//!
//! - `GET /traffic?clear=1`：读取与清零在内核的同一把锁里完成，返回的就是**增量**，不丢不重（H10）。
//! - `GET /online`：值是该 id 当前的 QUIC 连接数，`>0` 即在线（H11）。
//! - `POST /kick`：体是 JSON 字符串数组；只是**标记**，要等该用户下次有流量才断连（H12）。
//! - `trafficStats.secret` 非空时，`Authorization` 头的值**精确等于** secret，**没有** `Bearer ` 前缀（H13）。
//!   `bui_schema::render::hysteria` 目前渲染空串，所以默认不发这个头。

use super::{Hy2Api, TxRx};
use std::collections::BTreeMap;
use std::time::Duration;

pub const REQ_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Hy2Client {
    base: String,
    secret: String,
    http: reqwest::Client,
}

impl Hy2Client {
    pub fn new() -> Self {
        Self::with_base("http://127.0.0.1")
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            secret: super::HY2_STATS_SECRET.to_string(),
            http: reqwest::Client::builder()
                .timeout(REQ_TIMEOUT)
                .build()
                .expect("reqwest 客户端构造不会失败"),
        }
    }

    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = secret.into();
        self
    }

    pub fn url(&self, port: u16, path: &str) -> String {
        format!("{}:{}{}", self.base.trim_end_matches('/'), port, path)
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    fn with_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match auth_header(&self.secret) {
            Some((k, v)) => rb.header(k, v),
            None => rb,
        }
    }
}

impl Default for Hy2Client {
    fn default() -> Self {
        Self::new()
    }
}

pub fn auth_header(secret: &str) -> Option<(&'static str, String)> {
    (!secret.is_empty()).then(|| ("authorization", secret.to_string()))
}

pub fn parse_traffic(body: &str) -> BTreeMap<String, TxRx> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeMap::new();
    };
    let Some(obj) = v.as_object() else { return BTreeMap::new() };
    obj.iter()
        .filter_map(|(id, e)| {
            let tx = e.get("tx")?.as_u64()?;
            let rx = e.get("rx")?.as_u64()?;
            Some((id.clone(), TxRx { tx, rx }))
        })
        .collect()
}

pub fn parse_online(body: &str) -> BTreeMap<String, u32> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeMap::new();
    };
    let Some(obj) = v.as_object() else { return BTreeMap::new() };
    obj.iter()
        .filter_map(|(id, n)| {
            let n = n.as_i64()?;
            (n > 0).then(|| (id.clone(), n as u32))
        })
        .collect()
}

pub fn kick_body(ids: &[String]) -> Vec<u8> {
    serde_json::to_vec(ids).expect("字符串数组序列化不会失败")
}

#[async_trait::async_trait]
impl Hy2Api for Hy2Client {
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let url = self.url(port, "/traffic?clear=1");
        let resp = self.with_auth(self.http.get(&url)).send().await?.error_for_status()?;
        Ok(parse_traffic(&resp.text().await?))
    }

    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        let url = self.url(port, "/online");
        let resp = self.with_auth(self.http.get(&url)).send().await?.error_for_status()?;
        Ok(parse_online(&resp.text().await?))
    }

    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let url = self.url(port, "/kick");
        self.with_auth(self.http.post(&url))
            .header("content-type", "application/json")
            .body(kick_body(ids))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::hy2 && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 7 passed，clippy 无输出。

再复核 T1 那条「只拨死端口」的铁律测试：填实之后 `Hy2Api::online` 是真的会去连接的，一旦被改成 `HY2_STATS_PORT_DIRECT`，在跑着内核的机器上它会**成功**（`is_err()` 当场失败），而 `/traffic?clear=1` 那一侧还会把线上流量账取走。
```bash
cargo test -p bui panel::tests::the_clients_report_errors_instead_of_pretending_and_only_dial_a_dead_port
grep -n 'Hy2Api::online(&k, 1)' crates/bui/src/modules/panel/mod.rs   # 期望：命中
```
Expected: 1 passed；grep 命中。**不要**把端口改成 9999 / 9998，也不要删掉这条测试。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/hy2.rs
git commit -m "feat(panel): hysteria trafficStats 客户端（/traffic?clear=1、/online、/kick）"
```

---

### Task 6: 用户域（v3 兼容投影、CRUD 域逻辑、限额判定、同步反应器）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/users.rs`

**Interfaces:**
- Consumes: `crate::modules::panel::{Shared, TxRx, XRAY_INBOUND_TAGS}`、`crate::modules::panel::snapshot::{Snapshot, write_if_changed}`、`crate::modules::panel::xray::{rmu_args, xray_program}`、`crate::reconcile::DaemonCtx`、`crate::api::Event`、`crate::util::{fmt_rfc3339, parse_rfc3339}`、`bui_schema::model::{State, User, NodeParams, Entitlements, Protocol, Credentials, ResidentialEntitlement, TrafficLimit, Usage, PortalAuth, Billing}`
- Produces:
```rust
pub const GIB: u64 = 1_073_741_824;
pub const SYNC_INTERVAL_SECS: u64 = 60;

// ---- v3 兼容投影（`/api/users` 的元素，字段名逐字照 v3 的 users.json；见「v3 端点逐个契约」#10）----
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUser {
    pub username: String,
    pub protocol: &'static str,                 // "fusion" | "hysteria2" | "vless-reality"
    #[serde(rename = "createdAt")] pub created_at: String,
    pub limits: PanelLimits,
    pub usage: PanelUsage,
    pub password: String,                       // hy2 密码（v3 同名同义）
    pub uuid: Uuid,
    pub sni: String,                            // 全局 REALITY 伪装域（v4 没有 per-user sni）
    pub residential: bool,
    pub disabled: bool,                         // v4 新增，旧前端忽略未知字段
    pub blocked: bool,                          // v4 新增：本轮限额判定的结果
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelLimits {
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")] pub expires_at: Option<String>,
    #[serde(rename = "trafficLimit", skip_serializing_if = "Option::is_none")] pub traffic_limit: Option<u64>,
    #[serde(rename = "monthlyLimit", skip_serializing_if = "Option::is_none")] pub monthly_limit: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUsage { pub total: u64, pub monthly: BTreeMap<String, u64> }

pub fn protocol_label(e: &Entitlements) -> &'static str;
pub fn project(u: &User, node: &NodeParams, blocked: &BTreeSet<Uuid>) -> PanelUser;
pub fn project_all(state: &State, blocked: &BTreeSet<Uuid>) -> Vec<PanelUser>;

// ---- CRUD 域逻辑（HTTP 解析与状态写入在 Task 8）----
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateRequest {
    pub username: String,
    #[serde(default)] pub password: Option<String>,
    #[serde(default)] pub days: Option<f64>,
    #[serde(default)] pub traffic: Option<f64>,     // GB
    #[serde(default)] pub monthly: Option<f64>,     // GB
    #[serde(default)] pub protocol: Option<String>,
    #[serde(default)] pub residential: Option<bool>,
    #[serde(default)] pub sni: Option<String>,      // 接受但忽略（决策 D13）
    #[serde(default)] pub speed: Option<f64>,       // 接受但忽略（决策 D13）
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRequest {
    #[serde(default)] pub username: Option<String>,
    #[serde(default)] pub password: Option<String>,
    #[serde(default)] pub days: Option<f64>,
    #[serde(default)] pub traffic: Option<f64>,
    #[serde(default)] pub monthly: Option<f64>,
    #[serde(default)] pub speed: Option<f64>,       // 接受但忽略（决策 D13）
    #[serde(default)] pub disabled: Option<bool>,
}
pub fn validate_username(name: &str) -> Result<(), String>;      // 移植 web/server.js:246-251
pub fn validate_password(pw: &str) -> Result<(), String>;        // 移植 web/server.js:253-257
pub fn protocols_from_label(label: &str) -> Result<Vec<Protocol>, String>;
pub fn random_hy2_password() -> String;                          // 16 个 hex 字符，与 v3 同形
pub fn new_user(req: &CreateRequest, now: time::OffsetDateTime) -> Result<User, String>;
pub fn apply_update(u: &mut User, req: &UpdateRequest, now: time::OffsetDateTime) -> Result<(), String>;

// ---- 限额判定（纯函数，Task 7 的采样任务与本任务的反应器共用）----
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason { Disabled, Expired, TotalExceeded, MonthlyExceeded }
pub fn month_key(now: time::OffsetDateTime) -> String;            // "2026-09"（UTC，决策 D7）
/// `extra` 是尚未落盘的内存增量；判定按 `usage + extra`
pub fn is_blocked(u: &User, extra: TxRx, now: time::OffsetDateTime) -> Option<BlockReason>;
pub fn blocked_set(state: &State, pending: &BTreeMap<Uuid, TxRx>, now: time::OffsetDateTime) -> BTreeSet<Uuid>;

// ---- 同步反应器 ----
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncOutcome {
    pub snapshot_written: bool,
    pub added: Vec<Uuid>,
    pub removed: Vec<Uuid>,
    pub newly_blocked: Vec<Uuid>,
    pub errors: Vec<String>,
}
/// 幂等：把「期望态 + 拒绝集合」同步到内核 —— 重写快照 + 对两个 inbound 做 gRPC 差分。
pub async fn sync_users(ctx: &DaemonCtx, shared: &Shared, blocked: &BTreeSet<Uuid>) -> SyncOutcome;
/// 读当前 state 与内存增量、算拒绝集合、调 `sync_users`
pub async fn sync_now(ctx: &DaemonCtx, shared: &Shared) -> SyncOutcome;
/// 订阅 `EventBus`：任何 `StateChanged` 立刻同步一次；另有每 60 秒的安全网
pub async fn sync_loop(ctx: DaemonCtx, shared: Arc<Shared>);
```

**「该被拒的用户」要无条件 RemoveUser**（决策 D6 的核心，别写成从 `applied.xray_users` 里筛）：
`Applied` 只活在本进程内，装机后 / 守护进程重启后它是空的，而 `xray-config.json` 里带着**全量** `clients`（P1 Task 10 传 `&state.users`），到期 / 超限 / 禁用用户的 REALITY 凭据一直在里面。所以「该删谁」必须从**期望态**算：凡是有 Reality 权益、当前却该被拒的用户，每一轮都发一次 RemoveUser，不看本进程有没有 AddUser 过他。去重靠 `Applied::xray_removed`（删成功才记账），`NRestarts` 变化时 `xray_users` 与 `xray_removed` 一起清空重放。若只从 `applied.xray_users` 里筛，spec §4.2「超限/到期 → Xray RemoveUser」与 M3「到期用户被拒」在「守护进程重启后」这条路径上直接落空。

**xray 报「已存在 / 不存在」都按成功处理**（`target_already_reached`）：`AddUser` 撞上已有同 email（装机后第一轮必然发生）、`RemoveUser` 撞上根本不存在的 email（刚重启、或从没 add 过）时，xray 都返回错误，而这两种情形下**目标状态其实已经达成**。实现把错误串里含 `already` / `not found` / `not exist`（大小写不敏感）的当成功并记 debug 日志；否则装完机第一轮同步满屏报错，被封用户也会白跑一遍 CLI 退路。**错误文案未经实机核对**（调研只覆盖 proto 与调用链），M3 联调时核对一次；匹配不上的代价只是多一条 error 与一次幂等的 CLI 退路。

**为什么还要每 60 秒的安全网**（两条都是真实缺口，不是保险起见）：
1. xray 重启的那一瞬间，被封用户从配置里复活，而重启侦测要等下一轮才看到 `NRestarts` 变化。
2. `AddUser` / `RemoveUser` 真失败（xray 正在重启、gRPC 超时）时本轮只记 error，下一轮自然重试。所以 `AddUser` 这条方向不需要 CLI 退路（决策 D12）。
3. **它和采样 tick 不是互斥的冗余，别删掉任何一个**：Task 7 的 `tick` 每 10 秒就已经调一次 `users::sync_now`，所以上面两条缺口在正常情况下**由采样任务兜住**；这 60 秒一轮只兜「采样任务本身挂掉 / 被 panic 掉 / 长时间卡在某次 `spawn_blocking`」的情形——那时事件驱动的路径也不一定有事件来。看起来重复，实则是两套独立的心跳。

- [ ] **Step 1: 写失败测试（投影与纯函数）**

`crates/bui/src/modules/panel/users.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::harness;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn alice(s: &State) -> &User {
        s.users.iter().find(|u| u.username == "alice").unwrap()
    }

    #[test]
    fn protocol_labels_mirror_the_v3_mapping() {
        let mut e = sample_state().users[0].entitlements.clone();
        assert_eq!(protocol_label(&e), "fusion");
        e.protocols = vec![Protocol::Hysteria2];
        assert_eq!(protocol_label(&e), "hysteria2");
        e.protocols = vec![Protocol::Reality];
        assert_eq!(protocol_label(&e), "vless-reality");
        e.protocols = vec![];
        assert_eq!(protocol_label(&e), "hysteria2", "空权益按 v3 的默认值兜底");
    }

    #[test]
    fn projection_keeps_every_v3_field_name() {
        let mut s = sample_state();
        s.users[0].entitlements.expires_at = Some("2026-10-01T00:00:00Z".into());
        s.users[0].entitlements.traffic_limit.total_bytes = Some(5 * GIB);
        s.users[0].usage = Usage {
            total_bytes: 1234,
            monthly_bytes: 234,
            month_key: "2026-09".into(),
            last_seen_at: None,
        };
        let v = serde_json::to_value(project(alice(&s), &s.node, &BTreeSet::new())).unwrap();
        assert_eq!(v["username"], "alice");
        assert_eq!(v["protocol"], "fusion");
        assert_eq!(v["createdAt"], "2026-09-11T00:00:00Z");
        assert_eq!(v["limits"]["expiresAt"], "2026-10-01T00:00:00Z");
        assert_eq!(v["limits"]["trafficLimit"], 5 * GIB);
        assert!(v["limits"].get("monthlyLimit").is_none(), "不限的项不出现（v3 也是这样）");
        assert!(v["limits"].get("speedLimit").is_none(), "决策 D13：speedLimit 已删");
        assert_eq!(v["usage"]["total"], 1234);
        assert_eq!(v["usage"]["monthly"]["2026-09"], 234);
        assert_eq!(v["password"], "pw-alice-01");
        assert_eq!(v["uuid"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(v["sni"], "www.bing.com", "全局 REALITY 伪装域，不是建户时的旧拷贝");
        assert_eq!(v["residential"], true);
        assert_eq!(v["disabled"], false);
        assert_eq!(v["blocked"], false);
    }

    #[test]
    fn projection_marks_blocked_users() {
        let s = sample_state();
        let id = alice(&s).user_id;
        let p = project(alice(&s), &s.node, &BTreeSet::from([id]));
        assert!(p.blocked);
        assert_eq!(project_all(&s, &BTreeSet::from([id])).len(), 1);
    }

    #[test]
    fn usernames_and_passwords_follow_the_v3_rules() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("张三_a-b.c").is_ok());
        assert_eq!(validate_username("").unwrap_err(), "username 不能为空");
        assert_eq!(validate_username(&"a".repeat(65)).unwrap_err(), "username 长度不能超过 64 字符");
        assert_eq!(
            validate_username("a/b").unwrap_err(),
            "username 仅允许字母/数字/中文/下划线/连字符/点"
        );
        assert!(validate_username("a b").is_err());
        assert!(validate_password("x").is_ok());
        assert_eq!(validate_password("").unwrap_err(), "password 不能为空");
        assert_eq!(validate_password(&"a".repeat(257)).unwrap_err(), "password 长度不能超过 256 字符");
    }

    #[test]
    fn new_user_converts_days_and_gigabytes_like_v3() {
        let req = CreateRequest {
            username: "bob".into(),
            password: None,
            days: Some(30.0),
            traffic: Some(1.5),
            monthly: Some(0.0),
            protocol: Some("fusion".into()),
            residential: Some(false),
            sni: Some("ignored.example.com".into()),
            speed: Some(100.0),
            ..Default::default()
        };
        let u = new_user(&req, t0()).unwrap();
        assert_eq!(u.username, "bob");
        assert_eq!(u.created_at, "2026-09-11T00:00:00Z");
        assert_eq!(u.entitlements.expires_at.as_deref(), Some("2026-10-11T00:00:00Z"));
        assert_eq!(u.entitlements.traffic_limit.total_bytes, Some(1_610_612_736), "1.5 GiB");
        assert_eq!(u.entitlements.traffic_limit.monthly_bytes, None, "0 = 不限");
        assert_eq!(u.entitlements.protocols, vec![Protocol::Hysteria2, Protocol::Reality]);
        assert!(u.entitlements.direct);
        assert!(u.entitlements.residential.is_none(), "residential=false ⇒ 不给住宅权益");
        assert_eq!(u.credentials.hy2_password.len(), 16, "没给密码就随机 16 个 hex 字符");
        assert_eq!(u.usage, Usage::default());
        assert!(!u.disabled);
        // 显式给密码时原样用
        let req2 = CreateRequest { username: "carol".into(), password: Some("pw".into()), ..Default::default() };
        assert_eq!(new_user(&req2, t0()).unwrap().credentials.hy2_password, "pw");
        // 默认协议与住宅（v3：protocol 缺省 hysteria2、residential 缺省 true）
        let d = new_user(&CreateRequest { username: "dave".into(), ..Default::default() }, t0()).unwrap();
        assert_eq!(d.entitlements.protocols, vec![Protocol::Hysteria2]);
        assert_eq!(d.entitlements.residential.map(|r| r.group_id), Some("default".to_string()));
        // 不认的协议名报错，不静默兜底
        assert!(new_user(&CreateRequest { username: "e".into(), protocol: Some("vless-ws-tls".into()), ..Default::default() }, t0()).is_err());
        assert!(new_user(&CreateRequest { username: "a/b".into(), ..Default::default() }, t0()).is_err());
    }

    #[test]
    fn apply_update_follows_v3_semantics() {
        let mut s = sample_state();
        let u = &mut s.users[0];
        apply_update(
            u,
            &UpdateRequest {
                username: Some("alice2".into()),
                password: Some("new-pw".into()),
                days: Some(10.0),
                traffic: Some(2.0),
                monthly: Some(1.0),
                speed: Some(50.0),
                disabled: None,
            },
            t0(),
        )
        .unwrap();
        assert_eq!(u.username, "alice2");
        assert_eq!(u.credentials.hy2_password, "new-pw");
        assert_eq!(u.entitlements.expires_at.as_deref(), Some("2026-09-21T00:00:00Z"));
        assert_eq!(u.entitlements.traffic_limit.total_bytes, Some(2 * GIB));
        assert_eq!(u.entitlements.traffic_limit.monthly_bytes, Some(GIB));
        // 0 清除限制（v3：`> 0` 才设，否则 delete）
        apply_update(u, &UpdateRequest { days: Some(0.0), traffic: Some(0.0), monthly: Some(0.0), ..Default::default() }, t0()).unwrap();
        assert_eq!(u.entitlements.expires_at, None);
        assert_eq!(u.entitlements.traffic_limit, TrafficLimit::default());
        // 只有 Reality 权益的用户，password 改的是 uuid（v3 同逻辑）
        u.entitlements.protocols = vec![Protocol::Reality];
        apply_update(u, &UpdateRequest { password: Some("22222222-2222-4222-8222-222222222222".into()), ..Default::default() }, t0()).unwrap();
        assert_eq!(u.credentials.vless_uuid.to_string(), "22222222-2222-4222-8222-222222222222");
        assert_eq!(u.credentials.hy2_password, "new-pw", "hy2 密码没被动");
        // 不是合法 UUID 就报错
        assert!(apply_update(u, &UpdateRequest { password: Some("not-a-uuid".into()), ..Default::default() }, t0()).is_err());
        // disabled 是 v4 新增的显式开关
        apply_update(u, &UpdateRequest { disabled: Some(true), ..Default::default() }, t0()).unwrap();
        assert!(u.disabled);
    }

    #[test]
    fn month_key_and_limit_checks() {
        assert_eq!(month_key(t0()), "2026-09");
        assert_eq!(month_key(datetime!(2026-01-01 00:00:00 UTC)), "2026-01");
        let mut s = sample_state();
        let u = &mut s.users[0];
        assert_eq!(is_blocked(u, TxRx::default(), t0()), None);
        u.disabled = true;
        assert_eq!(is_blocked(u, TxRx::default(), t0()), Some(BlockReason::Disabled));
        u.disabled = false;
        u.entitlements.expires_at = Some("2026-09-10T00:00:00Z".into());
        assert_eq!(is_blocked(u, TxRx::default(), t0()), Some(BlockReason::Expired));
        u.entitlements.expires_at = Some("坏时间".into());
        assert_eq!(is_blocked(u, TxRx::default(), t0()), Some(BlockReason::Expired), "解析失败 fail-closed");
        u.entitlements.expires_at = None;
        u.entitlements.traffic_limit.total_bytes = Some(100);
        u.usage.total_bytes = 60;
        assert_eq!(is_blocked(u, TxRx::default(), t0()), None);
        assert_eq!(
            is_blocked(u, TxRx { tx: 20, rx: 20 }, t0()),
            Some(BlockReason::TotalExceeded),
            "判定要带上未落盘的内存增量"
        );
        u.entitlements.traffic_limit.total_bytes = None;
        u.entitlements.traffic_limit.monthly_bytes = Some(100);
        u.usage.monthly_bytes = 150;
        u.usage.month_key = "2026-09".into();
        assert_eq!(is_blocked(u, TxRx::default(), t0()), Some(BlockReason::MonthlyExceeded));
        u.usage.month_key = "2026-08".into();
        assert_eq!(is_blocked(u, TxRx::default(), t0()), None, "跨月了，上个月的用量不算这个月");
    }

    #[test]
    fn blocked_set_covers_every_user() {
        let mut s = sample_state();
        s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
        let id = s.users[0].user_id;
        let pending = BTreeMap::from([(id, TxRx { tx: 20, rx: 0 })]);
        assert_eq!(blocked_set(&s, &pending, t0()), BTreeSet::from([id]));
        assert!(blocked_set(&s, &BTreeMap::new(), t0()).is_empty());
    }
}
```

- [ ] **Step 2: 写失败测试（同步反应器）**

同一个 `mod tests` 里继续追加：
```rust
    use crate::modules::panel::testsupport::Harness;
    use crate::reconcile::DaemonCtx;

    fn ctx_of(h: &Harness) -> DaemonCtx {
        DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        }
    }

    #[tokio::test]
    async fn sync_writes_the_snapshot_and_adds_reality_users_to_both_inbounds() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.snapshot_written);
        assert_eq!(out.added, vec![id]);
        assert!(out.removed.is_empty());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![format!("add:vless-direct:{id}:{vid}"), format!("add:vless-residential:{id}:{vid}")]
        );
        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert_eq!(snap.users["alice"].blocked, false);
    }

    #[tokio::test]
    async fn a_second_sync_touches_neither_the_snapshot_nor_grpc() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(!out.snapshot_written, "内容没变就不该写盘");
        assert!(out.added.is_empty());
        assert!(h.xray.calls().is_empty(), "幂等：第二轮零 gRPC 调用");
    }

    #[tokio::test]
    async fn blocking_a_user_removes_him_from_both_inbounds_and_marks_the_snapshot() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id]);
        assert_eq!(out.newly_blocked, vec![id], "供采样任务去 kick");
        assert!(out.snapshot_written);
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-direct:{id}"), format!("remove:vless-residential:{id}")]
        );
        assert!(crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked);
        // 再同步一次不重复报 newly_blocked
        h.xray.clear_calls();
        let again = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert!(again.newly_blocked.is_empty());
        assert!(h.xray.calls().is_empty());
    }

    /// 决策 D6 的核心回归：`Applied` 只活在进程内，守护进程重启后它是空的，
    /// 而 `xray-config.json` 里还带着被封用户的 clients ⇒ 第一轮同步就必须无条件 RemoveUser。
    #[tokio::test]
    async fn a_blocked_user_is_removed_even_if_this_process_never_added_him() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id], "没 AddUser 过也要删（spec §4.2）");
        assert!(out.added.is_empty());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-direct:{id}"), format!("remove:vless-residential:{id}")]
        );
        // `applied.xray_removed` 去重：第二轮零 gRPC
        h.xray.clear_calls();
        let again = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert!(again.removed.is_empty());
        assert!(h.xray.calls().is_empty(), "{:?}", h.xray.calls());
    }

    #[tokio::test]
    async fn a_disabled_user_is_removed_on_the_first_sync_too() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].disabled = true;
            })
            .await
            .unwrap();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.removed, vec![id], "禁用与超限同样处理（spec §4.2）");
        assert!(crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked);
    }

    #[tokio::test]
    async fn a_failed_remove_falls_back_to_the_xray_cli() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        h.host.with(|i| {
            i.which.insert("xray".into());
        });
        h.xray.with(|i| {
            i.fail_on.insert(format!("remove:vless-direct:{id}"));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id], "退路成功就算删掉了");
        assert!(
            h.host.ops().contains(&format!(
                "run:xray api rmu --server=127.0.0.1:10085 -tag=vless-direct {id}"
            )),
            "没走 CLI 退路：{:?}",
            h.host.ops()
        );
    }

    #[tokio::test]
    async fn a_remove_that_reports_not_found_needs_no_cli_fallback() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.host.with(|i| {
            i.which.insert("xray".into());
        });
        // xray 侧本来就没有这个 email（刚重启 / 从没 AddUser 过）⇒ 目标已达成
        h.xray.with(|i| {
            for tag in XRAY_INBOUND_TAGS {
                let key = format!("remove:{tag}:{id}");
                i.fail_on.insert(key.clone());
                i.error_text.insert(key, format!("User {id} not found."));
            }
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id]);
        assert!(out.errors.is_empty(), "「不存在」不算失败：{:?}", out.errors);
        assert!(
            !h.host.ops().iter().any(|o| o.contains("api rmu")),
            "「不存在」不该再跑 CLI 退路：{:?}",
            h.host.ops()
        );
    }

    #[tokio::test]
    async fn an_add_that_reports_already_exists_is_not_an_error() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        // 装机后第一轮的真实情形：xray 侧已有同 email（`applied` 只活在进程内）
        let key = format!("add:vless-direct:{id}:{vid}");
        h.xray.with(|i| {
            i.fail_on.insert(key.clone());
            i.error_text.insert(key, format!("User {id} already exists."));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(out.errors.is_empty(), "「已存在」按成功处理：{:?}", out.errors);
        assert_eq!(out.added, vec![id], "记进 applied，第二轮就不再 AddUser");
    }

    #[tokio::test]
    async fn an_add_that_fails_for_real_is_recorded_as_an_error() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        let vid = h.store.read().await.users[0].credentials.vless_uuid;
        // 默认错误串（不含 already / not found）：真失败
        h.xray.with(|i| {
            i.fail_on.insert(format!("add:vless-direct:{id}:{vid}"));
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert!(
            out.errors.iter().any(|e| e.contains("vless-direct")),
            "真失败要记 error：{:?}",
            out.errors
        );
        assert!(out.added.is_empty(), "有 inbound 没加成功就不能记成已同步，留给 60 秒安全网重试");
    }

    #[tokio::test]
    async fn an_xray_restart_replays_the_whole_add_set() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.host.with(|i| {
            i.unit_props.insert(("xray.service".into(), "NRestarts".into()), "0".into());
        });
        sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        h.xray.clear_calls();
        // 决策 D6：xray 重启会从 xray-config.json 把全部 clients（含被封的）读回来
        h.host.with(|i| {
            i.unit_props.insert(("xray.service".into(), "NRestarts".into()), "1".into());
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.added.len(), 1, "重启后必须重放一遍");
        assert_eq!(h.xray.calls().len(), 2);
    }

    #[tokio::test]
    async fn an_xray_restart_replays_the_removal_of_a_blocked_user() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.host.with(|i| {
            i.unit_props.insert(("xray.service".into(), "NRestarts".into()), "0".into());
        });
        sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        h.xray.clear_calls();
        // 决策 D6：重启把 xray-config.json 里被封用户的 clients 又读回来了
        h.host.with(|i| {
            i.unit_props.insert(("xray.service".into(), "NRestarts".into()), "1".into());
        });
        let out = sync_users(&ctx, &h.shared, &BTreeSet::from([id])).await;
        assert_eq!(out.removed, vec![id], "重启后 RemoveUser 也要重放，不只是 AddUser");
        assert_eq!(
            h.xray.calls(),
            vec![format!("remove:vless-direct:{id}"), format!("remove:vless-residential:{id}")]
        );
    }

    #[tokio::test]
    async fn sync_now_derives_the_blocked_set_from_state_and_pending() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
            })
            .await
            .unwrap();
        h.shared.pending().await.insert(id, TxRx { tx: 50, rx: 0 });
        let out = sync_now(&ctx, &h.shared).await;
        assert_eq!(out.newly_blocked, vec![id]);
        assert!(crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked);
    }

    #[tokio::test]
    async fn reality_only_users_never_reach_the_snapshot_but_do_reach_xray() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.store
            .update(|s| {
                s.users[0].entitlements.protocols = vec![Protocol::Reality];
            })
            .await
            .unwrap();
        let out = sync_users(&ctx, &h.shared, &BTreeSet::new()).await;
        assert_eq!(out.added.len(), 1);
        assert!(crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users.is_empty());
    }
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p bui panel::users`
Expected: 编译失败，`cannot find function 'protocol_label' in this scope`。

- [ ] **Step 4: 实现投影与纯函数**

```rust
//! 用户域：面板投影（v3 兼容）、CRUD 域逻辑、限额判定、同步反应器。
//!
//! 移植参照：`web/server.js:1157-1226`（`handleManage` 的 create/update/delete）、
//! `web/server.js:1945-2067`（`/api/users` 的 GET/POST/PUT/DELETE）、
//! `web/server.js:246-257`（两个校验函数）、`web/server.js:1146-1155`（`checkUserLimits`）。

use super::snapshot::{self, Snapshot};
use super::xray::{rmu_args, xray_program};
use super::{Shared, TxRx, XRAY_INBOUND_TAGS};
use crate::api::Event;
use crate::reconcile::DaemonCtx;
use bui_schema::model::{
    Billing, Credentials, Entitlements, NodeParams, PortalAuth, Protocol, ResidentialEntitlement,
    State, TrafficLimit, Usage, User, DEFAULT_GROUP,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

pub const GIB: u64 = 1_073_741_824;
pub const SYNC_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelLimits {
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(rename = "trafficLimit", skip_serializing_if = "Option::is_none")]
    pub traffic_limit: Option<u64>,
    #[serde(rename = "monthlyLimit", skip_serializing_if = "Option::is_none")]
    pub monthly_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUsage {
    pub total: u64,
    pub monthly: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PanelUser {
    pub username: String,
    pub protocol: &'static str,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub limits: PanelLimits,
    pub usage: PanelUsage,
    pub password: String,
    pub uuid: Uuid,
    pub sni: String,
    pub residential: bool,
    pub disabled: bool,
    pub blocked: bool,
}

/// v3 的 `protocol` 字段，映射方向与 `bui_schema::v3::import` 相反（spec §4.1）。
pub fn protocol_label(e: &Entitlements) -> &'static str {
    let hy2 = e.protocols.contains(&Protocol::Hysteria2);
    let reality = e.protocols.contains(&Protocol::Reality);
    match (hy2, reality) {
        (true, true) => "fusion",
        (false, true) => "vless-reality",
        // 空权益按 v3 的默认值（`handleManage` 的 `protocol || "hysteria2"`）
        _ => "hysteria2",
    }
}

pub fn project(u: &User, node: &NodeParams, blocked: &BTreeSet<Uuid>) -> PanelUser {
    PanelUser {
        username: u.username.clone(),
        protocol: protocol_label(&u.entitlements),
        created_at: u.created_at.clone(),
        limits: PanelLimits {
            expires_at: u.entitlements.expires_at.clone(),
            traffic_limit: u.entitlements.traffic_limit.total_bytes,
            monthly_limit: u.entitlements.traffic_limit.monthly_bytes,
        },
        usage: PanelUsage {
            total: u.usage.total_bytes,
            monthly: if u.usage.month_key.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([(u.usage.month_key.clone(), u.usage.monthly_bytes)])
            },
        },
        password: u.credentials.hy2_password.clone(),
        uuid: u.credentials.vless_uuid,
        // v4 没有 per-user sni：面板与订阅都用全局 REALITY 伪装域（v3.5.13 的修复口径）
        sni: node.reality.sni().to_string(),
        residential: u.entitlements.residential.is_some(),
        disabled: u.disabled,
        blocked: u.disabled || blocked.contains(&u.user_id),
    }
}

pub fn project_all(state: &State, blocked: &BTreeSet<Uuid>) -> Vec<PanelUser> {
    state.users.iter().map(|u| project(u, &state.node, blocked)).collect()
}

/// 移植 `web/server.js:246-251`。正则 `^[\p{L}\p{N}_\-.]+$` 用 `char::is_alphanumeric`
/// （Unicode Alphabetic + Nd/Nl/No）等价实现，不引 `regex`。
/// 注意 v3 允许 `..` 这种名字：v4 的用户名从不参与拼路径（路由参数只用来在 `state.users`
/// 里做精确匹配），所以照抄 v3 的规则不引入路径穿越。
pub fn validate_username(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("username 不能为空".into());
    }
    if name.chars().count() > 64 {
        return Err("username 长度不能超过 64 字符".into());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err("username 仅允许字母/数字/中文/下划线/连字符/点".into());
    }
    Ok(())
}

/// 移植 `web/server.js:253-257`。
pub fn validate_password(pw: &str) -> Result<(), String> {
    if pw.is_empty() {
        return Err("password 不能为空".into());
    }
    if pw.chars().count() > 256 {
        return Err("password 长度不能超过 256 字符".into());
    }
    Ok(())
}

pub fn protocols_from_label(label: &str) -> Result<Vec<Protocol>, String> {
    match label {
        "fusion" => Ok(vec![Protocol::Hysteria2, Protocol::Reality]),
        "hysteria2" => Ok(vec![Protocol::Hysteria2]),
        "vless-reality" => Ok(vec![Protocol::Reality]),
        // v3 还有 `vless-ws-tls`，spec §0 已把它删掉：明确报错而不是静默降级
        other => Err(format!("不支持的协议：{other}（可选 fusion / hysteria2 / vless-reality）")),
    }
}

/// 与 v3 的 `crypto.randomBytes(8).toString("hex")` 同形（16 个 hex 字符）。
pub fn random_hy2_password() -> String {
    hex::encode(rand::random::<[u8; 8]>())
}

fn gb_to_bytes(gb: Option<f64>) -> Option<u64> {
    match gb {
        Some(g) if g > 0.0 => Some((g * GIB as f64) as u64),
        _ => None,
    }
}

fn expires_from_days(days: Option<f64>, now: OffsetDateTime) -> Option<String> {
    match days {
        Some(d) if d > 0.0 => {
            Some(crate::util::fmt_rfc3339(now + Duration::from_secs_f64(d * 86400.0)))
        }
        _ => None,
    }
}

pub fn new_user(req: &CreateRequest, now: OffsetDateTime) -> Result<User, String> {
    validate_username(&req.username)?;
    if let Some(pw) = &req.password {
        validate_password(pw)?;
    }
    let protocols = protocols_from_label(req.protocol.as_deref().unwrap_or("hysteria2"))?;
    // v3.5.5：`residential` 缺省 true
    let residential = req.residential.unwrap_or(true);
    Ok(User {
        user_id: Uuid::new_v4(),
        username: req.username.clone(),
        note: String::new(),
        created_at: crate::util::fmt_rfc3339(now),
        disabled: false,
        credentials: Credentials {
            hy2_password: req.password.clone().unwrap_or_else(random_hy2_password),
            vless_uuid: Uuid::new_v4(),
        },
        entitlements: Entitlements {
            protocols,
            direct: true,
            residential: residential
                .then(|| ResidentialEntitlement { group_id: DEFAULT_GROUP.to_string() }),
            expires_at: expires_from_days(req.days, now),
            traffic_limit: TrafficLimit {
                total_bytes: gb_to_bytes(req.traffic),
                monthly_bytes: gb_to_bytes(req.monthly),
            },
        },
        usage: Usage::default(),
        portal_auth: PortalAuth::default(),
        billing: Billing::default(),
    })
}

pub fn apply_update(u: &mut User, req: &UpdateRequest, now: OffsetDateTime) -> Result<(), String> {
    if let Some(name) = &req.username {
        validate_username(name)?;
        u.username = name.clone();
    }
    if let Some(pw) = &req.password {
        validate_password(pw)?;
        // 移植 v3：只有 Reality 权益的用户，这个字段改的是 UUID
        if protocol_label(&u.entitlements) == "vless-reality" {
            u.credentials.vless_uuid =
                Uuid::parse_str(pw).map_err(|_| "password 必须是合法 UUID（该用户只有 Reality 权益）".to_string())?;
        } else {
            u.credentials.hy2_password = pw.clone();
        }
    }
    if req.days.is_some() {
        u.entitlements.expires_at = expires_from_days(req.days, now);
    }
    if req.traffic.is_some() {
        u.entitlements.traffic_limit.total_bytes = gb_to_bytes(req.traffic);
    }
    if req.monthly.is_some() {
        u.entitlements.traffic_limit.monthly_bytes = gb_to_bytes(req.monthly);
    }
    if let Some(d) = req.disabled {
        u.disabled = d;
    }
    // req.speed 接受但忽略（决策 D13：spec §0「按用户限速…删除」）
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Disabled,
    Expired,
    TotalExceeded,
    MonthlyExceeded,
}

/// UTC 月键（决策 D7：`RealHost::now()` 是 `now_utc()`）。
pub fn month_key(now: OffsetDateTime) -> String {
    format!("{:04}-{:02}", now.year(), now.month() as u8)
}

pub fn is_blocked(u: &User, extra: TxRx, now: OffsetDateTime) -> Option<BlockReason> {
    if u.disabled {
        return Some(BlockReason::Disabled);
    }
    if let Some(exp) = &u.entitlements.expires_at {
        match crate::util::parse_rfc3339(exp) {
            Some(t) if t > now => {}
            // 解析失败也算到期：与 auth-hook 的 fail-closed 口径一致
            _ => return Some(BlockReason::Expired),
        }
    }
    let extra = extra.total();
    if let Some(limit) = u.entitlements.traffic_limit.total_bytes {
        if u.usage.total_bytes.saturating_add(extra) >= limit {
            return Some(BlockReason::TotalExceeded);
        }
    }
    if let Some(limit) = u.entitlements.traffic_limit.monthly_bytes {
        let base = if u.usage.month_key == month_key(now) { u.usage.monthly_bytes } else { 0 };
        if base.saturating_add(extra) >= limit {
            return Some(BlockReason::MonthlyExceeded);
        }
    }
    None
}

pub fn blocked_set(
    state: &State,
    pending: &BTreeMap<Uuid, TxRx>,
    now: OffsetDateTime,
) -> BTreeSet<Uuid> {
    state
        .users
        .iter()
        .filter(|u| {
            is_blocked(u, pending.get(&u.user_id).copied().unwrap_or_default(), now).is_some()
        })
        .map(|u| u.user_id)
        .collect()
}
```

- [ ] **Step 5: 实现请求结构与同步反应器**

接在上面之后（同一个文件）：
```rust
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateRequest {
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub days: Option<f64>,
    #[serde(default)]
    pub traffic: Option<f64>,
    #[serde(default)]
    pub monthly: Option<f64>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub residential: Option<bool>,
    /// 接受但忽略：v4 的 REALITY 伪装域全局唯一（决策 D13）
    #[serde(default)]
    pub sni: Option<String>,
    /// 接受但忽略：spec §0「按用户限速（内核不支持）」（决策 D13）
    #[serde(default)]
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRequest {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub days: Option<f64>,
    #[serde(default)]
    pub traffic: Option<f64>,
    #[serde(default)]
    pub monthly: Option<f64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncOutcome {
    pub snapshot_written: bool,
    pub added: Vec<Uuid>,
    pub removed: Vec<Uuid>,
    pub newly_blocked: Vec<Uuid>,
    pub errors: Vec<String>,
}

pub async fn sync_users(
    ctx: &DaemonCtx,
    shared: &Shared,
    blocked: &BTreeSet<Uuid>,
) -> SyncOutcome {
    // 轮次锁：采样任务与事件反应器都会调本函数，同一时刻只许一轮。
    // 注意它与 `applied` 是两把锁 —— `applied` 只在算差分与写回记账时短暂加锁，
    // 绝不跨 gRPC await 持有（否则 xray 掉线时 `/api/users/health` 会跟着卡）。
    let _turn = shared.sync_guard().await;
    let state = ctx.store.read().await;
    let mut out = SyncOutcome::default();

    // ① 快照（hysteria 侧的主保障）
    let snap = Snapshot::from_state(&state, blocked);
    let path = shared.snapshot_path();
    match snapshot::write_if_changed(&path, &snap) {
        Ok(written) => out.snapshot_written = written,
        Err(e) => out.errors.push(format!("写 auth-snapshot.json 失败：{e}")),
    }

    // ② xray 重启侦测（决策 D6）：重启会把 xray-config.json 里的全部 clients 读回来，
    //    包括被封的那些，所以必须清空两份记账、重放差分。
    let host = ctx.host.clone();
    let restarts = tokio::task::spawn_blocking(move || {
        host.unit_property("xray", "NRestarts").ok().flatten()
    })
    .await
    .ok()
    .flatten();

    // ③ 期望态：只有「未禁用、未被封、有 Reality 权益」的用户该留在两个 inbound 里
    let desired: BTreeMap<Uuid, Uuid> = state
        .users
        .iter()
        .filter(|u| {
            !u.disabled
                && !blocked.contains(&u.user_id)
                && u.entitlements.protocols.contains(&Protocol::Reality)
        })
        .map(|u| (u.user_id, u.credentials.vless_uuid))
        .collect();
    // 该被拒的 Reality 用户：**无条件**删（决策 D6）。不看 `applied.xray_users`——
    // 那份记账只活在本进程内，守护进程重启后是空的，而 `xray-config.json` 里
    // 还带着他们的凭据（P1 Task 10 传的是全量 `state.users`）。
    let mut want_removed: BTreeSet<Uuid> = state
        .users
        .iter()
        .filter(|u| {
            u.entitlements.protocols.contains(&Protocol::Reality)
                && !desired.contains_key(&u.user_id)
        })
        .map(|u| u.user_id)
        .collect();

    // ④ 短暂加锁算差分
    let (to_add, to_remove) = {
        let mut applied = shared.applied().await;
        applied.snapshot_sha = Some(snap.sha256());
        if applied.xray_restarts != restarts {
            if applied.xray_restarts.is_some() {
                tracing::info!(?restarts, "xray 重启过，重放 gRPC 用户差分");
            }
            applied.xray_restarts = restarts;
            applied.xray_users.clear();
            applied.xray_removed.clear();
        }
        // 已从面板删掉的用户不在 `state` 里，只能靠 `applied.xray_users` 发现
        for id in applied.xray_users.keys() {
            if !desired.contains_key(id) {
                want_removed.insert(*id);
            }
        }
        let to_add: Vec<(Uuid, Uuid)> = desired
            .iter()
            .filter(|(id, vid)| applied.xray_users.get(id) != Some(vid))
            .map(|(id, vid)| (*id, *vid))
            .collect();
        let to_remove: Vec<Uuid> = want_removed
            .iter()
            .filter(|id| !applied.xray_removed.contains(id))
            .copied()
            .collect();
        out.newly_blocked = blocked.difference(&applied.blocked).copied().collect();
        applied.blocked = blocked.clone();
        (to_add, to_remove)
    };

    // ⑤ gRPC 差分（不持 `applied`）。两个 inbound 都成功才记账，否则留给 60 秒安全网重试。
    let mut added: Vec<(Uuid, Uuid)> = Vec::new();
    for (id, vid) in to_add {
        let mut ok = true;
        for tag in XRAY_INBOUND_TAGS {
            if let Err(e) = shared.xray().add_user(tag, id, vid).await {
                let msg = e.to_string();
                if target_already_reached(&msg) {
                    tracing::debug!(tag, %id, "AddUser 报已存在，按成功处理");
                } else {
                    ok = false;
                    out.errors.push(format!("AddUser {tag} 失败：{msg}"));
                }
            }
        }
        if ok {
            added.push((id, vid));
            out.added.push(id);
        }
    }
    let mut removed: Vec<Uuid> = Vec::new();
    for id in to_remove {
        let mut ok = true;
        for tag in XRAY_INBOUND_TAGS {
            if let Err(e) = shared.xray().remove_user(tag, id).await {
                let msg = e.to_string();
                if target_already_reached(&msg) {
                    // xray 侧本来就没有这个 email ⇒ 目标已达成，不必跑 CLI 退路
                    tracing::debug!(tag, %id, "RemoveUser 报不存在，按成功处理");
                } else if !cli_remove(ctx, tag, &id.to_string()).await {
                    // CLI 退路（X9、决策 D12）：RemoveUser 这条方向不自愈，必须补上
                    ok = false;
                    out.errors.push(format!("RemoveUser {tag} 失败：{msg}"));
                }
            }
        }
        if ok {
            removed.push(id);
            out.removed.push(id);
        }
    }

    // ⑥ 短暂加锁写回记账
    {
        let mut applied = shared.applied().await;
        for (id, vid) in added {
            applied.xray_users.insert(id, vid);
            // 恢复过的用户将来再被封，还要能再删一次
            applied.xray_removed.remove(&id);
        }
        for id in removed {
            applied.xray_users.remove(&id);
            applied.xray_removed.insert(id);
        }
        // 已从 state 里删掉的用户不必再记账（同一个 user_id 不会回来），顺手别让这个集合长胖
        let known: BTreeSet<Uuid> = state.users.iter().map(|u| u.user_id).collect();
        applied.xray_removed.retain(|id| known.contains(id));
    }
    out
}

/// xray 的 `AddUser` 撞上「email 已存在」、`RemoveUser` 撞上「email 不存在」时都返回错误，
/// 而这两种情形下目标状态其实已经达成，所以按成功处理。
///
/// 错误文案本身**没有实机核对过**（调研 X1–X11 只覆盖 proto 与服务端调用链，没记错误串），
/// 所以这里宽匹配三个子串；M3 真内核联调（bwg-rick）时按 xray 的实际文案核对一次，
/// 对不上就把子串补齐 —— 匹配失败的后果只是多记一条 error + 多跑一次 CLI 退路（幂等），不会误判成功。
fn target_already_reached(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("already") || m.contains("not found") || m.contains("not exist")
}

/// `xray api rmu --server=<addr> -tag=<tag> <email>`；找不到可执行文件或非零退出返回 false。
async fn cli_remove(ctx: &DaemonCtx, tag: &str, email: &str) -> bool {
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let (tag, email) = (tag.to_string(), email.to_string());
    tokio::task::spawn_blocking(move || {
        let Some(prog) = xray_program(host.as_ref(), &paths) else { return false };
        let args = rmu_args(super::XRAY_API_ADDR, &tag, &email);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        host.run(&prog.to_string_lossy(), &refs).map(|o| o.ok()).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

pub async fn sync_now(ctx: &DaemonCtx, shared: &Shared) -> SyncOutcome {
    let now = ctx.host.now();
    let pending = shared.pending().await.clone();
    let blocked = {
        let state = ctx.store.read().await;
        blocked_set(&state, &pending, now)
    };
    sync_users(ctx, shared, &blocked).await
}

pub async fn sync_loop(ctx: DaemonCtx, shared: Arc<Shared>) {
    let mut rx = ctx.bus.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(SYNC_INTERVAL_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 启动时先收敛一次：`bui install` 写的初版快照收的是全量用户（含只有 Reality 权益的），
    // 而且 P2 合并前它也不带限额判定。
    report(sync_now(&ctx, &shared).await);
    loop {
        tokio::select! {
            _ = tick.tick() => report(sync_now(&ctx, &shared).await),
            ev = rx.recv() => match ev {
                Ok(Event::StateChanged(what)) => {
                    tracing::debug!(what, "期望态变化，同步用户");
                    report(sync_now(&ctx, &shared).await);
                }
                Ok(_) => {}
                // 落后就当「有过变化」，直接全量对一次（幂等）
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    report(sync_now(&ctx, &shared).await)
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

fn report(out: SyncOutcome) {
    for e in &out.errors {
        tracing::warn!(error = %e, "用户同步有失败项，下一轮安全网会重试");
    }
    if out.snapshot_written || !out.added.is_empty() || !out.removed.is_empty() {
        tracing::info!(
            snapshot = out.snapshot_written,
            added = out.added.len(),
            removed = out.removed.len(),
            "用户同步完成"
        );
    }
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui panel::users && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 21 passed（纯函数 8 + 反应器 13），clippy 无输出。

- [ ] **Step 7: Commit**

```
git add crates/bui/src/modules/panel/users.rs
git commit -m "feat(panel): 用户投影、CRUD 域逻辑、限额判定与用户同步反应器"
```

---

### Task 7: 采样、累加、落盘、月度重置、限额执行与恢复

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/traffic.rs`

**Interfaces:**
- Consumes: `crate::modules::panel::{Shared, SampleCache, TxRx, HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI}`、`crate::modules::panel::users::{blocked_set, month_key, sync_now, sync_users, SyncOutcome}`（`tick` 第 ④ 步调的是 `sync_now`）、`crate::reconcile::DaemonCtx`、`crate::util::fmt_rfc3339`
- Produces:
```rust
pub const SAMPLE_INTERVAL_SECS: u64 = 10;          // spec §4.2「守护进程内 10 秒任务」
pub const FLUSH_INTERVAL_SECS: i64 = 30;           // spec §4.2「最多每 30 秒合并落盘一次」
pub const XRAY_ONLINE_WINDOW_SECS: i64 = 30;       // spec §4.2「最近 30 秒有 Xray 增量的用户」

/// 一轮采样的原始结果（键都是 `user_id` 的字符串）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub deltas: BTreeMap<String, TxRx>,
    pub online: BTreeMap<String, u32>,
    pub xray_ids: BTreeSet<String>,      // 本轮有 Xray 增量的用户（在线判定用）
    pub errors: Vec<String>,
}
/// 打两个 hysteria 的 `/traffic?clear=1` 与 `/online`，再打一次 `QueryStats(reset=true)`。
/// 单个来源失败只记 error，不影响其余来源（spec §4.2；审计 web-C3：住宅 9998 也要读）。
pub async fn sample_once(shared: &Shared) -> Sample;

/// 把一轮增量并进期望态（纯函数）：月度重置 → 累加 → `last_seen_at`。返回被改动的用户数。
pub fn apply_sample(state: &mut State, deltas: &BTreeMap<Uuid, TxRx>, now: time::OffsetDateTime) -> usize;
/// `user_id` 字符串表 → `Uuid` 表；认不出的键丢弃（并不报错：内核里可能还留着已删用户的计数器）
pub fn to_uuid_map(raw: &BTreeMap<String, TxRx>) -> BTreeMap<Uuid, TxRx>;
/// 用 state 把 `user_id` 键的表翻译成用户名键的表（`/api/stats`、`/api/online` 要用户名）
pub fn by_username<T: Copy>(state: &State, raw: &BTreeMap<Uuid, T>) -> BTreeMap<String, T>;

/// 一个完整周期：采样 → 累加到内存 → 到点落盘 → 限额执行（含 /kick）→ 刷新缓存。
pub async fn tick(ctx: &DaemonCtx, shared: &Shared) -> anyhow::Result<()>;
pub async fn sampling_loop(ctx: DaemonCtx, shared: Arc<Shared>);
/// 汇总进 `runtime.extra["users"]`（由 T8 的 `GET /api/users/health` 读出来；裁决 D2 不改 `/api/health`）
pub async fn write_health_summary(ctx: &DaemonCtx, shared: &Shared);
```

**落盘节奏**：`pending` 累加每一轮的增量；距上次落盘 ≥ `FLUSH_INTERVAL_SECS` 时，在一次 `store.update` 里把 `pending` 全部并进 `usage` 并清空。限额判定用 `usage + pending`，所以「还没落盘」不会让超限用户多跑 30 秒（`users::is_blocked` 的 `extra` 参数就是为它准备的）。

**已知降级（写进 M3 验收清单，P2 不额外做退出钩子）**：`pending` 只在内存里。`systemctl restart b-ui` / SIGTERM 时 P1 不给后台任务收尾机会（`spawn` 出来的 JoinHandle 随进程结束），所以**最多丢 30 秒的流量计数**，`SampleCache`（`/api/stats`、`/api/online`）与 `Applied`（xray 差分记账）也要下一轮重建。三者的代价都能接受：流量丢 30 秒不影响限额判定的正确性（只是少计），缓存与记账都是幂等重建。要消掉这 30 秒就得在 P1 的 `finish_self_restart` 或一个统一的 shutdown 信号上挂一次 flush —— 那是 P1 的改动面，**本计划不动**，只把它作为已知降级落字。

**在线判定**（spec §4.2）：两个 `/online` 的并集（值相加，同一用户可能同时连着直连与住宅）∪ 最近 30 秒有 Xray 增量的用户（Xray 没有连接数接口，有增量就按 1 计）。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/traffic.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{harness, Harness};
    use crate::modules::panel::users;
    use crate::reconcile::DaemonCtx;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn ctx_of(h: &Harness) -> DaemonCtx {
        DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        }
    }

    #[test]
    fn apply_sample_accumulates_total_and_monthly_and_sets_last_seen() {
        let mut s = sample_state();
        let id = s.users[0].user_id;
        s.users[0].usage = bui_schema::model::Usage {
            total_bytes: 100,
            monthly_bytes: 40,
            month_key: "2026-09".into(),
            last_seen_at: None,
        };
        let n = apply_sample(&mut s, &BTreeMap::from([(id, TxRx { tx: 3, rx: 4 })]), t0());
        assert_eq!(n, 1);
        assert_eq!(s.users[0].usage.total_bytes, 107);
        assert_eq!(s.users[0].usage.monthly_bytes, 47);
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(s.users[0].usage.last_seen_at.as_deref(), Some("2026-09-11T00:00:00Z"));
    }

    #[test]
    fn a_new_month_resets_monthly_but_not_total() {
        let mut s = sample_state();
        let id = s.users[0].user_id;
        s.users[0].usage = bui_schema::model::Usage {
            total_bytes: 100,
            monthly_bytes: 90,
            month_key: "2026-08".into(),
            last_seen_at: None,
        };
        apply_sample(&mut s, &BTreeMap::from([(id, TxRx { tx: 5, rx: 0 })]), t0());
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(s.users[0].usage.monthly_bytes, 5, "跨月先清零再加本轮");
        assert_eq!(s.users[0].usage.total_bytes, 105, "总量不清零");
    }

    #[test]
    fn a_month_rollover_with_no_traffic_still_resets() {
        // 月初第一轮采样可能一个字节都没有，`month_key` 也必须翻过去，
        // 否则 `is_blocked` 会拿上个月的用量继续判这个月的月度上限
        let mut s = sample_state();
        s.users[0].usage.month_key = "2026-08".into();
        s.users[0].usage.monthly_bytes = 999;
        let n = apply_sample(&mut s, &BTreeMap::new(), t0());
        assert_eq!(n, 1, "只是重置月份也算改动，要落盘");
        assert_eq!(s.users[0].usage.month_key, "2026-09");
        assert_eq!(s.users[0].usage.monthly_bytes, 0);
    }

    #[test]
    fn unknown_ids_are_dropped_and_usernames_are_resolved() {
        let s = sample_state();
        let id = s.users[0].user_id;
        let raw = BTreeMap::from([
            (id.to_string(), TxRx { tx: 1, rx: 2 }),
            ("not-a-uuid".to_string(), TxRx { tx: 9, rx: 9 }),
        ]);
        let m = to_uuid_map(&raw);
        assert_eq!(m, BTreeMap::from([(id, TxRx { tx: 1, rx: 2 })]));
        assert_eq!(by_username(&s, &m), BTreeMap::from([("alice".to_string(), TxRx { tx: 1, rx: 2 })]));
        // state 里没有的 user_id（刚删的用户，内核里计数器还在）也丢掉
        let ghost = BTreeMap::from([(uuid::Uuid::nil(), 1u32)]);
        assert!(by_username(&s, &ghost).is_empty());
    }

    #[tokio::test]
    async fn sample_once_reads_both_hysteria_ports_and_xray() {
        let h = harness().await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.traffic.insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 10, rx: 0 })]));
            i.traffic.insert(9998, BTreeMap::from([(id.clone(), TxRx { tx: 1, rx: 2 })]));
            i.online.insert(9999, BTreeMap::from([(id.clone(), 1u32)]));
            i.online.insert(9998, BTreeMap::from([(id.clone(), 2u32)]));
        });
        h.xray.with(|i| {
            i.deltas.insert(id.clone(), TxRx { tx: 0, rx: 100 });
        });
        let s = sample_once(&h.shared).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 11, rx: 102 }, "三个来源相加");
        assert_eq!(s.online[&id], 3, "两个 /online 的值相加");
        assert!(s.xray_ids.contains(&id));
        assert!(s.errors.is_empty());
        assert_eq!(
            h.hy2.calls(),
            vec![
                "traffic:9999".to_string(),
                "traffic:9998".to_string(),
                "online:9999".to_string(),
                "online:9998".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn one_dead_source_does_not_lose_the_others() {
        let h = harness().await;
        let id = h.store.read().await.users[0].user_id.to_string();
        h.hy2.with(|i| {
            i.fail_ports.insert(9998);
            i.traffic.insert(9999, BTreeMap::from([(id.clone(), TxRx { tx: 7, rx: 0 })]));
        });
        h.xray.with(|i| {
            i.fail_on.insert("query".into());
        });
        let s = sample_once(&h.shared).await;
        assert_eq!(s.deltas[&id], TxRx { tx: 7, rx: 0 });
        assert_eq!(s.errors.len(), 3, "住宅的 traffic 与 online 各一条 + Xray 一条：{:?}", s.errors);
        assert!(s.errors.iter().any(|e| e.contains("9998")));
    }

    #[tokio::test]
    async fn tick_holds_the_delta_in_memory_then_flushes_after_30s() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.hy2.with(|i| {
            i.traffic.insert(9999, BTreeMap::from([(id.to_string(), TxRx { tx: 5, rx: 5 })]));
        });
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(h.store.read().await.users[0].usage.total_bytes, 0, "第一轮只进内存");
        assert_eq!(h.shared.pending().await[&id], TxRx { tx: 5, rx: 5 });
        // 面板读的是缓存，缓存里已经有了
        assert_eq!(h.shared.cache().await.stats["alice"], TxRx { tx: 5, rx: 5 });

        h.hy2.with(|i| {
            i.traffic.insert(9999, BTreeMap::from([(id.to_string(), TxRx { tx: 1, rx: 0 })]));
        });
        h.host.advance(31);
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(h.store.read().await.users[0].usage.total_bytes, 11, "到点一次性落盘");
        assert!(h.shared.pending().await.is_empty());
        assert_eq!(h.shared.cache().await.stats["alice"], TxRx { tx: 6, rx: 5 }, "缓存是累计值");
    }

    #[tokio::test]
    async fn online_is_the_union_of_hysteria_and_recent_xray_traffic() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.xray.with(|i| {
            i.deltas.insert(id.to_string(), TxRx { tx: 1, rx: 0 });
        });
        tick(&ctx, &h.shared).await.unwrap();
        assert_eq!(h.shared.cache().await.online["alice"], 1, "只有 Xray 增量也算在线");
        // 30 秒窗口过去、又没有新增量 ⇒ 下线
        h.host.advance(31);
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!h.shared.cache().await.online.contains_key("alice"));
    }

    #[tokio::test]
    async fn exceeding_the_quota_kicks_both_instances_and_blocks_the_snapshot() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
            })
            .await
            .unwrap();
        h.hy2.with(|i| {
            i.traffic.insert(9999, BTreeMap::from([(id.to_string(), TxRx { tx: 20, rx: 0 })]));
        });
        tick(&ctx, &h.shared).await.unwrap();
        let snap = crate::modules::panel::snapshot::read(&h.shared.snapshot_path());
        assert!(snap.users["alice"].blocked, "快照拒绝是主保障（spec §4.2）");
        assert!(
            h.hy2.calls().contains(&format!("kick:9999:{id}")) && h.hy2.calls().contains(&format!("kick:9998:{id}")),
            "两个实例都要 kick：{:?}",
            h.hy2.calls()
        );
        assert!(h.xray.calls().iter().any(|c| c.starts_with(&format!("remove:vless-direct:{id}"))));
        // 第二轮不重复 kick
        h.hy2.clear_calls();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!h.hy2.calls().iter().any(|c| c.starts_with("kick:")));
    }

    #[tokio::test]
    async fn raising_the_quota_restores_the_user() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        let id = h.store.read().await.users[0].user_id;
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(10);
                s.users[0].usage.total_bytes = 50;
            })
            .await
            .unwrap();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked);
        h.xray.clear_calls();
        h.store
            .update(|s| {
                s.users[0].entitlements.traffic_limit.total_bytes = Some(100 * users::GIB);
            })
            .await
            .unwrap();
        tick(&ctx, &h.shared).await.unwrap();
        assert!(!crate::modules::panel::snapshot::read(&h.shared.snapshot_path()).users["alice"].blocked);
        assert!(
            h.xray.calls().iter().any(|c| c.starts_with(&format!("add:vless-direct:{id}"))),
            "恢复要反向做一次 AddUser：{:?}",
            h.xray.calls()
        );
    }

    #[tokio::test]
    async fn the_health_summary_lands_in_runtime_extra() {
        let h = harness().await;
        let ctx = ctx_of(&h);
        h.hy2.with(|i| {
            i.fail_ports.insert(9998);
        });
        tick(&ctx, &h.shared).await.unwrap();
        write_health_summary(&ctx, &h.shared).await;
        let v = h.runtime.read().await.extra["users"].clone();
        assert_eq!(v["total"], 1);
        assert_eq!(v["blocked"], 0);
        assert_eq!(v["disabled"], 0);
        assert_eq!(v["month_key"], "2026-09");
        assert_eq!(v["last_sample_at"], "2026-09-11T00:00:00Z");
        assert!(
            v["sample_errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("9998")),
            "采样错误要能在 `/api/users/health` 里看见：{v}"
        );
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::traffic`
Expected: 编译失败，`cannot find function 'apply_sample' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! 流量采样、用量累加、限额执行（spec §4.2）。
//!
//! 与 v3 的差别（审计 eff-C1~C4、web-C2~C5）：
//! - 一次 `QueryStats(pattern="user>>>", reset=true)` 拉全量，不再每用户两次 `execSync`（O(N) 阻塞）。
//! - 住宅实例的 `:9998` 也读（v3 的常量从没被用过）。
//! - `clear=1` / `reset=true` 让每轮拿到的就是增量，重启守护进程不会把累计值当增量重复计。
//! - 限额真的执行（v3 的 `checkUserLimits` 哪里都没被调用）。
//! - `/api/stats`、`/api/online` 读同一份缓存，面板开着不增加采样。

use super::users::{self, month_key};
use super::{Shared, HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI};
use super::TxRx;
use crate::reconcile::DaemonCtx;
use bui_schema::model::State;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

pub const SAMPLE_INTERVAL_SECS: u64 = 10;
pub const FLUSH_INTERVAL_SECS: i64 = 30;
pub const XRAY_ONLINE_WINDOW_SECS: i64 = 30;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub deltas: BTreeMap<String, TxRx>,
    pub online: BTreeMap<String, u32>,
    pub xray_ids: BTreeSet<String>,
    pub errors: Vec<String>,
}

pub async fn sample_once(shared: &Shared) -> Sample {
    let mut s = Sample::default();
    // 顺序固定（9999 → 9998 → online 9999 → online 9998 → Xray），测试按这个顺序断言 calls()
    for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
        match shared.hy2().traffic_clear(port).await {
            Ok(m) => {
                for (id, d) in m {
                    s.deltas.entry(id).or_default().add(d);
                }
            }
            Err(e) => s.errors.push(format!("hysteria :{port} /traffic 失败：{e}")),
        }
    }
    for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
        match shared.hy2().online(port).await {
            Ok(m) => {
                for (id, n) in m {
                    *s.online.entry(id).or_insert(0) += n;
                }
            }
            Err(e) => s.errors.push(format!("hysteria :{port} /online 失败：{e}")),
        }
    }
    match shared.xray().query_user_deltas().await {
        Ok(m) => {
            for (id, d) in m {
                s.xray_ids.insert(id.clone());
                s.deltas.entry(id).or_default().add(d);
            }
        }
        Err(e) => s.errors.push(format!("Xray QueryStats 失败：{e}")),
    }
    s
}

pub fn to_uuid_map(raw: &BTreeMap<String, TxRx>) -> BTreeMap<Uuid, TxRx> {
    raw.iter()
        .filter_map(|(k, v)| Uuid::parse_str(k).ok().map(|id| (id, *v)))
        .collect()
}

pub fn by_username<T: Copy>(state: &State, raw: &BTreeMap<Uuid, T>) -> BTreeMap<String, T> {
    let names: BTreeMap<Uuid, &str> =
        state.users.iter().map(|u| (u.user_id, u.username.as_str())).collect();
    raw.iter()
        .filter_map(|(id, v)| names.get(id).map(|n| (n.to_string(), *v)))
        .collect()
}

pub fn apply_sample(
    state: &mut State,
    deltas: &BTreeMap<Uuid, TxRx>,
    now: OffsetDateTime,
) -> usize {
    let mk = month_key(now);
    let ts = crate::util::fmt_rfc3339(now);
    let mut changed = 0;
    for u in state.users.iter_mut() {
        let d = deltas.get(&u.user_id).copied().unwrap_or_default();
        // 月度重置：即使本轮没有流量也要翻月份，否则 `is_blocked` 会拿上个月的用量判这个月
        let rolled = u.usage.month_key != mk;
        if !rolled && d.is_zero() {
            continue;
        }
        if rolled {
            u.usage.month_key = mk.clone();
            u.usage.monthly_bytes = 0;
        }
        let total = d.total();
        if total > 0 {
            u.usage.total_bytes = u.usage.total_bytes.saturating_add(total);
            u.usage.monthly_bytes = u.usage.monthly_bytes.saturating_add(total);
            u.usage.last_seen_at = Some(ts.clone());
        }
        changed += 1;
    }
    changed
}

pub async fn tick(ctx: &DaemonCtx, shared: &Shared) -> anyhow::Result<()> {
    let now = ctx.host.now();
    let sample = sample_once(shared).await;
    let deltas = to_uuid_map(&sample.deltas);

    // ① 内存累加
    {
        let mut pending = shared.pending().await;
        for (id, d) in &deltas {
            pending.entry(*id).or_default().add(*d);
        }
    }
    // ② Xray 在线窗口
    {
        let mut seen = shared.xray_seen().await;
        for id in &sample.xray_ids {
            if let Ok(uid) = Uuid::parse_str(id) {
                seen.insert(uid, now);
            }
        }
        seen.retain(|_, t| (now - *t).whole_seconds() < XRAY_ONLINE_WINDOW_SECS);
    }

    // ③ 到点落盘（spec §4.2「最多每 30 秒合并落盘一次」）
    let due = {
        let cache = shared.cache().await;
        match cache.last_flush_at {
            Some(t) => (now - t).whole_seconds() >= FLUSH_INTERVAL_SECS,
            None => false,
        }
    };
    if due {
        let pending = std::mem::take(&mut *shared.pending().await);
        ctx.store.update(|s| {
            apply_sample(s, &pending, now);
        })
        .await?;
    }

    // ④ 限额执行与恢复（快照拒绝 + Xray 增删 + kick）
    let out = users::sync_now(ctx, shared).await;
    if !out.newly_blocked.is_empty() {
        let ids: Vec<String> = out.newly_blocked.iter().map(Uuid::to_string).collect();
        for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
            if let Err(e) = shared.hy2().kick(port, &ids).await {
                tracing::warn!(port, error = %e, "kick 失败；快照拒绝仍然生效");
            }
        }
    }

    // ⑤ 刷新面板缓存
    let state = ctx.store.read().await;
    let mut online: BTreeMap<Uuid, u32> = BTreeMap::new();
    for (id, n) in &sample.online {
        if let Ok(uid) = Uuid::parse_str(id) {
            *online.entry(uid).or_insert(0) += *n;
        }
    }
    for uid in shared.xray_seen().await.keys() {
        online.entry(*uid).or_insert(1);
    }
    let mut cache = shared.cache_mut().await;
    for (id, d) in &deltas {
        if let Some(name) = state.users.iter().find(|u| u.user_id == *id).map(|u| u.username.clone()) {
            cache.stats.entry(name).or_default().add(*d);
        }
    }
    cache.online = by_username(&state, &online);
    cache.last_sample_at = Some(crate::util::fmt_rfc3339(now));
    if due || cache.last_flush_at.is_none() {
        cache.last_flush_at = Some(now);
    }
    cache.errors = sample.errors;
    Ok(())
}

pub async fn write_health_summary(ctx: &DaemonCtx, shared: &Shared) {
    let now = ctx.host.now();
    let state = ctx.store.read().await;
    let pending = shared.pending().await.clone();
    let blocked = users::blocked_set(&state, &pending, now);
    let cache = shared.cache().await;
    let applied = shared.applied().await;
    let summary = serde_json::json!({
        "total": state.users.len(),
        "disabled": state.users.iter().filter(|u| u.disabled).count(),
        "blocked": blocked.len(),
        "online": cache.online.len(),
        "xray_synced": applied.xray_users.len(),
        "month_key": month_key(now),
        "last_sample_at": cache.last_sample_at,
        "pending_users": pending.len(),
        "sample_errors": cache.errors,
    });
    ctx.runtime
        .update(|r| {
            r.extra.insert("users".into(), summary);
        })
        .await;
}

pub async fn sampling_loop(ctx: DaemonCtx, shared: Arc<Shared>) {
    let mut iv = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        if let Err(e) = tick(&ctx, &shared).await {
            tracing::warn!(error = %e, "采样周期失败");
        }
        write_health_summary(&ctx, &shared).await;
    }
}
```

**`SampleCache::last_flush_at` 的语义**（字段由 Task 1 定义）：`tick` 第一次跑时它是 `None`，于是第一轮只把增量放进内存、并把 `last_flush_at` 设成当轮时间；第 31 秒起才开始落盘 —— 与 spec §4.2「最多每 30 秒合并落盘一次」一致，也让 `tick_holds_the_delta_in_memory_then_flushes_after_30s` 这条测试成立。

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::traffic && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 11 passed，clippy 无输出。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/traffic.rs
git commit -m "feat(panel): 10 秒采样、30 秒落盘、月度重置与限额执行"
```

---

### Task 8: 管理员 API（契约沿用 v3 + spec §4.3 的四处改动）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/api_admin.rs`

**前置**：Task 6（`users::{PanelUser, CreateRequest, UpdateRequest, new_user, apply_update, project_all, blocked_set}`）。

**Interfaces:**
- Consumes: `crate::modules::panel::{Shared, HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI}`、`crate::modules::panel::users`、`crate::api::{AppState, Event}`、`crate::api::auth::hash_password`、`crate::state::runtime::WatchdogRecord`、`bui_schema::model::State`
- Produces:
```rust
/// 全部端点都要求 JWT（由 P1 的 `require_admin` 统一拦），路径与响应形状照 v3
pub fn routes(shared: Arc<Shared>) -> axum::Router<AppState>;

/// `GET /api/config` 的响应（形状逐字段照 v3 `getConfig`，`web/server.js:392-489`）
pub fn config_payload(s: &State) -> serde_json::Value;
/// `GET /api/masquerade` 的响应
pub fn masquerade_payload(s: &State) -> serde_json::Value;
/// 从 `https://host:port/path` 取主机名（移植 v3 的 `replace(/https?:\/\/([^/:]+).*/, "$1")`）
pub fn host_of(url: &str) -> String;
/// `runtime.watchdog` → v3 `/api/hy2/watchdog/status` 的形状（兼容 shim，见契约表 #21）
pub fn watchdog_payload(w: &BTreeMap<String, WatchdogRecord>) -> serde_json::Value;
/// `GET /api/users/health` 的响应（**v4 新增**，裁决 D2）：把 T7 写进 `runtime.extra["users"]` 的
/// 用户与流量摘要原样发出去；还没采样过时给 `{}`（`api/health.rs` 一个字都不改）
pub fn users_health_payload(rt: &crate::state::runtime::RuntimeData) -> serde_json::Value;
```

挂载的路由（与「v3 端点逐个契约」表逐行对应）：

| 方法与路径 | 处理 |
|---|---|
| `GET /api/users` | `users::project_all(&state, &blocked)`，`blocked` 由 `users::blocked_set(state, pending, now)` 现算 |
| `POST /api/users` | `users::new_user` → 查重 → 入 `state.users` → 发 `Event::StateChanged("users")` → `{"success":true,"user","password","uuid","sni"}` |
| `PUT /api/users/{username}` | `users::apply_update`（改在副本上，成功才写回）→ 发事件 → `{"success":true,"user"}` |
| `DELETE /api/users/{username}` | 从 `state.users` 移除 → 发事件 → `{"success":true}` |
| `GET /api/stats` | `shared.cache().stats`（用户名 → `{tx,rx}`） |
| `GET /api/online` | `shared.cache().online`（用户名 → 连接数） |
| `POST /api/kick` | 体是用户名数组 → 翻成 `user_id` → 两个实例各 `POST /kick` |
| `GET /api/config` | `config_payload` |
| `POST /api/password` | argon2id 重哈希 + 轮换 `jwt_secret`（决策 D11） |
| `GET/POST /api/masquerade` | 读写 `state.node.reality.{dest,server_names}` + 发事件 |
| `GET/POST /api/bandwidth` | GET 恒 `{"up":0,"down":0}`；POST `501`（决策 D5） |
| `GET/POST /api/port-hopping` | 读写 `state.node.ports.hy2_hop` + 发事件（不再碰 iptables，审计 web-C13） |
| `GET /api/hy2/watchdog/status` | `watchdog_payload(&runtime.watchdog)` |
| `GET /api/users/health` | `users_health_payload(&runtime.read().await)`（**v4 新增**，裁决 D2：`/api/health` 不加用户段） |

**为什么改完 state 只发事件、不在 handler 里同步内核**：spec §4.1「state 变更 → 快照重写 + gRPC 增删」的两件事都由 Task 6 的同步反应器做，它订阅 `EventBus`、幂等、还带 60 秒安全网；handler 里再做一遍会出现两条并发路径抢 `applied` 锁。同一个事件也会触发 P1 的去抖对账，把新 `clients` 写进 `xray-config.json`（结构哈希不变 ⇒ 不重启 xray）。

**请求体一律手工解析**：v3 对坏 JSON 回 `400 {"error":"请求格式错误（JSON 解析失败）"}`，而 axum 的 `Json<T>` 提取器会回 `422` 和自己的文案。旧前端只看 `r.error`，但「形状照 v3」是硬约束，所以 handler 收 `axum::body::Bytes` 自己解析。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/api_admin.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, send, token, Harness};
    use crate::modules::panel::TxRx;
    use pretty_assertions::assert_eq;

    async fn app(h: &Harness) -> (axum::Router, String) {
        (mount(&h.app, routes(h.shared.clone())), token(h).await)
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(host_of("https://www.bing.com/"), "www.bing.com");
        assert_eq!(host_of("http://www.bing.com"), "www.bing.com");
        assert_eq!(host_of("https://a.example.com:8443/x?y=1"), "a.example.com");
        assert_eq!(host_of("www.bing.com"), "www.bing.com");
        assert_eq!(host_of(""), "");
    }

    #[tokio::test]
    async fn config_matches_the_v3_shape() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/config", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["domain"], "example.com");
        assert_eq!(v["port"], "10000", "v3 的 port 是字符串");
        assert_eq!(v["xrayPort"], 10001);
        assert_eq!(v["pubKey"], "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c");
        assert_eq!(v["shortId"], "0123456789abcdef");
        assert_eq!(v["sni"], "www.bing.com");
        assert_eq!(v["portHopping"], serde_json::json!({"enabled": true, "start": 20000, "end": 30000}));
        assert_eq!(v["obfs"], serde_json::json!({"enabled": false, "type": "", "password": ""}));
    }

    #[tokio::test]
    async fn users_list_is_the_v3_projection() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/users", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["username"], "alice");
        assert_eq!(arr[0]["protocol"], "fusion");
        assert_eq!(arr[0]["usage"]["total"], 0);
    }

    #[tokio::test]
    async fn creating_a_user_uses_post_with_jwt_and_emits_a_state_change() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let mut rx = h.app.bus.subscribe();
        let (s, v) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({
                "username": "bob", "days": 30, "traffic": 1.5, "monthly": 0,
                "protocol": "fusion", "residential": true, "sni": "ignored", "speed": 100
            })),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(v["user"], "bob");
        assert_eq!(v["password"].as_str().unwrap().len(), 16);
        assert_eq!(v["sni"], "www.bing.com", "回显真正生效的 SNI，不是请求里那个（决策 D13）");
        assert!(v["uuid"].as_str().is_some());
        assert_eq!(h.store.read().await.users.len(), 2);
        assert_eq!(rx.try_recv().unwrap(), crate::api::Event::StateChanged("users"));
    }

    #[tokio::test]
    async fn create_rejects_duplicates_bad_names_bad_protocols_and_broken_json() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "POST", "/api/users", Some(&t), Some(serde_json::json!({"username": "alice"}))).await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "用户名已存在");
        let (s2, v2) = send(&r, "POST", "/api/users", Some(&t), Some(serde_json::json!({"username": "a/b"}))).await;
        assert_eq!(s2, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v2["error"], "username 仅允许字母/数字/中文/下划线/连字符/点");
        let (s3, _) = send(
            &r,
            "POST",
            "/api/users",
            Some(&t),
            Some(serde_json::json!({"username": "c", "protocol": "vless-ws-tls"})),
        )
        .await;
        assert_eq!(s3, axum::http::StatusCode::BAD_REQUEST);
        // 空 / 坏请求体要回 v3 的 400 文案，不是 axum 默认的 422
        let (s4, v4) = send(&r, "POST", "/api/users", Some(&t), None).await;
        assert_eq!(s4, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v4["error"], "请求格式错误（JSON 解析失败）");
        assert_eq!(h.store.read().await.users.len(), 1, "失败请求不能改 state");
    }

    #[tokio::test]
    async fn update_and_delete_follow_the_v3_contract() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(
            &r,
            "PUT",
            "/api/users/alice",
            Some(&t),
            Some(serde_json::json!({"username": "alice2", "days": 10, "traffic": 2, "monthly": 0, "speed": 50})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"success": true, "user": "alice2"}));
        assert_eq!(h.store.read().await.users[0].username, "alice2");
        let (s404, v404) = send(&r, "PUT", "/api/users/nope", Some(&t), Some(serde_json::json!({}))).await;
        assert_eq!(s404, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(v404["error"], "User not found");
        let (sd, vd) = send(&r, "DELETE", "/api/users/alice2", Some(&t), None).await;
        assert_eq!(sd, axum::http::StatusCode::OK);
        assert_eq!(vd, serde_json::json!({"success": true}));
        assert!(h.store.read().await.users.is_empty());
        assert_eq!(
            send(&r, "DELETE", "/api/users/alice2", Some(&t), None).await.0,
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn renaming_onto_an_existing_username_is_rejected_and_changes_nothing() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        send(&r, "POST", "/api/users", Some(&t), Some(serde_json::json!({"username": "bob"}))).await;
        let (s, v) = send(&r, "PUT", "/api/users/bob", Some(&t), Some(serde_json::json!({"username": "alice"}))).await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "Username already exists");
        let names: Vec<String> = h.store.read().await.users.iter().map(|u| u.username.clone()).collect();
        assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[tokio::test]
    async fn stats_and_online_come_from_the_shared_cache() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        {
            let mut c = h.shared.cache_mut().await;
            c.stats.insert("alice".into(), TxRx { tx: 7, rx: 8 });
            c.online.insert("alice".into(), 2);
        }
        let (_, v) = send(&r, "GET", "/api/stats", Some(&t), None).await;
        assert_eq!(v, serde_json::json!({"alice": {"tx": 7, "rx": 8}}));
        let (_, o) = send(&r, "GET", "/api/online", Some(&t), None).await;
        assert_eq!(o, serde_json::json!({"alice": 2}));
    }

    #[tokio::test]
    async fn kick_translates_usernames_into_user_ids_for_both_instances() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let id = h.store.read().await.users[0].user_id;
        let (s, v) = send(&r, "POST", "/api/kick", Some(&t), Some(serde_json::json!(["alice", "ghost"]))).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"success": true, "kicked": 1}));
        assert_eq!(
            h.hy2.calls(),
            vec![format!("kick:9999:{id}"), format!("kick:9998:{id}")]
        );
    }

    #[tokio::test]
    async fn changing_the_admin_password_rehashes_and_rotates_the_jwt_secret() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let before = h.store.read().await.admin.jwt_secret.clone();
        let (s, v) = send(&r, "POST", "/api/password", Some(&t), Some(serde_json::json!({"newPassword": "short"}))).await;
        assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["error"], "密码至少6位");
        let (s2, v2) = send(&r, "POST", "/api/password", Some(&t), Some(serde_json::json!({"newPassword": "newpass123"}))).await;
        assert_eq!(s2, axum::http::StatusCode::OK);
        assert_eq!(v2, serde_json::json!({"success": true, "message": "密码已更新，请重新登录"}));
        let st = h.store.read().await;
        assert!(crate::api::auth::verify_password(&st.admin.password_hash, "newpass123"));
        assert_ne!(st.admin.jwt_secret, before, "决策 D11：旧 token 立即失效");
    }

    #[tokio::test]
    async fn masquerade_reads_and_writes_the_reality_dest() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (_, v) = send(&r, "GET", "/api/masquerade", Some(&t), None).await;
        assert_eq!(
            v,
            serde_json::json!({"masqueradeUrl": "https://www.bing.com/", "masqueradeDomain": "www.bing.com"})
        );
        let mut rx = h.app.bus.subscribe();
        let (s, out) = send(
            &r,
            "POST",
            "/api/masquerade",
            Some(&t),
            Some(serde_json::json!({"url": "https://www.apple.com/"})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(out, serde_json::json!({"success": true, "domain": "www.apple.com"}));
        let st = h.store.read().await;
        assert_eq!(st.node.reality.dest, "www.apple.com:443");
        assert_eq!(st.node.reality.server_names, vec!["www.apple.com".to_string()]);
        assert_eq!(rx.try_recv().unwrap(), crate::api::Event::StateChanged("masquerade"));
        let (bad, _) = send(&r, "POST", "/api/masquerade", Some(&t), Some(serde_json::json!({}))).await;
        assert_eq!(bad, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn port_hopping_writes_the_listen_range_instead_of_iptables() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (_, v) = send(&r, "GET", "/api/port-hopping", Some(&t), None).await;
        assert_eq!(v, serde_json::json!({"enabled": true, "start": 20000, "end": 30000}));
        let mut rx = h.app.bus.subscribe();
        let (s, out) = send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": true, "start": 21000, "end": 22000})),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(out, serde_json::json!({"success": true, "enabled": true, "start": 21000, "end": 22000}));
        assert_eq!(h.store.read().await.node.ports.hy2_hop, Some((21000, 22000)));
        assert_eq!(rx.try_recv().unwrap(), crate::api::Event::StateChanged("ports"));
        // 关掉 = 清空区间（对账会把 `listen:` 写回单端口并重启 hysteria-server）
        send(&r, "POST", "/api/port-hopping", Some(&t), Some(serde_json::json!({"enabled": false}))).await;
        assert_eq!(h.store.read().await.node.ports.hy2_hop, None);
        assert_eq!(rx.try_recv().unwrap(), crate::api::Event::StateChanged("ports"));
        // 再关一次：值没变 ⇒ 不发事件。否则「关掉→关掉」会白触发一轮对账，
        // 而对账认为 `listen:` 要改时会重启 hysteria-server、踢掉所有在线连接。
        let (again, _) = send(&r, "POST", "/api/port-hopping", Some(&t), Some(serde_json::json!({"enabled": false}))).await;
        assert_eq!(again, axum::http::StatusCode::OK, "幂等请求仍然回 200");
        assert!(rx.try_recv().is_err(), "值没变还发事件 ⇒ 白重启一次 hysteria-server");
        // 起点不小于终点直接拒绝
        let (bad, bv) = send(
            &r,
            "POST",
            "/api/port-hopping",
            Some(&t),
            Some(serde_json::json!({"enabled": true, "start": 30000, "end": 20000})),
        )
        .await;
        assert_eq!(bad, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(bv["error"], "起始端口必须小于结束端口");
        assert!(h.host.ops().iter().all(|o| !o.starts_with("run:iptables")), "v4 不写任何 iptables 规则");
    }

    #[tokio::test]
    async fn bandwidth_is_read_only_in_v4() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        let (s, v) = send(&r, "GET", "/api/bandwidth", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"up": 0, "down": 0}));
        let (s2, v2) = send(&r, "POST", "/api/bandwidth", Some(&t), Some(serde_json::json!({"up": 100, "down": 100}))).await;
        assert_eq!(s2, axum::http::StatusCode::NOT_IMPLEMENTED);
        assert!(v2["error"].as_str().unwrap().contains("ignoreClientBandwidth"), "{v2}");
    }

    #[tokio::test]
    async fn the_watchdog_shim_keeps_the_v3_keys() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        h.runtime
            .update(|rt| {
                rt.watchdog.insert(
                    "hysteria-server".into(),
                    crate::state::runtime::WatchdogRecord {
                        fails: 2,
                        restarts: 1,
                        last_restart_at: Some("2026-09-11T00:00:00Z".into()),
                        backoff_until: None,
                    },
                );
            })
            .await;
        let (s, v) = send(&r, "GET", "/api/hy2/watchdog/status", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["watchdog_active"], true);
        assert_eq!(v["next_run_at"], serde_json::Value::Null);
        assert_eq!(v["last_run_at"], "2026-09-11T00:00:00Z");
        assert_eq!(v["fail_count"], 2);
        let lines = v["log_recent_lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].as_str().unwrap().contains("hysteria-server"), "{lines:?}");
    }

    #[tokio::test]
    async fn the_user_health_endpoint_serves_the_runtime_summary() {
        let h = harness().await;
        let (r, t) = app(&h).await;
        // 还没采样过：空对象，不是 404、也不是 null（裁决 D2：摘要只在这条端点上）
        let (s0, v0) = send(&r, "GET", "/api/users/health", Some(&t), None).await;
        assert_eq!(s0, axum::http::StatusCode::OK);
        assert_eq!(v0, serde_json::json!({}));
        h.runtime
            .update(|rt| {
                rt.extra
                    .insert("users".into(), serde_json::json!({"total": 2, "blocked": 1}));
            })
            .await;
        let (s, v) = send(&r, "GET", "/api/users/health", Some(&t), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v, serde_json::json!({"total": 2, "blocked": 1}));
    }

    #[tokio::test]
    async fn every_admin_endpoint_is_401_without_a_token() {
        let h = harness().await;
        // 必须用 full_with 挂**本文件的** routes()：`h.router()` 挂的是 PanelModule，
        // 而它的 routes() 要到 T13 才接线，这里会一律拿到 404 而不是 401。
        let router = full_with(&h.app, routes(h.shared.clone()), axum::Router::new());
        for (m, p) in [
            ("GET", "/api/users"),
            ("POST", "/api/users"),
            ("GET", "/api/stats"),
            ("GET", "/api/online"),
            ("POST", "/api/kick"),
            ("GET", "/api/config"),
            ("POST", "/api/password"),
            ("GET", "/api/masquerade"),
            ("GET", "/api/bandwidth"),
            ("GET", "/api/port-hopping"),
            ("GET", "/api/hy2/watchdog/status"),
            ("GET", "/api/users/health"),
        ] {
            let (s, _) = send(&router, m, p, None, None).await;
            assert_eq!(s, axum::http::StatusCode::UNAUTHORIZED, "{m} {p} 居然不需要鉴权");
        }
    }

    #[tokio::test]
    async fn the_deleted_v3_endpoints_are_gone() {
        let h = harness().await;
        // 同上：挂本文件的 routes()，确认这些路径**在管理员这棵子树里**确实不存在
        let router = full_with(&h.app, routes(h.shared.clone()), axum::Router::new());
        let t = token(&h).await;
        for p in [
            "/api/manage?key=x&action=list",
            "/api/version",
            "/api/kernel-versions",
            "/api/kernel-downloads",
            "/packages",
            "/install-client?key=x",
        ] {
            let (s, _) = send(&router, "GET", p, Some(&t), None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "{p} 应该已被删除");
        }
        let (s, _) = send(&router, "POST", "/auth/hysteria", None, Some(serde_json::json!({"auth": "a:b"}))).await;
        assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "/auth/hysteria 已由 auth-hook 取代");
    }
}
```
- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::api_admin`
Expected: 编译失败，`cannot find function 'routes' in this scope`。

- [ ] **Step 3: 实现公共小工具与三个 v3 形状 payload**（v4 新增的 `users_health_payload` 与它的 handler 一起写在 Step 4）

```rust
//! 管理员 API（spec §4.3）。路径与响应形状沿用 v3（`web/app.js` 不改），四处改动见计划的契约表。
//!
//! 所有 handler 只改 `state` 并发 `Event::StateChanged(..)`：快照重写与 Xray gRPC 差分由
//! `users::sync_loop` 统一做（幂等 + 60 秒安全网），内核配置重渲染与重启映射由 P1 的对账做。

use super::users;
use super::{Shared, HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI};
use crate::api::{AppState, Event};
use crate::state::runtime::WatchdogRecord;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::model::State as BuiState;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

fn ok_json<T: Serialize>(v: T) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

fn fail(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

/// v3 对坏请求体回 `400 {"error":"请求格式错误（JSON 解析失败）"}`（`web/server.js:1948-1955`），
/// 而 axum 的 `Json<T>` 提取器回 422 + 自己的文案，所以这里手工解析。
fn parse_json<T: DeserializeOwned>(body: &Bytes) -> Result<T, Response> {
    if body.is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "请求格式错误（JSON 解析失败）"));
    }
    serde_json::from_slice(body)
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "请求格式错误（JSON 解析失败）"))
}

pub fn config_payload(s: &BuiState) -> serde_json::Value {
    let ph = match s.node.ports.hy2_hop {
        Some((start, end)) => json!({"enabled": true, "start": start, "end": end}),
        // v3 在没有区间时给的也是这对默认值（`getConfig` 的 portHopping 初值）
        None => json!({"enabled": false, "start": 20000, "end": 30000}),
    };
    json!({
        "domain": s.node.domain,
        // v3 的 `port` 来自正则捕获，是**字符串**；前端直接拼进 URL，这里保持同型
        "port": s.node.ports.hy2.to_string(),
        "xrayPort": s.node.ports.reality_direct,
        "pubKey": s.node.reality.public_key,
        "shortId": s.node.reality.short_id(),
        "sni": s.node.reality.sni(),
        "portHopping": ph,
        "obfs": {
            "enabled": s.node.obfs.enabled,
            "type": if s.node.obfs.enabled { "salamander" } else { "" },
            "password": s.node.obfs.password,
        },
    })
}

pub fn masquerade_payload(s: &BuiState) -> serde_json::Value {
    let d = s.node.reality.sni();
    json!({"masqueradeUrl": format!("https://{d}/"), "masqueradeDomain": d})
}

/// 移植 v3 的 `b.url.replace(/https?:\/\/([^/:]+).*/, "$1")`（`web/server.js:2588`）。
pub fn host_of(url: &str) -> String {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    rest.split(['/', ':']).next().unwrap_or("").to_string()
}

pub fn watchdog_payload(w: &BTreeMap<String, WatchdogRecord>) -> serde_json::Value {
    let fail_count: u32 = w.values().map(|r| r.fails).sum();
    let last_run_at = w.values().filter_map(|r| r.last_restart_at.clone()).max();
    let mut lines: Vec<String> = w
        .iter()
        .filter_map(|(unit, r)| {
            r.last_restart_at
                .as_ref()
                .map(|t| format!("{t} {unit} 已重启 {} 次（连续失败 {}）", r.restarts, r.fails))
        })
        .collect();
    lines.sort();
    lines.reverse();
    lines.truncate(5);
    json!({
        // v4 的 watchdog 是守护进程内的常驻任务，没有 timer；只要面板答得出这一问，它就在跑
        "watchdog_active": true,
        "next_run_at": serde_json::Value::Null,
        "last_run_at": last_run_at,
        "fail_count": fail_count,
        "log_recent_lines": lines,
    })
}
```

- [ ] **Step 4: 实现各 handler 与 `routes()`**

```rust
async fn blocked_now(app: &AppState, shared: &Shared) -> std::collections::BTreeSet<uuid::Uuid> {
    let now = app.host.now();
    let pending = shared.pending().await.clone();
    let state = app.store.read().await;
    users::blocked_set(&state, &pending, now)
}

async fn list_users(State(app): State<AppState>, shared: Arc<Shared>) -> Response {
    let blocked = blocked_now(&app, &shared).await;
    let state = app.store.read().await;
    ok_json(users::project_all(&state, &blocked))
}

async fn create_user(State(app): State<AppState>, body: Bytes) -> Response {
    let req: users::CreateRequest = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let user = match users::new_user(&req, app.host.now()) {
        Ok(u) => u,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    let mut dup = false;
    let to_push = user.clone();
    if let Err(e) = app
        .store
        .update(|s| {
            if s.users.iter().any(|u| u.username == to_push.username) {
                dup = true;
                return;
            }
            s.users.push(to_push);
        })
        .await
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
    }
    if dup {
        return fail(StatusCode::BAD_REQUEST, "用户名已存在");
    }
    app.bus.send(Event::StateChanged("users"));
    let sni = app.store.read().await.node.reality.sni().to_string();
    ok_json(json!({
        "success": true,
        "user": user.username,
        "password": user.credentials.hy2_password,
        "uuid": user.credentials.vless_uuid,
        // 回显真正生效的 SNI（v4 全局唯一），请求里的 `sni` 被忽略（决策 D13）
        "sni": sni,
    }))
}

async fn update_user(
    State(app): State<AppState>,
    Path(username): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(e) = users::validate_username(&username) {
        return fail(StatusCode::BAD_REQUEST, format!("URL 中的 {e}"));
    }
    let req: users::UpdateRequest = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let now = app.host.now();
    let mut missing = false;
    let mut problem: Option<String> = None;
    let mut new_name = username.clone();
    if let Err(e) = app
        .store
        .update(|s| {
            if let Some(n) = &req.username {
                if n != &username && s.users.iter().any(|u| &u.username == n) {
                    problem = Some("Username already exists".into());
                    return;
                }
            }
            match s.users.iter().position(|u| u.username == username) {
                None => missing = true,
                Some(i) => {
                    // 改在副本上，成功才写回：`apply_update` 中途报错不能留下半改的用户
                    let mut copy = s.users[i].clone();
                    match users::apply_update(&mut copy, &req, now) {
                        Ok(()) => {
                            new_name = copy.username.clone();
                            s.users[i] = copy;
                        }
                        Err(e) => problem = Some(e),
                    }
                }
            }
        })
        .await
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
    }
    if missing {
        return fail(StatusCode::NOT_FOUND, "User not found");
    }
    if let Some(e) = problem {
        return fail(StatusCode::BAD_REQUEST, e);
    }
    app.bus.send(Event::StateChanged("users"));
    ok_json(json!({"success": true, "user": new_name}))
}

async fn delete_user(State(app): State<AppState>, Path(username): Path<String>) -> Response {
    let mut removed = false;
    if let Err(e) = app
        .store
        .update(|s| {
            let before = s.users.len();
            s.users.retain(|u| u.username != username);
            removed = s.users.len() != before;
        })
        .await
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
    }
    if !removed {
        return fail(StatusCode::NOT_FOUND, "User not found");
    }
    app.bus.send(Event::StateChanged("users"));
    ok_json(json!({"success": true}))
}

async fn get_stats(shared: Arc<Shared>) -> Response {
    ok_json(shared.cache().await.stats.clone())
}

async fn get_online(shared: Arc<Shared>) -> Response {
    ok_json(shared.cache().await.online.clone())
}

async fn kick(State(app): State<AppState>, shared: Arc<Shared>, body: Bytes) -> Response {
    let names: Vec<String> = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ids: Vec<String> = {
        let state = app.store.read().await;
        state
            .users
            .iter()
            .filter(|u| names.contains(&u.username))
            .map(|u| u.user_id.to_string())
            .collect()
    };
    let mut all_ok = true;
    for port in [HY2_STATS_PORT_DIRECT, HY2_STATS_PORT_RESI] {
        if let Err(e) = shared.hy2().kick(port, &ids).await {
            tracing::warn!(port, error = %e, "kick 失败");
            all_ok = false;
        }
    }
    ok_json(json!({"success": all_ok, "kicked": ids.len()}))
}

async fn get_config(State(app): State<AppState>) -> Response {
    ok_json(config_payload(&app.store.read().await))
}

async fn set_password(State(app): State<AppState>, body: Bytes) -> Response {
    let v: serde_json::Value = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let pw = v.get("newPassword").and_then(|x| x.as_str()).unwrap_or("");
    if pw.chars().count() < 6 {
        return fail(StatusCode::BAD_REQUEST, "密码至少6位");
    }
    let hash = match crate::api::auth::hash_password(pw) {
        Ok(h) => h,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("哈希失败：{e}")),
    };
    // 决策 D11：轮换 JWT 密钥，让旧 token 立即失效（v3 靠重启进程达到同样效果）
    let secret = hex::encode(rand::random::<[u8; 32]>());
    tracing::info!(password = %crate::redact::secret(pw), "管理员密码已更新并轮换 JWT 密钥");
    if let Err(e) = app
        .store
        .update(|s| {
            s.admin.password_hash = hash;
            s.admin.jwt_secret = secret;
        })
        .await
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
    }
    ok_json(json!({"success": true, "message": "密码已更新，请重新登录"}))
}

async fn get_masquerade(State(app): State<AppState>) -> Response {
    ok_json(masquerade_payload(&app.store.read().await))
}

async fn set_masquerade(State(app): State<AppState>, body: Bytes) -> Response {
    let v: serde_json::Value = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(url) = v.get("url").and_then(|x| x.as_str()) else {
        return fail(StatusCode::BAD_REQUEST, "URL required");
    };
    let domain = host_of(url);
    if domain.is_empty() || !domain.contains('.') {
        return fail(StatusCode::BAD_REQUEST, "URL required");
    }
    let d = domain.clone();
    if let Err(e) = app
        .store
        .update(|s| {
            s.node.reality.dest = format!("{d}:443");
            s.node.reality.server_names = vec![d];
        })
        .await
    {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
    }
    // 对账会重写 xray-config.json（结构哈希变 ⇒ 重启 xray）与两份 hysteria 配置
    // （`masquerade.proxy.url` 由 `reality.sni()` 推导 ⇒ 重启两个 hysteria）
    app.bus.send(Event::StateChanged("masquerade"));
    ok_json(json!({"success": true, "domain": domain}))
}

async fn get_bandwidth() -> Response {
    // 决策 D5：v4 的 hysteria 配置固定 `ignoreClientBandwidth: true`，没有服务端带宽设置
    ok_json(json!({"up": 0, "down": 0}))
}

async fn set_bandwidth() -> Response {
    fail(
        StatusCode::NOT_IMPLEMENTED,
        "v4 不再设置服务端带宽：config.yaml 固定 ignoreClientBandwidth: true",
    )
}

async fn get_port_hopping(State(app): State<AppState>) -> Response {
    let s = app.store.read().await;
    match s.node.ports.hy2_hop {
        Some((start, end)) => ok_json(json!({"enabled": true, "start": start, "end": end})),
        None => ok_json(json!({"enabled": false, "start": 20000, "end": 30000})),
    }
}

async fn set_port_hopping(State(app): State<AppState>, body: Bytes) -> Response {
    let v: serde_json::Value = match parse_json(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
    let start = v.get("start").and_then(|x| x.as_u64()).unwrap_or(20000) as u16;
    let end = v.get("end").and_then(|x| x.as_u64()).unwrap_or(30000) as u16;
    if enabled && start >= end {
        return fail(StatusCode::BAD_REQUEST, "起始端口必须小于结束端口");
    }
    let want = if enabled { Some((start, end)) } else { None };
    // 值没变就不发事件：`StateChanged("ports")` 会触发一轮去抖对账，而对账把 `listen:` 行
    // 判成「要改」时会**重启 hysteria-server**。前端的「关掉→再关掉」不该踢掉所有连接。
    let changed = app.store.read().await.node.ports.hy2_hop != want;
    if changed {
        if let Err(e) = app.store.update(|s| s.node.ports.hy2_hop = want).await {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}"));
        }
        // 审计 web-C13：v3 在这里写 iptables REDIRECT；v4 改 `listen:` 行 + 重启，零 iptables 规则
        app.bus.send(Event::StateChanged("ports"));
    }
    ok_json(json!({"success": true, "enabled": enabled, "start": start, "end": end}))
}

async fn watchdog_status(State(app): State<AppState>) -> Response {
    ok_json(watchdog_payload(&app.runtime.read().await.watchdog))
}

/// 裁决 D2：`/api/health` 不加用户段，摘要由这条管理员端点透出（写入侧是 T7 的 `write_health_summary`）
pub fn users_health_payload(rt: &crate::state::runtime::RuntimeData) -> serde_json::Value {
    rt.extra
        .get("users")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

async fn users_health(State(app): State<AppState>) -> Response {
    ok_json(users_health_payload(&app.runtime.read().await))
}

pub fn routes(shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    let s_list = shared.clone();
    let s_stats = shared.clone();
    let s_online = shared.clone();
    let s_kick = shared.clone();
    axum::Router::new()
        .route(
            "/api/users",
            get(move |st: State<AppState>| list_users(st, s_list.clone())).post(create_user),
        )
        .route("/api/users/{username}", axum::routing::put(update_user).delete(delete_user))
        .route("/api/stats", get(move || get_stats(s_stats.clone())))
        .route("/api/online", get(move || get_online(s_online.clone())))
        .route(
            "/api/kick",
            post(move |st: State<AppState>, body: Bytes| kick(st, s_kick.clone(), body)),
        )
        .route("/api/config", get(get_config))
        .route("/api/password", post(set_password))
        .route("/api/masquerade", get(get_masquerade).post(set_masquerade))
        .route("/api/bandwidth", get(get_bandwidth).post(set_bandwidth))
        .route("/api/port-hopping", get(get_port_hopping).post(set_port_hopping))
        .route("/api/hy2/watchdog/status", get(watchdog_status))
        // 静态段优先于 `/api/users/{username}`（matchit 的静态优先规则），所以它不会被
        // 路径参数吃掉；`{username}` 只挂 PUT / DELETE，GET 也不冲突
        .route("/api/users/health", get(users_health))
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui panel::api_admin && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 17 passed，clippy 无输出。

- [ ] **Step 6: Commit**

```
git add crates/bui/src/modules/panel/api_admin.rs
git commit -m "feat(panel): 管理员 API（用户 CRUD 走 JWT、统计缓存、端口跳跃改 listen 行）"
```

---

### Task 9: 订阅与节点端点（无鉴权，全部复用 `bui-schema` 渲染）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/api_public.rs`

**Interfaces:**
- Consumes: `bui_schema::nodes::{Node, nodes_for}`、`bui_schema::render::SplitRules`、`bui_schema::render::subscription::{uri_list, singbox, clash}`、`bui_schema::model::{State, ResidentialGroup}`、`crate::api::AppState`
- Produces:
```rust
/// 四个端点都无鉴权（spec §4.3「无鉴权（按用户名，沿用 v3）」），挂在 `public_routes()` 里
pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState>;

/// `/api/nodes/<user>` 的载荷 = `nodes_for()` 与 `SplitRules` 直接 serde 序列化
/// （总纲裁决记录 + P4 决策 6；**不得**自己拼 JSON）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodesPayload { pub user: String, pub split: SplitRules, pub nodes: Vec<Node> }

/// 按用户名取出「节点集合 + 分流规则」；用户不存在返回 None
pub fn nodes_and_split(state: &State, username: &str) -> Option<(Vec<Node>, SplitRules)>;
/// 移植 v3 的 `encodeURIComponent(username).replace(/%/g, "_") + ".json"`（`web/server.js:1800`）
pub fn safe_filename(username: &str) -> String;
```

**`SplitRules` 从哪来**：`SplitRules::from_group(&ResidentialGroup)`。分组取 `state.residential.default_group()`；分组不存在（理论上不会，`Residential::default()` 就带一个 `default`）时用 `ResidentialGroup::default()`，它的 `enabled=false` ⇒ `pool_active()` 为假 ⇒ 订阅里不出现住宅分流规则，与 v3「未启用不给关键字」的行为一致（`web/server.js:507-520`）。

**`dial_ip` 传什么**：总纲 C1 注明 `singbox(&[Node], &SplitRules, dial_ip: &str)` 的 `dial_ip = node.public_ip`，即 `state.node.public_ip`。这是 IPv6 接管设计里「生成时解析、按 IP 拨号、SNI 用域名」的那一半（`docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md`），P2 只负责把它传对。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/api_public.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, raw, send, text};
    use pretty_assertions::assert_eq;

    #[test]
    fn filenames_follow_the_v3_encoding() {
        assert_eq!(safe_filename("alice"), "alice.json");
        assert_eq!(safe_filename("a-b_c.d"), "a-b_c.d.json");
        // encodeURIComponent("张") = "%E5%BC%A0"，把 % 换成 _ ⇒ "_E5_BC_A0"
        assert_eq!(safe_filename("张"), "_E5_BC_A0.json");
        assert_eq!(safe_filename("a b"), "a_20b.json");
    }

    #[tokio::test]
    async fn nodes_endpoint_serializes_nodes_for_and_split_rules_verbatim() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", "/api/nodes/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(v["user"], "alice");
        // 与 P4 决策 6 的线格式逐字对齐：kind 是 snake_case，transport 用内部标签 `type`
        let state = h.store.read().await;
        let expect = serde_json::to_value(NodesPayload {
            user: "alice".into(),
            split: bui_schema::render::SplitRules::from_group(
                state.residential.default_group().unwrap(),
            ),
            nodes: bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential),
        })
        .unwrap();
        assert_eq!(v, expect, "必须是 nodes_for + SplitRules 的直接序列化，不能自己拼");
        assert_eq!(v["nodes"][0]["kind"], "reality_direct");
        assert_eq!(v["nodes"][0]["transport"]["type"], "reality");
        assert_eq!(v["split"]["enabled"], false, "sample_state 的住宅池没启用");
    }

    #[tokio::test]
    async fn sub_is_base64_of_the_uri_list() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/sub/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        assert_eq!(String::from_utf8(bytes).unwrap(), bui_schema::render::subscription::uri_list(&nodes, "alice"));
    }

    #[tokio::test]
    async fn subscription_is_the_singbox_config_dialed_by_public_ip() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/subscription/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "application/json; charset=utf-8");
        assert_eq!(headers["content-disposition"], "inline; filename=\"alice.json\"");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        let split = bui_schema::render::SplitRules::from_group(state.residential.default_group().unwrap());
        let want = bui_schema::render::subscription::singbox(&nodes, &split, &state.node.public_ip);
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), want);
    }

    #[tokio::test]
    async fn clash_is_the_mihomo_yaml() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&r, "GET", "/api/clash/alice", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/yaml; charset=utf-8");
        let state = h.store.read().await;
        let nodes = bui_schema::nodes::nodes_for(&state.users[0], &state.node, &state.residential);
        let split = bui_schema::render::SplitRules::from_group(state.residential.default_group().unwrap());
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            bui_schema::render::subscription::clash(&nodes, "alice", &split)
        );
    }

    #[tokio::test]
    async fn an_unknown_user_is_404_on_all_four_endpoints() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for p in ["/api/sub/ghost", "/api/subscription/ghost", "/api/clash/ghost", "/api/nodes/ghost"] {
            let (s, v) = send(&r, "GET", p, None, None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "{p}");
            assert_eq!(v["error"], "User not found", "{p}");
        }
    }

    #[tokio::test]
    async fn all_four_endpoints_work_without_a_token_in_the_full_router() {
        let h = harness().await;
        // 整套装配（含 require_admin），但公开路由挂的是**本文件的** public_routes()：
        // `h.router()` 走 PanelModule，它要到 T13 才接线 ⇒ 这里会拿到 404 而不是 200。
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        for p in ["/api/sub/alice", "/api/clash/alice", "/api/nodes/alice"] {
            let (s, _) = text(&router, p).await;
            assert_eq!(s, axum::http::StatusCode::OK, "{p} 必须无鉴权可达（spec §4.3）");
        }
        let (s, _) = text(&router, "/api/subscription/alice").await;
        assert_eq!(s, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn a_residential_pool_turns_the_split_rules_on() {
        let h = harness().await;
        h.store
            .update(|s| {
                let g = s.residential.groups.get_mut("default").unwrap();
                g.enabled = true;
                g.mode = bui_schema::model::ResiMode::Global;
                g.upstreams.push(bui_schema::model::Upstream {
                    id: uuid::Uuid::nil(),
                    name: "url-1".into(),
                    kind: bui_schema::model::UpstreamKind::Http,
                    host: "isp.example.net".into(),
                    port: 10007,
                    username: "u".into(),
                    password: "p".into(),
                    priority: 10,
                    provider: None,
                    region: None,
                    ports_allowed: None,
                    verified: None,
                });
            })
            .await
            .unwrap();
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (_, v) = send(&r, "GET", "/api/nodes/alice", None, None).await;
        assert_eq!(v["split"]["enabled"], true);
        assert_eq!(v["split"]["global"], true);
        assert!(
            !v["split"]["keywords"].as_array().unwrap().is_empty(),
            "keywords=null ⇒ 跟随 DEFAULT_KEYWORDS（67 条）"
        );
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::api_public`
Expected: 编译失败，`cannot find function 'public_routes' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! 三种订阅 + `/api/nodes`（spec §4.3 的「无鉴权」组、§4.4）。
//!
//! 四个端点的节点集合都来自 `bui_schema::nodes::nodes_for`，渲染全部交给
//! `bui_schema::render::subscription`：P2 一行拼装逻辑都不写（总纲 C1、审计 web-C15）。

use super::Shared;
use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bui_schema::model::{ResidentialGroup, State as BuiState};
use bui_schema::nodes::{nodes_for, Node};
use bui_schema::render::subscription::{clash, singbox, uri_list};
use bui_schema::render::SplitRules;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodesPayload {
    pub user: String,
    pub split: SplitRules,
    pub nodes: Vec<Node>,
}

pub fn nodes_and_split(state: &BuiState, username: &str) -> Option<(Vec<Node>, SplitRules)> {
    let user = state.users.iter().find(|u| u.username == username)?;
    let nodes = nodes_for(user, &state.node, &state.residential);
    let fallback = ResidentialGroup::default();
    let group = state.residential.default_group().unwrap_or(&fallback);
    Some((nodes, SplitRules::from_group(group)))
}

/// 移植 `web/server.js:1800`：`encodeURIComponent(username).replace(/%/g, "_") + ".json"`。
/// `encodeURIComponent` 不编码的集合是 `A-Za-z0-9-_.!~*'()`。
pub fn safe_filename(username: &str) -> String {
    const KEEP: &[u8] = b"-_.!~*'()";
    let mut out = String::with_capacity(username.len() + 5);
    for b in username.as_bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(b) {
            out.push(*b as char);
        } else {
            out.push('_');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out.push_str(".json");
    out
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "User not found"}))).into_response()
}

async fn get_sub(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, _split)) = nodes_and_split(&state, &user) else { return not_found() };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        uri_list(&nodes, &user),
    )
        .into_response()
}

async fn get_subscription(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else { return not_found() };
    // 总纲 C1：dial_ip = node.public_ip（IPv6 接管设计的「按 IP 拨号、SNI 用域名」）
    let cfg = singbox(&nodes, &split, &state.node.public_ip);
    let body = serde_json::to_vec_pretty(&cfg).unwrap_or_default();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{}\"", safe_filename(&user)),
            ),
        ],
        body,
    )
        .into_response()
}

async fn get_clash(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else { return not_found() };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/yaml; charset=utf-8")],
        clash(&nodes, &user, &split),
    )
        .into_response()
}

async fn get_nodes(State(app): State<AppState>, Path(user): Path<String>) -> Response {
    let state = app.store.read().await;
    let Some((nodes, split)) = nodes_and_split(&state, &user) else { return not_found() };
    (StatusCode::OK, Json(NodesPayload { user, split, nodes })).into_response()
}

pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/sub/{user}", get(get_sub))
        .route("/api/subscription/{user}", get(get_subscription))
        .route("/api/clash/{user}", get(get_clash))
        .route("/api/nodes/{user}", get(get_nodes))
}
```
`public_routes` 的 `_shared` 参数是**有意留的**：四个 handler 只需要 `AppState`，但 Task 13 把所有子路由按同一个签名 `fn(Arc<Shared>) -> Router<AppState>` 合并，签名统一比省一个下划线更值。

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::api_public && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 8 passed，clippy 无输出。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/api_public.rs
git commit -m "feat(panel): 三种订阅与 /api/nodes（全部复用 bui-schema 渲染）"
```

---

### Task 10: 用户域 `/api/me/*` 的 501 桩（spec 附录 A）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/api_me.rs`

**Interfaces:**
- Consumes: `crate::api::AppState`、`crate::modules::panel::Shared`
- Produces:
```rust
/// spec §4.3：用户域**全部返回 501** `{"error":"not_implemented"}`；请求/响应结构见附录 A。
/// 挂在 `public_routes()` 里（`POST /api/me/login` 本来就不该要管理员 token）。
pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState>;
/// 七个端点的路径与方法，`bui status` 与 P5 的验收脚本按它核对
pub const ME_ENDPOINTS: [(&str, &str); 7] = [
    ("POST", "/api/me/login"),
    ("GET", "/api/me"),
    ("GET", "/api/me/subscription-links"),
    ("GET", "/api/me/entitlements"),
    ("GET", "/api/me/billing"),
    ("POST", "/api/me/orders"),
    ("GET", "/api/me/orders/{id}"),
];
```

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/api_me.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, mount, send};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn every_user_domain_endpoint_is_501_with_the_documented_body() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        for (m, p) in ME_ENDPOINTS {
            // 路由里的 `{id}` 换成一个具体值再请求
            let uri = p.replace("{id}", "ord-1");
            let (s, v) = send(&r, m, &uri, None, Some(serde_json::json!({}))).await;
            assert_eq!(s, axum::http::StatusCode::NOT_IMPLEMENTED, "{m} {uri}");
            assert_eq!(v, serde_json::json!({"error": "not_implemented"}), "{m} {uri}");
        }
        assert_eq!(ME_ENDPOINTS.len(), 7, "spec 附录 A 正好七个端点");
    }

    #[tokio::test]
    async fn the_user_domain_does_not_require_an_admin_token() {
        let h = harness().await;
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        // 501 而不是 401：它们在 require_admin 外面（否则将来接自助门户还要再搬一次）
        let (s, _) = send(&router, "POST", "/api/me/login", None, Some(serde_json::json!({"username": "a", "password": "b"}))).await;
        assert_eq!(s, axum::http::StatusCode::NOT_IMPLEMENTED);
        let (s2, _) = send(&router, "GET", "/api/me", None, None).await;
        assert_eq!(s2, axum::http::StatusCode::NOT_IMPLEMENTED);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::api_me`
Expected: 编译失败，`cannot find value 'ME_ENDPOINTS' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! 用户域（自助门户）API 的预留桩。spec §4.3：**v4 全部返回 501**
//! `{"error":"not_implemented"}`；请求与响应结构由 spec 附录 A 定义，照抄如下，
//! 将来实现时不必再回头查 spec：
//!
//! - `POST /api/me/login` `{ "username", "password" }` → `{ "token", "expires_at" }`
//! - `GET  /api/me` → `{ "user_id", "username", "entitlements", "usage", "expires_at" }`
//! - `GET  /api/me/subscription-links` → `{ "sub", "singbox", "clash", "nodes" }`（四个 URL）
//! - `GET  /api/me/entitlements` → 同 spec §4.1 的 `entitlements`
//! - `GET  /api/me/billing` → `{ "currency", "balance_minor", "orders": [...] }`
//! - `POST /api/me/orders` `{ "sku", "quantity", "region" }` → `{ "order_id", "status": "pending", "amount_minor" }`
//! - `GET  /api/me/orders/{id}` → 订单记录（spec §4.1 的 `Order`）
//!
//! 这些端点故意挂在 `public_routes()`（`require_admin` 外面）：用户域的凭据是
//! `user.portal_auth`，不是管理员 JWT；放在里面将来接门户还得再搬一次。

use super::Shared;
use crate::api::AppState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

pub const ME_ENDPOINTS: [(&str, &str); 7] = [
    ("POST", "/api/me/login"),
    ("GET", "/api/me"),
    ("GET", "/api/me/subscription-links"),
    ("GET", "/api/me/entitlements"),
    ("GET", "/api/me/billing"),
    ("POST", "/api/me/orders"),
    ("GET", "/api/me/orders/{id}"),
];

async fn not_implemented() -> Response {
    (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({"error": "not_implemented"}))).into_response()
}

pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/me/login", post(not_implemented))
        .route("/api/me", get(not_implemented))
        .route("/api/me/subscription-links", get(not_implemented))
        .route("/api/me/entitlements", get(not_implemented))
        .route("/api/me/billing", get(not_implemented))
        .route("/api/me/orders", post(not_implemented))
        .route("/api/me/orders/{id}", get(not_implemented))
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::api_me && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 2 passed，clippy 无输出。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/api_me.rs
git commit -m "feat(panel): 用户域 /api/me/* 的 501 桩（spec 附录 A）"
```

---

### Task 11: `rust-embed` 嵌入旧前端 + `web/app.js` 的那一处改动

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/assets.rs`
- **不改** `crates/bui/Cargo.toml`：`rust-embed` 已由 T1 一次加齐（决策 D4）
- Modify: `web/app.js`（**只改两处**：`addUser()` 的请求、`login()` 里的 `localStorage.setItem("ap", pw)`）

**Interfaces:**
- Consumes: `crate::api::AppState`、`crate::modules::panel::Shared`
- Produces:
```rust
/// 嵌入的面板前端（spec §1：`web/` 前端由 `rust-embed` 嵌入 `bui`；实际是 5 个文件）
#[derive(rust_embed::Embed)] pub struct Web;
/// 嵌入的客户端首次安装脚本（P4 Task 13 交付，Task 12 的 `/packages/bui-c-install.sh` 用它）
#[derive(rust_embed::Embed)] pub struct Scripts;

/// URL 路径 → 嵌入文件名（`/` 与 `/index.html` 都给 index.html）
pub const WEB_ROUTES: [(&str, &str); 6] = [
    ("/", "index.html"),
    ("/index.html", "index.html"),
    ("/app.js", "app.js"),
    ("/style.css", "style.css"),
    ("/qrcode.min.js", "qrcode.min.js"),
    ("/logo.jpg", "logo.jpg"),
];
pub fn content_type(name: &str) -> &'static str;
/// 取一个嵌入的前端文件（不存在返回 None）
pub fn web_file(name: &str) -> Option<Vec<u8>>;
/// 取嵌入的 `bui-c-install.sh`（P4 Task 13 未合并时返回 None）
pub fn install_script() -> Option<Vec<u8>>;
/// 六条前端路由，全部无鉴权
pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState>;
```

**`rust-embed` 的三个 feature 都是必需的**（本机实测，缺一个就编译不过或行为不对）：
- `interpolate-folder-path`：`#[folder = "$CARGO_MANIFEST_DIR/../../web/"]` 里的变量要它才展开，否则报「folder '<crate>/$CARGO_MANIFEST_DIR/../../web/' does not exist」。
- `include-exclude`：用到 `#[include = "…"]` 就必须开，否则报「Please turn on the `include-exclude` feature」。
- `debug-embed`：默认 debug 构建是「运行时读盘」，那样 `cargo test` 在别的 cwd 下会找不到文件、生产二进制也可能不自带前端。开了它 debug / release 一律真嵌入。

另外：`Web::get` / `Web::iter` 是 `rust_embed::Embed` trait 的方法，**用到它们的每个模块（含 `mod tests`）都要 `use rust_embed::Embed;`**，否则报「no function or associated item named `get` found」。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/assets.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{full_with, harness, raw};
    use pretty_assertions::assert_eq;
    use rust_embed::Embed;

    #[test]
    fn exactly_the_five_front_end_files_are_embedded() {
        let mut names: Vec<String> = Web::iter().map(|c: std::borrow::Cow<str>| c.to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "app.js".to_string(),
                "index.html".to_string(),
                "logo.jpg".to_string(),
                "qrcode.min.js".to_string(),
                "style.css".to_string(),
            ]
        );
        assert!(web_file("index.html").unwrap().starts_with(b"<!DOCTYPE"));
        assert!(web_file("server.js").is_none(), "v3 的 Node 服务端不能被嵌进来");
    }

    #[test]
    fn content_types_cover_every_route() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("app.js"), "application/javascript; charset=utf-8");
        assert_eq!(content_type("qrcode.min.js"), "application/javascript; charset=utf-8");
        assert_eq!(content_type("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("logo.jpg"), "image/jpeg");
        assert_eq!(content_type("whatever.bin"), "application/octet-stream");
    }

    #[tokio::test]
    async fn the_panel_is_served_without_a_token() {
        let h = harness().await;
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        for (path, name) in WEB_ROUTES {
            let (s, headers, bytes) = raw(&router, "GET", path, None, None).await;
            assert_eq!(s, axum::http::StatusCode::OK, "{path}");
            assert_eq!(headers["content-type"], content_type(name), "{path}");
            assert_eq!(bytes, web_file(name).unwrap(), "{path} 的内容要与嵌入的一致");
        }
        let (s404, _, _) = raw(&router, "GET", "/nope.html", None, None).await;
        assert_eq!(s404, axum::http::StatusCode::NOT_FOUND);
    }

    /// spec §4.3 改动 1 的回归锁：前端不能再走 `GET /api/manage`，也不能再往
    /// localStorage 存明文管理员密码（审计 web-C6）。改坏了这条会立刻红。
    #[test]
    fn app_js_creates_users_over_post_and_never_stores_the_admin_password() {
        let js = String::from_utf8(web_file("app.js").unwrap()).unwrap();
        assert!(!js.contains("/api/manage"), "app.js 还在调 /api/manage");
        assert!(!js.contains("localStorage.setItem(\"ap\""), "app.js 还在存明文管理员密码");
        assert!(!js.contains("localStorage.getItem(\"ap\")"), "app.js 还在读明文管理员密码");
        assert!(js.contains("api(\"/users\", {"), "addUser 没改成 POST /api/users");
    }

    #[test]
    fn the_installer_script_is_embedded_from_the_scripts_folder() {
        // P4 Task 13 交付 scripts/bui-c-install.sh；它按 `$BUI_C_SOURCE/<artifact 键名>` 取二进制
        let s = install_script().expect("scripts/bui-c-install.sh 必须存在（前置：P4 Task 13 已合并）");
        let text = String::from_utf8(s).unwrap();
        assert!(text.starts_with("#!"), "引导脚本要有 shebang");
        assert!(text.contains("BUI_C_SOURCE"), "引导脚本要认 BUI_C_SOURCE（P4 决策 9）");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::assets`
Expected: 编译失败，`cannot find type 'Web' in this scope`。

- [ ] **Step 3: Cargo.toml 追加依赖**

`rust-embed` 已由 **T1** 一次加齐（决策 D4：T1 / T4 / T11 并行，各改一次 `Cargo.toml` 会让 `Cargo.lock` 三方冲突）。本步只核对，**不要动 `Cargo.toml`**：

```bash
grep -n 'rust-embed' crates/bui/Cargo.toml
# 期望：rust-embed = { version = "8", features = ["debug-embed", "include-exclude", "interpolate-folder-path"] }
```
三个 feature 一个都不能少：`interpolate-folder-path` 展开 `$CARGO_MANIFEST_DIR`、`include-exclude` 启用 `#[include]`、`debug-embed` 让 debug 构建也真嵌入（默认是运行时读盘，测试会读到机器上的 `web/`）。对不上就是 T1 没合并或加漏了：回去补 T1。

- [ ] **Step 4: 实现 `assets.rs`**

```rust
//! 嵌入的面板前端与客户端引导脚本（spec §1、§4.3 改动 1）。
//!
//! v3 是从 `ADMIN_DIR` 读盘（`web/server.js:1476-1513`）；v4 编进二进制，
//! 于是 `/opt/b-ui/admin/` 整棵目录连同 Node 一起消失。

use super::Shared;
use crate::api::AppState;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::sync::Arc;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/"]
#[include = "index.html"]
#[include = "app.js"]
#[include = "style.css"]
#[include = "qrcode.min.js"]
#[include = "logo.jpg"]
pub struct Web;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../scripts/"]
#[include = "bui-c-install.sh"]
pub struct Scripts;

pub const WEB_ROUTES: [(&str, &str); 6] = [
    ("/", "index.html"),
    ("/index.html", "index.html"),
    ("/app.js", "app.js"),
    ("/style.css", "style.css"),
    ("/qrcode.min.js", "qrcode.min.js"),
    ("/logo.jpg", "logo.jpg"),
];

pub fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("json") => "application/json; charset=utf-8",
        Some("sh") => "text/x-shellscript; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub fn web_file(name: &str) -> Option<Vec<u8>> {
    Web::get(name).map(|f| f.data.into_owned())
}

pub fn install_script() -> Option<Vec<u8>> {
    Scripts::get("bui-c-install.sh").map(|f| f.data.into_owned())
}

fn serve(name: &'static str) -> Response {
    match web_file(name) {
        Some(bytes) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, content_type(name))], bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub fn public_routes(_shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    let mut r = axum::Router::new();
    for (path, name) in WEB_ROUTES {
        r = r.route(path, get(move || async move { serve(name) }));
    }
    r
}
```

- [ ] **Step 5: 改 `web/app.js` 的那两处**

第一处，`login()` 里删掉存明文密码那一行（原 `web/app.js:61`）：
```diff
             if (d.token) {
                 tok = d.token;
                 localStorage.setItem("t", tok);
-                localStorage.setItem("ap", pw);
                 init();
```

第二处，`addUser()` 的请求（原 `web/app.js:190-206`），整段替换：
```diff
-    let url = "/api/manage?key=" + encodeURIComponent(cfg.adminPass || localStorage.getItem("ap") || "") +
-        "&action=create&user=" + encodeURIComponent(u) +
-        (p ? "&pass=" + encodeURIComponent(p) : "") +
-        "&days=" + d + "&traffic=" + t + "&monthly=" + m + "&speed=" + s + "&protocol=" + proto +
-        "&residential=" + (residential ? "true" : "false");
-
-    if (customSni) url += "&sni=" + encodeURIComponent(customSni);
-
-    fetch(url).then(r => r.json()).then(r => {
+    // v4：创建用户改为 JWT 保护的 POST /api/users（spec §4.3 改动 1）。
+    // sni 与 speed 服务端会接受但忽略（v4 的 SNI 全局唯一、内核不支持按用户限速）。
+    api("/users", {
+        method: "POST",
+        body: JSON.stringify({
+            username: u,
+            password: p || undefined,
+            days: parseFloat(d),
+            traffic: parseFloat(t),
+            monthly: parseFloat(m),
+            protocol: proto,
+            residential: residential,
+            sni: customSni || undefined,
+            speed: parseFloat(s)
+        })
+    }).then(r => {
         if (r.success) {
             closeM();
             toast("用户 " + u + " 已创建");
             load();
         } else {
             toast(r.error || "操作失败", 1);
         }
     });
```
其余一行不动（`index.html` / `style.css` / `logo.jpg` / `qrcode.min.js` 一个字都不改）。改完跑 `node --check web/app.js`。

- [ ] **Step 6: 运行测试**

Run: `node --check web/app.js && cargo test -p bui panel::assets && cargo clippy -p bui --all-targets -- -D warnings`
Expected: `node --check` 无输出，5 passed，clippy 无输出。

- [ ] **Step 7: Commit**

```
git add crates/bui/src/modules/panel/assets.rs web/app.js
git commit -m "feat(panel): rust-embed 嵌入前端，创建用户改走 POST /api/users"
```

---

### Task 12: 客户端包缓存与 `/packages/*`、`/api/install-command`

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/panel/packages.rs`

**前置**：Task 11（`assets::{install_script, content_type}`）；P1 Task 15 已合并（`cache_loop` 编译依赖 `crate::serve::load_cached_manifest`；2026-09-12 核对 `v4` = `eb10eb1` 上它已在 ⇒ **这条已结清**）；P4 Task 13 已合并（`scripts/bui-c-install.sh` 存在，**这条是唯一还要确认的**）。

**Interfaces:**
- Consumes: `crate::kernels::{Manifest, Asset, Fetcher, sha256_hex}`、`crate::serve::load_cached_manifest`、`crate::sys::Host`、`crate::reconcile::DaemonCtx`、`crate::modules::panel::{Shared, assets}`、`crate::api::AppState`、`crate::redact::url_credentials`（`HttpFetcher` 由 **T13** 在 `spawn()` 里构造并注入，本任务只收 `&dyn Fetcher`；架构名用本文件的 `CLIENT_ARCHES`，不用 `kernels::asset_arch`——那是给本机架构选内核用的）
- Produces:
```rust
/// 服务端替客户端缓存的二进制（spec §6「服务端内核缓存继续维护 sing-box 与 `bui-c` 的 Linux 二进制」）
pub const CLIENT_BINARIES: [&str; 2] = ["bui-c", "sing-box"];
pub const CLIENT_ARCHES: [&str; 2] = ["amd64", "arm64"];
pub const INSTALL_SCRIPT: &str = "bui-c-install.sh";
/// 每日缓存一次（与 P1 的每日 manifest 自检同频，但各跑各的）
pub const CACHE_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheReport {
    pub downloaded: Vec<String>,   // 文件名
    pub skipped: Vec<String>,      // sha256 已一致
    pub notes: Vec<String>,        // 非致命提示（如 client_sing_box 与 sing_box 版本不一致）
    pub errors: Vec<String>,
}
/// **同步**函数（`Fetcher` 是同步 trait，`reqwest::blocking` 不能在 async 上下文跑）：
/// 只能在 `tokio::task::spawn_blocking` 里调用。
pub fn sync_once(host: &dyn Host, fetcher: &dyn Fetcher, manifest: &Manifest, dir: &Path) -> CacheReport;
/// 每日一次：读 `<base>/manifest.json` 缓存 → `sync_once`
pub async fn cache_loop(ctx: DaemonCtx, shared: Arc<Shared>, fetcher: Arc<dyn Fetcher>);

/// `/api/install-command`（无鉴权，**不带 install key**，spec §4.3 改动 2）
pub fn install_command(host: &str) -> String;
/// 三条公开路由：`/api/install-command`、`/packages/manifest.json`、`/packages/{file}`
pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState>;
/// 文件名白名单校验（无 `/`、无 `\`、无 `..`、非空）
pub fn safe_name(name: &str) -> bool;
```

**下载哪些文件**：manifest 的 `artifacts` 里取 `bui-c-linux-amd64`、`bui-c-linux-arm64`、`sing-box-linux-amd64`、`sing-box-linux-arm64` 四个，落到 `<base>/packages/<键名>`（0644）。文件名就是 C4 的键名 —— P4 的引导脚本按 `$BUI_C_SOURCE/<artifact 键名>` 取，键名对不上就下载不到（P4 Task 13 的责任段明写了这条）。

**为什么客户端 sing-box 用 `sing-box-linux-<arch>`**：C4 的 `artifacts` 里只有这一组 sing-box 键，而 `kernels` 同时有 `sing_box` 与 `client_sing_box` 两个版本号（决策 D8）。两者不等时只在 `CacheReport.notes` 里记一条，仍然缓存该 artifact —— 缓存空着比版本差一个小版本糟得多。

**`<base>/packages/` 不是 artifact**：它在 P1 Task 5 的 `BASE_WHITELIST` 里，而且漂移扫描不递归进去（P1 Task 5 的「扫描范围是有意收窄的」一段），所以往里放文件不会产生 `stray_file`。

**为什么下载走 `Host`、HTTP handler 却直接 `std::fs::read`**：P1 的铁律是「**改**机器的操作必须经 `Host`」——`sync_once` 写文件，所以走 `host.write_file`（于是它能用 `FakeHost` 单测）。而 `/packages/{name}` 与 `/packages/manifest.json` 只是**读**一个已经在盘上的文件，且跑在 async 路径上（`Host` 是同步 trait，塞进 handler 就得多一次 `spawn_blocking`）。对应的代价是这两条路由的测试要真在 tempdir 里放文件（Task 12 的 `packages_are_downloadable_without_a_key_and_reject_traversal` 就是这么做的），而不是给 `FakeHost` 播种。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/packages.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::modules::panel::testsupport::{full_with, harness, mount, raw, send};
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct FakeFetcher(Mutex<BTreeMap<String, Vec<u8>>>);
    impl crate::kernels::Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    fn manifest_with(bytes: &[u8]) -> Manifest {
        let sha = crate::kernels::sha256_hex(bytes);
        let mut artifacts = BTreeMap::new();
        for name in CLIENT_BINARIES {
            for arch in CLIENT_ARCHES {
                artifacts.insert(
                    format!("{name}-linux-{arch}"),
                    Asset { url: format!("https://x/{name}-{arch}"), sha256: sha.clone() },
                );
            }
        }
        Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([
                ("sing_box".to_string(), "1.14.5".to_string()),
                ("client_sing_box".to_string(), "1.14.5".to_string()),
            ]),
            artifacts,
            min_upgrade_from: None,
        }
    }

    fn fetcher_for(m: &Manifest, bytes: &[u8]) -> FakeFetcher {
        FakeFetcher(Mutex::new(
            m.artifacts.values().map(|a| (a.url.clone(), bytes.to_vec())).collect(),
        ))
    }

    #[test]
    fn safe_name_rejects_traversal() {
        assert!(safe_name("bui-c-linux-amd64"));
        assert!(safe_name("manifest.json"));
        assert!(!safe_name(""));
        assert!(!safe_name("../state.json"));
        assert!(!safe_name("a/b"));
        assert!(!safe_name("a\\b"));
    }

    #[test]
    fn install_command_has_no_key_and_points_at_the_panel() {
        let c = install_command("panel.example.com");
        assert_eq!(
            c,
            "curl -fsSL --noproxy '*' 'https://panel.example.com/packages/bui-c-install.sh' \
             | sudo BUI_C_SOURCE='https://panel.example.com/packages' bash"
        );
        assert!(!c.contains(" -k "), "v4 面板证书由 Caddy 正规签发，不能跳过校验");
        assert!(!c.contains("key="), "spec §4.3 改动 2：install key 机制已删除");
    }

    #[test]
    fn sync_once_downloads_four_artifacts_and_verifies_sha256() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        let f = fetcher_for(&m, b"ELF-bytes");
        let dir = std::path::Path::new("/opt/b-ui/packages");
        let rep = sync_once(&h, &f, &m, dir);
        assert_eq!(rep.errors, Vec::<String>::new());
        let mut got = rep.downloaded.clone();
        got.sort();
        assert_eq!(
            got,
            vec![
                "bui-c-linux-amd64".to_string(),
                "bui-c-linux-arm64".to_string(),
                "sing-box-linux-amd64".to_string(),
                "sing-box-linux-arm64".to_string(),
            ]
        );
        assert_eq!(h.mode("/opt/b-ui/packages/bui-c-linux-amd64"), Some(0o644));
        assert_eq!(h.text("/opt/b-ui/packages/sing-box-linux-arm64").as_deref(), Some("ELF-bytes"));
    }

    #[test]
    fn sync_once_skips_files_that_already_match() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        let f = fetcher_for(&m, b"ELF-bytes");
        let dir = std::path::Path::new("/opt/b-ui/packages");
        sync_once(&h, &f, &m, dir);
        h.clear_ops();
        let rep = sync_once(&h, &f, &m, dir);
        assert!(rep.downloaded.is_empty());
        assert_eq!(rep.skipped.len(), 4);
        assert!(h.ops().iter().all(|o| !o.starts_with("write:")), "第二轮不该写盘：{:?}", h.ops());
    }

    #[test]
    fn a_sha256_mismatch_is_an_error_and_writes_nothing() {
        let h = FakeHost::new();
        let m = manifest_with(b"ELF-bytes");
        // fetcher 返回的不是 manifest 里记的那份内容
        let f = fetcher_for(&m, b"tampered");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert!(rep.downloaded.is_empty());
        assert_eq!(rep.errors.len(), 4, "{:?}", rep.errors);
        assert!(rep.errors[0].contains("sha256"), "{:?}", rep.errors);
        assert_eq!(h.text("/opt/b-ui/packages/bui-c-linux-amd64"), None);
    }

    #[test]
    fn a_missing_artifact_key_is_an_error_not_a_panic() {
        let h = FakeHost::new();
        let mut m = manifest_with(b"ELF-bytes");
        m.artifacts.remove("bui-c-linux-arm64");
        let f = fetcher_for(&m, b"ELF-bytes");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert_eq!(rep.downloaded.len(), 3);
        assert!(rep.errors.iter().any(|e| e.contains("bui-c-linux-arm64")), "{:?}", rep.errors);
    }

    #[test]
    fn a_client_singbox_version_mismatch_only_notes_it() {
        let h = FakeHost::new();
        let mut m = manifest_with(b"ELF-bytes");
        m.kernels.insert("client_sing_box".into(), "1.13.19".into());
        let f = fetcher_for(&m, b"ELF-bytes");
        let rep = sync_once(&h, &f, &m, std::path::Path::new("/opt/b-ui/packages"));
        assert_eq!(rep.errors, Vec::<String>::new(), "决策 D8：只提示，照缓存");
        assert!(rep.notes.iter().any(|n| n.contains("client_sing_box")), "{:?}", rep.notes);
        assert_eq!(rep.downloaded.len(), 4);
    }

    #[tokio::test]
    async fn install_command_uses_the_host_header_then_the_state_domain() {
        let h = harness().await;
        let r = mount(&h.app, public_routes(h.shared.clone()));
        let (s, v) = send(&r, "GET", "/api/install-command", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        // oneshot 的请求没有 Host 头 ⇒ 回落 state.node.domain
        assert_eq!(v["server"], "example.com");
        assert_eq!(v["command"], install_command("example.com"));
        assert!(v.get("key").is_none(), "绝不能再回 install key");
        assert!(v["note"].as_str().unwrap().contains("bui-c"), "{v}");
    }

    #[tokio::test]
    async fn packages_are_downloadable_without_a_key_and_reject_traversal() {
        let h = harness().await;
        std::fs::create_dir_all(h.shared.packages_dir()).unwrap();
        std::fs::write(h.shared.packages_dir().join("bui-c-linux-amd64"), b"ELF").unwrap();
        std::fs::write(crate::paths::manifest_file(&h.paths), br#"{"version":"4.0.0"}"#).unwrap();
        // 整套装配，公开路由挂本文件的 public_routes()（PanelModule 要到 T13 才接线）
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&router, "GET", "/packages/bui-c-linux-amd64", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(bytes, b"ELF");
        assert_eq!(headers["content-type"], "application/octet-stream");
        let (sm, _, mb) = raw(&router, "GET", "/packages/manifest.json", None, None).await;
        assert_eq!(sm, axum::http::StatusCode::OK);
        assert_eq!(mb, br#"{"version":"4.0.0"}"#);
        let (s404, _, _) = raw(&router, "GET", "/packages/nope", None, None).await;
        assert_eq!(s404, axum::http::StatusCode::NOT_FOUND);
        let (sbad, _, _) = raw(&router, "GET", "/packages/..%2fstate.json", None, None).await;
        assert_eq!(sbad, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_installer_script_comes_from_the_embed_when_it_is_not_on_disk() {
        let h = harness().await;
        let router = full_with(&h.app, axum::Router::new(), public_routes(h.shared.clone()));
        let (s, headers, bytes) = raw(&router, "GET", "/packages/bui-c-install.sh", None, None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/x-shellscript; charset=utf-8");
        assert_eq!(bytes, crate::modules::panel::assets::install_script().unwrap());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::packages`
Expected: 编译失败，`cannot find function 'sync_once' in this scope`。

- [ ] **Step 3: 实现**

```rust
//! 客户端包缓存与 `/packages/*`、`/api/install-command`（spec §4.3 改动 2、§6）。
//!
//! v3 的做法是 `/install-client?key=<install key>` 发脚本、`/packages/<file>` 无 key 就能下
//! （审计 web-C8：这套 key 机制在安全上为零）。v4 把 key 整个删掉：引导脚本与二进制都公开可下，
//! 真正的门槛是订阅里的凭据。

use super::{assets, Shared};
use crate::api::AppState;
use crate::kernels::{sha256_hex, Fetcher, Manifest};
use crate::reconcile::DaemonCtx;
use crate::sys::Host;
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const CLIENT_BINARIES: [&str; 2] = ["bui-c", "sing-box"];
pub const CLIENT_ARCHES: [&str; 2] = ["amd64", "arm64"];
pub const INSTALL_SCRIPT: &str = "bui-c-install.sh";
pub const CACHE_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheReport {
    pub downloaded: Vec<String>,
    pub skipped: Vec<String>,
    pub notes: Vec<String>,
    pub errors: Vec<String>,
}

pub fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

pub fn sync_once(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    manifest: &Manifest,
    dir: &Path,
) -> CacheReport {
    let mut rep = CacheReport::default();
    // 决策 D8：C4 只有一组 sing-box artifact，两个版本号不等时只提示
    let sb = manifest.kernels.get("sing_box");
    let csb = manifest.kernels.get("client_sing_box");
    if let (Some(a), Some(b)) = (sb, csb) {
        if a != b {
            rep.notes.push(format!(
                "manifest 的 client_sing_box({b}) 与 sing_box({a}) 不一致，但 artifacts 只有一组 \
                 sing-box-linux-<arch>，缓存的是它"
            ));
        }
    }
    for name in CLIENT_BINARIES {
        for arch in CLIENT_ARCHES {
            let key = format!("{name}-linux-{arch}");
            let Some(asset) = manifest.artifacts.get(&key) else {
                rep.errors.push(format!("manifest 缺少 artifact：{key}"));
                continue;
            };
            let dest = dir.join(&key);
            if host
                .read_file(&dest)
                .ok()
                .flatten()
                .map(|b| sha256_hex(&b) == asset.sha256)
                .unwrap_or(false)
            {
                rep.skipped.push(key);
                continue;
            }
            let bytes = match fetcher.get_bytes(&asset.url) {
                Ok(b) => b,
                Err(e) => {
                    rep.errors.push(format!(
                        "{key} 下载失败：{}",
                        crate::redact::url_credentials(&e.to_string())
                    ));
                    continue;
                }
            };
            let got = sha256_hex(&bytes);
            if got != asset.sha256 {
                rep.errors.push(format!("{key} sha256 不符：期望 {} 实得 {got}", asset.sha256));
                continue;
            }
            match host.write_file(&dest, &bytes, 0o644) {
                Ok(()) => rep.downloaded.push(key),
                Err(e) => rep.errors.push(format!("{key} 写盘失败：{e}")),
            }
        }
    }
    rep
}

pub async fn cache_loop(ctx: DaemonCtx, shared: Arc<Shared>, fetcher: Arc<dyn Fetcher>) {
    let mut iv = tokio::time::interval(Duration::from_secs(CACHE_INTERVAL_SECS));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        let host = ctx.host.clone();
        let paths = ctx.paths.clone();
        let dir = shared.packages_dir();
        let f = fetcher.clone();
        // `Fetcher` 是同步 trait（reqwest::blocking 在 async 上下文会 panic）⇒ 必须 spawn_blocking
        let rep = tokio::task::spawn_blocking(move || {
            let m = crate::serve::load_cached_manifest(host.as_ref(), &paths)?;
            Some(sync_once(host.as_ref(), f.as_ref(), &m, &dir))
        })
        .await
        .ok()
        .flatten();
        match rep {
            None => tracing::debug!("还没有 manifest 缓存，跳过本轮客户端包缓存"),
            Some(r) => {
                for n in &r.notes {
                    tracing::warn!(note = %n, "客户端包缓存提示");
                }
                for e in &r.errors {
                    tracing::warn!(error = %e, "客户端包缓存失败项");
                }
                if !r.downloaded.is_empty() {
                    tracing::info!(files = ?r.downloaded, "客户端包缓存已更新");
                }
            }
        }
    }
}

pub fn install_command(host: &str) -> String {
    // 沿用 v3 的 `--noproxy '*'`（机房里客户端可能有 http_proxy 环境变量），但**去掉 v3 的 `-k`**：
    // v4 的面板证书由 Caddy 正规签发（P1 Task 11），`-k` 已无必要，而它会把这条
    // pipe-to-sudo 命令的可信度削掉一半（中间人可以换掉脚本）。也**不带 install key**
    // （spec §4.3 改动 2）。`BUI_C_SOURCE` 让引导脚本优先从本面板取二进制（P4 决策 9）。
    format!(
        "curl -fsSL --noproxy '*' 'https://{host}/packages/{INSTALL_SCRIPT}' | sudo BUI_C_SOURCE='https://{host}/packages' bash"
    )
}

async fn get_install_command(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(String::new);
    let host = if host.is_empty() { app.store.read().await.node.domain.clone() } else { host };
    Json(json!({
        "command": install_command(&host),
        "server": host,
        "note": "在客户端机器上执行；脚本只负责第一次把 bui-c 装上，之后用 `bui-c update` 升级",
    }))
    .into_response()
}

async fn get_manifest(shared: Arc<Shared>) -> Response {
    let path = crate::paths::manifest_file(&shared.paths());
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "manifest not cached"}))).into_response(),
    }
}

async fn get_package(AxPath(name): AxPath<String>, shared: Arc<Shared>) -> Response {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid filename"}))).into_response();
    }
    let ct = assets::content_type(&name);
    if let Ok(bytes) = std::fs::read(shared.packages_dir().join(&name)) {
        return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response();
    }
    // 引导脚本不在盘上：直接发嵌进二进制的那一份（永远与本机 bui 同版本）
    if name == INSTALL_SCRIPT {
        if let Some(bytes) = assets::install_script() {
            return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], bytes).into_response();
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({"error": "File not found"}))).into_response()
}

pub fn public_routes(shared: Arc<Shared>) -> axum::Router<AppState> {
    use axum::routing::get;
    let s_m = shared.clone();
    let s_p = shared.clone();
    axum::Router::new()
        .route("/api/install-command", get(get_install_command))
        .route(
            "/packages/manifest.json",
            get(move || get_manifest(s_m.clone())),
        )
        .route("/packages/{name}", get(move |p: AxPath<String>| get_package(p, s_p.clone())))
}
```
`/packages/manifest.json` 单独一条路由、排在 `/packages/{name}` 之前：manifest 在 `<base>/manifest.json`（不是 `packages/` 里），两条路径不能共用一个 handler。

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui panel::packages && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 10 passed，clippy 无输出。

- [ ] **Step 5: Commit**

```
git add crates/bui/src/modules/panel/packages.rs
git commit -m "feat(panel): 客户端包缓存、/packages/* 与不带 key 的安装命令"
```

---

### Task 13: 收口 —— `PanelModule` 接线、模块注册与全量回归

**Files:**
- Modify: `crates/bui/src/modules/panel/mod.rs`（把 Task 1 留的三处「Task 13 接线」填实）
- Modify: `crates/bui/src/serve.rs`（**两处，同一个 commit**：`modules()` 的向量里追加注册那一行，以及把 `serve::tests::p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 的精确列表断言改成「包含 P1 六个模块名 + `"panel"`」的子集断言——只加注册一行会把 P1 这条既有测试打红；裁决 D14 已批准这两处，详见 Step 3）

**前置**：T1…T12 全部合并。**P1 Task 15 已合并到 `v4`**（2026-09-12 核对：`v4` = `eb10eb1`，含 `merge: v4-p1-t15` 与 `merge: v4-p1-t16`，`crates/bui/src/serve.rs` 里已有 `pub fn modules(manifest: Option<Manifest>) -> Registry` 与 `load_cached_manifest`）⇒ **这条前置已满足，本任务不必再等**。

**Interfaces:**
- Consumes: 前面全部；`crate::kernels::{Fetcher, HttpFetcher}`、`crate::serve::modules`（只在注册回归测试里用）、`crate::modules::panel::{api_admin, api_me, api_public, assets, packages, traffic, users}`
- Produces（`Module` 的三个方法落地，签名不变）：
```rust
impl Module for PanelModule {
    fn name(&self) -> &'static str;                                    // "panel"
    fn render(&self, _s: &State, ctx: &RenderCtx) -> Vec<Artifact>;    // 仍然返回空（渲染边界）
    fn routes(&self) -> axum::Router<AppState>;                        // api_admin
    fn public_routes(&self) -> axum::Router<AppState>;                 // api_public + api_me + assets + packages
    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>>;  // 三个后台任务
}
```

三个后台任务（`spawn` 返回的 `JoinHandle` 顺序固定，测试按序断言）：

| # | 任务 | 周期 | 职责 |
|---|---|---|---|
| 1 | `traffic::sampling_loop` | 10 秒 | 采样 → 累加 → ≤30 秒落盘 → 限额执行 → 刷新缓存 → 写 `runtime.extra["users"]` |
| 2 | `users::sync_loop` | 事件驱动 + 60 秒 | 快照重写 + Xray gRPC 差分（含 xray 重启后重放） |
| 3 | `packages::cache_loop` | 每日 | 按 manifest 缓存 `bui-c` / `sing-box` 的两种架构 |

- [ ] **Step 1: 写失败测试**

`crates/bui/src/modules/panel/mod.rs` 的 `mod tests` 里追加：
```rust
    #[tokio::test]
    async fn routes_cover_every_admin_endpoint_and_nothing_public() {
        let h = testsupport::harness().await;
        let router = testsupport::mount(&h.app, PanelModule::with_shared(h.shared.clone()).routes());
        // 受保护路由在这里是裸挂的（没套 require_admin），只验「路由存在」
        for (m, p) in [
            ("GET", "/api/users"),
            ("GET", "/api/stats"),
            ("GET", "/api/online"),
            ("GET", "/api/config"),
            ("GET", "/api/masquerade"),
            ("GET", "/api/bandwidth"),
            ("GET", "/api/port-hopping"),
            ("GET", "/api/hy2/watchdog/status"),
            ("GET", "/api/users/health"),
        ] {
            let (s, _) = testsupport::send(&router, m, p, None, None).await;
            assert_ne!(s, axum::http::StatusCode::NOT_FOUND, "{m} {p} 没挂上");
        }
        // 公开端点不该出现在 routes() 里
        for p in ["/api/sub/alice", "/api/nodes/alice", "/", "/packages/manifest.json", "/api/me"] {
            let (s, _) = testsupport::send(&router, "GET", p, None, None).await;
            assert_eq!(s, axum::http::StatusCode::NOT_FOUND, "{p} 应该在 public_routes 里");
        }
    }

    #[tokio::test]
    async fn public_routes_cover_subscriptions_front_end_packages_and_the_user_domain() {
        let h = testsupport::harness().await;
        let router =
            testsupport::mount(&h.app, PanelModule::with_shared(h.shared.clone()).public_routes());
        for p in [
            "/",
            "/index.html",
            "/app.js",
            "/style.css",
            "/qrcode.min.js",
            "/logo.jpg",
            "/api/sub/alice",
            "/api/subscription/alice",
            "/api/clash/alice",
            "/api/nodes/alice",
            "/api/install-command",
            "/api/me",
            "/api/me/billing",
        ] {
            let (s, _) = testsupport::send(&router, "GET", p, None, None).await;
            assert_ne!(s, axum::http::StatusCode::NOT_FOUND, "{p} 没挂上");
        }
    }

    #[tokio::test]
    async fn spawn_starts_three_tasks_and_hands_the_paths_to_shared() {
        let h = testsupport::harness().await;
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        );
        let m = PanelModule::with_shared(std::sync::Arc::new(s));
        let ctx = crate::reconcile::DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        };
        let handles = m.spawn(ctx);
        assert_eq!(handles.len(), 3, "采样 / 用户同步 / 包缓存");
        assert_eq!(m.shared().paths().base_dir, h.paths.base_dir);
        // 让第一轮跑完再收摊，确认三个任务都没有立刻 panic
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        for hd in &handles {
            assert!(!hd.is_finished(), "后台任务不该自己结束");
        }
        for hd in handles {
            hd.abort();
        }
    }

    #[tokio::test]
    async fn the_panel_module_is_registered_in_serve_modules() {
        let reg = crate::serve::modules(None);
        assert!(
            reg.modules.iter().any(|m| m.name() == MODULE_NAME),
            "serve::modules() 里没有 panel：{:?}",
            reg.modules.iter().map(|m| m.name()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn the_user_health_endpoint_reports_the_summary_after_one_sampling_tick() {
        let h = testsupport::harness().await;
        let ctx = crate::reconcile::DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        };
        traffic::tick(&ctx, &h.shared).await.unwrap();
        traffic::write_health_summary(&ctx, &h.shared).await;
        // 裁决 D2：摘要从 T8 的 `GET /api/users/health` 读，`/api/health` 不加用户段
        let router = h.router();
        let tok = testsupport::token(&h).await;
        let (s, users) = testsupport::send(&router, "GET", "/api/users/health", Some(&tok), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(users["total"], 1);
        assert_eq!(users["blocked"], 0);
        assert_eq!(users["month_key"], "2026-09");
        // 回归锁：P1 的 `HealthResponse` 一个字都没改，`/api/health` 里不该出现用户段
        let out = crate::api::health::get(axum::extract::State(h.app.clone())).await;
        let raw = serde_json::to_value(&out.0).unwrap();
        assert!(raw.get("users").is_none(), "裁决 D2 不批准给 /api/health 加用户段：{raw}");
    }

    #[tokio::test]
    async fn the_full_router_serves_the_panel_login_and_a_subscription() {
        let h = testsupport::harness().await;
        let router = h.router();
        // 前端无鉴权
        let (s_index, _) = testsupport::text(&router, "/").await;
        assert_eq!(s_index, axum::http::StatusCode::OK);
        // 订阅无鉴权
        let (s_sub, _) = testsupport::text(&router, "/api/sub/alice").await;
        assert_eq!(s_sub, axum::http::StatusCode::OK);
        // 管理员端点要 token
        let (s_401, _) = testsupport::send(&router, "GET", "/api/users", None, None).await;
        assert_eq!(s_401, axum::http::StatusCode::UNAUTHORIZED);
        let tok = testsupport::token(&h).await;
        let (s_ok, v) = testsupport::send(&router, "GET", "/api/users", Some(&tok), None).await;
        assert_eq!(s_ok, axum::http::StatusCode::OK);
        assert_eq!(v[0]["username"], "alice");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui panel::tests`
Expected: 失败，`assertion failed: 路由没挂上` / `assert_eq!(handles.len(), 3)` 报 0。

- [ ] **Step 3: 接线**

把 `panel/mod.rs` 里 `impl Module for PanelModule` 的三个方法替换成：
```rust
    fn routes(&self) -> axum::Router<AppState> {
        api_admin::routes(self.shared.clone())
    }

    fn public_routes(&self) -> axum::Router<AppState> {
        api_public::public_routes(self.shared.clone())
            .merge(api_me::public_routes(self.shared.clone()))
            .merge(assets::public_routes(self.shared.clone()))
            .merge(packages::public_routes(self.shared.clone()))
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        self.shared.set_paths(&ctx.paths);
        let fetcher: Arc<dyn crate::kernels::Fetcher> = Arc::new(crate::kernels::HttpFetcher::new());
        vec![
            tokio::spawn(traffic::sampling_loop(ctx.clone(), self.shared.clone())),
            tokio::spawn(users::sync_loop(ctx.clone(), self.shared.clone())),
            tokio::spawn(packages::cache_loop(ctx, self.shared.clone(), fetcher)),
        ]
    }
```
这四个子模块已经由本文件顶部的 `pub mod` 行声明（Task 1 写的），所以直接写 `api_admin::routes(..)` 即可，不需要再加任何 `use`。

再在 `crates/bui/src/serve.rs` 的 `modules()` 里追加注册那一行（P1 Task 15 已把这个函数写出来了，照它的缩进插在 `WatchdogModule` 之后，把 P1 留的那句 P2 注释换掉）：
```rust
            Arc::new(WatchdogModule),
            // P2：面板 API + 采样 + auth 快照 + Xray gRPC（本计划）
            Arc::new(crate::modules::panel::PanelModule::new()),
            // P3 在此追加 ResidentialModule（体检 + 健康切换 + 黑名单）
```
这一行让 `the_panel_module_is_registered_in_serve_modules` 转绿。注意 `PanelModule::new()` 会构造真实的 `XrayClient` / `Hy2Client`，但两者都只在被调用时才建连接（Task 4 / Task 5 的客户端是 lazy 的），所以 `modules(None)` 在测试里不会碰网络。

**同一个 commit 里还必须把 P1 那条断言改成子集断言**（裁决 D14 已批准），否则 P1 的既有测试立刻红（2026-09-12 对 `v4` 的 `crates/bui/src/serve.rs:1016-1023` 核对，它断言的是**精确**的六模块名列表）：
```rust
    #[test]
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            vec!["core-files", "units", "system", "ssh", "certs", "watchdog"]
        );
```
改成（裁决口径「包含 P1 六个模块名」的子集断言，P2 在其中加自己的模块名；**函数名保持 P1 的原样不重命名**——重命名会把改动面从「一处断言」扩大到所有引用这个名字的计划与文档，P5 收口时可统一改名）：
```rust
    #[test]
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        // 裁决 D14：子集断言 —— P1 的六个模块名必须都在，P2 加自己的 "panel"；
        // P3 追加 ResidentialModule 时只在这个数组里再加 "residential"，不必再走一轮裁决
        for want in [
            "core-files",
            "units",
            "system",
            "ssh",
            "certs",
            "watchdog",
            "panel",
        ] {
            assert!(names.contains(&want), "{want} 不在 serve::modules() 里：{names:?}");
        }
```
⚠️ 这是 `serve.rs` 上的**第二处**改动（注册一行 + 这一处断言），总纲「裁决记录」的 D14 已经批准；别顺手扩大到别的断言，也别改函数名。

- [ ] **Step 4: 全量回归**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check web/app.js
```
Expected: `cargo test -p bui panel::` 的通过数 = 各任务 Expected 之和（T1 9 + T2 7 + T3 7 + T4 9 + T5 7 + T6 21 + T7 11 + T8 17 + T9 8 + T10 2 + T11 5 + T12 10 + T13 6 = **109**）；`api::` 比 P1 多 2 条（T0 的两条 D1 回归锁）；P0 / P1 / P4 的既有测试全绿；clippy / fmt 无输出。

- [ ] **Step 5: 人工核对四处改动与六处 P1 改动面**

```bash
# spec §4.3 改动 1：前端不再走 /api/manage，也不再存明文密码
grep -n "api/manage\|localStorage.setItem(\"ap\"\|localStorage.getItem(\"ap\")" web/app.js   # 期望：无输出
grep -n 'api("/users", {' web/app.js                                                          # 期望：命中一处
# spec §4.3 改动 2：三处已删
grep -rn "auth/hysteria\|kernel-downloads\|install-key\|getOrCreateInstallKey" crates/bui/src  # 期望：无输出
grep -n "packages/bui-c-install.sh" crates/bui/src/modules/panel/packages.rs                   # 期望：命中
# 改动 3（/api/health 的用户段）与改动 4（住宅端点）
grep -n "api/users/health" crates/bui/src/modules/panel/api_admin.rs                          # 期望：命中（裁决 D2）
git diff v4 -- crates/bui/src/api/health.rs                                                    # 期望：无输出（一个字都没改）
grep -n "fn public_routes" crates/bui/src/reconcile/mod.rs                                     # 期望：命中
# 渲染边界：P2 不碰这四份内核配置
grep -rn "xray-config.json\|config.yaml\|singbox-relay.json\|Caddyfile" crates/bui/src/modules/panel  # 期望：只在注释里出现
# P2 不在 <base> 顶层造新文件（auth-hook.log 超限是原地截断，不是 rename 出 .log.1）
grep -rn "auth-hook.log.1\|fs::rename" crates/bui/src/modules/panel/auth_hook.rs   # 期望：只在注释里出现，没有 fs::rename
# P1 边界：只动了许可/已批的六处；serve.rs 恰好两处（注册一行 + 子集断言）；api/health.rs 零改动
git diff --stat v4 -- crates/bui/src/serve.rs crates/bui/src/modules/mod.rs crates/bui/src/reconcile/mod.rs \
    crates/bui/src/api/mod.rs crates/bui/src/main.rs
git diff v4 -- crates/bui/src/serve.rs      # 期望：只有两处 hunk —— modules() 里的注册行（含注释），与改成子集断言的那条测试
# 安装命令不跳过证书校验、不带 key
grep -n "curl -fsSL" crates/bui/src/modules/panel/packages.rs   # 期望：无 -k、无 key=
```

- [ ] **Step 6: Commit**

```
git add crates/bui/src/modules/panel/mod.rs crates/bui/src/serve.rs
git commit -m "feat(panel): PanelModule 接线（管理员/公开路由 + 三个后台任务）与模块注册"
```

---

## 自查

**1. spec 覆盖**（逐节对照，每条指到任务）

| spec 条目 | 落在哪 |
|---|---|
| §3.2 `bui auth-hook <addr> <auth> <tx>`：极简路径、不初始化 tokio/tracing/state、≤2 秒硬超时、fail-closed、自写 `auth-hook.log`、常量时间比对、stdout 打印 `user_id` | Task 3（`decide` / `run` / `run_with` / `log_line`），入口改动在 `main.rs` |
| §3.2 快照在用户变更、限额变化时原子重写；形状照总纲 C5 | Task 2（`Snapshot` / `write_if_changed`）+ Task 6（`sync_users` 第 ① 步）+ Task 7（每轮 `tick` 调 `sync_now`） |
| §3.2 `userpass` 退路开关「M3 就实现好」 | **不在 P2，且目前无任务承接**：它要改 `bui_schema::render::hysteria`（把 `auth.type` 从 `command` 切回 `userpass` 并写用户表）+ P1 的重启映射，属 P0/P1 范围。总纲把 M3 归 P2，**不指派就是 M3 验收缺项** ⇒ 见「待裁决与需落字」第 1 条 |
| §3.3 tonic + vendored 10 个 proto；AddUser/RemoveUser 两层 TypedMessage；`email = user_id`；`QueryStats(pattern="user>>>", reset=true)`；CLI 退路 `xray api rmu` | Task 4（proto / `add_user_request` / `remove_user_request` / `deltas_from_stats` / `rmu_args`）+ Task 6（差分与退路调用点） |
| §3.3 `RemoveUser` 不断既有连接（接受该窗口，调研 X10） | 计划「渲染边界」一节的 D6 + Task 6 的无条件 RemoveUser 与 `NRestarts` 重放；**面板说明文案不在前端**（前端只改 `app.js` 那两处是硬约束），归 M3 验收清单 ⇒ 见「待裁决与需落字」第 2 条 |
| §4.1 用户 CRUD：state 变更 → 快照重写 + gRPC 增删，用户变更不重启内核 | Task 8（handler 改 state + 发事件）+ Task 6（反应器）；不重启由 P1 Task 10 的 `structural_hash` 保证 |
| §4.1 v3 `protocol` ↔ 权益映射、`nodes_for` 是唯一节点来源 | Task 6（`protocol_label` / `protocols_from_label`）+ Task 9（四个端点都调 `nodes_for`） |
| §4.1 订单与 SKU 只定义结构不实现 | P0 已定义 `Order` / `CatalogItem`；Task 10 的 `/api/me/orders*` 是 501 桩 |
| §4.2 10 秒采样（两个 `/traffic?clear=1` + `QueryStats`）、内存累加、≤30 秒落盘 | Task 7（`sample_once` / `tick` / `FLUSH_INTERVAL_SECS`） |
| §4.2 在线 = 两个 `/online` 并集 ∪ 30 秒内有 Xray 增量 | Task 7（`tick` 第 ⑤ 步 + `XRAY_ONLINE_WINDOW_SECS`） |
| §4.2 月度重置（spec 字面是「服务器本地时间每月 1 日 00:00」） | Task 6（`month_key`）+ Task 7（`apply_sample` 的 `rolled` 分支，含「无流量也翻月」）。**实现按 UTC 月初**（决策 D7，`host.now()` 是 `now_utc`）：两台 VPS 时区就是 UTC，差异为零，但与 spec 字面不一致 ⇒ 见「待裁决与需落字」第 3 条 |
| §4.2 到期/超限/禁用 → 快照标记 + `/kick`（体 `["<user_id>"]`）+ `RemoveUser`；恢复反向 | Task 6（`is_blocked` / `blocked_set` / `sync_users`：被拒用户**无条件** RemoveUser + `Applied::xray_removed` 去重 + `NRestarts` 重放，回归测试 `a_blocked_user_is_removed_even_if_this_process_never_added_him` / `a_disabled_user_is_removed_on_the_first_sync_too` / `an_xray_restart_replays_the_removal_of_a_blocked_user`）+ Task 7（`tick` 第 ④ 步的 kick、恢复由 `sync_users` 的 `to_add` 覆盖，`raising_the_quota_restores_the_user`） |
| §4.2 `/api/stats`、`/api/online` 读同一份缓存 | Task 1（`SampleCache`）+ Task 7（写）+ Task 8（读） |
| §4.3 管理员域全部端点逐个列表 | 计划的「v3 端点逐个契约」表（31 行）+ Task 8 / 9 / 10 / 11 / 12 |
| §4.3 改动 1：`POST /api/users` 走 JWT、删 `/api/manage`、前端改一处、删 localStorage `ap` | Task 8 + Task 11（`web/app.js` 两处 diff + 回归锁测试） |
| §4.3 改动 2：删 `/auth/hysteria`、`/api/kernel-downloads`、install-key；`/api/install-command` 不带 key；`/packages/*` 直接可下 | Task 12（`install_command` / `public_routes`）+ Task 8 的 `the_deleted_v3_endpoints_are_gone` |
| §4.3 改动 3：`/api/health` 增漂移、watchdog、上游体检 | 漂移与 watchdog 归 P1 Task 13，上游体检归 P3；**P2 不碰 `/api/health`**（裁决 D2 不批准加用户段），用户与流量摘要走 `GET /api/stats`（v3 既有契约）与 T8 新增的 `GET /api/users/health` |
| §4.3 改动 4：新增五个 `/api/residential/*` | **归 P3**，P2 不碰 |
| §4.3 无鉴权组 + 新增 `/api/nodes/<user>` | Task 9；无鉴权这件事本身由 **Task 0** 的 `Module::public_routes()` 通道打开（裁决 D1） |
| §4.3 用户域七个端点全部 501，结构见附录 A | Task 10（`ME_ENDPOINTS` + 模块文档里抄全了附录 A） |
| §4.4 三种订阅由 `bui-schema` 从同一节点列表渲染 | Task 9（`uri_list` / `singbox(dial_ip=public_ip)` / `clash`） |
| §6「服务端内核缓存继续维护 sing-box 与 `bui-c` 的 Linux 二进制」 | Task 12（`sync_once` / `cache_loop`） |
| §1「前端 `rust-embed` 嵌入 `bui`」 | Task 11 |
| 总纲 C5 `auth-snapshot.json` 形状 | Task 2（`shape_is_exactly_c5` 逐字段断言） |
| 总纲 C4 manifest 消费 | Task 12（`artifacts` 键名 `<name>-linux-<arch>`、未知字段忽略由 P1 的 `Manifest` 负责） |
| 裁决记录「`/api/nodes` 载荷 = 直接序列化，P2 不得自行拼 JSON」 | Task 9（`nodes_endpoint_serializes_nodes_for_and_split_rules_verbatim` 与 `NodesPayload` 逐字比对） |

**2. 占位扫描**：全篇无 TBD / TODO /「添加适当的错误处理」/「类似 Task N」/「为上面写测试」。每个代码步骤给的都是可直接落地的 Rust / Bash / JS diff；没有「先给错代码再用散文纠正」的写法。四处本来容易写成散文的地方都给了完整内容：v3 端点契约给了 31 行逐端点请求/响应、`web/app.js` 给了 unified diff、proto vendoring 给了 URL 循环与实测 SHA256SUMS、`rust-embed` 与 tonic 的 feature 组合给了「缺了会报什么错」。`tonic` / `tonic-prost-build` / `protoc-bin-vendored` / `rust-embed` 的组合与生成代码的模块树、`Account` 字段名、`query_stats` / `alter_inbound` 的方法名都在本机实测过（2026-09-12），不是照文档猜的。

**3. 类型一致性**（跨任务逐个核对）
- `TxRx` / `SampleCache`（含 `last_flush_at`）/ `Applied`（含 `xray_removed`）/ `Shared`（含 `sync_guard()`）/ `XrayApi` / `Hy2Api` 只在 Task 1 定义，Task 4/5 实现、6/7/8 消费，字段名与方法名全篇一致；`FakeXrayInner.error_text`（Task 1）只被 Task 6 的三条容错测试消费。
- `Snapshot` / `SnapshotUser`（Task 2）被 Task 3 的 `decide` 与 Task 6 的 `sync_users` 共用；`write_if_changed(path, &snap) -> Result<bool>` 的返回语义（`false` = 内容未变）在两处断言里一致。
- `users::{PanelUser, CreateRequest, UpdateRequest, SyncOutcome, BlockReason}` 与 `users::{month_key, is_blocked, blocked_set, sync_users, sync_now, sync_loop}` 在 Task 6 定义，Task 7 与 Task 8 按同名同签名调用；`blocked_set(&State, &BTreeMap<Uuid, TxRx>, OffsetDateTime)` 的三参数顺序三处一致。
- `traffic::{Sample, sample_once, apply_sample, to_uuid_map, by_username, tick, write_health_summary, sampling_loop}` 在 Task 7 定义，Task 13 按名接线。
- 每个 API 文件都导出 `fn routes(Arc<Shared>) -> Router<AppState>` 或 `fn public_routes(Arc<Shared>) -> Router<AppState>`（`api_public` / `api_me` / `assets` 用不到 `shared`，参数写成 `_shared` 保持签名统一），Task 13 的 `merge` 因此不需要任何特例；也正因为签名统一，T8…T12 的整套路由测试都能用同一个 `testsupport::full_with(&h.app, routes, public)`。
- `testsupport::{mount, full, full_with, raw, send, text, token, harness, Harness}` 只在 Task 1 定义。谁用哪一个有固定口径：**`mount`** = 裸挂子路由（只验「路由存在 / handler 逻辑」，不过鉴权）；**`full_with`** = 整套装配但只挂本任务的子路由（T8…T12 验「401 / 无鉴权可达」用它）；**`full`** = 整套装配挂一个完整 `Module`（T1 的 `ProbeModule` 回归锁用它）；**`Harness::router()`** = 整套装配挂 `PanelModule`，**只有 T13 能用**（T1…T12 期间那两个方法还是空的，会一律 404）。
- 消费 P1 的符号逐个对过 P1 计划：`Store::{read, update}`、`write_atomic`、`Runtime::{read, update}`、`RuntimeData.extra`、`WatchdogRecord` 四个字段、`Host::{read_file, write_file, run, which, unit_property, now}`、`FakeHost::{with, ops, text, mode, advance, clear_ops}` 与 `ops` 字符串格式（`write:<path>:<mode>`、`run:<program> <args>`）、`DaemonCtx` 五个字段、`AppState` 七个字段、`Event::StateChanged(&'static str)`、`EventBus::{new, send, subscribe}`、`MANAGED_UNITS`、`api::auth::{issue_token, hash_password, verify_password, LoginLimiter}`、`api::health::{get, HealthResponse}`、`kernels::{Manifest, Asset, Fetcher, HttpFetcher, sha256_hex}`、`serve::{modules, load_cached_manifest}`、`paths::{state_file, runtime_file, manifest_file, auth_snapshot_file}`、`util::{fmt_rfc3339, parse_rfc3339}`、`redact::{secret, url_credentials}`。
- 消费 P0 的符号逐个对过 `crates/bui-schema/src` 的真实代码：`Paths` 三个 pub 字段、`Reality::{sni, short_id}`、`Residential::default_group`、`ResidentialGroup::pool_active`、`Usage` 四个字段、`Entitlements` 五个字段、`TrafficLimit` 两个字段、`nodes_for(&User, &NodeParams, &Residential)`、`SplitRules::from_group`、`subscription::{uri_list(&[Node], &str), singbox(&[Node], &SplitRules, &str), clash(&[Node], &str, &SplitRules)}`、`DEFAULT_GROUP`。`Upstream` **没有** `Default`，所以 Task 9 的测试逐字段构造它。

**4. 与 P1 的改动面**：**六处**，逐处有归属与裁决状态——
1. `modules/mod.rs` 一行 `pub mod panel;`（T1，C2 已许可）；
2. `serve.rs` 的 `modules()` 注册一行（T13，C2 已许可）；
3. `serve.rs` 的 `serve::tests::p1_registers_exactly_six_modules_and_shares_the_manifest_handle` 改成「包含 P1 六个模块名 + `"panel"`」的子集断言（T13，**裁决 D14 已批准**；P3 追加 `ResidentialModule` 时只在数组里再加 `"residential"`，不必再裁）；
4. `reconcile/mod.rs` 的 `Module::public_routes()` 默认方法（**T0**，**裁决 D1 已批准**）；
5. `api/mod.rs` 的 `router()` 在 `require_admin` 外合并 `public` + 两条回归测试（**T0**，**裁决 D1 已批准**；裁决原文：这两个文件是 P2 唯一可改的 P1 文件对，改动限于这两处）；
6. `main.rs` 的 `Command::AuthHook` 那一支（T3，**D3 已由 P1 Task 1 的代码注释预授权**）。

`api/health.rs` **不在改动面里**：裁决 D2 不批准给 `HealthResponse` 加用户段，摘要改走 T8 的 `GET /api/users/health`（写入侧仍是 `runtime.extra["users"]`，不动 P1 的 `RuntimeData`），T13 还有一条回归锁断言 `/api/health` 的 JSON 里没有 `users` 键。三条裁决（D1 / D14 批准、D2 不批准）已在总纲「裁决记录」落字，C2 的许可随之为：`modules/mod.rs` 一行、`serve.rs` 注册一行 + 一处子集断言、`reconcile/mod.rs` 与 `api/mod.rs` 的公开路由通道、`main.rs` 的 auth-hook 分支。

**P2 在 `<base>` 顶层新造的文件：零**。`auth-hook.log` 与 `packages/` 都已在 P1 `reconcile::drift::BASE_WHITELIST` 的 13 项里（2026-09-12 核对），`auth-snapshot.json` 同样在；T3 的日志超限走**原地截断**而不是 `rename` 成 `auth-hook.log.1`，所以漂移扫描不会因为 P2 多报任何一条 `stray_file`，也就不需要请 P1 改白名单。

**5. 自查发现并已修掉的三处**（第二轮审查）：①T6 的 `sync_users` 原来只从 `applied.xray_users` 里筛 `to_remove`，导致「本进程没 AddUser 过的被封用户永远不会被 RemoveUser」，与 T7 的 `exceeding_the_quota_kicks_both_instances_and_blocks_the_snapshot` 直接冲突（该测试在全新 Harness 上只跑一轮 `tick` 就断言出现 `remove:vless-direct:<id>`）——改成按期望态无条件删 + `xray_removed` 去重，并补了三条回归测试；②`serve.rs` 那一行原在 T1——挪到 T13，与三个方法的接线同一个 commit；③`sync_users` 原来全程持 `applied` 跨 2×N 次 gRPC await，`write_health_summary` 会被 xray 掉线拖住——拆成「轮次锁 + 两段短临界区」。

**自查发现并已修掉的三处**（第三轮审查，对 `v4` = `eb10eb1` 的真实代码复核）：
1. T8…T12 里六条「整套 Router」测试原来用 `Harness::router()`，而 `PanelModule::routes()` / `public_routes()` 要到 T13 才接线 ⇒ 那些路径在 T8…T12 阶段返回 **404 而不是 401 / 200**，每个任务的 Expected 通过数都对不上；实现者要么过不了自己任务的验收，要么各自提前改 `panel/mod.rs`（同一文件、并行分支，与「并行合并零冲突」直接冲突）。改法：T1 的 testsupport 多一个 `full_with(app, routes, public)`（内部用一个临时 `Adhoc` 结构体实现 `Module`），那六条测试改挂自己的子路由，`h.router()` 只留给 T13。
2. T3 的日志轮转原来 `rename` 成 `<base>/auth-hook.log.1`，而 P1 的 `BASE_WHITELIST` 只有 `auth-hook.log`、`TRANSIENT_SUFFIXES` 只有 `.tmp` / `.new` ⇒ 第一次轮转之后每 10 分钟的漂移扫描都报 `stray_file`（`/api/health` 永久 degraded），`bui reconcile --force` 还会把它删掉。改法：**原地截断**（保留尾部 `LOG_KEEP_BYTES` 重写同一个文件），不产生兄弟文件、也不必请 P1 改白名单。
3. T13 只加注册那一行会打红 P1 既有测试 `serve::tests::p1_registers_exactly_six_modules_and_shares_the_manifest_handle`（精确六模块名列表），而计划宣称「P1 既有测试全绿」、`serve.rs` 又只许多一行 ⇒ Step 4 的全量回归必挂。改法：把它改成「包含 P1 六个模块名 + `"panel"`」的子集断言，写进 T13 Step 3；总纲已按**裁决 D14** 批准，P3 后续只在子集数组里加 `"residential"`。

## 待裁决与需落字（M3 验收前必须有裁决记录）

下面每一条都不是「实现细节」——要么 spec 里有明确要求而本计划没人接，要么本计划的处置与 spec 字面不一致，或者干脆是 spec 没写过的行为变更。**请 Fable 在裁决记录里逐条落字**（批准 / 驳回 / 改派），别让它们靠「计划里提过一句」蒙过 M3。

| # | 事项 | spec 依据 | 本计划的处置 | 需要的裁决 |
|---|---|---|---|---|
| 1 | `auth.type` 从 `command` 切回 `userpass` 的**配置开关** | §3.2、§11 明写「`userpass` 退路必须在 M3 就以配置开关实现好」 | **无任务承接**：要改 `bui_schema::render::hysteria`（写用户表）与 P1 的重启映射，都在 P2 的改动许可之外 | **指派**给 P0 + P1（各一个新任务），并给出 M3 前的完成时点。不指派 ⇒ M3 验收缺这一项，请在裁决记录里写明「M3 接受缺此项」 |
| 2 | 面板说明「超限/到期用户的既有连接可能延续到其断开」 | §3.3（成因见调研 X10：xray 的 `RemoveUser` 不断已建连接，hysteria 侧同理） | 因「前端界面一个字都不改，只动 `web/app.js` 的那两处」是硬约束，本计划把这句话推到 `docs/superpowers/checks/` 的 M3 清单，**前端里不会出现** | **确认接受**「说明只在文档、不在界面」。若要求上界面，请批准 P2 多改一处 `web/index.html`（T11 加一条 DOM 断言测试） |
| 3 | 月度重置的时区 | §4.2 写「服务器本地时间每月 1 日 00:00」 | 决策 D7：用 `host.now()`（`now_utc`）算 `month_key` ⇒ 按 **UTC 月初**重置 | **落字**：两台机时区就是 UTC、零差异，接受 UTC 口径；或指定东八区（改 `users::month_key` 一处 + 三条测试） |
| 4 | `xray-config.json` 只写当前有效用户 | §4.2「超限/到期 → Xray RemoveUser」的长期正解 | 决策 D6 的长期修法两条（P0 加派生字段 / P1 Task 10 过滤）都超出 P2 许可。P2 这一版靠「无条件 RemoveUser + `NRestarts` 重放 + 60 秒安全网」兜住运行期，**xray 重启到下一轮同步之间仍有一个窗口** | **裁决**走 (a) 还是 (b)，指派到 P0 或 P1；或明确「接受这个重启窗口，不做长期修法」 |
| 5 | spec 未明写的行为变更（五条） | — | D5：`POST /api/bandwidth` → 501（GET 恒 `{"up":0,"down":0}`）；D8：客户端 `client_sing_box` 与 `sing_box` 版本不一致只 `warn!` 并仍缓存；D11：改管理员密码同时轮换 `jwt_secret`（旧 token 立刻失效）；D13：`sni` / `speed` 接受但忽略，`/api/users` 投影里不再有 `limits.speedLimit`；D9 / D10：删 `/api/version`、`/api/kernel-versions`、`GET /packages` 目录列表 | 五条都是合理处置，但**都不是 spec 写过的**，请在裁决记录里逐条落字。D9 / D10 另带一条运维前提：**服务端切 v4 前先给客户端装 `bui-c`**（旧 v3 `b-ui-client.sh` 会调这些端点，见「P1 边界」一节末尾），写进 M1 / M4 清单 |
| 6 | 流量采样的 `pending` 只在内存 | §4.2「最多每 30 秒合并落盘一次」 | `systemctl restart b-ui` / SIGTERM 时后台任务随进程结束，**最多丢 30 秒流量**，`SampleCache` 与 `Applied` 下一轮重建（幂等）。P2 不加退出钩子——统一的 shutdown 信号在 P1 的 `serve` / `finish_self_restart` 那一侧 | **落字为已知降级**（写进 M3 验收清单）；若不接受，指派 P1 加 shutdown 钩子并让 P2 在其中 flush |
| 7 | T3 的日志超限处置（P2 会不会在 `<base>` 顶层造新文件） | 总纲 C3 与 spec 都没提 `auth-hook.log` 的轮转 | 本轮已改为**原地截断**（保留尾部 `LOG_KEEP_BYTES` 重写同一个文件）。备选 (b)「轮转出 `auth-hook.log.1` 并请 P1 在 `BASE_WHITELIST` 加一项」**已否掉**——那是又一处 P1 改动，且 `.log.1` 一旦漏加白名单就是永久 degraded + 被 `--force` 删除 | **只需确认**「P2 不在 `<base>` 顶层新增任何文件、不需要动 P1 的 `BASE_WHITELIST`」这条口径；若主理人更想留一份历史日志，就要改回 (b) 并批 P1 白名单多一项 |
| 8 | xray gRPC「已存在 / 不存在」的错误文案 | — | Task 6 的 `target_already_reached` 宽匹配 `already` / `not found` / `not exist`（大小写不敏感）。文案**未经实机核对**，调研 X1–X11 没记 | **M3 联调核对一次**（bwg-rick）。匹配不上的代价只是多一条 error + 一次幂等的 CLI 退路，不会误判成功 |

---

## 移交与未覆盖（明确归属，避免无人认领）

| 项 | 归属 | 说明 |
|---|---|---|
| `/api/residential/*` 九个端点、`/api/health` 的 `residential` 段 | **P3** | P2 的 `PanelModule::public_routes` 与 `routes` 里一条都没挂 |
| `auth.type` 从 `command` 切回 `userpass` 的退路开关（spec §3.2、§11） | **P0 + P1，待指派** | 要改 `bui_schema::render::hysteria`（写用户表）与 P1 的重启映射；P2 只保证钩子这条路走得通。**目前无任务承接**，见「待裁决与需落字」第 1 条 |
| 「超限/到期用户的既有连接可能延续到其断开」的面板说明文案（spec §3.3） | **M3 验收清单** | 要动 `web/index.html`，与「前端只改一处」冲突；建议写进 `docs/superpowers/checks/` 的 M3 清单而不是前端 |
| `xray-config.json` 只写有效用户（决策 D6 的长期修法） | **待 Fable 裁决**（P0 或 P1） | P2 用「无条件 RemoveUser + `NRestarts` 重放 + 60 秒安全网」兜住了运行期，但每次 xray 重启仍有一个「重放前」的窗口。见「待裁决与需落字」第 4 条 |
| `/api/bandwidth` 是否真支持（决策 D5） | **待 Fable 裁决** | 真支持要 P0 加字段 + 改 hysteria 渲染 |
| 真内核联调：`AlterInbound` / `QueryStats` / `auth.command` 的实际行为、200 建连/秒压测 | **M3 / M5 验收**（bwg-rick） | 本计划的单元测试只验组装与解析；调研 X11 与 §1.4 的未决项由 M3 覆盖 |
| `bui-c-install.sh` 本体 | **P4 Task 13** | P2 只负责嵌入并在 `/packages/bui-c-install.sh` 发出去 |
| `manifest.json` 的生成与发布 | **P5** | P2 只反序列化与消费（C4 键名固定） |
| 采样 `pending` 的退出前 flush（最多丢 30 秒流量） | **M3 已知降级 / 或 P1** | 需要一个统一的 shutdown 信号，在 P1 的 `serve` 那一侧；见「待裁决与需落字」第 6 条 |
| 「服务端切 v4 前先给客户端装 `bui-c`」的运维顺序 | **M1 / M4 清单** | D9 / D10 删掉的 `/api/version`、`/api/kernel-versions` 与 `packages/` 里的旧文件名都是 v3 `b-ui-client.sh` 在用的；不按这个顺序切，baiyi 上的旧客户端自更新与内核同步会一起失效 |
