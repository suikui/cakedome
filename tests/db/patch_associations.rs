//! Saving associations from a JSON body: `patch` and `save_patch` with hasMany, belongsTo, hasOne,
//! nested rows and the `associated` list.

use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::association::{
    BelongsToOf, FinderFuture, HasManyOf, HasOneOf, LoadQuery, ManyToManyOf,
};
use cakedome::defaults::DynamicDefault;
use cakedome::key::Key;
use cakedome::marshal::{FieldErrors, MarshalOptions, WriteMode};
use cakedome::patch::{PatchError, PatchOptions, Patching};
use cakedome::policy::{Action, ColumnAccess, EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, GuardedRepo, Repo, RepoError};
use cakedome::validate::ValidationInput;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::ActiveValue::Set;
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder,
};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{
    customer_billings, customers, master_tags, order_items, orders, setting_order_tags,
};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Tags = DefaultRepo<master_tags::Entity>;
type OrderTags = DefaultRepo<setting_order_tags::Entity>;

fn is_ng(value: &sea_orm::ActiveValue<String>) -> bool {
    matches!(value, Set(v) if v == "ng")
}

struct ItemsRepo;

impl ItemsRepo {
    const TAG: BelongsToOf<Self, Tags> = BelongsToOf::new("tag", order_items::Column::TagId).save();
    // Same column as `TAG` but without `save`.
    const PLAIN_TAG: BelongsToOf<Self, Tags> =
        BelongsToOf::new("plain_tag", order_items::Column::TagId);
}

impl Repo for ItemsRepo {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG).association(Self::PLAIN_TAG)
    }

    fn validate(&self, input: &ValidationInput<'_, order_items::Entity>, errors: &mut FieldErrors) {
        if is_ng(&input.after.name) {
            errors.add("name", "ng", "ng");
        }
    }
}

impl TablePolicy for ItemsRepo {}
impl EntityPolicy for ItemsRepo {}

type Calls = Arc<Mutex<Vec<(&'static str, WriteMode, Option<&'static str>)>>>;

struct BillingsRepo {
    calls: Calls,
}

#[async_trait]
impl Repo for BillingsRepo {
    type Entity = customer_billings::Entity;

    async fn before_marshal(
        &self,
        _ctx: &Context,
        _data: &mut serde_json::Map<String, Value>,
        options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        self.calls
            .lock()
            .unwrap()
            .push(("before", options.mode(), options.association()));
        Ok(())
    }

    async fn after_marshal(
        &self,
        _ctx: &Context,
        _am: &mut customer_billings::ActiveModel,
        _data: &serde_json::Map<String, Value>,
        _errors: &mut FieldErrors,
        options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        self.calls
            .lock()
            .unwrap()
            .push(("after", options.mode(), options.association()));
        Ok(())
    }

    fn validate(
        &self,
        input: &ValidationInput<'_, customer_billings::Entity>,
        errors: &mut FieldErrors,
    ) {
        if is_ng(&input.after.billing_name) {
            errors.add("billing_name", "ng", "ng");
        }
    }
}

impl TablePolicy for BillingsRepo {}
impl EntityPolicy for BillingsRepo {}

struct CustomersRepo;

impl CustomersRepo {
    const ORDERS: HasManyOf<Self, OrdersRepo> =
        HasManyOf::new("orders", orders::Column::CustomerId)
            .save(SaveStrategy::Replace)
            .dependent(true);
    const BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId)
            .save()
            .dependent(true);
    const PLAIN_BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("plain_billing", customer_billings::Column::CustomerId);
}

impl Repo for CustomersRepo {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::ORDERS)
            .association(Self::BILLING)
            .association(Self::PLAIN_BILLING)
    }

    fn validate(&self, input: &ValidationInput<'_, customers::Entity>, errors: &mut FieldErrors) {
        if is_ng(&input.after.name) {
            errors.add("name", "ng", "ng");
        }
    }
}

impl TablePolicy for CustomersRepo {}
impl EntityPolicy for CustomersRepo {}

fn not_blocked<'a>(
    _ctx: &'a Context,
    query: LoadQuery<CustomersRepo>,
) -> FinderFuture<'a, CustomersRepo> {
    Box::pin(async move { Ok(query.filter(customers::Column::Name.ne("blocked"))) })
}

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId).save();
    const ITEMS: HasManyOf<Self, ItemsRepo> = HasManyOf::new("items", order_items::Column::OrderId)
        .save(SaveStrategy::Replace)
        .dependent(true);
    const APPENDED: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("appended", order_items::Column::OrderId).save(SaveStrategy::Append);
    const AWD: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("awd", order_items::Column::OrderId).save(SaveStrategy::AppendWithDelete);
    // Rows named "blocked" are outside these associations.
    const FILTERED: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("filtered", order_items::Column::OrderId)
            .save(SaveStrategy::Replace)
            .filter(|| Condition::all().add(order_items::Column::Name.ne("blocked")));
    const FINDER_CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("finder_customer", orders::Column::CustomerId).finder(not_blocked);
    const VIEW_ONLY: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("view_only", order_items::Column::OrderId);
    const TAGS: ManyToManyOf<Self, OrderTags, Tags> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::APPENDED)
            .association(Self::AWD)
            .association(Self::FILTERED)
            .association(Self::FINDER_CUSTOMER)
            .association(Self::VIEW_ONLY)
            .association(Self::TAGS)
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.customer_billings.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
];

struct Untitled;

#[async_trait]
impl DynamicDefault for Untitled {
    async fn value(&self, _ctx: &Context) -> Result<Value, RepoError> {
        Ok(json!("無題"))
    }
}

fn context_for(db: &DatabaseConnection, patterns: &[&str]) -> (Context, Calls) {
    let calls = Calls::default();
    let mut r: Registry = registry();
    r.add(Tags::new());
    r.add(OrderTags::new());
    r.add(ItemsRepo);
    r.add(BillingsRepo {
        calls: calls.clone(),
    });
    r.add(CustomersRepo);
    r.add(OrdersRepo)
        .default_from(orders::Column::Title, Untitled);
    r.verify();
    (context(db, identity(patterns), r), calls)
}

