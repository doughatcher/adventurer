//! adventurer-poc: A/B test gemma3 state extraction.
//!
//! Same prompt, same transcript fixture, two backends:
//!   --ollama  → POST to http://localhost:11434/api/generate (the dnd-stage path)
//!   default   → llama-cpp-2 in-process inference
//!
//! Goal: confirm in-process inference gives same-or-better state JSON than the
//! existing Ollama integration before committing to the full Rust port.

use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;

const SYSTEM_PROMPT: &str =
    "You are a precise D&D session state tracker. Output only what is asked, in exact format specified. No extra commentary.";

#[derive(Parser, Debug)]
#[command(version, about = "PoC: gemma3 state extraction via in-process llama.cpp vs Ollama")]
struct Args {
    /// Path to a GGUF model file. Default: the dolphin3:8b blob (Llama 3.1 arch, well-supported).
    /// NOTE: gemma4:e4b is NOT compatible with llama-cpp-sys-2 0.1.145 — Ollama runs a forked
    /// llama.cpp with newer architecture support. See README "Findings".
    #[arg(
        long,
        default_value = "/var/home/me/.ollama/models/blobs/sha256-1eee6953530837b2b17d61a4e6f71a5aa31c9714cfcf3cb141aa5c1972b5116b"
    )]
    model: PathBuf,

    /// Prompt template (state.txt with {CURRENT_STATE}/{PARTY_DATA}/{TRANSCRIPT} placeholders).
    #[arg(long, default_value = "prompts/state.txt")]
    prompt: PathBuf,

    /// Sample transcript markdown.
    #[arg(long, default_value = "samples/transcript.md")]
    transcript: PathBuf,

    /// Sample party data markdown.
    #[arg(long, default_value = "samples/party.md")]
    party: PathBuf,

    /// Max tokens to generate.
    #[arg(long, default_value_t = 350)]
    max_tokens: i32,

    /// Context window size.
    #[arg(long, default_value_t = 4096)]
    n_ctx: u32,

    /// GPU layers to offload (0 = CPU-only). Set to 99 to offload everything when built with --features cuda.
    #[arg(long, default_value_t = 0)]
    gpu_layers: u32,

    /// Hit local Ollama instead of in-process inference (for A/B comparison).
    #[arg(long)]
    ollama: bool,

    /// Ollama base URL.
    #[arg(long, default_value = "http://localhost:11434")]
    ollama_base: String,

    /// Ollama model tag.
    #[arg(long, default_value = "gemma4:e4b")]
    ollama_model: String,
}

