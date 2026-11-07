//! Turns rows into JSON that follows column permissions. Used by `RepoHandle::to_json`, `RefBy` and
//! reference checks before saving. Output columns are those `ColumnAccess::can_read` allows, and
//! columns with `read_where` only for matching rows (`id` and `modified` are always shown).

mod contain;

use crate::repo::GuardedRepo;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, DbErr, QueryFilter, Value};
use serde_json::{Map, Value as Json};

use crate::association::{Association, Cardinality, primary_column, row_key};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::to_json;
use crate::meta::{self, FieldKind, column_named, default_order};
use crate::policy::{Action, ColumnAccess, resolve};
use crate::registry::EntityMeta;
use crate::repo::{JoinJson, Loaded, Repo, RepoEntity, RepoError, typed_decl_of};

/// The key of the display field string on target rows of an association. Not usable as a column or
/// association name.
pub const LABEL_KEY: &str = "_label";

/// The key of the join table row on target rows of a many-to-many.
pub const JOIN_DATA_KEY: &str = "_joinData";

pub(crate) use contain::loaded_to_json;
pub use contain::{ContainNode, ContainTree};
pub use contain::{default_contain_tree, trace};

#[async_trait]
pub(crate) trait JsonTarget: Send + Sync {
    /// Converts typed target rows to JSON. `values` holds, per parent row, an
    /// `Option<Loaded<Model>>` (belongsTo) or `Vec<Loaded<Model>>` (hasMany, many-to-many),
    /// type-erased. Returns, per parent row, an object or null (belongsTo) or an array; rows are
    /// `data_json` plus `_label` (without the foreign key column for hasMany).
    async fn related_json(
        &self,
        ctx: &Context,
        relation: &dyn Association,
        children: &ContainTree,
        values: &[&(dyn Any + Send + Sync)],
    ) -> Result<Vec<Json>, RepoError>;

    fn loaded_tree(
        &self,
        ctx: &Context,
        relation: &dyn Association,
        values: &[&(dyn Any + Send + Sync)],
    ) -> Result<ContainTree, RepoError>;
}

/// For columns with `read_where`, the ids of the rows where the column is readable (column name to
/// ids).
///
/// Columns without `read_where` are absent and readable in every row. The conditions are SQL, so
/// one query per such column runs for all displayed ids (no N+1), and none for entities without
/// `read_where`. Columns not readable at table level are not queried.
pub struct Readability<R: GuardedRepo> {
    access: Arc<ColumnAccess<R::Entity>>,
    scoped: HashMap<&'static str, HashSet<Key<R>>>,
}

impl<R: GuardedRepo> Readability<R> {
    /// Whether the column is readable in row `id`, by the row-level condition only.
    fn row_readable(&self, column: &str, id: &Key<R>) -> bool {
        self.scoped.get(column).is_none_or(|ids| ids.contains(id))
    }

    fn column_readable(&self, column: &str) -> bool {
        self.access.can_read_named(column)
    }
}

/// The JSON conversion for Repo `R`, used from outside only as `dyn JsonTarget`.
pub struct JsonFor<R: GuardedRepo> {
    pub(crate) repo: Arc<R>,
}

