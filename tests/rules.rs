//! Table rules: cakedome acts only on the `TableRules` result (`TableSpec`), not on column names.
//! Verified with rules using other column names (`inserted_at`, `alive`, `position`...). No DB.

use cakedome::DefaultRepo;
use cakedome::meta::{FieldMeta, default_order, fields_of};
use cakedome::meta::{Lifecycle, TableRules, TableSpec, spec_for};
use cakedome::registry::Registry;
use sea_orm::{IdenStatic, Order};
use serde_json::json;

mod widgets {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "widgets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub position: i32,
        pub alive: bool,
        pub inserted_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Table violating the rules (`broken_` prefix without a `title` column).
mod broken_things {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "broken_things")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Rules with other column names: `alive` is the soft-delete flag, `position` the sort order.
struct OddRules;

impl TableRules for OddRules {
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        let has = |name: &str| columns.iter().find(|c| **c == name).copied();
        if table.starts_with("broken_") && has("title").is_none() {
            return Err("title column required".to_string());
        }
        let active_column = has("alive");
        Ok(TableSpec {
            lifecycle: if active_column.is_some() {
                Lifecycle::SoftDelete
            } else {
                Lifecycle::HardDelete
            },
            active_column,
            created_column: has("inserted_at"),
            modified_column: has("updated_at"),
            order_column: has("position"),
            defaults: has("position")
                .map(|c| vec![(c, json!(1))])
                .unwrap_or_default(),
            uniques: vec![],
            warn_duplicates: vec![],
        })
    }
}

fn spec() -> TableSpec {
    spec_for::<widgets::Entity>(&OddRules).unwrap()
}

fn field<'a>(fields: &'a [FieldMeta], name: &str) -> &'a FieldMeta {
    fields.iter().find(|f| f.name == name).unwrap()
}

#[test]
fn spec_for_passes_table_name_and_columns() {
    assert_eq!(
        spec(),
        TableSpec {
            lifecycle: Lifecycle::SoftDelete,
            active_column: Some("alive"),
            created_column: Some("inserted_at"),
            modified_column: Some("updated_at"),
            order_column: Some("position"),
            defaults: vec![("position", json!(1))],
            uniques: vec![],
            warn_duplicates: vec![],
        }
    );
    assert_eq!(
        spec_for::<broken_things::Entity>(&OddRules),
        Err("title column required".to_string())
    );
}

#[test]
fn fields_follow_the_spec() {
    let fs = fields_of::<widgets::Entity>(&spec());

    for name in ["inserted_at", "updated_at"] {
        let f = field(&fs, name);
        assert!(f.managed && !f.internal, "{name}");
    }
    let alive = field(&fs, "alive");
    assert!(alive.managed && alive.internal);
    assert_eq!(field(&fs, "position").default, Some(json!(1)));
    let title = field(&fs, "title");
    assert!(!title.managed && !title.internal && title.default.is_none());
}

#[test]
fn plain_spec_has_no_managed_fields() {
    let fs = fields_of::<widgets::Entity>(&TableSpec::plain());

    assert!(
        fs.iter()
            .all(|f| !f.managed && !f.internal && f.default.is_none())
    );
    assert_eq!(TableSpec::plain().lifecycle, Lifecycle::HardDelete);
}

#[test]
fn default_order_uses_the_order_column() {
    let order: Vec<(&str, bool)> = default_order::<widgets::Entity>(&spec())
        .into_iter()
        .map(|(c, o)| (c.as_str(), matches!(o, Order::Asc)))
        .collect();
    assert_eq!(order, [("position", true), ("id", true)]);

    let plain: Vec<&str> = default_order::<widgets::Entity>(&TableSpec::plain())
        .into_iter()
        .map(|(c, _)| c.as_str())
        .collect();
    assert_eq!(plain, ["id"]);
}

#[test]
fn registry_keeps_the_spec_and_computes_it_for_unregistered_entities() {
    let mut r = Registry::new(OddRules, &[], &[]);
    assert_eq!(
        r.spec_of::<widgets::Entity>(),
        spec(),
        "built from rules even if unregistered"
    );

    r.add(DefaultRepo::<widgets::Entity>::new());
    assert_eq!(r.spec_of::<widgets::Entity>(), spec());
    let fs = r.fields_of::<widgets::Entity>().unwrap();
    assert!(field(fs, "alive").internal);
}

