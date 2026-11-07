//! Hook order for save/delete and transactions (post-commit hooks, rollback, savepoints).

use cakedome::Context;
use cakedome::marshal::{FieldErrors, WriteMode};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::Registry;
use cakedome::repo::{Repo, RepoError};
use cakedome::validate::ValidationInput;
use sea_orm::ActiveValue::{self, Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};

use crate::common::registry;
use crate::entities::customers;
use crate::support::{Log, context, db, identity, insert_customer};

/// Records hook calls.
/// - `validate` errors on name `ng`.
/// - `before_save` derives `email` from the name.
/// - `after_save` returns `Err` on name `after_ng`.
/// - Post-commit hooks also record whether the committed row is visible through the given `ctx`.
struct Hooked {
    log: Log,
}

fn mode(mode: WriteMode) -> &'static str {
    match mode {
        WriteMode::Add => "add",
        WriteMode::Edit => "edit",
    }
}

fn name_of(am: &customers::ActiveModel) -> String {
    match &am.name {
        ActiveValue::Set(v) | ActiveValue::Unchanged(v) => v.clone(),
        ActiveValue::NotSet => "-".to_string(),
    }
}

#[async_trait]
impl Repo for Hooked {
    type Entity = customers::Entity;

    async fn before_rules(
        &self,
        _ctx: &Context,
        am: &mut customers::ActiveModel,
        m: WriteMode,
        _before: Option<&customers::Model>,
    ) -> Result<(), RepoError> {
        self.log
            .push(format!("before_rules:{}:{}", mode(m), name_of(am)));
        Ok(())
    }

    fn validate(&self, input: &ValidationInput<'_, customers::Entity>, errors: &mut FieldErrors) {
        let name = name_of(input.after);
        self.log
            .push(format!("validate:{}:{name}", mode(input.mode)));
        if name == "ng" {
            errors.add("name", "ng", "使えない名前");
        }
    }

    async fn after_rules(
        &self,
        _ctx: &Context,
        am: &customers::ActiveModel,
        m: WriteMode,
        _before: Option<&customers::Model>,
        _errors: &mut FieldErrors,
    ) -> Result<(), RepoError> {
        self.log
            .push(format!("after_rules:{}:{}", mode(m), name_of(am)));
        Ok(())
    }

    async fn before_save(
        &self,
        _ctx: &Context,
        am: &mut customers::ActiveModel,
        m: WriteMode,
        _before: Option<&customers::Model>,
    ) -> Result<(), RepoError> {
        let name = name_of(am);
        self.log.push(format!("before_save:{}:{name}", mode(m)));
        am.email = Set(Some(format!("{name}@hook")));
        Ok(())
    }

    async fn after_save(
        &self,
        _ctx: &Context,
        model: &customers::Model,
        m: WriteMode,
        _before: Option<&customers::Model>,
    ) -> Result<(), RepoError> {
        self.log
            .push(format!("after_save:{}:{}", mode(m), model.name));
        if model.name == "after_ng" {
            return Err(RepoError::Forbidden);
        }
        Ok(())
    }

    async fn after_save_commit(
        &self,
        ctx: &Context,
        model: &customers::Model,
        m: WriteMode,
    ) -> Result<(), RepoError> {
        let seen = ctx.repo::<Self>().get(model.id).await.is_ok();
        self.log
            .push(format!("commit:{}:{}:{seen}", mode(m), model.name));
        Ok(())
    }

    async fn before_delete(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("before_delete:{}", model.name));
        Ok(())
    }

    async fn after_delete(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("after_delete:{}", model.name));
        Ok(())
    }

    async fn after_delete_commit(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("delete_commit:{}", model.name));
        Ok(())
    }
}

impl TablePolicy for Hooked {}
impl EntityPolicy for Hooked {}

fn hooked(db: &DatabaseConnection) -> (Context, Log) {
    let log = Log::default();
    let mut r: Registry = registry();
    r.add(Hooked { log: log.clone() });
    r.verify();
    (context(db, identity(&["entity.customers.*"]), r), log)
}

