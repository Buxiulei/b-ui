# B-UI 证书健康检查与自动修复 — 需求文档

> **状态**: 待评审  
> **优先级**: P0 (生产故障)  
> **起因**: 2026-03-04 hy2 服务出现大面积 `CRYPTO_ERROR 0x150 (remote): tls: internal error`

---

## 1. 问题分析

### 1.1 故障现象

```
connection: open connection to 142.251.119.100:443 using outbound/hysteria2[proxy]:
CRYPTO_ERROR 0x150 (remote): tls: internal error
```

所有经过 hy2 的 DNS 查询和代理连接均失败，`(remote)` 表明 **hy2 服务端** TLS 握手失败。

### 1.2 根因分析 (5 Whys)

| # | 为什么？ | 答案 |
|---|---|---|
| 1 | 为什么客户端所有连接都报 TLS 错误？ | hy2 服务端的 TLS 证书无法完成握手 |
| 2 | 为什么 TLS 证书无法握手？ | 证书过期 or 证书文件被续期后未被 hy2 加载 |
| 3 | 为什么 hy2 没加载新证书？ | certbot 续期后没有重启 hysteria-server |
| 4 | 为什么没有重启？ | cron 任务 `certbot renew --quiet` 没有配置 `--deploy-hook` |
| 5 | 为什么没有发现？ | 没有证书健康检查和告警机制 |

### 1.3 现有代码缺陷

#### 缺陷 1：certbot 续期后不重启服务

