//! MCP (Model Context Protocol) read-only service: JSON-RPC 2.0 envelope
//!
//! Exposes gateway capabilities as MCP tools to external agents (Claude Code / Cursor etc.),
//! currently only 3 read-only tools, zero write operations:
//! - `list_models`   list of available model IDs
//! - `list_accounts` account health status snapshot
//! - `usage_summary` cumulative usage stats
//!
//! This module does not depend on `AppState` (avoids circular dependency); it works off a
//! [`GatewaySnapshot`] data snapshot injected by the caller. The HTTP wiring is responsible for
//! assembling the snapshot from AppState and calling [`handle_json`] in the POST route.
//!
//! Protocol notes (MCP 2024-11-05):
//! - Request: `{"jsonrpc":"2.0","id":N,"method":"...","params":{...}}`
//! - Success: `{"jsonrpc":"2.0","id":N,"result":{...}}`
//! - Failure: `{"jsonrpc":"2.0","id":N,"error":{"code":N,"message":"..."}}`
//! - No `id` field = notification, no reply (returns `None`)
//! - `tools/call` tool-level failures are expressed via an `isError:true` tool result, not a protocol error

use serde_json::{json, Value};

/// Supported MCP protocol version
pub const PROTOCOL_VERSION: &str = "2024-11-05";
/// serverInfo.name
pub const SERVER_NAME: &str = "freebuff2api";

const ERR_PARSE: i64 = -32700;
const ERR_INVALID_REQUEST: i64 = -32600;
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;

/// Data snapshot provided by the caller at tool execution time (the wiring assembles this from AppState)
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewaySnapshot {
    /// List of available model IDs
    pub models: Vec<String>,
    /// Account health summary
    pub accounts: Vec<AccountBrief>,
    /// Usage totals (passed through as-is from `usage.totals()`)
    pub usage_totals: Value,
    /// Gateway version (recommend `env!("CARGO_PKG_VERSION")`)
    pub version: String,
    /// Process uptime in seconds
    pub uptime_sec: u64,
}

/// Account health summary (mapped from `pool::AccountSnapshot`)
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountBrief {
    pub name: String,
    pub healthy: bool,
    pub score: f64,
    /// Session status (`session::SessionSnapshot::status`, "unknown" when missing)
    pub session_status: String,
}

/// MCP tool definitions (the result of `tools/list`)
pub fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "list_models",
                "description": "List the model IDs currently available on the gateway (read-only)",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "list_accounts",
                "description": "List upstream account health status: name, availability, score, session status (read-only)",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "usage_summary",
                "description": "Return the gateway's cumulative usage stats (request count, tokens, etc., read-only)",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            }
        ]
    })
}

/// Handle one JSON-RPC request, returning a JSON-RPC response (`None` = notification, no reply needed)
///
/// Supported methods: `initialize` / `tools/list` / `tools/call` / `ping`;
/// unknown methods return `-32601`; an invalid envelope returns `-32600`; missing `tools/call` params returns `-32602`.
pub fn handle_request(req: &Value, snapshot: &GatewaySnapshot) -> Option<Value> {
    let Some(obj) = req.as_object() else {
        return Some(error_response(
            &Value::Null,
            ERR_INVALID_REQUEST,
            "Invalid Request: request must be a JSON object",
        ));
    };

    // No id field = notification, no reply per JSON-RPC 2.0
    if !obj.contains_key("id") {
        return None;
    }
    let id = obj.get("id").cloned().unwrap_or(Value::Null);

    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error_response(
            &id,
            ERR_INVALID_REQUEST,
            "Invalid Request: jsonrpc must be \"2.0\"",
        ));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            &id,
            ERR_INVALID_REQUEST,
            "Invalid Request: method must be a string",
        ));
    };

    let outcome = match method {
        "initialize" => Ok(initialize_result(snapshot)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_definitions()),
        "tools/call" => tools_call(obj.get("params"), snapshot),
        _ => Err((ERR_METHOD_NOT_FOUND, format!("Method not found: {method}"))),
    };

    Some(match outcome {
        Ok(result) => success_response(&id, result),
        Err((code, message)) => error_response(&id, code, &message),
    })
}

/// Convenience entry point: handles a raw JSON string; returns `-32700` on parse failure (id is null)
pub fn handle_json(raw: &str, snapshot: &GatewaySnapshot) -> Option<String> {
    match serde_json::from_str::<Value>(raw) {
        Ok(req) => handle_request(&req, snapshot).map(|resp| resp.to_string()),
        Err(_) => {
            Some(error_response(&Value::Null, ERR_PARSE, "Parse error: invalid JSON").to_string())
        }
    }
}

/// `initialize` result: protocol version + capabilities + server info
fn initialize_result(snapshot: &GatewaySnapshot) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": snapshot.version }
    })
}

/// Dispatch `tools/call`; only param envelope errors go through the protocol error path, unknown tools go through the isError tool result
fn tools_call(params: Option<&Value>, snapshot: &GatewaySnapshot) -> Result<Value, (i64, String)> {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .ok_or((
            ERR_INVALID_PARAMS,
            "Invalid params: tools/call requires a string field \"name\"".to_string(),
        ))?;

    match name {
        "list_models" => Ok(tool_text(to_json(&snapshot.models), false)),
        "list_accounts" => Ok(tool_text(to_json(&snapshot.accounts), false)),
        "usage_summary" => Ok(tool_text(snapshot.usage_totals.clone(), false)),
        other => Ok(tool_text(
            json!({ "error": format!("Unknown tool: {other}") }),
            true,
        )),
    }
}

