//! Spawns + manages the LLM and STT worker child processes.
//!
//! Each worker is a long-running `adventurer-{llm,stt}-bench --worker ...`
//! subprocess. Communication is line-delimited JSON on stdin/stdout.
//! Requests carry a unique `id`; responses echo it back; a background reader
//! task dispatches by id to a oneshot waiting in `pending`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Counter for unique request IDs.
fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("r{}", N.fetch_add(1, Ordering::Relaxed))
}

/// Cheap unique-enough token for filenames (process-local). No `uuid` crate dep.
fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: AtomicU64 = AtomicU64::new(0);
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}-{:08x}", now_us, n)
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

/// One running worker process.
pub struct Worker {
    name: &'static str,
    _child: Child,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
}

impl Worker {
    /// Spawn a worker, wait for its `ready` line, return.
    pub async fn spawn(name: &'static str, program: &str, args: &[String]) -> Result<Arc<Self>> {
        info!(worker = name, program, ?args, "spawning worker");

        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // worker logs flow into our stderr
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {name} worker ({program})"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        // Background reader: parse each line, dispatch by id.
        let pending_for_reader = pending.clone();
        let name_for_reader = name;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            // First line = "ready" sentinel
            let ready_line = match lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => {
                    error!(worker = name_for_reader, "stdout closed before ready");
                    return;
                }
                Err(e) => {
                    error!(worker = name_for_reader, ?e, "stdout read error pre-ready");
                    return;
                }
            };
            info!(worker = name_for_reader, ready = %ready_line, "worker ready");
            // Subsequent lines = responses
            while let Ok(Some(line)) = lines.next_line().await {
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(worker = name_for_reader, ?e, line, "non-JSON output line");
                        continue;
                    }
                };
                let id = match v.get("id").and_then(|i| i.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        warn!(worker = name_for_reader, line, "response missing 'id'");
                        continue;
                    }
                };
                let mut pending = pending_for_reader.lock().await;
                if let Some(tx) = pending.remove(&id) {
                    let _ = tx.send(v);
                } else {
                    warn!(
                        worker = name_for_reader,
                        id, "response with no pending request"
                    );
                }
            }
            warn!(worker = name_for_reader, "worker stdout closed");
        });

        // Block until the in-band ready notification arrives. We can't easily
        // intercept just the first line above (it's already consumed). Instead
        // we do an out-of-band ping with a short timeout to verify the worker
        // is actually responsive.
        let worker = Arc::new(Self {
            name,
            _child: child,
            stdin: Mutex::new(stdin),
            pending,
        });

        match tokio::time::timeout(std::time::Duration::from_secs(120), worker.ping()).await {
            Ok(Ok(())) => info!(worker = name, "ping/pong OK — worker live"),
            Ok(Err(e)) => warn!(
                worker = name,
                ?e,
                "ping failed (worker may still be loading)"
            ),
            Err(_) => warn!(worker = name, "ping timed out at 120s — model load slow?"),
        }
        Ok(worker)
    }

    /// Send a request, await response.
    pub async fn request(&self, mut req: Value) -> Result<Value> {
        let id = next_id();
        if let Some(obj) = req.as_object_mut() {
            obj.insert("id".into(), Value::String(id.clone()));
        } else {
            return Err(anyhow!("request must be a JSON object"));
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        let line = serde_json::to_string(&req)?;
        debug!(worker = self.name, id, "→ request");
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
        let resp = rx.await.context("worker reply channel dropped")?;
        if let Some("error") = resp.get("type").and_then(|t| t.as_str()) {
            let msg = resp
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(no message)");
            return Err(anyhow!("{} worker error: {}", self.name, msg));
        }
        Ok(resp)
    }

    pub async fn ping(&self) -> Result<()> {
        let resp = self.request(serde_json::json!({"type": "ping"})).await?;
        match resp.get("type").and_then(|t| t.as_str()) {
            Some("pong") => Ok(()),
            other => Err(anyhow!("expected pong, got {other:?}")),
        }
    }
}

/// Convenience: typed wrappers over `Worker::request` for the LLM bench.
pub struct LlmWorker {
    inner: Arc<Worker>,
}

