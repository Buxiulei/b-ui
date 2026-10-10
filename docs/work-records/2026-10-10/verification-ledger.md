# bui-c 恢复与隔离验收验证台账

截至 2026 年 10 月 10 日，恢复与 owner 生命周期的多个源码增量已有实际回归和独立审查证据，但完整产品验收、strict clippy 和发布资格仍未通过。4.1.9 是私有、未发布候选。本次公开内容是工作记录；下列私有源码快照、二进制、镜像、机器观测和原始日志不随文档上传，不能将本次文档提交理解为这些实现已提交或部署。

本台账整理既有执行结果，没有重新运行 Rust、Docker 或远程测试。**917 → 1330 → 1417 → 1435 是不同源码阶段的历史回归数，不能相加，也不能把旧阶段 PASS 直接记到后续源码上。** 最终 Rust 源码 map `451d4511…` 的独立执行是格式检查、72 项 lifecycle、6 项 integration、11 项 contract，以及私有交叉构建；1435 项完整回归实际绑定其格式修改前的 `4730ddb1…`。

本次公开的 [产物索引](artifact-index.json) 记录产物元数据与本地 Rust 源码指纹；它不包含原始产物或候选实现。最终 Rust tar 的 SHA 为 `ab89c48456216dcb78dcaced2058db9d3f4294b5fdccfe74f3e4407d7580b6d2`，与 source map 的 `451d4511…` 分属不同摘要类型。

## 证据口径与公开边界

- `PASS` 表示所列源码、目标和环境下的实际执行成功；源码审查的 `C0/I0` 仅表示该次审查范围内没有已确认的 Critical 或 Important 问题。
- `FAIL`、`ERROR`、`ignored`、零测试与 `NOT_RUN` 分别保留。预期 RED 只有到达预期业务断言才算回归证据；编译错误、fixture 前置条件错误、超时和 OOM 不能替代业务 RED。
- `source map` 是规范化源码文件映射的 SHA-256，不是 Git commit，也不是 manifest JSON 或 tar 文件本身的 SHA。表内会明确区分 commit、map、archive、ELF、镜像和日志摘要。
- 真实文件、线程、socketpair 或隔离平台测试只能证明被执行的那一层。模型、源码审查、构建成功和保存的 JSON 均不产生当前系统权限，也不替代真实角色、冷启动、流量捕获或故障验收。
- 原始交接、不可变源码包、完整日志和现场收据私有保留。这里只公开无敏感值的 SHA、计数和边界，不公开本地绝对路径、机器标识、地址、别名、运行令牌或现场对象身份。**公开文档不能独立重放未上传的源码与现场材料；SHA 是对应关系和完整性标识，不是公开可下载的证据附件。**

## Rust 历史回归与最终冻结

主要完整回归目标为 `bui-c` 与 `bui-schema`；早期 Task 1 仅按原台账记录为客户端 package gate。原生与 musl 构建是不同目标，不能用容器默认版本替代实际工作目录里的工具链观测。owner worker 阶段明确记录原生 GNU 工具链为 1.99.0，musl 工具链为 1.92.0。

