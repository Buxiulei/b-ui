# 更新日志

本项目的版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)：`MAJOR.MINOR.PATCH`。
发布 tag 一律为 `v<version>`（例：`v4.0.0`），tag 推送即触发 GitHub Actions 构建并生成 Release 与 `manifest.json`。
预发布走 `v<version>-rcN`（例：`v4.0.0-rc1` / `v4.0.0-rc2`）：`release.yml` 对 `-rcN` 标签置 `prerelease=true`，
而 `manifest.json` 的 `version` 按总纲 C4 必须是纯 semver，所以 rc1 / rc2 / 正式版共用本文件里**同一段**，
不为 rc 单独开段——rc 之间的差异补进该段的条目里。
`version` 的唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`；改版本必须同时在本文件加一段，`scripts/release/check-version.sh` 会在 CI 里卡住不一致（它只认 `## [<version>]` 这个标题，日期不参与校验）。
未发布的版本日期写「未发布」，由主理人打 tag 发版时替换成当天日期（UTC）。

## [4.0.0] - 未发布

v4 是一次完全重写：控制面与 Linux 客户端改为 Rust 单二进制，协议内核（Xray / hysteria / sing-box / Caddy）保持不变。
端口、标签、UUID、密码与 v3 完全一致，**现有订阅者无需重新导入**。

### 新增
- 单二进制控制器 `bui`：`install` / `upgrade` / `serve` / `reconcile` / `status` / `import-v3` / `auth-hook` / `menu` 一套 CLI，面板前端由二进制内嵌。
- 期望态 `state.json` + 对账器：装机与升级只有一条代码路，非受管的手工改动只报不改。
- Hysteria2 鉴权改 `auth.type: command`：加用户、到期、超限在建连时生效，内核不再整进程重启。
- Xray 用户增删与流量统计走 gRPC；流量按节点合并采样，限额真正执行。
- 住宅模块内置自动黑名单（候选 → 两次确认 → 生效 → 每日复核）与上游体检，relay 重启后重放已选上游。
- Rust 客户端 `bui-c`：单引擎（sing-box ≤ 1.14）SOCKS/TUN，`bui-c.timer` 每分钟自愈。
- 发布链路：`manifest.json`（总纲 C4 形状）钉住 `bui` / `bui-c` 与四个内核的裸二进制 URL 与 sha256，`bui upgrade --manifest-url` 可指定来源、`--rollback` 可回上一版二进制与最近一份 state 备份。
- 住宅 IP 池与槽位（spec §5.6）：池里每个 IP 是一个槽位，多个用户共用一个 IP、同一用户稳定走同一个 IP；每槽一个中继入站与一个 `hysteria-residential-<i>` 实例（端口 `40000+i`，跳跃区间 `41000-50000` 按槽位等分），Xray 住宅入站按用户路由到槽（每个用户一条路由规则，改分槽只增删受影响用户那一条，**xray 不重启、在线连接不断**）；新建用户分到负载最少的槽，`bui residential rebalance` / `assign` 手动调整，`bui residential slots` 与面板按槽显示 IP、当前出口、用户与指标；巡检按槽驱动（本槽优先，本槽不可用时临时借用最优 IP，本槽连续 3 轮恢复后切回）。

### 变更
- `install.sh` 从 869 行缩到 ≤130 行，只做架构识别、多源下载（GitHub Releases → 镜像）、sha256 校验，然后交给 `bui install`。
- 内核改为从上游 GitHub Releases 取静态二进制并校验 sha256，落在 `/opt/b-ui/bin/`；不再调用 `get.hy2.sh` / Xray-install / 发行版包，不再安装 Node.js。
- 定时任务全部收进守护进程：不再有 cron 行，也不再有 `hy2-watchdog` / `b-ui-resi-health` 等独立 timer。
- 升级到本版本时 `b-ui-relay` 与 `xray` 各重启一次（中继入站改成每槽一个 `slot-<i>`；xray 的住宅出站改名 `relay-slot-<i>`、`api.services` 追加 `RoutingService`），此后稳定；既有安装的槽位与用户分配在守护进程首次启动时按创建时间自动补齐，**非槽 0 的用户住宅 HY2 端口会变，需刷新一次订阅**。

### 移除
- v3 的 shell 与 Node 实现：`server/*.sh`（5 个）、`web/server.js`、`web/{package.json,package-lock.json,node_modules/}`、`b-ui-client.sh`、`b-ui-server.sh`、`version.json`、`test-hy2-tun-config.json`。前端三文件 `web/{index.html,app.js,style.css}` 保留，由 `bui` 内嵌。
- `update.sh` 的 25 个迁移块（重装即对账，迁移逻辑不再需要）。
- VLESS-WS-TLS「免流」节点与 `speedLimit` 字段（内核不支持按用户限速）。
- 客户端的三引擎互斥、死菜单簇、gum/fzf 依赖。
