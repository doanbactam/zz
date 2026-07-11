#!/bin/bash
# ZeroZero eval runner.
#
# Usage:
#   ./eval/run.sh              — structure validation only (fast)
#   ./eval/run.sh --agent      — run zz exec on each task, then verify
#   ./eval/run.sh --agent fix-typo  — run single task with agent

set -e

EVAL_DIR="$(cd "$(dirname "$0")" && pwd)"
TASKS_DIR="$EVAL_DIR/tasks"
RUN_AGENT=false

# Parse args
TASK_FILTER=""
while [ $# -gt 0 ]; do
    case "$1" in
        --agent) RUN_AGENT=true; shift ;;
        *) TASK_FILTER="$1"; shift ;;
    esac
done

# Build task list
if [ -n "$TASK_FILTER" ]; then
    if [ "$RUN_AGENT" = true ]; then
        TASKS=("$TASK_FILTER")
    else
        TASKS=("$TASK_FILTER")
    fi
else
    TASKS=()
    for d in "$TASKS_DIR"/*/; do
        TASKS+=("$(basename "$d")")
    done
fi

PASSED=0
TOTAL=0

for task in "${TASKS[@]}"; do
    TASK_DIR="$TASKS_DIR/$task"
    if [ ! -d "$TASK_DIR" ]; then
        echo "FAIL: task '$task' not found"
        continue
    fi

    TOTAL=$((TOTAL + 1))

    if [ "$RUN_AGENT" = false ]; then
        # Structure validation mode
        echo -n "Validating: $task ... "
        if bash "$TASK_DIR/verify.sh" "$TASK_DIR" "$TASK_DIR" 2>/dev/null; then
            echo "PASS"
            PASSED=$((PASSED + 1))
        else
            echo "FAIL"
        fi
        continue
    fi

    # Agent mode: run zz exec on the task
    echo -n "Running agent on: $task ... "

    # Copy fixture to a temp working directory
    WORK=$(mktemp -d)
    cp -r "$TASK_DIR/fixture/"* "$WORK/" 2>/dev/null || true
    # Also copy hidden files if any
    cp -r "$TASK_DIR/fixture/".* "$WORK/" 2>/dev/null || true

    # Read the task prompt
    PROMPT=$(cat "$TASK_DIR/task.md")

    # Find zz binary: prefer cargo target, then PATH
    ZZ_BIN=""
    PROJECT_ROOT="$(cd "$EVAL_DIR/.." && pwd)"
    if [ -x "$PROJECT_ROOT/target/debug/zz" ]; then
        ZZ_BIN="$PROJECT_ROOT/target/debug/zz"
    elif [ -x "$PROJECT_ROOT/target/release/zz" ]; then
        ZZ_BIN="$PROJECT_ROOT/target/release/zz"
    elif command -v zz > /dev/null 2>&1; then
        ZZ_BIN="zz"
    else
        echo "FAIL (zz not found)"
        cd "$EVAL_DIR"
        rm -rf "$WORK"
        continue
    fi

    # Copy .env to work dir so dotenvy loads API key
    if [ -f "$PROJECT_ROOT/.env" ]; then
        cp "$PROJECT_ROOT/.env" "$WORK/.env"
    fi

    # Run zz exec in the work directory with sandbox=full-access,
    # approval=never for unattended eval. 120s timeout per task.
    cd "$WORK"
    if ZZ_SANDBOX=full-access ZZ_APPROVAL=never ZZ_MAX_TURNS=10 \
       timeout 240 "$ZZ_BIN" exec "$PROMPT" > /dev/null 2>&1; then
        # Agent completed, run verify
        if bash "$TASK_DIR/verify.sh" "$TASK_DIR" "$WORK" 2>/dev/null; then
            echo "PASS"
            PASSED=$((PASSED + 1))
        else
            echo "FAIL (verify)"
        fi
    else
        echo "FAIL (agent timeout or error)"
    fi

    cd "$EVAL_DIR"
    rm -rf "$WORK"
done

echo ""
echo "Score: $PASSED / $TOTAL"
if [ "$TOTAL" -gt 0 ]; then
    PERCENTAGE=$((PASSED * 100 / TOTAL))
    echo "  ($PERCENTAGE%)"
fi

if [ "$PASSED" -eq "$TOTAL" ]; then
    exit 0
else
    exit 1
fi
