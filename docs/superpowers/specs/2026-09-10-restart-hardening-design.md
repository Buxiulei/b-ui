# 服务端重启硬化与订阅端口修正设计（v3.6.0 追加三项）

- 日期：2026-09-10
- 目标版本：v3.6.0（与 IPv6 接管、住宅硬化同版本，独立提交）
- 状态：已批准，待实施
- 来源：2026-09-10 服务端架构体检报告 P0-1 / P0-2 / P0-3（报告页：`B-UI 服务端体检`，主理人已选定并入）

## 1. 背景

主理人反馈 v2rayN 日志经常出现连接失败。体检把三个能在服务端确认的根因排在最前：

| # | 问题 | 证据（当前工作树） |
|---|---|---|
| P0-1 | `/api/sub/` 给 v2rayN 的「HY2直连」链接把 `mport=20000-30000` 写死，且 `mport` 无条件拼接。服务器安装时没开端口跳跃（`config.yaml` `listen: :PORT` 单端口）或用了自定义区间时，客户端往没人监听的端口发包，必现 `timeout: no recent network activity`。同文件的 sing-box、Clash 生成器和 `web/app.js` 都正确读了 `cfg.portHopping`，只有这条路径遗漏。 | `web/server.js:1640`（`mport=${hopRange}` 无条件）、`:1651`/`:1659`（字面量 `"20000-30000"`） |
| P0-2 | 任何用户增删改（含只改限额/到期日）都无条件重写三份配置并硬重启 `hysteria-server`、`hysteria-residential`、`xray`，全部在线用户瞬断；重启用 `execSync` 阻塞面板事件循环，`hysteria-residential` 的 `reload-or-restart` 因无 `ExecReload` 退化为 restart。 | `web/server.js:241-253`（`saveUsers`）、`:265`、`:276`、`:337` |
| P0-3 | cron 每 6 小时的 `update.sh auto` 只要版本号变了就无条件重启 `b-ui-admin`、`hysteria-server`、`hysteria-residential`、`xray` 四个服务，不看变更的是否是运行相关文件；两条 cron 整点齐步走，无抖动。内核自更新在安装脚本失败时也照样重启。 | `server/update.sh:2012-2015`、`:1894-1895`、`:1801`/`:1813` |

附带一项来自住宅 Task 2 的顾虑：`flock` 成为新依赖但安装期无检查；同类静默问题还有 `jq` 缺失时 userpass 认证迁移无声跳过（体检 P1-5）。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|---|---|---|
| 端口跳跃的真源 | `config.yaml` 的 `listen:` 行：有 `,START-END` 区间即启用且用该区间；无区间则沿用 `port-hopping.json` | Hysteria2 v2.9+ 的多端口 `listen` 就是实际监听，比 json 更可信；json 仅作为老式 iptables 路径的兜底，不制造回归 |
| `mport` 拼接 | 仅在 `cfg.portHopping.enabled` 时输出；住宅实例固定 `41000-50000`（`core.sh:531` 写死 `listen: :40000,41000-50000`） | URI 规范里 `mport` 可选 |
| 重启触发 | 三个 `update*Config()` 先生成新内容，与磁盘现有内容逐字节比较，相同则不写不重启 | 与 `core.sh:965` 证书同步的 `cmp` 范式一致 |
| 重启方式 | `spawn("systemctl",["restart",unit],{detached:true,stdio:"ignore"}).unref()` | 已有范式 `web/server.js:2373`；不再阻塞事件循环 |
| 不用 Xray HandlerService 热增删用户 | 本次不做 | 体检列为 M 成本；先用 diff 门控消掉"改限额也重启"的大头 |
| `auto_update()` 重启门控 | 按下载前后文件哈希得到变更集：`web/*` 变了才重启 `b-ui-admin`；`hysteria-*`/`xray` 一律不在这里重启（它们的配置迁移块 `apply_systemd_configs()` 本就按需重启；`residential-helper.sh reapply` 负责中继） | 版本升级改的是脚本与面板，代理核心的运行配置不受影响 |
| cron 抖动 | `auto`/`kernel` 非交互执行时先 `sleep $((RANDOM % 900))` | 避免机队整点齐步重启；交互/手动执行不延迟 |
| 内核自更新 | 安装后重新读取版本，只有版本真的变了才重启该服务 | 安装脚本失败时不再白重启 |
| 依赖检查 | `install.sh` 安装依赖后逐个 `command -v` 复查并把 `flock` 纳入；`core.sh`/`update.sh` 的 userpass 迁移在缺 `jq` 时 `print_warning` 而不是静默 `return 0` | 静默跳过是体检 P1-5 的根因 |

## 3. 组件设计

### 3.1 P0-1 `web/server.js`

- `getConfig()`（约 `:380-390`）读完 `port-hopping.json` 后，解析已读入的 `config.yaml` 文本的 `listen` 行：
  ```js
  // v3.6.0: hysteria 实际监听 (listen: :PORT[,START-END]) 是端口跳跃的真源；无区间时沿用 port-hopping.json
  const lm = hc.match(/^listen:\s*:?(\d+)(?:,(\d+)-(\d+))?\s*$/m);
  if (lm && lm[2]) portHopping = { enabled: true, start: parseInt(lm[2]), end: parseInt(lm[3]) };
  ```
  `hc` 为 `getConfig()` 内已有的 `config.yaml` 内容变量（按实际变量名）。
