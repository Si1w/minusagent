use std::sync::OnceLock;

use anyhow::Result;

use crate::config::tuning;
use crate::engine::node::Node;
use crate::engine::store::SharedStore;
use crate::tool::push_tool_result;

/// Shared HTTP client for web tools (avoids rebuilding connection pool per request)
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(tuning().web_timeout_secs))
            .build()
            .expect("failed to build HTTP client")
    })
}

/// Fetch a URL and return its content
pub struct WebFetch {
    pub call_id: String,
    pub url: String,
    pub max_length: Option<usize>,
}

impl Node for WebFetch {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<Self::PrepRes> {
        if self.url.is_empty() {
            anyhow::bail!("url is required");
        }
        Ok(self.url.clone())
    }

    async fn exec(&self, url: Self::PrepRes) -> Result<Self::ExecRes> {
        let resp = http_client().get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(format!("HTTP {status}"));
        }

        let is_html = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/html"));

        let body = resp.text().await?;
        let body = if is_html { html_to_markdown(&body) } else { body };

        let max = self.max_length.unwrap_or(tuning().web_fetch_max_body);
        if body.len() > max {
            Ok(format!(
                "{}...\n[truncated at {max} chars]",
                &body[..max]
            ))
        } else {
            Ok(body)
        }
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: Self::PrepRes,
        exec_res: Self::ExecRes,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

/// Search the web via DuckDuckGo HTML and return results
pub struct WebSearch {
    pub call_id: String,
    pub query: String,
}

impl Node for WebSearch {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<Self::PrepRes> {
        if self.query.is_empty() {
            anyhow::bail!("query is required");
        }
        Ok(self.query.clone())
    }

    async fn exec(&self, query: Self::PrepRes) -> Result<Self::ExecRes> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(&query)
        );

        let resp = http_client()
            .get(&url)
            .header("User-Agent", "minusagent/1.0")
            .send()
            .await?;

        let body = resp.text().await?;
        let results = parse_ddg_results(&body);

        if results.is_empty() {
            Ok(format!("No results found for: {query}"))
        } else {
            Ok(results)
        }
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: Self::PrepRes,
        exec_res: Self::ExecRes,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

/// Shared HTML-to-Markdown converter (avoids rebuilding per call)
fn md_converter() -> &'static htmd::HtmlToMarkdown {
    static CONVERTER: OnceLock<htmd::HtmlToMarkdown> = OnceLock::new();
    CONVERTER.get_or_init(|| {
        htmd::HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style", "nav", "footer", "header"])
            .build()
    })
}

/// Convert HTML to Markdown for LLM-friendly output
fn html_to_markdown(html: &str) -> String {
    match md_converter().convert(html) {
        Ok(md) => collapse_blank_lines(&md),
        Err(_) => strip_tags(html),
    }
}

/// Collapse 3+ consecutive blank lines into 2
fn collapse_blank_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut blank_count = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }
    let end = result.trim_end().len();
    result.truncate(end);
    result
}

/// Extract search results from DuckDuckGo HTML response
fn parse_ddg_results(html: &str) -> String {
    let mut results = Vec::new();
    let mut idx = 0;

    while let Some(start) = html[idx..].find("class=\"result__a\"") {
        let pos = idx + start;
        idx = pos + 1;

        let href = match extract_attr(&html[pos..], "href") {
            Some(h) => h,
            None => continue,
        };

        let title = match extract_tag_text(&html[pos..]) {
            Some(t) => strip_tags(&t),
            None => continue,
        };

        let snippet = html[pos..]
            .find("class=\"result__snippet\"")
            .and_then(|s| extract_tag_text(&html[pos + s..]))
            .map(|t| strip_tags(&t))
            .unwrap_or_default();

        results.push(format!(
            "{}. {}\n   {}\n   {}",
            results.len() + 1,
            title,
            href,
            snippet
        ));

        if results.len() >= 10 {
            break;
        }
    }

    results.join("\n\n")
}

/// Extract an HTML attribute value
fn extract_attr(html: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=\"");
    let start = html.find(&pattern)? + pattern.len();
    let end = html[start..].find('"')? + start;
    Some(html[start..end].to_string())
}

fn extract_tag_text(html: &str) -> Option<String> {
    let start = html.find('>')? + 1;
    let end = html[start..].find("</a>")? + start;
    Some(html[start..end].to_string())
}

/// Strip HTML tags from a string
fn strip_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_tags() {
        assert_eq!(strip_tags("<b>hello</b> world"), "hello world");
        assert_eq!(strip_tags("no tags"), "no tags");
    }

    #[test]
    fn test_extract_attr() {
        let html = r#"href="https://example.com" class="foo""#;
        assert_eq!(
            extract_attr(html, "href"),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_parse_ddg_results_empty() {
        assert_eq!(parse_ddg_results("<html></html>"), "");
    }

    #[test]
    fn test_html_to_markdown() {
        let html = r#"
        <html>
        <head><style>body{color:red}</style><script>alert(1)</script></head>
        <body>
            <nav><a href="/">Home</a></nav>
            <h1>Hello World</h1>
            <p>This is a <strong>test</strong> paragraph.</p>
            <ul><li>Item 1</li><li>Item 2</li></ul>
            <footer>Copyright 2025</footer>
        </body>
        </html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("# Hello World"), "should convert h1: {md}");
        assert!(md.contains("**test**"), "should convert bold: {md}");
        assert!(md.contains("Item 1"), "should keep list items: {md}");
        assert!(!md.contains("alert(1)"), "should skip script: {md}");
        assert!(!md.contains("color:red"), "should skip style: {md}");
    }

    #[test]
    fn test_collapse_blank_lines() {
        let input = "a\n\n\n\n\nb";
        let result = collapse_blank_lines(input);
        assert_eq!(result, "a\n\n\nb");
    }

    #[tokio::test]
    async fn test_web_fetch_html_to_markdown() {
        let resp = reqwest::Client::new()
            .get("https://example.com")
            .send()
            .await
            .expect("failed to fetch example.com");
        let is_html = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/html"));
        assert!(is_html, "example.com should return HTML");

        let body = resp.text().await.unwrap();
        let md = html_to_markdown(&body);
        assert!(md.contains("Example Domain"), "should contain title: {md}");
        assert!(!md.contains("<h1>"), "should not contain raw HTML tags: {md}");
    }
}
