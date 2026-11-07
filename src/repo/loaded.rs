//! `Loaded`, a row with its contained associations, and `Related` for typed access to them.

use crate::repo::GuardedRepo;
use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;

use sea_orm::DbErr;
use serde_json::Value as Json;

use super::RepoError;
use crate::association::{Arity, BelongsToOf, HasOf, ManyToManyOf, ModelOf};
use crate::context::Context;
use crate::key::RowKey;

type Value = Box<dyn Any + Send + Sync>;

pub(crate) type JoinJson =
    for<'a> fn(
        &'a Context,
        &'a [&'a (dyn Any + Send + Sync)],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Json>, RepoError>> + Send + 'a>>;

/// A row with the associations read by `contain`. Derefs to the Model.
pub struct Loaded<M> {
    model: M,
    related: HashMap<&'static str, Value>,
    // The join row (type erased; a `J` Model) of the link a many-to-many contain read.
    join: Option<(Value, JoinJson)>,
}

impl<M> Loaded<M> {
    pub(crate) fn new(model: M) -> Self {
        Self {
            model,
            related: HashMap::new(),
            join: None,
        }
    }

    pub(crate) fn with_join(mut self, join: Value, json: JoinJson) -> Self {
        self.join = Some((join, json));
        self
    }

    pub(crate) fn join_raw(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.join.as_ref().map(|(row, _)| &**row)
    }

    pub(crate) fn join_json(&self) -> Option<JoinJson> {
        self.join.as_ref().map(|(_, json)| *json)
    }

    pub(crate) fn attach(&mut self, name: &'static str, value: Value) {
        self.related.insert(name, value);
    }

    pub(crate) fn related_raw(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        self.related.get(name).map(|v| &**v)
    }

    /// Returns the Model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Consumes the `Loaded` and returns the Model.
    pub fn into_model(self) -> M {
        self.model
    }

    /// Returns the join-table row of the link through which this target row was read.
    ///
    /// Returns `None` for a row not read through a many-to-many contain. `_association` only fixes
    /// `J`.
    pub fn join_data<S, J, T, K, TK>(
        &self,
        _association: ManyToManyOf<S, J, T, K, TK>,
    ) -> Option<&ModelOf<J>>
    where
        S: GuardedRepo,
        J: GuardedRepo,
        T: GuardedRepo,
        K: RowKey,
        TK: RowKey,
        M: Is<ModelOf<T>>,
    {
        self.join.as_ref()?.0.downcast_ref::<ModelOf<J>>()
    }

    /// Returns the contained association, or `NotLoaded` if it was not contained.
    pub fn related<A: Related<M>>(&self, association: A) -> Result<A::Ref<'_>, NotLoaded> {
        let name = association.name();
        self.related
            .get(name)
            .and_then(|v| v.downcast_ref::<A::Value>())
            .map(A::pick)
            .ok_or(NotLoaded { name })
    }
}

impl<M> From<M> for Loaded<M> {
    fn from(model: M) -> Self {
        Self::new(model)
    }
}

impl<M> Deref for Loaded<M> {
    type Target = M;

    fn deref(&self) -> &M {
        &self.model
    }
}

impl<M: fmt::Debug> fmt::Debug for Loaded<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&&str> = self.related.keys().collect();
        names.sort();
        f.debug_struct("Loaded")
            .field("model", &self.model)
            .field("related", &names)
            .finish()
    }
}

/// Marker for type equality; limits `join_data` to rows of the target `T`.
pub trait Is<X> {}

impl<X> Is<X> for X {}

/// Error from `related` for an association that was not contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotLoaded {
    /// The association name.
    pub name: &'static str,
}

impl fmt::Display for NotLoaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "association {} was not loaded (not contained)",
            self.name
        )
    }
}

impl std::error::Error for NotLoaded {}

impl From<NotLoaded> for RepoError {
    fn from(e: NotLoaded) -> Self {
        RepoError::Db(DbErr::Custom(e.to_string()))
    }
}

/// Maps an association declaration to the type stored in `Loaded` and the type `related` returns.
pub trait Related<M> {
    type Value: Send + Sync + 'static;
    type Ref<'a>;

    fn name(&self) -> &'static str;

    /// Converts the stored value to the type `related` returns.
    fn pick(value: &Self::Value) -> Self::Ref<'_>;
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Related<ModelOf<S>> for BelongsToOf<S, T, K> {
    type Value = Option<Loaded<ModelOf<T>>>;
    type Ref<'a> = Option<&'a Loaded<ModelOf<T>>>;

    fn name(&self) -> &'static str {
        BelongsToOf::name(self)
    }

    fn pick(value: &Self::Value) -> Self::Ref<'_> {
        value.as_ref()
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Related<ModelOf<S>>
    for HasOf<S, T, K, C>
{
    type Value = C::Value<Loaded<ModelOf<T>>>;
    type Ref<'a> = C::Ref<'a, Loaded<ModelOf<T>>>;

    fn name(&self) -> &'static str {
        HasOf::name(self)
    }

    fn pick(value: &Self::Value) -> Self::Ref<'_> {
        C::pick(value)
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Related<ModelOf<S>>
    for ManyToManyOf<S, J, T, K, TK>
{
    type Value = Vec<Loaded<ModelOf<T>>>;
    type Ref<'a> = &'a [Loaded<ModelOf<T>>];

    fn name(&self) -> &'static str {
        ManyToManyOf::name(self)
    }

    fn pick(value: &Self::Value) -> Self::Ref<'_> {
        value
    }
}
