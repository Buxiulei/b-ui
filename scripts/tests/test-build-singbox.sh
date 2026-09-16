#!/usr/bin/env bash
# build-singbox.sh：工具链钉死（--go）、ldflags 取自上游 release/LDFLAGS、clone 的镜像回退
# 与退避重试、失败不留半成品。
# stub git（造最小上游源码树，可按需装不通）+ stub go（记 GOTOOLCHAIN 与 argv）+ stub sleep，
# 零网络零真构建零等待。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# git stub：只认 clone，把最后一个参数当目标目录，造出 go.mod + release/LDFLAGS + cmd/sing-box。
# 每次 clone 的 URL 记进 $CLONE_LOG；FAKE_CLONE_FAIL=1 一律装不通，
# FAKE_CLONE_NEED_MIRROR=1 只有带镜像前缀（URL 不以 https://github.com/ 开头）才成。
cat > "$WORK/bin/git" <<'STUB'
#!/usr/bin/env bash
[[ "$1" == "clone" ]] || exit 128
url=""; for a in "$@"; do case "$a" in *github.com/*) url="$a" ;; esac; done
dest=""; for a in "$@"; do dest="$a"; done
printf '%s\n' "$url" >> "${CLONE_LOG:-/dev/null}"
[[ "${FAKE_CLONE_FAIL:-0}" != "1" ]] || exit 128
if [[ "${FAKE_CLONE_NEED_MIRROR:-0}" == "1" && "$url" == https://github.com/* ]]; then
  exit 128
fi
mkdir -p "$dest/release" "$dest/cmd/sing-box"
printf 'module github.com/sagernet/sing-box\n\ngo 1.25.5\n' > "$dest/go.mod"
if [[ "${FAKE_NO_LDFLAGS:-0}" != "1" ]]; then
  printf -- '-X runtime.godebugDefault=multipathtcp=0,tlssha1=1 -checklinkname=0\n' > "$dest/release/LDFLAGS"
fi
STUB
chmod 755 "$WORK/bin/git"

# go stub：version 报 FAKE_GO_VER，build 按 -o 落个假二进制；两者都把 GOTOOLCHAIN 与 argv 记下来
cat > "$WORK/bin/go" <<'STUB'
#!/usr/bin/env bash
printf 'GOTOOLCHAIN=%s go %s\n' "${GOTOOLCHAIN-未设}" "$*" >> "$GO_LOG"
case "$1" in
  version) printf 'go version %s linux/amd64\n' "${FAKE_GO_VER:-go1.25.5}" ;;
  build)
    out=""; while [[ $# -gt 0 ]]; do case "$1" in -o) out="$2"; shift 2 ;; *) shift ;; esac; done
    [[ "${FAKE_BUILD_FAIL:-0}" != "1" ]] || exit 1
    printf 'fake-sing-box\n' > "$out" ;;
  *) exit 1 ;;
esac
STUB
chmod 755 "$WORK/bin/go"

# sleep stub：只把退避秒数记下来，测试不真等
cat > "$WORK/bin/sleep" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "${SLEEP_LOG:-/dev/null}"
STUB
chmod 755 "$WORK/bin/sleep"

export PATH="$WORK/bin:$PATH" GO_LOG="$WORK/go.log" \
  CLONE_LOG="$WORK/clone.log" SLEEP_LOG="$WORK/sleep.log"

build() { # 其余参数透传
    : > "$GO_LOG"; : > "$CLONE_LOG"; : > "$SLEEP_LOG"
    bash "$ROOT/scripts/release/build-singbox.sh" --version 1.14.1 --arch amd64 "$@" 2>"$WORK/err"
}

# —— 钉死工具链：--go 给了就 GOTOOLCHAIN=<它>，且回填进 stdout 的 go= ——
out=$(build --out "$WORK/o-pinned" --go go1.25.5); rc=$?
assert_eq "0" "$rc" "--go 与探到的版本一致时退 0"
assert_contains "go=go1.25.5" "$out" "stdout 的 go= 是钉死的那版"
assert_contains "sha256=$(sha256sum "$WORK/o-pinned" | cut -d' ' -f1)" "$out" "stdout 的 sha256 是落地产物的"
assert_eq "2" "$(grep -c '^GOTOOLCHAIN=go1.25.5 ' "$GO_LOG")" "go version 与 go build 都钉在 go1.25.5"
assert_eq "0" "$(grep -c 'GOTOOLCHAIN=auto' "$GO_LOG")" "钉死时不出现 auto"

# —— 不给 --go 才回落 auto（pin 时用来发现上游 go.mod 要求的版本）——
out=$(build --out "$WORK/o-auto"); rc=$?
assert_eq "0" "$rc" "不给 --go 也能构建"
assert_eq "2" "$(grep -c '^GOTOOLCHAIN=auto ' "$GO_LOG")" "不给 --go 时 GOTOOLCHAIN=auto"

# —— 工具链断言：探到的版本与 --go 不符即退非 0 且不产出 --out ——
out=$(FAKE_GO_VER=go1.26.8 build --out "$WORK/o-mismatch" --go go1.25.5); rc=$?
assert_eq "1" "$rc" "工具链版本与 --go 不符退 1"
assert_contains "工具链不符" "$(cat "$WORK/err")" "报出要求版本与实际版本"
assert_eq "0" "$([[ -e "$WORK/o-mismatch" ]] && echo 1 || echo 0)" "工具链不符时不产出 --out"
assert_eq "0" "$(grep -c ' go build ' "$GO_LOG")" "断言失败就不再 go build"

# —— ldflags 取自上游 release/LDFLAGS，只追加 constant.Version ——
build --out "$WORK/o-ldflags" --go go1.25.5 >/dev/null
ldline=$(grep ' go build ' "$GO_LOG")
assert_contains "-X runtime.godebugDefault=multipathtcp=0,tlssha1=1" "$ldline" \
  "带上上游 release/LDFLAGS 里的 godebug 默认值（抄一遍必漏，Tags 断言查不出这一项）"
assert_contains "-checklinkname=0" "$ldline" "release/LDFLAGS 带来的 -checklinkname=0"
assert_contains "-X github.com/sagernet/sing-box/constant.Version=1.14.1" "$ldline" "版本号是我们追加的那一项"
assert_contains "-s -w -buildid=" "$ldline" "照上游 Makefile 收尾 -s -w -buildid="
assert_contains "-trimpath" "$ldline" "-trimpath（可重现构建）"

# —— 上游没有 release/LDFLAGS 就报错，不许悄悄少几个 ldflags ——
out=$(FAKE_NO_LDFLAGS=1 build --out "$WORK/o-nold" --go go1.25.5); rc=$?
assert_eq "1" "$rc" "缺 release/LDFLAGS 退 1"
assert_contains "release/LDFLAGS" "$(cat "$WORK/err")" "点名缺的是哪个文件"
assert_eq "0" "$([[ -e "$WORK/o-nold" ]] && echo 1 || echo 0)" "缺 LDFLAGS 时不产出 --out"

# —— go build 失败也不留半成品 ——
out=$(FAKE_BUILD_FAIL=1 build --out "$WORK/o-failed" --go go1.25.5); rc=$?
assert_eq "1" "$([[ "$rc" != 0 ]] && echo 1 || echo 0)" "go build 失败退非 0"
assert_eq "0" "$([[ -e "$WORK/o-failed" ]] && echo 1 || echo 0)" "构建失败时不产出 --out"

# —— 参数校验：--go 形如 go1.x.y ——
build --out "$WORK/o-badgo" --go 1.25.5 >/dev/null; rc=$?
assert_eq "2" "$rc" "--go 格式非法退 2"

# —— clone 默认直连（BUI_MIRRORS 未设时第一次就打 github.com）——
build --out "$WORK/o-direct" --go go1.25.5 >/dev/null
assert_eq "https://github.com/SagerNet/sing-box" "$(head -1 "$WORK/clone.log")" \
  "第一次 clone 是直连（不带任何前缀）"
assert_eq "1" "$(wc -l < "$WORK/clone.log")" "直连成功就不再试镜像"

# —— 直连不通、镜像可达：仍能构建（发布链上多出来的这一段网络依赖不许把发版一次打死）——
out=$(FAKE_CLONE_NEED_MIRROR=1 BUI_MIRRORS="https://mirror.example.net/" \
  build --out "$WORK/o-mirror" --go go1.25.5); rc=$?
assert_eq "0" "$rc" "直连不通、镜像可达时仍构建成功"
assert_eq "https://mirror.example.net/https://github.com/SagerNet/sing-box" \
  "$(tail -1 "$WORK/clone.log")" "镜像前缀拼在完整 GitHub URL 前（同 fetch-kernels.sh 的 download()）"
assert_eq "0" "$(wc -l < "$WORK/sleep.log")" "第一轮里换镜像就成了，不退避"

# —— 直连与全部镜像都不通：3 轮各试遍全部来源、指数退避，退非 0 且不产出 --out ——
out=$(FAKE_CLONE_FAIL=1 BUI_MIRRORS="https://mirror.example.net/ https://mirror2.example.net/" \
  build --out "$WORK/o-noclone" --go go1.25.5); rc=$?
assert_eq "1" "$([[ "$rc" != 0 ]] && echo 1 || echo 0)" "clone 全败退非 0（锁不动、发布保持上一版）"
assert_contains "clone 失败（GitHub 与全部镜像均不可达）" "$(cat "$WORK/err")" "报出直连与镜像都不可达"
assert_eq "0" "$([[ -e "$WORK/o-noclone" ]] && echo 1 || echo 0)" "clone 失败时不产出 --out"
assert_eq "9" "$(wc -l < "$WORK/clone.log")" "3 轮 × （直连 + 2 个镜像）"
assert_eq "5 15" "$(tr '\n' ' ' < "$WORK/sleep.log" | sed 's/ $//')" "轮间指数退避 5s → 15s"
assert_eq "0" "$(grep -c ' go build ' "$GO_LOG")" "clone 不成就不进构建"

# —— BUI_MIRRORS="" 表示只用直连（离网环境的语义，同 fetch-kernels.sh）——
out=$(FAKE_CLONE_FAIL=1 BUI_MIRRORS="" build --out "$WORK/o-nomirror" --go go1.25.5)
assert_eq "3" "$(wc -l < "$WORK/clone.log")" "BUI_MIRRORS 为空时每轮只试直连"
finish
