use axum::{extract::{Path,Query,State},http::{HeaderMap,StatusCode},Json};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{ChatMessage,GatewayMessage,SearchHit};
use crate::{AppState,Broadcast,auth::current_user,data::{channel_server,membership,message_from},error::{ApiResult,AppError}};

#[derive(Deserialize)]
pub struct SearchQuery {
    q:String,
}

fn message_query()->&'static str{
    "SELECT m.id,m.channel_id,m.author_id,u.username,m.content,m.created_at,m.edited_at,m.reply_to,m.pinned_at,COALESCE((SELECT jsonb_agg(jsonb_build_object('kind',grouped.kind,'count',grouped.total)) FROM (SELECT kind,COUNT(*) AS total FROM message_reactions WHERE message_id=m.id GROUP BY kind) grouped),'[]'::jsonb) AS reactions FROM messages m JOIN users u ON u.id=m.author_id"
}

pub async fn search(State(state):State<AppState>,headers:HeaderMap,Path(server):Path<Uuid>,Query(query):Query<SearchQuery>)->ApiResult<Json<Vec<SearchHit>>>{
    let user=current_user(&state,&headers).await?;
    membership(&state,server,user.id).await?;
    let q=query.q.trim();
    if q.chars().count()<2||q.chars().count()>120 {
        return Err(AppError::bad_request("Search text must contain 2-120 characters"));
    }
    let select=message_query().replace(" FROM messages m"," ,c.name AS channel_name FROM messages m");
    let sql=format!("{} JOIN channels c ON c.id=m.channel_id WHERE c.server_id=$1 AND to_tsvector('simple',m.content) @@ plainto_tsquery('simple',$2) ORDER BY m.created_at DESC,m.id DESC LIMIT 60",select);
    let rows=sqlx::query(&sql).bind(server).bind(q).fetch_all(&state.db).await?;
    let mut hits=Vec::with_capacity(rows.len());
    for row in rows {
        let name:String=row.get("channel_name");
        hits.push(SearchHit{message:message_from(row),channel_name:name});
    }
    Ok(Json(hits))
}

pub async fn pinned(State(state):State<AppState>,headers:HeaderMap,Path(channel):Path<Uuid>)->ApiResult<Json<Vec<ChatMessage>>>{
    let user=current_user(&state,&headers).await?;
    let server=channel_server(&state,channel).await?;
    membership(&state,server,user.id).await?;
    let sql=format!("{} WHERE m.channel_id=$1 AND m.pinned_at IS NOT NULL ORDER BY m.pinned_at DESC LIMIT 100",message_query());
    let rows=sqlx::query(&sql).bind(channel).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(message_from).collect()))
}

async fn set_pin(state:&AppState,channel:Uuid,message:Uuid,user:Uuid,pin:bool)->ApiResult<()>{
    let server=channel_server(state,channel).await?;
    let role=membership(state,server,user).await?;
    if role!="owner"&&role!="moderator"{return Err(AppError::forbidden());}
    let sql=if pin{
        "UPDATE messages SET pinned_at=NOW(),pinned_by=$3 WHERE id=$1 AND channel_id=$2 AND pinned_at IS NULL"
    }else{
        "UPDATE messages SET pinned_at=NULL,pinned_by=NULL WHERE id=$1 AND channel_id=$2 AND pinned_at IS NOT NULL"
    };
    let outcome=if pin{
        sqlx::query(sql).bind(message).bind(channel).bind(user).execute(&state.db).await?
    }else{
        sqlx::query(sql).bind(message).bind(channel).execute(&state.db).await?
    };
    if outcome.rows_affected()==0{
        let found:Option<Uuid>=sqlx::query_scalar("SELECT id FROM messages WHERE id=$1 AND channel_id=$2")
            .bind(message).bind(channel).fetch_optional(&state.db).await?;
        if found.is_none(){return Err(AppError::not_found());}
    }
    let _=state.events.send(Broadcast::server(server,GatewayMessage::PinsChanged{channel_id:channel}));
    Ok(())
}

pub async fn pin(State(state):State<AppState>,headers:HeaderMap,Path((channel,message)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    set_pin(&state,channel,message,user.id,true).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unpin(State(state):State<AppState>,headers:HeaderMap,Path((channel,message)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    set_pin(&state,channel,message,user.id,false).await?;
    Ok(StatusCode::NO_CONTENT)
}
