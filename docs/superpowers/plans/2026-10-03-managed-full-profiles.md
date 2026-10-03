# Server-managed full proxy profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rust 从一致授权快照发布默认完整受管配置，使当前 Mac 用户一次导入与 TUN 授权后无需逐条编辑路由和 DNS；IPv4/IPv6 都捕获，无法确认的出口能力明确拒绝。

**Architecture:** 账户默认出口、能力证据、配置发布与客户端运行状态分离。单一 ManagedPolicy 经固定 sing-box1.14.2 compiler 交付到 v2rayN/macOS；bui-c 复用业务语义。独立 G0 验证官方预编译归档的计量与发布来源迁移，FAIL 阻止该迁移；已批准架构未要求所有既有未改上游源码构建物都迁移到归档。现有 source-build 发布链及 provenance 原样保留，不改称官方归档，也不由此获得新 managed 准入；当前业务完整性、计量 readiness、实际能力和 admission 闭环仍未通过。固定 REALITY 普通 TCP 六案中的两个半关闭案实测 FAIL，握手与普通响应通过不能替代完整性资格；有限评估不改变既有权益与服务范围，也不构成采用。

**Tech Stack:** Rust、serde、Axum、现有 tonic/prost、官方 sing-box1.14.2、原生 JS；不新增自研网络内核。

**Spec:** `docs/superpowers/specs/2026-10-03-managed-full-capture-design.md`，用户2026-10-03已审阅批准。

## Global Constraints

- BUI控制与管理代码坚持Rust；只使用官方原版内核，不维护关闭边界私有补丁。
- sing-box配置固定1.14.2，不写旧版本兼容分支。
- 默认捕获应用IPv4/IPv6及业务DNS；业务无native DIRECT，无国家/域名/端口/业务进程旁路。
- 明确选定一个获准出口身份；住宅失败不回落VPS或本机，不按延迟跨身份选择。
- 未验证IPv6能力捕获后拒绝，不能显示“双栈可用”；物理网当前无IPv6不能跳过捕获。
- 首次OS VPN/TUN授权及第三方自动更新时间不能由订阅代替；Android/iOS未设备验收不声明完整接管支持。
- 不修改Mac临时规则、不静音ERROR、不动用户tmux、不改生产自动更新、不运行旧v3子命令、不停机重启VM。
- 所有真实核心、网络、SSH及GUI操作由root执行；子agent只做分配的代码或离线审查。
- 每个任务编辑符号前做GitNexus impact；HIGH/CRITICAL先说明风险；commit前detect_changes和对应package gate。
- 订阅token和正文含凭据，HTTPS交付；不记录完整URL、凭据或配置正文到公开日志/提交。

## Review Focus

1. 住宅故障后仍有VPS授权节点：必须拒绝指定住宅profile，不默选VPS。归属Task2。
2. DNS冷启动或超时：业务DNS不得用节点bootstrap解析器回退，代理拨号不得递归。归属Task3。
3. 同revision重复订阅更新：确定性字节与稳定备注；ETag不等于第三方无损重载。归属Task4/5。
4. 无原生IPv6网络的测试环境：捕获不应省略，但防泄漏必须有真实IPv6上行才可判PASS。归属Task5。
5. API事件缺口与旧缓存撤权：计量不能把缺失当零，订阅失败不能代表已撤权。归属G0/Task4。

---

## 任务边界与交付状态

本计划是第一个可独立审阅的“Mac默认完整配置”闭环，包含原版控制面G0前置验证。真实VPS/住宅IPv6转发及Android/iOS交付是相同架构的后续闭环；它们分别需要网络运维与设备验收计划，不在本计划中凭未知平台字段生成假支持。

G0未通过时允许完成纯Rust策略、renderer、API和页面候选，但禁止生产切换计量/发布来源，禁止宣布原版发行物下计费契约成立。完整profile可声明“IPv6已捕获、当前出口未验收故拒绝”，不得声明IPv6访问已经可用。

