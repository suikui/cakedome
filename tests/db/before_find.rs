//! Where `before_find` (adds conditions before reading) applies, and the re-entry error.

use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::association::{Association, BelongsToOf, HasManyOf};
use cakedome::policy::{Action, EntityPolicy, TablePolicy};
use cakedome::registry::RepoConfig;
use cakedome::repo::{DefaultRepo, FindOptions, FindQuery, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::Values;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, Order};

use crate::common::registry;
use crate::entities::{customers, master_tags, order_items, orders};
use crate::support::{Log, context, db, identity, insert_customer, insert_tag, past};

/// Hides customers named `hidden_*`, recording each operation. `OnlyName` narrows to that name.
struct Hiding {
    log: Log,
}

struct OnlyName(&'static str);

#[async_trait]
impl Repo for Hiding {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<customers::Entity>,
        options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        self.log.push(format!("{:?}", query.action()));
        query.filter(
            Condition::all()
                .add(customers::Column::Name.starts_with("hidden_"))
                .not(),
        );
        if let Some(OnlyName(name)) = options.get::<OnlyName>() {
            query.filter(customers::Column::Name.eq(*name));
        }
        Ok(())
    }
}

impl TablePolicy for Hiding {}
impl EntityPolicy for Hiding {}

/// Reads the same Entity inside `before_find`.
struct Looping;

#[async_trait]
impl Repo for Looping {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        ctx: &Context,
        _query: &mut FindQuery<customers::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        ctx.repo::<Self>().find().all().await?;
        Ok(())
    }
}

impl TablePolicy for Looping {}
impl EntityPolicy for Looping {}

fn hiding(db: &DatabaseConnection) -> (Context, Log) {
    let log = Log::default();
    let mut r = registry();
    r.add(Hiding { log: log.clone() });
    (context(db, identity(&["entity.customers.*"]), r), log)
}

fn names(rows: &[customers::Model]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn before_find_applies_to_all_count_and_get_even_unchecked() {
    let db = db().await;
    insert_customer(&db, "shown", true).await;
    let hidden = insert_customer(&db, "hidden_a", true).await;
    let (ctx, _) = hiding(&db);
    let repo = ctx.repo::<Hiding>();

    assert_eq!(names(&repo.find().all().await.unwrap()), ["shown"]);
    assert_eq!(repo.find().count().await.unwrap(), 1);
    let res = repo.get(hidden.id).await;
    assert!(matches!(res, Err(RepoError::NotFound)), "{res:?}");
    let unchecked = ctx.repo::<Hiding>().unchecked();
    assert_eq!(names(&unchecked.find().all().await.unwrap()), ["shown"]);
}

#[tokio::test]
async fn edit_and_delete_read_the_row_through_before_find() {
    let db = db().await;
    let hidden = insert_customer(&db, "hidden_a", true).await;
    let (ctx, log) = hiding(&db);
    let repo = ctx.repo::<Hiding>();

    let edit = repo
        .save(customers::ActiveModel {
            id: Unchanged(hidden.id),
            name: Set("renamed".to_string()),
            ..Default::default()
        })
        .await;
    let delete = repo.delete(hidden.id).await;

    assert!(matches!(edit, Err(RepoError::NotFound)), "{edit:?}");
    assert!(matches!(delete, Err(RepoError::NotFound)), "{delete:?}");
    assert_eq!(log.take(), ["Edit", "Delete"]);
}

#[tokio::test]
async fn options_reach_before_find() {
    let db = db().await;
    insert_customer(&db, "apple", true).await;
    insert_customer(&db, "banana", true).await;
    let (ctx, _) = hiding(&db);

    let rows = ctx
        .repo::<Hiding>()
        .find()
        .option(OnlyName("banana"))
        .all()
        .await
        .unwrap();

    assert_eq!(names(&rows), ["banana"]);
}

#[tokio::test]
async fn reading_the_same_entity_inside_before_find_is_an_error() {
    let db = db().await;
    let mut r = registry();
    r.add(Looping);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let res = ctx.repo::<Looping>().find().all().await;

    match res {
        Err(RepoError::BeforeFindReentered(name)) => assert_eq!(name, "customers"),
        other => panic!("expected BeforeFindReentered: {other:?}"),
    }
}

// ---- hooks with a call record, on associations and options ----

/// Passed by the caller to read hidden rows too.
struct ShowHidden;

/// The (action, primary) of each `before_find` call.
type Calls = Arc<Mutex<Vec<(Action, bool)>>>;

/// Hides customers whose name ends with "hidden" and orders by name descending. A tag coded "off"
/// switches hiding off, which shows the hook can read other entities.
struct BfCustomers {
    calls: Calls,
}

#[async_trait]
impl Repo for BfCustomers {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        ctx: &Context,
        query: &mut FindQuery<customers::Entity>,
        options: &FindOptions,
        primary: bool,
    ) -> Result<(), RepoError> {
        self.calls.lock().unwrap().push((query.action(), primary));
        let off = ctx
            .repo::<DefaultRepo<master_tags::Entity>>()
            .unchecked()
            .find()
            .filter(master_tags::Column::Code.eq("off"))
            .count()
            .await?
            > 0;
        query.order_by_desc(customers::Column::Name);
        if off || options.contains::<ShowHidden>() {
            return Ok(());
        }
        query.filter(customers::Column::Name.not_like("%hidden"));
        Ok(())
    }
}

