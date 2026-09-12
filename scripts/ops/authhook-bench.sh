#!/usr/bin/env bash
# v4 M5：auth-hook 定速压测。多 worker 按固定速率调用钩子，逐次记录微秒级耗时与退出码；
# 旁路 watcher 采样守护进程 fd 与系统进程数峰值。
# 生产用法（bwg-rick，root）：
#   nohup bash /opt/b-ui/ops/authhook-bench.sh --rate 200 --seconds 300 --out /var/log/bui-bench >/dev/null 2>&1 &
# 不用 set -e：单次调用失败要记录并继续。时间用 bash 内建 EPOCHREALTIME（零 fork）。
set -uo pipefail
LC_ALL=C

RATE=200
SECS=300
WORKERS=8
HOOK="/opt/b-ui/bin/bui"
HOOK_SUB="auth-hook"
ARGV_TPL="${BUI_HOOK_ARGV:-%ADDR% %AUTH% 0}"
ADDR="203.0.113.9:54321"
CRED_FILE=""
STATE="/opt/b-ui/state.json"
OUT="/var/log/bui-bench"
UNIT="b-ui"

usage() {
    printf '用法：%s [--rate 200] [--seconds 300] [--workers 8] [--hook <path>] [--cred-file <path>] [--state <path>] [--argv-template "%%ADDR%% %%AUTH%% 0"] [--out <dir>]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rate) RATE="${2:-}"; shift 2 ;;
        --seconds) SECS="${2:-}"; shift 2 ;;
        --workers) WORKERS="${2:-}"; shift 2 ;;
        --hook) HOOK="${2:-}"; HOOK_SUB=""; shift 2 ;;
        --cred-file) CRED_FILE="${2:-}"; shift 2 ;;
        --state) STATE="${2:-}"; shift 2 ;;
        --argv-template) ARGV_TPL="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --unit) UNIT="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done

