# 住宅 IP 出站设计 (Residential IP Outbound)

**日期**: 2026-05-07
**目标版本**: v3.3.0
**状态**: 已通过设计评审，待用户最终确认

---

## 背景与目标

B-UI 当前部署的 Hysteria2 + Xray 代理直接使用 VPS 自身的机房 IP 出站，访问 Netflix / OpenAI / Twitter 等强 IP 风控站点容易被识别为数据中心 IP 并限流或拒绝。本设计为 B-UI 增加"住宅 IP 出站"功能：把代理服务器的所有出站流量转发到一个上游 SOCKS5 住宅代理，从而让目标站点看到的是住宅 ISP 分配的 IP。

**用户场景**: 用户从代理商处购买静态住宅 IP（包月不限流量套餐），获得 `socks5://user:pass@host:port` 形式的凭据，希望通过一键安装 / CLI / Web 看板任一入口配置后立即生效。

**非目标**:
- 不做多 SOCKS5 池负载均衡或自动切换。一次只用一个上游。
- 不做 HTTP/HTTPS 上游代理。仅 SOCKS5。
- 不做 Per-user 路由（用户 A 走住宅、用户 B 走直出）。所有 inbound 用户共享出站策略。
- 不做自定义 geosite 域名列表的 UI 编辑。分流站点列表内置在 helper 脚本中，改动需更新脚本。

---

## 关键决策（已与用户确认）

| 决策点 | 选择 | 理由 |
|---|---|---|
| 上游代理协议 | SOCKS5 (账密模式) | 云 VPS 的公网 IP 可能因重建/迁移变化，IP 白名单认证脆弱；账密认证一套凭据多机通用 |
| 凭据存储格式 | 标准 URI `socks5://user:pass@host:port` | RFC 3986 形式，Xray/Hysteria2/curl/sing-box 全部原生识别。安装器另接受三种厂商 CSV 格式但内部统一归一化为 URI |
| 分流策略 | 选择性：Netflix / OpenAI / Google / Claude 走住宅，其余直出 | 速度最快（大部分流量不走额外跳）；住宅带宽消耗小；这几个站点恰好是最需要住宅 IP 的场景 |
| 校验策略 | 强制连通性 + 出口 IP 校验，失败拒绝保存 | 防止错误凭据写入后服务重启全挂 |
| 配置入口 | 一键安装、CLI 主菜单、Web 看板（三处） | 一键安装和 CLI 为主，Web 看板补充 |

---

## 整体架构

引入**单一事实来源**模式：所有入口仅修改 `residential-proxy.json`，再调用一个共享 helper 脚本去重新生成 Xray/Hysteria 配置并 reload 服务。三个入口的逻辑不发散。

```
                       ┌─────────────────────────┐
   一键安装 (core.sh)──▶│                         │
   CLI 菜单 (b-ui-cli)─▶│ residential-helper.sh   │──▶ xray-config.json (jq atomic write)
   Web API (server.js)─▶│ (单一控制点)            │──▶ config.yaml (sed marker region)
                       │                         │──▶ systemctl reload xray + hysteria-server
                       └─────────────────────────┘
                                  ▲
                                  │
                       residential-proxy.json
                       { enabled, host, port, username, password,
                         lastVerifiedIp, lastVerifiedIspInfo,
                         lastVerifiedAt }
```

---

## 配置生成规范

### Xray (`/opt/b-ui/xray-config.json`)

现有配置（`core.sh:501-533`）：
```json
"outbounds": [{"protocol": "freedom", "tag": "direct"}],
"routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
```

启用住宅 IP 时，`outbounds` 数组替换为：

```json
[
  {
    "tag": "residential",
    "protocol": "socks",
    "settings": {
      "servers": [{
        "address": "<HOST>",
        "port": <PORT>,
        "users": [{ "user": "<USER>", "pass": "<PASS>", "level": 0 }]
      }]
    }
  },
  { "tag": "direct", "protocol": "freedom" }
]
```