fn all_context(db: &DatabaseConnection) -> Context {
    context_for(db, ALL).0
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn edit(modified: DateTime<Utc>, data: Value) -> Value {
    json!({"data": data, "modified": ts(modified)})
}

async fn run<R: GuardedRepo>(
    ctx: &Context,
    id: Option<Key<R>>,
    body: &Value,
) -> Result<<R::Entity as EntityTrait>::Model, PatchError> {
    let repo = ctx.repo::<R>();
    let patch = repo.patch(id, body).await?;
    repo.save_patch(patch).await
}

async fn run_with<R: GuardedRepo>(
    ctx: &Context,
    id: Option<Key<R>>,
    body: &Value,
    options: PatchOptions,
) -> Result<<R::Entity as EntityTrait>::Model, PatchError> {
    let repo = ctx.repo::<R>();
    let patch = repo.patch_with(id, body, options).await?;
    repo.save_patch(patch).await
}

fn invalid<T>(res: Result<T, PatchError>) -> Vec<(String, &'static str)> {
    match res {
        Err(PatchError::Invalid(errors)) => {
            let mut out: Vec<(String, &'static str)> = errors
                .0
                .iter()
                .flat_map(|(k, v)| v.iter().map(move |e| (k.clone(), e.code)))
                .collect();
            out.sort();
            out
        }
        other => panic!("expected Invalid: {:?}", other.err()),
    }
}

fn pairs(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    list.iter().map(|(k, c)| (k.to_string(), *c)).collect()
}

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

async fn insert_billing(
    db: &DatabaseConnection,
    customer_id: i64,
    name: &str,
) -> customer_billings::Model {
    customer_billings::ActiveModel {
        customer_id: Set(customer_id),
        billing_name: Set(name.to_string()),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
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

async fn raw_order(db: &DatabaseConnection, id: i64) -> orders::Model {
    orders::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("order row")
}

async fn raw_item(db: &DatabaseConnection, id: i64) -> order_items::Model {
    order_items::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("item row")
}

async fn raw_customer(db: &DatabaseConnection, id: i64) -> customers::Model {
    customers::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("customer row")
}

async fn customers_named(db: &DatabaseConnection, name: &str) -> Vec<customers::Model> {
    customers::Entity::find()
        .filter(customers::Column::Name.eq(name))
        .all(db)
        .await
        .unwrap()
}

async fn orders_of(db: &DatabaseConnection, customer_id: i64) -> Vec<orders::Model> {
    orders::Entity::find()
        .filter(orders::Column::CustomerId.eq(customer_id))
        .order_by_asc(orders::Column::Id)
        .all(db)
        .await
        .unwrap()
}

async fn billings_of(db: &DatabaseConnection, customer_id: i64) -> Vec<customer_billings::Model> {
    customer_billings::Entity::find()
        .filter(customer_billings::Column::CustomerId.eq(customer_id))
        .all(db)
        .await
        .unwrap()
}

async fn count<E: EntityTrait>(db: &DatabaseConnection) -> u64
where
    E::Model: Sync,
{
    E::find().count(db).await.unwrap()
}

// ---- hasMany ----

#[tokio::test]
async fn adding_a_parent_numbers_the_items_in_the_order_sent() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"customer_id": c.id, "title": "注文",
                         "items": [{"name": "一つ目"}, {"name": "二つ目"}]}}),
    )
    .await
    .unwrap();

    assert_eq!(
        items_of(&db, saved.id).await,
        [
            ("一つ目".to_string(), 1, true),
            ("二つ目".to_string(), 2, true)
        ]
    );
    let item = order_items::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(item.order_id, saved.id);
}

#[tokio::test]
async fn replace_updates_adds_removes_and_reorders() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let y = insert_item(&db, o.id, "Y", 2, true).await;
    let z = insert_item(&db, o.id, "Z", 3, true).await;
    let ctx = all_context(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"items": [{"id": z.id, "name": "Z2"}, {"name": "新規"}, {"id": x.id}]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        items_of(&db, o.id).await,
        [
            ("X".to_string(), 3, true),
            ("Y".to_string(), 2, false),
            ("Z2".to_string(), 1, true),
            ("新規".to_string(), 2, true),
        ],
        "Y is soft-deleted, the rest follow the order sent"
    );
    assert!(!raw_item(&db, y.id).await.is_active);
}

#[tokio::test]
async fn an_items_only_edit_advances_the_parent_modified_and_a_stale_one_conflicts() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": x.id, "name": "X2"}]})),
    )
    .await
    .unwrap();

    let parent = raw_order(&db, o.id).await;
    assert!(parent.modified > o.modified);
    assert_eq!(saved.modified, parent.modified);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": x.id, "name": "X3"}]})),
    )
    .await;
    assert!(matches!(res, Err(PatchError::Conflict { .. })), "{res:?}");
    assert_eq!(raw_item(&db, x.id).await.name, "X2");
}

#[tokio::test]
async fn unchanged_items_do_not_advance_the_parent_modified() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": x.id, "name": "X"}]})),
    )
    .await
    .unwrap();

    assert_eq!(raw_order(&db, o.id).await.modified, o.modified);
    assert_eq!(raw_item(&db, x.id).await.modified, past());
}

#[tokio::test]
async fn first_stage_errors_of_the_parent_and_rows_are_collected() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"customer_id": c.id, "title": 5,
                         "items": [{"name": ""}, {"name": 5}, {}]}}),
    )
    .await;

    assert_eq!(
        invalid(res),
        pairs(&[
            ("items.0.name", "required"),
            ("items.1.name", "type"),
            ("items.2.name", "required"),
            ("title", "type"),
        ])
    );
    assert_eq!(count::<orders::Entity>(&db).await, 0);
}

#[tokio::test]
async fn second_stage_row_errors_roll_back_everything() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"title": "書かれない",
                   "items": [{"id": x.id, "name": "X2"}, {"name": "ng"}, {"name": "ng"}]}),
        ),
    )
    .await;

    assert_eq!(
        invalid(res),
        pairs(&[("items.1.name", "ng"), ("items.2.name", "ng")])
    );
    let parent = raw_order(&db, o.id).await;
    assert_eq!(
        (parent.title.as_str(), parent.modified),
        ("注文", o.modified)
    );
    assert_eq!(items_of(&db, o.id).await, [("X".to_string(), 1, true)]);
}

