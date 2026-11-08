//! Default values looked up in another table (`RefBy`), the association trees of `cakedome::layer`,
//! `load` with a declaration that does not match the registration, and anonymous identities.

use std::sync::Arc;

use cakedome::Context;
use cakedome::association::{BelongsToOf, HasManyOf, ManyToManyOf};
use cakedome::defaults::{DynamicDefault, RefBy};
use cakedome::layer::{self, ContainTree, JsonFor};
use cakedome::patch::Patching;
use cakedome::policy::{Action, BasicIdentity, EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::ActiveValue::Set;
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection};
use serde_json::{Map, Value, json};

use crate::common::registry;
use crate::entities::{
    countries, customers, documents, master_tags, order_items, orders, setting_order_tags,
};
use crate::support::{context, db, identity, insert_customer, past};

type Tags = DefaultRepo<master_tags::Entity>;
type Customers = DefaultRepo<customers::Entity>;
type OrderTags = DefaultRepo<setting_order_tags::Entity>;
type Documents = DefaultRepo<documents::Entity>;
type Countries = DefaultRepo<countries::Entity>;
type PlainItems = DefaultRepo<order_items::Entity>;

async fn tag_named(
    db: &DatabaseConnection,
    code: &str,
    name: &str,
    active: bool,
) -> master_tags::Model {
    master_tags::ActiveModel {
        code: Set(code.to_string()),
        name: Set(name.to_string()),
        is_active: Set(active),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

// ---- RefBy ----

fn defaults_registry(default: RefBy) -> Registry {
    let mut r = registry();
    r.add(Tags::new());
    r.add(PlainItems::new())
        .default_from(order_items::Column::TagId, default);
    r
}

#[tokio::test]
async fn ref_by_fills_an_omitted_column_on_add_and_yields_the_id() {
    let db = db().await;
    let tag = tag_named(&db, "lk_red", "Red", true).await;
    let by_code = || RefBy::new::<Tags>(master_tags::Column::Code, "lk_red");
    let ctx = context(
        &db,
        identity(&["entity.master_tags.*", "entity.order_items.*"]),
        defaults_registry(by_code()),
    );

    assert_eq!(by_code().value(&ctx).await.unwrap(), json!(tag.id));

    let repo = ctx.repo::<PlainItems>();
    let patch = repo
        .patch(
            None,
            &json!({"data": {"order_id": 1, "name": "x", "seq": 0}}),
        )
        .await
        .unwrap();
    let saved = repo.save_patch(patch).await.unwrap();
    assert_eq!(saved.tag_id, Some(tag.id));
}

#[tokio::test]
async fn ref_by_takes_the_first_active_row_in_the_default_order() {
    let db = db().await;
    tag_named(&db, "lk_first_off", "Same", false).await;
    let second = tag_named(&db, "lk_second", "Same", true).await;
    tag_named(&db, "lk_third", "Same", true).await;
    let ctx = context(
        &db,
        identity(&["entity.master_tags.*"]),
        defaults_registry(RefBy::new::<Tags>(master_tags::Column::Name, "Same")),
    );

    let value = RefBy::new::<Tags>(master_tags::Column::Name, "Same")
        .value(&ctx)
        .await
        .unwrap();

    assert_eq!(value, json!(second.id));
}

/// Keeps only the tag `lk_visible` in view.
struct ScopedTags;

impl Repo for ScopedTags {
    type Entity = master_tags::Entity;
}

#[async_trait]
impl TablePolicy for ScopedTags {
    async fn scope(&self, _ctx: &Context, action: Action) -> Result<Condition, RepoError> {
        Ok(match action {
            Action::View => Condition::all().add(master_tags::Column::Code.eq("lk_visible")),
            _ => Condition::all(),
        })
    }
}

impl EntityPolicy for ScopedTags {}

#[tokio::test]
async fn ref_by_is_null_without_a_visible_active_match() {
    let db = db().await;
    tag_named(&db, "lk_inactive", "n", false).await;
    tag_named(&db, "lk_hidden", "n", true).await;
    let visible = tag_named(&db, "lk_visible", "n", true).await;
    insert_customer(&db, "lk_gone", false).await;
    let live = insert_customer(&db, "lk_live", true).await;
    let all = identity(&[
        "entity.master_tags.*",
        "entity.customers.*",
        "entity.documents.*",
    ]);

    let mut r = registry();
    r.add(Tags::new());
    r.add(Customers::new());
    let ctx = context(&db, all, r);
    for code in ["lk_missing", "lk_inactive"] {
        let value = RefBy::new::<Tags>(master_tags::Column::Code, code)
            .value(&ctx)
            .await
            .unwrap();
        assert_eq!(value, Value::Null, "{code}");
    }
    let by_name = |name: &'static str| RefBy::new::<Customers>(customers::Column::Name, name);
    assert_eq!(by_name("lk_gone").value(&ctx).await.unwrap(), Value::Null);
    assert_eq!(
        by_name("lk_live").value(&ctx).await.unwrap(),
        json!(live.id)
    );

    let mut r = registry();
    r.add(ScopedTags);
    let ctx = context(&db, identity(&["entity.master_tags.*"]), r);
    let by_code = |code: &'static str| RefBy::new::<ScopedTags>(master_tags::Column::Code, code);
    assert_eq!(by_code("lk_hidden").value(&ctx).await.unwrap(), Value::Null);
    assert_eq!(
        by_code("lk_visible").value(&ctx).await.unwrap(),
        json!(visible.id)
    );

    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(&[]), r);
    let value = RefBy::new::<Tags>(master_tags::Column::Code, "lk_visible")
        .value(&ctx)
        .await
        .unwrap();
    assert_eq!(value, Value::Null, "no permission to view");

    let ctx = context(&db, identity(&["entity.master_tags.*"]), registry());
    let value = RefBy::new::<Tags>(master_tags::Column::Code, "lk_visible")
        .value(&ctx)
        .await
        .unwrap();
    assert_eq!(value, Value::Null, "the target Repo is not registered");
}

#[tokio::test]
async fn ref_by_yields_uuid_and_string_ids_as_json_strings() {
    let db = db().await;
    let mut r = registry();
    r.add(Documents::new());
    r.add(Countries::new());
    let ctx = context(
        &db,
        identity(&["entity.documents.*", "entity.countries.*"]),
        r,
    );
    let doc = ctx
        .repo::<Documents>()
        .save(documents::ActiveModel {
            title: Set("lk_doc".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    ctx.repo::<Countries>()
        .insert(countries::ActiveModel {
            id: Set("JP".to_string()),
            name: Set("lk_japan".to_string()),
        })
        .await
        .unwrap();

    let value = RefBy::new::<Documents>(documents::Column::Title, "lk_doc")
        .value(&ctx)
        .await
        .unwrap();
    assert_eq!(value, json!(doc.id.to_string()));
    let value = RefBy::new::<Countries>(countries::Column::Name, "lk_japan")
        .value(&ctx)
        .await
        .unwrap();
    assert_eq!(value, json!("JP"));
}

// ---- trees, load and identities ----

struct ItemsRepo;

impl ItemsRepo {
    const TAG: BelongsToOf<Self, Tags> = BelongsToOf::new("tag", order_items::Column::TagId);
}

impl Repo for ItemsRepo {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG)
    }
}

impl TablePolicy for ItemsRepo {}
impl EntityPolicy for ItemsRepo {}

/// Orders whose association `hidden` is not usable.
struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    const ITEMS: HasManyOf<Self, ItemsRepo> = HasManyOf::new("items", order_items::Column::OrderId);
    const HIDDEN: HasManyOf<Self, ItemsRepo> =
        HasManyOf::new("hidden", order_items::Column::OrderId);
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
            .association(Self::HIDDEN)
            .association(Self::TAGS)
            .default_contain(["items", "items.tag", "customer"])
    }
}

