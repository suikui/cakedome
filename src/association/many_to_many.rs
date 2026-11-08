use crate::repo::GuardedRepo;
use std::any::Any;
use std::collections::HashSet;
use std::marker::PhantomData;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DbErr, EntityTrait, ExprTrait, IdenStatic,
    ModelTrait, Value,
};
use serde_json::{Map, Value as Json};

use super::reference::{
    binding_values, bulk_delete, bulk_purge, delete, links_where, matching_by, purge, selectable,
};
use super::typed::require_id_key;
use super::util::{
    ColumnOf, ModelOf, can_contain, can_view, handle_of, owner_value, primary_column,
    require_parent, row_id_of, row_key, strip_same_values, target_subquery,
};
use super::{
    Association, AssociationType, Cardinality, PreparedChildren, RelationCondition, RelationInput,
    prepare_fn,
};
use super::{ManyToManyOf, NestedScope, RelationInfo, RelationKind, SaveStrategy, require_target};
use super::{check_binding_key_type, check_key_type};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::meta::column_named;
use crate::patch::{JoinSpec, PatchError, prepare_join};
use crate::registry::{EntityMeta, Registry};
use crate::repo::{Repo, RepoError, typed_contain};

/// many-to-many: points at several target rows through a join table, which holds both foreign
/// keys (`foreign_key` to this entity, `target_foreign_key` to the target). Viewing the
/// association needs view permission on the target and on `through`.
#[async_trait]
impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Association
    for ManyToManyOf<S, J, T, K, TK>