当前源码HEAD2676acdc、4.1.6候选，已有PR6待合入。实施用`codex/managed-full-profiles`隔离工作树；先list_artifacts复用合适active worktree，必要时create_worktree(ref=已含本规格与计划的当前HEAD，产品基线仍为2676acdc)。不把新改造混入既有4.1.6住宅授权修复。后续PR明确依赖4.1.6；最终版本候选4.1.7，发布前重读GitHub与version.json确认未占用。

## 文件责任图

| 文件 | 单一责任 |
|---|---|
| `crates/bui-schema/src/managed.rs`（新） | 出口选择、能力时效与受管策略的纯模型，不包含内核JSON |
| `crates/bui-schema/src/render/managed.rs`（新） | 固定1.14.2完整配置compiler |
| `crates/bui-schema/src/model.rs`、`lib.rs`、`render/mod.rs` | 注册模型与账户默认出口字段，保持缺省序列化纪律 |
| `crates/bui/src/modules/panel/api_public.rs` | 同一授权快照交付完整profile，不另造账户授权 |
| `crates/bui/src/modules/panel/api_admin.rs`、`users.rs` | 管理员设置账户默认出口并校验已有权益 |
| `crates/bui-schema/src/sub.rs`、`web/app.js` | 默认完整配置入口、载荷诚实与首次动作说明 |
| `crates/bui-schema/tests/managed_profile.rs`（新） | 策略与compiler行为验收 |
| `crates/bui/src/modules/panel/official_api_fixture.rs`（新、cfg(test)） | G0真实官方API能力与缺口验证，不自动切生产 |
| `crates/bui/proto-api/`（新）、`build.rs` | 固定官方1.14.2 protobuf客户代码，仅给G0消费 |

## G0: 官方发行内核控制面与计量可行性门

**前置事实:** 当前住宅renderer使用experimental.v2ray_api，发布链为上游源码加with_v2ray_api构建；它不是官方release归档原件，但也未发现源补丁fork。官方1.14.2的services/api提供显式user和连接事件，却有256/64队列、1000关闭历史及静默丢事件，无持久逐用户重放。不能只删旧API字段。

**Files:** 新`crates/bui/src/modules/panel/official_api_fixture.rs`、`crates/bui/proto-api/daemon/started_service.proto`与其固定import闭包、SHA256SUMS；修改build.rs及panel/mod.rs的cfg(test)注册；新`scripts/tests/test-official-api-g0.sh`显式wrapper。现有生产hy2resi.rs与renderer暂不切换。

**Interfaces:** 固定上游daemon.StartedService；授权metadata `authorization: Bearer <fixture secret>`；SubscribeConnections/SubscribeStatus的interval单位纳秒，1秒=1_000_000_000，reset重建观察不能清账；startedAt/createdAt/closedAt为Unix毫秒，不能与interval纳秒混用。输出独立G0 JSON：required_cases、executed_cases、zero_case_skips、identity_attribution_complete、cases、delivered_bytes_by_user、ledger_deltas_by_user、gap_detected、actual_gate_denied、children_reaped、provenance。

- [ ] **Step1:** 编辑前impact build.rs关联生成模块和panel测试注册，报告范围。固定proto取af6e64c3b69e6132ebaee0e1a3d24e93903f6709 Git对象原文，SHA256必须4feeac3166f38074888d9a5e04e69c5f92a178d2a2c3e94a963d2999ef4a3994；保存摘要，不能用被修改的cache工作树。
- [ ] **Step2:** 写失败fixture。配置最小API如下，原版核心必须真实check/run，不因缺binary跳过；两凭据、四族协议目标使用loopback专属listener。

```json
{"services":[{"type":"api","tag":"bui-control","listen":"127.0.0.1","listen_port":10086,"secret":"fixture-g0-only"}]}
```

Fixture必须执行：两user TCP/UDP双族字节归户、持续活动、短连接、重复reset、重连累计去重、CLOSED尾字节、64/256事件压力、超过1000关闭记录、消费者退出、核心重启、计量缺口后真实门拒绝及存量流终止。每项独立nonce/长度/hash，不把API读计数当目标交付。缺口不能恢复时对应结果必须为明确检测并拒绝，不能零值PASS。

