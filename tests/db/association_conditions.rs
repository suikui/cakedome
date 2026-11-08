//! The `filter` and `finder` of an association definition: they decide which rows are related, for
//! reads (`contain`, `load`, `load_where`, `matching_condition`) and for writes (`patch`,
//! `save_with`, `replace`, `replace_links`, reference checks, dependent deletes).

use std::collections::HashMap;

use cakedome::Context;
use cakedome::association::{
    Association, BelongsToOf, FinderFuture, HasManyOf, HasOneOf, LoadQuery, ManyToManyOf, SortOrder,
};
use cakedome::key::Key;
use cakedome::patch::{PatchError, Patching};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, GuardedRepo, Loaded, Repo, RepoError};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{customers, master_tags, order_items, orders, setting_order_tags};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Customers = DefaultRepo<customers::Entity>;
type Items = DefaultRepo<order_items::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;
type Links = DefaultRepo<setting_order_tags::Entity>;

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
];

fn big_items() -> Condition {
    Condition::all().add(order_items::Column::Name.ne("one"))
}

fn active_tags() -> Condition {
    Condition::all().add(master_tags::Column::IsActive.eq(true))
}

fn not_blocked() -> Condition {
    Condition::all().add(order_items::Column::Name.ne("blocked"))
}

/// Every item but "one" for a viewer without `finder.all_items`, all items for one with it; biggest
/// seq first. The conditions use `name` because a replace renumbers `seq`.
fn finder_items(ctx: &Context, q: LoadQuery<Items>) -> FinderFuture<'_, Items> {
    Box::pin(async move {
        let q = if ctx.identity().can("finder.all_items") {
            q
        } else {
            q.filter(order_items::Column::Name.ne("one"))
        };
        Ok(q.order_by_desc(order_items::Column::Seq))
    })
}

fn huge_items(_ctx: &Context, q: LoadQuery<Items>) -> FinderFuture<'_, Items> {
    Box::pin(async move { Ok(q.filter(order_items::Column::Name.eq("ten"))) })
}

fn small_items(_ctx: &Context, q: LoadQuery<Items>) -> FinderFuture<'_, Items> {
    Box::pin(async move { Ok(q.filter(order_items::Column::Name.ne("big"))) })
}

/// Tags whose code does not end with "b", by code descending.
fn not_b_tags(_ctx: &Context, q: LoadQuery<Tags>) -> FinderFuture<'_, Tags> {
    Box::pin(async move {
        Ok(q.filter(master_tags::Column::Code.not_like("%b"))
            .order_by_desc(master_tags::Column::Code))
    })
}

/// Customers whose name does not end with "ng".
fn usable_customers(_ctx: &Context, q: LoadQuery<Customers>) -> FinderFuture<'_, Customers> {
    Box::pin(async move { Ok(q.filter(customers::Column::Name.not_like("%ng"))) })
}

