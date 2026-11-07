//! FieldMeta, registration settings, JSON conversion, first-stage validation and Schema. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::marshal::{FieldErrors, WriteMode, defaults_json, from_json, to_json};
use cakedome::messages::Messages;
use cakedome::meta::{FieldKind, FieldMeta};
use cakedome::registry::Registry;
use chrono::{NaiveDate, TimeZone, Utc};
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::prelude::Decimal;
use serde_json::{Map, Value, json};

mod samples {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "samples")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(10))")]
        pub name: String,
        #[sea_orm(column_type = "String(StringLen::N(255))", nullable)]
        pub email: Option<String>,
        #[sea_orm(column_type = "Text", nullable)]
        pub note: Option<String>,
        pub credit_limit: Option<i32>,
        pub points: i64,
        pub active: bool,
        pub started_on: Option<Date>,
        pub happened_at: Option<DateTimeUtc>,
        #[sea_orm(column_type = "String(StringLen::N(100))", nullable)]
        pub secret: Option<String>,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod unsized_strings {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "unsized_strings")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod amounts {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "amounts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "Decimal(Some((10, 2)))")]
        pub price: Decimal,
        #[sea_orm(column_type = "Decimal(Some((5, 0)))", nullable)]
        pub count: Option<Decimal>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod unsized_decimals {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "unsized_decimals")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub amount: Decimal,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod measures {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "measures")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub ratio: f64,
        pub weight: Option<f64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod configs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "configs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub data: Json,
        pub extra: Option<Json>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn registry() -> Registry {
    let mut r = common::registry();
    r.add(DefaultRepo::<samples::Entity>::new())
        .label("サンプル")
        .field(samples::Column::Name, |f| f.label("名前"))
        .field(samples::Column::Active, |f| {
            f.label("有効").default(json!(true))
        })
        .field(samples::Column::Points, |f| f.default(json!(0)))
        .hidden(samples::Column::Secret);
    r
}

fn fields() -> Vec<FieldMeta> {
    registry()
        .fields_of::<samples::Entity>()
        .expect("registered")
        .to_vec()
}

fn field<'a>(fields: &'a [FieldMeta], name: &str) -> &'a FieldMeta {
    fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} exists"))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn all(_: &FieldMeta) -> bool {
    true
}

fn sample() -> samples::Model {
    samples::Model {
        id: 7,
        name: "田中".into(),
        email: Some("tanaka@example.com".into()),
        note: Some("メモ".into()),
        credit_limit: Some(1000),
        points: 9_007_199_254_740_991,
        active: true,
        started_on: Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
        happened_at: Some(Utc.with_ymd_and_hms(2026, 10, 17, 1, 2, 3).unwrap()),
        secret: Some("見せない".into()),
        created: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        modified: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
            + chrono::Duration::microseconds(123_456),
        is_active: true,
    }
}

fn codes(errors: &FieldErrors) -> Vec<(String, Vec<&'static str>)> {
    errors
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|e| e.code).collect()))
        .collect()
}

fn add(input: Value) -> Result<samples::ActiveModel, FieldErrors> {
    from_json::<samples::Entity>(&fields(), &obj(input), WriteMode::Add, all)
}

fn edit(input: Value) -> Result<samples::ActiveModel, FieldErrors> {
    from_json::<samples::Entity>(&fields(), &obj(input), WriteMode::Edit, all)
}

fn add_err(input: Value) -> Vec<(String, Vec<&'static str>)> {
    codes(&add(input).expect_err("validation fails"))
}

fn edit_err(input: Value) -> Vec<(String, Vec<&'static str>)> {
    codes(&edit(input).expect_err("validation fails"))
}

fn one(name: &str, code: &'static str) -> Vec<(String, Vec<&'static str>)> {
    vec![(name.to_string(), vec![code])]
}

