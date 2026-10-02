#!/usr/bin/env bash
# Automatically discover portable test-*.sh; two private Docker tools are explicit opt-in.
set -uo pipefail
LC_ALL=C
cd "$(dirname "${BASH_SOURCE[0]}")" || exit 1
shopt -s nullglob
rc=0
passed=0
failed=0
executed=0
opt_in=0
for t in test-*.sh; do
    case "$t" in
        test-renderer-snapshot.sh)
            printf '# NOT RUN (explicit opt-in): %s requires private INPUT_DIRECTORY and empty OUTPUT_DIRECTORY in a network=none Docker container\n' "$t"
            printf '# Run separately: bash scripts/tests/test-renderer-snapshot.sh INPUT_DIRECTORY EMPTY_OUTPUT_DIRECTORY\n'
            opt_in=$((opt_in + 1))
            continue
            ;;
        test-residential-stock-gates.sh)
            printf '# NOT RUN (explicit opt-in): %s requires a preconfigured network=none Docker container and verified stock kernels/source\n' "$t"
            printf '# Run separately: BUI_TEST_CONTAINER=CONTAINER BUI_TEST_CONTAINER_SOURCE=SOURCE_DIRECTORY BUI_TEST_CONTAINER_TARGET=TARGET_DIRECTORY bash scripts/tests/test-residential-stock-gates.sh\n'
            opt_in=$((opt_in + 1))
            continue
            ;;
    esac
    printf '# %s\n' "$t"
    executed=$((executed + 1))
    if bash "$t"; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        rc=1
        printf '# FAILED: %s\n' "$t"
    fi
done
printf '# portable script tests: %s passed, %s failed, %s executed\n' "$passed" "$failed" "$executed"
printf '# %s explicit opt-in tools not run (not counted as passed)\n' "$opt_in"
if [[ "$executed" -eq 0 ]]; then
    printf '# FAILED: no portable script tests discovered\n'
    rc=1
fi
if [[ "$rc" -eq 0 ]]; then
    printf '# all portable script tests passed\n'
fi
exit "$rc"
