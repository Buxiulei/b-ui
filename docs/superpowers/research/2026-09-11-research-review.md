# 两份 v4 调研报告的审查结论（Fable，2026-09-11）

审查对象：`2026-09-11-anti-gfw-topology-protocols.md`（Sonnet，369 行）与 `2026-09-11-static-residential-proxy-providers.md`（Sonnet，316 行）。规则：只有 **measured**（论文 / 独立实验室实测 / 生产日志）与 **documented**（官方文档、变更记录、issue 原文）级别的结论才能进 v4 方案；**anecdotal** 一律降为「背景」或剔除。下面按「进方案 / 降级 / 剔除 / 与用户决策的对照」四类整理，并解决供应商报告里 IPRoyal 的内部矛盾。

---

## A. 对抗 GFW 调研

### A.1 进方案（measured / documented）

| # | 结论 | 级别 | 来源 | 对 v4 的约束 |
|---|---|---|---|---|
| 1 | GFW 能实时解密 QUIC Initial 里的明文 SNI 并按黑名单丢包（~180s） | measured | USENIX Security 2025（gfw.report）[1][2] | Hysteria2 的 Salamander 混淆 + 端口跳跃是承重件，v4 不得默认关闭；masquerade/SNI 选择进设计 |
| 2 | 全加密（高熵首包）TCP 流量被动过滤 | measured | USENIX 2023 [3] | v4 不新增任何无 TLS/QUIC 伪装的入站协议 |
| 3 | 主动探测（先标记后连回验证） | measured（2020 基础研究） | IMC'20 [11] | REALITY 的设计目标正是对抗它；不加缺此特性的协议 |
| 4 | 2025-08-20 对全部 443 端口无差别 RST 74 分钟 | measured（事件） | net4people #511、gfw.report 博客 [9][103] | 保留「443 REALITY + 高位端口 Hysteria2」双端口族，不收敛到单端口 |
| 5 | 大陆无一手来源证实 REALITY+Vision 被指纹化；Iran/Russia 数据不可套用 | documented（检索结论） | XTLS #3269/#5332/#6421/#6091、XTLS/BBS #15、net4people 搜索 [5][6][104-107] | REALITY 保留为 TCP 主力；检索本身设为季度任务 |
| 6 | TUIC 参考实现自 2025-05 无提交；trojan-go 2024-07 停；shadow-tls 2025-04 停 | documented | 各仓库 [17][18][19] | 不引入 TUIC / Trojan / ShadowTLS |
| 7 | Xray-core、hysteria、sing-box 均非常活跃 | documented | [20][21][22] | 协议内核继续用这三者 |
| 8 | REALITY-XHTTP 不能过 CDN（CDN 终结 TLS）；非 REALITY-XHTTP 过 CF 有多方不稳定报告；mihomo 的 XHTTP 仍有 bug | documented（前者）/ anecdotal（稳定性） | [7][58][59][97] | 不做 CDN 前置；XHTTP+REALITY 只作为同机第二种 TCP 传输的**备选**，不进 v4 首发 |
| 9 | **Rust 生态没有生产级 REALITY / Hysteria2 服务端**：shoes 是唯一同时实现两者的项目，但无 Salamander/obfs 字段、无运行时多用户、无流量统计钩子（两条 feature request 0 回复）、维护者自述从未与 sing-box/Xray 对比测试、曾有 3 个与 Xray-core 的握手兼容 bug 靠外部用户撞出来修的 | documented | shoes CONFIG.md、PR #105/#133、issue #47/#128/#154 [85-95] | **坐实用户决策**：协议内核保留成熟实现，不自实现、不换 shoes |
| 10 | Rust 没有等价于 Go uTLS 的成熟 ClientHello 拟态库（craftls 停滞 20 个月） | documented | [46] | 同上；也意味着自研 Rust 客户端不能自己做 REALITY 客户端握手，要么调 Xray/sing-box 子进程，要么只做 TUN→SOCKS 层 |
| 11 | XHTTP 没有任何 Rust 实现 | documented | [7] | 同上 |
| 12 | Go goroutine vs tokio task 内存差异是 measured 的，但在 10³–10⁴ 并发量级只是几十 MB | measured | [66] | Rust 化的诚实收益是内存可预测性与运维简化，**不是延迟**；v4 立项理由要这样写 |
| 13 | RTT 由线路、拥塞控制、握手轮次决定；QUIC 的 CPU 成本是工程优化程度问题；REALITY splice 后 CPU 接近裸 TCP | measured（2020 Fastly）/ documented（Xray 文档） | [64][75] | 「换语言降延迟」不成立；延迟杠杆顺序：线路 > 拥塞控制 > 握手轮次 > 本地跳数（微秒级）> 语言 |
| 14 | io_uring 朴素采用只比 epoll 快 1.06–1.10× | measured（2026） | [70] | Rust 控制面不必追 io_uring |
| 15 | Xray #5828 REALITY socket 泄漏已修（2026-03）；#6684（closed not planned）有纯 TCP+REALITY 复现评论；两台生产机跑 26.3.27 | documented（issue）+ measured（生产 RSS ~150MB） | [97][98][99] | v4 验收加 48–72h 高并发 soak test；watchdog 保留 |
| 16 | v2rayN 7.25.1 内置 sing-box 封顶 ≤1.14；iOS 依赖 Shadowrocket/Stash 闭源客户端；不存在统一订阅格式 | documented | [71][78][79] | 三套订阅生成器是结构性必需，要合并的是它们**内部**的节点集合逻辑；服务端协议决策以「客户端已支持」为界 |
| 17 | sing-box gVisor 栈在 TUN + Hysteria2 场景 CPU 偏高，官方推荐 mixed | documented | [65] | v4 客户端 TUN 栈选 mixed/system；现有 bui-c 模板要核 |
| 18 | b-ui 当前 inbound 是纯 TCP REALITY（非 XHTTP） | measured（读码） | core.sh [84] | — |

