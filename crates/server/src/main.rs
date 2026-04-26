//! adventurer — single-binary live D&D session companion.
//!
//! Layout at runtime:
//!
//!   main task ─┬─ axum on 0.0.0.0:3200
//!              │   ├─ static UI (rust-embed of dnd-stage/client/)
//!              │   ├─ /api/{panels,transcript,state,voice,update,characters,…}
//!              │   └─ /ws (broadcast: panels/transcript/state/decision)
//!              │
//!              ├─ LLM worker child process (adventurer-llm-bench --worker)
//!              │   stdin/stdout JSON; serialized inference jobs
//!              │
//!              ├─ STT worker child process (adventurer-stt-bench --worker)
//!              │
//!              └─ gemma update loops (state debounce 6s, panel debounce 12s)

mod api;
mod config;
mod embed;
mod gemma;
mod lan;
mod players;
mod state;
mod sync;
mod workers;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    routing::{get, patch, post},
    Router,
};
use clap::Parser;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{info, Level};

use api::AppContext;
use state::AppState;
use workers::{LlmSpawnOpts, LlmWorker, SttSpawnOpts, SttWorker};

#[derive(Parser, Debug)]
#[command(version, about = "adventurer — live D&D session companion")]
struct Args {
    #[arg(long, env = "PORT", default_value_t = 3200)]
    port: u16,

    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    host: String,

    /// Where session files (transcript.md, state.json, panels) get mirrored.
    #[arg(long, env = "SESSION_DIR", default_value = "/work/session")]
    session_dir: PathBuf,

    /// Path to the LLM worker executable (adventurer-llm-bench).
    /// Defaults to a sibling of the current exe.
    #[arg(long, env = "LLM_WORKER")]
    llm_worker: Option<PathBuf>,

    /// Path to the STT worker executable (adventurer-stt-bench).
    #[arg(long, env = "STT_WORKER")]
    stt_worker: Option<PathBuf>,

    /// LLM model file.
    #[arg(long, env = "LLM_MODEL", default_value = "/models/gemma-4-E4B-it-Q4_K_M.gguf")]
    llm_model: PathBuf,

    /// STT model file.
    #[arg(long, env = "STT_MODEL", default_value = "/models/ggml-medium.bin")]
    stt_model: PathBuf,

    /// LLM context window size.
    #[arg(long, env = "LLM_N_CTX", default_value_t = 4096)]
    llm_n_ctx: u32,

    /// LLM GPU layers — 99 to offload everything (assumes a CUDA build).
    #[arg(long, env = "LLM_GPU_LAYERS", default_value_t = 99)]
    llm_gpu_layers: u32,

    /// STT decode threads.
    #[arg(long, env = "STT_THREADS", default_value_t = 8)]
    stt_threads: i32,

    /// Skip spawning workers — useful for `/health`-only smoke tests.
    #[arg(long)]
    no_workers: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    std::fs::create_dir_all(&args.session_dir).ok();

    let app_state = AppState::new(args.session_dir.clone());

    let (state_tx, state_rx) = tokio::sync::mpsc::unbounded_channel();
    let (panel_tx, panel_rx) = tokio::sync::mpsc::unbounded_channel();

    // Resolve worker paths — default: sibling of current exe.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"));
    let llm_worker_path = args
        .llm_worker
        .clone()
        .unwrap_or_else(|| exe_dir.join("adventurer-llm-bench"));
    let stt_worker_path = args
        .stt_worker
        .clone()
        .unwrap_or_else(|| exe_dir.join("adventurer-stt-bench"));

    if args.no_workers {
        info!("--no-workers set; running with no inference (only /health works)");
        return run_server_only_health(&args).await;
    }

    info!(
        llm_worker = %llm_worker_path.display(),
        stt_worker = %stt_worker_path.display(),
        "spawning inference workers"
    );

    // Spawn workers SERIALLY. CUDA init from two processes onto the same GPU
    // simultaneously can deadlock — earlier runs only worked because the loads
    // happened to interleave. Load STT first (smaller, faster), then LLM.
    let stt = Arc::new(
        SttWorker::spawn(SttSpawnOpts {
            program: stt_worker_path.display().to_string(),
            model: args.stt_model.clone(),
            threads: args.stt_threads,
        })
        .await
        .context("spawn STT worker")?,
    );
    let llm = Arc::new(
        LlmWorker::spawn(LlmSpawnOpts {
            program: llm_worker_path.display().to_string(),
            model: args.llm_model.clone(),
            n_ctx: args.llm_n_ctx,
            gpu_layers: args.llm_gpu_layers,
            extra: None,
        })
        .await
        .context("spawn LLM worker")?,
    );

    // Spawn the debounced gemma loops.
    gemma::spawn(
        gemma::GemmaConfig::default(),
        app_state.clone(),
        llm.clone(),
        state_rx,
        panel_rx,
    );

    let lan_ip = lan::detect_lan_ip();
    let players = players::Players::new();
    let config_store = config::ConfigStore::load();

    // Live-reload watcher: when ADVENTURER_DEV_ASSETS points at a host-mounted
    // assets dir, watch it for changes. Each change broadcasts a DevReload
    // event so any connected browser can reload itself.
    if let Ok(dev_path) = std::env::var("ADVENTURER_DEV_ASSETS") {
        let app_state_for_watch = app_state.clone();
        std::thread::spawn(move || spawn_dev_asset_watcher(dev_path, app_state_for_watch));
    }
    let cfg_snap = config_store.snapshot().await;
    info!(
        %lan_ip,
        port = args.port,
        github_repo = ?cfg_snap.repo,
        github_pat = if cfg_snap.pat.is_some() { "set" } else { "unset" },
        "server context"
    );