| 阶段与源码绑定 | 目标与环境 | 既有实际结果 | 实际证明什么 | 明确没有证明什么 |
| --- | --- | --- | --- | --- |
| 外部故障分类，Git commit `1e8c1431a362d800273ed073495f452bdb8827ee` | 客户端 package gate；原任务台账 | 917 PASS、3 明确 ignored；RED→GREEN 与独立审查完成 | 外部故障本身不授予重启的源码增量及该阶段回归 | 后续 owner、Guard、realm 或当前候选的完整门禁。该条不补猜未在摘要中列出的原生工具链细节 |
| 较早的身份观察后回归，原历史冻结快照 | Linux 原生客户端/schema | 1274 PASS、0 FAIL、4 ignored；初次 FMT 失败，格式修正后 FMT RC0；strict clippy RC101 | 该历史源码的 package 回归和格式修正 | 本表未重新确认其唯一 source map，故不把该计数绑定到其他已知 map，也不作为后续源码通过凭据 |
| owner 初步接线，map `76fd421ca1fe84ee4f7c84c7da39be3b952ade1660a2ade0ab5e40c8a736a22b` | Linux 原生，245 个文件 | 1283 PASS、0 FAIL、4 ignored；FMT RC0 | 该冻结快照的回归；线程、私有锁、目录身份等增量 | 实际跨线程平台 producer、产品 StopPost、冷启动和完整发布资格 |
| 首版 worker harness，map `a4f9184c5a2e1e6b69e5fcbfe02098f30b238b2068a663312012618ac445de54` | Linux 原生，246 个文件 | 1329 PASS、0 FAIL、4 ignored；FMT RC0；strict clippy RC101 | 初版 harness 的源码回归 | 审查随后发现主进程退出会终止仍活动的 worker；此版没有取得 live 资格 |
| worker 保留与完成，map `74dbafd0421fa23887564fe56031326848a946fab71756d0d7400accb9b605bc` | Linux GNU 原生，247 个文件；另有明确目标 musl 构建 | **1330 PASS、0 FAIL、4 ignored**；FMT RC0；strict clippy RC101 | 活动 worker 保留问题经实际线程 RED 修正；该版完整回归和独立私有 worker 验收 | 后续 pump、warm 增量，产品 hook、Ready、cold、capture 与发布资格 |
| pump 合并快照，map `fa4ff9dd5a985801031d9c672d6ae05c7a0ddaba904d1467a5a01b06f7677c0e` | Linux 原生，248 个文件 | **1417 PASS、0 FAIL、4 ignored**；FMT 失败；strict clippy RC101 | per-peer 拒绝、同 scope delivery、StopPost 原期限、读取代次等增量的回归 | warm 全链路预算、固定 systemd 角色、完整物理验收；格式和 clippy 不算通过 |
| 结构与布局修正，map `b9cb4bbcd7c87287e5cb27849be38e35606978bd641bfc047a21e7426c965190` | Linux 原生 | 1417 PASS、0 FAIL、4 ignored 再次实际执行；7 项非 dead-code 诊断清除；strict clippy RC101，剩 89 项 dead-code | 四文件结构/布局修正后的完整回归及有限范围 C0/I0 | 未接线代码已经可用，或 strict clippy/发布通过 |
| 中间格式冻结，map `3611ccdb421631f2997dc57c5079f80fc12a97ce083cec8977366b7e15b66068` | Linux 原生；另有 musl 构建 | 仅外部 integration 测试 import 折行变化；FMT RC0、6 integration PASS、0 ignored | 该格式增量的定向验证；生产实现与前一阶段相同 | 不能写成在这个 map 又跑了一次 1417；后续 warm 源码也不属于本快照 |
| warm 原 scope 四文件整合，map `4730ddb1a6a9ac566dd82d92d990e39247bf64c4c4d6d644c1150aa8c37ad43c` | Linux 原生完整客户端/schema | **1435 PASS、0 FAIL、4 明确 ignored**；83 Guard unit PASS；FMT RC1；strict clippy RC101，92 项 dead-code | Core、共享读取、policy/NFT、最终 receipt 共用原 scope 的源码回归；增量审查 C0/I0 | 真实 warm factory、固定角色与整发布 gate；FMT 和 strict clippy 当次均失败 |
| 最终测试格式冻结，map `451d45118619a4bc5bc460311bac2ac4a0267baee1542e150d82193bf704651d` | Linux 原生定向 gate | **FMT RC0；72 lifecycle + 6 integration + 11 精准 contract PASS，各 0 FAIL、0 ignored** | 与 `4730ddb1…` 比较只有 lifecycle 测试排版/import 顺序变化；生产前缀逐字节相同，测试 token/import 等价已独立检查 | **没有在此 map 重跑 1435 完整回归，也没有新增 strict clippy PASS**；旧 92 项失败仍阻断发布 |

