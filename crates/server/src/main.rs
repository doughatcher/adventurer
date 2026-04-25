//! adventurer — single-binary live D&D session companion.
//!
//! Day-1 skeleton. Day-2 brings the axum port of `dnd-stage/server/main.py`:
//!   - REST: /api/panels, /api/transcript, /api/state, /api/voice, /api/characters/*,
//!           /api/session/end, /api/sessions, /api/recording/*, /api/proxy
//!   - WebSocket: /ws  (broadcast: panels, transcript, state, decision)
//!   - Static: /  ← rust-embed of dnd-stage/client/
//!   - Background: file watcher (notify), gemma update loops (debounced)

use std::net::SocketAddr;

use anyhow::Result;
use axum::{routing::get, Json, Router};
use clap::Parser;
use serde::Serialize;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about = "adventurer — live D&D session companion")]
struct Args {
    /// HTTP port. Production default mirrors dnd-stage's PORT (3200).
    #[arg(long, env = "PORT", default_value_t = 3200)]
    port: u16,

    /// Bind address.
    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    host: String,
}

#[derive(Serialize)]
struct Health {
    name: &'static str,
    version: &'static str,
    status: &'static str,
    backend: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let app = Router::new()
        .route("/health", get(health))
        .route("/", get(|| async { "adventurer — see /health (full UI lands Day 2)" }));

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, backend = cfg_backend(), "adventurer listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        // Block until SIGTERM. The earlier `.map(...).ok()` version returned
        // immediately because the inner future was constructed but never awaited.
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
