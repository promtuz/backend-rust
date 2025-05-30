use std::net::IpAddr;

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::{
    utils::{get_cookie, log_error},
    DB_POOL,
};

#[derive(sqlx::FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub user_agent: String,
    pub ip_address: IpAddr,
    pub last_active_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow, Serialize, Deserialize)]
pub struct PushToken {
    pub id: String,
    pub token: String,
    pub user_id: String,
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct AuthUser {
    pub me: String,
    pub user: User, 
    pub session: Option<Session>,
    pub push_token: Option<PushToken>,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    uid: String,
    sid: String,
    username: String,
    iat: usize,
    exp: usize,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = get_cookie(&parts.headers, "token") {
            let jwt_secret = std::env::var("JWTSECRET").unwrap_or_default();

            let jwt_res = jsonwebtoken::decode::<Claims>(
                &token,
                &DecodingKey::from_secret(jwt_secret.as_ref()),
                &Validation::default(),
            );
            if jwt_res.is_err() {
                return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
            }

            let jwt = jwt_res.unwrap();

            // let id = jwt.claims.uid;
            let pool = DB_POOL.get().unwrap();

            let user_res = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
                .bind(&jwt.claims.uid)
                .fetch_one(pool)
                .await;

            if user_res.is_err() {
                log_error("Authenticator", user_res.as_ref().err());
                return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            let user = user_res.unwrap();

            let session_res = sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = $1")
                .bind(&jwt.claims.sid)
                .fetch_optional(pool)
                .await;
            if session_res.is_err() {
                log_error("Authenticator", session_res.as_ref().err());
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"));
            }

            let session = session_res.unwrap();

            let push_token_res =
                sqlx::query_as::<_, PushToken>("SELECT * FROM push_tokens WHERE id = $1")
                    .bind(&jwt.claims.sid)
                    .fetch_optional(pool)
                    .await;

            if push_token_res.is_err() {
                log_error("Authenticator", push_token_res.as_ref().err());
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"));
            }

            let push_token = push_token_res.unwrap();

            Ok(AuthUser {
                me: user.id.clone(),
                user,
                session,
                push_token,
            })
        } else {
            Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
        }
    }
}
