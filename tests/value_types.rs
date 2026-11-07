//! Registration catches mismatches between the Rust types cakedome sets and the column types.
//! SeaORM `ActiveModel::set` panics on a mismatch, so fail at startup. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::registry::Registry;

mod offset_created {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "offset_created")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub created: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod offset_input {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "offset_input")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub shipped_at: Option<DateTimeWithTimeZone>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Sort field is `i64` (cakedome sets `i32`).
mod setting_wide_seq {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_wide_seq")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub seq: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Column type is `Integer` (cakedome sets `i32`) but the Rust type is `i64`.
mod narrow_column {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "narrow_column")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "Integer")]
        pub quantity: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod all_kinds {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "all_kinds")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub note: Option<String>,
        pub quantity: i32,
        pub total: Option<i64>,
        pub token: Uuid,
        pub flag: bool,
        pub on_day: Option<Date>,
        pub at: Option<DateTimeUtc>,
        pub is_active: bool,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn registry() -> Registry {
    Registry::new(common::TestRules, &[], &[])
}

#[test]
#[should_panic(expected = "offset_created.created")]
fn fixed_offset_created_column_panics_on_registration() {
    registry().add(DefaultRepo::<offset_created::Entity>::new());
}

#[test]
#[should_panic(expected = "offset_input.shipped_at")]
fn fixed_offset_input_datetime_panics_on_registration() {
    registry().add(DefaultRepo::<offset_input::Entity>::new());
}

#[test]
#[should_panic(expected = "setting_wide_seq.seq")]
fn i64_order_column_panics_on_registration() {
    registry().add(DefaultRepo::<setting_wide_seq::Entity>::new());
}

#[test]
#[should_panic(expected = "narrow_column.quantity")]
fn column_and_rust_type_mismatch_panics_on_registration() {
    registry().add(DefaultRepo::<narrow_column::Entity>::new());
}

#[test]
fn entity_with_matching_types_registers() {
    let mut r = registry();
    r.add(DefaultRepo::<all_kinds::Entity>::new());
    r.verify();
}

#[test]
fn panic_message_names_the_rust_type_to_use() {
    let message = std::panic::catch_unwind(|| {
        registry().add(DefaultRepo::<offset_created::Entity>::new());
    })
    .expect_err("registration panics");
    let text = message
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| message.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(text.contains("DateTime<Utc>"), "{text}");
}
