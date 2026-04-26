//! WebSocket handler — broadcasts state/panel/transcript/decision events to all
//! connected clients. Same payload shape as `dnd-stage`'s `/ws` so the existing
//! `client/stage.js` consumer doesn't need to change.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::api::AppContext;
use crate::state::Event;

pub async fn ws_handler(
    State(ctx): State<Arc<AppContext>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ctx))
}

async fn handle_socket(socket: WebSocket, ctx: Arc<AppContext>) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = ctx.state.subscribe();

    // Send the initial snapshot — same shape as dnd-stage's "init" message.
    let snap = ctx.state.snapshot().await;
    let init = Event::Init {
        panels: snap.panels,
        transcript: snap.transcript,
        state: snap.state,
    };
    if let Ok(text) = serde_json::to_string(&init) {
        if sender.send(Message::Text(text)).await.is_err() {
            return;
        }
    }

    // Two tasks:
    //   1. forward broadcast events to the client
    //   2. drain incoming messages (mostly keep-alive pings)
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(ev) => {
                    let Ok(text) = serde_json::to_string(&ev) else { continue };
                    if sender.send(Message::Text(text)).await.is_err() {
                        debug!("ws client disconnected (send failed)");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(missed = n, "ws client lagged broadcast");
                    // keep going; client will catch up via subsequent events
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(p))) => {
                    if sender.send(Message::Pong(p)).await.is_err() { break; }
                }
                Some(Ok(_)) => { /* ignore text/binary/pong from client */ }
                Some(Err(e)) => {
                    debug!(?e, "ws receive error");
                    break;
                }
            },
        }
    }
    debug!("ws session ended");
}
