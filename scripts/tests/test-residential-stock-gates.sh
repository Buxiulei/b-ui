#!/usr/bin/env bash
# Explicit isolated integration. The Rust fixture owns/reaps each child; timeout bounds Cargo too.
set -euo pipefail
container=${BUI_TEST_CONTAINER:-bui-policy-tests-20260929}
source_dir=${BUI_TEST_CONTAINER_SOURCE:-/tmp/bui-residential-activation-impl-src}
target_dir=${BUI_TEST_CONTAINER_TARGET:-/tmp/bui-residential-activation-baseline-target}
# 官方 sing-box 1.14.3 arm64 归档解包后的**裸二进制** sha256（不再是自建产物）。
# 归档本身：sing-box-1.14.3-linux-arm64.tar.gz，sha256
#   29dae24d76bea3bc62d07dd3e60cfc12003de7debbd6b768df9be9c68360174e
# 夹具容器必须用这份官方二进制重建后本脚本才有意义；历史自建 1.14.2 的 PASS 不继承。
expected_sha=299f9f6080d9c43504714ddddae6aa9b853d430ccfc816c456a73613b5455689
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
umask 077
run_log=$(mktemp "${TMPDIR:-/tmp}/bui-stock-run.XXXXXXXX")
trap 'rm -f -- "$run_log"' EXIT
# 手动 runner：需要一个 network=none 的隔离夹具容器。`run-all.sh` 按 `test-*.sh` 通配无参调用
# 本文件，而 CI 与开发机上都不存在这个容器；那种情况按「缺前置 ⇒ skip」处理，不能算失败——
# 否则 CI 的 lint job 恒红。（容器在、但网络模式不是 none，仍旧判 FAIL，见下。）
if ! docker inspect "$container" >/dev/null 2>&1; then
  printf '%s\n' "skipped: 隔离夹具容器 $container 不存在（手动 runner，需先建 network=none 的 stock 夹具）"
  exit 0
fi
[[ $(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container") == none ]] || {
  printf '%s\n' 'FAIL: stock fixture requires a network=none container' >&2; exit 1;
}
docker exec "$container" sh -eu -c '
  test "$(uname -s)" = Linux
  test "$(readlink -f /usr/local/bin/sing-box)" = /new-kernel/sing-box
  test "$(sha256sum /usr/local/bin/sing-box | cut -d " " -f 1)" = "$1"
  test "$(/usr/local/bin/sing-box version | head -n 1)" = "sing-box version 1.14.3"
  /usr/local/bin/sing-box version
  sha256sum /usr/local/bin/sing-box
' sh "$expected_sha"
set +e
docker exec -w "$source_dir" -e RUSTUP_TOOLCHAIN=1.92.0 -e CARGO_TARGET_DIR="$target_dir" \
  -e BUI_TEST_STOCK_SINGBOX_PATH=/usr/local/bin/sing-box "$container" \
  timeout --signal=TERM --kill-after=10s 600s cargo test -p bui --locked --offline \
  stock_residential_gate_restore_and_payloads -- --ignored --exact \
  modules::panel::stock_gate_fixture::stock_residential_gate_restore_and_payloads --nocapture >"$run_log" 2>&1
rc=$?
set -e
cat "$run_log"
[[ $rc == 0 ]] || exit "$rc"
python3 "$script_dir/verify-fixture-run.py" stock "$run_log"
