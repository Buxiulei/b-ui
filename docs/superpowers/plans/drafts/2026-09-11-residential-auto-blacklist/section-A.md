## Section A — 新增 `server/resi-blacklist.sh`（Tasks 1–4）

**Scope of this section.** The four tasks below build the new server-side script `server/resi-blacklist.sh` (deployed to `/opt/b-ui/resi-blacklist.sh` by the release task in a later section) and its bash test suite under `tests/resi-blacklist/`. Nothing in this section touches `residential-helper.sh`, `resi-health.sh`, `server.js`, `version.json` or the docs — those are Sections B–E. The script talks to the helper only through the contract subcommand `residential-helper.sh blacklist-apply` (Section B); in tests that call is replaced by a stub helper that records its arguments.

**Read before starting.** Spec: `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md` (§4 state file, §5 probing rules, §6 candidates, §8.1 CLI). Style references: `server/resi-health.sh` (log rotation `trap rotate_log EXIT`, `curl_proxy_cfg`, `-K -` credentials on stdin) and `server/residential-helper.sh:44-49` (`acquire_relay_lock`, `flock -w 30`) and `:60-75` (`file_digest`, `curl_cfg_escape`). Test format reference: `docs/superpowers/plans/2026-09-10-residential-hardening.md` Task 1.

**Conventions that apply to every task in this section**

- The tests run on macOS (bash 3.2, BSD sed/awk/xargs, no `flock`, no `timeout`) and the script runs on Ubuntu/Debian/CentOS (bash 4+/5, GNU tools). Therefore the script uses **no bash-4-only syntax** (no `${var,,}` — use the `lower` helper; no associative arrays; no `mapfile`), `with_bl_lock` degrades to unlocked execution with one WARN when `flock` is missing, and `direct_check` requires `timeout` (tests put a stub `timeout` on `PATH`; on a Linux host without coreutils `timeout` the port control fails closed, i.e. the port is never blacklisted).
- Every aggregation is done in `jq` (1.5+ compatible; production has jq 1.6), not in bash arrays.
- The script is **sourceable**: `if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi` at the bottom, so tests can `. "$SCRIPT"` and call internal functions directly. `BLACKLIST_SELF` (derived from `BASH_SOURCE`) is what the parallel worker re-executes, never `$0`.
- Exit codes: 0 ok / 1 error / 2 usage. `set -uo pipefail`, deliberately no `-e` (probing is made of expected-to-fail `curl` calls).
- File layout: Task 1 creates the file with three marker comment lines `# ===== [探测] =====`, `# ===== [学习] =====`, `# ===== [候选 / 进出规则 / 并行探测] =====`; Tasks 2–4 each replace one marker line with a block that starts with that same marker line, and add lines to the `case "$cmd" in` dispatch in `main`. Line numbers cited for Modify steps are the line numbers of the file **as left by the previous task**; each anchor is also quoted verbatim.
- TSV formats: candidate line = `<probe_spec>\t<target>`; worker/result line = `<target>\t<verdict>\t<reason>\t<source>`; `verdict` at the result level is `blocked|ok|unknown|unavailable`, at the single-probe level (`classify_probe`) it is `refused|ok|unknown|unavailable`.
- Test run command: `bash tests/resi-blacklist/<name>.sh`; every script ends with `finish`, prints `PASS n / FAIL m` and exits 1 on any failure. `tests/resi-blacklist/` is **never** added to `version.json` `files[]` (it is not deployed).
- Env overrides (all read once at the top of the script and exported to workers): `BASE_DIR`, `BL_LOG`, `BL_JOURNAL_CMD`, `BL_PARALLEL` (8), `BL_PROBE_TIMEOUT` (12), `BL_RETRY_DELAY` (3 s between the two upstream attempts; tests set 0), `BL_SKIP_APPLY=1`, `BL_NOW` (fixed ISO timestamp), `BL_CANDIDATES_FILE` (overrides the builtin table; same three-column format, space- or tab-separated, `#` comments allowed).

---

### Task 1: `resi-blacklist.sh` 骨架 —— 状态文件 I/O、锁、`status` / `pin` / `forget`、测试基座

**Files:**
- Create: `server/resi-blacklist.sh`
- Create: `tests/resi-blacklist/lib.sh`
- Create: `tests/resi-blacklist/test_state.sh`

**Interfaces:**
- Consumes: env `BASE_DIR`, `BL_LOG`, `BL_SKIP_APPLY`, `BL_NOW` (contract); `${BASE_DIR}/residential-helper.sh blacklist-apply` (Section B; stubbed in tests).
- Produces (script, exact names): variables `BASE_DIR`, `BLACKLIST_STATE`, `BLACKLIST_LOCK`, `HELPER`, `RESIDENTIAL_CONFIG`, `SINGBOX_CONFIG`, `BL_LOG`, `BL_JOURNAL_CMD`, `BL_PARALLEL`, `BL_PROBE_TIMEOUT`, `BL_RETRY_DELAY`, `BL_SKIP_APPLY`, `BL_NOW`, `BL_CANDIDATES_FILE`, `BLACKLIST_SELF`, `BL_SKELETON`, `BL_NEVER_PORTS`; functions `bl_now` (prints ISO-8601 UTC or `$BL_NOW`), `log <msg>` (appends `[YYYY-mm-dd HH:MM:SS] msg` to `$BL_LOG` and echoes to stderr), `rotate_log` (`tail -300`, on EXIT), `lower <s>`, `bl_state_init` (creates the 600 skeleton when missing/empty/corrupt), `bl_read` (prints state JSON, skeleton when unusable), `bl_write` (stdin JSON → validated → `.tmp` 600 → `mv`), `bl_update '<jq filter>' [jq args…]` (read→jq→write, no lock), `with_bl_lock <fn> [args…]` (flock -w 30 on fd 8, released on return; never nest; never call the helper from inside), `valid_target <t>`, `apply_via_helper` (runs `"$HELPER" blacklist-apply` unless `BL_SKIP_APPLY=1`; returns 1 on failure), `cmd_status [--json]`, `cmd_pin <target> direct|resi|clear` (lowercases target, validates, writes `.pins`, then `apply_via_helper`), `cmd_forget <host:port>` (deletes `.upstreams[key]` and `.learned[key]`, does **not** call the helper — the helper calls `forget` while holding `.relay.lock`), `usage`, `main`.
- Produces (test lib, exact names): `PASS`, `FAIL`, `pass`, `fail`, `assert_eq <expected> <actual> <msg>`, `assert_contains <needle> <haystack> <msg>`, `assert_file_exists <path> <msg>`, `finish`, `REPO_ROOT`, `SCRIPT`, `file_mode <path>`, `make_base` (creates a temp `BASE_DIR` and exports `BASE_DIR BL_LOG BL_SKIP_APPLY=1 BL_NOW=2026-09-11T04:00:00Z BL_RETRY_DELAY=0 BL_PARALLEL=2` — call it directly, never inside `$(...)`), `write_resi_config <dir>` (two-upstream `residential-proxy.json`: `brd.superproxy.io:44445` type http, `Gate.Smartproxy.COM:7000` without type), `write_relay_config <dir>` (matching `singbox-relay.json`, `resi-1` → brd, `resi-2` → smartproxy), `write_stub_helper <dir>` (helper stub appending `$*` to `<dir>/helper.log`, exit `${STUB_HELPER_EXIT:-0}`).

- [ ] **Step 1: Create the shared test library `tests/resi-blacklist/lib.sh`**

```bash
mkdir -p tests/resi-blacklist
```

Write `tests/resi-blacklist/lib.sh` with exactly this content:


```bash
#!/usr/bin/env bash
# tests/resi-blacklist/lib.sh — 共享断言与夹具（无测试框架，纯 bash；macOS 与 Linux 都能跑）
# 用法：在测试脚本里 `. "$(dirname "$0")/lib.sh"`，末尾调用 finish。
PASS=0; FAIL=0
pass() { PASS=$((PASS + 1)); echo "  ok   - $1"; }
fail() { FAIL=$((FAIL + 1)); echo "  FAIL - $1"; [ -n "${2:-}" ] && echo "         $2"; return 0; }
assert_eq() {        # assert_eq <expected> <actual> <msg>
    if [ "$1" = "$2" ]; then pass "$3"; else fail "$3" "expected: [$1]  actual: [$2]"; fi
}
assert_contains() {  # assert_contains <needle> <haystack> <msg>
    case "$2" in *"$1"*) pass "$3" ;; *) fail "$3" "expected to contain: [$1]  actual: [$2]" ;; esac
}
assert_file_exists() { if [ -e "$1" ]; then pass "$2"; else fail "$2" "missing: $1"; fi; }
finish() { echo "PASS $PASS / FAIL $FAIL"; [ "$FAIL" -eq 0 ] || exit 1; exit 0; }

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT="$REPO_ROOT/server/resi-blacklist.sh"
# 文件权限（八进制）：macOS stat -f / Linux stat -c
file_mode() { stat -f %Lp "$1" 2>/dev/null || stat -c %a "$1" 2>/dev/null; }

# 新建临时 BASE_DIR 并导出脚本用到的环境（BL_SKIP_APPLY=1 默认不调 helper）。
# 直接调用（不要放进 $(...)，否则 export 留在子 shell 里）；调用后 $BASE_DIR 即临时目录。
make_base() {
    BASE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bl-test.XXXXXX")
    export BASE_DIR
    export BL_LOG="$BASE_DIR/bl.log"
    export BL_SKIP_APPLY=1
    export BL_NOW="2026-09-11T04:00:00Z"
    export BL_RETRY_DELAY=0
    export BL_PARALLEL=2
}
# 两个上游的 residential-proxy.json（http 上游 + 缺 type 的 socks5 上游）
write_resi_config() {
    cat > "$1/residential-proxy.json" <<'EOF'
{"enabled":true,"global":true,"domains":null,
 "urls":[{"host":"brd.superproxy.io","port":44445,"username":"brd-customer-abc-zone-isp","password":"p4ss\"w0rd","type":"http","name":"url-1"},
         {"host":"Gate.Smartproxy.COM","port":7000,"username":"sp-user","password":"sp-pass","name":"url-2"}]}
EOF
}
# 与之对应的 singbox-relay.json（只需 outbounds 顺序：resi-1 → brd，resi-2 → smartproxy）
write_relay_config() {
    cat > "$1/singbox-relay.json" <<'EOF'
{"outbounds":[
  {"type":"http","tag":"resi-1","server":"brd.superproxy.io","server_port":44445,"username":"u","password":"p"},
  {"type":"socks","tag":"resi-2","server":"Gate.Smartproxy.COM","server_port":7000,"username":"u","password":"p","version":"5"},
  {"type":"selector","tag":"resi-pool","outbounds":["resi-1","resi-2"],"default":"resi-1"},
  {"type":"direct","tag":"direct"}]}
EOF
}
# 记录调用参数的 helper 桩：$BASE_DIR/residential-helper.sh → 追加到 $BASE_DIR/helper.log
write_stub_helper() {
    cat > "$1/residential-helper.sh" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "$(dirname "$0")/helper.log"
exit "${STUB_HELPER_EXIT:-0}"
EOF
    chmod +x "$1/residential-helper.sh"
}
```

- [ ] **Step 2: Write the failing test `tests/resi-blacklist/test_state.sh`**


```bash
#!/usr/bin/env bash
# Task 1：状态文件初始化 / status / pin / forget / 用法错误
set -u
. "$(dirname "$0")/lib.sh"
make_base; BASE="$BASE_DIR"
SKELETON='{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}'

# --- 用法错误 → exit 2 ---
bash "$SCRIPT" >/dev/null 2>&1;            assert_eq 2 $? "无参数 → exit 2"
bash "$SCRIPT" bogus >/dev/null 2>&1;      assert_eq 2 $? "未知命令 → exit 2"
bash "$SCRIPT" pin >/dev/null 2>&1;        assert_eq 2 $? "pin 缺参数 → exit 2"
bash "$SCRIPT" pin a.com sideways >/dev/null 2>&1; assert_eq 2 $? "pin 未知模式 → exit 2"
bash "$SCRIPT" forget >/dev/null 2>&1;     assert_eq 2 $? "forget 缺参数 → exit 2"
bash "$SCRIPT" forget nocolon >/dev/null 2>&1; assert_eq 2 $? "forget 非 host:port → exit 2"

# --- status --json 首次调用创建 600 骨架 ---
out=$(bash "$SCRIPT" status --json 2>/dev/null)
assert_eq "$(echo "$SKELETON" | jq -c .)" "$(echo "$out" | jq -c .)" "status --json 输出空骨架"
assert_file_exists "$BASE/residential-blacklist.json" "状态文件已创建"
assert_eq 600 "$(file_mode "$BASE/residential-blacklist.json")" "状态文件权限 600"

# --- 损坏的状态文件 → 重建 ---
echo 'not json' > "$BASE/residential-blacklist.json"
out=$(bash "$SCRIPT" status --json 2>/dev/null)
assert_eq 1 "$(echo "$out" | jq '.version')" "损坏文件被重建为骨架"

# --- pin ---
bash "$SCRIPT" pin www.google.com direct >/dev/null 2>&1; assert_eq 0 $? "pin direct 退出 0"
assert_eq direct "$(jq -r '.pins["www.google.com"]' "$BASE/residential-blacklist.json")" "pin direct 写入"
bash "$SCRIPT" pin WWW.Bing.COM resi >/dev/null 2>&1
assert_eq resi "$(jq -r '.pins["www.bing.com"]' "$BASE/residential-blacklist.json")" "pin 目标转小写"
bash "$SCRIPT" pin port:5228 direct >/dev/null 2>&1
assert_eq direct "$(jq -r '.pins["port:5228"]' "$BASE/residential-blacklist.json")" "pin port:N"
bash "$SCRIPT" pin 'bad target!' direct >/dev/null 2>&1; assert_eq 2 $? "pin 非法目标 → exit 2"
bash "$SCRIPT" pin port:abc direct >/dev/null 2>&1;      assert_eq 2 $? "pin port:非数字 → exit 2"
bash "$SCRIPT" pin nodot direct >/dev/null 2>&1;         assert_eq 2 $? "pin 无点主机名 → exit 2"
bash "$SCRIPT" pin www.google.com clear >/dev/null 2>&1
assert_eq false "$(jq '.pins | has("www.google.com")' "$BASE/residential-blacklist.json")" "pin clear 删除"
assert_eq 2 "$(jq '.pins | length' "$BASE/residential-blacklist.json")" "其余钉住保留"
assert_eq 600 "$(file_mode "$BASE/residential-blacklist.json")" "改写后仍 600"

# --- pin 调 helper blacklist-apply（BL_SKIP_APPLY=0 时）---
write_stub_helper "$BASE"
BL_SKIP_APPLY=0 bash "$SCRIPT" pin example.com direct >/dev/null 2>&1; assert_eq 0 $? "pin + helper 退出 0"
assert_eq "blacklist-apply" "$(cat "$BASE/helper.log")" "pin 调了 helper blacklist-apply 一次"
BL_SKIP_APPLY=0 STUB_HELPER_EXIT=1 bash "$SCRIPT" pin example.com clear >/dev/null 2>&1; assert_eq 1 $? "helper 失败 → exit 1"
rm -f "$BASE/helper.log"
bash "$SCRIPT" pin example.com direct >/dev/null 2>&1
assert_eq 0 "$(cat "$BASE/helper.log" 2>/dev/null | wc -l | tr -d ' ')" "BL_SKIP_APPLY=1 不调 helper"

# --- forget ---
jq '.upstreams["brd.superproxy.io:44445"] = {checkedAt:"2026-09-10T20:00:00Z", entries:{"www.google.com":{kind:"domain"}}}
  | .upstreams["gate.smartproxy.com:7000"] = {checkedAt:null, entries:{}}
  | .learned["brd.superproxy.io:44445"] = {"gateway.icloud.com":{count:9}}
  | .learned["gate.smartproxy.com:7000"] = {"x.com":{count:3}}' \
  "$BASE/residential-blacklist.json" > "$BASE/s.tmp" && mv "$BASE/s.tmp" "$BASE/residential-blacklist.json"
bash "$SCRIPT" forget BRD.superproxy.io:44445 >/dev/null 2>&1; assert_eq 0 $? "forget 退出 0"
assert_eq false "$(jq '.upstreams | has("brd.superproxy.io:44445")' "$BASE/residential-blacklist.json")" "forget 删除 upstreams"
assert_eq false "$(jq '.learned | has("brd.superproxy.io:44445")' "$BASE/residential-blacklist.json")" "forget 删除 learned"
assert_eq true "$(jq '.upstreams | has("gate.smartproxy.com:7000")' "$BASE/residential-blacklist.json")" "其它上游不受影响"
assert_eq 0 "$(cat "$BASE/helper.log" 2>/dev/null | wc -l | tr -d ' ')" "forget 不调 helper（helper 持锁时调用它）"

# --- status（人类可读）---
out=$(bash "$SCRIPT" status 2>/dev/null)
assert_contains "上游 gate.smartproxy.com:7000: 0 条" "$out" "status 列出上游"
assert_contains "钉住:" "$out" "status 列出钉住"

# --- 日志文件 ---
assert_file_exists "$BL_LOG" "日志文件存在"
rm -rf "$BASE"
finish
```

