use std::collections::HashSet;

use chrono::{DateTime, Utc};
use redis::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{Pool, Postgres};

use crate::{
    utils::{cached_query, log_error},
    DB_POOL, RD_POOL,
};

#[derive(sqlx::FromRow, Serialize, Deserialize)]
struct JsonResult {
    json: Value,
}

#[derive(sqlx::FromRow, Serialize, Deserialize)]
struct User {
    id: String,
    display_name: String,
    username: String,
}

#[derive(sqlx::FromRow, Serialize, Deserialize)]
struct Friend {
    id: String,
    user_a: String,
    user_b: String,
    accepted_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

async fn get_me_user(id: &String, client: &Client, pool: &Pool<Postgres>) -> Value {
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();

    cached_query(
        &mut conn,
        &format!("CACHE:ME_USER:{}", &id),
        21600,
        || async {
            let user_me_res = sqlx::query_as::<_, User>(
                "SELECT id, display_name, username FROM users WHERE id = $1",
            )
            .bind(&id)
            .fetch_one(pool)
            .await;
            if user_me_res.is_err() {
                log_error("GetIntialUser_GMU_UMR", user_me_res.as_ref().err());
            }
            json!(user_me_res.unwrap())
        },
    )
    .await
}

async fn get_user_relationships(
    id: &String,
    channels: &Value,
    client: &Client,
    pool: &Pool<Postgres>,
) -> Value {
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();

    let user_friends = cached_query(
        &mut conn,
        &format!("CACHE:U_FRNDS:{}", &id),
        21600,
        || async {
            let user_friends_res: Result<Vec<Friend>, sqlx::Error> = sqlx::query_as::<_, Friend>(
                r#"SELECT * FROM friends WHERE
              (
                "user_a" = $1
                OR "user_b" = $1
              )"#,
            )
            .bind(&id)
            .fetch_all(pool)
            .await;
            if user_friends_res.is_err() {
                log_error("GetIntialUser_GUR_UFR", user_friends_res.as_ref().err());
            }
            user_friends_res.unwrap()
        },
    )
    .await;

    let mut users_to_load: HashSet<String> = HashSet::new();

    let channels_map = channels.as_object().unwrap();
    for (_, channel) in channels_map {
        for member in channel
            .get("members")
            .unwrap_or(&json!("[]"))
            .as_array()
            .unwrap_or(&vec![])
        {
            users_to_load.insert(member.as_str().unwrap().to_string());
        }
    }

    user_friends.iter().clone().for_each(|f| {
        users_to_load.insert(if *id == f.user_a {
            f.user_b.clone()
        } else {
            f.user_a.clone()
        });
    });

    let mut users_list: Vec<String> = users_to_load.iter().cloned().collect();

    users_list.push("".to_owned());

    let users_ss = cached_query(&mut conn, &format!("CACHE:UF:{}", &id), 21600, || async {
        let query = format!(
            "SELECT id, display_name, username FROM users WHERE id in ({})",
            (0..users_list.len())
                .map(|i| format!("${}", i + 1))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut users_qb = sqlx::query_as::<_, User>(&query);

        for id in users_list {
            users_qb = users_qb.bind(id);
        }

        let users_res = users_qb.fetch_all(pool).await;

        if users_res.is_err() {
            log_error("GetIntialUser_GUR_UR", users_res.as_ref().err());
        }

        users_res.unwrap()
    })
    .await;

    let users: Map<String, Value> = users_ss
        .iter()
        .map(|user| (user.id.clone(), json!(user)))
        .collect();

    let relationships: Map<String, Value> = user_friends
        .into_iter()
        .map(|friend| {
            let user_id = if friend.user_a == *id {
                friend.user_b.clone()
            } else {
                friend.user_a.clone()
            };
            let relationship = json!({
                "id": friend.id,
                "user_id": user_id,
                "incoming": friend.user_a != *id,
                "accepted_at": friend.accepted_at,
                "created_at": friend.created_at,
            });
            (friend.id, relationship)
        })
        .collect();

    json!({
      "relationships": relationships,
      "users": users
    })
}

async fn get_channels(id: &String, client: &Client, pool: &Pool<Postgres>) -> Value {
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();

    let channels = cached_query(
        &mut conn,
        &format!("CACHE:U_CHANNELS:{}", &id),
        21600,
        || async {
            let channels_res = sqlx::query_as::<_, JsonResult>(
                r#"SELECT json_agg(to_json(sub)) AS json FROM (
              SELECT
            "c"."id",
            "c"."name",
            "c"."type",
            "c"."created_at",
            "message_reads"."read_at",
            "message_reads"."last_read_message_id",
            array_agg("members"."user_id") AS "members",
            (
              SELECT to_json(obj)
              FROM
                (
                  SELECT
                    "lm"."id",
                    LEFT(lm.content, 64) AS "content",
                    "lm"."created_at",
                    "lm"."channel_id",
                    "lm"."reply_to",
                    "lm"."author_id"
                  FROM "messages" AS "lm"
                  WHERE "lm"."channel_id" = "c"."id"
                  ORDER BY "lm"."created_at" DESC
                  LIMIT 1
                ) AS obj
            ) AS "last_message",
            EXISTS (
              SELECT FROM "messages"
              WHERE 
                "messages"."channel_id" = "cm"."channel_id"
              LIMIT 1
            ) AS "messages_exist"
          FROM
            "channel_members" AS "cm"
            LEFT JOIN "channels" AS "c" ON "cm"."channel_id" = "c"."id"
            LEFT JOIN "channel_members" AS "members" ON "members"."channel_id" = "cm"."channel_id"
            LEFT JOIN "message_reads" ON "message_reads"."channel_id" = "cm"."channel_id"
          WHERE
            "cm"."user_id" = $1
          GROUP BY
            "c"."id",
            "c"."name",
            "c"."type",
            "c"."created_at",
            "message_reads"."read_at",
            "cm"."channel_id",
            "message_reads"."last_read_message_id"
          ORDER BY
            "c"."created_at" DESC) AS sub"#,
            )
            .bind(&id)
            .fetch_one(pool)
            .await;

            if channels_res.is_err() {
                log_error("GetIntialUser_GC_CR", channels_res.as_ref().err());
            }

            channels_res.unwrap().json
        },
    )
    .await;

    let map: Map<String, Value> = channels
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?.to_owned();
            Some((id, v.clone()))
        })
        .collect();

    return Value::Object(map);
}

pub async fn get_initial_user(id: &String) -> Value {
    let client = RD_POOL.get().unwrap();

    let pool = DB_POOL.get().unwrap();

    let (me, channels) = tokio::join!(
        get_me_user(&id, client, pool),
        get_channels(&id, client, pool),
    );
    let mut relationships = get_user_relationships(&id, &channels, client, &pool).await;

    let partial = json!({
      "me": me,
      "channels": channels
    });

    if let (Value::Object(ref mut a_map), Value::Object(b_map)) = (&mut relationships, partial) {
        a_map.extend(b_map);
    }

    return relationships;
}
