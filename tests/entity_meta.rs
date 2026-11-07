//! Static entity info (`EntityMeta`), built at registration and independent of Identity. No DB.

mod common;

use cakedome::DefaultRepo;
use cakedome::association::{BelongsToOf, HasManyOf, RelationKind};
use cakedome::meta::Lifecycle;
use cakedome::registry::SaveStrategy;

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
        pub seq: i32,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::master_colors::Entity",
            from = "Column::ColorId",
            to = "super::master_colors::Column::Id"
        )]
        Color,
    }

    impl Related<super::master_colors::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Color.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

type Colors = DefaultRepo<master_colors::Entity>;
type Items = DefaultRepo<items::Entity>;

#[test]
fn meta_collects_the_static_information_of_an_entity() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .label("色")
        .display_field(master_colors::Column::Name)
        .association(
            HasManyOf::<Colors, Items>::new("items", items::Column::ColorId)
                .save(SaveStrategy::Replace),
        )
        .default_contain(["items"]);
    r.add(DefaultRepo::<items::Entity>::new())
        .label("品目")
        .display_field(items::Column::Title)
        .association(BelongsToOf::<Items, Colors>::new(
            "color",
            items::Column::ColorId,
        ));
    r.verify();

    let colors = r.meta_of::<master_colors::Entity>().unwrap();
    assert_eq!(colors.name, "master_colors");
    assert_eq!(colors.label, "色");
    assert_eq!(colors.spec.lifecycle, Lifecycle::Activatable);
    assert_eq!(colors.display_field, "name");
    assert_eq!(colors.default_contain, ["items"]);
    assert_eq!(colors.relations.len(), 1);
    assert_eq!(colors.relations[0].info().kind, RelationKind::HasMany);
    let names: Vec<&str> = colors.fields.iter().map(|f| f.name).collect();
    assert_eq!(names, ["id", "code", "name", "is_active"]);

    let items = r.meta_of::<items::Entity>().unwrap();
    assert_eq!(items.spec.lifecycle, Lifecycle::SoftDelete);
    assert_eq!(items.spec.order_column, Some("seq"));
    assert_eq!(items.display_field, "title");
}

#[test]
fn meta_can_be_looked_up_by_table_name() {
    let mut r = common::registry();
    r.add(DefaultRepo::<master_colors::Entity>::new())
        .label("色");

    assert_eq!(r.meta("master_colors").unwrap().label, "色");
    assert!(r.meta("items").is_none());
    assert!(r.meta_of::<items::Entity>().is_none());
}

#[test]
fn accessors_read_the_same_meta() {
    let mut r = common::registry();
    r.add(DefaultRepo::<items::Entity>::new())
        .label("品目")
        .display_field(items::Column::Title);

    let meta = r.meta_of::<items::Entity>().unwrap();
    assert_eq!(r.label_of::<items::Entity>(), Some(meta.label.as_str()));
    assert_eq!(
        r.fields_of::<items::Entity>().unwrap(),
        meta.fields.as_slice()
    );
    assert_eq!(
        r.display_field_of::<items::Entity>(),
        Some(meta.display_field)
    );
    assert_eq!(r.spec_of::<items::Entity>(), meta.spec);
}

#[derive(Debug, PartialEq)]
enum TableKind {
    System,
    Business,
}

struct SystemColorsRepo;

impl cakedome::Repo for SystemColorsRepo {
    type Entity = master_colors::Entity;

    fn initialize(
        e: cakedome::registry::RepoConfig<'_, master_colors::Entity>,
    ) -> cakedome::registry::RepoConfig<'_, master_colors::Entity> {
        e.extension(TableKind::System)
    }
}

impl cakedome::policy::TablePolicy for SystemColorsRepo {}
impl cakedome::policy::EntityPolicy for SystemColorsRepo {}

#[test]
fn extension_put_in_initialize_is_read_by_name_and_from_the_meta() {
    let mut r = common::registry();
    r.add(SystemColorsRepo);
    r.add(Items::new());

    assert_eq!(
        r.extension::<TableKind>("master_colors"),
        Some(&TableKind::System)
    );
    assert_eq!(
        r.meta("master_colors").unwrap().extension::<TableKind>(),
        Some(&TableKind::System)
    );
    assert_eq!(
        r.extension::<TableKind>("items"),
        None,
        "entity without the extension"
    );
    assert_eq!(
        r.extension::<String>("master_colors"),
        None,
        "separate value per type"
    );
    assert_eq!(
        r.extension::<TableKind>("nothing"),
        None,
        "unregistered name"
    );
}

#[test]
fn extension_can_be_put_after_add_and_the_same_type_is_replaced() {
    let mut r = common::registry();
    r.add(SystemColorsRepo);
    r.add(Items::new()).extension(TableKind::Business);
    r.set_extension::<master_colors::Entity, _>(TableKind::Business);

    assert_eq!(
        r.extension::<TableKind>("items"),
        Some(&TableKind::Business)
    );
    assert_eq!(
        r.extension::<TableKind>("master_colors"),
        Some(&TableKind::Business)
    );
}

#[test]
#[should_panic(expected = "is not registered")]
fn set_extension_on_an_unregistered_entity_panics() {
    let mut r = common::registry();
    r.set_extension::<items::Entity, _>(TableKind::System);
}

trait AppTable: Send + Sync {
    fn kind(&self) -> TableKind;
    fn export_name(&self) -> String;
}

impl AppTable for SystemColorsRepo {
    fn kind(&self) -> TableKind {
        TableKind::System
    }

    fn export_name(&self) -> String {
        "colors".to_string()
    }
}

struct PrefixedItemsRepo {
    prefix: &'static str,
}

impl cakedome::Repo for PrefixedItemsRepo {
    type Entity = items::Entity;
}

impl cakedome::policy::TablePolicy for PrefixedItemsRepo {}
impl cakedome::policy::EntityPolicy for PrefixedItemsRepo {}

impl AppTable for PrefixedItemsRepo {
    fn kind(&self) -> TableKind {
        TableKind::Business
    }

    fn export_name(&self) -> String {
        format!("{}items", self.prefix)
    }
}

#[test]
fn registered_repo_is_taken_out_by_its_type() {
    let mut r = common::registry();
    r.add(PrefixedItemsRepo { prefix: "tenant_" });

    let repo = r.repo::<PrefixedItemsRepo>().unwrap();
    assert_eq!(
        repo.export_name(),
        "tenant_items",
        "keeps the fields it was registered with"
    );
    assert!(
        r.repo::<Items>().is_none(),
        "None when looked up by a different type"
    );
}

#[test]
fn trait_object_in_the_extension_is_called_by_name() {
    use std::sync::Arc;

    let mut r = common::registry();
    r.add(SystemColorsRepo)
        .extension::<Arc<dyn AppTable>>(Arc::new(SystemColorsRepo));
    r.add(PrefixedItemsRepo { prefix: "tenant_" });
    // A Repo with fields is stored as the same instance that was registered.
    let items: Arc<dyn AppTable> = r.repo::<PrefixedItemsRepo>().unwrap();
    r.set_extension::<items::Entity, _>(items);

    let names: Vec<String> = r
        .metas_in_order()
        .filter_map(|m| m.extension::<Arc<dyn AppTable>>())
        .map(|t| t.export_name())
        .collect();
    assert_eq!(names, ["colors", "tenant_items"]);
    let colors = r.extension::<Arc<dyn AppTable>>("master_colors").unwrap();
    assert_eq!(colors.kind(), TableKind::System);
}
