use crate::repo::GuardedRepo;
use std::any::Any;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::ValueType;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DbErr, EntityTrait, IdenStatic, ModelTrait,
};
use serde_json::Value as Json;

use super::reference::{selectable, selectable_by};
use super::util::{can_contain, can_view, own_column, target_subquery};
use super::util::{handle_of, primary_column};
use super::{
    Association, AssociationType, BelongsToOf, Cardinality, NestedScope, PreparedChildren,
    PreparedParent, RelationInfo, RelationInput, RelationKind, check_binding_key_type,
    check_key_type, prepare_parent_fn, require_target,
};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::meta::column_named;
use crate::patch::{PatchError, prepare_parent};
use crate::policy::resolve;
use crate::registry::{EntityMeta, Registry};
use crate::repo::{Repo, RepoEntity, RepoError, typed_contain};

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Association for BelongsToOf<S, T, K>
where
    <S::Entity as EntityTrait>::Column: Send + Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn target(&self) -> &'static str {
        <T as Repo>::repo_name()
    }

    fn cardinality(&self) -> Cardinality {
        Cardinality::One
    }

    fn owner_foreign_key(&self) -> Option<&'static str> {
        Some(self.foreign_key.as_str())
    }

    fn binding_key(&self) -> Option<&'static str> {
        self.binding_key.map(|c| c.as_str())
    }

    fn info(&self) -> RelationInfo {
        RelationInfo {
            name: self.name,
            kind: RelationKind::BelongsTo,
            target: Association::target(self),
            through: None,
            foreign_key: self.foreign_key.as_str(),
            target_foreign_key: None,
            binding_key: Association::binding_key(self),
            target_binding_key: None,
            label: None,
            save: None,
            filtered: false,
            finder: self.finder.is_some(),
            sort: Vec::new(),
            dependent: false,
            cascade_callbacks: true,
        }
    }

    async fn usable(&self, ctx: &Context) -> Result<bool, RepoError> {
        let column_readable = match ctx.registry().repo::<S>() {
            Some(source) => {
                let access = resolve::columns(ctx, &*source).await?;
                column_named::<S::Entity>(self.foreign_key.as_str()).is_some()
                    && access.can_read_named(self.foreign_key.as_str())
            }
            None => false,
        };
        Ok(
            column_readable
                && can_contain::<S>(ctx, self.name).await?
                && can_view::<T>(ctx).await?,
        )
    }

    fn verify(&self, owner: &EntityMeta, registry: &Registry) {
        let target = require_target(self, owner, registry);
        match self.binding_key {
            Some(binding) => check_binding_key_type(
                &owner.name,
                self.foreign_key.as_str(),
                owner,
                binding.as_str(),
                target,
                K::KIND,
            ),
            None => check_key_type(&owner.name, self.foreign_key.as_str(), owner, target.key),
        }
    }

    fn association_type(&self) -> AssociationType {
        AssociationType::ManyToOne
    }

    fn typed_contain(&self) -> Box<dyn Any + Send + Sync> {
        typed_contain::<S, _>(*self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn read_input(
        &self,
        _ctx: &Context,
        value: &Json,
        _errors: &mut FieldErrors,
        scope: &NestedScope,
    ) -> Result<RelationInput, PatchError> {
        if !self.save {
            return Ok(RelationInput::Ignored);
        }
        if value.is_null() {
            return Ok(RelationInput::ReadOnly);
        }
        let row = value
            .as_object()
            .cloned()
            .ok_or_else(|| PatchError::BadRequest(format!("{} is not an object", self.name)))?;
        let (name, column) = (self.name, self.foreign_key.as_str());
        let binding = Association::binding_key(self);
        let scope = scope.clone();
        Ok(RelationInput::WriteParent(prepare_parent_fn(
            move |ctx: &Context| {
                Box::pin(async move {
                    prepare_parent::<T>(ctx, name, column, binding, &row, scope).await
                })
            },
        )))
    }

    async fn check_exists(&self, ctx: &Context, id: &RowId) -> Result<bool, RepoError> {
        // The association `finder` condition applies here too.
        let (extra, _) = self.association_query(ctx).await?;
        match (self.binding_key, extra) {
            (None, None) => selectable::<T>(ctx, id).await,
            (None, extra) => {
                selectable_by::<T, Key<T>>(ctx, primary_column::<T>(), id, extra).await
            }
            (Some(binding), extra) => selectable_by::<T, K>(ctx, binding, id, extra).await,
        }
    }

    async fn matching_condition(
        &self,
        ctx: &Context,
        target: Condition,
    ) -> Result<Condition, RepoError> {
        let column = self.binding_column().as_str();
        let (extra, _) = self.association_query(ctx).await?;
        let mut condition = Condition::all().add(target);
        if let Some(extra) = extra {
            condition = condition.add(extra);
        }
        let sub = target_subquery::<T>(ctx, column, condition).await?;
        let foreign_key = own_column::<S>(self.foreign_key.as_str())?;
        Ok(Condition::all().add(foreign_key.in_subquery(sub)))
    }
}

pub(crate) struct ParentRow<T: GuardedRepo> {
    pub(crate) name: &'static str,
    pub(crate) foreign_key: &'static str,
    pub(crate) binding_key: Option<&'static str>,
    pub(crate) am: <T::Entity as EntityTrait>::ActiveModel,
    pub(crate) parents: Vec<Box<dyn PreparedParent>>,
    /// Written after the target row.
    pub(crate) prepared: Vec<Box<dyn PreparedChildren>>,
}

#[async_trait]
impl<T, E> PreparedParent for ParentRow<T>
where
    T: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    fn foreign_key(&self) -> &'static str {
        self.foreign_key
    }

    async fn write(
        self: Box<Self>,
        ctx: &Context,
        unchecked: bool,
        errors: &mut FieldErrors,
    ) -> Result<Option<sea_orm::Value>, RepoError> {
        let ParentRow {
            name,
            binding_key,
            am,
            parents,
            prepared,
            ..
        } = *self;
        let adding = !matches!(am.get(primary_column::<T>()), sea_orm::ActiveValue::Set(_));
        match handle_of::<T>(ctx, unchecked)
            .save_associated(am, None, parents, prepared, adding)
            .await
        {
            Ok(model) => {
                if let Some(binding) = binding_key {
                    let column = column_named::<E>(binding)
                        .ok_or_else(|| DbErr::Custom(format!("column {binding} does not exist")))?;
                    return Ok(Some(model.get(column)));
                }
                let key = <Key<T> as ValueType>::try_from(model.get(primary_column::<T>()))
                    .map_err(|_| DbErr::Custom("cannot read the primary key".to_string()))?;
                Ok(Some(sea_orm::Value::from(&key.into_row_id())))
            }
            Err(RepoError::Validation(e)) => {
                errors.merge_prefixed(name, e);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