```python
# wrapper只验证运行证明，不编造业务结果。
import json
r = json.load(open("g0-result.safe.json"))
assert r["executed_cases"] == r["required_cases"]
assert r["provenance"]["official_archive_verified"] is True
assert r["identity_attribution_complete"] is True
assert r["zero_case_skips"] == 0
assert r["children_reaped"] is True
assert all(c["delivery_verified"] or (c["gap_detected"] and c["actual_gate_denied"])
           for c in r["cases"])
```

- [ ] **Step3:** 先明确运行红测试，再实现fixture消费器最小累计去重；键为startedAt/实例代际+connection UUID+user。CLOSED初始NEW不算在线。API没有Connection的UPDATE/CLOSED不得猜身份。总量对账只在实测稳定围栏执行，不均摊差额、不宣称原子快照。
- [ ] **Step4:** root在NetworkMode=none、官方资产digest核验且只有loopback可达的lab执行：`cargo test -p bui --locked official_api_g0 -- --ignored --exact modules::panel::official_api_fixture::official_api_g0 --nocapture`。wrapper要求明确1个目标测试实际执行；运行/缺口/清理任一失败非零。记录二进制完整SHA、真实分支和PID。
- [ ] **Step5:** 审查G0结论。不可恢复且无法发现的归户缺口为原版计量发布阻断；不得回到定制core或隐瞒计费损失。通过才另立官方API账本迁移任务与发布来源切换计划；当前任务只提交可复验fixture、证据及准确限制，`test(control): verify official 1.14.2 API accounting boundaries`。

## Task1: 纯策略模型与能力判据

**Files:** 新managed.rs、tests/managed_profile.rs；修改lib.rs注册。

**Interfaces:** EgressIdentity(Vps/Residential)、ClientTarget(V2raynMacOs/BuiCLinux)、EvidenceStatus(Unknown/Unsupported/Verified)、Evidence(path_fingerprint,observed_at,expires_at,observed_identity)、EgressCapabilities(v4_tcp/v4_udp/v6_tcp/v6_udp)、ManagedPolicy(revision,selected_identity,path_fingerprint,capabilities)。统一`allows(status:&EvidenceStatus,path:&str,now:i64)->bool`只允许相同path且未到期的Verified。Pure模型不授予节点权益。

- [ ] **Step1:** impact lib.rs新增模块相关导出；写到期、路径变更、未知/unsupported拒绝的失败测试。

```rust
#[test]
fn evidence_expires_without_granting_another_path() {
    let e = EvidenceStatus::Verified(Evidence {
        path_fingerprint: "resi-binding-a".into(), observed_at: 100,
        expires_at: 200, observed_identity: "fixture-residential".into(),
    });
    assert!(allows(&e, "resi-binding-a", 199));
    assert!(!allows(&e, "resi-binding-a", 200));
    assert!(!allows(&e, "vps-binding-b", 199));
    assert!(!allows(&EvidenceStatus::Unknown, "resi-binding-a", 199));
    assert!(!allows(&EvidenceStatus::Unsupported, "resi-binding-a", 199));
}
```

- [ ] **Step2:** 运行`cargo test -p bui-schema --test managed_profile evidence_expires_without_granting_another_path -- --exact`观察缺类型/函数失败。
- [ ] **Step3:** 实现上述serde模型、严格判据、非凭据Debug与纯选择函数`select_nodes(nodes:&[Node],identity:EgressIdentity)->Vec<Node>`。住宅只保留Hy2Residential/RealityResidential，VPS只保留两个Direct类型；它只能过滤已有授权节点，不创建凭据、不推断权益。

