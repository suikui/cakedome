//! Body conversion and save (`cakedome::patch`) called directly, without an HTTP layer.

use cakedome::Context;
use cakedome::association::{BelongsToOf, HasManyOf};
use cakedome::key::Key;
use cakedome::patch::{PatchError, PatchOptions, Patching};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, GuardedRepo, Repo, RepoError};
use sea_orm::prelude::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{configs, customers, logs, measures, order_items, orders, prices};
use crate::support::{context, db, identity, insert_customer, past};

type Customers = DefaultRepo<customers::Entity>;
type Items = DefaultRepo<order_items::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("items", order_items::Column::OrderId).save(SaveStrategy::Replace);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .default_contain(["items"])
    }
}

impl cakedome::policy::TablePolicy for OrdersRepo {}
impl cakedome::policy::EntityPolicy for OrdersRepo {}

fn orders_registry() -> Registry {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Items::new());
    r.add(OrdersRepo);
    r.verify();
    r
}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.order_items.*",
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

async fn items_of(db: &DatabaseConnection, order_id: i64) -> Vec<(String, i32)> {
    order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order_id))
        .order_by_asc(order_items::Column::Id)
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.name, i.seq))
        .collect()
}

#[tokio::test]
async fn patch_and_save_patch_write_the_parent_and_its_children() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let repo = ctx.repo::<OrdersRepo>();

    let p = repo
        .patch(
            None,
            &json!({"data": {"customer_id": c.id, "title": "o",
                             "items": [{"name": "A"}, {"name": "B"}]}}),
        )
        .await
        .unwrap();
    let saved = repo.save_patch(p).await.unwrap();

    assert_eq!(saved.title, "o");
    assert_eq!(
        items_of(&db, saved.id).await,
        [("A".to_string(), 1), ("B".to_string(), 2)]
    );
}

#[tokio::test]
async fn patch_returns_first_stage_errors_of_child_rows_at_their_place() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let res = ctx
        .repo::<OrdersRepo>()
        .patch(
            None,
            &json!({"data": {"customer_id": c.id, "title": "o",
                             "items": [{"name": "A"}, {}]}}),
        )
        .await;

    match res {
        Err(PatchError::Invalid(errors)) => {
            let codes: Vec<&str> = errors.0["items.1.name"].iter().map(|e| e.code).collect();
            assert_eq!(codes, ["required"], "{errors:?}");
        }
        other => panic!("expected Invalid: {:?}", other.err()),
    }
}

#[tokio::test]
async fn patch_rejects_a_body_whose_data_is_not_an_object() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let res = ctx
        .repo::<OrdersRepo>()
        .patch(None, &json!({"data": 1}))
        .await;

    assert!(
        matches!(res, Err(PatchError::BadRequest(_))),
        "{:?}",
        res.err()
    );
}

#[tokio::test]
async fn save_patch_returns_the_current_row_with_default_contain_on_conflict() {
    let db = db().await;
    let c = insert_customer(&db, "c", true).await;
    let o = insert_order(&db, c.id, "now").await;
    let ctx = context(&db, identity(ALL), orders_registry());
    order_items::ActiveModel {
        order_id: Set(o.id),
        name: Set("A".to_string()),
        seq: Set(1),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let repo = ctx.repo::<OrdersRepo>();

    let stale = past() - chrono::Duration::seconds(1);
    let p = repo
        .patch(
            Some(o.id),
            &json!({"data": {"title": "x"}, "modified": stale.to_rfc3339()}),
        )
        .await
        .unwrap();
    let res = repo.save_patch(p).await;

    match res {
        Err(PatchError::Conflict { current }) => {
            assert_eq!(current["title"], "now");
            assert_eq!(
                current["items"][0]["name"], "A",
                "default_contain relations are attached too"
            );
        }
        other => panic!("expected Conflict: {:?}", other.err()),
    }
}

#[tokio::test]
async fn patch_with_an_unregistered_association_is_a_code_error() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let res = ctx
        .repo::<OrdersRepo>()
        .patch_with(
            None,
            &json!({"data": {"title": "o"}}),
            PatchOptions::default().associated(["nothing"]),
        )
        .await;

    assert!(
        matches!(res, Err(PatchError::Repo(RepoError::Db(_)))),
        "{:?}",
        res.err()
    );
}

