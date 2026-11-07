//! Conversion of the rows of hasMany, hasOne, belongsTo and many-to-many targets (the `prepare_*`
//! methods of `Marshaller`).

use std::collections::HashSet;

use serde_json::{Map, Value as Json};

use super::save::RowSpec;
use super::{Marshaller, PatchError, marshaller_of};
use crate::association::{
    ChildRow, ChildRows, NestedScope, ParentRow, PreparedChildren, PreparedParent,
    RelationCondition,
};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::{FieldErrors, MarshalOptions, WriteMode};
use crate::registry::SaveStrategy;
use crate::repo::{GuardedRepo, RepoEntity};

pub(crate) struct ChildSpec<'a> {
    pub(crate) name: &'static str,
    pub(crate) column: &'a str,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) dependent: bool,
    pub(crate) cascade_callbacks: bool,
    pub(crate) save: SaveStrategy,
    pub(crate) filter: RelationCondition,
    pub(crate) scope: NestedScope,
}

// The two foreign keys are `unknown` and the sequence column is `read_only`.
pub(crate) async fn prepare_join<J: GuardedRepo>(
    ctx: &Context,
    spec: &JoinSpec<'_>,
    row: &Map<String, Json>,
    current: Option<RowId>,
) -> Result<Option<<J::Entity as sea_orm::EntityTrait>::ActiveModel>, PatchError> {
    let Some(marshaller) = marshaller_of::<J>(ctx) else {
        return Ok(None);
    };
    marshaller
        .prepare_join(ctx, spec, row, current)
        .await
        .map(Some)
}

pub(crate) struct JoinSpec<'a> {
    pub(crate) name: &'static str,
    pub(crate) index: usize,
    pub(crate) columns: [&'a str; 2],
    pub(crate) scope: NestedScope,
}

// - A row `id` makes it an update (`Edit`), otherwise an insert (`Add`); a repeated `id` is
//   `duplicate`.
// - The foreign key is `unknown`. With `Replace` and a `seq` column, `seq` is `read_only`. Columns
//   not readable for the table are `unknown`.
// - Only `AppendWithDelete` reads `_delete` as a delete mark; a mark on a row without `id` means
//   "do not add".
pub(crate) async fn prepare_children<T: GuardedRepo>(
    ctx: &Context,
    spec: &ChildSpec<'_>,
    rows: &[Map<String, Json>],
) -> Result<Option<Box<dyn PreparedChildren>>, PatchError> {
    let Some(marshaller) = marshaller_of::<T>(ctx) else {
        return Ok(None);
    };
    marshaller.prepare_children(ctx, spec, rows).await.map(Some)
}

// Without a body `id`, a `current` target is updated; the row is converted once.
pub(crate) async fn prepare_one<T: GuardedRepo>(
    ctx: &Context,
    spec: &ChildSpec<'_>,
    row: &Map<String, Json>,
    current: Option<RowId>,
) -> Result<Option<Box<dyn PreparedChildren>>, PatchError> {
    let Some(marshaller) = marshaller_of::<T>(ctx) else {
        return Ok(None);
    };
    marshaller
        .prepare_one(ctx, spec, row, current)
        .await
        .map(Some)
}

pub(crate) async fn prepare_parent<T: GuardedRepo>(
    ctx: &Context,
    name: &'static str,
    foreign_key: &'static str,
    binding_key: Option<&'static str>,
    row: &Map<String, Json>,
    scope: NestedScope,
) -> Result<Option<Box<dyn PreparedParent>>, PatchError> {
    let Some(marshaller) = marshaller_of::<T>(ctx) else {
        return Ok(None);
    };
    marshaller
        .prepare_parent(ctx, name, foreign_key, binding_key, row, &scope)
        .await
        .map(Some)
}

