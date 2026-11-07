//! Checks that a row may be chosen as the target of a foreign key, and bulk helpers that work
//! without the target's Policy.

use crate::repo::GuardedRepo;
use std::collections::{HashMap, HashSet};

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, QueryFilter, Value};

use super::util::{ColumnOf, primary_column, row_id_of, row_key};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::meta::{active_filter, column_named};
use crate::policy::{Action, resolve};
use crate::repo::RepoError;

/// Returns whether the row `id` can be chosen as a foreign-key target.
///
/// False without `can(View)` on `R`; otherwise the row must be within scope and soft delete, and
/// active if `R` is Activatable.
pub async fn selectable<R: GuardedRepo>(ctx: &Context, id: &RowId) -> Result<bool, RepoError> {
    selectable_by::<R, Key<R>>(ctx, primary_column::<R>(), id, None).await
}

/// A `value` that is not a `K` counts as not found.
pub(crate) async fn selectable_by<R: GuardedRepo, K: RowKey>(
    ctx: &Context,
    column: ColumnOf<R>,
    value: &RowId,
    extra: Option<Condition>,
) -> Result<bool, RepoError> {
    let Some(id) = K::from_row_id(value) else {
        return Ok(false);
    };
    let viewable = match ctx.registry().repo::<R>() {
        Some(repo) => resolve::can(ctx, &*repo, Action::View).await?,
        None => false,
    };
    if !viewable {
        return Ok(false);
    }
    let mut find = ctx
        .repo::<R>()
        .find()
        .for_association()
        .filter(column.eq(id))
        .limit(1);
    if let Some(extra) = extra {
        find = find.filter(extra);
    }
    if let Some(active) = active_filter::<R::Entity>(&ctx.registry().spec_of::<R::Entity>()) {
        find = find.filter(active);
    }
    Ok(!find.all().await?.is_empty())
}

/// Returns the `column` values among `ids` whose rows match the association condition.
///
/// Policy and soft delete are deliberately skipped: the condition alone defines which rows are
/// related, so the pairs a Replace removes do not depend on the viewer.
pub(crate) async fn matching_by<R: GuardedRepo, K: RowKey>(
    ctx: &Context,
    column: ColumnOf<R>,
    ids: &[RowId],
    filter: Condition,
) -> Result<HashSet<RowId>, RepoError> {
    let ids: Vec<K> = ids.iter().filter_map(K::from_row_id).collect();
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = R::Entity::find()
        .filter(column.is_in(ids))
        .filter(filter)
        .all(&ctx.conn())
        .await?;
    Ok(rows.iter().filter_map(|m| row_id_of(m, column)).collect())
}

pub(crate) async fn binding_values<R: GuardedRepo>(
    ctx: &Context,
    column: ColumnOf<R>,
    ids: &[RowId],
) -> Result<HashMap<RowId, RowId>, RepoError> {
    let ids: Vec<Key<R>> = ids.iter().filter_map(Key::<R>::from_row_id).collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = R::Entity::find()
        .filter(primary_column::<R>().is_in(ids))
        .all(&ctx.conn())
        .await?;
    Ok(rows
        .iter()
        .filter_map(|m| Some((row_key::<R>(m)?.into_row_id(), row_id_of(m, column)?)))
        .collect())
}

/// Returns the ids of rows with `column = value` and the association condition, skipping
/// soft-deleted rows. No Policy or scope. Returns `DbErr` if the column does not exist.
pub(crate) async fn ids_where<R: GuardedRepo>(
    ctx: &Context,
    column: &str,
    value: &RowId,
    filter: Option<Condition>,
) -> Result<Vec<RowId>, RepoError> {
    let column = column_named::<R::Entity>(column)
        .ok_or_else(|| DbErr::Custom(format!("column {column} does not exist")))?;
    let mut select = R::Entity::find().filter(column.eq(Value::from(value)));
    if let Some(filter) = filter {
        select = select.filter(filter);
    }
    if let Some(alive) = not_soft_deleted::<R::Entity>(ctx) {
        select = select.filter(alive);
    }
    Ok(select
        .all(&ctx.conn())
        .await?
        .iter()
        .filter_map(row_key::<R>)
        .map(RowKey::into_row_id)
        .collect())
}

