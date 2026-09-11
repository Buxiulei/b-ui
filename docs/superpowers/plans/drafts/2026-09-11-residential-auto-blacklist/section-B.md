# Section B — `server/residential-helper.sh` / `server/resi-health.sh`（Tasks 5–7）

> Section of the v3.7.0 residential auto-blacklist plan. Spec: `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md` (§3 route order, §7 triggers/apply, §8.2/§8.3 interfaces, §10 safety). Task numbers, names, env vars and JSON shapes follow the shared interface contract; Section A (Tasks 1–4) provides `resi-blacklist.sh`, Section C (Tasks 8+) the panel/release.

**What this section delivers**

- Task 5 — the helper becomes blacklist-aware but behaviour-neutral: a single `install_relay_config` sink that runs `sing-box check` before `mv`, `selected_upstream_key`, `blacklist_rules_json`, and the three optional route rules inserted after the `ip_cidr` rule in `write_singbox_config_residential_multi`. With no state file the generated `singbox-relay.json` is byte-identical to today's (proved by a test that runs the pre-change helper from git).
- Task 6 — the `blacklist-apply` subcommand (digest gate → restart only on change → `applied` record), `ensure_blacklist_timer` / `remove_blacklist_timer`, `spawn_blacklist_probe`, and the hooks in `setup` / `enable` / `enable --add` / `enable --remove` / `disable` / `reapply`.
- Task 7 — `resi-health.sh` calls `residential-helper.sh blacklist-apply` right after a successful selector switch.

**Conventions for this section**

