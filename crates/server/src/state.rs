//! Session state — the in-memory analog of `dnd-stage/session/`. Mirrors to disk
//! lazily so external tools (ddb_poll.py, manual edits) can still see/modify it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

/// One panel's markdown content keyed by name (scene, story-log, party, next-steps, map).
pub type Panels = BTreeMap<String, String>;

/// Whatever JSON the LLM produced for the state file. Free-form on purpose —
/// matches the existing dnd-stage shape but doesn't lock it down.
pub type StateJson = serde_json::Value;

/// Broadcast event types — same shape as `dnd-stage`'s WebSocket payloads
/// so the existing `client/stage.js` can consume them unchanged. New variants
/// are additive — older clients that don't recognize them just ignore.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Sent on initial WS connect — full snapshot.
    Init {
        panels: Panels,
        transcript: String,
        state: StateJson,
    },
    Transcript {
        content: String,
        tail: String,
    },
    Panels {
        data: Panels,
    },
    State {
        data: StateJson,
    },
    Decision {
        data: serde_json::Value,
    },
    /// A player connected (announced via /api/players/announce). DM UI uses this
    /// to show the "assign character" dropdown.
    PlayerJoined {
        player: crate::players::PlayerInfo,
    },
    /// A player got a character assigned (or unassigned) by the DM.
    PlayerAssigned {
        player: crate::players::PlayerInfo,
    },
    /// Live-reload signal — a watched asset on disk changed in dev mode.
    /// Browser listens and calls `location.reload()`. Production builds
    /// never emit this (no watcher spawned).
    DevReload {},
}

/// Cloneable handle. Internally a tower of Arc-wrapped concurrency primitives.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    pub session_dir: PathBuf,
    pub data: RwLock<SessionData>,
    pub events: broadcast::Sender<Event>,
}

#[derive(Default)]
pub struct SessionData {
    pub transcript: String,
    pub panels: Panels,
    pub state: StateJson,
    /// `"live"` or `"test"`. In test mode `/api/session/save` is refused so a
    /// quick mic / pipeline test doesn't pollute the GitHub content repo.
    pub session_mode: String,
    /// Stable per-session ID — `YYYY-MM-DD-HHMM` style, used as the GitHub
    /// data/sessions/<id>/ folder + as the prefix for chunk filenames so a
    /// crash mid-session leaves a self-describing archive on disk.
    pub session_id: String,
    /// Monotonic chunk counter — survives restarts because we reload from
    /// `/work/session/audio/` listing on startup.
    pub chunk_seq: u64,
}

impl AppState {
    pub fn new(session_dir: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        // Pre-create subdirs so the very first audio chunk has somewhere to land.
        let _ = std::fs::create_dir_all(session_dir.join("audio"));
        let _ = std::fs::create_dir_all(session_dir.join("panels"));
        // Resume chunk counter from existing audio/ contents if present.
        let chunk_seq = next_chunk_seq(&session_dir.join("audio"));
        // Resume session_id + transcript + state from disk if a prior crash
        // left them — never start with a blank slate when an archive exists.
        let (session_id, transcript, state, panels) = read_existing(&session_dir);
        Self {
            inner: Arc::new(Inner {
                session_dir,
                data: RwLock::new(SessionData {
                    transcript,
                    panels,
                    state,
                    session_mode: "live".to_string(),
                    session_id,
                    chunk_seq,
                }),
                events,
            }),
        }
    }

    pub async fn session_mode(&self) -> String {
        self.inner.data.read().await.session_mode.clone()
    }

    pub async fn set_session_mode(&self, mode: &str) {
        self.inner.data.write().await.session_mode = mode.to_string();
    }

    pub async fn session_id(&self) -> String {
        self.inner.data.read().await.session_id.clone()
    }

