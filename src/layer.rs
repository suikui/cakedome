//! Building blocks for layers on top of the ORM (such as a JSON API).
//! Unstable in 0.x; application code should use `RepoHandle` and the like.

use std::sync::Arc;

use sea_orm::sea_query::SimpleExpr;
use sea_orm::{EntityTrait, Value};
use serde_json::{Map, Value as Json};

use crate::defaults::DynamicDefault;
use crate::messages::Messages;
use crate::meta::{FieldMeta, TableSpec};
use crate::registry::{Registry, RepoConfig};
use crate::repo::GuardedRepo;

pub use crate::association::reference::selectable;
pub use crate::association::util::{primary_column, read_in, row_key, target_id_from_json};
pub use crate::json::{
    ContainNode, ContainTree, JOIN_DATA_KEY, JsonFor, LABEL_KEY, Readability, default_contain_tree,
    trace,
};
pub use crate::policy::columns::{table_readable, table_writable};
pub use crate::repo::contain::{NameTree, load_related};

/// Converts a JSON value to a value of the column's type.
///
/// The error is (error code, message, values used in the message).
pub fn json_to_value(
    f: &FieldMeta,
    value: &Json,
    messages: &Messages,
) -> Result<Value, (&'static str, String, Map<String, Json>)> {
    crate::marshal::json_to_value(f, value, messages)
}

/// Returns the condition that the active flag is TRUE for an Activatable table, else `None`.
///
/// Used to leave inactive rows out of choices and reference targets.
pub fn active_filter<E: EntityTrait>(spec: &TableSpec) -> Option<SimpleExpr> {
    crate::meta::active_filter::<E>(spec)
}

/// Returns the dynamic defaults (`default_from`) in registration order; empty if unregistered.
pub fn dynamic_defaults<E: EntityTrait>(
    registry: &Registry,
) -> Vec<(&'static str, Arc<dyn DynamicDefault>)> {
    registry.dynamic_defaults::<E>()
}

/// Returns the registered Repo `R` (the same `Arc` that `add` holds).
pub fn repo_of<R: GuardedRepo>(config: &RepoConfig<'_, R::Entity>) -> Arc<R> {
    config.repo_of::<R>()
}
