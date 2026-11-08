use std::{env, sync::mpsc::{self, Receiver, Sender}, thread, time::Duration};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Method};
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{mpsc as async_mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;
use utachat_shared::{ApiError, AuthResponse, Channel, ChatMessage, CredentialRequest, GatewayMessage, InviteResponse, Member, MessageRequest, NameRequest, PublicUser, Server, DirectRequest, DirectThread, Friend, EditMessageRequest, ReactionRequest, UserProfile, ProfileUpdate, SearchHit, ChannelUnread, Notification, RoleRequest, BanRequest};

#[derive(Clone)]
pub enum Command {
    Register { username: String, password: String },
    Login { username: String, password: String },
    Logout,
    FetchServers,
    SelectServer(Uuid),
    SelectChannel(Uuid),
    LoadOlder { channel: Uuid, before: Uuid },
    SendMessage { channel: Uuid, content: String, reply_to: Option<Uuid> },
    CreateServer(String),
    CreateChannel { server: Uuid, name: String },
    JoinInvite(String),
    FetchInvite(Uuid),
    FetchDirects,
    FetchFriends,
    CreateDirect(String),
    AddFriend(String),
    AcceptFriend(Uuid),
    RemoveFriend(Uuid),
    SelectDirect(Uuid),
    LoadOlderDirect { thread: Uuid, before: Uuid },
    SendDirect { thread: Uuid, content: String, reply_to: Option<Uuid> },
    EditMessage { channel: Uuid, message: Uuid, content: String, direct: bool },
    DeleteMessage { channel: Uuid, message: Uuid, direct: bool },
    React { channel: Uuid, message: Uuid, kind: String },
    FetchProfile,
    OpenUserProfile(Uuid),
    UpdateProfile { bio: String, status: String },
    Search { server: Uuid, query: String },
    FetchPins(Uuid),
    SetPin { channel: Uuid, message: Uuid, pinned: bool },
    FetchNotifications,
    MarkNotificationsRead,
    FetchUnreads,
    MarkRead(Uuid),
    LocateChannel(Uuid),
    SetMemberRole { server: Uuid, user: Uuid, role: String },
    KickMember { server: Uuid, user: Uuid },
    BanMember { server: Uuid, user: Uuid, reason: String },
    RenameServer { server: Uuid, name: String },
    RenameChannel { server: Uuid, channel: Uuid, name: String },
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
    DirectThreads(Vec<DirectThread>),
    Friends(Vec<Friend>),
    DirectCreated(DirectThread),
    MessageUpdated(ChatMessage),
    MessageDeleted { channel: Uuid, message: Uuid },
    MessageReactions(Uuid),
    Profile(UserProfile),
    OtherProfile(UserProfile),
    ProfileSaved(UserProfile),
    LocatedChannel(Channel),
    SearchResults(Vec<SearchHit>),
    PinnedMessages(Vec<ChatMessage>),
    Notifications(Vec<Notification>),
    Unreads(Vec<ChannelUnread>),
    RoleChanged,
    MemberKicked(Uuid),
    MemberBanned(Uuid),
    ServerRenamed(Server),
    ChannelRenamed(Channel),
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

fn configured_base() -> Result<String, String> {
    let configured = env::var("UTACHAT_API_URL").ok().or_else(|| {
        env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|folder| folder.join("config").join("connection.json")))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|json| serde_json::from_str::<Configuration>(&json).ok())
            .map(|config| config.api_url)
    }).unwrap_or_default();
    let endpoint = configured.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return Err("UTAchat online service has not been configured.".to_owned());
    }
    let address = reqwest::Url::parse(endpoint)
        .map_err(|_| "Invalid UTAchat server address.".to_owned())?;
    let local = address.host_str().is_some_and(|host| host == "127.0.0.1" || host == "localhost");
    if address.scheme() != "https" && !(cfg!(debug_assertions) && address.scheme() == "http" && local) {
        return Err("UTAchat requires a secure online server address.".to_owned());
    }
    if address.host_str().is_none() || !address.username().is_empty() || address.password().is_some() || address.query().is_some() || address.fragment().is_some() || address.path() != "/" {
        return Err("Invalid UTAchat server address.".to_owned());
    }
    Ok(endpoint.to_owned())
}

