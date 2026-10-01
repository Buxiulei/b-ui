# Residential Lifecycle Owner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 以同一个 Rust 住宅 owner 完成服务激活、完整门位读回及确认，消除各入口提前成功和并发写者。

**Architecture:** 保留已有单 consumer reconcile，住宅服务写操作统一进入专用 owner。严格 API inventory/门位屏障与同步 apply 的待激活结果组成完整事务；进程间 lease 分隔 daemon 与离线 CLI。

**Tech Stack:** Rust workspace、Tokio、现有 Host/FakeHost、Store/Runtime、stock sing-box 1.14.2、systemd/UDS。

**Spec:** `docs/superpowers/specs/2026-10-01-residential-lifecycle-owner-design.md`

## Global Constraints

- BUI 使用 Rust；只使用上游原版网络内核，sing-box 本次只针对 1.14.2。
- 不修改 Mac、网络路由规则、上游源代码或日志级别；不实施 relay generation bank。
- 所有住宅控制写者统一 owner，在线 CLI 走 UDS，离线写者与 daemon 使用同一进程间独占 lease。
- 住宅通知事件不承担正确性；完整门位读回之前不得确认住宅配置 keys 或证书已生效。
- Unknown 不等于失败探测阳性，门位收敛不等于上游业务健康。
- 记录不含代理密码或私钥；持久激活记录不能采用 best-effort runtime 更新。
- 更新 workspace 版本、锁文件与 CHANGELOG 为 4.1.5，创建新的 stacked PR，不 merge。

## Review Focus

- `/version` 返回 200 但错误版本；`/proxies` 返回坏 JSON、空对象、缺门或 gate 不是 Selector：激活必须失败。
- PUT 2xx 但值没改变或最后读回失败：不能确认新 keys。
- 开门期间 disable/rotate/quota flush 发生：旧授权不能恢复，receipt 有明确最新投影线性化点。
- HTTP 等待者取消但 spawn_blocking 尚在运行：下一写者不能进入，事务完成可恢复。
- daemon 启动 handoff 与住宅 ExecStartPre：同一 lease 不重入，也不存在两个同时写者。

---

### Task 1: Strict gate inventory and activation barrier

**Files:** 修改 `crates/bui/src/modules/panel/hy2resi.rs`、`gates.rs`、`mod.rs`、`fakes.rs`；授权序列化需要时修改 `traffic.rs`、`users.rs`、`api_admin.rs`、`api_me.rs`、`crates/bui/src/state/store.rs`。测试与对应模块同置。

**Interfaces:** 提供从完整候选配置解析出的 `GateManifest`，严格的 `GateInventory`（含 type/now/all），及 `gates::restore_and_verify(ctx, shared, manifest, deadline) -> Result<GateActivationPermit>`。`GateActivationPermit` 包含最终目标投影及其摘要，并继续私有持有 gate 写锁与 Store 状态发布 fence，直至 owner 持久提交；不是内核配置证明。具体 type 路径在报告中固定，后续 owner 直接消费。Store 提供持有当前 writer mutex 的只读 publication permit；所有 pending 更新/转入 usage 与 gate 写路径统一 gate 锁，锁顺序 gate→Store publication→pending。

- [x] 写失败用例：坏 JSON / 缺 proxies / gate 缺 now 或成员、完整候选缺门、PUT 不生效、末次 GET 失败、额外旧门 deny、授权变化不能恢复旧 allow、pending flush 不少算流量、整体 deadline。独立 literal oracle，不用被测 expected 函数生成断言。
- [x] `cargo test -p bui --locked modules::panel::`，保存 RED 的业务失败证据；纯缺符号/拼写错误不是证据。
- [x] 用 typed inventory 解析代替空表降级；新增严格屏障，周期收敛保留允许候选池尚未加载的语义。所有 gate 写入口同一序列化；ready 等待不长占 gate lock，deadline 覆盖整体；grant 只按当前授权。Store writer guard 跟随实际 blocking 写盘及 cache publication，即使等待者取消也不提前释放；final GET 后与 owner commit 前校验到期时间。
- [x] 再运行范围测试并自审；GitNexus impact 对所有修改符号在修改前完成。固定接口、保存测试命令与结果到 report，再作任务提交（detect_changes 在提交前）。

测试独立 oracle 示例：

