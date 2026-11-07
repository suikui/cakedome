//! Reading relations (contain, load), save_with, link/unlink, and deleting children with a parent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::association::{
    Association, AssociationType, BelongsToOf, HasManyOf, HasOneOf, ManyToManyOf,
};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};

use crate::common::registry;
use crate::entities::{
    customer_notes, customers, master_tags, order_items, orders, setting_order_tags,
};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Customers = DefaultRepo<customers::Entity>;
type Items = DefaultRepo<order_items::Entity>;
type OrderTags = DefaultRepo<setting_order_tags::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, Items> = HasManyOf::new("items", order_items::Column::OrderId)
        .save(SaveStrategy::Replace)
        .dependent(true);
    const TAGS: ManyToManyOf<Self, OrderTags, Tags> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace)
    .dependent(true);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::TAGS)
    }
}

impl cakedome::policy::TablePolicy for OrdersRepo {}
impl cakedome::policy::EntityPolicy for OrdersRepo {}

fn orders_registry() -> Registry {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Items::new());
    r.add(OrderTags::new());
    r.add(Tags::new());
    r.add(OrdersRepo);
    r.verify();
    r
}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
    "entity.customers.*",
];

async fn insert_order(db: &DatabaseConnection, customer_id: i64, title: &str) -> orders::Model {
    orders::ActiveModel {
        customer_id: Set(customer_id),
        title: Set(title.to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn insert_item(
    db: &DatabaseConnection,
    order_id: i64,
    name: &str,
    seq: i32,
    active: bool,
) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set(name.to_string()),
        seq: Set(seq),
        is_active: Set(active),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn insert_link(db: &DatabaseConnection, order_id: i64, tag_id: i64, seq: i32) {
    setting_order_tags::ActiveModel {
        order_id: Set(order_id),
        tag_id: Set(tag_id),
        seq: Set(seq),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

fn new_item(name: &str) -> order_items::ActiveModel {
    order_items::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

async fn items_of(db: &DatabaseConnection, order_id: i64) -> Vec<(String, i32, bool)> {
    order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order_id))
        .order_by_asc(order_items::Column::Id)
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.name, i.seq, i.is_active))
        .collect()
}

async fn links_of(db: &DatabaseConnection, order_id: i64) -> Vec<(i64, i32)> {
    setting_order_tags::Entity::find()
        .filter(setting_order_tags::Column::OrderId.eq(order_id))
        .order_by_asc(setting_order_tags::Column::Seq)
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|l| (l.tag_id, l.seq))
        .collect()
}

#[tokio::test]
async fn contain_reads_each_relation_with_one_query() {
    let db = db().await;
    let c1 = insert_customer(&db, "c1", true).await;
    let c2 = insert_customer(&db, "c2", true).await;
    let o1 = insert_order(&db, c1.id, "o1").await;
    insert_order(&db, c2.id, "o2").await;
    let o3 = insert_order(&db, c1.id, "o3").await;
    insert_item(&db, o1.id, "o1_a", 1, true).await;
    insert_item(&db, o1.id, "o1_gone", 2, false).await;
    insert_item(&db, o3.id, "o3_a", 1, true).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    insert_link(&db, o1.id, t2.id, 1).await;
    insert_link(&db, o1.id, t1.id, 2).await;

    let statements = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut counted = db.clone();
    let sink = statements.clone();
    counted.set_metric_callback(move |info| {
        sink.lock().unwrap().push(info.statement.sql.clone());
    });
    let ctx = context(&counted, identity(ALL), orders_registry());

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::CUSTOMER)
        .contain(OrdersRepo::ITEMS)
        .contain(OrdersRepo::TAGS)
        .all()
        .await
        .unwrap();

    let statements = statements.lock().unwrap().clone();
    assert_eq!(
        statements.len(),
        5,
        "orders, customers, lines, junction and tags once each: {statements:#?}"
    );
    let customer = |i: usize| {
        rows[i]
            .related(OrdersRepo::CUSTOMER)
            .unwrap()
            .map(|c| c.name.clone())
    };
    let items = |i: usize| -> Vec<String> {
        rows[i]
            .related(OrdersRepo::ITEMS)
            .unwrap()
            .iter()
            .map(|it| it.name.clone())
            .collect()
    };
    let tags: Vec<String> = rows[0]
        .related(OrdersRepo::TAGS)
        .unwrap()
        .iter()
        .map(|t| t.code.clone())
        .collect();
    assert_eq!(customer(0).as_deref(), Some("c1"));
    assert_eq!(customer(1).as_deref(), Some("c2"));
    assert_eq!(items(0), ["o1_a"], "soft-deleted lines are not read");
    assert!(items(1).is_empty());
    assert_eq!(items(2), ["o3_a"]);
    assert_eq!(tags, ["t2", "t1"], "junction table order");
}

