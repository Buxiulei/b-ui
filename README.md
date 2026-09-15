<p align="center">
  <img src="web/logo.jpg" alt="B-UI Logo" width="500">
</p>

# B-UI

轻量级 Hysteria2 + Xray 多协议代理一键部署工具，内置 Web 管理面板与全功能流量管理。

**当前版本**: v4.0.0（Rust 单二进制；版本号唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`）

---

## 最新更新

完整更新日志见 [CHANGELOG.md](CHANGELOG.md)。v4.0.0 起本节只保留 v3 的历史条目，新版本不再往这里追加。

### v3.5.14 ~ v3.5.18 — 安装健壮性 + 多用户/高并发硬化 + 隐私收口
- 🧯 **端口跳跃孤儿链崩溃循环根治** (v3.5.14/16)：SIGKILL/OOM 残留 `HYSTERIA-PR` 链 / `hysteria_*` nft 表 → 下次启动 `Chain already exists` FATAL 崩溃循环。新增 `hy2-portjump-cleanup.sh` 作为两实例 `ExecStartPre`，按 base 端口 + 跳跃端口段**双重定位**（连"建了空链、还没加 redirect"的残留也清），只清本实例、兼容 iptables/nft 两后端
- 🔁 **重启存活 + 双实例自愈** (v3.5.14)：补 `enable hysteria-residential`（此前重启后住宅 HY2 不恢复）；watchdog / cert-sync / cert-check 全覆盖 direct + residential
- 🚦 **多用户容量** (v3.5.14)：`nf_conntrack_max` 8192→按内存 131072+ + hashsize 开机预载；保留所有代理/跳跃端口避免临时端口抢占；`enable_bbr` 接入；`b-ui-admin` 加 `MemoryMax` 防 OOM 误杀代理进程
- 🛠 **安装中断根治** (v3.5.14)：`install.sh` 的 `set -e` + 未保护的 `systemctl enable --now` 遇 Caddy ACME 超时会中途夭折（漏装 watchdog/cron/住宅 enable 等）→ 全部加 `|| true`；`configure_cron_tasks` 统一三条 cron（更新/内核/证书健康检查），消除引用不存在脚本的 dead code
- ⚡ **高并发硬化** (v3.5.15)：hy2 两实例 auth 从 `http`（每条连接回调面板、重连风暴下成 SPOF）迁到本地 `userpass`，同一订阅几十客户端并发更稳
- 🔒 **二维码本地生成** (v3.5.17)：内置 `qrcode.min.js` 浏览器本地出码，不再把节点链接（含域名/uuid/密码）发给境外 `api.qrserver.com`（隐私 + 国内可用性）；面板复制链接 sni/端口对齐订阅
- 🩹 **新增静态文件投递自愈** (v3.5.18)：`apply_systemd_configs` 扫 `index.html` 引用却本地缺失的 web 资源自动从 GitHub 补下，消除新增前端文件的投递盲区
- 📜 各版本详细根因与验证见 git 历史：`git log --diff-filter=D -1 -- version.json` 找到删除提交，再 `git show <sha>^:version.json`（该文件已于 v4.0.0 删除）

### v3.5.13 — 伪装设置 Reality 四方分裂修复
- 🩺 **故障**：改伪装站后客户端 VLESS Reality 报 `received real certificate (potential MITM)`，延迟测试 -1 / OperationCancelled（服务端日志却显示 Reality 仍在成功代理真实流量）
- 🔍 **根因**：伪装值在 4 处不一致——`masquerade.json` / xray `vless-direct` / xray `vless-residential` / 订阅下发
- 🔧 **Bug A**：`server.js` 伪装 handler 用 `.find()` 只改第一个 reality inbound，v3.5 双 inbound 架构下 `vless-residential` 永远停在旧伪装域名 → 改 `.filter()` 遍历全部 reality inbound
- 🔧 **Bug B**：三个订阅生成器 `user.sni || cfg.sni` 优先级反了，`user.sni` 是建用户时固化的旧拷贝、改伪装从不回写 → 翻转为 `cfg.sni || user.sni`，sni 以服务端实时 xray 配置为唯一可信源
- ✅ 本地沙箱 + 临时测试机端到端实测：四节点全通（Reality 180/167ms、HY2 隧道 200/204），改伪装即时对所有用户订阅生效
- ⚠️ 改伪装后该机用户需重拉一次订阅（Reality sni 变更；HY2 sni=证书域不受影响）

### v3.5.12 — Web 面板苹果设计风格重做
- 🎨 **完全重写 `style.css`**：语义化 token 体系，淡金色主调 + 勃艮第辅助 + 米白画布；修掉旧文件两套设计系统混用导致的未定义变量 bug（dashboard nav 被渲染成深色等）
- 🪟 **iOS 17 玻璃材质**：半透明本体 + blur(24-40px) saturate(200%) + 顶缘 specular 高光 + 底缘暗边 + 双层投影；nav / 卡片 / modal / login / toast 全量应用，modal 由实色改为可透出背景的玻璃 sheet
- ✨ **苹果 spring 动画**：modal/toast 入场 overshoot 回弹、按钮 enter 慢 exit 快、iOS 开关拨杆按压拉长；`prefers-reduced-motion` 全局兜底
- 📱 桌面 / 移动 / modal Playwright 实测通过；纯前端改动，不涉及服务端逻辑

### v3.5.1 ~ v3.5.11 — 共 11 个版本
- 📜 详见 git 历史里的 changelog（`git log --diff-filter=D -1 -- version.json` 找到删除提交，再 `git show <sha>^:version.json`）：sing-box / Clash 订阅生成器补住宅出口 + 住宅域名分流 (v3.5.11)、update.sh 幂等自愈「打得死」迁移块 (v3.5.10)、xray routing 残留 v3.4 无条件 relay 规则修复 (v3.5.9)、订阅 host 切回域名 cfg.domain (v3.5.8)、hy2-direct config.yaml 残留 outbounds 修复 (v3.5.7)、单协议用户住宅版支持 (v3.5.6) 等

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
- 📜 **v3.4.11 ~ v3.4.40 共 31 个版本**详见 git 历史里的 changelog（`git log --diff-filter=D -1 -- version.json` 找到删除提交，再 `git show <sha>^:version.json`）：DNS UDP→DoH (v3.4.40)、紧急 hotfix（cmd_harden_ssh / first_run_setup / cgroup 限额）、Web 安全验证（POST/PUT users 白名单）、bui-c smoke test、住宅 IP ip-api 分流、test_proxy DNS 假阳性等

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
- **单二进制控制面**: `bui` 一个静态二进制装机、对账、跑面板、升级；协议内核（Hysteria2 / Xray / sing-box / Caddy）仍是上游原版
- **多协议支持**: Hysteria2（直连 + 住宅两实例）/ VLESS-Reality（直连 + 住宅两入站）
- **用户管理**: Web 面板可视化管理，支持多用户、流量统计、在线状态监控
- **访问控制**: 用户时长限制、总流量/月度流量限制；加用户与到期在建连时生效（Hysteria2 `auth.type: command`），不重启内核
- **住宅 IP 出站**: sing-box 中继架构，指定域名（OpenAI/Google/Claude/ping0 等）走住宅上游，其余直出 VPS；内置上游体检与自动黑名单
- **期望态对账**: `/opt/b-ui/state.json` 是唯一真源，装机与升级同一条代码路，非受管的手工改动只报不改
- **自动维护**: Caddy 自动 HTTPS 证书、证书同步、watchdog、每日自检全部收进守护进程（没有 cron、没有独立 timer）
- **便捷分享**: 二维码 (v2rayN/Shadowrocket)、sing-box/Clash 订阅

### 客户端 (Client)
- **统一导入**: 粘贴链接即可，自动识别 Hysteria2/VLESS 链接、订阅地址、批量
- **单引擎双模式**: sing-box（≤ 1.14）跑 SOCKS/HTTP 混合入口或全局 TUN，切模式只重渲染配置 + 重启一个单元；SSH 连接保护
- **自愈**: `bui-c.timer` 每分钟跑 `bui-c check`，失败按 1/2/4 分钟退避重启
- **更新来源**: 面板 `/packages/` → GitHub Releases → 镜像前缀，解决 GitHub 不可达问题
- **服务控制**: 实时状态显示，一键启停/重启/查看日志
- 🪟 **v2rayN TUN + IPv6**：服务端无 IPv6 出口时的推荐设置见 [docs/v2rayn-tun-ipv6.md](docs/v2rayn-tun-ipv6.md)

---

## 服务端部署

**系统要求**: Ubuntu / Debian / CentOS / RHEL，x86_64 或 aarch64，systemd，root 权限。

### 一键安装（v4）

新服务器上一行命令，跑起来后问几个关键信息（**只有面板域名必填**，其余每题**直接回车**用默认值）：

```bash
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh | bash
```

问答顺序与默认值：

| 问 | 默认值（回车即用） |
|---|---|
| 面板域名 | **无默认，必填**（空回车重问，3 次后报错退出，不会装一半） |
| 面板管理员密码 | 随机 16 位十六进制，**提示里直接显示**，回车即用 |
| Hysteria2 直连端口 | `10000` |
| REALITY 伪装站 | `www.bing.com:443` |
| 第一个用户名 | `user1`（HY2 密码与 UUID 随机生成，权益默认全开：两协议 + 直连 + 住宅、不限期不限量） |
| 节点名 | 主机名 |
| 公网 IP | 自动探测值 |

先给好的项不会再问那一项：`--domain`（域名那一问）、`--port`（HY2 端口那一问）、`--admin-password-stdin`（面板密码那一问）、`--answers` 文件里**显式写了**的键（`domain` / `node_name` / `public_ip` / `masquerade` / `ports` 各对应那一问）。

**一个问题都不问**的三种写法：`--yes` / `--non-interactive`、`BUI_DOMAIN=<域名>`（环境变量那条写法的语义就是无人值守）、`--import-v3`（沿用 v3 的值）。

```bash
# 域名先给好，密码/端口/伪装站/首用户照问
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh | bash -s -- --domain panel.example.com

