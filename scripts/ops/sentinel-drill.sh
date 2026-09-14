#!/usr/bin/env bash
# b-ui v4 日志哨兵真机演练（spec §5.7）。在生产机（先 bwg-rick）上以 root 运行，两种模式：
#
# 【缺省：单端口】临时丢弃发往**某一个**住宅上游（该上游的 IP:端口，只动 TCP）的流量 —— 生产上最
# 常见的故障形态（同一网关的兄弟端口还活着，每个端口一个出口 IP），验证四条判据：
#   ① 哨兵 ≤ DRILL_DETECT_SLA（15）秒记事件——从该上游第一条 relay 连接错误的日志时刻算到事件的
#     at（哨兵在快探与借用做完后才盖时间戳，所以这段时长已含借用与借用后的验证）。构成 = 等第 2 条
#     错误 ≤5 秒（连接类门槛 60 秒 2 条；下面的并发请求让几条错误几乎同时出现，串行时最多一次中继
#     拨号超时 5 秒）+ 哨兵轮询 ≤2 秒 + 判原上游快探 ≤ health::QUICK_PROBE_BUDGET_SECS（4）秒（整个
#     快探的**整体**预算；预算内没能确认也按不可用处置，见 health::Verdict）+ Clash PUT + 首候选的
#     带外验证（同一个预算；本模式下目标网关是通的、隧道正常 ⇒ < 1 秒）⇒ ≈ ≤12 秒（本模式判原上游
#     是丢包，网关 TCP 连不上 ≤3 秒就判不可达）。PUT 按正常毫秒级计（本机 Clash API，单次上限
#     CLASH_TIMEOUT_SECS 2 秒；Clash 自己挂起不在预算内）
#   ② 该槽借用到其它 IP（`bui residential slots --json` 的 borrowed=true、active ≠ 本槽）
#   ③ 该槽用户回环出网 IP 改变（经本槽中继入站 127.0.0.1:(2080+i) 请求 DRILL_EGRESS_URL 取出口 IP）
#   ④ 删掉丢包规则后，巡检在 DRILL_BACK_WAIT（660）秒内切回本槽（恢复后第 4 轮，见计划 D6）
#
# 【--all-ports：整网关不可用】把池里**所有**上游的端口一起丢包（同一网关的每个端口 = 每个出口 IP
# 全挂）。这是修完「借用后带外验证」之后最慢的一条路：哨兵会逐个候选 PUT + 快探，全不通才收尾，
# 判据改成四条：
#   ①' 首条 relay 错误 → 事件 ≤ DRILL_NOEXIT_SLA（25）秒。比 15 秒宽是因为这条路上要把验证预算
#     花完：等第 2 条错误 ≤5 + 轮询 ≤2 + 判原上游快探 ≤4 + BORROW_PROBES_PER_CALL（3）×（PUT +
#     单次验证 ≤4）+ 收尾 PUT（放回本槽）⇒ ≈ ≤24 秒，取 25（三个**满额**验证预算要落在三个不同的
#     槽上：同一个槽撞满一次预算就保留那个候选并停下，往下试的候选必然在 4 秒内给出明确失败 ——
#     单槽只能逼近，方向是安全的高估）。PUT 按正常毫秒级计、Clash 挂起不在预算内（次数还按受影响
#     槽数累加）。
#     那 4 秒（health::QUICK_PROBE_BUDGET_SECS）是**整体**时限，判原上游那次与借用后每次验证
#     共用同一份：网关连不上 ≤3 秒就返回，网关活着、隧道卡住时由代码里的 timeout 兜住
#     （本脚本的丢包只造得出前一种，量不到后一种 —— 后一种靠 sentinel::resi 与 slots 的假件单测）
#   ②' 事件文案说「无可用出口」且**指明终态指向本槽 IP**，绝不出现「已临时切到」（选择器不许停在
#     一条我们自己刚验证过是死路的上游上）。**前置**：池里的上游条数要 ≤ 验证预算 + 1，否则哨兵
#     会合法地停在一条没探过的候选上、报「未确认」，判据②'③' 量的不是那条路 —— 脚本按
#     DRILL_PROBES_PER_CALL 自查，超了就 FATAL（不是缺陷，换单端口模式即可）
#   ③' `bui residential slots --json` 里该槽的当前出口（active_upstream_id = runtime 的
#     current_upstream_id）== 本槽自己的上游、borrowed=false
#   ④' 删掉丢包规则后，巡检在 DRILL_BACK_WAIT 内让该槽出口恢复正常（回环出网 IP 回到演练前那个）
#   注意：这一模式会让**全池**住宅出口断流（约半分钟到一分钟），只在低峰做。
#   跑完（或因池太大跑不了）请把「哪台机器 / 池里几条上游 / 单端口还是 --all-ports」记进计划
#   Task 10 的覆盖面记录：池 ≥5 条上游时这一模式永远跑不起来，别让验收看起来覆盖了这条路。
#
#   真机：sudo bash scripts/ops/sentinel-drill.sh [--slot N] [--all-ports]   # 缺省槽 1、单端口
#   自测：bash scripts/ops/sentinel-drill.sh --self-test          # 不出网、不碰 iptables、不需要 root
#
# 丢包规则都带注释 bui-sentinel-drill；EXIT 的 trap 必定把它们删干净（INT/TERM/HUP 先转成 exit）。
# 分流模式：本槽入站的请求只有走到本槽 selector 才会拨上游。global 全走；split（缺省）只有域名
# 命中分流关键字的才走，其余 route.final = direct——探测 URL 不命中时请求根本不碰上游，判据①–③
# 必然误报失败。所以缺省探测 URL 用命中默认关键字 chatgpt 的 Cloudflare trace（取 ip= 行），
# 开跑前按 `bui residential status --json` 的 mode / domains 自查：split 且不命中 ⇒ FATAL 退 2。
# 演练期间该槽用户会断流约一分钟（直到哨兵借到别的 IP；--all-ports 下是全池断流），请在低峰做。
# 退出码 = FAIL 数（0 = 全过）；前置条件不满足打 FATAL 退 2。凭据一律不经本脚本。
set -uo pipefail
LC_ALL=C

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
BASE=${BASE:-/opt/b-ui}
BUI=${BUI:-$BASE/bin/bui}
IPTABLES=${IPTABLES:-iptables}
CURL=${CURL:-curl}
JOURNALCTL=${JOURNALCTL:-journalctl}
GETENT=${GETENT:-getent}
DETECT_SLA=${DRILL_DETECT_SLA:-15}
# 「无可用出口」那条路要把验证预算花完，算式见文件头 ①'
NOEXIT_SLA=${DRILL_NOEXIT_SLA:-25}
# 哨兵单次借用调用的验证预算（= `slots::BORROW_PROBES_PER_CALL`）：--all-ports 的判据②'③'
# 只在「候选探得完」时成立，见文件头 ②'
PROBES_PER_CALL=${DRILL_PROBES_PER_CALL:-3}
DETECT_WAIT=${DRILL_DETECT_WAIT:-90}
BACK_WAIT=${DRILL_BACK_WAIT:-660}
POLL=${DRILL_POLL:-2}
EGRESS_URL=${DRILL_EGRESS_URL:-https://chatgpt.com/cdn-cgi/trace}
TAG=bui-sentinel-drill
SLOT=1
SELF_TEST=0
ALL_PORTS=0
# 要丢包的端点，空格分隔的 "ip,port"
DROP_SPEC=""
PASS=0
FAIL=0

usage() {
  printf '用法：%s [--slot N] [--all-ports] [--base /opt/b-ui] | --self-test\n' "$0" >&2
  exit 2
}
pass() { PASS=$((PASS + 1)); printf 'PASS %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'FAIL %s\n' "$1"; }
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$1" >&2; }
fatal() { printf 'FATAL %s\n' "$1" >&2; exit 2; }

# slots JSON 里第 $2 槽的字段 $3（布尔打印成 true/false，缺失打印空）
slot_field() {
  python3 -c 'import json, sys
try:
    rows = (json.loads(sys.argv[1]) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    if r.get("index") == int(sys.argv[2]):
        v = r.get(sys.argv[3])
        print("" if v is None else (str(v).lower() if isinstance(v, bool) else v))
        break' "$1" "$2" "$3"
}

slot_count() {
  python3 -c 'import json, sys
try:
    print(len((json.loads(sys.argv[1]) or {}).get("slots") or []))
except Exception:
    print(0)' "$1"
}

# 经本槽中继入站取出口 IP（$1 = relay 端口）；取不到打印空。
# Cloudflare trace 取 ip= 那一行；返回裸 IP 的 URL（DRILL_EGRESS_URL 覆盖时）整行就是 IP
egress_ip() {
  "$CURL" -sS --max-time 20 --socks5-hostname "127.0.0.1:$1" "$EGRESS_URL" 2>/dev/null |
    awk -F= '$1 == "ip" { print $2; exit } NF == 1 && /^[0-9A-Fa-f:.]+$/ { print; exit }' | tr -d '[:space:]'
}

# 探测 URL 经本槽入站时会不会走本槽 selector（$1 = status JSON，$2 = URL）：global ⇒ 打印
# global；split ⇒ 主机名含某个生效关键字（sing-box domain_keyword 是子串匹配）就打印
# split:<关键字>，一个都不含打印空
egress_route() {
  python3 -c 'import json, sys, urllib.parse
try:
    st = json.loads(sys.argv[1]) or {}
except Exception:
    st = {}
host = (urllib.parse.urlsplit(sys.argv[2]).hostname or "").lower()
if st.get("mode") == "global":
    print("global")
else:
    hit = next((k for k in st.get("domains") or [] if k and k.lower() in host), None)
    print("" if hit is None else "split:" + hit)' "$1" "$2"
}

# 主机名 → IPv4（可能多个）；本身就是 IPv4 就原样返回
resolve_v4() {
  if [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf '%s\n' "$1"
    return
  fi
  "$GETENT" ahostsv4 "$1" | awk '{print $1}' | sort -u
}

# 池里每条上游的 "host port"（一槽一条上游，$1 = slots JSON）
all_endpoints() {
  python3 -c 'import json, sys
try:
    rows = (json.loads(sys.argv[1]) or {}).get("slots") or []
except Exception:
    rows = []
for r in rows:
    h, p = r.get("host"), r.get("port")
    if h and p:
        print(h, p)' "$1"
}

# stdin 的 "host port" 行 → "ip,port ip,port …"（主机解析成各 IPv4）
spec_of() {
  local host port ip out=""
  while read -r host port; do
    [[ -n "$host" && -n "$port" ]] || continue
    for ip in $(resolve_v4 "$host"); do
      out+="$ip,$port "
    done
  done
  printf '%s' "$out"
}

drop_on() {
  local e
  for e in $DROP_SPEC; do
    "$IPTABLES" -w -I OUTPUT -p tcp -d "${e%,*}" --dport "${e#*,}" -m comment --comment "$TAG" -j DROP || return 1
  done
}

# 删到删不动为止（同一条插了几次就删几次）；没插过时是 no-op
drop_off() {
  local e
  for e in $DROP_SPEC; do
    while "$IPTABLES" -w -D OUTPUT -p tcp -d "${e%,*}" --dport "${e#*,}" -m comment --comment "$TAG" -j DROP 2>/dev/null; do :; done
  done
}

# relay 日志里 $2（epoch 秒）之后成员 $1 的第一条连接错误的时刻（short-unix 的第一列）
first_error_epoch() {
  "$JOURNALCTL" -u b-ui-relay --since "@$2" -o short-unix --no-pager 2>/dev/null |
    sed 's/\x1b\[[0-9;]*m//g' | grep -F "[$1]: " | grep -F 'open connection to' | head -n1 | awk '{print $1}'
}

# 对象 $1（host:port）在 $2（epoch 秒）之后最早一条 relay_upstream_error 事件的字段 $3：
# at ⇒ 时刻（epoch 秒），result ⇒ 事件文案；没有这样的事件就打印空
incident_field() {
  "$BUI" incidents --json -n 50 2>/dev/null | python3 -c 'import datetime, json, sys
subj, since, field = sys.argv[1], float(sys.argv[2]), sys.argv[3]
try:
    rows = (json.load(sys.stdin) or {}).get("incidents") or []
except Exception:
    rows = []
best = None
for i in rows:
    if i.get("signature") != "relay_upstream_error" or i.get("subject") != subj:
        continue
    t = datetime.datetime.strptime(i["at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    if t >= since and (best is None or t < best[0]):
        best = (t, i)
if best is None:
    print("")
elif field == "at":
    print(int(best[0]))
else:
    print(best[1].get(field) or "")' "$1" "$2" "$3"
}

run_drill() {
  local slots status route n own_id own_name tag host port relay ip_before ip_during ip_after
  local t0 inc err delta result sla borrowed active waited
  slots=$("$BUI" residential slots --json 2>/dev/null) || fatal "bui residential slots --json 失败（守护进程在跑吗？）"
  n=$(slot_count "$slots")
  [[ "$n" -ge 2 ]] || fatal "至少要 2 个槽才有别的 IP 可借（实有 $n）"
  if ((ALL_PORTS)) && ((n - 1 > PROBES_PER_CALL)); then
    fatal "池里 $n 条上游 ⇒ 该槽有 $((n - 1)) 个候选，超过哨兵单次调用的验证预算 $PROBES_PER_CALL（slots::BORROW_PROBES_PER_CALL）：全池不通时哨兵会合法地停在一条没探过的候选上并报「未确认」，判据②'③' 量的不是那条路、会误报 FAIL。改用缺省的单端口模式，或按改过的常量设 DRILL_PROBES_PER_CALL"
  fi
  own_id=$(slot_field "$slots" "$SLOT" upstream_id)
  [[ -n "$own_id" ]] || fatal "槽 $SLOT 不存在"
  [[ "$(slot_field "$slots" "$SLOT" pinned)" != "true" ]] || fatal "槽 $SLOT 被手动 pin，哨兵按设计不动它；先 bui residential slot-pin $SLOT --auto"
  [[ "$(slot_field "$slots" "$SLOT" borrowed)" != "true" ]] || fatal "槽 $SLOT 此刻正在借用，等它切回再演练"
  status=$("$BUI" residential status --json 2>/dev/null) || fatal "bui residential status --json 失败"
  route=$(egress_route "$status" "$EGRESS_URL")
  [[ -n "$route" ]] || fatal "分流模式是 split，探测 URL $EGRESS_URL 的主机不命中任何分流关键字：经本槽入站的请求会走 direct、根本不拨上游，判据①–③ 必然误报失败。换一个命中关键字的 DRILL_EGRESS_URL，或临时 bui residential global on"
  tag=$(slot_field "$slots" "$SLOT" upstream_tag)
  host=$(slot_field "$slots" "$SLOT" host)
  port=$(slot_field "$slots" "$SLOT" port)
  relay=$(slot_field "$slots" "$SLOT" relay_port)
  # 事件文案里指称本槽 IP 的写法：体检学到的出口 IP，没有就退回 host:port（与 resi::ip_of 同口径）
  own_name=$(slot_field "$slots" "$SLOT" ip)
  [[ -n "$own_name" ]] || own_name="$host:$port"
  if ((ALL_PORTS)); then
    DROP_SPEC=$(all_endpoints "$slots" | spec_of)
  else
    DROP_SPEC=$(printf '%s %s\n' "$host" "$port" | spec_of)
  fi
  [[ -n "${DROP_SPEC// /}" ]] || fatal "解析不出要丢弃的 IPv4（$host）"
  ip_before=$(egress_ip "$relay")
  [[ -n "$ip_before" ]] || fatal "演练前经 127.0.0.1:$relay 取不到出口 IP"
  log "槽 $SLOT（$tag = $host:$port，本槽 IP $own_name），分流 $route，演练前出口 $ip_before"

  t0=$(date +%s)
  drop_on || fatal "iptables 插规则失败"
  log "已丢弃发往 ${DROP_SPEC% } 的 TCP（ip,port；注释 $TAG）"
  # 造连接错误：三个并发请求经本槽出网，都会卡在 relay 连上游这一步（连接类门槛 60 秒 ≥2 条；
  # 并发让错误同时出现，不用串行等 relay 拨号超时）
  for _ in 1 2 3; do
    egress_ip "$relay" >/dev/null &
  done

  # ① 事件与时延（--all-ports 走「无可用出口」那条更宽的 SLA，算式见文件头 ①'）
  sla=$DETECT_SLA
  ((ALL_PORTS)) && sla=$NOEXIT_SLA
  inc=""
  waited=0
  while ((waited < DETECT_WAIT)); do
    inc=$(incident_field "$host:$port" "$t0" at)
    [[ -n "$inc" ]] && break
    sleep "$POLL"
    waited=$((waited + POLL))
  done
  err=$(first_error_epoch "$tag" "$t0")
  if [[ -z "$inc" ]]; then
    fail "判据① 哨兵事件：${DETECT_WAIT}s 内没有 $host:$port 的 relay_upstream_error"
  elif [[ -z "$err" ]]; then
    fail "判据① 哨兵事件：有事件，但 relay 日志里找不到 [$tag] 的连接错误，无法计时"
  else
    delta=$(python3 -c 'import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 1))' "$inc" "$err")
    if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= float(sys.argv[2]) else 1)' "$delta" "$sla"; then
      pass "判据① 哨兵事件：首条错误后 ${delta}s 记事件（≤${sla}s）"
    else
      fail "判据① 哨兵事件：首条错误后 ${delta}s 才记事件（>${sla}s）"
    fi
  fi

  if ((ALL_PORTS)); then
    # ②' 事件文案：说「无可用出口」、指明终态是本槽 IP，且绝不出现「已临时切到」
    result=$(incident_field "$host:$port" "$t0" result)
    if [[ "$result" == *"已临时切到"* ]]; then
      fail "判据② 全池不通却报「已临时切到」：$result"
    elif [[ "$result" == *"无可用出口"* && "$result" == *"$own_name"* ]]; then
      pass "判据② 事件如实报无可用出口并指明终态 $own_name"
    else
      fail "判据② 事件没说清终态（要有「无可用出口」与本槽 IP $own_name）：${result:-（没有事件）}"
    fi

    # ③' selector 已放回本槽自己的上游
    slots=$("$BUI" residential slots --json 2>/dev/null)
    borrowed=$(slot_field "$slots" "$SLOT" borrowed)
    active=$(slot_field "$slots" "$SLOT" active_upstream_id)
    if [[ "$active" == "$own_id" && "$borrowed" == "false" ]]; then
      pass "判据③ 槽 $SLOT 的当前出口已放回本槽 IP"
    else
      fail "判据③ 槽 $SLOT 的当前出口不是本槽 IP（active=${active:-?} borrowed=${borrowed:-?}）"
    fi
  else
    # ② 该槽借用
    slots=$("$BUI" residential slots --json 2>/dev/null)
    borrowed=$(slot_field "$slots" "$SLOT" borrowed)
    active=$(slot_field "$slots" "$SLOT" active_upstream_id)
    if [[ "$borrowed" == "true" && -n "$active" && "$active" != "$own_id" ]]; then
      pass "判据② 槽 $SLOT 已借用 $(slot_field "$slots" "$SLOT" active_tag)"
    else
      fail "判据② 槽 $SLOT 没有借用（borrowed=${borrowed:-?} active=${active:-?}）"
    fi

    # ③ 回环出网 IP 改变
    ip_during=$(egress_ip "$relay")
    if [[ -n "$ip_during" && "$ip_during" != "$ip_before" ]]; then
      pass "判据③ 回环出网 IP $ip_before → $ip_during"
    else
      fail "判据③ 回环出网 IP 没变（前 $ip_before，后 ${ip_during:-取不到}）"
    fi
  fi

  # 恢复
  drop_off
  wait

  # ④ 单端口：巡检切回本槽；--all-ports：本来就在本槽，等出口恢复正常
  waited=0
  if ((ALL_PORTS)); then
    log "已删除丢包规则，等出口恢复（最多 ${BACK_WAIT}s）"
    ip_after=""
    while ((waited < BACK_WAIT)); do
      ip_after=$(egress_ip "$relay")
      [[ "$ip_after" == "$ip_before" ]] && break
      sleep "$POLL"
      waited=$((waited + POLL))
    done
    if ((waited < BACK_WAIT)); then
      pass "判据④ 恢复后 ${waited}s 出口回到 $ip_before"
    else
      fail "判据④ ${BACK_WAIT}s 内出口没恢复（前 $ip_before，后 ${ip_after:-取不到}）"
    fi
  else
    log "已删除丢包规则，等巡检切回（最多 ${BACK_WAIT}s）"
    while ((waited < BACK_WAIT)); do
      slots=$("$BUI" residential slots --json 2>/dev/null)
      if [[ "$(slot_field "$slots" "$SLOT" borrowed)" == "false" &&
        "$(slot_field "$slots" "$SLOT" active_upstream_id)" == "$own_id" ]]; then
        break
      fi
      sleep "$POLL"
      waited=$((waited + POLL))
    done
    if ((waited < BACK_WAIT)); then
      pass "判据④ 恢复后 ${waited}s 切回本槽 IP"
    else
      fail "判据④ ${BACK_WAIT}s 内没有切回本槽"
    fi
  fi
  printf '%d PASS / %d FAIL\n' "$PASS" "$FAIL"
  return "$FAIL"
}

# ── 自测：五个 stub 模拟 bui / iptables / curl / journalctl / getent ─────────────
write_stubs() {
  local d="$1"
  cat >"$d/iptables" <<'STUB'
#!/usr/bin/env bash
# 记账：-I 追加一行，-D 删掉第一条相同的（没有就退 1，与真 iptables 一致）
f="$ST_DIR/rules"
touch "$f"
args="$*"
key="${args/-I OUTPUT/}"
key="${key/-D OUTPUT/}"
case " $* " in
  *" -I "*) printf '%s\n' "$key" >>"$f" ;;
  *" -D "*)
    grep -qxF -- "$key" "$f" || exit 1
    awk -v k="$key" '!d && $0 == k { d = 1; next } { print }' "$f" >"$f.tmp" && mv "$f.tmp" "$f"
    ;;
esac
STUB
  cat >"$d/curl" <<'STUB'
#!/usr/bin/env bash
# Cloudflare trace 的形状。借用后出口 .9；丢包期间没借用 ⇒ 记一条 relay 连接错误并超时；平时出口 .8
trace() { printf 'fl=1f1\nh=chatgpt.com\nip=%s\nts=1\n' "$1"; }
if [[ -f "$ST_DIR/borrowed" ]]; then trace 198.51.100.9; exit 0; fi
if [[ -s "$ST_DIR/rules" ]]; then
  printf '%s.000000 node-a sing-box[1]: ERROR[4006] [1 5.00s] connection: open connection to chatgpt.com:443 using outbound/socks[resi-2]: dial tcp 198.51.100.200:10007: i/o timeout\n' "$(date +%s)" >>"$ST_DIR/journal"
  exit 28
fi
trace 198.51.100.8
STUB
  cat >"$d/journalctl" <<'STUB'
#!/usr/bin/env bash
cat "$ST_DIR/journal" 2>/dev/null
STUB
  cat >"$d/getent" <<'STUB'
#!/usr/bin/env bash
printf '198.51.100.200  STREAM isp2.example.net\n198.51.100.200  DGRAM\n'
STUB
  cat >"$d/bui" <<'STUB'
#!/usr/bin/env bash
# 模拟守护进程：丢包 + 有连接错误 ⇒ 记事件（首条错误后 FAKE_DELAY 秒）；
# 丢包规则只有 1 条（单端口模式）⇒ 借到另一条 IP；≥2 条（--all-ports 把整网关封了）⇒ 候选逐条
# 验证都不通、放回本槽 IP、事件报「无可用出口」（FAKE_BORROW_ANYWAY=1 模拟修复前那个缺陷：
# 全池不通却照样报「已临时切到」）。
# 规则删掉后第 3 次查询切回（FAKE_NO_BACK=1 永不切回；FAKE_NO_INCIDENT=1 永不记事件）；
# residential status 按 FAKE_MODE（缺省 split）/ FAKE_DOMAINS（缺省 ["openai","chatgpt"]）作答。
# 两条上游同一个网关、不同静态端口（生产拓扑），所以 --all-ports 会插 2 条规则
S="$ST_DIR"
rules_on() { [[ -s "$S/rules" ]]; }
all_ports() { [[ "$(grep -c . "$S/rules" 2>/dev/null || echo 0)" -ge 2 && "${FAKE_BORROW_ANYWAY:-0}" != 1 ]]; }
case "$1 $2" in
  "residential status")
    dflt='["openai","chatgpt"]'
    printf '{"mode":"%s","domains":%s}\n' "${FAKE_MODE:-split}" "${FAKE_DOMAINS:-$dflt}"
    ;;
  "residential slots")
    if [[ -f "$S/borrowed" ]] && ! rules_on && [[ "${FAKE_NO_BACK:-0}" != 1 ]]; then
      n=$(($(cat "$S/back" 2>/dev/null || echo 0) + 1))
      echo "$n" >"$S/back"
      [[ "$n" -ge 3 ]] && rm -f "$S/borrowed"
    fi
    if [[ -f "$S/borrowed" ]]; then b=true; a=3; else b=false; a=2; fi
    printf '{"slots":[{"index":0,"upstream_id":"00000000-0000-0000-0000-000000000001","upstream_tag":"resi-1","host":"isp1.example.net","port":10008,"ip":"198.51.100.7","relay_port":2080,"borrowed":false,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-000000000001","active_tag":"resi-1"},{"index":1,"upstream_id":"00000000-0000-0000-0000-000000000002","upstream_tag":"resi-2","host":"isp2.example.net","port":10007,"ip":"198.51.100.8","relay_port":2081,"borrowed":%s,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-00000000000%s","active_tag":"resi-%s"}]}\n' "$b" "$a" "$a"
    ;;
  "incidents --json")
    if rules_on && [[ -s "$S/journal" && ! -f "$S/incident" && "${FAKE_NO_INCIDENT:-0}" != 1 ]]; then
      e=$(head -n1 "$S/journal" | awk '{printf "%d", $1}')
      date -u -d "@$((e + ${FAKE_DELAY:-3}))" +%FT%TZ >"$S/incident"
      if all_ports; then
        printf '%s' 'IP 198.51.100.8 不可达，槽 1：候选 198.51.100.7 探不通，当前指向本槽 IP 198.51.100.8，当前无可用出口' >"$S/result"
      else
        printf '%s' 'IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.9' >"$S/result"
        touch "$S/borrowed"
      fi
    fi
    if [[ -f "$S/incident" ]]; then
      printf '{"incidents":[{"at":"%s","unit":"b-ui-relay","signature":"relay_upstream_error","subject":"isp2.example.net:10007","action":"probe_and_borrow","result":"%s","level":"error"}],"source":"daemon"}\n' "$(cat "$S/incident")" "$(cat "$S/result")"
    else
      printf '{"incidents":[],"source":"daemon"}\n'
    fi
    ;;
  *) echo "stub bui: $*" >&2; exit 1 ;;
