#!/usr/bin/env bash
# 校验版本号三处一致：workspace Cargo.toml / 三个 crate / CHANGELOG.md（可选再比对 tag）。
set -uo pipefail
LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TAG="${1:-}"
rc=0

ver=$(awk '/^\[workspace\.package\]/ {f = 1; next} f && /^\[/ {f = 0} f && /^version[[:space:]]*=/ {gsub(/[^0-9.]/, ""); print; exit}' "$ROOT/Cargo.toml")
if [[ ! "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Cargo.toml 的 [workspace.package] version 不是 semver：%s\n' "$ver" >&2
    exit 1
fi
printf 'workspace version = %s\n' "$ver"

for c in bui bui-c bui-schema; do
    if ! grep -qE '^version\.workspace[[:space:]]*=[[:space:]]*true' "$ROOT/crates/$c/Cargo.toml"; then
        printf 'crates/%s/Cargo.toml 必须用 version.workspace = true\n' "$c" >&2
        rc=1
    fi
done

if ! bash "$ROOT/scripts/release/changelog-section.sh" "$ver" > /dev/null 2>&1; then
    printf 'CHANGELOG.md 缺少 %s 的段落\n' "$ver" >&2
    rc=1
fi

if [[ -n "$TAG" && "$TAG" != "v$ver" ]]; then
    printf 'tag %s 与 workspace version %s 不符（规则：tag = v<version>）\n' "$TAG" "$ver" >&2
    rc=1
fi

[[ "$rc" -eq 0 ]] && printf '版本号一致性检查通过\n'
exit "$rc"
