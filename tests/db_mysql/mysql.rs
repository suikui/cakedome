//! MySQL DB (a fresh DB per test).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cakedome::patch::{PatchError, Patching};
use cakedome::repo::DefaultRepo;
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Set, Statement,
};
use serde_json::json;

use crate::common::registry;
use crate::entities::master_tags;
use crate::support::{connect, context, identity, is_stale_test_db};

static SWEPT: AtomicBool = AtomicBool::new(false);

/// Tests do not drop the databases they create, so the first test of a run drops those left by
/// earlier runs. Ones under ten minutes old may belong to a run in progress and are kept. Errors are
/// ignored.
async fn sweep(admin: &DatabaseConnection) {
    let rows = admin
        .query_all_raw(Statement::from_string(
            admin.get_database_backend(),
            "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'cakedome\\_test\\_%'",
        ))
        .await
        .unwrap_or_default();
    for row in rows {
        let Ok(name) = row.try_get_by_index::<String>(0) else {
            continue;
        };
        if is_stale_test_db(&name) {
            let _ = admin
                .execute_unprepared(&format!("DROP DATABASE IF EXISTS `{name}`"))
                .await;
        }
    }
}

/// Each test only creates and uses its own `cakedome_test_*` database, so the user needs
/// `CREATE` on those names.
pub async fn db() -> DatabaseConnection {
    let url = std::env::var("CAKEDOME_TEST_MYSQL_URL").expect("CAKEDOME_TEST_MYSQL_URL is not set");
    let name = format!("cakedome_test_{}", uuid::Uuid::now_v7().simple());
    let admin = Database::connect(url.as_str()).await.unwrap();
    if !SWEPT.swap(true, Ordering::SeqCst) {
        sweep(&admin).await;
    }
    admin
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

type Tags = DefaultRepo<master_tags::Entity>;

async fn tag(db: &DatabaseConnection, code: &str, name: &str) -> master_tags::Model {
    master_tags::ActiveModel {
        code: Set(code.to_string()),
        name: Set(name.to_string()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn edit(
    db: &DatabaseConnection,
    warn_duplicate: bool,
    id: i64,
    body: serde_json::Value,
) -> Result<master_tags::Model, PatchError> {
    let mut r = registry();
    let config = r.add(Tags::new());
    if warn_duplicate {
        config.warn_duplicate([master_tags::Column::Name]);
    }
    let ctx = context(db, identity(&["entity.master_tags.*"]), r);
    let repo = ctx.repo::<Tags>();
    let patch = repo.patch(Some(id), &body).await?;
    repo.save_patch(patch).await
}

#[tokio::test]
async fn unique_does_not_compare_a_row_with_itself_when_only_the_case_changes() {
    let db = db().await;
    let own = tag(&db, "abc", "own").await;
    let other = tag(&db, "xyz", "other").await;

    let saved = edit(&db, false, own.id, json!({"data": {"code": "ABC"}}))
        .await
        .unwrap();
    assert_eq!(saved.code, "ABC");

    match edit(&db, false, other.id, json!({"data": {"code": "abc"}})).await {
        Err(PatchError::Invalid(errors)) => assert_eq!(errors.0["code"][0].code, "unique"),
        other => panic!("expected Invalid: {other:?}"),
    }
}

#[tokio::test]
async fn warn_duplicate_does_not_compare_a_row_with_itself_when_only_the_case_changes() {
    let db = db().await;
    let own = tag(&db, "c1", "abc").await;
    let other = tag(&db, "c2", "xyz").await;
    let body = |name: &str| json!({"data": {"name": name}, "acceptWarnings": []});

    let saved = edit(&db, true, own.id, body("ABC")).await.unwrap();
    assert_eq!(saved.name, "ABC");

    match edit(&db, true, other.id, body("abc")).await {
        Err(PatchError::Warnings(w)) => {
            assert_eq!(
                w.into_iter().map(|w| w.key).collect::<Vec<_>>(),
                ["name.duplicate"]
            );
        }
        other => panic!("expected Warnings: {other:?}"),
    }
}
