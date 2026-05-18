<p align="center">
  <img src="web/logo.jpg" alt="B-UI Logo" width="500">
</p>

# B-UI

轻量级 Hysteria2 + Xray 多协议代理一键部署工具，内置 Web 管理面板与全功能流量管理。

**当前版本**: v3.5.13

---

## 最新更新

### v3.5.13 — 伪装设置 Reality 四方分裂修复
- 🩺 **故障**：改伪装站后客户端 VLESS Reality 报 `received real certificate (potential MITM)`，延迟测试 -1 / OperationCancelled（服务端日志却显示 Reality 仍在成功代理真实流量）
- 🔍 **根因**：伪装值在 4 处不一致——`masquerade.json` / xray `vless-direct` / xray `vless-residential` / 订阅下发
- 🔧 **Bug A**：`server.js` 伪装 handler 用 `.find()` 只改第一个 reality inbound，v3.5 双 inbound 架构下 `vless-residential` 永远停在旧伪装域名 → 改 `.filter()` 遍历全部 reality inbound
- 🔧 **Bug B**：三个订阅生成器 `user.sni || cfg.sni` 优先级反了，`user.sni` 是建用户时固化的旧拷贝、改伪装从不回写 → 翻转为 `cfg.sni || user.sni`，sni 以服务端实时 xray 配置为唯一可信源
- ✅ 本地沙箱 + bwg-temp 端到端实测：四节点全通（Reality 180/167ms、HY2 隧道 200/204），改伪装即时对所有用户订阅生效
- ⚠️ 改伪装后该机用户需重拉一次订阅（Reality sni 变更；HY2 sni=证书域不受影响）

### v3.5.12 — Web 面板苹果设计风格重做
- 🎨 **完全重写 `style.css`**：语义化 token 体系，淡金色主调 + 勃艮第辅助 + 米白画布；修掉旧文件两套设计系统混用导致的未定义变量 bug（dashboard nav 被渲染成深色等）
- 🪟 **iOS 17 玻璃材质**：半透明本体 + blur(24-40px) saturate(200%) + 顶缘 specular 高光 + 底缘暗边 + 双层投影；nav / 卡片 / modal / login / toast 全量应用，modal 由实色改为可透出背景的玻璃 sheet
- ✨ **苹果 spring 动画**：modal/toast 入场 overshoot 回弹、按钮 enter 慢 exit 快、iOS 开关拨杆按压拉长；`prefers-reduced-motion` 全局兜底
- 📱 桌面 / 移动 / modal Playwright 实测通过；纯前端改动，不涉及服务端逻辑

### v3.5.1 ~ v3.5.11 — 共 11 个版本
- 📜 详见 `version.json` changelog：sing-box / Clash 订阅生成器补住宅出口 + 住宅域名分流 (v3.5.11)、update.sh 幂等自愈「打得死」迁移块 (v3.5.10)、xray routing 残留 v3.4 无条件 relay 规则修复 (v3.5.9)、订阅 host 切回域名 cfg.domain (v3.5.8)、hy2-direct config.yaml 残留 outbounds 修复 (v3.5.7)、单协议用户住宅版支持 (v3.5.6) 等

### v3.5.0 — 双实例架构 + 4 订阅 URL + 多住宅 URL 池 + Global 模式
- 🏗 **架构重构**：xray 双 inbound (vless-direct :10001 / vless-residential :10002)；hysteria 双实例 (direct :10000+20000-30000 / residential :40000+41000-50000)；direct 路径完全绕开 sing-box 中转
- 🛜 **4 订阅 URL**：每用户输出 `-Reality直连 / -Reality住宅 / -HY2直连 / -HY2住宅`，订阅 host 用 IP literal 防 client DNS 投毒
- 🏠 **多住宅 URL urltest 池**：30s ping 自动选最优住宅；池空 fallback 直连
- 🌐 **Global toggle**：OFF 域名分流（默认）/ ON 全走住宅
- 🛡 **多层 DoH 防投毒**：hy2 resolver / xray dns / client predefined / 静态 /etc/resolv.conf + chattr +i
- 🔄 **老服务器自动迁移**：update.sh 5 幂等块（hy2-residential unit + config-residential.yaml + xray jq 转换 + DoH + 防火墙），老订阅 URL 继续有效