完整回归的 4 项 ignored 包括子进程入口 fixture、golden 重录、nft netns 和 residential traffic 类测试；它们均没有按成功或物理验收计数。早期 917 阶段的 3 项 ignored 保留其原计数，不用后期分类反向补写。

### 零测试与 strict clippy 的实际失败

`4730ddb1…` 阶段曾用名称 filter `guard_contract`，命令返回 0，但所有 suite 都是 0 tests/filtered，不能记为 11 项 focused PASS。该阶段完整回归确实进入 contract 测试二进制并执行 11 项；最终 `451d4511…` 又通过明确的 test-binary 入口执行 11 项。旧零测试收据未改写。

私有 runner 后续加入“成功的 cargo test 必须实际执行测试”的门禁：零执行会给出 gate 125，同时保留原命令 RC0，并记录测试计数和 runner SHA；已用 4 份保存的实际日志检查对应分支。它不能追溯性地把旧收据改成失败或通过。FMT 本身不是测试，不适用非零测试数要求。

最后一次已记录的严格 clippy 执行绑定 `4730ddb1…`，RC101，末行明确为 92 个 error，诊断标题均属 dead-code；没有通过 `allow` 或虚假引用屏蔽。`451d4511…` 的改变只在测试格式，之后的监督器工作没有重跑或清零这些诊断。**最新状态仍是 strict clippy 未通过，不是“测试全绿即可发布”。**

### 私有交叉构建

| 源码 map | 明确目标与工具链 | 结果与 ELF SHA-256 | 边界 |
| --- | --- | --- | --- |
| `74dbafd0421fa23887564fe56031326848a946fab71756d0d7400accb9b605bc` | `x86_64-unknown-linux-musl`，Rust 1.92.0 | 构建成功；6,036,424 字节；`58d7626c222b04e2172376218f511c98ebecea641625e3b19343c2139a83dd92` | 私有 worker 验收二进制；`release_qualified=false` |
| `3611ccdb421631f2997dc57c5079f80fc12a97ce083cec8977366b7e15b66068` | 同目标，Rust 1.92.0 | 97.72 秒构建成功；6,630,504 字节；`05329fc24eb2aa98ae4688024711dbe91df7539fe54757a0c8fbe5fccb281d4e` | 私有中间产物；不包含后续 warm 增量 |
| `451d45118619a4bc5bc460311bac2ac4a0267baee1542e150d82193bf704651d` | 同目标，Rust 1.92.0 | 95.334 秒构建成功；6,634,600 字节；`ec037983d66fc6fe3f7a085a082ed5330b75dd9ce8402c198bb5ecee1325ff9d` | 最终私有候选 ELF；`release_qualified=false`；构建不证明安装、启动或流量行为 |

## 旧隔离平台与 worker 的实际观测

这些观测来自此前独立的网络命名空间与平台 fixture。**旧平台的 10 case / 52 legs 不能外推为新 realm 的 PID1、AppArmor、cgroup 委派、固定角色或 runtime 已通过。**

