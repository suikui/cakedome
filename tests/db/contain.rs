//! Typed `contain` (nested `with`, `contain_default`, `query`) and the JSON of loaded rows
//! (`to_json`, `_label`, `_joinData`).

use cakedome::Context;
use cakedome::association::{BelongsToOf, HasManyOf, HasOneOf, ManyToManyOf};
use cakedome::policy::{ColumnAccess, EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig};
use cakedome::repo::{Containable, Loaded, NotLoaded, Repo, RepoError};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, DbErr, Set};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{
    customer_billings, customers, master_tags, order_items, orders, setting_order_tags,
};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

const TS: &str = "2020-01-01T00:00:00.000000Z";

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.customer_billings.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
];

/// Switches that change the Repos of one test.
#[derive(Clone, Copy, Default)]
struct Setup {
    /// `customers.email` is unreadable at table level.
    hide_email: bool,
    /// `customers.email` is readable only for names starting with "公開".
    email_for_open_names: bool,
    /// `customers.email` is `hidden` in the registration.
    hidden_email_column: bool,
    /// `setting_order_tags.note` is unreadable.
    hide_note: bool,
    /// `master_tags.name` is unreadable for the tag whose code is "secret".
    tag_name_unless_secret: bool,
    /// The display field of customers is `email` (nullable).
    customer_label_email: bool,
    /// The display field of order items is `seq` (a number).
    item_label_seq: bool,
    /// `customers` has an association whose declaration belongs to another Repo type, listed in
    /// `default_contain`.
    broken_default: bool,
}

struct TagsRepo(Setup);

impl Repo for TagsRepo {
    type Entity = master_tags::Entity;

    fn initialize(e: RepoConfig<'_, master_tags::Entity>) -> RepoConfig<'_, master_tags::Entity> {
        e.display_field(master_tags::Column::Name)
    }
}

#[async_trait]
impl TablePolicy for TagsRepo {
    async fn columns(
        &self,
        _ctx: &Context,
    ) -> Result<ColumnAccess<master_tags::Entity>, RepoError> {
        let access = ColumnAccess::all();
        if self.0.tag_name_unless_secret {
            return Ok(access.read_where(
                master_tags::Column::Name,
                Condition::all().add(master_tags::Column::Code.ne("secret")),
            ));
        }
        Ok(access)
    }
}

impl EntityPolicy for TagsRepo {}

struct JunctionRepo(Setup);

impl Repo for JunctionRepo {
    type Entity = setting_order_tags::Entity;
}

#[async_trait]
impl TablePolicy for JunctionRepo {
    async fn columns(
        &self,
        _ctx: &Context,
    ) -> Result<ColumnAccess<setting_order_tags::Entity>, RepoError> {
        let access = ColumnAccess::all();
        if self.0.hide_note {
            return Ok(access.hide(setting_order_tags::Column::Note));
        }
        Ok(access)
    }
}

impl EntityPolicy for JunctionRepo {}

struct ItemsRepo;

impl ItemsRepo {
    const TAG: BelongsToOf<Self, TagsRepo> = BelongsToOf::new("tag", order_items::Column::TagId);
}

impl Repo for ItemsRepo {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG)
            .display_field(order_items::Column::Name)
    }
}

impl TablePolicy for ItemsRepo {}
impl EntityPolicy for ItemsRepo {}

struct BillingsRepo;

impl Repo for BillingsRepo {
    type Entity = customer_billings::Entity;

    fn initialize(
        e: RepoConfig<'_, customer_billings::Entity>,
    ) -> RepoConfig<'_, customer_billings::Entity> {
        e.display_field(customer_billings::Column::BillingName)
    }
}

impl TablePolicy for BillingsRepo {}
impl EntityPolicy for BillingsRepo {}

struct CustomersRepo(Setup);

impl CustomersRepo {
    const ORDERS: HasManyOf<Self, OrdersRepo> =
        HasManyOf::new("orders", orders::Column::CustomerId);
    const BILLING: HasOneOf<Self, BillingsRepo> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId);
    const BROKEN: HasManyOf<AltCustomersRepo, OrdersRepo> =
        HasManyOf::new("broken", orders::Column::CustomerId);
}

impl Repo for CustomersRepo {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.association(Self::ORDERS)
            .association(Self::BILLING)
            .display_field(customers::Column::Name)
            .default_contain(["orders", "orders.tags"])
    }
}

#[async_trait]
impl TablePolicy for CustomersRepo {
    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<customers::Entity>, RepoError> {
        let mut access = ColumnAccess::all();
        if self.0.hide_email {
            access = access.hide(customers::Column::Email);
        }
        if self.0.email_for_open_names {
            access = access.read_where(
                customers::Column::Email,
                Condition::all().add(customers::Column::Name.starts_with("公開")),
            );
        }
        Ok(access)
    }
}

impl EntityPolicy for CustomersRepo {}

/// Another Repo type for the customers entity; never registered.
struct AltCustomersRepo;

