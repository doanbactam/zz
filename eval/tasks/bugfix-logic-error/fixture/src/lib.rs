/// Check if a year is a leap year.
/// BUG: uses && instead of || for the 100/400 rule.
pub fn is_leap_year(year: u32) -> bool {
    year % 4 == 0 && year % 100 == 0 && year % 400 == 0
}

/// Calculate the number of days in a month (1-12) for a given year.
/// BUG: February always returns 28 (ignores leap year).
pub fn days_in_month(month: u32, year: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28,
        _ => 0,
    }
}

/// Check if a string is a valid ISBN-10 (simplified).
/// BUG: checksum calculation is wrong (multiplies by index instead of 10-index).
pub fn is_valid_isbn10(s: &str) -> bool {
    let digits: Vec<u32> = s
        .chars()
        .filter_map(|c| c.to_digit(10))
        .collect();
    if digits.len() != 10 {
        return false;
    }
    let sum: u32 = digits.iter().enumerate().map(|(i, &d)| d * (i as u32 + 1)).sum();
    sum % 11 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leap_year() {
        assert!(is_leap_year(2000));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2023));
    }

    #[test]
    fn test_days_in_month() {
        assert_eq!(days_in_month(1, 2024), 31);
        assert_eq!(days_in_month(2, 2024), 29);
        assert_eq!(days_in_month(2, 2023), 28);
        assert_eq!(days_in_month(4, 2024), 30);
    }

    #[test]
    fn test_isbn10() {
        assert!(is_valid_isbn10("0306406152"));
        assert!(!is_valid_isbn10("0000000000"));
    }
}
