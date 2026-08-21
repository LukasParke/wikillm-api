//! MCP stdio server: JSON-RPC 2.0 over line-delimited stdin/stdout.

pub mod tools;

use serde_json::{json, Value};

use crate::error::Result;
use tools::{call_tool, tools as tool_registry, HttpClient};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the stdio MCP server until stdin is exhausted.
pub async fn run() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let client = HttpClient::from_env();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(&client, &line).await;
        let Some(response) = response else { continue };
        let mut payload = serde_json::to_string(&response)?;
        payload.push('\n');
        stdout.write_all(payload.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Handle one incoming JSON-RPC frame. Returns `None` for notifications
/// (no reply expected).
async fn handle_line(client: &HttpClient, line: &str) -> Option<Value> {
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return Some(error_response(Value::Null, -32700, "Parse error"));
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    // Notifications (requests without an id) never get a reply.
    if req.get("id").is_none() {
        return None;
    }
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return Some(error_response(id, -32600, "Invalid Request"));
    };
    match method {
        "initialize" => Some(ok_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "wikillm",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )),
        "ping" => Some(ok_response(id, json!({}))),
        "tools/list" => {
            let defs: Vec<Value> = tool_registry()
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            Some(ok_response(id, json!({ "tools": defs })))
        }
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let output = call_tool(client, &name, args).await;
            let mut result = json!({
                "content": [{ "type": "text", "text": output.text }],
            });
            if output.is_error {
                result["isError"] = Value::Bool(true);
            }
            Some(ok_response(id, result))
        }
        _ => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> HttpClient {
        HttpClient::from_env()
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_server_info() {
        let res = handle_line(&client(), r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .await
            .expect("reply expected");
        assert_eq!(res["jsonrpc"], "2.0");
        assert_eq!(res["id"], 1);
        assert_eq!(res["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(res["result"]["serverInfo"]["name"], "wikillm");
        assert!(res["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn tools_list_exposes_all_41_tools() {
        let res = handle_line(&client(), r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .expect("reply expected");
        let tools = res["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 41);
        for t in tools {
            assert!(!t["name"].as_str().unwrap_or_default().is_empty());
            assert!(t["inputSchema"].is_object());
        }
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let res = handle_line(&client(), r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#)
            .await
            .expect("reply expected");
        assert_eq!(res["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn notifications_get_no_reply() {
        let res = handle_line(
            &client(),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn malformed_json_is_parse_error() {
        let res = handle_line(&client(), "not json").await.expect("reply expected");
        assert_eq!(res["error"]["code"], -32700);
    }

    #[test]
    fn registry_names_match_ts_reference() {
        let names: Vec<&str> = tool_registry().into_iter().map(|t| t.name).collect();
        assert_eq!(names.len(), 41);
        for expected in [
            "search",
            "get_concept",
            "read_source",
            "list_changes",
            "graph_neighbors",
            "propose_edit",
            "append_log",
            "query",
            "refresh_index",
            "settings_list",
            "settings_get",
            "settings_set",
            "settings_reset",
            "keys_list",
            "key_create",
            "key_delete",
            "projects_list",
            "project_put",
            "project_delete",
            "connectors_list",
            "connector_create",
            "connector_delete",
            "connector_run",
            "admin_reindex",
            "admin_stats",
            "okf_validate",
            "delete_page",
            "put_source",
            "add_feedback",
            "documents_list",
            "download_document",
            "pages_batch",
            "documents_delete",
            "export_bundle",
            "graph_export",
            "webhooks_list",
            "webhook_create",
            "webhook_delete",
            "get_page_raw",
            "read_source_content",
            "okf_layout",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool in registry: {expected}"
            );
        }
    }
}