#[test]
fn fields_of_reads_column_definitions() {
    let fs = common::fields::<samples::Entity>();
    let names: Vec<&str> = fs.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        [
            "id",
            "name",
            "email",
            "note",
            "credit_limit",
            "points",
            "active",
            "started_on",
            "happened_at",
            "secret",
            "created",
            "modified",
            "is_active"
        ],
        "column definition order"
    );

    let kinds: Vec<(&str, FieldKind, bool)> =
        fs.iter().map(|f| (f.name, f.kind, f.nullable)).collect();
    assert_eq!(
        kinds,
        [
            ("id", FieldKind::BigInteger, false),
            ("name", FieldKind::String { max_length: 10 }, false),
            ("email", FieldKind::String { max_length: 255 }, true),
            ("note", FieldKind::Text, true),
            ("credit_limit", FieldKind::Integer, true),
            ("points", FieldKind::BigInteger, false),
            ("active", FieldKind::Boolean, false),
            ("started_on", FieldKind::Date, true),
            ("happened_at", FieldKind::DateTime, true),
            ("secret", FieldKind::String { max_length: 100 }, true),
            ("created", FieldKind::DateTime, false),
            ("modified", FieldKind::DateTime, false),
            ("is_active", FieldKind::Boolean, false),
        ]
    );

    let id = field(&fs, "id");
    assert!(id.primary_key && !id.managed);
    for name in ["created", "modified", "is_active"] {
        let f = field(&fs, name);
        assert!(f.managed && !f.primary_key, "{name}");
    }
    let name = field(&fs, "name");
    assert!(!name.primary_key && !name.managed && !name.hidden);
    assert_eq!(name.label, "name", "label defaults to the column name");
    assert_eq!(name.default, None);
}

#[test]
fn field_kind_names() {
    assert_eq!(FieldKind::String { max_length: 1 }.as_str(), "string");
    assert_eq!(FieldKind::Text.as_str(), "text");
    assert_eq!(FieldKind::Integer.as_str(), "integer");
    assert_eq!(FieldKind::BigInteger.as_str(), "big_integer");
    assert_eq!(FieldKind::Boolean.as_str(), "boolean");
    assert_eq!(FieldKind::Date.as_str(), "date");
    assert_eq!(FieldKind::DateTime.as_str(), "datetime");
}

#[test]
#[should_panic(expected = "unsized_strings.name")]
fn string_column_without_length_is_rejected() {
    common::fields::<unsized_strings::Entity>();
}

#[test]
fn registry_sets_label_field_label_default_and_hidden() {
    let r = registry();
    assert_eq!(r.label_of::<samples::Entity>(), Some("サンプル"));

    let fs = r.fields_of::<samples::Entity>().unwrap();
    assert_eq!(field(fs, "name").label, "名前");
    assert_eq!(field(fs, "email").label, "email");
    assert_eq!(field(fs, "active").label, "有効");
    assert_eq!(field(fs, "active").default, Some(json!(true)));
    assert_eq!(field(fs, "points").default, Some(json!(0)));
    assert!(field(fs, "secret").hidden);
    assert!(!field(fs, "email").hidden);
}

#[test]
fn registry_label_defaults_to_table_name() {
    let mut r = common::registry();
    r.add(DefaultRepo::<samples::Entity>::new());

    assert_eq!(r.label_of::<samples::Entity>(), Some("samples"));
    assert_eq!(r.fields_of::<samples::Entity>().unwrap().len(), 13);
    assert_eq!(r.label_of::<unsized_strings::Entity>(), None);
    assert!(r.fields_of::<unsized_strings::Entity>().is_none());
}

// ---- Model → JSON ----

#[test]
fn to_json_formats_each_type_and_skips_hidden_and_is_active() {
    let json = to_json::<samples::Entity>(&fields(), &sample(), all);

    assert_eq!(
        Value::Object(json),
        json!({
            "id": 7,
            "name": "田中",
            "email": "tanaka@example.com",
            "note": "メモ",
            "credit_limit": 1000,
            "points": 9_007_199_254_740_991_i64,
            "active": true,
            "started_on": "2026-10-01",
            "happened_at": "2026-10-17T01:02:03.000000Z",
            "created": "2026-01-02T03:04:05.000000Z",
            "modified": "2026-01-02T03:04:05.123456Z",
        })
    );
}

