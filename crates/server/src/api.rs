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

/// Multipart audio upload → SAVE to permanent path FIRST → STT worker → append
/// transcript → JSONL log → broadcast.
///
/// **Never-delete policy**: every chunk lands in `/work/session/audio/`
/// regardless of STT success. Every transcribe attempt appends one JSONL line
/// to `/work/session/raw-events.jsonl`. If STT crashes or returns nonsense, the
/// raw audio + the metadata survive and the session can be reconstructed
/// later (see `scripts/replay-session.sh`, planned).
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
    let format = mime_to_ffmpeg_format(&content_type);
    let ext = format; // we use the same string for the file extension

    // STEP 1: stamp a session_id if this is the first audio of the session.
    let session_id = ctx.state.ensure_session_id().await;

    // STEP 2: write the audio bytes to a permanent path BEFORE doing anything
    // else. If the server crashes between here and STT, the raw chunk is
    // recoverable.
    let (seq, audio_path) = ctx.state.next_chunk_path(ext).await;
    if let Err(e) = tokio::fs::write(&audio_path, &audio).await {
        warn!(?e, path = %audio_path.display(), "voice: failed to write audio chunk");
        ctx.state.append_raw_event(json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "session_id": session_id,
            "seq": seq,
            "status": "write_fail",
            "error": format!("{e}"),
        }));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("write: {e}") })),
        );
    }

    info!(
        seq,
        bytes = audio.len(),
        mime = %content_type,
        ffmpeg_format = format,
        path = %audio_path.display(),
        "voice: chunk archived, transcribing"
    );

    // STEP 3: hand the path to the STT worker (no copy / no base64 / no delete).
    let stt_result = ctx.stt.transcribe_path(&audio_path, format, "en").await;

    // STEP 4: log the attempt to raw-events.jsonl regardless of outcome.
    let now = chrono::Utc::now().to_rfc3339();
    match &stt_result {
        Ok(resp) => {
            let text = resp.text.trim().to_string();
            let kind = if text.is_empty() || is_whisper_hallucination(&text) {
                "skipped"
            } else {
                "ok"
            };
            ctx.state.append_raw_event(json!({
                "ts": now,
                "session_id": session_id,
                "seq": seq,
                "audio_file": audio_path.file_name()
                    .and_then(|n| n.to_str()).unwrap_or(""),
                "bytes": audio.len(),
                "mime": content_type,
                "status": kind,
                "text": text,
                "audio_secs": resp.audio_secs,
                "elapsed_secs": resp.elapsed_secs,
            }));
            if kind == "skipped" {
                info!(text = %text, "voice: skipping hallucinated/empty");
                return (
                    StatusCode::OK,
                    Json(json!({
                        "ok": false, "text": "", "skipped": true,
                        "audio_file": audio_path.file_name().and_then(|n| n.to_str()),
                    })),
                );
            }
            let ts = chrono::Local::now().format("%H:%M:%S");
            let line = format!("\n**[{ts}]** {text}");
            let (full, tail) = ctx.state.append_transcript(&line).await;
            ctx.state.mirror_transcript(&full);
            ctx.state.broadcast(Event::Transcript {
                content: full,
                tail,
            });
            let _ = ctx.trigger_state_pass.send(());
            let _ = ctx.trigger_panel_pass.send(());
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true, "text": text,
                    "audio_file": audio_path.file_name().and_then(|n| n.to_str()),
                })),
            )
        }
        Err(e) => {
            ctx.state.append_raw_event(json!({
                "ts": now,
                "session_id": session_id,
                "seq": seq,
                "audio_file": audio_path.file_name()
                    .and_then(|n| n.to_str()).unwrap_or(""),
                "bytes": audio.len(),
                "mime": content_type,
                "status": "stt_error",
                "error": format!("{e:#}"),
            }));
            warn!(
                ?e,
                "stt worker error — chunk preserved at {}",
                audio_path.display()
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}"),
                          "audio_file": audio_path.file_name().and_then(|n| n.to_str()) })),
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

