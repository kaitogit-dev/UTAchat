use axum::{extract::{Path,State},http::{HeaderMap,StatusCode},Json};
use uuid::Uuid;
use utachat_shared::{ChatMessage,EditMessageRequest,GatewayMessage,ReactionRequest};
use crate::{AppState,Broadcast,auth::current_user,data::{channel_server,membership,message_from},error::{ApiResult,AppError}};

pub async fn edit(State(state):State<AppState>,headers:HeaderMap,Path((channel,message)):Path<(Uuid,Uuid)>,Json(input):Json<EditMessageRequest>)->ApiResult<Json<ChatMessage>>{
    let user=current_user(&state,&headers).await?;
    let server=channel_server(&state,channel).await?;
    membership(&state,server,user.id).await?;
    let text=input.content.trim();
    if text.is_empty()||text.chars().count()>4000{return Err(AppError::bad_request("Message must contain 1-4000 characters"));}
    let modified=sqlx::query("UPDATE messages SET content=$1,edited_at=NOW() WHERE id=$2 AND channel_id=$3 AND author_id=$4")
        .bind(text).bind(message).bind(channel).bind(user.id).execute(&state.db).await?;
    if modified.rows_affected()==0{return Err(AppError::not_found());}
    let row=sqlx::query("SELECT m.id,m.channel_id,m.author_id,u.username,m.content,m.created_at,m.edited_at,m.reply_to,COALESCE((SELECT jsonb_agg(jsonb_build_object('kind',reactions.kind,'count',reactions.total)) FROM (SELECT kind,COUNT(*) AS total FROM message_reactions WHERE message_id=m.id GROUP BY kind) reactions),'[]'::jsonb) AS reactions FROM messages m JOIN users u ON u.id=m.author_id WHERE m.id=$1 AND m.channel_id=$2")
        .bind(message).bind(channel).fetch_one(&state.db).await?;
    let updated=message_from(row);
    let _=state.events.send(Broadcast::server(server,GatewayMessage::MessageUpdated{message:updated.clone()}));
    Ok(Json(updated))
}

pub async fn delete(State(state):State<AppState>,headers:HeaderMap,Path((channel,message)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let server=channel_server(&state,channel).await?;
    let role=membership(&state,server,user.id).await?;
    let author:Option<Uuid>=sqlx::query_scalar("SELECT author_id FROM messages WHERE id=$1 AND channel_id=$2")
        .bind(message).bind(channel).fetch_optional(&state.db).await?;
    let Some(author)=author else{return Err(AppError::not_found());};
    if author!=user.id&&role!="owner"{return Err(AppError::forbidden());}
    sqlx::query("DELETE FROM messages WHERE id=$1 AND channel_id=$2")
        .bind(message).bind(channel).execute(&state.db).await?;
    let _=state.events.send(Broadcast::server(server,GatewayMessage::MessageDeleted{channel_id:channel,message_id:message}));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn react(State(state):State<AppState>,headers:HeaderMap,Path((channel,message)):Path<(Uuid,Uuid)>,Json(input):Json<ReactionRequest>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    let server=channel_server(&state,channel).await?;
    membership(&state,server,user.id).await?;
    if !matches!(input.kind.as_str(),"like"|"agree"|"laugh"|"heart"|"surprised"){
        return Err(AppError::bad_request("Unknown reaction"));
    }
    let exists:Option<Uuid>=sqlx::query_scalar("SELECT id FROM messages WHERE id=$1 AND channel_id=$2")
        .bind(message).bind(channel).fetch_optional(&state.db).await?;
    if exists.is_none(){return Err(AppError::not_found());}
    let inserted=sqlx::query("INSERT INTO message_reactions(message_id,user_id,kind) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(message).bind(user.id).bind(&input.kind).execute(&state.db).await?;
    if inserted.rows_affected()==0{
        sqlx::query("DELETE FROM message_reactions WHERE message_id=$1 AND user_id=$2 AND kind=$3")
            .bind(message).bind(user.id).bind(&input.kind).execute(&state.db).await?;
    }
    let _=state.events.send(Broadcast::server(server,GatewayMessage::ReactionsChanged{channel_id:channel}));
    Ok(StatusCode::NO_CONTENT)
}
