# 草稿：住宅自动黑名单实施计划（未评审、未合并）

状态：**草稿，不可直接执行**。2026-09-11 由 4 个并行 agent 分别写成，交叉一致性评审（第 5 个 agent）**未跑完**（会话被 API 限流中断）。

- `header.md` — 计划头（Goal / Architecture / Global Constraints）
- `section-A.md` — Task 1-4：新增 `server/resi-blacklist.sh` 与 bash 测试基座
- `section-B.md` — Task 5-7：`residential-helper.sh` 规则注入与 `blacklist-apply`、`resi-health.sh` 钩子
- `section-C.md` — Task 8/11/12：`update.sh` D10 迁移、发版、生产验证
- `section-D.md` — Task 9-10：面板 API 与 UI

对应设计文档（已定稿已提交）：`docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`

**合并前必须做**：跨段一致性评审（函数名/JSON 键/端点/DOM id/测试 helper 是否对齐）、spec 覆盖度核对、可执行顺序核对。若 v4 架构重构推进，本计划的 `update.sh` D10 迁移块（section-C Task 8）会作废，需按新控制面重写。
