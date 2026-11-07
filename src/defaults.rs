//! Defaults computed per request from the `Context`, bound to columns with
//! `RepoConfig::default_from`.

use std::future::Future;
use std::pin::Pin;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{EntityTrait, IdenStatic};
use serde_json::Value;

use crate::context::Context;
use crate::json::{JsonFor, lookup_value};
use crate::key::RowId;
use crate::registry::Registry;
use crate::repo::{GuardedRepo, RepoEntity, RepoError};

/// A default computed per request.
///
/// It fills the initial value of a new row and fields omitted on an add. Precedence is dynamic,
/// then fixed (`Repo::initialize`), then rules. An `Err` is returned to the caller.
///
/// ```text
/// struct NameFromUser;
///
/// #[async_trait]
/// impl DynamicDefault for NameFromUser {
///     async fn value(&self, ctx: &Context) -> Result<Value, RepoError> {
///         let id = ctx.identity().user_id();
///         Ok(json!(id.map_or("anonymous".to_string(), |id| format!("user-{id}"))))
///     }
/// }
/// ```
#[async_trait]
pub trait DynamicDefault: Send + Sync {
    /// Computes the default value as JSON.
    async fn value(&self, ctx: &Context) -> Result<Value, RepoError>;

    /// Called from `Registry::verify` after registration; panic on a misconfiguration (default:
    /// nothing). `owner` is the entity with the default and `column` its column.
    fn verify(&self, _registry: &Registry, _owner: &str, _column: &str) {}
}

/// Uses the id of the row in another Repo whose column equals a value, such as the draft status.
///
/// The lookup goes through the target's `JsonFor::lookup_id`, so scope and soft delete apply and
/// only active rows match. It is null if the identity cannot view, the row is missing or the
/// target is unregistered. `Registry::verify` panics if the target is registered with a different
/// Repo type or the value does not fit the column type (skipped if unregistered).
pub struct RefBy {
    entity: std::any::TypeId,
    column: String,
    value: Value,
    lookup: LookupFn,
    is_registered_repo: fn(&Registry) -> bool,
}

type LookupFn =
    for<'a> fn(
        &'a Context,
        &'a str,
        &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Option<RowId>, RepoError>> + Send + 'a>>;

fn lookup_in<'a, T: GuardedRepo>(
    ctx: &'a Context,
    column: &'a str,
    value: &'a Value,
) -> Pin<Box<dyn Future<Output = Result<Option<RowId>, RepoError>> + Send + 'a>>
where
    T::Entity: RepoEntity,
{
    Box::pin(async move {
        match JsonFor::<T>::of(ctx) {
            Ok(json) => json.lookup_id(ctx, column, value).await,
            Err(RepoError::NotRegistered(_)) => Ok(None),
            Err(e) => Err(e),
        }
    })
}

impl RefBy {
    /// Creates a default that looks up the row of Repo `T` whose `column` equals `value`.
    pub fn new<T: GuardedRepo>(
        column: <T::Entity as EntityTrait>::Column,
        value: impl Into<Value>,
    ) -> Self {
        Self {
            entity: std::any::TypeId::of::<T::Entity>(),
            column: column.as_str().to_string(),
            value: value.into(),
            lookup: lookup_in::<T>,
            is_registered_repo: |registry| registry.repo::<T>().is_some(),
        }
    }
}

#[async_trait]
impl DynamicDefault for RefBy {
    async fn value(&self, ctx: &Context) -> Result<Value, RepoError> {
        match (self.lookup)(ctx, &self.column, &self.value).await? {
            Some(id) => Ok(id.to_json()),
            None => Ok(Value::Null),
        }
    }

    fn verify(&self, registry: &Registry, owner: &str, column: &str) {
        let Some(target) = registry.meta_of_entity(self.entity) else {
            return;
        };
        if !(self.is_registered_repo)(registry) {
            panic!(
                "{owner}: {column} default RefBy target {} is registered with a Repo of a different type",
                target.name
            );
        }
        let fits = target
            .fields
            .iter()
            .find(|f| f.name == self.column)
            .is_some_and(|f| lookup_value(&f.kind, &self.value).is_some());
        if !fits {
            panic!(
                "{owner}: {column} default RefBy value {} does not match the type of {}.{}",
                self.value, target.name, self.column
            );
        }
    }
}

/// Uses the logged-in user's id; null when anonymous.
pub struct CurrentUserId;

#[async_trait]
impl DynamicDefault for CurrentUserId {
    async fn value(&self, ctx: &Context) -> Result<Value, RepoError> {
        Ok(ctx
            .identity()
            .user_id()
            .map_or(Value::Null, |id| id.to_json()))
    }
}
