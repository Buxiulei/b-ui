#!/usr/bin/env bash
# scripts/bui-c-install.sh 的本地测试：起一个 127.0.0.1 的 http.server 当面板的
# /packages/ 目录，装到临时 PREFIX。不碰真实系统、不访问外网。
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
srv=""
cleanup() { [ -n "$srv" ] && kill "$srv" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

pkg="$work/packages"
mkdir -p "$pkg" "$work/bin"
printf 'fake-bui-c-binary\n' > "$pkg/bui-c-linux-amd64"
cp "$pkg/bui-c-linux-amd64" "$pkg/bui-c-linux-arm64"
sha="$(sha256sum "$pkg/bui-c-linux-amd64" | cut -d' ' -f1)"
dl="https://github.com/Buxiulei/b-ui/releases/download/v4.0.0"
# 总纲 C4 形状：artifact 是裸二进制的 {url, sha256}（url 在前），外加客户端不看的键
write_manifest() {
  cat > "$pkg/manifest.json" <<JSON
{"version":"4.0.0","released":"2026-09-12T00:00:00Z",
 "kernels":{"hysteria":"2.12.2","xray":"26.3.27","sing_box":"1.14.5","caddy":"2.10.2","client_sing_box":"1.14.5"},
 "artifacts":{"bui-linux-amd64":{"url":"$dl/bui-linux-amd64","sha256":"$1"},
              "bui-c-linux-amd64":{"url":"$dl/bui-c-linux-amd64","sha256":"$1"},
              "bui-c-linux-arm64":{"url":"$dl/bui-c-linux-arm64","sha256":"$1"},
              "sing-box-linux-amd64":{"url":"$dl/sing-box-linux-amd64","sha256":"$1"}}}
JSON
}
write_manifest "$sha"

( cd "$pkg" && exec python3 -m http.server 18099 --bind 127.0.0.1 ) >/dev/null 2>&1 &
srv=$!
ok=""
for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:18099/manifest.json" >/dev/null 2>&1 && { ok=1; break; }
  sleep 0.2
done
[ -n "$ok" ] || { echo "FAIL: 本地 http.server 没起来"; exit 1; }

# 1) 正常安装：C4 形状的 manifest（url 在 sha256 前）也要取得到 sha256
BUI_C_SOURCE="http://127.0.0.1:18099" BUI_C_PREFIX="$work/bin" bash "$here/bui-c-install.sh"
test -x "$work/bin/bui-c" || { echo "FAIL: 二进制没装上"; exit 1; }
[ "$(stat -c '%a' "$work/bin/bui-c")" = "755" ] || { echo "FAIL: 权限不是 755"; exit 1; }
cmp -s "$pkg/bui-c-linux-amd64" "$work/bin/bui-c" || { echo "FAIL: 内容与源不一致"; exit 1; }

# 2) 被篡改的产物必须拒绝安装，且一个字节都不写
printf 'tampered\n' > "$pkg/bui-c-linux-amd64"
cp "$pkg/bui-c-linux-amd64" "$pkg/bui-c-linux-arm64"
if BUI_C_SOURCE="http://127.0.0.1:18099" BUI_C_PREFIX="$work/bin2" bash "$here/bui-c-install.sh" 2>"$work/err"; then
  echo "FAIL: sha256 不符竟然安装成功"; exit 1
fi
grep -q "sha256 不符" "$work/err" || { echo "FAIL: 没报 sha256 不符"; cat "$work/err"; exit 1; }
test ! -e "$work/bin2/bui-c" || { echo "FAIL: 校验失败还是写了文件"; exit 1; }

# 3) manifest 取不到时报错退出，不留半个文件
if BUI_C_SOURCE="http://127.0.0.1:18099/nope" BUI_C_PREFIX="$work/bin3" bash "$here/bui-c-install.sh" 2>/dev/null; then
  echo "FAIL: manifest 404 竟然成功"; exit 1
fi
test ! -e "$work/bin3/bui-c" || { echo "FAIL: manifest 失败还是写了文件"; exit 1; }

# 4) manifest 里没有本机架构的 artifact（P5 漏发一个架构）→ 报错退出，不下载不写文件
cat > "$pkg/manifest.json" <<JSON
{"version":"4.0.0","kernels":{"client_sing_box":"1.14.5"},"artifacts":{}}
JSON
if BUI_C_SOURCE="http://127.0.0.1:18099" BUI_C_PREFIX="$work/bin4" bash "$here/bui-c-install.sh" 2>"$work/err4"; then
  echo "FAIL: manifest 缺 artifact 竟然成功"; exit 1
fi
grep -q "的 sha256" "$work/err4" || { echo "FAIL: 没报缺 sha256"; cat "$work/err4"; exit 1; }
test ! -e "$work/bin4/bui-c" || { echo "FAIL: 缺 artifact 还是写了文件"; exit 1; }

echo "PASS"
