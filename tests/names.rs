//! Separating the Repo name from the table name. No DB.
//! Two DBs may share a table name; one Repo gets a distinct name so both can be registered.

mod common;

use cakedome::DefaultRepo;
use cakedome::association::{BelongsToOf, RelationInfo, RelationKind};
use cakedome::defaults::RefBy;
use cakedome::meta::Lifecycle;
use cakedome::policy::{Action, BasicIdentity, PermissionSet, TablePolicy, entity_permission};
use cakedome::registry::Registry;
use cakedome::repo::Repo;
use sea_orm::{DbBackend, EntityTrait, QueryTrait};

mod tenant_products {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "master_products")]
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
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Same table name as the default-DB product, different `code` type (shows which one `RefBy` used).
mod common_products {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(schema_name = "common", table_name = "master_products")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub code: i32,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        pub is_active: bool,
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
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
        pub common_product_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Registered under a name different from its table name.
struct CommonProducts;

impl Repo for CommonProducts {
    type Entity = common_products::Entity;

    fn repo_name() -> &'static str {
        "common_master_products"
    }
}

impl cakedome::policy::TablePolicy for CommonProducts {}

impl cakedome::policy::EntityPolicy for CommonProducts {}

type TenantProducts = DefaultRepo<tenant_products::Entity>;
type Items = DefaultRepo<items::Entity>;

const COMMON_PRODUCT: BelongsToOf<Items, CommonProducts> =
    BelongsToOf::new("common_product", items::Column::CommonProductId);

fn identity(patterns: &[&str]) -> BasicIdentity {
    BasicIdentity::user(1, None, PermissionSet::new(patterns.iter().copied()))
}

fn registry() -> Registry {
    let mut r = Registry::new(
        common::TestRules,
        &[
            ("master_products", "商品"),
            ("common_master_products", "共通の商品"),
        ],
        &[],
    );
    r.add(TenantProducts::new());
    r.add(CommonProducts);
    r.add(Items::new()).association(COMMON_PRODUCT);
    r
}

#[test]
fn name_defaults_to_the_table_name() {
    assert_eq!(<TenantProducts as Repo>::repo_name(), "master_products");
    assert_eq!(<Items as Repo>::repo_name(), "items");
    assert_eq!(
        <CommonProducts as Repo>::repo_name(),
        "common_master_products"
    );
}

#[test]
#[should_panic(expected = "master_products")]
fn registering_two_repos_with_the_same_name_panics() {
    let mut r = common::registry();
    r.add(TenantProducts::new());
    r.add(DefaultRepo::<common_products::Entity>::new());
}

#[tokio::test]
async fn permission_string_uses_the_repo_name() {
    assert_eq!(
        entity_permission::<CommonProducts>(Action::View),
        "entity.common_master_products.view"
    );
    assert_eq!(
        entity_permission::<TenantProducts>(Action::Edit),
        "entity.master_products.edit"
    );

    // A default-DB product's permission does not cover the common product.
    let tenant_only = common::context(identity(&["entity.master_products.view"]));
    assert!(
        TenantProducts::new()
            .can(&tenant_only, Action::View)
            .await
            .unwrap()
    );
    assert!(
        !CommonProducts
            .can(&tenant_only, Action::View)
            .await
            .unwrap()
    );

    let common_viewer = common::context(identity(&["entity.common_master_products.view"]));
    assert!(
        CommonProducts
            .can(&common_viewer, Action::View)
            .await
            .unwrap()
    );
    assert!(
        !TenantProducts::new()
            .can(&common_viewer, Action::View)
            .await
            .unwrap()
    );
}

#[test]
fn entity_label_dictionary_is_keyed_by_the_repo_name() {
    let r = registry();
    assert_eq!(r.label_of::<tenant_products::Entity>(), Some("商品"));
    assert_eq!(r.label_of::<common_products::Entity>(), Some("共通の商品"));
}

#[test]
fn association_target_uses_the_repo_name() {
    let r = registry();
    let infos: Vec<RelationInfo> = r
        .relations_of::<items::Entity>()
        .unwrap()
        .iter()
        .map(|a| a.info())
        .collect();
    assert_eq!(
        infos,
        [RelationInfo {
            name: "common_product",
            kind: RelationKind::BelongsTo,
            target: "common_master_products",
            through: None,
            foreign_key: "common_product_id",
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
        }]
    );
}

#[test]
fn table_rules_still_see_the_table_name() {
    // Table rules depend on the table name (master_*), not the Repo name.
    let r = registry();
    assert_eq!(
        r.spec_of::<common_products::Entity>().lifecycle,
        Lifecycle::Activatable
    );
    assert_eq!(
        r.spec_of::<common_products::Entity>().uniques,
        vec![vec!["code"]]
    );
}

#[test]
fn ref_by_finds_the_target_by_repo_type() {
    // The common `code` is an integer; looking up by table name would hit the default-DB product
    // (string) and mismatch.
    let mut r = registry();
    r.add(DefaultRepo::<defaults_owner::Entity>::new())
        .default_from(
            defaults_owner::Column::CommonProductId,
            RefBy::new::<CommonProducts>(common_products::Column::Code, 5),
        );
    r.verify();
}

#[test]
#[should_panic(expected = "common_master_products.code")]
fn ref_by_message_uses_the_repo_name() {
    let mut r = registry();
    r.add(DefaultRepo::<defaults_owner::Entity>::new())
        .default_from(
            defaults_owner::Column::CommonProductId,
            RefBy::new::<CommonProducts>(common_products::Column::Code, "x"),
        );
    r.verify();
}

mod defaults_owner {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "defaults_owner")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub common_product_id: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[test]
fn sql_uses_the_table_name_and_schema() {
    let common = common_products::Entity::find()
        .build(DbBackend::MySql)
        .to_string();
    assert!(
        common.contains("FROM `common`.`master_products`"),
        "common product has a DB name: {common}"
    );
    let tenant = tenant_products::Entity::find()
        .build(DbBackend::MySql)
        .to_string();
    assert!(
        tenant.contains("FROM `master_products`"),
        "default-DB product has no DB name: {tenant}"
    );
}
