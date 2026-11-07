//! `Repo`, the trait that bundles an Entity's configuration, Policy and validation, and
//! `RepoHandle`, which applies the Policy to reads and writes.
//!
//! Supported Entities have a single-column primary key of type `i64`, `Uuid` or `String`.

mod before_find;
pub(crate) mod contain;
mod find;
mod guard;
mod handle;
mod link;
mod loaded;
mod save_with;

pub use before_find::{FindOptions, FindQuery};
pub use contain::{
    Containable, FindWith, MAX_CONTAIN_DEPTH, MAX_CONTAIN_PATHS, With, typed_contain,
};
pub(crate) use contain::{NameTree, load_related};
pub(crate) use contain::{TypedDecl, typed_decl_of};
pub use find::Find;
pub use handle::RepoHandle;
pub use link::Linkable;
pub(crate) use loaded::JoinJson;
pub use loaded::{Is, Loaded, NotLoaded, Related};
pub use save_with::{Nested, SaveWith};

use std::marker::PhantomData;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{DbErr, EntityName, EntityTrait, IntoActiveModel, PrimaryKeyTrait};

use crate::context::Context;
use crate::key::{Key, RowKey};
use crate::marshal::{FieldErrors, MarshalOptions, WriteMode};
use crate::policy::{EntityPolicy, TablePolicy};
use crate::registry::RepoConfig;
use crate::validate::{ValidationInput, Warning};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepoError {
    #[error("not authenticated")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("entity is not registered: {0}")]
    NotRegistered(String),
    #[error("column is managed by the repo and cannot be set: {0}")]
    ManagedColumn(String),
    #[error("entity cannot be deleted (deactivate it instead)")]
    NotDeletable,
    /// Second-stage input validation (`Repo::validate`) failed.
    #[error("invalid input")]
    Validation(FieldErrors),
    /// There are Warnings the caller has not accepted.
    #[error("there are warnings to accept")]
    Warnings(Vec<Warning>),
    /// `modified` changed since the row was read (`save_if_unmodified`).
    #[error("conflict with another update")]
    Conflict,
    /// A unique constraint was violated, e.g. by a concurrent insert.
    #[error("value is already in use")]
    Duplicate,
    #[error(transparent)]
    Db(DbErr),
    /// `before_find` read the same Entity again.
    #[error("before_find read the same entity again: {0}")]
    BeforeFindReentered(String),
}

impl From<DbErr> for RepoError {
    fn from(e: DbErr) -> Self {
        if matches!(
            e.sql_err(),
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
        ) {
            RepoError::Duplicate
        } else {
            RepoError::Db(e)
        }
    }
}

/// An Entity a Repo can handle: a single-column primary key of type `RowKey`, and a Model and
/// ActiveModel that are `Send + Sync`.
pub trait RepoEntity:
    EntityTrait<
        PrimaryKey: PrimaryKeyTrait<ValueType: RowKey>,
        Model: Send + Sync + IntoActiveModel<<Self as EntityTrait>::ActiveModel>,
        ActiveModel: Send + Sync,
    >
{
}

impl<E> RepoEntity for E
where
    E: EntityTrait,
    E::PrimaryKey: PrimaryKeyTrait<ValueType: RowKey>,
    E::Model: Send + Sync + IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: Send + Sync,
{
}

type ModelOf<E> = <E as EntityTrait>::Model;
type ActiveModelOf<E> = <E as EntityTrait>::ActiveModel;
type ColumnOf<E> = <E as EntityTrait>::Column;

