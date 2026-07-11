#!/bin/bash
# Verify multi-file-refactor: check src/validator.rs exists with fn validate,
# and src/main.rs uses the validator module.
# $1 = task dir, $2 = work dir
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/main.rs"
    test -f "$TASK_DIR/expected/src/main.rs"
    test -f "$TASK_DIR/expected/src/validator.rs"
    test -f "$TASK_DIR/scoring.md"
    grep -q "fn validate" "$TASK_DIR/expected/src/validator.rs"
    echo "Eval structure valid: multi-file-refactor"
    exit 0
fi

# Agent verification
if [ ! -f "$WORK_DIR/src/validator.rs" ]; then
    echo "FAIL: src/validator.rs not found"
    exit 1
fi

if [ ! -f "$WORK_DIR/src/main.rs" ]; then
    echo "FAIL: src/main.rs not found"
    exit 1
fi

if grep -q "fn validate" "$WORK_DIR/src/validator.rs" && \
   grep -q "validator" "$WORK_DIR/src/main.rs"; then
    echo "PASS: validator module extracted and used"
    exit 0
else
    echo "FAIL: validator not properly extracted"
    exit 1
fi
