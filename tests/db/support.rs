use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::policy::{AuthzMark, BasicIdentity, PermissionSet};
use cakedome::registry::Registry;
use chrono::{DateTime, TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, Schema, Set,
};

use crate::entities::{
    configs, countries, customer_notes, customers, documents, logs, master_tags, measures,
    order_items, orders, prices, setting_order_tags,
};

pub use crate::backend::db;

pub async fn connect(options: ConnectOptions) -> DatabaseConnection {
    let db = Database::connect(options).await.unwrap();
    let schema = Schema::new(db.get_database_backend());
    create(&db, &schema, customers::Entity).await;
    create(&db, &schema, customer_notes::Entity).await;
    create(&db, &schema, prices::Entity).await;
    create(&db, &schema, measures::Entity).await;
    create(&db, &schema, configs::Entity).await;
    create(&db, &schema, master_tags::Entity).await;
    create(&db, &schema, logs::Entity).await;
    create(&db, &schema, documents::Entity).await;
    create(&db, &schema, countries::Entity).await;
    create(&db, &schema, orders::Entity).await;
    create(&db, &schema, order_items::Entity).await;
    create(&db, &schema, setting_order_tags::Entity).await;
    db
}

async fn create<E: EntityTrait>(db: &DatabaseConnection, schema: &Schema, entity: E) {
    let backend = db.get_database_backend();
    let sql = backend.build(&schema.create_table_from_entity(entity)).sql;
    // sea-query maps MySQL datetime columns to TIMESTAMP (second precision), but Repo writes
    // microseconds, so use DATETIME(6).
    let sql = if backend == DbBackend::MySql {
        sql.replace(" timestamp", " datetime(6)")
    } else {
        sql
    };
    db.execute_unprepared(&sql).await.unwrap();
}

pub fn identity(patterns: &[&str]) -> BasicIdentity {
    BasicIdentity::user(1, None, PermissionSet::new(patterns.iter().copied()))
}

pub fn context(db: &DatabaseConnection, identity: BasicIdentity, registry: Registry) -> Context {
    Context::new(
        identity,
        db.clone(),
        AuthzMark::default(),
        Arc::new(registry),
    )
}

/// created / modified for directly inserted rows; always differs from the Repo's current time.
pub fn past() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()
}

pub fn assert_between(t: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) {
    assert!(from <= t && t <= to, "{from} <= {t} <= {to}");
}

pub async fn insert_customer(
    db: &DatabaseConnection,
    name: &str,
    active: bool,
) -> customers::Model {
    customers::ActiveModel {
        name: Set(name.to_string()),
        is_active: Set(active),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

pub async fn insert_tag(db: &DatabaseConnection, code: &str, active: bool) -> master_tags::Model {
    master_tags::ActiveModel {
        code: Set(code.to_string()),
        name: Set(format!("{code} の名前")),
        is_active: Set(active),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

pub async fn raw_customer(db: &DatabaseConnection, id: i64) -> Option<customers::Model> {
    customers::Entity::find_by_id(id).one(db).await.unwrap()
}

#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    pub fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}
