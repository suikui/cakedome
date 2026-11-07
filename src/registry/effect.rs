//! Declarations of what happens in the UI when a field changes (`RepoConfig::on_change`). Written
//! with the typed `Effect<E>`, stored type-erased as `OnChange` / `EffectMeta` in `EntityMeta`.

use crate::repo::GuardedRepo;
use std::marker::PhantomData;

use sea_orm::{EntityTrait, IdenStatic};

use crate::association::BelongsToOf;
use crate::repo::Repo;

/// A typed effect, built with `Effect::clear` / `Effect::copy` and passed to
/// `RepoConfig::on_change`.
pub struct Effect<E: EntityTrait> {
    pub(super) meta: EffectMeta,
    _entity: PhantomData<fn() -> E>,
}

impl<E: EntityTrait> Effect<E> {
    /// Clears `column` when the field changes.
    pub fn clear(column: E::Column) -> Self {
        Self {
            meta: EffectMeta::Clear {
                field: column.as_str(),
            },
            _entity: PhantomData,
        }
    }

    /// Copies values from the target of the belongsTo `relation` when the field changes.
    ///
    /// `map` is (destination own column, source target column). The foreign key of `relation` must
    /// be the `on_change` column, or `on_change` panics.
    pub fn copy<S, T>(
        relation: BelongsToOf<S, T>,
        map: impl IntoIterator<Item = (E::Column, <T::Entity as EntityTrait>::Column)>,
    ) -> Self
    where
        S: GuardedRepo<Entity = E>,
        T: GuardedRepo,
    {
        Self {
            meta: EffectMeta::Copy {
                relation: relation.name(),
                foreign_key: relation.foreign_key().as_str(),
                target: <T as Repo>::repo_name(),
                map: map
                    .into_iter()
                    .map(|(own, src)| (own.as_str(), src.as_str()))
                    .collect(),
            },
            _entity: PhantomData,
        }
    }
}

/// A type-erased effect.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectMeta {
    /// Clears `field`.
    Clear { field: &'static str },
    /// Copies values from the target (Repo name `target`) of `relation`; `map` is (destination,
    /// source).
    Copy {
        relation: &'static str,
        /// The foreign key column of the association, checked by `on_change`.
        foreign_key: &'static str,
        target: &'static str,
        map: Vec<(&'static str, &'static str)>,
    },
}

/// The effects declared on one column, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct OnChange {
    pub column: &'static str,
    pub effects: Vec<EffectMeta>,
}
