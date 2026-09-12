#!/usr/bin/env bash
# b-ui v4 M3 机器化验收（spec §9 的 M3 行）。逐项判定四条判据：
#   ① 加用户时内核 NRestarts / MainPID 不变，且在线会话不断
#   ② 已知字节数（下载源实报的 Content-Length）的流量计数误差 ±5%
#   ③ 到期用户新登录被拒、既有连接被踢
#   ④ 重启守护进程后计数不重复（不翻倍）
#
#   真机：sudo bash scripts/m3-acceptance.sh
#   自测：bash scripts/m3-acceptance.sh --self-test   # 不出网、不碰 /opt、不需要 root
#
# 选项：--admin-password-file <文件>  管理员密码（`ADMIN_PASSWORD=` 行或整行密码）；
#                                    缺省读 <base>/v3-backup/admin.env
#       --download-url <url>         下载源（缺省按候选列表逐个 HEAD 探活取第一个可用的）
#       --keep-url <url>             保活用的小文件（缺省同站 2KB）
#       --base <目录>                缺省 /opt/b-ui
# 环境变量：M3_ADD_WAIT / M3_FLUSH_WAIT / M3_STABLE_WAIT / M3_KICK_WAIT /
#           M3_RESTART_WAIT / M3_KEEP_INTERVAL / M3_USAGE_POLL / M3_FIRST_OK_WAIT /
#           M3_BLOCK_WAIT —— 各等待窗口（秒），自测里调小。
#
# 退出码 = FAIL 数（0 = 全过）；前置条件缺失（没有 python3 / 取不到密码 / 登录失败）打
# FATAL 退 2。密码与 JWT **绝不进 argv**：密码经 0600 临时文件喂 curl 的 `--data-binary`，
# token 经 `curl -K -` 的 stdin 配置传（`ps` 看不到）。
set -uo pipefail
LC_ALL=C

BASE=${BASE:-/opt/b-ui}
ADMIN_PW_FILE=""
KEEP_URL="https://speed.cloudflare.com/__down?bytes=2048"
SELF_TEST=0

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

# 判据 ① 看这些内核单元：直连 + 每个住宅实例 + xray；判据 ④ 重启的是守护进程。
# 这里先给 v3 的单实例名字兜底，真机分支（run_checks）开头再按槽位展开 ——
# **不在这里就调 $BUI**：`--self-test` 也会走到顶层赋值，那一下会去碰真实系统。
KERNEL_UNITS="hysteria-server hysteria-residential xray"
DAEMON_UNIT="b-ui"

# 槽位由 bui 自己报，脚本不重算（spec §5.6）。读不到（住宅未启用 / bui 没跑）时打印空。
resi_units() {
  "${BUI:-$BASE/bin/bui}" residential slots --json 2>/dev/null |
    python3 -c 'import json, sys
try:
    rows = (json.load(sys.stdin) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    i = r["index"]
    print("hysteria-residential" if i == 0 else "hysteria-residential-%d" % i)' 2>/dev/null
}

# 判据 ① 要看**每个**住宅实例的 NRestarts/MainPID，漏一个就等于没验「加用户不重启」。
# 读不到槽位时退回 v3 的单实例名字。
load_kernel_units() {
  KERNEL_UNITS="hysteria-server xray $(resi_units)"
  case " $KERNEL_UNITS " in
    *" hysteria-residential "*) : ;;
    *) KERNEL_UNITS="hysteria-server hysteria-residential xray" ;;
  esac
}

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

pass=0
fail=0

# 运行期状态
WORK=""
API=""
API_OUT=""
TOKEN=""
PW_SRC=""
TMP_USER=""
TMP_PASS=""
PIDS=""
SNI=""
HY2_PORT=""
ADMIN_PORT=""
OBFS_ON=0
OBFS_PW=""

