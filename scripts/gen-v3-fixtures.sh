#!/usr/bin/env bash
# 用 v3 的 web/server.js 以合成数据生成订阅 golden 样本。v3 源码从 git 历史物化，不动生产。
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FX="$ROOT/crates/bui-schema/tests/fixtures/v3"
SRC="$FX/src"; OUT="$FX/expected"
WORK=$(mktemp -d); trap 'rm -rf "$WORK"; [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null || true' EXIT
# v3 的 web/server.js 与 server/residential-helper.sh 在 v4.0.0 里已删除；
# 重生成 golden 样本时从删除前的 ref 物化：默认自动定位「删掉 web/server.js 的那个 commit」的父提交，
# 所以不用手填任何 sha，任何 clone 都能跑；要指定别的历史点就传 V3_REF。
V3_REF="${V3_REF:-$(git -C "$ROOT" log --diff-filter=D --format=%H -1 -- web/server.js)^}"
if [ "$V3_REF" = "^" ]; then
    printf '找不到删除 web/server.js 的 commit：请显式传 V3_REF=<含 v3 文件的 ref>\n' >&2
    exit 2
fi
V3SRC="$WORK/v3"
mkdir -p "$V3SRC/web" "$V3SRC/server"
git -C "$ROOT" show "$V3_REF:web/server.js"                > "$V3SRC/web/server.js"
git -C "$ROOT" show "$V3_REF:web/package.json"             > "$V3SRC/web/package.json"
git -C "$ROOT" show "$V3_REF:server/residential-helper.sh" > "$V3SRC/server/residential-helper.sh"
chmod +x "$V3SRC/server/residential-helper.sh"
# 前端文件（index.html / app.js / style.css …）v4 保留着，server.js 照 ADMIN_DIR=$ROOT/web 读原地的。
# web/node_modules/js-yaml 是 server.js 的运行期依赖，随 v4 一起被删了；
# 工作树里还在就复制，已删就现装（npm 只在重生成 golden 样本时才需要）。
if [ -d "$ROOT/web/node_modules" ]; then
    cp -r "$ROOT/web/node_modules" "$V3SRC/web/node_modules"
else
    printf '提示：v3 的 web/node_modules 已随 v4 删除，现在 %s 里跑一次 npm install\n' "$V3SRC/web" >&2
    (cd "$V3SRC/web" && npm install --no-audit --no-fund)
fi
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
  ( cd "$V3SRC/web" && exec env PATH="$WORK/stubs:$PATH" BASE_DIR="$WORK/base" ADMIN_DIR="$ROOT/web" ADMIN_PORT=$PORT ADMIN_PASSWORD=test123 SERVER_IP=$SERVER_IP node server.js >"$WORK/server-$mode.log" 2>&1 ) & PID=$!
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
  cp "$V3SRC/server/residential-helper.sh" "$WORK/base/residential-helper.sh"; chmod +x "$WORK/base/residential-helper.sh"
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
  # 记物化 v3 源码用的 ref（删除提交的父提交，或显式传进来的 V3_REF）：比 HEAD 稳定，重跑不漂移
  echo "v3 ref: $(git -C "$ROOT" rev-parse --short "$V3_REF")（$V3_REF，物化 web/server.js 的来源）"
  echo "生成时间: $(date -u +%FT%TZ)"; } > "$OUT/README.md"
# 每个 singbox.json 都过真实内核校验（缺二进制则跳过，不阻塞生成）
if command -v sing-box >/dev/null 2>&1; then
  for f in "$OUT"/*/*.singbox.json; do sing-box check -c "$f" || { echo "FAIL: $f"; exit 1; }; done
  echo "sing-box check: 全部通过（$(sing-box version | head -1)）"
else
  echo "skipped: sing-box 不存在，未做内核校验"
fi
echo "OK: $OUT"
