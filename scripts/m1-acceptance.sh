#!/usr/bin/env bash
# b-ui v4 M1 机器化验收（spec §9）。
#   真机：sudo bash scripts/m1-acceptance.sh
#   自测：bash scripts/m1-acceptance.sh --self-test   # 不碰系统、不需要 root
# 环境变量：BUI（默认 /opt/b-ui/bin/bui）、BASE（默认 /opt/b-ui）、
#           SB112/SB113/SB114（三个 sing-box 版本的二进制路径，缺则跳过该版本）
set -uo pipefail

BUI=${BUI:-/opt/b-ui/bin/bui}
BASE=${BASE:-/opt/b-ui}
pass=0
fail=0

ok() { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
no() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

need_python() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'FATAL 需要 python3 来解析 JSON\n'
    exit 2
  fi
}

# 体检 JSON → 问题描述（空串 = 通过）
check_status_json() {
  python3 - "$1" <<'PY'
import json, sys
try:
    h = json.loads(sys.argv[1])
except Exception as e:
    print("status JSON 解析失败: %s" % e)
    raise SystemExit(0)
p = []
if h.get("status") != "ok":
    p.append("status=%s" % h.get("status"))
if h.get("drift"):
    p.append("drift=%d 条: %s" % (len(h["drift"]), h["drift"][:3]))
r = h.get("reconcile") or {}
if r.get("errors"):
    p.append("errors=%s" % r["errors"])
if r.get("verify_failures"):
    p.append("verify_failures=%s" % r["verify_failures"])
down = [s["unit"] for s in h.get("services", []) if not s.get("active")]
if down:
    p.append("未运行: %s" % ",".join(down))
print("; ".join(p))
PY
}

# 一份 *.caddy 里的顶层站点地址（每行一个 host，已去重）。
# 注释（含带 } 的注释）与引号里的花括号都不计数，逻辑与 bui 的 split_caddy_blocks 一致。
site_hosts() {
  python3 - "$1" <<'PY'
import sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()


def code(line):
    """剥掉注释：# 只有在行首或前面是空白、且不在引号里时才起注释作用"""
    quote = None
    i = 0
    while i < len(line):
        c = line[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
        elif c in "\"`":
            quote = c
        elif c == "#" and (i == 0 or line[i - 1].isspace()):
            return line[:i]
        i += 1
    return line


hosts, header, depth, opened = [], "", 0, False
for line in text.splitlines(True):
    for ch in code(line):
        if ch == "{":
            depth += 1
            opened = True
            if depth == 1:
                continue
        elif ch == "}":
            depth = max(depth - 1, 0)
            if depth == 0:
                continue
        if depth == 0:
            header += ch
    if opened and depth == 0:
        for tok in header.replace(",", " ").split():
            if tok == "import":
                break  # 顶层 import 指令，不是站点地址
            tok = tok.split("://")[-1].split("/")[0]
            if not tok or tok.startswith("(") or tok.startswith(":") or "*" in tok:
                continue  # 片段定义 / 只写端口 / 通配域名：curl 不了
            host, _, port = tok.rpartition(":")
            tok = host if host and port.isdigit() else tok
            if tok and tok not in hosts:
                hosts.append(tok)
        header, opened = "", False
print("\n".join(hosts))
PY
}

# 打本机 443 要一次 TLS + HTTP；状态码非 000 就算通（自测模式下被替换成 fake）
probe_site_code() {
  curl -sk --resolve "$1:443:127.0.0.1" --max-time 10 -o /dev/null -w '%{http_code}' "https://$1/" 2>/dev/null
}

# step 5（2026-09-13 裁决「Caddy 外部站点通道」）：import-v3 搬过来的外部站点
# 在 v4 的 bundled caddy 上仍然能握手。状态码非 000 即通（403/404 也算：证明 caddy 在应答）。
check_external_sites() {
  local f=$1 hosts h code
  if [ ! -f "$f" ]; then
    skip "step5 外部站点通道（$f 不存在，这台机器没有从 v3 导入的站点）"
    return
  fi
  hosts=$(site_hosts "$f")
  if [ -z "$hosts" ]; then
    no "step5 外部站点通道：$f 里解析不出站点地址" "$(head -n 5 "$f")"
    return
  fi
  for h in $hosts; do
    code=$(probe_site_code "$h")
    if [ -n "$code" ] && [ "$code" != "000" ]; then
      ok "step5 外部站点 $h 可握手（HTTP $code）"
    else
      no "step5 外部站点 $h 无响应" "curl -sk --resolve $h:443:127.0.0.1 https://$h/ → ${code:-空}"
    fi
  done
}

# ---------------------------------------------------------------------------
# step 6：Hysteria2 端到端鉴权（事故回归，2026-09-12 bwg-rick 真机）
#
# `auth.command` 只接受**单个可执行文件路径**：内核 `CommandAuthenticator` 直接
# `exec.Command(a.Cmd, addr, auth, tx)`，不过 shell、不按空格拆参数（调研 H15）。v4 一度渲染成
# `command: /opt/b-ui/bin/bui auth-hook`，内核于是去找一个**文件名带空格**的可执行文件，
# 钩子从未被调用，两台 Hysteria2 实例全员鉴权失败（客户端 404）约一小时。
# 这种错配 `sing-box check` / `xray -test` 一类的静态校验一概看不出来，只能真跑一次鉴权：
# 拿 state.json 里第一个未禁用用户的凭据起 bundled hysteria 客户端，经它的 socks5 打一次 https，
# 再看 <base>/auth-hook.log 最后一行是不是 allow（= 钩子真的被调用并放行了）。
#
# 凭据只写进 0600 的临时配置文件，**绝不进 argv**（ps 会泄露）；无论成败都杀进程、删临时目录。
# ---------------------------------------------------------------------------

# state.json → 三行：用户名 / HY2 密码 / 面板域名（缺任何一样就什么都不打印 ⇒ 上层 SKIP）
hy2_probe_creds() {
  python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
domain = (d.get("node") or {}).get("domain") or ""
for u in d.get("users") or []:
    if u.get("disabled"):
        continue
    pw = (u.get("credentials") or {}).get("hy2_password") or ""
    if u.get("username") and pw and domain:
        print(u["username"])
        print(pw)
        print(domain)
        break
PY
}

# hysteria 配置的 listen 行 → 端口号（`listen: :10000,20000-30000` → 10000）
listen_port() {
  [ -f "$1" ] || return 0
  sed -n '/^listen:/{s/[^0-9]*\([0-9][0-9]*\).*/\1/p;q;}' "$1"
}

# 一个空闲的本地 TCP 端口（给客户端的 socks5 入站用）
free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}

