//! Saving associations with `save_with` / `Nested` / `link`, and deleting dependent children with
//! their parent.

use cakedome::Context;
use cakedome::association::{BelongsToOf, HasManyOf, HasOneOf, ManyToManyOf};
use cakedome::marshal::FieldErrors;
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, Nested, Repo, RepoError};
use cakedome::validate::ValidationInput;
use chrono::Duration;
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};

use crate::common::registry;
use crate::entities::{
    customer_billings, customer_notes, customers, master_tags, order_items, orders,
    setting_order_tags,
};
use crate::support::{Log, context, db, identity, insert_customer, insert_tag, past};

type OrderTags = DefaultRepo<setting_order_tags::Entity>;

// Refuses to detach a note whose body is "keep" (to see errors from a NULL foreign key update).
struct Notes;

impl Repo for Notes {
    type Entity = customer_notes::Entity;

    fn validate(
        &self,
        input: &ValidationInput<'_, customer_notes::Entity>,
        errors: &mut FieldErrors,
    ) {
        let detaching = matches!(input.after.customer_id, Set(None));
        if detaching && input.before.is_some_and(|b| b.body == "keep") {
            errors.add("customer_id", "required", "必須");
        }
    }
}

impl TablePolicy for Notes {}
impl EntityPolicy for Notes {}

struct TagsRepo;

impl TagsRepo {
    const ITEMS: HasManyOf<Self, ItemsRepo> = HasManyOf::new("items", order_items::Column::TagId);
}

impl Repo for TagsRepo {
    type Entity = master_tags::Entity;

    fn initialize(e: RepoConfig<'_, master_tags::Entity>) -> RepoConfig<'_, master_tags::Entity> {
        e.association(Self::ITEMS)
    }
}

impl TablePolicy for TagsRepo {}
impl EntityPolicy for TagsRepo {}

struct ItemsRepo {
    log: Log,
}

impl ItemsRepo {
    const TAG: BelongsToOf<Self, TagsRepo> = BelongsToOf::new("tag", order_items::Column::TagId);
}

#[async_trait]
impl Repo for ItemsRepo {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG)
    }

    async fn after_delete(
        &self,
        _ctx: &Context,
        model: &order_items::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("item:{}", model.name));
        Ok(())
    }
}

impl TablePolicy for ItemsRepo {}
impl EntityPolicy for ItemsRepo {}

struct BillingsRepo {
    log: Log,
}

#[async_trait]
impl Repo for BillingsRepo {
    type Entity = customer_billings::Entity;

    async fn after_delete(
        &self,
        _ctx: &Context,
        model: &customer_billings::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("billing:{}", model.billing_name));
        Ok(())
    }
}

impl TablePolicy for BillingsRepo {}
impl EntityPolicy for BillingsRepo {}

struct OrdersRepo {
    log: Log,
}

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("items", order_items::Column::OrderId).dependent(true);
    const TAGS: ManyToManyOf<Self, OrderTags, TagsRepo> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace)
    .dependent(true);
}

#[async_trait]
impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::TAGS)
    }

    async fn after_delete(&self, _ctx: &Context, model: &orders::Model) -> Result<(), RepoError> {
        self.log.push(format!("order:{}", model.title));
        Ok(())
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

struct CustomersRepo;

impl CustomersRepo {
    const ORDERS: HasManyOf<Self, OrdersRepo> =
        HasManyOf::new("orders", orders::Column::CustomerId)
            .save(SaveStrategy::Append)
            .dependent(true);
    const BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId).dependent(true);
    const NOTES: HasManyOf<Self, Notes> =
        HasManyOf::new("notes", customer_notes::Column::CustomerId).save(SaveStrategy::Append);
    const FIRST_ORDER: HasOneOf<Self, OrdersRepo> =
        HasOneOf::new("first_order", orders::Column::CustomerId);
}

impl Repo for CustomersRepo {
    type Entity = customers::Entity;

    fn validate(&self, input: &ValidationInput<'_, customers::Entity>, errors: &mut FieldErrors) {
        if matches!(&input.after.name, Set(name) if name == "ng") {
            errors.add("name", "ng", "使えない名前");
        }
    }

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::ORDERS)
            .association(Self::BILLING)
            .association(Self::NOTES)
            .association(Self::FIRST_ORDER)
    }
}

impl TablePolicy for CustomersRepo {}
impl EntityPolicy for CustomersRepo {}

// The same tables with associations that delete children with one statement.
struct BulkOrdersRepo {
    log: Log,
}

impl BulkOrdersRepo {
    const ITEMS: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("items", order_items::Column::OrderId).dependent(true);
}

#[async_trait]
impl Repo for BulkOrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::ITEMS)
    }

    async fn after_delete(&self, _ctx: &Context, model: &orders::Model) -> Result<(), RepoError> {
        self.log.push(format!("order:{}", model.title));
        Ok(())
    }
}

