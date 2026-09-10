# 住宅上游类型自动探测 + 填写交互 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 面板"添加代理"能直接粘贴供应商给的任一格式（`socks5://…`、`http://…`、`host:port:user:pass`、`user:pass@host:port`），添加时自动探测上游是 SOCKS5 还是 HTTP 并按类型出站；巡检与体检按类型探测；填写框实时解析预览、校验有进行态与结果。

**Architecture:** 数据契约 = `residential-proxy.json` 条目新增 `type`（缺省 socks5）。helper 负责解析/探测/写中继（socks 或 http 出站）；resi-health 与 server.js 体检按 `type` 选 curl 代理 scheme；server.js 通过 stdin 把 URL 交给 helper；前端只做解析预览与状态渲染。

**Tech Stack:** bash + jq + curl `-K -`；sing-box `socks`/`http` 出站；Node ESM；vanilla JS。

**Spec:** `docs/superpowers/specs/2026-09-10-residential-upstream-type-ui-design.md`

## Global Constraints

- 只动 spec 列出的七个文件。不改 `version.json`（随 IPv6 计划 Task 5 统一 bump；changelog 由 Task 5 补一条）。
- helper 有 `set -euo pipefail`；resi-health 只有 `set -u`；沿用 `print_*`/`err`/`info` 与 `# v3.6.0 R10: …` 注释风格。
- 测试放 `scratchpad/resi-r10-tests/`；复用 `scratchpad/resi-tests/final-c/` 的 stub 与 `run_server.sh` 套路；sing-box 三版二进制在 `scratchpad/singbox-bins/`；不碰 `/opt/b-ui`，`systemctl`/`curl` 用 PATH 前置 stub。
- 每个 Task 一个提交：`feat(residential): …`；尾注：
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01XW7ziRAejj5BFa3UUPkLUQ
- 只提交不 push；显式 `git add <文件>`；不 amend；不 rebase。

---

### Task 1: helper + 巡检：类型解析、自动探测、按类型出站

**Files:**
- Modify: `server/residential-helper.sh`（`parse_url` ~:90、`verify` ~:117、`--add` 写状态 ~:405-430、中继写入 ~:230-350、`status` 输出、`--remove`）
- Modify: `server/resi-health.sh`（成员读取 ~:88 的 `select(.type=="socks")`、`curl_socks_cfg` ~:31）
- Modify: `docs/residential-proxy-guide.md`（spec §2.6 各小节）
- Test: `scratchpad/resi-r10-tests/test_helper_type.sh`、`test_health_type.sh`

**Interfaces:**
- `parse_url <input>` → `RESI_HOST/RESI_PORT/RESI_USER/RESI_PASS/RESI_TYPE(socks5|http|auto)`。
- `verify host port user pass type` → 成功时 `RESI_TYPE` 为实际类型、`RESI_EXIT_IP`、`RESI_ISP_INFO`；stdout 第三行输出类型（server.js 读）。
- `enable --add -`：从 stdin 读一行 URL。
- 状态条目 `{host,port,username,password,name,type,lastVerifiedIp}`；中继出站 `type: socks|http`。

- [ ] **Step 1: 写测试（预期失败）** — 四种格式解析（含引号/空白/密码含 `:`），`verify` 在 curl stub 下：socks 通→socks5；socks 失败 http 通→http（stub 按 `-K -` 读到的 `proxy =` 行判定）；都失败→非零且不写状态；显式 `http://` 不试 socks；`--add -` 从 stdin 读且 stub 记录的 argv 不含凭据；生成的中继含 `{"type":"http",…}` 出站并三版 `sing-box check` 通过；`status` 显示类型；老条目（无 type）仍写 socks 出站。resi-health：http 成员的 curl 配置 `proxy = "http://…"`，socks 成员不变；混合池在 Clash API stub 下正常切换。
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**（按 spec §2.2 / §2.3）
- [ ] **Step 4: 跑测试至通过；`bash -n` 两脚本；跑 `run_regression.sh resi-tests` 确认既有 9 套不回归**
- [ ] **Step 5: 文档小节**
- [ ] **Step 6: 提交** `feat(residential): 上游类型自动探测(SOCKS5/HTTP) + 按类型出站与巡检 + 指南补供应商事实`

### Task 2: 面板后端 + 前端：任意格式粘贴、stdin 传凭据、解析预览与校验状态

**Files:**
- Modify: `web/server.js`（`POST /api/residential/urls` ~:2278、`GET /api/residential/status` 的 urls 行、`/api/residential/health` 成员选择与 `curlJsonViaSocks` ~:530-550、~:2317）
- Modify: `web/app.js`（`addResidentialUrl`/`openAddResiUrl` ~:604-628、节点池渲染 ~:569-600、新增 `parseResiInput`）
- Modify: `web/index.html`（~:306-318 添加区）、`web/style.css`（预览区/徽标/进行态）
- Test: `scratchpad/resi-r10-tests/test_api_type.sh`（run_server 套路 + helper stub）、`test_parse_input.mjs`（vm 单测）、`dom_smoke.mjs`（影子 DOM）

**Interfaces:**
- `POST /api/residential/urls {url}` → `{success, exitIp, ispInfo, type}`；helper 经 `--add -` + `spawnSync(..., {input})`，timeout 45000。
- status 行 `{name, host, port, type, displayUrl…}`；health 成员含 `type`。
- 前端 `parseResiInput(text)` → `{host, port, username, password, scheme}` 或 `{error}`。

- [ ] **Step 1: 写测试（预期失败）** — POST 四种格式均 200 且 `type` 回传（helper 用 stub 输出三行）；spawn 的 argv 不含 URL（stub 记录 `$@`）、stdin 收到原文；health 对 `type=http` 成员生成 `proxy = "http://…"`；status 行带 type。`parseResiInput` 覆盖四种格式、引号/空白、缺端口、端口非数字、认不出格式；DOM 影子：输入即预览（打码密码、类型文案）、点击后按钮禁用与进行态文案、成功 toast + 列表徽标、失败错误框原文。
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**（按 spec §2.4 / §2.5）
- [ ] **Step 4: 跑测试至通过；`node --check` 两文件；跑 `run_regression.sh resi-tests` 与 `scratchpad/final-review-web/compare_generators.py` 流程确认订阅生成不受影响**
- [ ] **Step 5: 提交** `feat(web): 住宅代理添加框支持任意格式粘贴 + 实时解析预览 + 校验进行态 + 类型徽标；凭据经 stdin 交 helper`
