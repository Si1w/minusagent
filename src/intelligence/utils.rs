use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// A file discovered by directory scanning, with parsed frontmatter
pub struct DiscoveredFile {
    /// Directory name (for subdirs) or filename stem (for files)
    pub name: String,
    /// Parsed frontmatter key-value pairs
    pub meta: HashMap<String, String>,
    /// Raw file content
    pub content: String,
    /// Absolute path to the file
    pub path: PathBuf,
}

/// Scan a directory for files with the given extension, sorted by name
///
/// Each file is read and its frontmatter parsed. Files that cannot be
/// read are silently skipped.
pub fn discover_files(dir: &Path, ext: &str) -> Vec<DiscoveredFile> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == ext))
        .collect();
    files.sort_by_key(|e| e.file_name());

    files
        .into_iter()
        .filter_map(|f| {
            let content = std::fs::read_to_string(f.path()).ok()?;
            let meta = parse_frontmatter(&content);
            let name = f.path().file_stem()?.to_string_lossy().to_string();
            Some(DiscoveredFile { name, meta, content, path: f.path() })
        })
        .collect()
}

/// Scan subdirectories for a specific file, sorted by directory name
///
/// Each subdirectory is checked for `filename`. If found, the file is read
/// and its frontmatter parsed. Directories without the file are skipped.
pub fn discover_subdirs(base: &Path, filename: &str) -> Vec<DiscoveredFile> {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    dirs.into_iter()
        .filter_map(|d| {
            let file_path = d.path().join(filename);
            let content = std::fs::read_to_string(&file_path).ok()?;
            let meta = parse_frontmatter(&content);
            let name = d.file_name().to_string_lossy().to_string();
            Some(DiscoveredFile { name, meta, content, path: file_path })
        })
        .collect()
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