#[test]
fn association_type_tells_how_each_relation_connects() {
    const LATEST: HasOneOf<OrdersRepo, Items> =
        HasOneOf::new("latest", order_items::Column::OrderId);

    assert_eq!(
        OrdersRepo::CUSTOMER.association_type(),
        AssociationType::ManyToOne
    );
    assert_eq!(
        OrdersRepo::ITEMS.association_type(),
        AssociationType::OneToMany
    );
    assert_eq!(LATEST.association_type(), AssociationType::OneToOne);
    assert_eq!(
        OrdersRepo::TAGS.association_type(),
        AssociationType::ManyToMany
    );
}

async fn order_ids(ctx: &Context, condition: Condition) -> Vec<i64> {
    ctx.repo::<OrdersRepo>()
        .find()
        .filter(condition)
        .order_by_asc(orders::Column::Id)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.id)
        .collect()
}

#[tokio::test]
async fn matching_condition_selects_parents_by_conditions_on_related_rows() {
    let db = db().await;
    let c1 = insert_customer(&db, "c1", true).await;
    let c2 = insert_customer(&db, "c2", true).await;
    let gone = insert_customer(&db, "gone", false).await;
    let o1 = insert_order(&db, c1.id, "o1").await;
    let o2 = insert_order(&db, c2.id, "o2").await;
    insert_order(&db, gone.id, "o3").await;
    insert_item(&db, o1.id, "A", 1, true).await;
    insert_item(&db, o2.id, "A", 1, false).await;
    let t1 = insert_tag(&db, "t1", true).await;
    insert_link(&db, o2.id, t1.id, 1).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let by_customer = OrdersRepo::CUSTOMER
        .matching_condition(
            &ctx,
            Condition::all().add(customers::Column::Name.is_in(["c1", "gone"])),
        )
        .await
        .unwrap();
    let by_item = OrdersRepo::ITEMS
        .matching_condition(
            &ctx,
            Condition::all().add(order_items::Column::Name.eq("A")),
        )
        .await
        .unwrap();
    let by_tag = OrdersRepo::TAGS
        .matching_condition(
            &ctx,
            Condition::all().add(master_tags::Column::Code.eq("t1")),
        )
        .await
        .unwrap();

    assert_eq!(
        order_ids(&ctx, by_customer).await,
        [o1.id],
        "soft-deleted customer does not match"
    );
    assert_eq!(
        order_ids(&ctx, by_item).await,
        [o1.id],
        "soft-deleted line does not match"
    );
    assert_eq!(order_ids(&ctx, by_tag).await, [o2.id]);
}

#[tokio::test]
async fn load_groups_children_by_parent() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o1 = insert_order(&db, c.id, "o1").await;
    let o2 = insert_order(&db, c.id, "o2").await;
    insert_item(&db, o1.id, "b", 2, true).await;
    insert_item(&db, o1.id, "a", 1, true).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let repo = ctx.repo::<OrdersRepo>();

    let orders = repo.find().all().await.unwrap();
    let items: HashMap<i64, Vec<order_items::Model>> =
        repo.load(&orders, OrdersRepo::ITEMS).await.unwrap();

    let names: Vec<&str> = items[&o1.id].iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["a", "b"], "default line order (seq)");
    assert!(
        items[&o2.id].is_empty(),
        "parents without children get an empty entry"
    );
}

#[tokio::test]
async fn save_with_replace_updates_adds_and_removes_children() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o = insert_order(&db, c.id, "o").await;
    insert_item(&db, o.id, "A", 1, true).await;
    let b = insert_item(&db, o.id, "B", 2, true).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let saved = ctx
        .repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace(
            OrdersRepo::ITEMS,
            vec![
                order_items::ActiveModel {
                    id: Unchanged(b.id),
                    name: Set("B2".to_string()),
                    ..Default::default()
                },
                new_item("C"),
            ],
        )
        .save()
        .await
        .unwrap();

    assert_eq!(
        items_of(&db, o.id).await,
        [
            ("A".to_string(), 1, false),
            ("B2".to_string(), 1, true),
            ("C".to_string(), 2, true),
        ],
        "omitted A is deleted, B updated and re-sequenced, C added"
    );
    assert!(
        saved.modified > past(),
        "parent modified advances when children change"
    );
}

