//! Types passed to `Repo::before_find`, which adds conditions and ordering before rows are read.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::sea_query::IntoCondition;
use sea_orm::{Condition, EntityTrait, Order};

use super::{GuardedRepo, RepoError};
use crate::context::Context;
use crate::policy::Action;

/// Conditions and ordering that `before_find` adds to the query.
pub struct FindQuery<E: EntityTrait> {
    action: Action,
    // Kept apart and ANDed last so the caller's OR cannot swallow it.
    pub(super) condition: Condition,
    pub(super) orders: Vec<(E::Column, Order)>,
}

impl<E: EntityTrait> FindQuery<E> {
    pub(crate) fn new(action: Action) -> Self {
        Self {
            action,
            condition: Condition::all(),
            orders: Vec::new(),
        }
    }

    /// Returns why the rows are read: `View` for lists, counts, single rows and subqueries, `Edit`
    /// before an update or touch, `Delete` before a delete.
    pub fn action(&self) -> Action {
        self.action
    }

    /// Adds a condition with AND.
    pub fn filter(&mut self, condition: impl IntoCondition) -> &mut Self {
        self.condition = std::mem::replace(&mut self.condition, Condition::all())
            .add(condition.into_condition());
        self
    }

    /// Adds an ordering. Anything other than `Desc` is treated as ascending.
    pub fn order_by(&mut self, column: E::Column, order: Order) -> &mut Self {
        let order = match order {
            Order::Desc => Order::Desc,
            _ => Order::Asc,
        };
        self.orders.push((column, order));
        self
    }

    /// Adds an ascending ordering.
    pub fn order_by_asc(&mut self, column: E::Column) -> &mut Self {
        self.order_by(column, Order::Asc)
    }

    /// Adds a descending ordering.
    pub fn order_by_desc(&mut self, column: E::Column) -> &mut Self {
        self.order_by(column, Order::Desc)
    }
}

/// Values the caller passes to `before_find`, looked up by type.
#[derive(Clone, Default)]
pub struct FindOptions {
    values: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl FindOptions {
    /// Returns the value of type `T`, if one was passed.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.values
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    /// Returns whether a value of type `T` was passed.
    pub fn contains<T: Any + Send + Sync>(&self) -> bool {
        self.values.contains_key(&TypeId::of::<T>())
    }

    pub(crate) fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Arc::new(value));
    }
}

tokio::task_local! {
    // Not stored on Context, so concurrent reads on one Context are not mistaken for re-entry.
    static ACTIVE: Vec<TypeId>;
}

pub(super) async fn run_before_find<R: GuardedRepo>(
    ctx: &Context,
    repo: &R,
    action: Action,
    options: &FindOptions,
    primary: bool,
) -> Result<FindQuery<R::Entity>, RepoError> {
    let id = TypeId::of::<R::Entity>();
    let mut active = ACTIVE.try_with(|v| v.clone()).unwrap_or_default();
    if active.contains(&id) {
        return Err(RepoError::BeforeFindReentered(R::repo_name().to_string()));
    }
    active.push(id);
    let mut query = FindQuery::new(action);
    ACTIVE
        .scope(active, repo.before_find(ctx, &mut query, options, primary))
        .await?;
    Ok(query)
}
