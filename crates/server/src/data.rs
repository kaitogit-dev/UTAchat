use axum::{extract::{Path, Query, State}, http::{HeaderMap, StatusCode}, Json};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{Channel, ChatMessage, GatewayMessage, InviteResponse, Member, MessageRequest, NameRequest, PublicUser, Server, ReactionCount};
use crate::{AppState, Broadcast, auth::current_user, error::{ApiResult, AppError}};

#[derive(Deserialize)]
pub struct MessagesQuery {
    before: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn membership(state: &AppState, server: Uuid, user: Uuid) -> ApiResult<String> {
    sqlx::query_scalar::<_, String>("SELECT role FROM memberships WHERE server_id = $1 AND user_id = $2")
        .bind(server).bind(user).fetch_optional(&state.db).await?
        .ok_or_else(AppError::forbidden)
}

pub async fn channel_server(state: &AppState, channel: Uuid) -> ApiResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT server_id FROM channels WHERE id = $1")
        .bind(channel).fetch_optional(&state.db).await?
        .ok_or_else(AppError::not_found)
}

fn server_from(row: sqlx::postgres::PgRow) -> Server {
    Server { id: row.get("id"), name: row.get("name"), owner_id: row.get("owner_id") }
}

fn channel_from(row: sqlx::postgres::PgRow) -> Channel {
    Channel { id: row.get("id"), server_id: row.get("server_id"), name: row.get("name"), position: row.get("position") }
}

pub fn message_from(row: sqlx::postgres::PgRow) -> ChatMessage {
    ChatMessage {
        id: row.get("id"), channel_id: row.get("channel_id"),
        author: PublicUser { id: row.get("author_id"), username: row.get("username") },
        content: row.get("content"), created_at: row.get("created_at"),
        edited_at: row.get("edited_at"), reply_to: row.get("reply_to"), pinned_at: row.get("pinned_at"),
        reactions: serde_json::from_value::<Vec<ReactionCount>>(row.get("reactions")).unwrap_or_default(),
    }
}

pub async fn list_servers(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<Server>>> {
    let user = current_user(&state, &headers).await?;
    let rows = sqlx::query("SELECT s.id, s.name, s.owner_id FROM servers s JOIN memberships m ON m.server_id = s.id WHERE m.user_id = $1 ORDER BY m.joined_at")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(server_from).collect()))
}

pub async fn create_server(State(state): State<AppState>, headers: HeaderMap, Json(input): Json<NameRequest>) -> ApiResult<(StatusCode, Json<Server>)> {
    let user = current_user(&state, &headers).await?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(AppError::bad_request("Server name must be 1-64 characters"));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO servers (id, name, owner_id, invite_code) VALUES ($1, $2, $3, $4)")
        .bind(id).bind(name).bind(user.id).bind(Uuid::new_v4().simple().to_string())
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memberships (server_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(id).bind(user.id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO channels (id, server_id, name, position) VALUES ($1, $2, 'general', 0)")
        .bind(Uuid::new_v4()).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(Server { id, name: name.to_string(), owner_id: user.id })))
}

pub async fn list_channels(State(state): State<AppState>, headers: HeaderMap, Path(server): Path<Uuid>) -> ApiResult<Json<Vec<Channel>>> {
    let user = current_user(&state, &headers).await?;
    membership(&state, server, user.id).await?;
    let rows = sqlx::query("SELECT id, server_id, name, position FROM channels WHERE server_id = $1 ORDER BY position, created_at")
        .bind(server).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(channel_from).collect()))
}

pub async fn create_channel(State(state): State<AppState>, headers: HeaderMap, Path(server): Path<Uuid>, Json(input): Json<NameRequest>) -> ApiResult<(StatusCode, Json<Channel>)> {
    let user = current_user(&state, &headers).await?;
    if !matches!(membership(&state, server, user.id).await?.as_str(), "owner" | "moderator") {
        return Err(AppError::forbidden());
    }
    let name = input.name.trim().to_lowercase().replace(' ', "-");
    if !(1..=48).contains(&name.len()) || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(AppError::bad_request("Channel name must use 1-48 letters, digits, hyphens or underscores"));
    }
    let id = Uuid::new_v4();
    let result = sqlx::query("INSERT INTO channels (id, server_id, name, position) VALUES ($1, $2, $3, COALESCE((SELECT MAX(position) + 1 FROM channels WHERE server_id = $4), 0))")
        .bind(id).bind(server).bind(&name).bind(server).execute(&state.db).await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => return Err(AppError::conflict("Channel already exists")),
        Err(error) => return Err(error.into()),
    }
    let channel = sqlx::query("SELECT id, server_id, name, position FROM channels WHERE id = $1")
        .bind(id).fetch_one(&state.db).await.map(channel_from)?;
    let _ = state.events.send(Broadcast::server(server, GatewayMessage::ChannelCreated { channel: channel.clone() }));
    Ok((StatusCode::CREATED, Json(channel)))
}

pub async fn list_members(State(state): State<AppState>, headers: HeaderMap, Path(server): Path<Uuid>) -> ApiResult<Json<Vec<Member>>> {
    let user = current_user(&state, &headers).await?;
    membership(&state, server, user.id).await?;
    let rows = sqlx::query("SELECT u.id, u.username, m.role FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.server_id = $1 ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'moderator' THEN 1 ELSE 2 END, LOWER(u.username)")
        .bind(server).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(|r| Member { user: PublicUser { id: r.get("id"), username: r.get("username") }, role: r.get("role") }).collect()))
}

