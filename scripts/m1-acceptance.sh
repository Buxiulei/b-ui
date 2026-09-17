#!/usr/bin/env bash
# b-ui v4 M1 机器化验收（spec §9）。八步：体检 / 二次装机 / 内核校验 / CLI 入口 /
# 外部站点 / Hysteria2 端到端鉴权 / IP 池槽位 / 住宅端口跳跃的 nft 表。
#   真机：sudo bash scripts/m1-acceptance.sh
#   自测：bash scripts/m1-acceptance.sh --self-test   # 不碰系统、不需要 root
# 环境变量：BUI（默认 /opt/b-ui/bin/bui）、BASE（默认 /opt/b-ui）、
#           SB112/SB113/SB114（三个 sing-box 版本的二进制路径，缺则跳过该版本）
set -uo pipefail
LC_ALL=C

BUI=${BUI:-/opt/b-ui/bin/bui}
BASE=${BASE:-/opt/b-ui}
pass=0
fail=0

ok() { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
no() { printf 'FAIL  %s\n      %s\n' "$1" "${2:-}"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

# 外部工具在不在（`curl` / `ss` / `nft`）。抽成函数是为了让自测能在子 shell 里顶掉它，
# 从而覆盖「本机没有这个工具 ⇒ 必须 SKIP 而不是当成通过」那几条分支。
have() { command -v "$1" > /dev/null 2>&1; }

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
# 4.1 起这一步有**三条**探测，住宅那两条与直连口径不同（spec §3.1、§6）：
#   ① 直连 `:10000`  —— 用户的 `hy2_password`，判据仍是 auth-hook.log 末行的 allow；
#   ② 住宅 `:40000`  —— 用户那条**池凭据** `name:secret`，判据是住宅单元 journald 里
#      `[<name>] inbound …` 那一行（sing-box 对鉴权失败不打任何日志 ⇒ 只能验成功路径，
#      `auth-hook.log` 住宅路径从此一条记录都没有）；
#   ③ 住宅 + 整段 mport —— 同 ②，但客户端往跳跃段发包，**专验 `inet bui` 的 output 链**：
#      本机发往自身地址的包不过 prerouting，那条链没了它就是唯一会红的判据。
#
# 凭据只写进 0600 的临时配置文件，**绝不进 argv**（ps 会泄露）；无论成败都杀进程、删临时目录。
# ---------------------------------------------------------------------------

# state.json → 五行：用户名 / HY2 密码 / 面板域名 / obfs 开关（1/0）/ obfs 密码
# （前三样缺任何一样就什么都不打印 ⇒ 上层 SKIP）。混淆覆盖直连与住宅，三条探测都要带。
hy2_probe_creds() {
  python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
domain = (d.get("node") or {}).get("domain") or ""
obfs = (d.get("node") or {}).get("obfs") or {}
obfs_pw = obfs.get("password") or ""
for u in d.get("users") or []:
    if u.get("disabled"):
        continue
    pw = (u.get("credentials") or {}).get("hy2_password") or ""
    if u.get("username") and pw and domain:
        print(u["username"])
        print(pw)
        print(domain)
        print("1" if obfs.get("enabled") and obfs_pw else "0")
        print(obfs_pw)
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

# 钩子自己写的日志的最后一行（spec §3.2：`<RFC3339> <addr> <username> <结果>`）。
# **只对直连有效**：4.1 的住宅走 sing-box，它对 hysteria2 鉴权失败不打任何日志，
# `auth-hook.log` 住宅路径从此一条记录都没有（spec §6）。
auth_log_tail() {
  tail -n 1 "$BASE/auth-hook.log" 2>/dev/null
}

# $1 = state.json，$2 = 用户名，$3 = 字段（name / secret）
#   → 该用户那条住宅凭据（`residential.hy2_pool.creds[]` 里 id ==
#     `users[].credentials.hy2_resi_cred` 的那条）的字段值；取不到就空串。
# **凭据只经 stdout 进变量、再进 0600 的临时配置文件，不进 argv**（`ps` 会泄露）。
resi_cred_field() {
  python3 - "$1" "$2" "$3" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
cid = ""
for u in d.get("users") or []:
    if u.get("username") == sys.argv[2]:
        cid = (u.get("credentials") or {}).get("hy2_resi_cred") or ""
        break
if not cid:
    raise SystemExit(0)
for c in ((d.get("residential") or {}).get("hy2_pool") or {}).get("creds") or []:
    if c.get("id") == cid:
        print(c.get(sys.argv[3]) or "")
        break
PY
}

# 住宅凭据的两半：sing-box `users[].name`（= `auth_user` 匹配值与日志里方括号那个名字）
# 与 secret。写进客户端的 auth 串是 `name:secret`，与订阅逐字同源（spec §3.1）。
resi_cred_name() { resi_cred_field "$1" "$2" name; }
resi_cred_secret() { resi_cred_field "$1" "$2" secret; }

# 探测起点：给 `journalctl --since` 用的本地时刻（住宅判据只看这之后的新行）
probe_since() {
  date '+%Y-%m-%d %H:%M:%S'
}

# 住宅单元 journald 里带某个凭据 name 的最后一行连接日志。**拆成两半**：纯的那一半
# （`resi_log_pick`，stdin → 命中行）让自测能直接灌日志原文，带 I/O 的那一半
# （`resi_log_hit`）只剩 journalctl 那一行。
#
# sing-box 的 hysteria2 入站对**鉴权失败不打任何日志**（auth 不命中时走 masquerade），
# 所以住宅这条只能验成功路径：`[<name>] inbound connection to …`（TCP）或
# `[<name>] inbound packet connection to …`（UDP）—— 载荷因此只能是 ` inbound `：收窄成
# `inbound connection to ` 会把 UDP 那一半全漏掉，而 `inbound connection from <地址>`
# 那行没有方括号 ⇒ 不会把「还没鉴权的握手」当成成功。就是这一行逼着
# `hy2-residential.json` 的 `log.level` 留在 `info`（spec §6）。
# 用 `grep -F` 而不是正则：生产用户名是中文、迁移凭据的 name 就是用户名。
#
# $1 = 凭据 name（自测喂 `sentinel::fixtures_hy2_resi` 的原文）
resi_log_pick() {
  grep -F "[$1] inbound " | tail -n 1
}

# $1 = 探测起点（`--since` 的实参）、$2 = 凭据 name。`--since` 不许省：少了它，该凭据
# 历史上任何一次成功连接都会让这条判据从此永远为真。
resi_log_hit() {
  journalctl -u hysteria-residential --since "$1" --no-pager 2>/dev/null | resi_log_pick "$2"
}

# 对一个 Hysteria2 监听端口跑一次真实鉴权 + 出网。
#   $6 / $7 = obfs 开关（1/0）/ obfs 密码
#   $8 = mport（整段跳跃，非空则客户端按 `server: 127.0.0.1:<port>,<mport>` 发包 ——
#        hysteria 的端口跳跃写法，等价于订阅里的 `mport=`）
#   $9 = 日志判据：`direct` 查 `auth-hook.log` 末行的 allow，`resi` 查住宅单元 journald 里
#        那条带凭据 name 的连接行
hy2_auth_probe() {
  local label=$1 port=$2 sni=$3 user=$4 upass=$5 obfs_on=${6:-0} obfs_pw=${7:-}
  local mport=${8:-} judge=${9:-direct}
  local dir cfg log socks pid code line auth obfs_q i server since
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
  obfs_q=${obfs_pw//\\/\\\\}
  obfs_q=${obfs_q//\"/\\\"}
  # 带 mport 的那条**专验 `inet bui` 的 output 链**：客户端随机挑跳跃段里的端口发包，
  # 而本机自己发出去的包不过 prerouting，只有 output 链能把它们 REDIRECT 到住宅端口
  # （spec §2.4，tizi PoC 实测）。少了那条链时这一条是唯一会红的判据。
  server="127.0.0.1:$port"
  [ -n "$mport" ] && server="127.0.0.1:$port,$mport"
  since=$(probe_since)
  (
    umask 077
    cat > "$cfg" <<EOF
server: $server
auth: "$auth"
tls:
  sni: $sni
  insecure: true
socks5:
  listen: 127.0.0.1:$socks
EOF
    if [ "$obfs_on" = "1" ]; then
      printf 'obfs:\n  type: salamander\n  salamander:\n    password: "%s"\n' "$obfs_q" >>"$cfg"
    fi
  )
  pid=$(start_hy2_client "$cfg" "$log" "$socks")
  code=$(probe_socks_code "$socks")
  if [ "$judge" = resi ]; then
    line=$(resi_log_hit "$since" "$user")
  else
    line=$(auth_log_tail)
  fi
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
  if [ "$judge" = resi ]; then
    if [ "$code" != "200" ]; then
      no "step6 $label 经 Hysteria2 出不了网（HTTP ${code:-空}）" \
        "住宅单元日志：${line:-空}（鉴权失败时 sing-box 一行都不打，看 journalctl -u hysteria-residential）；
      探测用户被 blocked（禁用 / 到期 / 超总量 / 超月量）时这条必红——门位把他切到 deny 出站，
      先看 bui status 里该用户的门位，再怀疑 nft 的 output 链"
      return
    fi
    if [ -n "$line" ]; then
      ok "step6 $label 端到端通过（HTTP 200，住宅日志有该凭据的连接行）"
    else
      no "step6 $label 出网通了但住宅日志里没有该凭据的连接行" \
        "journalctl -u hysteria-residential --since '$since' 里找不到 [<name>] inbound …（log.level 掉到 info 以下也会这样）"
    fi
    return
  fi
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
  local creds user upass sni obfs_on obfs_pw direct ports port hop rname rsec
  if [ ! -x "$BASE/bin/hysteria" ]; then
    skip "step6 Hysteria2 端到端鉴权（$BASE/bin/hysteria 不可执行）"
    return 0
  fi
  if ! have curl; then
    skip "step6 Hysteria2 端到端鉴权（本机没有 curl）"
    return 0
  fi
  creds=$(hy2_probe_creds "$BASE/state.json")
  user=$(printf '%s\n' "$creds" | sed -n 1p)
  upass=$(printf '%s\n' "$creds" | sed -n 2p)
  sni=$(printf '%s\n' "$creds" | sed -n 3p)
  obfs_on=$(printf '%s\n' "$creds" | sed -n 4p)
  obfs_pw=$(printf '%s\n' "$creds" | sed -n 5p)
  if [ -z "$user" ] || [ -z "$upass" ] || [ -z "$sni" ]; then
    skip "step6 Hysteria2 端到端鉴权（state.json 里没有可用的未禁用用户）"
    return 0
  fi
  # 直连：口径完全不变（bundled hysteria 客户端 → https 200 → auth-hook.log 末行 allow）
  direct=$(listen_port "$BASE/config.yaml")
  if [ -n "$direct" ]; then
    hy2_auth_probe "直连 :$direct" "$direct" "$sni" "$user" "$upass" "$obfs_on" "$obfs_pw" "" direct
  else
    no "step6 取不到直连 Hysteria2 监听端口" "config.yaml 里没有 listen 行"
  fi

  # 住宅：4.1 起只有一个监听端口（不再有 config-residential*.yaml 可读），端口与整段跳跃
  # 从期望态取；认证串换成该用户那条**池凭据** `name:secret`（不是他的直连密码）。
  ports=$(resi_ports "$BASE/state.json")
  port=$(printf '%s\n' "$ports" | sed -n 1p)
  hop=$(printf '%s\n' "$ports" | sed -n 2p)
  rname=$(resi_cred_name "$BASE/state.json" "$user")
  rsec=$(resi_cred_secret "$BASE/state.json" "$user")
  if [ -z "$rname" ] || [ -z "$rsec" ]; then
    skip "step6 住宅 HY2（该用户还没有住宅凭据）"
    return 0
  fi
  hy2_auth_probe "住宅 :$port" "$port" "$sni" "$rname" "$rsec" "$obfs_on" "$obfs_pw" "" resi
  # 再打一次带 mport 的：**这一条专验 nft 的 output 链**（本机发往自身地址的包不过 prerouting）
  hy2_auth_probe "住宅 跳跃段 $hop（验 output 链）" "$port" "$sni" "$rname" "$rsec" \
    "$obfs_on" "$obfs_pw" "$hop" resi
  return 0
}

# step 7（spec §5.6）：IP 池槽位。端口**只从 bui 自己的 API 与期望态取**，
# 脚本不重算（重算就会有第二份口径）。
#
# 4.1 起槽位与对外端口彻底脱钩：住宅 HY2 只有 `ports.hy2_resi` 这一个监听端口，整段
# `ports.hy2_resi_hop` 与 4.0 兼容段由 `table inet bui` REDIRECT 进去，每个用户的端口与
# 区间完全相同。所以这里不再校「每槽一个 HY2 端口 / 按槽切的跳跃片段」——那两条已经
# 没有对象了；槽位只剩 relay 入站与出站那一头。
#
# 槽位表 JSON → 问题描述（空串 = 自洽）。抽成独立函数是为了让自测能直接喂数据：
# 不需要 $BUI、不碰 systemd（与 check_status_json 同形）。**多余字段一律忽略**：
# 升级过渡期可能读到 4.0 的 bui（那版还带 hy2_port / hop）。
slots_json_problems() {
  python3 - "$1" <<'PY'
import json, sys

# 整个函数体都在 try 里：任何形状意外都得**打印出来**（判 FAIL），不然 python 在打印
# 之前就崩了、stdout 为空 ⇒ 上层照样判 PASS（假绿）
try:
    d = json.loads(sys.argv[1]) or {}
    p = []
    if not isinstance(d, dict) or "slots" not in d:
        print("输出里没有 slots 键（bui 的输出形状变了）")
        raise SystemExit(0)
    rows = d["slots"] or []
    if not isinstance(rows, list):
        print("slots 不是数组（bui 的输出形状变了）")
        raise SystemExit(0)
    if not rows:
        print("")
        raise SystemExit(0)
    idx = [r.get("index") for r in rows]
    ports = [r.get("relay_port") for r in rows]
    if None in idx or None in ports:
        # 多余字段一律忽略，但**必需的那两个**缺了要报：4.0 的 bui 也带这两个字段
        p.append("槽位表缺 index/relay_port 字段（bui 的输出形状变了）")
    else:
        if 0 not in idx:
            p.append("没有槽 0（2080 / relay 的 slot-0 入站 / slot-0-out 会悬空）")
        if len(set(ports)) != len(ports):
            p.append("relay 端口有重复")
    borrow = [r.get("index") for r in rows if r.get("borrowed")]
    if borrow:
        p.append("槽 %s 正在借用别的 IP（不是错误，但验收时应为空）" % borrow)
    print("; ".join(p))
except SystemExit:
    raise
except Exception as e:
    print("slots JSON 解析失败: %s" % e)
PY
}

# 槽位表 JSON → 每行一个用户名（落在任何一个槽上的住宅用户，一个槽几个就几行）。
# 4.1 起不再带端口：住宅 HY2 的端口对全员相同，从期望态取（`resi_ports`）。
slots_users() {
  printf '%s' "$1" | python3 -c 'import json, sys
try:
    rows = (json.load(sys.stdin) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    for u in r.get("users") or []:
        print(u)'
}

# state.json → 两行：住宅 HY2 的监听端口 / 整段跳跃（`41000-50000`）。
# 读不到就回落 4.1 的期望态默认值（`bui_schema::model::Ports` 的 default）。
resi_ports() {
  python3 - "$1" <<'PY'
import json, sys
try:
    p = ((json.load(open(sys.argv[1])) or {}).get("node") or {}).get("ports") or {}
except Exception:
    p = {}
hop = p.get("hy2_resi_hop") or [41000, 50000]
print(p.get("hy2_resi") or 40000)
print("%s-%s" % (hop[0], hop[1]))
PY
}

# 订阅正文（`/api/sub/<token>` 的 base64 解出来的 URI 列表）→ `<住宅端口> <mport>`，
# 取不到的那项打 `-`。4.1 的核心承诺是这两个值**每个用户都一样**（不随槽位变），
# 所以它们得能被单独拎出来比。
#
# `HY2%E4%BD%8F%E5%AE%85` = URL 编码的「HY2住宅」（fragment 里那个 label，
# `bui_schema::nodes` 的字面量钉死，连空格连全半角都不许动）。userinfo 里的 `@` 与 `:`
# 都被 `enc()` 编成 %40 / %3A，所以 `rpartition` 一定切在真的分隔符上。
resi_sub_ports() {
  printf '%s' "$1" | python3 -c 'import sys
port = hop = "-"
for line in sys.stdin.read().splitlines():
    line = line.strip()
    if not line.startswith("hysteria2://") or "HY2%E4%BD%8F%E5%AE%85" not in line:
        continue
    body = line[len("hysteria2://"):].split("#")[0]
    hostpart, _, query = body.partition("?")
    p = hostpart.rpartition("@")[2].rpartition(":")[2]
    if p.isdigit():
        port = p
    for kv in query.split("&"):
        k, _, v = kv.partition("=")
        if k == "mport" and v:
            hop = v
    break
print(port, hop)'
}

# 4.0 遗留的「每槽一个监听端口」兼容段宽度 = `slots::MAX_SLOTS - 1`
# （`bui_schema::render::nft::compat_range` 是它的唯一实现，这里只是判据侧的同一个常数）。
RESI_COMPAT_SPAN=7

# `ss -lnu` 的输出 + 住宅端口 + 整段跳跃 → 「不该在听却在听」的端口（空 = 干净）。
# 跳跃段与兼容段上**没有任何进程监听**：包是 `table inet bui` REDIRECT 到住宅端口的。
# 这些端口上真冒出监听，说明 4.0 那些按槽起的 hysteria 实例还活着（对账没把遗留单元
# 停掉，`reconcile::LEGACY_UNITS` 那七项），那正是 4.1 要消灭的跨进程静默丢包。
stray_resi_listens() {
  printf '%s' "$1" | python3 -c 'import sys
base = int(sys.argv[1])
a, b = (int(x) for x in sys.argv[2].split("-"))
span = int(sys.argv[3])
bad = set()
for line in sys.stdin:
    f = line.split()
    if len(f) < 4 or f[0] == "State":
        continue
    p = f[3].rpartition(":")[2]
    if not p.isdigit():
        continue
    p = int(p)
    if p != base and (a <= p <= b or base < p <= base + span):
        bad.add(p)
print(" ".join(str(p) for p in sorted(bad)))' "$2" "$3" "$RESI_COMPAT_SPAN"
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

# 核对「槽位表自洽」、唯一那台住宅实例在跑且只在住宅端口上听、每个住宅用户的订阅里的
# HY2 住宅端口与 mport 都等于期望态那一对值（4.1：全员相同、与槽位无关）。
check_slots() {
  local slots out ports port hop listens stray user aport tok body got
  slots=$("$BUI" residential slots --json 2>/dev/null)
  if [ -z "$slots" ]; then
    skip "step7 槽位（bui residential slots 无输出，可能住宅未启用）"
    return 0
  fi
  out=$(slots_json_problems "$slots")
  if [ -z "$out" ]; then
    ok "step7 槽位表自洽（含槽 0、relay 端口不重复、无借用）"
  else
    no "step7 槽位表有问题" "$out"
  fi

  ports=$(resi_ports "$BASE/state.json")
  port=$(printf '%s\n' "$ports" | sed -n 1p)
  hop=$(printf '%s\n' "$ports" | sed -n 2p)

  # ① 住宅只剩**一个**单元、一个监听端口（受管单元数固定回 6，spec §2.5）
  if systemctl is-active --quiet hysteria-residential; then
    ok "step7 hysteria-residential 在跑"
  else
    no "step7 hysteria-residential 未运行" "$(systemctl is-active hysteria-residential 2>&1)"
  fi
  # ② 跳跃段与兼容段一个都不在听：包是 `table inet bui` REDIRECT 过来的。
  #    这些端口上冒出监听 = 4.0 那些按槽起的实例还活着 ⇒ 又是跨进程静默丢包。
  #    没有 ss 时两条一起 SKIP，不然「查不到监听」会假绿。
  if have ss; then
    listens=$(ss -lnu 2>/dev/null)
    if printf '%s\n' "$listens" | grep -q ":$port\b"; then
      ok "step7 hysteria-residential 监听 :$port/udp"
    else
      no "step7 hysteria-residential 没在听 :$port/udp" "$(printf '%s\n' "$listens" | head -5)"
    fi
    stray=$(stray_resi_listens "$listens" "$port" "$hop")
    if [ -z "$stray" ]; then
      ok "step7 跳跃段 $hop 与兼容段上没有任何监听（全靠 REDIRECT）"
    else
      no "step7 跳跃段/兼容段上有游离监听" "$stray（4.0 的住宅实例没被停掉？）"
    fi
  else
    skip "step7 监听端口核对（本机没有 ss）"
  fi

  # ③ 订阅口径一致：每个住宅用户的 HY2 住宅节点都是同一个端口 + 同一段 mport
  aport=$(admin_port "$BASE/state.json")
  while read -r user; do
    [ -n "$user" ] || continue
    tok=$(sub_token "$BASE/state.json" "$user")
    if [ -z "$tok" ]; then
      no "step7 $user 没有订阅 token" "state.json 的 users[] 里取不到 sub_token（守护进程启动时应已补齐）"
      continue
    fi
    # token 就是凭据（响应体里有 hy2 明文密码与 vless uuid），经 `-K -` 的 stdin 传，
    # 绝不进 argv（ps 会泄露，跟 step6 的 HY2 密码同一条规矩）；失败详情里也不带 token
    body=$(printf 'url = "http://127.0.0.1:%s/api/sub/%s"\n' "$aport" "$tok" |
      curl -fsS --max-time 10 -K - 2>/dev/null | base64 -d 2>/dev/null)
    if [ -z "$body" ]; then
      no "step7 $user 的订阅取不到" "GET http://127.0.0.1:$aport/api/sub/<token> 没有可解码的响应体"
      continue
    fi
    got=$(resi_sub_ports "$body")
    if [ "$got" = "$port $hop" ]; then
      ok "step7 $user 的订阅住宅端口 = $port、mport = $hop"
    elif [ "$got" = "- -" ]; then
      # 没开 hysteria2 权益、或还没分到住宅凭据（`nodes_for` 那时不发这个节点）⇒ 无对象可比
      skip "step7 $user 的订阅里没有 HY2住宅 节点（没开 hysteria2 或还没分到住宅凭据）"
    else
      no "step7 $user 的订阅住宅端口/mport 不对" "期望「$port $hop」，实际「$got」"
    fi
  done <<EOF
$(slots_users "$slots")
EOF
  return 0
}

# ---------------------------------------------------------------------------
# step 8（spec §2.4）：住宅端口跳跃那张 `table inet bui`。
#
# 4.1 的住宅只监听一个端口，整段跳跃与 4.0 兼容段全靠这张表 REDIRECT 进去 ⇒ 表没了
# 就等于所有带 `mport` 的现役订阅全部连不上。判据两条：
#   ① `bui nft status` 说表在位，且**实际规则数 == 它自己算的期望数**
#      （兼容段开着时是 4 条：两条链 × 跳跃段 + 兼容段）；
#   ② `nft list table inet bui` 里 **prerouting 与 output 两个 hook 都在** —— 只挂
#      prerouting 时本机发往自身地址的包不过它，step6 那条带 mport 的探测就是它的活体判据。
# ---------------------------------------------------------------------------

# `bui nft status` 的输出 → 它自己算的期望规则数（取不到就空串）
nft_rule_count() {
  printf '%s' "$1" | sed -n 's/.*期望 \([0-9][0-9]*\) 条规则.*/\1/p' | head -1
}

# `bui nft status` 的输出 → 问题描述（空串 = 表在位且规则数与期望一致）。
#
# 判据只有两条，而且都挂在**它自己算的**那个期望数上（`render::nft::rule_count`，
# 兼容段开着是 4、关掉是 2 —— 脚本不重算，不然就有第二份口径）：
#   ① 那句「期望 N 条规则」在 —— 表不在位时 `status` 打的是「不存在」那一行，没有这句；
#      `bui` 整个跑不起来时输出是错误原文，同样没有这句。
#   ② 实际列出的 `udp dport` 规则条数 == N。
nft_status_problems() {
  printf '%s' "$1" | python3 -c 'import re, sys
text = sys.stdin.read()
p = []
m = re.search("期望 ([0-9]+) 条规则", text)
if not m:
    p.append("表不在位或读不到期望规则数：%s" % (text.strip().splitlines() or [""])[0])
else:
    got = len([l for l in text.splitlines() if l.strip().startswith("udp dport")])
    if got != int(m.group(1)):
        p.append("规则数 %d ≠ 期望 %s" % (got, m.group(1)))
print("; ".join(p))'
}

# `nft list table inet bui` 的输出 → 问题描述（空串 = 两个 hook 都在）
nft_hook_problems() {
  local list=$1 missing=
  printf '%s' "$list" | grep -q "hook prerouting" || missing="prerouting"
  printf '%s' "$list" | grep -q "hook output" || missing="${missing:+$missing 与 }output"
  [ -z "$missing" ] || printf '缺 %s 链（少了 output，本机带 mport 打自己就不通）\n' "$missing"
}

check_nft() {
  local status_out list_out problems
  if ! have nft; then
    skip "step8 nft 表 inet bui（本机没有 nft）"
    return 0
  fi
  status_out=$("$BUI" nft status 2>&1)
  problems=$(nft_status_problems "$status_out")
  if [ -z "$problems" ]; then
    ok "step8 table inet bui 在位、$(nft_rule_count "$status_out") 条规则"
  else
    no "step8 table inet bui 不对" "$problems"
  fi
  list_out=$(nft list table inet bui 2>&1)
  problems=$(nft_hook_problems "$list_out")
  if [ -z "$problems" ]; then
    ok "step8 prerouting 与 output 两个 hook 都在"
  else
    no "step8 nft 表的 hook 不全" "$problems"
  fi
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

  # step 6：直连 + 住宅（含带 mport 的那条）真跑鉴权 + 出网（事故回归，见上面那段说明）
  check_hy2_auth

  # step 7：IP 池槽位（spec §5.6）
  check_slots

  # step 8：住宅端口跳跃那张 nft 表（spec §2.4）
  check_nft
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
  self_test_resi_log
  self_test_slots
  self_test_nft
}

# step 7 的自测：只喂 JSON 给纯判定函数，不需要 $BUI、不碰 systemd
self_test_slots() {
  local good bad out
  # 4.1：槽位表不再有 hy2_port / hop 的语义（住宅 HY2 与槽位无关），只校
  # 「有槽 0 + relay 端口不重复 + 不在借用中」三条
  good='{"slots":[{"index":0,"relay_port":2080,"borrowed":false,"users":["a"]},
                  {"index":1,"relay_port":2081,"borrowed":false,"users":[]},
                  {"index":2,"relay_port":2082,"borrowed":false,"users":["b"]}]}'
  bad='{"slots":[{"index":1,"relay_port":2081,"borrowed":true,"users":[]},
                 {"index":2,"relay_port":2081,"borrowed":false,"users":[]}]}'
  out=$(slots_json_problems "$good")
  if [ -z "$out" ]; then ok "自测：新形状槽位表自洽"; else no "自测：新形状被误判" "$out"; fi
  out=$(slots_json_problems "$bad")
  if [[ "$out" == *"没有槽 0"* && "$out" == *"relay 端口有重复"* && "$out" == *借用* ]]; then
    ok "自测：坏槽位表三项全报出"
  else
    no "自测：坏槽位表漏报" "$out"
  fi
  out=$(slots_json_problems '{"slots":[{"index":1,"relay_port":2081}]}')
  case "$out" in *"没有槽 0"*) ok "自测：缺槽 0 仍判失败" ;; *) no "自测：缺槽 0 没被判失败" "$out" ;; esac
  out=$(slots_json_problems '{"slots":[{"index":0,"relay_port":2080},{"index":1,"relay_port":2080}]}')
  case "$out" in *"relay 端口有重复"*) ok "自测：relay 端口重复判失败" ;; *) no "自测：端口重复没被判失败" "$out" ;; esac
  # 旧形状（带 hy2_port / hop）也不许让脚本崩：升级过渡期可能读到 4.0 的 bui
  out=$(slots_json_problems '{"slots":[{"index":0,"relay_port":2080,"hy2_port":40000,"hop":[41000,50000]}]}')
  if [ -z "$out" ]; then ok "自测：多余字段被忽略"; else no "自测：多余字段让脚本报错" "$out"; fi
  # 必需的那两个字段缺了要**报出来**：`r["index"]` 会让 python 在打印之前崩掉、
  # stdout 为空 ⇒ 上层照样判 PASS（假绿）
  out=$(slots_json_problems '{"slots":[{"index":0,"relayPort":2080}]}')
  case "$out" in
    *"缺 index/relay_port"*) ok "自测：槽位表缺必需字段判失败（不是静默判绿）" ;;
    *) no "自测：缺必需字段没被判失败" "$out" ;;
  esac
  out=$(slots_json_problems '{"rows":[{"index":0,"relay_port":2080}]}')
  case "$out" in
    *"没有 slots 键"*) ok "自测：顶层键改名判失败" ;;
    *) no "自测：顶层键改名被判通过" "$out" ;;
  esac
  out=$(slots_json_problems '{"slots":[]}')
  if [ -z "$out" ]; then ok "自测：空槽位表不报错（住宅未启用）"; else no "自测：空槽位表被误判" "$out"; fi
  out=$(slots_users "$good")
  if [ "$out" = "a
b" ]; then ok "自测：slots_users 给出落在槽上的用户名"; else no "自测：slots_users 结果不对" "$out"; fi
  self_test_resi_ports
  self_test_resi_sub_ports
  self_test_stray_listens
  self_test_sub_token
  self_test_check_slots
}

