use axum::{extract::{Path,State},http::{HeaderMap,StatusCode},Json};
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{BanEntry,BanRequest,Channel,GatewayMessage,NameRequest,PublicUser,RoleRequest,Server};
use crate::{AppState,Broadcast,auth::current_user,data::membership,error::{ApiResult,AppError}};

fn can_manage(role:&str)->bool{
    matches!(role,"owner"|"moderator")
}

pub async fn rename_server(State(state):State<AppState>,headers:HeaderMap,Path(server):Path<Uuid>,Json(input):Json<NameRequest>)->ApiResult<Json<Server>>{
    let user=current_user(&state,&headers).await?;
    if membership(&state,server,user.id).await?!="owner"{return Err(AppError::forbidden());}
    let name=input.name.trim();
    if name.is_empty()||name.chars().count()>64{return Err(AppError::bad_request("Server name must contain 1-64 characters"));}
    let row=sqlx::query("UPDATE servers SET name=$1 WHERE id=$2 RETURNING id,name,owner_id")
        .bind(name).bind(server).fetch_one(&state.db).await?;
    Ok(Json(Server{id:row.get("id"),name:row.get("name"),owner_id:row.get("owner_id")}))
}

pub async fn rename_channel(State(state):State<AppState>,headers:HeaderMap,Path((server,channel)):Path<(Uuid,Uuid)>,Json(input):Json<NameRequest>)->ApiResult<Json<Channel>>{
    let user=current_user(&state,&headers).await?;
    if !can_manage(&membership(&state,server,user.id).await?){return Err(AppError::forbidden());}
    let name=input.name.trim().to_lowercase().replace(' ',"-");
    if name.is_empty()||name.len()>48||!name.chars().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||matches!(c,'_'|'-')){
        return Err(AppError::bad_request("Invalid channel name"));
    }
    let updated=sqlx::query("UPDATE channels SET name=$1 WHERE id=$2 AND server_id=$3 RETURNING id,server_id,name,position")
        .bind(name).bind(channel).bind(server).fetch_optional(&state.db).await?;
    let row=updated.ok_or_else(AppError::not_found)?;
    let item=Channel{id:row.get("id"),server_id:row.get("server_id"),name:row.get("name"),position:row.get("position")};
    let _=state.events.send(Broadcast::server(server,GatewayMessage::ChannelCreated{channel:item.clone()}));
    Ok(Json(item))
}