impl TablePolicy for BulkOrdersRepo {}
impl EntityPolicy for BulkOrdersRepo {}

struct BulkCustomersRepo;

impl BulkCustomersRepo {
    const ORDERS: HasManyOf<Self, BulkOrdersRepo> =
        HasManyOf::new("orders", orders::Column::CustomerId)
            .dependent(true)
            .cascade_callbacks(false);
    const BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId)
            .dependent(true)
            .cascade_callbacks(false);
}

impl Repo for BulkCustomersRepo {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::ORDERS).association(Self::BILLING)
    }
}

impl TablePolicy for BulkCustomersRepo {}
impl EntityPolicy for BulkCustomersRepo {}

// Notes are bound to a customer by `body = customers.email`.
struct BoundCustomersRepo;

impl BoundCustomersRepo {
    const NOTES: HasManyOf<Self, Notes, String> =
        HasManyOf::new("notes", customer_notes::Column::Body)
            .binding_key::<String>(customers::Column::Email)
            .dependent(true);
}

impl Repo for BoundCustomersRepo {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::NOTES)
    }
}

impl TablePolicy for BoundCustomersRepo {}
impl EntityPolicy for BoundCustomersRepo {}

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
    "entity.customer_notes.*",
    "entity.customer_billings.*",
];

fn main_context(db: &DatabaseConnection) -> (Context, Log) {
    let log = Log::default();
    let mut r: Registry = registry();
    r.add(TagsRepo);
    r.add(OrderTags::new());
    r.add(Notes);
    r.add(ItemsRepo { log: log.clone() });
    r.add(BillingsRepo { log: log.clone() });
    r.add(OrdersRepo { log: log.clone() });
    r.add(CustomersRepo);
    r.verify();
    (context(db, identity(ALL), r), log)
}

fn bulk_context(db: &DatabaseConnection) -> (Context, Log) {
    let log = Log::default();
    let mut r: Registry = registry();
    r.add(TagsRepo);
    r.add(ItemsRepo { log: log.clone() });
    r.add(BillingsRepo { log: log.clone() });
    r.add(BulkOrdersRepo { log: log.clone() });
    r.add(BulkCustomersRepo);
    r.verify();
    (context(db, identity(ALL), r), log)
}

