# 订阅链接的随机 token 与旧链接宽限期（v4.0.0）

- 日期：2026-09-14
- 状态：已批准（裁决：四个免鉴权端点的路径末段换成每用户一个随机 token；旧的「用户名链接」只在全局宽限期内还认，全新装机不给宽限期）
- 契约摘要在架构 spec：`docs/superpowers/specs/2026-09-11-v4-architecture-design.md` §4.5（本文是它的设计原文，两处不一致时以本文为准，请同步）
- 文件：新增 `crates/bui-schema/src/sub.rs`；改 `crates/bui-schema/src/{model.rs,v3.rs}`、`crates/bui/src/{redact.rs,api/mod.rs}`、`crates/bui/src/modules/panel/{api_public.rs,api_admin.rs,users.rs,xray.rs}`、`crates/bui/src/modules/core_files.rs`、`crates/bui/src/commands/{install.rs,status.rs,config.rs}`、`web/{app.js,index.html}`、`scripts/m1-acceptance.sh`、`scripts/ops/upgrade-drill.sh`、`README.md`、`CLAUDE.md`、`docs/HANDOVER-bui-c.md`

## 1. 背景与目标

四个免鉴权端点（`/api/sub`、`/api/subscription`、`/api/clash`、`/api/nodes`）的响应体里有 **hy2 明文密码与 vless uuid**——拿到链接就等于拿到这个用户的全部节点凭据，所以**路径末段本身就是凭据**。v3 到 v4-rc 的末段是用户名，于是「域名 + 用户名」就是订阅凭据，而这两半在本项目里都不是秘密：

- 仓库是**公开**的，git 历史里有真实域名与真实用户名（v3 的脚本、面板、文档、样本都写过），重写历史不在选项里；
- 就算把域名从历史里抹干净也补不回来：**证书透明日志**本来就公开每一张证书的所有子域，Caddy 每次签发都会把面板域名登记进去。

域名那一半不可挽回，所以只能换掉另一半：末段改成每用户一个**不可猜、可轮换**的随机 token。

目标：① 知道域名（甚至知道用户名）也猜不到任何人的订阅链接；② 链接泄露后有一个动作能把它连同凭据一起作废；③ 升级上来的存量用户不被当场掐断；④ 链接末段不进任何日志。

**非目标**：不做按用户鉴权的订阅（`/api/me/*` 仍是 501 的桩，用户域是 v4 之后的事）；不重写 git 历史；不改 `portal_auth.tokens`（面板登录 token 的空壳，与订阅无关）；不动 `keywords.rs` 里的第三方域名（住宅分流必需）。

## 2. 决策摘要

| 议题 | 决定 |
|---|---|
| token 形状 | 16 字节 CSPRNG → **32 位小写十六进制**（128 bit）。只产小写，大写不认——同一个 token 不许有两种写法 |
| 作用域 | 每**用户**一个，四个端点共用同一个 token（不按端点分，运维与面板都只用记一个值） |
| 旧用户名链接 | 只在**全局**宽限期 `system.legacy_sub_until` 内还认，且该用户没被轮换过；全新装机不设宽限期 ⇒ 从来不可用 |
| 宽限期长度 | 7 天（`sub::LEGACY_SUB_GRACE_DAYS`），运维可 `bui set legacy-sub <RFC3339>` 改期、`off` 立刻收口 |
| 查不到时的回应 | 一律沿用现有的 **404 `{"error":"User not found"}`**；不用 410、不用任何能区分「没这个用户」与「链接过期」的回应 |
| 轮换 | `POST /api/users/{name}/rotate`：token + hy2 密码 + vless uuid **一起**换，停用该用户的用户名链接，并踢掉他已建立的 hy2 会话 |
| 日志 | 末段一个字都不进日志：守护进程侧 `redact::sub_path` + 自造的 TraceLayer span，Caddy 侧 `log_skip` + 两个 logger 的字段掩码 |

## 3. 模型

`bui-schema` 的三个字段，一律 `#[serde(default, skip_serializing_if = …)]`——缺省不落盘，老 `state.json` 读得进来、没补齐的机器也不会多出空字段：