# step 7 的自测（续）：住宅端口与整段跳跃只从 state.json 的 node.ports 取，脚本不重算
self_test_resi_ports() {
  local f out
  f=$(mktemp) || { no "自测：建不出临时文件"; return; }
  printf '%s\n' '{"node":{"ports":{"hy2_resi":45000,"hy2_resi_hop":[46000,47000]}}}' > "$f"
  out=$(resi_ports "$f")
  if [ "$out" = "45000
46000-47000" ]; then ok "自测：resi_ports 读期望态的端口与整段"; else no "自测：resi_ports 结果不对" "$out"; fi
  out=$(resi_ports "$f/nope")
  if [ "$out" = "40000
41000-50000" ]; then ok "自测：读不到 state.json 时回落 4.1 默认端口"; else no "自测：默认端口不对" "$out"; fi
  rm -f "$f"
}

# step 7 的自测（续）：订阅正文 → 住宅 HY2 的端口与 mport。
# 4.1 的核心承诺就是这两个值**每个用户都一样**，所以它们得能被单独拎出来比。
self_test_resi_sub_ports() {
  local body out
  # `HY2%E4%BD%8F%E5%AE%85` = URL 编码的「HY2住宅」（enc() 编的 fragment）
  body='hysteria2://alice:pw@example.com:10000?sni=example.com&insecure=0&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E
hysteria2://r007:c2VjcmV0@example.com:40000?sni=example.com&insecure=0&mport=41000-50000&obfs=salamander&obfs-password=x#alice-HY2%E4%BD%8F%E5%AE%85
vless://11111111-1111-4111-8111-111111111111@example.com:10002?security=reality#alice-Reality%E4%BD%8F%E5%AE%85'
  out=$(resi_sub_ports "$body")
  if [ "$out" = "40000 41000-50000" ]; then
    ok "自测：从订阅里取到住宅端口与 mport（不会跟直连那条混）"
  else
    no "自测：订阅住宅端口/mport 解析不对" "$out"
  fi
  out=$(resi_sub_ports "$(printf '%s\n' "$body" | grep -v 'HY2%E4%BD%8F%E5%AE%85')")
  if [ "$out" = "- -" ]; then ok "自测：订阅里没有住宅节点 → 两项都是 -"; else no "自测：没有住宅节点却有值" "$out"; fi
  out=$(resi_sub_ports 'hysteria2://r007:s@example.com:40000?sni=example.com&insecure=0#a-HY2%E4%BD%8F%E5%AE%85')
  if [ "$out" = "40000 -" ]; then ok "自测：住宅节点丢了 mport → mport 是 -"; else no "自测：缺 mport 没被报出" "$out"; fi
}

# step 7 的自测（续）：跳跃段与兼容段上**不许有任何监听**（包是 REDIRECT 过来的）
self_test_stray_listens() {
  local clean dirty out
  clean='State  Recv-Q Send-Q Local Address:Port  Peer Address:Port
UNCONN 0      0            0.0.0.0:40000      0.0.0.0:*
UNCONN 0      0               [::]:40000         [::]:*
UNCONN 0      0          127.0.0.53%lo:53         0.0.0.0:*'
  dirty="$clean
UNCONN 0      0            0.0.0.0:40003      0.0.0.0:*
UNCONN 0      0               [::]:41234         [::]:*"
  out=$(stray_resi_listens "$clean" 40000 41000-50000)
  if [ -z "$out" ]; then ok "自测：只有 :40000 在听 → 无游离监听"; else no "自测：干净的 ss 输出被误判" "$out"; fi
  out=$(stray_resi_listens "$dirty" 40000 41000-50000)
  if [ "$out" = "40003 41234" ]; then
    ok "自测：兼容段与跳跃段上的监听都被报出"
  else
    no "自测：游离监听没被报全" "$out"
  fi
}

# step 8 的自测：`bui nft status` 与 `nft list table` 的输出都只过纯判定函数
self_test_nft() {
  local good out
  good='table inet bui 在位，期望 4 条规则
  udp dport 41000-50000 counter packets 12 bytes 1440 redirect to :40000 comment "hy2 residential hop"
  udp dport 40001-40007 counter packets 0 bytes 0 redirect to :40000 comment "hy2 residential 4.0 compat"
  udp dport 41000-50000 counter packets 3 bytes 360 redirect to :40000 comment "hy2 residential hop (local)"
  udp dport 40001-40007 counter packets 0 bytes 0 redirect to :40000 comment "hy2 residential 4.0 compat (local)"
（counter 是瞬时流计数：每次重放都从 0 开始，兼容段的下线判据看 `bui status` 里的累计值）'
  out=$(nft_status_problems "$good")
  if [ -z "$out" ]; then ok "自测：表在位且 4 条规则齐 → 判通过"; else no "自测：健康的 nft status 被误判" "$out"; fi
  out=$(nft_status_problems 'table inet bui 不存在（住宅 HY2 的端口跳跃整段不通）：执行 `bui nft apply`')
  case "$out" in *不在位*) ok "自测：表不存在判失败" ;; *) no "自测：表不存在没被判失败" "$out" ;; esac
  out=$(nft_status_problems 'Error: 连不上 /run/b-ui.sock')
  case "$out" in *不在位*) ok "自测：bui 自己都没输出规则数 → 判失败" ;; *) no "自测：错误输出没被判失败" "$out" ;; esac
  out=$(nft_status_problems "$(printf '%s\n' "$good" | grep -v 'compat (local)')")
  case "$out" in *"规则数 3"*) ok "自测：少一条规则判失败" ;; *) no "自测：缺规则没被判失败" "$out" ;; esac
  out=$(nft_rule_count "$good")
  if [ "$out" = "4" ]; then ok "自测：nft_rule_count 取到期望规则数"; else no "自测：nft_rule_count 不对" "$out"; fi

  # 双 hook 是硬要求：只挂 prerouting 时本机发往自身地址的包不过它，跳跃对本机失效
  good='table inet bui {
	chain prerouting {
		type nat hook prerouting priority dstnat; policy accept;
		udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop"
	}
	chain output {
		type nat hook output priority -100; policy accept;
		udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop (local)"
	}
}'
  out=$(nft_hook_problems "$good")
  if [ -z "$out" ]; then ok "自测：两个 hook 都在 → 判通过"; else no "自测：两个 hook 都在却被误判" "$out"; fi
  out=$(nft_hook_problems "$(printf '%s\n' "$good" | grep -v 'hook output')")
  case "$out" in *output*) ok "自测：缺 output 链判失败（本机带 mport 打自己就不通了）" ;; *) no "自测：缺 output 没被判失败" "$out" ;; esac
  out=$(nft_hook_problems "$(printf '%s\n' "$good" | grep -v 'hook prerouting')")
  case "$out" in *prerouting*) ok "自测：缺 prerouting 链判失败" ;; *) no "自测：缺 prerouting 没被判失败" "$out" ;; esac
  self_test_check_nft
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

