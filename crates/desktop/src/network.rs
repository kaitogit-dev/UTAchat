use std::{env, process::{Command as ProcessCommand, Stdio}, sync::mpsc::{self, Receiver, Sender}, thread, time::Duration};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Method};
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{mpsc as async_mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;
use utachat_shared::{ApiError, AuthResponse, Channel, ChatMessage, CredentialRequest, GatewayMessage, InviteResponse, Member, MessageRequest, NameRequest, PublicUser, Server};

#[derive(Clone)]
pub enum Command {
    Register { username: String, password: String },
    Login { username: String, password: String },
    Logout,
    FetchServers,
    SelectServer(Uuid),
    SelectChannel(Uuid),
    LoadOlder { channel: Uuid, before: Uuid },
    SendMessage { channel: Uuid, content: String },
    CreateServer(String),
    CreateChannel { server: Uuid, name: String },
    JoinInvite(String),
    FetchInvite(Uuid),
}

pub enum UiEvent {
    Authenticated(AuthResponse),
    LoggedOut,
    Servers(Vec<Server>),
    Channels { server: Uuid, items: Vec<Channel> },
    Members { server: Uuid, items: Vec<Member> },
    Messages { channel: Uuid, items: Vec<ChatMessage> },
    OlderMessages { channel: Uuid, items: Vec<ChatMessage> },
    MessagePosted(ChatMessage),
    CreatedServer(Server),
    CreatedChannel(Channel),
    JoinedServer(Server),
    InviteCode(String),
    Gateway(GatewayMessage),
    Connection(bool),
    Error(String),
}

pub struct Network {
    pub commands: async_mpsc::UnboundedSender<Command>,
    pub events: Receiver<UiEvent>,
}

pub fn start() -> Network {
    let (commands, receiver) = async_mpsc::unbounded_channel();
    let (events, messages) = mpsc::channel();
    thread::Builder::new().name("utachat-network".to_owned()).spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("network runtime");
        runtime.block_on(run(receiver, events));
    }).expect("network thread");
    Network { commands, events: messages }
}

async fn request<T: DeserializeOwned>(client: &Client, method: Method, url: &str, token: Option<&str>, body: Option<&impl Serialize>) -> Result<T, String> {
    let mut builder = client.request(method, url);
    if let Some(token) = token {
        builder = builder.bearer_auth(token);
    }
    if let Some(body) = body {
        builder = builder.json(body);
    }
    let response = builder.send().await.map_err(|error| format!("Cannot reach UTAchat server: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let error = response.json::<ApiError>().await.map(|payload| payload.error)
            .unwrap_or_else(|_| format!("Server returned {status}"));
        return Err(error);
    }
    response.json::<T>().await.map_err(|error| format!("Invalid server response: {error}"))
}

async fn get<T: DeserializeOwned>(client: &Client, base: &str, path: &str, token: &str) -> Result<T, String> {
    request::<T>(client, Method::GET, &format!("{base}{path}"), Some(token), Option::<&()>::None).await
}

async fn post<T: DeserializeOwned, B: Serialize>(client: &Client, base: &str, path: &str, token: Option<&str>, body: &B) -> Result<T, String> {
    request(client, Method::POST, &format!("{base}{path}"), token, Some(body)).await
}

fn credential() -> Option<keyring::Entry> {
    keyring::Entry::new("UTAchat", "session").ok()
}

fn persist_session(token: &str) {
    if let Some(entry) = credential() {
        let _ = entry.set_password(token);
    }
}

fn erase_session() {
    if let Some(entry) = credential() {
        let _ = entry.delete_credential();
    }
}

async fn load_servers(client: &Client, base: &str, token: &str, events: &Sender<UiEvent>) -> Result<(), String> {
    let servers = get::<Vec<Server>>(client, base, "/v1/servers", token).await?;
    let _ = events.send(UiEvent::Servers(servers));
    Ok(())
}


#[derive(serde::Deserialize)]
struct Configuration {
    api_url: String,
}

fn configured_base() -> String {
    if let Ok(base) = env::var("UTACHAT_API_URL") {
        return base.trim_end_matches('/').to_owned();
    }
    let configured = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|folder| folder.join("config.json")))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| serde_json::from_str::<Configuration>(&json).ok())
        .map(|config| config.api_url);
    configured.unwrap_or_else(|| "http://127.0.0.1:4800".to_owned())
        .trim_end_matches('/').to_owned()
}