fn broken(_ctx: &Context, _q: LoadQuery<Items>) -> FinderFuture<'_, Items> {
    Box::pin(async move { Err(RepoError::Forbidden) })
}

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const USABLE_CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("usable_customer", orders::Column::CustomerId).finder(usable_customers);
    const ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("items", order_items::Column::OrderId).save(SaveStrategy::Replace);
    const TAGS: ManyToManyOf<Self, Links, Tags> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace);
    // Every item but "one", biggest seq first.
    const BIG_ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("big_items", order_items::Column::OrderId)
            .filter(big_items)
            .sort(&[(order_items::Column::Seq, SortOrder::Desc)])
            .save(SaveStrategy::Replace);
    // Active tags, by code descending.
    const ACTIVE_TAGS: ManyToManyOf<Self, Links, Tags> = ManyToManyOf::new(
        "active_tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .filter(active_tags)
    .sort(&[(master_tags::Column::Code, SortOrder::Desc)])
    .save(SaveStrategy::Replace);
    const FINDER_ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("finder_items", order_items::Column::OrderId)
            .finder(finder_items)
            .save(SaveStrategy::Replace)
            .dependent(true);
    const HUGE_ITEM: HasOneOf<Self, Items> =
        HasOneOf::new("huge_item", order_items::Column::OrderId).finder(huge_items);
    const BROKEN_ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("broken_items", order_items::Column::OrderId).finder(broken);
    const NOT_B_TAGS: ManyToManyOf<Self, Links, Tags> = ManyToManyOf::new(
        "not_b_tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .finder(not_b_tags)
    .save(SaveStrategy::Replace);
    // Both a filter and a finder: not named "blocked" and not named "big".
    const BOTH_ITEMS: HasManyOf<Self, Items> =
        HasManyOf::new("both_items", order_items::Column::OrderId)
            .filter(not_blocked)
            .finder(small_items)
            .save(SaveStrategy::Replace);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
            .association(Self::USABLE_CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::TAGS)
            .association(Self::BIG_ITEMS)
            .association(Self::ACTIVE_TAGS)
            .association(Self::FINDER_ITEMS)
            .association(Self::HUGE_ITEM)
            .association(Self::BROKEN_ITEMS)
            .association(Self::NOT_B_TAGS)
            .association(Self::BOTH_ITEMS)
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

fn build_registry() -> Registry {
    let mut r = registry();
    r.add(Tags::new());
    r.add(Links::new());
    r.add(Items::new());
    r.add(Customers::new());
    r.add(OrdersRepo);
    r.verify();
    r
}

fn ctx_with(db: &DatabaseConnection, patterns: &[&str]) -> Context {
    context(db, identity(patterns), build_registry())
}

fn ctx(db: &DatabaseConnection) -> Context {
    ctx_with(db, ALL)
}

fn ctx_all_items(db: &DatabaseConnection) -> Context {
    let mut patterns = ALL.to_vec();
    patterns.push("finder.all_items");
    ctx_with(db, &patterns)
}

async fn new_order(db: &DatabaseConnection, name: &str) -> orders::Model {
    let c = insert_customer(db, &format!("{name}の顧客"), true).await;
    orders::ActiveModel {
        customer_id: Set(c.id),
        title: Set(name.to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn item(db: &DatabaseConnection, order_id: i64, name: &str, seq: i32) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set(name.to_string()),
        seq: Set(seq),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn link(db: &DatabaseConnection, order_id: i64, tag_id: i64, seq: i32) {
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

/// Items named one / ten / five with seq 1 / 10 / 5.
async fn order_with_items(db: &DatabaseConnection, name: &str) -> orders::Model {
    let o = new_order(db, name).await;
    item(db, o.id, "one", 1).await;
    item(db, o.id, "ten", 10).await;
    item(db, o.id, "five", 5).await;
    o
}

/// Tags a (active), b (inactive) and c (active), linked in that order.
async fn order_with_tags(db: &DatabaseConnection, name: &str) -> orders::Model {
    let o = new_order(db, name).await;
    let a = insert_tag(db, "a", true).await;
    let b = insert_tag(db, "b", false).await;
    let c = insert_tag(db, "c", true).await;
    link(db, o.id, a.id, 1).await;
    link(db, o.id, b.id, 2).await;
    link(db, o.id, c.id, 3).await;
    o
}

fn item_names(items: &[order_items::Model]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

fn loaded_item_names(items: &[Loaded<order_items::Model>]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

fn loaded_tag_codes(tags: &[Loaded<master_tags::Model>]) -> Vec<&str> {
    tags.iter().map(|t| t.code.as_str()).collect()
}

async fn read_order(ctx: &Context, id: i64) -> orders::Model {
    ctx.repo::<OrdersRepo>().get(id).await.unwrap()
}

/// Items as (name, active) by id, deleted ones included. A replace renumbers `seq`, so it is left out.
async fn raw_items(db: &DatabaseConnection, order_id: i64) -> Vec<(String, bool)> {
    order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order_id))
        .order_by_asc(order_items::Column::Id)
        .all(db)
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.name, i.is_active))
        .collect()
}

fn row(name: &str, active: bool) -> (String, bool) {
    (name.to_string(), active)
}

async fn item_id(db: &DatabaseConnection, order_id: i64, name: &str) -> i64 {
    order_items::Entity::find()
        .filter(order_items::Column::OrderId.eq(order_id))
        .filter(order_items::Column::Name.eq(name))
        .one(db)
        .await
        .unwrap()
        .unwrap()
        .id
}

/// Codes of the linked tags by tag id.
async fn linked_codes(db: &DatabaseConnection, order_id: i64) -> Vec<String> {
    let links = setting_order_tags::Entity::find()
        .filter(setting_order_tags::Column::OrderId.eq(order_id))
        .order_by_asc(setting_order_tags::Column::TagId)
        .all(db)
        .await
        .unwrap();
    let mut codes = Vec::new();
    for l in links {
        let tag = master_tags::Entity::find_by_id(l.tag_id)
            .one(db)
            .await
            .unwrap()
            .unwrap();
        codes.push(tag.code);
    }
    codes
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

fn validation_pairs<T>(res: Result<T, RepoError>) -> Vec<(String, &'static str)> {
    match res {
        Err(RepoError::Validation(errors)) => errors
            .0
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |e| (k.clone(), e.code)))
            .collect(),
        other => panic!("expected Validation: {:?}", other.err()),
    }
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

// ---- filter: reads ----

#[tokio::test]
async fn contain_applies_the_has_many_filter_and_sort() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::BIG_ITEMS)
        .all()
        .await
        .unwrap();

    assert_eq!(
        loaded_item_names(rows[0].related(OrdersRepo::BIG_ITEMS).unwrap()),
        ["ten", "five"]
    );
}

#[tokio::test]
async fn load_applies_the_has_many_filter_and_sort() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    let items: HashMap<i64, Vec<order_items::Model>> = ctx
        .repo::<OrdersRepo>()
        .load(std::slice::from_ref(&o), OrdersRepo::BIG_ITEMS)
        .await
        .unwrap();

    assert_eq!(item_names(&items[&o.id]), ["ten", "five"]);
}

#[tokio::test]
async fn load_where_adds_to_the_has_many_filter_and_sorts_first() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);
    let repo = ctx.repo::<OrdersRepo>();

    let narrowed = repo
        .load_where(std::slice::from_ref(&o), OrdersRepo::BIG_ITEMS, |q| {
            q.filter(order_items::Column::Seq.lte(7))
        })
        .await
        .unwrap();
    let reordered = repo
        .load_where(std::slice::from_ref(&o), OrdersRepo::BIG_ITEMS, |q| {
            q.order_by_asc(order_items::Column::Seq)
        })
        .await
        .unwrap();

    assert_eq!(item_names(&narrowed[&o.id]), ["five"]);
    assert_eq!(item_names(&reordered[&o.id]), ["five", "ten"]);
}