where
    <J::Entity as EntityTrait>::Column: Send + Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn target(&self) -> &'static str {
        <T as Repo>::repo_name()
    }

    fn label(&self) -> Option<&'static str> {
        self.label
    }

    fn cardinality(&self) -> Cardinality {
        Cardinality::Many
    }

    fn dependent(&self) -> bool {
        self.dependent
    }

    fn cascade_callbacks(&self) -> bool {
        self.cascade_callbacks
    }

    fn binding_key(&self) -> Option<&'static str> {
        self.binding_key.map(|c| c.as_str())
    }

    fn info(&self) -> RelationInfo {
        RelationInfo {
            name: self.name,
            kind: RelationKind::ManyToMany,
            target: Association::target(self),
            through: Some(self.through()),
            foreign_key: self.foreign_key.as_str(),
            target_foreign_key: Some(self.target_foreign_key.as_str()),
            binding_key: Association::binding_key(self),
            target_binding_key: self.target_binding_name(),
            label: self.label,
            save: self.save,
            filtered: self.filter.is_some(),
            finder: self.finder.is_some(),
            sort: self.sort_names(),
            dependent: self.dependent,
            cascade_callbacks: self.cascade_callbacks,
        }
    }

    fn verify(&self, owner: &EntityMeta, registry: &Registry) {
        registry.check_declared_repo::<S>(&owner.name, self.name, "own");
        registry.check_declared_repo::<T>(&owner.name, self.name, "target");
        registry.check_declared_repo::<J>(&owner.name, self.name, "join table");
        require_id_key::<S>(&owner.name, self.name, "parent");
        require_id_key::<T>(&owner.name, self.name, "target");
        let target = require_target(self, owner, registry);
        // Removal is covered by `Replace` and `unlink`, so `AppendWithDelete` is not allowed here.
        if self.save == Some(SaveStrategy::AppendWithDelete) {
            panic!(
                "{}: association {}: save strategy AppendWithDelete cannot be used with many-to-many (use Replace or Append; to remove, use Replace or unlink)",
                owner.name, self.name
            );
        }
        let through = self.through();
        let Some(middle) = registry.meta(through) else {
            panic!(
                "{}: association {}: join table {} is not registered",
                owner.name, self.name, through
            );
        };
        let foreign_key = self.foreign_key.as_str();
        let target_foreign_key = self.target_foreign_key.as_str();
        for column in [foreign_key, target_foreign_key] {
            if !middle.fields.iter().any(|f| f.name == column) {
                panic!(
                    "{}: association {}: foreign key column {} does not exist on {}",
                    owner.name, self.name, column, through
                );
            }
        }
        match self.binding_key {
            Some(binding) => check_binding_key_type(
                &middle.name,
                foreign_key,
                middle,
                binding.as_str(),
                owner,
                K::KIND,
            ),
            None => check_key_type(&middle.name, foreign_key, middle, owner.key),
        }
        match self.target_binding_key {
            Some(binding) => check_binding_key_type(
                &middle.name,
                target_foreign_key,
                middle,
                binding.as_str(),
                target,
                TK::KIND,
            ),
            None => check_key_type(&middle.name, target_foreign_key, middle, target.key),
        }
    }

    async fn usable(&self, ctx: &Context) -> Result<bool, RepoError> {
        Ok(can_view::<J>(ctx).await?
            && can_view::<T>(ctx).await?
            && can_contain::<S>(ctx, self.name).await?)
    }

    fn association_type(&self) -> AssociationType {
        AssociationType::ManyToMany
    }

    async fn matching_condition(
        &self,
        ctx: &Context,
        target: Condition,
    ) -> Result<Condition, RepoError> {
        let (filter, _) = self.association_query(ctx).await?;
        let mut condition = Condition::all().add(target);
        if let Some(filter) = filter {
            condition = condition.add(filter);
        }
        let target =
            target_subquery::<T>(ctx, self.target_binding_column().as_str(), condition).await?;
        let to_target = Condition::all()
            .add(Expr::col(Alias::new(self.target_foreign_key.as_str())).in_subquery(target));
        let links = target_subquery::<J>(ctx, self.foreign_key.as_str(), to_target).await?;
        Ok(Condition::all().add(self.binding_column().in_subquery(links)))
    }

    fn typed_contain(&self) -> Box<dyn Any + Send + Sync> {
        typed_contain::<S, _>(*self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Reads the body value: an array of target primary keys or `{"id": key, "_joinData": {..}}`
    /// objects (mixing is allowed).
    ///
    /// Returns `ReadOnly` unless `save` is `Replace` or `Append`, and `BadRequest` for a non-array
    /// value, an unreadable key, an object without `id`, or a non-object `_joinData`. Other keys
    /// give `unknown` and repeated ids give `duplicate`. `_joinData` is converted here as a row of
    /// the join table, deciding insert or update from the current pairs.
    fn read_input(
        &self,
        ctx: &Context,
        value: &Json,
        errors: &mut FieldErrors,
        scope: &NestedScope,
    ) -> Result<RelationInput, PatchError> {
        let add_only = match self.save {
            Some(SaveStrategy::Replace) => false,
            Some(SaveStrategy::Append) => true,
            _ => {
                errors.add(
                    self.name,
                    "read_only",
                    &ctx.registry().messages().text("read_only", &[]),
                );
                return Ok(RelationInput::ReadOnly);
            }
        };
        let bad = || {
            PatchError::BadRequest(format!(
                "{} is not an array of target primary keys or {{\"id\": …, \"_joinData\": …}}",
                self.name
            ))
        };
        let mut ids: Vec<RowId> = Vec::new();
        let mut join_inputs: Vec<Option<Map<String, Json>>> = Vec::new();
        for (index, item) in value.as_array().ok_or_else(bad)?.iter().enumerate() {
            let (id_value, join) = match item {
                Json::Object(object) => {
                    for key in object.keys().filter(|k| *k != "id" && *k != "_joinData") {
                        errors.add(
                            &format!("{}.{index}.{key}", self.name),
                            "unknown",
                            &ctx.registry().messages().text("unknown", &[]),
                        );
                    }
                    let join = match object.get("_joinData") {
                        None => None,
                        Some(Json::Object(join)) => Some(join.clone()),
                        Some(_) => return Err(bad()),
                    };
                    (object.get("id").ok_or_else(bad)?, join)
                }
                other => (other, None),
            };
            ids.push(
                Key::<T>::from_json(id_value)
                    .map(RowKey::into_row_id)
                    .ok_or_else(bad)?,
            );
            join_inputs.push(join);
        }
        let mut seen: HashSet<RowId> = HashSet::new();
        for (index, id) in ids.iter().enumerate() {
            if !seen.insert(id.clone()) {
                errors.add(
                    &format!("{}.{index}", self.name),
                    "duplicate",
                    &ctx.registry().messages().text("duplicate", &[]),
                );
            }
        }
        let spec = LinkSpec {
            name: self.name,
            parent_column: self.foreign_key.as_str(),
            target_column: self.target_foreign_key.as_str(),
            binding_key: Association::binding_key(self),
            target_binding_key: self.target_binding_name(),
            filter: self.relation_condition(),
        };
        let mut links = LinksOf::<J, T, TK>::new(&spec, &ids);
        links.add_only = add_only;
        if join_inputs.iter().all(Option::is_none) {
            // Without `_joinData` the join rows are not converted and no hooks run.
            return Ok(RelationInput::Write(prepare_fn(move |_ctx: &Context| {
                Box::pin(async move { Ok(Some(Box::new(links) as Box<dyn PreparedChildren>)) })
            })));
        }
        let (name, parent_column, target_column) =
            (self.name, spec.parent_column, spec.target_column);
        let (binding_key, target_binding_key) = (spec.binding_key, spec.target_binding_key);
        let filter = spec.filter;
        let owner_id = scope.owner_id.clone();
        let join_scope = scope.none();
        let own_binding = self.binding_column();
        Ok(RelationInput::Write(prepare_fn(move |ctx: &Context| {
            Box::pin(async move {
                // For an update the current pairs are read with the parent's `bindingKey` value, so
                // `_joinData` becomes an update or an insert, as `replace_links` decides.
                let spec = LinkSpec {
                    name,
                    parent_column,
                    target_column,
                    binding_key,
                    target_binding_key,
                    filter,
                };
                let parent_value = match owner_id.as_ref().and_then(Key::<S>::from_row_id) {
                    Some(owner) => owner_value::<S>(ctx, owner, own_binding).await?,
                    None => None,
                };
                let state = match parent_value {
                    Some(value) => {
                        Some(LinkState::<J>::load::<T, TK>(ctx, &spec, &value, &ids, false).await?)
                    }
                    None => None,
                };
                let mut joins = Vec::with_capacity(join_inputs.len());
                let mut join_errors = FieldErrors::default();
                for (index, input) in join_inputs.into_iter().enumerate() {
                    let Some(input) = input else {
                        joins.push(None);
                        continue;
                    };
                    let current = state
                        .as_ref()
                        .and_then(|s| s.existing(index, add_only))
                        .and_then(row_key::<J>)
                        .map(RowKey::into_row_id);
                    let join_spec = JoinSpec {
                        name,
                        index,
                        columns: [parent_column, target_column],
                        scope: join_scope.clone(),
                    };
                    match prepare_join::<J>(ctx, &join_spec, &input, current).await {
                        Ok(am) => joins.push(am),
                        Err(PatchError::Invalid(e)) => {
                            join_errors.merge(e);
                            joins.push(None);
                        }
                        Err(e) => return Err(e),
                    }
                }
                if !join_errors.is_empty() {
                    return Err(PatchError::Invalid(join_errors));
                }
                links.joins = joins;
                Ok(Some(Box::new(links) as Box<dyn PreparedChildren>))
            })
        })))
    }

    async fn delete_dependents(
        &self,
        ctx: &Context,
        parent: &RowId,
        unchecked: bool,
        purge_all: bool,
    ) -> Result<(), RepoError> {
        let links = links_where::<J>(
            ctx,
            self.foreign_key.as_str(),
            parent,
            self.target_foreign_key.as_str(),
            purge_all,
        )
        .await?;
        let mut ids: Vec<RowId> = links.iter().map(|(_, t)| t.clone()).collect();
        ids.sort_unstable();
        ids.dedup();
        let matched = match self.association_query(ctx).await?.0 {
            Some(condition) => Some(
                matching_by::<T, TK>(ctx, self.target_binding_column(), &ids, condition).await?,
            ),
            None => None,
        };
        let doomed: Vec<RowId> = links
            .into_iter()
            .filter(|(_, target_id)| matched.as_ref().is_none_or(|s| s.contains(target_id)))
            .map(|(link_id, _)| link_id)
            .collect();
        match (self.cascade_callbacks, purge_all) {
            (true, false) => {
                for link_id in doomed {
                    delete::<J>(ctx, &link_id, unchecked).await?;
                }
            }
            (true, true) => {
                for link_id in doomed {
                    purge::<J>(ctx, &link_id, unchecked).await?;
                }
            }
            (false, false) => bulk_delete::<J>(ctx, &doomed).await?,
            (false, true) => bulk_purge::<J>(ctx, &doomed).await?,
        }
        Ok(())
    }
}

pub(crate) fn link_columns<J: GuardedRepo>(
    name: &str,
    parent_column: &str,
    target_column: &str,
) -> Result<(ColumnOf<J>, ColumnOf<J>), RepoError> {
    let find = |c: &str| {
        column_named::<J::Entity>(c)
            .ok_or_else(|| DbErr::Custom(format!("{name}: foreign key column {c} does not exist")))
    };
    Ok((find(parent_column)?, find(target_column)?))
}

#[derive(Clone, Copy)]
pub(crate) struct LinkMode {
    pub(crate) unchecked: bool,
    pub(crate) add_only: bool,
}

/// The current pairs of a parent and how the sent targets map onto them. Shared by the
/// `_joinData` conversion and `replace_links` so both find the "current pair" the same way.
pub(crate) struct LinkState<J: GuardedRepo> {
    target: ColumnOf<J>,
    current: Vec<ModelOf<J>>,
    /// Per sent target, the value for the join table's target column: its `bindingKey` column if
    /// set (`None` if unreadable or null), else the sent id.
    link_values: Vec<Option<RowId>>,
    matched: Option<HashSet<RowId>>,
}

impl<J: GuardedRepo> LinkState<J> {
    pub(crate) async fn load<T: GuardedRepo, TK: RowKey>(
        ctx: &Context,
        spec: &LinkSpec<'_>,
        parent_value: &RowId,
        target_ids: &[RowId],
        unchecked: bool,
    ) -> Result<Self, RepoError> {
        let (parent, target) = link_columns::<J>(
            <J as Repo>::repo_name(),
            spec.parent_column,
            spec.target_column,
        )?;
        let target_column = match spec.target_binding_key {
            Some(name) => column_named::<T::Entity>(name).ok_or_else(|| {
                DbErr::Custom(format!("{}: column {name} does not exist", spec.name))
            })?,
            None => primary_column::<T>(),
        };
        let link_values: Vec<Option<RowId>> = if spec.target_binding_key.is_some() {
            let found = binding_values::<T>(ctx, target_column, target_ids).await?;
            target_ids.iter().map(|id| found.get(id).cloned()).collect()
        } else {
            target_ids.iter().cloned().map(Some).collect()
        };
        let current = handle_of::<J>(ctx, unchecked)
            .find()
            .for_association()
            .filter(parent.eq(Value::from(parent_value)))
            .all()
            .await?;
        // The condition applies also when `unchecked`, because it defines the association.
        let matched: Option<HashSet<RowId>> = match spec.filter.condition(ctx).await? {
            None => None,
            Some(condition) => {
                let mut ids: Vec<RowId> = current
                    .iter()
                    .filter_map(|m| row_id_of(m, target))
                    .collect();
                ids.extend(link_values.iter().flatten().cloned());
                ids.sort_unstable();
                ids.dedup();
                Some(matching_by::<T, TK>(ctx, target_column, &ids, condition).await?)
            }
        };
        Ok(Self {
            target,
            current,
            link_values,
            matched,
        })
    }

    fn in_association(&self, id: &RowId) -> bool {
        self.matched.as_ref().is_none_or(|m| m.contains(id))
    }

    /// The current pair for the sent target at `position`. A pair whose target fails the
    /// condition is outside the association and not current, except with `add_only`, which uses
    /// any pair of the target to avoid hitting a unique index.
    pub(crate) fn existing(&self, position: usize, add_only: bool) -> Option<&ModelOf<J>> {
        let target_id = self.link_values.get(position)?.as_ref()?;
        self.current.iter().find(|m| {
            row_id_of(*m, self.target).as_ref() == Some(target_id)
                && (add_only || self.in_association(target_id))
        })
    }
}

/// Replaces the pairs of the parent `parent_id` with `target_ids` (in sent order).
///
/// New ids must pass the target's `selectable`, else `name.N` gets `invalid_ref`. With
/// `spec.filter`, only pairs whose target matches are current (others are neither deleted nor
/// renumbered) and new targets must match too. Second-stage errors go to `errors` as
/// `name.N._joinData.field`. With `add_only` (`Append`, `link`) only targets without a pair are
/// added, the order column counts from the current maximum, and nothing is deleted.
pub(crate) async fn replace_links<J: GuardedRepo, T: GuardedRepo, TK: RowKey>(
    ctx: &Context,
    spec: &LinkSpec<'_>,
    parent_id: &RowId,
    target_ids: &[RowId],
    joins: Vec<Option<<J::Entity as EntityTrait>::ActiveModel>>,
    mode: LinkMode,
    errors: &mut FieldErrors,
) -> Result<bool, RepoError> {
    let LinkMode {
        unchecked,
        add_only,
    } = mode;
    let (parent, target) = link_columns::<J>(
        <J as Repo>::repo_name(),
        spec.parent_column,
        spec.target_column,
    )?;
    // A join table without an order column keeps no order; links are read in key order.
    let seq = ctx
        .registry()
        .spec_of::<J::Entity>()
        .order_column
        .and_then(column_named::<J::Entity>);
    let pk = primary_column::<J>();
    let state = LinkState::<J>::load::<T, TK>(ctx, spec, parent_id, target_ids, unchecked).await?;
    let mut joins = joins;
    joins.resize_with(target_ids.len(), || None);
    let mut changed = false;
    let mut next_seq = seq
        .and_then(|seq| state.current.iter().map(|m| seq_of(&m.get(seq))).max())
        .unwrap_or(0)
        .saturating_add(1);
    for (position, (sent_id, link_value)) in target_ids.iter().zip(&state.link_values).enumerate() {
        let key = |field: Option<&str>| match field {
            Some(f) => format!("{}.{position}.{f}", spec.name),
            None => format!("{}.{position}", spec.name),
        };
        let number = if add_only {
            next_seq
        } else {
            <i32 as TryFrom<usize>>::try_from(position + 1).unwrap_or(i32::MAX)
        };
        // An unreadable `bindingKey` value (no target, or null) cannot be linked.
        let Some(target_id) = link_value else {
            errors.add(
                &key(None),
                "invalid_ref",
                &ctx.registry().messages().text("invalid_ref", &[]),
            );
            continue;
        };
        let join = joins[position].take();
        let (am, adding) = match state.existing(position, add_only) {
            Some(now) => {
                let Some(id) = row_key::<J>(now) else {
                    continue;
                };
                let mut am = join.unwrap_or_else(|| {
                    <<J::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::default()
                });
                am.try_set(pk, id.into())?;
                let mut any = strip_same_values::<J>(&mut am, now);
                if let Some(seq) = seq
                    && !add_only
                    && now.get(seq) != Value::from(number)
                {
                    am.try_set(seq, Value::from(number))?;
                    any = true;
                }
                if !any {
                    continue;
                }
                (am, false)
            }
            None => {
                let selectable = if !state.in_association(target_id) {
                    false
                } else if unchecked {
                    true
                } else {
                    selectable::<T>(ctx, sent_id).await?
                };
                if !selectable {
                    errors.add(
                        &key(None),
                        "invalid_ref",
                        &ctx.registry().messages().text("invalid_ref", &[]),
                    );
                    continue;
                }
                let mut am = join.unwrap_or_else(|| {
                    <<J::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::default()
                });
                // The conversion may find no pair, so the primary key is not kept for the update.
                am.not_set(pk);
                am.try_set(parent, Value::from(parent_id))?;
                am.try_set(target, Value::from(target_id))?;
                if let Some(seq) = seq {
                    am.try_set(seq, Value::from(number))?;
                }
                (am, true)
            }
        };
        match handle_of::<J>(ctx, unchecked).save(am).await {
            Ok(_) => {
                changed = true;
                if adding {
                    next_seq = next_seq.saturating_add(1);
                }
            }
            Err(RepoError::Validation(e)) => {
                errors.merge_prefixed(&format!("{}.{position}._joinData", spec.name), e);
            }
            Err(e) => return Err(e),
        }
    }
    if add_only {
        return Ok(changed);
    }
    let sent: HashSet<&RowId> = state.link_values.iter().flatten().collect();
    for row in &state.current {
        let (Some(id), Some(target_id)) = (row_key::<J>(row), row_id_of(row, target)) else {
            continue;
        };
        if !sent.contains(&target_id) && state.in_association(&target_id) {
            handle_of::<J>(ctx, unchecked).delete(id).await?;
            changed = true;
        }
    }
    Ok(changed)
}

pub(crate) struct LinkSpec<'a> {
    pub(crate) name: &'a str,
    pub(crate) parent_column: &'a str,
    pub(crate) target_column: &'a str,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) target_binding_key: Option<&'static str>,
    pub(crate) filter: RelationCondition,
}

