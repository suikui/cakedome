//! `RefBy` yields null at runtime on a type mismatch; `Registry::verify` must catch it at startup.

mod common;

use cakedome::DefaultRepo;
use cakedome::defaults::RefBy;
use cakedome::registry::Registry;
use serde_json::json;

mod colors {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "colors")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(20))")]
        pub code: String,
        pub rank: i32,
        pub is_active: bool,
        pub released_on: Date,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod items {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "items")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub color_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn registry(default: RefBy) -> Registry {
    let mut r = common::registry();
    r.add(DefaultRepo::<colors::Entity>::new());
    r.add(DefaultRepo::<items::Entity>::new())
        .default_from(items::Column::ColorId, default);
    r
}

#[test]
fn verify_accepts_values_matching_the_column_type() {
    for default in [
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::Code, "red"),
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::Id, 1),
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::Rank, 2),
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::IsActive, true),
    ] {
        registry(default).verify();
    }
}

#[test]
#[should_panic(
    expected = "items: color_id default RefBy value 1 does not match the type of colors.code"
)]
fn verify_panics_when_a_number_is_given_for_a_string_column() {
    registry(RefBy::new::<DefaultRepo<colors::Entity>>(
        colors::Column::Code,
        1,
    ))
    .verify();
}

#[test]
#[should_panic(expected = "RefBy value \"red\" does not match the type of colors.id")]
fn verify_panics_when_a_string_is_given_for_an_integer_column() {
    registry(RefBy::new::<DefaultRepo<colors::Entity>>(
        colors::Column::Id,
        "red",
    ))
    .verify();
}

#[test]
#[should_panic(expected = "does not match the type of colors.rank")]
fn verify_panics_when_the_number_is_out_of_the_i32_range() {
    registry(RefBy::new::<DefaultRepo<colors::Entity>>(
        colors::Column::Rank,
        json!(1_i64 << 40),
    ))
    .verify();
}

#[test]
#[should_panic(expected = "does not match the type of colors.released_on")]
fn verify_panics_for_a_date_column() {
    // Date and datetime columns cannot be looked up (null at runtime), so reject them at startup.
    registry(RefBy::new::<DefaultRepo<colors::Entity>>(
        colors::Column::ReleasedOn,
        "2026-01-01",
    ))
    .verify();
}

struct ColorsRepo;

impl cakedome::repo::Repo for ColorsRepo {
    type Entity = colors::Entity;
}

impl cakedome::policy::TablePolicy for ColorsRepo {}

impl cakedome::policy::EntityPolicy for ColorsRepo {}

#[test]
#[should_panic(
    expected = "items: color_id default RefBy target colors is registered with a Repo of a different type"
)]
fn verify_rejects_ref_by_whose_repo_type_is_not_the_registered_one() {
    let mut r = common::registry();
    r.add(ColorsRepo);
    r.add(DefaultRepo::<items::Entity>::new()).default_from(
        items::Column::ColorId,
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::Code, "red"),
    );
    r.verify();
}

#[test]
fn verify_skips_ref_by_whose_target_is_not_registered() {
    // With no registered target there is nothing to check against; stays null at runtime.
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new()).default_from(
        items::Column::ColorId,
        RefBy::new::<DefaultRepo<colors::Entity>>(colors::Column::Code, 1),
    );
    r.verify();
}
