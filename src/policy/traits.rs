//! The Policy traits `TablePolicy` (per table) and `EntityPolicy` (per row).
//!
//! Both take a `Context` and are async, so they may read the DB. `ctx.repo::<B>()` applies the
//! current identity's Policy; use `unchecked()` to read regardless of permission. Implement them on
//! the Repo type, in a file separate from `impl Repo` if you like.
//!
//! ```text
//! #[async_trait]
//! impl TablePolicy for CustomersRepo {
//!     async fn scope(&self, ctx: &Context, action: Action) -> Result<Condition, RepoError> { .. }
//! }
//! #[async_trait]
//! impl EntityPolicy for CustomersRepo {
//!     async fn can_edit(&self, ctx: &Context, before: &Model, after: &ActiveModel)
//!         -> Result<bool, RepoError> { .. }
//! }
//! // With all defaults, write empty impls
//! impl TablePolicy for CountriesRepo {}
//! impl EntityPolicy for CountriesRepo {}
//! ```
//!
//! # Result reuse
//!
//! Within one `Context` (and the transactions begun from it), each `TablePolicy` method is called
//! once per Repo type and argument (action, association name) and the result is reused, even if
//! the permission table changes in between. `EntityPolicy` depends on the row, so it is called for
//! every row saved or deleted. Neither is called for an `unchecked()` handle.

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{Condition, EntityTrait};

use super::{Action, ColumnAccess, entity_permission};
use crate::context::Context;
use crate::repo::{Repo, RepoError};

type ModelOf<E> = <E as EntityTrait>::Model;
type ActiveModelOf<E> = <E as EntityTrait>::ActiveModel;

/// Policy per table (entity): whether an operation is allowed, which rows are visible, which
/// columns are readable and writable, and which associations are usable.
///
/// Implement with `#[async_trait]` and override only what you need; write empty impls of
/// `TablePolicy` and `EntityPolicy` to keep every default.
#[async_trait]
pub trait TablePolicy: Repo {
    /// Returns whether the action is allowed; the default checks `entity.<name>.<action>`.
    ///
    /// Called first by every `RepoHandle` operation. When false, no DB read happens and the result
    /// is `Forbidden` (403) for a logged-in identity or `Unauthenticated` (401) otherwise.
    /// Overriding replaces the default; call it yourself for actions that should keep it.
    /// Row-dependent checks belong in `scope` or `EntityPolicy`.
    ///
    /// ```text
    /// Ok(match action {
    ///     Action::Delete => false,
    ///     _ => ctx.identity().can(&entity_permission::<CustomersRepo>(action)),
    /// })
    /// ```
    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError>
    where
        Self: Sized,
    {
        Ok(ctx.identity().can(&entity_permission::<Self>(action)))
    }

    /// Returns the condition for visible and touchable rows; the default is no restriction.
    ///
    /// `RepoHandle` adds it to the WHERE clause. Rows outside it are left out of lists and counts
    /// and `get` returns `NotFound` (404). `View` covers find and get; `Edit` and `Delete` apply
    /// when the current row is fetched before the write; `Add` is not used, as the row is not in
    /// the DB yet (restrict adds with `EntityPolicy::can_add`). This is the only place to restrict
    /// visible rows, so lists, counts and gets share one SQL condition.
    ///
    /// With an anonymous identity, `user_id` is `None`, so a condition like `owner_id = NULL`
    /// matches nothing.
    ///
    /// ```text
    /// Ok(match action {
    ///     Action::View => Condition::all(),
    ///     _ => Condition::all().add(Column::OwnerId.eq(owner_id)),
    /// })
    /// ```
    async fn scope(&self, _ctx: &Context, _action: Action) -> Result<Condition, RepoError> {
        Ok(Condition::all())
    }

    /// Returns the readable and writable columns and the per-row hiding conditions; the default
    /// allows everything (`ColumnAccess::all()`).
    ///
    /// `read_where` conditions apply to output, display names, search and the `q` of choices, not
    /// to sorting (`unsortable`). Output reads the ids matching the condition once and omits the
    /// column in other rows; `id` and `modified` are always shown. Typed Models cannot hide
    /// columns, so this takes effect in JSON output and search.
    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<Self::Entity>, RepoError> {
        Ok(ColumnAccess::all())
    }

    /// Returns whether the association (a contain name) is usable; the default allows it.
    ///
    /// Override to hide an association from some viewers even when the target entity is visible. An
    /// association is usable when the target's `can(View)`, this check and reading the foreign key
    /// field (`columns`) are all true.
    async fn can_contain(&self, _ctx: &Context, _name: &str) -> Result<bool, RepoError> {
        Ok(true)
    }
}

/// Policy per row, called for each row saved or deleted (results are not reused).
///
/// Every method allows by default; false gives `Forbidden` (403). Restrict visible rows with
/// `TablePolicy::scope` instead (rows outside it are 404).
#[async_trait]
pub trait EntityPolicy: Repo {
    /// Returns whether adding is allowed; `after` is the values about to be written.
    async fn can_add(
        &self,
        _ctx: &Context,
        _after: &ActiveModelOf<Self::Entity>,
    ) -> Result<bool, RepoError> {
        Ok(true)
    }

    /// Returns whether updating is allowed; `before` is the current row and `after` the values
    /// about to be written. Compare them to treat adds and updates differently.
    async fn can_edit(
        &self,
        _ctx: &Context,
        _before: &ModelOf<Self::Entity>,
        _after: &ActiveModelOf<Self::Entity>,
    ) -> Result<bool, RepoError> {
        Ok(true)
    }

    /// Returns whether deleting is allowed; `model` is the current row.
    async fn can_delete(
        &self,
        _ctx: &Context,
        _model: &ModelOf<Self::Entity>,
    ) -> Result<bool, RepoError> {
        Ok(true)
    }
}
