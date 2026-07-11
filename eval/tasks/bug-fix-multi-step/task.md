# Task: bug-fix-multi-step

Fix all bugs in `src/lib.rs`. There are 3 bugs:

1. `count_vowels` panics on empty string (off-by-one: `0..len()-1` underflows when len=0) and misses the last character of non-empty strings.
2. `find_substring` panics when needle is longer than haystack (slice out of bounds) and misses the last possible match position.
3. `factorial` returns 2 for `n=1` instead of 1.

Fix all three bugs. Do not change the function signatures or test cases.