#[async_trait]
impl TablePolicy for OrdersRepo {
    async fn can_contain(&self, _ctx: &Context, name: &str) -> Result<bool, RepoError> {
        Ok(name != "hidden")
    }
}

impl EntityPolicy for OrdersRepo {}

fn orders_registry() -> Registry {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Tags::new());
    r.add(OrderTags::new());
    r.add(ItemsRepo);
    r.add(OrdersRepo);
    r.verify();
    r
}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.order_items.*",
    "entity.customers.*",
    "entity.master_tags.*",
    "entity.setting_order_tags.*",
];

async fn order_with_tagged_item(db: &DatabaseConnection) -> (orders::Model, i64) {
    let c = insert_customer(db, "lk_顧客", true).await;
    let tag = tag_named(db, "lk_item_tag", "lk_タグ", true).await;
    let o = orders::ActiveModel {
        customer_id: Set(c.id),
        title: Set("注文".to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    order_items::ActiveModel {
        order_id: Set(o.id),
        name: Set("明細".to_string()),
        tag_id: Set(Some(tag.id)),
        seq: Set(0),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    (o, tag.id)
}

fn names(found: &[Arc<dyn cakedome::association::Association>]) -> Vec<&'static str> {
    found.iter().map(|a| a.name()).collect()
}

#[tokio::test]
async fn trace_returns_the_associations_found_and_whether_all_levels_were_usable() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let meta = ctx.registry().meta_of::<orders::Entity>().unwrap();

    let (found, ok) = layer::trace(&ctx, meta, "items.tag").await.unwrap();
    assert_eq!((names(&found), ok), (vec!["items", "tag"], true));

    let (found, ok) = layer::trace(&ctx, meta, "items.nope").await.unwrap();
    assert_eq!((names(&found), ok), (vec!["items"], false));

    let (found, ok) = layer::trace(&ctx, meta, "nope").await.unwrap();
    assert_eq!((names(&found), ok), (Vec::<&str>::new(), false));

    let (found, ok) = layer::trace(&ctx, meta, "hidden").await.unwrap();
    assert_eq!((names(&found), ok), (Vec::<&str>::new(), false));

    let ctx = context(
        &db,
        identity(&[
            "entity.orders.*",
            "entity.order_items.*",
            "entity.customers.*",
        ]),
        orders_registry(),
    );
    let meta = ctx.registry().meta_of::<orders::Entity>().unwrap();
    let (found, ok) = layer::trace(&ctx, meta, "items.tag").await.unwrap();
    assert_eq!(
        (names(&found), ok),
        (vec!["items"], false),
        "no permission to view the target of the second level"
    );
}

#[tokio::test]
async fn default_contain_tree_shares_nodes_and_stops_at_an_unusable_level() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let meta = ctx.registry().meta_of::<orders::Entity>().unwrap();

    let tree = layer::default_contain_tree(&ctx, meta).await.unwrap();

    assert_eq!(names(&tree.top_relations()), ["items", "customer"]);
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(names(&tree.nodes[0].children.top_relations()), ["tag"]);
    assert!(tree.nodes[1].children.nodes.is_empty());

    let limited = context(
        &db,
        identity(&[
            "entity.orders.*",
            "entity.order_items.*",
            "entity.customers.*",
        ]),
        orders_registry(),
    );
    let meta = limited.registry().meta_of::<orders::Entity>().unwrap();
    let tree = layer::default_contain_tree(&limited, meta).await.unwrap();
    assert_eq!(names(&tree.top_relations()), ["items", "customer"]);
    assert!(tree.nodes[0].children.nodes.is_empty());
}

