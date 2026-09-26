//! Renders [`CanonicalEvent`] into Anthropic Messages streaming SSE event lines.
//!
//! Output frames look like `event: <name>\ndata: <json>\n\n`; the caller `join("")`s them
//! and writes the result straight to the response body.
//! Key constraints:
//! - `message_start` must precede any `content_block_*` (may be deferred until the first delta)
//! - Text blocks are streamed out in real time; at stream end, `content_block_stop` is emitted before tool blocks
//! - Tool blocks are **rendered from a buffer**: the start and arguments chunks of parallel upstream
//!   tool calls can interleave in any order, and emitting them immediately could produce an
//!   invalid sequence pointing at an already-closed block. So they're buffered by upstream index
//!   and, at finalization, emitted in ascending index order as
//!   `content_block_start -> content_block_delta(full arguments) -> content_block_stop`
//! - Content block indices are assigned by this renderer, starting from 0 and increasing, per the Anthropic spec

use std::collections::BTreeMap;

use serde_json::json;

use super::stream::CanonicalEvent;

/// A single complete SSE frame (including the trailing blank line).
type SseFrame = String;

/// A buffered tool call: `ToolUseStart` and argument chunks can arrive interleaved in any order,
/// so the whole block is emitted at finalization to keep block nesting valid.
#[derive(Debug, Default, Clone)]
struct BufferedTool {
    id: String,
    name: String,
    /// Accumulated upstream arguments JSON chunks
    arguments: String,
    /// Whether a ToolUseStart has been received (deltas without a start are ignored)
    started: bool,
}

/// Anthropic SSE renderer.
#[derive(Debug, Default)]
pub struct AnthropicSseRenderer {
    /// Message model (can be overridden by a `MessageStart` event)
    model: String,
    /// Message id (provided by a `MessageStart` event; a fallback id is generated if missing)
    message_id: Option<String>,
    /// Whether `message_start` has been emitted
    started: bool,
    /// Index of the currently open content block (can only be a text block; tool blocks are emitted whole at finalization)
    open_index: Option<u64>,
    /// Next content block index available for allocation
    next_index: u64,
    /// Tool calls buffered by upstream index (BTreeMap guarantees ascending-index output at finalization)
    tools: BTreeMap<u64, BufferedTool>,
    /// Input token count (used to backfill message_start and message_delta)
    input_tokens: u64,
    /// Output token count (used for message_delta)
    output_tokens: u64,
    /// Whether message_stop (or a terminal error) has been emitted
    stop_sent: bool,
}

