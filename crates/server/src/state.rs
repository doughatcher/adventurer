//! Session state — the in-memory analog of `dnd-stage/session/`. Mirrors to disk
//! lazily so external tools (ddb_poll.py, manual edits) can still see/modify it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

/// One panel's markdown content keyed by name (scene, story-log, party, next-steps, map).
pub type Panels = BTreeMap<String, String>;

/// Whatever JSON the LLM produced for the state file. Free-form on purpose —
/// matches the existing dnd-stage shape but doesn't lock it down.
pub type StateJson = serde_json::Value;

/// Broadcast event types — same shape as `dnd-stage`'s WebSocket payloads
/// so the existing `client/stage.js` can consume them unchanged.
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
}

impl AppState {
    pub fn new(session_dir: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Inner {
                session_dir,
                data: RwLock::new(SessionData {
                    transcript: String::new(),
                    panels: default_panels(),
                    state: serde_json::json!({}),
                }),
                events,
            }),
        }
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
        self.inner.data.write().await.panels.insert(name.into(), body);
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
