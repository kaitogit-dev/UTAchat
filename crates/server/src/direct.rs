use axum::{extract::{Path,Query,State},http::{HeaderMap,StatusCode},Json};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{ChatMessage,DirectRequest,DirectThread,EditMessageRequest,Friend,GatewayMessage,MessageRequest,PublicUser};
use crate::{AppState,Broadcast,auth::current_user,error::{ApiResult,AppError}};

#[derive(Deserialize)]
pub struct Page { before: Option<Uuid>, limit: Option<i64> }

fn ordered(a:Uuid,b:Uuid)->(Uuid,Uuid){if a<b{(a,b)}else{(b,a)}}

async fn user_named(state:&AppState,username:&str)->ApiResult<PublicUser>{
    let row=sqlx::query("SELECT id,username FROM users WHERE LOWER(username)=LOWER($1)")
        .bind(username.trim()).fetch_optional(&state.db).await?.ok_or_else(AppError::not_found)?;
    Ok(PublicUser{id:row.get("id"),username:row.get("username")})
}

async fn thread_peer(state:&AppState,thread:Uuid,user:Uuid)->ApiResult<PublicUser>{
    let row=sqlx::query("SELECT u.id,u.username FROM direct_threads t JOIN users u ON u.id=CASE WHEN t.user_a=$2 THEN t.user_b ELSE t.user_a END WHERE t.id=$1 AND (t.user_a=$2 OR t.user_b=$2)")
        .bind(thread).bind(user).fetch_optional(&state.db).await?.ok_or_else(AppError::not_found)?;
    Ok(PublicUser{id:row.get("id"),username:row.get("username")})
}

fn message_from(row:sqlx::postgres::PgRow,thread:Uuid)->ChatMessage{
    ChatMessage{id:row.get("id"),channel_id:thread,
        author:PublicUser{id:row.get("author_id"),username:row.get("username")},
        content:row.get("content"),created_at:row.get("created_at"),
        edited_at:row.get("edited_at"),reply_to:row.get("reply_to"),reactions:Vec::new(),pinned_at:None}
}

pub async fn friends(State(state):State<AppState>,headers:HeaderMap)->ApiResult<Json<Vec<Friend>>>{
    let user=current_user(&state,&headers).await?;
    let rows=sqlx::query("SELECT u.id,u.username,f.status,f.requested_by FROM friendships f JOIN users u ON u.id=CASE WHEN f.user_a=$1 THEN f.user_b ELSE f.user_a END WHERE f.user_a=$1 OR f.user_b=$1 ORDER BY CASE WHEN f.status='pending' THEN 0 ELSE 1 END,LOWER(u.username)")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(|row|Friend{
        user:PublicUser{id:row.get("id"),username:row.get("username")},
        status:row.get("status"),incoming:row.get::<Uuid,_>("requested_by")!=user.id
    }).collect()))
}

