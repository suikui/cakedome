//! Traits that write validated children and parents, with the target type erased.

use sea_orm::prelude::async_trait::async_trait;

use crate::context::Context;
use crate::key::RowId;
use crate::marshal::FieldErrors;
use crate::repo::RepoError;

/// Validated hasMany child rows or many-to-many pairs, ready to be written.
#[async_trait]
pub trait PreparedChildren: Send {
    /// Parent column (`bindingKey`) whose saved value is passed to `write` as `parent_id`;
    /// `None` means the primary key.
    fn parent_key(&self) -> Option<&'static str> {
        None
    }

    /// Writes the children or pairs of the parent `parent_id`; returns whether anything changed.
    ///
    /// Second-stage validation errors are collected into `errors`; permission and DB errors are
    /// returned. `unchecked` writes through the targets' unchecked Repos.
    async fn write(
        self: Box<Self>,
        tx: &Context,
        parent_id: Option<RowId>,
        unchecked: bool,
        errors: &mut FieldErrors,
    ) -> Result<bool, RepoError>;
}

/// A validated belongsTo target row, written before its parent.
#[async_trait]
pub trait PreparedParent: Send {
    fn foreign_key(&self) -> &'static str;

    /// Writes the target row (update with an id, insert without) and returns the value for the
    /// parent foreign key: the target's primary key, or its `bindingKey` column.
    ///
    /// Second-stage validation errors are added to `errors` as `name.field` and give `Ok(None)`.
    async fn write(
        self: Box<Self>,
        ctx: &Context,
        unchecked: bool,
        errors: &mut FieldErrors,
    ) -> Result<Option<sea_orm::Value>, RepoError>;
}
