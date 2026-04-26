//! Worker mode for the STT bench. Mirrors crates/llm-bench/src/worker.rs.
//!
//! Protocol — line-delimited JSON on stdin/stdout:
//!
//!   in:  {"type":"transcribe", "id":..., "audio_b64":..., "format":"webm", "language":"en"}
//!   out: {"type":"result",     "id":..., "text":..., "audio_secs":..., "elapsed_secs":..., "realtime_factor":...}
//!
//! Audio payload is base64 (the bytes are typically <2 MB per chunk; whisper
//! decode dwarfs the IPC overhead). Decode happens via ffmpeg shellout per
//! chunk for now — same lazy path the bench uses.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use adventurer_inference_stt::SttEngine;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub struct WorkerOpts {
    pub model: PathBuf,
    pub threads: i32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    /// `audio_path` MUST be a file the worker can read. The server writes the
    /// chunk to a tempfile and sends the path here — keeps the JSON tiny so
    /// stdin pipes don't choke on multi-hundred-KB single-line payloads.
    /// Set `delete_after = true` and the worker removes the file post-decode.
    Transcribe {
        id: String,
        audio_path: PathBuf,
        #[serde(default = "default_format")]
        format: String,
        #[serde(default = "default_language")]
        language: String,
        #[serde(default)]
        delete_after: bool,
    },
    Ping {
        id: String,
    },
    Shutdown {
        #[serde(default)]
        id: String,
    },
}

