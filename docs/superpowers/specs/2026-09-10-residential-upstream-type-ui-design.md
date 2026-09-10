# 住宅上游类型自动探测（SOCKS5 / HTTP）+ 填写交互重做（v3.6.0 追加，R10）

- 日期：2026-09-10
- 目标版本：v3.6.0
- 状态：已批准（主理人选定：原生支持 HTTP 上游 + 自动探测；单粘贴框 + 实时解析预览）
- 文件：`server/residential-helper.sh`、`server/resi-health.sh`、`web/server.js`、`web/app.js`、`web/index.html`、`web/style.css`、`docs/residential-proxy-guide.md`

## 1. 背景

主理人拿到的 Bright Data ISP 代理凭据是 `brd.superproxy.io:44445:brd-customer-…:密码` 这一行（供应商邮件原文格式）。现状：

| # | 事实 | 位置 |
|---|---|---|
| B1 | 面板提示"支持 host:port:user:pass"，但 `POST /api/residential/urls` 只接受 `socks5://` 开头，其它格式直接 400 | `web/server.js` ~:2280；`web/index.html` :315 |
| B2 | helper `parse_url` 已能拆 `socks5://u:p@h:port` 与 `h:port:u:p`，但没有"类型"概念，一律当 SOCKS5 | `server/residential-helper.sh` :90-112 |
| B3 | 44445 是 Bright Data 的 **HTTP** 代理端口（SOCKS5 是 22228）。中继只写 `socks` 出站，SOCKS5 打 HTTP 端口报 "invalid version in initial SOCKS5 response"，用户只看到"连接住宅代理失败" | 实测；helper :242/:282/:342 |
| B4 | 巡检脚本与面板体检探测都写死 `socks5h://` | `resi-health.sh` :37；`server.js` :538 |
| B5 | 凭据经 `spawnSync(helper, ["enable","--add", url])` 走 argv，服务器上 `ps` 可短暂看到（R3 只修了 curl 的 argv） | `server.js` :2284 |
| B6 | 添加框是一个裸输入 + "确认添加"，无解析预览、无进行中状态，错误只有一行文字 | `index.html` :306-318；`app.js` :619-628 |

## 2. 设计

### 2.1 数据契约

`residential-proxy.json` 的 `urls[]` 条目新增 `type`：`"socks5"` 或 `"http"`。**缺省（老条目）= socks5**，读侧一律 `.type // "socks5"`，不做文件迁移。顶层旧字段（host/port/username/password）若仍被写，同样带 `type`。

### 2.2 helper（`server/residential-helper.sh`）

- `parse_url` 接受四种输入，输出 `RESI_HOST/PORT/USER/PASS` 与 `RESI_TYPE`：
  - `socks5://user:pass@host:port` → `socks5`
  - `http://user:pass@host:port` → `http`
  - `host:port:user:pass` → `auto`
  - `user:pass@host:port`（无 scheme）→ `auto`
  - 首尾空白与成对引号去掉；密码取"最后一段之后的全部"（可含 `:`/`@` 以外任意字符；`user:pass@host:port` 形态以最后一个 `@` 切分）。
- `verify()` 改为按类型探测，返回时设置 `RESI_TYPE` 为实际可用类型：
  - `socks5` / `http`：只试该类型。
  - `auto`：先 SOCKS5（curl 配置 `proxy = "socks5h://host:port"`，10s），失败再 HTTP（`proxy = "http://host:port"` + 同样的 `proxy-user`，10s）。都失败才报错，错误文案区分"两种协议都连不上（凭据/端口）"与出口等于 VPS。
  - 凭据仍只走 `-K -` stdin。
- `--add`：写入 `type`（探测结果）。新增 `--add -`：从 stdin 读一行作为 URL（供 server.js 用，凭据不再进 argv）；`--add <url>` 保留给 CLI。`--remove` 按 host:port 匹配，scheme 无关。
- 中继写入（multi 模式）：每条上游按 `type` 生成出站：
  - socks5：现状 `{"type":"socks","version":"5",…}`
  - http：`{"type":"http","tag":"resi-N","server":…,"server_port":…,"username":…,"password":…}`（sing-box http 出站只走 TCP，与 R4 的 UDP 规则一致，无需改路由）
  - 生成结果必须在 1.13 / 1.14 / 1.15-alpha 三版 `sing-box check` 通过、无 deprecated。
- `status`/`domains`/`global`/`reapply` 语义不变；`status` 输出里带类型。

### 2.3 巡检（`server/resi-health.sh`）

成员读取改为 `select(.type=="socks" or .type=="http")`，探测 curl 配置按出站类型写 `socks5h://` 或 `http://`。其余（选择器切换、迟滞、跳过规则）不变。

### 2.4 面板后端（`web/server.js`）

- `POST /api/residential/urls`：不再要求 `socks5://` 前缀；`url` 为任意上述四种格式的原文，经 `helper enable --add -` 以 stdin 传入（`spawnSync` 的 `input`），超时 45s（两轮探测最坏 ~25s + 取 IP）。返回 `{success, exitIp, ispInfo, type}`（helper stdout 第三行输出探测到的类型）。
- `GET /api/residential/status` 的 urls 行带 `type`；显示用 URL 按类型拼（`socks5://` / `http://`），密码打码逻辑不变。
- `/api/residential/health`：成员取 `socks` 与 `http` 出站；`curlJsonViaSocks` 改名或加参数，按成员类型写 `socks5h://` 或 `http://` 代理行。
- `DELETE` 路径继续用 host:port 匹配。