### A.2 降级为背景（anecdotal，不作为设计依据）

- 2026-04「60–70% 大陆中转链路同周被打」及 SNI 黑名单 8–10 万、UDP >5Mbps 限速等数字（机场博客 [52]）——方向（不加国内中转）与 measured 的结论一致，但**数字不得写进 spec**。
- 社区测速给出的 REALITY / Hysteria2 / TUIC 晚高峰 RTT 区间 [74]、CN2 GIA 数字 [71]——只用于说明「线路 > 协议」的相对量级。
- 「masquerade 域名长期不变更容易进黑名单」——报告自己标注为基于论文机制的推论，不是测量。v4 可以把 SNI 可轮换做成配置项，但不以此为卖点。
- Hysteria2 的 **UDP QoS 风险**：报告里唯一 measured 的 GFW UDP 能力是 QUIC Initial SNI 解密；运营商对 UDP 的限速只有 anecdotal 报告。因此**不能**据此把 UDP 车道降级，也不能据此断言安全——正确处置是保留双车道互为对冲，并把「Hysteria2 vs REALITY 连接成功率/吞吐」做成 v4 健康探测的常驻指标（报告 §9 的一周 A/B 设计可作为验收的一部分）。
- 敏感时期加强审查（媒体报道）——进运维预案，不进架构。

### A.3 剔除

- 所有 SEO 内容农场的具体数字（「VMess 80% 检出」「30 秒识别 Hysteria2」「163 骨干 1Mbps」）。
- Iran/Russia 的 REALITY 封锁数据外推到大陆。
- 报告 §8 的「方向 B：数据面部分迁移到 shoes」——与用户已决策（协议核心保留成熟实现）冲突，且 A.1 第 9 条已否定其前提；不进 brainstorming 选项。
- 报告 §8 的「方向 C：保持现状」——不满足 v4 目标，只作为对照基线。

### A.4 与用户已定决策的对照