#[tokio::test]
async fn contain_applies_the_many_to_many_filter_and_sort() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::ACTIVE_TAGS)
        .all()
        .await
        .unwrap();

    // Not the join table's seq order (a, c).
    assert_eq!(
        loaded_tag_codes(rows[0].related(OrdersRepo::ACTIVE_TAGS).unwrap()),
        ["c", "a"]
    );
}

#[tokio::test]
async fn load_applies_the_many_to_many_filter_and_sort() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    let ctx = ctx(&db);

    let tags = ctx
        .repo::<OrdersRepo>()
        .load(std::slice::from_ref(&o), OrdersRepo::ACTIVE_TAGS)
        .await
        .unwrap();

    let codes: Vec<&str> = tags[&o.id].iter().map(|t| t.code.as_str()).collect();
    assert_eq!(codes, ["c", "a"]);
}

#[tokio::test]
async fn associations_without_a_filter_are_unchanged() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    item(&db, o.id, "one", 1).await;
    item(&db, o.id, "ten", 10).await;
    item(&db, o.id, "five", 5).await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::TAGS)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await
        .unwrap();

    // Inactive tags are read too, in the join table's seq order.
    assert_eq!(
        loaded_tag_codes(rows[0].related(OrdersRepo::TAGS).unwrap()),
        ["a", "b", "c"]
    );
    assert_eq!(
        loaded_item_names(rows[0].related(OrdersRepo::ITEMS).unwrap()),
        ["one", "five", "ten"]
    );
}

// ---- filter: writes ----