- [ ] **Step 3: Run the test and confirm it fails because the script does not exist**

Run: `bash tests/resi-blacklist/test_state.sh`
Expected: the first lines are

```
  FAIL - 无参数 → exit 2
         expected: [2]  actual: [127]
  FAIL - 未知命令 → exit 2
```

(bash prints `No such file or directory` for `server/resi-blacklist.sh`, exit 127) and the last line is `PASS 2 / FAIL 30` with exit status 1 (the two "passes" are the `helper.log` line-count checks, which trivially hold when nothing ran).

- [ ] **Step 4: Create `server/resi-blacklist.sh` (Task 1 version)**

Write `server/resi-blacklist.sh` with exactly this content (note the three marker lines `# ===== [...] =====` — Tasks 2–4 replace them):


```bash
#!/bin/bash
# resi-blacklist.sh — 住宅上游「机房直出黑名单」自动探测 / 日志学习 / 状态维护（v3.7.0 R13）
#
# 思路：默认全部流量走住宅，只把「这个上游代理不了的目标」送去机房直出。
#   黑名单按上游 (host:port) 分别维护，条目只有两种：域名（domain_suffix）与端口（port:N）。
#   候选 = 内置候选表（BLACKLIST_CANDIDATES）+ 从 b-ui-relay 日志学到的被拒目标（learned）。
#   判定 = 经上游探测两次都「代理拒绝」且直连对照成功 → 进；每日复测连续两次「能通」→ 出。
#   本脚本只写 residential-blacklist.json（.blacklist.lock 保护）；relay 配置仍由
#   residential-helper.sh 独家写入（blacklist-apply），调它之前必须已释放本脚本的锁。
#
# Usage:
#   resi-blacklist.sh probe [<host:port>|all]   探测候选，更新 entries，然后 helper blacklist-apply
#   resi-blacklist.sh learn                      扫描 relay 日志更新 learned（refresh 内部调用）
#   resi-blacklist.sh refresh                    learn → probe all → apply（b-ui-resi-blacklist.timer）
#   resi-blacklist.sh status [--json]            打印状态
#   resi-blacklist.sh pin <target> direct|resi|clear
#   resi-blacklist.sh forget <host:port>         删除一个上游的数据（helper --remove 时调用，不应用）
#   resi-blacklist.sh _probe-one <host:port> <target> [<probe_spec>]   内部：xargs 并行 worker
#
# 退出码：0 正常 / 1 错误 / 2 用法错误
# 无 set -e：探测里大量"预期失败"的 curl，逐处显式判断退出码更清楚。
set -uo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
BLACKLIST_STATE="${BASE_DIR}/residential-blacklist.json"
BLACKLIST_LOCK="${BASE_DIR}/.blacklist.lock"
HELPER="${BASE_DIR}/residential-helper.sh"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
SINGBOX_CONFIG="${BASE_DIR}/singbox-relay.json"
BL_LOG="${BL_LOG:-/var/log/b-ui-resi-blacklist.log}"
BL_JOURNAL_CMD="${BL_JOURNAL_CMD:-journalctl -u b-ui-relay --since -25h --no-pager -o cat}"
BL_PARALLEL="${BL_PARALLEL:-8}"
BL_PROBE_TIMEOUT="${BL_PROBE_TIMEOUT:-12}"
BL_RETRY_DELAY="${BL_RETRY_DELAY:-3}"      # 两次上游探测的间隔秒数（测试设 0）
BL_SKIP_APPLY="${BL_SKIP_APPLY:-0}"        # 1 = 不调 helper blacklist-apply（测试）
BL_NOW="${BL_NOW:-}"                       # 固定时间戳（测试）
BL_CANDIDATES_FILE="${BL_CANDIDATES_FILE:-}"   # 覆盖内置候选表（测试）
# xargs worker 以脚本自身路径再起进程；被 source 时 $0 是调用方，所以记 BASH_SOURCE
BLACKLIST_SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
export BASE_DIR BL_LOG BL_PROBE_TIMEOUT BL_RETRY_DELAY BL_CANDIDATES_FILE BL_NOW

BL_SKELETON='{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}'
# 这些端口永远不会成为端口条目（spec §3）
BL_NEVER_PORTS='[80,443,8080,8443]'

# ---------------------------------------------------------------------------
# 基础：时间 / 日志 / 状态文件 I/O / 锁
# ---------------------------------------------------------------------------
bl_now() { if [[ -n "$BL_NOW" ]]; then echo "$BL_NOW"; else date -u +%Y-%m-%dT%H:%M:%SZ; fi; }
log() { echo "[$(date '+%F %T')] $*" >> "$BL_LOG" 2>/dev/null; echo "$*" >&2; }
# 与 resi-health.sh 一样挂 EXIT 轮转，多处提前 return/exit 也不漏
rotate_log() { tail -300 "$BL_LOG" > "${BL_LOG}.tmp" 2>/dev/null && mv "${BL_LOG}.tmp" "$BL_LOG" 2>/dev/null; return 0; }
lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# 状态文件缺失 / 空 / 损坏 → 写入空骨架（600）
bl_state_init() {
    if [[ -s "$BLACKLIST_STATE" ]] && jq -e '.version == 1' "$BLACKLIST_STATE" >/dev/null 2>&1; then
        return 0
    fi
    [[ -e "$BLACKLIST_STATE" ]] && log "WARN 状态文件损坏或为空，重建为空骨架"
    echo "$BL_SKELETON" | bl_write
}
# 打印状态 JSON；文件不可用时打印骨架（读路径永不失败）
bl_read() {
    if [[ -s "$BLACKLIST_STATE" ]] && jq -e '.version == 1' "$BLACKLIST_STATE" >/dev/null 2>&1; then
        cat "$BLACKLIST_STATE"
    else
        echo "$BL_SKELETON"
    fi
}
# stdin JSON → 校验 → .tmp(600) → mv 原子替换
bl_write() {
    local tmp="${BLACKLIST_STATE}.tmp"
    mkdir -p "$(dirname "$BLACKLIST_STATE")" 2>/dev/null
    if ! jq -e '.' > "$tmp" 2>/dev/null || [[ ! -s "$tmp" ]]; then
        rm -f "$tmp"; log "ERROR 状态 JSON 无效，放弃写入"; return 1
    fi
    chmod 600 "$tmp" && mv "$tmp" "$BLACKLIST_STATE"
}
# 在状态文件上做一次 jq 改写：bl_update '<filter>' [jq 参数...]（不加锁，调用方负责）
bl_update() {
    local filter="$1"; shift
    bl_read | jq "$@" "$filter" | bl_write
}
# with_bl_lock <fn> [args...]：fd 8 上 flock -w 30，函数返回后关 fd 释放。
# 不能嵌套（同一进程再开一次会等自己 30s）；调 helper 前必须已经出了本函数（避免与 .relay.lock 互等）。
# 没有 flock（macOS 跑测试）时退化为无锁执行并记一条 WARN。
with_bl_lock() {
    local rc=0
    exec 8>>"$BLACKLIST_LOCK"
    chmod 600 "$BLACKLIST_LOCK" 2>/dev/null || true
    if command -v flock >/dev/null 2>&1; then
        flock -w 30 8 || { log "ERROR 获取黑名单锁超时(30s)，可能有另一个 resi-blacklist 在运行"; exec 8>&-; return 1; }
    else
        [[ -n "${BL_WARNED_NOFLOCK:-}" ]] || { log "WARN 缺少 flock，状态文件无锁写入"; BL_WARNED_NOFLOCK=1; }
    fi
    "$@" || rc=$?
    exec 8>&-
    return $rc
}

# 目标格式：小写主机名（至少一个点）或 port:N
valid_target() {
    local t="$1"
    [[ "$t" =~ ^([a-z0-9-]+\.)+[a-z0-9-]+$ ]] && return 0
    [[ "$t" =~ ^port:[0-9]{1,5}$ ]] && return 0
    return 1
}

# 调 helper 把当前有效黑名单写进 relay（helper 自己按摘要决定是否重启）。
# 调用前所有 with_bl_lock 必须已返回。
apply_via_helper() {
    local rc=0
    if [[ "$BL_SKIP_APPLY" == "1" ]]; then log "BL_SKIP_APPLY=1，跳过 helper blacklist-apply"; return 0; fi
    [[ -x "$HELPER" ]] || { log "WARN 找不到可执行的 ${HELPER}，黑名单未应用"; return 1; }
    "$HELPER" blacklist-apply || rc=$?
    if [[ "$rc" -ne 0 ]]; then log "ERROR helper blacklist-apply 失败(rc=${rc})"; return 1; fi
    log "helper blacklist-apply 完成"
}

# ===== [探测] =====

# ===== [学习] =====

# ===== [候选 / 进出规则 / 并行探测] =====

# ---------------------------------------------------------------------------
# 子命令
# ---------------------------------------------------------------------------
cmd_status() {
    bl_state_init || return 1
    if [[ "${1:-}" == "--json" ]]; then bl_read; return 0; fi
    bl_read | jq -r '
        "黑名单状态 (version \(.version))",
        (if .checking then "检测中: \(.checking.upstream) 自 \(.checking.startedAt)" else "检测中: 无" end),
        (if .applied then "最近应用: \(.applied.upstream) @ \(.applied.at) digest=\(.applied.digest)" else "最近应用: 无" end),
        (.upstreams | to_entries[]
            | "上游 \(.key): \(.value.entries | length) 条  最近检测 \(.value.checkedAt // "从未")",
              (.value.entries | to_entries[] | "    \(.key)  [\(.value.source)]  \(.value.reason)")),
        (.learned | to_entries[] | "候选 \(.key): \(.value | keys | join(" "))"),
        "钉住: \(.pins | to_entries | map("\(.key)=\(.value)") | join(" "))"'
}

cmd_pin() {
    local target mode="${2:-}"
    target=$(lower "${1:-}")
    [[ -n "$target" && -n "$mode" ]] || { usage; return 2; }
    case "$mode" in
        direct|resi)
            valid_target "$target" || { log "ERROR 目标格式无效: ${target}"; return 2; }
            with_bl_lock bl_update '.pins[$t] = $m' --arg t "$target" --arg m "$mode" || return 1
            log "钉住 ${target} → ${mode}" ;;
        clear)
            with_bl_lock bl_update 'del(.pins[$t])' --arg t "$target" || return 1
            log "取消钉住 ${target}" ;;
        *) usage; return 2 ;;
    esac
    apply_via_helper
}

# helper enable --remove 在持 .relay.lock 时调用本命令，所以这里绝不能反过来调 helper
cmd_forget() {
    local key
    key=$(lower "${1:-}")
    [[ "$key" =~ ^[a-z0-9.-]+:[0-9]{1,5}$ ]] || { usage; return 2; }
    with_bl_lock bl_update 'del(.upstreams[$k]) | del(.learned[$k])' --arg k "$key" || return 1
    log "已删除上游 ${key} 的黑名单与候选数据"
}

usage() {
    cat >&2 <<'EOF'
用法: resi-blacklist.sh <命令>
  probe [<host:port>|all]          探测内置候选表 + 学到的候选，更新黑名单，然后应用到 relay
  learn                            扫描 b-ui-relay 日志，更新候选池（learned）
  refresh                          learn → probe all → 应用（定时器每日调用）
  status [--json]                  打印状态（--json 输出原始状态文件）
  pin <target> direct|resi|clear   钉住目标：强制机房 / 强制住宅 / 取消钉住
  forget <host:port>               删除某个上游的黑名单与候选数据（helper --remove 调用）
EOF
}

main() {
    command -v jq >/dev/null 2>&1 || { echo "ERROR: 缺少 jq" >&2; exit 1; }
    trap rotate_log EXIT
    local cmd="${1:-}" rc=0
    [[ $# -gt 0 ]] && shift
    case "$cmd" in
        status)  cmd_status "$@" ;;
        pin)     cmd_pin "$@" ;;
        forget)  cmd_forget "$@" ;;
        *)       usage; exit 2 ;;
    esac
    rc=$?
    exit "$rc"
}

# 被 source（测试）时不执行主入口
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
```

Then make it executable and syntax-check it:

```bash
chmod +x server/resi-blacklist.sh
bash -n server/resi-blacklist.sh && echo SYNTAX_OK
```

Expected: `SYNTAX_OK`.

