#!/usr/bin/env bash
# authhttp-bench.py 跑 2 秒（打一个本机 stub 鉴权服务），自带判读给 PASS，
# 产物能被 authhook-report.sh 直接读；stub 全拒时两边都给 FAIL。不出网。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

command -v python3 >/dev/null 2>&1 || { printf '# skip: 本机没有 python3\n'; exit 0; }

WORK=$(mktemp -d)
STUB_PID=""
DENY_PID=""
# shellcheck disable=SC2317  # trap 里调用
cleanup() {
    [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null
    [[ -n "$DENY_PID" ]] && kill "$DENY_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

printf 'alice:pw-alice-01\n' > "$WORK/cred"
chmod 600 "$WORK/cred"

# 只监听回环的假鉴权服务：$1 = 端口文件, $2 = ok 的真假
cat > "$WORK/stub.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

OK = sys.argv[2] == "allow"

class H(BaseHTTPRequestHandler):
    # 1.1 + Content-Length 才有长连接；1.0 每次都断，压测量到的就是建连成本
    protocol_version = "HTTP/1.1"
    # 关掉 Nagle：头与体分两次 write，不关的话每次应答都撞上 40ms 延迟 ACK
    disable_nagle_algorithm = True

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        body = json.dumps({"ok": True, "id": "u-1"} if OK else {"ok": False}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass

srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1], "w").write(str(srv.server_address[1]))
srv.serve_forever()
PY

start_stub() {
    # $1 = 端口文件, $2 = allow|deny；回显 PID
    local pf=$1 mode=$2 pid i=0
    # stdout 必须重定向：本函数在命令替换里被调用，stub 继承的管道不关就永远读不完
    python3 "$WORK/stub.py" "$pf" "$mode" >"$pf.log" 2>&1 &
    pid=$!
    while [ "$i" -lt 40 ] && [ ! -s "$pf" ]; do
        sleep 0.1
        i=$((i + 1))
    done
    echo "$pid"
}

STUB_PID=$(start_stub "$WORK/port" allow)
PORT=$(cat "$WORK/port" 2>/dev/null)
assert_eq "1" "$([[ -n "$PORT" ]] && echo 1 || echo 0)" "stub 鉴权服务起来了"

OUT="$WORK/ok"
# 阈值放宽到 1s：这里验的是压测与判读的管道，不是这个 python stub 的快慢
# （p99 < 20ms 是对真守护进程的生产判据，见 spec §3.2）
out=$(python3 "$ROOT/scripts/ops/authhttp-bench.py" --rate 50 --seconds 2 --workers 2 \
    --port "$PORT" --cred-file "$WORK/cred" --out "$OUT" --p99-ms 1000 2>&1); rc=$?
assert_eq "0" "$rc" "全放行 → 自带判读 PASS"
assert_contains "p99" "$out" "打印 p99"
assert_contains "PASS" "$out" "结论 PASS"
assert_eq "1" "$([[ -f "$OUT/DONE" ]] && echo 1 || echo 0)" "写了 DONE 标记"
assert_eq "2" "$(find "$OUT" -name 'lat-*.csv' | wc -l)" "每个 worker 一份 CSV"
calls=$(cat "$OUT"/lat-*.csv | grep -c .)
assert_eq "1" "$([[ "$calls" -ge 70 && "$calls" -le 130 ]] && echo 1 || echo 0)" "50/s × 2s ≈ 100 次（实测 $calls）"
assert_eq "1" "$(awk -F, 'NR == 1 {print ($3 + 0 > 0) ? 1 : 0}' "$(find "$OUT" -name 'lat-*.csv' | head -1)")" "延迟记到微秒"

# 同一份产物交给既有的判读脚本
out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT" --rate 50 --p99-ms 1000 2>&1); rc=$?
assert_eq "0" "$rc" "authhook-report.sh 读得懂同一份 CSV"
assert_contains "PASS" "$out" "判读也给 PASS"
out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT" --rate 50 --p99-ms 0 2>&1); rc=$?
assert_eq "1" "$rc" "p99 阈值设为 0 → FAIL"

# 全拒：ok:false 每次都算失败
DENY_PID=$(start_stub "$WORK/port2" deny)
PORT2=$(cat "$WORK/port2" 2>/dev/null)
out=$(python3 "$ROOT/scripts/ops/authhttp-bench.py" --rate 50 --seconds 2 --workers 2 \
    --port "$PORT2" --cred-file "$WORK/cred" --out "$WORK/deny" --p99-ms 1000 2>&1); rc=$?
assert_eq "1" "$rc" "全拒 → FAIL"
assert_contains "FAIL 失败" "$out" "指出失败次数"

# 凭据不进 argv、也不落产物
assert_not_contains "pw-alice-01" "$(cat "$OUT/DONE" "$OUT"/lat-*.csv)" "密码不落产物"
out=$(python3 "$ROOT/scripts/ops/authhttp-bench.py" --rate 1 --seconds 1 \
    --port "$PORT" --cred-file "$WORK/nope" 2>&1); rc=$?
assert_eq "1" "$([[ "$rc" -ne 0 ]] && echo 1 || echo 0)" "读不到凭据就退非零"

finish
