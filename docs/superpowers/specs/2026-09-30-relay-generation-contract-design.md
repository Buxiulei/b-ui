# Relay 代际配置与发布协议

## 目标与范围

保留 Rust 控制面和固定 sing-box 1.14.2，将住宅出口配置的更新从稳定入口中分离。本阶段交付可执行的配置编译器、严格的发布状态协议，以及真实内核的离线流量验收。现有生产配置生成、服务管理与健康切换仍使用原入口；接入生产必须满足下文的协调与排空条件。

此前将 slot 入口、出口池、住宅策略和供应商凭据放在同一个 relay 配置中。保存出口选择已能避免结构无变化的重启，但更新策略或凭据仍会改变整个 relay。新的边界让固定 UUID 集合中的凭据、自动直连和端口策略只改变候选后端。

## 数据面拓扑

```text
HY2 / Xray
  → slot SOCKS 2080+i
  → 稳定 front：公共直连、私网保护、slot 池选择
  → resi-N 逻辑 selector
  → bank+UUID SOCKS 出站
  → A:2180+i / B:2280+i
  → generation+UUID 策略入口
  → 自动直连、UDP DNS、端口策略、私网保护
  → generation+UUID 供应商出站
```

front 使用 API 9091；现有 HY2 gate 保留 9092；A/B 后端使用 9093/9094。每个 bank 最多八个住宅出口。逻辑 selector、slot 池和健康检查名称保持稳定；bank 成员包含 UUID，避免索引复用导致身份错配。所有 selector 保留旧连接。

后端配置没有出口池、selector 或选择缓存。generation 为从 1 开始的单调编号，禁止复用；原始供应商日志标签为 `resi-egress-g<generation>-<canonical UUID>`。解析与状态绑定分开：唯一文本解析器返回代际编号和 UUID，账本判断该证据是否属于已观测发布的当前成员。Ready 探测和 Draining 旧凭据日志不得污染当前学习。

## 配置契约

front 编译器保留现有公共规则、slot sniff、DNS 与混合 UDP 语义。后端编译器保留住宅策略的顺序：内部 UDP DNS 绕过端口策略、必要的 IP sniff、自动直连、UDP IPv4 解析、解析后私网保护、端口保护、最终供应商路由。

相同有序 UUID 集合的凭据、自动策略与允许端口更新不改变 front 结构。出口增删、顺序变化、启停、模式、slot、绑定、split、公共规则或服务器地址变化属于维护切换。front hash 只忽略全局池默认选择和 `resi-N` 的 bank 默认选择，不能忽略 slot 默认选择或成员身份。

空池、停用组生成有效无引用配置；超过八个成员返回明确错误。仅支持 1.14.2，不增加跨版本分支。

## 严格发布协议

账本绑定配置 SHA256、内核 SHA256、front 结构 SHA256 和有序 UUID。A/B 是不可变资源租约；正在使用或排空的 bank 不能覆盖。资源满时返回 NeedDrain，由未来驱动器合并最新期望配置并等待，不强制终止旧连接。

状态依次为 Preparing、Ready、Publishing、Active/Draining、Stopped、Free。切换每个成员前持久化意图，再执行 API 请求并读回实际选择。1.14.2 没有对不同后端地址的全体成员原子切换；协议明确保存部分发布和未知观测，不能把请求成功等同于完成发布。

Stopped 租约必须完成释放后才能准备下一候选，以保持停止证明与未知发布观测的关系一致。编译前可读取类型化的 reservation identity；计算包含代号和 bank 的真实配置摘要后，再在预留时复核该身份，拒绝过期编译结果。同一入口实例的同批次观测不能反转已知成员；Unknown 可以补全。观测批次编号由驱动器管理，不是内核已提供的拨号屏障。

恢复必须使用严格反序列化和关系验证。损坏账本、非法 phase、错配 UUID、摘要或租约返回错误，不能重置为空账本。此阶段只定义纯状态转换；不使用 `Runtime.extra` 的尽力缓存语义承担关键事务。

