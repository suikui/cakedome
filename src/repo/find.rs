//! `Find`, a query builder that checks the Policy when `all` or `count` runs.

use std::any::Any;

use sea_orm::sea_query::{IntoCondition, SelectStatement};
use sea_orm::{Condition, Order, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait};

use super::before_find::run_before_find;
use super::guard::{authorize, base_condition};
use super::{ColumnOf, FindOptions, GuardedRepo, RepoEntity, RepoError};
use crate::context::Context;
use crate::policy::Action;

/// A query over a Repo's rows, created by `RepoHandle::find`.
///
/// Nothing is read until `all` or `count`, which check `can(View)` and add the scope (and
/// `is_active = TRUE` for soft delete). Callers cannot remove those conditions.
///
/// ```text
/// let rows = ctx.repo::<CustomersRepo>()
///     .find()
///     .filter(customers::Column::Name.contains("a"))
///     .order_by_asc(customers::Column::Name)
///     .all()
///     .await?;
/// ```
pub struct Find<'c, R: GuardedRepo> {
    pub(super) ctx: &'c Context,
    pub(super) unchecked: bool,
    pub(super) condition: Condition,
    pub(super) orders: Vec<(ColumnOf<R::Entity>, Order)>,
    pub(super) limit: Option<u64>,
    pub(super) offset: Option<u64>,
    pub(super) options: FindOptions,
    pub(super) primary: bool,
}

impl<'c, R, E> Find<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    /// Adds a condition with AND.
    pub fn filter(mut self, condition: impl IntoCondition) -> Self {
        self.condition = self.condition.add(condition.into_condition());
        self
    }

    /// Adds an ordering, in the order added. Anything other than `Desc` is treated as ascending.
    pub fn order_by(mut self, column: E::Column, order: Order) -> Self {
        let order = match order {
            Order::Desc => Order::Desc,
            _ => Order::Asc,
        };
        self.orders.push((column, order));
        self
    }

    /// Adds an ascending ordering.
    pub fn order_by_asc(self, column: E::Column) -> Self {
        self.order_by(column, Order::Asc)
    }

    /// Adds a descending ordering.
    pub fn order_by_desc(self, column: E::Column) -> Self {
        self.order_by(column, Order::Desc)
    }

    /// Passes a value to `before_find`, looked up by type. A value of the same type is replaced.
    pub fn option<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.options.insert(value);
        self
    }

    /// Marks this as a read of an association target, so `before_find` gets `primary = false`.
    pub fn for_association(mut self) -> Self {
        self.primary = false;
        self
    }

    /// Limits the number of rows.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skips rows, for paging. Used by `all` only, not `count`.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Returns the matching rows. Fails with `Forbidden` or `Unauthenticated` without `can(View)`.
    pub async fn all(self) -> Result<Vec<E::Model>, RepoError> {
        let guard = authorize::<R>(self.ctx, Action::View, self.unchecked).await?;
        let base = base_condition::<E>(self.ctx, &guard, Action::View).await?;
        let found = run_before_find::<R>(
            self.ctx,
            &guard.repo,
            Action::View,
            &self.options,
            self.primary,
        )
        .await?;
        let mut select = E::find()
            .filter(base)
            .filter(self.condition)
            .filter(found.condition);
        for (column, order) in self.orders.into_iter().chain(found.orders) {
            select = select.order_by(column, order);
        }
        if let Some(limit) = self.limit {
            select = select.limit(limit);
        }
        if let Some(offset) = self.offset {
            select = select.offset(offset);
        }
        Ok(select.all(&self.ctx.conn()).await?)
    }

    /// Reads with `association` contained. Set conditions, ordering and limits before this.
    pub fn contain<A: super::Containable<R>>(self, association: A) -> super::FindWith<'c, R> {
        super::FindWith::new(self).contain(association)
    }

    /// Reads with the associations of `default_contain` contained; `contain` can add more.
    pub fn contain_default(self) -> super::FindWith<'c, R> {
        super::FindWith::new(self).contain_default()
    }

    /// Counts the matching rows, under the same conditions as `all`.
    pub async fn count(self) -> Result<u64, RepoError> {
        let guard = authorize::<R>(self.ctx, Action::View, self.unchecked).await?;
        let base = base_condition::<E>(self.ctx, &guard, Action::View).await?;
        let found = run_before_find::<R>(
            self.ctx,
            &guard.repo,
            Action::View,
            &self.options,
            self.primary,
        )
        .await?;
        let count = E::find()
            .filter(base)
            .filter(self.condition)
            .filter(found.condition)
            .count(&self.ctx.conn())
            .await?;
        Ok(count)
    }

    /// Builds `SELECT column FROM E WHERE scope AND soft delete AND filter` without running it.
    ///
    /// Checks `can(View)` like `all`. Ordering, limit and offset are ignored. Used for
    /// `column IN (subquery)` filters across associations.
    pub async fn subquery(self, column: E::Column) -> Result<SelectStatement, RepoError> {
        let guard = authorize::<R>(self.ctx, Action::View, self.unchecked).await?;
        let base = base_condition::<E>(self.ctx, &guard, Action::View).await?;
        let found = run_before_find::<R>(
            self.ctx,
            &guard.repo,
            Action::View,
            &self.options,
            self.primary,
        )
        .await?;
        Ok(E::find()
            .filter(base)
            .filter(self.condition)
            .filter(found.condition)
            .select_only()
            .column(column)
            .into_query())
    }
}
