#!/bin/bash
# Verify refactor-error-handling task
# $1 = task dir, $2 = work dir (agent's working directory)
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

# Structure validation
if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/lib.rs"
    test -f "$TASK_DIR/expected/src/lib.rs"
    test -f "$TASK_DIR/scoring.md"
    echo "Eval structure valid: refactor-error-handling"
    exit 0
fi

# Agent verification
if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

# Check parse_config returns Result
if ! grep -q 'fn parse_config.*Result' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: parse_config must return Result"
    exit 1
fi

# Check no unwrap() in main functions (only in tests)
UNWRAP_COUNT=$(grep -c '\.unwrap()' "$WORK_DIR/src/lib.rs" || true)
# Count unwrap in tests only
TEST_UNWRAP=$(grep -A100 '#\[cfg(test)\]' "$WORK_DIR/src/lib.rs" | grep -c '\.unwrap()' || true)
MAIN_UNWRAP=$((UNWRAP_COUNT - TEST_UNWRAP))
if [ "$MAIN_UNWRAP" -gt 0 ]; then
    echo "FAIL: found $MAIN_UNWRAP unwrap() calls outside tests"
    exit 1
fi

# Check no panic! in main functions
if grep -q 'panic!' "$WORK_DIR/src/lib.rs" && ! grep -A100 '#\[cfg(test)\]' "$WORK_DIR/src/lib.rs" | grep -q 'panic!'; then
    :
elif grep 'panic!' "$WORK_DIR/src/lib.rs" | grep -v 'cfg(test)' | grep -v 'test' > /dev/null 2>&1; then
    echo "FAIL: found panic! in main code"
    exit 1
fi

# Check division by zero handling
if ! grep -q 'b == 0\|b.is_zero\|division by zero' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: missing division by zero check"
    exit 1
fi

echo "PASS: error handling refactored"
exit 0