```rust
pub fn allows(status: &EvidenceStatus, path: &str, now: i64) -> bool {
    matches!(status, EvidenceStatus::Verified(e)
        if e.path_fingerprint == path && e.observed_at <= now && now < e.expires_at)
}
pub fn select_nodes(nodes: &[Node], identity: EgressIdentity) -> Vec<Node> {
    nodes.iter().filter(|n| match identity {
        EgressIdentity::Residential => matches!(n.kind, NodeKind::Hy2Residential | NodeKind::RealityResidential),
        EgressIdentity::Vps => matches!(n.kind, NodeKind::Hy2Direct | NodeKind::RealityDirect),
    }).cloned().collect()
}
```
- [ ] **Step4:** 运行全部managed_profile测试；补构造只含Hy2Direct但选择Residential的输入，必须得到空集合。证据expiry不改变revision，不将健康抖动变成配置发布。
- [ ] **Step5:** package gate + fresh review +detect_changes，提交`feat(schema): define managed capture and egress policy`。

## Task2: 管理员默认出口与单一授权快照

**Files:** model.rs的User新增`managed_egress:Option<EgressIdentity>`（serde default+skip None），State新增`managed_egress_capabilities:BTreeMap<String,EgressCapabilities>`（serde default+skip空map），panel/users.rs、api_admin.rs、api_public.rs的Delivery与lookup；测试沿现有FakeHost/Store实例。

**Interfaces:** 新`ManagedDelivery { username:String, nodes:Vec<Node>, policy:ManagedPolicy, dial_ip:String }`仅由既有gate→publication_permit→pending取一致快照；`managed_delivery(...) -> Result<ManagedDelivery,ManagedDeliveryFailure>`返回MissingSelection/Unauthorized/Unavailable。HTTP发送前复核publication revision时用短锁，不将writer锁跨HTTP写出。 User增加managed_profile_revision:Option<u64>保存语义发布版本（缺省不序列化），不能用探测时间作revision。生成一次Owned snapshot并记录有效权限；任何已允许族的证据过期则新完整feed返回503，不在同一revision悄悄换为另一出口。运行中的拒绝由服务端准入owner承担，尚未验收不得发布。

- [ ] **Step1:** 对User、lookup及管理员更新入口impact，HIGH范围先告知；写三条明确红测试：账户仅有VPS却设置Residential返回403；指定住宅失效且剩余VPS仍可用时503；两种权益共存而默认未指定时返回明确缺默认错误，不按节点顺序选择。
- [ ] **Step2:** 写管理员请求测试载荷并执行红测试。

```json
{"managed_egress":"residential"}
```

要求默认仅由管理员设置；用户新建/导入时无明确旧身份不能猜。沿既有access_for/nodes_for验证授权，凭据与节点仍来自一个快照，不能由URL参数增权。

- [ ] **Step3:** 实现字段、默认绑定、Delivery投影和错误，不改四个旧端点的载荷类型。能力证据与期望态绑定采用同path fingerprint，从同一State.managed_egress_capabilities取记录；缺记录返回四项Unknown，不拆多个读取拼快照。Verified不通过公开用户或管理员任意JSON输入写入，只接后续真实能力验证owner的内部结果；该记录未实际生成时profile拒绝对应族/协议。真正可用的Mac交付必须与该后续能力验证和服务端准入闭环共同通过，不把本计划的纯候选生成当功能完成。
```rust
// 放在现有lookup持有一致快照的范围内；没有选择或同身份节点不可用即返回错误。
let identity = user.managed_egress.ok_or(ManagedDeliveryFailure::MissingSelection)?;
let selected = select_nodes(&nodes_for(user, &state.node, &state.residential), identity);
if selected.is_empty() {
    return Err(ManagedDeliveryFailure::Unavailable);
}
// path_fingerprint由该快照的授权绑定标识生成，不包含明文凭据。
// managed_egress_capabilities缺失映射为四项Unknown，不能授予已验证能力。
```

- [ ] **Step4:** 执行`cargo test -p bui-schema --lib`与Linux`cargo test -p bui --lib modules::panel -- --nocapture`；保留原state SAMPLE不改（None字段不序列化）。增加disabled/expiry/quota/pending账款/rotate-token/撤权中快照测试；旧缓存撤权按协议真实粒度，不能假设每stream重新auth。
- [ ] **Step5:** package gate +review+detect_changes，提交`feat(panel): bind managed profiles to authorized egress`。

