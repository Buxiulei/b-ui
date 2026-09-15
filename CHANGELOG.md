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

## [4.0.0] - 2026-09-15

v4 是一次完全重写：控制面与 Linux 客户端改为 Rust 单二进制，协议内核（Xray / hysteria / sing-box / Caddy）保持不变。
节点凭据（端口、标签、UUID、密码）与 v3 一致，客户端已导入的节点照常可用；但订阅链接改为每用户一个随机 token，旧的用户名链接只在升级或 v3 导入后的宽限期内可用，**订阅者要在宽限期内换成面板里复制的 token 链接**。

### 新增
- 单二进制控制器 `bui`：`install` / `upgrade` / `serve` / `reconcile` / `status` / `import-v3` / `auth-hook` / `menu` 一套 CLI，面板前端由二进制内嵌。
- 期望态 `state.json` + 对账器：装机与升级只有一条代码路，非受管的手工改动只报不改。
- Hysteria2 鉴权改 `auth.type: command`：加用户、到期、超限在建连时生效，内核不再整进程重启。
- Xray 用户增删与流量统计走 gRPC；流量按节点合并采样，限额真正执行。
- 住宅模块内置自动黑名单（候选 → 两次确认 → 生效 → 每日复核）与上游体检，relay 重启后重放已选上游。
- Rust 客户端 `bui-c`：单引擎（sing-box ≤ 1.14）SOCKS/TUN，`bui-c.timer` 每分钟自愈。
- 发布链路：`manifest.json`（总纲 C4 形状）钉住 `bui` / `bui-c` 与四个内核的裸二进制 URL 与 sha256，`bui upgrade --manifest-url` 可指定来源、`--rollback` 可回上一版二进制与最近一份 state 备份。
- 住宅 IP 池与槽位（spec §5.6）：池里每个 IP 是一个槽位，多个用户共用一个 IP、同一用户稳定走同一个 IP；每槽一个中继入站与一个 `hysteria-residential-<i>` 实例（端口 `40000+i`，跳跃区间 `41000-50000` 按槽位等分），Xray 住宅入站按用户路由到槽（每个用户一条路由规则，改分槽只增删受影响用户那一条，**xray 不重启、在线连接不断**）；新建用户分到负载最少的槽，`bui residential rebalance` / `assign` 手动调整，`bui residential slots` 与面板按槽显示 IP、当前出口、用户与指标；巡检按槽驱动（本槽优先，本槽不可用时临时借用最优 IP，本槽连续 3 轮恢复后切回）。
- 删住宅上游时点名**必须重新获取订阅**的用户，并**按后果分三组**：槽位变动会改 HY2 住宅节点的端口（`40000+槽序号`）与端口跳跃区间（`41000-50000` 按槽位空间等分），这两样都写死在已下发的订阅里、客户端不会主动发现。`bui residential remove`（新增分组输出）、面板与哨兵事件三处同一份口径逐组列出用户名：①**原槽位已删除、已换槽** —— 旧端口不再通向他的槽；②**槽位序号被搬到 0 号**（只在删 0 号槽时出现）—— 端口下移、旧端口无人监听、连不上；③**端口跳跃区间被重切** —— 端口没变、连得上，但旧区间里划给别的槽的那一段会从错误的出口 IP 出去。只列手里那份订阅真的不能用了的人（判据是旧跳跃区间是否仍是本槽新区间的子集，区间只变宽的不算）。同一份名单落一条事件（新签名 `resi_slot_port_changed`），`bui incidents` 与面板「事件」卡都能回看；删上游的两个端点回包新增 `port_changed` 字段（三组各一列用户名）。
- 日志哨兵（spec §5.7）：守护进程每 5 秒增量读 relay / hysteria / xray / caddy / 自身的日志，住宅上游连不上（60 秒内 ≥2 条连接错误，凭据失效类 ≥3 条）时立即带外探测（整体限时 4 秒），确认不可用、或 4 秒内没能确认（relay 刚连报错误即佐证，「隧道挂死」这条路上探测本来也拿不到明确失败）就让压在它上面的槽**当场**借用别的健康 IP（不等 2 分钟巡检；恢复后仍由巡检切回：先连续 2 轮探通重新判健康，再攒满 3 轮，约 8 分钟），并告警「IP X 不可达，槽 i 已临时切到 Y」——借到的那条会**当下用同一个带外快探验证**（整体限时 4 秒）：确认可用才说「已临时切到」，确认不可用就把它判不健康再试下一个候选（单次处置最多验证 3 次），4 秒内没能确认就保留它并如实说「已切到 Y，未能在 4 秒内确认可用」（下一轮巡检复核）；验证逐条按 `host:port` 探，所以同一网关的兄弟端口（各自一个出口 IP）照样借得到；候选逐条都探不通就把该槽放回自己的 IP 并如实告警「候选 X、Y 都探不通，当前指向本槽 IP Z，当前无可用出口」；上游封 Google、hysteria 鉴权端口连不上、端口冲突 / 崩溃循环、xray gRPC 不可用、证书签不下来各有对应处置或告警。每次触发记一条事件（保留最近 200 条）：`bui status` 末尾显示最近 5 条，`bui incidents [--json] [-n N]` 查询，面板新增「事件」卡。不可达持续 30 分钟会建议替换该 IP；哨兵从不增删池成员。真机演练脚本 `scripts/ops/sentinel-drill.sh`。
- 订阅链接改为随机 token（设计文 `docs/superpowers/specs/2026-09-14-subscription-token-design.md`）：每个用户一个 **32 位小写十六进制**的随机订阅 token（16 字节随机），四个免鉴权端点 `/api/sub`、`/api/subscription`、`/api/clash`、`/api/nodes` 的路径末段只认 token，面板里复制到的就是 token 链接。老 v4 安装升级上来（守护进程首次启动时给存量用户补 token）与 `bui install --import-v3` 给旧的用户名链接 **7 天宽限期**；全新装机从不认用户名链接。token 对不上、用户不存在、用户名链接已过宽限期或已被轮换停用，一律回同一个 404 `{"error":"User not found"}`，不区分「用户存在但链接过期」。轮换（面板用户配置弹窗的「重置订阅链接与凭据」，即 `POST /api/users/{name}/rotate`）同时换 token、hy2 密码与 vless uuid，停用该用户的用户名链接，并把他已建立的 hy2 会话踢下线（Reality 已建立的连接没有踢的手段，等它自己断）。`bui set legacy-sub off` 立刻停用全部用户名链接，`bui set legacy-sub <RFC3339 时刻>` 改期；`bui status` 的「旧订阅链接」一行显示宽限期还剩多久（或已停用 / 已过期）。订阅路径的末段不进日志：`bui` 自身日志与 HTTP 请求 span 把 `/api/{sub,subscription,clash,nodes}/<末段>` 掩成 `***`；Caddy 对这四条路径 `log_skip` 不记访问日志，default 与站点两个 logger 再对 `request>uri` 与 `resp_headers>Location` 做同样的掩码（兜住上游 502 的错误日志与 `:80` 跳转）。

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
- 升级提示（运维）：已装 v4.0.0-rc6 / rc7 的 `bui-c` 需要在客户端机器上重跑一次面板下发的安装脚本 `curl -fsSL https://<面板域名>/packages/bui-c-install.sh | sudo bash` 才能换到新构建（旧版 `bui-c update` 只比版本号，看不见同版本重建）；之后同版本重建由 `bui-c update` 自己跟上。
- 回滚注意（运维）：rc12 回滚到 rc11 再升回 rc12 会**重新生成全部订阅 token**——rc11 写 `state.json` 时会丢掉它不认识的新字段，`bui upgrade --rollback` 恢复的又是 `state.backups/` 里最近的一份（最后一次写 `state.json` 之前的那一版；升级后守护进程补 token 就是一次写盘，此后没再写过的话恢复的就是升级前、没有 token 的那一版）——已发出的 token 链接全部失效、需要重发；轮换过的用户其「用户名链接已停用」标记也会被清零，在重新开出的 7 天宽限期里复活。`--rollback` 等于撤销 rc12 上最后一次写 `state.json` 的改动：没在那次改动里换过凭据的用户，节点凭据（hy2 密码、vless uuid）不受影响、回滚后仍能连上（`--rollback` 会重启 `hysteria-server`、`xray` 等内核单元，在线会话瞬断一次，客户端用原凭据自动重连）；若那次改动正好是轮换某个用户（之后再没有别的写盘），回滚恢复的是轮换之前的备份，该用户的 hy2 密码与 vless uuid 会退回旧值，轮换后重新导入的客户端会连不上，需要再轮换一次并重发。建议全员换成 token 链接后执行 `bui set legacy-sub off`。

### 移除
- v3 的 shell 与 Node 实现：`server/*.sh`（5 个）、`web/server.js`、`web/{package.json,package-lock.json,node_modules/}`、`b-ui-client.sh`、`b-ui-server.sh`、`version.json`、`test-hy2-tun-config.json`。前端三文件 `web/{index.html,app.js,style.css}` 保留，由 `bui` 内嵌。
- `update.sh` 的 25 个迁移块（重装即对账，迁移逻辑不再需要）。
- VLESS-WS-TLS「免流」节点与 `speedLimit` 字段（内核不支持按用户限速）。
- 客户端的三引擎互斥、死菜单簇、gum/fzf 依赖。
