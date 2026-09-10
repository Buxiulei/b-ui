# 住宅代理选购与配置指南

> 本文核对日期 **2026-09-10**，表格里的参数格式、端口、超时值全部来自各供应商当时的官方文档。
> 供应商改文档不会通知你，**接入前请以其当前文档为准**；本文只保证"当时是这么写的"。

面板"添加代理"里把供应商给的那一行原样粘进去就能跑（`socks5://user:pass@host:port`、
`http://user:pass@host:port`、`host:port:user:pass`、`user:pass@host:port` 四种格式都认，
不写协议前缀时先试 SOCKS5、失败再试 HTTP），但**默认参数下住宅出口会静默轮换**——
这是所有主流供应商的默认行为，也是 B-UI 的健康探测最容易被误导的地方。这篇文档讲清三件事：
用户名里该写什么、为什么必须写、以及面板上的读数怎么看。

---

## 1. 先说结论：把粘性参数写进用户名

B-UI 解析出`网关:端口` + `用户名:密码`（协议可以是 SOCKS5 或 HTTP），**用户名字段原样透传给供应商**（供应商的所有会话
参数都编码在用户名里，且都不含冒号，不会破坏解析）。所以只要按下表把参数拼进用户名，
就能拿到供应商侧的**会话粘性 + 出口失效显式报错**，B-UI 一行代码都不用改。

| 供应商 | 网关 / 端口 | 粘性参数（写进用户名） | 默认 / 上限 | 防"静默换 IP" |
|---|---|---|---|---|
| Bright Data | `brd.superproxy.io`，**SOCKS5 = 22228**，HTTP(S) = 44445 | `-session-<自定字符串>`；另有 `-country/-state/-city/-zip/-asn/-os` | 空闲 7 分钟失效 | `-const` → peer 不可用时返回 **502**，不换 IP |
| Oxylabs | `pr.oxylabs.io:7777`，用户名形如 `customer-USER-cc-XX-sessid-…` | `sessid-<id>` + `sesstime-<分钟>` | 默认 10 分钟，或 **60 秒无请求**先到者失效；`sesstime` 最长 1440 分钟 | `sessid_oneip` → IP 不可用时请求以 **502** 失败，不轮换 |
| Decodo（原 Smartproxy） | `gate.decodo.com:7000`，用户名形如 `user-…-session-…-sessionduration-90` | `session-<id>` + `sessionduration-<分钟>` | 默认 10 分钟，可配 **1–1440** 分钟；60 秒无活动终止 | 未见显式参数 |
| IPRoyal | `geo.iproyal.com:12321` | `_session-<恰好 8 位字母数字>` + `_lifetime-<1 秒–7 天，单一时间单位>` | — | `_killswitch-1` → 返回 **410**，不静默换 IP |
| SOAX | `proxy.soax.com:1337`，用户名形如 `package-…-sessionid-…-sessionlength-…` | `sessionid-<≤32 位字母数字下划线>` + `sessionlength-<秒>` | **60 秒无活动**即过期 | 未文档化 |