pub(crate) struct LinksOf<J: GuardedRepo, T: GuardedRepo, TK: RowKey> {
    pub(crate) name: String,
    pub(crate) parent_column: String,
    pub(crate) target_column: String,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) target_binding_key: Option<&'static str>,
    pub(crate) filter: RelationCondition,
    pub(crate) ids: Vec<RowId>,
    pub(crate) joins: Vec<Option<<J::Entity as EntityTrait>::ActiveModel>>,
    pub(crate) add_only: bool,
    _types: super::typed::Marker<(J, T, TK)>,
}

impl<J: GuardedRepo, T: GuardedRepo, TK: RowKey> LinksOf<J, T, TK> {
    pub(crate) fn new(spec: &LinkSpec<'_>, ids: &[RowId]) -> Self {
        Self {
            name: spec.name.to_string(),
            parent_column: spec.parent_column.to_string(),
            target_column: spec.target_column.to_string(),
            binding_key: spec.binding_key,
            target_binding_key: spec.target_binding_key,
            filter: spec.filter.clone(),
            ids: ids.to_vec(),
            joins: Vec::new(),
            add_only: false,
            _types: PhantomData,
        }
    }
}

#[async_trait]
impl<J: GuardedRepo, T: GuardedRepo, TK: RowKey> PreparedChildren for LinksOf<J, T, TK> {
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
        // A null parent `bindingKey` value means the pairs cannot connect, so nothing is saved.
        let Some(parent_id) = require_parent(
            tx.registry().messages(),
            parent_id,
            &self.name,
            self.binding_key,
            !self.ids.is_empty(),
            errors,
        ) else {
            return Ok(false);
        };
        let spec = LinkSpec {
            name: &self.name,
            parent_column: &self.parent_column,
            target_column: &self.target_column,
            binding_key: self.binding_key,
            target_binding_key: self.target_binding_key,
            filter: self.filter.clone(),
        };
        replace_links::<J, T, TK>(
            tx,
            &spec,
            &parent_id,
            &self.ids,
            self.joins,
            LinkMode {
                unchecked,
                add_only: self.add_only,
            },
            errors,
        )
        .await
    }
}