impl TablePolicy for BfCustomers {}
impl EntityPolicy for BfCustomers {}

/// Hides items whose name ends with "hidden".
struct BfItems;

#[async_trait]
impl Repo for BfItems {
    type Entity = order_items::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<order_items::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        query.filter(order_items::Column::Name.not_like("%hidden"));
        Ok(())
    }
}

impl TablePolicy for BfItems {}
impl EntityPolicy for BfItems {}

struct BfOrders;

impl BfOrders {
    const CUSTOMER: BelongsToOf<Self, BfCustomers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, BfItems> =
        HasManyOf::new("items", order_items::Column::OrderId).dependent(true);
}

impl Repo for BfOrders {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER).association(Self::ITEMS)
    }
}

impl TablePolicy for BfOrders {}
impl EntityPolicy for BfOrders {}

/// Hides tags whose code ends with "hidden".
struct BfTags;

#[async_trait]
impl Repo for BfTags {
    type Entity = master_tags::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<master_tags::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        query.filter(master_tags::Column::Code.not_like("%hidden"));
        Ok(())
    }
}

impl TablePolicy for BfTags {}
impl EntityPolicy for BfTags {}

/// Orders ascending through `Order::Field`, which `FindQuery::order_by` treats as ascending.
struct FieldOrdered;

#[async_trait]
impl Repo for FieldOrdered {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<customers::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        query.order_by(customers::Column::Name, Order::Field(Values(Vec::new())));
        Ok(())
    }
}

impl TablePolicy for FieldOrdered {}
impl EntityPolicy for FieldOrdered {}

const BF_ALL: &[&str] = &[
    "entity.customers.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.master_tags.*",
];

fn bf_context(db: &DatabaseConnection) -> (Context, Calls) {
    let calls = Calls::default();
    let mut r = registry();
    r.add(BfCustomers {
        calls: calls.clone(),
    });
    r.add(BfOrders);
    r.add(BfItems);
    r.add(DefaultRepo::<master_tags::Entity>::new());
    r.verify();
    (context(db, identity(BF_ALL), r), calls)
}

fn taken(calls: &Calls) -> Vec<(Action, bool)> {
    std::mem::take(&mut *calls.lock().unwrap())
}

