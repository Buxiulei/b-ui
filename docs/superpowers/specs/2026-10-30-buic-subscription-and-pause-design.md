# bui-c：订阅来源管理与代理暂停/恢复（4.3.0）

2026-10-30 主理人提出：`bui-c` 没有删除订阅链接的入口（只能手改 `/opt/bui-c/profiles.json`），
也没有「停止代理、恢复直连」的入口（`systemctl stop bui-c.service` 会被每分钟的
`bui-c check` 拉回来）。本设计只做这两件事，不动节点导入/刷新与引擎。

## 1. 订阅来源

「订阅链接」在 4.x 的唯一落点是 `profiles.json` 的 `panel`（`base_url` + `username`，
`username` 是 `/api/nodes/<token>` 的最后一段，等价凭据）。`--sub` 导入的节点只记
`source: subscription`，**不存 URL**，所以没有可删的链接——本次只处理 `panel`。

- `bui-c subscription`：打印当前来源（`base_url` + 打码后的 `username`，如 `0123…cdef`）、
  以 `ApiNodes` 为来源的节点数；未设置时打印「未设置订阅来源」。
- `bui-c subscription remove`：确认（`-y` 跳过）后把 `profiles.panel = None` 写回，
  **保留**全部节点、墓碑、active、模式与端口。打印后果：不再从面板自动刷新节点；
  每日自更新的 manifest/二进制优先来源回落到 GitHub（面板不再是首选）。
- 菜单 `[7] 更新与维护` 新增 `[4] 订阅来源`：显示同样的信息，`[1] 清除订阅来源`
  （确认）→ 清除后回 `[0]`。没有来源时该页只显示「未设置」。
- 非目标：不删除单条节点（已有 `delete`）、不迁移 `Source` 标记、不新增「订阅 URL」字段。

## 2. 停止代理 / 恢复直连

### 2.1 状态

暂停是**用户意图**，不是期望配置：落一个标记文件 `/opt/bui-c/paused`（0600，内容一行
RFC3339 + 触发来源，便于事后排查）。`Paths` 新增 `paused()`；`Profiles` 不动
（旧版本读到本文件仍逐字节兼容）。`uninstall` 清掉它。

### 2.2 `bui-c stop`

1. 需要 root（与其它改机器子命令一致）。没有单元文件且没有节点时提示「本机没有安装代理」
   并返回错误，不写标记。
2. 写标记 → `systemctl disable bui-c.service` → `systemctl stop bui-c.service`。
   **timer 保持 enabled/running**：巡检仍会每分钟跑一次（只记一行「已暂停」日志），
   用户 `bui-c start` 后无需再 enable timer。
3. 打印「已停止代理，流量恢复直连；`bui-c start` 恢复」。重复执行幂等。

### 2.3 `bui-c start`

1. 没有 active 节点：提示先导入/切换，错误返回，**不**清标记。
2. 清标记 → `apply_with_ufw`（装内核 → UFW → `Engine::apply`，与导入同一条路径）→
   `systemctl enable bui-c.service`。timer 不 active 时 `systemctl start bui-c.timer`。
3. 已经在跑且没有标记：打印「代理已在运行」，不重启。

### 2.4 与既有逻辑的交点

- `check`：标记存在 → 新 `Verdict::Paused`，**不探测、不重启、不自更新**，只写一行日志
  （每分钟一次，journald 可见）。这是「停止后不被 timer 拉回来」与「重启机器后
  `bui-c.service` 被 enable 拉起」的唯一判据。
- 显式激活类动作（`switch`、`import --activate`、`mode`、菜单里等价的切换）先删标记：
  人选了节点就是要跑。`delete`、`status`、`list`、`subscription` 不动标记。
- **暂停期间除上述显式激活外的 apply 一律跳过**：`apply_with_ufw` 见到标记只打一行
  「配置未应用，`bui-c start` 后生效」并返回 no-op。覆盖「无激活的导入刷新活动节点」、
  v3 导入、删除后切节点等路径——它们可以保存配置，但不会把服务拉起来。
- 菜单 `[5] 连接检查` 与 `[4] 服务控制 → [1] 重启` 在暂停时都不执行修复/重启，只提示
  用 `[3] 恢复代理`；菜单 `[7] → [1]` 的安装段与 CLI `bui-c update` 同样拒绝。
- `update`：标记存在时 `bui-c update` 直接拒绝（「代理已停止，先 bui-c start」）；
  巡检里的自动更新已被 `Verdict::Paused` 跳过。`--auto on|off` 不受影响。
- `status` 与菜单状态行：标记存在时显示「已停止（直连）」；菜单 `[4] 服务控制` 增加
  `[3] 停止代理` / `[3] 恢复代理`（按标记切换文案），行为与 CLI 相同。
- 删除最后一个节点（现有 teardown）保持原语义（stop + disable）；标记存在时不清除，
  下次 `import --activate` 会清。

## 3. 验收

- 单测（FakeSys，禁止联网）覆盖：`stop` 的命令序列（disable → stop、timer 不动）、
  `start` 的 enable/apply、`check` 在标记下零探测零重启、`subscription` 打码输出、
  `subscription remove` 只清 `panel` 且节点/墓碑/active 不变、`update` 在暂停时拒绝。
- 真机（开发机）：`bui-c stop` 后 `systemctl is-active` 非 running、`bui-c check` 不拉起、
  出口恢复直连；`bui-c start` 后节点出口恢复；`bui-c subscription remove` 后
  `jq .panel profiles.json` 为 null、`bui-c list` 节点还在。
- 版本 4.3.0（新功能）；CHANGELOG 一段；`cargo fmt/clippy/test` 全绿。
