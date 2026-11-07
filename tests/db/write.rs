//! Save and delete: managed columns, soft vs hard delete, uniqueness, optimistic locking.

use cakedome::repo::{DefaultRepo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::{ActiveModelTrait, EntityTrait};

use crate::common::registry;
use crate::entities::{customers, logs, master_tags};
use crate::support::{
    assert_between, context, db, identity, insert_customer, insert_tag, past, raw_customer,
};

type Customers = DefaultRepo<customers::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;
type Logs = DefaultRepo<logs::Entity>;

fn full_registry() -> cakedome::registry::Registry {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Tags::new());
    r.add(Logs::new());
    r
}

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.master_tags.*",
    "entity.logs.*",
];

fn new_customer(name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

fn rename(id: i64, name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        id: Unchanged(id),
        name: Set(name.to_string()),
        ..Default::default()
    }
}

fn codes(errors: &cakedome::marshal::FieldErrors, field: &str) -> Vec<&'static str> {
    errors.0[field].iter().map(|e| e.code).collect()
}

#[tokio::test]
async fn add_sets_created_and_modified_and_makes_the_row_active() {
    let db = db().await;
    let ctx = context(&db, identity(&["entity.customers.add"]), full_registry());

    let from = cakedome::clock::now();
    let saved = ctx
        .repo::<Customers>()
        .save(new_customer("added"))
        .await
        .unwrap();
    let to = cakedome::clock::now();

    assert!(saved.id > 0);
    assert_eq!(saved.created, saved.modified);
    assert_between(saved.created, from, to);
    assert!(saved.is_active);
    assert_eq!(raw_customer(&db, saved.id).await, Some(saved));
}

#[tokio::test]
async fn edit_writes_set_columns_and_modified_only() {
    let db = db().await;
    let row = insert_customer(&db, "before", true).await;
    let ctx = context(&db, identity(&["entity.customers.edit"]), full_registry());

    let from = cakedome::clock::now();
    let saved = ctx
        .repo::<Customers>()
        .save(rename(row.id, "after"))
        .await
        .unwrap();
    let to = cakedome::clock::now();

    assert_eq!(saved.name, "after");
    assert_eq!(saved.created, past(), "created is unchanged");
    assert_between(saved.modified, from, to);
    assert_eq!(raw_customer(&db, row.id).await, Some(saved));
}

#[tokio::test]
async fn managed_columns_cannot_be_set() {
    let db = db().await;
    let row = insert_customer(&db, "row", true).await;
    let ctx = context(&db, identity(ALL), full_registry());
    let repo = ctx.repo::<Customers>();

    let mut add = new_customer("added");
    add.created = Set(past());
    let res = repo.save(add).await;
    assert!(
        matches!(&res, Err(RepoError::ManagedColumn(c)) if c == "created"),
        "{res:?}"
    );

    let edit = customers::ActiveModel {
        id: Unchanged(row.id),
        is_active: Set(false),
        ..Default::default()
    };
    let res = repo.save(edit).await;
    assert!(
        matches!(&res, Err(RepoError::ManagedColumn(c)) if c == "is_active"),
        "{res:?}"
    );

    assert_eq!(customers::Entity::find().all(&db).await.unwrap(), [row]);
}

#[tokio::test]
async fn delete_of_soft_delete_table_makes_the_row_inactive() {
    let db = db().await;
    let row = insert_customer(&db, "row", true).await;
    let ctx = context(&db, identity(ALL), full_registry());
    let repo = ctx.repo::<Customers>();

    repo.delete(row.id).await.unwrap();

    let raw = raw_customer(&db, row.id).await.expect("row remains");
    assert!(!raw.is_active);
    let res = repo.get(row.id).await;
    assert!(matches!(res, Err(RepoError::NotFound)), "{res:?}");
}

#[tokio::test]
async fn delete_of_hard_delete_table_removes_the_row() {
    let db = db().await;
    let row = logs::ActiveModel {
        message: Set("hello".to_string()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let ctx = context(&db, identity(ALL), full_registry());

    ctx.repo::<Logs>().delete(row.id).await.unwrap();

    assert_eq!(
        logs::Entity::find_by_id(row.id).one(&db).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn rows_of_activatable_table_cannot_be_deleted() {
    let db = db().await;
    let tag = insert_tag(&db, "t1", true).await;
    let ctx = context(&db, identity(ALL), full_registry());

    let res = ctx.repo::<Tags>().delete(tag.id).await;

    assert!(matches!(res, Err(RepoError::NotDeletable)), "{res:?}");
    assert_eq!(
        master_tags::Entity::find_by_id(tag.id)
            .one(&db)
            .await
            .unwrap(),
        Some(tag)
    );
}

#[tokio::test]
async fn unique_columns_are_checked_before_writing() {
    let db = db().await;
    insert_tag(&db, "dup", true).await;
    let ctx = context(&db, identity(ALL), full_registry());

    let res = ctx
        .repo::<Tags>()
        .save(master_tags::ActiveModel {
            code: Set("dup".to_string()),
            name: Set("2つ目".to_string()),
            is_active: Set(true),
            ..Default::default()
        })
        .await;

    match res {
        Err(RepoError::Validation(errors)) => assert_eq!(codes(&errors, "code"), ["unique"]),
        other => panic!("expected unique: {other:?}"),
    }
    assert_eq!(master_tags::Entity::find().all(&db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn db_unique_violation_is_duplicate() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), full_registry());
    let repo = ctx.repo::<Customers>();
    let with_email = |name: &str| customers::ActiveModel {
        email: Set(Some("same@example.com".to_string())),
        ..new_customer(name)
    };

    repo.save(with_email("first")).await.unwrap();
    let res = repo.save(with_email("second")).await;

    assert!(matches!(res, Err(RepoError::Duplicate)), "{res:?}");
}

#[tokio::test]
async fn save_if_unmodified_rejects_a_stale_modified() {
    let db = db().await;
    let row = insert_customer(&db, "row", true).await;
    let ctx = context(&db, identity(ALL), full_registry());

    let tx = ctx.begin().await.unwrap();
    let stale = past() - chrono::Duration::seconds(1);
    let res = tx
        .repo::<Customers>()
        .save_if_unmodified(rename(row.id, "stale"), stale)
        .await;
    assert!(matches!(res, Err(RepoError::Conflict)), "{res:?}");
    let saved = tx
        .repo::<Customers>()
        .save_if_unmodified(rename(row.id, "fresh"), row.modified)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(saved.name, "fresh");
    assert_eq!(raw_customer(&db, row.id).await, Some(saved));
}
