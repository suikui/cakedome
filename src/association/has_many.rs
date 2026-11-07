use crate::repo::GuardedRepo;
use std::collections::HashSet;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, ModelTrait, Value};

use super::reference::{bulk_delete, bulk_nullify};
use super::util::{
    current_children, handle_of, primary_column, require_parent, row_key, strip_same_values,
};
use super::{PreparedChildren, PreparedParent, RelationCondition};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::meta::column_named;
use crate::repo::{Repo, RepoEntity, RepoError};

/// How a hasMany saves the children sent in the body. Without one, children are not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStrategy {
    /// The sent list is authoritative: missing children are removed, new ones are added.
    Replace,
    /// Adds and updates the sent children; never removes any.
    Append,
    /// Like `Append`, and removes children marked `_delete: true`.
    AppendWithDelete,
}

pub(crate) struct ChildRow<R: GuardedRepo> {
    pub(crate) index: usize,
    pub(crate) id: Option<Key<R>>,
    pub(crate) delete: bool,
    pub(crate) am: Option<<R::Entity as EntityTrait>::ActiveModel>,
    /// Written before the row.
    pub(crate) parents: Vec<Box<dyn PreparedParent>>,
    /// Written after the row.
    pub(crate) prepared: Vec<Box<dyn PreparedChildren>>,
}

pub(crate) struct ChildRows<R: GuardedRepo> {
    pub(crate) name: String,
    pub(crate) column: String,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) dependent: bool,
    pub(crate) cascade_callbacks: bool,
    /// Whether rows with an id are re-parented (`RepoHandle::link`) even if owned by another.
    pub(crate) link: bool,
    pub(crate) save: SaveStrategy,
    /// With a condition, the current children are those matching both the foreign key and it.
    pub(crate) filter: RelationCondition,
    /// A hasOne row: error keys carry no row index.
    pub(crate) single: bool,
    pub(crate) rows: Vec<ChildRow<R>>,
}

pub(crate) fn row_prefix(name: &str, index: usize, single: bool) -> String {
    if single {
        name.to_string()
    } else {
        format!("{name}.{index}")
    }
}

#[async_trait]
impl<R, E> PreparedChildren for ChildRows<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    fn parent_key(&self) -> Option<&'static str> {
        self.binding_key
    }

    async fn write(
        self: Box<Self>,
        tx: &Context,
        parent_id: Option<RowId>,
        unchecked: bool,
        errors: &mut FieldErrors,
    ) -> Result<bool, RepoError> {
        let ChildRows {
            name,
            column,
            binding_key,
            dependent,
            cascade_callbacks,
            link,
            save,
            filter,
            single,
            rows,
        } = *self;
        // A null parent `bindingKey` value means children cannot connect, so nothing is saved.
        let Some(parent_id) = require_parent(
            tx.registry().messages(),
            parent_id,
            &name,
            binding_key,
            !rows.is_empty(),
            errors,
        ) else {
            return Ok(false);
        };
        let fk = column_named::<E>(&column)
            .ok_or_else(|| DbErr::Custom(format!("foreign key column {column} does not exist")))?;
        let pk = primary_column::<R>();
        // With Replace and a seq column, rows are numbered from 1 in the order sent.
        let seq = if save == SaveStrategy::Replace {
            tx.registry()
                .spec_of::<E>()
                .order_column
                .and_then(column_named::<E>)
        } else {
            None
        };
        // Current children match the foreign key and the condition (also when `unchecked`).
        let needs_current =
            save == SaveStrategy::Replace || link || rows.iter().any(|r| r.id.is_some());
        let current = if needs_current {
            current_children::<R>(tx, unchecked, fk, &parent_id, &filter).await?
        } else {
            Vec::new()
        };
        let mut changed = false;
        let mut sent: HashSet<Key<R>> = HashSet::new();
        let mut marked: Vec<Key<R>> = Vec::new();
        for (position, row) in rows.into_iter().enumerate() {
            let prefix = row_prefix(&name, row.index, single);
            let key = |field: &str| format!("{prefix}.{field}");
            let number = <i32 as TryFrom<usize>>::try_from(position + 1).unwrap_or(i32::MAX);
            let (parents, prepared) = (row.parents, row.prepared);
            let adding = row.id.is_none();
            let (am, modified_row) = match row.id {
                Some(id) => {
                    let in_current = current
                        .iter()
                        .find(|m| row_key::<R>(m).as_ref() == Some(&id));
                    // `link` also re-parents rows that are not current children.
                    let fetched = if in_current.is_none() && link {
                        handle_of::<R>(tx, unchecked)
                            .find()
                            .for_association()
                            .filter(pk.eq(id.clone()))
                            .all()
                            .await?
                            .into_iter()
                            .next()
                    } else {
                        None
                    };
                    let Some(now) = in_current.or(fetched.as_ref()) else {
                        errors.add(
                            &key("id"),
                            "not_found",
                            &tx.registry().messages().text("not_found", &[]),
                        );
                        continue;
                    };
                    sent.insert(id.clone());
                    if row.delete {
                        marked.push(id);
                        continue;
                    }
                    let Some(mut am) = row.am else { continue };
                    am.try_set(pk, id.into())?;
                    if link {
                        am.try_set(fk, Value::from(&parent_id))?;
                    }
                    if let Some(seq) = seq {
                        am.try_set(seq, Value::from(number))?;
                    }
                    // Drop `Set`s equal to current values so unchanged rows are not written.
                    let any = strip_same_values::<R>(&mut am, now);
                    (am, any)
                }
                None => {
                    let Some(mut am) = row.am else { continue };
                    am.try_set(fk, Value::from(&parent_id))?;
                    if let Some(seq) = seq {
                        am.try_set(seq, Value::from(number))?;
                    }
                    (am, true)
                }
            };
            let saved = handle_of::<R>(tx, unchecked)
                .save_associated_inner(am, None, parents, prepared, adding)
                .await;
            match saved {
                Ok((_, nested_changed)) => changed |= modified_row || nested_changed,
                Err(RepoError::Validation(e)) => {
                    errors.merge_prefixed(&prefix, e);
                }
                Err(e) => return Err(e),
            }
        }
        let deletes: Vec<Key<R>> = match save {
            SaveStrategy::Replace => current
                .iter()
                .filter_map(row_key::<R>)
                .filter(|id| !sent.contains(id))
                .collect(),
            SaveStrategy::AppendWithDelete => marked,
            SaveStrategy::Append => Vec::new(),
        };
        let rule = save == SaveStrategy::Replace;
        remove_children::<R, E>(
            tx,
            unchecked,
            RemoveSpec {
                name: &name,
                column: &column,
                dependent,
                cascade_callbacks,
                replace_rule: rule,
            },
            deletes,
            &current,
            errors,
            changed,
        )
        .await
    }
}