#[test]
fn to_json_writes_null_for_null_values() {
    let mut m = sample();
    m.email = None;
    m.note = None;
    m.credit_limit = None;
    m.started_on = None;
    m.happened_at = None;

    let json = to_json::<samples::Entity>(&fields(), &m, all);

    for name in ["email", "note", "credit_limit", "started_on", "happened_at"] {
        assert_eq!(json.get(name), Some(&Value::Null), "{name}");
    }
}

#[test]
fn to_json_skips_unreadable_fields() {
    let json = to_json::<samples::Entity>(&fields(), &sample(), |f| {
        !matches!(f.name, "email" | "credit_limit")
    });

    assert!(!json.contains_key("email"));
    assert!(!json.contains_key("credit_limit"));
    assert!(json.contains_key("name"));
}

#[test]
fn add_converts_each_type() {
    let am = add(json!({
        "name": "田中",
        "email": "tanaka@example.com",
        "note": "メモ",
        "credit_limit": -5,
        "points": 9_007_199_254_740_991_i64,
        "active": false,
        "started_on": "2026-10-01",
        "happened_at": "2026-10-17T10:02:03.1234567+09:00",
    }))
    .unwrap();

    assert_eq!(am.id, NotSet);
    assert_eq!(am.name, Set("田中".to_string()));
    assert_eq!(am.email, Set(Some("tanaka@example.com".to_string())));
    assert_eq!(am.note, Set(Some("メモ".to_string())));
    assert_eq!(am.credit_limit, Set(Some(-5)));
    assert_eq!(am.points, Set(9_007_199_254_740_991));
    assert_eq!(am.active, Set(false));
    assert_eq!(
        am.started_on,
        Set(Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()))
    );
    // Converted to UTC and truncated to microseconds.
    assert_eq!(
        am.happened_at,
        Set(Some(
            Utc.with_ymd_and_hms(2026, 10, 17, 1, 2, 3).unwrap()
                + chrono::Duration::microseconds(123_456)
        ))
    );
    assert_eq!(am.secret, NotSet);
    assert_eq!(am.created, NotSet);
    assert_eq!(am.modified, NotSet);
    assert_eq!(am.is_active, NotSet);
}

#[test]
fn add_fills_defaults_and_requires_not_null_fields() {
    let am = add(json!({"name": "田中"})).unwrap();
    assert_eq!(am.active, Set(true), "default is applied");
    assert_eq!(am.points, Set(0));
    assert_eq!(
        am.email, NotSet,
        "nullable column without default is not set"
    );

    let am = add(json!({"name": "田中", "active": false})).unwrap();
    assert_eq!(am.active, Set(false));

    assert_eq!(add_err(json!({})), one("name", "required"));
    assert_eq!(add_err(json!({"name": null})), one("name", "required"));
    assert_eq!(add_err(json!({"name": ""})), one("name", "required"));
    assert_eq!(
        add_err(json!({"name": "田中", "active": null})),
        one("active", "required")
    );
}

#[test]
fn add_fills_defaults_even_for_unwritable_fields() {
    // Defaults are system-supplied, so read-only columns get them too; those are never `required`.
    let am = from_json::<samples::Entity>(&fields(), &obj(json!({})), WriteMode::Add, |f| {
        !matches!(f.name, "active" | "name")
    })
    .unwrap();

    assert_eq!(am.active, Set(true));
    assert_eq!(am.name, NotSet);
}

#[test]
fn edit_leaves_missing_keys_unset() {
    let am = edit(json!({"name": "鈴木"})).unwrap();

    assert_eq!(am.name, Set("鈴木".to_string()));
    assert_eq!(am.id, NotSet, "id comes from the URL, not from JSON");
    assert_eq!(am.active, NotSet, "no defaults on update");
    assert_eq!(am.email, NotSet);
    assert_eq!(
        edit(json!({})).unwrap(),
        samples::ActiveModel {
            ..Default::default()
        }
    );
}

