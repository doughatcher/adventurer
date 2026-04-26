//! `rust-embed` bundle of the dnd-stage UI. Compiles HTML/JS/CSS into the
//! binary's `.text` section so the server has zero runtime UI dependencies.

use axum::{
    body::Body,
    extract::Path as AxumPath,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/assets/client/"]
pub struct Assets;

/// Root handler: serve index.html with a cache-busting query string injected
/// into the asset URLs (mirrors `dnd-stage/server/main.py::root()`).
pub async fn index() -> impl IntoResponse {
    match Assets::get("index.html") {
        Some(file) => {
            let mut html = String::from_utf8_lossy(file.data.as_ref()).into_owned();
            let ts = chrono::Utc::now().timestamp();
            html = html.replace("/static/style.css\"", &format!("/static/style.css?v={ts}\""));
            html = html.replace("/static/stage.js\"", &format!("/static/stage.js?v={ts}\""));
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
        }
        None => (StatusCode::NOT_FOUND, "index.html not embedded").into_response(),
    }
}

/// `/static/{*path}` handler. Mime guessed from extension.
pub async fn static_file(AxumPath(path): AxumPath<String>) -> Response {
    match Assets::get(path.as_str()) {
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
