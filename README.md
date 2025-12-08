# H-UI - Hysteria2 一键部署工具

基于 [Hysteria2](https://v2.hysteria.network/) 的一键安装脚本，支持服务端和客户端部署，自带 Web 管理面板。

## ✨ 功能特性

### 服务端 (H-UI)
- 🚀 一键安装 Hysteria2 服务器
- 👥 多用户管理 (Web 面板)
- 📊 流量统计 / 在线状态 / 月度流量
- 📱 二维码分享 (兼容 v2rayN / Shadowrocket / Clash Meta)
- ⏱️ 用户时长/流量限制
- 🔐 自动 HTTPS 证书 (Let's Encrypt)
- 🔑 管理密码可修改 (Web + 终端)
- 🌐 URL API 管理接口
- ⚡ BBR 优化
- 🖥️ h-ui 终端管理命令

### 客户端
- 🔌 SOCKS5 / HTTP 代理
- 🌍 TUN 全局代理模式
- 📋 链接导入配置 (hysteria2:// 格式)
- 🛡️ SSH 连接保护
- 📋 路由规则 (域名/IP 绕过)

---

## 🖥️ 服务端安装

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/h-ui/main/hysteria2-install.sh)
```

### 安装完成后

- Web 管理面板: `https://你的域名/`
- 终端管理: 输入 `h-ui`

### 终端管理 (h-ui)

安装后在终端输入 `h-ui` 即可查看：
- 服务运行状态
- 绑定域名和端口
- 管理员密码
- URL API 示例

**快捷键：**
- `p` - 修改管理密码
- `q` - 退出
- 其他键 - 刷新状态

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
| pass | 密码 (可选，留空自动生成) |
| days | 有效天数 (0=永久) |
| traffic | 总流量限制 GB (0=不限) |
| monthly | 月流量限制 GB (0=不限) |

---

## 💻 客户端安装

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/h-ui/main/hysteria2-client.sh)
```

### 客户端菜单
```
1. 一键安装 (手动输入配置)
2. 从链接导入配置          ← 支持 hysteria2:// 格式
3. 查看状态
4. 启动/停止
5. 重新配置
6. 编辑路由规则
7. TUN 模式开关
8. 测试代理
9. 查看日志
10. 卸载
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

---

## 📁 文件结构

```
/opt/hysteria/           # 服务端数据目录
├── config.yaml          # 服务器配置
├── users.json           # 用户数据 (含流量统计)
└── admin/               # 管理面板

/usr/local/bin/h-ui      # 终端管理命令
```

---

## ⚙️ 系统要求

| 组件 | 要求 |
|------|------|
| 操作系统 | Ubuntu / Debian |
| 权限 | root |
| 服务端 | 需要域名 |

---

## 📖 相关链接

- [Hysteria2 官网](https://v2.hysteria.network/zh/)
- [服务端配置](https://v2.hysteria.network/zh/docs/advanced/Full-Server-Config/)
- [客户端配置](https://v2.hysteria.network/zh/docs/advanced/Full-Client-Config/)

---

## 📜 License

MIT