# 后台起 bundled hysteria 客户端、等它的 socks 端口就绪，回显 PID（自测里整条被换成 fake）
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

# 经客户端的 socks5 打一次 https，回显状态码（自测里被换成 fake）
probe_socks_code() {
  curl -s --socks5-hostname "127.0.0.1:$1" --max-time 20 \
    -o /dev/null -w '%{http_code}' https://api.ipify.org 2>/dev/null
}

# 钩子自己写的日志的最后一行（spec §3.2：`<RFC3339> <addr> <username> <结果>`）
auth_log_tail() {
  tail -n 1 "$BASE/auth-hook.log" 2>/dev/null
}

# 对一个 Hysteria2 监听端口跑一次真实鉴权 + 出网
hy2_auth_probe() {
  local label=$1 port=$2 sni=$3 user=$4 upass=$5
  local dir cfg log socks pid code line auth i
  dir=$(mktemp -d) || { no "step6 $label：建不出临时目录"; return; }
  chmod 700 "$dir"
  cfg="$dir/client.yaml"
  log="$dir/client.log"
  socks=$(free_port)
  if [ -z "$socks" ]; then
    no "step6 $label：分配不到本地端口"
    rm -rf "$dir"
    return
  fi
  # `user:pass` 原串就是 auth 载荷（调研 H1）；双引号标量 + 转义，密码里的 : # { 都不会歪
  auth="$user:$upass"
  auth=${auth//\\/\\\\}
  auth=${auth//\"/\\\"}
  (
    umask 077
    cat > "$cfg" <<EOF
server: 127.0.0.1:$port
auth: "$auth"
tls:
  sni: $sni
  insecure: true
socks5:
  listen: 127.0.0.1:$socks
EOF
  )
  pid=$(start_hy2_client "$cfg" "$log" "$socks")
  code=$(probe_socks_code "$socks")
  line=$(auth_log_tail)
  # `wait` 用不上：客户端是命令替换那个子 shell 的子进程，不是本 shell 的
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null
    i=0
    while [ "$i" -lt 20 ] && kill -0 "$pid" 2>/dev/null; do
      sleep 0.1
      i=$((i + 1))
    done
    kill -9 "$pid" 2>/dev/null
  fi
  rm -rf "$dir"
  if [ "$code" != "200" ]; then
    no "step6 $label 经 Hysteria2 出不了网（HTTP ${code:-空}）" \
      "auth-hook.log 末行：${line:-空}（钩子没被调用时这里不会有新行）"
    return
  fi
  case $line in
    *allow*) ok "step6 $label 端到端鉴权通过（HTTP 200，钩子判 allow）" ;;
    *) no "step6 $label 出网通了但钩子没记 allow" "auth-hook.log 末行：${line:-空}" ;;
  esac
}

