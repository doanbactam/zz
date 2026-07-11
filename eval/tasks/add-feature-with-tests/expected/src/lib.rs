/// Reverse a string.
pub fn reverse(s: &str) -> String {
    s.chars().rev().collect()
}

/// Check if a string is a palindrome.
pub fn is_palindrome(s: &str) -> bool {
    let lower: String = s.chars().map(|c| c.to_ascii_lowercase()).collect();
    lower == reverse(&lower)
}

/// Count words in a string (whitespace-separated).
pub fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Capitalize the first letter of a string. Empty string returns empty.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let upper: String = first.to_uppercase().collect();
            upper + chars.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reverse() {
        assert_eq!(reverse("hello"), "olleh");
        assert_eq!(reverse(""), "");
    }

    #[test]
    fn test_is_palindrome() {
        assert!(is_palindrome("racecar"));
        assert!(!is_palindrome("hello"));
    }

    #[test]
    fn test_word_count() {
        assert_eq!(word_count("hello world"), 2);
        assert_eq!(word_count(""), 0);
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("hello"), "Hello");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("a"), "A");
        assert_eq!(capitalize("already Capital"), "Already Capital");
    }
}