fn bound_context(db: &DatabaseConnection) -> Context {
    let mut r: Registry = registry();
    r.add(Notes);
    r.add(BoundCustomersRepo);
    r.verify();
    context(db, identity(ALL), r)
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

fn new_customer(name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

fn new_order(title: &str) -> orders::ActiveModel {
    orders::ActiveModel {
        title: Set(title.to_string()),
        ..Default::default()
    }
}

fn new_item(name: &str) -> order_items::ActiveModel {
    order_items::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

fn new_billing(name: &str) -> customer_billings::ActiveModel {
    customer_billings::ActiveModel {
        billing_name: Set(name.to_string()),
        ..Default::default()
    }
}

fn new_note(body: &str) -> customer_notes::ActiveModel {
    customer_notes::ActiveModel {
        body: Set(body.to_string()),
        ..Default::default()
    }
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

async fn raw_note(db: &DatabaseConnection, id: i64) -> customer_notes::Model {
    customer_notes::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("note row")
}

async fn raw_customer(db: &DatabaseConnection, id: i64) -> customers::Model {
    customers::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .expect("customer row")
}

async fn orders_of(db: &DatabaseConnection, customer_id: i64) -> Vec<orders::Model> {
    orders::Entity::find()
        .filter(orders::Column::CustomerId.eq(customer_id))
        .order_by_asc(orders::Column::Id)
        .all(db)
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

async fn billings_of(db: &DatabaseConnection, customer_id: i64) -> Vec<customer_billings::Model> {
    customer_billings::Entity::find()
        .filter(customer_billings::Column::CustomerId.eq(customer_id))
        .all(db)
        .await
        .unwrap()
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

async fn count<E: EntityTrait>(db: &DatabaseConnection) -> u64
where
    E::Model: Sync,
{
    E::find().count(db).await.unwrap()
}

fn validation_codes(res: Result<impl std::fmt::Debug, RepoError>) -> Vec<(String, &'static str)> {
    match res {
        Err(RepoError::Validation(errors)) => {
            let mut out: Vec<(String, &'static str)> = errors
                .0
                .iter()
                .flat_map(|(k, v)| v.iter().map(move |e| (k.clone(), e.code)))
                .collect();
            out.sort();
            out
        }
        other => panic!("expected Validation: {other:?}"),
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

// ---- belongsTo ----

#[tokio::test]
async fn belongs_to_saves_a_new_target_before_the_parent() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<OrdersRepo>()
        .save_with(new_order("注文"))
        .belongs_to(OrdersRepo::CUSTOMER, new_customer("新しい顧客"))
        .save()
        .await
        .unwrap();

    let customers = customers::Entity::find().all(&db).await.unwrap();
    assert_eq!(customers.len(), 1);
    assert_eq!(customers[0].name, "新しい顧客");
    assert!(customers[0].is_active);
    assert_eq!(saved.customer_id, customers[0].id);
    let order = raw_order(&db, saved.id).await;
    assert_eq!(order.customer_id, customers[0].id);
    assert_eq!(order.title, "注文");
}

#[tokio::test]
async fn belongs_to_updates_a_target_with_an_id() {
    let db = db().await;
    let c = insert_customer(&db, "旧名", true).await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<OrdersRepo>()
        .save_with(new_order("注文"))
        .belongs_to(
            OrdersRepo::CUSTOMER,
            customers::ActiveModel {
                id: Unchanged(c.id),
                name: Set("新名".to_string()),
                ..Default::default()
            },
        )
        .save()
        .await
        .unwrap();

    assert_eq!(count::<customers::Entity>(&db).await, 1);
    let customer = raw_customer(&db, c.id).await;
    assert_eq!(customer.name, "新名");
    assert!(customer.modified > past());
    assert_eq!(raw_order(&db, saved.id).await.customer_id, c.id);
}

#[tokio::test]
async fn belongs_to_errors_are_keyed_by_the_association_and_nothing_is_saved() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(new_order("注文"))
        .belongs_to(OrdersRepo::CUSTOMER, new_customer("ng"))
        .save()
        .await;

    assert_eq!(validation_codes(res), [("customer.name".to_string(), "ng")]);
    assert_eq!(count::<orders::Entity>(&db).await, 0);
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn belongs_to_target_runs_through_its_own_policy() {
    let db = db().await;
    let log = Log::default();
    let mut r: Registry = registry();
    r.add(TagsRepo);
    r.add(OrderTags::new());
    r.add(Notes);
    r.add(ItemsRepo { log: log.clone() });
    r.add(BillingsRepo { log: log.clone() });
    r.add(OrdersRepo { log });
    r.add(CustomersRepo);
    r.verify();
    let ctx = context(&db, identity(&["entity.orders.*"]), r);

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(new_order("注文"))
        .belongs_to(OrdersRepo::CUSTOMER, new_customer("新しい顧客"))
        .save()
        .await;

    assert!(matches!(res, Err(RepoError::Forbidden)), "{res:?}");
    assert_eq!(count::<orders::Entity>(&db).await, 0);
    assert_eq!(count::<customers::Entity>(&db).await, 0);
}

#[tokio::test]
async fn nested_belongs_to_saves_the_target_of_a_child_row() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .append_nested(
            OrdersRepo::ITEMS,
            vec![Nested::new(new_item("明細")).belongs_to(
                ItemsRepo::TAG,
                master_tags::ActiveModel {
                    code: Set("new_tag".to_string()),
                    name: Set("新しいタグ".to_string()),
                    ..Default::default()
                },
            )],
        )
        .save()
        .await
        .unwrap();

    let tags = master_tags::Entity::find().all(&db).await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].code, "new_tag");
    assert!(tags[0].is_active);
    let items = order_items::Entity::find().all(&db).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "明細");
    assert_eq!(items[0].order_id, o.id);
    assert_eq!(items[0].tag_id, Some(tags[0].id));
}

// ---- hasOne ----

#[tokio::test]
async fn has_one_adds_the_target_with_the_parent_key() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<CustomersRepo>()
        .save_with(new_customer("新しい顧客"))
        .has_one(CustomersRepo::BILLING, new_billing("請求先"))
        .save()
        .await
        .unwrap();

    let rows = billings_of(&db, saved.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].billing_name, "請求先");
    assert_eq!(rows[0].address, None);
    assert_eq!(count::<customer_billings::Entity>(&db).await, 1);
}

#[tokio::test]
async fn has_one_adds_without_an_id_and_updates_with_one() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let (ctx, _) = main_context(&db);
    let parent = || customers::ActiveModel {
        id: Unchanged(c.id),
        ..Default::default()
    };

    ctx.repo::<CustomersRepo>()
        .save_with(parent())
        .has_one(CustomersRepo::BILLING, new_billing("請求先"))
        .save()
        .await
        .unwrap();
    let rows = billings_of(&db, c.id).await;
    assert_eq!(rows.len(), 1);

    ctx.repo::<CustomersRepo>()
        .save_with(parent())
        .has_one(
            CustomersRepo::BILLING,
            customer_billings::ActiveModel {
                id: Unchanged(rows[0].id),
                address: Set(Some("大阪府".to_string())),
                ..Default::default()
            },
        )
        .save()
        .await
        .unwrap();
    let rows = billings_of(&db, c.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].billing_name, "請求先");
    assert_eq!(rows[0].address.as_deref(), Some("大阪府"));
}