- [ ] **Step 5: Run the test and confirm it passes**

Run: `bash tests/resi-blacklist/test_state.sh`
Expected: 32 `ok` lines, last line `PASS 32 / FAIL 0`, exit 0. (On macOS one `WARN 缺少 flock，状态文件无锁写入` line per invocation goes to stderr — that is the documented fallback, not a failure.)

- [ ] **Step 6: Commit**


```bash
git add server/resi-blacklist.sh tests/resi-blacklist/lib.sh tests/resi-blacklist/test_state.sh
git commit -m "feat(residential): 新增 resi-blacklist.sh 骨架 —— 状态文件读写/锁/status/pin/forget + 测试基座

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: 探测判定 —— `classify_probe` / `upstream_cred_cfg` / `direct_check` / `probe_target` / `_probe-one`

**Files:**
- Modify: `server/resi-blacklist.sh:121` (the marker line `# ===== [探测] =====`) and `server/resi-blacklist.sh:190-191` (dispatch lines `forget)` / `*)` in `main`)
- Create: `tests/resi-blacklist/stubs/curl`
- Create: `tests/resi-blacklist/stubs/timeout`
- Create: `tests/resi-blacklist/test_probe.sh`

**Interfaces:**
- Consumes: `lower`, `log`, `usage`, `RESIDENTIAL_CONFIG`, `BL_PROBE_TIMEOUT`, `BL_RETRY_DELAY`, `BL_CANDIDATES_FILE` (Task 1); `curl`, `timeout`, `mktemp`.
- Produces (exact names): `BLACKLIST_CANDIDATES` (builtin table, lines `<group>\t<probe_spec>\t<target>`, groups `search|social|payment|apple|push`, exactly the spec §6.1 hosts: 6+7+6+6 domains + 2 push ports = 27 rows), `candidate_table` (prints the effective table without comments/blank lines; `BL_CANDIDATES_FILE` overrides), `source_for_target <target>` (`builtin:<group>` or `learned`), `curl_cfg_escape`, `upstream_cred_cfg <host:port>` (prints `proxy = "socks5h://h:p"` or `"http://h:p"` + `proxy-user = "u:p"`; missing type ⇒ socks5; unknown upstream or illegal host/port/newline ⇒ return 1), `verbose_status <vfile>`, `verbose_reason <vfile>`, `verbose_socks_reason <vfile>`, `classify_probe <domain|port> <curl_exit> <vfile>` → `verdict\treason` with verdict ∈ `refused|ok|unknown|unavailable`, `direct_check <target> [<probe_host>]` (exit 0 when reachable directly), `probe_once <kind> <cfg> <url>`, `probe_target <host:port> <target> [<probe_host>]` → `blocked|ok|unknown|unavailable\treason`, `cmd_probe_one` = CLI `_probe-one <host:port> <target> [<probe_spec>]` printing one result TSV line `<target>\t<verdict>\t<reason>\t<source>`.
- Test stubs: `stubs/curl` replays a scenario from `STUB_CURL_SCENARIO` for upstream calls (those with `-K`; comma-separated list = per-call sequence, counter in `STUB_CURL_COUNT_FILE`) and `STUB_DIRECT_SCENARIO` for direct calls, writes a realistic `-v` transcript to stderr, appends `proxy|direct <scenario> <url>` to `STUB_CURL_LOG`; scenarios: `ok200 site403 refused403 refused502 socks97 socks7 tls35 port52 timeout28 proxydown7 auth407 weird56 direct7`. `stubs/timeout` exits `${STUB_TCP_EXIT:-0}`.

Decision table implemented by `classify_probe` (spec §5.1/§5.2):

| kind | curl exit / transcript | verdict |
|---|---|---|
| any | CONNECT response `407` in transcript | `unavailable` |
| any | 0 | `ok` (any HTTP status, incl. site 401/403) |
| any | 28 | `unknown` |
| any | 56 with CONNECT 4xx/5xx status line | `refused` (reason = status line + `x-brd-*`/`x-luminati-*` headers) |
| any | 56 without CONNECT status | `unknown` |
| any | 97 | `refused` (SOCKS5) |
| any | 7 with `Can't complete SOCKS5…` / `SOCKS5 … not allowed|refused` | `refused` (old curl < 7.73) |
| any | 7 otherwise | `unavailable` |
| domain | 35 | `refused` (TLS cut after CONNECT; `probe_target` still needs the direct control to pass) |
| domain | 52 | `unknown` |
| port | 35 / 52 | `ok` (CONNECT established) |
| any | other | `unknown` |

`probe_target`: attempt 1 `ok|unknown|unavailable` ⇒ final immediately (no retry); `refused` ⇒ sleep `BL_RETRY_DELAY`, attempt 2: `refused` ⇒ run `direct_check`: reachable ⇒ `blocked`, else `unknown` (`直连对照失败，不归咎上游`); `ok` ⇒ `unknown` (`两次结果不一致`); other ⇒ that verdict.

- [ ] **Step 1: Create the curl and timeout stubs**

Write `tests/resi-blacklist/stubs/curl`:


```bash
#!/usr/bin/env bash
# curl 桩：按场景回放退出码 + 写一份逼真的 -v 转录到 stderr。
#   参数含 -K  → 经上游探测，场景取 STUB_CURL_SCENARIO（逗号分隔 = 按调用次序依次取，超出取最后一个）
#   其它       → 直连对照，场景取 STUB_DIRECT_SCENARIO（默认 ok200）
# 每次调用把 "proxy|direct <scenario> <url>" 追加到 STUB_CURL_LOG（若设置）
is_proxy=0; url=""
for a in "$@"; do
    [ "$a" = "-K" ] && is_proxy=1
    case "$a" in http://*|https://*) url="$a" ;; esac
done
if [ "$is_proxy" = 1 ]; then
    cat > /dev/null      # 吃掉 stdin 的 -K 配置（凭据）
    n=0; cf="${STUB_CURL_COUNT_FILE:-}"
    if [ -n "$cf" ]; then [ -f "$cf" ] && n=$(cat "$cf"); echo $((n + 1)) > "$cf"; fi
    IFS=, read -r -a arr <<< "${STUB_CURL_SCENARIO:-ok200}"
    idx=$n; [ "$idx" -ge "${#arr[@]}" ] && idx=$((${#arr[@]} - 1))
    sc="${arr[$idx]}"; who=proxy
else
    sc="${STUB_DIRECT_SCENARIO:-ok200}"; who=direct
fi
[ -n "${STUB_CURL_LOG:-}" ] && echo "$who $sc $url" >> "$STUB_CURL_LOG"
p() { printf '%s\n' "$@" >&2; }
case "$sc" in
    ok200)      p '* Connected to www.google.com (142.250.72.4) port 443' '> HEAD / HTTP/1.1' '< HTTP/1.1 200 OK' '< content-type: text/html'; exit 0 ;;
    site403)    p '< HTTP/1.1 403 Forbidden' '< server: cloudflare' '< cf-mitigated: challenge'; exit 0 ;;
    refused403) p '* CONNECT tunnel: HTTP/1.1 negotiated' '> CONNECT www.google.com:443 HTTP/1.1' \
                  '< HTTP/1.1 403 Forbidden serp domain' '< x-brd-error: policy_20110' '< x-brd-err-code: policy_20110' \
                  '* CONNECT tunnel failed, response 403'; exit 56 ;;
    refused502) p '< HTTP/1.1 502 Bad Gateway' '< x-luminati-error: target_connect_failed' '* CONNECT tunnel failed, response 502'; exit 56 ;;
    socks97)    p '* SOCKS5 connect to www.google.com:443 (remotely resolved)' "* Can't complete SOCKS5 connection to www.google.com. (2)"; exit 97 ;;
    socks7)     p "* Can't complete SOCKS5 connection to www.google.com. (2)"; exit 7 ;;
    tls35)      p '< HTTP/1.1 200 Connection established' '* CONNECT phase completed' \
                  '* OpenSSL SSL_connect: Connection reset by peer in connection to www.google.com:443'; exit 35 ;;
    port52)     p '< HTTP/1.1 200 Connection established' '* Empty reply from server'; exit 52 ;;
    timeout28)  p '* Operation timed out after 12000 milliseconds with 0 bytes received'; exit 28 ;;
    proxydown7) p '* Failed to connect to brd.superproxy.io port 44445: Connection refused'; exit 7 ;;
    auth407)    p '< HTTP/1.1 407 Proxy Authentication Required' '< Proxy-Authenticate: Basic' '* CONNECT tunnel failed, response 407'; exit 56 ;;
    weird56)    p '* Recv failure: Connection reset by peer'; exit 56 ;;
    direct7)    p '* Failed to connect to www.google.com port 443: No route to host'; exit 7 ;;
    *)          p "stub curl: unknown scenario $sc"; exit 99 ;;
esac
```

Write `tests/resi-blacklist/stubs/timeout`:


```bash
#!/usr/bin/env bash
# timeout 桩（macOS 无 coreutils timeout）：端口直连对照的结果由 STUB_TCP_EXIT 决定（0 通 / 124 超时）
exit "${STUB_TCP_EXIT:-0}"
```

```bash
chmod +x tests/resi-blacklist/stubs/curl tests/resi-blacklist/stubs/timeout
```

- [ ] **Step 2: Write the failing test `tests/resi-blacklist/test_probe.sh`**


```bash
#!/usr/bin/env bash
# Task 2：classify_probe / upstream_cred_cfg / direct_check / probe_target / _probe-one（spec §5.1 §5.2）
set -u
. "$(dirname "$0")/lib.sh"
make_base; BASE="$BASE_DIR"
write_resi_config "$BASE"
export PATH="$REPO_ROOT/tests/resi-blacklist/stubs:$PATH"
export STUB_CURL_LOG="$BASE/curl.log" STUB_CURL_COUNT_FILE="$BASE/curl.count"
. "$SCRIPT"     # source：主入口被 BASH_SOURCE 守卫跳过

reset_stub() { rm -f "$STUB_CURL_LOG" "$STUB_CURL_COUNT_FILE"; export STUB_CURL_SCENARIO="$1"; export STUB_DIRECT_SCENARIO="${2:-ok200}"; export STUB_TCP_EXIT="${3:-0}"; }
proxy_calls() { grep -c '^proxy ' "$STUB_CURL_LOG" 2>/dev/null || echo 0; }
# 用桩生成一份 -v 转录文件，然后直接喂给 classify_probe
transcript() { STUB_CURL_SCENARIO="$1" curl -K - https://x/ < /dev/null 2> "$BASE/v.txt"; echo $?; }

# --- upstream_cred_cfg ---
out=$(upstream_cred_cfg brd.superproxy.io:44445)
assert_eq 'proxy = "http://brd.superproxy.io:44445"' "$(echo "$out" | sed -n 1p)" "http 上游 → http:// proxy 行"
assert_eq 'proxy-user = "brd-customer-abc-zone-isp:p4ss\"w0rd"' "$(echo "$out" | sed -n 2p)" "proxy-user 行（引号已转义）"
out=$(upstream_cred_cfg GATE.smartproxy.com:7000)
assert_eq 'proxy = "socks5h://Gate.Smartproxy.COM:7000"' "$(echo "$out" | sed -n 1p)" "缺 type → socks5h（键不区分大小写）"
upstream_cred_cfg nope.example:1 >/dev/null; assert_eq 1 $? "未知上游 → 返回 1"

# --- classify_probe：域名（spec §5.1 每一行）---
rc=$(transcript refused403); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq $'refused\tHTTP/1.1 403 Forbidden serp domain; x-brd-error: policy_20110; x-brd-err-code: policy_20110' "$out" "56 + CONNECT 403 → refused（原因含状态行与 x-brd 头）"
rc=$(transcript refused502); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq $'refused\tHTTP/1.1 502 Bad Gateway; x-luminati-error: target_connect_failed' "$out" "56 + CONNECT 502 → refused（x-luminati 头）"
rc=$(transcript socks97); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "refused" "${out%%	*}" "97 → refused（SOCKS5）"
assert_contains "Can't complete SOCKS5" "$out" "SOCKS5 原因来自转录"
rc=$(transcript socks7); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "refused" "${out%%	*}" "老 curl 7 + SOCKS5 拒绝文案 → refused"
rc=$(transcript tls35); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq $'refused\tCONNECT 后 TLS 被掐(35)' "$out" "35 → refused（域名）"
rc=$(transcript ok200); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq $'ok\t' "$out" "0 → ok"
rc=$(transcript site403); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "ok" "${out%%	*}" "0 + 站点 403（Cloudflare 挑战）→ ok"
rc=$(transcript timeout28); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "unknown" "${out%%	*}" "28 → unknown"
rc=$(transcript proxydown7); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "unavailable" "${out%%	*}" "7（连不上代理）→ unavailable"
rc=$(transcript auth407); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "unavailable" "${out%%	*}" "CONNECT 407 → unavailable"
rc=$(transcript weird56); out=$(classify_probe domain "$rc" "$BASE/v.txt")
assert_eq "unknown" "${out%%	*}" "56 但无 CONNECT 响应 → unknown"

# --- classify_probe：端口（spec §5.2）---
rc=$(transcript refused403); assert_eq "refused" "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 56 → refused"
rc=$(transcript socks97);    assert_eq "refused" "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 97 → refused"
rc=$(transcript ok200);      assert_eq "ok"      "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 0 → ok"
rc=$(transcript tls35);      assert_eq "ok"      "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 35 → ok（CONNECT 已建立）"
rc=$(transcript port52);     assert_eq "ok"      "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 52 → ok"
rc=$(transcript proxydown7); assert_eq "unavailable" "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 7 → unavailable"
rc=$(transcript timeout28);  assert_eq "unknown" "$(classify_probe port "$rc" "$BASE/v.txt" | cut -f1)" "端口 28 → unknown"

# --- direct_check ---
reset_stub ok200 ok200;   direct_check www.google.com;  assert_eq 0 $? "域名直连：任意 HTTP 状态 → 可达"
reset_stub ok200 site403; direct_check www.google.com;  assert_eq 0 $? "域名直连：403 也算可达"
reset_stub ok200 direct7; direct_check www.google.com;  assert_eq 7 $? "域名直连失败 → 非 0"
reset_stub ok200 ok200 0;   direct_check port:5228 mtalk.google.com; assert_eq 0 $? "端口直连（timeout 桩 0）→ 可达"
reset_stub ok200 ok200 124; direct_check port:5228 mtalk.google.com; assert_eq 124 $? "端口直连超时 → 非 0"
direct_check port:5228; assert_eq 1 $? "端口目标缺探测主机 → 1"

# --- probe_target：域名 ---
reset_stub refused403;           out=$(probe_target brd.superproxy.io:44445 www.google.com)
assert_eq "blocked" "$(echo "$out" | cut -f1)" "两次拒绝 + 直连通 → blocked"
assert_contains "policy_20110" "$out" "blocked 原因带 x-brd-error"
assert_eq 2 "$(proxy_calls)" "blocked 用了两次上游探测"
assert_eq 1 "$(grep -c '^direct ' "$STUB_CURL_LOG")" "blocked 做了一次直连对照"
reset_stub refused403 direct7;   out=$(probe_target brd.superproxy.io:44445 www.google.com)
assert_eq $'unknown\t直连对照失败，不归咎上游' "$out" "两次拒绝但直连不通 → unknown（绝不 blocked）"
reset_stub ok200;                out=$(probe_target brd.superproxy.io:44445 www.google.com)
assert_eq "ok" "$(echo "$out" | cut -f1)" "第一次能通 → ok"
assert_eq 1 "$(proxy_calls)" "ok 只探一次"
reset_stub refused403,ok200;     out=$(probe_target brd.superproxy.io:44445 www.google.com)
assert_eq "unknown" "$(echo "$out" | cut -f1)" "拒绝/能通不一致 → unknown"
reset_stub proxydown7;           out=$(probe_target brd.superproxy.io:44445 www.google.com)
assert_eq "unavailable" "$(echo "$out" | cut -f1)" "连不上代理 → unavailable"
assert_eq 1 "$(proxy_calls)" "unavailable 不再重试"
reset_stub auth407;              assert_eq "unavailable" "$(probe_target brd.superproxy.io:44445 www.google.com | cut -f1)" "407 → unavailable"
reset_stub timeout28;            assert_eq "unknown" "$(probe_target brd.superproxy.io:44445 www.google.com | cut -f1)" "超时 → unknown"
reset_stub refused403,timeout28; assert_eq "unknown" "$(probe_target brd.superproxy.io:44445 www.google.com | cut -f1)" "拒绝后超时 → unknown"
reset_stub tls35;                assert_eq "blocked" "$(probe_target brd.superproxy.io:44445 www.google.com | cut -f1)" "35 两次 + 直连 TLS 正常 → blocked"
reset_stub socks97;              assert_eq "blocked" "$(probe_target gate.smartproxy.com:7000 www.google.com | cut -f1)" "SOCKS5 上游 97 两次 → blocked"
reset_stub refused403;           assert_eq "unavailable" "$(probe_target nope.example:1 www.google.com | cut -f1)" "未知上游 → unavailable"

# --- probe_target：端口 ---
reset_stub refused403 ok200 0;   out=$(probe_target brd.superproxy.io:44445 port:5228 mtalk.google.com)
assert_eq "blocked" "$(echo "$out" | cut -f1)" "端口两次拒绝 + TCP 直连通 → blocked"
assert_contains "https://mtalk.google.com:5228/" "$(grep '^proxy' "$STUB_CURL_LOG" | head -1)" "端口探测 URL 是 https://host:port/"
reset_stub refused403 ok200 124; assert_eq "unknown" "$(probe_target brd.superproxy.io:44445 port:5228 mtalk.google.com | cut -f1)" "端口直连超时 → unknown"
reset_stub tls35 ok200 0;        assert_eq "ok" "$(probe_target brd.superproxy.io:44445 port:5228 mtalk.google.com | cut -f1)" "端口 35 → ok"
out=$(probe_target brd.superproxy.io:44445 port:5228); assert_eq "unknown" "$(echo "$out" | cut -f1)" "端口目标缺探测主机 → unknown"

# --- _probe-one（CLI）---
reset_stub refused403
out=$(bash "$SCRIPT" _probe-one brd.superproxy.io:44445 www.google.com 2>/dev/null)
assert_eq "www.google.com" "$(echo "$out" | cut -f1)" "_probe-one 第 1 列 target"
assert_eq "blocked" "$(echo "$out" | cut -f2)" "_probe-one 第 2 列 verdict"
assert_eq "builtin:search" "$(echo "$out" | cut -f4)" "_probe-one 第 4 列 source（内置表）"
reset_stub refused403
out=$(bash "$SCRIPT" _probe-one brd.superproxy.io:44445 port:5228 mtalk.google.com:5228 2>/dev/null)
assert_eq $'port:5228\tblocked' "$(echo "$out" | cut -f1,2)" "_probe-one 端口目标（probe_spec 第 3 参）"
assert_eq "builtin:push" "$(echo "$out" | cut -f4)" "端口 source builtin:push"
out=$(bash "$SCRIPT" _probe-one brd.superproxy.io:44445 gateway.icloud.example 2>/dev/null)
assert_eq "learned" "$(echo "$out" | cut -f4)" "非内置目标 source=learned"
bash "$SCRIPT" _probe-one >/dev/null 2>&1; assert_eq 2 $? "_probe-one 缺参数 → 2"
assert_contains "probe brd.superproxy.io:44445 www.google.com: blocked" "$(cat "$BL_LOG")" "worker 写日志"

# --- source_for_target / candidate_table ---
assert_eq 27 "$(candidate_table | wc -l | tr -d ' ')" "内置候选表 27 行（6+7+6+6+2，spec §6.1）"
assert_eq $'push\tmtalk.google.com:5228\tport:5228' "$(candidate_table | grep 'port:5228')" "候选表行是三列 TAB"
printf 'g1 a.example a.example\n# c\ng2 h.example:5000 port:5000\n' > "$BASE/cands.txt"
assert_eq "builtin:g2" "$(BL_CANDIDATES_FILE="$BASE/cands.txt" source_for_target port:5000)" "BL_CANDIDATES_FILE 覆盖内置表"
rm -rf "$BASE"
finish
```

