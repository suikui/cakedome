//! The operations on the target of an association (`layer::TargetOps`): taken from the
//! declaration, they run with the target Repo type, without looking the target up by name.

use std::collections::HashMap;

use cakedome::Context;
use cakedome::association::{Association, BelongsToOf, HasManyOf, HasOneOf, ManyToManyOf};
use cakedome::key::RowId;
use cakedome::layer::{ContainTree, NamedColumns, ops_of, target_ops, trace};
use cakedome::policy::{Action, ColumnAccess, EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::{Expr, LikeExpr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, DbBackend, EntityTrait,
    ExprTrait, QueryFilter, QueryTrait, Set, Value as DbValue,
};
use serde_json::{Map, Value, json};

use crate::common::registry;
use crate::entities::{
    customer_billings, customers, logs, master_tags, order_items, orders, setting_order_tags,
};
use crate::support::{context, db, identity, insert_customer, past};

const TS: &str = "2020-01-01T00:00:00.000000Z";

type Billings = DefaultRepo<customer_billings::Entity>;
type Items = DefaultRepo<order_items::Entity>;
type OrderTags = DefaultRepo<setting_order_tags::Entity>;
type Tags = DefaultRepo<master_tags::Entity>;

/// Hides `email`, makes `name` read-only, and leaves rows named `hidden…` out of scope.
struct CustomersRepo;

impl CustomersRepo {
    const BILLING: HasOneOf<Self, Billings> =
        HasOneOf::new("billing", customer_billings::Column::CustomerId);
}

impl Repo for CustomersRepo {
    type Entity = customers::Entity;

    fn initialize(e: RepoConfig<'_, customers::Entity>) -> RepoConfig<'_, customers::Entity> {
        e.display_field(customers::Column::Name)
            .association(Self::BILLING)
    }
}

#[async_trait]
impl TablePolicy for CustomersRepo {
    async fn scope(&self, _ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(Condition::all().add(customers::Column::Name.not_like("hidden%")))
    }

    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<customers::Entity>, RepoError> {
        Ok(ColumnAccess::all()
            .hide(customers::Column::Email)
            .read_only(customers::Column::Name))
    }
}

impl EntityPolicy for CustomersRepo {}

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, Items> = HasManyOf::new("items", order_items::Column::OrderId);
    const TAGS: ManyToManyOf<Self, OrderTags, Tags> = ManyToManyOf::new(
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
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

fn build() -> Registry {
    let mut r = registry();
    r.add(CustomersRepo);
    r.add(Billings::new())
        .display_field(customer_billings::Column::BillingName);
    r.add(Items::new());
    r.add(OrderTags::new());
    r.add(Tags::new());
    r.add(OrdersRepo);
    r.verify();
    r
}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.customers.*",
    "entity.customer_billings.*",
    "entity.order_items.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
];

fn ctx_with(db: &DatabaseConnection, patterns: &[&str]) -> Context {
    context(db, identity(patterns), build())
}

async fn insert_billing(
    db: &DatabaseConnection,
    customer_id: i64,
    name: &str,
) -> customer_billings::Model {
    customer_billings::ActiveModel {
        customer_id: Set(customer_id),
        billing_name: Set(name.to_string()),
        address: Set(None),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

/// The tree of `paths` on the customers, as the JSON layer builds it.
async fn customer_tree(ctx: &Context, paths: &[&str]) -> ContainTree {
    let meta = ctx.registry().meta("customers").unwrap();
    let mut tree = ContainTree::default();
    for path in paths {
        let (found, complete) = trace(ctx, meta, path).await.unwrap();
        assert!(complete, "{path}");
        tree.insert(&found);
    }
    tree
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

/// The SQL of the customers filtered by `expr`.
fn customers_sql(expr: Expr) -> String {
    customers::Entity::find()
        .filter(expr)
        .build(DbBackend::Sqlite)
        .to_string()
}

fn row(rows: &HashMap<RowId, Map<String, Value>>, id: i64) -> &Map<String, Value> {
    rows.get(&RowId::Int(id))
        .unwrap_or_else(|| panic!("row {id} is missing: {rows:?}"))
}

#[test]
fn each_kind_of_declaration_gives_the_operations_of_its_target() {
    let cases: [(&dyn Association, &str); 3] = [
        (&OrdersRepo::CUSTOMER, "customers"),
        (&OrdersRepo::ITEMS, "order_items"),
        (&OrdersRepo::TAGS, "master_tags"),
    ];
    for (relation, target) in cases {
        let ops = target_ops::<OrdersRepo>(relation)
            .unwrap_or_else(|| panic!("{} gives no operations", relation.name()));
        assert_eq!(ops.name(), target);
    }
    let billing = target_ops::<CustomersRepo>(&CustomersRepo::BILLING).unwrap();
    assert_eq!(billing.name(), "customer_billings");
}

#[test]
fn the_registered_associations_give_the_operations_of_their_targets() {
    let r = build();
    let meta = r.meta("orders").unwrap();
    assert_eq!(meta.relations.len(), 3);
    for relation in &meta.relations {
        let ops = target_ops::<OrdersRepo>(relation.as_ref()).unwrap();
        assert_eq!(ops.name(), relation.target());
    }
}

#[test]
fn another_owner_type_gives_no_operations() {
    assert!(target_ops::<CustomersRepo>(&OrdersRepo::CUSTOMER).is_none());
    assert!(target_ops::<OrdersRepo>(&CustomersRepo::BILLING).is_none());
}

#[test]
fn only_many_to_many_gives_the_join_table() {
    let join = OrdersRepo::TAGS.join_ops().unwrap();
    assert_eq!(join.name(), "setting_order_tags");
    assert!(OrdersRepo::CUSTOMER.join_ops().is_none());
    assert!(OrdersRepo::ITEMS.join_ops().is_none());
    assert!(CustomersRepo::BILLING.join_ops().is_none());
}

#[test]
fn ops_of_a_repo_type_is_named_after_its_table() {
    assert_eq!(ops_of::<Tags>().name(), "master_tags");
    assert_eq!(ops_of::<OrdersRepo>().name(), "orders");
}

#[tokio::test]
async fn can_follows_the_policy_of_the_target() {
    let db = db().await;
    let ctx = ctx_with(&db, &["entity.customers.view"]);
    let customers = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    assert!(customers.can(&ctx, Action::View).await.unwrap());
    assert!(!customers.can(&ctx, Action::Add).await.unwrap());
    assert!(!customers.can(&ctx, Action::Delete).await.unwrap());

    let join = OrdersRepo::TAGS.join_ops().unwrap();
    assert!(!join.can(&ctx, Action::View).await.unwrap());
    let ctx = ctx_with(&db, &["entity.setting_order_tags.view"]);
    assert!(join.can(&ctx, Action::View).await.unwrap());
}

#[tokio::test]
async fn columns_follow_the_columns_policy_of_the_target() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let meta = ctx.registry().meta("customers").unwrap();
    let field = |name: &str| meta.fields.iter().find(|f| f.name == name).unwrap();

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let columns = ops.columns(&ctx).await.unwrap();
    assert!(!columns.table_readable(field("email")));
    assert!(!columns.table_writable(field("email")));
    assert!(columns.table_readable(field("name")));
    assert!(!columns.table_writable(field("name")));
    assert!(columns.table_readable(field("id")));

    let items_meta = ctx.registry().meta("order_items").unwrap();
    let name = items_meta.fields.iter().find(|f| f.name == "name").unwrap();
    let items = target_ops::<OrdersRepo>(&OrdersRepo::ITEMS).unwrap();
    let columns = items.columns(&ctx).await.unwrap();
    assert!(columns.table_readable(name));
    assert!(columns.table_writable(name));
}

#[tokio::test]
async fn rows_json_reads_the_rows_by_id_with_labels_and_the_tree() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let with = insert_customer(&db, "顧客A", true).await;
    let without = insert_customer(&db, "顧客B", true).await;
    let billing = insert_billing(&db, with.id, "請求先A").await;
    let tree = customer_tree(&ctx, &["billing"]).await;

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let rows = ops
        .rows_json(&ctx, &[RowId::Int(with.id), RowId::Int(without.id)], &tree)
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(
        Value::Object(row(&rows, with.id).clone()),
        json!({
            "id": with.id, "name": "顧客A", "created": TS, "modified": TS,
            "_label": "顧客A",
            "billing": {
                "id": billing.id, "billing_name": "請求先A", "address": null,
                "created": TS, "modified": TS, "_label": "請求先A",
            },
        })
    );
    let other = row(&rows, without.id);
    assert_eq!(other["_label"], "顧客B");
    assert_eq!(other["billing"], Value::Null);
    assert!(other.get("email").is_none(), "{other:?}");
}

#[tokio::test]
async fn rows_json_without_a_tree_has_no_associations() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let c = insert_customer(&db, "顧客A", true).await;
    insert_billing(&db, c.id, "請求先A").await;

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let rows = ops
        .rows_json(&ctx, &[RowId::Int(c.id)], &ContainTree::default())
        .await
        .unwrap();
    assert!(row(&rows, c.id).get("billing").is_none(), "{rows:?}");
}

#[tokio::test]
async fn rows_json_leaves_out_rows_outside_the_scope_soft_deleted_or_missing() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let shown = insert_customer(&db, "顧客A", true).await;
    let out_of_scope = insert_customer(&db, "hidden 顧客", true).await;
    let deleted = insert_customer(&db, "削除済み", false).await;

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let rows = ops
        .rows_json(
            &ctx,
            &[
                RowId::Int(shown.id),
                RowId::Int(out_of_scope.id),
                RowId::Int(deleted.id),
                RowId::Int(deleted.id + 1000),
            ],
            &ContainTree::default(),
        )
        .await
        .unwrap();
    assert_eq!(rows.keys().collect::<Vec<_>>(), vec![&RowId::Int(shown.id)]);
}

#[tokio::test]
async fn rows_json_skips_ids_of_another_key_type_and_reads_nothing_for_none() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let c = insert_customer(&db, "顧客A", true).await;

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let rows = ops
        .rows_json(
            &ctx,
            &[RowId::Str(c.id.to_string())],
            &ContainTree::default(),
        )
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");

    // Without a view permission, an empty `ids` still succeeds: nothing is read.
    let no_view = ctx_with(&db, &[]);
    let rows = ops
        .rows_json(&no_view, &[], &ContainTree::default())
        .await
        .unwrap();
    assert!(rows.is_empty());
}

