pub fn validate(input: &str) -> bool {
    !input.is_empty() && input.len() <= 100
}
