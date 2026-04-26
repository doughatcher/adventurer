//! adventurer-stt-bench: A/B Whisper transcription.
//!
//! Same audio fixture, two backends:
//!   --speaches → POST to http://localhost:8000/v1/audio/transcriptions
//!   default    → adventurer-inference (in-process whisper-rs)

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

mod worker;

use adventurer_inference_stt::SttEngine;
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "PoC bench: STT via in-process whisper.cpp vs Speaches HTTP"
)]
struct Args {
    /// Path to a whisper.cpp GGML model file.
    #[arg(long, default_value = "models/ggml-medium.bin")]
    model: PathBuf,

    /// Path to an audio file (mp3, webm, wav, anything ffmpeg can decode).
    #[arg(long, default_value = "samples/audio/clip.mp3")]
    audio: PathBuf,

    /// Hit local Speaches instead of in-process inference (for A/B comparison).
    #[arg(long)]
    speaches: bool,

    /// Worker mode: load model, then read line-delimited JSON requests from stdin
    /// and write responses to stdout. Used by the adventurer server.
    #[arg(long)]
    worker: bool,

    #[arg(long, default_value = "http://localhost:8000")]
    speaches_base: String,

    #[arg(long, default_value = "Systran/faster-whisper-medium")]
    speaches_model: String,

    #[arg(long, default_value = "en")]
    language: String,

    #[arg(long, default_value_t = 8)]
    threads: i32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,adventurer_inference=info")
            }),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    if args.worker {
        return worker::run(worker::WorkerOpts {
            model: args.model.clone(),
            threads: args.threads,
        })
        .await;
    }

    eprintln!("─── adventurer-stt-bench ───");
    eprintln!("audio:   {}", args.audio.display());

    let raw_bytes = tokio::fs::read(&args.audio)
        .await
        .with_context(|| format!("read audio file {}", args.audio.display()))?;
    eprintln!("audio:   {} bytes on disk", raw_bytes.len());

    let (transcript, audio_secs, elapsed_secs) = if args.speaches {
        eprintln!(
            "backend: speaches @ {} ({})",
            args.speaches_base, args.speaches_model
        );
        run_speaches(&args, &args.audio, &raw_bytes).await?
    } else {
        eprintln!("backend: in-process whisper-rs");
        eprintln!("model:   {}", args.model.display());
        let pcm = ffmpeg_decode_pcm(&args.audio).await?;
        let secs = pcm.len() as f64 / 16_000.0;
        eprintln!(
            "decoded: {} samples = {:.1}s of audio at 16 kHz mono f32",
            pcm.len(),
            secs
        );
        let model = args.model.clone();
        let lang = args.language.clone();
        let threads = args.threads;
        let (text, elapsed) = tokio::task::spawn_blocking(move || -> Result<_> {
            let engine = SttEngine::load(&model)?.with_threads(threads);
            let (text, m) = engine.transcribe(&pcm, &lang)?;
            Ok((text, m.elapsed_secs))
        })
        .await??;
        (text, secs, elapsed)
    };

    eprintln!("─── result ───");
    let realtime_factor = audio_secs / elapsed_secs.max(0.001);
    eprintln!(
        "transcribed {:.1}s of audio in {:.2}s (realtime ×{:.1})",
        audio_secs, elapsed_secs, realtime_factor
    );
    println!("\n{}\n", transcript.trim());

    validate(&transcript);
    Ok(())
}

async fn ffmpeg_decode_pcm(audio_path: &PathBuf) -> Result<Vec<f32>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            audio_path
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 path"))?,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-f",
            "f32le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn ffmpeg (is it installed?)")?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no ffmpeg stdout"))?;
    let mut bytes = Vec::with_capacity(16_000 * 4 * 60);
    stdout.read_to_end(&mut bytes).await?;
    let status = child.wait().await?;
    if !status.success() {
        let mut err = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut err).await;
        }
        return Err(anyhow!("ffmpeg failed: {status}\n{err}"));
    }
    if bytes.len() % 4 != 0 {
        return Err(anyhow!("ffmpeg output not a multiple of 4 bytes"));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

async fn run_speaches(
    args: &Args,
    audio_path: &PathBuf,
    raw_bytes: &[u8],
) -> Result<(String, f64, f64)> {
    let audio_secs = ffprobe_duration(audio_path).await.unwrap_or(0.0);
    let filename = audio_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio")
        .to_string();
    let mime = mime_for(&filename);

    let part = reqwest::multipart::Part::bytes(raw_bytes.to_vec())
        .file_name(filename)
        .mime_str(mime)?;
    let form = reqwest::multipart::Form::new()
        .text("model", args.speaches_model.clone())
        .text("response_format", "json")
        .text("language", args.language.clone())
        .part("file", part);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let t = Instant::now();
    let body: serde_json::Value = client
        .post(format!("{}/v1/audio/transcriptions", args.speaches_base))
        .multipart(form)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let elapsed = t.elapsed().as_secs_f64();

    let text = body
        .get("text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("speaches response missing 'text': {body}"))?;
    Ok((text, audio_secs, elapsed))
}

async fn ffprobe_duration(audio_path: &PathBuf) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
            audio_path
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 path"))?,
        ])
        .output()
        .await?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse()?)
}

fn mime_for(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "webm" => "audio/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

fn validate(text: &str) {
    eprintln!("─── validation ───");
    let trimmed = text.trim();
    let chars = trimmed.chars().count();
    let words = trimmed.split_whitespace().count();
    if chars == 0 {
        eprintln!("✗ empty transcript");
    } else if words < 5 {
        eprintln!("⚠ very short transcript ({words} words, {chars} chars)");
    } else {
        eprintln!("✓ transcript: {words} words, {chars} chars");
    }
}