#[tokio::test]
async fn has_one_error_keys_have_no_row_number() {
    let db = db().await;
    let mine = insert_customer(&db, "自分", true).await;
    let other = insert_customer(&db, "他", true).await;
    let others = insert_billing(&db, other.id, "他の請求先").await;
    let (ctx, _) = main_context(&db);

    let res = ctx
        .repo::<CustomersRepo>()
        .save_with(customers::ActiveModel {
            id: Unchanged(mine.id),
            ..Default::default()
        })
        .has_one(
            CustomersRepo::BILLING,
            customer_billings::ActiveModel {
                id: Unchanged(others.id),
                billing_name: Set("乗っ取り".to_string()),
                ..Default::default()
            },
        )
        .save()
        .await;

    assert_eq!(
        validation_codes(res),
        [("billing.id".to_string(), "not_found")]
    );
    assert_eq!(
        billings_of(&db, other.id).await[0].billing_name,
        "他の請求先"
    );
    assert!(billings_of(&db, mine.id).await.is_empty());
}

#[tokio::test]
async fn has_one_nested_writes_the_targets_associations() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<CustomersRepo>()
        .save_with(new_customer("新しい顧客"))
        .has_one_nested(
            CustomersRepo::FIRST_ORDER,
            Nested::new(new_order("最初の注文")).append(OrdersRepo::ITEMS, vec![new_item("X")]),
        )
        .save()
        .await
        .unwrap();

    let orders = orders_of(&db, saved.id).await;
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].title, "最初の注文");
    assert_eq!(
        items_of(&db, orders[0].id).await,
        [("X".to_string(), 0, true)]
    );
}

#[tokio::test]
async fn nested_has_one_and_has_one_nested_inside_a_belongs_to_target() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<OrdersRepo>()
        .save_with(new_order("本体"))
        .belongs_to(
            OrdersRepo::CUSTOMER,
            Nested::<CustomersRepo>::new(new_customer("新しい顧客"))
                .has_one(CustomersRepo::BILLING, new_billing("請求先"))
                .has_one_nested(
                    CustomersRepo::FIRST_ORDER,
                    Nested::new(new_order("最初")).append(OrdersRepo::ITEMS, vec![new_item("Y")]),
                ),
        )
        .save()
        .await
        .unwrap();

    let customer_id = saved.customer_id;
    assert_eq!(raw_customer(&db, customer_id).await.name, "新しい顧客");
    assert_eq!(
        billings_of(&db, customer_id).await[0].billing_name,
        "請求先"
    );
    let titles: Vec<String> = orders_of(&db, customer_id)
        .await
        .into_iter()
        .map(|o| o.title)
        .collect();
    assert_eq!(sorted(titles), ["最初", "本体"]);
    let first = orders_of(&db, customer_id)
        .await
        .into_iter()
        .find(|o| o.title == "最初")
        .unwrap();
    assert_eq!(items_of(&db, first.id).await, [("Y".to_string(), 0, true)]);
}

// ---- hasMany ----

#[tokio::test]
async fn append_adds_and_updates_children_and_keeps_the_rest() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let a = insert_item(&db, o.id, "A", 1, true).await;
    insert_item(&db, o.id, "B", 2, true).await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .append(
            OrdersRepo::ITEMS,
            vec![
                order_items::ActiveModel {
                    id: Unchanged(a.id),
                    name: Set("A2".to_string()),
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
            ("A2".to_string(), 1, true),
            ("B".to_string(), 2, true),
            ("C".to_string(), 0, true),
        ],
        "B stays, nothing is renumbered, and the new row keeps the DB default order"
    );
    assert!(raw_order(&db, o.id).await.modified > past());
}

#[tokio::test]
async fn append_nested_saves_three_levels() {
    let db = db().await;
    let (ctx, _) = main_context(&db);

    let saved = ctx
        .repo::<CustomersRepo>()
        .save_with(new_customer("新しい顧客"))
        .append_nested(
            CustomersRepo::ORDERS,
            vec![
                Nested::new(new_order("o1")).append_nested(
                    OrdersRepo::ITEMS,
                    vec![new_item("a").into(), new_item("b").into()],
                ),
                Nested::new(new_order("o2")).append(OrdersRepo::ITEMS, vec![new_item("c")]),
            ],
        )
        .save()
        .await
        .unwrap();

    let orders = orders_of(&db, saved.id).await;
    let titles: Vec<&str> = orders.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(titles, ["o1", "o2"]);
    assert_eq!(
        items_of(&db, orders[0].id).await,
        [("a".to_string(), 0, true), ("b".to_string(), 0, true)]
    );
    assert_eq!(
        items_of(&db, orders[1].id).await,
        [("c".to_string(), 0, true)]
    );
    assert_eq!(count::<order_items::Entity>(&db).await, 3);
}

