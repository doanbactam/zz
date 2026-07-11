/// Count vowels in a string. Returns 0 for empty input.
pub fn count_vowels(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut count = 0;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'a' | b'e' | b'i' | b'o' | b'u'
            | b'A' | b'E' | b'I' | b'O' | b'U' => count += 1,
            _ => {}
        }
    }
    count
}

/// Find the index of the first occurrence of a substring.
/// Returns None if not found.
pub fn find_substring(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    if n_bytes.len() > h_bytes.len() {
        return None;
    }
    for i in 0..=h_bytes.len() - n_bytes.len() {
        if &h_bytes[i..i + n_bytes.len()] == n_bytes {
            return Some(i);
        }
    }
    None
}

/// Compute factorial of n. Returns 1 for n=0.
pub fn factorial(n: u64) -> u64 {
    if n <= 1 {
        return 1;
    }
    let mut result = 1;
    for i in 2..=n {
        result *= i;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_vowels_basic() {
        assert_eq!(count_vowels("hello"), 2);
        assert_eq!(count_vowels("aeiou"), 5);
        assert_eq!(count_vowels("xyz"), 0);
    }

    #[test]
    fn test_find_substring_basic() {
        assert_eq!(find_substring("hello world", "world"), Some(6));
        assert_eq!(find_substring("hello", "xyz"), None);
        assert_eq!(find_substring("hello", ""), Some(0));
    }

    #[test]
    fn test_factorial_basic() {
        assert_eq!(factorial(0), 1);
        assert_eq!(factorial(1), 1);
        assert_eq!(factorial(5), 120);
    }
}
