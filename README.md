# B-UI - Hysteria2 + Xray 一键部署工具

基于 [Hysteria2](https://v2.hysteria.network/) 和 [Xray](https://github.com/XTLS/Xray-core) 的一键安装脚本，支持服务端和客户端部署，自带 Web 管理面板。

## ✨ 功能特性

### 服务端 (B-UI)
- 🚀 **双协议支持** - Hysteria2 + VLESS-Reality
- 👥 多用户管理 (Web 面板)
- 📊 流量统计 / 在线状态 / 月度流量
- 📱 二维码分享 (兼容 v2rayN / v2rayNG / Shadowrocket)
- ⏱️ 用户时长/流量限制
- 🔐 自动 HTTPS 证书 (Let's Encrypt)
- 🔑 管理密码可修改 (Web + 终端)
- � 伪装网站配置 (双协议统一)
- �🌐 URL API 管理接口
- ⚡ BBR 优化
- 🖥️ `b-ui` 终端管理命令

### 客户端
- 🔌 **双协议导入** - Hysteria2 + VLESS-Reality 链接
- 🌍 TUN 全局代理模式
- ️ SSH 连接保护
- 📋 路由规则 (域名/IP 绕过)
- 🔄 内核一键更新
- 🖥️ `b-ui-client` 终端管理命令

---

## 🖥️ 服务端安装

> ⚠️ **注意**: 安装脚本需要 root 权限，请先切换到 root 用户

```bash
# 先切换到 root 用户
sudo -i

# 一键安装
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-server.sh)
```

**国内镜像（推荐）：**
```bash
sudo -i
bash <(curl -fsSL https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main/b-ui-server.sh)
```

### 安装完成后

- Web 管理面板: `https://你的域名/`
- 终端管理: 输入 `sudo b-ui`

### 终端管理 (b-ui)

```
╔══════════════════════════════════════════════════════════════╗
║                    B-UI 操作菜单                            ║
╠══════════════════════════════════════════════════════════════╣
║  1. 一键安装 (Hysteria2 + Xray + 管理面板)                    ║
║  2. 查看客户端配置                                           ║
║  3. 重启所有服务                                             ║
║  4. 查看日志                                                 ║
║  5. 开启 BBR                                                 ║
║  6. 开机自启动设置                                           ║
║  7. 更新内核 (Hysteria2 + Xray)                              ║
║  8. 完全卸载                                                 ║
║  0. 退出                                                     ║
╚══════════════════════════════════════════════════════════════╝
```

---

## 🌐 URL 管理 API

无需登录面板，通过 URL 直接管理用户：

| 操作 | URL |
|------|-----|
| 创建用户 | `/api/manage?key=密码&action=create&user=用户名&days=30&traffic=10` |
| 删除用户 | `/api/manage?key=密码&action=delete&user=用户名` |
| 修改配置 | `/api/manage?key=密码&action=update&user=用户名&days=30` |
| 列出用户 | `/api/manage?key=密码&action=list` |

**参数说明：**
| 参数 | 说明 |
|------|------|
| key | 管理密码 (必填) |
| user | 用户名 (必填) |
| protocol | 协议类型: `hysteria2` 或 `vless-reality` |
| pass | 密码 (可选，留空自动生成) |
| days | 有效天数 (0=永久) |
| traffic | 总流量限制 GB (0=不限) |
| monthly | 月流量限制 GB (0=不限) |

---

## 💻 客户端安装

> ⚠️ **注意**: 安装脚本需要 root 权限，请先切换到 root 用户

```bash
# 先切换到 root 用户
sudo -i

# 一键安装
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh)
```

**国内镜像（推荐）：**
```bash
sudo -i
bash <(curl -fsSL https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main/b-ui-client.sh)
```

### 自动依赖安装

脚本启动时会自动检测并安装所需依赖：
- curl, wget (下载工具)
- dig (DNS 解析)
- ss/netstat (端口检测)
- iptables (TUN 模式)
- tar, gzip (解压)
- ca-certificates (HTTPS)

### 客户端菜单
```
╔══════════════════════════════════════════════════════════════╗
║              B-UI 客户端 操作菜单                            ║
╠══════════════════════════════════════════════════════════════╣
║  1. 从链接导入配置 (Hysteria2 / VLESS-Reality)               ║
║  2. 手动配置 Hysteria2                                       ║
║  3. 启动/停止服务                                            ║
║  4. 重启服务                                                 ║
║  5. 查看日志                                                 ║
╠══════════════════════════════════════════════════════════════╣
║  6. TUN 模式开关 (全局代理)                                  ║
║  7. 编辑路由规则                                             ║
║  8. 测试代理连接                                             ║
╠══════════════════════════════════════════════════════════════╣
║  9. 更新内核 (Hysteria2 + Xray)                              ║
║  10. 开机自启动设置                                          ║
║  11. 卸载                                                    ║
║  0. 退出                                                     ║
╚══════════════════════════════════════════════════════════════╝
```

### 代理使用
```bash
# SOCKS5 (默认 1080)
curl --socks5 127.0.0.1:1080 https://www.google.com

# HTTP (默认 8080)
export https_proxy=http://127.0.0.1:8080
curl https://www.google.com
```

---

## 🔓 需要开放的端口

| 端口 | 协议 | 用途 |
|------|------|------|
| 80 | TCP | SSL 证书验证 |
| 443 | TCP | 管理面板 (HTTPS) |
| 10000 | **UDP** | Hysteria2 代理 |
| 10001 | TCP | VLESS-Reality 代理 |

---

## 📁 文件结构

```
/opt/hysteria/               # 服务端数据目录
├── config.yaml              # Hysteria2 配置
├── xray-config.json         # Xray 配置
├── reality-keys.json        # Reality 密钥
├── masquerade.json          # 伪装网站配置
├── users.json               # 用户数据 (含流量统计)
└── admin/                   # 管理面板
    └── server.js

/usr/local/bin/b-ui          # 服务端终端命令
/usr/local/bin/b-ui-client   # 客户端终端命令
```

---

## ⚙️ 系统要求

| 组件 | 要求 |
|------|------|
| 操作系统 | Ubuntu / Debian / CentOS / RHEL / Fedora |
| 权限 | root (使用 `sudo -i` 切换) |
| 服务端 | 需要域名 |
| 客户端 | 支持 TUN 模式需要 systemd |

---

## 📖 相关链接

- [Hysteria2 官网](https://v2.hysteria.network/zh/)
- [Xray 官网](https://xtls.github.io/)
- [VLESS-Reality 配置](https://xtls.github.io/config/features/reality.html)

---

## 📜 License

MIT