async fn bf_order(db: &DatabaseConnection, customer_id: i64, title: &str) -> orders::Model {
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

async fn bf_item(db: &DatabaseConnection, order_id: i64, name: &str) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set(name.to_string()),
        seq: Set(1),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn customer_created(
    db: &DatabaseConnection,
    name: &str,
    created: chrono::DateTime<chrono::Utc>,
) {
    customers::ActiveModel {
        name: Set(name.to_string()),
        is_active: Set(true),
        created: Set(created),
        modified: Set(created),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

fn bf_names(rows: Vec<customers::Model>) -> Vec<String> {
    rows.into_iter().map(|c| c.name).collect()
}

#[tokio::test]
async fn before_find_orders_come_after_the_callers() {
    let db = db().await;
    let later = past() + chrono::Duration::days(1);
    customer_created(&db, "a", past()).await;
    customer_created(&db, "c", past()).await;
    customer_created(&db, "b", later).await;
    customer_created(&db, "d", later).await;
    let (ctx, _) = bf_context(&db);
    let repo = ctx.repo::<BfCustomers>();

    let plain = bf_names(repo.find().all().await.unwrap());
    let by_created = bf_names(
        repo.find()
            .order_by_asc(customers::Column::Created)
            .all()
            .await
            .unwrap(),
    );

    assert_eq!(plain, ["d", "c", "b", "a"]);
    assert_eq!(by_created, ["c", "a", "d", "b"]);
}

#[tokio::test]
async fn before_find_orders_other_than_desc_are_ascending() {
    let db = db().await;
    for name in ["b", "c", "a"] {
        insert_customer(&db, name, true).await;
    }
    let mut r = registry();
    r.add(FieldOrdered);
    let ctx = context(&db, identity(&["entity.customers.*"]), r);

    let rows = ctx.repo::<FieldOrdered>().find().all().await.unwrap();

    assert_eq!(bf_names(rows), ["a", "b", "c"]);
}

#[tokio::test]
async fn before_find_can_read_other_entities() {
    let db = db().await;
    insert_customer(&db, "a", true).await;
    insert_customer(&db, "x_hidden", true).await;
    let (ctx, _) = bf_context(&db);
    let repo = ctx.repo::<BfCustomers>();

    assert_eq!(bf_names(repo.find().all().await.unwrap()), ["a"]);

    insert_tag(&db, "off", true).await;
    assert_eq!(
        bf_names(repo.find().all().await.unwrap()),
        ["x_hidden", "a"]
    );
}

#[tokio::test]
async fn options_on_the_handle_reach_get_and_the_finds_made_from_it() {
    let db = db().await;
    insert_customer(&db, "a", true).await;
    let hidden = insert_customer(&db, "x_hidden", true).await;
    let (ctx, _) = bf_context(&db);

    let plain = ctx.repo::<BfCustomers>();
    let got_without = plain.get(hidden.id).await;
    let handle = ctx.repo::<BfCustomers>().option(ShowHidden);
    let got = handle.get(hidden.id).await;
    let inherited = handle.find().count().await.unwrap();

    assert!(
        matches!(got_without, Err(RepoError::NotFound)),
        "{got_without:?}"
    );
    assert!(got.is_ok(), "{got:?}");
    assert_eq!(inherited, 2);
    assert_eq!(plain.find().count().await.unwrap(), 1);
}

#[tokio::test]
async fn concurrent_reads_are_not_reentry() {
    let db = db().await;
    insert_customer(&db, "a", true).await;
    let (ctx, _) = bf_context(&db);
    let repo = ctx.repo::<BfCustomers>();

    let (rows, count) = tokio::join!(repo.find().all(), repo.find().count());

    assert_eq!(rows.unwrap().len(), 1);
    assert_eq!(count.unwrap(), 1);
}

#[tokio::test]
async fn contain_applies_the_target_before_find_as_not_primary() {
    let db = db().await;
    let shown = insert_customer(&db, "a", true).await;
    let hidden = insert_customer(&db, "x_hidden", true).await;
    let o1 = bf_order(&db, shown.id, "1").await;
    let o2 = bf_order(&db, hidden.id, "2").await;
    let (ctx, calls) = bf_context(&db);

    let rows = ctx
        .repo::<BfOrders>()
        .find()
        .filter(orders::Column::Id.is_in([o1.id, o2.id]))
        .order_by_asc(orders::Column::Id)
        .contain(BfOrders::CUSTOMER)
        .all()
        .await
        .unwrap();

    let customer = rows[0].related(BfOrders::CUSTOMER).unwrap().unwrap();
    assert_eq!(customer.name, "a");
    assert!(rows[1].related(BfOrders::CUSTOMER).unwrap().is_none());
    let calls = taken(&calls);
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|c| *c == (Action::View, false)),
        "{calls:?}"
    );
}

#[tokio::test]
async fn relation_conditions_apply_the_target_before_find_as_not_primary() {
    let db = db().await;
    let shown = insert_customer(&db, "a", true).await;
    let hidden = insert_customer(&db, "x_hidden", true).await;
    bf_order(&db, shown.id, "1").await;
    bf_order(&db, hidden.id, "2").await;
    let (ctx, calls) = bf_context(&db);
    let count_by_name = |name: &'static str| {
        let ctx = &ctx;
        async move {
            let condition = BfOrders::CUSTOMER
                .matching_condition(ctx, Condition::all().add(customers::Column::Name.eq(name)))
                .await
                .unwrap();
            ctx.repo::<BfOrders>()
                .find()
                .filter(condition)
                .count()
                .await
                .unwrap()
        }
    };

    assert_eq!(count_by_name("a").await, 1);
    assert_eq!(count_by_name("x_hidden").await, 0);

    let calls = taken(&calls);
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|c| *c == (Action::View, false)),
        "{calls:?}"
    );
}

