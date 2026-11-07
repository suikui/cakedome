//! `Find::contain` and `FindWith`: read rows, then load and attach their associations.

use crate::repo::GuardedRepo;
use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{DbErr, EntityTrait};
use serde_json::Value as Json;

use std::marker::PhantomData;

use super::{Find, Loaded, Related, Repo, RepoEntity, RepoError, RepoHandle};
use crate::association::Association;
use crate::association::{
    Arity, BelongsToOf, HasOf, LoadQuery, Loadable, ManyToManyOf, ModelOf, handle_of, key_value,
};
use crate::context::Context;
use crate::json::{JsonFor, JsonTarget, json_join_of};
use crate::key::{Key, RowId, RowKey};
use crate::registry::Registry;

/// Maximum depth of a contain path (`items.product` is 2).
pub const MAX_CONTAIN_DEPTH: usize = 3;
/// Maximum number of contain paths.
pub const MAX_CONTAIN_PATHS: usize = 20;

type Value = Box<dyn Any + Send + Sync>;

/// An association declaration that `contain` accepts.
///
/// Values are built in the order `fetch`, `fetch_loaded`, `assemble`. A nested contain (`With`)
/// hands the target rows of all parents to its children between `fetch_loaded` and `assemble`.
#[async_trait]
pub trait Containable<R: GuardedRepo>: Loadable<R> + Related<ModelOf<R>> + Clone + 'static {
    /// Returns the target Models per row, in the order of `rows` (belongsTo has zero or one).
    async fn fetch(
        self,
        handle: &RepoHandle<'_, R>,
        rows: &[ModelOf<R>],
        query: LoadQuery<Self::Target>,
    ) -> Result<Vec<Vec<ModelOf<Self::Target>>>, RepoError>;

    /// Returns the target `Loaded` per row. The default wraps the `fetch` Models; many-to-many
    /// overrides it to attach the join row (`Loaded::join_data`).
    async fn fetch_loaded(
        self,
        handle: &RepoHandle<'_, R>,
        rows: &[ModelOf<R>],
        query: LoadQuery<Self::Target>,
    ) -> Result<Vec<Vec<Loaded<ModelOf<Self::Target>>>>, RepoError> {
        Ok(self
            .fetch(handle, rows, query)
            .await?
            .into_iter()
            .map(wrap)
            .collect())
    }

    /// Boxes the target `Loaded` rows as `Related::Value` per row.
    ///
    /// The shape follows `Association::cardinality()`: `Option<Loaded<T>>` for `One`,
    /// `Vec<Loaded<T>>` for `Many`. A mismatch is a `Db` error.
    fn assemble(rows: Vec<Vec<Loaded<ModelOf<Self::Target>>>>) -> Vec<Value>;

    /// Checks for duplicate child names before any read.
    fn validate(&self) -> Result<(), RepoError> {
        Ok(())
    }

    /// Returns the boxed `Related::Value` per row, in the order of `rows`.
    async fn values(
        self,
        handle: &RepoHandle<'_, R>,
        rows: &[ModelOf<R>],
    ) -> Result<Vec<Value>, RepoError> {
        let targets = self.fetch_loaded(handle, rows, LoadQuery::new()).await?;
        Ok(Self::assemble(targets))
    }
}

fn wrap<M>(models: Vec<M>) -> Vec<Loaded<M>> {
    models.into_iter().map(Loaded::new).collect()
}

fn boxed<V: Send + Sync + 'static>(v: V) -> Value {
    Box::new(v)
}

