# cakedome の使い方

[English](guide.md)

アプリで cakedome を使う手順を、順に説明します。例では次の5つの表を使います。

| 表 | 列 | 種類 |
|---|---|---|
| `customers` | `id`・`name`・`email`・`is_active`・`created`・`modified` | 論理削除 |
| `orders` | `id`・`customer_id`・`title`・`is_active`・`created`・`modified` | 論理削除 |
| `order_items` | `id`・`order_id`・`name`・`seq`・`is_active`・`created`・`modified` | 論理削除、並び順あり |
| `master_tags` | `id`・`code`・`name`・`is_active` | マスタ（無効にするだけで、消さない） |
| `setting_order_tags` | `id`・`order_id`・`tag_id`・`seq` | 注文とタグの中間テーブル |

Entity は、ふつうの SeaORM の Entity（`DeriveEntityModel`。`Relation` は空）です。関連は、Entity ではなく Repo に宣言します。

1. [準備](#1-準備)
2. [テーブルの規則](#2-テーブルの規則)
3. [Repo](#3-repo)
4. [Policy](#4-policy)
5. [Registry と Context](#5-registry-と-context)
6. [読む](#6-読む)
7. [書く](#7-書く)
8. [リクエストの値を保存する](#8-リクエストの値を保存する)
9. [フックと observer](#9-フックと-observer)
10. [文言](#10-文言)
11. [Context に入れる値](#11-context-に入れる値)
12. [層を作る](#12-層を作る)

## 1. 準備

```toml
[dependencies]
cakedome = "0.1"
sea-orm = { version = "2.0", features = ["macros", "sqlx-mysql", "runtime-tokio-rustls"] }
```

cakedome は、DB のドライバーと非同期のランタイムを選びません。アプリが使うものを SeaORM の側で有効にしてください。よく使う部品は、次の2行で入ります。

```rust
use cakedome::prelude::*;
use sea_orm::entity::prelude::*;
```

## 2. テーブルの規則

cakedome は命名の規則を知りません。`TableRules` の実装を1つ書き、表の名前と列から `TableSpec` を作ります。

| `TableSpec` の field | 意味 |
|---|---|
| `lifecycle` | `SoftDelete`（削除で有効フラグを false にする）、`Activatable`（無効にするだけで、消さない）、`HardDelete` |
| `active_column` | 有効フラグ。`HardDelete` のほかは必須 |
| `created_column`・`modified_column` | cakedome が入れる日時。`modified` は楽観ロックにも使う |
| `order_column` | 子や中間テーブルの行の並び順を持つ `i32` の列 |
| `defaults` | 追加のときに省いた列の既定値 |
| `uniques` | 保存の前に一意を確かめる列の組 |
| `warn_duplicates` | 重なったら Warning を出す列の組 |

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

表が規則に合わないときは `Err` を返すと、起動のときに止まります。1つの表だけ変えたいときは、`RepoConfig::table_spec` で上書きできます。

並び順の列の名前は自由です（`seq` はこの例の決まりです）。多対多の中間テーブルには、並び順の列が要ります。

## 3. Repo

Entity ごとに Repo を書きます。設定・検証・フック（`impl Repo`）と、アクセスの決まり（`impl TablePolicy`・`impl EntityPolicy`）です。`DefaultRepo<E>` は、すべてが既定の Repo です。

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

関連は Repo の定数として宣言するので、外部キーは型付きの列になります。

| 宣言 | 外部キーがある表 | 設定 |
|---|---|---|
| `BelongsToOf<S, T>` | `S`（この Entity） | `binding_key`・`save`・`finder` |
| `HasManyOf<S, T>` | `T`（子） | `save(方式)`・`dependent`・`cascade_callbacks`・`filter`・`sort`・`label`・`binding_key`・`finder` |
| `HasOneOf<S, T>` | `T`（子） | `save()`・`dependent`・`cascade_callbacks`・`filter`・`label`・`binding_key`・`finder` |
| `ManyToManyOf<S, J, T>` | `J`（中間テーブル） | `save(方式)`・`dependent`・`cascade_callbacks`・`filter`・`sort`・`label`・`binding_key`・`finder` |

`SaveStrategy::Replace` は、送った子を全体とします（送らなかった子は外し、並び順の列を振り直します）。`Append` は足すか直すだけです。`dependent(true)` にすると、親と一緒に子も消します。

`initialize` で使えるほかの設定:

| メソッド | 用途 |
|---|---|
| `label`・`labels`・`field(列, \|f\| f.label(..).default(..))` | ラベルと、項目ごとの設定 |
| `default(列, json)`・`default_from(列, 動的な既定値)` | 追加のときの、固定の既定値とリクエストごとの既定値（`CurrentUserId`・`RefBy`・自分の `DynamicDefault`） |
| `hidden(列)` | 読まない・書かない・見せない |
| `unique([列])`・`warn_duplicate([列])` | 一意の確かめと、重なりの Warning |
| `default_contain([名前])` | `get_detail` と `contain_default` で読む関連 |
| `display_field(列)` | 行のラベルに使う列 |
| `optimistic_lock(false)` | リクエストの値からの保存で `modified` を確かめない |
| `table_spec(\|spec\| ..)` | この Entity だけテーブルの規則を上書きする |
| `skip_observer(名前)` | この Entity では observer を呼ばない |
| `extension(値)` | アプリの値を付ける（`Registry::extension` で読む） |

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

## 4. Policy

| メソッド | 既定 | 決めること |
|---|---|---|
| `TablePolicy::can(ctx, action)` | 権限の文字列 `entity.<名前>.<操作>` | その操作をしてよいか |
| `TablePolicy::scope(ctx, action)` | すべての行 | 見える・変えられる行（SQL の条件） |
| `TablePolicy::columns(ctx)` | すべての列 | 読める列・書ける列と、行ごとに隠す列 |
| `TablePolicy::can_contain(ctx, name)` | true | その関連を読んでよいか |
| `EntityPolicy::can_add`・`can_edit`・`can_delete` | true | 行の値を見て、行ごとに決める |

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

- `can` が偽なら、ログイン中は `Forbidden`、未ログインは `Unauthenticated` になります。`scope` の外の行は `NotFound` です。
- `TablePolicy` の答えは、1つの `Context` の中で使い回します。`EntityPolicy` は行ごとに呼びます。
- `ctx.repo::<R>().unchecked()` は、Policy の判定をすべて飛ばします（シード・バッチ・ログイン用）。
- `ctx.repo::<R>().can(action)` と `.columns()` で、操作をせずに答えだけを読めます（ボタンを出すかどうか、など）。

Identity: アプリのユーザーの型に `Identity`（`user_id`・`has_permission`）を実装するか、`BasicIdentity::user(id, role, PermissionSet::new(["entity.orders.*"]))`・`anonymous()`・`system()` を使います。`identity.can(..)` は、システムなら常に true、未ログインなら常に false です。

## 5. Registry と Context

Registry は、起動のときに1回作ります。

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

2つの配列は、Entity の名前と列の名前からラベルへの辞書です。`verify()` は、関連の相手が登録されていない、外部キーの型が合わない、などの誤りで panic します。

リクエストごとに `Context` を作ります。

```rust
let identity = BasicIdentity::user(1, None, PermissionSet::new(["*"]));
let ctx = Context::new(identity, db.clone(), AuthzMark::default(), registry.clone());
```

`AuthzMark` は、どれかの操作が認可を通ったことの印です。リクエストの処理の側で `ctx.authz().is_marked()` を見れば、何も確かめなかった処理を断れます。

## 6. 読む

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

関連は `contain` で読み（関連ごとに `IN` の1回のクエリ）、`related` で取り出します。

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

- 論理削除した行は読みません。関連の相手も、相手の Repo の Policy を通ります。
- `to_json` は、この Identity が読める列だけを出します。
- `get_detail(id)` は、`default_contain` の関連を付けて1件を読みます。

## 7. 書く

```rust
let order = orders
    .save(orders::ActiveModel {
        customer_id: Set(customer.id),
        title: Set("First".into()),
        ..Default::default()
    })
    .await?;
```

`save` は追加か更新をします。Policy・`validate`・一意と参照先の確かめ・フックを通し、`created`・`modified`・有効フラグを入れます。エラーは `RepoError` で、`Validation(errors)`・`Forbidden`・`NotFound`・`Duplicate` などです。

子と中間テーブルの行も、まとめて保存できます。

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

トランザクションと楽観ロック:

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

`commit` せずに `Transaction` を捨てると、取り消されます。入れ子の `begin` は savepoint になります。

削除:

| メソッド | `SoftDelete` | `Activatable` | `HardDelete` |
|---|---|---|---|
| `delete(id)` | 有効フラグを false にする | `NotDeletable` | 行を消す |
| `purge(id)` | 行を消す（無効にした行も） | 行を消す | 行を消す |

どちらも `can(Delete)`・`scope`・`can_delete` を確かめ、削除のフックを呼び、`dependent` の子も消します（`purge` は子も物理削除します）。

## 8. リクエストの値を保存する

`patch` は JSON の本文を、1段目の検証をしながら ActiveModel にします。`save_patch` は、それを1つのトランザクションで保存します。

```rust
let body = json!({
    "data": {"title": "Third", "items": [{"name": "Ink"}]},
    "modified": current_modified.to_rfc3339(),
    "associated": ["items"],
});
let patch = orders.patch(Some(order.id), &body).await?;
let saved = orders.save_patch(patch).await?;
```

| キー | 意味 |
|---|---|
| `data` | 列の値と、関連の名前ごとの値 |
| `modified` | 編集の前に読んだ `modified`。楽観ロックが有効な表の更新では必須 |
| `associated` | 保存する関連の道のり（`"items"`・`"orders.items"`）。無ければ1段目だけ |
| `acceptWarnings` | ユーザーが受け入れた Warning のキー |

何が起きたかは `PatchError` で分かります。`Invalid(FieldErrors)`・`Conflict { current }`・`Warnings(..)`・`BadRequest(..)`・`Repo(RepoError)` です。`FieldErrors` は `{"title": [{"code": "required", "message": "This field is required"}]}` の形の JSON になり、値のあるエラーには `params`（例: `{"max": 50, "unit": "chars"}`）が付きます。

## 9. フックと observer

| フック | 呼ばれるとき |
|---|---|
| `before_find` | 行を読む前。条件と並びを足す |
| `before_marshal`・`after_marshal` | リクエストの値の変換の前と後 |
| `validate` | すべての保存の、検証の2段目（ルールの段の一部） |
| `warnings` | リクエストの値からの保存の Warning |
| `before_rules`・`after_rules` | ルールの段（`validate`・参照先・一意の確かめ）の前と後。`after_rules` はエラーを直せる |
| `before_save`・`after_save` | 書き込みの前と後（トランザクションの中） |
| `after_save_commit`・`after_delete_commit` | いちばん外のトランザクションが確定した後 |
| `before_delete`・`after_delete` | 削除と purge の前と後 |

observer は、すべての Entity の保存と削除で、トランザクションの中で呼ばれます。

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

`change.before` と `change.after` に、行が JSON で入ります。observer の中では SeaORM で直接書いてください。Repo を通して書くと、その書き込みでもう一度 observer が呼ばれます。

## 10. 文言

検証のエラーには `code`・`params`・`message` があります。既定の文言は英語で、表を渡すと変えられます。

```rust
registry.set_messages(&[("required", "必須です"), ("max_length", "{max} 文字以内")]);
```

キーと既定の型紙の一覧は `cakedome::messages::DEFAULTS` にあります。表に無いキーは既定の文言のままです。

## 11. Context に入れる値

「誰か」ではない値を、Policy やフックに渡せます。

```rust
struct RequestInfo {
    locale: String,
}

let ctx = Context::new(identity, db, AuthzMark::default(), registry)
    .with_extension(RequestInfo { locale: "ja".into() });
let locale = ctx.extension::<RequestInfo>().map(|r| r.locale.as_str());
```

入れた値は、トランザクションと確定の後のフックにも引き継ぎます。値は Context を作るときに入れてください。

## 12. 層を作る

`cakedome::layer` には、登録したすべての Entity を扱う汎用の層（名前で Repo を引く JSON の API など）を作るための部品があります。`JsonFor`・contain の木・`load_related`・`read_in`・値の変換などです。0.x の間は変わることがあります。業務コードでは使いません。
