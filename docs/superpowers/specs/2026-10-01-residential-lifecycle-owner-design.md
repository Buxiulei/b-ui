# Rust BUI 住宅入站生命周期所有权

用户要求从架构解决控制面的断联风险；BUI 继续使用 Rust，只使用上游原版网络内核，sing-box 本次只针对 1.14.2。上一轮完成的架构报告是本设计的依据。本次交付住宅入站的完整纵向流程；不实施 relay generation bank，不修改 Mac、网络路由规则、上游源代码或日志级别。

## 已确认的问题与边界

daemon 的 reconcile 已经串行，但住宅入站的 API、watchdog、证书同步和离线 CLI 仍独立改变服务。restart_keys 与证书指纹提前确认，广播事件不是激活屏障。GET /proxies 的解析失败被降成空表；缺门与 PUT 后未生效可能被重放视为成功。

这些证据证明控制面存在缺陷，不证明 Mac 所有 QUIC error code 0 都由 BUI 引发。门位正确只证明住宅入站授权控制面收敛，不代表住宅供应商、客户端或所有业务请求健康。原版内核的协议行为仍由上游负责。

## 不变量

- `hysteria-residential` 的配置发布、start/restart/reload/stop、证书激活和恢复由同一个住宅专用 Rust owner 串行管理。其范围不是整个网络栈，也不把原生 HY2 鉴权处理锁在服务操作后面。
- sing-box 是住宅与 relay 的共享二进制；资源 owner 内发布候选后，relay 保留既有 apply 的校验与单次激活语义，回滚显式处理两个消费者，不能用旧 binary 校验新配置或漏重启。下载准备仍在资源锁外，不引入 relay generation bank。
- relay 的既有手动服务动作与 watchdog restart 也经过共享 binary 资源 guard；保留其原归因/通知语义，不套住宅门位屏障，不新增 relay generation 行为。
- 住宅监听端口、跳跃段和兼容开关组成最小已发布拓扑绑定，分别于期望 State。住宅 owner 在严格持久 prepared 记录后发布本次目标绑定；apply、ExecStartPre 和 watchdog 三处 BUI nft 表写者使用同一绑定投影与既有 renderer。恢复旧候选时先恢复旧绑定，再恢复服务与规则并读回验证；后续 watchdog 不得用新期望端口覆盖旧运行拓扑。仅跳跃段/兼容开关变化也属于完整发布。规则正文及一致时的无写入行为保持，prestart 子步骤免 lease 重入。
- startup 在住宅事务前发布当前鉴权 snapshot 并实际 bind 确认原生 HY2 独立 HTTP listener 可用，再执行较长的下载/激活；只 spawn 未就绪任务不足以保证服务可用。
- 机器上只有一个控制写者。daemon 持有进程间独占 lease；在线 CLI 通过 UDS 提交，无法连接 UDS 不能证明离线。离线写者（包括独立 import-v3 写受管 state 的入口）先取得同一 lease，不能与 daemon 并行写配置或 runtime。
- `harden-ssh` 保留既有加固行为，但在线经 UDS 提交给 daemon，离线在读取写入依据前取得控制 lease。SSH-only apply 不执行住宅网络清理；daemon reconcile 与 CLI 不能同时发布相同 SSH 配置文件。
- 住宅 ExecStartPre 的 `bui nft apply` 不取得控制写者 lease，防止 owner→systemctl→prestart→owner 死锁。离线安装/对账的 daemon handoff 在住宅流程结束并释放 lease 后进行。
- 请求取消不会把已开始的阻塞写操作遗留在 owner 外面。服务操作由受管事务完成，等待者取消不释放正在使用的资源锁。
- 无法观测监听状态、接口就绪、进程身份或读回门位属于 Unknown/失败，不作为成功或单独重启的依据。
- active 确认时住宅 UDP listener 必须已知且覆盖候选配置的预期端口；缺失、错误端口或 Unknown 均不确认候选 keys。
- RealHost 监听采样必须传播必要 `/proc/net` 表读取错误，不能伪造成功空集合；协议 family 不存在与读取故障分别处理并明确记录判据。
- 新的住宅 restart_keys 只能在完整激活后确认。证书指纹仅在相关运行消费者激活成功后确认；失败保留重试依据，不能提前标记 UpToDate。
- 激活失败不声称新配置生效。回滚后同样需要验证旧配置对应的门位；回滚失败可见地降级。未完成事务在下一次启动进入恢复/重新验证，不盲信旧 active receipt。

## 住宅 owner 的流程

准备与下载可以并行于正常业务，候选文件先用将要运行的原版内核校验。下载在资源提交锁外；实际二进制替换、文件发布与服务激活在该资源 owner 下完成。普通 reconcile 不因周期检查无差异而重启。

首次安装已知缺少候选证书、住宅服务尚未部署或启动时，返回明确的 `awaiting_certificate`/pending；允许必需的 Caddy 与 bootstrap/handoff 前置步骤继续，证书到达后再校验并激活。此阶段不发布未经校验的住宅 live 配置、不确认住宅 keys 或 active；普通候选校验错误与不明证书读取故障不属于该例外，共享 binary 失败仍须保持 relay 消费者一致性。