fn build_prompt(args: &Args) -> Result<String> {
    let template = fs::read_to_string(&args.prompt)
        .with_context(|| format!("read prompt template {}", args.prompt.display()))?;
    let transcript = fs::read_to_string(&args.transcript)
        .with_context(|| format!("read transcript {}", args.transcript.display()))?;
    let party = fs::read_to_string(&args.party)
        .with_context(|| format!("read party {}", args.party.display()))?;

    // Mirror server/gemma.py: keep last ~2000 chars of transcript.
    let tail = if transcript.len() > 2000 {
        &transcript[transcript.len() - 2000..]
    } else {
        &transcript[..]
    };

    Ok(template
        .replace("{CURRENT_STATE}", "{}")
        .replace("{PARTY_DATA}", party.trim())
        .replace("{TRANSCRIPT}", tail.trim()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("─── adventurer-poc ───");
    let prompt = build_prompt(&args)?;
    eprintln!("prompt: {} chars", prompt.len());

    let (output, generated_tokens, elapsed_secs) = if args.ollama {
        eprintln!("backend: ollama @ {} ({})", args.ollama_base, args.ollama_model);
        run_ollama(&args, &prompt).await?
    } else {
        eprintln!("backend: llama-cpp-2 (in-process)");
        eprintln!("model:   {}", args.model.display());
        eprintln!("ctx:     {}, gpu_layers: {}", args.n_ctx, args.gpu_layers);
        let args_clone = args_for_blocking(&args);
        let prompt_clone = prompt.clone();
        tokio::task::spawn_blocking(move || run_local(&args_clone, &prompt_clone)).await??
    };

    let tps = generated_tokens as f64 / elapsed_secs;
    eprintln!("─── result ───");
    eprintln!("generated: {} tokens in {:.2}s ({:.1} t/s)", generated_tokens, elapsed_secs, tps);
    println!("\n{}\n", output.trim());

    validate(&output);
    Ok(())
}

// ─── llama-cpp-2 path ───

#[derive(Clone)]
struct LocalArgs {
    model: PathBuf,
    n_ctx: u32,
    gpu_layers: u32,
    max_tokens: i32,
}

fn args_for_blocking(a: &Args) -> LocalArgs {
    LocalArgs {
        model: a.model.clone(),
        n_ctx: a.n_ctx,
        gpu_layers: a.gpu_layers,
        max_tokens: a.max_tokens,
    }
}

fn run_local(args: &LocalArgs, prompt: &str) -> Result<(String, usize, f64)> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{AddBos, LlamaModel, Special};
    use llama_cpp_2::sampling::LlamaSampler;

    let backend = LlamaBackend::init().context("init llama backend")?;

    let model_params = {
        let mut p = LlamaModelParams::default();
        p = p.with_n_gpu_layers(args.gpu_layers);
        p
    };

    let t0 = Instant::now();
    let model = LlamaModel::load_from_file(&backend, &args.model, &model_params)
        .with_context(|| format!("load model {}", args.model.display()))?;
    eprintln!("model loaded in {:.2}s", t0.elapsed().as_secs_f64());

    let n_ctx = NonZeroU32::new(args.n_ctx).ok_or_else(|| anyhow!("n_ctx must be > 0"))?;
    let ctx_params = LlamaContextParams::default().with_n_ctx(Some(n_ctx));
    let mut ctx = model
        .new_context(&backend, ctx_params)
        .context("new_context")?;

    // Prepend system prompt the way Ollama does for /api/generate.
    let full_prompt = format!("{SYSTEM_PROMPT}\n\n{prompt}");
    let tokens_list = model
        .str_to_token(&full_prompt, AddBos::Always)
        .context("tokenize prompt")?;
    eprintln!("prompt tokens: {}", tokens_list.len());

    let n_len = (tokens_list.len() as i32) + args.max_tokens;
    // Batch must hold the entire prompt in one decode pass.
    let batch_size = (tokens_list.len() + args.max_tokens as usize).max(512);
    let mut batch = LlamaBatch::new(batch_size, 1);
    let last_index = (tokens_list.len() - 1) as i32;
    for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
        let is_last = i == last_index;
        batch.add(token, i, &[0], is_last)?;
    }
    ctx.decode(&mut batch).context("decode prompt")?;

    let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);

    let mut output = String::new();
    let mut generated = 0usize;
    let mut n_cur = batch.n_tokens();
    let t_gen = Instant::now();
    while n_cur <= n_len {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        if model.is_eog_token(token) {
            break;
        }

        let piece = model
            .token_to_str(token, Special::Tokenize)
            .unwrap_or_default();
        output.push_str(&piece);
        // stream to stderr so stdout stays clean for piping the JSON
        eprint!("{piece}");

        batch.clear();
        batch.add(token, n_cur, &[0], true)?;
        n_cur += 1;
        ctx.decode(&mut batch).context("decode token")?;
        generated += 1;
    }
    eprintln!();

    let elapsed = t_gen.elapsed().as_secs_f64();
    Ok((output, generated, elapsed))
}

// ─── Ollama path (A/B baseline) ───

#[derive(Deserialize)]
struct OllamaResp {
    response: String,
    #[serde(default)]
    eval_count: usize,
    #[serde(default)]
    eval_duration: u64, // nanoseconds
}

async fn run_ollama(args: &Args, prompt: &str) -> Result<(String, usize, f64)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let body = serde_json::json!({
        "model": args.ollama_model,
        "prompt": prompt,
        "system": SYSTEM_PROMPT,
        "stream": false,
        "options": {
            "temperature": 0.1,
            "num_ctx": args.n_ctx,
            "num_predict": args.max_tokens,
            "top_k": 20,
            "top_p": 0.9,
            "repeat_penalty": 1.1,
        },
    });
    let t0 = Instant::now();
    let resp: OllamaResp = client
        .post(format!("{}/api/generate", args.ollama_base))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let wall = t0.elapsed().as_secs_f64();
    let eval = if resp.eval_duration > 0 {
        resp.eval_duration as f64 / 1e9
    } else {
        wall
    };
    Ok((resp.response, resp.eval_count, eval))
}

// ─── output validation ───

fn validate(raw: &str) {
    eprintln!("─── validation ───");
    // Strip code fences and find first {…}.
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    let json_slice = match (start, end) {
        (Some(s), Some(e)) if e > s => &cleaned[s..=e],
        _ => {
            eprintln!("✗ no JSON object found in output");
            return;
        }
    };
    match serde_json::from_str::<Value>(json_slice) {
        Ok(v) => {
            let has_chars = v.get("characters").is_some();
            let has_loc = v.get("location").is_some();
            let n_chars = v
                .get("characters")
                .and_then(|c| c.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            eprintln!(
                "✓ valid JSON | location:{} characters:{} ({}entries)",
                has_loc,
                has_chars,
                n_chars
            );
        }
        Err(e) => {
            eprintln!("✗ JSON parse failed: {e}");
            eprintln!("  slice: {}", &json_slice[..json_slice.len().min(200)]);
        }
    }
}
