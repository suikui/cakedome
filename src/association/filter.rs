//! Declaration-time `filter`, `finder` and sort order of an association, and the
//! type-erased `RelationCondition` that ANDs the first two.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::{Condition, Order};

use super::typed::{Finder, LoadQuery};
use crate::context::Context;
use crate::repo::{GuardedRepo, RepoError};

type ConditionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Condition>, RepoError>> + Send + 'a>>;

trait ErasedFinder: Send + Sync {
    fn run<'a>(&'a self, ctx: &'a Context) -> ConditionFuture<'a>;
}

struct FinderCall<T: GuardedRepo>(Finder<T>);

impl<T: GuardedRepo> ErasedFinder for FinderCall<T> {
    fn run<'a>(&'a self, ctx: &'a Context) -> ConditionFuture<'a> {
        Box::pin(async move {
            let (condition, _) = (self.0)(ctx, LoadQuery::<T>::new()).await?.into_parts();
            Ok(Some(condition))
        })
    }
}

/// The `filter` and `finder` conditions of an association, type-erased.
///
/// The `finder` is async and takes a Context, so it runs at write time (also when `unchecked`).
#[derive(Clone, Default)]
pub struct RelationCondition {
    filter: Option<fn() -> Condition>,
    finder: Option<Arc<dyn ErasedFinder>>,
}

impl RelationCondition {
    pub(crate) fn of<T: GuardedRepo>(
        filter: Option<fn() -> Condition>,
        finder: Option<Finder<T>>,
    ) -> Self {
        Self {
            filter,
            finder: finder.map(|f| Arc::new(FinderCall::<T>(f)) as Arc<dyn ErasedFinder>),
        }
    }

    /// Builds `filter` AND `finder`, or `None` if neither is set. A `finder` error is returned.
    pub async fn condition(&self, ctx: &Context) -> Result<Option<Condition>, RepoError> {
        let mut out = self.filter.map(|f| f());
        if let Some(finder) = &self.finder {
            let found = finder.run(ctx).await?;
            out = match (out, found) {
                (Some(a), Some(b)) => Some(Condition::all().add(a).add(b)),
                (a, b) => a.or(b),
            };
        }
        Ok(out)
    }
}

impl fmt::Debug for RelationCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RelationCondition(..)")
    }
}

/// Sort direction of an association; a `Copy` enum so it can live in a const array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    pub(crate) fn order(self) -> Order {
        match self {
            SortOrder::Asc => Order::Asc,
            SortOrder::Desc => Order::Desc,
        }
    }
}
