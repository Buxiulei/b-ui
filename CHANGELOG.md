# 更新日志

本项目的版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)：`MAJOR.MINOR.PATCH`。
发布 tag 一律为 `v<version>`（例：`v4.0.0`），tag 推送即触发 GitHub Actions 构建并生成 Release 与 `manifest.json`。
预发布走 `v<version>-rcN`（例：`v4.0.0-rc1` / `v4.0.0-rc2`）：`release.yml` 对 `-rcN` 标签置 `prerelease=true`，
而 `manifest.json` 的 `version` 按总纲 C4 必须是纯 semver，所以 rc1 / rc2 / 正式版共用本文件里**同一段**，
不为 rc 单独开段——rc 之间的差异补进该段的条目里。
`version` 的唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`；改版本必须同时在本文件加一段，`scripts/release/check-version.sh` 会在 CI 里卡住不一致（它只认 `## [<version>]` 这个标题，日期不参与校验）。
未发布的版本日期写「未发布」，由主理人打 tag 发版时替换成当天日期（UTC）。

## [4.0.1] - 未发布

Linux 客户端 `bui-c` 的数字菜单 v2：能删除节点、[5] 连接检查补齐到 v3.6.2、窄屏可用，以及多个会话同时操作时的互斥与中断后的自我收拾。

### 新增
- `bui-c` 菜单 [6] 删除节点：一次可以删多个（例如 `1-3`、`1,4`），先列出要删的节点再确认；删的不是当前节点时答 `y` 即可，不重启、不断网；删到当前节点要输入 `yes`，会优先换到不在被删节点那几台服务器上的节点，TUN 模式下断网几秒。
- `bui-c delete <名字>...`：在命令行删除节点，删到当前节点时用 `--switch-to <名字>` 指定换到哪个，脚本里加 `-y` 跳过确认。
- 删掉的节点会被记住：之后从面板、订阅或 v3 重新导入时先跳过它们，再问要不要加回；`bui-c import --with-deleted`、`bui-c import-v3 --with-deleted` 连删过的一起导，单独粘贴一条链接会直接加回。
- `bui-c` 菜单 [7] 更新与维护：检查更新、自动更新开关、从 v3 导入集中到这一页；有新版时主菜单在 [7] 后面挂 ★；按旧编号进来会提示新位置。
- 检查更新先说清最新版本、来源、本机版本和会替换什么，确认后才下载安装；答 N 什么都不动。
- [5] 连接检查逐项显示服务、本地端口、TUN、隧道、Google、YouTube、GitHub、百度直连、1MB 下载速度与评语、IPv4 出口（地理与风险分）和 IPv6，有问题时给出判断和下一步（再查一次 / 换个节点 / 看日志）。
- 两个 SSH 会话、或菜单与每分钟巡检同时改配置时会互相等待（最多 15 秒），等不到就说「稍后再试」且什么都不改；巡检遇到正在进行的操作跳过这一轮。
- 删除做到一半时断线或进程被杀，下次打开菜单或下一分钟巡检会按节点列表把代理收拾好，并在「上次」行说明。
- `bui-c import --user -`、`--sub -` 从标准输入读面板 token 或订阅链接，不留在 shell 历史里。
- 菜单在交互终端里每次回到主菜单都清屏重画，顶部「上次」行说明上一步做了什么；排版跟着终端宽度走，手机 SSH 的 40 列左右也能用。