| 阶段与源码绑定 | 目标与环境 | 实际结果 | 实际证明什么 | 明确没有证明什么 |
| --- | --- | --- | --- | --- |
| 首次实际平台 producer，map `dcafbfa2f315e52538149d7895a1010c4f9df318cce3395631064b9dda24e9b2`；私有 ELF `4c6c40c7c805d73d4d35aa9dc95ed6af4f8cc13e891ae3554cc1fb67d5625031` | Linux 旧隔离平台 fixture | 10/10 case；52 个不同 leg：24 可达控制、12 guarded allowed、16 denied；独立清理完成 | 对应 producer 的实际连通性、拒绝计数及完整规则观察 | 没有保存逐 leg accept-counter 增量；控制阶段占位计数不产生 Guard 信用；不证明产品 core/TUN/capture/Ready |
| 私有身份观察，map `687e7a61815ba1a154c44925a75f19893f7a5df05d8a250d6c18789cf9a76d84` | Linux 只读观察实际对象 | whole basis、SSH factory/shape/两次 current、TUN 成功；domain 为 InvalidTarget、core-owned 为 LimitExceeded；整体 exit 1、permission=false、Ready=false | 该次实际身份观察成立的部分，以及未完成资格被正确拒绝 | 不证明受保护 endpoint 解析、完整 Core 资格或授予权限；不能通过直接 DNS 或放大限额补造成功 |
| 两轮 owner worker，map `74dbafd0…` 与上表对应 ELF | Linux 私有 producer/worker 实际调用 | 21.119 秒，外部 wrapper RC0；第一轮 producer 成功且 delivery 接受；第二轮在取消后 producer 仍成功、delivery 拒绝；两轮均 finished/reaped，短锁和 journal 可用 | 同进程 owner 接收，以及取消后拒绝交付但仍完成清理的两条实际链路 | 不证明产品 StopPost adapter、TUI 全交互、主代理恢复或新 realm |
| 两轮平台逐项复核，同上 | 旧平台实际 audit 的独立只读复核 | 每轮 10/10 case、52/52 legs；合计 104 个不同探测标记：48 控制、24 allowed、32 denied；每轮 4 份完整 JSON、8 份完整 native 合同 | 有响应的实际探测与拒绝计数证据；源/ELF/清理绑定；有限范围 C0/I0 | 早期一个双读夹层通用拒绝计数变化的来源未知，未借作负向证明；没有逐项 accept-counter 增量，不能额外声称该统计已证实 |

首次最低 fixture 的两次早期运行均保留为失败：第一轮 24 个可达控制通过后，在 counter 语法处失败；第二轮同样到达控制后，在用户态 cgroup 路径观察处失败。两次清理完成，但没有到达匹配器，不能算 Guard PASS。其后 ABI 编译失败与 fixture 名称错误同样没有被计为业务 RED。

## 关键源码增量的 RED 与 GREEN 边界

较早的源码聚合阶段另有 recovery 42、check 38、current-basis integration 15、CLI 286、owner 12 项 focused GREEN；它们分别属于当时增量，不能累加到后续完整回归。full-capture renderer 的最终形状曾有 6 项测试通过，并实际到达 8 次 stock 1.14.2 配置检查，涉及 4 份不同 JSON；这些检查证明配置构造/语法，未启动 Core，不产生实际流量捕获或 Ready 证明。本表没有为这些摘要补猜未逐项复核的源码 SHA。

| 增量 | 既有 RED 与后续结果 | 已确认范围与限制 |
| --- | --- | --- |
| worker 保留 | 初版真实线程 RED 为 0 PASS / 1 业务 FAIL / 0 ignored；修正后 owner 15 PASS，进入 `74dbafd0…` 的 1330 回归 | RED 确实到达活动线程；原快照没有先行 live 运行 |
| 共享 clock domain | 初次 31 PASS 后，审查仍发现 HookOwner 共用构造路径缺 TIME pins；补充 RED 为 0 PASS / 1 业务 FAIL；最终 32 PASS / 0 ignored | 同一生产观察/current 路径接线及实际过期覆盖；固定角色和 TIME 切换物理矩阵仍未执行 |
| 有界 publication 读取 | 初始 1 正控制 PASS / 10 业务 FAIL；初步 GREEN 62 unit + 6 integration；审查再发现两次读取之间的同内容 rename 被吞掉，补充真实 RED 后最终 64 + 6 PASS | 原期限、累计字节及代次复查；不能将初步 GREEN 当作审查已关闭 |
| warm Core 原期限 | 2 控制 PASS / 3 业务 FAIL 后，模块 52 PASS / 0 ignored；GREEN map `2c761d188f2a6c33fb24929bd5b0b4a52dbd5de23e1670f395d237a15d21b5e7` | 真实 Core 角色/realm 阳性仍 NOT_RUN |
| warm 共享读取 | 3 PASS / 4 目标 FAIL；另一个初始信任控制本来就有效，实际 1 PASS；最终 72 unit + 6 integration PASS；map `89f5c9c98ed7dfe474fa9a4c4ee22659c8f93d169897df5a125bb5fd659f8cfa` | 原 FD、名称、父目录、代次和累计预算保留；不能把已有效的信任控制描述为新发现的漏洞或 RED |
| warm Guard 同 scope | 1 实际文件正控制 PASS / 4 目标业务 FAIL；原子 rename 和 JSON marker 前置条件均到达；GREEN 83 unit PASS，进入 `4730ddb1…` 完整回归 | 原 scope 的 raw/kernel/Core/policy/NFT/receipt 源码闭环；整个 genuine warm factory 仍 NOT_RUN |