#[tokio::test]
async fn decimal_is_saved_and_read_back() {
    type Prices = DefaultRepo<prices::Entity>;
    let db = db().await;
    let mut r = registry();
    r.add(Prices::new());
    let ctx = context(&db, identity(&["entity.prices.*"]), r);
    let repo = ctx.repo::<Prices>();

    let saved = repo
        .save_patch(
            repo.patch(
                None,
                &json!({"data": {"amount": "1234.5", "discount": "0.10"}}),
            )
            .await
            .unwrap(),
        )
        .await
        .unwrap();
    let read = repo.get(saved.id).await.unwrap();
    let out = repo.to_json_one(&read.clone().into()).await.unwrap();

    assert_eq!(read.amount, Decimal::new(12345, 1));
    assert_eq!(read.discount, Some(Decimal::new(10, 2)));
    assert_eq!(
        out["amount"], "1234.50",
        "normalizes digits even when read from f64 on SQLite"
    );
    assert_eq!(out["discount"], "0.10");
}

#[tokio::test]
async fn double_is_saved_and_read_back() {
    type Measures = DefaultRepo<measures::Entity>;
    let db = db().await;
    let mut r = registry();
    r.add(Measures::new());
    let ctx = context(&db, identity(&["entity.measures.*"]), r);
    let repo = ctx.repo::<Measures>();

    let saved = repo
        .save_patch(
            repo.patch(None, &json!({"data": {"ratio": 1.25, "weight": null}}))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let read = repo.get(saved.id).await.unwrap();
    let out = repo.to_json_one(&read.clone().into()).await.unwrap();

    assert_eq!(read.ratio, 1.25);
    assert_eq!(out["ratio"], 1.25);
    assert_eq!(out["weight"], Value::Null);
}

#[tokio::test]
async fn json_is_saved_and_read_back() {
    type Configs = DefaultRepo<configs::Entity>;
    let db = db().await;
    let mut r = registry();
    r.add(Configs::new());
    let ctx = context(&db, identity(&["entity.configs.*"]), r);
    let repo = ctx.repo::<Configs>();
    let data = json!({"tags": ["a", "b"], "limit": 3, "nested": {"on": true}});

    let saved = repo
        .save_patch(
            repo.patch(None, &json!({"data": {"data": data, "extra": null}}))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let read = repo.get(saved.id).await.unwrap();
    let out = repo.to_json_one(&read.clone().into()).await.unwrap();

    assert_eq!(read.data, data);
    assert_eq!(read.extra, None);
    assert_eq!(out["data"], data);
    assert_eq!(out["extra"], Value::Null);
}

#[tokio::test]
async fn messages_on_the_registry_replace_validation_and_warning_texts() {
    type Customers = DefaultRepo<customers::Entity>;
    let db = db().await;
    let mut r = registry();
    r.add(Customers::new())
        .warn_duplicate([customers::Column::Name]);
    r.set_messages(&[
        ("required", "必須です"),
        ("max_length", "{max}文字までです"),
        ("warning.duplicate", "{label}が重なっています"),
    ]);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);
    let repo = ctx.repo::<Customers>();

    match repo.patch(None, &json!({"data": {"nothing": 1}})).await {
        Err(PatchError::Invalid(errors)) => {
            assert_eq!(errors.0["name"][0].message, "必須です");
            assert_eq!(
                errors.0["nothing"][0].message, "This field does not exist",
                "keys missing from the table use the default message"
            );
        }
        other => panic!("expected Invalid: {:?}", other.err()),
    }
    match repo
        .patch(None, &json!({"data": {"name": "x".repeat(51)}}))
        .await
    {
        Err(PatchError::Invalid(errors)) => {
            assert_eq!(errors.0["name"][0].message, "50文字までです");
        }
        other => panic!("expected Invalid: {:?}", other.err()),
    }
    insert_customer(&db, "dup", true).await;
    let p = repo
        .patch(None, &json!({"data": {"name": "dup"}}))
        .await
        .unwrap();
    match repo.save_patch(p).await {
        Err(PatchError::Warnings(w)) => {
            assert_eq!(w[0].message, "nameが重なっています");
            assert_eq!(w[0].code, "duplicate");
            assert_eq!(
                serde_json::to_value(&w[0]).unwrap(),
                json!({"key": "name.duplicate", "field": "name", "code": "duplicate",
                       "params": {"label": "name"}, "message": "nameが重なっています"}),
            );
        }
        other => panic!("expected Warnings: {:?}", other.err()),
    }
}

/// Customers whose saves from request data skip the optimistic lock.
struct UnlockedCustomers;

impl Repo for UnlockedCustomers {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.optimistic_lock(false)
    }
}