### 2.5 面板前端（`web/app.js`、`web/index.html`、`web/style.css`）

- "添加 URL" 改为"添加代理"：一个粘贴框，placeholder `粘贴供应商给的代理：socks5://user:pass@host:port、http://…、host:port:user:pass`。
- 输入即解析（前端 `parseResiInput(text)`，与 helper 同规则），框下方实时预览四行：主机、端口、用户名、密码（打码，可点"显示"）、类型（SOCKS5 / HTTP / 自动探测）；解析失败显示具体原因（"缺少端口"、"端口不是数字"、"认不出格式，示例：…"）。
- 按钮"校验并添加"：点击后禁用并显示"正在连接上游校验，最长约 30 秒…"；成功 → toast、列表刷新、该行显示类型徽标与出口 IP；失败 → 现有错误框显示 helper 的错误原文（含 R8 的端口白名单提示）。
- 节点池列表每行加类型徽标（SOCKS5/HTTP）。
- 保留 R8 的粘性参数提示与指南链接。
- 样式沿用现有 token（`--hairline/--text-dim/--ivory-200`），预览区用等宽字体展示 host:port。

### 2.6 文档（`docs/residential-proxy-guide.md`，依据主理人提供的 Bright Data 官方文档 2026-09-10）

新增小节：
- **HTTP 与 SOCKS5 端口**：同一套凭据两种协议都可用；Bright Data HTTP/HTTPS 用 44445（旧 22225/33335），SOCKS5 固定 22228。B-UI 添加时自动识别；手动指定用 `http://` / `socks5://` 前缀。
- **目标必须是域名**：Bright Data 的 SOCKS5 只接受域名目标（`socks5h` 语义、远端解析），显式 IP 会被拒；HTTP 代理下 IP 目标会改由"超级代理"直接发出（出口不再是你的住宅/ISP IP）。B-UI 中继把目标域名原样交给上游，客户端侧请保持 TUN/嗅探开启，不要在本地把域名解析成 IP 再连。可在用户名加 `-dns-remote` 让解析发生在出口节点。
- **超级代理绕过（superproxy bypass）**：google/bing/youtube 等搜索引擎与第三方 IP 检测站可能被绕过，出口显示为 Bright Data 机房 IP 而非 peer；这不是配置错误。要"宁可失败也不绕过"可在用户名加 `-route_err-block`。B-UI 体检卡显示的 ippure/ipquery 结果以实际经上游请求为准（实测 ippure 未被绕过）。
- **Residential（非 ISP）zone 的 HTTPS 限制**：未完成 KYC 时 Residential/Mobile 网络的 HTTPS 会做证书拦截（需装 Bright Data 证书），B-UI 端到端 TLS 不会装第三方 CA → 会出现证书错误；建议用 ISP / Datacenter zone，或完成 KYC。
- **粘性与会话**：`-session-<id>` 同 ID 同 IP，空闲 7 分钟释放；`-const` 绑定 peer 不可用时报 502；专属池用 `-ip-x.x.x.x` 钉死（本 spec 主理人当前 zone 只有一个 IP）；`glob_` 前缀忽略来源 IP。
- **目标端口**：ISP/Datacenter 放开 >1024 与 80/443；Residential/Mobile 仅 8080/8443/5678/1962/2000/4443/4433/4430/4444/1969（与 R8 提示一致）。
- 供应商 IP 列表 CSV 就是 `host:port:username:password` 格式，可整行粘贴。

### 2.7 探测 URL 的考虑

巡检探测 `https://www.gstatic.com/generate_204` 与体检的 ippure/ipquery/ipinfo 都是域名目标，满足"只接受域名"的规则；gstatic 若被超级代理绕过仍返回 204，作为连通性探测可接受（不改）。

## 3. 不做

- `https://`（到代理本身加 TLS）上游、SOCKS4、需要 UDP 的上游。
- 老条目的文件迁移（读侧缺省 socks5 即可）。
- b-ui-cli 的住宅视图改动（v3.7）。

## 4. 验收

1. `bash -n` helper + resi-health；`node --check` server.js/app.js。
2. helper：四种格式解析（含引号/空白、密码含 `:`）；`verify` 在 curl 桩下：socks 通 → type socks5；socks 失败 http 通 → http；都失败 → 错误且不写状态；显式 `http://` 不试 socks。`--add -` 从 stdin 读、argv 不含凭据（`ps`/stub 记录 argv）。生成的中继含 http 出站，三版 sing-box check 通过。
3. resi-health：http 成员的 curl 配置为 `proxy = "http://…"`，socks 成员不变；混合池切换正常。
4. server.js：POST 三种格式均 200，`type` 回传；spawn 的 argv 不含 URL；health 对 http 成员用 http 代理行；status 行带 type。
5. app.js：`parseResiInput` 单测（vm）覆盖四种格式与错误分支；DOM 影子：输入即预览、按钮禁用态、成功/失败渲染、列表徽标。
6. 主理人线上验收：粘贴 `brd.superproxy.io:44445:…` 一行 → 预览显示自动探测 → 校验并添加 → 类型 HTTP、出口 168.158.161.12、体检卡显示住宅判定。