原期限覆盖是协作式准入、子进程 I/O、kill/reap 清理与同步文件操作边界；它不承诺任意内核阻塞下的硬性两秒墙钟。数据或模型测试不能制造 SSH、HookOwner、Guard 或平台权限事实。

## 新 realm 离线资产与镜像

| 阶段与内容绑定 | 目标与环境 | 实际结果 | 实际证明什么 | 明确没有证明什么 |
| --- | --- | --- | --- | --- |
| 初版离线资产，manifest `61171902c3a162043ec8c8409ed4ee7448a27c7911c27d037d1c6c62c1aed04b` | 离线纯数据检查 | 原 58 项 PASS；审查发现 2 个 Important；补充 3 个方法 RED，因其中子测试产生 4 个失败事件 | 暴露只读 profile 下初始化及验证后重新读取不同字节的问题 | 原 58 PASS 不关闭这些问题，也不验证 entry、真实 profile 或 runtime |
| 资产修正，manifest `b692bc86eb17d39d4c88f90e50c70ab384a60b04de22f2cc89ee58ab64a7e589`；archive `40e0c64241b8143a61329384b75759dc89980c2fac381635a4b955e3f324200c` | 61 纯数据测试；8 个 Linux 原生文件读取测试 | 61 + 8 PASS；两个 Important 源码审查关闭 | 读取一次后解析/哈希同一字节、有界文件读取及相关身份约束 | 并发代次变更 NOT_TESTED；entry main、AppArmor 加载、systemd/PID1 realm NOT_RUN |
| 私有 slice 名称修正后资产，manifest `f0f594b5982182bfe643d7543e4b71313b6ed9442f3b0fa02d798ac73e56c92f`；archive `53494a100bcf5ff20732d91b2ef5c745381faaf7870457e1199be9ba61f2a656` | 离线纯数据 | 61 PASS；7 项镜像资产不变 | 避免隐式共享 cgroup 父级的请求形状及纯数据回归 | 没有实际创建 slice、验证委派或启动 PID1 |
| 第一次镜像构建 | Linux 私有 image-only 构建 | **FAIL，build RC100，84.252 秒**；TLS 缺 CA；没有最终镜像成功或 runtime | 真实失败及其完成状态被保留；后续只读 rootfs 查明确有 keyring、无 CA bundle | 不能把失败当成功；旧索引被获取不等于安装了可变来源包 |
| 固定 CA 与 snapshot 修正后的镜像构建，context `7e271f2b66b514fd0ace5b51a75981ef0b3ce0505f1f9e28370715c60b258812` | Linux 私有 image-only；构建步骤限 512 MiB、1 CPU、360 秒双时钟期限 | **PASS，build RC0，165.870 秒**；CLI 完成且子进程已回收，daemon 结果已确认；image `84f0ce4884d30249eb4e46686eb83491dd2c3e6109cec63231b6655f90cd83b0` | 实际构建成功、CA 校验、3 个 suite InRelease、62 次固定 snapshot 获取；既有业务容器清单不变 | **runtime_started=false，release_qualified=false**；步骤限额不是整个 Docker daemon 的资源上限，结束 CLI 也不能单独证明 daemon 已取消 |
| 构建后 rootfs 来源核验，receipt SHA `e404b15a325941f209b08e72eff8e0793494d93deea5e31f8f9084c1734af147` | 只读 image save，内存内处理 272,183,808 字节 | 运行时依赖 pin 与候选 ELF 一致；systemd `255.4-1ubuntu8.17`、nftables `1.0.9-1ubuntu0.1`、iproute2 `6.1.0-1ubuntu6.4` | 镜像内实际字节与包版本、环境合并内容 | 没有文件系统解包运行、AppArmor 生效、PID1 启动或 cgroup 委派证明 |

