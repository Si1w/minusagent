/// Split text into chunks at newline boundaries
///
/// # Arguments
///
/// * `text` - The text to split
/// * `max_len` - Maximum byte length per chunk
#[must_use]
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
        // Find a char-safe boundary at or before end
        let safe_end = if text.is_char_boundary(end) {
            end
        } else {
            text[start..end]
                .char_indices()
                .map(|(i, _)| start + i)
                .next_back()
                .unwrap_or(start)
        };
        if safe_end <= start {
            // Single char wider than max_len — take one char to avoid infinite loop
            let next = start + text[start..].chars().next().map_or(1, char::len_utf8);
            chunks.push(&text[start..next]);
            start = next;
            continue;
        }
        let cut = text[start..safe_end]
            .rfind('\n')
            .map_or(safe_end, |i| start + i + 1);
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