## Task3: 固定1.14.2的完整配置compiler

**Files:** 新render/managed.rs，修改render/mod.rs；tests/managed_profile.rs；订阅与bui-c现有renderer先保留入口，业务语义调用共同compiler，不重写probe_config。

**Interfaces:** `ManagedRuntime { interface_name:String, mixed_port:u16, enable_tun:bool }`；`compile(policy:&ManagedPolicy,nodes:&[Node],dial_ip:&str,runtime:&ManagedRuntime,now:i64)->serde_json::Value`。只消费已授权筛选节点；IPv6捕获无条件存在于可建TUN目标，转发按能力决定。

- [ ] **Step1:** impact singbox/相关outbound renderer；红测试TUN同时有v4/v6地址、final为指定代理、所有业务DNS有同身份detour、不存在业务direct、未知族/UDP明确reject。例：

```rust
use bui_schema::managed::{Evidence, EvidenceStatus, EgressCapabilities, EgressIdentity, ManagedPolicy};
use bui_schema::nodes::{Node, NodeKind, Transport};
use bui_schema::render::managed::{compile, ManagedRuntime};
let v4 = EvidenceStatus::Verified(Evidence {
    path_fingerprint: "fixture-vps".into(), observed_at: 100,
    expires_at: 200, observed_identity: "fixture-vps-identity".into(),
});
let policy = ManagedPolicy {
    revision: 1, selected_identity: EgressIdentity::Vps,
    path_fingerprint: "fixture-vps".into(),
    capabilities: EgressCapabilities {
        v4_tcp: v4.clone(), v4_udp: v4,
        v6_tcp: EvidenceStatus::Unknown, v6_udp: EvidenceStatus::Unknown,
    },
};
let authorized_nodes = vec![Node {
    kind: NodeKind::Hy2Direct, label: "fixture".into(),
    host: "fixture.invalid".into(), port: 443, hop: None,
    transport: Transport::Hysteria2 {
        username: "fixture-user".into(), password: "fixture-only".into(),
        sni: "fixture.invalid".into(), obfs_password: None,
    },
}];
let runtime = ManagedRuntime {
    interface_name: "fixture-tun".into(), mixed_port: 7890, enable_tun: true,
};
let cfg = compile(&policy, &authorized_nodes, "203.0.113.10", &runtime, 150);
assert_eq!(cfg["inbounds"][1]["address"].as_array().unwrap().len(), 2);
assert_eq!(cfg["dns"]["final"], "business-dns");
assert_eq!(cfg["dns"]["servers"][0]["detour"], "managed-proxy");
assert!(cfg["route"]["rules"].as_array().unwrap().iter()
    .any(|r| r["ip_version"] == 6 && r["action"] == "reject"));
```

以上输入已完整定义，所有凭据只用合成常量。补空nodes可加载拒绝态、只剩异身份节点仍拒绝、IPv6证据过期、DNS超时与冷启动。

- [ ] **Step2:** `cargo test -p bui-schema --test managed_profile -- --nocapture`确认新renderer未存在/不符合策略为FAIL。
- [ ] **Step3:** 实现双栈TUN+auto_route，固定1.14.2dns_mode/route语法；proxy外层拨号auto_detect_interface及窄bootstrap解析，业务remote DNS显式detour。不得加入进程全放行、国内直连或DNS失败回退。私网业务拒绝；bootstrap-direct没有业务route引用。稳定proxy选择同身份，urltest不跨身份。
```rust
// compiler的失败路由在业务代理规则之前；DNS先由同身份解析器处理。
let mut rules = vec![
    serde_json::json!({"action":"sniff"}),
    serde_json::json!({"protocol":"dns","action":"hijack-dns"}),
    serde_json::json!({"ip_is_private":true,"action":"reject"}),
];
if !allows(&policy.capabilities.v6_tcp, &policy.path_fingerprint, now) {
    rules.push(serde_json::json!({"ip_version":6,"network":"tcp","action":"reject"}));
}
if !allows(&policy.capabilities.v6_udp, &policy.path_fingerprint, now) {
    rules.push(serde_json::json!({"ip_version":6,"network":"udp","action":"reject"}));
}
if !allows(&policy.capabilities.v4_tcp, &policy.path_fingerprint, now) {
    rules.push(serde_json::json!({"ip_version":4,"network":"tcp","action":"reject"}));
}
if !allows(&policy.capabilities.v4_udp, &policy.path_fingerprint, now) {
    rules.push(serde_json::json!({"ip_version":4,"network":"udp","action":"reject"}));
}
// outbounds只采用select_nodes返回的同身份已授权集合；route.final=managed-proxy。
```

