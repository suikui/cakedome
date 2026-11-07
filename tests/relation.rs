//! belongsTo registration, default order, `seq` for `setting_*`, display field. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::association::{
    BelongsToOf, HasManyOf, ManyToManyOf, RelationInfo, RelationKind, SortOrder,
};
use cakedome::meta::{FieldKind, default_order};
use cakedome::policy::{BasicIdentity, PermissionSet, TablePolicy};
use cakedome::registry::{Registry, SaveStrategy};
use cakedome::repo::Repo;
use sea_orm::{IdenStatic, Order};
use serde_json::json;

fn infos<E: sea_orm::EntityTrait>(r: &Registry) -> Vec<RelationInfo> {
    r.relations_of::<E>()
        .unwrap()
        .iter()
        .map(|a| a.info())
        .collect()
}

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
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::items::Entity")]
        Items,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

/// Two foreign keys to the same table (master_colors); no `name` column.
mod items {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "items")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub color_id: i64,
        pub alt_color_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::master_colors::Entity",
            from = "Column::ColorId",
            to = "super::master_colors::Column::Id"
        )]
        Color,
        #[sea_orm(
            belongs_to = "super::master_colors::Entity",
            from = "Column::AltColorId",
            to = "super::master_colors::Column::Id"
        )]
        AltColor,
    }

    /// Needed by the master_colors has_many: with two foreign keys, this points at the primary one.
    impl Related<super::master_colors::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Color.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod setting_item_colors {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_item_colors")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub item_id: i64,
        pub color_id: i64,
        pub seq: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::items::Entity",
            from = "Column::ItemId",
            to = "super::items::Column::Id"
        )]
        Item,
        #[sea_orm(
            belongs_to = "super::master_colors::Entity",
            from = "Column::ColorId",
            to = "super::master_colors::Column::Id"
        )]
        Color,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod setting_tags {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_tags")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub item_id: i64,
        pub seq: i32,
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
        pub item_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

type Items = DefaultRepo<items::Entity>;
type Colors = DefaultRepo<master_colors::Entity>;
type ItemColors = DefaultRepo<setting_item_colors::Entity>;

const COLOR: BelongsToOf<Items, Colors> = BelongsToOf::new("color", items::Column::ColorId);
const ALT_COLOR: BelongsToOf<Items, Colors> =
    BelongsToOf::new("alt_color", items::Column::AltColorId);
const ITEMS: HasManyOf<Colors, Items> = HasManyOf::new("items", items::Column::ColorId);
const COLORS: ManyToManyOf<Items, ItemColors, Colors> = ManyToManyOf::new(
    "colors",
    setting_item_colors::Column::ItemId,
    setting_item_colors::Column::ColorId,
);

fn order_names<C: IdenStatic>(order: Vec<(C, Order)>) -> Vec<(&'static str, bool)> {
    order
        .into_iter()
        .map(|(c, o)| (c.as_str(), matches!(o, Order::Asc)))
        .collect()
}

// ---- seq ----

#[test]
#[should_panic(expected = "seq")]
fn setting_table_requires_seq() {
    common::fields::<setting_links::Entity>();
}

#[test]
fn seq_is_an_integer_field_defaulting_to_zero() {
    let fields = common::fields::<setting_tags::Entity>();
    let seq = fields.iter().find(|f| f.name == "seq").unwrap();
    assert_eq!(seq.kind, FieldKind::Integer);
    assert!(!seq.managed);
    assert_eq!(seq.default, Some(json!(0)));
}

#[test]
fn default_order_uses_seq_then_id() {
    assert_eq!(
        order_names(default_order::<setting_tags::Entity>(&common::spec::<
            setting_tags::Entity,
        >())),
        [("seq", true), ("id", true)]
    );
}

#[test]
fn default_order_is_id_without_seq() {
    assert_eq!(
        order_names(default_order::<items::Entity>(
            &common::spec::<items::Entity>()
        )),
        [("id", true)]
    );
}

fn registry() -> Registry {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR)
        .association(ALT_COLOR)
        .default_contain(["color"]);
    r
}

