#!/usr/bin/env bash
# 判读 soak.csv：RSS 斜率与首末 1/4 均值比、b-ui RSS 上限、窗口内重启、采样覆盖率。
# 可在开发机上对 scp 回来的 CSV 重跑；纯 awk，无依赖。
set -uo pipefail
LC_ALL=C

CSV="${1:-}"
shift || true
RSS_MAX_MB=50
SLOPE_MAX=512
GROWTH=1.10
EXPECT_ROWS=0

if [[ -z "$CSV" || ! -f "$CSV" ]]; then
    printf '用法：%s <soak.csv> [--rss-max-mb 50] [--slope-kb-per-h 512] [--growth 1.10] [--expect-rows <n>]\n' "$0" >&2
    exit 2
fi
while [[ $# -gt 0 ]]; do
    case "$1" in
        --rss-max-mb) RSS_MAX_MB="${2:-}"; shift 2 ;;
        --slope-kb-per-h) SLOPE_MAX="${2:-}"; shift 2 ;;
        --growth) GROWTH="${2:-}"; shift 2 ;;
        --expect-rows) EXPECT_ROWS="${2:-}"; shift 2 ;;
        *) printf '未知参数 %s\n' "$1" >&2; exit 2 ;;
    esac
done

awk -F, -v rss_max_mb="$RSS_MAX_MB" -v slope_max="$SLOPE_MAX" -v growth="$GROWTH" -v expect_rows="$EXPECT_ROWS" '
NR == 1 { next }
{
    u = $3; n[u]++; i = n[u];
    t[u, i] = $2 + 0; r[u, i] = $5 + 0;
    if (r[u, i] > maxr[u]) maxr[u] = r[u, i];
    if (i == 1) nr0[u] = $7 + 0;
    nr1[u] = $7 + 0;
    rows++;
}
END {
    printf "%-22s %8s %11s %11s %12s %8s %8s  %s\n", "unit", "samples", "first_q_kb", "last_q_kb", "slope_kb/h", "max_MB", "restart", "verdict";
    bad = 0;
    for (u in n) {
        q = int(n[u] / 4); if (q < 1) q = 1;
        s1 = 0; for (i = 1; i <= q; i++) s1 += r[u, i]; m1 = s1 / q;
        s2 = 0; for (i = n[u] - q + 1; i <= n[u]; i++) s2 += r[u, i]; m2 = s2 / q;
        sx = 0; sy = 0; sxx = 0; sxy = 0;
        for (i = 1; i <= n[u]; i++) { x = t[u, i]; y = r[u, i]; sx += x; sy += y; sxx += x * x; sxy += x * y }
        d = n[u] * sxx - sx * sx;
        slope = (d > 0) ? (n[u] * sxy - sx * sy) / d * 3600 : 0;
        v = "PASS";
        if (slope > slope_max && m2 > m1 * growth) { v = "FAIL 单调增长"; bad = 1 }
        if (u == "b-ui" && maxr[u] / 1024 > rss_max_mb) { v = (v == "PASS" ? "" : v " ") "FAIL RSS 超限"; bad = 1 }
        if (nr1[u] > nr0[u]) { v = (v == "PASS" ? "" : v " ") "FAIL 重启+" (nr1[u] - nr0[u]); bad = 1 }
        printf "%-22s %8d %11.0f %11.0f %12.0f %8.1f %8d  %s\n", u, n[u], m1, m2, slope, maxr[u] / 1024, nr1[u] - nr0[u], v;
    }
    if (expect_rows > 0) {
        cov = rows / expect_rows * 100;
        printf "\n采样覆盖率：%d/%d = %.1f%%\n", rows, expect_rows, cov;
        if (cov < 95) { printf "FAIL 覆盖率低于 95%%（采样进程中途死过，曲线不可信）\n"; bad = 1 }
    }
    printf "\n结论：%s\n", (bad ? "FAIL" : "PASS");
    exit (bad ? 1 : 0);
}' "$CSV"