fn label_string(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

impl<R, E> JsonFor<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    /// Creates one from the Repo `R` held by the registry.
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }

    pub(crate) fn of(ctx: &Context) -> Result<Self, RepoError> {
        let repo = ctx
            .registry()
            .repo::<R>()
            .ok_or_else(|| RepoError::NotRegistered(<R as Repo>::repo_name().to_string()))?;
        Ok(Self { repo })
    }

    pub(crate) fn meta<'a>(&self, ctx: &'a Context) -> Result<&'a EntityMeta, RepoError> {
        ctx.registry()
            .meta_of::<E>()
            .ok_or_else(|| RepoError::NotRegistered(<R as Repo>::repo_name().to_string()))
    }

    /// Resolves column access (`TablePolicy::columns`) for `models` and reads, per column with
    /// `read_where`, the ids of the rows that can read it.
    ///
    /// One query `primary key IN (ids) AND read_where` runs per column, without Policy as the rows
    /// are already readable. No DB read when no column has `read_where`.
    pub async fn row_readability(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        models: &[E::Model],
    ) -> Result<Readability<R>, RepoError> {
        let ids: Vec<Key<R>> = models.iter().filter_map(row_key::<R>).collect();
        self.row_readability_of(ctx, meta, ids).await
    }

    pub(crate) async fn row_readability_of(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        ids: Vec<Key<R>>,
    ) -> Result<Readability<R>, RepoError> {
        let access = resolve::columns(ctx, &*self.repo).await?;
        let mut scoped: Vec<(&'static str, Condition)> = Vec::new();
        for f in &meta.fields {
            if !access.can_read_named(f.name) {
                continue;
            }
            if let Some(condition) = access.read_scope_named(f.name) {
                scoped.push((f.name, condition));
            }
        }
        let mut readable: HashMap<&'static str, HashSet<Key<R>>> = HashMap::new();
        if !scoped.is_empty() && !ids.is_empty() {
            for (name, condition) in scoped {
                let rows = E::find()
                    .filter(primary_column::<R>().is_in(ids.iter().cloned()))
                    .filter(condition)
                    .all(&ctx.conn())
                    .await?;
                readable.insert(name, rows.iter().filter_map(row_key::<R>).collect());
            }
        }
        Ok(Readability {
            access,
            scoped: readable,
        })
    }

    /// Returns the display field value as the `label`, or the id string if the column is unreadable
    /// at table level or for the row (so `_label` does not leak what `data_json` hides).
    pub fn row_label(
        &self,
        meta: &EntityMeta,
        model: &E::Model,
        id: &Key<R>,
        readability: &Readability<R>,
    ) -> String {
        if column_named::<E>(meta.display_field).is_none() {
            return id.clone().into_row_id().to_string();
        }
        if !readability.column_readable(meta.display_field)
            || !readability.row_readable(meta.display_field, id)
        {
            return id.clone().into_row_id().to_string();
        }
        let map = to_json::<E>(&meta.fields, model, |f| f.name == meta.display_field);
        map.get(meta.display_field)
            .map(label_string)
            .unwrap_or_else(|| id.clone().into_row_id().to_string())
    }

    /// Returns the `data` of a row: columns readable at table level, and those with `read_where`
    /// only for matching rows (`readability`). `id` and `modified` are always included.
    pub fn data_json(
        &self,
        meta: &EntityMeta,
        model: &E::Model,
        readability: &Readability<R>,
    ) -> Map<String, Json> {
        let id = row_key::<R>(model);
        to_json::<E>(&meta.fields, model, |f| {
            if f.primary_key || meta.spec.modified_column == Some(f.name) {
                return true;
            }
            column_named::<E>(f.name).is_some()
                && readability.column_readable(f.name)
                && id
                    .as_ref()
                    .is_some_and(|id| readability.row_readable(f.name, id))
        })
    }

    fn active_filter(ctx: &Context) -> Option<sea_orm::sea_query::SimpleExpr> {
        meta::active_filter::<E>(&ctx.registry().spec_of::<E>())
    }

    fn flatten<'v>(
        &self,
        relation: &dyn Association,
        values: &[&'v (dyn Any + Send + Sync)],
    ) -> Option<Flattened<'v, E::Model>> {
        let belongs = relation.cardinality() == Cardinality::One;
        let mut flat: Vec<&Loaded<E::Model>> = Vec::new();
        let mut sizes: Vec<usize> = Vec::new();
        for value in values {
            if belongs {
                let one = value.downcast_ref::<Option<Loaded<E::Model>>>()?;
                sizes.push(usize::from(one.is_some()));
                flat.extend(one.iter());
            } else {
                let many = value.downcast_ref::<Vec<Loaded<E::Model>>>()?;
                sizes.push(many.len());
                flat.extend(many.iter());
            }
        }
        Some((flat, sizes))
    }

    /// Returns the JSON of target rows (`Loaded`): `data_json` plus `_label`, without
    /// `drop_column`, with lower levels of `tree` nested. Row-level column readability is resolved
    /// once for all rows.
    pub async fn loaded_rows(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        rows: &[&Loaded<E::Model>],
        drop_column: Option<&str>,
        tree: &ContainTree,
    ) -> Result<Vec<Map<String, Json>>, RepoError> {
        let ids: Vec<Key<R>> = rows
            .iter()
            .filter_map(|r| row_key::<R>(r.model()))
            .collect();
        let readability = self.row_readability_of(ctx, meta, ids).await?;
        let mut datas: Vec<Map<String, Json>> = rows
            .iter()
            .map(|r| {
                let m = r.model();
                let mut data = self.data_json(meta, m, &readability);
                if let Some(column) = drop_column {
                    data.remove(column);
                }
                let label = row_key::<R>(m)
                    .map(|id| self.row_label(meta, m, &id, &readability))
                    .unwrap_or_default();
                data.insert(LABEL_KEY.to_string(), Json::String(label));
                data
            })
            .collect();
        if !tree.nodes.is_empty() {
            Self::attach_loaded(ctx, rows, &mut datas, tree).await?;
        }
        Ok(datas)
    }

    /// Attaches the join table row of each many-to-many target as `_joinData`, in the join Repo's
    /// `data_json` shape (its `columns` Policy applies). Rows read without a join are skipped.
    async fn attach_join_data(
        ctx: &Context,
        rows: &[&Loaded<E::Model>],
        datas: &mut [Map<String, Json>],
    ) -> Result<(), RepoError> {
        let present: Vec<(usize, &(dyn Any + Send + Sync), JoinJson)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| Some((i, r.join_raw()?, r.join_json()?)))
            .collect();
        let Some((_, _, json)) = present.first() else {
            return Ok(());
        };
        let models: Vec<&(dyn Any + Send + Sync)> = present.iter().map(|(_, m, _)| *m).collect();
        let jsons = json(ctx, &models).await?;
        for ((index, _, _), json) in present.iter().zip(jsons) {
            datas[*index].insert(JOIN_DATA_KEY.to_string(), json);
        }
        Ok(())
    }

    /// Builds the tree of associations to include from the loaded rows, in registration order.
    /// A level with no rows has empty children.
    pub(crate) fn tree_of(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        rows: &[&Loaded<E::Model>],
    ) -> Result<ContainTree, RepoError> {
        let mut tree = ContainTree::default();
        for relation in &meta.relations {
            let values: Vec<_> = rows
                .iter()
                .filter_map(|r| r.related_raw(relation.name()))
                .collect();
            if values.is_empty() {
                continue;
            }
            let children = match typed_decl_of::<R>(relation.as_ref())
                .and_then(|decl| decl.target_json(ctx).ok())
            {
                Some(target) => target.loaded_tree(ctx, relation.as_ref(), &values)?,
                None => ContainTree::default(),
            };
            tree.nodes.push(ContainNode {
                relation: relation.clone(),
                children,
            });
        }
        Ok(tree)
    }

    /// Converts `rows` to JSON in the shape of a get's `data` (no `_label` on top-level rows).
    pub(crate) async fn loaded_data(
        &self,
        ctx: &Context,
        rows: &[Loaded<E::Model>],
    ) -> Result<Vec<Json>, RepoError> {
        let meta = self.meta(ctx)?;
        let refs: Vec<&Loaded<E::Model>> = rows.iter().collect();
        let ids: Vec<Key<R>> = rows
            .iter()
            .filter_map(|r| row_key::<R>(r.model()))
            .collect();
        let readability = self.row_readability_of(ctx, meta, ids).await?;
        let mut datas: Vec<Map<String, Json>> = rows
            .iter()
            .map(|r| self.data_json(meta, r.model(), &readability))
            .collect();
        let tree = self.tree_of(ctx, meta, &refs)?;
        if !tree.nodes.is_empty() {
            Self::attach_loaded(ctx, &refs, &mut datas, &tree).await?;
        }
        Ok(datas.into_iter().map(Json::Object).collect())
    }
}