```rust
// 当前 production parser 对错误载荷给空表，此用例必须先失败。
assert!(parse_gate_inventory(r#"{"proxies":{"gate-a":{"type":"Selector","all":["deny","slot-0"]}}}"#).is_err());
// gate-a 被 PUT 为 slot-0 后 GET 仍为 deny，应返回失败，不能出 VerifiedGates。
// 待停用门优先 PUT deny，额外 gate-old 最终值为 deny。
```

### Task 2: One residential lifecycle writer and durable completion

**Files:** 新建 `crates/bui/src/residential_lifecycle.rs`（可按 owner/record/lease 分为相邻文件）；修改 `reconcile/apply.rs`、`reconcile/mod.rs`、`serve.rs`、`api/state.rs`、`api/system.rs`、`modules/certs.rs`、`modules/watchdog.rs`、`modules/panel/auth_http.rs`、`commands/install.rs`、`commands/upgrade.rs`、`commands/config.rs`、独立 live `commands/import_v3.rs` 的 lease 边界、`commands/harden_ssh.rs` 与最小 UDS intent route 的单写者边界、`commands/nft.rs` 读取住宅已发布拓扑绑定的最小边界、`kernels/mod.rs`、Host 最小原子 rename 方法及 RealHost/FakeHost listening probe、对应测试构造器、必要 paths/drift whitelist；`state/runtime.rs` 仅实际 blocking 持久写者的取消安全与 daemon lease 随行。

**Interfaces:** 消费 Task 1 的 `GateManifest` 与 `restore_and_verify`。住宅专用 owner 共享给 daemon/app/module；apply 提供待激活住宅动作、候选 keys 与可回滚文件，不自行确认这些 keys。操作来源枚举区分 reconcile/manual/watchdog/certificate/recovery。进程间 lease 持有范围覆盖整个控制写者，prestart 例外；活跃 receipt 包含 requested_config_sha、操作 ID、授权摘要与前后相同的观察实例。

