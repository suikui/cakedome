//! Registration checks without a DB: `on_change` and `Effect`, binding key types, `RepoConfig`
//! settings, association labels, `default_contain` of later levels and the small `layer` functions.

mod common;

use std::any::Any;

use cakedome::DefaultRepo;
use cakedome::association::{
    Association, AssociationType, BelongsToOf, Cardinality, HasManyOf, HasOneOf, ManyToManyOf,
    RelationInfo, RelationKind,
};
use cakedome::defaults::DynamicDefault;
use cakedome::messages::Messages;
use cakedome::meta::FieldKind;
use cakedome::policy::{BasicIdentity, PermissionSet};
use cakedome::registry::{Effect, EffectMeta, OnChange, Registry};
use cakedome::repo::RepoError;
use cakedome::{Context, layer};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition};
use serde_json::{Value as Json, json};

mod lines {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "lines")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub product_id: i64,
        pub other_id: i64,
        pub product_no: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub note: Option<String>,
        pub quantity: i32,
        pub flag: bool,
        pub at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod products {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "products")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub no: i64,
        pub line_id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub name: String,
        #[sea_orm(column_type = "Text")]
        pub memo: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod master_kinds {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "master_kinds")]
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

mod notes {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "notes")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub body: String,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod accounts {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "accounts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub username: String,
        pub employee_no: i64,
        pub is_admin: bool,
        pub joined_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod posts {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub account_id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub author_name: String,
        pub reviewer_no: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub title: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod groups {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "groups")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub code: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod account_groups {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "account_groups")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub username: String,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub group_code: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

type Lines = DefaultRepo<lines::Entity>;
type Products = DefaultRepo<products::Entity>;
type Accounts = DefaultRepo<accounts::Entity>;
type Posts = DefaultRepo<posts::Entity>;
type Groups = DefaultRepo<groups::Entity>;
type AccountGroups = DefaultRepo<account_groups::Entity>;

const PRODUCT: BelongsToOf<Lines, Products> = BelongsToOf::new("product", lines::Column::ProductId);
const PRODUCT_BY_NO: BelongsToOf<Lines, Products, i64> =
    BelongsToOf::new("product_by_no", lines::Column::ProductNo).binding_key(products::Column::No);

// ---- on_change and Effect ----

#[test]
fn on_change_is_stored_with_the_effects_in_order() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).association(PRODUCT).on_change(
        lines::Column::ProductId,
        [
            Effect::clear(lines::Column::Quantity),
            Effect::copy(
                PRODUCT,
                [
                    (lines::Column::Name, products::Column::Name),
                    (lines::Column::Note, products::Column::Memo),
                ],
            ),
        ],
    );
    r.verify();

    assert_eq!(
        r.meta_of::<lines::Entity>().unwrap().on_change,
        [OnChange {
            column: "product_id",
            effects: vec![
                EffectMeta::Clear { field: "quantity" },
                EffectMeta::Copy {
                    relation: "product",
                    foreign_key: "product_id",
                    target: "products",
                    map: vec![("name", "name"), ("note", "memo")],
                },
            ],
        }]
    );
}

#[test]
#[should_panic(expected = "lines.product_id: on_change is declared twice")]
fn on_change_twice_for_a_column_panics() {
    let mut r = common::registry();
    r.add(Lines::new())
        .on_change(
            lines::Column::ProductId,
            [Effect::clear(lines::Column::Name)],
        )
        .on_change(
            lines::Column::ProductId,
            [Effect::clear(lines::Column::Note)],
        );
}

#[test]
#[should_panic(
    expected = "lines.other_id: the foreign key of association product in on_change copy is product_id"
)]
fn copy_through_a_relation_on_another_column_panics() {
    let mut r = common::registry();
    r.add(Lines::new()).association(PRODUCT).on_change(
        lines::Column::OtherId,
        [Effect::copy(
            PRODUCT,
            [(lines::Column::Name, products::Column::Name)],
        )],
    );
}