fn assemble_many<M: Send + Sync + 'static>(rows: Vec<Vec<Loaded<M>>>) -> Vec<Value> {
    rows.into_iter().map(boxed).collect()
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Containable<S> for BelongsToOf<S, T, K>
where
    <S::Entity as EntityTrait>::Column: Send + Sync,
{
    async fn fetch(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<ModelOf<T>>>, RepoError> {
        let column = self.foreign_key();
        let keys: Vec<Option<K>> = rows.iter().map(|r| key_value(r, column)).collect();
        let found = self.load(handle, rows, query).await?;
        Ok(keys
            .into_iter()
            .map(|key| {
                key.and_then(|k| found.get(&k))
                    .cloned()
                    .into_iter()
                    .collect()
            })
            .collect())
    }

    fn assemble(rows: Vec<Vec<Loaded<ModelOf<T>>>>) -> Vec<Value> {
        rows.into_iter()
            .map(|r| boxed(r.into_iter().next()))
            .collect()
    }
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Containable<S> for HasOf<S, T, K, C>
where
    <T::Entity as EntityTrait>::Column: Send + Sync,
{
    async fn fetch(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<ModelOf<T>>>, RepoError> {
        let column = self.binding_column();
        let keys: Vec<Option<K>> = rows.iter().map(|r| key_value(r, column)).collect();
        let found = self.load(handle, rows, query).await?;
        Ok(keys
            .into_iter()
            .map(|key| key.map(|k| C::rows_of(&found, &k)).unwrap_or_default())
            .collect())
    }

    fn assemble(rows: Vec<Vec<Loaded<ModelOf<T>>>>) -> Vec<Value> {
        rows.into_iter().map(|r| boxed(C::collect(r))).collect()
    }
}

#[async_trait]
impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Containable<S>
    for ManyToManyOf<S, J, T, K, TK>
where
    <J::Entity as EntityTrait>::Column: Send + Sync,
{
    async fn fetch(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<ModelOf<T>>>, RepoError> {
        let column = self.binding_column();
        let keys: Vec<Option<K>> = rows.iter().map(|r| key_value(r, column)).collect();
        let found = self.load(handle, rows, query).await?;
        Ok(keys
            .into_iter()
            .map(|key| key.and_then(|k| found.get(&k)).cloned().unwrap_or_default())
            .collect())
    }

    async fn fetch_loaded(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<Loaded<ModelOf<T>>>>, RepoError> {
        let column = self.binding_column();
        let keys: Vec<Option<K>> = rows.iter().map(|r| key_value(r, column)).collect();
        let found = self.load_linked(handle, rows, query).await?;
        Ok(keys
            .into_iter()
            .map(|key| {
                key.and_then(|k| found.get(&k))
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(target, link)| {
                        Loaded::new(target).with_join(Box::new(link), json_join_of::<J>)
                    })
                    .collect()
            })
            .collect())
    }

    fn assemble(rows: Vec<Vec<Loaded<ModelOf<T>>>>) -> Vec<Value> {
        assemble_many(rows)
    }
}

#[async_trait]
pub(crate) trait ErasedContain<R: GuardedRepo>: Send + Sync {
    fn name(&self) -> &'static str;

    fn validate(&self) -> Result<(), RepoError>;

    async fn values(
        &self,
        handle: &RepoHandle<'_, R>,
        rows: &[ModelOf<R>],
    ) -> Result<Vec<Value>, RepoError>;
}

pub(crate) struct Slot<A>(A);

#[async_trait]
impl<R: GuardedRepo, A: Containable<R>> ErasedContain<R> for Slot<A> {
    fn name(&self) -> &'static str {
        Related::<ModelOf<R>>::name(&self.0)
    }

    fn validate(&self) -> Result<(), RepoError> {
        Containable::<R>::validate(&self.0)
    }

    async fn values(
        &self,
        handle: &RepoHandle<'_, R>,
        rows: &[ModelOf<R>],
    ) -> Result<Vec<Value>, RepoError> {
        Containable::values(self.0.clone(), handle, rows).await
    }
}

fn validate_all<R: GuardedRepo>(contains: &[Arc<dyn ErasedContain<R>>]) -> Result<(), RepoError> {
    let mut seen = HashSet::new();
    for c in contains {
        if !seen.insert(c.name()) {
            return Err(RepoError::Db(DbErr::Custom(format!(
                "association {} is contained twice",
                c.name()
            ))));
        }
        c.validate()?;
    }
    Ok(())
}

// Keeps the join rows already on `rows`.
async fn load_all<R: GuardedRepo>(
    handle: &RepoHandle<'_, R>,
    contains: &[Arc<dyn ErasedContain<R>>],
    rows: Vec<Loaded<ModelOf<R>>>,
) -> Result<Vec<Loaded<ModelOf<R>>>, RepoError> {
    let models: Vec<ModelOf<R>> = rows.iter().map(|l| l.model().clone()).collect();
    let mut columns = Vec::new();
    for c in contains {
        columns.push((c.name(), c.values(handle, &models).await?.into_iter()));
    }
    Ok(rows
        .into_iter()
        .map(|mut loaded| {
            for (name, values) in &mut columns {
                if let Some(v) = values.next() {
                    loaded.attach(name, v);
                }
            }
            loaded
        })
        .collect())
}

/// A nested contain, made with `association.with(child)`.
///
/// Read it with the parent's declaration (`related(OrdersRepo::ITEMS)`); read children with
/// `related` on the target `Loaded`.
pub struct With<A, T: GuardedRepo> {
    inner: A,
    children: Vec<Arc<dyn ErasedContain<T>>>,
    query: LoadQuery<T>,
}

impl<A: Clone, T: GuardedRepo> Clone for With<A, T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            children: self.children.clone(),
            query: self.query.clone(),
        }
    }
}