pub async fn get_invite(State(state): State<AppState>, headers: HeaderMap, Path(server): Path<Uuid>) -> ApiResult<Json<InviteResponse>> {
    let user = current_user(&state, &headers).await?;
    membership(&state, server, user.id).await?;
    let code = sqlx::query_scalar::<_, String>("SELECT invite_code FROM servers WHERE id = $1")
        .bind(server).fetch_optional(&state.db).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(InviteResponse { code }))
}

pub async fn join_invite(State(state): State<AppState>, headers: HeaderMap, Path(code): Path<String>) -> ApiResult<Json<Server>> {
    let user = current_user(&state, &headers).await?;
    let row = sqlx::query("SELECT id, name, owner_id FROM servers WHERE invite_code = $1")
        .bind(code.trim()).fetch_optional(&state.db).await?.ok_or_else(AppError::not_found)?;
    let server = server_from(row);
    let banned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM server_bans WHERE server_id=$1 AND user_id=$2)")
        .bind(server.id).bind(user.id).fetch_one(&state.db).await?;
    if banned {return Err(AppError::forbidden());}
    sqlx::query("INSERT INTO memberships (server_id, user_id, role) SELECT $1, $2, 'member' WHERE NOT EXISTS (SELECT 1 FROM server_bans WHERE server_id=$1 AND user_id=$2) ON CONFLICT DO NOTHING")
        .bind(server.id).bind(user.id).execute(&state.db).await?;
    let member = Member { user, role: "member".to_string() };
    let _ = state.events.send(Broadcast::server(server.id, GatewayMessage::MemberJoined { server_id: server.id, member }));
    Ok(Json(server))
}

pub async fn channel_info(State(state): State<AppState>, headers: HeaderMap, Path(channel): Path<Uuid>) -> ApiResult<Json<Channel>> {
    let user = current_user(&state, &headers).await?;
    let server = channel_server(&state, channel).await?;
    membership(&state, server, user.id).await?;
    let row = sqlx::query("SELECT id,server_id,name,position FROM channels WHERE id=$1")
        .bind(channel).fetch_one(&state.db).await?;
    Ok(Json(channel_from(row)))
}

pub async fn list_messages(State(state): State<AppState>, headers: HeaderMap, Path(channel): Path<Uuid>, Query(query): Query<MessagesQuery>) -> ApiResult<Json<Vec<ChatMessage>>> {
    let user = current_user(&state, &headers).await?;
    let server = channel_server(&state, channel).await?;
    membership(&state, server, user.id).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let rows = if let Some(before) = query.before {
        sqlx::query(
            "SELECT m.id, m.channel_id, m.author_id, u.username, m.content, m.created_at, m.edited_at, m.reply_to, m.pinned_at, COALESCE((SELECT jsonb_agg(jsonb_build_object('kind', grouped.kind, 'count', grouped.total)) FROM (SELECT kind, COUNT(*) AS total FROM message_reactions WHERE message_id = m.id GROUP BY kind) grouped), '[]'::jsonb) AS reactions FROM messages m JOIN users u ON u.id = m.author_id WHERE m.channel_id = $1 AND (m.created_at, m.id) < (SELECT created_at, id FROM messages WHERE id = $2 AND channel_id = $3) ORDER BY m.created_at DESC, m.id DESC LIMIT $4"
        )
            .bind(channel).bind(before).bind(channel).bind(limit).fetch_all(&state.db).await?
    } else {
        sqlx::query(
            "SELECT m.id, m.channel_id, m.author_id, u.username, m.content, m.created_at, m.edited_at, m.reply_to, m.pinned_at, COALESCE((SELECT jsonb_agg(jsonb_build_object('kind', grouped.kind, 'count', grouped.total)) FROM (SELECT kind, COUNT(*) AS total FROM message_reactions WHERE message_id = m.id GROUP BY kind) grouped), '[]'::jsonb) AS reactions FROM messages m JOIN users u ON u.id = m.author_id WHERE m.channel_id = $1 ORDER BY m.created_at DESC, m.id DESC LIMIT $2"
        )
            .bind(channel).bind(limit).fetch_all(&state.db).await?
    };
    let mut messages: Vec<_> = rows.into_iter().map(message_from).collect();
    messages.reverse();
    Ok(Json(messages))
}

pub async fn send_message(State(state): State<AppState>, headers: HeaderMap, Path(channel): Path<Uuid>, Json(input): Json<MessageRequest>) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    let user = current_user(&state, &headers).await?;
    let server = channel_server(&state, channel).await?;
    membership(&state, server, user.id).await?;
    let text = input.content.trim();
    if text.is_empty() || text.chars().count() > 4000 {
        return Err(AppError::bad_request("Message must contain 1-4000 characters"));
    }
    if let Some(parent) = input.reply_to {
        let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM messages WHERE id = $1 AND channel_id = $2")
            .bind(parent).bind(channel).fetch_optional(&state.db).await?;
        if exists.is_none() { return Err(AppError::bad_request("Reply target not found in channel")); }
    }
    let id = Uuid::new_v4();
    let row = sqlx::query("INSERT INTO messages (id, channel_id, author_id, content, reply_to) VALUES ($1, $2, $3, $4, $5) RETURNING created_at")
        .bind(id).bind(channel).bind(user.id).bind(text).bind(input.reply_to).fetch_one(&state.db).await?;
    let message = ChatMessage { id, channel_id: channel, author: user.clone(), content: text.to_string(), created_at: row.get("created_at"), edited_at: None, reply_to: input.reply_to, reactions: Vec::new(), pinned_at: None };
    if let Err(error)=crate::profile::mention_notifications(&state,server,channel,message.id,&user,&message.content).await {
        tracing::warn!(message_id=%message.id,error=%error.message,"Mention notifications could not be persisted");
    }
    let _ = state.events.send(Broadcast::server(server, GatewayMessage::MessageCreated { message: message.clone() }));
    Ok((StatusCode::CREATED, Json(message)))
}
