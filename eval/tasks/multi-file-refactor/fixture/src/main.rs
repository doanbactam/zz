fn main() {
    let input = "hello";
    // Inline validation
    let is_valid = !input.is_empty() && input.len() <= 100;
    if is_valid {
        println!("Valid input: {}", input);
    } else {
        println!("Invalid input");
    }
}