pub async fn request_friend(State(state):State<AppState>,headers:HeaderMap,Json(input):Json<DirectRequest>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let peer=user_named(&state,&input.username).await?;
    if peer.id==user.id{return Err(AppError::bad_request("Cannot add yourself"));}
    let(a,b)=ordered(user.id,peer.id);
    let existing=sqlx::query("SELECT status,requested_by FROM friendships WHERE user_a=$1 AND user_b=$2")
        .bind(a).bind(b).fetch_optional(&state.db).await?;
    match existing{
        Some(row)=>{
            if row.get::<String,_>("status")=="accepted"{return Err(AppError::conflict("Already friends"));}
            if row.get::<Uuid,_>("requested_by")!=user.id{
                sqlx::query("UPDATE friendships SET status='accepted' WHERE user_a=$1 AND user_b=$2")
                    .bind(a).bind(b).execute(&state.db).await?;
            }
        }
        None=>{
            sqlx::query("INSERT INTO friendships(user_a,user_b,requested_by,status) VALUES($1,$2,$3,'pending')")
                .bind(a).bind(b).bind(user.id).execute(&state.db).await?;
        }
    }
    if let Err(error)=crate::profile::save_notification(&state,peer.id,&user,"friend_request",None,None,None).await {
        tracing::warn!(error=%error.message,"Friend notification could not be persisted");
    }
    let _=state.events.send(Broadcast::users(vec![user.id,peer.id],GatewayMessage::FriendsChanged));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn accept_friend(State(state):State<AppState>,headers:HeaderMap,Path(peer):Path<Uuid>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let(a,b)=ordered(user.id,peer);
    let result=sqlx::query("UPDATE friendships SET status='accepted' WHERE user_a=$1 AND user_b=$2 AND status='pending' AND requested_by!=$3")
        .bind(a).bind(b).bind(user.id).execute(&state.db).await?;
    if result.rows_affected()==0{return Err(AppError::not_found());}
    let _=state.events.send(Broadcast::users(vec![user.id,peer],GatewayMessage::FriendsChanged));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_friend(State(state):State<AppState>,headers:HeaderMap,Path(peer):Path<Uuid>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let(a,b)=ordered(user.id,peer);
    sqlx::query("DELETE FROM friendships WHERE user_a=$1 AND user_b=$2")
        .bind(a).bind(b).execute(&state.db).await?;
    let _=state.events.send(Broadcast::users(vec![user.id,peer],GatewayMessage::FriendsChanged));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_threads(State(state):State<AppState>,headers:HeaderMap)->ApiResult<Json<Vec<DirectThread>>>{
    let user=current_user(&state,&headers).await?;
    let rows=sqlx::query("SELECT t.id,t.created_at,u.id AS peer_id,u.username FROM direct_threads t JOIN users u ON u.id=CASE WHEN t.user_a=$1 THEN t.user_b ELSE t.user_a END WHERE t.user_a=$1 OR t.user_b=$1 ORDER BY COALESCE((SELECT MAX(created_at) FROM direct_messages WHERE thread_id=t.id),t.created_at) DESC")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(|row|DirectThread{
        id:row.get("id"),peer:PublicUser{id:row.get("peer_id"),username:row.get("username")},
        created_at:row.get("created_at")
    }).collect()))
}

pub async fn create_thread(State(state):State<AppState>,headers:HeaderMap,Json(input):Json<DirectRequest>)->ApiResult<Json<DirectThread>>{
    let user=current_user(&state,&headers).await?;
    let peer=user_named(&state,&input.username).await?;
    if user.id==peer.id{return Err(AppError::bad_request("Cannot message yourself"));}
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM friendships WHERE ((user_a=$1 AND user_b=$2) OR (user_a=$2 AND user_b=$1)) AND status='accepted') OR EXISTS(SELECT 1 FROM memberships a JOIN memberships b ON a.server_id=b.server_id WHERE a.user_id=$1 AND b.user_id=$2)")
        .bind(user.id).bind(peer.id).fetch_one(&state.db).await?;
    if !allowed{return Err(AppError::forbidden());}
    let(a,b)=ordered(user.id,peer.id);
    let row=sqlx::query("INSERT INTO direct_threads(id,user_a,user_b) VALUES($1,$2,$3) ON CONFLICT(user_a,user_b) DO UPDATE SET user_a=EXCLUDED.user_a RETURNING id,created_at")
        .bind(Uuid::new_v4()).bind(a).bind(b).fetch_one(&state.db).await?;
    let thread=DirectThread{id:row.get("id"),peer:peer.clone(),created_at:row.get("created_at")};
    let reverse=DirectThread{id:thread.id,peer:user.clone(),created_at:thread.created_at};
    let _=state.events.send(Broadcast::users(vec![user.id],GatewayMessage::DirectThreadCreated{thread:thread.clone()}));
    let _=state.events.send(Broadcast::users(vec![peer.id],GatewayMessage::DirectThreadCreated{thread:reverse}));
    Ok(Json(thread))
}

pub async fn list_messages(State(state):State<AppState>,headers:HeaderMap,Path(thread):Path<Uuid>,Query(page):Query<Page>)->ApiResult<Json<Vec<ChatMessage>>>{
    let user=current_user(&state,&headers).await?;
    thread_peer(&state,thread,user.id).await?;
    let limit=page.limit.unwrap_or(100).clamp(1,100);
    let rows=if let Some(before)=page.before{
        sqlx::query("SELECT m.id,m.author_id,u.username,m.content,m.edited_at,m.reply_to,m.created_at FROM direct_messages m JOIN users u ON u.id=m.author_id WHERE m.thread_id=$1 AND (m.created_at,m.id)<(SELECT created_at,id FROM direct_messages WHERE id=$2 AND thread_id=$1) ORDER BY m.created_at DESC,m.id DESC LIMIT $3")
            .bind(thread).bind(before).bind(limit).fetch_all(&state.db).await?
    }else{
        sqlx::query("SELECT m.id,m.author_id,u.username,m.content,m.edited_at,m.reply_to,m.created_at FROM direct_messages m JOIN users u ON u.id=m.author_id WHERE m.thread_id=$1 ORDER BY m.created_at DESC,m.id DESC LIMIT $2")
            .bind(thread).bind(limit).fetch_all(&state.db).await?
    };
    let mut result:Vec<_>=rows.into_iter().map(|r|message_from(r,thread)).collect();
    result.reverse();
    Ok(Json(result))
}

