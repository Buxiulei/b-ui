<p align="center">
  <img src="web/logo.jpg" alt="B-UI Logo" width="500">
</p>

# B-UI

轻量级 Hysteria2 + Xray 多协议代理一键部署工具，内置 Web 管理面板与全功能流量管理。

**当前版本**: v3.2.2

---

## 最新更新

### v3.2.2 — Caddy 证书共享修复
- 🔐 **证书自动同步**：Caddy 申请的 SSL 证书自动同步到 Hysteria2，修复服务端启动失败
- ⏱ **启动顺序优化**：Caddy → 证书同步 → Hysteria2，确保证书就绪后再启动
- 🩺 **健康检查增强**：定时检查 Caddy 状态 + 证书同步 + Hysteria2 自动恢复

### v3.2.x — 客户端增强
- 🛡 UFW 防火墙兼容（TUN 启动前暂停/停止后恢复）
- 🌐 系统代理自动配置（不开 TUN 也能访问 GitHub）
- ▶ 服务控制菜单重构（按服务独立启停+显示端口号）

### v3.1.0 — 内核代理下载
- 🌐 服务端自动从 GitHub 同步最新内核（每 6h），客户端优先从服务端拉取

---

## 功能特性

### 服务端 (Core)
- **多协议支持**: Hysteria2 / VLESS-Reality / VLESS-WS-TLS
- **用户管理**: Web 面板可视化管理，支持多用户、流量统计、在线状态监控
- **访问控制**: 用户时长限制、总流量/月度流量限制、用户级别限速
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

```
1. 📥 导入节点      (粘贴链接即可，自动识别)
2. 📋 节点管理      (列表/切换/删除)
3. ▶  服务控制      (启动/停止/重启/日志)
4. 🌐 TUN 全局代理  (开启/关闭)
5. 🔍 连接测试
6. ⬆  一键更新      (服务端优先 → GitHub fallback)
7. ⚙  高级设置      (自启动/路由规则)
8. 🗑  卸载
```

### 内核更新策略

```
版本检测: 服务端 API + GitHub API (并行取最新)
下载优先级:
  1. 服务端 /packages/ (国内可达)
  2. GitHub Releases (fallback)
```

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
- `/usr/local/bin/b-ui`: 服务端命令
- `/usr/local/bin/bui-c`: 客户端命令

---

## License
MIT
