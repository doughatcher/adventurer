//! Debounced LLM update loops — port of `dnd-stage/server/gemma.py`'s state +
//! panel passes. Two background tasks:
//!
//!   - **state pass**: triggered every ~80 chars of new transcript, debounce 6s,
//!     fast LLM call (~350 token cap), parses JSON, deep-merges into state, broadcasts.
//!
//!   - **panel pass**: every ~300 chars new transcript, debounce 12s, slower
//!     LLM call (~1400 tokens), parses panel blocks, broadcasts.
//!
//! The two loops share the same single LLM worker (serialized at the worker
//! level — engine thread processes one request at a time), so they queue up
//! naturally if both fire close together.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{info, warn};

use crate::state::{AppState, Event, Panels};
use crate::workers::LlmWorker;

const SYSTEM_PROMPT: &str =
    "You are a precise D&D session state tracker. Output only what is asked, in exact format specified. No extra commentary.";

const STATE_PROMPT_TEMPLATE: &str = include_str!("../../../prompts/state.txt");

/// Tuning knobs (mirrors dnd-stage's GEMMA_* env vars).
pub struct GemmaConfig {
    pub state_debounce: Duration,
    pub panel_debounce: Duration,
    pub state_max_tokens: i32,
    pub panel_max_tokens: i32,
}

impl Default for GemmaConfig {
    fn default() -> Self {
        Self {
            state_debounce: Duration::from_secs(6),
            panel_debounce: Duration::from_secs(12),
            state_max_tokens: 350,
            panel_max_tokens: 1400,
        }
    }
}

/// Spawn the two debounced loops.
pub fn spawn(
    cfg: GemmaConfig,
    app: AppState,
    llm: Arc<LlmWorker>,
    state_rx: UnboundedReceiver<()>,
    panel_rx: UnboundedReceiver<()>,
) {
    let (app_a, app_b) = (app.clone(), app);
    let (llm_a, llm_b) = (llm.clone(), llm);

    tokio::spawn(debounced_loop(
        "state",
        cfg.state_debounce,
        state_rx,
        move || {
            let app = app_a.clone();
            let llm = llm_a.clone();
            let max_tokens = cfg.state_max_tokens;
            async move { do_state_pass(app, llm, max_tokens).await }
        },
    ));
    tokio::spawn(debounced_loop(
        "panel",
        cfg.panel_debounce,
        panel_rx,
        move || {
            let app = app_b.clone();
            let llm = llm_b.clone();
            let max_tokens = cfg.panel_max_tokens;
            async move { do_panel_pass(app, llm, max_tokens).await }
        },
    ));
}

/// Generic debounced loop:
///   wait for first nudge → sleep `debounce` → drain queue → run handler → repeat
async fn debounced_loop<F, Fut>(
    name: &'static str,
    debounce: Duration,
    mut rx: UnboundedReceiver<()>,
    handler: F,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    while let Some(()) = rx.recv().await {
        tokio::time::sleep(debounce).await;
        // drain extra nudges that arrived during the debounce window
        while rx.try_recv().is_ok() {}
        if let Err(e) = handler().await {
            warn!(loop_name = name, ?e, "gemma loop handler error");
        }
    }
    info!(loop_name = name, "gemma loop exiting");
}

async fn do_state_pass(app: AppState, llm: Arc<LlmWorker>, max_tokens: i32) -> Result<()> {
    let snap = app.snapshot().await;
    let transcript = snap.transcript.trim();
    if transcript.is_empty() {
        return Ok(());
    }
    let tail = transcript_tail(transcript, 2000);
    let current_state =
        serde_json::to_string(&snap.state).unwrap_or_else(|_| "{}".to_string());

    let prompt = STATE_PROMPT_TEMPLATE
        .replace("{CURRENT_STATE}", &current_state)
        .replace("{PARTY_DATA}", "(no party data yet)")
        .replace("{TRANSCRIPT}", &tail);

    info!("gemma: state pass starting");
    let resp = llm.generate(SYSTEM_PROMPT, &prompt, max_tokens).await?;
    info!(
        tokens = resp.tokens,
        elapsed = resp.elapsed_secs,
        tps = resp.tokens_per_sec,
        "gemma: state pass done"
    );

    let Some(json_obj) = extract_first_json_object(&resp.text) else {
        warn!(text = %resp.text, "gemma state: no JSON object in output");
        return Ok(());
    };
    match serde_json::from_str::<serde_json::Value>(json_obj) {
        Ok(v) => {
            app.merge_state(&v).await;
            let snap2 = app.snapshot().await;
            app.broadcast(Event::State { data: snap2.state });
        }
        Err(e) => warn!(?e, slice = json_obj, "gemma state: JSON parse failed"),
    }
    Ok(())
}