# step 6 的自测：hysteria 客户端、curl 与 journald 全换成 fake，只验
# 「取凭据（直连密码 / 池凭据两套）/ 取端口 / 三条探测的配置 / 判定 / SKIP」
self_test_hy2_auth() {
  local d out
  d=$(mktemp -d) || { no "自测：建不出临时目录"; return; }
  mkdir -p "$d/bin"
  printf '#!/bin/sh\nexit 0\n' > "$d/bin/hysteria"
  chmod 755 "$d/bin/hysteria"
  printf 'listen: :10000,20000-30000\nauth:\n  type: command\n  command: /opt/b-ui/bin/bui-auth-hook\n' > "$d/config.yaml"
  printf '2026-09-12T09:00:00Z 1.2.3.4:51820 alice allow\n' > "$d/auth-hook.log"
  # 4.1 的 state.json：住宅端口在期望态里（没有 config-residential.yaml 可读了），
  # 住宅凭据在 residential.hy2_pool 里、用户经 credentials.hy2_resi_cred 指向其中一条
  self_test_hy2_state "$d" ''

  out=$(hy2_probe_creds "$d/state.json")
  if [ "$out" = "alice
pw:with:colons
panel.example.com
0" ]; then
    ok "自测：跳过禁用用户，取第一个可用用户的凭据与域名"
  else
    no "自测：凭据解析不对" "$out"
  fi
  if [ "$(listen_port "$d/config.yaml")" = "10000" ]; then
    ok "自测：从直连配置的 listen 行取到端口（带端口跳跃区间也只取第一个数）"
  else
    no "自测：listen 端口解析不对" "$(listen_port "$d/config.yaml")"
  fi
  out=$(resi_cred_name "$d/state.json" alice)/$(resi_cred_secret "$d/state.json" alice)
  if [ "$out" = "r007/c2VjcmV0LTIyLWNoYXJz" ]; then
    ok "自测：住宅凭据从池里按 hy2_resi_cred 取到（name 与 secret 两半）"
  else
    no "自测：住宅凭据解析不对" "$out"
  fi
  out=$(resi_cred_name "$d/state.json" ghost)
  if [ -z "$out" ]; then ok "自测：没有 hy2_resi_cred 的用户返回空串"; else no "自测：凭空造了条住宅凭据" "$out"; fi

  # ① 通过：三条探测各一条 PASS（没开混淆 ⇒ 探测配置里没有 obfs 段）
  out=$(self_test_hy2_run "$d" 200 hit)
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] && [[ "$out" == *allow* ]] &&
    ! grep -q obfs "$d/seen.yaml"; then
    ok "自测：三条探测都通过 → 三条 PASS"
  else
    no "自测：鉴权通了却没判通过" "$out"
  fi
  # ①b 三条探测的 server 行与 auth 串：直连用 hy2_password，住宅两条用池凭据，
  #     第三条把整段跳跃写进 server（`:40100,45000-46000` = 订阅里的 mport=）。
  #     `40100` / `45000-46000` 是夹具里的**非默认**端口（见 `self_test_hy2_state`）⇒
  #     住宅那两条要是写死了期望态默认值，这里当场转红。
  if [ "$(grep -c '^server: 127.0.0.1:10000$' "$d/seen.yaml")" = "1" ] &&
    [ "$(grep -c '^server: 127.0.0.1:40100$' "$d/seen.yaml")" = "1" ] &&
    [ "$(grep -c '^server: 127.0.0.1:40100,45000-46000$' "$d/seen.yaml")" = "1" ] &&
    [ "$(grep -c '^auth: "alice:pw:with:colons"$' "$d/seen.yaml")" = "1" ] &&
    [ "$(grep -c '^auth: "r007:c2VjcmV0LTIyLWNoYXJz"$' "$d/seen.yaml")" = "2" ]; then
    ok "自测：住宅两条用池凭据、其中一条带整段跳跃（验 output 链）"
  else
    no "自测：三条探测的 server / auth 不对" "$(cat "$d/seen.yaml")"
  fi

  # ①c 开了混淆：三条探测的客户端配置都带同一段 salamander（混淆覆盖直连与住宅）
  self_test_hy2_state "$d" '"obfs":{"enabled":true,"password":"obfs-pw"},'
  out=$(self_test_hy2_run "$d" 200 hit)
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] &&
    [ "$(grep -c '^  type: salamander$' "$d/seen.yaml")" = "3" ] &&
    [ "$(grep -c '^    password: "obfs-pw"$' "$d/seen.yaml")" = "3" ]; then
    ok "自测：开了混淆 → 三条探测都带 obfs"
  else
    no "自测：开了混淆，探测配置没带齐 obfs" "$out"
  fi

  # ② 失败：出不了网（事故当天的形态——钩子没被调用，客户端 404）
  out=$(self_test_hy2_run "$d" 000 hit)
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "3" ] && [[ "$out" == *blocked* ]]; then
    ok "自测：出不了网判失败（住宅那两条点名 blocked 的可能）"
  else
    no "自测：出不了网没被判失败" "$out"
  fi

  # ②b 住宅特有的失败形态：出网通了，但住宅单元日志里没有那条凭据的连接行
  #     （门被切到 deny、或 log.level 掉到 info 以下）⇒ 只有直连那条该 PASS
  out=$(self_test_hy2_run "$d" 200 miss)
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "2" ]; then
    ok "自测：住宅日志没有该凭据的连接行 → 住宅两条判失败"
  else
    no "自测：住宅日志缺连接行没被判失败" "$out"
  fi

  # ③ 该用户还没分到住宅凭据 → 直连照旧跑，住宅那两条 SKIP
  self_test_hy2_state "$d" '' nocred
  out=$(self_test_hy2_run "$d" 200 hit)
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ]; then
    ok "自测：没有住宅凭据 → 直连 PASS + 住宅 SKIP"
  else
    no "自测：缺住宅凭据该 SKIP" "$out"
  fi

  # ④ 没有可用用户 → SKIP（新装机还没加用户时不该报红）
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

