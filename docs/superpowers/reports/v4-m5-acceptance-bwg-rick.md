# v4 M5 硬化验收报告（bwg-rick）——初稿

撰写：Opus（P5 Task 12 初稿）　裁决人：Fable　成稿日：2026-09-13（实测日期 2026-09-12 / 09-13）
被测版本：`bui 4.0.0-rc1` → `rc12`（= v4.0.0，同一份代码 f5571a9）（GitHub 预发布通道，按总纲裁决记录「发布：预发布与首推（2026-09-12）」放开；rc 由主会话打 tag、Actions 发 Release）
内核：rc2 起 sing-box 1.14.0（由 1.13.19 随 manifest 升级），其余三个内核以 rc8 的 `manifest.json` 为准（本稿未逐项抄录）
脚本：`scripts/ops/{soak-sample,soak-report,authhook-bench,authhook-report,authhttp-bench.py,upgrade-drill,v3-cutover}`、`scripts/{m1,m3}-acceptance.sh`（以 `v4` @ `7f723b1` 为准）

> 公开仓库：本报告只用 bwg-rick / bwg-tizi / baiyi 这几个别名，不写真实域名、IP 或用户名。原始 CSV 与日志留在测试机或仓库外，报告只写聚合结论。

## 0. 结论总览

| 判据（出处） | 实测（摘要） | 结论 |
|---|---|---|
| 72h soak：Xray RSS 无单调增长（spec §9 M5） | rc12 浸泡 2026-09-14T16:18:39Z 起在 bwg-rick 进行中；主理人 2026-09-15 裁决先发布 v4.0.0，浸泡跑满 72 小时后补判，问题进 4.0.1 | **发布后补测** |
| 72h soak：`bui` RSS < 50MB（spec §9 M5） | 同上；发布时已跑约 9 小时，bui RSS 约 16.7 MB、全部单元零重启（非判定结论） | **发布后补测** |
| 鉴权 200 建连/秒 p99 < 20ms（spec §9 M5） | rc7 http 模式：p99 12.38 ms，0 失败 | **PASS** |
| `upgrade` 演练成功（spec §9 M5） | rc1→rc8 全部经 `bui upgrade` 完成 | **PASS** |
| `--rollback` 演练成功（spec §9 M5） | 流程本身正确（bui 与四内核往返 .prev、版本指纹一致）；暴露孤儿链事故，rc5 修复并复验 | **PASS**（附事故，见 §5） |
| v3 恢复 < 10 分钟（spec §9「bwg-tizi 上线」，本计划 Task 12 Step 7） | bwg-tizi 2026-09-13：`restore` 15 秒完成，6 个服务 + 2 个定时器 active，7 个订阅与切换前逐字相同；随后一行命令装回 rc11，无漂移（见 §4） | **PASS** |
| 日志哨兵断网演练判据 ①–④（`2026-09-13-v4-log-sentinel.md` Task 10） | rc10：① 首条错误后 2.3 秒记事件、② 借到别槽 IP、③ 回环出网 IP 改变、④ 恢复后 350 秒切回；4 PASS / 0 FAIL（见 §7） | **PASS** |

M1–M4 与 IP 池的回顾见 §6，均已通过（IP 池有两项待测）。

## 1. 72h soak（spec §9 M5：Xray RSS 无单调增长、`bui` RSS < 50MB）