#[tokio::test]
async fn row_shape_errors_are_found_before_writing() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let other = insert_order(&db, c.id, "別の注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .patch(
            Some(o.id),
            &edit(
                o.modified,
                json!({"items": [
                    {"id": x.id},
                    {"id": x.id},
                    {"name": "a", "seq": 5},
                    {"name": "b", "order_id": other.id},
                ]}),
            ),
        )
        .await;

    assert_eq!(
        invalid(res),
        pairs(&[
            ("items.1.id", "duplicate"),
            ("items.2.seq", "read_only"),
            ("items.3.order_id", "unknown"),
        ])
    );
}

#[tokio::test]
async fn rows_not_belonging_to_the_order_are_not_found() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let other = insert_order(&db, c.id, "別の注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let foreign = insert_item(&db, other.id, "別の注文の明細", 1, true).await;
    let gone = insert_item(&db, o.id, "削除済み", 2, false).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"items": [{"id": foreign.id, "name": "奪う"}, {"id": x.id}, {"id": gone.id}]}),
        ),
    )
    .await;

    assert_eq!(
        invalid(res),
        pairs(&[("items.0.id", "not_found"), ("items.2.id", "not_found")])
    );
    let foreign_after = raw_item(&db, foreign.id).await;
    assert_eq!(
        (foreign_after.order_id, foreign_after.name.as_str()),
        (other.id, "別の注文の明細")
    );
    assert!(!raw_item(&db, gone.id).await.is_active);
}

#[tokio::test]
async fn items_must_be_an_array_of_objects() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    for items in [json!({"name": "a"}), json!([1, 2]), json!("x")] {
        let res = run::<OrdersRepo>(
            &ctx,
            None,
            &json!({"data": {"customer_id": c.id, "title": "注文", "items": items.clone()}}),
        )
        .await;
        assert!(
            matches!(res, Err(PatchError::BadRequest(_))),
            "{items}: {res:?}"
        );
    }
    assert_eq!(count::<orders::Entity>(&db).await, 0);
}

#[tokio::test]
async fn adding_or_removing_rows_requires_the_child_permissions() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let (ctx, _) = context_for(
        &db,
        &[
            "entity.orders.*",
            "entity.order_items.view",
            "entity.order_items.edit",
            "entity.customers.view",
        ],
    );

    let add = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": x.id}, {"name": "a"}]})),
    )
    .await;
    let remove = run::<OrdersRepo>(&ctx, Some(o.id), &edit(o.modified, json!({"items": []}))).await;
    let update = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": x.id, "name": "X2"}]})),
    )
    .await;

    assert!(
        matches!(add, Err(PatchError::Repo(RepoError::Forbidden))),
        "{add:?}"
    );
    assert!(
        matches!(remove, Err(PatchError::Repo(RepoError::Forbidden))),
        "{remove:?}"
    );
    update.unwrap();
    assert_eq!(items_of(&db, o.id).await, [("X2".to_string(), 1, true)]);
}

#[tokio::test]
async fn append_adds_and_updates_without_deleting() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    insert_item(&db, o.id, "Y", 2, true).await;
    let ctx = all_context(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"appended": [{"id": x.id, "name": "X2"}, {"name": "追加", "seq": 7}]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        items_of(&db, o.id).await,
        [
            ("X2".to_string(), 1, true),
            ("Y".to_string(), 2, true),
            ("追加".to_string(), 7, true),
        ],
        "unsent rows stay and seq is an ordinary field"
    );
}

#[tokio::test]
async fn append_with_delete_removes_only_marked_rows() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let y = insert_item(&db, o.id, "Y", 2, true).await;
    insert_item(&db, o.id, "Z", 3, true).await;
    let ctx = all_context(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"awd": [
                {"id": x.id, "_delete": true},
                {"name": "追加しない", "_delete": true},
                {"id": y.id, "name": "Y2", "_delete": false},
            ]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        items_of(&db, o.id).await,
        [
            ("X".to_string(), 1, false),
            ("Y2".to_string(), 2, true),
            ("Z".to_string(), 3, true),
        ]
    );
}

#[tokio::test]
async fn a_delete_mark_is_unknown_outside_append_with_delete() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"appended": [{"id": x.id, "_delete": true}]}),
        ),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("appended.0._delete", "unknown")]));
    assert!(raw_item(&db, x.id).await.is_active);
}

#[tokio::test]
async fn an_association_without_a_save_strategy_is_read_only() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"view_only": []})),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("view_only", "read_only")]));
    assert!(raw_item(&db, x.id).await.is_active);
}

#[tokio::test]
async fn a_malformed_has_many_is_a_bad_request_before_the_permission_check() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = context_for(&db, &["entity.orders.view", "entity.order_items.*"]);

    let res = ctx
        .repo::<OrdersRepo>()
        .patch(Some(o.id), &edit(o.modified, json!({"appended": 1})))
        .await;

    assert!(
        matches!(res, Err(PatchError::BadRequest(_))),
        "{:?}",
        res.err()
    );
}

#[tokio::test]
async fn relation_input_errors_come_after_the_permission_check() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let (ctx, _) = context_for(&db, &["entity.orders.view", "entity.order_items.*"]);

    let res = ctx
        .repo::<OrdersRepo>()
        .patch(Some(o.id), &edit(o.modified, json!({"view_only": []})))
        .await;

    assert!(
        matches!(res, Err(PatchError::Repo(RepoError::Forbidden))),
        "{:?}",
        res.err()
    );
    assert!(raw_item(&db, x.id).await.is_active);
}

// ---- belongsTo ----

#[tokio::test]
async fn a_new_belongs_to_target_is_saved_before_the_parent() {
    let db = db().await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer": {"name": "新しい顧客"}}}),
    )
    .await
    .unwrap();

    let created = customers_named(&db, "新しい顧客").await;
    assert_eq!(created.len(), 1);
    assert!(created[0].is_active);
    assert_eq!(raw_order(&db, saved.id).await.customer_id, created[0].id);
}