### v3.4.44 — UDP :7844 兜底 process_name race
- 🩺 v3.4.43 部署后 baiyi 仍偶发一批 DNS unpack ERROR（同 session id 17 秒内 10 个），cloudflared rule 没生效
- 🔍 sing-box `process_name` 依赖 /proc 反查，UDP socket race 时查不到 → 整 session miss 规则
- 🔧 加 `{ network: udp, port: [7844], outbound: direct-out }` 兜底，端口规则不查 /proc 永不 race；TUN_SCHEMA_VERSION 4→5

### v3.4.43 — 排除 cloudflared 被 sniff 误判为 DNS
- 🩺 baiyi 在 v3.4.42 修复后仍每 30s 一批 `router: process DNS packet: bad rdata / buffer size too small`
- 🔍 tcpdump tun0 抓到 `172.19.0.1 > 198.41.192.107:7844 UDP 41 bytes` —— cloudflared 的 QUIC tunnel 包被 sing-box `sniff` 协议嗅探**误判为 DNS** → `hijack-dns` 解析失败 → 噪音 ERROR
- 🔧 修复：`route.rules` 第一条加 `{ process_name: ["cloudflared"], outbound: "direct-out" }`，cloudflared 完全跳过 sniff；TUN_SCHEMA_VERSION 3→4 自动重建
- 💡 副产品：cloudflared 隧道不再双层封装，直走本机网络少一跳

### v3.4.42 — 客户端路由 keyword 子串误判 + 服务端 hy2 nft 孤儿规则
- 🩺 **故障 A**：baiyi 30min 内 47 个 `direct-out: dial 23.62.46.219:443: i/o timeout`。`domain_keyword: [tencent, qq, alibaba, baidu, ...]` 子串匹配误中 `*-akamai-cdn` / `qqmusic-akamai-edge` 等海外 CDN 域名 → 强制本地直连 Akamai 5s 超时
- 🔧 **修复 A**：`TUN_SCHEMA_VERSION` 2→3 触发自动重建；`domain_keyword` 改为 `domain_suffix` 精确列表（40+ 项根域 + `.cn`）；加 akamai/fastly/cloudfront keyword 强制走代理兜底；DNS rules 同步国内 suffix → local-dns
- 🩺 **故障 B**：bwg-tizi `nft list ruleset` 看到 `hysteria_e6fe45cb` ip6 表同 chain **重复 2 条** redirect 规则。hy2 SIGKILL 跳过 closer chain → 规则残留 → systemd 重启 add 规则到旧 chain
- 🔧 **修复 B**：override.conf 加 `ExecStartPre=-/opt/b-ui/hy2-nft-cleanup.sh` 启动前清扫所有 `hysteria_*` 表；加 `TimeoutStopSec=15`；update.sh 加缺失检测自动重写

### v3.4.41 — 防止孤儿 b-ui CLI 进程把 hy2 keepalive 吃垮
- 🩺 **故障**：bwg-tizi 实例排查到一条遗留管道 `bash -x b-ui-cli.sh </dev/null | grep -B2 'unknown' | head -20` —— SSH 断开后被 init 收养，grep 从未匹配 'unknown'、head 永远等不到 20 行，bash -x 持续吐 trace 死锁。3 天 15 小时累积把 1 vCPU VPS 拖到 sys 50% / idle 14%，hysteria QUIC 来不及发 keepalive，客户端 sing-box 看到 `outbound/hysteria2[proxy]: timeout: no recent network activity`
- 🔧 **update.sh `cleanup_orphan_cli_processes`**：停服阶段按 PGID SIGTERM→KILL 清掉 PPID==1 且 cmdline 含 b-ui CLI 的孤儿；主动跳过当前 update.sh 所在 PGID（防自杀）和其他活跃 sudo b-ui session
- 🔧 **b-ui-cli.sh TTY 守卫**：交互菜单 `while true` 前加 `[[ -t 0 ]]` 检查，无 TTY 直接 exit 1 不进死循环 read；`b-ui <子命令>` 非交互入口不受影响（cron 正常）
- 📜 **v3.4.11 ~ v3.4.40 共 31 个版本**详见 `version.json` changelog：DNS UDP→DoH (v3.4.40)、紧急 hotfix（cmd_harden_ssh / first_run_setup / cgroup 限额）、Web 安全验证（POST/PUT users 白名单）、bui-c smoke test、住宅 IP ip-api 分流、test_proxy DNS 假阳性等

