//! REST routes — port of `dnd-stage/server/main.py`'s `/api/*` endpoints.
//!
//! For Day 2 we cover what the existing UI actually exercises in the live
//! recording loop:
//!   GET  /api/panels       — return all panel markdown
//!   GET  /api/transcript   — {content, tail}
//!   GET  /api/state        — current state JSON
//!   POST /api/voice        — multipart audio chunk → STT → append → broadcast
//!   POST /api/update       — manually trigger an LLM state pass
//!
//! Stubbed for later (still return 200 to keep the UI from erroring):
//!   GET/POST/PATCH /api/characters[/:slug]
//!   POST /api/session/end
//!   GET  /api/sessions[/:ts]
//!   GET  /api/recording/*

use std::net::IpAddr;
use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::ConfigStore;
use crate::lan;
use crate::players::{AnnouncePayload, AssignPayload, Players};
use crate::state::{AppState, Event};
use crate::sync::{GitHubBackend, PushFile};
use crate::workers::{LlmWorker, SttWorker};

pub struct AppContext {
    pub state: AppState,
    pub stt: Arc<SttWorker>,
    pub llm: Arc<LlmWorker>,
    pub players: Players,
    pub lan_ip: IpAddr,
    pub port: u16,
    pub config: ConfigStore,
    pub trigger_state_pass: tokio::sync::mpsc::UnboundedSender<()>,
    pub trigger_panel_pass: tokio::sync::mpsc::UnboundedSender<()>,
}

pub type Ctx = State<Arc<AppContext>>;

pub async fn get_panels(State(ctx): Ctx) -> impl IntoResponse {
    let snap = ctx.state.snapshot().await;
    Json(snap.panels)
}

pub async fn get_transcript(State(ctx): Ctx) -> impl IntoResponse {
    let content = ctx.state.current_transcript().await;
    let lines: Vec<&str> = content.lines().collect();
    let tail_start = lines.len().saturating_sub(8);
    let tail: String = lines[tail_start..].join("\n");
    Json(json!({ "content": content, "tail": tail }))
}

pub async fn get_state(State(ctx): Ctx) -> impl IntoResponse {
    let snap = ctx.state.snapshot().await;
    Json(snap.state)
}

/// Multipart audio upload → STT worker → append to transcript → broadcast.
pub async fn post_voice(State(ctx): Ctx, mut multipart: Multipart) -> impl IntoResponse {
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut content_type = String::from("audio/webm");
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("audio") {
            content_type = field
                .content_type()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "audio/webm".into());
            match field.bytes().await {
                Ok(b) => audio_bytes = Some(b.to_vec()),
                Err(e) => {
                    warn!(?e, "voice: failed to read audio bytes");
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "ok": false, "error": format!("{e}") })),
                    );
                }
            }
        }
    }
    let Some(audio) = audio_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing 'audio' part" })),
        );
    };
    // Map MIME → ffmpeg format hint
    let format = mime_to_ffmpeg_format(&content_type);
    info!(
        bytes = audio.len(),
        mime = %content_type,
        ffmpeg_format = format,
        "voice: transcribing"
    );

    match ctx.stt.transcribe(&audio, format, "en").await {
        Ok(resp) => {
            let text = resp.text.trim().to_string();
            if text.is_empty() {
                return (StatusCode::OK, Json(json!({ "ok": false, "text": "" })));
            }
            // dnd-stage's append_to_transcript prepends a wall-clock timestamp.
            let ts = chrono::Local::now().format("%H:%M:%S");
            let line = format!("\n**[{ts}]** {text}");
            let (full, tail) = ctx.state.append_transcript(&line).await;
            ctx.state.broadcast(Event::Transcript {
                content: full,
                tail,
            });
            // Nudge the gemma loops — they'll debounce.
            let _ = ctx.trigger_state_pass.send(());
            let _ = ctx.trigger_panel_pass.send(());
            (StatusCode::OK, Json(json!({ "ok": true, "text": text })))
        }
        Err(e) => {
            warn!(?e, "stt worker error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}") })),
            )
        }
    }
}

pub async fn post_update(State(ctx): Ctx) -> impl IntoResponse {
    let _ = ctx.trigger_state_pass.send(());
    let _ = ctx.trigger_panel_pass.send(());
    Json(json!({ "ok": true, "message": "Update triggered" }))
}

// ─── Stubs (return shape the UI expects, no real work yet) ───

pub async fn list_characters(State(_ctx): Ctx) -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

#[derive(Deserialize)]
pub struct CharacterIn {
    name: String,
    #[serde(default)]
    char_class: String,
    #[serde(default)]
    hp_current: i64,
    #[serde(default)]
    hp_max: i64,
    #[serde(default)]
    ac: i64,
    #[serde(default)]
    notes: String,
}

pub async fn add_character(State(_ctx): Ctx, Json(c): Json<CharacterIn>) -> impl IntoResponse {
    let slug = slugify(&c.name);
    Json(json!({ "ok": true, "slug": slug }))
}

pub async fn patch_character(State(_ctx): Ctx, Path(slug): Path<String>, Json(_v): Json<Value>) -> impl IntoResponse {
    Json(json!({ "ok": true, "slug": slug }))
}

pub async fn list_sessions(State(_ctx): Ctx) -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

pub async fn get_session(State(_ctx): Ctx, Path(ts): Path<String>) -> impl IntoResponse {
    Json(json!({ "ts": ts, "stub": true }))
}

pub async fn end_session(State(_ctx): Ctx) -> impl IntoResponse {
    Json(json!({ "ok": true, "stub": true }))
}

// ─── LAN / QR / players ───