#[test]
fn null_and_empty_string_clear_nullable_fields() {
    let am =
        edit(json!({"email": null, "note": "", "credit_limit": null, "started_on": null})).unwrap();

    assert_eq!(am.email, Set(None));
    assert_eq!(
        am.note,
        Set(None),
        "empty string in a nullable string column becomes NULL"
    );
    assert_eq!(am.credit_limit, Set(None));
    assert_eq!(am.started_on, Set(None));

    assert_eq!(edit_err(json!({"name": null})), one("name", "required"));
    assert_eq!(edit_err(json!({"name": ""})), one("name", "required"));
    assert_eq!(edit_err(json!({"points": null})), one("points", "required"));
}

#[test]
fn wrong_types_are_rejected() {
    for (input, name) in [
        (json!({"name": 1}), "name"),
        (json!({"email": true}), "email"),
        (json!({"credit_limit": "1"}), "credit_limit"),
        (json!({"credit_limit": 1.5}), "credit_limit"),
        (json!({"credit_limit": 2_147_483_648_i64}), "credit_limit"),
        (json!({"points": "1"}), "points"),
        (json!({"points": 1.0e20}), "points"),
        (json!({"active": "true"}), "active"),
        (json!({"active": 1}), "active"),
        (json!({"started_on": "2026/10/01"}), "started_on"),
        (json!({"started_on": "2026-02-30"}), "started_on"),
        (json!({"happened_at": "2026-10-17"}), "happened_at"),
        (json!({"happened_at": "2026-10-17T01:02:03"}), "happened_at"),
    ] {
        assert_eq!(edit_err(input.clone()), one(name, "type"), "{input}");
    }
}

#[test]
fn integer_bounds_are_accepted() {
    let am = edit(json!({"credit_limit": 2_147_483_647, "points": i64::MIN})).unwrap();

    assert_eq!(am.credit_limit, Set(Some(i32::MAX)));
    assert_eq!(am.points, Set(i64::MIN));
}

#[test]
fn string_length_is_counted_in_characters() {
    let am = edit(json!({"name": "あいうえおかきくけこ"})).unwrap();
    assert_eq!(
        am.name,
        Set("あいうえおかきくけこ".to_string()),
        "10 chars pass"
    );

    let errors = edit(json!({"name": "あいうえおかきくけこさ"})).unwrap_err();
    assert_eq!(codes(&errors), one("name", "max_length"));
    assert_eq!(errors.0["name"][0].message, "Must be at most 10 characters");
}

#[test]
fn text_is_limited_to_65535_bytes() {
    assert!(edit(json!({"note": "a".repeat(65_535)})).is_ok());
    assert_eq!(
        edit_err(json!({"note": "a".repeat(65_536)})),
        one("note", "max_length")
    );
    // A hiragana char is 3 bytes in UTF-8: 21,846 chars = 65,538 bytes.
    assert_eq!(
        edit_err(json!({"note": "あ".repeat(21_846)})),
        one("note", "max_length")
    );
}

#[test]
fn unknown_hidden_and_read_only_keys_are_rejected() {
    assert_eq!(edit_err(json!({"foo": 1})), one("foo", "unknown"));
    assert_eq!(
        edit_err(json!({"secret": "x"})),
        one("secret", "unknown"),
        "hidden columns are treated as absent (existence not revealed)"
    );
    for name in ["id", "created", "modified", "is_active"] {
        assert_eq!(
            edit_err(json!({name: null})),
            one(name, "read_only"),
            "{name}"
        );
        assert_eq!(
            add_err(json!({"name": "田中", name: null})),
            one(name, "read_only"),
            "{name}"
        );
    }

    let errors = from_json::<samples::Entity>(
        &fields(),
        &obj(json!({"email": "a@example.com"})),
        WriteMode::Edit,
        |f| f.name != "email",
    )
    .unwrap_err();
    assert_eq!(
        codes(&errors),
        one("email", "read_only"),
        "read-only column"
    );
}

#[test]
fn errors_are_collected_for_all_fields_with_messages() {
    let errors = add(json!({
        "name": "あいうえおかきくけこさ",
        "credit_limit": "x",
        "foo": 1,
        "id": 1,
    }))
    .unwrap_err();

    assert_eq!(
        codes(&errors),
        vec![
            ("credit_limit".to_string(), vec!["type"]),
            ("foo".to_string(), vec!["unknown"]),
            ("id".to_string(), vec!["read_only"]),
            ("name".to_string(), vec!["max_length"]),
        ]
    );
    let message = |name: &str| errors.0[name][0].message.clone();
    assert_eq!(message("credit_limit"), "The value has an invalid format");
    assert_eq!(message("foo"), "This field does not exist");
    assert_eq!(message("id"), "This field cannot be changed");

    let errors = add(json!({})).unwrap_err();
    assert_eq!(errors.0["name"][0].message, "This field is required");
}

