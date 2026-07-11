#!/bin/bash
# Verify add-function task: check src/lib.rs has fn add and test_add
# $1 = task dir, $2 = work dir
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/lib.rs"
    test -f "$TASK_DIR/expected/src/lib.rs"
    test -f "$TASK_DIR/scoring.md"
    grep -q "fn add" "$TASK_DIR/expected/src/lib.rs"
    echo "Eval structure valid: add-function"
    exit 0
fi

if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

if grep -q "fn add" "$WORK_DIR/src/lib.rs" && grep -q "test_add" "$WORK_DIR/src/lib.rs"; then
    echo "PASS: function and test added"
    exit 0
else
    echo "FAIL: function or test missing"
    exit 1
fi
