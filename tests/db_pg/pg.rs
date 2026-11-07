//! PostgreSQL DB (a fresh schema per test).

use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection};

use crate::support::connect;

/// Each test only creates and uses its own `cakedome_test_*` schema.
pub async fn db() -> DatabaseConnection {
    let url = std::env::var("CAKEDOME_TEST_PG_URL").expect("CAKEDOME_TEST_PG_URL is not set");
    let schema = format!("cakedome_test_{}", uuid::Uuid::now_v7().simple());
    Database::connect(url.as_str())
        .await
        .unwrap()
        .execute_unprepared(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .set_schema_search_path(schema)
        .sqlx_logging(false);
    connect(options).await
}
