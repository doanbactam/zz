# Task: multi-file-refactor

The file `src/main.rs` has inline validation logic. Extract it into a
new file `src/validator.rs` as a function `validate(input: &str) -> bool`
that returns `true` if the input is non-empty and has length <= 100.
Update `src/main.rs` to use the new module.
