#!/usr/bin/env bash
# build-singbox.sh：工具链钉死（--go）、ldflags 取自上游 release/LDFLAGS、失败不留半成品。
# stub git（造最小上游源码树）+ stub go（记 GOTOOLCHAIN 与 argv），零网络零真构建。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# git stub：只认 clone，把最后一个参数当目标目录，造出 go.mod + release/LDFLAGS + cmd/sing-box
cat > "$WORK/bin/git" <<'STUB'
#!/usr/bin/env bash
[[ "$1" == "clone" ]] || exit 128
dest=""; for a in "$@"; do dest="$a"; done
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
export PATH="$WORK/bin:$PATH" GO_LOG="$WORK/go.log"

build() { # 其余参数透传
    : > "$GO_LOG"
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
finish