| 用户决策 | 调研支持度 |
|---|---|
| 协议核心保留 Xray / sing-box / hysteria | **强支持**（A.1 第 6–11 条） |
| 控制面 + Linux 客户端 Rust | 支持，但收益定义要改为「内存可预测、单二进制、运维简化、消灭 shell 生成器」，不是延迟（第 12–14 条） |
| 单机先扎实、多机留口 | 支持；调研指出千人瓶颈在硬件与面板统计（见审计报告 §6），多机是必需 |
| 不加国内中转、不做 CDN 前置 | 支持（第 4、8 条 + A.2 方向一致） |
| 保留双端口族与住宅出口分层 | 支持（第 4 条；loopback 一跳非延迟问题） |

---

## B. 静态住宅供应商调研

### B.1 IPRoyal 矛盾的解决

报告 §1「必须避开」把 IPRoyal 写成「降级为避免优先购买大额，仅可小额试测」，依据是 Proxyway 独立实测：ISP 样本仅 27% ASN 与宣传一致、4/10 样本不在宣传国家、曾持有疑似未授权的 Comcast 段并被回收 [24]（**measured**）。报告 §9 又把「IPRoyal 与 Decodo 是否白标 NetNut」列为待查，并称二者为「强候选」——这个措辞来自补查子任务的提问框架（补查因搜索配额耗尽无结果），不是新的证据。

**裁决**：以 measured 数据为准。IPRoyal **不是候选**；它唯一的优势（支付宝/微信/USDT 官方确认）不能抵消来源不实的实测。只有在 Top-5 全部试测失败时才允许用最小额度试一次。§9 的「强候选」措辞视为笔误。

### B.2 进方案的结论

| 结论 | 级别 | 处置 |
|---|---|---|
| Bright Data 的限制是 documented 的：policy_20110 封搜索引擎、20050 支付/TikTok 需特批、20020/20021/20240 端口白名单（拒 5228/5223）、HTTPS 仅 443 | documented + 生产日志 measured | 作为「不可接受」的基线 |
| NetNut（2026-07 谷歌 GTIG + FBI 处置，Popa 僵尸网络）与 IPIDEA 全家族（2026-01 处置；PIA S5 在内）是僵尸网络来源 | documented（谷歌博客、Krebs、BleepingComputer）| 一票否决；用户曾试的 PIA S5 属此家族 |
| CliProxy 的 ASN 是数据中心（FiberPower LLC）、IPQS 欺诈分 100 | measured（独立测评）| 排除；用户此前遇到的限制是来源问题不是策略问题 |
| Decodo：官方 FAQ 明文 Google 可访问；Proxyway 实测欺诈分最低、SOCKS5 UDP 已验证 | documented + measured | **#1**，但其受限清单是否适用于 Static/ISP 产品线未证实——下单前必须书面确认 |
| Rayobyte：Proxyway 实测 ASN 匹配 88%；AUP 只封邮件端口；SOCKS5 不支持 UDP | measured | **#2**；UDP 缺失对现有 relay（住宅路径 TCP-only）无影响；支付方式未确认 |
| MarsProxies / Oxylabs / Proxy-Seller | documented（仅官方页面），无独立实测 | 保留为 #3–#5 候选，但**必须先试测**；Oxylabs 有 KYC ~25% 拒绝率与转售用途被拒的实质风险 |
| Webshare 静态段被滥用库标记 | anecdotal（多篇论坛）| 排除（证据弱但方向一致，且支付不友好） |
| b-ui 的接入约束：selector 硬静态、所有用户共用同一选中 IP、黑名单只能识别硬拒绝（curl 56/97/35） | documented（读码） | 试测必须记录供应商的**拒绝方式**（硬 RST/403 还是软拦截页），软拦截型供应商与 R13 不兼容 |
| 验证计划 §8（1 个 IP 最小额度、三工具分类、Google/AI/支付/FCM/APNs 可达、24h IP 不变、CAPTCHA 检查） | — | 作为采购门槛，在 bwg-rick 上执行 |

### B.3 降级 / 剔除