#[test]
fn belongs_to_reads_column_and_target_from_relation() {
    let r = registry();
    assert_eq!(
        infos::<items::Entity>(&r),
        [
            RelationInfo {
                name: "color",
                kind: RelationKind::BelongsTo,
                target: "master_colors",
                through: None,
                foreign_key: "color_id",
                target_foreign_key: None,
                binding_key: None,
                target_binding_key: None,
                label: None,
                save: None,
                filtered: false,
                finder: false,
                sort: Vec::new(),
                dependent: false,
                cascade_callbacks: true,
            },
            RelationInfo {
                name: "alt_color",
                kind: RelationKind::BelongsTo,
                target: "master_colors",
                through: None,
                foreign_key: "alt_color_id",
                target_foreign_key: None,
                binding_key: None,
                target_binding_key: None,
                label: None,
                save: None,
                filtered: false,
                finder: false,
                sort: Vec::new(),
                dependent: false,
                cascade_callbacks: true,
            },
        ]
    );
    assert_eq!(r.default_contain_of::<items::Entity>().unwrap(), ["color"]);
    assert!(
        r.relations_of::<master_colors::Entity>()
            .unwrap()
            .is_empty()
    );
    assert!(
        r.default_contain_of::<master_colors::Entity>()
            .unwrap()
            .is_empty()
    );
    r.verify();
}

#[test]
#[should_panic(expected = "color")]
fn duplicate_relation_name_panics() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR)
        .association(BelongsToOf::<Items, Colors>::new(
            "color",
            items::Column::AltColorId,
        ));
}

#[test]
#[should_panic(expected = "title")]
fn relation_name_same_as_column_panics() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(BelongsToOf::<Items, Colors>::new(
            "title",
            items::Column::ColorId,
        ));
}

#[test]
#[should_panic(expected = "nope")]
fn default_contain_with_unknown_name_panics() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR)
        .default_contain(["color", "nope"]);
}

#[test]
#[should_panic(expected = "master_colors")]
fn verify_panics_when_target_is_not_registered() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR);
    r.verify();
}

// ---- display field ----

#[test]
fn display_field_defaults_to_the_primary_key() {
    let r = registry();
    assert_eq!(
        r.display_field_of::<master_colors::Entity>(),
        Some("id"),
        "default is the primary key even with a name column"
    );
    assert_eq!(r.display_field_of::<items::Entity>(), Some("id"));
    assert_eq!(
        r.display_field_of::<setting_tags::Entity>(),
        None,
        "unregistered"
    );

    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .display_field(items::Column::Title);
    assert_eq!(r.display_field_of::<items::Entity>(), Some("title"));
}

// ---- Policy ----

#[tokio::test]
async fn can_contain_defaults_to_true() {
    let identity = BasicIdentity::user(1, None, PermissionSet::new(["entity.items.view"]));
    let ctx = common::context(identity);
    assert!(
        DefaultRepo::<items::Entity>::new()
            .can_contain(&ctx, "color")
            .await
            .unwrap()
    );
}

#[test]
fn has_many_reads_child_column_and_table_from_child_relation() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(ITEMS.label("品目").save(SaveStrategy::Replace))
        .association(HasManyOf::<Colors, Items>::new(
            "alt_items",
            items::Column::AltColorId,
        ))
        .default_contain(["items"]);
    r.add(DefaultRepo::<items::Entity>::new());

    assert_eq!(
        infos::<master_colors::Entity>(&r),
        [
            RelationInfo {
                name: "items",
                kind: RelationKind::HasMany,
                target: "items",
                through: None,
                foreign_key: "color_id",
                target_foreign_key: None,
                binding_key: None,
                target_binding_key: None,
                label: Some("品目"),
                save: Some(SaveStrategy::Replace),
                filtered: false,
                finder: false,
                sort: Vec::new(),
                dependent: false,
                cascade_callbacks: true,
            },
            RelationInfo {
                name: "alt_items",
                kind: RelationKind::HasMany,
                target: "items",
                through: None,
                foreign_key: "alt_color_id",
                target_foreign_key: None,
                binding_key: None,
                target_binding_key: None,
                label: None,
                save: None,
                filtered: false,
                finder: false,
                sort: Vec::new(),
                dependent: false,
                cascade_callbacks: true,
            },
        ]
    );
    r.verify();
}