[b-ui-server.sh L2094-2095](file:///Users/woo/Desktop/b-ui/b-ui-server.sh#L2094-L2095):

```bash
# 当前代码 — 只有 --quiet，没有 --deploy-hook
(crontab -l 2>/dev/null; echo "0 3 * * * certbot renew --quiet") | crontab -
```

> [!CAUTION]
> Hysteria2 在启动时将证书读入内存，**不会**在运行期间自动检测证书文件变化。  
> 即使 certbot 成功续期，hy2 仍然使用内存中的旧证书，直到手动重启。

#### 缺陷 2：无证书健康检查

- [server.js](file:///Users/woo/Desktop/b-ui/web/server.js) 管理面板没有任何证书状态检查逻辑
- [b-ui-server.sh show_status()](file:///Users/woo/Desktop/b-ui/b-ui-server.sh#L2253-L2314) 只显示服务运行状态，不显示证书有效期

#### 缺陷 3：无主动告警和自愈

- 证书过期后，服务**静默失败**，用户只能在所有连接断开后才会发现
- 没有日志监控、webhook 通知或自动修复机制

---

## 2. 需求规格

### 2.1 [P0] certbot deploy-hook — 续期后自动重启

**修改文件**: [b-ui-server.sh](file:///Users/woo/Desktop/b-ui/b-ui-server.sh)

将 cron 任务从：
```bash
0 3 * * * certbot renew --quiet
```

改为：
```bash
0 3 * * * certbot renew --quiet --deploy-hook "systemctl restart hysteria-server && systemctl reload nginx"
```

> `--deploy-hook` 仅在**证书实际被续期时**才执行，不会每天无意义重启。

**同时需要修改两处**：
- L2094-2095（新证书申请成功后设置 cron）
- L2111-2112（已有证书时确保 cron 正确）

---

### 2.2 [P0] 证书健康检查脚本

**新建文件**: `server/cert-check.sh`

功能要求：
1. 读取 hy2 配置文件中的证书路径
2. 使用 `openssl x509 -checkend` 检查证书有效期
3. 如果 ≤ 7 天过期：尝试 `certbot renew --force-renewal`
4. 如果续期成功：自动重启 `hysteria-server` 和 `reload nginx`
5. 如果续期失败：写入错误日志，输出告警信息
6. 添加到 cron 任务：每 12 小时检查一次

```bash
#!/bin/bash
# /opt/hysteria/cert-check.sh
# 证书健康检查与自动修复

CERT_PATH="/etc/letsencrypt/live/$(域名)/fullchain.pem"
LOG_FILE="/opt/hysteria/cert-check.log"

check_cert() {
    if [ ! -f "$CERT_PATH" ]; then
        echo "[$(date)] ERROR: 证书文件不存在: $CERT_PATH" >> "$LOG_FILE"
        return 1
    fi
    
    # 检查证书是否在 7 天内过期
    if ! openssl x509 -checkend 604800 -noout -in "$CERT_PATH" 2>/dev/null; then
        local expiry=$(openssl x509 -enddate -noout -in "$CERT_PATH" | cut -d= -f2)
        echo "[$(date)] WARNING: 证书将在 7 天内过期 (过期时间: $expiry)" >> "$LOG_FILE"
        return 1
    fi
    
    return 0
}

auto_renew() {
    echo "[$(date)] INFO: 尝试自动续期..." >> "$LOG_FILE"
    certbot renew --force-renewal --deploy-hook "systemctl restart hysteria-server && systemctl reload nginx" 2>> "$LOG_FILE"
    
    if [ $? -eq 0 ]; then
        echo "[$(date)] SUCCESS: 证书续期成功，服务已重启" >> "$LOG_FILE"
    else
        echo "[$(date)] CRITICAL: 证书续期失败！请手动检查！" >> "$LOG_FILE"
    fi
}

# 主流程
if ! check_cert; then
    auto_renew
else
    echo "[$(date)] OK: 证书有效" >> "$LOG_FILE"
fi
```

**Cron 配置**:
```bash
0 */12 * * * /opt/hysteria/cert-check.sh
```

---

### 2.3 [P1] 管理面板显示证书状态

**修改文件**: [web/server.js](file:///Users/woo/Desktop/b-ui/web/server.js)

新增 API 端点 `GET /api/cert-status`：

```json
{
  "domain": "example.com",
  "certPath": "/etc/letsencrypt/live/example.com/fullchain.pem",
  "issuer": "Let's Encrypt",
  "notBefore": "2026-01-04T00:00:00Z",
  "notAfter": "2026-04-04T00:00:00Z",
  "daysRemaining": 31,
  "status": "valid",        // "valid" | "expiring_soon" | "expired" | "missing"
  "lastCheckTime": "2026-03-04T13:40:00+08:00"
}
```

**前端展示**: 在 [web/index.html](file:///Users/woo/Desktop/b-ui/web/index.html) 的状态面板中添加证书状态卡片：
- 🟢 有效（剩余 > 30 天）
- 🟡 即将过期（剩余 ≤ 30 天）
- 🔴 已过期 / 不存在
- 显示「立即续期」按钮（调用 `POST /api/cert-renew`）

---

### 2.4 [P1] CLI 面板显示证书信息

**修改文件**: [b-ui-server.sh](file:///Users/woo/Desktop/b-ui/b-ui-server.sh) 中的 `show_status()` 函数

在服务状态下方添加：
```
[证书状态]
  域名: example.com
  有效期: 2026-01-04 ~ 2026-04-04
  剩余天数: 31 天 ✓
```

如果证书 ≤ 7 天过期，显示红色告警。

---

### 2.5 [P2] 防御性措施 — Hysteria2 ACME 内建支持

Hysteria2 本身支持内建 ACME 自动证书管理，可以替代 certbot。但由于 b-ui 架构中 Nginx 占用了 443 端口，无法使用 hy2 内建 ACME。

**未来可考虑**：
- 让 hy2 使用 `dns-01` challenge 而非 `http-01`
- 或让 hy2 直接使用 ACME 并将证书共享给 Nginx

---

## 3. 实施优先级

| 优先级 | 需求 | 工作量 | 影响 |
|---|---|---|---|
| **P0** | certbot deploy-hook 修复 | 10 min | 修复续期不重启的根本问题 |
| **P0** | cert-check.sh 健康检查脚本 | 30 min | 主动发现并修复证书问题 |
| **P1** | 管理面板证书状态 API + UI | 2 hr | 可视化证书状态 |
| **P1** | CLI 显示证书信息 | 30 min | 运维可见性 |
| **P2** | ACME 内建支持调研 | 未定 | 长期优化 |

---

## 4. 验证计划

### 4.1 P0 验证（deploy-hook）

在服务器上执行：
```bash
# 1. 查看当前 cron
crontab -l | grep certbot

# 2. 更新后应显示 --deploy-hook
crontab -l | grep "deploy-hook"

# 3. 手动测试续期 + 重启
certbot renew --dry-run --deploy-hook "echo 'hook executed'"
```

### 4.2 P0 验证（健康检查脚本）

```bash
# 1. 运行脚本
bash /opt/hysteria/cert-check.sh

# 2. 查看日志
cat /opt/hysteria/cert-check.log

# 3. 模拟过期（修改 checkend 阈值为一个很大的值测试）
openssl x509 -checkend 99999999 -noout -in /etc/letsencrypt/live/DOMAIN/fullchain.pem
```

### 4.3 P1 验证（管理面板）

```bash
# 调用 API
curl -s https://DOMAIN/api/cert-status?key=ADMIN_KEY | jq .
```

---

## 5. 紧急止血方案

> [!IMPORTANT]
> 在代码修复部署前，请在服务器上手动执行以下命令**立即恢复服务**：

```bash
# 1. 检查证书有效期
openssl x509 -enddate -noout -in /etc/letsencrypt/live/YOUR_DOMAIN/fullchain.pem

# 2. 如果证书过期，手动续期
certbot renew --force-renewal

# 3. 修复权限
chmod 755 /etc/letsencrypt /etc/letsencrypt/live /etc/letsencrypt/archive
chmod 755 /etc/letsencrypt/live/YOUR_DOMAIN /etc/letsencrypt/archive/YOUR_DOMAIN
chmod 644 /etc/letsencrypt/archive/YOUR_DOMAIN/*.pem

# 4. 重启服务
systemctl restart hysteria-server
systemctl reload nginx
```