esac
STUB
  chmod +x "$d"/*
}

self_test() {
  ST_DIR=$(mktemp -d) || exit 2
  export ST_DIR
  trap 'rm -rf "$ST_DIR"' EXIT
  mkdir -p "$ST_DIR/bin"
  write_stubs "$ST_DIR/bin"
  local out rc pid i st_pass=0 st_fail=0
  check() {
    if [[ "$2" == *"$1"* ]]; then
      st_pass=$((st_pass + 1))
      printf 'PASS 自测 %s\n' "$3"
    else
      st_fail=$((st_fail + 1))
      printf 'FAIL 自测 %s\n    缺：%s\n' "$3" "$1"
    fi
  }
  # $1 起的 KEY=VAL 是 stub 的开关，`--` 之后是传给演练脚本自己的参数
  scenario() {
    local envs=()
    while [[ $# -gt 0 && "$1" != "--" ]]; do
      envs+=("$1")
      shift
    done
    [[ "${1:-}" == "--" ]] && shift
    rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" \
      "$ST_DIR/back" "$ST_DIR/result"
    env "${envs[@]}" BUI="$ST_DIR/bin/bui" IPTABLES="$ST_DIR/bin/iptables" CURL="$ST_DIR/bin/curl" \
      JOURNALCTL="$ST_DIR/bin/journalctl" GETENT="$ST_DIR/bin/getent" DRILL_ALLOW_NONROOT=1 \
      DRILL_DETECT_WAIT="${WAIT_DETECT:-3}" DRILL_BACK_WAIT=6 DRILL_POLL=1 \
      bash "$SELF" --slot 1 "$@" 2>&1
  }

  out=$(scenario)
  rc=$?
  check "PASS 判据① 哨兵事件：首条错误后 3.0s" "$out" "判据① 通过分支：3 秒记事件"
  check "PASS 判据② 槽 1 已借用 resi-3" "$out" "判据② 通过分支：借用"
  check "PASS 判据③ 回环出网 IP 198.51.100.8 → 198.51.100.9" "$out" "判据③ 通过分支：出口改变"
  check "PASS 判据④ 恢复后" "$out" "判据④ 通过分支：切回"
  check "4 PASS / 0 FAIL" "$out" "成功路径摘要"
  check "rc=0" "rc=$rc" "成功路径退出码 0"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "成功路径：丢包规则已删干净"
  check "分流 split:chatgpt" "$out" "分流 split 分支：探测 URL 命中关键字才演练"

  out=$(scenario FAKE_DOMAINS='["openai"]')
  rc=$?
  check "FATAL 分流模式是 split" "$out" "分流 split 分支：不命中关键字 ⇒ FATAL"
  check "rc=2" "rc=$rc" "分流 split 不命中：退出码 2"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "分流 split 不命中：一条丢包规则都没插"

  out=$(scenario FAKE_MODE=global FAKE_DOMAINS='[]')
  check "分流 global" "$out" "分流 global 分支：不看关键字"
  check "4 PASS / 0 FAIL" "$out" "分流 global 分支：照常全过"

  out=$(scenario FAKE_DELAY=30)
  check "FAIL 判据① 哨兵事件：首条错误后 30.0s 才记事件" "$out" "判据① 失败分支：超过 15 秒"

  out=$(scenario FAKE_NO_INCIDENT=1)
  rc=$?
  check "FAIL 判据① 哨兵事件：3s 内没有 isp2.example.net:10007" "$out" "判据① 失败分支：没有事件"
  check "FAIL 判据② 槽 1 没有借用" "$out" "判据② 失败分支：没借用"
  check "FAIL 判据③ 回环出网 IP 没变" "$out" "判据③ 失败分支：出口没变"
  check "rc=3" "rc=$rc" "没事件时退出码 = FAIL 数"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "失败路径：丢包规则同样删干净"

  out=$(scenario FAKE_NO_BACK=1)
  check "FAIL 判据④ 6s 内没有切回本槽" "$out" "判据④ 失败分支：不切回"

  # ── --all-ports：整网关不可用（判据②③④ 换成「无可用出口」那一套）──────────
  out=$(scenario -- --all-ports)
  rc=$?
  check "PASS 判据① 哨兵事件：首条错误后 3.0s 记事件（≤25s）" "$out" "判据① 无出口模式通过分支：按 25 秒判"
  check "PASS 判据② 事件如实报无可用出口并指明终态 198.51.100.8" "$out" \
    "判据② 无出口模式通过分支：文案说清终态"
  check "PASS 判据③ 槽 1 的当前出口已放回本槽 IP" "$out" "判据③ 无出口模式通过分支：放回本槽"
  check "PASS 判据④ 恢复后" "$out" "判据④ 无出口模式通过分支：出口恢复"
  check "4 PASS / 0 FAIL" "$out" "无出口模式摘要"
  check "rc=0" "rc=$rc" "无出口模式退出码 0"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" \
    "无出口模式：两条丢包规则都删干净"

  # 修复前那个缺陷的回归位：全池不通却报「已临时切到」⇒ 判据② 必须 FAIL
  out=$(scenario FAKE_BORROW_ANYWAY=1 -- --all-ports)
  check "FAIL 判据② 全池不通却报「已临时切到」" "$out" "判据② 无出口模式失败分支：谎报借用成功"

  # 候选比验证预算还多：判据②'③' 量不到那条路 ⇒ 开跑前 FATAL，一条规则都不插
  out=$(scenario DRILL_PROBES_PER_CALL=0 -- --all-ports)
  rc=$?
  check "FATAL 池里 2 条上游" "$out" "无出口模式前置：候选多于验证预算 ⇒ FATAL"
  check "rc=2" "rc=$rc" "候选多于验证预算：退出码 2"
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" \
    "候选多于验证预算：一条丢包规则都没插"

  # trap 兜底：演练卡在等事件时被 TERM 杀掉，规则也必须删干净
  rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" \
    "$ST_DIR/back" "$ST_DIR/result"
  env FAKE_NO_INCIDENT=1 BUI="$ST_DIR/bin/bui" IPTABLES="$ST_DIR/bin/iptables" CURL="$ST_DIR/bin/curl" \
    JOURNALCTL="$ST_DIR/bin/journalctl" GETENT="$ST_DIR/bin/getent" DRILL_ALLOW_NONROOT=1 \
    DRILL_DETECT_WAIT=60 DRILL_POLL=1 bash "$SELF" --slot 1 >/dev/null 2>&1 &
  pid=$!
  for i in $(seq 1 50); do
    [[ -s "$ST_DIR/rules" ]] && break
    sleep 0.1
  done
  kill -TERM "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  check "rules=0" "rules=$(grep -c . "$ST_DIR/rules" 2>/dev/null || echo 0)" "被 TERM 杀掉也删干净（等了 ${i} 个 0.1s 才见到规则）"

  printf '自测：%d PASS / %d FAIL\n' "$st_pass" "$st_fail"
  exit "$st_fail"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --slot) SLOT="${2:-}"; shift 2 ;;
    --all-ports) ALL_PORTS=1; shift ;;
    --base) BASE="${2:-}"; BUI="$BASE/bin/bui"; shift 2 ;;
    --self-test) SELF_TEST=1; shift ;;
    *) usage ;;
  esac
done
[[ "$SLOT" =~ ^[0-9]+$ ]] || usage

if [[ "$SELF_TEST" -eq 1 ]]; then
  self_test
fi

command -v python3 >/dev/null || fatal "需要 python3"
if [[ "${DRILL_ALLOW_NONROOT:-0}" != 1 ]]; then
  [[ "$EUID" -eq 0 ]] || fatal "需要 root（要动 iptables）"
  command -v "$IPTABLES" >/dev/null || fatal "找不到 $IPTABLES"
fi
trap 'drop_off' EXIT
trap 'exit 130' INT TERM HUP
run_drill
exit $?
