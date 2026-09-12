#!/usr/bin/env bash
# 判读 auth-hook 压测结果：延迟分位、失败数、实际速率、fd/进程峰值。纯 sort + awk。
set -uo pipefail
LC_ALL=C

DIR="${1:-}"
shift || true
P99_MS=20
RATE=200
MIN_RATE_PCT=95

if [[ -z "$DIR" || ! -d "$DIR" ]]; then
    printf '用法：%s <out-dir> [--p99-ms 20] [--rate 200] [--min-rate-pct 95]\n' "$0" >&2
    exit 2
fi
while [[ $# -gt 0 ]]; do
    case "$1" in
        --p99-ms) P99_MS="${2:-}"; shift 2 ;;
        --rate) RATE="${2:-}"; shift 2 ;;
        --min-rate-pct) MIN_RATE_PCT="${2:-}"; shift 2 ;;
        *) printf '未知参数 %s\n' "$1" >&2; exit 2 ;;
    esac
done

secs=$(awk '{for (i = 1; i <= NF; i++) if ($i ~ /^seconds=/) {sub(/seconds=/, "", $i); print $i; exit}}' "$DIR/DONE" 2>/dev/null)
[[ -n "$secs" ]] || secs=0

sorted=$(mktemp)
trap 'rm -f "$sorted"' EXIT
cat "$DIR"/lat-*.csv 2>/dev/null | grep . | sort -t, -k3 -n > "$sorted"
total=$(grep -c . "$sorted" || true)
if [[ "${total:-0}" -eq 0 ]]; then
    printf '没有延迟样本：%s/lat-*.csv 为空\n' "$DIR" >&2
    exit 1
fi

peak_fd=$(awk -F, 'NR > 1 && $3 + 0 > m {m = $3 + 0} END {print m + 0}' "$DIR/watch.csv" 2>/dev/null)
peak_proc=$(awk -F, 'NR > 1 && $4 + 0 > m {m = $4 + 0} END {print m + 0}' "$DIR/watch.csv" 2>/dev/null)
peak_rss=$(awk -F, 'NR > 1 && $5 + 0 > m {m = $5 + 0} END {print m + 0}' "$DIR/watch.csv" 2>/dev/null)

awk -F, -v n="$total" -v p99_ms="$P99_MS" -v rate="$RATE" -v secs="$secs" -v minpct="$MIN_RATE_PCT" \
    -v pfd="${peak_fd:-0}" -v pproc="${peak_proc:-0}" -v prss="${peak_rss:-0}" '
function idx(p) { i = int(n * p / 100 + 0.999); return (i < 1 ? 1 : (i > n ? n : i)) }
{ lat[NR] = $3 + 0; if ($4 + 0 != 0) fails++; if ($5 == "") empty++ }
END {
    p50 = lat[idx(50)] / 1000; p95 = lat[idx(95)] / 1000; p99 = lat[idx(99)] / 1000; mx = lat[n] / 1000;
    actual = (secs > 0) ? n / secs : 0;
    pct = (rate > 0) ? actual / rate * 100 : 0;
    printf "调用数 %d  失败 %d  空 stdout %d\n", n, fails + 0, empty + 0;
    printf "延迟 p50=%.2fms p95=%.2fms p99=%.2fms max=%.2fms\n", p50, p95, p99, mx;
    printf "实际速率 %.1f/s（目标 %s/s，%.1f%%）\n", actual, rate, pct;
    printf "峰值 fd=%d 进程数=%d 守护进程 RSS=%.1fMB\n", pfd, pproc, prss / 1024;
    bad = 0;
    if (p99 >= p99_ms) { printf "FAIL p99 %.2fms 不低于阈值 %sms\n", p99, p99_ms; bad = 1 }
    if (fails + 0 > 0) { printf "FAIL 失败调用 %d 次（放行路径必须 100%% 成功）\n", fails; bad = 1 }
    if (empty + 0 > 0) { printf "FAIL %d 次 stdout 为空（钩子没返回 user_id）\n", empty; bad = 1 }
    if (pct < minpct) { printf "FAIL 实际速率只有目标的 %.1f%%（压测机自身是瓶颈，结果不可用）\n", pct; bad = 1 }
    printf "\n结论：%s\n", (bad ? "FAIL" : "PASS");
    exit (bad ? 1 : 0);
}' "$sorted"