- `/api/sub/`：`buildHy2Url` 改为 `hopRange` 为空时不拼 `mport`：
  ```js
  let qp = `sni=${serverHost}&insecure=0`;
  if (hopRange) qp += `&mport=${hopRange}`;
  ```
  两处直连调用改传 `directHop`：`const directHop = cfg.portHopping?.enabled ? `${cfg.portHopping.start}-${cfg.portHopping.end}` : null;`。住宅两处保持 `"41000-50000"`。
- sing-box/Clash 生成器与 `web/app.js` 无需改（已读 `cfg.portHopping`）。

### 3.2 P0-2 `web/server.js`

- 新增：
  ```js
  // v3.6.0: 非阻塞重启（已有范式 :2373）；失败只记日志
  function restartServiceAsync(unit) {
      try { spawn("systemctl", ["restart", unit], { detached: true, stdio: "ignore" }).unref(); }
      catch (e) { log("ERROR", `restart ${unit}: ${e.message}`); }
  }
  ```
- `updateHysteriaConfig(users)` / `updateHysteriaResidentialConfig(users)`：生成 `next`（替换 auth 段后的全文）；`if (next === c) return;` 否则写盘 + `restartServiceAsync("hysteria-server" | "hysteria-residential")`。
- `updateXrayConfig()`：`const next = JSON.stringify(c, null, 2);` 与磁盘原文 `raw` 比较（`raw` 为读入的原始字符串）；相同则 return；否则写盘 + `restartServiceAsync("xray")`。
- `saveUsers()` 逻辑不变（仍按用户表调用三者），行为由 diff 门控决定。
- 效果：只改限额/到期日 → 三个配置内容不变 → 零重启；改密码/新增用户 → 只重启内容变了的服务，且不阻塞面板。

### 3.3 P0-3 `server/update.sh`

- `auto_update()`：
  - 下载循环前对 `file_map` 每个 `local_path` 记录 `md5sum`（文件不存在记空）；下载后再算一次，得到 `changed_files` 列表，写进日志。
  - 把无条件的四行 `systemctl restart` 替换为：`changed_files` 中有 `web/` 前缀项 → `systemctl restart b-ui-admin`；否则不重启。`hysteria-server`/`hysteria-residential`/`xray` 不在此处重启（`apply_systemd_configs` 与 D 块按需重启）。日志写明"重启: b-ui-admin"或"无需重启"。
  - `reapply`、`ensure_cron_jobs` 保持。
- `auto_update_kernel()`：Hysteria2 与 Xray 分支在安装脚本跑完后重新读版本（同现有取版本命令），`[[ "$new_ver" != "$local_ver" ]]` 才 `systemctl restart`，否则记日志"安装未生效，跳过重启"。
- 抖动：主入口 `case` 中 `auto)` 与 `kernel)` 分支执行前：`[[ -t 0 || -n "${B_UI_NO_JITTER:-}" ]] || sleep $((RANDOM % 900))`。

### 3.4 依赖检查 `install.sh` / `server/core.sh` / `server/update.sh`

- `install.sh check_dependencies()`：`deps_map` 加 `flock:util-linux:util-linux`；安装命令后对每个 dep `command -v` 复查，缺失的 `print_error` 列出并 `exit 1`（`jq`、`curl`、`flock` 缺失会让后续静默失效，宁可停）。
- `core.sh apply_hy2_userpass_auth()`（约 `:660-663`）与 `update.sh` D6（约 `:997-1020`）：`command -v jq || return 0` 改为 `command -v jq || { print_warning "缺少 jq，hy2 认证仍走 http 回调面板（高并发 SPOF），请安装 jq 后重跑更新"; return 0; }`。

## 4. 错误处理与回滚

| 场景 | 处理 |
|---|---|
| `listen` 行格式不匹配（老配置/手改） | 正则不命中 → 沿用 json，行为同 v3.5 |
| 配置比较时文件读失败 | 现有 `try/catch` 记日志，不重启 |
| `spawn` 失败 | 记日志；下一次真变更再触发 |
| `md5sum` 不存在 | 极少见；退化为"全部视为变更"（重启 b-ui-admin） |
| 回滚 | 各自独立提交，`git revert` |

## 5. 验收标准

1. `node --check web/server.js`；`bash -n install.sh server/core.sh server/update.sh`。
2. P0-1：本地起 server.js，三种 `config.yaml`（`listen: :10000` 无 json；`listen: :10000,25000-26000`；`listen: :10000` + json enabled 默认区间）下，`/api/sub/<fusion 用户>` base64 解码后 HY2直连 链接分别：无 `mport`；`mport=25000-26000`；`mport=20000-30000`。HY2住宅 始终 `mport=41000-50000`。`/api/subscription/` 与 `/api/clash/` 的 hy2-direct 端口跳跃字段与之一致。
3. P0-2：PATH 前置 `systemctl` stub 记录调用；登录取 token 后 `PUT /api/users/<u>` 只改 `trafficLimit` → stub 无记录、三份配置文件 mtime 不变；改 `password` → 记录含 `restart hysteria-server`（及 residential，若文件存在），不含 `restart xray`；新增带 uuid 用户 → 含 `restart xray`。
4. P0-3：抽取 `auto_update` 到测试壳（stub `download_and_validate` 按参数写入指定内容、stub `systemctl`/`apply_systemd_configs`/`ensure_cron_jobs`/`auto_update_kernel`/`npm`）：只改 `web/app.js` 内容 → 日志"变更文件"含它且 stub 只记录 `restart b-ui-admin`；只改 `b-ui-client.sh` → 无任何 restart；内核分支用 stub 的 `hysteria version` 前后相同 → 不重启。
5. 依赖：`install.sh` 提取 `check_dependencies` 在 PATH 缺 `flock` 的壳里跑 → 报错退出。