impl LlmWorker {
    pub async fn spawn(opts: LlmSpawnOpts) -> Result<Self> {
        let mut args = vec![
            "--worker".into(),
            "--model".into(),
            opts.model.display().to_string(),
            "--n-ctx".into(),
            opts.n_ctx.to_string(),
            "--gpu-layers".into(),
            opts.gpu_layers.to_string(),
        ];
        if let Some(extra) = opts.extra {
            args.extend(extra);
        }
        let inner = Worker::spawn("llm", &opts.program, &args).await?;
        Ok(Self { inner })
    }

    pub async fn generate(
        &self,
        system: &str,
        prompt: &str,
        max_tokens: i32,
    ) -> Result<GenerateResp> {
        let v = self
            .inner
            .request(serde_json::json!({
                "type": "generate",
                "system": system,
                "prompt": prompt,
                "max_tokens": max_tokens,
            }))
            .await?;
        Ok(serde_json::from_value(v)?)
    }
}

#[derive(Debug)]
pub struct LlmSpawnOpts {
    pub program: String, // path to adventurer-llm-bench
    pub model: PathBuf,
    pub n_ctx: u32,
    pub gpu_layers: u32,
    pub extra: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
pub struct GenerateResp {
    pub text: String,
    pub tokens: usize,
    pub elapsed_secs: f64,
    pub tokens_per_sec: f64,
}

/// STT wrapper.
pub struct SttWorker {
    inner: Arc<Worker>,
}

impl SttWorker {
    pub async fn spawn(opts: SttSpawnOpts) -> Result<Self> {
        let args = vec![
            "--worker".into(),
            "--model".into(),
            opts.model.display().to_string(),
            "--threads".into(),
            opts.threads.to_string(),
        ];
        let inner = Worker::spawn("stt", &opts.program, &args).await?;
        Ok(Self { inner })
    }

    /// Hand the worker a path on disk that the SERVER already wrote and
    /// owns. The worker reads but does NOT delete — caller is responsible
    /// for archive/cleanup policy. Used by /api/voice for never-delete
    /// audio archiving.
    pub async fn transcribe_path(
        &self,
        audio_path: &std::path::Path,
        format: &str,
        language: &str,
    ) -> Result<TranscribeResp> {
        let v = self
            .inner
            .request(serde_json::json!({
                "type": "transcribe",
                "audio_path": audio_path.display().to_string(),
                "format": format,
                "language": language,
                "delete_after": false,
            }))
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Legacy path: server writes a tempfile and asks the worker to delete it
    /// after reading. Kept for STT-bench / standalone use; the live server
    /// uses `transcribe_path` for never-delete archiving.
    pub async fn transcribe(
        &self,
        audio: &[u8],
        format: &str,
        language: &str,
    ) -> Result<TranscribeResp> {
        let ext = match format {
            "webm" => "webm",
            "ogg" | "opus" => "ogg",
            "mp4" | "m4a" => "m4a",
            "wav" => "wav",
            "mp3" | "mpeg" => "mp3",
            _ => "bin",
        };
        let tmp = std::env::temp_dir().join(format!("adv-stt-{}.{}", uuid_simple(), ext));
        tracing::info!(path = %tmp.display(), bytes = audio.len(), "stt: writing tempfile");
        tokio::fs::write(&tmp, audio).await?;
        tracing::info!("stt: sending request to worker");
        let v = self
            .inner
            .request(serde_json::json!({
                "type": "transcribe",
                "audio_path": tmp.display().to_string(),
                "format": format,
                "language": language,
                "delete_after": true,
            }))
            .await?;
        tracing::info!("stt: got worker response");
        Ok(serde_json::from_value(v)?)
    }
}

#[derive(Debug, Serialize)]
pub struct SttSpawnOpts {
    pub program: String,
    pub model: PathBuf,
    pub threads: i32,
}

#[derive(Deserialize, Debug)]
pub struct TranscribeResp {
    pub text: String,
    pub audio_secs: f64,
    pub elapsed_secs: f64,
    pub realtime_factor: f64,
}
