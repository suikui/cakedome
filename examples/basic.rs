//! The README example as a complete program on an in-memory SQLite DB.
//!
//! Run with `cargo run --example basic`.

use std::sync::Arc;

use cakedome::policy::{AuthzMark, BasicIdentity, PermissionSet};
use cakedome::prelude::*;
use sea_orm::Set;
use sea_orm::entity::prelude::*;

// Your table rules decide each table's lifecycle and managed columns.
struct Rules;

impl TableRules for Rules {
    fn spec(&self, _table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        let has = |name: &str| columns.iter().find(|c| **c == name).copied();
        Ok(TableSpec {
            lifecycle: Lifecycle::SoftDelete,
            active_column: has("is_active"),
            created_column: has("created"),
            modified_column: has("modified"),
            ..TableSpec::plain()
        })
    }
}

type CustomersRepo = DefaultRepo<customers::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, CustomersRepo> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
    }
}

#[async_trait]
impl TablePolicy for OrdersRepo {
    // Logged-in users may do anything but delete.
    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError> {
        Ok(action != Action::Delete && ctx.identity().is_authenticated())
    }
}

impl EntityPolicy for OrdersRepo {}

async fn run(db: DatabaseConnection) -> Result<(), RepoError> {
    let mut registry = Registry::new(Rules, &[], &[]);
    registry.add(CustomersRepo::new());
    registry.add(OrdersRepo);
    registry.verify();
    let registry = Arc::new(registry);

    // One Context per request: who is asking, and which connection to use.
    let identity = BasicIdentity::user(1, None, PermissionSet::new(["entity.customers.*"]));
    let ctx = Context::new(identity, db, AuthzMark::default(), registry);

    let customer = ctx
        .repo::<CustomersRepo>()
        .save(customers::ActiveModel {
            name: Set("Acme".into()),
            ..Default::default()
        })
        .await?;
    let orders = ctx.repo::<OrdersRepo>();
    orders
        .save(orders::ActiveModel {
            customer_id: Set(customer.id),
            title: Set("First order".into()),
            ..Default::default()
        })
        .await?;

    let rows = orders.find().contain(OrdersRepo::CUSTOMER).all().await?;
    let json = orders.to_json(&rows).await?; // [{"id": 1, "title": …, "customer": {…}}]
    println!("{}", serde_json::to_string_pretty(&json).unwrap());
    Ok(())
}

mod customers {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "customers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        pub is_active: bool,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod orders {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "orders")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub customer_id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub is_active: bool,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[tokio::main]
async fn main() {
    // An in-memory SQLite DB is per connection, so keep a single one.
    let mut options = sea_orm::ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);
    let db = sea_orm::Database::connect(options).await.unwrap();
    let backend = db.get_database_backend();
    let schema = sea_orm::Schema::new(backend);
    for table in [
        schema.create_table_from_entity(customers::Entity),
        schema.create_table_from_entity(orders::Entity),
    ] {
        db.execute_unprepared(&backend.build(&table).sql)
            .await
            .unwrap();
    }
    run(db).await.unwrap();
}
