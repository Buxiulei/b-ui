# bui-c 恢复与独立双栈出口保护 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans；按下列任务逐项实施和 fresh review。步骤使用 checkbox 记录真实状态。用户已授权 root 连续执行，本计划不增加审批等待。

**Goal:** 外部探测异常不再破坏有效实例；恢复绑定当前配置/实例并复核本地故障；独立双栈保护、全捕获配置及生命周期通过同一验收后交付 4.1.9 候选。

**Architecture:** 复用 `Sys`、`Net`、现 flock 和 `service::observe`，建立一个 Rust 生命周期 owner 和统一客户端动作入口；锁外观测，短生产锁内重验/合并记账，耗时平台验证使用同进程 worker 与独立锁。独立 inet guard 的基础拒绝不随核心退出；guard、全捕获 renderer、systemd hooks 和恢复入口是同一个交付单元。

**Tech Stack:** Rust 1.85+、bui-c/bui-schema、原版 sing-box **1.14.2**、Linux systemd/nftables；FakeSys/FakeNet 仅做策略/控制流测试，物理故障门禁使用隔离 Linux 与真实核心。

**Spec:** [本文 Global Constraints](#global-constraints) 为已授权恢复设计及独立出口保护设计的脱敏执行摘录；可独立随仓库流转，不含实机统计、私有路径、生产地址或账号。审计主机只用既有 alias 指代。

## 执行状态（2026-10-10，按已保存的执行记录）

本节包含公开代码基线和后续本地未提交候选的进度；本次只上传文档，不把本地实现描述为已经合入。差异、完整证据范围和接手顺序见[工作成果索引](../../work-records/2026-10-10/README.md)。历史实测不是此次文档任务重新执行的结果。

目标保持同一条主线：**控制面只根据当前本地故障中断实例；数据面在启动、退出、恢复全过程保持“受管转发或明确阻断”的双栈契约。** focused RED→GREEN、隔离平台资格与完整产品交付分别记账；下方完整步骤须逐项满足后才勾选。

| 任务 | 已核实状态 | 仍未完成的交付边界 |
|---|---|---|
| Task 1 | 外部失败不授权重启的恢复策略已实际 RED→GREEN；外部异常为 `ExternalDegraded`，本地证据未知为 `Uncertain`。第一阶段快照通过客户端 package gate。 | 第一阶段 package 结果不适用于此后的全部新增源码，也不代表当前候选已发布。 |
| Task 2 | 当前配置/实例/网络 basis、锁内重验、Runtime fresh-merge 及锁来源验证已完成对应 focused gate；CLI 回归实际 286 项通过。 | 所有维护、更新、删除、卸载和 systemd 动作尚未统一接入新生命周期 owner；严格 package gate 尚未通过。 |
| Task 3 平台与身份 | 审计主机支持实际 socket cgroup。Rust 隔离平台 10 个 case、52 条 IPv4/IPv6 TCP/UDP 对照和拒绝腿通过，独立清理通过。两种选定 raw transaction failure profile 的私有内核实验也通过。只读实际 SSH socket-activation 身份及 TUN 身份通过。 | 隔离 HostOutput 平台与当前身份不能替代完整产品：受保护域名 bootstrap、核心 sealed 启动、三类实际 hook、完整 capture/DNS、冷启动与物理故障矩阵未完成；AF_PACKET/FORWARD/桥接覆盖未获资格。 |
| Task 3 生命周期 | 独立保护规则、完整 renderer、publication journal、短动作 scope、root Unix channel、远端 hook 身份工厂已有源码和 focused gate。owner 前台及固定 StopPost 入口已接入源码，失效 peer 隔离、原期限传递、同操作记账、跨读取文件代次及 warm 统一读取/命令预算已通过回归与独立审查。同进程资格 worker 已完成两轮隔离验证和回收。 | 全部产品调用者、实际固定角色、启动前后 hooks 与启动/退出/恢复整体接线未完成；仍须迁移持锁等待 systemd 的旧入口后再交付 hooks。独立 worker 与源码测试不代表产品 hooks、Guard/Ready 或冷启动通过。 |
| Task 4 | workspace/lock/CHANGELOG 分配 **4.1.9 未发布候选**；此前只读记录的运行基线为 **4.1.8**，本次文档整理未复查生产。 | 完整 package、冷 IPv6/上行与管理连续性、强杀/重启/原子失败/回滚门槛通过前，不构建发布件、不部署候选、不经生产自动更新传播。 |

早期 LinuxKit 不支持 `CONFIG_NFT_SOCKET` 的失败记录仍有效，只描述该隔离平台；后续审计主机的实际成功是独立证据，并未回写或隐藏原失败。当前源代码不再依赖该失败平台授予资格。

第一阶段源码快照实际通过 `cargo fmt --all -- --check`、`cargo clippy --locked -p bui-c -p bui-schema --all-targets -- -D warnings` 和 `cargo test --locked -p bui-c -p bui-schema`，共 917 passed / 0 failed / 3 ignored。三个 ignored 未计作通过。**这些结果不是当前新增 guard、owner、hook、socket 代码的最终 package gate；当前最终 gate 仍未完成。**

worker retention 最终冻结快照（源码清单 `74dbafd0`）的客户端回归实际 **1,330 passed / 0 failed / 4 ignored**，格式检查通过，Linux x86_64-musl 私有构建通过。native 工具链实际为 Rust/Clippy 1.99.0，musl 为 Rust 1.92.0。15 项 owner 聚焦测试及实际跨线程 opaque proof 双轮验证通过；严格 clippy 仍返回 101，完整产品 hook/物理故障尚未完成。该快照结果不自动覆盖随后新增的 owner pump 源码，也未获得发布资格。

owner pump 结构收尾快照（源码清单 `b9cb4bbc`）已实际通过客户端和 schema 全套 **1,417 passed / 0 failed / 4 ignored**；随后仅修改测试 import 折行的 `3611ccdb` 快照通过格式检查、6 项 lifecycle integration 回归和 Linux x86_64-musl 私有构建。七项原结构性 clippy 提示已消除，严格 clippy 仍因 89 项未接线代码提示返回 101，未计作通过。对应私有 ELF SHA 为 `05329fc24eb2aa98ae4688024711dbe91df7539fe54757a0c8fbe5fccb281d4e`，未部署或用于生产自动更新。后续 warm 统一预算工作继续单独冻结、验证，不能借用此快照结果。

warm 统一预算快照（源码清单 `4730ddb1`）已实际通过全套 **1,435 passed / 0 failed / 4 ignored**。最终 `451d4511` 仅改变 lifecycle 测试排版，生产代码逐字节不变，格式检查、72 项生命周期单测、6 项集成和明确 test-binary 的 11 项 Guard 合同测试通过；四文件增量独立审查无剩余 Critical/Important。Core、请求文件、SSH policy、nft 与最终确认共用原操作期限和累计读取预算，旧事实不续期。strict clippy 仍因 **92 项未接线/未使用代码提示**返回 101；Linux x86_64-musl 私有构建成功，ELF SHA `ec037983d66fc6fe3f7a085a082ed5330b75dd9ce8402c198bb5ecee1325ff9d`，不具备发布资格。隔离固定角色环境仍处于资产和独立清理监督器准备阶段，真正 warm factory、完整角色与冷启动尚未运行。

隔离监督器的最新源码冻结与 Linux 文件/协议测试已完成：22 项通过（8 项契约、5 项 HTTP/socket、1 项 pending 恢复、8 项文件发布/删除），局部独立审查已关闭。镜像构建成功；但监督器 `main` 仍禁用，停止前的 systemd 实例代次复核未接通，真实 realm 的启动、退出及完整 hook 验收尚未运行。该工具验证不等于 Rust 产品已交付。

详细阶段证据见[验证台账](../../work-records/2026-10-10/verification-ledger.md)。当前不得宣称完整恢复已验收、防泄漏已证明或 4.1.9 已发布/部署。必要网络基础设施也必须纳入资格：当前隔离实验预置永久邻居，不能据此认为实际 DAD、ND、RA、DHCP、冷网关和新上行已经通过。

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
| `engine.rs/cli.rs/update.rs/units.rs/paths.rs/lib.rs` | 各维护入口、guard hook、完整就绪和模块登记；统一接入一个 Rust 生命周期 owner，避免独立恢复写者 |
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

每项记录 `source SHA / target / test names / precondition+branch markers / PASS-FAIL-ignored / log SHA / review / rollback`，缺实际前置不作业务 RED，缺故障流量不作 guard PASS。Task 1 和 Task 2 的已测切片按各自快照记账，完整候选门槛仍未通过；早期隔离 LinuxKit minimum guard 资格实际失败，后续支持 socket cgroup 的隔离平台已有独立成功记录，二者均不能替代新的完整 realm 与产品验收。源码版本已分配4.1.9，但 guard 实现/lifecycle、双栈防漏故障能力、完整回滚/实机验收、候选发布资格及实际发布/部署未因本文或版本号获得通过信用。root 直接继续 Task 1→2→3→4；本计划写者不改源码、不运行 Cargo/SSH、不再向用户请求计划批准。
