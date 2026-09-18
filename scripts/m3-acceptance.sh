#!/usr/bin/env bash
# b-ui v4.1 M3 机器化验收（spec §6 的「m3 判据变化」那一行）。逐项判定：
#   ① 加用户时三个内核单元的 NRestarts / MainPID 不变、`hy2-residential.json` 字节不变，
#      且在线会话不断
#   ② 已知字节数（下载源实报的 Content-Length）的住宅计数误差 ±5%（住宅那一路的 usage
#      由服务端经 v2ray_api 采上来，脚本只读 `GET /api/users`）
#   ③ 到期语义（4.1 变了）：**住宅握手仍成功、请求全被拒**，`/api/online` 里该用户归零、
#      同槽邻居的会话不断；直连仍是握手即拒，`auth-hook.log` 记 expired / blocked
#   ④ 删一个上游后：任一用户三种订阅的 sha 不变、每个人的门位与期望一致、
#      `hysteria-residential` 不重启（**要 `--remove-upstream`**，删上游不可逆）
#   ④' 门位的两件**活体**证据，**默认就跑**（只读、无副作用，不需要 `--remove-upstream`）：
#      **门位重放没报过失败事件**（`bui incidents`）、**未封用户经住宅节点仍真能出网**。
#      判据 ④ 的门位那半条只比「面板算出的期望门位」（真机上几乎恒真），所以这两件事才是
#      spec §3.4 真正要盯的；它们锁在 `--remove-upstream` 后面就等于没人看得到（T19 正文
#      那条命令也不带那个开关）。
#   ⑤ 重启守护进程后计数不重复（不翻倍）
#
#   真机：sudo bash scripts/m3-acceptance.sh
#   自测：bash scripts/m3-acceptance.sh --self-test   # 不出网、不碰 /opt、不需要 root
#
# 选项：--admin-password-file <文件>  管理员密码（`ADMIN_PASSWORD=` 行或整行密码）；
#                                    缺省读 <base>/v3-backup/admin.env
#       --download-url <url>         下载源（缺省按候选列表逐个 HEAD 探活取第一个可用的）
#       --keep-url <url>             保活用的小文件（**必须回 200 且只有几 KB**——它的字节
#                                    数进判据 ② 的计数）。同一个 URL 在不同出口 IP 上结果
#                                    不一样（Cloudflare 的 `__down` 在开发机回 200、在 rick
#                                    实测 403），**staging 跑 ①–④ 前先用 `--keep-url` 钉一个
#                                    在那台机器上实测回 200 的小文件**。
#       --base <目录>                缺省 /opt/b-ui
#       --remove-upstream <sel>      判据 ④ 要删的那条上游（`host:port` / uuid / `resi-N`）。
#                                    **不给就判据 ④ SKIP**（④' 照跑）：删上游是不可逆的
#                                    （上游凭据删了这脚本取不回来），只在 staging 显式开，
#                                    **且只在池里 ≥2 条上游时开**——删成空池后中继 fail-open
#                                    全部直连，④' 的活体探测无论门位对不对都回 200。
#       --strict                     把 SKIP 也算进退出码（判据被跳过 = 那一条没验收到）。
# 环境变量：M3_ADD_WAIT / M3_FLUSH_WAIT / M3_STABLE_WAIT / M3_KICK_WAIT /
#           M3_RESTART_WAIT / M3_KEEP_INTERVAL / M3_USAGE_POLL / M3_FIRST_OK_WAIT /
#           M3_BLOCK_WAIT / M3_GATE_WAIT —— 各等待窗口（秒），自测里调小。
#
# 退出码 = FAIL 数（0 = 全过）、`--strict` 下 = FAIL + SKIP；前置条件缺失（没有 python3 /
# 取不到密码 / 登录失败）打 FATAL 退 2。**SKIP 一律进摘要并计数**：判据 ②③ 共用
# 「取不到住宅 HY2 凭据」这一条静默路径，跳过了却只打「N PASS / 0 FAIL」就是假绿。
# 密码与 JWT **绝不进 argv**：密码经 0600 临时文件喂 curl 的 `--data-binary`，
# token 经 `curl -K -` 的 stdin 配置传（`ps` 看不到）。
set -uo pipefail
LC_ALL=C

BASE=${BASE:-/opt/b-ui}
ADMIN_PW_FILE=""
# 保活源：每秒打一次、字节数要进判据 ② 的计数 ⇒ 只能是几 KB 的小文件，而且**必须回 200**
# （判据 ①③④' 都拿「200」当通路证据）。缺省曾是 `speed.cloudflare.com/__down?bytes=2048`，
# 但这脚本自己下面那段注释就记着它在 rick 实测 403（速度站按出口 IP / ASN 挡）——默认值
# 指着一个已知会 403 的源，真机上三条判据会集体假 FAIL。换成 IANA 的 example.com 首页
# （2026-09-17 实测 200 / 559 字节，没有速度站那套限流）。出口不同结果就可能不同，所以
# staging 前照 `usage()` 那行提示用 `--keep-url` 钉一个在那台机器上实测过的源。
KEEP_URL="https://example.com/"
SELF_TEST=0
# 判据 ④ 要删的上游（`--remove-upstream`）。空 = 判据 ④ SKIP（只读的 ④' 不受影响，照跑）
REMOVE_UPSTREAM=""
# `--strict`：SKIP 也进退出码
STRICT=0

# 判据 ② 的下载源：不写死一个站。真机 rick 实测（2026-09-13）
# `https://speed.cloudflare.com/__down` 返 403（实收 1 字节）、OVH 的 100Mio 档 404，所以
# 缺省改成一串候选，逐个 HEAD 探活取**第一个 200 且 Content-Length 已知**的那个。候选之间
# 大小不保证一致（各站按 MB / Mb 命名的口径不同，随时也会换文件），所以判据 ② 的「已知
# 字节数」一律取该源实报的 Content-Length，不写死 104857600。
DOWNLOAD_CANDIDATES=(
  "http://speedtest.tele2.net/100MB.zip"
  "https://speed.hetzner.de/100MB.bin"
  "https://proof.ovh.net/files/100Mb.dat"
)
# 非空 = `--download-url` 显式指定（只探它自己）；空 = 走候选列表
DOWNLOAD_URL=""
# 探活后填成选中源的 Content-Length；探活前没有可信值
EXPECT_BYTES=0
# 探活的逐个失败原因（全失败时进判据 ② 的 FAIL 文案）
PROBE_DETAIL=""
TOL_PCT=5
DL_TIMEOUT=600
HEAD_TIMEOUT=20

# 判据 ① 看这三个内核单元，判据 ⑤ 重启的是守护进程。
#
# **4.1 起是固定三项**：住宅 HY2 收成了一个 sing-box 实例（单元名沿用
# `hysteria-residential`，spec §2.5），4.0 那些带槽序号后缀的住宅单元进了 `LEGACY_UNITS`
# ⇒ 再没有「按槽枚举单元」这回事，`bui residential slots --json` 也不必为此调用。
KERNEL_UNITS="hysteria-server hysteria-residential xray"
# 判据 ④ 只看住宅这一个单元（删上游改的是槽位与门位，不该重启内核）
RESI_UNIT="hysteria-residential"
DAEMON_UNIT="b-ui"

# 到期用 `days` 设的最短正 TTL（见 check_expiry 的说明）
EXPIRE_TTL=2

# 各等待窗口。缺省值来自 spec §9 与 traffic.rs：采样 10s 一轮、用量最多 30s 合并落盘一次，
# 所以「下完到面板可见」最坏 ~40s，这里给到 90s。
ADD_WAIT=${M3_ADD_WAIT:-10}
FLUSH_WAIT=${M3_FLUSH_WAIT:-90}
KICK_WAIT=${M3_KICK_WAIT:-60}
RESTART_WAIT=${M3_RESTART_WAIT:-30}
KEEP_INTERVAL=${M3_KEEP_INTERVAL:-1}
USAGE_POLL=${M3_USAGE_POLL:-5}
# 判据 ② 的「终值」判定窗口：usage.total 连续 STABLE_WAIT 秒没变才算落盘完毕。必须 ≥
# 采样(10s) + 落盘(30s) = 40s —— 一次 100MB 下载常常跨过一个落盘 tick，面板会**先落半截**
# （比如 60MB），剩下的要等下一轮（最多再 30s）才补齐；若只要求「连续两次读到同一个数」
# （5s 间隔），半截值就会被当成终值，算出 -40% 的假 FAIL。
STABLE_WAIT=${M3_STABLE_WAIT:-40}
# 保活首次成功、到期判定生效各等多久
FIRST_OK_WAIT=${M3_FIRST_OK_WAIT:-30}
BLOCK_WAIT=${M3_BLOCK_WAIT:-15}
# 判据 ④ 的门位收敛窗口：删上游改了槽位，门位重放走「`StateChanged` + 60 秒安全网」两条
# 触发路径（spec §3.4），所以最坏要等满那一轮安全网。
GATE_WAIT=${M3_GATE_WAIT:-75}
# 判据 ④' 的事件窗口起点：**在脚本干任何事之前**取（这里是加载期，比加用户 / 到期 / 删上游
# 都早），所以窗口必然盖住删上游与它后面那一轮门位重放。方向只能往早不能往晚：取晚了
# （比如挪到 `sleep "$GATE_WAIT"` 之后、或挪进 check_gate_live 里）窗口就漏掉正要盯的那一段
# 失败事件，判据 ④' 恒绿。池里早先的旧失败在这个起点之前，照样不算这一笔。
GATE_SINCE=$(date +%s)
# 判据 ③ 的「`/api/online` 归零」要连续读到几轮才算数（一次读到 0 可能只是采样间隙）
ONLINE_ZERO_ROUNDS=3
# 判据 ③ 住宅那半（`probe_until_fail`）的「稳定失败」判据：要连续读到几次非 200 才认门已切
# `deny`（一次失败可能是收敛中途的抖动）。与 `wait_session_drop` / `wait_online_zero` 同一口径。
RESI_FAIL_ROUNDS=3

pass=0
fail=0
# SKIP 也计数：判据 ②③ 共用「取不到住宅 HY2 凭据」这一条静默跳过路径（`/api/nodes` 的
# 形状漂移、凭据池还没发凭据都会踩到），不计数的话整跑仍显示「N PASS / 0 FAIL」并退 0。
skipped=0

# 运行期状态
WORK=""
API=""
API_OUT=""
TOKEN=""
PW_SRC=""
TMP_USER=""
TMP_PASS=""
# 临时用户的住宅 HY2 凭据与端口：`{name}` / `{secret}` / `ports.hy2_resi`，都取自他自己的
# `/api/nodes/<token>`（订阅怎么发的，判据就怎么连；住宅密码不是 `hy2_password`）
RESI_NAME=""
RESI_SECRET=""
RESI_PORT=""
PIDS=""
SNI=""
HY2_PORT=""
ADMIN_PORT=""
OBFS_ON=0
OBFS_PW=""

ok() { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
no() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }

# 退出码：缺省 = FAIL 数，`--strict` 把 SKIP 也算进去。$1=FAIL 数 $2=SKIP 数
exit_code() {
  local rc=${1:-0}
  [ "${STRICT:-0}" = "1" ] && rc=$((rc + ${2:-0}))
  [ "$rc" -gt 255 ] && rc=255
  printf '%s\n' "$rc"
}

need_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'FATAL 需要 python3 来解析 JSON\n'
    exit 2
  fi
}

mk_work() {
  WORK=$(mktemp -d) || {
    printf 'FATAL 建不出临时目录\n'
    exit 2
  }
  chmod 700 "$WORK"
  API_OUT="$WORK/api.out"
}

track_pid() { [ -n "${1:-}" ] && PIDS="$PIDS $1"; }

# 已经收干净的 PID 从列表里摘掉：退出时 kill_tracked 就不会再冲着一个可能被复用的 PID 开枪
untrack_pid() {
  local p out=""
  for p in $PIDS; do
    [ "$p" = "${1:-}" ] || out="$out $p"
  done
  PIDS=$out
}

kill_tracked() {
  local p
  for p in $PIDS; do kill "$p" 2>/dev/null; done
  for p in $PIDS; do wait "$p" 2>/dev/null; done
  for p in $PIDS; do kill -9 "$p" 2>/dev/null; done
  PIDS=""
}

# 无论成败都收摊：杀掉所有 hysteria 客户端、删掉临时用户、删掉带凭据的临时目录
cleanup() {
  kill_tracked
  if [ -n "$TMP_USER" ] && [ -n "$TOKEN" ] && [ -d "$WORK" ]; then
    api DELETE "/api/users/$TMP_USER" >/dev/null 2>&1
  fi
  [ -n "$WORK" ] && rm -rf "$WORK"
}

# ---------------------------------------------------------------------------
# 期望态与凭据
# ---------------------------------------------------------------------------

# state.json → 5 行：面板域名(SNI) / HY2 直连端口 / 面板端口 / obfs 开关 / obfs 密码
node_params() {
  python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
n = d.get("node") or {}
p = n.get("ports") or {}
o = n.get("obfs") or {}
print(n.get("domain") or "")
print(p.get("hy2") or "")
print(p.get("admin") or "")
print("1" if o.get("enabled") else "0")
print(o.get("password") or "")
PY
}

# admin.env 形式取 `ADMIN_PASSWORD=` 后面的整段，否则把首行当成密码本体
read_admin_password() {
  [ -f "$1" ] || return 0
  if grep -q '^ADMIN_PASSWORD=' "$1" 2>/dev/null; then
    grep '^ADMIN_PASSWORD=' "$1" | head -n 1 | cut -d= -f2-
  else
    head -n 1 "$1"
  fi
}

load_node_params() {
  local p
  p=$(node_params "$BASE/state.json")
  SNI=$(printf '%s\n' "$p" | sed -n 1p)
  HY2_PORT=$(printf '%s\n' "$p" | sed -n 2p)
  ADMIN_PORT=$(printf '%s\n' "$p" | sed -n 3p)
  OBFS_ON=$(printf '%s\n' "$p" | sed -n 4p)
  OBFS_PW=$(printf '%s\n' "$p" | sed -n 5p)
  if [ -z "$HY2_PORT" ] || [ -z "$ADMIN_PORT" ]; then
    printf 'FATAL 读不到 %s 的 node.ports（hy2=%s admin=%s）\n' "$BASE/state.json" "$HY2_PORT" "$ADMIN_PORT"
    exit 2
  fi
}

# ---------------------------------------------------------------------------
# 面板 API（契约见 crates/bui/src/modules/panel/api_admin.rs）
# ---------------------------------------------------------------------------

# `POST /api/login` → 回显 token（失败回显空）。密码写 0600 临时文件，不进 argv。
api_login() {
  local f="$WORK/login.json" body
  (
    umask 077
    printf '%s' "$1" | python3 -c 'import json,sys; sys.stdout.write(json.dumps({"password": sys.stdin.read()}))' >"$f"
  )
  body=$(curl -s --max-time 15 -H 'Content-Type: application/json' \
    --data-binary "@$f" "$API/api/login" 2>>"$WORK/curl.err")
  rm -f "$f"
  printf '%s' "$body" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("token") or "")
except Exception:
    pass'
}

# $1=方法 $2=路径 [$3=请求体文件] → 回显 HTTP 状态码；响应体落在 $API_OUT。
# JWT 经 `-K -` 的 stdin 配置传（不进 argv）。
api() {
  local m=$1 p=$2 f=${3:-}
  local a=(-s -o "$API_OUT" -w '%{http_code}' --max-time 60 -X "$m")
  if [ -n "$f" ]; then
    a+=(-H 'Content-Type: application/json' --data-binary "@$f")
  fi
  printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    curl "${a[@]}" -K - "$API$p" 2>>"$WORK/curl.err"
}

api_body() { cat "$API_OUT" 2>/dev/null; }

# $1 = 存着 `GET /api/users` 响应的文件，$2 = 用户名
# → 3 行：usage.total / blocked(0|1) / limits.expiresAt
user_row() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    arr = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for u in arr if isinstance(arr, list) else []:
    if u.get("username") == sys.argv[2]:
        print((u.get("usage") or {}).get("total", ""))
        print("1" if u.get("blocked") else "0")
        print((u.get("limits") or {}).get("expiresAt") or "")
        break
PY
}

read_user_row() {
  [ "$(api GET /api/users)" = "200" ] || return 0
  user_row "$API_OUT" "$1"
}

# $1 = 存着 `GET /api/users` 响应的文件，$2 = 要排除的用户名
# → 2 行：一个「已有」可用用户的用户名与 HY2 密码（判据 ① 的邻居会话用）。
# 条件：`blocked=false`——面板投影里这一位已经把 disabled / 到期 / 流量限额用尽
# 三种都算进去了（users.rs 的 `is_blocked`），而直读 state.json 只能看前两种，
# 会挑中一个配额已用尽的人，害得判据 ① 假 FAIL；protocol 含 hysteria2（fusion /
# hysteria2）；有密码；且不是本次的临时用户。
neighbour_pick() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    arr = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
skip = sys.argv[2]
for u in arr if isinstance(arr, list) else []:
    pw = u.get("password") or ""
    if u.get("blocked") or u.get("disabled") or u.get("username") in ("", skip) or not pw:
        continue
    if u.get("protocol") not in ("fusion", "hysteria2"):
        continue
    print(u["username"])
    print(pw)
    break
PY
}

# $1 = 要排除的用户名 → 同 neighbour_pick（读不到用户表就回显空，上层 SKIP）
neighbour_from_api() {
  [ "$(api GET /api/users)" = "200" ] || return 0
  neighbour_pick "$API_OUT" "$1"
}

# $1 = 存着 `GET /api/users` 响应的文件，$2 = 要排除的用户名，$3 = 想要的槽序号
# → 3 行：一个**有住宅 HY2 门位**的已有用户的用户名 / 订阅 token / 槽序号（判据 ③ 的
# 「同槽其他用户会话不断」用）。优先同槽（他们共用 `slot-<i>-out` 这一个出站，是最严的
# 那种邻居），同槽没人就退回任意一个住宅 HY2 用户，槽序号照实回显、由调用方在文案里点明。
resi_neighbour_pick() {
  python3 - "$1" "$2" "$3" <<'PY'
import json, sys
try:
    arr = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
skip, want = sys.argv[2], sys.argv[3]
cands = []
for u in arr if isinstance(arr, list) else []:
    gate = u.get("hy2ResiGate") or ""
    if u.get("blocked") or u.get("disabled") or u.get("username") in ("", skip):
        continue
    if not gate.startswith("slot-") or not u.get("subToken"):
        continue
    cands.append(u)
for u in sorted(cands, key=lambda x: str(x.get("slot")) != want):
    print(u["username"])
    print(u["subToken"])
    print(u.get("slot", ""))
    break
PY
}

# $1 = 要排除的用户名 $2 = 想要的槽序号 → 同 resi_neighbour_pick
resi_neighbour_from_api() {
  [ "$(api GET /api/users)" = "200" ] || return 0
  resi_neighbour_pick "$API_OUT" "$1" "$2"
}

# $1 = 存着 `GET /api/users` 响应的文件，$2 = 用户名 → 2 行：订阅 token / 槽序号
user_sub_row() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    arr = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for u in arr if isinstance(arr, list) else []:
    if u.get("username") == sys.argv[2]:
        print(u.get("subToken") or "")
        print(u.get("slot", ""))
        break
PY
}

read_sub_row() {
  [ "$(api GET /api/users)" = "200" ] || return 0
  user_sub_row "$API_OUT" "$1"
}

read_user_total() { read_user_row "$1" | sed -n 1p; }
read_user_blocked() { read_user_row "$1" | sed -n 2p; }

# 免鉴权端点（三种订阅 + `/api/nodes`）。**路径末段就是凭据**，所以 URL 经 `curl -K -` 的
# stdin 配置传，token 一次都不进 argv（`ps` 会泄露，与面板密码同一条规矩）。
# $1 = 路径 → 回显响应体
pub_body() {
  printf 'url = "%s%s"\n' "$API" "$1" |
    curl -fsS --max-time 30 -K - 2>>"$WORK/curl.err"
}