#[test]
fn field_errors_serialize_as_map_of_lists() {
    let errors = add(json!({"foo": 1, "name": ""})).unwrap_err();

    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "foo": [{"code": "unknown", "message": "This field does not exist"}],
            "name": [{"code": "required", "message": "This field is required"}],
        })
    );
}

#[test]
fn defaults_json_lists_input_fields_with_defaults() {
    let json = defaults_json(&fields(), all);

    assert_eq!(
        Value::Object(json),
        json!({
            "name": null,
            "email": null,
            "note": null,
            "credit_limit": null,
            "points": 0,
            "active": true,
            "started_on": null,
            "happened_at": null,
        }),
        "excludes primary key, managed and hidden columns"
    );

    let json = defaults_json(&fields(), |f| f.name != "email");
    assert!(!json.contains_key("email"), "excludes unreadable columns");
}

// ---- Schema ----

mod defined_items {
    use cakedome::registry::RepoConfig;
    use cakedome::repo::Repo;
    use sea_orm::entity::prelude::*;
    use serde_json::json;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "defined_items")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(20))")]
        pub name: String,
        pub flag: bool,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    pub struct DefinedItemsRepo;

    impl Repo for DefinedItemsRepo {
        type Entity = Entity;

        fn initialize(e: RepoConfig<'_, Entity>) -> RepoConfig<'_, Entity> {
            e.label("定義した品目")
                .labels([(Column::Id, "ID"), (Column::Name, "名前")])
                .default(Column::Flag, json!(true))
        }
    }

    impl cakedome::policy::TablePolicy for DefinedItemsRepo {}

    impl cakedome::policy::EntityPolicy for DefinedItemsRepo {}
}

#[test]
fn add_returns_the_config_for_app_settings() {
    let mut r = common::registry();
    r.add(defined_items::DefinedItemsRepo).label("上書き");

    assert_eq!(r.label_of::<defined_items::Entity>(), Some("上書き"));
}

#[test]
fn merge_appends_errors_of_the_same_key() {
    let mut errors = FieldErrors::default();
    errors.add("name", "required", "入力してください");
    let mut other = FieldErrors::default();
    other.add("name", "too_long", "長すぎます");
    other.add("code", "required", "入力してください");

    errors.merge(other);

    assert_eq!(
        codes(&errors),
        [
            ("code".to_string(), vec!["required"]),
            ("name".to_string(), vec!["required", "too_long"]),
        ]
    );
}

#[test]
fn merge_prefixed_puts_the_prefix_before_each_key() {
    let mut errors = FieldErrors::default();
    errors.add("items.0.quantity", "required", "入力してください");
    let mut row = FieldErrors::default();
    row.add("quantity", "min", "小さすぎます");
    row.add("product_id", "invalid_ref", "選択できない値です");

    errors.merge_prefixed("items.0", row);

    assert_eq!(
        codes(&errors),
        [
            ("items.0.product_id".to_string(), vec!["invalid_ref"]),
            ("items.0.quantity".to_string(), vec!["required", "min"]),
        ]
    );
}

// ---- decimal ----

fn amount_fields() -> Vec<FieldMeta> {
    let mut r = common::registry();
    r.add(DefaultRepo::<amounts::Entity>::new());
    r.fields_of::<amounts::Entity>()
        .expect("registered")
        .to_vec()
}

fn amount_edit(input: Value) -> Result<amounts::ActiveModel, FieldErrors> {
    from_json::<amounts::Entity>(&amount_fields(), &obj(input), WriteMode::Edit, all)
}

fn amount_err(input: Value) -> Vec<(String, Vec<&'static str>)> {
    codes(&amount_edit(input).expect_err("validation fails"))
}