#[tokio::test]
async fn save_with_writes_nothing_when_a_link_target_is_invalid() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            customer_id: Set(c.id),
            title: Set("bad".to_string()),
            ..Default::default()
        })
        .replace_links(OrdersRepo::TAGS, vec![i64::MAX])
        .save()
        .await;

    match res {
        Err(RepoError::Validation(errors)) => {
            let codes: Vec<&str> = errors.0["tags.0"].iter().map(|e| e.code).collect();
            assert_eq!(codes, ["invalid_ref"]);
        }
        other => panic!("expected Validation: {other:?}"),
    }
    assert!(
        orders::Entity::find().all(&db).await.unwrap().is_empty(),
        "parent is rolled back too"
    );
}

#[tokio::test]
async fn replace_links_sets_targets_in_the_given_order() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o = insert_order(&db, c.id, "o").await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    insert_link(&db, o.id, t1.id, 1).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    ctx.repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace_links(OrdersRepo::TAGS, vec![t3.id, t2.id])
        .save()
        .await
        .unwrap();

    assert_eq!(links_of(&db, o.id).await, [(t3.id, 1), (t2.id, 2)]);
}

#[tokio::test]
async fn link_and_unlink_add_and_remove_pairs_and_children() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o = insert_order(&db, c.id, "o").await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let repo = ctx.repo::<OrdersRepo>();

    repo.link(OrdersRepo::TAGS, &o, vec![t1.id, t2.id])
        .await
        .unwrap();
    repo.unlink(OrdersRepo::TAGS, &o, vec![t1.id])
        .await
        .unwrap();
    repo.link(OrdersRepo::ITEMS, &o, vec![new_item("X")])
        .await
        .unwrap();

    let tags: Vec<i64> = links_of(&db, o.id).await.into_iter().map(|l| l.0).collect();
    assert_eq!(tags, [t2.id]);
    let items: Vec<String> = items_of(&db, o.id).await.into_iter().map(|i| i.0).collect();
    assert_eq!(items, ["X"]);
}

#[tokio::test]
async fn deleting_the_parent_removes_dependent_children_and_pairs() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o = insert_order(&db, c.id, "o").await;
    insert_item(&db, o.id, "A", 1, true).await;
    let t = insert_tag(&db, "t", true).await;
    insert_link(&db, o.id, t.id, 1).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    ctx.repo::<OrdersRepo>().delete(o.id).await.unwrap();

    let order = orders::Entity::find_by_id(o.id).one(&db).await.unwrap();
    assert!(!order.expect("row remains (soft delete)").is_active);
    assert_eq!(items_of(&db, o.id).await, [("A".to_string(), 1, false)]);
    assert!(
        links_of(&db, o.id).await.is_empty(),
        "pairs are deleted, the tags remain"
    );
    assert!(
        master_tags::Entity::find_by_id(t.id)
            .one(&db)
            .await
            .unwrap()
            .is_some()
    );
}

type Notes = DefaultRepo<customer_notes::Entity>;

struct CustomersWithNotes;

impl CustomersWithNotes {
    const NOTES: HasManyOf<Self, Notes> =
        HasManyOf::new("notes", customer_notes::Column::CustomerId)
            .save(SaveStrategy::Replace)
            .cascade_callbacks(false);
}

impl Repo for CustomersWithNotes {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::NOTES)
    }
}

impl cakedome::policy::TablePolicy for CustomersWithNotes {}
impl cakedome::policy::EntityPolicy for CustomersWithNotes {}

async fn insert_note(
    db: &DatabaseConnection,
    customer_id: i64,
    body: &str,
) -> customer_notes::Model {
    customer_notes::ActiveModel {
        customer_id: Set(Some(customer_id)),
        body: Set(body.to_string()),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn raw_note(db: &DatabaseConnection, id: i64) -> customer_notes::Model {
    customer_notes::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn replace_without_cascade_callbacks_sets_null_on_removed_children() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let kept = insert_note(&db, c.id, "kept").await;
    let removed = insert_note(&db, c.id, "removed").await;
    let mut r = registry();
    r.add(Notes::new());
    r.add(CustomersWithNotes);
    r.verify();
    let ctx = context(
        &db,
        identity(&["entity.customers.*", "entity.customer_notes.*"]),
        r,
    );

    ctx.repo::<CustomersWithNotes>()
        .save_with(customers::ActiveModel {
            id: Unchanged(c.id),
            ..Default::default()
        })
        .replace(
            CustomersWithNotes::NOTES,
            vec![customer_notes::ActiveModel {
                id: Unchanged(kept.id),
                ..Default::default()
            }],
        )
        .save()
        .await
        .unwrap();

    let removed = raw_note(&db, removed.id).await;
    assert_eq!(
        removed.customer_id, None,
        "row remains: nullable and not dependent"
    );
    assert!(
        removed.modified > past(),
        "modified advances even with a single UPDATE"
    );
    assert_eq!(raw_note(&db, kept.id).await.customer_id, Some(c.id));
}
