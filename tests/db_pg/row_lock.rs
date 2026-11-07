//! Tests that need row locks (PostgreSQL, MySQL); the SQLite counterpart is `db/sqlite_only.rs`.

use std::time::Duration;

use cakedome::repo::{DefaultRepo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};

use crate::backend::db;
use crate::common::registry;
use crate::entities::customers;
use crate::support::{context, identity, insert_customer, raw_customer};

type Customers = DefaultRepo<customers::Entity>;

const ALL: &[&str] = &["entity.customers.*"];

fn full_registry() -> cakedome::registry::Registry {
    let mut r = registry();
    r.add(Customers::new());
    r
}

fn rename(id: i64, name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        id: Unchanged(id),
        name: Set(name.to_string()),
        ..Default::default()
    }
}

/// Optimistic locking reads the row with `SELECT … FOR UPDATE`, so a save queued behind an
/// uncommitted one waits, re-reads, sees a changed `modified` and gets `Conflict`.
#[tokio::test]
async fn a_save_racing_an_uncommitted_save_waits_and_then_conflicts() {
    let db = db().await;
    let row = insert_customer(&db, "row", true).await;
    let ctx = context(&db, identity(ALL), full_registry());

    let first = ctx.begin().await.unwrap();
    first
        .repo::<Customers>()
        .save_if_unmodified(rename(row.id, "first"), row.modified)
        .await
        .unwrap();
    // Run the second save on another connection to the same DB.
    let racing = context(&db, identity(ALL), full_registry());
    let second = tokio::spawn(async move {
        let tx = racing.begin().await.unwrap();
        let res = tx
            .repo::<Customers>()
            .save_if_unmodified(rename(row.id, "second"), row.modified)
            .await;
        tx.rollback().await.unwrap();
        res.map(|_| ())
    });
    // Commit the first save while the second waits for its row lock.
    tokio::time::sleep(Duration::from_millis(300)).await;
    first.commit().await.unwrap();
    let res = second.await.unwrap();

    assert!(matches!(res, Err(RepoError::Conflict)), "{res:?}");
    let name = raw_customer(&db, row.id).await.map(|c| c.name);
    assert_eq!(name.as_deref(), Some("first"));
}
