//! GitHub content-repo sync via the REST Trees API.
//!
//! On `push_session`:
//!   1. GET ref → current branch HEAD SHA
//!   2. GET commit → tree SHA of the parent commit
//!   3. POST blobs (one per file) → blob SHAs
//!   4. POST tree (new tree pointing parent_tree + new blobs at the right paths)
//!   5. POST commit (parent = HEAD, tree = new tree)
//!   6. PATCH ref → point branch at the new commit
//!
//! That's six API calls but produces ONE atomic commit containing every file
//! in `data/sessions/YYYY-MM-DD-HHMM/`. Same shape as `dnd-stage`'s
//! `git commit -m "Session archive"`.
//!
//! Only the REST API is used — no `git` binary at runtime. PAT must have
//! `contents: write` permission on the repo.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug)]
pub struct GitHubBackend {
    pub repo: String,    // "owner/repo"
    pub branch: String,
    pub pat: String,
}

#[derive(Debug, Serialize)]
pub struct PushFile {
    /// Repo-relative path, e.g. `data/sessions/2026-04-26-1530/transcript.md`
    pub path: String,
    /// File contents as bytes (base64-encoded for the API call internally).
    #[serde(skip)]
    pub content: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct PushResult {
    pub commit_sha: String,
    pub commit_url: String,
    pub files: usize,
}

const UA: &str = concat!("adventurer/", env!("CARGO_PKG_VERSION"));

/// What we can pull back from a previously-saved session in the content repo.
#[derive(Debug, Clone, Serialize)]
pub struct LoadedSession {
    pub id: String,
    pub transcript: String,
    pub state: serde_json::Value,
    pub panels: std::collections::BTreeMap<String, String>,
}

impl GitHubBackend {
    fn client(&self) -> Result<reqwest::Client> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.pat))?,
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            reqwest::header::HeaderValue::from_static("2022-11-28"),
        );
        Ok(reqwest::Client::builder()
            .user_agent(UA)
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?)
    }

    fn url(&self, path: &str) -> String {
        format!("https://api.github.com/repos/{}/{}", self.repo, path.trim_start_matches('/'))
    }

    pub async fn push_session(&self, message: &str, files: &[PushFile]) -> Result<PushResult> {
        if files.is_empty() {
            bail!("nothing to push");
        }
        let client = self.client()?;

        // 1. branch HEAD
        let head_sha = self.get_ref(&client).await?;

        // 2. parent tree
        let parent_tree = self.get_commit_tree(&client, &head_sha).await?;

        // 3. blobs
        let mut tree_entries = Vec::with_capacity(files.len());
        for f in files {
            let sha = self.create_blob(&client, &f.content).await
                .with_context(|| format!("blob for {}", f.path))?;
            tree_entries.push(json!({
                "path": f.path,
                "mode": "100644",
                "type": "blob",
                "sha": sha,
            }));
        }

        // 4. tree
        let new_tree_sha = self.create_tree(&client, &parent_tree, &tree_entries).await?;

        // 5. commit
        let new_commit_sha = self.create_commit(&client, message, &new_tree_sha, &head_sha).await?;

        // 6. ref update
        self.update_ref(&client, &new_commit_sha).await?;

        Ok(PushResult {
            commit_url: format!("https://github.com/{}/commit/{}", self.repo, new_commit_sha),
            commit_sha: new_commit_sha,
            files: files.len(),
        })
    }

    // ─── individual REST steps ───

    async fn get_ref(&self, c: &reqwest::Client) -> Result<String> {
        #[derive(Deserialize)]
        struct R { object: ObjRef }
        #[derive(Deserialize)]
        struct ObjRef { sha: String }
        let url = self.url(&format!("git/ref/heads/{}", self.branch));
        let r: R = api_json(c.get(&url)).await
            .with_context(|| format!("get ref {url}"))?;
        Ok(r.object.sha)
    }

    async fn get_commit_tree(&self, c: &reqwest::Client, commit_sha: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct R { tree: TreeRef }
        #[derive(Deserialize)]
        struct TreeRef { sha: String }
        let url = self.url(&format!("git/commits/{commit_sha}"));
        let r: R = api_json(c.get(&url)).await
            .with_context(|| format!("get commit {commit_sha}"))?;
        Ok(r.tree.sha)
    }

    async fn create_blob(&self, c: &reqwest::Client, content: &[u8]) -> Result<String> {
        #[derive(Deserialize)]
        struct R { sha: String }
        let body = json!({
            "encoding": "base64",
            "content": base64::engine::general_purpose::STANDARD.encode(content),
        });
        let url = self.url("git/blobs");
        let r: R = api_json(c.post(&url).json(&body)).await
            .context("create blob")?;
        Ok(r.sha)
    }

    async fn create_tree(
        &self,
        c: &reqwest::Client,
        base_tree: &str,
        entries: &[serde_json::Value],
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct R { sha: String }
        let body = json!({ "base_tree": base_tree, "tree": entries });
        let url = self.url("git/trees");
        let r: R = api_json(c.post(&url).json(&body)).await
            .context("create tree")?;
        Ok(r.sha)
    }

    async fn create_commit(
        &self,
        c: &reqwest::Client,
        message: &str,
        tree_sha: &str,
        parent_sha: &str,
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct R { sha: String }
        let body = json!({
            "message": message,
            "tree":    tree_sha,
            "parents": [parent_sha],
        });
        let url = self.url("git/commits");
        let r: R = api_json(c.post(&url).json(&body)).await
            .context("create commit")?;
        Ok(r.sha)
    }

    async fn update_ref(&self, c: &reqwest::Client, commit_sha: &str) -> Result<()> {
        let body = json!({ "sha": commit_sha, "force": false });
        let url = self.url(&format!("git/refs/heads/{}", self.branch));
        let resp = c.patch(&url).json(&body).send().await
            .context("PATCH ref")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("PATCH {url} → {status}: {text}"));
        }
        Ok(())
    }

    // ─── reading: list + load past sessions from data/sessions/ ───

    /// List every session folder under `data/sessions/`. Returns IDs sorted
    /// newest-first (assumes the `YYYY-MM-DD-HHMM` naming convention used by
    /// dnd-stage and by our own `/api/session/save`).
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Entry { name: String, #[serde(rename = "type")] kind: String }
        let client = self.client()?;
        let url = self.url("contents/data/sessions");
        let entries: Vec<Entry> = api_json(client.get(&url)).await
            .with_context(|| format!("GET {url}"))?;
        let mut ids: Vec<String> = entries.into_iter()
            .filter(|e| e.kind == "dir")
            .map(|e| e.name)
            .collect();
        ids.sort_by(|a, b| b.cmp(a));   // newest first
        Ok(ids)
    }

    /// Pull `data/sessions/<id>/{transcript.md, state.json, *.md}` into a
    /// `LoadedSession`. Files that don't exist in the repo are skipped (the
    /// session may pre-date a particular panel).
    pub async fn load_session(&self, id: &str) -> Result<LoadedSession> {
        #[derive(Deserialize)]
        struct ContentsEntry { name: String, #[serde(rename = "type")] kind: String }
        let client = self.client()?;
        let dir_url = self.url(&format!("contents/data/sessions/{id}"));
        let files: Vec<ContentsEntry> = api_json(client.get(&dir_url)).await
            .with_context(|| format!("GET {dir_url}"))?;

        let mut transcript = String::new();
        let mut state = serde_json::json!({});
        let mut panels = std::collections::BTreeMap::new();

        for f in files.iter().filter(|f| f.kind == "file") {
            let raw = self.fetch_file(&client, id, &f.name).await?;
            match f.name.as_str() {
                "transcript.md" => {
                    transcript = String::from_utf8_lossy(&raw).into_owned();
                }
                "state.json" => {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                        state = v;
                    }
                }
                name if name.ends_with(".md") => {
                    let stem = name.trim_end_matches(".md");
                    panels.insert(stem.to_string(), String::from_utf8_lossy(&raw).into_owned());
                }
                _ => {}
            }
        }

        Ok(LoadedSession { id: id.to_string(), transcript, state, panels })
    }

    async fn fetch_file(&self, c: &reqwest::Client, session_id: &str, name: &str) -> Result<Vec<u8>> {
        #[derive(Deserialize)]
        struct R { content: String, encoding: String }
        let url = self.url(&format!("contents/data/sessions/{session_id}/{name}"));
        let r: R = api_json(c.get(&url)).await
            .with_context(|| format!("GET {url}"))?;
        if r.encoding != "base64" {
            bail!("unexpected encoding {} for {url}", r.encoding);
        }
        let stripped: String = r.content.chars().filter(|c| !c.is_whitespace()).collect();
        Ok(base64::engine::general_purpose::STANDARD.decode(stripped)?)
    }
}

async fn api_json<T: serde::de::DeserializeOwned>(rb: reqwest::RequestBuilder) -> Result<T> {
    let resp = rb.send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{status}: {text}"));
    }
    serde_json::from_str(&text).with_context(|| format!("decode response: {text}"))
}
