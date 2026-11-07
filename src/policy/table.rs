//! The operation kind `Action` and permission strings used by policies.

use crate::repo::Repo;

/// The kind of operation a policy decides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Listing, searching (find) and fetching one row (get).
    View,
    Add,
    Edit,
    Delete,
}

impl Action {
    /// Returns `"view"`, `"add"`, `"edit"` or `"delete"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::View => "view",
            Action::Add => "add",
            Action::Edit => "edit",
            Action::Delete => "delete",
        }
    }
}

/// Returns `entity.<repo name>.<action>`.
pub fn entity_permission<R: Repo>(action: Action) -> String {
    format!("entity.{}.{}", R::repo_name(), action.as_str())
}
