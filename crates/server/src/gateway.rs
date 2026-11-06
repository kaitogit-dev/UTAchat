use axum::{extract::{State, ws::{Message, WebSocket, WebSocketUpgrade}}, response::Response};
use futures_util::{SinkExt, StreamExt};
use serde_json;
use tokio::time::{Duration, interval};
use utachat_shared::GatewayMessage;
use crate::{AppState, auth::session_user, data::membership};

pub async fn upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(32 * 1024).on_upgrade(move |socket| handle(socket, state))
}

async fn handle(mut socket: WebSocket, state: AppState) {
    let token = match tokio::time::timeout(Duration::from_secs(10), socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<GatewayMessage>(&text) {
            Ok(GatewayMessage::Hello { token }) => token,
            _ => return,
        },
        _ => return,
    };
    let user = match session_user(&state, &token).await {
        Ok(user) => user,
        Err(_) => {
            let _ = transmit(&mut socket, &GatewayMessage::Error { message: "Authentication failed".into() }).await;
            return;
        }
    };
    let mut receiver = state.events.subscribe();
    if transmit(&mut socket, &GatewayMessage::Ready { user_id: user.id }).await.is_err() {
        return;
    }
    let mut keepalive = interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            inbound = socket.next() => match inbound {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Ping(value))) => { if socket.send(Message::Pong(value)).await.is_err() { break; } },
                _ => {}
            },
            broadcast = receiver.recv() => match broadcast {
                Ok(message) => {
                    if membership(&state, message.server_id, user.id).await.is_ok() && transmit(&mut socket, &message.event).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if transmit(&mut socket, &GatewayMessage::Error { message: "Connection fell behind; refresh messages".into() }).await.is_err() { break; }
                }
                Err(_) => break,
            },
            _ = keepalive.tick() => {
                if socket.send(Message::Ping(Default::default())).await.is_err() { break; }
            }
        }
    }
}

async fn transmit(socket: &mut WebSocket, payload: &GatewayMessage) -> Result<(), axum::Error> {
    let data = serde_json::to_string(payload).expect("gateway serialization");
    socket.send(Message::Text(data.into())).await
}
