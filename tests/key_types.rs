//! UUID and string primary keys. No DB.

mod common;

use std::collections::HashSet;

use cakedome::DefaultRepo;
use cakedome::association::{BelongsToOf, HasManyOf, ManyToManyOf};
use cakedome::key::{Key, KeyKind, RowId, RowKey};
use cakedome::marshal::{WriteMode, from_json, to_json};
use cakedome::meta::{FieldKind, FieldMeta};
use cakedome::registry::Registry;
use cakedome::repo::Repo;
use sea_orm::ActiveValue::{NotSet, Set};
use sea_orm::Value;
use serde_json::{Map, json};
use uuid::Uuid;

mod docs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "docs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: Uuid,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub order_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod countries {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "countries")]
    pub struct Model {
        #[sea_orm(primary_key, column_type = "Char(Some(2))")]
        pub id: String,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
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
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub note: String,
        #[sea_orm(column_type = "Char(Some(2))", nullable)]
        pub country_id: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod setting_order_docs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_order_docs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub order_id: i64,
        pub doc_id: Uuid,
        pub seq: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod bad_refs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "bad_refs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub order_id: i64,
        pub doc_id: Option<i64>,
        pub country_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod auto_uuid {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "auto_uuid")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = true)]
        pub id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

type Docs = DefaultRepo<docs::Entity>;
type Countries = DefaultRepo<countries::Entity>;
type Orders = DefaultRepo<orders::Entity>;
type OrderDocs = DefaultRepo<setting_order_docs::Entity>;
type BadRefs = DefaultRepo<bad_refs::Entity>;

const DOC_ORDER: BelongsToOf<Docs, Orders> = BelongsToOf::new("order", docs::Column::OrderId);
const ORDER_DOCS: HasManyOf<Orders, Docs> = HasManyOf::new("docs", docs::Column::OrderId);
const ORDER_COUNTRY: BelongsToOf<Orders, Countries> =
    BelongsToOf::new("country", orders::Column::CountryId);
const ORDER_LINKED_DOCS: ManyToManyOf<Orders, OrderDocs, Docs> = ManyToManyOf::new(
    "linked_docs",
    setting_order_docs::Column::OrderId,
    setting_order_docs::Column::DocId,
);

fn registry() -> Registry {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(Countries::new());
    r.add(Docs::new()).association(DOC_ORDER);
    r.add(OrderDocs::new());
    r.add(Orders::new())
        .association(ORDER_COUNTRY)
        .association(ORDER_DOCS)
        .association(ORDER_LINKED_DOCS);
    r.verify();
    r
}

fn field<'a>(fields: &'a [FieldMeta], name: &str) -> &'a FieldMeta {
    fields.iter().find(|f| f.name == name).unwrap()
}

fn obj(v: serde_json::Value) -> Map<String, serde_json::Value> {
    v.as_object().unwrap().clone()
}

fn all(_: &FieldMeta) -> bool {
    true
}

const SAMPLE: &str = "0192f5c4-7a1b-7c3d-8e4f-0123456789ab";

fn sample() -> Uuid {
    Uuid::parse_str(SAMPLE).unwrap()
}

// ---- RowKey: Uuid ----

#[test]
fn uuid_kind_and_json() {
    assert_eq!(<Uuid as RowKey>::KIND, KeyKind::Uuid);
    assert_eq!(sample().to_json(), json!(SAMPLE));
}

#[test]
fn uuid_reads_hyphenated_or_32_char_strings() {
    assert_eq!(Uuid::from_json(&json!(SAMPLE)), Some(sample()));
    assert_eq!(
        Uuid::from_json(&json!("0192F5C4-7A1B-7C3D-8E4F-0123456789AB")),
        Some(sample()),
        "uppercase is accepted"
    );
    assert_eq!(
        Uuid::from_json(&json!("0192f5c47a1b7c3d8e4f0123456789ab")),
        Some(sample()),
        "32 chars without hyphens are accepted"
    );
    assert_eq!(Uuid::parse(SAMPLE), Some(sample()));
    assert_eq!(
        Uuid::parse("0192f5c47a1b7c3d8e4f0123456789ab"),
        Some(sample())
    );
}

