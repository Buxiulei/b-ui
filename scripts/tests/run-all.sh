#!/usr/bin/env bash
# 跑 scripts/tests 下所有 test-*.sh（自动发现，新增测试不用改本文件）。
set -uo pipefail
LC_ALL=C
cd "$(dirname "${BASH_SOURCE[0]}")" || exit 1
rc=0
for t in test-*.sh; do
    printf '# %s\n' "$t"
    if ! bash "$t"; then
        rc=1
        printf '# FAILED: %s\n' "$t"
    fi
done
if [[ "$rc" -eq 0 ]]; then
    printf '# all script tests passed\n'
fi
exit "$rc"