impl<A, T: GuardedRepo> With<A, T> {
    /// Adds conditions (ANDed) and ordering (appended) for the target, as `load_where` does.
    pub fn query(mut self, f: impl FnOnce(LoadQuery<T>) -> LoadQuery<T>) -> Self {
        self.query = f(self.query);
        self
    }

    /// Adds a child contain.
    pub fn with<B: Containable<T>>(mut self, child: B) -> Self {
        self.children.push(Arc::new(Slot(child)));
        self
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>
    ManyToManyOf<S, J, T, K, TK>
{
    /// Contains an association of the target rows, nested.
    pub fn with<B: Containable<T>>(self, child: B) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: LoadQuery::new(),
        }
        .with(child)
    }

    /// Adds conditions and ordering for the target, as `load_where` does.
    pub fn query(self, f: impl FnOnce(LoadQuery<T>) -> LoadQuery<T>) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: f(LoadQuery::new()),
        }
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> HasOf<S, T, K, C> {
    /// Contains an association of the target rows, nested.
    pub fn with<B: Containable<T>>(self, child: B) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: LoadQuery::new(),
        }
        .with(child)
    }

    /// Adds conditions and ordering for the target, as `load_where` does.
    pub fn query(self, f: impl FnOnce(LoadQuery<T>) -> LoadQuery<T>) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: f(LoadQuery::new()),
        }
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> BelongsToOf<S, T, K> {
    /// Contains an association of the target rows, nested.
    pub fn with<B: Containable<T>>(self, child: B) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: LoadQuery::new(),
        }
        .with(child)
    }

    /// Adds conditions and ordering for the target, as `load_where` does.
    pub fn query(self, f: impl FnOnce(LoadQuery<T>) -> LoadQuery<T>) -> With<Self, T> {
        With {
            inner: self,
            children: Vec::new(),
            query: f(LoadQuery::new()),
        }
    }
}