#[tokio::test]
async fn a_belongs_to_row_with_an_id_updates_that_target() {
    let db = db().await;
    let existing = insert_customer(&db, "旧名", true).await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer": {"id": existing.id, "name": "新名"}}}),
    )
    .await
    .unwrap();

    assert_eq!(count::<customers::Entity>(&db).await, 1);
    assert_eq!(raw_customer(&db, existing.id).await.name, "新名");
    assert_eq!(raw_order(&db, saved.id).await.customer_id, existing.id);
}

#[tokio::test]
async fn the_target_row_wins_over_the_foreign_key() {
    let db = db().await;
    let other = insert_customer(&db, "別の顧客", true).await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer_id": other.id,
                         "customer": {"name": "作る顧客"}}}),
    )
    .await
    .unwrap();

    let created = customers_named(&db, "作る顧客").await;
    assert_eq!(raw_order(&db, saved.id).await.customer_id, created[0].id);
}

#[tokio::test]
async fn belongs_to_target_errors_are_prefixed_and_nothing_is_saved() {
    let db = db().await;
    let ctx = all_context(&db);
    let body = |customer: Value| json!({"data": {"title": "注文", "customer": customer}});

    let first_stage = run::<OrdersRepo>(&ctx, None, &body(json!({"name": "a".repeat(51)}))).await;
    let typed = run::<OrdersRepo>(&ctx, None, &body(json!({"name": 5}))).await;
    let second_stage = run::<OrdersRepo>(&ctx, None, &body(json!({"name": "ng"}))).await;

    assert_eq!(
        invalid(first_stage),
        pairs(&[("customer.name", "max_length")])
    );
    assert_eq!(invalid(typed), pairs(&[("customer.name", "type")]));
    assert_eq!(invalid(second_stage), pairs(&[("customer.name", "ng")]));
    assert_eq!(count::<customers::Entity>(&db).await, 0);
    assert_eq!(count::<orders::Entity>(&db).await, 0);
}

#[tokio::test]
async fn the_belongs_to_target_runs_through_its_own_policy() {
    let db = db().await;
    let (ctx, _) = context_for(&db, &["entity.customers.view", "entity.orders.*"]);

    let res = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer": {"name": "新しい顧客"}}}),
    )
    .await;

    assert!(
        matches!(res, Err(PatchError::Repo(RepoError::Forbidden))),
        "{res:?}"
    );
    assert_eq!(count::<orders::Entity>(&db).await, 0);
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn a_null_belongs_to_target_uses_the_foreign_key() {
    let db = db().await;
    let existing = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let saved = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer_id": existing.id, "customer": null}}),
    )
    .await
    .unwrap();

    assert_eq!(saved.customer_id, existing.id);
    assert_eq!(count::<customers::Entity>(&db).await, 1);
}

#[tokio::test]
async fn a_belongs_to_target_that_is_not_an_object_is_a_bad_request() {
    let db = db().await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer": 5}}),
    )
    .await;

    assert!(matches!(res, Err(PatchError::BadRequest(_))), "{res:?}");
}

#[tokio::test]
async fn a_belongs_to_without_save_rejects_target_rows() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let item = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &json!({
            "data": {"appended": [{"id": item.id, "plain_tag": {"code": "t", "name": "タグ"}}]},
            "modified": ts(o.modified),
            "associated": ["appended.plain_tag"],
        }),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("appended.0.plain_tag", "unknown")]));
    assert_eq!(count::<master_tags::Entity>(&db).await, 0);
}

#[tokio::test]
async fn associated_can_exclude_the_belongs_to_target() {
    let db = db().await;
    let existing = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let saved = run_with::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "注文", "customer_id": existing.id,
                         "customer": {"name": "作られない"}}}),
        PatchOptions::default().associated(["items"]),
    )
    .await
    .unwrap();

    assert_eq!(saved.customer_id, existing.id);
    assert!(customers_named(&db, "作られない").await.is_empty());
}

// ---- hasOne ----

#[tokio::test]
async fn a_new_parent_adds_the_has_one_target_with_the_foreign_key() {
    let db = db().await;
    let ctx = all_context(&db);

    let saved = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": {"name": "新しい顧客",
                         "billing": {"billing_name": "請求先", "address": "東京都"}}}),
    )
    .await
    .unwrap();

    let rows = billings_of(&db, saved.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].billing_name, "請求先");
    assert_eq!(rows[0].address.as_deref(), Some("東京都"));
}

#[tokio::test]
async fn a_row_without_an_id_is_merged_into_the_current_target() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let b = insert_billing(&db, c.id, "請求先").await;
    let ctx = all_context(&db);

    run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &edit(
            c.modified,
            json!({"billing": {"address": "東京都千代田区"}}),
        ),
    )
    .await
    .unwrap();

    let rows = billings_of(&db, c.id).await;
    assert_eq!(rows.len(), 1, "no second row");
    assert_eq!(rows[0].id, b.id);
    assert_eq!(rows[0].billing_name, "請求先");
    assert_eq!(rows[0].address.as_deref(), Some("東京都千代田区"));
}

#[tokio::test]
async fn a_row_with_an_id_updates_that_target_and_a_missing_target_is_added() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let b = insert_billing(&db, c.id, "請求先").await;
    let empty = insert_customer(&db, "請求先なし", true).await;
    let ctx = all_context(&db);

    run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &edit(
            c.modified,
            json!({"billing": {"id": b.id, "billing_name": "変更"}}),
        ),
    )
    .await
    .unwrap();
    run::<CustomersRepo>(
        &ctx,
        Some(empty.id),
        &edit(empty.modified, json!({"billing": {"billing_name": "追加"}})),
    )
    .await
    .unwrap();

    let rows = billings_of(&db, c.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].billing_name, "変更");
    assert_eq!(billings_of(&db, empty.id).await[0].billing_name, "追加");
}

#[tokio::test]
async fn the_id_of_another_parents_target_is_not_found() {
    let db = db().await;
    let mine = insert_customer(&db, "自分", true).await;
    let other = insert_customer(&db, "他", true).await;
    let others = insert_billing(&db, other.id, "他の請求先").await;
    let ctx = all_context(&db);

    let res = run::<CustomersRepo>(
        &ctx,
        Some(mine.id),
        &edit(
            mine.modified,
            json!({"billing": {"id": others.id, "billing_name": "乗っ取り"}}),
        ),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("billing.id", "not_found")]));
    assert_eq!(
        billings_of(&db, other.id).await[0].billing_name,
        "他の請求先"
    );
}

