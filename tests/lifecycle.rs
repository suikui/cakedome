//! `is_active` semantics per table class (`TableRules` output), reflected in fields, JSON, Schema.

mod common;

use cakedome::meta::FieldMeta;
use cakedome::meta::Lifecycle;
use serde_json::json;

mod master_colors {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "master_colors")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(20))")]
        pub code: String,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        pub is_active: bool,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// `users`: not a master, but `is_active` means "usable".
mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(64))")]
        pub login_id: String,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod notes {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "notes")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub is_active: bool,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod setting_links {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_links")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub note_id: i64,
        pub seq: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod master_broken {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "master_broken")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod master_no_flag {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "master_no_flag")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(20))")]
        pub code: String,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn field<'a>(fields: &'a [FieldMeta], name: &str) -> &'a FieldMeta {
    fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} exists"))
}

#[test]
fn lifecycle_depends_on_table_name_and_is_active() {
    assert_eq!(
        common::spec::<master_colors::Entity>().lifecycle,
        Lifecycle::Activatable
    );
    assert_eq!(
        common::spec::<users::Entity>().lifecycle,
        Lifecycle::Activatable
    );
    assert_eq!(
        common::spec::<notes::Entity>().lifecycle,
        Lifecycle::SoftDelete
    );
    assert_eq!(
        common::spec::<setting_links::Entity>().lifecycle,
        Lifecycle::HardDelete
    );
}

#[test]
fn is_active_is_managed_only_for_soft_delete() {
    let fs = common::fields::<notes::Entity>();
    let f = field(&fs, "is_active");
    assert!(f.managed, "the soft-delete flag is managed by the Repo");
    assert_eq!(f.default, None);

    for fs in [
        common::fields::<master_colors::Entity>(),
        common::fields::<users::Entity>(),
    ] {
        let f = field(&fs, "is_active");
        assert!(!f.managed, "the usable flag is a writable column");
        assert_eq!(f.default, Some(json!(true)), "active when not sent on add");
    }

    let fs = common::fields::<master_colors::Entity>();
    assert!(field(&fs, "created").managed);
    assert!(field(&fs, "modified").managed);
}

#[test]
#[should_panic(expected = "master_broken")]
fn master_without_required_columns_is_rejected() {
    common::fields::<master_broken::Entity>();
}

#[test]
#[should_panic(expected = "master_no_flag")]
fn master_without_is_active_is_rejected() {
    common::fields::<master_no_flag::Entity>();
}