pub async fn patch_character(
    State(_ctx): Ctx,
    Path(slug): Path<String>,
    Json(_v): Json<Value>,
) -> impl IntoResponse {
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

pub async fn announce_player(State(ctx): Ctx, Json(p): Json<AnnouncePayload>) -> impl IntoResponse {
    let info = ctx.players.touch(&p.token, p.label).await;
    info!(token = %info.token, label = ?info.label, "player announced");
    ctx.state.broadcast(Event::PlayerJoined {
        player: info.clone(),
    });
    Json(info)
}

pub async fn assign_player_character(
    State(ctx): Ctx,
    Path(token): Path<String>,
    Json(p): Json<AssignPayload>,
) -> impl IntoResponse {
    match ctx
        .players
        .assign_character(&token, p.character.clone())
        .await
    {
        Some(info) => {
            info!(token = %token, character = ?p.character, "player assigned character");
            ctx.state.broadcast(Event::PlayerAssigned {
                player: info.clone(),
            });
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
/// Browser → server diagnostic logging. Fire-and-forget POSTs from the
/// gamepad/input debug overlay so we can see what reaches the page without
/// reading the on-screen overlay over a TV. Body is free-form JSON.
pub async fn log_input_debug(State(_ctx): Ctx, Json(v): Json<Value>) -> impl IntoResponse {
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
    info!(target: "adventurer::input_debug", kind, payload = %v, "browser input event");
    StatusCode::NO_CONTENT
}

/// GET /api/session — current session metadata (id, mode).
pub async fn get_session_info(State(ctx): Ctx) -> impl IntoResponse {
    Json(json!({
        "id":   ctx.state.session_id().await,
        "mode": ctx.state.session_mode().await,
    }))
}

/// POST /api/session/mode — flip between "live" and "test" modes.
#[derive(Deserialize)]
pub struct SetModePayload {
    pub mode: String,
}
pub async fn set_session_mode(State(ctx): Ctx, Json(p): Json<SetModePayload>) -> impl IntoResponse {
    let mode = match p.mode.as_str() {
        "test" | "live" => p.mode,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("unknown mode: {other:?}") })),
            )
        }
    };
    ctx.state.set_session_mode(&mode).await;
    info!(mode = %mode, "session mode set");
    (StatusCode::OK, Json(json!({ "ok": true, "mode": mode })))
}

#[derive(Deserialize, Default)]
pub struct StartSessionPayload {
    /// Optional explicit session ID. Defaults to `YYYY-MM-DD-HHMM`.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// POST /api/session/start — explicit session-start (sets stable id).
pub async fn start_session(
    State(ctx): Ctx,
    Json(p): Json<StartSessionPayload>,
) -> impl IntoResponse {
    let id = p
        .session_id
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d-%H%M").to_string());
    ctx.state.set_session_id(id.clone()).await;
    if let Some(m) = p.mode.as_deref() {
        if m == "live" || m == "test" {
            ctx.state.set_session_mode(m).await;
        }
    }
    info!(session_id = %id, "session started");
    (
        StatusCode::OK,
        Json(json!({
            "ok": true, "session_id": id, "mode": ctx.state.session_mode().await,
        })),
    )
}

// ─── continue from a previously archived adventure-log session ───

pub async fn list_adventure_log_sessions(State(ctx): Ctx) -> impl IntoResponse {
    let cfg = ctx.config.snapshot().await;
    if !cfg.is_ready() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "GitHub repo + PAT not configured" })),
        );
    }
    let branch = cfg.branch_or_main().to_string();
    let backend = GitHubBackend {
        repo: cfg.repo.expect("checked above"),
        branch,
        pat: cfg.pat.expect("checked above"),
    };
    match backend.list_sessions().await {
        Ok(ids) => (StatusCode::OK, Json(json!({ "ok": true, "sessions": ids }))),
        Err(e) => {
            warn!(?e, "list_adventure_log_sessions failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}") })),
            )
        }
    }
}

#[derive(Deserialize)]
pub struct LoadSessionPayload {
    pub session_id: String,
}

pub async fn load_session(State(ctx): Ctx, Json(p): Json<LoadSessionPayload>) -> impl IntoResponse {
    let cfg = ctx.config.snapshot().await;
    if !cfg.is_ready() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "GitHub repo + PAT not configured" })),
        );
    }
    let branch = cfg.branch_or_main().to_string();
    let backend = GitHubBackend {
        repo: cfg.repo.expect("checked above"),
        branch,
        pat: cfg.pat.expect("checked above"),
    };
    let loaded = match backend.load_session(&p.session_id).await {
        Ok(s) => s,
        Err(e) => {
            warn!(?e, "load_session failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("{e:#}") })),
            );
        }
    };
    // Seed the running session with the loaded data + adopt its session_id so
    // future /api/session/save updates the SAME folder rather than creating
    // a new sibling.
    ctx.state.set_session_id(loaded.id.clone()).await;
    ctx.state
        .load_full(
            loaded.transcript.clone(),
            loaded.state.clone(),
            loaded.panels.clone(),
        )
        .await;
    info!(session_id = %loaded.id, transcript_len = loaded.transcript.len(),
          "loaded session from adventure-log");
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "session_id": loaded.id,
            "transcript_len": loaded.transcript.len(),
            "panels": loaded.panels.keys().collect::<Vec<_>>(),
        })),
    )
}

