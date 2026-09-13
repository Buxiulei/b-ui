//! 日志哨兵与预案（spec §5.7，2026-09-13 主理人：「bui 需要监听后台日志，如果出现报错，要立马处理，
//! 比如某个住宅代理 ip 连不通了，就要启用预案，立马分配新的 ip」）。
//!
//! 结构：`signature`（一行日志 → 签名，纯函数）→ `engine`（去抖 / 冷却）→ 预案（`resi` 住宅、
//! `system` 系统）→ `incidents`（事件落 `runtime.extra["incidents"]`）；`run` 是主循环。
//! 边界：只做「探测 → 借用 / 重试 / 告警」，**不增删池成员、不做切回**（切回归巡检的 `drive_slots`）。

pub mod signature;
