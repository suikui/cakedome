//! Helpers shared by typed loading and by the hasMany and many-to-many writers.

use crate::messages::{Messages, params};
use crate::repo::GuardedRepo;
use std::collections::HashMap;

use sea_orm::Value;
use sea_orm::sea_query::{SelectStatement, ValueType};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, DbErr, EntityTrait, IdenStatic,
    Iterable, ModelTrait, Order, PrimaryKeyToColumn,
};
use serde_json::Value as Json;

use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::meta::column_named;
use crate::policy::{Action, resolve};
use crate::repo::{Repo, RepoError, RepoHandle, typed_decl_of};

use super::{Association, RelationCondition};

pub(crate) type ModelOf<R> = <<R as Repo>::Entity as EntityTrait>::Model;
pub(crate) type ColumnOf<R> = <<R as Repo>::Entity as EntityTrait>::Column;

/// Returns the primary key column of `R`.
pub fn primary_column<R: GuardedRepo>() -> ColumnOf<R> {
    <<R::Entity as EntityTrait>::PrimaryKey as Iterable>::iter()
        .next()
        .expect("the entity has a primary key")
        .into_column()
}

pub(crate) fn key_value<K: RowKey, M: ModelTrait>(
    model: &M,
    column: <M::Entity as EntityTrait>::Column,
) -> Option<K> {
    <Option<K> as ValueType>::try_from(model.get(column))
        .ok()
        .flatten()
}

/// Add a branch here when adding a primary key type.
pub(crate) fn row_id_of<M: ModelTrait>(
    model: &M,
    column: <M::Entity as EntityTrait>::Column,
) -> Option<RowId> {
    row_id_of_value(model.get(column))
}

pub(crate) fn row_id_of_value(value: Value) -> Option<RowId> {
    match value {
        Value::BigInt(Some(v)) => Some(RowId::Int(v)),
        Value::Uuid(Some(v)) => Some(RowId::Uuid(v)),
        Value::String(Some(v)) => Some(RowId::Str(v.to_string())),
        _ => None,
    }
}

/// Returns the primary key of `model`.
pub fn row_key<R: GuardedRepo>(model: &ModelOf<R>) -> Option<Key<R>> {
    key_value(model, primary_column::<R>())
}

pub(crate) async fn owner_value<S: GuardedRepo>(
    ctx: &Context,
    owner: Key<S>,
    column: ColumnOf<S>,
) -> Result<Option<RowId>, RepoError> {
    let Some(parent) = <S::Entity as EntityTrait>::find_by_id(owner)
        .one(&ctx.conn())
        .await?
    else {
        return Ok(None);
    };
    Ok(row_id_of(&parent, column))
}

pub(crate) fn handle_of<R: GuardedRepo>(ctx: &Context, unchecked: bool) -> RepoHandle<'_, R> {
    let handle = ctx.repo::<R>();
    if unchecked {
        handle.unchecked()
    } else {
        handle
    }
}

/// Reads the rows of `R` with `column IN ids AND condition` in one query, in `orders` order.
///
/// `R`'s `can(View)`, scope and soft delete apply unless `unchecked`. Reads nothing for empty
/// `ids`. The caller supplies the complete order; no default order is added.
pub async fn read_in<R: GuardedRepo, K: RowKey>(
    ctx: &Context,
    unchecked: bool,
    column: ColumnOf<R>,
    ids: &[K],
    condition: Option<Condition>,
    orders: Vec<(ColumnOf<R>, Order)>,
) -> Result<Vec<ModelOf<R>>, RepoError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut find = handle_of::<R>(ctx, unchecked)
        .find()
        .for_association()
        .filter(column.is_in(ids.iter().cloned()));
    if let Some(condition) = condition {
        find = find.filter(condition);
    }
    for (order_column, order) in orders {
        find = find.order_by(order_column, order);
    }
    find.all().await
}