#[tokio::test]
async fn replace_nested_updates_adds_and_removes_at_each_level() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o1 = insert_order(&db, c.id, "O1").await;
    let a = insert_item(&db, o1.id, "a", 1, true).await;
    insert_item(&db, o1.id, "b", 2, true).await;
    let o2 = insert_order(&db, c.id, "O2").await;
    insert_item(&db, o2.id, "d", 1, true).await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<CustomersRepo>()
        .save_with(customers::ActiveModel {
            id: Unchanged(c.id),
            ..Default::default()
        })
        .replace_nested(
            CustomersRepo::ORDERS,
            vec![
                Nested::new(orders::ActiveModel {
                    id: Unchanged(o1.id),
                    ..Default::default()
                })
                .replace_nested(
                    OrdersRepo::ITEMS,
                    vec![
                        order_items::ActiveModel {
                            id: Unchanged(a.id),
                            name: Set("a2".to_string()),
                            ..Default::default()
                        }
                        .into(),
                        new_item("c").into(),
                    ],
                ),
            ],
        )
        .save()
        .await
        .unwrap();

    assert!(raw_order(&db, o1.id).await.is_active);
    assert!(!raw_order(&db, o2.id).await.is_active, "O2 is soft-deleted");
    assert_eq!(
        items_of(&db, o2.id).await,
        [("d".to_string(), 1, false)],
        "the dependent items of O2 go with it"
    );
    assert_eq!(
        items_of(&db, o1.id).await,
        [
            ("a2".to_string(), 1, true),
            ("b".to_string(), 2, false),
            ("c".to_string(), 2, true),
        ]
    );
    assert!(raw_customer(&db, c.id).await.modified > past());
}

#[tokio::test]
async fn the_same_association_twice_is_an_error() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = main_context(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            title: Set("変更".to_string()),
            ..Default::default()
        })
        .append(OrdersRepo::ITEMS, vec![new_item("x")])
        .replace(OrdersRepo::ITEMS, vec![new_item("y")])
        .save()
        .await;

    match res {
        Err(RepoError::Db(e)) => assert!(e.to_string().contains("specified twice"), "{e}"),
        other => panic!("expected Db: {other:?}"),
    }
    assert_eq!(count::<order_items::Entity>(&db).await, 0);
    assert_eq!(raw_order(&db, o.id).await.title, "注文");
}

// ---- if_unmodified ----

#[tokio::test]
async fn if_unmodified_conflicts_on_a_stale_parent_and_writes_nothing() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = main_context(&db);
    let save = |expected| {
        ctx.repo::<OrdersRepo>()
            .save_with(orders::ActiveModel {
                id: Unchanged(o.id),
                title: Set("変更".to_string()),
                ..Default::default()
            })
            .append(OrdersRepo::ITEMS, vec![new_item("x")])
            .if_unmodified(expected)
            .save()
    };

    let res = save(o.modified + Duration::seconds(1)).await;

    assert!(matches!(res, Err(RepoError::Conflict)), "{res:?}");
    assert_eq!(raw_order(&db, o.id).await.title, "注文");
    assert_eq!(count::<order_items::Entity>(&db).await, 0);

    save(o.modified).await.unwrap();

    assert_eq!(raw_order(&db, o.id).await.title, "変更");
    assert_eq!(items_of(&db, o.id).await, [("x".to_string(), 0, true)]);
}

// ---- RepoHandle link / unlink / replace ----

#[tokio::test]
async fn replace_on_the_handle_replaces_children_whatever_the_save_mode() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let kept_note = insert_note(&db, c.id, "残す").await;
    let dropped_note = insert_note(&db, c.id, "外す").await;
    let kept_order = insert_order(&db, c.id, "残る注文").await;
    let dropped_order = insert_order(&db, c.id, "消える注文").await;
    insert_item(&db, dropped_order.id, "x", 1, true).await;
    let (ctx, _) = main_context(&db);
    let repo = ctx.repo::<CustomersRepo>();

    repo.replace(
        CustomersRepo::NOTES,
        &c,
        vec![customer_notes::ActiveModel {
            id: Unchanged(kept_note.id),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    repo.replace(
        CustomersRepo::ORDERS,
        &c,
        vec![orders::ActiveModel {
            id: Unchanged(kept_order.id),
            ..Default::default()
        }],
    )
    .await
    .unwrap();

    let dropped = raw_note(&db, dropped_note.id).await;
    assert_eq!(dropped.customer_id, None, "nullable and not dependent");
    assert!(dropped.modified > past());
    assert_eq!(raw_note(&db, kept_note.id).await.customer_id, Some(c.id));
    assert!(raw_order(&db, kept_order.id).await.is_active);
    assert!(
        !raw_order(&db, dropped_order.id).await.is_active,
        "dependent children are deleted"
    );
    assert_eq!(
        items_of(&db, dropped_order.id).await,
        [("x".to_string(), 1, false)]
    );
}

#[tokio::test]
async fn replace_links_on_the_handle_relinks_targets_in_order() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    for (tag, seq) in [(&t1, 1), (&t2, 2)] {
        setting_order_tags::ActiveModel {
            order_id: Set(o.id),
            tag_id: Set(tag.id),
            seq: Set(seq),
            ..Default::default()
        }
        .insert(&db)
        .await
        .unwrap();
    }
    let (ctx, _) = main_context(&db);

    ctx.repo::<OrdersRepo>()
        .replace_links(OrdersRepo::TAGS, &o, vec![t3.id, t1.id])
        .await
        .unwrap();

    assert_eq!(links_of(&db, o.id).await, [(t3.id, 1), (t1.id, 2)]);
    assert_eq!(count::<setting_order_tags::Entity>(&db).await, 2);
}

#[tokio::test]
async fn unlink_detaches_has_many_children_by_nulling_or_deleting() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let other = insert_customer(&db, "他の顧客", true).await;
    let n1 = insert_note(&db, c.id, "n1").await;
    let n2 = insert_note(&db, c.id, "n2").await;
    let foreign_note = insert_note(&db, other.id, "他の顧客のメモ").await;
    let o = insert_order(&db, c.id, "注文").await;
    let other_order = insert_order(&db, other.id, "他の注文").await;
    let i1 = insert_item(&db, o.id, "i1", 1, true).await;
    let i2 = insert_item(&db, o.id, "i2", 2, true).await;
    let foreign_item = insert_item(&db, other_order.id, "他の明細", 1, true).await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<CustomersRepo>()
        .unlink(CustomersRepo::NOTES, &c, vec![n1.id, foreign_note.id])
        .await
        .unwrap();
    ctx.repo::<OrdersRepo>()
        .unlink(OrdersRepo::ITEMS, &o, vec![i1.id, foreign_item.id])
        .await
        .unwrap();

    assert_eq!(raw_note(&db, n1.id).await.customer_id, None);
    assert_eq!(raw_note(&db, n2.id).await.customer_id, Some(c.id));
    assert_eq!(
        raw_note(&db, foreign_note.id).await.customer_id,
        Some(other.id)
    );
    assert!(!raw_item(&db, i1.id).await.is_active);
    assert!(raw_item(&db, i2.id).await.is_active);
    assert!(raw_item(&db, foreign_item.id).await.is_active);
}