pub async fn save_session(State(ctx): Ctx, Json(p): Json<SaveSessionPayload>) -> impl IntoResponse {
    if ctx.state.session_mode().await == "test" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "session is in test mode — start a live session to save to GitHub",
                "mode": "test",
            })),
        );
    }
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
    // Default to the current session_id so subsequent saves UPDATE the same
    // folder (continuing a session) rather than creating siblings each time.
    let session_id = if let Some(id) = p.session_id {
        id
    } else {
        let from_state = ctx.state.session_id().await;
        if from_state.is_empty() {
            chrono::Local::now().format("%Y-%m-%d-%H%M").to_string()
        } else {
            from_state
        }
    };
    let message = p
        .message
        .unwrap_or_else(|| format!("Session archive: {session_id}"));
    // Extract first — `branch_or_main()` borrows `cfg`, so we can't call it
    // after `unwrap()`-ing other fields out.
    let branch = cfg.branch_or_main().to_string();
    let backend = GitHubBackend {
        repo: cfg.repo.expect("checked is_ready above"),
        branch,
        pat: cfg.pat.expect("checked is_ready above"),
    };

    // Snapshot session content into PushFiles.
    let snap = ctx.state.snapshot().await;
    let prefix = format!("data/sessions/{session_id}");

    let mut files: Vec<PushFile> = Vec::new();
    files.push(PushFile {
        path: format!("{prefix}/transcript.md"),
        content: snap.transcript.into_bytes(),
    });
    files.push(PushFile {
        path: format!("{prefix}/state.json"),
        content: serde_json::to_vec_pretty(&snap.state).unwrap_or_else(|_| b"{}".to_vec()),
    });
    for (name, body) in &snap.panels {
        files.push(PushFile {
            path: format!("{prefix}/{name}.md"),
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

/// True if Whisper returned a placeholder for "I heard nothing", OR one of
/// its notorious YouTube-end-card hallucinations on quiet/empty audio.
///
/// Whisper was trained on a lot of YouTube video subtitles; when it's fed
/// audio that's quiet but not pure silence (mic in a room with no speech,
/// background music, ambient HVAC noise), it confabulates from the most
/// common phrases in its training data — "Thank you for watching",
/// "Subtitles by Amara.org", "Music by Audio Library", etc. These are
/// pure noise from our perspective and pollute the transcript fast.
///
/// We deliberately keep ambient-sound brackets like `[paper rustling]` or
/// `[footsteps]` — those ARE real signal that whisper actually heard.
fn is_whisper_hallucination(text: &str) -> bool {
    let t = text.trim();
    let lower = t.to_lowercase();
    // Exact-match common empties (whisper.cpp returns these for silent audio)
    if matches!(
        lower.as_str(),
        "[blank_audio]"
            | "[silence]"
            | "[inaudible]"
            | "[no audio]"
            | "[no speech]"
            | "(silence)"
            | "(blank_audio)"
            | "(no audio)"
            | "[ silence ]"
            | "blank_audio"
            | "silence"
            | "you"
            | "."
    ) {
        return true;
    }
    // Substring match on known YouTube confabulations. Match the whole
    // (lowercased) chunk because these are typically the ENTIRE output
    // of a single whisper call. Anything wrapped around real speech in
    // the same chunk would have other text, so checking the whole
    // string is a reasonable signal.
    const YT_HALLUCINATIONS: &[&str] = &[
        "thank you for watching",
        "thanks for watching",
        "thanks for being with us",
        "subtitles by",
        "subtitled by",
        "subscribe to",
        "like and subscribe",
        "music by",
        "video by",
        "video extract",
        "edited by",
        "see you next",
        "see you in the next",
        "outro music",
        "intro music",
        "amara.org",
        "the end of the video",
    ];
    for needle in YT_HALLUCINATIONS {
        if lower.contains(needle) {
            return true;
        }
    }
    false
}

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
    if m.contains("webm") {
        "webm"
    } else if m.contains("ogg") || m.contains("opus") {
        "ogg"
    } else if m.contains("mp4") || m.contains("m4a") {
        "mp4"
    } else if m.contains("wav") {
        "wav"
    } else if m.contains("mp3") || m.contains("mpeg") {
        "mp3"
    } else {
        "webm"
    } // browser default
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── is_whisper_hallucination ─────────────────────────────────────
    // Two-tier policy: only filter the 3 sentinel "I heard nothing"
    // markers + a hand-curated list of YouTube end-card phrases. Real
    // ambient captures like [paper rustling] MUST pass through.

    #[test]
    fn rejects_blank_audio_sentinels() {
        assert!(is_whisper_hallucination("[BLANK_AUDIO]"));
        assert!(is_whisper_hallucination("[blank_audio]"));
        assert!(is_whisper_hallucination("[silence]"));
        assert!(is_whisper_hallucination("[inaudible]"));
        assert!(is_whisper_hallucination("[no audio]"));
        assert!(is_whisper_hallucination("(silence)"));
    }

    #[test]
    fn rejects_single_token_artifacts() {
        // Whisper sometimes returns just "you" or "." on noise.
        assert!(is_whisper_hallucination("you"));
        assert!(is_whisper_hallucination("."));
    }

    #[test]
    fn rejects_youtube_end_cards() {
        assert!(is_whisper_hallucination("Thank you for watching!"));
        assert!(is_whisper_hallucination("Thanks for watching"));
        assert!(is_whisper_hallucination(
            "Subtitles by the Amara.org community"
        ));
        assert!(is_whisper_hallucination("Music by AudioLibrary.com"));
        assert!(is_whisper_hallucination("Like and subscribe to my channel"));
        assert!(is_whisper_hallucination("Video by David"));
        assert!(is_whisper_hallucination("See you next time"));
    }

    #[test]
    fn keeps_real_ambient_captures() {
        // The whole point of dialing back the filter — real ambient
        // sound captures from gemma's gf painting in the room must
        // make it through to the transcript.
        assert!(!is_whisper_hallucination("[paper rustling]"));
        assert!(!is_whisper_hallucination("[chair creaks]"));
        assert!(!is_whisper_hallucination("[footsteps approaching]"));
        assert!(!is_whisper_hallucination("[door opens]"));
        assert!(!is_whisper_hallucination("[music playing]"));
    }

    #[test]
    fn keeps_real_dialog() {
        assert!(!is_whisper_hallucination("I cast fireball at the goblin"));
        assert!(!is_whisper_hallucination("Aryn rolls a 17 for athletics"));
        assert!(!is_whisper_hallucination(
            "It seems we have arrived at the temple"
        ));
    }

    // ─── slugify ─────────────────────────────────────────────────────

    #[test]
    fn slugify_basic_ascii() {
        assert_eq!(slugify("Aryn the Druid"), "aryn-the-druid");
        assert_eq!(slugify("Goblin"), "goblin");
    }

    #[test]
    fn slugify_collapses_runs_and_strips_trailing_dashes() {
        assert_eq!(slugify("  whitespace  surround  "), "whitespace-surround");
        assert_eq!(slugify("multiple---dashes"), "multiple-dashes");
        assert_eq!(slugify("trailing!!!"), "trailing");
        assert_eq!(slugify("!!!leading"), "leading");
    }

    #[test]
    fn slugify_lowercases() {
        assert_eq!(slugify("MiXeD CaSe"), "mixed-case");
    }

    #[test]
    fn slugify_handles_special_chars() {
        // Apostrophes, parentheses etc. become dashes/dropped.
        assert_eq!(slugify("Rides-the-Wake"), "rides-the-wake");
        assert_eq!(slugify("A'priest (B)"), "a-priest-b");
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!@#$%"), "");
    }

    // ─── mime_to_ffmpeg_format ───────────────────────────────────────

    #[test]
    fn mime_browser_codec_strings() {
        assert_eq!(mime_to_ffmpeg_format("audio/webm"), "webm");
        assert_eq!(mime_to_ffmpeg_format("audio/webm;codecs=opus"), "webm");
        assert_eq!(mime_to_ffmpeg_format("audio/ogg;codecs=opus"), "ogg");
        // Safari / iOS — both mp4 and m4a should map to mp4 for ffmpeg.
        assert_eq!(mime_to_ffmpeg_format("audio/mp4"), "mp4");
        assert_eq!(mime_to_ffmpeg_format("audio/mp4;codecs=mp4a.40.2"), "mp4");
        assert_eq!(mime_to_ffmpeg_format("audio/x-m4a"), "mp4");
        assert_eq!(mime_to_ffmpeg_format("audio/wav"), "wav");
        assert_eq!(mime_to_ffmpeg_format("audio/mp3"), "mp3");
        assert_eq!(mime_to_ffmpeg_format("audio/mpeg"), "mp3");
    }

    #[test]
    fn mime_case_insensitive() {
        assert_eq!(mime_to_ffmpeg_format("AUDIO/WEBM"), "webm");
        assert_eq!(mime_to_ffmpeg_format("Audio/MP4"), "mp4");
    }

    #[test]
    fn mime_unknown_falls_back_to_webm() {
        // The browser's default MediaRecorder mimetype is webm in
        // Chrome/Firefox so unknown == "probably webm" is the right
        // optimistic guess for ffmpeg to try first.
        assert_eq!(mime_to_ffmpeg_format(""), "webm");
        assert_eq!(mime_to_ffmpeg_format("audio/something-weird"), "webm");
    }
}
