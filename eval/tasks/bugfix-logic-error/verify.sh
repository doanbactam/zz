#!/bin/bash
# Verify bugfix-logic-error task
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
    echo "Eval structure valid: bugfix-logic-error"
    exit 0
fi

# Agent verification
if [ ! -f "$WORK_DIR/src/lib.rs" ]; then
    echo "FAIL: src/lib.rs not found"
    exit 1
fi

# Bug 1: leap year must use || for 100/400 rule
if ! grep -q '% 100 != 0' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: is_leap_year missing '!= 0' check for 100"
    exit 1
fi

# Bug 2: February must check is_leap_year
if ! grep -q 'is_leap_year' "$WORK_DIR/src/lib.rs" | head -1 > /dev/null; then
    echo "FAIL: days_in_month doesn't call is_leap_year"
    exit 1
fi

# Bug 3: ISBN weight must be (10 - i) not (i + 1)
if grep -q 'i as u32 + 1' "$WORK_DIR/src/lib.rs"; then
    echo "FAIL: ISBN still uses wrong weight (i + 1)"
    exit 1
fi

echo "PASS: all 3 logic bugs fixed"
exit 0
