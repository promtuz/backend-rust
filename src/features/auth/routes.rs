use argon2::{Argon2, PasswordVerifier};
use axum::{
    body::Bytes,
    http::header::SET_COOKIE,
    response::{AppendHeaders, IntoResponse},
    routing::post,
    Json, Router,
};
use chrono::Duration;
use jsonwebtoken::{EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::query_as;


use crate::DB_POOL;

use crate::utils::decompress::decode_zlib_json;


#[derive(Deserialize, Serialize)]
struct Claims {
    uid: String,
    username: String,
    iat: usize,
    exp: usize,
}

#[derive(Deserialize, Serialize)]
struct LoginPayload {
    username: String,
    password: String,
    cookie: bool,
}

#[derive(sqlx::FromRow, Serialize, Deserialize)]
struct User {
    id: String,
    username: String,
    password_hash: String,
}

async fn login(body: Bytes) -> impl IntoResponse {
    let payload: LoginPayload = match decode_zlib_json(body) {
        Ok(p) => p,
        Err(_) => return Json(json!({ "ok": 0, "error": "Invalid Credentials" })).into_response(),
    };

    let pool = DB_POOL.get().unwrap();

    let user_res = query_as::<_, User>(
        "SELECT id, username, password_hash 
          FROM users 
          LEFT JOIN auth_credentials ON users.id = auth_credentials.user_id 
          WHERE username = $1",
    )
    .bind(&payload.username)
    .fetch_one(pool)
    .await;

    if user_res.is_err() {
        return Json(json!({ "ok": 0, "error": "Invalid Credentials", })).into_response();
    }

    let user = user_res.unwrap();
    let hash = argon2::PasswordHash::new(&user.password_hash).unwrap();

    if !Argon2::default()
        .verify_password(&payload.password.as_bytes(), &hash)
        .is_ok()
    {
        return Json(json!({ "ok": 0, "error": "Invalid Credentials", })).into_response();
    }

    let jwt_secret = std::env::var("JWTSECRET").unwrap_or_default();

    let now = chrono::Utc::now();
    let token = jsonwebtoken::encode(
        &Header::default(),
        &Claims {
            uid: user.id,
            username: user.username,
            exp: (now + Duration::hours(1)).timestamp() as usize,
            iat: now.timestamp() as usize,
        },
        &EncodingKey::from_secret(jwt_secret.as_ref()),
    )
    .unwrap();

    if payload.cookie {
        let cookie = format!(
            "token={}; Max-Age=2592000; HttpOnly; Path=/; Domain={}",
            token, "localhost"
        );

        let headers = AppendHeaders([(SET_COOKIE, cookie)]);
        let content = Json(json!({ "token": token }));

        return (headers, content).into_response();
    }

    Json(json!({ "token": token })).into_response()
}

pub fn routes() -> Router {
    Router::new().route("/login", post(login))
}