### v3.4.10 — UI 简化 + 自愈强化
- 🎯 **CLI 菜单回到数字直选**：实测下来 gum 箭头选择体验比直接敲数字慢，重新设计两栏紧凑布局，纯 bash + ANSI 渲染零依赖
- 🔒 **入向 SSH 在 TUN 模式下保活**：路由规则补 `source_port [22, 2222] → direct`，sshd 回包不再被 strict_route 塞进隧道
- 🔧 **配置文件原子写入**：客户端 `singbox-tun.json` 和服务端下载流程都改为 `.tmp` + 校验 + `mv`，半截下载/中断不会破坏现有配置
- ⚡ **TUN 路由模板自动同步**：客户端脚本升级后下次 TUN 重启自动应用新规则（`TUN_SCHEMA_VERSION` sidecar 比对），新增 `bui-c reload-tun` 子命令立即应用

### v3.4.x — 维护期修复（v3.4.2 ~ v3.4.9）
- 🐛 **紧急修复 update.sh 截断 config.yaml 的严重 bug**：旧版 sed 范围删除找不到结束锚点会一路删到 EOF，把 auth/sniff/masquerade 全冲掉（v3.4.4）
- 🛡 **update.sh 新增 config.yaml 完整性兜底**：检查关键段缺失则自动 `repair_hysteria_config` 重建（v3.4.6）
- 🔄 **客户端 TUN 配置自愈**：`ensure_tun_config_ready` 检测配置缺失/字段空/指向旧节点时自动重新生成（v3.4.7）

### v3.4.1 — sing-box 1.13 兼容性
- 🔧 **DNS server 新格式**：迁移到 `type: udp + server` 字段（旧 `address: udp://` 已移除）
- 🔧 **inbound sniff 移除**：改为 route rule action `{action: sniff}`
- 🔧 **route.default_domain_resolver**：sing-box 1.12+ 必需字段
- 🔧 **修复 b-ui-relay 启动失败**：彻底解决 sing-box 1.13 启动报错

### v3.4.0 — 架构重构：sing-box 统一控制平面
- 🌐 **路由集中**：所有路由决策和 DNS 解析由 sing-box 统一负责
- 🚀 **简化转发链**：Xray/Hysteria2 全部流量直接转 sing-box，不再各自维护路由规则
- 🛡 **解决 hairpin 回环**：自动检测服务器公网 IP，加入直连例外，修复 TUN 模式下 SSH 到服务器自身超时

### v3.3.x — 住宅 IP 出站功能
- 🏠 **住宅 SOCKS5 中继**：OpenAI/Google/Claude/ping0 等指定域名走住宅代理，其余直出 VPS
- ⚡ **sing-box 永久中继**：127.0.0.1:2080 常驻服务，切换住宅代理无需重启 Xray/Hysteria2
- 🎛 **三入口配置**：一键安装向导 / CLI 菜单 / Web 看板
- ✅ **硬失败校验**：开启前验证凭据，出口 IP ≠ VPS 才放行

### v3.2.x — 安装与稳定性加固
- 🔐 Caddy 证书自动同步给 Hysteria2，修复启动失败
- 🔒 SSH 安全加固：检测公钥后自动关闭密码登录
- 🛡 UFW 兼容、系统代理自动配置、服务控制菜单重构

### v3.1.0 — 内核代理下载
- 🌐 服务端自动从 GitHub 同步最新内核（每 6h），客户端优先从服务端拉取

---

## 功能特性

### 服务端 (Core)
- **多协议支持**: Hysteria2 / VLESS-Reality / VLESS-WS-TLS
- **用户管理**: Web 面板可视化管理，支持多用户、流量统计、在线状态监控
- **访问控制**: 用户时长限制、总流量/月度流量限制、用户级别限速
- **住宅 IP 出站**: sing-box 中继架构，指定域名（OpenAI/Google/Claude/ping0 等）走住宅 SOCKS5，其余直出 VPS
- **内核代理**: 自动缓存 GitHub 最新内核二进制，供客户端国内环境下载
- **自动维护**: Caddy 自动 HTTPS 证书、证书同步、自动更新、BBR 优化
- **便捷分享**: 二维码 (v2rayN/Shadowrocket)、sing-box/Clash 订阅

### 客户端 (Client)
- **统一导入**: 粘贴链接即可，自动识别 Hysteria2/VLESS 链接、订阅地址、批量
- **多模式**: 全局 TUN 代理 (Hysteria2 + VLESS-Reality)、SSH 连接保护
- **服务端优先更新**: 内核从服务端下载，解决 GitHub 不可达问题
- **服务控制**: 实时状态显示，一键启停/重启/查看日志

