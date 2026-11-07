//! Where `before_find` (adds conditions before reading) applies, and the re-entry error.

use cakedome::Context;
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::repo::{FindOptions, FindQuery, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, DatabaseConnection};

use crate::common::registry;
use crate::entities::customers;
use crate::support::{Log, context, db, identity, insert_customer};

/// Hides customers named `hidden_*`, recording each operation. `OnlyName` narrows to that name.
struct Hiding {
    log: Log,
}

struct OnlyName(&'static str);

#[async_trait]
impl Repo for Hiding {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<customers::Entity>,
        options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        self.log.push(format!("{:?}", query.action()));
        query.filter(
            Condition::all()
                .add(customers::Column::Name.starts_with("hidden_"))
                .not(),
        );
        if let Some(OnlyName(name)) = options.get::<OnlyName>() {
            query.filter(customers::Column::Name.eq(*name));
        }
        Ok(())
    }
}

impl TablePolicy for Hiding {}
impl EntityPolicy for Hiding {}

/// Reads the same Entity inside `before_find`.
struct Looping;

#[async_trait]
impl Repo for Looping {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        ctx: &Context,
        _query: &mut FindQuery<customers::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        ctx.repo::<Self>().find().all().await?;
        Ok(())
    }
}

impl TablePolicy for Looping {}
impl EntityPolicy for Looping {}

fn hiding(db: &DatabaseConnection) -> (Context, Log) {
    let log = Log::default();
    let mut r = registry();
    r.add(Hiding { log: log.clone() });
    (context(db, identity(&["entity.customers.*"]), r), log)
}

fn names(rows: &[customers::Model]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn before_find_applies_to_all_count_and_get_even_unchecked() {
    let db = db().await;
    insert_customer(&db, "shown", true).await;
    let hidden = insert_customer(&db, "hidden_a", true).await;
    let (ctx, _) = hiding(&db);
    let repo = ctx.repo::<Hiding>();

    assert_eq!(names(&repo.find().all().await.unwrap()), ["shown"]);
    assert_eq!(repo.find().count().await.unwrap(), 1);
    let res = repo.get(hidden.id).await;
    assert!(matches!(res, Err(RepoError::NotFound)), "{res:?}");
    let unchecked = ctx.repo::<Hiding>().unchecked();
    assert_eq!(names(&unchecked.find().all().await.unwrap()), ["shown"]);
}

#[tokio::test]
async fn edit_and_delete_read_the_row_through_before_find() {
    let db = db().await;
    let hidden = insert_customer(&db, "hidden_a", true).await;
    let (ctx, log) = hiding(&db);
    let repo = ctx.repo::<Hiding>();

    let edit = repo
        .save(customers::ActiveModel {
            id: Unchanged(hidden.id),
            name: Set("renamed".to_string()),
            ..Default::default()
        })
        .await;
    let delete = repo.delete(hidden.id).await;

    assert!(matches!(edit, Err(RepoError::NotFound)), "{edit:?}");
    assert!(matches!(delete, Err(RepoError::NotFound)), "{delete:?}");
    assert_eq!(log.take(), ["Edit", "Delete"]);
}

#[tokio::test]
async fn options_reach_before_find() {
    let db = db().await;
    insert_customer(&db, "apple", true).await;
    insert_customer(&db, "banana", true).await;
    let (ctx, _) = hiding(&db);

    let rows = ctx
        .repo::<Hiding>()
        .find()
        .option(OnlyName("banana"))
        .all()
        .await
        .unwrap();

    assert_eq!(names(&rows), ["banana"]);
}

#[tokio::test]
async fn reading_the_same_entity_inside_before_find_is_an_error() {
    let db = db().await;
    let mut r = registry();
    r.add(Looping);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let res = ctx.repo::<Looping>().find().all().await;

    match res {
        Err(RepoError::BeforeFindReentered(name)) => assert_eq!(name, "customers"),
        other => panic!("expected BeforeFindReentered: {other:?}"),
    }
}
