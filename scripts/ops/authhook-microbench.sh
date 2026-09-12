#!/usr/bin/env bash
# v4 M5：auth-hook **每次调用的固定开销**微基准。
#
# 与 authhook-bench.sh 的分工：那一份是定速并发压测（判 M5 的 200 建连/秒 p99），跑在生产机上、
# 读真 state.json；这一份只回答「一次 exec + 读快照 + 判定要多久」，**串行**调用、
# 不出网、不碰 /opt、不碰 systemd，凭据是本脚本自己在 tempdir 里造的假值。
#
# 用法：
#   bash scripts/ops/authhook-microbench.sh [--bin target/.../release/bui] [--count 200] [--csv <path>]
#
# 钩子入口是 argv[0] = bui-auth-hook（内核只接受一个不带参数的可执行路径），所以脚本在
# tempdir 里建一条 bui-auth-hook 符号链接指向 --bin，量到的就是生产那条路径。
# 快照位置靠 BUI_BASE_DIR 覆盖（auth_hook.rs 里那条只为测量存在的环境变量）。
#
# 不用 set -e：单次调用失败要记录并继续。时间用 bash 内建 EPOCHREALTIME（零 fork）。
set -uo pipefail
LC_ALL=C

COUNT=200
WARMUP=20
BIN=""
CSV=""
ADDR="203.0.113.9:54321"
USER_NAME="bench"
# 假密码：本脚本自己写进 tempdir 的快照，不是任何真实凭据
FAKE_PW="microbench-not-a-real-password"

usage() {
    printf '用法：%s [--bin <bui 可执行文件>] [--count 200] [--warmup 20] [--csv <path>]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin) BIN="${2:-}"; shift 2 ;;
        --count) COUNT="${2:-}"; shift 2 ;;
        --warmup) WARMUP="${2:-}"; shift 2 ;;
        --csv) CSV="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done

[[ "$COUNT" =~ ^[0-9]+$ && "$COUNT" -gt 0 ]] || usage
[[ "$WARMUP" =~ ^[0-9]+$ ]] || usage

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
if [[ -z "$BIN" ]]; then
    for c in \
        "$ROOT/target/x86_64-unknown-linux-musl/release/bui" \
        "$ROOT/target/aarch64-unknown-linux-musl/release/bui" \
        "$ROOT/target/release/bui"
    do
        [[ -x "$c" ]] && { BIN="$c"; break; }
    done
fi
[[ -n "$BIN" && -x "$BIN" ]] || { printf '找不到可执行的 bui，用 --bin 指定\n' >&2; exit 2; }
BIN=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")

WORK=$(mktemp -d) || exit 2
trap 'rm -rf "$WORK"' EXIT
HOOK="$WORK/bin/bui-auth-hook"
mkdir -p "$WORK/bin" || exit 2
ln -s "$BIN" "$HOOK" || exit 2

cat > "$WORK/auth-snapshot.json" <<JSON
{
  "schema": 1,
  "users": {
    "$USER_NAME": {
      "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa",
      "hy2_password": "$FAKE_PW",
      "expires_at": null,
      "blocked": false
    }
  }
}
JSON
chmod 600 "$WORK/auth-snapshot.json"
export BUI_BASE_DIR="$WORK"

now_us() {
    local e=$EPOCHREALTIME
    printf '%s' "$(( ${e%.*} * 1000000 + 10#${e#*.} ))"
}

# 先确认量的是「放行」那条完整路径（读到快照 + 比对通过 + 打 user_id + 退 0），
# 否则量到的可能是提前 return 的短路径，数字没有意义。
probe=$("$HOOK" "$ADDR" "$USER_NAME:$FAKE_PW" 0 2>/dev/null); prc=$?
if [[ "$prc" -ne 0 || -z "$probe" ]]; then
    printf '钩子没有放行（rc=%s out=%q）：快照或二进制不对，测不出有效数字\n' "$prc" "$probe" >&2
    exit 2
fi

i=0
while (( i < WARMUP )); do
    "$HOOK" "$ADDR" "$USER_NAME:$FAKE_PW" 0 >/dev/null 2>&1
    i=$(( i + 1 ))
done

samples=()
fails=0
i=0
while (( i < COUNT )); do
    t0=$(now_us)
    "$HOOK" "$ADDR" "$USER_NAME:$FAKE_PW" 0 >/dev/null 2>&1
    rc=$?
    t1=$(now_us)
    (( rc != 0 )) && fails=$(( fails + 1 ))
    samples+=( "$(( t1 - t0 ))" )
    i=$(( i + 1 ))
done

if [[ -n "$CSV" ]]; then
    printf 'n,us\n' > "$CSV"
    i=0
    for s in "${samples[@]}"; do
        printf '%d,%d\n' "$i" "$s" >> "$CSV"
        i=$(( i + 1 ))
    done
fi

sorted=$(printf '%s\n' "${samples[@]}" | sort -n)
sum=0
for s in "${samples[@]}"; do sum=$(( sum + s )); done
pick() { printf '%s\n' "$sorted" | sed -n "$1p"; }
idx() { local p="$1" n="${#samples[@]}" k; k=$(( (n * p + 99) / 100 )); (( k < 1 )) && k=1; (( k > n )) && k=$n; printf '%s' "$k"; }

printf 'bin=%s\n' "$BIN"
printf 'count=%d failures=%d\n' "${#samples[@]}" "$fails"
printf 'avg_us=%d p50_us=%s p95_us=%s p99_us=%s min_us=%s max_us=%s\n' \
    "$(( sum / ${#samples[@]} ))" \
    "$(pick "$(idx 50)")" "$(pick "$(idx 95)")" "$(pick "$(idx 99)")" \
    "$(pick 1)" "$(pick "${#samples[@]}")"
(( fails == 0 )) || { printf '有失败调用，数字不可用\n' >&2; exit 1; }
