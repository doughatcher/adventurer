//! Persistent settings: GitHub PAT + content-repo coordinates for session sync.
//!
//! Storage strategy (in priority order at load time):
//!   1. Env vars (`ADVENTURER_GITHUB_PAT`, `ADVENTURER_GITHUB_REPO`,
//!      `ADVENTURER_GITHUB_BRANCH`)
//!   2. JSON file at `$XDG_DATA_HOME/adventurer/config.json` (chmod 600)
//!   3. Defaults — repo unset (sync disabled until configured)
//!
//! The PAT is **never** echoed back through the API. `GET /api/config` returns
//! `{repo, branch, has_pat: bool}`; the secret stays put.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// `owner/repo` — e.g. `doughatcher/adventure-log`.
    #[serde(default)]
    pub repo: Option<String>,
    /// Branch to push to (default `main`).
    #[serde(default)]
    pub branch: Option<String>,
    /// Personal Access Token. Lives in memory + chmod-600 file. Never returned
    /// over the API.
    #[serde(default)]
    pub pat: Option<String>,
}

impl GitHubConfig {
    pub fn branch_or_main(&self) -> &str {
        self.branch.as_deref().unwrap_or("main")
    }
    pub fn is_ready(&self) -> bool {
        self.repo.is_some() && self.pat.is_some()
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<GitHubConfig>>,
    path: PathBuf,
}

impl ConfigStore {
    pub fn load() -> Self {
        let path = Self::default_path();
        let mut cfg: GitHubConfig = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => GitHubConfig::default(),
        };
        // Env overrides file (so launcher-set values win without rewriting file)
        if let Ok(v) = std::env::var("ADVENTURER_GITHUB_PAT") {
            cfg.pat = Some(v);
        }
        if let Ok(v) = std::env::var("ADVENTURER_GITHUB_REPO") {
            cfg.repo = Some(v);
        }
        if let Ok(v) = std::env::var("ADVENTURER_GITHUB_BRANCH") {
            cfg.branch = Some(v);
        }
        Self {
            inner: Arc::new(RwLock::new(cfg)),
            path,
        }
    }

    pub fn default_path() -> PathBuf {
        // $XDG_DATA_HOME or ~/.local/share, then adventurer/config.json
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
                PathBuf::from(home).join(".local/share")
            });
        base.join("adventurer").join("config.json")
    }

    pub async fn snapshot(&self) -> GitHubConfig {
        self.inner.read().await.clone()
    }

    /// Update fields. `pat` of `Some("")` clears it; `None` leaves it as-is.
    pub async fn update(
        &self,
        repo: Option<String>,
        branch: Option<String>,
        pat: Option<String>,
    ) -> Result<()> {
        let mut g = self.inner.write().await;
        if let Some(r) = repo {
            g.repo = if r.is_empty() { None } else { Some(r) };
        }
        if let Some(b) = branch {
            g.branch = if b.is_empty() { None } else { Some(b) };
        }
        if let Some(p) = pat {
            g.pat = if p.is_empty() { None } else { Some(p) };
        }
        // Persist (chmod 600 — secret on disk).
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(&*g)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("write {}", self.path.display()))?;
        chmod_600(&self.path).ok();
        Ok(())
    }
}

#[cfg(unix)]
fn chmod_600(path: &PathBuf) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn chmod_600(_path: &PathBuf) -> std::io::Result<()> {
    Ok(())
}