async fn service_healthy(client: &Client, base: &str) -> bool {
    match client.get(format!("{base}/v1/health"))
        .timeout(Duration::from_millis(900))
        .send().await
    {
        Ok(response) if response.status().is_success() => {
            response.json::<serde_json::Value>().await
                .map(|body| body.get("service").and_then(|s| s.as_str()) == Some("utachat"))
                .unwrap_or(false)
        }
        _ => false,
    }
}

async fn ensure_local_service(client: &Client, base: &str) -> Result<(), String> {
    if service_healthy(client, base).await {
        return Ok(());
    }
    let path = env::current_exe()
        .map_err(|e| format!("Cannot locate UTAchat installation: {e}"))?
        .parent()
        .ok_or("Cannot locate UTAchat installation folder")?
        .join("Service")
        .join("utachat-server.exe");
    if !path.is_file() {
        return Err("UTAchat service is missing. Extract the complete application folder.".into());
    }
    let mut process = ProcessCommand::new(&path);
    process.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x08000000);
    }
    let mut child = process.spawn()
        .map_err(|e| format!("Cannot start UTAchat service: {e}"))?;
    for _ in 0..80 {
        if service_healthy(client, base).await {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("UTAchat service exited ({status}). Check %LOCALAPPDATA%\\UTAchat\\server.log"));
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    Err("UTAchat service did not become ready. Check %LOCALAPPDATA%\\UTAchat\\server.log".into())
}

async fn run(mut commands: async_mpsc::UnboundedReceiver<Command>, events: Sender<UiEvent>) {
    let base = configured_base();
    let client = Client::builder().timeout(Duration::from_secs(15)).build().expect("HTTP client");
    if base == "http://127.0.0.1:4800" {
        if let Err(error) = ensure_local_service(&client, &base).await {
            let _ = events.send(UiEvent::Error(error));
            return;
        }
    }
    let (session_sender, session_receiver) = watch::channel::<Option<String>>(None);
    tokio::spawn(gateway_loop(base.clone(), session_receiver, events.clone()));
    let mut token = credential().and_then(|entry| entry.get_password().ok());
    if let Some(saved) = token.clone() {
        match get::<PublicUser>(&client, &base, "/v1/me", &saved).await {
            Ok(user) => {
                let _ = session_sender.send(Some(saved.clone()));
                let _ = events.send(UiEvent::Authenticated(AuthResponse { token: saved.clone(), user }));
                if let Err(error) = load_servers(&client, &base, &saved, &events).await {
                    let _ = events.send(UiEvent::Error(error));
                }
            }
            Err(error) => {
                token = None;
                let _ = events.send(UiEvent::Error(error));
            }
        }
    }
    while let Some(command) = commands.recv().await {
        let result: Result<(), String> = match command {
            Command::Login { username, password } => {
                match post::<AuthResponse, _>(&client, &base, "/v1/auth/login", None, &CredentialRequest { username, password }).await {
                    Ok(auth) => {
                        persist_session(&auth.token);
                        token = Some(auth.token.clone());
                        let _ = session_sender.send(token.clone());
                        let _ = events.send(UiEvent::Authenticated(auth));
                        load_servers(&client, &base, token.as_ref().unwrap(), &events).await
                    }
                    Err(error) => Err(error),
                }
            }
            Command::Register { username, password } => {
                match post::<AuthResponse, _>(&client, &base, "/v1/auth/register", None, &CredentialRequest { username, password }).await {
                    Ok(auth) => {
                        persist_session(&auth.token);
                        token = Some(auth.token.clone());
                        let _ = session_sender.send(token.clone());
                        let _ = events.send(UiEvent::Authenticated(auth));
                        load_servers(&client, &base, token.as_ref().unwrap(), &events).await
                    }
                    Err(error) => Err(error),
                }
            }
            Command::Logout => {
                if let Some(active) = token.take() {
                    let _ = client.post(format!("{base}/v1/auth/logout")).bearer_auth(&active).send().await;
                }
                erase_session();
                let _ = session_sender.send(None);
                let _ = events.send(UiEvent::LoggedOut);
                Ok(())
            }
            other => {
                if let Some(active) = &token {
                    execute(other, &client, &base, active, &events).await
                } else {
                    Err("Sign in to continue".to_string())
                }
            }
        };
        if let Err(error) = result {
            let _ = events.send(UiEvent::Error(error));
        }
    }
}