#[tokio::test]
async fn save_with_replace_applies_the_condition() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ten = item_id(&db, o.id, "ten").await;
    let ctx = ctx(&db);

    ctx.repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace(
            OrdersRepo::BIG_ITEMS,
            vec![order_items::ActiveModel {
                id: Unchanged(ten),
                name: Set("ten2".to_string()),
                ..Default::default()
            }],
        )
        .save()
        .await
        .unwrap();

    assert_eq!(
        raw_items(&db, o.id).await,
        [row("one", true), row("ten2", true), row("five", false)],
        "the item outside the condition stays, the one in it that was not sent is deleted"
    );
}

#[tokio::test]
async fn replace_links_keeps_links_outside_the_condition() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    let d = insert_tag(&db, "d", true).await;
    let ctx = ctx(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"active_tags": [d.id]})),
    )
    .await
    .unwrap();

    assert_eq!(
        linked_codes(&db, o.id).await,
        ["b", "d"],
        "the link to inactive b stays; a and c, in the condition and not sent, are removed"
    );
}

#[tokio::test]
async fn the_condition_applies_even_when_unchecked() {
    let db = db().await;
    let o = new_order(&db, "注文").await;
    let inactive = insert_tag(&db, "x", false).await;
    let ctx = ctx(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .unchecked()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace_links(OrdersRepo::ACTIVE_TAGS, vec![inactive.id])
        .save()
        .await;

    assert_eq!(
        validation_pairs(res),
        [("active_tags.0".to_string(), "invalid_ref")]
    );
    assert!(linked_codes(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn which_links_belong_to_the_association_does_not_depend_on_the_viewer() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    // Cannot view tags, but can write the join table.
    let ctx = ctx_with(
        &db,
        &[
            "entity.orders.*",
            "entity.setting_order_tags.*",
            "entity.customers.view",
        ],
    );

    ctx.repo::<OrdersRepo>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace_links(OrdersRepo::ACTIVE_TAGS, vec![])
        .save()
        .await
        .unwrap();

    assert_eq!(linked_codes(&db, o.id).await, ["b"]);
}

// ---- finder: reads ----

#[tokio::test]
async fn contain_and_load_apply_the_finder_with_the_context() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::FINDER_ITEMS)
        .all()
        .await
        .unwrap();
    assert_eq!(
        loaded_item_names(rows[0].related(OrdersRepo::FINDER_ITEMS).unwrap()),
        ["ten", "five"]
    );
    let items = ctx
        .repo::<OrdersRepo>()
        .load(std::slice::from_ref(&o), OrdersRepo::FINDER_ITEMS)
        .await
        .unwrap();
    assert_eq!(item_names(&items[&o.id]), ["ten", "five"]);

    // The condition depends on who looks.
    let ctx = ctx_all_items(&db);
    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::FINDER_ITEMS)
        .all()
        .await
        .unwrap();
    assert_eq!(
        loaded_item_names(rows[0].related(OrdersRepo::FINDER_ITEMS).unwrap()),
        ["ten", "five", "one"]
    );
}

#[tokio::test]
async fn load_where_sorts_before_the_finder_and_ands_the_condition() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);
    let repo = ctx.repo::<OrdersRepo>();

    let reordered = repo
        .load_where(std::slice::from_ref(&o), OrdersRepo::FINDER_ITEMS, |q| {
            q.order_by_asc(order_items::Column::Seq)
        })
        .await
        .unwrap();
    let narrowed = repo
        .load_where(std::slice::from_ref(&o), OrdersRepo::FINDER_ITEMS, |q| {
            q.filter(order_items::Column::Seq.lte(7))
        })
        .await
        .unwrap();

    assert_eq!(item_names(&reordered[&o.id]), ["five", "ten"]);
    assert_eq!(item_names(&narrowed[&o.id]), ["five"]);
}

#[tokio::test]
async fn the_finder_applies_even_when_unchecked() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    let items = ctx
        .repo::<OrdersRepo>()
        .unchecked()
        .load(std::slice::from_ref(&o), OrdersRepo::FINDER_ITEMS)
        .await
        .unwrap();

    assert_eq!(item_names(&items[&o.id]), ["ten", "five"]);
}