- Repo root is referred to as `$REPO` (the worktree `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5`). All `Modify` line numbers are from the **current** worktree (HEAD `92953c6`, helper identical to v3.6.2). Later steps inside a task shift line numbers; every edit therefore also quotes the exact anchor text — search for the text, not the number.
- Tests live in `tests/resi-blacklist/` (never deployed, not in `version.json`). Run with `bash tests/resi-blacklist/<name>.sh` from `$REPO`. They must pass under macOS `/bin/bash` 3.2: the helper's `reapply` / `blacklist-apply` / `disable` paths do not touch `${x,,}`; only the `enable` paths do (`parse_url`, `verify`), so those cases auto-SKIP without a bash ≥ 4 (`brew install bash` to run them).
- Every stub the tests need (`flock`, `systemctl`, `systemd-run`, `curl`, a fake `${BASE_DIR}/sing-box`, a fake `${BASE_DIR}/resi-blacklist.sh`) is written by the test itself into a `mktemp -d` and prepended to `PATH`. The helper decides the sing-box path from `SINGBOX_BIN="${BASE_DIR}/sing-box"` (`ensure_singbox` returns early when it is executable — line 306), so a fake binary at that path skips the download.
- Real sing-box binaries for `check` (darwin, may be absent on other machines → tests print `SKIP`): `/private/tmp/claude-501/-Users-woo-Desktop-b-ui--claude-worktrees-bui-c-tun-mode-issue-df82e5/df6c5c3f-03e2-4ccb-b279-60c80612f9cf/scratchpad/sbbin/1.13.0/sing-box` and `.../sbbin/1.14.0/sing-box`. Both were run against the exact rule shapes below (`domain_suffix` + `port` rules after the `ip_cidr` rule) and pass `check`.
- Shell style: production runs Ubuntu/Debian/CentOS; helper keeps `set -euo pipefail`. Note the errexit trap: a function invoked inside `||` / `if` runs with errexit *off*, so every failure inside `write_singbox_config_*` must be propagated with explicit `|| return 1` (Task 5 Step 9 does exactly that — without it `blacklist-apply`'s `write_singbox_config_from_state || exit 1` would mask a failed `sing-box check`).
- Commits: one per task, message in Chinese conventional style, ending with a blank line and `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Commit only, do not push.

---

### Task 5: `install_relay_config` sing-box check gate + `selected_upstream_key` + `blacklist_rules_json` + rule insertion

**Files:**
- Create: `tests/resi-blacklist/lib.sh` (only if Section A has not already created it — same content)
- Create: `tests/resi-blacklist/test_helper_rules.sh`
- Modify: `server/residential-helper.sh:41` (append `BLACKLIST_*` vars after `RELAY_LOCK=`)
- Modify: `server/residential-helper.sh:65-67` (insert `install_relay_config()` between `file_digest()` and the `# v3.6.0 R3: curl 配置文件…` comment)
- Modify: `server/residential-helper.sh:346` (insert `selected_upstream_key()` + `blacklist_rules_json()` before `write_singbox_config_residential()`)
- Modify: `server/residential-helper.sh:402` (`--arg cache` line: add `--argjson bl`), `:446-450` (rules array: insert three optional rules), `:455-458` (tail: `install_relay_config`)
- Modify: `server/residential-helper.sh:500-503` (`write_singbox_config_direct` tail: `install_relay_config`)
- Modify: `server/residential-helper.sh:609,612,616` (`write_singbox_config_from_state`: propagate failures)

**Interfaces:**
- Consumes: `BLACKLIST_STATE` JSON (contract §4: `.upstreams[<host:port>].entries[<target>].kind`, `.pins`), `RESIDENTIAL_CONFIG` via existing `build_urls_json_from_config` (line 578), Clash API `GET http://127.0.0.1:9091/proxies/resi-pool` `.now`, existing `SINGBOX_BIN`, `SINGBOX_CONFIG`, `err`/`info`.
- Produces (bash functions in `server/residential-helper.sh`):
  - `install_relay_config` — no args; takes `${SINGBOX_CONFIG}.tmp`, runs `"${SINGBOX_BIN}" check -c` when the binary is executable, then `chmod 600` + `mv`; on failure removes the tmp, prints `ERROR: relay 新配置未通过 sing-box check…`, returns 1.
  - `selected_upstream_key` — prints `"host:port"` (host lowercased) of the selector's current choice, first pool entry on API failure, `""` when the pool is empty; always exit 0.
  - `blacklist_rules_json <host:port>` — prints one-line JSON `{"resi":[…],"direct":[…],"ports":[…]}`; missing/corrupt state, empty key or unknown upstream ⇒ all empty; always exit 0.
  - Variables `BLACKLIST_STATE`, `BLACKLIST_LOCK`, `BLACKLIST_SCRIPT`, `BLACKLIST_UNIT_BASE="b-ui-resi-blacklist"`, `BLACKLIST_UNIT_DIR` (env override, default `/etc/systemd/system`; Task 6 uses the last three).
  - Route rules in `singbox-relay.json` (residential mode only), in this order right after the `ip_cidr` rule and before the keyword rule: `{"domain_suffix": $bl.resi, "outbound": "resi-pool"}` → `{"domain_suffix": $bl.direct, "outbound": "direct"}` → `{"port": $bl.ports, "outbound": "direct"}`, each omitted when its array is empty.

- [ ] **Step 1: Create the shared assertion library (skip if `tests/resi-blacklist/lib.sh` already exists)**

Create `tests/resi-blacklist/lib.sh`:

```bash
#!/usr/bin/env bash
# tests/resi-blacklist/lib.sh — 极简断言库（无测试框架；bash 3.2 兼容；不进 version.json、不部署）
# 用法：. "$(dirname "$0")/lib.sh"，末尾调 finish（打印 "PASS n / FAIL m"，有失败退出 1）
PASS=0
FAIL=0
pass() { PASS=$((PASS + 1)); }
fail() { FAIL=$((FAIL + 1)); echo "FAIL: $*" >&2; }
# assert_eq <期望> <实际> <说明>
assert_eq() { if [[ "$1" == "$2" ]]; then pass; else fail "$3 —— 期望 [$1] 实际 [$2]"; fi; }
# assert_contains <期望子串> <实际> <说明>
assert_contains() { if [[ "$2" == *"$1"* ]]; then pass; else fail "$3 —— 未包含 [$1]，实际 [$2]"; fi; }
# assert_file_exists <路径> <说明>
assert_file_exists() { if [[ -e "$1" ]]; then pass; else fail "$2 —— 文件不存在 $1"; fi; }
finish() {
    echo "PASS ${PASS} / FAIL ${FAIL}"
    [[ "$FAIL" -eq 0 ]] || exit 1
    exit 0
}
```

Argument order is `assert_eq <expected> <actual> <msg>` and `assert_contains <expected-substring> <actual> <msg>`; `finish` prints `PASS n / FAIL m` and exits 1 when `m > 0`.

- [ ] **Step 2: Write the failing test `tests/resi-blacklist/test_helper_rules.sh`**

Note on case (a): it fetches the pre-blacklist helper with `git show 92953c6:server/residential-helper.sh` (the R13 design-doc commit; the helper there is byte-for-byte v3.6.2) and runs both helpers through `reapply` in the same scratch `BASE_DIR`, then `cmp`s the two `singbox-relay.json`. `BASELINE_COMMIT` is an env override only for running the test in a throwaway repo.

```bash
#!/usr/bin/env bash
# tests/resi-blacklist/test_helper_rules.sh — Task 5
#   (a) 无黑名单文件 → singbox-relay.json 与黑名单之前的 helper（基线提交）输出逐字节一致
#   (b) 有 resi / direct / ports → 规则按顺序插入且 sing-box 1.13 / 1.14 check 通过；选中 resi-2 时按该上游取
#   (c) 假 sing-box check 失败 → 旧配置保留、退出 1、无 .tmp 残留
# 在 macOS 上跑：/bin/bash 3.2 即可（reapply 路径不碰 ${x,,}）。systemctl / flock / curl 全部 stub。
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
. "$HERE/lib.sh"

HELPER="$REPO/server/residential-helper.sh"
# 黑名单之前的最后一版 helper（v3.6.2 + R13 设计文档提交）；本地验证可用 BASELINE_COMMIT 覆盖
BASELINE_COMMIT="${BASELINE_COMMIT:-92953c6}"
SBBIN_ROOT="${SBBIN_ROOT:-/private/tmp/claude-501/-Users-woo-Desktop-b-ui--claude-worktrees-bui-c-tun-mode-issue-df82e5/df6c5c3f-03e2-4ccb-b279-60c80612f9cf/scratchpad/sbbin}"

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stubs" "$T/base" "$T/units"

# ---- stubs（PATH 前置）----
cat > "$T/stubs/flock" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
cat > "$T/stubs/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >> "${STUB_LOG:-/dev/null}"
case "$1" in is-active) exit "${STUB_RELAY_ACTIVE_RC:-0}" ;; esac
exit 0
STUB
cat > "$T/stubs/curl" <<'STUB'
#!/usr/bin/env bash
# 假 curl：api.ipify.org → 固定公网 IP；Clash API → STUB_RELAY_NOW_JSON（默认 resi-1）
url=""
for a in "$@"; do case "$a" in http://*|https://*) url="$a" ;; esac; done
case "$url" in
    *api.ipify.org*)     echo "203.0.113.10" ;;
    *proxies/resi-pool*) echo "${STUB_RELAY_NOW_JSON:-{\"now\":\"resi-1\"}}" ;;
esac
exit 0
STUB
chmod +x "$T/stubs/"*

# ---- 假 sing-box（helper 的 ensure_singbox 只看 ${BASE_DIR}/sing-box 是否可执行）----
# check 子命令：FAKE_SB_FAIL=1 → 失败；有真 1.13.0 二进制则转发给它；否则直接成功
SB_REAL="$SBBIN_ROOT/1.13.0/sing-box"
cat > "$T/base/sing-box" <<STUB
#!/usr/bin/env bash
if [[ "\${FAKE_SB_FAIL:-0}" == "1" ]]; then echo "FATAL[0000] fake check failure" >&2; exit 1; fi
if [[ "\${1:-}" == "check" && -x "$SB_REAL" ]]; then exec "$SB_REAL" "\$@"; fi
exit 0
STUB
chmod +x "$T/base/sing-box"

cat > "$T/base/residential-proxy.json" <<'EOF2'
{"enabled":true,"global":true,"domains":null,"urls":[
 {"host":"brd.superproxy.io","port":44445,"username":"u1","password":"p1","type":"http","name":"url-1"},
 {"host":"gw.example.net","port":1080,"username":"u2","password":"p2","name":"url-2"}]}
EOF2

export STUB_LOG="$T/stub.log"
run_helper() {   # run_helper <helper 路径> <子命令...>；stdout → $T/out.txt，stderr → $T/err.txt，返回退出码
    PATH="$T/stubs:$PATH" BASE_DIR="$T/base" RELAY_UNIT_FILE="$T/base/relay.service" \
    BLACKLIST_UNIT_DIR="$T/units" bash "$@" >"$T/out.txt" 2>"$T/err.txt"
}
CFG="$T/base/singbox-relay.json"

# ---- (a) 无黑名单文件：与基线 helper 逐字节一致 ----
git -C "$REPO" show "${BASELINE_COMMIT}:server/residential-helper.sh" > "$T/helper-baseline.sh"
assert_file_exists "$T/helper-baseline.sh" "取基线 helper"
run_helper "$T/helper-baseline.sh" reapply; assert_eq 0 "$?" "基线 helper reapply 退出 0"
cp "$CFG" "$T/baseline.json"
run_helper "$HELPER" reapply; assert_eq 0 "$?" "新 helper reapply 退出 0"
cmp -s "$T/baseline.json" "$CFG"; assert_eq 0 "$?" "(a) 无黑名单文件时配置与基线逐字节一致"
assert_eq 5 "$(jq '.route.rules | length' "$CFG")" "(a) 规则数仍为 5"

# ---- (b) 有黑名单：resi → direct → ports 顺序插在 ip_cidr 之后 ----
cat > "$T/base/residential-blacklist.json" <<'EOF2'
{"version":1,
 "upstreams":{"brd.superproxy.io:44445":{"checkedAt":"2026-09-11T20:05:12Z","entries":{
   "www.google.com":{"kind":"domain","source":"builtin:search","reason":"403 Forbidden serp domain","fails":2,"okStreak":0,"since":"2026-09-11T20:05:12Z","lastCheck":"2026-09-11T20:05:12Z"},
   "port:5228":{"kind":"port","source":"learned","reason":"403 Forbidden","fails":2,"okStreak":0,"since":"2026-09-11T20:05:12Z","lastCheck":"2026-09-11T20:05:12Z"},
   "pinned.example.org":{"kind":"domain","source":"learned","reason":"403","fails":2,"okStreak":0,"since":"2026-09-11T20:05:12Z","lastCheck":"2026-09-11T20:05:12Z"}}}},
 "learned":{},
 "pins":{"example.com":"resi","pinned.example.org":"resi","checkout.stripe.com":"direct","port:5223":"direct"},
 "checking":null,"applied":null}
EOF2
run_helper "$HELPER" reapply; assert_eq 0 "$?" "(b) 有黑名单 reapply 退出 0"
assert_eq 8 "$(jq '.route.rules | length' "$CFG")" "(b) 规则数 5 + 3"
assert_eq '{"domain_suffix":["example.com","pinned.example.org"],"outbound":"resi-pool"}' \
          "$(jq -c '.route.rules[5]' "$CFG")" "(b) 第 6 条 = 钉住住宅"
assert_eq '{"domain_suffix":["checkout.stripe.com","www.google.com"],"outbound":"direct"}' \
          "$(jq -c '.route.rules[6]' "$CFG")" "(b) 第 7 条 = 黑名单域名 + 钉住机房，去掉钉住住宅的"
assert_eq '{"port":[5223,5228],"outbound":"direct"}' \
          "$(jq -c '.route.rules[7]' "$CFG")" "(b) 第 8 条 = 端口（整数、排序）"
assert_eq '"resi-pool"' "$(jq -c '.route.final' "$CFG")" "(b) global 时 final 仍是 resi-pool"
assert_eq '{"ip_cidr"' "$(jq -c '.route.rules[4]' "$CFG" | cut -c1-10)" "(b) 第 5 条仍是 ip_cidr 直连"
for v in 1.13.0 1.14.0; do
    if [[ -x "$SBBIN_ROOT/$v/sing-box" ]]; then
        "$SBBIN_ROOT/$v/sing-box" check -c "$CFG" >/dev/null 2>&1; assert_eq 0 "$?" "(b) sing-box $v check 通过"
    else
        echo "SKIP: 没有 $SBBIN_ROOT/$v/sing-box，跳过真机 check"
    fi
done
# 选中 resi-2（gw.example.net:1080 没有 entries）→ 只剩钉住规则
STUB_RELAY_NOW_JSON='{"now":"resi-2"}' run_helper "$HELPER" reapply; assert_eq 0 "$?" "(b) resi-2 reapply 退出 0"
assert_eq '["checkout.stripe.com"]' "$(jq -c '.route.rules[6].domain_suffix' "$CFG")" "(b) resi-2 的 direct 只有钉住机房"
assert_eq '[5223]' "$(jq -c '.route.rules[7].port' "$CFG")" "(b) resi-2 的端口只有钉住的 5223"

# ---- (c) sing-box check 失败 → 保留旧配置、退出 1、无 .tmp ----
cp "$CFG" "$T/keep.json"
rm -f "$T/base/residential-blacklist.json"    # 本来会回到 5 条规则 → 摘要有变化 → 必须经过 check
FAKE_SB_FAIL=1 run_helper "$HELPER" reapply; assert_eq 1 "$?" "(c) check 失败 reapply 退出 1"
cmp -s "$T/keep.json" "$CFG"; assert_eq 0 "$?" "(c) 旧配置原样保留"
[[ -e "$CFG.tmp" ]]; assert_eq 1 "$?" "(c) 无 .tmp 残留"
assert_contains "未通过 sing-box check" "$(cat "$T/err.txt")" "(c) 报错说明 check 未过"

finish
```

- [ ] **Step 3: Run the test and confirm it fails on the unmodified helper**

Run: `cd $REPO && bash tests/resi-blacklist/test_helper_rules.sh`

Expected (case (a) passes because nothing changed yet; (b) and (c) fail):

```
FAIL: (b) 规则数 5 + 3 —— 期望 [8] 实际 [5]
FAIL: (b) 第 6 条 = 钉住住宅 —— 期望 [{"domain_suffix":["example.com","pinned.example.org"],"outbound":"resi-pool"}] 实际 [null]
FAIL: (b) 第 7 条 = 黑名单域名 + 钉住机房，去掉钉住住宅的 —— 期望 [...] 实际 [null]
FAIL: (b) 第 8 条 = 端口（整数、排序） —— 期望 [{"port":[5223,5228],"outbound":"direct"}] 实际 [null]
FAIL: (b) resi-2 的 direct 只有钉住机房 —— 期望 [["checkout.stripe.com"]] 实际 [null]
FAIL: (b) resi-2 的端口只有钉住的 5223 —— 期望 [[5223]] 实际 [null]
FAIL: (c) check 失败 reapply 退出 1 —— 期望 [1] 实际 [0]
FAIL: (c) 报错说明 check 未过 —— 未包含 [未通过 sing-box check]，实际 [...]
PASS 13 / FAIL 8
```

Exit code 1.

- [ ] **Step 4: Add the `BLACKLIST_*` variables**

`server/residential-helper.sh:41` currently reads:

```bash
RELAY_LOCK="${BASE_DIR}/.relay.lock"
```

Append directly below it:

```bash
# v3.7.0 R13: 机房直出黑名单。状态文件由 resi-blacklist.sh 维护，helper 只读它生成 relay 规则；
# 两把锁各管各的：.relay.lock 管 singbox-relay.json，.blacklist.lock 只管状态文件
BLACKLIST_STATE="${BASE_DIR}/residential-blacklist.json"
BLACKLIST_LOCK="${BASE_DIR}/.blacklist.lock"
BLACKLIST_SCRIPT="${BASE_DIR}/resi-blacklist.sh"
BLACKLIST_UNIT_BASE="b-ui-resi-blacklist"
BLACKLIST_UNIT_DIR="${BLACKLIST_UNIT_DIR:-/etc/systemd/system}"
```

- [ ] **Step 5: Add `install_relay_config()` after `file_digest()`**

`server/residential-helper.sh:60-67` currently reads:

```bash
file_digest() {
    [[ -f "$1" ]] || { echo ""; return 0; }
    local d
    d=$(md5sum "$1" 2>/dev/null | awk '{print $1}') || d=""
    [[ -n "$d" ]] && echo "$d" || echo "nodigest-$$-${RANDOM}"
}

# v3.6.0 R3: curl 配置文件双引号内需转义 \ 与 "
```

Insert between the closing `}` of `file_digest` and the `# v3.6.0 R3: curl 配置文件…` comment:

```bash
# v3.7.0 R13: relay 配置唯一落盘口。.tmp 先过 sing-box check（二进制在才查），不过则丢弃 .tmp、
# 保留旧配置并返回 1 —— 黑名单/钉住来自状态文件，一条坏规则绝不能让 b-ui-relay 起不来。
install_relay_config() {
    local tmp="${SINGBOX_CONFIG}.tmp" check_out
    [[ -s "$tmp" ]] || { err "relay 配置生成失败（${tmp} 为空）"; rm -f "$tmp"; return 1; }
    if [[ -x "${SINGBOX_BIN}" ]]; then
        if ! check_out=$("${SINGBOX_BIN}" check -c "$tmp" 2>&1); then
            err "relay 新配置未通过 sing-box check，保留旧配置：${check_out}"
            rm -f "$tmp"
            return 1
        fi
    fi
    chmod 600 "$tmp" && mv "$tmp" "${SINGBOX_CONFIG}" || { rm -f "$tmp"; return 1; }
    chmod 600 "${SINGBOX_CONFIG}"
}

```

- [ ] **Step 6: Add `selected_upstream_key()` and `blacklist_rules_json()` before `write_singbox_config_residential()`**

`server/residential-helper.sh:346` currently reads:

```bash
write_singbox_config_residential() {
```

Insert above that line:

```bash
# v3.7.0 R13: 当前选中上游 → "host:port"（host 小写）。Clash API 的 .now = resi-N 对应池第 N 条
# （池顺序 = build_urls_json_from_config，与 relay 出站 resi-N 同源）；API 不通/越界取池首；池空输出空串。
selected_upstream_key() {
    local now idx urls
    urls=$(build_urls_json_from_config 2>/dev/null) || urls="[]"
    [[ -n "$urls" ]] || urls="[]"
    now=$(curl -s --max-time 2 "http://${SINGBOX_RELAY_API}/proxies/resi-pool" 2>/dev/null | jq -r '.now // empty' 2>/dev/null || true)
    idx=0
    [[ "$now" =~ ^resi-([0-9]+)$ ]] && idx=$((BASH_REMATCH[1] - 1))
    echo "$urls" | jq -r --argjson i "$idx" '
        if length == 0 then ""
        else (if $i >= 0 and $i < length then .[$i] else .[0] end)
             | ((.host | ascii_downcase) + ":" + (.port | tostring))
        end' 2>/dev/null || echo ""
}

# v3.7.0 R13: 某上游的生效黑名单 → {"resi":[...],"direct":[...],"ports":[...]}（与 resi-blacklist.sh effective_json 同义）
#   resi   = 钉住:强制住宅 的目标
#   direct = 该上游 entries 里 kind=domain 的域名 + 钉住:强制机房 的域名，去掉钉住住宅的
#   ports  = entries 里 kind=port 的 N + 钉住:强制机房 的 port:N，去掉钉住住宅的，整数排序去重
# 状态文件缺失/损坏/上游未知/键为空 → 三者皆空（生成的配置与 v3.6.x 逐字节一致）
blacklist_rules_json() {
    local key="${1:-}" empty='{"resi":[],"direct":[],"ports":[]}'
    [[ -n "$key" && -s "${BLACKLIST_STATE}" ]] || { echo "$empty"; return 0; }
    jq -c --arg k "$key" '
        (.pins // {}) as $pins
        | ((.upstreams[$k].entries) // {}) as $e
        | [$pins | to_entries[] | select(.value == "resi")   | .key] as $pin_resi
        | [$pins | to_entries[] | select(.value == "direct") | .key] as $pin_direct
        | {
            resi:   ($pin_resi | unique),
            direct: (([$e | to_entries[] | select(.value.kind == "domain") | .key]
                      + [$pin_direct[] | select(startswith("port:") | not)]) - $pin_resi | unique),
            ports:  (([$e | to_entries[] | select(.value.kind == "port") | .key]
                      + [$pin_direct[] | select(startswith("port:"))]) - $pin_resi
                     | map(sub("^port:"; "") | tonumber) | unique)
          }' "${BLACKLIST_STATE}" 2>/dev/null || echo "$empty"
}

```

`selected_upstream_key` deliberately goes through `build_urls_json_from_config` rather than `.urls` alone: after `enable <url>` (single-URL mode) `.urls` is `[]` and the upstream lives in the top-level `host`/`port` fields (`save_config`, line 542), and the relay outbound order is built from exactly this function (`write_singbox_config_from_state`, line 603), so `resi-N` ⇒ `urls[N-1]` is the same mapping the relay uses.

- [ ] **Step 7: Wire the rules into `write_singbox_config_residential_multi`**

Three edits inside the function (lines 357-459).

(7a) `server/residential-helper.sh:402` currently reads:

```bash
        --arg  cache      "${BASE_DIR}/relay-cache.db" \
```

Replace with:

```bash
        --arg  cache      "${BASE_DIR}/relay-cache.db" \
        --argjson bl      "$(blacklist_rules_json "$(selected_upstream_key)")" \
```

(7b) `server/residential-helper.sh:446-450` currently reads:

```bash
               {"ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"}]
              + (if $is_global then []
                 else [{"domain_keyword": $kw, "outbound": "resi-pool"}]
                 end)
```

Replace with (the `ip_cidr` lines are unchanged; three `+ (...)` lines are inserted before the `$is_global` one):

```bash
               {"ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"}]
              # v3.7.0 R13: 机房直出黑名单——钉住住宅 → 黑名单+钉住机房 → 黑名单端口；三者空时不生成任何规则
              + (if ($bl.resi   | length) > 0 then [{"domain_suffix": $bl.resi,   "outbound": "resi-pool"}] else [] end)
              + (if ($bl.direct | length) > 0 then [{"domain_suffix": $bl.direct, "outbound": "direct"}]    else [] end)
              + (if ($bl.ports  | length) > 0 then [{"port": $bl.ports,           "outbound": "direct"}]    else [] end)
              + (if $is_global then []
                 else [{"domain_keyword": $kw, "outbound": "resi-pool"}]
                 end)
```

(`#` comments inside a jq program are legal; the file already uses them at lines 405-407.)

(7c) `server/residential-helper.sh:455-459` (end of the function) currently reads:

```bash
        }' > "${SINGBOX_CONFIG}.tmp" \
    && chmod 600 "${SINGBOX_CONFIG}.tmp" \
    && mv "${SINGBOX_CONFIG}.tmp" "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}
```

Replace with:

```bash
        }' > "${SINGBOX_CONFIG}.tmp" \
    || { rm -f "${SINGBOX_CONFIG}.tmp"; err "生成 relay 配置失败（jq）"; return 1; }
    install_relay_config
}
```

- [ ] **Step 8: Route `write_singbox_config_direct` through the same sink**

`server/residential-helper.sh:500-504` currently reads (identical tail to 7c):

```bash
        }' > "${SINGBOX_CONFIG}.tmp" \
    && chmod 600 "${SINGBOX_CONFIG}.tmp" \
    && mv "${SINGBOX_CONFIG}.tmp" "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}
```

Replace with:

```bash
        }' > "${SINGBOX_CONFIG}.tmp" \
    || { rm -f "${SINGBOX_CONFIG}.tmp"; err "生成 relay 配置失败（jq）"; return 1; }
    install_relay_config
}
```

After this step `grep -c 'mv "${SINGBOX_CONFIG}.tmp"' server/residential-helper.sh` must print `1` (only inside `install_relay_config`).

- [ ] **Step 9: Make `write_singbox_config_from_state` propagate failures**

`server/residential-helper.sh:599-619` currently reads:

```bash
write_singbox_config_from_state() {
    if [[ -f "${RESIDENTIAL_CONFIG}" ]] && \
       [[ "$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]]; then
        local urls_json
        urls_json=$(build_urls_json_from_config)
        local cnt
        cnt=$(echo "$urls_json" | jq 'length')
        if [[ "$cnt" -gt 0 ]]; then
            local is_global
            is_global=$(jq -r '.global // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")
            write_singbox_config_residential_multi "$urls_json"
            info "sing-box 配置：住宅代理模式（${cnt} 个 URL，global=${is_global}）"
        else
            write_singbox_config_direct
            info "sing-box 配置：直连模式（urls 为空）"
        fi
    else
        write_singbox_config_direct
        info "sing-box 配置：直连模式"
    fi
}
```

Change the three writer calls (lines 609, 612, 616) so each is followed by `|| return 1`:

```bash
            write_singbox_config_residential_multi "$urls_json" || return 1
            info "sing-box 配置：住宅代理模式（${cnt} 个 URL，global=${is_global}）"
        else
            write_singbox_config_direct || return 1
            info "sing-box 配置：直连模式（urls 为空）"
        fi
    else
        write_singbox_config_direct || return 1
        info "sing-box 配置：直连模式"
    fi
```

Why: with `set -e`, a caller like `write_singbox_config_from_state || exit 1` (Task 6) runs the function with errexit disabled; without the explicit `|| return 1` the trailing `info` would turn a failed `sing-box check` into exit 0.

- [ ] **Step 10: Syntax check and run the test**

Run: `cd $REPO && bash -n server/residential-helper.sh && bash tests/resi-blacklist/test_helper_rules.sh`

Expected:

```
PASS 21 / FAIL 0
```

(exit 0; if the scratchpad binaries are absent you will additionally see two `SKIP: 没有 …/sing-box，跳过真机 check` lines and `PASS 19 / FAIL 0`). If `shellcheck` is installed also run `shellcheck -S error server/residential-helper.sh` (expected: no output).

- [ ] **Step 11: Commit**

```bash
cd $REPO && git add server/residential-helper.sh tests/resi-blacklist/lib.sh tests/resi-blacklist/test_helper_rules.sh && git commit -m "feat(residential): relay 配置落盘前过 sing-box check，按选中上游插入黑名单/钉住路由规则

- install_relay_config：.tmp 先 sing-box check 再 mv，不过则保留旧配置返回 1（两个写入器共用）
- selected_upstream_key：Clash API .now(resi-N) → 池第 N 条 host:port，API 不通取池首
- blacklist_rules_json：从 residential-blacklist.json 算 {resi,direct,ports}，缺文件全空
- write_singbox_config_residential_multi：ip_cidr 之后依次插 钉住住宅/黑名单机房/黑名单端口 规则，三者空时与 v3.6.2 输出逐字节一致
- write_singbox_config_from_state 显式传播写入失败
- tests/resi-blacklist：lib.sh + test_helper_rules.sh（基线逐字节对比、1.13/1.14 check、check 失败保旧）

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: `blacklist-apply` subcommand, timer units, `spawn_blacklist_probe`, and the enable/remove/disable/reapply hooks

**Files:**
- Create: `tests/resi-blacklist/test_helper_apply.sh`
- Modify: `server/residential-helper.sh:657-660` (insert the new function block right before the `# 主入口` banner)
- Modify: `server/residential-helper.sh:664` (lock list gains `blacklist-apply`)
- Modify: `server/residential-helper.sh:671-672` (`setup`: timer)
- Modify: `server/residential-helper.sh:690-692` (`enable --add`: timer + probe)
- Modify: `server/residential-helper.sh:699-701` and `:707-708` (`enable --remove`: `forget` + timer removal when the pool empties)
- Modify: `server/residential-helper.sh:734-737` (`enable <url>`: timer + probe)
- Modify: `server/residential-helper.sh:753` (`disable`: timer removal)
- Modify: `server/residential-helper.sh:792-795` (`reapply`: timer ensure/remove) and add the `blacklist-apply)` case before `*)` at `:854`
- Modify: `server/residential-helper.sh:855` (usage line)

(Line numbers are pre-Task-5; after Task 5 they sit ~85 lines lower. Use the quoted anchors.)

**Interfaces:**
- Consumes: Task 5's `install_relay_config`, `selected_upstream_key`, `blacklist_rules_json`, `BLACKLIST_*`; existing `file_digest`, `acquire_relay_lock` (fd 9), `build_urls_json_from_config`, `RELAY_SERVICE`, `SINGBOX_CONFIG`; `resi-blacklist.sh probe <host:port>` and `forget <host:port>` (Section A) with the contract env `BL_SKIP_APPLY=1`; `systemd-run`, `systemctl`, `flock`.
- Produces:
  - Subcommand `residential-helper.sh blacklist-apply` — exit 0 when the relay config was (re)written (restarted only if the md5 changed and `b-ui-relay` is active), exit 1 when generation/`sing-box check` failed (old config and old `applied` kept). stderr messages: `黑名单有变化，b-ui-relay 已重启` / `黑名单无变化，保持运行` / `黑名单有变化，配置已写入（b-ui-relay 未在运行，下次启动生效）`. Writes `BLACKLIST_STATE.applied = {upstream, digest, at}` under `BLACKLIST_LOCK` (fd 8, `flock -w 30`), creating the empty skeleton first if the state file is missing.
  - `ensure_blacklist_timer` / `remove_blacklist_timer` — write/remove `${BLACKLIST_UNIT_DIR}/b-ui-resi-blacklist.service` and `.timer` (heredocs below), `daemon-reload`, `enable --now` / `disable --now`; idempotent (no write, no reload when unchanged).
  - `spawn_blacklist_probe <host:port>` — `systemd-run --unit b-ui-resi-blacklist-probe-$(date +%s) --quiet "${BLACKLIST_SCRIPT}" probe <host:port>`, `nohup … &` fallback; never fails the caller; writes nothing to stdout.
  - `pool_active` (exit 0 when `enabled=true` and the pool is non-empty), `write_blacklist_applied <host:port> <digest>`, `lower <string>`.
  - New env override `BLACKLIST_UNIT_DIR` (default `/etc/systemd/system`) for tests.

- [ ] **Step 1: Write the failing test `tests/resi-blacklist/test_helper_apply.sh`**

```bash
#!/usr/bin/env bash
# tests/resi-blacklist/test_helper_apply.sh — Task 6
#   blacklist-apply：摘要变了才 restart、applied 回写、check 失败退出 1 且 applied 不动
#   reapply / disable：b-ui-resi-blacklist.{service,timer} 的安装（幂等）与移除
#   enable --add / --remove 钩子：systemd-run 启动 probe、resi-blacklist.sh forget（需要 bash ≥ 4，缺则 SKIP）
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
. "$HERE/lib.sh"
HELPER="$REPO/server/residential-helper.sh"

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stubs" "$T/base" "$T/units"
export STUB_LOG="$T/stub.log"

cat > "$T/stubs/flock" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
cat > "$T/stubs/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >> "${STUB_LOG:-/dev/null}"
case "$1" in is-active) exit "${STUB_RELAY_ACTIVE_RC:-0}" ;; esac
exit 0
STUB
cat > "$T/stubs/systemd-run" <<'STUB'
#!/usr/bin/env bash
echo "systemd-run $*" >> "${STUB_LOG:-/dev/null}"
exit 0
STUB
cat > "$T/stubs/curl" <<'STUB'
#!/usr/bin/env bash
# 假 curl：直连 ipify → VPS IP；经代理(-K -) ipify → 住宅出口 IP；ipinfo → {}；Clash API → resi-1
url="" viaproxy=0
for a in "$@"; do case "$a" in http://*|https://*) url="$a" ;; -K) viaproxy=1 ;; esac; done
[[ "$viaproxy" == 1 ]] && cat >/dev/null    # 吃掉 stdin 的 curl 配置
case "$url" in
    *api.ipify.org*)     if [[ "$viaproxy" == 1 ]]; then echo "198.51.100.7"; else echo "203.0.113.10"; fi ;;
    *ipinfo.io*)         echo '{}' ;;
    *proxies/resi-pool*) echo '{"now":"resi-1"}' ;;
esac
exit 0
STUB
chmod +x "$T/stubs/"*
cat > "$T/base/sing-box" <<'STUB'
#!/usr/bin/env bash
if [[ "${FAKE_SB_FAIL:-0}" == "1" ]]; then echo "FATAL[0000] fake check failure" >&2; exit 1; fi
exit 0
STUB
# 假 resi-blacklist.sh：只记录调用（含 BL_SKIP_APPLY）
cat > "$T/base/resi-blacklist.sh" <<'STUB'
#!/usr/bin/env bash
echo "resi-blacklist.sh $* BL_SKIP_APPLY=${BL_SKIP_APPLY:-}" >> "${STUB_LOG:-/dev/null}"
exit 0
STUB
chmod +x "$T/base/sing-box" "$T/base/resi-blacklist.sh"
cat > "$T/base/residential-proxy.json" <<'EOF2'
{"enabled":true,"global":true,"domains":null,"urls":[
 {"host":"brd.superproxy.io","port":44445,"username":"u1","password":"p1","type":"http","name":"url-1"},
 {"host":"gw.example.net","port":1080,"username":"u2","password":"p2","name":"url-2"}]}
EOF2

run_helper() {   # 参数原样传给 helper；stdout → $T/out.txt，stderr(info/err) → $T/err.txt；RUN_BASH 可换解释器
    PATH="$T/stubs:$PATH" BASE_DIR="$T/base" RELAY_UNIT_FILE="$T/base/relay.service" \
    BLACKLIST_UNIT_DIR="$T/units" "${RUN_BASH:-bash}" "$HELPER" "$@" >"$T/out.txt" 2>"$T/err.txt"
}
count_log() { [[ -f "$STUB_LOG" ]] || { echo 0; return 0; }; grep -c -- "$1" "$STUB_LOG" || true; }
digest_of() { if command -v md5sum >/dev/null 2>&1; then md5sum "$1" | awk '{print $1}'; else md5 -q "$1"; fi; }
CFG="$T/base/singbox-relay.json"
STATE="$T/base/residential-blacklist.json"
SVC="$T/units/b-ui-resi-blacklist.service"
TMR="$T/units/b-ui-resi-blacklist.timer"

# ---- 1. reapply 安装 timer（幂等）----
run_helper reapply; assert_eq 0 "$?" "reapply 退出 0"
assert_file_exists "$SVC" "reapply 写了 service"
assert_file_exists "$TMR" "reapply 写了 timer"
assert_contains "ExecStart=$T/base/resi-blacklist.sh refresh" "$(cat "$SVC")" "service ExecStart 指向 resi-blacklist.sh refresh"
assert_contains "Nice=10" "$(cat "$SVC")" "service Nice=10"
assert_contains "TimeoutStartSec=15min" "$(cat "$SVC")" "service TimeoutStartSec=15min"
assert_contains "OnCalendar=*-*-* 04:00:00 Asia/Shanghai" "$(cat "$TMR")" "timer 北京时间 04:00"
assert_contains "RandomizedDelaySec=30min" "$(cat "$TMR")" "timer 随机 30min"
assert_contains "Persistent=true" "$(cat "$TMR")" "timer Persistent"
assert_eq 1 "$(count_log 'enable --now b-ui-resi-blacklist.timer')" "第一次 reapply enable --now 一次"
run_helper reapply; assert_eq 0 "$?" "第二次 reapply 退出 0"
assert_eq 1 "$(count_log 'enable --now b-ui-resi-blacklist.timer')" "第二次 reapply 内容未变，不再 enable"

# ---- 2. blacklist-apply：无状态文件 → 无变化不重启，但 applied 回写 ----
restarts_before=$(count_log 'restart b-ui-relay')
run_helper blacklist-apply; assert_eq 0 "$?" "blacklist-apply(无状态文件) 退出 0"
assert_eq "$restarts_before" "$(count_log 'restart b-ui-relay')" "无变化不重启"
assert_contains "黑名单无变化，保持运行" "$(cat "$T/err.txt")" "无变化提示"
assert_eq "brd.superproxy.io:44445" "$(jq -r '.applied.upstream' "$STATE")" "applied.upstream = 选中上游(resi-1)"
assert_eq "$(digest_of "$CFG")" "$(jq -r '.applied.digest' "$STATE")" "applied.digest = 现配置 md5"
assert_contains "T" "$(jq -r '.applied.at' "$STATE")" "applied.at 是 ISO 时间"
assert_eq '{}' "$(jq -c '.upstreams' "$STATE")" "骨架 upstreams 为空对象"

# ---- 3. blacklist-apply：有条目 → 摘要变化 → restart 一次；再跑一次 → 不重启 ----
jq '.upstreams["brd.superproxy.io:44445"] = {checkedAt:"2026-09-11T20:05:12Z",entries:{
      "www.google.com":{kind:"domain",source:"builtin:search",reason:"403",fails:2,okStreak:0,since:"2026-09-11T20:05:12Z",lastCheck:"2026-09-11T20:05:12Z"},
      "port:5228":{kind:"port",source:"learned",reason:"403",fails:2,okStreak:0,since:"2026-09-11T20:05:12Z",lastCheck:"2026-09-11T20:05:12Z"}}}' \
   "$STATE" > "$STATE.new" && mv "$STATE.new" "$STATE"
restarts_before=$(count_log 'restart b-ui-relay')
run_helper blacklist-apply; assert_eq 0 "$?" "blacklist-apply(有条目) 退出 0"
assert_eq $((restarts_before + 1)) "$(count_log 'restart b-ui-relay')" "摘要变化 → restart 一次"
assert_contains "黑名单有变化，b-ui-relay 已重启" "$(cat "$T/err.txt")" "有变化提示"
assert_eq '["www.google.com"]' "$(jq -c '.route.rules[5].domain_suffix' "$CFG")" "规则已写入 relay 配置"
assert_eq "$(digest_of "$CFG")" "$(jq -r '.applied.digest' "$STATE")" "applied.digest 跟上新配置"
run_helper blacklist-apply; assert_eq 0 "$?" "再跑一次退出 0"
assert_eq $((restarts_before + 1)) "$(count_log 'restart b-ui-relay')" "再跑一次无变化，不重启"

# ---- 4. relay 未运行 + 有变化 → 只写配置不 restart ----
jq 'del(.upstreams["brd.superproxy.io:44445"].entries["port:5228"])' "$STATE" > "$STATE.new" && mv "$STATE.new" "$STATE"
restarts_before=$(count_log 'restart b-ui-relay')
STUB_RELAY_ACTIVE_RC=3 run_helper blacklist-apply; assert_eq 0 "$?" "relay 未运行时退出 0"
assert_eq "$restarts_before" "$(count_log 'restart b-ui-relay')" "relay 未运行不 restart"
assert_eq 6 "$(jq '.route.rules | length' "$CFG")" "配置仍然写入（端口规则已去掉）"

# ---- 5. sing-box check 失败 → 退出 1、配置与 applied 都不动 ----
applied_before=$(jq -c '.applied' "$STATE")
cp "$CFG" "$T/keep.json"
jq '.pins = {"example.com":"resi"}' "$STATE" > "$STATE.new" && mv "$STATE.new" "$STATE"
FAKE_SB_FAIL=1 run_helper blacklist-apply; assert_eq 1 "$?" "check 失败退出 1"
cmp -s "$T/keep.json" "$CFG"; assert_eq 0 "$?" "check 失败旧配置保留"
assert_eq "$applied_before" "$(jq -c '.applied' "$STATE")" "check 失败 applied 不更新"

# ---- 6. disable 移除 timer ----
run_helper disable; assert_eq 0 "$?" "disable 退出 0"
[[ -e "$SVC" || -e "$TMR" ]]; assert_eq 1 "$?" "disable 后 unit 文件已删"
assert_eq 1 "$(count_log 'disable --now b-ui-resi-blacklist.timer')" "disable --now 一次"

# ---- 7. enable --add / --remove 钩子（parse_url 用了 ${x,,}，需要 bash ≥ 4；macOS 自带 3.2 → brew install bash）----
BASH4=""
for b in /opt/homebrew/bin/bash /usr/local/bin/bash "$(command -v bash)"; do
    [[ -x "$b" ]] || continue
    if [[ "$("$b" -c 'echo ${BASH_VERSINFO[0]}')" -ge 4 ]]; then BASH4="$b"; break; fi
done
if [[ -z "$BASH4" ]]; then
    echo "SKIP: 没有 bash ≥ 4，跳过 enable --add / --remove 钩子用例（brew install bash 后重跑）"
else
    cat > "$T/base/residential-proxy.json" <<'EOF2'
{"enabled":true,"global":true,"domains":null,"urls":[
 {"host":"brd.superproxy.io","port":44445,"username":"u1","password":"p1","type":"http","name":"url-1"}]}
EOF2
    RUN_BASH="$BASH4" run_helper enable --add "http://u:p@New.Example.com:8080"; assert_eq 0 "$?" "--add 退出 0"
    assert_eq "198.51.100.7" "$(sed -n 1p "$T/out.txt")" "--add stdout 第一行仍是出口 IP（探测不污染 stdout）"
    assert_eq 1 "$(count_log 'systemd-run --unit b-ui-resi-blacklist-probe-')" "--add 经 systemd-run 启动一次探测"
    assert_contains "--quiet $T/base/resi-blacklist.sh probe new.example.com:8080" "$(grep 'systemd-run' "$STUB_LOG")" "探测目标 = 小写 host:port"
    assert_file_exists "$TMR" "--add 后 timer 已装回"
    RUN_BASH="$BASH4" run_helper enable --remove "socks5://x:x@New.Example.com:8080"; assert_eq 0 "$?" "--remove 退出 0"
    assert_eq 1 "$(count_log 'resi-blacklist.sh forget new.example.com:8080 BL_SKIP_APPLY=1')" "--remove 先调 forget（且不让它回调 apply）"
    assert_file_exists "$TMR" "还剩 1 条上游，timer 保留"
    RUN_BASH="$BASH4" run_helper enable --remove "socks5://x:x@brd.superproxy.io:44445"; assert_eq 0 "$?" "移除最后一条退出 0"
    [[ -e "$TMR" ]]; assert_eq 1 "$?" "池空后 timer 已移除"
fi

finish
```

- [ ] **Step 2: Run it and confirm it fails (Task 5 helper, no Task 6 code yet)**

Run: `cd $REPO && bash tests/resi-blacklist/test_helper_apply.sh`

Expected: 26 `FAIL:` lines (timer files missing, `blacklist-apply` rejected as unknown subcommand → exit 1 and no `applied`, …), then

```
SKIP: 没有 bash ≥ 4，跳过 enable --add / --remove 钩子用例（brew install bash 后重跑）
PASS 9 / FAIL 26
```

(exit 1). With a bash ≥ 4 installed the SKIP line is replaced by further failures.

- [ ] **Step 3: Insert the function block before the main-entry banner**

`server/residential-helper.sh:657-660` currently reads:

```bash
# ---------------------------------------------------------------------------
# 主入口
# ---------------------------------------------------------------------------
cmd="${1:-}"
```

Insert the following block immediately above that banner:

```bash
# ---------------------------------------------------------------------------
# v3.7.0 R13: 黑名单 timer / 异步探测 / applied 回写
# ---------------------------------------------------------------------------
# 住宅池"有效" = enabled 且池非空（与 write_singbox_config_from_state 同判据）
pool_active() {
    [[ -f "${RESIDENTIAL_CONFIG}" ]] || return 1
    [[ "$(jq -r '.enabled // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]] || return 1
    [[ "$(build_urls_json_from_config 2>/dev/null | jq 'length' 2>/dev/null || echo 0)" -gt 0 ]]
}

blacklist_service_unit_text() {
    cat <<EOF
[Unit]
Description=B-UI Residential Blacklist Refresh
After=b-ui-relay.service

[Service]
Type=oneshot
ExecStart=${BLACKLIST_SCRIPT} refresh
Nice=10
TimeoutStartSec=15min
EOF
}

blacklist_timer_unit_text() {
    cat <<'EOF'
[Unit]
Description=B-UI Residential Blacklist Refresh Timer

[Timer]
OnCalendar=*-*-* 04:00:00 Asia/Shanghai
RandomizedDelaySec=30min
AccuracySec=5min
Persistent=true

[Install]
WantedBy=timers.target
EOF
}

# 内容一样则不写并返回 1（调用方据此决定要不要 daemon-reload）
write_unit_if_changed() {
    local path="$1" content="$2"
    if [[ -f "$path" ]] && [[ "$(cat "$path")" == "$content" ]]; then return 1; fi
    printf '%s\n' "$content" > "$path"
    return 0
}

# 每日刷新 timer（北京时间 04:00 起 30 分钟内随机；生产机 UTC，OnCalendar 自带时区）。幂等：内容未变不写不 reload
ensure_blacklist_timer() {
    local svc="${BLACKLIST_UNIT_DIR}/${BLACKLIST_UNIT_BASE}.service"
    local tmr="${BLACKLIST_UNIT_DIR}/${BLACKLIST_UNIT_BASE}.timer"
    local changed=0
    write_unit_if_changed "$svc" "$(blacklist_service_unit_text)" && changed=1
    write_unit_if_changed "$tmr" "$(blacklist_timer_unit_text)" && changed=1
    if [[ "$changed" == 1 ]]; then
        systemctl daemon-reload 2>/dev/null || true
        systemctl enable --now "${BLACKLIST_UNIT_BASE}.timer" 2>/dev/null || true
        info "已安装 ${BLACKLIST_UNIT_BASE}.timer（每日北京时间 04:00 起 30 分钟内刷新黑名单）"
    elif ! systemctl is-enabled --quiet "${BLACKLIST_UNIT_BASE}.timer" 2>/dev/null; then
        systemctl enable --now "${BLACKLIST_UNIT_BASE}.timer" 2>/dev/null || true
    fi
    return 0
}

remove_blacklist_timer() {
    local svc="${BLACKLIST_UNIT_DIR}/${BLACKLIST_UNIT_BASE}.service"
    local tmr="${BLACKLIST_UNIT_DIR}/${BLACKLIST_UNIT_BASE}.timer"
    [[ -f "$svc" || -f "$tmr" ]] || return 0
    systemctl disable --now "${BLACKLIST_UNIT_BASE}.timer" 2>/dev/null || true
    rm -f "$svc" "$tmr"
    systemctl daemon-reload 2>/dev/null || true
    info "已移除 ${BLACKLIST_UNIT_BASE}.timer（住宅池为空或已关闭）"
    return 0
}

# 录入即探测：后台跑 resi-blacklist.sh probe <host:port>，跑完它自己会调回 blacklist-apply。
# systemd-run 缺席（或 unit 名撞车）→ nohup 兜底；任何失败都不影响调用方；不碰 stdout（面板按行解析 stdout）
spawn_blacklist_probe() {
    local key="$1"
    [[ -x "${BLACKLIST_SCRIPT}" ]] || { info "缺少 ${BLACKLIST_SCRIPT}，跳过黑名单探测"; return 0; }
    if command -v systemd-run >/dev/null 2>&1; then
        if systemd-run --unit "${BLACKLIST_UNIT_BASE}-probe-$(date +%s)" --quiet \
               "${BLACKLIST_SCRIPT}" probe "$key" >/dev/null 2>&1; then
            info "黑名单探测已在后台启动（${key}）"
            return 0
        fi
    fi
    nohup "${BLACKLIST_SCRIPT}" probe "$key" >/dev/null 2>&1 </dev/null &
    info "黑名单探测已在后台启动（${key}，nohup）"
    return 0
}

# applied 回写状态文件。只在这一小段拿 .blacklist.lock（fd 8）；此时仍持 .relay.lock（fd 9），
# 顺序固定 relay → blacklist，而 resi-blacklist.sh 调 helper 前必须已释放 .blacklist.lock，不会互等
write_blacklist_applied() {
    local key="$1" digest="$2" now
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if [[ ! -s "${BLACKLIST_STATE}" ]]; then
        printf '{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}\n' > "${BLACKLIST_STATE}"
    fi
    chmod 600 "${BLACKLIST_STATE}" 2>/dev/null || true
    (
        exec 8>"${BLACKLIST_LOCK}"
        chmod 600 "${BLACKLIST_LOCK}" 2>/dev/null || true
        flock -w 30 8 || { err "获取 blacklist 锁超时(30s)，applied 未更新"; exit 1; }
        jq --arg k "$key" --arg d "$digest" --arg at "$now" \
           '.applied = {upstream: $k, digest: $d, at: $at}' "${BLACKLIST_STATE}" > "${BLACKLIST_STATE}.tmp" \
        && chmod 600 "${BLACKLIST_STATE}.tmp" && mv "${BLACKLIST_STATE}.tmp" "${BLACKLIST_STATE}"
    ) || { rm -f "${BLACKLIST_STATE}.tmp"; err "applied 写入失败"; return 1; }
    return 0
}

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

```

- [ ] **Step 4: Put `blacklist-apply` under the relay lock and extend the usage line**

`server/residential-helper.sh:664` currently reads:

```bash
    setup|enable|disable|reapply|set-domains|global) acquire_relay_lock ;;
```

Replace with:

```bash
    setup|enable|disable|reapply|set-domains|global|blacklist-apply) acquire_relay_lock ;;
```

`server/residential-helper.sh:855` currently reads:

```bash
        echo "Usage: $0 {setup|enable <url>|disable|status|domains|reapply|set-domains <json>|global on|off}" >&2
```

Replace with:

```bash
        echo "Usage: $0 {setup|enable <url>|disable|status|domains|reapply|set-domains <json>|global on|off|blacklist-apply}" >&2
```

- [ ] **Step 5: Hook `setup`, `enable --add`, `enable --remove`, `enable <url>`, `disable`**

(5a) `setup` — `server/residential-helper.sh:671-672` currently reads:

```bash
        start_relay_service
        info "sing-box 中继已就绪，b-ui-relay 监听 127.0.0.1:${SINGBOX_RELAY_PORT}"
```

Replace with:

```bash
        start_relay_service
        # v3.7.0 R13: 新装池空 → 不装 timer（升级迁移 D10 走 reapply）
        if pool_active; then ensure_blacklist_timer; else remove_blacklist_timer; fi
        info "sing-box 中继已就绪，b-ui-relay 监听 127.0.0.1:${SINGBOX_RELAY_PORT}"
```

(5b) `enable --add` — `server/residential-helper.sh:689-692` currently reads:

```bash
            write_singbox_config_from_state
            reload_relay_service
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}")
            info "已新增 URL（共 ${total} 个住宅出口，本条 ${RESI_TYPE}）"
```

Replace with:

```bash
            write_singbox_config_from_state
            reload_relay_service
            # v3.7.0 R13: 录入即探测（异步；结果就是该上游的初始黑名单）+ 确保每日刷新 timer
            ensure_blacklist_timer
            spawn_blacklist_probe "$(lower "$RESI_HOST"):${RESI_PORT}"
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}")
            info "已新增 URL（共 ${total} 个住宅出口，本条 ${RESI_TYPE}）"
```

(5c) `enable --remove` — `server/residential-helper.sh:699-701` currently reads:

```bash
            parse_url "$3"
            ensure_singbox
            remove_url_from_config "$RESI_HOST" "$RESI_PORT"
```

Replace with:

```bash
            parse_url "$3"
            ensure_singbox
            # v3.7.0 R13: 先删该上游的黑名单数据（BL_SKIP_APPLY=1：此刻持有 relay 锁，别让它回调 blacklist-apply）
            if [[ -x "${BLACKLIST_SCRIPT}" ]]; then
                BL_SKIP_APPLY=1 "${BLACKLIST_SCRIPT}" forget "$(lower "$RESI_HOST"):${RESI_PORT}" >/dev/null 2>&1 || true
            fi
            remove_url_from_config "$RESI_HOST" "$RESI_PORT"
```

and `server/residential-helper.sh:707-708` currently reads:

```bash
                write_singbox_config_direct
                info "最后一个 URL 已移除，住宅代理已禁用"
```

Replace with:

```bash
                write_singbox_config_direct
                remove_blacklist_timer
                info "最后一个 URL 已移除，住宅代理已禁用"
```

(5d) `enable <url>` (single) — `server/residential-helper.sh:734-737` currently reads:

```bash
        save_config true
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        echo "$RESI_TYPE"
```

Replace with:

```bash
        save_config true
        # v3.7.0 R13: 录入即探测（异步）+ 确保每日刷新 timer
        ensure_blacklist_timer
        spawn_blacklist_probe "$(lower "$RESI_HOST"):${RESI_PORT}"
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        echo "$RESI_TYPE"
```

(5e) `disable` — `server/residential-helper.sh:753` currently reads:

```bash
        info "住宅代理已关闭，b-ui-relay 继续运行（直连模式）"
```

Replace with:

```bash
        remove_blacklist_timer
        info "住宅代理已关闭，b-ui-relay 继续运行（直连模式）"
```

- [ ] **Step 6: Hook `reapply` and add the `blacklist-apply)` case**

`server/residential-helper.sh:792-795` currently reads (end of the `reapply)` case):

```bash
                info "b-ui-relay 配置与 unit 无变化，保持运行（不掐断住宅连接）"
            fi
        fi
        ;;
```

Replace with:

```bash
                info "b-ui-relay 配置与 unit 无变化，保持运行（不掐断住宅连接）"
            fi
        fi
        # v3.7.0 R13: 池有效才装每日刷新 timer，否则确保它不存在（与 D8 对 resi-health.timer 的处理一致）
        if pool_active; then ensure_blacklist_timer; else remove_blacklist_timer; fi
        ;;

    blacklist-apply)
        # v3.7.0 R13: 按当前选中上游的生效黑名单重写 relay 配置；摘要没变就不重启（同供应商多条上游
        # 黑名单几乎相同，健康切换通常零重启）。sing-box check 不过 → install_relay_config 保留旧配置，
        # 这里退出 1 且 applied 不更新（面板据 applied.at < checkedAt 提示"上次应用失败"）。
        bl_key=$(selected_upstream_key)
        bl_before=$(file_digest "${SINGBOX_CONFIG}")
        write_singbox_config_from_state || { err "黑名单应用失败：relay 配置未更新，保持旧配置"; exit 1; }
        bl_after=$(file_digest "${SINGBOX_CONFIG}")
        if [[ "$bl_after" != "$bl_before" ]]; then
            if systemctl is-active --quiet "${RELAY_SERVICE}" 2>/dev/null; then
                systemctl restart "${RELAY_SERVICE}" 2>/dev/null || true
                info "黑名单有变化，b-ui-relay 已重启"
            else
                info "黑名单有变化，配置已写入（b-ui-relay 未在运行，下次启动生效）"
            fi
        else
            info "黑名单无变化，保持运行"
        fi
        if [[ -n "$bl_key" ]]; then
            write_blacklist_applied "$bl_key" "$bl_after" || true
        fi
        ;;
```

Notes: `blacklist-apply` intentionally does not call `ensure_singbox` (it is invoked from the probe script and from the health timer; the relay is already running with a binary, and `install_relay_config` skips the check when the binary is somehow missing rather than downloading on a hot path). `set-domains` / `global` / `reapply` paths keep calling `write_singbox_config_from_state` unguarded, so a failed `sing-box check` there aborts the script with exit 1 via errexit — the old config stays in place.

- [ ] **Step 7: Syntax check, run both helper tests**

Run: `cd $REPO && bash -n server/residential-helper.sh && bash tests/resi-blacklist/test_helper_apply.sh && bash tests/resi-blacklist/test_helper_rules.sh`

Expected on macOS `/bin/bash` 3.2:

```
SKIP: 没有 bash ≥ 4，跳过 enable --add / --remove 钩子用例（brew install bash 后重跑）
PASS 35 / FAIL 0
PASS 21 / FAIL 0
```

With a bash ≥ 4 on the machine (`brew install bash`) the first line disappears and the apply test prints `PASS 45 / FAIL 0` (the ten hook assertions: `--add` exit 0, stdout line 1 still the exit IP, one `systemd-run --unit b-ui-resi-blacklist-probe-… --quiet …/resi-blacklist.sh probe new.example.com:8080`, timer re-installed, `--remove` exit 0, one `forget new.example.com:8080` with `BL_SKIP_APPLY=1`, timer kept while one upstream remains, last removal exit 0, timer gone). Both exit 0.

- [ ] **Step 8: Commit**

```bash
cd $REPO && git add server/residential-helper.sh tests/resi-blacklist/test_helper_apply.sh && git commit -m "feat(residential): blacklist-apply 子命令 + 每日刷新 timer + 录入即探测/移除即遗忘钩子

- blacklist-apply：按选中上游重写 relay 配置，md5 变了且 relay 在跑才 restart；check 不过退出 1 保旧配置；applied 经 .blacklist.lock 回写
- ensure_blacklist_timer / remove_blacklist_timer：b-ui-resi-blacklist.{service,timer}（北京时间 04:00 起 30 分钟内随机，Persistent），内容未变不写不 reload；BLACKLIST_UNIT_DIR 可覆盖
- spawn_blacklist_probe：systemd-run --unit b-ui-resi-blacklist-probe-<ts>，无 systemd-run 时 nohup 兜底，不碰 stdout
- 钩子：enable / --add 装 timer 并异步探测；--remove 先 BL_SKIP_APPLY=1 forget，池空拆 timer；disable 拆 timer；setup / reapply 按池有效与否装/拆
- tests/resi-blacklist/test_helper_apply.sh（重启只在摘要变化时、applied 回写、timer 幂等、钩子用例需 bash≥4）

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: `resi-health.sh` applies the new upstream's blacklist after a successful switch

**Files:**
- Create: `tests/resi-blacklist/test_health_hook.sh`
- Modify: `server/resi-health.sh:139-145` (the `elif curl … PUT` success branch; insert after line 142)

**Interfaces:**
- Consumes: `residential-helper.sh blacklist-apply` (Task 6) located at `$BASE/residential-helper.sh` (`BASE="${RESI_HEALTH_BASE_DIR:-/opt/b-ui}"`, line 14); the script's `LOG` (`RESI_HEALTH_LOG`), `DRY_RUN` (`RESI_HEALTH_DRY_RUN`).
- Produces: after the log line `切换住宅出口 <old> → <new>（…）` the health log also contains the helper's stderr (`黑名单无变化，保持运行` or `黑名单有变化，b-ui-relay 已重启`). No new env vars; exit code of the health script unchanged (`|| true`).

- [ ] **Step 1: Write the failing test `tests/resi-blacklist/test_health_hook.sh`**

```bash
#!/usr/bin/env bash
# tests/resi-blacklist/test_health_hook.sh — Task 7
#   resi-health.sh 切换成功（PUT selector）后调 residential-helper.sh blacklist-apply；DRY_RUN=1 不调；不切换不调
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
. "$HERE/lib.sh"
HEALTH="$REPO/server/resi-health.sh"

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stubs" "$T/base"
export STUB_LOG="$T/stub.log"

cat > "$T/stubs/curl" <<'STUB'
#!/usr/bin/env bash
# 假 curl：探测 URL 按 -K - 配置里的代理主机定成败（bad.example.net → 连不上）；
# Clash API GET → 当前 resi-1；PUT → 记录并成功（STUB_PUT_FAIL=1 → 失败）
url="" put=0 viaproxy=0
for a in "$@"; do case "$a" in http://*|https://*) url="$a" ;; PUT) put=1 ;; -K) viaproxy=1 ;; esac; done
cfg=""; [[ "$viaproxy" == 1 ]] && cfg=$(cat)
case "$url" in
    *generate_204*)      case "$cfg" in *bad.example.net*) exit 7 ;; *) exit 0 ;; esac ;;
    *proxies/resi-pool*) if [[ "$put" == 1 ]]; then echo "curl PUT $*" >> "${STUB_LOG:-/dev/null}"; exit "${STUB_PUT_FAIL:-0}"; fi
                         echo '{"now":"resi-1"}'; exit 0 ;;
