#!/usr/bin/env bash
# 解析 kernel-versions.env 的版本轨道 → 下载每个内核资产 → 算 sha256 → 写 kernels.lock。
#   --write   解析 + 下载 + 覆盖 kernels.lock（会下载约 200MB，几分钟）
#   --check   只解析版本号并与 lock 比对，有漂移退出 1（CI 用，不下载）
# sha256 一律「自己下载自己算」：上游 checksums 文件的命名各家不同且会变。
# lock 里的 sha256 是**上游归档**的 sha256（fetch-kernels.sh 校验用）；manifest 里的 sha256 是
# 解包后裸二进制的 sha256（gen-manifest.sh 现算），两者不同，不要互相照抄。
set -euo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$HERE/kernel-versions.env"
LOCK="$HERE/kernels.lock"

gh_tags() {
    # $1 = owner/repo, $2 = ERE；输出匹配 tag，按版本号倒序（最高在第一行）。
    # 用 git ls-remote 而不是 GitHub API：API 的 /releases 一次最多 100 条且要分页
    # （2026-09-12 实测 sing-box 的 v1.12.25 排在第 89 位，再来十几个 alpha/patch 发布
    #  minor:1.12 就解析不到，--write 会退 1、CI 的 1.12 矩阵失去数据源），
    # 而 ls-remote 一次给全部 tag、不限速、不需要 token。
    git ls-remote --tags --refs "https://github.com/$1" 2>/dev/null \
        | awk '{print $2}' \
        | sed -E 's#^refs/tags/##' \
        | grep -E "$2" \
        | sort -V -r || true
}

resolve_track() {
    # $1 = owner/repo, $2 = 轨道；stdout = 纯版本号（无 v / app/v 前缀）
    local repo="$1" track="$2" body re tag
    case "$track" in
        pin:*)
            printf '%s\n' "${track#pin:v}"
            return 0
            ;;
        minor:*)
            body="${track#minor:}"
            re="^v${body//./\\.}\.[0-9]+$"
            ;;
        major:*)
            body="${track#major:}"
            re="^v${body}\.[0-9]+\.[0-9]+$"
            ;;
        appmajor:*)
            body="${track#appmajor:}"
            re="^app/v${body}\.[0-9]+\.[0-9]+$"
            ;;
        *)
            printf '未知轨道 %s\n' "$track" >&2
            return 1
            ;;
    esac
    tag=$(gh_tags "$repo" "$re" | head -1)
    if [[ -z "$tag" ]]; then
        printf '无法解析 %s 的轨道 %s\n' "$repo" "$track" >&2
        return 1
    fi
    tag="${tag#app/}"
    printf '%s\n' "${tag#v}"
}

asset_url() {
    # $1 = 内核名, $2 = 版本, $3 = amd64|arm64（上游资产命名，与 manifest 的 artifacts 键同口径）
    case "$1" in
        sing-box) printf 'https://github.com/SagerNet/sing-box/releases/download/v%s/sing-box-%s-linux-%s.tar.gz\n' "$2" "$2" "$3" ;;
        xray)
            if [[ "$3" == "amd64" ]]; then
                printf 'https://github.com/XTLS/Xray-core/releases/download/v%s/Xray-linux-64.zip\n' "$2"
            else
                printf 'https://github.com/XTLS/Xray-core/releases/download/v%s/Xray-linux-arm64-v8a.zip\n' "$2"
            fi
            ;;
        hysteria) printf 'https://github.com/apernet/hysteria/releases/download/app/v%s/hysteria-linux-%s\n' "$2" "$3" ;;
        caddy)    printf 'https://github.com/caddyserver/caddy/releases/download/v%s/caddy_%s_linux_%s.tar.gz\n' "$2" "$2" "$3" ;;
        *)
            printf '未知内核 %s\n' "$1" >&2
            return 1
            ;;
    esac
}

remote_sha256() {
    # 流式下载并算 sha256，不落盘
    curl -fsSL --connect-timeout 15 --max-time 600 "$1" | sha256sum | cut -d' ' -f1
}