impl Repo for AltCustomersRepo {
    type Entity = customers::Entity;
}

impl TablePolicy for AltCustomersRepo {}
impl EntityPolicy for AltCustomersRepo {}

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, ItemsRepo> = HasManyOf::new("items", order_items::Column::OrderId);
    const TAGS: ManyToManyOf<Self, JunctionRepo, TagsRepo> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    );
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::TAGS)
            .display_field(orders::Column::Title)
            .default_contain(["customer", "items", "items.tag", "tags"])
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

type Customers = CustomersRepo;
type Items = ItemsRepo;
type Orders = OrdersRepo;
type Tags = TagsRepo;

fn build_registry(s: Setup) -> Registry {
    let mut r: Registry = registry();
    r.add(TagsRepo(s));
    r.add(JunctionRepo(s));
    let items = r.add(ItemsRepo);
    if s.item_label_seq {
        items.display_field(order_items::Column::Seq);
    }
    r.add(BillingsRepo);
    let c = r.add(CustomersRepo(s));
    let c = if s.hidden_email_column {
        c.hidden(customers::Column::Email)
    } else {
        c
    };
    let c = if s.customer_label_email {
        c.display_field(customers::Column::Email)
    } else {
        c
    };
    if s.broken_default {
        c.add_association(CustomersRepo::BROKEN)
            .default_contain(["orders", "broken"]);
    }
    r.add(OrdersRepo);
    r.verify();
    r
}

fn ctx_with(db: &DatabaseConnection, patterns: &[&str], setup: Setup) -> Context {
    context(db, identity(patterns), build_registry(setup))
}

fn ctx(db: &DatabaseConnection) -> Context {
    ctx_with(db, ALL, Setup::default())
}

