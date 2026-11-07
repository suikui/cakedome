//! Tests that only hold on SQLite; the row-lock DBs are in `db_pg/row_lock.rs`.

use cakedome::repo::{DefaultRepo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};

use crate::backend::FileDb;
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

/// SQLite has no row locks, so `FOR UPDATE` is dropped from the optimistic-lock read. The
/// later save cannot see uncommitted values and passes the `modified` check, but fails with a DB
/// error when it cannot take the write lock (MySQL waits for the first commit and returns `Conflict`).
#[tokio::test]
async fn a_save_racing_an_uncommitted_save_fails_without_overwriting_it() {
    let file = FileDb::new().await;
    let db = &file.db;
    let row = insert_customer(db, "row", true).await;
    let ctx = context(db, identity(ALL), full_registry());

    let first = ctx.begin().await.unwrap();
    first
        .repo::<Customers>()
        .save_if_unmodified(rename(row.id, "first"), row.modified)
        .await
        .unwrap();
    let second = ctx.begin().await.unwrap();
    let res = second
        .repo::<Customers>()
        .save_if_unmodified(rename(row.id, "second"), row.modified)
        .await;
    assert!(matches!(res, Err(RepoError::Db(_))), "{res:?}");
    second.rollback().await.unwrap();
    first.commit().await.unwrap();

    let name = raw_customer(db, row.id).await.map(|c| c.name);
    assert_eq!(name.as_deref(), Some("first"));
}