check_hy2_auth() {
  local creds user upass sni direct resi
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step6 Hysteria2 端到端鉴权（$BASE/bin/hysteria 不可执行）"
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    skip "step6 Hysteria2 端到端鉴权（本机没有 curl）"
    return 0
  fi
  creds=$(hy2_probe_creds "$BASE/state.json")
  user=$(printf '%s\n' "$creds" | sed -n 1p)
  upass=$(printf '%s\n' "$creds" | sed -n 2p)
  sni=$(printf '%s\n' "$creds" | sed -n 3p)
  if [ -z "$user" ] || [ -z "$upass" ] || [ -z "$sni" ]; then
    skip "step6 Hysteria2 端到端鉴权（state.json 里没有可用的未禁用用户）"
    return 0
  fi
  direct=$(listen_port "$BASE/config.yaml")
  resi=$(listen_port "$BASE/config-residential.yaml")
  if [ -z "$direct" ] && [ -z "$resi" ]; then
    no "step6 取不到 Hysteria2 监听端口" "config.yaml / config-residential.yaml 里没有 listen 行"
    return 0
  fi
  [ -n "$direct" ] && hy2_auth_probe "直连 :$direct" "$direct" "$sni" "$user" "$upass"
  [ -n "$resi" ] && hy2_auth_probe "住宅 :$resi" "$resi" "$sni" "$user" "$upass"
  return 0
}

# step 7（spec §5.6）：IP 池槽位。端口与槽位的换算**只从 bui 自己的 API 取**，
# 脚本不重算（重算就会有第二份口径）。
#
# 槽位表 JSON → 问题描述（空串 = 自洽）。抽成独立函数是为了让自测能直接喂数据：
# 不需要 $BUI、不碰 systemd（与 check_status_json 同形）。
slots_json_problems() {
  python3 - "$1" <<'PY'
import json, sys
try:
    rows = (json.loads(sys.argv[1]) or {}).get("slots") or []
except Exception as e:
    print("slots JSON 解析失败: %s" % e)
    raise SystemExit(0)
p = []
if not rows:
    print("")
    raise SystemExit(0)
idx = [r["index"] for r in rows]
if 0 not in idx:
    p.append("没有槽 0（40000 / 2080 / hysteria-residential.service 会悬空）")
# 端口不许重叠、跳跃区间不许交叉
hops = sorted((r["hop"][0], r["hop"][1], r["index"]) for r in rows)
for a, b in zip(hops, hops[1:]):
    if a[1] >= b[0]:
        p.append("槽 %d 与槽 %d 的跳跃区间重叠" % (a[2], b[2]))
if len({r["hy2_port"] for r in rows}) != len(rows):
    p.append("HY2 端口有重复")
if len({r["relay_port"] for r in rows}) != len(rows):
    p.append("中继入站端口有重复")
borrow = [r["index"] for r in rows if r.get("borrowed")]
if borrow:
    p.append("槽 %s 正在借用别的 IP（不是错误，但验收时应为空）" % borrow)
print("; ".join(p))
PY
}