async fn customer_with_email(db: &DatabaseConnection, name: &str, email: &str) -> customers::Model {
    customers::ActiveModel {
        name: Set(name.to_string()),
        email: Set(Some(email.to_string())),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn order(db: &DatabaseConnection, customer_id: i64, title: &str) -> orders::Model {
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

async fn item(
    db: &DatabaseConnection,
    order_id: i64,
    name: &str,
    seq: i32,
    tag_id: Option<i64>,
    active: bool,
) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set(name.to_string()),
        tag_id: Set(tag_id),
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

async fn link(
    db: &DatabaseConnection,
    order_id: i64,
    tag_id: i64,
    seq: i32,
    note: Option<&str>,
) -> setting_order_tags::Model {
    setting_order_tags::ActiveModel {
        order_id: Set(order_id),
        tag_id: Set(tag_id),
        seq: Set(seq),
        note: Set(note.map(str::to_string)),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn billing(db: &DatabaseConnection, customer_id: i64, name: &str) {
    customer_billings::ActiveModel {
        customer_id: Set(customer_id),
        billing_name: Set(name.to_string()),
        address: Set(Some("東京".to_string())),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

/// A customer with one order of two items (the first has a tag) and one linked tag.
async fn full_order(db: &DatabaseConnection, prefix: &str) -> orders::Model {
    let c = insert_customer(db, &format!("{prefix}_顧客"), true).await;
    let t = insert_tag(db, &format!("{prefix}_a"), true).await;
    let u = insert_tag(db, &format!("{prefix}_b"), true).await;
    let o = order(db, c.id, &format!("{prefix}_注文")).await;
    item(db, o.id, "明細1", 1, Some(t.id), true).await;
    item(db, o.id, "明細2", 2, None, true).await;
    link(db, o.id, u.id, 1, None).await;
    o
}

fn tag_codes(items: &[Loaded<order_items::Model>]) -> Vec<String> {
    items
        .iter()
        .map(|i| {
            i.related(ItemsRepo::TAG)
                .unwrap()
                .map(|t| t.code.clone())
                .unwrap_or_else(|| "-".to_string())
        })
        .collect()
}

fn item_names(items: &[Loaded<order_items::Model>]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

fn tag_code_list(tags: &[Loaded<master_tags::Model>]) -> Vec<&str> {
    tags.iter().map(|t| t.code.as_str()).collect()
}

async fn read_order<A: Containable<OrdersRepo>>(
    ctx: &Context,
    id: i64,
    contain: A,
) -> Loaded<orders::Model> {
    ctx.repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(id))
        .contain(contain)
        .all()
        .await
        .unwrap()
        .remove(0)
}

fn is_db_error<T: std::fmt::Debug>(res: &Result<T, RepoError>) -> bool {
    matches!(res, Err(RepoError::Db(_)))
}

// ---- nested `with` ----

#[tokio::test]
async fn nested_belongs_to_attaches_the_tag_and_none_when_absent() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let o1 = order(&db, c.id, "注文1").await;
    let o2 = order(&db, c.id, "注文2").await;
    item(&db, o1.id, "明細1", 1, Some(a.id), true).await;
    item(&db, o1.id, "明細2", 2, None, true).await;
    item(&db, o2.id, "明細3", 1, Some(b.id), true).await;

    let rows = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::ITEMS.with(ItemsRepo::TAG))
        .all()
        .await
        .unwrap();

    assert_eq!(
        tag_codes(rows[0].related(OrdersRepo::ITEMS).unwrap()),
        ["a", "-"]
    );
    assert_eq!(
        tag_codes(rows[1].related(OrdersRepo::ITEMS).unwrap()),
        ["b"]
    );
}

#[tokio::test]
async fn three_levels_and_two_associations_on_one_level() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let o1 = order(&db, c.id, "注文1").await;
    let o2 = order(&db, c.id, "注文2").await;
    item(&db, o1.id, "明細1", 1, Some(a.id), true).await;
    link(&db, o1.id, b.id, 1, None).await;

    let rows = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.eq(c.id))
        .contain(
            CustomersRepo::ORDERS
                .with(OrdersRepo::ITEMS.with(ItemsRepo::TAG))
                .with(OrdersRepo::TAGS),
        )
        .all()
        .await
        .unwrap();

    let orders = rows[0].related(CustomersRepo::ORDERS).unwrap();
    let ids: Vec<i64> = orders.iter().map(|o| o.id).collect();
    assert_eq!(ids, [o1.id, o2.id]);
    assert_eq!(
        tag_codes(orders[0].related(OrdersRepo::ITEMS).unwrap()),
        ["a"]
    );
    assert_eq!(
        tag_code_list(orders[0].related(OrdersRepo::TAGS).unwrap()),
        ["b"]
    );
    assert!(orders[1].related(OrdersRepo::ITEMS).unwrap().is_empty());
    assert!(orders[1].related(OrdersRepo::TAGS).unwrap().is_empty());
}

#[tokio::test]
async fn belongs_to_target_gets_its_own_nested_association() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o1 = order(&db, c.id, "注文1").await;
    let o2 = order(&db, c.id, "注文2").await;

    let rows = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::CUSTOMER.with(CustomersRepo::ORDERS))
        .all()
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    for row in &rows {
        let customer = row.related(OrdersRepo::CUSTOMER).unwrap().unwrap();
        let ids: Vec<i64> = customer
            .related(CustomersRepo::ORDERS)
            .unwrap()
            .iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(ids, [o1.id, o2.id]);
    }
}

#[tokio::test]
async fn a_level_without_with_is_not_loaded() {
    let db = db().await;
    let ctx = ctx(&db);
    let o = full_order(&db, "nl").await;

    let row = read_order(&ctx, o.id, OrdersRepo::ITEMS).await;

    let items = row.related(OrdersRepo::ITEMS).unwrap();
    let err = items[0].related(ItemsRepo::TAG).unwrap_err();
    assert_eq!(err, NotLoaded { name: "tag" });
    assert_eq!(
        err.to_string(),
        "association tag was not loaded (not contained)"
    );
    assert!(matches!(RepoError::from(err), RepoError::Db(_)));
    assert_eq!(row.related(OrdersRepo::TAGS).unwrap_err().name, "tags");
}

#[tokio::test]
async fn nested_target_needs_view_permission_unless_unchecked() {
    let db = db().await;
    let o = full_order(&db, "perm").await;

    let ctx = ctx_with(
        &db,
        &["entity.orders.*", "entity.order_items.*"],
        Setup::default(),
    );
    let res = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::ITEMS.with(ItemsRepo::TAG))
        .all()
        .await;
    assert!(matches!(res, Err(RepoError::Forbidden)), "{res:?}");

    let ctx = ctx_with(&db, &[], Setup::default());
    let rows = ctx
        .repo::<Orders>()
        .unchecked()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::ITEMS.with(ItemsRepo::TAG))
        .all()
        .await
        .unwrap();
    assert_eq!(
        tag_codes(rows[0].related(OrdersRepo::ITEMS).unwrap()),
        ["perm_a", "-"]
    );
}

#[tokio::test]
async fn containing_the_same_association_twice_is_an_error() {
    let db = db().await;
    let ctx = ctx(&db);
    let o = full_order(&db, "dup").await;

    let nested = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::ITEMS.with(ItemsRepo::TAG).with(ItemsRepo::TAG))
        .all()
        .await;
    assert!(is_db_error(&nested), "{nested:?}");

    let top = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::ITEMS)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await;
    assert!(is_db_error(&top), "{top:?}");
}

// ---- defaults ----

#[tokio::test]
async fn get_detail_loads_every_default_including_nested() {
    let db = db().await;
    let ctx = ctx(&db);
    let o = full_order(&db, "gd").await;

    let d = ctx.repo::<Orders>().get_detail(o.id).await.unwrap();

    assert_eq!(d.id, o.id);
    assert_eq!(
        d.related(OrdersRepo::CUSTOMER).unwrap().unwrap().name,
        "gd_顧客"
    );
    // "items" and "items.tag" are both listed; the items are read once and carry the tags.
    let items = d.related(OrdersRepo::ITEMS).unwrap();
    assert_eq!(item_names(items), ["明細1", "明細2"]);
    assert_eq!(tag_codes(items), ["gd_a", "-"]);
    assert_eq!(
        tag_code_list(d.related(OrdersRepo::TAGS).unwrap()),
        ["gd_b"]
    );
}