write_lock() {
    local tmp row kernel role ver arch url sha minor
    tmp=$(mktemp)
    {
        printf '# 由 scripts/release/pin-kernels.sh --write 生成，勿手工编辑（生成时间 %s）\n' "$(date -u +%FT%TZ)"
        printf '# kernel role version arch sha256 url\n'
    } > "$tmp"
    for row in "sing-box target $(resolve_track "$SINGBOX_REPO" "$SINGBOX_TRACK")" \
               "xray target $(resolve_track "$XRAY_REPO" "$XRAY_TRACK")" \
               "hysteria target $(resolve_track "$HYSTERIA_REPO" "$HYSTERIA_TRACK")" \
               "caddy target $(resolve_track "$CADDY_REPO" "$CADDY_TRACK")"; do
        read -r kernel role ver <<< "$row"
        for arch in amd64 arm64; do
            url=$(asset_url "$kernel" "$ver" "$arch")
            sha=$(remote_sha256 "$url")
            printf '%s %s %s %s %s %s\n' "$kernel" "$role" "$ver" "$arch" "$sha" "$url" >> "$tmp"
            printf '  pinned %s %s %s %s\n' "$kernel" "$ver" "$arch" "${sha:0:12}" >&2
        done
    done
    for minor in $SINGBOX_CHECK_MINORS; do
        ver=$(resolve_track "$SINGBOX_REPO" "minor:$minor")
        url=$(asset_url sing-box "$ver" amd64)
        sha=$(remote_sha256 "$url")
        printf 'sing-box check %s amd64 %s %s\n' "$ver" "$sha" "$url" >> "$tmp"
        printf '  pinned sing-box(check) %s amd64 %s\n' "$ver" "${sha:0:12}" >&2
    done
    mv "$tmp" "$LOCK"
    printf '写入 %s\n' "$LOCK" >&2
}

check_lock() {
    local rc=0 kernel repo track locked resolved minor
    for kernel in sing-box:"$SINGBOX_REPO":"$SINGBOX_TRACK" \
                  xray:"$XRAY_REPO":"$XRAY_TRACK" \
                  hysteria:"$HYSTERIA_REPO":"$HYSTERIA_TRACK" \
                  caddy:"$CADDY_REPO":"$CADDY_TRACK"; do
        IFS=: read -r kernel repo track <<< "$kernel"
        locked=$(awk -v k="$kernel" '$1 == k && $2 == "target" {print $3; exit}' "$LOCK")
        resolved=$(resolve_track "$repo" "$track")
        if [[ "$locked" != "$resolved" ]]; then
            printf '漂移：%s lock=%s 轨道解析=%s（跑 pin-kernels.sh --write）\n' "$kernel" "${locked:-缺失}" "$resolved" >&2
            rc=1
        else
            printf '一致：%s %s\n' "$kernel" "$locked" >&2
        fi
    done
    # check minor 也要盯：只比 target 的话 CI 的 1.12 / 1.13 矩阵会在上游出新 patch 后失去数据源
    for minor in $SINGBOX_CHECK_MINORS; do
        locked=$(awk -v p="$minor." '$1 == "sing-box" && $2 == "check" && index($3, p) == 1 {print $3; exit}' "$LOCK")
        resolved=$(resolve_track "$SINGBOX_REPO" "minor:$minor")
        if [[ "$locked" != "$resolved" ]]; then
            printf '漂移：sing-box(check %s) lock=%s 轨道解析=%s（跑 pin-kernels.sh --write）\n' "$minor" "${locked:-缺失}" "$resolved" >&2
            rc=1
        else
            printf '一致：sing-box(check %s) %s\n' "$minor" "$locked" >&2
        fi
    done
    return "$rc"
}

main() {
    case "${1:---check}" in
        --write) write_lock ;;
        --check) check_lock ;;
        *) printf '用法：%s [--write|--check]\n' "$0" >&2; exit 2 ;;
    esac
}

if [[ "${BUI_PIN_SOURCED:-0}" != "1" ]]; then
    main "$@"
fi