#[tokio::test]
async fn contain_tree_insert_shares_a_path_and_names_drive_a_typed_load() {
    let db = db().await;
    let (order, tag_id) = order_with_tagged_item(&db).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let meta = ctx.registry().meta_of::<orders::Entity>().unwrap();
    let (path, _) = layer::trace(&ctx, meta, "items.tag").await.unwrap();

    let mut tree = ContainTree::default();
    tree.insert(&path);
    tree.insert(&path);
    tree.insert(&[]);
    assert_eq!(tree.nodes.len(), 1);
    assert_eq!(tree.nodes[0].children.nodes.len(), 1);

    let mut datas = vec![Map::new()];
    JsonFor::<OrdersRepo>::attach_typed(&ctx, vec![order], &mut datas, &tree)
        .await
        .unwrap();
    let items = datas[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], json!("明細"));
    assert_eq!(items[0]["tag"]["id"], json!(tag_id));
    assert!(datas[0].get("customer").is_none());
}

#[tokio::test]
async fn json_for_new_builds_the_same_row_json_as_to_json_one() {
    let db = db().await;
    let (order, _) = order_with_tagged_item(&db).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let meta = ctx.registry().meta_of::<orders::Entity>().unwrap();
    let json_for = JsonFor::new(ctx.registry().repo::<OrdersRepo>().unwrap());

    let readability = json_for
        .row_readability(&ctx, meta, std::slice::from_ref(&order))
        .await
        .unwrap();
    let data = json_for.data_json(meta, &order, &readability);

    let repo = ctx.repo::<OrdersRepo>();
    let read = repo.get(order.id).await.unwrap();
    let out = repo.to_json_one(&read.into()).await.unwrap();
    assert_eq!(Value::Object(data), out);
}

