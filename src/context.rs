//! The entry point of Repo operations: who (`Identity`) and which DB (a pool or a transaction).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use std::ops::Deref;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    DatabaseConnection, DatabaseExecutor, DatabaseTransaction, DbErr, EntityTrait, TransactionTrait,
};

use crate::marshal::WriteMode;
use crate::policy::resolve::PolicyCache;
use crate::policy::{AuthzMark, Identity};
use crate::registry::Registry;
use crate::repo::{GuardedRepo, Repo, RepoError, RepoHandle};

#[async_trait]
trait Deferred: Send {
    async fn run(self: Box<Self>, ctx: &Context) -> Result<(), RepoError>;
}

struct SaveCommit<R: Repo> {
    repo: Arc<R>,
    model: <R::Entity as EntityTrait>::Model,
    mode: WriteMode,
}

#[async_trait]
impl<R: Repo> Deferred for SaveCommit<R> {
    async fn run(self: Box<Self>, ctx: &Context) -> Result<(), RepoError> {
        self.repo
            .after_save_commit(ctx, &self.model, self.mode)
            .await
    }
}

struct DeleteCommit<R: Repo> {
    repo: Arc<R>,
    model: <R::Entity as EntityTrait>::Model,
}

#[async_trait]
impl<R: Repo> Deferred for DeleteCommit<R> {
    async fn run(self: Box<Self>, ctx: &Context) -> Result<(), RepoError> {
        self.repo.after_delete_commit(ctx, &self.model).await
    }
}

type Pending = Arc<Mutex<Vec<Box<dyn Deferred>>>>;

enum Conn {
    Pool(DatabaseConnection),
    /// A transaction; every operation uses the same connection until commit or rollback.
    Tx(DatabaseTransaction),
}

/// Holds the identity, connection, authz mark and registry that Repo operations need.
///
/// Create one per request, then get a handle with `repo::<R>()`.
pub struct Context {
    identity: Arc<dyn Identity>,
    conn: Conn,
    /// The connection outside any transaction, used for the Context of after-commit hooks.
    pool: DatabaseConnection,
    /// Hooks to run after commit; `None` for a pool, where they run right after the save.
    pending: Option<Pending>,
    authz: AuthzMark,
    registry: Arc<Registry>,
    /// Shared with Contexts made by `begin`.
    policy_cache: Arc<PolicyCache>,
    extensions: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl Context {
    /// Creates a Context on a connection pool.
    pub fn new(
        identity: impl Identity,
        db: DatabaseConnection,
        authz: AuthzMark,
        registry: Arc<Registry>,
    ) -> Self {
        Self::new_shared(Arc::new(identity), db, authz, registry)
    }

    /// Like `new`, from an `Arc<dyn Identity>`.
    pub fn new_shared(
        identity: Arc<dyn Identity>,
        db: DatabaseConnection,
        authz: AuthzMark,
        registry: Arc<Registry>,
    ) -> Self {
        Self {
            identity,
            conn: Conn::Pool(db.clone()),
            pool: db,
            pending: None,
            authz,
            registry,
            policy_cache: Arc::default(),
            extensions: Arc::default(),
        }
    }

    /// Returns the Context with `value` added (a value of the same type is replaced).
    ///
    /// Set values when creating the Context. They are inherited by transactions from `begin` and by
    /// the Context of after-commit hooks. Adding a value starts a fresh Policy result cache, since
    /// Policies may read it; transactions begun earlier keep the previous values and cache.
    pub fn with_extension<T: Any + Send + Sync>(mut self, value: T) -> Self {
        Arc::make_mut(&mut self.extensions).insert(TypeId::of::<T>(), Arc::new(value));
        self.policy_cache = Arc::default();
        self
    }

    /// Returns the value of type `T` added with `with_extension`, if any.
    pub fn extension<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    /// Returns the identity of the request.
    pub fn identity(&self) -> &dyn Identity {
        &*self.identity
    }

    /// Returns the identity as the application's type, or `None` if it is another type.
    pub fn identity_as<T: Identity>(&self) -> Option<&T> {
        self.identity().downcast_ref::<T>()
    }

    /// Returns the connection (pool or transaction) that Repo operations use.
    pub fn conn(&self) -> DatabaseExecutor<'_> {
        match &self.conn {
            Conn::Pool(db) => DatabaseExecutor::Connection(db),
            Conn::Tx(tx) => DatabaseExecutor::Transaction(tx),
        }
    }

    /// Begins a transaction (a savepoint when already inside one).
    ///
    /// Identity, authz mark and registry are inherited. Dropping the `Transaction` without
    /// `commit` rolls back, including an early return through `?`.
    ///
    /// ```text
    /// let tx = ctx.begin().await?;
    /// tx.repo::<OrdersRepo>().save(order).await?;
    /// tx.repo::<OrderItemsRepo>().save(item).await?;
    /// tx.commit().await?;
    /// ```
    pub async fn begin(&self) -> Result<Transaction, DbErr> {
        let tx = self.conn().begin().await?;
        Ok(Transaction {
            ctx: Context {
                identity: Arc::clone(&self.identity),
                conn: Conn::Tx(tx),
                pool: self.pool.clone(),
                pending: Some(Pending::default()),
                authz: self.authz.clone(),
                registry: self.registry.clone(),
                policy_cache: Arc::clone(&self.policy_cache),
                extensions: Arc::clone(&self.extensions),
            },
            // A nested transaction hands its hooks to the outer one on commit.
            parent: self.pending.clone(),
        })
    }