# $1 = 路径 → 回显响应体的 sha256。取不到也**照样占一行**（回显空行）：sub_shas 的三行是
# 按行号对上名字的，中间那个 404 少打一行会让后面的 sha 集体错位、报错报到别的端点头上。
pub_sha() {
  local b
  b=$(pub_body "$1") || {
    printf '\n'
    return 0
  }
  if [ -z "$b" ]; then
    printf '\n'
    return 0
  fi
  printf '%s' "$b" | sha256sum | cut -d' ' -f1
}

# $1 = 订阅 token → 3 行 sha256：/api/sub、/api/subscription、/api/clash。
# 判据 ④ 的核心承诺是「删上游不动任何人手里那份订阅」，所以三种订阅都要比，
# 不能只比 v2rayN 那一种。
sub_shas() {
  pub_sha "/api/sub/$1"
  pub_sha "/api/subscription/$1"
  pub_sha "/api/clash/$1"
}

# $1 = 订阅面 token → 3 行：住宅 HY2 节点的 `{name}` / `{secret}` / 端口。
# 4.1 的住宅密码是凭据池里那条 `{name}:{secret}`（不是 `hy2_password`），端口恒为
# `ports.hy2_resi` —— 订阅怎么发的，判据就照样连（`nodes::nodes_for` 是唯一口径）。
# 没有住宅 HY2 节点（没住宅权益 / 还没分到凭据）时三行都空，上层 SKIP。
resi_node_of() {
  pub_body "/api/nodes/$1" | python3 -c 'import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
for n in (d or {}).get("nodes") or []:
    if n.get("kind") != "hy2_residential":
        continue
    t = n.get("transport") or {}
    print(t.get("username") or "")
    print(t.get("password") or "")
    print(n.get("port") or "")
    break' 2>/dev/null
}

# 每行 `<用户名> <槽序号> <门位> <blocked>`，只含有住宅 HY2 门位的用户（判据 ④ 用）。
# 门位来自 `GET /api/users` 的 `hy2ResiGate` —— 它与门位收敛同一口径
# （`panel::gates::expected`），面板与内核的差集由守护进程自己收敛。
gate_rows() {
  [ "$(api GET /api/users)" = "200" ] || return 0
  python3 - "$API_OUT" <<'PY'
import json, sys
try:
    arr = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for u in arr if isinstance(arr, list) else []:
    gate = u.get("hy2ResiGate")
    if not gate:
        continue
    print("%s %s %s %s" % (u.get("username") or "?", u.get("slot", "-"), gate,
                           "1" if u.get("blocked") else "0"))
PY
}

# 判据 ④ 的**活体**证据①：门位重放有没有报过失败。门位那半条（gate_rows + judge_gates）
# 比的是面板算出的**期望**门位与脚本按同样输入重算的同一个期望值 —— 它抓不到 spec §6/§3.4
# 真正要盯的「Clash API 的 select 重放失败」：`PUT /proxies/gate-<id>` 失败时面板照样回显
# 期望值，只有日志哨兵会落一条 `hy2_resi_gate_sync_failed` / `hy2_resi_gate_replay_failed`
# 事件（`modules/sentinel/signature.rs`）。
# $1 = 只看这个 epoch 秒（含）之后的事件 → 每行 `<签名> <对象> <结果>`（空 = 干净）。
# 时刻解析不出来的事件**一并回显**：宁可多报一条，不能漏掉真失败。回包整个读不出来
# （`bui` 不在 / 守护进程连不上 / 不是 JSON）时回显 `!unreadable` —— 那是「无从核对」而不是
# 「干净」，由 judge_gate_incidents 判失败。
gate_incident_rows() {
  "$BASE/bin/bui" incidents --json -n 200 2>>"$WORK/bui.err" |
    python3 -c 'import datetime, json, sys
since = float(sys.argv[1])
want = ("hy2_resi_gate_sync_failed", "hy2_resi_gate_replay_failed")
try:
    rows = (json.load(sys.stdin) or {}).get("incidents")
except Exception:
    print("!unreadable")
    raise SystemExit(0)
for i in rows if isinstance(rows, list) else []:
    if not isinstance(i, dict) or i.get("signature") not in want:
        continue
    try:
        t = datetime.datetime.fromisoformat(
            (i.get("at") or "").replace("Z", "+00:00")).timestamp()
    except Exception:
        t = since
    if t >= since:
        print("%s %s %s" % (i.get("signature"), i.get("subject") or "?",
                            i.get("result") or "?"))' "$1"
}

# $1 = 用户名 → `/api/online` 里他的值（不在表里 = 0；读不到 `/api/online` 回显空）。
# `/api/online` 的值恒为 1（`traffic::tick` 把三个来源归一成「这个人在线」，不在线的人
# 压根不进表），所以「该用户的连接数归零」在面板这一侧就是「他从表里消失」。
online_count() {
  [ "$(api GET /api/online)" = "200" ] || return 0
  python3 - "$API_OUT" "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
print(d.get(sys.argv[2], 0) if isinstance(d, dict) else 0)
PY
}

# 「有效」上游数：`GET /api/residential/status` 的 `upstreams` 行数，**池无效时一律回 0**
# （`status.enabled` = `ResidentialGroup::pool_active()` = 开关开着 **且** 池非空）。
# 读不到 / 形状不对回显空。
#
# 判据 ④' 的活体探测靠它判「探测还有没有判别力」：池无效时中继渲染成「监听照在、出口全
# direct」（`render::relay::config` 的 fail-open，relay.rs 的
# `an_inactive_pool_keeps_the_slot_inbounds_and_fails_open` 就是它的守门测试），于是无论
# 门位指着哪个槽，探测都回 200 —— 那个 200 什么都不证明。
upstream_count() {
  [ "$(api GET /api/residential/status)" = "200" ] || return 0
  python3 - "$API_OUT" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
u = d.get("upstreams") if isinstance(d, dict) else None
if isinstance(u, list):
    print(len(u) if d.get("enabled") else 0)
PY
}

# $1 = 上游选择子（`host:port` / uuid / `resi-N`）→ 回显状态码。
# 凭据不在 argv 里：选择子本身不含密码（`POST /api/residential/remove` 只收 id）。
remove_upstream() {
  local f="$WORK/remove.json" code
  (
    umask 077
    printf '{"id":"%s"}\n' "$1" >"$f"
  )
  code=$(api POST /api/residential/remove "$f")
  rm -f "$f"
  printf '%s' "$code"
}

wait_api_up() {
  local i=0
  while [ "$i" -lt "$1" ]; do
    [ "$(api GET /api/users)" = "200" ] && return 0
    sleep 1
    i=$((i + 1))
  done
  return 1
}

# $1=用户名 → 回显状态码；响应体留在 $API_OUT（调用方从里面取 HY2 密码）。
# protocol=fusion ⇒ 两个内核都要动（hysteria 走 auth-hook 快照、xray 走 gRPC AddUser），
# 判据 ① 才有意义。
create_temp_user() {
  local f="$WORK/create.json" code
  (
    umask 077
    printf '{"username":"%s","protocol":"fusion","residential":true}\n' "$1" >"$f"
  )
  code=$(api POST /api/users "$f")
  rm -f "$f"
  printf '%s' "$code"
}

# 随机用户名后缀
rand_suffix() { od -An -N4 -tx1 /dev/urandom | tr -d ' \n'; }

# $1=存着创建响应的文件 → 回显 HY2 密码
created_password() {
  python3 - "$1" <<'PY'
import json, sys
try:
    print(json.load(open(sys.argv[1])).get("password") or "")
except Exception:
    pass
PY
}

# 判据 ③ 的到期：面板 `PUT /api/users/<user>` 只收 `days`（相对天数），而
# `users::expires_from_days` 只接受 days > 0 —— `days <= 0` 被当成「永不过期」
# （crates/bui/src/modules/panel/users.rs:161），所以**没有**直接写过去时间的字段。
# 这里给最短的正 TTL（EXPIRE_TTL 秒 = days × 86400）再等它过期，等价于「到期用户」。
expire_temp_user() {
  local f="$WORK/expire.json" code
  (
    umask 077
    printf '{"days":%s}\n' "$(awk -v s="$EXPIRE_TTL" 'BEGIN{printf "%.10f", s / 86400}')" >"$f"
  )
  code=$(api PUT "/api/users/$TMP_USER" "$f")
  rm -f "$f"
  printf '%s' "$code"
}

wait_user_blocked() {
  local i=0
  while [ "$i" -lt "$2" ]; do
    [ "$(read_user_blocked "$1")" = "1" ] && return 0
    sleep 1
    i=$((i + 1))
  done
  return 1
}

# ---------------------------------------------------------------------------
# hysteria 客户端与探测（自测里整段被 fake 覆盖）
# ---------------------------------------------------------------------------

free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}

# $1=配置文件 $2=用户名 $3=HY2 密码 $4=本地 socks 端口 $5=服务端端口（缺省 = 直连的
# `ports.hy2`；住宅那一路传 `ports.hy2_resi`，$2:$3 就换成凭据池的 `{name}:{secret}`）。
# 凭据只落 0600 的文件。`user:pass` 原串就是 auth 载荷（spec §7.5 不变量 ①，住宅那边
# sing-box 的 `users[].password` 也是这个形状）；双引号标量 + 转义，密码里的 : # { 都不会歪。
# obfs 两路都带（混淆覆盖全部 HY2 实例，2026-09-15 裁决）。
write_client_cfg() {
  local auth=$2:$3
  auth=${auth//\\/\\\\}
  auth=${auth//\"/\\\"}
  (
    umask 077
    printf 'server: 127.0.0.1:%s\nauth: "%s"\ntls:\n  sni: %s\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:%s\n' \
      "${5:-$HY2_PORT}" "$auth" "$SNI" "$4" >"$1"
    if [ "$OBFS_ON" = "1" ]; then
      printf 'obfs:\n  type: salamander\n  salamander:\n    password: %s\n' "$OBFS_PW" >>"$1"
    fi
  )
}

# 后台起 bundled hysteria 客户端、等它的 socks 端口就绪，回显 PID
start_hy2_client() {
  local cfg=$1 log=$2 socks=$3 pid i=0
  "$BASE/bin/hysteria" client -c "$cfg" >"$log" 2>&1 &
  pid=$!
  while [ "$i" -lt 40 ]; do
    kill -0 "$pid" 2>/dev/null || break
    (exec 3<>"/dev/tcp/127.0.0.1/$socks") 2>/dev/null && break
    sleep 0.25
    i=$((i + 1))
  done
  echo "$pid"
}

# $1=socks 端口 $2=url → HTTP 状态码。
# max-time 压到 8s：判据 ③ 要在 KICK_WAIT(60s) 内数到连续 3 次失败，单次不能拖太久。
probe_socks_code() {
  curl -s --socks5-hostname "127.0.0.1:$1" --max-time 8 \
    -o /dev/null -w '%{http_code}' "$2" 2>/dev/null
}

# $1=socks 端口 $2=url → 「下载字节数 HTTP 状态码」
download_via_socks() {
  curl -s --socks5-hostname "127.0.0.1:$1" --max-time "$DL_TIMEOUT" \
    -o /dev/null -w '%{size_download} %{http_code}' "$2" 2>/dev/null
}

# $1=url → 「<HTTP 状态码> <Content-Length，未知写 `-`>」。HEAD 直连本机出网（下载本身才
# 走 socks），curl 起不来 / 没回状态行就报 `000 -`。跟随重定向，取最后一段的头。
probe_download_head() {
  local hdr code len
  hdr=$(curl -s -L -I --max-time "$HEAD_TIMEOUT" "$1" 2>/dev/null)
  code=$(printf '%s\n' "$hdr" |
    awk '/^HTTP\//{c = $2} END {print (c == "") ? "000" : c}')
  len=$(printf '%s\n' "$hdr" |
    awk 'tolower($1) == "content-length:" {gsub(/\r/, "", $2); l = $2} END {print (l == "") ? "-" : l}')
  printf '%s %s\n' "$code" "$len"
}

# 选定判据 ② 的下载源：显式 `--download-url` 只探它自己，否则按 DOWNLOAD_CANDIDATES 顺序
# 取第一个「200 且 Content-Length 是正整数」的源。成功 ⇒ 设 DOWNLOAD_URL / EXPECT_BYTES 并
# 回 0；全失败 ⇒ 回 1，逐个失败原因留在 PROBE_DETAIL。
resolve_download_url() {
  local url code len num
  local -a cands
  if [ -n "$DOWNLOAD_URL" ]; then
    cands=("$DOWNLOAD_URL")
  else
    cands=("${DOWNLOAD_CANDIDATES[@]}")
  fi
  PROBE_DETAIL=""
  for url in "${cands[@]}"; do
    read -r code len < <(probe_download_head "$url")
    num=$len
    case "$num" in '' | *[!0-9]*) num=0 ;; esac
    if [ "$code" = "200" ] && [ "$num" -gt 0 ]; then
      DOWNLOAD_URL=$url
      EXPECT_BYTES=$num
      return 0
    fi
    PROBE_DETAIL="${PROBE_DETAIL:+$PROBE_DETAIL；}$url → HTTP $code、Content-Length $len"
  done
  return 1
}

# $1=用户名 → `$BASE/auth-hook.log` 里最近一条属于该用户的**判定结果**字段。
# 格式是 `<RFC3339> <addr> <用户名> <结果>`（auth_hook.rs::log_line），到期与封禁分别记
# `expired` / `blocked`。用它把「钩子把人拒了」和「服务端宕了/端口不通」区分开 —— 后者
# 探测同样失败，但不能算判据 ③ 通过。
hook_last_result() {
  awk -v u="$1" '$3 == u {r = $4} END {if (r != "") print r}' \
    "$BASE/auth-hook.log" 2>/dev/null
}

restart_daemon() { systemctl restart "$DAEMON_UNIT" >/dev/null 2>&1; }

# $1=客户端 PID（在命令替换里起的，不是本 shell 的作业，`wait` 用不上）
stop_client() {
  local i=0
  [ -n "${1:-}" ] || return 0
  kill "$1" 2>/dev/null
  while [ "$i" -lt 20 ] && kill -0 "$1" 2>/dev/null; do
    sleep 0.1
    i=$((i + 1))
  done
  kill -9 "$1" 2>/dev/null
  untrack_pid "$1"
}

# 保活：在 $2 秒里经同一条 socks 通路反复打小文件，回显「成功次数 失败次数」。
# 同步跑、不起后台作业 —— 后台作业会一直攥着 `out=$(check_…)` 那个命令替换的管道写端
# （bash 为恢复重定向保留了原 fd1 的副本），命令替换于是永远等不到 EOF。
# 这串持续流量还有第二个用处：H12 的 `/kick` 只是**标记**，要等该用户下次有流量才真断连。
probe_until() {
  local deadline=$((SECONDS + $2)) o=0 b=0
  while :; do
    if [ "$(probe_socks_code "$1" "$KEEP_URL")" = "200" ]; then
      o=$((o + 1))
    else
      b=$((b + 1))
    fi
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep "$KEEP_INTERVAL"
  done
  printf '%s %s\n' "$o" "$b"
}

# 等这条通路第一次打通（客户端起来 + 握手 + 鉴权），一成功就返回。
# $1=socks 端口 $2=最多等几秒 → 「成功次数 失败次数」（成功次数为 0 = 始终没通）
probe_first_ok() {
  local deadline=$((SECONDS + $2)) o=0 b=0
  while :; do
    if [ "$(probe_socks_code "$1" "$KEEP_URL")" = "200" ]; then
      o=1
      break
    fi
    b=$((b + 1))
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep "$KEEP_INTERVAL"
  done
  printf '%s %s\n' "$o" "$b"
}

# 4.1 判据 ③（住宅到期）：到期后经**新建**连接的请求必须**稳定失败**（门切到 `deny`）。
#
# 竞态口径（rick 2026-09-18 实测）：住宅到期是**时间点、无事件**，门（`gate-<id>` selector）
# 收敛不是即时的——由 `traffic::sampling_loop` 每 10 秒那次 `sync_now → gates::converge`
# 把门切 `deny`（`SAMPLE_INTERVAL_SECS=10`；兜底是 `sync_loop` 的 60 秒安全网 `SYNC_INTERVAL_SECS`）。
# 实测孤立到期用户 ~8–9 秒门切 `deny`、探测随之 200→000。所以到期后**头 ~10 秒里探到 200 是
# 正常的**（还没到下一个采样轮），不是 FAIL——旧做法「一看到 200 就判 FAIL」正是在这个
# 窗口里抢跑误判。门一旦 `deny`，出站黑洞掉一切请求，收敛后不可能再漏 200。
#
# 于是判据反过来：轮询到**连续 RESI_FAIL_ROUNDS 次失败**（门已收敛）就回该失败码（PASS）；
# 到 deadline 仍未稳定失败（最后还能 200 / 一直抖不收敛）才回 200（= 门没切干净，FAIL）。
# $1=socks 端口 $2=最多等几秒（门位收敛窗口，传 GATE_WAIT，覆盖到 60 秒兜底路径都够）→ 代表码。
probe_until_fail() {
  local deadline=$((SECONDS + $2)) c run=0
  while :; do
    c=$(probe_socks_code "$1" "$KEEP_URL")
    if [ "$c" = "200" ]; then
      run=0
    else
      run=$((run + 1))
      [ "$run" -ge "$RESI_FAIL_ROUNDS" ] && {
        printf '%s\n' "${c:-000}"
        return
      }
    fi
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep "$KEEP_INTERVAL"
  done
  # 到窗口末仍没连续失败够 RESI_FAIL_ROUNDS 次 ⇒ 门没稳定切到 deny，判 FAIL（回 200）。
  printf '200\n'
}

# $1=客户端日志 → 日志里 `connected to server` 出现的次数。
# 这是**客户端这一侧**唯一的握手证据，两处要用它：
#   - 判据 ③ 的「到期后住宅仍握手成功」（服务端 sing-box 对 hysteria2 鉴权不打任何日志，
#     spec §6，所以只能从客户端看）；
#   - 判据 ③ 的「同槽其他用户 `connected` 计数不变」（每次重连都会再打一行）。
client_connects() {
  local n
  # 日志文件还没生成时 grep 什么都不打（退 2），空值会让上层的算术比较炸掉 ⇒ 归一成 0
  n=$(grep -cF 'connected to server' "${1:-/dev/null}" 2>/dev/null)
  case "$n" in '' | *[!0-9]*) n=0 ;; esac
  printf '%s\n' "$n"
}

# $1=用户名 $2=最多等几秒 → 回显**确认过**的连续归零轮数（没确认满就回 0）。
# 「归零」= 该用户从 `/api/online` 里消失（见 online_count）。要求连续
# ONLINE_ZERO_ROUNDS 轮：一次读到 0 可能只是采样间隙，而误判方向在这里是危险的那一侧
# （门其实没切干净，却判了通过）。
wait_online_zero() {
  local deadline=$((SECONDS + $2)) run=0
  while :; do
    if [ "$(online_count "$1")" = "0" ]; then
      run=$((run + 1))
      [ "$run" -ge "$ONLINE_ZERO_ROUNDS" ] && break
    else
      run=0
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      run=0
      break
    fi
    sleep "$KEEP_INTERVAL"
  done
  printf '%s\n' "$run"
}

# `hy2-residential.json` 的 sha256（读不到回显空）。判据 ① 用它做 spec §3.5 那条守门测试
# 在真机上的对照：用户的任何生命周期动作都不许碰这个文件。
cfg_sha() {
  sha256sum "$BASE/hy2-residential.json" 2>/dev/null | cut -d' ' -f1
}

