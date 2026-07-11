# Task: bugfix-logic-error

Fix 3 logic bugs in `src/lib.rs`:

1. `is_leap_year`: returns wrong result for years like 2000 (should be leap) and 1900 (should not be leap). The condition logic is wrong.
2. `days_in_month`: February always returns 28, even for leap years.
3. `is_valid_isbn10`: checksum weight is wrong — uses index+1 instead of 10-index.

Fix all three bugs. Do not change function signatures or test cases.