#[tokio::test]
async fn a_null_has_one_does_nothing_and_a_non_object_is_a_bad_request() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    insert_billing(&db, c.id, "請求先").await;
    let ctx = all_context(&db);

    run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &edit(c.modified, json!({"billing": null})),
    )
    .await
    .unwrap();
    let bad = run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &edit(c.modified, json!({"billing": [1, 2]})),
    )
    .await;

    assert_eq!(billings_of(&db, c.id).await[0].billing_name, "請求先");
    assert!(matches!(bad, Err(PatchError::BadRequest(_))), "{bad:?}");
}

#[tokio::test]
async fn has_one_target_errors_are_prefixed_and_nothing_is_saved() {
    let db = db().await;
    let ctx = all_context(&db);

    let missing = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": {"name": "新しい顧客", "billing": {"address": "1"}}}),
    )
    .await;
    let second_stage = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": {"name": "新しい顧客", "billing": {"billing_name": "ng"}}}),
    )
    .await;

    assert_eq!(
        invalid(missing),
        pairs(&[("billing.billing_name", "required")])
    );
    assert_eq!(
        invalid(second_stage),
        pairs(&[("billing.billing_name", "ng")])
    );
    assert_eq!(count::<customers::Entity>(&db).await, 0);
    assert_eq!(count::<customer_billings::Entity>(&db).await, 0);
}

#[tokio::test]
async fn has_one_target_hooks_run_once_and_a_merge_is_an_edit() {
    let db = db().await;
    let with = insert_customer(&db, "請求先あり", true).await;
    let without = insert_customer(&db, "請求先なし", true).await;
    insert_billing(&db, with.id, "請求先").await;
    let (ctx, calls) = context_for(&db, ALL);
    let take = || std::mem::take(&mut *calls.lock().unwrap());

    run::<CustomersRepo>(
        &ctx,
        Some(with.id),
        &edit(with.modified, json!({"billing": {"address": "京都府"}})),
    )
    .await
    .unwrap();
    assert_eq!(
        take(),
        [
            ("before", WriteMode::Edit, Some("billing")),
            ("after", WriteMode::Edit, Some("billing")),
        ]
    );

    run::<CustomersRepo>(
        &ctx,
        Some(without.id),
        &edit(
            without.modified,
            json!({"billing": {"billing_name": "新しい請求先"}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        take(),
        [
            ("before", WriteMode::Add, Some("billing")),
            ("after", WriteMode::Add, Some("billing")),
        ]
    );
    assert_eq!(billings_of(&db, without.id).await.len(), 1);
}

// ---- nested rows and `associated` ----

fn customer_body(name: &str, orders: Value, associated: Value) -> Value {
    json!({"data": {"name": name, "orders": orders}, "associated": associated})
}

#[tokio::test]
async fn three_levels_are_saved_in_one_request() {
    let db = db().await;
    let ctx = all_context(&db);

    let saved = run::<CustomersRepo>(
        &ctx,
        None,
        &customer_body(
            "新しい顧客",
            json!([
                {"title": "o1", "items": [{"name": "一つ目"}, {"name": "二つ目"}]},
                {"title": "o2", "items": [{"name": "三つ目"}]},
            ]),
            json!(["orders.items"]),
        ),
    )
    .await
    .unwrap();

    let orders = orders_of(&db, saved.id).await;
    let titles: Vec<&str> = orders.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(titles, ["o1", "o2"]);
    assert_eq!(
        items_of(&db, orders[0].id).await,
        [
            ("一つ目".to_string(), 1, true),
            ("二つ目".to_string(), 2, true)
        ]
    );
    assert_eq!(
        items_of(&db, orders[1].id).await,
        [("三つ目".to_string(), 1, true)]
    );
}

#[tokio::test]
async fn belongs_to_inside_child_rows_saves_the_target() {
    let db = db().await;
    let ctx = all_context(&db);

    let saved = run::<CustomersRepo>(
        &ctx,
        None,
        &customer_body(
            "新しい顧客",
            json!([{"title": "o1", "items": [
                {"name": "明細", "tag": {"code": "nt", "name": "新しいタグ"}},
            ]}]),
            json!(["orders.items.tag"]),
        ),
    )
    .await
    .unwrap();

    let tag = master_tags::Entity::find().one(&db).await.unwrap().unwrap();
    assert_eq!(tag.code, "nt");
    let order = orders_of(&db, saved.id).await.remove(0);
    let item = order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order.id))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.tag_id, Some(tag.id));
}

#[tokio::test]
async fn nested_error_keys_carry_the_path_and_a_second_stage_error_writes_nothing() {
    let db = db().await;
    let ctx = all_context(&db);
    let body = |items: Value, associated: Value| {
        customer_body(
            "新しい顧客",
            json!([{"title": "o1", "items": [{"name": "よい"}]}, {"title": "o2", "items": items}]),
            associated,
        )
    };

    let first_stage = run::<CustomersRepo>(
        &ctx,
        None,
        &body(
            json!([{"name": "よい"}, {"name": 5}]),
            json!(["orders.items"]),
        ),
    )
    .await;
    let second_stage = run::<CustomersRepo>(
        &ctx,
        None,
        &body(json!([{"name": "ng"}]), json!(["orders.items"])),
    )
    .await;
    let target = run::<CustomersRepo>(
        &ctx,
        None,
        &body(
            json!([{"name": "x", "tag": {"code": "t"}}]),
            json!(["orders.items.tag"]),
        ),
    )
    .await;

    assert_eq!(
        invalid(first_stage),
        pairs(&[("orders.1.items.1.name", "type")])
    );
    assert_eq!(
        invalid(second_stage),
        pairs(&[("orders.1.items.0.name", "ng")])
    );
    assert_eq!(
        invalid(target),
        pairs(&[("orders.1.items.0.tag.name", "required")])
    );
    assert_eq!(count::<customers::Entity>(&db).await, 0);
    assert_eq!(count::<orders::Entity>(&db).await, 0);
}

