#!/usr/bin/env bash
# Explicit lab-only gate; a real accounting defect is a nonzero outcome, never expected-failure PASS.
set -euo pipefail
container=${BUI_G0_CONTAINER:?set BUI_G0_CONTAINER to the owned NetworkMode=none lab}
source_dir=${BUI_G0_SOURCE:?set BUI_G0_SOURCE to the copied fixture source}
target_dir=${BUI_G0_TARGET:?set BUI_G0_TARGET to a dedicated Cargo target}
binary=${BUI_G0_BINARY:?set BUI_G0_BINARY to the verified official archive binary}
archive=${BUI_G0_ARCHIVE:?set BUI_G0_ARCHIVE to the verified official archive}
result=${BUI_G0_RESULT:-/tmp/g0-result.safe.json}
expected=b8610f45abb7e967e195264383f5cbd20fba7821a3c37e3a8c4c5ab6cad28eac
branch=$(git branch --show-current)
[[ $(docker inspect --format '{{.HostConfig.NetworkMode}}' "$container") == none ]] || { echo 'FAIL: NetworkMode must be none' >&2; exit 1; }
docker exec "$container" sh -eu -c 'test "$(uname -s)" = Linux; test "$(sha256sum "$1" | cut -d " " -f 1)" = "$2"' sh "$binary" "$expected"
docker exec -i "$container" python3 - "$archive" "$binary" <<'PYVERIFY'
import hashlib,pathlib,sys,tarfile
archive,binary=sys.argv[1:]
assert hashlib.sha256(pathlib.Path(archive).read_bytes()).hexdigest()=='b43a1fb1bda131c6653576741ce527eb2bdeab7c9308ca90ee8b972abb7e4a7f'
with tarfile.open(archive,'r:gz') as tf:
    member=tf.getmember('sing-box-1.14.2-linux-arm64/sing-box')
    assert member.isfile(), 'official archive binary must be a regular file'
    data=tf.extractfile(member).read()
assert hashlib.sha256(data).hexdigest()=='b8610f45abb7e967e195264383f5cbd20fba7821a3c37e3a8c4c5ab6cad28eac'
assert data==pathlib.Path(binary).read_bytes(), 'live binary differs from official archive entry'
PYVERIFY
log=$(mktemp)
trap 'rm -f -- "$log"' EXIT
set +e
docker exec -w "$source_dir" -e RUSTUP_TOOLCHAIN=1.92.0 -e BUI_G0_ARCHIVE="$archive" -e CARGO_TARGET_DIR="$target_dir" -e BUI_G0_BINARY="$binary" \
  -e BUI_G0_NETWORK_NONE=verified-by-wrapper -e BUI_G0_RESULT="$result" -e BUI_G0_BRANCH="$branch" \
  "$container" timeout --signal=TERM --kill-after=10s 600s cargo test -p bui --locked --offline \
  official_api_g0 -- --ignored --exact modules::panel::official_api_fixture::official_api_g0 --nocapture >"$log" 2>&1
rc=$?
set -e
cat "$log"
# The wrapper validates real execution even on a migration FAIL. No result rewriting.
python3 - "$log" <<'PY'
import pathlib,re,sys
text=pathlib.Path(sys.argv[1]).read_text()
assert re.search(r'^running 1 test$',text,re.M), 'target test did not execute exactly once'
assert re.search(r'^test modules::panel::official_api_fixture::official_api_g0 \.\.\. (ok|FAILED)$',text,re.M), 'missing explicit fixture result'
PY
docker exec -i "$container" python3 - "$result" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['executed_cases']==r['required_cases'], 'missing required case'
assert r['provenance']['official_archive_verified'] is True
assert r['zero_case_skips']==0
assert r['children_reaped'] is True
for c in r['cases']:
    assert c['delivery_verified'] or (c['gap_detected'] and c['actual_gate_denied']), c['name']
    if c['gap_detected']:
        assert c['post_gap_denial_verified'] is True, c['name'] + ': gap not contained after detection'
assert r['identity_attribution_complete'] is True, 'accounting migration blocked'
assert all(c['accounting_complete'] for c in r['cases']), 'incomplete per-case ledger'
assert r['migration_gate']=='PASS'
PY
[[ $rc == 0 ]] || exit "$rc"