`routing.rules` 替换为（保留原有 api 规则，插入分流规则，其余默认直出）：

```json
[
  { "type": "field", "inboundTag": ["api"], "outboundTag": "api" },
  { "type": "field", "ip": ["geoip:private"], "outboundTag": "direct" },
  {
    "type": "field",
    "domain": [
      "geosite:netflix",
      "geosite:openai",
      "geosite:google",
      "domain:anthropic.com",
      "domain:claude.ai"
    ],
    "outboundTag": "residential"
  },
  { "type": "field", "outboundTag": "direct", "network": "tcp,udp" }
]
```

规则顺序说明：Xray 按规则从上往下匹配，第一条命中即止。最后一条 `network: tcp,udp` 是兜底直出规则，未匹配到住宅域名的流量全部走 VPS 直出。

`geosite:openai` 在 v2fly domain-list-community 中覆盖 `openai.com`、`chatgpt.com`、`oaistatic.com` 等 OpenAI 旗下域名。
`geosite:google` 覆盖 `google.com`、`*.google.com`（含 `gemini.google.com`）、`googleapis.com`、`youtube.com` 等。
Anthropic/Claude 不在标准 geosite 中，需显式列出。

禁用时还原为现有原始形式（单 `freedom` 出站 + 仅 api 路由规则）。

**注**：原有 `inboundTag: api → outboundTag: api` 规则引用了不存在的 `api` outbound，但 Xray 的 API 流量（`dokodemo-door` inbound + `api.services`）由内部直接处理，此规则实际是无害冗余。本设计保持现状不动，仅追加新规则。

实现方式：使用 `jq` 做原子读改写：
```bash
jq '.outbounds = $new_outbounds | .routing.rules = $new_rules' \
   --argjson new_outbounds "$OUTBOUNDS_JSON" \
   --argjson new_rules "$RULES_JSON" \
   xray-config.json > xray-config.json.tmp && mv xray-config.json.tmp xray-config.json
```

依赖：`jq`（apt/yum 都有，加入 `core.sh` 的依赖安装清单）。

### Hysteria2 (`/opt/b-ui/config.yaml`)

在文件末尾维护一对锚点注释：

```yaml
# B-UI:RESIDENTIAL-START
# B-UI:RESIDENTIAL-END
```

启用住宅 IP 时，两锚点之间填入：

```yaml
outbounds:
  - name: residential
    type: socks5
    socks5:
      addr: <HOST>:<PORT>
      username: <USER>
      password: <PASS>
  - name: direct
    type: direct
acl:
  inline:
    # 内网直出（安全垫，必须排在最前）
    - direct(127.0.0.0/8)
    - direct(10.0.0.0/8)
    - direct(172.16.0.0/12)
    - direct(192.168.0.0/16)
    - direct(169.254.0.0/16)
    # Netflix
    - residential(*.netflix.com)
    - residential(netflix.com)
    - residential(*.nflxvideo.net)
    - residential(*.nflxso.net)
    # OpenAI / ChatGPT
    - residential(*.openai.com)
    - residential(openai.com)
    - residential(*.chatgpt.com)
    - residential(chatgpt.com)
    # Google / Gemini
    - residential(*.google.com)
    - residential(google.com)
    - residential(*.googleapis.com)
    - residential(*.gstatic.com)
    # Anthropic / Claude
    - residential(*.anthropic.com)
    - residential(anthropic.com)
    - residential(*.claude.ai)
    - residential(claude.ai)
    # 其余全部直出
    - direct(all)
```

Hysteria2 的 ACL 不依赖 geosite.dat，使用内置域名通配符匹配，安装时无需下载额外数据文件。`*.example.com` 匹配任意层级子域名，`example.com` 匹配根域名，两者需同时列出。

禁用时清空两锚点之间内容（保留锚点本身作为下次启用的 sed 替换目标）。

