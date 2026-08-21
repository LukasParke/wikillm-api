//! GitHub connector: issues + pulls + releases via REST, incremental by
//! `since` watermark, one markdown doc per item.

use crate::error::Result;
use serde_json::Value;

struct Item {
    number: i64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    created_at: String,
    updated_at: String,
    author: Option<String>,
    html_url: Option<String>,
    is_pull: bool,
    labels: Vec<String>,
}

async fn fetch_items(url: &str, token: Option<&str>) -> Result<Vec<Value>> {
    let mut req = reqwest::Client::new()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "wikillm-api")
        .timeout(std::time::Duration::from_secs(30));
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let response = req.send().await?;
    if !response.status().is_success() {
        return Ok(Vec::new());
    }
    Ok(response.json().await.unwrap_or_default())
}

pub async fn poll(config: &Value, state: &Value) -> Result<(Vec<(String, String, String, i64)>, Value)> {
    let repo = config
        .get("repo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| crate::error::Error::Validation("github connector requires config.repo".into()))?
        .to_string();
    let token = config.get("token").and_then(|v| v.as_str());
    let include: Vec<String> = config
        .get("include")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["issues".into(), "pulls".into(), "releases".into()]);

    let since = state.get("since").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut newest = since.clone();
    let mut docs = Vec::new();

    for kind in &include {
        match kind.as_str() {
            "releases" => {
                let items = fetch_items(
                    &format!("https://api.github.com/repos/{repo}/releases?per_page=30"),
                    token,
                )
                .await?;
                for item in items {
                    let created = item.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                    if !since.is_empty() && created <= since.as_str() {
                        continue;
                    }
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("tag_name").and_then(|v| v.as_str()))
                        .unwrap_or("release");
                    let tag = item.get("tag_name").and_then(|v| v.as_str()).unwrap_or(name);
                    let safe_tag: String = tag.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
                    docs.push((
                        format!("{}__releases__{}.md", repo.replace('/', "__"), safe_tag),
                        format!(
                            "---\ntype: Release\ntitle: {}\ncreated_at: \"{}\"\n---\n\n{}\n",
                            name,
                            created,
                            item.get("body").and_then(|v| v.as_str()).unwrap_or("")
                        ),
                        name.to_string(),
                        chrono::DateTime::parse_from_rfc3339(created)
                            .map(|t| t.timestamp_millis())
                            .unwrap_or(now_ms()),
                    ));
                    newest = max_iso(&newest, created);
                }
            }
            k => {
                let items = fetch_items(
                    &format!("https://api.github.com/repos/{repo}/issues?state=all&sort=created&direction=desc&per_page=50"),
                    token,
                )
                .await?;
                for item in items {
                    let created = item.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                    if !since.is_empty() && created <= since.as_str() {
                        continue;
                    }
                    let is_pull = item.get("pull_request").is_some();
                    let want_pulls = k == "pulls";
                    if is_pull != want_pulls {
                        continue;
                    }
                    let number = item.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
                    let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("#").to_string();
                    let labels: Vec<String> = item
                        .get("labels")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                        .unwrap_or_default();
                    let fm_labels = labels.iter().map(|l| format!("{l:?}")).collect::<Vec<_>>().join(", ");
                    docs.push((
                        format!("{}/{}_{}.md", repo.replace('/', "__"), if is_pull { "pulls" } else { "issues" }, number),
                        format!(
                            "---\ntype: {}\ntitle: {:?}\ntags: [{}]\nstate: {}\nauthor: {}\ncreated_at: \"{}\"\n---\n\n# {}\n\n{}\n",
                            if is_pull { "Pull Request" } else { "Issue" },
                            title,
                            fm_labels,
                            item.get("state").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            item.pointer("/user/login").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            created,
                            title,
                            item.get("body").and_then(|v| v.as_str()).unwrap_or("")
                        ),
                        title,
                        chrono::DateTime::parse_from_rfc3339(
                            item.get("updated_at").and_then(|v| v.as_str()).unwrap_or(created),
                        )
                        .map(|t| t.timestamp_millis())
                        .unwrap_or(now_ms()),
                    ));
                    newest = max_iso(&newest, created);
                }
            }
        }
    }

    Ok((docs, serde_json::json!({ "since": newest })))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn max_iso(a: &str, b: &str) -> String {
    if b > a { b.to_string() } else { a.to_string() }
}
