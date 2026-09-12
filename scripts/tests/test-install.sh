#!/usr/bin/env bash
# install.sh 引导逻辑单测：全部 stub，零网络、无需 root。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# uname stub：按 FAKE_MACHINE 返回架构
cat > "$WORK/bin/uname" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "-m" ]]; then printf '%s\n' "${FAKE_MACHINE:-x86_64}"; else /usr/bin/uname "$@"; fi
STUB
# curl stub：记录请求顺序；github.com 直连一律失败，镜像前缀成功
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [[ $# -gt 0 ]]; do
  case "$1" in -o) out="$2"; shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
printf '%s\n' "$url" >> "$CURL_LOG"
if [[ "$url" == https://github.com/* && "${FAKE_GITHUB_OK:-0}" != "1" ]]; then exit 22; fi
printf '%s' "${FAKE_BODY:-body}" > "$out"
STUB
chmod +x "$WORK/bin/uname" "$WORK/bin/curl"
export PATH="$WORK/bin:$PATH"
export CURL_LOG="$WORK/curl.log"

BUI_BOOTSTRAP_SOURCED=1
BUI_MIRRORS="https://mirror-a.example/ https://mirror-b.example/"
# shellcheck source=/dev/null
. "$ROOT/install.sh"
# install.sh 顶部是 set -euo pipefail，源入后会污染测试外壳（测试要主动读非零退出码），必须关掉
set +eu

# ---- 架构识别（C4 的 artifacts 键用 amd64 / arm64）----
# 注意：不能写 `FAKE_MACHINE=x assert_eq … "$(detect_arch)"` —— 命令替换在赋值生效之前展开，
# stub 的 uname 会读到旧值；必须单独一行赋值，且 export 给 stub 子进程。
export FAKE_MACHINE
FAKE_MACHINE=x86_64
assert_eq "amd64" "$(detect_arch)" "x86_64 → amd64"
FAKE_MACHINE=amd64
assert_eq "amd64" "$(detect_arch)" "amd64 原样"
FAKE_MACHINE=aarch64
assert_eq "arm64" "$(detect_arch)" "aarch64 → arm64"
FAKE_MACHINE=arm64
assert_eq "arm64" "$(detect_arch)" "arm64 原样"
FAKE_MACHINE=armv7l
out=$(detect_arch 2>&1); rc=$?
assert_eq "1" "$rc" "armv7l 退 1"
assert_contains "不支持的架构" "$out" "armv7l 有中文错误"
FAKE_MACHINE=x86_64

# ---- URL 与下载源顺序 ----
TAG=latest
assert_eq "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" \
    "$(gh_url manifest.json)" "latest 用 releases/latest/download"
TAG=v4.0.1
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/manifest.json" \
    "$(gh_url manifest.json)" "指定 tag 用 releases/download/<tag>"
TAG=latest

: > "$CURL_LOG"
FAKE_BODY="payload" fetch "$(gh_url manifest.json)" "$WORK/m.json"
assert_eq "0" "$?" "直连失败后镜像成功"
assert_eq "payload" "$(cat "$WORK/m.json")" "落地内容来自镜像"
assert_eq "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" \
    "$(sed -n 1p "$CURL_LOG")" "第 1 源是直连"
assert_eq "https://mirror-a.example/https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" \
    "$(sed -n 2p "$CURL_LOG")" "第 2 源是第一个镜像前缀"
assert_eq "2" "$(wc -l < "$CURL_LOG")" "镜像 A 成功后不再试镜像 B（共 2 次请求）"

: > "$CURL_LOG"
# MIRRORS 在源入时就从 BUI_MIRRORS 取值定型了，这里要覆盖的是 MIRRORS 本身
MIRRORS="" fetch "$(gh_url manifest.json)" "$WORK/m.json"
assert_eq "1" "$?" "无镜像且直连失败 → 退 1"

: > "$CURL_LOG"
MIRRORS="" fetch "http://127.0.0.1:8000/v4.0.0/manifest.json" "$WORK/m.json"
assert_eq "0" "$?" "本机托管的 manifest 直连即可（M5 演练路径）"

# ---- manifest 提取（C4 扁平 artifacts，不依赖 jq）----
cat > "$WORK/manifest.json" <<'EOF'
{
  "version": "4.0.0",
  "released": "2026-09-18T07:22:10Z",
  "kernels": {
    "hysteria": "2.12.2",
    "xray": "26.3.27",
    "sing_box": "1.14.5",
    "caddy": "2.11.4",
    "client_sing_box": "1.14.5"
  },
  "artifacts": {
    "bui-linux-amd64": {
      "url": "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/bui-linux-amd64",
      "sha256": "1111111111111111111111111111111111111111111111111111111111111111"
    },
    "bui-linux-arm64": {
      "url": "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/bui-linux-arm64",
      "sha256": "2222222222222222222222222222222222222222222222222222222222222222"
    },
    "bui-c-linux-amd64": {
      "url": "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/bui-c-linux-amd64",
      "sha256": "3333333333333333333333333333333333333333333333333333333333333333"
    },
    "sing-box-linux-amd64": {
      "url": "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/sing-box-linux-amd64",
      "sha256": "4444444444444444444444444444444444444444444444444444444444444444"
    }
  }
}
EOF
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/bui-linux-arm64" \
    "$(manifest_field "$WORK/manifest.json" bui-linux-arm64 url)" "取 bui arm64 的 url"
assert_eq "2222222222222222222222222222222222222222222222222222222222222222" \
    "$(manifest_field "$WORK/manifest.json" bui-linux-arm64 sha256)" "取同一项的 sha256"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/bui-c-linux-amd64" \
    "$(manifest_field "$WORK/manifest.json" bui-c-linux-amd64 url)" "bui-c 可单独取（不被 bui- 误命中）"
assert_eq "1111111111111111111111111111111111111111111111111111111111111111" \
    "$(manifest_field "$WORK/manifest.json" bui-linux-amd64 sha256)" "bui-linux-amd64 不被 bui-c-linux-amd64 抢走"
assert_eq "" "$(manifest_field "$WORK/manifest.json" caddy-linux-arm64 url)" "不存在的键返回空串"

# ---- sha256 校验 ----
printf 'hello' > "$WORK/blob"
good=$(sha256sum "$WORK/blob" | cut -d' ' -f1)
verify_sha256 "$WORK/blob" "$good"
assert_eq "0" "$?" "sha256 一致通过"
out=$(verify_sha256 "$WORK/blob" "0000000000000000000000000000000000000000000000000000000000000000" 2>&1); rc=$?
assert_eq "1" "$rc" "sha256 不一致退 1"
assert_contains "sha256 校验失败" "$out" "不一致有中文错误"

# ---- 安装参数决策 ----
mkdir -p "$WORK/fresh" "$WORK/legacy"
: > "$WORK/legacy/users.json"
assert_eq "" "$(install_args "$WORK/fresh")" "全新机不加参数"
assert_eq "--import-v3" "$(install_args "$WORK/legacy")" "检测到 v3 users.json 自动加 --import-v3"
assert_eq "--import-v3" "$(install_args "$WORK/legacy" --import-v3)" "已显式给出则不重复"
assert_eq "$(printf -- '--non-interactive\n--answers\n/root/bui-answers.json')" \
    "$(install_args "$WORK/fresh" --non-interactive --answers /root/bui-answers.json)" "C5 的非交互参数原样透传"
assert_eq "$(printf -- '--port\n20000')" "$(install_args "$WORK/fresh" --port 20000)" "其它参数原样透传"

# ---- 规模守门：引导脚本保持精简（spec §7 ≈100 行）----
code_lines=$(grep -cvE '^[[:space:]]*(#|$)' "$ROOT/install.sh")
assert_eq "1" "$([[ "$code_lines" -le 130 ]] && echo 1 || echo 0)" "install.sh 有效代码 ≤ 130 行（实测 $code_lines）"
assert_not_contains "apt-get" "$(cat "$ROOT/install.sh")" "引导脚本不装任何系统包（依赖由 bui install 负责）"
assert_contains "BUI_MANIFEST_URL" "$(cat "$ROOT/install.sh")" "支持 BUI_MANIFEST_URL（C5，M5 演练与离线源都用它）"
assert_not_contains "no-import-v3" "$(cat "$ROOT/install.sh")" "不自造 C5 之外的 bui 参数名（C5 只有 --import-v3）"
finish