### 修复
- 60 列终端下状态行的节点名不再折行，节点列表与各页文字在窄屏下不再撑出屏幕。
- 菜单不再接受全局 `-y`：`sudo bui-c -y` 进菜单做删除等操作仍然会问。
- 已从 v3 迁移过的机器在没有节点时，不再弹出从 v3 导入的邀请。
- 导入面板复制的 token 订阅链接后，节点名用面板里的用户名，不再把 32 位 token 露在节点列表、菜单和截图里。
- 面板不认导入地址（404）时不再误说「这个面板还没有节点接口」，改为提示从面板重新复制整条订阅链接；`bui-c import` 缺少导入来源时退出码改为 2（用法错误）。
- `bui-c update` 与别的会话同时更新时，不会拿自己早先的下载覆盖别处刚装好的版本——菜单会重新显示再问，命令行退出码 1 并提示再跑一次；没有活动节点时更新不再重启代理。

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
- `bui-c import-v3`：先备好 sing-box 并用 `sing-box check` 自检，最后才卸 v3 单元（拿不到内核即中止、v3 一字不动）；新增 `--panel` / `--mode` 与 `BUI_C_PANEL` 环境变量；profile 名沿用 v3 目录名；接受 v3 早期 `user%3Apass@` 形式的 hysteria2 链接（此前三个目录会被跳过）；重跑幂等，不再重复导入、不再删掉正在用的 `bui-tun`。
- `bui-c update` 与 `bui-c-install.sh`：GitHub `releases/latest` 404（仓库里只有预发布）时回退到最新的 `v<x.y.z>-rcN`；面板下发的 `bui-c-install.sh` 默认从该面板的 `/packages` 取制品（`Host` 头只认主机名形状）。
- `bui-c update` 与服务端 `bui upgrade` 同口径判断要不要换自身：版本号不同，或版本号相同但 manifest 里本机架构 `bui-c-linux-<arch>` 的 sha256 与已装二进制不同（rc 通道的同版本重建）都升级；`update --check-only` 会说明「有同版本的新构建」，菜单 ★ 同步。已装的 rc6 / rc7 自身还只比版本号，要重跑一次面板下发的 `bui-c-install.sh` 换上新构建，之后 `bui-c update` 才能跟上。
- `bui-c` 导入按连接身份去重：同一连接原地更新（活动节点会重新生效），同名不同账号另起 `-2` 并提示；中文面板用户名导入的节点名为 `<主机名>-<kind>`。
- `bui-c` 自动更新来源只认 https 且 `/api/nodes` 成功返回的 v4 面板；订阅导入不再改写更新源，换来源时会提示；网络错误不再把完整 URL（含用户名）打到屏幕与日志。
- `bui-c` 数字菜单经真机逐键测试后修订：服务控制改为「重启 / 最近日志」二级菜单，导入可直接粘贴面板或订阅地址，节点列表两行一组压到 80 列内，输错原地重问，主菜单空行不再退出，非 UTF-8 输入不再崩出，巡检异常改用中文说明。
- `bui-c` 命令行：错误信息改走 stderr，`--json` 输出以换行结尾，非终端输入时不打提示符，还没有节点时切模式只记下选择。
- `install.sh` 从 869 行缩到 ≤130 行，只做架构识别、多源下载（GitHub Releases → 镜像）、sha256 校验，然后交给 `bui install`。
- 内核改为从上游 GitHub Releases 取静态二进制并校验 sha256，落在 `/opt/b-ui/bin/`；不再调用 `get.hy2.sh` / Xray-install / 发行版包，不再安装 Node.js。
- 定时任务全部收进守护进程：不再有 cron 行，也不再有 `hy2-watchdog` / `b-ui-resi-health` 等独立 timer。
- 升级到本版本时 `b-ui-relay` 与 `xray` 各重启一次（中继入站改成每槽一个 `slot-<i>`；xray 的住宅出站改名 `relay-slot-<i>`、`api.services` 追加 `RoutingService`），此后稳定；既有安装的槽位与用户分配在守护进程首次启动时按创建时间自动补齐，**非槽 0 的用户住宅 HY2 端口会变，需刷新一次订阅**。

### 移除
- v3 的 shell 与 Node 实现：`server/*.sh`（5 个）、`web/server.js`、`web/{package.json,package-lock.json,node_modules/}`、`b-ui-client.sh`、`b-ui-server.sh`、`version.json`、`test-hy2-tun-config.json`。前端三文件 `web/{index.html,app.js,style.css}` 保留，由 `bui` 内嵌。
- `update.sh` 的 25 个迁移块（重装即对账，迁移逻辑不再需要）。
- VLESS-WS-TLS「免流」节点与 `speedLimit` 字段（内核不支持按用户限速）。
- 客户端的三引擎互斥、死菜单簇、gum/fzf 依赖。