#[tokio::test]
async fn get_detail_is_not_found_or_forbidden_like_get() {
    let db = db().await;
    let ctx_all = ctx(&db);
    let err = ctx_all
        .repo::<Orders>()
        .get_detail(i64::MAX)
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotFound), "{err:?}");

    let o = full_order(&db, "gf").await;
    let ctx = ctx_with(&db, &["entity.customers.view"], Setup::default());
    let err = ctx.repo::<Orders>().get_detail(o.id).await.unwrap_err();
    assert!(matches!(err, RepoError::Forbidden), "{err:?}");
}

#[tokio::test]
async fn levels_the_viewer_cannot_use_are_dropped_silently() {
    let db = db().await;
    let o = full_order(&db, "drop").await;

    // No view of tags: items stay, their tag level and the many-to-many drop.
    let ctx = ctx_with(
        &db,
        &[
            "entity.orders.*",
            "entity.order_items.*",
            "entity.customers.view",
            "entity.setting_order_tags.*",
        ],
        Setup::default(),
    );
    let d = ctx.repo::<Orders>().get_detail(o.id).await.unwrap();
    assert!(d.related(OrdersRepo::CUSTOMER).unwrap().is_some());
    let items = d.related(OrdersRepo::ITEMS).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].related(ItemsRepo::TAG).unwrap_err().name, "tag");
    assert_eq!(d.related(OrdersRepo::TAGS).unwrap_err().name, "tags");

    // No view of the join table drops the many-to-many only.
    let ctx = ctx_with(
        &db,
        &[
            "entity.orders.*",
            "entity.order_items.*",
            "entity.customers.view",
            "entity.master_tags.*",
        ],
        Setup::default(),
    );
    let d = ctx.repo::<Orders>().get_detail(o.id).await.unwrap();
    assert_eq!(
        tag_codes(d.related(OrdersRepo::ITEMS).unwrap()),
        ["drop_a", "-"]
    );
    assert_eq!(d.related(OrdersRepo::TAGS).unwrap_err().name, "tags");

    // A level below an unusable one goes with it, even if its own target is viewable.
    let ctx = ctx_with(
        &db,
        &[
            "entity.orders.*",
            "entity.customers.view",
            "entity.master_tags.*",
        ],
        Setup::default(),
    );
    let d = ctx.repo::<Orders>().get_detail(o.id).await.unwrap();
    assert_eq!(d.related(OrdersRepo::ITEMS).unwrap_err().name, "items");
    assert!(d.related(OrdersRepo::CUSTOMER).unwrap().is_some());
}

#[tokio::test]
async fn unchecked_keeps_every_level() {
    let db = db().await;
    let o = full_order(&db, "uc").await;
    let ctx = ctx_with(&db, &[], Setup::default());

    let d = ctx
        .repo::<Orders>()
        .unchecked()
        .get_detail(o.id)
        .await
        .unwrap();

    assert_eq!(
        tag_codes(d.related(OrdersRepo::ITEMS).unwrap()),
        ["uc_a", "-"]
    );
    assert_eq!(d.related(OrdersRepo::TAGS).unwrap().len(), 1);
    assert!(d.related(OrdersRepo::CUSTOMER).unwrap().is_some());
}

#[tokio::test]
async fn find_contain_default_applies_to_every_row() {
    let db = db().await;
    let ctx = ctx(&db);
    let o1 = full_order(&db, "fd1").await;
    let o2 = full_order(&db, "fd2").await;

    let rows = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain_default()
        .all()
        .await
        .unwrap();
    assert_eq!(
        tag_codes(rows[0].related(OrdersRepo::ITEMS).unwrap()),
        ["fd1_a", "-"]
    );
    assert_eq!(
        tag_codes(rows[1].related(OrdersRepo::ITEMS).unwrap()),
        ["fd2_a", "-"]
    );

    let customers = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.eq(o1.customer_id))
        .contain_default()
        .all()
        .await
        .unwrap();
    let orders = customers[0].related(CustomersRepo::ORDERS).unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(
        tag_code_list(orders[0].related(OrdersRepo::TAGS).unwrap()),
        ["fd1_b"]
    );
    // The customers' default has no items.
    assert!(orders[0].related(OrdersRepo::ITEMS).is_err());
    assert!(customers[0].related(CustomersRepo::BILLING).is_err());
}