    let ctx = Arc::new(AppContext {
        state: app_state.clone(),
        stt,
        llm,
        players,
        lan_ip,
        port: args.port,
        config: config_store,
        trigger_state_pass: state_tx,
        trigger_panel_pass: panel_tx,
    });

    // axum 0.7 path syntax: `:name` typed param, `*name` catch-all.
    // (axum 0.8 uses `{name}` / `{*name}` with braces but cargo resolved 0.7.)
    let app = Router::new()
        .route("/", get(embed::index))
        .route("/join", get(embed::player_index))
        .route("/health", get(health))
        .route("/api/panels", get(api::get_panels))
        .route("/api/transcript", get(api::get_transcript))
        .route("/api/state", get(api::get_state))
        .route("/api/voice", post(api::post_voice))
        .route("/api/update", post(api::post_update))
        .route("/api/characters", get(api::list_characters).post(api::add_character))
        .route("/api/characters/:slug", patch(api::patch_character))
        .route("/api/sessions", get(api::list_sessions))
        .route("/api/sessions/:ts", get(api::get_session))
        .route("/api/session/end", post(api::end_session))
        .route("/api/lan-info", get(api::get_lan_info))
        .route("/api/players", get(api::list_players))
        .route("/api/players/announce", post(api::announce_player))
        .route("/api/players/:token/assign", post(api::assign_player_character))
        .route("/api/config", get(api::get_config).post(api::set_config))
        .route("/api/session/save",  post(api::save_session))
        .route("/api/session",       get(api::get_session_info))
        .route("/api/session/start", post(api::start_session))
        .route("/api/session/mode",  post(api::set_session_mode))
        .route("/api/session/load",  post(api::load_session))
        .route("/api/adventure-log/sessions", get(api::list_adventure_log_sessions))
        .route("/api/debug/input",   post(api::log_input_debug))
        .route("/ws", get(ws::ws_handler))
        .route("/static/*path", get(embed::static_file))
        .with_state(ctx)
        // Log every HTTP request — method + path + status + latency. Lets us
        // see iPad-from-cloudflare hits + diagnose missing /api/voice POSTs.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        );

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, backend = cfg_backend(), "adventurer listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[derive(serde::Serialize)]
struct Health {
    name: &'static str,
    version: &'static str,
    status: &'static str,
    backend: &'static str,
}

async fn health() -> axum::Json<Health> {
    axum::Json(Health {
        name: "adventurer",
        version: env!("CARGO_PKG_VERSION"),
        status: "ok",
        backend: cfg_backend(),
    })
}

const fn cfg_backend() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else if cfg!(feature = "metal") {
        "metal"
    } else {
        "cpu"
    }
}

/// Slim path: just the /health route, no workers, no inference. For early
/// smoke tests of CI artifacts where the model files aren't shipped.
async fn run_server_only_health(args: &Args) -> Result<()> {
    let app = Router::new().route("/health", get(health));
    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "adventurer (health only) listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Watch the host-mounted assets dir and broadcast a DevReload event each
/// time something changes. Runs on its own std::thread because notify is
/// callback-based and we don't want it tangled with tokio's runtime. The
/// callback hops back into tokio via the AppState's broadcast channel.
fn spawn_dev_asset_watcher(path: String, app: state::AppState) {
    use notify::{Config, PollWatcher, RecursiveMode, Watcher};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    info!(%path, "dev-assets watcher starting (poll mode)");
    let last_fire = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(60)));
    let app_for_cb = app.clone();
    // PollWatcher (fs polling) is mandatory here: the assets dir is a docker
    // bind-mount, and inotify events from the host do NOT propagate inside
    // the container. RecommendedWatcher would silently install an inotify
    // hook that never fires. Poll every 750ms — fast enough that an asset
    // edit shows up by the time we tab back to the browser, slow enough to
    // be invisible CPU-wise.
    let cfg = Config::default()
        .with_poll_interval(Duration::from_millis(750))
        .with_compare_contents(false); // mtime is enough; cheaper.
    let mut watcher: PollWatcher = match PollWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if res.is_err() { return; }
            // Debounce — file editors often fire several events per save
            // (write, chmod, rename); collapse to one reload per ~500ms.
            let mut last = last_fire.lock().unwrap();
            let now = Instant::now();
            if now.duration_since(*last) < Duration::from_millis(500) { return; }
            *last = now;
            tracing::info!("dev-asset change detected → broadcasting reload");
            app_for_cb.broadcast(state::Event::DevReload {});
        },
        cfg,
    ) {
        Ok(w) => w,
        Err(e) => { tracing::warn!(?e, "dev watcher init failed"); return; }
    };
    if let Err(e) = watcher.watch(std::path::Path::new(&path), RecursiveMode::Recursive) {
        tracing::warn!(?e, "dev watcher watch() failed");
        return;
    }
    info!(%path, "dev-assets watcher running");
    // Keep `watcher` in scope (dropping it would unsubscribe from the OS
    // notifications). Park forever; the parent process exit kills the thread.
    let _keepalive = watcher;
    loop { std::thread::park(); }
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(?e, "couldn't install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    info!("shutdown signal received");
}