/// Orders the targets of `parent` for a many-to-many load.
/// - `links`: parent id to target ids in join-table order
/// - `readable`: whether a target may be shown (others are dropped)
/// - `position`: `Some` re-sorts by read order (association order, then default order); `None`
///   keeps the join-table order
pub(crate) fn link_order<P: RowKey, T: RowKey>(
    links: &HashMap<P, Vec<T>>,
    parent: &P,
    readable: impl Fn(&T) -> bool,
    position: Option<&HashMap<T, usize>>,
) -> Vec<T> {
    let mut ids: Vec<T> = links
        .get(parent)
        .map(|ids| ids.iter().filter(|id| readable(id)).cloned().collect())
        .unwrap_or_default();
    if let Some(position) = position {
        ids.sort_by_key(|id| position.get(id).copied());
    }
    ids
}

/// Builds the subquery `SELECT column FROM target WHERE condition` without running it.
/// The target Repo's `find()` adds its scope and soft delete (`for_association`).
pub async fn target_subquery<T: GuardedRepo>(
    ctx: &Context,
    column: &str,
    condition: Condition,
) -> Result<SelectStatement, RepoError> {
    let column = own_column::<T>(column)?;
    ctx.repo::<T>()
        .find()
        .for_association()
        .filter(condition)
        .subquery(column)
        .await
}

pub(crate) fn own_column<R: GuardedRepo>(name: &str) -> Result<ColumnOf<R>, DbErr> {
    column_named::<R::Entity>(name).ok_or_else(|| {
        DbErr::Custom(format!(
            "{}: column {name} does not exist",
            <R as Repo>::repo_name()
        ))
    })
}

pub(crate) async fn can_view<R: GuardedRepo>(ctx: &Context) -> Result<bool, RepoError> {
    match ctx.registry().repo::<R>() {
        Some(repo) => resolve::can(ctx, &*repo, Action::View).await,
        None => Ok(false),
    }
}

pub(crate) async fn can_contain<S: GuardedRepo>(
    ctx: &Context,
    name: &str,
) -> Result<bool, RepoError> {
    match ctx.registry().repo::<S>() {
        Some(repo) => resolve::can_contain(ctx, &*repo, name).await,
        None => Ok(false),
    }
}

pub(crate) fn require_parent(
    messages: &Messages,
    parent_id: Option<RowId>,
    name: &str,
    binding_key: Option<&'static str>,
    has_input: bool,
    errors: &mut FieldErrors,
) -> Option<RowId> {
    if parent_id.is_none() && has_input {
        let params = params([("key", binding_key.unwrap_or("id").into())]);
        errors.add_with_params(
            name,
            "invalid",
            &messages.text_params("invalid.parent_key", &params),
            params,
        );
    }
    parent_id
}

pub(crate) async fn current_children<R: GuardedRepo>(
    ctx: &Context,
    unchecked: bool,
    column: ColumnOf<R>,
    parent_value: &RowId,
    filter: &RelationCondition,
) -> Result<Vec<ModelOf<R>>, RepoError> {
    let mut find = handle_of::<R>(ctx, unchecked)
        .find()
        .for_association()
        .filter(column.eq(Value::from(parent_value)));
    if let Some(condition) = filter.condition(ctx).await? {
        find = find.filter(condition);
    }
    find.all().await
}

pub(crate) fn strip_same_values<R: GuardedRepo>(
    am: &mut <R::Entity as EntityTrait>::ActiveModel,
    now: &ModelOf<R>,
) -> bool {
    let pk = primary_column::<R>();
    for c in ColumnOf::<R>::iter() {
        if c.as_str() == pk.as_str() {
            continue;
        }
        if let ActiveValue::Set(v) = am.get(c)
            && v == now.get(c)
        {
            am.not_set(c);
        }
    }
    ColumnOf::<R>::iter()
        .any(|c| c.as_str() != pk.as_str() && matches!(am.get(c), ActiveValue::Set(_)))
}

/// Reads a JSON value as the target primary key of the association `relation` of `S`.
/// `None` if the association is not typed or the value cannot be read.
pub fn target_id_from_json<S: GuardedRepo>(
    relation: &dyn Association,
    value: &Json,
) -> Option<RowId> {
    typed_decl_of::<S>(relation)?.target_id_from_json(value)
}
