//! Connected players (mobile / browser clients that joined via QR).
//!
//! The DM runs the main display, players scan the QR shown in a modal and
//! land on `/join`. The player JS picks (or reads from localStorage) a token,
//! announces itself via `POST /api/players/announce`, and connects to the
//! WebSocket with `?role=player&token=…`.
//!
//! State per player:
//!   - token       — random opaque ID, stable per device
//!   - character   — slug assigned by the DM (None until the DM picks one)
//!   - first_seen  — timestamp
//!   - last_seen   — heartbeat from WS connect / message
//!
//! Day-2 simplification: the WS broadcasts ALL events to ALL clients; players
//! just render a stripped view and highlight their assigned character. Per-
//! player event filtering can come later if there's a real reason to.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct PlayerInfo {
    pub token: String,
    /// Slug into characters/* — set by DM via /api/players/{token}/assign.
    pub character: Option<String>,
    /// Optional friendly name from the player's device (browser hostname etc.).
    pub label: Option<String>,
    pub first_seen: chrono::DateTime<chrono::Utc>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Default)]
pub struct Players {
    inner: Arc<RwLock<BTreeMap<String, PlayerInfo>>>,
}

impl Players {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn touch(&self, token: &str, label: Option<String>) -> PlayerInfo {
        let now = chrono::Utc::now();
        let mut g = self.inner.write().await;
        let entry = g.entry(token.to_string()).or_insert_with(|| PlayerInfo {
            token: token.to_string(),
            character: None,
            label: label.clone(),
            first_seen: now,
            last_seen: now,
        });
        entry.last_seen = now;
        if entry.label.is_none() && label.is_some() {
            entry.label = label;
        }
        entry.clone()
    }

    pub async fn assign_character(
        &self,
        token: &str,
        character: Option<String>,
    ) -> Option<PlayerInfo> {
        let mut g = self.inner.write().await;
        let entry = g.get_mut(token)?;
        entry.character = character;
        Some(entry.clone())
    }

    pub async fn list(&self) -> Vec<PlayerInfo> {
        self.inner.read().await.values().cloned().collect()
    }

    pub async fn get(&self, token: &str) -> Option<PlayerInfo> {
        self.inner.read().await.get(token).cloned()
    }
}

#[derive(Deserialize)]
pub struct AnnouncePayload {
    pub token: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct AssignPayload {
    /// Slug to assign, or `null` to unassign.
    pub character: Option<String>,
}