| 字段 | 位置 | 含义 |
|---|---|---|
| `sub_token: Option<String>` | `User` | 这个用户的订阅 token。`None` 只出现在还没补齐的老 `state.json` 上（见 §5） |
| `legacy_sub_disabled: bool` | `User` | 这个用户的「用户名链接」是否已停用（轮换时置 `true`，此后即便宽限期还在也不认） |
| `legacy_sub_until: Option<String>` | `SystemSettings` | 全局宽限期截止时刻（RFC3339）。`None` = 一概不认用户名链接 |

`crates/bui-schema/src/sub.rs` 是唯一知道 token 形状的地方：`new_sub_token()`（生成）、`is_sub_token()`（严格判据：长度 32 且只含 `0-9a-f`）、`legacy_sub_deadline(now)`（`now + 7 天`，秒级 RFC3339）、`sub_urls(domain, token)`（四条地址，装机摘要与面板共用，免得两边各拼一遍）、`LEGACY_SUB_GRACE_DAYS`。

token 不是密码，不做 hash：它必须能原样发给用户，也必须能在响应体里给出（面板要显示可复制的链接）。它的保密性靠 128 bit 随机 + 文件 600 + 不进日志。

## 4. 四个端点的解析：三步与 404 口径

四个 handler 共用 `panel::api_public::resolve(state, seg, now)`，顺序固定：

1. **末段是 token 形状**（`sub::is_sub_token`）⇒ 只按 `sub_token` 找用户，比较走**常量时间**（`subtle::ConstantTimeEq`，与 `auth_hook::decide` 比 hy2 密码同口径）。形状对但对不上任何人 ⇒ 直接失败，**不**回退去试用户名（否则「32 位十六进制的用户名」会成为一条绕路）；
2. **否则**（长得像用户名）⇒ 只有 `legacy_sub_until` 存在、解析成功、且 `now < 截止` 时，才按 `username` **精确**匹配，并且要求该用户 `legacy_sub_disabled == false`；
3. 其余一律查不到。

三条路都收敛到同一个 **404 `{"error":"User not found"}`**：

- 不给 410 Gone、不给「链接已过期，请到面板取新链接」之类的提示——那等于白送一个免鉴权的用户名探测器（谁都能拿它枚举「这台机器上有没有 alice」）；
- 时刻**解析不出来就算过期**（fail-closed，与 `auth_hook` 处理 `expires_at` 同口径）：判不出「当前时间早于它」就一律不认；
- 响应体、状态码、耗时都不许因为「用户存在但链接过期」而不同。

下载文件名（`Content-Disposition`）按**查到的** `u.username` 生成（`safe_filename`），不是路径末段——否则 token 链接下载到的文件名就是那个 token，凭据会被存进用户的下载目录与浏览器历史。

节点标签、`/api/nodes` 载荷里的 `user` 同理，一律取期望态里的用户名。

## 5. 宽限期的三种来源

| 场景 | token | `legacy_sub_until` |
|---|---|---|
| 全新装机 | 建首用户时就有（`panel::users::new_user` → `install::first_user`） | **不设**（`None`）⇒ 用户名链接从来不可用 |
| `bui install --import-v3` | 每个导入的用户现生成（`v3::user_from_v3`） | 导入时刻 + 7 天（`v3::import`）——这些人手里拿的正是 v3 的用户名链接 |
| 老 v4 安装升级上来 | 守护进程启动时补齐（`panel::users::backfill_sub_tokens`，形状照 `residential::slots::migrate_on_start`：幂等、零变更不写盘，所以每次启动无条件调） | **补出过 token 且这一位还是 `None`** 时设成「启动时刻 + 7 天」；已经设过（v3 导入或运维自己设的）不覆盖 |

三条建用户路径（面板 `POST /api/users`、装机首用户、v3 导入）都自带 token，所以 `backfill` 只服务存量；补齐的日志**只写个数与宽限期天数，不写 token**。