#[test]
fn decimal_column_has_its_precision_and_scale() {
    let fields = amount_fields();

    assert_eq!(
        field(&fields, "price").kind,
        FieldKind::Decimal {
            precision: 10,
            scale: 2
        }
    );
    assert_eq!(field(&fields, "price").kind.as_str(), "decimal");
}

#[test]
#[should_panic(expected = "unsized_decimals.amount")]
fn decimal_without_precision_and_scale_panics_at_registration() {
    let mut r = common::registry();
    r.add(DefaultRepo::<unsized_decimals::Entity>::new());
}

#[test]
fn decimal_is_written_as_a_string_padded_to_its_scale() {
    let row = amounts::Model {
        id: 1,
        price: Decimal::new(12345, 1),
        count: Some(Decimal::new(42, 0)),
    };

    let json = to_json::<amounts::Entity>(&amount_fields(), &row, all);

    assert_eq!(
        Value::Object(json),
        json!({"id": 1, "price": "1234.50", "count": "42"})
    );
}

#[test]
fn decimal_reads_strings_and_numbers() {
    let am = amount_edit(json!({"price": "1234.5", "count": 12})).unwrap();
    assert_eq!(am.price, Set(Decimal::new(12345, 1)));
    assert_eq!(am.count, Set(Some(Decimal::new(12, 0))));

    let am = amount_edit(json!({"price": -0.25, "count": null})).unwrap();
    assert_eq!(am.price, Set(Decimal::new(-25, 2)));
    assert_eq!(am.count, Set(None));

    let am = amount_edit(json!({"price": "99999999.990"})).unwrap();
    assert_eq!(
        am.price,
        Set(Decimal::new(9_999_999_999, 2)),
        "trailing zeros do not count as digits"
    );
}

#[test]
fn decimal_rejects_other_forms_and_too_many_digits() {
    for input in ["12a", "1e3", " 1", "+1", "1.", ".5", ""] {
        assert_eq!(
            amount_err(json!({"price": input})),
            one("price", "type"),
            "{input:?}"
        );
    }
    assert_eq!(amount_err(json!({"price": true})), one("price", "type"));
    assert_eq!(
        amount_err(json!({"price": "1.234"})),
        one("price", "decimal_places")
    );
    assert_eq!(
        amount_err(json!({"price": "123456789"})),
        one("price", "max_digits")
    );
    assert_eq!(
        amount_err(json!({"count": "1.5"})),
        one("count", "decimal_places")
    );
}

// ---- double ----

fn measure_fields() -> Vec<FieldMeta> {
    let mut r = common::registry();
    r.add(DefaultRepo::<measures::Entity>::new());
    r.fields_of::<measures::Entity>()
        .expect("registered")
        .to_vec()
}

fn measure_edit(input: Value) -> Result<measures::ActiveModel, FieldErrors> {
    from_json::<measures::Entity>(&measure_fields(), &obj(input), WriteMode::Edit, all)
}

#[test]
fn double_column_is_read_as_double() {
    let fields = measure_fields();

    assert_eq!(field(&fields, "ratio").kind, FieldKind::Double);
    assert_eq!(FieldKind::Double.as_str(), "double");
}

#[test]
fn double_is_written_as_a_json_number() {
    let row = measures::Model {
        id: 1,
        ratio: 1.5,
        weight: None,
    };

    let json = to_json::<measures::Entity>(&measure_fields(), &row, all);

    assert_eq!(
        Value::Object(json),
        json!({"id": 1, "ratio": 1.5, "weight": null})
    );
}

#[test]
fn double_reads_json_numbers_only() {
    let am = measure_edit(json!({"ratio": -0.25, "weight": 3})).unwrap();
    assert_eq!(am.ratio, Set(-0.25));
    assert_eq!(am.weight, Set(Some(3.0)));

    let am = measure_edit(json!({"ratio": 1.0e3, "weight": null})).unwrap();
    assert_eq!(am.ratio, Set(1000.0));
    assert_eq!(am.weight, Set(None));

    for input in [json!("1.5"), json!(""), json!(true)] {
        let errors = measure_edit(json!({"ratio": input})).expect_err("validation fails");
        assert_eq!(codes(&errors), one("ratio", "type"), "{input}");
    }
}

