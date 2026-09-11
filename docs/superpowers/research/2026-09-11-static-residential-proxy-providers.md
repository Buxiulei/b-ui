# 静态住宅/ISP 代理供应商调研 — Bright Data 替代方案

调研日期：2026-09-11 | 调研范围：仅公开资料研究，未注册、未付款、未提交任何表单

---

## 1. 结论先行

### 排名前 5（相对 Bright Data 的替代候选）

1. **Decodo（原 Smartproxy）** — 全场唯一在官方 FAQ 明确写出"Static Residential (ISP) Proxies 上 Google 可访问"的供应商 [11]；Proxyway 独立实验室测得其 ISP 池的欺诈分数（32.72）是被测供应商中最低、"被滥用最少"的 [16]，且是唯一被独立验证支持 SOCKS5-over-UDP 的供应商 [16]。价格中等（$0.27–3.33/IP）。支付：确认支持支付宝，但 ISP/静态住宅按 IP 计费产品**不能直接**用支付宝结算，必须先用支付宝充值到钱包再消费代币 [14]——这是需要向卖家确认的摩擦点。**最大风险**：其官方限制清单（银行/政府/流媒体/App Store/邮件等）是否与 Static Residential(ISP) 产品共用同一份文档尚未证实，需直接询问客服 [12]。

2. **Rayobyte** — 独立测试中 ASN 与所宣称运营商（Comcast、Verizon 等）匹配率高达 88%，是本次调研中所有被独立验证过的供应商里最高的，100% 被数据库识别为住宅 IP [37]；AUP 仅有明确的邮件端口封锁（25/110/465/587/993/995/143），未发现 Google/搜索引擎限制 [36]。**最大风险**：SOCKS5 仅支持 TCP、不支持 UDP [37]；官方仅确认信用卡与 PayPal，未发现支付宝/USDT 支持，需直接核实付款渠道是否适配中国业主。

3. **MarsProxies** — 官方宣称"零带宽限制、零并发会话限制"，协议覆盖 HTTP/HTTPS/SOCKS5/TCP/UDP，且是本轮调研中支付方式与业务模型契合度最高的一家（信用卡+PayPal+支付宝+USDT via CoinGate）[42][43]，价格也具竞争力（约 $1.35–1.39/IP）。**最大风险**：品牌较新，本轮调研的搜索预算在深入核实其独立口碑（Trustpilot/Reddit/投诉记录）前耗尽，IP 来源与目标限制均未经第三方验证——上线前必须先做小额试用。

4. **Oxylabs** — 行业头部企业级供应商，第三方测评中欺诈分数排名靠前，支持 ASN/城市/邮编级选址，KYC 门槛本身是合规成熟度的信号 [26]。**最大风险有两层**：(a) 官方文档承认"为防滥用默认限制部分网站"，且访问需通过一次性人工解锁，Google 是否默认放行未被证实 [25]；(b) 强制性 KYC "用例问卷"每年拒绝约 25% 的申请者 [26]，B-UI 把住宅静态 IP 转售给约 1000 名下游订阅用户、代理其任意流量的商业模式，是否会被 Oxylabs 风控团队判定为"代理转售/规避检测"而拒绝，官方文档没有给出可预期的答案——这是一个需要提前用邮件问清楚、而不是先充值再赌的实质性风险。

5. **Proxy-Seller** — 本轮调研中"支付宝 + USDT(TRC20/ERC20) + 信用卡"三者同时被官方直接确认的唯一供应商 [41]，价格是所有强候选中最低的一档（约 $0.75–0.98/IP），国家覆盖 220+ 含 JP/SG。**最大风险**：没有找到任何独立 ASN/欺诈分数审计；同时存在至少一条与官方宣传严重相反的独立用户评价（"最差体验之一"），信号相互矛盾，需要用最小额度试用亲自验证质量。

### 必须避开（3–5 家，附理由）

- **NetNut** — 2026-07-02 谷歌威胁情报小组（GTIG）联合 FBI、Lumen 公开处置了 NetNut 背后的"Popa"僵尸网络：至少 200 万台未经用户同意被植入 SDK 的智能电视/streaming box 被纳入其出口节点池，谷歌博客原文明确写出"许多流行的住宅代理品牌实际上是在白标 NetNut 的僵尸网络"[3]；KrebsOnSecurity 独立报道证实 FBI 已查封相关域名 [4]。这正是需求里明确要排除的"SDK/僵尸网络来源"，且事发仅两个月前，网上大量旧评价已完全失效。**必须排除**，并对任何"我们与 NetNut 无关"的其它供应商声明保持怀疑。

- **IPIDEA 家族（922 S5 Proxy、LunaProxy、PIA S5 Proxy、360 Proxy、ABC Proxy 部分佐证、Galleon VPN、Radish VPN、旧版 PyProxy 等约 19 个香港注册的转售品牌）** — 谷歌 GTIG 于 2026-01-28/29 处置了该网络：600+ 木马化安卓 App（Packet/Castar/Hex/Earn SDK）和 3000+ 伪装成 OneDrive Sync/Windows Update 的木马 Windows 程序，在用户不知情下把设备变成代理出口节点，支撑了 Aisuru、Kimwolf、BADBOX2.0 等 DDoS 僵尸网络，一周内被 550+ 个中/朝/伊/俄背景的威胁组织使用 [5][6][7]。**owner 已经试用过的 PIA S5 Proxy 正是该家族成员之一**，这很可能直接解释了此前遇到的限制问题本质并非"策略限制"而是"来源黑历史"。**全家族必须排除**。

- **CliProxy（owner 已试用）** — 2025-06-13 一篇独立中文测评（katorly.com）用主流 IP 数据库测该供应商的"静态 IP"，多数库误判为住宅 IP，但 IPQualityScore（被该测评称为业内识别代理的权威工具）给出**欺诈风险分 100 分**并标记为已知代理，WHOIS/ASN 解析结果是 `FiberPower LLC`（AS214483）——一个数据中心/主机托管 ASN，不是消费级 ISP [10]；同一测试中 Meta AI 在该 IP 上无法免登录访问，暗示已被部分 AI 服务标记。这正是需求明确要避免的"数据中心 IP 贴牌成住宅 IP"的实锤案例，很可能是 owner 之前用 CliProxy 仍遇限制的根本原因（数据中心 ASN 天然更容易被风控），而非 Bright Data 式的策略性拦截。**建议排除**，除非 owner 愿意在新一轮购买前先用 ippure/IPQS 重新抽测确认供应商是否已更换 IP 池。

- **IPRoyal — 降级为"避免优先购买大额，仅可小额试测"** — 虽然官方 AUP/支付页面看起来是本轮最契合中国支付需求的供应商之一（支付宝+微信支付+USDT 均官方确认）[18][20][21]，但 Proxyway 独立实验室测试发现其 ISP 代理样本中**仅 27% 的 IP 的 ASN+组织信息与宣传一致**（Rayobyte 同类测试为 88%），且 4/10 抽样 IP 的实际归属地并不在宣传的美国境内；更严重的是 Proxyway 记录 IPRoyal 曾持有"超百万个 Comcast 相关 ASN 段 IP，Comcast 怀疑其未经授权获取"，测试期间 Comcast 已回收了其中大部分 [24]。这是本轮调研里唯一有**第三方实测数据**（而非广告文案）支持的"来源不实"红旗，权重应高于其漂亮的支付页面。