pub async fn delete_channel(State(state):State<AppState>,headers:HeaderMap,Path((server,channel)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    if !can_manage(&membership(&state,server,user.id).await?){return Err(AppError::forbidden());}
    let mut tx=state.db.begin().await?;
    sqlx::query("SELECT id FROM servers WHERE id=$1 FOR UPDATE").bind(server).fetch_optional(&mut *tx).await?
        .ok_or_else(AppError::not_found)?;
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE server_id=$1")
        .bind(server).fetch_one(&mut *tx).await?;
    if count<=1{return Err(AppError::bad_request("Server must keep at least one channel"));}
    let deleted=sqlx::query("DELETE FROM channels WHERE id=$1 AND server_id=$2")
        .bind(channel).bind(server).execute(&mut *tx).await?;
    if deleted.rows_affected()==0{return Err(AppError::not_found());}
    tx.commit().await?;
    let _=state.events.send(Broadcast::server(server,GatewayMessage::ChannelDeleted{server_id:server,channel_id:channel}));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_role(State(state):State<AppState>,headers:HeaderMap,Path((server,peer)):Path<(Uuid,Uuid)>,Json(input):Json<RoleRequest>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    if membership(&state,server,user.id).await?!="owner"{return Err(AppError::forbidden());}
    if !matches!(input.role.as_str(),"member"|"moderator"){return Err(AppError::bad_request("Unknown role"));}
    let result=sqlx::query("UPDATE memberships SET role=$1 WHERE server_id=$2 AND user_id=$3 AND role!='owner'")
        .bind(&input.role).bind(server).bind(peer).execute(&state.db).await?;
    if result.rows_affected()==0{return Err(AppError::not_found());}
    let _=state.events.send(Broadcast::server(server,GatewayMessage::MemberRoleChanged{
        server_id:server,user_id:peer,role:input.role
    }));
    Ok(StatusCode::NO_CONTENT)
}

async fn kick_authorized(state:&AppState,server:Uuid,actor:Uuid,peer:Uuid)->ApiResult<()>{
    let actor_role=membership(state,server,actor).await?;
    if !can_manage(&actor_role){return Err(AppError::forbidden());}
    let target=membership(state,server,peer).await?;
    if target=="owner"||actor_role=="moderator"&&target!="member"{return Err(AppError::forbidden());}
    Ok(())
}

pub async fn kick(State(state):State<AppState>,headers:HeaderMap,Path((server,peer)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    kick_authorized(&state,server,user.id,peer).await?;
    sqlx::query("DELETE FROM memberships WHERE server_id=$1 AND user_id=$2")
        .bind(server).bind(peer).execute(&state.db).await?;
    let event=GatewayMessage::MemberRemoved{server_id:server,user_id:peer};
    let _=state.events.send(Broadcast::server(server,event.clone()));
    let _=state.events.send(Broadcast::users(vec![peer],event));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn ban(State(state):State<AppState>,headers:HeaderMap,Path((server,peer)):Path<(Uuid,Uuid)>,Json(input):Json<BanRequest>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    if input.reason.chars().count()>500{return Err(AppError::bad_request("Ban reason is too long"));}
    let actor_role=membership(&state,server,user.id).await?;
    if !can_manage(&actor_role){return Err(AppError::forbidden());}
    if peer==user.id{return Err(AppError::bad_request("Cannot ban yourself"));}
    let target=sqlx::query_scalar::<_,String>("SELECT role FROM memberships WHERE server_id=$1 AND user_id=$2")
        .bind(server).bind(peer).fetch_optional(&state.db).await?;
    if let Some(ref role)=target{
        if role=="owner"||(actor_role=="moderator"&&role!="member"){return Err(AppError::forbidden());}
    }
    let exists:Option<Uuid>=sqlx::query_scalar("SELECT id FROM users WHERE id=$1")
        .bind(peer).fetch_optional(&state.db).await?;
    if exists.is_none(){return Err(AppError::not_found());}
    let mut tx=state.db.begin().await?;
    sqlx::query("INSERT INTO server_bans(server_id,user_id,moderator_id,reason) VALUES($1,$2,$3,$4) ON CONFLICT(server_id,user_id) DO UPDATE SET reason=EXCLUDED.reason,moderator_id=EXCLUDED.moderator_id")
        .bind(server).bind(peer).bind(user.id).bind(input.reason.trim()).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM memberships WHERE server_id=$1 AND user_id=$2")
        .bind(server).bind(peer).execute(&mut *tx).await?;
    tx.commit().await?;
    let event=GatewayMessage::MemberRemoved{server_id:server,user_id:peer};
    let _=state.events.send(Broadcast::server(server,event.clone()));
    let _=state.events.send(Broadcast::users(vec![peer],event));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unban(State(state):State<AppState>,headers:HeaderMap,Path((server,peer)):Path<(Uuid,Uuid)>)->ApiResult<StatusCode>{
    let user=current_user(&state,&headers).await?;
    if !can_manage(&membership(&state,server,user.id).await?){return Err(AppError::forbidden());}
    sqlx::query("DELETE FROM server_bans WHERE server_id=$1 AND user_id=$2")
        .bind(server).bind(peer).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_bans(State(state):State<AppState>,headers:HeaderMap,Path(server):Path<Uuid>)->ApiResult<Json<Vec<BanEntry>>>{
    let user=current_user(&state,&headers).await?;
    if !can_manage(&membership(&state,server,user.id).await?){return Err(AppError::forbidden());}
    let rows=sqlx::query("SELECT u.id,u.username,b.reason,b.created_at FROM server_bans b JOIN users u ON u.id=b.user_id WHERE b.server_id=$1 ORDER BY b.created_at DESC")
        .bind(server).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().map(|r|BanEntry{
        user:PublicUser{id:r.get("id"),username:r.get("username")},
        reason:r.get("reason"),created_at:r.get("created_at")
    }).collect()))
}
