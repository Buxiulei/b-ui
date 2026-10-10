#!/usr/bin/env bash
# check-vendored-proto.sh：假 repo + stub curl（零网络）覆盖 通过 / 本地漂移 / 上游漂移 /
# UPSTREAM 落后 / 404 / 取不到（退 2）/ --warn 的降级面。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/upstream/daemon"
export CURL_LOG="$WORK/curl.log"
export RAW_BASE="https://raw.test"
export UPSTREAM_DIR="$WORK/upstream"
: > "$CURL_LOG"

# curl stub：零网络。状态码探测（`-o /dev/null -w`）与下载（`-fsSL -o <file>`）两种形状，
# 内容从 $UPSTREAM_DIR/<rel> 取（rel = URL 里 tag 之后的部分）；$FAKE_CODE 覆盖状态码，
# $FAKE_CURL_FAIL=1 让 curl 本身失败（连不上）。
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
# 只实现门禁用到的两种形状：状态码探测（不带 -f）与下载（`-fsSL`，带 -f）。
out=""; url=""; fail=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -f*) fail=1; shift ;;
    "$RAW_BASE"/*) url="$1"; shift ;;
    *) shift ;;
  esac
done
printf '%s\n' "$url" > "$CURL_LOG"
[[ "${FAKE_CURL_FAIL:-0}" == "1" ]] && { printf 'stub curl: 连不上\n' >&2; exit 7; }
rest="${url#"$RAW_BASE"/}"              # SagerNet/sing-box/v1.14.3/daemon/x.proto
rest="${rest#*/}"; rest="${rest#*/}"    # v1.14.3/daemon/x.proto（仓库是两段）
rest="${rest#*/}"                       # daemon/x.proto（tag）
code="${FAKE_CODE:-}"
if [[ -n "$code" ]]; then :; elif [[ -f "$UPSTREAM_DIR/$rest" ]]; then code=200; else code=404; fi
if [[ -n "$out" && "$out" != "/dev/null" ]]; then
  [[ "$code" == "200" ]] || exit 22
  cp "$UPSTREAM_DIR/$rest" "$out"
fi
printf '%s' "$code"
[[ "$fail" == "0" || "$code" == "200" ]]
STUB
chmod +x "$WORK/bin/curl"
export PATH="$WORK/bin:$PATH"

REPO_DIR="$WORK/repo"
# 假 repo：lock + vendored 树（proto 内容字节无关紧要，只要三方自洽）
make_repo() {
    local dir="$1"
    rm -rf "$dir"
    mkdir -p "$dir/scripts/release" "$dir/crates/bui/proto-singbox/daemon"
    cat > "$dir/scripts/release/kernels.lock" <<'LOCK'
# 假锁
sing-box target 1.14.3 amd64 e2bdf179c15a3652955dc44867e33ca0965c9c9b91b34267dcc5a5d639a5feee https://github.com/SagerNet/sing-box/releases/download/v1.14.3/sing-box-1.14.3-linux-amd64.tar.gz
sing-box target 1.14.3 arm64 29dae24d76bea3bc62d07dd3e60cfc12003de7debbd6b768df9be9c68360174e https://github.com/SagerNet/sing-box/releases/download/v1.14.3/sing-box-1.14.3-linux-arm64.tar.gz
LOCK
    printf 'syntax = "proto3";\npackage daemon;\n' \
        > "$dir/crates/bui/proto-singbox/daemon/started_service.proto"
    printf 'repo=SagerNet/sing-box\ntag=v1.14.3\npath=daemon/started_service.proto\n' \
        > "$dir/crates/bui/proto-singbox/UPSTREAM"
    ( cd "$dir/crates/bui/proto-singbox" && sha256sum daemon/started_service.proto > SHA256SUMS )
    cp "$dir/crates/bui/proto-singbox/daemon/started_service.proto" \
        "$UPSTREAM_DIR/daemon/started_service.proto"
}

run() { # 跑门禁，把退出码回显到 stdout（输出落在 $WORK/out 与 $WORK/err）
    local rc
    bash "$ROOT/scripts/release/check-vendored-proto.sh" "$@" > "$WORK/out" 2> "$WORK/err"
    rc=$?
    printf '%s' "$rc"
}

make_repo "$REPO_DIR"

# ① 三方自洽
assert_eq "0" "$(run --repo "$REPO_DIR")" "自洽时退 0"
assert_contains "逐字节一致" "$(cat "$WORK/out")" "自洽时的口吻"
assert_contains "sing-box 1.14.3（v1.14.3）" "$(cat "$WORK/out")" "报出锁里的版本与 tag"

# ② 本地漂移：文件改了，SHA256SUMS 与上游都还是旧的
printf '\n// drift\n' >> "$REPO_DIR/crates/bui/proto-singbox/daemon/started_service.proto"
assert_eq "3" "$(run --repo "$REPO_DIR")" "本地与记录不符 ⇒ 漂移退 3"
assert_contains "内容与 SHA256SUMS 记录不符" "$(cat "$WORK/err")" "指名到「本地 vs 记录」"
assert_contains "daemon/started_service.proto" "$(cat "$WORK/err")" "报错直指文件"
assert_eq "0" "$(run --repo "$REPO_DIR" --warn)" "--warn 下漂移不卡"
assert_contains "::warning::" "$(cat "$WORK/out")" "--warn 下走 GitHub 注解"

# ③ 上游漂移：本地与 SHA256SUMS 自洽，上游那份改了
make_repo "$REPO_DIR"
printf '\n// upstream drift\n' >> "$UPSTREAM_DIR/daemon/started_service.proto"
assert_eq "3" "$(run --repo "$REPO_DIR")" "上游与 vendored 不一致 ⇒ 漂移退 3"
assert_contains "与上游 v1.14.3 的原文不一致" "$(cat "$WORK/err")" "指名到「vendored vs 上游」"
assert_contains "不得只改 sha" "$(cat "$WORK/err")" "给出可执行的补救"

# ④ UPSTREAM 落后于锁（内核升了 patch、proto 没重核）
make_repo "$REPO_DIR"
sed -i 's/^tag=v1.14.3$/tag=v1.14.2/' "$REPO_DIR/crates/bui/proto-singbox/UPSTREAM"
assert_eq "3" "$(run --repo "$REPO_DIR")" "UPSTREAM 的 tag 与锁不一致 ⇒ 漂移退 3"
assert_contains "内核升了版，vendored proto 没跟它核过" "$(cat "$WORK/err")" \
    "说清是「没核过」不是「内容错了」"

# ⑤ 上游 404
make_repo "$REPO_DIR"
rm -f "$UPSTREAM_DIR/daemon/started_service.proto"
assert_eq "3" "$(run --repo "$REPO_DIR")" "上游 404 ⇒ 漂移退 3（不是退 2）"
assert_contains "HTTP 404" "$(cat "$WORK/err")" "把 404 与「取不到」分开"

# ⑥ 取不到：curl 失败与 5xx 都是「核实不了」⇒ 退 2，--warn 也吞不掉
make_repo "$REPO_DIR"
assert_eq "2" "$(FAKE_CURL_FAIL=1 run --repo "$REPO_DIR")" "curl 失败 ⇒ 退 2"
assert_contains "取上游失败（核实不了）" "$(cat "$WORK/err")" "退 2 的口吻是「核实不了」"
assert_eq "2" "$(FAKE_CURL_FAIL=1 run --repo "$REPO_DIR" --warn)" "--warn 不吞「核实不了」"
assert_eq "2" "$(FAKE_CODE=503 run --repo "$REPO_DIR")" "上游 5xx ⇒ 退 2"

# ⑦ 锁坏了 / 缺件 / 用法错误
make_repo "$REPO_DIR"
printf '# 空锁\n' > "$REPO_DIR/scripts/release/kernels.lock"
assert_eq "2" "$(run --repo "$REPO_DIR")" "锁里没有 sing-box target 行 ⇒ 退 2"
make_repo "$REPO_DIR"
rm -f "$REPO_DIR/crates/bui/proto-singbox/UPSTREAM"
assert_eq "2" "$(run --repo "$REPO_DIR")" "缺 UPSTREAM ⇒ 退 2（不是漂移）"
assert_eq "2" "$(run --repo "$REPO_DIR" --bogus)" "未知选项 ⇒ 退 2"

# ⑧ 真仓库自己离线那一半必须自洽
real="$ROOT/crates/bui/proto-singbox"
if ( cd "$real" && sha256sum -c --status SHA256SUMS ); then
    ok "本仓 proto-singbox 与 SHA256SUMS 自洽"
else
    fail "本仓 proto-singbox 与 SHA256SUMS 自洽"
fi
assert_eq "repo=SagerNet/sing-box" "$(awk -F= '$1=="repo"{print}' "$real/UPSTREAM")" \
    "本仓 UPSTREAM 记的是 sing-box 官方仓库"

finish