旧实例只有在实际 selector 和磁盘启动默认值均不再引用它、精确代际的 front 在途拨号屏障已得到证明、TCP/UDP/握手计数均为零时，才可进入停止流程。停止成功且确认不活跃后，才能释放 bank。任何未知值都阻止回收。

## 真实内核验收

使用实际编译结果运行 front、A、B 三个 sing-box 1.14.2 进程。fixture 可重定位端口、API 与离线供应商/DNS端点，不能替换待验收的策略路由。

- 至少两个住宅 UUID 和两个 slot；候选准备、部分切换、全部切换后，50 个旧 TCP 和 50 个旧 UDP association 继续走 A，新连接跟随实际选择。
- 验证同一 UDP association 多目标、TCP 半关闭后的延迟响应，以及 front/backend API 的 TCP/UDP 归属。
- 关闭旧流、停止 A 后，新 B 流继续成功。
- 只发送 SOCKS greeting 首字节的连接不会出现在连接 API 中。该反例必须存在，不能把 API 为零宣称为完整排空证明。
- 状态协议拒绝未知观测、缺失拨号屏障、仍被启动默认值引用、未成功停止等回收条件。

## 接入生产前的条件

后续驱动器必须统一 reconcile、健康切换、手动选择和 watchdog 的生命周期所有权，恢复先于 front 渲染/启动；CLI 与 daemon 共用进程锁。关键账本使用 0600、文件 fsync、原子 rename 和目录 fsync。先保存有效的启动默认值再回收，实际探测 bank 端口占用，并把 bank 配置、单元、账本加入漂移修复、状态、卸载和 watchdog 的管理范围。

最关键的未完成条件是可靠的 front 在途拨号屏障。真实 1.14.2 API 不覆盖尚未完成 SOCKS 握手的连接；连续多次零计数不能替代这个证明。屏障未知时必须保留旧实例。这一阶段的完成不代表生产热更新或断流问题已经全部解决。

进一步的 1.14.2 源码与离线探针确认：tracker 在 dial 前登记，关闭时先从 API 移除再关闭入站；入站关闭并不取消共享 listener context 中的旧 SOCKS 拨号。暂停 A 握手、切到 B、删除 tracker 后 API 已为零，恢复 A 仍能收到旧 CONNECT。因此公开 API、连接 chains 快照或 socket 清单都不能提供上述屏障。`chains` 是登记时读取 group.Now() 的快照，不能在竞争条件下代替供应商实际归属。参见 [tracker](https://github.com/SagerNet/sing-box/blob/v1.14.2/common/trafficcontrol/tracker.go#L117) 与 [selector](https://github.com/SagerNet/sing-box/blob/v1.14.2/protocol/group/selector.go#L147)。

不改内核时，当前契约的安全策略是保持旧 bank，并将结束旧 front 实例归入维护切换。后续自动回收可考虑 Rust 稳定 UUID dispatch gate：front 只选择 UUID，gate 在同一个同步边界内完成取得 generation permit 与发布新 dispatch table；permit 覆盖后端拨号、握手、TCP 双向结束和整个 UDP association。迟到请求只能取得当前代，旧代停止接受新请求且全部 permit 释放后，才形成 admission 排空证明。这个证据必须绑定 gate 实例、dispatch epoch、generation 和 table 摘要，是新的 admission 契约，不能伪造为当前 `FrontDialBarrier`。

该 gate 会增加一个内部转发边界，并必须处理 SOCKS UDP 的返回地址和多目标 association；只转发 TCP 会让 UDP 绕过 permit。另一选择是内部 UoT v2，但需要单独验证元数据、策略和性能。此阶段不实现或默认采用任一方案；下一阶段先以真实内核验收决定完整数据面的边界，不能仅给现有 A/B 固定端口套 gate 而留下端口复用的 ABA 问题。
