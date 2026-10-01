#!/usr/bin/env bash
# Explicit isolated integration. The Rust fixture owns/reaps each child; timeout bounds Cargo too.
set -euo pipefail
container=${BUI_TEST_CONTAINER:-bui-policy-tests-20260929}
source_dir=${BUI_TEST_CONTAINER_SOURCE:-/tmp/bui-residential-activation-impl-src}
target_dir=${BUI_TEST_CONTAINER_TARGET:-/tmp/bui-residential-activation-baseline-target}
expected_sha=1a60ac17d93042c5a12410cfe83472ddee5084131dfae7a9b9806926ffb84447
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
umask 077
run_log=$(mktemp "${TMPDIR:-/tmp}/bui-stock-run.XXXXXXXX")
trap 'rm -f -- "$run_log"' EXIT
[[ $(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container") == none ]] || {
  printf '%s\n' 'FAIL: stock fixture requires a network=none container' >&2; exit 1;
}
docker exec "$container" sh -eu -c '
  test "$(uname -s)" = Linux
  test "$(readlink -f /usr/local/bin/sing-box)" = /new-kernel/sing-box
  test "$(sha256sum /usr/local/bin/sing-box | cut -d " " -f 1)" = "$1"
  test "$(/usr/local/bin/sing-box version | head -n 1)" = "sing-box version 1.14.2"
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