CA 来源链从固定官方镜像 keyring 验证签名索引，再经 Packages 与对应 DEB 提取 bundle；没有复制主机信任库，也没有关闭 TLS/GPG 校验。APT Snapshot 的旧索引处于 shadowed 状态，可以被获取但不作为候选来源；初次失败不支持“已从可变源安装包”的结论。修正后的直接 snapshot 来源与构建日志另外核对通过。

成功镜像的构建输入包含最终 `451d4511…` 对应 ELF。镜像留作后续私有验收，完成与失败阶段的专属 staging 均有核对归属后的清理收据；这些清理和保留记录没有增加 runtime 验收信用。

## 最新监督器存储与 HTTP 测试

最新监督器范围是 pending 恢复、有界 Engine HTTP、文件原子发布/删除和原始期限。它是私有验收工具，10 月 10 日此轮没有继续改动 Rust 产品源码。

| 冻结材料 | SHA-256 |
| --- | --- |
| supervisor 源码 | `cbc146af006873efefefe97526c0bc5104f0f32e2916982fe001b9e8840f1a28` |
| tests 源码 | `22d8bfe6654d91f649fc69e5a7a4abfad3796f203e3e0799d190018ae8b8b9ce` |
| 最终源码 archive | `96e8759a5cdfca2f7574abd92285d62c0226d72eea59ed184971dd8d37da01d6` |
| 最终实际 Linux 日志 | `dfc1eab95bc82ec478cd77d39800b680e3af59c5611e9a9446f957a2d83c51e5` |
| 最终执行收据 | `5336251b3081032fbbecfa8ee1d24ce99b02d12652a625765b33297aa59140aa` |
| 有限范围审查记录 | `724745d0deee435db56282722c8383fd18b3663043f22ad38ea5c64fe6e25508` |

最终实际环境为 Linux 原生 Python 3.11，运行真实 helper 和 socketpair/文件 fixture，**22 PASS / 0 FAIL**；没有以 macOS 结果替代。日志逐项拆分如下：

| 类别 | 实际数量 | 实际覆盖 | 没有覆盖 |
| --- | --- | --- | --- |
| PureContract | 8 | 请求形状、原双时钟期限、arm-readback 顺序模型、UNKNOWN 与有限清理契约 | 真实 systemd arm、manager 代次、realm 创建或 cgroup 委派 |
| EngineProtocol | 5 | 真实 socketpair 的完整 Content-Length/chunked、截断与歧义 framing、元数据/body 上限；到达完整 header 后 body 停顿仍受原期限限制 | 真实 daemon 资源创建、真实 create UNKNOWN 的现场恢复 |
| NativeStorage | 1 | pending 写入中断后保留原 inode/字节，并能恢复 closing 发布 | 全流程 crash recovery 或真实 realm 清理 |
| NativeUnitPublication | 8 | 私有 staging、发布前耐久身份、RENAME_NOREPLACE、前后中断、异 inode 同内容、closing、期限在 receipt fsync 中到期、身份绑定 unlink | 文件删除不等于 systemd manager 已停止；没有实际停 slice/watch/timer |

这 22 项不是 22 项端到端 runtime 测试。最终收据为 RC0、`runtime_realm_started=false`、fixture 清单前后相同、stage cleanup RC0、builder stop RC0。测试日志自身记 0.114 秒，runner 收据记约 0.223 秒，两者范围不同。

### 同一监督器的失败历史

