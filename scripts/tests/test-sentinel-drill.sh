#!/usr/bin/env bash
# sentinel-drill.sh 的 --self-test 必须全绿：四条判据的通过与失败分支、分流模式三个分支、
# --all-ports（整网关不可用 ⇒ 无可用出口）那一套判据、trap 兜底删规则都在那里守着。
# 本测试只跑它并核对摘要、退出码，以及「每条判据的两个分支都有人断言」。全 stub，零网络。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

out=$(bash "$ROOT/scripts/ops/sentinel-drill.sh" --self-test 2>&1)
rc=$?
assert_eq "0" "$rc" "--self-test 退出码 0（退出码 = 自测失败数）"
assert_eq "0" "$(printf '%s\n' "$out" | grep -c '^FAIL 自测')" "自测没有失败项"
assert_contains "自测：" "$out" "打印了自测摘要"
assert_contains "丢包规则已删干净" "$out" "成功路径删规则有断言"
assert_contains "被 TERM 杀掉也删干净" "$out" "trap 兜底有断言"
assert_contains "PASS 自测 分流 split 分支：探测 URL 命中关键字才演练" "$out" "split 命中分支有断言"
assert_contains "PASS 自测 分流 split 分支：不命中关键字 ⇒ FATAL" "$out" "split 不命中分支有断言"
assert_contains "PASS 自测 分流 global 分支" "$out" "global 分支有断言"
assert_contains "PASS 自测 判据② 无出口模式通过分支" "$out" "--all-ports 的终态文案有断言"
assert_contains "PASS 自测 判据③ 无出口模式通过分支" "$out" "--all-ports 的放回本槽有断言"
assert_contains "PASS 自测 判据② 无出口模式失败分支" "$out" "谎报「已临时切到」必须判失败"
assert_contains "PASS 自测 无出口模式：两条丢包规则都删干净" "$out" "--all-ports 的规则清理有断言"
for c in 判据① 判据② 判据③ 判据④; do
    p=$(printf '%s\n' "$out" | grep -c "^PASS 自测 $c 通过分支")
    f=$(printf '%s\n' "$out" | grep -c "^PASS 自测 $c 失败分支")
    assert_eq "1" "$([[ "$p" -ge 1 ]] && echo 1 || echo 0)" "$c 的通过分支有断言（实测 $p 条）"
    assert_eq "1" "$([[ "$f" -ge 1 ]] && echo 1 || echo 0)" "$c 的失败分支有断言（实测 $f 条）"
done

out=$(bash "$ROOT/scripts/ops/sentinel-drill.sh" --no-such-flag 2>&1)
rc=$?
assert_eq "2" "$rc" "未知参数退 2"
assert_contains "用法" "$out" "未知参数打印用法"
finish