fn default_format() -> String { "webm".into() }
fn default_language() -> String { "en".into() }

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response<'a> {
    Ready {
        backend: &'static str,
        model: &'a str,
    },
    Pong {
        id: &'a str,
    },
    Result_ {
        id: &'a str,
        text: String,
        audio_secs: f64,
        elapsed_secs: f64,
        realtime_factor: f64,
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

struct Job {
    pcm: Vec<f32>,
    language: String,
    reply: mpsc::SyncSender<JobResult>,
}

type JobResult =
    anyhow::Result<(String, adventurer_inference_stt::TranscribeMetrics)>;

pub async fn run(opts: WorkerOpts) -> Result<()> {
    let WorkerOpts { model, threads } = opts;
    let model_str = model.display().to_string();
    eprintln!("[stt-worker] loading {} (threads={})", model_str, threads);

    let (job_tx, job_rx) = mpsc::sync_channel::<Job>(0);
    let (ready_tx, ready_rx) = mpsc::sync_channel::<anyhow::Result<()>>(1);
    let model_for_thread = model.clone();
    thread::Builder::new()
        .name("stt-engine".into())
        .spawn(move || {
            let engine = match SttEngine::load(&model_for_thread) {
                Ok(e) => {
                    let _ = ready_tx.send(Ok(()));
                    e.with_threads(threads)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            while let Ok(job) = job_rx.recv() {
                let res = engine.transcribe(&job.pcm, &job.language);
                let _ = job.reply.send(res);
            }
            eprintln!("[stt-worker] engine thread exiting");
        })
        .context("spawn engine thread")?;

    ready_rx.recv().context("engine thread died before ready")??;
    eprintln!("[stt-worker] model loaded, ready");

    write_json(&Response::Ready {
        backend: BACKEND,
        model: &model_str,
    })?;

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = stdin.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[stt-worker] bad request JSON: {e}");
                continue;
            }
        };
        match req {
            Request::Ping { id } => write_json(&Response::Pong { id: &id })?,
            Request::Shutdown { id } => {
                write_json(&Response::ShutdownAck { id: &id })?;
                eprintln!("[stt-worker] shutting down on request");
                break;
            }
            Request::Transcribe {
                id,
                audio_path,
                format: _format,
                language,
                delete_after,
            } => {
                let pcm_result = ffmpeg_decode_pcm_from_path(&audio_path).await;
                if delete_after {
                    let _ = tokio::fs::remove_file(&audio_path).await;
                }
                let pcm = match pcm_result {
                    Ok(p) => p,
                    Err(e) => {
                        write_json(&Response::Error {
                            id: &id,
                            message: format!("ffmpeg decode: {e:#}"),
                        })?;
                        continue;
                    }
                };
                let (reply_tx, reply_rx) = mpsc::sync_channel::<JobResult>(1);
                if job_tx
                    .send(Job {
                        pcm,
                        language,
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
                let received = tokio::task::spawn_blocking(move || reply_rx.recv()).await?;
                match received {
                    Ok(Ok((text, m))) => write_json(&Response::Result_ {
                        id: &id,
                        text,
                        audio_secs: m.audio_secs,
                        elapsed_secs: m.elapsed_secs,
                        realtime_factor: m.realtime_factor,
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
    eprintln!("[stt-worker] stdin closed, exiting");
    drop(job_tx);
    Ok(())
}

/// ffmpeg shellout — reads from `audio_path` on disk, writes raw f32le PCM to
/// stdout. We deliberately do NOT pipe bytes through ffmpeg's stdin: when the
/// child process duplicates stdin/stdout fds internally (which ffmpeg does for
/// pipe:0 / pipe:1), my parent-side `read_to_end` never sees EOF and hangs
/// after ffmpeg has written all its output. Reading directly from a file
/// path sidesteps the whole class of pipe-fd-deadlock bugs.
async fn ffmpeg_decode_pcm_from_path(audio_path: &std::path::Path) -> Result<Vec<f32>> {
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-i", audio_path.to_str().ok_or_else(|| anyhow!("non-utf8 path"))?,
            "-ar", "16000",     // resample to 16 kHz
            "-ac", "1",         // mono
            "-f", "f32le",      // raw 32-bit float little-endian
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn ffmpeg (is it installed?)")?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no ffmpeg stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no ffmpeg stderr"))?;
    let mut bytes = Vec::with_capacity(16_000 * 4 * 30);
    let mut err = String::new();
    let (read_res, err_res) = tokio::join!(
        stdout.read_to_end(&mut bytes),
        stderr.read_to_string(&mut err),
    );
    read_res?;
    err_res?;
    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow!("ffmpeg failed: {status}\n{err}"));
    }
    if bytes.len() % 4 != 0 {
        return Err(anyhow!("ffmpeg PCM not a multiple of 4 bytes"));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// (Legacy: pipe-stdin variant, kept for the bench's old code path. Unused
/// by the worker now.)
#[allow(dead_code)]
async fn ffmpeg_decode_pcm(audio: &[u8], format: &str) -> Result<Vec<f32>> {
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", format,         // input format hint
            "-i", "pipe:0",       // input from stdin
            "-ar", "16000",       // resample to 16 kHz
            "-ac", "1",           // mono
            "-f", "f32le",        // raw 32-bit float little-endian
            "pipe:1",             // stdout
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn ffmpeg (is it installed?)")?;

    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no ffmpeg stdin"))?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no ffmpeg stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no ffmpeg stderr"))?;

    // Concurrently: feed bytes on stdin, drain PCM from stdout, capture stderr.
    // No tokio::spawn — that needs 'static and our `audio` is borrowed. join!
    // polls all three futures inline, which is exactly what we need.
    let mut bytes = Vec::with_capacity(16_000 * 4 * 30);
    let mut err = String::new();
    let writer = async {
        stdin.write_all(audio).await?;
        stdin.shutdown().await?;
        anyhow::Ok(())
    };
    let reader = async {
        stdout.read_to_end(&mut bytes).await?;
        anyhow::Ok(())
    };
    let stderr_drain = async {
        stderr.read_to_string(&mut err).await?;
        anyhow::Ok(())
    };
    let (write_res, read_res, stderr_res) = tokio::join!(writer, reader, stderr_drain);
    write_res?;
    read_res?;
    stderr_res?;

    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow!("ffmpeg failed: {status}\n{err}"));
    }
    if bytes.len() % 4 != 0 {
        return Err(anyhow!("ffmpeg PCM not a multiple of 4 bytes"));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn write_json<T: Serialize>(v: &T) -> Result<()> {
    let line = serde_json::to_string(v)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}
