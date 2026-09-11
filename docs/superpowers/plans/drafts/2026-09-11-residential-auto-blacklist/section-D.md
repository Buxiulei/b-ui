## Section D — Web panel (Tasks 9–10)

**Scope:** `web/server.js` (API), then `web/index.html` + `web/app.js` + `web/style.css` (UI). Nothing here touches `singbox-relay.json` or the state file directly for writes: the panel *reads* `residential-blacklist.json` and delegates every write to `resi-blacklist.sh` (`pin`) or to an asynchronous `probe` process, exactly as spec §8.4/§8.5.

**Assumptions from earlier sections (do not redefine):**
- `tests/resi-blacklist/lib.sh` exists (created by the first bash-test task) and provides `assert_eq <expected> <actual> <msg>`, `assert_contains <haystack> <needle> <msg>`, `assert_file_exists <path> <msg>`, the `PASS_N`/`FAIL_N` counters and `finish` (prints `PASS n / FAIL m`, exits 1 on failures).
- The state-file JSON shape and the `resi-blacklist.sh` CLI (`probe [<host:port>|all]`, `pin <target> direct|resi|clear`) are exactly the CONTRACT ones; in the tests below both scripts are **stubs** that only record their argv.
- Upstream key = `"host:port"` with lowercase host; `checking` older than 10 min is stale.

All line numbers are from the current worktree at `/Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5` **before** Task 9 is applied; Task 10 line numbers are given relative to the same baseline for `index.html`/`app.js`/`style.css` (Task 9 does not touch those files, so they stay valid).

---

### Task 9: `web/server.js` — blacklist API (`/api/residential/blacklist*`, summary fields)

**Files:**
- Modify: `web/server.js:45-47` (CONFIG — add two paths)
- Modify: `web/server.js:632-647` (after `getRelaySelected()`, before the `// 生成 sing-box 融合配置` comment — add the blacklist helper functions)
- Modify: `web/server.js:2189-2193` (`GET /api/residential` — add `display.blacklist`)
- Modify: `web/server.js:2404-2409` (insert the four new routes between the `DELETE /api/residential/urls/…` block and the `// 住宅 IP 健康检查` comment)
- Modify: `web/server.js:2484-2500` (`GET /api/residential/health` — `blacklist_count` per member)
- Create: `tests/resi-blacklist/test_server_api.sh`
- Test: `node --check web/server.js && bash tests/resi-blacklist/test_server_api.sh`

**Interfaces:**
- Consumes: `BLACKLIST_STATE` JSON (`${BASE_DIR}/residential-blacklist.json`, CONTRACT shape); `resi-blacklist.sh pin <target> direct|resi|clear` (exit 0 ok) and `resi-blacklist.sh probe <host:port>|all`; existing `getRelaySelected()` (returns `"resi-N"` or `null`), `sendJSON`, `parseBody`, `helperErrText`, `log`, `spawnSync`/`spawn` (already imported), `RELAY_CLASH_API`.
- Produces (server.js internal, exact names): `CONFIG.resiBlacklistScript`, `CONFIG.resiBlacklistState`, `BL_CHECKING_STALE_MS`, `BL_DOMAIN_TARGET_RE`, `BL_PORT_TARGET_RE`, `blacklistEmptyState()`, `readBlacklistState()`, `blacklistUpstreamKey(u)`, `blacklistSummaryFor(key, state?)` → `{count, checkedAt, checking}`, `blacklistPinsList(state)` → `[{target, mode}]`, `selectedUpstreamKey()` (async → `"host:port"|null`), `isValidBlacklistTarget(t)`, `spawnBlacklistProbe(upstream)` → boolean, `runBlacklistPin(res, target, mode)`.
- Produces (HTTP, all after the auth line):
  - `GET  /api/residential/blacklist` → 200 `{selected, upstreams:{"host:port":{checkedAt, checking, entries:[{target,kind,source,reason,since}]}}, pins:[{target,mode}], learned:[{upstream,target,count,lastSeen}], applied}`
  - `POST /api/residential/blacklist/probe` `{upstream?}` → 202 `{started:true, upstream}` | 409 `{error:"检测进行中，请稍后"}` | 400 `{error:"住宅代理未启用或代理池为空"}` | 400 `{error:"未找到匹配上游"}`
  - `POST /api/residential/blacklist/pins` `{target, mode}` → 200 `{success:true, pins:[…]}` | 400 `{error:"目标格式无效"}` | 400 `{error:"mode 需要 direct 或 resi"}`
  - `DELETE /api/residential/blacklist/pins/<encodeURIComponent(target)>` → 200 `{success:true, pins:[…]}`
  - `GET /api/residential` gains `blacklist: {count, checkedAt, checking}`; `GET /api/residential/health` `members[]` gain `blacklist_count`.

- [ ] **Step 1: Write the failing API test**

Create `tests/resi-blacklist/test_server_api.sh` (the whole file). It starts `web/server.js` against a scratch `BASE_DIR` exactly as CLAUDE.md's "Run the panel locally" describes, with every fixture the server reads (`users.json`, `config.yaml` with a `listen:` line, `reality-keys.json`, `xray-config.json` with the `vless-direct` inbound, `certs/.domain`, `residential-proxy.json`, `singbox-relay.json`, a stub `residential-helper.sh` and a stub `resi-blacklist.sh` that record their argv). The server process (only the server, not the test) gets a stub `curl` first on `PATH` so the health endpoint's egress probes fail instantly instead of dialling the network.

