//! Shared implementation of hasMany and hasOne (`HasOf<S, T, K, C>`); `C` is `Many` or `One`.

use std::any::Any;
use std::collections::HashMap;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, IdenStatic};
use serde_json::Value as Json;

use super::reference::{bulk_delete, bulk_purge, delete, ids_where, purge};
use super::util::{can_contain, can_view, current_children, owner_value, row_key, target_subquery};
use super::{
    Association, AssociationType, Cardinality, HasOf, NestedScope, RelationCondition, RelationInfo,
    RelationInput, RelationKind, SaveStrategy, prepare_fn,
};
use super::{check_binding_key_type, check_key_type, require_target};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::messages::Messages;
use crate::meta::column_named;
use crate::patch::{ChildSpec, PatchError, prepare_children, prepare_one};
use crate::registry::{EntityMeta, Registry};
use crate::repo::{GuardedRepo, Repo, RepoError, typed_contain};

mod sealed {
    pub trait Sealed {}
}

/// Marker for hasMany.
#[derive(Clone, Copy, Debug)]
pub struct Many;

/// Marker for hasOne.
#[derive(Clone, Copy, Debug)]
pub struct One;

impl sealed::Sealed for Many {}
impl sealed::Sealed for One {}

/// Number of targets of a `HasOf` declaration. Sealed: cannot be implemented outside cakedome.
pub trait Arity: sealed::Sealed + Copy + Send + Sync + 'static {
    const KIND: RelationKind;
    const CARDINALITY: Cardinality;
    const TYPE: AssociationType;
    const NAME: &'static str;

    /// Result shape of `Loadable::load`: parent value to children (`Many`) or to one row (`One`).
    type Grouped<K: RowKey, M: Send>: Send;

    /// Groups the rows `found` by parent value. `Many` seeds every parent with an empty list;
    /// `One` keeps the first row per parent.
    fn group<K: RowKey, M: Send>(
        parents: &[K],
        found: Vec<M>,
        key_of: impl Fn(&M) -> Option<K>,
    ) -> Self::Grouped<K, M>;

    fn rows_of<K: RowKey, M: Clone + Send>(grouped: &Self::Grouped<K, M>, key: &K) -> Vec<M>;

    /// Value stored in `Loaded`: `Vec<X>` for `Many`, `Option<X>` for `One`.
    type Value<X: Send + Sync + 'static>: Send + Sync + 'static;
    /// Type returned by `related`: `&[X]` for `Many`, `Option<&X>` for `One`.
    type Ref<'a, X: Send + Sync + 'static>;

    fn collect<X: Send + Sync + 'static>(rows: Vec<X>) -> Self::Value<X>;

    fn pick<X: Send + Sync + 'static>(value: &Self::Value<X>) -> Self::Ref<'_, X>;

    fn read_input<S: GuardedRepo, T: GuardedRepo, K: RowKey>(
        decl: &HasOf<S, T, K, Self>,
        value: &Json,
        errors: &mut FieldErrors,
        scope: &NestedScope,
        messages: &Messages,
    ) -> Result<RelationInput, PatchError>
    where
        <T::Entity as EntityTrait>::Column: Send + Sync;
}

impl Arity for Many {
    const KIND: RelationKind = RelationKind::HasMany;
    const CARDINALITY: Cardinality = Cardinality::Many;
    const TYPE: AssociationType = AssociationType::OneToMany;
    const NAME: &'static str = "HasManyOf";

    type Grouped<K: RowKey, M: Send> = HashMap<K, Vec<M>>;

    fn group<K: RowKey, M: Send>(
        parents: &[K],
        found: Vec<M>,
        key_of: impl Fn(&M) -> Option<K>,
    ) -> Self::Grouped<K, M> {
        let mut grouped: HashMap<K, Vec<M>> =
            parents.iter().map(|id| (id.clone(), Vec::new())).collect();
        for m in found {
            if let Some(parent) = key_of(&m)
                && let Some(list) = grouped.get_mut(&parent)
            {
                list.push(m);
            }
        }
        grouped
    }

    fn rows_of<K: RowKey, M: Clone + Send>(grouped: &Self::Grouped<K, M>, key: &K) -> Vec<M> {
        grouped.get(key).cloned().unwrap_or_default()
    }

    type Value<X: Send + Sync + 'static> = Vec<X>;
    type Ref<'a, X: Send + Sync + 'static> = &'a [X];

