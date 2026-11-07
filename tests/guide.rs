//! The examples of the usage guide, run on SQLite so that they keep compiling.

#![allow(dead_code)]
#[path = "db/sqlite.rs"]
mod backend;
#[path = "common/mod.rs"]
mod common;
#[path = "db/entities.rs"]
mod entities;
#[path = "db/support.rs"]
mod support;

use std::sync::Arc;

use cakedome::defaults::CurrentUserId;
use cakedome::marshal::WriteMode;
use cakedome::observer::{Change, ChangeObserver};
use cakedome::patch::PatchError;
use cakedome::policy::{AuthzMark, BasicIdentity, PermissionSet};
use cakedome::prelude::*;
use sea_orm::entity::prelude::*;
use sea_orm::{Condition, IntoActiveModel, Set};
use serde_json::json;

use entities::{customer_notes, customers, master_tags, order_items, orders, setting_order_tags};

struct AppRules;

impl TableRules for AppRules {
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        let has = |name: &str| columns.iter().find(|c| **c == name).copied();
        let active_column = has("is_active");
        let lifecycle = match active_column {
            None => Lifecycle::HardDelete,
            Some(_) if table.starts_with("master_") => Lifecycle::Activatable,
            Some(_) => Lifecycle::SoftDelete,
        };
        Ok(TableSpec {
            lifecycle,
            active_column,
            created_column: has("created"),
            modified_column: has("modified"),
            order_column: has("seq"),
            ..TableSpec::plain()
        })
    }
}

type CustomersRepo = DefaultRepo<customers::Entity>;
type OrderItemsRepo = DefaultRepo<order_items::Entity>;
type OrderTagsRepo = DefaultRepo<setting_order_tags::Entity>;
type TagsRepo = DefaultRepo<master_tags::Entity>;

pub struct OrdersRepo;

impl OrdersRepo {
    pub const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
    pub const ITEMS: HasManyOf<Self, OrderItemsRepo> =
        HasManyOf::new("items", order_items::Column::OrderId)
            .save(SaveStrategy::Replace)
            .dependent(true);
    pub const TAGS: ManyToManyOf<Self, OrderTagsRepo, TagsRepo> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace);
}

#[async_trait]
impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.label("Order")
            .field(orders::Column::Title, |f| f.label("Title"))
            .association(Self::CUSTOMER)
            .association(Self::ITEMS)
            .association(Self::TAGS)
            .default_contain(["customer", "items"])
    }

    fn validate(&self, input: &ValidationInput<'_, orders::Entity>, errors: &mut FieldErrors) {
        if input
            .after
            .title
            .try_as_ref()
            .is_some_and(|t| t.trim().is_empty())
        {
            errors.add("title", "required", "Enter a title");
        }
    }

    async fn before_save(
        &self,
        _ctx: &Context,
        am: &mut orders::ActiveModel,
        mode: WriteMode,
        _before: Option<&orders::Model>,
    ) -> Result<(), RepoError> {
        if mode == WriteMode::Add && am.title.is_not_set() {
            am.title = Set("Untitled".to_string());
        }
        Ok(())
    }
}

#[async_trait]
impl TablePolicy for OrdersRepo {
    async fn scope(&self, ctx: &Context, action: Action) -> Result<Condition, RepoError> {
        if action == Action::View || ctx.identity().can("entity.orders.all") {
            return Ok(Condition::all());
        }
        Ok(Condition::all().add(orders::Column::Title.not_like("locked%")))
    }

    async fn columns(&self, ctx: &Context) -> Result<ColumnAccess<orders::Entity>, RepoError> {
        let access = ColumnAccess::all();
        Ok(if ctx.identity().can("entity.orders.rename") {
            access
        } else {
            access.read_only(orders::Column::Title)
        })
    }
}

#[async_trait]
impl EntityPolicy for OrdersRepo {
    async fn can_delete(&self, _ctx: &Context, model: &orders::Model) -> Result<bool, RepoError> {
        Ok(model.customer_id != 0)
    }
}

struct NotesRepo;

impl Repo for NotesRepo {
    type Entity = customer_notes::Entity;

    fn initialize(
        e: RepoConfig<'_, customer_notes::Entity>,
    ) -> RepoConfig<'_, customer_notes::Entity> {
        e.default(customer_notes::Column::Body, json!("(empty)"))
            .default_from(customer_notes::Column::CustomerId, CurrentUserId)
            .optimistic_lock(false)
    }
}

impl TablePolicy for NotesRepo {}
impl EntityPolicy for NotesRepo {}

struct AuditLog;

