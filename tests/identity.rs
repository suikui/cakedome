//! The Identity trait: callers implement it on their own user type. No DB.

mod common;

use std::sync::Arc;

use cakedome::Context;
use cakedome::DefaultRepo;
use cakedome::defaults::{CurrentUserId, DynamicDefault};
use cakedome::key::RowId;
use cakedome::policy::{
    Action, AuthzMark, BasicIdentity, EntityPolicy, Identity, PermissionSet, TablePolicy,
};
use cakedome::registry::Registry;
use cakedome::repo::{Repo, RepoError};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, DbBackend, EntityTrait, QueryFilter, QueryTrait,
};
use serde_json::json;
use uuid::Uuid;

#[derive(Debug)]
struct AppUser {
    id: Uuid,
    department: String,
    granted: Vec<&'static str>,
}

impl Identity for AppUser {
    fn user_id(&self) -> Option<RowId> {
        Some(RowId::Uuid(self.id))
    }

    fn has_permission(&self, permission: &str) -> bool {
        self.granted.contains(&permission)
    }
}

/// Allows every permission but is not logged in (checks that cakedome's decision comes first).
#[derive(Debug)]
struct Guest;

impl Identity for Guest {
    fn user_id(&self) -> Option<RowId> {
        None
    }

    fn has_permission(&self, _permission: &str) -> bool {
        true
    }
}

/// System with no permissions (cakedome always allows the system).
#[derive(Debug)]
struct Batch;

impl Identity for Batch {
    fn user_id(&self) -> Option<RowId> {
        None
    }

    fn has_permission(&self, _permission: &str) -> bool {
        false
    }

    fn is_system(&self) -> bool {
        true
    }
}

const ID: &str = "0192f5c4-7a1b-7c3d-8e4f-0123456789ab";

fn alice() -> AppUser {
    AppUser {
        id: Uuid::parse_str(ID).unwrap(),
        department: "sales".to_string(),
        granted: vec!["entity.docs.view"],
    }
}

fn context(identity: impl Identity) -> Context {
    Context::new(
        identity,
        DatabaseConnection::default(),
        AuthzMark::default(),
        Arc::new(Registry::new(common::TestRules, &[], &[])),
    )
}

#[test]
fn user_answers_with_has_permission() {
    let user = alice();
    let identity: &dyn Identity = &user;
    assert!(identity.is_authenticated());
    assert!(identity.can("entity.docs.view"));
    assert!(!identity.can("entity.docs.edit"));
}

#[test]
fn anonymous_is_denied_regardless_of_has_permission() {
    let identity: &dyn Identity = &Guest;
    assert!(!identity.is_authenticated());
    assert!(!identity.can("entity.docs.view"));
}

#[test]
fn system_is_allowed_regardless_of_has_permission() {
    let identity: &dyn Identity = &Batch;
    assert!(identity.is_authenticated());
    assert!(identity.can("entity.docs.view"));
}

#[test]
fn downcast_ref_returns_the_application_type() {
    let user = alice();
    let identity: &dyn Identity = &user;
    assert_eq!(
        identity
            .downcast_ref::<AppUser>()
            .map(|u| u.department.as_str()),
        Some("sales")
    );
    assert!(
        identity.downcast_ref::<BasicIdentity>().is_none(),
        "None for a different type"
    );
}

#[test]
fn context_returns_the_identity_and_its_application_type() {
    let ctx = context(alice());
    assert_eq!(
        ctx.identity().user_id(),
        Some(RowId::Uuid(Uuid::parse_str(ID).unwrap()))
    );
    assert_eq!(
        ctx.identity_as::<AppUser>().map(|u| u.department.as_str()),
        Some("sales")
    );
    assert!(ctx.identity_as::<BasicIdentity>().is_none());
}

