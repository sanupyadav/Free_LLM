//! Upstream OpenAI-compatible `chat.completions` streaming chunk → [`CanonicalEvent`].
//!
//! Compatibility notes:
//! - `delta.content` → [`CanonicalEvent::TextDelta`]
//! - `delta.tool_calls[]` fragments: first one (carrying id/name) → `ToolUseStart`,
//!   `function.arguments` fragments → `ToolUseInputDelta` (matched by `index`)
//! - `finish_reason`: `stop`→`end_turn`, `length`→`max_tokens`, `tool_calls`→`tool_use`
//! - `usage` (only present on the final chunk when `stream_options.include_usage` is set) → `Usage`
//! - `data: [DONE]` → `MessageStop`; falls back to `end_turn` when upstream didn't send `finish_reason`

use std::collections::HashMap;

use super::stream::{CanonicalEvent, SseLineParser};

/// Accumulated state for a single upstream tool call (id/name may arrive in fragments).
#[derive(Debug, Default, Clone)]
struct ToolCallState {
    id: String,
    name: String,
    started: bool,
}

/// OpenAI-compatible SSE decoder.
#[derive(Debug, Default)]
pub struct OpenAiSseDecoder {
    parser: SseLineParser,
    message_id: String,
    model: String,
    started: bool,
    stopped: bool,
    /// Upstream's finish_reason (already mapped to an Anthropic value)
    finish_reason: Option<String>,
    /// Upstream tool call index → accumulated state
    tools: HashMap<u64, ToolCallState>,
}

impl OpenAiSseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed upstream SSE bytes in, emit 0..n canonical events out.
    ///
    /// On `data: [DONE]`, emits [`CanonicalEvent::MessageStop`]
    /// (`stop_reason = "end_turn"` if upstream didn't give a `finish_reason`).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<CanonicalEvent> {
        let mut out = Vec::new();
        for (_event, data) in self.parser.feed(chunk) {
            self.handle_payload(data.trim(), &mut out);
        }
        out
    }

    /// Fallback for when the stream drops without upstream sending `[DONE]`: emits a `MessageStop`.
    ///
    /// Returns empty if already stopped; also emits a preceding `MessageStart` if no chunk was ever received.
    pub fn finish(&mut self) -> Vec<CanonicalEvent> {
        let mut out = Vec::new();
        self.emit_stop(&mut out);
        out
    }

    /// Handle a single SSE data payload.
    fn handle_payload(&mut self, data: &str, out: &mut Vec<CanonicalEvent>) {
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.emit_stop(out);
            return;
        }
        if self.stopped {
            return;
        }
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => {
                out.push(CanonicalEvent::Error(format!(
                    "invalid upstream SSE payload: {}",
                    truncate(data, 200)
                )));
                return;
            }
        };
        if let Some(err) = value.get("error") {
            out.push(CanonicalEvent::Error(extract_error_message(err)));
            return;
        }
        self.absorb_identity(&value);
        // Handle usage before choices: if this chunk carries both, message_start can use input_tokens
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            let input = json_u64(usage.get("prompt_tokens"));
            let output = json_u64(usage.get("completion_tokens"));
            self.ensure_started(out, input);
            out.push(CanonicalEvent::Usage {
                input_tokens: input,
                output_tokens: output,
            });
        }
        self.handle_choices(&value, out);
        // Emit MessageStart on the first valid chat chunk even without content yet, so downstream gets the message header early
        if !self.started && (value.get("choices").is_some() || !self.message_id.is_empty()) {
            self.ensure_started(out, 0);
        }
    }

    /// Record upstream message id / model (used later by MessageStart).
    fn absorb_identity(&mut self, value: &serde_json::Value) {
        if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                self.message_id = id.to_string();
            }
        }
        if let Some(model) = value.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.model = model.to_string();
            }
        }
    }

    /// Parse `choices[0]`'s delta and finish_reason.
    fn handle_choices(&mut self, value: &serde_json::Value, out: &mut Vec<CanonicalEvent>) {
        let Some(choice) = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
            return;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    self.ensure_started(out, 0);
                    out.push(CanonicalEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for call in calls {
                    self.handle_tool_call(call, out);
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.finish_reason = Some(map_finish_reason(reason).to_string());
        }
    }

    /// Parse a single tool call fragment (id/name/arguments may all arrive in fragments).
    fn handle_tool_call(&mut self, call: &serde_json::Value, out: &mut Vec<CanonicalEvent>) {
        let index = json_u64(call.get("index"));
        let incoming_id = call.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let function = call.get("function");
        let incoming_name = function
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("");
        let args = function
            .and_then(|f| f.get("arguments"))
            .and_then(|a| a.as_str())
            .unwrap_or("");
        let (started, id, name) = {
            let state = self.tools.entry(index).or_default();
            if !incoming_id.is_empty() {
                state.id = incoming_id.to_string();
            }
            if !incoming_name.is_empty() {
                state.name = incoming_name.to_string();
            }
            (state.started, state.id.clone(), state.name.clone())
        };
        let complete = !id.is_empty() && !name.is_empty();
        if !started && (complete || !args.is_empty()) {
            if let Some(state) = self.tools.get_mut(&index) {
                state.started = true;
            }
            self.ensure_started(out, 0);
            out.push(CanonicalEvent::ToolUseStart { index, id, name });
        }
        if !args.is_empty() {
            self.ensure_started(out, 0);
            out.push(CanonicalEvent::ToolUseInputDelta {
                index,
                partial_json: args.to_string(),
            });
        }
    }

    /// Emit MessageStop (idempotent).
    fn emit_stop(&mut self, out: &mut Vec<CanonicalEvent>) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.ensure_started(out, 0);
        let reason = self
            .finish_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string());
        out.push(CanonicalEvent::MessageStop {
            stop_reason: reason,
        });
    }

    /// Ensure MessageStart has been emitted (only once).
    fn ensure_started(&mut self, out: &mut Vec<CanonicalEvent>, input_tokens: u64) {
        if self.started {
            return;
        }
        self.started = true;
        let id = if self.message_id.is_empty() {
            "chatcmpl-unknown".to_string()
        } else {
            self.message_id.clone()
        };
        out.push(CanonicalEvent::MessageStart {
            id,
            model: self.model.clone(),
            input_tokens,
        });
    }
}