- [ ] **Step4:** root用fresh官方release配置check/run和loopback真实业务测试。缺核心/openssl必须FAIL，不能early return。Unknown IPv6验证捕获后拒绝；verified仅合成受控出口fixture验证语法/业务，不能把它当生产能力。输出配置内容在私有目录，公开只摘要和分支。
- [ ] **Step5:** package与真实核心gate+review+detect_changes，提交`feat(render): compile managed full-capture profiles for 1.14.2`。

## Task4: 默认完整配置接口与页面入口

**Files:** api_public.rs、sub.rs、web/app.js、现有面板订阅UI测试；新`scripts/tests/test-panel-managed-profile.cjs`。

**Interfaces:** `GET /api/profile/{token}/v2rayn-sb1142-macos`返回一个原生完整sing-box对象；明确target解析，不用UA授权。固定Content-Disposition名称保持profile备注，元数据走headers/页面。账户选择由Task2决定。保留URI入口并标“仅节点”。

- [ ] **Step1:** impact get_subscription/sub_urls/前端订阅生成；红测试目标路由与错误：unknown-target400，blocked403，住宅不可用503，未指定默认出口明确409，同revision同body与ETag，另一个账户不能使用共用缓存。
- [ ] **Step2:** 测试响应体直接含inbounds/outbounds/route，不含额外manifest包装；Content-Type application/json、Cache-Control private,no-store；不输出token/配置正文到日志。前端首选复制完整配置URL，不默认二维码复制URI列表。
- [ ] **Step3:** 实现新端点从ManagedDelivery调用compile；发布revision只由语义变化更新，不放时间戳/随机备注/延迟。HTTP状态错误不能返回DIRECT临时配置。新/旧profile内容不要自动热切在线用户。
```rust
// target必须明确，不借UA猜测OS/核心/VPN权限。
let target = match target_segment.as_str() {
    "v2rayn-sb1142-macos" => ClientTarget::V2raynMacOs,
    _ => return (StatusCode::BAD_REQUEST, "Unknown profile target").into_response(),
};
// renderer返回原生对象，不能json!({"profile": body})包装。
// ETag以实际确定性body摘要生成、缓存private,no-store；过期许可在生成前拒绝。
```

- [ ] **Step4:** `cargo test -p bui --lib modules::panel::api_public -- --nocapture`与`node scripts/tests/test-panel-managed-profile.cjs`。页面只展示复制/导入、首次TUN授权、一次启用自动更新和当前能力，不让用户逐条编辑DNS。Android/iOS入口显示未验收，不假发送sing-box JSON给v2rayNG。
- [ ] **Step5:** repeatfetch/端点下线/无效JSON/旧缓存测试。ETag不作为v2rayN不重启保证；告知官方更新成功会Reload，禁止短周期轮询。gate+review+detect_changes，提交`feat(panel): default to server-managed complete profiles`。

## Task5: 当前Mac真实交付、失败与更新验收

**Files:** 私有真实验收记录；`docs/architecture/managed-profile-acceptance.md`只记录去敏结论。bui-c仅共用compiler的语义测试，不扩大本任务Linux OS工程。