#[test]
fn context_is_built_from_a_shared_identity() {
    // A request's Identity is held as `Arc<dyn Identity>` and passed to Context as is.
    let shared: Arc<dyn Identity> = Arc::new(alice());
    let ctx = Context::new_shared(
        shared.clone(),
        DatabaseConnection::default(),
        AuthzMark::default(),
        Arc::new(Registry::new(common::TestRules, &[], &[])),
    );
    assert!(std::ptr::addr_eq(ctx.identity(), &*shared), "not cloned");
    assert!(ctx.identity_as::<AppUser>().is_some());

    let anonymous: Arc<dyn Identity> = Arc::new(BasicIdentity::anonymous());
    let ctx = Context::new_shared(
        anonymous,
        DatabaseConnection::default(),
        AuthzMark::default(),
        Arc::new(Registry::new(common::TestRules, &[], &[])),
    );
    assert!(ctx.identity_as::<AppUser>().is_none());
    assert!(!ctx.identity().is_authenticated());
}

// ---- CurrentUserId ----

#[tokio::test]
async fn current_user_id_is_the_user_id_as_json() {
    let uuid = context(alice());
    assert_eq!(CurrentUserId.value(&uuid).await.unwrap(), json!(ID));

    let int = context(BasicIdentity::user(7, None, PermissionSet::empty()));
    assert_eq!(CurrentUserId.value(&int).await.unwrap(), json!(7));

    let system = context(BasicIdentity::system());
    assert_eq!(CurrentUserId.value(&system).await.unwrap(), json!(null));
}

#[test]
fn basic_identity_keeps_its_behavior() {
    let user = BasicIdentity::user(
        7,
        Some("admin".to_string()),
        PermissionSet::new(["entity.*"]),
    );
    let identity: &dyn Identity = &user;
    assert_eq!(identity.user_id(), Some(RowId::Int(7)));
    assert!(identity.can("entity.orders.view"));
    assert!(!identity.can("field.customers.credit_limit"));
    assert_eq!(user.role.as_deref(), Some("admin"));

    let anonymous = BasicIdentity::anonymous();
    let identity: &dyn Identity = &anonymous;
    assert!(!identity.is_authenticated());
    assert!(!identity.can("entity.orders.view"));

    let system = BasicIdentity::system();
    let identity: &dyn Identity = &system;
    assert!(identity.is_system());
    assert!(identity.can("entity.orders.view"));
}

#[test]
fn basic_identity_user_id_may_be_a_string_or_uuid() {
    let s = BasicIdentity::user("u-001".to_string(), None, PermissionSet::empty());
    assert_eq!(
        (&s as &dyn Identity).user_id(),
        Some(RowId::Str("u-001".to_string()))
    );
    let u = BasicIdentity::user(Uuid::parse_str(ID).unwrap(), None, PermissionSet::empty());
    assert_eq!(
        (&u as &dyn Identity).user_id(),
        Some(RowId::Uuid(Uuid::parse_str(ID).unwrap()))
    );
}

mod docs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "docs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        #[sea_orm(column_type = "String(StringLen::N(50))")]
        pub department: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

struct DepartmentDocs;

impl Repo for DepartmentDocs {
    type Entity = docs::Entity;
}

#[async_trait]
impl TablePolicy for DepartmentDocs {
    async fn scope(&self, ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(match ctx.identity().downcast_ref::<AppUser>() {
            Some(user) => {
                Condition::all().add(docs::Column::Department.eq(user.department.as_str()))
            }
            // Not the app type (e.g. system): no narrowing.
            None => Condition::all(),
        })
    }
}

impl EntityPolicy for DepartmentDocs {}

fn sql(condition: Condition) -> String {
    docs::Entity::find()
        .filter(condition)
        .build(DbBackend::MySql)
        .to_string()
}

#[tokio::test]
async fn scope_can_use_fields_of_the_application_identity() {
    let user = context(alice());
    assert_eq!(
        sql(DepartmentDocs.scope(&user, Action::View).await.unwrap()),
        "SELECT `docs`.`id`, `docs`.`department` FROM `docs` WHERE `docs`.`department` = 'sales'"
    );
    assert_eq!(
        sql(DepartmentDocs
            .scope(&context(BasicIdentity::system()), Action::View)
            .await
            .unwrap()),
        "SELECT `docs`.`id`, `docs`.`department` FROM `docs` WHERE TRUE"
    );
    let _ = DefaultRepo::<docs::Entity>::new()
        .scope(&user, Action::View)
        .await
        .unwrap();
}
