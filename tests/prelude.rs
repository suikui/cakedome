//! `use cakedome::prelude::*;` is enough to write, register and use Repos; globbing it together with
//! sea-orm's prelude causes no name clashes.

use std::sync::Arc;

use cakedome::policy::{AuthzMark, BasicIdentity, PermissionSet};
use cakedome::prelude::*;
use sea_orm::entity::prelude::*;
use sea_orm::{ConnectOptions, Database, Schema, Set};
use serde_json::json;

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

type Customers = DefaultRepo<customers::Entity>;

struct OrdersRepo;

impl OrdersRepo {
    const CUSTOMER: BelongsToOf<Self, Customers> =
        BelongsToOf::new("customer", orders::Column::CustomerId);
}

#[async_trait]
impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::CUSTOMER)
    }

    fn validate(&self, input: &ValidationInput<'_, orders::Entity>, errors: &mut FieldErrors) {
        if input.before.is_some_and(|b| b.title == "locked") {
            errors.add("title", "locked", "locked");
        }
    }

    async fn warnings(
        &self,
        _ctx: &Context,
        _input: &ValidationInput<'_, orders::Entity>,
    ) -> Result<Vec<Warning>, RepoError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl TablePolicy for OrdersRepo {
    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError> {
        Ok(action != Action::Delete && ctx.identity().is_authenticated())
    }

    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<orders::Entity>, RepoError> {
        Ok(ColumnAccess::all())
    }
}

#[async_trait]
impl EntityPolicy for OrdersRepo {}

async fn count_all<R: GuardedRepo>(ctx: &Context) -> Result<u64, RepoError> {
    ctx.repo::<R>().find().count().await
}

#[tokio::test]
async fn prelude_is_enough_to_write_register_and_use_repos() {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);
    let db: DatabaseConnection = Database::connect(options).await.unwrap();
    let schema = Schema::new(db.get_database_backend());
    let backend = db.get_database_backend();
    db.execute_unprepared(
        &backend
            .build(&schema.create_table_from_entity(customers::Entity))
            .sql,
    )
    .await
    .unwrap();
    db.execute_unprepared(
        &backend
            .build(&schema.create_table_from_entity(orders::Entity))
            .sql,
    )
    .await
    .unwrap();
    let mut registry = Registry::new(Rules, &[], &[]);
    registry.add(Customers::new());
    registry.add(OrdersRepo);
    registry.verify();
    let identity = BasicIdentity::user(
        1,
        None,
        PermissionSet::new(["entity.customers.*", "entity.orders.*"]),
    );
    let ctx = Context::new(identity, db, AuthzMark::default(), Arc::new(registry));
    let _: &dyn Identity = ctx.identity();

    let tx: Transaction = ctx.begin().await.unwrap();
    let customer = tx
        .repo::<Customers>()
        .save(customers::ActiveModel {
            name: Set("c".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let orders: RepoHandle<'_, OrdersRepo> = ctx.repo::<OrdersRepo>();
    let patch = orders
        .patch(
            None,
            &json!({"data": {"customer_id": customer.id, "title": "o"}}),
        )
        .await
        .unwrap();
    let order = orders.save_patch(patch).await.unwrap();
    let key: Key<OrdersRepo> = order.id;
    let found: Find<'_, OrdersRepo> = orders.find();
    let detail: Loaded<orders::Model> = Loaded::from(orders.get(key).await.unwrap());
    let json = orders.to_json_one(&detail).await.unwrap();

    assert_eq!(found.count().await.unwrap(), 1);
    assert_eq!(count_all::<Customers>(&ctx).await.unwrap(), 1);
    assert_eq!(json["title"], "o");
    assert_eq!(RowId::from(key), RowId::from(order.id));
    assert!(!orders.can(Action::Delete).await.unwrap());
}

#[test]
fn prelude_names_the_other_declarations() {
    const ORDERS: HasManyOf<Customers, OrdersRepo> =
        HasManyOf::new("orders", orders::Column::CustomerId)
            .save(SaveStrategy::Replace)
            .sort(&[(orders::Column::Id, SortOrder::Desc)]);
    let _ = HasOneOf::<Customers, OrdersRepo>::new("order", orders::Column::CustomerId);
    let _ = std::any::type_name::<ManyToManyOf<Customers, OrdersRepo, Customers>>();

    assert_eq!(ORDERS.name(), "orders");
}
