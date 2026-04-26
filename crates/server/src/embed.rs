//! `rust-embed` bundles for the DM stage UI (vendored from dnd-stage) AND
//! the player view UI (the read-only mobile-friendly companion served at `/join`).
//!
//! Both folders are compiled into the binary's `.text` section; the server has
//! zero runtime UI dependencies.

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

/// Root handler — DM stage. Cache-bust the asset URLs the same way
/// `dnd-stage/server/main.py::root()` does, then inject our additive scripts
/// (qr-modal.js for the players/QR overlay, gamepad.js for controller nav)
/// so they overlay the existing vanilla-JS app without touching `stage.js`.
pub async fn index() -> impl IntoResponse {
    match DmAssets::get("index.html") {
        Some(file) => {
            let mut html = String::from_utf8_lossy(file.data.as_ref()).into_owned();
            let ts = chrono::Utc::now().timestamp();
            html = html.replace("/static/style.css\"", &format!("/static/style.css?v={ts}\""));
            html = html.replace("/static/stage.js\"", &format!("/static/stage.js?v={ts}\""));
            let inject = format!(
                concat!(
                    "<script src=\"/static/qr-modal.js?v={ts}\" defer></script>\n",
                    "<script src=\"/static/gamepad.js?v={ts}\" defer></script>\n",
                    "</body>"
                ),
                ts = ts
            );
            html = html.replace("</body>", &inject);
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
        }
        None => (StatusCode::NOT_FOUND, "index.html not embedded").into_response(),
    }
}

/// Player view at `/join` — minimal mobile-friendly companion page.
pub async fn player_index() -> impl IntoResponse {
    match PlayerAssets::get("index.html") {
        Some(file) => {
            let html = String::from_utf8_lossy(file.data.as_ref()).into_owned();
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
        }
        None => (StatusCode::NOT_FOUND, "player/index.html not embedded").into_response(),
    }
}

/// `/static/{*path}` handler. Tries DM assets first, then player assets,
/// then 404. (Player files have a `player-` prefix so collisions are rare.)
pub async fn static_file(AxumPath(path): AxumPath<String>) -> Response {
    let file = DmAssets::get(path.as_str())
        .or_else(|| PlayerAssets::get(path.as_str()));
    match file {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                Body::from(file.data),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, format!("not found: {path}")).into_response(),
    }
}
