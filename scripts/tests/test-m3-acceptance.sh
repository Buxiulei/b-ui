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
for c in 判据① 判据② 判据③ 判据④ "判据④'" 判据⑤; do
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
# 哨兵的门位同步/重放失败事件 + 未封用户真打一次住宅节点（spec §3.4 / §6）。
# 这两件只读证据是判据 ④'，**默认就跑**：锁在不可逆的 --remove-upstream 后面就等于
# 缺省整跑与 T19 正文那条命令都看不到它们
assert_contains "判据④'（不给 --remove-upstream 也跑：事件干净 + 活体探测通）PASS" "$out" \
    "判据④' 默认就跑（不需要 --remove-upstream）有断言"
assert_contains "判据④'（哨兵报了门位同步/重放失败）FAIL" "$out" "门位重放失败事件有失败分支"
assert_contains "判据④'（读不到 bui incidents）FAIL" "$out" "读不到事件表算失败而不是干净"
assert_contains "判据④'（未封用户的活体探测不通）FAIL" "$out" "门位活体探测有失败分支"
assert_contains "判据④' 只看脚本开跑之后的门位事件" "$out" "事件窗口的起点有断言"
assert_contains "判据④' 的事件窗口起点早于脚本的一切动作" "$out" "事件窗口起点的方向有断言"
assert_contains "窗口起点不许挪到删上游/门位收敛之后" "$out" \
    "起点挪到 sleep GATE_WAIT 之后这种变异有用例钉住"
# 删到池空 ⇒ 中继 fail-open 全部直连，活体探测无论门位对不对都 200（假 PASS）⇒ 必须 SKIP
assert_contains "池空 ⇒ 门位活体探测 SKIP" "$out" "池空时活体探测 SKIP 有断言"
assert_contains "住宅开关关着（池无效）⇒ 门位活体探测 SKIP" "$out" \
    "池无效（开关关着）时活体探测 SKIP 有断言"
assert_contains "读不到上游数 ⇒ 门位活体探测 SKIP" "$out" "读不到上游数时活体探测 SKIP 有断言"
# 判据④' 是独立的一项：它的两件证据不许再出现在删上游那一步里（回退就等于又被锁上）
assert_eq "1" "$(grep -c '^check_gate_live() {' "$ROOT/scripts/m3-acceptance.sh")" \
    "活体证据独立成 check_gate_live"
assert_eq "1" "$(grep -c '^  check_gate_live$' "$ROOT/scripts/m3-acceptance.sh")" \
    "check_gate_live 进了 run_checks（默认整跑就跑）"

# CLAUDE.md 的 scripts/ 约定：`#!/usr/bin/env bash` + `LC_ALL=C`
assert_eq "1" "$(grep -c '^LC_ALL=C$' "$ROOT/scripts/m3-acceptance.sh")" "脚本带 LC_ALL=C"

# 缺省保活源不许是脚本自己注释里记着「rick 实测 403」的那个（判据 ①③④' 都拿 200 当通路证据）
assert_eq "0" "$(grep -c '^KEEP_URL=.*speed\.cloudflare\.com' "$ROOT/scripts/m3-acceptance.sh")" \
    "缺省 --keep-url 不指向实测 403 的源"

# 帮助文本要点明两件真机上会踩的事：--keep-url 的源必须实测 200、--remove-upstream 只在
# 池里 ≥2 条上游时开
out2=$(bash "$ROOT/scripts/m3-acceptance.sh" --keep-url 2>&1)
assert_contains "实测回 200" "$out2" "用法点明 --keep-url 要钉一个实测 200 的源"
assert_contains "≥2 条上游" "$out2" "用法点明 --remove-upstream 只在池里 ≥2 条上游时用"

# 归零测量的三个辅助（复核里四个变异全存活的那几处）
assert_contains "online_count 回显空（fail-closed）" "$out" "online_count 读不到时不当成归零"
assert_contains "只读到一次 0 ⇒ 不算归零" "$out" "wait_online_zero 的连续轮数有断言"
# 判据③ 住宅到期改用 probe_until_fail（门切 deny 有 ~10 秒采样轮延迟，不能「一看到 200 就 FAIL」）：
# PASS 分支（收敛前 200 不误判）、门没切的 FAIL、抖不收敛的 FAIL、失败码回显四条都要有断言，
# 且脚本里不许再残留旧的 probe_all_fail。
assert_contains "probe_until_fail 收敛前的 200 不误判" "$out" "probe_until_fail 的 PASS 分支有断言"
assert_contains "probe_until_fail 门没切（一直 200）到窗口末回 200（FAIL）" "$out" "门没切的 FAIL 分支有断言"
assert_contains "probe_until_fail 凑不满连续失败（一直抖）判 FAIL" "$out" "连续 N 次失败判据有断言"
assert_contains "probe_until_fail 稳定失败时回最后一次的码（403）" "$out" "失败码回显有断言"
assert_eq "0" "$(grep -c 'probe_all_fail' "$ROOT/scripts/m3-acceptance.sh")" \
    "脚本里没有旧 probe_all_fail 的残留（判据③ 住宅那半已换成 probe_until_fail）"

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