# step 6 自测用的 state.json：$1 = 目录，$2 = 塞进 node 的额外片段（obfs），
# $3 非空则那个用户**没有** hy2_resi_cred（还没分到住宅凭据）
#
# 住宅端口 `40100` 与整段 `45000-46000` 都是**故意的非默认值**（期望态默认是 40000 与
# 41000-50000）。裁决是「住宅端口与整段跳跃只从期望态 node.ports 取」，夹具要是用默认值，
# 把 `check_hy2_auth` 里那句 `resi_ports` 换成写死的 40000 / 41000-50000 照样全绿 ——
# 非默认值一上，①b 那两条 `server:` 断言就成了这条裁决的活体判据。
self_test_hy2_state() {
  local cred='"hy2_resi_cred":"r007",'
  [ -z "${3:-}" ] || cred=
  printf '%s\n' \
    "{\"node\":{\"domain\":\"panel.example.com\",$2\"ports\":{\"hy2_resi\":40100,\"hy2_resi_hop\":[45000,46000]}}," \
    ' "residential":{"hy2_pool":{"creds":[{"id":"r000","name":"bob","secret":"c2VjcmV0LWJvYi0yMmNo"},' \
    '                                     {"id":"r007","name":"r007","secret":"c2VjcmV0LTIyLWNoYXJz"}]}},' \
    ' "users":[{"username":"ghost","disabled":true,"credentials":{"hy2_password":"x"}},' \
    "          {\"username\":\"alice\",\"disabled\":false,\"credentials\":{$cred\"hy2_password\":\"pw:with:colons\"}}]}" \
    > "$1/state.json"
}