- **结论：待测。**
- rc11 浸泡：2026-09-13T15:25:36Z 起（bwg-rick，8 个单元 = 6 个固定单元 + hysteria-residential-1/-2，`--interval 60`），预计 09-16 15:25Z 采满。rc10 那轮只采了 6 个固定单元，随 rc11 升级作废。
- rc12 浸泡：2026-09-14T16:18:39Z 起（bwg-rick，8 个单元，先等旧采样器退出再挪目录，无残留 DONE）。**发布时浸泡进行中**：主理人 2026-09-15 裁决先发布 v4.0.0（与 rc12 同一份代码 f5571a9），浸泡继续跑满 72 小时并后台监控，发现的问题进 4.0.1。rick 保持 rc12 构建、不手动升级到 v4.0.0 构建，以免重启打断浸泡；对账器按**版本号**比对二进制（`reconcile/diff.rs` 的 `Artifact::Binary`），v4.0.0 清单与 rc12 同为 4.0.0、内核版本一致，所以发布后守护进程每日自检拉到新清单也不会自动重装。
- 采样器重启的坑：kill 旧采样器后，bash 要等当前的 `sleep 60` 结束才跑 EXIT trap；这时旧目录已被挪走、新目录同名，旧采样器把 `DONE`（reason=signal）写进了新目录。已改名为 `DONE.stray-from-rc10-sampler`。以后先等旧进程退出（按 pid 轮询 `kill -0`）再挪目录。
- 首轮：2026-09-12 14:57 UTC 起采样。窗口内 rick 被连续升级（rc 迭代），`b-ui` 出现 11 个 pid、`xray` 出现 3 个。RSS 曲线在每次重启时归零，斜率与「首末 1/4 均值」都失去意义，所以整轮**作废**，不出判读表。
- 重跑计划：日志哨兵合并后，在打出的那个 rc 上，从升级完成算起采满 72h，**窗口内不再升级、不重启受管单元**。
  ```bash
  ssh bwg-rick 'nohup bash /opt/b-ui/ops/soak-sample.sh --hours 72 --interval 60 \
      --units "xray b-ui hysteria-server hysteria-residential hysteria-residential-1 hysteria-residential-2 b-ui-relay caddy" \
      --out /var/log/bui-soak >/dev/null 2>&1 & echo started'
  # Monitor 盯 /var/log/bui-soak/DONE（until 循环），不在主会话里干等；结束后：
  ssh bwg-rick 'cat /var/log/bui-soak/DONE; bash /opt/b-ui/ops/soak-report.sh /var/log/bui-soak/soak.csv --expect-rows <72×60×单元数>'
  ```
  `soak-sample.sh` 默认只采六个固定单元。rc6 起 rick 是三槽三实例，要显式传 `--units` 把 `hysteria-residential-1/-2` 带上，`--expect-rows` 也要按实际单元数算：6 单元是 25920，8 单元是 34560。
- 判读口径不变：`xray` 的 `slope_kb/h` 不显著为正，`b-ui` 的 `max_MB < 50`。
- 窗口作废本身就是一条结论：soak 必须在「发版冻结」之后跑。以后每次复采都要在 `DONE` 里核对 pid 个数，每个单元必须只有 1 个。

## 2. 鉴权压测（spec §9 M5：200 建连/秒 p99 < 20ms）

| 轮次 | 模式 / 版本 | 脚本 | 目标速率 × 时长 | 次数 | 失败 | p50 | p95 | p99 | 判读 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 命令钩子 `auth.type=command`，rc3（static-pie） | `authhook-bench.sh` | 200/s × 300s | 59997 | 0 | 18.36 ms | 31.73 ms | 42.06 ms | FAIL |
| 2 | 命令钩子，rc4（非 PIE 静态构建 + 钩子路径去线程，`2b13d4c`） | `authhook-bench.sh` | 200/s × 300s | 60000 | 0 | 16.22 ms | 28.66 ms | 37.39 ms | FAIL |
| 3 | `auth.type=http`（守护进程进程内应答），rc7 | `authhttp-bench.py` | 200/s × 300s | 60000 | 0 | 0.71 ms | 2.06 ms | 12.38 ms | **PASS** |

- 第 3 轮时守护进程 RSS 为 15.3 MB（< 50 MB）。
- 命令钩子模式的地板在于**每次登录都新建一个进程**。rick 只有 2 vCPU，第 1 轮到第 2 轮已经把单次调用的固定开销压掉一截，p99 仍有 37 ms，再往下压没有空间。
- 处置：主理人 2026-09-13 批准 Hysteria2 鉴权默认改为 `auth.type=http`，由守护进程在 `127.0.0.1` 的独立端口上进程内应答，判定复用 `auth_hook::decide`，出错一律拒绝（fail-closed）。命令钩子保留为开关 `bui set hy2-auth command`（总纲裁决记录，实现 `40d4c86` / 合并 `783da34`）。代价已写进裁决：守护进程重启的约 1 秒窗口内，新登录会被拒。
- 与计划骨架的差异：
  - 计划原本要在 soak 第 2 小时、第 36 小时各压一轮。soak 作废（§1），这两轮按版本迭代的实际顺序记录在上表。
  - 实际速率、max、峰值 fd / 进程数、`/bin/echo` harness 基线，本稿都没有收到数据，未记录。
  - soak 重跑时是否在窗口内再压一轮 http 模式，由 Fable 定。
