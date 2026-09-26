//! Canonical intermediate representation for streaming protocols, plus generic SSE line parsing.
//!
//! Upstream (OpenAI-compatible) and downstream (Anthropic) are decoupled via [`CanonicalEvent`]:
//! decoders are only responsible for producing canonical events, renderers are only responsible for
//! consuming canonical events, and neither side is aware of the other's data structures.

/// A protocol-agnostic streaming event (intermediate representation).
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalEvent {
    /// Message start: carries the upstream message id, model, and input token count
    MessageStart {
        id: String,
        model: String,
        input_tokens: u64,
    },
    /// Body text delta
    TextDelta(String),
    /// Tool-use block start: `index` is the upstream tool call's index (used to correlate subsequent deltas)
    ToolUseStart {
        index: u64,
        id: String,
        name: String,
    },
    /// Tool-use input JSON delta: `index` corresponds to [`CanonicalEvent::ToolUseStart`]
    ToolUseInputDelta { index: u64, partial_json: String },
    /// Usage stats (usually a standalone chunk at the end of the stream)
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Message stop: stop_reason ∈ `end_turn|max_tokens|tool_use|stop_sequence`
    MessageStop { stop_reason: String },
    /// Upstream error or parse error
    Error(String),
}

/// Generic SSE parser: slices an arbitrary byte stream into `(event_name, data)` pairs.
///
/// Supports:
/// - A line split across multiple chunks (bytes buffered internally, line boundaries determined by `\n`)
/// - A single chunk containing multiple lines / multiple events
/// - Multi-line `data:` joined with `\n` into one event
/// - Comment lines (e.g. `: keep-alive`) ignored
/// - CRLF and LF line endings
/// - UTF-8 multi-byte characters split across chunk boundaries (buffered by byte, decoded only once a full line forms)
#[derive(Debug, Default)]
pub struct SseLineParser {
    /// Raw bytes not yet forming a complete line
    buf: Vec<u8>,
    /// Current event name (the `event:` field)
    event: Option<String>,
    /// Current event's data lines (the `data:` field, can be multi-line)
    data: Vec<String>,
}

impl SseLineParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed in a chunk of bytes, returns the complete events parsed out this call.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(Option<String>, String)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // strip the trailing '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            self.handle_line(&line, &mut out);
        }
        out
    }

    /// Handles a single line of text; an empty line marks the end of an event and triggers dispatch.
    fn handle_line(&mut self, line: &str, out: &mut Vec<(Option<String>, String)>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        // A line starting with ':' is a comment line (e.g. `: keep-alive`), ignored per the SSE spec
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.find(':') {
            Some(i) => (
                &line[..i],
                line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..]),
            ),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            // Fields like id / retry are meaningless for protocol conversion, ignored
            _ => {}
        }
    }

    /// Dispatches the currently accumulated event; with no data buffered, drops it per the SSE spec (only clears the event name).
    fn dispatch(&mut self, out: &mut Vec<(Option<String>, String)>) {
        if self.data.is_empty() {
            self.event = None;
            return;
        }
        let data = self.data.join("\n");
        self.data.clear();
        out.push((self.event.take(), data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_event_with_data() {
        let mut parser = SseLineParser::new();
        let events = parser.feed(b"event: message_start\ndata: {\"a\":1}\n\n");
        assert_eq!(
            events,
            vec![(Some("message_start".to_string()), "{\"a\":1}".to_string())]
        );
    }

    #[test]
    fn parses_data_only_event() {
        let mut parser = SseLineParser::new();
        let events = parser.feed(b"data: [DONE]\n\n");
        assert_eq!(events, vec![(None, "[DONE]".to_string())]);
    }

    #[test]
    fn buffers_line_split_across_chunks() {
        let mut parser = SseLineParser::new();
        assert!(parser.feed(b"data: he").is_empty());
        assert!(parser.feed(b"llo").is_empty());
        let events = parser.feed(b"\n\n");
        assert_eq!(events, vec![(None, "hello".to_string())]);
    }

    #[test]
    fn joins_multiline_data_with_newline() {
        let mut parser = SseLineParser::new();
        let events = parser.feed(b"data: line1\ndata: line2\n\n");
        assert_eq!(events, vec![(None, "line1\nline2".to_string())]);
    }

    #[test]
    fn ignores_comment_and_unknown_fields() {
        let mut parser = SseLineParser::new();
        assert!(parser.feed(b": keep-alive\n\n").is_empty());
        assert!(parser.feed(b"id: 42\n\n").is_empty());
        assert!(parser.feed(b"retry: 1000\n\n").is_empty());
    }

    #[test]
    fn handles_crlf_and_multiple_events_in_one_chunk() {
        let mut parser = SseLineParser::new();
        let events = parser.feed(b"data: a\r\n\r\ndata: b\r\n\r\n");
        assert_eq!(
            events,
            vec![(None, "a".to_string()), (None, "b".to_string()),]
        );
    }

    #[test]
    fn carries_utf8_char_split_across_chunks() {
        // In "data: 中\n\n", "中" takes 3 bytes; feed it split apart in the middle of the character
        let raw = "data: 中\n\n".as_bytes();
        let split = raw.len() - 4; // keep the last byte of "中" plus the two newlines
        let mut parser = SseLineParser::new();
        assert!(parser.feed(&raw[..split]).is_empty());
        let events = parser.feed(&raw[split..]);
        assert_eq!(events, vec![(None, "中".to_string())]);
    }

    #[test]
    fn data_value_keeps_leading_spaces_after_one_separator() {
        // SSE spec: only the first space after the colon is removed, remaining spaces are kept
        let mut parser = SseLineParser::new();
        let events = parser.feed(b"data:  two spaces\n\n");
        assert_eq!(events, vec![(None, " two spaces".to_string())]);
    }
}
