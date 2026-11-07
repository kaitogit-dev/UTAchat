use axum::{extract::{Path,State},http::{HeaderMap,StatusCode},Json};
use chrono::{DateTime,Utc};
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{ChannelUnread,GatewayMessage,Notification,ProfileUpdate,PublicUser,UserProfile};
use crate::{AppState,Broadcast,auth::current_user,data::{channel_server,membership},error::{ApiResult,AppError}};

fn profile(row:sqlx::postgres::PgRow)->UserProfile{
    UserProfile{id:row.get("id"),username:row.get("username"),bio:row.get("bio"),status:row.get("status")}
}

pub async fn get_profile(State(state):State<AppState>,headers:HeaderMap,Path(target):Path<Uuid>)->ApiResult<Json<UserProfile>>{
    current_user(&state,&headers).await?;
    let row=sqlx::query("SELECT id,username,bio,status FROM users WHERE id=$1")
        .bind(target).fetch_optional(&state.db).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(profile(row)))
}

pub async fn my_profile(State(state):State<AppState>,headers:HeaderMap)->ApiResult<Json<UserProfile>>{
    let user=current_user(&state,&headers).await?;
    let row=sqlx::query("SELECT id,username,bio,status FROM users WHERE id=$1")
        .bind(user.id).fetch_one(&state.db).await?;
    Ok(Json(profile(row)))
}

pub async fn update_profile(State(state):State<AppState>,headers:HeaderMap,Json(input):Json<ProfileUpdate>)->ApiResult<Json<UserProfile>>{
    let user=current_user(&state,&headers).await?;
    if input.bio.chars().count()>600 {
        return Err(AppError::bad_request("Bio must not exceed 600 characters"));
    }
    if !matches!(input.status.as_str(),"online"|"idle"|"dnd"|"invisible"){
        return Err(AppError::bad_request("Invalid availability status"));
    }
    let row=sqlx::query("UPDATE users SET bio=$1,status=$2 WHERE id=$3 RETURNING id,username,bio,status")
        .bind(input.bio.trim()).bind(&input.status).bind(user.id).fetch_one(&state.db).await?;
    let updated=profile(row);
    let _=state.events.send(Broadcast::users(vec![user.id],GatewayMessage::ProfileUpdated{profile:updated.clone()}));
    Ok(Json(updated))
}

fn notification(row:sqlx::postgres::PgRow)->Notification{
    Notification{
        id:row.get("id"),kind:row.get("kind"),
        actor:PublicUser{id:row.get("actor_id"),username:row.get("username")},
        channel_id:row.get("channel_id"),thread_id:row.get("thread_id"),
        message_id:row.get("message_id"),
        created_at:row.get("created_at"),read_at:row.get("read_at"),
    }
}

pub async fn save_notification(state:&AppState,recipient:Uuid,actor:&PublicUser,kind:&str,
    channel:Option<Uuid>,thread:Option<Uuid>,message:Option<Uuid>)->ApiResult<()>{
    if recipient==actor.id {return Ok(());}
    let id=Uuid::new_v4();
    let inserted=sqlx::query("INSERT INTO notifications(id,recipient_id,actor_id,kind,channel_id,thread_id,message_id) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING RETURNING id,created_at")
        .bind(id).bind(recipient).bind(actor.id).bind(kind).bind(channel).bind(thread).bind(message)
        .fetch_optional(&state.db).await?;
    if let Some(row)=inserted {
        let item=Notification{id:row.get("id"),kind:kind.to_owned(),actor:actor.clone(),
            channel_id:channel,thread_id:thread,message_id:message,
            created_at:row.get("created_at"),read_at:None};
        let _=state.events.send(Broadcast::users(vec![recipient],GatewayMessage::NotificationCreated{notification:item}));
    }
    Ok(())
}

pub async fn mention_notifications(state:&AppState,server:Uuid,channel:Uuid,message:Uuid,
    actor:&PublicUser,content:&str)->ApiResult<()>{
    let mut usernames:Vec<String>=content.split(|c:char|!(c.is_ascii_alphanumeric()||matches!(c,'_'|'.'|'-'|'@')))
        .filter_map(|word|word.strip_prefix('@'))
        .filter(|name|!name.is_empty()&&name.len()<=24)
        .map(|name|name.to_ascii_lowercase()).collect();
    usernames.sort();
    usernames.dedup();
    if usernames.is_empty(){return Ok(());}
    let rows=sqlx::query("SELECT DISTINCT u.id FROM users u JOIN memberships m ON m.user_id=u.id WHERE m.server_id=$1 AND LOWER(u.username)=ANY($2)")
        .bind(server).bind(&usernames).fetch_all(&state.db).await?;
    for row in rows{
        let user:Uuid=row.get("id");
        save_notification(state,user,actor,"mention",Some(channel),None,Some(message)).await?;
    }
    Ok(())
}

pub async fn list_notifications(State(state):State<AppState>,headers:HeaderMap)->ApiResult<Json<Vec<Notification>>>{
    let user=current_user(&state,&headers).await?;
    let rows=sqlx::query("SELECT n.id,n.kind,n.actor_id,u.username,n.channel_id,n.thread_id,n.message_id,n.created_at,n.read_at FROM notifications n JOIN users u ON u.id=n.actor_id WHERE n.recipient_id=$1 ORDER BY n.created_at DESC LIMIT 100")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(notification).collect()))
}

pub async fn clear_notifications(State(state):State<AppState>,headers:HeaderMap)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    sqlx::query("UPDATE notifications SET read_at=NOW() WHERE recipient_id=$1 AND read_at IS NULL")
        .bind(user.id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unread_channels(State(state):State<AppState>,headers:HeaderMap)->ApiResult<Json<Vec<ChannelUnread>>>{
    let user=current_user(&state,&headers).await?;
    let rows=sqlx::query("SELECT c.id AS channel_id,COUNT(msg.id)::bigint AS count FROM memberships member JOIN channels c ON c.server_id=member.server_id LEFT JOIN channel_reads r ON r.user_id=member.user_id AND r.channel_id=c.id LEFT JOIN messages msg ON msg.channel_id=c.id AND msg.author_id!=$1 AND msg.created_at>COALESCE(r.last_read_at,member.joined_at) WHERE member.user_id=$1 GROUP BY c.id")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(|r|ChannelUnread{channel_id:r.get("channel_id"),count:r.get("count")}).collect()))
}

pub async fn mark_channel_read(State(state):State<AppState>,headers:HeaderMap,Path(channel):Path<Uuid>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let server=channel_server(&state,channel).await?;
    membership(&state,server,user.id).await?;
    sqlx::query("INSERT INTO channel_reads(user_id,channel_id,last_read_at) VALUES($1,$2,NOW()) ON CONFLICT(user_id,channel_id) DO UPDATE SET last_read_at=GREATEST(channel_reads.last_read_at,EXCLUDED.last_read_at)")
        .bind(user.id).bind(channel).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