read_cred() {
    # stdout = "user:password"；只读文件，不进 argv、不 echo。
    # state.json 的字段名来自总纲 C1（users[].username / users[].disabled /
    # users[].credentials.hy2_password），不是本脚本自己发明的。
    # 先 tr 把 , { } 换成换行，这样 serde 输出是紧凑还是 pretty 都能解析；
    # node / admin 段没有 username 字段，residential 的 upstream username 排在 users 之后，
    # 所以「第一个 username 之后的第一个 hy2_password」必定属于同一个用户。
    local line
    if [[ -n "$CRED_FILE" ]]; then
        [[ -r "$CRED_FILE" ]] || { printf '读不到 --cred-file %s\n' "$CRED_FILE" >&2; return 1; }
        line=$(head -1 "$CRED_FILE")
    elif [[ -r "$STATE" ]]; then
        line=$(tr ',{}' '\n\n\n' < "$STATE" | awk '
            /"username"[[:space:]]*:/ {
                u = $0; sub(/.*"username"[[:space:]]*:[[:space:]]*"/, "", u); sub(/".*/, "", u); d = 0; next
            }
            /"disabled"[[:space:]]*:[[:space:]]*true/ { d = 1; next }
            /"hy2_password"[[:space:]]*:/ {
                if (u != "" && d == 0) {
                    p = $0; sub(/.*"hy2_password"[[:space:]]*:[[:space:]]*"/, "", p); sub(/".*/, "", p)
                    printf "%s:%s\n", u, p; exit
                }
            }
        ')
    else
        printf '没有 --cred-file，且读不到 %s\n' "$STATE" >&2
        return 1
    fi
    [[ "$line" == *:* ]] || { printf '凭据格式应为 user:password（密码含 , { } 时用 --cred-file）\n' >&2; return 1; }
    printf '%s\n' "$line"
}

now_us() {
    local e=$EPOCHREALTIME
    printf '%s' "$(( ${e%.*} * 1000000 + 10#${e#*.} ))"
}

build_argv() {
    # 把模板展开成数组（全局 ARGV）。**先按模板分词，再逐 token 替换占位符**：
    # 反过来写（先替换再 ARGV=($tpl)）会让密码里的空格被再分一次词、里面的 * ? [ ] 被 glob 展开。
    # set -f 关掉 glob 是为了模板本身万一带通配符也不展开。
    local -a raw=()
    local tok
    set -f
    # shellcheck disable=SC2206
    raw=($ARGV_TPL)
    set +f
    ARGV=()
    for tok in "${raw[@]}"; do
        tok="${tok//%ADDR%/$ADDR}"
        tok="${tok//%AUTH%/$AUTH}"
        ARGV+=("$tok")
    done
    if [[ -n "$HOOK_SUB" ]]; then
        ARGV=("$HOOK_SUB" "${ARGV[@]}")
    fi
}

worker() {
    # $1 = worker 编号, $2 = 本 worker 速率, $3 = 秒数, $4 = CSV
    local id="$1" wrate="$2" secs="$3" csv="$4"
    local interval_us start end n=0 t0 t1 rc out due wait_us
    interval_us=$(( 1000000 / wrate ))
    build_argv
    start=$(now_us)
    end=$(( start + secs * 1000000 ))
    while :; do
        due=$(( start + n * interval_us ))
        t0=$(now_us)
        if (( due > t0 )); then
            wait_us=$(( due - t0 ))
            sleep "$(( wait_us / 1000000 )).$(printf '%06d' $(( wait_us % 1000000 )))"
            t0=$(now_us)
        fi
        (( t0 >= end )) && break
        out=$("$HOOK" "${ARGV[@]}" 2>/dev/null)
        rc=$?
        t1=$(now_us)
        printf '%s,%d,%d,%d,%s\n' "$id" "$n" "$(( t1 - t0 ))" "$rc" "${out:0:32}" >> "$csv"
        n=$(( n + 1 ))
    done
}

watcher() {
    # $1 = 秒数, $2 = CSV
    local secs="$1" csv="$2" start now pid fd procs rss
    printf 'ts,elapsed_s,daemon_fd,proc_count,daemon_rss_kb\n' > "$csv"
    start=$(date +%s)
    while :; do
        now=$(date +%s)
        (( now - start >= secs )) && break
        pid=$(systemctl show -p MainPID --value "$UNIT" 2>/dev/null)
        [[ -n "$pid" && "$pid" != "0" && -d "/proc/$pid" ]] || pid=""
        if [[ -n "$pid" ]]; then
            fd=$(ls -U "/proc/$pid/fd" 2>/dev/null | wc -l)
            rss=$(awk '/^VmRSS:/ {print $2; exit}' "/proc/$pid/status" 2>/dev/null)
        else
            fd=0; rss=0
        fi
        procs=$(find /proc -maxdepth 1 -regextype posix-extended -regex '/proc/[0-9]+' 2>/dev/null | wc -l)
        printf '%s,%d,%s,%s,%s\n' "$(date -u +%FT%TZ)" "$(( now - start ))" "${fd:-0}" "${procs:-0}" "${rss:-0}" >> "$csv"
        sleep 0.5
    done
}

main() {
    local per_worker watch_pid w calls fails
    AUTH=$(read_cred) || exit 2
    mkdir -p "$OUT" || exit 2
    DONE="$OUT/DONE"
    rm -f "$DONE" "$OUT"/lat-*.csv "$OUT/watch.csv"
    printf '%s\n' "$$" > "$OUT/bench.pid"

    per_worker=$(( RATE / WORKERS ))
    (( per_worker < 1 )) && per_worker=1
    watcher "$SECS" "$OUT/watch.csv" &
    watch_pid=$!
    wpids=()
    for w in $(seq 1 "$WORKERS"); do
        worker "$w" "$per_worker" "$SECS" "$OUT/lat-$w.csv" &
        wpids+=("$!")
    done
    wait "${wpids[@]}"
    wait "$watch_pid"

    calls=$(cat "$OUT"/lat-*.csv 2>/dev/null | grep -c .)
    fails=$(awk -F, '$4 != 0 {c++} END {print c + 0}' "$OUT"/lat-*.csv 2>/dev/null)
    printf 'finished=%s target_rate=%s workers=%s seconds=%s calls=%s failures=%s\n' \
        "$(date -u +%FT%TZ)" "$RATE" "$WORKERS" "$SECS" "$calls" "$fails" > "$DONE"
    rm -f "$OUT/bench.pid"
}

if [[ "${BUI_BENCH_SOURCED:-0}" != "1" ]]; then
    main
fi
