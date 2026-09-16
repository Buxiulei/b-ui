#!/usr/bin/env bash
# 解析 kernel-versions.env 的版本轨道 → 下载（sing-box 是自建）每个内核资产 → 算 sha256 → 写 kernels.lock。
#   --write         解析 + 下载 / 构建 + 覆盖 kernels.lock（会下载约 200MB 并构建两次 sing-box，十几分钟）
#                   整文件重生成，但**保留锁里已有的手写注记行**，只重写自己那两行锁头
#   --check         解析版本号 + 比自建行的 tags= 与 env 是否一致，有漂移退出 1（CI 用，不下载、不构建）
#   --lock <path>   改写/比对别处的 lock（测试用；默认 scripts/release/kernels.lock）
# sha256 一律「自己下载自己算」：上游 checksums 文件的命名各家不同且会变。
# lock 里的 sha256 是**上游归档**的 sha256（fetch-kernels.sh 校验用）；manifest 里的 sha256 是
# 解包后裸二进制的 sha256（gen-manifest.sh 现算），两者不同，不要互相照抄。
# 例外：`sing-box target` 两行是**自建**（唯一动机 with_v2ray_api，spec §5.3），URL 列是
# `build:<repo>@v<ver>;go=<go>;tags=<tags>`，sha256 列就是构建出来的裸二进制的 sha256。
# `sing-box check` 的 1.12 / 1.13 不自建，仍是上游归档。
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

build_singbox_row() {
    # 随发布分发的 sing-box 自建（为了 with_v2ray_api，spec §5.3）：
    # $1 = 版本, $2 = amd64|arm64；stdout = "<构建产物 sha256> build:<repo>@v<ver>;go=<go>;tags=<tags>"
    # 任一步失败即退非 0 —— write_lock 在 set -e 下当场中止，锁不动（spec §5.4 第 8 条）。
    # 这里**故意不传 --go**：GOTOOLCHAIN=auto 探出上游 go.mod 要求的版本写进锁的 go=，
    # 取件时再由 fetch-kernels.sh 透传回去钉死（钉的人和发现的人不是同一步）。
    local ver="$1" arch="$2" builder out raw line go tags sha
    builder="${BUI_SINGBOX_BUILDER:-$HERE/build-singbox.sh}"
    out=$(mktemp)
    if ! raw=$("$builder" --repo "$SINGBOX_REPO" --version "$ver" --arch "$arch" --out "$out"); then
        rm -f "$out"
        printf '自建 sing-box 失败：%s %s\n' "$ver" "$arch" >&2
        return 1
    fi
    rm -f "$out"
    line=$(printf '%s\n' "$raw" | tail -1)
    go=$(printf '%s\n' "$line" | sed -nE 's/.*(^| )go=([^ ]+).*/\2/p')
    tags=$(printf '%s\n' "$line" | sed -nE 's/.*(^| )tags=([^ ]+).*/\2/p')
    sha=$(printf '%s\n' "$line" | sed -nE 's/.*(^| )sha256=([0-9a-f]{64}).*/\2/p')
    if [[ -z "$go" || -z "$tags" || -z "$sha" ]]; then
        printf '构建器输出不合规（要 go= tags= sha256=）：%s\n' "$line" >&2
        return 1
    fi
    printf '%s build:%s@v%s;go=%s;tags=%s\n' "$sha" "$SINGBOX_REPO" "$ver" "$go" "$tags"
}

write_lock() {
    local tmp row kernel role ver arch url sha minor built notes
    tmp=$(mktemp)
    # 锁里已有的**手写**注记行（哪两行是回填的、某个内核为什么顶了版本之类）要留住：
    # --write 是整文件重生成，不留的话每次重写都静默删掉它们，而周更 bot 的 PR 正文
    # 又只列内核行 ⇒ 评审者看不见注记被删。本函数只重写自己那两行锁头（生成时间 + 列名）。
    notes=""
    if [[ -f "$LOCK" ]]; then
        notes=$(grep '^#' "$LOCK" \
            | grep -v '^# 由 scripts/release/pin-kernels\.sh --write 生成' \
            | grep -v '^# kernel role version arch sha256 url' || true)
    fi
    {
        printf '# 由 scripts/release/pin-kernels.sh --write 生成，勿手工编辑（生成时间 %s）\n' "$(date -u +%FT%TZ)"
        if [[ -n "$notes" ]]; then
            printf '%s\n' "$notes"
        fi
        printf '# kernel role version arch sha256 url\n'
    } > "$tmp"
    for row in "sing-box target $(resolve_track "$SINGBOX_REPO" "$SINGBOX_TRACK")" \
               "xray target $(resolve_track "$XRAY_REPO" "$XRAY_TRACK")" \
               "hysteria target $(resolve_track "$HYSTERIA_REPO" "$HYSTERIA_TRACK")" \
               "caddy target $(resolve_track "$CADDY_REPO" "$CADDY_TRACK")"; do
        read -r kernel role ver <<< "$row"
        for arch in amd64 arm64; do
            if [[ "$kernel" == "sing-box" ]]; then
                built=$(build_singbox_row "$ver" "$arch")
                read -r sha url <<< "$built"
            else
                url=$(asset_url "$kernel" "$ver" "$arch")
                sha=$(remote_sha256 "$url")
            fi
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
    local rc=0 kernel repo track locked resolved minor ltags
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
    # 自建那两行的 tags= 也要盯：只比版本号的话，改了 env 的 SINGBOX_TAGS 而忘了 --write
    # 是**静默 no-op**（取件读的是锁里的 tags=，CI 全绿），标签集就此与 env 脱钩。
    # sort -u：两行不一致或哪行缺 ;tags= 都会与 env 不等，一并算漂移。
    ltags=$(awk '$1 == "sing-box" && $2 == "target" {sub(/.*;tags=/, "", $6); print $6}' "$LOCK" | sort -u)
    if [[ "$ltags" != "$SINGBOX_TAGS" ]]; then
        printf '漂移：sing-box tags lock=%s env=%s（跑 pin-kernels.sh --write）\n' "${ltags:-缺失}" "$SINGBOX_TAGS" >&2
        rc=1
    else
        printf '一致：sing-box tags %s\n' "$ltags" >&2
    fi
    return "$rc"
}

main() {
    local mode="--check"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --write | --check) mode="$1"; shift ;;
            --lock) LOCK="${2:-}"; shift 2 ;;
            *) printf '用法：%s [--write|--check] [--lock <kernels.lock>]\n' "$0" >&2; exit 2 ;;
        esac
    done
    [[ -n "$LOCK" ]] || { printf '--lock 不能为空\n' >&2; exit 2; }
    case "$mode" in
        --write) write_lock ;;
        --check) check_lock ;;
    esac
}

if [[ "${BUI_PIN_SOURCED:-0}" != "1" ]]; then
    main "$@"
fi
