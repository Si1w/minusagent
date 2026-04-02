use std::sync::OnceLock;

use anyhow::Result;

use crate::config::tuning;
use crate::core::node::Node;
use crate::core::store::SharedStore;
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

        let body = resp.text().await?;
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
}