// ---- json ----

fn config_fields() -> Vec<FieldMeta> {
    let mut r = common::registry();
    r.add(DefaultRepo::<configs::Entity>::new());
    r.fields_of::<configs::Entity>()
        .expect("registered")
        .to_vec()
}

fn config_edit(input: Value) -> Result<configs::ActiveModel, FieldErrors> {
    from_json::<configs::Entity>(&config_fields(), &obj(input), WriteMode::Edit, all)
}

#[test]
fn json_column_is_read_as_json() {
    let fields = config_fields();

    assert_eq!(field(&fields, "data").kind, FieldKind::Json);
    assert_eq!(FieldKind::Json.as_str(), "json");
}

#[test]
fn json_is_written_as_it_is() {
    let row = configs::Model {
        id: 1,
        data: json!({"tags": ["a", "b"], "limit": 3}),
        extra: None,
    };

    let json = to_json::<configs::Entity>(&config_fields(), &row, all);

    assert_eq!(
        Value::Object(json),
        json!({"id": 1, "data": {"tags": ["a", "b"], "limit": 3}, "extra": null})
    );
}

#[test]
fn json_reads_objects_and_arrays_only() {
    let am = config_edit(json!({"data": {"a": [1, {"b": null}]}, "extra": [1, "x"]})).unwrap();
    assert_eq!(am.data, Set(json!({"a": [1, {"b": null}]})));
    assert_eq!(am.extra, Set(Some(json!([1, "x"]))));

    let am = config_edit(json!({"extra": null})).unwrap();
    assert_eq!(am.extra, Set(None));

    for input in [json!("{}"), json!(""), json!(1), json!(true)] {
        let errors = config_edit(json!({"data": input})).expect_err("validation fails");
        assert_eq!(codes(&errors), one("data", "type"), "{input}");
    }
    let errors = config_edit(json!({"data": null})).expect_err("NOT NULL");
    assert_eq!(codes(&errors), one("data", "required"));
}

#[test]
fn messages_render_the_template_with_its_values_and_fall_back_to_the_default() {
    let m = Messages::new(&[("max_length", "{max} chars max")]);
    let max = [("max", "5".to_string())];

    assert_eq!(
        m.render("max_length", "{max} 文字以内", &max),
        "5 chars max"
    );
    assert_eq!(
        m.render("required", "入力してください", &[]),
        "入力してください",
        "keys missing from the table use the default message"
    );
    assert_eq!(
        Messages::default().render("max_length", "{max} 文字以内", &max),
        "5 文字以内"
    );
}

#[test]
fn field_errors_carry_the_values_used_in_their_messages() {
    let errors = edit(json!({"name": "12345678901", "note": "あ".repeat(21_846)})).unwrap_err();
    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "name": [{"code": "max_length", "params": {"max": 10, "unit": "chars"},
                      "message": "Must be at most 10 characters"}],
            "note": [{"code": "max_length", "params": {"max": 65_535, "unit": "bytes"},
                      "message": "Must be at most 65535 bytes"}],
        })
    );

    let errors = amount_edit(json!({"price": "1.234", "count": "123456"})).unwrap_err();
    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "price": [{"code": "decimal_places", "params": {"scale": 2},
                       "message": "Must have at most 2 decimal places"}],
            "count": [{"code": "max_digits", "params": {"digits": 5},
                       "message": "Must have at most 5 digits before the decimal point"}],
        })
    );
}

#[test]
fn add_with_params_puts_the_values_beside_the_message() {
    let mut errors = FieldErrors::default();
    errors.add("quantity", "min", "1 以上で入力してください");
    errors.add_with_params(
        "price",
        "max",
        "100 以下で入力してください",
        json!({"max": 100}).as_object().unwrap().clone(),
    );

    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "quantity": [{"code": "min", "message": "1 以上で入力してください"}],
            "price": [{"code": "max", "params": {"max": 100}, "message": "100 以下で入力してください"}],
        }),
        "errors without params keep the old shape"
    );
}
