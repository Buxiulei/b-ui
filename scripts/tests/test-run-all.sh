#!/usr/bin/env bash
# Exercise the real runner in private fixtures, without running network/kernel tests.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
umask 077
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/bui-run-all-test.XXXXXXXX")
trap 'rm -rf -- "$fixture_root"' EXIT
failures=0

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

assert_contains() {
    grep -Fq -- "$2" "$1" || fail "$3"
}

prepare_fixture() {
    mkdir "$1"
    cp "$script_dir/run-all.sh" "$1/run-all.sh"
    # These tools would have an observable side effect if the runner invoked them.
    cat >"$1/test-renderer-snapshot.sh" <<'SH'
#!/usr/bin/env bash
printf 'snapshot\n' >> opt-in-touched
SH
    cat >"$1/test-official-api-g0.sh" <<'SH'
#!/usr/bin/env bash
printf 'official-api-g0\n' >> opt-in-touched
SH
    cat >"$1/test-residential-stock-gates.sh" <<'SH'
#!/usr/bin/env bash
printf 'stock\n' >> opt-in-touched
SH
}

assert_opt_in_not_run() {
    [[ ! -e $1/opt-in-touched ]] || fail "$2: explicit opt-in tool was executed"
    assert_contains "$1/run.log" '# NOT RUN (explicit opt-in): test-renderer-snapshot.sh' "$2: snapshot exclusion was silent"
    assert_contains "$1/run.log" 'private INPUT_DIRECTORY and empty OUTPUT_DIRECTORY' "$2: snapshot prerequisites were not explained"
    assert_contains "$1/run.log" 'bash scripts/tests/test-renderer-snapshot.sh INPUT_DIRECTORY EMPTY_OUTPUT_DIRECTORY' "$2: snapshot invocation was not shown"
    assert_contains "$1/run.log" '# NOT RUN (explicit opt-in): test-residential-stock-gates.sh' "$2: stock exclusion was silent"
    assert_contains "$1/run.log" 'preconfigured network=none Docker container' "$2: stock prerequisites were not explained"
    assert_contains "$1/run.log" 'BUI_TEST_CONTAINER_SOURCE=SOURCE_DIRECTORY' "$2: stock source argument was not shown"
    assert_contains "$1/run.log" 'bash scripts/tests/test-residential-stock-gates.sh' "$2: stock invocation was not shown"
    assert_contains "$1/run.log" '# NOT RUN (explicit opt-in): test-official-api-g0.sh' "$2: official API exclusion was silent"
    assert_contains "$1/run.log" 'BUI_G0_BINARY=VERIFIED_OFFICIAL_BINARY' "$2: official API binary prerequisite was not shown"
    assert_contains "$1/run.log" 'bash scripts/tests/test-official-api-g0.sh' "$2: official API invocation was not shown"
    assert_contains "$1/run.log" '# 3 explicit opt-in tools not run (not counted as passed)' "$2: opt-in tools were counted as passed or omitted from the summary"
}

# A failed portable test must not hide the result or stop later discovered tests.
failed_case=$fixture_root/failure
prepare_fixture "$failed_case"
cat >"$failed_case/test-01-success.sh" <<'SH'
#!/usr/bin/env bash
printf 'first\n' >> executed.log
SH
cat >"$failed_case/test-02-failure.sh" <<'SH'
#!/usr/bin/env bash
printf 'failure\n' >> executed.log
exit 7
SH
cat >"$failed_case/test-03-after-failure.sh" <<'SH'
#!/usr/bin/env bash
printf 'after\n' >> executed.log
SH
failed_rc=0
bash "$failed_case/run-all.sh" >"$failed_case/run.log" 2>&1 || failed_rc=$?
[[ $failed_rc == 1 ]] || fail 'portable failure must make the runner exit 1'
printf 'first\nfailure\nafter\n' >"$failed_case/expected.log"
cmp -s "$failed_case/expected.log" "$failed_case/executed.log" || fail 'portable tests must all execute, including after a failure'
assert_contains "$failed_case/run.log" '# FAILED: test-02-failure.sh' 'failing portable script was not identified'
assert_contains "$failed_case/run.log" '# portable script tests: 2 passed, 1 failed, 3 executed' 'failure summary counts were inaccurate'
if grep -Fq '# all portable script tests passed' "$failed_case/run.log"; then
    fail 'failure was also reported as all tests passing'
fi
assert_opt_in_not_run "$failed_case" failure

# An unlisted future test is discovered automatically; opt-in tools do not inflate pass counts.
success_case=$fixture_root/success
prepare_fixture "$success_case"
cat >"$success_case/test-z-future.sh" <<'SH'
#!/usr/bin/env bash
printf 'future\n' > executed.log
SH
cat >"$success_case/helper.sh" <<'SH'
#!/usr/bin/env bash
printf 'helper\n' >> opt-in-touched
SH
success_rc=0
bash "$success_case/run-all.sh" >"$success_case/run.log" 2>&1 || success_rc=$?
[[ $success_rc == 0 ]] || fail 'successful portable test must make the runner exit 0'
printf 'future\n' >"$success_case/expected.log"
cmp -s "$success_case/expected.log" "$success_case/executed.log" || fail 'an automatically discovered future test did not execute'
assert_contains "$success_case/run.log" '# portable script tests: 1 passed, 0 failed, 1 executed' 'success summary counts were inaccurate'
assert_contains "$success_case/run.log" '# all portable script tests passed' 'portable success was not reported'
assert_opt_in_not_run "$success_case" success

# Having only opt-in tools must fail instead of silently passing with zero portable coverage.
empty_case=$fixture_root/empty
prepare_fixture "$empty_case"
empty_rc=0
bash "$empty_case/run-all.sh" >"$empty_case/run.log" 2>&1 || empty_rc=$?
[[ $empty_rc == 1 ]] || fail 'zero discovered portable tests must make the runner exit 1'
assert_contains "$empty_case/run.log" '# FAILED: no portable script tests discovered' 'empty discovery was not explicitly rejected'
assert_contains "$empty_case/run.log" '# portable script tests: 0 passed, 0 failed, 0 executed' 'empty discovery summary counts were inaccurate'
assert_opt_in_not_run "$empty_case" empty

# An empty glob must not be treated as a literal script named test-*.sh.
no_files_case=$fixture_root/no-files
mkdir "$no_files_case"
cp "$script_dir/run-all.sh" "$no_files_case/run-all.sh"
no_files_rc=0
bash "$no_files_case/run-all.sh" >"$no_files_case/run.log" 2>&1 || no_files_rc=$?
[[ $no_files_rc == 1 ]] || fail 'a directory without tests must make the runner exit 1'
assert_contains "$no_files_case/run.log" '# FAILED: no portable script tests discovered' 'no-file discovery was not explicitly rejected'
assert_contains "$no_files_case/run.log" '# portable script tests: 0 passed, 0 failed, 0 executed' 'an unmatched glob was counted as an executed test'
assert_contains "$no_files_case/run.log" '# 0 explicit opt-in tools not run (not counted as passed)' 'absent opt-in files were counted as excluded'

if [[ $failures != 0 ]]; then
    printf 'FAIL: %s runner contract assertions failed\n' "$failures" >&2
    exit 1
fi
printf 'PASS: runner discovery, failure continuation, exact counts, explicit opt-in isolation, and empty discovery rejection\n'
