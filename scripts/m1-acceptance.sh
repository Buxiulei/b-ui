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
  out=$("$BUI" install --yes 2>&1 | tail -n 20)
  if printf '%s' "$out" | grep -q "已安装"; then ok "step2 二次 install 走对账路径（不覆盖 state）"; else no "step2 二次 install 行为异常" "$out"; fi
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
  self_test_external_sites
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