# 无人值守：一个问题都不问，面板密码随机生成并在最后一屏打印一次（下面两行等价）
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh | bash -s -- --domain panel.example.com --yes
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh | BUI_DOMAIN=panel.example.com bash

# 无人值守 + 自己指定面板密码（凭据不进命令行，从 stdin 读）
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh \
  | bash -s -- --domain panel.example.com --yes --admin-password-stdin < /root/panel-password.txt
```

没有终端可问（CI、`/dev/tty` 也开不了）时同样全用默认值；不问的这几种情形下域名仍然缺不得——没给就打印用法并以非 0 退出，**不会装一半**。域名之外的事——架构、发行版与包管理器、公网 IP、SELinux、时间同步、关键端口占用、域名解析核对、IPv6 出口、v3 迁移、防火墙——全部自动探测处理，装完打印一张「环境」表、一张自检 PASS/FAIL 表，以及面板地址、一次性管理员密码、第一个用户名与他那三条订阅地址（`/api/sub`、`/api/subscription`、`/api/clash`，末段是他的随机订阅 token，见[订阅链接与 token](#订阅链接与-token)）。

全新服务器上首张证书要等 Caddy 签到，自检会先有界等待（最多 2 分钟）；到点还没签下来时，
「两个 hysteria 单元 / HY2 的 UDP 端口 / HY2 回环鉴权」这三行报 `SKIP 待证书` 而**不算失败**
（证书到位后 systemd 会自动把它们拉起，`bui status` 复检即可）。

三个环境变量（等价写法 / 覆盖项）：

| 变量 | 作用 |
|---|---|
| `BUI_DOMAIN` | 等价于 `--domain`，域名不想进命令行时用它 |
| `BUI_VERSION` | 指定版本，如 `v4.0.0-rc1`；默认 `latest`，仓库只有预发布时自动回退到最新的 `v4*` 标签 |
| `BUI_MANIFEST_URL` | 直接指定 manifest 地址（离线源 / 演练用），覆盖 `BUI_VERSION` |
| `BUI_MIRRORS` | 覆盖 GitHub 镜像前缀（按序回退，拼在完整 URL 前）；设成空串就只走直连 |

*国内网络直连 GitHub 不通时*（默认已内置两个镜像前缀，下面这种写法是连引导脚本本身也走镜像）：

```bash
BUI_MIRRORS="https://ghfast.top/" bash <(curl -fsSL https://ghfast.top/https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh) --domain panel.example.com
```

引导脚本只做四件事：识别架构 → 下载 `manifest.json` 与对应架构的 `bui` 静态二进制 → 校验 sha256 → 交给 `bui install`（检测到 v3 安装时自动加 `--import-v3`，现有用户与订阅原样迁移）。

v3 机器上原地切 v4：检测到 `/opt/b-ui/users.json` 会自动补 `--import-v3`，沿用 v3 的域名与全部用户。
导入的用户各自拿到一个随机订阅 token，v3 的「用户名链接」还认 7 天，到点只认 token
（见[订阅链接与 token](#订阅链接与-token)）——这 7 天里要让现有订阅者重拉一次。

升级与回滚：

```bash
sudo bui upgrade              # 升 bui 与四个内核，失败自动回退
sudo bui upgrade --rollback   # 回到上一版二进制、内核与最近一份 state
```

### 管理与更新

- `sudo b-ui`：数字菜单（状态与体检 / 立即对账 / 对账并清理漂移 / 重启数据面 / 查看日志 / 升级与回滚 / SSH 硬化 / 住宅出口；用户仍在面板里管）
- `sudo bui upgrade`：按 `manifest.json` 升级 `bui` 与四个内核；`sudo bui upgrade --rollback` 回上一版
- `sudo bui status`：期望态与实际状态的体检（漂移只报不改）；`sudo bui status --json` 出 JSON
- `sudo bui reconcile`：按 `state.json` 重新对账（幂等）

---

## 客户端部署

Linux 客户端 `bui-c` 的安装命令从服务端面板取（面板「客户端」页，或 `GET /api/install-command`），
形如：

```bash
curl -fsSL --noproxy '*' 'https://panel.example.com/packages/bui-c-install.sh' \
  | sudo BUI_C_SOURCE='https://panel.example.com/packages' bash
```

脚本按架构从面板 `/packages/` 取 `bui-c` 裸二进制、校验 `manifest.json` 里的 sha256 后装进
`/usr/local/bin/bui-c`；面板不可达时把 `BUI_C_SOURCE` 指向 GitHub Releases 的
`https://github.com/Buxiulei/b-ui/releases/latest/download` 即可。

### 使用说明
- **启动菜单**: 输入 `bui-c`
- **代理端口**: SOCKS5/HTTP 混合入口 127.0.0.1:1080 与 127.0.0.1:8080
- **TUN 模式**: `bui-c` 菜单里切换（单元 `bui-c.service`，`bui-c.timer` 每分钟跑 `bui-c check` 自愈）
- **测试连接**: `curl --socks5 127.0.0.1:1080 https://www.gstatic.com/generate_204 -o /dev/null -w '%{http_code}\n'`
- **从 v3 升级**: 菜单 [7] 更新与维护 → [3] 从 v3 导入，导入 `/opt/hysteria-client/` 的节点，并停用 v3 的 `hysteria-client / xray-client / bui-tun` 单元

### 服务端改了节点参数之后

`bui-c` 不会自己重新拉节点：每分钟的 `bui-c check` 只做连通性探测与失败重启，每日自动更新只换
`bui-c` 与 sing-box 内核，取节点只发生在导入的时候。服务端有下面这些改动之后，v2rayN 等订阅客户端
刷新订阅即可（重置过订阅链接的要换成新链接），Linux 客户端要重新导入一次：

- `bui set obfs on|off` 开关了 HY2 混淆；
- 住宅槽位或端口跳跃区间变了（含池里从一个住宅 IP 加到多个）；
- 面板里对这个用户点了「重置订阅链接与凭据」：订阅 token、HY2 密码、vless uuid 一起换，旧链接回 404，
  要用面板里新复制的链接导入。

```bash
sudo bui-c import --sub -    # 粘贴面板里复制的整条订阅链接
```

或者进菜单 [3] 导入节点，粘贴同一条链接。

重新导入按连接原地更新：端口与凭据都没变的节点沿用原名，跳跃区间、混淆密码按服务端覆盖；当前节点不变
（改到的是当前节点时自动重新生效）；删掉过的节点默认跳过。下面两种情况会另起一个新节点，旧的留在列表里：
重置过凭据的旧节点已经连不上；换槽后的旧节点可能还连得上，但出口是别的住宅 IP，都要清理：

- **住宅 HY2 换了槽位（端口变了）、或重置过凭据（HY2 密码变了）**：只有名字正是这次导入会起的
  `<用户名>-hy2-resi`（HY2 直连是 `<用户名>-hy2-direct`；用户名全是中文时用主机名，例如
  `panel.example.com-hy2-resi`）的节点原地更新。其余的——从 v3 迁来、名字沿用 v3 目录名的
  （例如 `hysteria2-<数字>`），4.0.0 用 `bui-c import --sub <token 链接>` 导入的 `<32 位 token>-hy2-resi`，
  因重名带 `-2` 后缀的，先粘贴节点链接（名字是 `<主机名>-<类型>`）后来改从面板导入的——都会另起新节点。
- **重置过凭据后的 Reality 节点**：uuid 换了，算另一个账号，新节点与旧节点同名时命名为 `<原名>-2`
  （屏幕上提示「节点名 … 已被另一个账号占用」）。

清理旧节点要先切后删。旧节点往往就是当前节点，直接删的话默认会换到别的服务器上的节点，TUN 模式下还会断网几秒：

1. 切到新节点：菜单 [3] 导入完会问「切换到新导入的 …？」——问的是第一个新节点，正是你要的就答 y，
   不是就答 n 再用菜单 [1] 切换节点；命令行用 `sudo bui-c switch <新名字>`（`bui-c import` 不会替你切；
   加 `--activate` 切到的是这次导入的第一个节点，不一定是新的）。
2. 进菜单 [6] 删除节点，删掉旧的。

删除会记下删掉的账号（不看端口；HY2 认用户名，Reality 认 uuid）。HY2 的旧节点与新节点是同一个账号：之后重新导入时
新节点照常更新、这条记录顺手清掉；要是还没重新导入过、槽位又变了或凭据又重置了，命令行导入会提示
「跳过 1 个删过的节点：<旧名字>（要加回用 --with-deleted）」，这时加 `--with-deleted` 再导一次；菜单 [3] 会问
「要加回来吗？」，答 y。重置凭据后 Reality 的新旧节点不是同一个账号，互不影响。

池空或池里只有一个住宅 IP 时导入的住宅 HY2 节点，存的是整段跳跃区间（例如 `41000-50000`）。池里加到两个及以上
IP 后区间按槽位切开，服务端不会点名受影响的用户；不重新导入的话，跳到别的槽那段端口的连接会从别的住宅 IP 出去，
所以这类机器也要重新导入一次。

**节点名里的订阅 token**：4.0.0 用 `bui-c import --sub <token 链接>` 导入过的机器，节点名是 `<32 位 token>-<类型>`，
带着订阅 token（一长串十六进制），`bui-c list`、菜单、`bui-c status` 都会显示，截图就会泄露。4.0.1 导入时按面板用户名
命名（用户名全是中文时用主机名），但端口与凭据没变的节点重新导入沿用原名，存量的十六进制名字不会自己变。截过图或
把输出发给过别人的，到面板里对这个用户「重置订阅链接与凭据」，用新链接重新导入（凭据换了，会按用户名另起新节点），
再按上面的先切后删把十六进制名字的旧节点删掉。没外泄、不想换凭据的，先用菜单 [6] 删掉十六进制名字的节点（删的是
当前节点时会换到别的节点；全是这类节点的话删完代理停掉，直到重新导入；重新导入要能直连面板，连不上面板的机器走上面换凭据那条路），再重新导入：命令行加 `--with-deleted`，
菜单 [3] 问「要加回来吗？」时答 y（以前删掉的同一面板的其他节点也会一起回来，不要的再删一次），这回按用户名命名，
最后切回想用的节点。

### 客户端菜单一览

`bui-c` 无参数运行即进数字菜单（Rust 渲染，零外部依赖）：

```
  ── B-UI 客户端 v4.0.1 ──

   节点   ●  运行中  alice-reality-direct
          Reality直连  reality-direct  panel.example.com:10001
   代理      SOCKS5 :1080   HTTP :8080
   模式   TUN   ●  运行中

     [1] 切换节点        [2] 切到 SOCKS
     [3] 导入节点        [4] 服务控制
     [5] 连接检查        [6] 删除节点
     [7] 更新与维护      [8] 卸载
     ──────────────────────────────────
     [0] 退出
```

[2] 直接写要切到的模式（SOCKS 模式下是「切到 TUN」）。[7] 更新与维护是二级菜单：[1] 检查更新、
[2] 关闭自动更新（关着时是「开启自动更新」）、[3] 从 v3 导入；有新版时主菜单在「更新与维护」后面挂 ★。

引擎只有 sing-box（≤ 1.14），切模式 = 重渲染 `config.json` + 重启 `bui-c.service`；服务端
`sudo b-ui` 同样是数字菜单，两者都不依赖 gum/fzf。

### 内核更新策略

服务端与客户端的版本、下载地址、sha256 全部由 Release 里的 `manifest.json` 决定（顶层 `version` / `kernels` 版本表 / 扁平 `artifacts`，每项都是裸二进制的 `url` + `sha256`）：

```
服务端：sudo bui upgrade            # 按 manifest 换 bui 与四个内核，失败可 bui upgrade --rollback
客户端：bui-c update                # 每日 timer 自动，菜单 [7] 更新与维护 → [2] 可关
下载顺序：服务端面板 /packages/ → GitHub Releases → BUI_MIRRORS 里的镜像前缀
```

不再有 v3 的 `version.json`、不再有 cron 行、不再从发行版包或 `get.hy2.sh` 装内核。

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
    ├─ 关键词域名 (openai/chatgpt/gemini.google/anthropic/claude/...) → 住宅上游（SOCKS5 或 HTTP）
    └─ 其余 → 直出 VPS（池空时全部直出，fail-open）
```

- 默认分流关键词 67 条，权威副本在 `crates/bui-schema/src/keywords.rs` 的 `DEFAULT_KEYWORDS`；`sudo bui residential domains` 打印当前生效的一份
- 状态：`/opt/b-ui/state.json` 的 `residential`（600），上游凭据随它一起落盘；不再有 `residential-proxy.json`
- 控制命令：`sudo bui residential {status|add|remove|enable|disable|global|domains|restore-default|check|select|health}`（`add -` 从 stdin 读，凭据不进 argv）
- 入口：`sudo b-ui` 菜单第 8 项 / Web 看板🏠 / 上面这套子命令；上游体检与自动黑名单由守护进程按日跑
- 选购与配置：[docs/residential-proxy-guide.md](docs/residential-proxy-guide.md) —— 供应商粘性参数（不加就静默换 IP）、目标端口白名单、"不限量"与转售条款、池子来源风险披露、体检读数判读

---

## API 与端口配置

### 端口列表
| 端口 | 协议 | 用途 |
|------|------|------|
| 80/443 | TCP | Caddy (Web 面板 HTTPS + 证书申请) |
| 10000 | UDP | Hysteria2 直连 |
| 20000-30000* | UDP | Hysteria2 直连的端口跳跃（可选） |
| 40000 | UDP | Hysteria2 住宅 |
| 41000-50000 | UDP | Hysteria2 住宅的端口跳跃 |
| 10001 | TCP | VLESS-Reality 直连 |
| 10002 | TCP | VLESS-Reality 住宅 |
| 127.0.0.1:8080 | TCP | 面板（只听本机，外部经 Caddy 反代） |
| 127.0.0.1:2080 | TCP | 本地 sing-box 中继（住宅分流） |

### API 端点
| 端点 | 用途 |
|------|------|
| `/api/users` | 用户增删改查（需登录） |
| `/api/config` / `/api/stats` / `/api/online` | 服务端配置、流量统计、在线状态（需登录） |
| `/api/sub/:token` | base64 的 `vless://` / `hysteria2://` 列表（v2rayN） |
| `/api/subscription/:token` | sing-box 订阅配置 |
| `/api/clash/:token` | Clash Meta (mihomo) 订阅配置 |
| `/api/nodes/:token` | 节点集合 + 分流规则 JSON（`bui-c` 用） |
| `/api/install-command` | 客户端一键安装命令 |
| `/packages/` | `bui-c` 与内核二进制、`manifest.json`、`bui-c-install.sh` |

### 订阅链接与 token

这四个端点**免鉴权**，而响应体里就是该用户的 HY2 明文密码与 VLESS UUID ——
所以**路径末段本身就是凭据**，跟面板密码同级，不要贴进聊天群、issue 或截图。

- 末段是每个用户的**随机订阅 token**（32 位小写十六进制，建用户时生成）：
  `https://panel.example.com/api/sub/0123456789abcdef0123456789abcdef`。
  面板里点开该用户的配置弹窗可以复制，装机收尾也会打印第一个用户的三条地址。
- **旧的「用户名链接」**（v3 的 `/api/sub/<用户名>` 形状）只在全局宽限期内还认：
  全新装机不设宽限期，一开始就只认 token；从 v3 导入时给 **7 天**，让现有订阅者有时间重拉。
  `sudo bui status` 有「旧订阅链接」一行（已停用 / 还剩多久 / 已过期）；
  `sudo bui set legacy-sub off` 立刻停用全部用户名链接，
  `sudo bui set legacy-sub 2026-09-21T00:00:00Z` 改期。
- **重置（怀疑链接泄露时）**：面板配置弹窗里的「重置订阅链接与凭据」（`POST /api/users/<用户名>/rotate`）
  ——同时换订阅 token、HY2 密码与 VLESS UUID，并立刻停用他的用户名链接。
  旧链接与旧凭据当即失效，**该用户必须重新导入一次订阅**；
  已部署 `bui-c` 的机器不会自愈，要人工重新导入（见 [docs/HANDOVER-bui-c.md](docs/HANDOVER-bui-c.md) §6）。
- 认不出的末段一律 404 `{"error":"User not found"}`，不区分「查无此人」与「链接已过期」。

---

## 文件结构

- `/opt/b-ui/bin/`: `bui` 与四个内核（hysteria / xray / sing-box / caddy）的静态二进制，sha256 由 `manifest.json` 钉住
- `/opt/b-ui/state.json`: 期望态（600），`state.backups/` 保留最近 10 份
- `/opt/b-ui/runtime.json` / `auth-snapshot.json`: 运行期计数与鉴权快照（600）
- `/opt/b-ui/{config.yaml,config-residential.yaml,xray-config.json,singbox-relay.json,Caddyfile}`: 由对账器渲染，勿手改
- `/opt/b-ui/certs/`: 证书（Caddy 签发后由守护进程同步）
- `/opt/b-ui/packages/`: 给客户端下载的二进制与 `manifest.json`
- `/run/b-ui.sock`: CLI ↔ 守护进程（600，root）
- `/usr/local/bin/{bui,b-ui}`: 服务端命令（`b-ui` 裸跑 = 数字菜单）
- `/opt/bui-c/{bin/sing-box,profiles.json,config.json}` 与 `/usr/local/bin/bui-c`: Linux 客户端

---

## License
MIT
