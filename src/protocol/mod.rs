//! OpenAI <-> Anthropic streaming protocol conversion.
//!
//! Layers:
//! - [`stream`]: protocol-agnostic [`stream::CanonicalEvent`] intermediate representation + generic SSE line parser
//! - [`openai_sse`]: upstream OpenAI-compatible SSE chunks -> canonical events
//! - [`anthropic_sse`]: canonical events -> downstream Anthropic SSE event lines
//!
//! Wiring: `OpenAiSseDecoder::feed` consumes the upstream byte stream and emits canonical events,
//! which are handed to `AnthropicSseRenderer::render` to render SSE frames ready to write straight to the response body;
//! `AnthropicSseRenderer::finish` is called as a fallback wrap-up when the upstream stream breaks unexpectedly.

pub mod anthropic_sse;
pub mod openai_sse;
pub mod stream;