```bash
#!/usr/bin/env bash
# 面板黑名单 API 冒烟测试：起真 server.js（scratch BASE_DIR）+ stub 脚本，逐个端点断言状态码与 JSON 字段
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

T="$(mktemp -d)"
PORT="${TEST_PORT:-18093}"
BASE="http://127.0.0.1:${PORT}"
SERVER_PID=""
cleanup() {
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null
    rm -rf "$T"
}
trap cleanup EXIT

# ---------- fixtures（CLAUDE.md「Run the panel locally」列出的每个文件） ----------
mkdir -p "$T/certs" "$T/stubs"
cat > "$T/users.json" <<'EOF'
[{"username":"alice","password":"pw","uuid":"11111111-2222-3333-4444-555555555555","sni":"www.bing.com","protocol":"fusion","residential":true,"createdAt":"2026-09-11T00:00:00Z","limits":{}}]
EOF
cat > "$T/config.yaml" <<'EOF'
listen: :10000,20000-30000
tls:
  cert: /x/fullchain.pem
  key: /x/privkey.pem
EOF
cat > "$T/reality-keys.json" <<'EOF'
{"privateKey":"priv","publicKey":"pubkeyXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX","shortId":"abcd1234"}
EOF
cat > "$T/xray-config.json" <<'EOF'
{"inbounds":[{"tag":"vless-direct","port":10001,"protocol":"vless","streamSettings":{"realitySettings":{"dest":"www.bing.com:443","serverNames":["www.bing.com"],"shortIds":["abcd1234"]}}}]}
EOF
echo "example.test" > "$T/certs/.domain"
cat > "$T/residential-proxy.json" <<'EOF'
{"enabled":true,"global":true,"domains":null,"urls":[{"host":"BRD.superproxy.io","port":44445,"username":"u","password":"p","type":"http","lastVerifiedIp":"1.2.3.4","name":"url-1"}]}
EOF
# 健康端点的成员真源是 relay 配置里的 socks/http 出站
cat > "$T/singbox-relay.json" <<'EOF'
{"outbounds":[{"type":"http","tag":"resi-1","server":"brd.superproxy.io","server_port":44445,"username":"u","password":"p"},{"type":"selector","tag":"resi-pool","outbounds":["resi-1"]},{"type":"direct","tag":"direct"}],"route":{"rules":[],"final":"resi-pool"}}
EOF

# stub helper：记录 argv；domains 子命令回一个 JSON 数组（GET /api/residential 会调它）
cat > "$T/residential-helper.sh" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "${BASE_DIR:-.}/helper-argv.log"
case "${1:-}" in
  domains) echo '["openai","anthropic"]' ;;
esac
exit 0
EOF
# stub resi-blacklist.sh：记录 argv；pin 直接改状态文件的 pins（模拟真脚本的效果，好断言端点会重读）
cat > "$T/resi-blacklist.sh" <<'EOF'
#!/usr/bin/env bash
BASE_DIR="${BASE_DIR:-/opt/b-ui}"
echo "$*" >> "${BASE_DIR}/bl-argv.log"
case "${1:-}" in
  pin)
    st="${BASE_DIR}/residential-blacklist.json"
    [[ -f "$st" ]] || echo '{"version":1,"upstreams":{},"learned":{},"pins":{},"checking":null,"applied":null}' > "$st"
    if [[ "${3:-}" == "clear" ]]; then
      jq --arg t "$2" 'del(.pins[$t])' "$st" > "$st.tmp" && mv "$st.tmp" "$st"
    else
      jq --arg t "$2" --arg m "$3" '.pins[$t]=$m' "$st" > "$st.tmp" && mv "$st.tmp" "$st"
    fi
    ;;
esac
exit 0
EOF
chmod +x "$T/residential-helper.sh" "$T/resi-blacklist.sh"
# 只给 server 进程用的 stub curl：健康端点的出口探测立刻失败（退出 7），测试自己仍用真 curl
printf '#!/usr/bin/env bash\nexit 7\n' > "$T/stubs/curl"
chmod +x "$T/stubs/curl"

# ---------- 起服务 ----------
( cd "$REPO/web" && PATH="$T/stubs:$PATH" BASE_DIR="$T" ADMIN_DIR="$REPO/web" ADMIN_PORT="$PORT" \
    ADMIN_PASSWORD=test123 SERVER_IP=203.0.113.10 node server.js > "$T/server.log" 2>&1 ) &
SERVER_PID=$!
for _ in $(seq 1 50); do
    curl -s -o /dev/null "$BASE/api/version" && break
    sleep 0.1
done
TOKEN="$(curl -s -X POST -H 'Content-Type: application/json' -d '{"password":"test123"}' "$BASE/api/login" | jq -r '.token')"
assert_contains "$TOKEN" "." "login returned a token"

# ---------- helpers ----------
HTTP_CODE=""; BODY=""
req() {   # req METHOD PATH [JSON_BODY]  → HTTP_CODE / BODY
    local m="$1" p="$2" d="${3:-}"
    if [[ -n "$d" ]]; then
        HTTP_CODE="$(curl -s -o "$T/body" -w '%{http_code}' -X "$m" -H "Authorization: Bearer $TOKEN" \
                     -H 'Content-Type: application/json' -d "$d" "$BASE/api$p")"
    else
        HTTP_CODE="$(curl -s -o "$T/body" -w '%{http_code}' -X "$m" -H "Authorization: Bearer $TOKEN" "$BASE/api$p")"
    fi
    BODY="$(cat "$T/body")"
}
jqv() { printf '%s' "$BODY" | jq -r "$1"; }
now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }
write_state() {   # write_state <checking-json>
    cat > "$T/residential-blacklist.json" <<EOF
{"version":1,
 "upstreams":{"brd.superproxy.io:44445":{"checkedAt":"2026-09-11T20:05:12Z","entries":{
    "www.google.com":{"kind":"domain","source":"builtin:search","reason":"403 Forbidden serp domain","fails":2,"okStreak":0,"since":"2026-09-11T20:05:12Z","lastCheck":"2026-09-11T20:05:12Z"},
    "port:5228":{"kind":"port","source":"learned","reason":"403 Forbidden","fails":2,"okStreak":0,"since":"2026-09-11T20:05:12Z","lastCheck":"2026-09-11T20:05:12Z"}}}},
 "learned":{"brd.superproxy.io:44445":{"gateway.icloud.com":{"count":68,"firstSeen":"2026-09-10T00:00:00Z","lastSeen":"2026-09-11T19:00:00Z"}}},
 "pins":{"example.com":"resi"},
 "checking":$1,
 "applied":{"upstream":"brd.superproxy.io:44445","digest":"abc","at":"2026-09-11T20:06:00Z"}}
EOF
}

# ---------- 1. 无状态文件 → 空骨架 ----------
rm -f "$T/residential-blacklist.json"
req GET /residential/blacklist
assert_eq 200 "$HTTP_CODE" "GET blacklist (no state) status"
assert_eq '{}' "$(jqv '.upstreams')" "no state → upstreams {}"
assert_eq '[]' "$(jqv '.pins')" "no state → pins []"
assert_eq 'null' "$(jqv '.applied')" "no state → applied null"
assert_eq 'brd.superproxy.io:44445' "$(jqv '.selected')" "selected falls back to first url, host lowercased"

# ---------- 2. 有状态文件（checking 过期）→ 明细 ----------
write_state '{"upstream":"all","startedAt":"2020-01-01T00:00:00Z"}'
req GET /residential/blacklist
assert_eq 200 "$HTTP_CODE" "GET blacklist status"
assert_eq 'false' "$(jqv '.upstreams["brd.superproxy.io:44445"].checking')" "stale checking → false"
assert_eq '2026-09-11T20:05:12Z' "$(jqv '.upstreams["brd.superproxy.io:44445"].checkedAt')" "checkedAt passthrough"
assert_eq '2' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries | length')" "two entries"
assert_eq 'port:5228' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries[0].target')" "entries sorted by target (port:5228 first)"
assert_eq 'www.google.com' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries[1].target')" "entry target"
assert_eq '403 Forbidden serp domain' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries[1].reason')" "entry reason"
assert_eq 'builtin:search' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries[1].source')" "entry source"
assert_eq 'domain' "$(jqv '.upstreams["brd.superproxy.io:44445"].entries[1].kind')" "entry kind"
assert_eq 'example.com' "$(jqv '.pins[0].target')" "pin target"
assert_eq 'resi' "$(jqv '.pins[0].mode')" "pin mode"
assert_eq 'gateway.icloud.com' "$(jqv '.learned[0].target')" "learned target"
assert_eq '68' "$(jqv '.learned[0].count')" "learned count"
assert_eq 'brd.superproxy.io:44445' "$(jqv '.learned[0].upstream')" "learned upstream"
assert_eq 'abc' "$(jqv '.applied.digest')" "applied passthrough"

# ---------- 3. GET /api/residential 带 blacklist 摘要 ----------
req GET /residential
assert_eq 200 "$HTTP_CODE" "GET residential status"
assert_eq '2' "$(jqv '.blacklist.count')" "residential.blacklist.count"
assert_eq 'false' "$(jqv '.blacklist.checking')" "residential.blacklist.checking"
assert_eq '2026-09-11T20:05:12Z' "$(jqv '.blacklist.checkedAt')" "residential.blacklist.checkedAt"

# ---------- 4. GET /api/residential/health 成员带 blacklist_count ----------
req GET /residential/health
assert_eq 200 "$HTTP_CODE" "GET health status"
assert_eq 'resi-1' "$(jqv '.members[0].tag')" "health member from relay config"
assert_eq '2' "$(jqv '.members[0].blacklist_count')" "health member blacklist_count"

# ---------- 5. probe：checking 新鲜 → 409；过期 → 202 并异步调脚本 ----------
write_state "{\"upstream\":\"all\",\"startedAt\":\"$(now_iso)\"}"
req POST /residential/blacklist/probe '{"upstream":"brd.superproxy.io:44445"}'
assert_eq 409 "$HTTP_CODE" "probe while checking fresh → 409"
assert_eq '检测进行中，请稍后' "$(jqv '.error')" "409 message"

write_state 'null'
rm -f "$T/bl-argv.log"
req POST /residential/blacklist/probe '{"upstream":"BRD.superproxy.io:44445"}'
assert_eq 202 "$HTTP_CODE" "probe one upstream → 202"
assert_eq 'true' "$(jqv '.started')" "probe started"
assert_eq 'brd.superproxy.io:44445' "$(jqv '.upstream')" "probe upstream echoed lowercased"
sleep 1
assert_file_exists "$T/bl-argv.log" "probe spawned resi-blacklist.sh"
assert_contains "$(cat "$T/bl-argv.log")" "probe brd.superproxy.io:44445" "stub recorded probe <host:port>"

req POST /residential/blacklist/probe
assert_eq 202 "$HTTP_CODE" "probe without body → 202"
assert_eq 'all' "$(jqv '.upstream')" "probe without body → all"
sleep 1
assert_contains "$(cat "$T/bl-argv.log")" "probe all" "stub recorded probe all"

req POST /residential/blacklist/probe '{"upstream":"nobody.example:1"}'
assert_eq 400 "$HTTP_CODE" "probe unknown upstream → 400"
assert_eq '未找到匹配上游' "$(jqv '.error')" "unknown upstream message"

# ---------- 6. pins：格式校验 / 成功 / 删除 ----------
req POST /residential/blacklist/pins '{"target":"bad target!","mode":"direct"}'
assert_eq 400 "$HTTP_CODE" "bad target → 400"
assert_eq '目标格式无效' "$(jqv '.error')" "bad target message"
req POST /residential/blacklist/pins '{"target":"www.google.com","mode":"maybe"}'
assert_eq 400 "$HTTP_CODE" "bad mode → 400"
req POST /residential/blacklist/pins '{"target":"port:99999x","mode":"direct"}'
assert_eq 400 "$HTTP_CODE" "bad port target → 400"

rm -f "$T/bl-argv.log"
req POST /residential/blacklist/pins '{"target":"WWW.Google.com","mode":"direct"}'
assert_eq 200 "$HTTP_CODE" "pin ok → 200"
assert_eq 'true' "$(jqv '.success')" "pin success"
assert_contains "$(cat "$T/bl-argv.log")" "pin www.google.com direct" "stub recorded pin www.google.com direct"
assert_eq 'direct' "$(jqv '.pins[] | select(.target=="www.google.com") | .mode')" "pins list reflects new pin"
assert_eq 'resi' "$(jqv '.pins[] | select(.target=="example.com") | .mode')" "existing pin still listed"

req POST /residential/blacklist/pins '{"target":"port:5228","mode":"direct"}'
assert_eq 200 "$HTTP_CODE" "pin port target → 200"

req DELETE "/residential/blacklist/pins/$(printf 'www.google.com' | jq -sRr @uri)"
assert_eq 200 "$HTTP_CODE" "unpin → 200"
assert_contains "$(cat "$T/bl-argv.log")" "pin www.google.com clear" "stub recorded pin … clear"
assert_eq '' "$(jqv '.pins[] | select(.target=="www.google.com") | .mode')" "pin removed from list"

# ---------- 7. 住宅未启用 → probe 400 ----------
jq '.enabled=false' "$T/residential-proxy.json" > "$T/rp.tmp" && mv "$T/rp.tmp" "$T/residential-proxy.json"
req POST /residential/blacklist/probe
assert_eq 400 "$HTTP_CODE" "probe while disabled → 400"
assert_eq '住宅代理未启用或代理池为空' "$(jqv '.error')" "disabled message"

# ---------- 8. 未鉴权 → 401 ----------
code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/residential/blacklist")"
assert_eq 401 "$code" "blacklist endpoint requires auth"

finish
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && bash tests/resi-blacklist/test_server_api.sh`
Expected: the login assertion passes, then a FAIL line for `GET blacklist (no state) status` (expected 200, got 404 — the route does not exist yet; unknown `/api/*` paths fall through to `{"error":"Not found"}`), a FAIL for `residential.blacklist.count` (expected 2, got null), a FAIL for `health member blacklist_count` (expected 2, got null), every probe/pin assertion failing with 404 instead of 202/409/400/200, and `finish` printing `PASS n / FAIL m` with `m > 0` and exit 1.