type Flattened<'v, M> = (Vec<&'v Loaded<M>>, Vec<usize>);

pub(crate) fn lookup_value(kind: &FieldKind, value: &Json) -> Option<Value> {
    match (kind, value) {
        (FieldKind::String { .. } | FieldKind::Text, Json::String(s)) => Some(s.clone().into()),
        (FieldKind::Integer, Json::Number(n)) => n
            .as_i64()
            .and_then(|v| <i32 as TryFrom<i64>>::try_from(v).ok())
            .map(Into::into),
        (FieldKind::BigInteger, Json::Number(n)) => n.as_i64().map(Into::into),
        (FieldKind::Uuid, Json::String(s)) => <uuid::Uuid as RowKey>::parse(s).map(Into::into),
        (FieldKind::Boolean, Json::Bool(b)) => Some((*b).into()),
        (FieldKind::Double, Json::Number(n)) => n.as_f64().map(Into::into),
        (FieldKind::Decimal { .. }, Json::String(_) | Json::Number(_)) => {
            crate::marshal::decimal_text(value)?
                .parse::<sea_orm::prelude::Decimal>()
                .ok()
                .map(Into::into)
        }
        _ => None,
    }
}

#[async_trait]
impl<R, E> JsonTarget for JsonFor<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    async fn related_json(
        &self,
        ctx: &Context,
        relation: &dyn Association,
        children: &ContainTree,
        values: &[&(dyn Any + Send + Sync)],
    ) -> Result<Vec<Json>, RepoError> {
        let mismatch = || {
            RepoError::from(DbErr::Custom(format!(
                "{}: the loaded value of association {} has an unexpected type",
                <R as Repo>::repo_name(),
                relation.name()
            )))
        };
        let belongs = relation.cardinality() == Cardinality::One;
        let drop_column = relation.child_foreign_key();
        let (flat, sizes) = self.flatten(relation, values).ok_or_else(mismatch)?;
        let meta = self.meta(ctx)?;
        let mut datas = self
            .loaded_rows(ctx, meta, &flat, drop_column, children)
            .await?;
        Self::attach_join_data(ctx, &flat, &mut datas).await?;
        let mut datas = datas.into_iter();
        Ok(sizes
            .into_iter()
            .map(|n| {
                if belongs {
                    // Do not advance for a parent without a target (n == 0), or the next row's
                    // target is lost.
                    if n == 1 {
                        datas.next().map_or(Json::Null, Json::Object)
                    } else {
                        Json::Null
                    }
                } else {
                    Json::Array(datas.by_ref().take(n).map(Json::Object).collect())
                }
            })
            .collect())
    }

    fn loaded_tree(
        &self,
        ctx: &Context,
        relation: &dyn Association,
        values: &[&(dyn Any + Send + Sync)],
    ) -> Result<ContainTree, RepoError> {
        let (flat, _) = self.flatten(relation, values).ok_or_else(|| {
            RepoError::from(DbErr::Custom(format!(
                "{}: the loaded value of association {} has an unexpected type",
                <R as Repo>::repo_name(),
                relation.name()
            )))
        })?;
        self.tree_of(ctx, self.meta(ctx)?, &flat)
    }
}