impl<M, A: Related<M>, T: GuardedRepo> Related<M> for With<A, T> {
    type Value = A::Value;
    type Ref<'a> = A::Ref<'a>;

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn pick(value: &Self::Value) -> Self::Ref<'_> {
        A::pick(value)
    }
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, A: Loadable<S, Target = T>> Loadable<S> for With<A, T>
where
    <T::Entity as EntityTrait>::Column: Send + Sync,
{
    type Output = A::Output;
    type Target = T;

    async fn load(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Self::Output, RepoError> {
        // `load` returns only target Models, so `.with()` children are not read; use `contain`.
        self.inner.load(handle, rows, self.query.merge(query)).await
    }
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, A> Containable<S> for With<A, T>
where
    A: Containable<S> + Loadable<S, Target = T>,
    <T::Entity as EntityTrait>::Column: Send + Sync,
{
    async fn fetch(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<ModelOf<T>>>, RepoError> {
        self.inner
            .fetch(handle, rows, self.query.merge(query))
            .await
    }

    async fn fetch_loaded(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Vec<Vec<Loaded<ModelOf<T>>>>, RepoError> {
        self.inner
            .fetch_loaded(handle, rows, self.query.merge(query))
            .await
    }

    fn assemble(rows: Vec<Vec<Loaded<ModelOf<T>>>>) -> Vec<Value> {
        A::assemble(rows)
    }

    fn validate(&self) -> Result<(), RepoError> {
        self.inner.validate()?;
        validate_all(&self.children)
    }

    async fn values(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
    ) -> Result<Vec<Value>, RepoError> {
        let per_row = self.inner.fetch_loaded(handle, rows, self.query).await?;
        let sizes: Vec<usize> = per_row.iter().map(Vec::len).collect();
        let models: Vec<Loaded<ModelOf<T>>> = per_row.into_iter().flatten().collect();
        let flat = if models.is_empty() {
            Vec::new()
        } else {
            let child_handle = handle_of::<T>(handle.context(), handle.is_unchecked());
            load_all(&child_handle, &self.children, models).await?
        };
        let mut flat = flat.into_iter();
        let grouped = sizes
            .into_iter()
            .map(|n| flat.by_ref().take(n).collect())
            .collect();
        Ok(A::assemble(grouped))
    }
}

/// A `Find` with associations contained; `all` reads the rows and the associations.
pub struct FindWith<'c, R: GuardedRepo> {
    find: Find<'c, R>,
    contains: Vec<Arc<dyn ErasedContain<R>>>,
    defaults: bool,
}

impl<'c, R, E> FindWith<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    pub(super) fn new(find: Find<'c, R>) -> Self {
        Self {
            find,
            contains: Vec::new(),
            defaults: false,
        }
    }

    /// Adds the associations of `default_contain`, silently dropping levels the caller cannot view
    /// (not dropped when `unchecked`). `all` fails with `RepoError::Db` if a name is also contained
    /// explicitly.
    pub fn contain_default(mut self) -> Self {
        self.defaults = true;
        self
    }

    /// Adds an association to contain.
    pub fn contain<A: Containable<R>>(mut self, association: A) -> Self {
        self.contains.push(Arc::new(Slot(association)));
        self
    }

    /// Reads the rows (with the checks of `Find::all`) and attaches each contained association.
    pub async fn all(self) -> Result<Vec<Loaded<E::Model>>, RepoError> {
        let ctx = self.find.ctx;
        let unchecked = self.find.unchecked;
        let mut contains = self.contains;
        if self.defaults {
            contains.extend(default_contains::<R>(ctx, unchecked).await?);
        }
        validate_all(&contains)?;
        let rows = self.find.all().await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let handle = handle_of::<R>(ctx, unchecked);
        load_all(
            &handle,
            &contains,
            rows.into_iter().map(Loaded::new).collect(),
        )
        .await
    }
}

pub(crate) trait TypedDecl<S: GuardedRepo>: Send + Sync {
    fn build(
        &self,
        registry: &Registry,
        tree: &NameTree,
    ) -> Result<Arc<dyn ErasedContain<S>>, RepoError>;

    fn target_json(&self, ctx: &Context) -> Result<Box<dyn JsonTarget>, RepoError>;

    fn target_id_from_json(&self, value: &Json) -> Option<RowId>;
}

pub(crate) fn typed_decl_of<S: GuardedRepo>(
    relation: &dyn Association,
) -> Option<Arc<dyn TypedDecl<S>>> {
    relation
        .typed_contain()
        .downcast_ref::<Arc<dyn TypedDecl<S>>>()
        .cloned()
}

struct Decl<A, T>(A, PhantomData<fn() -> T>);

/// Boxes a typed declaration for `Association::typed_contain`.
pub fn typed_contain<S: GuardedRepo, A>(declaration: A) -> Box<dyn Any + Send + Sync>
where
    A: Containable<S> + Loadable<S>,
{
    Box::new(typed_decl::<S, <A as Loadable<S>>::Target, A>(declaration))
}

pub(crate) fn typed_decl<S: GuardedRepo, T: GuardedRepo, A>(declaration: A) -> Arc<dyn TypedDecl<S>>
where
    A: Containable<S> + Loadable<S, Target = T>,
{
    Arc::new(Decl(declaration, PhantomData::<fn() -> T>))
}

impl<S: GuardedRepo, T: GuardedRepo, A> TypedDecl<S> for Decl<A, T>
where
    A: Containable<S> + Loadable<S, Target = T>,
{
    fn build(
        &self,
        registry: &Registry,
        tree: &NameTree,
    ) -> Result<Arc<dyn ErasedContain<S>>, RepoError> {
        if tree.nodes.is_empty() {
            return Ok(Arc::new(Slot(self.0.clone())));
        }
        let mut with = With {
            inner: self.0.clone(),
            children: Vec::new(),
            query: LoadQuery::new(),
        };
        for (name, sub) in &tree.nodes {
            let child = registry.typed_decl::<T>(name).ok_or_else(|| {
                RepoError::Db(DbErr::Custom(format!(
                    "cannot look up the typed declaration of association {name} (the registered Repo type differs from the declaration)"
                )))
            })?;
            with.children.push(child.build(registry, sub)?);
        }
        Ok(Arc::new(Slot(with)))
    }

    fn target_json(&self, ctx: &Context) -> Result<Box<dyn JsonTarget>, RepoError> {
        Ok(Box::new(JsonFor::<T>::of(ctx)?))
    }

    fn target_id_from_json(&self, value: &Json) -> Option<RowId> {
        Key::<T>::from_json(value).map(RowKey::into_row_id)
    }
}

/// A tree of association names; a name at one level appears once, so `items` and `items.product`
/// share `items`. For layers built on top of the ORM (such as a JSON API).
#[derive(Default)]
pub struct NameTree {
    pub(crate) nodes: Vec<(String, NameTree)>,
}

impl NameTree {
    fn insert(&mut self, path: &[String]) {
        let Some((first, rest)) = path.split_first() else {
            return;
        };
        let index = match self.nodes.iter().position(|(n, _)| n == first) {
            Some(i) => i,
            None => {
                self.nodes.push((first.clone(), NameTree::default()));
                self.nodes.len() - 1
            }
        };
        self.nodes[index].1.insert(rest);
    }
}

// Stops at the first unusable level. Unchecked only requires registration.
async fn trace_names(
    ctx: &Context,
    table: &str,
    path: &str,
    unchecked: bool,
) -> Result<Vec<String>, RepoError> {
    let registry = ctx.registry();
    let mut current = table.to_string();
    let mut names = Vec::new();
    for segment in path.split('.') {
        let found: Option<&Arc<dyn Association>> = registry
            .meta(&current)
            .and_then(|m| m.relations.iter().find(|r| r.name() == segment));
        let relation = match found {
            Some(r) if unchecked || r.usable(ctx).await? => r.clone(),
            _ => break,
        };
        names.push(relation.name().to_string());
        current = relation.target().to_string();
    }
    Ok(names)
}

async fn default_contains<R: GuardedRepo>(
    ctx: &Context,
    unchecked: bool,
) -> Result<Vec<Arc<dyn ErasedContain<R>>>, RepoError> {
    let registry = ctx.registry();
    let Some(meta) = registry.meta_of::<R::Entity>() else {
        return Err(RepoError::NotRegistered(
            <R as Repo>::repo_name().to_string(),
        ));
    };
    let mut tree = NameTree::default();
    for path in &meta.default_contain {
        tree.insert(&trace_names(ctx, &meta.name, path, unchecked).await?);
    }
    build_contains::<R>(registry, &tree)
}

fn build_contains<R: GuardedRepo>(
    registry: &Registry,
    tree: &NameTree,
) -> Result<Vec<Arc<dyn ErasedContain<R>>>, RepoError> {
    tree.nodes
        .iter()
        .map(|(name, sub)| {
            let decl = registry.typed_decl::<R>(name).ok_or_else(|| {
                RepoError::Db(DbErr::Custom(format!(
                    "cannot look up the typed declaration of association {name} (the registered Repo type differs from the declaration)"
                )))
            })?;
            decl.build(registry, sub)
        })
        .collect()
}

/// Loads the associations in `tree` for `rows` and attaches them. For layers built on top of the
/// ORM (such as a JSON API).
///
/// Never skips checks; the caller must pass a tree already limited to usable levels.
pub async fn load_related<R: GuardedRepo>(
    ctx: &Context,
    tree: &NameTree,
    rows: Vec<ModelOf<R>>,
) -> Result<Vec<Loaded<ModelOf<R>>>, RepoError> {
    let contains = build_contains::<R>(ctx.registry(), tree)?;
    validate_all(&contains)?;
    let handle = handle_of::<R>(ctx, false);
    load_all(
        &handle,
        &contains,
        rows.into_iter().map(Loaded::new).collect(),
    )
    .await
}