- 结论：**PASS**（http 模式）。spec §3.2 的 `userpass` 退路没有启用：http 模式已经达标，不需要它。

## 3. 升级 / 回滚演练（spec §9 M5：`upgrade` 与 `--rollback` 各演练成功）

### 3.1 升级

- **判据**：`bui upgrade` 演练成功；内核随 manifest 升级（spec §7）。
- **实测**：
  - rc1→rc2→…→rc8 全部在 rick 上经 `bui upgrade` 完成，用的都是默认 URL。
  - GitHub `latest` 返回 404（全是预发布）时，回退到最新预发布（`2c818e2`）。
  - 同版本号、不同 sha256 的产物，被识别为「同版本的新构建」并照常升级（`c61b8db`）。
  - rc2 这一跳顺带把 sing-box 从 1.13.19 升到 1.14.0，四个内核的 `.prev` 都保留了下来（`f3f08f0`）。
- **与计划 Step 2/6 的差异**：计划原本用「CI 产物 + 本机 `http.server` + 造一个 4.0.1 补丁版」来演练。总纲裁决放开 rc 预发布之后，实际走的是真实的发布通道，所以既没造 4.0.1，也没本机托管。「内核随 manifest 升级」由 rc2 的 sing-box 升级覆盖。计划骨架里逐个二进制的 sha 比对表，本稿未收到数据。
- **结论**：**PASS**。
- **rc 选取缺陷（rc11 修复）**：GitHub releases 列表不按创建时间排序（2026-09-13 实测 rc9、rc8、rc7、rc10、rc6），rc10 及以前的无参 `bui upgrade`、一行安装、`bui-c update`、bui-c 安装都取列表里第一个预发布。rick 在 rc10 上无参升级时被「升级」回了 rc9（同版本不同构建）。rc11 按 (x, y, z, N) 数值取最大。**已装 ≤ rc10 的机器要先用 `bui upgrade --manifest-url …/v4.0.0-rc11/manifest.json` 升一次**，之后无参升级才对：rick 上 rc11 的无参升级回「已最新」，tizi 的一行安装不钉版本装到 rc11。

### 3.2 回滚

- **判据**：`bui upgrade --rollback` 演练成功；回滚后 bui 与四个内核同升级前一致（总纲裁决「`--rollback` 也回内核」，P5 Task 10 判据 5）。
- **实测**（rc4，scripts/ops 的回滚演练）：
  - `--rollback` 与再次升级的流程都正确：bui 与四个内核在 `.prev` 之间来回，版本指纹一致。
  - 但过程中 `hysteria-residential` 被重启，暴露了一起事故：住宅 HY2 中断约 4 分钟，手工清链后恢复。详见 §5 事故 1。
- **修复与复验**：rc5 修复。真机上 `kill -9` 住宅实例后，重启 1 次即恢复，v4 / v6 两张 nat 表的孤儿链都被清掉；当时的验收 13/13。
- **结论**：**PASS**（回滚流程本身）。修复复验用的是 `kill -9` 住宅实例，**不是**再跑一遍完整的 `--rollback`。在最终 rc 上是否复跑一次回滚演练，由 Fable 定（见 §7）。
- **最终 rc 上的往返复验**（§8 裁决；bwg-tizi，2026-09-13 15:31Z）：rc11 → `upgrade --manifest-url …/v4.0.0-rc10/manifest.json`（同版本另一份构建）→ `upgrade --rollback` → 无参 `upgrade`。
  - 回滚恢复了 `manifest.prev.json` 与升级前那份期望态备份；bui 与四个内核的 sha256 指纹回到起点，逐项一致。
  - 每一步后 8 个受管单元都是 active，四个 hysteria 单元的 NRestarts 始终为 0；无参升级回「已最新」；`bui status` 无漂移，M1 24 PASS / 0 FAIL。
  - 覆盖范围：rc10 与 rc11 的内核相同，这一轮只换了 bui、没有重启 hysteria，所以孤儿链修复（§5 事故 1）仍然只由 rc5 的 `kill -9` 复验覆盖。端口跳跃规则：rick 在 iptables（每个实例一条 `HYSTERIA-PR-*` 链），tizi 在 nftables（18 条 redirect）。
- **结论**：**PASS**。

## 4. v3 恢复演练（spec §9「bwg-tizi 上线」的回滚路径，Task 12 Step 7）