/// OpenAI finish_reason → Anthropic stop_reason.
fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        // "stop" and other values like content_filter are all treated as a normal end
        _ => "end_turn",
    }
}

/// Extract a u64 from a `Value` (defaults to 0).
fn json_u64(value: Option<&serde_json::Value>) -> u64 {
    value.and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Extract an error message (falls back to the whole JSON text if there's no message field).
fn extract_error_message(err: &serde_json::Value) -> String {
    err.get("message")
        .and_then(|m| m.as_str())
        .map(|m| m.to_string())
        .unwrap_or_else(|| err.to_string())
}

/// Truncate overly long text (on a char boundary, to avoid panicking).
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_chunk(content: &str, finish: Option<&str>) -> String {
        let payload = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-x",
            "choices": [{
                "index": 0,
                "delta": { "content": content },
                "finish_reason": finish,
            }],
        });
        format!("data: {payload}\n\n")
    }

    fn tool_chunk(index: u64, id: Option<&str>, name: Option<&str>, args: Option<&str>) -> String {
        let mut function = serde_json::Map::new();
        if let Some(name) = name {
            function.insert("name".to_string(), serde_json::json!(name));
        }
        if let Some(args) = args {
            function.insert("arguments".to_string(), serde_json::json!(args));
        }
        let mut call = serde_json::Map::new();
        call.insert("index".to_string(), serde_json::json!(index));
        if let Some(id) = id {
            call.insert("id".to_string(), serde_json::json!(id));
        }
        call.insert("function".to_string(), serde_json::Value::Object(function));
        let payload = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-x",
            "choices": [{
                "index": 0,
                "delta": { "tool_calls": [call] },
                "finish_reason": null,
            }],
        });
        format!("data: {payload}\n\n")
    }

    fn usage_chunk(input: u64, output: u64) -> String {
        let payload = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-x",
            "choices": [],
            "usage": { "prompt_tokens": input, "completion_tokens": output },
        });
        format!("data: {payload}\n\n")
    }

    #[test]
    fn decodes_text_stream_across_chunks() {
        let mut decoder = OpenAiSseDecoder::new();
        let first = decoder.feed(text_chunk("He", None).as_bytes());
        assert_eq!(
            first,
            vec![
                CanonicalEvent::MessageStart {
                    id: "chatcmpl-1".to_string(),
                    model: "gpt-x".to_string(),
                    input_tokens: 0,
                },
                CanonicalEvent::TextDelta("He".to_string()),
            ]
        );
        let second = decoder.feed(text_chunk("llo", None).as_bytes());
        assert_eq!(second, vec![CanonicalEvent::TextDelta("llo".to_string())]);
        let finish = decoder.feed(text_chunk("", Some("stop")).as_bytes());
        assert!(finish.is_empty());
        let done = decoder.feed(b"data: [DONE]\n\n");
        assert_eq!(
            done,
            vec![CanonicalEvent::MessageStop {
                stop_reason: "end_turn".to_string(),
            }]
        );
    }

    #[test]
    fn decodes_tool_call_arguments_in_three_fragments() {
        let mut decoder = OpenAiSseDecoder::new();
        let started =
            decoder.feed(tool_chunk(0, Some("call_a"), Some("get_weather"), None).as_bytes());
        assert_eq!(
            started,
            vec![
                CanonicalEvent::MessageStart {
                    id: "chatcmpl-1".to_string(),
                    model: "gpt-x".to_string(),
                    input_tokens: 0,
                },
                CanonicalEvent::ToolUseStart {
                    index: 0,
                    id: "call_a".to_string(),
                    name: "get_weather".to_string(),
                },
            ]
        );
        let part1 = decoder.feed(tool_chunk(0, None, None, Some("{\"ci")).as_bytes());
        let part2 = decoder.feed(tool_chunk(0, None, None, Some("ty\":\"SF")).as_bytes());
        let part3 = decoder.feed(tool_chunk(0, None, None, Some("\"}")).as_bytes());
        let deltas: Vec<CanonicalEvent> = [part1, part2, part3].concat();
        assert_eq!(
            deltas,
            vec![
                CanonicalEvent::ToolUseInputDelta {
                    index: 0,
                    partial_json: "{\"ci".to_string(),
                },
                CanonicalEvent::ToolUseInputDelta {
                    index: 0,
                    partial_json: "ty\":\"SF".to_string(),
                },
                CanonicalEvent::ToolUseInputDelta {
                    index: 0,
                    partial_json: "\"}".to_string(),
                },
            ]
        );
    }

    #[test]
    fn decodes_two_tool_calls_by_index() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(tool_chunk(0, Some("call_a"), Some("f_a"), None).as_bytes());
        decoder.feed(tool_chunk(1, Some("call_b"), Some("f_b"), None).as_bytes());
        let delta = decoder.feed(tool_chunk(1, None, None, Some("{}")).as_bytes());
        assert_eq!(
            delta,
            vec![CanonicalEvent::ToolUseInputDelta {
                index: 1,
                partial_json: "{}".to_string(),
            }]
        );
    }

    #[test]
    fn decodes_usage_chunk_after_text() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(text_chunk("hi", None).as_bytes());
        let events = decoder.feed(usage_chunk(11, 22).as_bytes());
        assert_eq!(
            events,
            vec![CanonicalEvent::Usage {
                input_tokens: 11,
                output_tokens: 22,
            }]
        );
    }

    #[test]
    fn usage_only_stream_puts_tokens_in_message_start() {
        let mut decoder = OpenAiSseDecoder::new();
        let events = decoder.feed(usage_chunk(7, 3).as_bytes());
        assert_eq!(
            events,
            vec![
                CanonicalEvent::MessageStart {
                    id: "chatcmpl-1".to_string(),
                    model: "gpt-x".to_string(),
                    input_tokens: 7,
                },
                CanonicalEvent::Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                },
            ]
        );
    }

    #[test]
    fn maps_all_finish_reasons() {
        let cases = [
            ("stop", "end_turn"),
            ("length", "max_tokens"),
            ("tool_calls", "tool_use"),
        ];
        for (upstream, expected) in cases {
            let mut decoder = OpenAiSseDecoder::new();
            decoder.feed(text_chunk("x", Some(upstream)).as_bytes());
            let events = decoder.feed(b"data: [DONE]\n\n");
            assert_eq!(
                events,
                vec![CanonicalEvent::MessageStop {
                    stop_reason: expected.to_string(),
                }],
                "finish_reason={upstream}"
            );
        }
    }

    #[test]
    fn done_without_finish_reason_defaults_end_turn() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(text_chunk("x", None).as_bytes());
        let events = decoder.feed(b"data: [DONE]\n\n");
        assert_eq!(
            events,
            vec![CanonicalEvent::MessageStop {
                stop_reason: "end_turn".to_string(),
            }]
        );
    }

    #[test]
    fn error_payload_emits_error_event() {
        let mut decoder = OpenAiSseDecoder::new();
        let events = decoder
            .feed(b"data: {\"error\":{\"message\":\"rate limited\",\"type\":\"rate_limit\"}}\n\n");
        assert_eq!(
            events,
            vec![CanonicalEvent::Error("rate limited".to_string())]
        );
    }

    #[test]
    fn malformed_json_emits_error_event() {
        let mut decoder = OpenAiSseDecoder::new();
        let events = decoder.feed(b"data: {oops not json\n\n");
        match events.as_slice() {
            [CanonicalEvent::Error(message)] => {
                assert!(message.contains("invalid upstream SSE payload"));
            }
            other => panic!("expected error event, got {other:?}"),
        }
    }

    #[test]
    fn assembles_event_split_across_feed_calls() {
        let mut decoder = OpenAiSseDecoder::new();
        let full = tool_chunk(0, Some("call_a"), Some("f_a"), Some("{}"));
        let (head, tail) = full.split_at(full.len() / 2);
        assert!(decoder.feed(head.as_bytes()).is_empty());
        let events = decoder.feed(tail.as_bytes());
        assert_eq!(
            events,
            vec![
                CanonicalEvent::MessageStart {
                    id: "chatcmpl-1".to_string(),
                    model: "gpt-x".to_string(),
                    input_tokens: 0,
                },
                CanonicalEvent::ToolUseStart {
                    index: 0,
                    id: "call_a".to_string(),
                    name: "f_a".to_string(),
                },
                CanonicalEvent::ToolUseInputDelta {
                    index: 0,
                    partial_json: "{}".to_string(),
                },
            ]
        );
    }

    #[test]
    fn ignores_comment_lines_between_events() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(text_chunk("hi", None).as_bytes());
        let events = decoder.feed(b": keep-alive\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn finish_without_done_emits_stop_once() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(text_chunk("hi", Some("length")).as_bytes());
        let first = decoder.finish();
        assert_eq!(
            first,
            vec![CanonicalEvent::MessageStop {
                stop_reason: "max_tokens".to_string(),
            }]
        );
        assert!(decoder.finish().is_empty());
    }

    #[test]
    fn ignores_payload_after_done() {
        let mut decoder = OpenAiSseDecoder::new();
        decoder.feed(text_chunk("hi", None).as_bytes());
        decoder.feed(b"data: [DONE]\n\n");
        let after = decoder.feed(text_chunk("late", None).as_bytes());
        assert!(after.is_empty());
    }
}