    /// Runs the after-commit hook now outside a transaction, or holds it until commit inside one.
    pub(crate) async fn after_save_commit<R: Repo>(
        &self,
        repo: Arc<R>,
        model: &<R::Entity as EntityTrait>::Model,
        mode: WriteMode,
    ) {
        self.defer(Box::new(SaveCommit {
            repo,
            model: model.clone(),
            mode,
        }))
        .await;
    }

    pub(crate) async fn after_delete_commit<R: Repo>(
        &self,
        repo: Arc<R>,
        model: &<R::Entity as EntityTrait>::Model,
    ) {
        self.defer(Box::new(DeleteCommit {
            repo,
            model: model.clone(),
        }))
        .await;
    }

    async fn defer(&self, hook: Box<dyn Deferred>) {
        // A poisoned lock still holds usable data; do not panic here.
        match &self.pending {
            Some(pending) => pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(hook),
            None => run_hooks(self, vec![hook]).await,
        }
    }

    /// Returns the authz mark shared with the request.
    pub fn authz(&self) -> &AuthzMark {
        &self.authz
    }

    /// Returns the registry.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub(crate) fn policy_cache(&self) -> &PolicyCache {
        &self.policy_cache
    }

    /// Returns a handle for Repo `R`, the entry point to read and write entities.
    ///
    /// Every handle operation passes the Policy checks before touching the DB, and uses the
    /// transaction connection when called on a `Transaction`. Creating a handle never fails; the
    /// registry is checked by each operation, which returns `NotRegistered` if `R` is not
    /// registered (even with `unchecked`).
    ///
    /// ```text
    /// let c = ctx.repo::<CustomersRepo>().get(id).await?;
    /// ```
    pub fn repo<R: GuardedRepo>(&self) -> RepoHandle<'_, R> {
        RepoHandle::new(self)
    }
}

/// Runs the hooks in order. An `Err` is logged, not returned, and the rest still run (the save is
/// committed).
async fn run_hooks(ctx: &Context, hooks: Vec<Box<dyn Deferred>>) {
    for hook in hooks {
        if let Err(error) = hook.run(ctx).await {
            tracing::error!(%error, "after-commit hook failed");
        }
    }
}

/// A Context for an open transaction, usable as a `Context` through `Deref` (`tx.repo::<R>()`).
///
/// `commit` commits and `rollback` cancels; dropping it rolls back. After-commit hooks run after
/// the outermost commit, in save order. A nested commit hands them to the outer transaction and a
/// nested rollback discards them.
pub struct Transaction {
    ctx: Context,
    parent: Option<Pending>,
}

impl Transaction {
    /// Commits. At the outermost level, runs after-commit hooks afterwards (their `Err` is not
    /// returned).
    pub async fn commit(self) -> Result<(), DbErr> {
        let Transaction { ctx, parent } = self;
        let Context {
            identity,
            conn,
            pool,
            pending,
            authz,
            registry,
            policy_cache,
            extensions,
        } = ctx;
        let Conn::Tx(tx) = conn else {
            unreachable!("a Transaction always holds a transaction connection")
        };
        tx.commit().await?;
        let hooks = pending
            .map(|p| {
                std::mem::take(&mut *p.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            })
            .unwrap_or_default();
        match parent {
            Some(parent) => parent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(hooks),
            None => {
                let outside = Context {
                    identity,
                    conn: Conn::Pool(pool.clone()),
                    pool,
                    pending: None,
                    authz,
                    registry,
                    policy_cache,
                    extensions,
                };
                run_hooks(&outside, hooks).await;
            }
        }
        Ok(())
    }

    /// Rolls back without running after-commit hooks.
    pub async fn rollback(self) -> Result<(), DbErr> {
        match self.ctx.conn {
            Conn::Tx(tx) => tx.rollback().await,
            Conn::Pool(_) => unreachable!("a Transaction always holds a transaction connection"),
        }
    }

    /// Commits if `result` is `Ok`; otherwise rolls back and returns the error.
    /// A rollback failure is logged and the original error wins.
    ///
    /// ```text
    /// let tx = ctx.begin().await?;
    /// let result = tx.repo::<OrdersRepo>().save(order).await;
    /// tx.finish(result).await
    /// ```
    pub async fn finish<T, E: From<DbErr>>(self, result: Result<T, E>) -> Result<T, E> {
        match result {
            Ok(value) => {
                self.commit().await?;
                Ok(value)
            }
            Err(e) => {
                if let Err(error) = self.rollback().await {
                    tracing::error!(%error, "transaction rollback failed");
                }
                Err(e)
            }
        }
    }
}

impl Deref for Transaction {
    type Target = Context;

    fn deref(&self) -> &Context {
        &self.ctx
    }
}
