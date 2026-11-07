use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicUser {
    pub id: Uuid,
    pub username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: PublicUser,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub position: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub user: PublicUser,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author: PublicUser,
    pub content: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub edited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
    #[serde(default)]
    pub reactions: Vec<ReactionCount>,
    #[serde(default)]
    pub pinned_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReactionCount {
    pub kind: String,
    pub count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectThread {
    pub id: Uuid,
    pub peer: PublicUser,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Friend {
    pub user: PublicUser,
    pub status: String,
    pub incoming: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectRequest {
    pub username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditMessageRequest {
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReactionRequest {
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub username: String,
    pub bio: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileUpdate {
    pub bio: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub message: ChatMessage,
    pub channel_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelUnread {
    pub channel_id: Uuid,
    pub count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub actor: PublicUser,
    pub channel_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleRequest {
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanRequest {
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanEntry {
    pub user: PublicUser,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialRequest {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NameRequest {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageRequest {
    pub content: String,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteResponse {
    pub code: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayMessage {
    Hello { token: String },
    Ready { user_id: Uuid },
    MessageCreated { message: ChatMessage },
    MessageUpdated { message: ChatMessage },
    MessageDeleted { channel_id: Uuid, message_id: Uuid },
    ReactionsChanged { channel_id: Uuid },
    DirectThreadCreated { thread: DirectThread },
    DirectMessageCreated { message: ChatMessage },
    DirectMessageUpdated { message: ChatMessage },
    DirectMessageDeleted { channel_id: Uuid, message_id: Uuid },
    FriendsChanged,
    NotificationCreated { notification: Notification },
    ProfileUpdated { profile: UserProfile },
    PinsChanged { channel_id: Uuid },
    MemberRemoved { server_id: Uuid, user_id: Uuid },
    MemberRoleChanged { server_id: Uuid, user_id: Uuid, role: String },
    ChannelCreated { channel: Channel },
    ChannelDeleted { server_id: Uuid, channel_id: Uuid },
    MemberJoined { server_id: Uuid, member: Member },
    Error { message: String },
}
