use std::fs;

/// Read and parse a config file. Returns lines as key=value pairs.
/// Returns Err if file not found or a line has no '=' delimiter.
pub fn parse_config(path: &str) -> Result<Vec<(String, String)>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {path}: {e}"))?;
    let mut result = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() < 2 {
            return Err(format!("line {}: missing '=' delimiter", i + 1));
        }
        let key = parts[0].trim().to_string();
        let value = parts[1].trim().to_string();
        if key.is_empty() {
            return Err(format!("line {}: empty key", i + 1));
        }
        result.push((key, value));
    }
    Ok(result)
}

/// Read a file and return its content as a string.
pub fn read_file(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))
}

/// Parse a number from string.
pub fn parse_number(s: &str) -> Result<i64, String> {
    s.parse().map_err(|_| format!("invalid number: '{s}'"))
}

/// Divide two numbers. Returns Err on division by zero.
pub fn safe_divide(a: i64, b: i64) -> Result<i64, String> {
    if b == 0 {
        Err("division by zero".to_string())
    } else {
        Ok(a / b)
    }
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
        let path = create_temp_file("test_config2.txt", "key1 = value1\nkey2 = value2\n");
        let config = parse_config(&path).unwrap();
        assert_eq!(config.len(), 2);
        assert_eq!(config[0].0, "key1");
        assert_eq!(config[0].1, "value1");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_config_with_comments() {
        let path = create_temp_file("test_config_comments2.txt", "# comment\nkey = value\n");
        let config = parse_config(&path).unwrap();
        assert_eq!(config.len(), 1);
        assert_eq!(config[0].0, "key");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_config_bad_line() {
        let path = create_temp_file("test_config_bad.txt", "no-equal-sign\n");
        let result = parse_config(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing '='"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_parse_config_missing_file() {
        let result = parse_config("/nonexistent/file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file() {
        let path = create_temp_file("test_read2.txt", "hello world");
        let content = read_file(&path).unwrap();
        assert_eq!(content, "hello world");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_file_missing() {
        let result = read_file("/nonexistent/file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(parse_number("42").unwrap(), 42);
        assert_eq!(parse_number("-7").unwrap(), -7);
        assert!(parse_number("abc").is_err());
    }

    #[test]
    fn test_safe_divide() {
        assert_eq!(safe_divide(10, 2).unwrap(), 5);
        assert_eq!(safe_divide(7, 3).unwrap(), 2);
        assert!(safe_divide(10, 0).is_err());
    }
}