# 槽位表 JSON → 每行 `<槽序号> <HY2 端口>`
slots_ports() {
  printf '%s' "$1" | python3 -c 'import json, sys
try:
    rows = (json.load(sys.stdin) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    print(r["index"], r["hy2_port"])'
}

# 槽位表 JSON → 每行 `<HY2 端口> <用户名>`（一个槽上有几个用户就几行）
slots_users() {
  printf '%s' "$1" | python3 -c 'import json, sys
try:
    rows = (json.load(sys.stdin) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    for u in r.get("users") or []:
        print(r["hy2_port"], u)'
}

# 面板端口（state.json 的 node.ports.admin），读不到按 8080
admin_port() {
  python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print(8080)
    raise SystemExit(0)
print(((d.get("node") or {}).get("ports") or {}).get("admin") or 8080)
PY
}

# $1 = state.json，$2 = 用户名 → 该用户的订阅 token（users[].sub_token），取不到就空串。
# 2026-09-14 裁决：四个免鉴权端点认随机 token，用户名链接只在全局宽限期内还认（全新装机
# 从来不开宽限期）⇒ 验收一律按 token 取订阅，不然新装机上必然 404。
sub_token() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for u in d.get("users") or []:
    if u.get("username") == sys.argv[2]:
        print(u.get("sub_token") or "")
        break
PY
}

# 核对「槽位表自洽」、每槽的实例在跑且端口在听、每个住宅用户的订阅里的 HY2 住宅端口
# == 他槽位的端口。
check_slots() {
  local slots out port i user unit aport want tok
  slots=$("$BUI" residential slots --json 2>/dev/null)
  if [ -z "$slots" ]; then
    skip "step7 槽位（bui residential slots 无输出，可能住宅未启用）"
    return 0
  fi
  out=$(slots_json_problems "$slots")
  if [ -z "$out" ]; then
    ok "step7 槽位表自洽（含槽 0、端口不重复、跳跃区间不重叠、无借用）"
  else
    no "step7 槽位表有问题" "$out"
  fi

  # 每槽一个 hysteria 实例：单元在跑 + UDP 端口在听
  while read -r i port; do
    [ -n "$i" ] || continue
    if [ "$i" = "0" ]; then unit="hysteria-residential"; else unit="hysteria-residential-$i"; fi
    if systemctl is-active --quiet "$unit"; then
      ok "step7 $unit 在跑"
    else
      no "step7 $unit 未运行" "$(systemctl is-active "$unit" 2>&1)"
    fi
    if ss -lnu 2>/dev/null | grep -q ":$port\b"; then
      ok "step7 $unit 监听 :$port/udp"
    else
      no "step7 $unit 没在听 :$port/udp" "$(ss -lnu 2>/dev/null | head -5)"
    fi
  done <<EOF
$(slots_ports "$slots")
EOF

  # 订阅口径一致：用户的 HY2 住宅节点端口 == 他槽位的 hy2_port
  aport=$(admin_port "$BASE/state.json")
  while read -r want user; do
    [ -n "$user" ] || continue
    tok=$(sub_token "$BASE/state.json" "$user")
    if [ -z "$tok" ]; then
      no "step7 $user 没有订阅 token" "state.json 的 users[] 里取不到 sub_token（守护进程启动时应已补齐）"
      continue
    fi
    port=$(curl -fsS --max-time 10 "http://127.0.0.1:$aport/api/sub/$tok" 2>/dev/null |
      base64 -d 2>/dev/null | grep -F 'HY2%E4%BD%8F%E5%AE%85' |
      sed -n 's#.*@[^:]*:\([0-9]*\)?.*#\1#p' | head -1)
    if [ "$want" = "$port" ]; then
      ok "step7 $user 的订阅住宅端口 = $want"
    else
      no "step7 $user 的订阅住宅端口不对" "期望 $want，实际 ${port:-取不到}"
    fi
  done <<EOF
$(slots_users "$slots")
EOF
  return 0
}

# 二次 install 的输出 → 问题描述（空串 = 走了对账路径）。
# **整份输出**里找「已安装」，不许先 tail 截尾：已装机上 `install --yes` 之后还要打自检表与
# 结尾摘要，「已安装（…）执行对账」那行离尾巴几十行远，截尾窗口一概看不到它
# （2026-09-13 bwg-rick 真机据此误判 step2）。
check_reinstall_output() {
  if printf '%s' "$1" | grep -q "已安装"; then
    return 0
  fi
  printf '整份输出（%d 行）里没有「已安装」，这趟像是走了全新装机路径\n' \
    "$(printf '%s\n' "$1" | wc -l)"
}

# 对账报告 JSON → 问题描述（空串 = 零变更零错误）
check_report_clean() {
  python3 - "$1" <<'PY'
import json, sys
try:
    r = json.loads(sys.argv[1])
except Exception as e:
    print("报告 JSON 解析失败: %s" % e)
    raise SystemExit(0)
p = []
for k in ("changed", "restarted", "errors", "verify_failures"):
    if r.get(k):
        p.append("%s=%s" % (k, r[k]))
print("; ".join(p))
PY
}

run_checks() {
  # step 1：体检无漂移、六单元在跑、上轮对账无错
  local status_json out
  status_json=$("$BUI" status --json 2>/dev/null)
  out=$(check_status_json "$status_json")
  if [ -z "$out" ]; then ok "step1 体检：无漂移、无错误、六单元在跑"; else no "step1 体检不干净" "$out"; fi

  # step 2：二次 install 零变更 + 二次对账零变更
  local install_out
  install_out=$("$BUI" install --yes 2>&1)
  out=$(check_reinstall_output "$install_out")
  if [ -z "$out" ]; then
    ok "step2 二次 install 走对账路径（不覆盖 state）"
  else
    # 判定看的是整份输出，失败时才只贴尾巴几十行（整份太长，贴出来没人看）
    no "step2 二次 install 行为异常" "$out$(printf '%s\n' "$install_out" | tail -n 20)"
  fi
  local report
  report=$("$BUI" reconcile --dry-run 2>/dev/null)
  out=$(check_report_clean "$report")
  if [ -z "$out" ]; then ok "step2 二次对账零变更"; else no "step2 二次对账仍有改动" "$out"; fi

  # step 3：渲染结果过真实内核校验
  if "$BASE/bin/xray" run -test -c "$BASE/xray-config.json" >/dev/null 2>&1; then
    ok "step3 xray run -test"
  else
    no "step3 xray run -test 失败" "$("$BASE/bin/xray" run -test -c "$BASE/xray-config.json" 2>&1 | tail -n 3)"
  fi
  local sb
  for sb in "${SB112:-}" "${SB113:-}" "${SB114:-}" "$BASE/bin/sing-box"; do
    [ -n "$sb" ] || continue
    [ -x "$sb" ] || { skip "step3 sing-box check（$sb 不可执行）"; continue; }
    if "$sb" check -c "$BASE/singbox-relay.json" >/dev/null 2>&1; then
      ok "step3 sing-box check（$("$sb" version | head -n1)）"
    else
      no "step3 sing-box check 失败（$sb）" "$("$sb" check -c "$BASE/singbox-relay.json" 2>&1 | tail -n 3)"
    fi
  done
  if "$BASE/bin/caddy" validate --config "$BASE/Caddyfile" --adapter caddyfile >/dev/null 2>&1; then
    ok "step3 caddy validate（配置在 $BASE/Caddyfile）"
  else
    no "step3 caddy validate 失败" "$("$BASE/bin/caddy" validate --config "$BASE/Caddyfile" --adapter caddyfile 2>&1 | tail -n 3)"
  fi

  # step 3b：鉴权快照存在、0600、形状合总纲 C5（spec §3.2；形状不对 hysteria 会 fail-closed 拒绝所有人）
  if [ -f "$BASE/auth-snapshot.json" ] && [ "$(stat -c %a "$BASE/auth-snapshot.json")" = "600" ]; then
    out=$(python3 - "$BASE/auth-snapshot.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print("不是合法 JSON: %s" % e)
    raise SystemExit(0)
p = []
if d.get("schema") != 1:
    p.append("schema=%r（总纲 C5 要求 1）" % d.get("schema"))
if not isinstance(d.get("users"), dict):
    p.append("缺 users 对象（顶层键只能是 schema/users，不是扁平的用户名表）")
print("; ".join(p))
PY
)
    if [ -z "$out" ]; then
      ok "step3b auth-snapshot.json 存在、0600、形状合 C5（schema=1 + users）"
    else
      no "step3b auth-snapshot.json 形状不对" "$out"
    fi
  else
    no "step3b auth-snapshot.json 缺失或权限不对" "$(ls -l "$BASE/auth-snapshot.json" 2>&1)"
  fi

  # step 4：CLI 入口（spec §2.4）
  if [ "$(readlink -f /usr/local/bin/b-ui)" = "$(readlink -f "$BASE/bin/bui")" ]; then
    ok "step4 /usr/local/bin/b-ui 指向 bin/bui"
  else
    no "step4 b-ui 入口不对" "$(readlink -f /usr/local/bin/b-ui)"
  fi
  if [ -S /run/b-ui.sock ] && [ "$(stat -c %a /run/b-ui.sock)" = "600" ]; then
    ok "step4 /run/b-ui.sock 存在且 0600"
  else
    no "step4 socket 异常" "$(ls -l /run/b-ui.sock 2>&1)"
  fi

  # step 5：从 v3 导过来的外部站点仍然可达
  check_external_sites "${SITES_FILE:-$BASE/caddy/sites/imported-from-v3.caddy}"

  # step 6：两台 Hysteria2 真跑一次鉴权 + 出网（事故回归，见上面那段说明）
  check_hy2_auth

  # step 7：IP 池槽位（spec §5.6）
  check_slots
}

self_test() {
  local clean dirty out
  clean='{"status":"ok","services":[{"unit":"b-ui","active":true},{"unit":"xray","active":true}],
          "drift":[],"reconcile":{"changed":[],"restarted":[],"errors":[],"verify_failures":[]}}'
  dirty='{"status":"degraded","services":[{"unit":"xray","active":false}],
          "drift":[{"kind":"cron","path":"crontab","detail":"x"}],
          "reconcile":{"changed":[],"restarted":[],"errors":["boom"],"verify_failures":[]}}'
  out=$(check_status_json "$clean")
  if [ -z "$out" ]; then ok "自测：干净体检判通过"; else no "自测：干净体检被误判" "$out"; fi
  out=$(check_status_json "$dirty")
  if [[ "$out" == *degraded* && "$out" == *drift* && "$out" == *errors* && "$out" == *xray* ]]; then
    ok "自测：脏体检四项全报出"
  else
    no "自测：脏体检漏报" "$out"
  fi
  out=$(check_report_clean '{"changed":[],"restarted":[],"errors":[],"verify_failures":[],"drift":[],"notes":[]}')
  if [ -z "$out" ]; then ok "自测：零变更报告判通过"; else no "自测：零变更报告被误判" "$out"; fi
  out=$(check_report_clean '{"changed":["/opt/b-ui/config.yaml"],"restarted":["hysteria-server"],"errors":[],"verify_failures":[],"drift":[],"notes":[]}')
  if [[ "$out" == *config.yaml* ]]; then ok "自测：有变更的报告被判失败"; else no "自测：漏判变更" "$out"; fi
  self_test_reinstall_output
  self_test_external_sites
  self_test_hy2_auth
  self_test_slots
}