运维收口与改期：`bui set legacy-sub off`（置 `None`，立刻停用全部用户名链接）/ `bui set legacy-sub <RFC3339>`（改期）。取值校验只有一处（`commands::config::parse_legacy_sub`），CLI 与 `POST /api/system/legacy-sub` 共用；socket 通就交给守护进程写，没跑才自己写盘（与 `bui set hy2-auth` 同口径）。

现在到底认不认，两处显示、**同一处判定**（`commands::status::legacy_sub` 的三态 `Off` / `Active` / `Expired`）：

- `bui status`：`旧订阅链接  已停用（只认随机 token 链接）` / `用户名链接还剩 7d 0h 到期（<时刻>）` / `已过期（<时刻>），只认随机 token 链接`；
- `bui install` 收尾摘要：没有宽限期一个字不提（全新装机提它就是给人死链）；宽限期内报「还剩多久」+ `bui set legacy-sub off`；已过期只报「已过期」。这一屏在**已装机的对账**路径上也会跑，所以不许无条件写「还认到 <时刻>」。

## 6. 轮换

`POST /api/users/{username}/rotate`（管理员鉴权内，请求体是空对象，面板「轮换」按钮）。四件事必须一起做，少一件就不叫轮换：

1. **换 token**（`sub_token = new_sub_token()`）——旧链接的路径段作废；
2. **换 hy2 密码与 vless uuid**（`credentials`）——泄露的链接里同时有这两样，只换 token 等于没换；
3. **停用这个用户的用户名链接**（`legacy_sub_disabled = true`）——否则全局宽限期没到时，旧的用户名链接照样能取到**新**凭据；
4. **踢掉他已经建立的 hy2 会话**：两条鉴权路径都只在**握手时**过 `auth_hook::decide`，xray 的 RemoveUser/AddUser 也只影响新握手，所以不踢的话拿着旧凭据的那一方照旧有流量，直到连接自己断。先发 `StateChanged("users")`（两条鉴权路径据此刷新、鉴权快照重写）再踢，被踢的客户端拿旧密码重连时已经会被拒；踢是 best-effort，失败只记 warn（新凭据已经生效，踢不动只是旧会话多活一会儿）。

回包直接给出新的 `subToken` / `password` / `uuid`（与 `create_user` 同口径：这条路由在 `require_admin` 里面，面板本来就在显示每个用户的密码与 uuid）。

两个边界：

- **uuid 换了必须让 xray 当场生效**：`sync_users` 走「先 RemoveUser 再 AddUser」，且不把 xray 的 `already exists` 当成已达目标——`render::xray::structural_hash` 剥掉了 clients，对账**不会**因为换 uuid 重启 xray，靠重启兜底等于轮换对 Reality 用户空转（实现细节见 `panel::xray`）；
- **Reality 那条已建立的连接没有踢的手段**（xray 没有 kick），只能等它自己断。

URL 里的用户名**只用来查**，不跑 `validate_username`（与 `delete_user` 同口径）：v3 导入原样照抄 v3 的用户名，存量里可能有不合今天规则的名字——卡在校验上会让这种用户能改、能删、能取订阅却永远轮换不了，而他恰恰是最该轮换的那个。

## 7. 日志面：末段一个字都不许进日志

