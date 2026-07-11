mod validator;

fn main() {
    let input = "hello";
    if validator::validate(input) {
        println!("Valid input: {}", input);
    } else {
        println!("Invalid input");
    }
}