#[tokio::test]
async fn associated_limits_the_levels_and_the_default_is_the_first_level() {
    let db = db().await;
    let ctx = all_context(&db);
    let orders = json!([{"title": "o1", "items": [{"name": "捨てる"}]}]);

    let listed = run_with::<CustomersRepo>(
        &ctx,
        None,
        &customer_body("一覧あり", orders.clone(), json!(["orders.items"])),
        PatchOptions::default().associated(["orders"]),
    )
    .await
    .unwrap();
    let default = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": {"name": "既定", "orders": orders.clone()}}),
    )
    .await
    .unwrap();
    let path = run_with::<CustomersRepo>(
        &ctx,
        None,
        &customer_body("道のり", orders, json!([])),
        PatchOptions::default().associated(["orders.items"]),
    )
    .await
    .unwrap();

    for (saved, items) in [(&listed, 0), (&default, 0), (&path, 1)] {
        let order = orders_of(&db, saved.id).await.remove(0);
        assert_eq!(items_of(&db, order.id).await.len(), items, "{}", saved.name);
    }
}

#[tokio::test]
async fn nested_rows_under_an_existing_child_are_written() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    insert_item(&db, o.id, "古い", 1, true).await;
    let ctx = all_context(&db);

    run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &json!({"data": {"orders": [{"id": o.id, "items": [{"name": "新しい"}]}]},
                "modified": ts(c.modified), "associated": ["orders.items"]}),
    )
    .await
    .unwrap();

    assert_eq!(
        items_of(&db, o.id).await,
        [
            ("古い".to_string(), 1, false),
            ("新しい".to_string(), 1, true)
        ]
    );
}

#[tokio::test]
async fn unknown_associated_names_are_errors() {
    let db = db().await;
    let ctx = all_context(&db);
    let data = json!({"name": "顧客"});

    let from_code = run_with::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": data}),
        PatchOptions::default().associated(["orders.itemz"]),
    )
    .await;
    let top = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": data, "associated": ["nothing"]}),
    )
    .await;
    let nested = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({"data": data, "associated": ["orders.itemz"]}),
    )
    .await;
    let not_an_array =
        run::<CustomersRepo>(&ctx, None, &json!({"data": data, "associated": "orders"})).await;
    let not_strings =
        run::<CustomersRepo>(&ctx, None, &json!({"data": data, "associated": [1]})).await;

    assert!(
        matches!(from_code, Err(PatchError::Repo(RepoError::Db(_)))),
        "{from_code:?}"
    );
    for res in [top, nested, not_an_array, not_strings] {
        assert!(matches!(res, Err(PatchError::BadRequest(_))), "{res:?}");
    }
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn the_bodys_associated_list_selects_associations() {
    let db = db().await;
    let ctx = all_context(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let t = insert_tag(&db, "t1", true).await;
    let body = |associated: Value| {
        json!({"data": {"customer_id": c.id, "title": "注文",
                        "items": [{"name": "a"}], "tags": [t.id]},
               "associated": associated})
    };

    let items_only = run::<OrdersRepo>(&ctx, None, &body(json!(["items"])))
        .await
        .unwrap();
    let none = run::<OrdersRepo>(&ctx, None, &body(json!([])))
        .await
        .unwrap();

    assert_eq!(items_of(&db, items_only.id).await.len(), 1);
    assert_eq!(count_links(&db, items_only.id).await, 0);
    assert!(items_of(&db, none.id).await.is_empty());
}

async fn count_links(db: &DatabaseConnection, order_id: i64) -> usize {
    setting_order_tags::Entity::find()
        .filter(setting_order_tags::Column::OrderId.eq(order_id))
        .all(db)
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn the_default_accepts_every_association_and_unlisted_ones_are_ignored() {
    let db = db().await;
    let ctx = all_context(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let t = insert_tag(&db, "t1", true).await;
    let body = |items: Value| json!({"data": {"customer_id": c.id, "title": "注文", "items": items, "tags": [t.id]}});

    let all = run::<OrdersRepo>(&ctx, None, &body(json!([{"name": "a"}, {"name": "b"}])))
        .await
        .unwrap();
    let tags_only = run_with::<OrdersRepo>(
        &ctx,
        None,
        &body(json!(1)),
        PatchOptions::default().associated(["tags"]),
    )
    .await
    .unwrap();
    let none = run_with::<OrdersRepo>(
        &ctx,
        None,
        &body(json!([{"name": "a"}])),
        PatchOptions::default().associated(Vec::<&str>::new()),
    )
    .await
    .unwrap();
    let listed_bad_shape = run_with::<OrdersRepo>(
        &ctx,
        None,
        &body(json!(1)),
        PatchOptions::default().associated(["items"]),
    )
    .await;
    let unknown = run_with::<OrdersRepo>(
        &ctx,
        None,
        &body(json!([])),
        PatchOptions::default().associated(["nothing"]),
    )
    .await;

    assert_eq!(items_of(&db, all.id).await.len(), 2);
    assert_eq!(count_links(&db, all.id).await, 1);
    assert!(
        items_of(&db, tags_only.id).await.is_empty(),
        "the shape of an ignored key is not checked"
    );
    assert_eq!(count_links(&db, tags_only.id).await, 1);
    assert!(items_of(&db, none.id).await.is_empty());
    assert_eq!(count_links(&db, none.id).await, 0);
    assert!(
        matches!(listed_bad_shape, Err(PatchError::BadRequest(_))),
        "{listed_bad_shape:?}"
    );
    assert!(
        matches!(unknown, Err(PatchError::Repo(RepoError::Db(_)))),
        "{unknown:?}"
    );
}

// ---- input and body errors ----

#[tokio::test]
async fn a_row_id_that_is_not_a_key_and_a_delete_mark_that_is_not_a_boolean_are_type_errors() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let x = insert_item(&db, o.id, "X", 1, true).await;
    let ctx = all_context(&db);

    let bad_id = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"id": "x", "name": "a"}]})),
    )
    .await;
    let bad_mark = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"awd": [{"id": x.id, "_delete": "yes"}]})),
    )
    .await;

    assert_eq!(invalid(bad_id), pairs(&[("items.0.id", "type")]));
    assert_eq!(invalid(bad_mark), pairs(&[("awd.0._delete", "type")]));
    assert!(raw_item(&db, x.id).await.is_active);
}

