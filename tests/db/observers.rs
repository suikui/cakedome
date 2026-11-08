//! Observers (`Registry::observe`): the rows passed, call order, errors, `skip_observer` and the
//! saves and deletes that come through associations.

use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::association::HasManyOf;
use cakedome::key::RowId;
use cakedome::marshal::WriteMode;
use cakedome::observer::{Change, ChangeKind, ChangeObserver};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::SaveStrategy;
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{Map, Value, json};

use crate::common::registry;
use crate::entities::{customers, order_items, orders};
use crate::support::{Log, context, db, identity, insert_customer, past};

type Customers = DefaultRepo<customers::Entity>;
type Orders = DefaultRepo<orders::Entity>;
type Items = DefaultRepo<order_items::Entity>;

const ITEMS: HasManyOf<Orders, Items> = HasManyOf::new("items", order_items::Column::OrderId)
    .save(SaveStrategy::Replace)
    .dependent(true);

#[derive(Clone, Debug)]
struct Seen {
    table: String,
    id: RowId,
    kind: &'static str,
    before: Option<Map<String, Value>>,
    after: Option<Map<String, Value>>,
}

/// Records every change; returns `Err` when the row after the change has the name `fail_on`.
struct Recorder {
    name: &'static str,
    log: Log,
    seen: Arc<Mutex<Vec<Seen>>>,
    fail_on: Option<&'static str>,
}

impl Recorder {
    fn new(name: &'static str, log: &Log) -> Self {
        Self {
            name,
            log: log.clone(),
            seen: Arc::default(),
            fail_on: None,
        }
    }
}

#[async_trait]
impl ChangeObserver for Recorder {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn on_change(&self, _ctx: &Context, change: &Change<'_>) -> Result<(), RepoError> {
        self.log.push(format!(
            "{}:{}:{}:{}",
            self.name,
            change.table,
            change.kind.as_str(),
            change.id
        ));
        self.seen.lock().unwrap().push(Seen {
            table: change.table.to_string(),
            id: change.id.clone(),
            kind: change.kind.as_str(),
            before: change.before.cloned(),
            after: change.after.cloned(),
        });
        if let (Some(fail), Some(after)) = (self.fail_on, change.after)
            && after.get("name") == Some(&json!(fail))
        {
            return Err(RepoError::Forbidden);
        }
        Ok(())
    }
}

/// A customers Repo that logs its own hooks, to see they run before the observers.
struct Hooked {
    log: Log,
}

#[async_trait]
impl Repo for Hooked {
    type Entity = customers::Entity;

    async fn after_save(
        &self,
        _ctx: &Context,
        model: &customers::Model,
        mode: WriteMode,
        _before: Option<&customers::Model>,
    ) -> Result<(), RepoError> {
        let mode = match mode {
            WriteMode::Add => "add",
            WriteMode::Edit => "edit",
        };
        self.log.push(format!("repo:customers:{mode}:{}", model.id));
        Ok(())
    }

    async fn after_delete(
        &self,
        _ctx: &Context,
        model: &customers::Model,
    ) -> Result<(), RepoError> {
        self.log.push(format!("repo:customers:delete:{}", model.id));
        Ok(())
    }
}

impl TablePolicy for Hooked {}
impl EntityPolicy for Hooked {}

const CUSTOMERS: &[&str] = &["entity.customers.*"];
const ORDERS_ALL: &[&str] = &[
    "entity.orders.*",
    "entity.order_items.*",
    "entity.customers.*",
];

fn new_customer(name: &str) -> customers::ActiveModel {
    customers::ActiveModel {
        name: Set(name.to_string()),
        ..Default::default()
    }
}

#[test]
fn change_kind_names() {
    assert_eq!(ChangeKind::Add.as_str(), "add");
    assert_eq!(ChangeKind::Edit.as_str(), "edit");
    assert_eq!(ChangeKind::Delete.as_str(), "delete");
}

