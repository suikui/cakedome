# cakedome

[English](README.md)

[SeaORM](https://www.sea-ql.org/SeaORM/) の上の ORM の層です。Entity の読み書きを、すべて Policy を持つ Repo に通します。

[CakePHP](https://cakephp.org/) の ORM に着想を得ています。関連を持つテーブルのクラス、`contain` での関連の読み込み、リクエストの値を検証しながら Entity にすること、入れ子の保存を、型付きの宣言で Rust に書き直しました。

状態: 0.x。マイナーバージョンの間で API が変わることがあります。

このリポジトリのコードは、テストと文書を含めて、すべて AI がメンテナーの指示で書いています。

## 特徴

- **読み書きは、必ず Policy を通ります。** `TablePolicy` は、操作をしてよいか（`can`）、どの行を見たり変えたりできるか（`scope`）、どの列を読み書きできるか（`columns`）を決めます。`EntityPolicy` は、行の値を見て、その行を追加・変更・削除してよいかを決めます。`ctx.repo::<R>()` で行う読み書きは、Policy を省けません。省けるのは、`unchecked()` と明示して作ったハンドル（シードやバッチ用）だけです。
- **型付きの関連。** `BelongsToOf`・`HasManyOf`・`HasOneOf`・`ManyToManyOf` を、型付きの列で定数として宣言します。外部キーを間違えるとコンパイルが通りません。関連ごとに `IN` の1回のクエリで読み（`load`・`contain`）、関連をまたいで絞り込み、入れ子の行を保存します（`save_with`・`link`・`unlink`）。
- **テーブルの扱い方。** 論理削除・有効フラグを持つマスタ・物理削除を、アプリのテーブルの規則で決めます。作成日時と更新日時は cakedome が入れます。
- **JSON の入出力。** リクエストの値を ActiveModel にし、検証のエラーをコードと値（`required`、`max_length {max}` など）で返します。行は、Policy で読める列だけの JSON にします。既定の文言は英語で、表を渡せば訳せます。
- **フックと observer。** `before_find`・`before_save`・`after_save`・確定の後のフックと、すべての変更で呼ぶ observer。
- `save_if_unmodified` での**楽観ロック**。
- **SQLite・PostgreSQL・MySQL。** テストは3つすべてで回しています。

## cakedome がすること・アプリが書くこと

cakedome がすること:

- すべての読み書きで Policy を呼びます。`TablePolicy` の答えは、1つの `Context` の中で使い回します。
- SeaORM で行を読み書きします。論理削除、作成・更新日時、楽観ロック、トランザクション（入れ子は savepoint）、確定の後のフックを扱います。
- 関連を読み、関連をまたいで絞り込み、保存します。入れ子の保存と、親を消したときの子の削除も含みます。
- リクエストの値を ActiveModel にし（型・長さの確かめ、一意・参照先の確かめ）、行を JSON にします。
- 起動のときに登録を確かめます。列と Rust の型、関連のキー、既定の `contain` の道のり。

アプリが書くこと:

- SeaORM の Entity と、DB の表（migration）。cakedome は表を作りません。
- `TableRules` の実装を1つ。命名の規則で、テーブルごとの扱い方・cakedome が入れる列・並び順の列・既定値・一意の組を決めます。
- Entity ごとの Repo（`impl Repo`・`TablePolicy`・`EntityPolicy`）。既定でよければ `DefaultRepo`。ラベル・既定値・関連・検証・フック・アクセスの決まりを書きます。
- ユーザーの型（`impl Identity`）。または、権限の文字列で判定する `BasicIdentity`。
- 起動のとき: `Registry` を作り、必要なら訳した文言（`set_messages`）と observer を足して、`verify()` を呼びます。
- リクエストごと: Identity、使う DB の接続、Policy が読む値（`with_extension`）を持つ `Context` を作ります。
- ORM の外のこと: HTTP、認証とセッション、`RepoError` を応答にすること、汎用の JSON API（欲しければ。部品は `layer` にあります）。

## 例

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

`customers` と `orders` は、`is_active`・`created`・`modified` の列を持つ、ふつうの SeaORM の Entity です。[`examples/basic.rs`](examples/basic.rs) が、そのまま動く完全なプログラムです。

使い方の手順（テーブルの規則・Repo・Policy・読む・書く・リクエストの値の保存・フック・文言）は [使い方](docs/guide.ja.md) にあります。

## 主な型

| | |
|---|---|
| `Repo` | Entity ごとの設定: ラベル・既定値・関連・検証・フック |
| `TablePolicy`・`EntityPolicy` | 誰が、どの行と列に、何をしてよいか |
| `Registry` | すべての Repo。起動のときに1回作り、`verify()` で確かめる |
| `Context` | 1つのリクエストの Identity・接続・アプリの値。`ctx.repo::<R>()` が `RepoHandle` を返す |
| `Identity` | アプリのユーザーの型。`BasicIdentity` は `entity.orders.edit` のような権限の文字列で判定する |
| `AuthzMark` | 操作が認可を通ったことの印。リクエストの処理の側で確かめられる |
| `layer` | ORM の上に層（JSON の API など）を作るための部品 |

## DB ごとの注意

- MySQL では、日時の列を `DATETIME(6)` にしてください。`TIMESTAMP`・`DATETIME` は秒に丸めるので、楽観ロックが効かなくなります。
- SQLite には行のロックがありません。同時の `save_if_unmodified` は、`Conflict` ではなく `database is locked` で失敗します。先の保存を上書きはしません。

## 開発

`cargo test` は SQLite のテストを回し、準備は要りません。PostgreSQL と MySQL のテストは同じ DB のテストを回し、feature を付けたときだけ作ります。テストごとに `cakedome_test_*` のスキーマか DB を作ります。

```sh
CAKEDOME_TEST_PG_URL=postgres://user:pass@localhost:5432/postgres \
CAKEDOME_TEST_MYSQL_URL=mysql://root:pass@localhost:3306/mysql \
cargo test --all-features
```

## Issue と PR

Issue は歓迎します。PR は受け付けておらず、自動で閉じます。代わりに Issue を開いてください。返事に時間がかかることがあります。詳しくは [CONTRIBUTING.md](CONTRIBUTING.md) を、脆弱性の報告は [SECURITY.md](SECURITY.md) を見てください。

## ライセンス

[Apache License, Version 2.0](LICENSE-APACHE) と [MIT license](LICENSE-MIT) の、どちらかを選んで使えます。

あなたが明示しない限り、この crate に含めるために意図して送った貢献（Apache-2.0 のライセンスの定義による）は、追加の条件なしに、上の2つのライセンスで提供されます。
