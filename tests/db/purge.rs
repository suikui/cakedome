//! Physical deletes regardless of the table lifecycle (`RepoHandle::purge`).

use cakedome::Context;
use cakedome::association::HasManyOf;
use cakedome::policy::{Action, EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, Set,
};

use crate::common::registry;
use crate::entities::{customers, master_tags, order_items, orders};
use crate::support::{Log, context, db, identity, insert_customer, insert_tag, past, raw_customer};

type Customers = DefaultRepo<customers::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;
type Items = DefaultRepo<order_items::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("items", order_items::Column::OrderId).dependent(true);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::ITEMS)
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

/// Sees only names starting with `a`, and logs the delete hooks.
struct ScopedCustomers {
    log: Log,
}

#[async_trait]
impl Repo for ScopedCustomers {
    type Entity = customers::Entity;

    async fn before_delete(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("before_delete {}", model.name));
        Ok(())
    }

    async fn after_delete(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("after_delete {}", model.name));
        Ok(())
    }
}

#[async_trait]
impl TablePolicy for ScopedCustomers {
    async fn scope(&self, _ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(Condition::all().add(customers::Column::Name.starts_with("a")))
    }
}

impl EntityPolicy for ScopedCustomers {}

fn scoped_registry(log: &Log) -> Registry {
    let mut r = registry();
    r.add(ScopedCustomers { log: log.clone() });
    r
}

async fn insert_order(db: &DatabaseConnection, customer_id: i64) -> orders::Model {
    orders::ActiveModel {
        customer_id: Set(customer_id),
        title: Set("o".to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn insert_item(db: &DatabaseConnection, order_id: i64, active: bool) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set("i".to_string()),
        seq: Set(1),
        is_active: Set(active),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn purge_removes_soft_deleted_and_active_rows() {
    let db = db().await;
    let a = insert_customer(&db, "a", true).await;
    let b = insert_customer(&db, "b", true).await;
    let mut r = registry();
    r.add(Customers::new());
    let ctx = context(&db, identity(&["entity.customers.*"]), r);
    let repo = ctx.repo::<Customers>();
    repo.delete(a.id).await.unwrap();
    assert!(
        raw_customer(&db, a.id).await.is_some(),
        "delete only deactivates"
    );

    repo.purge(a.id).await.unwrap();
    repo.purge(b.id).await.unwrap();

    assert_eq!(raw_customer(&db, a.id).await, None);
    assert_eq!(raw_customer(&db, b.id).await, None);
}

#[tokio::test]
async fn purge_removes_a_master_row_that_delete_refuses() {
    let db = db().await;
    let tag = insert_tag(&db, "t", true).await;
    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(&["entity.master_tags.*"]), r);
    let repo = ctx.repo::<Tags>();

    let deleted = repo.delete(tag.id).await;
    repo.purge(tag.id).await.unwrap();

    assert!(
        matches!(deleted, Err(RepoError::NotDeletable)),
        "{deleted:?}"
    );
    let left = master_tags::Entity::find_by_id(tag.id)
        .one(&db)
        .await
        .unwrap();
    assert_eq!(left, None);
}

#[tokio::test]
async fn purge_needs_delete_permission_applies_the_scope_and_runs_delete_hooks() {
    let db = db().await;
    let inside = insert_customer(&db, "a_in", true).await;
    let outside = insert_customer(&db, "b_out", true).await;
    let log = Log::default();
    let viewer = context(
        &db,
        identity(&["entity.customers.view"]),
        scoped_registry(&log),
    );
    let deleter = context(
        &db,
        identity(&["entity.customers.*"]),
        scoped_registry(&log),
    );

    let forbidden = viewer.repo::<ScopedCustomers>().purge(inside.id).await;
    let hidden = deleter.repo::<ScopedCustomers>().purge(outside.id).await;
    deleter
        .repo::<ScopedCustomers>()
        .purge(inside.id)
        .await
        .unwrap();

    assert!(
        matches!(forbidden, Err(RepoError::Forbidden)),
        "{forbidden:?}"
    );
    assert!(matches!(hidden, Err(RepoError::NotFound)), "{hidden:?}");
    assert_eq!(log.take(), ["before_delete a_in", "after_delete a_in"]);
    assert_eq!(raw_customer(&db, inside.id).await, None);
    assert!(raw_customer(&db, outside.id).await.is_some());
}

#[tokio::test]
async fn unchecked_purge_skips_the_policy() {
    let db = db().await;
    let c = insert_customer(&db, "b_out", true).await;
    let log = Log::default();
    let ctx = context(&db, identity(&[]), scoped_registry(&log));

    let hidden = ctx.repo::<ScopedCustomers>().unchecked().purge(c.id).await;

    // Unchecked skips `can` and the scope, so the row outside the scope is purged.
    assert!(hidden.is_ok(), "{hidden:?}");
    assert_eq!(raw_customer(&db, c.id).await, None);
}

#[tokio::test]
async fn purge_also_purges_dependent_children_including_deactivated_ones() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let order = insert_order(&db, c.id).await;
    insert_item(&db, order.id, true).await;
    insert_item(&db, order.id, false).await;
    let mut r = registry();
    r.add(Items::new());
    r.add(OrdersRepo);
    r.verify();
    let ctx = context(
        &db,
        identity(&["entity.orders.*", "entity.order_items.*"]),
        r,
    );

    ctx.repo::<OrdersRepo>().purge(order.id).await.unwrap();

    let order_left = orders::Entity::find_by_id(order.id).one(&db).await.unwrap();
    let items_left = order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order.id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(order_left, None);
    assert_eq!(items_left, 0);
}
