//! Application values on the Context (`with_extension` / `extension`).

use cakedome::Context;
use cakedome::marshal::WriteMode;
use cakedome::policy::{Action, EntityPolicy, TablePolicy};
use cakedome::repo::{Repo, RepoError};
use sea_orm::ActiveValue::Set;
use sea_orm::prelude::async_trait::async_trait;

use crate::common::registry;
use crate::entities::customers;
use crate::support::{Log, context, db, identity};

#[derive(Debug, PartialEq)]
struct ReadOnly(bool);

#[derive(Debug, PartialEq)]
struct Locale(&'static str);

/// Denies everything but `View` while the request is read-only, and logs the locale after commit.
struct Customers {
    log: Log,
}

#[async_trait]
impl Repo for Customers {
    type Entity = customers::Entity;

    async fn after_save_commit(
        &self,
        ctx: &Context,
        _model: &customers::Model,
        _mode: WriteMode,
    ) -> Result<(), RepoError> {
        self.log
            .push(format!("after_save_commit {:?}", ctx.extension::<Locale>()));
        Ok(())
    }
}

#[async_trait]
impl TablePolicy for Customers {
    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError> {
        let read_only = ctx.extension::<ReadOnly>().is_some_and(|r| r.0);
        Ok(action == Action::View || !read_only)
    }
}

impl EntityPolicy for Customers {}

fn new_customer(name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn policies_read_application_values_from_the_context() {
    let db = db().await;
    let mut r = registry();
    r.add(Customers {
        log: Log::default(),
    });
    let ctx = context(&db, identity(&[]), r).with_extension(ReadOnly(true));
    let repo = ctx.repo::<Customers>();

    assert!(repo.can(Action::View).await.unwrap());
    assert!(!repo.can(Action::Add).await.unwrap());
    let saved = repo.save(new_customer("a")).await;
    assert!(matches!(saved, Err(RepoError::Forbidden)), "{saved:?}");
}

#[tokio::test]
async fn transactions_and_after_commit_hooks_see_the_values() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Customers { log: log.clone() });
    let ctx = context(&db, identity(&[]), r).with_extension(Locale("ja"));

    let tx = ctx.begin().await.unwrap();
    assert_eq!(tx.extension::<Locale>(), Some(&Locale("ja")));
    tx.repo::<Customers>()
        .save(new_customer("a"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(log.take(), [r#"after_save_commit Some(Locale("ja"))"#]);
}

#[tokio::test]
async fn a_missing_value_is_none_and_a_second_value_replaces_the_first() {
    let db = db().await;
    let ctx = context(&db, identity(&[]), registry())
        .with_extension(Locale("ja"))
        .with_extension(Locale("en"));

    assert_eq!(ctx.extension::<Locale>(), Some(&Locale("en")));
    assert_eq!(ctx.extension::<ReadOnly>(), None);
}

#[tokio::test]
async fn adding_a_value_starts_a_fresh_policy_cache() {
    let db = db().await;
    let mut r = registry();
    r.add(Customers {
        log: Log::default(),
    });
    let ctx = context(&db, identity(&[]), r);
    assert!(ctx.repo::<Customers>().can(Action::Add).await.unwrap());

    let ctx = ctx.with_extension(ReadOnly(true));

    assert!(!ctx.repo::<Customers>().can(Action::Add).await.unwrap());
}
