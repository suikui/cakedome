//! Reuses `TablePolicy` results within one `Context`.
//!
//! Every `TablePolicy` call in cakedome goes through these functions. The key is the Repo type, the
//! method and its argument. The cache lives in `Context` and is shared with transactions begun from
//! it. No lock is held across a call, and `Err` is not cached.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use sea_orm::Condition;

use super::{Action, ColumnAccess};
use crate::context::Context;
use crate::repo::{GuardedRepo, RepoError};

#[derive(Clone, PartialEq, Eq, Hash)]
enum What {
    Can(Action),
    Scope(Action),
    Columns,
    CanContain(String),
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    repo: TypeId,
    what: What,
}

#[derive(Default)]
pub(crate) struct PolicyCache {
    map: Mutex<HashMap<CacheKey, Arc<dyn Any + Send + Sync>>>,
}

impl PolicyCache {
    fn get<T: Clone + 'static>(&self, key: &CacheKey) -> Option<T> {
        // A poisoned lock still holds usable data.
        let map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(key).and_then(|v| v.downcast_ref::<T>()).cloned()
    }

    fn put<T: Send + Sync + 'static>(&self, key: CacheKey, value: T) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, Arc::new(value));
    }
}

async fn cached<R, T, F>(ctx: &Context, what: What, compute: F) -> Result<T, RepoError>
where
    R: 'static,
    T: Clone + Send + Sync + 'static,
    F: Future<Output = Result<T, RepoError>>,
{
    let key = CacheKey {
        repo: TypeId::of::<R>(),
        what,
    };
    if let Some(hit) = ctx.policy_cache().get::<T>(&key) {
        return Ok(hit);
    }
    let value = compute.await?;
    ctx.policy_cache().put(key, value.clone());
    Ok(value)
}

pub(crate) async fn can<R: GuardedRepo>(
    ctx: &Context,
    repo: &R,
    action: Action,
) -> Result<bool, RepoError> {
    cached::<R, _, _>(ctx, What::Can(action), repo.can(ctx, action)).await
}

pub(crate) async fn scope<R: GuardedRepo>(
    ctx: &Context,
    repo: &R,
    action: Action,
) -> Result<Condition, RepoError> {
    cached::<R, _, _>(ctx, What::Scope(action), repo.scope(ctx, action)).await
}

pub(crate) async fn columns<R: GuardedRepo>(
    ctx: &Context,
    repo: &R,
) -> Result<Arc<ColumnAccess<R::Entity>>, RepoError> {
    cached::<R, _, _>(ctx, What::Columns, async {
        Ok(Arc::new(repo.columns(ctx).await?))
    })
    .await
}

pub(crate) async fn can_contain<R: GuardedRepo>(
    ctx: &Context,
    repo: &R,
    name: &str,
) -> Result<bool, RepoError> {
    cached::<R, _, _>(
        ctx,
        What::CanContain(name.to_string()),
        repo.can_contain(ctx, name),
    )
    .await
}
