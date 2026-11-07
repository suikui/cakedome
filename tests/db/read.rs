//! Conditions always applied on read (permission, soft delete, scope), and `Find` order/count.

use cakedome::Context;
use cakedome::policy::{Action, EntityPolicy, TablePolicy};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition};

use crate::common::registry;
use crate::entities::{customers, master_tags};
use crate::support::{context, db, identity, insert_customer, insert_tag};

type Customers = DefaultRepo<customers::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;

fn customers_registry() -> cakedome::registry::Registry {
    let mut r = registry();
    r.add(Customers::new());
    r
}

fn names(rows: &[customers::Model]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

struct ScopedCustomers;

impl Repo for ScopedCustomers {
    type Entity = customers::Entity;
}

#[async_trait]
impl TablePolicy for ScopedCustomers {
    async fn scope(&self, _ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(Condition::all().add(customers::Column::Name.starts_with("a")))
    }
}

impl EntityPolicy for ScopedCustomers {}

#[tokio::test]
async fn find_requires_view_permission() {
    let db = db().await;
    let ctx = context(
        &db,
        identity(&["entity.customers.edit"]),
        customers_registry(),
    );

    let all = ctx.repo::<Customers>().find().all().await;
    let count = ctx.repo::<Customers>().find().count().await;

    assert!(matches!(all, Err(RepoError::Forbidden)), "{all:?}");
    assert!(matches!(count, Err(RepoError::Forbidden)), "{count:?}");
    assert!(!ctx.authz().is_marked(), "no mark when denied");
}

#[tokio::test]
async fn soft_deleted_rows_are_not_read() {
    let db = db().await;
    insert_customer(&db, "b", true).await;
    insert_customer(&db, "a", true).await;
    let deleted = insert_customer(&db, "c", false).await;
    let ctx = context(
        &db,
        identity(&["entity.customers.view"]),
        customers_registry(),
    );
    let repo = ctx.repo::<Customers>();

    let rows = repo
        .find()
        .order_by_asc(customers::Column::Name)
        .all()
        .await
        .unwrap();
    assert_eq!(names(&rows), ["a", "b"]);
    assert_eq!(repo.find().count().await.unwrap(), 2);
    let res = repo.get(deleted.id).await;
    assert!(matches!(res, Err(RepoError::NotFound)), "{res:?}");
    assert!(ctx.authz().is_marked());
}

#[tokio::test]
async fn scope_applies_to_find_count_and_get() {
    let db = db().await;
    let apple = insert_customer(&db, "apple", true).await;
    let banana = insert_customer(&db, "banana", true).await;
    let mut r = registry();
    r.add(ScopedCustomers);
    let ctx = context(&db, identity(&["entity.customers.view"]), r);
    let repo = ctx.repo::<ScopedCustomers>();

    assert_eq!(names(&repo.find().all().await.unwrap()), ["apple"]);
    assert_eq!(repo.find().count().await.unwrap(), 1);
    assert_eq!(repo.get(apple.id).await.unwrap(), apple);
    let res = repo.get(banana.id).await;
    assert!(matches!(res, Err(RepoError::NotFound)), "{res:?}");
}

#[tokio::test]
async fn caller_or_condition_cannot_escape_the_scope() {
    let db = db().await;
    insert_customer(&db, "apple", true).await;
    insert_customer(&db, "banana", true).await;
    let mut r = registry();
    r.add(ScopedCustomers);
    let ctx = context(&db, identity(&["entity.customers.view"]), r);

    let rows = ctx
        .repo::<ScopedCustomers>()
        .find()
        .filter(
            Condition::any()
                .add(customers::Column::Name.eq("apple"))
                .add(customers::Column::Name.eq("banana")),
        )
        .all()
        .await
        .unwrap();

    assert_eq!(names(&rows), ["apple"]);
}

#[tokio::test]
async fn inactive_rows_of_activatable_table_are_read() {
    let db = db().await;
    insert_tag(&db, "on", true).await;
    let off = insert_tag(&db, "off", false).await;
    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(&["entity.master_tags.view"]), r);
    let repo = ctx.repo::<Tags>();

    assert_eq!(repo.find().count().await.unwrap(), 2);
    assert_eq!(repo.get(off.id).await.unwrap(), off);
}

#[tokio::test]
async fn count_ignores_limit_and_offset() {
    let db = db().await;
    for name in ["n1", "n2", "n3", "n4", "n5"] {
        insert_customer(&db, name, true).await;
    }
    let ctx = context(
        &db,
        identity(&["entity.customers.view"]),
        customers_registry(),
    );
    let repo = ctx.repo::<Customers>();

    let rows = repo
        .find()
        .order_by_desc(customers::Column::Name)
        .limit(2)
        .offset(1)
        .all()
        .await
        .unwrap();
    let count = repo.find().limit(2).offset(1).count().await.unwrap();

    assert_eq!(names(&rows), ["n4", "n3"]);
    assert_eq!(count, 5);
}
