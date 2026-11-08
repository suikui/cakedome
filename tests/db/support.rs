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
    configs, countries, customer_billings, customer_notes, customers, documents, logs, master_tags,
    measures, order_items, order_tag_links, orders, prices, setting_order_tags,
};

pub use crate::backend::db;

const TEST_DB_PREFIX: &str = "cakedome_test_";

/// The creation time (ms since the epoch) in the name of a test DB, which is `cakedome_test_`
/// followed by a simple-format UUID v7; `None` for any other name.
pub fn test_db_created_at_ms(name: &str) -> Option<u64> {
    let id = name.strip_prefix(TEST_DB_PREFIX)?;
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(&id[..12], 16).ok()
}

/// Whether a test DB is old enough (over ten minutes) to be left over from an earlier run.
pub fn is_stale_test_db(name: &str) -> bool {
    let Some(created) = test_db_created_at_ms(name) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    now.saturating_sub(created) >= 600_000
}

#[test]
fn the_creation_time_is_read_from_a_test_db_name() {
    let name = format!("{TEST_DB_PREFIX}{}", uuid::Uuid::now_v7().simple());
    let read = test_db_created_at_ms(&name).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(now.saturating_sub(read) < 60_000);
    assert!(!is_stale_test_db(&name));
    assert!(is_stale_test_db(&format!(
        "{TEST_DB_PREFIX}{:012x}70008000000000000000",
        now - 7_200_000
    )));
    assert_eq!(test_db_created_at_ms("cakedome_test_abc"), None);
    assert_eq!(
        test_db_created_at_ms("other_0192f3b4c5d67890abcdef0123456789"),
        None
    );
    assert_eq!(
        test_db_created_at_ms(&format!("{TEST_DB_PREFIX}{}", "z".repeat(32))),
        None
    );
}

pub async fn connect(options: ConnectOptions) -> DatabaseConnection {
    let db = Database::connect(options).await.unwrap();
    let schema = Schema::new(db.get_database_backend());
    create(&db, &schema, customers::Entity).await;
    create(&db, &schema, customer_notes::Entity).await;
    create(&db, &schema, customer_billings::Entity).await;
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
    create(&db, &schema, order_tag_links::Entity).await;
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