#[tokio::test]
async fn link_advances_the_parents_modified() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let other = insert_customer(&db, "他の顧客", true).await;
    let moved = insert_note(&db, other.id, "移すメモ").await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<CustomersRepo>()
        .link(
            CustomersRepo::NOTES,
            &c,
            vec![
                new_note("新しいメモ"),
                customer_notes::ActiveModel {
                    id: Unchanged(moved.id),
                    ..Default::default()
                },
            ],
        )
        .await
        .unwrap();

    assert!(raw_customer(&db, c.id).await.modified > past());
    assert_eq!(raw_customer(&db, other.id).await.modified, past());
    assert_eq!(raw_note(&db, moved.id).await.customer_id, Some(c.id));
    let notes = customer_notes::Entity::find()
        .filter(customer_notes::Column::CustomerId.eq(c.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(notes.len(), 2);
}

#[tokio::test]
async fn link_on_a_parent_without_a_modified_column_still_connects() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let item = insert_item(&db, o.id, "明細", 1, true).await;
    let tag = insert_tag(&db, "t", true).await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<TagsRepo>()
        .link(
            TagsRepo::ITEMS,
            &tag,
            vec![order_items::ActiveModel {
                id: Unchanged(item.id),
                ..Default::default()
            }],
        )
        .await
        .unwrap();

    assert_eq!(raw_item(&db, item.id).await.tag_id, Some(tag.id));
}

// ---- deleting a parent ----

struct Family {
    customer: customers::Model,
    orders: Vec<orders::Model>,
}

async fn insert_family(db: &DatabaseConnection, name: &str) -> Family {
    let customer = insert_customer(db, name, true).await;
    let o1 = insert_order(db, customer.id, &format!("{name}-o1")).await;
    let o2 = insert_order(db, customer.id, &format!("{name}-o2")).await;
    insert_item(db, o1.id, &format!("{name}-i1"), 1, true).await;
    insert_item(db, o1.id, &format!("{name}-i2"), 2, true).await;
    insert_item(db, o2.id, &format!("{name}-i3"), 1, true).await;
    insert_billing(db, customer.id, &format!("{name}-b")).await;
    Family {
        customer,
        orders: vec![o1, o2],
    }
}

#[tokio::test]
async fn deleting_a_parent_deletes_dependents_in_bulk_without_child_hooks() {
    let db = db().await;
    let gone = insert_family(&db, "gone").await;
    let kept = insert_family(&db, "kept").await;
    let (ctx, log) = bulk_context(&db);

    ctx.repo::<BulkCustomersRepo>()
        .delete(gone.customer.id)
        .await
        .unwrap();

    assert!(!raw_customer(&db, gone.customer.id).await.is_active);
    for o in &gone.orders {
        let order = raw_order(&db, o.id).await;
        assert!(!order.is_active, "soft-deleted by one statement");
        assert!(order.modified > past());
    }
    assert!(billings_of(&db, gone.customer.id).await.is_empty());
    assert!(
        items_of(&db, gone.orders[0].id).await.iter().all(|i| i.2),
        "the children's own dependents are not followed"
    );
    assert!(log.take().is_empty(), "no child hooks run");
    assert!(raw_customer(&db, kept.customer.id).await.is_active);
    assert!(raw_order(&db, kept.orders[0].id).await.is_active);
    assert_eq!(billings_of(&db, kept.customer.id).await.len(), 1);
}