impl AnthropicSseRenderer {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            ..Self::default()
        }
    }

    /// Renders one canonical event into 0..n complete SSE frames.
    pub fn render(&mut self, ev: &CanonicalEvent) -> Vec<SseFrame> {
        match ev {
            CanonicalEvent::MessageStart {
                id,
                model,
                input_tokens,
            } => self.apply_message_start(id, model, *input_tokens),
            CanonicalEvent::TextDelta(text) => self.render_text_delta(text),
            CanonicalEvent::ToolUseStart { index, id, name } => {
                self.render_tool_start(*index, id, name)
            }
            CanonicalEvent::ToolUseInputDelta {
                index,
                partial_json,
            } => self.render_tool_input_delta(*index, partial_json),
            CanonicalEvent::Usage {
                input_tokens,
                output_tokens,
            } => self.render_usage(*input_tokens, *output_tokens),
            CanonicalEvent::MessageStop { stop_reason } => self.render_stop(stop_reason),
            CanonicalEvent::Error(message) => self.render_error(message),
        }
    }

    /// Fallback at stream end: ensures `content_block_stop` (if a block is open), buffered
    /// tool blocks, `message_delta`, and `message_stop` have all been emitted; returns
    /// empty if already finalized.
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.stop_sent {
            return Vec::new();
        }
        self.finalize("end_turn")
    }

    /// Handles MessageStart: records id/model/input_tokens (first one wins); ignored once already started.
    fn apply_message_start(&mut self, id: &str, model: &str, input_tokens: u64) -> Vec<SseFrame> {
        if !id.is_empty() {
            self.message_id = Some(id.to_string());
        }
        if !model.is_empty() {
            self.model = model.to_string();
        }
        self.input_tokens = input_tokens;
        self.ensure_started()
    }

    /// Text delta: ensures started and inside a text block, then emits text_delta.
    fn render_text_delta(&mut self, text: &str) -> Vec<SseFrame> {
        let mut out = self.ensure_started();
        if text.is_empty() {
            return out;
        }
        // The open block can only be a text block (tool blocks are emitted whole at finalization)
        let index = match self.open_index {
            Some(index) => index,
            None => self.open_text_block(&mut out),
        };
        out.push(frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text },
            }),
        ));
        out
    }

    /// Tool block start: only buffered (not emitted immediately), to keep parallel tool call block nesting valid.
    fn render_tool_start(&mut self, upstream_index: u64, id: &str, name: &str) -> Vec<SseFrame> {
        let out = self.ensure_started();
        let state = self.tools.entry(upstream_index).or_default();
        if !state.started {
            state.started = true;
            if !id.is_empty() {
                state.id = id.to_string();
            }
            if !name.is_empty() {
                state.name = name.to_string();
            }
        }
        out
    }

    /// Tool argument delta: appends to the matching buffer (ignored for an upstream index that never started).
    fn render_tool_input_delta(
        &mut self,
        upstream_index: u64,
        partial_json: &str,
    ) -> Vec<SseFrame> {
        let out = self.ensure_started();
        if partial_json.is_empty() {
            return out;
        }
        if let Some(state) = self.tools.get_mut(&upstream_index) {
            state.arguments.push_str(partial_json);
        }
        out
    }

    /// Usage: records the token counts; also takes the chance to emit message_start first if not yet started.
    fn render_usage(&mut self, input_tokens: u64, output_tokens: u64) -> Vec<SseFrame> {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.ensure_started()
    }

    /// Message end: closes the text block + emits buffered tool blocks + message_delta + message_stop (idempotent).
    fn render_stop(&mut self, stop_reason: &str) -> Vec<SseFrame> {
        if self.stop_sent {
            return Vec::new();
        }
        self.finalize(stop_reason)
    }

    /// Unified finalization: close the text block -> emit buffered tool blocks in ascending index order -> message_delta + message_stop.
    fn finalize(&mut self, stop_reason: &str) -> Vec<SseFrame> {
        let mut out = self.ensure_started();
        out.extend(self.close_block());
        out.extend(self.flush_tools());
        out.extend(self.stop_frames(stop_reason));
        out
    }

    /// Emits all buffered tool blocks: each block as start -> delta (full arguments) -> stop.
    fn flush_tools(&mut self) -> Vec<SseFrame> {
        let tools = std::mem::take(&mut self.tools);
        let mut out = Vec::new();
        for tool in tools.into_values() {
            let index = self.alloc_index();
            out.push(frame(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": tool.id,
                        "name": tool.name,
                        "input": {},
                    },
                }),
            ));
            // Empty arguments are emitted as an empty JSON object, so the client can always parse it
            let partial = if tool.arguments.is_empty() {
                "{}"
            } else {
                tool.arguments.as_str()
            };
            out.push(frame(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "input_json_delta", "partial_json": partial },
                }),
            ));
            out.push(frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
        }
        out
    }

    /// Error event: closes any open text block, discards the tool buffer, and emits an
    /// Anthropic error frame; no normal finalization follows after this.
    fn render_error(&mut self, message: &str) -> Vec<SseFrame> {
        let mut out = self.ensure_started();
        out.extend(self.close_block());
        self.tools.clear();
        out.push(frame(
            "error",
            json!({
                "type": "error",
                "error": { "type": "api_error", "message": message },
            }),
        ));
        self.stop_sent = true;
        out
    }

    /// Ensures message_start has been emitted (only once).
    fn ensure_started(&mut self) -> Vec<SseFrame> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![self.message_start_frame()]
    }

    /// Closes the currently open content block (empty if none is open).
    fn close_block(&mut self) -> Vec<SseFrame> {
        match self.open_index.take() {
            Some(index) => vec![frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            )],
            None => Vec::new(),
        }
    }

    /// Opens a new text block (caller guarantees the current block is already closed).
    fn open_text_block(&mut self, out: &mut Vec<SseFrame>) -> u64 {
        let index = self.alloc_index();
        self.open_index = Some(index);
        out.push(frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" },
            }),
        ));
        index
    }

    /// The message_delta + message_stop frame pair; also sets the stop flag.
    fn stop_frames(&mut self, stop_reason: &str) -> Vec<SseFrame> {
        self.stop_sent = true;
        vec![
            frame(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": normalize_stop_reason(stop_reason),
                        "stop_sequence": null,
                    },
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": self.output_tokens,
                    },
                }),
            ),
            frame("message_stop", json!({ "type": "message_stop" })),
        ]
    }

    /// Builds the message_start frame.
    fn message_start_frame(&self) -> SseFrame {
        let id = self.message_id.clone().unwrap_or_else(fallback_message_id);
        frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": self.input_tokens, "output_tokens": 0 },
                },
            }),
        )
    }

    /// Allocates the next content block index.
    fn alloc_index(&mut self) -> u64 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }
}