/// Build an MCP tool result; text is always a pretty JSON string
fn tool_text(payload: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}"));
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

/// Serialize to a JSON value (these types cannot fail; degrades to an error object instead of panicking)
fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

/// Success response envelope
fn success_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Error response envelope
fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> GatewaySnapshot {
        GatewaySnapshot {
            models: vec!["claude-sonnet-5".into(), "gpt-5".into()],
            accounts: vec![
                AccountBrief {
                    name: "token-1".into(),
                    healthy: true,
                    score: 120.5,
                    session_status: "active".into(),
                },
                AccountBrief {
                    name: "token-2".into(),
                    healthy: false,
                    score: -999.0,
                    session_status: "cooldown".into(),
                },
            ],
            usage_totals: json!({ "requests": 42, "prompt_tokens": 1000 }),
            version: "0.3.0".into(),
            uptime_sec: 3600,
        }
    }

    fn call(method: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method })
    }

    fn call_tool(name: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": {} }
        })
    }

    #[test]
    fn initialize_returns_protocol_shape() {
        let resp = handle_request(&call("initialize"), &snapshot()).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(resp["result"]["serverInfo"]["version"], "0.3.0");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert!(resp.get("error").is_none());
    }

    #[test]
    fn tools_list_returns_three_tools_with_valid_schema() {
        let resp = handle_request(&call("tools/list"), &snapshot()).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["list_models", "list_accounts", "usage_summary"]);
        for t in tools {
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["inputSchema"]["properties"].is_object());
            assert!(t["inputSchema"]["required"].is_array());
        }
    }

    #[test]
    fn tools_call_list_models_returns_catalog() {
        let resp = handle_request(&call_tool("list_models"), &snapshot()).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let models: Vec<String> = serde_json::from_str(text).unwrap();
        assert_eq!(models, vec!["claude-sonnet-5", "gpt-5"]);
        assert_eq!(resp["result"]["content"][0]["type"], "text");
    }

    #[test]
    fn tools_call_list_accounts_returns_health() {
        let resp = handle_request(&call_tool("list_accounts"), &snapshot()).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let accounts: Value = serde_json::from_str(text).unwrap();
        assert_eq!(accounts[0]["name"], "token-1");
        assert_eq!(accounts[0]["healthy"], true);
        assert_eq!(accounts[1]["session_status"], "cooldown");
    }

    #[test]
    fn tools_call_usage_summary_returns_totals() {
        let resp = handle_request(&call_tool("usage_summary"), &snapshot()).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let totals: Value = serde_json::from_str(text).unwrap();
        assert_eq!(totals["requests"], 42);
        assert_eq!(totals["prompt_tokens"], 1000);
    }

    #[test]
    fn tools_call_unknown_tool_is_tool_error_not_protocol_error() {
        let resp = handle_request(&call_tool("delete_everything"), &snapshot()).unwrap();
        assert!(resp.get("error").is_none());
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Unknown tool"));
    }

    #[test]
    fn tools_call_without_name_returns_invalid_params() {
        let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {} });
        let resp = handle_request(&req, &snapshot()).unwrap();
        assert_eq!(resp["error"]["code"], ERR_INVALID_PARAMS);
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn notification_without_id_returns_none() {
        let req = json!({ "jsonrpc": "2.0", "method": "tools/list" });
        assert!(handle_request(&req, &snapshot()).is_none());
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_request(&note, &snapshot()).is_none());
    }

    #[test]
    fn ping_returns_empty_result() {
        let resp = handle_request(&call("ping"), &snapshot()).unwrap();
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let resp = handle_request(&call("tools/delete"), &snapshot()).unwrap();
        assert_eq!(resp["error"]["code"], ERR_METHOD_NOT_FOUND);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tools/delete"));
    }

    #[test]
    fn invalid_jsonrpc_version_returns_invalid_request() {
        let req = json!({ "jsonrpc": "1.0", "id": 3, "method": "ping" });
        let resp = handle_request(&req, &snapshot()).unwrap();
        assert_eq!(resp["error"]["code"], ERR_INVALID_REQUEST);
        assert_eq!(resp["id"], 3);
    }

    #[test]
    fn non_object_request_returns_invalid_request() {
        let resp = handle_request(&json!([1, 2, 3]), &snapshot()).unwrap();
        assert_eq!(resp["error"]["code"], ERR_INVALID_REQUEST);
        assert_eq!(resp["id"], Value::Null);
    }

    #[test]
    fn id_is_echoed_for_number_string_and_null() {
        for id in [json!(123), json!("abc"), json!(null)] {
            let req = json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
            let resp = handle_request(&req, &snapshot()).unwrap();
            assert_eq!(resp["id"], id);
        }
    }

    #[test]
    fn handle_json_parse_error_returns_32700() {
        let out = handle_json("{ not json", &snapshot()).unwrap();
        let resp: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(resp["error"]["code"], ERR_PARSE);
        assert_eq!(resp["id"], Value::Null);
    }

    #[test]
    fn handle_json_end_to_end_string() {
        let out = handle_json(
            r#"{"jsonrpc":"2.0","id":"req-9","method":"tools/call","params":{"name":"list_models"}}"#,
            &snapshot(),
        )
        .unwrap();
        let resp: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(resp["id"], "req-9");
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains('\n'), "text should be pretty JSON");
        assert!(text.contains("claude-sonnet-5"));
    }

    #[test]
    fn handle_json_notification_returns_none() {
        let out = handle_json(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &snapshot(),
        );
        assert!(out.is_none());
    }
}
