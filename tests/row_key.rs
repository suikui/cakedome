//! Primary key types: `RowKey` for `i64` and the type-erased `RowId`. No DB.

use std::collections::HashSet;

use cakedome::key::{Key, KeyKind, RowId, RowKey};
use cakedome::repo::DefaultRepo;
use serde_json::json;

mod widgets {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "widgets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[test]
fn i64_kind_is_big_integer() {
    assert_eq!(<i64 as RowKey>::KIND, KeyKind::BigInteger);
}

#[test]
fn repo_key_type_is_the_entity_primary_key_type() {
    let id: Key<DefaultRepo<widgets::Entity>> = 5_i64;
    assert_eq!(id, 5);
}

#[test]
fn i64_serializes_to_a_json_number() {
    assert_eq!(12_i64.to_json(), json!(12));
    assert_eq!((-3_i64).to_json(), json!(-3));
}

#[test]
fn i64_reads_only_json_integers() {
    assert_eq!(i64::from_json(&json!(12)), Some(12));
    assert_eq!(i64::from_json(&json!(i64::MAX)), Some(i64::MAX));
    // Strings, floats, bools, null and out-of-range values are rejected.
    assert_eq!(i64::from_json(&json!("12")), None);
    assert_eq!(i64::from_json(&json!(1.5)), None);
    assert_eq!(i64::from_json(&json!(true)), None);
    assert_eq!(i64::from_json(&json!(null)), None);
    assert_eq!(i64::from_json(&json!(u64::MAX)), None);
}

#[test]
fn i64_parses_url_strings() {
    assert_eq!(i64::parse("12"), Some(12));
    assert_eq!(i64::parse("-1"), Some(-1));
    assert_eq!(i64::parse("+7"), Some(7));
    assert_eq!(i64::parse(""), None);
    assert_eq!(i64::parse("abc"), None);
    assert_eq!(i64::parse("1.5"), None);
    assert_eq!(i64::parse(" 1"), None);
    assert_eq!(i64::parse("99999999999999999999"), None);
}

#[test]
fn i64_does_not_generate_values() {
    assert_eq!(<i64 as RowKey>::generate(), None);
}

#[test]
fn i64_round_trips_through_row_id() {
    assert_eq!(12_i64.into_row_id(), RowId::Int(12));
    assert_eq!(i64::from_row_id(&RowId::Int(5)), Some(5));
    assert_eq!(i64::from_row_id(&RowId::Str("5".to_string())), None);
    assert_eq!(
        i64::from_row_id(&RowId::Uuid(uuid::Uuid::from_u128(5))),
        None
    );
}

#[test]
fn row_id_is_built_from_i64() {
    assert_eq!(RowId::from(3_i64), RowId::Int(3));
}

#[test]
fn row_id_to_string() {
    // Used in change history and logs: UUIDs are lowercase and hyphenated.
    assert_eq!(RowId::Int(12).to_string(), "12");
    assert_eq!(RowId::Str("JP".to_string()).to_string(), "JP");
    let uuid = uuid::Uuid::parse_str("0192F5C4-7A1B-7C3D-8E4F-0123456789AB").unwrap();
    assert_eq!(
        RowId::Uuid(uuid).to_string(),
        "0192f5c4-7a1b-7c3d-8e4f-0123456789ab"
    );
}

#[test]
fn row_id_to_json() {
    assert_eq!(RowId::Int(12).to_json(), json!(12));
    assert_eq!(RowId::Str("JP".to_string()).to_json(), json!("JP"));
    let uuid = uuid::Uuid::from_u128(1);
    assert_eq!(
        RowId::Uuid(uuid).to_json(),
        json!("00000000-0000-0000-0000-000000000001")
    );
}

#[test]
fn row_id_works_as_a_set_key() {
    let ids: HashSet<RowId> = [RowId::Int(1), RowId::Int(1), RowId::Str("1".to_string())]
        .into_iter()
        .collect();
    assert_eq!(ids.len(), 2);
}
