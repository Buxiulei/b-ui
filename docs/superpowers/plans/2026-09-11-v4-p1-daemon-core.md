# v4 P1 守护进程核心实施计划（`crates/bui`）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 `bui` crate：期望态存储（`state.json` / `runtime.json`）、唯一改机器的对账器（文件 / 单元 / 单元状态 / sysctl / **内核模块** / 防火墙 / 内核二进制 / **符号链接** / 删除项）、系统硬化模块（sysctl 与 conntrack、静态 DNS、防火墙、SSH、systemd 单元与资源限制、证书 inotify、watchdog）、axum + unix socket 的 API 框架（`/api/login`、`/api/health` 与 CLI 自用的两个系统端点）、`install`（含卸载 v3、Caddy 数据迁移、初版 `auth-snapshot.json`）/ `import-v3` / `upgrade --rollback` / `status` / `menu` 的命令、守护进程的每日带抖动自检，以及 M1 的机器化验收脚本。

**Architecture:** 单二进制 `bui`，`clap` 子命令派发（`/usr/local/bin/b-ui` 符号链接裸跑进菜单）；`Store` 是进程内唯一的 `State` 持有者；`reconcile()` 是唯一改机器的函数，输入期望态、输出 `Vec<Artifact>`、diff 后只写有差异的项；所有碰真实系统的操作走**同步**的 `Host` trait（真实 `RealHost` / 内存 `FakeHost`），对账在 `tokio::task::spawn_blocking` 里跑，因此全部逻辑都能在单元测试里不碰真实系统地验证；P2/P3 通过实现 `Module`（`render` / `routes` / `spawn`）挂进同一个对账器、同一个 Router、同一组后台任务。下载走同步的 `Fetcher` trait（`Arc<dyn Fetcher>`，只在 `spawn_blocking` 里调用——`reqwest::blocking` 在 async 上下文会 panic）。

**Tech Stack:** Rust 1.93 stable（edition 2021）、tokio 1、axum 0.8、tower 0.5 / tower-http 0.6、hyper 1 + hyper-util 0.1（unix socket 客户端与服务端）、serde / serde_json / serde_yaml、clap 4（derive）、tracing + tracing-subscriber + tracing-journald、reqwest 0.12（rustls + blocking，只用于下载二进制）、sha2 + hex、inotify 0.11（stream）、nix 0.29（SO_PEERCRED / gethostname）、argon2 0.5、jsonwebtoken 9、rand 0.8、uuid 1、time 0.3、thiserror 2 + anyhow 1；dev：pretty_assertions、tempfile。消费 `bui-schema`（P0 交付）。

**Spec:** `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`（§1、§2.1、§2.2、§2.3、§2.4、§3.1、§3.4、§7、§11）；总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1 消费契约、C2 交付契约、C3 路径）；审计 `docs/superpowers/audits/2026-09-11-architecture-audit.md` §3.1/§3.2（每处移植的处置）。

## Global Constraints

- Rust stable ≥ 1.85（本机 1.93），edition 2021，`cargo clippy --workspace --all-targets -- -D warnings` 零告警，`cargo fmt --check` 通过。
- 发布目标 `x86_64-unknown-linux-musl`（本机已装）与 `aarch64-unknown-linux-musl`（CI）；依赖优先纯 Rust；TLS 用 `rustls` + `ring`，禁止 openssl。
- 协议集、端口、标签、UUID、密码与 v3 一致；订阅输出必须与 v3 golden 样本逐项相等（由 P0 保证，P1 不得绕过 `bui-schema` 自行拼配置）。
- sing-box 配置兼容 1.12–1.14：typed DNS servers、TUN `address` 数组、rule action（`sniff`/`hijack-dns`/`reject`）、`route.default_domain_resolver`、**不用** `rule_set` / `download_detour`。
- 凭据、密钥、密码不进 argv、不进日志（`tracing` 字段一律经 `redact::` 脱敏）；`state.json`、`runtime.json`、`auth-snapshot.json`、`config.yaml`、`config-residential.yaml`、`xray-config.json`、`singbox-relay.json` 一律 0600。
- 生产主机只用别名 bwg-rick / bwg-tizi / baiyi；公开仓库里不出现真实 IP、域名。测试与示例只用合成值：域名 `example.com`、IP `203.0.113.10`、伪装域 `www.bing.com`。
- 分支 `v4`，任务分支 `v4-p1-t<N>`，每任务一个 commit，格式 `feat(scope): …` / `test(scope): …` / `fix(scope): …`，尾部附 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`；不 `git commit -a`；不裸 `git stash`；不推送。
- 每个任务先写失败测试再实现。
- **`#![allow(dead_code)]` 的生命周期**：Task 1 在 `main.rs` 顶部加这一行（分任务落地期间被调用方先于调用方合并，非 test 构建下 `-D warnings` 会因 dead_code 失败），Task 17 收口时删除并修掉真正的死代码。除此之外全程不加任何 `allow`。
- **测试位置约定**：`bui` 是 bin-only crate，P1 的全部测试都是 crate 内单元测试（`#[cfg(test)] mod tests`），这样 `FakeHost` 与 `testutil::sample_state()` 能跨模块复用；不建 `crates/bui/tests/` 目录。`FakeHost` 与 `testutil` 用 `#[cfg(test)]` 声明，避免 `-D warnings` 下的 dead_code。
- **不碰真实系统的铁律**：单元测试只允许接触 `tempfile` 给的临时目录；任何 `systemctl` / `sysctl` / `chattr` / `ufw` / 网络下载都必须经 `Host` 或 `Fetcher` trait，测试里注入 fake。真实系统验证留给 M1 里程碑验收（bwg-rick）。
- v3 的 `server/`、`web/server.js`、`b-ui-client.sh` 在 P1 期间**只读**，作为移植参照，不修改、不删除（P5 最后一个任务才删）。

---

## 文件结构

```
crates/bui/Cargo.toml                      P1 依赖（Task 1）
crates/bui/src/main.rs                     入口：解析 CLI → 初始化日志 → 派发子命令
crates/bui/src/cli.rs                      clap 定义（Cli / Command）
crates/bui/src/logging.rs                  tracing 初始化（journald，回退 stderr）
crates/bui/src/redact.rs                   日志脱敏（URL 凭据 / 任意 secret）
crates/bui/src/paths.rs                    bui 侧路径helper（state.json / runtime.json / auth-snapshot.json / Caddyfile / 备份目录 / caddy 数据目录 / socket / v3-backup / CLI 符号链接）
crates/bui/src/util.rs                     fmt_rfc3339 / parse_rfc3339 / human_duration（三个纯函数，Task 1 建）
crates/bui/src/testutil.rs                 #[cfg(test)] sample_state()
crates/bui/src/state/mod.rs                pub mod runtime; pub mod store;
crates/bui/src/state/store.rs              Store（唯一 State 持有者；临时文件 + rename + 备份 10 份）
crates/bui/src/state/runtime.rs            Runtime / RuntimeData / WatchdogRecord / DriftItem / ReconcileReport
crates/bui/src/sys/mod.rs                  Host trait / CmdOut / Proto / parse_proc_net
crates/bui/src/sys/real.rs                 RealHost（std::fs + Command + /proc/net）
crates/bui/src/sys/fake.rs                 #[cfg(test)] FakeHost（内存 FS + 单元表 + 操作流水）
crates/bui/src/reconcile/mod.rs            Artifact / Unit / Verify / PortSpec / Facts / RenderCtx / Module / DaemonCtx / MANAGED_UNITS / LEGACY_UNITS
crates/bui/src/reconcile/diff.rs           Change / Plan / PlanInput / plan()
crates/bui/src/reconcile/apply.rs          BinaryInstaller / ApplyOutcome / apply()（含验证再重启、失败回滚）
crates/bui/src/reconcile/drift.rs          scan() / clean()（非受管项只报不改）
crates/bui/src/modules/mod.rs              pub mod certs, core_files, ssh, system, units, watchdog;
crates/bui/src/modules/core_files.rs       四内核配置 + Caddyfile（调 bui-schema render）
crates/bui/src/modules/units.rs            6 个 systemd 单元 + 单元状态 + v3 遗留项删除
crates/bui/src/modules/system.rs           sysctl / conntrack / modprobe / 静态 DNS / 防火墙
crates/bui/src/modules/ssh.rs              SSH 硬化（含 count_pubkeys）
crates/bui/src/modules/certs.rs            证书 inotify 监听 + 同步 + 间隔 10s 重启两个 hysteria
crates/bui/src/modules/watchdog.rs         60s 存活 + 端口探测 + 退避状态机
crates/bui/src/kernels/mod.rs              Manifest / Fetcher / 版本解析 / sha256 校验 / 原子替换
crates/bui/src/api/mod.rs                  `pub mod auth/health/state/system;` + `pub use state::{AppState, Event, EventBus};` + router()
crates/bui/src/api/state.rs                AppState / Event / EventBus（Task 4 建，只放类型，供 reconcile 与各模块引用）
crates/bui/src/api/auth.rs                 argon2 / JWT / LoginLimiter / require_admin 中间件
crates/bui/src/api/health.rs               HealthResponse / GET /api/health
crates/bui/src/api/system.rs               POST /api/reconcile、POST /api/services/:unit/:action
crates/bui/src/ipc.rs                      unix socket 服务端（0600 + SO_PEERCRED）与 CLI 客户端
crates/bui/src/serve.rs                    守护进程装配：模块注册、reconcile_once、去抖、10 分钟巡检
crates/bui/src/commands/mod.rs             pub mod harden_ssh, import_v3, install, menu, status, upgrade;
crates/bui/src/commands/install.rs         bui install（交互/非交互 → state → 初版 auth-snapshot → reconcile，幂等）
crates/bui/src/commands/import_v3.rs       bui import-v3（调 bui-schema::v3::import + 迁移 Caddy 数据 + 卸载 v3）
crates/bui/src/commands/upgrade.rs         bui upgrade / --rollback
crates/bui/src/commands/status.rs          bui status（经 socket 读 /api/health）
crates/bui/src/commands/menu.rs            sudo b-ui 数字菜单
crates/bui/src/commands/harden_ssh.rs      bui harden-ssh（只跑 ssh 模块的一次对账）
scripts/m1-acceptance.sh                   M1 机器化验收脚本（Task 18，bwg-rick 上跑；含 --self-test）
docs/superpowers/checks/2026-09-11-v4-m1-acceptance.md   M1 逐项检查清单与期望输出（Task 18）
```

**Task 1 一次建齐全部桩文件**（照 P0 Task 1 的做法）：上表每个 `crates/bui/src/**` 文件在 Task 1 就以 `//! placeholder filled by Task N` 建好，`main.rs` 一次写全 `mod` 行、`state/mod.rs`、`sys/mod.rs`、`reconcile/mod.rs`、`modules/mod.rs`、`api/mod.rs`、`commands/mod.rs`、`kernels/mod.rs` 一次写全 `pub mod` 行。这样后续任务只填自己的文件，不需要互相追加 `mod` 行，并行合并零冲突、任何单个任务合并后 workspace 都能编译。

## 并行编排

依赖是**编译依赖**（下游任务的代码里出现了上游任务定义的类型/常量/函数），已逐条按代码核对：

| 阶段 | 任务 | 依赖 | 依赖的具体符号 |
|---|---|---|---|
| 串行 | Task 1 | — | 全部桩文件与 `mod` 行、`Cli`/`Command`、`redact`、`paths`、`util`、`testutil::sample_state` |
| 并行 | Task 2（Store/Runtime）、Task 3（Host） | 各自只依赖 Task 1 | 2 用 `paths::{state_file,runtime_file,backups_dir}`；3 不用别的任务 |
| 串行 | Task 4（Artifact/diff/api 类型/受管单元常量） | Task 2、Task 3 | `Store`、`Runtime`（`DaemonCtx`/`AppState` 的字段）、`Host`/`Proto`/`CmdOut`、`DriftItem`/`ReconcileReport` |
| 串行 | Task 5（apply/drift） | Task 4 | `Artifact`/`Change`/`Plan`/`Verify`/`Unit`/`Facts`/`MANAGED_UNITS`/`LEGACY_UNITS` |
| 并行 | Task 6（system）、7（ssh）、8（units）、9（kernels） | Task 4、5（7 还用 `Store`） | 6/7/8 用 `Artifact` 建造链与 `MANAGED_UNITS`/`LEGACY_UNITS`；9 只用 `Host` 与 `BinaryInstaller` |
| 串行 | Task 10（core_files） | Task 9（+4、5） | `kernels::{Manifest, KERNELS}`；`bui_schema::render::*` |
| 并行 | Task 11（certs）、12（watchdog） | Task 10（+4、9） | 11 用 `kernels::sha256_hex`、`api::EventBus`、`DaemonCtx`；12 用 `core_files::RELAY_LISTEN_PORT`、`api::EventBus` |
| 并行 | Task 13（API 实现） | Task 4（+8 的 `MANAGED_UNITS` 已前移到 4） | `AppState`/`Event`/`EventBus`（Task 4 的 `api/state.rs`）、`Store`、`Runtime`、`Host`、`reconcile::MANAGED_UNITS` |
| 串行 | Task 14（unix socket）→ Task 15（serve 装配）→ Task 16（install / import-v3）→ Task 17（upgrade / status / menu）→ Task 18（M1 验收） | 14 依赖 13；15 依赖 6–14；16、17 依赖 15；18 依赖 17 | 收口链 |

**四条曾经互相打死的环已解开**（本次修订）：
1. `Event`/`EventBus`/`AppState` 从 Task 13 前移到 **Task 4 的 `api/state.rs`**（Task 1 已在 `api/mod.rs` 写好 `pub mod state;`，Task 4 只追加一行 `pub use state::{AppState, Event, EventBus};`）。于是 `Module::routes(&self) -> axum::Router<crate::api::AppState>` 与 `DaemonCtx.bus` 在 Task 4 就能编译，Task 13 不再被 Task 4 反向依赖。
2. 受管单元与遗留单元常量从 Task 5（`drift::MANAGED_UNITS`）与 Task 8（`units::MANAGED` / `units::LEGACY_UNITS`）合并成 **Task 4 的 `reconcile::{MANAGED_UNITS, LEGACY_UNITS}` 各一份**；Task 5、8、13 全部引用它，不再有两份内容不同的 `LEGACY_UNITS`。
3. Task 10 用 `kernels::{Manifest, KERNELS}`，因此 **10 排在 9 之后**（原表把 9、10 放同组并行是错的）。
4. Task 11/12 用 `kernels::sha256_hex`（9）与 `core_files::RELAY_LISTEN_PORT`（10），因此 **11/12 排在 10 之后**。

**P2/P3 在 Task 4（AppState/Module 契约）与 Task 13（API 实现）合并后即可启动**（总纲：「P1 的 state 存储 + API 框架两个任务完成后 → {P2, P3} 并行」；Task 2 的 `Store` 是 Task 4 的前置，所以这两项合并即意味着 state 存储已就绪）。P2/P3 追加自己的模块时只改 `serve.rs` 的 `modules()` 与 `modules/mod.rs` 的一行 `pub mod`，冲突可机械解决。

## 依赖与契约决策（已按 Fable 2026-09-12 裁决落实）

总纲 `2026-09-11-v4-master.md` 末尾「裁决记录」里与 P1 相关的每一条，都已在本计划里按裁决后的形态写死；批准本计划时按本节回写总纲 C2/C3。逐条对照：

| 裁决记录里的议题 | 裁决 | 本计划落地位置 |
|---|---|---|
| P1：`Host` trait 同步 + `spawn_blocking` | 批准 | Task 3 的 `trait Host`（全部方法同步）；Task 5/9/15/16/17 的每个调用点都在 `tokio::task::spawn_blocking` 里（`Fetcher` 同理） |
| P1：`Artifact` 扩三个变体（`UnitState`/`FirewallPorts`/`Absent`）与 `File{immutable,restart_key,verify}`、`Unit{name,action}` | 批准（细化不改名） | Task 4 的 `enum Artifact`；该批准范围内的其余字段细化见下表 |
| P1：额外落地 `POST /api/reconcile`、`POST /api/services/:unit/:action` | 批准 | Task 13 的 `api/system.rs`（单元名走 `MANAGED_UNITS` 白名单）、Task 17 的菜单只经这两个端点动手 |
| P1：unix socket 鉴权走 SO_PEERCRED | 批准 | Task 14 的 `ipc.rs`（socket 0600 + `SO_PEERCRED` 只认 uid 0） |
| P1：Caddy 改为自带静态二进制 + `XDG_DATA_HOME=<base>/caddy` | 批准 | Task 10 产出 `Artifact::Binary{caddy}`；Task 8 的 `caddy.service` 用 `{bin}/caddy` 且 `XDG_DATA_HOME` 与 `XDG_CONFIG_HOME` 都是 `/opt/b-ui/caddy`；Task 11 在 `<base>/caddy/caddy/certificates/` 下监听证书 |
| P1：Caddyfile 路径 | 按 C3 放 `/opt/b-ui/Caddyfile`，`caddy.service` 以 `--config /opt/b-ui/Caddyfile` 启动 | Task 1 的 `paths::caddyfile(&Paths)`、Task 8 的单元正文、Task 10 的 Caddyfile artifact。不再写 `/etc/caddy/Caddyfile`；发行版包自带的那份留在原地不动（v4 不再引用，也不在任何漂移扫描范围内） |
| P1：`import-v3` 与 Caddy 数据 | 复制 `/var/lib/caddy/.local/share/caddy/`（ACME 账号 + 证书）到 `/opt/b-ui/caddy/caddy/`，再停用发行版 `caddy`；避免重签与 Let's Encrypt 速率限制 | Task 16 的 `migrate_caddy_data()`：`uninstall_v3` 的**第一步**，复制完才 `systemctl stop caddy`，随后由对账写 v4 的 `caddy.service` 并拉起（用新的 XDG 目录）。FakeHost 测试 `caddy_acme_data_is_copied_before_the_distro_unit_is_stopped` 断言「先复制完、再停」的顺序 |
| P1：manifest 缓存于 `<base>/manifest.json`，拉不到不阻塞对账 | 批准；形状按 C4 | Task 1 的 `paths::manifest_file`、Task 9 的 `Manifest`（C4 形状）、Task 10 在 `manifest=None` 时不产出任何 `Binary`、Task 15 的 `load_cached_manifest` / `selfcheck_loop`、Task 16 第 3 步失败只警告 |
| P0：xray `clients[].email` = `user_id` | 已修（08a401f） | P1 不自己拼 xray JSON，直接落 `bui_schema::render::xray::config` 的产出（Task 10） |

**第二轮审查（2026-09-12）的逐条落地**——每条都改成了「实现者照着写就不会踩」的形态，评审时按这张表核对：

| 审查项 | 结论落在哪 |
|---|---|
| **B1** CLI 与总纲 C5 冲突、`MANIFEST_URL` 在 M1 必然 404 导致装不上机 | Task 1 的 `Cli`/`Command` 逐字照 C5（`install --non-interactive --answers`、`upgrade --version/--manifest-url`、废掉 `--channel`、`--version` 自己处理只打印版本号）；Task 9 新增 `MANIFEST_URL_TEMPLATE` / `MANIFEST_URL_ENV` / `manifest_url_for_version` / `resolve_manifest_url` / `manifest_url`，`HttpFetcher` 兼容 `file://` 与本地路径；Task 15 的 `selfcheck_loop`、Task 16 的 `run_with(.., manifest_url, ..)`、Task 17 的 `run_with` 全部用解析后的 URL；Task 16 的 `xray_program` 回退 PATH 并给出点名 `BUI_MANIFEST_URL` 的错误 + 内核不齐时的提示；Task 18 清单写明 M1 怎么托管 manifest。文首「manifest 地址的解析顺序」是这条的总口径 |
| **B2** `auth-snapshot.json` 形状与 C5 矛盾（P2 读不到用户 → 导入用户 fail-closed） | 文首契约段、Task 16 的 `auth_snapshot` 与两处断言、Task 18 的 step3b 全部改成 C5 的 `{"schema":1,"users":{…}}`（空快照 = `{"schema":1,"users":{}}`） |
| **B3** `start_paused = true` 缺 tokio `test-util` feature，Task 11/15 测试编译不过 | Task 1 的 `[dev-dependencies]` 加 `tokio = { version = "1", features = ["test-util"] }`（附理由，免得被后人删掉） |
| **B4** `ApplyOutcome.keys` 从未规定怎么填 → `restart_keys` 不落盘 → 每次加用户都重启 xray | Task 4 的 diff 规则 1/8 注明 `Plan.keys` 只是候选；Task 5 的 apply 第 1 步与第 10 步写明搬运条件（写盘成功才搬、没有防火墙不搬），Interfaces 的 `keys` 字段带上后果说明，新增 `restart_keys_are_recorded_only_for_the_writes_that_landed`；Task 15 的 `first_pass…` 补 `keys` 断言 |
| **B5** 对账会在 apply 中途 `systemctl restart b-ui` 杀掉自己 | Task 5 第 12 步把 `b-ui` 从同步重启循环里摘出、只置 `ApplyOutcome.self_restart_required`；Task 2 的 `ReconcileReport` 加同名字段；Task 15 新增 `finish_self_restart(ctx, report, in_daemon)`（守护进程 `restart --no-block`、CLI 同步）并在启动对账、去抖消费者、`reconcile_cli`、Task 16 的 install 四处调用；新增两条断言测试（apply 里不出现 `systemd:restart:b-ui`、守护进程的 `--no-block` 在报告落盘之后） |
| **B6** 全新装机的首张证书只能靠 6h 兜底轮询 | Task 11 建 watch 前 `create_dir_all`、`CertWatcher::rescan` 把新子目录补进 watch、新增 `PENDING_POLL_SECS`（30s）/ `poll_interval` / 独立的 `poll_loop`（`CertsModule::spawn` 起两个任务），新增 `start_paused` 时钟推进测试 |
| **B7** REALITY 的 `server_names` 与 `dest` 可能不一致 | Task 16 的 `generate_reality` 两项都留空占位，`build_state` 用 `reality.sni()` 同时填 `dest` 与 `server_names`；测试补 `assert_eq!(s.node.reality.server_names, vec!["www.bing.com"])` |
| 缺口：`min_upgrade_from` 没人实现 | Task 9 的 `Manifest` 加字段（`serde(default)`），Task 17 的 `plan_upgrade` 消费它 + 新增 `version_lt` 与两条测试 |
| 缺口：spec §7 的「按指定版本升级」 | 同 B1：`--version <x.y.z>` → `manifest_url_for_version` → 拉回后校验 `m.version == 请求版本` |
| 缺口：v3 的 iptables/nft 端口跳跃规则没人清 | Task 16 的 `flush_v3_portjump_rules`（移植 `core.sh:159-186`），排在 `uninstall_v3` 最后一步；两条测试；并写明 v4 运行中的 hysteria 自建的同名链不纳入漂移 |
| 缺口：受管目录里的陌生文件只扫 `<base>` 顶层 | Task 5 的 Interfaces 写明「有意收窄」+ 新增 `MANAGED_CONF_DIRS` 与 `stray_conf` 一类（四个 `/etc` 目录里 b-ui 前缀的陌生文件），一条测试 |
| 缺口：`chattr` 不被支持时体检永久 degraded | Task 5 的 `resolv_immutable` 只在 `chattr`+`lsattr` 都存在时才报，apply 第 2 步复核后记 note；三种情况的降级口径写死，逃生口是 `system.static_dns = false`；Task 18 清单新增「已知降级」一节 |
| 缺口：`--answers` 文件格式无人定义 | Task 16 的 `AnswersFile` / `load_answers` 给出逐字段格式与「文件里不放密码」的理由，一条测试；P5 的 `v3-cutover.sh` 照它生成 |
| **S1** 三个模块的 `Module::name()` | Task 7/8/11/12 的 Produces 里写死 `"ssh"` / `"units"` / `"certs"` / `"watchdog"` |
| **S2** `RemoveFile` 的 `chattr -i` 与 ops 断言冲突 | Task 5 第 11 步改成「仅当 `is_immutable` 为 true 才 `chattr -i`」，并把那条测试改名为 `removals_disable_and_stop_first_then_delete_then_daemon_reload` |
| **S3** FakeHost 的单元名归一化 | Task 3 第 5 步写死规则（补 `.service`、`fail_units` 裸名与全名都命中、`ops` 记原样名） |
| **S4** 第二次 install 的零写入 | Task 16 的 `fetch_and_install_kernels` 写明「内容相同则不写」 |
| **S5** `auth-hook` 不该初始化 tokio / tracing | Task 1 的 `main` 改成同步函数：`--version` 与 `AuthHook` 在建 runtime、初始化日志之前返回，其余走 `dispatch`；Task 17 的那一支变 `unreachable!` |
| **S6** 三条对账路径无互斥 | Task 15 的 10 分钟巡检改成往同一条 mpsc 投触发（守护进程里只有一个 consumer 调 `reconcile_from_ctx`）；Task 16 的 install 第 9 步在 socket 可用时改走 `POST /api/reconcile` |
| **S7** 两份 v3 单元表 | Task 16 的 `v3_units_never_touch_a_v4_managed_unit` 加断言 `V3_UNITS ⊂ LEGACY_UNITS` |
| **S8** systemd stop 发 SIGTERM | Task 15 的 `run` 的 `select!` 加 `SignalKind::terminate()` |
| **S9** `BASE_WHITELIST` 缺 `auth-hook.log` | Task 5 的白名单 12 → 13 项，drift 测试里也放一个该文件 |
| **S10** `caddy validate` 会碰家目录 | Task 10 的测试给 `Command` 设 `XDG_DATA_HOME` / `XDG_CONFIG_HOME` 为 tempdir |
| **S11** watchdog 把「单元不 active」判 Healthy | Task 12 的 `decide` 注释写明这是有意为之（与 spec §3.4 字面不同的地方、以及为什么），M5 按这段口径验收 |
| **S12** 「约 140 个测试」不准 | Task 17 第 6 步删掉总数，改成「以各任务 Expected 之和为准」；受影响任务的 Expected 逐个改准（T1 19、T5 35、T6 12、T9 9、T11 9、T15 12、T16 23、T17 16） |
| **S13** `harden-ssh` 与 spec §2.4 白名单冲突 | Task 7 写明放行理由（只渲染一个 artifact、不写 `.verify/` 与 `runtime.json`，与守护进程无交叉写），spec §2.4 的白名单扩一项、批准时回写 spec（回写的完整集合见 Task 7）；Task 17 的菜单第 7 项不再标「需守护进程」，测试同步 |

**第三轮审查（2026-09-12 晚）的逐条落地**——两条阻塞项都会让 bwg-rick 的 M1「体检无漂移 / 二次对账零变更」永远不可达，因此按「实现者照着写就不会踩」的形态改到了产出端，评审时按这张表核对：

| 审查项 | 结论落在哪 |
|---|---|
| **C1**（阻塞）没有 ufw/firewalld 的机器上 `OpenPorts` 每轮重规划：Task 5 第 10 步不搬 `firewall` key，而 Task 4 的 diff 规则 8 又靠这个 key 判「已放行」→ `changed` 每轮多一条 `firewall: …`、`keys["firewall"]` 永不出现。Task 15 的 `first_pass…second_pass_is_a_no_op`（断言 `keys.contains_key("firewall")` + `second.is_clean()`）与 `a_hand_edited_config_is_rewritten_and_only_its_unit_restarts`（断言 `changed == ["/opt/b-ui/config.yaml"]`）因此与 Task 5 互相打死；真机上 `bui reconcile --dry-run` 也会永远报改动（Task 18 step2 恒 FAIL） | 采纳修法 (a)「没有防火墙 → 只出提示、算 unchanged」：**Task 6 的 `SystemModule::render` 只在 `facts.ufw_active \|\| facts.firewalld_active` 为真时才产出 `Artifact::FirewallPorts`**（与 apply 第 10 步的两支判据同一个字段，避免「装了 ufw 但没启用」落进同一个坑）；提示改由 **Task 15 的 `reconcile_once` 从 `facts` 生成**（与 ssh 公钥提示同一套模式，`notes` 不影响 `/api/health` 的 `degraded` 判定，见 Task 13）；Task 6 的那条测试改名 `no_firewall_means_no_artifact_only_a_reconcile_note`；Task 5 第 10 步的「两者都没有」分支保留为**守卫**（P2/P3 若自行产出 `FirewallPorts` 仍有兜底）并写明正常路径不会命中；Task 15 的 `ready_host()` 补 `which.insert("ufw")` + 脚本化 `ufw status` → `Status: active`，让这两条测试真正走 ufw 路径；Task 18 清单 §0 要求先记录 `ufw status` / `firewalld` 的实测结果，并在「已知降级」里写明无防火墙机器的判定 |
| **C2**（阻塞）`V3_FILES` / `V3_STATE_FILES` 漏掉真实的 v3 残留 → `bui install --import-v3` 之后 `<base>` 顶层永久 `stray_file`、`/api/health` 恒 `degraded`（spec §2.3 覆盖缺口）。已从仓库里的 v3 脚本逐条核实：`server/residential-helper.sh:35` 的 relay 二进制在 `${BASE_DIR}/sing-box`（顶层，不在 `bin/`）；`server/core.sh:773` 写 `masquerade.json`、`:1170-1217` 写 `cert-check.sh`、`:1422` 写 `port-hopping.json`；`web/server.js:73` 读 `server_ip.txt`（P0 fixture 就带一个）；`server/update.sh:682/703/734/774/858/1660/1674/1709` 与 `server/b-ui-cli.sh:939/963` 留下 `*.bak.v357.<ts>` / `*.bak.v359.<ts>` / `*.bak.v360.<ts>` / `*.bak.obfs.<ts>` / `*.bak.broken-<ts>` / `*.tmp`。原来的测试用同一份（不全的）常量给假机器播种，所以是套套逻辑抓不出来 | Task 16：`V3_FILES` 11 → 13（加 `sing-box`、`cert-check.sh`）、`V3_STATE_FILES` 6 → 9（加 `port-hopping.json`、`masquerade.json`、`server_ip.txt`，都进 `v3-backup/`）、新增 `V3_LEFTOVER_PREFIXES` 与 `sweep_v3_leftovers`（只扫 `<base>` 顶层、只认这六个基名的 `*.bak.*`（归档，含秘密）与 `*.tmp`（删除）），排在归档状态文件之后；`uninstall_v3` 的顺序注释同步；`uninstall_stops_v3_units_archives_state_deletes_shell_and_keeps_certs` 增播 5 个真实残留名并逐条断言；`install_with_import_v3_keeps_the_v4_relay_and_leaves_no_drift` 改为**从 P0 fixture 的真实目录清单播种**（`std::fs::read_dir(src)`）再叠加这些残留名，测试不再自我印证 |
| **C3** Task 16 的 `run_with` 与 Task 15 的 `reconcile_cli` 硬编码 `paths::SOCKET_PATH` → 三个 install 单元测试会真的 `connect("/run/b-ui.sock")`；在跑着 v4 守护进程的机器上（M1 之后的 bwg-rick）`cargo test` 会真发一次 `POST /api/reconcile`，违反「单元测试不碰真实系统」 | socket 路径一律由调用方传入：`InstallOpts` 加 `pub socket: PathBuf`、`serve::reconcile_cli(paths, host, socket, force, dry_run)`；`main.rs` 的两个 arm 与 Task 17 的 `upgrade::run_with` 传 `PathBuf::from(crate::paths::SOCKET_PATH)`，Task 16 的测试 `opts(&d)` 传 `d.path().join("absent.sock")` |
| **C4** Task 2 的 `backup()` 按路径字典序裁剪 → 同一秒内多次写盘产生的 `state-<stamp>-1.json`…`-13.json` 排在 `state-<stamp>.json` **之前**（`-` < `.`）、`-10` 又排在 `-2` 之前，裁掉的是**新**备份 | 改成按 mtime 升序裁剪（取不到 mtime 的当最旧），新增断言说明 |
| **C5** Task 2 的 `Runtime::update` 在 async fn 里直接 `std::fs::write` + 事后 chmod（阻塞 runtime 线程；chmod 之前那一瞬是 0644，`runtime.json` 要求 0600） | 复用 `store::write_atomic`（tmp + 0600 + rename）并放进 `spawn_blocking`，与 `Store` 同一条路径 |
| **C6** Task 3 的 `RealHost::symlink` 用 `Path::exists()` 判「已存在」→ 悬空链接（`uninstall_v3` 删掉 `b-ui-cli.sh` 之后的 `/usr/local/bin/b-ui` 正是这种）返回 false，`std::os::unix::fs::symlink` 随后 EEXIST | 改用 `symlink_metadata(link).is_ok()`（不跟随链接），注释写明就是这个场景 |
| **C7** Global Constraints 要求 `tracing` 字段一律经 `redact::`，但 `url_credentials` / `secret` 全篇没有调用点 → Task 17 收口删 `#![allow(dead_code)]` 时会被当死代码删掉 | 点名三处调用点：Task 9 的 `HttpFetcher::get_bytes`（`send()` 失败的 `warn!` 与 HTTP 非 2xx 的 `bail!` 都把 url 过 `redact::url_credentials`——`BUI_MANIFEST_URL` 可以带 basic auth）、Task 16 的 `run`（随机管理员密码只 `println!` 一次明文，`tracing` 那行走 `redact::secret` 只记长度） |
| **C8** Task 16 的 `uninstall_stops_v3_units…` 用 `ops().any(\|o\| o.starts_with("run:crontab"))` 证明 cron 被重写，但 `crontab -l` 自己就产生 `run:crontab -l`，断言恒真 | 改断言 `run:crontab <base>/.crontab.new` 这一条实际写入命令 |
| **C9** Task 9 第 4 步 Expected 写 10，实际列出 9 条测试 | 改成 9 passed（S12 的计数表同步） |
| **C10** Task 8 没写每个 `Artifact::Unit` 带哪个 `UnitAction`；`Unit::reload("caddy")` 用在单元文件上会变成改完单元文件只 `systemctl reload caddy` | Task 8 的实现段写死：六个 `Artifact::Unit` 一律 `Unit::restart(...)`；`Unit::reload` 只出现在两个 `Artifact::File` 上（Task 10 的 Caddyfile、Task 7 的 sshd 配置） |
| **C11** spec §3.4「sysctl 与 v3 同值」只有自证测试，没有与 `server/core.sh` 的对照 | Task 6 新增 golden 测试 `v4_sysctl_values_match_the_v3_block_verbatim`：只读 `server/core.sh`（P5 删掉它之后自动 skip），提取 24 条 `net.*=` 行逐键比对 v4 的两个 conf |
| **C12** `/tmp/hy2-watchdog-*` 只在 `import-v3` 路径删，漂移扫描也不看它 | Task 5 第 5 步写明「`/tmp` 不在漂移扫描范围内」及理由（tmpfs 重启即清），Task 18 的「已知降级」记一行 |
| **C13** spec §2.4 的守护进程未运行白名单没列 `import-v3` / `serve` | Task 7 的放行理由段给出**回写 spec 的完整集合**与每一项的理由 |
| **C14** Task 18 清单的期望输出写「step3b … 可解析」，脚本实际打印「存在、0600、形状合 C5（schema=1 + users）」 | 清单的期望输出块逐字改成脚本的文案 |

**对总纲 C2/C3 的字段细化**（在「批准（细化不改名）」范围内，回写总纲时一并覆盖）：

| 契约 | 总纲原文 | P1 实际 | 理由 |
|---|---|---|---|
| C2 `Artifact` | 四个变体 `File/Unit/Sysctl/Binary` | 裁决点名的 `UnitState`/`FirewallPorts`/`Absent`，另加 `Modprobe`、`Symlink` | spec §2.2 的表把「`nf_conntrack` 模块」也算对账项（只写 `modules-load.d` 首装当轮 `sysctl -w` 会 ENOENT）；spec §1/§2.4 要求 `/usr/local/bin/{bui,b-ui}` 两个入口 |
| C2 `Artifact::File` | `{ path, content, mode, restart }` | 增 `restart_key`、`verify`、`immutable`（裁决点名） | spec §2.2「结构哈希变才重启 xray」「验证再重启」；spec §3.4 静态 DNS 的 `chattr +i` |
| C2 `Artifact::Unit` | `{ name: String, content }` | `{ name: Unit, dropin: Option<String>, content }`（裁决点名的 `Unit{name,action}`） | spec §2.2 的重启映射里 Caddy 是 `reload`、其余是 `restart`；drop-in 用于漂移比对 |
| C2 `Artifact::Binary` | `{ name, version, sha256 }` | 增 `url` | 下载地址只在 manifest 里，diff 产出的 `InstallBinary` 要把它带到 `BinaryInstaller::install` |
| C2 `AppState` | `{ store, bus, runtime }` | 增 `host: Arc<dyn Host>`、`started_at`、`version`、`login: LoginLimiter` | 端点要报单元状态与 uptime；登录限速必须随 router 实例隔离（见 Task 13） |
| C3 路径 | 未列 | 新增 `<base>/{manifest.json, relay-cache.db, .verify/, caddy/, v3-backup/}`、`<base>/bin/bui.prev`、`/usr/local/bin/{bui,b-ui}` | 分别是 manifest 缓存、sing-box relay 的 `cache_file`、渲染结果的校验落地目录、Caddy 的 XDG 目录、v3 状态文件归档、回滚用的上一版二进制、CLI 入口 |
| 发布 | 未约定 asset 形态 | C4 `artifacts` 里每个 `url` 必须指向**裸二进制**（hysteria 官方本来就是裸二进制；xray / sing-box / caddy 官方发压缩包，由 P5 的 Actions 解包重发） | P1 的 `KernelInstaller` 只做「下载 → sha256 → 写 0755」，不实现 tar/zip 解包；这是对 P5 的硬性要求 |

**消费 P0 的签名**（总纲 C1 已于 2026-09-12 按 `crates/bui-schema/src` 的真实代码同步，本计划逐字照用，不得改写）：

```rust
bui_schema::paths::Paths { base_dir, certs_dir, bin_dir }             // Paths::default_server() = /opt/b-ui{,/certs,/bin}
bui_schema::render::hysteria::{direct_yaml(&NodeParams, &Paths) -> String, residential_yaml(&NodeParams, &Paths) -> String}
bui_schema::render::xray::{config(&NodeParams, &[User], &Paths) -> serde_json::Value, structural_hash(&Value) -> String}
bui_schema::render::relay::{config(&ResidentialGroup, &RelayOpts) -> serde_json::Value,
                            RelayOpts { listen_port: u16, api: String, cache_path: String, server_ip: Option<String> }}
bui_schema::v3::import(dir: &Path) -> Result<ImportReport, ImportError>   // ImportReport { state: State, warnings: Vec<String> }
// P1 不消费、但同属 C1（列出来是为了不重复实现）：
bui_schema::render::SplitRules { enabled, global, keywords }                                   // P3
bui_schema::render::subscription::singbox(&[Node], &SplitRules, dial_ip: &str) -> Value        // P2（dial_ip = node.public_ip）
bui_schema::render::client::{ClientOpts { mode, socks_port, http_port, host_has_ipv6, split }, ClientMode::{Tun, Mixed}}  // P4
```

**C4 `manifest.json`（三方唯一契约；P5 产出，P1 只反序列化）**：

```jsonc
{ "version": "4.0.0",
  "kernels": { "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.14.5", "caddy": "2.10.2", "client_sing_box": "1.14.5" },
  "artifacts": { "bui-linux-amd64": { "url": "…", "sha256": "<64 hex>" },
                 "hysteria-linux-arm64": { "url": "…", "sha256": "…" } /* … */ } }
```

`version` 是 `bui` 自己的版本；`kernels` 只记版本号，键是 state `versions` 的字段名（下划线，`sing_box`；`client_sing_box` 是 P4 用的，P1 读进来不使用）；下载地址与校验和一律取 `artifacts`，键固定 `<name>-linux-<amd64|arm64>`（name = 二进制名，连字符，`sing-box`）。这一层换名由 Task 9 的 `kernels_key` / `artifact_key` 负责；未知字段一律忽略（serde 默认），P5 往 manifest 里加字段不会打死 P1。

**manifest 地址的解析顺序（总纲 C4「manifest 来源与覆盖」明写「P1 实现」）**：`--manifest-url <url|file>` > `--version <x.y.z>`（套 `MANIFEST_URL_TEMPLATE`；GitHub 的形状是 `releases/download/v<x.y.z>/manifest.json`，**不是** `releases/<tag>/download/`）> 环境变量 `BUI_MANIFEST_URL` > 内置 `MANIFEST_URL`（`releases/latest/download/manifest.json`）。这条顺序只在 **Task 9 的 `resolve_manifest_url` / `manifest_url`** 实现一次，Task 15（每日自检）、16（装机）、17（升级）都调它，没有第二处拼 URL 的代码；被废弃的 `--channel` 不再存在（它把 `/latest/` 换成 `/<c>/` 会得到 `releases/<c>/download/manifest.json`，不是 GitHub 的形状）。`HttpFetcher::get_bytes` 对 `file://` 前缀与不含 `://` 的入参按**本地文件**读取，所以 M5 的升级/回滚演练既可用本机 `python3 -m http.server` 托管 `dist/`，也可直接给一个文件路径，**不依赖任何公开 Release**。

**这条为什么是 M1 的阻塞项**：裁决记录写明「M5 前不创建任何 Release/tag」，所以内置 `MANIFEST_URL` 在 M1 必然 404 → `fetch_and_install_kernels` 返回 `None` → `bin/` 里没有内核。因此 P1 必须同时满足三件事，缺一件 bwg-rick 就装不上：① manifest 地址可被 `BUI_MANIFEST_URL` 覆盖到本机托管的 CI 产物（本节）；② `generate_reality` 在 `<bin>/xray` 缺失时回退到 PATH 上的 `xray`（v3 机器上有 `/usr/local/bin/xray`），两处都没有时报出**点名 `BUI_MANIFEST_URL`** 的可操作错误（Task 16）；③ 装完内核后若 `installed_versions` 不足四项，install 打印缺哪几个并提示怎么补（Task 16 第 4 步）。M1 的操作口径写在 Task 18 的清单里。

可选字段 `min_upgrade_from`（低于此版本的 `bui` 必须先升到该版本）由 **Task 17 的 `plan_upgrade`** 消费：`version_lt(current_bui, min_upgrade_from)` 为真时直接报错并提示先升到该中间版本；其余可选字段（`released` / `changelog_url`）P1 反序列化后不使用。

**`auth-snapshot.json` 与 P2 的交接**：spec §3.2 的钩子逻辑（`bui auth-hook`）归 P2；P1 只在 `bui install` 里写一次初版快照（0600，内容不变则不写，保证第二次 install 零写入）。形状**逐字照总纲 C5**——顶层两个键 `schema` 与 `users`，`users` 的键是用户名：

```jsonc
{ "schema": 1, "users": { "alice": { "user_id": "<uuid>", "hy2_password": "…", "expires_at": null, "blocked": false } } }
```

没有用户时是 `{"schema":1,"users":{}}`（不是 `{}`、也不是缺文件）。这一点是硬要求：P2 的 `auth-hook` 与快照重写按 C5 实现，P1 若写成「顶层直接以用户名为键」的扁平对象，P2 合并前后钩子都读不到用户 → `bui install --import-v3` 之后所有导入用户建连一律 fail-closed，与这一步的目的正好相反。`expires_at` 取 `entitlements.expires_at`（`null` = 不过期）；`blocked` 在 install 当轮等于 `user.disabled`（到期与超限的判定、快照的后续原子重写、`/kick` 都归 P2 的用户与采样模块）。这样 `bui install --import-v3` 装完立刻就能用导入的用户建连，不必等 P2 合并。它**不是** `Artifact`（P1 不对账它的内容），所以必须在 Task 5 的 `BASE_WHITELIST` 里，否则每轮对账都报一条陌生文件。

**与 spec 最新版的核对（只列 P1 范围内的结论）**：

- §3.2 `auth: { type: command, command: /opt/b-ui/bin/bui auth-hook }`：由 `bui_schema::render::hysteria` 渲染（`paths.bin_dir` 代入，P0 已落地），P1 只负责写盘与重启映射；钩子的 ≤2 秒硬超时、fail-closed、`auth-hook.log` 都在钩子实现里（P2）。Task 1 的 `main` 在建 tokio runtime、初始化 tracing **之前**就以「auth-hook 由 P2 实现」报错——绝不放行（spec §3.2 要求钩子不初始化 tokio/tracing/不加载 state，P1 的入口就按这个形状写好，P2 只替换那一支）。
- §3.3 `clients[].email = user_id`：P1 不拼 xray JSON；Task 10 的 `restart_key` 用 `structural_hash`（排除 `inbounds[].settings.clients`），因此 P2 走 gRPC 增删用户时 xray 不重启，与 spec 一致。
- §4.2 kick 语义（`POST /kick` 体为 `["<user_id>"]`、只标记不立即断连、被踢后客户端重连再过钩子）：整段归 P2 的采样/限额模块。P1 的 watchdog 只做进程存活与监听端口探测，不碰 `/traffic`、`/online`、`/kick`；两个 hysteria 的 `trafficStats` 监听端口（9999 直连 / 9998 住宅）由 `bui-schema` 渲染，P2 按此连。

---

### Task 1: `bui` crate 骨架（全部桩文件）、CLI 定义与日志脱敏

**Files:**
- Create（本任务填实）: `crates/bui/src/cli.rs`, `crates/bui/src/logging.rs`, `crates/bui/src/redact.rs`, `crates/bui/src/paths.rs`, `crates/bui/src/util.rs`, `crates/bui/src/testutil.rs`
- Create（本任务只建桩 `//! placeholder filled by Task N`，并把 `mod` / `pub mod` 行一次写全）: `crates/bui/src/state/{mod,store,runtime}.rs`、`crates/bui/src/sys/{mod,real,fake}.rs`、`crates/bui/src/reconcile/{mod,diff,apply,drift}.rs`、`crates/bui/src/modules/{mod,core_files,units,system,ssh,certs,watchdog}.rs`、`crates/bui/src/kernels/mod.rs`、`crates/bui/src/api/{mod,state,auth,health,system}.rs`、`crates/bui/src/ipc.rs`、`crates/bui/src/serve.rs`、`crates/bui/src/commands/{mod,install,import_v3,upgrade,status,menu,harden_ssh}.rs`
- Modify: `crates/bui/Cargo.toml`（替换 P0 的占位依赖）, `crates/bui/src/main.rs`（替换 P0 的 `println!` 占位）

**为什么一次建齐**：`bui` 是 bin crate，`mod` 行必须有对应文件才能编译。若让每个任务自己追加 `mod` 行，Task 4 的 `reconcile/mod.rs` 就无法声明 Task 5 才建的 `apply`/`drift`，Task 13 也无法被 Task 4 引用。照 P0 Task 1 的做法一次建齐桩文件后，任何任务单独合并到 `v4` 都能编译，并行任务之间零文件冲突。

**Interfaces:**
- Produces:
```rust
// crate::cli（子命令与选项名**逐字照总纲 C5**，P5 的脚本按 C5 调用，不得自造参数名）
pub struct Cli { pub log: Option<String>, pub version: bool, pub command: Option<Command> }   // 无子命令时看 argv[0]
pub enum Command {
    /// C5：`bui install [--import-v3] [--non-interactive --answers <file>]`（另有 P1 自己的 --domain/--port/--admin-password-stdin/-y）
    Install { domain: Option<String>, port: Option<u16>, admin_password_stdin: bool, import_v3: Option<PathBuf>,
              non_interactive: bool, answers: Option<PathBuf>, yes: bool },
    /// C5：`bui upgrade [--version <x.y.z>] [--manifest-url <url|file>] [--rollback]`（原草稿的 --channel 已废弃）
    Upgrade { rollback: bool, version: Option<String>, manifest_url: Option<String> },
    Serve,
    Reconcile { force: bool, dry_run: bool },
    Status { json: bool },
    ImportV3 { dir: PathBuf, out: Option<PathBuf> },
    AuthHook { args: Vec<String> },
    Menu,
    HardenSsh,
}
/// argv[0] 的文件名是 `b-ui`（`/usr/local/bin/b-ui` 符号链接）且没给子命令时进菜单；
/// 以 `bui` 名字裸跑则返回 None，由 main 打印 help。
pub fn default_command(argv0: &str) -> Option<Command>
// crate::logging
pub fn init(level: Option<&str>)
// crate::redact
pub fn url_credentials(s: &str) -> String
pub fn secret(s: &str) -> String
// crate::paths（都以 bui_schema::paths::Paths 为输入）
pub fn state_file(p: &Paths) -> PathBuf        // <base>/state.json
pub fn runtime_file(p: &Paths) -> PathBuf      // <base>/runtime.json
pub fn manifest_file(p: &Paths) -> PathBuf     // <base>/manifest.json
pub fn backups_dir(p: &Paths) -> PathBuf       // <base>/state.backups
pub fn verify_dir(p: &Paths) -> PathBuf        // <base>/.verify
pub fn v3_backup_dir(p: &Paths) -> PathBuf     // <base>/v3-backup（0700，归档 v3 的秘密文件）
pub fn caddy_xdg(p: &Paths) -> PathBuf         // <base>/caddy（XDG_DATA_HOME 与 XDG_CONFIG_HOME）
pub fn caddy_data(p: &Paths) -> PathBuf        // <base>/caddy/caddy（Caddy 真正的数据目录）
pub fn caddyfile(p: &Paths) -> PathBuf         // <base>/Caddyfile（C3；caddy.service 的 --config）
pub fn auth_snapshot_file(p: &Paths) -> PathBuf // <base>/auth-snapshot.json（install 写初版，P2 接手重写）
pub const SOCKET_PATH: &str = "/run/b-ui.sock";
/// CLI 入口符号链接（都指向 <base>/bin/bui）：spec §2.4「`sudo b-ui` 是 `bui menu` 的符号链接」
pub const CLI_LINKS: [&str; 2] = ["/usr/local/bin/bui", "/usr/local/bin/b-ui"];
// crate::util（纯函数，不依赖 Host，避免 Task 1 反向依赖 Task 3）
pub fn fmt_rfc3339(t: time::OffsetDateTime) -> String            // 格式化失败返回空串，不 panic
pub fn parse_rfc3339(s: &str) -> Option<time::OffsetDateTime>
pub fn human_duration(secs: u64) -> String                       // 3725 → "1h 2m"；59 → "59s"
// crate::testutil（#[cfg(test)]）
pub fn sample_state() -> bui_schema::model::State
```

- [ ] **Step 1: 写失败测试**

`crates/bui/src/redact.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_socks_url() {
        assert_eq!(
            url_credentials("socks5://user:pa:ss@isp.example.net:10007"),
            "socks5://***:***@isp.example.net:10007"
        );
    }

    #[test]
    fn redacts_http_url_with_path() {
        assert_eq!(
            url_credentials("http://u-x-ip-1:bz@isp.example.net:44445/x?y=1"),
            "http://***:***@isp.example.net:44445/x?y=1"
        );
    }

    #[test]
    fn redacts_at_form_without_scheme() {
        assert_eq!(url_credentials("u:p@h.example:1080"), "***:***@h.example:1080");
    }

    #[test]
    fn leaves_credential_free_url_untouched() {
        assert_eq!(url_credentials("https://example.com/a?b=1"), "https://example.com/a?b=1");
        assert_eq!(url_credentials("127.0.0.1:2080"), "127.0.0.1:2080");
    }

    #[test]
    fn secret_never_leaks_the_value() {
        assert_eq!(secret("hunter2hunter2"), "***(14 字符)");
        assert_eq!(secret(""), "<empty>");
        assert!(!secret("pw-alice-01").contains("alice"));
    }
}
```

`crates/bui/src/cli.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_install_with_bare_import_flag() {
        let cli = Cli::try_parse_from(["bui", "install", "--domain", "example.com", "--import-v3"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: Some("example.com".into()),
                port: None,
                admin_password_stdin: false,
                import_v3: Some(PathBuf::from("/opt/b-ui")),
                non_interactive: false,
                answers: None,
                yes: false,
            })
        );
    }

    #[test]
    fn parses_non_interactive_install_with_an_answers_file() {
        // 总纲 C5 的 `bui install [--non-interactive --answers <file>]`：P5 的 v3-cutover.sh 只准用这一条
        let cli = Cli::try_parse_from([
            "bui", "install", "--non-interactive", "--answers", "/root/answers.json", "--admin-password-stdin",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: None,
                port: None,
                admin_password_stdin: true,
                import_v3: None,
                non_interactive: true,
                answers: Some(PathBuf::from("/root/answers.json")),
                yes: false,
            })
        );
    }

    #[test]
    fn an_answers_file_without_non_interactive_is_rejected() {
        assert!(Cli::try_parse_from(["bui", "install", "--answers", "/root/answers.json"]).is_err());
    }

    #[test]
    fn parses_install_with_import_dir_and_password_stdin() {
        let cli = Cli::try_parse_from([
            "bui", "install", "--domain", "example.com", "--port", "10000",
            "--admin-password-stdin", "--import-v3", "/tmp/old", "-y",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: Some("example.com".into()),
                port: Some(10000),
                admin_password_stdin: true,
                import_v3: Some(PathBuf::from("/tmp/old")),
                non_interactive: false,
                answers: None,
                yes: true,
            })
        );
    }

    #[test]
    fn rejects_password_on_argv() {
        // 凭据不进 argv：没有 --admin-password 这个选项
        assert!(Cli::try_parse_from(["bui", "install", "--admin-password", "x"]).is_err());
    }

    #[test]
    fn parses_upgrade_flags_per_c5_and_reconcile_flags() {
        assert_eq!(
            Cli::try_parse_from(["bui", "upgrade", "--rollback"]).unwrap().command,
            Some(Command::Upgrade { rollback: true, version: None, manifest_url: None })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "upgrade", "--version", "4.0.1"]).unwrap().command,
            Some(Command::Upgrade { rollback: false, version: Some("4.0.1".into()), manifest_url: None })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "upgrade", "--manifest-url", "http://127.0.0.1:8000/manifest.json"])
                .unwrap()
                .command,
            Some(Command::Upgrade {
                rollback: false,
                version: None,
                manifest_url: Some("http://127.0.0.1:8000/manifest.json".into()),
            })
        );
        assert!(Cli::try_parse_from(["bui", "upgrade", "--channel", "beta"]).is_err(), "--channel 已废弃");
        assert_eq!(
            Cli::try_parse_from(["bui", "reconcile", "--force", "--dry-run"]).unwrap().command,
            Some(Command::Reconcile { force: true, dry_run: true })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "import-v3"]).unwrap().command,
            Some(Command::ImportV3 { dir: PathBuf::from("/opt/b-ui"), out: None })
        );
    }

    #[test]
    fn auth_hook_collects_trailing_args() {
        let cli = Cli::try_parse_from(["bui", "auth-hook", "alice", "pw"]).unwrap();
        assert_eq!(cli.command, Some(Command::AuthHook { args: vec!["alice".into(), "pw".into()] }));
    }

    #[test]
    fn bare_invocation_parses_and_leaves_the_command_empty() {
        // spec §2.4：`sudo b-ui` 无参也要能跑（进菜单），所以子命令不是必填
        assert_eq!(Cli::try_parse_from(["b-ui"]).unwrap().command, None);
        assert_eq!(Cli::try_parse_from(["bui"]).unwrap().command, None);
    }

    #[test]
    fn version_is_our_own_flag_so_it_can_print_just_the_number() {
        // 总纲 C5：`bui --version` 只打印 `4.0.0`。clap 自带的 version 会打印 `bui 4.0.0`，
        // 所以 `#[command(...)]` 里 `disable_version_flag = true`，改由 main 自己处理这个 bool。
        let cli = Cli::try_parse_from(["bui", "--version"]).unwrap();
        assert!(cli.version);
        assert_eq!(cli.command, None);
        assert!(!Cli::try_parse_from(["bui", "status"]).unwrap().version);
    }

    #[test]
    fn the_b_ui_alias_defaults_to_the_menu() {
        assert_eq!(default_command("/usr/local/bin/b-ui"), Some(Command::Menu));
        assert_eq!(default_command("b-ui"), Some(Command::Menu));
        assert_eq!(default_command("/opt/b-ui/bin/bui"), None, "以 bui 名字裸跑打印 help");
        assert_eq!(default_command(""), None);
    }
}
```

`crates/bui/src/util.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    #[test]
    fn rfc3339_round_trips() {
        let t = datetime!(2026-09-11 00:01:02 UTC);
        assert_eq!(fmt_rfc3339(t), "2026-09-11T00:01:02Z");
        assert_eq!(parse_rfc3339("2026-09-11T00:01:02Z"), Some(t));
        assert_eq!(parse_rfc3339("not a time"), None);
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(3725), "1h 2m");
        assert_eq!(human_duration(90), "1m 30s");
        assert_eq!(human_duration(90061), "1d 1h");
        assert_eq!(human_duration(0), "0s");
    }
}
```

`crates/bui/src/paths.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn derives_every_bui_path_from_base_dir() {
        let p = Paths::default_server();
        assert_eq!(state_file(&p), PathBuf::from("/opt/b-ui/state.json"));
        assert_eq!(runtime_file(&p), PathBuf::from("/opt/b-ui/runtime.json"));
        assert_eq!(manifest_file(&p), PathBuf::from("/opt/b-ui/manifest.json"));
        assert_eq!(backups_dir(&p), PathBuf::from("/opt/b-ui/state.backups"));
        assert_eq!(verify_dir(&p), PathBuf::from("/opt/b-ui/.verify"));
        assert_eq!(v3_backup_dir(&p), PathBuf::from("/opt/b-ui/v3-backup"));
        assert_eq!(caddy_xdg(&p), PathBuf::from("/opt/b-ui/caddy"));
        assert_eq!(caddy_data(&p), PathBuf::from("/opt/b-ui/caddy/caddy"));
        assert_eq!(caddyfile(&p), PathBuf::from("/opt/b-ui/Caddyfile"));
        assert_eq!(auth_snapshot_file(&p), PathBuf::from("/opt/b-ui/auth-snapshot.json"));
        assert_eq!(SOCKET_PATH, "/run/b-ui.sock");
        assert_eq!(CLI_LINKS, ["/usr/local/bin/bui", "/usr/local/bin/b-ui"]);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui`
Expected: 编译失败，`error[E0433]: failed to resolve: use of undeclared crate or module 'cli'` 一类（模块尚未建立）。

- [ ] **Step 3: 写 Cargo.toml**

`crates/bui/Cargo.toml`（整体替换 P0 的占位内容）：
```toml
[package]
name = "bui"
version = "4.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
bui-schema = { path = "../bui-schema" }
serde.workspace = true
serde_json.workspace = true
serde_yaml.workspace = true
uuid.workspace = true
sha2.workspace = true
hex.workspace = true
thiserror.workspace = true
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "net", "signal", "io-util", "process"] }
axum = "0.8"
tower = { version = "0.5", features = ["util"] }
tower-http = { version = "0.6", features = ["trace"] }
hyper = { version = "1", features = ["client", "server", "http1"] }
hyper-util = { version = "0.1", features = ["tokio", "service"] }
http-body-util = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-journald = "0.3"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "blocking"] }
inotify = { version = "0.11", features = ["stream"] }
nix = { version = "0.29", features = ["socket", "user", "hostname", "fs"] }
argon2.workspace = true
jsonwebtoken = "9"
rand.workspace = true
time = { version = "0.3", features = ["formatting", "parsing", "macros"] }
futures-util = "0.3"

[dev-dependencies]
pretty_assertions.workspace = true
tempfile.workspace = true
# Task 11/15 用 `#[tokio::test(start_paused = true)]` 推进时钟，它需要 tokio 的 `test-util` feature。
# cargo 在 test 构建时把 dev-dependencies 的 feature 与 [dependencies] 的并集生效，生产构建不带这一项；
# 少了这一行，`start_paused = true` 直接报 `no method named start_paused`，Task 11/15 的测试编译不过。
tokio = { version = "1", features = ["test-util"] }
```
`argon2`（0.5）与 `rand`（0.8）已在 P0 建好的根 `Cargo.toml` `[workspace.dependencies]` 里，用 `.workspace = true` 引用，不重复写版本；其余 P1 独有的依赖直接在本 crate 里写版本，不改根 `Cargo.toml`（那是 P0 的文件）。

- [ ] **Step 4: 写 CLI 定义**

`crates/bui/src/cli.rs`：
```rust
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// b-ui v4 单二进制控制器（守护进程 + CLI）。
#[derive(Debug, Parser)]
#[command(
    name = "bui",
    about = "b-ui v4 期望态控制器",
    subcommand_required = false,
    arg_required_else_help = false,
    // 总纲 C5：`bui --version` 只打印版本号，所以关掉 clap 自带的 --version（它会打印 "bui 4.0.0"）
    disable_version_flag = true
)]
pub struct Cli {
    /// 日志级别（覆盖 RUST_LOG），例如 info / debug
    #[arg(long, global = true)]
    pub log: Option<String>,
    /// 打印版本号后退出（只打印 `4.0.0`，不带程序名）
    #[arg(long, short = 'V')]
    pub version: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// 首次安装：交互收集参数 → 写 state.json → 对账
    Install {
        /// 面板域名
        #[arg(long)]
        domain: Option<String>,
        /// Hysteria2 直连端口（默认 10000）
        #[arg(long)]
        port: Option<u16>,
        /// 从 stdin 读管理员密码（凭据不进 argv）
        #[arg(long)]
        admin_password_stdin: bool,
        /// 从 v3 安装目录导入（不带值时用 /opt/b-ui）
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "/opt/b-ui")]
        import_v3: Option<PathBuf>,
        /// 非交互安装：一个问题都不问（总纲 C5）
        #[arg(long)]
        non_interactive: bool,
        /// 非交互安装的答案文件（JSON；形状见 Task 16 的 `Answers` / `load_answers`）
        #[arg(long, value_name = "FILE", requires = "non_interactive")]
        answers: Option<PathBuf>,
        /// 非交互：缺失项用默认值（`--non-interactive` 的简写别名，两者都接受）
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// 升级 bui 与内核二进制
    Upgrade {
        /// 回滚到上一版二进制与最近一份 state 备份
        #[arg(long)]
        rollback: bool,
        /// 升级到指定版本（manifest 取 `releases/download/v<x.y.z>/manifest.json`）
        #[arg(long, value_name = "X.Y.Z")]
        version: Option<String>,
        /// 覆盖 manifest 地址：http(s) URL、`file://…` 或本地路径（总纲 C4，M5 演练用）
        #[arg(long, value_name = "URL|FILE")]
        manifest_url: Option<String>,
    },
    /// 运行守护进程（systemd 用）
    Serve,
    /// 手动对账一次
    Reconcile {
        /// 连非受管的漂移项一起清理
        #[arg(long)]
        force: bool,
        /// 只打印将要做的改动，不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 打印状态与体检
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 只从 v3 生成 state.json（不对账、不卸载 v3）
    ImportV3 {
        #[arg(long, default_value = "/opt/b-ui")]
        dir: PathBuf,
        /// 输出路径（默认写 <base>/state.json）
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Hysteria2 auth.command 钩子（逻辑由 P2 实现）
    AuthHook {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 数字菜单（sudo b-ui 的符号链接目标）
    Menu,
    /// 只做 SSH 硬化
    HardenSsh,
}

/// `/usr/local/bin/b-ui` 这个符号链接裸跑时进菜单（spec §1、§2.4）。
pub fn default_command(argv0: &str) -> Option<Command> {
    let name = std::path::Path::new(argv0).file_name().and_then(|s| s.to_str()).unwrap_or("");
    (name == "b-ui").then_some(Command::Menu)
}
```

- [ ] **Step 5: 写脱敏、日志、路径与 main 派发**

`crates/bui/src/redact.rs`：
```rust
//! 日志与错误信息脱敏。凭据、密钥、密码一律不进 journal。

/// 把 URL（或 `u:p@host:port` 形态）里的 userinfo 换成 `***:***`。
pub fn url_credentials(s: &str) -> String {
    let (scheme, rest) = match s.find("://") {
        Some(i) => (&s[..i + 3], &s[i + 3..]),
        None => ("", s),
    };
    // userinfo 只可能出现在第一个 '/' 之前
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];
    match authority.rfind('@') {
        Some(at) => {
            let userinfo = &authority[..at];
            let masked = if userinfo.contains(':') { "***:***" } else { "***" };
            format!("{scheme}{masked}@{}{tail}", &authority[at + 1..])
        }
        None => s.to_string(),
    }
}

/// 只暴露长度，不暴露内容。
pub fn secret(s: &str) -> String {
    if s.is_empty() {
        "<empty>".to_string()
    } else {
        format!("***({} 字符)", s.chars().count())
    }
}
```

`crates/bui/src/logging.rs`：
```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// 优先 journald（systemd 下），失败回落 stderr。默认 INFO。
pub fn init(level: Option<&str>) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level.unwrap_or("info")))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    match tracing_journald::layer() {
        Ok(journald) => registry.with(journald).init(),
        Err(_) => registry.with(tracing_subscriber::fmt::layer().with_target(false)).init(),
    }
}
```

`crates/bui/src/paths.rs`：
```rust
use bui_schema::paths::Paths;
use std::path::PathBuf;

/// CLI 与守护进程之间的 unix socket（0600，root）。
pub const SOCKET_PATH: &str = "/run/b-ui.sock";

/// CLI 入口符号链接（spec §2.4）：两个都指向 <base>/bin/bui。
pub const CLI_LINKS: [&str; 2] = ["/usr/local/bin/bui", "/usr/local/bin/b-ui"];

pub fn state_file(p: &Paths) -> PathBuf { p.base_dir.join("state.json") }
pub fn runtime_file(p: &Paths) -> PathBuf { p.base_dir.join("runtime.json") }
/// install / upgrade / 每日自检写下的 manifest 缓存，对账据它决定内核版本。
pub fn manifest_file(p: &Paths) -> PathBuf { p.base_dir.join("manifest.json") }
pub fn backups_dir(p: &Paths) -> PathBuf { p.base_dir.join("state.backups") }
/// 渲染结果过内核校验时用的落地目录（FakeHost 下可断言）。
pub fn verify_dir(p: &Paths) -> PathBuf { p.base_dir.join(".verify") }
/// v3 状态文件（含秘密）归档目录，0700；`uninstall_v3` 写，漂移扫描白名单里有它。
pub fn v3_backup_dir(p: &Paths) -> PathBuf { p.base_dir.join("v3-backup") }
/// 传给 caddy 的 XDG_DATA_HOME 与 XDG_CONFIG_HOME（同一个目录）。
pub fn caddy_xdg(p: &Paths) -> PathBuf { p.base_dir.join("caddy") }
/// Caddy 实际的数据目录（$XDG_DATA_HOME/caddy），ACME 账号与证书都在它下面。
pub fn caddy_data(p: &Paths) -> PathBuf { caddy_xdg(p).join("caddy") }
/// Caddyfile：按总纲 C3 与 2026-09-12 裁决放 `<base>`，`caddy.service` 以 `--config` 指它。
pub fn caddyfile(p: &Paths) -> PathBuf { p.base_dir.join("Caddyfile") }
/// hysteria 鉴权钩子读的快照（`bui install` 写初版，P2 的用户模块接手后原子重写）。
pub fn auth_snapshot_file(p: &Paths) -> PathBuf { p.base_dir.join("auth-snapshot.json") }
```

`crates/bui/src/util.rs`：
```rust
//! 纯时间与格式化工具（不依赖 Host，任何任务都能用）。
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// RFC3339（UTC，秒级）。格式化失败返回空串，绝不 panic。
pub fn fmt_rfc3339(t: OffsetDateTime) -> String {
    t.replace_nanosecond(0).unwrap_or(t).format(&Rfc3339).unwrap_or_default()
}

pub fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// 给人看的时长：`0s` / `59s` / `1m 30s` / `1h 2m` / `1d 1h`。
pub fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}
```

`crates/bui/src/main.rs`（`mod` 行一次写全，桩文件由下一步建）：
```rust
// 分任务落地期间，被调用方常常先于调用方合并（例如 Task 2 的 Store 在 Task 13 之前），
// 非 test 构建下它们还没人用，`-D warnings` 会因 dead_code 直接失败。
// 这一行在 Task 17 收口（最后一个子命令接上）时删除，并修掉真正的死代码。
#![allow(dead_code)]

mod api;
mod cli;
mod commands;
mod ipc;
mod kernels;
mod logging;
mod modules;
mod paths;
mod reconcile;
mod redact;
mod serve;
mod state;
mod sys;
#[cfg(test)]
mod testutil;
mod util;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::{Cli, Command};

// main 是**同步**的：spec §3.2 要求 `bui auth-hook` 不初始化 tokio、不初始化 tracing、不加载 state
// （M5 有 200 建连/秒、p99 < 20ms 的门槛，每次建连都要 fork 一个 bui），所以派发在建 runtime 之前
// 先把 AuthHook 摘出去。其余子命令再建 runtime、初始化日志。P2 落地钩子时只替换下面那一支，
// 不必回头重构入口。
fn main() -> Result<()> {
    let argv0 = std::env::args().next().unwrap_or_default();
    let args = Cli::parse();
    // 总纲 C5：只打印版本号
    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let Some(command) = args.command.or_else(|| cli::default_command(&argv0)) else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    if let Command::AuthHook { args } = &command {
        let _ = args;
        // P2 的交付物；P1 在这里就返回，连 runtime 与日志都不初始化
        anyhow::bail!("auth-hook 由 P2 实现");
    }
    logging::init(args.log.as_deref());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(dispatch(command))
}

async fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Install { .. } => not_yet("install"),
        Command::Upgrade { .. } => not_yet("upgrade"),
        Command::Serve => not_yet("serve"),
        Command::Reconcile { .. } => not_yet("reconcile"),
        Command::Status { .. } => not_yet("status"),
        Command::ImportV3 { .. } => not_yet("import-v3"),
        Command::AuthHook { .. } => unreachable!("auth-hook 已在 main 里提前返回"),
        Command::Menu => not_yet("menu"),
        Command::HardenSsh => not_yet("harden-ssh"),
    }
}

/// 脚手架：后续任务逐个替换对应 arm；最后一个 arm 被替换时连这个函数一起删（Task 17）。
fn not_yet(what: &str) -> Result<()> {
    anyhow::bail!("子命令 {what} 尚未在本分支实现")
}
```

- [ ] **Step 5b: 一次建齐全部桩文件**

按「Files」里第二条列出的路径逐个创建，内容只有一行注释（例：`//! placeholder filled by Task 5`），并把各 `mod.rs` 的声明一次写全（这些 `mod.rs` 之后**不再被任何任务修改**，除 `api/mod.rs` 由 Task 4 追加一行 `pub use`、由 Task 13 追加 `router()`）：

```rust
// crates/bui/src/state/mod.rs
pub mod runtime;
pub mod store;
// crates/bui/src/sys/mod.rs（Task 3 填内容时保留这两行）
pub mod real;
#[cfg(test)]
pub mod fake;
// crates/bui/src/reconcile/mod.rs（Task 4 填内容时保留这三行）
pub mod apply;
pub mod diff;
pub mod drift;
// crates/bui/src/modules/mod.rs
pub mod certs;
pub mod core_files;
pub mod ssh;
pub mod system;
pub mod units;
pub mod watchdog;
// crates/bui/src/api/mod.rs
pub mod auth;
pub mod health;
pub mod state;
pub mod system;
// crates/bui/src/commands/mod.rs
pub mod harden_ssh;
pub mod import_v3;
pub mod install;
pub mod menu;
pub mod status;
pub mod upgrade;
```
桩文件阶段 `cargo clippy -- -D warnings` 会因为「模块里什么都没有」而通过（空模块不是告警），但 `main.rs` 的 `mod x;` 若指向不存在的文件会直接编译失败——所以这一步和 Step 5 必须同一个 commit。

`crates/bui/src/testutil.rs`：
```rust
use bui_schema::model::State;

/// spec §2.1 的期望态样例（合成值，无生产秘密）。argon2 哈希是 `test123` 的固定串。
pub fn sample_state() -> State {
    serde_json::from_str(SAMPLE).expect("sample_state 必须能解析")
}

const SAMPLE: &str = r#"{
  "schema_version": 1,
  "node": { "id": "8d5a1a1e-3b2c-4d1e-9f00-000000000001", "name": "node-a", "domain": "example.com", "public_ip": "203.0.113.10",
            "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                       "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
            "reality": { "private_key": "CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4",
                         "public_key": "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c",
                         "short_ids": ["0123456789abcdef"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
            "obfs": { "enabled": false, "password": "" } },
  "admin": { "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$Q2g5cWFrZXN0aGFzaHZhbHVl", "jwt_secret": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff" },
  "users": [ { "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa", "username": "alice", "note": "", "created_at": "2026-09-11T00:00:00Z", "disabled": false,
               "credentials": { "hy2_password": "pw-alice-01", "vless_uuid": "11111111-1111-4111-8111-111111111111" },
               "entitlements": { "protocols": ["hysteria2", "reality"], "direct": true, "residential": { "group_id": "default" },
                                 "expires_at": null, "traffic_limit": { "total_bytes": null, "monthly_bytes": null } },
               "usage": { "total_bytes": 0, "monthly_bytes": 0, "month_key": "2026-09", "last_seen_at": null },
               "portal_auth": { "password_hash": null, "tokens": [] },
               "billing": { "currency": "CNY", "balance_minor": 0, "orders": [] } } ],
  "residential": { "groups": { "default": { "enabled": false, "mode": "split", "keywords": null, "upstreams": [],
                    "selected_upstream_id": null, "blacklist": { "pins": [], "auto": [] } } } },
  "system": { "ssh_hardening": true, "static_dns": true, "sysctl_profile": "auto", "firewall": "auto" },
  "versions": { "bui": "4.0.0", "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2", "client_sing_box": "1.13.19" },
  "catalog": []
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_state_parses() {
        let s = sample_state();
        assert_eq!(s.node.ports.hy2, 10000);
        assert_eq!(s.users.len(), 1);
    }
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui && cargo clippy -p bui --all-targets -- -D warnings && cargo fmt --check`
Expected: 19 passed（redact 5 + cli 10 + paths 1 + util 2 + testutil 1），clippy/fmt 无输出。再验一次入口行为：`cargo run -p bui -- --help` 打印帮助且退出码 0；`cargo run -p bui -- --version` 只打印 `4.0.0`（一行、不带程序名）；`cargo run -p bui -- auth-hook a b` 报「auth-hook 由 P2 实现」；`cargo run -p bui -- status --json` 报「子命令 status 尚未在本分支实现」。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/Cargo.toml crates/bui/src Cargo.lock
git commit -m "feat(bui): crate 骨架、CLI 子命令与日志脱敏"
```

---

### Task 2: `Store`（期望态）与 `Runtime`（运行时数据）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/state/store.rs`, `crates/bui/src/state/runtime.rs`（`state/mod.rs` 与 `main.rs` 的 `mod state;` 已由 Task 1 写好，本任务不动）

**Interfaces:**
- Consumes: `bui_schema::model::State`；`crate::paths::{state_file, runtime_file, backups_dir}`
- Produces（总纲 C2 的 `Store`，签名细化但不改名）：
```rust
// crate::state::store
pub const BACKUP_KEEP: usize = 10;
#[derive(Clone)]
pub struct Store { /* Arc<Inner> */ }
impl Store {
    pub async fn open(path: impl Into<PathBuf>) -> anyhow::Result<Self>;
    pub async fn create(path: impl Into<PathBuf>, state: State) -> anyhow::Result<Self>;
    pub async fn read(&self) -> Arc<State>;
    pub async fn update(&self, f: impl FnOnce(&mut State)) -> anyhow::Result<Arc<State>>;
    pub fn path(&self) -> &Path;
}
// crate::state::runtime
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeData {
    pub restart_keys: BTreeMap<String, String>,
    pub watchdog: BTreeMap<String, WatchdogRecord>,
    pub cert_sha256: Option<String>,
    pub drift: Vec<DriftItem>,
    pub last_reconcile: Option<ReconcileReport>,
    pub started_at: Option<String>,
    /// 每日自检发现的新版本号（spec §7），`None` = 已是最新
    pub upgrade_available: Option<String>,
    /// P2/P3 追加的运行时字段（健康 streak、黑名单候选计数、上次选中上游、采样游标，spec §2.1）
    /// 直接落在这里，P2/P3 不必回头改本任务的文件
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WatchdogRecord { pub fails: u32, pub restarts: u32, pub last_restart_at: Option<String>, pub backoff_until: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriftItem { pub kind: String, pub path: String, pub detail: String }
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReconcileReport {
    pub at: String, pub changed: Vec<String>, pub restarted: Vec<String>, pub notes: Vec<String>,
    pub verify_failures: Vec<String>, pub errors: Vec<String>, pub drift: Vec<DriftItem>, pub dry_run: bool,
    /// 本轮 `b-ui.service` 自身需要重启（apply 从不在同步重启循环里动它，见 Task 5 第 12 步）；
    /// 由调用方执行：CLI 路径在报告落盘后 `systemctl restart b-ui`，守护进程内用 `restart --no-block`
    #[serde(default)]
    pub self_restart_required: bool,
}
impl ReconcileReport { pub fn is_clean(&self) -> bool; }   // 只看 changed/restarted/errors/verify_failures
#[derive(Clone)]
pub struct Runtime { /* Arc<Inner> */ }
impl Runtime {
    pub fn load(path: impl Into<PathBuf>) -> Self;                       // 解析失败 → 默认值 + warn
    pub async fn read(&self) -> RuntimeData;
    pub async fn update(&self, f: impl FnOnce(&mut RuntimeData)) -> RuntimeData;   // best-effort 落盘
}
```
`WatchdogRecord` / `DriftItem` / `ReconcileReport` 定义在 `runtime.rs`（它们是持久化的运行时数据），Task 4/5/11/12 从这里 import，避免模块环。

- [ ] **Step 1: 写失败测试（store）**

`crates/bui/src/state/store.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;

    fn dir() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    #[tokio::test]
    async fn create_then_read_round_trips() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        assert_eq!(store.read().await.node.ports.hy2, 10000);
        let reopened = Store::open(&p).await.unwrap();
        assert_eq!(reopened.read().await.users[0].username, "alice");
    }

    #[tokio::test]
    async fn state_file_is_0600() {
        let d = dir();
        let p = d.path().join("state.json");
        Store::create(&p, sample_state()).await.unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn each_write_backs_up_the_previous_version() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        for i in 0..3u32 {
            store.update(|s| s.node.name = format!("node-{i}")).await.unwrap();
        }
        let backups = std::fs::read_dir(d.path().join("state.backups")).unwrap().count();
        assert_eq!(backups, 3, "每次写入前备份旧版");
        assert_eq!(store.read().await.node.name, "node-2");
    }

    #[tokio::test]
    async fn backups_are_capped_at_ten() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        for i in 0..14u32 {
            store.update(|s| s.node.name = format!("node-{i}")).await.unwrap();
        }
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(d.path().join("state.backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();   // 文件名是 `state-<stamp>-<nnn>.json`，零填充 ⇒ 字典序 == 时间序
        assert_eq!(files.len(), BACKUP_KEEP);
        // 内容断言，不依赖时间戳落在同一秒：`update` 备份的是**上一版**，所以 14 次写盘留下
        // [sample, node-0 … node-12] 共 15 份候选里最新的 10 份 = node-3 … node-12。
        // 裁剪按字典序而命名又不零填充时（`-1`…`-13` 排在 `.json` 之前、`-10` 排在 `-2` 之前），
        // 这里会看到最新的几份被删掉——正是 `bui upgrade --rollback` 要恢复的那一份。
        let name_in = |p: &std::path::PathBuf| {
            serde_json::from_slice::<bui_schema::model::State>(&std::fs::read(p).unwrap()).unwrap().node.name
        };
        assert_eq!(name_in(files.last().unwrap()), "node-12", "最新一份备份被裁掉了：{files:?}");
        assert_eq!(name_in(files.first().unwrap()), "node-3", "裁掉的不是最旧的四份：{files:?}");
    }

    #[tokio::test]
    async fn no_change_means_no_write_and_no_backup() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        store.update(|s| s.node.name = "changed".into()).await.unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        let count_before = std::fs::read_dir(d.path().join("state.backups")).unwrap().count();
        store.update(|_| {}).await.unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read_dir(d.path().join("state.backups")).unwrap().count(), count_before);
    }

    #[tokio::test]
    async fn concurrent_updates_are_serialized() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        let mut handles = Vec::new();
        for i in 0..20u32 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                s.update(move |st| {
                    st.catalog.push(bui_schema::model::CatalogItem {
                        sku: format!("sku-{i}"),
                        title: "t".into(),
                        kind: bui_schema::model::CatalogKind::Plan,
                        region: None,
                        price_minor: 1,
                        period_days: None,
                    })
                })
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(store.read().await.catalog.len(), 20, "20 次并发更新不能互相覆盖");
    }

    #[tokio::test]
    async fn open_missing_file_errors() {
        let d = dir();
        assert!(Store::open(d.path().join("nope.json")).await.is_err());
    }
}
```

`crates/bui/src/state/runtime.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn missing_file_loads_defaults() {
        let d = tempfile::tempdir().unwrap();
        let rt = Runtime::load(d.path().join("runtime.json"));
        assert_eq!(rt.read().await, RuntimeData::default());
    }

    #[tokio::test]
    async fn corrupt_file_loads_defaults_without_panicking() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        std::fs::write(&p, b"{ this is not json").unwrap();
        let rt = Runtime::load(&p);
        assert_eq!(rt.read().await.restart_keys.len(), 0);
    }

    #[tokio::test]
    async fn update_persists_and_reloads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        let rt = Runtime::load(&p);
        rt.update(|r| {
            r.restart_keys.insert("xray-config".into(), "abc123".into());
            r.watchdog.insert(
                "hysteria-server".into(),
                WatchdogRecord { fails: 1, restarts: 2, last_restart_at: Some("2026-09-11T00:00:00Z".into()), backoff_until: None },
            );
        })
        .await;
        let back = Runtime::load(&p).read().await;
        assert_eq!(back.restart_keys["xray-config"], "abc123");
        assert_eq!(back.watchdog["hysteria-server"].restarts, 2);
    }

    #[tokio::test]
    async fn unknown_fields_survive_a_round_trip() {
        // P2/P3 会往 runtime.json 里加自己的字段；P1 读写不能把它们吃掉
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        std::fs::write(&p, br#"{"cert_sha256":"abc","resi_streak":{"u1":3}}"#).unwrap();
        let rt = Runtime::load(&p);
        assert_eq!(rt.read().await.extra["resi_streak"], serde_json::json!({"u1": 3}));
        rt.update(|r| r.upgrade_available = Some("4.0.1".into())).await;
        let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(back["resi_streak"], serde_json::json!({"u1": 3}));
        assert_eq!(back["upgrade_available"], "4.0.1");
        assert_eq!(back["cert_sha256"], "abc");
    }

    #[test]
    fn report_is_clean_only_when_nothing_happened() {
        let mut r = ReconcileReport { at: "2026-09-11T00:00:00Z".into(), ..Default::default() };
        assert!(r.is_clean());
        r.changed.push("/opt/b-ui/config.yaml".into());
        assert!(!r.is_clean());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui state::`
Expected: 编译失败（`Store` / `Runtime` 未定义）。

- [ ] **Step 3: 实现 `store.rs`**

```rust
use anyhow::{Context, Result};
use bui_schema::model::State;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// 备份保留份数（spec §2.1）。
pub const BACKUP_KEEP: usize = 10;

/// 单进程唯一持有 `State`；写 = 临时文件 + rename + 备份 10 份。
#[derive(Clone)]
pub struct Store(Arc<Inner>);

struct Inner {
    path: PathBuf,
    backups: PathBuf,
    cache: RwLock<Arc<State>>,
    write: Mutex<()>,
}

impl Store {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let bytes = std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
        let state: State = serde_json::from_slice(&bytes).with_context(|| format!("解析 {} 失败", path.display()))?;
        Ok(Self::wrap(path, state))
    }

    pub async fn create(path: impl Into<PathBuf>, state: State) -> Result<Self> {
        let path = path.into();
        let store = Self::wrap(path, state);
        let bytes = serde_json::to_vec_pretty(&*store.read().await)?;
        let inner = store.0.clone();
        tokio::task::spawn_blocking(move || write_atomic(&inner.path, &bytes)).await??;
        Ok(store)
    }

    fn wrap(path: PathBuf, state: State) -> Self {
        let backups = path.parent().unwrap_or(Path::new(".")).join("state.backups");
        Self(Arc::new(Inner { path, backups, cache: RwLock::new(Arc::new(state)), write: Mutex::new(()) }))
    }

    pub async fn read(&self) -> Arc<State> {
        self.0.cache.read().await.clone()
    }

    pub async fn update(&self, f: impl FnOnce(&mut State)) -> Result<Arc<State>> {
        let _guard = self.0.write.lock().await;
        let current = self.read().await;
        let old_bytes = serde_json::to_vec_pretty(&*current)?;
        let mut next = (*current).clone();
        f(&mut next);
        let new_bytes = serde_json::to_vec_pretty(&next)?;
        if new_bytes == old_bytes {
            return Ok(current);
        }
        let inner = self.0.clone();
        let bytes = new_bytes.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if inner.path.exists() {
                backup(&inner.path, &inner.backups)?;
            }
            write_atomic(&inner.path, &bytes)
        })
        .await??;
        let next = Arc::new(next);
        *self.0.cache.write().await = next.clone();
        Ok(next)
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }
}

/// `pub(crate)`：`state::runtime::Runtime::update` 复用它，保证 `runtime.json` 也是
/// 「tmp + 0600 + rename」，中途不会出现 0644 的窗口。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 旧版复制进 state.backups/，并裁到最近 BACKUP_KEEP 份。
fn backup(path: &Path, backups: &Path) -> Result<()> {
    std::fs::create_dir_all(backups)?;
    let stamp = time::OffsetDateTime::now_utc()
        .format(&time::macros::format_description!("[year][month][day]T[hour][minute][second]Z"))?;
    // 文件名一律带**零填充**的三位计数后缀 `state-<stamp>-<nnn>.json`。不要写成「先试
    // `state-<stamp>.json`，撞了再加 `-1`、`-2`…」：那种命名下同一秒内的多次写盘会产生
    // `state-<stamp>-1.json`…`-13.json`，而 `-` < `.`、`-10` < `-2`，下面按路径字典序裁剪就会
    // 把**最新**的几份删掉，正好毁掉 `bui upgrade --rollback` 要恢复的那一份。零填充之后
    // 「字典序 == 时间序」，裁剪才等于「删最旧的」。
    let mut dest = backups.join(format!("state-{stamp}-000.json"));
    let mut n = 1u32;
    while dest.exists() {
        dest = backups.join(format!("state-{stamp}-{n:03}.json"));
        n += 1;
    }
    std::fs::copy(path, &dest)?;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(backups)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    while entries.len() > BACKUP_KEEP {
        let oldest = entries.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}
```
注意：`create` 不备份（首次落盘没有旧版）；`update` 里 `old_bytes` 用**当前内存值**序列化后比较，因此「零变更不写盘」不依赖读磁盘。`write_atomic` 是本文件唯一的落盘入口（`Runtime::update` 也复用它，见下一步）。

- [ ] **Step 4: 实现 `runtime.rs`**

结构体按 Interfaces 原样写（每个字段 `#[serde(default)]`，`extra` 用 `#[serde(flatten)]`；`flatten` 会吃掉所有未知键，所以它必须是最后一个字段），`Runtime` 内部 `Arc<Inner { path: PathBuf, data: RwLock<RuntimeData> }>`：
```rust
impl Runtime {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "runtime.json 解析失败，按空白重建");
                RuntimeData::default()
            }),
            Err(_) => RuntimeData::default(),
        };
        Self(Arc::new(Inner { path, data: RwLock::new(data) }))
    }

    pub async fn read(&self) -> RuntimeData {
        self.0.data.read().await.clone()
    }

    /// 落盘是 best-effort：runtime.json 丢了能从零重建（spec §2.1）。
    /// 走 `store::write_atomic`（tmp + 0600 + rename）并放进 `spawn_blocking`——本方法在守护进程的
    /// 每一轮对账、watchdog 的每 60 秒都会被调，直接 `std::fs::write` 会阻塞 runtime 线程，而
    /// 「先 write 再 chmod」在这两次系统调用之间会把 `runtime.json` 暴露成 0644（里面有 `restart_keys`
    /// 与上一轮报告）。Global Constraints 要求它恒为 0600。
    pub async fn update(&self, f: impl FnOnce(&mut RuntimeData)) -> RuntimeData {
        let mut guard = self.0.data.write().await;
        f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        match serde_json::to_vec_pretty(&snapshot) {
            Ok(bytes) => {
                let path = self.0.path.clone();
                let joined = tokio::task::spawn_blocking(move || {
                    crate::state::store::write_atomic(&path, &bytes)
                })
                .await;
                match joined {
                    Ok(Err(e)) => tracing::warn!(error = %e, "runtime.json 落盘失败（忽略）"),
                    Err(e) => tracing::warn!(error = %e, "runtime.json 落盘任务 panic（忽略）"),
                    Ok(Ok(())) => {}
                }
            }
            Err(e) => tracing::warn!(error = %e, "runtime.json 序列化失败（忽略）"),
        }
        snapshot
    }
}
impl ReconcileReport {
    pub fn is_clean(&self) -> bool {
        self.changed.is_empty() && self.restarted.is_empty() && self.errors.is_empty() && self.verify_failures.is_empty()
    }
}
```
- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui state:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 12 passed（store 7 + runtime 5）。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/state
git commit -m "feat(bui): 期望态 Store（原子写 + 备份 10 份）与 runtime.json"
```

---

### Task 3: 可注入的系统执行器 `Host`（真实 / 内存 fake）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/sys/mod.rs`, `crates/bui/src/sys/real.rs`, `crates/bui/src/sys/fake.rs`（`main.rs` 的 `mod sys;` 已由 Task 1 写好）

**Interfaces:**
- Produces:
```rust
// crate::sys
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOut { pub status: i32, pub stdout: String, pub stderr: String }
impl CmdOut { pub fn ok(&self) -> bool; pub fn success(stdout: &str) -> Self; pub fn failure(code: i32, stderr: &str) -> Self; }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proto { Tcp, Udp }

/// 所有碰真实系统的操作都走这里；同步接口，对账在 spawn_blocking 里跑。
pub trait Host: Send + Sync {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>>;
    fn write_file(&self, path: &Path, content: &[u8], mode: u32) -> Result<()>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    /// **只返回直接子项**（文件与目录都返回，绝对路径，按路径名升序）；目录不存在 → `Ok(vec![])`，不是错误。
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;
    fn is_dir(&self, path: &Path) -> Result<bool>;
    /// 递归删除一个目录（`uninstall_v3` 删 v3 的 `admin/`、漂移清理删陌生目录）。
    fn remove_dir_all(&self, path: &Path) -> Result<()>;
    fn is_symlink(&self, path: &Path) -> Result<bool>;
    fn read_link(&self, path: &Path) -> Result<Option<PathBuf>>;
    /// 建/改符号链接（`/usr/local/bin/{bui,b-ui}`）：目标存在则先删再建。
    fn symlink(&self, target: &Path, link: &Path) -> Result<()>;
    fn set_immutable(&self, path: &Path, on: bool) -> Result<()>;
    fn is_immutable(&self, path: &Path) -> Result<bool>;
    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut>;
    fn which(&self, program: &str) -> bool;
    fn systemd_daemon_reload(&self) -> Result<()>;
    fn systemd(&self, verb: &str, unit: &str) -> Result<CmdOut>;
    fn unit_is_active(&self, unit: &str) -> Result<bool>;
    fn unit_is_enabled(&self, unit: &str) -> Result<bool>;
    fn unit_exists(&self, unit: &str) -> Result<bool>;
    fn unit_property(&self, unit: &str, prop: &str) -> Result<Option<String>>;
    fn sysctl_get(&self, key: &str) -> Result<Option<String>>;
    fn sysctl_set(&self, key: &str, value: &str) -> Result<()>;
    fn modprobe(&self, module: &str) -> Result<()>;
    fn mem_mb(&self) -> Result<u64>;
    fn arch(&self) -> Result<String>;
    fn hostname(&self) -> Result<String>;
    fn listening_ports(&self, proto: Proto) -> Result<BTreeSet<u16>>;
    fn now(&self) -> time::OffsetDateTime;
}

/// /proc/net/{tcp,udp,tcp6,udp6} 的本地端口解析。listening_only=true 时只取 st==0A（TCP LISTEN）。
pub fn parse_proc_net(text: &str, listening_only: bool) -> BTreeSet<u16>
pub struct RealHost;
impl RealHost { pub fn new() -> Self }
#[cfg(test)] pub mod fake;  // FakeHost / FakeInner
```
`FakeHost`（测试用，`#[cfg(test)]`）：
```rust
pub struct FakeHost { /* Mutex<FakeInner> */ }
#[derive(Default)]
pub struct FakeInner {
    pub files: BTreeMap<PathBuf, (Vec<u8>, u32)>,
    pub immutable: BTreeSet<PathBuf>,
    /// 符号链接：link → target
    pub symlinks: BTreeMap<PathBuf, PathBuf>,
    /// 显式存在的空目录（有文件的目录由 files 的路径前缀隐式存在）
    pub dirs: BTreeSet<PathBuf>,
    pub units_active: BTreeSet<String>,
    pub units_enabled: BTreeSet<String>,
    pub units_exist: BTreeSet<String>,
    /// 键是 `(单元**全名**, 属性名)`，如 `("hysteria-server.service", "NRestarts")`
    pub unit_props: BTreeMap<(String, String), String>,
    pub sysctl: BTreeMap<String, String>,
    pub which: BTreeSet<String>,
    pub modules: BTreeSet<String>,
    /// 前缀匹配的脚本化命令结果："xray run -test" → CmdOut
    pub scripted: Vec<(String, CmdOut)>,
    /// 令某单元的 systemd 动作失败（测回滚）
    pub fail_units: BTreeSet<String>,
    pub listening: BTreeMap<Proto, BTreeSet<u16>>,
    pub mem_mb: u64,
    pub arch: String,
    pub hostname: String,
    pub now: time::OffsetDateTime,
    /// 操作流水（顺序可断言）
    pub ops: Vec<String>,
}
impl FakeHost {
    pub fn new() -> Self;                                   // mem_mb=2048, arch="x86_64", hostname="node-a", now=2026-09-11T00:00:00Z
    pub fn with(&self, f: impl FnOnce(&mut FakeInner)) -> &Self;
    pub fn ops(&self) -> Vec<String>;
    pub fn clear_ops(&self);
    pub fn text(&self, path: &str) -> Option<String>;       // 读回写入的文件内容
    pub fn mode(&self, path: &str) -> Option<u32>;
    pub fn advance(&self, secs: i64);                       // now += secs
}
```
`ops` 字符串格式（后续任务的断言依赖它，不得更改）：
`write:<path>:<mode 八进制三位>`、`remove:<path>`、`rmdir:<path>`、`symlink:<link>-><target>`、`chattr:+i:<path>` / `chattr:-i:<path>`、`daemon-reload`、`systemd:<verb>:<unit>`、`sysctl:<key>=<value>`、`modprobe:<module>`、`run:<program> <args 以空格连接>`。

**`list_dir` / `is_dir` 语义（真假两套必须一致，漂移扫描与 v3 卸载都依赖它）**：
- `list_dir` 只返回**直接子项**，不递归；文件与目录都返回；绝对路径；按路径名升序；目录不存在返回空 `Vec`。
- `RealHost`：`std::fs::read_dir` 的条目路径（含目录），忽略 `NotFound`。
- `FakeHost`：遍历 `files` 与 `dirs` 的键，取在 `path` 下的部分，只保留第一段拼回 `path`，去重排序——于是 `files` 里有 `/opt/b-ui/bin/xray` 时 `list_dir("/opt/b-ui")` 返回 `["/opt/b-ui/bin"]`（一个隐式目录条目），不会把 `bin/` 里的四个文件抖到 `/opt/b-ui` 层。
- `is_dir`：`FakeHost` 里 `/sys/module/<m>` 形态的路径按 `modules` 集合回答（这样 `modprobe` 之后第二轮对账判定为已加载，幂等成立）；其余路径按「是 `dirs` 成员，或是某个 `files` 键的严格前缀目录」回答。`RealHost` 用 `std::fs::metadata(..).is_dir()`。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/sys/mod.rs` 末尾（`/proc/net` 解析用本机真实样本）：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const UDP: &str = "\
   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  654: 00000000:D3A1 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 201613515 2 0000000000000000 0
 1858: 00000000:D855 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 201624164 2 0000000000000000 0
";

    const TCP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:7235 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5931149 1 0000000000000000 100 0 0 10 0
   1: 0100007F:7751 00000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 35608872 1 0000000000000000 100 0 0 10 0
";

    #[test]
    fn parses_udp_local_ports() {
        assert_eq!(parse_proc_net(UDP, false), BTreeSet::from([0xD3A1, 0xD855]));
    }

    #[test]
    fn parses_only_listening_tcp_ports() {
        // 0x7235 是 LISTEN(0A)，0x7751 是 ESTABLISHED(01)
        assert_eq!(parse_proc_net(TCP, true), BTreeSet::from([0x7235]));
        assert_eq!(parse_proc_net(TCP, false), BTreeSet::from([0x7235, 0x7751]));
    }

    #[test]
    fn ignores_garbage_lines() {
        assert_eq!(parse_proc_net("nonsense\n\n   sl  local_address\n", false), BTreeSet::new());
    }
}
```

`crates/bui/src/sys/fake.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Host;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[test]
    fn records_file_writes_with_mode() {
        let h = FakeHost::new();
        h.write_file(Path::new("/opt/b-ui/config.yaml"), b"listen: :10000", 0o600).unwrap();
        assert_eq!(h.text("/opt/b-ui/config.yaml").unwrap(), "listen: :10000");
        assert_eq!(h.mode("/opt/b-ui/config.yaml"), Some(0o600));
        assert_eq!(h.ops(), vec!["write:/opt/b-ui/config.yaml:600"]);
        assert_eq!(h.read_file(Path::new("/nope")).unwrap(), None);
    }

    #[test]
    fn records_systemd_and_sysctl_ops_in_order() {
        let h = FakeHost::new();
        h.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.sysctl.insert("net.ipv4.tcp_retries2".into(), "15".into());
        });
        assert!(h.unit_is_active("hysteria-server").unwrap());
        h.systemd_daemon_reload().unwrap();
        h.systemd("restart", "hysteria-server").unwrap();
        h.sysctl_set("net.ipv4.tcp_retries2", "8").unwrap();
        assert_eq!(h.sysctl_get("net.ipv4.tcp_retries2").unwrap().as_deref(), Some("8"));
        assert_eq!(
            h.ops(),
            vec!["daemon-reload", "systemd:restart:hysteria-server", "sysctl:net.ipv4.tcp_retries2=8"]
        );
    }

    #[test]
    fn scripted_commands_and_failures() {
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push(("xray run -test".into(), CmdOut::failure(1, "invalid config")));
            i.fail_units.insert("b-ui-relay".into());
        });
        let out = h.run("xray", &["run", "-test", "-c", "/opt/b-ui/.verify/xray-config.json"]).unwrap();
        assert!(!out.ok());
        assert_eq!(out.stderr, "invalid config");
        // 未脚本化的命令默认成功
        assert!(h.run("sshd", &["-t"]).unwrap().ok());
        assert!(!h.systemd("restart", "b-ui-relay").unwrap().ok());
        assert!(h.systemd("restart", "xray").unwrap().ok());
    }

    #[test]
    fn immutable_flag_and_clock() {
        let h = FakeHost::new();
        let p = Path::new("/etc/resolv.conf");
        assert!(!h.is_immutable(p).unwrap());
        h.set_immutable(p, true).unwrap();
        assert!(h.is_immutable(p).unwrap());
        let t0 = h.now();
        h.advance(600);
        assert_eq!((h.now() - t0).whole_seconds(), 600);
        assert_eq!(h.ops(), vec!["chattr:+i:/etc/resolv.conf"]);
    }

    #[test]
    fn list_dir_returns_only_direct_children_including_implicit_dirs() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert("/opt/b-ui/admin/node_modules/x/index.js".into(), (b"x".to_vec(), 0o644));
            i.dirs.insert("/opt/b-ui/certs".into());
        });
        assert_eq!(
            h.list_dir(Path::new("/opt/b-ui")).unwrap(),
            vec![
                std::path::PathBuf::from("/opt/b-ui/admin"),
                std::path::PathBuf::from("/opt/b-ui/bin"),
                std::path::PathBuf::from("/opt/b-ui/certs"),
                std::path::PathBuf::from("/opt/b-ui/state.json"),
            ]
        );
        assert!(h.is_dir(Path::new("/opt/b-ui/bin")).unwrap());
        assert!(h.is_dir(Path::new("/opt/b-ui/certs")).unwrap());
        assert!(!h.is_dir(Path::new("/opt/b-ui/state.json")).unwrap());
        assert_eq!(h.list_dir(Path::new("/nope")).unwrap(), Vec::<std::path::PathBuf>::new());
    }

    #[test]
    fn remove_dir_all_takes_the_whole_subtree() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/admin/server.js".into(), (b"node".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/admin/node_modules/x/index.js".into(), (b"x".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
        });
        h.clear_ops();
        h.remove_dir_all(Path::new("/opt/b-ui/admin")).unwrap();
        assert!(h.text("/opt/b-ui/admin/node_modules/x/index.js").is_none());
        assert!(h.text("/opt/b-ui/state.json").is_some(), "只删指定子树");
        assert_eq!(h.ops(), vec!["rmdir:/opt/b-ui/admin"]);
    }

    #[test]
    fn symlinks_are_readable_and_replaceable() {
        let h = FakeHost::new();
        let link = Path::new("/usr/local/bin/b-ui");
        assert_eq!(h.read_link(link).unwrap(), None);
        h.symlink(Path::new("/opt/b-ui/bin/bui"), link).unwrap();
        assert_eq!(h.read_link(link).unwrap(), Some(std::path::PathBuf::from("/opt/b-ui/bin/bui")));
        assert!(h.is_symlink(link).unwrap());
        h.symlink(Path::new("/opt/b-ui/bin/bui2"), link).unwrap();
        assert_eq!(h.read_link(link).unwrap(), Some(std::path::PathBuf::from("/opt/b-ui/bin/bui2")));
        assert_eq!(
            h.ops(),
            vec!["symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui", "symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui2"]
        );
    }

    #[test]
    fn modprobe_makes_sys_module_appear() {
        let h = FakeHost::new();
        assert!(!h.is_dir(Path::new("/sys/module/nf_conntrack")).unwrap());
        h.modprobe("nf_conntrack").unwrap();
        assert!(h.is_dir(Path::new("/sys/module/nf_conntrack")).unwrap(), "加载后第二轮对账要判成已加载");
        assert_eq!(h.ops(), vec!["modprobe:nf_conntrack"]);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui sys::`
Expected: 编译失败（`parse_proc_net` / `FakeHost` 未定义）。

- [ ] **Step 3: 实现 `mod.rs` 的 trait 与解析器**

```rust
pub mod real;
#[cfg(test)]
pub mod fake;

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOut { pub status: i32, pub stdout: String, pub stderr: String }

impl CmdOut {
    pub fn ok(&self) -> bool { self.status == 0 }
    pub fn success(stdout: &str) -> Self { Self { status: 0, stdout: stdout.into(), stderr: String::new() } }
    pub fn failure(code: i32, stderr: &str) -> Self { Self { status: code, stdout: String::new(), stderr: stderr.into() } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proto { Tcp, Udp }

pub trait Host: Send + Sync { /* 见 Interfaces，逐条声明 */ }

/// 第二列是 `HEXIP:HEXPORT`；TCP 的 LISTEN 状态是 `0A`（第四列）。
pub fn parse_proc_net(text: &str, listening_only: bool) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        if listening_only && cols[3] != "0A" {
            continue;
        }
        let Some((_, port_hex)) = cols[1].split_once(':') else { continue };
        if let Ok(port) = u16::from_str_radix(port_hex, 16) {
            out.insert(port);
        }
    }
    out
}
```

- [ ] **Step 4: 实现 `real.rs`**

要点（每条都只是薄封装，不含业务判断）：
- `read_file`：`std::fs::read`，`NotFound` → `Ok(None)`。
- `write_file`：`create_dir_all(parent)` → 同目录 `.<name>.tmp` → `write_all` + `sync_all` → `set_permissions(mode)` → `rename`；若目标是 symlink 先 `remove_file`（`/etc/resolv.conf` 是 systemd-resolved 的 symlink 时必须先删）。
- `list_dir`：`std::fs::read_dir` 的条目路径（文件与目录都要），排序返回；`NotFound` → `Ok(vec![])`。
- `is_dir`：`std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)`（`/sys/module/<m>` 也用这条，模块加载后该目录存在）。
- `remove_dir_all`：`std::fs::remove_dir_all`，`NotFound` → `Ok(())`。
- `read_link`：`std::fs::read_link`，非链接或不存在 → `Ok(None)`。
- `symlink`：`create_dir_all(link.parent())` 先行；**用 `std::fs::symlink_metadata(link).is_ok()` 判「已占位」**（链接、普通文件、悬空链接都命中）→ 先 `remove_file(link)`；再 `std::os::unix::fs::symlink(target, link)`。
  **不能用 `Path::exists()`**：它跟随符号链接，对**悬空链接**返回 false。而 `uninstall_v3` 删掉 `<base>/b-ui-cli.sh` 之后，v3 留下的 `/usr/local/bin/b-ui → /opt/b-ui/b-ui-cli.sh` 正是一条悬空链接；判 false 就不删，随后的 `symlink()` 直接 `EEXIST`，`bui install --import-v3` 每轮都报一条「`/usr/local/bin/b-ui` 写入失败」而 CLI 入口永远建不起来（spec §2.4 的 `sudo b-ui` 进不去菜单）。
- `set_immutable`：`run("chattr", &[if on {"+i"} else {"-i"}, path])`，失败只 `warn!` 返回 `Ok(())`（文件系统可能不支持，与 v3 `core.sh:1317` 行为一致）。
- `is_immutable`：`run("lsattr", &["-d", path])`，输出首字段含 `i` 即 true；命令缺失 → `Ok(false)`。
- `run`：`std::process::Command`，捕获 stdout/stderr（`from_utf8_lossy`），`status.code().unwrap_or(-1)`。**不记录 args 到日志**（可能含域名，但绝不含凭据；凭据只经 stdin）。
- `systemd(verb, unit)`：`run("systemctl", &[verb, &format!("{unit}.service")])`；`unit` 已含 `.timer` 等后缀时不再追加（判断 `unit.contains('.')`）。
- `unit_is_active` / `unit_is_enabled`：`systemctl is-active|is-enabled` 的 `status == 0`。
- `unit_exists`：`systemctl list-unit-files <unit>` 输出含 unit 名。
- `unit_property`：`systemctl show -p <prop> --value <unit>`，空串 → `None`。
- `sysctl_get`：`run("sysctl", &["-n", key])`，失败 → `Ok(None)`；`sysctl_set`：`run("sysctl", &["-w", &format!("{key}={value}")])`，非零 → `Err`。
- `mem_mb`：解析 `/proc/meminfo` 的 `MemTotal:`（kB / 1024）。
- `arch`：`std::env::consts::ARCH`（`x86_64` / `aarch64`）。
- `hostname`：`nix::unistd::gethostname()`。
- `listening_ports(Tcp)`：`parse_proc_net(/proc/net/tcp, true) ∪ parse_proc_net(/proc/net/tcp6, true)`；`Udp` 同理但 `listening_only=false`。
- `now`：`time::OffsetDateTime::now_utc()`。

- [ ] **Step 5: 实现 `fake.rs`**

按 Interfaces 实现；要点：
- 所有状态在 `Mutex<FakeInner>`；每个改动方法 `push` 一条 `ops`。
- `run`：在 `scripted` 里按「`program` + 空格 + args 连接」的**前缀**匹配，找到即返回对应 `CmdOut`；否则 `CmdOut::success("")`。始终记录 `run:` 流水。
- **单元名归一化（后续任务的断言隐含依赖它，必须写死）**：`unit_is_active` / `unit_is_enabled` / `unit_exists` / `unit_property` / `systemd` / `fail_units` 的查询键一律先归一化——参数**不含 `.`** 时补 `.service`，已含后缀（`.timer` / `.service`）则原样。于是 `units_active` 里放 `hysteria-server.service`、查询写 `unit_is_active("hysteria-server")` 能命中（Task 13、16 的断言这么写），`fail_units` 里放裸名 `hysteria-server` 也能命中（Task 5 的回滚测试这么写：归一化后按裸名与全名两种键各查一次，任一命中即失败）。
  **`unit_props` 只认全名**（`fail_units` 的「两种键各查一次」宽容**不适用**于它，别顺手抄过去）：`unit_property("hysteria-server", "NRestarts")` 归一化后拿 `("hysteria-server.service", "NRestarts")` 去查 `unit_props`，所以播种必须写全名；播成裸名查不到、返回 `None`，调用方（Task 13 的 `health::get` 用 `.unwrap_or(0)`）会把它静默算成 `n_restarts: 0`，测试断言 `== 3` 必失败。Task 13 的 health 测试按全名播种，与同一处 `units_active` 的写法一致。`RealHost::systemd` 用同一条规则拼 `systemctl <verb> <unit>.service`（Task 3 第 4 步已写明），真假两套语义一致。
- `systemd`：`fail_units` 命中该 unit → `CmdOut::failure(1, "Job failed")`，且不改 `units_active`；否则按 verb 更新 `units_active` / `units_enabled`（键用归一化后的全名）并返回成功。`ops` 里记的是**传进来的原样名字**（`systemd:restart:hysteria-server`）。
- `write_file` 记录 `write:<path>:<mode:03o>` 并写入 `files`；`remove_file` 记录 `remove:` 并删除。
- `list_dir` / `is_dir` 按上面「语义」一节实现；`remove_dir_all` 删掉所有以 `<path>/` 开头的 `files` 与 `dirs` 键并记 `rmdir:<path>`；`symlink` 写 `symlinks` 并记 `symlink:<link>-><target>`；`read_link` 查 `symlinks`；`is_symlink` = `symlinks` 含该键。
- `modprobe` 把模块名插入 `modules` 并记 `modprobe:<module>`（于是 `is_dir("/sys/module/<m>")` 变 true）。
- `Default for FakeHost` 调 `new()`（clippy `new_without_default`）。

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui sys:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 11 passed（mod 3 + fake 8）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/sys
git commit -m "feat(bui): 可注入的系统执行器 Host（RealHost / FakeHost）"
```

---

### Task 4: `Artifact` / `Module` 契约、受管单元常量、`AppState`/`EventBus` 类型与 diff 计算

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/reconcile/mod.rs`, `crates/bui/src/reconcile/diff.rs`
- Modify（填 Task 1 建好的桩）: `crates/bui/src/api/state.rs`（只放 `AppState` / `Event` / `EventBus` 三个类型；HTTP 实现在 Task 13）
- Modify: `crates/bui/src/api/mod.rs`（在 Task 1 写好的四行 `pub mod` 后追加一行 `pub use state::{AppState, Event, EventBus};`）

**为什么 `AppState`/`EventBus` 在这里**：总纲 C2 的 `Module::routes(&self) -> axum::Router<AppState>` 与 `DaemonCtx.bus: EventBus` 都在本任务定义，若把这三个类型留到 Task 13，Task 4 与 Task 13 互相依赖、谁都编译不过。类型前移后 Task 13 只负责 handler、中间件与 `router()`。

**Interfaces:**
- Consumes: `crate::sys::{Host, Proto, CmdOut}`；`crate::state::runtime::{DriftItem, ReconcileReport, Runtime}`；`crate::state::store::Store`；`bui_schema::{model::State, paths::Paths}`
- Produces（总纲 C2 的 `Module` / `Artifact`，变体名与方法名不变，字段细化；新增变体与裁决依据见文首「依赖与契约决策」）：
```rust
// crate::reconcile
/// v4 受管的六个单元；health 端点、`/api/services` 白名单、漂移的 drop-in 扫描都用这一份
pub const MANAGED_UNITS: [&str; 6] = ["b-ui", "hysteria-server", "hysteria-residential", "xray", "b-ui-relay", "caddy"];
/// v3 遗留的单元与定时器（v4 一概不生成）：units 模块产出 `UnitState{false,false}` + `Absent`，
/// 漂移扫描对「删不掉还在的」再报一次。**唯一一份**，Task 5 与 Task 8 都引用它。
pub const LEGACY_UNITS: [&str; 9] = [
    "hy2-watchdog.timer", "hy2-watchdog.service",
    "b-ui-cert-sync.timer", "b-ui-cert-sync.service",
    "b-ui-resi-health.timer", "b-ui-resi-health.service",
    "b-ui-admin.service", "hysteria-server@.service", "xray@.service",
];
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unit { pub name: String, pub action: UnitAction }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitAction { Restart, Reload }
impl Unit {
    pub fn restart(name: &str) -> Self;
    pub fn reload(name: &str) -> Self;
    pub fn service(&self) -> String;         // "hysteria-server.service"
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify { Xray, SingBox, Caddy, Sshd }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec { pub proto: Proto, pub from: u16, pub to: u16 }
impl PortSpec { pub fn one(proto: Proto, port: u16) -> Self; pub fn range(proto: Proto, from: u16, to: u16) -> Self; }

#[derive(Debug, Clone, PartialEq)]
pub enum Artifact {
    File { path: PathBuf, content: Vec<u8>, mode: u32, immutable: bool, restart: Option<Unit>, restart_key: Option<String>, verify: Option<Verify> },
    Unit { name: Unit, dropin: Option<String>, content: String },
    UnitState { name: String, enabled: bool, active: bool },
    Sysctl { key: String, value: String },
    /// 内核模块必须**现在**加载（spec §2.2 的「nf_conntrack 模块」）；
    /// `/etc/modules-load.d` 只管下次开机，不写这条的话首装当轮 `sysctl -w net.netfilter.*` 全 ENOENT。
    Modprobe { module: String },
    Binary { name: String, version: String, sha256: String, url: String },
    /// CLI 入口符号链接（`/usr/local/bin/{bui,b-ui}` → `<base>/bin/bui`，spec §1、§2.4）
    Symlink { path: PathBuf, target: PathBuf },
    FirewallPorts { ports: Vec<PortSpec> },
    Absent { path: PathBuf },
}
impl Artifact {
    pub fn file(path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> Self;   // mode 0600、无 restart/verify
    pub fn mode(self, mode: u32) -> Self;
    pub fn restart(self, unit: Unit) -> Self;
    pub fn restart_key(self, key: impl Into<String>) -> Self;
    pub fn verify(self, v: Verify) -> Self;
    pub fn immutable(self) -> Self;
    pub fn id(&self) -> String;     // 稳定标识，用作 runtime.restart_keys 的键
    pub fn unit_path(name: &str, dropin: Option<&str>) -> PathBuf;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub mem_mb: u64, pub arch: String, pub hostname: String,
    pub has_ufw: bool, pub ufw_active: bool, pub has_firewalld: bool, pub firewalld_active: bool,
    pub ssh_unit: String, pub ssh_pubkeys: u32, pub systemd_resolved: bool,
}
impl Facts { pub fn probe(host: &dyn Host) -> anyhow::Result<Facts>; }

pub struct RenderCtx { pub paths: Paths, pub facts: Facts }

pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;
    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact>;
    fn routes(&self) -> axum::Router<crate::api::AppState> { axum::Router::new() }
    fn spawn(&self, _ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> { Vec::new() }
}

#[derive(Clone)]
pub struct DaemonCtx {
    pub store: Store, pub runtime: Runtime, pub bus: crate::api::EventBus,
    pub host: std::sync::Arc<dyn Host>, pub paths: Paths,
}
// crate::api::state（本任务创建，Task 13 只加 login 字段与 handler）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event { StateChanged(&'static str), RelayRestarted, ReconcileRequested { force: bool } }
#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<Event>);
impl EventBus {
    pub fn new() -> Self;                                   // 容量 64
    pub fn send(&self, e: Event);                           // 无订阅者时静默丢弃
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event>;
}
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub bus: EventBus,
    pub runtime: Runtime,
    pub host: std::sync::Arc<dyn Host>,
    pub started_at: time::OffsetDateTime,
    pub version: &'static str,
    // Task 13 追加：pub login: crate::api::auth::LoginLimiter
}
// crate::reconcile::diff
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    WriteFile { path: PathBuf, content: Vec<u8>, mode: u32, verify: Option<Verify>, restart: Option<Unit> },
    SetImmutable { path: PathBuf, on: bool },
    RemoveFile { path: PathBuf },
    WriteUnit { path: PathBuf, content: String, unit: Unit },
    SetUnitState { unit: String, enabled: bool, active: bool },
    SetSysctl { key: String, value: String },
    LoadModule { module: String },
    InstallBinary { name: String, version: String, sha256: String, url: String, path: PathBuf },
    WriteSymlink { path: PathBuf, target: PathBuf },
    OpenPorts { ports: Vec<PortSpec> },
}
#[derive(Debug, Default, PartialEq)]
pub struct Plan { pub changes: Vec<Change>, pub keys: BTreeMap<String, String>, pub unchanged: usize }
pub struct PlanInput<'a> {
    pub artifacts: &'a [Artifact],
    pub paths: &'a Paths,
    pub keys: &'a BTreeMap<String, String>,             // runtime.restart_keys
    pub installed_versions: &'a BTreeMap<String, String>,
}
pub fn plan(input: PlanInput<'_>, host: &dyn Host) -> anyhow::Result<Plan>
```
`plan` 规则（spec §2.2「按内容哈希比对，只写有差异的」）：
1. `File`：内容不同 → `WriteFile`；重启单元的决定：`restart_key = Some(k)` 时只有 `keys[id] != k` 才带 `restart`（xray 的 `clients` 变化不重启），同时把 `id → k` 记入 `Plan.keys`——`Plan.keys` 只是**候选**，真正落盘由 Task 5 的 apply 在写成功后搬进 `ApplyOutcome.keys`（校验失败/写失败不搬）；`restart_key = None` 时内容变化就带 `restart`。内容相同但 `immutable` 位与现状不符 → 只出 `SetImmutable`。内容不同且 `immutable=true` → `WriteFile` + `SetImmutable{on:true}`（apply 里先解锁再写，见 Task 5）。
2. `Unit`：目标路径 `/etc/systemd/system/<name>.service` 或 `<name>.service.d/<dropin>`；内容不同 → `WriteUnit`。
3. `UnitState`：`unit_is_enabled` / `unit_is_active` 与期望不符 → `SetUnitState`。
4. `Sysctl`：`sysctl_get` 与期望不符 → `SetSysctl`。
5. `Modprobe`：`host.is_dir("/sys/module/<module>")` 为 false → `LoadModule`（已加载则 `unchanged += 1`，所以第二轮对账为零变更）。
6. `Binary`：`installed_versions[name] != version` → `InstallBinary`（路径 `paths.bin_dir/<name>`）。
7. `Symlink`：`host.read_link(path) != Some(target)` → `WriteSymlink`（含「路径是普通文件」「链接指错地方」两种情况）。
8. `FirewallPorts`：以端口集的 sha256 前 16 位作为 key，`keys["firewall"]` 不符 → `OpenPorts` 并把 `firewall → key` 记入 `Plan.keys`（同样只是候选：apply 只在真的调过 ufw/firewalld 之后才搬进 `ApplyOutcome.keys`，避免每轮重复 `ufw allow`）。
   **本规则的前提**：产出这个 artifact 的模块必须确认机器上有活的防火墙。Task 6 的 `SystemModule::render` 因此只在 `facts.ufw_active || facts.firewalld_active` 为真时才产出 `FirewallPorts`；没有防火墙的机器上它一个都不产出，提示由 Task 15 的 `reconcile_once` 从 `facts` 生成。否则就会撞上一个死循环：apply 什么都没改 → 不搬 key → 本规则下一轮又出一条 `OpenPorts` → `changed` 每轮多一条、`keys["firewall"]` 永不出现，M1 的「二次对账零变更」在没装 ufw 的机器上永远不可达。
9. `Absent`：文件存在 → `RemoveFile`。
其余情况 `unchanged += 1`。

- [ ] **Step 1: 写失败测试**

`crates/bui/src/reconcile/diff.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Unit, Verify};
    use crate::sys::{fake::FakeHost, Host, Proto};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    fn input<'a>(
        artifacts: &'a [Artifact],
        paths: &'a Paths,
        keys: &'a BTreeMap<String, String>,
        versions: &'a BTreeMap<String, String>,
    ) -> PlanInput<'a> {
        PlanInput { artifacts, paths, keys, installed_versions: versions }
    }

    #[test]
    fn missing_file_is_written_and_restarts_its_unit() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/opt/b-ui/config.yaml", "listen: :10000")
            .restart(Unit::restart("hysteria-server"))];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(),
                content: b"listen: :10000".to_vec(),
                mode: 0o600,
                verify: None,
                restart: Some(Unit::restart("hysteria-server")),
            }]
        );
        assert_eq!(p.unchanged, 0);
    }

    #[test]
    fn identical_file_is_untouched() {
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/config.yaml"), b"listen: :10000", 0o600).unwrap();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/opt/b-ui/config.yaml", "listen: :10000")];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert!(p.changes.is_empty());
        assert_eq!(p.unchanged, 1);
    }

    #[test]
    fn restart_key_suppresses_restart_when_structure_is_unchanged() {
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/xray-config.json"), b"{\"clients\":[1]}", 0o600).unwrap();
        let paths = Paths::default_server();
        let mut keys = BTreeMap::new();
        keys.insert("file:/opt/b-ui/xray-config.json".to_string(), "hash-A".to_string());
        let versions = BTreeMap::new();
        let arts = vec![Artifact::file("/opt/b-ui/xray-config.json", "{\"clients\":[1,2]}")
            .restart(Unit::restart("xray"))
            .restart_key("hash-A")
            .verify(Verify::Xray)];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        match &p.changes[0] {
            Change::WriteFile { restart, verify, .. } => {
                assert_eq!(*restart, None, "结构哈希未变 → 不重启 xray");
                assert_eq!(*verify, Some(Verify::Xray));
            }
            other => panic!("{other:?}"),
        }
        // 结构哈希变了就要重启
        let arts2 = vec![Artifact::file("/opt/b-ui/xray-config.json", "{\"clients\":[1,2]}")
            .restart(Unit::restart("xray"))
            .restart_key("hash-B")];
        let p2 = plan(input(&arts2, &paths, &keys, &versions), &h).unwrap();
        match &p2.changes[0] {
            Change::WriteFile { restart, .. } => assert_eq!(*restart, Some(Unit::restart("xray"))),
            other => panic!("{other:?}"),
        }
        assert_eq!(p2.keys.get("file:/opt/b-ui/xray-config.json").map(String::as_str), Some("hash-B"));
    }

    #[test]
    fn immutable_flag_alone_produces_only_a_flag_change() {
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/etc/resolv.conf"), b"nameserver 1.1.1.1\n", 0o644).unwrap();
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![Artifact::file("/etc/resolv.conf", "nameserver 1.1.1.1\n").mode(0o644).immutable()];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(p.changes, vec![Change::SetImmutable { path: "/etc/resolv.conf".into(), on: true }]);
    }

    #[test]
    fn unit_sysctl_unitstate_binary_and_absent() {
        let h = FakeHost::new();
        h.with(|i| {
            i.sysctl.insert("net.ipv4.tcp_retries2".into(), "8".into());
            i.sysctl.insert("net.core.default_qdisc".into(), "pfifo_fast".into());
            i.units_enabled.insert("b-ui.service".into());
            i.files.insert("/etc/systemd/system/hy2-watchdog.timer".into(), (b"x".to_vec(), 0o644));
        });
        let paths = Paths::default_server();
        let (keys, mut versions) = (BTreeMap::new(), BTreeMap::new());
        versions.insert("xray".to_string(), "26.3.27".to_string());
        let arts = vec![
            Artifact::Unit { name: Unit::restart("b-ui"), dropin: None, content: "[Service]\n".into() },
            Artifact::UnitState { name: "b-ui".into(), enabled: true, active: true },
            Artifact::Sysctl { key: "net.ipv4.tcp_retries2".into(), value: "8".into() },
            Artifact::Sysctl { key: "net.core.default_qdisc".into(), value: "fq".into() },
            Artifact::Binary { name: "xray".into(), version: "26.3.27".into(), sha256: "aa".into(), url: "https://x/y".into() },
            Artifact::Binary { name: "sing-box".into(), version: "1.13.19".into(), sha256: "bb".into(), url: "https://x/z".into() },
            Artifact::Absent { path: "/etc/systemd/system/hy2-watchdog.timer".into() },
            Artifact::Absent { path: "/etc/systemd/system/gone.timer".into() },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("b-ui"),
                },
                Change::SetUnitState { unit: "b-ui".into(), enabled: true, active: true },
                Change::SetSysctl { key: "net.core.default_qdisc".into(), value: "fq".into() },
                Change::InstallBinary {
                    name: "sing-box".into(),
                    version: "1.13.19".into(),
                    sha256: "bb".into(),
                    url: "https://x/z".into(),
                    path: "/opt/b-ui/bin/sing-box".into(),
                },
                Change::RemoveFile { path: "/etc/systemd/system/hy2-watchdog.timer".into() },
            ]
        );
        assert_eq!(p.unchanged, 3, "tcp_retries2 / xray 版本 / 不存在的 Absent");
    }

    #[test]
    fn modprobe_and_symlink_are_planned_only_when_missing_or_wrong() {
        let h = FakeHost::new();
        h.with(|i| {
            i.modules.insert("tcp_bbr".into());
            i.symlinks.insert("/usr/local/bin/bui".into(), "/opt/b-ui/bin/bui".into());
            i.symlinks.insert("/usr/local/bin/b-ui".into(), "/opt/hysteria/b-ui-cli.sh".into());
        });
        let paths = Paths::default_server();
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let arts = vec![
            Artifact::Modprobe { module: "tcp_bbr".into() },
            Artifact::Modprobe { module: "nf_conntrack".into() },
            Artifact::Symlink { path: "/usr/local/bin/bui".into(), target: "/opt/b-ui/bin/bui".into() },
            Artifact::Symlink { path: "/usr/local/bin/b-ui".into(), target: "/opt/b-ui/bin/bui".into() },
        ];
        let p = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert_eq!(
            p.changes,
            vec![
                Change::LoadModule { module: "nf_conntrack".into() },
                Change::WriteSymlink { path: "/usr/local/bin/b-ui".into(), target: "/opt/b-ui/bin/bui".into() },
            ]
        );
        assert_eq!(p.unchanged, 2, "已加载的模块与已正确的链接都不动");
    }

    #[test]
    fn firewall_ports_only_reapply_when_the_set_changes() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let versions = BTreeMap::new();
        let ports = vec![PortSpec::one(Proto::Tcp, 22), PortSpec::range(Proto::Udp, 20000, 30000)];
        let arts = vec![Artifact::FirewallPorts { ports: ports.clone() }];
        let first = plan(input(&arts, &paths, &BTreeMap::new(), &versions), &h).unwrap();
        assert_eq!(first.changes, vec![Change::OpenPorts { ports }]);
        let key = first.keys.get("firewall").cloned().unwrap();
        let mut keys = BTreeMap::new();
        keys.insert("firewall".to_string(), key);
        let second = plan(input(&arts, &paths, &keys, &versions), &h).unwrap();
        assert!(second.changes.is_empty(), "端口集没变就不重复放行");
    }

    #[test]
    fn artifact_ids_are_stable_and_distinct() {
        assert_eq!(Artifact::file("/a", "x").id(), "file:/a");
        assert_eq!(
            Artifact::Unit { name: Unit::restart("xray"), dropin: None, content: String::new() }.id(),
            "unit:xray"
        );
        assert_eq!(
            Artifact::Unit { name: Unit::restart("xray"), dropin: Some("99-b-ui.conf".into()), content: String::new() }.id(),
            "unit:xray:99-b-ui.conf"
        );
        assert_eq!(Artifact::Sysctl { key: "a.b".into(), value: "1".into() }.id(), "sysctl:a.b");
        assert_eq!(Artifact::Modprobe { module: "nf_conntrack".into() }.id(), "modprobe:nf_conntrack");
        assert_eq!(
            Artifact::Symlink { path: "/usr/local/bin/b-ui".into(), target: "/x".into() }.id(),
            "symlink:/usr/local/bin/b-ui"
        );
        assert_eq!(Artifact::FirewallPorts { ports: vec![] }.id(), "firewall");
    }
}
```

`crates/bui/src/reconcile/mod.rs` 末尾（Facts 探测）：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;

    #[test]
    fn probes_firewall_ssh_and_resolved_facts() {
        let h = FakeHost::new();
        h.with(|i| {
            i.mem_mb = 3072;
            i.which.insert("ufw".into());
            i.scripted.push(("ufw status".into(), crate::sys::CmdOut::success("Status: active\n")));
            i.units_exist.insert("ssh.service".into());
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"# comment\nssh-ed25519 AAAA me@host\necdsa-sha2-nistp256 BBBB other\n".to_vec(), 0o600),
            );
            i.units_active.insert("systemd-resolved.service".into());
        });
        let f = Facts::probe(&h).unwrap();
        assert_eq!(f.mem_mb, 3072);
        assert!(f.has_ufw && f.ufw_active);
        assert!(!f.has_firewalld && !f.firewalld_active);
        assert_eq!(f.ssh_unit, "ssh", "有 ssh.service 就用 ssh，否则 sshd");
        assert_eq!(f.ssh_pubkeys, 2);
        assert!(f.systemd_resolved);
        assert_eq!(f.arch, "x86_64");
        assert_eq!(f.hostname, "node-a");
    }

    #[test]
    fn defaults_to_sshd_when_no_ssh_unit_and_zero_pubkeys() {
        let h = FakeHost::new();
        let f = Facts::probe(&h).unwrap();
        assert_eq!(f.ssh_unit, "sshd");
        assert_eq!(f.ssh_pubkeys, 0);
        assert!(!f.systemd_resolved);
    }

    #[test]
    fn unit_paths_cover_dropins() {
        assert_eq!(Artifact::unit_path("xray", None), std::path::PathBuf::from("/etc/systemd/system/xray.service"));
        assert_eq!(
            Artifact::unit_path("xray", Some("99-b-ui-override.conf")),
            std::path::PathBuf::from("/etc/systemd/system/xray.service.d/99-b-ui-override.conf")
        );
        assert_eq!(Unit::restart("hysteria-server").service(), "hysteria-server.service");
    }

    #[test]
    fn managed_and_legacy_unit_lists_are_the_single_source_of_truth() {
        assert_eq!(MANAGED_UNITS.len(), 6);
        assert!(MANAGED_UNITS.contains(&"b-ui-relay"), "v4 自己的 relay 单元必须在受管列表里");
        // v3 遗留列表绝不能含 v4 受管单元，否则对账刚写完就被卸载/清理掉
        for m in MANAGED_UNITS {
            assert!(
                !LEGACY_UNITS.contains(&format!("{m}.service").as_str()),
                "{m} 同时出现在受管与遗留列表里"
            );
        }
        assert!(LEGACY_UNITS.contains(&"hysteria-server@.service"));
        assert!(LEGACY_UNITS.contains(&"xray@.service"));
        assert!(LEGACY_UNITS.contains(&"b-ui-admin.service"));
    }
}
```
`Facts::probe` 里 pubkey 计数复用 Task 7 的 `crate::modules::ssh::count_pubkeys`——为避免 Task 4 依赖 Task 7，把这个纯函数放在 `reconcile/mod.rs` 里叫 `count_pubkeys(text: &str) -> u32`，Task 7 的 ssh 模块从这里 import。

`crates/bui/src/api/state.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn bus_broadcasts_to_every_subscriber_and_tolerates_none() {
        let bus = EventBus::new();
        bus.send(Event::StateChanged("nobody-listening"));   // 不能 panic
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.send(Event::ReconcileRequested { force: true });
        assert_eq!(a.recv().await.unwrap(), Event::ReconcileRequested { force: true });
        assert_eq!(b.recv().await.unwrap(), Event::ReconcileRequested { force: true });
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui reconcile::`
Expected: 编译失败（`Artifact` / `plan` 未定义）。

- [ ] **Step 3: 实现 `mod.rs`**

```rust
pub mod apply;
pub mod diff;
pub mod drift;

pub const MANAGED_UNITS: [&str; 6] =
    ["b-ui", "hysteria-server", "hysteria-residential", "xray", "b-ui-relay", "caddy"];
pub const LEGACY_UNITS: [&str; 9] = [
    "hy2-watchdog.timer",
    "hy2-watchdog.service",
    "b-ui-cert-sync.timer",
    "b-ui-cert-sync.service",
    "b-ui-resi-health.timer",
    "b-ui-resi-health.service",
    "b-ui-admin.service",
    "hysteria-server@.service",
    "xray@.service",
];

use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::{CmdOut, Host, Proto};
use anyhow::Result;
use bui_schema::model::State;
use bui_schema::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitAction { Restart, Reload }

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unit { pub name: String, pub action: UnitAction }

impl Unit {
    pub fn restart(name: &str) -> Self { Self { name: name.to_string(), action: UnitAction::Restart } }
    pub fn reload(name: &str) -> Self { Self { name: name.to_string(), action: UnitAction::Reload } }
    pub fn service(&self) -> String {
        if self.name.contains('.') { self.name.clone() } else { format!("{}.service", self.name) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify { Xray, SingBox, Caddy, Sshd }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec { pub proto: Proto, pub from: u16, pub to: u16 }

impl PortSpec {
    pub fn one(proto: Proto, port: u16) -> Self { Self { proto, from: port, to: port } }
    pub fn range(proto: Proto, from: u16, to: u16) -> Self { Self { proto, from, to } }
    /// ufw 语法：`22/tcp` 或 `20000:30000/udp`
    pub fn ufw(&self) -> String {
        let p = if self.proto == Proto::Tcp { "tcp" } else { "udp" };
        if self.from == self.to { format!("{}/{p}", self.from) } else { format!("{}:{}/{p}", self.from, self.to) }
    }
    /// firewalld 语法：`22/tcp` 或 `20000-30000/udp`
    pub fn firewalld(&self) -> String {
        let p = if self.proto == Proto::Tcp { "tcp" } else { "udp" };
        if self.from == self.to { format!("{}/{p}", self.from) } else { format!("{}-{}/{p}", self.from, self.to) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Artifact { /* 见 Interfaces */ }

impl Artifact {
    pub fn file(path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> Self {
        Artifact::File {
            path: path.into(), content: content.into(), mode: 0o600,
            immutable: false, restart: None, restart_key: None, verify: None,
        }
    }
    // mode / restart / restart_key / verify / immutable 都是 `match self { Artifact::File{..} => …, other => other }`
    pub fn id(&self) -> String {
        match self {
            Artifact::File { path, .. } => format!("file:{}", path.display()),
            Artifact::Unit { name, dropin, .. } => match dropin {
                Some(d) => format!("unit:{}:{d}", name.name),
                None => format!("unit:{}", name.name),
            },
            Artifact::UnitState { name, .. } => format!("unitstate:{name}"),
            Artifact::Sysctl { key, .. } => format!("sysctl:{key}"),
            Artifact::Modprobe { module } => format!("modprobe:{module}"),
            Artifact::Binary { name, .. } => format!("binary:{name}"),
            Artifact::Symlink { path, .. } => format!("symlink:{}", path.display()),
            Artifact::FirewallPorts { .. } => "firewall".to_string(),
            Artifact::Absent { path } => format!("absent:{}", path.display()),
        }
    }
    pub fn unit_path(name: &str, dropin: Option<&str>) -> PathBuf {
        let base = PathBuf::from("/etc/systemd/system");
        let unit = if name.contains('.') { name.to_string() } else { format!("{name}.service") };
        match dropin { Some(d) => base.join(format!("{unit}.d")).join(d), None => base.join(unit) }
    }
}

/// 一轮对账开始时探一次的系统事实（render 里不再碰 Host）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts { /* 见 Interfaces */ }

impl Facts {
    pub fn probe(host: &dyn Host) -> Result<Facts> {
        let has_ufw = host.which("ufw");
        let ufw_active = has_ufw
            && host.run("ufw", &["status"]).map(|o| o.stdout.contains("Status: active")).unwrap_or(false);
        let has_firewalld = host.which("firewall-cmd");
        let firewalld_active = has_firewalld && host.unit_is_active("firewalld").unwrap_or(false);
        let ssh_unit = if host.unit_exists("ssh.service").unwrap_or(false) { "ssh" } else { "sshd" };
        let keys = host
            .read_file(std::path::Path::new("/root/.ssh/authorized_keys"))?
            .map(|b| count_pubkeys(&String::from_utf8_lossy(&b)))
            .unwrap_or(0);
        Ok(Facts {
            mem_mb: host.mem_mb()?, arch: host.arch()?, hostname: host.hostname()?,
            has_ufw, ufw_active, has_firewalld, firewalld_active,
            ssh_unit: ssh_unit.to_string(), ssh_pubkeys: keys,
            systemd_resolved: host.unit_is_active("systemd-resolved").unwrap_or(false)
                || host.unit_exists("systemd-resolved.service").unwrap_or(false),
        })
    }
}

/// 非注释行且以 ssh-rsa/ssh-ed25519/ssh-dss/ecdsa-sha2- 开头（移植 core.sh:1871）。
pub fn count_pubkeys(text: &str) -> u32 {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter(|l| {
            l.starts_with("ssh-rsa") || l.starts_with("ssh-ed25519") || l.starts_with("ssh-dss") || l.starts_with("ecdsa-sha2-")
        })
        .count() as u32
}

pub struct RenderCtx { pub paths: Paths, pub facts: Facts }

pub trait Module: Send + Sync { /* 见 Interfaces，routes/spawn 有默认实现 */ }

#[derive(Clone)]
pub struct DaemonCtx { /* 见 Interfaces */ }
```

- [ ] **Step 3b: 实现 `api/state.rs` 与 `api/mod.rs` 的再导出**

```rust
//! 守护进程的共享状态与事件总线；HTTP 实现见 api/auth.rs / health.rs / system.rs（Task 13）。
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use std::sync::Arc;
use time::OffsetDateTime;

/// 进程内事件：state 变更、relay 重启（P3 据此重放上游）、显式请求对账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StateChanged(&'static str),
    RelayRestarted,
    ReconcileRequested { force: bool },
}

#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<Event>);

impl EventBus {
    pub fn new() -> Self {
        Self(tokio::sync::broadcast::channel(64).0)
    }
    /// 没有订阅者时静默丢弃（装机阶段还没起后台任务）。
    pub fn send(&self, e: Event) {
        let _ = self.0.send(e);
    }
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.0.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// axum handler 的共享状态（总纲 C2 的 `AppState`，字段细化）。
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub bus: EventBus,
    pub runtime: Runtime,
    pub host: Arc<dyn Host>,
    pub started_at: OffsetDateTime,
    pub version: &'static str,
}
```
`crates/bui/src/api/mod.rs` 追加一行（保留 Task 1 写的四行 `pub mod`）：
```rust
pub use state::{AppState, Event, EventBus};
```

- [ ] **Step 4: 实现 `diff.rs`**

按 Interfaces 的 9 条规则实现；`firewall` 的 key：
```rust
fn ports_key(ports: &[PortSpec]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in ports { h.update(p.ufw().as_bytes()); h.update(b","); }
    hex::encode(h.finalize())[..16].to_string()
}
```
`Change` 的产出顺序必须与 `artifacts` 的输入顺序一致（测试断言依赖它）。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui reconcile:: api::state && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 13 passed（diff 8 + reconcile::mod 4 + api::state 1）。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/reconcile crates/bui/src/api
git commit -m "feat(bui): Artifact/Module 契约、受管单元常量、AppState/EventBus 与对账 diff"
```

---

### Task 5: 对账应用（验证再重启、失败回滚、漂移只报不改）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/reconcile/apply.rs`, `crates/bui/src/reconcile/drift.rs`
- Test: 同文件

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Change, Plan, Unit, UnitAction, Verify, Facts, MANAGED_UNITS, LEGACY_UNITS}`、`crate::sys::Host`、`crate::state::runtime::DriftItem`、`crate::paths::{verify_dir, v3_backup_dir}`
- Produces:
```rust
// crate::reconcile::apply
pub trait BinaryInstaller: Send + Sync {
    fn install(&self, name: &str, version: &str, sha256: &str, url: &str, dest: &Path) -> anyhow::Result<()>;
}
#[derive(Debug, Default, PartialEq)]
pub struct ApplyOutcome {
    pub changed: Vec<String>, pub restarted: Vec<String>, pub notes: Vec<String>,
    pub verify_failures: Vec<String>, pub errors: Vec<String>,
    /// 本轮**真正落地**的 `restart_key`：从 `plan.keys` 搬过来（第 1 步与第 10 步写明了搬的条件）。
    /// Task 15 的 `reconcile_once` 把它 `extend` 进 `runtime.restart_keys`；不搬 = 下一轮 `keys[id] != k`
    /// 永远成立 = 每次改 `clients` 都重启 xray（spec §3.3 失效），乱搬 = 该重启时不重启。
    pub keys: BTreeMap<String, String>, pub relay_restarted: bool,
    /// `b-ui.service` 自身需要重启；apply **绝不**自己动它（第 12 步），由调用方处理
    pub self_restart_required: bool,
}
pub struct ApplyInput<'a> {
    pub plan: Plan, pub paths: &'a Paths, pub facts: &'a Facts,
    pub installer: &'a dyn BinaryInstaller, pub dry_run: bool,
}
pub fn apply(input: ApplyInput<'_>, host: &dyn Host) -> ApplyOutcome
// crate::reconcile::drift
pub fn scan(host: &dyn Host, artifacts: &[Artifact], paths: &Paths) -> Vec<DriftItem>
pub fn clean(host: &dyn Host, items: &[DriftItem]) -> Vec<String>
/// `<base>` 顶层允许存在、但不是 artifact 的条目（少一项就会每轮报永久漂移）
pub const BASE_WHITELIST: [&str; 13] = [
    "bin", "certs", "caddy", "packages", "state.backups", ".verify", "v3-backup",
    "state.json", "runtime.json", "manifest.json", "relay-cache.db",
    "auth-snapshot.json", "auth-hook.log",
];
/// 只在这四个 `/etc` 目录里找「b-ui 前缀的陌生配置」（`stray_conf`）；其余 `/etc` 一概不扫
pub const MANAGED_CONF_DIRS: [&str; 4] =
    ["/etc/sysctl.d", "/etc/modprobe.d", "/etc/modules-load.d", "/etc/ssh/sshd_config.d"];
```
`MANAGED_UNITS` / `LEGACY_UNITS` **不在这里定义**，直接 `use crate::reconcile::{LEGACY_UNITS, MANAGED_UNITS}`（Task 4 的唯一一份）。
`BASE_WHITELIST` 逐项理由：`bin/`（内核与 bui 二进制，含 `bui.prev`）、`certs/`（Caddy 签的证书副本，导入时保留）、`caddy/`（Caddy 的 XDG 数据目录）、`packages/`（客户端内核缓存，v3 沿用）、`state.backups/`、`.verify/`（渲染结果校验落地）、`v3-backup/`（`uninstall_v3` 归档的 v3 秘密文件，0700）、`state.json`、`runtime.json`、`manifest.json`（install/upgrade/每日自检写）、`relay-cache.db`（sing-box relay 的 `cache_file`，运行时自建）、`auth-snapshot.json`（`bui install` 写初版、P2 的用户模块接手重写；它不是 artifact，不在白名单里就会每轮报一条陌生文件）、`auth-hook.log`（spec §3.2 的钩子自己写在 `<base>` 下；P2 合并后它必然出现，现在就加进来，免得那时每 10 分钟报一条永久 `stray_file`）。

**扫描范围是有意收窄的**（spec §2.2「受管目录里的陌生文件」）：`stray_file` 只看 `<base>` 顶层的直接子项，不递归进 `bin/` `certs/` `caddy/` `packages/`（那些目录的内容由 Caddy、客户端缓存与内核自己生成，递归扫描只会产生噪音）。`/etc` 下则只多扫一层「b-ui 前缀的陌生配置」：`/etc/sysctl.d`、`/etc/modprobe.d`、`/etc/modules-load.d`、`/etc/ssh/sshd_config.d` 四个目录里文件名以 `b-ui` / `00-b-ui` / `99-b-ui` 开头、又不在本轮 artifact 路径集合里的（典型是手工复制的 `99-b-ui-network.conf.bak`），报成 `stray_conf`（见第 5 步）。这四个目录之外的 `/etc` 一概不扫。

**apply 的固定步骤顺序**（测试断言 `ops` 顺序，实现不得改）：
1. `WriteFile`：`verify` 为 `Some` 时先把候选内容写到 `verify_dir/<文件名>`，跑校验命令；失败 → 记 `verify_failures`，**不写目标文件**，继续下一项（spec §2.2「校验失败不写盘」）。通过后：若目标当前 immutable（`host.is_immutable(path)` 为 true）→ 先 `chattr -i`；读旧内容留作回滚；`write_file`；记 `changed`；`restart` 进重启集合；**把 `plan.keys` 里 `file:<path>` 那一条搬进 `outcome.keys`**（`Artifact::id()` 的格式，Task 4）。搬的条件严格是「写盘成功」：校验失败或 `write_file` 报错都不搬——记了就等于告诉下一轮「这个 key 已生效」，下次真写成功时反而不会重启对应单元。
2. `SetImmutable`：`on = true` 时 `set_immutable(path, true)` 之后用 `host.is_immutable(path)` 复核；仍为 false（文件系统不支持 `chattr +i`，overlayfs / 部分 VPS 模板）→ 记一条 `notes`「`<path>` 所在文件系统不支持 chattr +i，已跳过 immutable 位」，不记 `errors`。`on = false` 时只在当前确实是 immutable 时才调（避免多记一条 `chattr:-i:` 流水）。
3. `WriteUnit` → 记 `changed`，置 `need_daemon_reload`，该 unit 进重启集合。
4. `need_daemon_reload` → `systemd_daemon_reload()`（只一次）。
5. `LoadModule` → `host.modprobe(module)`。**必须在 `SetSysctl` 之前**：`net.netfilter.nf_conntrack_*` 三个键在模块未加载时 `sysctl -w` 直接 ENOENT，新装机会每 10 分钟报三条 errors、健康永久 degraded。失败只记 `notes`（有的内核把 conntrack 编进内核、没有可加载模块），不记 `errors`。
6. `SetSysctl`。
7. `InstallBinary` → `installer.install(...)`，成功则该内核对应的单元进重启集合（hysteria→两个 hysteria；xray→xray；sing-box→b-ui-relay；caddy→caddy）。
8. `WriteSymlink` → `host.symlink(target, path)`，记 `changed`。
9. `SetUnitState`：先处理 `enabled=false/active=false`（`disable` + `stop`，停用 v3 遗留单元），再处理启用项（`enable`；**仅当该单元不在本轮重启集合里才 `start`**——本轮刚写过单元文件或配置的单元会在第 11 步 restart，若这里再 start 就变成「start 紧跟 restart」的双启动）。
10. `OpenPorts`：`facts.ufw_active` → `ufw allow <spec.ufw()>` 逐条；否则 `facts.firewalld_active` → `firewall-cmd --permanent --add-port=<spec.firewalld()>` 逐条 + `--reload`；**这两支执行完把 `plan.keys["firewall"]` 搬进 `outcome.keys`**（否则每轮都重复 `ufw allow`）。两者都没有 → 记 `notes`：「未检测到 ufw/firewalld，请在云厂商安全组放行：<端口列表>」，并**不搬** key——机器上什么都没改，记了 key 等于骗下一轮说已放行。
    **「两者都没有」这一支在 P1 的正常路径上不会命中**，它是给 P2/P3 的守卫：Task 6 的 `SystemModule::render` 只在 `facts.ufw_active || facts.firewalld_active` 为真时才产出 `FirewallPorts`，没有防火墙的提示改由 Task 15 的 `reconcile_once` 直接从 `facts` 追加进 `notes`（与 ssh 公钥提示同一套模式）。理由见 Task 4 的 diff 规则 8：若让没有防火墙的机器照样产出 artifact，「不搬 key」就会让 `OpenPorts` 每轮重新入 `changed`，M1 的「二次对账零变更」永远不可达。下面的 `open_ports_without_a_firewall_becomes_a_health_note` 直接构造 `Plan`，所以它验的正是这条守卫。
11. `RemoveFile`：**仅当 `host.is_immutable(path)` 为 true 时**才先 `chattr -i`（无条件调会多出一条 `chattr:-i:<path>` 流水，与 `removals_disable_and_stop_first_then_delete_then_daemon_reload` 的 `ops` 断言冲突），再 `remove_file`；若删的是 `/etc/systemd/system/` 下的文件，置 `need_daemon_reload2` → 末尾再 `daemon-reload` 一次。
12. 重启集合按 `(name, action)` 去重并按 name 排序后逐个执行，**但 `b-ui` 不在这个循环里**：把它从集合里摘出来，只置 `outcome.self_restart_required = true`。其余单元 `Restart` → `systemd("restart", unit)`；`Reload` → `systemd("reload", unit)`。失败 → 把该单元相关文件恢复成旧内容（回滚）并再启一次；仍失败 → 记 `errors`（健康状态降级）。`b-ui-relay` 成功重启 → `relay_restarted = true`（Task 15 据此发 `Event::RelayRestarted`，P3 重放选中上游）。
    **为什么 `b-ui` 必须摘出去**：按 name 排序时 `b-ui` 排第一，而这段代码是在守护进程自己的进程里跑的（Task 15 的 `reconcile_from_ctx`）。只要 `b-ui.service` 内容有差（`bui upgrade` 换了新版、新版模板改了单元或任一内核配置），第一条 `systemctl restart b-ui` 就把自己杀掉——后面 hysteria / xray / relay 的重启、`runtime.json` 的报告与 `restart_keys` 全都不执行；下次启动时文件已经一致，这些内核就永远跑着旧配置。`bui install` 同理：`b-ui` 先起来会立刻开始自己的启动对账，与 install 剩下的 5 条 restart 并发打架。
    `self_restart_required` 的落地在 Task 15：CLI 路径（`install` / `reconcile_cli` / `upgrade`）在报告与 `restart_keys` 落盘之后 `systemctl restart b-ui`；守护进程内的路径用 `systemctl restart --no-block b-ui.service`（systemd 排队、本轮先返回；单元有 `Restart=always`，就算被立即停掉也会被拉起）。
13. `dry_run=true` 时：不调任何写操作，只把每个 `Change` 的描述填进 `changed`。

**校验命令**（`Verify` → 命令）：
- `Xray`：`xray run -test -c <candidate>`（程序取 `paths.bin_dir/xray`，不存在则 PATH 上的 `xray`）
- `SingBox`：`sing-box check -c <candidate>`
- `Caddy`：`caddy validate --config <candidate> --adapter caddyfile`
- `Sshd`：先写目标文件再 `sshd -t`；失败则删除刚写的文件并恢复旧内容（sshd 只能校验全局配置，无法校验孤立文件——与 v3 `core.sh:1929` 同语义）。这是 `Sshd` 的特例，实现里单独一支。
- **校验器根本不存在时（离线装机 / 内核还没下载完）**：`paths.bin_dir/<n>` 没有文件且 `!host.which(n)` → **跳过校验直接写盘**，并记一条 `notes`「校验器 `<n>` 不存在，已跳过 `<file>` 的校验」。理由：这些文件是派生物，内核不存在时对应服务也没在跑，写下来无害；若改成「校验不了就不写」，`bui reconcile` 在 manifest 拉取失败的离线机器上会永远写不出配置（死锁）。`bui install` 的第 4 步先装内核，正常路径不会命中这一支。

- [ ] **Step 1: 写失败测试（apply）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::diff::{Change, Plan};
    use crate::reconcile::{Facts, PortSpec, Unit, Verify};
    use crate::sys::{fake::FakeHost, CmdOut, Host, Proto};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    struct NoopInstaller;
    impl BinaryInstaller for NoopInstaller {
        fn install(&self, _n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> { Ok(()) }
    }
    struct FailingInstaller;
    impl BinaryInstaller for FailingInstaller {
        fn install(&self, n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            anyhow::bail!("{n}: sha256 不匹配")
        }
    }

    fn facts() -> Facts {
        Facts {
            mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
            has_ufw: true, ufw_active: true, has_firewalld: false, firewalld_active: false,
            ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: false,
        }
    }

    fn run(plan: Plan, host: &FakeHost, installer: &dyn BinaryInstaller) -> ApplyOutcome {
        let paths = Paths::default_server();
        apply(ApplyInput { plan, paths: &paths, facts: &facts(), installer, dry_run: false }, host)
    }

    #[test]
    fn writes_file_then_reloads_and_restarts_in_fixed_order() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/config.yaml".into(), content: b"listen: :10000".to_vec(),
                    mode: 0o600, verify: None, restart: Some(Unit::restart("hysteria-server")),
                },
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(), unit: Unit::restart("b-ui"),
                },
            ],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "write:/opt/b-ui/config.yaml:600",
                "write:/etc/systemd/system/b-ui.service:644",
                "daemon-reload",
                "systemd:restart:hysteria-server",
            ],
            "b-ui 不在同步重启序列里（第 12 步）：在守护进程里第一条就会把自己杀掉"
        );
        assert_eq!(out.restarted, vec!["hysteria-server".to_string()]);
        assert!(out.self_restart_required, "改由调用方在报告落盘后重启 b-ui");
        assert!(out.errors.is_empty());
    }

    #[test]
    fn the_daemons_own_unit_is_never_restarted_inside_apply() {
        // 排序后 `b-ui` 排第一；apply 跑在守护进程自己的进程里，restart 自己 = 后面的重启、
        // runtime.json 的报告与 restart_keys 全丢，下次启动时文件已一致 → 内核永远跑旧配置。
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("b-ui"),
                },
                Change::WriteUnit {
                    path: "/etc/systemd/system/xray.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("xray"),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(
            !h.ops().iter().any(|o| o == "systemd:restart:b-ui"),
            "apply 不许重启守护进程自己：{:?}",
            h.ops()
        );
        assert_eq!(out.restarted, vec!["xray".to_string()]);
        assert!(out.self_restart_required);
    }

    #[test]
    fn restart_keys_are_recorded_only_for_the_writes_that_landed() {
        // 校验失败的文件不记 key（否则下次真写成功时该重启的单元不重启）；
        // 写成功的文件与真正调过 ufw 的防火墙才记（否则每轮都重启 xray / 重复 ufw allow）。
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push(("/opt/b-ui/bin/xray run -test".into(), CmdOut::failure(1, "bad inbound")));
        });
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/xray-config.json".into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    verify: Some(Verify::Xray),
                    restart: Some(Unit::restart("xray")),
                },
                Change::WriteFile {
                    path: "/opt/b-ui/config.yaml".into(),
                    content: b"listen: :10000".to_vec(),
                    mode: 0o600,
                    verify: None,
                    restart: Some(Unit::restart("hysteria-server")),
                },
                Change::OpenPorts { ports: vec![PortSpec::one(Proto::Udp, 40000)] },
            ],
            keys: std::collections::BTreeMap::from([
                ("file:/opt/b-ui/xray-config.json".to_string(), "hash-A".to_string()),
                ("file:/opt/b-ui/config.yaml".to_string(), "hash-B".to_string()),
                ("firewall".to_string(), "fw-1".to_string()),
            ]),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            out.keys,
            std::collections::BTreeMap::from([
                ("file:/opt/b-ui/config.yaml".to_string(), "hash-B".to_string()),
                ("firewall".to_string(), "fw-1".to_string()),
            ])
        );
    }

    #[test]
    fn verify_failure_blocks_the_write_and_is_reported() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push(("/opt/b-ui/bin/xray run -test".into(), CmdOut::failure(1, "bad inbound")));
        });
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/xray-config.json".into(), content: b"{}".to_vec(), mode: 0o600,
                verify: Some(Verify::Xray), restart: Some(Unit::restart("xray")),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(h.text("/opt/b-ui/xray-config.json").is_none(), "校验失败不写盘");
        assert_eq!(out.changed, Vec::<String>::new());
        assert_eq!(out.restarted, Vec::<String>::new());
        assert_eq!(out.verify_failures.len(), 1);
        assert!(out.verify_failures[0].contains("bad inbound"));
        assert!(h.ops().iter().any(|o| o.starts_with("write:/opt/b-ui/.verify/xray-config.json")));
    }

    #[test]
    fn restart_failure_restores_the_previous_file_and_retries_once() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"listen: :9999".to_vec(), 0o600));
            i.fail_units.insert("hysteria-server".into());
        });
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(), content: b"listen: :10000".to_vec(),
                mode: 0o600, verify: None, restart: Some(Unit::restart("hysteria-server")),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(h.text("/opt/b-ui/config.yaml").unwrap(), "listen: :9999", "回滚到上一版内容");
        assert_eq!(
            h.ops(),
            vec![
                "write:/opt/b-ui/config.yaml:600",
                "systemd:restart:hysteria-server",
                "write:/opt/b-ui/config.yaml:600",
                "systemd:restart:hysteria-server",
            ]
        );
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("hysteria-server"));
    }

    #[test]
    fn a_missing_verifier_skips_verification_and_still_writes() {
        // 离线装机：bin/ 里还没有 xray，PATH 上也没有。派生文件照写，只记一条提示。
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/xray-config.json".into(), content: b"{}".to_vec(), mode: 0o600,
                verify: Some(Verify::Xray), restart: Some(Unit::restart("xray")),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(h.text("/opt/b-ui/xray-config.json").as_deref(), Some("{}"));
        assert!(out.verify_failures.is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("xray") && out.notes[0].contains("跳过"));
    }

    #[test]
    fn modules_are_loaded_before_sysctl_and_failures_are_only_notes() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::LoadModule { module: "nf_conntrack".into() },
                Change::SetSysctl { key: "net.netfilter.nf_conntrack_max".into(), value: "131072".into() },
            ],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec!["modprobe:nf_conntrack", "sysctl:net.netfilter.nf_conntrack_max=131072"],
            "先 modprobe 再 sysctl，否则 net.netfilter.* 全 ENOENT"
        );
        assert!(out.errors.is_empty());
        assert_eq!(out.changed, vec!["nf_conntrack".to_string(), "net.netfilter.nf_conntrack_max".to_string()]);
    }

    #[test]
    fn symlinks_are_written() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteSymlink {
                path: "/usr/local/bin/b-ui".into(),
                target: "/opt/b-ui/bin/bui".into(),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(h.ops(), vec!["symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui"]);
        assert_eq!(out.changed, vec!["/usr/local/bin/b-ui".to_string()]);
    }

    #[test]
    fn a_freshly_written_unit_is_enabled_then_restarted_once_not_started_twice() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/xray.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("xray"),
                },
                Change::SetUnitState { unit: "xray".into(), enabled: true, active: true },
            ],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "write:/etc/systemd/system/xray.service:644",
                "daemon-reload",
                "systemd:enable:xray",
                "systemd:restart:xray",
            ],
            "本轮要 restart 的单元不再额外 start（首装时会变成 start 后紧跟 restart 的双启动）"
        );
        assert_eq!(out.restarted, vec!["xray".to_string()]);
    }

    #[test]
    fn an_already_written_unit_that_is_down_gets_started() {
        let h = FakeHost::new();
        h.with(|i| i.files.insert("/etc/systemd/system/xray.service".into(), (b"[Service]\n".to_vec(), 0o644)));
        let plan = Plan {
            changes: vec![Change::SetUnitState { unit: "xray".into(), enabled: true, active: true }],
            keys: Default::default(), unchanged: 1,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(h.ops(), vec!["systemd:enable:xray", "systemd:start:xray"]);
    }

    #[test]
    fn relay_restart_is_flagged_for_upstream_replay() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/singbox-relay.json".into(), content: b"{}".to_vec(), mode: 0o600,
                verify: None, restart: Some(Unit::restart("b-ui-relay")),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(out.relay_restarted, "relay 重启后要重放 selected_upstream_id");
    }

    #[test]
    fn caddyfile_change_reloads_instead_of_restarting() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/Caddyfile".into(), content: b"example.com {}\n".to_vec(), mode: 0o644,
                verify: None, restart: Some(Unit::reload("caddy")),
            }],
            keys: Default::default(), unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert!(h.ops().contains(&"systemd:reload:caddy".to_string()));
        assert!(!h.ops().iter().any(|o| o == "systemd:restart:caddy"));
    }

    #[test]
    fn binary_install_failure_is_an_error_and_skips_the_restart() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::InstallBinary {
                name: "sing-box".into(), version: "1.13.19".into(), sha256: "bb".into(),
                url: "https://x/z".into(), path: "/opt/b-ui/bin/sing-box".into(),
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = run(plan, &h, &FailingInstaller);
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("sha256"));
        assert!(out.restarted.is_empty());
    }

    #[test]
    fn open_ports_uses_ufw_when_active() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::OpenPorts {
                ports: vec![PortSpec::one(Proto::Tcp, 22), PortSpec::range(Proto::Udp, 41000, 50000)],
            }],
            keys: Default::default(), unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(h.ops(), vec!["run:ufw allow 22/tcp", "run:ufw allow 41000:50000/udp"]);
    }

    /// 守卫分支（第 10 步）：`SystemModule` 在没有活防火墙时根本不产出 `FirewallPorts`，
    /// 所以这条测试直接构造 `Plan`。它保证 P2/P3 将来自己产出这个 artifact 时不会静默失败。
    #[test]
    fn open_ports_without_a_firewall_becomes_a_health_note() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let mut f = facts();
        f.has_ufw = false;
        f.ufw_active = false;
        let plan = Plan {
            changes: vec![Change::OpenPorts { ports: vec![PortSpec::one(Proto::Udp, 40000)] }],
            keys: Default::default(), unchanged: 0,
        };
        let out = apply(ApplyInput { plan, paths: &paths, facts: &f, installer: &NoopInstaller, dry_run: false }, &h);
        assert!(h.ops().is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("40000/udp"));
        assert!(out.keys.is_empty(), "什么都没改就不记 firewall key，下一轮还要再提醒一次");
    }

    /// 顺序：先 disable+stop、再删文件、最后一次 `daemon-reload`（第 11 步的 `need_daemon_reload2`）。
    /// `chattr -i` 只在文件当前确实是 immutable 时才调，所以这里的 ops 里没有 `chattr:-i:` 一行。
    #[test]
    fn removals_disable_and_stop_first_then_delete_then_daemon_reload() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/etc/systemd/system/hy2-watchdog.timer".into(), (b"x".to_vec(), 0o644));
            i.units_enabled.insert("hy2-watchdog.timer".into());
            i.units_active.insert("hy2-watchdog.timer".into());
        });
        let plan = Plan {
            changes: vec![
                Change::SetUnitState { unit: "hy2-watchdog.timer".into(), enabled: false, active: false },
                Change::RemoveFile { path: "/etc/systemd/system/hy2-watchdog.timer".into() },
            ],
            keys: Default::default(), unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "systemd:disable:hy2-watchdog.timer",
                "systemd:stop:hy2-watchdog.timer",
                "remove:/etc/systemd/system/hy2-watchdog.timer",
                "daemon-reload",
            ]
        );
    }

    #[test]
    fn dry_run_touches_nothing() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(), content: b"x".to_vec(), mode: 0o600, verify: None, restart: None,
            }],
            keys: Default::default(), unchanged: 0,
        };
        let out = apply(
            ApplyInput { plan, paths: &paths, facts: &facts(), installer: &NoopInstaller, dry_run: true },
            &h,
        );
        assert!(h.ops().is_empty());
        assert_eq!(out.changed, vec!["/opt/b-ui/config.yaml".to_string()]);
    }
}
```

- [ ] **Step 2: 写失败测试（drift）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::Artifact;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn managed() -> Vec<Artifact> {
        vec![
            Artifact::file("/opt/b-ui/config.yaml", "a"),
            // Caddyfile 按 C3 就在 <base> 顶层：它是 artifact，所以不该被报成陌生文件
            Artifact::file("/opt/b-ui/Caddyfile", "example.com {}\n").mode(0o644),
            Artifact::file("/etc/resolv.conf", "b").mode(0o644).immutable(),
            Artifact::Unit { name: crate::reconcile::Unit::restart("xray"), dropin: None, content: "x".into() },
        ]
    }

    #[test]
    fn reports_foreign_dropins_legacy_units_stray_files_and_cron() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.files.insert("/etc/systemd/system/xray.service".into(), (b"x".to_vec(), 0o644));
            i.files.insert("/etc/systemd/system/xray.service.d/50-manual.conf".into(), (b"y".to_vec(), 0o644));
            i.files.insert("/etc/systemd/system/hy2-watchdog.timer".into(), (b"z".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/stray-note.txt".into(), (b"?".to_vec(), 0o644));
            // chattr/lsattr 都在 → 才有资格报 resolv_immutable（见 Step 5 的降级口径）
            i.which.insert("chattr".into());
            i.which.insert("lsattr".into());
            i.scripted.push(("crontab -l".into(), CmdOut::success("0 */6 * * * /opt/b-ui/update.sh auto\n")));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds, vec!["unit_dropin", "legacy_unit", "stray_file", "cron", "resolv_immutable"]);
        assert_eq!(items[0].path, "/etc/systemd/system/xray.service.d/50-manual.conf");
        assert_eq!(items[1].path, "/etc/systemd/system/hy2-watchdog.timer");
        assert_eq!(items[2].path, "/opt/b-ui/stray-note.txt");
        assert!(items[3].detail.contains("update.sh"));
        assert!(items[4].detail.contains("immutable"));
        assert!(h.ops().iter().all(|o| o.starts_with("run:")), "scan 只读，不改任何东西");
    }

    #[test]
    fn a_clean_host_has_no_drift() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert("/etc/systemd/system/xray.service".into(), (b"x".to_vec(), 0o644));
            i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root")));
        });
        assert_eq!(scan(&h, &managed(), &Paths::default_server()), vec![]);
    }

    #[test]
    fn every_file_a_healthy_v4_install_has_is_whitelisted() {
        // 少一项白名单就会每 10 分钟报一条永久漂移、M1 的「体检无漂移」不可达
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/Caddyfile".into(), (b"example.com {}\n".to_vec(), 0o644));
            i.files.insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert("/etc/systemd/system/xray.service".into(), (b"x".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/auth-snapshot.json".into(), (br#"{"schema":1,"users":{}}"#.to_vec(), 0o600));
            // P2 的钩子自己写的日志（spec §3.2）：白名单里没有它就会每 10 分钟报一条永久漂移
            i.files.insert("/opt/b-ui/auth-hook.log".into(), (b"".to_vec(), 0o600));
            // 对账器与运行时自己造的东西
            i.files.insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/runtime.json".into(), (b"{}".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/manifest.json".into(), (b"{}".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/relay-cache.db".into(), (b"sqlite".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/state.backups/state-20260911T000000Z.json".into(), (b"{}".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert("/opt/b-ui/bin/bui.prev".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert("/opt/b-ui/certs/fullchain.pem".into(), (b"CERT".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/caddy/caddy/certificates/x/y.crt".into(), (b"CERT".to_vec(), 0o600));
            i.files.insert("/opt/b-ui/packages/versions.json".into(), (b"{}".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/.verify/xray-config.json".into(), (b"{}".to_vec(), 0o600));
            // import-v3 归档的 v3 秘密文件
            i.files.insert("/opt/b-ui/v3-backup/users.json".into(), (b"[]".to_vec(), 0o600));
            i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root")));
        });
        assert_eq!(scan(&h, &managed(), &Paths::default_server()), vec![], "健康装机必须零漂移");
    }

    #[test]
    fn a_b_ui_prefixed_backup_under_etc_is_reported_as_stray_conf() {
        // spec §2.2「受管目录里的陌生文件」：四个 /etc 目录只看 b-ui 前缀的文件名
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert("/etc/systemd/system/xray.service".into(), (b"x".to_vec(), 0o644));
            // 手工复制出来的备份：不是 artifact，但前缀属于我们
            i.files.insert("/etc/sysctl.d/99-b-ui-network.conf.bak".into(), (b"x".to_vec(), 0o644));
            // 别人的文件：一律不管
            i.files.insert("/etc/sysctl.d/60-cloudimg.conf".into(), (b"x".to_vec(), 0o644));
            i.which.insert("chattr".into());
            i.which.insert("lsattr".into());
            i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root")));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].kind, "stray_conf");
        assert_eq!(items[0].path, "/etc/sysctl.d/99-b-ui-network.conf.bak");
    }

    #[test]
    fn a_stray_directory_is_reported_and_cleaned_recursively() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert("/etc/systemd/system/xray.service".into(), (b"x".to_vec(), 0o644));
            i.files.insert("/opt/b-ui/admin/node_modules/x/index.js".into(), (b"x".to_vec(), 0o644));
            i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root")));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "stray_file");
        assert_eq!(items[0].path, "/opt/b-ui/admin");
        assert!(items[0].detail.contains("目录"));
        h.clear_ops();
        clean(&h, &items);
        assert_eq!(h.ops(), vec!["rmdir:/opt/b-ui/admin"]);
        assert!(h.text("/opt/b-ui/admin/node_modules/x/index.js").is_none());
    }

    #[test]
    fn clean_removes_everything_but_cron() {
        let h = FakeHost::new();
        let items = vec![
            DriftItem { kind: "unit_dropin".into(), path: "/etc/systemd/system/xray.service.d/50-manual.conf".into(), detail: String::new() },
            DriftItem { kind: "legacy_unit".into(), path: "/etc/systemd/system/hy2-watchdog.timer".into(), detail: String::new() },
            DriftItem { kind: "stray_file".into(), path: "/opt/b-ui/stray-note.txt".into(), detail: String::new() },
            DriftItem { kind: "cron".into(), path: "crontab".into(), detail: "0 */6 * * * /opt/b-ui/update.sh auto".into() },
        ];
        let done = clean(&h, &items);
        assert_eq!(done.len(), 3, "cron 行只报不删（删别人的 cron 太危险）");
        assert_eq!(
            h.ops(),
            vec![
                "remove:/etc/systemd/system/xray.service.d/50-manual.conf",
                "systemd:disable:hy2-watchdog.timer",
                "systemd:stop:hy2-watchdog.timer",
                "remove:/etc/systemd/system/hy2-watchdog.timer",
                "remove:/opt/b-ui/stray-note.txt",
                "daemon-reload",
            ]
        );
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p bui reconcile::apply reconcile::drift`
Expected: 编译失败（`apply` / `scan` 未定义）。

- [ ] **Step 4: 实现 `apply.rs`**

按上面的 13 步顺序实现。`changed` 每项填什么（`dry_run` 与非 dry_run 一致，Task 15 的报告断言依赖它）：`WriteFile`/`RemoveFile`/`WriteSymlink` 填路径；`WriteUnit` 填单元文件路径；`SetUnitState` 填 `unit`；`SetSysctl` 填 `key`；`LoadModule` 填 `module`；`InstallBinary` 填 `"<name> <version>"`；`OpenPorts` 填 `"firewall: <逗号分隔的端口>"`；`SetImmutable` 填路径。

关键片段：
```rust
/// 返回 Ok(None)=校验通过；Ok(Some(note))=校验器不存在已跳过；Err(msg)=校验失败（不写盘）
fn verify_candidate(
    host: &dyn Host,
    paths: &Paths,
    path: &Path,
    content: &[u8],
    v: Verify,
) -> Result<Option<String>, String> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("candidate");
    let program = match v {
        Verify::Xray => "xray",
        Verify::SingBox => "sing-box",
        Verify::Caddy => "caddy",
        Verify::Sshd => return Ok(None), // sshd 只能校验全局配置，见下面的特例分支
    };
    // 校验器不存在（离线装机、内核还没下载）→ 跳过校验直接写盘，只记提示
    let bundled = paths.bin_dir.join(program);
    let bin = if host.read_file(&bundled).map(|o| o.is_some()).unwrap_or(false) {
        bundled.display().to_string()
    } else if host.which(program) {
        program.to_string()
    } else {
        return Ok(Some(format!("校验器 {program} 不存在，已跳过 {} 的校验", path.display())));
    };
    let candidate = crate::paths::verify_dir(paths).join(name);
    host.write_file(&candidate, content, 0o600).map_err(|e| e.to_string())?;
    let cand = candidate.display().to_string();
    let out = match v {
        Verify::Xray => host.run(&bin, &["run", "-test", "-c", &cand]),
        Verify::SingBox => host.run(&bin, &["check", "-c", &cand]),
        Verify::Caddy => host.run(&bin, &["validate", "--config", &cand, "--adapter", "caddyfile"]),
        Verify::Sshd => unreachable!("Sshd 已在上面提前返回"),
    };
    let _ = host.remove_file(&candidate);
    match out {
        Ok(o) if o.ok() => Ok(None),
        Ok(o) => Err(format!(
            "{} 校验失败：{}",
            path.display(),
            if o.stderr.is_empty() { o.stdout } else { o.stderr }.trim()
        )),
        Err(e) => Err(format!("{} 校验无法执行：{e}", path.display())),
    }
}
```
`Verify::Sshd` 特例：写目标文件 → `host.run("sshd", &["-t"])` → 失败则恢复旧内容（或删除新建的文件）并记 `verify_failures`。
内核名 → 单元映射：
```rust
fn units_for_binary(name: &str) -> Vec<Unit> {
    match name {
        "hysteria" => vec![Unit::restart("hysteria-server"), Unit::restart("hysteria-residential")],
        "xray" => vec![Unit::restart("xray")],
        "sing-box" => vec![Unit::restart("b-ui-relay")],
        "caddy" => vec![Unit::restart("caddy")],
        _ => vec![],
    }
}
```
`WriteUnit` 的 mode 固定 0o644；`notes` 文案：`format!("未检测到 ufw/firewalld，请在云厂商安全组放行：{}", specs.join(", "))`，其中每项用 `PortSpec::ufw()` 的写法（`40000/udp`）。

- [ ] **Step 5: 实现 `drift.rs`**

`scan` 的检查顺序固定为：`unit_dropin` → `legacy_unit` → `stray_file` → `stray_conf` → `cron` → `resolv_immutable`；常量用 `crate::reconcile::{LEGACY_UNITS, MANAGED_UNITS}` 与本文件的 `BASE_WHITELIST`、`MANAGED_CONF_DIRS`（见 Interfaces）。
- `unit_dropin`：对 `MANAGED_UNITS` 的每个 `<unit>.service.d/` 目录 `list_dir`，凡文件名不在本轮 `Artifact::Unit` 的 dropin 集合里 → 漂移。
- `legacy_unit`：`LEGACY_UNITS` 里在 `/etc/systemd/system/` 下存在的 → 漂移（正常路径上 units 模块已产出 `Absent` 把它们删了，这里是兜底：手工放回来的会被报出来）。
- `stray_file`：`list_dir(paths.base_dir)`（只含直接子项）里既不在本轮 artifact 的路径集合、也不在 `BASE_WHITELIST`、也不以 `.tmp` / `.new` 结尾的 → 漂移；`host.is_dir(e)` 为 true 时 `detail` 写「陌生目录」，否则写「陌生文件」。
  注意 `list_dir` 只返回直接子项，所以 `bin/` 里的四个内核二进制不会各报一条——`bin` 本身在白名单里；也不递归进 `caddy/` `certs/` `packages/`（见 Interfaces 的「扫描范围是有意收窄的」）。
- `stray_conf`：对 `MANAGED_CONF_DIRS`（`/etc/sysctl.d`、`/etc/modprobe.d`、`/etc/modules-load.d`、`/etc/ssh/sshd_config.d`）逐个 `list_dir`，文件名以 `b-ui` / `00-b-ui` / `99-b-ui` 开头、又不在本轮 artifact 路径集合里的 → 漂移（`detail` 写「受管目录里的陌生 b-ui 配置」）。别人的文件（`60-cloudimg.conf`、`50-cloud-init.conf`）一律不看。`clean` 里按普通文件删。
- `cron`：`run("crontab", &["-l"])` 成功时，含 `/opt/b-ui` 的行 → 每行一个漂移（`path: "crontab"`）。
- **`/tmp` 不在漂移扫描范围内**（有意为之，别加）：v3 的 `/tmp/hy2-watchdog-*` 计数文件只在 `import-v3` 路径的 `uninstall_v3` 里删（Task 16）。手工卸载过 v3、再装 v4 的机器上可能残留几个，但 `/tmp` 在所有目标发行版上都是 tmpfs，重启即清；把它纳入扫描只会给「重启前」这段时间制造一条无法修复的永久 `degraded`（漂移非空 → `/api/health` 降级，Task 13），却换不到任何安全收益——那些文件既不被 v4 读也不被 v4 写。Task 18 的「已知降级」表记了这一行。
- `resolv_immutable`：本轮存在 `/etc/resolv.conf` 的 File artifact 且 `immutable=true`，但 `host.is_immutable` 为 false（或反之）→ 漂移，**但只在 `host.which("chattr") && host.which("lsattr")` 都为真时才报**。
  **`chattr` 降级口径（写死，别让实现者自己发挥）**：`drift` 非空会让 `/api/health` 变 `degraded`（Task 13），而 `RealHost::set_immutable` 在失败时只 `warn!` 返回 `Ok(())`，所以没有这条判定的机器会永久 degraded、M1 的「体检无漂移」不可达。分三种情况：① 容器 / 精简镜像**没有** `chattr` 或 `lsattr` → 不报漂移、不当错误（apply 第 2 步会记一条 note），`/api/health` 保持 `ok`；② 有 `chattr` 但文件系统不支持（overlayfs、部分 VPS 模板）→ apply 每轮记一条 note、`resolv_immutable` 每轮报一条，这类机器把 `state.system.static_dns` 置 false（面板或 `state.json`）即彻底关掉这条期望（静态 DNS 与 immutable 位一起不再要求），M1 验收按 Task 18 清单的「已知降级」一栏记录；③ 正常机器照报照修。
`clean`：`unit_dropin` 与 `stray_conf` 直接 `remove_file`；`stray_file` 按 `detail`（或现场再问一次 `host.is_dir`）选 `remove_dir_all` / `remove_file`；`legacy_unit` 先 `systemd disable` + `stop` 再删；`cron` 跳过（删别人的 cron 行太危险）；末尾若删过 `/etc/systemd/system` 下的东西 → `daemon-reload`。

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui reconcile:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 35 passed（Task 4 的 12 = diff 8 + reconcile::mod 4，加 apply 17 + drift 6；`api::state` 的 1 条不在这个过滤器里）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/reconcile
git commit -m "feat(bui): 对账应用（验证再重启、重启失败回滚、漂移只报不改）"
```

---

### Task 6: 系统模块（sysctl / conntrack / 静态 DNS / 防火墙）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/system.rs`（`modules/mod.rs` 与 `main.rs` 的 `mod modules;` 已由 Task 1 写全，本任务不动）

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Facts, Module, PortSpec, RenderCtx}`、`crate::sys::Proto`、`bui_schema::model::{Ports, State}`
- Produces:
```rust
pub struct SystemModule;
impl Module for SystemModule { fn name(&self) -> &'static str { "system" } /* render */ }
/// 按物理内存分档（与 v3 core.sh:1504-1508 同值）
pub fn conntrack_max(mem_mb: u64) -> u32;
/// ip_local_reserved_ports 的值（v3 core.sh:1485 的硬编码在 v4 从 state 推导）
pub fn reserved_ports(ports: &Ports) -> String;
/// 防火墙要放行的端口集（v3 core.sh:1237-1283 + update.sh 块 E）。
/// `render` 只在机器上确实有活防火墙时把它包成 `Artifact::FirewallPorts`；
/// Task 15 的 `reconcile_once` 在没有防火墙时也调它，用来拼「请在云厂商安全组放行」的提示，
/// 所以它是 `pub`。
pub fn firewall_ports(ports: &Ports) -> Vec<PortSpec>;
pub const RESOLV_CONF: &str = "/etc/resolv.conf";
/// conntrack 的 sysctl 键必须在模块加载后才存在（spec §2.2 的「nf_conntrack 模块」）
pub const CONNTRACK_MODULE: &str = "nf_conntrack";
/// 小内存机器（≤2G）的 swappiness 文件：**与 v3 同名**（v3 `server/core.sh:283-288`、
/// `server/update.sh:987-996` 都写这一个），>2G 时产出 `Absent` 删掉它
pub const MEMORY_CONF: &str = "/etc/sysctl.d/99-b-ui-memory.conf";
```
移植参照：`server/core.sh:1443-1521`（三个 sysctl 文件 + conntrack 分档 + modprobe + BBR）、`server/core.sh:283-288` 与 `server/update.sh:987-996`（≤2G 机器的 `/etc/sysctl.d/99-b-ui-memory.conf`，内容 `vm.swappiness = 10`）、`server/core.sh:1237-1283`（`configure_firewall`，审计判为「删除，但 v4 装机路径要**新增**真正的开端口调用」）、`server/update.sh:1738-1752`（块 D 静态 DNS，审计判为「重新落位到装机路径」）、`server/update.sh:1754-1767`（块 E 防火墙端口）。v4 的变化：三个 sysctl 文件合并成 `99-b-ui-network.conf` + `99-b-ui-conntrack.conf`（不再写 `99-hysteria-perf.conf`，也不再写 `hysteria-server.service.d/priority.conf` 的 RT 优先级——单元里统一 `Nice=-5`，见 Task 8）；每个 sysctl 键额外产出一条 `Artifact::Sysctl` 以便立即生效（不再 `sysctl --system`）。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx};
    use crate::sys::Proto;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn ctx(mem_mb: u64, ufw: bool, firewalld: bool, resolved: bool) -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb, arch: "x86_64".into(), hostname: "node-a".into(),
                has_ufw: ufw, ufw_active: ufw, has_firewalld: firewalld, firewalld_active: firewalld,
                ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: resolved,
            },
        }
    }

    fn files(arts: &[Artifact]) -> Vec<String> {
        arts.iter()
            .filter_map(|a| match a {
                Artifact::File { path, .. } => Some(path.display().to_string()),
                _ => None,
            })
            .collect()
    }

    fn file_text(arts: &[Artifact], want: &str) -> String {
        arts.iter()
            .find_map(|a| match a {
                Artifact::File { path, content, .. } if path.to_str() == Some(want) => {
                    Some(String::from_utf8(content.clone()).unwrap())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("没有渲染 {want}"))
    }

    #[test]
    fn conntrack_tiers_match_v3() {
        assert_eq!(conntrack_max(1024), 131072);
        assert_eq!(conntrack_max(2048), 131072);
        assert_eq!(conntrack_max(3072), 262144);
        assert_eq!(conntrack_max(4096), 262144);
        assert_eq!(conntrack_max(8192), 524288);
    }

    #[test]
    fn reserved_ports_are_derived_from_state() {
        let s = sample_state();
        assert_eq!(reserved_ports(&s.node.ports), "10000-10002,20000-30000,40000,41000-50000");
        let mut p = s.node.ports.clone();
        p.hy2_hop = None;
        assert_eq!(reserved_ports(&p), "10000-10002,40000,41000-50000");
    }

    #[test]
    fn firewall_ports_cover_every_listener_and_hop_range() {
        let s = sample_state();
        let want = vec![
            PortSpec::one(Proto::Tcp, 22),
            PortSpec::one(Proto::Tcp, 80),
            PortSpec::one(Proto::Tcp, 443),
            PortSpec::one(Proto::Tcp, 10001),
            PortSpec::one(Proto::Tcp, 10002),
            PortSpec::one(Proto::Udp, 10000),
            PortSpec::range(Proto::Udp, 20000, 30000),
            PortSpec::one(Proto::Udp, 40000),
            PortSpec::range(Proto::Udp, 41000, 50000),
        ];
        assert_eq!(firewall_ports(&s.node.ports), want);
    }

    #[test]
    fn renders_conf_files_plus_one_live_sysctl_per_key() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert_eq!(
            files(&arts),
            vec![
                "/etc/sysctl.d/99-b-ui-network.conf",
                "/etc/sysctl.d/99-b-ui-conntrack.conf",
                "/etc/sysctl.d/99-b-ui-memory.conf",
                "/etc/modprobe.d/b-ui-nf_conntrack.conf",
                "/etc/modules-load.d/b-ui-conntrack.conf",
                "/etc/resolv.conf",
            ]
        );
        let net = file_text(&arts, "/etc/sysctl.d/99-b-ui-network.conf");
        assert!(net.contains("net.ipv4.tcp_congestion_control=bbr"));
        assert!(net.contains("net.core.default_qdisc=fq"));
        assert!(net.contains("net.ipv4.ip_local_reserved_ports=10000-10002,20000-30000,40000,41000-50000"));
        assert!(net.contains("net.core.rmem_max=16777216"));
        assert!(net.contains("net.ipv4.tcp_retries2=8"));
        assert!(net.contains("net.ipv4.tcp_rmem=4096 262144 16777216"));
        let ct = file_text(&arts, "/etc/sysctl.d/99-b-ui-conntrack.conf");
        assert!(ct.contains("net.netfilter.nf_conntrack_max=131072"));
        assert!(ct.contains("net.netfilter.nf_conntrack_udp_timeout=20"));
        assert!(file_text(&arts, "/etc/modprobe.d/b-ui-nf_conntrack.conf").contains("hashsize=32768"));
        assert_eq!(file_text(&arts, "/etc/modules-load.d/b-ui-conntrack.conf"), "nf_conntrack\n");
        let keys: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Sysctl { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"net.ipv4.tcp_congestion_control".to_string()));
        assert!(keys.contains(&"net.netfilter.nf_conntrack_max".to_string()));
        let conf_keys = |t: &str| t.lines().filter(|l| !l.trim_start().starts_with('#') && l.contains('=')).count();
        // ctx 的 mem_mb=2048（≤2G）→ 还多渲染一个 99-b-ui-memory.conf，它的键也要有 Sysctl
        let mem = file_text(&arts, MEMORY_CONF);
        assert_eq!(
            keys.len(),
            conf_keys(&net) + conf_keys(&ct) + conf_keys(&mem),
            "每个 conf 里的键都要有一条 Sysctl artifact"
        );
    }

    #[test]
    fn small_memory_hosts_keep_the_v3_swappiness_conf() {
        // v3 在 ≤2G 机器上写 /etc/sysctl.d/99-b-ui-memory.conf（`server/core.sh:283-288`、
        // `server/update.sh:987-996`，内容 `vm.swappiness = 10`）。v4 不渲染同名文件的话，
        // `bui install --import-v3` 之后这个文件既不在本轮 artifact 的路径集合里、
        // 名字又带 `99-b-ui` 前缀 → Task 5 的 `stray_conf` 每 10 分钟报一条、
        // `/api/health` 恒 degraded，M1 的「体检无漂移」不可达。spec §3.4「与 v3 同值」。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(files(&arts).contains(&MEMORY_CONF.to_string()));
        assert!(file_text(&arts, MEMORY_CONF).contains("vm.swappiness=10"));
        assert!(arts.contains(&Artifact::Sysctl { key: "vm.swappiness".into(), value: "10".into() }));
        // 1024MB 也是同一档
        let arts = SystemModule.render(&s, &ctx(1024, true, false, false));
        assert!(files(&arts).contains(&MEMORY_CONF.to_string()));
    }

    #[test]
    fn big_memory_hosts_get_an_absent_for_it_instead_of_a_stray_conf() {
        // >2G：v3 本来就不写这个文件，v4 也不要它——但机器可能是从 2G 升配上来的（v3 写过），
        // 所以要产出 `Absent` 让对账删掉，而不是放着不管（放着就是一条永久 `stray_conf`，
        // 且 swappiness 被钉在 10）。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(4096, true, false, false));
        assert!(!files(&arts).contains(&MEMORY_CONF.to_string()));
        assert!(arts.contains(&Artifact::Absent { path: MEMORY_CONF.into() }));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Sysctl { key, .. } if key == "vm.swappiness")));
    }

    #[test]
    fn conntrack_module_is_loaded_now_not_only_on_next_boot() {
        // modules-load.d 只管下次开机；不产出 Modprobe 的话首装当轮
        // `sysctl -w net.netfilter.nf_conntrack_max` 直接 ENOENT，健康永久 degraded
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        let modprobe_at = arts
            .iter()
            .position(|a| a == &Artifact::Modprobe { module: CONNTRACK_MODULE.into() })
            .expect("必须产出 Modprobe{nf_conntrack}");
        let first_sysctl = arts.iter().position(|a| matches!(a, Artifact::Sysctl { .. })).unwrap();
        assert!(
            modprobe_at < first_sysctl,
            "render 里也把 Modprobe 排在 Sysctl 前（apply 的第 5/6 步已保证执行顺序，这里保证 dry-run 报告的可读顺序一致）"
        );
    }

    #[test]
    fn resolv_conf_is_immutable_0644_and_disables_systemd_resolved() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, true));
        let resolv = arts
            .iter()
            .find(|a| matches!(a, Artifact::File { path, .. } if path.to_str() == Some(RESOLV_CONF)))
            .unwrap();
        match resolv {
            Artifact::File { mode, immutable, content, restart, .. } => {
                assert_eq!(*mode, 0o644);
                assert!(*immutable);
                assert_eq!(*restart, None);
                let text = String::from_utf8(content.clone()).unwrap();
                assert!(text.contains("nameserver 1.1.1.1"));
                assert!(text.contains("nameserver 8.8.8.8"));
                assert!(text.contains("options edns0 timeout:2 attempts:2 single-request"));
            }
            other => panic!("{other:?}"),
        }
        assert!(arts.contains(&Artifact::UnitState {
            name: "systemd-resolved".into(),
            enabled: false,
            active: false
        }));
    }

    #[test]
    fn static_dns_off_drops_resolv_conf_and_the_unit_state() {
        let mut s = sample_state();
        s.system.static_dns = false;
        let arts = SystemModule.render(&s, &ctx(2048, true, false, true));
        assert!(!files(&arts).contains(&RESOLV_CONF.to_string()));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::UnitState { name, .. } if name == "systemd-resolved")));
    }

    #[test]
    fn sysctl_profile_off_drops_every_sysctl_and_conf_file() {
        let mut s = sample_state();
        s.system.sysctl_profile = "off".into();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Sysctl { .. })));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Modprobe { .. })));
        assert_eq!(files(&arts), vec!["/etc/resolv.conf"]);
        // `99-b-ui-memory.conf` 的两支都在 `sysctl_profile != "off"` 里面：关了就连 Absent 也不产出
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Absent { .. })));
    }

    #[test]
    fn firewall_off_drops_the_artifact() {
        let mut s = sample_state();
        s.system.firewall = "off".into();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(!arts.iter().any(|a| matches!(a, Artifact::FirewallPorts { .. })));
    }

    #[test]
    fn firewall_artifact_is_emitted_when_ufw_is_active() {
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, true, false, false));
        assert!(arts.iter().any(|a| matches!(a, Artifact::FirewallPorts { .. })));
        // firewalld 单独在也算
        let arts = SystemModule.render(&s, &ctx(2048, false, true, false));
        assert!(arts.iter().any(|a| matches!(a, Artifact::FirewallPorts { .. })));
    }

    #[test]
    fn no_firewall_means_no_artifact_only_a_reconcile_note() {
        // spec §2.2「防火墙：没装就在体检里提示」——提示走 `notes`，**不能**走 artifact。
        // 产出 artifact 的话 apply 什么都改不了、不搬 `firewall` key，diff 规则 8 就会每轮
        // 重新产出一条 `OpenPorts`：`changed` 永远非空，M1 的「二次对账零变更」不可达
        // （第三轮审查 C1）。提示的生成点在 `serve::reconcile_once`，那里有一条
        // `a_host_without_a_firewall_gets_a_note_and_still_reconciles_clean` 覆盖。
        let s = sample_state();
        let arts = SystemModule.render(&s, &ctx(2048, false, false, false));
        assert!(
            !arts.iter().any(|a| matches!(a, Artifact::FirewallPorts { .. })),
            "没有活防火墙时不许产出 FirewallPorts"
        );
        // 但端口集本身照样可算——提示文案要用它
        assert_eq!(firewall_ports(&s.node.ports).len(), 9);
    }

    #[test]
    fn v4_sysctl_values_match_the_v3_block_verbatim() {
        // spec §3.4「sysctl … 与 v3 同值」：不自证，直接**只读**比对仓库里的 v3 实现
        // （`server/core.sh` 的三个 sysctl heredoc + BBR 那两行，共 24 条 `net.*=`）。
        // P5 删掉 server/core.sh 之后本测试自动 skip，与 v3 fixture 的处理方式一致。
        let core = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../server/core.sh"));
        if !core.exists() {
            eprintln!("skipped: 仓库里已没有 server/core.sh（P5 删除后正常）");
            return;
        }
        let text = std::fs::read_to_string(core).unwrap();
        let v3: Vec<(String, String)> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("net.") && l.contains('='))
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .collect();
        assert_eq!(v3.len(), 24, "v3 的 sysctl 行数变了，先核对 core.sh 再改本测试：{v3:?}");
        let s = sample_state();
        let v4: std::collections::BTreeMap<String, String> =
            parse_conf(&network_conf(&s.node.ports)).into_iter().chain(parse_conf(&conntrack_conf(conntrack_max(2048)))).collect();
        for (k, v) in &v3 {
            // v3 这两条的值是 shell 变量（`${ct_max}` / `${algo}`），只能比键在不在，值单独断言
            if k == "net.netfilter.nf_conntrack_max" || k == "net.ipv4.tcp_congestion_control" {
                assert!(v4.contains_key(k), "{k} 在 v4 里丢了");
                continue;
            }
            assert_eq!(v4.get(k).map(String::as_str), Some(v.as_str()), "{k} 与 v3 不同值");
        }
        assert_eq!(v4.get("net.netfilter.nf_conntrack_max").map(String::as_str), Some("131072"));
        assert_eq!(v4.get("net.ipv4.tcp_congestion_control").map(String::as_str), Some("bbr"));
        // v4 只多不少：v3 的 enable_bbr 是独立函数，v4 把它并进 99-b-ui-network.conf
        assert_eq!(v4.len(), 24, "v4 的键集与 v3 不是一一对应：{v4:?}");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::system`
Expected: 编译失败，`cannot find struct SystemModule`。

- [ ] **Step 3: 实现**

`crates/bui/src/modules/system.rs`（`modules/mod.rs` 的六行 `pub mod` 已由 Task 1 写好，本任务不碰它）：
```rust
use crate::reconcile::{Artifact, Module, PortSpec, RenderCtx};
use crate::sys::Proto;
use bui_schema::model::{Ports, State};

pub const RESOLV_CONF: &str = "/etc/resolv.conf";
pub const CONNTRACK_MODULE: &str = "nf_conntrack";
pub const MEMORY_CONF: &str = "/etc/sysctl.d/99-b-ui-memory.conf";

pub struct SystemModule;

/// 与 v3 core.sh:1504-1508 同值（>4G → 524288，>2G → 262144，否则 131072）。
pub fn conntrack_max(mem_mb: u64) -> u32 {
    if mem_mb > 4096 {
        524288
    } else if mem_mb > 2048 {
        262144
    } else {
        131072
    }
}

/// 监听端口里连续的合成段（10000-10002）+ 两个跳跃段 + 住宅基础端口。
pub fn reserved_ports(p: &Ports) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut singles = vec![p.hy2, p.reality_direct, p.reality_resi];
    singles.sort_unstable();
    singles.dedup();
    let mut i = 0;
    while i < singles.len() {
        let start = singles[i];
        let mut end = start;
        while i + 1 < singles.len() && singles[i + 1] == end + 1 {
            i += 1;
            end = singles[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}-{end}") });
        i += 1;
    }
    if let Some((a, b)) = p.hy2_hop {
        parts.push(format!("{a}-{b}"));
    }
    parts.push(p.hy2_resi.to_string());
    parts.push(format!("{}-{}", p.hy2_resi_hop.0, p.hy2_resi_hop.1));
    parts.join(",")
}

pub fn firewall_ports(p: &Ports) -> Vec<PortSpec> {
    let mut v = vec![
        PortSpec::one(Proto::Tcp, 22),
        PortSpec::one(Proto::Tcp, 80),
        PortSpec::one(Proto::Tcp, 443),
        PortSpec::one(Proto::Tcp, p.reality_direct),
        PortSpec::one(Proto::Tcp, p.reality_resi),
        PortSpec::one(Proto::Udp, p.hy2),
    ];
    if let Some((a, b)) = p.hy2_hop {
        v.push(PortSpec::range(Proto::Udp, a, b));
    }
    v.push(PortSpec::one(Proto::Udp, p.hy2_resi));
    v.push(PortSpec::range(Proto::Udp, p.hy2_resi_hop.0, p.hy2_resi_hop.1));
    v
}

impl Module for SystemModule {
    fn name(&self) -> &'static str {
        "system"
    }

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::new();
        if s.system.sysctl_profile != "off" {
            let ct = conntrack_max(ctx.facts.mem_mb);
            let network = network_conf(&s.node.ports);
            let conntrack = conntrack_conf(ct);
            out.push(Artifact::file("/etc/sysctl.d/99-b-ui-network.conf", network.clone()).mode(0o644));
            out.push(Artifact::file("/etc/sysctl.d/99-b-ui-conntrack.conf", conntrack.clone()).mode(0o644));
            // ≤2G 机器接管 v3 的同名文件（v3 core.sh:283-288 / update.sh:987-996），>2G 删掉它
            let small_mem = ctx.facts.mem_mb <= 2048;
            if small_mem {
                out.push(Artifact::file(MEMORY_CONF, MEMORY_BODY).mode(0o644));
            } else {
                out.push(Artifact::Absent { path: MEMORY_CONF.into() });
            }
            out.push(
                Artifact::file(
                    "/etc/modprobe.d/b-ui-nf_conntrack.conf",
                    format!("options nf_conntrack hashsize={}\n", ct / 4),
                )
                .mode(0o644),
            );
            out.push(Artifact::file("/etc/modules-load.d/b-ui-conntrack.conf", "nf_conntrack\n").mode(0o644));
            // modules-load.d 只管下次开机；本轮就要加载，否则 net.netfilter.* 三个键 sysctl -w 报 ENOENT
            out.push(Artifact::Modprobe { module: CONNTRACK_MODULE.to_string() });
            let mut confs: Vec<&str> = vec![&network, &conntrack];
            if small_mem {
                confs.push(MEMORY_BODY);
            }
            for text in confs {
                for (key, value) in parse_conf(text) {
                    out.push(Artifact::Sysctl { key, value });
                }
            }
        }
        if s.system.static_dns {
            out.push(Artifact::file(RESOLV_CONF, RESOLV_BODY).mode(0o644).immutable());
            if ctx.facts.systemd_resolved {
                out.push(Artifact::UnitState { name: "systemd-resolved".into(), enabled: false, active: false });
            }
        }
        // 只在机器上确实有**活的**防火墙时才产出这个 artifact。判据必须与 apply 第 10 步的两支
        // 完全一致（`*_active`，不是 `has_*`）：装了 ufw 但没 enable 的机器上 `ufw allow` 不会生效，
        // apply 会走「两者都没有」的守卫支、不搬 `firewall` key，于是 diff 规则 8 每轮重新产出
        // 一条 `OpenPorts` —— `changed` 永远非空、`keys["firewall"]` 永不出现，
        // M1 的「二次对账零变更」在这类机器上永远不可达（第三轮审查 C1）。
        // 没有防火墙时的提示由 `serve::reconcile_once` 从 `facts` 追加进 `notes`（notes 不影响
        // `/api/health` 的 degraded 判定，所以它是「每轮提醒」而不是「每轮改动」）。
        if s.system.firewall != "off" && (ctx.facts.ufw_active || ctx.facts.firewalld_active) {
            out.push(Artifact::FirewallPorts { ports: firewall_ports(&s.node.ports) });
        }
        out
    }
}

/// 取 conf 里的 `key=value` 行（跳过注释与空行）。
fn parse_conf(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

/// v3 写的是 `vm.swappiness = 10`（带空格），v4 用统一的 `key=value` 写法——**值同为 10**
/// （spec §3.4「与 v3 同值」）。import-v3 后的首轮对账会因内容哈希不同重写这一个文件一次，
/// 第二轮起零变更，M1 的「二次对账零变更」不受影响。
const MEMORY_BODY: &str = "\
# B-UI v4 内存策略：小内存机器（≤2G）降低 swap 倾向（移植 v3 core.sh:283-288）
# 配合两个 hysteria 的 GOMEMLIMIT/MemoryHigh/MemoryMax 一起生效
vm.swappiness=10
";

const RESOLV_BODY: &str = "\
# B-UI v4 静态 DNS（绕 systemd-resolved，防 GFW UDP 投毒兜底）
# hy2/xray/sing-box 各自用 DoH 解析；这里只给 apt/curl 等次要进程用
nameserver 1.1.1.1
nameserver 8.8.8.8
options edns0 timeout:2 attempts:2 single-request
";

fn network_conf(p: &Ports) -> String {
    format!(
        "\
# B-UI v4 网络栈调优（移植 v3 core.sh:1447-1494，合并原 99-hysteria-perf.conf）
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=1048576
net.core.wmem_default=1048576
net.ipv4.tcp_retries2=8
net.ipv4.tcp_mtu_probing=1
net.ipv4.tcp_keepalive_time=600
net.ipv4.tcp_keepalive_intvl=30
net.ipv4.tcp_keepalive_probes=3
net.ipv4.tcp_no_metrics_save=1
net.ipv4.tcp_slow_start_after_idle=0
net.ipv4.tcp_notsent_lowat=131072
net.ipv4.tcp_rmem=4096 262144 16777216
net.ipv4.tcp_wmem=4096 65536 16777216
net.ipv4.udp_mem=262144 524288 1048576
net.ipv4.ip_local_port_range=10000 65535
# 把监听端口与跳跃段从临时端口池排除，避免出向连接抢占跳跃段端口
net.ipv4.ip_local_reserved_ports={reserved}
net.core.netdev_max_backlog=5000
net.ipv4.tcp_max_syn_backlog=8192
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=bbr
",
        reserved = reserved_ports(p)
    )
}

fn conntrack_conf(ct: u32) -> String {
    format!(
        "\
# B-UI v4 conntrack 容量（两个 hy2 + xray + relay 共享一张表）
net.netfilter.nf_conntrack_max={ct}
net.netfilter.nf_conntrack_udp_timeout=20
net.netfilter.nf_conntrack_udp_timeout_stream=60
"
    )
}
```
注意 `net.ipv4.tcp_rmem=4096 262144 16777216` 这类带空格的值：`Artifact::Sysctl.value` 原样保留空格，`RealHost::sysctl_set` 用 `&["-w", "key=value"]` 传一个参数，天然正确。

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui modules::system && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 14 passed（`v4_sysctl_values_match_the_v3_block_verbatim` 在 P5 删掉 `server/core.sh` 之后打印 skipped 仍计 passed）。

- [ ] **Step 5: Commit**

```bash
git add crates/bui/src/modules/system.rs
git commit -m "feat(bui): 系统模块（sysctl/conntrack 模块加载/静态 DNS/防火墙端口）"
```

---

### Task 7: SSH 硬化模块与 `bui harden-ssh`

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/ssh.rs`, `crates/bui/src/commands/harden_ssh.rs`
- Modify: `crates/bui/src/main.rs`（只改一处：`Command::HardenSsh` 的 arm 改成 `commands::harden_ssh::run(Paths::default_server(), Arc::new(RealHost::new())).await`；`mod` 行与 `commands/mod.rs` 已由 Task 1 写好）

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Module, RenderCtx, Unit, Verify, count_pubkeys}`、`crate::reconcile::diff::{plan, PlanInput}`、`crate::reconcile::apply::{apply, ApplyInput, BinaryInstaller}`、`crate::state::store::Store`
- Produces:
```rust
// crate::modules::ssh
pub struct SshModule;
impl Module for SshModule { fn name(&self) -> &'static str { "ssh" } /* render */ }
pub const HARDENING_CONF: &str = "/etc/ssh/sshd_config.d/00-b-ui-hardening.conf";
pub const LEGACY_CONF: &str = "/etc/ssh/sshd_config.d/99-b-ui-hardening.conf";
pub fn hardening_body() -> &'static str;
// crate::commands::harden_ssh
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
pub async fn run_with(store: Store, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
```
**为什么 `harden-ssh` 在守护进程未运行时也放行（spec §2.4 的白名单要扩一项，批准本计划时一并回写 spec）**：spec §2.4 原文把「守护进程未运行时可用」限定为 install / upgrade / status / reconcile，而 spec §3.4 的提示语又要求用户「加好公钥后运行 `b-ui harden-ssh`」——这条提示恰恰出现在装机对账之后、守护进程可能还没起来的时候。`harden-ssh` 在本进程内直接 `plan` + `apply` 是安全的，因为它只渲染 `SshModule` 的一个 artifact：不写 `.verify/`（`Verify::Sshd` 是「写目标文件再 `sshd -t`」的特例，不落候选文件）、不写 `runtime.json`、不碰任何内核单元，因此与守护进程正在跑的对账没有交叉写。所以本计划的口径是：`harden-ssh` 与 install/upgrade/status/reconcile 同级放行，菜单第 7 项不标「需守护进程」（Task 17）。

**回写 spec §2.4 时要写的完整集合**（原文只列了四项，但 P1 实际交付的 CLI 在守护进程未运行时可用的是七项；不把这张表写全，P2/P3 的实现者会按四项那条去加不必要的「需守护进程」拦截，第三轮审查 C13）：

| 子命令 | 为什么不需要守护进程 |
|---|---|
| `install` | 它自己就是首装路径：socket 还不存在，第 9 步在本进程内跑完整对账（Task 16 第 9 步；守护进程已在跑时反而改走 `POST /api/reconcile`） |
| `upgrade`（含 `--rollback`） | 只换 `bin/bui` 与恢复 state 备份，随后 `systemctl restart b-ui`；socket 不可用时退化为进程内对账（Task 17） |
| `status` | socket 不可用时退化为本地读 `state.json` + `runtime.json` + `host.unit_is_active` 现场拼一个 `HealthResponse`（Task 17 的 `status.rs`） |
| `reconcile` | `serve::reconcile_cli` 的两支之一就是进程内直接跑（Task 15） |
| `harden-ssh` | 只渲染 `SshModule` 的一个 artifact，不写 `.verify/` 与 `runtime.json`，与守护进程的对账无交叉写（本节上面的理由） |
| `import-v3` | 只调 `bui_schema::v3::import` 并写一个 `state.json`（或 `--out` 指定的文件），**不对账、不碰任何单元**（Task 16 的 `import_v3::run`） |
| `menu` | 菜单本身要在守护进程挂掉时仍能进去看状态、手动对账；4/5 两项（重启数据面 / 查看日志）标灰（Task 17） |

余下的 `serve` 是守护进程自身的入口（谈不上「需要守护进程」），`auth-hook` 归 P2。

移植参照：`server/core.sh:1867-1935`（`harden_ssh`）。审计 §3.2「SSH 硬化 ×3 → 合并为一份」：v4 只有这一处实现，`bui install` / `bui reconcile` / `bui harden-ssh` 都走它。必须保留 v3 两条血泪经验：① 文件名 `00-` 前缀（OpenSSH 首个匹配生效，要盖过 `50-cloud-init.conf`；v3 用 `99-` 时加固形同虚设）；② 没有公钥时**不写**（否则把自己锁在门外）。

- [ ] **Step 1: 写失败测试（模块）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx, Unit, Verify};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn ctx(pubkeys: u32, ssh_unit: &str) -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
                has_ufw: false, ufw_active: false, has_firewalld: false, firewalld_active: false,
                ssh_unit: ssh_unit.into(), ssh_pubkeys: pubkeys, systemd_resolved: false,
            },
        }
    }

    #[test]
    fn hardening_file_is_00_prefixed_verified_and_reloads_the_right_unit() {
        let arts = SshModule.render(&sample_state(), &ctx(2, "ssh"));
        assert_eq!(arts.len(), 2, "硬化文件 + 删除 99- 老文件");
        match &arts[0] {
            Artifact::File { path, content, mode, verify, restart, immutable, restart_key } => {
                assert_eq!(path.to_str(), Some(HARDENING_CONF));
                assert_eq!(*mode, 0o644);
                assert_eq!(*verify, Some(Verify::Sshd));
                assert_eq!(*restart, Some(Unit::reload("ssh")));
                assert!(!*immutable);
                assert_eq!(*restart_key, None);
                let text = String::from_utf8(content.clone()).unwrap();
                assert!(text.contains("PasswordAuthentication no"));
                assert!(text.contains("PermitRootLogin prohibit-password"));
                assert!(text.contains("KbdInteractiveAuthentication no"));
                assert!(text.contains("ChallengeResponseAuthentication no"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(arts[1], Artifact::Absent { path: LEGACY_CONF.into() });
    }

    #[test]
    fn uses_sshd_unit_when_that_is_what_the_distro_has() {
        let arts = SshModule.render(&sample_state(), &ctx(1, "sshd"));
        match &arts[0] {
            Artifact::File { restart, .. } => assert_eq!(*restart, Some(Unit::reload("sshd"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_pubkey_means_no_hardening_at_all() {
        let arts = SshModule.render(&sample_state(), &ctx(0, "sshd"));
        assert_eq!(arts, vec![], "没有公钥时绝不禁用密码登录");
    }

    #[test]
    fn disabled_in_state_means_no_artifacts() {
        let mut s = sample_state();
        s.system.ssh_hardening = false;
        assert_eq!(SshModule.render(&s, &ctx(3, "sshd")), vec![]);
    }

    #[test]
    fn counts_only_real_pubkey_lines() {
        assert_eq!(
            crate::reconcile::count_pubkeys(
                "# 注释\n\nssh-ed25519 AAAA a@b\n  ecdsa-sha2-nistp256 BBB c@d\nnot-a-key xxx\nssh-rsa CCC e@f\n"
            ),
            3
        );
        assert_eq!(crate::reconcile::count_pubkeys(""), 0);
        assert_eq!(crate::reconcile::count_pubkeys("#ssh-rsa AAA commented\n"), 0);
    }
}
```

- [ ] **Step 2: 写失败测试（命令）**

`crates/bui/src/commands/harden_ssh.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use bui_schema::paths::Paths;

    fn scratch(d: &tempfile::TempDir) -> Paths {
        Paths { base_dir: d.path().into(), certs_dir: d.path().join("certs"), bin_dir: d.path().join("bin") }
    }

    #[tokio::test]
    async fn writes_the_conf_and_reloads_sshd() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.files.insert("/root/.ssh/authorized_keys".into(), (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600));
        });
        let store = Store::create(crate::paths::state_file(&paths), crate::testutil::sample_state()).await.unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(host.text(crate::modules::ssh::HARDENING_CONF).unwrap().contains("PasswordAuthentication no"));
        assert!(host.ops().contains(&"systemd:reload:sshd".to_string()));
    }

    #[tokio::test]
    async fn sshd_test_failure_rolls_back_and_does_not_reload() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.files.insert("/root/.ssh/authorized_keys".into(), (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600));
            i.scripted.push(("sshd -t".into(), CmdOut::failure(255, "bad configuration option")));
        });
        let store = Store::create(crate::paths::state_file(&paths), crate::testutil::sample_state()).await.unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(host.text(crate::modules::ssh::HARDENING_CONF).is_none(), "sshd -t 失败要回滚");
        assert!(!host.ops().iter().any(|o| o.starts_with("systemd:reload")));
    }

    #[tokio::test]
    async fn without_a_pubkey_nothing_is_written() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        let store = Store::create(crate::paths::state_file(&paths), crate::testutil::sample_state()).await.unwrap();
        run_with(store, paths, host.clone()).await.unwrap();
        assert!(host.ops().is_empty());
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p bui modules::ssh commands::harden_ssh`
Expected: 编译失败（`SshModule` / `run_with` 未定义）。

- [ ] **Step 4: 实现 `modules/ssh.rs`**

```rust
use crate::reconcile::{Artifact, Module, RenderCtx, Unit, Verify};
use bui_schema::model::State;

pub const HARDENING_CONF: &str = "/etc/ssh/sshd_config.d/00-b-ui-hardening.conf";
/// v3 早期用过 99- 前缀，反被 50-cloud-init.conf 抢先生效（core.sh:1893 的踩坑记录）。
pub const LEGACY_CONF: &str = "/etc/ssh/sshd_config.d/99-b-ui-hardening.conf";

pub struct SshModule;

pub fn hardening_body() -> &'static str {
    "\
# B-UI SSH 加固（00- 前缀确保先于 50-cloud-init.conf 生效；OpenSSH 首个匹配生效）
PasswordAuthentication no
PermitRootLogin prohibit-password
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
"
}

impl Module for SshModule {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        if !s.system.ssh_hardening || ctx.facts.ssh_pubkeys == 0 {
            return Vec::new();
        }
        vec![
            Artifact::file(HARDENING_CONF, hardening_body())
                .mode(0o644)
                .verify(Verify::Sshd)
                .restart(Unit::reload(&ctx.facts.ssh_unit)),
            Artifact::Absent { path: LEGACY_CONF.into() },
        ]
    }
}
```
「没有公钥」的提示不在 `render` 里发（`render` 必须是纯函数）：Task 15 的 `reconcile_once` 在 `state.system.ssh_hardening && facts.ssh_pubkeys == 0` 时往 `report.notes` 追加
「未在 /root/.ssh/authorized_keys 检测到公钥，已跳过 SSH 硬化；加好公钥后运行 `b-ui harden-ssh`」。

- [ ] **Step 5: 实现 `commands/harden_ssh.rs`**

```rust
use crate::modules::ssh::SshModule;
use crate::reconcile::apply::{apply, ApplyInput, BinaryInstaller};
use crate::reconcile::diff::{plan, PlanInput};
use crate::reconcile::{Facts, Module, RenderCtx};
use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub async fn run(paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    run_with(store, paths, host).await
}

pub async fn run_with(store: Store, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    let state = store.read().await;
    let h = host.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<_> {
        let facts = Facts::probe(h.as_ref())?;
        let ctx = RenderCtx { paths: paths.clone(), facts };
        let arts = SshModule.render(&state, &ctx);
        let (keys, versions) = (BTreeMap::new(), BTreeMap::new());
        let p = plan(
            PlanInput { artifacts: &arts, paths: &paths, keys: &keys, installed_versions: &versions },
            h.as_ref(),
        )?;
        Ok(apply(
            ApplyInput { plan: p, paths: &paths, facts: &ctx.facts, installer: &NoBinaries, dry_run: false },
            h.as_ref(),
        ))
    })
    .await??;
    for line in out.notes.iter().chain(out.verify_failures.iter()).chain(out.errors.iter()) {
        tracing::warn!("{line}");
        println!("{line}");
    }
    if out.changed.is_empty() {
        println!("SSH 加固已是最新（无改动）");
    } else {
        println!("SSH 加固已写入 {}", crate::modules::ssh::HARDENING_CONF);
    }
    Ok(())
}

/// ssh 模块不产出二进制 artifact。
struct NoBinaries;
impl BinaryInstaller for NoBinaries {
    fn install(&self, name: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> Result<()> {
        anyhow::bail!("ssh 模块不应产出二进制 artifact：{name}")
    }
}
```
- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui modules::ssh commands::harden_ssh && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 8 passed（ssh 5 + harden_ssh 3）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/modules/ssh.rs crates/bui/src/commands/harden_ssh.rs crates/bui/src/main.rs
git commit -m "feat(bui): SSH 硬化模块（单一实现）与 bui harden-ssh"
```

---

### Task 8: systemd 单元模块（6 单元 + 资源限制 + v3 遗留清理）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/units.rs`

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, Module, RenderCtx, Unit, MANAGED_UNITS, LEGACY_UNITS}`、`crate::paths::{caddy_xdg, CLI_LINKS}`
- Produces:
```rust
pub struct UnitsModule;
impl Module for UnitsModule { fn name(&self) -> &'static str { "units" } /* render */ }
pub const LEGACY_FILES: [&str; 9] = [
    "/etc/systemd/system/hysteria-server.service.d/override.conf",
    "/etc/systemd/system/hysteria-server.service.d/priority.conf",
    "/etc/systemd/system/xray.service.d/99-b-ui-override.conf",
    "/etc/systemd/system/caddy.service.d/override.conf",
    "/etc/sysctl.d/99-hysteria-perf.conf",
    "/etc/sysctl.d/99-hysteria-bbr.conf",
    "/opt/b-ui/hy2-portjump-cleanup.sh",
    "/opt/b-ui/hy2-watchdog.sh",
    "/opt/b-ui/cert-sync.sh",
];
pub fn unit_text(name: &str, ctx: &RenderCtx) -> String;
```
`MANAGED_UNITS` / `LEGACY_UNITS` 用 Task 4 里的唯一一份（`use crate::reconcile::{LEGACY_UNITS, MANAGED_UNITS};`），本模块**不再自己定义**——v3 遗留列表曾经在 units 与 drift 各有一份、内容还不一样，导致 `hysteria-server@.service` / `xray@.service` 只有 `--force` 才清理。

`LEGACY_FILES` 里的 `/etc/sysctl.d/99-hysteria-bbr.conf` 是 v3 的 BBR 文件（`server/core.sh:1349` 写 `net.core.default_qdisc=fq` + `net.ipv4.tcp_congestion_control=<algo>`，`server/update.sh:1200-1211` 的 D2 块还会把 `<algo>` 升成 `bbr3`/`bbrv3`/`bbr_v3`）。v4 把这两个键并进 Task 6 的 `99-b-ui-network.conf`（值 `bbr`），而 `sysctl --system` 按文件名升序加载——`99-hysteria-bbr.conf` 排在 `99-b-ui-network.conf` **之后**，留着它就等于 v3 的值每次开机覆盖 v4 的期望值（对账只 `sysctl -w` 当前值，看不出下次开机会被翻回去）。它的文件名不带 `b-ui` 前缀，所以 Task 5 的 `stray_conf` 也扫不到，只能在这里显式删。

移植参照：`server/core.sh:197-274`（两个 hysteria 单元的资源限制与内存调优）、`server/core.sh:1722-1790`（`create_services`：面板单元 + xray drop-in）、`server/residential-helper.sh:507-522`（relay 单元）、`server/core.sh:884-891`（caddy drop-in）、`server/core.sh` 写 `/usr/local/bin/b-ui` 的那一段（v4 变成 `Artifact::Symlink`）。v4 的变化（spec §3.4、审计 §3.1/§3.2）：① 六个单元都写**完整单元文件**，不再 drop-in 覆盖发行版单元（四个内核二进制都在 `/opt/b-ui/bin/`）；② 两个 hysteria 的 `ExecStartPre=…hy2-portjump-cleanup.sh` 删掉（v4 无任何 iptables/nft 规则）；③ `xray` 与 `b-ui-relay` 补 `MemoryHigh/MemoryMax`（eff-C5）；④ 全部 `LimitNOFILE=1048576`，四个数据面单元 `Nice=-5`；⑤ `b-ui.service` 自身 `MemoryMax=200M` + `Restart=always`；⑥ 不生成任何 timer；⑦ 产出 `/usr/local/bin/bui` 与 `/usr/local/bin/b-ui` 两个符号链接（都指向 `<base>/bin/bui`），spec §1/§2.4 的「`sudo b-ui` 是 `bui menu` 的符号链接」由此落地，且 `bui reconcile` 能自愈被误删的入口。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn ctx() -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
                has_ufw: false, ufw_active: false, has_firewalld: false, firewalld_active: false,
                ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: false,
            },
        }
    }

    fn unit_of(arts: &[Artifact], name: &str) -> String {
        arts.iter()
            .find_map(|a| match a {
                Artifact::Unit { name: u, content, dropin: None } if u.name == name => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("没有渲染单元 {name}"))
    }

    #[test]
    fn renders_exactly_six_units_and_no_timers() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let names: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Unit { name, .. } => Some(name.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, MANAGED_UNITS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Unit { name, .. } if name.name.contains(".timer"))));
    }

    #[test]
    fn every_unit_has_the_required_limits() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            let t = unit_of(&arts, name);
            assert!(t.contains("LimitNOFILE=1048576"), "{name} 缺 LimitNOFILE");
            assert!(t.contains("Restart=always"), "{name} 缺 Restart=always");
            assert!(t.contains("WantedBy=multi-user.target"), "{name} 缺 Install 段");
        }
        for name in ["hysteria-server", "hysteria-residential", "xray", "b-ui-relay"] {
            assert!(unit_of(&arts, name).contains("Nice=-5"), "{name} 缺 Nice=-5");
        }
    }

    #[test]
    fn hysteria_units_keep_v3_memory_tuning_and_drop_the_portjump_hook() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let direct = unit_of(&arts, "hysteria-server");
        assert!(direct.contains("ExecStart=/opt/b-ui/bin/hysteria server --config /opt/b-ui/config.yaml"));
        assert!(direct.contains("Environment=GOMEMLIMIT=400MiB"));
        assert!(direct.contains("Environment=HYSTERIA_LOG_LEVEL=warn"));
        assert!(direct.contains("MemoryHigh=500M"));
        assert!(direct.contains("MemoryMax=700M"));
        assert!(direct.contains("TimeoutStopSec=15"));
        assert!(!direct.contains("ExecStartPre"), "v4 没有端口跳跃 NAT 链，不需要清理钩子");
        let low = direct.to_lowercase();
        assert!(!low.contains("iptables") && !low.contains("nft"));
        let resi = unit_of(&arts, "hysteria-residential");
        assert!(resi.contains("ExecStart=/opt/b-ui/bin/hysteria server --config /opt/b-ui/config-residential.yaml"));
        assert!(resi.contains("Environment=GOMEMLIMIT=200MiB"));
        assert!(resi.contains("MemoryHigh=300M"));
        assert!(resi.contains("MemoryMax=500M"));
        assert!(resi.contains("LimitNPROC=512"));
        assert!(resi.contains("After=network-online.target b-ui-relay.service"));
    }

    #[test]
    fn xray_and_relay_get_the_memory_caps_v3_was_missing() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        let xray = unit_of(&arts, "xray");
        assert!(xray.contains("ExecStart=/opt/b-ui/bin/xray run -config /opt/b-ui/xray-config.json"));
        assert!(xray.contains("MemoryHigh=300M") && xray.contains("MemoryMax=500M"));
        let relay = unit_of(&arts, "b-ui-relay");
        assert!(relay.contains("ExecStart=/opt/b-ui/bin/sing-box run -c /opt/b-ui/singbox-relay.json"));
        assert!(relay.contains("MemoryHigh=200M") && relay.contains("MemoryMax=300M"));
        assert!(relay.contains("LogRateLimitIntervalSec=10s") && relay.contains("LogRateLimitBurst=200"));
    }

    #[test]
    fn daemon_unit_caps_itself_at_200m() {
        let t = unit_of(&UnitsModule.render(&sample_state(), &ctx()), "b-ui");
        assert!(t.contains("ExecStart=/opt/b-ui/bin/bui serve"));
        assert!(t.contains("MemoryMax=200M"));
        assert!(t.contains("Environment=RUST_LOG=info"));
    }

    #[test]
    fn caddy_unit_uses_the_bundled_binary_with_its_own_data_dir() {
        let t = unit_of(&UnitsModule.render(&sample_state(), &ctx()), "caddy");
        assert!(t.contains("ExecStart=/opt/b-ui/bin/caddy run --config /opt/b-ui/Caddyfile --adapter caddyfile"));
        assert!(t.contains("ExecReload=/opt/b-ui/bin/caddy reload --config /opt/b-ui/Caddyfile --adapter caddyfile --force"));
        assert!(t.contains("Environment=XDG_DATA_HOME=/opt/b-ui/caddy"));
        assert!(t.contains("Environment=XDG_CONFIG_HOME=/opt/b-ui/caddy"));
    }

    #[test]
    fn enables_all_six_and_removes_v3_leftovers() {
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            assert!(
                arts.contains(&Artifact::UnitState { name: name.into(), enabled: true, active: true }),
                "{name} 要 enable+start"
            );
        }
        for legacy in LEGACY_UNITS {
            assert!(
                arts.contains(&Artifact::UnitState { name: legacy.into(), enabled: false, active: false }),
                "{legacy} 要停用"
            );
            assert!(
                arts.contains(&Artifact::Absent { path: format!("/etc/systemd/system/{legacy}").into() }),
                "{legacy} 的单元文件要删"
            );
        }
        for f in LEGACY_FILES {
            assert!(arts.contains(&Artifact::Absent { path: f.into() }), "{f} 要删");
        }
    }

    #[test]
    fn v3_sysctl_leftovers_are_deleted_but_the_memory_conf_is_not() {
        // `99-hysteria-bbr.conf`（v3 core.sh:1349 / update.sh:1200-1211 的 D2 块，可能写成 bbr3）
        // 按文件名排在 `99-b-ui-network.conf` 之后 → 不删就每次开机覆盖 v4 的拥塞算法期望值；
        // 而 `99-b-ui-memory.conf` 是 Task 6 在 ≤2G 机器上**接管**的同名文件，
        // 误放进这里会让两个模块每轮互斗（system 写、units 删），对账永不收敛。
        let arts = UnitsModule.render(&sample_state(), &ctx());
        assert!(LEGACY_FILES.contains(&"/etc/sysctl.d/99-hysteria-bbr.conf"));
        assert!(arts.contains(&Artifact::Absent { path: "/etc/sysctl.d/99-hysteria-bbr.conf".into() }));
        // 路径写字面量、不引 `system::MEMORY_CONF`：Task 6 与本任务是并行的两支，
        // 引过去就凭空多一条编译依赖
        assert!(
            !arts.iter().any(|a| matches!(a, Artifact::Absent { path }
                if path.to_str() == Some("/etc/sysctl.d/99-b-ui-memory.conf"))),
            "99-b-ui-memory.conf 归 system 模块管，不是遗留文件"
        );
    }

    #[test]
    fn v3_leftovers_never_include_a_v4_managed_unit() {
        // b-ui-relay.service 是 v4 自己的单元：如果它出现在遗留清理里，
        // 对账刚写完就会被 disable/stop/删文件，住宅两个节点一起断
        let arts = UnitsModule.render(&sample_state(), &ctx());
        for name in MANAGED_UNITS {
            assert!(
                !arts.contains(&Artifact::UnitState { name: name.into(), enabled: false, active: false }),
                "{name} 被当成遗留单元停用了"
            );
            assert!(
                !arts.contains(&Artifact::Absent { path: format!("/etc/systemd/system/{name}.service").into() }),
                "{name} 的单元文件被当成遗留文件删了"
            );
        }
    }

    #[test]
    fn both_cli_entry_symlinks_point_at_the_bundled_binary() {
        // spec §1/§2.4：sudo b-ui 是 bui menu 的符号链接
        let arts = UnitsModule.render(&sample_state(), &ctx());
        assert!(arts.contains(&Artifact::Symlink {
            path: "/usr/local/bin/bui".into(),
            target: "/opt/b-ui/bin/bui".into()
        }));
        assert!(arts.contains(&Artifact::Symlink {
            path: "/usr/local/bin/b-ui".into(),
            target: "/opt/b-ui/bin/bui".into()
        }));
    }

    #[test]
    fn render_is_pure_so_the_diff_does_not_flap() {
        let a = UnitsModule.render(&sample_state(), &ctx());
        let b = UnitsModule.render(&sample_state(), &ctx());
        assert_eq!(a, b);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::units`
Expected: 编译失败，`cannot find struct UnitsModule`。

- [ ] **Step 3: 实现**

`render` 的产出顺序固定为：6 个 `Artifact::Unit`（按 `MANAGED_UNITS` 顺序）→ 2 个 `Artifact::Symlink`（`CLI_LINKS` 顺序，target 都是 `ctx.paths.bin_dir.join("bui")`）→ 6 个启用态 `UnitState` → `LEGACY_UNITS` 的停用态 `UnitState` → `LEGACY_UNITS` 的单元文件 `Absent` → `LEGACY_FILES` 的 `Absent`。

**六个 `Artifact::Unit` 的 `name` 一律用 `Unit::restart(<名字>)`，caddy 也不例外**：单元文件本身变了必须 `systemctl restart`，`reload` 不会让新的 `ExecStart` / `Environment` / `MemoryMax` 生效（Task 5 第 3 步把 `WriteUnit` 的这个 `Unit` 直接放进重启集合，第 12 步照 `action` 派发；写成 `Unit::reload("caddy")` 就会出现「改完 caddy.service 只 `systemctl reload caddy`」——换了 XDG 目录或 `--config` 路径却仍跑着旧命令行，且 daemon-reload 之后单元状态与实际进程长期不一致）。**全计划里 `Unit::reload` 只用在两个 `Artifact::File` 上**：Task 10 的 `<base>/Caddyfile`（`.restart(Unit::reload("caddy"))`，spec §2.2 的重启映射：配置内容变化用 `caddy reload`，热更不断连）与 Task 7 的 `/etc/ssh/sshd_config.d/00-b-ui-hardening.conf`（`.restart(Unit::reload(&ctx.facts.ssh_unit))`，reload 不断开当前 ssh 会话）。**没有任何 `Artifact::Unit` 用 `reload`。**

`unit_text` 用 `format!` 把 `ctx.paths.bin_dir`（`{bin}`）、`ctx.paths.base_dir`（`{base}`，Caddyfile 路径由它拼出）、`crate::paths::caddy_xdg(&ctx.paths)`（`{caddy_xdg}`）代入。六份正文：

```
# b-ui.service
[Unit]
Description=B-UI v4 Controller
Documentation=https://github.com/Buxiulei/b-ui
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/bui serve
Restart=always
RestartSec=3
LimitNOFILE=1048576
MemoryMax=200M
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```
```
# hysteria-server.service
[Unit]
Description=Hysteria Server (Direct)
Documentation=https://v2.hysteria.network/
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/hysteria server --config {base}/config.yaml
User=root
Group=root
Restart=always
RestartSec=3
TimeoutStopSec=15
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=400MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=500M
MemoryMax=700M

[Install]
WantedBy=multi-user.target
```
```
# hysteria-residential.service
[Unit]
Description=Hysteria Server (Residential)
Documentation=https://v2.hysteria.network/
After=network-online.target b-ui-relay.service
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/hysteria server --config {base}/config-residential.yaml
User=root
Group=root
Restart=always
RestartSec=3
TimeoutStopSec=15
LimitNOFILE=1048576
LimitNPROC=512
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=200MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=300M
MemoryMax=500M

[Install]
WantedBy=multi-user.target
```
```
# xray.service
[Unit]
Description=Xray Service (VLESS-REALITY)
Documentation=https://xtls.github.io/
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/xray run -config {base}/xray-config.json
Restart=always
RestartSec=3
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
MemoryHigh=300M
MemoryMax=500M

[Install]
WantedBy=multi-user.target
```
```
# b-ui-relay.service
[Unit]
Description=B-UI Outbound Relay (sing-box)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin}/sing-box run -c {base}/singbox-relay.json
Restart=always
RestartSec=3
LimitNOFILE=1048576
Nice=-5
MemoryHigh=200M
MemoryMax=300M
LogRateLimitIntervalSec=10s
LogRateLimitBurst=200

[Install]
WantedBy=multi-user.target
```
```
# caddy.service
[Unit]
Description=Caddy (B-UI managed)
Documentation=https://caddyserver.com/docs/
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart={bin}/caddy run --config {base}/Caddyfile --adapter caddyfile
ExecReload={bin}/caddy reload --config {base}/Caddyfile --adapter caddyfile --force
Restart=always
RestartSec=5
LimitNOFILE=1048576
Environment=XDG_DATA_HOME={caddy_xdg}
Environment=XDG_CONFIG_HOME={caddy_xdg}

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui modules::units && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 11 passed。

- [ ] **Step 5: Commit**

```bash
git add crates/bui/src/modules/units.rs
git commit -m "feat(bui): systemd 单元模块（6 单元资源限制 + CLI 符号链接 + 清理 v3 遗留单元）"
```

---

### Task 9: 内核二进制管理（manifest / 下载 / sha256 / 版本探测）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/kernels/mod.rs`

**Interfaces:**
- Consumes: `crate::sys::Host`、`crate::reconcile::apply::BinaryInstaller`
- Produces:
```rust
pub const KERNELS: [&str; 4] = ["hysteria", "xray", "sing-box", "caddy"];
pub const MANIFEST_URL: &str = "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json";
/// 指定版本的 manifest：GitHub 的形状是 `releases/download/<tag>/<asset>`（**不是** `releases/<tag>/download/`）
pub const MANIFEST_URL_TEMPLATE: &str = "https://github.com/Buxiulei/b-ui/releases/download/v{version}/manifest.json";
/// 环境变量覆盖（总纲 C4「manifest 来源与覆盖」明写由 P1 实现）
pub const MANIFEST_URL_ENV: &str = "BUI_MANIFEST_URL";
/// `MANIFEST_URL_TEMPLATE` 代入版本号
pub fn manifest_url_for_version(version: &str) -> String;
/// **唯一**一处决定 manifest 地址（纯函数，便于测试）：
/// `cli_override` > `version`（套模板）> `env`（`$BUI_MANIFEST_URL`）> `MANIFEST_URL`
pub fn resolve_manifest_url(cli_override: Option<&str>, version: Option<&str>, env: Option<&str>) -> String;
/// 读进程环境后调 `resolve_manifest_url`；Task 15/16/17 一律用它，不再各自拼 URL
pub fn manifest_url(cli_override: Option<&str>, version: Option<&str>) -> String;
/// 总纲 C4 的形状：`version` 是 bui 自己的版本，`kernels` 是版本表（键 = state `versions`
/// 的字段名，下划线），`artifacts` 的键固定 `<name>-linux-<amd64|arm64>`（name = 二进制名，
/// 连字符）。未知字段一律忽略（serde 默认），P5 往 manifest 里加字段不会打死 P1。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)] pub kernels: BTreeMap<String, String>,
    #[serde(default)] pub artifacts: BTreeMap<String, Asset>,
    /// 总纲 C4 的可选字段：低于此版本的 `bui` 必须先升到该版本（Task 17 的 `plan_upgrade` 消费）
    #[serde(default)] pub min_upgrade_from: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset { pub url: String, pub sha256: String }
impl Manifest {
    pub fn from_url(fetcher: &dyn Fetcher, url: &str) -> anyhow::Result<Manifest>;
    /// 返回 (`kernels` 表里记的版本, 该架构的 `artifacts` 条目)；`name` 传二进制名
    /// （`sing-box`），内部换成 `kernels` 的键（`sing_box`）与 `artifacts` 的键
    /// （`sing-box-linux-amd64`）
    pub fn kernel_asset(&self, name: &str, arch: &str) -> anyhow::Result<(&str, &Asset)>;
    /// `bui` 自己的资产 = `artifacts["bui-linux-<amd64|arm64>"]`（按架构查，不是 target 三元组）
    pub fn bui_asset(&self, arch: &str) -> anyhow::Result<&Asset>;
}
/// `kernels` 表的键：二进制名里的连字符换成下划线（`sing-box` → `sing_box`）
pub fn kernels_key(name: &str) -> String;
/// `artifacts` 表的键：`<name>-linux-<amd64|arm64>`（架构不支持则报错）
pub fn artifact_key(name: &str, arch: &str) -> anyhow::Result<String>;
/// **同步** trait：实现会阻塞线程，所以每个调用点都必须在 `tokio::task::spawn_blocking` 里
/// （或在纯 CLI 的同步上下文里）执行；`Send + Sync + 'static` 是为了能以 `Arc<dyn Fetcher>`
/// 跨 `spawn_blocking` 边界传递（Task 15/16/17 都这么传）。
/// `url` 允许是 `file://<abs path>` 或**不含 `://` 的本地路径**：`HttpFetcher` 直接读文件，
/// 于是 M5 的升级/回滚演练既能用本机 `python3 -m http.server`，也能直接给一个文件路径
/// （裁决：M5 前不创建任何 Release/tag，所以演练不能依赖公开 Release）。
pub trait Fetcher: Send + Sync + 'static { fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>>; }
pub struct HttpFetcher { client: std::sync::OnceLock<reqwest::blocking::Client> }
impl HttpFetcher { pub fn new() -> Self }
pub fn parse_version(kind: &str, stdout: &str) -> Option<String>;
pub fn installed_version(host: &dyn Host, bin_dir: &Path, kind: &str) -> Option<String>;
pub fn installed_versions(host: &dyn Host, bin_dir: &Path) -> BTreeMap<String, String>;
pub fn sha256_hex(bytes: &[u8]) -> String;
pub fn asset_arch(arch: &str) -> anyhow::Result<&'static str>;   // x86_64→amd64, aarch64→arm64
pub struct KernelInstaller<'a> { pub fetcher: &'a dyn Fetcher, pub host: &'a dyn Host }
impl BinaryInstaller for KernelInstaller<'_> { /* 下载 → sha256 → host.write_file(dest, bytes, 0o755) */ }
```
说明（spec §3.1、审计 §3.2「GitHub release 轮询 ×4 合并」）：内核版本与 sha256 只由 `manifest.json` 决定，v4 不再轮询 GitHub API。形状**以总纲 C4 为准**（文首契约段抄了一份）：`kernels` 只记版本号（键 `hysteria` / `xray` / `sing_box` / `caddy`，另有 P4 用的 `client_sing_box`，P1 读进来不使用），下载地址与校验和一律取 `artifacts`（键 `hysteria-linux-amd64` 这种），安装时以 `artifacts` 为准。manifest 指向的 URL 必须是**裸二进制**（hysteria 官方本来就是裸二进制；xray / sing-box / caddy 官方发压缩包，由 P5 的 Actions 解包重发——这是对 P5 的硬性要求，已写进文首契约段），P1 不实现 tar/gz/zip 解包。

**`reqwest::blocking` 的铁律**：reqwest 文档原文「the functionality in `reqwest::blocking` must not be executed within an async runtime, or it will panic when attempting to block」。因此 `Fetcher` 是同步 trait，`HttpFetcher` 只允许在 `tokio::task::spawn_blocking` 的闭包里或纯同步 CLI 路径里调用；本计划里 Task 15 的 `reconcile_from_ctx`、Task 16 的 `install::run_with`、Task 17 的 `upgrade::run` 与每日自检任务全部用 `Arc<dyn Fetcher>` + `spawn_blocking`，没有任何 `async fn` 直接调 `get_bytes` 的地方。假的 `Fetcher` 测不出这个 panic，所以它靠「签名 + 调用点全部包在 `spawn_blocking` 里」来保证，评审时逐个调用点核对。

- [ ] **Step 1: 写失败测试（四个内核的真实 version 输出）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;
    use std::sync::Mutex;

    // 本机实测输出：sing-box 1.13.19 / xray 26.3.27 / hysteria 2.12.2 / caddy 2.10.2
    const SINGBOX: &str = "sing-box version 1.13.19\n\nEnvironment: go1.25.12 linux/amd64\nTags: with_gvisor,with_quic\n";
    const XRAY: &str = "Xray 26.3.27 (Xray, Penetrates Everything.) d2758a0 (go1.26.1 linux/amd64)\nA unified platform for anti-censorship.\n";
    const HYSTERIA: &str = "\n░█░█\n\na powerful, lightning fast and censorship resistant proxy\n\nVersion:\tv2.12.2\nBuildDate:\t2026-08-23T00:39:00Z\nBuildType:\trelease\n";
    const CADDY: &str = "v2.10.2 h1:g/gTYjGMD0dec+UgMw8SnfmJ3I9+M2TdvoRL/Ovu6U8=\n";

    #[test]
    fn parses_all_four_real_version_outputs() {
        assert_eq!(parse_version("sing-box", SINGBOX).as_deref(), Some("1.13.19"));
        assert_eq!(parse_version("xray", XRAY).as_deref(), Some("26.3.27"));
        assert_eq!(parse_version("hysteria", HYSTERIA).as_deref(), Some("2.12.2"));
        assert_eq!(parse_version("caddy", CADDY).as_deref(), Some("2.10.2"));
        assert_eq!(parse_version("xray", ""), None);
        assert_eq!(parse_version("unknown-kernel", "whatever 1.2.3"), None);
    }

    #[test]
    fn installed_versions_skips_missing_binaries() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files.insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push(("/opt/b-ui/bin/xray version".into(), CmdOut::success(XRAY)));
            i.scripted.push(("/opt/b-ui/bin/sing-box version".into(), CmdOut::success(SINGBOX)));
        });
        let v = installed_versions(&h, std::path::Path::new("/opt/b-ui/bin"));
        assert_eq!(v.get("xray").map(String::as_str), Some("26.3.27"));
        assert_eq!(v.get("sing-box").map(String::as_str), Some("1.13.19"));
        assert_eq!(v.get("hysteria"), None, "文件不存在就不进表，diff 会判成要装");
    }

    struct FakeFetcher { files: Mutex<Vec<(String, Vec<u8>)>> }
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    // 总纲 C4 的形状（只放 amd64，用来同时测「架构缺资产」这一支）
    const MANIFEST_JSON: &str = r#"{
      "version": "4.0.1",
      "kernels": { "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2",
                   "client_sing_box": "1.13.19" },
      "artifacts": {
        "bui-linux-amd64":      { "url": "https://x/bui-amd64", "sha256": "PLACE_SUM" },
        "hysteria-linux-amd64": { "url": "https://x/hysteria", "sha256": "PLACE_SUM" },
        "xray-linux-amd64":     { "url": "https://x/xray", "sha256": "aa" },
        "sing-box-linux-amd64": { "url": "https://x/sb", "sha256": "bb" },
        "caddy-linux-amd64":    { "url": "https://x/caddy", "sha256": "cc" }
      }
    }"#;

    fn fixture(payload: &[u8]) -> (Manifest, FakeFetcher) {
        let json = MANIFEST_JSON.replace("PLACE_SUM", &sha256_hex(payload));
        let m: Manifest = serde_json::from_str(&json).unwrap();
        let f = FakeFetcher {
            files: Mutex::new(vec![
                ("https://x/manifest.json".into(), json.into_bytes()),
                ("https://x/hysteria".into(), payload.to_vec()),
                ("https://x/bui-amd64".into(), payload.to_vec()),
                ("https://x/xray".into(), payload.to_vec()),
            ]),
        };
        (m, f)
    }

    #[test]
    fn manifest_round_trip_and_asset_lookup() {
        let (m, f) = fixture(b"binary-bytes");
        assert_eq!(Manifest::from_url(&f, "https://x/manifest.json").unwrap(), m);
        let (ver, asset) = m.kernel_asset("hysteria", "x86_64").unwrap();
        assert_eq!(ver, "2.12.2");
        assert_eq!(asset.url, "https://x/hysteria");
        // 连字符的二进制名 → 下划线的版本表键 + 连字符的资产键
        let (ver, asset) = m.kernel_asset("sing-box", "x86_64").unwrap();
        assert_eq!(ver, "1.13.19");
        assert_eq!(asset.url, "https://x/sb");
        assert_eq!(kernels_key("sing-box"), "sing_box");
        assert_eq!(artifact_key("sing-box", "aarch64").unwrap(), "sing-box-linux-arm64");
        assert!(m.kernel_asset("hysteria", "riscv64").is_err(), "架构不支持");
        assert!(m.kernel_asset("hysteria", "aarch64").is_err(), "本 fixture 没有 arm64 资产");
        assert!(m.kernel_asset("shadowsocks", "x86_64").is_err(), "版本表里没有这个内核");
        assert_eq!(m.bui_asset("x86_64").unwrap().url, "https://x/bui-amd64");
        assert!(m.bui_asset("armv7l").is_err());
    }

    #[test]
    fn manifest_url_resolution_follows_the_c4_order() {
        // C4：--manifest-url > --version（模板）> $BUI_MANIFEST_URL > 内置 latest。
        // 测纯函数，不动进程环境（`std::env::set_var` 会影响并行跑的其它测试）。
        assert_eq!(resolve_manifest_url(None, None, None), MANIFEST_URL);
        assert_eq!(
            resolve_manifest_url(None, None, Some("http://127.0.0.1:8000/manifest.json")),
            "http://127.0.0.1:8000/manifest.json"
        );
        assert_eq!(
            resolve_manifest_url(None, Some("4.0.1"), Some("http://127.0.0.1:8000/manifest.json")),
            "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/manifest.json",
            "--version 优先于环境变量，且必须是 releases/download/<tag>/ 的形状"
        );
        assert_eq!(
            resolve_manifest_url(Some("/tmp/dist/manifest.json"), Some("4.0.1"), Some("http://x/m.json")),
            "/tmp/dist/manifest.json",
            "--manifest-url 最高优先"
        );
        assert_eq!(manifest_url_for_version("4.0.1"), resolve_manifest_url(None, Some("4.0.1"), None));
    }

    #[test]
    fn the_http_fetcher_also_reads_local_paths_and_file_urls() {
        // M5 演练用：`--manifest-url /tmp/dist/manifest.json` 或 `file:///tmp/dist/manifest.json`
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("manifest.json");
        std::fs::write(&p, b"{\"version\":\"4.0.1\"}").unwrap();
        let f = HttpFetcher::new();
        assert_eq!(f.get_bytes(p.to_str().unwrap()).unwrap(), b"{\"version\":\"4.0.1\"}");
        assert_eq!(
            f.get_bytes(&format!("file://{}", p.display())).unwrap(),
            b"{\"version\":\"4.0.1\"}"
        );
        let m = Manifest::from_url(&f, p.to_str().unwrap()).unwrap();
        assert_eq!(m.version, "4.0.1");
        assert_eq!(m.min_upgrade_from, None, "可选字段缺失不报错");
        assert!(f.get_bytes(&d.path().join("nope.json").display().to_string()).is_err());
    }

    #[test]
    fn asset_arch_maps_only_supported_arches() {
        assert_eq!(asset_arch("x86_64").unwrap(), "amd64");
        assert_eq!(asset_arch("aarch64").unwrap(), "arm64");
        assert!(asset_arch("armv7l").is_err());
    }

    #[test]
    fn install_writes_0755_after_verifying_sha256() {
        let payload = b"binary-bytes";
        let (m, f) = fixture(payload);
        let h = FakeHost::new();
        let (ver, asset) = m.kernel_asset("hysteria", "x86_64").unwrap();
        KernelInstaller { fetcher: &f, host: &h }
            .install("hysteria", ver, &asset.sha256, &asset.url, std::path::Path::new("/opt/b-ui/bin/hysteria"))
            .unwrap();
        assert_eq!(h.text("/opt/b-ui/bin/hysteria").unwrap(), "binary-bytes");
        assert_eq!(h.mode("/opt/b-ui/bin/hysteria"), Some(0o755));
        assert_eq!(h.ops(), vec!["write:/opt/b-ui/bin/hysteria:755"]);
    }

    #[test]
    fn install_refuses_on_sha256_mismatch_and_writes_nothing() {
        let (m, f) = fixture(b"binary-bytes");
        let h = FakeHost::new();
        let (ver, asset) = m.kernel_asset("xray", "x86_64").unwrap();   // sha256 写的是 "aa"，与内容不符
        let err = KernelInstaller { fetcher: &f, host: &h }
            .install("xray", ver, &asset.sha256, &asset.url, std::path::Path::new("/opt/b-ui/bin/xray"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256"), "{err}");
        assert!(h.ops().is_empty(), "校验不过不写盘");
    }

    #[test]
    fn install_propagates_download_failure() {
        let (_m, f) = fixture(b"x");
        let h = FakeHost::new();
        assert!(KernelInstaller { fetcher: &f, host: &h }
            .install("caddy", "2.10.2", "cc", "https://x/missing", std::path::Path::new("/opt/b-ui/bin/caddy"))
            .is_err());
        assert!(h.ops().is_empty());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui kernels::`
Expected: 编译失败，`cannot find struct Manifest`。

- [ ] **Step 3: 实现**

```rust
pub fn parse_version(kind: &str, stdout: &str) -> Option<String> {
    let strip = |s: &str| s.trim().trim_start_matches('v').to_string();
    match kind {
        // "sing-box version 1.13.19"
        "sing-box" => stdout.lines().next()?.split_whitespace().nth(2).map(strip),
        // "Xray 26.3.27 (Xray, Penetrates Everything.) …"
        "xray" => stdout.lines().next()?.split_whitespace().nth(1).map(strip),
        // banner 多行，取 "Version:\tv2.12.2"
        "hysteria" => stdout
            .lines()
            .find(|l| l.trim_start().starts_with("Version:"))
            .and_then(|l| l.split(':').nth(1))
            .map(strip),
        // "v2.10.2 h1:…"
        "caddy" => stdout.lines().next()?.split_whitespace().next().map(strip),
        _ => None,
    }
    .filter(|v| !v.is_empty())
}

pub fn installed_version(host: &dyn Host, bin_dir: &Path, kind: &str) -> Option<String> {
    let bin = bin_dir.join(kind);
    if host.read_file(&bin).ok().flatten().is_none() {
        return None;
    }
    let out = host.run(&bin.display().to_string(), &["version"]).ok()?;
    parse_version(kind, if out.stdout.is_empty() { &out.stderr } else { &out.stdout })
}

pub fn installed_versions(host: &dyn Host, bin_dir: &Path) -> BTreeMap<String, String> {
    KERNELS
        .iter()
        .filter_map(|k| installed_version(host, bin_dir, k).map(|v| (k.to_string(), v)))
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub fn asset_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => anyhow::bail!("不支持的架构：{other}（只支持 x86_64 / aarch64）"),
    }
}

pub fn kernels_key(name: &str) -> String {
    name.replace('-', "_")
}

pub fn manifest_url_for_version(version: &str) -> String {
    MANIFEST_URL_TEMPLATE.replace("{version}", version.trim_start_matches('v'))
}

pub fn resolve_manifest_url(cli_override: Option<&str>, version: Option<&str>, env: Option<&str>) -> String {
    match (cli_override, version, env) {
        (Some(u), _, _) if !u.is_empty() => u.to_string(),
        (_, Some(v), _) if !v.is_empty() => manifest_url_for_version(v),
        (_, _, Some(u)) if !u.is_empty() => u.to_string(),
        _ => MANIFEST_URL.to_string(),
    }
}

pub fn manifest_url(cli_override: Option<&str>, version: Option<&str>) -> String {
    let env = std::env::var(MANIFEST_URL_ENV).ok();
    resolve_manifest_url(cli_override, version, env.as_deref())
}

pub fn artifact_key(name: &str, arch: &str) -> anyhow::Result<String> {
    Ok(format!("{name}-linux-{}", asset_arch(arch)?))
}

impl Manifest {
    pub fn from_url(fetcher: &dyn Fetcher, url: &str) -> anyhow::Result<Manifest> {
        let bytes = fetcher.get_bytes(url)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn kernel_asset(&self, name: &str, arch: &str) -> anyhow::Result<(&str, &Asset)> {
        let key = artifact_key(name, arch)?;
        let version = self
            .kernels
            .get(&kernels_key(name))
            .ok_or_else(|| anyhow::anyhow!("manifest 的 kernels 表里没有 {name}"))?;
        let asset = self
            .artifacts
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("manifest 的 artifacts 表里没有 {key}"))?;
        Ok((version.as_str(), asset))
    }

    pub fn bui_asset(&self, arch: &str) -> anyhow::Result<&Asset> {
        let key = artifact_key("bui", arch)?;
        self.artifacts
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("manifest 的 artifacts 表里没有 {key}"))
    }
}

impl BinaryInstaller for KernelInstaller<'_> {
    fn install(&self, name: &str, version: &str, sha256: &str, url: &str, dest: &Path) -> anyhow::Result<()> {
        let bytes = self.fetcher.get_bytes(url)?;
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(sha256) {
            anyhow::bail!("{name} {version} 的 sha256 不匹配：期望 {sha256}，实际 {got}");
        }
        self.host.write_file(dest, &bytes, 0o755)?;
        tracing::info!(kernel = name, version, "内核二进制已更新");
        Ok(())
    }
}
```
`HttpFetcher`：
```rust
pub struct HttpFetcher {
    client: std::sync::OnceLock<reqwest::blocking::Client>,
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self { client: std::sync::OnceLock::new() }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for HttpFetcher {
    /// 只能在 spawn_blocking 线程或同步 CLI 路径里调用（见上面的铁律）。
    fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let client = self.client.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .user_agent(concat!("b-ui/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("rustls 客户端构建失败")
        });
        // Global Constraints：凭据不进日志。`BUI_MANIFEST_URL` / `--manifest-url` 允许带 basic auth
        // （`https://user:token@host/manifest.json`，私有 Release 或跳板机常这么给），所以**凡是把
        // url 放进 tracing 字段或错误串的地方都过 `crate::redact::url_credentials`**。
        // 这两处是 `url_credentials` 在 P1 的调用点（Task 17 收口删 `#![allow(dead_code)]` 时
        // 它就不再是死代码）。
        let resp = client.get(url).send().map_err(|e| {
            tracing::warn!(url = %crate::redact::url_credentials(url), error = %e, "下载失败");
            e
        })?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "下载 {} 失败：HTTP {}",
                crate::redact::url_credentials(url),
                resp.status().as_u16()
            );
        }
        Ok(resp.bytes()?.to_vec())
    }
}
```
客户端用 `OnceLock` 复用，避免每次下载都重建连接池。`get_bytes` 的**第一件事**是判本地路径（在建 client 之前）：

```rust
        // `file://…` 或不含 `://` 的入参按本地文件读（总纲 C4 的覆盖方式之一；M5 演练不依赖公开 Release）
        if let Some(path) = url.strip_prefix("file://").or_else(|| (!url.contains("://")).then_some(url)) {
            return std::fs::read(path).map_err(|e| anyhow::anyhow!("读取 {path} 失败：{e}"));
        }
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui kernels:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 9 passed（`parses_all_four…` / `installed_versions_skips…` / `manifest_round_trip…` / `manifest_url_resolution…` / `the_http_fetcher…` / `asset_arch_maps…` / `install_writes_0755…` / `install_refuses…` / `install_propagates…`）。

- [ ] **Step 5: Commit**

```bash
git add crates/bui/src/kernels Cargo.lock
git commit -m "feat(bui): 内核二进制管理（manifest 驱动的下载/校验/版本探测）"
```

---

### Task 10: 内核配置与 Caddyfile 模块（消费 `bui-schema` 渲染）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/core_files.rs`

**Interfaces:**
- Consumes（总纲 C1，签名必须逐字一致；路径按 P0 计划 Task 7 的实际定义——`RelayOpts` 在 `render/relay.rs` 里，不是 `render/mod.rs`）：
```rust
bui_schema::render::hysteria::{direct_yaml(&NodeParams, &Paths) -> String, residential_yaml(&NodeParams, &Paths) -> String}
bui_schema::render::xray::{config(&NodeParams, &[User], &Paths) -> serde_json::Value, structural_hash(&Value) -> String}
bui_schema::render::relay::{config(&ResidentialGroup, &RelayOpts) -> serde_json::Value,
                            RelayOpts { listen_port: u16, api: String, cache_path: String, server_ip: Option<String> }}
crate::kernels::{Manifest, KERNELS}
```
- Produces:
```rust
/// manifest 可在运行中被每日自检刷新（spec §7），所以存在共享锁里而不是值里。
pub struct CoreFilesModule { manifest: Arc<std::sync::RwLock<Option<Manifest>>> }
impl CoreFilesModule {
    pub fn new(manifest: Option<Manifest>) -> Self;
    /// 从共享锁构造（`serve::modules()` 用它，把同一个锁交给每日自检任务）
    pub fn with_handle(handle: Arc<std::sync::RwLock<Option<Manifest>>>) -> Self;
    pub fn manifest_handle(&self) -> Arc<std::sync::RwLock<Option<Manifest>>>;
}
impl Module for CoreFilesModule { fn name(&self) -> &'static str { "core-files" } /* render */ }
pub const RELAY_LISTEN_PORT: u16 = 2080;
pub const RELAY_CLASH_API: &str = "127.0.0.1:9091";
pub fn caddyfile_text(domain: &str, admin_port: u16) -> String;
pub fn relay_opts(state: &State, paths: &Paths) -> RelayOpts;
```
产出顺序（apply 里二进制在单元写入之后、重启之前落地，所以这个顺序安全）：
1. 四个 `Artifact::Binary`（`manifest` 为 `None` 时不产出——离线装机或 manifest 拉取失败时不阻塞对账）。
2. `Artifact::File` × 5：
   | 路径 | mode | verify | restart | restart_key |
   |---|---|---|---|---|
   | `<base>/config.yaml` | 0600 | 无（hysteria 没有校验命令，靠 P0 的 golden 测试兜底） | `Unit::restart("hysteria-server")` | 无 |
   | `<base>/config-residential.yaml` | 0600 | 无 | `Unit::restart("hysteria-residential")` | 无 |
   | `<base>/xray-config.json` | 0600 | `Verify::Xray` | `Unit::restart("xray")` | `structural_hash`（排除 `inbounds[].settings.clients`） |
   | `<base>/singbox-relay.json` | 0600 | `Verify::SingBox` | `Unit::restart("b-ui-relay")` | 无 |
   | `<base>/Caddyfile` | 0644 | `Verify::Caddy` | `Unit::reload("caddy")` | 无 |
   `auth-snapshot.json`（spec §2.2 的表里也有）**不是** artifact：`bui install` 写初版（Task 16），P2 的用户模块接手后在每次用户变更时原子重写，对账不比对它的内容（形状见文首契约段）。
   `relay_opts.cache_path` 指向 `<base>/relay-cache.db`（sing-box 运行时自建的文件，不是 artifact）——它已在 Task 5 的 `BASE_WHITELIST` 里，否则每轮对账都报一条 `stray_file`。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::reconcile::{Artifact, Facts, Module, RenderCtx, Unit, Verify};
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;

    fn ctx() -> RenderCtx {
        RenderCtx {
            paths: Paths::default_server(),
            facts: Facts {
                mem_mb: 2048, arch: "x86_64".into(), hostname: "node-a".into(),
                has_ufw: false, ufw_active: false, has_firewalld: false, firewalld_active: false,
                ssh_unit: "sshd".into(), ssh_pubkeys: 1, systemd_resolved: false,
            },
        }
    }

    /// 总纲 C4 形状（`kernels` 用下划线键、`artifacts` 用 `<name>-linux-<arch>` 键）
    fn manifest() -> Manifest {
        let asset = |u: &str| Asset { url: u.into(), sha256: "00".into() };
        Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([
                ("hysteria".to_string(), "2.12.2".to_string()),
                ("xray".to_string(), "26.3.27".to_string()),
                ("sing_box".to_string(), "1.13.19".to_string()),
                ("caddy".to_string(), "2.10.2".to_string()),
            ]),
            artifacts: BTreeMap::from([
                ("bui-linux-amd64".to_string(), asset("https://x/bui")),
                ("hysteria-linux-amd64".to_string(), asset("https://x/hy")),
                ("xray-linux-amd64".to_string(), asset("https://x/xray")),
                ("sing-box-linux-amd64".to_string(), asset("https://x/sb")),
                ("caddy-linux-amd64".to_string(), asset("https://x/caddy")),
            ]),
            min_upgrade_from: None,
        }
    }

    fn find_file(arts: &[Artifact], want: &str) -> Artifact {
        arts.iter()
            .find(|a| matches!(a, Artifact::File { path, .. } if path.to_str() == Some(want)))
            .cloned()
            .unwrap_or_else(|| panic!("没有渲染 {want}"))
    }

    #[test]
    fn renders_four_binaries_then_five_files() {
        let arts = CoreFilesModule::new(Some(manifest())).render(&sample_state(), &ctx());
        let bins: Vec<(String, String, String)> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::Binary { name, version, url, .. } => Some((name.clone(), version.clone(), url.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            bins,
            vec![
                ("hysteria".to_string(), "2.12.2".to_string(), "https://x/hy".to_string()),
                ("xray".to_string(), "26.3.27".to_string(), "https://x/xray".to_string()),
                ("sing-box".to_string(), "1.13.19".to_string(), "https://x/sb".to_string()),
                ("caddy".to_string(), "2.10.2".to_string(), "https://x/caddy".to_string()),
            ]
        );
        let files: Vec<String> = arts
            .iter()
            .filter_map(|a| match a {
                Artifact::File { path, .. } => Some(path.display().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            files,
            vec![
                "/opt/b-ui/config.yaml",
                "/opt/b-ui/config-residential.yaml",
                "/opt/b-ui/xray-config.json",
                "/opt/b-ui/singbox-relay.json",
                "/opt/b-ui/Caddyfile",
            ]
        );
    }

    #[test]
    fn without_a_manifest_only_the_files_are_rendered() {
        let arts = CoreFilesModule::new(None).render(&sample_state(), &ctx());
        assert!(!arts.iter().any(|a| matches!(a, Artifact::Binary { .. })));
        assert_eq!(arts.len(), 5);
    }

    #[test]
    fn hysteria_files_carry_the_schema_output_and_the_right_restart() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let expect = bui_schema::render::hysteria::direct_yaml(&s.node, &Paths::default_server());
        match find_file(&arts, "/opt/b-ui/config.yaml") {
            Artifact::File { content, mode, restart, verify, restart_key, immutable, .. } => {
                assert_eq!(String::from_utf8(content).unwrap(), expect, "必须原样用 bui-schema 的渲染结果");
                assert_eq!(mode, 0o600);
                assert_eq!(restart, Some(Unit::restart("hysteria-server")));
                assert_eq!(verify, None);
                assert_eq!(restart_key, None);
                assert!(!immutable);
            }
            other => panic!("{other:?}"),
        }
        match find_file(&arts, "/opt/b-ui/config-residential.yaml") {
            Artifact::File { content, restart, .. } => {
                assert_eq!(
                    String::from_utf8(content).unwrap(),
                    bui_schema::render::hysteria::residential_yaml(&s.node, &Paths::default_server())
                );
                assert_eq!(restart, Some(Unit::restart("hysteria-residential")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn xray_file_uses_the_structural_hash_as_its_restart_key() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let cfg = bui_schema::render::xray::config(&s.node, &s.users, &Paths::default_server());
        let hash = bui_schema::render::xray::structural_hash(&cfg);
        match find_file(&arts, "/opt/b-ui/xray-config.json") {
            Artifact::File { content, restart, restart_key, verify, .. } => {
                let got: serde_json::Value = serde_json::from_slice(&content).unwrap();
                assert_eq!(got, cfg);
                assert_eq!(restart, Some(Unit::restart("xray")));
                assert_eq!(restart_key, Some(hash));
                assert_eq!(verify, Some(Verify::Xray));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn adding_a_user_changes_the_content_but_not_the_restart_key() {
        let mut s = sample_state();
        let before = CoreFilesModule::new(None).render(&s, &ctx());
        let mut bob = s.users[0].clone();
        bob.username = "bob".into();
        bob.user_id = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-0000000000bb").unwrap();
        bob.credentials.vless_uuid = uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        s.users.push(bob);
        let after = CoreFilesModule::new(None).render(&s, &ctx());
        let key = |arts: &[Artifact]| match find_file(arts, "/opt/b-ui/xray-config.json") {
            Artifact::File { restart_key, content, .. } => (restart_key, content),
            other => panic!("{other:?}"),
        };
        let (k1, c1) = key(&before);
        let (k2, c2) = key(&after);
        assert_ne!(c1, c2, "clients 变了");
        assert_eq!(k1, k2, "结构没变 → 不重启 xray（eff-C7 的正解）");
    }

    #[test]
    fn relay_file_uses_the_fixed_local_ports_and_singbox_verify() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        let opts = relay_opts(&s, &Paths::default_server());
        assert_eq!(opts.listen_port, RELAY_LISTEN_PORT);
        assert_eq!(opts.api, RELAY_CLASH_API);
        assert_eq!(opts.cache_path, "/opt/b-ui/relay-cache.db");
        assert_eq!(opts.server_ip.as_deref(), Some("203.0.113.10"));
        let expect = bui_schema::render::relay::config(s.residential.default_group().unwrap(), &opts);
        match find_file(&arts, "/opt/b-ui/singbox-relay.json") {
            Artifact::File { content, restart, verify, mode, .. } => {
                assert_eq!(serde_json::from_slice::<serde_json::Value>(&content).unwrap(), expect);
                assert_eq!(restart, Some(Unit::restart("b-ui-relay")));
                assert_eq!(verify, Some(Verify::SingBox));
                assert_eq!(mode, 0o600);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn caddyfile_reverse_proxies_the_admin_port_and_reloads() {
        let s = sample_state();
        let arts = CoreFilesModule::new(None).render(&s, &ctx());
        match find_file(&arts, "/opt/b-ui/Caddyfile") {
            Artifact::File { content, mode, restart, verify, .. } => {
                let text = String::from_utf8(content).unwrap();
                assert!(text.contains("example.com {"));
                assert!(text.contains("reverse_proxy 127.0.0.1:8080"));
                assert!(text.contains("output stderr"), "日志进 journald，不再写 /var/log/caddy");
                assert_eq!(mode, 0o644);
                assert_eq!(restart, Some(Unit::reload("caddy")));
                assert_eq!(verify, Some(Verify::Caddy));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(caddyfile_text("example.com", 8080), caddyfile_text("example.com", 8080));
    }

    #[test]
    fn render_is_pure() {
        let s = sample_state();
        assert_eq!(
            CoreFilesModule::new(Some(manifest())).render(&s, &ctx()),
            CoreFilesModule::new(Some(manifest())).render(&s, &ctx())
        );
    }

    #[test]
    fn refreshing_the_shared_manifest_changes_the_next_render() {
        // 每日自检写这个锁，下一轮对账就按新版本装内核（spec §7）
        let m = CoreFilesModule::new(None);
        let s = sample_state();
        assert!(!m.render(&s, &ctx()).iter().any(|a| matches!(a, Artifact::Binary { .. })));
        *m.manifest_handle().write().unwrap() = Some(manifest());
        let bins = m
            .render(&s, &ctx())
            .into_iter()
            .filter(|a| matches!(a, Artifact::Binary { .. }))
            .count();
        assert_eq!(bins, 4);
    }

    /// P1 唯一自己渲染（不经 P0 golden 覆盖）的内核输入就是 Caddyfile，
    /// 所以拿真实 caddy 过一次 `validate`；二进制不在就 skip（Global Constraints）。
    #[test]
    fn caddyfile_passes_a_real_caddy_validate() {
        let caddy = ["/opt/b-ui/bin/caddy", "/usr/bin/caddy", "/usr/local/bin/caddy"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists());
        let Some(caddy) = caddy else {
            eprintln!("skipped: 本机没有 caddy 二进制");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("Caddyfile");
        std::fs::write(&f, caddyfile_text("example.com", 8080)).unwrap();
        // caddy validate 会 provision tls 模块，可能往 ~/.local/share/caddy 建目录；
        // 把两个 XDG 目录指到 tempdir，保持测试封闭（不碰开发机的家目录）
        let out = std::process::Command::new(caddy)
            .args(["validate", "--config", f.to_str().unwrap(), "--adapter", "caddyfile"])
            .env("XDG_DATA_HOME", d.path())
            .env("XDG_CONFIG_HOME", d.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "caddy validate 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::core_files`
Expected: 编译失败，`cannot find struct CoreFilesModule`。

- [ ] **Step 3: 实现**

```rust
use crate::kernels::{Manifest, KERNELS};
use crate::reconcile::{Artifact, Module, RenderCtx, Unit, Verify};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use bui_schema::render::relay::RelayOpts;
use std::sync::{Arc, RwLock};

pub const RELAY_LISTEN_PORT: u16 = 2080;
pub const RELAY_CLASH_API: &str = "127.0.0.1:9091";

/// 四个内核二进制 + 它们的五份配置（唯一来源是 bui-schema 的渲染器）。
/// manifest 放共享锁里：守护进程每日自检拉到新 manifest 后直接写这个锁，
/// 下一轮对账就按新版本装内核，不需要重启进程（spec §7）。
pub struct CoreFilesModule {
    manifest: Arc<RwLock<Option<Manifest>>>,
}

impl CoreFilesModule {
    pub fn new(manifest: Option<Manifest>) -> Self {
        Self { manifest: Arc::new(RwLock::new(manifest)) }
    }

    pub fn with_handle(handle: Arc<RwLock<Option<Manifest>>>) -> Self {
        Self { manifest: handle }
    }

    pub fn manifest_handle(&self) -> Arc<RwLock<Option<Manifest>>> {
        self.manifest.clone()
    }
}

pub fn relay_opts(state: &State, paths: &Paths) -> RelayOpts {
    RelayOpts {
        listen_port: RELAY_LISTEN_PORT,
        api: RELAY_CLASH_API.to_string(),
        cache_path: paths.base_dir.join("relay-cache.db").display().to_string(),
        server_ip: if state.node.public_ip.is_empty() { None } else { Some(state.node.public_ip.clone()) },
    }
}

pub fn caddyfile_text(domain: &str, admin_port: u16) -> String {
    format!(
        "\
# B-UI v4 —— 由 bui 对账器生成，手改会被覆盖
{domain} {{
\treverse_proxy 127.0.0.1:{admin_port}
\tlog {{
\t\toutput stderr
\t\tformat console
\t}}
}}
"
    )
}

impl Module for CoreFilesModule {
    fn name(&self) -> &'static str {
        "core-files"
    }

    fn render(&self, s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        let mut out = Vec::with_capacity(9);
        // 锁被别的线程 poison 也不能让对账崩：退化成「没有 manifest」
        let guard = self.manifest.read().ok();
        if let Some(m) = guard.as_ref().and_then(|g| g.as_ref()) {
            for name in KERNELS {
                match m.kernel_asset(name, &ctx.facts.arch) {
                    Ok((version, asset)) => out.push(Artifact::Binary {
                        name: name.to_string(),
                        version: version.to_string(),
                        sha256: asset.sha256.clone(),
                        url: asset.url.clone(),
                    }),
                    Err(e) => tracing::warn!(kernel = name, error = %e, "manifest 缺该内核资产，跳过"),
                }
            }
        }
        let p = &ctx.paths;
        out.push(
            Artifact::file(p.base_dir.join("config.yaml"), bui_schema::render::hysteria::direct_yaml(&s.node, p))
                .restart(Unit::restart("hysteria-server")),
        );
        out.push(
            Artifact::file(
                p.base_dir.join("config-residential.yaml"),
                bui_schema::render::hysteria::residential_yaml(&s.node, p),
            )
            .restart(Unit::restart("hysteria-residential")),
        );
        let xray = bui_schema::render::xray::config(&s.node, &s.users, p);
        let hash = bui_schema::render::xray::structural_hash(&xray);
        out.push(
            Artifact::file(
                p.base_dir.join("xray-config.json"),
                serde_json::to_vec_pretty(&xray).expect("xray 配置必须可序列化"),
            )
            .verify(Verify::Xray)
            .restart(Unit::restart("xray"))
            .restart_key(hash),
        );
        let group = s.residential.default_group().cloned().unwrap_or_default();
        let relay = bui_schema::render::relay::config(&group, &relay_opts(s, p));
        out.push(
            Artifact::file(
                p.base_dir.join("singbox-relay.json"),
                serde_json::to_vec_pretty(&relay).expect("relay 配置必须可序列化"),
            )
            .verify(Verify::SingBox)
            .restart(Unit::restart("b-ui-relay")),
        );
        out.push(
            Artifact::file(crate::paths::caddyfile(p), caddyfile_text(&s.node.domain, s.node.ports.admin))
                .mode(0o644)
                .verify(Verify::Caddy)
                .restart(Unit::reload("caddy")),
        );
        out
    }
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test -p bui modules::core_files -- --nocapture && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 10 passed（本机装了 caddy 时 `caddyfile_passes_a_real_caddy_validate` 真跑；没装则打印 `skipped: 本机没有 caddy 二进制` 仍算 passed）。

- [ ] **Step 5: Commit**

```bash
git add crates/bui/src/modules/core_files.rs
git commit -m "feat(bui): 内核配置与 Caddyfile 模块（复用 bui-schema 渲染 + 结构哈希重启键）"
```

---

### Task 11: 证书 inotify 监听与两个 hysteria 的错峰重启

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/certs.rs`

**Interfaces:**
- Consumes: `crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx}`（Task 4）、`crate::api::EventBus`（Task 4 的 `api/state.rs`）、`crate::sys::Host`（Task 3）、`crate::paths::caddy_data`（Task 1）、`crate::kernels::sha256_hex`（Task 9）
- Produces:
```rust
pub struct CertsModule;
impl Module for CertsModule { fn name(&self) -> &'static str { "certs" } /* render 返回空 Vec；spawn 见下 */ }
/// 两个 hysteria 之间的重启间隔（spec §3.4「间隔 10 秒依次重启两个 hysteria」）
pub const RESTART_GAP_SECS: u64 = 10;
/// 已经有证书之后的兜底轮询间隔（inotify 漏事件时的保险）
pub const FALLBACK_POLL_SECS: u64 = 21600;   // 6h
/// **还没拿到首张证书**时的轮询间隔：全新装机时 Caddy 的 `certificates/` 目录还不存在，
/// inotify 完全收不到事件，这时必须是秒级而不是 6h（见下面「B6」一段）
pub const PENDING_POLL_SECS: u64 = 30;
/// 本轮兜底轮询该等多久：没证书 30s，有证书回到 6h（纯函数，可直接断言）
pub fn poll_interval(cert_ready: bool) -> std::time::Duration;
/// 兜底轮询循环（与 inotify 任务并行跑；`CertsModule::spawn` 同时起这两个）。
/// 域名每轮从 `ctx.store` 现读（与 `watch_loop` 一致），所以签名里不带 domain
pub async fn poll_loop(ctx: DaemonCtx);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertPair { pub cert: PathBuf, pub key: PathBuf }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertAction { NotReady, UpToDate, Copy { sha256: String } }
/// 在 Caddy 数据目录里找 <domain> 的证书对（移植 core.sh:952-963 的两段查找）
pub fn find_cert(host: &dyn Host, caddy_data: &Path, domain: &str) -> Option<CertPair>;
/// 与 runtime 里记的指纹比较，决定要不要复制
pub fn decide(host: &dyn Host, pair: &CertPair, known_sha256: Option<&str>) -> anyhow::Result<CertAction>;
/// fullchain.pem 0644 / privkey.pem 0600
pub fn copy_pair(host: &dyn Host, pair: &CertPair, certs_dir: &Path) -> anyhow::Result<()>;
/// 同步一轮：找证书 → 判断 → 复制 → 错峰重启两个 hysteria。返回新指纹（无变化则 None）
pub async fn sync_once(ctx: &DaemonCtx, domain: &str) -> anyhow::Result<Option<String>>;
pub async fn watch_loop(ctx: DaemonCtx);
```
移植参照：`server/core.sh:923-1040`（`setup_cert_sync` 的脚本正文：查找证书目录、`cmp` 比对、`cp` + chmod、重启两个 hysteria）。v4 的变化（审计 §3.1「证书同步三重触发 → 合并」）：v4 既不写 `cert-sync.sh`、也不建 timer/cron，改成守护进程里的 inotify 任务 + 兜底轮询任务；比对从 `cmp` 改成 sha256 指纹存 `runtime.cert_sha256`；两个 hysteria 之间加 10 秒间隔（v3 是连续 restart，曾造成两实例同时失联）。

**全新装机的首张证书不能靠 6h 兜底（B6，本次修订）**：`bui install` 刚跑完时 `<base>/caddy/caddy/certificates` 这个目录**还不存在**（Caddy 拿到证书后才建 `certificates/<issuer>/<domain>/`），`watches.add(dir)` 直接 ENOENT；即便目录存在，事件掩码也只在 `certificates/` 这一层生效，Caddy 随后新建的 issuer / domain 子目录不在 watch 里，往里写文件不会触发任何事件。于是两个 hysteria 会因为缺 `<base>/certs/fullchain.pem` 崩溃循环最长 6 小时（watchdog 判「进程不在」交给 systemd，也救不了），spec §7 的全新 `bui install` 交付不可用。三处必改，缺一条这个坑就还在：
1. 建 watch **之前** `std::fs::create_dir_all(&dir)`（目录属于我们的 XDG 数据目录，先建出来 Caddy 照用）；
2. 每次事件之后重新 `walk_dirs(&dir)` 把新出现的子目录加进 watch（Caddy 的 issuer/domain 子目录是事后才有的）；
3. `runtime.cert_sha256` 还是 `None`（= 一张证书都没拿到）期间，兜底轮询用 `PENDING_POLL_SECS`（30s）而不是 6h，拿到证书后再回到 6h——这一条由独立的 `poll_loop` 任务承担，不依赖 inotify 是否建成功。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::reconcile::DaemonCtx;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::{fake::FakeHost, Host};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    const ACME: &str = "/opt/b-ui/caddy/caddy/certificates/acme-v02.api.letsencrypt.org-directory/example.com";

    fn seed(h: &FakeHost, cert: &str, key: &str) {
        h.with(|i| {
            i.files.insert(format!("{ACME}/example.com.crt").into(), (cert.as_bytes().to_vec(), 0o600));
            i.files.insert(format!("{ACME}/example.com.key").into(), (key.as_bytes().to_vec(), 0o600));
        });
    }

    #[test]
    fn finds_the_cert_pair_under_the_acme_directory() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let pair = find_cert(&h, &crate::paths::caddy_data(&Paths::default_server()), "example.com").unwrap();
        assert_eq!(pair.cert.to_str().unwrap(), format!("{ACME}/example.com.crt"));
        assert_eq!(pair.key.to_str().unwrap(), format!("{ACME}/example.com.key"));
    }

    #[test]
    fn missing_cert_is_not_an_error() {
        let h = FakeHost::new();
        assert_eq!(find_cert(&h, &crate::paths::caddy_data(&Paths::default_server()), "example.com"), None);
    }

    #[test]
    fn decide_compares_against_the_known_fingerprint() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let pair = find_cert(&h, &crate::paths::caddy_data(&Paths::default_server()), "example.com").unwrap();
        let sum = crate::kernels::sha256_hex(b"CERT");
        assert_eq!(decide(&h, &pair, None).unwrap(), CertAction::Copy { sha256: sum.clone() });
        assert_eq!(decide(&h, &pair, Some(&sum)).unwrap(), CertAction::UpToDate);
        assert_eq!(decide(&h, &pair, Some("stale")).unwrap(), CertAction::Copy { sha256: sum });
    }

    #[test]
    fn copy_pair_writes_644_and_600() {
        let h = FakeHost::new();
        seed(&h, "CERT", "KEY");
        let paths = Paths::default_server();
        let pair = find_cert(&h, &crate::paths::caddy_data(&paths), "example.com").unwrap();
        copy_pair(&h, &pair, &paths.certs_dir).unwrap();
        assert_eq!(h.text("/opt/b-ui/certs/fullchain.pem").unwrap(), "CERT");
        assert_eq!(h.text("/opt/b-ui/certs/privkey.pem").unwrap(), "KEY");
        assert_eq!(h.mode("/opt/b-ui/certs/fullchain.pem"), Some(0o644));
        assert_eq!(h.mode("/opt/b-ui/certs/privkey.pem"), Some(0o600));
    }

    async fn ctx_with(host: Arc<FakeHost>) -> (DaemonCtx, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::default_server();
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state()).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        (DaemonCtx { store, runtime, bus: EventBus::new(), host, paths }, d)
    }

    #[tokio::test(start_paused = true)]
    async fn sync_restarts_both_hysteria_with_a_ten_second_gap() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
        });
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        let start = tokio::time::Instant::now();
        let sum = sync_once(&ctx, "example.com").await.unwrap().unwrap();
        assert_eq!(sum, crate::kernels::sha256_hex(b"CERT"));
        assert!(tokio::time::Instant::now().duration_since(start).as_secs() >= RESTART_GAP_SECS);
        let ops = host.ops();
        let i1 = ops.iter().position(|o| o == "systemd:restart:hysteria-server").unwrap();
        let i2 = ops.iter().position(|o| o == "systemd:restart:hysteria-residential").unwrap();
        assert!(i1 < i2, "先直连后住宅");
        assert_eq!(ctx.runtime.read().await.cert_sha256.as_deref(), Some(sum.as_str()));
    }

    #[tokio::test(start_paused = true)]
    async fn second_sync_is_a_no_op() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
        });
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        sync_once(&ctx, "example.com").await.unwrap();
        host.clear_ops();
        assert_eq!(sync_once(&ctx, "example.com").await.unwrap(), None);
        assert!(host.ops().is_empty(), "证书没变不重启");
    }

    #[tokio::test(start_paused = true)]
    async fn inactive_instances_are_not_started_by_the_cert_sync() {
        let host = Arc::new(FakeHost::new());   // 两个单元都不 active
        seed(&host, "CERT", "KEY");
        let (ctx, _d) = ctx_with(host.clone()).await;
        sync_once(&ctx, "example.com").await.unwrap().unwrap();
        assert!(
            !host.ops().iter().any(|o| o.starts_with("systemd:restart")),
            "没在跑的实例交给 systemd，不由证书同步拉起（移植 core.sh:1000-1006）"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_certificate_is_picked_up_in_seconds_not_six_hours() {
        // B6：全新装机时 certificates/ 目录都还不存在，inotify 收不到任何事件；
        // 兜底轮询必须是 30 秒级，否则两个 hysteria 因缺 fullchain.pem 崩溃循环最长 6 小时。
        assert_eq!(poll_interval(false), std::time::Duration::from_secs(PENDING_POLL_SECS));
        assert_eq!(poll_interval(true), std::time::Duration::from_secs(FALLBACK_POLL_SECS));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-server.service".into());
            i.units_active.insert("hysteria-residential.service".into());
        });
        let (ctx, _d) = ctx_with(host.clone()).await;
        let task = tokio::spawn(poll_loop(ctx.clone())); // sample_state 的域名就是 example.com
        tokio::time::sleep(std::time::Duration::from_secs(PENDING_POLL_SECS + 5)).await;
        assert!(ctx.runtime.read().await.cert_sha256.is_none(), "证书还没签出来，这一轮什么都不做");
        seed(&host, "CERT", "KEY"); // Caddy 这时才把证书写出来
        tokio::time::sleep(std::time::Duration::from_secs(PENDING_POLL_SECS + RESTART_GAP_SECS + 5)).await;
        assert_eq!(
            ctx.runtime.read().await.cert_sha256.as_deref(),
            Some(crate::kernels::sha256_hex(b"CERT").as_str()),
            "下一轮 30 秒轮询就该复制并记指纹"
        );
        assert!(host.ops().iter().any(|o| o == "write:/opt/b-ui/certs/fullchain.pem:644"));
        task.abort();
    }

    /// 证书文件不是渲染产物（Caddy 签、我们只复制），所以 render 必须为空；
    /// 本文件自带一个 RenderCtx 构造器，不跨模块借用别的任务的测试辅助。
    #[test]
    fn module_renders_nothing() {
        let ctx = RenderCtx {
            paths: Paths::default_server(),
            facts: crate::reconcile::Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 0,
                systemd_resolved: false,
            },
        };
        assert_eq!(CertsModule.render(&crate::testutil::sample_state(), &ctx), vec![]);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::certs`
Expected: 编译失败，`cannot find struct CertsModule`。

- [ ] **Step 3: 实现纯逻辑部分**

```rust
pub fn find_cert(host: &dyn Host, caddy_data: &Path, domain: &str) -> Option<CertPair> {
    let acme = caddy_data
        .join("certificates")
        .join("acme-v02.api.letsencrypt.org-directory")
        .join(domain);
    let candidates = [acme.clone(), caddy_data.join("certificates").join(domain)];
    for dir in candidates {
        let cert = dir.join(format!("{domain}.crt"));
        let key = dir.join(format!("{domain}.key"));
        let have = |p: &Path| host.read_file(p).ok().flatten().is_some();
        if have(&cert) && have(&key) {
            return Some(CertPair { cert, key });
        }
    }
    None
}

pub fn decide(host: &dyn Host, pair: &CertPair, known: Option<&str>) -> anyhow::Result<CertAction> {
    let Some(bytes) = host.read_file(&pair.cert)? else { return Ok(CertAction::NotReady) };
    let sha256 = crate::kernels::sha256_hex(&bytes);
    Ok(if known == Some(sha256.as_str()) { CertAction::UpToDate } else { CertAction::Copy { sha256 } })
}

pub fn copy_pair(host: &dyn Host, pair: &CertPair, certs_dir: &Path) -> anyhow::Result<()> {
    let cert = host.read_file(&pair.cert)?.ok_or_else(|| anyhow::anyhow!("证书消失了"))?;
    let key = host.read_file(&pair.key)?.ok_or_else(|| anyhow::anyhow!("私钥消失了"))?;
    host.write_file(&certs_dir.join("fullchain.pem"), &cert, 0o644)?;
    host.write_file(&certs_dir.join("privkey.pem"), &key, 0o600)?;
    Ok(())
}
```

- [ ] **Step 4: 实现 `sync_once` 与 `watch_loop`**

```rust
pub async fn sync_once(ctx: &DaemonCtx, domain: &str) -> anyhow::Result<Option<String>> {
    let known = ctx.runtime.read().await.cert_sha256;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let domain_owned = domain.to_string();
    let action = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let Some(pair) = find_cert(host.as_ref(), &crate::paths::caddy_data(&paths), &domain_owned) else {
            return Ok((CertAction::NotReady, None));
        };
        let action = decide(host.as_ref(), &pair, known.as_deref())?;
        if let CertAction::Copy { .. } = &action {
            copy_pair(host.as_ref(), &pair, &paths.certs_dir)?;
        }
        Ok((action, Some(pair)))
    })
    .await??;
    let CertAction::Copy { sha256 } = action.0 else { return Ok(None) };
    ctx.runtime.update(|r| r.cert_sha256 = Some(sha256.clone())).await;
    // hysteria 只在启动时读证书（CanReload=no），两个实例共用同一份，都要重启；
    // 间隔 10 秒，避免两条线路同时失联。
    for (i, unit) in ["hysteria-server", "hysteria-residential"].iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(RESTART_GAP_SECS)).await;
        }
        let host = ctx.host.clone();
        let u = unit.to_string();
        let restarted = tokio::task::spawn_blocking(move || {
            if host.unit_is_active(&u).unwrap_or(false) {
                host.systemd("restart", &u).map(|o| o.ok()).unwrap_or(false)
            } else {
                false
            }
        })
        .await?;
        if restarted {
            tracing::info!(unit = *unit, "证书更新后已重启");
        }
    }
    Ok(Some(sha256))
}

pub fn poll_interval(cert_ready: bool) -> std::time::Duration {
    std::time::Duration::from_secs(if cert_ready { FALLBACK_POLL_SECS } else { PENDING_POLL_SECS })
}

/// 兜底轮询：没证书时 30 秒一轮（全新装机的首张证书全靠它），拿到后 6 小时一轮。
/// 与 inotify 无关——inotify 在目录还不存在或子目录后建时都可能一个事件都收不到。
pub async fn poll_loop(ctx: DaemonCtx) {
    loop {
        let ready = ctx.runtime.read().await.cert_sha256.is_some();
        tokio::time::sleep(poll_interval(ready)).await;
        let domain = ctx.store.read().await.node.domain.clone();
        if let Err(e) = sync_once(&ctx, &domain).await {
            tracing::warn!(error = %e, "兜底轮询的证书同步失败");
        }
    }
}

pub async fn watch_loop(ctx: DaemonCtx) {
    let domain = ctx.store.read().await.node.domain.clone();
    if let Err(e) = sync_once(&ctx, &domain).await {
        tracing::warn!(error = %e, "启动时证书同步失败");
    }
    let dir = crate::paths::caddy_data(&ctx.paths).join("certificates");
    // 全新装机时这个目录还不存在（Caddy 签到证书才建）；不先建出来，watches.add 会 ENOENT，
    // 整个监听退化成兜底轮询（B6）。目录在我们自己的 XDG 数据目录里，先建无害。
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "创建证书目录失败");
    }
    let mut watcher = match CertWatcher::open(&dir) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "inotify 建立失败，只靠兜底轮询");
            None
        }
    };
    loop {
        match watcher.as_mut() {
            Some(w) => {
                if w.stream.next().await.is_none() {
                    tracing::warn!("inotify 流结束，只靠兜底轮询");
                    watcher = None;
                    continue;
                }
                // Caddy 事后才建 issuer/domain 子目录，新目录要补进 watch，否则里面的写入无事件
                w.rescan();
            }
            // inotify 用不了：这条任务直接退出，证书同步交给 poll_loop（30s / 6h）
            None => return,
        }
        // Caddy 写证书是多文件操作，等 2 秒收敛再同步
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let domain = ctx.store.read().await.node.domain.clone();
        if let Err(e) = sync_once(&ctx, &domain).await {
            tracing::warn!(error = %e, "证书同步失败");
        }
    }
}

/// 持有 inotify 句柄与 watch 集合，`rescan` 把新出现的子目录补进去。
struct CertWatcher {
    root: PathBuf,
    watches: inotify::Watches,
    stream: inotify::EventStream<[u8; 1024]>,
    watched: std::collections::BTreeSet<PathBuf>,
}

impl CertWatcher {
    fn open(dir: &Path) -> anyhow::Result<Self> {
        use inotify::Inotify;
        let inotify = Inotify::init()?;
        let watches = inotify.watches();
        let mut me = Self {
            root: dir.to_path_buf(),
            watches,
            stream: inotify.into_event_stream([0u8; 1024])?,
            watched: std::collections::BTreeSet::new(),
        };
        me.add(&me.root.clone())?;
        me.rescan();
        Ok(me)
    }

    fn mask() -> inotify::WatchMask {
        inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO | inotify::WatchMask::CREATE
    }

    fn add(&mut self, dir: &Path) -> anyhow::Result<()> {
        self.watches.add(dir, Self::mask())?;
        self.watched.insert(dir.to_path_buf());
        Ok(())
    }

    fn rescan(&mut self) {
        for d in walk_dirs(&self.root) {
            if !self.watched.contains(&d) {
                if let Err(e) = self.add(&d) {
                    tracing::warn!(error = %e, dir = %d.display(), "补加 watch 失败");
                }
            }
        }
    }
}
```
`walk_dirs` 是本文件内的两层目录枚举（`std::fs::read_dir`，只收目录，深度 2，足够覆盖 `certificates/<issuer>/<domain>`）；`futures_util::StreamExt::next` 用于 `stream.next()`。`CertsModule` 的 `Module` 实现：`name()` 返回 `"certs"`（Task 15 的 `p1_registers_exactly_six_modules…` 按这个名字断言），`render` 返回空 `Vec`，`spawn` 返回**两个**任务：`vec![tokio::spawn(watch_loop(ctx.clone())), tokio::spawn(poll_loop(ctx))]`（两个循环各自从 `ctx.store` 读域名，所以 `spawn` 这个同步函数里不需要 await）。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::certs && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 9 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/certs.rs
git commit -m "feat(bui): 证书 inotify 监听与两个 hysteria 的错峰重启"
```

---

### Task 12: watchdog（存活 + 端口探测 + 退避）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/modules/watchdog.rs`

**Interfaces:**
- Consumes: `crate::sys::{Host, Proto}`（Task 3）、`crate::state::runtime::WatchdogRecord`（Task 2）、`crate::reconcile::{DaemonCtx, Module}` 与 `crate::api::EventBus`（Task 4）、`crate::modules::core_files::RELAY_LISTEN_PORT`（Task 10）、`crate::util::{fmt_rfc3339, parse_rfc3339}`（Task 1）
- Produces:
```rust
pub struct WatchdogModule;
impl Module for WatchdogModule { fn name(&self) -> &'static str { "watchdog" } /* render 返回空 Vec */ }
pub const INTERVAL_SECS: u64 = 60;
pub const FAIL_THRESHOLD: u32 = 2;
/// 退避 1/2/4 分钟（spec §3.4）
pub const BACKOFF_MINUTES: [i64; 3] = [1, 2, 4];
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target { pub unit: String, pub proto: Proto, pub port: u16 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision { Healthy, Failing { fails: u32 }, Restart, Backoff }
/// 四个内核 + 各自的监听端口（relay 与 xray 是 TCP，两个 hysteria 是 UDP）
pub fn targets(state: &State) -> Vec<Target>;
/// 纯状态机：返回决定并就地更新记录
pub fn decide(rec: &mut WatchdogRecord, alive: bool, listening: bool, now: OffsetDateTime) -> Decision;
pub async fn check_once(ctx: &DaemonCtx) -> anyhow::Result<Vec<(String, Decision)>>;
pub async fn watch_loop(ctx: DaemonCtx);
```
移植参照：`server/core.sh:1050-1130`（`setup_hy2_watchdog`：进程在但 UDP 端口失活时重启，连续 3 次触发，`/tmp` 计数文件）。v4 的变化（审计 §3.1/§3.2 与 web-C12）：不再写脚本、不再建 timer、不再用 `/tmp/hy2-watchdog-*` 计数文件（web-C12：面板还在读那个文件），改成守护进程里 60 秒一轮的内存状态机 + `runtime.json` 持久化；覆盖面从两个 hysteria 扩到四个内核；阈值 2 次（60s×2）+ 退避 1/2/4 分钟。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::state::runtime::{Runtime, WatchdogRecord};
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    #[test]
    fn targets_cover_four_kernels_with_the_right_protocol() {
        assert_eq!(
            targets(&sample_state()),
            vec![
                Target { unit: "hysteria-server".into(), proto: Proto::Udp, port: 10000 },
                Target { unit: "hysteria-residential".into(), proto: Proto::Udp, port: 40000 },
                Target { unit: "xray".into(), proto: Proto::Tcp, port: 10001 },
                Target { unit: "b-ui-relay".into(), proto: Proto::Tcp, port: 2080 },
            ]
        );
    }

    #[test]
    fn healthy_resets_the_counter() {
        let mut rec = WatchdogRecord { fails: 1, restarts: 3, last_restart_at: None, backoff_until: None };
        assert_eq!(decide(&mut rec, true, true, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.restarts, 3, "重启次数是累计值，不清零");
    }

    #[test]
    fn dead_process_is_left_to_systemd() {
        let mut rec = WatchdogRecord::default();
        // 进程不在 → systemd Restart=always 会管，watchdog 只清计数（移植 core.sh:1065-1068）
        assert_eq!(decide(&mut rec, false, false, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
    }

    #[test]
    fn two_consecutive_half_dead_rounds_trigger_a_restart_then_backoff() {
        let mut rec = WatchdogRecord::default();
        assert_eq!(decide(&mut rec, true, false, t0()), Decision::Failing { fails: 1 });
        let r = decide(&mut rec, true, false, t0() + time::Duration::seconds(60));
        assert_eq!(r, Decision::Restart);
        assert_eq!(rec.restarts, 1);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.last_restart_at.as_deref(), Some("2026-09-11T00:01:00Z"));
        assert_eq!(rec.backoff_until.as_deref(), Some("2026-09-11T00:02:00Z"), "第一次退避 1 分钟");
        // 退避窗口内即使再连续两轮失败也不重启
        assert_eq!(decide(&mut rec, true, false, t0() + time::Duration::seconds(70)), Decision::Failing { fails: 1 });
        assert_eq!(decide(&mut rec, true, false, t0() + time::Duration::seconds(80)), Decision::Backoff);
        assert_eq!(rec.restarts, 1);
    }

    #[test]
    fn backoff_grows_one_two_four_then_stays_at_four() {
        let mut rec = WatchdogRecord::default();
        let mut now = t0();
        let mut seen = Vec::new();
        for _ in 0..4 {
            // 每轮：两次失败触发一次重启，然后跳过退避窗口
            decide(&mut rec, true, false, now);
            now += time::Duration::seconds(60);
            assert_eq!(decide(&mut rec, true, false, now), Decision::Restart);
            let until = time::OffsetDateTime::parse(
                rec.backoff_until.as_deref().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            seen.push((until - now).whole_minutes());
            now = until + time::Duration::seconds(1);
        }
        assert_eq!(seen, vec![1, 2, 4, 4]);
        assert_eq!(rec.restarts, 4);
    }

    async fn ctx(host: Arc<FakeHost>) -> (crate::reconcile::DaemonCtx, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(d.path().join("state.json"), sample_state()).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        (
            crate::reconcile::DaemonCtx { store, runtime, bus: EventBus::new(), host, paths: Paths::default_server() },
            d,
        )
    }

    #[tokio::test]
    async fn check_once_restarts_only_the_half_dead_unit() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in ["hysteria-server", "hysteria-residential", "xray", "b-ui-relay"] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening.insert(Proto::Udp, [40000].into_iter().collect());   // 10000 失活
            i.listening.insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        let first = check_once(&c).await.unwrap();
        assert_eq!(first[0], ("hysteria-server".to_string(), Decision::Failing { fails: 1 }));
        assert_eq!(first[1], ("hysteria-residential".to_string(), Decision::Healthy));
        assert!(!host.ops().iter().any(|o| o.starts_with("systemd:restart")));
        host.advance(60);
        let second = check_once(&c).await.unwrap();
        assert_eq!(second[0], ("hysteria-server".to_string(), Decision::Restart));
        assert_eq!(host.ops(), vec!["systemd:restart:hysteria-server"]);
        let rt = c.runtime.read().await;
        assert_eq!(rt.watchdog["hysteria-server"].restarts, 1);
        assert_eq!(rt.watchdog["xray"].fails, 0);
    }
}
```
`check_once` 里读 `listening_ports` 与 `unit_is_active` 都经 `Host`，`now` 也取 `host.now()`，所以上面的 `host.advance(60)` 能推进状态机。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui modules::watchdog`
Expected: 编译失败，`cannot find function targets`。

- [ ] **Step 3: 实现状态机**

```rust
pub fn targets(state: &State) -> Vec<Target> {
    vec![
        Target { unit: "hysteria-server".into(), proto: Proto::Udp, port: state.node.ports.hy2 },
        Target { unit: "hysteria-residential".into(), proto: Proto::Udp, port: state.node.ports.hy2_resi },
        Target { unit: "xray".into(), proto: Proto::Tcp, port: state.node.ports.reality_direct },
        Target { unit: "b-ui-relay".into(), proto: Proto::Tcp, port: crate::modules::core_files::RELAY_LISTEN_PORT },
    ]
}

pub fn decide(rec: &mut WatchdogRecord, alive: bool, listening: bool, now: OffsetDateTime) -> Decision {
    // 进程不在 → systemd 的 Restart=always 负责，watchdog 不插手（core.sh:1065-1068）。
    // **这与 spec §3.4「检查四个内核进程存活 + 监听端口」的字面读法不同，是有意为之**（v3 同语义）：
    // watchdog 只解决「进程还在、端口却不 listen」这一类僵死；`!alive` 交给 systemd，重复插手会和
    // `Restart=always` 抢着重启。单元不 active 这件事本身由 `/api/health` 的 services 报 degraded，
    // 不会被吞掉。M5 验收时按这一段口径核对，不要按 spec 字面要求 watchdog 去 start 单元。
    if !alive || listening {
        rec.fails = 0;
        return Decision::Healthy;
    }
    rec.fails += 1;
    if rec.fails < FAIL_THRESHOLD {
        return Decision::Failing { fails: rec.fails };
    }
    if let Some(until) = rec.backoff_until.as_deref().and_then(parse_rfc3339) {
        if now < until {
            return Decision::Backoff;
        }
    }
    rec.fails = 0;
    rec.restarts += 1;
    let minutes = BACKOFF_MINUTES[(rec.restarts as usize - 1).min(BACKOFF_MINUTES.len() - 1)];
    rec.last_restart_at = Some(fmt_rfc3339(now));
    rec.backoff_until = Some(fmt_rfc3339(now + time::Duration::minutes(minutes)));
    Decision::Restart
}
```
`fmt_rfc3339` / `parse_rfc3339` 用 Task 1 的 `crate::util::{fmt_rfc3339, parse_rfc3339}`（本文件 `use crate::util::{fmt_rfc3339, parse_rfc3339};`，不再重复实现）。

- [ ] **Step 4: 实现 `check_once` 与 `watch_loop`**

```rust
pub async fn check_once(ctx: &DaemonCtx) -> anyhow::Result<Vec<(String, Decision)>> {
    let state = ctx.store.read().await;
    let targets = targets(&state);
    let mut records = ctx.runtime.read().await.watchdog;
    let host = ctx.host.clone();
    let (decisions, records) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let now = host.now();
        let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
        let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
        let mut out = Vec::with_capacity(targets.len());
        for t in &targets {
            let rec = records.entry(t.unit.clone()).or_default();
            let alive = host.unit_is_active(&t.unit).unwrap_or(false);
            let listening = match t.proto {
                Proto::Udp => udp.contains(&t.port),
                Proto::Tcp => tcp.contains(&t.port),
            };
            let d = decide(rec, alive, listening, now);
            if d == Decision::Restart {
                tracing::warn!(unit = %t.unit, port = t.port, "监听失活连续 2 轮，重启");
                let _ = host.systemd("restart", &t.unit);
            }
            out.push((t.unit.clone(), d));
        }
        Ok((out, records))
    })
    .await??;
    ctx.runtime.update(|r| r.watchdog = records).await;
    Ok(decisions)
}

pub async fn watch_loop(ctx: DaemonCtx) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = check_once(&ctx).await {
            tracing::warn!(error = %e, "watchdog 一轮检查失败");
        }
    }
}
```
`WatchdogModule` 的 `Module` 实现：`render` 返回空 `Vec`，`spawn` 返回 `vec![tokio::spawn(watch_loop(ctx))]`。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui modules::watchdog && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 6 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/modules/watchdog.rs
git commit -m "feat(bui): 进程内 watchdog（四内核存活+端口探测、2 次阈值、1/2/4 分钟退避）"
```

---

### Task 13: API 实现（`/api/login` 限速与 JWT、`/api/health`、CLI 用的两个系统端点）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/api/auth.rs`, `crates/bui/src/api/health.rs`, `crates/bui/src/api/system.rs`
- Modify: `crates/bui/src/api/mod.rs`（在 Task 4 写的 `pub use` 后追加 `router()` 与本文件的测试模块）、`crates/bui/src/api/state.rs`（给 `AppState` 加一个字段 `pub login: auth::LoginLimiter`）

**Interfaces:**
- Consumes: `crate::api::{AppState, Event, EventBus}`（Task 4 已定义）、`crate::state::store::Store`、`crate::state::runtime::{Runtime, DriftItem, ReconcileReport, WatchdogRecord}`、`crate::sys::Host`、`crate::reconcile::{Module, MANAGED_UNITS}`、`crate::util::fmt_rfc3339`
- Produces:
```rust
// crate::api（mod.rs）
/// 基础 Router：公开 /api/login，其余走 require_admin；再 merge 各模块的 routes()
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router;
// crate::api::state（本任务只加一个字段）
pub struct AppState { /* Task 4 的六个字段 */ pub login: crate::api::auth::LoginLimiter }
// crate::api::auth
pub const TOKEN_HOURS: i64 = 24;
pub const LOGIN_MAX_ATTEMPTS: u32 = 5;
pub const LOGIN_WINDOW_SECS: i64 = 60;
#[derive(Debug, Serialize, Deserialize)] pub struct Claims { pub sub: String, pub admin: bool, pub exp: i64 }
pub fn hash_password(pw: &str) -> anyhow::Result<String>;
pub fn verify_password(hash: &str, pw: &str) -> bool;
pub fn issue_token(secret: &str, now: OffsetDateTime) -> anyhow::Result<(String, String)>;   // (token, expires_at)
pub fn decode_token(secret: &str, token: &str, now: OffsetDateTime) -> Option<Claims>;
/// 登录限速器：**每个 router 实例一份**，作为 `AppState` 的字段随 `AppState` 克隆
#[derive(Clone, Default)] pub struct LoginLimiter { /* Arc<Mutex<HashMap<String,(OffsetDateTime,u32)>>> */ }
impl LoginLimiter { pub fn allow(&self, ip: &str, now: OffsetDateTime) -> bool; pub fn record(&self, ip: &str, ok: bool, now: OffsetDateTime); }
/// 取限速与日志用的客户端 IP：UdsPeer → "unix"；否则取 X-Forwarded-For 的**最后一跳**；都没有则取 ConnectInfo
pub fn client_ip(req: &axum::extract::Request) -> String;
/// unix socket 连接注入的扩展；uid 命中即视为管理员（Task 14）
#[derive(Debug, Clone, Copy)] pub struct UdsPeer { pub uid: u32 }
pub async fn login(State(app): State<AppState>, req: Request) -> Response;
pub async fn require_admin(state: State<AppState>, req: Request, next: Next) -> Response;
// crate::api::health
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceStatus { pub unit: String, pub active: bool, pub enabled: bool, pub n_restarts: u32 }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthResponse {
    pub status: String,                  // "ok" | "degraded"
    pub version: String,
    pub uptime_secs: u64,
    pub node: String,
    pub services: Vec<ServiceStatus>,
    pub reconcile: Option<ReconcileReport>,
    pub drift: Vec<DriftItem>,
    pub watchdog: BTreeMap<String, WatchdogRecord>,
    pub upgrade_available: Option<String>,        // 每日自检发现的新版本（spec §7）
    pub residential: Option<serde_json::Value>,   // P3 填
}
pub async fn get(State(app): State<AppState>) -> Json<HealthResponse>;
// crate::api::system（CLI 通过 socket 调的两个系统端点，spec §2.4）
#[derive(Debug, Deserialize)] pub struct ReconcileRequest { #[serde(default)] pub force: bool, #[serde(default)] pub dry_run: bool }
// POST /api/reconcile  { "force": bool, "dry_run": bool } → 200 ReconcileReport（已有报告）/ 202 {"queued":true}
// POST /api/services/{unit}/{action}  action ∈ restart|stop|start|reload → { "ok": bool, "detail": String }
```

契约对齐 v3（`web/server.js:1693-1702`）：`POST /api/login` 请求体 `{"password":"…"}`，成功 `200 {"token":"…"}`，失败 `401 {"error":"Auth failed"}`，限速 `429 {"error":"Too many attempts. Try again later."}`。与 v3 的差异只有两处，都按 spec §4.3：① 限速窗口从 5 分钟改成 **5 次/分钟/IP**；② JWT 密钥来自 `state.admin.jwt_secret`（持久化，修 web-C17：v3 每次重启随机）。`POST /api/services/{unit}/{action}` 的 `unit` 只接受 `crate::reconcile::MANAGED_UNITS` 里的六个名字，其余 400。

**限速器为什么在 `AppState` 里**：生产上它是「每进程一份」，因为一个进程只建一个 router；但若写成进程级 `OnceLock`，同一个测试二进制里的多个 `#[tokio::test]` 会共享同一张计数表——`login_rejects_a_wrong_password_like_v3`（失败 1 次）、`sixth_failed_login_within_a_minute_is_rate_limited`（失败 6 次）与三个成功登录的测试并行跑，加上 `FakeHost` 的时钟固定（窗口永不过期）、`oneshot` 请求没有 `ConnectInfo`（所有测试共用同一个 IP 键 `"unknown"`），谁后跑谁拿到错位的 429/401。放进 `AppState` 后每个 `app()` 一份，测试天然隔离，生产语义不变。

**`X-Forwarded-For` 的信任边界**（本任务必须写死）：面板只经 Caddy 反代暴露（`reverse_proxy 127.0.0.1:8080`），所以 TCP 对端永远是 `127.0.0.1`，按 `ConnectInfo` 限速等于全站共用一个桶。Caddy 会把真实客户端 IP **追加**到 `X-Forwarded-For` 末尾，因此 `client_ip` 的规则是：
1. 请求扩展里有 `UdsPeer` → `"unix"`（socket 路径不限速，本来就只有 root 能连）；
2. 否则取 `X-Forwarded-For` 最后一个逗号分隔项（去空格）——它是我们自己的 Caddy 追加的那一跳，客户端伪造的前缀项一律忽略；
3. 没有该头 → `ConnectInfo<SocketAddr>` 的 IP；连它也没有（`oneshot` 测试）→ `"unknown"`。
`axum::serve` 必须用 `into_make_service_with_connect_info::<SocketAddr>()`（Task 15 的 `run` 里如此调用），否则第 3 条永远落到 `"unknown"`。

- [ ] **Step 1: 写失败测试（auth 纯逻辑）**

`crates/bui/src/api/auth.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime { datetime!(2026-09-11 00:00:00 UTC) }
    const SECRET: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn password_hash_round_trips_and_rejects_wrong_input() {
        let h = hash_password("test123").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "test123"));
        assert!(!verify_password(&h, "test124"));
        assert!(!verify_password("not-a-hash", "test123"));
    }

    #[test]
    fn token_round_trips_and_expires_after_24h() {
        let (token, expires_at) = issue_token(SECRET, t0()).unwrap();
        assert_eq!(expires_at, "2026-09-12T00:00:00Z");
        let claims = decode_token(SECRET, &token, t0() + time::Duration::hours(23)).unwrap();
        assert!(claims.admin);
        assert_eq!(claims.sub, "admin");
        assert!(decode_token(SECRET, &token, t0() + time::Duration::hours(25)).is_none());
        assert!(decode_token("another-secret", &token, t0()).is_none());
        assert!(decode_token(SECRET, "garbage", t0()).is_none());
    }

    #[test]
    fn limiter_allows_five_failures_per_minute_per_ip() {
        let l = LoginLimiter::default();
        for i in 0..5 {
            assert!(l.allow("203.0.113.10", t0()), "第 {} 次应放行", i + 1);
            l.record("203.0.113.10", false, t0());
        }
        assert!(!l.allow("203.0.113.10", t0()), "第 6 次被限速");
        assert!(l.allow("203.0.113.11", t0()), "限速按 IP 隔离");
        assert!(l.allow("203.0.113.10", t0() + time::Duration::seconds(61)), "窗口过期后恢复");
    }

    #[test]
    fn a_successful_login_clears_the_counter() {
        let l = LoginLimiter::default();
        for _ in 0..4 {
            l.record("203.0.113.10", false, t0());
        }
        l.record("203.0.113.10", true, t0());
        for _ in 0..5 {
            assert!(l.allow("203.0.113.10", t0()));
            l.record("203.0.113.10", false, t0());
        }
        assert!(!l.allow("203.0.113.10", t0()));
    }

    #[test]
    fn two_limiters_do_not_share_counters() {
        // 这条锁住「限速器在 AppState 里」的设计：两个 router 实例 = 两张计数表
        let (a, b) = (LoginLimiter::default(), LoginLimiter::default());
        for _ in 0..5 {
            a.record("203.0.113.10", false, t0());
        }
        assert!(!a.allow("203.0.113.10", t0()));
        assert!(b.allow("203.0.113.10", t0()));
    }

    #[test]
    fn client_ip_takes_the_last_forwarded_hop() {
        use axum::body::Body;
        use axum::http::Request as HttpRequest;
        // Caddy 追加的最后一跳才可信；客户端伪造的前缀项忽略
        let req = HttpRequest::builder()
            .uri("/api/login")
            .header("x-forwarded-for", "1.1.1.1, 203.0.113.10")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "203.0.113.10");
        let bare = HttpRequest::builder().uri("/api/login").body(Body::empty()).unwrap();
        assert_eq!(client_ip(&bare), "unknown");
        let mut uds = HttpRequest::builder().uri("/api/login").body(Body::empty()).unwrap();
        uds.extensions_mut().insert(UdsPeer { uid: 0 });
        assert_eq!(client_ip(&uds), "unix");
    }
}
```

- [ ] **Step 2: 写失败测试（HTTP 行为，`tower::ServiceExt::oneshot`）**

`crates/bui/src/api/mod.rs` 末尾（三个辅助函数与被测行为一次写全，不留半成品）：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::health::HealthResponse;
    use crate::state::runtime::{ReconcileReport, Runtime};
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// 一台「六个单元都在跑」的假机器 + 一个独立 AppState（限速器随之独立）
    async fn app_with_runtime() -> (axum::Router, tempfile::TempDir, Arc<FakeHost>, Runtime) {
        let d = tempfile::tempdir().unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = crate::api::auth::hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in crate::reconcile::MANAGED_UNITS {
                i.units_active.insert(format!("{u}.service"));
                i.units_enabled.insert(format!("{u}.service"));
            }
            // 键必须是单元**全名**：`unit_property` 的查询键会补 `.service`（Task 3 的归一化），
            // 播成裸名查不到 → `n_restarts` 静默变 0，下面的 `== 3` 必失败
            i.unit_props.insert(("hysteria-server.service".into(), "NRestarts".into()), "3".into());
        });
        let app_state = AppState {
            store,
            bus: EventBus::new(),
            runtime: runtime.clone(),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: crate::api::auth::LoginLimiter::default(),
        };
        (router(app_state, &[]), d, host, runtime)
    }

    async fn app() -> (axum::Router, tempfile::TempDir, Arc<FakeHost>) {
        let (app, d, host, _rt) = app_with_runtime().await;
        (app, d, host)
    }

    async fn json(res: axum::response::Response) -> serde_json::Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    fn post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn with_token(mut req: Request<Body>, token: &str) -> Request<Body> {
        req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
        req
    }

    /// 登录拿 token（成功登录会清掉本 router 的失败计数）
    async fn login(app: &axum::Router) -> String {
        let res = app
            .clone()
            .oneshot(post("/api/login", serde_json::json!({"password": "test123"})))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        json(res).await["token"].as_str().unwrap().to_string()
    }

    async fn health(app: &axum::Router, token: &str) -> HealthResponse {
        let res = app
            .clone()
            .oneshot(with_token(
                Request::builder().uri("/api/health").body(Body::empty()).unwrap(),
                token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        serde_json::from_value(json(res).await).unwrap()
    }

    #[tokio::test]
    async fn login_rejects_a_wrong_password_like_v3() {
        let (app, _d, _h) = app().await;
        let res = app.oneshot(post("/api/login", serde_json::json!({"password": "nope"}))).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json(res).await, serde_json::json!({"error": "Auth failed"}));
    }

    #[tokio::test]
    async fn login_returns_a_token_on_success() {
        let (app, _d, _h) = app().await;
        let res = app.oneshot(post("/api/login", serde_json::json!({"password": "test123"}))).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json(res).await;
        assert_eq!(body["token"].as_str().unwrap().split('.').count(), 3, "JWT 三段");
        assert!(body["expires_at"].is_string());
    }

    #[tokio::test]
    async fn sixth_failed_login_within_a_minute_is_rate_limited() {
        let (app, _d, _h) = app().await;
        for _ in 0..5 {
            let res = app
                .clone()
                .oneshot(post("/api/login", serde_json::json!({"password": "nope"})))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
        let res = app.oneshot(post("/api/login", serde_json::json!({"password": "nope"}))).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json(res).await["error"], "Too many attempts. Try again later.");
    }

    #[tokio::test]
    async fn health_requires_a_bearer_token() {
        let (app, _d, _h) = app().await;
        let res = app
            .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_reports_services_drift_watchdog_and_reconcile() {
        let (app, _d, _h) = app().await;
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "ok");
        assert_eq!(h.version, "4.0.0");
        assert_eq!(h.node, "node-a");
        assert_eq!(h.services.len(), 6, "六个受管单元都要报");
        let hy = h.services.iter().find(|s| s.unit == "hysteria-server").unwrap();
        assert!(hy.active && hy.enabled);
        assert_eq!(hy.n_restarts, 3);
        assert_eq!(h.drift, vec![]);
        assert!(h.watchdog.is_empty());
        assert_eq!(h.reconcile, None);
        assert_eq!(h.upgrade_available, None);
        assert_eq!(h.residential, None, "P3 才填");
    }

    #[tokio::test]
    async fn health_is_degraded_when_the_last_reconcile_had_errors() {
        let (app, _d, _h, runtime) = app_with_runtime().await;
        runtime
            .update(|r| {
                r.last_reconcile = Some(ReconcileReport {
                    at: "2026-09-11T00:00:00Z".into(),
                    errors: vec!["hysteria-server 重启失败".into()],
                    ..Default::default()
                });
                r.upgrade_available = Some("4.0.1".into());
            })
            .await;
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "degraded");
        assert_eq!(h.reconcile.unwrap().errors.len(), 1);
        assert_eq!(h.upgrade_available.as_deref(), Some("4.0.1"));
    }

    #[tokio::test]
    async fn health_is_degraded_when_a_managed_unit_is_down() {
        let (app, _d, host) = app().await;
        host.with(|i| {
            i.units_active.remove("xray.service");
        });
        let token = login(&app).await;
        let h = health(&app, &token).await;
        assert_eq!(h.status, "degraded");
        assert!(!h.services.iter().find(|s| s.unit == "xray").unwrap().active);
    }

    #[tokio::test]
    async fn reconcile_endpoint_queues_and_returns_the_last_report() {
        let (app, _d, _h, runtime) = app_with_runtime().await;
        let token = login(&app).await;
        // 还没有任何报告 → 202 排队
        let res = app
            .clone()
            .oneshot(with_token(post("/api/reconcile", serde_json::json!({"force": false, "dry_run": false})), &token))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        assert_eq!(json(res).await["queued"], true);
        // 有报告后 → 200 + 报告本体
        runtime
            .update(|r| {
                r.last_reconcile = Some(ReconcileReport { at: "2026-09-11T00:00:00Z".into(), ..Default::default() })
            })
            .await;
        let res = app
            .oneshot(with_token(post("/api/reconcile", serde_json::json!({"force": true, "dry_run": false})), &token))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json(res).await["at"], "2026-09-11T00:00:00Z");
    }

    #[tokio::test]
    async fn service_action_rejects_unmanaged_units() {
        let (app, _d, host) = app().await;
        let token = login(&app).await;
        let call = |unit: &str, action: &str| {
            let token = token.clone();
            let app = app.clone();
            let uri = format!("/api/services/{unit}/{action}");
            async move {
                app.oneshot(with_token(
                    Request::builder().method("POST").uri(uri).body(Body::empty()).unwrap(),
                    &token,
                ))
                .await
                .unwrap()
            }
        };
        assert_eq!(call("hysteria-server", "restart").await.status(), StatusCode::OK);
        assert!(host.ops().contains(&"systemd:restart:hysteria-server".to_string()));
        assert_eq!(call("sshd", "stop").await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(call("hysteria-server", "chown").await.status(), StatusCode::BAD_REQUEST);
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p bui api::`
Expected: 编译失败，`cannot find function router` / `no field \`login\` on type \`AppState\``。

- [ ] **Step 4: 实现 `auth.rs`**

```rust
pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut OsRng);
    Ok(argon2::Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 哈希失败：{e}"))?
        .to_string())
}

pub fn verify_password(hash: &str, pw: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    match PasswordHash::new(hash) {
        Ok(parsed) => argon2::Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

pub fn issue_token(secret: &str, now: OffsetDateTime) -> anyhow::Result<(String, String)> {
    let exp = now + time::Duration::hours(TOKEN_HOURS);
    let claims = Claims { sub: "admin".into(), admin: true, exp: exp.unix_timestamp() };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok((token, crate::util::fmt_rfc3339(exp)))
}

pub fn decode_token(secret: &str, token: &str, now: OffsetDateTime) -> Option<Claims> {
    let mut v = jsonwebtoken::Validation::default();
    v.validate_exp = false;   // 自己按传入的 now 判断，便于测试
    let data = jsonwebtoken::decode::<Claims>(token, &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()), &v).ok()?;
    (data.claims.exp > now.unix_timestamp() && data.claims.admin).then_some(data.claims)
}

/// 限速与日志用的客户端 IP。信任边界见本任务开头的说明。
pub fn client_ip(req: &axum::extract::Request) -> String {
    if req.extensions().get::<UdsPeer>().is_some() {
        return "unix".to_string();
    }
    if let Some(xff) = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.rsplit(',').next() {
            let last = last.trim();
            if !last.is_empty() {
                return last.to_string();
            }
        }
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
```
`LoginLimiter`：`Arc<std::sync::Mutex<HashMap<String, (OffsetDateTime, u32)>>>`；`allow` 先把「窗口已过期（`now - first > LOGIN_WINDOW_SECS`）」的条目删掉，再判 `count < LOGIN_MAX_ATTEMPTS`；`record(ok=true)` 删条目，`record(ok=false)` 在条目不存在时写 `(now, 1)`、存在时 `count += 1`（窗口过期则重置成 `(now, 1)`）；锁 poison 时放行（宁可不限速也不要 500）。

`login` handler（不用 `Json` 提取器，因为要先读 header 再读 body）：
```rust
pub async fn login(State(app): State<AppState>, req: axum::extract::Request) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let ip = client_ip(&req);
    let now = app.host.now();
    if !app.login.allow(&ip, now) {
        return (StatusCode::TOO_MANY_REQUESTS, axum::Json(serde_json::json!({"error": "Too many attempts. Try again later."}))).into_response();
    }
    let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": "Bad request"}))).into_response(),
    };
    let password = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("password").and_then(|p| p.as_str()).map(str::to_string))
        .unwrap_or_default();
    let state = app.store.read().await;
    if !verify_password(&state.admin.password_hash, &password) {
        app.login.record(&ip, false, now);
        tracing::warn!(ip = %ip, "管理员登录失败");
        return (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"error": "Auth failed"}))).into_response();
    }
    app.login.record(&ip, true, now);
    match issue_token(&state.admin.jwt_secret, now) {
        Ok((token, expires_at)) => {
            (StatusCode::OK, axum::Json(serde_json::json!({"token": token, "expires_at": expires_at}))).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "签发 JWT 失败");
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({"error": "Internal error"}))).into_response()
        }
    }
}
```
`require_admin` 中间件：请求扩展里有 `UdsPeer` 且 `uid == 0 || uid == nix::unistd::geteuid().as_raw()` → 直接放行（socket 是 0600，只有守护进程用户能连；生产下就是 root）；否则取 `Authorization: Bearer`，用 `state.store.read().await.admin.jwt_secret` 与 `state.host.now()` 校验，失败返回 `401 {"error":"Unauthorized"}`。

- [ ] **Step 5: 实现 `health.rs`、`system.rs` 与 `mod.rs`**

`health.rs` handler：
```rust
pub async fn get(State(app): State<AppState>) -> Json<HealthResponse> {
    let state = app.store.read().await;
    let rt = app.runtime.read().await;
    let host = app.host.clone();
    let services = tokio::task::spawn_blocking(move || {
        crate::reconcile::MANAGED_UNITS
            .iter()
            .map(|u| ServiceStatus {
                unit: u.to_string(),
                active: host.unit_is_active(u).unwrap_or(false),
                enabled: host.unit_is_enabled(u).unwrap_or(false),
                n_restarts: host
                    .unit_property(u, "NRestarts")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let degraded = services.iter().any(|s| !s.active)
        || !rt.drift.is_empty()
        || rt.last_reconcile.as_ref().is_some_and(|r| !r.errors.is_empty() || !r.verify_failures.is_empty());
    Json(HealthResponse {
        status: if degraded { "degraded".into() } else { "ok".into() },
        version: app.version.to_string(),
        uptime_secs: (app.host.now() - app.started_at).whole_seconds().max(0) as u64,
        node: state.node.name.clone(),
        services,
        reconcile: rt.last_reconcile.clone(),
        drift: rt.drift.clone(),
        watchdog: rt.watchdog.clone(),
        upgrade_available: rt.upgrade_available.clone(),
        residential: None,
    })
}
```

`system.rs`：
```rust
pub async fn reconcile(State(app): State<AppState>, Json(req): Json<ReconcileRequest>) -> Response {
    app.bus.send(Event::ReconcileRequested { force: req.force });
    // 真正的对账由 serve.rs 的去抖触发器执行；这里只排队并回最近一份报告
    match app.runtime.read().await.last_reconcile {
        Some(r) => (StatusCode::OK, Json(r)).into_response(),
        None => (StatusCode::ACCEPTED, Json(serde_json::json!({"queued": true}))).into_response(),
    }
}

pub async fn service_action(State(app): State<AppState>, Path((unit, action)): Path<(String, String)>) -> Response {
    if !crate::reconcile::MANAGED_UNITS.contains(&unit.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("不受管的单元：{unit}")}))).into_response();
    }
    if !["restart", "stop", "start", "reload"].contains(&action.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("不支持的动作：{action}")}))).into_response();
    }
    let host = app.host.clone();
    let out = tokio::task::spawn_blocking(move || host.systemd(&action, &unit)).await;
    match out {
        Ok(Ok(o)) => (StatusCode::OK, Json(serde_json::json!({"ok": o.ok(), "detail": o.stderr}))).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "systemctl 调用失败"}))).into_response(),
    }
}
```
`mod.rs` 的 `router`：
```rust
pub fn router(state: AppState, modules: &[Arc<dyn Module>]) -> axum::Router {
    let protected = axum::Router::new()
        .route("/api/health", axum::routing::get(health::get))
        .route("/api/reconcile", axum::routing::post(system::reconcile))
        .route("/api/services/{unit}/{action}", axum::routing::post(system::service_action));
    let protected = modules.iter().fold(protected, |acc, m| acc.merge(m.routes()));
    axum::Router::new()
        .route("/api/login", axum::routing::post(auth::login))
        .merge(protected.layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_admin)))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui api:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 16 passed（auth 6 + api::mod 9 + api::state 1）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/api Cargo.lock
git commit -m "feat(bui): API 实现（JWT 登录与按 IP 限速、健康端点、CLI 系统端点）"
```

---

### Task 14: unix socket 服务端与 CLI 客户端

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/ipc.rs`

**Interfaces:**
- Consumes: `crate::api::{auth::UdsPeer, AppState}`、`crate::paths::SOCKET_PATH`
- Produces:
```rust
/// 绑定 socket（0600）并把同一个 Router 服务在上面；连接扩展里注入 UdsPeer。
pub async fn serve_uds(path: &Path, app: axum::Router) -> anyhow::Result<()>;
#[derive(Clone)]
pub struct Client { path: PathBuf }
impl Client {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    /// socket 存在且能连上
    pub async fn available(&self) -> bool;
    pub async fn request(&self, method: &str, path: &str, body: Option<serde_json::Value>)
        -> anyhow::Result<(u16, serde_json::Value)>;
}
```
spec §2.4：`/run/b-ui.sock` 权限 0600、只有 root；所有菜单项通过它调守护进程 API。鉴权：socket 上的请求由 `require_admin` 依 `UdsPeer` 放行（`uid == 0` 或 `uid == 守护进程自身 euid`）——socket 本身是 0600 且属守护进程用户，能连上就说明是同一用户或 root，不需要再拿 JWT；公开端点 `/api/login` 不受影响。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{auth::hash_password, AppState, EventBus};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    async fn spawn_server(dir: &std::path::Path) -> (PathBuf, Arc<FakeHost>) {
        let sock = dir.join("b-ui.sock");
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = hash_password("test123").unwrap();
        let store = Store::create(dir.join("state.json"), state).await.unwrap();
        let runtime = Runtime::load(dir.join("runtime.json"));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in crate::reconcile::MANAGED_UNITS {
                i.units_active.insert(format!("{u}.service"));
                i.units_enabled.insert(format!("{u}.service"));
            }
        });
        let app = crate::api::router(
            AppState {
                store,
                bus: EventBus::new(),
                runtime,
                host: host.clone(),
                started_at: host.now(),
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            &[],
        );
        let s = sock.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&s, app).await;
        });
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (sock, host)
    }

    #[tokio::test]
    async fn socket_is_created_with_0600() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn local_peer_reaches_health_without_a_token() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let (status, body) = Client::new(&sock).request("GET", "/api/health", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["version"], "4.0.0");
        assert_eq!(body["node"], "node-a");
    }

    #[tokio::test]
    async fn login_over_the_socket_still_checks_the_password() {
        let d = tempfile::tempdir().unwrap();
        let (sock, _h) = spawn_server(d.path()).await;
        let c = Client::new(&sock);
        let (status, body) = c
            .request("POST", "/api/login", Some(serde_json::json!({"password": "wrong"})))
            .await
            .unwrap();
        assert_eq!(status, 401);
        assert_eq!(body["error"], "Auth failed");
        let (status, body) = c
            .request("POST", "/api/login", Some(serde_json::json!({"password": "test123"})))
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(body["token"].is_string());
    }

    #[tokio::test]
    async fn service_action_over_the_socket_hits_systemctl() {
        let d = tempfile::tempdir().unwrap();
        let (sock, host) = spawn_server(d.path()).await;
        let (status, body) = Client::new(&sock)
            .request("POST", "/api/services/xray/restart", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert!(host.ops().contains(&"systemd:restart:xray".to_string()));
    }

    #[tokio::test]
    async fn available_is_false_when_nothing_is_listening() {
        let d = tempfile::tempdir().unwrap();
        assert!(!Client::new(d.path().join("absent.sock")).available().await);
        let (sock, _h) = spawn_server(d.path()).await;
        assert!(Client::new(&sock).available().await);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced_on_bind() {
        let d = tempfile::tempdir().unwrap();
        let sock = d.path().join("b-ui.sock");
        std::fs::write(&sock, b"stale").unwrap();
        let mut state = crate::testutil::sample_state();
        state.admin.password_hash = hash_password("test123").unwrap();
        let store = Store::create(d.path().join("state.json"), state).await.unwrap();
        let host = Arc::new(FakeHost::new());
        let app = crate::api::router(
            AppState {
                store,
                bus: EventBus::new(),
                runtime: Runtime::load(d.path().join("runtime.json")),
                host: host.clone(),
                started_at: host.now(),
                version: "4.0.0",
                login: crate::api::auth::LoginLimiter::default(),
            },
            &[],
        );
        let s = sock.clone();
        tokio::spawn(async move {
            let _ = serve_uds(&s, app).await;
        });
        for _ in 0..100 {
            if Client::new(&sock).available().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("陈旧 socket 文件没被替换");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui ipc::`
Expected: 编译失败，`cannot find function serve_uds`。

- [ ] **Step 3: 实现服务端**

```rust
pub async fn serve_uds(path: &Path, app: axum::Router) -> anyhow::Result<()> {
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 陈旧的 socket 文件（上次崩溃留下的）会让 bind 报 EADDRINUSE
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %path.display(), "CLI socket 就绪");
    loop {
        let (stream, _) = listener.accept().await?;
        let uid = peer_uid(&stream).unwrap_or(u32::MAX);
        let app = app.clone();
        tokio::spawn(async move {
            let svc = tower::ServiceBuilder::new()
                .layer(axum::middleware::from_fn(move |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
                    req.extensions_mut().insert(crate::api::auth::UdsPeer { uid });
                    next.run(req).await
                }))
                .service(app);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), TowerToHyperService::new(svc))
                .await
            {
                tracing::debug!(error = %e, "socket 连接结束");
            }
        });
    }
}

fn peer_uid(stream: &tokio::net::UnixStream) -> Option<u32> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials).ok().map(|c| c.uid())
}
```
（`from_fn` 里捕获 `uid` 需要 `move` 闭包 + `Clone`，`u32` 天然满足；`nix::sys::socket::getsockopt` 在 nix 0.29 接受实现了 `AsFd` 的引用，`UnixStream` 满足。）

- [ ] **Step 4: 实现客户端**

```rust
impl Client {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub async fn available(&self) -> bool {
        tokio::net::UnixStream::connect(&self.path).await.is_ok()
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<(u16, serde_json::Value)> {
        use http_body_util::BodyExt;
        use hyper_util::rt::TokioIo;

        let stream = tokio::net::UnixStream::connect(&self.path)
            .await
            .map_err(|e| anyhow::anyhow!("连不上守护进程（{}）：{e}", self.path.display()))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let req = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(payload)?;
        let res = sender.send_request(req).await?;
        let status = res.status().as_u16();
        let bytes = res.into_body().collect().await?.to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        Ok((status, json))
    }
}
```
`String` 作为 hyper body 需要 `http_body_util::Full<Bytes>`：实现时用 `.body(http_body_util::Full::new(hyper::body::Bytes::from(payload)))`。

- [ ] **Step 5: 运行测试**

Run: `cargo test -p bui ipc:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 6 passed。

- [ ] **Step 6: Commit**

```bash
git add crates/bui/src/ipc.rs Cargo.lock
git commit -m "feat(bui): unix socket 服务端（0600 + SO_PEERCRED）与 CLI 客户端"
```

---

### Task 15: `bui serve` 装配（模块注册、对账触发器、10 分钟巡检、每日带抖动自检）

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/serve.rs`
- Modify: `crates/bui/src/main.rs`（`Command::Serve` 与 `Command::Reconcile` 两个 arm 落地）

**Interfaces:**
- Consumes: 前面全部；`crate::kernels::{Fetcher, HttpFetcher, KernelInstaller, Manifest, manifest_url}`、`crate::util::fmt_rfc3339`
- Produces:
```rust
pub const DEBOUNCE_MS: u64 = 500;
pub const DRIFT_INTERVAL_SECS: u64 = 600;
/// 每日自检间隔（spec §7「守护进程每日带抖动自检」）
pub const SELFCHECK_INTERVAL_SECS: u64 = 86400;
/// P1 注册的六个模块 + 它们共享的 manifest 句柄；P2/P3 在 `modules()` 里各追加自己的 Module
pub struct Registry {
    pub modules: Vec<Arc<dyn Module>>,
    pub manifest: Arc<std::sync::RwLock<Option<Manifest>>>,
}
pub fn modules(manifest: Option<Manifest>) -> Registry;
pub struct ReconcileInput<'a> {
    pub state: &'a State,
    pub modules: &'a [Arc<dyn Module>],
    pub paths: &'a Paths,
    pub keys: &'a BTreeMap<String, String>,
    pub installer: &'a dyn BinaryInstaller,
    pub force: bool,
    pub dry_run: bool,
}
/// 一次完整对账：探事实 → 收集 artifacts → diff → apply → 漂移扫描 → 报告
pub fn reconcile_once(input: ReconcileInput<'_>, host: &dyn Host) -> anyhow::Result<(ReconcileReport, BTreeMap<String, String>)>;
/// 从 DaemonCtx 跑一轮并把报告/漂移/重启键写进 runtime；relay 重启时发 `Event::RelayRestarted`。
/// `fetcher` 用 `Arc<dyn Fetcher>` 传入并在 `spawn_blocking` 里使用（`reqwest::blocking` 不能在 async 上下文里跑）。
/// **不重启 `b-ui` 自己**（apply 也不会，见 Task 5 第 12 步）——调用方拿到报告后调 `finish_self_restart`。
pub async fn reconcile_from_ctx(ctx: &DaemonCtx, modules: &[Arc<dyn Module>], fetcher: Arc<dyn Fetcher>, force: bool, dry_run: bool)
    -> anyhow::Result<ReconcileReport>;
/// 报告与 `restart_keys` 已落盘之后才重启守护进程自己：`in_daemon = true` 用
/// `systemctl restart --no-block b-ui.service`（systemd 排队、本轮先返回；单元有 `Restart=always`），
/// CLI 路径用同步 `systemctl restart b-ui.service`。`report.self_restart_required` 为 false 时什么都不做。
pub async fn finish_self_restart(ctx: &DaemonCtx, report: &ReconcileReport, in_daemon: bool);
/// 500ms 去抖：把一串 Event 压成一次触发
pub async fn debounce_loop(bus: EventBus, tx: tokio::sync::mpsc::Sender<bool>);
/// 每日自检：延 `jitter_secs(node.id)` 秒后每 24h 拉一次 manifest → 写 `<base>/manifest.json`
/// → 刷新共享 manifest 句柄 → 记录可升级版本 → 请求一次对账（内核随 manifest 升级）
pub async fn selfcheck_loop(ctx: DaemonCtx, manifest: Arc<std::sync::RwLock<Option<Manifest>>>, fetcher: Arc<dyn Fetcher>, url: String);
/// 读 `<base>/manifest.json`；没有或解析失败 → None（对账跳过 Binary）
pub fn load_cached_manifest(host: &dyn Host, paths: &Paths) -> Option<Manifest>;
/// 每日自检的抖动：按节点 id 定死（0..3600 秒），避免全网同一秒打 GitHub。
/// 定义在这里而不是 `commands::upgrade`，是为了不让 Task 15 反向依赖 Task 17；
/// Task 17 用 `pub use crate::serve::jitter_secs;` 再导出给 CLI。
pub fn jitter_secs(node_id: uuid::Uuid) -> u64;
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
/// CLI 的 `bui reconcile`：socket 可用就走 API，否则进程内直接跑。
/// `socket` 由调用方传入（`main.rs` 传 `PathBuf::from(crate::paths::SOCKET_PATH)`）而不是在函数体里
/// 读常量——否则任何测到这条路径的单元测试都会真的 `connect("/run/b-ui.sock")`，在跑着 v4 守护进程的
/// 机器上会真发一次 `POST /api/reconcile`（第三轮审查 C3；与 `InstallOpts.socket` 同一条口径）。
pub async fn reconcile_cli(paths: Paths, host: Arc<dyn Host>, socket: PathBuf, force: bool, dry_run: bool) -> anyhow::Result<()>;
```
触发点（spec §2.2、§7）：启动时立即一次；`EventBus` 上任何 `StateChanged` / `ReconcileRequested` 经 500ms 去抖；每 10 分钟一次漂移检查；每日一次带抖动的 manifest 自检（`jitter_secs` 就在本文件，按节点 id 定死，避免同一秒全网打 GitHub）；`bui reconcile` 手动。`reconcile_once` 还负责 spec §3.4 的那条提示：`state.system.ssh_hardening && facts.ssh_pubkeys == 0` 时往 `report.notes` 追加「未在 /root/.ssh/authorized_keys 检测到公钥，已跳过 SSH 硬化；加好公钥后运行 `b-ui harden-ssh`」。

**自检只刷新内核、不自动换 bui 自己**：spec §7 说的是「守护进程每日带抖动自检」，`bui upgrade` 仍由面板/CLI 手动触发。所以自检任务只做三件事：写 manifest 缓存、刷新共享句柄（下一轮对账即按新版本装内核）、把 `manifest.version != 本进程版本` 记进 `runtime.upgrade_available` 供 `/api/health` 与 `bui status` 显示。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Event, EventBus};
    use crate::kernels::{Asset, Fetcher, Manifest};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct NoopInstaller;
    impl crate::reconcile::apply::BinaryInstaller for NoopInstaller {
        fn install(&self, _n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FakeFetcher(Mutex<Vec<(String, Vec<u8>)>>);
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    /// 一台「装好了 v4」的假机器：六个单元在跑、四个内核版本正确、公钥在位、**ufw 已启用**。
    /// ufw 那两行不是装饰：`SystemModule::render` 只在 `facts.ufw_active || facts.firewalld_active`
    /// 为真时才产出 `FirewallPorts`（Task 6），所以少了它，下面 `first_pass…` 的
    /// `keys.contains_key("firewall")` 就无从成立，`a_hand_edited_config…` 也测不到 ufw 那条路径。
    fn ready_host() -> Arc<FakeHost> {
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            i.files.insert("/root/.ssh/authorized_keys".into(), (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600));
            i.which.insert("ufw".into());
            i.scripted.push(("ufw status".into(), crate::sys::CmdOut::success("Status: active\n")));
            for (bin, out) in [
                ("hysteria", "Version:\tv2.12.2\n"),
                ("xray", "Xray 26.3.27 (Xray) abc (go1.26.1 linux/amd64)\n"),
                ("sing-box", "sing-box version 1.13.19\n"),
                ("caddy", "v2.10.2 h1:xxx\n"),
            ] {
                i.files.insert(format!("/opt/b-ui/bin/{bin}").into(), (b"ELF".to_vec(), 0o755));
                i.scripted.push((format!("/opt/b-ui/bin/{bin} version"), CmdOut::success(out)));
            }
        });
        h
    }

    fn input<'a>(
        state: &'a bui_schema::model::State,
        mods: &'a [Arc<dyn Module>],
        paths: &'a bui_schema::paths::Paths,
        keys: &'a BTreeMap<String, String>,
        installer: &'a dyn crate::reconcile::apply::BinaryInstaller,
    ) -> ReconcileInput<'a> {
        ReconcileInput { state, modules: mods, paths, keys, installer, force: false, dry_run: false }
    }

    #[test]
    fn first_pass_writes_everything_second_pass_is_a_no_op() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let keys = BTreeMap::new();
        let (first, keys) =
            reconcile_once(input(&state, &reg.modules, &paths, &keys, &NoopInstaller), host.as_ref()).unwrap();
        assert!(!first.changed.is_empty(), "首轮要写配置、单元、sysctl、符号链接");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(first.verify_failures.is_empty(), "{:?}", first.verify_failures);
        assert!(first.changed.iter().any(|c| c.contains("config.yaml")));
        assert!(first.changed.iter().any(|c| c.contains("hysteria-server.service")));
        assert!(first.changed.iter().any(|c| c.contains("99-b-ui-network.conf")));
        assert!(first.changed.iter().any(|c| c == "nf_conntrack"), "首轮要加载 conntrack 模块");
        assert!(first.changed.iter().any(|c| c == "/usr/local/bin/b-ui"), "首轮要建 CLI 符号链接");
        assert!(first.self_restart_required, "首轮写了 b-ui.service，自身重启交给调用方");
        // B4：apply 必须把 plan.keys 搬进 outcome.keys，reconcile_once 再 extend 进 restart_keys。
        // 少了这一步：`firewall` 键缺失 → 下一轮又出一条 OpenPorts（第二轮就不是零变更）；
        // xray 的 restart_key 缺失 → `keys[id] != k` 永远成立 → 每次加用户改 clients 都重启 xray。
        assert!(keys.contains_key("firewall"), "{keys:?}");
        assert!(keys.contains_key("file:/opt/b-ui/xray-config.json"), "{keys:?}");
        host.clear_ops();
        let (second, _) =
            reconcile_once(input(&state, &reg.modules, &paths, &keys, &NoopInstaller), host.as_ref()).unwrap();
        assert!(second.is_clean(), "二次对账必须零变更（M1 验收项）：{second:?}");
        assert!(second.drift.is_empty(), "二次对账也不能报漂移：{:?}", second.drift);
        assert!(
            host.ops().iter().all(|o| o.starts_with("run:") || o.starts_with("write:/opt/b-ui/.verify/")),
            "只允许剩下只读探测与校验落地：{:?}",
            host.ops()
        );
    }

    #[test]
    fn a_hand_edited_config_is_rewritten_and_only_its_unit_restarts() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (_, keys) = reconcile_once(
            input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        host.write_file(Path::new("/opt/b-ui/config.yaml"), b"listen: :1\n", 0o600).unwrap();
        host.clear_ops();
        let (report, _) =
            reconcile_once(input(&state, &reg.modules, &paths, &keys, &NoopInstaller), host.as_ref()).unwrap();
        assert_eq!(report.changed, vec!["/opt/b-ui/config.yaml".to_string()]);
        assert_eq!(report.restarted, vec!["hysteria-server".to_string()]);
    }

    #[test]
    fn drift_is_reported_but_untouched_until_force() {
        let host = ready_host();
        host.with(|i| {
            i.files.insert("/etc/systemd/system/xray.service.d/50-manual.conf".into(), (b"x".to_vec(), 0o644));
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (report, keys) = reconcile_once(
            input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        // `bin/` 里的四个内核二进制不会各报一条：`list_dir` 只返回直接子项，`bin` 在白名单里
        assert_eq!(report.drift.len(), 1, "只有手工 drop-in 一条：{:?}", report.drift);
        assert_eq!(report.drift[0].kind, "unit_dropin");
        assert!(host.text("/etc/systemd/system/xray.service.d/50-manual.conf").is_some(), "只报不改");
        let mut forced = input(&state, &reg.modules, &paths, &keys, &NoopInstaller);
        forced.force = true;
        let (report2, _) = reconcile_once(forced, host.as_ref()).unwrap();
        assert!(host.text("/etc/systemd/system/xray.service.d/50-manual.conf").is_none(), "--force 才清理");
        assert!(report2.notes.iter().any(|n| n.contains("50-manual.conf")));
    }

    #[test]
    fn a_host_without_a_firewall_gets_a_note_and_still_reconciles_clean() {
        // 第三轮审查 C1 的回归：没有 ufw/firewalld 的机器上，「放行端口」只能落进 `notes`，
        // 绝不能落进 `changed`。写成 artifact 的话 apply 什么都改不了 → 不搬 `firewall` key →
        // diff 规则 8 每轮又出一条 `OpenPorts`：`bui reconcile --dry-run` 永远报改动，
        // Task 18 的 step2「二次对账零变更」在这类机器上恒 FAIL。
        let host = Arc::new(FakeHost::new());   // 没有 ufw、没有 firewalld
        host.with(|i| {
            i.files.insert("/root/.ssh/authorized_keys".into(), (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600));
        });
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (first, keys) = reconcile_once(
            input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(
            first.notes.iter().any(|n| n.contains("安全组") && n.contains("40000/udp")),
            "{:?}",
            first.notes
        );
        assert!(
            !first.changed.iter().any(|c| c.starts_with("firewall")),
            "没有防火墙时不该有 firewall 这条改动：{:?}",
            first.changed
        );
        assert!(!keys.contains_key("firewall"), "没改过防火墙就不该记 key：{keys:?}");
        let (second, _) =
            reconcile_once(input(&state, &reg.modules, &paths, &keys, &NoopInstaller), host.as_ref()).unwrap();
        assert!(second.is_clean(), "第二轮必须零变更：{second:?}");
        assert!(second.notes.iter().any(|n| n.contains("安全组")), "提示要每轮都在（只报不改）");
    }

    #[test]
    fn missing_pubkey_becomes_a_note_not_a_failure() {
        let host = Arc::new(FakeHost::new());   // 没有 authorized_keys
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let (report, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(report.notes.iter().any(|n| n.contains("harden-ssh")), "{:?}", report.notes);
        assert!(report.errors.is_empty());
        assert!(host.text("/etc/ssh/sshd_config.d/00-b-ui-hardening.conf").is_none());
    }

    #[test]
    fn binaries_come_from_the_manifest_when_versions_differ() {
        let host = ready_host();
        host.with(|i| {
            // 装的是旧 sing-box
            i.scripted.insert(0, ("/opt/b-ui/bin/sing-box version".into(), CmdOut::success("sing-box version 1.12.0\n")));
        });
        let m = Manifest {
            version: "4.0.0".into(),
            kernels: BTreeMap::from([("sing_box".to_string(), "1.13.19".to_string())]),
            artifacts: BTreeMap::from([(
                "sing-box-linux-amd64".to_string(),
                Asset { url: "https://x/sb".into(), sha256: "00".into() },
            )]),
            min_upgrade_from: None,
        };
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(Some(m));
        let (report, _) = reconcile_once(
            input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller),
            host.as_ref(),
        )
        .unwrap();
        assert!(report.changed.iter().any(|c| c.contains("sing-box")), "{:?}", report.changed);
    }

    #[test]
    fn dry_run_changes_nothing_on_disk() {
        let host = ready_host();
        let state = crate::testutil::sample_state();
        let paths = bui_schema::paths::Paths::default_server();
        let reg = modules(None);
        let mut i = input(&state, &reg.modules, &paths, &BTreeMap::new(), &NoopInstaller);
        i.dry_run = true;
        let (report, _) = reconcile_once(i, host.as_ref()).unwrap();
        assert!(!report.changed.is_empty());
        assert!(report.dry_run);
        assert!(host.text("/opt/b-ui/config.yaml").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn debounce_collapses_a_burst_into_one_trigger() {
        let bus = EventBus::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(debounce_loop(bus.clone(), tx));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        for _ in 0..5 {
            bus.send(Event::StateChanged("test"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert_eq!(rx.try_recv().ok(), Some(false), "五次变更压成一次（force=false）");
        assert!(rx.try_recv().is_err());
        bus.send(Event::ReconcileRequested { force: true });
        tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS + 50)).await;
        assert_eq!(rx.try_recv().ok(), Some(true), "force 要透传");
    }

    async fn ctx_for(host: Arc<FakeHost>, d: &tempfile::TempDir) -> DaemonCtx {
        let store = Store::create(d.path().join("state.json"), crate::testutil::sample_state()).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        DaemonCtx {
            store,
            runtime,
            bus: EventBus::new(),
            host,
            paths: bui_schema::paths::Paths {
                base_dir: d.path().into(),
                certs_dir: d.path().join("certs"),
                bin_dir: d.path().join("bin"),
            },
        }
    }

    #[tokio::test(start_paused = true)]
    async fn selfcheck_waits_for_its_jitter_then_refreshes_the_manifest() {
        let host = Arc::new(FakeHost::new());
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let node_id = ctx.store.read().await.node.id;
        let jitter = jitter_secs(node_id);
        let manifest_json = serde_json::json!({
            "version": "4.0.1",
            "kernels": { "sing_box": "1.14.2" },
            "artifacts": { "sing-box-linux-amd64": { "url": "https://x/sb", "sha256": "00" } }
        })
        .to_string();
        let fetcher: Arc<dyn Fetcher> =
            Arc::new(FakeFetcher(Mutex::new(vec![("https://x/manifest.json".into(), manifest_json.into_bytes())])));
        let handle = Arc::new(std::sync::RwLock::new(None));
        let mut events = ctx.bus.subscribe();
        let task = tokio::spawn(selfcheck_loop(
            ctx.clone(),
            handle.clone(),
            fetcher,
            "https://x/manifest.json".to_string(),
        ));
        // 抖动窗口内什么都不该发生
        tokio::time::sleep(std::time::Duration::from_secs(jitter.saturating_sub(1).max(1) - 1)).await;
        assert!(handle.read().unwrap().is_none(), "抖动没到就不该拉 manifest");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert_eq!(
            handle.read().unwrap().as_ref().map(|m| m.version.clone()),
            Some("4.0.1".to_string()),
            "共享句柄要被刷新，否则内核永远跟不上 manifest"
        );
        assert!(
            host.text(d.path().join("manifest.json").to_str().unwrap()).is_some(),
            "manifest 要落盘给下次启动用"
        );
        assert_eq!(ctx.runtime.read().await.upgrade_available.as_deref(), Some("4.0.1"));
        assert_eq!(events.recv().await.unwrap(), Event::ReconcileRequested { force: false });
        task.abort();
    }

    #[tokio::test]
    async fn the_daemon_restarts_itself_only_after_the_report_is_persisted() {
        // B5：apply 里不重启 b-ui；守护进程路径改成报告落盘后 `restart --no-block`。
        // 顺序颠倒（apply 里同步 restart 自己）会丢掉后面 5 条内核重启与整份报告。
        let host = ready_host();
        let d = tempfile::tempdir().unwrap();
        let ctx = ctx_for(host.clone(), &d).await;
        let reg = modules(None);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false).await.unwrap();
        assert!(report.self_restart_required);
        assert!(
            !host.ops().iter().any(|o| o.contains("restart") && o.contains("b-ui.service")),
            "对账本身不许动 b-ui：{:?}",
            host.ops()
        );
        assert!(ctx.runtime.read().await.last_reconcile.is_some(), "报告先落盘");
        finish_self_restart(&ctx, &report, true).await;
        assert_eq!(
            host.ops().last().map(String::as_str),
            Some("run:systemctl restart --no-block b-ui.service"),
            "守护进程内不能同步 restart 自己"
        );
    }

    #[test]
    fn jitter_is_deterministic_per_node_and_within_an_hour() {
        let a = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000001").unwrap();
        let b = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000002").unwrap();
        assert_eq!(jitter_secs(a), jitter_secs(a));
        assert_ne!(jitter_secs(a), jitter_secs(b));
        assert!(jitter_secs(a) < 3600 && jitter_secs(b) < 3600);
    }

    #[test]
    fn p1_registers_exactly_six_modules_and_shares_the_manifest_handle() {
        let reg = modules(None);
        let names: Vec<&str> = reg.modules.iter().map(|m| m.name()).collect();
        assert_eq!(names, vec!["core-files", "units", "system", "ssh", "certs", "watchdog"]);
        *reg.manifest.write().unwrap() = Some(Manifest {
            version: "4.0.1".into(),
            kernels: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            min_upgrade_from: None,
        });
        // 句柄与 core-files 模块里的是同一个锁：写进去以后 render 会看到
        let state = crate::testutil::sample_state();
        let ctx = crate::reconcile::RenderCtx {
            paths: bui_schema::paths::Paths::default_server(),
            facts: crate::reconcile::Facts::probe(&FakeHost::new()).unwrap(),
        };
        let _ = reg.modules[0].render(&state, &ctx);   // 不 panic 即证明锁未被 poison
        assert!(reg.manifest.read().unwrap().is_some());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui serve::`
Expected: 编译失败，`cannot find function reconcile_once`。

- [ ] **Step 3: 实现 `modules` 与 `reconcile_once`**

```rust
pub fn modules(manifest: Option<Manifest>) -> Registry {
    let core = CoreFilesModule::new(manifest);
    let handle = core.manifest_handle();
    Registry {
        modules: vec![
            Arc::new(core),
            Arc::new(UnitsModule),
            Arc::new(SystemModule),
            Arc::new(SshModule),
            Arc::new(CertsModule),
            Arc::new(WatchdogModule),
            // P2 在此追加 UsersModule（面板 API + 采样 + auth 快照）
            // P3 在此追加 ResidentialModule（体检 + 健康切换 + 黑名单）
        ],
        manifest: handle,
    }
}

pub fn reconcile_once(
    input: ReconcileInput<'_>,
    host: &dyn Host,
) -> anyhow::Result<(ReconcileReport, BTreeMap<String, String>)> {
    let facts = Facts::probe(host)?;
    let ctx = RenderCtx { paths: input.paths.clone(), facts };
    let mut artifacts = Vec::new();
    for m in input.modules {
        artifacts.extend(m.render(input.state, &ctx));
    }
    let installed = crate::kernels::installed_versions(host, &input.paths.bin_dir);
    let p = plan(
        PlanInput {
            artifacts: &artifacts,
            paths: input.paths,
            keys: input.keys,
            installed_versions: &installed,
        },
        host,
    )?;
    let mut keys = input.keys.clone();
    let out = apply(
        ApplyInput {
            plan: p,
            paths: input.paths,
            facts: &ctx.facts,
            installer: input.installer,
            dry_run: input.dry_run,
        },
        host,
    );
    keys.extend(out.keys.clone());
    let drift = crate::reconcile::drift::scan(host, &artifacts, input.paths);
    let mut notes = out.notes.clone();
    if input.force && !drift.is_empty() && !input.dry_run {
        notes.extend(crate::reconcile::drift::clean(host, &drift));
    }
    if input.state.system.ssh_hardening && ctx.facts.ssh_pubkeys == 0 {
        notes.push(
            "未在 /root/.ssh/authorized_keys 检测到公钥，已跳过 SSH 硬化；加好公钥后运行 `b-ui harden-ssh`".into(),
        );
    }
    // spec §2.2「防火墙：没装就在体检里提示」。判据与 Task 6 的 render、Task 5 第 10 步的两支
    // 完全一致（`*_active`）。放在这里而不是产出 `FirewallPorts` artifact：notes 不进 `changed`、
    // 不影响 `/api/health` 的 degraded 判定，于是「每轮提醒」不等于「每轮有改动」——
    // 没有防火墙的机器仍然满足 M1 的「二次对账零变更」（第三轮审查 C1）。
    if input.state.system.firewall != "off" && !ctx.facts.ufw_active && !ctx.facts.firewalld_active {
        let specs: Vec<String> = crate::modules::system::firewall_ports(&input.state.node.ports)
            .iter()
            .map(|p| p.ufw())
            .collect();
        notes.push(format!("未检测到 ufw/firewalld，请在云厂商安全组放行：{}", specs.join(", ")));
    }
    let report = ReconcileReport {
        at: crate::util::fmt_rfc3339(host.now()),
        changed: out.changed,
        restarted: out.restarted,
        notes,
        verify_failures: out.verify_failures,
        errors: out.errors,
        drift: if input.force && !input.dry_run { Vec::new() } else { drift },
        dry_run: input.dry_run,
        self_restart_required: out.self_restart_required && !input.dry_run,
    };
    if out.relay_restarted {
        tracing::info!("b-ui-relay 已重启，需要重放选中的住宅上游");
    }
    Ok((report, keys))
}
```
`out.relay_restarted` 在 `reconcile_from_ctx` 里转成 `ctx.bus.send(Event::RelayRestarted)`（见下一步）。

- [ ] **Step 4: 实现 `reconcile_from_ctx` / `debounce_loop` / `selfcheck_loop`**

```rust
pub async fn reconcile_from_ctx(
    ctx: &DaemonCtx,
    modules: &[Arc<dyn Module>],
    fetcher: Arc<dyn Fetcher>,
    force: bool,
    dry_run: bool,
) -> anyhow::Result<ReconcileReport> {
    let state = ctx.store.read().await;
    let keys = ctx.runtime.read().await.restart_keys;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let mods = modules.to_vec();
    // 整轮对账（含 reqwest::blocking 下载内核）都在阻塞线程里跑：
    // reqwest 文档明确 blocking 客户端在 async runtime 里会 panic。
    let (report, keys) = tokio::task::spawn_blocking(move || {
        let installer = KernelInstaller { fetcher: fetcher.as_ref(), host: host.as_ref() };
        reconcile_once(
            ReconcileInput {
                state: &state,
                modules: &mods,
                paths: &paths,
                keys: &keys,
                installer: &installer,
                force,
                dry_run,
            },
            host.as_ref(),
        )
    })
    .await??;
    if !dry_run {
        ctx.runtime
            .update(|r| {
                r.restart_keys = keys;
                r.drift = report.drift.clone();
                r.last_reconcile = Some(report.clone());
            })
            .await;
    }
    if report.restarted.iter().any(|u| u == "b-ui-relay") {
        ctx.bus.send(Event::RelayRestarted);
    }
    Ok(report)
}

pub async fn finish_self_restart(ctx: &DaemonCtx, report: &ReconcileReport, in_daemon: bool) {
    if !report.self_restart_required || report.dry_run {
        return;
    }
    let host = ctx.host.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if in_daemon {
            // 本进程就是 b-ui：--no-block 让 systemd 排队，本轮的收尾代码先跑完
            host.run("systemctl", &["restart", "--no-block", "b-ui.service"])
                .map(|o| o.ok())
                .unwrap_or(false)
        } else {
            host.systemd("restart", "b-ui").map(|o| o.ok()).unwrap_or(false)
        }
    })
    .await;
    tracing::info!(in_daemon, "b-ui.service 自身需要重启，已提交");
}
```
（`state` 是 `Arc<State>`、`keys` 是 `BTreeMap`、`fetcher` 是 `Arc<dyn Fetcher>`、`host` 是 `Arc<dyn Host>`、`mods` 是 `Vec<Arc<dyn Module>>`——全部 `Send + 'static`，闭包按值捕获，没有借用跨越 `spawn_blocking` 边界的生命周期问题。）

```rust
pub async fn debounce_loop(bus: EventBus, tx: tokio::sync::mpsc::Sender<bool>) {
    let mut rx = bus.subscribe();
    loop {
        let force = match rx.recv().await {
            Ok(Event::StateChanged(_)) => false,
            Ok(Event::ReconcileRequested { force }) => force,
            Ok(Event::RelayRestarted) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
        };
        // 去抖窗口内把后续事件吞掉，force 取或
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(DEBOUNCE_MS);
        let mut force = force;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(Event::ReconcileRequested { force: f })) => force |= f,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        let _ = tx.send(force).await;
    }
}

pub async fn selfcheck_loop(
    ctx: DaemonCtx,
    manifest: Arc<std::sync::RwLock<Option<Manifest>>>,
    fetcher: Arc<dyn Fetcher>,
    url: String,
) {
    let node_id = ctx.store.read().await.node.id;
    // 按节点 id 定死的抖动，避免所有机器同一秒打 GitHub（spec §7）
    tokio::time::sleep(std::time::Duration::from_secs(jitter_secs(node_id))).await;
    loop {
        let f = fetcher.clone();
        let u = url.clone();
        match tokio::task::spawn_blocking(move || Manifest::from_url(f.as_ref(), &u)).await {
            Ok(Ok(m)) => {
                // 写缓存：下次启动直接用，拉不到网也能装内核
                let host = ctx.host.clone();
                let path = crate::paths::manifest_file(&ctx.paths);
                match serde_json::to_vec_pretty(&m) {
                    Ok(bytes) => {
                        let h = host.clone();
                        let p = path.clone();
                        let _ = tokio::task::spawn_blocking(move || h.write_file(&p, &bytes, 0o644)).await;
                    }
                    Err(e) => tracing::warn!(error = %e, "manifest 序列化失败"),
                }
                let newer = (m.version != env!("CARGO_PKG_VERSION")).then(|| m.version.clone());
                if let Ok(mut guard) = manifest.write() {
                    *guard = Some(m);
                }
                ctx.runtime.update(|r| r.upgrade_available = newer.clone()).await;
                if let Some(v) = &newer {
                    tracing::info!(version = %v, "每日自检：有新版 bui，可运行 `b-ui upgrade`");
                }
                // 内核版本随 manifest 走：请求一次对账（不 force）
                ctx.bus.send(Event::ReconcileRequested { force: false });
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "每日自检拉取 manifest 失败"),
            Err(e) => tracing::warn!(error = %e, "每日自检任务 panic"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(SELFCHECK_INTERVAL_SECS)).await;
    }
}
```

- [ ] **Step 5: 实现 `run` 与 `reconcile_cli`**

```rust
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()> {
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let bus = EventBus::new();
    let cached = {
        let h = host.clone();
        let p = paths.clone();
        tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p)).await?
    };
    let reg = modules(cached);
    let mods = reg.modules.clone();
    let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new());
    let ctx = DaemonCtx {
        store: store.clone(),
        runtime: runtime.clone(),
        bus: bus.clone(),
        host: host.clone(),
        paths: paths.clone(),
    };
    let app_state = AppState {
        store,
        bus: bus.clone(),
        runtime: runtime.clone(),
        host: host.clone(),
        started_at: host.now(),
        version: env!("CARGO_PKG_VERSION"),
        login: crate::api::auth::LoginLimiter::default(),
    };
    runtime.update(|r| r.started_at = Some(crate::util::fmt_rfc3339(host.now()))).await;
    let app = crate::api::router(app_state, &mods);
    // 启动时先对账一次，再拉起后台任务
    match reconcile_from_ctx(&ctx, &mods, fetcher.clone(), false, false).await {
        Ok(r) => {
            tracing::info!(changed = r.changed.len(), restarted = r.restarted.len(), "启动对账完成");
            // 报告已落盘，这时才允许重启自己（B5）
            finish_self_restart(&ctx, &r, true).await;
        }
        Err(e) => tracing::error!(error = %e, "启动对账失败"),
    }
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for m in &mods {
        tasks.extend(m.spawn(ctx.clone()));
    }
    // 守护进程里**只有这一个** consumer 会调 reconcile_from_ctx（启动那一轮在它之前、串行跑完）：
    // 去抖触发、10 分钟巡检、每日自检（经 bus → 去抖）全部经这条 mpsc 排队，天然互斥。
    // 若让 10 分钟 tick 自己起一个任务直接对账，就会与去抖触发的那一轮并发——两轮同时
    // restart 同一个单元、`.verify/<file>` 候选文件互相覆盖、`runtime.json` 交叉写。
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    tasks.push(tokio::spawn(debounce_loop(bus.clone(), tx.clone())));
    {
        let (ctx2, mods2, f2) = (ctx.clone(), mods.clone(), fetcher.clone());
        tasks.push(tokio::spawn(async move {
            while let Some(force) = rx.recv().await {
                match reconcile_from_ctx(&ctx2, &mods2, f2.clone(), force, false).await {
                    Ok(r) => finish_self_restart(&ctx2, &r, true).await,
                    Err(e) => tracing::error!(error = %e, "对账失败"),
                }
            }
        }));
    }
    {
        // 10 分钟巡检只往同一条队列里投一次触发，不自己对账
        let tick_tx = tx.clone();
        tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(DRIFT_INTERVAL_SECS));
            tick.tick().await;
            loop {
                tick.tick().await;
                if tick_tx.send(false).await.is_err() {
                    return;
                }
            }
        }));
    }
    // spec §7：守护进程每日带抖动自检（刷新 manifest → 下一轮对账按新版本装内核）
    tasks.push(tokio::spawn(selfcheck_loop(
        ctx.clone(),
        reg.manifest.clone(),
        fetcher.clone(),
        // 总纲 C4：`$BUI_MANIFEST_URL` 可覆盖（M1 时内置 URL 必然 404，演练/装机都靠它）
        crate::kernels::manifest_url(None, None),
    )));
    let sock = PathBuf::from(crate::paths::SOCKET_PATH);
    let admin_bind = format!("127.0.0.1:{}", ctx.store.read().await.node.ports.admin);
    let http = tokio::net::TcpListener::bind(&admin_bind).await?;
    tracing::info!(bind = %admin_bind, "面板 HTTP 就绪（Caddy 反代）");
    let app_uds = app.clone();
    tasks.push(tokio::spawn(async move {
        if let Err(e) = crate::ipc::serve_uds(&sock, app_uds).await {
            tracing::error!(error = %e, "socket 服务退出");
        }
    }));
    // ConnectInfo 必须挂上，否则 auth::client_ip 拿不到对端地址（限速退化成一个桶）
    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    // systemd stop / restart 发的是 SIGTERM，不是 SIGINT：只 select ctrl_c 的话
    // `systemctl restart b-ui` 时后台任务不会走 abort 路径（socket 文件也不会被清掉）。
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = axum::serve(http, service).into_future() => { r?; }
        _ = sigterm.recv() => tracing::info!("收到 SIGTERM，退出"),
        _ = tokio::signal::ctrl_c() => tracing::info!("收到中断，退出"),
    }
    for t in tasks {
        t.abort();
    }
    Ok(())
}

/// `<base>/manifest.json`（由 install / upgrade / 每日自检写入）；没有就返回 None，对账跳过 Binary
pub fn load_cached_manifest(host: &dyn Host, paths: &Paths) -> Option<Manifest> {
    let bytes = host.read_file(&crate::paths::manifest_file(paths)).ok().flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// 每日自检的抖动：sha256(node_id) 取前 8 字节对 3600 取模，同一台机器永远同一个偏移。
pub fn jitter_secs(node_id: uuid::Uuid) -> u64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(node_id.as_bytes());
    let n = u64::from_be_bytes(digest[..8].try_into().expect("sha256 至少 8 字节"));
    n % 3600
}

pub async fn reconcile_cli(
    paths: Paths,
    host: Arc<dyn Host>,
    socket: PathBuf,
    force: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::Client::new(&socket);
    if client.available().await && !dry_run {
        let (status, body) = client
            .request("POST", "/api/reconcile", Some(serde_json::json!({"force": force, "dry_run": false})))
            .await?;
        println!("已提交给守护进程（HTTP {status}）：{body}");
        return Ok(());
    }
    let store = Store::open(crate::paths::state_file(&paths)).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let ctx = DaemonCtx { store, runtime, bus: EventBus::new(), host: host.clone(), paths: paths.clone() };
    let cached = {
        let h = host.clone();
        let p = paths.clone();
        tokio::task::spawn_blocking(move || load_cached_manifest(h.as_ref(), &p)).await?
    };
    let reg = modules(cached);
    let fetcher: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new());
    let report = reconcile_from_ctx(&ctx, &reg.modules, fetcher, force, dry_run).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    // CLI 路径：报告与 restart_keys 已落盘，这时同步重启守护进程（B5）
    finish_self_restart(&ctx, &report, false).await;
    if !report.errors.is_empty() || !report.verify_failures.is_empty() {
        anyhow::bail!("对账有失败项，见上面的报告");
    }
    Ok(())
}
```
`main.rs`：`Command::Serve => serve::run(paths, host).await`；`Command::Reconcile { force, dry_run } => serve::reconcile_cli(paths, host, PathBuf::from(crate::paths::SOCKET_PATH), force, dry_run).await`（`main.rs` 顶部随之加一行 `use std::path::PathBuf;`）。

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui serve:: && cargo test -p bui && cargo clippy -p bui --all-targets -- -D warnings`
Expected: serve 12 passed；`cargo test -p bui` 全绿。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/serve.rs crates/bui/src/main.rs
git commit -m "feat(bui): serve 装配（模块注册、500ms 去抖、10 分钟漂移巡检、每日带抖动自检）"
```

---

### Task 16: `bui install` 与 `bui import-v3`

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/commands/install.rs`, `crates/bui/src/commands/import_v3.rs`
- Modify: `crates/bui/src/main.rs`（`Command::Install` 与 `Command::ImportV3` 两个 arm 落地）

**Interfaces:**
- Consumes: `bui_schema::v3::import(dir: &Path) -> Result<ImportReport, ImportError>`（P0 Task 3；`ImportReport { state: State, warnings: Vec<String> }`）、`crate::kernels::{Manifest, Fetcher, HttpFetcher, KernelInstaller, manifest_url, asset_arch, installed_versions}`、`crate::serve::{modules, reconcile_from_ctx, finish_self_restart}`、`crate::ipc::Client`（第 9 步在守护进程已运行时改走 `POST /api/reconcile`）、`crate::api::auth::hash_password`、`crate::paths::{auth_snapshot_file, caddy_data, v3_backup_dir}`
- Produces:
```rust
// crate::commands::install
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    pub domain: String, pub admin_password: String, pub node_name: String, pub public_ip: String,
    pub ports: Ports, pub masquerade: String,
}
impl Answers { pub fn defaults(hostname: &str, public_ip: &str) -> Answers }
//   端口 10000/20000-30000/40000/41000-50000/10001/10002/8080，masquerade www.bing.com:443，
//   node_name = hostname，domain 空串，admin_password **空串**（密码只由 --admin-password-stdin 或随机生成填）
/// `--non-interactive --answers <file>` 的文件形状（总纲 C5 只给了参数名，格式在这里定死，
/// 因为 P5 的 `scripts/ops/v3-cutover.sh` 要照它生成）。所有键都可省，省掉的沿用 `defaults`：
/// ```jsonc
/// { "domain": "example.com", "node_name": "node-a", "public_ip": "203.0.113.10",
///   "masquerade": "www.bing.com:443",
///   "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000,
///              "hy2_resi_hop": [41000, 50000], "reality_direct": 10001,
///              "reality_resi": 10002, "admin": 8080 } }
/// ```
/// `ports` 给了就必须**给全**（`bui_schema::model::Ports` 除 `hy2_hop` 外没有 serde 默认值）。
/// **文件里没有管理员密码**：它会留在磁盘上、还会进 P5 脚本的 git 历史，所以密码只能经
/// `--admin-password-stdin` 从 stdin 读；两者都没给时生成随机密码并打印一次（不写日志）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswersFile {
    pub domain: Option<String>, pub node_name: Option<String>, pub public_ip: Option<String>,
    pub masquerade: Option<String>, pub ports: Option<Ports>,
}
pub fn load_answers(path: &Path, defaults: Answers) -> anyhow::Result<Answers>;
pub struct InstallOpts {
    pub domain: Option<String>, pub port: Option<u16>, pub admin_password_stdin: bool,
    pub import_v3: Option<PathBuf>, pub non_interactive: bool, pub answers: Option<PathBuf>, pub yes: bool,
    /// 第 9 步探测守护进程用的 unix socket。**必须由调用方传入**（`main.rs` 传
    /// `PathBuf::from(crate::paths::SOCKET_PATH)`），不许在函数体里读常量：五处 install 测试都会
    /// 走到第 9 步，硬编码 `/run/b-ui.sock` 会让它们在跑着 v4 守护进程的机器上（M1 之后的
    /// bwg-rick）真的 `connect()` 并真发一次 `POST /api/reconcile`，违反 Global Constraints 的
    /// 「单元测试不碰真实系统」（第三轮审查 C3）。测试传 `d.path().join("absent.sock")`。
    pub socket: PathBuf,
}
impl InstallOpts { pub fn quiet(&self) -> bool { self.yes || self.non_interactive } }   // 一个问题都不问
/// `xray x25519` 的两种输出格式都要认
pub fn parse_x25519(stdout: &str) -> Option<(String, String)>;
pub fn random_short_id() -> String;                       // 8 字节随机 → 16 位 hex
pub fn random_hex(bytes: usize) -> String;
pub fn build_state(answers: &Answers, reality: Reality, versions: Versions) -> anyhow::Result<State>;
/// hysteria 鉴权钩子读的快照（spec §3.2）。形状**逐字照总纲 C5**：
/// `{"schema":1,"users":{"<username>":{user_id,hy2_password,expires_at,blocked}}}`；
/// 没有用户时 `users` 是空对象。P2 的钩子与快照重写按同一形状（见文首契约段）
pub fn auth_snapshot(state: &State) -> serde_json::Value;
pub async fn run(opts: InstallOpts, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
/// `manifest_url` 由 `run` 用 `crate::kernels::manifest_url(None, None)` 算好传进来
/// （总纲 C4：`$BUI_MANIFEST_URL` 可覆盖）；测试直接传固定值，不读进程环境
pub async fn run_with(opts: InstallOpts, answers: Answers, manifest_url: String, paths: Paths, host: Arc<dyn Host>, fetcher: Arc<dyn Fetcher>) -> anyhow::Result<()>;
// crate::commands::import_v3
/// 要停用并删除的 v3 单元。**不含 `b-ui-relay.service`**：v4 原地重写同名单元，
/// 由内容 diff 决定是否重启；把它列进来会在装好 v4 之后又把 relay 卸掉，住宅两个节点一起断。
pub const V3_UNITS: [&str; 7] = [
    "b-ui-admin.service", "b-ui-cert-sync.timer", "b-ui-cert-sync.service",
    "hy2-watchdog.timer", "hy2-watchdog.service",
    "b-ui-resi-health.timer", "b-ui-resi-health.service",
];
/// v3 的 shell / Node / 引导文件与它自带的 sing-box 二进制：直接删。
/// 每一项都是从仓库里的 v3 脚本核实过的（第三轮审查 C2），不是凭记忆列的：
/// `sing-box` —— v3 的 relay 二进制在 **`${BASE_DIR}/sing-box`**（顶层，不在 `bin/`；
///   `server/residential-helper.sh:35`），v4 的那份在 `<base>/bin/sing-box`，所以删顶层这个不影响 v4；
/// `cert-check.sh` —— `server/core.sh:1170-1217` 写出并 `chmod +x`，`update.sh:2044-2066` 还给它挂了
///   一条 12 小时 cron（cron 行由 `filter_cron` 一并清掉）。
/// 少一项就会在 `<base>` 顶层留一个永久 `stray_file`：每 10 分钟一条漂移 → `/api/health` 恒
/// `degraded` → M1 的「体检无漂移」不可达（spec §2.3 + §9）。
pub const V3_FILES: [&str; 13] = [
    "core.sh", "update.sh", "b-ui-cli.sh", "residential-helper.sh", "resi-health.sh",
    "hy2-watchdog.sh", "cert-sync.sh", "cert-check.sh", "hy2-portjump-cleanup.sh", "install-key.txt",
    "b-ui-client.sh", "version.json", "sing-box",
];
/// v3 的状态文件：移进 `<base>/v3-backup/`（0700）而不是删——出问题要能回查，
/// 留在原地则被 §2.2 的漂移扫描永久报告。同样逐条核实过（第三轮审查 C2）：
/// `port-hopping.json`（`core.sh:1422` 写，`update.sh:801-808/1315-1321`、`web/server.js:434` 读）、
/// `masquerade.json`（`core.sh:773` 写，`web/server.js:2567` 读）、
/// `server_ip.txt`（`web/server.js:73` 读，P0 的 v3 fixture 就带一个）。
/// 前两个不含秘密但含节点配置，第三个是公网 IP：一起归档，既不丢线索也不留漂移。
pub const V3_STATE_FILES: [&str; 9] = [
    "users.json", "reality-keys.json", "residential-proxy.json", "admin.env",
    ".resi-health-state.json", ".relay.lock",
    "port-hopping.json", "masquerade.json", "server_ip.txt",
];
/// v3 的迁移块与 CLI 留在 `<base>` 顶层的**备份 / 临时文件**的基名。只认这六个基名，
/// 别人的文件一概不碰。核实来源（第三轮审查 C2）：
/// `server/update.sh:682/703/734`（`*.bak.v360.<ts>`，三个 config）、`:774`（`*.bak.<ts>`）、
/// `:858`（`*.bak.broken-<ts>`）、`:1660`（`config.yaml.bak.v357.<ts>`）、
/// `:1674`（`xray-config.json.bak.<ts>`）、`:1709`（`xray-config.json.bak.v359.<ts>`）、
/// `server/b-ui-cli.sh:939/963`（`config.yaml.bak.obfs.<ts>`）；
/// `.tmp` 来自 `core.sh`/`update.sh` 的 `config.yaml.tmp` / `config.yaml.v357.tmp` /
/// `xray-config.json.tmp` 与 `residential-helper.sh:455/500/565` 的原子写中断残留。
/// `*.bak.*` 里有 HY2 明文密码与 UUID → **归档**进 `v3-backup/`；`.tmp` 是半成品 → **删除**。
pub const V3_LEFTOVER_PREFIXES: [&str; 6] = [
    "config.yaml", "config-residential.yaml", "xray-config.json",
    "singbox-relay.json", "residential-proxy.json", ".resi-health-state.json",
];
/// 扫 `<base>` **顶层**（`list_dir` 只给直接子项），把 `V3_LEFTOVER_PREFIXES` 里某个基名后面
/// 跟着 `.bak.…` 的文件归档进 `v3-backup/`（0600）、跟着 `.tmp` 结尾的删掉；文件名恰好等于基名
/// 本身的（就是 v4 自己在管的那四个配置）绝不碰。返回每条处置的说明。
/// 漂移扫描只跳过 `.tmp` / `.new` 后缀，**不跳过 `*.bak.*`**，所以不清掉这些备份，
/// 导入后的机器每 10 分钟就会报一串 `stray_file`（spec §2.3 + §9）。
pub fn sweep_v3_leftovers(host: &dyn Host, paths: &Paths) -> Vec<String>;
/// 只删 b-ui 自己写的 cron 行
pub fn filter_cron(text: &str) -> String;
/// v3 早期版本用 iptables/nft 的 REDIRECT 链做端口跳跃（`hy2-portjump-cleanup.sh` 负责清理，
/// v4 把这个脚本删了，spec §3.1 也要求 v4「不再有任何 iptables/nft 规则」由 b-ui 自己写）。
/// 只删两类**孤儿**：`iptables`/`ip6tables` 的 `nat` 表里 `HYSTERIA-PR-*` 链（移植
/// `server/core.sh:159-178`：跳转规则先 `-D`，再 `-F` + `-X`），以及 `nft` 里
/// `hysteria_*` 表（`server/core.sh:180-186`）。命令不存在 / 删不掉 → 只记一行说明，不算错误。
/// **注意**：hysteria 2.12 自己会为内置端口跳跃创建同名链并在 shutdown 时清理，所以
/// ① 这一步必须排在 `uninstall_v3` 的**最后**（紧接着的对账会重写两个 hysteria 单元并重启，
/// 启动时 hysteria 自建所需的链）；② 漂移扫描**不**看这些链——v4 运行中的 hysteria 正当持有它们。
pub fn flush_v3_portjump_rules(host: &dyn Host) -> Vec<String>;
/// 顺序：`migrate_caddy_data` → 停 v3 单元 → 删 v3 shell/Node 文件 → 归档 v3 状态文件 →
/// `sweep_v3_leftovers`（`*.bak.*` 归档、`*.tmp` 删） → 删 v3 `admin/` → 删 `/tmp/hy2-watchdog-*` →
/// 清 cron 行 → `flush_v3_portjump_rules`；保留 certs/ 与 packages/
pub fn uninstall_v3(host: &dyn Host, paths: &Paths) -> Vec<String>;
/// 发行版 Caddy（v3 用的那个）的数据目录：ACME 账号与已签证书都在它下面
pub const V3_CADDY_DATA: &str = "/var/lib/caddy/.local/share/caddy";
/// 把 `V3_CADDY_DATA` 整棵复制到 `<base>/caddy/caddy/`（目标已有同名文件则跳过，不覆盖），
/// **复制完才** `systemctl stop caddy`；`uninstall_v3` 的第一步
pub fn migrate_caddy_data(host: &dyn Host, paths: &Paths) -> Vec<String>;
pub async fn run(dir: PathBuf, out: Option<PathBuf>, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
```
`bui install` 的步骤（spec §7）：
1. `state.json` 已存在 → 不覆盖，直接对账并打印「已安装，执行对账」（幂等：第二次运行零变更）。
2. 收集 `Answers`：`--answers <file>` 给了就 `load_answers(file, Answers::defaults(hostname, public_ip))`；`opts.quiet()`（`--non-interactive` 或 `-y`）或非 TTY 时用默认值，一个问题都不问；否则逐项交互。`--domain` / `--port` 覆盖对应项（命令行优先于文件）。密码经 `--admin-password-stdin` 从 stdin 读，绝不进 argv、也绝不进答案文件。
3. `Manifest::from_url(fetcher, &manifest_url)` → 成功则缓存到 `<base>/manifest.json`（**内容与现有文件相同就不写**，否则第二次 install 不是零写入）；失败只 `warn!` 并继续（内核用现有的，对账跳过 `Binary`）。`manifest_url` 是 `run` 用 `crate::kernels::manifest_url(None, None)` 算出来的（总纲 C4：`$BUI_MANIFEST_URL` 可覆盖；裁决「M5 前不创建任何 Release/tag」意味着内置 URL 在 M1 必然 404，所以 bwg-rick 上必须用这个环境变量指向本机托管的 CI 产物，见 Task 18 清单）。
4. 用 `KernelInstaller` 先把四个内核装到 `bin/`（生成 REALITY 密钥需要 xray；也让第 9 步的对账能真跑内核校验）。装完 `installed_versions(host, bin_dir)` 不足四项 → 打印一行 `缺少内核：<名字列表>；请设 BUI_MANIFEST_URL 指向可用的 manifest，或把二进制放进 <bin>/ 后重跑 bui install`，继续往下走（不中断：`--import-v3` 的机器上四个内核可能已在 PATH 里，对账失败项会在第 9 步集中报出来）。
5. `--import-v3` → `bui_schema::v3::import(dir)`，warnings 打印；否则 `generate_reality` 生成密钥 + `build_state`。
6. `Store::create` 写 `state.json`（0600）。
7. **`--import-v3` → `uninstall_v3`（在对账之前）**。顺序是硬要求：v3 的 `b-ui-admin` 还占着 `:8080` 时，第 9 步对账 start 的 `b-ui.service` 首启 bind 失败会进 `Restart=always` 循环；v3 的 timer 也会在对账期间继续改文件。`uninstall_v3` 的第一步是 `migrate_caddy_data`（先把发行版 Caddy 的 ACME 账号与证书搬到 `<base>/caddy/caddy/`，再停发行版 caddy），这是 2026-09-12 裁决的硬要求——顺序颠倒就会重新签发并可能撞上 Let's Encrypt 速率限制。
8. 写初版 `auth-snapshot.json`（0600；内容与现有文件相同就不写，保证第二次 install 零写入）。形状按总纲 C5 的 `{"schema":1,"users":{…}}`（**不是**顶层直接以用户名为键的扁平对象——P2 的钩子按 C5 读，形状不对等于导入的用户一律 fail-closed）。hysteria 的 `auth.type: command` 指向 `bui auth-hook`，快照缺失时钩子 fail-closed、所有人连不上；P2 合并前靠这一步让导入的用户立刻能连（结构见文首契约段）。
9. 对账：`ipc::Client::available()` 为真（守护进程已经在跑，典型是在活机器上重跑 `bui install`）→ `POST /api/reconcile {"force":false,"dry_run":false}` 交给守护进程，打印它回的报告；否则（首装，socket 还不存在）在本进程内 `reconcile_from_ctx` 跑一次完整对账，再 `finish_self_restart(&ctx, &report, false)`。**不允许在守护进程已运行时于本进程内再跑一轮**：两边会同时 restart 同一个单元、`.verify/<file>` 候选文件互相覆盖、`runtime.json` 交叉写（与 `serve::reconcile_cli` 同一条口径）。
10. 打印面板地址与后续命令（`b-ui` 进菜单、`bui status`）。

第 2 步的 `Answers` 里**没有**「可选住宅 URL」（spec §7 的交互项之一）：住宅上游的解析、探测与入池全在 P3（`bui residential add`），P3 的第一个任务负责往 `install` 的交互里追加这一问并调自己的入池函数。P1 装出来的机器住宅组是 `enabled=false` 的空池，relay 全直连（fail-open），不影响四个节点可用。

- [ ] **Step 1: 写失败测试（install，全程 tempdir，不碰真实 `/opt/b-ui`）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::Fetcher;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;
    use std::sync::{Arc, Mutex};

    // 本机实测：xray 26.3.27 的输出
    const X25519: &str = "PrivateKey: CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4\nPassword (PublicKey): cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c\nHash32: 3Vnzmd-rq4njP5IfMf_wYgFrGEuMBpJqgOh-UAdLOUY\n";
    // 老版本 xray 的输出
    const X25519_OLD: &str = "Private key: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nPublic key: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";

    #[test]
    fn parses_both_x25519_output_formats() {
        assert_eq!(
            parse_x25519(X25519),
            Some((
                "CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4".to_string(),
                "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c".to_string()
            ))
        );
        assert_eq!(
            parse_x25519(X25519_OLD),
            Some((
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
            ))
        );
        assert_eq!(parse_x25519("nothing useful"), None);
    }

    #[test]
    fn random_values_have_the_right_shape() {
        let a = random_short_id();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, random_short_id(), "两次生成不应相同");
        assert_eq!(random_hex(32).len(), 64);
    }

    #[test]
    fn default_answers_match_the_v3_port_layout() {
        let a = Answers::defaults("node-a", "203.0.113.10");
        assert_eq!(a.ports.hy2, 10000);
        assert_eq!(a.ports.hy2_hop, Some((20000, 30000)));
        assert_eq!(a.ports.hy2_resi, 40000);
        assert_eq!(a.ports.hy2_resi_hop, (41000, 50000));
        assert_eq!(a.ports.reality_direct, 10001);
        assert_eq!(a.ports.reality_resi, 10002);
        assert_eq!(a.ports.admin, 8080);
        assert_eq!(a.masquerade, "www.bing.com:443");
        assert_eq!(a.node_name, "node-a");
    }

    #[test]
    fn build_state_hashes_the_password_and_fills_the_node() {
        let answers = Answers {
            domain: "example.com".into(),
            admin_password: "test123".into(),
            ..Answers::defaults("node-a", "203.0.113.10")
        };
        let reality = bui_schema::model::Reality {
            private_key: "priv".into(),
            public_key: "pub".into(),
            short_ids: vec![random_short_id()],
            // generate_reality 给的就是空占位，两项都由 build_state 按 answers.masquerade 填
            dest: String::new(),
            server_names: Vec::new(),
        };
        let versions = bui_schema::model::Versions { bui: "4.0.0".into(), ..Default::default() };
        let s = build_state(&answers, reality, versions).unwrap();
        assert_eq!(s.schema_version, bui_schema::model::SCHEMA_VERSION);
        assert_eq!(s.node.domain, "example.com");
        assert_eq!(s.node.name, "node-a");
        assert_eq!(s.node.public_ip, "203.0.113.10");
        assert_eq!(s.node.reality.dest, "www.bing.com:443");
        assert_eq!(
            s.node.reality.server_names,
            vec!["www.bing.com".to_string()],
            "serverNames 必须与 dest 同源，否则用户改了伪装域名 REALITY 握手就失败"
        );
        assert_eq!(s.node.reality.short_ids[0].len(), 16);
        assert!(s.admin.password_hash.starts_with("$argon2id$"));
        assert!(crate::api::auth::verify_password(&s.admin.password_hash, "test123"));
        assert_eq!(s.admin.jwt_secret.len(), 64);
        assert!(s.users.is_empty());
        assert!(s.residential.default_group().is_some());
        assert!(s.system.ssh_hardening && s.system.static_dns);
        assert_eq!(s.versions.bui, "4.0.0");
    }

    struct FakeFetcher(Mutex<Vec<(String, Vec<u8>)>>);
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    fn fetcher_with_manifest() -> Arc<dyn Fetcher> {
        let payload = b"ELF".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let asset = |n: &str| serde_json::json!({"url": format!("https://x/{n}"), "sha256": sum});
        let manifest = serde_json::json!({
            "version": "4.0.0",
            "kernels": { "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2" },
            "artifacts": {
                "bui-linux-amd64": asset("bui"),
                "hysteria-linux-amd64": asset("hysteria"),
                "xray-linux-amd64": asset("xray"),
                "sing-box-linux-amd64": asset("sing-box"),
                "caddy-linux-amd64": asset("caddy")
            }
        });
        let mut files = vec![(crate::kernels::MANIFEST_URL.to_string(), manifest.to_string().into_bytes())];
        for n in ["bui", "hysteria", "xray", "sing-box", "caddy"] {
            files.push((format!("https://x/{n}"), payload.clone()));
        }
        Arc::new(FakeFetcher(Mutex::new(files)))
    }

    /// 全部路径都在 tempdir 下：Global Constraints 的「单元测试不碰真实系统」
    fn scratch(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    /// 装机用的假机器：公钥在位；`bin/` 下的 xray 被脚本化（脚本键按 tempdir 拼）
    fn host_for_install(paths: &bui_schema::paths::Paths) -> Arc<FakeHost> {
        let bin = |n: &str| paths.bin_dir.join(n).display().to_string();
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            i.files.insert("/root/.ssh/authorized_keys".into(), (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600));
            i.scripted.push((format!("{} x25519", bin("xray")), CmdOut::success(X25519)));
            i.scripted.push((format!("{} version", bin("xray")), CmdOut::success("Xray 26.3.27 (Xray) a (go1 linux/amd64)\n")));
            i.scripted.push((format!("{} version", bin("hysteria")), CmdOut::success("Version:\tv2.12.2\n")));
            i.scripted.push((format!("{} version", bin("sing-box")), CmdOut::success("sing-box version 1.13.19\n")));
            i.scripted.push((format!("{} version", bin("caddy")), CmdOut::success("v2.10.2 h1:x\n")));
            i.scripted.push(("curl".into(), CmdOut::success("203.0.113.10")));
        });
        h
    }

    /// `socket` 指向 tempdir 里一个**不存在**的路径：第 9 步的 `Client::available()` 立刻返回
    /// false，对账走进程内那一支，测试全程不碰 `/run/b-ui.sock`
    fn opts(d: &tempfile::TempDir) -> InstallOpts {
        InstallOpts {
            domain: Some("example.com".into()),
            port: None,
            admin_password_stdin: false,
            import_v3: None,
            non_interactive: false,
            answers: None,
            yes: true,
            socket: d.path().join("absent.sock"),
        }
    }

    /// 测试一律显式传 manifest 地址，不读进程环境（`$BUI_MANIFEST_URL` 在开发机上可能有值）
    fn murl() -> String {
        crate::kernels::MANIFEST_URL.to_string()
    }

    fn answers() -> Answers {
        Answers {
            domain: "example.com".into(),
            admin_password: "test123".into(),
            ..Answers::defaults("node-a", "203.0.113.10")
        }
    }

    #[tokio::test]
    async fn install_writes_state_downloads_kernels_and_reconciles() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(opts(&d), answers(), murl(), paths.clone(), host.clone(), fetcher_with_manifest()).await.unwrap();
        let at = |rel: &str| d.path().join(rel).display().to_string();
        assert!(host.text(&at("manifest.json")).is_some(), "manifest 要缓存下来给对账用");
        assert_eq!(host.mode(&at("bin/xray")), Some(0o755));
        assert!(host.text(&at("config.yaml")).unwrap().contains("listen: :10000,20000-30000"));
        assert!(host.text(&at("xray-config.json")).is_some());
        assert!(host.text(&at("singbox-relay.json")).is_some());
        assert!(host.text(&at("Caddyfile")).unwrap().contains("example.com"), "Caddyfile 按 C3 放 <base>");
        assert!(host.text("/etc/systemd/system/b-ui.service").is_some());
        assert_eq!(
            host.read_link(std::path::Path::new("/usr/local/bin/b-ui")).unwrap(),
            Some(paths.bin_dir.join("bui")),
            "sudo b-ui 的入口要建好"
        );
        // state.json 由 Store 写真实文件系统（tempdir 内）
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap()).unwrap();
        assert_eq!(state.node.reality.public_key, "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c");
        assert_eq!(state.node.domain, "example.com");
    }

    #[tokio::test]
    async fn install_writes_an_auth_snapshot_the_hook_can_read() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(opts(&d), answers(), murl(), paths.clone(), host.clone(), fetcher_with_manifest()).await.unwrap();
        let snap_path = crate::paths::auth_snapshot_file(&paths).display().to_string();
        let snap: serde_json::Value = serde_json::from_str(&host.text(&snap_path).unwrap()).unwrap();
        // 形状逐字照总纲 C5：顶层 schema + users；全新装机没有用户 → users 是空对象（不是缺文件）
        assert_eq!(snap, serde_json::json!({ "schema": 1, "users": {} }));
        assert_eq!(host.mode(&snap_path), Some(0o600), "快照含明文 HY2 密码，必须 0600");
        // 有用户时：users 的键 = 用户名，四个字段与 P2 的 auth-hook 约定一致（文首契约段）
        let state = crate::testutil::sample_state();
        let u = &state.users[0];
        let snap = auth_snapshot(&state);
        assert_eq!(snap["schema"], 1);
        assert_eq!(
            snap["users"][u.username.as_str()],
            serde_json::json!({
                "user_id": u.user_id.to_string(),
                "hy2_password": u.credentials.hy2_password,
                "expires_at": u.entitlements.expires_at,
                "blocked": u.disabled
            })
        );
        assert!(snap["users"].get("nobody").is_none());
    }

    #[test]
    fn an_answers_file_overrides_only_what_it_names() {
        // 总纲 C5 的 `--non-interactive --answers <file>`：格式在本任务定死，P5 的 v3-cutover.sh 照它生成
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("answers.json");
        std::fs::write(
            &f,
            r#"{"domain":"example.com","ports":{"hy2":10500,"hy2_hop":[20000,30000],"hy2_resi":40000,
                "hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080}}"#,
        )
        .unwrap();
        let a = load_answers(&f, Answers::defaults("node-a", "203.0.113.10")).unwrap();
        assert_eq!(a.domain, "example.com");
        assert_eq!(a.ports.hy2, 10500);
        assert_eq!(a.node_name, "node-a", "文件里没写的项沿用默认");
        assert_eq!(a.public_ip, "203.0.113.10");
        assert_eq!(a.masquerade, "www.bing.com:443");
        assert!(a.admin_password.is_empty(), "答案文件里不放管理员密码");
        assert!(load_answers(&d.path().join("nope.json"), Answers::defaults("node-a", "")).is_err());
    }

    #[tokio::test]
    async fn install_without_a_reachable_manifest_says_how_to_fix_it() {
        // 裁决「M5 前不创建任何 Release/tag」→ M1 时内置 MANIFEST_URL 必然 404、bin/ 里没有 xray。
        // 这一支必须给出点名 BUI_MANIFEST_URL 的可操作错误，而不是静默写出没有密钥的 state。
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = Arc::new(FakeHost::new()); // 没有 bin/xray，PATH 上也没有 xray
        let empty: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let err = run_with(opts(&d), answers(), murl(), paths.clone(), host.clone(), empty)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("xray") && err.contains("BUI_MANIFEST_URL"), "{err}");
        assert!(!crate::paths::state_file(&paths).exists(), "失败就不该留下半成品 state.json");
    }

    #[tokio::test]
    async fn a_second_install_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(opts(&d), answers(), murl(), paths.clone(), host.clone(), fetcher_with_manifest()).await.unwrap();
        host.clear_ops();
        run_with(opts(&d), answers(), murl(), paths.clone(), host.clone(), fetcher_with_manifest()).await.unwrap();
        let verify_prefix = format!("write:{}", crate::paths::verify_dir(&paths).display());
        let writes: Vec<String> = host
            .ops()
            .into_iter()
            .filter(|o| o.starts_with("write:") && !o.starts_with(&verify_prefix))
            .collect();
        assert_eq!(writes, Vec::<String>::new(), "幂等：第二次 install 零写入");
    }

    #[tokio::test]
    async fn install_with_import_v3_keeps_the_v4_relay_and_leaves_no_drift() {
        // 回归两个真机事故：① V3_UNITS 含 b-ui-relay 会把刚装好的 v4 relay 卸掉；
        // ② v3 的状态文件留在 /opt/b-ui 下会被漂移扫描永久报告
        let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../bui-schema/tests/fixtures/v3/src"));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        // 假机器的 `<base>` 从 **fixture 的真实目录清单**播种，而不是从 `V3_FILES` /
        // `V3_STATE_FILES` 这两个常量播种：常量漏了什么，那种播种法就恰好也漏同一个，测试变成
        // 自我印证（第三轮审查 C2 就是这么漏掉 `sing-box` / `port-hopping.json` / `server_ip.txt`
        // 的）。再叠加真机上确实存在、但 fixture 不带的那几类残留。
        let fixture_names: Vec<String> = std::fs::read_dir(src)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(fixture_names.contains(&"server_ip.txt".to_string()), "fixture 应带 server_ip.txt");
        host.with(|i| {
            for u in crate::commands::import_v3::V3_UNITS {
                i.files.insert(format!("/etc/systemd/system/{u}").into(), (b"x".to_vec(), 0o644));
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            // ① fixture 里的每个顶层条目（certs/ 是目录，交给 is_dir 分支，不会被当陌生文件）
            for name in &fixture_names {
                let p = src.join(name);
                if p.is_dir() {
                    i.files.insert(d.path().join(name).join("fullchain.pem"), (b"CERT".to_vec(), 0o644));
                } else {
                    i.files.insert(d.path().join(name), (std::fs::read(&p).unwrap(), 0o600));
                }
            }
            // ② v3 的 shell / Node / 二进制 + 迁移块留下的备份与临时文件（fixture 不带这些）
            for f in [
                "core.sh", "update.sh", "b-ui-cli.sh", "residential-helper.sh", "resi-health.sh",
                "hy2-watchdog.sh", "cert-sync.sh", "cert-check.sh", "hy2-portjump-cleanup.sh",
                "install-key.txt", "b-ui-client.sh", "version.json",
            ] {
                i.files.insert(d.path().join(f), (b"#!/bin/bash".to_vec(), 0o755));
            }
            i.files.insert(d.path().join("sing-box"), (b"ELF".to_vec(), 0o755));   // v3 的 relay 二进制在顶层
            i.files.insert(d.path().join("port-hopping.json"), (br#"{"enabled":true}"#.to_vec(), 0o644));
            i.files.insert(d.path().join("masquerade.json"), (br#"{"masqueradeDomain":"www.bing.com"}"#.to_vec(), 0o644));
            i.files.insert(d.path().join(".resi-health-state.json"), (b"{}".to_vec(), 0o600));
            i.files.insert(d.path().join(".relay.lock"), (b"".to_vec(), 0o600));
            i.files.insert(d.path().join("config.yaml.bak.v357.1757000000"), (b"old".to_vec(), 0o600));
            i.files.insert(d.path().join("xray-config.json.bak.v359.1757000001"), (b"{}".to_vec(), 0o600));
            i.files.insert(d.path().join("config-residential.yaml.bak.v360.1757000002"), (b"old".to_vec(), 0o600));
            i.files.insert(d.path().join("config.yaml.tmp"), (b"half".to_vec(), 0o600));
            i.files.insert(d.path().join("admin/server.js"), (b"node".to_vec(), 0o644));
            i.files.insert(d.path().join("admin/node_modules/x/index.js"), (b"x".to_vec(), 0o644));
            i.files.insert("/tmp/hy2-watchdog-10000".into(), (b"2".to_vec(), 0o644));
        });
        let mut o = opts(&d);
        o.import_v3 = Some(src.to_path_buf());
        run_with(o, answers(), murl(), paths.clone(), host.clone(), fetcher_with_manifest()).await.unwrap();
        // v4 的 relay 单元必须还在、还在跑
        assert!(host.text("/etc/systemd/system/b-ui-relay.service").is_some(), "v4 relay 单元被删了");
        assert!(host.unit_is_active("b-ui-relay").unwrap(), "v4 relay 被停了");
        assert!(!host.ops().iter().any(|o| o == "systemd:stop:b-ui-relay"));
        // v3 的痕迹：shell / 二进制删掉、状态文件归档、备份归档、临时文件删掉、
        // Node 目录整棵删掉、/tmp 计数文件删掉
        for f in crate::commands::import_v3::V3_FILES {
            assert!(host.text(d.path().join(f).to_str().unwrap()).is_none(), "{f} 应删除");
        }
        for f in crate::commands::import_v3::V3_STATE_FILES {
            assert!(host.text(d.path().join(f).to_str().unwrap()).is_none(), "{f} 应移走");
            assert!(
                host.text(crate::paths::v3_backup_dir(&paths).join(f).to_str().unwrap()).is_some(),
                "{f} 应进 v3-backup"
            );
        }
        for f in [
            "config.yaml.bak.v357.1757000000",
            "xray-config.json.bak.v359.1757000001",
            "config-residential.yaml.bak.v360.1757000002",
        ] {
            assert!(
                host.text(crate::paths::v3_backup_dir(&paths).join(f).to_str().unwrap()).is_some(),
                "{f} 应进 v3-backup"
            );
        }
        assert!(host.text(d.path().join("config.yaml.tmp").to_str().unwrap()).is_none());
        // v4 自己的 relay 二进制在 bin/ 下，不能被「删 <base>/sing-box」连带删掉
        assert!(host.text(paths.bin_dir.join("sing-box").to_str().unwrap()).is_some(), "v4 的 bin/sing-box 被删了");
        assert!(host.text(d.path().join("admin/node_modules/x/index.js").to_str().unwrap()).is_none());
        assert!(host.text("/tmp/hy2-watchdog-10000").is_none(), "spec §3.4：/tmp 计数文件要删");
        // 导入 + 卸载 + 对账之后，体检必须零漂移（M1 验收项）
        let store = crate::state::store::Store::open(crate::paths::state_file(&paths)).await.unwrap();
        let state = store.read().await;
        let reg = crate::serve::modules(None);
        let ctx = crate::reconcile::RenderCtx {
            paths: paths.clone(),
            facts: crate::reconcile::Facts::probe(host.as_ref()).unwrap(),
        };
        let arts: Vec<crate::reconcile::Artifact> =
            reg.modules.iter().flat_map(|m| m.render(&state, &ctx)).collect();
        assert_eq!(
            crate::reconcile::drift::scan(host.as_ref(), &arts, &paths),
            vec![],
            "import-v3 之后不能留下任何漂移"
        );
        assert_eq!(state.users.len(), 4, "四个 v3 用户都要导入");
    }
}
```

- [ ] **Step 2: 写失败测试（import-v3）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;

    const CRONTAB: &str = "\
# m h dom mon dow command
0 */6 * * * /opt/b-ui/update.sh auto >/dev/null 2>&1
0 */12 * * * /opt/b-ui/update.sh kernel >/dev/null 2>&1
30 3 * * * /usr/local/bin/backup-my-blog.sh
";

    fn scratch(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    #[test]
    fn filter_cron_only_drops_b_ui_lines() {
        let out = filter_cron(CRONTAB);
        assert!(!out.contains("/opt/b-ui/update.sh"));
        assert!(out.contains("backup-my-blog.sh"), "别人的 cron 行必须留着");
        assert!(out.contains("# m h dom mon dow command"));
    }

    #[test]
    fn v3_units_never_touch_a_v4_managed_unit() {
        for u in V3_UNITS {
            let bare = u.trim_end_matches(".service").trim_end_matches(".timer");
            assert!(
                !crate::reconcile::MANAGED_UNITS.contains(&bare),
                "{u} 是 v4 受管单元，不能在卸载列表里"
            );
        }
        assert!(!V3_UNITS.contains(&"b-ui-relay.service"));
        // 两份 v3 单元表必须是包含关系：上一轮事故就是「units 与 drift 各有一份、内容还不一样」，
        // 结果 `hysteria-server@.service` / `xray@.service` 只有 --force 才清理。
        for u in V3_UNITS {
            assert!(
                crate::reconcile::LEGACY_UNITS.contains(&u),
                "{u} 不在 LEGACY_UNITS 里：uninstall 停了它、漂移扫描却不认它"
            );
        }
    }

    #[test]
    fn orphan_port_hopping_nat_rules_are_flushed() {
        // spec §2.3 + §3.1：v3 早期版本留下的 iptables/nft REDIRECT 规则没人清（v4 把
        // hy2-portjump-cleanup.sh 删了），这里在卸载的最后一步清掉孤儿链。
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.which.insert("nft".into());
            i.scripted.push((
                "iptables -t nat -S".into(),
                CmdOut::success(concat!(
                    "-N HYSTERIA-PR-abc123\n",
                    "-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-abc123\n",
                    "-A HYSTERIA-PR-abc123 -p udp -j REDIRECT --to-ports 10000\n",
                )),
            ));
            i.scripted.push(("nft list tables".into(), CmdOut::success("table inet hysteria_abc123\n")));
        });
        let done = flush_v3_portjump_rules(&h);
        let ops = h.ops();
        assert!(
            ops.iter().any(|o| o.contains("iptables") && o.contains("-D PREROUTING")),
            "先删跳转规则：{ops:?}"
        );
        assert!(ops.iter().any(|o| o.contains("-F HYSTERIA-PR-abc123")));
        assert!(ops.iter().any(|o| o.contains("-X HYSTERIA-PR-abc123")));
        assert!(ops.iter().any(|o| o.contains("nft delete table inet hysteria_abc123")));
        assert!(done.iter().any(|l| l.contains("HYSTERIA-PR-abc123")));
    }

    #[test]
    fn flushing_nat_rules_on_a_clean_machine_reports_nothing() {
        let h = FakeHost::new(); // iptables / nft 都不存在
        assert_eq!(flush_v3_portjump_rules(&h), Vec::<String>::new());
    }

    #[test]
    fn uninstall_stops_v3_units_archives_state_deletes_shell_and_keeps_certs() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            for u in V3_UNITS {
                i.files.insert(format!("/etc/systemd/system/{u}").into(), (b"x".to_vec(), 0o644));
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            for f in V3_FILES {
                i.files.insert(d.path().join(f), (b"#!/bin/bash".to_vec(), 0o755));
            }
            for f in V3_STATE_FILES {
                i.files.insert(d.path().join(f), (b"secret".to_vec(), 0o600));
            }
            // v3 迁移块与 CLI 留下的真实残留（第三轮审查 C2 的核实清单）
            i.files.insert(d.path().join("config.yaml.bak.v357.1757000000"), (b"listen: :10000".to_vec(), 0o600));
            i.files.insert(d.path().join("config.yaml.bak.obfs.20260901-120000"), (b"obfs".to_vec(), 0o600));
            i.files.insert(d.path().join("xray-config.json.bak.v359.1757000001"), (b"{}".to_vec(), 0o600));
            i.files.insert(d.path().join("config-residential.yaml.bak.v360.1757000002"), (b"resi".to_vec(), 0o600));
            i.files.insert(d.path().join("xray-config.json.tmp"), (b"half".to_vec(), 0o600));
            // v4 自己在管的四个配置：同名文件绝不能被这一步碰到
            i.files.insert(d.path().join("config.yaml"), (b"listen: :10000".to_vec(), 0o600));
            i.files.insert(d.path().join("certs/fullchain.pem"), (b"CERT".to_vec(), 0o644));
            i.files.insert(d.path().join("packages/versions.json"), (b"{}".to_vec(), 0o644));
            i.files.insert(d.path().join("admin/server.js"), (b"node".to_vec(), 0o644));
            i.files.insert(d.path().join("admin/node_modules/y/index.js"), (b"y".to_vec(), 0o644));
            i.files.insert("/tmp/hy2-watchdog-10000".into(), (b"2".to_vec(), 0o644));
            i.files.insert("/tmp/hy2-watchdog-40000".into(), (b"0".to_vec(), 0o644));
            i.files.insert("/tmp/unrelated.txt".into(), (b"keep".to_vec(), 0o644));
            i.scripted.push(("crontab -l".into(), CmdOut::success(CRONTAB)));
        });
        let done = uninstall_v3(&h, &paths);
        for u in V3_UNITS {
            assert!(h.ops().contains(&format!("systemd:disable:{u}")), "{u} 要停用");
            assert!(h.text(&format!("/etc/systemd/system/{u}")).is_none(), "{u} 的单元文件要删");
        }
        for f in V3_FILES {
            assert!(h.text(d.path().join(f).to_str().unwrap()).is_none(), "{f} 要删");
        }
        for f in V3_STATE_FILES {
            assert!(h.text(d.path().join(f).to_str().unwrap()).is_none(), "{f} 要移走");
            assert_eq!(
                h.text(crate::paths::v3_backup_dir(&paths).join(f).to_str().unwrap()).as_deref(),
                Some("secret"),
                "{f} 要进 v3-backup"
            );
            assert_eq!(h.mode(crate::paths::v3_backup_dir(&paths).join(f).to_str().unwrap()), Some(0o600));
        }
        assert!(h.text(d.path().join("admin/node_modules/y/index.js").to_str().unwrap()).is_none(), "Node 面板整棵删");
        assert!(h.ops().contains(&format!("rmdir:{}", d.path().join("admin").display())));
        assert_eq!(h.text(d.path().join("certs/fullchain.pem").to_str().unwrap()).as_deref(), Some("CERT"), "证书必须保留");
        assert!(h.text(d.path().join("packages/versions.json").to_str().unwrap()).is_some(), "内核缓存保留");
        assert!(h.text("/tmp/hy2-watchdog-10000").is_none() && h.text("/tmp/hy2-watchdog-40000").is_none());
        assert_eq!(h.text("/tmp/unrelated.txt").as_deref(), Some("keep"), "只删自己的 /tmp 文件");
        // v3 的迁移备份：归档（含明文密码）；临时文件：删掉；v4 在管的同名配置：不许碰
        for bak in [
            "config.yaml.bak.v357.1757000000",
            "config.yaml.bak.obfs.20260901-120000",
            "xray-config.json.bak.v359.1757000001",
            "config-residential.yaml.bak.v360.1757000002",
        ] {
            assert!(h.text(d.path().join(bak).to_str().unwrap()).is_none(), "{bak} 应移走");
            assert!(
                h.text(crate::paths::v3_backup_dir(&paths).join(bak).to_str().unwrap()).is_some(),
                "{bak} 应进 v3-backup（漂移扫描不跳过 *.bak.*）"
            );
        }
        assert!(h.text(d.path().join("xray-config.json.tmp").to_str().unwrap()).is_none(), ".tmp 应删掉");
        assert_eq!(
            h.text(d.path().join("config.yaml").to_str().unwrap()).as_deref(),
            Some("listen: :10000"),
            "v4 在管的配置本身不能被 sweep 碰到"
        );
        // cron 必须被**重写**：`crontab -l` 自己就会产生一条 `run:crontab -l`，所以断言
        // `starts_with("run:crontab")` 是恒真的空断言（第三轮审查 C8）；要断言真正的写入命令。
        assert!(
            h.ops().contains(&format!("run:crontab {}", d.path().join(".crontab.new").display())),
            "cron 行要重写：{:?}",
            h.ops()
        );
        assert!(done.iter().any(|l| l.contains("已清理 b-ui 的 cron 行")));
        assert!(!done.is_empty());
    }

    #[test]
    fn caddy_acme_data_is_copied_before_the_distro_unit_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let certs = format!("{V3_CADDY_DATA}/certificates/acme-v02.api.letsencrypt.org-directory/example.com");
        let acct = format!("{V3_CADDY_DATA}/acme/acme-v02.api.letsencrypt.org-directory/users/default");
        h.with(|i| {
            i.files.insert(format!("{certs}/example.com.crt").into(), (b"CERT".to_vec(), 0o644));
            i.files.insert(format!("{certs}/example.com.key").into(), (b"KEY".to_vec(), 0o600));
            i.files.insert(format!("{acct}/default.key").into(), (b"ACCT".to_vec(), 0o600));
            i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root")));
        });
        let done = uninstall_v3(&h, &paths);
        let dest = crate::paths::caddy_data(&paths);
        let key = dest.join("certificates/acme-v02.api.letsencrypt.org-directory/example.com/example.com.key");
        assert_eq!(h.text(key.to_str().unwrap()).as_deref(), Some("KEY"), "证书私钥要搬过来");
        assert_eq!(h.mode(key.to_str().unwrap()), Some(0o600));
        assert_eq!(
            h.text(dest.join("acme/acme-v02.api.letsencrypt.org-directory/users/default/default.key").to_str().unwrap())
                .as_deref(),
            Some("ACCT"),
            "ACME 账号私钥必须一起搬，否则 Caddy 重新注册账号并重签，撞 Let's Encrypt 速率限制"
        );
        let ops = h.ops();
        let stop = ops.iter().position(|o| o == "systemd:stop:caddy").expect("要停发行版 caddy");
        let last_copy = ops
            .iter()
            .rposition(|o| o.starts_with(&format!("write:{}", dest.display())))
            .expect("要复制文件");
        assert!(last_copy < stop, "必须先复制完再停 caddy，否则会出现无证书窗口：{ops:?}");
        assert!(done.iter().any(|l| l.contains("Caddy 数据目录")));
    }

    #[test]
    fn caddy_migration_is_idempotent_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let dest = crate::paths::caddy_data(&paths).join("certificates/x/example.com.crt");
        h.with(|i| {
            i.files.insert(format!("{V3_CADDY_DATA}/certificates/x/example.com.crt").into(), (b"OLD".to_vec(), 0o600));
            i.files.insert(dest.clone(), (b"NEW".to_vec(), 0o600));
        });
        let done = migrate_caddy_data(&h, &paths);
        assert_eq!(h.text(dest.to_str().unwrap()).as_deref(), Some("NEW"), "目标已有的文件不许被旧数据盖掉");
        assert!(done.iter().all(|l| !l.contains("已复制")), "{done:?}");
    }

    #[test]
    fn uninstall_on_a_machine_without_v3_is_a_no_op() {
        let d = tempfile::tempdir().unwrap();
        let h = FakeHost::new();
        h.with(|i| i.scripted.push(("crontab -l".into(), CmdOut::failure(1, "no crontab for root"))));
        let done = uninstall_v3(&h, &scratch(&d));
        assert_eq!(done, Vec::<String>::new());
        assert!(!h.ops().iter().any(|o| o.starts_with("remove:") || o.starts_with("rmdir:")));
    }

    #[tokio::test]
    async fn import_writes_state_from_a_v3_directory() {
        // 复用 P0 的合成 v3 fixture（crates/bui-schema/tests/fixtures/v3/src）
        let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../bui-schema/tests/fixtures/v3/src"));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| i.hostname = "node-b".into());
        run(src.to_path_buf(), None, paths.clone(), host).await.unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap()).unwrap();
        assert_eq!(state.users.len(), 4);
        assert_eq!(state.node.domain, "example.com");
        assert_eq!(state.node.name, "example.com", "导入值优先，hostname 只在导入值为空时兜底");
        assert_eq!(state.node.ports.hy2_hop, Some((20000, 30000)));
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(crate::paths::state_file(&paths)).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn import_probes_the_public_ip_only_when_it_is_missing() {
        let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../bui-schema/tests/fixtures/v3/src"));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        // fixture 里有 server_ip.txt → 导入值非空 → 不该调 curl 覆盖它
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| i.scripted.push(("curl".into(), CmdOut::failure(7, "couldn't connect"))));
        run(src.to_path_buf(), None, paths.clone(), host.clone()).await.unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap()).unwrap();
        assert_eq!(state.node.public_ip, "203.0.113.10", "导入到的公网 IP 不能被失败的探测清空");
        assert!(!host.ops().iter().any(|o| o.starts_with("run:curl")), "导入值非空就别探测");
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p bui commands::install commands::import_v3`
Expected: 编译失败，`cannot find function parse_x25519`。

- [ ] **Step 4: 实现 `install.rs`**

```rust
pub fn parse_x25519(stdout: &str) -> Option<(String, String)> {
    let grab = |prefixes: [&str; 2]| -> Option<String> {
        stdout.lines().find_map(|l| {
            let l = l.trim();
            prefixes.iter().find_map(|p| l.strip_prefix(p)).map(|v| v.trim().to_string())
        })
    };
    let private = grab(["PrivateKey:", "Private key:"])?;
    let public = grab(["Password (PublicKey):", "Public key:"])?;
    (!private.is_empty() && !public.is_empty()).then_some((private, public))
}

pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn random_short_id() -> String {
    random_hex(8)
}

pub fn build_state(answers: &Answers, reality: Reality, versions: Versions) -> anyhow::Result<State> {
    let mut reality = reality;
    // dest 与 serverNames 必须同源：用户在交互里改了伪装域名而 server_names 还写死 www.bing.com 时，
    // xray 的 REALITY 握手直接失败。`Reality::sni()` 就是「dest 去掉端口」的语义（P0 已有）。
    reality.dest = answers.masquerade.clone();
    reality.server_names = vec![reality.sni().to_string()];
    Ok(State {
        schema_version: bui_schema::model::SCHEMA_VERSION,
        node: NodeParams {
            id: uuid::Uuid::new_v4(),
            name: answers.node_name.clone(),
            domain: answers.domain.clone(),
            public_ip: answers.public_ip.clone(),
            ports: answers.ports.clone(),
            reality,
            obfs: Obfs::default(),
        },
        admin: Admin {
            password_hash: crate::api::auth::hash_password(&answers.admin_password)?,
            jwt_secret: random_hex(32),
        },
        users: Vec::new(),
        residential: Residential::default(),
        system: SystemSettings::default(),
        versions,
        catalog: Vec::new(),
    })
}
```
`run_with` 按上面的 10 步落地，**所有阻塞调用（manifest 下载、内核安装、`xray x25519`、`curl` 探测、`uninstall_v3`、写快照）都在 `tokio::task::spawn_blocking` 里**（`reqwest::blocking` 在 async 上下文会 panic；`Host::run` 也是同步的）：
```rust
pub async fn run_with(
    opts: InstallOpts,
    answers: Answers,
    manifest_url: String,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
) -> anyhow::Result<()> {
    let state_path = crate::paths::state_file(&paths);
    let fresh = !state_path.exists();
    // 3 + 4：拉 manifest → 缓存 → 装四个内核（阻塞线程）
    let manifest = {
        let (h, f, p, u) = (host.clone(), fetcher.clone(), paths.clone(), manifest_url.clone());
        tokio::task::spawn_blocking(move || fetch_and_install_kernels(h.as_ref(), f.as_ref(), &p, &u)).await?
    };
    if fresh {
        // 5 + 6：生成或导入 state
        let state = match &opts.import_v3 {
            Some(dir) => {
                let report = bui_schema::v3::import(dir)?;
                for w in &report.warnings {
                    println!("导入提示：{w}");
                    tracing::warn!("{w}");
                }
                report.state
            }
            None => {
                let (h, p) = (host.clone(), paths.clone());
                let keys = tokio::task::spawn_blocking(move || generate_reality(h.as_ref(), &p)).await??;
                let versions = {
                    let (h, p) = (host.clone(), paths.clone());
                    tokio::task::spawn_blocking(move || versions_from_disk(h.as_ref(), &p)).await?
                };
                build_state(&answers, keys, versions)?
            }
        };
        Store::create(&state_path, state).await?;
        // 7：先卸 v3（停 b-ui-admin 腾出 :8080、停 v3 timer），再对账
        if opts.import_v3.is_some() {
            let (h, p) = (host.clone(), paths.clone());
            let done = tokio::task::spawn_blocking(move || {
                crate::commands::import_v3::uninstall_v3(h.as_ref(), &p)
            })
            .await?;
            for line in done {
                println!("{line}");
            }
        }
    } else {
        println!("已安装（{} 已存在），执行对账", state_path.display());
    }
    let store = Store::open(&state_path).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let ctx = DaemonCtx { store, runtime, bus: EventBus::new(), host: host.clone(), paths: paths.clone() };
    // 8：初版 auth-snapshot.json（钩子的输入；内容不变则不写，保证第二次 install 零写入）
    {
        let bytes = serde_json::to_vec_pretty(&auth_snapshot(&ctx.store.read().await))?;
        let (h, p) = (host.clone(), paths.clone());
        tokio::task::spawn_blocking(move || write_auth_snapshot(h.as_ref(), &p, &bytes)).await??;
    }
    // 9：一次完整对账。守护进程已经在跑（活机器上重跑 install）就交给它，别在本进程里并发再跑一轮。
    // socket 路径来自 `opts.socket`（调用方传入），测试因此不会碰真实的 /run/b-ui.sock
    let client = crate::ipc::Client::new(&opts.socket);
    let report = if client.available().await {
        let (status, body) = client
            .request("POST", "/api/reconcile", Some(serde_json::json!({"force": false, "dry_run": false})))
            .await?;
        println!("守护进程已在运行，对账已交给它（HTTP {status}）：{body}");
        serde_json::from_value(body).unwrap_or_default()
    } else {
        let reg = crate::serve::modules(manifest);
        let r = crate::serve::reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false).await?;
        crate::serve::finish_self_restart(&ctx, &r, false).await;
        r
    };
    for line in report.notes.iter().chain(report.verify_failures.iter()).chain(report.errors.iter()) {
        println!("{line}");
    }
    // 10
    let state = ctx.store.read().await;
    println!("面板: https://{}/    用户: 见 `b-ui` 菜单", state.node.domain);
    println!("后续: `b-ui` 进菜单 / `bui status` 看体检 / `bui reconcile` 手动对账");
    if !report.errors.is_empty() || !report.verify_failures.is_empty() {
        anyhow::bail!("对账有失败项，见上面的输出");
    }
    Ok(())
}
```
`auth_snapshot` 与写盘辅助：
```rust
pub fn auth_snapshot(state: &State) -> serde_json::Value {
    // 形状逐字照总纲 C5：顶层 {"schema":1,"users":{…}}。写成扁平对象 P2 的钩子读不到用户，
    // 导入的用户在 `bui install --import-v3` 之后建连一律 fail-closed。
    let mut users = serde_json::Map::new();
    for u in &state.users {
        users.insert(
            u.username.clone(),
            serde_json::json!({
                "user_id": u.user_id.to_string(),
                "hy2_password": u.credentials.hy2_password,
                "expires_at": u.entitlements.expires_at,
                "blocked": u.disabled,
            }),
        );
    }
    serde_json::json!({ "schema": 1, "users": serde_json::Value::Object(users) })
}

/// 只在内容变化时写（0600）；否则第二次 `bui install` 就不是零写入了。
fn write_auth_snapshot(host: &dyn Host, paths: &Paths, bytes: &[u8]) -> anyhow::Result<()> {
    let path = crate::paths::auth_snapshot_file(paths);
    if host.read_file(&path)?.as_deref() == Some(bytes) {
        return Ok(());
    }
    host.write_file(&path, bytes, 0o600)?;
    Ok(())
}
```
三个同步辅助（都只在 `spawn_blocking` 里调）：
- `fetch_and_install_kernels(host, fetcher, paths, manifest_url) -> Option<Manifest>`：`Manifest::from_url(fetcher, manifest_url)` 失败 → `tracing::warn!`（点名 `BUI_MANIFEST_URL` 可覆盖）+ 返回 `None`（离线装机继续，对账跳过 Binary）；成功 → 序列化后**先读现有文件比对，内容相同就不写**（`a_second_install_changes_nothing` 要求第二次 install 除 `.verify/` 外零写入），不同才 `host.write_file(manifest_file(paths), bytes, 0o644)`；再对 `KERNELS` 逐个 `KernelInstaller { fetcher, host }.install(...)`（版本一致则跳过），最后按第 4 步的口径检查四个内核是否齐全，返回 `Some(manifest)`。
- `xray_program(host, paths) -> Option<String>`：`<bin>/xray` 有文件 → 它；否则 `host.which("xray")` → `"xray"`；都没有 → `None`。（与 Task 5 的校验器查找同一套规则：M1 时 manifest 必然 404，而 v3 机器上 `/usr/local/bin/xray` 是现成的。）
- `generate_reality(host, paths) -> Result<Reality>`：`xray_program` 为 `None` → `Err("未找到 xray 二进制（<bin>/xray 与 PATH 都没有）：manifest 拉取失败时请设 BUI_MANIFEST_URL 指向可用的 manifest，或先把 xray 放进 <bin>/")`；否则 `host.run(prog, &["x25519"])` → `parse_x25519` → `Reality { private_key, public_key, short_ids: vec![random_short_id()], dest: String::new(), server_names: Vec::new() }`——`dest` 与 `server_names` **都留空占位**，由 `build_state` 按 `answers.masquerade` 一起填（见下一条）；解析失败 → `Err`。
- `versions_from_disk(host, paths) -> Versions`：用 `crate::kernels::installed_versions` 填 `hysteria/xray/sing_box/caddy`，`bui` 填 `env!("CARGO_PKG_VERSION")`。

答案文件的加载（第 2 步用）：
```rust
pub fn load_answers(path: &Path, defaults: Answers) -> anyhow::Result<Answers> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("读取 {} 失败：{e}", path.display()))?;
    let f: AnswersFile = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("{} 不是合法的 answers JSON：{e}", path.display()))?;
    Ok(Answers {
        domain: f.domain.unwrap_or(defaults.domain),
        // 密码永远不从文件里来（会落盘、会进 P5 脚本的 git 历史）：只认 --admin-password-stdin
        admin_password: defaults.admin_password,
        node_name: f.node_name.unwrap_or(defaults.node_name),
        public_ip: f.public_ip.unwrap_or(defaults.public_ip),
        ports: f.ports.unwrap_or(defaults.ports),
        masquerade: f.masquerade.unwrap_or(defaults.masquerade),
    })
}
```

`run`（面向真实终端）：先 `Answers::defaults(&host.hostname()?, &probed_ip)`（`public_ip` 由 `host.run("curl", &["-sS", "--max-time", "5", "https://api.ipify.org"])` 探测，失败则留空并提示——对账不依赖它，只影响 relay 的本机 IP 直连例外规则）；`opts.answers` 有值 → `load_answers(file, defaults)?`；`opts.quiet()`（`--non-interactive` 或 `-y`）或 stdin 不是 TTY → 直接用这份 `Answers` 不提问，否则用 `std::io::stdin().read_line` 逐项确认（回车即接受默认值）；`--domain` / `--port` 最后覆盖 `domain` / `ports.hy2`（命令行优先于文件）。密码在 `--admin-password-stdin` 时整行读取并 `trim_end_matches('\n')`，否则用 `random_hex(8)` 生成并**打印一次**（`println!` 到 stdout，不写日志、不进 argv）；随机生成这一支同时记一行审计日志，字段过脱敏——这是 `redact::secret` 在 P1 的调用点（第三轮审查 C7）：

```rust
    let admin_password = if opts.admin_password_stdin {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        buf.trim_end_matches('\n').to_string()
    } else {
        let pw = random_hex(8);
        println!("已生成随机管理员密码：{pw}（只显示这一次，请立刻存好）");
        // Global Constraints：密码不进日志。`secret` 只记长度，不记内容
        tracing::info!(password = %crate::redact::secret(&pw), "已生成随机管理员密码");
        pw
    };
```

最后调 `run_with(opts, answers, crate::kernels::manifest_url(None, None), paths, host, Arc::new(HttpFetcher::new()))`（`opts.socket` 由 `main.rs` 填 `PathBuf::from(crate::paths::SOCKET_PATH)`）。

- [ ] **Step 5: 实现 `import_v3.rs`**

```rust
pub fn filter_cron(text: &str) -> String {
    text.lines()
        .filter(|l| !l.contains("/opt/b-ui/"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// 递归复制目录：只用 `Host` 的原语，所以测试里注入 `FakeHost` 就能全程不碰真实系统。
/// 目标已存在同名文件 → 跳过（幂等，且绝不用旧数据盖掉新的）。
fn copy_tree(host: &dyn Host, src: &Path, dest: &Path, done: &mut Vec<String>) {
    let Ok(entries) = host.list_dir(src) else { return };
    for e in entries {
        let Some(name) = e.file_name() else { continue };
        let target = dest.join(name);
        if host.is_dir(&e).unwrap_or(false) {
            copy_tree(host, &e, &target, done);
        } else if host.read_file(&target).ok().flatten().is_some() {
            continue;
        } else if let Ok(Some(bytes)) = host.read_file(&e) {
            // 里面有 ACME 账号私钥与证书私钥，一律 0600（v4 的 caddy 以 root 运行）
            if host.write_file(&target, &bytes, 0o600).is_ok() {
                done.push(format!("已复制 {} → {}", e.display(), target.display()));
            }
        }
    }
}

pub fn migrate_caddy_data(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let src = Path::new(V3_CADDY_DATA);
    if !host.is_dir(src).unwrap_or(false) {
        return Vec::new();   // 没装过发行版 caddy（全新机器）：什么都不做
    }
    let dest = crate::paths::caddy_data(paths);
    let mut done = Vec::new();
    copy_tree(host, src, &dest, &mut done);
    // 复制完才停发行版 caddy：它还活着就会继续往旧目录写，停早了 443 上会出现无证书窗口。
    // 对账（install 第 9 步）随后写 v4 的 caddy.service 并把它拉起来，届时用的是新的 XDG 目录。
    let _ = host.systemd("stop", "caddy");
    done.push(format!("已迁移 v3 Caddy 数据目录（ACME 账号与证书）到 {} 并停用发行版 caddy", dest.display()));
    done
}

pub fn uninstall_v3(host: &dyn Host, paths: &Paths) -> Vec<String> {
    // 第一步（2026-09-12 裁决）：先把发行版 Caddy 的 ACME 账号与证书搬进 v4 的数据目录，
    // 再停它；顺序颠倒就会重新签发。必须排在所有删除动作之前。
    let mut done = migrate_caddy_data(host, paths);
    let mut touched_units = false;
    for u in V3_UNITS {
        let unit_file = PathBuf::from("/etc/systemd/system").join(u);
        if host.read_file(&unit_file).ok().flatten().is_none() && !host.unit_exists(u).unwrap_or(false) {
            continue;
        }
        let _ = host.systemd("disable", u);
        let _ = host.systemd("stop", u);
        let _ = host.remove_file(&unit_file);
        touched_units = true;
        done.push(format!("已移除 v3 单元 {u}"));
    }
    for f in V3_FILES {
        let p = paths.base_dir.join(f);
        if host.read_file(&p).ok().flatten().is_some() {
            let _ = host.remove_file(&p);
            done.push(format!("已删除 {}", p.display()));
        }
    }
    // v3 的状态文件（含秘密）归档到 v3-backup/，0600；目录本身 0700 由 write_file 的父目录创建 + chmod 保证
    for f in V3_STATE_FILES {
        let src = paths.base_dir.join(f);
        if let Ok(Some(bytes)) = host.read_file(&src) {
            let dest = crate::paths::v3_backup_dir(paths).join(f);
            if host.write_file(&dest, &bytes, 0o600).is_ok() {
                let _ = host.remove_file(&src);
                done.push(format!("已归档 {} → {}", src.display(), dest.display()));
            }
        }
    }
    // v3 迁移块与 CLI 留下的备份 / 临时文件（`*.bak.v357.<ts>` 一类）。漂移扫描只跳过
    // `.tmp` / `.new`，不跳过 `*.bak.*`，不清就是一串永久 stray_file
    done.extend(sweep_v3_leftovers(host, paths));
    // v3 的 Node 面板整棵删（server.js + node_modules + 目录本身）
    let admin = paths.base_dir.join("admin");
    if host.is_dir(&admin).unwrap_or(false) {
        let _ = host.remove_dir_all(&admin);
        done.push(format!("已删除 v3 Node 面板目录 {}", admin.display()));
    }
    // spec §3.4：/tmp/hy2-watchdog-* 计数文件（v3 面板还在读，web-C12）
    if let Ok(entries) = host.list_dir(Path::new("/tmp")) {
        for e in entries {
            let name = e.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with("hy2-watchdog-") {
                let _ = host.remove_file(&e);
                done.push(format!("已删除 {}", e.display()));
            }
        }
    }
    if let Ok(out) = host.run("crontab", &["-l"]) {
        if out.ok() && out.stdout.contains("/opt/b-ui/") {
            let kept = filter_cron(&out.stdout);
            let tmp = paths.base_dir.join(".crontab.new");
            if host.write_file(&tmp, kept.as_bytes(), 0o600).is_ok() {
                let _ = host.run("crontab", &[&tmp.display().to_string()]);
                let _ = host.remove_file(&tmp);
                done.push("已清理 b-ui 的 cron 行（其它行保留）".into());
            }
        }
    }
    if touched_units {
        let _ = host.systemd_daemon_reload();
    }
    // 最后一步：清掉 v3 早期版本留下的端口跳跃 NAT 孤儿链（spec §2.3、§3.1）。
    // 必须最后做：紧接着 install 第 9 步的对账会重写两个 hysteria 单元并重启，
    // hysteria 2.12 启动时自建它需要的链。
    done.extend(flush_v3_portjump_rules(host));
    done
}

pub fn sweep_v3_leftovers(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(entries) = host.list_dir(&paths.base_dir) else { return done };
    for e in entries {
        let Some(name) = e.file_name().and_then(|s| s.to_str()) else { continue };
        // 只认「某个受管基名 + 后缀」，且不能等于基名本身（那是 v4 正在管的配置）
        let Some(rest) = V3_LEFTOVER_PREFIXES
            .iter()
            .find_map(|pre| name.strip_prefix(pre).filter(|r| !r.is_empty()))
        else {
            continue;
        };
        if host.is_dir(&e).unwrap_or(false) {
            continue;   // 只处理文件
        }
        if rest.starts_with(".bak.") {
            // 里面有 HY2 明文密码与 UUID：归档而不是删，0600
            if let Ok(Some(bytes)) = host.read_file(&e) {
                let dest = crate::paths::v3_backup_dir(paths).join(name);
                if host.write_file(&dest, &bytes, 0o600).is_ok() {
                    let _ = host.remove_file(&e);
                    done.push(format!("已归档 v3 备份 {} → {}", e.display(), dest.display()));
                }
            }
        } else if rest.ends_with(".tmp") {
            let _ = host.remove_file(&e);
            done.push(format!("已删除 v3 临时文件 {}", e.display()));
        }
    }
    done
}

/// 移植 `server/core.sh:159-186`（v3 的 `hy2-portjump-cleanup.sh` 正文），只清孤儿、不碰别的规则。
/// 什么都没删就返回空 `Vec`（`uninstall_on_a_machine_without_v3_is_a_no_op` 依赖这一点）。
pub fn flush_v3_portjump_rules(host: &dyn Host) -> Vec<String> {
    let mut done = Vec::new();
    for ipt in ["iptables", "ip6tables"] {
        if !host.which(ipt) {
            continue;
        }
        let Ok(out) = host.run(ipt, &["-t", "nat", "-S"]) else { continue };
        if !out.ok() {
            continue;
        }
        let chains: std::collections::BTreeSet<String> = out
            .stdout
            .lines()
            .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("HYSTERIA-PR-")))
            .map(str::to_string)
            .collect();
        for ch in chains {
            // 先删所有跳转到该链的规则（`-A … -j <ch>` → `-D … -j <ch>`），再清空并删链
            for line in out.stdout.lines().filter(|l| l.starts_with("-A ") && l.ends_with(&format!("-j {ch}"))) {
                let rule = line.replacen("-A ", "-D ", 1);
                let mut args = vec!["-t", "nat"];
                args.extend(rule.split_whitespace());
                let _ = host.run(ipt, &args);
            }
            let _ = host.run(ipt, &["-t", "nat", "-F", &ch]);
            let _ = host.run(ipt, &["-t", "nat", "-X", &ch]);
            done.push(format!("已清理 {ipt} nat 链 {ch}（v3 端口跳跃遗留）"));
        }
    }
    if host.which("nft") {
        if let Ok(out) = host.run("nft", &["list", "tables"]) {
            for (family, table) in out.stdout.lines().filter_map(|l| {
                let mut w = l.split_whitespace();
                let (_, f, t) = (w.next()?, w.next()?, w.next()?);
                t.starts_with("hysteria_").then(|| (f.to_string(), t.to_string()))
            }) {
                let _ = host.run("nft", &["delete", "table", &family, &table]);
                done.push(format!("已删除 nft 表 {family} {table}（v3 端口跳跃遗留）"));
            }
        }
    }
    done
}
```
`run(dir, out, paths, host)`：调 `bui_schema::v3::import(&dir)?`，打印 `warnings`；只在导入值为空时兜底——`node.name` 空则用 `host.hostname()`（v3 的 `import` 把 `name` 填成域名，正常不为空），`node.public_ip` 空则 `host.run("curl", &["-sS", "--max-time", "5", "https://api.ipify.org"])` 探测（失败保持为空并打印提示，**不覆盖已有值**）；`versions` 用 `crate::kernels::installed_versions` 填（探不到就留空）；然后 `Store::create(out.unwrap_or(state_file(&paths)), state)`。**不**调 `uninstall_v3`（那是 `bui install --import-v3` 的第 7 步），也不对账——命令语义就是「只生成 state.json」，方便先人工 diff。

- [ ] **Step 6: 运行测试**

Run: `cargo test -p bui commands:: && cargo clippy -p bui --all-targets -- -D warnings`
Expected: 23 passed（harden_ssh 3 + install 10 + import_v3 10；upgrade/status/menu 的测试在 Task 17）。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/commands/install.rs crates/bui/src/commands/import_v3.rs crates/bui/src/main.rs
git commit -m "feat(bui): bui install（幂等、先卸 v3 再对账）与 bui import-v3"
```

---

### Task 17: `bui upgrade --rollback`、`bui status`、`sudo b-ui` 菜单

**Files:**
- Modify（填 Task 1 建好的桩）: `crates/bui/src/commands/upgrade.rs`, `crates/bui/src/commands/status.rs`, `crates/bui/src/commands/menu.rs`
- Modify: `crates/bui/src/main.rs`（剩余 arm 落地，删掉 `not_yet`）

**Interfaces:**
- Produces:
```rust
// crate::commands::upgrade
#[derive(Debug, PartialEq)]
pub struct UpgradePlan { pub self_from: String, pub self_to: Option<String>, pub kernels: Vec<(String, String, String)> }  // (name, from, to)
/// 比对 manifest（总纲 C4 形状）与现装版本：`m.version` 决定要不要换 bui 自己，
/// `m.kernels`（下划线键，用 `kernels::kernels_key` 换名）决定四个内核；
/// 资产一律按架构从 `m.artifacts` 查，没有 Rust target 三元组这一层。
/// 还负责总纲 C4 的可选字段 `min_upgrade_from`：`version_lt(current_bui, min)` 为真 → 直接 `Err`
/// 并提示先升到该中间版本（消费方规则在 P1，不在 P5）
pub fn plan_upgrade(m: &Manifest, current_bui: &str, installed: &BTreeMap<String, String>, arch: &str) -> anyhow::Result<UpgradePlan>;
/// 点分版本号比较（只比数字段，段数不等时缺位当 0；非数字段当 0）。不引新依赖
pub fn version_lt(a: &str, b: &str) -> bool;
/// 下载 → sha256 → bin/bui.new → 旧版另存 bin/bui.prev → rename 到 bin/bui
pub fn apply_self(host: &dyn Host, fetcher: &dyn Fetcher, asset: &Asset, bin_dir: &Path) -> anyhow::Result<()>;
/// bin/bui.prev → bin/bui，并把最近一份 state 备份恢复成 state.json
pub fn rollback(host: &dyn Host, paths: &Paths) -> anyhow::Result<Vec<String>>;
/// 每日自检的抖动（实现在 Task 15 的 `serve.rs`，这里只再导出，避免 15↔17 循环依赖）
pub use crate::serve::jitter_secs;
/// 总纲 C5：`bui upgrade [--version <x.y.z>] [--manifest-url <url|file>] [--rollback]`
pub async fn run(rollback_flag: bool, version: Option<String>, manifest_url: Option<String>, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
/// 注入 fetcher 的版本（`run` 转调它并传 `Arc::new(HttpFetcher::new())`）；
/// 所有下载都在 `tokio::task::spawn_blocking` 里跑（`reqwest::blocking` 不能在 async 上下文里调）。
/// manifest 地址一律经 `crate::kernels::manifest_url(manifest_url.as_deref(), version.as_deref())`
/// 解析（总纲 C4 的顺序），本文件不自己拼 URL
pub async fn run_with(rollback_flag: bool, version: Option<String>, manifest_url: Option<String>, paths: Paths, host: Arc<dyn Host>, fetcher: Arc<dyn Fetcher>) -> anyhow::Result<()>;
// crate::commands::status
pub fn format_status(h: &HealthResponse) -> String;
pub async fn run(json: bool, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
// crate::commands::menu
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem { pub key: &'static str, pub title: &'static str, pub action: MenuAction }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction { Status, Reconcile, ReconcileForce, Service(&'static str), Logs, Upgrade, HardenSsh, Quit }
pub fn items() -> Vec<MenuItem>;
pub fn parse_choice(input: &str, items: &[MenuItem]) -> Option<MenuAction>;
pub fn render(items: &[MenuItem]) -> String;      // 两列数字菜单
pub async fn run(paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()>;
```
spec §7：`bui upgrade` 下载 **manifest 指定版本**的 `bui` → 原子替换 → `systemctl restart b-ui` → 对账（内核随 manifest 升级）；`--rollback` 恢复上一版二进制 + 最近一份 state 备份。「指定版本」就是 C5 的 `--version <x.y.z>`：它决定取哪份 manifest（`releases/download/v<x.y.z>/manifest.json`，Task 9 的 `manifest_url_for_version`），拉回来之后还要校验 `m.version == 请求的版本`（本地/环境变量里的 manifest 可能是别的版本，不校验就会装错），不一致直接报错。守护进程的每日带抖动自检由 **Task 15 的 `serve::selfcheck_loop`** 承接（它 `use crate::commands::upgrade::jitter_secs`，延迟后每 24h 拉 manifest、刷新共享句柄、记 `runtime.upgrade_available`、请求一次对账）；本任务提供 `plan_upgrade`、`apply_self`、`rollback` 并再导出 `jitter_secs`（实现在 `serve.rs`），GitHub Actions 归 P5。
spec §2.4：守护进程未运行时 CLI 只允许 `install / upgrade / status / reconcile`，**本计划再放行 `harden-ssh`**（连同 `import-v3` 与 `serve` 一起，完整集合与逐项理由见 Task 7；批准时回写 spec）；菜单在 socket 不可用时打印提示并保留「状态 / 对账 / 对账并清理漂移 / 升级 / SSH 硬化 / 退出」六项（对账两项退化为进程内跑，与 `serve::reconcile_cli` 同一条路径），只把「重启数据面」与「查看日志」标成「(需守护进程)」。

- [ ] **Step 1: 写失败测试**

```rust
// crates/bui/src/commands/upgrade.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::sys::{fake::FakeHost, Host};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct F(Mutex<Vec<(String, Vec<u8>)>>);
    impl crate::kernels::Fetcher for F {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0.lock().unwrap().iter().find(|(u, _)| u == url).map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    /// 总纲 C4 形状；只放 amd64 资产，用来顺带测「缺架构资产直接报错」
    fn manifest(bui: &str, sb: &str, sum: &str) -> Manifest {
        let a = |u: &str| Asset { url: u.into(), sha256: sum.into() };
        Manifest {
            version: bui.into(),
            kernels: BTreeMap::from([("sing_box".to_string(), sb.to_string())]),
            artifacts: BTreeMap::from([
                ("bui-linux-amd64".to_string(), a("https://x/bui")),
                ("sing-box-linux-amd64".to_string(), a("https://x/sb")),
            ]),
            min_upgrade_from: None,
        }
    }

    #[test]
    fn bui_asset_is_keyed_by_arch_not_by_target_triple() {
        let m = manifest("4.0.1", "1.13.19", "00");
        assert_eq!(m.bui_asset("x86_64").unwrap().url, "https://x/bui");
        assert!(m.bui_asset("aarch64").is_err(), "manifest 没放 arm64 资产就该当场报错");
        assert!(m.bui_asset("armv7l").is_err(), "架构本身不支持");
        assert!(plan_upgrade(&m, "4.0.0", &BTreeMap::new(), "aarch64").is_err(), "缺资产不许出计划");
    }

    #[test]
    fn version_compare_handles_uneven_segments() {
        assert!(version_lt("4.0.0", "4.0.1"));
        assert!(version_lt("4.0", "4.0.1"));
        assert!(version_lt("3.9.9", "4.0.0"));
        assert!(!version_lt("4.0.1", "4.0.1"));
        assert!(!version_lt("4.1.0", "4.0.9"));
        assert!(!version_lt("4.0.10", "4.0.9"), "按数字比，不是字典序");
    }

    #[test]
    fn min_upgrade_from_refuses_to_skip_the_required_intermediate_version() {
        // 总纲 C4 的可选字段：低于此版本必须先升到它。消费方规则在 P1。
        let mut m = manifest("4.2.0", "1.13.19", "00");
        m.min_upgrade_from = Some("4.1.0".into());
        let err = plan_upgrade(&m, "4.0.0", &BTreeMap::new(), "x86_64").unwrap_err().to_string();
        assert!(err.contains("4.1.0"), "{err}");
        // 已经到了门槛版本就放行
        assert!(plan_upgrade(&m, "4.1.0", &BTreeMap::new(), "x86_64").is_ok());
        // 没有这个字段时一切照旧
        let m2 = manifest("4.2.0", "1.13.19", "00");
        assert!(plan_upgrade(&m2, "4.0.0", &BTreeMap::new(), "x86_64").is_ok());
    }

    #[test]
    fn nothing_to_do_when_versions_match() {
        let m = manifest("4.0.0", "1.13.19", "00");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let p = plan_upgrade(&m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(p, UpgradePlan { self_from: "4.0.0".into(), self_to: None, kernels: vec![] });
    }

    #[test]
    fn plan_lists_self_and_kernel_upgrades() {
        let m = manifest("4.0.1", "1.14.2", "00");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let p = plan_upgrade(&m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(p.self_to.as_deref(), Some("4.0.1"));
        assert_eq!(p.kernels, vec![("sing-box".to_string(), "1.13.19".to_string(), "1.14.2".to_string())]);
    }

    #[test]
    fn apply_self_keeps_the_previous_binary() {
        let payload = b"NEWBUI".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let m = manifest("4.0.1", "1.13.19", &sum);
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), payload)]));
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755).unwrap();
        h.clear_ops();
        apply_self(&h, &f, m.bui_asset("x86_64").unwrap(), std::path::Path::new("/opt/b-ui/bin")).unwrap();
        assert_eq!(h.text("/opt/b-ui/bin/bui").as_deref(), Some("NEWBUI"));
        assert_eq!(h.text("/opt/b-ui/bin/bui.prev").as_deref(), Some("OLDBUI"));
        assert_eq!(h.mode("/opt/b-ui/bin/bui"), Some(0o755));
    }

    #[test]
    fn apply_self_refuses_a_bad_checksum() {
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), b"NEWBUI".to_vec())]));
        let m = manifest("4.0.1", "1.13.19", "deadbeef");
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755).unwrap();
        assert!(apply_self(&h, &f, m.bui_asset("x86_64").unwrap(), std::path::Path::new("/opt/b-ui/bin")).is_err());
        assert_eq!(h.text("/opt/b-ui/bin/bui").as_deref(), Some("OLDBUI"), "校验失败不替换");
    }

    #[test]
    fn rollback_restores_binary_and_the_newest_state_backup() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        std::fs::create_dir_all(crate::paths::backups_dir(&paths)).unwrap();
        std::fs::write(crate::paths::backups_dir(&paths).join("state-20260910T000000Z.json"), b"{\"old\":1}").unwrap();
        std::fs::write(crate::paths::backups_dir(&paths).join("state-20260911T000000Z.json"), b"{\"new\":1}").unwrap();
        std::fs::write(crate::paths::state_file(&paths), b"{\"current\":1}").unwrap();
        let h = FakeHost::new();
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755).unwrap();
        h.write_file(&paths.bin_dir.join("bui"), b"NEWBUI", 0o755).unwrap();
        let done = rollback(&h, &paths).unwrap();
        assert_eq!(h.text(paths.bin_dir.join("bui").to_str().unwrap()).as_deref(), Some("OLDBUI"));
        assert_eq!(
            std::fs::read_to_string(crate::paths::state_file(&paths)).unwrap(),
            "{\"new\":1}",
            "恢复最近一份备份"
        );
        assert!(done.iter().any(|l| l.contains("state-20260911T000000Z.json")));
        assert!(h.ops().contains(&"systemd:restart:b-ui".to_string()));
    }

    #[test]
    fn rollback_without_a_previous_binary_is_an_explicit_error() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        let err = rollback(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("bui.prev"), "{err}");
    }

    #[test]
    fn jitter_is_reexported_from_serve() {
        // 实现与单元测试在 Task 15；这里只保证 CLI 侧的路径可用且行为一致
        let a = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000001").unwrap();
        assert_eq!(jitter_secs(a), crate::serve::jitter_secs(a));
        assert!(jitter_secs(a) < 3600);
    }
}
```
```rust
// crates/bui/src/commands/menu.rs
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn menu_keys_are_stable_and_unique() {
        let items = items();
        let keys: Vec<&str> = items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec!["1", "2", "3", "4", "5", "6", "7", "0"]);
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
        assert_eq!(items.last().unwrap().action, MenuAction::Quit);
    }

    #[test]
    fn parse_choice_accepts_only_listed_keys() {
        let items = items();
        assert_eq!(parse_choice("1", &items), Some(MenuAction::Status));
        assert_eq!(parse_choice(" 2 \n", &items), Some(MenuAction::Reconcile));
        assert_eq!(parse_choice("0", &items), Some(MenuAction::Quit));
        assert_eq!(parse_choice("99", &items), None);
        assert_eq!(parse_choice("", &items), None);
    }

    #[test]
    fn render_is_two_columns_and_mentions_every_item() {
        let items = items();
        let text = render(&items);
        for i in &items {
            assert!(text.contains(i.title), "菜单缺 {}", i.title);
        }
        let body: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit())).collect();
        assert_eq!(body.len(), items.len().div_ceil(2), "两列排版");
    }

    #[test]
    fn items_needing_the_daemon_are_marked_when_it_is_down() {
        // spec §2.4 的白名单在本计划里扩了一项 harden-ssh（理由见 Task 7 的「为什么 harden-ssh 放行」）：
        // 守护进程没跑时可用 install / upgrade / status / reconcile / harden-ssh
        let items = items();
        let up = render_with(&items, true);
        let down = render_with(&items, false);
        assert_eq!(up, render(&items), "render 就是 render_with(.., true)");
        assert!(!up.contains("需守护进程"));
        for i in &items {
            let needs_daemon = matches!(i.action, MenuAction::Service(_) | MenuAction::Logs);
            let line = down.lines().find(|l| l.contains(i.title)).unwrap();
            // 标注**紧跟标题**（`标题(需守护进程)`），不是行尾：两列排版下一行有两项，
            // 断言「这一行里有没有标注」会把同行邻居的标注算到自己头上（4/5 同行时 3 必假阳）
            assert_eq!(
                line.contains(&format!("{}(需守护进程)", i.title)),
                needs_daemon,
                "{} 的标注不对：{line}",
                i.title
            );
        }
    }
}
```
```rust
// crates/bui/src/commands/status.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::health::{HealthResponse, ServiceStatus};
    use crate::state::runtime::{DriftItem, ReconcileReport};

    fn sample() -> HealthResponse {
        HealthResponse {
            status: "degraded".into(),
            version: "4.0.0".into(),
            uptime_secs: 3725,
            node: "node-a".into(),
            services: vec![
                ServiceStatus { unit: "hysteria-server".into(), active: true, enabled: true, n_restarts: 2 },
                ServiceStatus { unit: "xray".into(), active: false, enabled: true, n_restarts: 7 },
            ],
            reconcile: Some(ReconcileReport {
                at: "2026-09-11T00:00:00Z".into(),
                changed: vec!["/opt/b-ui/config.yaml".into()],
                errors: vec!["xray 重启失败".into()],
                ..Default::default()
            }),
            drift: vec![DriftItem { kind: "cron".into(), path: "crontab".into(), detail: "0 */6 * * * /opt/b-ui/update.sh".into() }],
            watchdog: Default::default(),
            upgrade_available: Some("4.0.1".into()),
            residential: None,
        }
    }

    #[test]
    fn status_text_shows_units_uptime_errors_and_drift() {
        let t = format_status(&sample());
        assert!(t.contains("node-a"));
        assert!(t.contains("4.0.0"));
        assert!(t.contains("1h 2m"), "uptime 要人读得懂：{t}");
        assert!(t.contains("hysteria-server") && t.contains("running"));
        assert!(t.contains("xray") && t.contains("stopped"));
        assert!(t.contains("xray 重启失败"));
        assert!(t.contains("漂移") && t.contains("crontab"));
        assert!(t.contains("4.0.1"), "有新版本要提示（spec §7 的每日自检结果）：{t}");
    }

    #[test]
    fn status_text_is_clean_when_everything_is_ok() {
        let mut h = sample();
        h.status = "ok".into();
        h.services.iter_mut().for_each(|s| s.active = true);
        h.reconcile = None;
        h.drift.clear();
        h.upgrade_available = None;
        let t = format_status(&h);
        assert!(t.contains("无漂移"));
        assert!(!t.contains("重启失败"));
        assert!(!t.contains("4.0.1"));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p bui commands::upgrade commands::status commands::menu`
Expected: 编译失败，`cannot find function plan_upgrade`。

- [ ] **Step 3: 实现 `upgrade.rs`**

```rust
pub fn version_lt(a: &str, b: &str) -> bool {
    let seg = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (x, y) = (seg(a), seg(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if l != r {
            return l < r;
        }
    }
    false
}

pub fn plan_upgrade(
    m: &Manifest,
    current_bui: &str,
    installed: &BTreeMap<String, String>,
    arch: &str,
) -> anyhow::Result<UpgradePlan> {
    // 总纲 C4 的 min_upgrade_from（可选）：跨太多版本不许直接升
    if let Some(min) = m.min_upgrade_from.as_deref() {
        if version_lt(current_bui, min) {
            anyhow::bail!("当前 {current_bui} 低于 manifest 要求的 min_upgrade_from {min}：请先 `bui upgrade --version {min}`，再升到 {}", m.version);
        }
    }
    let _ = m.bui_asset(arch)?;   // 资产缺失就直接报错，别等下载才发现
    let self_to = (m.version != current_bui).then(|| m.version.clone());
    let mut kernels = Vec::new();
    for name in crate::kernels::KERNELS {
        // manifest 的 kernels 表用下划线键（sing_box），装在盘上的二进制名用连字符
        if let Some(want) = m.kernels.get(&crate::kernels::kernels_key(name)) {
            let from = installed.get(name).cloned().unwrap_or_default();
            if &from != want {
                kernels.push((name.to_string(), from, want.clone()));
            }
        }
    }
    Ok(UpgradePlan { self_from: current_bui.to_string(), self_to, kernels })
}

pub fn apply_self(host: &dyn Host, fetcher: &dyn Fetcher, asset: &Asset, bin_dir: &Path) -> anyhow::Result<()> {
    let bytes = fetcher.get_bytes(&asset.url)?;
    let got = crate::kernels::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        anyhow::bail!("bui 二进制 sha256 不匹配：期望 {}，实际 {got}", asset.sha256);
    }
    let current = bin_dir.join("bui");
    if let Some(old) = host.read_file(&current)? {
        host.write_file(&bin_dir.join("bui.prev"), &old, 0o755)?;
    }
    host.write_file(&current, &bytes, 0o755)?;
    Ok(())
}

pub fn rollback(host: &dyn Host, paths: &Paths) -> anyhow::Result<Vec<String>> {
    let prev = paths.bin_dir.join("bui.prev");
    let bytes = host
        .read_file(&prev)?
        .ok_or_else(|| anyhow::anyhow!("没有 {}，无法回滚二进制", prev.display()))?;
    host.write_file(&paths.bin_dir.join("bui"), &bytes, 0o755)?;
    let mut done = vec![format!("已恢复上一版 bui（{}）", prev.display())];
    let backups = crate::paths::backups_dir(paths);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&backups)
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    entries.sort();
    if let Some(newest) = entries.last() {
        std::fs::copy(newest, crate::paths::state_file(paths))?;
        done.push(format!("已恢复期望态备份 {}", newest.display()));
    }
    let _ = host.systemd("restart", "b-ui");
    done.push("已重启 b-ui".into());
    Ok(done)
}

// jitter_secs 不在本文件实现，只再导出（见 Interfaces）：
pub use crate::serve::jitter_secs;
```
`run_with(rollback_flag, version, manifest_url, paths, host, fetcher)`：`--rollback` → `spawn_blocking` 里跑 `rollback(host, paths)` 并打印；否则
```rust
let (h, f, p) = (host.clone(), fetcher.clone(), paths.clone());
// 总纲 C4 的解析顺序在 kernels 里一处实现：--manifest-url > --version（模板）> $BUI_MANIFEST_URL > latest
let url = crate::kernels::manifest_url(manifest_url.as_deref(), version.as_deref());
let want = version.clone();
let (plan, asset) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
    let m = Manifest::from_url(f.as_ref(), &url)?;
    if let Some(v) = want.as_deref() {
        // 指定了版本就必须拿到那一版（本地文件或 $BUI_MANIFEST_URL 里可能是别的版本）
        anyhow::ensure!(m.version == v.trim_start_matches('v'), "manifest 里是 {}，不是请求的 {v}", m.version);
    }
    let bytes = serde_json::to_vec_pretty(&m)?;
    h.write_file(&crate::paths::manifest_file(&p), &bytes, 0o644)?;   // 缓存给对账用
    let installed = crate::kernels::installed_versions(h.as_ref(), &p.bin_dir);
    let arch = h.arch()?;
    let plan = plan_upgrade(&m, env!("CARGO_PKG_VERSION"), &installed, &arch)?;
    let asset = m.bui_asset(&arch)?.clone();
    Ok((plan, asset))
})
.await??;
```
（`Asset` 在 Task 9 已 `#[derive(Clone)]`，这里的 `.clone()` 直接可用。）打印计划 → 有新版则在 `spawn_blocking` 里 `apply_self(host, fetcher, &asset, &paths.bin_dir)` → `host.systemd("restart", "b-ui")`（守护进程重启后自己会对账，内核随 manifest 落地）；若 socket 不可用（守护进程没在跑）则改为在本进程内跑一次 `serve::reconcile_cli(paths, host, PathBuf::from(crate::paths::SOCKET_PATH), false, false)`。`run` 只是 `run_with(..., Arc::new(HttpFetcher::new()))`。**没有任何 `async fn` 直接调 `Fetcher::get_bytes`**。

- [ ] **Step 4: 实现 `status.rs` 与 `menu.rs`**

`status.rs`：`run` 先用 `ipc::Client` 取 `/api/health`；socket 不可用时退化为本地读 `state.json` + `host.unit_is_active` 现场拼一个 `HealthResponse`（`reconcile`/`drift`/`watchdog`/`upgrade_available` 从 `runtime.json` 读）。`uptime` 用 `crate::util::human_duration`。`format_status` 输出：
```
b-ui v4.0.0 @ node-a     状态: degraded     运行: 1h 2m
单元        hysteria-server running(enabled, 重启 2 次)
            xray            stopped(enabled, 重启 7 次)
上次对账    2026-09-11T00:00:00Z  变更 1 项
错误        xray 重启失败
漂移        cron crontab —— 0 */6 * * * /opt/b-ui/update.sh
```
一切正常时最后两行替换为 `漂移        无漂移`；`upgrade_available` 非空时再加一行 `新版本      4.0.1 可用（运行 b-ui upgrade）`。`--json` 直接打印 `serde_json::to_string_pretty(&health)`。

`menu.rs` 的 `items()`：
```rust
pub fn items() -> Vec<MenuItem> {
    vec![
        MenuItem { key: "1", title: "状态与体检", action: MenuAction::Status },
        MenuItem { key: "2", title: "立即对账", action: MenuAction::Reconcile },
        MenuItem { key: "3", title: "对账并清理漂移", action: MenuAction::ReconcileForce },
        MenuItem { key: "4", title: "重启数据面（两个 hysteria + xray + relay）", action: MenuAction::Service("restart") },
        MenuItem { key: "5", title: "查看日志", action: MenuAction::Logs },
        MenuItem { key: "6", title: "升级 / 回滚", action: MenuAction::Upgrade },
        MenuItem { key: "7", title: "SSH 硬化", action: MenuAction::HardenSsh },
        MenuItem { key: "0", title: "退出", action: MenuAction::Quit },
    ]
}
```
`run`：循环 `render` → 读一行 → `parse_choice` → 派发。`Status` / `Reconcile` / `ReconcileForce` / `Service` 都经 `ipc::Client`（spec §2.4「所有菜单项通过 socket 调守护进程 API」）；socket 不可用时打印「守护进程未运行，仅可用 1/2/3/6/7/0」并把 4/5 置灰（`ReconcileForce` 与 `HardenSsh` 都不置灰：前者退化为进程内对账，后者理由见 Task 7）（`render` 接受一个 `available: bool` 参数决定是否加「(需守护进程)」后缀——测试里的 `render(&items)` 对应 `available = true` 的重载 `render_with(items, true)`，本任务把签名定为 `pub fn render(items: &[MenuItem]) -> String` + `pub fn render_with(items: &[MenuItem], daemon_up: bool) -> String`，`render` 转调 `render_with(items, true)`）。**「(需守护进程)」紧跟在标题后面、不带空格**（`format!("{}(需守护进程)", i.title)`），即每个单元格渲染成 `<key>) <标题>[(需守护进程)]`，列宽按加了后缀的单元格算、不换行：两列排版下一行放两项，标注若写在行尾就分不清是左项还是右项的（`items_needing_the_daemon_are_marked_when_it_is_down` 正是按 `标题(需守护进程)` 整串断言的）；两列的行数断言（`render_is_two_columns_and_mentions_every_item`）保持不变。`Logs` 直接 `host.run("journalctl", &["-u", unit, "-n", "200", "--no-pager"])` 并打印。

- [ ] **Step 5: 收口 `main.rs`**

把剩余 arm 全部接上，删除 `not_yet`，并删掉 Task 1 加的 `#![allow(dead_code)]`（删掉后 `cargo clippy --workspace --all-targets -- -D warnings` 必须仍然全绿；若报出真正没人用的函数，就地删掉它或补上调用方，不要把 allow 加回来。`redact::url_credentials` / `redact::secret` 这时**必须**已有调用点——Task 9 的 `HttpFetcher::get_bytes` 与 Task 16 的 `install::run`，别把它们当死代码删了，Global Constraints 要求日志里的凭据一律过脱敏）。`main.rs` 顶部需要 `use std::path::PathBuf;`（下面两处 `PathBuf::from(crate::paths::SOCKET_PATH)`）：
```rust
    let paths = Paths::default_server();
    let host: Arc<dyn Host> = Arc::new(RealHost::new());
    // args.command 是 Option：无子命令时按 argv[0]（`b-ui` → 菜单），都没有则打印 help（Task 1）
    let Some(command) = args.command.or_else(|| cli::default_command(&argv0)) else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Install { domain, port, admin_password_stdin, import_v3, non_interactive, answers, yes } => {
            commands::install::run(
                commands::install::InstallOpts {
                    domain, port, admin_password_stdin, import_v3, non_interactive, answers, yes,
                    socket: PathBuf::from(crate::paths::SOCKET_PATH),
                },
                paths, host,
            ).await
        }
        Command::Upgrade { rollback, version, manifest_url } => {
            commands::upgrade::run(rollback, version, manifest_url, paths, host).await
        }
        Command::Serve => serve::run(paths, host).await,
        Command::Reconcile { force, dry_run } => {
            serve::reconcile_cli(paths, host, PathBuf::from(crate::paths::SOCKET_PATH), force, dry_run).await
        }
        Command::Status { json } => commands::status::run(json, paths, host).await,
        Command::ImportV3 { dir, out } => commands::import_v3::run(dir, out, paths, host).await,
        Command::AuthHook { .. } => unreachable!("auth-hook 已在 main 里提前返回（Task 1 的入口）"),
        Command::Menu => commands::menu::run(paths, host).await,
        Command::HardenSsh => commands::harden_ssh::run(paths, host).await,
    }
```
`AuthHook` 是 P2 的交付物：Task 1 的 `main` 在建 tokio runtime、初始化日志**之前**就 `bail!("auth-hook 由 P2 实现")`（spec §3.2 要求钩子不初始化 tokio/tracing、不加载 state），所以派发函数里这一支是 `unreachable!`。P2 落地钩子时只改 `main` 里那一支。

- [ ] **Step 6: 运行全量检查**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。本任务自己 16 passed（upgrade 10 + menu 4 + status 2）；`cargo test -p bui` 的总数以各任务 Expected 之和为准，本计划不再给一个容易过期的合计数（本机没装 caddy 时 `caddyfile_passes_a_real_caddy_validate` 打印 skipped 仍计 passed）。再手验入口：`sudo ln -sf $PWD/target/debug/bui /tmp/b-ui && /tmp/b-ui` 进菜单（socket 不可用时亮 1/2/3/6/7/0，4/5 标「需守护进程」），`target/debug/bui` 裸跑打印 help，`target/debug/bui --version` 只打印 `4.0.0`。

- [ ] **Step 7: Commit**

```bash
git add crates/bui/src/commands/upgrade.rs crates/bui/src/commands/status.rs crates/bui/src/commands/menu.rs crates/bui/src/main.rs
git commit -m "feat(bui): upgrade/--rollback、status 与 sudo b-ui 数字菜单"
```

---

### Task 18: M1 机器化验收脚本与清单

**Files:**
- Create: `scripts/m1-acceptance.sh`, `docs/superpowers/checks/2026-09-11-v4-m1-acceptance.md`

**Interfaces:**
- Consumes: 装好的 v4（`bui status --json`、`bui reconcile --dry-run`、`bui install`、`<base>/bin/{xray,sing-box,caddy}`）
- Produces: 一条可在 bwg-rick 上跑的命令 `bash scripts/m1-acceptance.sh`，逐项打印 `PASS` / `FAIL` 并以非零退出码收尾；`bash scripts/m1-acceptance.sh --self-test` 在本机（无 root、不碰系统）用内置样例 JSON 验证脚本自己的判定逻辑。

spec §9 的 M1 验收有五项，本任务把**能机器化的三项**写成脚本，另两项明确写清归属，避免「验收标准无人认领」：

| M1 验收项 | 谁来验 | 命令 |
|---|---|---|
| 二次 `install` 零变更 | 本脚本（+ Task 15 `first_pass…second_pass_is_a_no_op`、Task 16 `a_second_install_changes_nothing` 的单元级版本） | `bash scripts/m1-acceptance.sh` 的 step 2 |
| 体检无漂移 | 本脚本（+ Task 16 `install_with_import_v3_keeps_the_v4_relay_and_leaves_no_drift`） | step 1 |
| 渲染结果过真实内核校验（`xray run -test` / `sing-box check` 1.12+1.13+1.14 / `caddy validate`） | 本脚本 | step 3 |
| 每个现有用户三种订阅与 v3 逐项相同 | **P0** 的 golden 测试（`cargo test -p bui-schema --test golden_subscription`）+ **P2** 的订阅端点任务；P1 不产出订阅端点 | 见清单文档 |
| v2rayN 四节点可连 | **主理人**手动（P2 里程碑验收时） | 见清单文档 |

- [ ] **Step 1: 写自测（脚本的判定逻辑先可测）**

`scripts/m1-acceptance.sh` 的 `--self-test` 分支就是本任务的「失败测试」：先写下面这段自测，运行它，因为 `check_status_json` / `check_report_clean` 还不存在而失败。

```bash
self_test() {
  local clean dirty out
  clean='{"status":"ok","services":[{"unit":"b-ui","active":true},{"unit":"xray","active":true}],
          "drift":[],"reconcile":{"changed":[],"restarted":[],"errors":[],"verify_failures":[]}}'
  dirty='{"status":"degraded","services":[{"unit":"xray","active":false}],
          "drift":[{"kind":"cron","path":"crontab","detail":"x"}],
          "reconcile":{"changed":[],"restarted":[],"errors":["boom"],"verify_failures":[]}}'
  out=$(check_status_json "$clean")
  if [ -z "$out" ]; then ok "自测：干净体检判通过"; else no "自测：干净体检被误判" "$out"; fi
  out=$(check_status_json "$dirty")
  if [[ "$out" == *degraded* && "$out" == *drift* && "$out" == *errors* && "$out" == *xray* ]]; then
    ok "自测：脏体检四项全报出"
  else
    no "自测：脏体检漏报" "$out"
  fi
  out=$(check_report_clean '{"changed":[],"restarted":[],"errors":[],"verify_failures":[],"drift":[],"notes":[]}')
  if [ -z "$out" ]; then ok "自测：零变更报告判通过"; else no "自测：零变更报告被误判" "$out"; fi
  out=$(check_report_clean '{"changed":["/opt/b-ui/config.yaml"],"restarted":["hysteria-server"],"errors":[],"verify_failures":[],"drift":[],"notes":[]}')
  if [[ "$out" == *config.yaml* ]]; then ok "自测：有变更的报告被判失败"; else no "自测：漏判变更" "$out"; fi
}
```

- [ ] **Step 2: 运行确认失败**

Run: `bash scripts/m1-acceptance.sh --self-test`
Expected: `bash: check_status_json: command not found` 一类，退出码非零。

- [ ] **Step 3: 写脚本**

`scripts/m1-acceptance.sh`（`bash -n` 与 `shellcheck -S error` 必须过；判定逻辑用 `python3` 解析 JSON，机器上没有 `python3` 时明确报错而不是静默跳过）：
```bash
#!/usr/bin/env bash
# b-ui v4 M1 机器化验收（spec §9）。
#   真机：sudo bash scripts/m1-acceptance.sh
#   自测：bash scripts/m1-acceptance.sh --self-test   # 不碰系统、不需要 root
# 环境变量：BUI（默认 /opt/b-ui/bin/bui）、BASE（默认 /opt/b-ui）、
#           SB112/SB113/SB114（三个 sing-box 版本的二进制路径，缺则跳过该版本）
set -uo pipefail

BUI=${BUI:-/opt/b-ui/bin/bui}
BASE=${BASE:-/opt/b-ui}
pass=0
fail=0

ok() { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
no() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

need_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'FATAL 需要 python3 来解析 JSON\n'
    exit 2
  fi
}

# 体检 JSON → 问题描述（空串 = 通过）
check_status_json() {
  python3 - "$1" <<'PY'
import json, sys
try:
    h = json.loads(sys.argv[1])
except Exception as e:
    print("status JSON 解析失败: %s" % e)
    raise SystemExit(0)
p = []
if h.get("status") != "ok":
    p.append("status=%s" % h.get("status"))
if h.get("drift"):
    p.append("drift=%d 条: %s" % (len(h["drift"]), h["drift"][:3]))
r = h.get("reconcile") or {}
if r.get("errors"):
    p.append("errors=%s" % r["errors"])
if r.get("verify_failures"):
    p.append("verify_failures=%s" % r["verify_failures"])
down = [s["unit"] for s in h.get("services", []) if not s.get("active")]
if down:
    p.append("未运行: %s" % ",".join(down))
print("; ".join(p))
PY
}

# 对账报告 JSON → 问题描述（空串 = 零变更零错误）
check_report_clean() {
  python3 - "$1" <<'PY'
import json, sys
try:
    r = json.loads(sys.argv[1])
except Exception as e:
    print("报告 JSON 解析失败: %s" % e)
    raise SystemExit(0)
p = []
for k in ("changed", "restarted", "errors", "verify_failures"):
    if r.get(k):
        p.append("%s=%s" % (k, r[k]))
print("; ".join(p))
PY
}

run_checks() {
  # step 1：体检无漂移、六单元在跑、上轮对账无错
  local status_json out
  status_json=$("$BUI" status --json 2>/dev/null)
  out=$(check_status_json "$status_json")
  if [ -z "$out" ]; then ok "step1 体检：无漂移、无错误、六单元在跑"; else no "step1 体检不干净" "$out"; fi

  # step 2：二次 install 零变更 + 二次对账零变更
  out=$("$BUI" install --yes 2>&1 | tail -n 20)
  if printf '%s' "$out" | grep -q "已安装"; then ok "step2 二次 install 走对账路径（不覆盖 state）"; else no "step2 二次 install 行为异常" "$out"; fi
  local report
  report=$("$BUI" reconcile --dry-run 2>/dev/null)
  out=$(check_report_clean "$report")
  if [ -z "$out" ]; then ok "step2 二次对账零变更"; else no "step2 二次对账仍有改动" "$out"; fi

  # step 3：渲染结果过真实内核校验
  if "$BASE/bin/xray" run -test -c "$BASE/xray-config.json" >/dev/null 2>&1; then
    ok "step3 xray run -test"
  else
    no "step3 xray run -test 失败" "$("$BASE/bin/xray" run -test -c "$BASE/xray-config.json" 2>&1 | tail -n 3)"
  fi
  local sb
  for sb in "${SB112:-}" "${SB113:-}" "${SB114:-}" "$BASE/bin/sing-box"; do
    [ -n "$sb" ] || continue
    [ -x "$sb" ] || { skip "step3 sing-box check（$sb 不可执行）"; continue; }
    if "$sb" check -c "$BASE/singbox-relay.json" >/dev/null 2>&1; then
      ok "step3 sing-box check（$("$sb" version | head -n1)）"
    else
      no "step3 sing-box check 失败（$sb）" "$("$sb" check -c "$BASE/singbox-relay.json" 2>&1 | tail -n 3)"
    fi
  done
  if "$BASE/bin/caddy" validate --config "$BASE/Caddyfile" --adapter caddyfile >/dev/null 2>&1; then
    ok "step3 caddy validate（配置在 $BASE/Caddyfile）"
  else
    no "step3 caddy validate 失败" "$("$BASE/bin/caddy" validate --config "$BASE/Caddyfile" --adapter caddyfile 2>&1 | tail -n 3)"
  fi

  # step 3b：鉴权快照存在、0600、形状合总纲 C5（spec §3.2；形状不对 hysteria 会 fail-closed 拒绝所有人）
  if [ -f "$BASE/auth-snapshot.json" ] && [ "$(stat -c %a "$BASE/auth-snapshot.json")" = "600" ]; then
    out=$(python3 - "$BASE/auth-snapshot.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("不是合法 JSON: %s" % e)
    raise SystemExit(0)
p = []
if d.get("schema") != 1:
    p.append("schema=%r（总纲 C5 要求 1）" % d.get("schema"))
if not isinstance(d.get("users"), dict):
    p.append("缺 users 对象（顶层键只能是 schema/users，不是扁平的用户名表）")
print("; ".join(p))
PY
)
    if [ -z "$out" ]; then
      ok "step3b auth-snapshot.json 存在、0600、形状合 C5（schema=1 + users）"
    else
      no "step3b auth-snapshot.json 形状不对" "$out"
    fi
  else
    no "step3b auth-snapshot.json 缺失或权限不对" "$(ls -l "$BASE/auth-snapshot.json" 2>&1)"
  fi

  # step 4：CLI 入口（spec §2.4）
  if [ "$(readlink -f /usr/local/bin/b-ui)" = "$(readlink -f "$BASE/bin/bui")" ]; then
    ok "step4 /usr/local/bin/b-ui 指向 bin/bui"
  else
    no "step4 b-ui 入口不对" "$(readlink -f /usr/local/bin/b-ui)"
  fi
  if [ -S /run/b-ui.sock ] && [ "$(stat -c %a /run/b-ui.sock)" = "600" ]; then
    ok "step4 /run/b-ui.sock 存在且 0600"
  else
    no "step4 socket 异常" "$(ls -l /run/b-ui.sock 2>&1)"
  fi
}

main() {
  need_python
  if [ "${1:-}" = "--self-test" ]; then
    self_test
  else
    run_checks
  fi
  printf '\n合计 %d PASS / %d FAIL\n' "$pass" "$fail"
  [ "$fail" -eq 0 ]
}

main "$@"
```
（`self_test` 就是 Step 1 写的那段，放在 `run_checks` 之后、`main` 之前。）

- [ ] **Step 4: 运行自测与静态检查**

Run: `bash -n scripts/m1-acceptance.sh && shellcheck -S error scripts/m1-acceptance.sh; bash scripts/m1-acceptance.sh --self-test`
Expected: `bash -n` 与 `shellcheck` 无输出；自测打印 4 条 `PASS`、`合计 4 PASS / 0 FAIL`，退出码 0。（`shellcheck` 未安装则跳过该命令，并在 commit message 里注明。）

- [ ] **Step 5: 写清单文档**

`docs/superpowers/checks/2026-09-11-v4-m1-acceptance.md`：
````markdown
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

## 2. 装机与导入（人工执行，脚本之前）
```
sudo bash install.sh              # P5 交付前用：sudo /opt/b-ui/bin/bui install --import-v3 --admin-password-stdin
sudo /opt/b-ui/bin/bui status --json | python3 -m json.tool | head -40
```
期望：`status": "ok"`、`drift": []`、`services` 六项 `active: true`、`reconcile.errors` 为空；`/opt/b-ui/v3-backup/` 下有 `users.json` / `reality-keys.json` / `residential-proxy.json` / `admin.env` / `.resi-health-state.json` / `.relay.lock` / `port-hopping.json` / `masquerade.json` / `server_ip.txt` 这 9 个归档文件（0600，机器上原本没有的不会出现）外加迁移块留下的 `*.bak.*`；`/opt/b-ui` 顶层没有 v3 的 `*.sh`、`admin/`、`install-key.txt`、`sing-box`（v4 的那份在 `bin/`）、`*.bak.*`、`*.tmp`。**顶层逐项过一遍**：`ls -A /opt/b-ui` 的结果必须全部落在 Task 5 的 `BASE_WHITELIST` 13 项 + 本轮 artifact 的路径集合里，否则就是一条 `stray_file`（脚本的 step1 会替你报出来）。另外人工确认 Caddy 数据已迁移、没有重签：`ls /opt/b-ui/caddy/caddy/certificates/*/*/` 里是导入前那套证书，`journalctl -u caddy --since -10min | grep -ci "obtain"` 为 0（有输出说明 ACME 账号没搬成功，按 Task 16 的 `migrate_caddy_data` 排查）。

## 3. 不由本脚本覆盖的两项（写清归属，避免无人认领）
| 项 | 归属 | 怎么验 |
|---|---|---|
| 每个现有用户三种订阅与 v3 逐项相同 | P0 的 golden 测试 + P2 的订阅端点任务 | 本机 `cargo test -p bui-schema --test golden_subscription`；bwg-rick 上 P2 合并后 `curl -s http://127.0.0.1:8080/api/sub/<user> \| base64 -d` 与 v3 抓取的样本逐行 `diff` |
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
````

- [ ] **Step 6: Commit**

```bash
git add scripts/m1-acceptance.sh docs/superpowers/checks/2026-09-11-v4-m1-acceptance.md
git commit -m "test(bui): M1 机器化验收脚本与清单"
```

---

## 自查

**1. Spec 覆盖**

| spec 章节 | 要求 | 落在哪 |
|---|---|---|
| §1 | `crates/bui` 形态：守护进程 + CLI（`install / upgrade / serve / reconcile / status / import-v3 / auth-hook / menu`）；`sudo b-ui` 是 `bui menu` 的符号链接 | Task 1（CLI 定义 + `default_command` 让 `b-ui` 裸跑进菜单）、Task 8（`Artifact::Symlink` 产出 `/usr/local/bin/{bui,b-ui}`）、Task 15（serve）、16、17；`auth-hook` 按 spec §3.2 归 P2，Task 17 的 arm 明确报错 |
| §2.1 | `state.json` 600、临时文件 + rename、备份保留 10 份；`runtime.json` 丢了能重建、还要放 P2/P3 的运行时数据 | Task 2（含 `#[serde(flatten)] extra`，P2/P3 追加字段不必回头改 P1） |
| §2.2 | Artifact 模型（文件/单元/单元状态/sysctl/**内核模块**/防火墙/二进制/符号链接/删除项）、内容哈希 diff、重启映射、结构哈希不重启 xray、验证再重启、重启失败回滚、触发（启动/500ms 去抖/10 分钟/手动）、漂移只报不改 + `--force` 清理 | Task 4（模型与 diff）、Task 5（apply/drift，含 `Modprobe` 在 `SetSysctl` 之前、`BASE_WHITELIST` 13 项 + `MANAGED_CONF_DIRS` 的 `stray_conf`、`b-ui` 自身重启交给调用方）、Task 15（触发器与装配） |
| §2.2 表「内核参数：sysctl 值、nf_conntrack 模块」 | 模块要**当轮**加载，不能只写 `modules-load.d` | Task 4 的 `Artifact::Modprobe` + diff 规则 5（查 `/sys/module/<m>`）、Task 5 的 apply 第 5 步、Task 6 产出 `Modprobe{nf_conntrack}` |
| §2.2「防火墙：没装就在体检里提示」（× §9 M1「二次 install 零变更」） | 没有活防火墙的机器上必须**既提示又零变更** | Task 6 的 `render` 只在 `facts.ufw_active \|\| facts.firewalld_active` 时产出 `FirewallPorts`；提示由 Task 15 的 `reconcile_once` 从 `facts` 追加进 `notes`（`notes` 不进 `changed`、不影响 `/api/health`）；Task 5 第 10 步的「两者都没有」支降级为守卫；Task 18 清单 §0 要求先实测并记录 `ufw` / `firewalld` 状态 |
| §2.3 | `bui import-v3` 调 `bui_schema::v3::import`，卸载 v3 单元/timer/cron/shell/Node 文件、**v3 早期版本的 iptables/nft 端口跳跃孤儿规则**，保留 `certs/` 与 Caddy 数据目录 | Task 16（`V3_UNITS` 不含 v4 的 `b-ui-relay.service`；`V3_FILES` 13 项含 v3 顶层的 `sing-box` 与 `cert-check.sh`、`V3_STATE_FILES` 9 项含 `port-hopping.json` / `masquerade.json` / `server_ip.txt`、`sweep_v3_leftovers` 清迁移块的 `*.bak.*` 与 `*.tmp`——每一项都对着 v3 脚本核实过行号，漏一项就是一条永久 `stray_file`；v3 状态文件归档进 `v3-backup/`；`admin/` 整棵 `remove_dir_all`；卸载排在对账**之前**；`migrate_caddy_data` 先把 `/var/lib/caddy/.local/share/caddy/` 搬到 `<base>/caddy/caddy/` 再停发行版 caddy——2026-09-12 裁决；最后一步 `flush_v3_portjump_rules` 清 `HYSTERIA-PR-*` 链与 `hysteria_*` nft 表，spec §3.1「v4 不再有任何 iptables/nft 规则」由此闭合——v4 运行中的 hysteria 自己建的同名链**不**纳入漂移扫描） |
| §2.4 | `sudo b-ui` 数字菜单经 `/run/b-ui.sock`（0600）调 API；守护进程未运行时只允许 install/upgrade/status/reconcile（本计划放行的完整集合是 7 项：再加 `harden-ssh` / `import-v3` / `menu`，逐项理由与回写 spec 用的表在 Task 7） | Task 14（socket）、Task 17（菜单与降级、`render_with(items, false)` 的标注）；socket 路径由调用方传入（见下面「类型一致」一节） |
| §3.1 | 四个内核为下载的静态二进制，sha256 校验，放 `bin/`；不再调上游安装器、不装 Node | Task 9（下载/校验/版本，`Fetcher: Send+Sync+'static` 且只在 `spawn_blocking` 里调）、Task 10（Binary artifact）、Task 8（单元指向 `bin/`） |
| §3.2 | `auth: { type: command, command: /opt/b-ui/bin/bui auth-hook }`；快照 600、fail-closed | Task 10（把 P0 渲染好的 `config.yaml` 写盘，`auth` 段已是 `command`）、Task 16（`install` 第 8 步写初版 `auth-snapshot.json`，形状按总纲 C5 的 `{schema, users}`，见文首契约段）；钩子本体与快照的持续重写、到期/超限判定归 **P2**，Task 1 的 `main` 在建 runtime 前就以「auth-hook 由 P2 实现」报错 |
| §3.3 | `clients[].email = user_id`；增删用户走 gRPC 不重启 xray | Task 10（直接落 `bui_schema::render::xray::config` 的产出，`restart_key` 用排除 `settings.clients` 的 `structural_hash`）；gRPC 客户端归 P2 |
| §3.4 | 证书 inotify + 间隔 10 秒重启两个 hysteria；静态 DNS；sysctl 与 conntrack 分档（**与 v3 同值**由 Task 6 的 golden 测试 `v4_sysctl_values_match_the_v3_block_verbatim` 逐键对着 `server/core.sh` 比，不自证；≤2G 机器的 `99-b-ui-memory.conf` 同名同值由 Task 6 接管、>2G 产出 `Absent`，v3 的 `99-hysteria-bbr.conf` 由 Task 8 的 `LEGACY_FILES` 删除）；防火墙；SSH 硬化一份实现；单元资源限制（含 relay/xray 的 MemoryMax、`b-ui` 自身 200M）；watchdog 60s/2 次/1-2-4 分钟退避；`/tmp/hy2-watchdog-*` 与 timer 删除；日志脱敏 | Task 11、6、6、6、7、8、12、16（`/tmp/hy2-watchdog-*` 在 `uninstall_v3` 里删、timer 由 `LEGACY_UNITS` 停用删除）、1 |
| §7 | `bui install`（交互 / `--non-interactive --answers <file>` / 幂等）、`bui upgrade`（**按 `--version` 取指定版本的 manifest** → 原子替换 → 重启 → 对账）、`--rollback`（上一版二进制 + 最近 state 备份）、**守护进程每日带抖动自检** | Task 16、17、**Task 15 的 `selfcheck_loop`**（延 `jitter_secs(node.id)` → 每 24h 拉 manifest → 写 `<base>/manifest.json` → 刷新 `CoreFilesModule` 的共享 manifest 句柄 → 记 `runtime.upgrade_available` → 发 `ReconcileRequested`）。`install` 交互里的「可选住宅 URL」按分工归 **P3**（Task 16 已注明），GitHub Actions 归 P5 |
| §9 M1 | 二次 install 零变更、体检无漂移、渲染过真实内核校验、三种订阅逐项相同、v2rayN 四节点可连 | Task 18（前三项机器化；后两项写清归属：P0 golden + P2 端点、主理人手动） |
| 总纲 C2 | `Store`、`Module` trait（`name`/`render`/`routes`/`spawn`）、`Artifact`、`AppState`；axum Router 骨架，`/api/health` 与 `/api/login` 落地 | Task 2、4、13；另含 CLI 必需的 `POST /api/reconcile` 与 `POST /api/services/{unit}/{action}`（spec §2.4 要求菜单经 socket 调 API，故属 P1；其余端点仍归 P2/P3）。对 C2/C3 的字段细化集中在文首「依赖与契约决策」一节——2026-09-12 裁决已批准，回写总纲时按该节覆盖 |
| 总纲 C3 | 路径与单元名 | Task 1（`paths.rs`，含 `caddyfile(&Paths)` = `<base>/Caddyfile`、`auth_snapshot_file`）、Task 8（单元名与 `caddy.service --config <base>/Caddyfile`）；新增路径列在文首契约段 |
| 总纲 C4 | `manifest.json` 的形状与键名、**来源与覆盖**（`--manifest-url` / `$BUI_MANIFEST_URL`）、可选字段 `min_upgrade_from` | Task 9（`Manifest{version, kernels, artifacts, min_upgrade_from}` + `kernels_key` / `artifact_key` + `resolve_manifest_url` / `manifest_url` / `manifest_url_for_version`，`HttpFetcher` 兼容 `file://` 与本地路径）、Task 10（按键查 `artifacts` 出 `Binary`）、Task 15（缓存与每日自检用解析后的 URL 与 `m.version`）、Task 16/17（装机与升级按架构查 `bui-linux-<arch>`；`plan_upgrade` 消费 `min_upgrade_from`） |
| 总纲 C5 | CLI 逐字契约（`--version` 只打印版本号、`install [--non-interactive --answers <file>]`、`upgrade [--version] [--manifest-url] [--rollback]`、`reconcile [--force]`、`auth-hook`、`harden-ssh`、`menu`）与 `auth-snapshot.json` 的 `{schema, users}` 形状 | Task 1（`Cli`/`Command` 逐字照 C5，`--version` 自己处理；`auth-hook` 在建 runtime 前返回）、Task 16（`load_answers` 定义答案文件格式、`auth_snapshot` 输出 C5 形状）、Task 17（`upgrade` 的三个开关）、Task 18（step3b 校验快照形状） |

**不在本计划内（按范围划分）**：`auth-hook` 的钩子逻辑与 `auth-snapshot.json` 的持续重写（P1 只在 `install` 写一次初版，形状见文首契约段）、Xray gRPC、流量采样与限额、前端嵌入与面板其余端点（P2）；住宅上游管理/体检/健康切换/黑名单、`install` 交互里的可选住宅 URL、relay 选择重放的消费端（P3，P1 只发 `Event::RelayRestarted`）；`bui-c`（P4）；GitHub Actions、`install.sh` 引导、soak/压测、删除 v3 文件（P5）。

**2. 占位扫描**：全篇无 TBD / TODO / 「添加适当的错误处理」/「类似 Task N」。每个代码步骤给的是可直接落地的 Rust / Bash；上一版里三处「先给错代码再用散文改」已全部改成最终代码（Task 11 的 `RenderCtx` 构造、Task 13 的第 6 个测试与三个测试辅助、Task 15 的 `reconcile_from_ctx`、Task 16 的两个 install 测试改用 tempdir 派生的 `Paths`）。四处「实现要点用条目描述」的地方（`RealHost` 的逐方法封装、`FakeHost` 的字段与 `list_dir`/`is_dir` 语义、`status`/`menu` 的文本排版、`install::run` 的交互采集）都给了完整字段清单、输出样例与断言，测试是可执行的规格。移植来源全部带文件与行号。第三轮修订新增的每一段也都是可落地代码：Task 2 的 `backup` 命名与裁剪、`Runtime::update` 的 `spawn_blocking` + `write_atomic`、Task 6 的 `render` 防火墙判据与 golden 测试、Task 15 的 `reconcile_once` 防火墙提示与新测试、Task 16 的 `sweep_v3_leftovers` 全文与三份常量的核实注释、Task 9 的 `HttpFetcher` 脱敏两处、Task 16 的密码生成片段。

**3. 类型一致**（跨任务复查）
- `Store::{open, create, read, update, path}`（Task 2）被 7、11、12、13、14、15、16 使用，签名一致。
- `Runtime::{load, read, update}` 与 `RuntimeData` 的八个字段（Task 2）被 11（`cert_sha256`）、12（`watchdog`）、13（`drift`/`last_reconcile`/`upgrade_available`）、15（`restart_keys`/`drift`/`last_reconcile`/`started_at`/`upgrade_available`）使用；`extra` 留给 P2/P3。
- `Host` 的全部方法（Task 3，本次新增 `is_dir` / `remove_dir_all` / `read_link` / `symlink` 四个）在 5、6、7、9、10、11、12、13、15、16、17 中被调用；`FakeHost.ops()` 的字符串格式（含新增 `rmdir:`、`symlink:<link>-><target>`）在 5、6、7、8、9、11、12、14、16、17 的断言里统一；`FakeHost` 的单元名归一化（补 `.service`）决定了播种写法：`units_active` / `units_enabled` / `units_exist` / **`unit_props`** 一律用**全名**（Task 13 的 `("hysteria-server.service","NRestarts")`），只有 `fail_units` 裸名与全名都认（Task 5）；`list_dir` 只返回直接子项这一语义被 Task 5 的 `stray_file`、Task 16 的 `/tmp/hy2-watchdog-*` 清理共同依赖。
- `Artifact` 的九个变体与 `Artifact::file().mode().restart().restart_key().verify().immutable()` 建造链（Task 4）被 6、7、8、10 使用；`Unit::{restart, reload, service}`、`Verify`、`PortSpec::{one, range, ufw, firewalld}` 同。
- `MANAGED_UNITS` / `LEGACY_UNITS` 只有 Task 4 一份，被 5（drift）、8（产出 `UnitState`+`Absent`）、13（health 服务表与 `/api/services` 白名单）、16（`v3_units_never_touch_a_v4_managed_unit` 反向断言）引用。
- `plan(PlanInput{artifacts, paths, keys, installed_versions}, host)`（Task 4）与 `apply(ApplyInput{plan, paths, facts, installer, dry_run}, host)`（Task 5）在 7、15 被调用，参数名与顺序一致。
- `BinaryInstaller::install(name, version, sha256, url, dest)`（Task 5）由 Task 9 的 `KernelInstaller` 实现，Task 7 的 `NoBinaries` 与 Task 15/16 测试里的 `NoopInstaller` 同签名。
- `Module::{name, render, routes, spawn}` 与 `RenderCtx{paths, facts}`、`Facts` 的九个字段、`DaemonCtx{store, runtime, bus, host, paths}`（Task 4）被 6–12、15、16 使用；`serve::modules()` 返回的 `Registry{modules, manifest}` 里模块顺序固定为 `["core-files","units","system","ssh","certs","watchdog"]`。
- `AppState{store, bus, runtime, host, started_at, version, login}`（Task 4 六字段 + Task 13 的 `login`）与 `EventBus::{new, send, subscribe}`、`Event` 三变体（Task 4 的 `api/state.rs`）被 11、12、13、14、15 使用。
- `HealthResponse` / `ServiceStatus`（Task 13，含 `upgrade_available`）被 Task 17 的 `status` 复用（服务端与 CLI 同一份类型）。
- `Manifest{version, kernels: BTreeMap<String,String>, artifacts: BTreeMap<String,Asset>, min_upgrade_from: Option<String>}`（总纲 C4 形状，含可选字段）与 `Manifest::{from_url, kernel_asset, bui_asset}`、`kernels_key`、`artifact_key`、`Asset`（`Clone`）、`Fetcher::get_bytes`、`installed_versions`、`sha256_hex`（Task 9）被 10、15、16、17 使用：Task 10 只经 `kernel_asset(name, arch)`，Task 17 只经 `bui_asset(arch)`、`m.kernels` 与 `m.min_upgrade_from`，四处的 JSON / 结构体 fixture 都是 C4 形状（都写了 `min_upgrade_from: None`）；`Arc<dyn Fetcher>` 在 15/16/17 的签名里一致。
- manifest 地址只有一处实现：`kernels::{MANIFEST_URL, MANIFEST_URL_TEMPLATE, MANIFEST_URL_ENV, manifest_url_for_version, resolve_manifest_url, manifest_url}`（Task 9）。Task 15 的 `run` 传 `manifest_url(None, None)` 给 `selfcheck_loop`；Task 16 的 `run` 传 `manifest_url(None, None)` 给 `run_with(.., manifest_url: String, ..)`；Task 17 的 `run_with` 传 `manifest_url(cli.manifest_url, cli.version)`。三处都不自己拼字符串，测试一律显式传固定 URL（不读进程环境）。
- `ApplyOutcome{changed, restarted, notes, verify_failures, errors, keys, relay_restarted, self_restart_required}`（Task 5）→ `ReconcileReport{…, self_restart_required}`（Task 2）→ `serve::finish_self_restart`（Task 15）是一条链：`apply` 不重启 `b-ui`，`reconcile_once` 把标记搬进报告，`finish_self_restart` 在报告落盘后执行（守护进程 `--no-block`，CLI 同步）。Task 16 的 install 与 Task 15 的 `reconcile_cli` 都调它。
- `auth_snapshot(&State) -> serde_json::Value`（Task 16）是 P1 唯一碰 `auth-snapshot.json` 的地方，形状 = 总纲 C5 的 `{"schema":1,"users":{"<username>":{user_id,hy2_password,expires_at,blocked}}}`，与文首契约段逐字一致；P2 的钩子与快照重写沿用同一形状。
- `crate::util::{fmt_rfc3339, parse_rfc3339, human_duration}`（Task 1）被 12（退避时间戳）、13（`expires_at`）、15（报告时间戳、`started_at`）、17（uptime）使用，不再有各文件自己的时间格式化副本。
- **socket 路径一律由调用方传入**（第三轮审查 C3）：`crate::paths::SOCKET_PATH` 这个常量只在 `main.rs` 的三处（`Command::Install` 的 `InstallOpts.socket`、`Command::Reconcile` 的 `serve::reconcile_cli` 第三参、Task 15 的 `serve::run` 里 `serve_uds` 的监听路径）与 Task 17 的 `upgrade::run_with` 里出现；`commands::install::run_with`（用 `opts.socket`）、`serve::reconcile_cli`（用 `socket` 参数）、`commands::status::run` / `commands::menu::run`（用 `paths` 与传入的路径）自己都不读它。于是 Task 16 的五处 install 测试传 `d.path().join("absent.sock")`，`Client::available()` 立刻 false、走进程内那一支，全程不碰 `/run/b-ui.sock`。
- `crate::redact::{url_credentials, secret}`（Task 1）的调用点：`url_credentials` 在 Task 9 的 `HttpFetcher::get_bytes`（`send()` 失败的 `warn!` 与 HTTP 非 2xx 的 `bail!`），`secret` 在 Task 16 的 `install::run`（随机管理员密码那一行 `tracing::info!`）。两个函数都有真实调用点，Task 17 收口删掉 `#![allow(dead_code)]` 时不会被当成死代码删掉。
- `crate::state::store::write_atomic`（Task 2，`pub(crate)`）是全 crate 唯一的「tmp + 0600 + rename」落盘实现，`Store::{create, update}` 与 `Runtime::update` 共用它；`Host::write_file`（Task 3）是另一条路径，只用于对账写出的 artifact（模式由 artifact 决定）。
- `crate::modules::system::firewall_ports(&Ports) -> Vec<PortSpec>`（Task 6）有两个调用者：`SystemModule::render`（有活防火墙时包成 `Artifact::FirewallPorts`）与 `serve::reconcile_once`（没有防火墙时拼 `notes` 文案）。两处的判据同为 `facts.ufw_active || facts.firewalld_active`，互为补集，不会同时命中。
- `jitter_secs(node_id) -> u64` 定义在 **Task 15 的 `serve.rs`**（`selfcheck_loop` 要用它），Task 17 的 `commands::upgrade` 只 `pub use crate::serve::jitter_secs;`。这样收口链上没有任何反向引用（15 不依赖 17）。
- 消费 P0（总纲 C1）的四处签名与 `crates/bui-schema/src` 的真实代码逐字对齐（2026-09-12 复核）：`render::hysteria::{direct_yaml(&NodeParams, &Paths), residential_yaml(&NodeParams, &Paths)}`、`render::xray::{config(&NodeParams, &[User], &Paths), structural_hash(&Value)}`、`render::relay::{config(&ResidentialGroup, &RelayOpts), RelayOpts{listen_port, api, cache_path, server_ip}}`（`RelayOpts` 在 `render/relay.rs`，不是 `render/mod.rs`）、`v3::import(&Path) -> Result<ImportReport, ImportError>`（Task 10、16）。P1 不消费 `render::subscription::singbox(&[Node], &SplitRules, dial_ip)`、`render::client::{ClientOpts, ClientMode}`、`render::SplitRules`——它们分别归 P2/P4/P3，列在文首契约段以免重复实现。

**4. 与 v3 的行为差异（有意为之，均有依据）**
- 登录限速 5 次/**分钟**/IP（spec §4.3），v3 是 5 次/5 分钟（`server.js:196`）；IP 取 `X-Forwarded-For` 最后一跳（Caddy 反代下 `ConnectInfo` 永远是 127.0.0.1）。
- JWT 密钥持久化在 `state.admin.jwt_secret`（修 web-C17）。
- 静态 DNS 去掉 `9.9.9.9`（spec §3.4 只列 1.1.1.1 / 8.8.8.8）。
- Caddy 改用 `bin/caddy` + `XDG_DATA_HOME` 与 `XDG_CONFIG_HOME` 都是 `<base>/caddy`、配置改为 `<base>/Caddyfile`（不再写 `/etc/caddy/Caddyfile`）、日志进 journald；证书监听目录随之变成 `<base>/caddy/caddy/certificates/…`。`import-v3` 把发行版 Caddy 的数据目录（`/var/lib/caddy/.local/share/caddy/`，含 ACME 账号与已签证书）整棵复制过来再停它，所以**不会重新签发**（避开 Let's Encrypt 速率限制）；`<base>/certs/` 也保留，Caddy 万一重签期间 hysteria 仍用旧证书。
- 两个 hysteria 的证书重启之间加 10 秒间隔（v3 连续 restart）。
- watchdog 阈值由「5 分钟 ×3」改为「60 秒 ×2」+ 退避，覆盖面从 2 个内核扩到 4 个；`/tmp/hy2-watchdog-*` 计数文件删除（面板不再读它）。
- v3 的状态文件不再留在 `/opt/b-ui` 顶层：`users.json` / `reality-keys.json` / `residential-proxy.json` / `admin.env` / `.resi-health-state.json` / `.relay.lock` / `port-hopping.json` / `masquerade.json` / `server_ip.txt` 归档到 `<base>/v3-backup/`（0600），迁移块留下的 `*.bak.*` 一并归档、`*.tmp` 删除；v3 的 `admin/` 与顶层的 `sing-box`（v3 的 relay 二进制）整棵删除（v4 的 sing-box 在 `<base>/bin/`）。
- `nf_conntrack` 在装机当轮 `modprobe` 加载（v3 只写 `modules-load.d`，重装后首轮 sysctl 报错）。
- v3 的 `/etc/sysctl.d/99-hysteria-bbr.conf`（D2 块可能已升成 `bbr3`）由 v4 删除，两个键并进 `99-b-ui-network.conf`（值 `bbr`）：留着它会因文件名排序在后而每次开机覆盖 v4 的期望值（Task 8 的 `LEGACY_FILES`）。`99-b-ui-memory.conf` 相反——v4 **保持同名同值接管**（≤2G 写 `vm.swappiness=10`，>2G 删除），否则 import-v3 后它是一条永久 `stray_conf`（Task 6）。