# step 6 自测用的一趟 check_hy2_auth：$1 = 目录、$2 = 假状态码、
# $3 = `hit`（住宅日志里有那条凭据的连接行）/ 其它（没有）。
# 真客户端、真 curl、真 journalctl 全在子 shell 里被顶掉，**一个系统调用都不发**。
self_test_hy2_run() {
  # 变量名全带 st_ 前缀：`hy2_auth_probe` 自己有 `local code` / `local line`，
  # 同名的话它那份（此刻还未赋值）会把这里的值遮掉，`set -u` 当场报未绑定
  local st_dir=$1 st_code=$2 st_mode=$3
  rm -f "$st_dir/seen.yaml"
  # shellcheck disable=SC2317  # 这几个函数由 check_hy2_auth 间接调用，shellcheck 看不到
  (
    BASE=$st_dir
    start_hy2_client() {
      cat "$1" >> "$st_dir/seen.yaml"
      # `> /dev/null` 不能省：这个函数是在 `pid=$( )` 里跑的，后台进程继承着那根管子
      # 不放，命令替换就得等它自己退（每条探测白等 5 秒，13 条 = 一分多钟）
      sleep 5 > /dev/null 2>&1 &
      echo $!
    }
    probe_socks_code() { echo "$st_code"; }
    resi_log_hit() {
      [ "$st_mode" = hit ] || return 0
      printf '+0800 2026-09-17 03:48:11 INFO [3493080625 0ms] inbound/hysteria2[hy2-resi]: [%s] inbound connection to www.example.com:443\n' "$2"
    }
    check_hy2_auth
  )
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

# step 6 的自测（续）：住宅那条判据的**提取逻辑**。它是 4.1 住宅路径唯一的成功证据
# （sing-box 对 hysteria2 鉴权失败一行都不打），写错了两道门禁都看不出来，所以载荷字符串、
# 单元名与 `--since` 时间窗三样都得钉住。日志原文逐字取自
# `crates/bui/src/modules/sentinel/fixtures_hy2_resi.rs` 的 CONN_TO_USER /
# CONN_TO_USER_UDP / CONN_FROM —— 那边重采了这里要跟着改。
self_test_resi_log() {
  local tcp udp from other old out
  tcp='+0800 2026-09-17 03:48:11 INFO [3493080625 0ms] inbound/hysteria2[hy2-resi]: [alice] inbound connection to www.example.com:443'
  udp='+0800 2026-09-17 03:48:50 INFO [620844294 0ms] inbound/hysteria2[hy2-resi]: [alice] inbound packet connection to 198.51.100.53:53'
  from='+0800 2026-09-17 03:48:11 INFO [3493080625 0ms] inbound/hysteria2[hy2-resi]: inbound connection from 203.0.113.10:55076'
  other=${tcp/\[alice\]/[bob]}
  out=$(printf '%s\n' "$from" "$tcp" | resi_log_pick alice)
  if [ "$out" = "$tcp" ]; then ok "自测：TCP 连接行（真原文）被认出"; else no "自测：TCP 连接行没被认出" "$out"; fi
  out=$(printf '%s\n' "$from" "$udp" | resi_log_pick alice)
  if [ "$out" = "$udp" ]; then
    ok "自测：UDP 连接行也被认出（载荷不许收窄成 connection to）"
  else
    no "自测：UDP 连接行没被认出" "$out"
  fi
  out=$(printf '%s\n' "$tcp" "$udp" | resi_log_pick alice)
  if [ "$out" = "$udp" ]; then ok "自测：多条命中取最后一条"; else no "自测：没取最后一条" "$out"; fi
  out=$(printf '%s\n' "$other" | resi_log_pick alice)
  if [ -z "$out" ]; then
    ok "自测：别人的连接行不算（判据必须带这条凭据的 name）"
  else
    no "自测：认了别人的连接行" "$out"
  fi
  out=$(printf '%s\n' "$from" | resi_log_pick alice)
  if [ -z "$out" ]; then ok "自测：没有方括号的握手行不算（那时还没鉴权）"; else no "自测：认了握手行" "$out"; fi
  out=$(printf '' | resi_log_pick alice)
  if [ -z "$out" ]; then ok "自测：空日志 → 空串"; else no "自测：空日志却有输出" "$out"; fi

  # 时间窗：夹具里一条探测之前的旧行 + 一条窗口内的新行。少了 `--since`，该凭据历史上
  # 任何一次成功连接都会让这条判据从此永远为真（单元名写错则一行都取不到）。
  old=${tcp/03:48:11/03:00:00}
  out=$(self_test_journal_run '2026-09-17 03:40:00' "$old" "$tcp")
  if [ "$out" = "$tcp" ]; then ok "自测：窗口内的新连接行被取到"; else no "自测：窗口内的新行没被取到" "$out"; fi
  out=$(self_test_journal_run '2026-09-17 03:40:00' "$old")
  if [ -z "$out" ]; then
    ok "自测：只有探测之前的旧行 → 空串（--since 不许省）"
  else
    no "自测：探测之前的旧行被当成了本次成功" "$out"
  fi
}

# 自测用的 journalctl 桩 + 一次 resi_log_hit：$1 = 探测起点（`--since` 的实参），
# 其余参数是 journald 里的行。桩只在 `-u hysteria-residential` 时出货，且只出时间戳
# 晚于 `--since` 的行 ⇒ 单元名与时间窗都被钉住。
self_test_journal_run() {
  local st_since=$1
  shift
  # shellcheck disable=SC2317  # journalctl 由 resi_log_hit 间接调用，shellcheck 看不到
  (
    st_lines=("$@")
    journalctl() {
      local unit='' since='' line ts
      while [ "$#" -gt 0 ]; do
        case $1 in
          -u) unit=${2:-}; shift 2 ;;
          --since) since=${2:-}; shift 2 ;;
          *) shift ;;
        esac
      done
      [ "$unit" = hysteria-residential ] || return 0
      for line in "${st_lines[@]}"; do
        ts=$(printf '%s' "$line" | cut -d' ' -f2,3)
        if [ -z "$since" ] || [[ "$ts" > "$since" ]]; then
          printf '%s\n' "$line"
        fi
      done
    }
    resi_log_hit "$st_since" alice
  )
}