- [ ] **Step 3: Run the test and confirm it fails because the probe functions do not exist yet**

Run: `bash tests/resi-blacklist/test_probe.sh`
Expected: first lines

```
tests/resi-blacklist/test_probe.sh: line 17: upstream_cred_cfg: command not found
  FAIL - http 上游 → http:// proxy 行
         expected: [proxy = "http://brd.superproxy.io:44445"]  actual: []
```

and last line `PASS 1 / FAIL 60`, exit 1.

- [ ] **Step 4: Replace the `# ===== [探测] =====` marker with the probe block**

In `server/resi-blacklist.sh`, line 121 currently reads exactly:

```bash
# ===== [探测] =====
```

Replace that single line with the following block (it begins with the same marker line; the blank line that followed the marker stays):


```bash
# ===== [探测] =====
# 内置候选表（spec §6.1）：三列 "<group> <probe_spec> <target>"，空白转成 TAB 存放。
#   域名目标：probe_spec == target（探 https://target/）
#   端口目标：probe_spec 是「真实推送主机:端口」，target 是 port:N（命中后按端口直出）
# 表只是「值得一探」的候选，进不进黑名单完全由探测结果决定；政府/银行/流媒体不内置，靠日志学习。
BLACKLIST_CANDIDATES=$(tr -s ' ' '\t' <<'EOF'
# 搜索（Bright Data policy_20110）
search www.google.com www.google.com
search www.bing.com www.bing.com
search duckduckgo.com duckduckgo.com
search search.yahoo.com search.yahoo.com
search www.baidu.com www.baidu.com
search yandex.com yandex.com
# 短视频 / 社交（policy_20050 等）
social www.tiktok.com www.tiktok.com
social api.tiktokv.com api.tiktokv.com
social www.instagram.com www.instagram.com
social www.facebook.com www.facebook.com
social x.com x.com
social api.x.com api.x.com
social www.reddit.com www.reddit.com
# 支付处理商（policy_20050）
payment checkout.stripe.com checkout.stripe.com
payment js.stripe.com js.stripe.com
payment api.stripe.com api.stripe.com
payment m.stripe.com m.stripe.com
payment www.paypal.com www.paypal.com
payment api.paypal.com api.paypal.com
# Apple 服务（2026-09-11 bwg-tizi 日志实测被拒）
apple gateway.icloud.com gateway.icloud.com
apple query.ess.apple.com query.ess.apple.com
apple courier.push.apple.com courier.push.apple.com
apple gdmf.apple.com gdmf.apple.com
apple mesu.apple.com mesu.apple.com
apple xp.apple.com xp.apple.com
# 推送端口（用真实推送主机探测，命中记为端口条目）
push mtalk.google.com:5228 port:5228
push courier.push.apple.com:5223 port:5223
EOF
)

# 输出候选表有效行 "<group>\t<probe_spec>\t<target>"（BL_CANDIDATES_FILE 覆盖内置表；空白分隔亦可）
candidate_table() {
    if [[ -n "$BL_CANDIDATES_FILE" ]]; then
        tr -s ' ' '\t' < "$BL_CANDIDATES_FILE"
    else
        printf '%s\n' "$BLACKLIST_CANDIDATES"
    fi | grep -v -E '^(#|[[:space:]]*$)'
}
# 目标在内置表里 → builtin:<group>，否则 learned
source_for_target() {
    local g
    g=$(candidate_table | awk -F'\t' -v t="$1" '$3 == t { print $1; exit }')
    if [[ -n "$g" ]]; then echo "builtin:${g}"; else echo "learned"; fi
}

# curl 配置文件双引号内需转义 \ 与 "（与 residential-helper.sh 的 curl_cfg_escape 一致）
curl_cfg_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }

# upstream_cred_cfg <host:port>：从 residential-proxy.json 找该上游，打印 curl -K - 配置行
#   proxy = "socks5h://h:p" 或 "http://h:p"，再一行 proxy-user = "u:p"（凭据不进 argv）
# type 缺省 = socks5；找不到 / 字段非法 → 返回 1。老版单 URL（urls 为空、顶层 host/port）也兼容。
upstream_cred_cfg() {
    local key h p row scheme host port user pass type
    key=$(lower "$1"); h="${key%:*}"; p="${key##*:}"
    [[ -f "$RESIDENTIAL_CONFIG" ]] || return 1
    row=$(jq -c --arg h "$h" --arg p "$p" '
        . as $c
        | (($c.urls // []) | if length > 0 then . else
             [{host: ($c.host // ""), port: ($c.port // 0), username: ($c.username // ""),
               password: ($c.password // ""), type: ($c.type // "socks5")}] end)
        | map(select(((.host // "") | ascii_downcase) == $h and ((.port | tostring) == $p)))
        | .[0] // empty' "$RESIDENTIAL_CONFIG" 2>/dev/null) || return 1
    [[ -n "$row" ]] || return 1
    host=$(printf '%s' "$row" | jq -r '.host');            port=$(printf '%s' "$row" | jq -r '.port')
    user=$(printf '%s' "$row" | jq -r '.username // ""');  pass=$(printf '%s' "$row" | jq -r '.password // ""')
    type=$(printf '%s' "$row" | jq -r '.type // "socks5"')
    # 换行会在 curl 配置里注入额外指令；host/port 直接进 proxy 行，严格校验（同 resi-health.sh）
    case "${user}${pass}" in *$'\n'*|*$'\r'*) log "WARN ${key} 凭据含换行符，跳过"; return 1 ;; esac
    case "$host" in ""|*$'\n'*|*$'\r'*|*'"'*|*'\'*) log "WARN ${key} host 非法，跳过"; return 1 ;; esac
    [[ "$port" =~ ^[0-9]{1,5}$ ]] || { log "WARN ${key} port 非法，跳过"; return 1; }
    if [[ "$type" == "http" ]]; then scheme="http"; else scheme="socks5h"; fi
    printf 'proxy = "%s://%s:%s"\n' "$scheme" "$(curl_cfg_escape "$host")" "$port"
    [[ -n "$user" ]] && printf 'proxy-user = "%s:%s"\n' "$(curl_cfg_escape "$user")" "$(curl_cfg_escape "$pass")"
    return 0
}

# -v 转录里 CONNECT 响应的 HTTP 状态码（第一条 4xx/5xx），没有则空
verbose_status() { tr -d '\r' < "$1" | grep -E -m1 '^< HTTP/[0-9.]+ [45][0-9]{2}' | awk '{print $3}'; }
# 拒绝原因：CONNECT 响应状态行 + x-brd-* / x-luminati-* 头，分号拼接（最多 5 行）
verbose_reason() {
    tr -d '\r' < "$1" | grep -E '^< (HTTP/[0-9.]+ [45][0-9]{2}|[Xx]-[Bb]rd-|[Xx]-[Ll]uminati-)' \
        | sed -e 's/^< //' | head -5 | paste -sd ';' - | sed -e 's/;/; /g'
}
# SOCKS5 拒绝形态（curl ≥7.73 退出 97，老 curl 退出 7 但转录里有同样的话）
verbose_socks_reason() {
    tr -d '\r' < "$1" | grep -E -m1 "Can't complete SOCKS5|SOCKS5.*(refused|not allowed|failed)" | sed -e 's/^\* //'
}

# classify_probe <domain|port> <curl_exit> <verbose_file> → "verdict<TAB>reason"
# verdict ∈ refused | ok | unknown | unavailable（spec §5.1 / §5.2）
#   407 ⇒ unavailable（代理鉴权失败，整轮跳过）；7 ⇒ unavailable（连不上代理本身）；28 ⇒ unknown
#   域名：56+CONNECT 4xx/5xx、97、35（CONNECT 后 TLS 被掐）⇒ refused；0 ⇒ ok（任意 HTTP 状态，含 401/403）
#   端口：56/97 ⇒ refused；0/35/52 ⇒ ok（CONNECT 已建立）
classify_probe() {
    local kind="$1" rc="$2" vfile="$3" status reason
    status=$(verbose_status "$vfile")
    if [[ "$status" == "407" ]]; then printf 'unavailable\t%s\n' "代理鉴权失败(407)"; return 0; fi
    case "$rc" in
        0)  printf 'ok\t%s\n' "" ;;
        28) printf 'unknown\t%s\n' "超时(28)" ;;
        56)
            if [[ -n "$status" ]]; then
                reason=$(verbose_reason "$vfile"); printf 'refused\t%s\n' "$reason"
            else
                printf 'unknown\t%s\n' "curl 56 但转录里没有 CONNECT 响应"
            fi ;;
        97)
            reason=$(verbose_socks_reason "$vfile"); printf 'refused\t%s\n' "SOCKS5 拒绝: ${reason:-curl 97}" ;;
        7)
            reason=$(verbose_socks_reason "$vfile")
            if [[ -n "$reason" ]]; then printf 'refused\t%s\n' "SOCKS5 拒绝: ${reason}"
            else printf 'unavailable\t%s\n' "连不上代理(7)"; fi ;;
        35|52)
            if [[ "$kind" == "port" ]]; then printf 'ok\t%s\n' "CONNECT 已建立(curl ${rc})"
            elif [[ "$rc" == "35" ]]; then printf 'refused\t%s\n' "CONNECT 后 TLS 被掐(35)"
            else printf 'unknown\t%s\n' "curl 52"; fi ;;
        *)  printf 'unknown\t%s\n' "curl ${rc}" ;;
    esac
    return 0
}

# direct_check <target> [<probe_host>]：直连对照，可达返回 0
#   域名 → curl -I https://host/，任意 HTTP 状态都算可达；端口 → /dev/tcp 建连（timeout 8）
direct_check() {
    local target="$1" probe_host="${2:-}" port
    if [[ "$target" == port:* ]]; then
        port="${target#port:}"
        [[ -n "$probe_host" ]] || return 1
        command -v timeout >/dev/null 2>&1 || { log "WARN 缺少 timeout，端口直连对照按失败处理"; return 1; }
        timeout 8 bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$probe_host" "$port" 2>/dev/null
    else
        curl -sS -o /dev/null -I --max-time "$BL_PROBE_TIMEOUT" "https://${target}/" 2>/dev/null
    fi
}

# probe_once <kind> <cfg> <url>：经上游探一次 → "verdict<TAB>reason"
probe_once() {
    local kind="$1" cfg="$2" url="$3" vfile rc=0
    vfile=$(mktemp "${TMPDIR:-/tmp}/bl-probe.XXXXXX") || { printf 'unknown\t%s\n' "mktemp 失败"; return 0; }
    printf '%s\n' "$cfg" | curl -sS -o /dev/null -I -v --max-time "$BL_PROBE_TIMEOUT" -K - "$url" 2>"$vfile" || rc=$?
    classify_probe "$kind" "$rc" "$vfile"
    rm -f "$vfile"
}

# probe_target <host:port> <target> [<probe_host>] → "blocked|ok|unknown|unavailable<TAB>reason"
#   第一次 ok/unknown/unavailable 直接定论；第一次 refused → 隔 BL_RETRY_DELAY 秒再探：
#   两次都 refused 且直连对照成功 ⇒ blocked；直连也不通 ⇒ unknown（不是代理的错，不动状态）
probe_target() {
    local up="$1" target="$2" probe_host="${3:-}" kind url cfg line v1 r1 v2 r2
    if [[ "$target" == port:* ]]; then
        kind="port"
        [[ -n "$probe_host" ]] || { printf 'unknown\t%s\n' "端口目标缺少探测主机"; return 0; }
        url="https://${probe_host}:${target#port:}/"
    else
        kind="domain"; url="https://${target}/"
    fi
    cfg=$(upstream_cred_cfg "$up") || { printf 'unavailable\t%s\n' "上游 ${up} 不在配置中"; return 0; }
    line=$(probe_once "$kind" "$cfg" "$url"); IFS=$'\t' read -r v1 r1 <<< "$line"
    case "$v1" in
        refused) ;;
        *) printf '%s\t%s\n' "$v1" "${r1:-}"; return 0 ;;
    esac
    sleep "$BL_RETRY_DELAY"
    line=$(probe_once "$kind" "$cfg" "$url"); IFS=$'\t' read -r v2 r2 <<< "$line"
    case "$v2" in
        refused) ;;
        ok) printf 'unknown\t%s\n' "两次结果不一致(拒绝/能通)"; return 0 ;;
        *)  printf '%s\t%s\n' "$v2" "${r2:-}"; return 0 ;;
    esac
    if direct_check "$target" "$probe_host"; then
        printf 'blocked\t%s\n' "${r2:-}"
    else
        printf 'unknown\t%s\n' "直连对照失败，不归咎上游"
    fi
}

# _probe-one <host:port> <target> [<probe_spec>]（xargs worker）
# 输出一行结果 TSV："<target>\t<verdict>\t<reason>\t<source>"
cmd_probe_one() {
    local up="${1:-}" target="${2:-}" spec probe_host line verdict reason source
    [[ -n "$up" && -n "$target" ]] || { usage; return 2; }
    spec="${3:-$target}"; probe_host="${spec%%:*}"
    line=$(probe_target "$up" "$target" "$probe_host"); IFS=$'\t' read -r verdict reason <<< "$line"
    reason=$(printf '%s' "${reason:-}" | tr '\t\n' '  ')
    source=$(source_for_target "$target")
    log "probe ${up} ${target}: ${verdict}${reason:+ (${reason})}"
    printf '%s\t%s\t%s\t%s\n' "$target" "$verdict" "$reason" "$source"
}
```