| 位置 | 做法 |
|---|---|
| `bui` 自己的日志 | `redact::sub_path(s)`：把 `/api/{sub,subscription,clash,nodes}/<段>` 的 `<段>` 换成 `***`，一行里出现多条全换，查询串与后面的路径保留；前缀匹配对 ASCII 大小写不敏感（Caddy 记的是客户端原样的大小写）。`redact::line` 先过它再做原有的 userinfo 脱敏 |
| HTTP 请求 span | `tower_http` 的 `DefaultMakeSpan` 会把整条 URI 记进 span，`--log debug` 一开就全进 journald ⇒ `api::router` 自造 span（`debug_span!("request", method, uri = %redact::sub_path(...), version)`）。INFO 级下这个字段表达式受 callsite interest 保护，不会被求值 |
| 日志哨兵 | 事件的 `sample` 是**先脱敏后截断**的日志原文（`sentinel::signature` 在**匹配之后**才脱敏，所以叠 `sub_path` 不改探测语义），Caddy 漏出来的行在落 `runtime.json` 前也被它挡住 |
| Caddy 访问日志 | 站点块里 `@sub path /api/sub/* /api/subscription/* /api/clash/* /api/nodes/*` + `log_skip @sub`：这四条路径的访问日志整条不记，其余请求照旧 |
| Caddy 的另外两条路 | `log_skip` 只挡站点路由树里的**访问**日志，挡不住：(a) `reverse_proxy` 连不上上游时的**错误**日志（`http.log.error.*`）落到 **default** logger——面板的上游就是本机 `:admin_port`，`b-ui` 每次重启/升级/watchdog 拉起的窗口里客户端拉订阅都会 502，一条一个 token；(b) `:80` 的 HTTP→HTTPS 跳转服务器不走站点路由树，它的访问日志与 308 的 `Location` 头各带一份末段。所以 **default 与站点两个 logger 都挂 `format filter`**，`request>uri` 与 `resp_headers>Location` 两个字段都过同一条 `regexp`（`(?i)(/api/(?:sub\|subscription\|clash\|nodes)/)[^/?]+` → `${1}***`）。default logger 的 `wrap` 必须留 **json**：哨兵按 JSON 解 Caddy 的证书失败行，换成 console 那条告警会静默失效 |
| `backfill` / 轮换 | 只写个数、用户名与动作，不写 token 与密码 |

面板与客户端侧：`PanelUser` 投影带 `subToken`，前端据此拼四条链接（页面上本来就显示密码与 uuid）；`bui-c` 的 `error::redact_url` 早有同类实现，错误文案里的 URL 不带末段。

## 8. 运维面与验收

- `scripts/m1-acceptance.sh` step7：先从 `state.json` 取该用户的 `sub_token` 再拼 `/api/sub/<token>`，URL 经 `curl -K -` 的 **stdin** 传（末段是凭据，`ps` 会泄露 argv）；取不到 token 就 SKIP 而不是报红。
- `scripts/ops/upgrade-drill.sh`：三个相位的订阅指纹同样按 token 取（用户名链接在任何没有活动宽限期的机器上都是 404，拿它取指纹会让三个相位记下同一个空串的 sha ⇒ 「订阅无漂移」假绿）；取不到一律记 `missing` 而不是空串的 sha，基线相位一有 `missing` 就判失败。
- 文档：`README.md`「订阅链接与 token」、`CLAUDE.md` 的 Subscriptions 小节、`docs/HANDOVER-bui-c.md` §6（客户端侧的口径与「轮换后必须人工重新导入」）。

## 9. 已知限制

- **`--log trace` 不要用来排障**：`logging::init` 的 `EnvFilter` 是全局级别，`trace` 会一并打开 hyper / reqwest / rustls / tonic 等第三方 crate 的日志，那些行不经 `redact`，完整 URI（含末段）会进 journald。排障用 `--log debug`（自造 span 已脱敏）。
- **`state.backups/` 里留着轮换前的旧凭据**：每次写 `state.json` 前会把旧版复制进去（保留最近 10 份，600）。轮换只保证**新连接**认新凭据，不保证磁盘上不再有旧值；真要清干净得等备份滚完或手动删。
- **`bui-c` 的 `profiles.json` 会存下 token**：`panel.username` 存的就是那个末段（token 化之后即 token），profile 里的节点本来也带 hy2 密码与 uuid——那份文件（0600）本身就是凭据文件。轮换会让已部署的 `bui-c` 当场失效且**无法自愈**（`bui-c.timer` 每分钟 `check` 失败重启，更新源 `/api/nodes/<旧 token>` 也 404），必须人工重新导入一次；轮换前先确认这个用户有没有 Linux 客户端。
- **git 历史里的真实域名与用户名补不回来**：token 化只让「知道域名 + 用户名」不再等于知道链接；历史本身不重写。
- **宽限期是全局的、不是按用户的**：`legacy_sub_until` 一到，所有还在用用户名链接的人同时失效（单个用户可以用轮换提前停用，反过来不行——给某人单独续期得改期望态里的全局时刻）。