#[tokio::test]
async fn explicit_contain_adds_to_the_default_but_not_twice() {
    let db = db().await;
    let ctx = ctx(&db);
    let o = full_order(&db, "mix").await;
    billing(&db, o.customer_id, "請求先").await;

    let customers = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.eq(o.customer_id))
        .contain_default()
        .contain(CustomersRepo::BILLING)
        .all()
        .await
        .unwrap();
    assert_eq!(
        customers[0].related(CustomersRepo::ORDERS).unwrap().len(),
        1
    );
    assert_eq!(
        customers[0]
            .related(CustomersRepo::BILLING)
            .unwrap()
            .unwrap()
            .billing_name,
        "請求先"
    );

    let res = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.eq(o.customer_id))
        .contain_default()
        .contain(CustomersRepo::ORDERS)
        .all()
        .await;
    assert!(is_db_error(&res), "{res:?}");

    let res = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::CUSTOMER)
        .contain_default()
        .all()
        .await;
    assert!(is_db_error(&res), "{res:?}");

    // A Repo without `default_contain` adds nothing, so an explicit contain is the only one.
    let items = ctx
        .repo::<Items>()
        .find()
        .filter(order_items::Column::OrderId.eq(o.id))
        .order_by_asc(order_items::Column::Seq)
        .contain_default()
        .contain(ItemsRepo::TAG)
        .all()
        .await
        .unwrap();
    assert_eq!(tag_codes(&items), ["mix_a", "-"]);

    let bare = ctx
        .repo::<Items>()
        .find()
        .filter(order_items::Column::OrderId.eq(o.id))
        .contain_default()
        .all()
        .await
        .unwrap();
    assert_eq!(bare[0].related(ItemsRepo::TAG).unwrap_err().name, "tag");
}

#[tokio::test]
async fn default_naming_an_association_declared_for_another_repo_type_fails_to_look_it_up() {
    let db = db().await;
    let o = full_order(&db, "bk").await;
    let ctx = ctx_with(
        &db,
        ALL,
        Setup {
            broken_default: true,
            ..Setup::default()
        },
    );

    // A checked read drops the level (its owner Repo is not the registered one); only `unchecked`
    // tries to look it up.
    let res = ctx
        .repo::<Customers>()
        .unchecked()
        .find()
        .filter(customers::Column::Id.eq(o.customer_id))
        .contain_default()
        .all()
        .await;

    match res {
        Err(RepoError::Db(DbErr::Custom(message))) => assert!(
            message.contains("cannot look up the typed declaration of association broken"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
}

// ---- conditions ----

#[tokio::test]
async fn has_many_takes_a_condition_and_an_order() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "x", 5, None, true).await;
    item(&db, o.id, "y", 1, None, true).await;
    item(&db, o.id, "z", 7, None, true).await;

    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::ITEMS.query(|q| {
            q.filter(order_items::Column::Seq.gte(5))
                .order_by_desc(order_items::Column::Seq)
        }),
    )
    .await;

    assert_eq!(
        item_names(row.related(OrdersRepo::ITEMS).unwrap()),
        ["z", "x"]
    );
}

#[tokio::test]
async fn query_and_with_combine_in_either_order() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let a = insert_tag(&db, "a", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "小", 1, Some(a.id), true).await;
    item(&db, o.id, "大", 9, Some(a.id), true).await;

    let check = |row: &Loaded<orders::Model>| {
        let items = row.related(OrdersRepo::ITEMS).unwrap();
        assert_eq!(item_names(items), ["大"]);
        assert_eq!(tag_codes(items), ["a"]);
    };
    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::ITEMS
            .query(|q| q.filter(order_items::Column::Seq.gte(5)))
            .with(ItemsRepo::TAG),
    )
    .await;
    check(&row);
    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::ITEMS
            .with(ItemsRepo::TAG)
            .query(|q| q.filter(order_items::Column::Seq.gte(5))),
    )
    .await;
    check(&row);

    // A nested child takes a query too (customers, orders, items).
    let customers = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.eq(c.id))
        .contain(
            CustomersRepo::ORDERS
                .with(OrdersRepo::ITEMS.query(|q| q.filter(order_items::Column::Seq.lt(5)))),
        )
        .all()
        .await
        .unwrap();
    let orders = customers[0].related(CustomersRepo::ORDERS).unwrap();
    assert_eq!(
        item_names(orders[0].related(OrdersRepo::ITEMS).unwrap()),
        ["小"]
    );
}

#[tokio::test]
async fn query_twice_ands_the_conditions_and_appends_the_order() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "d", 3, None, true).await;
    item(&db, o.id, "b", 5, None, true).await;
    item(&db, o.id, "a", 8, None, true).await;
    item(&db, o.id, "c", 5, None, true).await;

    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::ITEMS
            .query(|q| {
                q.filter(order_items::Column::Seq.gte(4))
                    .order_by_desc(order_items::Column::Seq)
            })
            .query(|q| {
                q.filter(order_items::Column::Seq.lte(5))
                    .order_by_desc(order_items::Column::Name)
            }),
    )
    .await;

    assert_eq!(
        item_names(row.related(OrdersRepo::ITEMS).unwrap()),
        ["c", "b"]
    );
}

#[tokio::test]
async fn a_query_cannot_widen_the_target_policy() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "生きている", 1, None, true).await;
    item(&db, o.id, "消した", 2, None, false).await;

    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::ITEMS.query(|q| q.filter(order_items::Column::IsActive.eq(false))),
    )
    .await;

    assert!(row.related(OrdersRepo::ITEMS).unwrap().is_empty());
}

