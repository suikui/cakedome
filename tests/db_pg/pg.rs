//! PostgreSQL DB (a fresh schema per test).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};

use crate::support::{connect, is_stale_test_db};

static SWEPT: AtomicBool = AtomicBool::new(false);

/// Tests do not drop the schemas they create, so the first test of a run drops those left by
/// earlier runs. Ones under ten minutes old may belong to a run in progress and are kept. Errors are
/// ignored.
async fn sweep(admin: &DatabaseConnection) {
    let rows = admin
        .query_all_raw(Statement::from_string(
            admin.get_database_backend(),
            "SELECT nspname FROM pg_namespace WHERE nspname LIKE 'cakedome\\_test\\_%'",
        ))
        .await
        .unwrap_or_default();
    for row in rows {
        let Ok(name) = row.try_get_by_index::<String>(0) else {
            continue;
        };
        if is_stale_test_db(&name) {
            let _ = admin
                .execute_unprepared(&format!("DROP SCHEMA IF EXISTS {name} CASCADE"))
                .await;
        }
    }
}

/// Each test only creates and uses its own `cakedome_test_*` schema.
pub async fn db() -> DatabaseConnection {
    let url = std::env::var("CAKEDOME_TEST_PG_URL").expect("CAKEDOME_TEST_PG_URL is not set");
    let schema = format!("cakedome_test_{}", uuid::Uuid::now_v7().simple());
    let admin = Database::connect(url.as_str()).await.unwrap();
    if !SWEPT.swap(true, Ordering::SeqCst) {
        sweep(&admin).await;
    }
    admin
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
