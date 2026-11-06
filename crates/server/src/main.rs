mod auth;
mod data;
mod direct;
mod message_actions;
mod error;
mod gateway;

use std::{env, net::SocketAddr};
use axum::{Router, routing::{get, post}};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::broadcast;
use utachat_shared::GatewayMessage;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub events: broadcast::Sender<Broadcast>,
}

#[derive(Clone)]
pub enum BroadcastTarget {
    Server(Uuid),
    Users(Vec<Uuid>),
}

#[derive(Clone)]
pub struct Broadcast {
    pub target: BroadcastTarget,
    pub event: GatewayMessage,
}

impl Broadcast {
    pub fn server(server_id: Uuid, event: GatewayMessage) -> Self {
        Self { target: BroadcastTarget::Server(server_id), event }
    }

    pub fn users(users: Vec<Uuid>, event: GatewayMessage) -> Self {
        Self { target: BroadcastTarget::Users(users), event }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "utachat_server=info".into()))
        .init();
    let database_url = env::var("DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    let (events, _) = broadcast::channel(2048);
    let state = AppState { db: pool, events };
    let router = Router::new()
        .route("/v1/health", get(|| async { axum::Json(serde_json::json!({"service": "utachat"})) }))
        .route("/v1/auth/register", post(auth::register))
        .route("/v1/auth/login", post(auth::login))
        .route("/v1/auth/logout", post(auth::logout))
        .route("/v1/me", get(auth::me))
        .route("/v1/servers", get(data::list_servers).post(data::create_server))
        .route("/v1/servers/{server}/channels", get(data::list_channels).post(data::create_channel))
        .route("/v1/servers/{server}/members", get(data::list_members))
        .route("/v1/servers/{server}/invite", get(data::get_invite))
        .route("/v1/invites/{code}/join", post(data::join_invite))
        .route("/v1/channels/{channel}/messages", get(data::list_messages).post(data::send_message))
        .route("/v1/channels/{channel}/messages/{message}", axum::routing::patch(message_actions::edit).delete(message_actions::delete))
        .route("/v1/channels/{channel}/messages/{message}/reactions", post(message_actions::react))
        .route("/v1/friends", get(direct::friends))
        .route("/v1/friends/requests", post(direct::request_friend))
        .route("/v1/friends/{peer}/accept", post(direct::accept_friend))
        .route("/v1/friends/{peer}", axum::routing::delete(direct::remove_friend))
        .route("/v1/dms", get(direct::list_threads).post(direct::create_thread))
        .route("/v1/dms/{thread}/messages", get(direct::list_messages).post(direct::send_message))
        .route("/v1/dms/{thread}/messages/{message}", axum::routing::patch(direct::edit_message).delete(direct::delete_message))
        .route("/v1/gateway", get(gateway::upgrade))
        .with_state(state);
    let address: SocketAddr = match env::var("UTACHAT_LISTEN_ADDR") {
        Ok(address) => address.parse()?,
        Err(_) => format!("0.0.0.0:{}", env::var("PORT").unwrap_or_else(|_| "8080".to_owned())).parse()?,
    };
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "UTAchat backend ready");
    axum::serve(listener, router).await?;
    Ok(())
}