- [x] 写现有行为的失败用例：start/restart 返回 0 但 gate 不恢复不能成功；apply keys 未过屏障不能确认；cert restart 失败不能记新SHA；unknown listener 不触发重启；双入口和请求取消不能并行写；离线CLI不能因UDS不可用侵入daemon所有权；maintenance stop不会被watchdog撤销。
- [x] 用 Linux target 跑这些用例，保存 RED。优先复用真实 Store/Runtime 与 fake 外部 Host/API。
- [x] 住宅 owner 实现 `prepare → prepared record → publish → activate → observe → restore/readback → active record`，失败记录并恢复上一候选，恢复也验证；资源提交锁覆盖替换/写盘/激活，下载留在锁外。复用 KernelInstaller 将下载校验结果安装至同文件系统 op-id 候选路径（非 live bin），通过 owned PathBuf+SHA 交接；在 owner guard 内原子 rename 提交。校验使用候选 binary，apply 同时 defer 第9步住宅 start/stop 及第12步 restart。sing-box 是住宅与 relay 的共享 binary：在资源 owner 内发布后，relay 沿既有 apply/units_for_binary 语义单次激活，不能用旧 binary 校验或漏掉消费者；必需下载仍在锁外，回滚说明两消费者的恢复结果。操作任务独立于 HTTP 等待者存活；Store/Runtime 实际阻塞写入持有 writer 与 daemon lease，shutdown 取消等待者不能提前释放机器写权（Runtime 保留现有 best-effort 语义）。
- [x] 将住宅 apply、startup、API、watchdog、证书、安装/升级/离线对账接入同一所有权。不把广播当屏障，不新加全局 reconcile mutex。daemon 在打开将用于写回的 Store/Runtime 前取得 lease；离线 config/install/upgrade/reconcile 及写受管状态的 import-v3 在读取将用于写回的状态及任何 state/runtime/auth-snapshot/live 文件写入前取得同一 lease，lease 忙且 UDS 不通则返回 owner unavailable，不执行本地写。正常在线 upgrade 走最小 `/api/upgrade` UDS intent，由现 daemon 的 owner 完成后 handoff；rollback 要求离线独占。移除住宅重复激活/异步成功路径；未完成记录启动后重新验证。SELF start/restart handoff 在 lease 释放后；nft prestart 不取 lease。
- [x] `harden-ssh` 在线经最小 UDS intent 交同一控制写者，离线读取写入依据前持有控制 lease；UDS 不通且 lease busy 则拒绝。保留原加固行为；SSH-only apply 关闭住宅网络清理，并覆盖同路径写入竞争及正常在线使用测试。
- [x] 首装缺证书且住宅未部署/启动时明确 awaiting_certificate/pending，允许必需 Caddy/bootstrap/handoff 前置继续，住宅配置完成校验后才发布、确认 active/keys。普通校验故障不得降成待证书，共享 binary 失败不能导致 relay 使用不一致候选。
- [x] 共享 binary 的 relay 手动服务动作与 watchdog restart 通过同一资源 guard，保留既有归因与广播；用持锁竞争测试证明不会提前执行，不新增住宅屏障或 generation 行为。
- [x] watchdog 的单步 check_nft 在资源 guard 后取得最新 State 与 owner 的已发布住宅绑定，执行既有比对/重放；apply 与 prestart 读取同一绑定，候选 hold/pending 不提前重写旧表。previous/target 严格 prepared 后再发布，新 prepared 前优先消费既有未完成操作且不覆写 Unknown/foreign 证据，恢复旧配置前恢复旧绑定并验证表/监听，后续 watchdog 仍保持旧端口；仅 hop/compat 变化也触发完整发布，未知绑定 fail closed。排队中 State 变更、changed-port 失败恢复和各绑定发布中断测试使用实际规则 payload；规则正文和 healthy no-write 不变，不包整个 watchdog，prestart 保持免 lease 重入。
- [x] 在较长的 startup reconcile/住宅barrier前，fresh鉴权snapshot并实际bind独立AuthHttp listener确认成功；测试住宅屏障未完成时native auth仍可响应。RealHost必要proc监听表读取错误保留为Unknown而非空集合；测试读取故障不累计restart证据，不增加平台兼容工作。
- [x] 跑新用例及完整 `cargo test --workspace --locked`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`。实际失败修复后再重跑覆盖范围，不无限复测。
- [x] 更新接口/实施说明与 report，记录已证实的保证和外部边界；detect_changes 后作任务提交。

事务行为 oracle：

```text
prepare durable -> candidate publish -> systemd action -> stable instance
-> exact candidate inventory -> deny before grant -> PUT -> final GET
-> current authorization -> active durable -> candidate restart_keys
failure at any prefix => no new active keys; explicit failed/prepared record
```

### Task 3: Stock kernel fixture and 4.1.5 delivery

**Files:** 在 `scripts/tests/` 新建住宅门位集成 wrapper，fixture 放在 `bui` 的独立 test-only panel 模块；最小独立纯 renderer/strict snapshot Plan test-only helper 与离线 wrapper，用于部署零差异验证；修改 `Cargo.toml`、`Cargo.lock`、`CHANGELOG.md`，补充本次完整流程交付说明。

**Interfaces:** 直接使用 stock sing-box 1.14.2 与 Task 1 inventory/barrier 的真实 HTTP 结构，住宅 HY2 配置来自本项目 renderer。fixture 不证明 restart 零丢流，也不使用定制 Go 内核。

- [ ] 独立 Linux 容器与临时目录/随机端口，记录内核版本和 SHA，有限超时与精确子进程清理。实际 version/inventory 结构与 gate 写入/读回通过；错 version、缺门、假 PUT 在 Rust 测试中失败。
- [ ] 独立调用四个实际 producer 的纯 render 与 strict readonly snapshot 的原始 Plan，比较 exact e02 和最终候选在两台私有输入下的 artifact 字节/语义及当前受管字节；无未知 observation 默认值，无 generation 调用或生产写入。验证只变 BUI manifest/state metadata 后数据面 Plan 仍无变化，启动的 migration/backfill/idle reroll 对已初始化输入 no-op；方法参见 private deployment-renderer-preflight-method.md，缺证据不得部署。
- [ ] 更新 workspace version `4.1.5`、三个锁文件 package 版本与中文 CHANGELOG，明确只收口 Rust 控制面。
- [ ] 在最终版本重新执行受版本影响的验证；保存命令、SHA、结果和失败边界。不重复无变化的全量测试。
- [ ] 最终独立审查所有任务，处理阻塞项，GitNexus detect_changes 核对范围，commit/push `codex/residential-lifecycle-owner`，创建以 `codex/relay-generation-contract` 为 base 的 stacked PR 并 attach_artifact。
