#!/usr/bin/env bash
# Pure offline producer/Plan adapter. Only directory paths appear in argv, never credentials.
set -euo pipefail
[[ $# == 2 ]] || { printf '%s\n' 'usage: test-renderer-snapshot.sh INPUT_DIRECTORY EMPTY_OUTPUT_DIRECTORY' >&2; exit 2; }
input_dir=$1
output_dir=$2
container=${BUI_TEST_CONTAINER:-bui-policy-tests-20260929}
source_dir=${BUI_TEST_CONTAINER_SOURCE:-/tmp/bui-residential-activation-impl-src}
target_dir=${BUI_TEST_CONTAINER_TARGET:-/tmp/bui-residential-activation-baseline-target}
[[ -f $input_dir/inputs.private.json && -d $output_dir && ! -L $output_dir ]] || exit 2
[[ $(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container") == none ]] || {
  printf '%s\n' 'FAIL: offline snapshot requires a network=none container' >&2; exit 1;
}
umask 077
chmod 700 "$output_dir"
remote_dir=$(docker exec "$container" mktemp -d /tmp/bui-renderer-snapshot.XXXXXXXX)
[[ $remote_dir == /tmp/bui-renderer-snapshot.* ]] || exit 1
cleanup() { docker exec "$container" rm -rf -- "$remote_dir" >/dev/null; }
trap cleanup EXIT
docker exec "$container" mkdir -m 700 "$remote_dir/input" "$remote_dir/output"
docker cp "$input_dir/." "$container:$remote_dir/input/" >/dev/null
set +e
docker exec -w "$source_dir" -e RUSTUP_TOOLCHAIN=1.92.0 -e CARGO_TARGET_DIR="$target_dir" \
  -e BUI_RENDER_INPUT_DIR="$remote_dir/input" -e BUI_RENDER_OUTPUT_DIR="$remote_dir/output" "$container" \
  timeout --signal=TERM --kill-after=10s 600s cargo test -p bui --locked --offline \
  offline_renderer_snapshot::offline_renderer_snapshot -- --ignored --exact --nocapture
rc=$?
set -e
docker cp "$container:$remote_dir/output/." "$output_dir/" >/dev/null
# Preserve private error evidence even when the adapter rejects a real deployment risk.
exit "$rc"