pub(crate) async fn links_where<R: GuardedRepo>(
    ctx: &Context,
    column: &str,
    value: &RowId,
    other: &str,
) -> Result<Vec<(RowId, RowId)>, RepoError> {
    let (Some(column), Some(other)) = (
        column_named::<R::Entity>(column),
        column_named::<R::Entity>(other),
    ) else {
        return Err(DbErr::Custom(format!("neither column {column} nor {other} exists")).into());
    };
    let mut select = R::Entity::find().filter(column.eq(Value::from(value)));
    if let Some(alive) = not_soft_deleted::<R::Entity>(ctx) {
        select = select.filter(alive);
    }
    Ok(select
        .all(&ctx.conn())
        .await?
        .iter()
        .filter_map(|m| Some((row_key::<R>(m)?.into_row_id(), row_id_of(m, other)?)))
        .collect())
}

pub(crate) async fn delete<R: GuardedRepo>(
    ctx: &Context,
    id: &RowId,
    unchecked: bool,
) -> Result<(), RepoError> {
    let Some(id) = Key::<R>::from_row_id(id) else {
        return Err(RepoError::NotFound);
    };
    super::util::handle_of::<R>(ctx, unchecked)
        .without_before_find()
        .delete(id)
        .await
}

/// Deletes the child rows `ids` with one statement (no per-row callbacks).
///
/// Soft-delete tables get an UPDATE setting `is_active = false` and `modified`. Policy, hooks,
/// observers and history are skipped, and the children's `dependent` associations are not followed.
pub(crate) async fn bulk_delete<R: GuardedRepo>(
    ctx: &Context,
    ids: &[RowId],
) -> Result<(), RepoError> {
    if ids.is_empty() {
        return Ok(());
    }
    let values: Vec<Value> = ids.iter().map(Value::from).collect();
    let pk = primary_column::<R>();
    let spec = ctx.registry().spec_of::<R::Entity>();
    let active = spec.active_column.and_then(column_named::<R::Entity>);
    match (spec.lifecycle == crate::meta::Lifecycle::SoftDelete, active) {
        (true, Some(active)) => {
            let mut update = R::Entity::update_many()
                .col_expr(active, Expr::value(false))
                .filter(pk.is_in(values));
            if let Some(modified) = spec.modified_column.and_then(column_named::<R::Entity>) {
                update = update.col_expr(modified, Expr::value(crate::clock::now()));
            }
            update.exec(&ctx.conn()).await?;
        }
        _ => {
            R::Entity::delete_many()
                .filter(pk.is_in(values))
                .exec(&ctx.conn())
                .await?;
        }
    }
    Ok(())
}

/// Sets the foreign key `column` of the child rows `ids` to NULL with one statement, advancing
/// `modified`. Policy, hooks, observers and history are skipped.
pub(crate) async fn bulk_nullify<R: GuardedRepo>(
    ctx: &Context,
    column: ColumnOf<R>,
    ids: &[RowId],
) -> Result<(), RepoError> {
    if ids.is_empty() {
        return Ok(());
    }
    let values: Vec<Value> = ids.iter().map(Value::from).collect();
    let spec = ctx.registry().spec_of::<R::Entity>();
    let mut update = R::Entity::update_many()
        .col_expr(column, Expr::cust("NULL"))
        .filter(primary_column::<R>().is_in(values));
    if let Some(modified) = spec.modified_column.and_then(column_named::<R::Entity>) {
        update = update.col_expr(modified, Expr::value(crate::clock::now()));
    }
    update.exec(&ctx.conn()).await?;
    Ok(())
}

fn not_soft_deleted<E: sea_orm::EntityTrait>(
    ctx: &Context,
) -> Option<sea_orm::sea_query::SimpleExpr> {
    let spec = ctx.registry().spec_of::<E>();
    if spec.lifecycle != crate::meta::Lifecycle::SoftDelete {
        return None;
    }
    spec.active_column
        .and_then(column_named::<E>)
        .map(|c| c.eq(true))
}
