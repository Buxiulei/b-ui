#!/usr/bin/env bash
# 极简 TAP 断言库：无外部依赖。用法见 scripts/tests/test-*.sh。
# 注意：使用者不要开 set -e —— 测试要主动检查非零退出码。
LC_ALL=C
TESTS_RUN=0
TESTS_FAILED=0

ok() { TESTS_RUN=$((TESTS_RUN + 1)); printf 'ok %d - %s\n' "$TESTS_RUN" "$1"; }

fail() {
    TESTS_RUN=$((TESTS_RUN + 1))
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf 'not ok %d - %s\n' "$TESTS_RUN" "$1"
}

assert_eq() {
    if [[ "$1" == "$2" ]]; then
        ok "$3"
    else
        fail "$3"
        printf '    expected: %s\n    actual:   %s\n' "$1" "$2"
    fi
}

assert_contains() {
    if [[ "$2" == *"$1"* ]]; then
        ok "$3"
    else
        fail "$3"
        printf '    missing: %s\n    in:      %s\n' "$1" "$2"
    fi
}

assert_not_contains() {
    if [[ "$2" != *"$1"* ]]; then
        ok "$3"
    else
        fail "$3"
        printf '    unexpected: %s\n' "$1"
    fi
}

finish() {
    printf '1..%d\n' "$TESTS_RUN"
    [[ "$TESTS_FAILED" -eq 0 ]]
    exit $?
}