- [ ] **Step 3: Add the two CONFIG paths**

`web/server.js:45-47` currently reads:

```js
    residentialConfig: process.env.RESIDENTIAL_CONFIG || `${BASE_DIR}/residential-proxy.json`,
    residentialHelper: `${BASE_DIR}/residential-helper.sh`,
    usersFile: process.env.USERS_FILE || `${BASE_DIR}/users.json`,
```

Replace with:

```js
    residentialConfig: process.env.RESIDENTIAL_CONFIG || `${BASE_DIR}/residential-proxy.json`,
    residentialHelper: `${BASE_DIR}/residential-helper.sh`,
    // v3.7.0 R13: 机房直出黑名单 —— 脚本负责探测/学习/钉住，状态文件由脚本维护，面板只读
    resiBlacklistScript: `${BASE_DIR}/resi-blacklist.sh`,
    resiBlacklistState: `${BASE_DIR}/residential-blacklist.json`,
    usersFile: process.env.USERS_FILE || `${BASE_DIR}/users.json`,
```

- [ ] **Step 4: Add the blacklist helper functions after `getRelaySelected()`**

`web/server.js:645-647` currently reads (end of `getRelaySelected`, blank line, next comment):

```js
    });
}

// 生成 sing-box 融合配置 — v3.6.0: sing-box 1.12-1.14 语法 + IPv6 接管(ipv4_only + v6 reject)
```

Insert the following block between the closing `}` of `getRelaySelected()` and the `// 生成 sing-box 融合配置` comment (keep one blank line on each side):

```js
// ─── v3.7.0 R13: 机房直出黑名单（面板侧只读状态文件，写操作一律经 resi-blacklist.sh）────────────
// 状态文件由 resi-blacklist.sh 维护（chmod 600、原子写）；这里不碰 singbox-relay.json、不拿 .blacklist.lock
const BL_CHECKING_STALE_MS = 10 * 60 * 1000;
// target 校验：小写主机名（至少一个点）或 port:N；与 resi-blacklist.sh 的 target 规则同口径
const BL_DOMAIN_TARGET_RE = /^([a-z0-9-]+\.)+[a-z0-9-]+$/;
const BL_PORT_TARGET_RE = /^port:[0-9]{1,5}$/;

function blacklistEmptyState() {
    return { version: 1, upstreams: {}, learned: {}, pins: {}, checking: null, applied: null };
}

// 读状态文件 → 规范化（缺字段补空、非对象归空）；文件不存在/坏 JSON → 空骨架。
// checking 超过 10 分钟视为过期（探测进程被杀/机器重启时脚本来不及清），归 null，面板不再显示"检测中"
function readBlacklistState() {
    const empty = blacklistEmptyState();
    let s;
    try {
        if (!fs.existsSync(CONFIG.resiBlacklistState)) return empty;
        s = JSON.parse(fs.readFileSync(CONFIG.resiBlacklistState, "utf8"));
    } catch { return empty; }
    if (!s || typeof s !== "object" || Array.isArray(s)) return empty;
    const obj = (v) => (v && typeof v === "object" && !Array.isArray(v)) ? v : {};
    const st = {
        version: 1,
        upstreams: obj(s.upstreams),
        learned: obj(s.learned),
        pins: obj(s.pins),
        checking: null,
        applied: (s.applied && typeof s.applied === "object" && !Array.isArray(s.applied)) ? s.applied : null,
    };
    if (s.checking && typeof s.checking === "object" && typeof s.checking.startedAt === "string") {
        const t = Date.parse(s.checking.startedAt);
        if (!Number.isNaN(t) && Date.now() - t < BL_CHECKING_STALE_MS) st.checking = s.checking;
    }
    return st;
}

// 上游键 host:port（host 小写），与 resi-blacklist.sh / residential-helper.sh 的 upstream key 同规则
function blacklistUpstreamKey(u) {
    return `${String(u.host || "").toLowerCase()}:${u.port}`;
}

// 某上游的摘要：条数 / 最近检测 / 是否检测中（checking 为 all 或正好是这个上游）；state 可传入避免重复读文件
function blacklistSummaryFor(key, state) {
    const st = state || readBlacklistState();
    const up = (key && st.upstreams[key] && typeof st.upstreams[key] === "object") ? st.upstreams[key] : null;
    const entries = (up && up.entries && typeof up.entries === "object" && !Array.isArray(up.entries)) ? up.entries : {};
    const checking = !!st.checking && (st.checking.upstream === "all" || st.checking.upstream === key);
    return { count: Object.keys(entries).length, checkedAt: (up && up.checkedAt) || null, checking };
}

function blacklistPinsList(state) {
    return Object.entries(state.pins)
        .filter(([, mode]) => mode === "direct" || mode === "resi")
        .map(([target, mode]) => ({ target, mode }))
        .sort((a, b) => a.target.localeCompare(b.target));
}

// selector 当前选中的成员映射成 host:port（同 residential-helper.sh selected_upstream_key）：
// Clash API 的 .now 是 resi-N → residential-proxy.json .urls 第 N 条（1 起）；API 不通/越界 → 池首；池空 → null
async function selectedUpstreamKey() {
    let urls = [];
    try {
        if (fs.existsSync(CONFIG.residentialConfig)) {
            const r = JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"));
            if (Array.isArray(r.urls)) urls = r.urls.filter(u => u && u.host && u.port);
        }
    } catch { }
    if (!urls.length) return null;
    const now = await getRelaySelected();
    const m = typeof now === "string" ? now.match(/^resi-(\d+)$/) : null;
    const idx = m ? parseInt(m[1], 10) - 1 : 0;
    const pick = (idx >= 0 && idx < urls.length) ? urls[idx] : urls[0];
    return blacklistUpstreamKey(pick);
}

function isValidBlacklistTarget(t) {
    return typeof t === "string" && t.length <= 253 && (BL_DOMAIN_TARGET_RE.test(t) || BL_PORT_TARGET_RE.test(t));
}

// 异步起一次探测：优先 systemd-run（独立 transient unit，不随 b-ui-admin 重启被杀；unit 名只含时间戳），
// 没有 systemd-run（本机开发/测试）就 detached spawn。返回是否成功启动；永不抛
function spawnBlacklistProbe(upstream) {
    const unit = `b-ui-resi-blacklist-probe-${Date.now()}`;
    try {
        const r = spawnSync("systemd-run",
            ["--unit", unit, "--quiet", CONFIG.resiBlacklistScript, "probe", upstream],
            { env: { ...process.env, BASE_DIR }, encoding: "utf8", timeout: 10000 });
        if (!r.error && r.status === 0) return true;
        log("WARN", `[黑名单] systemd-run 不可用（${r.error ? r.error.code : "exit " + r.status}），改用 detached spawn`);
    } catch (e) { log("WARN", `[黑名单] systemd-run: ${e.message}`); }
    try {
        const child = spawn(CONFIG.resiBlacklistScript, ["probe", upstream],
            { env: { ...process.env, BASE_DIR }, detached: true, stdio: "ignore" });
        child.on("error", e => log("ERROR", `[黑名单] probe ${upstream}: ${e.message}`));
        child.unref();
        return true;
    } catch (e) {
        log("ERROR", `[黑名单] probe ${upstream}: ${e.message}`);
        return false;
    }
}

// 钉住 / 取消钉住都走脚本（脚本写 pins 后自己调 helper blacklist-apply，有变化立即重启 relay）
function runBlacklistPin(res, target, mode) {
    try {
        if (!fs.existsSync(CONFIG.resiBlacklistScript)) {
            return sendJSON(res, { error: "resi-blacklist.sh 不存在，请先升级到 v3.7.0" }, 500);
        }
        const result = spawnSync(CONFIG.resiBlacklistScript, ["pin", target, mode], {
            env: { ...process.env, BASE_DIR },
            encoding: "utf8",
            timeout: 60000,
        });
        if (result.error && result.error.code === "ETIMEDOUT") {
            return sendJSON(res, { error: "钉住操作超时（60 秒）" }, 500);
        }
        if (result.status !== 0) {
            return sendJSON(res, { error: helperErrText(result, "钉住操作失败") }, 500);
        }
        return sendJSON(res, { success: true, pins: blacklistPinsList(readBlacklistState()) });
    } catch (e) {
        return sendJSON(res, { error: e.message }, 500);
    }
}
```

