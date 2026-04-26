//! Worker mode for the LLM bench.
//!
//! Activated via `--worker`. After model load:
//!
//!   1. Print a single `{"type":"ready", ...}` line to stdout
//!   2. Read line-delimited JSON requests on stdin
//!   3. For each request, generate and write a single JSON response to stdout
//!   4. Exit on stdin EOF
//!
//! Stdout = JSON only. Stderr = logs / model load chatter / progress.
//! The server reads stdout line-by-line and matches `id` for correlation.
//!
//! Architecture: `LlmEngine` is `!Send` (holds raw llama.cpp pointers), so
//! it lives on a dedicated `std::thread` and the tokio side hands it work via
//! a sync mpsc channel. Each job carries a oneshot back for the result.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use adventurer_inference_llm::LlmEngine;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

pub struct WorkerOpts {
    pub model: PathBuf,
    pub n_ctx: u32,
    pub gpu_layers: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Generate {
        id: String,
        #[serde(default)]
        system: String,
        prompt: String,
        #[serde(default = "default_max_tokens")]
        max_tokens: i32,
    },
    Ping {
        id: String,
    },
    Shutdown {
        #[serde(default)]
        id: String,
    },
}

fn default_max_tokens() -> i32 {
    500
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response<'a> {
    Ready {
        backend: &'static str,
        model: &'a str,
        n_ctx: u32,
        gpu_layers: u32,
    },
    Pong {
        id: &'a str,
    },
    Result_ {
        id: &'a str,
        text: String,
        tokens: usize,
        elapsed_secs: f64,
        tokens_per_sec: f64,
    },
    Error {
        id: &'a str,
        message: String,
    },
    ShutdownAck {
        id: &'a str,
    },
}

const BACKEND: &str = if cfg!(feature = "cuda") {
    "cuda"
} else if cfg!(feature = "vulkan") {
    "vulkan"
} else if cfg!(feature = "metal") {
    "metal"
} else {
    "cpu"
};

/// A single inference job sent from tokio → engine thread.
struct Job {
    system: String,
    prompt: String,
    max_tokens: i32,
    reply: std::sync::mpsc::SyncSender<JobResult>,
}

type JobResult = anyhow::Result<(String, adventurer_inference_llm::GenerateMetrics)>;

pub async fn run(opts: WorkerOpts) -> Result<()> {
    let WorkerOpts {
        model,
        n_ctx,
        gpu_layers,
    } = opts;
    let model_str = model.display().to_string();

    eprintln!(
        "[llm-worker] loading {} (n_ctx={}, gpu_layers={})",
        model_str, n_ctx, gpu_layers
    );

    // Spawn the engine thread. It loads the model, then loops on the channel.
    let (job_tx, job_rx) = mpsc::sync_channel::<Job>(0); // rendezvous — backpressure
    let (ready_tx, ready_rx) = mpsc::sync_channel::<anyhow::Result<()>>(1);
    let model_for_thread = model.clone();
    thread::Builder::new()
        .name("llm-engine".into())
        .spawn(move || {
            let engine = match LlmEngine::load(&model_for_thread, n_ctx, gpu_layers) {
                Ok(e) => {
                    let _ = ready_tx.send(Ok(()));
                    e
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Job loop. Channel close = exit.
            while let Ok(job) = job_rx.recv() {
                let result = engine.generate(&job.system, &job.prompt, job.max_tokens);
                let _ = job.reply.send(result);
            }
            eprintln!("[llm-worker] engine thread exiting");
        })
        .context("spawn engine thread")?;

    ready_rx
        .recv()
        .context("engine thread died before ready")??;
    eprintln!("[llm-worker] model loaded, ready");

    write_json(&Response::Ready {
        backend: BACKEND,
        model: &model_str,
        n_ctx,
        gpu_layers,
    })?;

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = stdin.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[llm-worker] bad request JSON: {e} | line: {line}");
                continue;
            }
        };
        match req {
            Request::Ping { id } => {
                write_json(&Response::Pong { id: &id })?;
            }
            Request::Shutdown { id } => {
                write_json(&Response::ShutdownAck { id: &id })?;
                eprintln!("[llm-worker] shutting down on request");
                break;
            }
            Request::Generate {
                id,
                system,
                prompt,
                max_tokens,
            } => {
                let (reply_tx, reply_rx) = mpsc::sync_channel::<JobResult>(1);
                if job_tx
                    .send(Job {
                        system,
                        prompt,
                        max_tokens,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    write_json(&Response::Error {
                        id: &id,
                        message: "engine thread died".into(),
                    })?;
                    break;
                }
                // The engine thread is doing work; we await its reply.
                // Block tokio thread briefly: the worker is single-purpose,
                // there's no other work to do during inference.
                let received = tokio::task::spawn_blocking(move || reply_rx.recv()).await?;
                match received {
                    Ok(Ok((text, m))) => write_json(&Response::Result_ {
                        id: &id,
                        text,
                        tokens: m.tokens_generated,
                        elapsed_secs: m.elapsed_secs,
                        tokens_per_sec: m.tokens_per_sec,
                    })?,
                    Ok(Err(e)) => write_json(&Response::Error {
                        id: &id,
                        message: format!("{e:#}"),
                    })?,
                    Err(_) => write_json(&Response::Error {
                        id: &id,
                        message: "engine thread dropped reply".into(),
                    })?,
                }
            }
        }
    }
    eprintln!("[llm-worker] stdin closed, exiting");
    drop(job_tx); // signals engine thread to exit
    Ok(())
}

fn write_json<T: Serialize>(v: &T) -> Result<()> {
    let line = serde_json::to_string(v)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}
