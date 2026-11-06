use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::{SaltString, rand_core::OsRng}};
use axum::{extract::State, http::{HeaderMap, StatusCode}, Json};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;
use utachat_shared::{AuthResponse, CredentialRequest, PublicUser};
use crate::{AppState, error::{ApiResult, AppError}};

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub async fn session_user(state: &AppState, token: &str) -> ApiResult<PublicUser> {
    let row = sqlx::query(
        "SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires_at > ?"
    )
    .bind(token_hash(token))
    .bind(Utc::now())
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(AppError::unauthorized)?;
    Ok(PublicUser { id: row.get("id"), username: row.get("username") })
}

pub async fn current_user(state: &AppState, headers: &HeaderMap) -> ApiResult<PublicUser> {
    let authorization = headers.get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(AppError::unauthorized)?;
    session_user(state, authorization).await
}

async fn new_session(state: &AppState, user: PublicUser) -> ApiResult<AuthResponse> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO sessions (id, user_id, token_hash, expires_at) VALUES (?, ?, ?, ?)")
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(token_hash(&token))
        .bind(Utc::now() + Duration::days(30))
        .execute(&state.db)
        .await?;
    Ok(AuthResponse { token, user })
}

pub async fn register(State(state): State<AppState>, Json(input): Json<CredentialRequest>) -> ApiResult<(StatusCode, Json<AuthResponse>)> {
    let username = input.username.trim().to_lowercase();
    if !(3..=24).contains(&username.len()) || !username.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '.' || ch == '-') {
        return Err(AppError::bad_request("Username must be 3-24 characters: letters, numbers, underscore, period or hyphen"));
    }
    if !(8..=128).contains(&input.password.len()) {
        return Err(AppError::bad_request("Password must be 8-128 characters"));
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(input.password.as_bytes(), &salt)
        .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "Password hashing failed"))?
        .to_string();
    let id = Uuid::new_v4();
    let result = sqlx::query("INSERT INTO users (id, username, password_hash) VALUES (?, ?, ?)")
        .bind(id).bind(&username).bind(hash).execute(&state.db).await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => return Err(AppError::conflict("Username is already taken")),
        Err(error) => return Err(error.into()),
    }
    let user = PublicUser { id, username };
    Ok((StatusCode::CREATED, Json(new_session(&state, user).await?)))
}

pub async fn login(State(state): State<AppState>, Json(input): Json<CredentialRequest>) -> ApiResult<Json<AuthResponse>> {
    let row = sqlx::query("SELECT id, username, password_hash FROM users WHERE LOWER(username) = LOWER(?)")
        .bind(input.username.trim()).fetch_optional(&state.db).await?;
    let row = row.ok_or_else(AppError::unauthorized)?;
    let hash: String = row.get("password_hash");
    let password_hash = PasswordHash::new(&hash).map_err(|_| AppError::unauthorized())?;
    Argon2::default().verify_password(input.password.as_bytes(), &password_hash).map_err(|_| AppError::unauthorized())?;
    let user = PublicUser { id: row.get("id"), username: row.get("username") };
    Ok(Json(new_session(&state, user).await?))
}

pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<PublicUser>> {
    Ok(Json(current_user(&state, &headers).await?))
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ")).ok_or_else(AppError::unauthorized)?;
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash(token)).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
