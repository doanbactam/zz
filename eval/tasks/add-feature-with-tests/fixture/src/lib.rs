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