# 自测用的 $BUI 桩：`residential slots --json` 与 `nft status` 的输出各从一个文件取，
# 自测按形态改那两个文件。`"$BUI"` 是变量展开，shell 函数顶不掉 —— 只能桩成一个真脚本
# （与 $BASE/bin/hysteria 同法）。
self_test_bui_stub() {
  mkdir -p "$1/bin"
  cat > "$1/bin/bui" <<'EOF'
#!/bin/sh
case "$1 $2" in
  'residential slots') cat "$(dirname "$0")/../slots.json" ;;
  'nft status') cat "$(dirname "$0")/../nft-status.txt" ;;
  *) echo "bui 桩收到意外的子命令: $*" >&2; exit 64 ;;
esac
EOF
  chmod 755 "$1/bin/bui"
}

# step 7 自测用的 state.json：$1 = 目录，$2 = users[0] 里 sub_token 那个键值对（空 = 没有
# token）。住宅端口 / 整段跳跃 / 面板端口全在期望态里，脚本不重算。
#
# 与 step 6 的夹具同一理由：`40100` / `45000-46000` 是**故意的非默认值**，`check_slots`
# 里那句 `resi_ports` 换成写死的 40000 / 41000-50000 时，下面 ① 的监听判据与订阅口径
# 判据会一起转红（夹具用默认值的话两道门禁照样全绿）。
self_test_slots_state() {
  printf '%s\n' \
    '{"node":{"ports":{"hy2_resi":40100,"hy2_resi_hop":[45000,46000],"admin":9876}},' \
    " \"users\":[{\"username\":\"alice\"${2:+,$2}}]}" \
    > "$1/state.json"
}

