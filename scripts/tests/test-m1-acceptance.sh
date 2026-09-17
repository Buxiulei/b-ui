#!/usr/bin/env bash
# m1-acceptance.sh 的 --self-test 必须全绿：八步判据的纯函数（体检 / 站点解析 /
# Hysteria2 三条探测 / 住宅日志提取 / 槽位表 / 订阅端口与 mport / 游离监听 / nft 表）
# **与编排层**（每个 check_* 的判据接没接上、PASS / FAIL / SKIP 怎么分派）都在那里守着；
# $BUI / systemctl / ss / nft / 客户端 / curl / journald 全是桩，**一个网络包都不发**。
# 本测试跑它并核对摘要与退出码（与 test-m3-acceptance.sh 同形）。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

out=$(bash "$ROOT/scripts/m1-acceptance.sh" --self-test 2>&1)
rc=$?
assert_eq "0" "$rc" "--self-test 退出码 0"
assert_eq "0" "$(printf '%s\n' "$out" | grep -c '^FAIL')" "自测没有 FAIL 行"
assert_contains "/ 0 FAIL" "$out" "摘要是 0 FAIL"

# 4.1：住宅只有一个实例、一个监听端口，按槽的实例名与端口换算都已退役
assert_eq "0" "$(grep -c 'hysteria-residential-' "$ROOT/scripts/m1-acceptance.sh")" \
    "脚本里没有按槽的住宅单元名"
assert_eq "0" "$(grep -c '9998' "$ROOT/scripts/m1-acceptance.sh")" \
    "脚本里没有 4.0 的住宅计量端口"

# 编排层的最后一环：每一步真的在 run_checks 里被调用。`--self-test` 只跑各 check_* 与
# 判定函数、不跑 run_checks，所以把其中一行调用整条删掉自测照样全绿 —— 只能在这里钉。
body=$(sed -n '/^run_checks()/,/^}$/p' "$ROOT/scripts/m1-acceptance.sh")
for fn in check_external_sites check_hy2_auth check_slots check_nft; do
    assert_eq "1" "$(printf '%s\n' "$body" | grep -cE "^  $fn( |\$)")" "run_checks 调用 $fn"
done
finish