pub(crate) struct RemoveSpec<'a> {
    pub(crate) name: &'a str,
    pub(crate) column: &'a str,
    pub(crate) dependent: bool,
    pub(crate) cascade_callbacks: bool,
    /// True for the Replace rule: NULL the foreign key if it is nullable and not `dependent`,
    /// else delete, one statement or per row by `cascade_callbacks`. False deletes row by row.
    pub(crate) replace_rule: bool,
}

/// Removes the children `deletes` per `replace_rule`; `current` supplies the NULL value type.
/// Returns true if `changed` was already true or anything was removed.
pub(crate) async fn remove_children<R, E>(
    tx: &Context,
    unchecked: bool,
    spec: RemoveSpec<'_>,
    deletes: Vec<Key<R>>,
    current: &[E::Model],
    errors: &mut FieldErrors,
    mut changed: bool,
) -> Result<bool, RepoError>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    let RemoveSpec {
        name,
        column,
        dependent,
        cascade_callbacks,
        replace_rule,
    } = spec;
    let fk = column_named::<E>(column)
        .ok_or_else(|| DbErr::Custom(format!("foreign key column {column} does not exist")))?;
    let pk = primary_column::<R>();
    // A Replace removal keeps the row with a NULL foreign key if the column is nullable and the
    // association is not `dependent`; otherwise (and for `_delete`) the row is deleted.
    let nullable = tx
        .registry()
        .meta(<R as Repo>::repo_name())
        .and_then(|m| m.fields.iter().find(|f| f.name == column))
        .is_some_and(|f| f.nullable);
    let nullify = replace_rule && !dependent && nullable;
    if deletes.is_empty() {
        return Ok(changed);
    }
    if replace_rule && !cascade_callbacks {
        let ids: Vec<RowId> = deletes.into_iter().map(RowKey::into_row_id).collect();
        if nullify {
            bulk_nullify::<R>(tx, fk, &ids).await?;
        } else {
            bulk_delete::<R>(tx, &ids).await?;
        }
        return Ok(true);
    }
    for id in deletes {
        if nullify {
            // Only the primary key and the NULL foreign key are `Set`, then saved via the child.
            let Some(now) = current
                .iter()
                .find(|m| row_key::<R>(m).as_ref() == Some(&id))
            else {
                continue;
            };
            let mut am = <E::ActiveModel as ActiveModelTrait>::default();
            am.try_set(pk, id.into())?;
            am.try_set(fk, now.get(fk).as_null())?;
            match handle_of::<R>(tx, unchecked).save(am).await {
                Ok(_) => {}
                Err(RepoError::Validation(e)) => errors.merge_prefixed(name, e),
                Err(e) => return Err(e),
            }
        } else {
            handle_of::<R>(tx, unchecked).delete(id).await?;
        }
        changed = true;
    }
    Ok(changed)
}

pub(crate) struct UnlinkChildren<R: GuardedRepo> {
    pub(crate) name: String,
    pub(crate) column: String,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) dependent: bool,
    pub(crate) cascade_callbacks: bool,
    pub(crate) filter: RelationCondition,
    pub(crate) ids: Vec<Key<R>>,
}

#[async_trait]
impl<R, E> PreparedChildren for UnlinkChildren<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    fn parent_key(&self) -> Option<&'static str> {
        self.binding_key
    }

    async fn write(
        self: Box<Self>,
        tx: &Context,
        parent_id: Option<RowId>,
        unchecked: bool,
        errors: &mut FieldErrors,
    ) -> Result<bool, RepoError> {
        let UnlinkChildren {
            name,
            column,
            binding_key,
            dependent,
            cascade_callbacks,
            filter,
            ids,
        } = *self;
        let Some(parent_id) = require_parent(
            tx.registry().messages(),
            parent_id,
            &name,
            binding_key,
            !ids.is_empty(),
            errors,
        ) else {
            return Ok(false);
        };
        let fk = column_named::<E>(&column)
            .ok_or_else(|| DbErr::Custom(format!("foreign key column {column} does not exist")))?;
        let current = current_children::<R>(tx, unchecked, fk, &parent_id, &filter).await?;
        // Keys that are not current children are ignored.
        let doomed: Vec<Key<R>> = current
            .iter()
            .filter_map(row_key::<R>)
            .filter(|id| ids.contains(id))
            .collect();
        remove_children::<R, E>(
            tx,
            unchecked,
            RemoveSpec {
                name: &name,
                column: &column,
                dependent,
                cascade_callbacks,
                replace_rule: true,
            },
            doomed,
            &current,
            errors,
            false,
        )
        .await
    }
}