- [ ] **Step 5: Register `_probe-one` in the dispatch**

In `main`, the lines (190–191 before this edit) currently read exactly:

```bash
        forget)  cmd_forget "$@" ;;
        *)       usage; exit 2 ;;
```

Change them to:

```bash
        forget)  cmd_forget "$@" ;;
        _probe-one) cmd_probe_one "$@" ;;
        *)       usage; exit 2 ;;
```

Then: `bash -n server/resi-blacklist.sh && echo SYNTAX_OK` → `SYNTAX_OK`.

- [ ] **Step 6: Run the tests**

Run: `bash tests/resi-blacklist/test_probe.sh`
Expected: last line `PASS 61 / FAIL 0`, exit 0.

Run: `bash tests/resi-blacklist/test_state.sh`
Expected: still `PASS 32 / FAIL 0`.

- [ ] **Step 7: Commit**


```bash
git add server/resi-blacklist.sh tests/resi-blacklist/stubs/curl tests/resi-blacklist/stubs/timeout tests/resi-blacklist/test_probe.sh
git commit -m "feat(residential): resi-blacklist.sh 探测判定 —— classify_probe/upstream_cred_cfg/probe_target/_probe-one

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: 从 relay 日志学习候选 —— `learn_from_lines` / `learn`

**Files:**
- Modify: `server/resi-blacklist.sh:321` (the marker line `# ===== [学习] =====`) and `server/resi-blacklist.sh:385-386` (`case "$cmd" in` / `status)` in `main`)
- Create: `tests/resi-blacklist/fixtures/make-journal.sh`
- Create: `tests/resi-blacklist/fixtures/relay-journal.log` (generated by the script above; committed so the test is deterministic)
- Create: `tests/resi-blacklist/test_learn.sh`

