//! In-process LLM inference for the adventurer LLM worker.
//!
//! ```ignore
//! use adventurer_inference_llm::LlmEngine;
//!
//! let llm = LlmEngine::load(&model_gguf, 4096, 99)?;
//! let (text, metrics) = llm.generate(SYSTEM_PROMPT, &user_prompt, 500)?;
//! ```
//!
//! `LlmEngine` is `!Send` (it holds raw pointers into llama.cpp state). Wrap in
//! `tokio::sync::Mutex` or pin to a single tokio task / thread.
//!
//! **This crate intentionally does not link `whisper-rs`.** Both libraries
//! vendor their own static copy of `ggml`; linking them into the same binary
//! produces ~hundreds of duplicate symbols and a runtime-crashing executable.
//! The STT engine lives in `adventurer-inference-stt` and is run as a separate
//! worker process by the server.

pub mod llm;

pub use llm::{GenerateMetrics, LlmEngine};