实现方式：`sed -i '/# B-UI:RESIDENTIAL-START/,/# B-UI:RESIDENTIAL-END/{//!d}'` 先清空，再 `sed -i '/# B-UI:RESIDENTIAL-START/r <(echo "$BLOCK")'` 插入。

锚点不存在时（升级而来的旧 config.yaml）由 helper 自动追加到文件末尾。

**安全垫说明**：内网 CIDR 走直出有两个原因：
1. Xray/Hysteria2 自身的 API 健康检查、masquerade 反代到 127.0.0.1 的 b-ui 看板、Hysteria 的 HTTP auth 回调（`http://127.0.0.1:8080/auth/hysteria`）必须走本机直连。
2. 防止内网 IP 通过住宅 SOCKS5 出站时绕一大圈或失败。

---

## 数据格式

### `/opt/b-ui/residential-proxy.json`

```json
{
  "enabled": true,
  "host": "us.proxy.example.com",
  "port": 1080,
  "username": "abc123",
  "password": "xyz789",
  "lastVerifiedIp": "173.45.12.34",
  "lastVerifiedIspInfo": "Comcast Cable Communications, US-CA-Los Angeles",
  "lastVerifiedAt": "2026-05-07T14:32:01Z"
}
```

文件权限 `600` (`chmod 600`)，仅 root 可读，因为含明文密码。
禁用时 `enabled: false`，其它字段可保留（方便用户重新启用时不用再粘贴）。

### 凭据格式归一化

接受四种输入格式，归一化为标准 URI 后入库：

| 输入 | 解析后 |
|---|---|
| `socks5://user:pass@host:1080` | URI 形式，直解 |
| `host:1080:user:pass` | CSV 形式（厂商常用） |
| `user:pass@host:1080` | 缺协议头 |
| `host:1080@user:pass` | 倒装格式 |

正则：`^(?:socks5://)?(?:([^:@/]+):([^@/]+)@)?([^:@/]+):([0-9]+)(?::([^:]+):(.+))?$` 配合分支判断。包含 `@` 的走 URI/缺协议路径，否则按冒号分隔取最后两段为账密。

---

## 校验逻辑

`verify_residential(host, port, user, pass)` 函数：

1. **连通性 + 出口 IP**：`curl -sS --max-time 8 --socks5-hostname "<user>:<pass>@<host>:<port>" https://api.ipify.org`
   - 返回非 200 或超时 → 失败，错误码 `CONN_FAIL`
2. **真伪检查**：拿到的出口 IP 与 VPS 自己公网 IP（`curl -sS https://api.ipify.org` 不走代理）对比
   - 一样 → 失败，错误码 `NOT_PROXIED`（说明 SOCKS5 没真正生效，或代理被透传）
3. **ISP 信息（可选）**：`curl -sS --max-time 5 https://ipinfo.io/<exit_ip>/json` 获取 `org` + `city, country`，存到 `lastVerifiedIspInfo`
   - 失败不影响保存，仅记录空字符串

校验通过 → 写 `residential-proxy.json` → 应用到 Xray/Hysteria → reload 服务。
**校验失败 → 不动现有任何文件，返回错误信息给调用方**。

---

## 三个入口的用户体验

### 入口 1：一键安装 (`server/core.sh`)

在安装流程末尾（所有服务启动并验证后）追加：

```
======================================================
是否配置住宅 IP 出站？(可让 Netflix/ChatGPT 等看到住宅 IP)
配置后所有出站流量将通过住宅 SOCKS5 代理转发。
======================================================
是否启用住宅 IP？(y/N): y
请粘贴住宅 IP 凭据 (支持 socks5://user:pass@host:port 等格式):
> socks5://abc:xyz@us.proxy.com:1080
正在校验连通性...
✓ 连通成功，出口 IP: 173.45.xx.xx
✓ ISP: Comcast Cable Communications, US-CA-Los Angeles
✓ 已应用到 Xray 和 Hysteria2，服务已重载
```