- **Webshare（静态住宅款）** — 多篇独立论坛/Reddit 用户报告用 IP 滥用数据库检测 Webshare 的静态住宅 IP 段，"几乎全部"被标记为 SERVER/PROXY/VPN [34]；Trustpilot 上有多条 1 星评价称代理"不断被封"、国家与宣传不符 [35]。其官方"受限网站"文档明确只覆盖"Rotating Residential 网络"，未确认是否适用于 Static Residential 产品线 [33]——即便价格是全场最低（$0.225–0.30/IP），廉价很可能正是"数据中心 IP 被打上滥用标签后降价出清"的信号。

其余未列入优先候选、但也不建议在无进一步验证前使用的：**ipipgo、Kookeey（第三方指出 IP 重复率偏高）、ABCproxy（二手中文资料称其与 IPIDEA 同属"第三方聚合网络/预装 SDK 终端设备"模式）、Nstproxy（自称"P2P private residential"，与 IPIDEA/NetNut 同一来源模式，consent 状态未知）、PYPROXY（官网自称 2026 年 8 月被"关停并由新团队接盘"，旧账户余额不继承）**，以及所有域内（大陆）IP 代理商（神龙HTTP代理/穿云/小象代理等）——这些提供的是境内 IP，与"让大陆用户呈现境外住宅 IP"的用途方向相反 [57]。

---

## 2. 评估标准（打分口径）

依据 B-UI 的硬性要求，把评估拆成 6 个维度，按重要性加权（总分 100，5 分制 ×权重折算）：

| 维度 | 权重 | 打分要点 |
|---|---|---|
| **真实 ISP/静态证据** | 30% | 有无第三方 ASN 匹配测试（Proxyway 等）> 官方"来自 ISP"文案 > 无任何证据；出现"P2P/SDK/僵尸网络"关联证据直接 0 分并一票否决 |
| **目标限制（Google 必须放行）** | 25% | 官方文档明确写出 Google 可访问 > 官方未提及限制（沉默不等于允许）> 官方文档明确列出限制但可解锁 > 明确封锁不可解锁 |
| **协议与 UDP** | 15% | SOCKS5 + HTTP CONNECT 都支持 > 只支持一种；有独立验证的 SOCKS5-over-UDP 加分（注：见第 7 节，b-ui 当前 relay 架构暂不依赖 UDP，此项权重可下调） |
| **价格与带宽** | 15% | 明确"无上限"且第三方未发现隐藏封顶 > 官方宣传无限但存在隐藏 GB/IP 封顶（如 IPRoyal 100GB、Rayobyte 200GB）> 按流量计费不确定成本（如 SOAX、NetNut） |
| **支付（对中国业主友好）** | 10% | 官方明确支持支付宝/微信/USDT 且结算路径无摩擦 > 支持但有摩擦（需先充值钱包）> 未确认 > 明确不支持 |
| **口碑与独立验证** | 5% | 有 Proxyway/第三方实验室实测 > 有第三方评测网站综述 > 仅自媒体/联盟营销文章 > 无独立信息 |

**一票否决项**（不进入加权计算，直接列入"避开"）：被谷歌/FBI/执法机构公开认定为僵尸网络或遭查封（NetNut、IPIDEA 全家族）；独立技术测试证实 ASN 为数据中心而非消费 ISP（CliProxy）。

综合评分换算：≥7 分＝强（strong）；4–6.9 分＝可能（possible）；<4 分＝弱（weak）；证据不足以打分＝未知（unknown）。第 3 节表格中的"综合评分"按此口径给出，均为**基于现有公开证据的定性估计，非供应商官方数据**。

---

## 3. 供应商总表（全部已调研供应商）