#[test]
fn has_many_column_borrows_the_static_column_name() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(ITEMS);

    let info = &infos::<master_colors::Entity>(&r)[0];
    assert_eq!(info.kind, RelationKind::HasMany, "registered as hasMany");
    let column = info.foreign_key;
    assert!(
        std::ptr::eq(column, items::Column::ColorId.as_str()),
        "borrows the child's Column::as_str() string (no new allocation)"
    );
}

#[test]
#[should_panic(expected = "items")]
fn has_many_name_conflicting_with_belongs_to_panics() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(ITEMS)
        .association(HasManyOf::<Colors, Items>::new(
            "items",
            items::Column::AltColorId,
        ));
}

#[test]
#[should_panic(expected = "items")]
fn verify_panics_when_has_many_child_is_not_registered() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(ITEMS);
    r.verify();
}

#[test]
fn many_to_many_reads_columns_and_tables_from_both_relations() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLORS.label("色").save(SaveStrategy::Replace))
        .default_contain(["colors"]);
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.add(DefaultRepo::<setting_item_colors::Entity>::new());

    assert_eq!(
        infos::<items::Entity>(&r),
        [RelationInfo {
            name: "colors",
            kind: RelationKind::ManyToMany,
            target: "master_colors",
            through: Some("setting_item_colors"),
            foreign_key: "item_id",
            target_foreign_key: Some("color_id"),
            binding_key: None,
            target_binding_key: None,
            label: Some("色"),
            save: Some(SaveStrategy::Replace),
            filtered: false,
            finder: false,
            sort: Vec::new(),
            dependent: true,
            cascade_callbacks: true,
        }]
    );
    r.verify();
}

/// Many-to-many allows Replace and Append; `verify` rejects AppendWithDelete.
#[test]
#[should_panic(expected = "AppendWithDelete")]
fn many_to_many_rejects_append_with_delete() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLORS.save(SaveStrategy::AppendWithDelete));
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.add(DefaultRepo::<setting_item_colors::Entity>::new());
    r.verify();
}

#[test]
#[should_panic(expected = "setting_item_colors")]
fn verify_panics_when_through_table_is_not_registered() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLORS);
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.verify();
}

mod labelled {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "labelled")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_name = "_label")]
        pub extra: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[test]
#[should_panic(expected = "_label")]
fn column_named_label_is_reserved() {
    let mut r = common::registry();
    r.add(DefaultRepo::<labelled::Entity>::new());
}

#[test]
#[should_panic(expected = "_label")]
fn belongs_to_named_label_is_reserved() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .association(BelongsToOf::<Items, Colors>::new(
            "_label",
            items::Column::ColorId,
        ));
}

#[test]
#[should_panic(expected = "_label")]
fn has_many_named_label_is_reserved() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(HasManyOf::<Colors, Items>::new(
            "_label",
            items::Column::ColorId,
        ));
}

#[test]
#[should_panic(expected = "_label")]
fn many_to_many_named_label_is_reserved() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new()).association(
        ManyToManyOf::<Items, ItemColors, Colors>::new(
            "_label",
            setting_item_colors::Column::ItemId,
            setting_item_colors::Column::ColorId,
        ),
    );
}

fn nested_registry(default_contain: &'static str) -> Registry {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .association(ITEMS);
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR)
        .default_contain([default_contain]);
    r
}

#[test]
fn default_contain_accepts_nested_path() {
    let r = nested_registry("color.items");
    r.verify();
    assert_eq!(
        r.default_contain_of::<items::Entity>().unwrap(),
        ["color.items"]
    );
}

#[test]
#[should_panic(expected = "nope")]
fn default_contain_checks_first_segment_at_registration() {
    nested_registry("nope.items");
}

#[test]
#[should_panic(expected = "color.nope")]
fn verify_checks_nested_segments_of_default_contain() {
    // Targets of later levels may be registered afterwards, so check with verify().
    let r = nested_registry("color.nope");
    r.verify();
}