**Interfaces:** 每条验收记录target/core/archive+binary SHA/profile revision/hash、OS权限、路由、实际source identity、业务字节、过程PID/重载；状态只能PASS/FAIL/未验收/不支持。

- [ ] **Step1:** root备份现有Mac配置并记录当前进程/路由；从候选服务器完整URL实际导入。首次TUN授权保持系统UI，不替用户写大量规则。确认运行core版本1.14.2与完整配置真正被使用。
- [ ] **Step2:** 无HTTP/SOCKS环境的原始socket：IPv4 literal、IPv6 literal、A/AAAA-only、TCP/UDP、冷缓存UDP/TCP53及应用DoH，唯一nonce和受控目标来源身份/精确字节；物理接口只有限建链控制通信。显式curl --proxy成功不能代替TUN测试。
- [ ] **Step3:** 验证原生IPv6上行、后续加入IPv6、网络切换与睡眠恢复。当前Mac无公网IPv6不能记防泄漏PASS；用确实有IPv6的受控网络补齐。unknown族需捕获后拒绝，verified目标需选定出口成功。未具备环境则保持未验收，阻止“双栈可用”发布。
- [ ] **Step4:** 出口故障、账号撤销、旧缓存不刷新、DNS失败、订阅503、核心/TUN停止分别验证；OS无kill-switch时如实记限制。不得恢复已撤权旧配置。核心退出仍直连的路径不能称断开保护通过。
- [ ] **Step5:** 重复无变更订阅与真实变更订阅，同时运行135秒双向序列，观察Reload/PID/active profile和字节。没有证明无损则公布重建窗口和保守更新时间，不承诺即时无损推送。
- [ ] **Step6:** 保留官方HY2 A/B的36半关闭失败作为已知阻断；完整配置交付通过不标HY2关闭已修复。传输变化另用同一字节oracle验证，不能自动跨身份切换。独立审查后提交只去敏验收说明。

## 发布与后续交付围栏

- [ ] 逐任务fresh review和全分支review；cargo fmt --all -- --check；Linux cargo clippy --workspace --all-targets -- -D warnings；对应package test与scripts/tests/run-all.sh。Mac不以Linux inotify/kcmp失败误判schema；服务端完整gate在Linux执行。
- [ ] G0可行性通过后单独审阅实际账本迁移与官方archive锁定计划，再切release/kernels.lock、CI、manifest；旧1a60测试不得改名充当b861官方发行验证。G0 FAIL阻止该生产计量／来源切换，不删除计量契约；它不要求迁移每个既有未改上游源码构建物。保留既有source-build及其真实provenance不能证明部署产物已独立核验，不能豁免计量／业务完整性／准入资格；无论是否迁移，当前managed admission均保持阻断。
- [ ] VPS/住宅各自真实双栈网络能力通过，再审阅对应server renderer/6in4持久化与故障验收计划；不执行用户粘贴的命令或VM停机。服务器mode4与relay IPv4解析仍未改时不宣称IPv6转发完成。
- [ ] Android Xray完整profile与iOS配置分别取得设备验证，再形成独立交付计划；保留一个策略真源，没有大量旧版本兼容分支。
- [ ] 实际版本候选4.1.7：先重读version.json与远端版本，写version字段和changelog；跑scripts/release/check-version.sh、check-release-gate.sh、validate-manifest.sh规定参数。docs-only阶段不改版本。commit使用`bump: v4.1.7 server-managed full profiles`。
- [ ] push前GitNexusdetect_changes、敏感值检查（只新增行/文件）、确认没有临时日志/配置；创建依赖4.1.6的draft PR并attach_artifact。发布/真实机器改动按具体已审阅结果执行，不把PR创建等同部署成功。

## 自检记录

规格覆盖：统一授权/出口→Task1/2；完整默认交付→Task3/4；首次最少动作、更新、失效、真实捕获→Task5；原版来源与计量→G0；真实IPv6转发与Android/iOS按独立闭环在上述围栏推进，未声明完成。接口类型和函数名统一；五类Review Focus均有所属任务。用户审阅本计划并确认执行方法后才开始产品代码。
