# 反 GFW 网络拓扑与协议选型研究 — 面向 b-ui v4

研究日期：2026-09-11。范围：GFW 2025-2026 检测能力、协议横评、拓扑模式、Rust 生态现状、延迟真相、客户端兼容矩阵，全部落到 b-ui 当前栈（Hysteria2 + VLESS-REALITY/Vision + sing-box 住宅 relay + Node 面板 + bash 客户端，2 台 2vCPU/1GB VPS，目标 ~1000 用户）之上。协议/项目/标识符保留英文，正文中文。每条结论标注置信度：**measured**（论文/可复现实测）、**documented**（官方文档/变更记录）、**anecdotal**（论坛/社区/单一报告）。引用以 `[n]` 标注，对应第 10 节参考文献。

---

## 1. 结论先行

1. **REALITY+Vision 继续作为 TCP/443 主力，不要替换**。截至 2026-09，没有任何大陆专属的一手来源（gfw.report 论文/博客、XTLS 官方讨论、net4people/bbs）证实 REALITY 握手本身在中国大陆被指纹化或规模化封锁；唯一疑似个案（#6421）更可能是 IP 被第三方盗用为 CF relay 引发的连带审查，而非协议指纹 [104][105][106][107]。Iran/Russia 的数据（#3269、#5332）是不同审查环境，不能直接套用 [5][6]。
2. **Hysteria2 + Salamander 混淆 + 内置端口跳跃继续作为 UDP 主力，且必须保持开启**。GFW 对 QUIC Initial 包的 SNI 解密阻断是 2025 年 USENIX 论文实测的现网能力（非匿名论坛传言），未加混淆的 Hysteria2 直接暴露在这条检测链上 [1]。
3. **不要新增"国内中转"层**（IEPL/IPLC 或 BGP relay 落地机）。2026-04 的社区复盘（置信度 anecdotal，未经 gfw.report 或学术源核实）描述大陆 IDC 中转链路被集中打击，这与"单 VPS 直连"模型的最小攻击面优势正相反 [52]。
4. **不要迁移到 Cloudflare/CDN 前置**（Workers/Pages + XHTTP）。REALITY 架构性无法过 CDN（CDN 终止 TLS，破坏 REALITY 借用真实证书的前提）；非 REALITY 版 XHTTP 过 CF 在 2025-2026 被多方报告为不稳定（H3 代理模式失败、workers.dev/pages.dev 疑似被部分 SNI 封锁）[7][58][59]。
5. **"核心代码尽可能用 Rust"目前没有可验证的延迟收益证据**。本次研究未找到任何 2025-2026 年、China 路由上、协议对等的 Go-vs-Rust 代理吞吐/延迟可复现基准；RTT 由物理链路和协议握手轮次决定，与实现语言无关 [64]。
6. **Rust 化的真实、可辩护的收益点是内存可预测性与运维简化，不是速度**。Go goroutine 最小栈 ~8KB vs Rust tokio task ~200-400B，在 100 万并发任务规模下 Rust 内存占用低 12 倍以上；但 b-ui 在 1000 用户量级（10³-10⁴ 并发连接）下这一差距是几十 MB 级，大概率不是当前的瓶颈 [66]。
7. **b-ui 生产服务器 bwg-rick/bwg-tizi 当前内存远未触及 Xray-core 已知的内存泄漏阈值**（各服务器 5-6 用户下整套代理进程总 RSS 约 150MB，对比社区报告中 ~150 用户、2 天后 RSS 600MB+ 且仍在增长的 REALITY/XHTTP 泄漏个案）；但两台服务器都运行 Xray 26.3.27——恰是那个泄漏报告涉及的版本，且有另一位用户在**纯 TCP+REALITY（非 XHTTP）**配置上复现了类似增长模式，应在 v4 上线前做 48-72 小时高并发 soak test，而非假设"用户少所以安全"能线性外推到 1000 用户 [97][98][99]。
8. **保持双端口族冗余**（443 REALITY-Vision + 高位端口 Hysteria2，直连与住宅各一套）。这正好对冲了目前唯一被 measured 的"无差别 443 端口整体 RST"事件（2025-08-20，持续 74 分钟，原因未定）；v4 不应为"简洁"把所有出口收敛到单一端口/协议 [9][103]。
9. **本地 SOCKS5/relay 跳转（127.0.0.1:2080）不是延迟瓶颈**，同机 loopback TCP RTT 量级在微秒级（~18-24µs），不要以"降延迟"为理由拆掉住宅出口分层设计；如果要简化它，理由应该是运维复杂度而非性能 [69]。
10. **v4 最值得修的"简洁"债务，是节点集合逻辑在 4-5 处手工同步**（web/server.js 的三个生成器 + web/app.js 的 genUri() + b-ui-client.sh 独立的 TUN 模板），这与协议或语言选择无关；无论走哪条 Rust 化路线，都应先把它收敛成一份共享 schema 再各格式渲染 [82][83]。

---

## 2. GFW 2025-2026 能检测什么

按当前证据强度从高到低排列，每条给出对 2-VPS/1000-用户运营者的实际意义。