# probe_first_ok 的「成功次数」→ 探测码：$1 > 0 ⇒ 200（这条通路通了），否则 000。
# judge_expiry_41 / judge_neighbour 收的都是探测码，这里把两种表示收成一处。
first_ok_code() {
  if [ "${1:-0}" -gt 0 ]; then printf '200\n'; else printf '000\n'; fi
}

# 连续 3 次探测失败即认定既有连接已断。$1=socks 端口 $2=最多等几秒
wait_session_drop() {
  local deadline=$((SECONDS + $2)) run=0
  while :; do
    if [ "$(probe_socks_code "$1" "$KEEP_URL")" = "200" ]; then
      run=0
    else
      run=$((run + 1))
      [ "$run" -ge 3 ] && return 0
    fi
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep "$KEEP_INTERVAL"
  done
}

# ---------------------------------------------------------------------------
# 纯判定（自测直接喂数据）
# ---------------------------------------------------------------------------

# 每行：<unit> <NRestarts> <MainPID>。$1 = 要看的单元清单（缺省 KERNEL_UNITS 那三个；
# 判据 ④ 只传住宅那一个）
unit_stamps() {
  local u list=${1:-$KERNEL_UNITS}
  for u in $list; do
    printf '%s %s %s\n' "$u" \
      "$(systemctl show -p NRestarts --value "$u" 2>/dev/null)" \
      "$(systemctl show -p MainPID --value "$u" 2>/dev/null)"
  done
}

# $1=前 $2=后 → 问题描述（空串 = 三个单元都没动过）
judge_stamps() {
  local after=$2 u nb pb na pa out=""
  while read -r u nb pb; do
    [ -n "$u" ] || continue
    na=$(printf '%s\n' "$after" | awk -v u="$u" '$1 == u {print $2}')
    pa=$(printf '%s\n' "$after" | awk -v u="$u" '$1 == u {print $3}')
    if [ -z "$nb" ] || [ -z "$na" ] || [ -z "$pb" ] || [ -z "$pa" ]; then
      out="$out$u: 取不到 NRestarts/MainPID（$nb/$pb → $na/$pa）; "
    elif [ "$nb" != "$na" ] || [ "$pb" != "$pa" ]; then
      out="$out$u: NRestarts $nb→$na MainPID $pb→$pa; "
    fi
  done < <(printf '%s\n' "$1")
  printf '%s' "$out"
}

# $1/$2=窗口前的 ok/bad 次数，$3/$4=窗口后的 → 问题描述（空串 = 会话没断）
judge_session() {
  if [ "${4:-0}" -gt "${2:-0}" ]; then
    printf '窗口内 curl 失败 %d 次（加用户前累计 %d 次）' "$(($4 - $2))" "$2"
    return
  fi
  if [ "${3:-0}" -le "${1:-0}" ]; then
    printf '窗口内成功次数没涨（%s → %s）：会话已经死了或保活循环没跑' "$1" "$3"
  fi
}

# $1=实测增量 $2=已知流量 → 带符号的百分比偏差（已知流量为 0 时回显 nan）
usage_dev() {
  awk -v d="$1" -v e="$2" 'BEGIN {
    if (e + 0 <= 0) { print "nan"; exit }
    printf "%+.2f", (d - e) / e * 100
  }'
}

# $1=实测增量 $2=已知流量 $3=容差% → 问题描述（空串 = 在容差内）。
# 4.1 的住宅那一路由服务端经 v2ray_api 的 `QueryStats(patterns=["user>>>"], reset=true)`
# 采上来（`traffic::tick`），脚本只读 `GET /api/users` 的 `usage` —— 一个住宅进程、
# 一个计数器名空间，不再有「按槽分别核对」这回事。
judge_usage() {
  local dev
  dev=$(usage_dev "$1" "$2")
  if [ "$dev" = "nan" ]; then
    printf '已知流量为 0（什么都没下到）'
    return
  fi
  if awk -v dev="$dev" -v p="$3" 'BEGIN {d = dev < 0 ? -dev : dev; exit !(d > p)}'; then
    printf '偏差 %s%% 超出 ±%s%%（计数 %s 字节 / 已知 %s 字节）' "$dev" "$3" "$1" "$2"
  fi
}

# $1=重启前 usage.total $2=重启后 → 问题描述（空串 = 不减也不重复）。
# 容差 = max(5%, 1MB)：`clear=1` / `reset=true` 让每轮拿到的就是增量，重启不该把累计值
# 再算一遍（翻倍）；反方向的已知降级是 pending 只在内存里，最多丢 30 秒计数。
judge_restart_total() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    if (a == "" || b == "") { printf "读不到 usage.total（%s → %s）", a, b; exit }
    slack = a * 0.05
    if (slack < 1048576) slack = 1048576
    if (b + 0 < a + 0) { printf "usage.total 变小了：%d → %d（计数丢了）", a, b; exit }
    if (b + 0 > a + slack) {
      printf "usage.total 异常增长：%d → %d（+%d 超过容差 %d；≈翻倍就是把累计值当增量重复计了)", a, b, b - a, slack
    }
  }'
}

# $1=前 $2=后 → 问题描述（空串 = `hy2-residential.json` 字节没变）。
# spec §3.5：这个文件只因五件事变化（凭据池扩容 / obfs 开关 / 证书轮换 / 住宅端口改动 / 伪装域变更），
# **用户的任何生命周期动作都不许碰它**——碰了就是全体住宅 HY2 会话陪着重连一次。
judge_cfg_sha() {
  if [ -z "${1:-}" ] || [ -z "${2:-}" ]; then
    printf '读不到 %s/hy2-residential.json 的 sha（%s → %s）' "$BASE" "${1:-空}" "${2:-空}"
    return
  fi
  [ "$1" = "$2" ] || printf 'hy2-residential.json 变了（%s → %s）：用户生命周期动作碰了这个文件（spec §3.5）' "$1" "$2"
}

# 判据 ③ 的 4.1 语义（spec §6）：到期 / 封禁的住宅 HY2 用户**握手仍然成功**，门停在
# `deny` ⇒ 经他的请求全部被拒、`/connections` 里他归零。三个入参都是实测值：
#   $1 = 到期**前**经住宅节点的探测码（必须 200，否则这条判据无从判定）
#   $2 = 到期**后**经新建连接的探测码（必须非 200）
#   $3 = 到期后确认过的 `/api/online` 连续归零轮数（必须 > 0，见 wait_online_zero）
# 回显 `PASS …` / `FAIL …`（与其余判定函数不同口径：这一条自测直接按前缀匹配）。
judge_expiry_41() {
  if [ "${1:-}" != "200" ]; then
    printf 'FAIL 到期前住宅节点就不通（HTTP %s），判据无从判定' "${1:-空}"
    return
  fi
  if [ "${2:-}" = "200" ]; then
    printf 'FAIL 到期后请求还能出网（HTTP 200）：门没切到 deny'
    return
  fi
  case "${3:-}" in
    '' | *[!0-9]*)
      printf 'FAIL 读不到 /api/online 的归零轮数（%s）' "${3:-空}"
      return
      ;;
  esac
  if [ "$3" -le 0 ]; then
    printf 'FAIL 请求被拒了，但 /api/online 里该用户没归零：门没切干净 / 存量连接没被 interrupt（也可能是他那条直连会话还没被踢干净、或读不到 /api/online）'
    return
  fi
  printf 'PASS 到期后握手仍成功、请求全被拒（HTTP %s）、/api/online 连续 %s 轮归零' "$2" "$3"
}

# $1=窗口前的 connected 计数 $2=窗口后的 $3=窗口后的探测码 → 问题描述（空串 = 邻居没受
# 影响）。spec §6 判据 ③ 的第三项：切别人的门不许动到同槽其他用户 ——
# `interrupt_exist_connections` 只作用于那一个 selector，重连会在日志里多一行 connected。
judge_neighbour() {
  if [ "${1:-0}" -le 0 ]; then
    printf '邻居会话压根没建起来（connected 计数 %s）' "${1:-空}"
    return
  fi
  if [ "${2:-0}" -ne "${1:-0}" ]; then
    printf '邻居重连了：connected 计数 %s → %s' "$1" "$2"
    return
  fi
  [ "${3:-}" = "200" ] || printf '邻居窗口后不通了（HTTP %s）' "${3:-空}"
}

# $1=删上游前的三行 sha（/api/sub、/api/subscription、/api/clash）$2=之后的三行
# → 问题描述（空串 = 三种订阅逐字节不变）。
# 这是 4.1 的核心承诺：槽位与对外端口彻底解耦，增删槽只换出口 IP，**不动任何人手里那份
# 订阅**（4.0.x 那套按槽切跳跃段会让全员必须刷新，就是 2026-09-15 那场回归事故）。
judge_sub_sha() {
  local n i=1 a b out=""
  for n in sub subscription clash; do
    a=$(printf '%s\n' "$1" | sed -n "${i}p")
    b=$(printf '%s\n' "$2" | sed -n "${i}p")
    if [ -z "$a" ] || [ -z "$b" ]; then
      out="$out/api/$n 的 sha 取不到（${a:-空} → ${b:-空}）; "
    elif [ "$a" != "$b" ]; then
      out="$out/api/$n 的 sha 变了（$a → $b）; "
    fi
    i=$((i + 1))
  done
  printf '%s' "$out"
}

# $1=每行 `<用户名> <槽序号> <门位> <blocked>` → 问题描述（空串 = 每个人的门位都对）。
# 期望（spec §3.3）：未封 ⇒ 他那一槽的出站 `slot-<槽序号>-out`；已封 / 已到期 ⇒ `deny`。
# `未分配`（面板对悬空凭据的显示）一律算不对。一个住宅 HY2 用户都没有时也判失败 ——
# 那样这半条判据是空转的，不能算通过。
judge_gates() {
  printf '%s\n' "$1" | awk '
    NF == 0 { next }
    {
      n++
      want = ($4 == "1") ? "deny" : "slot-" $2 "-out"
      if ($3 != want) out = out $1 " 的门位是 " $3 "，期望 " want "; "
    }
    END {
      if (n == 0) printf "没有任何住宅 HY2 用户（门位这半条无从核对）"
      else printf "%s", out
    }'
}

# $1 = gate_incident_rows 的输出 → 问题描述（空 = 窗口内没有门位同步 / 重放失败事件）。
# judge_gates 只能证明「面板算出的期望门位自洽」，这一条才是内核里那次 `PUT /proxies` 的
# 活体证据（spec §3.4 的两条触发路径任一失败都会落事件）。
judge_gate_incidents() {
  local n
  case "${1:-}" in
    *'!unreadable'*)
      printf '读不到 bui incidents --json 的回包（%s/bin/bui 不在？守护进程没在跑？）⇒ 门位重放这一项无从核对' "$BASE"
      return
      ;;
  esac
  n=$(printf '%s\n' "${1:-}" | grep -c .)
  [ "$n" = "0" ] ||
    printf '窗口内有 %s 条门位同步/重放失败事件（bui incidents）：%s' \
      "$n" "$(printf '%s' "$1" | tr '\n' ';')"
}

# ---------------------------------------------------------------------------
# 五条判据
# ---------------------------------------------------------------------------

# 判据 ①：加用户时内核不重启、在线会话不断。
# 用户增删只改 auth-snapshot.json（hysteria 侧）与走 gRPC AddUser（xray 侧），
# `clients` 不进 xray 配置的结构哈希（render/xray.rs:64），所以对账不该重启任何内核。
check_add_user() {
  local before after out code creds nuser npass sha0 sha1
  local socks="" cfg client="" live=0 ok0=0 bad0=0 ok1=0 bad1=0

  creds=$(neighbour_from_api "")
  nuser=$(printf '%s\n' "$creds" | sed -n 1p)
  npass=$(printf '%s\n' "$creds" | sed -n 2p)
  if [ -z "$nuser" ] || [ ! -x "$BASE/bin/hysteria" ] || ! command -v curl >/dev/null 2>&1; then
    skip "step1 在线会话不断（没有 blocked=false 的已有 hysteria2 用户 / 没有 $BASE/bin/hysteria / 没有 curl）"
  else
    socks=$(free_port)
    cfg="$WORK/keep-neighbour.yaml"
    write_client_cfg "$cfg" "$nuser" "$npass" "$socks"
    client=$(start_hy2_client "$cfg" "$WORK/keep-neighbour.client.log" "$socks")
    track_pid "$client"
    # 加用户之前先把这条通路打通（同一个客户端进程 ⇒ 同一条 QUIC 连接）
    read -r ok0 bad0 < <(probe_first_ok "$socks" "$FIRST_OK_WAIT")
    if [ "$ok0" -gt 0 ]; then
      live=1
    else
      no "step1 邻居会话建不起来（用户 $nuser 经 :$HY2_PORT 打不通）" \
        "客户端日志末行：$(tail -n 1 "$WORK/keep-neighbour.client.log" 2>/dev/null)"
    fi
  fi

  before=$(unit_stamps)
  sha0=$(cfg_sha)
  TMP_USER="m3-$(rand_suffix)"
  code=$(create_temp_user "$TMP_USER")
  if [ "$code" != "200" ]; then
    no "step1 建临时用户 $TMP_USER 失败（HTTP $code）" "$(api_body)"
    TMP_USER=""
    stop_client "$client"
    return
  fi
  TMP_PASS=$(created_password "$API_OUT")
  # 加用户后的窗口里继续经那条连接打小文件；失败一次都算断
  if [ "$live" = "1" ]; then
    read -r ok1 bad1 < <(probe_until "$socks" "$ADD_WAIT")
  else
    sleep "$ADD_WAIT"
  fi
  after=$(unit_stamps)
  out=$(judge_stamps "$before" "$after")
  if [ -z "$out" ]; then
    ok "step1 加用户 $TMP_USER 后三个内核的 NRestarts/MainPID 不变"
  else
    no "step1 加用户把内核重启了" "$out"
  fi

  # 建号只是「分一条池凭据 + PUT 一次门位」，`hy2-residential.json` 一个字节都不该动
  sha1=$(cfg_sha)
  out=$(judge_cfg_sha "$sha0" "$sha1")
  if [ -z "$out" ]; then
    ok "step1 加用户后 hy2-residential.json 字节不变（${sha0:0:12}…）"
  else
    no "step1 加用户动了 hy2-residential.json" "$out"
  fi

  if [ "$live" = "1" ]; then
    out=$(judge_session "$ok0" "$bad0" "$((ok0 + ok1))" "$((bad0 + bad1))")
    if [ -z "$out" ]; then
      ok "step1 加用户期间邻居会话不断（curl 成功 $ok0→$((ok0 + ok1)) 次，失败恒 $((bad0 + bad1)) 次）"
    else
      no "step1 加用户打断了在线会话" "$out"
    fi
  fi
  # 邻居会话只为这一个窗口而起，判完就收（判据 ②③ 用的是临时用户自己的连接）
  stop_client "$client"
}

# $1=用户名 → 把他的住宅 HY2 凭据与端口读进 RESI_NAME / RESI_SECRET / RESI_PORT，
# 三者齐了回 0。来源是他自己的 `/api/nodes/<token>`：订阅怎么发的，判据就照样连
# （4.1 的住宅密码是池凭据 `{name}:{secret}`，端口恒为 `ports.hy2_resi`）。
load_resi_creds() {
  local tok row
  RESI_NAME=""
  RESI_SECRET=""
  RESI_PORT=""
  tok=$(read_sub_row "$1" | sed -n 1p)
  [ -n "$tok" ] || return 1
  row=$(resi_node_of "$tok")
  RESI_NAME=$(printf '%s\n' "$row" | sed -n 1p)
  RESI_SECRET=$(printf '%s\n' "$row" | sed -n 2p)
  RESI_PORT=$(printf '%s\n' "$row" | sed -n 3p)
  [ -n "$RESI_NAME" ] && [ -n "$RESI_SECRET" ] && [ -n "$RESI_PORT" ]
}

# 判据 ②：已知字节数的流量计数误差 ±5%，走**住宅**那一路（4.1 起它的计数换成了
# v2ray_api 的 `QueryStats`，直连那一路一个字没改 ⇒ 要测的是住宅）。已知字节数来自
# 下载源 HEAD 探活实报的 Content-Length（不写死 104857600 —— 候选源换了、或换成小一号的
# 文件都不该让判据失真）。
# 口径（以代码为准）：面板 `usage.total` = 上下行之和 —— `TxRx::total()` 是 `tx + rx`
# （crates/bui/src/modules/panel/mod.rs:59），住宅那一路的 uplink / downlink 来自
# `user>>>{name}>>>traffic>>>uplink|downlink` 两个计数器，所以一次纯下载 ≈ 100MB(下行)
# + 请求头/TLS 记录开销(上行)，正偏差很小；判定用 curl 自己数的 `size_download` 当
# 「已知流量」。
# 门槛：spec §10 写的是 ≤1%，脚本现值是 ±5%（TOL_PCT）——**按脚本现值判、把实测偏差一并
# 打出来**，要不要收紧由主理人在 T19 的生产演练之后裁决。
# 等待：采样 10s 一轮、用量最多 30s 合并落盘一次（traffic.rs 的
# SAMPLE_INTERVAL_SECS / FLUSH_INTERVAL_SECS），所以最坏 ~40s 才在面板可见，而且一次下载
# 可能跨过落盘 tick、先落半截 ⇒ 轮询到「连续 STABLE_WAIT(40s) 不变」或 FLUSH_WAIT(90s) 为止。
check_traffic() {
  local t0 t1 socks cfg pid out size code dev
  if [ -z "$TMP_USER" ]; then
    skip "step2 住宅计数误差（没有临时用户）"
    return
  fi
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step2 住宅计数误差（$BASE/bin/hysteria 不可执行）"
    return
  fi
  if ! load_resi_creds "$TMP_USER"; then
    skip "step2 住宅计数误差（$TMP_USER 的 /api/nodes 里没有住宅 HY2 节点：住宅未启用 / 凭据池还没给他发凭据）"
    return
  fi
  # 先定下载源：拿不到任何「200 + 已知 Content-Length」的源就没有可信的已知字节数，
  # 判据 ② 直接 FAIL（不能拿写死的 100MB 顶）。
  if ! resolve_download_url; then
    no "step2 没有可用的下载源 ⇒ 判据②无法测量（候选逐个 HEAD 探活全失败）" \
      "$PROBE_DETAIL；可用 --download-url <url> 显式指定一个返回 200 且带 Content-Length 的源"
    return
  fi
  t0=$(read_user_total "$TMP_USER")
  if [ -z "$t0" ]; then
    no "step2 读不到 $TMP_USER 的 usage.total" "$(api_body | head -c 300)"
    return
  fi

  socks=$(free_port)
  cfg="$WORK/dl.yaml"
  write_client_cfg "$cfg" "$RESI_NAME" "$RESI_SECRET" "$socks" "$RESI_PORT"
  pid=$(start_hy2_client "$cfg" "$WORK/dl.client.log" "$socks")
  track_pid "$pid"
  read -r size code < <(download_via_socks "$socks" "$DOWNLOAD_URL")
  stop_client "$pid"

  if [ "$code" != "200" ] || [ "${size:-0}" -lt $((EXPECT_BYTES / 10 * 9)) ]; then
    no "step2 没下满已知字节数（HTTP ${code:-空}，实收 ${size:-0} / 应 $EXPECT_BYTES 字节）" \
      "下载源 $DOWNLOAD_URL；客户端日志末行：$(tail -n 1 "$WORK/dl.client.log" 2>/dev/null)"
    return
  fi

  t1=$(wait_for_usage "$TMP_USER" "$t0" "$FLUSH_WAIT")
  if [ -z "$t1" ]; then
    no "step2 下载后读不到 usage.total" "等了 ${FLUSH_WAIT}s"
    return
  fi
  out=$(judge_usage "$((t1 - t0))" "$size" "$TOL_PCT")
  dev=$(usage_dev "$((t1 - t0))" "$size")
  if [ -z "$out" ]; then
    ok "step2 住宅计数偏差 $dev%（面板 $t0→$t1 字节，已知 $size 字节，源 $DOWNLOAD_URL 报 $EXPECT_BYTES 字节，经 :$RESI_PORT，口径 tx+rx；spec §10 的目标是 ≤1%，本脚本门槛 ±$TOL_PCT%）"
  else
    no "step2 住宅计数超出 ±$TOL_PCT%（实测偏差 $dev%）" "$out（源 $DOWNLOAD_URL 报 $EXPECT_BYTES 字节，经 :$RESI_PORT）"
  fi
}