fn new_customer(name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

async fn names(db: &DatabaseConnection) -> Vec<String> {
    customers::Entity::find()
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect()
}

#[tokio::test]
async fn add_runs_hooks_in_order_and_the_commit_hook_right_after() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let saved = ctx.repo::<Hooked>().save(new_customer("a")).await.unwrap();

    assert_eq!(
        log.take(),
        [
            "before_rules:add:a",
            "validate:add:a",
            "after_rules:add:a",
            "before_save:add:a",
            "after_save:add:a",
            "commit:add:a:true",
        ]
    );
    assert_eq!(
        saved.email.as_deref(),
        Some("a@hook"),
        "values set in before_save are written too"
    );
}

#[tokio::test]
async fn edit_runs_the_same_hooks() {
    let db = db().await;
    let row = insert_customer(&db, "before", true).await;
    let (ctx, log) = hooked(&db);

    ctx.repo::<Hooked>()
        .save(customers::ActiveModel {
            id: Unchanged(row.id),
            name: Set("after".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        log.take(),
        [
            "before_rules:edit:after",
            "validate:edit:after",
            "after_rules:edit:after",
            "before_save:edit:after",
            "after_save:edit:after",
            "commit:edit:after:true",
        ]
    );
}

#[tokio::test]
async fn validation_error_stops_before_before_save() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let res = ctx.repo::<Hooked>().save(new_customer("ng")).await;

    match res {
        Err(RepoError::Validation(errors)) => {
            assert_eq!(errors.0["name"][0].code, "ng");
        }
        other => panic!("expected Validation: {other:?}"),
    }
    assert_eq!(
        log.take(),
        [
            "before_rules:add:ng",
            "validate:add:ng",
            "after_rules:add:ng"
        ]
    );
    assert!(names(&db).await.is_empty());
}

#[tokio::test]
async fn commit_hooks_wait_for_the_outermost_commit() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let tx = ctx.begin().await.unwrap();
    tx.repo::<Hooked>().save(new_customer("a")).await.unwrap();
    let inner = tx.begin().await.unwrap();
    inner
        .repo::<Hooked>()
        .save(new_customer("b"))
        .await
        .unwrap();
    inner.commit().await.unwrap();
    let before_commit: Vec<String> = log
        .take()
        .into_iter()
        .filter(|e| e.starts_with("commit:"))
        .collect();
    tx.commit().await.unwrap();

    assert!(before_commit.is_empty(), "{before_commit:?}");
    assert_eq!(log.take(), ["commit:add:a:true", "commit:add:b:true"]);
    assert_eq!(names(&db).await, ["a", "b"]);
}

#[tokio::test]
async fn rollback_discards_writes_and_commit_hooks() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let tx = ctx.begin().await.unwrap();
    tx.repo::<Hooked>().save(new_customer("a")).await.unwrap();
    tx.rollback().await.unwrap();

    assert!(log.take().iter().all(|e| !e.starts_with("commit:")));
    assert!(names(&db).await.is_empty());
}

#[tokio::test]
async fn inner_rollback_discards_only_the_savepoint() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let tx = ctx.begin().await.unwrap();
    tx.repo::<Hooked>().save(new_customer("a")).await.unwrap();
    let inner = tx.begin().await.unwrap();
    inner
        .repo::<Hooked>()
        .save(new_customer("b"))
        .await
        .unwrap();
    inner.rollback().await.unwrap();
    log.take();
    tx.commit().await.unwrap();

    assert_eq!(log.take(), ["commit:add:a:true"]);
    assert_eq!(names(&db).await, ["a"]);
}

#[tokio::test]
async fn after_save_error_in_a_transaction_rolls_back_with_finish() {
    let db = db().await;
    let (ctx, log) = hooked(&db);

    let tx = ctx.begin().await.unwrap();
    let res = tx.repo::<Hooked>().save(new_customer("after_ng")).await;
    let res = tx.finish(res).await;

    assert!(matches!(res, Err(RepoError::Forbidden)), "{res:?}");
    assert!(log.take().iter().all(|e| !e.starts_with("commit:")));
    assert!(names(&db).await.is_empty());
}

#[tokio::test]
async fn delete_runs_hooks_in_order() {
    let db = db().await;
    let row = insert_customer(&db, "gone", true).await;
    let (ctx, log) = hooked(&db);

    ctx.repo::<Hooked>().delete(row.id).await.unwrap();

    assert_eq!(
        log.take(),
        [
            "before_delete:gone",
            "after_delete:gone",
            "delete_commit:gone"
        ]
    );
}
