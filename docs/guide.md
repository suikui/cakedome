# cakedome guide

[日本語](guide.ja.md)

This guide walks through using cakedome in an application. The examples use five tables:

| Table | Columns | Kind |
|---|---|---|
| `customers` | `id`, `name`, `email`, `is_active`, `created`, `modified` | soft delete |
| `orders` | `id`, `customer_id`, `title`, `is_active`, `created`, `modified` | soft delete |
| `order_items` | `id`, `order_id`, `name`, `seq`, `is_active`, `created`, `modified` | soft delete, ordered |
| `master_tags` | `id`, `code`, `name`, `is_active` | master (deactivated, never deleted) |
| `setting_order_tags` | `id`, `order_id`, `tag_id`, `seq` | join table of orders and tags |

Each entity is an ordinary SeaORM entity (`DeriveEntityModel`, with an empty `Relation`).
Associations are declared on the Repos instead.

1. [Setup](#1-setup)
2. [Table rules](#2-table-rules)
3. [Repos](#3-repos)
4. [Policies](#4-policies)
5. [Registry and Context](#5-registry-and-context)
6. [Reading](#6-reading)
7. [Writing](#7-writing)
8. [Saving request data](#8-saving-request-data)
9. [Hooks and observers](#9-hooks-and-observers)
10. [Messages](#10-messages)
11. [Values on the Context](#11-values-on-the-context)
12. [Building layers](#12-building-layers)

## 1. Setup

```toml
[dependencies]
cakedome = "0.1"
sea-orm = { version = "2.0", features = ["macros", "sqlx-mysql", "runtime-tokio-rustls"] }
```

cakedome does not pick a database driver or an async runtime; enable the ones your application
uses on SeaORM. Most items come in with:

```rust
use cakedome::prelude::*;
use sea_orm::entity::prelude::*;
```

## 2. Table rules

cakedome knows no naming conventions. One `TableRules` implementation turns each table's name and
columns into a `TableSpec`:

| `TableSpec` field | Meaning |
|---|---|
| `lifecycle` | `SoftDelete` (delete sets the active flag to false), `Activatable` (rows are deactivated, never deleted) or `HardDelete` |
| `active_column` | The active flag; required unless `HardDelete` |
| `created_column`, `modified_column` | Timestamps cakedome sets; `modified` is also used by optimistic locking |
| `order_column` | An `i32` column that keeps the order of children and join rows |
| `defaults` | Defaults for omitted columns on an add |
| `uniques` | Column groups checked for uniqueness before saving |
| `warn_duplicates` | Column groups that raise a Warning when duplicated |

```rust
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
```

Return `Err` to stop at startup when a table breaks your conventions. A single table can override
the result with `RepoConfig::table_spec`.

The order column can have any name (`seq` is only this example's convention). A many-to-many join
table needs one.

## 3. Repos

Each entity has a Repo: configuration, validation and hooks (`impl Repo`) and access rules
(`impl TablePolicy`, `impl EntityPolicy`). `DefaultRepo<E>` is a Repo with every default.

```rust
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
        if input.after.title.try_as_ref().is_some_and(|t| t.trim().is_empty()) {
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
```

Associations are constants on the Repo, so foreign keys are typed columns:

| Declaration | The foreign key is on | Options |
|---|---|---|
| `BelongsToOf<S, T>` | `S` (this entity) | `binding_key`, `save`, `finder` |
| `HasManyOf<S, T>` | `T` (the children) | `save(strategy)`, `dependent`, `cascade_callbacks`, `filter`, `sort`, `label`, `binding_key`, `finder` |
| `HasOneOf<S, T>` | `T` (the child) | `save()`, `dependent`, `cascade_callbacks`, `filter`, `label`, `binding_key`, `finder` |
| `ManyToManyOf<S, J, T>` | `J` (the join table) | `save(strategy)`, `dependent`, `cascade_callbacks`, `filter`, `sort`, `label`, `binding_key`, `finder` |

`SaveStrategy::Replace` makes the sent children the full set (others are dropped and the order
column is renumbered); `Append` only adds and updates. `dependent(true)` deletes the children with
the parent.

Other settings in `initialize`:

| Method | Purpose |
|---|---|
| `label`, `labels`, `field(column, \|f\| f.label(..).default(..))` | Labels and per-field settings |
| `default(column, json)`, `default_from(column, dynamic)` | Fixed and per-request defaults on an add (`CurrentUserId`, `RefBy` or your `DynamicDefault`) |
| `hidden(column)` | Never read, written or shown |
| `unique([columns])`, `warn_duplicate([columns])` | Unique checks and duplicate Warnings |
| `default_contain([names])` | Associations loaded by `get_detail` and `contain_default` |
| `display_field(column)` | The column used as a row's label |
| `optimistic_lock(false)` | Do not check `modified` on saves from request data |
| `table_spec(\|spec\| ..)` | Override the table rules for this entity |
| `skip_observer(name)` | Skip an observer for this entity |
| `extension(value)` | Attach your own value, read with `Registry::extension` |

```rust
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
```

## 4. Policies

| Method | Default | Decides |
|---|---|---|
| `TablePolicy::can(ctx, action)` | the permission string `entity.<name>.<action>` | whether the action is allowed at all |
| `TablePolicy::scope(ctx, action)` | all rows | which rows can be seen or changed (a SQL condition) |
| `TablePolicy::columns(ctx)` | all columns | readable and writable columns, and columns hidden per row |
| `TablePolicy::can_contain(ctx, name)` | true | whether an association can be loaded |
| `EntityPolicy::can_add`, `can_edit`, `can_delete` | true | per-row decisions with the row's values |

```rust
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
```

- A denied `can` returns `Forbidden` for a logged-in identity and `Unauthenticated` otherwise; rows
  outside the `scope` are `NotFound`.
- `TablePolicy` answers are reused within one `Context`; `EntityPolicy` runs per row.
- `ctx.repo::<R>().unchecked()` skips every Policy decision (for seeds, batches and login).
- `ctx.repo::<R>().can(action)` and `.columns()` read the answers without acting, for example to
  show or hide buttons.

Identities: implement `Identity` on your user type (`user_id`, `has_permission`), or use
`BasicIdentity::user(id, role, PermissionSet::new(["entity.orders.*"]))`, `anonymous()` or
`system()`. `identity.can(..)` is always true for the system and false for anonymous.

## 5. Registry and Context

Build the registry once at startup:

```rust
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
let registry = Arc::new(registry);
```

The two slices map entity names and column names to labels. `verify()` panics on mistakes such as
an association whose target is not registered or a foreign key whose type does not match.

Create a `Context` per request:

```rust
let identity = BasicIdentity::user(1, None, PermissionSet::new(["*"]));
let ctx = Context::new(identity, db.clone(), AuthzMark::default(), registry.clone());
```

`AuthzMark` records that some operation passed authorization; a request handler can check
`ctx.authz().is_marked()` and reject handlers that never checked anything.

## 6. Reading

```rust
let orders = ctx.repo::<OrdersRepo>();
let one = orders.get(order.id).await?;
let recent = orders
    .find()
    .filter(orders::Column::Title.contains("Sec"))
    .order_by_desc(orders::Column::Created)
    .limit(20)
    .all()
    .await?;
let n = orders.find().count().await?;
```

Load associations with `contain` (one `IN` query per association) and read them with `related`:

```rust
let rows = orders
    .find()
    .contain(OrdersRepo::CUSTOMER)
    .contain(OrdersRepo::ITEMS)
    .all()
    .await?;
for row in &rows {
    let customer = row.related(OrdersRepo::CUSTOMER).expect("contained"); // Option<&Loaded<..>>
    let items = row.related(OrdersRepo::ITEMS).expect("contained"); // &[Loaded<..>]
}
let json = orders.to_json(&rows).await?;
let items = orders.load(&recent, OrdersRepo::ITEMS).await?; // order id -> Vec<order_items::Model>
```

- Soft-deleted rows are never read. Targets of associations go through their own Repo's Policy.
- `to_json` returns only the columns the Policy lets this identity read.
- `get_detail(id)` reads one row with the `default_contain` associations.

## 7. Writing

```rust
let order = orders
    .save(orders::ActiveModel {
        customer_id: Set(customer.id),
        title: Set("First".into()),
        ..Default::default()
    })
    .await?;
```

`save` inserts or updates, runs the Policy, `validate`, unique and reference checks and the hooks,
and sets `created`, `modified` and the active flag. Errors are `RepoError`: `Validation(errors)`,
`Forbidden`, `NotFound`, `Duplicate` and so on.

Children and links in one call:

```rust
let item = order_items::ActiveModel { name: Set("Pen".into()), ..Default::default() };
let order = orders
    .save_with(order.into_active_model())
    .replace(OrdersRepo::ITEMS, vec![item])
    .replace_links(OrdersRepo::TAGS, vec![tag.id])
    .save()
    .await?;
orders.link(OrdersRepo::TAGS, &order, vec![tag.id]).await?;
orders.unlink(OrdersRepo::TAGS, &order, vec![tag.id]).await?;
```

Transactions and optimistic locking:

```rust
let tx = ctx.begin().await?;
let current = tx.repo::<OrdersRepo>().get(order.id).await?;
let mut am = current.clone().into_active_model();
am.title = Set("Second".into());
tx.repo::<OrdersRepo>()
    .save_if_unmodified(am, current.modified)
    .await?; // Conflict if someone saved in between
tx.commit().await?;
```

Dropping a `Transaction` without `commit` rolls back. Nested `begin` uses a savepoint.

Deleting:

| Method | `SoftDelete` | `Activatable` | `HardDelete` |
|---|---|---|---|
| `delete(id)` | sets the active flag to false | `NotDeletable` | deletes the row |
| `purge(id)` | deletes the row (also when already deactivated) | deletes the row | deletes the row |

Both check `can(Delete)`, the `scope` and `can_delete`, run the delete hooks, and delete
`dependent` children (`purge` deletes them physically too).

## 8. Saving request data

`patch` turns a JSON body into an ActiveModel with the first-stage validation; `save_patch`
saves it in one transaction:

```rust
let body = json!({
    "data": {"title": "Third", "items": [{"name": "Ink"}]},
    "modified": current_modified.to_rfc3339(),
    "associated": ["items"],
});
let patch = orders.patch(Some(order.id), &body).await?;
let saved = orders.save_patch(patch).await?;
```

| Key | Meaning |
|---|---|
| `data` | Column values, and association values by name |
| `modified` | The `modified` value read before editing; required on updates while the optimistic lock is on |
| `associated` | Association paths to save (`"items"`, `"orders.items"`); defaults to one level |
| `acceptWarnings` | Warning keys the user has accepted |

`PatchError` tells what went wrong: `Invalid(FieldErrors)`, `Conflict { current }`,
`Warnings(..)`, `BadRequest(..)` or `Repo(RepoError)`. `FieldErrors` serializes as
`{"title": [{"code": "required", "message": "This field is required"}]}`; errors with values add
`params` (for example `{"max": 50, "unit": "chars"}`).

## 9. Hooks and observers

| Hook | When |
|---|---|
| `before_find` | Before rows are read: add conditions and ordering |
| `before_marshal`, `after_marshal` | Around the conversion of request data |
| `validate` | Second-stage validation of every save (part of the rules stage) |
| `warnings` | Warnings for saves from request data |
| `before_rules`, `after_rules` | Around the rules stage (`validate`, reference and unique checks); `after_rules` may change the errors |
| `before_save`, `after_save` | Around the write, inside the transaction |
| `after_save_commit`, `after_delete_commit` | After the outermost transaction commits |
| `before_delete`, `after_delete` | Around a delete or purge |

Observers run for every entity's saves and deletes, inside the transaction:

```rust
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
```

`change.before` and `change.after` hold the row as JSON. Write with SeaORM directly inside an
observer; writing through a Repo would call the observers again.

## 10. Messages

Validation errors carry a `code`, `params` and a `message`. Default messages are English; pass a
table to change them:

```rust
registry.set_messages(&[("required", "必須です"), ("max_length", "{max} 文字以内")]);
```

`cakedome::messages::DEFAULTS` lists every key and its template. Keys you leave out keep the
default.

## 11. Values on the Context

Give Policies and hooks values that are not about who is asking:

```rust
struct RequestInfo {
    locale: String,
}

let ctx = Context::new(identity, db, AuthzMark::default(), registry)
    .with_extension(RequestInfo { locale: "ja".into() });
let locale = ctx.extension::<RequestInfo>().map(|r| r.locale.as_str());
```

Values are inherited by transactions and after-commit hooks. Set them when you create the
Context.

## 12. Building layers

`cakedome::layer` has the building blocks for a generic layer over all registered entities, such
as a JSON API that finds Repos by name: `JsonFor`, contain trees, `load_related`, `read_in` and
the value conversions. They may change during 0.x. Business code does not need them.