#[tokio::test]
async fn rows_json_needs_the_view_permission_of_the_target() {
    let db = db().await;
    let ctx = ctx_with(&db, &["entity.orders.*"]);
    let c = insert_customer(&db, "顧客A", true).await;

    let ops = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let err = ops
        .rows_json(&ctx, &[RowId::Int(c.id)], &ContainTree::default())
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::Forbidden), "{err:?}");
}

#[tokio::test]
async fn rows_json_of_an_unregistered_repo_is_not_registered() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let err = ops_of::<DefaultRepo<logs::Entity>>()
        .rows_json(&ctx, &[RowId::Int(1)], &ContainTree::default())
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::NotRegistered(_)), "{err:?}");
}

#[test]
fn target_of_gives_the_target_of_an_association_of_the_target() {
    let customers = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let billing = customers.target_of(&CustomersRepo::BILLING).unwrap();
    assert_eq!(billing.name(), "customer_billings");

    // An association of another Repo is not one of the target's.
    assert!(customers.target_of(&OrdersRepo::ITEMS).is_none());
    assert!(
        ops_of::<OrdersRepo>()
            .target_of(&CustomersRepo::BILLING)
            .is_none()
    );
}

#[test]
fn target_of_follows_the_registered_associations_of_the_target() {
    let r = build();
    let customer = r
        .meta("orders")
        .unwrap()
        .relations
        .iter()
        .find(|relation| relation.name() == "customer")
        .unwrap();
    let customers = ops_of::<OrdersRepo>().target_of(customer.as_ref()).unwrap();
    assert_eq!(customers.name(), "customers");

    let relations = &r.meta("customers").unwrap().relations;
    assert!(!relations.is_empty());
    for relation in relations {
        let ops = customers.target_of(relation.as_ref()).unwrap();
        assert_eq!(ops.name(), relation.target());
    }
}

