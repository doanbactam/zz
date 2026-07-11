# ZeroZero Eval Suite

Standard benchmark tasks for measuring agent capability.

## Structure

Each eval task is a directory under `eval/tasks/<name>/` containing:
- `task.md` — task description (the prompt given to the agent)
- `fixture/` — initial repo state (files to set up before running)
- `expected/` — expected outcome (files that should exist/change)
- `verify.sh` — verification script (exit 0 = pass, exit 1 = fail)
- `scoring.md` — rubric for partial credit

## Tasks

| Name | Category | Description |
|---|---|---|
| fix-typo | fix-bug | Fix a typo in a Rust file |
| add-function | add-feature | Add a utility function with tests |
| refactor-extract | refactor | Extract a function from inline code |
| hello-world | TUI | Start TUI and verify it renders |

## Running

```bash
# Run all evals
./eval/run.sh

# Run single eval
./eval/run.sh fix-typo
```

## Scoring

Each task: 0 (fail) or 1 (pass). Score = passed / total.
Promotion gate: score must be ≥ previous version's score.
