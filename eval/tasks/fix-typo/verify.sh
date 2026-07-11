#!/bin/bash
# Verify fix-typo task: check that src/main.rs has "Hello World" (not "Helo World")
# $1 = task dir, $2 = work dir (agent's working directory)
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

# Structure validation (when WORK_DIR == TASK_DIR)
if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/main.rs"
    test -f "$TASK_DIR/expected/src/main.rs"
    test -f "$TASK_DIR/scoring.md"
    grep -q "Hello World" "$TASK_DIR/expected/src/main.rs"
    echo "Eval structure valid: fix-typo"
    exit 0
fi

# Agent verification: check the agent fixed the typo
if [ ! -f "$WORK_DIR/src/main.rs" ]; then
    echo "FAIL: src/main.rs not found"
    exit 1
fi

if grep -q "Hello World" "$WORK_DIR/src/main.rs" && ! grep -q "Helo World" "$WORK_DIR/src/main.rs"; then
    echo "PASS: typo fixed"
    exit 0
else
    echo "FAIL: typo not fixed"
    exit 1
fi
