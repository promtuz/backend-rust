use axum::{http::Method, routing::get, Router};

use once_cell::sync::OnceCell;
use redis::{Client, ConnectionLike};
use sqlx::{postgres::PgPoolOptions, Postgres};

static DB_POOL: OnceCell<sqlx::Pool<Postgres>> = OnceCell::new();
static RD_POOL: OnceCell<Client> = OnceCell::new();

use tower::ServiceBuilder;
use tower_http::cors::{AllowOrigin, CorsLayer};

mod features;
mod middlewares;
mod utils;
mod database;

use features::{auth::routes as auth_routes, realtime::routes as realtime_routes};

async fn root() -> &'static str {
    "Sup!"
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let db_uri = std::env::var("DATABASE_URL").unwrap_or_default();


    let mut rd_client = redis::Client::open("redis://127.0.0.1/").unwrap();
    
    println!("Connected to RedisDB : {}", rd_client.check_connection());

    RD_POOL.set(rd_client).unwrap();

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_uri)
        .await
        .unwrap();

    println!("Connected to PostgreSQL : {}", !pool.is_closed());

    DB_POOL.set(pool).unwrap();
    let cors_layer = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "http://localhost:3000".parse().unwrap(),
            "https://promtuz.xyz".parse().unwrap(),
        ]))
        .allow_credentials(true)
        .allow_methods(vec![Method::GET, Method::POST]);

    let app = Router::new()
        .route("/", get(root))
        .nest("/auth", auth_routes::routes())
        .nest("/ws", realtime_routes::routes())
        .layer(
            ServiceBuilder::new()
                .layer(cors_layer)
        );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8888").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