- **判据**：`v3-cutover.sh restore` 退出码为 0，`real` < 10 分钟；恢复后 v3 的六个服务与快照里的定时器都是 `active`；订阅返回 200；重新装回 v4 后无漂移。
- **实测**（bwg-tizi，2026-09-13 15:22Z，从 rc10 的 v4 恢复到切换前 13:17Z 打的快照）：
  - `restore` 自己的汇总是「v3 的 6 个服务 + 2 个定时器全部 active」：hysteria-server / hysteria-residential / xray / b-ui-admin / b-ui-relay / caddy，hy2-watchdog.timer / b-ui-cert-sync.timer。快照里本来就是 inactive 的 b-ui-resi-health.timer 恢复后同样 inactive。
  - 从执行 `restore` 到 7 个用户的 `/api/sub` 与切换前抓下的订阅**逐字相同**，共 **15 秒**。
  - 随后按 README 的一行命令（不钉版本）装回 v4：装上的是 rc11，v3 导入 7 个用户。v3 遗留的一个调试 drop-in（把住宅 HY2 的 ExecStart 改成旧二进制加 debug 日志）与九个陌生文件挪进 `v3-backup/leftovers` 后 `bui status` 无漂移；M1 验收 24 PASS / 0 FAIL，订阅 7/7 与 v3 等价，M3 8 PASS / 0 FAIL。
  - 流量计数：恢复会回到快照时刻的 v3 计数；重新导入后按「恢复前 v4 计数 + 恢复窗口内 v3 新增」补回，7 个用户都补了。
- **结论**：**PASS**。

## 5. 事故与修复

### 事故 1：回滚演练后住宅 HY2 崩溃循环（孤儿端口跳跃链）

| 项 | 内容 |
|---|---|
| 时间 | 2026-09-12 20:33 UTC，rc4 回滚演练（`bui upgrade --rollback`）之后 |
| 影响 | bwg-rick 的 `hysteria-residential` 崩溃循环 52 次，住宅 HY2 线路中断约 4 分钟；直连 HY2 与两条 Reality 不受影响 |
| 现象 | 日志报 `ip6tables … -t nat -N HYSTERIA-PR-<hash>: … Chain already exists` |
| 根因 | Hysteria2 内置端口跳跃在启动时自建 nat 链 `HYSTERIA-PR-*`。进程被非正常终止（回滚流程里的重启）后链残留，新进程再 `-N` 同名链即 FATAL。这是 v3.5.14「端口跳跃孤儿链崩溃循环」在 v4 复活：v4 只在 `import-v3` 卸载末尾清过一次 |
| 临时处置 | 手工清链后恢复 |
| 修复提交 | `44287e7`（合并 `a3d1918`），随 rc5 发布：<br>① 新增 `bui hy2-prestart <config>`，两份 hysteria 单元加 `ExecStartPre=-…/bui hy2-prestart`，按本实例的 `listen:` 端口在 iptables 与 ip6tables 的 nat 表里**各自**清掉本实例的孤儿链（不碰其它实例的链，失败也永远退 0）；<br>② 看门狗识别「Chain already exists」签名：先清链，再 `reset-failed` + `restart`，同一单元 10 分钟冷却 |
| 复验 | rc5 真机 `kill -9` 住宅实例：重启 1 次即恢复，v4 / v6 两张表的孤儿链都被清掉；验收 13/13 |

### 事故 2：`runtime.json` 多写者竞态（同版本修复）

| 项 | 内容 |
|---|---|
| 时间 | 与事故 1 同期，排查中发现 |
| 影响 | 日志里间歇出现「runtime.json 落盘失败（忽略）」，运行期状态可能被旧快照覆盖；无服务中断 |
| 根因 | `store::write_atomic` 的临时文件名固定为 `runtime.json.tmp`。守护进程里多个任务和 CLI 进程并发写时，会互相截断、或把对方的临时文件 rename 走，导致 ENOENT；而错误被 `%e` 吞成了一句话 |
| 修复提交 | 同一个 `44287e7`：runtime 改用自己的 `persist()`，临时文件名带 pid 与序号，失败即清理，每一步都带上下文；`update()` 的写锁覆盖整个「读-改-写盘」 |
| 复验 | 单元测试：并发落盘后不留临时文件、失败原因点名路径；rc5 起 rick 在跑 |

