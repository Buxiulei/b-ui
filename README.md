<p align="center">
  <img src="web/logo.jpg" alt="B-UI Logo" width="500">
</p>

# B-UI

轻量级 Hysteria2 + Xray 一键部署工具，内置 Web 管理面板与全功能流量管理。

**当前版本**: v2.15.2

---

## 功能特性

### 服务端 (Core)
- **多协议支持**: Hysteria2 / VLESS-Reality / VLESS-WS-TLS
- **用户管理**: Web 面板可视化管理，支持多用户、流量统计、在线状态监控
- **访问控制**: 支持用户时长限制、总流量/月度流量限制
- **自动维护**: 自动 HTTPS 证书 (Let's Encrypt)、自动更新、BBR 优化
- **便捷分享**: 二维码 (兼容 v2rayN/Shadowrocket)、URL 订阅
- **免流支持**: 内置电信/联通/移动免流 SNI 策略

### 客户端 (Client)
- **多模式**: 全局 TUN 代理、SSH 连接保护
- **灵活配置**: 支持 Hysteria2 / VLESS 协议导入、路由规则自定义
- **工具集成**: 内核一键更新、网络连通性测试

---

## 服务端部署

**系统要求**: Ubuntu / Debian / CentOS / RHEL (需 root 权限)

### 一键安装
```bash
sudo -i
bash <(curl -fsSL "https://raw.githubusercontent.com/Buxiulei/b-ui/main/install.sh?$(date +%s)")
```
*国内镜像*: `bash <(curl -fsSL "https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main/install.sh?$(date +%s)")`

### 管理与更新
安装后可通过终端命令 `b-ui` 进行管理：
- 查看配置 / 重启服务 / 查看日志
- 修改密码 / 开启 BBR / 检查更新

---

## 客户端部署

> **提示**: 客户端一键安装命令需要从服务端 Web 面板获取，包含服务端地址和自动配置信息。请先完成服务端部署，然后登录 Web 管理面板查看专属安装命令。

### 使用说明
- **启动菜单**: 输入 `bui-c`
- **代理端口**: SOCKS5 (1080) / HTTP (8080)
- **测试连接**: `curl --socks5 127.0.0.1:1080 https://www.google.com`

---

## API 与端口配置

### 端口列表
| 端口 | 协议 | 用途 |
|------|------|------|
| 80/443 | TCP | Web 面板 / 证书申请 |
| 10000 | UDP | Hysteria2 |
| 10001 | TCP | VLESS-Reality |
| 10002 | TCP | VLESS-WS-TLS |
| 20000-30000* | UDP | 端口跳跃（可选，范围可自定义） |

### URL 管理 API
可通过 GET 请求直接管理用户：
- **创建**: `/api/manage?key=密码&action=create&user=用户名`
- **删除**: `/api/manage?key=密码&action=delete&user=用户名`
- **列表**: `/api/manage?key=密码&action=list`

*参数*: `traffic` (总流量), `monthly` (月流量), `days` (有效期), `sni` (指定域名)

---

## 文件结构

- `/opt/b-ui/`: 核心数据目录 (配置、证书、数据库)
- `/usr/local/bin/b-ui`: 服务端命令
- `/usr/local/bin/b-ui-client`: 客户端命令

---

## License
MIT
