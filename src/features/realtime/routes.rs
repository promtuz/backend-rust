// use  axum

use std::{io::Write, sync::Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        WebSocketUpgrade,
    },
    response::Response,
    routing::any,
    Json, Router,
};

use cuid::cuid1;
use flate2::{
    write::{DeflateDecoder, DeflateEncoder},
    Compression,
};
use futures::{future::join_all, StreamExt};
use redis::{AsyncCommands, ToRedisArgs};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    database::sql::get_initial_user::get_initial_user, middlewares::auth::AuthUser,
    utils::log_error, DB_POOL, RD_POOL,
};

use super::events::RealTimeEvents;

pub fn routes() -> Router {
    Router::new().route("/", any(handler))
}

async fn handler(ws: WebSocketUpgrade, auth_user: AuthUser) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, auth_user))
}

fn compress(data: String) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());

    encoder.write_all(&data.as_bytes()).unwrap();
    let ret_vec = encoder.finish().unwrap();

    ret_vec
}
fn compress_msg(data: String) -> Message {
    Message::binary(compress(data))
}

#[derive(Deserialize)]
struct InEvent {
    #[serde(rename = "type")]
    kind: String,
    data: Value,
}

fn decompress(msg: Message) -> Result<InEvent, String> {
    let d_bytes = msg.into_data();
    let mut decoder = DeflateDecoder::new(Vec::new());
    decoder
        .write_all(&d_bytes)
        .map_err(|_| "Decompression failed")?;
    let json_b = decoder.finish().map_err(|_| "Finish failed")?;
    serde_json::from_slice(&json_b).map_err(|_| "Invalid Payload".into())
}

fn payload(kind: &str, data: Option<&Value>) -> String {
    Json(json!({
        "type": kind,
        "data": data
    }))
    .to_string()
}

async fn handle_socket(socket: WebSocket, auth_user: AuthUser) {
    if let Some(session) = auth_user.session {
        let socket = Arc::new(Mutex::from(socket));
        let mut user_data = get_initial_user(&auth_user.me).await;

        let mut presences = serde_json::Map::new();

        let client = RD_POOL.get().unwrap().clone();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();

        let fetch_tasks = user_data
            .get("users")
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(friend_id, _)| {
                let mut conn = conn.clone();
                let fid = friend_id.clone();
                async move {
                    let presence: String = conn
                        .hget(format!("user:{}", fid), "presence")
                        .await
                        .unwrap_or("OFFLINE".to_owned());
                    let last_seen: Option<String> = conn
                        .hget(format!("user-lastSeen:{}", fid), "lastSeen")
                        .await
                        .unwrap_or_default();
                    (fid, presence, last_seen)
                }
            });

        let results = join_all(fetch_tasks).await;

        for (fid, presence, last_seen) in results {
            presences.insert(fid, json!({ "presence": presence, "lastSeen": last_seen }));
        }

        if let Value::Object(ref mut map) = user_data {
            map.entry("presence").or_insert(json!(&presences));
            map.insert("session".to_string(), json!(&session.id));
            map.insert("push_token".to_string(), json!(&auth_user.push_token));
        }

        if socket
            .lock()
            .await
            .send(compress_msg(payload("INIT", Some(&user_data))))
            .await
            .is_err()
        {
            return;
        }

        let user_id = auth_user.me.clone();
        let friend_ids: Vec<String> = user_data
            .get("users")
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        let (mut sink, mut stream) = client.get_async_pubsub().await.unwrap().split();

        let socket_spawn = socket.clone();
        let sid = session.id.clone();
        let mut conn_spawn = conn.clone();
        tokio::spawn(async move {
            let mut subscriptions: Vec<String> = vec![];

            sink.subscribe(&[format!("U.{}", user_id)]).await.ok();

            subscriptions.push(format!("U.{}", user_id));

            for fid in friend_ids {
                sink.subscribe(format!("F.{}", fid)).await.ok();
                subscriptions.push(format!("F.{}", fid));

                if fid != auth_user.user.id {
                    conn_spawn
                        .publish::<String, Vec<u8>, ()>(
                            format!("U.{}", fid),
                            compress(payload(
                                "PRESENCE_UPDATE",
                                Some(&json!({
                                    "id": &user_id,
                                    "presence": "ONLINE",
                                    "lastSeen": null
                                })),
                            )),
                        )
                        .await
                        .ok();
                }
            }

            redis::cmd("SADD")
                .arg(format!("user:session:{}:subscriptions", sid))
                .arg(subscriptions.to_redis_args())
                .query_async::<String>(&mut conn)
                .await
                .unwrap();

            while let Some(msg) = stream.next().await {
                let payload = msg.get_payload_bytes();

                if socket_spawn
                    .lock()
                    .await
                    .send(Message::from(payload))
                    .await
                    .is_err()
                {
                    return;
                };
            }
        });

        while let Some(Ok(msg)) = socket.lock().await.recv().await {
            match decompress(msg) {
                Ok(event) => match event.kind.as_str() {
                    "PING" => {
                        if socket
                            .lock()
                            .await
                            .send(compress_msg(payload("PONG", None)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }

                    "PUSH_TOKEN" => {
                        if let Ok(RealTimeEvents::PushToken { token }) =
                            serde_json::from_value(event.data)
                        {
                            let pool = DB_POOL.get().unwrap();

                            let push_token_res = sqlx::query(
                                r#"INSERT INTO
                            "push_tokens" ("id", "session_id", "token", "user_id")
                                VALUES
                                  ($1, $2, $3, $4)
                                ON CONFLICT ("session_id") DO
                                UPDATE
                                SET
                            "token" = $5"#,
                            )
                            .bind(cuid1().unwrap())
                            .bind(&session.id)
                            .bind(&token)
                            .bind(&auth_user.me)
                            .bind(&token)
                            .execute(pool)
                            .await;

                            if push_token_res.is_err() {
                                log_error(
                                    "RealTimeRoutes_PUSH_TOKEN",
                                    push_token_res.as_ref().err(),
                                );
                            }
                        } else {
                            // TODO: ERROR
                        }
                    }

                    _ => {}
                },
                Err(_) => {}
            }
        }

        println!("Session ID {} Disconnected", &session.id);

        let mut pubsub = client.get_async_pubsub().await.unwrap();
        let mut conn_close = client.get_multiplexed_async_connection().await.unwrap();
        let sub_key = format!("user:session:{}:subscriptions", &session.id);
        let subscriptions = conn_close
            .smembers::<_, Vec<String>>(&sub_key)
            .await
            .unwrap_or(vec![]);

        println!("{:?}", subscriptions);
        pubsub.unsubscribe(subscriptions).await.ok();
        conn_close.del::<_, String>(&sub_key).await.unwrap();
    }
}