#[async_trait]
impl ChangeObserver for AuditLog {
    fn name(&self) -> &'static str {
        "audit"
    }

    async fn on_change(&self, _ctx: &Context, change: &Change<'_>) -> Result<(), RepoError> {
        tracing::info!(table = change.table, id = %change.id, kind = change.kind.as_str());
        Ok(())
    }
}

struct RequestInfo {
    locale: String,
}

fn build_registry() -> Arc<Registry> {
    let mut registry = Registry::new(AppRules, &[("orders", "Order")], &[("title", "Title")]);
    registry.add(CustomersRepo::new());
    registry.add(OrderItemsRepo::new());
    registry.add(OrderTagsRepo::new());
    registry.add(TagsRepo::new());
    registry.add(OrdersRepo);
    registry.add(NotesRepo);
    registry.observe(AuditLog);
    registry.set_messages(&[("required", "必須です"), ("max_length", "{max} 文字以内")]);
    registry.verify();
    Arc::new(registry)
}

#[tokio::test]
async fn guide_examples_run() {
    let db = support::db().await;
    let registry = build_registry();
    let identity = BasicIdentity::user(1, None, PermissionSet::new(["*"]));
    let ctx = Context::new(identity, db.clone(), AuthzMark::default(), registry.clone())
        .with_extension(RequestInfo {
            locale: "ja".into(),
        });
    let _locale = ctx.extension::<RequestInfo>().map(|r| r.locale.as_str());

    let customer = ctx
        .repo::<CustomersRepo>()
        .save(customers::ActiveModel {
            name: Set("Acme".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let tag = support::insert_tag(&db, "t", true).await;

    let orders = ctx.repo::<OrdersRepo>();
    let order = orders
        .save(orders::ActiveModel {
            customer_id: Set(customer.id),
            title: Set("First".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let item = order_items::ActiveModel {
        name: Set("Pen".into()),
        ..Default::default()
    };
    let order = orders
        .save_with(order.into_active_model())
        .replace(OrdersRepo::ITEMS, vec![item])
        .replace_links(OrdersRepo::TAGS, vec![tag.id])
        .save()
        .await
        .unwrap();
    orders
        .unlink(OrdersRepo::TAGS, &order, vec![tag.id])
        .await
        .unwrap();
    orders
        .link(OrdersRepo::TAGS, &order, vec![tag.id])
        .await
        .unwrap();

    let tx = ctx.begin().await.unwrap();
    let current = tx.repo::<OrdersRepo>().get(order.id).await.unwrap();
    let mut am = current.clone().into_active_model();
    am.title = Set("Second".into());
    tx.repo::<OrdersRepo>()
        .save_if_unmodified(am, current.modified)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let one = orders.get(order.id).await.unwrap();
    let recent = orders
        .find()
        .filter(orders::Column::Title.contains("Sec"))
        .order_by_desc(orders::Column::Created)
        .limit(20)
        .all()
        .await
        .unwrap();
    let n = orders.find().count().await.unwrap();
    let rows = orders
        .find()
        .contain(OrdersRepo::CUSTOMER)
        .contain(OrdersRepo::ITEMS)
        .all()
        .await
        .unwrap();
    for row in &rows {
        let customer = row.related(OrdersRepo::CUSTOMER).expect("contained");
        let items = row.related(OrdersRepo::ITEMS).expect("contained");
        assert!(customer.is_some());
        assert_eq!(items.len(), 1);
    }
    let json = orders.to_json(&rows).await.unwrap();
    let items = orders.load(&recent, OrdersRepo::ITEMS).await.unwrap();
    assert_eq!(one.title, "Second");
    assert_eq!(n, 1);
    assert_eq!(json.len(), 1);
    assert_eq!(items[&order.id].len(), 1);
    let can_delete = orders.can(Action::Delete).await.unwrap();
    assert!(can_delete);

    let body = json!({
        "data": {"title": "Third", "items": [{"name": "Ink"}]},
        "modified": orders.get(order.id).await.unwrap().modified.to_rfc3339(),
        "associated": ["items"],
    });
    let patch = orders.patch(Some(order.id), &body).await.unwrap();
    let saved = orders.save_patch(patch).await.unwrap();
    assert_eq!(saved.title, "Third");
    match orders.patch(None, &json!({"data": {"title": ""}})).await {
        Err(PatchError::Invalid(errors)) => {
            let _ = serde_json::to_value(&errors).unwrap();
        }
        Ok(p) => match orders.save_patch(p).await {
            Err(PatchError::Invalid(errors)) => {
                let _ = serde_json::to_value(&errors).unwrap();
            }
            other => panic!("{:?}", other.err()),
        },
        Err(e) => panic!("{e:?}"),
    }

    orders.delete(order.id).await.unwrap();
    orders.purge(order.id).await.unwrap();
    assert!(ctx.authz().is_marked());
}