esac
exit 0
STUB
chmod +x "$T/stubs/curl"
# 假 helper：记录子命令
cat > "$T/base/residential-helper.sh" <<'STUB'
#!/usr/bin/env bash
echo "helper $*" >> "${STUB_LOG:-/dev/null}"
echo "黑名单无变化，保持运行" >&2
exit 0
STUB
chmod +x "$T/base/residential-helper.sh"
cat > "$T/base/singbox-relay.json" <<'EOF2'
{"outbounds":[
 {"type":"http","tag":"resi-1","server":"bad.example.net","server_port":44445,"username":"u1","password":"p1"},
 {"type":"socks","tag":"resi-2","server":"good.example.net","server_port":1080,"username":"u2","password":"p2","version":"5"},
 {"type":"selector","tag":"resi-pool","outbounds":["resi-1","resi-2"],"default":"resi-1"},
 {"type":"direct","tag":"direct"}]}
EOF2

run_health() {   # 1 次探测、1 次失败即剔除、切换不限速 → 一轮就能触发切换
    PATH="$T/stubs:$PATH" RESI_HEALTH_BASE_DIR="$T/base" RESI_HEALTH_LOG="$T/base/health.log" \
    RESI_HEALTH_TRIES=1 RESI_HEALTH_FAIL_TO_REMOVE=1 RESI_HEALTH_SWITCH_MIN_INTERVAL=0 \
    bash "$HEALTH" "$@" >"$T/out.txt" 2>&1
}
count_log() { [[ -f "$STUB_LOG" ]] || { echo 0; return 0; }; grep -c -- "$1" "$STUB_LOG" || true; }