| 名称 | 产品 | IP 类型证据 | Google/搜索放行 | 其他限制 | 端口/UDP | 协议/认证 | 价格 | 国家 | 支付 | 口碑 | 综合评分 | 置信度 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Bright Data（基线） | ISP/静态住宅 | 官方自称 ISP 分配，未独立复核 | **明确封锁**（policy_20110）[1] | 支付/TikTok 需特批（policy_20050）；端口白名单拒绝 5228/5223（policy_20020/21/240）[1] | 部分端口白名单外拒绝；HTTPS 限 443 | HTTP(S)/SOCKS5，user:pass | $1.30–1.80/IP | 全球 | 卡/PayPal/汇款 | 行业龙头，但正是问题源头 | 3.5/10 弱 | documented |
| **Decodo（原 Smartproxy）** | Static Residential(ISP) | Proxyway 实测欺诈分 32.72（最低）[16] | **官方 FAQ 明确放行** [11] | 银行/政府/流媒体/App Store/邮件/电信默认限制，部分可 KYC 解锁 [12] | 独立验证支持 SOCKS5+UDP [16] | HTTP(S)/SOCKS5 | $0.27–3.33/IP | 16+国含US/JP/SG/HK [13] | 支付宝(需充值钱包，ISP档不支持) [14] | 老牌口碑好，Proxyway 测评优 | 7.5/10 强 | measured/documented |
| **Rayobyte** | Static ISP Proxies | Proxyway 实测 ASN 匹配 88%（最高）[37] | 未发现明确限制 | 仅邮件端口封锁 [36] | **不支持 UDP** [37] | HTTP(S)/SOCKS5(TCP only) | $3–5/IP | 6国2大洲(较窄) | 卡/PayPal，未确认支付宝/USDT | 老牌，评价正面 | 6.5/10 可能 | measured |
| **MarsProxies** | ISP Proxies | 官方宣称"零限制"，无独立审计 | 未找到限制文档 | 未确认 | HTTP/HTTPS/SOCKS5/TCP/UDP | $1.35–1.39/IP | 40+国 | 卡/PayPal/支付宝/USDT [43] | 新品牌，独立口碑未核实（搜索预算耗尽） | 6/10 可能 | documented |
| **Oxylabs** | Dedicated/ISP Proxies | 官方称来自 Comcast/BT/Orange 等知名 ASN | 未明确，默认限制部分站点需解锁 [25] | 官方承认默认限制部分网站防滥用 | 并发 50GB/IP 后限速至10/proxy | HTTP(S)/SOCKS5(需邮件申请开启) [28] | $1.20–3.20/IP | 25国 | Stripe卡确认，支付宝/USDT未确认 | 头部企业品牌，**强制KYC~25%拒绝率**[26] | 6/10 可能 | documented |
| **Proxy-Seller** | ISP Proxies | 官方称来自 AT&T/Windstream/Frontier | 未确认 | 未确认（页面未详列） | HTTP(S)/SOCKS5 | $0.75–0.98/IP起 | 22国含JP | 卡/PayPal/**支付宝+USDT**均确认 [41] | 混合：多数正面+1条强负面 | 6/10 可能 | documented/anecdotal |
| **Infatica** | ISP Proxies | 官方"来自本地ISP，托管在数据中心" | 自营 SERP 抓取产品暗示不封 Google | 未确认 | HTTP确认，SOCKS5列出未独家验证 | $1.95–3.00/IP | 覆盖US/JP/SG/HK/TW等APAC | 未确认支付宝/微信/USDT | 定位"IPIDEA合规替代品"，ISO认证自称 [45] | 5.5/10 可能 | documented |
| Proxy-Cheap | Static Residential | 官方称"直接来自ISP，非机房ISP代理" | 未找到限制文档 | 未确认 | HTTP/SOCKS5 | $1.99周–$2.71月 | 24+国 | 卡/PayPal/支付宝/USDT(CoinGate) | 中端预算品牌，评价尚可 | 5.5/10 可能 | documented |
| Ping Proxies/Byteful | Static Residential ISP | 官方详列具体ASN(AT&T/Spectrum/Comcast/BT/德电) | 未确认 | ToS无目标限制条款，仅通用滥用禁止 [64] | HTTP(S)/SOCKS5/UDP | 约$2.25/IP起 | 15+国 | 未确认 | Proxyway实测ISP成功率100% | 5/10 可能 | documented |
| SOAX | ISP Proxies | 官方称"clean residential"，无独立ASN审计 | 未明确列出，KYC后可申请解锁 [29] | **默认仅放行80/443端口**，需KYC+人工申请解锁其它端口/域名 [29][30] | 端口默认受限；UDP未确认 | HTTP/HTTPS/SOCKS5 | 按GB计费($1.5–3/GB) | 多国但静态款疑似仅US | 未确认 | 中端，SMTP外限制未见独立报告 | 4/10 弱 | documented |
| IPRoyal | ISP/Static Residential | **Proxyway实测仅27%ASN匹配，曾持有疑似未授权Comcast IP段被回收**[24] | AUP未明列限制 [18] | 默认限银行/政府/邮件端口，可KYC解锁 | HTTP(S)/SOCKS5 | $1.80–2.70/IP | 31+国 | 卡/PayPal/**支付宝+微信+USDT均确认**[20][21][22] | 支付最友好但实测来源不实 | 4.5/10 弱 | measured |
| NodeMaven | Static Residential | 官方称"ISP分配，Scamalytics质检" | 未确认 | **明确禁止访问银行/支付服务/政府/教育/新闻网站**[59]，与"payment必须能过"部分冲突 | HTTP(S)/SOCKS5/UDP | $2.99/IP | 仅7-9国(BR/FR/DE/HK/IT/PL/RO/UK/US) | 未确认 | 新品牌，客服口碑好，静态IP国家随机分配被投诉 | 4.5/10 弱 | documented/anecdotal |
| Webshare | Static Residential | **多篇独立报告：抽测IP被滥用库标记为SERVER/PROXY/VPN**[34] | 受限清单仅确认覆盖Rotating网络，Static款未知 [33] | Netflix/银行/政府/流媒体/票务/邮件默认限制(仅Rotating确认) | HTTP/SOCKS5 | $0.225–0.30/IP | ~15国 | 卡/GPay/ApplePay，无支付宝/USDT/微信 | Trustpilot多条1星，"不断被封" [35] | 3.5/10 弱 | anecdotal |
| Proxy302（代理IP自助超市，中文平台） | 静态住宅IP | 官方称来自电信运营商注册路由器/PC，聚合型平台，未独立验证 | 未确认 | 未确认 | HTTP/SOCKS5 | 按需计费(代币制) | 240+国(聚合池) | 支付宝/信用卡/**USDT** | 中文市场自助平台，评价偏营销向 | 3.5/10 弱 | anecdotal |
| ipipgo | 静态住宅代理 | 完全自述，无第三方独立报道 | 未确认 | 未确认 | HTTP(S)/SOCKS5 | 未确认 | 220+国(自称) | 未确认(推测支付宝/微信) | **几乎零独立第三方评价**，本身即是信号 | 3/10 弱 | anecdotal |
| IPFoxy | Dedicated Residential | 官方"real ISP proxy"，无独立验证 | 官方**主动列出ChatGPT/Gemini/Claude/TikTok为"支持场景"**(自我宣传，未验证) | 未确认 | HTTP/SOCKS5 | $7.03/月起 | 25+国(专属产品) | 未确认 | 中文比较文章指出带宽偏低(2-5M) | 4/10 弱 | documented(自述) |
| Nstproxy(nstdata.io) | Static Residential ISP / IPv4/IPv6 | **自称"P2P private residential"**，consent模式未知，与IPIDEA/NetNut同类来源模式 | 未确认 | 未确认 | 未确认 | 未确认 | 未确认 | 未确认 | 独立报道极少 | 2.5/10 弱 | unknown |
| Kookeey | 静态ISP代理 | 官方称"真人家庭住宅IP" | 未确认 | 仅限境外网络环境使用；无Google/AI相关条款 | 未确认 | 未确认 | 未确认 | 未逐项列出 | 未确认 | **第三方比较文章指出IP重复率偏高**[53] | 2/10 弱 | anecdotal |
| ABCproxy | 静态住宅IP/ISP | 混合信号：自称200M+真实住宅IP，但**二手中文资料称其与IPIDEA同属"第三方聚合/预装SDK设备"模式**[56] | 未确认 | 未确认 | SOCKS5/HTTP(S) | $0.04/IP起(疑似促销价) | 190+国(自称) | 未确认(推测支付宝友好) | 中文代理测评圈常见推广对象，独立信号少 | 2/10 弱 | anecdotal |
| PYPROXY | 静态住宅/ISP/Mobile | **官网自述2026年8月原服务已"关停"，由陌生"PyProxyBack团队"接盘，旧账户余额不继承**[8][9] | 未确认 | 未确认 | 未确认 | 按GB计费(~$1.2/GB) | 190+国(自称) | 未确认 | **与IPIDEA家族(PIA S5)共享底层网络**的自述佐证 | 1/10 弱 | anecdotal |
| CliProxy（owner已试用） | 静态ISP/住宅 | **独立测试：IPQualityScore欺诈分100，ASN解析为数据中心FiberPower LLC而非消费ISP**[10] | Meta AI需登录才可用（同测试中） | 未确认 | HTTP(S)/SOCKS5 | $3.80/IP起 | 100+城市(北美/欧洲/东南亚为主) | 未确认(中文站，推测支付宝友好) | 中文自媒体推广多、独立实测负面 | 1.5/10 弱 | measured |
| IPIDEA 家族（922 S5/LunaProxy/PIA S5/360 Proxy/Galleon VPN/Radish VPN等约19品牌） | Residential/ISP/Static Residential | **谷歌GTIG确认为僵尸网络：600+木马App+3000+木马程序，未经同意植入用户设备**[5][6][7] | 不适用(来源已一票否决) | 不适用 | 不适用 | SOCKS5/HTTP | 历史极低($0.04-0.045/IP) | 历史190+国(自称) | 历史支付宝/PayPal/卡 | **谷歌/FBI级处置，2026-01已被拆解** | 0/10 一票否决 | documented |
| NetNut | Static Residential/ISP | **2026-07-02谷歌+FBI公开处置，绑定"Popa"僵尸网络，200万+受害设备**[3][4] | 不适用 | 不适用 | 历史HTTP(S)/SOCKS5 | 历史$1.59-13.13/GB | 历史195国 | 未验证(非面向中国市场) | **上市公司Alarum Technologies旗下，已确认配合执法调查** | 0/10 一票否决 | documented |
| DataImpulse | Residential(仅轮换) | 官方称"自有带宽共享App，opt-in补偿"，属consent式SDK来源 | 未确认 | 未确认 | 未确认 | $1/GB(轮换池，非静态) | 195国(轮换池) | 未确认 | **无确认的静态/ISP产品线**(专属URL 404) | 2/10 弱 | unknown |
| AnyIP | Residential/Mobile(轮换粘性) | 未确认为ISP-ASN来源 | 未确认 | 未确认 | 未确认 | $0.88–2.70/GB | 未详列 | 未确认 | 评价矛盾(一说偏贵一说"最佳之一") | 2/10 弱 | anecdotal |
| HydraProxy | Static ISP Proxies | 仅第三方评测站描述，官方页未独立核实 | 未确认 | 未确认 | HTTPS确认，SOCKS5/UDP未确认 | $3.60–4/IP | 100+国(自称) | 未确认 | **混合评价：服务器不可用、退款差评报告** | 2/10 弱 | anecdotal |
| Proxy-Store | 未识别 | 数据严重不足 | 未确认 | 未确认 | 未确认 | 未确认 | 未确认 | 疑似支持USDT(Utrust网关，未验证) | 独立覆盖极少 | 1/10 未知 | unknown |
| 神龙HTTP代理/穿云/小象代理等境内代理 | 国内动态/静态IP | 境内IP池，与出海用途方向相反 | 不适用(错误地理) | 不适用 | 不适用 | 不适用 | 不适用 | 仅中国大陆 | 支付宝/微信(境内常规) | 与本需求场景不符，仅供参考排除 | 0/10 不适用 | documented |

---

## 4. 限制条款对照（vs. Bright Data 基线）

Bright Data 基线（b-ui 生产日志实测 + 官方错误码目录 [1][2]）：

- `policy_20110`：部分 zone 类型对搜索引擎/SERP 域名（google.com、bing.com 等）直接拦截 —— 对应 owner 观察到的 Google 403。
- `policy_20050/20051/20052`："需特殊权限"，明确涵盖政府网站、支付类目、TikTok 等，需 KYC 解锁。
- `policy_20010`：HTTPS 仅允许走 443。
- `policy_20020/20021/20240`：目标端口不在白名单内直接拒绝——对应 FCM `5228`、APNs `5223` 的 403。
- AUP 另行禁止"仿真点击/刷量""crypto/NFT 交易""流媒体相关域名"，并可自由裁量封锁成人内容、政府网站等 [2]。

逐一对照候选供应商官方条款原文：

**Decodo（原 Smartproxy）** — FAQ 原句："Google targets are accessible ... with Web Scraping API, Site Unblocker, Datacenter and Static Residential (ISP)"（fetched 2026-09-11）[11]。同时其受限清单文档写明默认限制：银行/金融+加密货币融资、政府、票务、游戏、邮件、商业、电信（fetched 2026-09-11）[12]；邮件/流媒体/商业类目仅在 Residential(轮换) 产品上可通过身份验证解锁，Static/ISP 层是否共用该文档未证实——**这是 Decodo 排第一但仍需向客服直接确认的关键缺口**。

**IPRoyal** — AUP 原文（fetched 2026-09-11）明确的是"数据中心与静态住宅代理默认封锁 .gov 政府网站与银行/金融机构，以及 IMAP 邮件端口 110/993/995/143，均可通过身份验证解锁"[18]；**未发现**任何关于 Google/搜索/TikTok/支付处理商的条款——沉默不等于允许，只是没有像 Bright Data 那样明写。

**Oxylabs** — 官方限制目标文档（fetched 2026-09-11）列出默认限制类目"不限于"：流媒体（Netflix/Spotify/Twitch/Disney）、银行金融（PayPal/美国银行/Binance）、政府（.gov/.ca）、游戏（PlayStation/Steam）、票务（Ticketmaster/Eventbrite）、邮件（Outlook/Yahoo Mail），可联系客服解锁 [25]。**未列出 Google/搜索、TikTok、AI 服务**——同样是沉默而非确认放行。KYC 页原文："If a use case is deemed inappropriate, suspicious, or unethical, Oxylabs may refuse to render any services"，"Each year at least quarter of all customer enquires are rejected"（fetched 2026-09-11）[26]。

**Webshare** — 受限网站文档明确写"适用范围：Rotating Residential proxy network"，默认封锁 Netflix/PlayStation/银行/政府/流媒体/票务/邮件，可 KYC 解锁 [33]；**该文档是否也适用于 owner 会购买的 Static Residential 产品线，官方文档未说明** —— 这是需要在下单前用工单书面确认的空白。

**Rayobyte** — 官方支持门户 FAQ 原文中唯一确认的限制是"端口 25/110/465/587 封锁以防垃圾邮件；代理不支持入站 UDP"[36]，其余仅泛泛要求"遵守 Rayobyte 政策与适用法律"，**未逐项列出目标类目**。

**SOAX** — 官方开发者 FAQ 原文（fetched 2026-09-11）："Some domains (financial services, government, certain news sites) and ports (including SMTP 25, 465, 587) are restricted by default." 明确的解锁路径：Settings→Profile→Verification（Sumsub 身份验证，约 5 分钟）后，在套餐 Settings→Guardrails→Access exceptions→Request exception 提交端口/域名解锁申请，仅组织 Owner 权限可提交 [29]。ToS 第 5.3 条独立印证该机制的存在："SOAX may implement technical guardrails, access controls, domain or port restrictions..."[30]。**FCM(5228)/APNs(5223) 未被文档列为受限示例**（唯一明确举例的是 SMTP），但文档也未给出完整端口清单，无法排除隐性限制。

**NodeMaven** — 术语表原文："prohibits opening bank, payment service, educational, government domains, mailing ports, and news websites"，**未找到任何解锁路径**——若 owner 的场景涉及支付类目通行需求，NodeMaven 的这条封锁本身没有豁免机制，风险高于其余候选 [59]。

综合看，**没有任何供应商的公开条款像 Bright Data 一样，用可枚举的 `policy_XXXXX` 错误码明确回答"Google/AI/支付处理商是否放行"**——只有 Decodo 一家在 FAQ 里正面写出 Google 可用；其余候选全部停留在"未提及＝可能允许，但没有书面保证"的状态，必须以第 8 节的验证计划实测确认。

---

## 5. IP 质量证据（"静态住宅"三种来源的辨析）

住宅/ISP 代理市场的 IP 来源，本质上分三类，质量与合规风险依次递增：

**(a) ISP 租赁型（IPRoyal/Decodo/Oxylabs/Webshare/Rayobyte/Proxy-Seller/NodeMaven/Ping Proxies/Thordata 宣称的模式）**：供应商与 Comcast、AT&T、Lumen 等电信运营商建立商业/合规合同，租下一段消费级 ISP 已分配、但实际托管在数据中心机房的 IP 地址段。IP 的 WHOIS/ASN 显示为消费 ISP，因此被 ipinfo/ipapi/ippure 等归类为"住宅/ISP"，但**物理上不经过真实家庭网络**——这是行业里"合法"静态住宅代理的标准做法，B-UI 需求里"preferably NOT SDK/botnet-sourced"实际上是在接受这一类。**关键是"租赁"是否真的取得授权**：Proxyway 对 Rayobyte 的实测显示 88% 样本 ASN 与宣传运营商吻合 [37]（目前调研中最高），对 Decodo 的实测显示约一半样本吻合、且欺诈分数最低（32.72，"被滥用最少的池"）[16]，而对 IPRoyal 的实测**仅 27% 吻合**，且记录了"IPRoyal 曾持有超百万个 Comcast 相关 ASN 的 IP，Comcast 怀疑未获授权，测试期间已回收大部分"的具体证据 [24]——这说明同属"ISP 租赁型"，实际授权合法性可以天差地别，广告词完全不能作为判断依据。

**(b) 数据中心贴牌型（CliProxy 被证实的模式）**：供应商把普通数据中心 IP 段包装成"静态住宅/ISP"出售。ASN 解析直接落在托管商（如 CliProxy 的 FiberPower LLC），IPQualityScore 等专业反代理工具能识别，但 ipinfo 等偏消费级数据库可能误判为住宅——这解释了为什么"多数数据库显示住宅"不能作为质量证据，必须交叉核对 IPQualityScore/Scamalytics/ASN WHOIS 至少两种工具 [10]。

**(c) SDK/僵尸网络型（NetNut 的"Popa"、IPIDEA 全家族被证实的模式；Massive/DataImpulse 官方承认的"consent 式"版本；Nstproxy 自称的"P2P private residential"）**：通过嵌入 App/固件的 SDK，把终端用户设备（智能电视、路由器、手机 App）未经真正知情同意地变成代理出口节点。NetNut/IPIDEA 已被证实为**非同意**版本（僵尸网络，一票否决）；Massive/DataImpulse 声称是**同意补偿**版本（用户主动装 App 换取报酬），合规性更高但仍属于"peer/SDK 来源"，与 B-UI 需求里"preferably NOT SDK-sourced"的偏好方向相反，且通常按流量而非固定 IP 计费，与 b-ui 的静态 selector 架构不匹配（见第 7 节）；Nstproxy 的"P2P private residential"是否为同意式，本轮调研未能确认（开放问题）。

**AI 服务与 Google 对这三类 IP 的态度**：没有任何供应商给出"api.openai.com/api.anthropic.com/generativelanguage.googleapis.com 是否放行"的官方书面声明（开放问题，见第 9 节）。可推断的间接信号：Massive 自我报告"ISP 产品线对 Google 100% 成功率、对 Amazon 98.03%"（自测，未独立验证）[本轮1号调研原文引用]；IPFoxy 官方营销明确把 ChatGPT/Gemini/Claude 列为"支持场景"（自我宣传，同样未独立验证）；Infatica 专门销售"Google/Bing/Yahoo SERP 抓取"产品，隐含其 ISP 池不被 Google 一刀切封锁的假设，但也非直接证据 [44]。**唯一有独立测试数据支撑的结论**是：ASN 匹配率越低（IPRoyal 27%）、或来源被证实为数据中心/僵尸网络的供应商，越可能被 Google/AI 服务的风控模型标记，因为这些系统正是靠检测 ASN 异常和已知代理特征工作的——所以"IP 类型证据"在本报告权重中被列为最高的一项（30%）并非偶然。

---

## 6. 价格与采购

### 各候选价格对比（按每 IP 每月价格换算，非官方统一口径，来自各自定价页面）

| 供应商 | 最低价/IP/月 | 计费模型 | 隐藏封顶（若有） | 备注 |
|---|---|---|---|---|
| Bright Data（基线） | $1.30–1.80 | 按IP+按GB两种 | 未见独立FUP报告 | 现用，问题是限制而非价格 |
| Decodo | $0.27–3.33 | 按IP（共享/独占两档差价大） | 未见明确封顶 | ISP档不接受加密货币支付 |
| Rayobyte | $3.00–5.00 | 按IP | **200GB/IP/月封顶（无超额费但达量后隐含限速/截断）**[36] | 价格偏高 |
| MarsProxies | $1.35–1.39 | 按IP | 官方宣称无封顶，未见反例 | 全场机制最灵活 |
| Oxylabs | $1.20–3.20 | 按IP，阶梯 | **50GB/IP后并发从100降到10** | 10 IP起订，最低$16/mo |
| Proxy-Seller | $0.75–0.98起 | 按IP | 未见反例 | 具体阶梯表未取得 |
| Infatica | $1.95–3.00 | 按IP | 未见反例 | 年付另有20%折扣 |
| Proxy-Cheap | ~$2.71/月 | 按IP，周/月同价 | 官方称无限流量 | — |
| Webshare | $0.225–0.30 | 按IP，20个起订 | 未见明确封顶 | 但IP质量本身有独立负面报告 |
| IPRoyal | $1.80–2.70 | 按IP | **100GB/IP/30天封顶，超额限速至~5%**（与其自家"无限流量"宣传页矛盾）[23] | 已在第1/5节降级 |
| SOAX | $1.5–3/GB | **按GB非按IP**（与b-ui固定静态IP模型不匹配） | 不适用 | 计费模型本身不合适 |

### B-UI 预估用量的成本测算

按 B-UI 需求（2–4 个 US 静态 IP + 1–2 个 JP/SG 静态 IP，共约 6 个 IP，供约 1000 名下游订阅用户共享出口，理论上"无限带宽"）：

- **Proxy-Seller**：约 $4.50–5.88/月（6×$0.75–0.98），若定价页信息准确，是全场最低；但缺独立验证，且起订量/国家阶梯细节未取得，需以官方在线客服核实。
- **MarsProxies**：约 $8.10–8.34/月（6×$1.35–1.39），机制最契合（无限带宽+无限并发），风险仅在口碑未核实。
- **Webshare**：官方阶梯从 20 个 IP 起订，即使只用 6 个，实际最低消费约 $6/月（20×$0.30）——价格好但第 5 节的独立负面信号需要权衡。
- **Decodo**：若走共享 IP 档（$4.7/10 IP≈$0.47/IP），约 $2.82/月；若走独占档（$10/3 IP≈$3.33/IP），约 $20/月——差距很大，需先确认哪档才是"专属静态"而非"共享出口"。
- **IPRoyal**：约 $14.40–16.20/月（6×$2.40–2.70）——但叠加第 5 节的来源不实证据，不建议作为长期首选。
- **Oxylabs**：最低起订 10 个 IP（$1.60/IP）＝ $16/月，即使只用 6 个也要按 10 个付费。
- **Rayobyte**：约 $18–30/月（6×$3.00–5.00起）——本轮最贵的强候选。

### 从中国结算的支付路径

- **官方同时确认支付宝＋USDT＋信用卡**：Proxy-Seller [41]、MarsProxies [43]、Proxy-Cheap；
- **官方确认支付宝＋微信支付＋USDT但有摩擦**：IPRoyal（ISP档需先充值代币钱包，微信支付未在ISP档确认可直接使用）[20][21][22]；
- **官方确认支付宝但仅限充值钱包、ISP档不可直接用**：Decodo（原 Smartproxy 中文站）[14]；
- **仅确认信用卡/PayPal/Stripe，支付宝/USDT 状态不明需直接核实**：Oxylabs（Stripe 结算，理论上 Stripe 支持支付宝但未见 Oxylabs 官方明确开启）、Rayobyte、Webshare（仅信用卡+Google Pay+Apple Pay，**未发现支付宝/USDT**，是本轮对中国业主支付最弱的强候选）。

### 转售条款的潜在冲突（重要，容易被忽略）

Webshare 的 ToS 明确禁止"未经书面同意复制、转售或再分发本服务"[32]；IPRoyal 的 AUP 同样禁止未经同意的商业性转售/利用 [18]。B-UI 的模式本质上是把共享的静态 IP 池转售给约 1000 名订阅用户、代理其任意流量——这与典型条款里"转售"针对的"直接倒卖代理账号"未必是一回事，但**在下单前应主动向候选供应商的销售/合规团队说明真实用途**，而不是假设条款只针对别的场景。这是第 9 节未决问题之一。

---

## 7. 接入 b-ui 的技术细节

b-ui 当前的接入约束（来自 `server/residential-helper.sh` 与 `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`，[47][48]）：

- `parse_url()` 只接受四种格式：`socks5://user:pass@host:port`、`http://user:pass@host:port`、`host:port:user:pass`、`user:pass@host:port`（自动先探测 SOCKS5 再探测 HTTP）。**任何供应商只要发的是"网关 host + user:pass"这种近乎行业通用的凭据形式，都可以零代码改动直接粘贴进去**。
- 架构是**硬静态 IP 设计**：每条配置的 URL 会变成一个独立的 sing-box outbound（resi-1、resi-2…），包在一个 `selector`（不是 `urltest`）里，只有 `resi-health.sh` 通过 Clash API 主动切换才会换出口 IP。**任何"轮换池"产品（IP 按请求/按会话超时自动变化）都会悄悄破坏这个模型**，也会打乱 auto-blacklist 按 `host:port` 的键控逻辑——只有明确标注"专属/静态"、且通常单独命名为 ISP/Static Residential（区分于同一供应商的"Residential/轮换"产品）的 SKU 才可用。
- relay 目前**已经假设住宅 SOCKS5 上游不支持 UDP ASSOCIATE**并做了兜底：DNS 走直连、QUIC(UDP/443) 直接拒绝迫使客户端回落到 TCP（TCP 仍走住宅出口）、其余 UDP 一律直连。**因此 UDP-over-SOCKS5 支持目前是"锦上添花"而非硬性阻塞项**，第 2 节评分权重可据此下调，除非未来改造 relay 主动利用 UDP。
- 因为 selector 让**所有用户、所有住宅模式流量同一时刻共用一个已选中的静态 IP**（而非按连接负载均衡到整个 IP 池），所选凭据必须能承受同一 IP 上潜在数百到数千的并发连接——这是本场景区别于典型"单客户端做爬虫"用途的核心差异，比 UDP 支持更值得优先核实（Webshare 官方文档明确到"500–3000 并发线程"分档，是本轮唯一给出具体并发数字的供应商；Oxylabs/IPRoyal/Rayobyte/Proxy-Seller 均未公开并发上限）。
- 2026-09-11 设计中的自动黑名单（`resi-blacklist.sh`）**只能识别"硬拒绝"信号**：curl 退出码 56/97（CONNECT/SOCKS5 握手被拒）或 35（CONNECT 成功后 TLS 被杀），并与直连同目标的 curl 退出码 0 做交叉验证。**如果某供应商是"软封锁"**——接受 CONNECT/SOCKS5 握手后返回 HTTP 200 的拦截页/验证码页面，或直接静默超时而非主动 RST——**这套机制完全捕捉不到**，需要运维人工把该目标手动钉死为直连。目前只有 Bright Data 的硬拒绝行为（CONNECT 403 + policy_XXXXX）被确认与该机制兼容；任何新候选上线前都要先确认其拒绝方式。

### 逐候选接入形态（据公开资料推断，未经实测确认，标注 host:port 仅为占位示意）

**Decodo** — 静态住宅(ISP)产品预期按购买的 IP 列表发放独立 `host:port` 端点（而非共享网关+会话参数），user:pass 鉴权，理论上可直接匹配 `host:port:user:pass` 格式；SOCKS5+UDP 已被 Proxyway 实测确认 [16]。**需改动**：无需改 `residential-helper.sh` 解析逻辑；需在 `enable --add` 时确认该端点是否为真正固定 IP（而非同一域名解析到轮换后端）。

**Rayobyte** — 静态/独立列表格式的 proxy list（ip:port:user:pass），与 b-ui 解析器天然兼容；SOCKS5 仅 TCP，与当前 relay 的"UDP 无论如何直连"假设并不冲突。**需改动**：无。

**MarsProxies** — 官方页面未披露静态 ISP 产品确切连接格式，但同样属于"购买后发一批固定 IP 的代理列表"模式，大概率兼容 `host:port:user:pass`；协议明确含 UDP。**需改动**：无，接入前先用试用账号核实一次端点格式。

**Oxylabs** — 自助版按 IP 固定端口（`disp.oxylabs.io:8001+`，每个端口对应一个固定出口 IP，port 8000 是轮换池必须避开）；企业版默认只开 HTTP(S)，SOCKS5 需邮件申请人工开通后才是普通 `host:port` 形式 [28]。**需改动**：`enable --add` 时需注意 port 8000 陷阱；企业版路线需提前完成 SOCKS5 开通申请，否则首次接入会失败。

**Proxy-Seller** — 描述为"来自 600+ 子网的独享静态住宅 IP"，典型代理商模式预期为固定 `ip:port:user:pass`。**需改动**：无，同样建议先用最小订单验证实际发放格式。

**通用建议**：无论选哪家，`enable --add` 时优先用 `-` 从 stdin 读取凭据（避免密码出现在 `ps`），并在正式全量接入前，先按第 8 节跑通 24 小时稳定性测试，确认该固定 IP 在观察窗口内真的没有变化——这是比协议细节更容易被厂商文档模糊带过、但对 b-ui 架构更致命的一点。

---

## 8. 验证计划（bwg-rick 上的低成本试测协议）

**前提**：只买 1 个美区 IP 的最小额度（通常 $2–5，24 小时/周付起），不做任何长期承诺。

### 步骤

1. **接入**：通过面板以 `socks5://user:pass@host:port`（或 `http://…`，视供应商而定）添加为住宅上游，等价于 `residential-helper.sh enable --add -`（凭据走 stdin，不落 `ps`）。
2. **IP 类型分类**（在 relay 生效后，从走该上游的出口发起）：
   - `curl` 到 ippure.com、`api.ipquery.io`、`ipinfo.io` 三个服务，记录返回的 ASN、组织名、`type` 字段（residential/isp/hosting/datacenter）、fraud/purity 分数。
   - **通过标准**：三者一致判定为 residential/isp，且 ASN 组织名与供应商宣传的运营商一致（如宣传"Comcast"则 WHOIS 也应落在 Comcast 相关 ASN）；任一工具判定为 datacenter/hosting，或 fraud 分数明显偏高（IPQualityScore 式 >75），判为不通过。
3. **目标可达性测试**（逐个用 `curl -x socks5h://…` 或对应代理测试，记录 HTTP 状态码/curl 退出码/是否出现验证码页面）：
   - `www.google.com`、`gemini.google.com` — 通过标准：200，且响应体中不含 `recaptcha`/`sorry/index` 关键字（人机验证页特征）。
   - `chatgpt.com`、`api.openai.com` — 通过标准：200/正常 API 响应，非 Cloudflare 拦截页。
   - `claude.ai` — 通过标准：200，正常加载登录页而非区域拦截提示。
   - `tiktok.com` — 通过标准：200（若供应商明确声明不支持可跳过，标记为已知限制）。
   - `checkout.stripe.com` — 通过标准：200，非拒绝页（对应 Bright Data 的 policy_20050 支付类目封锁基线）。
   - `mtalk.google.com:5228`（FCM）与 `courier.push.apple.com:5223`（APNs）— 用 `curl -v --connect-timeout 5 telnet://<host>:<port>` 或 `nc -zv` 方式测 TCP 连通；通过标准：连接建立不被立即 RST（对应 Bright Data 的端口白名单封锁基线，这是 owner 遇到的第二大量级 403 来源）。
4. **UDP 测试**：对该上游的 SOCKS5 端口发起一次标准 SOCKS5 UDP ASSOCIATE 请求（可用支持 UDP 的测试脚本或 `curl --socks5-basic` 结合已知 UDP echo 服务），确认是否真的支持 UDP ASSOCIATE，而不是仅信任供应商文案；不支持也不阻塞上线（见第 7 节），但应记录在案。
5. **24 小时稳定性测试**：以 cron 每 10–15 分钟测一次出口 IP（同 `ippure`/`ipinfo` 调用），确认 24 小时内 IP 未发生变化；同时监控 relay 日志中该上游触发 auto-blacklist（403/连接被拒）的次数，**通过标准**：IP 全程不变，且 24 小时内因该上游导致的目标拒绝次数为 0 或远低于 Bright Data 现有基线（b-ui 生产日志显示 Bright Data 约每 30 分钟 256 次 403）。
6. **人机验证（CAPTCHA）检查**：在 Google 搜索结果页面手动检查（或用 `get_page_text` 抓取响应体）是否出现"我们的系统检测到异常流量"提示；出现即视为该 IP 已被标记，即便 HTTP 状态码仍是 200，也应判为**不通过**。

### 综合通过/不通过判据

**通过（可进入正式采购）**：步骤 2 三工具一致判定为真实 ISP/住宅且无高欺诈分；步骤 3 中 Google/AI 服务/支付/FCM/APNs 全部可达且无验证码；步骤 5 的 24 小时窗口内 IP 未变、无异常拒绝。三者缺一即应视为**不通过**，退回第 1 节候选名单中的下一顺位重新试测，而不是加大同一家的购买量。

---

## 9. 未决问题

**本轮公开资料研究未能确认，需要供应商直接书面答复或实测才能解决：**

- Decodo 的受限目标清单文档明确写"适用于 mobile proxy"，是否**同一份清单也适用于 owner 会购买的 Static Residential(ISP)** 产品线，还是该产品线有更宽松（或更严格）的独立清单——这是 Decodo 排名第一但最需要在下单前用工单书面确认的疑点 [12]。
- Webshare 的受限网站文档同样只明确覆盖"Rotating Residential network"，Static Residential SKU 的实际限制范围未知 [33]。
- **IPRoyal 与 Decodo 的静态住宅/ISP IP 池，是否（哪怕部分）通过转售/白标关系间接来自已被处置的 NetNut/Popa 或 IPIDEA 网络**——谷歌官方博客原文写"许多流行的住宅代理品牌实际上是在白标 NetNut 僵尸网络"，但未点名具体哪些品牌 [3]。本轮的两次专项追问（针对 IPRoyal/Decodo 与 NetNut/IPIDEA 的白标关系）因子任务的网络搜索配额耗尽，**未获得任何新证据**，需在预算充足的新会话中重跑。
- **CliProxy 是否与 IPIDEA 网络存在股权/白标/基础设施关联**——2026-01 谷歌/BleepingComputer 公布的 IPIDEA 品牌清单（360/922/ABC/Cherry/DoorVPN/GalleonVPN/IP2World/LunaProxy/PIA-S5/PyProxy/RadishVPN/TabProxy）中**未包含** cliproxy.com，这是弱的排除性证据而非清白证明；专项追问同样因搜索预算耗尽未获得新结果。
- **UDP ASSOCIATE 支持的独立实验室实测**——除 Decodo 被 Proxyway 明确验证、Rayobyte 被明确验证为不支持外，IPRoyal、Webshare、Proxy-Cheap、Oxylabs、MarsProxies 均只有营销文案、无独立测试；专项追问因搜索预算耗尽未获得新结果，需第 8 节步骤 4 亲自实测替代。
- **api.openai.com / api.anthropic.com / generativelanguage.googleapis.com 的真实可达性**——本轮对 IPRoyal、Decodo、Proxy-Cheap、MarsProxies 的专项活体测试追问，因子任务网络搜索预算耗尽而完全未获得证据（该问题本身也超出纯资料调研的能力边界，唯一可靠答案来自第 8 节的实测协议，而非文献检索）。
- Oxylabs 的强制 KYC"用例问卷"是否会把 B-UI"住宅 IP 转售给约 1000 名下游 VPN 订阅用户、代理其任意流量"的模式判定为需拒绝的"代理转售/规避检测"类用例——官方文档给出的是主观表述（"若被判定为不合适、可疑或不道德"），没有可预期的清单，需直接邮件咨询销售/风控团队后再决定是否投入超出免费试用的预算 [26]。
- SOAX 的 Guardrails 解锁流程是否对"FCM/APNs 等非 SMTP 的自定义端口"同样生效（官方文档目前唯一明确举例的受限端口只有 SMTP 25/465/587），以及 Sumsub 身份验证环节对中国大陆身份证件是否存在摩擦，均未证实 [29]。
- 多数候选供应商（Oxylabs、Rayobyte、SOAX、Proxy-Seller 之外的绝大多数）的支付宝/微信支付/USDT 支持状态未经结算页直接核实，仅凭第三方转述或缺席推断。
- Webshare、IPRoyal 的转售条款是否会把 B-UI 的共享静态 IP 转售模式判定为违约，未直接询问供应商合规团队确认 [32][18]。
- ippure.com 自身的分类方法论未见独立技术审计，其分数应作为参考信号之一、与 IPQualityScore/Scamalytics/ASN WHOIS 交叉验证，而非唯一依据。

---

## 10. 参考文献

1. Bright Data 错误码目录 — https://docs.brightdata.com/proxy-networks/errorCatalog （查取 2026-09-11）
2. Bright Data 可接受使用政策 — https://brightdata.com/acceptable-use-policy （查取 2026-09-11）
3. Google Cloud 博客：GTIG 联合 FBI/Lumen 处置 NetNut/Popa 住宅代理网络 — https://cloud.google.com/blog/topics/threat-intelligence/google-continued-disruption-residential-proxy-networks （事件 2026-07-02，查取 2026-09-11）
4. KrebsOnSecurity：FBI 查封 NetNut 代理平台（Popa 僵尸网络） — https://krebsonsecurity.com/2026/07/fbi-seizes-netnut-proxy-platform-popa-botnet/ （2026-07-02）
5. BleepingComputer：谷歌处置由恶意软件驱动的 IPIDEA 住宅代理网络 — https://www.bleepingcomputer.com/news/security/google-disrupts-ipidea-residential-proxy-networks-fueled-by-malware/ （2026-01-29）
6. TheHackerNews：谷歌处置全球最大住宅代理网络之一 IPIDEA — https://thehackernews.com/2026/01/google-disrupts-ipidea-one-of-worlds.html （2026-01-29）
7. ipweb.cc：IPIDEA/922 关停分析与替代方案 — https://ipweb.cc/blog/ipidea-922-shutdown-analysis-alternatives （查取 2026-09-11）
8. pyproxy.com：PIA S5 Proxy 专题页 — https://pyproxy.com/pia-s5/ （查取 2026-09-11）
9. pyproxy.com 官网首页（关停/接盘声明） — https://pyproxy.com/ （查取 2026-09-11）
10. blog.katorly.com：CliProxy 伪住宅代理实测 — https://blog.katorly.com/Proxy-Cliproxy-Fake-Residential/ （2025-06-13）
11. Decodo FAQ：是否有屏蔽网站 — https://decodo.com/faq/general/do-you-have-any-blocked-sites （查取 2026-09-11）
12. Decodo 帮助中心：受限目标（mobile-proxy-restricted-targets） — https://help.decodo.com/docs/mobile-proxy-restricted-targets （查取 2026-09-11）
13. Decodo 帮助中心：ISP 按GB付费代理地区列表 — https://help.decodo.com/docs/cn/isp-pay-per-gb-proxy-locations （查取 2026-09-11）
14. Smartdaili（Decodo 中文站）帮助中心：支付宝说明 — https://help.smartdaili.cn/docs/miscellaneous-alipay （查取 2026-09-11）
15. Smartdaili 帮助中心：独享 ISP 代理 — https://help.smartdaili.cn/docs/dedicated-isp （查取 2026-09-11）
16. Proxyway：Smartproxy/Decodo 代理实测评测 — https://proxyway.com/reviews/smartproxy-proxies （查取 2026-09-11）
17. Proxyway：最佳 ISP 代理对比榜 — https://proxyway.com/best/isp-proxies （查取 2026-09-11）
18. IPRoyal 可接受使用政策 — https://iproyal.com/acceptable-use-policy/ （查取 2026-09-11）
19. IPRoyal ISP 代理产品页 — https://iproyal.com/isp-proxies/ （查取 2026-09-11）
20. IPRoyal 微信支付购买页 — https://iproyal.com/other-proxies/buy-a-proxy-with-wechat-pay/ （查取 2026-09-11）
21. IPRoyal 支付宝购买页 — https://iproyal.com/other-proxies/buy-a-proxy-with-alipay/ （查取 2026-09-11）
22. IPRoyal USDT/Tether 购买页 — https://iproyal.com/other-proxies/tether-usdt/ （查取 2026-09-11）
23. IPRoyal 帮助中心：流量使用上限说明 — https://help.iproyal.com/en/articles/7222172-how-much-data-can-i-use （查取 2026-09-11）
24. Proxyway：IPRoyal 代理实测评测（ASN匹配率27%，Comcast IP回收事件） — https://proxyway.com/reviews/iproyal-proxies （查取 2026-04，复核 2026-09-11）
25. Oxylabs 开发者文档：ISP 代理受限目标 — https://developers.oxylabs.io/products/proxies/isp-proxies/restricted-targets （查取 2026-09-11）
26. Oxylabs：KYC 与安全说明 — https://oxylabs.io/kyc-and-safety （查取 2026-09-11）
27. Oxylabs 可接受使用政策 — https://oxylabs.io/legal/oxylabs-acceptable-use-policy （查取 2026-09-11）
28. Oxylabs 帮助中心：开始使用独享 ISP 代理（SOCKS5 开通说明） — https://developers.oxylabs.io/help-center/getting-started/start-using-dedicated-isp-proxies （查取 2026-09-11）
29. SOAX 开发者文档 FAQ（Guardrails 解锁流程） — https://developers.soax.com/troubleshooting/faq.md （查取 2026-09-11）
30. SOAX 服务条款 — https://soax.com/legal/terms-of-use （查取 2026-09-11）
31. SOAX 开发者文档：错误码 — https://developers.soax.com/troubleshooting/error-codes.md （查取 2026-09-11）
32. Webshare 服务条款 — https://www.webshare.io/terms （查取 2026-09-11）
33. Webshare 帮助中心：轮换住宅代理网络受限网站 — https://help.webshare.io/en/articles/10068143-restricted-websites-on-our-rotating-residential-proxy-network （查取 2026-09-11）
34. caproxy.com：Webshare 供应商评测（IP滥用标记报告） — https://caproxy.com/en/list/webshare/ （查取 2026-09-11）
35. Trustpilot：Webshare 用户评价 — https://www.trustpilot.com/review/webshare.io （查取 2026-09-11）
36. Rayobyte 服务条款与可接受使用政策 — https://rayobyte.com/tos-and-aup/ （查取 2026-09-11）
37. Proxyway：Rayobyte 代理实测评测（ASN匹配率88%，无UDP） — https://proxyway.com/reviews/rayobyte-proxies （查取 2026-09-11）
38. Rayobyte 支持门户：ISP 代理 ASN 列表 — https://portal.rayobyte.com/en/support/solutions/articles/64000264987-isp-proxy-asns （查取 2026-09-11）
39. Proxy-Cheap 静态住宅代理产品页 — https://www.proxy-cheap.com/services/static-residential-proxies （查取 2026-09-11）
40. Proxy-Seller ISP 代理产品页 — https://proxy-seller.com/isp/ （查取 2026-09-11）
41. buyresidentialproxy.com：加密货币支付供应商榜单（含Proxy-Seller） — https://www.buyresidentialproxy.com/best/crypto-payment/ （查取 2026-09-11）
42. MarsProxies ISP 代理产品页 — https://marsproxies.com/proxies/isp-proxies/ （查取 2026-09-11）
43. MarsProxies 加密货币购买说明 — https://marsproxies.com/other-proxies/buy-proxy-with-cryptocurrency/ （查取 2026-09-11）
44. Infatica ISP 代理产品页 — https://infatica.io/proxy-types/isp-proxies/ （查取 2026-09-11）
45. Infatica：IPIDEA 替代方案说明 — https://infatica.io/ipidea-alternative/ （查取 2026-09-11）
46. ippure.com 官网 — https://ippure.com/en/ （查取 2026-09-11）
47. b-ui 仓库内部文件：`server/residential-helper.sh`（本次工作树）
48. b-ui 仓库内部设计文档：`docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`（本次工作树）
49. IPFoxy 官网 — https://www.ipfoxy.com/ （查取 2026-09-11）
50. Proxy302 常见问题 — https://www.proxy302.com/faq/ （查取 2026-09-11）
51. ipipgo 套餐下单页 — https://www.ipipgo.com/zh-CN/packageOrder?packageType=1&type=5 （查取 2026-09-11）
52. Kookeey 官网 — https://www.kookeey.com/ （查取 2026-09-11）
53. 腾讯云开发者社区：Kookeey 相关报道（IP重复率投诉） — https://cloud.tencent.com/developer/news/3732830 （查取 2026-09-11）
54. Nstproxy（nstdata.io）官网 — https://www.nstdata.io/ （查取 2026-09-11）
55. ABCproxy 静态住宅代理产品页 — https://www.abcproxy.com/products/static-residential-proxies.html （查取 2026-09-11）
56. 腾讯云开发者社区：代理商整治相关报道（涉及ABCproxy） — https://cloud.tencent.com/developer/news/3627133 （查取 2026-09-11）
57. 神龙HTTP代理 — https://h.shenlongip.com/ipdaili/2604.html （查取 2026-09-11）
58. Webshare 公平使用政策 — https://www.webshare.io/fair-usage-policy （查取 2026-09-11）
59. NodeMaven 术语表：代理限制说明 — https://nodemaven.com/glossary/limitations-of-proxies/ （查取 2026-09-11）
60. Massive 博客：最佳 ISP 代理供应商（自我基准数据） — https://www.joinmassive.com/blog/best-isp-proxy-providers （查取 2026-09-11）
61. Thordata ISP 代理定价页 — https://www.thordata.com/pricing/isp-proxies （查取 2026-09-11）
62. Thordata 评测（Trustpilot 评分被暂停问题） — https://networthexplained.com/articles/thordata-review/ （查取 2026-09-11）
63. DataImpulse 博客：NetNut 评测与定价（交叉引用） — https://dataimpulse.com/blog/netnut-review-pricing/ （查取 2026-09-11）
64. Byteful（Ping Proxies）服务条款 — https://byteful.com/terms-and-conditions （查取 2026-09-11）

**方法论说明**：本报告完全基于公开网页资料（官方文档、第三方实验室评测、Trustpilot/评测站、中英文自媒体）的二手综合，未进行任何账号注册、付款或实测；标注为 *measured* 的条目来自 Proxyway 等第三方在独立实验室环境下的实测数据，*documented* 来自供应商官方文档/条款原文，*anecdotal* 来自零散第三方评价或自媒体文章，可信度依次递减。凡与官方营销文案冲突的独立实测数据，本报告均优先采信实测数据。