#[test]
fn uuid_rejects_malformed_values() {
    for bad in [
        "",
        "12",
        "0192f5c4-7a1b-7c3d-8e4f-0123456789a", // one char short
        "0192f5c4-7a1b-7c3d-8e4f-0123456789abc", // one char too many
        "{0192f5c4-7a1b-7c3d-8e4f-0123456789ab}", // braces
        "urn:uuid:0192f5c4-7a1b-7c3d-8e4f-0123456789ab",
        "0192f5c4-7a1b-7c3d-8e4f-0123456789ag", // not hex
    ] {
        assert_eq!(Uuid::parse(bad), None, "{bad}");
        assert_eq!(Uuid::from_json(&json!(bad)), None, "{bad}");
    }
    assert_eq!(Uuid::from_json(&json!(12)), None, "numbers are rejected");
    assert_eq!(Uuid::from_json(&json!(null)), None);
}

#[test]
fn uuid_generates_v7() {
    let a = <Uuid as RowKey>::generate().unwrap();
    let b = <Uuid as RowKey>::generate().unwrap();
    assert_eq!(a.get_version_num(), 7);
    assert_ne!(a, b);
}

#[test]
fn uuid_round_trips_through_row_id() {
    assert_eq!(sample().into_row_id(), RowId::Uuid(sample()));
    assert_eq!(Uuid::from_row_id(&RowId::Uuid(sample())), Some(sample()));
    assert_eq!(Uuid::from_row_id(&RowId::Int(1)), None);
    assert_eq!(Uuid::from_row_id(&RowId::Str(SAMPLE.to_string())), None);
}

// ---- RowKey: String ----

#[test]
fn string_kind_and_json() {
    assert_eq!(<String as RowKey>::KIND, KeyKind::String);
    assert_eq!("JP".to_string().to_json(), json!("JP"));
}

#[test]
fn string_reads_only_non_empty_strings() {
    assert_eq!(String::from_json(&json!("JP")), Some("JP".to_string()));
    assert_eq!(String::from_json(&json!("")), None);
    assert_eq!(String::from_json(&json!(12)), None, "numbers are rejected");
    assert_eq!(String::from_json(&json!(null)), None);
    assert_eq!(String::parse("JP"), Some("JP".to_string()));
    assert_eq!(
        String::parse("001"),
        Some("001".to_string()),
        "digits-only stays a string"
    );
    assert_eq!(String::parse(""), None);
}

#[test]
fn string_does_not_generate_values() {
    assert_eq!(<String as RowKey>::generate(), None);
}

#[test]
fn string_round_trips_through_row_id() {
    assert_eq!("JP".to_string().into_row_id(), RowId::Str("JP".to_string()));
    assert_eq!(
        String::from_row_id(&RowId::Str("JP".to_string())),
        Some("JP".to_string())
    );
    assert_eq!(String::from_row_id(&RowId::Int(1)), None);
}

#[test]
fn row_id_sql_value_follows_its_type() {
    // UUID is compared against a BINARY(16) column, so bind a UUID value, not a string.
    assert_eq!(Value::from(&RowId::Uuid(sample())), Value::from(sample()));
    assert_eq!(Value::from(&RowId::Int(3)), Value::from(3_i64));
    assert_eq!(
        Value::from(&RowId::Str("JP".to_string())),
        Value::from("JP".to_string())
    );
}

#[test]
fn repo_primary_key_types() {
    let _: Key<Docs> = sample();
    let _: Key<Countries> = "JP".to_string();
    let _: Key<Orders> = 1_i64;
}

#[test]
fn new_key_defaults_to_the_type_default() {
    let id = Docs::new().new_key().unwrap();
    assert_eq!(id.get_version_num(), 7);
    assert_eq!(Countries::new().new_key(), None);
    assert_eq!(Orders::new().new_key(), None);
}

/// Overrides `new_key` to build string keys itself (ULID, Snowflake…; fixed values here).
struct NumberedCountries;

impl Repo for NumberedCountries {
    type Entity = countries::Entity;

    fn new_key(&self) -> Option<Key<Self>> {
        Some("ZZ".to_string())
    }
}

impl cakedome::policy::TablePolicy for NumberedCountries {}