# 1. 真切换：PUT 成功 → 调一次 blacklist-apply，helper 输出进健康日志
run_health; assert_eq 0 "$?" "巡检退出 0"
assert_eq 1 "$(count_log 'curl PUT')" "切换 PUT 一次"
assert_contains 'resi-2' "$(grep 'curl PUT' "$STUB_LOG")" "切到健康的 resi-2"
assert_eq 1 "$(count_log 'helper blacklist-apply')" "切换后调 blacklist-apply 一次"
assert_contains "切换住宅出口 resi-1 → resi-2" "$(cat "$T/base/health.log")" "日志记录切换"
assert_contains "黑名单无变化，保持运行" "$(cat "$T/base/health.log")" "helper 输出追加进健康日志"

# 2. DRY_RUN=1：不 PUT、不调 helper
rm -f "$STUB_LOG" "$T/base/.resi-health-state.json"
RESI_HEALTH_DRY_RUN=1 run_health; assert_eq 0 "$?" "DRY_RUN 退出 0"
assert_eq 0 "$(count_log 'helper blacklist-apply')" "DRY_RUN 不调 blacklist-apply"
assert_eq 0 "$(count_log 'curl PUT')" "DRY_RUN 不 PUT"

# 3. PUT 失败：不调 helper
rm -f "$STUB_LOG" "$T/base/.resi-health-state.json"
STUB_PUT_FAIL=22 run_health; assert_eq 0 "$?" "PUT 失败退出 0"
assert_eq 0 "$(count_log 'helper blacklist-apply')" "PUT 失败不调 blacklist-apply"
assert_contains "WARN 切换到 resi-2 失败" "$(cat "$T/base/health.log")" "PUT 失败记 WARN"