#[tokio::test]
async fn belongs_to_is_none_when_the_condition_does_not_match() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;

    let hit = read_order(
        &ctx,
        o.id,
        OrdersRepo::CUSTOMER.query(|q| q.filter(customers::Column::Name.eq("顧客"))),
    )
    .await;
    assert!(hit.related(OrdersRepo::CUSTOMER).unwrap().is_some());

    let miss = read_order(
        &ctx,
        o.id,
        OrdersRepo::CUSTOMER.query(|q| q.filter(customers::Column::Name.eq("別の名前"))),
    )
    .await;
    assert!(miss.related(OrdersRepo::CUSTOMER).unwrap().is_none());
}

#[tokio::test]
async fn many_to_many_takes_a_condition_and_an_order_over_the_join_order() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let d = insert_tag(&db, "c", true).await;
    link(&db, o.id, b.id, 1, None).await;
    link(&db, o.id, a.id, 2, None).await;
    link(&db, o.id, d.id, 3, None).await;

    let plain = read_order(&ctx, o.id, OrdersRepo::TAGS).await;
    assert_eq!(
        tag_code_list(plain.related(OrdersRepo::TAGS).unwrap()),
        ["b", "a", "c"]
    );

    let row = read_order(
        &ctx,
        o.id,
        OrdersRepo::TAGS.query(|q| {
            q.filter(master_tags::Column::Code.ne("b"))
                .order_by_desc(master_tags::Column::Code)
        }),
    )
    .await;

    assert_eq!(
        tag_code_list(row.related(OrdersRepo::TAGS).unwrap()),
        ["c", "a"]
    );
}

#[tokio::test]
async fn load_applies_the_query_of_the_declaration() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "a", 2, None, true).await;
    item(&db, o.id, "b", 6, None, true).await;
    item(&db, o.id, "c", 9, None, true).await;

    let mut loaded = ctx
        .repo::<Orders>()
        .load(
            std::slice::from_ref(&o),
            OrdersRepo::ITEMS.query(|q| {
                q.filter(order_items::Column::Seq.gte(5))
                    .order_by_desc(order_items::Column::Seq)
            }),
        )
        .await
        .unwrap();

    let names: Vec<String> = loaded
        .remove(&o.id)
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert_eq!(names, ["c", "b"]);
}

// ---- JSON ----

#[tokio::test]
async fn json_of_a_row_with_nested_associations() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客A", true).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let o = order(&db, c.id, "注文1").await;
    let i1 = item(&db, o.id, "明細1", 1, Some(a.id), true).await;
    let i2 = item(&db, o.id, "明細2", 2, None, true).await;
    let i3 = item(&db, o.id, "明細3", 3, Some(b.id), true).await;
    let l = link(&db, o.id, a.id, 1, Some("メモ")).await;

    let row = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::CUSTOMER)
        .contain(OrdersRepo::ITEMS.with(ItemsRepo::TAG))
        .contain(OrdersRepo::TAGS)
        .all()
        .await
        .unwrap()
        .remove(0);

    let json = ctx.repo::<Orders>().to_json_one(&row).await.unwrap();

    let tag = |t: &master_tags::Model| {
        json!({
            "id": t.id, "code": t.code, "name": t.name, "is_active": true,
            "_label": t.name,
        })
    };
    let item_json = |i: &order_items::Model, tag_json: Value| {
        json!({
            "id": i.id, "name": i.name, "tag_id": i.tag_id, "seq": i.seq,
            "created": TS, "modified": TS,
            "_label": i.name, "tag": tag_json,
        })
    };
    let mut joined = tag(&a);
    joined["_joinData"] = json!({
        "id": l.id, "order_id": o.id, "tag_id": a.id, "seq": 1, "note": "メモ",
    });
    assert_eq!(
        json,
        json!({
            "id": o.id, "customer_id": c.id, "title": "注文1",
            "created": TS, "modified": TS,
            "customer": {
                "id": c.id, "name": "顧客A", "email": null,
                "created": TS, "modified": TS, "_label": "顧客A",
            },
            "items": [
                item_json(&i1, tag(&a)),
                item_json(&i2, Value::Null),
                item_json(&i3, tag(&b)),
            ],
            "tags": [joined],
        })
    );
}

#[tokio::test]
async fn json_of_has_one_leaves_out_the_foreign_key() {
    let db = db().await;
    let ctx = ctx(&db);
    let with = insert_customer(&db, "請求あり", true).await;
    let without = insert_customer(&db, "請求なし", true).await;
    billing(&db, with.id, "請求先").await;

    let rows = ctx
        .repo::<Customers>()
        .find()
        .filter(customers::Column::Id.is_in([with.id, without.id]))
        .order_by_asc(customers::Column::Id)
        .contain(CustomersRepo::BILLING)
        .all()
        .await
        .unwrap();
    let json = ctx.repo::<Customers>().to_json(&rows).await.unwrap();

    assert_eq!(json[0]["billing"]["billing_name"], "請求先");
    assert_eq!(json[0]["billing"]["_label"], "請求先");
    assert!(json[0]["billing"].get("customer_id").is_none(), "{json:?}");
    assert_eq!(json[1]["billing"], Value::Null);
}

