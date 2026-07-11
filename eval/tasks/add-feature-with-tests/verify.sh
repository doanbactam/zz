#!/bin/bash
# Verify add-feature-with-tests task
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
    echo "Eval structure valid: add-feature-with-tests"
    exit 0
fi

# Agent verification
if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

# Check capitalize function exists
if ! grep -q 'pub fn capitalize' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: capitalize function not found"
    exit 1
fi

# Check capitalize handles empty string
if ! grep -q 'None =>' "$WORK_DIR/src/lib.rs" && ! grep -q 'is_empty' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: capitalize doesn't handle empty string"
    exit 1
fi

# Check at least 3 tests exist for capitalize
TEST_COUNT=$(grep -c 'fn test_capitalize' "$WORK_DIR/src/lib.rs" || true)
if [ "$TEST_COUNT" -lt 1 ]; then
    echo "FAIL: need at least 1 test for capitalize (found $TEST_COUNT)"
    exit 1
fi

# Check existing functions still exist
if ! grep -q 'pub fn reverse' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: reverse function missing"
    exit 1
fi
if ! grep -q 'pub fn is_palindrome' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: is_palindrome function missing"
    exit 1
fi
if ! grep -q 'pub fn word_count' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: word_count function missing"
    exit 1
fi

echo "PASS: feature added with tests"
exit 0