# step 7 的**编排级**自测：$BUI / systemctl / ss / curl 全在子 shell 里顶掉，验的是
# check_slots 自己那几条判据接没接上、以及 PASS / FAIL / SKIP 的分派。两层都得有：
# 判据函数写对了但没接上、或 SKIP 被写成 ok，只有这一层抓得到。
#   $1 = 目录、$2 = `ss -lnu` 的输出（`-` = 本机没有 ss）、$3 = 订阅正文（`-` = 取不到）、
#   $4 = 住宅单元在不在跑（默认 active）
self_test_slots_run() {
  local st_dir=$1 st_ss=$2 st_body=$3 st_active=${4:-active} st_b64 st_want
  st_b64=$(printf '%s\n' "$st_body" | base64 | tr -d '\n')
  # 订阅得按 users[].sub_token 取、走面板端口、且 token 只经 stdin（`-K -`，不进 argv）：
  # 桩只认逐字相同的这一行配置，端口或末段写错了就取不到订阅
  st_want='url = "http://127.0.0.1:9876/api/sub/0123456789abcdef0123456789abcdef"'
  # shellcheck disable=SC2317  # 这几个函数由 check_slots 间接调用，shellcheck 看不到
  (
    BASE=$st_dir
    BUI=$st_dir/bin/bui
    have() { [ "$1" != ss ] || [ "$st_ss" != - ]; }
    systemctl() {
      [ "$st_active" = active ] && return 0
      [ "${2:-}" = --quiet ] || printf 'inactive\n'
      return 3
    }
    ss() { [ "$st_ss" = - ] || printf '%s\n' "$st_ss"; }
    curl() {
      [ "$(cat)" = "$st_want" ] || return 22
      [ "$st_body" != - ] || return 22
      printf '%s' "$st_b64"
    }
    check_slots
  )
}