#[tokio::test]
async fn purging_a_parent_purges_dependents_in_bulk() {
    let db = db().await;
    let gone = insert_family(&db, "gone").await;
    insert_order(&db, gone.customer.id, "gone-inactive").await;
    orders::ActiveModel {
        id: Unchanged(gone.orders[1].id),
        is_active: Set(false),
        ..Default::default()
    }
    .update(&db)
    .await
    .unwrap();
    let kept = insert_family(&db, "kept").await;
    let (ctx, log) = bulk_context(&db);

    ctx.repo::<BulkCustomersRepo>()
        .purge(gone.customer.id)
        .await
        .unwrap();

    assert!(
        customers::Entity::find_by_id(gone.customer.id)
            .one(&db)
            .await
            .unwrap()
            .is_none()
    );
    assert!(orders_of(&db, gone.customer.id).await.is_empty());
    assert!(billings_of(&db, gone.customer.id).await.is_empty());
    assert_eq!(
        items_of(&db, gone.orders[0].id).await.len(),
        2,
        "the children's own dependents are not followed"
    );
    assert!(log.take().is_empty(), "no child hooks run");
    assert_eq!(orders_of(&db, kept.customer.id).await.len(), 2);
    assert_eq!(billings_of(&db, kept.customer.id).await.len(), 1);
}

#[tokio::test]
async fn deleting_or_purging_with_cascade_callbacks_runs_child_hooks() {
    let db = db().await;
    let deleted = insert_family(&db, "d").await;
    let purged = insert_family(&db, "p").await;
    let (ctx, log) = main_context(&db);

    ctx.repo::<CustomersRepo>()
        .delete(deleted.customer.id)
        .await
        .unwrap();

    assert_eq!(
        sorted(log.take()),
        [
            "billing:d-b",
            "item:d-i1",
            "item:d-i2",
            "item:d-i3",
            "order:d-o1",
            "order:d-o2"
        ]
    );
    assert!(!raw_customer(&db, deleted.customer.id).await.is_active);
    for o in &deleted.orders {
        assert!(!raw_order(&db, o.id).await.is_active);
        assert!(items_of(&db, o.id).await.iter().all(|i| !i.2));
    }
    assert!(billings_of(&db, deleted.customer.id).await.is_empty());
    assert!(raw_order(&db, purged.orders[0].id).await.is_active);

    ctx.repo::<CustomersRepo>()
        .purge(purged.customer.id)
        .await
        .unwrap();

    assert_eq!(
        sorted(log.take()),
        [
            "billing:p-b",
            "item:p-i1",
            "item:p-i2",
            "item:p-i3",
            "order:p-o1",
            "order:p-o2"
        ]
    );
    assert!(
        customers::Entity::find_by_id(purged.customer.id)
            .one(&db)
            .await
            .unwrap()
            .is_none()
    );
    assert!(orders_of(&db, purged.customer.id).await.is_empty());
    for o in &purged.orders {
        assert!(items_of(&db, o.id).await.is_empty());
    }
    assert!(billings_of(&db, purged.customer.id).await.is_empty());
    assert_eq!(
        count::<orders::Entity>(&db).await,
        2,
        "only the deleted family's orders remain"
    );
}

#[tokio::test]
async fn deleting_a_parent_keeps_non_dependent_children_untouched() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let note = insert_note(&db, c.id, "メモ").await;
    let (ctx, _) = main_context(&db);

    ctx.repo::<CustomersRepo>().delete(c.id).await.unwrap();

    assert!(!raw_customer(&db, c.id).await.is_active);
    let note = raw_note(&db, note.id).await;
    assert_eq!(
        note.customer_id,
        Some(c.id),
        "only dependent children are touched when the parent is deleted"
    );
    assert_eq!(note.modified, past());
}

// ---- input mistakes, bulk replace, binding keys ----

#[tokio::test]
async fn repeated_ids_are_input_errors_and_nothing_is_saved() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let a = insert_item(&db, o.id, "A", 1, true).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let (ctx, _) = main_context(&db);
    let item = || order_items::ActiveModel {
        id: Unchanged(a.id),
        name: Set("A2".to_string()),
        ..Default::default()
    };

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            title: Set("変更".to_string()),
            ..Default::default()
        })
        .append(OrdersRepo::ITEMS, vec![item(), item()])
        .replace_links(OrdersRepo::TAGS, vec![t1.id, t1.id])
        .save()
        .await;

    assert_eq!(
        validation_codes(res),
        [
            ("items.1.id".to_string(), "duplicate"),
            ("tags.1".to_string(), "duplicate"),
        ]
    );
    assert_eq!(raw_order(&db, o.id).await.title, "注文");
    assert_eq!(items_of(&db, o.id).await, [("A".to_string(), 1, true)]);
    assert_eq!(count::<setting_order_tags::Entity>(&db).await, 0);
}

