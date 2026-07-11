# Scoring: refactor-error-handling

1. **1 point** if `parse_config` returns `Result` (not panics on bad input)
2. **1 point** if no `unwrap()` calls outside test code
3. **1 point** if `safe_divide` handles division by zero
4. **1 point** if at least 2 error-case tests exist and pass

Maximum score: 4/4.