## 6. M1–M4 与 IP 池回顾（M5 的前提）

### M1 控制面闭环（spec §9 M1）

| 判据 | 实测 | 结论 |
|---|---|---|
| v2rayN 四节点可连 | 主理人 2026-09-12 22:5x CST 真机确认：现有订阅不重导即可连四个节点，住宅线路可上 Google / YouTube | PASS |
| 订阅逐项等价、sing-box / xray / caddy 配置校验、二次 install 零变更、体检无漂移 | 由 `scripts/m1-acceptance.sh` 机器化覆盖。当时 13 项全过；后续扩到 24 项（外部站点、端到端鉴权、槽位），rc8 上 24/24 | PASS |

### M2 住宅模块（spec §9 M2；端口一条按总纲裁决修订），rc8

| 判据 | 实测 | 结论 |
|---|---|---|
| 24h 内学到支付域名 | stripe / pay.google / paypal 在三个上游上均为待生效，另有自动 3 条、候选 41 条 | PASS |
| 端口：探测结论与实测一致（原为「24h 内学到 `ports_allowed`」，按实测修订） | Decodo SOCKS5 实际放行 22 / 993 / 5228 / 853 / 8080，只拦 SMTP 465，与官方文档不符 ⇒ 无白名单可学，`ports_allowed=None` 是正确结论 | PASS |
| relay 日拒绝数较 v3 基线降 ≥ 90% | relay 上游侧错误 24h：v4 1660，v3 为 34344 / 20538，分别降 95% / 92% | PASS |
| relay 重启后选中不变 | 重启 relay 后，`resi-pool` 锁定与 slot-pin 都被重放恢复，重放失败 0 次（修复 `d5da722`：退避重试 + 覆盖各槽 selector） | PASS |
| pin 立即生效 | pin 即时生效，unpin 即移除 | PASS |

另记（不是判据）：UDP 目标先解析成 IPv4 后，Decodo 的 code=8 拒绝归零（`ffdd9d6`）。

### M3 用户与流量（spec §9 M3），rc2 起跑 `scripts/m3-acceptance.sh`，8/8 PASS

| 判据 | 实测 | 结论 |
|---|---|---|
| 加用户时 `NRestarts` 不变、在线会话不断 | 三个内核的 NRestarts / MainPID 都不变，邻居会话不断 | PASS |
| 100MB 已知流量计数误差 ±5% | 偏差 +0.01%（口径 tx+rx） | PASS |
| 到期用户被拒并被踢 | 面板判 blocked，新登录被拒，既有连接 60s 内被踢断 | PASS |
| 重启守护进程计数不重复 | `restart b-ui` 后计数不重复 | PASS |

### M4 客户端（spec §9 M4），baiyi，另一会话验收，已合并（`295833b`）

| 判据 | 实测 | 结论 |
|---|---|---|
| 四节点 SOCKS / TUN 均通 | 四节点在 SOCKS 与 TUN 下均通 | PASS |
| 裸 IPv6 回落符合 spec | TUN 下 `curl -6` 失败，普通 `curl` 返回节点的 IPv4，v6 默认路由在 `bui-tun` | PASS |
| 杀 sing-box 一分钟内自愈 | `kill -9` sing-box 后 6 秒拉起；`systemctl stop` 后 40 秒由 timer 拉起 | PASS |
| 从 v3 客户端原地升级不丢节点 | `import-v3` 五个节点不丢 | PASS |

### IP 池与槽位（spec §5.6；判据见 `2026-09-13-v4-p3-ip-pool.md` 末尾），rc6

| 判据 | 实测 | 结论 |
|---|---|---|
| 三槽齐全、三个 `hysteria-residential*` 实例在跑 | 三槽三实例（40000–40002） | PASS |
| 各槽用户从各自的 IP 出网 | 各槽回环出网的 IP 各不相同 | PASS |
| `assign` 后 xray 不重启（D7，RoutingService 热加载） | `assign` 后 xray 不重启，规则经 RoutingService 热加载 | PASS |
| 拔掉一条上游后该槽用户被重分配、刷新订阅后可连 | 本稿未收到实测 | 待测 |
| `rebalance` 后面板与 CLI 的槽位表一致 | 本稿未收到实测 | 待测 |

## 7. 日志哨兵断网演练（spec §5.7；`docs/superpowers/plans/2026-09-13-v4-log-sentinel.md` Task 10）