#[tokio::test]
async fn load_with_a_declaration_that_does_not_match_the_registration_is_a_db_error() {
    let db = db().await;
    let (order, _) = order_with_tagged_item(&db).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let repo = ctx.repo::<OrdersRepo>();
    let rows = std::slice::from_ref(&order);

    let unknown: BelongsToOf<OrdersRepo, Customers> =
        BelongsToOf::new("ghost", orders::Column::CustomerId);
    match repo.load(rows, unknown).await {
        Err(RepoError::Db(e)) => assert!(
            e.to_string()
                .contains("no association with that name is registered"),
            "{e}"
        ),
        other => panic!("expected Db: {:?}", other.err()),
    }

    let other_kind: HasManyOf<OrdersRepo, ItemsRepo> =
        HasManyOf::new("customer", order_items::Column::OrderId);
    match repo.load(rows, other_kind).await {
        Err(RepoError::Db(e)) => {
            assert!(e.to_string().contains("the kind or type differs"), "{e}");
        }
        other => panic!("expected Db: {:?}", other.err()),
    }
}

#[tokio::test]
async fn many_to_many_load_of_no_rows_is_empty() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), orders_registry());

    let loaded = ctx
        .repo::<OrdersRepo>()
        .load(&[], OrdersRepo::TAGS)
        .await
        .unwrap();

    assert!(loaded.is_empty());
}

#[tokio::test]
async fn load_of_an_association_the_policy_refuses_is_forbidden_unless_unchecked() {
    let db = db().await;
    let (order, _) = order_with_tagged_item(&db).await;
    let ctx = context(&db, identity(ALL), orders_registry());
    let rows = std::slice::from_ref(&order);

    let res = ctx
        .repo::<OrdersRepo>()
        .load(rows, OrdersRepo::HIDDEN)
        .await;
    assert!(matches!(res, Err(RepoError::Forbidden)), "{:?}", res.err());

    let loaded = ctx
        .repo::<OrdersRepo>()
        .unchecked()
        .load(rows, OrdersRepo::HIDDEN)
        .await
        .unwrap();
    assert_eq!(loaded[&order.id].len(), 1);
}

#[tokio::test]
async fn an_operation_the_policy_refuses_is_unauthenticated_only_for_an_anonymous_identity() {
    let db = db().await;
    let anonymous = context(&db, BasicIdentity::anonymous(), orders_registry());
    let res = anonymous.repo::<Customers>().find().all().await;
    assert!(
        matches!(res, Err(RepoError::Unauthenticated)),
        "{:?}",
        res.err()
    );

    let no_permission = context(&db, identity(&[]), orders_registry());
    let res = no_permission.repo::<Customers>().find().all().await;
    assert!(matches!(res, Err(RepoError::Forbidden)), "{:?}", res.err());
}