fn seq_of(value: &Value) -> i32 {
    match value {
        Value::Int(Some(v)) => *v,
        Value::BigInt(Some(v)) => i32::try_from(*v).unwrap_or(i32::MAX),
        Value::SmallInt(Some(v)) => i32::from(*v),
        Value::TinyInt(Some(v)) => i32::from(*v),
        _ => 0,
    }
}

pub(crate) struct UnlinkLinks<J: GuardedRepo, T: GuardedRepo> {
    pub(crate) name: String,
    pub(crate) parent_column: String,
    pub(crate) target_column: String,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) target_binding_key: Option<&'static str>,
    pub(crate) cascade_callbacks: bool,
    pub(crate) ids: Vec<RowId>,
    _types: PhantomData<fn() -> (J, T)>,
}

impl<J: GuardedRepo, T: GuardedRepo> UnlinkLinks<J, T> {
    pub(crate) fn new(spec: &LinkSpec<'_>, cascade_callbacks: bool, ids: &[RowId]) -> Self {
        Self {
            name: spec.name.to_string(),
            parent_column: spec.parent_column.to_string(),
            target_column: spec.target_column.to_string(),
            binding_key: spec.binding_key,
            target_binding_key: spec.target_binding_key,
            cascade_callbacks,
            ids: ids.to_vec(),
            _types: PhantomData,
        }
    }
}