#[tokio::test]
async fn has_one_and_many_to_many_apply_the_finder() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let small = new_order(&db, "小さい注文").await;
    item(&db, small.id, "小", 3).await;
    let tagged = order_with_tags(&db, "タグの注文").await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.is_in([o.id, small.id]))
        .order_by_asc(orders::Column::Id)
        .contain(OrdersRepo::HUGE_ITEM)
        .all()
        .await
        .unwrap();
    assert_eq!(
        rows[0]
            .related(OrdersRepo::HUGE_ITEM)
            .unwrap()
            .unwrap()
            .name,
        "ten"
    );
    assert!(rows[1].related(OrdersRepo::HUGE_ITEM).unwrap().is_none());

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(tagged.id))
        .contain(OrdersRepo::NOT_B_TAGS)
        .all()
        .await
        .unwrap();
    assert_eq!(
        loaded_tag_codes(rows[0].related(OrdersRepo::NOT_B_TAGS).unwrap()),
        ["c", "a"],
        "the tag coded b is outside, and the order is the finder's"
    );
}

#[tokio::test]
async fn belongs_to_contain_applies_the_finder() {
    let db = db().await;
    let ng = insert_customer(&db, "ng", true).await;
    let o = new_order(&db, "注文").await;
    let mut am: orders::ActiveModel = o.clone().into();
    am.customer_id = Set(ng.id);
    am.update(&db).await.unwrap();
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::USABLE_CUSTOMER)
        .contain(OrdersRepo::CUSTOMER)
        .all()
        .await
        .unwrap();

    assert!(
        rows[0]
            .related(OrdersRepo::USABLE_CUSTOMER)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rows[0].related(OrdersRepo::CUSTOMER).unwrap().unwrap().name,
        "ng"
    );
}

#[tokio::test]
async fn path_conditions_apply_the_finder() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);
    let by_item = |name: &'static str| {
        let ctx = &ctx;
        async move {
            let condition = OrdersRepo::FINDER_ITEMS
                .matching_condition(
                    ctx,
                    Condition::all().add(order_items::Column::Name.eq(name)),
                )
                .await
                .unwrap();
            order_ids(ctx, condition).await
        }
    };

    assert_eq!(by_item("ten").await, [o.id]);
    assert!(
        by_item("one").await.is_empty(),
        "\"one\" is outside the finder"
    );
}

#[tokio::test]
async fn a_failing_finder_fails_the_read() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    let res = ctx
        .repo::<OrdersRepo>()
        .load(std::slice::from_ref(&o), OrdersRepo::BROKEN_ITEMS)
        .await;

    assert!(matches!(res, Err(RepoError::Forbidden)), "{res:?}");
}

// ---- finder: writes ----

#[tokio::test]
async fn replace_keeps_children_outside_the_finder() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ten = item_id(&db, o.id, "ten").await;
    let ctx = ctx(&db);

    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"finder_items": [{"id": ten, "name": "ten2"}]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        raw_items(&db, o.id).await,
        [row("one", true), row("ten2", true), row("five", false)]
    );
}

#[tokio::test]
async fn a_child_outside_the_finder_is_not_found() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let one = item_id(&db, o.id, "one").await;
    let ctx = ctx(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"finder_items": [{"id": one}]})),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("finder_items.0.id", "not_found")]));
    assert_eq!(
        raw_items(&db, o.id).await,
        [row("one", true), row("ten", true), row("five", true)]
    );
}

#[tokio::test]
async fn replace_links_keeps_links_outside_the_finder_and_rejects_new_ones() {
    let db = db().await;
    let o = order_with_tags(&db, "注文").await;
    let a = insert_tag(&db, "a2", true).await;
    let ctx = ctx(&db);

    // b is outside the finder and stays; c, in it and not sent, is removed.
    run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"not_b_tags": [a.id]})),
    )
    .await
    .unwrap();
    assert_eq!(linked_codes(&db, o.id).await, ["b", "a2"]);

    let o = read_order(&ctx, o.id).await;
    let outside = insert_tag(&db, "xb", true).await;
    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"not_b_tags": [a.id, outside.id]})),
    )
    .await;
    assert_eq!(invalid(res), pairs(&[("not_b_tags.1", "invalid_ref")]));
}