#[test]
#[should_panic(expected = "association product in on_change copy is not registered as belongsTo")]
fn copy_of_a_has_many_registered_under_the_name_panics() {
    const PRODUCTS_OF_LINE: HasManyOf<Lines, Products> =
        HasManyOf::new("product", products::Column::LineId);
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).association(PRODUCTS_OF_LINE).on_change(
        lines::Column::ProductId,
        [Effect::copy(
            PRODUCT,
            [(lines::Column::Name, products::Column::Name)],
        )],
    );
    r.verify();
}

#[test]
#[should_panic(expected = "association product in on_change copy is not registered as belongsTo")]
fn copy_of_an_association_that_is_not_registered_panics() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).on_change(
        lines::Column::ProductId,
        [Effect::copy(
            PRODUCT,
            [(lines::Column::Name, products::Column::Name)],
        )],
    );
    r.verify();
}

#[test]
#[should_panic(expected = "association product_by_no in on_change copy cannot be used")]
fn copy_through_a_binding_key_panics() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).association(PRODUCT_BY_NO).on_change(
        lines::Column::ProductNo,
        [Effect::copy(
            PRODUCT_BY_NO,
            [(lines::Column::Name, products::Column::Name)],
        )],
    );
    r.verify();
}

#[test]
#[should_panic(
    expected = "lines.quantity: on_change copy type integer does not match the type string of products.name"
)]
fn copy_between_columns_of_different_types_panics() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).association(PRODUCT).on_change(
        lines::Column::ProductId,
        [Effect::copy(
            PRODUCT,
            [(lines::Column::Quantity, products::Column::Name)],
        )],
    );
    r.verify();
}

#[test]
fn copy_between_string_and_text_columns_is_allowed() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).association(PRODUCT).on_change(
        lines::Column::ProductId,
        [Effect::copy(
            PRODUCT,
            [
                (lines::Column::Note, products::Column::Name),
                (lines::Column::Name, products::Column::Memo),
            ],
        )],
    );
    r.verify();
    let fields = r.fields_of::<lines::Entity>().unwrap();
    assert_eq!(
        fields.iter().find(|f| f.name == "note").unwrap().kind,
        FieldKind::Text
    );
}

fn registry_with_effects(
    effects: impl IntoIterator<Item = (lines::Column, Vec<Effect<lines::Entity>>)>,
) -> Registry {
    let mut r = common::registry();
    let mut config = r.add(Lines::new());
    for (column, list) in effects {
        config = config.on_change(column, list);
    }
    r
}

#[test]
#[should_panic(expected = "lines: on_change forms a cycle (name → note → name)")]
fn a_cycle_of_two_columns_panics() {
    registry_with_effects([
        (
            lines::Column::Name,
            vec![Effect::clear(lines::Column::Note)],
        ),
        (
            lines::Column::Note,
            vec![Effect::clear(lines::Column::Name)],
        ),
    ])
    .verify();
}

#[test]
#[should_panic(expected = "lines: on_change forms a cycle (name → note → quantity → name)")]
fn a_cycle_of_three_columns_panics() {
    registry_with_effects([
        (
            lines::Column::Name,
            vec![Effect::clear(lines::Column::Note)],
        ),
        (
            lines::Column::Note,
            vec![Effect::clear(lines::Column::Quantity)],
        ),
        (
            lines::Column::Quantity,
            vec![Effect::clear(lines::Column::Name)],
        ),
    ])
    .verify();
}

#[test]
#[should_panic(expected = "lines: on_change forms a cycle (product_id → name → product_id)")]
fn a_cycle_through_a_copy_panics() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new())
        .association(PRODUCT)
        .on_change(
            lines::Column::ProductId,
            [Effect::copy(
                PRODUCT,
                [(lines::Column::Name, products::Column::Name)],
            )],
        )
        .on_change(
            lines::Column::Name,
            [Effect::clear(lines::Column::ProductId)],
        );
    r.verify();
}