来源：[Bright Data SOCKS5](https://docs.brightdata.com/proxy-networks/socks5)、
[Bright Data 配置项](https://docs.brightdata.com/proxy-networks/config-options)、
[Bright Data FAQ](https://docs.brightdata.com/proxy-networks/faqs)、
[Oxylabs 会话控制](https://developers.oxylabs.io/products/proxies/residential-proxies/session-control)、
[Decodo 用户名格式](https://help.decodo.com/docs/residential-proxy-user-pass-requests)、
[IPRoyal 轮换](https://docs.iproyal.com/proxies/residential/proxy/rotation)、
[SOAX 住宅](https://developers.soax.com/proxies/residential)。

**例**（Bright Data，会话固定 + 出口失效直接报错）：

```
socks5://brd-customer-hl_xxxx-zone-residential-session-buiai01-const:密码@brd.superproxy.io:22228
```

---

## 2. 为什么 AI 登录必须粘住同一个出口 IP

- 目标站把"同一账号短时间内从多个国家/多个 ASN 出现"当作强风险信号。住宅链路本身抖动就大，
  一旦出口在会话中途换掉，等于给账号加了一条异地登录记录。
- 商业住宅的粘性会话上限普遍是 **10–30 分钟**，本来就不是"永久固定"；能锁多久就锁多久，
  并且**同一个用户长期用同一条线路**，比"自动选延迟最低的那条"重要得多。
- 因此 v3.6.0 起中继侧 `resi-pool` 从 `urltest`（按延迟择优，50ms 抖动就漂移）改成
  `selector`（粘住当前线路），只有健康巡检判定当前线路连续不达标时才切换，
  且切换走 Clash API 热切换、**不重启中继、不掐断现有连接**。

**不加粘性参数会怎样**：所有核实过的供应商默认行为都是"当前出口不可用就静默换一个新 IP 继续"。
这时 B-UI 探到的"健康"只意味着"网关又给了一个没验证过的 IP"，健康语义直接失真。
`-const` / `sessid_oneip` / `_killswitch-1` 这类参数把"IP 死了"变成显式 502/410，
巡检才能真的探出问题。

---

## 3. 套餐与条款：两个容易踩的坑

- **"不限量"不等于无限**：Bright Data 的 unlimited（机房/ISP 线路）实为**每个 IP 每月 100GB
  公平使用配额**，85% / 100% 触发邮件告警，超出转按量计费而非断服
  （[fair use allowance](https://docs.brightdata.com/general/usage-monitoring/fair_use_allowance)）。
  住宅按流量计费的套餐，AI 网页端的图片/流式响应比想象中吃流量，先按 GB 估算再买。
- **转售/共享条款**：Bright Data 许可协议未经事先书面授权禁止整体或部分转售
  （[license](https://brightdata.com/license)）；IPRoyal AUP 2.3.15 禁止"出售、赠与或向未经
  授权第三方授予账号访问权"（[AUP](https://iproyal.com/acceptable-use-policy/)）；
  Oxylabs AUP 保留"首次违规即终止服务"的权利
  （[AUP](https://oxylabs.io/legal/oxylabs-acceptable-use-policy)）。
  **把一份个人住宅套餐分给面板上的多个付费用户，属于条款风险，需要自行确认**；行业里有官方
  转售通道（PacketStream Reseller、Geonode Reseller Program），那是另一种商业关系。
- 健康探测本身不违规：翻遍 Bright Data AUP 未见针对"心跳/连通性自动化测试"的禁令，
  禁的是 DDoS、垃圾邮件、抓登录后内容等内容层滥用。**约束来自技术侧**（见下一节的探测频率）。

---

## 4. 采购建议与池子来源的风险披露

- 优先 **Static ISP / 静态住宅**产品线：长期绑定单一客户、出口可预测，没有 rotating 池
  "随机换到一个坏 IP"的问题；其次是 KYC 严格的企业级供应商。
- 避开只收加密货币、无需身份验证的极低价包。Bitsight 2026 年对 30 家弱 KYC 代理服务、
  5,334 万个唯一出口 IP 的测量显示：**15.49% 同时被标记为活跃恶意软件感染**，12.78% 被标记
  riskware（[报告](https://www.bitsight.com/blog/residential-proxy-services-malware-ecosystems)）。
  用这类池子，等于让你的 AI 账号从一台被入侵的家用设备出网。
- 住宅 IP 高流失是结构性的，不是配置问题：IPinfo 实测 46% 的住宅 IP 同时出现在 ≥2 家代理商
  网络，60% 的 IP 在 90 天窗口里只出现过一次，平均可见期 4.56 天
  （[数据](https://ipinfo.io/blog/residential-proxy-shared-infrastructure-churn)）。
  所以"多买一条备线 + 让巡检自动剔除/恢复"比"精挑一个好 IP"有用。
- **IP 干净只是必要条件，不是充分条件**：TLS 指纹（JA3/JA4）换 IP 洗不掉，CLI / SDK 类客户端
  被 Cloudflare 拦下的案例与节点位置无关；支付渠道、设备指纹同样进风控模型。
  B-UI 只能改善 IP 这一个因子，**不能承诺不封号**。
- 合规：Anthropic AUP 明文禁止为不受支持地区的用户提供 Claude 访问，其
  [supported countries](https://www.anthropic.com/supported-countries) 列表不含中国大陆；
  OpenAI Terms of Use 的 Trade Controls 条款同理。**这是使用者自己的合规责任。**

---

## 5. 端口白名单：verify 失败最常见的真实原因

Bright Data 文档写死了住宅/移动线路的可用目标端口：
`Datacenter & ISP: all ports higher than 1024`；
**`Residential & Mobile: 8080, 8443, 5678, 1962, 2000, 4443, 4433, 4430, 4444, 1969`**，
且住宅 SOCKS5 "only towards HTTPS targets"（[SOCKS5 文档](https://docs.brightdata.com/proxy-networks/socks5)）。
**80 / 443 都不在名单里。**

后果：B-UI 添加住宅 URL 时的 `verify()` 会拨 `https://api.ipify.org`（443），面板体检也拨 443，
在 Bright Data 住宅 SOCKS5 上**必然失败**，而报错看起来像"凭据错了"。
遇到这种情况：改用该供应商的 HTTP(S) 代理端口（Bright Data 是 44445），或联系供应商放开目标端口。
v3.6.0 起 B-UI **原生支持 HTTP 上游**（中继写 sing-box `http` 出站），添加时自动探测是 SOCKS5
还是 HTTP，所以直接粘 44445 那一行也能用——详见下面第 9 节。

---

## 6. UDP / QUIC 是怎么处理的

住宅 SOCKS5 是否支持 UDP ASSOCIATE 基本没有供应商文档化，RFC 1928 允许服务器直接回
`X'07' Command not supported`。B-UI 因此**假设住宅腿是 TCP-only**，在中继 `route.rules` 里显式处理：

```
udp/53  → direct    （DNS 不进住宅腿）
udp/443 → reject    （QUIC 显式拒绝，浏览器自动回退 TCP/HTTP2，再走住宅）
其余 udp → direct
```

所以开启"全局模式"时 DNS 与 QUIC 不会被塞进住宅 SOCKS5 里挂死。域名解析交给上游
（`socks5h` 语义：目标域名原样传给供应商解析），HTTP 上游则是 `CONNECT 域名:端口`。

**中继自己在热路径上不做 DNS 解析**（2026-09-10 用 sing-box 1.14 实测：把中继的两个 DNS 服务器
都换成本机假 DNS 再经中继请求 AI 域名，上游收到的是 `CONNECT chatgpt.com:443`，两个假 DNS
一次都没被查）。所以配置里 `dns_resi`（`detour: resi-pool`）在纯 HTTP 上游池下也不会被查到——
HTTP 出站扛不了 UDP，但它压根不在请求路径上；路由需要解析时走的是
`route.default_domain_resolver`（`dns_direct`，直连）。

---

## 7. 探测频率：别把自己的出口 IP 打成机器流量

供应商按 **per-IP 请求速率**风控（Bright Data 明文说不设全局上限但监控每个 IP 的请求数，
过快返回 429），而住宅会话的空闲超时普遍是 60 秒到 7 分钟——10 秒一次的探测既在人为给
可能已劣化的会话续命，又把该出口 IP 的请求节奏做成了机器指纹。v3.6.0 的三层频率：

| 层 | 频率 | 说明 |
|---|---|---|
| 中继 `resi-pool` | 不再主动探测 | 已从 `urltest` 改成 `selector`，只在巡检要求时切换 |
| 客户端订阅 `urltest` | 60s，`interrupt_exist_connections: false`，tolerance 100 | 不打断已建立的连接 |
| 服务端巡检 `resi-health.sh` | 约 2 分钟一轮（timer 带 30s 随机抖动），每条线路 2 次探测，任一成功即算健康 | 连续 2 轮不达标才剔除，连续 2 轮健康才恢复 |

探测目标是中性的 `https://www.gstatic.com/generate_204`，**不会**反复直打 AI 域名
（那会给出口 IP 加行为指纹）。

---

## 8. 面板"住宅 IP 健康"卡怎么看

卡片下方的成员表以**中继实际生效的配置**（`/opt/b-ui/singbox-relay.json` 的 socks 出站）为真源，
和巡检脚本看的是同一份数据：

| 列 | 含义 |
|---|---|
| 线路 | 中继里的出站 tag（`resi-1` / `resi-2`…），高亮那一行是 selector **当前选中**的出口 |
| 上游 | 该线路的供应商网关 `host:port`（不显示凭据） |
| 巡检 | `健康` / `已剔除`——来自 `.resi-health-state.json` 的迟滞判定；悬停看连续轮数 |
| 出口 IP | 该线路刚才实测的出口 IP（每次点"检查"都经这条线路真拨一次） |
| 类型 | 出口 IP 的画像：`家庭宽带 IP` / `IDC机房 IP` / `移动网络 IP` / `代理 IP` / `unknown` |

数据源三层，全部 HTTPS、全部无需 API key、全部经该成员的 SOCKS5 拨出（v3.6.0 起不再用明文
`http://ip-api.com`：明文经住宅腿可被篡改，且其免费端点禁商用）：

| 顺序 | 数据源 | 提供什么 |
|---|---|---|
| 主源 | `https://my.ippure.com/v1/info` | `isResidential`（→ 家庭宽带 / IDC机房）、`fraudScore`（面板 ISP 一列里的"风险分 N"）、ASN 与归属、国家城市 |
| 备源 | `https://api.ipquery.io/?format=json` | `risk.is_datacenter / is_vpn / is_proxy / is_mobile` → 同样的类型映射；ASN 与归属 |
| 兜底 | `https://ipinfo.io/json` | 只有 IP / `org` / 国家城市，类型记 `unknown` |

主源失败、返回非 JSON、或**返回了 IP 但没有分类字段**时自动往下一层退（最后一种情况仍保留主源
拿到的 IP 与归属）。三层都不通 → 该行出口 IP 显示 `—`，说明这条线路当下拨不出去。

**关于 `风险分`**：来自主源的 `fraudScore`（0–100，越高越可疑），是"这个 IP 的历史滥用画像"，
不是"你这次请求被判定的结果"。数值高不必然封号，但配合 `IDC机房 IP` 一起出现时应该换线路。

**关于 `unknown`**：只会在"走到兜底源"时出现，意思是**没有判据**，不是"判定为可疑"；
此时出口 IP、ISP、国家/城市仍然准确。（历史注记：v3.6.0 开发期间曾用 `api.ipapi.is` 作主源，
但它自 2026-09-01 起把 `is_datacenter`/`is_vpn`/`is_proxy`/`is_tor` 移到了 API key 之后，
匿名调用不再返回任何分类字段，因此改用上表三源。）

判读建议：

- 高亮行显示 `IDC机房 IP` → 供应商很可能给了机房段，AI 站点风险显著升高，建议换线路或找供应商。
- 某行 `已剔除` 且出口 IP 为空 → 这条线路探测不通；若供应商加了 killswitch 参数，
  这通常意味着**那个粘住的 IP 真的下线了**，需要换一个 session id。
- 全部 `已剔除` → 巡检不会乱切，会保持当前选择并在
  `/var/log/b-ui-resi-health.log` 记 `WARN`；此时住宅链路实际不可用，请优先看供应商余额与条款状态。

---

## 9. Bright Data 的官方事实（2026-09-10 核对，含 HTTP 上游）

这一节是主理人手上那套 ISP zone 凭据的实测 + Bright Data 官方文档核对结果，其它供应商同理但参数名不同。

### 9.1 HTTP 与 SOCKS5 是同一套凭据、不同端口

同一个 `brd-customer-…` 用户名/密码，两种协议都能用，只是端口不同：

| 协议 | Bright Data 端口 | 备注 |
|---|---|---|
| HTTP / HTTPS | **44445** | 旧文档里的 22225 / 33335 是同一个入口的历史端口 |
| SOCKS5 | **22228** | 固定端口，不随 zone 变 |

把 SOCKS5 打到 44445 会收到 `invalid version in initial SOCKS5 response`（那是个 HTTP 端口），
把 HTTP CONNECT 打到 22228 同样不通。**B-UI 添加时会自动识别**：不带协议前缀的输入先试 SOCKS5、
失败再试 HTTP，识别结果记进 `residential-proxy.json` 的 `type` 字段，中继按类型出站
（`socks` / `http`），面板节点池那一行会显示 `SOCKS5` 或 `HTTP` 徽标。
想跳过探测就自己写前缀：`socks5://…`、`socks5h://…`（同义）或 `http://…`。

**Bright Data 推荐用 HTTP 端口（44445）当上游**。2026-09-10 在生产机上用真实 ISP zone 凭据实测，
同一套凭据两个端口的差别是：

| 目标 | SOCKS5 · 22228 | HTTP · 44445 |
|---|---|---|
| HTTPS + 域名（如 `https://api.ipify.org`） | 通 | 通 |
| HTTPS + 显式 IP | 通 | 通 |
| **明文 HTTP（80 端口，如 `http://neverssl.com/`）** | **不通（无响应）** | 通 |

两个端口的出口 IP 是同一个，所以选 HTTP 端口没有任何损失，还多拿到明文 HTTP 目标的能力
（AI 站点全是 HTTPS，但客户端里总有零星明文回落、探测与 OCSP 之类的请求）。
操作上不用记这些：把供应商 IP 列表里的 `host:44445:user:pass` **整行粘进面板**，
自动探测会识别成 `http` 类型。

供应商后台导出的 **IP 列表 CSV 每行就是 `host:port:username:password`**，可以整行粘贴，不用手工改写。

### 9.2 目标必须是域名，不要在本地解析成 IP

Bright Data 文档要求 SOCKS5 用 `socks5h` 语义（把域名交给出口节点远端解析）。
2026-09-10 实测：**HTTPS 打显式 IP 其实是通的**，所以"显式 IP 一定被拒"这句话不成立；
但按文档，走 HTTP 代理时 IP 目标会改由"超级代理"直接发出，**出口就不再是你的住宅/ISP IP**——
这才是"尽量交域名"的真实理由（出口归属，不是能不能连）。
B-UI 中继把目标域名原样交给上游（不本地解析，实测见第 6 节），所以客户端侧请保持 TUN / 域名嗅探
开启，不要在本地把域名解析成 IP 再连。想让解析确定发生在出口节点，可在用户名加 `-dns-remote`。

### 9.3 超级代理绕过（superproxy bypass）：出口显示成机房 IP 不一定是配置错了

Google / Bing / YouTube 等搜索引擎与一部分第三方 IP 检测站会被 Bright Data 的超级代理直接代发，
此时对方看到的是 Bright Data 机房 IP 而不是住宅 peer。**这不是配置错误。**
要"宁可失败也不绕过"，在用户名加 `-route_err-block`。
面板体检卡的 ippure / ipquery 读数以"实际经上游请求"为准（实测 ippure 未被绕过）。

### 9.4 Residential（非 ISP）zone 未做 KYC 时 HTTPS 会被证书拦截

未完成 KYC 的 Residential / Mobile 网络对 HTTPS 做证书拦截，需要在客户端装 Bright Data 的根证书。
B-UI 是端到端 TLS、**不会**装第三方 CA，所以这种 zone 上会直接出现证书错误。
建议用 **ISP / Datacenter zone**，或把 KYC 做完。

### 9.5 粘性与会话参数

| 参数（写进用户名） | 行为 |
|---|---|
| `-session-<id>` | 同 ID 同 IP；**空闲 7 分钟**释放 |
| `-const` | 绑定 peer，该 peer 不可用时返回 **502**，不静默换 IP |
| `-ip-x.x.x.x` | 专属池里钉死某个 IP（本项目主理人当前 zone 只有一个 IP） |
| `glob_` 前缀 | 忽略来源 IP（多台机器共用同一会话时用） |

### 9.6 目标端口白名单（与第 5 节一致）

- **ISP / Datacenter**：>1024 的全部端口，外加 80 / 443。
- **Residential / Mobile**：仅 `8080, 8443, 5678, 1962, 2000, 4443, 4433, 4430, 4444, 1969`。

面板添加失败且报"两种协议都连不上"时，先看这条：住宅 zone 上 443 探测**必然**失败，
和凭据对不对无关。
