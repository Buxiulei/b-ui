#!/usr/bin/env bash
# 抽取 CHANGELOG.md 里某个版本的正文（不含标题行），供 Release notes 用。
set -uo pipefail
LC_ALL=C

VER="${1:-}"
CL="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/CHANGELOG.md}"
if [[ -z "$VER" || ! -f "$CL" ]]; then
    printf '用法：%s <x.y.z> [CHANGELOG.md]\n' "$0" >&2
    exit 2
fi

body=$(awk -v v="$VER" '
    $0 ~ ("^## \\[" v "\\]") { f = 1; next }
    f && /^## \[/ { exit }
    f { print }
' "$CL")

if [[ -z "${body//[[:space:]]/}" ]]; then
    printf '找不到版本 %s 的段落：%s\n' "$VER" "$CL" >&2
    exit 1
fi
printf '%s\n' "$body"
