//! Many-to-many through a join table without an order column.

use cakedome::association::ManyToManyOf;
use cakedome::patch::Patching;
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, Repo};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set,
};
use serde_json::json;

use crate::common::registry;
use crate::entities::{customers, master_tags, order_tag_links, orders};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Customers = DefaultRepo<customers::Entity>;
type Links = DefaultRepo<order_tag_links::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const TAGS: ManyToManyOf<Self, Links, Tags> = ManyToManyOf::new(
        "tags",
        order_tag_links::Column::OrderId,
        order_tag_links::Column::TagId,
    )
    .save(SaveStrategy::Replace);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::TAGS)
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

fn links_registry() -> Registry {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Links::new());
    r.add(Tags::new());
    r.add(OrdersRepo);
    r.verify();
    r
}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.order_tag_links.*",
    "entity.master_tags.*",
    "entity.customers.*",
];

async fn insert_order(db: &DatabaseConnection) -> orders::Model {
    let customer = insert_customer(db, "c", true).await;
    orders::ActiveModel {
        customer_id: Set(customer.id),
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

/// Tag ids linked to the order, in the join table's key order.
async fn linked_tags(db: &DatabaseConnection, order_id: i64) -> Vec<i64> {
    order_tag_links::Entity::find()
        .filter(order_tag_links::Column::OrderId.eq(order_id))
        .order_by_asc(order_tag_links::Column::Id)
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.tag_id)
        .collect()
}

#[tokio::test]
async fn replace_links_works_without_an_order_column() {
    let db = db().await;
    let order = insert_order(&db).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let ctx = context(&db, identity(ALL), links_registry());
    let repo = ctx.repo::<OrdersRepo>();

    repo.save_with(order.clone().into_active_model())
        .replace_links(OrdersRepo::TAGS, vec![b.id, a.id])
        .save()
        .await
        .unwrap();
    assert_eq!(linked_tags(&db, order.id).await, [b.id, a.id]);

    repo.save_with(order.clone().into_active_model())
        .replace_links(OrdersRepo::TAGS, vec![a.id])
        .save()
        .await
        .unwrap();
    assert_eq!(linked_tags(&db, order.id).await, [a.id]);
}

#[tokio::test]
async fn link_and_unlink_work_without_an_order_column() {
    let db = db().await;
    let order = insert_order(&db).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let ctx = context(&db, identity(ALL), links_registry());
    let repo = ctx.repo::<OrdersRepo>();

    repo.link(OrdersRepo::TAGS, &order, vec![a.id])
        .await
        .unwrap();
    repo.link(OrdersRepo::TAGS, &order, vec![b.id, a.id])
        .await
        .unwrap();
    assert_eq!(linked_tags(&db, order.id).await, [a.id, b.id]);

    repo.unlink(OrdersRepo::TAGS, &order, vec![a.id])
        .await
        .unwrap();
    assert_eq!(linked_tags(&db, order.id).await, [b.id]);
}

#[tokio::test]
async fn targets_are_read_in_the_join_table_key_order() {
    let db = db().await;
    let order = insert_order(&db).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let ctx = context(&db, identity(ALL), links_registry());
    let repo = ctx.repo::<OrdersRepo>();
    repo.link(OrdersRepo::TAGS, &order, vec![b.id, a.id])
        .await
        .unwrap();

    let tags = repo
        .load(std::slice::from_ref(&order), OrdersRepo::TAGS)
        .await
        .unwrap();

    let codes: Vec<&str> = tags[&order.id].iter().map(|t| t.code.as_str()).collect();
    assert_eq!(codes, ["b", "a"]);
}

#[tokio::test]
async fn request_data_saves_links_without_an_order_column() {
    let db = db().await;
    let order = insert_order(&db).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let ctx = context(&db, identity(ALL), links_registry());
    let repo = ctx.repo::<OrdersRepo>();

    let body = json!({
        "data": {"tags": [a.id, b.id]},
        "modified": order.modified.to_rfc3339(),
        "associated": ["tags"],
    });
    let patch = repo.patch(Some(order.id), &body).await.unwrap();
    repo.save_patch(patch).await.unwrap();

    assert_eq!(linked_tags(&db, order.id).await, [a.id, b.id]);
}