impl cakedome::policy::EntityPolicy for NumberedCountries {}

#[test]
fn new_key_can_be_overridden() {
    assert_eq!(NumberedCountries.new_key(), Some("ZZ".to_string()));
}

#[test]
fn uuid_column_is_a_uuid_field() {
    let r = registry();
    let docs = r.meta_of::<docs::Entity>().unwrap();
    assert_eq!(field(&docs.fields, "id").kind, FieldKind::Uuid);
    assert_eq!(FieldKind::Uuid.as_str(), "uuid");
}

#[test]
fn char_column_is_a_string_with_max_length() {
    let r = registry();
    let countries = r.meta_of::<countries::Entity>().unwrap();
    assert_eq!(
        field(&countries.fields, "id").kind,
        FieldKind::String { max_length: 2 }
    );
    let orders = r.meta_of::<orders::Entity>().unwrap();
    assert_eq!(
        field(&orders.fields, "country_id").kind,
        FieldKind::String { max_length: 2 }
    );
}

#[test]
fn entity_primary_key_kinds() {
    let r = registry();
    assert_eq!(r.meta_of::<docs::Entity>().unwrap().key, KeyKind::Uuid);
    assert_eq!(
        r.meta_of::<countries::Entity>().unwrap().key,
        KeyKind::String
    );
    assert_eq!(
        r.meta_of::<orders::Entity>().unwrap().key,
        KeyKind::BigInteger
    );
    assert_eq!(KeyKind::BigInteger.as_str(), "big_integer");
    assert_eq!(KeyKind::Uuid.as_str(), "uuid");
    assert_eq!(KeyKind::String.as_str(), "string");
}

#[test]
fn string_primary_key_is_checked_for_uniqueness() {
    // Report duplicate ids as 422 `unique` before the DB's primary-key 409.
    let r = registry();
    assert!(
        r.meta_of::<countries::Entity>()
            .unwrap()
            .spec
            .uniques
            .contains(&vec!["id"])
    );
    assert!(
        !r.meta_of::<docs::Entity>()
            .unwrap()
            .spec
            .uniques
            .contains(&vec!["id"]),
        "UUID is generated, so not checked"
    );
    assert!(
        !r.meta_of::<orders::Entity>()
            .unwrap()
            .spec
            .uniques
            .contains(&vec!["id"])
    );
}

#[test]
fn uuid_column_serializes_lowercase_hyphenated() {
    let r = registry();
    let fields = &r.meta_of::<docs::Entity>().unwrap().fields;
    let model = docs::Model {
        id: sample(),
        title: "見積".to_string(),
        order_id: Some(3),
    };
    let out = to_json::<docs::Entity>(fields, &model, all);
    assert_eq!(out["id"], json!(SAMPLE));
    assert_eq!(out["order_id"], json!(3));
}

#[test]
fn uuid_foreign_key_is_read_from_json() {
    let r = registry();
    let fields = &r.meta_of::<setting_order_docs::Entity>().unwrap().fields;
    let am = from_json::<setting_order_docs::Entity>(
        fields,
        &obj(json!({"order_id": 1, "doc_id": "0192F5C4-7A1B-7C3D-8E4F-0123456789AB", "seq": 0})),
        WriteMode::Add,
        all,
    )
    .unwrap();
    assert_eq!(am.doc_id, Set(sample()));

    let errors = from_json::<setting_order_docs::Entity>(
        fields,
        &obj(json!({"order_id": 1, "doc_id": "abc", "seq": 0})),
        WriteMode::Add,
        all,
    )
    .unwrap_err();
    assert_eq!(errors.0["doc_id"][0].code, "type");

    let errors = from_json::<setting_order_docs::Entity>(
        fields,
        &obj(json!({"order_id": 1, "doc_id": 5, "seq": 0})),
        WriteMode::Add,
        all,
    )
    .unwrap_err();
    assert_eq!(errors.0["doc_id"][0].code, "type", "a number is not a UUID");
}