#[tokio::test]
async fn add_edit_delete_pass_the_rows_before_and_after() {
    let db = db().await;
    let log = Log::default();
    let recorder = Recorder::new("a", &log);
    let seen = recorder.seen.clone();
    let mut r = registry();
    r.add(Customers::new()).hidden(customers::Column::Email);
    r.observe(recorder);
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let mut am = new_customer("ob_rows");
    am.email = Set(Some("ob@example.com".to_string()));
    let added = ctx.repo::<Customers>().save(am).await.unwrap();
    ctx.repo::<Customers>()
        .save(customers::ActiveModel {
            id: Unchanged(added.id),
            name: Set("変更".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    ctx.repo::<Customers>().delete(added.id).await.unwrap();

    let seen = seen.lock().unwrap().clone();
    let kinds: Vec<_> = seen
        .iter()
        .map(|s| (s.table.as_str(), s.id.clone(), s.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("customers", RowId::Int(added.id), "add"),
            ("customers", RowId::Int(added.id), "edit"),
            ("customers", RowId::Int(added.id), "delete"),
        ]
    );

    let (add, edit, delete) = (&seen[0], &seen[1], &seen[2]);
    assert!(add.before.is_none());
    let after = add.after.as_ref().unwrap();
    assert_eq!(after["id"], json!(added.id));
    assert_eq!(after["name"], json!("ob_rows"));
    assert!(!after.contains_key("email"), "hidden column: {after:?}");
    assert!(
        !after.contains_key("is_active"),
        "internal column: {after:?}"
    );

    assert_eq!(edit.before.as_ref().unwrap()["name"], json!("ob_rows"));
    assert_eq!(edit.after.as_ref().unwrap()["name"], json!("変更"));

    assert_eq!(delete.before.as_ref().unwrap()["name"], json!("変更"));
    assert!(delete.after.is_none(), "a soft delete has no after row");
}

#[tokio::test]
async fn observers_run_after_the_repo_hook_in_registration_order() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Hooked { log: log.clone() });
    r.observe(Recorder::new("a", &log));
    r.observe(Recorder::new("b", &log));
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let added = ctx
        .repo::<Hooked>()
        .save(new_customer("ob_order"))
        .await
        .unwrap();
    ctx.repo::<Hooked>().delete(added.id).await.unwrap();

    let id = added.id;
    assert_eq!(
        log.take(),
        [
            format!("repo:customers:add:{id}"),
            format!("a:customers:add:{id}"),
            format!("b:customers:add:{id}"),
            format!("repo:customers:delete:{id}"),
            format!("a:customers:delete:{id}"),
            format!("b:customers:delete:{id}"),
        ]
    );
}

#[tokio::test]
async fn observer_error_rolls_back_the_save_and_skips_later_observers() {
    let db = db().await;
    let log = Log::default();
    let mut failing = Recorder::new("a", &log);
    failing.fail_on = Some("ob_fail");
    let mut r = registry();
    r.add(Customers::new());
    r.observe(failing);
    r.observe(Recorder::new("b", &log));
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let tx = ctx.begin().await.unwrap();
    let err = tx
        .repo::<Customers>()
        .save(new_customer("ob_fail"))
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::Forbidden), "{err:?}");
    tx.rollback().await.unwrap();

    let rows = customers::Entity::find()
        .filter(customers::Column::Name.eq("ob_fail"))
        .all(&db)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
    let entries = log.take();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].starts_with("a:customers:add:"), "{entries:?}");
}

#[tokio::test]
async fn unchanged_update_is_not_observed() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Customers::new());
    r.observe(Recorder::new("a", &log));
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let added = ctx
        .repo::<Customers>()
        .save(new_customer("ob_same"))
        .await
        .unwrap();
    log.take();
    ctx.repo::<Customers>()
        .save(customers::ActiveModel {
            id: Unchanged(added.id),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(log.take(), Vec::<String>::new());
}

#[tokio::test]
async fn unchecked_save_is_observed() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Customers::new());
    r.observe(Recorder::new("a", &log));
    r.verify();
    let ctx = context(&db, identity(&[]), r);

    let added = ctx
        .repo::<Customers>()
        .unchecked()
        .save(new_customer("ob_unchecked"))
        .await
        .unwrap();

    assert_eq!(log.take(), [format!("a:customers:add:{}", added.id)]);
}

#[tokio::test]
async fn skip_observer_excludes_only_that_observer() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Customers::new()).skip_observer("a");
    r.observe(Recorder::new("a", &log));
    r.observe(Recorder::new("b", &log));
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let added = ctx
        .repo::<Customers>()
        .save(new_customer("ob_skip"))
        .await
        .unwrap();

    assert_eq!(log.take(), [format!("b:customers:add:{}", added.id)]);
}