#[async_trait]
impl<J: GuardedRepo, T: GuardedRepo> PreparedChildren for UnlinkLinks<J, T> {
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
        let Some(parent_id) = require_parent(
            tx.registry().messages(),
            parent_id,
            &self.name,
            self.binding_key,
            !self.ids.is_empty(),
            errors,
        ) else {
            return Ok(false);
        };
        let values: HashSet<RowId> = match self.target_binding_key {
            Some(name) => {
                let column = column_named::<T::Entity>(name).ok_or_else(|| {
                    DbErr::Custom(format!("{}: column {name} does not exist", self.name))
                })?;
                binding_values::<T>(tx, column, &self.ids)
                    .await?
                    .into_values()
                    .collect()
            }
            None => self.ids.iter().cloned().collect(),
        };
        let links = links_where::<J>(
            tx,
            &self.parent_column,
            &parent_id,
            &self.target_column,
            false,
        )
        .await?;
        let doomed: Vec<RowId> = links
            .into_iter()
            .filter(|(_, target_id)| values.contains(target_id))
            .map(|(link_id, _)| link_id)
            .collect();
        if doomed.is_empty() {
            return Ok(false);
        }
        if self.cascade_callbacks {
            for link_id in doomed {
                delete::<J>(tx, &link_id, unchecked).await?;
            }
        } else {
            bulk_delete::<J>(tx, &doomed).await?;
        }
        Ok(true)
    }
}