#[test]
fn column_builds_the_same_sql_as_the_typed_column() {
    let ops = ops_of::<CustomersRepo>();
    let id = ops.column("id").unwrap();
    let name = ops.column("name").unwrap();
    let email = ops.column("email").unwrap();
    let text = |s: &str| name.value(DbValue::from(s));
    let int = |n: i64| id.value(DbValue::from(n));
    let pattern = || LikeExpr::new("%a\\_%").escape('\\');

    let cases = [
        (name.expr().eq(text("A")), customers::Column::Name.eq("A")),
        (name.expr().ne(text("A")), customers::Column::Name.ne("A")),
        (id.expr().gt(int(1)), customers::Column::Id.gt(1i64)),
        (id.expr().lte(int(1)), customers::Column::Id.lte(1i64)),
        (
            id.expr().between(int(1), int(3)),
            customers::Column::Id.between(1i64, 3i64),
        ),
        (
            name.expr().is_in([text("A"), text("B")]),
            customers::Column::Name.is_in(["A", "B"]),
        ),
        (
            name.expr().is_not_in([text("A")]),
            customers::Column::Name.is_not_in(["A"]),
        ),
        (email.expr().is_null(), customers::Column::Email.is_null()),
        (
            email.expr().is_not_null(),
            customers::Column::Email.is_not_null(),
        ),
        (
            name.expr().like(pattern()),
            customers::Column::Name.into_expr().like(pattern()),
        ),
    ];
    for (erased, typed) in cases {
        assert_eq!(customers_sql(erased), customers_sql(typed));
    }
}