- [ ] **Step 5: Add the four routes after the `DELETE /api/residential/urls/…` block**

`web/server.js:2404-2409` currently reads (tail of the DELETE-urls handler, blank line, the health-route comment):

```js
                } catch (e) {
                    return sendJSON(res, { error: e.message }, 500);
                }
            }

            // 住宅 IP 健康检查 — v3.6.0 R7: 成员真源是中继的 socks 出站（与 resi-health.sh 同源，
```

Insert this block between the `}` that closes the DELETE-urls `if` and the `// 住宅 IP 健康检查` comment (so it lives after the auth line, next to the other `/api/residential` routes):

```js
            // v3.7.0 R13: GET /api/residential/blacklist — 面板直接读状态文件，不调脚本
            if (r === "residential/blacklist" && req.method === "GET") {
                try {
                    const st = readBlacklistState();
                    const upstreams = {};
                    for (const [key, up] of Object.entries(st.upstreams)) {
                        const entries = (up && up.entries && typeof up.entries === "object" && !Array.isArray(up.entries)) ? up.entries : {};
                        upstreams[key] = {
                            checkedAt: (up && up.checkedAt) || null,
                            checking: blacklistSummaryFor(key, st).checking,
                            entries: Object.entries(entries).map(([target, e]) => ({
                                target,
                                kind: (e && e.kind === "port") ? "port" : "domain",
                                source: (e && e.source) || "",
                                reason: (e && e.reason) || "",
                                since: (e && e.since) || null,
                            })).sort((a, b) => a.target.localeCompare(b.target)),
                        };
                    }
                    const learned = [];
                    for (const [upstream, targets] of Object.entries(st.learned)) {
                        if (!targets || typeof targets !== "object") continue;
                        for (const [target, l] of Object.entries(targets)) {
                            learned.push({
                                upstream,
                                target,
                                count: (l && typeof l.count === "number") ? l.count : 0,
                                lastSeen: (l && l.lastSeen) || null,
                            });
                        }
                    }
                    learned.sort((a, b) => b.count - a.count || a.target.localeCompare(b.target));
                    return sendJSON(res, {
                        selected: await selectedUpstreamKey(),
                        upstreams,
                        pins: blacklistPinsList(st),
                        learned,
                        applied: st.applied,
                    });
                } catch (e) {
                    return sendJSON(res, { error: e.message }, 500);
                }
            }

            // v3.7.0 R13: POST /api/residential/blacklist/probe {upstream?} — 异步探测，立即 202；
            // checking 新鲜时 409（面板轮询 GET 直到 checking 消失）
            if (r === "residential/blacklist/probe" && req.method === "POST") {
                const b = await parseBody(req);
                let rConfig = { enabled: false, urls: [] };
                try {
                    if (fs.existsSync(CONFIG.residentialConfig)) {
                        rConfig = JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"));
                    }
                } catch { }
                const urls = Array.isArray(rConfig.urls) ? rConfig.urls.filter(u => u && u.host && u.port) : [];
                if (rConfig.enabled !== true || !urls.length) {
                    return sendJSON(res, { error: "住宅代理未启用或代理池为空" }, 400);
                }
                let upstream = "all";
                if (b && typeof b.upstream === "string" && b.upstream.trim()) {
                    upstream = b.upstream.trim().toLowerCase();
                    if (!urls.some(u => blacklistUpstreamKey(u) === upstream)) {
                        return sendJSON(res, { error: "未找到匹配上游" }, 400);
                    }
                }
                if (readBlacklistState().checking) {
                    return sendJSON(res, { error: "检测进行中，请稍后" }, 409);
                }
                if (!spawnBlacklistProbe(upstream)) {
                    return sendJSON(res, { error: "无法启动检测进程" }, 500);
                }
                log("INFO", `[黑名单] 面板触发检测: ${upstream}`);
                return sendJSON(res, { started: true, upstream }, 202);
            }

            // v3.7.0 R13: POST /api/residential/blacklist/pins {target, mode} — 钉住（强制机房/强制住宅）
            if (r === "residential/blacklist/pins" && req.method === "POST") {
                const b = await parseBody(req);
                const target = (b && typeof b.target === "string") ? b.target.trim().toLowerCase() : "";
                const mode = (b && typeof b.mode === "string") ? b.mode.trim() : "";
                if (!isValidBlacklistTarget(target)) return sendJSON(res, { error: "目标格式无效" }, 400);
                if (mode !== "direct" && mode !== "resi") return sendJSON(res, { error: "mode 需要 direct 或 resi" }, 400);
                return runBlacklistPin(res, target, mode);
            }

            // v3.7.0 R13: DELETE /api/residential/blacklist/pins/<target> — 取消钉住
            if (r.startsWith("residential/blacklist/pins/") && req.method === "DELETE") {
                const target = decodeURIComponent(r.slice("residential/blacklist/pins/".length)).trim().toLowerCase();
                if (!isValidBlacklistTarget(target)) return sendJSON(res, { error: "目标格式无效" }, 400);
                return runBlacklistPin(res, target, "clear");
            }

```

- [ ] **Step 6: Add `blacklist` to `GET /api/residential` and `blacklist_count` to the health members**

(a) `web/server.js:2189-2193` currently reads:

```js
                        if (display.password) display.password = display.password.slice(0, 2) + "***";
                        if (display.username && display.host) {
                            display.displayUrl = resiRow(display).displayUrl;
                        }
                        return sendJSON(res, display);
```

Replace with:

```js
                        if (display.password) display.password = display.password.slice(0, 2) + "***";
                        if (display.username && display.host) {
                            display.displayUrl = resiRow(display).displayUrl;
                        }
                        // v3.7.0 R13: 当前选中上游的黑名单摘要（条数 / 最近检测 / 是否检测中）
                        display.blacklist = blacklistSummaryFor(await selectedUpstreamKey());
                        return sendJSON(res, display);
```

