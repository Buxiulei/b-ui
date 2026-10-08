# bui-c 恢复与独立双栈出口保护 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans；按下列任务逐项实施和 fresh review。步骤使用 checkbox 记录真实状态。用户已授权 root 连续执行，本计划不增加审批等待。

**Goal:** 外部探测异常不再破坏有效实例；恢复绑定当前配置/实例并复核本地故障；独立双栈保护、全捕获配置及生命周期通过同一验收后交付 4.1.9 候选。

**Architecture:** 复用 `Sys`、`Net`、现 flock 和 `service::observe`，建立一个客户端动作入口；锁外观测，锁内重验/合并记账。新增独立 inet guard，基础拒绝不随核心退出；guard、全捕获 renderer、systemd hooks 和恢复入口是同一个交付单元。

**Tech Stack:** Rust 1.85+、bui-c/bui-schema、原版 sing-box **1.14.2**、Linux systemd/nftables；FakeSys/FakeNet 仅做策略/控制流测试，物理故障门禁使用隔离 Linux 与真实核心。

**Spec:** [本文 Global Constraints](#global-constraints) 为已授权恢复设计及独立出口保护设计的脱敏执行摘录；可独立随仓库流转，不含实机统计、私有路径、生产地址或账号。审计主机只用既有 alias 指代。

## 执行状态（2026-10-08，按 root 实际台账）

本节只记录实际到达/执行的切片；下方完整步骤须逐项满足后才勾选。focused RED→GREEN 不等于完整任务、full package 或发布通过。

| 任务 | 已核实状态 | 仍未完成的交付边界 |
|---|---|---|
| Task 1 | 外部失败不授权重启的业务 RED→GREEN 已实跑；实际 API 为 `ExternalDegraded`，本地查询未知为 `Uncertain`。最终客户端 package gate 已通过。 | 本节不宣称发布或部署完成。 |
| Task 2 | observer/fence、full Profiles Eq、实际 config bytes 对比、锁内 local 重验、healthy fresh Runtime 合并已实现，并分步实际 RED→GREEN。 | 所有 writer 的统一动作入口、network/boot/monotonic 新鲜性及 guard 尚未实现；完整 Task 2 不勾完，其余合同须继续核定。 |
| Task 3 | 隔离 LinuxKit 的 minimum guard 平台资格实际 **FAIL**：`# CONFIG_NFT_SOCKET is not set`。同次 IPv4/IPv6 TCP/UDP 对照实际 reachable，实验资源清理成功。 | 该结果仅属于所测隔离平台，不能推断审计主机 alias 的能力。guard 实现、全捕获/lifecycle 同交付验收及防漏故障门禁未完成；禁止将可达对照或清理成功计作 guard PASS。 |
| Task 4 | 按用户既有版本更新指令，源码 workspace/lock/CHANGELOG 已分配 **4.1.9**；仅标识源码候选，**未发布、未部署**。生产 fresh 只读核查仍为 **4.1.8**，主实例未变。 | 版本分配不获得完整候选发布资格；guard/lifecycle、回滚、完整实机验收及实际发布/部署仍未完成，原门禁持续阻断。 |

当前不得据切片 GREEN 或源码版本分配宣称独立双栈 guard 已实现、恢复已完整验收、已证明防泄漏或已发布/部署 4.1.9；没有实际全包结果时不补写 PASS。后续执行仍遵循下列原合同，不因隔离平台不支持而放宽身份/例外规则。

最终源码候选在 Linux ARM64 / Rust 1.92 / 原版 sing-box 1.14.2 环境实际完成 `cargo fmt --all -- --check`、`cargo clippy --locked -p bui-c -p bui-schema --all-targets -- -D warnings` 和 `cargo test --locked -p bui-c -p bui-schema`。同一源码快照共 917 passed / 0 failed / 3 ignored；bui-c 为 666 单元测试及 12 恢复竞态测试。ignored 分别为 golden 重录、nft 网络命名空间和住宅流量集成，均未计作通过。该包门禁没有替代 Task 3 的物理出口故障资格或生产验收。

## Global Constraints

- 外部单站失败、HTTP 拒绝、多站投票或累计失败次数，均不单独授权 restart；诊断不足保留 Unknown/ExternalDegraded。
- 分别报告本地数据面、外部可达性和捕获保护；active、TUN 存在、一次 IPv4 route-get、HTTP 204 都不是完整就绪/无泄漏证明。
- 所有实际维护/恢复动作及 runtime 读改写经同一 flock/动作入口；profiles 相同不能复用旧实例失败。systemd 自动拉起也视为新实例。
- 破坏性动作前先确认独立 guard；失败仍保留物理出口拒绝。guard 规则缺失/无法读回时不启动或重启受管核心。
- 保护覆盖 IPv4/IPv6/DNS、所有物理接口和既有旁路连接；无 GUA 时也不删除 IPv6 基础拒绝。IPv6 实测必须先有真实可达对照，解析失败不算防漏通过。
- 核心例外必须共同绑定实际服务 socket cgroup、所选节点 literal 目的地和协议/端口/hop 范围；无 root UID、宽泛 fwmark、任意 UDP/SSH 端口或通用 established/related 放行。
- 仅原子修改 BUI 自有 inet 表；不 flush ruleset、不改 Docker/UFW/f2b 的表。初次资格实验只在隔离平台执行。容器是否纳入保护必须在平台门禁前确定；纳入时同交付覆盖 FORWARD，否则明确拒绝候选的容器覆盖宣称。
- 管理 SSH 与必要的冷启动控制 worker 需真实身份、具体 tuple/端点和有限范围；未知 probe 服务没有继承许可。无法证明 cgroup matcher 或冷启动管理路径时暂停该交付，不能放宽例外补救。
- 继续固定原版 sing-box 1.14.2；不改外部内核、v2rayN、用户账号/目标规则或日志等级。旧磁盘 receipt/runtime 不恢复当前 guard 资格。
- 示例只用 `example.com`、`203.0.113.10`、`2001:db8::10`、`alice`；保留地址仅用于 fixture，不作真实公网可达目标。实测端点及凭据放私有台账，不进入提交/命令行/原始日志。
- 恢复基线及已核实生产版本为 4.1.8；源码 workspace/lock/CHANGELOG 已按用户指令分配 4.1.9。版本分配只标识源码候选，不能代替完整候选发布资格。Task 1/2 可以独立合并审查，但未完成 Task 3/4 不单独发布或部署为“防泄漏版本”。不触用户 tmux；生产自动更新开关保持原授权状态，不自行开启。

## Review Focus

1. profiles/config 字节不变而 InvocationID/PID/start/boot 已变：旧探测不得重启新实例（Task 2）。
2. 拿锁前本地故障已被修复，或查询进入 Unknown/Transitioning：不执行旧动作、不把查不到当 UnitDown（Task 2）。
3. 健康检查与 update 并发、runtime 写失败或墙钟逆跳：合并最新字段；不把写入/ACK 当完成（Task 2/3）。
4. 新 IPv6/上行、旧旁路连接及重建 cgroup ID：基础拒绝持续生效，旧 ID 不自动授权新核心（Task 3）。
5. systemd hook 争锁/owner 中断/回滚：不死锁，不借缺失 pending 或删表恢复直连（Task 3/4）。

## 文件责任与最小接口

| 文件 | 责任 |
|---|---|
| `crates/bui-c/src/check.rs` | 决策、typed probe 观测、巡检结果及 runtime 锁内合并 |
| `crates/bui-c/src/service.rs` | **复用已新增** `Instance/StoppedState/ServiceObservation`、一次 systemctl show；不是 loaded-config 或 guard 来源 |
| `crates/bui-c/src/net.rs`、`nettest.rs` | 复用 `Net::probe -> Result<Probe,ProbeError>`；保留实际 HTTP/类别/耗时，不泄露错误载荷 |
| 新 `crates/bui-c/src/recovery.rs` | 当前 basis、短期观测新鲜性、唯一受锁动作入口及不确定动作收尾 |
| 新 `crates/bui-c/src/guard.rs` | 有界平台资格、有限规则计划、原子安装/内核读回；私有 typed receipt，不建第二权益库 |
| `engine.rs/cli.rs/update.rs/units.rs/paths.rs/lib.rs` | 各维护入口、guard hook、完整就绪和模块登记；不另起恢复 daemon |
| `crates/bui-schema/src/render/client.rs` | 显式全捕获渲染；沿同一 node/outbound，删除与全捕获冲突的普通业务 direct 例外 |
| 新 `crates/bui-c/tests/guard_linux.rs` | 隔离平台真实资格/故障门禁，严格前置和到达断言 |
| 新 `scripts/ops/bui-c-recovery-guard-drill.sh` | 明确 lab/主机 alias 的实测及回滚记录；公共示例，不嵌生产目标 |

---

### Task 1：外部失败不授权重启（先交付策略，typed probe 放 Task 2）

**Files:** Modify `check.rs` 及对应 CLI 文案/匹配；保留旧 failure 表达供本轮最小迁移。

**Interfaces（按实际 API 校正）：** `Decision`/`Verdict` 使用 `ExternalDegraded { failures: Vec<Failure> }`；新增 `Uncertain { failures: Vec<Failure> }` 表示本地查询未知，不授权恢复；`decide` 仍消费现 Runtime 和失败集；下例放在既有 `failures.is_empty()` → Healthy 返回之后。ExternalDegraded 只报告/诊断，不累加实际重启计数。退避只限制已经有本地资格的恢复，不生成资格。

- [ ] **RED：** 沿当前三个 `external_probe_*_keeps_a_healthy_tun_instance` 测试，明确 unit/TUN/route 前置有效、FakeNet 请求实际被消费；断言无 restart/stop/start、runtime 的 restart 计数不变。加 timeout、多轮持续失败和 204 与本地故障同时存在的反例；旧策略必须在业务断言失败，编译错误不算 RED。
- [ ] **最小 GREEN：** 在退避之前排除只有外部失败的集合，任何次数都保持 ExternalDegraded；保留本地故障分支的现行为作为后两任务的输入。

```rust
if !failures.iter().any(|f| matches!(f,
    Failure::UnitDown | Failure::TunMissing | Failure::TunNoDefaultRoute)) {
    return Decision::ExternalDegraded { failures };
}
```

- [ ] **验证/审查：** `cargo test -p bui-c check::tests:: -- --nocapture`；修订“首次外部失败立即重启”旧合同，保留真正本地故障正向测试，不能删除重启覆盖。fresh review 关注健康判定与重启授权是否分开。记录源 SHA/RED→GREEN 数量，再作 `fix(client): 外部探测异常保留当前实例` 的任务提交。

### Task 2：typed probe、同配置新实例 fence、本地重验和 runtime 锁

**Files:** Modify `check.rs/net.rs/nettest.rs/fake.rs/cli.rs/update.rs/lib.rs`；Create `recovery.rs`；Consume `service.rs` 现接口。新测试 colocated，不新增虚假实际身份工厂。

**Interfaces（本任务定义，后续复用）：**

```rust
pub enum ExternalProbe { Http(crate::net::Probe), Failed(crate::net::ProbeError) }
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryBasis {
    pub service: crate::service::ServiceObservation,
    pub boot_id: String,
    pub profiles_sha256: String,
    pub requested_config_sha256: Option<String>,
    pub network_sha256: String,
}
pub struct CheckObservation {
    pub before: RecoveryBasis, pub after: RecoveryBasis,
    pub external: ExternalProbe, pub external_elapsed: std::time::Duration,
    pub local: Vec<crate::check::Failure>,
    pub completed_wall: i64, pub completed_mono: std::time::Instant,
}
pub fn basis(sys: &impl Sys, paths: &Paths, profiles: &Profiles) -> Result<RecoveryBasis>;
pub fn runtime_update_locked(sys: &impl Sys, paths: &Paths, guard: &LockGuard,
    change: impl FnOnce(&mut Runtime)) -> Result<()>;
```

`basis` 从当前文件、一次 `service::observe(UNIT_MAIN)` 和实际双族 addr/rule/route 得出；network canonical 输入固定为 ifindex/地址前缀/flags、规则优先级与 table、路由 destination/table/dev/gateway，不含易变 lifetime/统计。缺失/坏输出返回不可用，不凑正常值。requested config SHA 只证明磁盘请求，**不称内核 loaded SHA**；systemd monotonic start 与 /proc startticks 不做数值等同。

- [ ] **RED：** 加五组明确到达测试：同 profiles 下换 InvocationID（包括同 PID）；仅 PID/start/config/network 变化；探测前后实例变化；锁内原 UnitDown/TUN 故障已经修好；Unknown/Transitioning 查询。用真实约定的完整 show 输出及 FakeSys 按调用阶段的输出序列，不仅改测试 metadata。若需 fake seam，在 `fake.rs` 增 `stage_reply_on_lock(command, Output)`，与现 `stage_on_lock` 同时只在实际取得锁时应用；fixture 不获得 Linux guard/实例物理信用。
- [ ] **typed GREEN：** check 调 `Net::probe`，保存 HTTP 与 ProbeError；成功耗时取 Probe.elapsed，失败耗时取同次请求外层 monotonic 差值，既不含 URL 也不猜失败地址族。分类 Unknown 不强猜 DNS/TLS/地址族。复用现 via：SOCKS 走 inbound，TUN 的 Via::Direct 只在 capture 合同已核实时代表受管腿；capture 不明时不发可旁路的诊断。不增加互联网 direct 对照请求。菜单沿同类别输出，保持宽度回归。
- [ ] **fence GREEN：** 锁外 before→probe→after，任何 basis 差异废弃结果；自动恢复仅 `How::Once`，锁内重读 profiles/runtime/basis 和当前本地故障。本地故障消失、basis 不同、service Unknown/Transitioning、墙钟逆跳或 `now_mono-completed_mono > PROBE_TIMEOUT` 都放弃旧动作；外部结果不因拿锁变新鲜。Stopped 必须来自 `ServiceObservation::Stopped`，命令失败不能当停机。配置/实例相同但本地故障修好同样不能重启。
- [ ] **runtime GREEN：** 所有 health/update/converge/维护写者通过 `runtime_update_locked` 先加载新 Runtime、只修改自己字段再原子保存；不嵌套 flock。`runtime_update_locked` 接受的是已取得的真实 LockGuard，先load→change→save；无需另一个 runtime store。`stage_on_lock` 注入新的 update 时间/版本后清健康 streak，断言两者均保留；写失败不报告已记账。外部诊断计数不再复用 actual-restart streak。

```rust
// 健康分支只补自己字段，不能保存锁外 old_runtime。
runtime_update_locked(sys, paths, &guard, |fresh| {
    fresh.fail_streak = 0;
})?;
```

- [ ] **正向与 gate：** 完整 Stopped 或锁内仍成立的本地故障必须能到实际动作入口，不以全 Busy/全拒绝假 GREEN；不确定仍只报告。执行 `cargo test -p bui-c service::tests::`、`cargo test -p bui-c check::tests::`、`cargo test -p bui-c update::tests::`、相关 `nettest/cli` 回归。service 已有 12 项源/执行证据由 root 台账复用，不冒称本计划运行过。fresh review 后任务提交 `fix(client): 绑定当前实例并锁内合并恢复记账`。

### Task 3：guard 资格 + 全捕获 renderer + 完整 lifecycle（一个交付单元）

**Files:** Create `guard.rs`、`tests/guard_linux.rs`；Modify `recovery.rs/engine.rs/units.rs/cli.rs/update.rs/paths.rs/lib.rs`、schema `render/client.rs`。现库不拼客户端 JSON；schema 新入口 `full_capture_config(node: &Node, opts: &ClientOpts) -> Value`，保留既有模式入口，guard 交付只能选全捕获入口。

**Interfaces:** `Guard<'a,S:Sys>` 持现 Sys/Paths 引用，沿 Engine 模式；`GuardQualification`、`GuardReceipt` 字段私有且不 Deserialize，只有本次平台资格/内核读回路径可构造。方法为 `qualify_platform(&self,&LockGuard)->Result<GuardQualification>`、`plan(&self,&Profiles,&RecoveryBasis,&GuardQualification,&LockGuard)->Result<GuardSpec>`、`install_and_readback(&self,&GuardSpec,&GuardQualification,&LockGuard)->Result<GuardReceipt>`、`withdraw_core(&self,&LockGuard)->Result<()>`、`confirm_current(&self,&GuardSpec,&LockGuard)->Result<GuardReceipt>`。`GuardSpec` 是由 plan 产生的有限端点/已验证控制面身份合同，无“qualified=true”构造器；FakeSys 可测规则/控制流，但不得记录平台物理 PASS。`ActionIntent::{Start,Restart,Stop,Apply,KernelSwap}` 和 `ActionResult::{Ready(service::Instance),Stopped,Uncertain}` 定义在 recovery；`execute_locked(sys:&impl Sys,paths:&Paths,intent:ActionIntent,guard:&LockGuard)->Result<ActionResult>` 是唯一实际动作入口。Engine 负责计划/验证/落盘，check/menu/apply/stop/kernel install 共用入口。Runtime 的 `PendingAction` 只存 operation UUID、intent、requested config SHA、phase（Prepared/Enqueued/Verifying），是动作记账，不能恢复 receipt。

- [ ] **先做平台 RED/GREEN，不先部署规则：** 隔离 Linux/netns/systemd 中真实验证 inet OUTPUT 两族 hook、socket cgroup matcher、实际核心/其它 root/cgroup 的区分、ID 重建、原子替换及其它 nft 表共存。正向用实际服务 socket 对所选 literal endpoint 成功；错 cgroup、错目标、错协议/端口和旧 ID 必须实际拒绝。不支持 matcher 或无法建立冷启动 SSH/控制路径就停止该交付；Task 1/2 仍可完成源码审查，release 保持阻断。
- [ ] **纯规则/渲染 RED→GREEN：** 默认拒绝所有物理业务；允许 loopback/TUN，核心仅身份×端点交集，SSH 仅实际管理身份/具体 tuple。全捕获 JSON 无 cloudflared 全进程、任意 UDP7844、22/2222、私网/国内域名普通业务 direct；DNS 沿受管路径。冷启动域名解析/配置拉取若必要，由具名短时 control worker 限定目标，不能 root 泛放；没有实际资格就拒绝。保持现 fixed node 的 TLS/SNI/obfs，不加 insecure/fallback。精确 JSON 断言及固定 1.14.2 `sing-box check` 都必须实际到达。
- [ ] **原子 guard GREEN：** 写 0600 私有规则文件→`nft -c -f`→一次本表 transaction→`nft -j list table inet bui_c_guard` 规范化读回。失败不删除旧表；所有 receipt 在同次内核读回及当前 basis 上建立。核读回缺项/额外宽例外/ID 改变即 Closed。没有 GUA 仍安装 IPv6 基础拒绝；新接口由基础规则自动覆盖。
- [ ] **真实生命周期接线：** 首次/开机先 base guard 再核心；停止/强杀/重启先收紧旧核心许可，guard 常驻。systemd `Restart=always` 的 start-pre/stop-post 进入同 guard 合同，不只修 check。**不得持 flock 调同步 restart，再让 hook 取同锁**：动作入口在锁内核 guard、记 pending operation、`systemctl --no-block` 入队后释放锁；hooks 取同锁核当前节点/cgroup并原子收紧/安装；等待 job 在锁外，完成后同锁重读 operation/basis/guard/本地 capture，才能确认。pending 时其它破坏性入口不插队；missing/坏 pending 不授权旧例外；重入只凭实际实例与内核状态收尾。自动启动也由 hook 核当前真实服务 cgroup，不能借旧路径/旧 ID。
- [ ] **本地完整就绪：** 替换 active+interface 五秒近似资格：核新 service 实例、请求配置、实际两族 rule/route、DNS 捕获与 guard 的 Captured-or-Blocked 契约。ACK/active 不直接给 Ready。no-op/self-only 不重启、不撤 guard；真实 kernel swap 才走一次受保护动作；停止代理与显式解除保护分开，普通 stop/uninstall-cleanup 不借删表打开出口。
- [ ] **强制真实 fault gate：** 长 TCP/UDP、DNS 与新连接在启动前/正常/stop/kill/restart/owner 崩溃/规则失败/新 IPv6/新上行/route 改变时持续采样物理接口；SSH 原连接及允许的新连接分别实测。负例前先证明相同 IPv4/IPv6 通路真实可达，否则计 NOT_RUN/infra；旧旁路 established 连接必须也被阻断。覆盖 pending 中断、hook 争锁和配置/更新/回滚失败，有限控制 probe 与另一 root 同端口业务不可互借许可。

最小 oracle（实现为 `guard_linux.rs` 的 `guard_actual_*` cases；条件不足必须显式 FAIL/NOT_RUN）：

```text
assert(precondition.v4_reachable && precondition.v6_reachable);
assert(stage.guard_kernel_readback && stage.real_core_socket_cgroup);
assert(stage.fault_injected && stage.application_retry_reached);
assert(physical.business_packets_v4 == 0 && physical.business_packets_v6 == 0);
assert(readback.guard_remains && ssh.existing_session_ok && ssh.allowed_new_session_ok);
```

- [ ] **同交付 gate/提交：** `cargo test -p bui-c guard::tests::`、`cargo test -p bui-schema full_capture`、隔离 lab 的 `cargo test -p bui-c --test guard_linux -- --ignored --nocapture`，再跑相关 Engine/units/check/update/CLI 全回归。核实际 case 数及 marker，禁止 ignored/静默 skip 充通过。分别 fresh 审身份/例外和生命周期故障，合并同一个 `feat(client): 全捕获配置与独立双栈生命周期保护` 交付；未通过不能单独启用 renderer 或 guard 宣称完成。

### Task 4：实机回滚与 4.1.9 候选发布门槛

**Files:** Create drill 脚本；Modify `Cargo.toml/CHANGELOG.md`、必要 CI gate；复用 `scripts/release/check-version.sh/check-release-gate.sh/validate-manifest.sh`。版本唯一来源仍 workspace Cargo，不创建旧 version.json。

**Interfaces:** drill 接收已记录的 host alias、候选/回滚构建 SHA 和私有配置备份路径；输出各 case 的到达/结果/源及二进制 SHA，脱敏台账与失败恢复指令。没有简单 bool “无泄漏”总开关。

- [ ] **构建门禁：** 同最终 SHA 执行 `cargo fmt --all -- --check`、`cargo clippy -p bui-c -p bui-schema --all-targets -- -D warnings`、`cargo test -p bui-c -p bui-schema`；Linux workspace gate 单列。固定 1.14.2 实际 renderer 校验，记录 build target/toolchain/依赖/ELF SHA；不把 macOS server 平台错误算客户端通过，也不绕 strict warning。
- [ ] **先验证回滚：** 在隔离系统演练替换/中断/恢复与 cold boot，备份 binary/config/profiles/units/runtime/本表意图和实际读回。回滚只恢复受验证配置/二进制，guard 保持；旧构建不兼容 guard 时停核心、保拒绝并用独立管理恢复入口，不能删 guard 求联网。禁止先推候选再临时找回滚方法。
- [ ] **受控实机：** root 按长期授权及既有 machine guards 执行，保留独立管理入口；维护 timer/auto-update 状态可逆且核读回。先保持长期连接，只让一个外部目标 timeout/403/5xx，确认实例不变和连接持续；再真实本地故障、同配置手动/更新换实例竞态以及 Task 3 两族故障矩阵。capture 原始证据私有；所有用户可见恢复结果必须绑定新实例和 capture 读回。
- [x] **源码版本分配：** 用户既有“记得更新版本号”指令已执行：workspace/lock/CHANGELOG 分配 **4.1.9**。这一项只确认源码候选标识，不确认构建、完整候选发布资格、发布或部署。
- [ ] **候选发布资格：** 完整 Task 1–3、fresh review、严格构建、固定核心、IPv4/IPv6 对照/故障、SSH/boot/回滚均有实际 PASS 后才可通过完整候选发布门禁。任一 NOT_RUN/guard资格不成立/IPv6只有解析失败/回滚不成立均继续阻断；不能因源码已为4.1.9、长期授权或一次路由正常而放行。
- [ ] **发布 gate：** `bash scripts/release/check-version.sh v4.1.9`、`bash scripts/release/check-release-gate.sh v4.1.9`、`bash scripts/release/validate-manifest.sh dist/manifest.json` 实际执行（dist/manifest.json 是本次生成资产清单，须真实存在）；确认 required-commit ancestry、资产 SHA 和旧版本兼容边界。最后任务提交、生成候选资产、受控验收后再按既定 release 流程发布；不擅自开启生产自动更新。观察窗口记录实际原因/实例变化/长连接及物理流量，失败回滚而不删除保护。

## 执行记录与完成定义

每项记录 `source SHA / target / test names / precondition+branch markers / PASS-FAIL-ignored / log SHA / review / rollback`，缺实际前置不作业务 RED，缺故障流量不作 guard PASS。当前已完成上述 Task 1 和 Task 2 切片，整包回归仍在修正；隔离 LinuxKit minimum guard 资格实际失败，审计主机能力不得据此推断。源码版本已分配4.1.9，但 guard 实现/lifecycle、双栈防漏故障能力、完整回滚/实机验收、候选发布资格及实际发布/部署未因本文或版本号获得通过信用。root 直接继续 Task 1→2→3→4；本计划写者不改源码、不运行 Cargo/SSH、不再向用户请求计划批准。