- **结论：PASS**（rc10，bwg-rick 槽 1，2026-09-13 14:21:49Z 起跑，4 PASS / 0 FAIL，退出码 0，丢包规则由 trap 删净）。rc11 没动哨兵（只改 rc 选取与单协议直连权益），结论沿用。
- 实测：① 首条 relay 连接错误后 **2.3 秒**记事件（含借用）；② 槽 1 借到 resi-1；③ 回环出网 IP 改变；④ 删规则后 **350 秒**切回本槽 IP（限 660 秒）。
- rc9 的首跑 ① 用了 30.3 秒（超 15 秒）。根因是网关解析出 6 个 IPv4，旧的 TCP 检查逐个串行、每个 5 秒超时。rc10（`9e2183f`）改为轮询 2 秒、连接类门槛 2 条、网关 TCP 快探并发且总时限 3 秒、连不上即借用。
- rc10 的第一次演练（14:18:46Z）是**假失败**：它落在被中断的 rc9 演练于 14:12:08Z 借用之后的 600 秒动作冷却窗里。冷却表落盘、跨守护进程重启仍然有效，所以哨兵按设计不动作，①②③ 随之失败。以后同一上游的两次演练至少间隔 600 秒（看 `bui incidents` 里该上游最后一次动作的时刻）。
- 判据（照搬 Task 10，不增不改）：
  1. 从该上游第一条 relay 连接错误起，≤ 15 秒记下事件（含借用）。
  2. 该槽借用到其它 IP（`borrowed=true`，active ≠ 本槽）。
  3. 该槽用户的回环出网 IP 改变。
  4. 删掉丢包规则后，巡检在 660 秒内切回本槽（恢复后第 4 轮巡检）。
- 前置：
  - 切回口径按 D6（恢复后第 4 轮，660 秒）判，与哨兵计划终稿一致。
  - 演练期间该槽用户会断流数秒到数十秒（直到哨兵借到别的 IP）；主理人 2026-09-13 裁决生产测试不等低峰。
- 执行：
  ```bash
  scp scripts/ops/sentinel-drill.sh bwg-rick:/opt/b-ui/ops/
  ssh bwg-rick 'bash /opt/b-ui/ops/sentinel-drill.sh --self-test'     # 先自测
  ssh bwg-rick 'nohup bash /opt/b-ui/ops/sentinel-drill.sh --slot 1 >/var/log/bui-sentinel-drill.log 2>&1 & echo started'
  # Monitor 盯日志里的「N PASS / M FAIL」摘要；退出码 = FAIL 数
  ```
  丢包规则都带注释 `bui-sentinel-drill`，由 EXIT trap 兜底删除。演练结束后要确认 `iptables -S | grep bui-sentinel-drill` 没有输出。

## 8. 发版建议（初稿）

- **v4.0.0 已于 2026-09-15 发布**（主理人裁决「先发布、后浸泡」）：发布前 `pin-kernels.sh --check` 与 `check-version.sh v4.0.0` 均通过；72h soak 在发布后补测，结论与问题进 4.0.1。
- 裁决（2026-09-13）：在最终 rc 上再跑一遍完整的 `upgrade` → `--rollback` → `upgrade`。rick 在浸泡不能动，放在 bwg-tizi 上跑。
- 发布命令只由主理人执行，照计划 Task 12 Step 8 §5：
  - 先跑 `pin-kernels.sh --check`。
  - 在 `v4` HEAD 上打 `v4.0.0` tag。
  - 用 `gh release view` 核对 15 个资产。
- 已知问题（留 4.0.1，不阻塞发版）：`bui reconcile` 与菜单里的手动对账经 socket 提交后，立刻打印的是**上一轮**的对账报告——守护进程把请求放进 500ms 去抖队列异步执行，接口直接回了 `last_reconcile`。紧跟守护进程启动或文件变动时，会显示已经不存在的漂移。以 `bui status` 为准，或隔几秒再跑一次。
- bwg-tizi 上线（2026-09-13，rc11）复核：
  - 低峰窗口：主理人裁决生产测试不等低峰。
  - 快照与 `restore` 演练：已做，见 §4。
  - 外部站点：tizi 没有从 v3 导入的站点（M1 step5 SKIP）。
  - `bui-c` 的 `/packages/` 同步链路：本轮没有复核（M4 由另一会话在 baiyi 验收）。