pub async fn get_lan_info(State(ctx): Ctx) -> impl IntoResponse {
    let url = lan::join_url(ctx.lan_ip, ctx.port);
    let qr = match lan::qr_svg(&url) {
        Ok(svg) => svg,
        Err(e) => {
            warn!(?e, "qr render failed");
            String::new()
        }
    };
    Json(json!({
        "lan_ip":  ctx.lan_ip.to_string(),
        "port":    ctx.port,
        "join_url": url,
        "qr_svg":  qr,
    }))
}

pub async fn list_players(State(ctx): Ctx) -> impl IntoResponse {
    Json(ctx.players.list().await)
}

pub async fn announce_player(
    State(ctx): Ctx,
    Json(p): Json<AnnouncePayload>,
) -> impl IntoResponse {
    let info = ctx.players.touch(&p.token, p.label).await;
    info!(token = %info.token, label = ?info.label, "player announced");
    ctx.state.broadcast(Event::PlayerJoined { player: info.clone() });
    Json(info)
}

pub async fn assign_player_character(
    State(ctx): Ctx,
    Path(token): Path<String>,
    Json(p): Json<AssignPayload>,
) -> impl IntoResponse {
    match ctx.players.assign_character(&token, p.character.clone()).await {
        Some(info) => {
            info!(token = %token, character = ?p.character, "player assigned character");
            ctx.state.broadcast(Event::PlayerAssigned { player: info.clone() });
            Json(json!({ "ok": true, "player": info }))
        }
        None => Json(json!({ "ok": false, "error": "unknown player token" })),
    }
}

// ─── GitHub sync ───

#[derive(Deserialize)]
pub struct ConfigPatch {
    /// `owner/repo`
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// PAT — accept-only. Never returned.
    #[serde(default)]
    pub pat: Option<String>,
}

/// Returns `{repo, branch, has_pat}`. PAT itself is **never** sent back.
pub async fn get_config(State(ctx): Ctx) -> impl IntoResponse {
    let cfg = ctx.config.snapshot().await;
    Json(json!({
        "repo":   cfg.repo,
        "branch": cfg.branch_or_main(),
        "has_pat": cfg.pat.is_some(),
    }))
}

pub async fn set_config(State(ctx): Ctx, Json(p): Json<ConfigPatch>) -> impl IntoResponse {
    if let Err(e) = ctx.config.update(p.repo, p.branch, p.pat).await {
        warn!(?e, "config persist failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("{e:#}") })),
        );
    }
    let cfg = ctx.config.snapshot().await;
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "repo":    cfg.repo,
            "branch":  cfg.branch_or_main(),
            "has_pat": cfg.pat.is_some(),
        })),
    )
}

#[derive(Deserialize, Default)]
pub struct SaveSessionPayload {
    /// Optional — defaults to current `YYYY-MM-DD-HHMM`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional commit message — defaults to "Session archive: <id>".
    #[serde(default)]
    pub message: Option<String>,
}

/// Snapshot the current session into a fresh `data/sessions/<id>/` folder
/// and push it as a single atomic commit to the configured GitHub repo.
pub async fn save_session(
    State(ctx): Ctx,
    Json(p): Json<SaveSessionPayload>,
) -> impl IntoResponse {
    let cfg = ctx.config.snapshot().await;
    if !cfg.is_ready() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "GitHub repo + PAT not configured (POST /api/config)",
            })),
        );
    }
    let session_id = p.session_id.unwrap_or_else(|| {
        chrono::Local::now().format("%Y-%m-%d-%H%M").to_string()
    });
    let message = p.message.unwrap_or_else(|| format!("Session archive: {session_id}"));
    // Extract first — `branch_or_main()` borrows `cfg`, so we can't call it
    // after `unwrap()`-ing other fields out.
    let branch = cfg.branch_or_main().to_string();
    let backend = GitHubBackend {
        repo:   cfg.repo.expect("checked is_ready above"),
        branch,
        pat:    cfg.pat.expect("checked is_ready above"),
    };

    // Snapshot session content into PushFiles.
    let snap = ctx.state.snapshot().await;
    let prefix = format!("data/sessions/{session_id}");

    let mut files: Vec<PushFile> = Vec::new();
    files.push(PushFile {
        path:    format!("{prefix}/transcript.md"),
        content: snap.transcript.into_bytes(),
    });
    files.push(PushFile {
        path:    format!("{prefix}/state.json"),
        content: serde_json::to_vec_pretty(&snap.state)
            .unwrap_or_else(|_| b"{}".to_vec()),
    });
    for (name, body) in &snap.panels {
        files.push(PushFile {
            path:    format!("{prefix}/{name}.md"),
            content: body.clone().into_bytes(),
        });
    }

    info!(
        repo = %backend.repo,
        branch = %backend.branch,
        session_id,
        files = files.len(),
        "saving session to GitHub"
    );

    match backend.push_session(&message, &files).await {
        Ok(r) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "session_id": session_id,
                "commit_sha": r.commit_sha,
                "commit_url": r.commit_url,
                "files":      r.files,
            })),
        ),
        Err(e) => {
            warn!(?e, "github push failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}") })),
            )
        }
    }
}

// ─── helpers ───

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn mime_to_ffmpeg_format(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.contains("webm") { "webm" }
    else if m.contains("ogg") || m.contains("opus") { "ogg" }
    else if m.contains("mp4") || m.contains("m4a") { "mp4" }
    else if m.contains("wav") { "wav" }
    else if m.contains("mp3") || m.contains("mpeg") { "mp3" }
    else { "webm" }  // browser default
}