    /// Set the session ID. Idempotent. Used by /api/session/start and
    /// /api/session/load.
    pub async fn set_session_id(&self, id: String) {
        let mut g = self.inner.data.write().await;
        g.session_id = id;
        let _ = std::fs::write(
            self.inner.session_dir.join("session.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "session_id": g.session_id,
                "session_mode": g.session_mode,
            }))
            .unwrap_or_default(),
        );
    }

    /// Auto-stamp a session_id if blank. Called from /api/voice so an
    /// impromptu mic test still produces a coherent on-disk archive.
    pub async fn ensure_session_id(&self) -> String {
        let mut g = self.inner.data.write().await;
        if g.session_id.is_empty() {
            g.session_id = chrono::Local::now().format("%Y-%m-%d-%H%M").to_string();
        }
        g.session_id.clone()
    }

    /// Reserve a chunk number, return (seq, path). Caller writes audio bytes
    /// to that path. Never deletes anything.
    pub async fn next_chunk_path(&self, ext: &str) -> (u64, PathBuf) {
        let mut g = self.inner.data.write().await;
        g.chunk_seq += 1;
        let seq = g.chunk_seq;
        let fname = format!("chunk-{seq:06}.{ext}");
        let path = self.inner.session_dir.join("audio").join(fname);
        (seq, path)
    }

    /// Append one JSONL line to /work/session/raw-events.jsonl. Never errors
    /// to caller — best-effort, but if it fails we want to know in the log
    /// not in the request response.
    pub fn append_raw_event(&self, ev: serde_json::Value) {
        let path = self.inner.session_dir.join("raw-events.jsonl");
        let line = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
        if let Err(e) = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
            Ok(())
        })() {
            tracing::warn!(?e, path = %path.display(), "raw event log append failed");
        }
    }

    /// Persist the in-memory transcript to /work/session/transcript.md.
    /// Called after every successful append so the disk copy is always
    /// at least as fresh as the broadcast.
    pub fn mirror_transcript(&self, text: &str) {
        let path = self.inner.session_dir.join("transcript.md");
        if let Err(e) = std::fs::write(&path, text) {
            tracing::warn!(?e, "transcript mirror failed");
        }
    }

    /// Persist the JSON state to /work/session/state.json.
    pub fn mirror_state(&self, state: &StateJson) {
        let path = self.inner.session_dir.join("state.json");
        if let Ok(bytes) = serde_json::to_vec_pretty(state) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Persist all panels to /work/session/panels/<name>.md.
    pub fn mirror_panels(&self, panels: &Panels) {
        let dir = self.inner.session_dir.join("panels");
        let _ = std::fs::create_dir_all(&dir);
        for (name, body) in panels {
            let _ = std::fs::write(dir.join(format!("{name}.md")), body);
        }
    }

    /// Bulk replace state + panels + transcript (used by /api/session/load to
    /// seed from a pulled GitHub session). Mirrors to disk + broadcasts.
    pub async fn load_full(&self, transcript: String, state: StateJson, panels: Panels) {
        let snap = {
            let mut d = self.inner.data.write().await;
            d.transcript = transcript;
            d.state = state;
            d.panels = panels;
            (d.transcript.clone(), d.state.clone(), d.panels.clone())
        };
        self.mirror_transcript(&snap.0);
        self.mirror_state(&snap.1);
        self.mirror_panels(&snap.2);
        self.broadcast(Event::Init {
            panels: snap.2.clone(),
            transcript: snap.0.clone(),
            state: snap.1.clone(),
        });
    }

    pub fn session_dir(&self) -> &PathBuf {
        &self.inner.session_dir
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn broadcast(&self, ev: Event) {
        // ignore "no receivers" — that's normal during startup
        let _ = self.inner.events.send(ev);
    }

    pub async fn snapshot(&self) -> SnapshotData {
        let d = self.inner.data.read().await;
        SnapshotData {
            transcript: d.transcript.clone(),
            panels: d.panels.clone(),
            state: d.state.clone(),
        }
    }

    /// Append to transcript, return the tail (last N lines).
    pub async fn append_transcript(&self, line: &str) -> (String, String) {
        let mut d = self.inner.data.write().await;
        if !d.transcript.ends_with('\n') && !d.transcript.is_empty() {
            d.transcript.push('\n');
        }
        d.transcript.push_str(line);
        let lines: Vec<&str> = d.transcript.lines().collect();
        let tail_start = lines.len().saturating_sub(12);
        let tail: String = lines[tail_start..].join("\n");
        (d.transcript.clone(), tail)
    }

    pub async fn current_transcript(&self) -> String {
        self.inner.data.read().await.transcript.clone()
    }

    pub async fn set_state(&self, state: StateJson) {
        self.inner.data.write().await.state = state;
    }

    pub async fn merge_state(&self, incoming: &StateJson) {
        let mut d = self.inner.data.write().await;
        deep_merge(&mut d.state, incoming);
    }

    pub async fn set_panel(&self, name: &str, body: String) {
        self.inner
            .data
            .write()
            .await
            .panels
            .insert(name.into(), body);
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotData {
    pub transcript: String,
    pub panels: Panels,
    pub state: StateJson,
}

fn default_panels() -> Panels {
    let mut p = Panels::new();
    for name in ["scene", "story-log", "party", "next-steps", "map"] {
        p.insert(name.into(), format!("## PANEL: {name}\n\n*New session.*\n"));
    }
    p
}

/// Look at /work/session/audio/ and find the largest existing chunk-NNNNNN
/// number so we don't overwrite prior chunks after a server restart.
fn next_chunk_seq(audio_dir: &PathBuf) -> u64 {
    let mut max = 0u64;
    if let Ok(entries) = std::fs::read_dir(audio_dir) {
        for ent in entries.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            // chunk-NNNNNN.ext
            if let Some(rest) = name.strip_prefix("chunk-") {
                if let Some(num) = rest.split('.').next() {
                    if let Ok(n) = num.parse::<u64>() {
                        if n > max {
                            max = n;
                        }
                    }
                }
            }
        }
    }
    max
}

/// Resume from prior session.json + transcript.md + state.json + panels/.
fn read_existing(session_dir: &Path) -> (String, String, StateJson, Panels) {
    let session_id = std::fs::read_to_string(session_dir.join("session.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("session_id")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let transcript = std::fs::read_to_string(session_dir.join("transcript.md")).unwrap_or_default();
    let state = std::fs::read_to_string(session_dir.join("state.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut panels = default_panels();
    let panels_dir = session_dir.join("panels");
    if let Ok(entries) = std::fs::read_dir(&panels_dir) {
        for ent in entries.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".md") {
                if let Ok(body) = std::fs::read_to_string(ent.path()) {
                    panels.insert(stem.to_string(), body);
                }
            }
        }
    }
    (session_id, transcript, state, panels)
}

/// Merge `src` into `dst` recursively. Objects merge field-by-field; other
/// types replace. Mirrors the deep-merge `gemma.py::_update_state` does.
fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    use serde_json::Value;
    match (dst, src) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                deep_merge(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (slot, other) => {
            // Don't overwrite with null — preserves existing characters when
            // a state pass returns nothing for them.
            if !other.is_null() {
                *slot = other.clone();
            }
        }
    }
}