finish
```

- [ ] **Step 2: Run it and confirm it fails on the unmodified script**

Run: `cd $REPO && bash tests/resi-blacklist/test_health_hook.sh`

Expected:

```
FAIL: 切换后调 blacklist-apply 一次 —— 期望 [1] 实际 [0]
FAIL: helper 输出追加进健康日志 —— 未包含 [黑名单无变化，保持运行]，实际 [...]
PASS 10 / FAIL 2
```

(exit 1).

- [ ] **Step 3: Add the hook**

`server/resi-health.sh:139-145` currently reads:

```bash
elif curl -sf --max-time 3 -X PUT -H 'Content-Type: application/json' \
          -d "{\"name\":\"${target}\"}" "http://${API}/proxies/resi-pool" >/dev/null 2>&1; then
    jq --argjson t "$now" '._last_switch=$t' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE"
    log "切换住宅出口 ${sel} → ${target}（${sel} 连续探测不达标）"
else
    log "WARN 切换到 ${target} 失败（Clash API PUT 出错）"
fi
```

Replace with (three lines inserted after the `log "切换住宅出口 …"` line):

```bash
elif curl -sf --max-time 3 -X PUT -H 'Content-Type: application/json' \
          -d "{\"name\":\"${target}\"}" "http://${API}/proxies/resi-pool" >/dev/null 2>&1; then
    jq --argjson t "$now" '._last_switch=$t' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE"
    log "切换住宅出口 ${sel} → ${target}（${sel} 连续探测不达标）"
    # v3.7.0 R13: 新上游可能有不同的机房直出黑名单——切换后让 helper 按新选中上游重写 relay 规则。
    # 这是本脚本唯一的写路径，且 helper 只在黑名单真有差异时才重启（同供应商通常零重启）。
    [ "$DRY_RUN" = "1" ] || "$BASE/residential-helper.sh" blacklist-apply >>"$LOG" 2>&1 || true
