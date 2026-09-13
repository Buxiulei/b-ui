# v4 M1 验收清单（spec §9 第一行）

在 bwg-rick 上执行，逐条记录结果；有 FAIL 不进入 M2。

## 0. 前置
- 已在 bwg-rick 上 `tar` 快照 `/opt/b-ui` 与 `/etc/systemd/system/{hysteria-*,xray,b-ui*,caddy}.service`（回滚用，保留 30 天）
- v3 仍在跑（用来对比订阅输出）
- **先记录这台机器有没有活的防火墙**（决定下面 step1/step2 的期望，也决定端口要不要去云厂商控制台放行）：
  ```
  command -v ufw && ufw status | head -n1     # 期望 "Status: active" 或 "Status: inactive"
  systemctl is-active firewalld 2>/dev/null   # 期望 "active" 或 "inactive"/"unknown"
  ```
  - 两者任一 `active` → 对账会实际执行 `ufw allow` / `firewall-cmd --add-port`，第二轮起 `firewall` 的 key 命中、零变更。
  - **两者都不 active（bwg-rick 的实测结果填进 §5 的「已知降级」栏）** → 对账**不**产出防火墙改动，只在每轮报告的 `notes` 里留一行「未检测到 ufw/firewalld，请在云厂商安全组放行：…」。这不算 FAIL：`notes` 不进 `changed`、也不影响 `/api/health` 的 `status`，所以 step1 与 step2 照样 PASS（Task 6 + Task 15 的口径）。此时**必须人工**去 VPS 控制台的安全组放行 `notes` 里列出的那串端口，否则四个节点连不上——这一步没有任何自动化替代。
- **manifest 必须可达**（裁决「M5 前不创建任何 Release/tag」，所以内置的 `releases/latest/download/manifest.json` 在 M1 必然 404）。二选一：
  1. 本机/跳板上 `python3 -m http.server 8000` 托管 CI 产物的 `dist/`（含 `manifest.json` 与五个裸二进制），装机时 `export BUI_MANIFEST_URL=http://127.0.0.1:8000/manifest.json`（走 ssh 端口转发）；
  2. 手工把四个内核二进制放进 `/opt/b-ui/bin/`（0755）再装机——此时 manifest 拉不到只会 warn，对账跳过 `Binary`。
  两条都没做的话 `bui install` 会在生成 REALITY 密钥那一步报「未找到 xray 二进制…请设 BUI_MANIFEST_URL」（Task 16 的 `install_without_a_reachable_manifest_says_how_to_fix_it` 就是这个口径的单元级版本）。

## 1. 一条命令跑机器化部分
```
sudo BASE=/opt/b-ui BUI=/opt/b-ui/bin/bui \
  SB112=/opt/kernels/sing-box-1.12 SB113=/opt/kernels/sing-box-1.13 SB114=/opt/kernels/sing-box-1.14 \
  bash scripts/m1-acceptance.sh
```
期望输出（顺序固定）：
```
PASS  step1 体检：无漂移、无错误、六单元在跑
PASS  step2 二次 install 走对账路径（不覆盖 state）
PASS  step2 二次对账零变更
PASS  step3 xray run -test
PASS  step3 sing-box check（sing-box version 1.12.x）
PASS  step3 sing-box check（sing-box version 1.13.x）
PASS  step3 sing-box check（sing-box version 1.14.x）
PASS  step3 sing-box check（sing-box version 1.13.19）
PASS  step3 caddy validate（配置在 /opt/b-ui/Caddyfile）
PASS  step3b auth-snapshot.json 存在、0600、形状合 C5（schema=1 + users）
PASS  step4 /usr/local/bin/b-ui 指向 bin/bui
PASS  step4 /run/b-ui.sock 存在且 0600
合计 12 PASS / 0 FAIL
```

本机（无 root、不碰系统）先验脚本自己的判定逻辑：
```
bash scripts/m1-acceptance.sh --self-test
```
期望 4 条 `PASS`、`合计 4 PASS / 0 FAIL`、退出码 0。

