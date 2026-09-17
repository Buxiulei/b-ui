#!/usr/bin/env bash
# m3-acceptance.sh 的 --self-test 必须全绿：五条判据的 PASS 与 FAIL 分支、判定纯函数、
# 面板 API 路径（真 curl + python 面板桩）都在那里守着。本测试跑它并核对摘要、退出码、
# 「每条判据的两个分支都有人断言」，以及 4.1 换掉的那几处口径（固定三项内核单元、
# 住宅到期的新语义）没被悄悄改回去。
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
# SKIP 必须进摘要并计数：判据 ②③ 共用「取不到住宅 HY2 凭据」这一条静默跳过路径，
# 不计数的话它一跳就是「N PASS / 0 FAIL」+ 退 0 的假绿
assert_contains "/ 0 SKIP" "$out" "摘要带 SKIP 计数"
assert_contains "自测：skip 计数（SKIP 进摘要）" "$out" "skip 会累加计数有断言"
assert_contains "--strict 把 SKIP 也算进退出码" "$out" "--strict 的退出码语义有断言"
assert_contains "判据②（/api/nodes 里没有住宅 HY2 节点）SKIP" "$out" \
    "判据② 的静默 SKIP 路径有用例"

# 自测里每条判据的断言标签都以「PASS」或「FAIL」收尾，分别对应被测分支
for c in 判据① 判据② 判据③ 判据④ 判据⑤; do
    p=$(printf '%s\n' "$out" | grep -c "^PASS.*$c.*PASS$")
    f=$(printf '%s\n' "$out" | grep -c "^PASS.*$c.*FAIL$")
    assert_eq "1" "$([[ "$p" -ge 1 ]] && echo 1 || echo 0)" "$c 的通过分支有断言（实测 $p 条）"
    assert_eq "1" "$([[ "$f" -ge 1 ]] && echo 1 || echo 0)" "$c 的失败分支有断言（实测 $f 条）"
done

# 4.1 的语义变化（spec §6）：住宅那一侧到期后**握手仍成功、请求全被拒**，而不是握手即拒
assert_contains "判据③（握手成功但流被拒）PASS" "$out" "判据③ 的 4.1 新语义有专门的通过分支"
assert_contains "判据③（连接没归零）FAIL" "$out" "门没切干净（/api/online 没归零）有失败分支"
assert_contains "判据③（住宅握手被拒）FAIL" "$out" "住宅连握手都没成功也算失败"
assert_contains "内核单元固定三项" "$out" "KERNEL_UNITS 回固定三项有断言"

# 判据④ 的门位那一项只比「面板算出的期望门位」（真机上几乎恒真）⇒ 必须另有活体证据：
# 哨兵的门位同步/重放失败事件 + 未封用户真打一次住宅节点（spec §3.4 / §6）
assert_contains "判据④（哨兵报了门位同步/重放失败）FAIL" "$out" "门位重放失败事件有失败分支"
assert_contains "判据④（读不到 bui incidents）FAIL" "$out" "读不到事件表算失败而不是干净"
assert_contains "判据④（未封用户的活体探测不通）FAIL" "$out" "门位活体探测有失败分支"
assert_contains "判据④ 只看删上游之后的门位事件" "$out" "事件窗口的起点有断言"

# 归零测量的三个辅助（复核里四个变异全存活的那几处）
assert_contains "online_count 回显空（fail-closed）" "$out" "online_count 读不到时不当成归零"
assert_contains "只读到一次 0 ⇒ 不算归零" "$out" "wait_online_zero 的连续轮数有断言"
assert_contains "probe_all_fail 中途成功过一次就回 200" "$out" "probe_all_fail 的早退有断言"

# 4.0.x 的按槽枚举彻底退场：带后缀的单元名、住宅那个 trafficStats 端口、
# 两个按槽展开的函数都不许再出现在脚本里（spec §2.5 / §5.1）
for pat in 'hysteria-residential-' '9998' 'resi_units' 'load_kernel_units'; do
    assert_eq "0" "$(grep -c -- "$pat" "$ROOT/scripts/m3-acceptance.sh")" \
        "脚本里没有 $pat 的残留"
done

out=$(bash "$ROOT/scripts/m3-acceptance.sh" --no-such-flag 2>&1)
rc=$?
assert_eq "2" "$rc" "未知参数退 2"
assert_contains "用法" "$out" "未知参数打印用法"
assert_contains "--strict" "$out" "用法里有 --strict"
finish
