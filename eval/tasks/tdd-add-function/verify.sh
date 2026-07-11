#!/bin/bash
# Verify tdd-add-function: check src/lib.rs has fn multiply and test_multiply
# $1 = task dir, $2 = work dir
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/lib.rs"
    test -f "$TASK_DIR/expected/src/lib.rs"
    test -f "$TASK_DIR/scoring.md"
    grep -q "fn multiply" "$TASK_DIR/expected/src/lib.rs"
    grep -q "test_multiply" "$TASK_DIR/expected/src/lib.rs"
    echo "Eval structure valid: tdd-add-function"
    exit 0
fi

if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

if grep -q "fn multiply" "$WORK_DIR/src/lib.rs" && \
   grep -q "test_multiply" "$WORK_DIR/src/lib.rs"; then
    echo "PASS: function and test added"
    exit 0
else
    echo "FAIL: function or test missing"
    exit 1
fi