pub async fn send_message(State(state):State<AppState>,headers:HeaderMap,Path(thread):Path<Uuid>,Json(input):Json<MessageRequest>)->ApiResult<(StatusCode,Json<ChatMessage>)>{
    let user=current_user(&state,&headers).await?;
    let peer=thread_peer(&state,thread,user.id).await?;
    let text=input.content.trim();
    if text.is_empty()||text.chars().count()>4000{return Err(AppError::bad_request("Message must contain 1-4000 characters"));}
    if let Some(reply)=input.reply_to{
        let found:Option<Uuid>=sqlx::query_scalar("SELECT id FROM direct_messages WHERE id=$1 AND thread_id=$2")
            .bind(reply).bind(thread).fetch_optional(&state.db).await?;
        if found.is_none(){return Err(AppError::bad_request("Reply target not found"));}
    }
    let id=Uuid::new_v4();
    let row=sqlx::query("INSERT INTO direct_messages(id,thread_id,author_id,content,reply_to) VALUES($1,$2,$3,$4,$5) RETURNING created_at")
        .bind(id).bind(thread).bind(user.id).bind(text).bind(input.reply_to).fetch_one(&state.db).await?;
    let message=ChatMessage{id,channel_id:thread,author:user.clone(),content:text.to_string(),
        created_at:row.get("created_at"),edited_at:None,reply_to:input.reply_to,reactions:Vec::new(),pinned_at:None};
    if let Err(error)=crate::profile::save_notification(&state,peer.id,&user,"direct_message",None,Some(thread),Some(id)).await {
        tracing::warn!(message_id=%id,error=%error.message,"Direct message notification could not be persisted");
    }
    let _=state.events.send(Broadcast::users(vec![user.id,peer.id],GatewayMessage::DirectMessageCreated{message:message.clone()}));
    Ok((StatusCode::CREATED,Json(message)))
}

pub async fn edit_message(State(state):State<AppState>,headers:HeaderMap,Path((thread,message)):Path<(Uuid,Uuid)>,Json(input):Json<EditMessageRequest>)->ApiResult<Json<ChatMessage>>{
    let user=current_user(&state,&headers).await?;
    let peer=thread_peer(&state,thread,user.id).await?;
    let text=input.content.trim();
    if text.is_empty()||text.chars().count()>4000{return Err(AppError::bad_request("Message must contain 1-4000 characters"));}
    let changed=sqlx::query("UPDATE direct_messages SET content=$1,edited_at=NOW() WHERE id=$2 AND thread_id=$3 AND author_id=$4")
        .bind(text).bind(message).bind(thread).bind(user.id).execute(&state.db).await?;
    if changed.rows_affected()==0{return Err(AppError::not_found());}
    let row=sqlx::query("SELECT m.id,m.author_id,u.username,m.content,m.edited_at,m.reply_to,m.created_at FROM direct_messages m JOIN users u ON u.id=m.author_id WHERE m.id=$1")
        .bind(message).fetch_one(&state.db).await?;
    let result=message_from(row,thread);
    let _=state.events.send(Broadcast::users(vec![user.id,peer.id],GatewayMessage::DirectMessageUpdated{message:result.clone()}));
    Ok(Json(result))
}

pub async fn delete_message(State(state):State<AppState>,headers:HeaderMap,Path((thread,message)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let peer=thread_peer(&state,thread,user.id).await?;
    let removed=sqlx::query("DELETE FROM direct_messages WHERE id=$1 AND thread_id=$2 AND author_id=$3")
        .bind(message).bind(thread).bind(user.id).execute(&state.db).await?;
    if removed.rows_affected()==0{return Err(AppError::not_found());}
    let _=state.events.send(Broadcast::users(vec![user.id,peer.id],GatewayMessage::DirectMessageDeleted{channel_id:thread,message_id:message}));
    Ok(StatusCode::NO_CONTENT)
}