#[tokio::test]
async fn belongs_to_reference_check_applies_the_finder() {
    let db = db().await;
    let o = new_order(&db, "注文").await;
    let ng = insert_customer(&db, "ng", true).await;
    let ok = insert_customer(&db, "ok", true).await;
    let ctx = ctx(&db);

    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"customer_id": ng.id})),
    )
    .await;
    assert_eq!(invalid(res), pairs(&[("customer_id", "invalid_ref")]));

    // The plain belongsTo on the same column does not reject it, so the other one is the finder's.
    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(o.modified, json!({"customer_id": ok.id})),
    )
    .await;
    assert!(res.is_ok(), "{:?}", res.err());
}

#[tokio::test]
async fn dependent_delete_applies_the_finder() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let ctx = ctx(&db);

    ctx.repo::<OrdersRepo>().delete(o.id).await.unwrap();

    assert_eq!(
        raw_items(&db, o.id).await,
        [row("one", true), row("ten", false), row("five", false)],
        "only the items in the finder are deleted"
    );
}

#[tokio::test]
async fn replace_and_replace_links_on_the_handle_apply_the_finder() {
    let db = db().await;
    let o = order_with_items(&db, "注文").await;
    let t = order_with_tags(&db, "タグの注文").await;
    let ten = item_id(&db, o.id, "ten").await;
    let a = insert_tag(&db, "a2", true).await;
    let outside = insert_tag(&db, "xb", true).await;
    let ctx = ctx(&db);
    let repo = ctx.repo::<OrdersRepo>();

    repo.replace(
        OrdersRepo::FINDER_ITEMS,
        &o,
        vec![order_items::ActiveModel {
            id: Unchanged(ten),
            ..Default::default()
        }],
    )
    .await
    .unwrap();
    repo.replace_links(OrdersRepo::NOT_B_TAGS, &t, vec![a.id])
        .await
        .unwrap();
    let res = repo
        .replace_links(OrdersRepo::NOT_B_TAGS, &t, vec![a.id, outside.id])
        .await;

    assert_eq!(
        raw_items(&db, o.id).await,
        [row("one", true), row("ten", true), row("five", false)]
    );
    assert_eq!(linked_codes(&db, t.id).await, ["b", "a2"]);
    assert_eq!(
        validation_pairs(res),
        [("not_b_tags.1".to_string(), "invalid_ref")]
    );
}

// ---- filter and finder together ----

#[tokio::test]
async fn a_filter_and_a_finder_both_narrow_reads_and_writes() {
    let db = db().await;
    let o = new_order(&db, "注文").await;
    let a = item(&db, o.id, "a", 1).await;
    let blocked = item(&db, o.id, "blocked", 2).await;
    let big = item(&db, o.id, "big", 20).await;
    item(&db, o.id, "b", 3).await;
    let ctx = ctx(&db);

    let rows = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(orders::Column::Id.eq(o.id))
        .contain(OrdersRepo::BOTH_ITEMS)
        .all()
        .await
        .unwrap();
    assert_eq!(
        loaded_item_names(rows[0].related(OrdersRepo::BOTH_ITEMS).unwrap()),
        ["a", "b"]
    );

    // An item outside either condition is not a current child.
    let res = run::<OrdersRepo>(
        &ctx,
        Some(o.id),
        &edit(
            o.modified,
            json!({"both_items": [{"id": blocked.id}, {"id": big.id}]}),
        ),
    )
    .await;
    assert_eq!(
        invalid(res),
        pairs(&[
            ("both_items.0.id", "not_found"),
            ("both_items.1.id", "not_found")
        ])
    );

    ctx.repo::<OrdersRepo>()
        .replace(
            OrdersRepo::BOTH_ITEMS,
            &o,
            vec![order_items::ActiveModel {
                id: Unchanged(a.id),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        raw_items(&db, o.id).await,
        [
            row("a", true),
            row("blocked", true),
            row("big", true),
            row("b", false)
        ],
        "only b, in both conditions and not sent, is deleted"
    );
}
