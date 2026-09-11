#!/usr/bin/env bash
# 用 v3 的 web/server.js 以合成数据生成订阅 golden 样本。只读 v3 源码，不动生产。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FX="$ROOT/crates/bui-schema/tests/fixtures/v3"
SRC="$FX/src"; OUT="$FX/expected"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"; [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null || true' EXIT
PORT=18090
SERVER_IP=203.0.113.10
# server.js 生成订阅时会 `getent ahostsv4 <域名>` 把域名解析成公网 IPv4 写进 sing-box 出站
# （web/server.js:101）。fixture 的 example.com 在真实 DNS 里有解析，样本会随机器与时间漂移，
# 所以在 PATH 上放一个 getent 桩：ahostsv4 一律回合成 IP，其余调用转给真的 getent。
mkdir -p "$WORK/stubs"
cat > "$WORK/stubs/getent" <<STUB
#!/usr/bin/env bash
if [ "\${1:-}" = "ahostsv4" ]; then printf '%s STREAM %s\n' "$SERVER_IP" "\${2:-}"; exit 0; fi
exec /usr/bin/getent "\$@"
STUB
chmod +x "$WORK/stubs/getent"
run_mode() {  # $1 = mode name; 调用前已把 $WORK/base 准备好
  local mode="$1"
  ( cd "$ROOT/web" && exec env PATH="$WORK/stubs:$PATH" BASE_DIR="$WORK/base" ADMIN_DIR="$ROOT/web" ADMIN_PORT=$PORT ADMIN_PASSWORD=test123 SERVER_IP=$SERVER_IP node server.js >"$WORK/server-$mode.log" 2>&1 ) & PID=$!
  for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/api/sub/alice" >/dev/null 2>&1 && break; sleep 0.2; done
  mkdir -p "$OUT/$mode"
  for u in alice bob carol dave; do
    curl -sf "http://127.0.0.1:$PORT/api/sub/$u"          > "$OUT/$mode/$u.sub.txt"
    curl -sf "http://127.0.0.1:$PORT/api/subscription/$u" | python3 -m json.tool > "$OUT/$mode/$u.singbox.json"
    curl -sf "http://127.0.0.1:$PORT/api/clash/$u"        > "$OUT/$mode/$u.clash.yaml"
  done
  kill "$PID"; wait "$PID" 2>/dev/null || true; PID=
}
prep() {  # 复制 src 到 $WORK/base，server.js 需要 residential-helper.sh 同目录
  rm -rf "$WORK/base"; mkdir -p "$WORK/base/certs"
  cp "$SRC"/users.json "$SRC"/config.yaml "$SRC"/config-residential.yaml "$SRC"/reality-keys.json "$SRC"/xray-config.json "$WORK/base/"
  cp "$SRC/certs/.domain" "$WORK/base/certs/.domain"
  cp "$ROOT/server/residential-helper.sh" "$WORK/base/residential-helper.sh"; chmod +x "$WORK/base/residential-helper.sh"
}
# mode global：residential-proxy.json 原样（global=true）
prep; cp "$SRC/residential-proxy.json" "$WORK/base/"; run_mode global
# mode split：global=false，domains=null（跟随默认表）
prep; python3 - "$SRC/residential-proxy.json" "$WORK/base/residential-proxy.json" <<'PY'
import json,sys; d=json.load(open(sys.argv[1])); d["global"]=False; d["domains"]=None; json.dump(d,open(sys.argv[2],"w"))
PY
run_mode split
# mode obfs：global=true + config.yaml 顶部插入 obfs 段（与 b-ui-cli.sh cmd_obfs 相同格式）
prep; cp "$SRC/residential-proxy.json" "$WORK/base/"
{ printf 'obfs:\n  type: salamander\n  salamander:\n    password: obfs-pw-test\n'; cat "$SRC/config.yaml"; } > "$WORK/base/config.yaml"
run_mode obfs
{ echo "# v3 golden fixtures"; echo "生成命令: scripts/gen-v3-fixtures.sh"
  # 记生成器自身的 commit（web/server.js 最后一次改动）：比 HEAD 稳定，重跑不随新 commit 漂移
  echo "v3 commit: $(git -C "$ROOT" log -1 --format=%h -- web/server.js)（web/server.js 最后改动）"
  echo "生成时间: $(date -u +%FT%TZ)"; } > "$OUT/README.md"
# 每个 singbox.json 都过真实内核校验（缺二进制则跳过，不阻塞生成）
if command -v sing-box >/dev/null 2>&1; then
  for f in "$OUT"/*/*.singbox.json; do sing-box check -c "$f" || { echo "FAIL: $f"; exit 1; }; done
  echo "sing-box check: 全部通过（$(sing-box version | head -1)）"
else
  echo "skipped: sing-box 不存在，未做内核校验"
fi
echo "OK: $OUT"