#[tokio::test]
async fn belongs_to_reference_check_applies_the_target_before_find() {
    let db = db().await;
    let shown = insert_customer(&db, "a", true).await;
    let hidden = insert_customer(&db, "x_hidden", true).await;
    let o = bf_order(&db, shown.id, "1").await;
    let (ctx, calls) = bf_context(&db);

    let saved = ctx
        .repo::<BfOrders>()
        .save(orders::ActiveModel {
            id: Unchanged(o.id),
            customer_id: Set(hidden.id),
            ..Default::default()
        })
        .await;

    match saved {
        Err(RepoError::Validation(errors)) => {
            assert_eq!(errors.0["customer_id"][0].code, "invalid_ref")
        }
        other => panic!("expected Validation: {other:?}"),
    }
    assert_eq!(taken(&calls), vec![(Action::View, false)]);
}

#[tokio::test]
async fn the_unique_check_ignores_before_find() {
    let db = db().await;
    insert_tag(&db, "dup_hidden", true).await;
    let mut r = registry();
    r.add(BfTags);
    let ctx = context(&db, identity(&["entity.master_tags.*"]), r);

    let saved = ctx
        .repo::<BfTags>()
        .save(master_tags::ActiveModel {
            code: Set("dup_hidden".to_string()),
            name: Set("2".to_string()),
            ..Default::default()
        })
        .await;

    match saved {
        Err(RepoError::Validation(errors)) => assert_eq!(errors.0["code"][0].code, "unique"),
        other => panic!("expected Validation: {other:?}"),
    }
}

#[tokio::test]
async fn dependent_delete_ignores_the_child_before_find() {
    let db = db().await;
    let c = insert_customer(&db, "a", true).await;
    let o = bf_order(&db, c.id, "1").await;
    let shown = bf_item(&db, o.id, "a").await;
    let hidden = bf_item(&db, o.id, "x_hidden").await;
    let (ctx, _) = bf_context(&db);

    let deleted = ctx.repo::<BfOrders>().delete(o.id).await;

    assert!(deleted.is_ok(), "{deleted:?}");
    for id in [shown.id, hidden.id] {
        let row = order_items::Entity::find_by_id(id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert!(!row.is_active, "item {id} is deleted too");
    }
}