#[tokio::test]
async fn json_of_get_detail_and_of_many_rows() {
    let db = db().await;
    let ctx = ctx(&db);
    let o1 = full_order(&db, "jd1").await;
    let o2 = full_order(&db, "jd2").await;

    let detail = ctx.repo::<Orders>().get_detail(o1.id).await.unwrap();
    let json = ctx.repo::<Orders>().to_json_one(&detail).await.unwrap();
    assert_eq!(json["title"], "jd1_注文");
    assert!(json.get("_label").is_none(), "{json}");
    assert_eq!(json["customer"]["_label"], "jd1_顧客");
    assert_eq!(json["items"][0]["tag"]["code"], "jd1_a");
    assert_eq!(json["items"][1]["tag"], Value::Null);
    assert_eq!(json["tags"][0]["code"], "jd1_b");

    let rows = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await
        .unwrap();
    let jsons = ctx.repo::<Orders>().to_json(&rows).await.unwrap();
    assert_eq!(jsons.len(), 2);
    assert_eq!(jsons[0]["title"], "jd1_注文");
    assert_eq!(jsons[1]["title"], "jd2_注文");
    for json in &jsons {
        assert_eq!(json["items"].as_array().unwrap().len(), 2);
        assert!(json.get("customer").is_none(), "{json}");
        assert!(json.get("tags").is_none(), "{json}");
    }
}

#[tokio::test]
async fn unreadable_columns_are_left_out_even_when_unchecked() {
    let db = db().await;
    let c = customer_with_email(&db, "顧客", "a@example.com").await;

    let ctx = ctx(&db);
    let model = ctx.repo::<Customers>().get(c.id).await.unwrap();
    let json = ctx
        .repo::<Customers>()
        .to_json_one(&Loaded::from(model.clone()))
        .await
        .unwrap();
    assert_eq!(json["email"], "a@example.com");

    let setup = Setup {
        hide_email: true,
        ..Setup::default()
    };
    let ctx = ctx_with(&db, ALL, setup);
    let json = ctx
        .repo::<Customers>()
        .to_json_one(&Loaded::from(model.clone()))
        .await
        .unwrap();
    assert_eq!(json["name"], "顧客");
    assert!(json.get("email").is_none(), "{json}");

    let json = ctx
        .repo::<Customers>()
        .unchecked()
        .to_json_one(&Loaded::from(model))
        .await
        .unwrap();
    assert!(json.get("email").is_none(), "{json}");
}

#[tokio::test]
async fn columns_readable_only_for_matching_rows_and_labels_that_fall_back_to_the_id() {
    let db = db().await;
    let open = customer_with_email(&db, "公開A", "a@example.com").await;
    let closed = customer_with_email(&db, "非公開B", "b@example.com").await;
    let setup = Setup {
        email_for_open_names: true,
        tag_name_unless_secret: true,
        ..Setup::default()
    };
    let ctx = ctx_with(&db, ALL, setup);

    let rows = [Loaded::from(open), Loaded::from(closed)];
    let jsons = ctx.repo::<Customers>().to_json(&rows).await.unwrap();
    assert_eq!(jsons[0]["email"], "a@example.com");
    assert!(jsons[1].get("email").is_none(), "{}", jsons[1]);
    assert_eq!(jsons[1]["name"], "非公開B");

    // The display field of a tag is unreadable for the "secret" tag, so its label is the id.
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    let shown = insert_tag(&db, "shown", true).await;
    let secret = insert_tag(&db, "secret", true).await;
    link(&db, o.id, shown.id, 1, None).await;
    link(&db, o.id, secret.id, 2, None).await;
    let row = read_order(&ctx, o.id, OrdersRepo::TAGS).await;
    let json = ctx.repo::<Orders>().to_json_one(&row).await.unwrap();
    assert_eq!(json["tags"][0]["name"], "shown の名前");
    assert_eq!(json["tags"][0]["_label"], "shown の名前");
    assert!(json["tags"][1].get("name").is_none(), "{json}");
    assert_eq!(json["tags"][1]["_label"], secret.id.to_string());
}

#[tokio::test]
async fn hidden_columns_are_never_output() {
    let db = db().await;
    let c = customer_with_email(&db, "顧客", "a@example.com").await;
    let o = order(&db, c.id, "注文").await;
    let setup = Setup {
        hidden_email_column: true,
        ..Setup::default()
    };
    let ctx = ctx_with(&db, &["*"], setup);

    let row = read_order(&ctx, o.id, OrdersRepo::CUSTOMER).await;
    let json = ctx.repo::<Orders>().to_json_one(&row).await.unwrap();
    assert_eq!(json["customer"]["name"], "顧客");
    assert!(json["customer"].get("email").is_none(), "{json}");

    let json = ctx
        .repo::<Customers>()
        .unchecked()
        .to_json_one(&Loaded::from(c))
        .await
        .unwrap();
    assert!(json.get("email").is_none(), "{json}");
    assert_eq!(json["name"], "顧客");
}

