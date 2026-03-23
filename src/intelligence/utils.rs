use std::collections::HashMap;

/// Parse simple YAML frontmatter delimited by `---`
///
/// # Returns
///
/// Key-value pairs from the frontmatter block. Empty map if no valid
/// frontmatter is found.
pub fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut meta = HashMap::new();
    if !text.starts_with("---") {
        return meta;
    }
    let parts: Vec<&str> = text.splitn(3, "---").collect();
    if parts.len() < 3 {
        return meta;
    }
    for line in parts[1].trim().lines() {
        if let Some((key, value)) = line.split_once(':') {
            meta.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    meta
}

/// Extract body content after frontmatter
///
/// Returns the text after the closing `---`, or the original text
/// if no frontmatter is present.
pub fn extract_body(text: &str) -> String {
    if !text.starts_with("---") {
        return text.to_string();
    }
    text.splitn(3, "---")
        .nth(2)
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let text = "---\nname: test\ndescription: a test\n---\nBody.";
        let meta = parse_frontmatter(text);
        assert_eq!(meta.get("name").unwrap(), "test");
        assert_eq!(meta.get("description").unwrap(), "a test");
    }

    #[test]
    fn test_parse_frontmatter_none() {
        assert!(parse_frontmatter("Just text.").is_empty());
        assert!(parse_frontmatter("---\nno closing").is_empty());
    }

    #[test]
    fn test_extract_body() {
        assert_eq!(
            extract_body("---\nk: v\n---\nBody here."),
            "Body here."
        );
    }

    #[test]
    fn test_extract_body_no_frontmatter() {
        assert_eq!(extract_body("Plain text."), "Plain text.");
    }
}