- 所有价格与「无限带宽」宣称（documented 但是营销页面，隐藏封顶已有反例：IPRoyal 100GB、Rayobyte 200GB、Oxylabs 50GB 后降并发）——只用于预算量级（$3–30/月），不作为选型依据。
- IPFoxy「支持 ChatGPT/Gemini/Claude」、Massive「Google 100% 成功率」——自我宣传，剔除。
- 中文市场供应商（ipipgo、Kookeey、ABCproxy、PYPROXY、Proxy302、Nstproxy）——零独立证据或与 IPIDEA 同模式，剔除。
- 「IPRoyal/Decodo 是否白标 NetNut」——未获证据，保留为待决，不影响 Decodo #1（Proxyway 的欺诈分实测是对 Decodo 池本身的测量）。

### B.4 对 v4 住宅模块的直接要求（从调研推出）

1. 上游模型继续是**静态 IP 列表 + selector**，不接轮换池 SKU。
2. 黑名单（R13）要能区分硬拒绝与软拦截；软拦截靠人工 pin。
3. 健康探测要包含「同一 IP 承受本机全部并发」的压力维度，而不只是可达性。
4. 供应商试测流程（§8）应做成 v4 面板/CLI 里可重复执行的「上游体检」，而不是一次性脚本。

---

## C. 汇总：进入 brainstorming 的硬约束

1. 协议集固定：TCP/443 = VLESS+REALITY+Vision（Xray-core）；UDP 高位端口 = Hysteria2 + Salamander + 端口跳跃（apernet/hysteria）。不新增第三条 wire 协议。
2. 单 VPS 直连；不加中转、不加 CDN；保留双端口族；直连/住宅各一套。
3. Rust 只覆盖控制面、住宅 agent、Linux 客户端；协议握手全部交给成熟二进制。
4. 住宅上游 = 静态 ISP IP，selector 热切换，fail-open，黑名单硬拒绝自动/软拦截人工。
5. 验收含 48–72h soak（Xray 26.3.27）、Hysteria2 vs REALITY 连接成功率对照、供应商 §8 试测。

---

## D. 追加：Decodo Dedicated ISP 在 bwg-rick 的实测（2026-09-11，measured）

按 §8 验证计划执行，10 个已购 IP 分类 + 直连与经 relay 两条路径的目标可达性：

| 项 | 结果 |
|---|---|
| IP 质量 | 10 个 IP 中 4 个美国 ISP（2 Comcast，ippure fraud 1–3、residential；2 Verizon Business），其余 6 个为 AU/CA/HK 机房或 Lumen「Private Customer」段，fraud 83–91 |
| Google 搜索 / Gemini | 200，无 sorry/验证码（Bright Data policy_20110 的问题消失） |
| OpenAI / Anthropic API | 可达（401 / authentication_error，不是地区封锁） |
| Stripe checkout / TikTok / Apple、Google 商店首页 / accounts.google.com | 可达 |
| SOCKS5 UDP ASSOCIATE | 通（与 HTTP 同端口） |
| 端口 | **只放行 80/443**；5228 / 5223 / 993 / 22 / 8080 / 853 全部 CONNECT 403 |
| 443 上被封的主机 | pay.google.com、www.paypal.com、api.stripe.com、gateway.icloud.com、itunes/ess.apple.com、x.com / api.x.com |
| 拒绝方式 | 硬拒：CONNECT 返回 `403 Forbidden`。直连 curl 8.18 退出码 **7**（不是旧 spec 假设的 56），`%{http_connect}` 为 403；经 sing-box relay 时 curl 得 35 / 52 / 97，relay 日志 `unexpected status: 403 Forbidden` |

对 v4 的修正：
1. R13 黑名单的硬拒判定改用 `%{http_connect}`（HTTP 上游）/ SOCKS 回复码，退出码集合加入 7。
2. 「谷歌支付域名走住宅」在 Decodo 上不可行（pay.google.com 403），支付类目必须直连。
3. 供应商切换后 global 模式的拒绝目标集合与 Bright Data 高度重合（Apple 服务、推送端口、x.com、IMAP），黑名单是与供应商无关的必需模块。
4. 采购时必须按国家/ASN 选 IP（Decodo 支持 `-country-` / `-asn-` 与自助换 IP），随机分配会拿到机房段。