async fn do_panel_pass(app: AppState, llm: Arc<LlmWorker>, max_tokens: i32) -> Result<()> {
    let snap = app.snapshot().await;
    let transcript = snap.transcript.trim();
    if transcript.is_empty() {
        return Ok(());
    }
    let tail = transcript_tail(transcript, 3000);
    let current_state =
        serde_json::to_string(&snap.state).unwrap_or_else(|_| "{}".to_string());

    // Panel prompt is constructed inline (verbatim from gemma.py PANEL_PROMPT).
    let prompt = format!(
        r#"Update these D&D session panels from the transcript. Output ONLY the blocks below, no other text.

IMPORTANT RULES:
- OOC = out-of-character table talk. IGNORE OOC for scene/map/story.
- Scene and Map describe the GAME WORLD only — never describe the players talking at the table.
- If recent transcript is mostly OOC, keep previous scene/map content unchanged.
- Next-steps: 3-5 COMPLETE sentences. Suggest items if shopping.
- Every sentence in every panel must be complete — never cut off mid-sentence.

## PANEL: scene
Line 1: ONE punchy sentence (≤12 words, present tense). Then 1-2 sentences of optional detail.

## PANEL: story-log
(growing bullet list of major IN-GAME events only, keep all previous, add newest last)

## PANEL: party
(each character: name HP/max AC, active conditions — enemies listed separately)

## PANEL: next-steps
3-5 bullets. Each bullet: ≤7 words, starts with a verb.

## PANEL: map
Output ONLY lines in exactly these formats (no prose, no markdown):
node: ID | Label | type
edge: FromID | ToID | label
here: CharacterName | NodeID

Current game state:
{current_state}

Transcript:
{tail}
"#
    );

    info!("gemma: panel pass starting");
    let resp = llm.generate(SYSTEM_PROMPT, &prompt, max_tokens).await?;
    info!(
        tokens = resp.tokens,
        elapsed = resp.elapsed_secs,
        tps = resp.tokens_per_sec,
        "gemma: panel pass done"
    );

    let panels = parse_panels(&resp.text);
    if panels.is_empty() {
        warn!(text = %resp.text, "gemma panel: no panels parsed");
        return Ok(());
    }
    for (name, body) in &panels {
        app.set_panel(name, body.clone()).await;
    }
    app.broadcast(Event::Panels { data: panels });
    Ok(())
}

fn transcript_tail(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        text[text.len() - max_chars..].to_string()
    }
}

/// Find the first balanced `{...}` JSON object in `raw` (skips ``` fences etc.).
fn extract_first_json_object(raw: &str) -> Option<&str> {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = cleaned.find('{')?;
    let mut depth = 0;
    for (i, ch) in cleaned[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&cleaned[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse `## PANEL: name\n…` blocks. Same regex shape as gemma.py.
fn parse_panels(text: &str) -> Panels {
    let re = Regex::new(r"(?ms)^## PANEL:\s*(\S+)\s*\n(.*?)(?=\n## PANEL:|\n## DECISION:|\n## STATE:|\z)")
        .expect("static regex");
    let mut out = Panels::new();
    for cap in re.captures_iter(text) {
        let name = cap[1].to_lowercase();
        let body = cap[2].trim();
        out.insert(name.clone(), format!("## PANEL: {name}\n\n{body}"));
    }
    out
}