/// Per-Entity configuration, validation and hooks.
///
/// The Policy is two other traits, `TablePolicy` and `EntityPolicy`, implemented on the same type
/// (`impl TablePolicy for T {}` and `impl EntityPolicy for T {}` keep the defaults). Register the
/// Repo with `Registry::add`. Values that exist only at startup go in the Repo's fields.
///
/// `get`, `find`, `save` and `delete` are methods of the `RepoHandle` returned by `ctx.repo::<R>()`
/// and cannot be overridden, so the Policy cannot be bypassed; use the Policy, `validate` and the
/// hooks instead. `BasicIdentity::system()` passes every `can(..)` and has no `user_id()`, so
/// decisions on `user_id()` must handle `None`.
///
/// The hooks are async, so the impl needs `#[async_trait]` (`sea_orm::prelude::async_trait`).
///
/// ```ignore
/// #[async_trait]
/// impl Repo for OrdersRepo {
///     type Entity = orders::Entity;
///
///     async fn after_save(&self, ctx: &Context, model: &orders::Model, mode: WriteMode,
///                         before: Option<&orders::Model>) -> Result<(), RepoError> {
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Repo: Send + Sync + 'static {
    type Entity: RepoEntity;

    /// Returns the name used by the Registry, schema, permission strings and association targets.
    /// Defaults to the table name, which SQL uses on its own.
    fn repo_name() -> &'static str
    where
        Self: Sized,
    {
        <Self::Entity as Default>::default().table_name()
    }

    /// Configures labels, defaults and associations. Called once, at `Registry::add`.
    fn initialize(t: RepoConfig<'_, Self::Entity>) -> RepoConfig<'_, Self::Entity>
    where
        Self: Sized,
    {
        t
    }

    /// Adds conditions and ordering before rows are read. Does nothing by default.
    ///
    /// Called by `Find::all`, `count` and `subquery`, `RepoHandle::get`, and before the current row
    /// is read for an update, touch or delete (`query.action()` is `Edit` or `Delete`), also when
    /// `unchecked`. Conditions are ANDed and cannot remove the scope, soft delete or caller
    /// conditions; ordering is appended after the caller's and used by `all` only. It cannot change
    /// columns or joins.
    ///
    /// `primary` is false when the ORM itself reads an association target (contain, filters across
    /// associations, belongsTo checks, current children on save, RefBy defaults). `options` are the
    /// values passed with `RepoHandle::option` or `Find::option`.
    ///
    /// Not applied to unique checks, duplicate Warnings, dependent child deletes or `read_where`.
    /// Reading another Entity through `ctx.repo` is allowed; reading the same one fails with
    /// `BeforeFindReentered`. It runs once per query, so keep it cheap.
    async fn before_find(
        &self,
        _ctx: &Context,
        _query: &mut FindQuery<Self::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Rewrites the request data before it becomes an ActiveModel. Does nothing by default.
    ///
    /// Called only by layers that save from request data (such as a JSON API), not by
    /// `RepoHandle::save`. `data` still holds association keys. For hasMany children the child
    /// Repo's hook runs per row (`options.association()` is the association name); never for
    /// many-to-many. Order: permission check, `before_marshal`, first-stage validation,
    /// `after_marshal`, `can_add`/`can_edit`, `validate`, unique check, `before_save`.
    async fn before_marshal(
        &self,
        _ctx: &Context,
        _data: &mut serde_json::Map<String, serde_json::Value>,
        _options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Fixes the ActiveModel or adds errors after first-stage validation. Does nothing by default.
    ///
    /// Called only by layers that save from request data, and not for rows whose ActiveModel could
    /// not be built. `data` is read-only. Children run before the parent; never for many-to-many.
    /// Errors added to `errors` stop the save (child keys are `association.row.field`).
    async fn after_marshal(
        &self,
        _ctx: &Context,
        _am: &mut ActiveModelOf<Self::Entity>,
        _data: &serde_json::Map<String, serde_json::Value>,
        _errors: &mut FieldErrors,
        _options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Second-stage input validation (business rules). Does nothing by default.
    ///
    /// `RepoHandle::save` and `save_if_unmodified` call it after `can_add`/`can_edit` and before
    /// writing, also when `unchecked`. Add field errors with `errors.add(field, code, message)`;
    /// any error makes the save fail with `RepoError::Validation`.
    fn validate(&self, _input: &ValidationInput<'_, Self::Entity>, _errors: &mut FieldErrors) {}

    /// Returns Warnings: things that can be saved but a person should confirm. Empty by default.
    ///
    /// Called only by layers that save from request data, after `warn_duplicate` and only when
    /// there are no validation errors. Rules that must hold belong in `validate`.
    async fn warnings(
        &self,
        _ctx: &Context,
        _input: &ValidationInput<'_, Self::Entity>,
    ) -> Result<Vec<Warning>, RepoError> {
        Ok(Vec::new())
    }

    /// Returns the primary key of a new row. Defaults to the type's default: `None` for `i64`
    /// (AUTO_INCREMENT), a v7 UUID for `Uuid`, `None` for `String` (the input must supply it).
    fn new_key(&self) -> Option<Key<Self>>
    where
        Self: Sized,
    {
        <Key<Self> as RowKey>::generate()
    }

    /// Runs just before a save. Does nothing by default.
    ///
    /// Insert: after `can_add` and `validate`, before `created` is set. Update: after `validate`
    /// (not called when no column is written), before `modified` is set. Values added to `am` skip
    /// `can_write` and `validate`, and managed columns (`created`, `modified`, `is_active`) are set
    /// afterwards and overwrite them. `Err` aborts the save. Called also when `unchecked`; writes
    /// join the surrounding transaction.
    ///
    /// `before` is the row before the save (`None` on insert), without a re-read.
    async fn before_save(
        &self,
        _ctx: &Context,
        _am: &mut ActiveModelOf<Self::Entity>,
        _mode: WriteMode,
        _before: Option<&ModelOf<Self::Entity>>,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs before the rules stage. Does nothing by default.
    ///
    /// Called on every save, also when `unchecked`, after `can_add`/`can_edit`. Order:
    /// `before_rules`, rules (`validate`, references, unique), `after_rules`, errors, Warnings,
    /// `before_save`. Values added here skip `can_write`. `Err` aborts the save. On updates it is
    /// called even when no column is written, wherever the rules stage runs.
    async fn before_rules(
        &self,
        _ctx: &Context,
        _am: &mut ActiveModelOf<Self::Entity>,
        _mode: WriteMode,
        _before: Option<&ModelOf<Self::Entity>>,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs after the rules stage. Does nothing by default.
    ///
    /// `errors` holds the result of the rules stage and may be changed; if it is not empty
    /// afterwards the save fails with `Validation`. `Err` aborts the save.
    async fn after_rules(
        &self,
        _ctx: &Context,
        _am: &ActiveModelOf<Self::Entity>,
        _mode: WriteMode,
        _before: Option<&ModelOf<Self::Entity>>,
        _errors: &mut FieldErrors,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs right after an insert or update, before the Registry observers. Does nothing by
    /// default.
    ///
    /// `model` is the saved row; `before` is the row before the save (`None` on insert). `Err` is
    /// returned, and inside a transaction the caller rolls the save back. Called also when
    /// `unchecked`.
    async fn after_save(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
        _mode: WriteMode,
        _before: Option<&ModelOf<Self::Entity>>,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs after the outermost transaction commits. Does nothing by default.
    ///
    /// Called in save order; for a save outside a transaction, right after `after_save`. Not called
    /// for rolled-back saves (a nested commit waits for the outer one). `ctx` is the Context
    /// outside the transaction, so the committed row is readable. The save is already committed, so
    /// `Err` is logged and the remaining hooks still run. Called also when `unchecked`.
    async fn after_save_commit(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
        _mode: WriteMode,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs after a delete commits. Does nothing by default.
    ///
    /// Timing, `ctx` and `Err` follow `after_save_commit`. `model` is the row read before the
    /// delete.
    async fn after_delete_commit(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs just before a delete (soft or hard), after `can_delete`. Does nothing by default.
    ///
    /// `model` is the row read before the delete. `Err` aborts the delete. Called also when
    /// `unchecked`.
    async fn before_delete(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Runs right after a delete, before the Registry observers. Does nothing by default.
    ///
    /// `model` is the row read before the delete (also for soft delete). `Err` is returned, and
    /// inside a transaction the delete is rolled back. Called also when `unchecked`.
    async fn after_delete(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
    ) -> Result<(), RepoError> {
        Ok(())
    }
}

/// The Repo type the ORM requires for reads and writes: a `Repo` that also implements both Policy
/// traits. It is a blanket alias, never implemented by hand; a missing Policy impl fails here.
pub trait GuardedRepo: Repo + TablePolicy + EntityPolicy {}

impl<R: Repo + TablePolicy + EntityPolicy> GuardedRepo for R {}

/// A Repo with default configuration and Policy, for tests and Entities that need no setup.
pub struct DefaultRepo<E>(PhantomData<fn() -> E>);

impl<E> DefaultRepo<E> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<E> Default for DefaultRepo<E> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<E: RepoEntity> Repo for DefaultRepo<E> {
    type Entity = E;
}

impl<E: RepoEntity> TablePolicy for DefaultRepo<E> {}

impl<E: RepoEntity> EntityPolicy for DefaultRepo<E> {}
