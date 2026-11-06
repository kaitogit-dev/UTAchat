#![cfg_attr(windows, windows_subsystem = "windows")]

mod auth;
mod data;
mod error;
mod gateway;

use std::{env, net::SocketAddr, path::PathBuf, time::Duration};
use axum::{Router, routing::{get, post}};
use sqlx::{SqlitePool, sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions}};
use tokio::sync::broadcast;
use utachat_shared::GatewayMessage;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub events: broadcast::Sender<Broadcast>,
}

#[derive(Clone)]
pub struct Broadcast {
    pub server_id: uuid::Uuid,
    pub event: GatewayMessage,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| env::temp_dir()))
        .join("UTAchat");
    std::fs::create_dir_all(&data_dir)?;
    let database_path = env::var_os("UTACHAT_DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("utachat.sqlite3"));
    if let Some(parent) = database_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("server.log"))?;
    let (writer, _guard) = tracing_appender::non_blocking(log_file);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "utachat_server=info".into()))
        .init();
    let outcome = serve(database_path).await;
    if let Err(error) = &outcome {
        tracing::error!(%error, "UTAchat service stopped");
    }
    outcome
}

async fn serve(database_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let options = SqliteConnectOptions::new()
        .filename(&database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(options).await?;
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
        .route("/v1/gateway", get(gateway::upgrade))
        .with_state(state);
    let addr: SocketAddr = env::var("UTACHAT_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:4800".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "UTAchat service ready");
    axum::serve(listener, router).await?;
    Ok(())
}