impl cakedome::policy::TablePolicy for UnlockedCustomers {}
impl cakedome::policy::EntityPolicy for UnlockedCustomers {}

type Logs = DefaultRepo<logs::Entity>;

async fn patch_and_save<R: GuardedRepo>(
    ctx: &Context,
    id: Key<R>,
    body: Value,
) -> Result<<R::Entity as EntityTrait>::Model, PatchError> {
    let repo = ctx.repo::<R>();
    let patch = repo.patch(Some(id), &body).await?;
    repo.save_patch(patch).await
}

#[tokio::test]
async fn an_update_without_modified_is_a_bad_request_while_the_lock_is_on() {
    let db = db().await;
    let c = insert_customer(&db, "a", true).await;
    let mut r = registry();
    r.add(Customers::new());
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let saved = patch_and_save::<Customers>(&ctx, c.id, json!({"data": {"name": "b"}})).await;

    assert!(
        matches!(saved, Err(PatchError::BadRequest(_))),
        "{:?}",
        saved.err()
    );
}

#[tokio::test]
async fn optimistic_lock_false_updates_without_modified_and_ignores_a_stale_one() {
    let db = db().await;
    let c = insert_customer(&db, "a", true).await;
    let mut r = registry();
    r.add(UnlockedCustomers);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let first = patch_and_save::<UnlockedCustomers>(&ctx, c.id, json!({"data": {"name": "b"}}))
        .await
        .unwrap();
    let stale = (past() - chrono::Duration::days(1)).to_rfc3339();
    let second = patch_and_save::<UnlockedCustomers>(
        &ctx,
        c.id,
        json!({"data": {"name": "c"}, "modified": stale}),
    )
    .await
    .unwrap();

    assert_eq!(first.name, "b");
    assert_eq!(second.name, "c");
}

#[tokio::test]
async fn a_table_without_a_modified_column_is_updated_without_the_lock() {
    let db = db().await;
    let log = logs::ActiveModel {
        message: Set("a".to_string()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let mut r = registry();
    r.add(Logs::new());
    let ctx = context(&db, identity(&["entity.logs.*"]), r);

    let saved = patch_and_save::<Logs>(&ctx, log.id, json!({"data": {"message": "b"}}))
        .await
        .unwrap();

    assert_eq!(saved.message, "b");
}

#[test]
fn the_lock_setting_is_on_the_entity_meta() {
    let mut locked = registry();
    locked.add(Customers::new());
    let mut unlocked = registry();
    unlocked.add(UnlockedCustomers);

    assert!(locked.meta("customers").unwrap().optimistic_lock);
    assert!(!unlocked.meta("customers").unwrap().optimistic_lock);
}
