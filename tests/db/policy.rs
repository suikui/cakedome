//! Per-row Policy (`EntityPolicy`) and `unchecked()`, which skips it.

use cakedome::Context;
use cakedome::policy::{Action, ColumnAccess, EntityPolicy, TablePolicy};
use cakedome::repo::{Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition};

use crate::common::registry;
use crate::entities::customers;
use crate::support::{context, db, identity, insert_customer, raw_customer};

/// Rows named `locked` cannot be updated; only rows whose name starts with `a` are visible.
struct Guarded;

impl Repo for Guarded {
    type Entity = customers::Entity;
}

#[async_trait]
impl TablePolicy for Guarded {
    async fn scope(&self, _ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(Condition::all().add(customers::Column::Name.starts_with("a")))
    }
}

#[async_trait]
impl EntityPolicy for Guarded {
    async fn can_edit(
        &self,
        _ctx: &Context,
        before: &customers::Model,
        _after: &customers::ActiveModel,
    ) -> Result<bool, RepoError> {
        Ok(before.name != "a_locked")
    }
}

fn rename(id: i64, name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        id: Unchanged(id),
        name: Set(name.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn can_edit_decides_per_row() {
    let db = db().await;
    let locked = insert_customer(&db, "a_locked", true).await;
    let open = insert_customer(&db, "a_open", true).await;
    let mut r = registry();
    r.add(Guarded);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);
    let repo = ctx.repo::<Guarded>();

    let denied = repo.save(rename(locked.id, "a_changed")).await;
    let allowed = repo.save(rename(open.id, "a_changed")).await;

    assert!(matches!(denied, Err(RepoError::Forbidden)), "{denied:?}");
    assert_eq!(allowed.unwrap().name, "a_changed");
    assert_eq!(raw_customer(&db, locked.id).await, Some(locked));
}

#[tokio::test]
async fn unchecked_skips_permission_scope_and_entity_policy() {
    let db = db().await;
    let outside = insert_customer(&db, "b_outside", true).await;
    let locked = insert_customer(&db, "a_locked", true).await;
    let mut r = registry();
    r.add(Guarded);
    let ctx = context(&db, identity(&[]), r);
    let repo = ctx.repo::<Guarded>().unchecked();

    assert_eq!(repo.find().count().await.unwrap(), 2);
    assert_eq!(repo.get(outside.id).await.unwrap(), outside);
    let saved = repo.save(rename(locked.id, "a_changed")).await.unwrap();
    assert_eq!(saved.name, "a_changed");
    assert!(ctx.authz().is_marked());
}

/// Hides `email` and makes `name` read-only.
struct Columned;

impl Repo for Columned {
    type Entity = customers::Entity;
}

#[async_trait]
impl TablePolicy for Columned {
    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<customers::Entity>, RepoError> {
        Ok(ColumnAccess::all()
            .hide(customers::Column::Email)
            .read_only(customers::Column::Name))
    }
}

#[async_trait]
impl EntityPolicy for Columned {}

#[tokio::test]
async fn can_answers_the_table_policy_without_marking() {
    let db = db().await;
    let mut r = registry();
    r.add(Guarded);
    let ctx = context(&db, identity(&["entity.customers.view"]), r);
    let repo = ctx.repo::<Guarded>();

    assert!(repo.can(Action::View).await.unwrap());
    assert!(!repo.can(Action::Edit).await.unwrap());
    assert!(!ctx.authz().is_marked());
}

#[tokio::test]
async fn columns_return_the_table_policy_columns() {
    let db = db().await;
    let mut r = registry();
    r.add(Columned);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let access = ctx.repo::<Columned>().columns().await.unwrap();

    assert!(!access.can_read(customers::Column::Email));
    assert!(access.can_read(customers::Column::Name));
    assert!(!access.can_write(customers::Column::Name));
    assert!(access.can_write(customers::Column::IsActive));
    assert!(!ctx.authz().is_marked());
}

#[tokio::test]
async fn can_and_columns_skip_the_policy_when_unchecked() {
    let db = db().await;
    let mut r = registry();
    r.add(Columned);
    let ctx = context(&db, identity(&[]), r);
    let repo = ctx.repo::<Columned>().unchecked();

    assert!(repo.can(Action::Delete).await.unwrap());
    let access = repo.columns().await.unwrap();
    assert!(access.can_read(customers::Column::Email));
    assert!(access.can_write(customers::Column::Name));
    assert!(!ctx.authz().is_marked());
}

#[tokio::test]
async fn can_and_columns_need_a_registered_repo() {
    let db = db().await;
    let ctx = context(&db, identity(&["entity.customers.*"]), registry());

    let can = ctx.repo::<Guarded>().unchecked().can(Action::View).await;
    let columns = ctx.repo::<Columned>().unchecked().columns().await;

    assert!(matches!(can, Err(RepoError::NotRegistered(_))), "{can:?}");
    assert!(
        matches!(columns, Err(RepoError::NotRegistered(_))),
        "{columns:?}"
    );
}