#[test]
fn uuid_primary_key_cannot_be_sent() {
    let r = registry();
    let fields = &r.meta_of::<docs::Entity>().unwrap().fields;
    let errors = from_json::<docs::Entity>(
        fields,
        &obj(json!({"id": SAMPLE, "title": "見積"})),
        WriteMode::Add,
        all,
    )
    .unwrap_err();
    assert_eq!(errors.0["id"][0].code, "read_only");

    let am = from_json::<docs::Entity>(fields, &obj(json!({"title": "見積"})), WriteMode::Add, all)
        .unwrap();
    assert_eq!(
        am.id, NotSet,
        "NotSet when omitted (new_key fills it on save)"
    );
}

#[test]
fn string_primary_key_can_be_sent_only_on_add() {
    let r = registry();
    let fields = &r.meta_of::<countries::Entity>().unwrap().fields;

    let am = from_json::<countries::Entity>(
        fields,
        &obj(json!({"id": "JP", "name": "日本"})),
        WriteMode::Add,
        all,
    )
    .unwrap();
    assert_eq!(am.id, Set("JP".to_string()));

    let errors = from_json::<countries::Entity>(
        fields,
        &obj(json!({"id": "JPN", "name": "日本"})),
        WriteMode::Add,
        all,
    )
    .unwrap_err();
    assert_eq!(
        errors.0["id"][0].code, "max_length",
        "checks the column length"
    );

    // Omitted is not `required` at the first stage: `new_key` may fill it, else required on save.
    let am =
        from_json::<countries::Entity>(fields, &obj(json!({"name": "日本"})), WriteMode::Add, all)
            .unwrap();
    assert_eq!(am.id, NotSet);

    let errors = from_json::<countries::Entity>(
        fields,
        &obj(json!({"id": "US", "name": "米国"})),
        WriteMode::Edit,
        all,
    )
    .unwrap_err();
    assert_eq!(
        errors.0["id"][0].code, "read_only",
        "the primary key cannot change on update"
    );
}

#[test]
fn integer_primary_key_still_cannot_be_sent() {
    let r = registry();
    let fields = &r.meta_of::<orders::Entity>().unwrap().fields;
    let errors = from_json::<orders::Entity>(
        fields,
        &obj(json!({"id": 5, "note": "x"})),
        WriteMode::Add,
        all,
    )
    .unwrap_err();
    assert_eq!(errors.0["id"][0].code, "read_only");
}

#[test]
fn associations_connect_primary_keys_of_different_types() {
    let r = registry();
    let names: HashSet<&str> = r
        .meta_of::<orders::Entity>()
        .unwrap()
        .relations
        .iter()
        .map(|a| a.name())
        .collect();
    assert_eq!(names, HashSet::from(["country", "docs", "linked_docs"]));
}

#[test]
#[should_panic(expected = "doc_id")]
fn belongs_to_foreign_key_type_mismatch_panics() {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(Docs::new());
    r.add(BadRefs::new())
        .association(BelongsToOf::<BadRefs, Docs>::new(
            "doc",
            bad_refs::Column::DocId,
        ));
    r.verify();
}

#[test]
#[should_panic(expected = "country_id")]
fn integer_foreign_key_to_string_primary_key_panics() {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(Countries::new());
    r.add(BadRefs::new())
        .association(BelongsToOf::<BadRefs, Countries>::new(
            "country",
            bad_refs::Column::CountryId,
        ));
    r.verify();
}

#[test]
#[should_panic(expected = "doc_id")]
fn has_many_foreign_key_type_mismatch_panics() {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(BadRefs::new());
    r.add(Docs::new())
        .association(HasManyOf::<Docs, BadRefs>::new(
            "bad_refs",
            bad_refs::Column::DocId,
        ));
    r.verify();
}

#[test]
#[should_panic(expected = "bad_refs.doc_id")]
fn many_to_many_join_foreign_key_type_mismatch_panics() {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(Docs::new());
    r.add(BadRefs::new());
    r.add(Orders::new())
        .association(ManyToManyOf::<Orders, BadRefs, Docs>::new(
            "bad_docs",
            bad_refs::Column::OrderId,
            bad_refs::Column::DocId,
        ));
    r.verify();
}

#[test]
#[should_panic(expected = "auto_increment")]
fn auto_increment_on_non_integer_key_panics() {
    let mut r = Registry::new(common::TestRules, &[], &[]);
    r.add(DefaultRepo::<auto_uuid::Entity>::new());
}