else
    log "WARN 切换到 ${target} 失败（Clash API PUT 出错）"
fi
```

Also update the header comment at `server/resi-health.sh:5` which currently reads:

```bash
# 既不改写 singbox-relay.json 也不重启 b-ui-relay（重启会掐断所有用户的现有连接）。
```

Replace with:

```bash
# 既不改写 singbox-relay.json 也不重启 b-ui-relay（重启会掐断所有用户的现有连接）——
# 唯一例外（v3.7.0 R13）：切换成功后调 residential-helper.sh blacklist-apply，它只在新上游的
# 机房直出黑名单与现生效的不同时才重写并重启。
```

- [ ] **Step 4: Syntax check and run the test**

Run: `cd $REPO && bash -n server/resi-health.sh && bash tests/resi-blacklist/test_health_hook.sh`

Expected:

```
PASS 12 / FAIL 0
```

(exit 0.) Also confirm the whole section still passes: `bash tests/resi-blacklist/test_helper_rules.sh && bash tests/resi-blacklist/test_helper_apply.sh` → `PASS 21 / FAIL 0`, `PASS 35 / FAIL 0` (+SKIP line on bash 3.2).

- [ ] **Step 5: Commit**

```bash
cd $REPO && git add server/resi-health.sh tests/resi-blacklist/test_health_hook.sh && git commit -m "feat(residential): 巡检切换上游后应用对应黑名单（blacklist-apply）