#[test]
fn merging_effects_without_a_cycle_is_allowed() {
    registry_with_effects([
        (
            lines::Column::Name,
            vec![Effect::clear(lines::Column::Flag)],
        ),
        (
            lines::Column::Note,
            vec![Effect::clear(lines::Column::Flag)],
        ),
        (lines::Column::Flag, vec![Effect::clear(lines::Column::At)]),
    ])
    .verify();
}

// ---- binding key types ----

const AUTHOR: BelongsToOf<Posts, Accounts> = BelongsToOf::new("author", posts::Column::AccountId);
const POSTS: HasManyOf<Accounts, Posts> = HasManyOf::new("posts", posts::Column::AccountId);

fn bound_registry(f: impl FnOnce(&mut Registry)) -> Registry {
    let mut r = common::registry();
    f(&mut r);
    r.add(Groups::new());
    r.add(AccountGroups::new());
    r
}

#[test]
fn matching_binding_keys_are_accepted() {
    const BY_NAME: BelongsToOf<Posts, Accounts, String> =
        BelongsToOf::new("by_name", posts::Column::AuthorName)
            .binding_key(accounts::Column::Username);
    const BY_NO: BelongsToOf<Posts, Accounts, i64> =
        BelongsToOf::new("by_no", posts::Column::ReviewerNo)
            .binding_key(accounts::Column::EmployeeNo);
    const WRITTEN: HasManyOf<Accounts, Posts, String> =
        HasManyOf::new("written", posts::Column::AuthorName)
            .binding_key(accounts::Column::Username);
    const GROUPS: ManyToManyOf<Accounts, AccountGroups, Groups, String, String> =
        ManyToManyOf::new(
            "groups",
            account_groups::Column::Username,
            account_groups::Column::GroupCode,
        )
        .binding_key(accounts::Column::Username)
        .target_binding_key(groups::Column::Code);
    bound_registry(|r| {
        r.add(Accounts::new())
            .association(WRITTEN)
            .association(GROUPS);
        r.add(Posts::new()).association(BY_NAME).association(BY_NO);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "posts.author_name: foreign key column type string does not match bindingKey column accounts.employee_no of type big_integer"
)]
fn belongs_to_foreign_key_of_another_type_than_the_binding_key_panics() {
    const BAD: BelongsToOf<Posts, Accounts, i64> =
        BelongsToOf::new("bad", posts::Column::AuthorName)
            .binding_key(accounts::Column::EmployeeNo);
    bound_registry(|r| {
        r.add(Accounts::new());
        r.add(Posts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "posts.reviewer_no: association value type string does not match bindingKey column accounts.employee_no of type big_integer"
)]
fn belongs_to_value_type_that_does_not_match_the_binding_key_panics() {
    const BAD: BelongsToOf<Posts, Accounts, String> =
        BelongsToOf::new("bad", posts::Column::ReviewerNo)
            .binding_key(accounts::Column::EmployeeNo);
    bound_registry(|r| {
        r.add(Accounts::new());
        r.add(Posts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "bindingKey column accounts.is_admin has type boolean, which is not usable as a key"
)]
fn binding_key_on_a_boolean_column_panics() {
    const BAD: BelongsToOf<Posts, Accounts, i64> =
        BelongsToOf::new("bad", posts::Column::ReviewerNo).binding_key(accounts::Column::IsAdmin);
    bound_registry(|r| {
        r.add(Accounts::new());
        r.add(Posts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "bindingKey column accounts.joined_at has type datetime, which is not usable as a key"
)]
fn binding_key_on_a_datetime_column_panics() {
    const BAD: BelongsToOf<Posts, Accounts, i64> =
        BelongsToOf::new("bad", posts::Column::ReviewerNo).binding_key(accounts::Column::JoinedAt);
    bound_registry(|r| {
        r.add(Accounts::new());
        r.add(Posts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "posts.author_name: foreign key column type string does not match bindingKey column accounts.employee_no of type big_integer"
)]
fn has_many_foreign_key_of_another_type_than_the_binding_key_panics() {
    const BAD: HasManyOf<Accounts, Posts, i64> =
        HasManyOf::new("bad", posts::Column::AuthorName).binding_key(accounts::Column::EmployeeNo);
    bound_registry(|r| {
        r.add(Accounts::new()).association(BAD);
        r.add(Posts::new());
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "posts.author_name: association value type big_integer does not match bindingKey column accounts.username of type string"
)]
fn has_many_value_type_that_does_not_match_the_binding_key_panics() {
    const BAD: HasManyOf<Accounts, Posts, i64> =
        HasManyOf::new("bad", posts::Column::AuthorName).binding_key(accounts::Column::Username);
    bound_registry(|r| {
        r.add(Accounts::new()).association(BAD);
        r.add(Posts::new());
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "account_groups.username: foreign key column type string does not match bindingKey column accounts.employee_no of type big_integer"
)]
fn many_to_many_join_column_of_another_type_than_the_binding_key_panics() {
    const BAD: ManyToManyOf<Accounts, AccountGroups, Groups, i64, String> = ManyToManyOf::new(
        "bad",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    )
    .binding_key(accounts::Column::EmployeeNo)
    .target_binding_key(groups::Column::Code);
    bound_registry(|r| {
        r.add(Accounts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "account_groups.group_code: foreign key column type string does not match bindingKey column groups.id of type big_integer"
)]
fn many_to_many_target_join_column_of_another_type_than_the_binding_key_panics() {
    const BAD: ManyToManyOf<Accounts, AccountGroups, Groups, String, i64> = ManyToManyOf::new(
        "bad",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    )
    .binding_key(accounts::Column::Username)
    .target_binding_key(groups::Column::Id);
    bound_registry(|r| {
        r.add(Accounts::new()).association(BAD);
    })
    .verify();
}

#[test]
#[should_panic(
    expected = "account_groups.group_code: association value type big_integer does not match bindingKey column groups.code of type string"
)]
fn many_to_many_target_value_type_that_does_not_match_panics() {
    const BAD: ManyToManyOf<Accounts, AccountGroups, Groups, String, i64> = ManyToManyOf::new(
        "bad",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    )
    .binding_key(accounts::Column::Username)
    .target_binding_key(groups::Column::Code);
    bound_registry(|r| {
        r.add(Accounts::new()).association(BAD);
    })
    .verify();
}

// ---- RepoConfig settings ----

#[derive(Debug, PartialEq)]
struct Tag(&'static str);

#[derive(Debug, PartialEq)]
struct Other(i32);

#[test]
fn extension_is_read_by_table_name_and_by_meta() {
    let mut r = common::registry();
    r.add(Products::new())
        .extension(Tag("first"))
        .extension(Other(1))
        .extension(Tag("second"));

    assert_eq!(r.extension::<Tag>("products"), Some(&Tag("second")));
    assert_eq!(r.extension::<Other>("products"), Some(&Other(1)));
    let meta = r.meta_of::<products::Entity>().unwrap();
    assert_eq!(meta.extension::<Tag>(), Some(&Tag("second")));
    assert_eq!(r.extension::<Tag>("lines"), None);
}

#[test]
fn api_is_true_by_default_and_api_false_hides_the_entity() {
    let mut r = common::registry();
    r.add(Products::new());
    r.add(Lines::new()).api(false);
    assert!(r.meta_of::<products::Entity>().unwrap().api);
    assert!(!r.meta_of::<lines::Entity>().unwrap().api);
}

#[test]
fn unique_ignores_empty_and_repeated_groups_and_survives_table_spec() {
    let mut r = common::registry();
    r.add(Products::new())
        .unique([])
        .unique([products::Column::Name, products::Column::No])
        .unique([products::Column::Name, products::Column::No]);
    assert_eq!(
        r.meta_of::<products::Entity>().unwrap().spec.uniques,
        [vec!["name", "no"]]
    );

    r.add(Lines::new())
        .unique([lines::Column::Name])
        .table_spec(|mut spec| {
            spec.uniques = Vec::new();
            spec
        });
    assert_eq!(
        r.meta_of::<lines::Entity>().unwrap().spec.uniques,
        [vec!["name"]]
    );
}

#[test]
fn an_empty_unique_adds_nothing() {
    let mut r = common::registry();
    r.add(Products::new()).unique([]);
    assert!(
        r.meta_of::<products::Entity>()
            .unwrap()
            .spec
            .uniques
            .is_empty()
    );
}

#[test]
fn association_labels_are_read_from_the_declaration() {
    const POSTS_LABELLED: HasManyOf<Accounts, Posts> =
        HasManyOf::new("posts", posts::Column::AccountId).label("投稿");
    const LATEST: HasOneOf<Accounts, Posts> =
        HasOneOf::new("latest", posts::Column::AccountId).label("最新の投稿");
    const GROUPS_LABELLED: ManyToManyOf<Accounts, AccountGroups, Groups> = ManyToManyOf::new(
        "groups",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    )
    .label("所属");
    const PLAIN_POSTS: HasManyOf<Accounts, Posts> =
        HasManyOf::new("plain_posts", posts::Column::AccountId);
    const PLAIN_LATEST: HasOneOf<Accounts, Posts> =
        HasOneOf::new("plain_latest", posts::Column::AccountId);
    const PLAIN_GROUPS: ManyToManyOf<Accounts, AccountGroups, Groups> = ManyToManyOf::new(
        "plain_groups",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    );

    let mut r = common::registry();
    r.add(Accounts::new())
        .association(POSTS_LABELLED)
        .association(LATEST)
        .association(GROUPS_LABELLED)
        .association(PLAIN_POSTS)
        .association(PLAIN_LATEST)
        .association(PLAIN_GROUPS);
    r.add(Posts::new()).association(AUTHOR);

    let label_of = |entity: &[std::sync::Arc<dyn Association>], name: &str| {
        let a = entity.iter().find(|a| a.name() == name).unwrap();
        assert_eq!(a.label(), a.info().label);
        a.label()
    };
    let accounts = r.relations_of::<accounts::Entity>().unwrap();
    assert_eq!(label_of(accounts, "posts"), Some("投稿"));
    assert_eq!(label_of(accounts, "latest"), Some("最新の投稿"));
    assert_eq!(label_of(accounts, "groups"), Some("所属"));
    assert_eq!(label_of(accounts, "plain_posts"), None);
    assert_eq!(label_of(accounts, "plain_latest"), None);
    assert_eq!(label_of(accounts, "plain_groups"), None);
    // belongsTo has no label of its own
    assert_eq!(
        label_of(r.relations_of::<posts::Entity>().unwrap(), "author"),
        None
    );
}

// ---- default_contain of later levels ----

fn nested(path: &'static str) -> Registry {
    let mut r = common::registry();
    r.add(Accounts::new()).association(POSTS);
    r.add(Posts::new())
        .association(AUTHOR)
        .default_contain([path]);
    r
}

#[test]
fn default_contain_accepts_three_levels() {
    nested("author.posts.author").verify();
}

#[test]
#[should_panic(
    expected = "posts: default_contain author.posts.nope: nope is not registered on posts"
)]
fn default_contain_checks_the_third_level_against_the_second_target() {
    nested("author.posts.nope").verify();
}

/// A custom association whose target is never registered and whose `verify` does not check it.
#[derive(Debug)]
struct Ghost;

#[async_trait]
impl Association for Ghost {
    fn name(&self) -> &'static str {
        "ghost"
    }

    fn target(&self) -> &'static str {
        "nowhere"
    }

    fn info(&self) -> RelationInfo {
        RelationInfo {
            name: "ghost",
            kind: RelationKind::Custom("ghost"),
            target: "nowhere",
            through: None,
            foreign_key: "id",
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
        }
    }

    fn cardinality(&self) -> Cardinality {
        Cardinality::One
    }

    fn verify(&self, _owner: &cakedome::registry::EntityMeta, _registry: &Registry) {}

    async fn usable(&self, _ctx: &Context) -> Result<bool, RepoError> {
        Ok(true)
    }

    fn association_type(&self) -> AssociationType {
        AssociationType::ManyToOne
    }

    async fn matching_condition(
        &self,
        _ctx: &Context,
        target: Condition,
    ) -> Result<Condition, RepoError> {
        Ok(target)
    }

    fn typed_contain(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn default_contain_does_not_check_beyond_an_unregistered_target() {
    let mut r = common::registry();
    r.add(Posts::new())
        .add_association(Ghost)
        .default_contain(["ghost.anything"]);
    r.verify();
}

// ---- layer ----

#[test]
fn json_to_value_converts_to_the_column_type() {
    let fields = common::fields::<lines::Entity>();
    let field = |name: &str| fields.iter().find(|f| f.name == name).unwrap();
    let messages = Messages::default();

    assert_eq!(
        layer::json_to_value(field("quantity"), &json!(5), &messages).unwrap(),
        sea_orm::Value::from(5_i32)
    );
    assert_eq!(
        layer::json_to_value(field("name"), &json!("品目"), &messages).unwrap(),
        sea_orm::Value::from("品目".to_string())
    );
    assert_eq!(
        layer::json_to_value(field("note"), &Json::Null, &messages).unwrap(),
        sea_orm::Value::String(None)
    );
}

#[test]
fn json_to_value_reports_code_message_and_values() {
    let fields = common::fields::<lines::Entity>();
    let field = |name: &str| fields.iter().find(|f| f.name == name).unwrap();
    let messages = Messages::default();

    let (code, text, values) =
        layer::json_to_value(field("quantity"), &json!("x"), &messages).unwrap_err();
    assert_eq!(code, "type");
    assert_eq!(text, "The value has an invalid format");
    assert!(values.is_empty());

    let (code, _, _) = layer::json_to_value(field("name"), &Json::Null, &messages).unwrap_err();
    assert_eq!(code, "required");

    let long = "あ".repeat(51);
    let (code, text, values) =
        layer::json_to_value(field("name"), &json!(long), &messages).unwrap_err();
    assert_eq!(code, "max_length");
    assert_eq!(text, "Must be at most 50 characters");
    assert_eq!(values["max"], json!(50));
    assert_eq!(values["unit"], json!("chars"));

    let custom = Messages::new(&[("type", "bad value")]);
    let (_, text, _) = layer::json_to_value(field("quantity"), &json!("x"), &custom).unwrap_err();
    assert_eq!(text, "bad value");
}

#[test]
fn active_filter_is_some_only_for_an_activatable_table() {
    let filter =
        layer::active_filter::<master_kinds::Entity>(&common::spec::<master_kinds::Entity>());
    assert_eq!(filter, Some(master_kinds::Column::IsActive.eq(true)));
    assert!(layer::active_filter::<notes::Entity>(&common::spec::<notes::Entity>()).is_none());
    assert!(layer::active_filter::<lines::Entity>(&common::spec::<lines::Entity>()).is_none());
}

struct Fixed(&'static str);

#[async_trait]
impl DynamicDefault for Fixed {
    async fn value(&self, _ctx: &Context) -> Result<Json, RepoError> {
        Ok(json!(self.0))
    }
}

#[tokio::test]
async fn dynamic_defaults_are_returned_in_registration_order() {
    let mut r = common::registry();
    r.add(Lines::new())
        .default_from(lines::Column::Name, Fixed("first"))
        .default_from(lines::Column::Note, Fixed("note"))
        .default_from(lines::Column::Name, Fixed("second"));

    let defaults = layer::dynamic_defaults::<lines::Entity>(&r);
    let columns: Vec<_> = defaults.iter().map(|(c, _)| *c).collect();
    assert_eq!(columns, ["note", "name"]);
    let ctx = common::context(BasicIdentity::user(1, None, PermissionSet::new(["*"])));
    assert_eq!(defaults[1].1.value(&ctx).await.unwrap(), json!("second"));

    assert!(layer::dynamic_defaults::<products::Entity>(&r).is_empty());
    assert!(layer::dynamic_defaults::<lines::Entity>(&common::registry()).is_empty());
}

#[test]
fn repo_of_returns_the_arc_that_add_holds() {
    let mut r = common::registry();
    let config = r.add(Products::new());
    let from_config = layer::repo_of::<Products>(&config);
    let registered = r.repo::<Products>().unwrap();
    assert!(std::sync::Arc::ptr_eq(&from_config, &registered));
}

// ---- declarations added with add_association ----

mod coded_targets {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "coded_targets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub key: i64,
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

macro_rules! other_repo {
    ($name:ident, $entity:ty) => {
        struct $name;
        impl cakedome::repo::Repo for $name {
            type Entity = $entity;
        }
        impl cakedome::policy::TablePolicy for $name {}
        impl cakedome::policy::EntityPolicy for $name {}
    };
}

other_repo!(OtherPosts, posts::Entity);
other_repo!(OtherAccounts, accounts::Entity);
other_repo!(OtherAccountGroups, account_groups::Entity);

#[test]
fn a_matching_declaration_added_with_add_association_passes_verify() {
    const AUTHOR_OK: BelongsToOf<Posts, Accounts> =
        BelongsToOf::new("author_ok", posts::Column::AccountId);
    const GROUPS_OK: ManyToManyOf<Accounts, AccountGroups, Groups, String, String> =
        ManyToManyOf::new(
            "groups_ok",
            account_groups::Column::Username,
            account_groups::Column::GroupCode,
        )
        .binding_key(accounts::Column::Username)
        .target_binding_key(groups::Column::Code);
    let mut r = bound_registry(|_| {});
    r.add(Accounts::new()).add_association(GROUPS_OK);
    r.add(Posts::new()).add_association(AUTHOR_OK);
    r.verify();
}

#[test]
#[should_panic(expected = "own type")]
fn belongs_to_with_another_own_type_added_with_add_association_panics() {
    const BAD: BelongsToOf<OtherPosts, Accounts> =
        BelongsToOf::new("bad", posts::Column::AccountId);
    let mut r = bound_registry(|_| {});
    r.add(Accounts::new());
    r.add(Posts::new()).add_association(BAD);
    r.verify();
}

#[test]
#[should_panic(expected = "target type")]
fn belongs_to_with_another_target_type_added_with_add_association_panics() {
    const BAD: BelongsToOf<Posts, OtherAccounts> =
        BelongsToOf::new("bad", posts::Column::AccountId);
    let mut r = bound_registry(|_| {});
    r.add(Accounts::new());
    r.add(Posts::new()).add_association(BAD);
    r.verify();
}

#[test]
#[should_panic(expected = "join table type")]
fn many_to_many_with_another_join_table_type_added_with_add_association_panics() {
    const BAD: ManyToManyOf<Accounts, OtherAccountGroups, Groups> = ManyToManyOf::new(
        "bad",
        account_groups::Column::Username,
        account_groups::Column::GroupCode,
    );
    let mut r = bound_registry(|_| {});
    r.add(Accounts::new()).add_association(BAD);
    r.verify();
}

#[test]
#[should_panic(expected = "must have a single id column")]
fn a_target_whose_primary_key_is_not_id_added_with_add_association_panics() {
    const BAD: BelongsToOf<DefaultRepo<coded_refs::Entity>, DefaultRepo<coded_targets::Entity>> =
        BelongsToOf::new("bad", coded_refs::Column::TargetKey);
    let mut r = common::registry();
    r.add(DefaultRepo::<coded_refs::Entity>::new())
        .add_association(BAD);
    r.add(DefaultRepo::<coded_targets::Entity>::new());
    r.verify();
}