ok() { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
no() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

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

read_user_total() { read_user_row "$1" | sed -n 1p; }
read_user_blocked() { read_user_row "$1" | sed -n 2p; }

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

# $1=配置文件 $2=用户名 $3=HY2 密码 $4=本地 socks 端口。凭据只落 0600 的文件。
# `user:pass` 原串就是 auth 载荷；双引号标量 + 转义，密码里的 : # { 都不会歪。
write_client_cfg() {
  local auth=$2:$3
  auth=${auth//\\/\\\\}
  auth=${auth//\"/\\\"}
  (
    umask 077
    printf 'server: 127.0.0.1:%s\nauth: "%s"\ntls:\n  sni: %s\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:%s\n' \
      "$HY2_PORT" "$auth" "$SNI" "$4" >"$1"
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

# 每行：<unit> <NRestarts> <MainPID>
unit_stamps() {
  local u
  for u in $KERNEL_UNITS; do
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
# 多实例时流量可能落在任意一个住宅实例上，`bui` 的采样已覆盖全部 `trafficStats` 端口
# （`traffic::stats_ports`），所以这一项**不需要**按槽分别核对。
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

# ---------------------------------------------------------------------------
# 四条判据
# ---------------------------------------------------------------------------

# 判据 ①：加用户时内核不重启、在线会话不断。
# 用户增删只改 auth-snapshot.json（hysteria 侧）与走 gRPC AddUser（xray 侧），
# `clients` 不进 xray 配置的结构哈希（render/xray.rs:64），所以对账不该重启任何内核。
check_add_user() {
  local before after out code creds nuser npass
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

# 判据 ②：已知字节数的流量计数误差 ±5%。已知字节数来自下载源 HEAD 探活实报的
# Content-Length（不写死 104857600 —— 候选源换了、或换成小一号的文件都不该让判据失真）。
# 口径（以代码为准）：面板 `usage.total` = 上下行之和 —— `TxRx::total()` 是 `tx + rx`
# （crates/bui/src/modules/panel/mod.rs:59），hysteria 的 tx 是「发给客户端」、rx 是
# 「从客户端收到」的字节，所以一次纯下载 ≈ 100MB(tx) + 请求头/TLS 记录开销(rx)，
# 正偏差很小；判定用 curl 自己数的 `size_download` 当「已知流量」。
# 等待：采样 10s 一轮、用量最多 30s 合并落盘一次（traffic.rs 的
# SAMPLE_INTERVAL_SECS / FLUSH_INTERVAL_SECS），所以最坏 ~40s 才在面板可见，而且一次下载
# 可能跨过落盘 tick、先落半截 ⇒ 轮询到「连续 STABLE_WAIT(40s) 不变」或 FLUSH_WAIT(90s) 为止。
check_traffic() {
  local t0 t1 socks cfg pid out size code dev
  if [ -z "$TMP_USER" ] || [ -z "$TMP_PASS" ]; then
    skip "step2 已知字节数的计数误差（没有临时用户）"
    return
  fi
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step2 已知字节数的计数误差（$BASE/bin/hysteria 不可执行）"
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
  write_client_cfg "$cfg" "$TMP_USER" "$TMP_PASS" "$socks"
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
    ok "step2 计数偏差 $dev%（面板 $t0→$t1 字节，已知 $size 字节，源 $DOWNLOAD_URL 报 $EXPECT_BYTES 字节，口径 tx+rx）"
  else
    no "step2 计数超出 ±$TOL_PCT%" "$out（源 $DOWNLOAD_URL 报 $EXPECT_BYTES 字节）"
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

# 判据 ③：到期用户新登录被拒 + 既有连接被踢。
# 新登录由 `auth_hook::decide` 自己比 `expires_at`（不等快照重写），所以过期即拒；
# 既有连接要等采样轮（≤10s）算出 newly_blocked 再 `POST /kick`，而 kick 只是标记，
# 要等该用户下次有流量才真断 —— 保活循环提供的就是这串流量（H12）。
check_expiry() {
  local socks cfg pid live=0 ok0=0 bad0=0 code probe hres
  if [ -z "$TMP_USER" ] || [ -z "$TMP_PASS" ]; then
    skip "step3 到期被拒并被踢（没有临时用户）"
    return
  fi
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step3 到期被拒并被踢（$BASE/bin/hysteria 不可执行）"
    return
  fi

  socks=$(free_port)
  cfg="$WORK/keep-tmp.yaml"
  write_client_cfg "$cfg" "$TMP_USER" "$TMP_PASS" "$socks"
  pid=$(start_hy2_client "$cfg" "$WORK/keep-tmp.client.log" "$socks")
  track_pid "$pid"
  read -r ok0 bad0 < <(probe_first_ok "$socks" "$FIRST_OK_WAIT")
  if [ "$ok0" -gt 0 ]; then
    live=1
  else
    no "step3 到期前临时用户就连不上，判据无法判定" \
      "失败 $bad0 次；客户端日志末行：$(tail -n 1 "$WORK/keep-tmp.client.log" 2>/dev/null)"
  fi

  code=$(expire_temp_user)
  if [ "$code" != "200" ]; then
    no "step3 设到期失败（PUT /api/users/$TMP_USER → HTTP $code）" "$(api_body)"
    stop_client "$pid"
    return
  fi
  sleep $((EXPIRE_TTL + 1))
  if wait_user_blocked "$TMP_USER" "$BLOCK_WAIT"; then
    ok "step3 面板把到期用户判成 blocked（expiresAt=$(read_user_row "$TMP_USER" | sed -n 3p)）"
  else
    no "step3 面板没把到期用户判成 blocked" "limits.expiresAt=$(read_user_row "$TMP_USER" | sed -n 3p)"
  fi

  # 新登录：另起一个客户端，凭据没变，只是人已到期 ⇒ 钩子必须拒（出不了网）
  local nsocks ncfg npid
  nsocks=$(free_port)
  ncfg="$WORK/relogin.yaml"
  write_client_cfg "$ncfg" "$TMP_USER" "$TMP_PASS" "$nsocks"
  npid=$(start_hy2_client "$ncfg" "$WORK/relogin.client.log" "$nsocks")
  track_pid "$npid"
  probe=$(probe_socks_code "$nsocks" "$KEEP_URL")
  stop_client "$npid"
  # 只看「探测失败」不够：服务端宕机 / 端口不通也失败。必须同时在钩子日志里看到这个人被
  # 判 expired|blocked，才算「鉴权错」。
  hres=$(hook_last_result "$TMP_USER")
  if [ "$probe" = "200" ]; then
    no "step3 到期用户还能新建连接出网（HTTP 200）" \
      "钩子日志末行：$(tail -n 1 "$BASE/auth-hook.log" 2>/dev/null)"
  elif [ "$hres" = "expired" ] || [ "$hres" = "blocked" ]; then
    ok "step3 到期用户新登录被拒（HTTP ${probe:-空}，钩子判定 $hres）"
  else
    no "step3 新登录没通，但钩子没记下鉴权拒绝（判定=${hres:-无}），只能算连不上" \
      "钩子日志末行：$(tail -n 1 "$BASE/auth-hook.log" 2>/dev/null)"
  fi

  if [ "$live" = "1" ]; then
    if wait_session_drop "$socks" "$KICK_WAIT"; then
      ok "step3 既有连接在 ${KICK_WAIT}s 内被踢断（连续 3 次 curl 失败）"
    else
      no "step3 既有连接 ${KICK_WAIT}s 内没断" \
        "最后一次探测仍是 HTTP $(probe_socks_code "$socks" "$KEEP_URL")"
    fi
  fi
  stop_client "$pid"
}

# 判据 ④：重启守护进程后计数不重复。
check_restart_count() {
  local t0 t1 out
  if [ -z "$TMP_USER" ]; then
    skip "step4 重启不重复计数（没有临时用户）"
    return
  fi
  t0=$(read_user_total "$TMP_USER")
  if [ -z "$t0" ]; then
    no "step4 重启前读不到 usage.total" "$(api_body | head -c 300)"
    return
  fi
  restart_daemon
  sleep "$RESTART_WAIT"
  if ! wait_api_up 30; then
    no "step4 重启 $DAEMON_UNIT 后面板没恢复应答" "$API/api/users 连不上"
    return
  fi
  t1=$(read_user_total "$TMP_USER")
  out=$(judge_restart_total "$t0" "$t1")
  if [ -z "$out" ]; then
    ok "step4 restart $DAEMON_UNIT 后 usage.total 没重复计（$t0 → $t1 字节）"
  else
    no "step4 重启后计数异常" "$out"
  fi
}

# 判据 ⑤（收尾）：临时用户删干净
check_cleanup() {
  local code
  if [ -z "$TMP_USER" ]; then
    skip "step5 删除临时用户（没建成）"
    return
  fi
  code=$(api DELETE "/api/users/$TMP_USER")
  if [ "$code" = "200" ]; then
    ok "step5 临时用户 $TMP_USER 已删除"
    TMP_USER=""
  else
    no "step5 删不掉临时用户 $TMP_USER（HTTP $code）" \
      "$(api_body)；请手工 DELETE $API/api/users/$TMP_USER"
  fi
}

run_checks() {
  local pw
  [ "$(id -u)" = "0" ] || printf 'WARN  不是 root：systemctl 与 %s 下的文件可能读不到\n' "$BASE"
  load_kernel_units
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
"""M3 自测用的面板桩：只实现本脚本用到的 5 个端点，状态放在一个 JSON 文件里
（自测直接改这个文件来摆局面）。只监听 127.0.0.1，不出网。

state 文件的键：password / token / users（/api/users 的投影数组）/
ramp（{用户名: 目标 usage.total}，每次 GET /api/users 把该用户的 total 抬到目标值，
模拟采样落盘）/ ramp_seq（{用户名: [总量, 总量, …]}，每次 GET 取走一个，模拟「先落半截、
再落全量」的多轮落盘）/ expire_marks_blocked（PUT 设到期后是否置 blocked）。
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
                "disabled": False, "blocked": False,
            })
            save(d)
            return self.reply(200, {"success": True, "user": name,
                                    "password": "stub-hy2-password",
                                    "uuid": "00000000-0000-4000-8000-000000000000"})
        return self.reply(404, {"error": "no route"})

    def do_GET(self):
        d = load()
        if not self.authed(d):
            return self.reply(401, {"error": "Unauthorized"})
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
 "ramp_seq": {}, "expire_marks_blocked": true}
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
  printf '2026-09-12T09:00:00Z 1.2.3.4:51820 alice allow\n' >"$ST_DIR/base/auth-hook.log"
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
  sleep 30 >/dev/null 2>&1 &
  echo $!
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
  write_client_cfg "$cfg" alice 'pw:with:colons' 1080
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

  # 槽位读不到时 KERNEL_UNITS 必须退回单住宅实例名（不然判据 ① 会漏掉住宅那台）
  out=$(
    KERNEL_UNITS="hysteria-server xray"
    case " $KERNEL_UNITS " in
      *" hysteria-residential "*) printf 'kept' ;;
      *) printf 'fallback' ;;
    esac
  )
  if [ "$out" = "fallback" ]; then
    ok "自测：读不到槽位时退回单住宅实例名"
  else
    no "自测：兜底分支没生效" "$out"
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
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "2" ]; then
    ok "自测：判据①（内核没动 + 会话不断）两条 PASS"
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
}

st_check_traffic() {
  local out
  # ① 通过：下满 100MB，面板随后涨 100MB
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    start_hy2_client() { st_fake_client; }
    download_via_socks() {
      st_stub_set ramp "{\"$TMP_USER\": 105000000}"
      printf '104857600 200\n'
    }
    check_traffic
  )
  if [[ "$out" == PASS*偏差* ]]; then ok "自测：判据②（100MB 计到 ~100MB）PASS"; else no "自测：判据② 通过分支不对" "$out"; fi
  st_stub_reset
  # ② 失败：只计到一半
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
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
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
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
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
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
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
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
    st_stub_set users '[{"username": "m3-selftest", "usage": {"total": 0, "monthly": {}}, "limits": {}, "blocked": false}]'
    st_stub_set ramp_seq '{"m3-selftest": [52428800, 52428800, 105000000]}'
    wait_for_usage m3-selftest 0 "$FLUSH_WAIT"
  )
  if [ "$out" = "52428800" ]; then
    ok "自测：STABLE_WAIT 只有一个轮询间隔时半截值会被当终值 ⇒ 该场景有判别力"
  else
    no "自测：半截场景没判别力（压小 STABLE_WAIT 仍拿到终值）" "$out"
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
  # ① 通过：PUT 之后桩把用户置 blocked ⇒ 面板判 blocked、新登录打不通、保活开始失败
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 1, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] &&
    [[ "$out" == *新登录被拒* && "$out" == *被踢断* ]]; then
    ok "自测：判据③（判 blocked + 新登录被拒 + 既有连接被踢）三条 PASS"
  else
    no "自测：判据③ 通过分支不对" "$out"
  fi
  st_stub_reset
  # ② 失败：到期不生效（blocked 一直是 false）⇒ 三条判定全红
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 1, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    st_stub_set expire_marks_blocked false
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "3" ] &&
    [[ "$out" == *"还能新建连接出网"* && "$out" == *没断* ]]; then
    ok "自测：判据③（到期没生效）三条 FAIL"
  else
    no "自测：判据③ 失败分支不对" "$out"
  fi
  st_stub_reset
  # ③ 失败：新登录确实没通，但钩子日志里没有这一位的鉴权拒绝（= 服务端宕了 / 端口不通）
  out=$(
    TMP_USER=m3-selftest
    TMP_PASS=pw
    export ST_HOOK_LOG=0
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 1, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    start_hy2_client() { st_fake_client; }
    probe_socks_code() { st_probe_by_blocked; }
    check_expiry
  )
  if [[ "$out" == *"钩子没记下鉴权拒绝"* ]]; then
    ok "自测：判据③（连不上但钩子没判拒）不算通过"
  else
    no "自测：没区分「被鉴权拒」和「连不上」" "$out"
  fi
  st_stub_reset
}

st_check_restart() {
  local out
  # ① 通过：重启后计数不变
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 104857600, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    restart_daemon() { :; }
    check_restart_count
  )
  if [[ "$out" == PASS*没重复计* ]]; then ok "自测：判据④（重启后计数不变）PASS"; else no "自测：判据④ 通过分支不对" "$out"; fi
  st_stub_reset
  # ② 失败：重启把累计值又算了一遍（翻倍）
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 104857600, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
    restart_daemon() { st_stub_set ramp "{\"$TMP_USER\": 209715200}"; }
    check_restart_count
  )
  if [[ "$out" == FAIL* && "$out" == *异常增长* ]]; then ok "自测：判据④（重启后翻倍）FAIL"; else no "自测：判据④ 失败分支不对" "$out"; fi
  st_stub_reset
}

st_check_cleanup() {
  local out
  out=$(
    TMP_USER=m3-selftest
    st_stub_set users "[{\"username\": \"$TMP_USER\", \"usage\": {\"total\": 0, \"monthly\": {}}, \"limits\": {}, \"blocked\": false}]"
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
  st_check_expiry
  st_check_restart
  st_check_cleanup
  st_teardown
}

usage() {
  printf '用法：%s [--self-test] [--admin-password-file <文件>] [--download-url <url>] [--keep-url <url>] [--base <目录>]\n' "$0" >&2
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
  printf '\n合计 %d PASS / %d FAIL\n' "$pass" "$fail"
  [ "$fail" -gt 255 ] && exit 255
  exit "$fail"
}

main "$@"
