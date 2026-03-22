/// Split text into chunks at newline boundaries
///
/// # Arguments
///
/// * `text` - The text to split
/// * `max_len` - Maximum byte length per chunk
pub fn chunk_text(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = std::cmp::min(start + max_len, text.len());
        if end == text.len() {
            chunks.push(&text[start..]);
            break;
        }
        let cut = text[start..end]
            .rfind('\n')
            .map(|i| start + i + 1)
            .unwrap_or(end);
        chunks.push(&text[start..cut]);
        start = cut;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_text_short() {
        let result = chunk_text("hello", 100);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn test_chunk_text_split_at_newline() {
        let text = "line1\nline2\nline3";
        let result = chunk_text(text, 10);
        assert_eq!(result, vec!["line1\n", "line2\n", "line3"]);
    }
}
