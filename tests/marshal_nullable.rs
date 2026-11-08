//! JSON conversion for nullable and non-null column kinds that `marshal.rs` does not cover. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::marshal::{FieldErrors, WriteMode, from_json};
use cakedome::meta::{FieldKind, FieldMeta};
use cakedome::registry::Registry;
use chrono::{NaiveDate, TimeZone, Utc};
use sea_orm::ActiveValue::Set;
use serde_json::{Map, Value, json};
use uuid::Uuid;

mod value_rows {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "value_rows")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub count: Option<i64>,
        pub ref_id: Option<Uuid>,
        pub flag: Option<bool>,
        pub on_day: Date,
        pub at: DateTimeUtc,
        pub seen_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn fields() -> Vec<FieldMeta> {
    let mut r: Registry = common::registry();
    r.add(DefaultRepo::<value_rows::Entity>::new());
    r.fields_of::<value_rows::Entity>()
        .expect("registered")
        .to_vec()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn convert(
    fields: &[FieldMeta],
    mode: WriteMode,
    input: Value,
) -> Result<value_rows::ActiveModel, FieldErrors> {
    from_json::<value_rows::Entity>(fields, &obj(input), mode, |_| true)
}

#[test]
fn nullable_and_non_null_kinds_are_converted_and_cleared() {
    let id = Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();

    let am = convert(
        &fields(),
        WriteMode::Add,
        json!({"count": 5, "ref_id": id.to_string(), "flag": true,
               "on_day": "2026-10-05", "at": "2026-10-05T01:02:03Z",
               "seen_at": "2026-10-06T01:02:03Z"}),
    )
    .unwrap();
    assert_eq!(am.count, Set(Some(5)));
    assert_eq!(am.ref_id, Set(Some(id)));
    assert_eq!(am.flag, Set(Some(true)));
    assert_eq!(
        am.on_day,
        Set(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap())
    );
    assert_eq!(
        am.at,
        Set(Utc.with_ymd_and_hms(2026, 10, 5, 1, 2, 3).unwrap())
    );

    let cleared = convert(
        &fields(),
        WriteMode::Edit,
        json!({"count": null, "ref_id": null, "flag": null, "seen_at": null}),
    )
    .unwrap();
    assert_eq!(cleared.count, Set(None));
    assert_eq!(cleared.ref_id, Set(None));
    assert_eq!(cleared.flag, Set(None));
    assert_eq!(cleared.seen_at, Set(None));
}

#[test]
fn a_date_must_be_written_in_the_canonical_form() {
    let errors = convert(
        &fields(),
        WriteMode::Edit,
        json!({"on_day": "2026-1-5", "at": "2026-10-05"}),
    )
    .expect_err("rejected");

    let codes: Vec<(&str, &str)> = errors
        .0
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |e| (k.as_str(), e.code)))
        .collect();
    assert_eq!(codes, [("at", "type"), ("on_day", "type")]);
}

#[test]
fn a_field_that_does_not_match_its_column_is_a_type_mismatch() {
    let mut fields = fields();
    for f in &mut fields {
        if f.name == "count" {
            f.kind = FieldKind::Integer;
        }
    }

    let errors = convert(&fields, WriteMode::Edit, json!({"count": 5})).expect_err("mismatch");

    let codes: Vec<(&str, &str)> = errors
        .0
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |e| (k.as_str(), e.code)))
        .collect();
    assert_eq!(codes, [("", "type_mismatch")]);
}
