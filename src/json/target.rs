use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Value};
use serde_json::{Map, Value as Json};

use super::{ContainTree, JsonFor};
use crate::association::util::read_in;
use crate::association::{Association, primary_column, row_key};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::meta::{FieldMeta, column_named};
use crate::policy::columns::{table_readable, table_writable};
use crate::policy::{Action, ColumnAccess};
use crate::repo::contain::load_related;
use crate::repo::{GuardedRepo, Loaded, Repo, RepoError, typed_decl_of};

/// Operations on the target of an association, run with the target Repo type.
#[async_trait]
pub trait TargetOps: Send + Sync {
    /// Registered name of the target (`Repo::repo_name`).
    fn name(&self) -> &'static str;

    /// The target's `TablePolicy::can` (no authorization mark).
    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError>;

    /// The target's column access (`TablePolicy::columns`), type-erased.
    async fn columns(&self, ctx: &Context) -> Result<Arc<dyn NamedColumns>, RepoError>;

    /// Reads target rows by id and returns their JSON, keyed by id.
    async fn rows_json(
        &self,
        ctx: &Context,
        ids: &[RowId],
        tree: &ContainTree,
    ) -> Result<HashMap<RowId, Map<String, Json>>, RepoError>;

    /// The target of `relation`, an association of this target (`target_ops::<T>`); `None` when it
    /// is not one.
    fn target_of(&self, relation: &dyn Association) -> Option<Arc<dyn TargetOps>>;

    /// The target's column `name` (`column_named`); `None` when there is none.
    fn column(&self, name: &str) -> Option<Box<dyn DynColumn>>;
}

/// Column access looked up by field (a type-erased `ColumnAccess`).
pub trait NamedColumns: Send + Sync {
    /// Whether the field is readable at table level (as `layer::table_readable`).
    fn table_readable(&self, f: &FieldMeta) -> bool;
    /// Whether the field is writable at table level (as `layer::table_writable`).
    fn table_writable(&self, f: &FieldMeta) -> bool;
    /// The condition of rows where the field is readable (`ColumnAccess::read_scope_named`);
    /// `None` means all rows.
    fn read_scope(&self, f: &FieldMeta) -> Option<Condition>;
}

impl<E: EntityTrait> NamedColumns for ColumnAccess<E> {
    fn table_readable(&self, f: &FieldMeta) -> bool {
        table_readable(self, f)
    }

    fn table_writable(&self, f: &FieldMeta) -> bool {
        table_writable(self, f)
    }

    fn read_scope(&self, f: &FieldMeta) -> Option<Condition> {
        self.read_scope_named(f.name)
    }
}

/// A column with its type erased, building expressions as `ColumnTrait` does.
pub trait DynColumn: Send + Sync {
    /// The column qualified by its table (`Expr::col(c.as_column_ref())`).
    fn expr(&self) -> Expr;
    /// A value for the column, cast as `ColumnTrait::eq` and `is_in` cast it
    /// (`ColumnTrait::save_as`).
    fn value(&self, v: Value) -> Expr;
}

struct ColumnOf<C>(C);

impl<C: ColumnTrait> DynColumn for ColumnOf<C> {
    fn expr(&self) -> Expr {
        Expr::col(self.0.as_column_ref())
    }

    fn value(&self, v: Value) -> Expr {
        self.0.save_as(Expr::val(v))
    }
}

struct OpsFor<R>(PhantomData<fn() -> R>);

/// The operations of Repo `R`.
pub fn ops_of<R: GuardedRepo>() -> Arc<dyn TargetOps> {
    Arc::new(OpsFor::<R>(PhantomData))
}

/// The operations of the target of `relation`; `S` is the Repo that owns the association.
///
/// `None` when `S` is not the owner or the association has no typed contain.
pub fn target_ops<S: GuardedRepo>(relation: &dyn Association) -> Option<Arc<dyn TargetOps>> {
    typed_decl_of::<S>(relation).map(|d| d.target_ops())
}

#[async_trait]
impl<R: GuardedRepo> TargetOps for OpsFor<R> {
    fn name(&self) -> &'static str {
        <R as Repo>::repo_name()
    }

    async fn can(&self, ctx: &Context, action: Action) -> Result<bool, RepoError> {
        ctx.repo::<R>().can(action).await
    }

    async fn columns(&self, ctx: &Context) -> Result<Arc<dyn NamedColumns>, RepoError> {
        let access: Arc<dyn NamedColumns> = ctx.repo::<R>().columns().await?;
        Ok(access)
    }

    async fn rows_json(
        &self,
        ctx: &Context,
        ids: &[RowId],
        tree: &ContainTree,
    ) -> Result<HashMap<RowId, Map<String, Json>>, RepoError> {
        let ids: Vec<Key<R>> = ids.iter().filter_map(Key::<R>::from_row_id).collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let json = JsonFor::<R>::of(ctx)?;
        let meta = json.meta(ctx)?;
        let models =
            read_in::<R, _>(ctx, false, primary_column::<R>(), &ids, None, Vec::new()).await?;
        let loaded = if tree.nodes.is_empty() {
            models.into_iter().map(Loaded::from).collect()
        } else {
            load_related::<R>(ctx, &tree.names(), models).await?
        };
        let refs: Vec<&Loaded<_>> = loaded.iter().collect();
        let rows = json.loaded_rows(ctx, meta, &refs, None, tree).await?;
        Ok(loaded
            .iter()
            .zip(rows)
            .filter_map(|(l, row)| Some((row_key::<R>(l.model())?.into_row_id(), row)))
            .collect())
    }

    fn target_of(&self, relation: &dyn Association) -> Option<Arc<dyn TargetOps>> {
        target_ops::<R>(relation)
    }

    fn column(&self, name: &str) -> Option<Box<dyn DynColumn>> {
        let column = column_named::<R::Entity>(name)?;
        Some(Box::new(ColumnOf(column)))
    }
}
