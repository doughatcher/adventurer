//! `rust-embed` bundles for the DM stage UI (vendored from dnd-stage) AND
//! the player view UI (the read-only mobile-friendly companion served at `/join`).
//!
//! Both folders are compiled into the binary's `.text` section; the server has
//! zero runtime UI dependencies in production.
//!
//! **Dev / live-reload mode:** if env var `ADVENTURER_DEV_ASSETS` is set
//! (e.g. `/work/assets` when the launcher mounts the host repo), every asset
//! lookup tries the filesystem first and only falls back to the embedded
//! copy if the file isn't there. Means CSS / JS / HTML changes show up on a
//! browser reload without rebuilding the docker image.

use std::borrow::Cow;
use std::path::PathBuf;

use axum::{
    body::Body,
    extract::Path as AxumPath,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/assets/client/"]
pub struct DmAssets;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/assets/player/"]
pub struct PlayerAssets;

/// Asset kinds — picks the right subfolder when reading from the on-disk
/// dev-assets dir.
#[derive(Clone, Copy)]
enum Kind { Dm, Player }

fn dev_root() -> Option<PathBuf> {
    std::env::var("ADVENTURER_DEV_ASSETS").ok().map(PathBuf::from)
}

/// Look up an asset by name, dev-asset filesystem first, then rust-embed.
fn get_asset(kind: Kind, name: &str) -> Option<Cow<'static, [u8]>> {
    if let Some(root) = dev_root() {
        let sub = match kind { Kind::Dm => "client", Kind::Player => "player" };
        let p = root.join(sub).join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            tracing::debug!(path = %p.display(), "dev asset hit");
            return Some(Cow::Owned(bytes));
        }
    }
    let file = match kind {
        Kind::Dm     => DmAssets::get(name),
        Kind::Player => PlayerAssets::get(name),
    };
    file.map(|f| f.data)
}

/// Root handler — DM stage. Cache-bust the asset URLs the same way
/// `dnd-stage/server/main.py::root()` does, then inject our additive scripts
/// (qr-modal.js for the players/QR overlay, gamepad.js for controller nav,
/// transcript-style.js for ambient-sound styling) so they overlay the
/// existing vanilla-JS app without touching `stage.js`.
pub async fn index() -> impl IntoResponse {
    match get_asset(Kind::Dm, "index.html") {
        Some(bytes) => {
            let mut html = String::from_utf8_lossy(bytes.as_ref()).into_owned();
            let ts = chrono::Utc::now().timestamp();
            html = html.replace("/static/style.css\"", &format!("/static/style.css?v={ts}\""));
            html = html.replace("/static/stage.js\"", &format!("/static/stage.js?v={ts}\""));
            let inject = format!(
                concat!(
                    "<script src=\"/static/qr-modal.js?v={ts}\" defer></script>\n",
                    "<script src=\"/static/gamepad.js?v={ts}\" defer></script>\n",
                    "<script src=\"/static/transcript-style.js?v={ts}\" defer></script>\n",
                    "<script src=\"/static/dev-reload.js?v={ts}\" defer></script>\n",
                    "</body>"
                ),
                ts = ts
            );
            html = html.replace("</body>", &inject);
            (
                [
                    (header::CONTENT_TYPE,  "text/html; charset=utf-8".to_string()),
                    (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".to_string()),
                ],
                html,
            ).into_response()
        }
        None => (StatusCode::NOT_FOUND, "index.html not found").into_response(),
    }
}

/// Player view at `/join` — minimal mobile-friendly companion page.
/// Same cache-busting trick as the DM index — iPad Safari aggressively caches
/// JS, and we'd never know if our latest fix actually got loaded otherwise.
pub async fn player_index() -> impl IntoResponse {
    match get_asset(Kind::Player, "index.html") {
        Some(bytes) => {
            let mut html = String::from_utf8_lossy(bytes.as_ref()).into_owned();
            let ts = chrono::Utc::now().timestamp();
            html = html.replace("/static/player.css\"", &format!("/static/player.css?v={ts}\""));
            html = html.replace("/static/player.js\"",  &format!("/static/player.js?v={ts}\""));
            // Inject dev-reload.js so iPad / phone clients also reload when
            // an asset changes in dev mode.
            html = html.replace("</body>",
                &format!("<script src=\"/static/dev-reload.js?v={ts}\" defer></script>\n</body>"));
            (
                [
                    (header::CONTENT_TYPE,  "text/html; charset=utf-8".to_string()),
                    (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".to_string()),
                ],
                html,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "player/index.html not found").into_response(),
    }
}

/// `/static/{*path}` handler. DM assets first, then player assets.
pub async fn static_file(AxumPath(path): AxumPath<String>) -> Response {
    let bytes = get_asset(Kind::Dm, path.as_str())
        .or_else(|| get_asset(Kind::Player, path.as_str()));
    match bytes {
        Some(b) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE,  mime.as_ref().to_string()),
                    // Always no-cache so dev-mode edits + cache-busted versions
                    // are both honored without browser confusion.
                    (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".to_string()),
                ],
                Body::from(b.into_owned()),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, format!("not found: {path}")).into_response(),
    }
}