校验失败示例：
```
✗ 校验失败：连接超时（错误码 CONN_FAIL）
✗ 配置未保存。请检查凭据后通过 sudo b-ui 菜单或 Web 看板重新配置。
```

不阻塞安装继续完成。安装日志会记录用户最终是否启用住宅 IP。

### 入口 2：CLI 菜单 (`server/b-ui-cli.sh`)

在主菜单加 `12. 配置住宅 IP`（位置：BBR 选项后），dispatch 到子菜单：

```
================== 配置住宅 IP ==================
当前状态: 已启用 ✓
出口 IP: 173.45.12.34 (Comcast, US-CA)
最后校验: 2026-05-07 14:32:01

  1) 启用 / 修改凭据
  2) 禁用住宅 IP
  3) 重新校验连通性
  0) 返回主菜单
```

未启用时状态行显示 `当前状态: 未启用`，并隐藏出口 IP / 最后校验字段。

### 入口 3：Web 看板

`web/server.js` 新增三个 API（仿现有 `/api/masquerade` 模式）：

| 方法 | 路径 | 行为 |
|---|---|---|
| GET | `/api/residential` | 返回 `{ enabled, displayUrl, lastVerifiedIp, lastVerifiedIspInfo, lastVerifiedAt }`。`displayUrl` 已脱敏：`socks5://ab***@us.proxy.com:1080` |
| POST | `/api/residential` | Body `{ url }`。校验 → 应用 → reload。返回 `{ success, exitIp, ispInfo, error? }` |
| DELETE | `/api/residential` | 禁用并恢复 Xray/Hysteria 默认 outbounds |

所有三个 API shell-out 调 `residential-helper.sh`，避免在 Node.js 重写一遍 jq/sed 逻辑。

`web/app.js` + `web/index.html` 新增"住宅 IP 出站"卡片，仿现有 `m-masq` 弹窗风格：
- 启用开关 + 凭据输入框（`<input type="password">` 显示脱敏值）
- "保存"按钮（保存 = 校验 + 应用 + reload，失败显示 toast 错误）
- 状态行：`当前出口 IP: 173.45.xx.xx · ISP: Comcast · 最后校验: 5 分钟前`

---

## 文件改动清单

### 新增（2 个）

| 文件 | 行数估计 | 用途 |
|---|---|---|
| `server/residential-helper.sh` | ~250 | 共用助手脚本：parse/verify/apply-xray/apply-hysteria/reload/clear |
| `residential-proxy.json` | 运行时生成 | 配置存储，与 `users.json` 等同目录 |

### 修改（9 个）

| 文件 | 改动概要 |
|---|---|
| `install.sh` | 确保 `server/residential-helper.sh` 在下载文件清单中 |
| `server/core.sh` | (a) 依赖安装加 `jq`；(b) Hysteria 配置生成时追加锚点注释；(c) 安装末尾新增住宅 IP 询问交互 |
| `server/b-ui-cli.sh` | 主菜单加 `12.` 入口 + 子菜单函数 + dispatcher case |
| `server/update.sh` | 升级后若 `residential-proxy.json` 存在且 `enabled=true`，重新调 helper 应用一次（防止配置被升级覆盖丢失） |
| `web/server.js` | 三个 API 端点 + 路径常量 `residentialConfig` |
| `web/app.js` | 住宅 IP 卡片 UI 逻辑 + API 调用 |
| `web/index.html` | 加住宅 IP 卡片 / 弹窗 DOM |
| `version.json` | bump `v3.2.7 → v3.3.0` + changelog 条目 |
| `CLAUDE.md` | 在 "Key Design Patterns" 补 helper 脚本和 sed 锚点约定 |

---

## 错误处理与回滚