#[tokio::test]
async fn skipping_the_same_observer_twice_is_the_same_as_once() {
    let db = db().await;
    let log = Log::default();
    let mut r = registry();
    r.add(Customers::new())
        .skip_observer("a")
        .skip_observer("a");
    r.observe(Recorder::new("a", &log));
    r.observe(Recorder::new("b", &log));
    r.verify();
    let ctx = context(&db, identity(CUSTOMERS), r);

    let added = ctx
        .repo::<Customers>()
        .save(new_customer("ob_twice"))
        .await
        .unwrap();

    assert_eq!(log.take(), [format!("b:customers:add:{}", added.id)]);
}

#[test]
#[should_panic(expected = "customers: skip_observer nope is not registered")]
fn skipping_an_unknown_observer_panics_on_verify() {
    let mut r = registry();
    r.add(Customers::new()).skip_observer("nope");
    r.verify();
}

#[test]
#[should_panic(expected = "an observer is registered twice: a")]
fn observing_the_same_name_twice_panics() {
    let log = Log::default();
    let mut r = registry();
    r.observe(Recorder::new("a", &log));
    r.observe(Recorder::new("a", &log));
}

async fn order_with_items(db: &DatabaseConnection) -> (orders::Model, Vec<i64>) {
    let c = insert_customer(db, "ob_顧客", true).await;
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
    let mut ids = Vec::new();
    for name in ["明細1", "明細2"] {
        let item = order_items::ActiveModel {
            order_id: Set(o.id),
            name: Set(name.to_string()),
            seq: Set(0),
            is_active: Set(true),
            created: Set(past()),
            modified: Set(past()),
            ..Default::default()
        }
        .insert(db)
        .await
        .unwrap();
        ids.push(item.id);
    }
    (o, ids)
}

fn orders_context(db: &DatabaseConnection, log: &Log) -> Context {
    let mut r = registry();
    r.add(Customers::new());
    r.add(Items::new());
    r.add(Orders::new()).association(ITEMS);
    r.observe(Recorder::new("t", log));
    r.verify();
    context(db, identity(ORDERS_ALL), r)
}

#[tokio::test]
async fn children_saved_with_the_parent_are_observed() {
    let db = db().await;
    let log = Log::default();
    let ctx = orders_context(&db, &log);
    let (o, items) = order_with_items(&db).await;

    ctx.repo::<Orders>()
        .save_with(orders::ActiveModel {
            id: Unchanged(o.id),
            ..Default::default()
        })
        .replace(
            ITEMS,
            vec![
                order_items::ActiveModel {
                    id: Unchanged(items[0]),
                    name: Set("変更".to_string()),
                    ..Default::default()
                },
                order_items::ActiveModel {
                    name: Set("追加".to_string()),
                    ..Default::default()
                },
            ],
        )
        .save()
        .await
        .unwrap();

    let entries = log.take();
    let mut kinds: Vec<&str> = entries
        .iter()
        .map(|e| e.rsplit_once(':').unwrap().0)
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        [
            "t:order_items:add",
            "t:order_items:delete",
            "t:order_items:edit"
        ],
        "{entries:?}"
    );
    assert!(entries.contains(&format!("t:order_items:edit:{}", items[0])));
    assert!(entries.contains(&format!("t:order_items:delete:{}", items[1])));
}

#[tokio::test]
async fn dependent_deletes_are_observed_children_first() {
    let db = db().await;
    let log = Log::default();
    let ctx = orders_context(&db, &log);
    let (o, items) = order_with_items(&db).await;

    ctx.repo::<Orders>().delete(o.id).await.unwrap();

    let entries = log.take();
    assert_eq!(entries.len(), 3, "{entries:?}");
    let mut children = entries[..2].to_vec();
    children.sort();
    let mut expected = vec![
        format!("t:order_items:delete:{}", items[0]),
        format!("t:order_items:delete:{}", items[1]),
    ];
    expected.sort();
    assert_eq!(children, expected);
    assert_eq!(entries[2], format!("t:orders:delete:{}", o.id));
}