#[test]
#[should_panic(expected = "3")]
fn default_contain_deeper_than_three_panics() {
    nested_registry("color.items.color.items");
}

#[test]
#[should_panic(expected = "color..items")]
fn default_contain_with_empty_segment_panics() {
    nested_registry("color..items");
}

// Associations are declared as constants carrying all relation info. Type mismatches are
// compile errors; these tests cover the runtime checks (target table name, save strategy).

const COLOR_ITEMS: HasManyOf<Colors, Items> = HasManyOf::new("items", items::Column::ColorId)
    .label("品目")
    .save(SaveStrategy::Replace);
const ITEM_COLORS: ManyToManyOf<Items, ItemColors, Colors> = COLORS.save(SaveStrategy::Replace);

#[test]
fn belongs_to_is_registered_from_a_typed_declaration() {
    let mut r = common::registry();
    r.add(Items::new()).association(COLOR);

    assert_eq!(
        infos::<items::Entity>(&r),
        [RelationInfo {
            name: "color",
            kind: RelationKind::BelongsTo,
            target: "master_colors",
            through: None,
            foreign_key: "color_id",
            target_foreign_key: None,
            binding_key: None,
            target_binding_key: None,
            label: None,
            save: None,
            filtered: false,
            finder: false,
            sort: Vec::new(),
            dependent: false,
            cascade_callbacks: true,
        }],
        "same as registering by string name"
    );
}

#[test]
fn has_many_is_registered_from_a_typed_declaration_with_its_settings() {
    let mut r = common::registry();
    r.add(Colors::new()).association(COLOR_ITEMS);

    assert_eq!(
        infos::<master_colors::Entity>(&r),
        [RelationInfo {
            name: "items",
            kind: RelationKind::HasMany,
            target: "items",
            through: None,
            foreign_key: "color_id",
            target_foreign_key: None,
            binding_key: None,
            target_binding_key: None,
            label: Some("品目"),
            save: Some(SaveStrategy::Replace),
            filtered: false,
            finder: false,
            sort: Vec::new(),
            dependent: false,
            cascade_callbacks: true,
        }],
        "includes the declared label and save strategy"
    );
}

#[test]
fn many_to_many_is_registered_from_a_typed_declaration() {
    let mut r = common::registry();
    r.add(Items::new()).association(ITEM_COLORS);

    assert_eq!(
        infos::<items::Entity>(&r),
        [RelationInfo {
            name: "colors",
            kind: RelationKind::ManyToMany,
            target: "master_colors",
            through: Some("setting_item_colors"),
            foreign_key: "item_id",
            target_foreign_key: Some("color_id"),
            binding_key: None,
            target_binding_key: None,
            label: None,
            save: Some(SaveStrategy::Replace),
            filtered: false,
            finder: false,
            sort: Vec::new(),
            dependent: true,
            cascade_callbacks: true,
        }]
    );
}

fn only_first_color() -> sea_orm::Condition {
    sea_orm::Condition::all().add(sea_orm::ColumnTrait::eq(&items::Column::ColorId, 1))
}

#[test]
fn has_many_declaration_keeps_its_filter_and_sort() {
    const FILTERED: HasManyOf<Colors, Items> = ITEMS
        .filter(only_first_color)
        .sort(&[(items::Column::Id, SortOrder::Desc)]);
    let mut r = common::registry();
    r.add(Colors::new()).association(FILTERED);

    let info = &infos::<master_colors::Entity>(&r)[0];
    assert_eq!(info.kind, RelationKind::HasMany, "registered as hasMany");
    assert!(info.filtered, "has a condition builder");
    assert_eq!(
        info.sort,
        [("id", SortOrder::Desc)],
        "sort is held by column name"
    );
}

