//! In-process STT inference for the adventurer STT worker.
//!
//! ```ignore
//! use adventurer_inference_stt::SttEngine;
//!
//! let stt = SttEngine::load(&ggml_bin)?.with_threads(8);
//! let (text, metrics) = stt.transcribe(&pcm_16khz_mono_f32, "en")?;
//! ```
//!
//! **This crate intentionally does not link `llama-cpp-2`** — see the
//! `adventurer-inference-llm` crate docs for the why (vendored ggml clash).

pub mod stt;

pub use stt::{SttEngine, TranscribeMetrics};
