# Scoring: bugfix-logic-error

1. **1 point** if `is_leap_year` uses `year % 100 != 0 || year % 400 == 0`
2. **1 point** if `days_in_month` calls `is_leap_year` for February
3. **1 point** if `is_valid_isbn10` uses `10 - i` weight

Maximum score: 3/3.