# $1=用户名 $2=下载前的基线 $3=最多等几秒 → 回显最终 usage.total。
# 「终值」= 涨过基线、且**连续 STABLE_WAIT 秒没再变**。只要 STABLE_WAIT ≥ 采样(10s)+落盘(30s)
# = 40s，就不可能在最后一份增量落盘之前收手：最后一份增量最晚在下载结束后 10s 被采样、
# 其后 ≤30s 必落盘（traffic.rs 的 SAMPLE_INTERVAL_SECS / FLUSH_INTERVAL_SECS），而「连续
# 40s 不变」本身要求收手时刻 ≥ 40s。一次 100MB 下载常常跨过一个落盘 tick、面板先落半截
# （比如 60MB），那个半截值会在下一轮被改写、stable 计数随之归零，不会被当成终值。
# 回显的是**判稳的那个** cur，不是再去读一次 —— 再读一次拿到的数没经过稳定判定，正好
# 可能是新一轮刚落的半截值。只有等满 $3（FLUSH_WAIT，兜底总上限）才退而读一次当时值。
wait_for_usage() {
  local rounds need i=0 same=0 cur="" prev=""
  rounds=$(awk -v m="$3" -v p="$USAGE_POLL" 'BEGIN {r = int(m / p); print (r < 1) ? 1 : r}')
  need=$(awk -v s="$STABLE_WAIT" -v p="$USAGE_POLL" \
    'BEGIN {r = int(s / p + 0.999999); print (r < 1) ? 1 : r}')
  while [ "$i" -lt "$rounds" ]; do
    cur=$(read_user_total "$1")
    if [ -n "$cur" ] && [ "$cur" != "$2" ] && [ "$cur" = "$prev" ]; then
      same=$((same + 1))
      if [ "$same" -ge "$need" ]; then
        printf '%s\n' "$cur"
        return 0
      fi
    else
      same=0
    fi
    prev=$cur
    sleep "$USAGE_POLL"
    i=$((i + 1))
  done
  read_user_total "$1"
}

# 判据 ③：到期语义（spec §6 —— **4.1 变了**）。
#
# 住宅：密码静态在 `hy2-residential.json` 的 `users[]` 里，到期 / 封禁只是把他那条凭据的
# 门 `gate-<id>` 切到 `deny` ⇒ **握手照旧成功**、每条流被拒。客户端那边显示「已连接」但
# 所有请求失败，`/connections` 里他归零（`interrupt_exist_connections: true` 连存量流一起
# 掐断）。同槽其他用户不受影响 —— 切的是单个 selector。
# **门切 `deny` 有延迟、不是到期即时事件**（rick 2026-09-18 实测 ~8–9 秒）：到期是时间点、
# 无 `StateChanged`，靠 `traffic::sampling_loop` 每 10 秒（`SAMPLE_INTERVAL_SECS`）那次
# `sync_now → gates::converge` 收敛，兜底才是 `sync_loop` 的 60 秒安全网。所以到期后要
# **轮询到稳定失败**（`probe_until_fail`，窗口 GATE_WAIT），别用「一看到 200 就 FAIL」抢跑。
# 直连：一个字没改，仍是 `auth_hook::decide` 在**握手**就拒（它自己比 `expires_at`，不等
# 快照重写），`auth-hook.log` 记 `expired` / `blocked`；既有连接要等采样轮（≤10s）算出
# newly_blocked 再 `POST /kick`，而 kick 只是标记，要等该用户下次有流量才真断（H12）。
#
# 于是这一步要开四条通路：住宅的「到期前既有连接」与「到期后新建连接」、直连的同两条，
# 外加一个**同槽邻居**的住宅连接（判「切别人的门不许动到我」）。
check_expiry() {
  local socks cfg pid rok=0 resi_pre=000 resi_post=000 zero=0 code probe hres out
  local dsocks dcfg dpid dlive=0 dok=0 dbad=0
  local nsocks="" ncfg="" npid="" nuser="" ntok="" nslot="" nrow="" nok=0 nconn0=0 nconn1=0 nprobe=000
  local bsocks bcfg bpid slot bwait
  if [ -z "$TMP_USER" ] || [ -z "$TMP_PASS" ]; then
    skip "step3 到期语义（没有临时用户）"
    return
  fi
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step3 到期语义（$BASE/bin/hysteria 不可执行）"
    return
  fi
  if ! load_resi_creds "$TMP_USER"; then
    skip "step3 到期语义（$TMP_USER 的 /api/nodes 里没有住宅 HY2 节点：住宅未启用 / 还没发到凭据）"
    return
  fi
  slot=$(read_sub_row "$TMP_USER" | sed -n 2p)

  # ① 住宅「到期前」的既有连接
  socks=$(free_port)
  cfg="$WORK/keep-resi.yaml"
  write_client_cfg "$cfg" "$RESI_NAME" "$RESI_SECRET" "$socks" "$RESI_PORT"
  pid=$(start_hy2_client "$cfg" "$WORK/keep-resi.client.log" "$socks")
  track_pid "$pid"
  read -r rok _ < <(probe_first_ok "$socks" "$FIRST_OK_WAIT")
  resi_pre=$(first_ok_code "$rok")

  # ② 直连「到期前」的既有连接（判据里要它被踢断）
  dsocks=$(free_port)
  dcfg="$WORK/keep-direct.yaml"
  write_client_cfg "$dcfg" "$TMP_USER" "$TMP_PASS" "$dsocks"
  dpid=$(start_hy2_client "$dcfg" "$WORK/keep-direct.client.log" "$dsocks")
  track_pid "$dpid"
  read -r dok dbad < <(probe_first_ok "$dsocks" "$FIRST_OK_WAIT")
  if [ "$dok" -gt 0 ]; then
    dlive=1
  else
    no "step3 到期前临时用户的直连节点就不通，「既有连接被踢」无从判定" \
      "失败 $dbad 次；客户端日志末行：$(tail -n 1 "$WORK/keep-direct.client.log" 2>/dev/null)"
  fi

  # ③ 同槽邻居的住宅连接
  nrow=$(resi_neighbour_from_api "$TMP_USER" "$slot")
  nuser=$(printf '%s\n' "$nrow" | sed -n 1p)
  ntok=$(printf '%s\n' "$nrow" | sed -n 2p)
  nslot=$(printf '%s\n' "$nrow" | sed -n 3p)
  if [ -n "$nuser" ] && [ -n "$ntok" ]; then
    nrow=$(resi_node_of "$ntok")
    nsocks=$(free_port)
    ncfg="$WORK/keep-neighbour-resi.yaml"
    write_client_cfg "$ncfg" "$(printf '%s\n' "$nrow" | sed -n 1p)" \
      "$(printf '%s\n' "$nrow" | sed -n 2p)" "$nsocks" "$(printf '%s\n' "$nrow" | sed -n 3p)"
    npid=$(start_hy2_client "$ncfg" "$WORK/keep-neighbour-resi.client.log" "$nsocks")
    track_pid "$npid"
    read -r nok _ < <(probe_first_ok "$nsocks" "$FIRST_OK_WAIT")
    nprobe=$(first_ok_code "$nok")
    nconn0=$(client_connects "$WORK/keep-neighbour-resi.client.log")
  else
    skip "step3 同槽邻居会话不断（没有第二个拿到住宅 HY2 门位的 blocked=false 用户）"
  fi

  code=$(expire_temp_user)
  if [ "$code" != "200" ]; then
    no "step3 设到期失败（PUT /api/users/$TMP_USER → HTTP $code）" "$(api_body)"
    stop_client "$pid"
    stop_client "$dpid"
    stop_client "$npid"
    return
  fi
  sleep $((EXPIRE_TTL + 1))
  if wait_user_blocked "$TMP_USER" "$BLOCK_WAIT"; then
    ok "step3 面板把到期用户判成 blocked（expiresAt=$(read_user_row "$TMP_USER" | sed -n 3p)）"
  else
    no "step3 面板没把到期用户判成 blocked" "limits.expiresAt=$(read_user_row "$TMP_USER" | sed -n 3p)"
  fi

  # 住宅新建连接：**握手仍该成功**（凭据没动），但请求要在门位收敛后稳定失败。门切 `deny`
  # 不是到期即时事件，靠 10 秒采样轮收敛（兜底 60 秒安全网，rick 实测 ~8–9 秒），所以这里给
  # 门位收敛窗口 GATE_WAIT（≥ 兜底路径）、轮询到稳定失败为止，别用「一看到 200 就 FAIL」抢跑。
  bsocks=$(free_port)
  bcfg="$WORK/relogin-resi.yaml"
  write_client_cfg "$bcfg" "$RESI_NAME" "$RESI_SECRET" "$bsocks" "$RESI_PORT"
  bpid=$(start_hy2_client "$bcfg" "$WORK/relogin-resi.client.log" "$bsocks")
  track_pid "$bpid"
  # 先等这条新连接**握手成功**（日志里出现 connected to server）再探请求是否稳定失败：
  # 客户端刚起、还没拨通那几秒探到的 000 不是「门切了 deny」而是「还没连上」，若门其实没切
  # （真漏），慢启动会让 probe_until_fail 在连上之前先凑满连续失败、把漏判成 PASS —— 误判到
  # 危险的那一侧。等到握手成功后，000 才唯一地意味着「握手成功但请求被拒」（4.1 到期语义）。
  bwait=0
  while [ "$bwait" -lt "$FIRST_OK_WAIT" ] &&
    [ "$(client_connects "$WORK/relogin-resi.client.log")" -eq 0 ]; do
    sleep 1
    bwait=$((bwait + 1))
  done
  resi_post=$(probe_until_fail "$bsocks" "$GATE_WAIT")
  if [ "$(client_connects "$WORK/relogin-resi.client.log")" -gt 0 ]; then
    ok "step3 到期用户的住宅节点仍握手成功（客户端日志有 connected to server）"
  else
    no "step3 到期用户的住宅节点连握手都没成功（4.1 的语义是握手成功、请求被拒）" \
      "客户端日志末行：$(tail -n 1 "$WORK/relogin-resi.client.log" 2>/dev/null)"
  fi
  stop_client "$bpid"

  # 直连新登录：凭据没变，只是人已到期 ⇒ 钩子必须在握手就拒（出不了网）
  local rsocks rcfg rpid
  rsocks=$(free_port)
  rcfg="$WORK/relogin-direct.yaml"
  write_client_cfg "$rcfg" "$TMP_USER" "$TMP_PASS" "$rsocks"
  rpid=$(start_hy2_client "$rcfg" "$WORK/relogin-direct.client.log" "$rsocks")
  track_pid "$rpid"
  probe=$(probe_socks_code "$rsocks" "$KEEP_URL")
  stop_client "$rpid"
  # 只看「探测失败」不够：服务端宕机 / 端口不通也失败。必须同时在钩子日志里看到这个人被
  # 判 expired|blocked，才算「鉴权错」。这两条只对**直连**有效 —— 住宅路径在
  # `auth-hook.log` 里不再有任何记录（sing-box 对 hysteria2 鉴权失败不打日志，spec §6）。
  hres=$(hook_last_result "$TMP_USER")
  if [ "$probe" = "200" ]; then
    no "step3 到期用户还能经直连新建连接出网（HTTP 200）" \
      "钩子日志末行：$(tail -n 1 "$BASE/auth-hook.log" 2>/dev/null)"
  elif [ "$hres" = "expired" ] || [ "$hres" = "blocked" ]; then
    ok "step3 到期用户直连新登录被拒（HTTP ${probe:-空}，钩子判定 $hres）"
  else
    no "step3 直连新登录没通，但钩子没记下鉴权拒绝（判定=${hres:-无}），只能算连不上" \
      "钩子日志末行：$(tail -n 1 "$BASE/auth-hook.log" 2>/dev/null)"
  fi

  if [ "$dlive" = "1" ]; then
    if wait_session_drop "$dsocks" "$KICK_WAIT"; then
      ok "step3 直连的既有连接在 ${KICK_WAIT}s 内被踢断（连续 3 次 curl 失败）"
    else
      no "step3 直连的既有连接 ${KICK_WAIT}s 内没断" \
        "最后一次探测仍是 HTTP $(probe_socks_code "$dsocks" "$KEEP_URL")"
    fi
  fi

  # 「归零」放在**最后**测，而且要先把直连那两条通路收干净：`/api/online` 是三个来源
  # 合并后的一张表（直连 `/online` 的会话数 + 住宅 `/connections` 的连接数，traffic.rs
  # 把它们归一成「这个人在线」），直连会话只要还挂着，他就一直在表里 —— 那时读到的非零
  # 与住宅的门切没切干净无关。此刻只剩住宅那条既有连接（①）还握着手、且没人往里打流量，
  # 于是表里任何残留都只能来自住宅的 `/connections`。
  stop_client "$dpid"
  dpid=""
  zero=$(wait_online_zero "$TMP_USER" "$KICK_WAIT")
  out=$(judge_expiry_41 "$resi_pre" "$resi_post" "$zero")
  case "$out" in
    PASS*) ok "step3 住宅到期语义：${out#PASS }" ;;
    *) no "step3 住宅到期语义不对" "${out#FAIL }" ;;
  esac

  if [ -n "$npid" ]; then
    nprobe=$(probe_socks_code "$nsocks" "$KEEP_URL")
    nconn1=$(client_connects "$WORK/keep-neighbour-resi.client.log")
    out=$(judge_neighbour "$nconn0" "$nconn1" "$nprobe")
    if [ -z "$out" ]; then
      ok "step3 邻居 $nuser（槽 ${nslot:-?}，本人槽 ${slot:-?}）的 connected 计数不变（$nconn1 次）且仍通"
    else
      no "step3 切到期用户的门动到了邻居 $nuser（槽 ${nslot:-?}）" "$out"
    fi
  fi

  stop_client "$pid"
  stop_client "$dpid"
  stop_client "$npid"
}

# 判据 ④：删一个上游后，任一用户三种订阅的 sha 不变、每个人的门位与期望一致、
# `hysteria-residential` 不重启（spec §6 的 m3 判据 ④）。
#
# 门位那一项只比「面板算出的期望门位」（`GET /api/users` 的 `hy2ResiGate`，与
# `panel::gates::expected` 同源）—— 真机上它几乎恒真，抓不到 Clash API 的 select 重放失败；
# 两件**活体**证据（哨兵事件 + 真打一次住宅节点）在 check_gate_live 里，**默认就跑**。
#
# **这一步会真删一条上游**，而上游的凭据删了这脚本取不回来 ⇒ 只在显式给了
# `--remove-upstream <sel>` 时才走，缺省整条 SKIP。
check_remove_upstream() {
  local tok sha0 sha1 gates before after out code
  if [ -z "$REMOVE_UPSTREAM" ]; then
    skip "step4 删上游后订阅 sha 与门位不变（没给 --remove-upstream <host:port|uuid|resi-N>；它不可逆，只在 staging 开）"
    return
  fi
  if [ -z "$TMP_USER" ]; then
    skip "step4 删上游后订阅 sha 与门位不变（没有临时用户）"
    return
  fi
  tok=$(read_sub_row "$TMP_USER" | sed -n 1p)
  if [ -z "$tok" ]; then
    skip "step4 删上游后订阅 sha 与门位不变（取不到 $TMP_USER 的订阅 token）"
    return
  fi
  sha0=$(sub_shas "$tok")
  before=$(unit_stamps "$RESI_UNIT")
  code=$(remove_upstream "$REMOVE_UPSTREAM")
  if [ "$code" != "200" ]; then
    no "step4 删上游失败（POST /api/residential/remove $REMOVE_UPSTREAM → HTTP $code）" "$(api_body)"
    return
  fi
  # 删上游改了槽位，门位重放最坏要等满那一轮 60 秒安全网（spec §3.4）
  sleep "$GATE_WAIT"
  sha1=$(sub_shas "$tok")
  out=$(judge_sub_sha "$sha0" "$sha1")
  if [ -z "$out" ]; then
    ok "step4 删上游 $REMOVE_UPSTREAM 后 $TMP_USER 的三种订阅 sha 全不变（$(printf '%s\n' "$sha1" | sed -n 1p | cut -c1-12)…）"
  else
    no "step4 删上游动了订阅（4.1 的核心承诺是不动）" "$out"
  fi

  gates=$(gate_rows)
  out=$(judge_gates "$gates")
  if [ -z "$out" ]; then
    ok "step4 每个住宅 HY2 用户的门位都与期望一致（$(printf '%s\n' "$gates" | grep -c .) 人）"
  else
    no "step4 删上游后有人的门位不对" "$out"
  fi

  after=$(unit_stamps "$RESI_UNIT")
  out=$(judge_stamps "$before" "$after")
  if [ -z "$out" ]; then
    ok "step4 删上游没重启 $RESI_UNIT"
  else
    no "step4 删上游把住宅内核重启了" "$out"
  fi
}