resi-health.sh 在 Clash API PUT 切换成功那一行之后调 residential-helper.sh blacklist-apply
（输出进巡检日志，失败不影响巡检退出码）；DRY_RUN 与 PUT 失败都不调。
仍保持"只读、不重启"——helper 只在新上游黑名单与现生效不同时才重启 b-ui-relay。
tests/resi-blacklist/test_health_hook.sh 用 stub curl / stub helper 覆盖三种情况。

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Section B notes for the other sections

- `blacklist_rules_json` semantics (Task 5) — Section A's `effective_json` must produce the same three arrays: `resi` = every pinned-`resi` target (sorted unique, as-is); `direct` = domain entries of that upstream ∪ pinned-`direct` non-`port:` targets, minus pinned-`resi`; `ports` = port entries ∪ pinned-`direct` `port:N`, minus pinned-`resi`, as sorted unique integers. The jq filter in Task 5 Step 6 can be copied verbatim into `resi-blacklist.sh`.
- `enable --remove` runs `BL_SKIP_APPLY=1 resi-blacklist.sh forget <host:port>` while holding `.relay.lock`; Section A's `forget` must honour `BL_SKIP_APPLY=1` (no `blacklist-apply` call-back) or it will wait 30 s on the relay lock and fail.
- `selected_upstream_key` derives the key from `build_urls_json_from_config` (not `.urls` alone) so single-URL installs (`enable <url>`, `.urls=[]`, top-level `host`/`port`) get a key too; server.js's `getRelaySelected()` mapping in Section C should mirror that (`urls[]` when non-empty, else top-level host/port).
- Timer unit names/paths: `${BLACKLIST_UNIT_DIR}/b-ui-resi-blacklist.service` / `.timer`; update.sh D10 (Section C) should not write its own copies — call `residential-helper.sh reapply`, which installs or removes them based on `pool_active`.