async fn load_social(client: &Client, base: &str, token: &str, events: &Sender<UiEvent>) -> Result<(), String> {
    let threads = get::<Vec<DirectThread>>(client, base, "/v1/dms", token).await?;
    let friends = get::<Vec<Friend>>(client, base, "/v1/friends", token).await?;
    let _ = events.send(UiEvent::DirectThreads(threads));
    let _ = events.send(UiEvent::Friends(friends));
    let profile = get::<UserProfile>(client, base, "/v1/me/profile", token).await?;
    let _ = events.send(UiEvent::Profile(profile));
    let unread = get::<Vec<ChannelUnread>>(client, base, "/v1/unreads", token).await?;
    let _ = events.send(UiEvent::Unreads(unread));
    Ok(())
}

async fn request_empty(client: &Client, method: Method, url: String, token: &str, body: Option<&impl Serialize>) -> Result<(), String> {
    let mut builder = client.request(method, url).bearer_auth(token);
    if let Some(body) = body { builder = builder.json(body); }
    let response = builder.send().await.map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(response.json::<ApiError>().await.map(|value| value.error)
            .unwrap_or_else(|_| format!("Server returned {status}")));
    }
    Ok(())
}

async fn run(mut commands: async_mpsc::UnboundedReceiver<Command>, events: Sender<UiEvent>) {
    let base = match configured_base() {
        Ok(base) => base,
        Err(error) => {
            let _ = events.send(UiEvent::Error(error.clone()));
            while commands.recv().await.is_some() {
                let _ = events.send(UiEvent::Error(error.clone()));
            }
            return;
        }
    };
    let client = Client::builder().timeout(Duration::from_secs(15)).build().expect("HTTP client");
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
                if let Err(error) = load_social(&client, &base, &saved, &events).await {
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
                        match load_servers(&client, &base, token.as_ref().unwrap(), &events).await {
                            Ok(()) => load_social(&client, &base, token.as_ref().unwrap(), &events).await,
                            Err(error) => Err(error),
                        }
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
                        match load_servers(&client, &base, token.as_ref().unwrap(), &events).await {
                            Ok(()) => load_social(&client, &base, token.as_ref().unwrap(), &events).await,
                            Err(error) => Err(error),
                        }
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
            request_empty(client, Method::POST, format!("{base}/v1/channels/{channel}/read"), token, Option::<&()>::None).await?;
            let unreads = get::<Vec<ChannelUnread>>(client, base, "/v1/unreads", token).await?;
            let _ = events.send(UiEvent::Unreads(unreads));
        }
        Command::LoadOlder { channel, before } => {
            let messages = get(client, base, &format!("/v1/channels/{channel}/messages?before={before}"), token).await?;
            let _ = events.send(UiEvent::OlderMessages { channel, items: messages });
        }
        Command::SendMessage { channel, content, reply_to } => {
            let message = post::<ChatMessage, _>(client, base, &format!("/v1/channels/{channel}/messages"), Some(token), &MessageRequest { content, reply_to }).await?;
            let _ = events.send(UiEvent::MessagePosted(message));
        }
        Command::SendDirect { thread, content, reply_to } => {
            let message = post::<ChatMessage, _>(client, base, &format!("/v1/dms/{thread}/messages"), Some(token), &MessageRequest { content, reply_to }).await?;
            let _ = events.send(UiEvent::MessagePosted(message));
        }
        Command::SelectDirect(thread) => {
            let items = get::<Vec<ChatMessage>>(client, base, &format!("/v1/dms/{thread}/messages"), token).await?;
            let _ = events.send(UiEvent::Messages { channel: thread, items });
        }
        Command::LoadOlderDirect { thread, before } => {
            let items = get::<Vec<ChatMessage>>(client, base, &format!("/v1/dms/{thread}/messages?before={before}"), token).await?;
            let _ = events.send(UiEvent::OlderMessages { channel: thread, items });
        }
        Command::FetchDirects => {
            let items = get::<Vec<DirectThread>>(client, base, "/v1/dms", token).await?;
            let _ = events.send(UiEvent::DirectThreads(items));
        }
        Command::FetchFriends => {
            let items = get::<Vec<Friend>>(client, base, "/v1/friends", token).await?;
            let _ = events.send(UiEvent::Friends(items));
        }
        Command::CreateDirect(username) => {
            let item = post::<DirectThread, _>(client, base, "/v1/dms", Some(token), &DirectRequest { username }).await?;
            let _ = events.send(UiEvent::DirectCreated(item));
        }
        Command::AddFriend(username) => {
            request_empty(client, Method::POST, format!("{base}/v1/friends/requests"), token, Some(&DirectRequest { username })).await?;
            let items = get::<Vec<Friend>>(client, base, "/v1/friends", token).await?;
            let _ = events.send(UiEvent::Friends(items));
        }
        Command::AcceptFriend(peer) => {
            request_empty(client, Method::POST, format!("{base}/v1/friends/{peer}/accept"), token, Some(&())).await?;
            let items = get::<Vec<Friend>>(client, base, "/v1/friends", token).await?;
            let _ = events.send(UiEvent::Friends(items));
        }
        Command::RemoveFriend(peer) => {
            request_empty(client, Method::DELETE, format!("{base}/v1/friends/{peer}"), token, Option::<&()>::None).await?;
            let items = get::<Vec<Friend>>(client, base, "/v1/friends", token).await?;
            let _ = events.send(UiEvent::Friends(items));
        }
        Command::EditMessage { channel, message, content, direct } => {
            let path = if direct {
                format!("/v1/dms/{channel}/messages/{message}")
            } else {
                format!("/v1/channels/{channel}/messages/{message}")
            };
            let edited = request::<ChatMessage>(client, Method::PATCH, &format!("{base}{path}"), Some(token), Some(&EditMessageRequest { content })).await?;
            let _ = events.send(UiEvent::MessageUpdated(edited));
        }
        Command::DeleteMessage { channel, message, direct } => {
            let path = if direct {
                format!("/v1/dms/{channel}/messages/{message}")
            } else {
                format!("/v1/channels/{channel}/messages/{message}")
            };
            request_empty(client, Method::DELETE, format!("{base}{path}"), token, Option::<&()>::None).await?;
            let _ = events.send(UiEvent::MessageDeleted { channel, message });
        }
        Command::React { channel, message, kind } => {
            request_empty(client, Method::POST, format!("{base}/v1/channels/{channel}/messages/{message}/reactions"), token, Some(&ReactionRequest { kind })).await?;
            let _ = events.send(UiEvent::MessageReactions(channel));
        }
        Command::FetchProfile => {
            let profile = get::<UserProfile>(client, base, "/v1/me/profile", token).await?;
            let _ = events.send(UiEvent::Profile(profile));
        }
        Command::OpenUserProfile(user) => {
            let profile = get::<UserProfile>(client, base, &format!("/v1/users/{user}"), token).await?;
            let _ = events.send(UiEvent::OtherProfile(profile));
        }
        Command::UpdateProfile { bio, status } => {
            let profile = request::<UserProfile>(client, Method::PATCH, &format!("{base}/v1/me/profile"), Some(token), Some(&ProfileUpdate { bio, status })).await?;
            let _ = events.send(UiEvent::ProfileSaved(profile));
        }
        Command::Search { server, query } => {
            let search = reqwest::Url::parse_with_params(&format!("{base}/v1/servers/{server}/search"), &[("q", query.as_str())])
                .map_err(|error| error.to_string())?;
            let hits = request::<Vec<SearchHit>>(client, Method::GET, search.as_str(), Some(token), Option::<&()>::None).await?;
            let _ = events.send(UiEvent::SearchResults(hits));
        }
        Command::FetchPins(channel) => {
            let pinned = get::<Vec<ChatMessage>>(client, base, &format!("/v1/channels/{channel}/pins"), token).await?;
            let _ = events.send(UiEvent::PinnedMessages(pinned));
        }
        Command::SetPin { channel, message, pinned } => {
            let method = if pinned { Method::PUT } else { Method::DELETE };
            request_empty(client, method, format!("{base}/v1/channels/{channel}/messages/{message}/pin"), token, Option::<&()>::None).await?;
            let _ = events.send(UiEvent::MessageReactions(channel));
            let pinned = get::<Vec<ChatMessage>>(client, base, &format!("/v1/channels/{channel}/pins"), token).await?;
            let _ = events.send(UiEvent::PinnedMessages(pinned));
        }
        Command::FetchNotifications => {
            let items = get::<Vec<Notification>>(client, base, "/v1/notifications", token).await?;
            let _ = events.send(UiEvent::Notifications(items));
        }
        Command::MarkNotificationsRead => {
            request_empty(client, Method::POST, format!("{base}/v1/notifications/read"), token, Option::<&()>::None).await?;
            let items = get::<Vec<Notification>>(client, base, "/v1/notifications", token).await?;
            let _ = events.send(UiEvent::Notifications(items));
        }
        Command::FetchUnreads => {
            let items = get::<Vec<ChannelUnread>>(client, base, "/v1/unreads", token).await?;
            let _ = events.send(UiEvent::Unreads(items));
        }
        Command::LocateChannel(channel) => {
            let item = get::<Channel>(client, base, &format!("/v1/channels/{channel}/info"), token).await?;
            let _ = events.send(UiEvent::LocatedChannel(item));
        }
        Command::MarkRead(channel) => {
            request_empty(client, Method::POST, format!("{base}/v1/channels/{channel}/read"), token, Option::<&()>::None).await?;
            let items = get::<Vec<ChannelUnread>>(client, base, "/v1/unreads", token).await?;
            let _ = events.send(UiEvent::Unreads(items));
        }
        Command::SetMemberRole { server, user, role } => {
            request_empty(client, Method::PATCH, format!("{base}/v1/servers/{server}/members/{user}"), token, Some(&RoleRequest { role })).await?;
            let _ = events.send(UiEvent::RoleChanged);
            let members = get::<Vec<Member>>(client, base, &format!("/v1/servers/{server}/members"), token).await?;
            let _ = events.send(UiEvent::Members { server, items: members });
        }
        Command::KickMember { server, user } => {
            request_empty(client, Method::DELETE, format!("{base}/v1/servers/{server}/members/{user}"), token, Option::<&()>::None).await?;
            let _ = events.send(UiEvent::MemberKicked(user));
            let members = get::<Vec<Member>>(client, base, &format!("/v1/servers/{server}/members"), token).await?;
            let _ = events.send(UiEvent::Members { server, items: members });
        }
        Command::BanMember { server, user, reason } => {
            request_empty(client, Method::POST, format!("{base}/v1/servers/{server}/bans/{user}"), token, Some(&BanRequest { reason })).await?;
            let _ = events.send(UiEvent::MemberBanned(user));
            let members = get::<Vec<Member>>(client, base, &format!("/v1/servers/{server}/members"), token).await?;
            let _ = events.send(UiEvent::Members { server, items: members });
        }
        Command::RenameServer { server, name } => {
            let updated = request::<Server>(client, Method::PATCH, &format!("{base}/v1/servers/{server}"), Some(token), Some(&NameRequest { name })).await?;
            let _ = events.send(UiEvent::ServerRenamed(updated));
        }
        Command::RenameChannel { server, channel, name } => {
            let updated = request::<Channel>(client, Method::PATCH, &format!("{base}/v1/servers/{server}/channels/{channel}"), Some(token), Some(&NameRequest { name })).await?;
            let _ = events.send(UiEvent::ChannelRenamed(updated));
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
