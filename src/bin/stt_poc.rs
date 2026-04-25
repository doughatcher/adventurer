//! adventurer-stt-poc: A/B test whisper.cpp state extraction.
//!
//! Same audio fixture, two backends:
//!   --speaches → POST to http://localhost:8000/v1/audio/transcriptions (the dnd-stage path)
//!   default    → whisper-rs in-process inference
//!
//! Goal: confirm in-process whisper transcription gives same-or-better text than the
//! existing Speaches integration, before committing to the full Rust port.
//!
//! Audio decode is currently a lazy ffmpeg subprocess shellout (mp3/webm → 16 kHz mono f32 PCM).
//! The production binary will use `symphonia` for pure-Rust decoding — out of scope here.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Parser, Debug)]
#[command(version, about = "PoC: STT via in-process whisper.cpp vs Speaches HTTP")]
struct Args {
    /// Path to a whisper.cpp GGML model file. Default: ggml-medium.bin (matches Speaches).
    #[arg(long, default_value = "models/ggml-medium.bin")]
    model: PathBuf,

    /// Path to an audio file (mp3, webm, wav, anything ffmpeg can decode).
    #[arg(long, default_value = "samples/audio/clip.mp3")]
    audio: PathBuf,

    /// Hit local Speaches instead of in-process inference (for A/B comparison).
    #[arg(long)]
    speaches: bool,

    /// Speaches base URL.
    #[arg(long, default_value = "http://localhost:8000")]
    speaches_base: String,

    /// Speaches model tag (matches dnd-stage/server/stt.py).
    #[arg(long, default_value = "Systran/faster-whisper-medium")]
    speaches_model: String,

    /// Language hint for whisper (en, auto, etc.).
    #[arg(long, default_value = "en")]
    language: String,

    /// CPU threads (whisper-rs only — Speaches manages its own).
    #[arg(long, default_value_t = 8)]
    threads: i32,

    /// GPU layers (whisper.cpp uses GPU automatically when built with cuda/vulkan/metal;
    /// this flag is here for symmetry with adventurer-poc but currently no-op).
    #[arg(long, default_value_t = 0)]
    gpu_layers: i32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("─── adventurer-stt-poc ───");
    eprintln!("audio:   {}", args.audio.display());

    // Decode audio once (both paths use it; Speaches gets the original bytes,
    // whisper-rs gets the decoded PCM).
    let raw_bytes = tokio::fs::read(&args.audio)
        .await
        .with_context(|| format!("read audio file {}", args.audio.display()))?;
    eprintln!("audio:   {} bytes on disk", raw_bytes.len());

    let (transcript, audio_secs, elapsed_secs) = if args.speaches {
        eprintln!("backend: speaches @ {} ({})", args.speaches_base, args.speaches_model);
        run_speaches(&args, &args.audio, &raw_bytes).await?
    } else {
        eprintln!("backend: whisper-rs (in-process)");
        eprintln!("model:   {}", args.model.display());
        let pcm = ffmpeg_decode_pcm(&args.audio).await?;
        let secs = pcm.len() as f64 / 16_000.0;
        eprintln!("decoded: {} samples = {:.1}s of audio at 16 kHz mono f32", pcm.len(), secs);
        let model = args.model.clone();
        let lang = args.language.clone();
        let threads = args.threads;
        let (text, elapsed) = tokio::task::spawn_blocking(move || {
            run_local(&model, &lang, threads, &pcm)
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

// ─── ffmpeg decode helper ───
//
// Lazy: shell out to ffmpeg, request 16 kHz mono f32 little-endian PCM on stdout.
// The production binary will swap this for `symphonia` so the .exe has zero
// runtime dependencies. For the PoC, ffmpeg is the same dependency dnd-stage
// already shells out to for chunk concat → MP3.

async fn ffmpeg_decode_pcm(audio_path: &PathBuf) -> Result<Vec<f32>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel", "error",
            "-i", audio_path.to_str().ok_or_else(|| anyhow!("non-utf8 path"))?,
            "-ar", "16000",      // resample to 16 kHz
            "-ac", "1",          // downmix to mono
            "-f", "f32le",       // raw 32-bit float little-endian
            "-",                 // stdout
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn ffmpeg (is it installed?)")?;

    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no ffmpeg stdout"))?;
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
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Ok(samples)
}

// ─── whisper-rs path ───

fn run_local(model_path: &PathBuf, language: &str, threads: i32, pcm: &[f32]) -> Result<(String, f64)> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let t_load = Instant::now();
    let ctx = WhisperContext::new_with_params(
        model_path.to_str().ok_or_else(|| anyhow!("non-utf8 model path"))?,
        WhisperContextParameters::default(),
    )
    .with_context(|| format!("load whisper model {}", model_path.display()))?;
    eprintln!("model loaded in {:.2}s", t_load.elapsed().as_secs_f64());

    let mut state = ctx.create_state().context("create whisper state")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads);
    params.set_translate(false);
    params.set_language(Some(language));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    let t_run = Instant::now();
    state.full(params, pcm).context("whisper full() decode")?;
    let elapsed = t_run.elapsed().as_secs_f64();

    // whisper-rs 0.16: full_n_segments returns c_int directly,
    // get_segment(i) returns Option<WhisperSegment>, segment.to_str() returns Result<&str>.
    let n_segments = state.full_n_segments();
    let mut text = String::new();
    for i in 0..n_segments {
        let segment = state
            .get_segment(i)
            .ok_or_else(|| anyhow!("segment {i} out of bounds"))?;
        let seg_text = segment
            .to_str()
            .with_context(|| format!("segment {i} to_str"))?;
        text.push_str(seg_text);
    }
    Ok((text, elapsed))
}

// ─── Speaches path (A/B baseline) ───

async fn run_speaches(args: &Args, audio_path: &PathBuf, raw_bytes: &[u8]) -> Result<(String, f64, f64)> {
    // Detect duration via ffprobe so we can compute the realtime factor consistently.
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

    let t0 = Instant::now();
    let resp = client
        .post(format!("{}/v1/audio/transcriptions", args.speaches_base))
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = resp.json().await?;
    let elapsed = t0.elapsed().as_secs_f64();

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
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=nw=1:nk=1",
            audio_path.to_str().ok_or_else(|| anyhow!("non-utf8 path"))?,
        ])
        .output()
        .await?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe failed"));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(s.parse()?)
}

fn mime_for(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "webm" => "audio/webm",
        "mp3"  => "audio/mpeg",
        "wav"  => "audio/wav",
        "ogg" | "opus" => "audio/ogg",
        "m4a"  => "audio/mp4",
        "flac" => "audio/flac",
        _      => "application/octet-stream",
    }
}

// ─── output validation ───

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
