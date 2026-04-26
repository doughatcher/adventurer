//! adventurer-llm-bench: A/B Gemma 4 state extraction.
//!
//! Same prompt, same transcript fixture, two backends:
//!   --ollama  → POST to http://localhost:11434/api/generate (the dnd-stage path)
//!   default   → adventurer-inference (in-process llama-cpp-2)

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

mod worker;

use adventurer_inference_llm::LlmEngine;
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;

const SYSTEM_PROMPT: &str =
    "You are a precise D&D session state tracker. Output only what is asked, in exact format specified. No extra commentary.";

#[derive(Parser, Debug)]
#[command(version, about = "PoC bench: gemma4 state extraction via in-process llama.cpp vs Ollama")]
struct Args {
    /// Path to a GGUF model file. Default: dolphin3:8b blob (Llama 3.1 arch).
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

    /// GPU layers to offload (0 = CPU-only). 99 = offload everything when built with --features cuda.
    #[arg(long, default_value_t = 0)]
    gpu_layers: u32,

    /// Hit local Ollama instead of in-process inference (for A/B comparison).
    #[arg(long)]
    ollama: bool,

    /// Worker mode: load model, then read line-delimited JSON requests from stdin
    /// and write responses to stdout. Used by the adventurer server to keep an
    /// LLM engine warm across requests.
    #[arg(long)]
    worker: bool,

    #[arg(long, default_value = "http://localhost:11434")]
    ollama_base: String,

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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,adventurer_inference=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    if args.worker {
        return worker::run(worker::WorkerOpts {
            model: args.model.clone(),
            n_ctx: args.n_ctx,
            gpu_layers: args.gpu_layers,
        })
        .await;
    }

    eprintln!("─── adventurer-llm-bench ───");
    let prompt = build_prompt(&args)?;
    eprintln!("prompt: {} chars", prompt.len());

    let (output, generated_tokens, elapsed_secs) = if args.ollama {
        eprintln!("backend: ollama @ {} ({})", args.ollama_base, args.ollama_model);
        run_ollama(&args, &prompt).await?
    } else {
        eprintln!("backend: in-process llama-cpp-2");
        eprintln!("model:   {}", args.model.display());
        eprintln!("ctx:     {}, gpu_layers: {}", args.n_ctx, args.gpu_layers);
        let model = args.model.clone();
        let n_ctx = args.n_ctx;
        let gpu_layers = args.gpu_layers;
        let max_tokens = args.max_tokens;
        let prompt_clone = prompt.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let engine = LlmEngine::load(&model, n_ctx, gpu_layers)?;
            let (out, m) = engine.generate(SYSTEM_PROMPT, &prompt_clone, max_tokens)?;
            Ok((out, m.tokens_generated, m.elapsed_secs))
        })
        .await??
    };

    let tps = generated_tokens as f64 / elapsed_secs.max(0.001);
    eprintln!("─── result ───");
    eprintln!(
        "generated: {} tokens in {:.2}s ({:.1} t/s)",
        generated_tokens, elapsed_secs, tps
    );
    println!("\n{}\n", output.trim());

    validate(&output);
    Ok(())
}

#[derive(Deserialize)]
struct OllamaResp {
    response: String,
    #[serde(default)]
    eval_count: usize,
    #[serde(default)]
    eval_duration: u64, // ns
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
    let t = Instant::now();
    let resp: OllamaResp = client
        .post(format!("{}/api/generate", args.ollama_base))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let wall = t.elapsed().as_secs_f64();
    let eval = if resp.eval_duration > 0 {
        resp.eval_duration as f64 / 1e9
    } else {
        wall
    };
    Ok((resp.response, resp.eval_count, eval))
}

fn validate(raw: &str) {
    eprintln!("─── validation ───");
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
            let n_chars = v
                .get("characters")
                .and_then(|c| c.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            eprintln!(
                "✓ valid JSON | location:{} characters:{} ({} entries)",
                v.get("location").is_some(),
                v.get("characters").is_some(),
                n_chars
            );
        }
        Err(e) => {
            let _ = anyhow!(e);
            eprintln!("✗ JSON parse failed");
        }
    }
}