## 2. 装机与导入（人工执行，脚本之前）
```
sudo bash install.sh              # P5 交付前用：sudo /opt/b-ui/bin/bui install --import-v3 --admin-password-stdin
sudo /opt/b-ui/bin/bui status --json | python3 -m json.tool | head -40
```
期望：`status": "ok"`、`drift": []`、`services` 六项 `active: true`、`reconcile.errors` 为空；`/opt/b-ui/v3-backup/` 下有 `users.json` / `reality-keys.json` / `residential-proxy.json` / `admin.env` / `.resi-health-state.json` / `.relay.lock` / `port-hopping.json` / `masquerade.json` / `server_ip.txt` 这 9 个归档文件（0600，机器上原本没有的不会出现）外加迁移块留下的 `*.bak.*`；`/opt/b-ui` 顶层没有 v3 的 `*.sh`、`admin/`、`install-key.txt`、`sing-box`（v4 的那份在 `bin/`）、`*.bak.*`、`*.tmp`。**顶层逐项过一遍**：`ls -A /opt/b-ui` 的结果必须全部落在 Task 5 的 `BASE_WHITELIST` 13 项 + 本轮 artifact 的路径集合里，否则就是一条 `stray_file`（脚本的 step1 会替你报出来）。另外人工确认 Caddy 数据已迁移、没有重签：`ls /opt/b-ui/caddy/caddy/certificates/*/*/` 里是导入前那套证书，`journalctl -u caddy --since -10min | grep -ci "obtain"` 为 0（有输出说明 ACME 账号没搬成功，按 Task 16 的 `migrate_caddy_data` 排查）。

## 3. 不由本脚本覆盖的两项（写清归属，避免无人认领）
| 项 | 归属 | 怎么验 |
|---|---|---|
| 每个现有用户三种订阅与 v3 逐项相同 | P0 的 golden 测试 + P2 的订阅端点任务 | 本机 `cargo test -p bui-schema --test golden_subscription`；bwg-rick 上 P2 合并后 `curl -s http://127.0.0.1:8080/api/sub/<订阅token> \| base64 -d`（token 取 `state.json` 的 `users[].sub_token`）与 v3 抓取的样本逐行 `diff` |
| v2rayN 四节点可连 | 主理人（M1/M3 验收窗口） | 导入订阅 → 四个节点依次连通性测试 → 记录延迟 |

## 4. 已知降级（不阻塞 M1，但要记下来）
| 现象 | 判定 | 处理 |
|---|---|---|
| 机器上没有 `chattr` / `lsattr`（容器、精简镜像） | 不报漂移、`status` 仍 `ok` | 无需处理（Task 5 的降级口径） |
| 有 `chattr` 但文件系统不支持 `+i`（overlayfs 等） | 每轮一条 note + 一条 `resolv_immutable` 漂移 → `status` 恒为 `degraded` | 在面板或 `state.json` 把 `system.static_dns` 置 false（连静态 DNS 一起关），或在本表记为「已知降级」放行 |
| manifest 不可达且内核已手工放好 | 每轮一条 warn，`versions` 不随 manifest 走 | M5 有真 Release 后自愈 |
| 机器上没有活的 ufw/firewalld（见 §0） | 每轮报告的 `notes` 多一行「请在云厂商安全组放行：…」；`changed` 不受影响，`status` 仍 `ok`，step1/step2 照样 PASS | 照那行 `notes` 去云厂商控制台放行端口；装了 ufw 但没 `enable` 的机器按「没有」处理（判据是 `ufw status` 的 `Status: active`） |
| 手工卸载过 v3 的机器上残留 `/tmp/hy2-watchdog-*` | 不报漂移（`/tmp` 不在扫描范围，Task 5 第 5 步） | 无需处理，`/tmp` 是 tmpfs，重启即清 |

## 5. 结果记录
| 日期 | 执行人 | PASS/FAIL | 已知降级 | 备注 |
|---|---|---|---|---|
| | | | | |