impl<R, E> JsonFor<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    pub(crate) async fn models_json(
        &self,
        ctx: &Context,
        models: &[&(dyn Any + Send + Sync)],
    ) -> Result<Vec<Json>, RepoError> {
        let rows: Vec<&E::Model> = models
            .iter()
            .map(|m| m.downcast_ref::<E::Model>())
            .collect::<Option<_>>()
            .ok_or_else(|| {
                RepoError::from(DbErr::Custom(format!(
                    "{}: the row has an unexpected type",
                    <R as Repo>::repo_name()
                )))
            })?;
        let meta = self.meta(ctx)?;
        let ids: Vec<Key<R>> = rows.iter().filter_map(|m| row_key::<R>(m)).collect();
        let readability = self.row_readability_of(ctx, meta, ids).await?;
        Ok(rows
            .into_iter()
            .map(|m| Json::Object(self.data_json(meta, m, &readability)))
            .collect())
    }

    /// Returns the id of the first row whose `column` equals `value`, used by `RefBy`.
    ///
    /// `None` if `can(View)` is false, the column is missing or the value does not fit the column
    /// type. Goes through `RepoHandle::find()`, so scope and soft delete apply, and only active
    /// rows for Activatable, in `default_order`.
    pub(crate) async fn lookup_id(
        &self,
        ctx: &Context,
        column: &str,
        value: &Json,
    ) -> Result<Option<RowId>, RepoError> {
        if !resolve::can(ctx, &*self.repo, Action::View).await? {
            return Ok(None);
        }
        let meta = self.meta(ctx)?;
        let (Some(col), Some(field)) = (
            column_named::<E>(column),
            meta.fields.iter().find(|f| f.name == column),
        ) else {
            return Ok(None);
        };
        let Some(sql_value) = lookup_value(&field.kind, value) else {
            return Ok(None);
        };
        let mut find = ctx
            .repo::<R>()
            .find()
            .for_association()
            .filter(col.eq(sql_value));
        if let Some(active) = Self::active_filter(ctx) {
            find = find.filter(active);
        }
        for (column, order) in default_order::<E>(&meta.spec) {
            find = find.order_by(column, order);
        }
        let rows = find.limit(1).all().await?;
        Ok(rows.first().and_then(row_key::<R>).map(RowKey::into_row_id))
    }
}

pub(crate) fn json_join_of<'a, J: GuardedRepo>(
    ctx: &'a Context,
    models: &'a [&'a (dyn Any + Send + Sync)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Json>, RepoError>> + Send + 'a>>
where
    J::Entity: RepoEntity,
{
    Box::pin(async move { JsonFor::<J>::of(ctx)?.models_json(ctx, models).await })
}