| 阶段及 archive 绑定 | 既有实际结果 | 结论 |
| --- | --- | --- |
| 初始纯契约 RED，archive `1037a532e39044ab5804ba4380d26ec1851409826971ad4a9f1eab429eada494` | 8 项中 1 正控制 PASS、7 个显式未实现 seam ERROR；解码 primer 已到达 | 保存为未实现失败，不授予 live 权限 |
| 原纯契约 GREEN，archive `c9e90a94358fc3386e3ef6b785b64a8745782146edb4e1c4ea9f876a56f323c0` | 8 PASS | 仅绑定当时纯数据源码；不能把旧 PASS 借给之后更大的 adapter WIP |
| pending RED，archive `c6d520611f6c690f75c0eda8d794f31ac6d52719443ab557725ba6f344da5abc` | 1 项、1 ERROR、RC1 | 中断 pending 阻塞 closing 的实际 Linux 回归；后续修正前保持失败 |
| pending GREEN，archive `20ac4474c0a0fdadf3232c2eef96923a357e90649ccb5dd87bbd645055b29954` | 8 契约 + 1 pending = 9 PASS、RC0 | 该次窄范围恢复修正 |
| 文件发布 RED，archive `9ad2812bb3e89acd84b29d0648ca48c8f2ae8e96b8d7e832d664c686fede5c50` | 4 项：1 FAIL、3 ERROR、RC1 | 未完成原子发布/身份接线的实际失败 |
| 首次合并，archive `4ba02cfa35035c86f42f62702c3c634866028125c08daa54fb45112c3b0a705e` | 17 项：16 PASS、1 HTTP ERROR、RC1 | HTTP read1 完成判据错误；**没有算整组通过** |
| 最终修正，archive `96e8759a…` | 上述独立 22 项 PASS、RC0 | 修正 framing/期限/发布增量的实际结果，不覆盖完整 adapter |

HTTP 的总 wire 限额为 512 KiB、累计行元数据 64 KiB、单行 16 KiB、解码 body 262,144 字节；connect/send/read 共用 `min(原 mono/boot 截止时间, 本次 2 秒 cap)`，读取和重试不重置原期限，JSON 解码完成前不清除 UNKNOWN。非父进程观察到退出记为 `exit_observed`，不冒充已经 wait/reap。

独立 HTTP seam 审查在该 seam 未变的最终源码上保持 C0/I0。文件发布审查先指出 receipt fsync 后缺少期限复查，最终在耐久记录后、rename 前增加原 clock 检查，并以实际 fault fixture 证明过期时不会到达 rename；reviewer 核对最终 source/test/archive/log 后关闭该增量 Important。**完整 supervisor/adapter 审查尚未关闭。**

## 尚未取得的资格

| 门禁 | 截至本台账的状态 | 对继续工作的约束 |
| --- | --- | --- |
| strict clippy | FAIL，最后实际 RC101、92 项 dead-code | 不能用源码审查或测试数量替代 |
| supervisor main、bootstrap/cleanup/internal watch 接线 | 入口仍关闭，未完成 | 不能运行 live supervisor |
| 清理前实际 manager 代次 | 未完成实际 Invocation/PID/cgroup 代次核验 | 不能只凭文件归属或名称停止 slice/watch/timer |
| 新 realm 的真实 arm、AppArmor、PID1、cgroup 委派、bootstrap/cleanup 故障验证 | NOT_RUN | 镜像 PASS、61 个资产测试和 22 个监督器测试均不替代 |
| genuine warm、固定 owner/Core/hook 角色和同 owner StopPost | NOT_RUN | 旧网络平台与 worker 结果不能迁移为新 realm 权限 |
| 完整 Start/Restart、pre/post、sealed core 实际启动 | 未接线/未验收 | 不可只安装 locking hooks；仍有持 flock 等待同步 systemctl 的旧路径，需要先迁移 |
| 所有 Runtime/配置/kernel 写者 | 迁移未完成 | 源码部分 fresh-merge 不等于全写者覆盖 |
| 冷启动、双栈 ND/DAD/DHCP、新链路、受保护 endpoint 解析 | NOT_RUN/未完成 | 不能借 warm 或直接 DNS 补造冷启动权限 |
| 全流量 capture、旧流/kill/restart、SSH/管理、回滚物理矩阵 | NOT_RUN/未完成 | 语法检查、数据合同与隔离最小平台不替代 |
| 完整发布与部署 | 未通过，候选未发布 | 此次公开文档不上传私有候选实现、二进制或现场证据 |