#[test]
fn many_to_many_declaration_keeps_its_filter_and_sort() {
    fn active() -> sea_orm::Condition {
        sea_orm::Condition::all().add(sea_orm::ColumnTrait::eq(
            &master_colors::Column::IsActive,
            true,
        ))
    }
    const FILTERED: ManyToManyOf<Items, ItemColors, Colors> = COLORS
        .filter(active)
        .sort(&[(master_colors::Column::Code, SortOrder::Asc)]);
    let mut r = common::registry();
    r.add(Items::new()).association(FILTERED);

    let info = &infos::<items::Entity>(&r)[0];
    assert_eq!(
        info.kind,
        RelationKind::ManyToMany,
        "registered as many-to-many"
    );
    assert!(info.filtered, "has a condition builder");
    assert_eq!(
        info.sort,
        [("code", SortOrder::Asc)],
        "sort uses the target's column name"
    );
}

mod coded_targets {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "coded_targets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub key: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod coded_refs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "coded_refs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub target_key: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[test]
#[should_panic(expected = "the target of association target must have a single id column")]
fn belongs_to_a_target_whose_primary_key_is_not_id_panics() {
    // A foreign key points at the target's primary key, which must be the single column `id`.
    const TARGET: cakedome::association::BelongsToOf<
        DefaultRepo<coded_refs::Entity>,
        DefaultRepo<coded_targets::Entity>,
    > = cakedome::association::BelongsToOf::new("target", coded_refs::Column::TargetKey);
    let mut r = common::registry();
    r.add(DefaultRepo::<coded_refs::Entity>::new())
        .association(TARGET);
}

/// Items under a Repo type other than the registered `DefaultRepo<items::Entity>`.
struct OtherItems;
impl Repo for OtherItems {
    type Entity = items::Entity;
}

impl cakedome::policy::TablePolicy for OtherItems {}

impl cakedome::policy::EntityPolicy for OtherItems {}

struct OtherColors;
impl Repo for OtherColors {
    type Entity = master_colors::Entity;
}

impl cakedome::policy::TablePolicy for OtherColors {}

impl cakedome::policy::EntityPolicy for OtherColors {}

struct OtherItemColors;
impl Repo for OtherItemColors {
    type Entity = setting_item_colors::Entity;
}

impl cakedome::policy::TablePolicy for OtherItemColors {}

impl cakedome::policy::EntityPolicy for OtherItemColors {}

#[test]
fn verify_accepts_matching_declaration_types() {
    let mut r = common::registry();
    r.add(Items::new()).association(COLOR).association(COLORS);
    r.add(Colors::new()).association(ITEMS);
    r.add(ItemColors::new());
    r.verify();
}

#[test]
#[should_panic(expected = "OtherItems")]
fn verify_panics_when_own_type_differs() {
    const C: BelongsToOf<OtherItems, Colors> = BelongsToOf::new("color", items::Column::ColorId);
    let mut r = common::registry();
    r.add(Items::new()).association(C);
    r.add(Colors::new());
    r.verify();
}

#[test]
#[should_panic(expected = "OtherColors")]
fn verify_panics_when_target_type_differs() {
    const C: BelongsToOf<Items, OtherColors> = BelongsToOf::new("color", items::Column::ColorId);
    let mut r = common::registry();
    r.add(Items::new()).association(C);
    r.add(Colors::new());
    r.verify();
}

#[test]
#[should_panic(expected = "OtherItemColors")]
fn verify_panics_when_through_type_differs() {
    const C: ManyToManyOf<Items, OtherItemColors, Colors> = ManyToManyOf::new(
        "colors",
        setting_item_colors::Column::ItemId,
        setting_item_colors::Column::ColorId,
    );
    let mut r = common::registry();
    r.add(Items::new()).association(C);
    r.add(Colors::new());
    r.add(ItemColors::new());
    r.verify();
}

#[test]
#[should_panic(expected = "colors")]
fn verify_type_mismatch_message_names_the_association() {
    const C: ManyToManyOf<Items, ItemColors, OtherColors> = ManyToManyOf::new(
        "colors",
        setting_item_colors::Column::ItemId,
        setting_item_colors::Column::ColorId,
    );
    let mut r = common::registry();
    r.add(Items::new()).association(C);
    r.add(Colors::new());
    r.add(ItemColors::new());
    r.verify();
}
