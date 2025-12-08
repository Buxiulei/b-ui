# Hysteria2 一键部署工具

基于 [Hysteria2](https://v2.hysteria.network/) 的一键安装脚本，支持服务端和客户端部署。

## ✨ 功能特性

### 服务端
- 🚀 一键安装 Hysteria2 服务器
- 👥 多用户管理 (Web 面板)
- 📊 流量统计 / 在线状态
- 🔐 自动 HTTPS 证书 (Let's Encrypt)
- ⚡ BBR 优化
- 🌐 全中文界面

### 客户端
- 🔌 SOCKS5 / HTTP 代理
- 🌍 TUN 全局代理模式
- 🛡️ SSH 连接保护
- 📋 路由规则 (域名/IP/关键词绕过)
- 🔧 systemd 服务管理

---

## 🖥️ 服务端安装

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/hysteria2-server/main/hysteria2-install.sh)
```

### 服务端菜单
```
1. 一键安装 (Hysteria2 + 管理面板)
2. 查看状态
3. 查看客户端配置
4. 重启所有服务
5. 查看日志
6. 开启 BBR
7. 开机自启动设置
8. 一键卸载
0. 退出
```

### 安装完成后
- 管理面板: `https://你的域名/`
- 管理密码: 安装时显示

---

## 💻 客户端安装

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/Buxiulei/hysteria2-server/main/hysteria2-client.sh)
```

### 客户端菜单
```
1. 一键安装
2. 查看状态
3. 启动/停止
4. 重新配置
5. 编辑路由规则
6. TUN 模式开关
7. 测试代理
8. 查看日志
9. 卸载
0. 退出
```

### 代理使用示例
```bash
# SOCKS5 代理 (默认端口 1080)
curl --socks5 127.0.0.1:1080 https://www.google.com

# HTTP 代理 (默认端口 8080)
export https_proxy=http://127.0.0.1:8080
curl https://www.google.com
```

---

## 📋 路由规则

客户端支持灵活的路由规则配置，可以指定哪些流量直连不走代理：

### 添加规则示例

**1. IP 地址绕过**
```
192.168.1.0/24
10.0.0.0/8
```

**2. 域名绕过**
```
*.baidu.com
*.qq.com
```

**3. 关键词匹配**
输入 `cn,baidu,taobao` 将自动生成：
```
*cn*
*baidu*
*taobao*
```

---

## 🛡️ TUN 模式

TUN 模式提供全局透明代理，所有流量自动走代理：

- 自动保护 SSH 连接 (服务器 IP 排除在代理外)
- 私有网络自动绕过 (10.x.x.x, 192.168.x.x 等)
- 需要 root 权限

---

## 📁 文件结构

### 服务端
```
脚本目录/
├── hysteria2-install.sh    # 安装脚本
└── data/                   # 数据目录
    ├── config.yaml         # 服务器配置
    ├── users.json          # 用户数据
    └── admin/              # 管理面板
```

### 客户端
```
脚本目录/
├── hysteria2-client.sh     # 安装脚本
└── hysteria-client/        # 数据目录
    ├── config.yaml         # 客户端配置
    └── bypass-rules.txt    # 路由规则
```

---

## ⚙️ 系统要求

| 组件 | 要求 |
|------|------|
| 操作系统 | Ubuntu / Debian / CentOS |
| 权限 | root |
| 服务端 | 需要域名 + 80/443 端口 |
| 客户端 | 任意 Linux |

---

## 📖 官方文档

- [Hysteria2 官网](https://v2.hysteria.network/zh/)
- [服务端配置](https://v2.hysteria.network/zh/docs/advanced/Full-Server-Config/)
- [客户端配置](https://v2.hysteria.network/zh/docs/advanced/Full-Client-Config/)

---

## 📜 License

MIT
