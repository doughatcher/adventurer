//! STT inference via `whisper-rs`. Holds the loaded model; create a fresh
//! state per `transcribe` call.
//!
//! Audio decode (webm/mp3/opus → 16 kHz mono f32 PCM) is the caller's problem.
//! Today the bench shells out to ffmpeg; the production server will swap that
//! for `symphonia`.

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Debug, Clone, Copy)]
pub struct TranscribeMetrics {
    pub audio_secs: f64,
    pub elapsed_secs: f64,
    pub realtime_factor: f64,
}

pub struct SttEngine {
    ctx: WhisperContext,
    threads: i32,
    initial_prompt: Option<String>,
}

/// A baseline prompt that biases whisper toward fantasy/RPG vocabulary —
/// names, action verbs, dice mechanics. Without it, narration like
/// "Spock shoots the alien for 3 damage" tends to get reduced to
/// `[BLANK_AUDIO]` or replaced with hallucinated boilerplate.
pub const DEFAULT_DND_PROMPT: &str = "Audio from a tabletop role-playing game session. \
Players narrate their characters' actions and the dungeon master describes scenes, \
monsters, NPCs, and combat. Common terms include attack, damage, rolls, hit points, \
initiative, spell, save, ability check, dexterity, strength. Character names like \
Granit, Rides the Wake, Vargr, Spock, Lyvriele appear frequently.";

impl SttEngine {
    pub fn load(model_path: &Path) -> Result<Self> {
        let t = Instant::now();
        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 model path"))?,
            WhisperContextParameters::default(),
        )
        .with_context(|| format!("load whisper model {}", model_path.display()))?;
        tracing::info!(
            model = %model_path.display(),
            elapsed_secs = t.elapsed().as_secs_f64(),
            "whisper model loaded"
        );
        Ok(Self {
            ctx,
            threads: default_threads(),
            initial_prompt: Some(DEFAULT_DND_PROMPT.to_string()),
        })
    }

    pub fn with_threads(mut self, threads: i32) -> Self {
        self.threads = threads;
        self
    }

    /// Override the default initial prompt. `None` disables the bias.
    pub fn with_initial_prompt(mut self, prompt: Option<String>) -> Self {
        self.initial_prompt = prompt;
        self
    }

    /// Transcribe a slab of 16 kHz mono f32 PCM. `language` is an ISO-639-1
    /// code or `"auto"` for whisper's auto-detection.
    pub fn transcribe(&self, pcm: &[f32], language: &str) -> Result<(String, TranscribeMetrics)> {
        let mut state = self.ctx.create_state().context("create whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_translate(false);
        params.set_language(Some(language));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Bias Whisper toward D&D narration vocabulary so it doesn't replace
        // real game speech with `[BLANK_AUDIO]` or YouTube-style hallucinations.
        if let Some(p) = self.initial_prompt.as_deref() {
            params.set_initial_prompt(p);
        }
        // Suppress non-speech token outputs ([silence], [music], etc.).
        params.set_suppress_blank(true);
        params.set_suppress_nst(true); // suppress non-speech tokens

        let t = Instant::now();
        state.full(params, pcm).context("whisper full() decode")?;
        let elapsed = t.elapsed().as_secs_f64();

        let n_segments = state.full_n_segments();
        let mut text = String::new();
        for i in 0..n_segments {
            let segment = state
                .get_segment(i)
                .ok_or_else(|| anyhow!("segment {i} out of bounds"))?;
            let seg_text = segment
                .to_str()
                .with_context(|| format!("segment {i} to_str"))?;
            text.push_str(seg_text);
        }

        let audio_secs = pcm.len() as f64 / 16_000.0;
        let realtime = audio_secs / elapsed.max(0.001);
        Ok((
            text,
            TranscribeMetrics {
                audio_secs,
                elapsed_secs: elapsed,
                realtime_factor: realtime,
            },
        ))
    }
}

fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(8)
        .min(16)
}
