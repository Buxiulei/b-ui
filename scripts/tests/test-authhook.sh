#!/usr/bin/env bash
# 压测脚本跑 2 秒（hook 用本地 stub），判读脚本对成功/失败两种数据给出相反结论。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/hook-ok" <<'STUB'
#!/usr/bin/env bash
printf 'u-1\n'
STUB
cat > "$WORK/hook-deny" <<'STUB'
#!/usr/bin/env bash
exit 1
STUB
chmod +x "$WORK/hook-ok" "$WORK/hook-deny"
printf 'alice:pw-alice-01\n' > "$WORK/cred"
chmod 600 "$WORK/cred"

OUT="$WORK/ok"
bash "$ROOT/scripts/ops/authhook-bench.sh" --rate 50 --seconds 2 --workers 2 \
    --hook "$WORK/hook-ok" --cred-file "$WORK/cred" --out "$OUT" > "$WORK/bench.log" 2>&1
assert_eq "0" "$?" "压测脚本正常退出"
assert_eq "1" "$([[ -f "$OUT/DONE" ]] && echo 1 || echo 0)" "写了 DONE 标记"
assert_eq "2" "$(find "$OUT" -name 'lat-*.csv' | wc -l)" "每个 worker 一份 CSV"
calls=$(cat "$OUT"/lat-*.csv | grep -c .)
assert_eq "1" "$([[ "$calls" -ge 70 && "$calls" -le 130 ]] && echo 1 || echo 0)" "50/s × 2s ≈ 100 次（实测 $calls）"
assert_eq "1" "$([[ -s "$OUT/watch.csv" ]] && echo 1 || echo 0)" "watch.csv 有内容"
assert_eq "1" "$(awk -F, 'NR == 1 {print ($3 + 0 > 0) ? 1 : 0}' "$(find "$OUT" -name 'lat-*.csv' | head -1)")" "延迟记到微秒"

out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT" --rate 50 2>&1); rc=$?
assert_eq "0" "$rc" "全成功 → PASS"
assert_contains "p99" "$out" "打印 p99"
assert_contains "PASS" "$out" "结论 PASS"

out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT" --rate 50 --p99-ms 0 2>&1); rc=$?
assert_eq "1" "$rc" "p99 阈值设为 0 → FAIL"
assert_contains "FAIL p99" "$out" "指出 p99 超标"

OUT2="$WORK/deny"
bash "$ROOT/scripts/ops/authhook-bench.sh" --rate 50 --seconds 2 --workers 2 \
    --hook "$WORK/hook-deny" --cred-file "$WORK/cred" --out "$OUT2" > "$WORK/bench2.log" 2>&1
out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT2" --rate 50 2>&1); rc=$?
assert_eq "1" "$rc" "hook 全拒 → FAIL"
assert_contains "FAIL 失败调用" "$out" "指出失败调用数"

out=$(bash "$ROOT/scripts/ops/authhook-report.sh" "$OUT" --rate 5000 2>&1); rc=$?
assert_eq "1" "$rc" "实际速率远低于目标 → FAIL"
assert_contains "FAIL 实际速率" "$out" "指出速率不足"

# 凭据不进 argv：DONE 与日志里都不能出现密码
assert_not_contains "pw-alice-01" "$(cat "$OUT/DONE" "$WORK/bench.log")" "密码不落日志"
out=$(bash "$ROOT/scripts/ops/authhook-bench.sh" --rate 1 --seconds 1 --hook "$WORK/hook-ok" \
    --state "$WORK/nope.json" --out "$WORK/nocred" 2>&1); rc=$?
assert_eq "2" "$rc" "没有可用凭据退 2"

# ---- read_cred / build_argv 单测（源入，不跑 main）----
BUI_BENCH_SOURCED=1
# shellcheck source=/dev/null
. "$ROOT/scripts/ops/authhook-bench.sh"
# 被测脚本是 set -uo pipefail（无 -e），但 -u 会打死测试里的空变量引用，关掉
set +u

# 紧凑一行的 state.json：第一个用户 disabled，必须跳过
CRED_FILE=""
STATE="$WORK/state-compact.json"
cat > "$STATE" <<'EOF'
{"schema_version":1,"node":{"name":"bwg-rick","domain":"example.com"},"admin":{"password_hash":"h","jwt_secret":"s"},"users":[{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000a1","username":"ghost","note":"","created_at":"2026-09-11T00:00:00Z","disabled":true,"credentials":{"hy2_password":"pw-ghost","vless_uuid":"11111111-1111-4111-8111-111111111111"}},{"user_id":"8d5a1a1e-3b2c-4d1e-9f00-0000000000a2","username":"bob","note":"","created_at":"2026-09-11T00:00:00Z","disabled":false,"credentials":{"hy2_password":"pw-bob-02","vless_uuid":"22222222-2222-4222-8222-222222222222"}}],"residential":{"groups":{"default":{"upstreams":[{"username":"resi-u","password":"resi-p"}]}}}}
EOF
assert_eq "bob:pw-bob-02" "$(read_cred)" "state.json：跳过 disabled 用户，取第一个可用用户"

# pretty 排版的同一份数据：解析结果必须一样（tr ',{}' 归一化的作用）
STATE="$WORK/state-pretty.json"
cat > "$STATE" <<'EOF'
{
  "schema_version": 1,
  "admin": { "password_hash": "h", "jwt_secret": "s" },
  "users": [
    {
      "username": "bob",
      "disabled": false,
      "credentials": {
        "hy2_password": "pw-bob-02",
        "vless_uuid": "22222222-2222-4222-8222-222222222222"
      }
    }
  ]
}
EOF
assert_eq "bob:pw-bob-02" "$(read_cred)" "state.json 换成 pretty 排版结果不变"

# --cred-file 优先于 state.json
CRED_FILE="$WORK/cred"
assert_eq "alice:pw-alice-01" "$(read_cred)" "--cred-file 优先"

# build_argv：密码里的空格与通配符不能被再分词 / glob 展开
AUTH='alice:pw a*b'
ADDR="203.0.113.9:54321"
ARGV_TPL='%ADDR% %AUTH% 0'
HOOK_SUB=""
build_argv
assert_eq "3" "${#ARGV[@]}" "模板三段就是三个参数（密码含空格也不多分）"
assert_eq "alice:pw a*b" "${ARGV[1]}" "密码原样进参数，* 没被路径展开"
HOOK_SUB="auth-hook"
build_argv
assert_eq "auth-hook" "${ARGV[0]}" "带子命令时子命令在最前"
assert_eq "4" "${#ARGV[@]}" "子命令 + 三段"
finish