| 排名 | 检测/阻断向量 | 机制摘要 | 来源 | 置信度 | 对 b-ui 的意义 |
|---|---|---|---|---|---|
| 1 | QUIC Initial 包 SNI 解密阻断 | 国家级实时解密 QUIC Client Initial，按明文 SNI 匹配独立黑名单；60ms-7.5s 内触发（>90% 在 1s 内），命中后对该 4 元组做 ~180s（论文另称 ~3 分钟）UDP 丢包；不重组跨 UDP 报文分片的 Initial；~1500Kpps 时解密管线过载、夜间时段封锁率下降 [1] | USENIX Security 2025（gfw.report） | **measured** | 直接命中 `hysteria-server`/`hysteria-residential` 两个监听器；masquerade/SNI 域名选择与轮换和端口跳跃同等重要，Salamander 混淆是承重构件而非可选项 |
| 2 | 全加密流量被动过滤器 | 首包高熵/无可打印 ASCII → 判定为"全加密"→ 封锁；仅覆盖 TCP；~120-180s 该 (client IP, server IP, server port) 残留封锁；历史上偏向特定 VPS ASN（Vultr/Alibaba HK-SG/DO SF-NYC），未见针对 AWS/Oracle | USENIX Security 2023 + Geneva/UMD 2021 | **measured**（VPS-ASN 偏向数据点已是 2021 年基线，可能已漂移）| 对 REALITY 低风险（呈现真实 TLS 握手，天然豁免）；任何"裸"混淆回退协议都会直接暴露在这条规则下——v4 不应新增无 TLS/QUIC 伪装的协议 |
| 3 | 主动探测（active probing） | 被动规则先标记可疑服务器，censor 随后主动连接验证是否为代理协议再拉黑 IP | GFW Report IMC'20 | **measured**（2020 年基础研究，机制持续被 2025-2026 二手来源引用为现行基础） | 这正是 REALITY 的设计目标：被探测时把连接转发给真实 dest 网站并返回其真实握手，探测者只看到一个正常网站；v4 不应再加缺乏此特性的传统混淆协议（裸 Shadowsocks、无 TLS 的 VMess、无 REALITY 的 Trojan）|
| 4 | 无差别整端口 RST 注入 | 2025-08-20 00:34-01:48 北京时间，对所有 443 端口 TCP 流量双向伪造 RST+ACK，持续 74 分钟，22/80/8443 未受影响；TCP window size 递增，不符合已知 GFW 设备特征 | net4people/bbs #511 | **measured**（事件本身）/ 原因未定 | 论证不应把 100% 出口收敛到 443；b-ui 现有"443 REALITY + 高位端口 Hysteria2"双端口族设计已经是对冲，应保留 |
| 5 | 大陆中转/IDC 定向打击 | 2026-04-15~22 期间约 60-70% 国内中转链路同时遭遇 RST+QUIC 限速+整网段黑洞，社区归因于 SNI 黑名单扩容至 8-10 万域名（含 CF Workers/Pages、Vercel 子域）、UDP/QUIC 限速扩展到三大运营商 >5Mbps 流量、IDC 网段路由层黑洞三者叠加 | 中文机场/运维复盘博客 | **anecdotal**（未见 gfw.report/学术源交叉验证，数字未经独立核实） | 强烈提示不要新增国内中转层；大陆托管的中转节点现在是更大而非更小的攻击面 |
| 6 | REALITY/Vision 协议指纹化（中国大陆专属） | 未找到任何大陆专属一手来源证实该能力；Iran 数据显示检测是多因素（流量体量、SNI 域名声誉、按 ISP 启发式），而非纯协议字节模式指纹；平行的俄罗斯 issue 从未被维护者诊断根因 | XTLS/Xray-core #3269（Iran）、#5332（Russia，未解决）| **anecdotal**（且样本来自 Iran/Russia，非中国大陆） | 视为"当前耐用但未经证实"；不要单一 REALITY dest/SNI 承载过多用户，避免复现 Iran 数据里"流量体量触发多因素检测"的模式 |
| 7 | ECH（Encrypted Client Hello） | 不在传输层直接被阻断；GFW 转而封锁获取 ECH 配置所需的加密 DNS 通道（DoH/DoT/HTTPS-RR），使 ECH 难以引导启用；实际可用性还受限于仅 Cloudflare 广泛提供 ECH | FOCI 2025 论文 | **documented** | v4 低优先级，不建议作为主打特性投入 |
| 8 | JA3/JA4/uTLS ClientHello 指纹 | 未找到 GFW 直接使用此类指纹的证据（对比俄罗斯 TSPU 2026-03 已确认用 DTLS ClientHello JA3/JA4 式指纹封锁 Snowflake）；REALITY 已借用真实浏览器 uTLS 指纹缓解此类风险 | 行业背景 + 俄罗斯 Snowflake 对照事件 | **documented**（背景）/**anecdotal**（"不模拟浏览器更易被标记"的具体说法溯源到低质量内容农场） | 保持 REALITY 客户端的 uTLS chrome 指纹是近零成本的保险，但不是当前已证实的 GFW 向量，不应作为 v4 头条特性 |
| 9 | 敏感时期强化审查 | 党代会/十一/六四等窗口期历史上出现 DPI 强度提升与主动封锁代理服务器；2025-04 有报道描述运营商被要求加强扫网封 VPN | ABC News 2026-06-04（引用 2025-04 事件）+ SCMP 2022 二十大先例 | **anecdotal** | 与用户自身运维记忆（bui-rollout-order.md：bwg-rick 先测再动 bwg-tizi）一致；应在健康检查/切换逻辑里内建针对该类窗口的降级预案，而非假设常态条件全年成立 |
| 10 | SEO 内容农场编造的具体数字声明 | "VMess 80% 检出率""non-uTLS Go 指纹 4 倍易被标记""2026-02 中国电信 30 秒识别未混淆 Hysteria2、163 骨干网高峰限速 1Mbps"等——均无法溯源到 gfw.report、net4people/bbs、学术论文或 XTLS/apernet 维护者声明 | greatfirewallguide.com 等内容农场站点 | **anecdotal**（应视为噪音，不作为证据） | v4 设计文档中任何引用此类具体百分比/日期的说法都应剔除或明确标注为未证实 |

**对运营者的综合意义**：真正 measured 的能力集中在两点——QUIC Initial SNI 解密（命中 Hysteria2）和主动探测（REALITY 已针对性设计防御）。真正的"新风险"不是协议指纹进化，而是运营侧变量：把中转层搬回大陆、把流量前置到 CDN、把所有出口收敛到单一端口。这三件事都是 v4 应该主动避免，而不是靠更换实现语言来解决的问题。

**逐条展开三条 measured 能力对 b-ui 的具体动作**：

- **针对向量 1（QUIC SNI 解密）**：`config.yaml`/`config-residential.yaml` 的 Hysteria2 masquerade 域名不应是一个长期不变的固定值——既然 GFW 的封锁是按明文 SNI 维护独立黑名单，一个从未轮换、长期暴露的 masquerade 域名理论上比轮换的更容易被动态加入黑名单（本条为基于论文机制的推论，非直接测量结果，标注为**documented**级别的推论而非**measured**结论）。同时论文本身指出 GFW 不重组跨 UDP 数据报分片的 Initial 包——这是 Hysteria2/sing-box 生态可能已经或应该利用的规避手段，值得在 v4 中核实客户端侧实现是否已具备此能力。
- **针对向量 3（主动探测）**：REALITY 的防御前提是 dest 网站必须对主动探测者呈现完全真实、可信的响应——v4 若调整 dest 选择策略（例如为了负载均衡在多用户间共享同一个 dest），需要确认这不会引入可被批量识别的行为差异（例如同一 dest 短时间内被大量不同 REALITY 客户端连接的流量模式本身，是否会成为向量 6 里 Iran 数据所述"流量体量驱动检测"的诱因）。
- **针对向量 4/5（整端口 RST、大陆中转打击）**：两者共同指向"运营基础设施本身（IP、ASN、物理位置）比协议实现更容易成为封锁对象"这一结论，因此 v4 的韧性预算应优先投入 IP 快速更换/DNS 快速切换的运维能力，而不是协议层的进一步伪装。

---

## 3. 协议横评

| 协议 | 传输 | 伪装/隐藏方式 | 已知弱点/风险 | 握手/0-RTT | UDP 支持 | 服务端实现（语言/维护状态，查于 2026-09-11）| 客户端支持 | 结论 |
|---|---|---|---|---|---|---|---|---|
| **VLESS+REALITY+Vision** | TCP | 借用真实网站证书+握手；Vision 拉平 TLS-in-TLS 长度特征 | 需保持 uTLS 指纹与真实浏览器同步；ClientHello Session-ID 是认证标记，理论上可被规模化分析（未见实证）[27] | 1-RTT TLS（无 0-RTT）| 否（需单独 UDP 通道） | Xray-core，Go，**非常活跃**（v26.9.9，当日推送）[20] | v2rayN/sing-box iOS·Android·macOS/Clash Verge/Shadowrocket 全支持——覆盖面最广 | **推荐：TCP 主力，不动** |
| **VLESS+XHTTP(+REALITY)** | HTTP/1.1、H2 或 QUIC/H3 | 伪装成普通 HTTP 请求；REALITY 变体不能过 CDN，非 REALITY 变体可过 CDN 但靠 CDN 自身 TLS | REALITY-XHTTP 与上面同样的指纹风险；非 REALITY-XHTTP 依赖 CDN 自身伪装强度更弱；packet-up 模式 2025-26 已接近 stream-up 性能 | 1-RTT | 仅 H3/QUIC 变体 | Xray-core，Go，活跃 [7][8] | sing-box 原生；v2rayN 经 Xray 核心；mihomo 2026 仍有 bug（缺 MLKEM768、不跟随重定向、缺 GET 上行）[97] | **候选：作为同安全级别的传输层备选，仅配置层面改动，不作为 CDN 前置方案** |
| **Hysteria2** | QUIC/UDP | 默认无 TLS 拟态；Salamander 混淆报文字节；端口跳跃规避单端口限速 | GFW 的 QUIC SNI/Initial 检测（measured）可命中未混淆 Hysteria2；China 需 Salamander+端口跳跃组合 | 0-RTT 可用 | 原生支持 | apernet/hysteria，Go，**非常活跃**（v2.12.2，2026-09-06）[22] | 全平台支持 | **推荐：UDP 主力，不动** |
| **TUIC v5** | QUIC/UDP | 无拟态，依赖 QUIC 自身加密+可配置 CC | 同样暴露在 QUIC 检测下，且多数部署无 Salamander 等价物；**参考实现自 2025-05 起无提交**，仅靠 sing-box/mihomo 自行重实现维持 | 0-RTT | 原生支持 | EAimTY/tuic 事实性无人维护 [17]；社区继任者分裂（tuic-protocol vs Itsusinn/tuic）| sing-box/mihomo 支持，客户端支持早已脱离参考实现 | **不建议**：治理风险高，且不在现有拓扑中，引入即增加第 4 条 wire 协议 |
| **ShadowTLS v3 + SS-2022** | TCP（TLS 封装） | 真实 TLS 握手后复用会话传加密流量 | 参考实现 **自 2025-04 起停滞超一年**；SS-2022 本身仍需过熵检测 | 1-RTT 后近 0 | 否 | ihciah/shadow-tls，Rust，**停滞**[19]；shadowsocks-rust 本身活跃 | sing-box/mihomo 支持，v2rayN 部分 | 不建议作为新增主力 |
| **Shadowsocks-2022（裸）** | TCP/UDP | 全加密，无 TLS 拟态 | 正是 2021 年"全加密流量检测器"的靶子，China 内需外包裹层 | 1-RTT | 支持 | shadowsocks-rust，Rust，**非常活跃**[32] | 最广泛（历史最久） | 不建议裸用于 China 直连 |
| **Trojan(-Go)** | TCP（真实 TLS） | 伪装成 HTTPS，非 Trojan 流量落到真实网站 | 密码鉴权已被熟知；**原版 trojan-go 自 2024-07 起停滞**[18] | 1-RTT | 否 | p4gefau1t/trojan-go 事实性放弃 | 广泛但趋势下降 | 不建议 |
| **NaiveProxy** | TCP（H2 CONNECT over 真实 TLS）| 直接嵌入 Chromium 网络栈 | 资源占用重——在 2vCPU/1GB VPS 上是硬约束 | 1-RTT | 否 | klzgrad/naiveproxy，C++，**非常活跃**[23] | 需专用客户端，生态窄于 Xray/sing-box | 不评估为主力 |
| **mieru** | TCP/UDP | 自定义 AEAD+时间派生密钥 | 部署基数小，现实对抗证据少 | 1-RTT | 支持 | enfein/mieru，Go，**极活跃**（周更级）[24]| v2rayN/Shadowrocket 无 GUI 集成 | 不适合面向 Windows/v2rayN 用户群 |
| **AnyTLS** | TCP（真实 TLS） | 类 ShadowTLS/Trojan 拟态，意在用单协议替代多协议堆叠 | 2025 年新协议；客户端元数据字段有自曝指纹风险；**无标准订阅链接格式**，v2rayN/Shadowrocket 支持"有限" | 1-RTT | 否 | sing-box 原生，Go，活跃 [26] | 生态未成熟 | 6-12 个月后复查，暂不引入 |

**未纳入主表、仅作参考的两类协议**：(1) WireGuard/AmneziaWG——原生 WireGuard 因固定格式握手是最容易被指纹化的协议之一（无拟态设计），AmneziaWG 的垃圾报文填充+报头混淆防御据称正在被更新的检测手段追上（anecdotal，未证实的"2026 Q2 机器学习升级"说法不可信），且两者都不在 v2rayN/sing-box/Xray 的订阅生态内，与 b-ui 用户群脱节；(2) Tor 可插拔传输（obfs4/snowflake/meek）——仅作学术参照，obfs4 历史上已被主动探测攻破过，且不是通用代理场景的部署候选。两者均不建议纳入 v4 协议集。

**推荐 TCP 车道**：VLESS+REALITY+Vision 保持主力（客户端支持面最广、核心最活跃、大陆运营者社区最成熟）；把 VLESS+XHTTP+REALITY 作为同安全级别的传输备选（纯配置层改动，无需新依赖），用于 Vision 特定长度/时序特征一旦被针对性指纹化时的切换手段——注意它不是 CDN 前置方案，只是同一台 VPS 直连上的另一种 TCP 传输。

**推荐 UDP 车道**：Hysteria2 + Salamander + 端口跳跃不变。不迁移到 TUIC（参考实现停摆是 measured 事实，依赖 sing-box/mihomo 的重实现是单点治理风险）；QUIC-XHTTP 解决的是 CDN 前置问题，与 Hysteria2 解决的"直连、抗丢包、高吞吐 UDP"不是同一问题，除非 v4 明确需要 CDN 回退车道，否则不必引入。

**为什么不建议同时加两条新车道**：横评里唯一值得纳入 v4 路线图的"新增项"只有 VLESS+XHTTP+REALITY 一个——它不需要新二进制、不需要新客户端类型（Xray-core 本身已支持），只是同一台服务器上的第二个 Xray inbound 配置。其余所有候选（TUIC、ShadowTLS、AnyTLS、mieru、NaiveProxy）都要求引入一个新的服务端进程/新的客户端协议类型，而协议横评的结论是：这些项目要么治理风险高（TUIC 参考实现停摆、分叉），要么生态不成熟（AnyTLS 无标准订阅格式），要么与 b-ui 的用户画像（Windows v2rayN + Linux bash 客户端）不匹配（mieru 无 GUI 客户端、NaiveProxy 资源占用重）。第 2 节的"更少协议家族暴露"原则在这里直接落地为一条判断标准：新协议只有在不增加客户端协议类型数量、且用现有二进制就能实现时才值得引入。

---

## 4. 拓扑模式

| 模式 | 额外 RTT/跳数 | 2025-2026 可检测性 | 主要失效模式 | 运维复杂度 | 成本 |
|---|---|---|---|---|---|
| (1) 单 VPS 直连——b-ui 现状 | 0（客户端直连 VPS） | 用 REALITY+Hysteria2 跳跃时攻击面最小（击败主动探测），仍暴露于整网段黑洞和批量 SNI/QUIC 限速扫荡（如 2026-04 事件类比）| IP/ASN 网段被黑洞；单节点单点故障 | 最低——单机，配置一次生成 | 最低 |
| (2) 国内中转（IEPL/IPLC 或 BGP relay → 落地 VPS）| +1 跳；专线段延迟增量可忽略，BGP relay 段有公网波动 | 专线段绕开 DPI，但落地节点公网 IP 仍可被探测；2026-04 一周内约 60-70% 大陆 IDC 中转链路同时遭 RST+QUIC 限速+黑洞（anecdotal）| 大陆 IDC 段整体被针对性打击 | 中高——多一台机器要保持同步 | 高——专线 Mbps 计费，2026-04 事件后订阅价约翻倍 |
| (3) CDN 前置（CF/Fastly + WS/gRPC/XHTTP，Argo/Workers）| +1 跳，边缘节点若离用户近可能净减延迟 | 高且上升：XHTTP+H3 过 CF 代理模式记录为不稳定（仅 DNS-only 生效，等于暴露源站 IP）；*.workers.dev/*.pages.dev 疑似 2026 起部分被 SNI 黑名单 | CDN 域名模式被封、CDN 滥用限流、CDN 边缘自身 QUIC 限速 | 中——依赖第三方产品行为变化 | 直接成本低（CF 免费层）但脆弱性成本高 |
| (4) 多跳/relay 链 | 每跳线性叠加延迟；任一跳明文仍可指纹则无 DPI 收益 | 每增一跳=多一个可单独封锁的 IP，跳数本身不击败 SNI/内容检测 | 链中任一跳失效即全链失效 | 高——N 份配置、N 个故障点 | 高——N 台 VPS 成本 |
| (5) 出口分层（b-ui 现有住宅出口 relay：sing-box → socks5/http 上游）| 仅在目的地方向的那一段加 1 跳；客户端到 VPS 的握手不受影响 | 客户端-VPS 段的 REALITY/Hysteria2 反探测特性不受影响；只影响目标域名看到的出口 IP 声誉，这正是买住宅出口要解决的问题 | 上游住宅/ISP 代理池耗尽或被封；b-ui 设计为 fail-open 直连而非硬失败 | 中——多一个本地 relay 进程+健康检查，但与两个 REALITY/Hysteria2 监听器隔离，从不被改写 | 中高——按 GB 或按 IP 付费的住宅代理池 |
| (6) Anycast/GeoDNS 多节点 LB | 可能降低 RTT（路由到最近 PoP），但需要小运营者通常没有的 BGP 级基础设施 | 本身不是反 GFW 手段；未见个人/小规模运营者对原始代理节点做真 Anycast 的案例；多用于 CDN 前置层，继承其可检测性 | 需要真实 BGP 存在——2 VPS 规模不现实 | 极高 | 极高——企业级 Anycast 托管 |
| (7) 端口跳跃/宽端口范围 | 0 额外 RTT（同一物理跳，仅轮换 5 元组）| 专门击败"单端口长时 UDP 流"指纹；不击败逐包 SNI/内容检测（那是 REALITY/Salamander 的职责） | GFW 的整端口无差别 RST 事件（2025-08）绕过了"针对特定端口"这个前提 | 低——一次性配置时决策，b-ui 已实现 | 免费 |
| (8) 商业机场式面板+Agent（V2board/Xboard ↔ XrayR/V2bX 每节点）| 0 额外客户端 RTT（agent 与代理内核同机运行）；新增约一个轮询周期（~60s）的用户变更传播延迟 | 不直接影响 GFW 可检测性（纯运维层模式，与协议选择正交）——但在 1000-10000 用户规模时，比 b-ui 现在"每节点手写配置文件由 shell 重新生成"的模型更能集中管理多节点用户 | 面板 API 不可达→节点保留最后已知用户列表（优雅降级）但无法增删用户；agent 进程本身多一个崩溃/升级点 | 中——每节点多一个常驻 Go 进程+中心面板数据库，但去掉了 b-ui 现在"按节点 shell 重生成配置"的模式 | 低增量成本——agent 是免费 OSS，面板需要自己一台小 VPS/数据库 |

**这个运营者最简单可靠的拓扑**：保持模式 (1)+(5)+(7) 的组合——单 VPS 直连 + REALITY/Hysteria2 端口跳跃 + 住宅出口分层 relay——正是 b-ui 目前的架构，不是需要替换的落后设计。USENIX 2025 论文和 2025-08 的 443 RST 事件都指向同一个方向：GFW 检测越来越聚焦于（a）解密/检查 QUIC Initial 里的明文 SNI，（b）粗暴的整段/整端口操作，而不是"数跳数"或"路由花活"。加更多拓扑层（中转跳、CDN 前置、anycast）都不解决这两个问题，反而增加攻击面。

**"更聪明"在这里的具体含义**，拆成四条可执行标准：

1. **协议家族做减法，不做加法**——每一个暴露给 GFW 的入站协议都已经是 TLS/QUIC 伪装的（REALITY 的真实 TLS 握手、Hysteria2 的 QUIC+Salamander），不新增任何达不到这个门槛的协议（裸 Shadowsocks、无 TLS 的 VMess 等）。
2. **拓扑层做减法，不做加法**——把现有的最小复杂度拓扑（模式 1+5+7 的组合）做得更可靠，而不是叠加模式 2/3/4/6 里任何一层；每加一层都是新增一个可被单独针对的 IP/域名，而 2025-2026 的证据显示这恰恰是审查方实际在打击的对象。
3. **可靠性从"协议更隐蔽"转移到"故障恢复更快"**——2025-08 的整端口 RST 事件和 2026-04 的中转打击共同说明，下一次大规模影响 b-ui 的事件更可能是"某个 IP/网段被盯上"而非"某个协议被破解"；相应地，"更聪明"应体现为快速 IP 轮换、健康检查驱动的自动切换、敏感时期降级预案，而不是又发明一种新的流量伪装。
4. **运维层是唯一有结构性缺口、且减负担明确对齐"更简洁"的地方**——模式 (8) 指出的面板+Agent 模式说明，b-ui 现在"改配置文件+重启服务"的用户管理方式，在千用户规模下开始显现出与商业机场实践的差距（前者是同步阻塞式全量重写，后者是异步增量轮询）。这也恰好是 Rust 化投入产出比最高的地方——协议内核（Xray-core/Hysteria2）已经是被优化过的成熟 Go/C 实现，重写它们的收益证据薄弱（见第 5、6 节），但控制面（面板、住宅 relay 控制器、客户端安装脚本）用 Rust 重写没有协议兼容性风险，且直接服务于"更简洁"这个目标（见第 8 节方向 A）。

---

## 5. Rust 生态现状

### 5.1 项目现状表

| 项目 | 覆盖 b-ui 相关协议 | Stars | 最近推送 | 许可证 | 维护状态 | 生产证据 |
|---|---|---|---|---|---|---|
| shadowsocks-rust [32] | 仅 Shadowsocks | 10,855 | 2026-09-11（当日） | MIT | **非常活跃** | 广泛采用、官方推荐实现 |
| **cfal/shoes** [33] | VLESS+REALITY+Vision、Hysteria2、TUIC v5、AnyTLS、Trojan、SS、TUN 模式 | 1,223 | 2026-08-09 | MIT | 活跃，小团队/单维护者 | **未经规模验证**——维护者本人明确表示"从未与 sing-box/Xray 做过对比测试"[92] |
| leaf (eycorsican) [34] | Trojan（入+出）；VLESS/REALITY **仅出站** | 2,827 | 2026-09-10 | Apache-2.0 | 活跃 | 用于部分 iOS 代理 App（未确认具体哪些）；**不能替代 REALITY 服务端** |
| clash-rs (Watfaq) [35] | Clash/mihomo 规则引擎重实现 | 1,713 | 2026-09-10 | Apache-2.0 | 活跃 | 无采用数据 |
| tuic-protocol/tuic [36] | TUIC v5（QUIC 0-RTT）| 3,273 | 2025-05-15 | GPL-3.0 | 曾被归档一次，2025 年在新组织下复活 | 规范文档化，但分叉严重（另有 Itsusinn/tuic 自称"继任者"[37]）|
| Nehereus/rusteria [38] | Hysteria2 服务端（业余项目）| 11 | 2025-04-03 | 无许可证 | 停滞 | 无 |
| trojan-r / TrojanRust / trojan-rust [39][40][41] | Trojan | 337 / 120 / 3 | 2021 / 2023 / 2026 | GPL/MIT/GPL | 已死 / 已死 / 低信号 | 无 |
| boringtun (Cloudflare) [42] | WireGuard（非 GFW 代理协议）| 7,190 | 2026-06-29 | BSD-3-Clause | 活跃 | **巨大**：Cloudflare WARP，数百万设备部署 |
| quinn [43] | 通用 QUIC 传输 | 5,254 | 2026-09-10 | Apache-2.0 | 非常活跃 | 广泛内嵌，但在 interop 测试中部分场景比 quiche 慢 2 倍以上 |
| s2n-quic (AWS) [44] | 通用 QUIC 传输 | 1,372 | 2026-09-10 | Apache-2.0 | 活跃 | AWS 生产环境（2022 年公告）[50] |
| rustls [45] | TLS 1.3 | 7,612 | 2026-09-09 | Apache/MIT/ISC | 非常活跃 | 广泛内嵌（AWS、Cloudflare 等） |
| 3andne/craftls [46] | 类 uTLS ClientHello 拟态 | 27 | 2024-01-19 | 未标注 | **停滞约 20 个月** | 无——**Rust 生态目前没有等价于 Go utls 的成熟项目** |
| REALITY（协议）[27] | — | n/a | n/a | n/a | 只有 shoes 移植到 Rust | 参考实现是 Go（Xray-core）|
| XHTTP（协议）[7] | — | n/a | n/a | n/a | **未找到任何语言的 Rust 实现** | 参考实现是 Go（Xray-core）|
| XrayR / V2bX（面板 agent）[49][51] | — | n/a | n/a | n/a | **纯 Go 生态，无 Rust 对应物** | 广泛用于商业机场 |

### 5.2 shoes 与 b-ui 客户端栈的实测互操作性（追加调查）

cfal/shoes 是唯一同时实现 REALITY 服务端和 Hysteria2 服务端的 Rust 项目，值得单独展开：

- **REALITY 握手**：2026-05-26 的 PR #133 [85] 修复了与 Xray-core 客户端（即 v2rayN 使用的核心）之间的 **3 个独立 TLS 握手兼容性 bug**（signature_algorithms 扩展错误、transcript-hash 字节错误、Xray-core 的零填充 TLS1.3 记录被误判为截断），维护者自述"我们主要只测试过对 shoes 自己的服务端和 sing-box"——即 Xray-core 特定行为不在默认测试矩阵内，是外部贡献者在真实 3x-ui/Xray-core 部署中撞见才被发现修复的。另有 2026-02-08 的 PR #105 [86] 修复了 short_id 零填充方向错误，此前会导致与 Xray-core/sing-box/Mihomo 的鉴权失配。一位真实用户曾用 shoes 作 REALITY 服务端对接实际 v2rayN 客户端跑通（但吞吐仅为 sing-box 同配置下的约 30%，维护者未能复现，issue 未解决关闭）[87]。**结论：功能性互通存在，但只在被动撞见 bug 后修复，没有主动的 Xray-core 兼容性测试套件**。
- **Hysteria2 混淆**：shoes 的 Hysteria2 配置模式（CONFIG.md）只暴露 `password`/`udp_enabled` 字段，**全文没有 obfs/Salamander/masquerade 字段**[88]。如果 b-ui 任何生产节点启用了 obfs（CLAUDE.md 提到 `b-ui-cli.sh` 有 obfs 开关），这些客户端切到 shoes 服务端会直接不兼容，除非先关闭 obfs。
- **端口跳跃**：shoes 有原生端口范围绑定能力（PR #54，2025-03-06）[89]，架构上是正确的原语，但没有任何 issue/PR 显示对真实 `mport=` 客户端（v2rayN 内置 sing-box、bui-c 自身 Hysteria2 客户端）做过端到端测试；已有的端口跳跃相关 PR（#118/#120/#122）只测过"shoes 作为客户端"，且测试者用的是"Android lib"[90]。
- **拥塞控制**：一位用户在 2026-01-21 直接对比 shoes vs sing-box（相同协议）报告"非常慢"，之后 BBR 相关 PR 反复修改，截至最近一次搜索仍有 2026-08 的开放 PR 在调整 QUIC 入站拥塞控制算法 [91]——BBR/吞吐与官方 Hysteria2（apernet）的对等性尚未确立。
- **生产规模负载**：**shoes 维护者本人明确表示从未与 sing-box/Xray 做过对比测试**[92]；shoes 有一个反复修补的 AnyTLS 内存增长 bug（3-4 个客户端下 3 天增长约 2GB RSS，PR #153 仍在修）[93]；更关键的是，shoes **原生不支持 b-ui 面板运营所依赖的两个能力**——运行时动态增删多用户（issue #128，0 回复）[94]和按用户流量统计/限速钩子（issue #154，0 回复）[95]，两者都是开放中、无维护者响应的 feature request。

**结论**：把 shoes 当作"现成可用的 REALITY+Hysteria2 服务端"是不成立的——它更接近"一个值得借鉴设计、但需要先 fork 补齐核心运维能力、再做正式 wire-compat 和负载测试"的起点，而不是可以直接替换 Xray-core+Hysteria2 二进制的生产依赖。

### 5.3 "核心代码用 Rust"三档范围的诚实工作量估计

**(i) 仅控制面**（面板/安装器/更新器/relay 控制器/客户端，Go 内核不动）
把 `web/server.js`（Node）重写为 Rust（如 axum）、把 `residential-helper.sh`+`resi-health.sh` 合并成一个 Rust agent、把 `b-ui-client.sh` 的 Linux 逻辑基于成熟的 `tun2proxy`[47]（跨 Linux/Android/macOS/iOS/Windows，含 Wintun 绑定，2026-06-17 仍活跃）逐步重写；Xray-core、Hysteria2、sing-box relay 二进制原样保留。
**工作量**：数周级工程，**无协议兼容性风险**（wire 格式完全不变，只是管理这些进程的代码换了语言）。这是唯一一档没有找到 Rust 生态"空白"的范围，也是本研究最能站得住的推荐方向。

**(ii) 部分数据面**（把成熟的 Rust 实现用在其真正成熟的场景，如把 shadowsocks-rust 用于未来若要新增的 SS-2022 车道；或评估 shoes 替换 Xray-core/Hysteria2）
**工作量**：shoes 路线需要先花约 1 天做 wire-compat 验证（导入现有 `reality-keys.json`、对接真实 v2rayN/bui-c 客户端、测试 obfs 场景），随后是数周到数月级的 fork 工作补齐 Authenticator trait（动态多用户）和 TrafficObserver hook（流量计费/限速），再叠加一轮在真实硬件（1vCPU/1GB）上对 500-1000 并发用户的负载测试——这部分测试目前在公开互联网上**完全不存在**，无论是 shoes 还是任何其他候选（shadowsocks-rust 的内存报告也只是路由器级小规模 DNS 突发问题，非规模化负载数据）[96]。

**(iii) 全 Rust 数据面**（含 REALITY、Hysteria2 从零重实现）
- REALITY：除 shoes 外无独立 Rust crate；参考实现仍是 Go 的 Xray-core [27]。
- Hysteria2：除 shoes 内置版本外，唯一独立项目是 11 星的业余项目 rusteria，**无生产级 Rust Hysteria2 服务端**[38][51]。
- XHTTP：**没有任何语言实现出现在 shoes、clash-rs、leaf 里**，需要对着 Xray 仍在演进的规范从零实现 split-uplink/streaming-downlink 的 HTTP/1.1+H2+H3 传输 [7]。
- uTLS 等价物：Rust 没有成熟的 ClientHello 指纹拟态方案；唯一的尝试（craftls）已停滞 20 个月，rustls 官方的原生 ClientHello 定制 tracking issue 仍只是讨论、未落地 [46]。
**工作量**：数月到 1-2 年级的协议工程，且需要长期跟随 Xray-core/sing-box 的 wire 格式演进（正如 CLAUDE.md 已记录 sing-box 1.12→1.14 的 schema churn 迫使 b-ui 自己的订阅生成器只能定位到兼容子集）。对一个 2vCPU/1GB、目标 1000 用户、疑似小团队维护的项目而言，**风险显著高于收益**。

**对 v2rayN/sing-box 用户的兼容性后果**：无论走哪档，任何 Rust 重写的服务端都必须与 Xray-core 的 REALITY 握手字节级兼容（shoes 自己的 bug 历史证明这不是"读文档就能做对"的事），且客户端生态（v2rayN、Shadowrocket、Stash 等）无法被 b-ui 影响——只能适配它们已支持的协议，不能反过来要求它们支持一个自定义的 Rust 协议变体。

---

## 6. 延迟与效率的真相

**RTT 层（物理/路由决定，与语言无关）**
- CN2 GIA 基线：中国→香港 10-50ms、→东京 40-70ms、→美西 140-170ms（低负载）；普通 163/BGP 路线晚高峰再加 20-60ms，中国联通 169 晚高峰丢包 4%+，中国电信 163 晚高峰拥塞 [71]（**anecdotal**，机房测速类站点数据）。
- 同一线路等级下的协议对比（**anecdotal**，社区测试，非受控实验）：中国→美 REALITY 理想 140-170ms/晚高峰 160-220ms；Hysteria2(Brutal) 理想 150-180ms/晚高峰 155-200ms（峰值方差更小，因 Brutal 忽略丢包）；TUIC v5 理想 110-140ms/晚高峰 120-160ms（0-RTT 握手更精简）[74]。

**握手轮次（documented）**
- TCP+TLS1.3：约 1.5-2 RTT 到首个应用字节（TCP 三次握手 + TLS1.3 1-RTT）；REALITY/Vision 复用同一个 1-RTT TLS1.3 握手，splice 生效后不再增加额外 RTT [75]。
- QUIC（Hysteria2）：1 RTT 完成加密+传输握手（新连接），支持 0-RTT 恢复 [76]。
- TUIC v5：为 0-RTT 应用数据设计。

**丢包下的吞吐（measured，非代理专属背景研究）**
- CUBIC 类 loss-based 拥塞控制：1% 丢包可导致吞吐下降 >70%；BBR 类 model-based 拥塞控制对约 1% 丢包几乎无影响；200ms RTT/500Mbps 场景下 BBR 吞吐比 CUBIC 高约 115% [62][63]。
- Hysteria2 的 Brutal CC 刻意忽略丢包信号、按固定目标速率发送，在 GFW 常见的高丢包晚高峰链路上表现最好，但代价是**主动破坏拥塞公平性**——会从共享瓶颈的 BBR/CUBIC 流那里抢带宽；作者本人已承认并提供 `ignoreClientBandwidth` 服务端选项回退到公平控制器 [61]。

**CPU/字节成本（measured，2020 年数据，可能已随 GSO/GRO 默认化而收窄）**
- 朴素 QUIC 吞吐仅为纯 TCP 的约 28%（单核测试）；经 GSO 报文合并+更大报文+减少 ACK 频率优化后，QUIC 可追平/略超同核 TLS1.3-over-TCP [64]。这说明"QUIC 比 TCP 耗 CPU 2-3 倍"是**工程优化程度的问题**，不是 QUIC 协议内禀的语言无关税。
- REALITY/XTLS-Vision 的 splice 机制专门用于消除握手后的用户态加解密/拷贝开销，一旦 splice 生效，Xray 的 REALITY 入站 CPU 成本接近裸 TCP splice——这意味着**一个假设中的 Rust 重写在 REALITY 稳态吞吐上不太可能有可测量的 CPU 优势**，差异集中在握手期加密计算（Go crypto/tls 与 Rust rustls/ring 都有 AES-NI/AVX2 硬件加速）[75]。

**内存层（Rust 唯一有干净测量优势的地方，measured）**
- Go goroutine 最小栈约 8KB（可增长）vs Rust tokio 异步任务约 200-400 字节；100 万并发任务测试中 Go 内存占用是 Tokio 胜出方的 12 倍以上 [66]。
- 但 Xray-core 在生产中报告的严重内存问题（20MB 配置文件膨胀到 2GB RSS；数百用户/100-200Mbps 下超 2GB）经查证**主要是用户/配置表在内存中常驻和连接泄漏导致，不是 goroutine 栈开销**（见第 8 节对 #6684/#5828 的分析）[67][98][99]——这是一个可以靠修 bug/调优解决的问题，不是必须换语言才能解决的问题。

**客户端 TUN 层（documented）**
- sing-box 的 gVisor 用户态网络栈在持续 TUN 流量下 CPU 占用明显偏高，官方文档推荐 `mixed`（TCP 走 system 栈、UDP 走 gVisor）为大多数用户的默认值；2025-09 有活跃 issue 报告纯 gVisor 栈在 TUN+Hysteria2 场景下 CPU 异常，`system`/`mixed` 未复现 [65]。b-ui 的 bui-c TUN 模式应确认使用 `mixed` 或 `system`，而非 `gvisor`。

**io_uring（measured，2026 最新研究，反直觉）**
- 朴素采用 io_uring 相比 epoll 只提升约 1.06-1.10 倍；要达到 2倍+ 需要重度工程投入（注册缓冲区、批处理、零拷贝接收）；同一研究结论是"2026 年 epoll 仍是大多数网络服务的正确默认选择"[70]。这排除了"Rust+io_uring=免费的延迟提升"这个假设。

**结论**：v4 若想真正降低用户感知延迟，杠杆排序应为——(1) 线路质量/路由选择（CN2 GIA vs 普通 BGP 的差距是几十毫秒级，远大于任何协议/语言优化）> (2) 拥塞控制算法选择（BBR/Brutal vs CUBIC 在晚高峰丢包下差 4-6 倍吞吐）> (3) 握手轮次（0-RTT vs 1.5-2RTT）> (4) 减少本地跳数（客观上已经是微秒级，几乎无收益空间）> (5) 实现语言（Go vs Rust，在协议对等、工程质量相当的前提下，无可测量差异的证据）。

**给 owner 的直接回答**：如果"用 Rust 减少延迟"是 v4 立项的核心论据之一，这个论据目前站不住——本节列出的每一个真实延迟来源（线路、拥塞控制、握手轮次）都是协议/网络层决策，与实现语言正交；一个用 Go 写的、正确选用 Brutal/BBR 拥塞控制、走 CN2 GIA 线路的节点，会比一个用 Rust 写但线路差、拥塞控制选错的节点慢得多。Rust 值得投入的唯一站得住脚的技术理由是内存占用的可预测性（第 5.3 节、第 8 节方向 A/B），这个理由应该单独、诚实地对 owner 陈述，而不是包装成"降延迟"。

---

## 7. 客户端兼容矩阵

| 客户端 | 平台 | 核心 | 相关协议 | 订阅格式 | TUN | 2026 维护信号 |
|---|---|---|---|---|---|---|
| v2rayN [71] | Windows/Linux/macOS | Xray-core（VLESS/REALITY/XHTTP）+ 内置 sing-box（Hysteria2、TUN）| VLESS-REALITY、Hysteria2、XHTTP | base64 URI 列表 | 是，经 sing-box 后端，7.25.1 版封顶 sing-box ≤1.14 | 非常活跃——周更（7.25.1 @ 2026-09-10） |
| v2rayNG [72] | Android | Xray-core（默认）/v2fly | 同上 | base64 URI 列表 | 有限 | 非常活跃——周更 |
| sing-box 官方 App（SFI/SFA/SFM）[78] | iOS/Android/macOS/tvOS | sing-box 原生 | Hysteria2、VLESS-REALITY、TUIC 等 | sing-box JSON | 原生 | **iOS/macOS 分发状态不稳**：App Store 更新因审核纠纷被暂时阻断，TestFlight 仅限赞助者 |
| Clash Verge Rev (mihomo) [73] | Windows/macOS/Linux | mihomo | Hysteria2 ✅、TUIC ✅、REALITY(Vision) ✅、XHTTP ⚠️新且有 bug（缺 MLKEM768、不跟随重定向、缺 GET 上行）[97] | Clash/mihomo YAML | 是 | 发版节奏放缓（v2.5.1→v2.5.2 间隔 2 个月） |
| Shadowrocket | iOS | 闭源 | Hysteria2 ✅、REALITY ✅ | base64 URI/QR | 是（VPN Profile）| 活跃，App Store 抗审核能力强——大陆 iOS 用户事实标准 |
| Stash [79] | iOS/macOS | 闭源（Clash 兼容）| Hysteria2 ✅（含混淆）、TUIC v4/v5 ✅、VLESS ✅、WireGuard ✅ | Clash 风格 | 是 | 活跃（2026-03-31 TUIC 兼容修复） |
| NekoBox for Android [76] | Android | sing-box | sing-box 所支持的一切 | sing-box JSON/URI | 是 | **停滞**——最后发布 2026-02-09，社区多个 fork 接棒 |
| Hiddify [77] | 跨平台（含 iOS 侧载） | sing-box（Flutter 包装） | sing-box 所支持的一切 | sing-box JSON | 是 | v4 系列自 2026-02 起恢复发版，此前有 16 个月空窗，需重新核实协议/schema 支持 |
| Loon | iOS | 闭源 | 本次未核实 | Loon 专有 | 是 | 本次未研究——存在覆盖缺口 |

**这套矩阵施加的约束**：（1）没有一个主流客户端核心同时原生打包 REALITY 和 Hysteria2 的实现，v2rayN/v2rayNG 靠内部双核心（Xray-core+sing-box）解决——任何服务端协议决策都应假设客户端继续走这条路，而不是假设存在统一核心。（2）iOS 是 v4 最脆弱的平台：官方 sing-box App 目前无法可靠上架更新，实际可依赖的是 Shadowrocket/Stash 两个闭源客户端，b-ui 无法影响其协议路线图，只能对齐它们已支持的协议（Hysteria2、REALITY、TUIC 安全；XHTTP 尚不安全）。（3）不存在、也不会出现统一订阅格式——v2rayN/v2rayNG 要 base64 URI，sing-box 系客户端要 sing-box JSON，Clash 系要 YAML，第三方转换服务（Sub-Store、subconverter）正是为弥合这道结构性鸿沟而存在 [81]；b-ui 现在维护三套生成器不是可以省略的捷径，而是结构性必需——真正该修的是这三套生成器**内部** 4-5 份手工同步的节点集合逻辑（第 1、10 节已述）。

**自研客户端选项**：Linux/Windows/macOS/Android 方向可行——`tun2proxy`[47] 是成熟、跨平台、持续维护的 TUN-to-SOCKS5/HTTP 用户态栈，是自研 Rust 客户端 TUN 模式的现实构建块。iOS 仍是难点：任何自研客户端都需要 NetworkExtension 权限和 App Store 审核，会撞上 sing-box 团队正在经历的同一种审核风险。现实选项是（a）继续依赖 Shadowrocket/Stash 的标准订阅链接（零控制力但当下可用），或（b）接受 TestFlight/侧载分发（限制主流触达）。现有证据不支持"全面自研客户端替代 v2rayN/Shadowrocket"——更高投入产出比是先修复第 10 节的内部重复，并保持对现有客户端生态的最大兼容。

---

## 8. 对 b-ui v4 的含义

本节把前 7 节的证据落到 b-ui 今天实际运行的四层架构上（Install/Bootstrap、Server-side shell、Web 面板、Linux 客户端——见 CLAUDE.md [82]）。核心判断方式是：先问"这个组件今天暴露在哪条 GFW 检测向量下"（第 2 节），再问"现有 Rust 生态在这个组件上是否有站得住脚的替代品"（第 5 节），最后问"换掉它能不能带来第 6 节里论证过的真实收益"。三个问题都过关的组件才值得纳入 v4 的 Rust 化范围；只过第三关（"用 Rust 写会更好看"）但过不了前两关的组件，不建议纳入。

**今日栈 → 保留/修改/舍弃 映射**

| 组件 | 判断 | 依据 |
|---|---|---|
| VLESS-REALITY-Vision（vless-direct :10001, vless-residential :10002，network:tcp）| **保留** | 无大陆专属证据显示被指纹化；客户端支持面最广；splice 后 CPU 成本已接近裸 TCP [104][105][106][107][75] |
| Hysteria2 + Salamander + 端口跳跃（config.yaml / config-residential.yaml）| **保留，且视为承重设计** | GFW QUIC SNI 解密是 measured 现网能力，未混淆直接暴露 [1] |
| 双端口族（443 REALITY + 高位端口 Hysteria2，direct+residential 各一套）| **保留** | 对冲已 measured 的整端口 443 RST 事件 [9] |
| 住宅出口 sing-box relay（127.0.0.1:2080，fail-open）| **保留** | loopback 延迟可忽略；解决的是出口 IP 声誉问题，不是拓扑问题 [69] |
| Node.js 面板（web/server.js）| **候选改为 Rust（axum 等）**，控制面优先级最高 | 无 wire 协议兼容性风险，可清理 4-5 处重复节点集合逻辑 |
| residential-helper.sh / resi-health.sh | **候选合并为单一 Rust agent** | 同上，纯运维层改动 |
| b-ui-client.sh（Linux 客户端）| **候选基于 tun2proxy 渐进 Rust 化** | 成熟跨平台库已存在 [47] |
| Xray-core / Hysteria2 / sing-box relay 二进制内核 | **不建议替换**，除非先完成 shoes 的 wire-compat+负载验证 | 无生产级 Rust REALITY/Hysteria2 实现；shoes 缺多用户/流量钩子，无 obfs [33][88][94][95] |
| 新增国内中转层 / CDN 前置 | **明确不做** | 2026-04 中转打击、CF XHTTP 不稳定的一致证据方向 [52][58][59] |
| TUIC / AnyTLS / NaiveProxy / mieru 作为新增主力协议 | **不做** | 治理风险（TUIC）、生态不成熟（AnyTLS）、资源占用（NaiveProxy）、客户端生态窄（mieru） |

**三个候选架构方向**（非最终设计，供 owner 的设计者决策参考）：

**方向 A：运维层 Rust 化，协议内核不动（保守路线）**
Rust 落点：面板（axum 重写 web/server.js）、把 residential-helper.sh+resi-health.sh 合并为一个常驻 Rust agent、b-ui-client.sh 的 Linux 部分基于 tun2proxy 渐进重写；Xray-core/Hysteria2/sing-box relay 二进制原样保留。协议集：完全不变（REALITY+Vision TCP/443 ×2、Hysteria2 UDP ×2、经 sing-box 的住宅 relay）。拓扑：单 VPS 直连不变，新增一份共享节点集合 schema，由 Rust 控制面统一渲染三种订阅格式，替代 web/server.js 里 4-5 份手工同步逻辑。主要风险：收益集中在内存可预测性和运维简化，不是延迟；仍依赖 Xray-core 已知的 REALITY 连接泄漏历史（#5828 已修/#6684 未解决），必须自行做 soak test 并准备降级预案（watchdog/自动重启）。

**方向 B：数据面部分迁移到 shoes 试点（中等路线）**
Rust 落点：在第三台测试机或 bwg-tizi 上并行部署 cfal/shoes 承载 REALITY+Hysteria2，先做 wire-compat 与吞吐 soak test；控制面同方向 A。协议集：不变，但服务端二进制换为 shoes（需先 fork 补齐 Authenticator/TrafficObserver 钩子——目前均是无人响应的开放 issue）。拓扑：不变；shoes 不支持 Salamander，需先确认生产是否已启用 obfs，若已启用则本路线要么放弃 obfs、要么只迁移 REALITY 部分保留官方 Hysteria2。主要风险：500-1000 用户/1vCPU/1GB 规模下的 CPU/RSS 完全无第三方或作者测量数据；对 v2rayN/bui-c 的 wire 兼容性历史上出过 3 个已修复的握手 bug，暴露出"缺乏主动测试矩阵"的项目成熟度问题。

**方向 C：保持现状+强化韧性（最小改动路线）**
Rust 落点：仅在新增功能处引入 Rust（例如一个独立的健康检查/IP 轮换 runbook 小工具），核心 Go 组件与 Node 面板均不动。协议集：不变。拓扑：不变，但把"IP/ASN 被黑洞"列为一等失败模式，建立自动切换预案（呼应 2025-08 RST 事件和 2026-04 中转打击的教训），并为敏感时期窗口新增降级开关。主要风险：不满足 owner "核心代码尽量用 Rust" 的目标；4-5 处节点集合逻辑的技术债继续累积；作为"v4"定位偏弱，但风险最低、最快可落地，适合作为方向 A/B 之前的过渡步骤。

---

## 9. 未决问题

本次研究依赖公开来源（学术论文、官方文档、GitHub issue/PR、社区博客）加上一次生产服务器快照，覆盖面受限于两点：一是中国大陆审查行为本身缺乏公开、可复现的实时测量渠道（gfw.report 这类项目的测量频率和覆盖协议都有限）；二是 Rust 候选项目（尤其 cfal/shoes）在生产规模下的表现完全没有第三方数据，只能靠项目自身的 issue 历史侧面推断。以下问题**无法**仅从公开来源确证，建议以 bwg-rick（先测原则，符合用户既有 bui-rollout-order.md 惯例）为对象做经验验证，每条都给出具体测试设计而非只提出问题：

1. **Xray 26.3.27 在 b-ui 实际 TCP+REALITY 配置下是否存在 #6684/#5828 式连接泄漏？** 测试方法：在 bwg-rick 上对 vless-direct 做 150-300 并发模拟用户、48-72 小时 soak test，同时用 pprof/RSS 采样，对比 2026-08 issue 报告的增长曲线形状（而非只看用户数是否达标）[98][99]。
2. **shoes 的 REALITY 服务端是否能对接 b-ui 现有的 reality-keys.json（Xray-core `x25519` 生成）？** 测试方法：在一台一次性测试机上跑一份最小 shoes 配置，直接粘贴现有密钥对，用真实 v2rayN 客户端连接验证握手成功率与吞吐。
3. **shoes 的 Hysteria2 在生产规模下 BBR/吞吐是否与官方 apernet/hysteria 对等？** 测试方法：同一测试机上对照跑 shoes vs 官方 hysteria2，用 iperf3 风格的多并发压测，采集吞吐/延迟/CPU/RSS 曲线。
4. **中国电信/联通/移动在 2026 年对 Hysteria2/QUIC 的真实限速情况**（"30 秒识别""163 骨干网 1Mbps 限速"等具体数字均未经证实）。测试方法：在 bwg-rick/bwg-tizi 上做为期一周的 A/B——A 组保持当前 Salamander+端口跳跃配置，B 组临时关闭混淆，对比晚高峰吞吐/延迟/连接成功率差异，直接产出一手数据而非依赖 SEO 博客。
5. **REALITY/Vision 是否在中国大陆（而非 Iran/Russia）被规模化指纹化？** 本次系统性检索 gfw.report、XTLS/Xray-core、XTLS/BBS、net4people/bbs 均未发现确证来源；建议将此检索本身设为周期性任务（如季度复查），而非一次性结论。
6. **XrayR/V2bX 式面板+Agent 模式在 2vCPU/1GB 节点、500-1000 并发用户下的真实 RAM/CPU 占用？** 未找到任何公开基准；这决定了方向 A/C 中"是否值得引入常驻 agent 进程"的判断，建议直接部署 V2bX 做一次性资源画像测试。
7. **b-ui 自身当前生产内存/CPU 画像是否会随用户数增长到 150-300 区间（已知泄漏触发区间附近）？** bwg-rick/bwg-tizi 目前仅 5-6 用户，与 v4 目标 1000 用户相差 2 个数量级；建议在用户数自然增长过程中持续采样 RSS 曲线，而非等到接近阈值才关注。

**推荐的一周 A/B 测试设计（可直接在 bwg-rick 上执行，呼应问题 4 与 7）**：

- **第 1-2 天（基线）**：不改动任何配置，用现有 `resi-health.sh`/watchdog 日志加一份定时 `ps`/`free` 采样脚本（cron 每 15 分钟一次），建立 RSS/CPU/连接数的基线曲线，同时记录晚高峰（20:00-24:00）与非高峰时段的 Hysteria2/REALITY 连接成功率和平均吞吐。
- **第 3-4 天（混淆对照）**：在一个不影响现有真实用户的测试端口上，并行起一个关闭 Salamander 的 Hysteria2 监听器，用境内测试节点（如用户 MEMORY.md 中记录的 baiyi Linux 客户端主机）分时段对比开/关混淆下的连接成功率、首包延迟、晚高峰丢包率，直接产出第 4 条未决问题需要的一手数据。
- **第 5-6 天（负载探测）**：用 bui-c 或 v2rayN 模拟多并发连接（不需要真实到 150-300，用可达到的最大值即可，例如 20-50 并发长连接反复建连断连模拟真实使用模式），观察 Xray 26.3.27 的 RSS 是否呈现 #6684/#5828 报告过的单调增长模式（而非只看峰值是否达标），为问题 1 提供 b-ui 自身数据而非依赖第三方报告外推。
- **第 7 天（复盘）**：对比基线与两组对照数据，产出一份"是否需要在 v4 上线前处理"的结论，而不是把这类验证推迟到 1000 用户规模才做。

---

## 10. 参考文献

1. https://gfw.report/publications/usenixsecurity25/en/ — USENIX Security 2025，GFW QUIC SNI 审查论文（gfw.report 主页）。2025-07-31。
2. https://www.usenix.org/system/files/usenixsecurity25-zohaib.pdf — 同上论文 PDF 镜像。2025。
3. https://www.usenix.org/system/files/sec23fall-prepub-234-wu-mingshi.pdf — USENIX Security 2023，"How China Detects and Blocks Fully Encrypted Traffic"。2023-08。
4. https://geneva.cs.umd.edu/posts/fully-encrypted-traffic/en/ — Geneva/UMD，全加密流量检测的早期主动测量，含 VPS ASN 偏向性数据。2021-11-18。
5. https://github.com/XTLS/Xray-core/discussions/3269 — "Investigation on Blocking of Reality in IRAN"，Iran REALITY 探测社区调查。2023-12 至 2026-06。
6. https://github.com/XTLS/Xray-core/issues/5332 — 俄罗斯 TCP+Reality 被封报告，未获维护者诊断，未解决。
7. https://github.com/XTLS/Xray-core/discussions/4113 — "XHTTP: Beyond REALITY"，XHTTP 设计动机讨论。2024-2025。
8. https://github.com/XTLS/Xray-core/discussions/3555 — SplitHTTP 讨论串。2024 年中。
9. https://github.com/net4people/bbs/issues/511 — 2025-08-20 整端口 443 RST 注入事件，社区抓包分析。
10. https://www.petsymposium.org/foci/2025/foci-2025-0016.pdf — FOCI 2025，ECH 与审查规避论文（Niklas Niere）。2025。
11. https://gfw.report/publications/imc20/en/ — IMC'20，"How China Detects and Blocks Shadowsocks"（主动探测机制）。2020-10。
12. https://greatfirewallguide.com/lab/hysteria2 — 未经证实的 SEO 内容农场站点，具体数字声明无法溯源。2026（不确定）。
13. https://www.abc.net.au/news/2026-06-04/as-beijing-cracks-down-on-vpns-internet-users-in-china-adapt/106754254 — ABC News，2026 年 VPN 打压报道，引用 2025-04 事件。2026-06-04。
14. https://www.scmp.com/tech/policy/article/3195045/china-blocks-internet-anticensorship-tools-ahead-20th-party-congress — SCMP，二十大前封锁先例。2022。
15. https://corpus.lantern.io/techniques/tls-fingerprint/ — JA3/JA4 TLS 指纹背景资料。2025-2026。
16. https://gfw.report/blog/thoughs_on_cat_and_mouse_game/en/ — GFW Report 博客，猫鼠游戏综述。
17. https://github.com/EAimTY/tuic — TUIC 参考实现仓库，最后提交 2025-05-15，事实性无人维护（查于 2026-09-11）。
18. https://github.com/p4gefau1t/trojan-go — trojan-go 仓库，最后提交 2024-07-14（查于 2026-09-11）。
19. https://github.com/ihciah/shadow-tls — shadow-tls v3 参考实现，最后提交 2025-04-25（查于 2026-09-11）。
20. https://github.com/XTLS/Xray-core — Xray-core 仓库，v26.9.9，当日推送（查于 2026-09-11）。
21. https://github.com/SagerNet/sing-box — sing-box 仓库，v1.14.0 @ 2026-08-31（查于 2026-09-11）。
22. https://github.com/apernet/hysteria — apernet/hysteria 仓库，v2.12.2 @ 2026-09-06（查于 2026-09-11）。
23. https://github.com/klzgrad/naiveproxy — naiveproxy 仓库（查于 2026-09-11）。
24. https://github.com/enfein/mieru — mieru 仓库，v3.36.1 @ 2026-09-05（查于 2026-09-11）。
25. https://github.com/MetaCubeX/mihomo — mihomo 仓库，v1.19.30 @ 2026-08-16（查于 2026-09-11）。
26. https://sing-box.sagernet.org/manual/misc/anytls-client-metadata/ — sing-box 官方文档，AnyTLS 客户端元数据字段说明。2026。
27. https://github.com/XTLS/REALITY/blob/main/README.en.md — REALITY 官方 README，Session-ID 认证机制说明。
28. https://github.com/XTLS/Xray-core/discussions/4118 — XHTTP 相关讨论。
29. https://gfw.report/blog/gfw_shadowsocks/en/ — GFW Report，全加密流量检测基础机制博客。2021-2023。
30. https://github.com/2dust/v2rayN/wiki/List-of-supported-cores — v2rayN 支持核心列表。
31. https://github.com/anytls/anytls-go — AnyTLS-go 参考实现。
32. https://github.com/shadowsocks/shadowsocks-rust — shadowsocks-rust 仓库，v1.25.0 @ 2026-08-26（查于 2026-09-11）。
33. https://github.com/cfal/shoes — cfal/shoes 仓库，v0.2.7 @ 2026-01-22，最后推送 2026-08-09（查于 2026-09-11）。
34. https://github.com/eycorsican/leaf — leaf 仓库，v0.14.2 @ 2026-02-25（查于 2026-09-11）。
35. https://github.com/Watfaq/clash-rs — clash-rs 仓库，v0.10.8 @ 2026-07-23（查于 2026-09-11）。
36. https://github.com/tuic-protocol/tuic — tuic-protocol 组织仓库，最后推送 2025-05-15（查于 2026-09-11）。
37. https://github.com/Itsusinn/tuic — TUIC "继任者" fork，最后推送 2026-09-09（查于 2026-09-11）。
38. https://github.com/Nehereus/rusteria — Rust Hysteria2 业余实现，最后推送 2025-04-03，无许可证。
39. https://github.com/p4gefau1t/trojan-r — trojan-r 仓库，最后推送 2021-07-10。
40. https://github.com/cty123/TrojanRust — TrojanRust 仓库，最后推送 2023-04-10。
41. https://github.com/trojan-rust/trojan-rust — trojan-rust 组织仓库，最后推送 2026-08-12，3 星，低信号。
42. https://github.com/cloudflare/boringtun — Cloudflare boringtun（Rust WireGuard），最后推送 2026-06-29。
43. https://github.com/quinn-rs/quinn — quinn QUIC 库，最后推送 2026-09-10。
44. https://github.com/aws/s2n-quic — AWS s2n-quic，最后推送 2026-09-10。
45. https://github.com/rustls/rustls — rustls 仓库，最后推送 2026-09-09。
46. https://github.com/3andne/craftls — craftls（类 uTLS ClientHello 拟态），最后推送 2024-01-19，27 星。
47. https://github.com/tun2proxy/tun2proxy — tun2proxy（跨平台 TUN-to-SOCKS5/HTTP），最后推送 2026-06-17。
48. https://github.com/SagerNet/sing-box/issues/589 — sing-box vs Xray-core 吞吐对比（Go-vs-Go，非 Rust 相关但常被误引），2023-05，closed not planned。
49. https://github.com/wyx2685/V2bX — V2bX 面板节点 agent（Go）。
50. https://aws.amazon.com/blogs/security/introducing-s2n-quic-open-source-protocol-rust — AWS s2n-quic 生产环境公告。2022-02-01。
51. https://crates.io/crates/hysteria2 — crates.io 上的 Hysteria2 客户端 crate（非服务端）。查于 2026-09-11。
52. https://www.chonglangbiji.com/security/2026-04-transit-server-crackdown-postmortem/ — 中文机场/运维社区博客，2026-04 大陆中转打击复盘（未经一手源核实）。2026-04。
53. https://clash-jichang.com/airport/iepl-iplc — 机场对比博客，IEPL/IPLC 说明。2026。
54. https://v2.hysteria.network/zh/docs/advanced/Full-Client-Config/ — Hysteria2 官方文档，端口跳跃/mport 参数说明。2025-2026。
55. https://sing-box.sagernet.org/manual/proxy-protocol/hysteria2/ — sing-box 官方 Hysteria2 协议文档。
56. https://docs.v2board.com/use/node — V2board 官方文档，节点/Agent 架构说明。2025-2026。
57. https://www.jtti.cc/supports/4050.html ；https://www.jtti.cc/supports/3317.html ；https://www.tjsky.net/tutorial/633 — 机房测速类博客，CN2 GIA/BGP RTT 数据（anecdotal）。2025-2026（jtti），聚合自 2019-2024（tjsky）。
58. https://www.chonglangbiji.com/protocols/proxy-protocol-comparison-vless-reality-hysteria2-tuic-may-2026/ — 中文社区协议实测对比（REALITY/Hysteria2/TUIC）。2026-05-26。
59. https://xtls.github.io/en/config/outbounds/vless.html — Xray 官方文档，VLESS/REALITY splice 机制说明。
60. https://v2.hysteria.network/docs/developers/Protocol/ — Hysteria2 官方协议文档。
61. https://gist.github.com/tobyxdd/0993ac063b2eee94f7d36ddd786f52ce — Hysteria2 作者关于 Brutal 拥塞控制公平性问题的说明。
62. https://web.cs.wpi.edu/~claypool/papers/bbr/bbr-aict-19.pdf — BBR vs CUBIC 丢包敏感度学术论文。2019。
63. https://blog.apnic.net/2020/01/10/when-to-use-and-not-use-bbr/ — APNIC 博客，BBR 适用场景分析。2020。
64. https://www.fastly.com/blog/measuring-quic-vs-tcp-computational-efficiency — Fastly，QUIC vs TCP 计算效率受控测量。2020-04-30。
65. https://github.com/SagerNet/sing-box/issues/3382 — sing-box gVisor TUN 栈 CPU 异常 issue。2025-09-10。
66. https://pkolaczk.github.io/memory-consumption-of-async/ — Go goroutine vs Rust tokio 异步任务内存占用受控基准。2023-05-21。
67. https://github.com/XTLS/Xray-core/discussions/3339 ；https://github.com/XTLS/Xray-core/issues/4054 ；https://github.com/XTLS/Xray-core/issues/5344 — Xray-core 内存问题报告合集。
68. https://github.com/golang/go/issues/14812 ；https://github.com/golang/go/issues/18534 — Go GC 延迟历史 issue（约 2016 年，背景参考）。
69. https://openbenchmarking.org/test/pts/network-loopback — TCP echo loopback RTT 基准测试。
70. https://api.openalex.org/works/doi:10.1145%2F3749216 — io_uring vs epoll 网络服务性能研究（VLDB 方向）。2026。
71. https://github.com/2dust/v2rayN/releases — v2rayN 发布记录，7.25.1 @ 2026-09-10。
72. https://github.com/2dust/v2rayNG/releases — v2rayNG 发布记录，2.3.8 @ 2026-09-10。
73. https://github.com/clash-verge-rev/clash-verge-rev/releases — Clash Verge Rev 发布记录，v2.5.2 @ 2026-07-19。
74. https://github.com/MetaCubeX/mihomo/issues/2998 — mihomo XHTTP 缺陷报告（MLKEM768/重定向/GET 上行）。2026-08-01。
75. https://github.com/mihomo-party-org/clash-party/issues/576 — Clash Party，XHTTP 协议支持请求。
76. https://github.com/MatsuriDayo/NekoBoxForAndroid/releases — NekoBox for Android 发布记录，最后 1.4.2 @ 2026-02-09。
77. https://github.com/hiddify/hiddify-app/releases — Hiddify 发布记录，v4.1.1 @ 2026-03-05。
78. https://sing-box.sagernet.org/clients/apple/ — sing-box 苹果客户端页面，App Store 分发状态说明。2026。
79. https://stash.wiki/en/release-notes/ios — Stash iOS 发布说明，2026-03-31 TUIC 兼容修复。
80. https://github.com/fscarmen/sing-box — 一键安装脚本，客户端目标列表参考（Shadowrocket 等）。
81. https://www.chonglangbiji.com/howto/subscription-convert/ — 中文社区博客，订阅格式碎片化与转换工具说明。2026。
82. `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5/CLAUDE.md` — b-ui 仓库内部项目文档（架构、约定、已知技术债）。查于 2026-09-11。
83. `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5/web/server.js` — b-ui 仓库源码，订阅生成器重复逻辑位置确认。查于 2026-09-11。
84. `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5/server/core.sh` — b-ui 仓库源码，确认 vless-direct/vless-residential 均为 `"network": "tcp"`（非 XHTTP）。查于 2026-09-11。
85. https://github.com/cfal/shoes/pull/133 — shoes REALITY 与 Xray-core 三处 TLS 握手兼容性修复。2026-05-26。
86. https://github.com/cfal/shoes/pull/105 — shoes REALITY short_id 零填充方向修复。2026-02-08。
87. https://github.com/cfal/shoes/issues/97 — shoes vs sing-box 吞吐对比报告（对接真实 v2rayN），未解决关闭。2026-01-21。
88. https://github.com/cfal/shoes/blob/master/CONFIG.md — shoes 配置文档，确认 Hysteria2 配置无 obfs/Salamander/masquerade 字段。2026-08-09。
89. https://github.com/cfal/shoes/pull/54 — shoes 端口范围绑定实现（Hysteria2/QUIC/TUIC/TCP）。2025-03-06。
90. https://github.com/cfal/shoes/pull/118 ；https://github.com/cfal/shoes/pull/120 ；https://github.com/cfal/shoes/pull/122 — shoes 客户端端口跳跃实现（仅测过"作为客户端"）。合并于 2026-03-26。
91. https://github.com/cfal/shoes/issues/96 — shoes IPv6 双栈监听与 BBR 拥塞控制问题报告。2026-01-21。
92. https://github.com/cfal/shoes/issues/47 — 维护者明确回复"从未对比测试过 sing-box/Xray"。
93. https://github.com/cfal/shoes/issues/90 ；https://github.com/cfal/shoes/pull/153 — shoes AnyTLS 会话内存持续增长问题及修复 PR（仍开放）。
94. https://github.com/cfal/shoes/issues/128 — shoes "多用户设置"功能请求，0 回复，开放中。
95. https://github.com/cfal/shoes/issues/154 — shoes "动态鉴权与流量统计钩子"功能请求，0 回复，开放中。
96. https://github.com/shadowsocks/shadowsocks-rust/issues/608 ；https://github.com/shadowsocks/shadowsocks-rust/issues/939 ；https://github.com/shadowsocks/shadowsocks-rust/issues/1679 — shadowsocks-rust 零散内存问题报告（路由器/小设备场景，非规模化负载数据）。
97. ssh://bwg-rick, ssh://bwg-tizi — b-ui 生产服务器实测（ps/free 输出），进程 RSS 与主机内存/CPU 画像。2026-09-11。
98. https://github.com/XTLS/Xray-core/issues/6684 — Xray-core VLESS+XHTTP+REALITY 内存/socket 泄漏报告，含纯 TCP+REALITY 复现评论，closed not planned。2026-08-26 至 2026-08-30。
99. https://github.com/XTLS/Xray-core/issues/5828 — Xray-core VLESS+REALITY socket 泄漏报告（已定位根因并修复）。2026-03-20 至 2026-03-22。
100. https://github.com/XTLS/Xray-core/commit/2320416ca3869d7818b9d86b749259a75fd3e103 — 对应修复提交。
101. https://github.com/XTLS/REALITY/pull/25 — REALITY 上游依赖修复 PR（#5828 根因）。2025-10。
102. https://gfw.report/publications/foci26b/en/ — gfw.report，"Geedge Cases"，FOCI'26（未提及 REALITY/VLESS/Xray-core）。2026-02-19。
103. https://gfw.report/blog/gfw_unconditional_rst_20250820/en/ — gfw.report 官方博客，2025-08-20 整端口 443 RST 事件分析。2025-08-20。
104. https://github.com/XTLS/Xray-core/issues/6421 — 中国大陆 VLESS+REALITY+XHTTP :443 握手失败个案报告，疑似 IP 被盗用而非协议指纹，未解决。2026-07-02。
105. https://github.com/XTLS/Xray-core/issues/6091 — "REALITY 协议已被完全攻破"未证实声明，被社区判定为无 PoC，已关闭。2026-05-08，closed 2026-05-12。
106. https://github.com/XTLS/BBS/issues/15 — REALITY 理论化 SNI 白名单封锁设想，作者自称"仅是猜想"。2026-01-31。
107. https://github.com/net4people/bbs/issues?q=REALITY — net4people/bbs 站内 REALITY 关键词搜索结果，2025-2026 窗口内无大陆专属确证报告（仅 Iran 相关）。查于 2026-09-11。