async fn execute(command: Command, client: &Client, base: &str, token: &str, events: &Sender<UiEvent>) -> Result<(), String> {
    match command {
        Command::FetchServers => load_servers(client, base, token, events).await?,
        Command::SelectServer(server) => {
            let channels = get(client, base, &format!("/v1/servers/{server}/channels"), token).await?;
            let members = get(client, base, &format!("/v1/servers/{server}/members"), token).await?;
            let _ = events.send(UiEvent::Channels { server, items: channels });
            let _ = events.send(UiEvent::Members { server, items: members });
        }
        Command::SelectChannel(channel) => {
            let messages = get(client, base, &format!("/v1/channels/{channel}/messages"), token).await?;
            let _ = events.send(UiEvent::Messages { channel, items: messages });
        }
        Command::LoadOlder { channel, before } => {
            let messages = get(client, base, &format!("/v1/channels/{channel}/messages?before={before}"), token).await?;
            let _ = events.send(UiEvent::OlderMessages { channel, items: messages });
        }
        Command::SendMessage { channel, content } => {
            let message = post::<ChatMessage, _>(client, base, &format!("/v1/channels/{channel}/messages"), Some(token), &MessageRequest { content }).await?;
            let _ = events.send(UiEvent::MessagePosted(message));
        }
        Command::CreateServer(name) => {
            let server = post::<Server, _>(client, base, "/v1/servers", Some(token), &NameRequest { name }).await?;
            let _ = events.send(UiEvent::CreatedServer(server));
            load_servers(client, base, token, events).await?;
        }
        Command::CreateChannel { server, name } => {
            let channel = post::<Channel, _>(client, base, &format!("/v1/servers/{server}/channels"), Some(token), &NameRequest { name }).await?;
            let _ = events.send(UiEvent::CreatedChannel(channel));
        }
        Command::JoinInvite(code) => {
            let server = post::<Server, _>(client, base, &format!("/v1/invites/{}/join", code.trim()), Some(token), &()).await?;
            let _ = events.send(UiEvent::JoinedServer(server));
            load_servers(client, base, token, events).await?;
        }
        Command::FetchInvite(server) => {
            let code = get::<InviteResponse>(client, base, &format!("/v1/servers/{server}/invite"), token).await?;
            let _ = events.send(UiEvent::InviteCode(code.code));
        }
        Command::Login { .. } | Command::Register { .. } | Command::Logout => {}
    }
    Ok(())
}

async fn gateway_loop(base: String, mut session: watch::Receiver<Option<String>>, events: Sender<UiEvent>) {
    let endpoint = if let Some(host) = base.strip_prefix("https://") {
        format!("wss://{host}/v1/gateway")
    } else if let Some(host) = base.strip_prefix("http://") {
        format!("ws://{host}/v1/gateway")
    } else {
        let _ = events.send(UiEvent::Error("UTACHAT_API_URL must start with http:// or https://".into()));
        return;
    };
    loop {
        let Some(token) = session.borrow().clone() else {
            if session.changed().await.is_err() { return; }
            continue;
        };
        match connect_async(endpoint.as_str()).await {
            Ok((mut socket, _)) => {
                let hello = serde_json::to_string(&GatewayMessage::Hello { token }).unwrap();
                if socket.send(Message::Text(hello.into())).await.is_ok() {
                    loop {
                        tokio::select! {
                            change = session.changed() => {
                                if change.is_err() { return; }
                                break;
                            }
                            frame = socket.next() => match frame {
                                Some(Ok(Message::Text(raw))) => {
                                    if let Ok(event) = serde_json::from_str::<GatewayMessage>(&raw) {
                                        if matches!(event, GatewayMessage::Ready { .. }) {
                                            let _ = events.send(UiEvent::Connection(true));
                                        }
                                        let _ = events.send(UiEvent::Gateway(event));
                                    }
                                }
                                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(_) => {}
        }
        let _ = events.send(UiEvent::Connection(false));
        tokio::select! {
            changed = session.changed() => { if changed.is_err() { return; } }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
    }
}