# step 7 的自测：只喂 JSON 给纯判定函数，不需要 $BUI、不碰 systemd
self_test_slots() {
  local good bad out
  good='{"slots":[{"index":0,"hy2_port":40000,"relay_port":2080,"hop":[41000,43999],"borrowed":false,"users":["a"]},
                  {"index":1,"hy2_port":40001,"relay_port":2081,"hop":[44000,46999],"borrowed":false,"users":[]},
                  {"index":2,"hy2_port":40002,"relay_port":2082,"hop":[47000,50000],"borrowed":false,"users":["b"]}]}'
  bad='{"slots":[{"index":1,"hy2_port":40001,"relay_port":2081,"hop":[41000,46999],"borrowed":true,"users":[]},
                 {"index":2,"hy2_port":40001,"relay_port":2082,"hop":[44000,50000],"borrowed":false,"users":[]}]}'
  out=$(slots_json_problems "$good")
  if [ -z "$out" ]; then ok "自测：自洽的槽位表判通过"; else no "自测：自洽的槽位表被误判" "$out"; fi
  out=$(slots_json_problems "$bad")
  if [[ "$out" == *槽\ 0* && "$out" == *重叠* && "$out" == *重复* && "$out" == *借用* ]]; then
    ok "自测：坏槽位表四项全报出"
  else
    no "自测：坏槽位表漏报" "$out"
  fi
  out=$(slots_json_problems '{"slots":[]}')
  if [ -z "$out" ]; then ok "自测：空槽位表不报错（住宅未启用）"; else no "自测：空槽位表被误判" "$out"; fi
  out=$(slots_ports "$good")
  if [ "$out" = "0 40000
1 40001
2 40002" ]; then ok "自测：slots_ports 逐槽给出 HY2 端口"; else no "自测：slots_ports 结果不对" "$out"; fi
  out=$(slots_users "$good")
  if [ "$out" = "40000 a
40002 b" ]; then ok "自测：slots_users 给出端口与用户名"; else no "自测：slots_users 结果不对" "$out"; fi
  self_test_sub_token
}

# step 7 的自测（续）：订阅 URL 的末段取自 state.json 的 users[].sub_token，不再是用户名
self_test_sub_token() {
  local f out
  f=$(mktemp) || { no "自测：建不出临时文件"; return; }
  printf '%s\n' '{"users":[{"username":"alice","sub_token":"0123456789abcdef0123456789abcdef"},
                            {"username":"bob"}]}' > "$f"
  out=$(sub_token "$f" alice)
  if [ "$out" = "0123456789abcdef0123456789abcdef" ]; then
    ok "自测：sub_token 取到用户的订阅 token"
  else
    no "自测：sub_token 没取到 token" "$out"
  fi
  out=$(sub_token "$f" bob)
  if [ -z "$out" ]; then ok "自测：没有 sub_token 的用户返回空串"; else no "自测：sub_token 凭空造了个 token" "$out"; fi
  out=$(sub_token "$f" carol)
  if [ -z "$out" ]; then ok "自测：不存在的用户返回空串"; else no "自测：sub_token 认了不存在的用户" "$out"; fi
  out=$(sub_token "$f/nope" alice)
  if [ -z "$out" ]; then ok "自测：state.json 读不到时返回空串"; else no "自测：读不到 state.json 却有输出" "$out"; fi
  rm -f "$f"
}

