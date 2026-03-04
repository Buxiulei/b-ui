# B-UI Hysteria2 TLS 故障排查报告

> **日期**: 2026-03-04 13:40 ~ 13:52 (UTC+8)  
> **域名**: bwg.baiyibaiyi.cn  
> **服务器**: honest-bump-6 (138.128.195.22)

---

## 1. 故障现象

客户端（v2rayN macOS）报大面积 TLS 握手失败：

```
CRYPTO_ERROR 0x150 (remote): tls: internal error
```

**影响范围**: 所有经 hy2 代理的 DNS 查询和 TCP 连接全部失败。

**涉及域名**: `push.apple.com`、`eagleyun.cn`、`googleapis.com`、`icloud.com`、`baidu.com` 等（非特定域名，所有请求均失败）。

---

## 2. 服务器排查结果

### 2.1 证书状态 ✅ 正常

| 项目 | 值 |
|---|---|
| cert1.pem 过期时间 | May 29, 2026 GMT |
| cert2.pem 过期时间 | Jun 02, 2026 GMT |
| 证书路径 | `/etc/letsencrypt/live/bwg.baiyibaiyi.cn/` |
| 权限 | fullchain: 644, privkey: 600 (正确) |

> **结论**: 证书未过期，排除证书过期作为根因。

### 2.2 服务状态

- **故障期间**: PID 8401 运行中，服务端日志**无 TLS 错误**
- **重启后** (13:49:44): PID 14754，客户端立即恢复连接
- **成功连接的客户端**: 白衣挚友、一家人（重启后 ~1 分钟内）

### 2.3 服务端日志分析

故障期间的服务端日志（05:30~05:49 UTC）未出现任何 TLS 相关错误，只有：
- 正常的 client connected / disconnected
- TCP connection reset 错误（上游目标站如 Microsoft 返回的，非 hy2 本身）
- `api.hy2.io:443` Application error 0x100

### 2.4 系统资源

| 项目 | 值 |
|---|---|
| 内存 | 1023MB 总量，607MB 可用 |
| 磁盘 | 19G 总量，14G 可用 (23%) |
| Swap | 544MB，已用 35MB |

### 2.5 crontab ⚠️ 问题

```
crontab 为空 — certbot 自动续期 cron 任务未设置！
```

---

## 3. 根因分析

### 3.1 排除项

| 可能原因 | 排除依据 |
|---|---|
| 证书过期 | cert1.pem 到 2026-05-29，cert2.pem 到 2026-06-02 |
| 证书权限 | privkey 600, fullchain 644, 目录 755 |
| 磁盘/内存 | 资源充足 |
| 配置错误 | config.yaml 正确指向 letsencrypt 目录 |

### 3.2 最终根因：QUIC 协议层状态异常

**诊断依据**:
1. 服务端日志无 TLS 错误，但客户端报 `(remote): tls: internal error`
2. 重启 hy2 后立即恢复
3. 不是所有客户端同时受影响

**推测机制**: Hysteria2 基于 QUIC 协议（quic-go 库）。在长期运行中，QUIC/TLS 会话状态可能因以下原因损坏：
- **TLS Session Ticket 过期或损坏** — QUIC 使用 TLS 1.3 Session Ticket 做 0-RTT 恢复，状态异常会导致 `tls: internal error`
- **quic-go 内存状态异常** — 长时间运行后 QUIC 连接追踪表可能损坏
- **服务端 panic 抑制** — 日志中出现 `suppressing panic for copyResponse error`，说明已有 panic 发生

**关键日志**:
```
05:46:51 suppressing panic for copyResponse error in test; copy error: context canceled
```

这说明 hy2 进程内部已出现 panic（被捕获抑制），可能导致 TLS 状态机异常。

---

## 4. 需要后端排查的问题

请后端工程师检查以下内容，返回分析文档：

### 4.1 Hysteria2 版本和 quic-go 版本

```bash
hysteria version
```

确认当前版本是否有已知的 TLS/QUIC 状态机 bug。

### 4.2 panic 日志追溯

```bash
# 搜索所有 panic 和 error 日志
journalctl -u hysteria-server --since "2026-03-01" | grep -i "panic\|suppress\|fatal\|error" | head -50
```

需要确认：
- `suppressing panic for copyResponse error` 出现频率
- 是否有规律（特定客户端/特定目标站触发）

### 4.3 QUIC 流控配置是否过大

当前配置：
```yaml
quic:
  initStreamReceiveWindow: 26843545    # 25.6 MB
  maxStreamReceiveWindow: 26843545     # 25.6 MB  
  initConnReceiveWindow: 67108864      # 64 MB
  maxConnReceiveWindow: 67108864       # 64 MB
```

服务器仅 1GB 内存，6 个活跃用户。窗口设置可能偏大，建议评估：
- 当前设置是否会导致内存压力
- 是否应降低到默认值

### 4.4 建议检查项

1. **是否需要定期重启**: 设置 systemd Watchdog 或定时重启（如每周一次）
2. **升级 Hysteria2**: 检查最新版本是否修复了相关 QUIC/TLS bug
3. **添加健康检查**: 定时从外部探测 TLS 握手是否正常

---

## 5. 已执行的修复

| # | 操作 | 状态 |
|---|---|---|
| 1 | `certbot renew --force-renewal` | ✅ 完成 |
| 2 | `systemctl restart hysteria-server` | ✅ 完成，客户端恢复 |
| 3 | 修复 b-ui-server.sh certbot deploy-hook | ✅ 代码已修改 |
| 4 | 添加 CLI 证书状态显示 | ✅ 代码已修改 |
| 5 | 创建 cert-check.sh 健康检查脚本 | 🔄 进行中 |
| 6 | 设置 crontab (当前为空) | ⏳ 待执行 |

---

## 6. 紧急止血：如果再次发生

```bash
# 在服务器上执行
systemctl restart hysteria-server

# 验证客户端已恢复
journalctl -u hysteria-server -f | grep "client connected"
```
