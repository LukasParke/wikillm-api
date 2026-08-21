//! Web connector: fetch configured URLs, extract readable text, skip
//! unchanged pages via If-None-Match.

use crate::error::{Error, Result};
use serde_json::Value;

fn strip_html(html: &str) -> (String, String) {
    // minimal extraction: drop script/style/noscript blocks and tags
    let mut cleaned = html.to_string();
    for tag in ["script", "style", "noscript"] {
        while let Some(start) = cleaned.find(&format!("<{tag}")) {
            if let Some(end_rel) = cleaned[start..].find(&format!("</{tag}>")) {
                let end = start + end_rel + format!("</{tag}>").len();
                cleaned.replace_range(start..end, "");
            } else {
                break;
            }
        }
    }
    // title from <title>
    let title = cleaned
        .split("<title>")
        .nth(1)
        .and_then(|rest| rest.split("</title>").next())
        .unwrap_or("untitled")
        .trim()
        .to_string();
    // strip remaining tags
    let mut text = String::new();
    let mut depth = 0usize;
    for ch in cleaned.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 => text.push(c),
            _ => {}
        }
    }
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (title, compact)
}

pub fn url_to_path(url: &str) -> String {
    let parsed = url.split("://").nth(1).unwrap_or(url);
    let sanitized: String = parsed
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') { c } else { '_' })
        .collect();
    if sanitized.ends_with(".md") {
        sanitized
    } else {
        format!("{sanitized}.md")
    }
}

/// Returns docs as (path, title, content, mtime=now).
pub async fn poll(config: &Value, state: &Value) -> Result<(Vec<(String, String, String, i64)>, Value)> {
    let urls: Vec<String> = config
        .get("urls")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .ok_or_else(|| Error::Validation("web connector requires config.urls".into()))?;
    let etags = state.get("etags").cloned().unwrap_or(Value::Null);
    let mut new_etags = serde_json::Map::new();
    if let Some(obj) = etags.as_object() {
        new_etags.extend(obj.clone());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut docs = Vec::new();
    for url in &urls {
        let mut request = client.get(url);
        if let Some(etag) = new_etags.get(url).and_then(|v| v.as_str()) {
            request = request.header("If-None-Match", etag);
        }
        match request.send().await {
            Ok(response) if response.status().as_u16() == 304 => continue,
            Ok(response) if !response.status().is_success() => continue,
            Ok(response) => {
                if let Some(etag) = response.headers().get("etag").and_then(|v| v.to_str().ok()) {
                    new_etags.insert(url.clone(), Value::String(etag.to_string()));
                }
                let html = response.text().await.unwrap_or_default();
                let (title, text) = strip_html(&html);
                docs.push((
                    url_to_path(url),
                    format!("# {title}\n\nSource: {url}\n\n{text}\n"),
                    title,
                    now,
                ));
            }
            Err(_) => continue,
        }
    }
    Ok((docs, serde_json::json!({ "etags": new_etags })))
}