# 判据 ④'：门位的两件**活体**证据，**默认就跑**（两件都只读、没有副作用）：
#   ① `bui incidents` 里没有 `hy2_resi_gate_sync_failed` / `hy2_resi_gate_replay_failed`
#      （窗口 = `GATE_SINCE`，脚本加载时取的，必然盖住本次加用户 / 到期 / 删上游触发的
#      那几轮门位重放）；
#   ② 一个**未封**住宅 HY2 用户真连一次 `ports.hy2_resi` 还能出网。
# 这两件事原先锁在 `--remove-upstream` 后面 ⇒ 缺省整跑与 T19 正文那条命令都看不到它们，
# 而判据 ④ 的门位那半条只比期望值、真机上几乎恒真。所以这里独立成项：删上游那一步
# （不可逆）留在 check_remove_upstream，只读的这两件默认就跑。
#
# 顺序上跑在 check_remove_upstream 之后：给了 `--remove-upstream` 时，这两件证据正好落在
# 「删上游 + 门位重放收敛」之后量。
check_gate_live() {
  local out n nrow nuser ntok gname gsecret gport gsocks gcfg gpid gok=0
  out=$(judge_gate_incidents "$(gate_incident_rows "$GATE_SINCE")")
  if [ -z "$out" ]; then
    ok "step4' 没有门位同步/重放失败事件（bui incidents 自 epoch $GATE_SINCE 起）"
  else
    no "step4' 门位重放报了失败" "$out"
  fi

  # 活体探测的判别力前提：池有效（开关开着 + 池非空）。池无效 ⇒ 中继 fail-open、监听照在
  # 但出口全直连，探测无论门位对不对都 200，那就不是证据而是假 PASS ⇒ 明说并 SKIP。
  n=$(upstream_count)
  if [ -z "$n" ]; then
    skip "step4' 门位活体探测（读不到 GET /api/residential/status 的 enabled / upstreams ⇒ 判不出池还有没有效、探测有没有判别力）"
    return
  fi
  if [ "$n" = "0" ]; then
    skip "step4' 门位活体探测（池无效：池空或住宅开关关着 ⇒ 中继 fail-open 全部直连，无论门位对不对都回 200，这个探测没有判别力）"
    return
  fi
  nrow=$(resi_neighbour_from_api "$TMP_USER" "")
  nuser=$(printf '%s\n' "$nrow" | sed -n 1p)
  ntok=$(printf '%s\n' "$nrow" | sed -n 2p)
  if [ -z "$nuser" ] || [ -z "$ntok" ] || [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step4' 门位活体探测（没有第二个未封的住宅 HY2 用户 / 没有 $BASE/bin/hysteria）"
    return
  fi
  nrow=$(resi_node_of "$ntok")
  gname=$(printf '%s\n' "$nrow" | sed -n 1p)
  gsecret=$(printf '%s\n' "$nrow" | sed -n 2p)
  gport=$(printf '%s\n' "$nrow" | sed -n 3p)
  if [ -z "$gname" ] || [ -z "$gsecret" ] || [ -z "$gport" ]; then
    skip "step4' 门位活体探测（$nuser 的 /api/nodes 里没有住宅 HY2 节点）"
    return
  fi
  gsocks=$(free_port)
  gcfg="$WORK/gate-live.yaml"
  write_client_cfg "$gcfg" "$gname" "$gsecret" "$gsocks" "$gport"
  gpid=$(start_hy2_client "$gcfg" "$WORK/gate-live.client.log" "$gsocks")
  track_pid "$gpid"
  read -r gok _ < <(probe_first_ok "$gsocks" "$FIRST_OK_WAIT")
  stop_client "$gpid"
  if [ "$(first_ok_code "$gok")" = "200" ]; then
    ok "step4' 未封用户 $nuser 经 :$gport 仍真能出网（门位活体探测 200，池里 $n 条上游）"
  else
    no "step4' 未封用户 $nuser 经 :$gport 出不了网：门位可能还指着已删除的槽" \
      "客户端日志末行：$(tail -n 1 "$WORK/gate-live.client.log" 2>/dev/null)"
  fi
}

# 判据 ⑤：重启守护进程后计数不重复。
check_restart_count() {
  local t0 t1 out
  if [ -z "$TMP_USER" ]; then
    skip "step5 重启不重复计数（没有临时用户）"
    return
  fi
  t0=$(read_user_total "$TMP_USER")
  if [ -z "$t0" ]; then
    no "step5 重启前读不到 usage.total" "$(api_body | head -c 300)"
    return
  fi
  restart_daemon
  sleep "$RESTART_WAIT"
  if ! wait_api_up 30; then
    no "step5 重启 $DAEMON_UNIT 后面板没恢复应答" "$API/api/users 连不上"
    return
  fi
  t1=$(read_user_total "$TMP_USER")
  out=$(judge_restart_total "$t0" "$t1")
  if [ -z "$out" ]; then
    ok "step5 restart $DAEMON_UNIT 后 usage.total 没重复计（$t0 → $t1 字节）"
  else
    no "step5 重启后计数异常" "$out"
  fi
}

# 收尾：临时用户删干净
check_cleanup() {
  local code
  if [ -z "$TMP_USER" ]; then
    skip "step6 删除临时用户（没建成）"
    return
  fi
  code=$(api DELETE "/api/users/$TMP_USER")
  if [ "$code" = "200" ]; then
    ok "step6 临时用户 $TMP_USER 已删除"
    TMP_USER=""
  else
    no "step6 删不掉临时用户 $TMP_USER（HTTP $code）" \
      "$(api_body)；请手工 DELETE $API/api/users/$TMP_USER"
  fi
}

run_checks() {
  local pw
  [ "$(id -u)" = "0" ] || printf 'WARN  不是 root：systemctl 与 %s 下的文件可能读不到\n' "$BASE"
  load_node_params
  API="http://127.0.0.1:$ADMIN_PORT"
  PW_SRC=${ADMIN_PW_FILE:-$BASE/v3-backup/admin.env}
  pw=$(read_admin_password "$PW_SRC")
  if [ -z "$pw" ]; then
    printf 'FATAL 取不到管理员密码（%s）；用 --admin-password-file <文件> 指一份\n' "$PW_SRC"
    exit 2
  fi
  TOKEN=$(api_login "$pw")
  if [ -z "$TOKEN" ]; then
    printf 'FATAL 面板登录失败（%s/api/login，密码来源 %s）\n' "$API" "$PW_SRC"
    exit 2
  fi
  check_add_user
  check_traffic
  check_expiry
  check_remove_upstream
  check_gate_live
  check_restart_count
  check_cleanup
}

# ---------------------------------------------------------------------------
# 自测：fake systemctl / fake hysteria / python 面板桩 + 覆盖网络探测的那几个函数。
# 不出网、不碰 /opt、不需要 root；每条判据的 PASS 与 FAIL 分支都走一遍。
# ---------------------------------------------------------------------------

ST_DIR=""
ST_STUB_PID=""

st_write_panel_stub() {
  cat >"$ST_DIR/panel.py" <<'PY'
#!/usr/bin/env python3
"""M3 自测用的面板桩：只实现本脚本用到的那几个端点，状态放在一个 JSON 文件里
（自测直接改这个文件来摆局面）。只监听 127.0.0.1，不出网。

state 文件的键：password / token / users（/api/users 的投影数组）/
ramp（{用户名: 目标 usage.total}，每次 GET /api/users 把该用户的 total 抬到目标值，
模拟采样落盘）/ ramp_seq（{用户名: [总量, 总量, …]}，每次 GET 取走一个，模拟「先落半截、
再落全量」的多轮落盘）/ expire_marks_blocked（PUT 设到期后是否置 blocked）/
online（在线用户名列表，`GET /api/online` 按它出表，blocked 的人默认不出现）/
online_ignores_blocked（摆「门没切干净、他还在 /connections 里」）/
online_while_pid（{"file": PID 文件, "user": 用户名}：只要那个进程还活着，该用户就算在表里
——摆真机上「直连会话还挂着 ⇒ 合并表里非零」，归零必须在收掉直连通路之后才量）/
resi_node（`GET /api/nodes/<token>` 里那个住宅 HY2 节点的 {username,password,port}，
null = 没有住宅 HY2 节点）/ sub_bodies（三种订阅的响应体原文）/
remove_ok（`POST /api/residential/remove` 是否成功）/ remove_breaks_subs（删上游后把订阅
改掉，摆 4.1 最怕的那种回归）/ remove_breaks_gates（删上游后把未封用户的门位写成 deny）/
upstreams（`GET /api/residential/status` 的 `upstreams` 行数；`[]` = 池空，判据 ④' 的活体
探测据此 SKIP。null = 这个键整个缺席，摆「读不到上游数」）/ resi_enabled（那个端点的
`enabled`，即 `pool_active()`；false = 池里有上游但住宅开关关着，中继照样 fail-open）。
"""
import json
import os
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

STATE = os.environ["M3_STUB_STATE"]
PORT = int(os.environ["M3_STUB_PORT"])


def load():
    with open(STATE) as f:
        return json.load(f)


def save(d):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(d, f)
    os.replace(tmp, STATE)


def find(d, name):
    for u in d["users"]:
        if u["username"] == name:
            return u
    return None


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def reply(self, code, body):
        raw = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def text(self, code, body):
        raw = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def read_body(self):
        n = int(self.headers.get("Content-Length") or 0)
        try:
            return json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return {}

    def authed(self, d):
        return self.headers.get("Authorization") == "Bearer " + d["token"]

    def name_from_path(self):
        return self.path.rsplit("/", 1)[-1]

    def sub_owner(self, d, seg):
        """订阅 token → 用户（认不出一律 404，与真面板同口径）"""
        for u in d["users"]:
            if seg and u.get("subToken") == seg:
                return u
        return None

    def do_POST(self):
        d = load()
        if self.path == "/api/login":
            if self.read_body().get("password") == d["password"]:
                return self.reply(200, {"token": d["token"]})
            return self.reply(401, {"error": "Auth failed"})
        if not self.authed(d):
            return self.reply(401, {"error": "Unauthorized"})
        if self.path == "/api/users":
            name = self.read_body().get("username") or ""
            if not name or find(d, name):
                return self.reply(400, {"error": "bad username"})
            d["users"].append({
                "username": name, "protocol": "fusion",
                "usage": {"total": 0, "monthly": {}}, "limits": {},
                "password": "stub-hy2-password", "uuid": "00000000-0000-4000-8000-000000000000",
                "subToken": "0123456789abcdef0123456789abcdef",
                "residential": True, "slot": 0, "hy2ResiGate": "slot-0-out",
                "disabled": False, "blocked": False,
            })
            save(d)
            return self.reply(200, {"success": True, "user": name,
                                    "password": "stub-hy2-password",
                                    "uuid": "00000000-0000-4000-8000-000000000000"})
        if self.path == "/api/residential/remove":
            if not (self.read_body().get("id") or ""):
                return self.reply(400, {"error": "id 或 host_port 字段必填"})
            if not d.get("remove_ok", True):
                return self.reply(404, {"error": "Upstream not found"})
            if d.get("remove_breaks_subs"):
                b = d.get("sub_bodies") or {}
                b["sub"] = (b.get("sub") or "") + "-rebalanced"
                d["sub_bodies"] = b
            if d.get("remove_breaks_gates"):
                for u in d["users"]:
                    if u.get("hy2ResiGate"):
                        u["hy2ResiGate"] = "deny"
            save(d)
            return self.reply(200, {"success": True})
        return self.reply(404, {"error": "no route"})

    def do_GET(self):
        d = load()
        # 免鉴权的四个端点（路径末段就是凭据）先于鉴权处理，与真面板一样
        for pre, key in (("/api/sub/", "sub"), ("/api/subscription/", "subscription"),
                         ("/api/clash/", "clash")):
            if self.path.startswith(pre):
                if self.sub_owner(d, self.path[len(pre):]) is None:
                    return self.reply(404, {"error": "User not found"})
                return self.text(200, (d.get("sub_bodies") or {}).get(key) or "")
        if self.path.startswith("/api/nodes/"):
            u = self.sub_owner(d, self.path[len("/api/nodes/"):])
            if u is None:
                return self.reply(404, {"error": "User not found"})
            n = d.get("resi_node")
            nodes = []
            if n:
                nodes.append({
                    "kind": "hy2_residential", "label": "HY2住宅",
                    "host": "panel.example.com", "port": n["port"],
                    "hop": [41000, 50000],
                    "transport": {"type": "hysteria2", "username": n["username"],
                                  "password": n["password"], "sni": "panel.example.com",
                                  "obfs_password": "obfs-pw"},
                })
            return self.reply(200, {"user": u["username"], "split": {}, "nodes": nodes})
        if not self.authed(d):
            return self.reply(401, {"error": "Unauthorized"})
        if self.path == "/api/residential/status":
            # 真面板回的是一大张 StatusResponse；本脚本只读 `enabled` 与 `upstreams` 行数
            u = d.get("upstreams")
            on = d.get("resi_enabled", True)
            if u is None:
                return self.reply(200, {"enabled": on})
            return self.reply(200, {"enabled": on, "upstreams": u})
        if self.path == "/api/online":
            out = {}
            hold = d.get("online_while_pid") or {}
            holding = False
            if hold.get("file"):
                try:
                    with open(hold["file"]) as f:
                        os.kill(int(f.read().strip()), 0)
                    holding = True
                except Exception:
                    holding = False
            for name in d.get("online") or []:
                u = find(d, name)
                if u is None:
                    continue
                if (u.get("blocked") and not d.get("online_ignores_blocked")
                        and not (holding and hold.get("user") == name)):
                    continue
                out[name] = 1
            return self.reply(200, out)
        if self.path == "/api/users":
            dirty = False
            for name, target in (d.get("ramp") or {}).items():
                u = find(d, name)
                if u and u["usage"]["total"] < target:
                    u["usage"]["total"] = target
                    dirty = True
            for name, seq in (d.get("ramp_seq") or {}).items():
                u = find(d, name)
                if u and seq:
                    u["usage"]["total"] = seq.pop(0)
                    dirty = True
            if dirty:
                save(d)
            return self.reply(200, d["users"])
        return self.reply(404, {"error": "no route"})

    def do_PUT(self):
        d = load()
        if not self.authed(d):
            return self.reply(401, {"error": "Unauthorized"})
        u = find(d, self.name_from_path())
        if u is None:
            return self.reply(404, {"error": "User not found"})
        days = self.read_body().get("days")
        if days and float(days) > 0:
            u["limits"]["expiresAt"] = time.strftime(
                "%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + float(days) * 86400))
            u["blocked"] = bool(d.get("expire_marks_blocked", True))
        save(d)
        return self.reply(200, {"success": True, "user": u["username"]})

    def do_DELETE(self):
        d = load()
        if not self.authed(d):
            return self.reply(401, {"error": "Unauthorized"})
        before = len(d["users"])
        d["users"] = [u for u in d["users"] if u["username"] != self.name_from_path()]
        if len(d["users"]) == before:
            return self.reply(404, {"error": "User not found"})
        save(d)
        return self.reply(200, {"success": True})


HTTPServer(("127.0.0.1", PORT), H).serve_forever()
PY
}

st_write_systemctl() {
  mkdir -p "$ST_DIR/bin"
  cat >"$ST_DIR/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
# 只实现本脚本用到的三种调用：show -p NRestarts|MainPID --value <unit>、restart、is-active。
# M3_FAKE_BUMP=1 时每次 show 都把计数与 PID 往上加 —— 摆「加用户把内核重启了」的局面。
case "${1:-}" in
  restart) exit 0 ;;
  is-active) printf 'active\n'; exit 0 ;;
esac
prop=""
getnext=0
for a in "$@"; do
  if [ "$getnext" = 1 ]; then
    prop=$a
    getnext=0
    continue
  fi
  [ "$a" = "-p" ] && getnext=1
done
n=$(cat "$M3_FAKE_NR" 2>/dev/null || printf '0')
p=$(cat "$M3_FAKE_PID" 2>/dev/null || printf '4242')
if [ "${M3_FAKE_BUMP:-0}" = 1 ]; then
  printf '%s\n' "$((n + 1))" >"$M3_FAKE_NR"
  printf '%s\n' "$((p + 1))" >"$M3_FAKE_PID"
fi
case "$prop" in
  NRestarts) printf '%s\n' "$n" ;;
  MainPID) printf '%s\n' "$p" ;;
  *) printf '\n' ;;
esac
STUB
  chmod 755 "$ST_DIR/bin/systemctl"
  printf '0\n' >"$ST_DIR/nr"
  printf '4242\n' >"$ST_DIR/pid"
  M3_FAKE_NR="$ST_DIR/nr"
  M3_FAKE_PID="$ST_DIR/pid"
  export M3_FAKE_NR M3_FAKE_PID
}

# 自测里那个临时用户在 `GET /api/users` 里的投影（住宅那几档齐了，判据 ②③④⑤ 都用它）。
# $1 = usage.total。订阅 token 用合成值（公开仓库不写真 token）。
ST_SUB_TOKEN="0123456789abcdef0123456789abcdef"
st_tmp_user_json() {
  printf '[{"username": "m3-selftest", "protocol": "fusion", "password": "stub-hy2-password",
  "usage": {"total": %s, "monthly": {}}, "limits": {}, "disabled": false, "blocked": false,
  "residential": true, "slot": 0, "hy2ResiGate": "slot-0-out", "subToken": "%s"}]' \
    "$1" "$ST_SUB_TOKEN"
}

# `bui incidents --json` 的桩回包：每个入参是一条事件的签名（不给 = 干净）。
# 时刻写 `@NOW@`（由 bui 桩换成当前时刻）⇒ 落在事件窗口内；要摆「窗口之前的旧事件」
# 就传 `<签名>@<时刻>`。
st_incidents() {
  local s sig at out=""
  for s in "$@"; do
    sig=${s%%@*}
    at="@NOW@"
    [ "$sig" = "$s" ] || at=${s#*@}
    out="$out${out:+,}{\"at\":\"$at\",\"unit\":\"b-ui\",\"signature\":\"$sig\","
    out="$out\"subject\":\"gate-r000\",\"action\":\"replay\","
    out="$out\"result\":\"PUT /proxies/gate-r000 失败\",\"level\":\"error\"}"
  done
  printf '{"incidents":[%s],"source":"daemon"}\n' "$out" >"$ST_DIR/base/incidents.json"
}

# stub 的状态文件：$1=键 $2=JSON 值
st_stub_set() {
  python3 - "$ST_DIR/stub.json" "$1" "$2" <<'PY'
import json, sys
p, k, v = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.load(open(p))
d[k] = json.loads(v)
json.dump(d, open(p, "w"))
PY
}

st_stub_reset() {
  cat >"$ST_DIR/stub.json" <<'JSON'
{"password": "stub-admin-pw", "token": "stub-token", "users": [], "ramp": {},
 "ramp_seq": {}, "expire_marks_blocked": true, "online": ["m3-selftest"],
 "resi_node": {"username": "r000", "password": "s3cret-r000", "port": 40000},
 "upstreams": [{"id": "u1"}, {"id": "u2"}],
 "sub_bodies": {"sub": "c3R1Yi1zdWI=", "subscription": "{\"outbounds\":[]}", "clash": "proxies: []"}}
JSON
}

st_setup() {
  ST_DIR=$(mktemp -d) || {
    printf 'FATAL 建不出自测临时目录\n'
    exit 2
  }
  chmod 700 "$ST_DIR"
  st_write_systemctl
  PATH="$ST_DIR/bin:$PATH"
  export PATH
  # fake 的 bundled hysteria：只要「存在且可执行」，真正的连通性在自测里由
  # start_hy2_client / probe_socks_code / download_via_socks 三个覆盖函数决定。
  printf '#!/bin/sh\nexit 0\n' >"$ST_DIR/bin/hysteria"
  chmod 755 "$ST_DIR/bin/hysteria"
  mkdir -p "$ST_DIR/base/bin"
  cp "$ST_DIR/bin/hysteria" "$ST_DIR/base/bin/hysteria"
  chmod 755 "$ST_DIR/base/bin/hysteria"
  # 判据 ④ 的活体证据①读 `$BASE/bin/bui incidents --json`：桩只实现这一条子命令，回包原文
  # 放在 `$BASE/incidents.json`（各用例用 st_incidents 摆），`@NOW@` 由桩换成当前时刻 ——
  # 事件窗口的起点是 check_remove_upstream 运行期才取的，写死时刻会被窗口过滤掉。
  cat >"$ST_DIR/base/bin/bui" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  *incidents*) sed "s/@NOW@/$(date -u +%Y-%m-%dT%H:%M:%SZ)/g" "$(dirname "$0")/../incidents.json" ;;
  *) printf 'unsupported: %s\n' "$*" >&2; exit 2 ;;
esac
STUB
  chmod 755 "$ST_DIR/base/bin/bui"
  st_incidents
  printf '2026-09-12T09:00:00Z 1.2.3.4:51820 alice allow\n' >"$ST_DIR/base/auth-hook.log"
  # 判据 ① 要比它的 sha：真机上是渲染出来的住宅 sing-box 配置，自测只要「有这个文件」
  printf '{"inbounds":[{"type":"hysteria2","tag":"hy2-resi"}]}\n' >"$ST_DIR/base/hy2-residential.json"
  BASE="$ST_DIR/base"

  # 面板桩
  st_stub_reset
  M3_STUB_STATE="$ST_DIR/stub.json"
  M3_STUB_PORT=$(free_port)
  export M3_STUB_STATE M3_STUB_PORT
  st_write_panel_stub
  python3 "$ST_DIR/panel.py" >"$ST_DIR/panel.log" 2>&1 &
  ST_STUB_PID=$!

  # 假 state.json：本脚本只从它读 node（域名 / 端口 / obfs）—— 用户是从面板 API 挑的。
  # admin 端口就写成面板桩的端口，load_node_params 之后 API 直接指向它。
  cat >"$ST_DIR/base/state.json" <<JSON
{"node": {"domain": "panel.example.com",
          "ports": {"hy2": 10000, "hy2_resi": 40000, "reality_direct": 10001,
                    "reality_resi": 10002, "admin": $M3_STUB_PORT},
          "obfs": {"enabled": true, "password": "obfs-pw"}}}
