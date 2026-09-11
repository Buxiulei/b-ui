## Section C — Release plumbing (Tasks 8, 11, 12)

Scope of this section: the `update.sh` D10 migration block, registering the new `server/resi-blacklist.sh` in every file table, uninstall cleanup, the fresh-install path, the guide/CLAUDE.md/version.json release edits, and the production verification on `bwg-tizi`. Tasks 9 and 10 (web panel) belong to Section B and are assumed done before Task 11 commits the release.

Conventions used below (same as the rest of the plan):

- `REPO` = `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5`; every command runs from there.
- Tests are plain bash under `tests/resi-blacklist/` (never deployed, not in `version.json files[]`). They source `tests/resi-blacklist/lib.sh`, which Task 1 created with `assert_eq <expected> <actual> <msg>`, `assert_contains <haystack> <needle> <msg>`, `assert_file_exists <path> <msg>`, `pass <msg>`, `fail <msg>`, and `finish` (prints `PASS n / FAIL m`, exits 1 when m > 0). The tests in this section only use `assert_eq` and `finish` so they do not depend on the argument order of the other helpers.
- Line numbers are from the current worktree at commit `92953c6` (before Tasks 1–7 land). Tasks 1–7 only touch `server/resi-blacklist.sh` (new), `server/residential-helper.sh`, `server/resi-health.sh`, `web/*`; the anchors below in `update.sh`, `install.sh`, `version.json`, `b-ui-cli.sh`, `CLAUDE.md` and the guide are therefore still valid, but always re-check with `grep -n` before editing.
- Commit messages are Chinese conventional commits and end with a blank line plus `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

---

### Task 8: `update.sh` D10 migration + registering `server/resi-blacklist.sh` everywhere

**Files:**
- Modify: `server/update.sh:257-272` (interactive `files=(...)` table), `server/update.sh:293-297` (`chmod +x` after download), `server/update.sh:1193-1196` (insert D10 right after the D8 `fi` and before the `# v3.6.0 D9:` call), `server/update.sh:2133-2148` (`auto` `file_map`)
- Modify: `install.sh:239-254` (fresh-install `files=(...)` table), `install.sh:274-277` (`chmod +x`)
- Modify: `version.json:4-18` (`files[]`)
- Modify: `server/b-ui-cli.sh:682-684` (uninstall stop/disable loop), `server/b-ui-cli.sh:698` (uninstall `rm -f` of unit files)
- Create: `tests/resi-blacklist/test_release_files.sh`, `tests/resi-blacklist/test_update_d10.sh`
- Test: `bash -n install.sh server/update.sh server/b-ui-cli.sh`, `bash tests/resi-blacklist/test_release_files.sh`, `bash tests/resi-blacklist/test_update_d10.sh`

**Interfaces:**
- Consumes: `${BASE_DIR}/residential-helper.sh reapply` (Task 6: installs `b-ui-resi-blacklist.timer` via `ensure_blacklist_timer` when the pool is non-empty and enabled, removes it otherwise; empty blacklist ⇒ relay config byte-identical ⇒ no restart), `${BASE_DIR}/resi-blacklist.sh probe all` (Task 3 `cmd_probe`), the empty state skeleton `{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}`, `download_and_validate <url> <local_path>` and `select_download_source` (existing in `update.sh`), `RESIDENTIAL_CONFIG` = `${BASE_DIR}/residential-proxy.json`.
- Produces: `update.sh` D10 block (idempotent), env override `BLACKLIST_TIMER_FILE` (default `/etc/systemd/system/b-ui-resi-blacklist.timer`, tests only — same pattern as the existing `RESI_HEALTH_TIMER_FILE` in `patch_resi_health_timer`), registration of `server/resi-blacklist.sh` → `${BASE_DIR}/resi-blacklist.sh` in the four file tables, uninstall cleanup of `b-ui-resi-blacklist.timer`/`.service` and any `b-ui-resi-blacklist-probe-*` unit.