    fn collect<X: Send + Sync + 'static>(rows: Vec<X>) -> Self::Value<X> {
        rows
    }

    fn pick<X: Send + Sync + 'static>(value: &Self::Value<X>) -> Self::Ref<'_, X> {
        value
    }

    fn read_input<S: GuardedRepo, T: GuardedRepo, K: RowKey>(
        decl: &HasOf<S, T, K, Self>,
        value: &Json,
        errors: &mut FieldErrors,
        scope: &NestedScope,
        messages: &Messages,
    ) -> Result<RelationInput, PatchError>
    where
        <T::Entity as EntityTrait>::Column: Send + Sync,
    {
        let Some(strategy) = decl.save else {
            errors.add(decl.name, "read_only", &messages.text("read_only", &[]));
            return Ok(RelationInput::ReadOnly);
        };
        let bad = || PatchError::BadRequest(format!("{} is not an array of objects", decl.name));
        let rows = value
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|row| row.as_object().cloned().ok_or_else(bad))
            .collect::<Result<Vec<_>, _>>()?;
        let (name, column) = (decl.name, decl.foreign_key.as_str());
        let binding_key = Association::binding_key(decl);
        let (dependent, cascade_callbacks) = (decl.dependent, decl.cascade_callbacks);
        let filter = decl.relation_condition();
        let scope = scope.clone();
        Ok(RelationInput::Write(prepare_fn(move |ctx: &Context| {
            Box::pin(async move {
                let spec = ChildSpec {
                    name,
                    column,
                    binding_key,
                    dependent,
                    cascade_callbacks,
                    save: strategy,
                    filter,
                    scope,
                };
                prepare_children::<T>(ctx, &spec, &rows).await
            })
        })))
    }
}

impl Arity for One {
    const KIND: RelationKind = RelationKind::HasOne;
    const CARDINALITY: Cardinality = Cardinality::One;
    const TYPE: AssociationType = AssociationType::OneToOne;
    const NAME: &'static str = "HasOneOf";

    type Grouped<K: RowKey, M: Send> = HashMap<K, M>;

    fn group<K: RowKey, M: Send>(
        _parents: &[K],
        found: Vec<M>,
        key_of: impl Fn(&M) -> Option<K>,
    ) -> Self::Grouped<K, M> {
        let mut out: HashMap<K, M> = HashMap::new();
        for m in found {
            if let Some(parent) = key_of(&m) {
                out.entry(parent).or_insert(m);
            }
        }
        out
    }

    fn rows_of<K: RowKey, M: Clone + Send>(grouped: &Self::Grouped<K, M>, key: &K) -> Vec<M> {
        grouped.get(key).cloned().into_iter().collect()
    }

    type Value<X: Send + Sync + 'static> = Option<X>;
    type Ref<'a, X: Send + Sync + 'static> = Option<&'a X>;

