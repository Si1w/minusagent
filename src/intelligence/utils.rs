use std::collections::HashMap;
use std::fs::DirEntry;
use std::path::{Path, PathBuf};

/// Parse simple YAML frontmatter delimited by `---`
///
/// # Returns
///
/// Key-value pairs from the frontmatter block. Empty map if no valid
/// frontmatter is found.
#[must_use]
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
#[must_use]
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
#[must_use]
pub fn discover_files(dir: &Path, ext: &str) -> Vec<DiscoveredFile> {
    let mut files = read_dir_sorted(dir);
    files.retain(|entry| entry.path().extension().is_some_and(|value| value == ext));

    files
        .into_iter()
        .map(|entry| entry.path())
        .filter_map(read_discovered_file)
        .collect()
}

/// Scan subdirectories for a specific file, sorted by directory name
///
/// Each subdirectory is checked for `filename`. If found, the file is read
/// and its frontmatter parsed. Directories without the file are skipped.
#[must_use]
pub fn discover_subdirs(base: &Path, filename: &str) -> Vec<DiscoveredFile> {
    let mut dirs = read_dir_sorted(base);
    dirs.retain(|entry| entry.path().is_dir());

    dirs.into_iter()
        .filter_map(|entry| {
            let file_path = entry.path().join(filename);
            read_discovered_file_at(entry.file_name().to_string_lossy().to_string(), file_path)
        })
        .collect()
}

fn read_dir_sorted(dir: &Path) -> Vec<DirEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(DirEntry::file_name);
    entries
}

fn read_discovered_file(path: PathBuf) -> Option<DiscoveredFile> {
    let name = path.file_stem()?.to_string_lossy().to_string();
    read_discovered_file_at(name, path)
}

fn read_discovered_file_at(name: String, path: PathBuf) -> Option<DiscoveredFile> {
    let content = std::fs::read_to_string(&path).ok()?;
    Some(DiscoveredFile {
        name,
        meta: parse_frontmatter(&content),
        content,
        path,
    })
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
        assert_eq!(extract_body("---\nk: v\n---\nBody here."), "Body here.");
    }

    #[test]
    fn test_extract_body_no_frontmatter() {
        assert_eq!(extract_body("Plain text."), "Plain text.");
    }
}