Fresh-install path (no code needed beyond the file table): `install.sh:604-609` runs `residential-helper.sh setup` (pool empty ⇒ Task 6's `ensure_blacklist_timer` is not invoked / removes nothing), then `install.sh:652` runs `residential-helper.sh enable "$resi_url"` if the user pastes a residential URL. Task 6's `enable` hook (`spawn_blacklist_probe "host:port"` after `save_config`) starts the initial probe, and — per spec §7 「由 residential-helper.sh ensure_blacklist_timer 在 enable / reapply 里创建」 — `enable` must also call `ensure_blacklist_timer`. Task 8 does not duplicate that logic; Step 2's test asserts the helper contains at least two occurrences of `ensure_blacklist_timer` (definition + at least one call) so a Task 6 regression is caught here. The only thing `install.sh` needs for the new script is the download entry and `chmod +x` (Step 4).

Why D10 is needed although `reapply` installs the timer: the chicken-and-egg noted in D7/D8 — during the 3.6.3 → 3.7.0 upgrade the *old* `update.sh` (already loaded in RAM, file table without `resi-blacklist.sh`) is what downloads files and runs `apply_systemd_configs`; the new D10 executes only on the *next* `update.sh` run (the "已是最新 → 幂等自愈检查" path in `check_and_update`). Without D10, `/opt/b-ui/resi-blacklist.sh` would never be downloaded on existing installs, the timer written by `reapply` would point at a missing script, and no initial probe would ever run. Task 12 therefore runs `update.sh -y` twice.

- [ ] **Step 1: Make sure the shared test library exists**

Run: `ls tests/resi-blacklist/lib.sh`

If it prints the path, skip to Step 2. If Task 1 has not been merged yet (file missing), create `tests/resi-blacklist/lib.sh` with exactly this content (Task 1's version is a superset; keep whichever lands first):

```bash
#!/usr/bin/env bash
# tests/resi-blacklist/lib.sh —— 极简断言库（无测试框架；每个测试脚本 source 它，最后调 finish）
PASS_COUNT=0
FAIL_COUNT=0
pass() { PASS_COUNT=$((PASS_COUNT + 1)); echo "  ok   - $1"; }
fail() { FAIL_COUNT=$((FAIL_COUNT + 1)); echo "  FAIL - $1" >&2; }
# assert_eq <expected> <actual> <msg>
assert_eq() {
    if [[ "$1" == "$2" ]]; then pass "$3"; else fail "$3 (expected: [$1] actual: [$2])"; fi
}
# assert_contains <haystack> <needle> <msg>
assert_contains() {
    if [[ "$1" == *"$2"* ]]; then pass "$3"; else fail "$3 (needle not found: [$2])"; fi
}
# assert_file_exists <path> <msg>
assert_file_exists() {
    if [[ -e "$1" ]]; then pass "$2"; else fail "$2 (missing: $1)"; fi
}
finish() {
    echo "PASS ${PASS_COUNT} / FAIL ${FAIL_COUNT}"
    [[ "$FAIL_COUNT" -eq 0 ]] || exit 1
    exit 0
}
```

- [ ] **Step 2: Write the registration test (expected to fail)**

Create `tests/resi-blacklist/test_release_files.sh`:

```bash
#!/usr/bin/env bash
# 发版登记一致性：server/resi-blacklist.sh 必须同时出现在
#   version.json files[] / install.sh 文件表 / update.sh 交互表 / update.sh auto file_map
# 并且 version.json 里每个 server/*、web/*、b-ui-client.sh 都在 install.sh 与 update.sh 两表里（防以后再漏）。
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$REPO/tests/resi-blacklist/lib.sh"

yn() { if "$@"; then echo yes; else echo no; fi; }

# 1) version.json files[] 登记，且紧跟 server/resi-health.sh
files_list=$(jq -r '.files[]' "$REPO/version.json")
assert_eq "1" "$(printf '%s\n' "$files_list" | grep -cx 'server/resi-blacklist.sh')" "version.json files[] 登记 server/resi-blacklist.sh"
after_health=$(jq -r '(.files | index("server/resi-health.sh")) as $i | .files[$i+1] // ""' "$REPO/version.json")
assert_eq "server/resi-blacklist.sh" "$after_health" "version.json: resi-blacklist.sh 紧跟 resi-health.sh"

# 2) install.sh 文件表 + chmod +x
assert_eq "1" "$(grep -cF '"server/resi-blacklist.sh:${BASE_DIR}/resi-blacklist.sh"' "$REPO/install.sh")" "install.sh 文件表登记"
assert_eq "1" "$(grep -cF 'chmod +x "${BASE_DIR}/resi-blacklist.sh"' "$REPO/install.sh")" "install.sh chmod +x resi-blacklist.sh"

# 3) update.sh 交互表 + auto file_map + chmod +x
assert_eq "1" "$(grep -cF '"server/resi-blacklist.sh:${BASE_DIR}/resi-blacklist.sh"' "$REPO/server/update.sh")" "update.sh 交互文件表登记"
assert_eq "1" "$(grep -cF '["server/resi-blacklist.sh"]="${BASE_DIR}/resi-blacklist.sh"' "$REPO/server/update.sh")" "update.sh auto file_map 登记"
assert_eq "1" "$(grep -cF 'chmod +x "${BASE_DIR}/resi-blacklist.sh"' "$REPO/server/update.sh")" "update.sh chmod +x resi-blacklist.sh"

# 4) 通用一致性：version.json 每个文件（install.sh 自身除外）都在三张表里
while IFS= read -r f; do
    [[ -z "$f" || "$f" == "install.sh" ]] && continue
    assert_eq "yes" "$(yn grep -qF "\"$f:" "$REPO/install.sh")"          "install.sh 文件表含 $f"
    assert_eq "yes" "$(yn grep -qF "\"$f:" "$REPO/server/update.sh")"    "update.sh 交互表含 $f"
    assert_eq "yes" "$(yn grep -qF "[\"$f\"]=" "$REPO/server/update.sh")" "update.sh file_map 含 $f"
done <<< "$files_list"

# 5) D10 块存在且只有一处
assert_eq "1" "$(grep -c '# v3.7.0 D10:' "$REPO/server/update.sh")" "update.sh 有且只有一个 D10 块"

# 6) 卸载清理 + 新装路径（Task 6 的 enable/reapply 必须调用 ensure_blacklist_timer）
assert_eq "yes" "$(yn grep -qF 'b-ui-resi-blacklist.timer b-ui-resi-blacklist.service' "$REPO/server/b-ui-cli.sh")" "b-ui-cli.sh 卸载时停用 blacklist timer/service"
assert_eq "yes" "$(yn grep -qF 'rm -f /etc/systemd/system/b-ui-resi-blacklist.service /etc/systemd/system/b-ui-resi-blacklist.timer' "$REPO/server/b-ui-cli.sh")" "b-ui-cli.sh 卸载时删除 blacklist unit 文件"
n_ens=$(grep -c 'ensure_blacklist_timer' "$REPO/server/residential-helper.sh")
assert_eq "yes" "$([[ "${n_ens:-0}" -ge 2 ]] && echo yes || echo no)" "residential-helper.sh 定义并调用 ensure_blacklist_timer（出现 ${n_ens} 次，需 ≥2）"

finish
```

- [ ] **Step 3: Run it, confirm the expected failures**

Run: `bash tests/resi-blacklist/test_release_files.sh`

Expected: the four `resi-blacklist.sh` registration checks, the two `chmod +x` checks, the `D10` check and the two `b-ui-cli.sh` checks print `FAIL - ...`; the section-4 loop passes for every existing file; the `ensure_blacklist_timer` check passes (Task 6 done) — last line `PASS 40 / FAIL 10` (measured against the current worktree; FAIL 11 if Task 6 is not merged yet), exit status 1.

- [ ] **Step 4: Register the file in `version.json`, `install.sh` and `update.sh`**

`version.json:9-10` currently reads:

```json
        "server/residential-helper.sh",
        "server/resi-health.sh",
```

Replace with:

```json
        "server/residential-helper.sh",
        "server/resi-health.sh",
        "server/resi-blacklist.sh",
```

`install.sh:244-246` currently reads:

```bash
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "server/resi-health.sh:${BASE_DIR}/resi-health.sh"
        "b-ui-client.sh:${BASE_DIR}/b-ui-client.sh"
```

Replace with:

```bash
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "server/resi-health.sh:${BASE_DIR}/resi-health.sh"
        "server/resi-blacklist.sh:${BASE_DIR}/resi-blacklist.sh"
        "b-ui-client.sh:${BASE_DIR}/b-ui-client.sh"
```

`install.sh:274-277` currently reads:

```bash
    chmod +x "${BASE_DIR}/core.sh"
    chmod +x "${BASE_DIR}/b-ui-cli.sh"
    chmod +x "${BASE_DIR}/update.sh"
    chmod +x "${BASE_DIR}/residential-helper.sh"
```

Replace with:

```bash
    chmod +x "${BASE_DIR}/core.sh"
    chmod +x "${BASE_DIR}/b-ui-cli.sh"
    chmod +x "${BASE_DIR}/update.sh"
    chmod +x "${BASE_DIR}/residential-helper.sh"
    # v3.7.0: 黑名单脚本由 systemd timer / systemd-run 直接执行，必须有 x 位
    chmod +x "${BASE_DIR}/resi-blacklist.sh"
```

`server/update.sh:262-264` (interactive `do_update` table) currently reads:

```bash
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "server/resi-health.sh:${BASE_DIR}/resi-health.sh"
        "b-ui-client.sh:${BASE_DIR}/b-ui-client.sh"
```

Replace with:

```bash
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "server/resi-health.sh:${BASE_DIR}/resi-health.sh"
        "server/resi-blacklist.sh:${BASE_DIR}/resi-blacklist.sh"
        "b-ui-client.sh:${BASE_DIR}/b-ui-client.sh"
```

`server/update.sh:296-297` currently reads:

```bash
    chmod +x "${BASE_DIR}/update.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/residential-helper.sh" 2>/dev/null
```

Replace with:

```bash
    chmod +x "${BASE_DIR}/update.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/residential-helper.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/resi-blacklist.sh" 2>/dev/null
```

`server/update.sh:2138-2140` (`auto_update` `file_map`) currently reads:

```bash
            ["server/residential-helper.sh"]="${BASE_DIR}/residential-helper.sh"
            ["server/resi-health.sh"]="${BASE_DIR}/resi-health.sh"
            ["web/server.js"]="${ADMIN_DIR}/server.js"
```

Replace with:

```bash
            ["server/residential-helper.sh"]="${BASE_DIR}/residential-helper.sh"
            ["server/resi-health.sh"]="${BASE_DIR}/resi-health.sh"
            ["server/resi-blacklist.sh"]="${BASE_DIR}/resi-blacklist.sh"
            ["web/server.js"]="${ADMIN_DIR}/server.js"
```

(The `auto` loop already does `chmod +x "$local_path"` for every downloaded file, so no extra chmod is needed there.)

- [ ] **Step 5: Uninstall cleanup in `b-ui-cli.sh`**

`server/b-ui-cli.sh:682-687` currently reads:

```bash
    for s in hysteria-server hysteria-residential xray b-ui-admin caddy \
             b-ui-cert-sync.timer b-ui-cert-sync.service hy2-watchdog.timer hy2-watchdog.service \
             b-ui-resi-health.timer b-ui-resi-health.service b-ui-relay; do
        systemctl stop "$s" 2>/dev/null || true
        systemctl disable "$s" 2>/dev/null || true
    done
```

Replace with:

```bash
    for s in hysteria-server hysteria-residential xray b-ui-admin caddy \
             b-ui-cert-sync.timer b-ui-cert-sync.service hy2-watchdog.timer hy2-watchdog.service \
             b-ui-resi-health.timer b-ui-resi-health.service \
             b-ui-resi-blacklist.timer b-ui-resi-blacklist.service b-ui-relay; do
        systemctl stop "$s" 2>/dev/null || true
        systemctl disable "$s" 2>/dev/null || true
    done
    # v3.7.0: 正在跑的黑名单一次性探测单元（systemd-run 起的 transient unit）一并停掉
    systemctl stop 'b-ui-resi-blacklist-probe-*' 2>/dev/null || true
```

`server/b-ui-cli.sh:698` currently reads:

```bash
    rm -f /etc/systemd/system/b-ui-resi-health.service /etc/systemd/system/b-ui-resi-health.timer
```

Replace with:

```bash
    rm -f /etc/systemd/system/b-ui-resi-health.service /etc/systemd/system/b-ui-resi-health.timer
    rm -f /etc/systemd/system/b-ui-resi-blacklist.service /etc/systemd/system/b-ui-resi-blacklist.timer
```

- [ ] **Step 6: Write the D10 dry-run test (expected to fail: no block to extract yet)**

Create `tests/resi-blacklist/test_update_d10.sh`:

```bash
#!/usr/bin/env bash
# update.sh D10 块干跑：用 awk 抽出块（从 "# v3.7.0 D10:" 注释到 "# v3.6.0 D9:" 前一行）包成函数，
# 打桩 systemctl / systemd-run / download_and_validate / residential-helper.sh，验证：
#   A 首次（池非空、脚本缺、状态缺）：自愈下载 + 播种 600 空骨架 + helper reapply + systemd-run probe all + updated=1
#   B 第二次：零动作（幂等）
#   C 池空 + 残留 timer：disable --now + 删两个 unit 文件 + daemon-reload；再跑零动作
#   D 下载失败：不播种、不 reapply（下次 update 重试）
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$REPO/tests/resi-blacklist/lib.sh"
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
mkdir -p "$T/base" "$T/stubs" "$T/units"
export STUB_LOG="$T/stub.log"; : > "$STUB_LOG"

cat > "$T/stubs/systemctl" <<'EOF'
#!/usr/bin/env bash
echo "systemctl $*" >> "$STUB_LOG"; exit 0
EOF
cat > "$T/stubs/systemd-run" <<'EOF'
#!/usr/bin/env bash
echo "systemd-run $*" >> "$STUB_LOG"; exit 0
EOF
cat > "$T/base/residential-helper.sh" <<'EOF'
#!/usr/bin/env bash
echo "helper $*" >> "$STUB_LOG"; exit 0
EOF
chmod +x "$T/stubs/systemctl" "$T/stubs/systemd-run" "$T/base/residential-helper.sh"
export PATH="$T/stubs:$PATH"

# 抽块并包成函数（块内用了 local，必须在函数里执行）
{ echo 'd10_block() {'
  awk '/# v3\.7\.0 D10:/{p=1} /# v3\.6\.0 D9: /{p=0} p' "$REPO/server/update.sh"
  echo '}'; } > "$T/d10.sh"
assert_eq "yes" "$([[ $(wc -l < "$T/d10.sh") -gt 5 ]] && echo yes || echo no)" "从 update.sh 抽出了 D10 块"

# update.sh 运行环境打桩（与 update.sh 顶部变量同名）
BASE_DIR="$T/base"; GITHUB_RAW="https://example.invalid/main"; DOWNLOAD_URL=""
BLACKLIST_TIMER_FILE="$T/units/b-ui-resi-blacklist.timer"
print_success() { echo "[SUCCESS] $1"; }
print_warning() { echo "[WARNING] $1"; }
select_download_source() { DOWNLOAD_URL="$GITHUB_RAW"; }
download_and_validate() {
    echo "download $1 -> $2" >> "$STUB_LOG"
    [[ "${DL_FAIL:-0}" == "1" ]] && return 1
    printf '#!/usr/bin/env bash\necho fake-resi-blacklist "$@"\n' > "$2"
    return 0
}
. "$T/d10.sh"
STATE="$T/base/residential-blacklist.json"
SKELETON='{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}'

# --- A. 池非空 + 脚本缺 + 状态缺（3.6.3 → 3.7.0 后第一次进 D10）
echo '{"enabled":true,"global":true,"urls":[{"host":"brd.superproxy.io","port":44445,"username":"u","password":"p","type":"http"}]}' > "$T/base/residential-proxy.json"
updated=0; d10_block
assert_eq "1" "$updated" "A: updated=1"
assert_eq "1" "$(grep -c 'download .*server/resi-blacklist.sh -> ' "$STUB_LOG")" "A: 自愈下载了 resi-blacklist.sh"
assert_eq "yes" "$([[ -x "$T/base/resi-blacklist.sh" ]] && echo yes || echo no)" "A: resi-blacklist.sh 可执行"
assert_eq "yes" "$([[ -f "$STATE" ]] && echo yes || echo no)" "A: 状态文件已播种"
assert_eq "-rw-------" "$(ls -l "$STATE" | cut -c1-10)" "A: 状态文件权限 600"
assert_eq "$SKELETON" "$(jq -c . "$STATE")" "A: 状态文件是空骨架"
assert_eq "no" "$([[ -e "$STATE.tmp" ]] && echo yes || echo no)" "A: 无 .tmp 残留"
assert_eq "1" "$(grep -c '^helper reapply$' "$STUB_LOG")" "A: 调了一次 helper reapply（由它装 timer）"
assert_eq "1" "$(grep -c '^systemd-run --unit b-ui-resi-blacklist-probe-[0-9][0-9]* --quiet .*/resi-blacklist.sh probe all$' "$STUB_LOG")" "A: systemd-run 起了一次 probe all"
assert_eq "0" "$(grep -c '^systemctl' "$STUB_LOG")" "A: 池非空时 D10 自己不碰 systemctl"

# --- B. 第二次运行：零动作
sum_before=$(cksum "$STATE"); lines_before=$(wc -l < "$STUB_LOG")
updated=0; d10_block
assert_eq "0" "$updated" "B: 第二次 updated=0"
assert_eq "$lines_before" "$(wc -l < "$STUB_LOG")" "B: 第二次没有任何下载/helper/systemd-run 调用"
assert_eq "$sum_before" "$(cksum "$STATE")" "B: 状态文件未被改写"

# --- C. 池空 + 残留 timer → 移除；再跑零动作
echo '{"enabled":false,"global":true,"urls":[]}' > "$T/base/residential-proxy.json"
echo '[Timer]' > "$BLACKLIST_TIMER_FILE"; echo '[Service]' > "$T/units/b-ui-resi-blacklist.service"
updated=0; d10_block
assert_eq "yes" "$([[ ! -e "$BLACKLIST_TIMER_FILE" && ! -e "$T/units/b-ui-resi-blacklist.service" ]] && echo yes || echo no)" "C: timer/service 文件已删除"
assert_eq "1" "$(grep -c '^systemctl disable --now b-ui-resi-blacklist.timer$' "$STUB_LOG")" "C: disable --now 了 timer"
assert_eq "1" "$(grep -c '^systemctl daemon-reload$' "$STUB_LOG")" "C: daemon-reload 一次"
assert_eq "0" "$updated" "C: 清理 timer 不置 updated（与 D8 一致）"
lines_before=$(wc -l < "$STUB_LOG")
updated=0; d10_block
assert_eq "$lines_before" "$(wc -l < "$STUB_LOG")" "C: 池空第二次零动作"

# --- D. 池非空 + 脚本缺 + 下载失败 → 不播种、不 reapply、不探测
rm -f "$T/base/resi-blacklist.sh" "$STATE"
echo '{"enabled":true,"urls":[{"host":"h","port":1,"username":"u","password":"p"}]}' > "$T/base/residential-proxy.json"
lines_before=$(wc -l < "$STUB_LOG")
export DL_FAIL=1; updated=0; d10_block; unset DL_FAIL
assert_eq "0" "$updated" "D: 下载失败 updated=0"
assert_eq "no" "$([[ -e "$STATE" ]] && echo yes || echo no)" "D: 下载失败不播种状态文件"
assert_eq "0" "$(tail -n +$((lines_before + 1)) "$STUB_LOG" | grep -c '^helper\|^systemd-run')" "D: 下载失败不调 helper / 不探测"

finish
```

- [ ] **Step 7: Run it, confirm the expected failure**

Run: `bash tests/resi-blacklist/test_update_d10.sh`

Expected: first line `FAIL - 从 update.sh 抽出了 D10 块 (expected: [yes] actual: [no])` (awk extracts nothing, `d10.sh` has 2 lines), then every A/B/C/D assertion that depends on the block fails (`updated=1`, download, state file …); last line `PASS 10 / FAIL 12` (the passes are the negative checks such as `updated=0`), exit status 1.

- [ ] **Step 8: Insert the D10 block in `server/update.sh`**

`server/update.sh:1193-1196` currently reads (end of D8, then the D9 call):

```bash
    fi

    # v3.6.0 D9: 服务端出站 IPv4-only（hy2 direct mode 4 / xray ForceIPv4）
    migrate_ipv4_only_egress
```

Replace with:

```bash
    fi

    # v3.7.0 D10: 住宅代理自动黑名单（机房直出）—— 自愈下载 resi-blacklist.sh + 首次播种状态文件 + 初始探测
    # 与 D7/D8 同类 chicken-and-egg：resi-blacklist.sh 是 v3.7.0 新增，3.6.x → 3.7.0 那一刻内存里跑的是
    # 旧 update.sh（file_map 没有它）→ 版本变 3.7.0 后同版本不再下载 → 文件永远缺。这里缺失/为空就补下。
    # 幂等：状态文件存在即不再播种/探测；timer 由 residential-helper.sh reapply 维护（Task 6 ensure_blacklist_timer），
    # 池空/未启用时确保 timer 不存在（与 D8 对 resi-health.timer 的处理一致）。
    if command -v jq >/dev/null 2>&1; then
        local _bl_script="${BASE_DIR}/resi-blacklist.sh"
        local _bl_state="${BASE_DIR}/residential-blacklist.json"
        local _bl_rcfg="${BASE_DIR}/residential-proxy.json"
        local _bl_timer="${BLACKLIST_TIMER_FILE:-/etc/systemd/system/b-ui-resi-blacklist.timer}"
        local _bl_pool=0
        if [[ -f "$_bl_rcfg" ]]; then
            _bl_pool=$(jq -r 'if (.enabled // false) == true then ((.urls // []) | length) else 0 end' "$_bl_rcfg" 2>/dev/null || echo 0)
        fi
        if [[ "${_bl_pool:-0}" -gt 0 ]]; then
            if [[ ! -s "$_bl_script" ]]; then
                select_download_source 2>/dev/null || true
                download_and_validate "${DOWNLOAD_URL:-$GITHUB_RAW}/server/resi-blacklist.sh" "$_bl_script" 2>/dev/null \
                    || print_warning "  D10: resi-blacklist.sh 缺失且下载失败，本次跳过（下次 update 重试）"
            fi
            [[ -f "$_bl_script" ]] && chmod +x "$_bl_script"
            if [[ -s "$_bl_script" ]] && [[ ! -f "$_bl_state" ]]; then
                # 预置为空：内置表只是候选，初始黑名单由下面这次探测决定（spec §2）
                printf '%s\n' '{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}' > "${_bl_state}.tmp" \
                    && chmod 600 "${_bl_state}.tmp" && mv "${_bl_state}.tmp" "$_bl_state"
                # reapply 装 b-ui-resi-blacklist.timer；空黑名单 → relay 配置逐字节不变 → 不重启
                "${BASE_DIR}/residential-helper.sh" reapply >/dev/null 2>&1 || true
                # 初始探测异步跑（一轮最长约 10 分钟，不能阻塞 update）；探测完由 helper blacklist-apply 落地
                if command -v systemd-run >/dev/null 2>&1; then
                    systemd-run --unit "b-ui-resi-blacklist-probe-$(date +%s)" --quiet "$_bl_script" probe all >/dev/null 2>&1 || true
                else
                    nohup "$_bl_script" probe all >/dev/null 2>&1 &
                fi
                print_success "  ✓ D10 住宅黑名单已初始化（空状态文件 + 每日北京时间 04:00 刷新 timer），初始探测已在后台启动"
                updated=1
            fi
        else
            # 池空/未启用 → 黑名单 timer 不应存在
            if [[ -f "$_bl_timer" ]]; then
                systemctl disable --now b-ui-resi-blacklist.timer 2>/dev/null || true
                rm -f "$_bl_timer" "${_bl_timer%.timer}.service"
                systemctl daemon-reload 2>/dev/null || true
            fi
        fi
    fi

    # v3.6.0 D9: 服务端出站 IPv4-only（hy2 direct mode 4 / xray ForceIPv4）
    migrate_ipv4_only_egress
```

- [ ] **Step 9: Run both tests and the syntax checks**

Run:

```bash
bash -n install.sh server/update.sh server/b-ui-cli.sh server/residential-helper.sh \
  && bash tests/resi-blacklist/test_release_files.sh \
  && bash tests/resi-blacklist/test_update_d10.sh
```

Expected: no `bash -n` output; `test_release_files.sh` ends with `PASS 53 / FAIL 0`; `test_update_d10.sh` prints 22 `ok` lines (plus one `[SUCCESS] ✓ D10 …` and one `[WARNING] D10: …` line from the stubbed printers) and ends with `PASS 22 / FAIL 0`; overall exit 0. (Both numbers were measured by running these exact scripts against a scratch copy of the worktree with the Step 4/5/8 edits applied.) If `shellcheck` is installed also run `shellcheck -S error server/update.sh install.sh server/b-ui-cli.sh` (expected: no output).

- [ ] **Step 10: Commit**

```bash
git add version.json install.sh server/update.sh server/b-ui-cli.sh tests/resi-blacklist/lib.sh tests/resi-blacklist/test_release_files.sh tests/resi-blacklist/test_update_d10.sh
git commit -m "feat(update): D10 住宅黑名单迁移（自愈下载 resi-blacklist.sh、播种状态、装 timer、后台初探）+ 四处文件表登记 + 卸载清理

- update.sh apply_systemd_configs 新增幂等块 D10：池非空且无状态文件时播种空骨架、helper reapply 装 timer、systemd-run 跑一次 probe all；池空则移除 timer
- version.json files[] / install.sh 文件表 / update.sh 交互表与 auto file_map 登记 server/resi-blacklist.sh 并 chmod +x
- b-ui-cli.sh 卸载时停用并删除 b-ui-resi-blacklist.timer/.service 与 probe transient unit
- tests/resi-blacklist/test_release_files.sh、test_update_d10.sh（awk 抽块 + stub 干跑，验证第二次零变更）

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: Guide §11, `version.json` 3.7.0 changelog, CLAUDE.md, release commit

**Files:**
- Modify: `docs/residential-proxy-guide.md:322` (append a new `## 11.` section after the end of §10)
- Modify: `version.json:2` (`"version": "3.6.3"`), `version.json:19-20` (`"changelog": {` + first entry)
- Modify: `CLAUDE.md` — insert one paragraph after the paragraph that starts with `` `127.0.0.1:2080` is a permanent local sing-box relay `` (currently line 37) and before `The VPS has no IPv6 egress:`; add one line after `BASE_DIR=/tmp/t bash server/residential-helper.sh reapply` (currently line 78) in "Development Commands"
- Test: `jq empty version.json`, `bash tests/resi-blacklist/test_release_files.sh`, full syntax sweep, all `tests/resi-blacklist/test_*.sh`

**Interfaces:**
- Consumes: everything Tasks 1–10 produced (CLI names `probe|learn|refresh|status|pin|forget`, `blacklist-apply`, the panel `<details id="resi-blacklist-details">`, units `b-ui-resi-blacklist.timer`/`.service`, files `residential-blacklist.json`, `/var/log/b-ui-resi-blacklist.log`), the existing changelog style of 3.6.1–3.6.3 (one `feat(...)`/`fix(...)` headline, then `\n\n**粗体小节**：…` paragraphs).
- Produces: `version.json` `version` = `3.7.0` with changelog key `"3.7.0"` as the first entry; guide §11 「机房直出黑名单（自动检测）」; CLAUDE.md paragraph; the release commit `bump: v3.7.0 …`.

- [ ] **Step 1: Append §11 to the guide**

`docs/residential-proxy-guide.md` currently ends (lines 320-322) with:

```markdown
同理，如果你的供应商拒绝表里的其它域名，面板改关键字就行，不用等版本更新；改过之后这台
服务器就不再跟随默认表，后续默认表扩充要自己点「恢复默认」再补上自定义项。
```

Append after it (keep one blank line before the `---`):

```markdown

---

## 11. 机房直出黑名单（自动检测）

第 10 节说的是「哪些域名走住宅」；这一节相反：**默认全部走住宅（global 模式）时，哪些目标住宅
上游根本代理不了，得自动送去 VPS 直出**。v3.7.0 起由 `/opt/b-ui/resi-blacklist.sh` 自动维护，
按上游（`host:port`）分别记录，面板可看、可重测、可钉住。

### 11.1 它解决什么

Bright Data 这类供应商按策略拒绝一部分目标：搜索（`policy_20110`）、支付处理商和 TikTok
（`policy_20050`）、非白名单端口。global 模式下这些连接直接失败——2026-09-11 在 bwg-tizi 的
relay 日志里每 30 分钟约 256 条 `403 Forbidden`，目标是 `www.google.com`、`gateway.icloud.com`、
`courier.push.apple.com`、`<ip>:5228`（FCM 推送）、`<ip>:5223`（APNs）。黑名单就是把这些「这个
上游代理不了」的目标挑出来，从机房直出；其它一切照旧走住宅。

### 11.2 怎么判定（不是写死的表）

- **候选**来自两处：脚本内置的候选表（搜索 / 短视频社交 / 支付处理商 / Apple 服务 / 推送端口，
  都是具体主机名）+ 从 `b-ui-relay` 日志学到的被拒目标（同一上游 24 小时内同一主机名 ≥3 次；
  裸 IP 按端口聚合，≥3 次且来自 ≥2 个 IP 才算，80/443/8080/8443 永不聚合）。
- **进黑名单**：对候选做两次上游探测（间隔 3 秒）+ 一次直连对照，**两次都被上游拒绝且直连能通**
  才进；直连也不通的目标一律不碰（不是代理的错）。超时视为「未知」，不改状态；上游本身连不上
  （或 407 鉴权失败）则整轮跳过、保留旧黑名单。
- **出黑名单**：条目每日复测，连续两次刷新都能通才移除，避免供应商偶发抖动来回翻。
- **新上游从空开始**：录入（面板添加 / `enable`）成功后立刻在后台探测一次，那次结果就是它的
  初始黑名单；升级到 3.7.0 的老机器由 `update.sh` D10 做同样的一次初始探测。
- 域名条目是**探测过的那个主机名本身**（`domain_suffix`）：`www.google.com` 进黑名单不影响
  `gemini.google.com`、`accounts.google.com`，它们继续走住宅；自动逻辑永远不写 `google.com` 这种 apex。
- 生效的规则插在 relay 路由里私网直连之后、关键字规则之前：钉住强制住宅 → 黑名单 + 钉住强制机房
  → 黑名单端口 → 其余按 global / 关键字。

### 11.3 在哪看

- **面板**：住宅弹窗「代理节点池」下方的折叠区「机房直出黑名单（自动检测）」。徽标显示
  `N 条 · 最近检测 HH:MM`（检测中显示「检测中…」）；展开后按上游列出目标 / 来源（`builtin:*` 或
  `learned`）/ 拒绝原因（供应商返回的状态行，如 `403 Forbidden serp domain`）/ 加入时间，下面是
  候选池和「立即重新检测」按钮。首页「住宅 IP 健康」卡的线路表多了一列「黑名单」（每条线路的条数）。
- **命令行**：

  ```bash
  /opt/b-ui/resi-blacklist.sh status          # 人类可读
  /opt/b-ui/resi-blacklist.sh status --json   # 原样输出状态文件
  tail -50 /var/log/b-ui-resi-blacklist.log   # 探测/学习日志（每次运行后 tail -300 轮转）
  ```

- **文件**：状态 `/opt/b-ui/residential-blacklist.json`（600，`learned` 里是用户真实访问过的目标，
  只在鉴权后的面板可见）；生效规则最终写在 `/opt/b-ui/singbox-relay.json` 的 `route.rules` 里。

### 11.4 钉住（人工覆盖自动判定）

面板黑名单区底部：输入目标（`www.example.com` 或 `port:5228`）→ 选「强制机房」或「强制住宅」→
「钉住」。已钉列表可逐条移除。等价命令：

```bash
/opt/b-ui/resi-blacklist.sh pin www.example.com direct   # 强制机房直出（无需探测）
/opt/b-ui/resi-blacklist.sh pin www.example.com resi     # 强制住宅（即使被判拒绝也不进 direct 规则）
/opt/b-ui/resi-blacklist.sh pin www.example.com clear    # 取消钉住
```

钉住是全局的（不分上游），改完立即重写 relay 配置并重启 `b-ui-relay`（会掐断现有住宅连接，客户端自动重连）。

### 11.5 什么时候刷新、什么时候重启

| 时机 | 动作 | 是否重启 relay |
|---|---|---|
| 面板添加上游 / `enable` / `enable --add` | 后台立即探测该上游 | 探出条目才重写并重启 |
| 删除上游（`enable --remove`） | 删该上游的数据 | 有变化才重启 |
| `resi-health.sh` 切换上游 | 切到对应上游的黑名单 | 新旧生效黑名单不同才重启（同供应商通常无变化） |
| 每日刷新 `b-ui-resi-blacklist.timer` | 学习日志 → 复测全部上游 → 应用 | 有变化才重启 |
| 面板「立即重新检测」/ 钉住 | 同上 / 立即应用 | 有变化才重启 / 立即 |

定时器按**北京时间 04:00** 触发（`OnCalendar=*-*-* 04:00:00 Asia/Shanghai`，再随机延后 0–30 分钟；
生产机时区是 UTC 也照北京时间跑）。看下次触发：

```bash
TZ=Asia/Shanghai systemctl list-timers b-ui-resi-blacklist.timer --no-pager
```

手动强制全量重测（约 1–10 分钟，异步跑完自动应用）：

```bash
/opt/b-ui/resi-blacklist.sh probe all                       # 前台跑完再应用
# 或与面板/升级一致的后台方式：
systemd-run --unit "b-ui-resi-blacklist-probe-$(date +%s)" --quiet /opt/b-ui/resi-blacklist.sh probe all
journalctl -u 'b-ui-resi-blacklist-probe-*' --no-pager -o cat | tail -20
```

只重测一条上游：`/opt/b-ui/resi-blacklist.sh probe brd.superproxy.io:44445`；只应用不探测：
`/opt/b-ui/residential-helper.sh blacklist-apply`（无变化时打印「黑名单无变化，保持运行」，不重启）。

### 11.6 常见问题

- **面板提示「上次应用失败」**：`applied.at` 早于 `checkedAt`，通常是新配置没过 `sing-box check`，
  旧配置被保留、relay 没动。看 `/var/log/b-ui-resi-blacklist.log` 与 `journalctl -u b-ui-relay`。
- **3.6.x 升 3.7.0 后黑名单区一直是空**：第一次 `update.sh -y` 只下载文件，D10（自愈下载脚本、
  播种状态、装 timer、后台初探）在**第二次**运行时才执行（「幂等自愈检查」路径）。再跑一次
  `bash /opt/b-ui/update.sh -y` 即可；第三次起不再有任何变更。
- **黑名单不会被清空**：探测网络异常、上游整体不可用、日志读不到，都只是「本轮跳过」，旧黑名单保留。
```

- [ ] **Step 2: Bump `version.json` and add the 3.7.0 changelog entry**

`version.json:2` currently reads:

```json
    "version": "3.6.3",
```

Replace with:

```json
    "version": "3.7.0",
```

`version.json:19-20` currently starts the changelog with (the 3.6.3 line is long; only its beginning is shown — anchor on `"changelog": {` followed by `"3.6.3": "fix(client)`):

```json
    "changelog": {
        "3.6.3": "fix(client): TUN 模式打不开 —— TUN inbound 补回 `interface_name: bui-tun`（TUN schema 7 → 8）。
```

Insert the new entry between those two lines so the changelog starts with:

```json
    "changelog": {
        "3.7.0": "feat(residential): 住宅代理自动黑名单（机房直出）—— 按上游探测供应商拒绝的目标并自动送去 VPS 直出，面板可看、可重测、可钉住，每日北京时间 04:00 后台刷新。\n\n**问题**：Bright Data 等供应商按策略拒绝搜索（policy_20110）、支付处理商 / TikTok（policy_20050）与非白名单端口。bwg-tizi 全局住宅模式下 relay 日志每 30 分钟约 256 条 `403 Forbidden`：`www.google.com`、`gateway.icloud.com`、`courier.push.apple.com`、`<ip>:5228`（FCM）、`<ip>:5223`（APNs），这些连接直接失败；改用关键字白名单又太脆（AI 域名漏一个就出问题）。\n\n**方案**：默认全部走住宅，只把「这个上游代理不了的目标」送去机房直出。新增 `resi-blacklist.sh`：内置候选表（搜索 / 短视频社交 / 支付处理商 / Apple 服务 / 推送端口，具体主机名）+ 从 relay 日志学习被拒目标（域名 24h ≥3 次；裸 IP 按端口聚合 ≥3 次且 ≥2 个 IP，80/443/8080/8443 不聚合），每个候选两次上游探测（间隔 3s）+ 直连对照，两次都被拒且直连能通才进黑名单，连续两次刷新能通才移除；超时不改状态、上游整体不可用本轮跳过，绝不因一轮失败清空。黑名单按上游 host:port 分别维护，预置为空，录入上游时立即后台探测一次。relay 路由在私网直连之后、关键字之前插入 `domain_suffix → resi-pool`（钉住强制住宅）/ `domain_suffix → direct`（黑名单 + 钉住强制机房）/ `port → direct`（黑名单端口）三条规则，域名条目只写探测过的主机名本身（`www.google.com` 不影响 `gemini.google.com`），空黑名单时配置与 3.6.x 逐字节一致、不重启。`residential-helper.sh blacklist-apply` 是 relay 配置唯一写入者：按当前选中上游生成 → 摘要相同则不动 → `sing-box check` 通过才 mv + 重启；`resi-health.sh` 切换上游后跟着应用对应黑名单。两把锁分离：`.blacklist.lock` 只保护状态文件，`.relay.lock` 只保护 relay 配置。\n\n**面板**：住宅弹窗新增「机房直出黑名单（自动检测）」折叠区——按上游列条数 / 目标 / 来源 / 拒绝原因 / 加入时间，候选池，「立即重新检测」（异步，5s 轮询到检测结束），钉住（强制机房 / 强制住宅，可移除）；「住宅 IP 健康」卡线路表加「黑名单」列。新增接口 GET/POST `/api/residential/blacklist`、`/probe`、`/pins`。\n\n**升级**：update.sh 新增幂等块 D10——自愈下载 `resi-blacklist.sh`（同 D8 套路），住宅池非空且无状态文件时播种空状态、`reapply` 安装 `b-ui-resi-blacklist.timer`（`OnCalendar=*-*-* 04:00:00 Asia/Shanghai`，随机延后 30 分钟，`Persistent=true`）并 `systemd-run` 后台跑一次初始探测；池空则移除 timer。**注意**：3.6.x → 3.7.0 第一次 `update.sh -y` 只下载文件，D10 在第二次运行（「幂等自愈检查」路径）才执行，升级后请连跑两次。状态文件 `/opt/b-ui/residential-blacklist.json`（600），日志 `/var/log/b-ui-resi-blacklist.log`；卸载时一并清理 timer。指南新增第 11 节「机房直出黑名单（自动检测）」。",
        "3.6.3": "fix(client): TUN 模式打不开 —— TUN inbound 补回 `interface_name: bui-tun`（TUN schema 7 → 8）。
```

(`"updated": "2026-09-11"` on line 3 already matches today's date; leave it.)

Run: `jq -r '.version, (.changelog | keys_unsorted | .[0]), (.files | index("server/resi-blacklist.sh"))' version.json`

Expected:

```
3.7.0
3.7.0
6
```

- [ ] **Step 3: CLAUDE.md — one paragraph under "Server topology" and one dev command**

In `CLAUDE.md`, the paragraph beginning `` `127.0.0.1:2080` is a permanent local sing-box relay `` currently ends with:

```markdown
… State: `/opt/b-ui/residential-proxy.json` (chmod 600), `.resi-health-state.json`, lock `.relay.lock` (`flock`, taken only by `residential-helper.sh` writers; the health script is read-only and switches via the Clash API).

The VPS has no IPv6 egress: every server egress is pinned to IPv4, …
```

Insert this paragraph between them (blank line on each side):

```markdown
Residential auto-blacklist (v3.7.0, spec `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`): `resi-blacklist.sh` (`probe [<host:port>|all]`, `learn`, `refresh`, `status [--json]`, `pin <target> direct|resi|clear`, `forget <host:port>`) probes a builtin candidate table plus targets learned from `b-ui-relay` journal lines (`unexpected status: 4xx/5xx` / SOCKS5 refusals, `resi-N` mapped back to `host:port` through the pool order in `singbox-relay.json`) and keeps per-upstream entries in `/opt/b-ui/residential-blacklist.json` (chmod 600, lock `.blacklist.lock`, log `/var/log/b-ui-resi-blacklist.log`). A target enters only when two upstream attempts are refused **and** a direct control succeeds; it leaves after two consecutive daily "ok" checks. The script never writes the relay config: it calls `residential-helper.sh blacklist-apply`, which regenerates `singbox-relay.json` for the currently selected upstream (`selected_upstream_key`, Clash API `.now` → `resi-N` → `urls[N-1]`), inserts `domain_suffix→direct` / `port→direct` / pinned `domain_suffix→resi-pool` rules right after the private-IP rule, validates with `sing-box check`, and restarts `b-ui-relay` only when the file digest changed (empty blacklist ⇒ byte-identical to v3.6.x output ⇒ no restart). Units: `b-ui-resi-blacklist.timer` (`OnCalendar=*-*-* 04:00:00 Asia/Shanghai`, `RandomizedDelaySec=30min`) → `resi-blacklist.sh refresh`; one-shot `b-ui-resi-blacklist-probe-<epoch>` transient units via `systemd-run` from `enable`, the panel (`POST /api/residential/blacklist/probe`) and `update.sh` D10. `resi-health.sh` calls `blacklist-apply` after a successful switch. Pins are global (`pins` in the state file). Tests are plain bash under `tests/resi-blacklist/` (not in `version.json`, never deployed).
```

In "Development Commands", after the line

```bash
BASE_DIR=/tmp/t bash server/residential-helper.sh reapply
```

add:

```bash

# Residential auto-blacklist tests (plain bash, stubs on PATH, no framework; sing-box checks skipped if binaries absent)
for t in tests/resi-blacklist/test_*.sh; do bash "$t" || break; done
```

- [ ] **Step 4: Full pre-release verification**

Run:

```bash
bash -n install.sh server/core.sh server/update.sh server/b-ui-cli.sh server/residential-helper.sh server/resi-health.sh server/resi-blacklist.sh b-ui-client.sh \
  && node --check web/server.js && node --check web/app.js \
  && jq empty version.json \
  && for t in tests/resi-blacklist/test_*.sh; do echo "== $t"; bash "$t" || exit 1; done \
  && git status --short
```

Expected: no syntax errors; every test ends with `PASS n / FAIL 0`; `git status --short` lists exactly ` M CLAUDE.md`, ` M docs/residential-proxy-guide.md`, ` M version.json`. If `shellcheck` exists: `shellcheck -S error server/*.sh b-ui-client.sh install.sh` prints nothing.

- [ ] **Step 5: Release commit**

```bash
git add version.json docs/residential-proxy-guide.md CLAUDE.md
git commit -m "bump: v3.7.0 住宅代理自动黑名单（机房直出）—— 按上游探测 + 日志学习、面板可看可钉、每日 04:00 刷新、update.sh D10 迁移

- version.json 3.6.3 → 3.7.0，changelog 记录问题 / 方案 / 面板 / 升级注意（升级后需连跑两次 update.sh）
- docs/residential-proxy-guide.md 新增第 11 节「机房直出黑名单（自动检测）」
- CLAUDE.md Server topology 补黑名单文件 / 单元 / 锁 / 测试目录说明

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: Integration verification on `bwg-tizi` (production)

**Files:**
- Test only: no repo files change. Optional at the end: append a note to the user's memory file `/Users/woo/.claude/projects/-Users-woo-Desktop-b-ui/memory/b-ui-update-quirks.md` (Step 12).

**Interfaces:**
- Consumes: GitHub `main` served by `https://raw.githubusercontent.com/Buxiulei/b-ui/main` (what `update.sh` downloads), `ssh -o BatchMode=yes -J bwg-rick bwg-tizi` (root; direct SSH to :<ssh-port> is cut by the residential egress, see memory), `ssh baiyi` (client host, `sudo -n bui-c …`, local SOCKS `127.0.0.1:1080` from the active node), `/opt/b-ui/update.sh -y`, `/opt/b-ui/resi-blacklist.sh probe|pin|status`, `/opt/b-ui/residential-helper.sh blacklist-apply`, Clash API `127.0.0.1:9091/proxies/resi-pool`.
- Produces: a verified 3.7.0 deployment on `bwg-tizi`; expected observations recorded below; rollback recipe.

Facts to keep in mind (from memory notes): `bwg-tizi` = bwg-tizi, global residential mode is intentional (do not suggest turning it off), the VPS has no IPv6, the panel is `<bwg-tizi-domain>`, the client host `baiyi` has a node for `<bwg-tizi-domain>:40000` (HY2 residential, named `HY2` on 2026-09-11) and normally runs `hysteria2-1785892136` with TUN on — restore that at the end. `update.sh` restarts `hysteria-server` in some cases and the SSH session can drop; always run it detached with `nohup` and re-verify services afterwards.

Define once in the local shell (used by every step):

```bash
SSH='ssh -o BatchMode=yes -J bwg-rick bwg-tizi'
```

- [ ] **Step 1: Push `main` and confirm GitHub Raw serves 3.7.0**

```bash
git log --oneline -3                       # 顶部应是 "bump: v3.7.0 …"
git push origin Baiyi/bui-c-tun-mode-issue-df82e5:main
sleep 60   # raw.githubusercontent.com 有约 1 分钟缓存
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/main/version.json | jq -r .version
curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/main/server/resi-blacklist.sh | head -1
```

Expected: push is a fast-forward (if it is rejected as non-fast-forward, `git fetch origin main && git rebase origin/main` and push again — never force-push `main`); the two curls print `3.7.0` and `#!/bin/bash` (or `#!/usr/bin/env bash`, whatever Task 2 used).

- [ ] **Step 2: Baseline on `bwg-tizi` before touching anything**

```bash
$SSH 'jq -r .version /opt/b-ui/version.json;
      systemctl is-active hysteria-server hysteria-residential xray b-ui-admin caddy b-ui-relay;
      jq -c "{enabled, global, n: (.urls|length), urls: [.urls[] | \"\(.host):\(.port)\"]}" /opt/b-ui/residential-proxy.json;
      cp -a /opt/b-ui/singbox-relay.json /opt/b-ui/singbox-relay.json.bak.pre-3.7.0;
      md5sum /opt/b-ui/singbox-relay.json;
      systemctl show b-ui-relay -p ActiveEnterTimestamp --value;
      ls /opt/b-ui/resi-blacklist.sh /opt/b-ui/residential-blacklist.json /etc/systemd/system/b-ui-resi-blacklist.timer 2>&1 | head -3;
      date -u +%FT%TZ'
```

Expected: `3.6.3`; six `active` lines; `{"enabled":true,"global":true,"n":N,…}` with N ≥ 1 (the upstream keys, e.g. `brd.superproxy.io:44445`); an md5; a timestamp (save it as `RELAY_T0`); three `No such file or directory` lines (nothing from 3.7.0 exists yet); save the UTC time as `UPDATE_START`.

- [ ] **Step 3: First `update.sh -y` (downloads 3.7.0), detached**

```bash
$SSH 'nohup bash /opt/b-ui/update.sh -y > /var/log/b-ui-update-3.7.0-run1.log 2>&1 &
      echo started'
sleep 90
$SSH 'tail -n 15 /var/log/b-ui-update-3.7.0-run1.log'
```

Expected: `started`; after the sleep the log tail contains `更新完成！` and `新版本: v3.7.0` (if not yet, wait 30 s and tail again; `npm install` can take a minute). Because the old `update.sh` is the one running, the log must **not** contain `D10` — that is expected. Then re-verify (the memory note: SSH may have dropped mid-run):

```bash
$SSH 'jq -r .version /opt/b-ui/version.json;
      systemctl is-active hysteria-server hysteria-residential xray b-ui-admin caddy b-ui-relay;
      ls -l /opt/b-ui/resi-blacklist.sh 2>&1;
      grep -c "ensure_blacklist_timer\|blacklist-apply" /opt/b-ui/residential-helper.sh;
      ls /etc/systemd/system/b-ui-resi-blacklist.timer 2>&1;
      md5sum /opt/b-ui/singbox-relay.json; systemctl show b-ui-relay -p ActiveEnterTimestamp --value'
```

Expected: `3.7.0`; six `active` (if any is `inactive`, `systemctl start` it and rerun `/opt/b-ui/residential-helper.sh reapply` per the memory note); `resi-blacklist.sh`: **No such file** (old file table, expected); helper grep count ≥ 3 (new helper is in place); the timer file exists (the new helper's `reapply` from the tail of `do_update` installed it — pool non-empty); `singbox-relay.json` md5 **unchanged** and `ActiveEnterTimestamp == RELAY_T0` (empty blacklist ⇒ byte-identical config ⇒ no restart; this is the spec §3 zero-change guarantee).

- [ ] **Step 4: Second `update.sh -y` — D10 runs exactly once**

```bash
$SSH 'nohup bash /opt/b-ui/update.sh -y > /var/log/b-ui-update-3.7.0-run2.log 2>&1 &
      echo started'
sleep 40
$SSH 'grep -n "幂等自愈检查\|D10" /var/log/b-ui-update-3.7.0-run2.log;
      ls -l /opt/b-ui/resi-blacklist.sh; stat -c "%a %U" /opt/b-ui/residential-blacklist.json;
      jq -c . /opt/b-ui/residential-blacklist.json | cut -c1-120;
      TZ=Asia/Shanghai systemctl list-timers b-ui-resi-blacklist.timer --no-pager;
      systemctl cat b-ui-resi-blacklist.timer | grep -E "OnCalendar|RandomizedDelaySec|Persistent";
      systemctl list-units "b-ui-resi-blacklist-probe-*" --all --no-pager --plain | head -3'
```

Expected:
- log has one `幂等自愈检查（无残留时为 no-op）...` line and exactly one `✓ D10 住宅黑名单已初始化…` line;
- `-rwxr-xr-x … /opt/b-ui/resi-blacklist.sh`; `600 root`;
- the state file is the empty skeleton or already has `"checking":{"upstream":"all",…}` (probe started);
- `list-timers` shows one row whose NEXT is today's or tomorrow's `04:00–04:30 CST` (if the current Beijing time is past 04:30 it shows tomorrow); `OnCalendar=*-*-* 04:00:00 Asia/Shanghai`, `RandomizedDelaySec=30min`, `Persistent=true`;
- one `b-ui-resi-blacklist-probe-<epoch>.service` unit `active running` (or already `inactive` if fast).

Then the idempotence check — a third run must change nothing:

```bash
$SSH 'S=$(md5sum /opt/b-ui/residential-blacklist.json); bash /opt/b-ui/update.sh -y 2>&1 | grep -c "D10"; echo "$S"; md5sum /opt/b-ui/residential-blacklist.json'
```

Expected: `0` (no D10 line — the interactive run here is short because `apply_systemd_configs` is a no-op, so running it in the foreground is acceptable; if you prefer, detach as above), and the two md5 lines are equal *unless* the background probe finished in between (then the state grew — compare `.upstreams|keys` instead: still the same upstream keys).

- [ ] **Step 5: Wait for the initial probe and inspect the results**

```bash
until $SSH 'systemctl list-units "b-ui-resi-blacklist-probe-*" --no-pager --plain | grep -q running' ; do :; done 2>/dev/null; echo "probe still running, waiting"
while $SSH 'systemctl list-units "b-ui-resi-blacklist-probe-*" --no-pager --plain | grep -q running'; do sleep 20; done
$SSH 'journalctl -u "b-ui-resi-blacklist-probe-*" --since "-30min" --no-pager -o cat | tail -30;
      echo ---; tail -40 /var/log/b-ui-resi-blacklist.log'
```

(The first `until` line only prints the notice; the `while` loop is the real wait — a full round is ≤ 10 min, typically 1–3 min.)

Expected: the journal/log show the round for every upstream, a summary line per upstream (`entries` count, blocked/ok/unknown counts), and the `blacklist-apply` result `黑名单有变化，b-ui-relay 已重启`. No `ERROR`, no `上游不可用` (if the latter appears, the upstream itself was unreachable — rerun `probe all` later, the state is untouched by design).

Now the content. Find the selected upstream key exactly like `selected_upstream_key` does, then query the state:

```bash
$SSH 'N=$(curl -s --max-time 2 127.0.0.1:9091/proxies/resi-pool | jq -r ".now" | sed "s/^resi-//"); N=${N:-1};
      KEY=$(jq -r --argjson n "$N" ".urls[\$n-1] | \"\(.host|ascii_downcase):\(.port)\"" /opt/b-ui/residential-proxy.json); echo "KEY=$KEY";
      jq -r --arg k "$KEY" ".upstreams[\$k] | .checkedAt, (.entries | keys[])" /opt/b-ui/residential-blacklist.json;
      echo "--- gemini present? (must be 0)"; jq -r --arg k "$KEY" "[.upstreams[\$k].entries | keys[] | select(. == \"gemini.google.com\")] | length" /opt/b-ui/residential-blacklist.json;
      echo "--- learned"; jq -c --arg k "$KEY" ".learned[\$k] // {} | to_entries | map({t: .key, c: .value.count})" /opt/b-ui/residential-blacklist.json;
      echo "--- checking/applied"; jq -c "{checking, applied}" /opt/b-ui/residential-blacklist.json'
```

Expected (based on the 2026-09-11 Bright Data measurements; a host that the vendor has since unblocked is a data difference, not a bug):
- `checkedAt` is a fresh ISO timestamp;
- entry keys include `www.google.com` (`source: builtin:search`), `port:5228` and `port:5223` (from the builtin push probes `mtalk.google.com:5228` / `courier.push.apple.com:5223`, or `learned` if the journal had them first — either is acceptable), and the Apple push hosts `courier.push.apple.com`, `gateway.icloud.com`, `query.ess.apple.com` (`builtin:apple` or `learned`);
- the gemini count prints `0` (never a candidate, never blocked);
- `learned` lists the journal-derived targets with counts ≥ 3;
- `checking` is `null`, `applied.upstream == KEY`, `applied.digest` equals `md5sum /opt/b-ui/singbox-relay.json`, `applied.at ≥ checkedAt`.

- [ ] **Step 6: Relay config has the rules and `b-ui-relay` restarted exactly once**

```bash
$SSH 'echo "--- direct domain rule"; jq -c ".route.rules[] | select(.outbound==\"direct\" and .domain_suffix) | .domain_suffix" /opt/b-ui/singbox-relay.json;
      echo "--- direct port rule";   jq -c ".route.rules[] | select(.outbound==\"direct\" and .port) | .port" /opt/b-ui/singbox-relay.json;
      echo "--- resi pin rule (none yet)"; jq -c "[.route.rules[] | select(.outbound==\"resi-pool\" and .domain_suffix)] | length" /opt/b-ui/singbox-relay.json;
      echo "--- rule order"; jq -r ".route.rules | to_entries[] | \"\(.key) \(.value.outbound // .value.action) \(.value | keys - [\"outbound\",\"action\"] | join(\",\"))\"" /opt/b-ui/singbox-relay.json;
      /opt/b-ui/sing-box check -c /opt/b-ui/singbox-relay.json && echo "sing-box check ok";
      echo "--- relay restarts since update start"; journalctl -u b-ui-relay --since "'"$UPDATE_START"'" --no-pager | grep -c "Started B-UI Outbound Relay";
      systemctl show b-ui-relay -p ActiveEnterTimestamp --value; systemctl is-active b-ui-relay'
```

Expected: one `domain_suffix` array containing `www.google.com` and the Apple hosts; one `port` array containing `5223` and `5228` (integers, sorted); pin-rule count `0`; in the rule order listing the `direct domain_suffix` and `direct port` rules come right after the `ip_cidr` direct rule and before any `domain_keyword` rule (global mode: `final` is `resi-pool`, so there may be no keyword rule at all); `sing-box check ok`; restart count `1`; `ActiveEnterTimestamp` is newer than `RELAY_T0` (save as `RELAY_T1`); `active`.

- [ ] **Step 7: A second `blacklist-apply` is a no-op**

```bash
$SSH '/opt/b-ui/residential-helper.sh blacklist-apply; systemctl show b-ui-relay -p ActiveEnterTimestamp --value; jq -r ".applied.at" /opt/b-ui/residential-blacklist.json'
```

Expected: stderr line `黑名单无变化，保持运行`; `ActiveEnterTimestamp == RELAY_T1` (unchanged); `applied.at` refreshed to now (the digest is the same, only the timestamp moves).

- [ ] **Step 8: Client-side proof from `baiyi` through the residential node**

```bash
ssh baiyi 'sudo -n bui-c list --json | jq -r ".[] | select(.server | test(\":40000$\")) | .name"'
```

Expected: one name (e.g. `HY2`). Save it as `RESI_NODE`, then:

```bash
ssh baiyi "sudo -n bui-c switch $RESI_NODE && sleep 3 && sudo -n bui-c status | head -5"
ssh baiyi 'P=--socks5-hostname; curl -s $P 127.0.0.1:1080 --max-time 20 -o /dev/null -w "google %{http_code}\n" https://www.google.com/;
           curl -s $P 127.0.0.1:1080 --max-time 20 -o /dev/null -w "ippure %{http_code}\n" https://ippure.com/;
           curl -s $P 127.0.0.1:1080 --max-time 20 https://ipinfo.io/ip; echo'
```

Expected: `google 200` (before 3.7.0 this was curl exit 35/56 or `403` — the residential vendor refused it); `ippure 200`; the `ipinfo.io/ip` line is the **residential** IP (Bright Data, `168.158.x.x`-style), not `bwg-tizi`. If `127.0.0.1:1080` is refused because the client is in TUN-only mode, drop the `$P 127.0.0.1:1080` part and rely on TUN routing — the expectations are identical.

Confirm on the server which outbound each connection used (this is the proof that google went out via the VPS IP while the rest stayed residential):

```bash
$SSH 'journalctl -u b-ui-relay --since "-3min" --no-pager -o cat | sed "s/\x1b\[[0-9;]*m//g" | grep -E "www\.google\.com:443|ippure\.com:443|ipinfo\.io:443" | sed -E "s/.*open connection to ([^ ]+) using outbound\/([^ ]+).*/\1 -> \2/" | sort | uniq -c'
```

Expected: `www.google.com:443 -> direct[direct]` and `ippure.com:443 -> http[resi-N]` (or `socks[resi-N]`), `ipinfo.io:443 -> http[resi-N]`.

- [ ] **Step 9: Pin test — force `www.google.com` back to residential, then clear**

```bash
$SSH '/opt/b-ui/resi-blacklist.sh pin www.google.com resi; echo "rc=$?";
      jq -c ".route.rules[] | select(.outbound==\"resi-pool\" and .domain_suffix) | .domain_suffix" /opt/b-ui/singbox-relay.json;
      jq -c "[.route.rules[] | select(.outbound==\"direct\" and .domain_suffix) | .domain_suffix[] | select(. == \"www.google.com\")] | length" /opt/b-ui/singbox-relay.json;
      jq -c ".pins" /opt/b-ui/residential-blacklist.json;
      systemctl show b-ui-relay -p ActiveEnterTimestamp --value'
ssh baiyi 'curl -s --socks5-hostname 127.0.0.1:1080 --max-time 20 -o /dev/null -w "google %{http_code}\n" https://www.google.com/; echo "curl rc=$?"'
```

Expected: `rc=0` with the helper line `黑名单有变化，b-ui-relay 已重启`; the resi-pool rule prints `["www.google.com"]` (it now sits *before* the direct rule); the direct-rule count for google is `0`; `{"www.google.com":"resi"}`; `ActiveEnterTimestamp` newer than `RELAY_T1`; from `baiyi` google is refused again (`google 000` with `curl rc=35` or `56`, or `google 403` — depends on how the vendor's CONNECT refusal surfaces through hysteria; anything but `200` is the expected "pinned to residential ⇒ vendor refuses" result).

Clear the pin:

```bash
$SSH '/opt/b-ui/resi-blacklist.sh pin www.google.com clear; jq -c ".pins" /opt/b-ui/residential-blacklist.json;
      jq -c "[.route.rules[] | select(.outbound==\"resi-pool\" and .domain_suffix)] | length" /opt/b-ui/singbox-relay.json;
      jq -c "[.route.rules[] | select(.outbound==\"direct\" and .domain_suffix) | .domain_suffix[] | select(. == \"www.google.com\")] | length" /opt/b-ui/singbox-relay.json'
ssh baiyi 'curl -s --socks5-hostname 127.0.0.1:1080 --max-time 20 -o /dev/null -w "google %{http_code}\n" https://www.google.com/'
```

Expected: `{}`, `0`, `1`, and `google 200` again.

- [ ] **Step 10: Panel smoke check**

Open `https://<bwg-tizi-domain>`, log in, open the residential dialog: the 「机房直出黑名单（自动检测）」 details block shows `N 条 · 最近检测 HH:MM` with N equal to the entry count from Step 5, the table lists `www.google.com` with reason `403 Forbidden serp domain`-style text, and the 「住宅 IP 健康」 card has a 「黑名单」 column with the same count. Click 「立即重新检测」: the badge switches to `检测中…`, and within ≤ 10 min returns to a count; `journalctl -u 'b-ui-resi-blacklist-probe-*' --since -15min` on the server shows a new transient unit. Or via curl with a token from the panel login (`TOKEN` = the JWT the browser stores; read it from DevTools → Application → localStorage):

```bash
curl -s -H "Authorization: Bearer $TOKEN" https://<bwg-tizi-domain>/api/residential/blacklist | jq '{selected, n: (.upstreams[.selected].entries|length), pins, applied}'
```

Expected: `selected` = `KEY` from Step 5, `n` ≥ 5, `pins: []`, `applied.upstream == selected`.

- [ ] **Step 11: Restore the client and record the final state**

```bash
ssh baiyi 'sudo -n bui-c switch hysteria2-1785892136 && sudo -n bui-c tun status | head -3'
$SSH 'jq -r .version /opt/b-ui/version.json; systemctl is-active hysteria-server hysteria-residential xray b-ui-admin caddy b-ui-relay b-ui-resi-blacklist.timer b-ui-resi-health.timer; rm -f /opt/b-ui/singbox-relay.json.bak.pre-3.7.0'
```

Expected: the client is back on `hysteria2-1785892136` with TUN running; `3.7.0`; eight `active` lines. (Delete the `.bak.pre-3.7.0` only after everything above passed; keep it if anything is still open.)

- [ ] **Step 12 (optional): Memory note**

Append to `/Users/woo/.claude/projects/-Users-woo-Desktop-b-ui/memory/b-ui-update-quirks.md`:

```markdown

**v3.7.0 起（2026-09-11）**：跨版本升级后要**连跑两次** `update.sh -y`——第一次只下载文件（内存里跑的是旧 update.sh），新迁移块（如 D10 住宅黑名单）在第二次「幂等自愈检查」路径才执行；第三次起零变更。验证黑名单：`jq '.upstreams|map_values(.entries|keys)' /opt/b-ui/residential-blacklist.json`，`TZ=Asia/Shanghai systemctl list-timers b-ui-resi-blacklist.timer`。
```

**Rollback (if any step above fails in a way that hurts users):**

1. Immediate, server-side only (keeps 3.7.0 files, removes the blacklist behaviour):

   ```bash
   $SSH 'systemctl disable --now b-ui-resi-blacklist.timer; systemctl stop "b-ui-resi-blacklist-probe-*";
         rm -f /etc/systemd/system/b-ui-resi-blacklist.timer /etc/systemd/system/b-ui-resi-blacklist.service; systemctl daemon-reload;
         mv /opt/b-ui/residential-blacklist.json /opt/b-ui/residential-blacklist.json.disabled;
         cp -a /opt/b-ui/singbox-relay.json.bak.pre-3.7.0 /opt/b-ui/singbox-relay.json && systemctl restart b-ui-relay;
         systemctl is-active b-ui-relay; md5sum /opt/b-ui/singbox-relay.json'
   ```

   Expected: `active` and the md5 from Step 2. (Alternative to the `cp` when the backup is gone: `/opt/b-ui/residential-helper.sh reapply` — with the state file moved away, `blacklist_rules_json` returns all-empty and the generated config is byte-identical to 3.6.x.) Note that `update.sh -y` would re-run D10 and re-seed the state; to keep the rollback in place until a fix ships, leave the timer removed and do not run `update.sh -y` again, or go to option 2.

2. Repo-side revert + re-release (needed for a fleet-wide rollback because `update.sh` never downgrades — remote 3.6.3 < local 3.7.0 is "already latest"):

   ```bash
   git revert --no-edit <sha of "bump: v3.7.0">^..HEAD    # revert the whole 3.7.0 series (Tasks 1-11)
   # then bump forward: version.json "3.7.1" with changelog "fix: 回滚 v3.7.0 住宅自动黑名单（…原因…）", commit "bump: v3.7.1 回滚 v3.7.0 住宅自动黑名单"
   git push origin Baiyi/bui-c-tun-mode-issue-df82e5:main
   $SSH 'nohup bash /opt/b-ui/update.sh -y > /var/log/b-ui-update-3.7.1.log 2>&1 &'
   ```

   Then repeat the server-side cleanup of option 1 (the reverted `update.sh` has no D10, so the timer/state must be removed by hand once) and re-verify services per Step 3.