    fn collect<X: Send + Sync + 'static>(rows: Vec<X>) -> Self::Value<X> {
        rows.into_iter().next()
    }

    fn pick<X: Send + Sync + 'static>(value: &Self::Value<X>) -> Self::Ref<'_, X> {
        value.as_ref()
    }

    /// The body value must be an object or null (null is ignored); otherwise `BadRequest`.
    /// Without `save` the association is read-only.
    fn read_input<S: GuardedRepo, T: GuardedRepo, K: RowKey>(
        decl: &HasOf<S, T, K, Self>,
        value: &Json,
        errors: &mut FieldErrors,
        scope: &NestedScope,
        messages: &Messages,
    ) -> Result<RelationInput, PatchError>
    where
        <T::Entity as EntityTrait>::Column: Send + Sync,
    {
        if decl.save.is_none() {
            errors.add(decl.name, "read_only", &messages.text("read_only", &[]));
            return Ok(RelationInput::ReadOnly);
        }
        if value.is_null() {
            return Ok(RelationInput::ReadOnly);
        }
        let row = value
            .as_object()
            .cloned()
            .ok_or_else(|| PatchError::BadRequest(format!("{} is not an object", decl.name)))?;
        let (name, column) = (decl.name, decl.foreign_key.as_str());
        let binding_key = Association::binding_key(decl);
        let filter = decl.relation_condition();
        let scope = scope.clone();
        let parent_column = decl.binding_column();
        let current_filter = filter.clone();
        Ok(RelationInput::Write(prepare_fn(move |ctx: &Context| {
            Box::pin(async move {
                // For an update, the current target is read by the parent's `bindingKey` value.
                let current = match scope.owner_id.as_ref().and_then(Key::<S>::from_row_id) {
                    Some(owner) => {
                        current_target::<S, T>(ctx, parent_column, owner, column, &current_filter)
                            .await?
                    }
                    None => None,
                };
                let spec = ChildSpec {
                    name,
                    column,
                    binding_key,
                    dependent: false,
                    cascade_callbacks: true,
                    save: SaveStrategy::Append,
                    filter,
                    scope,
                };
                prepare_one::<T>(ctx, &spec, &row, current).await
            })
        })))
    }
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Association for HasOf<S, T, K, C>
where
    <T::Entity as EntityTrait>::Column: Send + Sync,
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
        C::CARDINALITY
    }

    fn child_foreign_key(&self) -> Option<&'static str> {
        Some(self.foreign_key.as_str())
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
            kind: C::KIND,
            target: Association::target(self),
            through: None,
            foreign_key: self.foreign_key.as_str(),
            target_foreign_key: None,
            binding_key: Association::binding_key(self),
            target_binding_key: None,
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
        let target = require_target(self, owner, registry);
        let foreign_key = self.foreign_key.as_str();
        if !target.fields.iter().any(|f| f.name == foreign_key) {
            panic!(
                "{}: association {}: foreign key column {} does not exist on {}",
                owner.name,
                self.name,
                foreign_key,
                self.target()
            );
        }
        match self.binding_key {
            Some(binding) => check_binding_key_type(
                &target.name,
                foreign_key,
                target,
                binding.as_str(),
                owner,
                K::KIND,
            ),
            None => check_key_type(&target.name, foreign_key, target, owner.key),
        }
    }

    async fn usable(&self, ctx: &Context) -> Result<bool, RepoError> {
        Ok(can_view::<T>(ctx).await? && can_contain::<S>(ctx, self.name).await?)
    }

    fn association_type(&self) -> AssociationType {
        C::TYPE
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
        let sub = target_subquery::<T>(ctx, self.foreign_key.as_str(), condition).await?;
        Ok(Condition::all().add(self.binding_column().in_subquery(sub)))
    }

    fn typed_contain(&self) -> Box<dyn Any + Send + Sync> {
        typed_contain::<S, _>(*self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn read_input(
        &self,
        ctx: &Context,
        value: &Json,
        errors: &mut FieldErrors,
        scope: &NestedScope,
    ) -> Result<RelationInput, PatchError> {
        C::read_input(self, value, errors, scope, ctx.registry().messages())
    }

    async fn delete_dependents(
        &self,
        ctx: &Context,
        parent: &RowId,
        unchecked: bool,
        purge_all: bool,
    ) -> Result<(), RepoError> {
        let (filter, _) = self.association_query(ctx).await?;
        let ids = ids_where::<T>(ctx, self.foreign_key.as_str(), parent, filter, purge_all).await?;
        match (self.cascade_callbacks, purge_all) {
            (true, false) => {
                for child_id in ids {
                    delete::<T>(ctx, &child_id, unchecked).await?;
                }
            }
            (true, true) => {
                for child_id in ids {
                    purge::<T>(ctx, &child_id, unchecked).await?;
                }
            }
            (false, false) => bulk_delete::<T>(ctx, &ids).await?,
            (false, true) => bulk_purge::<T>(ctx, &ids).await?,
        }
        Ok(())
    }
}

async fn current_target<S: GuardedRepo, T: GuardedRepo>(
    ctx: &Context,
    parent_column: <S::Entity as EntityTrait>::Column,
    owner: Key<S>,
    foreign_key: &str,
    filter: &RelationCondition,
) -> Result<Option<RowId>, RepoError> {
    let Some(value) = owner_value::<S>(ctx, owner, parent_column).await? else {
        return Ok(None);
    };
    let fk = column_named::<T::Entity>(foreign_key)
        .ok_or_else(|| DbErr::Custom(format!("foreign key column {foreign_key} does not exist")))?;
    Ok(current_children::<T>(ctx, false, fk, &value, filter)
        .await?
        .first()
        .and_then(row_key::<T>)
        .map(RowKey::into_row_id))
}
