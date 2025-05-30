use std::{error::Error, future::Future};

use axum::http;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

pub mod decompress;

pub fn get_cookie(headers: &http::HeaderMap, key: &str) -> Option<String> {
  headers.get("cookie")
      .and_then(|v| v.to_str().ok())
      .and_then(|cookies| cookies.split(';')
          .find_map(|cookie| {
              let mut parts = cookie.trim().splitn(2, '=');
              match (parts.next(), parts.next()) {
                  (Some(k), Some(v)) if k == key => Some(v.to_string()),
                  _ => None,
              }
          }))
}

pub async fn cached_query<T, F, Fut>(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &String,
    ttl_secs: u64,
    query_fn: F,
) -> T
where
    T: Serialize + for<'de> Deserialize<'de>,
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    // Try Redis
    if let Ok(json) = conn.get::<_, String>(key).await {
        if let Ok(parsed) = serde_json::from_str::<T>(&json) {
            return parsed;
        }
    }

    // Fallback to SQL
    let result = query_fn().await;

    // Store in Redis
    conn.set_ex::<_, String, ()>(key, serde_json::to_string(&result).unwrap(), ttl_secs).await.ok();

    result
}

pub fn log_error(label: &str, _err: Option<&sqlx::Error>) {
    let err = _err.unwrap();
    
    eprintln!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("⚠️  DATABASE ERROR: {}", label);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("📌 Error Type: {:?}", err.as_database_error());
    
    if let Some(db_err) = err.as_database_error() {
        if let Some(code) = db_err.code() {
            eprintln!("📋 Error Code: {}", code);
        }
        if let Some(constraint) = db_err.constraint() {
            eprintln!("🔒 Constraint: {}", constraint);
        }
        if let Some(table) = db_err.table() {
            eprintln!("🗃️  Table: {}", table);
        }
    }
    
    eprintln!("📝 Message: {}", err);
    
    // Print error source chain if available
    let mut source = err.source();
    if source.is_some() {
        eprintln!("\n🔍 Error source chain:");
        let mut level = 1;
        while let Some(err_source) = source {
            eprintln!("   {} Level {}: {}", "→".repeat(level), level, err_source);
            source = err_source.source();
            level += 1;
        }
    }
    
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
}