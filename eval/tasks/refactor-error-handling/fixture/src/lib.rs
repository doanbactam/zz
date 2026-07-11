use std::fs;

/// Read and parse a config file. Returns lines as key=value pairs.
/// BUG: panics on missing file, panics on bad lines, panics on empty values.
pub fn parse_config(path: &str) -> Vec<(String, String)> {
    let content = fs::read_to_string(path).unwrap();
    let mut result = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        let key = parts[0].trim().to_string();
        let value = parts[1].trim().to_string();
        result.push((key, value));
    }
    result
}

/// Read a file and return its content as a string.
/// BUG: panics on missing file.
pub fn read_file(path: &str) -> String {
    fs::read_to_string(path).unwrap()
}

/// Parse a number from string. BUG: panics on invalid input.
pub fn parse_number(s: &str) -> i64 {
    s.parse().unwrap()
}

/// Divide two numbers. BUG: panics on division by zero.
pub fn safe_divide(a: i64, b: i64) -> i64 {
    a / b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_temp_file(name: &str, content: &str) -> String {
        let path = format!("/tmp/{name}");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_parse_config() {
        let path = create_temp_file("test_config.txt", "key1 = value1\nkey2 = value2\n");
        let config = parse_config(&path);
        assert_eq!(config.len(), 2);
        assert_eq!(config[0].0, "key1");
        assert_eq!(config[0].1, "value1");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_config_with_comments() {
        let path = create_temp_file("test_config_comments.txt", "# comment\nkey = value\n");
        let config = parse_config(&path);
        assert_eq!(config.len(), 1);
        assert_eq!(config[0].0, "key");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_file() {
        let path = create_temp_file("test_read.txt", "hello world");
        let content = read_file(&path);
        assert_eq!(content, "hello world");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(parse_number("42"), 42);
        assert_eq!(parse_number("-7"), -7);
    }

    #[test]
    fn test_safe_divide() {
        assert_eq!(safe_divide(10, 2), 5);
        assert_eq!(safe_divide(7, 3), 2);
    }
}
