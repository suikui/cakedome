//! Observers called on every save and delete, registered with `Registry::observe`.

use sea_orm::prelude::async_trait::async_trait;
use serde_json::{Map, Value};

use crate::context::Context;
use crate::key::RowId;
use crate::repo::RepoError;

/// The kind of a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Edit,
    Delete,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Add => "add",
            ChangeKind::Edit => "edit",
            ChangeKind::Delete => "delete",
        }
    }
}

/// A change to one row. Values are `marshal::to_json` output without hidden or internal columns.
pub struct Change<'a> {
    pub table: &'a str,
    pub id: RowId,
    pub kind: ChangeKind,
    /// The row before the change; `None` for an add.
    pub before: Option<&'a Map<String, Value>>,
    /// The row after the change; `None` for a delete (also a soft delete).
    pub after: Option<&'a Map<String, Value>>,
}

/// An observer called right after a save or delete.
///
/// It runs after `Repo::after_save` / `Repo::after_delete`, in the same transaction, in
/// registration order. An `Err` is returned to the caller (rolling back the save inside a
/// transaction) and later observers are skipped. Not called for no-op updates, `touch`, or writes
/// that bypass Repo (`update_many`, raw SQL, unregistered entities). Writing through a Repo from an
/// observer triggers observers again, so write with SeaORM directly.
#[async_trait]
pub trait ChangeObserver: Send + Sync + 'static {
    /// The observer's unique name, used by `RepoConfig::skip_observer`.
    fn name(&self) -> &'static str;

    /// Receives the change of one row.
    async fn on_change(&self, ctx: &Context, change: &Change<'_>) -> Result<(), RepoError>;
}
