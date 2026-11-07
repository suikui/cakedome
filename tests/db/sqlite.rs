//! SQLite DB (a fresh in-memory DB per test).

use std::path::PathBuf;
use std::time::Duration;

use sea_orm::{ConnectOptions, DatabaseConnection};

use crate::support::connect;

pub async fn db() -> DatabaseConnection {
    // An in-memory SQLite DB is per connection, so use a single connection.
    // Taking a second connection inside a transaction would hang, so keep the wait short and fail.
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    connect(options).await
}

/// File DB with two connections for interleaved-transaction tests; removed on drop.
pub struct FileDb {
    pub db: DatabaseConnection,
    path: PathBuf,
}

impl FileDb {
    pub async fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cakedome-test-{}.sqlite", uuid::Uuid::now_v7()));
        let mut options = ConnectOptions::new(format!("sqlite://{}?mode=rwc", path.display()));
        options
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .sqlx_logging(false);
        Self {
            db: connect(options).await,
            path,
        }
    }
}

impl Drop for FileDb {
    fn drop(&mut self) {
        // Journal files remain only if the process dies mid-transaction.
        for suffix in ["", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
