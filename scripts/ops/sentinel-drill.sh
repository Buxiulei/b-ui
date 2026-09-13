#!/usr/bin/env bash
# b-ui v4 日志哨兵真机演练（spec §5.7）。在生产机（先 bwg-rick）上以 root 运行：临时丢弃发往
# **某一个**住宅上游（该上游的 IP:端口，只动 TCP）的流量，验证四条判据：
#   ① 哨兵 ≤ DRILL_DETECT_SLA（15）秒记事件——从该上游第一条 relay 连接错误的日志时刻算到事件的
#     at（哨兵在快探与借用做完后才盖时间戳，所以这段时长已含借用）
#   ② 该槽借用到其它 IP（`bui residential slots --json` 的 borrowed=true、active ≠ 本槽）
#   ③ 该槽用户回环出网 IP 改变（经本槽中继入站 127.0.0.1:(2080+i) 请求 DRILL_EGRESS_URL 取出口 IP）
#   ④ 删掉丢包规则后，巡检在 DRILL_BACK_WAIT（660）秒内切回本槽（恢复后第 4 轮，见计划 D6）
#
#   真机：sudo bash scripts/ops/sentinel-drill.sh [--slot N]      # 缺省槽 1
#   自测：bash scripts/ops/sentinel-drill.sh --self-test          # 不出网、不碰 iptables、不需要 root
#
# 丢包规则都带注释 bui-sentinel-drill；EXIT 的 trap 必定把它们删干净（INT/TERM/HUP 先转成 exit）。
# 分流模式：本槽入站的请求只有走到本槽 selector 才会拨上游。global 全走；split（缺省）只有域名
# 命中分流关键字的才走，其余 route.final = direct——探测 URL 不命中时请求根本不碰上游，判据①–③
# 必然误报失败。所以缺省探测 URL 用命中默认关键字 chatgpt 的 Cloudflare trace（取 ip= 行），
# 开跑前按 `bui residential status --json` 的 mode / domains 自查：split 且不命中 ⇒ FATAL 退 2。
# 演练期间该槽用户会断流约一分钟（直到哨兵借到别的 IP），请在低峰做。
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
DETECT_WAIT=${DRILL_DETECT_WAIT:-90}
BACK_WAIT=${DRILL_BACK_WAIT:-660}
POLL=${DRILL_POLL:-2}
EGRESS_URL=${DRILL_EGRESS_URL:-https://chatgpt.com/cdn-cgi/trace}
TAG=bui-sentinel-drill
SLOT=1
SELF_TEST=0
DROP_IPS=""
DROP_PORT=""
PASS=0
FAIL=0

usage() {
  printf '用法：%s [--slot N] [--base /opt/b-ui] | --self-test\n' "$0" >&2
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

drop_on() {
  local ip
  for ip in $DROP_IPS; do
    "$IPTABLES" -w -I OUTPUT -p tcp -d "$ip" --dport "$DROP_PORT" -m comment --comment "$TAG" -j DROP || return 1
  done
}

# 删到删不动为止（同一条插了几次就删几次）；没插过时是 no-op
drop_off() {
  local ip
  [[ -n "$DROP_PORT" ]] || return 0
  for ip in $DROP_IPS; do
    while "$IPTABLES" -w -D OUTPUT -p tcp -d "$ip" --dport "$DROP_PORT" -m comment --comment "$TAG" -j DROP 2>/dev/null; do :; done
  done
}

# relay 日志里 $2（epoch 秒）之后成员 $1 的第一条连接错误的时刻（short-unix 的第一列）
first_error_epoch() {
  "$JOURNALCTL" -u b-ui-relay --since "@$2" -o short-unix --no-pager 2>/dev/null |
    sed 's/\x1b\[[0-9;]*m//g' | grep -F "[$1]: " | grep -F 'open connection to' | head -n1 | awk '{print $1}'
}

# 对象 $1（host:port）在 $2（epoch 秒）之后最早一条 relay_upstream_error 事件的时刻（epoch 秒）
incident_epoch() {
  "$BUI" incidents --json -n 50 2>/dev/null | python3 -c 'import datetime, json, sys
subj, since = sys.argv[1], float(sys.argv[2])
try:
    rows = (json.load(sys.stdin) or {}).get("incidents") or []
except Exception:
    rows = []
best = None
for i in rows:
    if i.get("signature") != "relay_upstream_error" or i.get("subject") != subj:
        continue
    t = datetime.datetime.strptime(i["at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc).timestamp()
    if t >= since and (best is None or t < best):
        best = t
print("" if best is None else int(best))' "$1" "$2"
}

run_drill() {
  local slots status route n own_id tag host port relay ip_before ip_during t0 inc err delta borrowed active waited
  slots=$("$BUI" residential slots --json 2>/dev/null) || fatal "bui residential slots --json 失败（守护进程在跑吗？）"
  n=$(slot_count "$slots")
  [[ "$n" -ge 2 ]] || fatal "至少要 2 个槽才有别的 IP 可借（实有 $n）"
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
  DROP_IPS=$(resolve_v4 "$host" | tr '\n' ' ')
  [[ -n "${DROP_IPS// /}" ]] || fatal "解析不出 $host 的 IPv4"
  DROP_PORT=$port
  ip_before=$(egress_ip "$relay")
  [[ -n "$ip_before" ]] || fatal "演练前经 127.0.0.1:$relay 取不到出口 IP"
  log "槽 $SLOT（$tag = $host:$port → ${DROP_IPS% }），分流 $route，演练前出口 $ip_before"

  t0=$(date +%s)
  drop_on || fatal "iptables 插规则失败"
  log "已丢弃发往 ${DROP_IPS% } 端口 $port 的 TCP（注释 $TAG）"
  # 造连接错误：三个并发请求经本槽出网，都会卡在 relay 连上游这一步（门槛 60 秒 ≥3 条）
  for _ in 1 2 3; do
    egress_ip "$relay" >/dev/null &
  done

  # ① 事件与时延
  inc=""
  waited=0
  while ((waited < DETECT_WAIT)); do
    inc=$(incident_epoch "$host:$port" "$t0")
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
    if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= float(sys.argv[2]) else 1)' "$delta" "$DETECT_SLA"; then
      pass "判据① 哨兵事件：首条错误后 ${delta}s 记事件（≤${DETECT_SLA}s）"
    else
      fail "判据① 哨兵事件：首条错误后 ${delta}s 才记事件（>${DETECT_SLA}s）"
    fi
  fi

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

  # 恢复
  drop_off
  wait
  log "已删除丢包规则，等巡检切回（最多 ${BACK_WAIT}s）"

  # ④ 切回
  waited=0
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
# 模拟守护进程：丢包 + 有连接错误 ⇒ 记事件（首条错误后 FAKE_DELAY 秒）并借用；
# 规则删掉后第 3 次查询切回（FAKE_NO_BACK=1 永不切回；FAKE_NO_INCIDENT=1 永不记事件）；
# residential status 按 FAKE_MODE（缺省 split）/ FAKE_DOMAINS（缺省 ["openai","chatgpt"]）作答
S="$ST_DIR"
rules_on() { [[ -s "$S/rules" ]]; }
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
    printf '{"slots":[{"index":0,"upstream_id":"00000000-0000-0000-0000-000000000001","upstream_tag":"resi-1","host":"isp1.example.net","port":10007,"relay_port":2080,"borrowed":false,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-000000000001","active_tag":"resi-1"},{"index":1,"upstream_id":"00000000-0000-0000-0000-000000000002","upstream_tag":"resi-2","host":"isp2.example.net","port":10007,"relay_port":2081,"borrowed":%s,"pinned":false,"active_upstream_id":"00000000-0000-0000-0000-00000000000%s","active_tag":"resi-%s"}]}\n' "$b" "$a" "$a"
    ;;
  "incidents --json")
    if rules_on && [[ -s "$S/journal" && ! -f "$S/incident" && "${FAKE_NO_INCIDENT:-0}" != 1 ]]; then
      e=$(head -n1 "$S/journal" | awk '{printf "%d", $1}')
      date -u -d "@$((e + ${FAKE_DELAY:-3}))" +%FT%TZ >"$S/incident"
      touch "$S/borrowed"
    fi
    if [[ -f "$S/incident" ]]; then
      printf '{"incidents":[{"at":"%s","unit":"b-ui-relay","signature":"relay_upstream_error","subject":"isp2.example.net:10007","action":"probe_and_borrow","result":"stub","level":"error"}],"source":"daemon"}\n' "$(cat "$S/incident")"
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
  scenario() {
    rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" "$ST_DIR/back"
    env "$@" BUI="$ST_DIR/bin/bui" IPTABLES="$ST_DIR/bin/iptables" CURL="$ST_DIR/bin/curl" \
      JOURNALCTL="$ST_DIR/bin/journalctl" GETENT="$ST_DIR/bin/getent" DRILL_ALLOW_NONROOT=1 \
      DRILL_DETECT_WAIT="${WAIT_DETECT:-3}" DRILL_BACK_WAIT=6 DRILL_POLL=1 \
      bash "$SELF" --slot 1 2>&1
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

  # trap 兜底：演练卡在等事件时被 TERM 杀掉，规则也必须删干净
  rm -f "$ST_DIR/rules" "$ST_DIR/journal" "$ST_DIR/incident" "$ST_DIR/borrowed" "$ST_DIR/back"
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
