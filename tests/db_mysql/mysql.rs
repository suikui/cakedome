//! MySQL DB (a fresh DB per test).

use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection};

use crate::support::connect;

/// Each test only creates and uses its own `cakedome_test_*` database, so the user needs
/// `CREATE` on those names.
pub async fn db() -> DatabaseConnection {
    let url = std::env::var("CAKEDOME_TEST_MYSQL_URL").expect("CAKEDOME_TEST_MYSQL_URL is not set");
    let name = format!("cakedome_test_{}", uuid::Uuid::now_v7().simple());
    Database::connect(url.as_str())
        .await
        .unwrap()
        .execute_unprepared(&format!(
            "CREATE DATABASE `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_as_ci"
        ))
        .await
        .unwrap();
    let base = url.rsplit_once('/').map_or(url.as_str(), |(base, _)| base);
    let mut options = ConnectOptions::new(format!("{base}/{name}"));
    options
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    connect(options).await
}
