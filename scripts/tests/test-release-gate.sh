#!/usr/bin/env bash
# 发布 tag 门禁：受管 tag 前缀的必需提交必须是 HEAD 的祖先。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
GATE="$ROOT/scripts/release/check-release-gate.sh"
G=(-c user.email=t@example.com -c user.name=t -c commit.gpgsign=false)

repo="$WORK/repo"
mkdir -p "$repo"
git -C "$repo" init -q -b main
git -C "$repo" "${G[@]}" commit -q --allow-empty -m base
base=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" "${G[@]}" commit -q --allow-empty -m head
git -C "$repo" checkout -q -b side "$base"
git -C "$repo" "${G[@]}" commit -q --allow-empty -m side
side=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" checkout -q main

req="$WORK/required.env"
run_gate() { bash "$GATE" "$@" --repo "$repo" --required "$req" 2>&1; }

printf 'GATE_v4_1=%s\n' "$base" > "$req"
out=$(run_gate v4.1.0-rc1); rc=$?
assert_eq "0" "$rc" "必需提交是祖先 ⇒ 退 0"
assert_contains "门禁通过" "$out" "说通过"

out=$(run_gate v4.0.2); rc=$?
assert_eq "0" "$rc" "不在门禁范围的 tag ⇒ 退 0"
assert_contains "不在门禁范围" "$out" "说明为什么放过"

printf 'GATE_v4_1=%s\n' "$side" > "$req"
out=$(run_gate v4.1.0); rc=$?
assert_eq "3" "$rc" "必需提交在旁支 ⇒ 退 3"
assert_contains "不是 HEAD 的祖先" "$out" "文案点名祖先判据"
assert_contains "${side:0:12}" "$out" "文案带提交的短 sha"
out=$(run_gate v4.1.0 --warn); rc=$?
assert_eq "0" "$rc" "--warn 只告警不卡"
assert_contains "::warning::" "$out" "warn 模式走 GitHub 注解"

printf 'GATE_v4_1=pending\n' > "$req"
out=$(run_gate v4.1.0-rc3); rc=$?
assert_eq "3" "$rc" "清单还是 pending ⇒ 退 3（fail-closed）"
assert_contains "门禁未配置" "$out" "说清要填什么"

printf 'GATE_v4_1=%s\n' "0000000000000000000000000000000000000000" > "$req"
out=$(run_gate v4.1.1); rc=$?
assert_eq "3" "$rc" "清单里的提交本仓库没有 ⇒ 退 3"
assert_contains "fetch-depth" "$out" "提示浅克隆这个坑"

out=$(bash "$GATE" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 tag ⇒ 用法错误退 2"
out=$(run_gate 4.1.0); rc=$?
assert_eq "2" "$rc" "tag 形状不对 ⇒ 退 2"

# 门禁只有接进 workflow 才有用，而这两处最容易在改 workflow 时静默丢掉：
# 丢了 fetch-depth: 0 ⇒ 浅克隆拿不到祖先关系，门禁一律误判成「提交在本仓库里找不到」
#（release.yml 里是错理由地卡住发版，ci.yml 里是 --warn 下静默放过）。所以连 job 块一起钉。
job_block() { awk -v j="  $2:" '$0 == j {f = 1; next} f && /^  [a-z]/ {exit} f' "$1"; }

rel=$(job_block "$ROOT/.github/workflows/release.yml" verify)
assert_contains "fetch-depth: 0" "$rel" "release.yml 的 verify job 取全量历史"
assert_contains 'bash scripts/release/check-release-gate.sh "$TAG"' "$rel" "release.yml 的 verify job 跑硬门禁"
assert_contains "TAG: \${{ steps.v.outputs.tag }}" "$rel" "tag 经 env 进 shell，不插值进 run"

lint=$(job_block "$ROOT/.github/workflows/ci.yml" lint)
assert_contains "fetch-depth: 0" "$lint" "ci.yml 的 lint job 取全量历史"
assert_contains 'check-release-gate.sh "v$ver" --warn' "$lint" "ci.yml 只做 --warn 预检，不卡 CI"
finish