| 故障情景 | 处理 |
|---|---|
| 校验阶段 curl 超时 / 凭据错误 | 不动任何配置文件，返回错误码给调用方，UI 显示错误 |
| jq 修改 xray-config.json 中途失败 | 因为用 `tmp + mv` 原子替换，失败时原文件不变；helper 检测 jq 退出码非 0 时直接 abort |
| sed 修改 config.yaml 中途失败 | 修改前先 `cp config.yaml config.yaml.bak.<ts>`；sed 失败时 `mv` 还原备份 |
| `systemctl reload` 失败 | helper 记录到 stderr，调用方决定是否回滚（看板显示警告，但配置文件已写） |
| 升级后 xray-config.json 被覆盖 | `update.sh` 在升级末尾自动重新调 helper 应用一次（idempotent） |

---

## 兼容性 / 升级路径

- **新装用户**：v3.3.0 安装时多一个询问步骤，不强制配置，向后兼容。
- **老用户升级**：`update.sh` 在升级时不主动询问。但 b-ui 看板和 CLI 菜单会出现新选项。`config.yaml` 升级时由 `update.sh` 检测并追加锚点注释（如不存在）。
- **降级**：v3.3.0 → v3.2.x 不在支持范围内（用户需要手动清理 `residential-proxy.json` 和 Xray/Hysteria 配置中的住宅 outbound 块）。

---

## 性能影响（用户须知）

选择性分流后，只有住宅规则命中的流量（Netflix / OpenAI / Google / Claude）走额外一跳，其余（YouTube 下载、普通网页、大文件）全部直出 VPS 跑满速。实测住宅流量占比通常 < 20%。

住宅规则匹配的流量链路：`客户端 → VPS → 住宅代理 → 目标站`。

| 场景 | 命中住宅规则时额外延迟 | 其余流量 |
|---|---|---|
| VPS 与住宅代理同区域（如同在美西） | +10~30ms | 无影响，直出 |
| VPS 香港/日本，住宅在美国 | +120~180ms | 无影响，直出 |
| VPS 美西，住宅在美东 | +60~80ms | 无影响，直出 |

**建议**：VPS 选址尽量贴近住宅代理 POP；Hysteria2 (QUIC + BBR) 对额外延迟容忍度优于 VLESS-Reality (TCP)。

这部分会在 b-ui 看板的住宅 IP 卡片下方放一个简短说明文案，一键安装询问时也带一行提示。

---

## 已知限制与未来扩展

- **本期不做**：自定义 geosite 域名列表的 UI、多 SOCKS5 上游池、Per-user 路由、HTTP/HTTPS 上游代理、自动测速选最佳 POP。
- **未来扩展站点列表**：Xray 端只需在 `domain[]` 数组加 geosite 标签；Hysteria2 端在 ACL inline 规则中追加域名行。两处改动均在 `residential-helper.sh` 同一函数中，不影响其它组件。

---

## 验收标准

实施完成后，应满足以下所有：

1. 一键安装时回答 Y 并粘贴有效凭据 → 安装结束后访问 `netflix.com` 看到的出口 IP 是住宅 IP。
2. 一键安装时粘贴**错误**凭据 → 校验失败提示，安装继续完成，住宅 IP 未启用。
3. 启用住宅 IP 后访问 `baidu.com`（不在分流列表）→ 出口 IP 仍为 VPS 机房 IP（直出）。
4. 启用住宅 IP 后访问 `openai.com` / `claude.ai` / `netflix.com` / `gemini.google.com` → 出口 IP 为住宅 IP。
5. 通过 CLI 菜单启用 → 服务自动 reload，新流量立即按分流规则生效。
6. 通过 b-ui 看板启用 → API 返回成功 + 出口 IP，刷新页面状态正确显示。
7. CLI 禁用住宅 IP → Xray/Hysteria 配置恢复为安装时的默认（仅 freedom 出站，所有流量直出）。
8. 通过 `bash -n server/residential-helper.sh` 等语法检查。
9. 升级 v3.3.0 后再升级到下一个 patch 版本，已启用的住宅 IP 分流规则依然生效（idempotent 验证）。
