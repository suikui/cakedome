//! The registered Repo with its unchecked flag, plus `authorize` and `base_condition`.
//!
//! Policies are called only from here and `policy::resolve`.

use std::sync::Arc;

use sea_orm::{ColumnTrait, Condition, EntityTrait};

use super::{ActiveModelOf, GuardedRepo, ModelOf, RepoError};
use crate::context::Context;
use crate::meta::{Lifecycle, column_named};
use crate::policy::{Action, ColumnAccess, EntityPolicy, resolve};

// When unchecked, everything is allowed and scope adds no condition. `validate` is never skipped.
pub(super) struct Guard<R: GuardedRepo> {
    pub(super) repo: Arc<R>,
    unchecked: bool,
}

impl<R: GuardedRepo> Guard<R> {
    async fn scope(&self, ctx: &Context, action: Action) -> Result<Condition, RepoError> {
        if self.unchecked {
            Ok(Condition::all())
        } else {
            resolve::scope(ctx, &*self.repo, action).await
        }
    }

    pub(super) async fn columns(
        &self,
        ctx: &Context,
    ) -> Result<Arc<ColumnAccess<R::Entity>>, RepoError> {
        if self.unchecked {
            Ok(Arc::new(ColumnAccess::all()))
        } else {
            resolve::columns(ctx, &*self.repo).await
        }
    }

    pub(super) async fn can_add(
        &self,
        ctx: &Context,
        after: &ActiveModelOf<R::Entity>,
    ) -> Result<bool, RepoError> {
        if self.unchecked {
            return Ok(true);
        }
        EntityPolicy::can_add(&*self.repo, ctx, after).await
    }

    pub(super) async fn can_edit(
        &self,
        ctx: &Context,
        before: &ModelOf<R::Entity>,
        after: &ActiveModelOf<R::Entity>,
    ) -> Result<bool, RepoError> {
        if self.unchecked {
            return Ok(true);
        }
        EntityPolicy::can_edit(&*self.repo, ctx, before, after).await
    }

    pub(super) async fn can_delete(
        &self,
        ctx: &Context,
        model: &ModelOf<R::Entity>,
    ) -> Result<bool, RepoError> {
        if self.unchecked {
            return Ok(true);
        }
        EntityPolicy::can_delete(&*self.repo, ctx, model).await
    }
}

pub(super) fn registered<R: GuardedRepo>(ctx: &Context) -> Result<Arc<R>, RepoError> {
    ctx.registry()
        .repo::<R>()
        .ok_or_else(|| RepoError::NotRegistered(<R as super::Repo>::repo_name().to_string()))
}

// Not registered fails with `NotRegistered` even when unchecked, without marking.
// Unchecked skips `can` but still marks.
pub(super) async fn authorize<R: GuardedRepo>(
    ctx: &Context,
    action: Action,
    unchecked: bool,
) -> Result<Guard<R>, RepoError> {
    let repo = registered::<R>(ctx)?;
    if !unchecked && !resolve::can(ctx, &*repo, action).await? {
        return Err(if ctx.identity().is_authenticated() {
            RepoError::Forbidden
        } else {
            RepoError::Unauthenticated
        });
    }
    ctx.authz().mark();
    Ok(Guard { repo, unchecked })
}

// `Activatable` rows are not filtered by `is_active`: inactive rows are still targets.
pub(super) async fn base_condition<E: EntityTrait>(
    ctx: &Context,
    guard: &Guard<impl GuardedRepo<Entity = E>>,
    action: Action,
) -> Result<Condition, RepoError> {
    let mut cond = Condition::all().add(guard.scope(ctx, action).await?);
    let spec = ctx.registry().spec_of::<E>();
    if spec.lifecycle == Lifecycle::SoftDelete
        && let Some(is_active) = spec.active_column.and_then(column_named::<E>)
    {
        cond = cond.add(is_active.eq(true));
    }
    Ok(cond)
}