(b) `web/server.js:2484-2500` (inside the health route's async IIFE) currently reads:

```js
                    const [selected, egressList] = await Promise.all([
                        getRelaySelected(),
                        Promise.all(members.map(m => probeEgress(m))),
                    ]);
                    const rows = members.map((m, i) => {
                        const st = healthState[m.tag] || {};
                        return {
                            tag: m.tag,
                            type: m.type,
                            host: m.host,
                            port: m.port,
                            active: st.active !== false,
                            failstreak: st.failstreak || 0,
                            okstreak: st.okstreak || 0,
                            egress: egressList[i],
                        };
                    });
```

Replace with:

```js
                    const [selected, egressList] = await Promise.all([
                        getRelaySelected(),
                        Promise.all(members.map(m => probeEgress(m))),
                    ]);
                    // v3.7.0 R13: 每条线路的黑名单条数（状态文件读一次，按 host:port 取）
                    const blState = readBlacklistState();
                    const rows = members.map((m, i) => {
                        const st = healthState[m.tag] || {};
                        return {
                            tag: m.tag,
                            type: m.type,
                            host: m.host,
                            port: m.port,
                            active: st.active !== false,
                            failstreak: st.failstreak || 0,
                            okstreak: st.okstreak || 0,
                            egress: egressList[i],
                            blacklist_count: blacklistSummaryFor(blacklistUpstreamKey(m), blState).count,
                        };
                    });
```

- [ ] **Step 7: Syntax check and run the API test**

Run: `cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && node --check web/server.js && bash tests/resi-blacklist/test_server_api.sh`
Expected: `node --check` prints nothing; the test prints one `PASS …` line per assertion (no `FAIL` lines) and ends with `PASS 55 / FAIL 0`, exit 0. If `probe spawned resi-blacklist.sh` fails, check `$T/server.log` for the `[黑名单] systemd-run 不可用` WARN line — on macOS it must be followed by the detached spawn writing `bl-argv.log`.

- [ ] **Step 8: Commit**

```bash
cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && git add web/server.js tests/resi-blacklist/test_server_api.sh && git commit -m "feat(web): 黑名单 API —— 读状态文件/异步探测/钉住 + 住宅摘要与健康表条数

- GET /api/residential/blacklist 直接读 residential-blacklist.json（checking 超 10 分钟视为过期）
- POST /api/residential/blacklist/probe 经 systemd-run 异步起 resi-blacklist.sh probe，检测中 409
- POST/DELETE /api/residential/blacklist/pins 调脚本 pin <target> direct|resi|clear
- GET /api/residential 增加 blacklist 摘要，/api/residential/health 成员增加 blacklist_count
- tests/resi-blacklist/test_server_api.sh：起真 server.js + stub 脚本逐端点断言

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: `web/index.html` + `web/app.js` + `web/style.css` — blacklist section in the residential modal, health-card column

**Files:**
- Modify: `web/index.html:319-323` (insert `<details id="resi-blacklist-details">` between the end of `.resi-urls-section` and `#resi-error`)
- Modify: `web/app.js:868-873` (`openResi()` — reset the new details block and stop polling)
- Modify: `web/app.js:930-933` (`openResi()` — load the blacklist after the domains render)
- Modify: `web/app.js:990-998` → append the blacklist functions right after `disableResi()` (before the `// ─── 系统状态卡片` comment at line 1000)
- Modify: `web/app.js:1129` (health table header — add 「黑名单」)
- Modify: `web/app.js:1154-1157` (health table row — add the count cell)
- Modify: `web/style.css:1162-1170` (`.resi-member` grid — add a 6th column)
- Modify: `web/style.css` — append the `.resi-bl-*` / `.resi-pin-*` rules at the end of the file (after line 1815)
- Create: `tests/resi-blacklist/test_ui_render.sh`
- Test: `node --check web/app.js && bash tests/resi-blacklist/test_ui_render.sh`

**Interfaces:**
- Consumes: Task 9's `GET /api/residential/blacklist`, `POST /api/residential/blacklist/probe`, `POST /api/residential/blacklist/pins`, `DELETE /api/residential/blacklist/pins/<target>`, `GET /api/residential/health` (`members[].blacklist_count`); existing app.js helpers `api()`, `$`, `esc`, `toast`, `_resiErr`, `_resiClearErr`, `_sysTag`.
- Produces (app.js, exact names): pure region between `/* BL-RENDER-START */` and `/* BL-RENDER-END */` containing `_blFmtClock(iso)`, `_blFmtTime(iso)`, `_blSourceLabel(src)`, `_blIsChecking(data)`, `renderResidentialBlacklist(data)` → `{badge, body, pins}` (strings; `body`/`pins` are HTML, `badge` is text) and `applyResidentialBlacklist(data)` (writes those three into `#resi-blacklist-badge`, `#resi-blacklist-body`, `#resi-pins-list`); outside the region `loadResidentialBlacklist()`, `_blStartPoll()`, `_blStopPoll()`, `probeResidentialBlacklist(upstream)`, `addResidentialPin()`, `removeResidentialPin(target)`.
- Produces (DOM ids): `#resi-blacklist-details`, `#resi-blacklist-badge`, `#resi-blacklist-body`, `#resi-blacklist-probe-btn`, `#resi-pin-target`, `#resi-pin-mode`, `#resi-pin-add`, `#resi-pins-list`.

- [ ] **Step 1: Write the failing render smoke test**

Create `tests/resi-blacklist/test_ui_render.sh`:

```bash
#!/usr/bin/env bash
# 无 jsdom 的渲染冒烟：用 sed 切出 app.js 的 BL-RENDER 区（纯函数）+ esc 定义，在一个假 document 上跑
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

{
    sed -n '/^const esc = /p' "$REPO/web/app.js"
    sed -n '/^\/\* BL-RENDER-START \*\/$/,/^\/\* BL-RENDER-END \*\/$/p' "$REPO/web/app.js"
} > "$T/render.js"
assert_contains "$(cat "$T/render.js")" "function renderResidentialBlacklist" "BL-RENDER region extracted from app.js"
assert_contains "$(cat "$T/render.js")" "function applyResidentialBlacklist" "apply function inside region"

cat > "$T/sample.json" <<'EOF'
{"selected":"brd.superproxy.io:44445",
 "upstreams":{
   "brd.superproxy.io:44445":{"checkedAt":"2026-09-11T20:05:12Z","checking":false,"entries":[
      {"target":"port:5228","kind":"port","source":"learned","reason":"403 Forbidden","since":"2026-09-11T20:05:12Z"},
      {"target":"www.google.com","kind":"domain","source":"builtin:search","reason":"403 Forbidden serp domain <b>x</b>","since":"2026-09-11T20:05:12Z"}]},
   "other.proxy.net:1080":{"checkedAt":null,"checking":false,"entries":[]}},
 "pins":[{"target":"example.com","mode":"resi"},{"target":"port:5223","mode":"direct"}],
 "learned":[{"upstream":"brd.superproxy.io:44445","target":"gateway.icloud.com","count":68,"lastSeen":"2026-09-11T19:00:00Z"}],
 "applied":{"upstream":"brd.superproxy.io:44445","digest":"abc","at":"2026-09-11T20:06:00Z"}}
EOF

cat > "$T/run.js" <<'EOF'
const fs = require("fs");
// 最小假 DOM：getElementById 返回可写 innerHTML/textContent/hidden 的对象
const els = {};
const mk = (id) => ({ id, innerHTML: "", textContent: "", hidden: false, disabled: false, value: "",
    classList: { contains: () => true }, addEventListener() { } });
global.document = { getElementById: (id) => (els[id] ||= mk(id)) };
global.$ = (s) => document.getElementById(String(s).replace(/^#/, ""));
eval(fs.readFileSync(process.argv[2], "utf8"));
const sample = JSON.parse(fs.readFileSync(process.argv[3], "utf8"));
const pure = renderResidentialBlacklist(sample);
applyResidentialBlacklist(sample);
const checkingSample = { ...sample, upstreams: { ...sample.upstreams,
    "brd.superproxy.io:44445": { ...sample.upstreams["brd.superproxy.io:44445"], checking: true } } };
const checkingBadge = renderResidentialBlacklist(checkingSample).badge;
console.log(JSON.stringify({
    pure,
    badge: els["resi-blacklist-badge"].textContent,
    body: els["resi-blacklist-body"].innerHTML,
    pins: els["resi-pins-list"].innerHTML,
    checkingBadge,
    emptyBody: renderResidentialBlacklist({ selected: null, upstreams: {}, pins: [], learned: [], applied: null }).body,
}));
EOF

OUT="$(node "$T/run.js" "$T/render.js" "$T/sample.json" 2>"$T/err")" || { echo "node failed:"; cat "$T/err"; }
assert_contains "$OUT" '"badge"' "render script produced JSON"
j() { printf '%s' "$OUT" | jq -r "$1"; }

BODY="$(j '.body')"; PINS="$(j '.pins')"; BADGE="$(j '.badge')"
assert_eq "$(j '.pure.body')" "$BODY" "applyResidentialBlacklist writes exactly renderResidentialBlacklist().body"
assert_contains "$BODY" "brd.superproxy.io:44445" "body has upstream key"
assert_contains "$BODY" "当前选中" "selected upstream is flagged"
assert_contains "$BODY" "www.google.com" "body has entry target"
assert_contains "$BODY" "403 Forbidden serp domain" "body has reason"
assert_contains "$BODY" "&lt;b&gt;x&lt;/b&gt;" "reason is HTML-escaped"
assert_contains "$BODY" "port:5228" "body has port entry"
assert_contains "$BODY" "内置·搜索" "builtin source label"
assert_contains "$BODY" "日志学习" "learned source label"
assert_contains "$BODY" "other.proxy.net:1080" "second upstream rendered"
assert_contains "$BODY" "该上游没有需要直出的目标" "empty upstream copy"
assert_contains "$BODY" "gateway.icloud.com" "learned candidates table"
assert_contains "$BODY" "68" "learned count"
assert_contains "$PINS" "example.com" "pin row target"
assert_contains "$PINS" "强制住宅" "pin row mode label (resi)"
assert_contains "$PINS" "强制机房" "pin row mode label (direct)"
assert_contains "$PINS" "removeResidentialPin('example.com')" "pin row has remove handler"
assert_contains "$BADGE" "2 条" "badge shows selected upstream count"
assert_contains "$BADGE" "最近检测" "badge shows last check"
assert_contains "$(j '.checkingBadge')" "检测中" "badge while checking"
assert_contains "$(j '.emptyBody')" "尚未检测" "empty state copy"

finish
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && bash tests/resi-blacklist/test_ui_render.sh`
Expected: `FAIL BL-RENDER region extracted from app.js` (the sed range prints nothing because the markers do not exist), `node failed:` followed by `ReferenceError: renderResidentialBlacklist is not defined`, then every content assertion failing, and `PASS 0 / FAIL 24` with exit 1.

- [ ] **Step 3: Add the `<details>` block to `web/index.html`**

`web/index.html:319-323` currently reads:

```html
                    <!-- v3.6.0 R8: 供应商默认"出口不可用就静默换 IP"，不加粘性/失效参数健康探测语义就失真 -->
                    <div class="resi-hint resi-global-hint">建议在用户名里加供应商的粘性/失效参数（如 Bright Data <code>-session-xxx-const</code>），否则出口 IP 会静默轮换；见 <a href="https://github.com/Buxiulei/b-ui/blob/main/docs/residential-proxy-guide.md" target="_blank" rel="noopener">住宅代理指南</a></div>
                </div>
            </div>
            <div id="resi-error" class="resi-error-box" style="display:none">
```

Insert the following between the `</div>` that closes `.resi-urls-section` (line 322) and `<div id="resi-error"` (line 323):

```html
            <!-- v3.7.0 R13: 机房直出黑名单 —— 按上游列出"住宅代理不了"的目标；探测/学习在服务端 resi-blacklist.sh -->
            <details id="resi-blacklist-details" class="resi-domains-details resi-blacklist-details">
                <summary class="resi-domains-summary">
                    <svg class="resi-chevron" xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>
                    <span>机房直出黑名单（自动检测）</span>
                    <span id="resi-blacklist-badge" class="resi-count-badge"></span>
                </summary>
                <div class="resi-domains-body">
                    <div class="resi-domains-hint">默认全部走住宅；下面列出的目标是「当前上游代理不了」的（两次探测都被拒、且直连正常），b-ui-relay 自动把它们送去机房直出。每天北京时间 04:00 后台复测一次，连续两次能通才移除。</div>
                    <div id="resi-blacklist-body" class="resi-blacklist-body"></div>
                    <div class="resi-domains-actions">
                        <button id="resi-blacklist-probe-btn" class="btn btn-secondary resi-update-btn" type="button" onclick="probeResidentialBlacklist()">立即重新检测</button>
                    </div>
                    <div class="resi-domains-hint">检测在后台进行（内置候选表 + 日志学到的目标，约 1-3 分钟），黑名单有变化才重启 b-ui-relay</div>
                    <div class="resi-pin-row">
                        <input type="text" id="resi-pin-target" class="resi-pin-input" placeholder="域名（如 www.google.com）或 port:5228" autocomplete="off">
                        <select id="resi-pin-mode" class="resi-pin-select">
                            <option value="direct">强制机房</option>
                            <option value="resi">强制住宅</option>
                        </select>
                        <button id="resi-pin-add" class="btn btn-secondary resi-update-btn" type="button" onclick="addResidentialPin()">钉住</button>
                    </div>
                    <div id="resi-pins-list" class="resi-pins-list"></div>
                    <div class="resi-domains-hint">钉住优先于自动判定：「强制机房」不探测直接直出；「强制住宅」即使被拒也不进直出规则。域名只匹配自身与子域（www.google.com 不影响 gemini.google.com）。</div>
                </div>
            </details>
```

- [ ] **Step 4: Add the render region and the action functions to `web/app.js`**

`web/app.js:990-1000` currently reads:

```js
function disableResi() {
    _resiClearErr();
    api("/residential", { method: "DELETE" }).then(r => {
        if (r.success) { closeM(); toast("住宅 IP 已禁用"); }
        else _resiErr(r.error || "禁用失败");
    }).catch(e => {
        _resiErr(e.message || "请求失败");
    });
}

// ─── 系统状态卡片：住宅 IP 健康 + hy2 watchdog ───────────────────────────────
```

Insert the following block between the closing `}` of `disableResi()` and the `// ─── 系统状态卡片` comment (one blank line each side). The part between the markers is pure (data → strings) and is what `test_ui_render.sh` slices out; it must not reference anything except `esc`, `$` and `document.getElementById`.

```js
// ─── v3.7.0 R13: 机房直出黑名单（住宅弹窗内） ─────────────────────────────────
/* BL-RENDER-START */
// 纯渲染区：只依赖 esc / $，数据 → HTML 字符串。tests/resi-blacklist/test_ui_render.sh 用 sed 切出来在假 DOM 上跑
function _blFmtClock(iso) {
    if (!iso) return "—";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return String(iso);
    const p = n => String(n).padStart(2, "0");
    return p(d.getHours()) + ":" + p(d.getMinutes());
}
function _blFmtTime(iso) {
    if (!iso) return "—";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return String(iso);
    const p = n => String(n).padStart(2, "0");
    return p(d.getMonth() + 1) + "-" + p(d.getDate()) + " " + p(d.getHours()) + ":" + p(d.getMinutes());
}
// 来源文案：builtin:<group> → 内置·分组名；learned → 日志学习
function _blSourceLabel(src) {
    const s = String(src || "");
    if (s === "learned") return "日志学习";
    if (s.startsWith("builtin:")) {
        const g = s.slice("builtin:".length);
        const names = { search: "搜索", social: "社交", payment: "支付", apple: "Apple", push: "推送" };
        return "内置·" + (names[g] || g);
    }
    return s || "—";
}
function _blIsChecking(data) {
    const ups = (data && data.upstreams && typeof data.upstreams === "object") ? data.upstreams : {};
    return Object.keys(ups).some(k => ups[k] && ups[k].checking === true);
}
// data 是 GET /api/residential/blacklist 的响应；返回 {badge, body, pins}
function renderResidentialBlacklist(data) {
    const d = data || {};
    const ups = (d.upstreams && typeof d.upstreams === "object") ? d.upstreams : {};
    const keys = Object.keys(ups).sort((a, b) => (a === d.selected ? -1 : b === d.selected ? 1 : a.localeCompare(b)));
    const pins = Array.isArray(d.pins) ? d.pins : [];
    const learned = Array.isArray(d.learned) ? d.learned : [];
    const checking = _blIsChecking(d);

    // summary 徽标：检测中… / N 条 · 最近检测 HH:MM（当前选中上游，没有选中就取全部之和）
    let badge = "";
    if (checking) {
        badge = "检测中…";
    } else if (keys.length) {
        const sel = d.selected && ups[d.selected] ? ups[d.selected] : null;
        const count = sel ? (sel.entries || []).length
            : keys.reduce((n, k) => n + ((ups[k].entries || []).length), 0);
        const at = sel ? sel.checkedAt : keys.map(k => ups[k].checkedAt).filter(Boolean).sort().pop();
        badge = count + " 条" + (at ? " · 最近检测 " + _blFmtClock(at) : "");
    }

    let body = "";
    if (!keys.length) {
        body += '<div class="resi-urls-empty">尚未检测。添加上游后会自动探测一次；也可以点「立即重新检测」</div>';
    }
    // 上次应用失败提示：applied.at 早于任一 checkedAt（spec §10）
    const latestCheck = keys.map(k => ups[k].checkedAt).filter(Boolean).sort().pop() || null;
    if (d.applied && d.applied.at && latestCheck && String(d.applied.at) < String(latestCheck)) {
        body += '<div class="resi-bl-warn">上次应用失败：最近一次检测结果还没写进 b-ui-relay（sing-box check 未通过或 helper 出错），请看 /var/log/b-ui-resi-blacklist.log</div>';
    }
    keys.forEach(k => {
        const up = ups[k] || {};
        const entries = Array.isArray(up.entries) ? up.entries : [];
        const isSel = k === d.selected;
        body += '<div class="resi-bl-upstream' + (isSel ? " selected" : "") + '">';
        body += '<div class="resi-bl-upstream-head">'
            + '<span class="resi-bl-key">' + esc(k) + '</span>'
            + (isSel ? '<span class="resi-type-badge resi-type-http">当前选中</span>' : "")
            + '<span class="resi-bl-meta">' + (up.checking ? "检测中…" : entries.length + " 条 · 最近检测 " + _blFmtTime(up.checkedAt)) + '</span>'
            + '</div>';
        if (!entries.length) {
            body += '<div class="resi-bl-empty">该上游没有需要直出的目标</div>';
        } else {
            body += '<table class="resi-bl-table"><thead><tr><th>目标</th><th>来源</th><th>拒绝原因</th><th>加入时间</th></tr></thead><tbody>';
            entries.forEach(e => {
                body += '<tr>'
                    + '<td class="resi-bl-target">' + esc(e.target) + (e.kind === "port" ? ' <span class="resi-type-badge">端口</span>' : "") + '</td>'
                    + '<td>' + esc(_blSourceLabel(e.source)) + '</td>'
                    + '<td class="resi-bl-reason" title="' + esc(e.reason || "") + '">' + esc(e.reason || "—") + '</td>'
                    + '<td>' + esc(_blFmtTime(e.since)) + '</td>'
                    + '</tr>';
            });
            body += '</tbody></table>';
        }
        body += '</div>';
    });
    if (learned.length) {
        body += '<details class="resi-bl-learned"><summary>候选池（日志学到，' + learned.length + ' 条，每日刷新时一起探测）</summary>';
        body += '<table class="resi-bl-table"><thead><tr><th>上游</th><th>目标</th><th>24h 被拒次数</th><th>最近出现</th></tr></thead><tbody>';
        learned.forEach(l => {
            body += '<tr><td>' + esc(l.upstream) + '</td><td class="resi-bl-target">' + esc(l.target) + '</td><td>' + esc(l.count) + '</td><td>' + esc(_blFmtTime(l.lastSeen)) + '</td></tr>';
        });
        body += '</tbody></table></details>';
    }

    let pinsHtml = "";
    if (!pins.length) {
        pinsHtml = '<div class="resi-bl-empty">暂无钉住的目标</div>';
    } else {
        pins.forEach(p => {
            const mode = p.mode === "resi" ? "resi" : "direct";
            pinsHtml += '<div class="resi-pin-item">'
                + '<span class="resi-bl-target">' + esc(p.target) + '</span>'
                + '<span class="resi-type-badge ' + (mode === "resi" ? "resi-type-http" : "") + '">' + (mode === "resi" ? "强制住宅" : "强制机房") + '</span>'
                + '<button type="button" class="resi-icon-btn resi-del-btn" title="取消钉住" onclick="removeResidentialPin(\'' + esc(p.target) + '\')">✕</button>'
                + '</div>';
        });
    }
    return { badge, body, pins: pinsHtml };
}
// 把渲染结果写进 DOM（三个容器都可能不存在，比如旧页面缓存）
function applyResidentialBlacklist(data) {
    const out = renderResidentialBlacklist(data);
    const badge = $("#resi-blacklist-badge");
    const body = $("#resi-blacklist-body");
    const pins = $("#resi-pins-list");
    if (badge) badge.textContent = out.badge;
    if (body) body.innerHTML = out.body;
    if (pins) pins.innerHTML = out.pins;
}
/* BL-RENDER-END */

let _blPollTimer = null;   // 探测中每 5 秒轮询一次，checking 消失或弹窗关闭即停

function _blStopPoll() {
    if (_blPollTimer) { clearInterval(_blPollTimer); _blPollTimer = null; }
}
function _blStartPoll() {
    if (_blPollTimer) return;
    _blPollTimer = setInterval(() => {
        const m = $("#m-resi");
        if (!m || !m.classList.contains("on")) { _blStopPoll(); return; }
        loadResidentialBlacklist();
    }, 5000);
}

function loadResidentialBlacklist() {
    return api("/residential/blacklist").then(d => {
        const btn = $("#resi-blacklist-probe-btn");
        if (!d || d.error) {
            const badge = $("#resi-blacklist-badge");
            if (badge) badge.textContent = "读取失败";
            _blStopPoll();
            return;
        }
        applyResidentialBlacklist(d);
        const checking = _blIsChecking(d);
        if (btn) { btn.disabled = checking; btn.textContent = checking ? "检测中…" : "立即重新检测"; }
        if (checking) _blStartPoll(); else _blStopPoll();
    }).catch(() => {
        _blStopPoll();
        const badge = $("#resi-blacklist-badge");
        if (badge) badge.textContent = "读取失败";
    });
}

// upstream 省略 = 全部上游；后端 202 后开始轮询
function probeResidentialBlacklist(upstream) {
    _resiClearErr();
    const btn = $("#resi-blacklist-probe-btn");
    if (btn) { btn.disabled = true; btn.textContent = "正在启动检测…"; }
    const body = upstream ? { upstream } : {};
    api("/residential/blacklist/probe", { method: "POST", body: JSON.stringify(body) }).then(r => {
        if (r && r.started) toast("黑名单检测已在后台开始，约 1-3 分钟，有变化会自动应用");
        else _resiErr((r && r.error) || "启动检测失败");
        loadResidentialBlacklist();
    }).catch(e => {
        _resiErr(e.message || "请求失败");
        loadResidentialBlacklist();
    });
}

function addResidentialPin() {
    _resiClearErr();
    const inp = $("#resi-pin-target");
    const sel = $("#resi-pin-mode");
    const target = inp ? String(inp.value).trim().toLowerCase() : "";
    const mode = (sel && sel.value === "resi") ? "resi" : "direct";
    if (!target) { _resiErr("请输入要钉住的域名或 port:端口"); return; }
    // 与 server.js 同口径的校验，先在前端拦掉
    if (!/^([a-z0-9-]+\.)+[a-z0-9-]+$/.test(target) && !/^port:[0-9]{1,5}$/.test(target)) {
        _resiErr("目标格式无效：填域名（如 www.google.com）或 port:5228");
        return;
    }
    const btn = $("#resi-pin-add");
    const restore = () => { if (btn) { btn.disabled = false; btn.textContent = "钉住"; } };
    if (btn) { btn.disabled = true; btn.textContent = "应用中…"; }
    api("/residential/blacklist/pins", { method: "POST", body: JSON.stringify({ target, mode }) }).then(r => {
        restore();
        if (r && r.success) {
            if (inp) inp.value = "";
            toast("已钉住 " + target + "（" + (mode === "resi" ? "强制住宅" : "强制机房") + "），b-ui-relay 已应用");
            loadResidentialBlacklist();
        } else _resiErr((r && r.error) || "钉住失败");
    }).catch(e => { restore(); _resiErr(e.message || "请求失败"); });
}

function removeResidentialPin(target) {
    _resiClearErr();
    api("/residential/blacklist/pins/" + encodeURIComponent(target), { method: "DELETE" }).then(r => {
        if (r && r.success) { toast("已取消钉住 " + target); loadResidentialBlacklist(); }
        else _resiErr((r && r.error) || "取消钉住失败");
    }).catch(e => _resiErr(e.message || "请求失败"));
}
```

- [ ] **Step 5: Hook the block into `openResi()`**

(a) `web/app.js:868-873` currently reads:

```js
    const table = $("#resi-urls-table");
    if (table) table.replaceChildren();
    cancelAddResiUrl();
    _resiClearErr();
    $("#resi-domains-details").removeAttribute("open");
    openM("m-resi");
```

Replace with:

```js
    const table = $("#resi-urls-table");
    if (table) table.replaceChildren();
    cancelAddResiUrl();
    _resiClearErr();
    $("#resi-domains-details").removeAttribute("open");
    // v3.7.0 R13: 黑名单区折叠、清旧内容、停掉上次遗留的轮询
    const blDetails = $("#resi-blacklist-details");
    if (blDetails) blDetails.removeAttribute("open");
    _blStopPoll();
    applyResidentialBlacklist({ selected: null, upstreams: {}, pins: [], learned: [], applied: null });
    const blBadge = $("#resi-blacklist-badge");
    if (blBadge) blBadge.textContent = "";
    openM("m-resi");
```

(b) `web/app.js:930-933` currently reads:

```js
        if (hint) hint.textContent = r.global ? "全走住宅" : "域名分流";

        _resiRenderDomains(r);
    }).catch(() => {
```

Replace with:

```js
        if (hint) hint.textContent = r.global ? "全走住宅" : "域名分流";

        _resiRenderDomains(r);
        // v3.7.0 R13: 黑名单明细独立请求（状态文件小、读得快；探测中会自己 5 秒轮询）
        loadResidentialBlacklist();
    }).catch(() => {
```

- [ ] **Step 6: Add the 「黑名单」 column to the health card**

(a) `web/app.js:1129` currently reads:

```js
            ["线路", "上游", "巡检", "出口 IP", "类型"].forEach(t => {
```

Replace with:

```js
            ["线路", "上游", "巡检", "出口 IP", "类型", "黑名单"].forEach(t => {
```

(b) `web/app.js:1154-1157` currently reads:

```js
                const cType = document.createElement("span");
                cType.textContent = eg.type || "unknown";
                cType.title = eg.isp || "";
                row.append(cTag, cUp, cState, cIp, cType);
```

Replace with:

```js
                const cType = document.createElement("span");
                cType.textContent = eg.type || "unknown";
                cType.title = eg.isp || "";
                // v3.7.0 R13: 该线路的机房直出黑名单条数（0 = 全部走住宅）
                const cBl = document.createElement("span");
                const blN = (typeof m.blacklist_count === "number") ? m.blacklist_count : 0;
                cBl.textContent = String(blN);
                cBl.title = blN ? blN + " 个目标经机房直出（住宅弹窗里可查看明细）" : "没有需要直出的目标";
                if (blN) cBl.className = "resi-member-bl";
                row.append(cTag, cUp, cState, cIp, cType, cBl);
```

- [ ] **Step 7: CSS — 6th grid column and the new blacklist/pin styles**

(a) `web/style.css:1162-1170` currently reads:

```css
.resi-member {
    display: grid;
    grid-template-columns: 52px minmax(0, 1fr) 58px minmax(0, 1fr) minmax(0, 76px);
    gap: 8px;
    align-items: center;
    padding: 6px 4px;
    font-size: 11px;
    border-bottom: 1px solid var(--hairline);
}
```

Replace with:

```css
.resi-member {
    display: grid;
    /* v3.7.0 R13: 末列「黑名单」条数 */
    grid-template-columns: 52px minmax(0, 1fr) 58px minmax(0, 1fr) minmax(0, 76px) 44px;
    gap: 8px;
    align-items: center;
    padding: 6px 4px;
    font-size: 11px;
    border-bottom: 1px solid var(--hairline);
}

.resi-member-bl { color: var(--gold-700); font-weight: 600; }
```

(b) Append at the very end of `web/style.css` (after the last line, `}` of `@media (max-width: 420px) { .stats { grid-template-columns: 1fr; } }`, line 1815):

```css

/* ---------- v3.7.0 R13: 机房直出黑名单（住宅弹窗） ---------- */
.resi-blacklist-body { margin-bottom: 10px; }

.resi-bl-upstream {
    border: 1px solid var(--hairline);
    border-radius: 10px;
    background: var(--surface);
    margin-bottom: 10px;
    overflow: hidden;
}
.resi-bl-upstream.selected { border-color: rgba(184, 150, 77, 0.45); }

.resi-bl-upstream-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--ivory-200);
    font-size: 12px;
}
.resi-bl-key {
    font-family: 'JetBrains Mono', monospace;
    font-weight: 600;
    color: var(--text-main);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.resi-bl-meta { margin-left: auto; color: var(--text-light); font-size: 11px; white-space: nowrap; }

.resi-bl-empty {
    padding: 10px 12px;
    font-size: 12px;
    color: var(--text-light);
}

.resi-bl-warn {
    font-size: 11.5px;
    color: #8C660C;
    background: var(--warning-bg);
    border: 1px solid rgba(184, 134, 11, 0.20);
    border-radius: 8px;
    padding: 7px 10px;
    margin-bottom: 10px;
    line-height: 1.4;
}

.resi-bl-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 11.5px;
    table-layout: fixed;
}
.resi-bl-table th, .resi-bl-table td {
    padding: 6px 10px;
    text-align: left;
    border-top: 1px solid var(--hairline);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.resi-bl-table th { color: var(--text-dim); font-size: 10px; letter-spacing: 0.04em; font-weight: 600; }
.resi-bl-table th:nth-child(1), .resi-bl-table td:nth-child(1) { width: 36%; }
.resi-bl-table th:nth-child(2), .resi-bl-table td:nth-child(2) { width: 16%; }
.resi-bl-table th:nth-child(4), .resi-bl-table td:nth-child(4) { width: 18%; }
.resi-bl-target { font-family: 'JetBrains Mono', monospace; }
.resi-bl-reason { color: var(--text-dim); }

.resi-bl-learned { margin-bottom: 10px; }
.resi-bl-learned > summary {
    cursor: pointer;
    font-size: 11.5px;
    color: var(--text-dim);
    padding: 6px 0;
    user-select: none;
}

.resi-pin-row {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 12px;
    margin-bottom: 8px;
}
.resi-pin-input {
    flex: 1;
    min-width: 0;
    font-family: 'JetBrains Mono', monospace;
    font-size: 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 8px 12px;
    color: var(--text-main);
    outline: none;
    transition: border-color 0.2s, box-shadow 0.2s;
}
.resi-pin-input:focus { border-color: var(--primary); box-shadow: 0 0 0 3px var(--primary-glow); }
.resi-pin-select {
    font-size: 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 8px 10px;
    color: var(--text-main);
    outline: none;
}

.resi-pins-list { margin-bottom: 8px; }
.resi-pin-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 4px;
    border-bottom: 1px solid var(--hairline);
    font-size: 12px;
}
.resi-pin-item:last-child { border-bottom: none; }
.resi-pin-item .resi-del-btn { margin-left: auto; }

@media (max-width: 768px) {
    .resi-pin-row { flex-wrap: wrap; }
    .resi-pin-input { flex-basis: 100%; }
}
```

- [ ] **Step 8: Syntax check and run both UI tests**

Run: `cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && node --check web/app.js && bash tests/resi-blacklist/test_ui_render.sh && bash tests/resi-blacklist/test_server_api.sh`
Expected: `node --check` silent; `test_ui_render.sh` prints `PASS 24 / FAIL 0`; `test_server_api.sh` still `PASS 55 / FAIL 0` (Task 9 unchanged). If `applyResidentialBlacklist writes exactly …` fails, the region has a side effect or reads DOM state during render — move it out of the `BL-RENDER` markers.

- [ ] **Step 9: Manual check in a browser against the scratch server (optional but recommended)**

Run (reuse the fixtures from `test_server_api.sh` by commenting out `cleanup`'s `rm -rf`, or start the panel per CLAUDE.md with `BASE_DIR=/tmp/bui-dev … ADMIN_PORT=18080`): open `http://127.0.0.1:18080`, log in with `test123`, open 「住宅 IP 出站」, expand 「机房直出黑名单（自动检测）」.
Expected: summary badge `2 条 · 最近检测 HH:MM` (or `检测中…` after clicking 「立即重新检测」, with the button disabled and the badge refreshing every 5 s until `checking` clears — write `"checking":null` into the scratch state file to end it); entry table with 目标 / 来源 / 拒绝原因 / 加入时间; pins list with ✕ buttons; the dashboard 「住宅 IP 健康」 table shows a 「黑名单」 column with `2` for `resi-1`. No horizontal scroll at 375 px width.

- [ ] **Step 10: Commit**

```bash
cd /Users/woo/Desktop/b-ui/.claude/worktrees/bui-c-tun-mode-issue-df82e5 && git add web/index.html web/app.js web/style.css tests/resi-blacklist/test_ui_render.sh && git commit -m "feat(web): 住宅弹窗新增机房直出黑名单区（明细/立即重测/钉住）+ 健康卡黑名单列

- #resi-blacklist-details：按上游分组的目标/来源/拒绝原因/加入时间表、候选池折叠、上次应用失败提示
- 「立即重新检测」→ POST probe，检测中每 5 秒轮询 GET 直到 checking 消失
- 钉住区：输入框 + 强制机房/强制住宅 + 已钉列表可移除
- 首页住宅 IP 健康表增加「黑名单」列（members[].blacklist_count）
- 渲染函数 renderResidentialBlacklist 为纯函数（BL-RENDER 标记区），tests/resi-blacklist/test_ui_render.sh 用假 DOM 冒烟

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
