//! Column lookup, Policy defaults, Registry, AuthzMark and clock. No DB.

mod common;

use cakedome::meta::column_named;
use cakedome::policy::{
    Action, AuthzMark, BasicIdentity, EntityPolicy, PermissionSet, TablePolicy, entity_permission,
};
use cakedome::repo::DefaultRepo;

mod widgets {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "widgets")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(100))")]
        pub name: String,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
        pub is_active: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod links {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting_links")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub widget_id: i64,
        pub created: DateTimeUtc,
        pub modified: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn identity(patterns: &[&str]) -> BasicIdentity {
    BasicIdentity::user(1, None, PermissionSet::new(patterns.iter().copied()))
}

/// Name of the found column (`Column` has no `==`, so compare by name).
fn found<E: sea_orm::EntityTrait>(name: &str) -> Option<&'static str> {
    use sea_orm::IdenStatic;
    column_named::<E>(name).map(|c| c.as_str())
}

#[test]
fn column_named_finds_common_columns() {
    assert_eq!(found::<widgets::Entity>("created"), Some("created"));
    assert_eq!(found::<widgets::Entity>("is_active"), Some("is_active"));
    assert_eq!(found::<links::Entity>("modified"), Some("modified"));
    assert_eq!(
        found::<links::Entity>("is_active"),
        None,
        "hard delete without is_active"
    );
    assert_eq!(found::<widgets::Entity>("no_such_column"), None);
}

#[test]
fn action_names_and_permission_strings() {
    assert_eq!(Action::View.as_str(), "view");
    assert_eq!(Action::Add.as_str(), "add");
    assert_eq!(Action::Edit.as_str(), "edit");
    assert_eq!(Action::Delete.as_str(), "delete");
    assert_eq!(
        entity_permission::<DefaultRepo<widgets::Entity>>(Action::View),
        "entity.widgets.view"
    );
    assert_eq!(
        entity_permission::<DefaultRepo<links::Entity>>(Action::Delete),
        "entity.setting_links.delete"
    );
}

#[tokio::test]
async fn default_table_policy_checks_entity_permission() {
    let p = DefaultRepo::<widgets::Entity>::new();
    let viewer = common::context(identity(&["entity.widgets.view"]));

    assert!(p.can(&viewer, Action::View).await.unwrap());
    assert!(!p.can(&viewer, Action::Edit).await.unwrap());
    let links_repo = DefaultRepo::<links::Entity>::new();
    assert!(!links_repo.can(&viewer, Action::View).await.unwrap());

    let all = common::context(identity(&["entity.widgets.*"]));
    for a in [Action::View, Action::Add, Action::Edit, Action::Delete] {
        assert!(p.can(&all, a).await.unwrap(), "{a:?}");
    }
    let anonymous = common::context(BasicIdentity::anonymous());
    assert!(!p.can(&anonymous, Action::View).await.unwrap());
}

#[tokio::test]
async fn default_table_policy_has_no_scope_and_allows_columns() {
    let p = DefaultRepo::<widgets::Entity>::new();
    let ctx = common::context(identity(&[]));
    let scope = p.scope(&ctx, Action::View).await.unwrap();
    assert!(scope.is_empty(), "default scope adds no condition");
    let access = p.columns(&ctx).await.unwrap();
    assert!(access.can_read(widgets::Column::Name));
    assert!(access.can_write(widgets::Column::Name));
    assert!(access.read_scope(widgets::Column::Name).is_none());
}

#[tokio::test]
async fn default_entity_policy_allows_everything() {
    use sea_orm::ActiveValue::NotSet;
    let p = DefaultRepo::<widgets::Entity>::new();
    let ctx = common::context(identity(&[]));
    let now = cakedome::clock::now();
    let model = widgets::Model {
        id: 1,
        name: "a".into(),
        created: now,
        modified: now,
        is_active: true,
    };
    let am = widgets::ActiveModel {
        id: NotSet,
        name: NotSet,
        created: NotSet,
        modified: NotSet,
        is_active: NotSet,
    };
    assert!(p.can_add(&ctx, &am).await.unwrap());
    assert!(p.can_edit(&ctx, &model, &am).await.unwrap());
    assert!(p.can_delete(&ctx, &model).await.unwrap());
}

#[test]
fn registry_knows_registered_entities_by_table_name() {
    let mut r = common::registry();
    r.add(DefaultRepo::<widgets::Entity>::new());

    assert!(r.contains::<widgets::Entity>());
    assert!(!r.contains::<links::Entity>());
    assert_eq!(r.name_of::<widgets::Entity>(), Some("widgets"));
    assert_eq!(r.name_of::<links::Entity>(), None);
}

#[test]
#[should_panic(expected = "widgets")]
fn registering_the_same_entity_twice_panics() {
    let mut r = common::registry();
    r.add(DefaultRepo::<widgets::Entity>::new());
    r.add(DefaultRepo::<widgets::Entity>::new());
}

#[test]
fn authz_mark_is_shared_between_clones() {
    let mark = AuthzMark::default();
    let copy = mark.clone();
    assert!(!mark.is_marked());

    copy.mark();

    assert!(mark.is_marked());
}

#[test]
fn clock_is_truncated_to_microseconds() {
    use chrono::Timelike;
    for _ in 0..100 {
        assert_eq!(cakedome::clock::now().nanosecond() % 1_000, 0);
    }
}

struct WidgetsRepo;

impl cakedome::repo::Repo for WidgetsRepo {
    type Entity = widgets::Entity;
}

impl cakedome::policy::TablePolicy for WidgetsRepo {}

impl cakedome::policy::EntityPolicy for WidgetsRepo {}

#[test]
#[should_panic(expected = "widgets")]
fn registering_two_repos_for_the_same_entity_panics() {
    // Registering the same Entity twice panics even with a different Repo type.
    let mut r = common::registry();
    r.add(cakedome::repo::DefaultRepo::<widgets::Entity>::new());
    r.add(WidgetsRepo);
}
