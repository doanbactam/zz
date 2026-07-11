# Task: refactor-error-handling

Refactor all functions in `src/lib.rs` to use `Result` instead of `panic!`/`unwrap()`.

Requirements:
- `parse_config` returns `Result<Vec<(String, String)>, String>`
- `read_file` returns `Result<String, String>`
- `parse_number` returns `Result<i64, String>`
- `safe_divide` returns `Result<i64, String>` (Err on division by zero)
- All existing tests must still pass (update them to use `.unwrap()`)
- Add at least 2 new tests for error cases (missing file, bad input, division by zero)

Do not add external dependencies.
