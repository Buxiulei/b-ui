#!/usr/bin/env bash
# 原子性门禁：**内核来源**与**渲染器是否产出 `experimental.v2ray_api`** 必须自洽。
# 用法：check-kernel-consistency.sh [--warn] [--repo <dir>]
# 退出码：0 通过 / --warn 下不过；2 用法错误；3 门禁不过。
#
# 为什么需要它（2026-10-10 新增）：
#   官方 sing-box 发行归档**不含** `with_v2ray_api`，对含该段的配置直接
#   `FATAL create v2ray-server: v2ray api is not included in this build`——
#   住宅内核**起不来**，不是降级。反过来，自建那份的唯一动机就是让这个段可用。
#   所以「锁说官方」与「渲染器还吐这一段」是**绝对不能同时成立**的组合。
#
#   原先兜住这件事的是 `.github/workflows/*.yml` 里一句
#   `grep -Eq 'skipped: (…|自建 sing-box（with_v2ray_api）不可用)'`——它把
#   `kernel_hy2_singbox` 的静默 skip 当失败。切官方时那句被删了，而该用例的 skip 门还在，
#   于是**这种不一致会让 CI 跑成绿的**：住宅用例静默跳过，其余用例的配置对官方内核都安全。
#   绿 CI 恰恰会让人放心地发版。本脚本把这条不一致变成**发布路径上的硬闸门**。
#
# 落点（刻意分两处，别加进 run-all.sh）：
#   - `ci.yml` 的 lint job：`--warn`，只告警不卡 CI——迁移期本就不一致，
#     让 CI 长期常红会训练人忽略红色（同 `check-release-gate.sh --warn` 的口径）。
#   - `release.yml` 的 verify job：**硬闸门**，判红即中止发布。
# 迁移完成（渲染器摘段 + `kernel_hy2_singbox` 拆 skip 门）后，本脚本自然转绿，
# 并退化为与那条真跑用例重复的守卫——那时可以删掉它，或保留作快速自检。
#
# 全程只读本地文件：不联网、不需要内核、不跑 cargo。纯文本与 JSON 无关的源码模式判定。
set -uo pipefail
LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WARN=0
REPO="$ROOT"

usage() {
    printf '用法：%s [--warn] [--repo <dir>]\n' "$0" >&2
    exit 2
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --warn) WARN=1; shift ;;
        --repo) [[ "$#" -ge 2 ]] || usage; REPO="$2"; shift 2 ;;
        -*) printf '未知选项：%s\n' "$1" >&2; usage ;;
        *) usage ;;
    esac
done

# 退 3 的唯一出口：--warn 下改成 GitHub 注解走 stdout 并退 0，文案一字不改，方便 grep。
deny() {
    if [[ "$WARN" -eq 1 ]]; then
        printf '::warning::%s\n' "$1"
        exit 0
    fi
    printf '%s\n' "$1" >&2
    exit 3
}

LOCK="$REPO/scripts/release/kernels.lock"
RENDERER="$REPO/crates/bui-schema/src/render/hy2_singbox.rs"
[[ -f "$LOCK" ]] || { printf '内核锁读不到：%s\n' "$LOCK" >&2; exit 2; }
[[ -f "$RENDERER" ]] || { printf '渲染器读不到：%s\n' "$RENDERER" >&2; exit 2; }

# ① 锁里 sing-box 那两行的来源：官方归档 URL，还是 `build:` 自建 URI。
official=0
built=0
rows=0
unknown=""
while read -r kernel _role _version _arch _sha url _rest; do
    [[ "$kernel" == "sing-box" ]] || continue
    rows=$((rows + 1))
    case "$url" in
        https://github.com/*) official=$((official + 1)) ;;
        build:*) built=$((built + 1)) ;;
        *) unknown="$url" ;;
    esac
done < "$LOCK"

[[ "$rows" -gt 0 ]] || { printf '内核锁里没有 sing-box 行：%s\n' "$LOCK" >&2; exit 2; }
[[ -z "$unknown" ]] || deny "sing-box 行的 URL 列既不是官方归档也不是 build:（$unknown）——锁格式异常，先修锁"
if [[ "$official" -gt 0 && "$built" -gt 0 ]]; then
    deny "内核锁里 sing-box 两行来源不一致（官方 $official 行 / 自建 $built 行）——同一发布只能有一种来源"
fi

# ② 渲染器是否**产出**该段。只认顶层 JSON 键的发射形态 `"v2ray_api": {`；
#    测试里的 `v["experimental"]["v2ray_api"]` 下标断言与注释提到都不会命中。
emits=0
grep -qE '^[[:space:]]*"v2ray_api"[[:space:]]*:' "$RENDERER" && emits=1

# ③ 自洽判定
if [[ "$official" -gt 0 && "$emits" -eq 1 ]]; then
    deny "内核锁里 sing-box 是官方归档（不含 with_v2ray_api），但渲染器仍产出 experimental.v2ray_api："\
"住宅内核会 FATAL（create v2ray-server: v2ray api is not included in this build）起不来。"\
"要么摘掉渲染器该段并接上替代计量，要么把内核退回自建——两者不能只有一半。"
fi
if [[ "$built" -gt 0 && "$emits" -eq 0 ]]; then
    deny "内核锁里 sing-box 是自建（唯一动机是 with_v2ray_api 计量），但渲染器不再产出该段："\
"住宅按用户计量会失去来源（自建那份的价值只在这一段）。"
fi

printf '内核来源与渲染器自洽：%s，v2ray_api 段%s产出\n' \
    "$([[ "$official" -gt 0 ]] && printf '官方归档' || printf '自建 build:')" \
    "$([[ "$emits" -eq 1 ]] && printf '有' || printf '无')"
exit 0