JSON
  load_node_params
  API="http://127.0.0.1:$ADMIN_PORT"
  local i=0
  while [ "$i" -lt 60 ]; do
    (exec 3<>"/dev/tcp/127.0.0.1/$M3_STUB_PORT") 2>/dev/null && break
    sleep 0.1
    i=$((i + 1))
  done

  # 把各等待窗口压到秒级，自测才跑得快
  ADD_WAIT=1
  FLUSH_WAIT=2
  # need = ceil(STABLE_WAIT / USAGE_POLL) = 2 轮：够验「半截值不算终值」，又不拖时间
  STABLE_WAIT=0.4
  KICK_WAIT=5
  RESTART_WAIT=1
  KEEP_INTERVAL=0.2
  USAGE_POLL=0.2
  EXPIRE_TTL=1
  FIRST_OK_WAIT=3
  BLOCK_WAIT=2
  # 判据 ③ 住宅那半改用 probe_until_fail（窗口取 GATE_WAIT），到期没生效那条 FAIL 用例会一直
  # 探到 200、轮询满整个窗口才回 200 ⇒ 自测里也得把它压到秒级（默认 75s 会拖满一分多钟）。
  GATE_WAIT=2

  # 自测不出网：HEAD 探活整体桩成「首个候选就 200 + 100MB」。要摆 403 / 全挂的场景，
  # 各用例在自己的子外壳里再覆盖一次。
  probe_download_head() { printf '200 104857600\n'; }
}

st_teardown() {
  TMP_USER=""
  if [ -n "$ST_STUB_PID" ]; then
    kill "$ST_STUB_PID" 2>/dev/null
    wait "$ST_STUB_PID" 2>/dev/null
  fi
  [ -n "$ST_DIR" ] && rm -rf "$ST_DIR"
}

# 自测里替换真客户端与真探测：客户端换成一个活着的 sleep，探测看面板桩里的 blocked 位
# （到期后就打不通，正是真机上的形态）。
# shellcheck disable=SC2317  # 这些覆盖在 $( ) 子外壳里定义，下面那次 check_* 调用会用到
st_fake_client() {
  # 起一个活着的进程冒充客户端。stdout 必须重定向掉：否则它会一直占着
  # `pid=$(start_hy2_client …)` 那个命令替换的管道写端，命令替换要等它退出才返回。
  local pid
  sleep 30 >/dev/null 2>&1 &
  pid=$!
  # 直连那条既有连接的 PID 落盘：面板桩的 `online_while_pid` 档据此摆「直连会话还挂着 ⇒
  # 他还在 /api/online 表里」。真机上 /api/online 是三来源合并表，所以「归零」必须在收掉
  # 直连通路之后才量 —— 先量后收会读到与住宅门位无关的非零（作者自述的那个坑）。
  case "${1:-}" in
    *keep-direct.yaml) printf '%s\n' "$pid" >"$ST_DIR/direct.pid" ;;
  esac
  echo "$pid"
}

# 与 start_hy2_client 同签名（$1=cfg $2=日志 $3=socks 端口）的两种客户端：
# 握手成功的（日志里有 `connected to server`，4.1 住宅到期后就是这个形态）与握手就被拒的。
st_fake_client_connected() {
  # 日志**截断**（真 start_hy2_client 也是 `>"$log"`）：$WORK 整趟自测共用，不截断就跨用例累积
  printf '2026-09-17T09:00:00Z INFO connected to server {"addr": "127.0.0.1:40000"}\n' >"$2"
  st_fake_client "$@"
}

st_fake_client_rejected() {
  printf '2026-09-17T09:00:00Z FATAL authentication failed\n' >"$2"
  st_fake_client "$@"
}

# $WORK 与钩子日志是整趟自测共用的：每个判据 ③ 用例开跑前把它们复位 ——
#   - 钩子日志恢复成只有一行基线，否则上一个用例落的 `expired` 会被下一个用例的
#     hook_last_result 当成本轮证据；
#   - 上一轮留下的客户端配置要删掉，否则 st_probe_by_cfg 可能按一份陈旧的
#     `listen: 127.0.0.1:<端口>` 认错通路（free_port 偶尔会把端口发回来）。
st_reset_probe_fixtures() {
  printf '2026-09-12T09:00:00Z 1.2.3.4:51820 alice allow\n' >"$BASE/auth-hook.log"
  rm -f "$WORK"/keep-*.yaml "$WORK"/relogin-*.yaml "$ST_DIR/direct.pid"
}

# 判据 ③ 里同时有五条通路（住宅 / 直连 × 既有 / 新建，外加邻居），探测桩得分得清哪条是
# 哪条。端口是运行期才定的，所以按「这个 socks 端口写在哪份客户端配置里」认 —— 不去碰
# check_expiry 的局部变量名。邻居那份恒 200（切别人的门不该动到他）；
# ST_NB_RECONNECT=1 时顺带往邻居日志里多落一行 connected，摆「邻居被踢重连了」。
st_probe_by_cfg() {
  local f
  f=$(grep -l "listen: 127.0.0.1:${1:-0}\$" "$WORK"/*.yaml 2>/dev/null | head -1)
  case "$f" in
    *keep-neighbour-resi.yaml)
      [ "${ST_NB_RECONNECT:-0}" = "1" ] &&
        printf '2026-09-17T09:00:01Z INFO connected to server {"addr": "127.0.0.1:40000"}\n' \
          >>"${f%.yaml}.client.log"
      echo 200
      ;;
    *) st_probe_by_blocked ;;
  esac
}

# 判据 ③ 的邻居：第二个拿到住宅 HY2 门位的用户（同槽）
st_users_with_neighbour() {
  printf '%s' "$(st_tmp_user_json "$1")" |
    python3 -c 'import json, sys
d = json.load(sys.stdin)
n = dict(d[0])
n.update({"username": "bob", "subToken": "fedcba9876543210fedcba9876543210",
          "usage": {"total": 0, "monthly": {}}})
d.append(n)
print(json.dumps(d))'
}

st_probe_by_blocked() {
  local b
  b=$(python3 - "$ST_DIR/stub.json" "$TMP_USER" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
for u in d["users"]:
    if u["username"] == sys.argv[2]:
        print("1" if u.get("blocked") else "0")
        break
PY
  )
  # 真机上每次新建连接都过 auth 钩子、落一行 `<ts> <addr> <用户名> <结果>`；自测照样落，
  # 判据 ③ 才分得清「被鉴权拒」和「连不上」。ST_HOOK_LOG=0 专门摆「探测失败但钩子没记」。
  if [ "$b" = "1" ]; then
    [ "${ST_HOOK_LOG:-1}" = "1" ] &&
      printf '2026-09-12T09:00:01Z 1.2.3.4:51821 %s expired\n' "$TMP_USER" >>"$BASE/auth-hook.log"
    echo 000
  else
    [ "${ST_HOOK_LOG:-1}" = "1" ] &&
      printf '2026-09-12T09:00:01Z 1.2.3.4:51821 %s allow\n' "$TMP_USER" >>"$BASE/auth-hook.log"
    echo 200
  fi
}

# state.json / admin.env 的解析 + 客户端配置的落盘
st_state_parsing() {
  local out cfg
  if [ "$SNI" = "panel.example.com" ] && [ "$HY2_PORT" = "10000" ] &&
    [ "$ADMIN_PORT" = "$M3_STUB_PORT" ] && [ "$OBFS_ON" = "1" ] && [ "$OBFS_PW" = "obfs-pw" ]; then
    ok "自测：state.json 解析出域名 / HY2 端口 / 面板端口 / obfs"
  else
    no "自测：state.json 解析不对" "$SNI / $HY2_PORT / $ADMIN_PORT / $OBFS_ON / $OBFS_PW"
  fi
  printf 'ADMIN_PASSWORD=p@ss:with=equals\nADMIN_PORT=8080\n' >"$ST_DIR/admin.env"
  assert_pw() { # $1=期望 $2=文件 $3=标签
    if [ "$(read_admin_password "$2")" = "$1" ]; then ok "$3"; else no "$3" "$(read_admin_password "$2")"; fi
  }
  assert_pw 'p@ss:with=equals' "$ST_DIR/admin.env" "自测：admin.env 取 ADMIN_PASSWORD=（含 = 与 : 不截断）"
  printf 'bare-password\n' >"$ST_DIR/bare.txt"
  assert_pw 'bare-password' "$ST_DIR/bare.txt" "自测：--admin-password-file 也接受整行密码"
  assert_pw '' "$ST_DIR/not-there" "自测：密码文件不存在 ⇒ 空（上层 FATAL）"

  cfg="$ST_DIR/probe.yaml"
  write_client_cfg "$cfg" alice 'pw:with:colons' 1080 "$HY2_PORT"
  out=$(cat "$cfg")
  if [[ "$out" == *'auth: "alice:pw:with:colons"'* && "$out" == *'server: 127.0.0.1:10000'* &&
    "$out" == *'listen: 127.0.0.1:1080'* && "$out" == *'type: salamander'* &&
    "$out" == *'password: obfs-pw'* ]] && [ "$(stat -c %a "$cfg")" = "600" ]; then
    ok "自测：客户端配置 0600、带 obfs、密码里的冒号不歪"
  else
    no "自测：客户端配置不对" "$out（权限 $(stat -c %a "$cfg")）"
  fi
}

st_units_and_judges() {
  local out
  out=$(unit_stamps)
  if [ "$(printf '%s\n' "$out" | wc -l)" = "3" ] &&
    [ "$(printf '%s\n' "$out" | awk '$1 == "xray" {print $2 " " $3}')" = "0 4242" ]; then
    ok "自测：unit_stamps 读到三个单元的 NRestarts/MainPID"
  else
    no "自测：unit_stamps 结果不对" "$out"
  fi
  out=$(judge_stamps "xray 0 9" "xray 0 9")
  if [ -z "$out" ]; then ok "自测：没动过的单元判通过"; else no "自测：没动过却被判失败" "$out"; fi
  out=$(judge_stamps "xray 0 9" "xray 1 10")
  if [[ "$out" == *"NRestarts 0→1"* && "$out" == *"MainPID 9→10"* ]]; then
    ok "自测：NRestarts/MainPID 变了都报出"
  else
    no "自测：漏报内核重启" "$out"
  fi
  out=$(judge_stamps "xray 0 9" "")
  if [[ "$out" == *取不到* ]]; then ok "自测：取不到 NRestarts 判失败"; else no "自测：空值被当成通过" "$out"; fi

  out=$(judge_session 5 0 12 0)
  if [ -z "$out" ]; then ok "自测：成功数涨、失败数不涨 ⇒ 会话没断"; else no "自测：会话误判为断" "$out"; fi
  out=$(judge_session 5 0 7 3)
  if [[ "$out" == *"失败 3 次"* ]]; then ok "自测：窗口内出现失败 ⇒ 判断连"; else no "自测：漏判断连" "$out"; fi
  out=$(judge_session 5 0 5 0)
  if [[ "$out" == *没涨* ]]; then ok "自测：成功数不涨也判失败"; else no "自测：保活死了没被发现" "$out"; fi

  out=$(judge_usage 104857600 104857600 5)
  if [ -z "$out" ]; then ok "自测：零偏差判通过"; else no "自测：零偏差被误判" "$out"; fi
  out=$(judge_usage 108000000 104857600 5)
  if [ -z "$out" ]; then ok "自测：+3% 在容差内"; else no "自测：+3% 被误判" "$out"; fi
  out=$(judge_usage 209715200 104857600 5)
  if [[ "$out" == *"+100.00%"* ]]; then ok "自测：翻倍（+100%）判失败"; else no "自测：翻倍没被判失败" "$out"; fi
  out=$(judge_usage 52428800 104857600 5)
  if [[ "$out" == *"-50.00%"* ]]; then ok "自测：少算一半判失败"; else no "自测：少算一半没被判失败" "$out"; fi
  out=$(judge_usage 0 0 5)
  if [[ "$out" == *已知流量为\ 0* ]]; then ok "自测：什么都没下到判失败"; else no "自测：空下载被当成通过" "$out"; fi

  out=$(judge_restart_total 104857600 104857600)
  if [ -z "$out" ]; then ok "自测：重启后计数不变判通过"; else no "自测：不变却被误判" "$out"; fi
  out=$(judge_restart_total 104857600 209715200)
  if [[ "$out" == *异常增长* ]]; then ok "自测：重启后翻倍判失败"; else no "自测：翻倍没被判失败" "$out"; fi
  out=$(judge_restart_total 104857600 104000000)
  if [[ "$out" == *变小* ]]; then ok "自测：重启后变小判失败"; else no "自测：计数丢了没被判失败" "$out"; fi
  out=$(judge_restart_total "" 1)
  if [[ "$out" == *读不到* ]]; then ok "自测：读不到 usage.total 判失败"; else no "自测：空值被当成通过" "$out"; fi

  # 判据①：内核单元回固定三项（4.1 的住宅只有一个进程了，不再按槽枚举）
  out=$KERNEL_UNITS
  if [ "$out" = "hysteria-server hysteria-residential xray" ]; then
    ok "自测：内核单元固定三项"
  else
    no "自测：内核单元清单不对" "$out"
  fi
  # 判据④只看住宅那一个单元 ⇒ unit_stamps 要认参数
  out=$(unit_stamps "$RESI_UNIT")
  if [ "$out" = "hysteria-residential 0 4242" ]; then
    ok "自测：unit_stamps 认单元清单参数（判据④ 只看住宅那一个）"
  else
    no "自测：unit_stamps 的参数没生效" "$out"
  fi

  # 判据①：hy2-residential.json 的 sha（spec §3.5）
  out=$(judge_cfg_sha aaa aaa)
  if [ -z "$out" ]; then ok "自测：hy2-residential.json 不变判通过"; else no "自测：不变却被误判" "$out"; fi
  out=$(judge_cfg_sha aaa bbb)
  if [[ "$out" == *变了* ]]; then ok "自测：hy2-residential.json 变了判失败"; else no "自测：配置被改了没判失败" "$out"; fi
  out=$(judge_cfg_sha "" aaa)
  if [[ "$out" == *读不到* ]]; then ok "自测：读不到配置 sha 判失败"; else no "自测：空 sha 被当成通过" "$out"; fi

  # 判据③：到期语义变了 —— **新握手仍成功**，但请求全失败
  out=$(judge_expiry_41 "200" "000" "3") # 到期前 200 / 到期后 000 / 该用户连接数归零
  case "$out" in PASS*) ok "自测：判据③（握手成功但流被拒）PASS" ;; *) no "自测：判据③ 通过分支不对" "$out" ;; esac
  out=$(judge_expiry_41 "200" "200" "3") # 到期后还能出网 ⇒ 必须 FAIL
  case "$out" in FAIL*) ok "自测：判据③（到期后还能出网）FAIL" ;; *) no "自测：判据③ 失败分支不对" "$out" ;; esac
  out=$(judge_expiry_41 "200" "000" "0") # 流被拒但 /connections 没归零 ⇒ FAIL（门没切干净）
  case "$out" in FAIL*) ok "自测：判据③（连接没归零）FAIL" ;; *) no "自测：判据③ 连接数分支不对" "$out" ;; esac
  out=$(judge_expiry_41 "000" "000" "3") # 到期前就不通 ⇒ 无从判定，也是 FAIL
  case "$out" in FAIL*到期前*) ok "自测：判据③（到期前就不通）FAIL" ;; *) no "自测：判据③ 前置分支不对" "$out" ;; esac

  # 判据③ 的第三项：同槽邻居不受影响
  out=$(judge_neighbour 1 1 200)
  if [ -z "$out" ]; then ok "自测：邻居没重连且仍通 ⇒ 通过"; else no "自测：邻居被误判" "$out"; fi
  out=$(judge_neighbour 1 2 200)
  if [[ "$out" == *重连* ]]; then ok "自测：邻居 connected 计数涨了判失败"; else no "自测：漏判邻居重连" "$out"; fi
  out=$(judge_neighbour 1 1 000)
  if [[ "$out" == *不通* ]]; then ok "自测：邻居窗口后不通判失败"; else no "自测：漏判邻居掉线" "$out"; fi
  out=$(judge_neighbour 0 0 200)
  if [[ "$out" == *没建起来* ]]; then ok "自测：邻居压根没连上判失败"; else no "自测：空邻居被当成通过" "$out"; fi

  # 判据④：三种订阅的 sha 与门位
  out=$(judge_sub_sha "a
b
c" "a
b
c")
  if [ -z "$out" ]; then ok "自测：三种订阅 sha 都不变 ⇒ 通过"; else no "自测：sha 不变却被误判" "$out"; fi
  out=$(judge_sub_sha "a
b
c" "a
b
z")
  if [[ "$out" == */api/clash* ]]; then ok "自测：只有 clash 订阅变了也判失败"; else no "自测：漏判订阅变化" "$out"; fi
  out=$(judge_sub_sha "a
b
c" "a
b")
  if [[ "$out" == *取不到* ]]; then ok "自测：订阅 sha 取不到判失败"; else no "自测：空 sha 被当成通过" "$out"; fi
  out=$(judge_gates "alice 0 slot-0-out 0
bob 2 slot-2-out 0
carol 1 deny 1")
  if [ -z "$out" ]; then ok "自测：门位与期望一致 ⇒ 通过"; else no "自测：门位被误判" "$out"; fi
  out=$(judge_gates "alice 0 slot-1-out 0")
  if [[ "$out" == *"期望 slot-0-out"* ]]; then ok "自测：门位指到别的槽判失败"; else no "自测：漏判门位错槽" "$out"; fi
  out=$(judge_gates "alice 0 deny 0")
  if [[ "$out" == *deny* ]]; then ok "自测：未封用户的门停在 deny 判失败"; else no "自测：漏判误封" "$out"; fi
  out=$(judge_gates "alice 0 未分配 0")
  if [[ "$out" == *未分配* ]]; then ok "自测：门位「未分配」判失败"; else no "自测：未分配被当成通过" "$out"; fi
  out=$(judge_gates "")
  if [[ "$out" == *没有任何住宅* ]]; then ok "自测：一个住宅用户都没有时不算通过"; else no "自测：空门位表被当成通过" "$out"; fi

  # 判据④ 的活体证据①：门位同步/重放失败事件（门位那半条比的是期望值，抓不到这个）
  out=$(judge_gate_incidents "")
  if [ -z "$out" ]; then ok "自测：没有门位失败事件 ⇒ 通过"; else no "自测：干净的事件表被误判" "$out"; fi
  out=$(judge_gate_incidents "hy2_resi_gate_replay_failed gate-r000 PUT /proxies 失败")
  if [[ "$out" == *"1 条"* && "$out" == *hy2_resi_gate_replay_failed* ]]; then
    ok "自测：有门位重放失败事件判失败"
  else
    no "自测：漏判门位重放失败" "$out"
  fi
  out=$(judge_gate_incidents "!unreadable")
  if [[ "$out" == *无从核对* ]]; then
    ok "自测：读不到 bui incidents 判失败（不当成「干净」）"
  else
    no "自测：读不到事件表被当成通过" "$out"
  fi
}

st_login_and_crud() {
  local t code
  t=$(api_login stub-admin-pw)
  if [ -n "$t" ]; then ok "自测：密码对 ⇒ 拿到 token（密码经 0600 文件，不进 argv）"; else no "自测：登录拿不到 token" "$(cat "$ST_DIR/panel.log" 2>/dev/null | tail -n 3)"; fi
  if [ -z "$(api_login wrong-pw)" ]; then ok "自测：密码错 ⇒ token 为空"; else no "自测：错密码也拿到了 token"; fi
  TOKEN=$t
  code=$(api GET /api/users)
  if [ "$code" = "200" ] && [ "$(api_body)" = "[]" ]; then
    ok "自测：带 token 读 /api/users（JWT 经 -K - 的 stdin，不进 argv）"
  else
    no "自测：读 /api/users 不对" "HTTP $code：$(api_body)"
  fi
  code=$(
    TOKEN=bogus
    api GET /api/users
  )
  if [ "$code" = "401" ]; then ok "自测：token 不对 ⇒ 401"; else no "自测：错 token 没被拒" "HTTP $code"; fi
}

# 邻居用户从 `GET /api/users` 挑：blocked 这一位把 disabled / 到期 / 配额用尽都盖住了。
# 得排在 st_login_and_crud 之后：没 TOKEN 时 /api/users 是 401，挑出来的必然是空。
st_neighbour() {
  local out
  st_stub_set users '[
    {"username": "ghost", "protocol": "fusion", "password": "x", "disabled": true, "blocked": true},
    {"username": "over-quota", "protocol": "fusion", "password": "y", "disabled": false, "blocked": true},
    {"username": "reality-only", "protocol": "vless-reality", "password": "z", "disabled": false, "blocked": false},
    {"username": "nopass", "protocol": "hysteria2", "password": "", "disabled": false, "blocked": false},
    {"username": "alice", "protocol": "fusion", "password": "pw:with:colons", "disabled": false, "blocked": false}]'
  out=$(neighbour_from_api "")
  if [ "$out" = "alice
pw:with:colons" ]; then
    ok "自测：邻居用户跳过 disabled / 配额用尽(blocked) / 只有 Reality / 没密码四种人"
  else
    no "自测：邻居用户挑错了" "$out"
  fi
  if [ -z "$(neighbour_from_api alice)" ]; then
    ok "自测：排除临时用户后没有可用的邻居 ⇒ 空（上层 SKIP）"
  else
    no "自测：排除参数没生效" "$(neighbour_from_api alice)"
  fi
  # 判据 ③ 的住宅邻居：要有门位、有订阅 token，且**同槽优先**
  st_stub_set users '[
    {"username": "no-gate", "slot": 0, "subToken": "00000000000000000000000000000001", "disabled": false, "blocked": false},
    {"username": "denied", "slot": 0, "hy2ResiGate": "deny", "subToken": "00000000000000000000000000000002", "disabled": false, "blocked": true},
    {"username": "other-slot", "slot": 3, "hy2ResiGate": "slot-3-out", "subToken": "00000000000000000000000000000003", "disabled": false, "blocked": false},
    {"username": "same-slot", "slot": 0, "hy2ResiGate": "slot-0-out", "subToken": "00000000000000000000000000000004", "disabled": false, "blocked": false}]'
  out=$(resi_neighbour_from_api m3-selftest 0)
  if [ "$out" = "same-slot
00000000000000000000000000000004
0" ]; then
    ok "自测：住宅邻居跳过没门位 / 被封 的人，同槽的优先"
  else
    no "自测：住宅邻居挑错了" "$out"
  fi
  out=$(resi_neighbour_from_api same-slot 0 | sed -n 1p)
  if [ "$out" = "other-slot" ]; then
    ok "自测：同槽没人时退回任意住宅 HY2 用户（槽序号照实回显）"
  else
    no "自测：住宅邻居的回退不对" "$out"
  fi
  st_stub_reset
}

st_check_add_user() {
  local out
  # ① 通过：内核没动、保活一路 ok
  out=$(
    st_stub_set users '[{"username": "alice", "protocol": "fusion", "password": "pw:with:colons", "disabled": false, "blocked": false}]'
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { echo 200; }
    check_add_user
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] &&
    [[ "$out" == *"hy2-residential.json 字节不变"* ]]; then
    ok "自测：判据①（内核没动 + 配置字节不变 + 会话不断）三条 PASS"
  else
    no "自测：判据① 通过分支不对" "$out"
  fi
  # 桩里已经建出临时用户；换个名字继续下一轮
  st_stub_reset
  # ② 失败：systemctl 每次读都自增（=内核被重启了）+ 保活全失败
  out=$(
    export M3_FAKE_BUMP=1
    st_stub_set users '[{"username": "alice", "protocol": "fusion", "password": "pw:with:colons", "disabled": false, "blocked": false}]'
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { echo 000; }
    check_add_user
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "2" ] &&
    [[ "$out" == *NRestarts* && "$out" == *建不起来* ]]; then
    ok "自测：判据①（内核被重启 + 会话建不起来）两条 FAIL"
  else
    no "自测：判据① 失败分支不对" "$out"
  fi
  st_stub_reset
  # ③ 失败：建号动了 hy2-residential.json（spec §3.5 的红线）
  out=$(
    st_stub_set users '[{"username": "alice", "protocol": "fusion", "password": "pw:with:colons", "disabled": false, "blocked": false}]'
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { echo 200; }
    # 摆「建号顺手重写了住宅配置」：与真 create_temp_user 同形，只多改那个文件
    create_temp_user() {
      local f="$WORK/create.json" code
      printf '{"inbounds":[{"type":"hysteria2","tag":"hy2-resi","grown":true}]}\n' >"$BASE/hy2-residential.json"
      (
        umask 077
        printf '{"username":"%s","protocol":"fusion","residential":true}\n' "$1" >"$f"
      )
      code=$(api POST /api/users "$f")
      rm -f "$f"
      printf '%s' "$code"
    }
    check_add_user
  )
  if [[ "$out" == *"加用户动了 hy2-residential.json"* ]]; then
    ok "自测：判据①（建号改了 hy2-residential.json）FAIL"
  else
    no "自测：配置被建号改了却没判失败" "$out"
  fi
  printf '{"inbounds":[{"type":"hysteria2","tag":"hy2-resi"}]}\n' >"$BASE/hy2-residential.json"
  st_stub_reset
}

st_check_traffic() {
  local out
  # ① 通过：下满 100MB，面板随后涨 100MB。顺带把**下载走的是哪条通路**钉死：4.1 要测的是
  # 住宅那一路（端口 = ports.hy2_resi、认证 = 凭据池的 `{name}:{secret}`，都来自
  # `/api/nodes`），拿直连的 `username:hy2_password` 顶等于换了被测对象。
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client; }
    download_via_socks() {
      st_stub_set ramp "{\"$TMP_USER\": 105000000}"
      printf '104857600 200\n'
    }
    check_traffic
    grep -E '^(server|auth):' "$WORK/dl.yaml"
  )
  if [[ "$out" == PASS*偏差* ]]; then ok "自测：判据②（100MB 计到 ~100MB）PASS"; else no "自测：判据② 通过分支不对" "$out"; fi
  if [[ "$out" == *'server: 127.0.0.1:40000'* && "$out" == *'auth: "r000:s3cret-r000"'* ]]; then
    ok "自测：判据② 的下载走住宅节点（:40000 + 池凭据 name:secret）"
  else
    no "自测：判据② 的下载没走住宅那一路" "$out"
  fi
  st_stub_reset
  # ② 失败：只计到一半
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client; }
    download_via_socks() {
      st_stub_set ramp "{\"$TMP_USER\": 52428800}"
      printf '104857600 200\n'
    }
    check_traffic
  )
  if [[ "$out" == FAIL*超出* ]]; then ok "自测：判据②（少算一半）FAIL"; else no "自测：判据② 失败分支不对" "$out"; fi
  st_stub_reset
  # ③ 失败：根本没下满
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client; }
    download_via_socks() { printf '1024 000\n'; }
    check_traffic
  )
  if [[ "$out" == *没下满* ]]; then ok "自测：判据②（没下满已知字节数）FAIL"; else no "自测：没下满没被判失败" "$out"; fi
  st_stub_reset
  # ③' 失败：所有候选下载源都探不活 ⇒ 判据②直接 FAIL，文案说明「没有可用的下载源」
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    DOWNLOAD_URL=""
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client; }
    probe_download_head() { printf '403 -\n'; }
    download_via_socks() { printf '0 000\n'; }
    check_traffic
  )
  if [[ "$out" == FAIL*没有可用的下载源* && "$out" == *tele2*hetzner*ovh* ]]; then
    ok "自测：判据②（候选下载源全挂 ⇒ 无法测量）FAIL"
  else
    no "自测：下载源全挂没被判失败 / 文案没说明" "$out"
  fi
  st_stub_reset
  # ④ 通过：一次下载跨过落盘 tick ⇒ 面板先落半截、再落全量（面板桩的 ramp_seq 每次
  # GET /api/users 取走一格，正是「一个轮询一格」）。终值必须等到全量那一格。
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client; }
    download_via_socks() {
      st_stub_set ramp_seq "{\"$TMP_USER\": [52428800, 52428800, 105000000]}"
      printf '104857600 200\n'
    }
    check_traffic
  )
  if [[ "$out" == PASS*偏差* ]]; then
    ok "自测：判据②（先落半截、再落全量 ⇒ 等到全量才判）PASS"
  else
    no "自测：半截落盘被当成了终值" "$out"
  fi
  st_stub_reset
  # 上面这一场景确实抓得住回归：把 STABLE_WAIT 压回一个轮询间隔（= 修这条 blocking 之前
  # 的「连续两次读到同一个数就收手」），同一条 ramp_seq 的终值就停在半截值 52428800 上。
  out=$(
    STABLE_WAIT=$USAGE_POLL
    st_stub_set users "$(st_tmp_user_json 0)"
    st_stub_set ramp_seq '{"m3-selftest": [52428800, 52428800, 105000000]}'
    wait_for_usage m3-selftest 0 "$FLUSH_WAIT"
  )
  if [ "$out" = "52428800" ]; then
    ok "自测：STABLE_WAIT 只有一个轮询间隔时半截值会被当终值 ⇒ 该场景有判别力"
  else
    no "自测：半截场景没判别力（压小 STABLE_WAIT 仍拿到终值）" "$out"
  fi
  st_stub_reset
  # ⑤ 没有住宅 HY2 节点（`/api/nodes` 形状漂移 / 凭据池还没发到凭据）⇒ 整条 SKIP。
  # 判据 ②③ 共用 load_resi_creds 这一条**静默**路径：它一跳，两条最关键的判据一起消失，
  # 所以 SKIP 必须计数进摘要（`--strict` 下还要算成失败），否则整跑是假绿。
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "$(st_tmp_user_json 0)"
    st_stub_set resi_node null
    start_hy2_client() { st_fake_client "$@"; }
    download_via_socks() { printf '104857600 200\n'; }
    check_traffic
  )
  if [[ "$out" == SKIP*住宅\ HY2\ 节点* ]]; then
    ok "自测：判据②（/api/nodes 里没有住宅 HY2 节点）SKIP"
  else
    no "自测：拿不到住宅凭据却没 SKIP" "$out"
  fi
  st_stub_reset
}

