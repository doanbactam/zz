# Scoring: bug-fix-multi-step

1 point per bug correctly fixed (3 total):

- **1 point** if `count_vowels` uses `0..bytes.len()` (fixes off-by-one + empty string panic)
- **1 point** if `find_substring` checks `n_bytes.len() > h_bytes.len()` before slicing
- **1 point** if `factorial` returns `1` for `n=1` (not `2`)

Maximum score: 3/3.