#[test]
fn define_can_override_the_spec() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .table_spec(|s| TableSpec {
            lifecycle: Lifecycle::HardDelete,
            active_column: None,
            ..s
        });

    assert_eq!(
        r.spec_of::<widgets::Entity>().lifecycle,
        Lifecycle::HardDelete
    );
    let fs = r.fields_of::<widgets::Entity>().unwrap();
    let alive = field(fs, "alive");
    assert!(!alive.managed && !alive.internal, "becomes a normal field");
    assert!(field(fs, "inserted_at").managed, "other rules remain");
}

#[test]
fn override_keeps_explicit_settings() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .labels([(widgets::Column::Title, "題")])
        .default(widgets::Column::Position, json!(5))
        .table_spec(|s| s);

    let fs = r.fields_of::<widgets::Entity>().unwrap();
    assert_eq!(field(fs, "title").label, "題");
    assert_eq!(
        field(fs, "position").default,
        Some(json!(5)),
        "explicit default is not overridden by rules"
    );
}

#[test]
#[should_panic(expected = "widgets")]
fn soft_delete_without_active_column_panics() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .table_spec(|s| TableSpec {
            active_column: None,
            ..s
        });
}

#[test]
#[should_panic(expected = "broken_things: title column required")]
fn rules_error_panics_on_registration() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<broken_things::Entity>::new());
}

/// Same spec as `OddRules`; display field is `title` when present.
struct TitleRules;

impl TableRules for TitleRules {
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        OddRules.spec(table, columns)
    }

    fn display_field(&self, _table: &str, columns: &[&'static str]) -> Option<&'static str> {
        columns.iter().find(|c| **c == "title").copied()
    }
}

/// Rules whose display field is a missing column (registration panics).
struct MissingColumnRules;

impl TableRules for MissingColumnRules {
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        OddRules.spec(table, columns)
    }

    fn display_field(&self, _table: &str, _columns: &[&'static str]) -> Option<&'static str> {
        Some("nope")
    }
}

#[test]
fn display_field_defaults_to_the_primary_key() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new());

    assert_eq!(
        r.display_field_of::<widgets::Entity>(),
        Some("id"),
        "primary key when the rules decide nothing (default None), even with a title column"
    );
}

#[test]
fn overriding_the_spec_does_not_change_the_display_field() {
    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .display_field(widgets::Column::Title)
        .table_spec(|s| TableSpec {
            lifecycle: Lifecycle::HardDelete,
            active_column: None,
            ..s
        });
    assert_eq!(r.display_field_of::<widgets::Entity>(), Some("title"));

    let mut r = Registry::new(OddRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .table_spec(|s| s);
    assert_eq!(r.display_field_of::<widgets::Entity>(), Some("id"));
}

#[test]
fn rules_choose_the_display_field() {
    let mut r = Registry::new(TitleRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new());
    assert_eq!(r.display_field_of::<widgets::Entity>(), Some("title"));
}

#[test]
fn initialize_overrides_the_display_field_of_the_rules() {
    let mut r = Registry::new(TitleRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .display_field(widgets::Column::Id);
    assert_eq!(r.display_field_of::<widgets::Entity>(), Some("id"));
}

#[test]
fn overriding_the_spec_keeps_the_display_field_of_the_rules() {
    let mut r = Registry::new(TitleRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new())
        .table_spec(|s| TableSpec {
            lifecycle: Lifecycle::HardDelete,
            active_column: None,
            ..s
        });
    assert_eq!(r.display_field_of::<widgets::Entity>(), Some("title"));
}

#[test]
#[should_panic(expected = "widgets: display field column nope does not exist")]
fn display_field_of_the_rules_must_be_a_column() {
    let mut r = Registry::new(MissingColumnRules, &[], &[]);
    r.add(DefaultRepo::<widgets::Entity>::new());
}