/// Assembles a single SSE frame.
fn frame(event: &str, data: serde_json::Value) -> SseFrame {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Normalizes the upstream stop_reason: any value not valid for Anthropic falls back to end_turn.
fn normalize_stop_reason(reason: &str) -> &str {
    match reason {
        "end_turn" | "max_tokens" | "tool_use" | "stop_sequence" => reason,
        _ => "end_turn",
    }
}

/// Fallback id used when the upstream message id is missing (a timestamp guarantees uniqueness within the process).
fn fallback_message_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("msg_{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_frame(text: &str, index: u64) -> String {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text },
            }),
        )
    }

    fn start_frame(id: &str, model: &str, input_tokens: u64) -> String {
        frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": input_tokens, "output_tokens": 0 },
                },
            }),
        )
    }

    #[test]
    fn renders_text_stream_byte_level() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let events = [
            CanonicalEvent::MessageStart {
                id: "msg_1".to_string(),
                model: "claude-x".to_string(),
                input_tokens: 7,
            },
            CanonicalEvent::TextDelta("He".to_string()),
            CanonicalEvent::TextDelta("llo".to_string()),
            CanonicalEvent::MessageStop {
                stop_reason: "end_turn".to_string(),
            },
        ];
        let out: String = events.iter().flat_map(|ev| renderer.render(ev)).collect();
        let expected = [
            start_frame("msg_1", "claude-x", 7),
            frame(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" },
                }),
            ),
            text_frame("He", 0),
            text_frame("llo", 0),
            frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ),
            frame(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                    "usage": { "input_tokens": 7, "output_tokens": 0 },
                }),
            ),
            frame("message_stop", json!({ "type": "message_stop" })),
        ]
        .concat();
        assert_eq!(out, expected);
    }

    /// Full tool block frame sequence: start -> delta(full arguments) -> stop
    fn tool_frames(index: u64, id: &str, name: &str, partial_json: &str) -> String {
        [
            frame(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {},
                    },
                }),
            ),
            frame(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "input_json_delta", "partial_json": partial_json },
                }),
            ),
            frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ),
        ]
        .concat()
    }

    fn message_delta_frame(stop_reason: &str, input_tokens: u64, output_tokens: u64) -> String {
        frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens },
            }),
        )
    }

    #[test]
    fn closes_text_block_then_emits_buffered_tool_block() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::MessageStart {
            id: "msg_1".to_string(),
            model: "claude-3".to_string(),
            input_tokens: 1,
        });
        renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        // ToolUseStart and argument chunks only go into the buffer, not emitted immediately
        assert!(renderer
            .render(&CanonicalEvent::ToolUseStart {
                index: 0,
                id: "toolu_1".to_string(),
                name: "search".to_string(),
            })
            .is_empty());
        assert!(renderer
            .render(&CanonicalEvent::ToolUseInputDelta {
                index: 0,
                partial_json: "{\"q\":\"x\"}".to_string(),
            })
            .is_empty());
        let out: String = renderer
            .render(&CanonicalEvent::MessageStop {
                stop_reason: "tool_use".to_string(),
            })
            .concat();
        let expected = [
            frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ),
            tool_frames(1, "toolu_1", "search", "{\"q\":\"x\"}"),
            message_delta_frame("tool_use", 1, 0),
            frame("message_stop", json!({ "type": "message_stop" })),
        ]
        .concat();
        assert_eq!(out, expected);
    }

    #[test]
    fn renders_parallel_tool_calls_in_index_order_byte_level() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::MessageStart {
            id: "msg_1".to_string(),
            model: "claude-3".to_string(),
            input_tokens: 3,
        });
        renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        // Parallel tools: the first chunk starts index 0/1 simultaneously; arguments backfill interleaved afterward
        renderer.render(&CanonicalEvent::ToolUseStart {
            index: 0,
            id: "toolu_a".to_string(),
            name: "fa".to_string(),
        });
        renderer.render(&CanonicalEvent::ToolUseStart {
            index: 1,
            id: "toolu_b".to_string(),
            name: "fb".to_string(),
        });
        renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 1,
            partial_json: "{\"y\"".to_string(),
        });
        renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 0,
            partial_json: "{\"x\"".to_string(),
        });
        renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 1,
            partial_json: ":2}".to_string(),
        });
        renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 0,
            partial_json: ":1}".to_string(),
        });
        let out: String = renderer
            .render(&CanonicalEvent::MessageStop {
                stop_reason: "tool_use".to_string(),
            })
            .concat();
        let expected = [
            frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ),
            tool_frames(1, "toolu_a", "fa", "{\"x\":1}"),
            tool_frames(2, "toolu_b", "fb", "{\"y\":2}"),
            message_delta_frame("tool_use", 3, 0),
            frame("message_stop", json!({ "type": "message_stop" })),
        ]
        .concat();
        assert_eq!(out, expected);
    }

    #[test]
    fn buffers_tool_arguments_until_stop() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let started = renderer.render(&CanonicalEvent::ToolUseStart {
            index: 0,
            id: "toolu_0".to_string(),
            name: "f0".to_string(),
        });
        assert_eq!(started.len(), 1); // only backfills message_start
        assert!(started[0].starts_with("event: message_start\n"));
        assert!(renderer
            .render(&CanonicalEvent::ToolUseInputDelta {
                index: 0,
                partial_json: "{\"a\"".to_string(),
            })
            .is_empty());
        assert!(renderer
            .render(&CanonicalEvent::ToolUseInputDelta {
                index: 0,
                partial_json: ":1}".to_string(),
            })
            .is_empty());
        let out: String = renderer
            .render(&CanonicalEvent::MessageStop {
                stop_reason: "tool_use".to_string(),
            })
            .concat();
        let expected = [
            tool_frames(0, "toolu_0", "f0", "{\"a\":1}"),
            message_delta_frame("tool_use", 0, 0),
            frame("message_stop", json!({ "type": "message_stop" })),
        ]
        .concat();
        assert_eq!(out, expected);
    }

    #[test]
    fn usage_sets_message_delta_output_tokens() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::MessageStart {
            id: "msg_1".to_string(),
            model: "claude-3".to_string(),
            input_tokens: 5,
        });
        renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        let usage_out = renderer.render(&CanonicalEvent::Usage {
            input_tokens: 5,
            output_tokens: 42,
        });
        assert!(usage_out.is_empty());
        let stop = renderer.render(&CanonicalEvent::MessageStop {
            stop_reason: "max_tokens".to_string(),
        });
        assert!(stop[1].contains("\"input_tokens\":5"));
        assert!(stop[1].contains("\"output_tokens\":42"));
        assert!(stop[1].contains("\"stop_reason\":\"max_tokens\""));
    }

    #[test]
    fn usage_before_any_delta_starts_message_with_input_tokens() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let out = renderer.render(&CanonicalEvent::Usage {
            input_tokens: 9,
            output_tokens: 1,
        });
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("event: message_start\n"));
        assert!(out[0].contains("\"input_tokens\":9"));
    }

    #[test]
    fn delayed_message_start_on_first_delta() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let out = renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        assert_eq!(out.len(), 3);
        assert!(out[0].starts_with("event: message_start\n"));
        assert!(out[0].contains("\"model\":\"claude-3\""));
        assert!(out[0].contains("\"id\":\"msg_"));
        assert!(out[1].starts_with("event: content_block_start\n"));
        assert!(out[2].starts_with("event: content_block_delta\n"));
    }

    #[test]
    fn finish_emits_fallback_stop_after_partial_stream() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        let out: String = renderer.finish().concat();
        assert!(out.contains("event: content_block_stop\n"));
        assert!(out.contains("\"stop_reason\":\"end_turn\""));
        assert!(out.contains("event: message_stop\n"));
        assert!(renderer.finish().is_empty());
    }

    #[test]
    fn finish_flushes_buffered_tools() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let mut frames = renderer.render(&CanonicalEvent::ToolUseStart {
            index: 0,
            id: "toolu_0".to_string(),
            name: "f0".to_string(),
        });
        frames.extend(renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 0,
            partial_json: "{}".to_string(),
        }));
        frames.extend(renderer.finish());
        let out: String = frames.concat();
        assert!(out.contains("event: message_start\n"));
        assert!(out.contains("event: content_block_start\n"));
        assert!(out.contains("\"partial_json\":\"{}\""));
        assert!(out.contains("event: content_block_stop\n"));
        assert!(out.contains("\"stop_reason\":\"end_turn\""));
        assert!(out.contains("event: message_stop\n"));
    }

    #[test]
    fn finish_is_noop_after_message_stop() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::MessageStop {
            stop_reason: "end_turn".to_string(),
        });
        assert!(renderer.finish().is_empty());
    }

    #[test]
    fn empty_stream_stop_still_emits_valid_sequence() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let out: String = renderer
            .render(&CanonicalEvent::MessageStop {
                stop_reason: "end_turn".to_string(),
            })
            .concat();
        // Only asserts structure: message_start before message_delta before message_stop, and no content block
        // (id is a timestamp fallback value, no byte-level comparison)
        let start_pos = out.find("event: message_start").unwrap_or(usize::MAX);
        let delta_pos = out.find("event: message_delta").unwrap_or(usize::MAX);
        let stop_pos = out.find("event: message_stop").unwrap_or(usize::MAX);
        assert!(start_pos < delta_pos && delta_pos < stop_pos);
        assert!(!out.contains("content_block_start"));
        assert!(out.contains("\"model\":\"claude-3\""));
    }

    #[test]
    fn error_event_closes_block_and_terminates() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::TextDelta("hi".to_string()));
        let out: String = renderer
            .render(&CanonicalEvent::Error("boom".to_string()))
            .concat();
        assert!(out.contains("event: content_block_stop\n"));
        assert!(out.contains("event: error\n"));
        assert!(out.contains("\"message\":\"boom\""));
        assert!(renderer.finish().is_empty());
    }

    #[test]
    fn normalizes_unknown_stop_reason() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        let out: String = renderer
            .render(&CanonicalEvent::MessageStop {
                stop_reason: "content_filter".to_string(),
            })
            .concat();
        assert!(out.contains("\"stop_reason\":\"end_turn\""));
    }

    #[test]
    fn ignores_tool_input_delta_without_start() {
        let mut renderer = AnthropicSseRenderer::new("claude-3");
        renderer.render(&CanonicalEvent::MessageStart {
            id: "msg_1".to_string(),
            model: "claude-3".to_string(),
            input_tokens: 0,
        });
        let out = renderer.render(&CanonicalEvent::ToolUseInputDelta {
            index: 3,
            partial_json: "{}".to_string(),
        });
        assert!(out.is_empty());
    }
}
