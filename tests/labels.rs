//! Field label resolution: explicit > base dictionary > column name. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::association::{BelongsToOf, HasManyOf, ManyToManyOf};
use cakedome::registry::Registry;
use sea_orm::EntityTrait;

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
    pub enum Relation {
        #[sea_orm(has_many = "super::items::Entity")]
        Items,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

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

type Colors = DefaultRepo<master_colors::Entity>;
type Items = DefaultRepo<items::Entity>;
type ItemColors = DefaultRepo<setting_item_colors::Entity>;

const COLOR: BelongsToOf<Items, Colors> = BelongsToOf::new("color", items::Column::ColorId);
const ALT_COLOR: BelongsToOf<Items, Colors> =
    BelongsToOf::new("alt_color", items::Column::AltColorId);
const COLOR_ITEMS: HasManyOf<Colors, Items> = HasManyOf::new("items", items::Column::ColorId);
const ITEM_COLORS: ManyToManyOf<Items, ItemColors, Colors> = ManyToManyOf::new(
    "colors",
    setting_item_colors::Column::ItemId,
    setting_item_colors::Column::ColorId,
);

const BASE: &[(&str, &str)] = &[
    ("id", "ID"),
    ("code", "コード"),
    ("name", "名前"),
    ("created", "作成日時"),
    ("modified", "更新日時"),
    ("seq", "並び順"),
    ("color_id", "ベースの色ID"),
];

fn label<E: EntityTrait>(r: &Registry, name: &str) -> String {
    r.fields_of::<E>()
        .expect("registered")
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} exists"))
        .label
        .clone()
}

fn base() -> Registry {
    Registry::new(common::TestRules, &[], BASE)
}

#[test]
fn base_labels_apply_and_other_columns_keep_their_name() {
    let mut r = base();
    r.add(DefaultRepo::<master_colors::Entity>::new());

    assert_eq!(label::<master_colors::Entity>(&r, "id"), "ID");
    assert_eq!(label::<master_colors::Entity>(&r, "code"), "コード");
    assert_eq!(label::<master_colors::Entity>(&r, "name"), "名前");
    assert_eq!(label::<master_colors::Entity>(&r, "created"), "作成日時");
    assert_eq!(label::<master_colors::Entity>(&r, "is_active"), "is_active");
}

#[test]
fn explicit_labels_override_base() {
    let mut r = base();
    r.add(DefaultRepo::<items::Entity>::new())
        .labels([(items::Column::Title, "品名")])
        .field(items::Column::Id, |f| f.label("品目番号"));

    assert_eq!(label::<items::Entity>(&r, "title"), "品名");
    assert_eq!(label::<items::Entity>(&r, "id"), "品目番号");
}

#[test]
fn relations_do_not_change_foreign_key_labels() {
    let mut r = base();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .label("色")
        .association(COLOR_ITEMS);
    r.add(DefaultRepo::<items::Entity>::new())
        .label("品目")
        .association(COLOR)
        .association(ALT_COLOR)
        .association(ITEM_COLORS);
    r.add(DefaultRepo::<setting_item_colors::Entity>::new());
    r.verify();

    assert_eq!(
        label::<items::Entity>(&r, "color_id"),
        "ベースの色ID",
        "belongsTo/hasMany foreign keys keep the dictionary label"
    );
    assert_eq!(
        label::<items::Entity>(&r, "alt_color_id"),
        "alt_color_id",
        "falls back to the column name (not the target's label)"
    );
    assert_eq!(
        label::<setting_item_colors::Entity>(&r, "color_id"),
        "ベースの色ID",
        "junction table columns keep the dictionary label"
    );
    assert_eq!(
        label::<setting_item_colors::Entity>(&r, "item_id"),
        "item_id"
    );
}

#[test]
fn explicit_foreign_key_label_overrides_base() {
    let mut r = base();
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.add(DefaultRepo::<items::Entity>::new())
        .association(COLOR)
        .labels([(items::Column::ColorId, "色")]);
    r.verify();

    assert_eq!(label::<items::Entity>(&r, "color_id"), "色");
}

const ENTITY_LABELS: &[(&str, &str)] = &[("master_colors", "色"), ("items", "品目")];

#[test]
fn entity_labels_come_from_the_dictionary_and_define_overrides() {
    let mut r = Registry::new(common::TestRules, ENTITY_LABELS, BASE);
    r.add(DefaultRepo::<master_colors::Entity>::new());
    r.add(DefaultRepo::<items::Entity>::new())
        .label("商品")
        .association(COLOR);
    r.add(DefaultRepo::<setting_item_colors::Entity>::new());
    r.verify();

    assert_eq!(r.label_of::<master_colors::Entity>(), Some("色"));
    assert_eq!(
        r.label_of::<items::Entity>(),
        Some("商品"),
        "label from initialize wins"
    );
    assert_eq!(
        r.label_of::<setting_item_colors::Entity>(),
        Some("setting_item_colors"),
        "falls back to the table name"
    );
    // Foreign-key labels come from the field dictionary, not from the entity label.
    assert_eq!(label::<items::Entity>(&r, "color_id"), "ベースの色ID");
}