# 下载源候选列表 + HEAD 探活：回退顺序、已知字节数取实报 Content-Length、显式指定只探它。
st_download_source() {
  local out
  # ① 第一个源 403（真机 cloudflare 的形态）⇒ 回退到第二个，已知字节数取它报的长度
  out=$(
    DOWNLOAD_URL=""
    probe_download_head() {
      case "$1" in
        *tele2*) printf '403 -\n' ;;
        *hetzner*) printf '200 104857600\n' ;;
        *) printf '404 -\n' ;;
      esac
    }
    resolve_download_url && printf '%s %s\n' "$DOWNLOAD_URL" "$EXPECT_BYTES"
  )
  if [ "$out" = "https://speed.hetzner.de/100MB.bin 104857600" ]; then
    ok "自测：第一个源 403 ⇒ 回退到第二个候选"
  else
    no "自测：403 没回退到第二个候选" "$out"
  fi
  # ② 前两个挂（403 + 200 但没报长度）⇒ 用第三个，已知字节数就取它实报的（这里刻意不是 100MB）
  out=$(
    DOWNLOAD_URL=""
    probe_download_head() {
      case "$1" in
        *tele2*) printf '403 -\n' ;;
        *hetzner*) printf '200 -\n' ;;
        *) printf '200 13107200\n' ;;
      esac
    }
    resolve_download_url && printf '%s %s\n' "$DOWNLOAD_URL" "$EXPECT_BYTES"
  )
  if [ "$out" = "https://proof.ovh.net/files/100Mb.dat 13107200" ]; then
    ok "自测：200 但 Content-Length 未知的源被跳过，已知字节数取实报值（非 104857600）"
  else
    no "自测：未知长度的源没被跳过 / 字节数没取实报值" "$out"
  fi
  # ③ 显式 --download-url 只探它自己（不回退到候选列表）
  out=$(
    DOWNLOAD_URL="http://example.invalid/blob.bin"
    probe_download_head() {
      case "$1" in
        *example.invalid*) printf '200 4096\n' ;;
        *) printf '200 104857600\n' ;;
      esac
    }
    resolve_download_url && printf '%s %s\n' "$DOWNLOAD_URL" "$EXPECT_BYTES"
  )
  if [ "$out" = "http://example.invalid/blob.bin 4096" ]; then
    ok "自测：显式 --download-url 只探自己并取它的 Content-Length"
  else
    no "自测：显式 --download-url 没被优先用" "$out"
  fi
  # ④ 全挂 ⇒ 回非 0，失败原因逐条留在 PROBE_DETAIL
  out=$(
    DOWNLOAD_URL=""
    probe_download_head() { printf '403 -\n'; }
    resolve_download_url
    printf '%s|%s\n' "$?" "$PROBE_DETAIL"
  )
  if [[ "$out" == 1\|*tele2*hetzner*ovh* ]]; then
    ok "自测：候选全挂时探活回非 0 并列出每个源的状态码"
  else
    no "自测：全挂的返回值 / 失败明细不对" "$out"
  fi
}

