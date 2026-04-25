//! LLM inference via `llama-cpp-2`. Wraps the model + context lifecycle so the
//! server doesn't have to touch the underlying API directly.
//!
//! Lifted from the original `crates/llm-bench` PoC (which still uses the same
//! pattern via this crate).

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

/// Per-call generation metrics.
#[derive(Debug, Clone, Copy)]
pub struct GenerateMetrics {
    pub tokens_generated: usize,
    pub elapsed_secs: f64,
    pub tokens_per_sec: f64,
}

/// Owns one llama.cpp model + context. Single-threaded use only (`!Send`).
pub struct LlmEngine {
    // Backend must outlive model/context; held to keep the ref count up.
    _backend: &'static LlamaBackend,
    model: LlamaModel,
    n_ctx: u32,
}

impl LlmEngine {
    /// Load a GGUF model from disk.
    ///
    /// `n_gpu_layers = 0` → CPU only. `99` → offload everything (typical when
    /// built with one of the GPU features and a GPU is available).
    pub fn load(model_path: &Path, n_ctx: u32, n_gpu_layers: u32) -> Result<Self> {
        let backend = global_backend()?;

        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
        let t = Instant::now();
        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .with_context(|| format!("load model {}", model_path.display()))?;
        tracing::info!(
            model = %model_path.display(),
            elapsed_secs = t.elapsed().as_secs_f64(),
            n_gpu_layers,
            "llama model loaded"
        );

        Ok(Self {
            _backend: backend,
            model,
            n_ctx,
        })
    }

    /// Run a single generate-to-EOG (or `max_tokens`, whichever first).
    /// `system_prompt` is prepended to `prompt` separated by a blank line — same
    /// shape Ollama's `/api/generate` would do server-side.
    ///
    /// Returns `(generated_text, metrics)`.
    pub fn generate(
        &self,
        system_prompt: &str,
        prompt: &str,
        max_tokens: i32,
    ) -> Result<(String, GenerateMetrics)> {
        let n_ctx_nz = NonZeroU32::new(self.n_ctx).ok_or_else(|| anyhow!("n_ctx must be > 0"))?;
        let ctx_params = LlamaContextParams::default().with_n_ctx(Some(n_ctx_nz));
        let mut ctx: LlamaContext<'_> = self
            .model
            .new_context(self._backend, ctx_params)
            .context("new_context")?;

        let full_prompt = if system_prompt.is_empty() {
            prompt.to_string()
        } else {
            format!("{system_prompt}\n\n{prompt}")
        };
        let tokens_list = self
            .model
            .str_to_token(&full_prompt, AddBos::Always)
            .context("tokenize prompt")?;

        let n_len = (tokens_list.len() as i32) + max_tokens;
        let batch_size = (tokens_list.len() + max_tokens as usize).max(512);
        let mut batch = LlamaBatch::new(batch_size, 1);
        let last_index = (tokens_list.len() - 1) as i32;
        for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
            batch.add(token, i, &[0], i == last_index)?;
        }
        ctx.decode(&mut batch).context("decode prompt")?;

        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);

        let mut output = String::new();
        let mut generated = 0usize;
        let mut n_cur = batch.n_tokens();
        let t = Instant::now();
        while n_cur <= n_len {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            if self.model.is_eog_token(token) {
                break;
            }

            let piece = self
                .model
                .token_to_str(token, Special::Tokenize)
                .unwrap_or_default();
            output.push_str(&piece);

            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch).context("decode token")?;
            generated += 1;
        }
        let elapsed = t.elapsed().as_secs_f64();
        let tps = generated as f64 / elapsed.max(0.001);

        Ok((
            output,
            GenerateMetrics {
                tokens_generated: generated,
                elapsed_secs: elapsed,
                tokens_per_sec: tps,
            },
        ))
    }

    /// Streaming variant — invokes `on_token` for each piece as it comes off the
    /// sampler. Useful for the WebSocket path where the UI wants live updates.
    pub fn generate_streaming<F: FnMut(&str)>(
        &self,
        system_prompt: &str,
        prompt: &str,
        max_tokens: i32,
        mut on_token: F,
    ) -> Result<(String, GenerateMetrics)> {
        let n_ctx_nz = NonZeroU32::new(self.n_ctx).ok_or_else(|| anyhow!("n_ctx must be > 0"))?;
        let ctx_params = LlamaContextParams::default().with_n_ctx(Some(n_ctx_nz));
        let mut ctx: LlamaContext<'_> = self.model.new_context(self._backend, ctx_params)?;

        let full_prompt = if system_prompt.is_empty() {
            prompt.to_string()
        } else {
            format!("{system_prompt}\n\n{prompt}")
        };
        let tokens_list = self.model.str_to_token(&full_prompt, AddBos::Always)?;

        let n_len = (tokens_list.len() as i32) + max_tokens;
        let batch_size = (tokens_list.len() + max_tokens as usize).max(512);
        let mut batch = LlamaBatch::new(batch_size, 1);
        let last_index = (tokens_list.len() - 1) as i32;
        for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
            batch.add(token, i, &[0], i == last_index)?;
        }
        ctx.decode(&mut batch)?;

        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);

        let mut output = String::new();
        let mut generated = 0usize;
        let mut n_cur = batch.n_tokens();
        let t = Instant::now();
        while n_cur <= n_len {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            if self.model.is_eog_token(token) {
                break;
            }

            let piece = self
                .model
                .token_to_str(token, Special::Tokenize)
                .unwrap_or_default();
            on_token(&piece);
            output.push_str(&piece);

            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch)?;
            generated += 1;
        }
        let elapsed = t.elapsed().as_secs_f64();
        let tps = generated as f64 / elapsed.max(0.001);

        Ok((
            output,
            GenerateMetrics {
                tokens_generated: generated,
                elapsed_secs: elapsed,
                tokens_per_sec: tps,
            },
        ))
    }
}

/// Lazy global LlamaBackend init. `LlamaBackend::init` is idempotent enough that
/// calling it from multiple engines is fine; we still cache to avoid the cost.
fn global_backend() -> Result<&'static LlamaBackend> {
    use std::sync::OnceLock;
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    if let Some(b) = BACKEND.get() {
        return Ok(b);
    }
    let b = LlamaBackend::init().context("init llama backend")?;
    Ok(BACKEND.get_or_init(|| b))
}