---

## 服务端部署

**系统要求**: Ubuntu / Debian / CentOS / RHEL (需 root 权限)

### 一键安装
```bash
sudo -i
bash <(curl -fsSL "https://raw.githubusercontent.com/Buxiulei/b-ui/main/install.sh?$(date +%s)")
```
*国内镜像*: `bash <(curl -fsSL "https://raw.githack.com/Buxiulei/b-ui/main/install.sh?$(date +%s)")`

### 管理与更新
安装后可通过终端命令 `b-ui` 进行管理：
- 查看配置 / 重启服务 / 查看日志
- 修改密码 / 开启 BBR / 检查更新

---

## 客户端部署

> **提示**: 客户端一键安装命令需要从服务端 Web 面板获取，包含服务端地址和自动配置信息。

### 使用说明
- **启动菜单**: 输入 `bui-c`
- **代理端口**: SOCKS5 (1080) / HTTP (8080)
- **测试连接**: `curl --socks5 127.0.0.1:1080 https://www.google.com`

### 客户端菜单一览

v3.4.10 起为数字两栏布局（纯 bash + ANSI 渲染，零依赖）：

```
  ━━━━━  B-UI 客户端  · v3.4.10  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  节点: ● bwg-tizi  Hysteria2  •  SOCKS5 :1080  HTTP :8080  •  TUN: ● running

     [1] 切换节点        [2] 停止 TUN
     [3] 导入节点        [4] 服务控制
     [5] 高级设置        [6] 一键更新
     [7] 卸载            [0] 退出

  ▸ 选择 [0-7]:
```

直接键入数字 + 回车即可触发对应操作；服务端 `b-ui` 看板同样为两栏数字布局，覆盖 12 项功能。

### 内核更新策略

```
版本检测: 服务端 API + GitHub API (并行取最新)
下载优先级:
  1. 服务端 /packages/ (国内可达)
  2. GitHub Releases (fallback)
```

---

## 住宅 IP 出站架构

```
客户端 VPN 流量
    │
    ▼
Xray / Hysteria2  (无路由逻辑，全部转发)
    │
    ▼
sing-box 中继 (127.0.0.1:2080)
    ├─ 私有 IP / 服务器自身 IP → 直出 VPS
    ├─ 关键词域名 (openai/chatgpt/google/anthropic/claude/...) → 住宅 SOCKS5
    └─ 其余 → 直出 VPS
```

- 默认分流关键词：`openai chatgpt google googleapis gstatic anthropic claude ping0 grok tiktok`
- 配置文件：`/opt/b-ui/residential-proxy.json`（chmod 600）
- 控制脚本：`/opt/b-ui/residential-helper.sh {setup|enable|disable|status|reapply|set-domains}`
- 入口：一键安装向导 / `b-ui` CLI 菜单 / Web 看板🏠

---

## API 与端口配置

### 端口列表
| 端口 | 协议 | 用途 |
|------|------|------|
| 80/443 | TCP | Caddy (Web 面板 HTTPS + 证书申请) |
| 10000 | UDP | Hysteria2 |
| 10001 | TCP | VLESS-Reality |
| 10002 | TCP | VLESS-WS-TLS |
| 20000-30000* | UDP | 端口跳跃（可选） |

### API 端点
| 端点 | 用途 |
|------|------|
| `/api/manage` | 用户增删改查 |
| `/api/kernel-versions` | 服务端内核版本 |
| `/api/kernel-downloads` | 可下载的内核文件列表 |
| `/api/subscription/:user` | sing-box 订阅配置 |
| `/api/clash/:user` | Clash Meta 订阅配置 |
| `/packages/` | 内核二进制文件下载 |

---

## 文件结构

- `/opt/b-ui/`: 核心数据目录 (配置、用户数据)
- `/opt/b-ui/certs/`: SSL 证书 (Caddy 自动同步)
- `/opt/b-ui/packages/`: 内核二进制缓存 (自动同步)
- `/opt/b-ui/sing-box`: 出站中继内核 (住宅 IP 分流)
- `/opt/b-ui/singbox-relay.json`: sing-box 中继配置
- `/opt/b-ui/residential-proxy.json`: 住宅代理凭据 (chmod 600)
- `/opt/b-ui/residential-helper.sh`: 住宅代理控制脚本
- `/usr/local/bin/b-ui`: 服务端命令
- `/usr/local/bin/bui-c`: 客户端命令

---

## License
MIT
