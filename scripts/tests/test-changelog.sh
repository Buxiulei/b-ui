#!/usr/bin/env bash
# CHANGELOG 段落抽取 + 版本号三处一致性。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

CL="$WORK/CHANGELOG.md"
cat > "$CL" <<'EOF'
# 更新日志

## [4.0.1] - 2026-09-25

### 修复
- 修一个东西

## [4.0.0] - 2026-09-18

### 新增
- 单二进制 bui

### 移除
- v3 的 shell 实现
EOF

s=$(bash "$ROOT/scripts/release/changelog-section.sh" 4.0.0 "$CL")
assert_contains "单二进制 bui" "$s" "取到 4.0.0 的新增条目"
assert_contains "v3 的 shell 实现" "$s" "取到 4.0.0 的移除条目"
assert_not_contains "修一个东西" "$s" "不串到 4.0.1 的内容"
assert_not_contains "## [4.0.0]" "$s" "不含标题行"

s=$(bash "$ROOT/scripts/release/changelog-section.sh" 4.0.1 "$CL")
assert_contains "修一个东西" "$s" "取到最后一个版本（文件末尾边界）"

out=$(bash "$ROOT/scripts/release/changelog-section.sh" 9.9.9 "$CL" 2>&1); rc=$?
assert_eq "1" "$rc" "不存在的版本退 1"
assert_contains "找不到版本" "$out" "有中文错误"

# 真实仓库：版本号三处一致
ver=$(awk '/^\[workspace\.package\]/ {f = 1; next} f && /^\[/ {f = 0} f && /^version[[:space:]]*=/ {gsub(/[^0-9.]/, ""); print; exit}' "$ROOT/Cargo.toml")
# 只核格式：具体数字与 CHANGELOG / tag 的一致性由下面的 check-version 用例对真实仓库核对，写死数字只会每次发版都红
assert_eq "1" "$([[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] && echo 1 || echo 0)" "workspace version 是 x.y.z 格式（实测 $ver）"
for c in bui bui-c bui-schema; do
    assert_contains "version.workspace = true" "$(cat "$ROOT/crates/$c/Cargo.toml")" "$c 继承 workspace 版本"
done
bash "$ROOT/scripts/release/check-version.sh" "v$ver"
assert_eq "0" "$?" "check-version 对真实仓库通过"
out=$(bash "$ROOT/scripts/release/check-version.sh" v9.9.9 2>&1); rc=$?
assert_eq "1" "$rc" "tag 与 workspace version 不符退 1"
assert_contains "tag" "$out" "错误提到 tag"
finish
