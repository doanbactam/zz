#!/bin/bash
# Verify refactor-extract task: check src/main.rs has fn calculate
# $1 = task dir, $2 = work dir
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/main.rs"
    test -f "$TASK_DIR/expected/src/main.rs"
    test -f "$TASK_DIR/scoring.md"
    grep -q "fn calculate" "$TASK_DIR/expected/src/main.rs"
    echo "Eval structure valid: refactor-extract"
    exit 0
fi

if [ ! -f "$WORK_DIR/src/main.rs" ]; then
    echo "FAIL: src/main.rs not found"
    exit 1
fi

if grep -q "fn calculate" "$WORK_DIR/src/main.rs"; then
    echo "PASS: function extracted"
    exit 0
else
    echo "FAIL: function not extracted"
    exit 1
fi
