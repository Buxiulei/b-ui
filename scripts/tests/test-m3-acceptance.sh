#!/usr/bin/env bash
# m3-acceptance.sh 的 --self-test 必须全绿：四条判据的 PASS 与 FAIL 分支、判定纯函数、
# 面板 API 路径（真 curl + python 面板桩）都在那里守着。本测试跑它并核对摘要、退出码、
# 以及「每条判据的两个分支都有人断言」。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

out=$(bash "$ROOT/scripts/m3-acceptance.sh" --self-test 2>&1)
rc=$?
assert_eq "0" "$rc" "--self-test 退出码 0（退出码 = FAIL 数）"
assert_eq "0" "$(printf '%s\n' "$out" | grep -c '^FAIL')" "自测没有 FAIL 行"
assert_contains "/ 0 FAIL" "$out" "摘要是 0 FAIL"

# 自测里每条判据的断言标签都以「PASS」或「FAIL」收尾，分别对应被测分支
for c in 判据① 判据② 判据③ 判据④; do
    p=$(printf '%s\n' "$out" | grep -c "^PASS.*$c.*PASS$")
    f=$(printf '%s\n' "$out" | grep -c "^PASS.*$c.*FAIL$")
    assert_eq "1" "$([[ "$p" -ge 1 ]] && echo 1 || echo 0)" "$c 的通过分支有断言（实测 $p 条）"
    assert_eq "1" "$([[ "$f" -ge 1 ]] && echo 1 || echo 0)" "$c 的失败分支有断言（实测 $f 条）"
done

out=$(bash "$ROOT/scripts/m3-acceptance.sh" --no-such-flag 2>&1)
rc=$?
assert_eq "2" "$rc" "未知参数退 2"
assert_contains "用法" "$out" "未知参数打印用法"
finish