#[test]
fn column_of_a_missing_name_is_none() {
    assert!(ops_of::<CustomersRepo>().column("missing").is_none());
    assert!(ops_of::<CustomersRepo>().column("billing").is_none());
}

#[test]
fn read_scope_is_the_read_where_condition_of_the_column() {
    let r = build();
    let meta = r.meta("customers").unwrap();
    let field = |name: &str| meta.fields.iter().find(|f| f.name == name).unwrap();
    let only_a = Condition::all().add(customers::Column::Name.eq("顧客A"));
    let access = ColumnAccess::<customers::Entity>::all()
        .read_where(customers::Column::Email, only_a.clone())
        .read_only(customers::Column::Name);

    let columns: &dyn NamedColumns = &access;
    assert_eq!(columns.read_scope(field("email")), Some(only_a));
    assert_eq!(columns.read_scope(field("name")), None);
    assert_eq!(columns.read_scope(field("id")), None);
}

#[tokio::test]
async fn column_target_of_and_matching_condition_filter_through_two_associations() {
    let db = db().await;
    let ctx = ctx_with(&db, ALL);
    let a = insert_customer(&db, "顧客A", true).await;
    let b = insert_customer(&db, "顧客B", true).await;
    insert_customer(&db, "顧客C", true).await;
    insert_billing(&db, a.id, "請求先A").await;
    insert_billing(&db, b.id, "請求先B").await;
    insert_order(&db, a.id, "注文A").await;
    insert_order(&db, b.id, "注文B1").await;
    insert_order(&db, b.id, "注文B2").await;

    let customers = target_ops::<OrdersRepo>(&OrdersRepo::CUSTOMER).unwrap();
    let billings = customers.target_of(&CustomersRepo::BILLING).unwrap();
    let billing_name = billings.column("billing_name").unwrap();
    let on_billing =
        Condition::all().add(billing_name.expr().eq(billing_name.value("請求先B".into())));
    let on_customer = CustomersRepo::BILLING
        .matching_condition(&ctx, on_billing)
        .await
        .unwrap();
    let on_order = OrdersRepo::CUSTOMER
        .matching_condition(&ctx, on_customer)
        .await
        .unwrap();

    let mut titles: Vec<String> = ctx
        .repo::<OrdersRepo>()
        .find()
        .filter(on_order)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.title)
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["注文B1", "注文B2"]);
}
