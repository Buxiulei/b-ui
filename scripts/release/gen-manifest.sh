#!/usr/bin/env bash
# 由 kernels.lock（内核版本表）+ dist 目录里的 12 个裸二进制合成 manifest.json（stdout）。
# 形状 = 总纲 2026-09-11-v4-master.md 的 C4 契约：顶层 version / kernels / artifacts，
# artifacts 扁平、键名 <name>-linux-<amd64|arm64>、值只有 url 与 sha256，全部指向裸二进制。
# sha256 一律现算 dist 里的真实文件（内核资产是 Actions 解包后重新上传的裸二进制，
# 与 kernels.lock 里上游归档的 sha256 不同，不能照抄 lock）。
set -euo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

VERSION=""
DIST=""
LOCK="$HERE/kernels.lock"
RELEASED=""
BASE_URL=""
REPO="${BUI_REPO:-Buxiulei/b-ui}"

ARTIFACT_NAMES="bui bui-c hysteria xray sing-box caddy"
ARCHES="amd64 arm64"

usage() {
    printf '用法：%s --version <x.y.z> --dist <dir> [--lock <kernels.lock>] [--base-url <prefix>] [--released <ISO8601Z>]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --dist) DIST="${2:-}"; shift 2 ;;
        --lock) LOCK="${2:-}"; shift 2 ;;
        --base-url) BASE_URL="${2:-}"; shift 2 ;;
        --released) RELEASED="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$VERSION" && -n "$DIST" ]] || usage
[[ -f "$LOCK" ]] || { printf '找不到 lock：%s\n' "$LOCK" >&2; exit 2; }
[[ -z "$RELEASED" ]] && RELEASED=$(date -u +%FT%TZ)
[[ -z "$BASE_URL" ]] && BASE_URL="https://github.com/$REPO/releases/download/v$VERSION/"
[[ "$BASE_URL" == */ ]] || BASE_URL="$BASE_URL/"

lock_version() {
    # $1 = lock 里的内核名（sing-box / xray / hysteria / caddy）
    local ver
    ver=$(awk -v k="$1" '$1 == k && $2 == "target" {print $3; exit}' "$LOCK")
    [[ -n "$ver" ]] || { printf 'lock 缺 %s 的 target 行：%s\n' "$1" "$LOCK" >&2; exit 2; }
    printf '%s\n' "$ver"
}

require_artifacts() {
    local name arch path
    for name in $ARTIFACT_NAMES; do
        for arch in $ARCHES; do
            path="$DIST/$name-linux-$arch"
            [[ -f "$path" ]] || { printf '缺少构建产物：%s\n' "$path" >&2; exit 2; }
        done
    done
}

artifacts_json() {
    # 每行「键 url sha256」喂给 jq 折成对象（jq 的 --arg 变量名不允许出现 -，所以不走 --arg）
    local name arch key
    for name in $ARTIFACT_NAMES; do
        for arch in $ARCHES; do
            key="$name-linux-$arch"
            printf '%s %s%s %s\n' "$key" "$BASE_URL" "$key" "$(sha256sum "$DIST/$key" | cut -d' ' -f1)"
        done
    done | jq -Rn '[inputs | split(" ")] | map({key: .[0], value: {url: .[1], sha256: .[2]}}) | from_entries'
}

require_artifacts

SB_VER=$(lock_version sing-box)
jq -n \
    --arg version "$VERSION" \
    --arg released "$RELEASED" \
    --arg changelog "https://github.com/$REPO/releases/tag/v$VERSION" \
    --arg hysteria "$(lock_version hysteria)" \
    --arg xray "$(lock_version xray)" \
    --arg singbox "$SB_VER" \
    --arg caddy "$(lock_version caddy)" \
    --argjson artifacts "$(artifacts_json)" \
    '{
        version: $version,
        released: $released,
        changelog_url: $changelog,
        min_upgrade_from: "4.0.0",
        kernels: {
            hysteria: $hysteria,
            xray: $xray,
            sing_box: $singbox,
            caddy: $caddy,
            client_sing_box: $singbox
        },
        artifacts: $artifacts
    }'