#[tokio::test]
async fn accept_warnings_must_be_an_array_of_strings() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);
    let body = |accept: Value| json!({"data": {"customer_id": c.id, "title": "注文"}, "acceptWarnings": accept});

    let bad = run::<OrdersRepo>(&ctx, None, &body(json!("x"))).await;
    let bad_item = run::<OrdersRepo>(&ctx, None, &body(json!([1]))).await;
    let ok = run::<OrdersRepo>(&ctx, None, &body(json!(["a.b"]))).await;

    assert!(matches!(bad, Err(PatchError::BadRequest(_))), "{bad:?}");
    assert!(
        matches!(bad_item, Err(PatchError::BadRequest(_))),
        "{bad_item:?}"
    );
    ok.unwrap();
    assert_eq!(count::<orders::Entity>(&db).await, 1);
}

#[tokio::test]
async fn nesting_deeper_than_the_limit_is_a_bad_request() {
    let db = db().await;
    let ctx = all_context(&db);

    let res = run::<CustomersRepo>(
        &ctx,
        None,
        &json!({
            "data": {"name": "A", "orders": [{"title": "o", "customer": {"name": "B",
                "orders": [{"title": "p", "customer": {"name": "C"}}]}}]},
            "associated": ["orders.customer.orders.customer"],
        }),
    )
    .await;

    assert!(matches!(res, Err(PatchError::BadRequest(_))), "{res:?}");
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn a_shape_error_inside_a_nested_row_is_a_bad_request() {
    let db = db().await;
    let ctx = all_context(&db);

    let in_child = run::<CustomersRepo>(
        &ctx,
        None,
        &customer_body(
            "顧客",
            json!([{"title": "o", "items": 5}]),
            json!(["orders.items"]),
        ),
    )
    .await;
    let in_target = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"title": "o", "customer": {"name": "x", "orders": 5}},
                "associated": ["customer.orders"]}),
    )
    .await;

    assert!(
        matches!(in_child, Err(PatchError::BadRequest(_))),
        "{in_child:?}"
    );
    assert!(
        matches!(in_target, Err(PatchError::BadRequest(_))),
        "{in_target:?}"
    );
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn an_association_the_caller_cannot_view_is_an_unknown_key() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = context_for(&db, &["entity.orders.*", "entity.customers.view"]);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"items": [{"name": "a"}]})),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("items", "unknown")]));
    assert_eq!(count::<order_items::Entity>(&db).await, 0);
}

#[tokio::test]
async fn a_has_one_without_save_is_read_only() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let res = run::<CustomersRepo>(
        &ctx,
        Some(c.id),
        &edit(c.modified, json!({"plain_billing": {"billing_name": "x"}})),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("plain_billing", "read_only")]));
    assert_eq!(count::<customer_billings::Entity>(&db).await, 0);
}

#[tokio::test]
async fn a_has_one_for_a_missing_parent_is_not_found() {
    let db = db().await;
    let ctx = all_context(&db);

    let res = run::<CustomersRepo>(
        &ctx,
        Some(9_999_999),
        &edit(past(), json!({"billing": {"billing_name": "x"}})),
    )
    .await;

    assert!(
        matches!(res, Err(PatchError::Repo(RepoError::NotFound))),
        "{res:?}"
    );
    assert_eq!(count::<customer_billings::Entity>(&db).await, 0);
}

#[tokio::test]
async fn an_association_condition_decides_which_children_are_current() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let a1 = insert_item(&db, o.id, "a1", 1, true).await;
    let blocked = insert_item(&db, o.id, "blocked", 2, true).await;
    let ctx = all_context(&db);

    let foreign = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"filtered": [{"id": blocked.id}]})),
    )
    .await;
    run::<OrdersRepo>(&ctx, Some(o.id), &edit(o.modified, json!({"filtered": []})))
        .await
        .unwrap();

    assert_eq!(invalid(foreign), pairs(&[("filtered.0.id", "not_found")]));
    assert!(
        !raw_item(&db, a1.id).await.is_active,
        "a current child that is not sent is deleted"
    );
    assert!(
        raw_item(&db, blocked.id).await.is_active,
        "a row outside the condition is left alone"
    );
}

#[tokio::test]
async fn the_foreign_key_must_point_at_a_row_the_caller_can_choose() {
    let db = db().await;
    let fine = insert_customer(&db, "顧客", true).await;
    let blocked = insert_customer(&db, "blocked", true).await;
    let ctx = all_context(&db);
    let (no_view, _) = context_for(&db, &["entity.orders.*"]);
    let body = |customer_id: i64| json!({"data": {"title": "注文", "customer_id": customer_id}});

    let by_finder = run::<OrdersRepo>(&ctx, None, &body(blocked.id)).await;
    let without_view = run::<OrdersRepo>(&no_view, None, &body(fine.id)).await;
    let allowed = run::<OrdersRepo>(&ctx, None, &body(fine.id)).await;

    assert_eq!(invalid(by_finder), pairs(&[("customer_id", "invalid_ref")]));
    let mut without_view = invalid(without_view);
    without_view.dedup();
    assert_eq!(
        without_view,
        pairs(&[("customer_id", "invalid_ref")]),
        "each association on the column reports it"
    );
    allowed.unwrap();
    assert_eq!(count::<orders::Entity>(&db).await, 1);
}

#[tokio::test]
async fn patch_exposes_the_id_and_needs_a_registered_repo() {
    struct Stray;

    impl Repo for Stray {
        type Entity = crate::entities::logs::Entity;
    }

    impl TablePolicy for Stray {}
    impl EntityPolicy for Stray {}

    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let ctx = all_context(&db);
    let patch = ctx
        .repo::<OrdersRepo>()
        .patch(Some(o.id), &edit(o.modified, json!({"title": "変更"})))
        .await
        .unwrap();
    let elsewhere = context(&db, identity(ALL), registry());

    assert_eq!(patch.id(), Some(&o.id));
    let unregistered = elsewhere.repo::<OrdersRepo>().save_patch(patch).await;
    let stray = ctx
        .repo::<Stray>()
        .patch(None, &json!({"data": {"message": "x"}}))
        .await;

    assert!(
        matches!(
            unregistered,
            Err(PatchError::Repo(RepoError::NotRegistered(_)))
        ),
        "{unregistered:?}"
    );
    assert!(
        matches!(stray, Err(PatchError::Repo(RepoError::NotRegistered(_)))),
        "{:?}",
        stray.err()
    );
    assert_eq!(raw_order(&db, o.id).await.title, "注文");
}