#[tokio::test]
async fn label_is_empty_for_null_and_the_text_of_a_number() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    item(&db, o.id, "明細", 3, None, true).await;
    let setup = Setup {
        customer_label_email: true,
        item_label_seq: true,
        ..Setup::default()
    };
    let ctx = ctx_with(&db, ALL, setup);

    let row = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::CUSTOMER)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await
        .unwrap()
        .remove(0);
    let json = ctx.repo::<Orders>().to_json_one(&row).await.unwrap();

    assert_eq!(json["customer"]["_label"], "");
    assert_eq!(json["items"][0]["_label"], "3");
}

#[tokio::test]
async fn many_to_many_json_carries_the_join_row_of_each_parent() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o1 = order(&db, c.id, "注文1").await;
    let o2 = order(&db, c.id, "注文2").await;
    let t = insert_tag(&db, "t", true).await;
    let u = insert_tag(&db, "u", true).await;
    let l1 = link(&db, o1.id, t.id, 1, Some("1 の組")).await;
    let l2 = link(&db, o2.id, t.id, 4, Some("2 の組")).await;
    let l3 = link(&db, o1.id, u.id, 2, None).await;

    let rows = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::TAGS)
        .all()
        .await
        .unwrap();
    let jsons = ctx.repo::<Orders>().to_json(&rows).await.unwrap();

    let first = &jsons[0]["tags"][0];
    assert_eq!(first["id"], t.id);
    assert_eq!(first["code"], "t");
    assert_eq!(
        first["_joinData"],
        json!({
            "id": l1.id, "order_id": o1.id, "tag_id": t.id, "seq": 1, "note": "1 の組",
        })
    );
    assert!(first.get("note").is_none(), "{first}");
    assert!(first.get("seq").is_none(), "{first}");
    assert_eq!(jsons[0]["tags"][1]["_joinData"]["id"], l3.id);
    assert_eq!(jsons[0]["tags"][1]["_joinData"]["note"], Value::Null);
    // The same tag under the other order carries that order's join row.
    assert_eq!(jsons[1]["tags"][0]["id"], t.id);
    assert_eq!(jsons[1]["tags"][0]["_joinData"]["id"], l2.id);
    assert_eq!(jsons[1]["tags"][0]["_joinData"]["note"], "2 の組");
    assert_eq!(jsons[1]["tags"][0]["_joinData"]["seq"], 4);
}

#[tokio::test]
async fn join_data_follows_the_columns_policy_of_the_join_table() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    let t = insert_tag(&db, "t", true).await;
    link(&db, o.id, t.id, 7, Some("秘密")).await;
    let setup = Setup {
        hide_note: true,
        ..Setup::default()
    };
    let ctx = ctx_with(&db, ALL, setup);

    let row = read_order(&ctx, o.id, OrdersRepo::TAGS).await;
    let json = ctx.repo::<Orders>().to_json_one(&row).await.unwrap();

    let join = &json["tags"][0]["_joinData"];
    assert!(join.get("note").is_none(), "{join}");
    assert_eq!(join["seq"], 7);
    assert_eq!(join["order_id"], o.id);
}

#[tokio::test]
async fn join_data_is_the_join_row_only_when_read_through_the_many_to_many() {
    let db = db().await;
    let ctx = ctx(&db);
    let c = insert_customer(&db, "顧客", true).await;
    let o = order(&db, c.id, "注文").await;
    let t = insert_tag(&db, "t", true).await;
    let l = link(&db, o.id, t.id, 4, Some("型付き")).await;

    let row = read_order(&ctx, o.id, OrdersRepo::TAGS).await;
    let tags = row.related(OrdersRepo::TAGS).unwrap();
    let join = tags[0].join_data(OrdersRepo::TAGS).unwrap();
    assert_eq!(join, &l);

    let plain = ctx
        .repo::<Tags>()
        .find()
        .filter(master_tags::Column::Id.eq(t.id))
        .contain_default()
        .all()
        .await
        .unwrap();
    assert!(plain[0].join_data(OrdersRepo::TAGS).is_none());
}

// ---- Loaded ----

#[tokio::test]
async fn loaded_gives_back_the_model_and_debug_lists_the_associations_by_name() {
    let db = db().await;
    let ctx = ctx(&db);
    let o = full_order(&db, "ld").await;

    let row = ctx
        .repo::<Orders>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::TAGS)
        .contain(OrdersRepo::CUSTOMER)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await
        .unwrap()
        .remove(0);

    let debug = format!("{row:?}");
    assert!(
        debug.contains(r#"related: ["customer", "items", "tags"]"#),
        "{debug}"
    );
    assert_eq!(row.into_model(), o);
}