st_check_expiry() {
  local out
  # ① 通过（4.1 的语义）：PUT 之后桩把用户置 blocked ⇒ 面板判 blocked；住宅那一路**握手
  # 仍成功**（客户端日志有 connected to server）但请求全 000、`/api/online` 里他消失；
  # 直连那一路新登录被拒 + 钩子记 expired + 既有连接被踢；邻居不受影响。
  # `online_while_pid` 顺带钉住**归零的测量顺序**：直连那条既有连接的进程只要还活着，他就
  # 一直在 `/api/online` 的合并表里 ⇒ 只有「先收直连、后量归零」才判得过（先量后收会假 FAIL）。
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    st_stub_set users "$(st_users_with_neighbour 1)"
    st_stub_set online_while_pid "{\"file\": \"$ST_DIR/direct.pid\", \"user\": \"m3-selftest\"}"
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_cfg "$@"; }
    check_expiry
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "6" ] &&
    [[ "$out" == *仍握手成功* && "$out" == *"住宅到期语义"* && "$out" == *直连新登录被拒* &&
    "$out" == *被踢断* && "$out" == *"connected 计数不变"* ]]; then
    ok "自测：判据③（判 blocked + 住宅握手成功但流被拒 + 邻居不受影响 + 直连握手即拒 + 直连既有连接被踢）PASS"
  else
    no "自测：判据③ 通过分支不对" "$out"
  fi
  st_stub_reset
  # ①' 失败：切到期用户的门把同槽邻居也踢重连了（`interrupt_exist_connections` 只该作用于
  # 那一个 selector —— 漏到别人身上就是全槽用户陪着断一次）
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    export ST_NB_RECONNECT=1
    st_stub_set users "$(st_users_with_neighbour 1)"
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_cfg "$@"; }
    check_expiry
  )
  if [[ "$out" == *"动到了邻居"* && "$out" == *重连* ]]; then
    ok "自测：判据③（邻居被踢重连）FAIL"
  else
    no "自测：邻居重连没被判失败" "$out"
  fi
  st_stub_reset
  # ② 失败：到期不生效（blocked 一直是 false）⇒ 住宅还能出网、直连还能新登录、都不断
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    st_stub_set users "$(st_tmp_user_json 1)"
    st_stub_set expire_marks_blocked false
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "4" ] &&
    [[ "$out" == *"住宅到期语义不对"* && "$out" == *"还能经直连新建连接出网"* && "$out" == *没断* ]]; then
    ok "自测：判据③（到期没生效：住宅仍出网 + 直连仍放行）FAIL"
  else
    no "自测：判据③ 失败分支不对" "$out"
  fi
  st_stub_reset
  # ③ 失败：请求确实被拒了，但 `/api/online` 里他还在 ⇒ 门没切干净（4.1 特有的失败形态）
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    st_stub_set users "$(st_tmp_user_json 1)"
    st_stub_set online_ignores_blocked true
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [[ "$out" == *没归零* ]]; then
    ok "自测：判据③（流被拒但 /api/online 没归零）FAIL"
  else
    no "自测：门没切干净却被判通过" "$out"
  fi
  st_stub_reset
  # ④ 失败：住宅那一路连握手都没成功（= 4.1 之前的形态，或者内核把人踢在了握手）
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    st_stub_set users "$(st_tmp_user_json 1)"
    start_hy2_client() { st_fake_client_rejected "$@"; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [[ "$out" == *"连握手都没成功"* ]]; then
    ok "自测：判据③（住宅握手被拒）FAIL"
  else
    no "自测：住宅握手失败没被判出来" "$out"
  fi
  st_stub_reset
  # ⑤ 失败：直连新登录确实没通，但钩子日志里没有这一位的鉴权拒绝（= 服务端宕了 / 端口不通）
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    export ST_HOOK_LOG=0
    st_stub_set users "$(st_tmp_user_json 1)"
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [[ "$out" == *"钩子没记下鉴权拒绝"* ]]; then
    ok "自测：判据③（直连连不上但钩子没判拒）不算通过"
  else
    no "自测：没区分「被鉴权拒」和「连不上」" "$out"
  fi
  st_stub_reset
  # ⑥ 没有住宅 HY2 节点（住宅未启用 / 还没发到凭据）⇒ 整条 SKIP，不假装通过
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_reset_probe_fixtures
    st_stub_set users "$(st_tmp_user_json 1)"
    st_stub_set resi_node null
    start_hy2_client() { st_fake_client_connected "$@"; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [[ "$out" == SKIP*住宅\ HY2\ 节点* ]]; then
    ok "自测：没有住宅 HY2 节点时判据③ SKIP"
  else
    no "自测：没有住宅节点却没 SKIP" "$out"
  fi
  st_stub_reset
}

# 归零测量的三个辅助（复核发现：这三处改坏了原先没有任何断言会红）。它们决定判据 ③
# 第三项能不能失败：`online_count` 必须 fail-closed（读不到就不算归零）、`wait_online_zero`
# 必须连续 ONLINE_ZERO_ROUNDS 轮才认、`probe_until_fail` 必须轮询到连续 RESI_FAIL_ROUNDS 次
# 失败才算门已切（收敛前的 200 不许当结论），到窗口末仍收不敛才回 200（FAIL）。
st_online_and_probe() {
  local out
  st_stub_set users "$(st_tmp_user_json 0)"
  st_stub_set online '["m3-selftest"]'
  out=$(online_count m3-selftest)
  if [ "$out" = "1" ]; then ok "自测：online_count 读到在表里的用户"; else no "自测：online_count 读错了" "$out"; fi
  out=$(online_count nobody)
  if [ "$out" = "0" ]; then ok "自测：不在 /api/online 表里 ⇒ 0"; else no "自测：不在表里没回 0" "$out"; fi
  # fail-closed：`/api/online` 非 200（这里用错 token 摆 401）必须**回显空**，绝不能当成归零
  out=$(
    TOKEN=bogus
    online_count m3-selftest
  )
  if [ -z "$out" ]; then
    ok "自测：/api/online 非 200 时 online_count 回显空（fail-closed）"
  else
    no "自测：读不到 /api/online 却回了数 ⇒ 判据③ 第三项恒真" "$out"
  fi
  st_stub_set online '[]'
  out=$(wait_online_zero m3-selftest 3)
  if [ "$out" = "$ONLINE_ZERO_ROUNDS" ]; then
    ok "自测：wait_online_zero 确认满 $ONLINE_ZERO_ROUNDS 轮"
  else
    no "自测：wait_online_zero 的确认轮数不对" "$out"
  fi
  st_stub_set online '["m3-selftest"]'
  out=$(wait_online_zero m3-selftest 1)
  if [ "$out" = "0" ]; then ok "自测：一直在表里 ⇒ wait_online_zero 回 0"; else no "自测：没归零却回了非 0" "$out"; fi
  # 只读到一次 0 随后读不到 ⇒ 仍回 0（「连续 N 轮确认」不等于「读到一次 0」）
  out=$(
    printf '0\n' >"$ST_DIR/online.seq"
    online_count() {
      sed -n 1p "$ST_DIR/online.seq"
      tail -n +2 "$ST_DIR/online.seq" >"$ST_DIR/online.seq.tmp"
      mv "$ST_DIR/online.seq.tmp" "$ST_DIR/online.seq"
    }
    wait_online_zero m3-selftest 1
  )
  if [ "$out" = "0" ]; then
    ok "自测：只读到一次 0 ⇒ 不算归零（连续 $ONLINE_ZERO_ROUNDS 轮的确认没被作废）"
  else
    no "自测：一次 0 就被当成归零" "$out"
  fi
  # probe_until_fail（判据③ 住宅到期，取代旧的「一看到 200 就 FAIL」做法）：门切 deny 有 ~10 秒采样轮延迟
  # （rick 实测），所以要**轮询到连续 RESI_FAIL_ROUNDS 次失败**才算 PASS——收敛中途探到 200 是
  # 正常态，别抢跑判 FAIL；到窗口末仍能 200 / 一直抖不收敛才回 200（FAIL）。
  # A. 到期后先 200 两轮、随后稳定失败 ⇒ 回失败码（PASS）。KEEP_INTERVAL=0 让 seq 一次跑完。
  out=$(
    KEEP_INTERVAL=0
    printf '200\n200\n000\n000\n000\n' >"$ST_DIR/probe.seq"
    probe_socks_code() {
      sed -n 1p "$ST_DIR/probe.seq"
      tail -n +2 "$ST_DIR/probe.seq" >"$ST_DIR/probe.seq.tmp"
      mv "$ST_DIR/probe.seq.tmp" "$ST_DIR/probe.seq"
    }
    probe_until_fail 1080 1080
  )
  if [ "$out" = "000" ]; then
    ok "自测：probe_until_fail 收敛前的 200 不误判、连续失败后回失败码（PASS）"
  else
    no "自测：probe_until_fail 把收敛中途的 200 当成了结论（退回一看到 200 就 FAIL 的抢跑）" "$out"
  fi
  # B. 到窗口末一直 200（门根本没切）⇒ 回 200（FAIL）。
  out=$(
    KEEP_INTERVAL=1
    probe_socks_code() { echo 200; }
    probe_until_fail 1080 1
  )
  if [ "$out" = "200" ]; then ok "自测：probe_until_fail 门没切（一直 200）到窗口末回 200（FAIL）"; else no "自测：门没切却没判 FAIL" "$out"; fi
  # C. 一直抖（000/200 交替、凑不满连续 RESI_FAIL_ROUNDS 次失败）⇒ 回 200（FAIL）。
  #    钉住「连续 N 次」：换成「见一次失败就回」会在这里回 000（假 PASS）。
  out=$(
    KEEP_INTERVAL=1
    printf '0\n' >"$ST_DIR/flap.n"
    probe_socks_code() {
      local n
      n=$(cat "$ST_DIR/flap.n")
      echo $((n + 1)) >"$ST_DIR/flap.n"
      if [ $((n % 2)) -eq 0 ]; then echo 000; else echo 200; fi
    }
    probe_until_fail 1080 2
  )
  if [ "$out" = "200" ]; then ok "自测：probe_until_fail 凑不满连续失败（一直抖）判 FAIL"; else no "自测：抖动被当成稳定失败（连续 N 次判据丢了）" "$out"; fi
  # D. 稳定失败时回显**最后一次的码**（403 而不是笼统 000），保留诊断信息。
  out=$(
    KEEP_INTERVAL=0
    printf '403\n403\n403\n' >"$ST_DIR/probe.seq"
    probe_socks_code() {
      sed -n 1p "$ST_DIR/probe.seq"
      tail -n +2 "$ST_DIR/probe.seq" >"$ST_DIR/probe.seq.tmp"
      mv "$ST_DIR/probe.seq.tmp" "$ST_DIR/probe.seq"
    }
    probe_until_fail 1080 1080
  )
  if [ "$out" = "403" ]; then ok "自测：probe_until_fail 稳定失败时回最后一次的码（403）"; else no "自测：probe_until_fail 的失败码不对" "$out"; fi
  st_stub_reset
}

# 摘要与退出码：SKIP 必须计数（判据 ②③ 共用一条静默跳过路径），`--strict` 把它算成失败。
st_summary_counts() {
  local out
  out=$(
    skipped=0
    skip "x" >/dev/null
    skip "y" >/dev/null
    printf '%s' "$skipped"
  )
  if [ "$out" = "2" ]; then ok "自测：skip 计数（SKIP 进摘要）"; else no "自测：SKIP 没被计数" "$out"; fi
  out=$(
    STRICT=0
    exit_code 0 2
  )
  if [ "$out" = "0" ]; then ok "自测：缺省退出码只算 FAIL"; else no "自测：缺省退出码不对" "$out"; fi
  out=$(
    STRICT=1
    exit_code 1 2
  )
  if [ "$out" = "3" ]; then ok "自测：--strict 把 SKIP 也算进退出码"; else no "自测：--strict 的退出码不对" "$out"; fi
  out=$(
    STRICT=1
    exit_code 200 100
  )
  if [ "$out" = "255" ]; then ok "自测：退出码封顶 255"; else no "自测：退出码没封顶" "$out"; fi
}

st_check_remove_upstream() {
  local out
  # ① 没给 --remove-upstream ⇒ 整条 SKIP（这一步不可逆，不许默认执行）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    check_remove_upstream
  )
  if [[ "$out" == SKIP*remove-upstream* ]]; then
    ok "自测：不给 --remove-upstream ⇒ 判据④ SKIP"
  else
    no "自测：判据④ 的 SKIP 分支不对" "$out"
  fi
  # ② 通过：删上游后三种订阅 sha 不变、门位与期望一致、住宅单元没重启（三条）。
  # 两件活体证据（哨兵事件 + 活体探测）在 st_check_gate_live 里，**不**在这一步
  # （它们只读、默认就跑，不该跟着这个不可逆开关走）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM="203.0.113.10:10007"
    GATE_WAIT=0.2
    st_stub_set users "$(st_users_with_neighbour 0)"
    check_remove_upstream
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] &&
    [[ "$out" == *"三种订阅 sha 全不变"* && "$out" == *门位都与期望一致* &&
    "$out" == *没重启* ]]; then
    ok "自测：判据④（订阅 sha 不变 + 门位对 + 不重启）PASS"
  else
    no "自测：判据④ 通过分支不对" "$out"
  fi
  st_stub_reset
  # ③ 失败：删上游把订阅改了（4.0.x 按槽切跳跃段就是这个形态 —— 全员必须刷新订阅）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM="203.0.113.10:10007"
    GATE_WAIT=0.2
    st_stub_set users "$(st_tmp_user_json 0)"
    st_stub_set remove_breaks_subs true
    check_remove_upstream
  )
  if [[ "$out" == *"/api/sub 的 sha 变了"* ]]; then
    ok "自测：判据④（删上游动了订阅）FAIL"
  else
    no "自测：订阅被改了没判失败" "$out"
  fi
  st_stub_reset
  # ④ 失败：门位没重放对（未封用户的门停在 deny ⇒ 他握手成功但全程不通）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM="203.0.113.10:10007"
    GATE_WAIT=0.2
    st_stub_set users "$(st_tmp_user_json 0)"
    st_stub_set remove_breaks_gates true
    check_remove_upstream
  )
  if [[ "$out" == *"门位不对"* ]]; then
    ok "自测：判据④（门位没重放对）FAIL"
  else
    no "自测：门位错了没判失败" "$out"
  fi
  st_stub_reset
  # ⑤ 失败：删上游本身失败（选择子不认）⇒ 报 HTTP 码，不接着比 sha
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM="203.0.113.10:10007"
    GATE_WAIT=0.2
    st_stub_set users "$(st_tmp_user_json 0)"
    st_stub_set remove_ok false
    check_remove_upstream
  )
  if [[ "$out" == FAIL*删上游失败*404* ]]; then
    ok "自测：判据④（删上游被面板拒）FAIL"
  else
    no "自测：删上游失败没被判出来" "$out"
  fi
  st_stub_reset
}

# 判据 ④'：两件活体证据**默认就跑**（每个用例都把 REMOVE_UPSTREAM 摆空 —— 这一条要是
# 又被锁回那个不可逆开关后面，整块就红）。
st_check_gate_live() {
  local out
  # ① 通过：不给 --remove-upstream 也照跑，两项都过（事件干净 + 活体探测 200）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "2" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "0" ] &&
    [[ "$out" == *没有门位同步/重放失败事件* && "$out" == *门位活体探测\ 200* ]]; then
    ok "自测：判据④'（不给 --remove-upstream 也跑：事件干净 + 活体探测通）PASS"
  else
    no "自测：判据④' 没在缺省整跑里跑起来（T19 正文那条命令就看不到这两件事）" "$out"
  fi
  st_stub_reset
  # ② 失败：哨兵报了门位同步 / 重放失败 —— 面板的 `hy2ResiGate` 是期望值，
  # `PUT /proxies/gate-<id>` 失败在那一侧看不出来（spec §3.4 / §5.7）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_incidents hy2_resi_gate_sync_failed hy2_resi_gate_replay_failed
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"门位重放报了失败"* && "$out" == *2\ 条* &&
    "$out" == *hy2_resi_gate_replay_failed* ]]; then
    ok "自测：判据④'（哨兵报了门位同步/重放失败）FAIL"
  else
    no "自测：门位重放失败事件没被判出来" "$out"
  fi
  st_incidents
  st_stub_reset
  # ③ 通过：窗口**之前**的旧门位失败事件不算这一笔（池里早先的失败不该记在本次头上）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_incidents 'hy2_resi_gate_replay_failed@2026-01-01T00:00:00Z'
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *没有门位同步/重放失败事件* ]]; then
    ok "自测：判据④' 只看脚本开跑之后的门位事件（窗口前的旧事件不算）"
  else
    no "自测：窗口前的旧事件把判据④' 判红了" "$out"
  fi
  st_incidents
  st_stub_reset
  # ④ 失败：事件窗口的**方向**。GATE_SINCE 是脚本加载时取的 ⇒ 必须盖住开跑之后落的一切
  # 门位失败事件。这一格**不覆盖 GATE_SINCE**，专门钉「起点被挪到动作之后」那种变异
  # （挪到 `sleep "$GATE_WAIT"` 之后 / 挪进 check_gate_live 里）：事件时刻取
  # `GATE_SINCE + 1`（开跑之后、此刻之前），起点一挪晚这条就被过滤掉、判据 ④' 恒绿。
  while [ "$(($(date +%s) - GATE_SINCE))" -lt 2 ]; do sleep 0.5; done
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_incidents "hy2_resi_gate_replay_failed@$(date -u -d "@$((GATE_SINCE + 1))" +%Y-%m-%dT%H:%M:%SZ)"
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"门位重放报了失败"* ]]; then
    ok "自测：判据④' 的事件窗口起点早于脚本的一切动作（起点取晚了就漏掉这条）"
  else
    no "自测：窗口起点被挪晚了 —— 开跑后落的门位失败事件没被判出来" "$out"
  fi
  st_incidents
  st_stub_reset
  # ④' 同一个方向，但走完整次序（删上游 → 等门位收敛 → 才量证据）：起点被挪进删上游那一步
  # （尤其挪到 `sleep "$GATE_WAIT"` 之后）时，正要盯的那一段失败事件同样会被过滤掉。
  # GATE_WAIT 给 2 秒 ⇒ 挪晚后的起点必然大于事件时刻（`date` 只到秒）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM="203.0.113.10:10007"
    GATE_WAIT=2
    st_incidents "hy2_resi_gate_replay_failed@$(date -u -d "@$((GATE_SINCE + 1))" +%Y-%m-%dT%H:%M:%SZ)"
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_remove_upstream >/dev/null
    check_gate_live
  )
  if [[ "$out" == *"门位重放报了失败"* ]]; then
    ok "自测：窗口起点不许挪到删上游/门位收敛之后（挪了这条就漏）"
  else
    no "自测：起点挪到删上游那一段之后 —— 门位重放失败事件被窗口过滤掉了" "$out"
  fi
  st_incidents
  st_stub_reset
  # ⑤ 失败：`bui incidents --json` 读不出来（守护进程没在跑 / 回包不是 JSON）⇒ 无从核对，
  # 不许当成「干净」
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    printf 'bui: 连不上 /run/b-ui.sock\n' >"$BASE/incidents.json"
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *无从核对* ]]; then
    ok "自测：判据④'（读不到 bui incidents）FAIL"
  else
    no "自测：读不到事件表却没判失败" "$out"
  fi
  st_incidents
  st_stub_reset
  # ⑥ 失败：事件干净，但未封用户经住宅节点**真打一次打不通**（门位还指着已删除的槽
  # 出站就是这个形态）
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_users_with_neighbour 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 000; }
    check_gate_live
  )
  if [[ "$out" == *"出不了网：门位可能还指着已删除的槽"* ]]; then
    ok "自测：判据④'（未封用户的活体探测不通）FAIL"
  else
    no "自测：活体探测不通却没判失败" "$out"
  fi
  st_stub_reset
  # ⑦ 池空 ⇒ 活体探测没有判别力（中继 fail-open 全部直连，门位对不对都回 200）⇒ SKIP。
  # 这正是「把池里最后一条上游删掉」之后的局面：探测回 200 是假 PASS
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_users_with_neighbour 0)"
    st_stub_set upstreams '[]'
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"SKIP  step4' 门位活体探测（池无效"* && "$out" != *活体探测\ 200* ]]; then
    ok "自测：池空 ⇒ 门位活体探测 SKIP（不拿 fail-open 的 200 当证据）"
  else
    no "自测：池空了还拿活体探测的 200 当通过" "$out"
  fi
  st_stub_reset
  # ⑦' 池里有上游、但住宅开关关着（`status.enabled` = `pool_active()` 为假）⇒ 同样是
  # fail-open 全直连，同样没有判别力：光数 `upstreams` 行数会漏掉这一种
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_users_with_neighbour 0)"
    st_stub_set resi_enabled false
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"SKIP  step4' 门位活体探测（池无效"* && "$out" != *活体探测\ 200* ]]; then
    ok "自测：住宅开关关着（池无效）⇒ 门位活体探测 SKIP"
  else
    no "自测：池无效（开关关着）却拿活体探测的 200 当通过" "$out"
  fi
  st_stub_reset
  # ⑧ 读不到上游数（`/api/residential/status` 没有 `upstreams`）⇒ 同样 SKIP：
  # 判不出探测有没有判别力，就不能拿它的 200 当证据
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_users_with_neighbour 0)"
    st_stub_set upstreams null
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"读不到 GET /api/residential/status"* && "$out" != *活体探测\ 200* ]]; then
    ok "自测：读不到上游数 ⇒ 门位活体探测 SKIP"
  else
    no "自测：读不到上游数却照样拿探测结果当证据" "$out"
  fi
  st_stub_reset
  # ⑨ 只有一个住宅 HY2 用户（没有第二个未封的）⇒ 活体探测那一项 SKIP，不假装通过
  out=$(
    TMP_USER=m3-selftest
    REMOVE_UPSTREAM=""
    st_stub_set users "$(st_tmp_user_json 0)"
    start_hy2_client() { st_fake_client "$@"; }
    probe_socks_code() { echo 200; }
    check_gate_live
  )
  if [[ "$out" == *"SKIP  step4' 门位活体探测（没有第二个未封"* ]]; then
    ok "自测：没有第二个未封住宅用户 ⇒ 门位活体探测 SKIP（进摘要的 SKIP 计数）"
  else
    no "自测：活体探测没有可用邻居时没 SKIP" "$out"
  fi
  st_stub_reset
}

st_check_restart() {
  local out
  # ① 通过：重启后计数不变
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "$(st_tmp_user_json 104857600)"
    restart_daemon() { :; }
    check_restart_count
  )
  if [[ "$out" == PASS*没重复计* ]]; then ok "自测：判据⑤（重启后计数不变）PASS"; else no "自测：判据⑤ 通过分支不对" "$out"; fi
  st_stub_reset
  # ② 失败：重启把累计值又算了一遍（翻倍）
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "$(st_tmp_user_json 104857600)"
    restart_daemon() { st_stub_set ramp "{\"$TMP_USER\": 209715200}"; }
    check_restart_count
  )
  if [[ "$out" == FAIL* && "$out" == *异常增长* ]]; then ok "自测：判据⑤（重启后翻倍）FAIL"; else no "自测：判据⑤ 失败分支不对" "$out"; fi
  st_stub_reset
}

st_check_cleanup() {
  local out
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "$(st_tmp_user_json 0)"
    check_cleanup
  )
  if [[ "$out" == PASS*已删除* ]]; then ok "自测：收尾删掉临时用户 PASS"; else no "自测：收尾删除分支不对" "$out"; fi
  out=$(
    TMP_USER=m3-not-there
    check_cleanup
  )
  if [[ "$out" == FAIL*删不掉* ]]; then ok "自测：删不掉时 FAIL 并提示手工清理"; else no "自测：删除失败没被判失败" "$out"; fi
  st_stub_reset
}

self_test() {
  st_setup
  st_state_parsing
  st_units_and_judges
  st_login_and_crud
  st_neighbour
  st_check_add_user
  st_download_source
  st_check_traffic
  st_online_and_probe
  st_check_expiry
  st_summary_counts
  st_check_remove_upstream
  st_check_gate_live
  st_check_restart
  st_check_cleanup
  st_teardown
}

usage() {
  printf '用法：%s [--self-test] [--strict] [--admin-password-file <文件>] [--download-url <url>] [--keep-url <url>] [--base <目录>] [--remove-upstream <host:port|uuid|resi-N>]\n' "$0" >&2
  printf '  --keep-url  保活源必须回 200 且只有几 KB（字节数进判据 ② 的计数）。同一个 URL 在不同出口 IP 上结果不一样（Cloudflare 的 __down 在开发机 200、在 rick 实测 403）⇒ staging 跑判据 ①–④ 前先用 --keep-url 钉一个在那台机器上实测回 200 的小文件\n' >&2
  printf '  --remove-upstream  不可逆（上游凭据删了取不回来），而且只在池里 ≥2 条上游时用：删成空池后中继 fail-open 全部直连，判据 ④'"'"' 的门位活体探测无论门位对不对都回 200（那时它会 SKIP，不会假 PASS）\n' >&2
  exit 2
}

# 带值的选项：值缺了就报用法退出（不能 shift 到 $# 不动、把循环卡死）
need_value() { [ "$#" -ge 2 ] || usage; }

main() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --self-test)
        SELF_TEST=1
        shift
        ;;
      --admin-password-file)
        need_value "$@"
        ADMIN_PW_FILE=$2
        shift 2
        ;;
      --download-url)
        need_value "$@"
        DOWNLOAD_URL=$2
        shift 2
        ;;
      --keep-url)
        need_value "$@"
        KEEP_URL=$2
        shift 2
        ;;
      --base)
        need_value "$@"
        BASE=$2
        shift 2
        ;;
      --remove-upstream)
        need_value "$@"
        REMOVE_UPSTREAM=$2
        shift 2
        ;;
      --strict)
        STRICT=1
        shift
        ;;
      *) usage ;;
    esac
  done
  need_python
  mk_work
  trap cleanup EXIT
  if [ "$SELF_TEST" = "1" ]; then
    self_test
  else
    run_checks
  fi
  printf '\n合计 %d PASS / %d FAIL / %d SKIP\n' "$pass" "$fail" "$skipped"
  if [ "$skipped" -gt 0 ]; then
    printf '注意：%d 条判据被跳过（那几条什么都没验收到）；要让它们也算失败请加 --strict\n' \
      "$skipped"
  fi
  exit "$(exit_code "$fail" "$skipped")"
}

main "$@"
