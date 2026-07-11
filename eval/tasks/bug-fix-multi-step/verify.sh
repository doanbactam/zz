#!/bin/bash
# Verify bug-fix-multi-step task
# $1 = task dir, $2 = work dir (agent's working directory)
set -e
TASK_DIR="$1"
WORK_DIR="${2:-$TASK_DIR}"

# Structure validation (when WORK_DIR == TASK_DIR)
if [ "$WORK_DIR" = "$TASK_DIR" ]; then
    test -f "$TASK_DIR/task.md"
    test -f "$TASK_DIR/fixture/src/lib.rs"
    test -f "$TASK_DIR/expected/src/lib.rs"
    test -f "$TASK_DIR/scoring.md"
    echo "Eval structure valid: bug-fix-multi-step"
    exit 0
fi

# Agent verification: check all 3 bugs are fixed
if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

# Bug 1: count_vowels must handle empty string without panic
# and count all vowels (including last char)
if ! grep -q '0..bytes.len()' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: count_vowels still has off-by-one (not using 0..len())"
    exit 1
fi

# Bug 2: find_substring must check needle length before slicing
if ! grep -q 'n_bytes.len() > h_bytes.len()' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: find_substring missing length check"
    exit 1
fi

# Bug 3: factorial must return 1 for n=1
if ! grep -q 'return 1' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: factorial still returns wrong value for n=1"
    exit 1
fi

echo "PASS: all 3 bugs fixed"
exit 0
