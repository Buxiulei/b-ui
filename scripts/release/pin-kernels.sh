#!/usr/bin/env bash
# 解析 kernel-versions.env 的版本轨道 → 下载每个内核资产 → 算 sha256 → 写 kernels.lock。
# 四个内核（含 sing-box）都是上游官方发行物，不再有自建行。
#   --write         解析 + 下载 + 覆盖 kernels.lock（会下载约 200MB，几分钟）
#                   整文件重生成，但**保留锁里已有的手写注记行**，只重写自己那两行锁头
#   --check         解析版本号并与锁里的版本列比对，有漂移退出 1（CI 用，不下载）
#   --warn          与 --check 联用：**只把「漂移」降级为 ::warning:: 并退 0**。
#                   lock 少行 / 轨道解析或取远端失败（「核实不了」）**仍旧退 1**——
#                   那两类是锁或环境坏了，不是「上游发了新 patch」，把它们一起吞成告警
#                   就是 fail-open（2026-10-10 安全审查指出过这个洞）。
#   --lock <path>   改写/比对别处的 lock（测试用；默认 scripts/release/kernels.lock）
# sha256 一律「自己下载自己算」：上游 checksums 文件的命名各家不同且会变。
# lock 里的 sha256 是**上游归档**的 sha256（fetch-kernels.sh 校验用）；manifest 里的 sha256 是
# 解包后裸二进制的 sha256（gen-manifest.sh 现算），两者不同，不要互相照抄。
set -euo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$HERE/kernel-versions.env"
LOCK="$HERE/kernels.lock"
WARN=0

gh_tags() {
    # $1 = owner/repo, $2 = ERE；输出匹配 tag，按版本号倒序（最高在第一行）。
    # ls-remote 一次给全部 tag，不受 releases API 分页限制。
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
    local tmp row kernel role ver arch url sha notes
    tmp=$(mktemp)
    # 锁里已有的**手写**注记行（某个内核为什么钉在固定版本、为什么顶了版本之类）要留住：
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
            url=$(asset_url "$kernel" "$ver" "$arch")
            sha=$(remote_sha256 "$url")
            printf '%s %s %s %s %s %s\n' "$kernel" "$role" "$ver" "$arch" "$sha" "$url" >> "$tmp"
            printf '  pinned %s %s %s %s\n' "$kernel" "$ver" "$arch" "${sha:0:12}" >&2
        done
    done
    mv "$tmp" "$LOCK"
    printf '写入 %s\n' "$LOCK" >&2
}

check_lock() {
    local rc=0 hard=0 kernel repo track locked resolved
    for kernel in sing-box:"$SINGBOX_REPO":"$SINGBOX_TRACK" \
                  xray:"$XRAY_REPO":"$XRAY_TRACK" \
                  hysteria:"$HYSTERIA_REPO":"$HYSTERIA_TRACK" \
                  caddy:"$CADDY_REPO":"$CADDY_TRACK"; do
        IFS=: read -r kernel repo track <<< "$kernel"
        locked=$(awk -v k="$kernel" '$1 == k && $2 == "target" {print $3; exit}' "$LOCK")
        # `|| resolved=""`：resolve_track 失败时 set -e 会直接中止脚本，那条清晰的诊断就打不出来；
        # 接住它、归到下面的「核实不了」分支。
        resolved=$(resolve_track "$repo" "$track") || resolved=""
        if [[ -z "$locked" ]]; then
            printf '锁里没有 %s 的行（lock 坏了，不是漂移）——先修 lock\n' "$kernel" >&2
            hard=1; rc=1; continue
        fi
        if [[ -z "$resolved" ]]; then
            printf '无法核实 %s 的轨道（解析或取远端失败）——「核实不了」不等于「一致」\n' "$kernel" >&2
            hard=1; rc=1; continue
        fi
        if [[ "$locked" != "$resolved" ]]; then
            printf '漂移：%s lock=%s 轨道解析=%s（跑 pin-kernels.sh --write）\n' "$kernel" "$locked" "$resolved" >&2
            rc=1
        else
            printf '一致：%s %s\n' "$kernel" "$locked" >&2
        fi
    done
    # --warn 只降级「漂移」；hard（lock 少行 / 核实不了）永远退 1。
    if [[ "$rc" -ne 0 && "$WARN" -eq 1 && "$hard" -eq 0 ]]; then
        printf '::warning::kernels.lock 落后于版本轨道（上游出了新 patch，不是代码问题）：跑 bash scripts/release/pin-kernels.sh --write 顶版，或等周更 bot（kernels-bump.yml，周一 03:00）的 PR。\n'
        return 0
    fi
    return "$rc"
}

main() {
    local mode="--check"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --write | --check) mode="$1"; shift ;;
            --warn) WARN=1; shift ;;
            --lock) LOCK="${2:-}"; shift 2 ;;
            *) printf '用法：%s [--write|--check] [--warn] [--lock <kernels.lock>]\n' "$0" >&2; exit 2 ;;
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