self_test_check_slots() {
  local d clean stray body out
  d=$(mktemp -d) || { no "自测：建不出临时目录"; return; }
  self_test_bui_stub "$d"
  printf '%s\n' '{"slots":[{"index":0,"relay_port":2080,"borrowed":false,"users":["alice"]}]}' > "$d/slots.json"
  self_test_slots_state "$d" '"sub_token":"0123456789abcdef0123456789abcdef"'
  clean='State  Recv-Q Send-Q Local Address:Port  Peer Address:Port
UNCONN 0      0            0.0.0.0:40100      0.0.0.0:*
UNCONN 0      0               [::]:40100         [::]:*'
  stray="$clean
UNCONN 0      0               [::]:45234         [::]:*"
  # 订阅正文（base64 解出来的那串 URI）：直连那条也在，判据不许跟它混
  body='hysteria2://alice:pw@example.com:10000?sni=example.com&insecure=0&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E
hysteria2://r007:c2VjcmV0@example.com:40100?sni=example.com&insecure=0&mport=45000-46000#alice-HY2%E4%BD%8F%E5%AE%85'

  # ① 健康：槽位表自洽 / 单元在跑 / 只在住宅端口上听 / 跳跃段无监听 / 订阅口径一致。
  #    夹具里那对端口是非默认值 ⇒ 住宅端口与整段跳跃要是写死了默认值，这一条当场转红。
  out=$(self_test_slots_run "$d" "$clean" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "5" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ] &&
    [[ "$out" == *"住宅端口 = 40100、mport = 45000-46000"* ]]; then
    ok "自测：健康机器 → step7 五条 PASS"
  else
    no "自测：健康机器的 step7 没全绿" "$out"
  fi
  # ② 跳跃段上冒出监听 = 4.0 那些按槽起的实例还活着 ⇒ 跨进程静默丢包
  out=$(self_test_slots_run "$d" "$stray" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *游离监听* ]] &&
    [[ "$out" == *45234* ]]; then
    ok "自测：跳跃段上冒出监听 → 判失败"
  else
    no "自测：游离监听没被判失败" "$out"
  fi
  # ③ 订阅里的 mport 与期望态不符（改过端口却没刷订阅）
  out=$(self_test_slots_run "$d" "$clean" "${body//45000-46000/45000-45999}")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"mport 不对"* ]]; then
    ok "自测：订阅的 mport 与期望态不符 → 判失败"
  else
    no "自测：订阅 mport 不符没被判失败" "$out"
  fi
  # ④ 订阅里压根没有 HY2住宅 节点（没开 hysteria2 或还没分到住宅凭据）→ 无对象可比 ⇒ SKIP
  out=$(self_test_slots_run "$d" "$clean" "$(printf '%s\n' "$body" | grep -v 'HY2%E4%BD%8F%E5%AE%85')")
  if [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ] && [[ "$out" == *"没有 HY2住宅 节点"* ]]; then
    ok "自测：订阅里没有住宅节点 → SKIP（不是 FAIL）"
  else
    no "自测：没有住宅节点该 SKIP" "$out"
  fi
  # ⑤ 用户没有 sub_token（守护进程启动时该补齐）→ 订阅根本取不了 ⇒ 判失败
  self_test_slots_state "$d" ''
  out=$(self_test_slots_run "$d" "$clean" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"没有订阅 token"* ]]; then
    ok "自测：用户缺 sub_token → 判失败"
  else
    no "自测：缺 sub_token 没被判失败" "$out"
  fi
  self_test_slots_state "$d" '"sub_token":"0123456789abcdef0123456789abcdef"'
  # ⑥ 订阅取不到（面板没起来 / 端口不对 / token 不认）
  out=$(self_test_slots_run "$d" "$clean" -)
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"订阅取不到"* ]]; then
    ok "自测：订阅取不到 → 判失败"
  else
    no "自测：订阅取不到没被判失败" "$out"
  fi
  # ⑦ 本机没有 ss：两条监听判据必须一起 SKIP —— 写成 ok 就是代码注释里点名要防的假绿
  out=$(self_test_slots_run "$d" - "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "3" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ] && [[ "$out" == *"没有 ss"* ]]; then
    ok "自测：本机没有 ss → 监听核对 SKIP（不许算通过）"
  else
    no "自测：没有 ss 却不是 SKIP" "$out"
  fi
  # ⑧ 住宅单元没在跑
  out=$(self_test_slots_run "$d" "$clean" "$body" inactive)
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *未运行* ]]; then
    ok "自测：hysteria-residential 没在跑 → 判失败"
  else
    no "自测：单元没在跑却没判失败" "$out"
  fi
  # ⑨ 住宅端口上压根没人听（单元起着但监听没落，或端口被改过）
  out=$(self_test_slots_run "$d" "$(printf '%s\n' "$clean" | grep -v 40100)" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"没在听 :40100/udp"* ]]; then
    ok "自测：住宅端口上没有监听 → 判失败"
  else
    no "自测：住宅端口没人听却没判失败" "$out"
  fi
  # ⑩ 槽位表自己就不自洽（缺槽 0 ⇒ 2080 / slot-0 入站 / slot-0-out 全悬空）
  printf '%s\n' '{"slots":[{"index":1,"relay_port":2081,"borrowed":false,"users":[]}]}' > "$d/slots.json"
  out=$(self_test_slots_run "$d" "$clean" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"槽位表有问题"* ]] &&
    [[ "$out" == *"没有槽 0"* ]]; then
    ok "自测：槽位表不自洽 → 判失败（判据真接上了 slots_json_problems）"
  else
    no "自测：槽位表不自洽却没判失败" "$out"
  fi
  # ⑪ bui 一个字都没输出（住宅没启用）→ 整步 SKIP
  : > "$d/slots.json"
  out=$(self_test_slots_run "$d" "$clean" "$body")
  if [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "0" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ]; then
    ok "自测：bui residential slots 无输出 → 整步 SKIP"
  else
    no "自测：住宅未启用该整步 SKIP" "$out"
  fi
  rm -rf "$d"
}

# step 8 的**编排级**自测：$BUI 与 nft 都是桩。
#   $1 = 目录、$2 = `nft list table inet bui` 的输出（`-` = 本机没有 nft）
self_test_nft_run() {
  local st_dir=$1 st_list=$2
  # shellcheck disable=SC2317  # 这两个函数由 check_nft 间接调用，shellcheck 看不到
  (
    BASE=$st_dir
    BUI=$st_dir/bin/bui
    have() { [ "$1" != nft ] || [ "$st_list" != - ]; }
    nft() { [ "$st_list" = - ] || printf '%s\n' "$st_list"; }
    check_nft
  )
}

self_test_check_nft() {
  local d status list out
  d=$(mktemp -d) || { no "自测：建不出临时目录"; return; }
  self_test_bui_stub "$d"
  # 故意用**兼容段已关掉**那台机器的形状（`bui set hy2-resi-compat off` 之后期望 2 条）：
  # 判据比的是 bui 自己算的期望数，写死 4 的话这台合法机器当场假红
  status='table inet bui 在位，期望 2 条规则
  udp dport 41000-50000 counter packets 12 bytes 1440 redirect to :40000 comment "hy2 residential hop"
  udp dport 41000-50000 counter packets 3 bytes 360 redirect to :40000 comment "hy2 residential hop (local)"'
  # 两个 hook 的完整原文在 self_test_nft 里，这里只留判据认的那两行
  list='	chain prerouting { type nat hook prerouting priority dstnat; policy accept; }
	chain output { type nat hook output priority -100; policy accept; }'
  printf '%s\n' "$status" > "$d/nft-status.txt"
  out=$(self_test_nft_run "$d" "$list")
  if [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "2" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ] && [[ "$out" == *"2 条规则"* ]]; then
    ok "自测：表在位 + 两个 hook 都在 → step8 两条 PASS（规则数按 bui 自己算的期望比）"
  else
    no "自测：健康的 step8 没全绿" "$out"
  fi
  # 表不在位：所有带 mport 的现役订阅整段连不上
  printf '%s\n' 'table inet bui 不存在（住宅 HY2 的端口跳跃整段不通）：执行 `bui nft apply`' > "$d/nft-status.txt"
  out=$(self_test_nft_run "$d" "$list")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *不在位* ]]; then
    ok "自测：表不在位 → step8 第一条判失败"
  else
    no "自测：表不在位没被判失败" "$out"
  fi
  # 实际规则数比它自己算的期望数少一条
  printf '%s\n' "$(printf '%s\n' "$status" | grep -v 'hop (local)')" > "$d/nft-status.txt"
  out=$(self_test_nft_run "$d" "$list")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *"规则数 1"* ]]; then
    ok "自测：规则数与期望不符 → 判失败"
  else
    no "自测：规则数不符没被判失败" "$out"
  fi
  printf '%s\n' "$status" > "$d/nft-status.txt"
  # 只挂了 prerouting：本机发往自身地址的包不过它 ⇒ step6 那条带 mport 的探测的静态对照
  out=$(self_test_nft_run "$d" "$(printf '%s\n' "$list" | grep -v 'hook output')")
  if [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "1" ] && [[ "$out" == *output* ]]; then
    ok "自测：缺 output 链 → step8 第二条判失败"
  else
    no "自测：缺 output 链没被判失败" "$out"
  fi
  # 本机没有 nft：必须整步 SKIP，不许算通过
  out=$(self_test_nft_run "$d" -)
  if [ "$(printf '%s\n' "$out" | grep -c '^SKIP')" = "1" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^PASS')" = "0" ] &&
    [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = "0" ]; then
    ok "自测：本机没有 nft → 整步 SKIP（不许算通过）"
  else
    no "自测：没有 nft 却不是 SKIP" "$out"
  fi
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
