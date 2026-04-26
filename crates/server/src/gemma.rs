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
use futures_util::FutureExt; // catch_unwind
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
        // Catch panics in the handler. A previous bug had `parse_panels`
        // panic on a regex with lookahead — when that happened the panel
        // pass tokio task died silently, sender keeps queueing triggers,
        // no one reading. AssertUnwindSafe is OK here: handler() returns
        // a fresh Future each iteration; nothing leaks across the catch.
        let result = std::panic::AssertUnwindSafe(handler()).catch_unwind().await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(loop_name = name, ?e, "gemma loop handler error"),
            Err(_) => warn!(
                loop_name = name,
                "gemma loop handler PANICKED — caught, loop continues"
            ),
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
    let current_state = serde_json::to_string(&snap.state).unwrap_or_else(|_| "{}".to_string());

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
    let current_state = serde_json::to_string(&snap.state).unwrap_or_else(|_| "{}".to_string());

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

/// Parse `## PANEL: name\n…` blocks. Same shape as gemma.py.
///
/// Original implementation used a regex with a lookahead
/// (`(?=\n## PANEL:|\n## DECISION:|\n## STATE:|\z)`) — but Rust's `regex`
/// crate is RE2-based and **panics** on lookahead. Each panel pass
/// silently took down the entire panel-pass tokio task, after which
/// trigger_panel_pass.send() succeeded but no one was reading the rx.
/// Result: panels permanently stuck on the loaded-from-GitHub baseline,
/// no extraction happening for the rest of the session.
///
/// Reimplemented as a hand-rolled state machine over the lines: locate
/// each `## PANEL: <name>` header, slurp lines until the NEXT header
/// (any of `## PANEL:`, `## DECISION:`, `## STATE:`, or end of input)
/// without any regex at all. No lookahead needed.
fn parse_panels(text: &str) -> Panels {
    let mut out = Panels::new();
    let mut current_name: Option<String> = None;
    let mut current_body: Vec<&str> = Vec::new();

    fn is_section_header(line: &str) -> Option<&'static str> {
        let l = line.trim_start();
        if l.starts_with("## PANEL:") {
            Some("PANEL")
        } else if l.starts_with("## DECISION:") {
            Some("DECISION")
        } else if l.starts_with("## STATE:") {
            Some("STATE")
        } else {
            None
        }
    }

    fn finalize(out: &mut Panels, name: Option<String>, body: Vec<&str>) {
        if let Some(name) = name {
            // Trim leading + trailing blank lines from the body.
            let mut start = 0;
            let mut end = body.len();
            while start < end && body[start].trim().is_empty() {
                start += 1;
            }
            while end > start && body[end - 1].trim().is_empty() {
                end -= 1;
            }
            let body_str = body[start..end].join("\n");
            out.insert(name.clone(), format!("## PANEL: {name}\n\n{body_str}"));
        }
    }

    for line in text.lines() {
        if let Some(kind) = is_section_header(line) {
            // Close out the previous panel (if any).
            finalize(
                &mut out,
                current_name.take(),
                std::mem::take(&mut current_body),
            );
            // Start a new panel block only if it's a PANEL header — DECISION
            // and STATE blocks terminate the previous panel but don't open
            // a new one.
            if kind == "PANEL" {
                // Header format: `## PANEL: name`. Take the part after the colon.
                let after_colon = line.trim_start().trim_start_matches("## PANEL:").trim();
                // Name is the first whitespace-separated token, lowercased.
                if let Some(first_tok) = after_colon.split_whitespace().next() {
                    current_name = Some(first_tok.to_lowercase());
                }
            }
        } else if current_name.is_some() {
            current_body.push(line);
        }
    }
    finalize(&mut out, current_name, current_body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parse_panels ──────────────────────────────────────────────────
    // These exist specifically because the original regex-based version
    // panicked at runtime due to RE2's lack of lookahead support — every
    // panel pass killed the panel-update tokio task. We never want that
    // to happen again.

    #[test]
    fn parse_panels_extracts_each_block() {
        let input = "\
## PANEL: scene
A dim corridor stretches ahead.

## PANEL: party
- Aryn HP 24/30 AC 15
- Tom HP 18/22 AC 13

## PANEL: next-steps
* Open the door.
* Light a torch.
";
        let out = parse_panels(input);
        assert_eq!(
            out.len(),
            3,
            "expected 3 panels, got {:?}",
            out.keys().collect::<Vec<_>>()
        );
        assert!(out["scene"].contains("A dim corridor stretches ahead."));
        assert!(out["party"].contains("Aryn HP 24/30"));
        assert!(out["next-steps"].contains("Open the door"));
        // Header is normalized + re-emitted at the start of the body.
        assert!(out["scene"].starts_with("## PANEL: scene\n\n"));
    }

    #[test]
    fn parse_panels_lowercases_panel_name() {
        let out = parse_panels("## PANEL: Scene\nfoo\n");
        assert!(out.contains_key("scene"));
        assert!(!out.contains_key("Scene"));
    }

    #[test]
    fn parse_panels_terminates_on_decision_and_state_headers() {
        // DECISION + STATE end the previous panel but DON'T open a new one.
        let input = "\
## PANEL: scene
in the woods

## DECISION: fight or flee
options here

## STATE: live
some state

## PANEL: party
the rest
";
        let out = parse_panels(input);
        assert_eq!(out.len(), 2);
        assert!(out["scene"].contains("in the woods"));
        // DECISION/STATE content is NOT slurped into the previous panel
        // and isn't its own panel either.
        assert!(!out["scene"].contains("fight or flee"));
        assert!(!out["scene"].contains("some state"));
        assert!(out["party"].contains("the rest"));
    }

    #[test]
    fn parse_panels_trims_blank_lines() {
        let out = parse_panels("## PANEL: scene\n\n\nfoo\n\n\n## PANEL: party\nbar\n");
        // No leading or trailing blank lines in the body.
        assert!(out["scene"].ends_with("\n\nfoo"));
    }

    #[test]
    fn parse_panels_empty_input_yields_no_panels() {
        assert!(parse_panels("").is_empty());
        assert!(parse_panels("just some prose with no headers").is_empty());
    }

    #[test]
    fn parse_panels_only_one_panel() {
        let out = parse_panels("## PANEL: scene\njust this one\n");
        assert_eq!(out.len(), 1);
        assert!(out["scene"].contains("just this one"));
    }

    // ─── extract_first_json_object ────────────────────────────────────

    #[test]
    fn extract_first_json_object_handles_code_fence() {
        let input = "```json\n{\"a\":1, \"b\":2}\n```";
        assert_eq!(extract_first_json_object(input), Some("{\"a\":1, \"b\":2}"));
    }

    #[test]
    fn extract_first_json_object_handles_nested_braces() {
        let input = "preamble {\"outer\": {\"inner\": 1}, \"x\": 2} trailing";
        assert_eq!(
            extract_first_json_object(input),
            Some("{\"outer\": {\"inner\": 1}, \"x\": 2}")
        );
    }

    #[test]
    fn extract_first_json_object_returns_none_on_unmatched() {
        assert_eq!(extract_first_json_object("no braces here"), None);
        assert_eq!(extract_first_json_object("{ unclosed"), None);
    }

    #[test]
    fn extract_first_json_object_stops_at_first_balanced() {
        // Should return the FIRST balanced object, not concatenate.
        let input = "{\"a\":1} extra {\"b\":2}";
        assert_eq!(extract_first_json_object(input), Some("{\"a\":1}"));
    }

    // ─── transcript_tail ──────────────────────────────────────────────

    #[test]
    fn transcript_tail_returns_full_when_short() {
        assert_eq!(transcript_tail("hello", 100), "hello");
    }

    #[test]
    fn transcript_tail_truncates_to_last_n_chars() {
        let s = "0123456789ABCDEF";
        assert_eq!(transcript_tail(s, 4), "CDEF");
    }
}