impl<R, E> Marshaller<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    // Returns `Ok(None)` when the row is not used: bad id shape, a delete mark without id, a
    // first-stage error, or no `am`.
    async fn marshal_nested(
        &self,
        ctx: &Context,
        row: &Map<String, Json>,
        spec: NestedRow<'_, R>,
        errors: &mut FieldErrors,
    ) -> Result<Option<NestedOut<R>>, PatchError> {
        let NestedRow {
            name,
            prefix,
            body_id,
            current,
            seen,
            delete_flag,
            hidden_columns,
            read_only_column,
            scope,
        } = spec;
        let mut input = row.clone();
        let hook_id = body_id
            .then(|| input.get("id").and_then(Key::<R>::from_json))
            .flatten()
            .or_else(|| current.clone());
        let hook_options = MarshalOptions::new(
            if hook_id.is_some() {
                WriteMode::Edit
            } else {
                WriteMode::Add
            },
            hook_id.map(RowKey::into_row_id),
            Some(name),
        );
        self.repo
            .before_marshal(ctx, &mut input, &hook_options)
            .await?;
        let hooked = input.clone();
        let mut id = None;
        if body_id && let Some(value) = input.remove("id") {
            match Key::<R>::from_json(&value) {
                Some(n) => {
                    if let Some(seen) = seen
                        && !seen.insert(n.clone())
                    {
                        errors.add(
                            &format!("{prefix}.id"),
                            "duplicate",
                            &ctx.registry().messages().text("duplicate", &[]),
                        );
                    }
                    id = Some(n);
                }
                None => {
                    errors.add(
                        &format!("{prefix}.id"),
                        "type",
                        &ctx.registry().messages().text("type", &[]),
                    );
                    return Ok(None);
                }
            }
        }
        let id = id.or(current);
        let mut delete = false;
        if delete_flag && let Some(value) = input.remove("_delete") {
            match value.as_bool() {
                Some(b) => delete = b,
                None => errors.add(
                    &format!("{prefix}._delete"),
                    "type",
                    &ctx.registry().messages().text("type", &[]),
                ),
            }
        }
        if delete && id.is_none() {
            return Ok(None);
        }
        let mode = if id.is_some() {
            WriteMode::Edit
        } else {
            WriteMode::Add
        };
        let none_scope;
        let row_scope = if delete {
            none_scope = scope.none();
            &none_scope
        } else {
            scope
        };
        let mut row_errors = FieldErrors::default();
        let out = self
            .marshal_row(
                ctx,
                &hooked,
                input,
                RowSpec {
                    mode,
                    id: id.clone(),
                    association: Some(name),
                    hidden_columns,
                    read_only_column,
                    defaults: false,
                    run_after: !delete,
                    scope: row_scope,
                },
                &mut row_errors,
            )
            .await?;
        if !row_errors.is_empty() {
            errors.merge_prefixed(&prefix, row_errors);
            return Ok(None);
        }
        Ok(out.am.map(|am| NestedOut {
            id,
            delete,
            am,
            parents: out.parents,
            prepared: out.prepared,
        }))
    }

    async fn prepare_join(
        &self,
        ctx: &Context,
        spec: &JoinSpec<'_>,
        row: &Map<String, Json>,
        current: Option<RowId>,
    ) -> Result<<E as sea_orm::EntityTrait>::ActiveModel, PatchError> {
        let mut errors = FieldErrors::default();
        let out = self
            .marshal_nested(
                ctx,
                row,
                NestedRow {
                    name: spec.name,
                    prefix: format!("{}.{}._joinData", spec.name, spec.index),
                    body_id: false,
                    current: current.as_ref().and_then(Key::<R>::from_row_id),
                    seen: None,
                    delete_flag: false,
                    hidden_columns: &spec.columns,
                    read_only_column: ctx.registry().spec_of::<E>().order_column,
                    scope: &spec.scope,
                },
                &mut errors,
            )
            .await?;
        match out {
            Some(out) if errors.is_empty() => Ok(out.am),
            _ => Err(PatchError::Invalid(errors)),
        }
    }

    async fn prepare_one(
        &self,
        ctx: &Context,
        spec: &ChildSpec<'_>,
        row: &Map<String, Json>,
        current: Option<RowId>,
    ) -> Result<Box<dyn PreparedChildren>, PatchError> {
        let mut errors = FieldErrors::default();
        let out = self
            .marshal_nested(
                ctx,
                row,
                NestedRow {
                    name: spec.name,
                    prefix: spec.name.to_string(),
                    body_id: true,
                    current: current.as_ref().and_then(Key::<R>::from_row_id),
                    seen: None,
                    delete_flag: false,
                    hidden_columns: &[spec.column],
                    read_only_column: None,
                    scope: &spec.scope,
                },
                &mut errors,
            )
            .await?;
        match out {
            Some(out) if errors.is_empty() => Ok(Box::new(ChildRows::<R> {
                name: spec.name.to_string(),
                column: spec.column.to_string(),
                binding_key: spec.binding_key,
                dependent: spec.dependent,
                cascade_callbacks: spec.cascade_callbacks,
                link: false,
                save: spec.save,
                filter: spec.filter.clone(),
                single: true,
                rows: vec![ChildRow {
                    index: 0,
                    id: out.id,
                    delete: false,
                    am: Some(out.am),
                    parents: out.parents,
                    prepared: out.prepared,
                }],
            })),
            _ => Err(PatchError::Invalid(errors)),
        }
    }

    async fn prepare_parent(
        &self,
        ctx: &Context,
        name: &'static str,
        foreign_key: &'static str,
        binding_key: Option<&'static str>,
        row: &Map<String, Json>,
        scope: &NestedScope,
    ) -> Result<Box<dyn PreparedParent>, PatchError> {
        let mut errors = FieldErrors::default();
        let out = self
            .marshal_nested(
                ctx,
                row,
                NestedRow {
                    name,
                    prefix: name.to_string(),
                    body_id: true,
                    current: None,
                    seen: None,
                    delete_flag: false,
                    hidden_columns: &[],
                    read_only_column: None,
                    scope,
                },
                &mut errors,
            )
            .await?;
        match out {
            Some(out) if errors.is_empty() => Ok(Box::new(ParentRow::<R> {
                name,
                foreign_key,
                binding_key,
                am: out.am,
                parents: out.parents,
                prepared: out.prepared,
            })),
            _ => Err(PatchError::Invalid(errors)),
        }
    }

    async fn prepare_children(
        &self,
        ctx: &Context,
        spec: &ChildSpec<'_>,
        rows: &[Map<String, Json>],
    ) -> Result<Box<dyn PreparedChildren>, PatchError> {
        let meta = self.meta(ctx)?;
        let replace_seq = spec.save == SaveStrategy::Replace && meta.spec.order_column.is_some();
        let mut errors = FieldErrors::default();
        let mut seen: HashSet<Key<R>> = HashSet::new();
        let mut prepared = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let out = self
                .marshal_nested(
                    ctx,
                    row,
                    NestedRow {
                        name: spec.name,
                        prefix: format!("{}.{index}", spec.name),
                        body_id: true,
                        current: None,
                        seen: Some(&mut seen),
                        delete_flag: spec.save == SaveStrategy::AppendWithDelete,
                        hidden_columns: &[spec.column],
                        read_only_column: if replace_seq {
                            meta.spec.order_column
                        } else {
                            None
                        },
                        scope: &spec.scope,
                    },
                    &mut errors,
                )
                .await?;
            let Some(out) = out else { continue };
            prepared.push(ChildRow {
                index,
                id: out.id,
                delete: out.delete,
                am: (!out.delete).then_some(out.am),
                parents: out.parents,
                prepared: out.prepared,
            });
        }
        if !errors.is_empty() {
            return Err(PatchError::Invalid(errors));
        }
        Ok(Box::new(ChildRows::<R> {
            name: spec.name.to_string(),
            column: spec.column.to_string(),
            binding_key: spec.binding_key,
            dependent: spec.dependent,
            cascade_callbacks: spec.cascade_callbacks,
            link: false,
            save: spec.save,
            filter: spec.filter.clone(),
            single: false,
            rows: prepared,
        }))
    }
}

struct NestedRow<'a, R: GuardedRepo> {
    name: &'static str,
    prefix: String,
    // `_joinData` does not read the body's `id`.
    body_id: bool,
    current: Option<Key<R>>,
    seen: Option<&'a mut HashSet<Key<R>>>,
    delete_flag: bool,
    hidden_columns: &'a [&'a str],
    read_only_column: Option<&'a str>,
    scope: &'a NestedScope,
}

struct NestedOut<R: GuardedRepo> {
    id: Option<Key<R>>,
    delete: bool,
    am: <R::Entity as sea_orm::EntityTrait>::ActiveModel,
    parents: Vec<Box<dyn PreparedParent>>,
    prepared: Vec<Box<dyn PreparedChildren>>,
}