## 私有证据摘要索引

以下摘要用于区分材料；原始文件私有保留，公开记录不提供包含现场数据的原始附件。

| 材料 | 类型及 SHA-256 | 支持的结论 |
| --- | --- | --- |
| pump 1417 首次完整回归 | 日志 `8e78f1b7b1febe283e1da41ccbf8a39531daec4f2d43c0dacbecce288c297bca` | `fa4ff9dd…` 的历史完整回归 |
| 结构修正后 1417 回归 | 日志 `796427b8615b9730b6a9db2d9ca60576eb6ddbca56e3e4ebf89c3bc7e7f57478` | `b9cb4bbc…` 的再次实际完整回归 |
| warm 1435 完整回归 | 日志 `54943b962867cb918266b77cd246bb1846a2127aec58945444094712408ebded` | `4730ddb1…` 的完整测试计数 |
| warm 名称 filter 零测试 | 日志 `6c719323e5da94f45652ca419209d0b6c07f8b61dbbfb9b597914e60f0549878` | RC0 但 0 tests，不计 focused PASS |
| warm FMT 失败 | 日志 `13ce85f0543eaac70bbed430901a4529dba205d3e31ceea913a041b7c82dd515` | 格式修正前 RC1 |
| warm strict clippy 失败 | 日志 `786d3f53c6c17a115767a712adb16231ac4f744a938616a446f3f19c32e05f36` | 92 项 dead-code、RC101 |
| 最终 Rust 源码包 | archive `ab89c48456216dcb78dcaced2058db9d3f4294b5fdccfe74f3e4407d7580b6d2` | 与 source map `451d4511…` 的私有不可变归档对应 |
| 最终 lifecycle / integration / contract | 日志依次 `ef5289c645d5a69eda41ab1c64cefd6bfb541880601f72a7fc3d0cfb3ffd7f0e`、`6e44489bc8a6fb9ab55a5f26939a1330f0580c2940dccb421aacb33636599402`、`3f8680735764db487b5b572a0b88fb1c2a87789637ace72b55e8325882531b77` | 最终 72 / 6 / 11 定向执行 |
| 最终私有 musl 构建 | 日志 `73d5f20913037b26f1790acfc1eeb0ef227f754bff97424229f39b5f60d7782c` | 最终 ELF 的成功构建，不是发布资格 |
| 零测试 runner 检查 | 收据 `82f78983a1d6b684d5692d7b563a21eac6736d345919555fdd77367182336791` | 对保存的 4 份实际日志验证新检查，不改写历史 |
| 首次镜像失败 | 日志 `fec21e45d30c441ebce56528b5246254f72cf201a8281c6cfc323fa06ea38e41` | RC100/TLS 缺 CA 的真实失败 |
| 修正后镜像成功 | 收据 `01cf12dac89af8fe42df54be804fba31cc385bd7944adfa50d98352e9591eb8b`；日志 `cc8b23e14341b3b30183c5703db820ad672a7f6dc160f7380743883e974ed945`；inspect 摘要 `f301deeda3f32e7e63707a5d012086e0ceff2840831970e2cf037dfab229b3af` | build 成功、实际镜像对应、runtime 未启动 |
| 最新监督器进展摘要 | 文本 `7c0d01259e5ce5f93f0d79af18a314ea67eb946f21b5f480a98dad582b398ad2` | 22 项分类、当前入口与代次门禁；须连同上方源码/archive/日志绑定阅读 |