流程为 `prepare → durable prepared record → publish → activate → observe instance → restore gates → read back → active record/keys`。记录包含操作 ID、来源、候选配置摘要、观察到的进程实例、阶段及明确失败原因，不包含代理密码或私钥。操作记录必须严格原子持久化，不能以 runtime 的 best-effort 写盘冒充持久提交。

已发布绑定仅保存住宅三项拓扑及配置 SHA，不保存代理密码或私钥，不回滚当前授权 State。prepared 记录保存 previous/target，作为绑定发布的写前依据。建立新 prepared 之前必须优先处理既有未完成记录：对比 live 配置 SHA 与完整三字段绑定的 previous/target 组合，沿原操作恢复或继续并严格收尾；foreign/missing/unreadable 组合不能覆盖旧记录。绑定、配置和表的发布/恢复必须覆盖每个中断窗口，未验证的组合不能确认 active。没有绑定的初次 adoption 仅在 live 配置等于当前 renderer 且实际 BUI 表匹配时建立；未知或损坏绑定不可用期望态替代。候选校验被 hold 或首次待证书时，apply 不能提前按新期望态改变旧运行表。恢复后的监听判据、预启动和 watchdog 均消费已恢复绑定，直到下一完整新候选提交。

普通 adoption 必须绑定当前期望配置；明确恢复上一候选时使用已保存候选的配置、端口与发布证据，并仍按最新授权读回门位。恢复记录诚实区分 rejected-new 与 recovered-old，不能假称旧配置等于新期望态，也不能确认新 keys。恢复不以失败服务已 active 为前提；新发布后的各验证阶段失败均进入恢复。保存的维护/停止意图优先于恢复重启，只有明确手动 start/restart 解除维护。

既有同步 apply 的住宅操作改为可延续的待激活结果，候选 keys 与回滚点交给 async owner；其他服务的既有 apply 语义保持。启动直接等待该屏障，不依赖订阅时序。住宅通知事件只作为观察信号，不承担正确性。manual stop 是明确维护状态，不让 watchdog 立即启动；明确 start/restart 才解除维护状态。

## 门位屏障

候选配置给出完整 gate manifest（selector tag 及其允许成员）。stock 1.14.2 的 `/version` 必须得到正确产品版本；`/proxies` 必须是可解析对象，gate 必须是 Selector，成员包括 deny 与配置中允许的目标。不得把无效响应转换为空表。

接口可达之后才进入有限时长的门位写入临界区。所有 gate PUT，包括普通收敛、用户操作和 kick，都与激活屏障序列化。每次使用最新的期望态、时间和待入账流量计算授权投影；禁用、到期、配额耗尽、轮换、删除与 pending→usage 转移不能被旧的 restore_to 覆盖。使用短授权提交 fence 或版本复核，使 receipt 的授权投影具有明确线性化点；不持鉴权 HTTP 的 snapshot 锁等待 I/O。

关闭应拒绝的门优先于开放。PUT 返回成功后再次 GET；完整候选门集合必须全覆盖，值与最新授权相符，多余的旧门必须 deny。读取失败、缺门、错成员、PUT 假成功、授权变化或实例变化均不发 ready receipt。整体 deadline 覆盖等待锁、API 调用及最终读回，而不是只限制两轮之间的 sleep。

成功屏障返回仍持有 gate 写者锁和短期授权发布 fence 的 `GateActivationPermit`；直到 owner 严格持久化 active 记录才释放。Store 的 writer guard 必须跟随实际写盘与 cache 发布，等待者取消不能释放它。Store 的状态发布 fence 不锁原生鉴权 snapshot；门位阶段在整体 deadline 内完成。流量 pending→usage 转移也须在同一 gate 锁内完成，避免少算配额。普通业务读与原生鉴权仍可并发；门位提交期间管理状态写操作会等待这一有限临界区。时间不受该 fence 冻结：final GET 后与 active 提交前重新检验到期授权。

Clash API 无法证明 loaded config SHA。记录诚实区分 requested_config_sha、受管启动来源与稳定的 systemd InvocationID/MainPID 前后观测，不声称内核提供了配置证明。

## 验收

真实 Rust 路径测试覆盖成功时的顺序与持久确认，错误响应/缺门/PUT 不生效无确认，失败回滚及恢复，两个来源同时激活与等待者取消，证书失败仍重试，Unknown listener 不触发重启，显式维护 stop，CLI/daemon 独占 handoff 与 prestart 免重入。原版 sing-box 1.14.2 的隔离 fixture 验证实际 version、完整 inventory、门位写入及读回；记录原版二进制 SHA。全 workspace 测试、fmt 与 clippy 通过后交付 PR，版本更新为 4.1.5。

生产部署必须以该完整验证结果为依据。任何尚未验证的条件应明确列出，不能用隐藏错误、延长空闲 timeout 或客户端规则替代。
