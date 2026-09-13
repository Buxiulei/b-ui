#!/usr/bin/env bash
# scripts/bui-c-install.sh 的本地测试：起一个 127.0.0.1 的 http.server 当面板的
# /packages/ 目录，装到临时 PREFIX。不碰真实系统、不访问外网。
#
# **每个用例都显式把 BUI_C_GITHUB / BUI_C_RELEASES_API 指到 127.0.0.1**：面板源取不到
# manifest 时脚本会回退 GitHub，漏设一处这里就真的出网了（CLAUDE.md：scripts/tests 禁止访问网络）。
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
srv=""
cleanup() { [ -n "$srv" ] && kill "$srv" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

base="http://127.0.0.1:18099"
# GitHub 的两条来源：默认指向本地不存在的路径（= 404），用例 6 再按需覆盖
gh404="$base/nope-github"
api404="$base/nope-github/releases.json"

# 权限读取：GNU stat 是 -c，BSD/macOS stat 是 -f（CI 跑 Linux，开发机常是 macOS）
perm() { stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"; }

pkg="$work/packages"
mkdir -p "$pkg" "$work/bin"
printf 'fake-bui-c-binary\n' > "$pkg/bui-c-linux-amd64"
cp "$pkg/bui-c-linux-amd64" "$pkg/bui-c-linux-arm64"
sha="$(sha256sum "$pkg/bui-c-linux-amd64" | cut -d' ' -f1)"
dl="https://github.com/Buxiulei/b-ui/releases/download/v4.0.0"
# 总纲 C4 形状：artifact 是裸二进制的 {url, sha256}（url 在前），外加客户端不看的键
write_manifest() {
  # $1 = 落地路径，$2 = 四个 artifact 共用的 sha256
  cat > "$1" <<JSON
{"version":"4.0.0","released":"2026-09-12T00:00:00Z",
 "kernels":{"hysteria":"2.12.2","xray":"26.3.27","sing_box":"1.14.5","caddy":"2.10.2","client_sing_box":"1.14.5"},
 "artifacts":{"bui-linux-amd64":{"url":"$dl/bui-linux-amd64","sha256":"$2"},
              "bui-c-linux-amd64":{"url":"$dl/bui-c-linux-amd64","sha256":"$2"},
              "bui-c-linux-arm64":{"url":"$dl/bui-c-linux-arm64","sha256":"$2"},
              "sing-box-linux-amd64":{"url":"$dl/sing-box-linux-amd64","sha256":"$2"}}}
JSON
}
write_manifest "$pkg/manifest.json" "$sha"

( cd "$pkg" && exec python3 -m http.server 18099 --bind 127.0.0.1 ) >/dev/null 2>&1 &
srv=$!
ok=""
for _ in $(seq 1 50); do
  curl -fsS "$base/manifest.json" >/dev/null 2>&1 && { ok=1; break; }
  sleep 0.2
done
[ -n "$ok" ] || { echo "FAIL: 本地 http.server 没起来"; exit 1; }

# 1) 正常安装：C4 形状的 manifest（url 在 sha256 前）也要取得到 sha256
BUI_C_SOURCE="$base" BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
  BUI_C_PREFIX="$work/bin" bash "$here/bui-c-install.sh"
test -x "$work/bin/bui-c" || { echo "FAIL: 二进制没装上"; exit 1; }
[ "$(perm "$work/bin/bui-c")" = "755" ] || { echo "FAIL: 权限不是 755"; exit 1; }
cmp -s "$pkg/bui-c-linux-amd64" "$work/bin/bui-c" || { echo "FAIL: 内容与源不一致"; exit 1; }

# 2) 被篡改的产物必须拒绝安装，且一个字节都不写
printf 'tampered\n' > "$pkg/bui-c-linux-amd64"
cp "$pkg/bui-c-linux-amd64" "$pkg/bui-c-linux-arm64"
if BUI_C_SOURCE="$base" BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
     BUI_C_PREFIX="$work/bin2" bash "$here/bui-c-install.sh" 2>"$work/err"; then
  echo "FAIL: sha256 不符竟然安装成功"; exit 1
fi
grep -q "sha256 不符" "$work/err" || { echo "FAIL: 没报 sha256 不符"; cat "$work/err"; exit 1; }
test ! -e "$work/bin2/bui-c" || { echo "FAIL: 校验失败还是写了文件"; exit 1; }

# 3) manifest 取不到时报错退出，不留半个文件（三条来源都指本地 404，不出网）
if BUI_C_SOURCE="$base/nope" BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
     BUI_C_PREFIX="$work/bin3" bash "$here/bui-c-install.sh" 2>/dev/null; then
  echo "FAIL: manifest 404 竟然成功"; exit 1
fi
test ! -e "$work/bin3/bui-c" || { echo "FAIL: manifest 失败还是写了文件"; exit 1; }

# 4) manifest 里没有本机架构的 artifact（P5 漏发一个架构）→ 报错退出，不下载不写文件
cat > "$pkg/manifest.json" <<JSON
{"version":"4.0.0","kernels":{"client_sing_box":"1.14.5"},"artifacts":{}}
JSON
if BUI_C_SOURCE="$base" BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
     BUI_C_PREFIX="$work/bin4" bash "$here/bui-c-install.sh" 2>"$work/err4"; then
  echo "FAIL: manifest 缺 artifact 竟然成功"; exit 1
fi
grep -q "的 sha256" "$work/err4" || { echo "FAIL: 没报缺 sha256"; cat "$work/err4"; exit 1; }
test ! -e "$work/bin4/bui-c" || { echo "FAIL: 缺 artifact 还是写了文件"; exit 1; }

# 用例 2 与 4 把面板上的产物与 manifest 弄坏了，后面的用例要好的那一份
printf 'fake-bui-c-binary\n' > "$pkg/bui-c-linux-amd64"
cp "$pkg/bui-c-linux-amd64" "$pkg/bui-c-linux-arm64"
write_manifest "$pkg/manifest.json" "$sha"

# 5) 面板下发的那一份（占位符已被替换成面板的 /packages）→ 不设 BUI_C_SOURCE 也从面板装
sed "s|__BUI_C_PANEL_SOURCE__|$base|" "$here/bui-c-install.sh" > "$work/panel.sh"
if grep -q "__BUI_C_PANEL_SOURCE__" "$work/panel.sh"; then
  echo "FAIL: 占位符没被替换（脚本里那行的写法变了？）"; exit 1
fi
BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
  BUI_C_PREFIX="$work/bin5" bash "$work/panel.sh"
test -x "$work/bin5/bui-c" || { echo "FAIL: 面板占位符版没装上"; exit 1; }
cmp -s "$pkg/bui-c-linux-amd64" "$work/bin5/bui-c" || { echo "FAIL: 面板占位符版内容不一致"; exit 1; }

# 6) 面板源 404 → GitHub releases/latest 404 → 回退到 releases 列表里最新的预发布 tag
ghdir="$pkg/gh/releases/download/v4.0.0-rc9"
mkdir -p "$ghdir"
printf 'prerelease-bui-c-binary\n' > "$ghdir/bui-c-linux-amd64"
cp "$ghdir/bui-c-linux-amd64" "$ghdir/bui-c-linux-arm64"
ghsha="$(sha256sum "$ghdir/bui-c-linux-amd64" | cut -d' ' -f1)"
write_manifest "$ghdir/manifest.json" "$ghsha"
# 仿 GitHub API 的 releases 列表（新→旧）：带空格与换行的 pretty JSON。
#   第一条 v4.0.1 不是 vX.Y.Z-rcN 形状（且 prerelease=false）→ 必须跳过；它的正文里故意
#   写了字面的 \"tag_name\": \"v9.9.9\"，用来验证按字段名比对（$2 == "tag_name"）不会误命中；
#   第二条 nightly 也不是 rc 形状 → 跳过；第三条 v4.0.0-rc9 才是要选的那个；
#   第四条 v4.0.0-rc8 更旧，守住「只取第一个匹配」（选中它下面的 cmp 就过不了）。
#
# 夹具必须撑到 300KB 这么大，小了测不出真正的缺陷。脚本里那条 `tr … | awk …` 跑在
# `set -o pipefail` 下：awk 命中后若提前 exit，tr 会继续往已关闭的管道写而吃到 SIGPIPE
# （退出码 141），pipefail 把整条管道判为非零，set -e 于是静默杀掉整个脚本（2026-09-13
# 在 baiyi 真机 `bash -x` 复现，rc=141 且什么都不打印；真实 GitHub 响应 251KB）。
# 管道缓冲是 64KB，所以两段填充缺一不可：
#   * 第一条 body 里的 100KB，把命中的 rc tag 顶到 64KB 之后；
#   * 最后一条 body 里的 200KB，保证 awk 退出时 tr 还剩 ≫64KB 没写完 —— 只填前面的话，
#     awk 读到命中位置时 tr 早已写完并正常退出，SIGPIPE 根本不会发生（实测 rc=0）。
# 填充不含逗号，免得被脚本里的 `tr ',' '\n'` 切开。
{
  printf '%s' '[
  {
    "tag_name": "v4.0.1",
    "prerelease": false,
    "body": "正文里故意写了字面量 \"tag_name\": \"v9.9.9\" 用来验证按字段名比对不会误命中；后面是填充 '
  head -c 100000 /dev/zero | tr '\0' 'x'
  printf '%s' '"
  },
  {
    "tag_name": "nightly",
    "prerelease": true,
    "body": "不是 vX.Y.Z-rcN 形状 必须跳过"
  },
  {
    "tag_name": "v4.0.0-rc9",
    "prerelease": true,
    "body": "预发布 应当选中它"
  },
  {
    "tag_name": "v4.0.0-rc8",
    "prerelease": true,
    "body": "更旧的预发布 不该被选中；正文填充把管道撑过 64KB '
  head -c 200000 /dev/zero | tr '\0' 'y'
  printf '%s\n' '"
  }
]'
} > "$pkg/gh/releases.json"
BUI_C_SOURCE="$base/nope" BUI_C_GITHUB="$base/gh" BUI_C_RELEASES_API="$base/gh/releases.json" \
  BUI_C_PREFIX="$work/bin6" bash "$here/bui-c-install.sh" 2>"$work/err6"
test -x "$work/bin6/bui-c" || { echo "FAIL: 预发布回退没装上"; cat "$work/err6"; exit 1; }
cmp -s "$ghdir/bui-c-linux-amd64" "$work/bin6/bui-c" \
  || { echo "FAIL: 装的不是预发布 tag 下的那份产物"; exit 1; }
grep -q "回退到预发布 v4.0.0-rc9" "$work/err6" \
  || { echo "FAIL: 没报回退到 v4.0.0-rc9"; cat "$work/err6"; exit 1; }
# 探测型尝试（面板源、releases/latest）失败时已有中文提示，curl 自己的英文报错行只是噪音：
# 2026-09-13 baiyi 真机上每次都先蹦一行 `curl: (22) The requested URL returned error: 404`
if grep -q '^curl:' "$work/err6"; then
  echo "FAIL: 探测失败不该打出 curl 的原始报错行"; cat "$work/err6"; exit 1
fi

# 7) 三条来源都不通 → 报错退出，不写任何文件
if BUI_C_SOURCE="$base/nope" BUI_C_GITHUB="$gh404" BUI_C_RELEASES_API="$api404" \
     BUI_C_PREFIX="$work/bin7" bash "$here/bui-c-install.sh" 2>"$work/err7"; then
  echo "FAIL: 三条来源都不通竟然成功"; exit 1
fi
grep -q "都不可达" "$work/err7" || { echo "FAIL: 没报三条来源都不可达"; cat "$work/err7"; exit 1; }
test ! -e "$work/bin7/bui-c" || { echo "FAIL: 取不到 manifest 还是写了文件"; exit 1; }

echo "PASS"