# step 2 的自测：只验「已安装」这一行的判定，重点是**输出很长也不能漏**（旧版 tail -n 20 就漏了）
self_test_reinstall_output() {
  local long out
  long="已安装（/opt/b-ui/state.json 已存在），执行对账"$'\n'
  long+=$(for i in $(seq 1 80); do printf 'PASS  自检项 %d\n' "$i"; done)
  long+=$'\n面板        https://example.com/\n用户        3 个（订阅见面板）'
  out=$(check_reinstall_output "$long")
  if [ -z "$out" ]; then
    ok "自测：输出 80+ 行、「已安装」在最前面也判通过（不截尾）"
  else
    no "自测：长输出里的「已安装」被漏掉" "$out"
  fi
  out=$(check_reinstall_output "$(printf '开始全新装机\nPASS  自检项 1\n')")
  if [[ "$out" == *已安装* ]]; then
    ok "自测：输出里没有「已安装」判失败"
  else
    no "自测：没有「已安装」却没被判失败" "$out"
  fi
}

# step 6 的自测：hysteria 客户端与 curl 都换成 fake，只验「取凭据 / 取端口 / 判定 / SKIP」
self_test_hy2_auth() {
  local d out
  d=$(mktemp -d) || { no "自测：建不出临时目录"; return; }
  mkdir -p "$d/bin"
  printf '#!/bin/sh\nexit 0\n' > "$d/bin/hysteria"
  chmod 755 "$d/bin/hysteria"
  printf 'listen: :10000,20000-30000\nauth:\n  type: command\n  command: /opt/b-ui/bin/bui-auth-hook\n' > "$d/config.yaml"
  printf 'listen: :40000,41000-50000\n' > "$d/config-residential.yaml"
  printf '2026-09-12T09:00:00Z 1.2.3.4:51820 alice allow\n' > "$d/auth-hook.log"
  printf '%s\n' '{"node":{"domain":"panel.example.com"},' \
    ' "users":[{"username":"ghost","disabled":true,"credentials":{"hy2_password":"x"}},' \
    '          {"username":"alice","disabled":false,"credentials":{"hy2_password":"pw:with:colons"}}]}' \
    > "$d/state.json"

  out=$(hy2_probe_creds "$d/state.json")
  if [ "$out" = "alice
pw:with:colons
panel.example.com" ]; then
    ok "自测：跳过禁用用户，取第一个可用用户的凭据与域名"
  else
    no "自测：凭据解析不对" "$out"
  fi
  if [ "$(listen_port "$d/config.yaml")" = "10000" ] &&
    [ "$(listen_port "$d/config-residential.yaml")" = "40000" ]; then
    ok "自测：从两份配置的 listen 行取到端口（带端口跳跃区间也只取第一个数）"
  else
    no "自测：listen 端口解析不对" \
      "$(listen_port "$d/config.yaml") / $(listen_port "$d/config-residential.yaml")"
  fi

  # ① 通过：两条通路各一条 PASS
  # shellcheck disable=SC2317  # 在 $( ) 子 shell 里覆盖真客户端与真 curl，下面那次调用会用到
  out=$(
    BASE=$d
    start_hy2_client() {
      sleep 5 &
      echo $!
    }
    probe_socks_code() { echo 200; }
    check_hy2_auth
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "2" ] && [[ "$out" == *allow* ]]; then
    ok "自测：两条通路都鉴权通过 → 两条 PASS"
  else
    no "自测：鉴权通了却没判通过" "$out"
  fi

  # ② 失败：出不了网（事故当天的形态——钩子没被调用，客户端 404）
  # shellcheck disable=SC2317
  out=$(
    BASE=$d
    start_hy2_client() {
      sleep 5 &
      echo $!
    }
    probe_socks_code() { echo 000; }
    check_hy2_auth
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "2" ]; then
    ok "自测：出不了网判失败"
  else
    no "自测：出不了网没被判失败" "$out"
  fi

  # ③ 没有可用用户 → SKIP（新装机还没加用户时不该报红）
  printf '%s\n' '{"node":{"domain":"panel.example.com"},"users":[{"username":"ghost","disabled":true,"credentials":{"hy2_password":"x"}}]}' > "$d/state.json"
  out=$(
    BASE=$d
    check_hy2_auth
  )
  if [[ "$out" == SKIP* ]]; then
    ok "自测：state.json 里没有未禁用用户 → SKIP"
  else
    no "自测：缺用户该 SKIP" "$out"
  fi
  rm -rf "$d"
}

# step 5 的自测：真 curl 换成 fake，只验「站点地址解析 + 状态码判定」
self_test_external_sites() {
  local d f out
  d=$(mktemp -d) || { no "自测：建不出临时目录"; return; }
  f="$d/imported-from-v3.caddy"
  cat > "$f" <<'EOF'
# 由 bui import-v3 原样导出
blog.example.com {
    root * /srv/blog  # 这个注释里有个 } 别被当成收尾
    tls /etc/caddy/certs/blog.crt /etc/caddy/certs/blog.key
}

https://shop.example.com:443, www.shop.example.com {
    reverse_proxy 127.0.0.1:3000
}
EOF
  out=$(site_hosts "$f")
  if [ "$out" = "blog.example.com
shop.example.com
www.shop.example.com" ]; then
    ok "自测：外部站点地址解析（含行内注释、scheme、端口、多地址）"
  else
    no "自测：站点地址解析不对" "$out"
  fi
  # shellcheck disable=SC2317  # 在 $( ) 子 shell 里覆盖真 curl，下面那次调用会用到
  out=$(
    probe_site_code() { echo 200; }
    check_external_sites "$f"
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] && [[ "$out" == *"HTTP 200"* ]]; then
    ok "自测：三个站点都应答 → 三条 PASS（带状态码）"
  else
    no "自测：站点可达却没判通过" "$out"
  fi
  # shellcheck disable=SC2317
  out=$(
    probe_site_code() { echo 000; }
    check_external_sites "$f"
  )
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "3" ]; then
    ok "自测：状态码 000 判失败"
  else
    no "自测：000 没被判失败" "$out"
  fi
  out=$(check_external_sites "$d/not-there.caddy")
  if [[ "$out" == SKIP* ]]; then ok "自测：没有导入文件 → SKIP"; else no "自测：缺文件该 SKIP" "$out"; fi
  rm -rf "$d"
}

main() {
  need_python
  if [ "${1:-}" = "--self-test" ]; then
    self_test
  else
    run_checks
  fi
  printf '\n合计 %d PASS / %d FAIL\n' "$pass" "$fail"
  [ "$fail" -eq 0 ]
}

main "$@"