#[tokio::test]
async fn setting_the_foreign_key_of_a_child_is_a_managed_column_error() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = insert_order(&db, c.id, "注文").await;
    let (ctx, _) = main_context(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .append(
            OrdersRepo::ITEMS,
            vec![order_items::ActiveModel {
                order_id: Set(o.id),
                name: Set("x".to_string()),
                ..Default::default()
            }],
        )
        .save()
        .await;

    assert!(
        matches!(&res, Err(RepoError::ManagedColumn(c)) if c == "order_id"),
        "{res:?}"
    );
    assert_eq!(count::<order_items::Entity>(&db).await, 0);
}

#[tokio::test]
async fn nulling_a_removed_child_reports_its_validation_errors_under_the_association() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let kept = insert_note(&db, c.id, "残す").await;
    let pinned = insert_note(&db, c.id, "keep").await;
    let (ctx, _) = main_context(&db);

    let res = ctx
        .repo::<CustomersRepo>()
        .replace(
            CustomersRepo::NOTES,
            &c,
            vec![customer_notes::ActiveModel {
                id: Unchanged(kept.id),
                ..Default::default()
            }],
        )
        .await;

    assert_eq!(
        validation_codes(res),
        [("notes.customer_id".to_string(), "required")]
    );
    assert_eq!(raw_note(&db, pinned.id).await.customer_id, Some(c.id));
}

#[tokio::test]
async fn replace_without_cascade_callbacks_deletes_dependents_with_one_statement() {
    let db = db().await;
    let family = insert_family(&db, "f").await;
    let (ctx, log) = bulk_context(&db);

    ctx.repo::<BulkCustomersRepo>()
        .replace(
            BulkCustomersRepo::ORDERS,
            &family.customer,
            vec![orders::ActiveModel {
                id: Unchanged(family.orders[0].id),
                ..Default::default()
            }],
        )
        .await
        .unwrap();

    assert!(raw_order(&db, family.orders[0].id).await.is_active);
    let dropped = raw_order(&db, family.orders[1].id).await;
    assert!(!dropped.is_active);
    assert!(dropped.modified > past());
    assert!(log.take().is_empty(), "no child hooks run");
}

#[tokio::test]
async fn deleting_or_purging_a_parent_without_children_is_fine_in_bulk() {
    let db = db().await;
    let a = insert_customer(&db, "a", true).await;
    let b = insert_customer(&db, "b", true).await;
    let (ctx, _) = bulk_context(&db);

    ctx.repo::<BulkCustomersRepo>().delete(a.id).await.unwrap();
    ctx.repo::<BulkCustomersRepo>().purge(b.id).await.unwrap();

    assert!(!raw_customer(&db, a.id).await.is_active);
    assert!(
        customers::Entity::find_by_id(b.id)
            .one(&db)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn binding_key_children_follow_the_bound_column_and_a_null_key_is_an_error() {
    let db = db().await;
    let with_email = customers::ActiveModel {
        name: Set("メールあり".to_string()),
        email: Set(Some("a@example.com".to_string())),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let without_email = insert_customer(&db, "メールなし", true).await;
    let unrelated = insert_note(&db, without_email.id, "a@example.com 以外").await;
    let ctx = bound_context(&db);
    let repo = ctx.repo::<BoundCustomersRepo>();

    repo.link(
        BoundCustomersRepo::NOTES,
        &with_email,
        vec![customer_notes::ActiveModel {
            customer_id: Set(None),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    let notes = customer_notes::Entity::find()
        .filter(customer_notes::Column::Body.eq("a@example.com"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        notes.len(),
        1,
        "the child's bound column takes the parent's email"
    );

    let linked = repo
        .link(
            BoundCustomersRepo::NOTES,
            &without_email,
            vec![customer_notes::ActiveModel {
                customer_id: Set(None),
                ..Default::default()
            }],
        )
        .await;
    assert_eq!(
        validation_codes(linked.map(|_| ())),
        [("notes".to_string(), "invalid")]
    );
    let unlinked = repo
        .unlink(
            BoundCustomersRepo::NOTES,
            &without_email,
            vec![unrelated.id],
        )
        .await;
    assert_eq!(
        validation_codes(unlinked.map(|_| ())),
        [("notes".to_string(), "invalid")]
    );

    repo.delete(without_email.id).await.unwrap();
    assert_eq!(count::<customer_notes::Entity>(&db).await, 2);
    repo.delete(with_email.id).await.unwrap();
    let left = customer_notes::Entity::find().all(&db).await.unwrap();
    assert_eq!(
        left.iter().map(|n| n.id).collect::<Vec<_>>(),
        [unrelated.id],
        "only the notes bound to the deleted customer's email go"
    );
}