#[test]
fn a_target_key_is_read_from_json_for_a_typed_association() {
    let mut r: Registry = registry();
    r.add(Tags::new());
    r.add(OrderTags::new());
    r.add(ItemsRepo);
    r.add(BillingsRepo {
        calls: Calls::default(),
    });
    r.add(CustomersRepo);
    r.add(OrdersRepo);
    r.verify();
    let relations = r.relations_of::<orders::Entity>().unwrap();
    let customer = relations.iter().find(|a| a.name() == "customer").unwrap();

    use cakedome::key::RowId;
    use cakedome::layer::target_id_from_json;
    assert_eq!(
        target_id_from_json::<OrdersRepo>(customer.as_ref(), &json!(5)),
        Some(RowId::Int(5))
    );
    assert_eq!(
        target_id_from_json::<OrdersRepo>(customer.as_ref(), &json!("x")),
        None
    );
}

// ---- optimistic lock conflicts ----

#[derive(Clone, Copy)]
enum Probe {
    Normal,
    HiddenOnView,
    ContainFails,
    UnreadableScope,
}

struct ProbeCustomers {
    probe: Probe,
}

impl ProbeCustomers {
    const BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId).save();
}

impl Repo for ProbeCustomers {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::BILLING).default_contain(["billing"])
    }
}

#[async_trait]
impl TablePolicy for ProbeCustomers {
    async fn scope(&self, _ctx: &Context, action: Action) -> Result<Condition, RepoError> {
        Ok(match (self.probe, action) {
            (Probe::HiddenOnView, Action::View) => {
                Condition::all().add(customers::Column::Name.ne("hidden"))
            }
            _ => Condition::all(),
        })
    }

    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<customers::Entity>, RepoError> {
        Ok(match self.probe {
            Probe::UnreadableScope => ColumnAccess::all().read_where(
                customers::Column::Email,
                Condition::all().add(Expr::cust("no_such_column = 1")),
            ),
            _ => ColumnAccess::all(),
        })
    }

    async fn can_contain(&self, _ctx: &Context, _name: &str) -> Result<bool, RepoError> {
        match self.probe {
            Probe::ContainFails => Err(RepoError::Forbidden),
            _ => Ok(true),
        }
    }
}

impl EntityPolicy for ProbeCustomers {}

async fn stale_save(
    db: &DatabaseConnection,
    probe: Probe,
    name: &str,
    drop_billings: bool,
) -> Result<customers::Model, PatchError> {
    let c = insert_customer(db, name, true).await;
    insert_billing(db, c.id, "請求先").await;
    let calls = Calls::default();
    let mut r: Registry = registry();
    r.add(BillingsRepo { calls });
    r.add(ProbeCustomers { probe });
    r.verify();
    let ctx = context(db, identity(ALL), r);
    let repo = ctx.repo::<ProbeCustomers>();
    let stale = past() - chrono::Duration::seconds(1);
    let patch = repo
        .patch(Some(c.id), &edit(stale, json!({"name": "変更"})))
        .await
        .unwrap();
    if drop_billings {
        db.execute_unprepared("DROP TABLE customer_billings")
            .await
            .unwrap();
    }
    repo.save_patch(patch).await
}

#[tokio::test]
async fn a_conflict_returns_the_current_row_with_its_default_contain() {
    let db = db().await;

    let res = stale_save(&db, Probe::Normal, "顧客", false).await;

    match res {
        Err(PatchError::Conflict { current }) => {
            assert_eq!(current["name"], "顧客");
            assert_eq!(current["billing"]["billing_name"], "請求先");
        }
        other => panic!("expected Conflict: {:?}", other.err()),
    }
}

#[tokio::test]
async fn a_conflict_reports_why_the_current_row_cannot_be_read() {
    let hidden = stale_save(&db().await, Probe::HiddenOnView, "hidden", false).await;
    let contain = stale_save(&db().await, Probe::ContainFails, "顧客", false).await;
    let scope = stale_save(&db().await, Probe::UnreadableScope, "顧客", false).await;
    let dropped = stale_save(&db().await, Probe::Normal, "顧客", true).await;

    assert!(
        matches!(hidden, Err(PatchError::Repo(RepoError::NotFound))),
        "{:?}",
        hidden.err()
    );
    assert!(
        matches!(contain, Err(PatchError::Repo(RepoError::Forbidden))),
        "{:?}",
        contain.err()
    );
    assert!(
        matches!(scope, Err(PatchError::Repo(RepoError::Db(_)))),
        "{:?}",
        scope.err()
    );
    assert!(
        matches!(dropped, Err(PatchError::Repo(RepoError::Db(_)))),
        "{:?}",
        dropped.err()
    );
}

#[tokio::test]
async fn a_dynamic_default_fills_an_omitted_column_on_add_only() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let ctx = all_context(&db);

    let omitted = run::<OrdersRepo>(&ctx, None, &json!({"data": {"customer_id": c.id}}))
        .await
        .unwrap();
    let sent = run::<OrdersRepo>(
        &ctx,
        None,
        &json!({"data": {"customer_id": c.id, "title": "指定"}}),
    )
    .await
    .unwrap();
    let other = insert_customer(&db, "別の顧客", true).await;
    let edited = run::<OrdersRepo>(
        &ctx,
        Some(sent.id),
        &edit(sent.modified, json!({"customer_id": other.id})),
    )
    .await
    .unwrap();

    assert_eq!(omitted.title, "無題");
    assert_eq!(sent.title, "指定");
    assert_eq!(
        (edited.customer_id, edited.title.as_str()),
        (other.id, "指定")
    );
}
