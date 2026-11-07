# cakedome

[日本語](README.ja.md)

An ORM layer on [SeaORM](https://www.sea-ql.org/SeaORM/) that puts every read and write of an
entity through a Repo with a Policy.

Inspired by the [CakePHP](https://cakephp.org/) ORM: table classes with associations, `contain`
for eager loading, request data marshalled into entities with validation, and nested saves. The
same ideas are written for Rust with typed declarations.

Status: 0.x. The API may change between minor versions.

## Features

- **Policies on every operation.** `TablePolicy` decides which actions are allowed, which rows are
  visible (`scope`) and which columns are readable or writable; `EntityPolicy` decides per row.
  `RepoHandle` methods cannot bypass them, except through an explicit `unchecked()` handle.
- **Typed associations.** `BelongsToOf`, `HasManyOf`, `HasOneOf` and `ManyToManyOf` are declared
  as constants with typed columns, so a wrong foreign key does not compile. They load with one
  `IN` query per association (`load`, `contain`), filter across associations, and save nested
  rows (`save_with`, `link`, `unlink`).
- **Table lifecycles.** Soft delete, activatable masters and hard delete are decided by your table
  rules; created and modified timestamps are managed for you.
- **JSON in and out.** Request data becomes an ActiveModel with validation errors as codes and
  parameters (`required`, `max_length {max}`, …); rows become JSON with the Policy's columns only.
  Default messages are English; pass your own table to translate them.
- **Hooks and observers.** `before_find`, `before_save`, `after_save`, after-commit hooks and
  observers for every change.
- **Optimistic locking** with `save_if_unmodified`.
- **SQLite, PostgreSQL and MySQL.** The test suite runs on all three.

## What cakedome does and what you write

cakedome does:

- Runs the Policy on every read and write, and reuses `TablePolicy` answers within a `Context`.
- Reads and writes rows through SeaORM with soft delete, timestamps, optimistic locking,
  transactions (savepoints when nested) and after-commit hooks.
- Loads, filters across and saves associations, including nested saves and dependent deletes.
- Turns request data into ActiveModels (type and length checks, unique and reference checks) and
  rows into JSON.
- Checks the registrations at startup: column and Rust types, association keys, default
  `contain` paths.

You write:

- SeaORM entities and the database schema (migrations). cakedome does not create tables.
- One `TableRules` implementation: your naming conventions decide each table's lifecycle, managed
  columns, sort column, defaults and unique groups.
- A Repo per entity (`impl Repo`, `TablePolicy` and `EntityPolicy`), or `DefaultRepo` when the
  defaults are enough: labels, defaults, associations, validation, hooks and access rules.
- Your user type (`impl Identity`), or `BasicIdentity` with permission strings.
- Startup: build the `Registry`, optionally add translated messages (`set_messages`) and
  observers, then call `verify()`.
- Per request: create a `Context` with the identity, the DB connection to use and any values your
  Policies need (`with_extension`).
- Everything around the ORM: HTTP, authentication and sessions, mapping `RepoError` to responses,
  and a generic JSON API if you want one (`layer` has the building blocks).

## Example

```rust
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
```

`customers` and `orders` are ordinary SeaORM entities with `is_active`, `created` and `modified`
columns; [`examples/basic.rs`](examples/basic.rs) is the complete program.

See the [guide](docs/guide.md) for every step: table rules, Repos, Policies, reading, writing,
saving request data, hooks and messages.

## Concepts

| | |
|---|---|
| `Repo` | Per-entity configuration: labels, defaults, associations, validation and hooks |
| `TablePolicy`, `EntityPolicy` | Who may do what, on which rows and columns |
| `Registry` | All Repos, built once at startup and checked with `verify()` |
| `Context` | The identity, connection and your values of one request; `ctx.repo::<R>()` returns a `RepoHandle` |
| `Identity` | Your user type; `BasicIdentity` checks permission strings like `entity.orders.edit` |
| `AuthzMark` | Records that an operation passed authorization, so a request handler can verify it |
| `layer` | Building blocks for layers on top of the ORM, such as a JSON API |

## Database notes

- On MySQL use `DATETIME(6)` for timestamp columns; `TIMESTAMP` and `DATETIME` round to seconds,
  which breaks optimistic locking.
- SQLite has no row locks. A concurrent `save_if_unmodified` fails with `database is locked`
  instead of `Conflict`; it never overwrites the first save.

## Development

`cargo test` runs the SQLite suite and needs no setup. The PostgreSQL and MySQL suites run the same
DB tests and are behind features; each test creates its own `cakedome_test_*` schema or database:

```sh
CAKEDOME_TEST_PG_URL=postgres://user:pass@localhost:5432/postgres \
CAKEDOME_TEST_MYSQL_URL=mysql://root:pass@localhost:3306/mysql \
cargo test --all-features
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
