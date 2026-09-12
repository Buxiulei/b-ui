#!/usr/bin/env bash
# 采样脚本跑 2 秒（stub systemctl，指向本进程），判读脚本对两种曲线给出相反结论。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"
cat > "$WORK/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
# show -p MainPID --value <unit> / show -p NRestarts --value <unit>
# MainPID 必须是一个**在采样期间一直活着**的进程：不能用 $PPID（那是 stub 所在命令替换
# 的子外壳，采样脚本读 /proc/<pid>/status 时它已经退出，RSS 会全是 0）。
prop=""; for a in "$@"; do case "$a" in -p) getnext=1 ;; *) [[ "${getnext:-0}" == 1 ]] && { prop="$a"; getnext=0; } ;; esac; done
case "$prop" in
  MainPID) printf '%s\n' "${FAKE_PID:-0}" ;;
  NRestarts) printf '%s\n' "${FAKE_NRESTARTS:-0}" ;;
  *) printf '\n' ;;
esac
STUB
chmod +x "$WORK/bin/systemctl"
export PATH="$WORK/bin:$PATH"

# 一个活得比采样久的真实进程，冒充被采样单元的主进程
sleep 30 &
FAKE_PID=$!
export FAKE_PID
OUT="$WORK/soak"
bash "$ROOT/scripts/ops/soak-sample.sh" --seconds 2 --interval 1 --out "$OUT" --units "xray b-ui" > "$WORK/sample.log" 2>&1
assert_eq "0" "$?" "采样脚本正常退出"
assert_eq "1" "$([[ -f "$OUT/DONE" ]] && echo 1 || echo 0)" "写了 DONE 标记"
assert_contains "rows=" "$(cat "$OUT/DONE")" "DONE 里有行数统计"
assert_eq "ts,elapsed_s,unit,pid,rss_kb,fd,nrestarts,tcp_estab,udp_alloc,load1,mem_avail_kb" \
    "$(head -1 "$OUT/soak.csv")" "CSV 表头固定"
rows=$(tail -n +2 "$OUT/soak.csv" | wc -l)
assert_eq "1" "$([[ "$rows" -ge 4 ]] && echo 1 || echo 0)" "2 秒 × 1 秒 × 2 单元 ≥ 4 行（实测 $rows）"
assert_eq "1" "$(awk -F, 'NR == 2 {print ($5 + 0 > 0) ? 1 : 0}' "$OUT/soak.csv")" "RSS 取到真实值"
assert_eq "2" "$(tail -n +2 "$OUT/soak.csv" | awk -F, '{print $3}' | sort -u | wc -l)" "两个单元都被采到"
kill "$FAKE_PID" 2>/dev/null

# 造两条曲线喂判读脚本
mk_csv() {
    # $1 = 输出文件, $2 = xray 每 tick 增长 kb, $3 = b-ui 基准 kb, $4 = nrestarts 末值
    {
        printf 'ts,elapsed_s,unit,pid,rss_kb,fd,nrestarts,tcp_estab,udp_alloc,load1,mem_avail_kb\n'
        for i in $(seq 0 39); do
            printf '2026-09-12T00:00:00Z,%d,xray,101,%d,50,0,80,16,0.10,900000\n' "$((i * 60))" "$((40000 + i * $2))"
            printf '2026-09-12T00:00:00Z,%d,b-ui,102,%d,40,%d,80,16,0.10,900000\n' "$((i * 60))" "$3" \
                "$([[ "$i" -eq 39 ]] && echo "$4" || echo 0)"
        done
    } > "$1"
}

mk_csv "$WORK/flat.csv" 0 30000 0
out=$(bash "$ROOT/scripts/ops/soak-report.sh" "$WORK/flat.csv" 2>&1); rc=$?
assert_eq "0" "$rc" "平坦曲线 PASS"
assert_contains "PASS" "$out" "输出含 PASS"
assert_contains "xray" "$out" "表里有 xray 行"

mk_csv "$WORK/leak.csv" 400 30000 0
out=$(bash "$ROOT/scripts/ops/soak-report.sh" "$WORK/leak.csv" 2>&1); rc=$?
assert_eq "1" "$rc" "单调增长 FAIL"
assert_contains "FAIL 单调增长" "$out" "指出单调增长"

mk_csv "$WORK/big.csv" 0 60000 0
out=$(bash "$ROOT/scripts/ops/soak-report.sh" "$WORK/big.csv" 2>&1); rc=$?
assert_eq "1" "$rc" "b-ui RSS 超 50MB FAIL"
assert_contains "FAIL RSS 超限" "$out" "指出 RSS 超限"

mk_csv "$WORK/restart.csv" 0 30000 3
out=$(bash "$ROOT/scripts/ops/soak-report.sh" "$WORK/restart.csv" 2>&1); rc=$?
assert_eq "1" "$rc" "窗口内重启 FAIL"
assert_contains "FAIL 重启+3" "$out" "指出重启次数"

out=$(bash "$ROOT/scripts/ops/soak-report.sh" "$WORK/flat.csv" --expect-rows 200 2>&1); rc=$?
assert_eq "1" "$rc" "覆盖率不足 FAIL"
assert_contains "覆盖率" "$out" "指出覆盖率"
finish