**Interfaces:**
- Consumes: `bl_now`, `log`, `with_bl_lock`, `bl_update`, `SINGBOX_CONFIG`, `BL_NEVER_PORTS`, `BL_JOURNAL_CMD` (Tasks 1–2).
- Produces (exact names): `BL_JOURNAL_RE` (the contract regex, bash ERE form), `relay_tag_map_json` (prints `{"resi-1":"host:port",…}` from `SINGBOX_CONFIG` socks/http outbounds, `{}` when missing), `learn_from_lines` (stdin = raw journal lines incl. ANSI → strips ANSI → regex → jq aggregation → updates `.learned` under `with_bl_lock`; thresholds: domain ≥3 hits; bare-IP targets aggregated per port, ≥3 hits **and** ≥2 distinct IPs, never for 80/443/8080/8443, `sampleIp` = last IP seen; `count` = hits in this scan, `firstSeen` kept, `lastSeen` = now; entries with `lastSeen` older than 7 days that are not in that upstream's `entries` are dropped; ≤200 per upstream by newest `lastSeen`; returns 1 when the relay config has no residential outbounds), `cmd_learn` = CLI `learn` (runs `bash -c "$BL_JOURNAL_CMD"`; non-zero exit **with empty output** ⇒ WARN + return 1, state untouched).

- [ ] **Step 1: Create the fixture generator and generate the fixture**

Write `tests/resi-blacklist/fixtures/make-journal.sh`:


```bash
#!/usr/bin/env bash
# 生成 relay-journal.log 夹具：bwg-tizi 上 journalctl -u b-ui-relay -o cat 的真实形态（含 ANSI 颜色码）
# 用法：bash tests/resi-blacklist/fixtures/make-journal.sh > tests/resi-blacklist/fixtures/relay-journal.log
E=$'\033'
line() {   # line <target:port> <resi-N> <reason> [http|socks]
    printf '%s[31mERROR%s[0m[4006] [%s[38;5;48m2302991392%s[0m 6.42s] connection: open connection to %s using outbound/%s[%s]: %s\n' \
        "$E" "$E" "$E" "$E" "$1" "${4:-http}" "$2" "$3"
}
rep() { local n="$1"; shift; local i; for ((i = 0; i < n; i++)); do line "$@"; done; }
rep 4 www.google.com:443      resi-1 'unexpected status: 403 Forbidden serp domain'        # ≥3 → learned
rep 2 query.ess.apple.com:443 resi-1 'unexpected status: 403 Forbidden'                    # 2 次 → 不学
rep 3 WWW.TikTok.com:443      resi-2 'unexpected status: 403 Forbidden policy_20050' socks # 大写 + 第二个上游
line 142.251.2.188:5228 resi-1 'unexpected status: 403 Forbidden'                          # port 5228：3 次、2 个 IP
line 142.251.2.188:5228 resi-1 'unexpected status: 403 Forbidden'
line 142.251.2.189:5228 resi-1 'unexpected status: 403 Forbidden'
rep 3 17.57.146.20:5223 resi-1 'unexpected status: 403 Forbidden'                          # 3 次同一 IP → 不聚合
for ip in 1.1.1.1 1.0.0.1 9.9.9.9; do line "$ip:443"  resi-1 'unexpected status: 403 Forbidden';   done   # 443 永不聚合
for ip in 2.2.2.2 3.3.3.3 4.4.4.4; do line "$ip:8443" resi-1 'unexpected status: 502 Bad Gateway'; done   # 8443 永不聚合
rep 3 checkout.stripe.com:443 resi-2 'SOCKS5 connection not allowed by ruleset' socks      # SOCKS5 拒绝形态
rep 5 example.org:443 resi-9 'unexpected status: 403 Forbidden'                            # 未知 tag → 忽略
rep 5 ok.example:443  resi-1 'unexpected status: 302 Found'                                # 2xx/3xx 不算拒绝
printf '%s[36mINFO%s[0m[4007] [123 0.1s] inbound/socks[socks-in]: inbound connection from 127.0.0.1:5555\n' "$E" "$E"
printf -- '-- Journal begins at Tue 2026-09-01 --\n'
```

Generate the fixture and verify it carries ANSI escape bytes:

```bash
mkdir -p tests/resi-blacklist/fixtures
bash tests/resi-blacklist/fixtures/make-journal.sh > tests/resi-blacklist/fixtures/relay-journal.log
wc -l < tests/resi-blacklist/fixtures/relay-journal.log
grep -cF $'\x1b[31mERROR' tests/resi-blacklist/fixtures/relay-journal.log
```

Expected: `36` lines, and `34` lines containing the red `ERROR` prefix (the fixture is committed so the test stays deterministic; regenerate only via this script).

- [ ] **Step 2: Write the failing test `tests/resi-blacklist/test_learn.sh`**


```bash
#!/usr/bin/env bash
# Task 3：learn_from_lines / learn —— 真实形态（含 ANSI）的 relay 日志 → learned（spec §6.2）
set -u
. "$(dirname "$0")/lib.sh"
make_base; BASE="$BASE_DIR"
write_resi_config "$BASE"; write_relay_config "$BASE"
FIX="$REPO_ROOT/tests/resi-blacklist/fixtures/relay-journal.log"
STATE="$BASE/residential-blacklist.json"
. "$SCRIPT"

# 夹具本身带 ANSI
assert_eq 1 "$(grep -c $'\x1b\\[31mERROR' "$FIX" | awk '{print ($1>0)}')" "夹具含 ANSI 颜色码"

# --- 预置：8 天前的候选（一条已在黑名单、一条没有）+ 1 天前的候选 ---
bash "$SCRIPT" status --json >/dev/null 2>&1
jq '.learned["brd.superproxy.io:44445"] = {
      "old.example":  {count: 3, firstSeen: "2026-09-01T00:00:00Z", lastSeen: "2026-09-03T00:00:00Z"},
      "kept.example": {count: 3, firstSeen: "2026-09-01T00:00:00Z", lastSeen: "2026-09-03T00:00:00Z"},
      "fresh.example":{count: 3, firstSeen: "2026-09-10T00:00:00Z", lastSeen: "2026-09-10T00:00:00Z"},
      "www.google.com": {count: 1, firstSeen: "2026-09-05T00:00:00Z", lastSeen: "2026-09-10T00:00:00Z"}}
   | .upstreams["brd.superproxy.io:44445"] = {checkedAt: null, entries: {"kept.example": {kind: "domain"}}}' \
   "$STATE" > "$STATE.t" && mv "$STATE.t" "$STATE"

# --- learn（BL_JOURNAL_CMD 指向夹具）---
BL_JOURNAL_CMD="cat '$FIX'" bash "$SCRIPT" learn >/dev/null 2>&1; assert_eq 0 $? "learn 退出 0"
L1=$(jq -c '.learned["brd.superproxy.io:44445"]' "$STATE")
L2=$(jq -c '.learned["gate.smartproxy.com:7000"]' "$STATE")
assert_eq 4 "$(echo "$L1" | jq '.["www.google.com"].count')" "域名 ≥3 次 → learned（count=本轮次数）"
assert_eq "2026-09-05T00:00:00Z" "$(echo "$L1" | jq -r '.["www.google.com"].firstSeen')" "已有候选保留 firstSeen"
assert_eq "$BL_NOW" "$(echo "$L1" | jq -r '.["www.google.com"].lastSeen')" "lastSeen = 本轮时间"
assert_eq false "$(echo "$L1" | jq 'has("query.ess.apple.com")')" "域名 2 次 → 不学"
assert_eq 3 "$(echo "$L1" | jq '.["port:5228"].count')" "裸 IP 3 次 2 个 IP → port:5228"
assert_eq "142.251.2.189" "$(echo "$L1" | jq -r '.["port:5228"].sampleIp')" "sampleIp = 最近一个 IP"
assert_eq false "$(echo "$L1" | jq 'has("port:5223")')" "3 次但同一 IP → 不聚合"
assert_eq false "$(echo "$L1" | jq 'has("port:443")')" "443 永不聚合"
assert_eq false "$(echo "$L1" | jq 'has("port:8443")')" "8443 永不聚合"
assert_eq false "$(echo "$L1" | jq 'has("ok.example")')" "302 不是拒绝"
assert_eq 3 "$(echo "$L2" | jq '.["www.tiktok.com"].count')" "resi-2 → 第二个上游（主机名转小写）"
assert_eq 3 "$(echo "$L2" | jq '.["checkout.stripe.com"].count')" "SOCKS5 拒绝形态也学"
assert_eq null "$(jq -c '.learned | to_entries | map(select(.value | has("example.org"))) | .[0]' "$STATE")" "未知 tag resi-9 忽略"
assert_eq false "$(echo "$L1" | jq 'has("old.example")')" "7 天未出现且不在黑名单 → 清理"
assert_eq true "$(echo "$L1" | jq 'has("kept.example")')" "7 天未出现但在黑名单 → 保留"
assert_eq true "$(echo "$L1" | jq 'has("fresh.example")')" "1 天前的候选保留"
assert_eq 600 "$(file_mode "$STATE")" "状态文件仍 600"
assert_contains "学习完成：本轮 4 个候选" "$(cat "$BL_LOG")" "日志汇总"

# --- 上限 200：预置 205 条旧候选 + 本轮 → 只留 200 条，最旧的被淘汰 ---
jq '.learned["gate.smartproxy.com:7000"] = ([range(205)] | map({key: ("h\(.).example"), value: {count: 3, firstSeen: "2026-09-08T00:00:00Z", lastSeen: ("2026-09-08T" + (("0" + (. % 24 | tostring)) | .[-2:]) + ":" + (("0" + ((. / 24 | floor) % 60 | tostring)) | .[-2:]) + ":00Z")}}) | from_entries)' \
   "$STATE" > "$STATE.t" && mv "$STATE.t" "$STATE"
BL_JOURNAL_CMD="cat '$FIX'" bash "$SCRIPT" learn >/dev/null 2>&1
assert_eq 200 "$(jq '.learned["gate.smartproxy.com:7000"] | length' "$STATE")" "learned ≤200/上游"
assert_eq true "$(jq '.learned["gate.smartproxy.com:7000"] | has("www.tiktok.com")' "$STATE")" "本轮新学到的（最新 lastSeen）保留"

# --- 边界：日志读取失败 / 空日志 / 无 relay 配置 ---
BL_JOURNAL_CMD="false" bash "$SCRIPT" learn >/dev/null 2>&1; assert_eq 1 $? "日志命令失败 → exit 1（不动状态）"
BL_JOURNAL_CMD="true" bash "$SCRIPT" learn >/dev/null 2>&1;  assert_eq 0 $? "空日志 → exit 0"
rm -f "$BASE/singbox-relay.json"
BL_JOURNAL_CMD="cat '$FIX'" bash "$SCRIPT" learn >/dev/null 2>&1; assert_eq 1 $? "无 relay 配置 → exit 1"
rm -rf "$BASE"
finish
```

- [ ] **Step 3: Run the test and confirm it fails because `learn` is not a command yet**

Run: `bash tests/resi-blacklist/test_learn.sh`
Expected: first lines

```
  ok   - 夹具含 ANSI 颜色码
  FAIL - learn 退出 0
         expected: [0]  actual: [2]
```

(usage error: `learn` unknown), last line `PASS 10 / FAIL 15`, exit 1.

- [ ] **Step 4: Replace the `# ===== [学习] =====` marker with the learn block**

In `server/resi-blacklist.sh`, line 321 currently reads exactly:

```bash
# ===== [学习] =====
```

Replace that single line with:


```bash
# ===== [学习] =====
# relay 日志里的拒绝行（先去 ANSI）。分组：1=目标主机/IP 2=目标端口 3=http|socks 4=resi-N 5=原因
BL_JOURNAL_RE='open connection to ([^ ]+):([0-9]+) using outbound/(http|socks)\[(resi-[0-9]+)\]: (unexpected status: [45][0-9]{2}.*|SOCKS5.*(refused|not allowed).*)$'

# resi-N → host:port（按当前 singbox-relay.json 的住宅出站；tag 本身就是 resi-N），输出 JSON 对象
relay_tag_map_json() {
    [[ -f "$SINGBOX_CONFIG" ]] || { echo '{}'; return 0; }
    jq -c '[.outbounds[] | select(.type == "socks" or .type == "http")
            | {key: .tag, value: ((.server | ascii_downcase) + ":" + (.server_port | tostring))}]
           | from_entries' "$SINGBOX_CONFIG" 2>/dev/null || echo '{}'
}

# learn_from_lines：stdin = 原始 journal 行（可含 ANSI）→ 更新 state.learned（自己加锁）
#   域名：同一（上游，主机名）≥ 3 次 → learned
#   裸 IP：按端口聚合，≥ 3 次且 ≥ 2 个不同 IP → port:N（sampleIp = 最近一个 IP）；80/443/8080/8443 不聚合
#   7 天没再出现且未进黑名单的 learned 清理；每上游最多 200 条（按 lastSeen 淘汰最旧）
learn_from_lines() {
    local esc=$'\x1b' tagmap rows agg now n
    now=$(bl_now)
    tagmap=$(relay_tag_map_json)
    if [[ "$tagmap" == "{}" ]]; then log "WARN relay 配置里没有住宅出站，学习跳过"; return 1; fi
    # bash 正则逐行抽取（12k 行/天量级，几秒内），映射/聚合交给 jq
    rows=$(sed -e "s/${esc}\\[[0-9;]*m//g" | grep -F 'open connection to' | while IFS= read -r line; do
        [[ "$line" =~ $BL_JOURNAL_RE ]] || continue
        printf '%s\t%s\t%s\n' "${BASH_REMATCH[4]}" "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
    done)
    agg=$(printf '%s\n' "$rows" | jq -R -s -c --argjson map "$tagmap" --argjson never "$BL_NEVER_PORTS" '
        split("\n") | map(select(length > 0) | split("\t"))
        | map({up: $map[.[0]], host: (.[1] | ascii_downcase), port: .[2]})
        | map(select(.up != null and (.host | contains("[") | not)))
        | map(if (.host | test("^[0-9]+(\\.[0-9]+){3}$"))
              then select((.port | tonumber) as $p | ($never | any(. == $p) | not))
                   | {up, target: ("port:" + .port), ip: .host}
              else {up, target: .host, ip: null} end)
        | group_by([.up, .target])
        | map({up: .[0].up, target: .[0].target, count: length,
               ips: ([.[].ip | select(. != null)] | unique),
               lastIp: ([.[].ip | select(. != null)] | last)})
        | map(select(if (.target | startswith("port:")) then (.count >= 3 and (.ips | length) >= 2)
                     else .count >= 3 end))') || { log "ERROR 日志聚合失败"; return 1; }
    with_bl_lock bl_update '
        ($now | fromdateiso8601) as $t
        | . as $st
        | reduce $agg[] as $a (.;
            (.learned[$a.up][$a.target] // null) as $cur
            | .learned[$a.up][$a.target] = (
                {count: $a.count, firstSeen: ($cur.firstSeen // $now), lastSeen: $now}
                + (if $a.lastIp != null then {sampleIp: $a.lastIp} else {} end)))
        | .learned |= with_entries(
            .key as $up
            | .value |= (to_entries
                | map(select(.key as $k | .value.lastSeen as $ls
                    | (($st.upstreams[$up].entries // {}) | has($k))
                      or ((($ls // "1970-01-01T00:00:00Z") | fromdateiso8601) >= ($t - 7 * 86400))))
                | sort_by(.value.lastSeen) | reverse | .[:200] | from_entries))
        ' --arg now "$now" --argjson agg "$agg" || return 1
    n=$(printf '%s' "$agg" | jq 'length')
    log "学习完成：本轮 ${n} 个候选达到阈值（$(printf '%s' "$agg" | jq -r 'map("\(.up) \(.target)x\(.count)") | join(", ")')）"
}

cmd_learn() {
    local out rc=0
    out=$(bash -c "$BL_JOURNAL_CMD" 2>/dev/null) || rc=$?
    if [[ "$rc" -ne 0 && -z "$out" ]]; then log "WARN 读取 relay 日志失败(rc=${rc})，本轮不学习"; return 1; fi
    printf '%s\n' "$out" | learn_from_lines
}
```

- [ ] **Step 5: Register `learn` in the dispatch**

In `main`, the two lines (385–386 before this edit) currently read exactly:

```bash
    case "$cmd" in
        status)  cmd_status "$@" ;;
```

Change them to:

```bash
    case "$cmd" in
        learn)   cmd_learn ;;
        status)  cmd_status "$@" ;;
```

Then: `bash -n server/resi-blacklist.sh && echo SYNTAX_OK` → `SYNTAX_OK`.

- [ ] **Step 6: Run the tests**

Run: `bash tests/resi-blacklist/test_learn.sh`
Expected: last line `PASS 25 / FAIL 0`, exit 0.

Regression: `bash tests/resi-blacklist/test_state.sh` → `PASS 32 / FAIL 0`; `bash tests/resi-blacklist/test_probe.sh` → `PASS 61 / FAIL 0`.

- [ ] **Step 7: Commit**


```bash
git add server/resi-blacklist.sh tests/resi-blacklist/fixtures/make-journal.sh tests/resi-blacklist/fixtures/relay-journal.log tests/resi-blacklist/test_learn.sh
git commit -m "feat(residential): resi-blacklist.sh 从 relay 日志学习候选(learn) —— 域名≥3次/端口≥3次且≥2个IP/7天清理

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: 候选表、进出规则、并行探测 —— `candidates_for` / `apply_probe_results` / `effective_json` / `run_probe` / `probe` / `refresh`

**Files:**
- Modify: `server/resi-blacklist.sh:388` (the marker line `# ===== [候选 / 进出规则 / 并行探测] =====`) and `server/resi-blacklist.sh:450-452` (`case "$cmd" in` / `learn)` / `status)` in `main`)
- Create: `tests/resi-blacklist/test_apply.sh`
- Create: `tests/resi-blacklist/test_run_probe.sh`

**Interfaces:**
- Consumes: `candidate_table`, `upstream_cred_cfg`, `cmd_probe_one` (Task 2); `cmd_learn` (Task 3); `bl_read`, `bl_update`, `with_bl_lock`, `bl_state_init`, `apply_via_helper`, `lower`, `log`, `BLACKLIST_SELF`, `BL_PARALLEL`, `BL_NEVER_PORTS` (Task 1); `xargs -P`.
- Produces (exact names): `candidates_for <host:port>` (lines `<probe_spec>\t<target>`: builtin table + that upstream's `learned` (port candidates only when `sampleIp` exists, spec `<sampleIp>:<N>`) + that upstream's existing `kind=domain` entries for re-check; targets pinned `direct` are skipped; deduplicated on target, builtin first), `apply_probe_results <host:port> <results_tsv>` (returns 0 applied / 2 round skipped / 1 error; rules below), `effective_json <host:port>` → `{"resi":[…],"direct":[…],"ports":[…]}`, `bl_set_checking <upstream|all>`, `bl_clear_checking`, `all_upstream_keys`, `run_probe <host:port> [keep_marker]` (sets `checking`, writes candidates to a temp dir, runs `xargs -P $BL_PARALLEL -L 1 bash -c 'bash "$0" _probe-one "$1" "$3" "$2"' "$BLACKLIST_SELF" "$up"`, applies results under the lock, clears `checking`, removes the temp dir; with `keep_marker=1` it leaves `checking` alone), `cmd_probe [<host:port>|all]` (for `all`: sets `checking={"upstream":"all"}`, runs every upstream with `keep_marker=1`, clears; calls `apply_via_helper` **once**, and only when at least one upstream round returned 0 — all locks are released by then), `cmd_refresh` (`cmd_learn` — failure only logged — then `cmd_probe all`).
- `apply_probe_results` rules (spec §5.3): `blocked` ⇒ create `{kind, source, reason, fails:1, okStreak:0, since:now, lastCheck:now}` or update `fails+1, okStreak:0, reason, lastCheck`; `ok` ⇒ existing entry `okStreak+1, lastCheck`, deleted when `okStreak` reaches 2; non-existing target with `ok` ⇒ nothing; `unknown`/`unavailable` ⇒ nothing; if the file has **no** `ok`/`blocked` and ≥1 `unavailable` ⇒ upstream unavailable this round: nothing changes (not even `checkedAt`), return 2; otherwise `checkedAt = now`; port entries for 80/443/8080/8443 never created; result lines whose target fails the target regex are ignored; limits ≤20 port entries and ≤500 entries per upstream, evicting the oldest `lastCheck` (ports are trimmed first, then the total).
- `effective_json` semantics (identical to Section B's `blacklist_rules_json`): `resi` = domains pinned `resi`; `direct` = that upstream's `kind=domain` entries ∪ domains pinned `direct`, minus `resi`; `ports` = that upstream's `kind=port` entries ∪ `port:N` pinned `direct`, as sorted unique integers.

- [ ] **Step 1: Write the failing test `tests/resi-blacklist/test_apply.sh`**


```bash
#!/usr/bin/env bash
# Task 4：apply_probe_results 进出规则（spec §5.3）/ effective_json / candidates_for
set -u
. "$(dirname "$0")/lib.sh"
make_base; BASE="$BASE_DIR"
write_resi_config "$BASE"
STATE="$BASE/residential-blacklist.json"
UP="brd.superproxy.io:44445"
. "$SCRIPT"
bl_state_init
R="$BASE/results.tsv"
entry() { jq -c --arg t "$1" '.upstreams["brd.superproxy.io:44445"].entries[$t]' "$STATE"; }

# --- 进：blocked 新建条目 ---
printf 'www.google.com\tblocked\tHTTP/1.1 403 Forbidden serp domain\tbuiltin:search\nport:5228\tblocked\t403\tbuiltin:push\nwww.bing.com\tunknown\t超时\tbuiltin:search\nx.com\tunavailable\t407\tbuiltin:social\n' > "$R"
apply_probe_results "$UP" "$R"; assert_eq 0 $? "apply 返回 0"
assert_eq '{"kind":"domain","source":"builtin:search","reason":"HTTP/1.1 403 Forbidden serp domain","fails":1,"okStreak":0,"since":"2026-09-11T04:00:00Z","lastCheck":"2026-09-11T04:00:00Z"}' "$(entry www.google.com)" "blocked → 新条目字段完整"
assert_eq port "$(entry port:5228 | jq -r .kind)" "port:N → kind=port"
assert_eq null "$(entry www.bing.com)" "unknown 不建条目"
assert_eq null "$(entry x.com)" "unavailable 不建条目（有 blocked 时本轮仍应用）"
assert_eq "$BL_NOW" "$(jq -r '.upstreams["brd.superproxy.io:44445"].checkedAt' "$STATE")" "checkedAt 已设"

# --- 留：unknown 不动；blocked 再次命中 fails+1 ---
printf 'www.google.com\tunknown\t超时\tbuiltin:search\nport:5228\tblocked\t403 again\tbuiltin:push\n' > "$R"
BL_NOW="2026-09-12T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq 1 "$(entry www.google.com | jq .fails)" "unknown → 条目不变"
assert_eq 2 "$(entry port:5228 | jq .fails)" "再次 blocked → fails=2"
assert_eq "403 again" "$(entry port:5228 | jq -r .reason)" "reason 更新"
assert_eq "2026-09-12T04:00:00Z" "$(entry port:5228 | jq -r .lastCheck)" "lastCheck 更新"
assert_eq "2026-09-11T04:00:00Z" "$(entry port:5228 | jq -r .since)" "since 不变"

# --- 出：连续两次 ok 才移除；中途 blocked 归零 ---
printf 'www.google.com\tok\t\tbuiltin:search\n' > "$R"
BL_NOW="2026-09-13T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq 1 "$(entry www.google.com | jq .okStreak)" "第一次 ok → okStreak=1，仍在"
printf 'www.google.com\tblocked\t403\tbuiltin:search\n' > "$R"
BL_NOW="2026-09-14T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq 0 "$(entry www.google.com | jq .okStreak)" "又被拒 → okStreak 归零"
printf 'www.google.com\tok\t\tbuiltin:search\n' > "$R"
BL_NOW="2026-09-15T04:00:00Z" apply_probe_results "$UP" "$R"
BL_NOW="2026-09-16T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq null "$(entry www.google.com)" "连续两次 ok → 移除"
printf 'never.example\tok\t\tlearned\n' > "$R"
apply_probe_results "$UP" "$R"
assert_eq null "$(entry never.example)" "不在黑名单的目标 ok → 不建条目"

# --- 上游整体不可用：只有 unavailable/unknown → 返回 2，什么都不改 ---
before=$(jq -c . "$STATE")
printf 'www.google.com\tunavailable\t连不上代理(7)\tbuiltin:search\nport:5228\tunknown\t超时\tbuiltin:push\n' > "$R"
BL_NOW="2026-09-20T04:00:00Z" apply_probe_results "$UP" "$R"; assert_eq 2 $? "整体不可用 → 返回 2"
assert_eq "$before" "$(jq -c . "$STATE")" "整体不可用 → 状态逐字节不变（checkedAt 也不动）"
printf 'a.example\tunknown\t\tlearned\n' > "$R"
BL_NOW="2026-09-21T04:00:00Z" apply_probe_results "$UP" "$R"; assert_eq 0 $? "只有 unknown（无 unavailable）→ 正常应用"
assert_eq "2026-09-21T04:00:00Z" "$(jq -r '.upstreams["brd.superproxy.io:44445"].checkedAt' "$STATE")" "checkedAt 更新"
apply_probe_results "$UP" /nonexistent >/dev/null 2>&1; assert_eq 1 $? "结果文件不存在 → 1"

# --- 永不建 80/443/8080/8443 端口条目；非法 target 行忽略 ---
printf 'port:443\tblocked\t403\tlearned\nport:8080\tblocked\t403\tlearned\nbad target\tblocked\t403\tlearned\n' > "$R"
apply_probe_results "$UP" "$R"
assert_eq null "$(entry port:443)" "port:443 永不建条目"
assert_eq null "$(entry port:8080)" "port:8080 永不建条目"
assert_eq 1 "$(jq '.upstreams["brd.superproxy.io:44445"].entries | length' "$STATE")" "非法 target 忽略（只剩 port:5228）"

# --- 上限：端口 ≤20、总数 ≤500（淘汰 lastCheck 最旧）---
: > "$R"
for i in $(seq 1000 1024); do printf 'port:%s\tblocked\t403\tlearned\n' "$i" >> "$R"; done
BL_NOW="2026-09-22T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq 20 "$(jq '[.upstreams["brd.superproxy.io:44445"].entries[] | select(.kind=="port")] | length' "$STATE")" "端口条目 ≤20"
assert_eq null "$(entry port:5228)" "最旧的端口条目（port:5228）被淘汰"
: > "$R"
for i in $(seq 1 520); do printf 'h%s.example\tblocked\t403\tlearned\n' "$i" >> "$R"; done
BL_NOW="2026-09-23T04:00:00Z" apply_probe_results "$UP" "$R"
assert_eq 500 "$(jq '.upstreams["brd.superproxy.io:44445"].entries | length' "$STATE")" "总条目 ≤500"
assert_eq 0 "$(jq '[.upstreams["brd.superproxy.io:44445"].entries[] | select(.kind=="port")] | length' "$STATE")" "总数上限淘汰的是更旧的端口条目"

# --- effective_json：钉住优先 ---
echo '{"version":1,"upstreams":{"brd.superproxy.io:44445":{"checkedAt":null,"entries":{
  "www.google.com":{"kind":"domain"},"gateway.icloud.com":{"kind":"domain"},"port:5228":{"kind":"port"},"port:5223":{"kind":"port"}}}},
 "learned":{},"pins":{"www.google.com":"resi","example.com":"direct","port:9999":"direct","port:5228":"direct","zz.example":"resi"},"checking":null,"applied":null}' | bl_write
assert_eq '{"resi":["www.google.com","zz.example"],"direct":["example.com","gateway.icloud.com"],"ports":[5223,5228,9999]}' "$(effective_json "$UP")" "effective_json：resi 钉住优先于条目；direct 钉住并入；端口整数升序去重"
assert_eq '{"resi":["www.google.com","zz.example"],"direct":["example.com"],"ports":[5228,9999]}' "$(effective_json other.example:1)" "无条目的上游只剩钉住"

# --- candidates_for：内置表 + learned + 现有域名条目，去重，跳过钉住 direct ---
echo '{"version":1,"upstreams":{"brd.superproxy.io:44445":{"checkedAt":null,"entries":{
  "old-entry.example":{"kind":"domain"},"port:5228":{"kind":"port"},"port:7777":{"kind":"port"}}}},
 "learned":{"brd.superproxy.io:44445":{"gateway.icloud.com":{"count":9},"learned.example":{"count":3},
   "port:13861":{"count":5,"sampleIp":"1.2.3.4"},"port:5555":{"count":5}},
   "gate.smartproxy.com:7000":{"other.example":{"count":3}}},
 "pins":{"www.bing.com":"direct","port:5223":"direct"},"checking":null,"applied":null}' | bl_write
C=$(candidates_for "$UP")
assert_eq 1 "$(echo "$C" | grep -c $'^www.google.com\twww.google.com$')" "内置域名候选"
assert_eq 1 "$(echo "$C" | grep -c $'^mtalk.google.com:5228\tport:5228$')" "内置端口候选（probe_spec=真实主机:端口）"
assert_eq 1 "$(echo "$C" | grep -c $'^learned.example\tlearned.example$')" "learned 域名候选"
assert_eq 1 "$(echo "$C" | grep -c $'^1.2.3.4:13861\tport:13861$')" "learned 端口候选用 sampleIp"
assert_eq 0 "$(echo "$C" | grep -c 'port:5555')" "无 sampleIp 的端口候选跳过"
assert_eq 1 "$(echo "$C" | grep -c $'^old-entry.example\told-entry.example$')" "现有域名条目复测"
assert_eq 0 "$(echo "$C" | grep -c 'port:7777')" "无探测主机的端口条目不复测"
assert_eq 1 "$(echo "$C" | grep -c 'gateway.icloud.com')" "内置与 learned 重复只出一次"
assert_eq 0 "$(echo "$C" | grep -c 'www.bing.com')" "钉住 direct 的域名不探测"
assert_eq 0 "$(echo "$C" | grep -c 'port:5223')" "钉住 direct 的端口不探测"
assert_eq 0 "$(echo "$C" | grep -c 'other.example')" "别的上游的 learned 不混入"
assert_eq 28 "$(echo "$C" | wc -l | tr -d ' ')" "候选总数 = 27 内置 - 2 钉住 + 1 learned 域名(另一条与内置重复) + 1 learned 端口 + 1 复测"
rm -rf "$BASE"
finish
```

- [ ] **Step 2: Write the failing end-to-end test `tests/resi-blacklist/test_run_probe.sh`**


```bash
#!/usr/bin/env bash
# Task 4：probe / refresh 端到端（stub curl + stub timeout + stub helper）
set -u
. "$(dirname "$0")/lib.sh"
make_base; BASE="$BASE_DIR"
write_resi_config "$BASE"; write_relay_config "$BASE"; write_stub_helper "$BASE"
export PATH="$REPO_ROOT/tests/resi-blacklist/stubs:$PATH"
export BL_SKIP_APPLY=0 STUB_DIRECT_SCENARIO=ok200 STUB_TCP_EXIT=0
export BL_CANDIDATES_FILE="$BASE/cands.txt"
printf 'search www.google.com www.google.com\nsearch www.bing.com www.bing.com\npush mtalk.google.com:5228 port:5228\n' > "$BL_CANDIDATES_FILE"
STATE="$BASE/residential-blacklist.json"
UP="brd.superproxy.io:44445"
helper_calls() { cat "$BASE/helper.log" 2>/dev/null | wc -l | tr -d ' '; }
entries() { jq -c --arg u "$1" '.upstreams[$u].entries | keys' "$STATE"; }

# --- 未知上游 → exit 1，不调 helper ---
STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" probe nope.example:1 >/dev/null 2>&1; assert_eq 1 $? "未知上游 → exit 1"
assert_eq 0 "$(helper_calls)" "未知上游不调 helper"

# --- 单上游：全部被拒 → 3 条进黑名单，helper 调一次 ---
STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1; assert_eq 0 $? "probe 退出 0"
assert_eq '["port:5228","www.bing.com","www.google.com"]' "$(entries "$UP")" "三个候选都进黑名单"
assert_eq "builtin:push" "$(jq -r '.upstreams["brd.superproxy.io:44445"].entries["port:5228"].source' "$STATE")" "端口条目 source"
assert_eq "$BL_NOW" "$(jq -r '.upstreams["brd.superproxy.io:44445"].checkedAt' "$STATE")" "checkedAt 已设"
assert_eq null "$(jq '.checking' "$STATE")" "结束后 checking 清空"
assert_eq 1 "$(helper_calls)" "helper 调用一次"
assert_eq "blacklist-apply" "$(cat "$BASE/helper.log")" "调用的是 blacklist-apply"
assert_eq 600 "$(file_mode "$STATE")" "状态文件 600"
assert_contains "开始探测 3 个目标" "$(cat "$BL_LOG")" "日志记录候选数"
assert_eq 0 "$(jq '.upstreams["gate.smartproxy.com:7000"].entries // {} | length' "$STATE")" "另一上游未被碰"

# --- 上游不可用 → 不改条目、不调 helper、checkedAt 不动 ---
before=$(jq -c . "$STATE")
BL_NOW="2026-09-12T04:00:00Z" STUB_CURL_SCENARIO=proxydown7 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1; assert_eq 0 $? "上游不可用 → 仍退出 0（不是错误）"
assert_eq "$before" "$(jq -c . "$STATE")" "上游不可用 → 状态不变"
assert_eq 1 "$(helper_calls)" "上游不可用 → 不调 helper"

# --- 能通两轮 → 出黑名单 ---
BL_NOW="2026-09-13T04:00:00Z" STUB_CURL_SCENARIO=ok200 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1
assert_eq '["port:5228","www.bing.com","www.google.com"]' "$(entries "$UP")" "第一轮 ok → 仍在（okStreak=1）"
BL_NOW="2026-09-14T04:00:00Z" STUB_CURL_SCENARIO=ok200 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1
assert_eq '[]' "$(entries "$UP")" "第二轮 ok → 移除"
assert_eq 3 "$(helper_calls)" "每轮有效探测后都调 helper（由 helper 按摘要决定是否重启）"

# --- probe all：两个上游都探，checking 标记为 all，helper 一次 ---
rm -f "$BASE/helper.log"
BL_NOW="2026-09-15T04:00:00Z" STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" probe all >/dev/null 2>&1; assert_eq 0 $? "probe all 退出 0"
assert_eq 3 "$(jq '.upstreams["brd.superproxy.io:44445"].entries | length' "$STATE")" "上游 1 有条目"
assert_eq 3 "$(jq '.upstreams["gate.smartproxy.com:7000"].entries | length' "$STATE")" "上游 2 有条目"
assert_eq null "$(jq '.checking' "$STATE")" "probe all 结束后 checking 清空"
assert_eq 1 "$(helper_calls)" "probe all 只调 helper 一次"

# --- 探测期间 checking 标记可见（用一个慢 worker 观察）---
rm -f "$BASE/helper.log"
cat > "$BASE/slowcurl" <<'EOF'
#!/usr/bin/env bash
sleep 2; exec "$STUB_REAL_CURL" "$@"
EOF
chmod +x "$BASE/slowcurl"
mkdir -p "$BASE/slowbin"; cp "$BASE/slowcurl" "$BASE/slowbin/curl"
export STUB_REAL_CURL="$REPO_ROOT/tests/resi-blacklist/stubs/curl"
( PATH="$BASE/slowbin:$PATH" STUB_CURL_SCENARIO=ok200 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1 ) &
sleep 1
assert_eq "$UP" "$(jq -r '.checking.upstream' "$STATE")" "探测中 checking.upstream = 上游"
wait
assert_eq null "$(jq '.checking' "$STATE")" "探测完 checking 清空"

# --- BL_SKIP_APPLY=1 → 不调 helper；helper 失败 → exit 1 ---
rm -f "$BASE/helper.log"
BL_SKIP_APPLY=1 STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1
assert_eq 0 "$(helper_calls)" "BL_SKIP_APPLY=1 不调 helper"
STUB_HELPER_EXIT=1 STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" probe "$UP" >/dev/null 2>&1; assert_eq 1 $? "helper 失败 → exit 1"

# --- refresh = learn + probe all + apply ---
rm -f "$BASE/helper.log"
FIX="$REPO_ROOT/tests/resi-blacklist/fixtures/relay-journal.log"
BL_NOW="2026-09-16T04:00:00Z" BL_JOURNAL_CMD="cat '$FIX'" STUB_CURL_SCENARIO=refused403 bash "$SCRIPT" refresh >/dev/null 2>&1; assert_eq 0 $? "refresh 退出 0"
assert_eq 4 "$(jq '.learned["brd.superproxy.io:44445"]["www.google.com"].count' "$STATE")" "refresh 先学习"
assert_eq true "$(jq '.upstreams["brd.superproxy.io:44445"].entries | has("port:5228")' "$STATE")" "学到的 port:5228 被探测并进黑名单"
assert_eq true "$(jq '.upstreams["gate.smartproxy.com:7000"].entries | has("checkout.stripe.com")' "$STATE")" "resi-2 学到的 checkout.stripe.com 进上游 2 的黑名单"
assert_eq 1 "$(helper_calls)" "refresh 只调 helper 一次"
BL_JOURNAL_CMD="false" STUB_CURL_SCENARIO=ok200 bash "$SCRIPT" refresh >/dev/null 2>&1; assert_eq 0 $? "学习失败不阻断 refresh"

# --- 空池 ---
echo '{"enabled":false,"urls":[]}' > "$BASE/residential-proxy.json"
rm -f "$BASE/helper.log"
bash "$SCRIPT" probe all >/dev/null 2>&1; assert_eq 0 $? "空池 probe all → 0"
assert_eq 0 "$(helper_calls)" "空池不调 helper"
rm -rf "$BASE"
finish
```

- [ ] **Step 3: Run both tests and confirm they fail because the functions/commands do not exist yet**

Run: `bash tests/resi-blacklist/test_apply.sh`
Expected: first lines

```
tests/resi-blacklist/test_apply.sh: line 16: apply_probe_results: command not found
  FAIL - apply 返回 0
         expected: [0]  actual: [127]
```

last line `PASS 13 / FAIL 28`, exit 1.

Run: `bash tests/resi-blacklist/test_run_probe.sh`
Expected: last line `PASS 4 / FAIL 31`, exit 1 (`probe` is a usage error → exit 2, so `未知上游 → exit 1` fails first).

- [ ] **Step 4: Replace the `# ===== [候选 / 进出规则 / 并行探测] =====` marker with the block**

In `server/resi-blacklist.sh`, line 388 currently reads exactly:

```bash
# ===== [候选 / 进出规则 / 并行探测] =====
```

Replace that single line with:


```bash
# ===== [候选 / 进出规则 / 并行探测] =====
# candidates_for <host:port>：输出 "<probe_spec>\t<target>"（内置表 + 该上游 learned + 已在黑名单的域名条目复测）
#   钉住为 direct 的目标无需探测；同一 target 只出现一次（内置表优先）。
#   端口条目复测的探测主机：内置表有则用内置表的；否则用 learned 的 sampleIp（learned 在条目存续期间不清理）
candidates_for() {
    local up pinned
    up=$(lower "$1")
    pinned=$(bl_read | jq -r '.pins | to_entries[] | select(.value == "direct") | .key' | paste -sd ' ' -)
    {
        candidate_table | awk -F'\t' '{ print $2 "\t" $3 }'
        bl_read | jq -r --arg up "$up" '
            (.learned[$up] // {}) | to_entries[]
            | if (.key | startswith("port:"))
              then (if .value.sampleIp then "\(.value.sampleIp):\(.key[5:])\t\(.key)" else empty end)
              else "\(.key)\t\(.key)" end'
        bl_read | jq -r --arg up "$up" '
            (.upstreams[$up].entries // {}) | to_entries[] | select(.value.kind == "domain") | "\(.key)\t\(.key)"'
    } | awk -F'\t' -v pins="$pinned" '
        BEGIN { n = split(pins, a, " "); for (i = 1; i <= n; i++) skip[a[i]] = 1 }
        NF == 2 && !skip[$2] && !seen[$2]++'
}

# apply_probe_results <host:port> <results_tsv>（results 行："<target>\t<verdict>\t<reason>\t<source>"）
#   blocked：新建条目（fails=1）或 fails+1、okStreak 归零；ok：okStreak+1，≥2 删除；unknown/unavailable 不动
#   全部结果里没有 ok/blocked 且有 unavailable → 上游整体不可用，本轮不改任何条目、不设 checkedAt，返回 2
#   上限：entries ≤500、端口条目 ≤20（按 lastCheck 淘汰最旧）；80/443/8080/8443 永不建端口条目
#   调用方持锁（run_probe 里经 with_bl_lock），本函数自己不加锁
apply_probe_results() {
    local up res now n_ok n_blocked n_unavail n_unknown
    up=$(lower "$1")
    [[ -f "${2:-}" ]] || { log "ERROR 结果文件不存在: ${2:-}"; return 1; }
    now=$(bl_now)
    res=$(jq -R -s -c 'split("\n") | map(select(length > 0) | split("\t")
            | {target: .[0], verdict: .[1], reason: (.[2] // ""), source: (.[3] // "learned")})
            | map(select(.target | test("^([a-z0-9-]+\\.)+[a-z0-9-]+$|^port:[0-9]{1,5}$")))' "$2") \
        || { log "ERROR 结果文件解析失败"; return 1; }
    n_ok=$(printf '%s' "$res" | jq '[.[] | select(.verdict == "ok")] | length')
    n_blocked=$(printf '%s' "$res" | jq '[.[] | select(.verdict == "blocked")] | length')
    n_unavail=$(printf '%s' "$res" | jq '[.[] | select(.verdict == "unavailable")] | length')
    n_unknown=$(printf '%s' "$res" | jq '[.[] | select(.verdict == "unknown")] | length')
    if [[ "$n_unavail" -gt 0 && "$n_ok" -eq 0 && "$n_blocked" -eq 0 ]]; then
        log "上游 ${up} 本轮整体不可用（${n_unavail} 个目标 unavailable），不改任何条目"
        return 2
    fi
    bl_update '
        .upstreams[$up] //= {checkedAt: null, entries: {}}
        | reduce $res[] as $r (.;
            $r.target as $t
            | (.upstreams[$up].entries[$t]) as $cur
            | if $r.verdict == "blocked" then
                if ($t | startswith("port:")) and ($never | any(. == ($t[5:] | tonumber))) then .
                elif $cur == null then
                    .upstreams[$up].entries[$t] = {
                        kind: (if ($t | startswith("port:")) then "port" else "domain" end),
                        source: $r.source, reason: $r.reason, fails: 1, okStreak: 0, since: $now, lastCheck: $now}
                else
                    .upstreams[$up].entries[$t] += {fails: (($cur.fails // 0) + 1), okStreak: 0, reason: $r.reason, lastCheck: $now}
                end
              elif $r.verdict == "ok" and $cur != null then
                if (($cur.okStreak // 0) + 1) >= 2 then del(.upstreams[$up].entries[$t])
                else .upstreams[$up].entries[$t] += {okStreak: (($cur.okStreak // 0) + 1), lastCheck: $now} end
              else . end)
        | .upstreams[$up].checkedAt = $now
        | .upstreams[$up].entries |= (to_entries
            | (map(select(.value.kind == "port")) | sort_by(.value.lastCheck) | reverse | .[:20]) as $ports
            | (map(select(.value.kind != "port"))) as $doms
            | ($doms + $ports) | sort_by(.value.lastCheck) | reverse | .[:500] | from_entries)
        ' --arg up "$up" --arg now "$now" --argjson res "$res" --argjson never "$BL_NEVER_PORTS" || return 1
    log "上游 ${up} 探测完成：blocked=${n_blocked} ok=${n_ok} unknown=${n_unknown} unavailable=${n_unavail}，当前条目 $(bl_read | jq --arg up "$up" '.upstreams[$up].entries | length') 条"
}

# effective_json <host:port> → {"resi":[...],"direct":[...],"ports":[...]}（helper 的 blacklist_rules_json 同语义）
#   resi   = 钉住为 resi 的域名
#   direct = 该上游 kind=domain 的条目 + 钉住为 direct 的域名，减去钉住为 resi 的
#   ports  = 该上游 kind=port 的条目 + 钉住为 direct 的 port:N，整数、去重、升序
effective_json() {
    local up
    up=$(lower "$1")
    bl_read | jq -c --arg up "$up" '
        (.pins // {}) as $pins
        | ([$pins | to_entries[] | select(.value == "resi" and (.key | startswith("port:") | not)) | .key] | unique) as $resi
        | (.upstreams[$up].entries // {}) as $e
        | ([($e | to_entries[] | select(.value.kind == "domain") | .key),
            ($pins | to_entries[] | select(.value == "direct" and (.key | startswith("port:") | not)) | .key)]
           | unique | map(select(. as $x | ($resi | any(. == $x) | not)))) as $direct
        | ([($e | to_entries[] | select(.value.kind == "port") | .key),
            ($pins | to_entries[] | select(.value == "direct" and (.key | startswith("port:"))) | .key)]
           | map(.[5:] | tonumber) | unique) as $ports
        | {resi: $resi, direct: $direct, ports: $ports}'
}

bl_set_checking() { bl_update '.checking = {upstream: $u, startedAt: $now}' --arg u "$1" --arg now "$(bl_now)"; }
bl_clear_checking() { bl_update '.checking = null'; }

# 池里全部上游键 "host:port"（小写），每行一个
all_upstream_keys() {
    [[ -f "$RESIDENTIAL_CONFIG" ]] || return 0
    jq -r '. as $c
           | (($c.urls // []) | if length > 0 then . else
                (if ($c.host // "") != "" then [{host: $c.host, port: $c.port}] else [] end) end)
           | .[] | "\(.host | ascii_downcase):\(.port)"' "$RESIDENTIAL_CONFIG" 2>/dev/null
}

# run_probe <host:port> [keep_marker]：设 checking → 候选经 xargs -P 并行 _probe-one → 应用结果 → 清 checking
#   keep_marker=1 时不碰 checking（probe all 统一设成 "all"）。返回 0 已更新 / 2 本轮跳过 / 1 错误
run_probe() {
    local up keep="${2:-0}" tmp n rc=0
    up=$(lower "$1")
    upstream_cred_cfg "$up" >/dev/null || { log "ERROR 上游 ${up} 不在 residential-proxy.json 中"; return 1; }
    [[ "$keep" == "1" ]] || with_bl_lock bl_set_checking "$up"
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/bl-run.XXXXXX") || { log "ERROR mktemp 失败"; return 1; }
    candidates_for "$up" > "$tmp/cands.tsv"
    n=$(grep -c . "$tmp/cands.tsv" 2>/dev/null); n=${n:-0}
    if [[ "$n" -eq 0 ]]; then
        log "上游 ${up} 没有候选目标"
        : > "$tmp/results.tsv"
    else
        log "上游 ${up}: 开始探测 ${n} 个目标（并发 ${BL_PARALLEL}，单次超时 ${BL_PROBE_TIMEOUT}s）"
        # 每行 "probe_spec target" → worker 参数 (_probe-one up target probe_spec)；worker 自己写 BL_LOG
        xargs -P "$BL_PARALLEL" -L 1 bash -c 'bash "$0" _probe-one "$1" "$3" "$2"' "$BLACKLIST_SELF" "$up" \
            < "$tmp/cands.tsv" > "$tmp/results.tsv" 2>/dev/null
    fi
    with_bl_lock apply_probe_results "$up" "$tmp/results.tsv" || rc=$?
    [[ "$keep" == "1" ]] || with_bl_lock bl_clear_checking
    rm -rf "$tmp"
    return $rc
}

cmd_probe() {
    local which="${1:-all}" ups up rc=0 any=0 failed=0
    bl_state_init || return 1
    if [[ "$which" == "all" ]]; then
        ups=$(all_upstream_keys)
        [[ -n "$ups" ]] || { log "住宅代理池为空，无需探测"; return 0; }
        with_bl_lock bl_set_checking all
        for up in $ups; do
            run_probe "$up" 1 || rc=$?
            [[ "$rc" -eq 0 ]] && any=1
            [[ "$rc" -eq 1 ]] && failed=1
            rc=0
        done
        with_bl_lock bl_clear_checking
    else
        run_probe "$which" || rc=$?
        [[ "$rc" -eq 0 ]] && any=1
        [[ "$rc" -eq 1 ]] && failed=1
    fi
    # 至少一个上游本轮真的更新了才应用；全部跳过（上游不可用）时保持现状（spec §5.3）
    if [[ "$any" == 1 ]]; then apply_via_helper || failed=1; fi
    return "$failed"
}

cmd_refresh() {
    cmd_learn || log "WARN 学习失败，仍继续探测"
    cmd_probe all
}
```

- [ ] **Step 5: Register `probe` and `refresh` in the dispatch**

In `main`, the lines (450–452 before this edit) currently read exactly:

```bash
    case "$cmd" in
        learn)   cmd_learn ;;
        status)  cmd_status "$@" ;;
```

Change them to:

```bash
    case "$cmd" in
        probe)   cmd_probe "$@" ;;
        learn)   cmd_learn ;;
        refresh) cmd_refresh ;;
        status)  cmd_status "$@" ;;
```

Then: `bash -n server/resi-blacklist.sh && echo SYNTAX_OK` → `SYNTAX_OK`. The final `main` dispatch must be, in order: `probe`, `learn`, `refresh`, `status`, `pin`, `forget`, `_probe-one`, `*`.

- [ ] **Step 6: Run the two new tests**

Run: `bash tests/resi-blacklist/test_apply.sh`
Expected: last line `PASS 41 / FAIL 0`, exit 0 (the `上游 … 探测完成：…` lines on stderr are the script's own log echo).

Run: `bash tests/resi-blacklist/test_run_probe.sh`
Expected: last line `PASS 35 / FAIL 0`, exit 0 (takes ~15 s: the `checking` visibility case runs a deliberately slow worker, and every `probe` round spawns real worker processes).

- [ ] **Step 7: Run the whole suite and the static checks**

```bash
for t in tests/resi-blacklist/test_*.sh; do printf '%s: ' "$t"; bash "$t" 2>/dev/null | tail -1; done
bash -n server/resi-blacklist.sh && echo SYNTAX_OK
command -v shellcheck >/dev/null && shellcheck -S error server/resi-blacklist.sh && echo SHELLCHECK_OK || true
```

Expected:

```
tests/resi-blacklist/test_apply.sh: PASS 41 / FAIL 0
tests/resi-blacklist/test_learn.sh: PASS 25 / FAIL 0
tests/resi-blacklist/test_probe.sh: PASS 61 / FAIL 0
tests/resi-blacklist/test_run_probe.sh: PASS 35 / FAIL 0
tests/resi-blacklist/test_state.sh: PASS 32 / FAIL 0
SYNTAX_OK
```

(and `SHELLCHECK_OK` where shellcheck is installed). Also sanity-check the CLI surface once by hand: `bash server/resi-blacklist.sh` → usage on stderr, exit 2; `BASE_DIR=$(mktemp -d) BL_LOG=/dev/null bash server/resi-blacklist.sh status` → prints `黑名单状态 (version 1)`, `检测中: 无`, `最近应用: 无`, `钉住: `.

- [ ] **Step 8: Commit**


```bash
git add server/resi-blacklist.sh tests/resi-blacklist/test_apply.sh tests/resi-blacklist/test_run_probe.sh
git commit -m "feat(residential): resi-blacklist.sh 候选表/进出规则/并行探测 —— probe/refresh + effective_json

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

**Hand-off notes for Sections B–E (what this section guarantees):**

- `${BASE_DIR}/resi-blacklist.sh probe <host:port>` is safe to launch from the helper via `systemd-run` right after `enable`: it validates the upstream against `residential-proxy.json`, marks `checking`, and calls `residential-helper.sh blacklist-apply` exactly once at the end with no lock held.
- `forget <host:port>` never calls the helper, so the helper may call it while holding `.relay.lock`.
- `pin` calls `blacklist-apply` synchronously; server.js should give it a 60 s timeout (a relay restart is included).
- `effective_json` is the reference for Section B's `blacklist_rules_json` jq; the fixtures in `tests/resi-blacklist/test_apply.sh` ("effective_json：钉住优先…") are the cases it must reproduce.
- The state file is only ever written via `bl_write` (validated, 600, atomic); server.js can read it directly and must treat a `checking.startedAt` older than 10 min as null.

