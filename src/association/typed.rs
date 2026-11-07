//! Typed association declarations (`BelongsToOf`, `HasManyOf`, `HasOneOf`, `ManyToManyOf`) and the
//! `Loadable` trait that loads them. Declare each as a const on its Repo and list it with
//! `RepoConfig::association`; foreign keys are typed `Column`s, so mixing them up does not compile.

use crate::repo::GuardedRepo;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use sea_orm::prelude::async_trait::async_trait;
use sea_orm::sea_query::IntoCondition;
use sea_orm::{
    ColumnTrait, Condition, DbErr, EntityTrait, IdenStatic, Iterable, Order, PrimaryKeyToColumn,
};

use super::util::{ColumnOf, ModelOf, key_value, link_order, primary_column, read_in};
use super::{Arity, Association, Many, One, RelationCondition, SaveStrategy, SortOrder, unique};
use crate::context::Context;
use crate::key::{Key, RowKey};
use crate::meta::{column_named, default_order};
use crate::policy::resolve;
use crate::registry::{RepoConfig, RepoType};
use crate::repo::{Repo, RepoError, RepoHandle};

pub(super) type Marker<X> = PhantomData<fn() -> X>;

/// A typed belongsTo declaration: `S` holds the foreign key and points at one row of `T`.
///
/// `K` is the value type of the foreign key (default: the primary key type of `T`); set it with
/// `.binding_key` when pointing at a non-primary-key column.
///
/// ```text
/// const CUSTOMER: BelongsToOf<OrdersRepo, CustomersRepo> =
///     BelongsToOf::new("customer", orders::Column::CustomerId);
/// ```
pub struct BelongsToOf<S: GuardedRepo, T: GuardedRepo, K: RowKey = Key<T>> {
    pub(super) name: &'static str,
    pub(super) foreign_key: <S::Entity as EntityTrait>::Column,
    pub(super) binding_key: Option<<T::Entity as EntityTrait>::Column>,
    pub(super) save: bool,
    pub(super) finder: Option<Finder<T>>,
    _types: PhantomData<fn() -> (T, K)>,
}

/// A typed declaration of an association whose target holds the foreign key (hasMany, hasOne).
/// Write it with the aliases `HasManyOf` and `HasOneOf`.
///
/// `S` is the parent Repo, `T` the child Repo, `K` the value type of the parent column the foreign
/// key points at (default: the primary key type) and `C` is `Many` or `One`.
pub struct HasOf<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> {
    pub(super) name: &'static str,
    pub(super) foreign_key: <T::Entity as EntityTrait>::Column,
    pub(super) binding_key: Option<<S::Entity as EntityTrait>::Column>,
    pub(super) label: Option<&'static str>,
    pub(super) save: Option<SaveStrategy>,
    pub(super) filter: Option<fn() -> Condition>,
    pub(super) sort: &'static [(<T::Entity as EntityTrait>::Column, SortOrder)],
    pub(super) dependent: bool,
    pub(super) cascade_callbacks: bool,
    pub(super) finder: Option<Finder<T>>,
    _types: Marker<(S, K, C)>,
}

/// A typed hasMany declaration: a parent row has zero or more children.
///
/// ```text
/// const ITEMS: HasManyOf<OrdersRepo, OrderItemsRepo> =
///     HasManyOf::new("items", order_items::Column::OrderId).save(SaveStrategy::Replace);
/// ```
pub type HasManyOf<S, T, K = Key<S>> = HasOf<S, T, K, Many>;

/// A typed hasOne declaration: a parent row has zero or one target (the foreign key is unique).
pub type HasOneOf<S, T, K = Key<S>> = HasOf<S, T, K, One>;

/// A typed many-to-many declaration through a join table.
///
/// `S` is this Repo, `J` the join table Repo and `T` the target Repo. The two join table columns
/// point at `S` and at `T`. `K` and `TK` are the value types on each side (default: primary key
/// types); `.binding_key` and `.target_binding_key` point the join table at other columns. The
/// save strategy is `Replace` or `Append`.
///
/// ```text
/// const TAGS: ManyToManyOf<OrdersRepo, SettingOrderTagsRepo, MasterTagsRepo> = ManyToManyOf::new(
///     "tags", setting_order_tags::Column::OrderId, setting_order_tags::Column::TagId);
/// ```
pub struct ManyToManyOf<
    S: GuardedRepo,
    J: GuardedRepo,
    T: GuardedRepo,
    K: RowKey = Key<S>,
    TK: RowKey = Key<T>,
> {
    pub(super) name: &'static str,
    pub(super) foreign_key: <J::Entity as EntityTrait>::Column,
    pub(super) target_foreign_key: <J::Entity as EntityTrait>::Column,
    pub(super) binding_key: Option<<S::Entity as EntityTrait>::Column>,
    pub(super) target_binding_key: Option<<T::Entity as EntityTrait>::Column>,
    pub(super) label: Option<&'static str>,
    pub(super) save: Option<SaveStrategy>,
    pub(super) filter: Option<fn() -> Condition>,
    pub(super) sort: &'static [(<T::Entity as EntityTrait>::Column, SortOrder)],
    pub(super) dependent: bool,
    pub(super) cascade_callbacks: bool,
    pub(super) finder: Option<Finder<T>>,
    _types: Marker<(S, T, K, TK)>,
}

impl<S: GuardedRepo, T: GuardedRepo> BelongsToOf<S, T> {
    /// Creates the declaration from the name (used in `contain`) and the own foreign key.
    pub const fn new(name: &'static str, foreign_key: <S::Entity as EntityTrait>::Column) -> Self {
        Self {
            name,
            foreign_key,
            binding_key: None,
            save: false,
            finder: None,
            _types: PhantomData,
        }
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> BelongsToOf<S, T, K> {
    /// Points the foreign key at a non-primary-key column of the target. `K2` is the value type of
    /// both columns; `Registry::verify` checks it.
    pub const fn binding_key<K2: RowKey>(
        self,
        column: <T::Entity as EntityTrait>::Column,
    ) -> BelongsToOf<S, T, K2> {
        BelongsToOf {
            name: self.name,
            foreign_key: self.foreign_key,
            binding_key: Some(column),
            save: self.save,
            finder: self.finder,
            _types: PhantomData,
        }
    }

    /// Also saves the target row sent in the body: it is written before the parent and its
    /// primary key (or `binding_key` column) becomes the foreign key.
    pub const fn save(mut self) -> Self {
        self.save = true;
        self
    }

    /// Sets a `finder`, an async function that adds conditions and order to the target `LoadQuery`.
    /// ANDed with `filter`, it defines which rows are related in loading, matching, saving and
    /// deleting; its order applies only when loading.
    pub const fn finder(mut self, f: Finder<T>) -> Self {
        self.finder = Some(f);
        self
    }

    pub(crate) async fn association_query(
        &self,
        ctx: &Context,
    ) -> Result<QueryParts<T>, RepoError> {
        query_of::<T>(ctx, None, self.finder).await
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) const fn foreign_key(&self) -> <S::Entity as EntityTrait>::Column {
        self.foreign_key
    }

    pub(crate) fn binding_column(&self) -> ColumnOf<T> {
        self.binding_key.unwrap_or_else(primary_column::<T>)
    }
}

impl<S: GuardedRepo, T: GuardedRepo, C: Arity> HasOf<S, T, Key<S>, C> {
    /// Creates the declaration from the name and the child's foreign key column.
    pub const fn new(name: &'static str, foreign_key: <T::Entity as EntityTrait>::Column) -> Self {
        Self {
            name,
            foreign_key,
            binding_key: None,
            label: None,
            save: None,
            filter: None,
            sort: &[],
            dependent: false,
            cascade_callbacks: true,
            finder: None,
            _types: PhantomData,
        }
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> HasOf<S, T, K, C> {
    /// Points the child foreign key at a non-primary-key column of the parent. `K2` is the value
    /// type of both columns; `Registry::verify` checks it.
    pub const fn binding_key<K2: RowKey>(
        self,
        column: <S::Entity as EntityTrait>::Column,
    ) -> HasOf<S, T, K2, C> {
        HasOf {
            name: self.name,
            foreign_key: self.foreign_key,
            binding_key: Some(column),
            label: self.label,
            save: self.save,
            filter: self.filter,
            sort: self.sort,
            dependent: self.dependent,
            cascade_callbacks: self.cascade_callbacks,
            finder: self.finder,
            _types: PhantomData,
        }
    }

    pub(crate) fn binding_column(&self) -> ColumnOf<S> {
        self.binding_key.unwrap_or_else(primary_column::<S>)
    }

    /// Sets the label (default: the target entity's label).
    pub const fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// Sets a condition, as a function because `Condition` cannot be a const. ANDed when reading.
    pub const fn filter(mut self, f: fn() -> Condition) -> Self {
        self.filter = Some(f);
        self
    }

    /// Deletes the children when the parent is deleted, through the child Repo's `delete` (so soft
    /// delete, permissions and hooks apply).
    pub const fn dependent(mut self, dependent: bool) -> Self {
        self.dependent = dependent;
        self
    }

    /// Whether removals and NULL updates go row by row through the child Repo (default true, so
    /// Policy, hooks, observers and history apply); false uses one statement and skips them.
    pub const fn cascade_callbacks(mut self, on: bool) -> Self {
        self.cascade_callbacks = on;
        self
    }

    /// Sets a `finder`, an async function that adds conditions and order to the target `LoadQuery`.
    /// ANDed with `filter`, it defines which rows are related in loading, matching, saving and
    /// deleting; its order applies only when loading.
    pub const fn finder(mut self, f: Finder<T>) -> Self {
        self.finder = Some(f);
        self
    }

    pub(crate) async fn association_query(
        &self,
        ctx: &Context,
    ) -> Result<QueryParts<T>, RepoError> {
        query_of::<T>(ctx, self.filter, self.finder).await
    }

    pub(crate) fn relation_condition(&self) -> RelationCondition {
        RelationCondition::of::<T>(self.filter, self.finder)
    }

    /// Returns the name of the association.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn sort_names(&self) -> Vec<(&'static str, SortOrder)> {
        sort_names(self.sort)
    }

    pub(crate) fn foreign_key(&self) -> <T::Entity as EntityTrait>::Column {
        self.foreign_key
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> HasManyOf<S, T, K> {
    /// Sets how children in the body are saved. Without it they are not saved.
    pub const fn save(mut self, save: SaveStrategy) -> Self {
        self.save = Some(save);
        self
    }

    /// Sets the order of the children, placed before the child's default order when reading.
    pub const fn sort(
        mut self,
        sort: &'static [(<T::Entity as EntityTrait>::Column, SortOrder)],
    ) -> Self {
        self.sort = sort;
        self
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> HasOneOf<S, T, K> {
    /// Also saves the target row sent in the body; without it the body value is `read_only`.
    pub const fn save(mut self) -> Self {
        self.save = Some(SaveStrategy::Append);
        self
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo> ManyToManyOf<S, J, T> {
    /// Creates the declaration from the name and the join table columns to `S` and to the target.
    pub const fn new(
        name: &'static str,
        foreign_key: <J::Entity as EntityTrait>::Column,
        target_foreign_key: <J::Entity as EntityTrait>::Column,
    ) -> Self {
        Self {
            name,
            foreign_key,
            target_foreign_key,
            binding_key: None,
            target_binding_key: None,
            label: None,
            save: None,
            filter: None,
            sort: &[],
            dependent: true,
            cascade_callbacks: true,
            finder: None,
            _types: PhantomData,
        }
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>
    ManyToManyOf<S, J, T, K, TK>
{
    /// Points the join table at a non-primary-key column of this entity.
    pub const fn binding_key<K2: RowKey>(
        self,
        column: <S::Entity as EntityTrait>::Column,
    ) -> ManyToManyOf<S, J, T, K2, TK> {
        ManyToManyOf {
            name: self.name,
            foreign_key: self.foreign_key,
            target_foreign_key: self.target_foreign_key,
            binding_key: Some(column),
            target_binding_key: self.target_binding_key,
            label: self.label,
            save: self.save,
            filter: self.filter,
            sort: self.sort,
            dependent: self.dependent,
            cascade_callbacks: self.cascade_callbacks,
            finder: self.finder,
            _types: PhantomData,
        }
    }

    /// Points the join table at a non-primary-key column of the target.
    pub const fn target_binding_key<TK2: RowKey>(
        self,
        column: <T::Entity as EntityTrait>::Column,
    ) -> ManyToManyOf<S, J, T, K, TK2> {
        ManyToManyOf {
            name: self.name,
            foreign_key: self.foreign_key,
            target_foreign_key: self.target_foreign_key,
            binding_key: self.binding_key,
            target_binding_key: Some(column),
            label: self.label,
            save: self.save,
            filter: self.filter,
            sort: self.sort,
            dependent: self.dependent,
            cascade_callbacks: self.cascade_callbacks,
            finder: self.finder,
            _types: PhantomData,
        }
    }

    pub(crate) fn binding_column(&self) -> ColumnOf<S> {
        self.binding_key.unwrap_or_else(primary_column::<S>)
    }

    pub(crate) fn target_binding_name(&self) -> Option<&'static str> {
        self.target_binding_key.map(|c| c.as_str())
    }

    pub(crate) fn target_binding_column(&self) -> ColumnOf<T> {
        self.target_binding_key.unwrap_or_else(primary_column::<T>)
    }

    /// Sets the label (default: the target entity's label).
    pub const fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// Sets how the body is saved (default: not saved). `AppendWithDelete` panics at registration.
    pub const fn save(mut self, save: SaveStrategy) -> Self {
        self.save = Some(save);
        self
    }

    /// Sets a condition, as a function because `Condition` cannot be a const. ANDed when reading.
    pub const fn filter(mut self, f: fn() -> Condition) -> Self {
        self.filter = Some(f);
        self
    }

    /// Deletes the join rows when the parent is deleted, through the join Repo's `delete` (so soft
    /// delete, permissions and hooks apply). Defaults to true.
    pub const fn dependent(mut self, dependent: bool) -> Self {
        self.dependent = dependent;
        self
    }

    /// Whether removals go row by row through the join Repo (default true, so Policy, hooks,
    /// observers and history apply); false uses one statement and skips them.
    pub const fn cascade_callbacks(mut self, on: bool) -> Self {
        self.cascade_callbacks = on;
        self
    }

    /// Sets the order of the targets, placed before the target's default order when reading.
    /// Without it targets follow the join table's default order (its order column, else its key).
    pub const fn sort(
        mut self,
        sort: &'static [(<T::Entity as EntityTrait>::Column, SortOrder)],
    ) -> Self {
        self.sort = sort;
        self
    }

    /// Sets a `finder`, an async function that adds conditions and order to the target `LoadQuery`.
    /// ANDed with `filter`, it defines which rows are related in loading, matching, saving and
    /// deleting; its order applies only when loading.
    pub const fn finder(mut self, f: Finder<T>) -> Self {
        self.finder = Some(f);
        self
    }

    pub(crate) async fn association_query(
        &self,
        ctx: &Context,
    ) -> Result<QueryParts<T>, RepoError> {
        query_of::<T>(ctx, self.filter, self.finder).await
    }

    pub(crate) fn relation_condition(&self) -> RelationCondition {
        RelationCondition::of::<T>(self.filter, self.finder)
    }

    /// Returns the name of the association.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn sort_names(&self) -> Vec<(&'static str, SortOrder)> {
        sort_names(self.sort)
    }

    pub(crate) fn through(&self) -> &'static str {
        <J as Repo>::repo_name()
    }

    pub(crate) fn foreign_key(&self) -> <J::Entity as EntityTrait>::Column {
        self.foreign_key
    }

    pub(crate) fn target_foreign_key(&self) -> <J::Entity as EntityTrait>::Column {
        self.target_foreign_key
    }
}

// `derive` would require `Clone`/`Copy` of the type parameters; columns are always `Copy`.
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Clone for BelongsToOf<S, T, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Copy for BelongsToOf<S, T, K> {}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Clone for HasOf<S, T, K, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Copy for HasOf<S, T, K, C> {}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> fmt::Debug for HasOf<S, T, K, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(C::NAME)
            .field("name", &self.name)
            .field("foreign_key", &self.foreign_key.as_str())
            .field("binding_key", &self.binding_key.map(|c| c.as_str()))
            .field("target", &<T as Repo>::repo_name())
            .finish()
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Clone
    for ManyToManyOf<S, J, T, K, TK>
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Copy
    for ManyToManyOf<S, J, T, K, TK>
{
}

impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> fmt::Debug for BelongsToOf<S, T, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BelongsToOf")
            .field("name", &self.name)
            .field("foreign_key", &self.foreign_key.as_str())
            .field("binding_key", &self.binding_key.map(|c| c.as_str()))
            .field("target", &<T as Repo>::repo_name())
            .finish()
    }
}

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> fmt::Debug
    for ManyToManyOf<S, J, T, K, TK>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManyToManyOf")
            .field("name", &self.name)
            .field("foreign_key", &self.foreign_key.as_str())
            .field("target_foreign_key", &self.target_foreign_key.as_str())
            .field("binding_key", &self.binding_key.map(|c| c.as_str()))
            .field(
                "target_binding_key",
                &self.target_binding_key.map(|c| c.as_str()),
            )
            .field("target", &<T as Repo>::repo_name())
            .field("through", &<J as Repo>::repo_name())
            .finish()
    }
}

/// A declaration that `RepoConfig::association` accepts for the Entity `E`.
///
/// Implemented for the declaration types with `S: GuardedRepo<Entity = E>`, so a declaration of
/// another Entity does not compile.
pub trait Declaration<E: EntityTrait> {
    /// Registers the association in `config`.
    fn register(self, config: RepoConfig<'_, E>) -> RepoConfig<'_, E>;
}

impl<E: EntityTrait, S: GuardedRepo<Entity = E>, T: GuardedRepo, K: RowKey> Declaration<E>
    for BelongsToOf<S, T, K>
where
    <S::Entity as EntityTrait>::Column: Send + Sync,
{
    fn register(self, mut config: RepoConfig<'_, E>) -> RepoConfig<'_, E> {
        let table = <S as Repo>::repo_name();
        require_id_key::<T>(table, self.name, "target");
        config.push_typed_checks(
            self.name,
            vec![
                ("own", RepoType::of::<S>()),
                ("target", RepoType::of::<T>()),
            ],
        );
        config.add_association(self)
    }
}

impl<E: EntityTrait, S: GuardedRepo<Entity = E>, T: GuardedRepo, K: RowKey, C: Arity> Declaration<E>
    for HasOf<S, T, K, C>
where
    <T::Entity as EntityTrait>::Column: Send + Sync,
{
    fn register(self, mut config: RepoConfig<'_, E>) -> RepoConfig<'_, E> {
        let table = <S as Repo>::repo_name();
        require_id_key::<S>(table, self.name, "parent");
        config.push_typed_checks(
            self.name,
            vec![
                ("own", RepoType::of::<S>()),
                ("target", RepoType::of::<T>()),
            ],
        );
        config.add_association(self)
    }
}

impl<
    E: EntityTrait,
    S: GuardedRepo<Entity = E>,
    J: GuardedRepo,
    T: GuardedRepo,
    K: RowKey,
    TK: RowKey,
> Declaration<E> for ManyToManyOf<S, J, T, K, TK>
where
    <J::Entity as EntityTrait>::Column: Send + Sync,
{
    fn register(self, mut config: RepoConfig<'_, E>) -> RepoConfig<'_, E> {
        let table = <S as Repo>::repo_name();
        require_id_key::<S>(table, self.name, "target");
        require_id_key::<T>(table, self.name, "target");
        config.push_typed_checks(
            self.name,
            vec![
                ("own", RepoType::of::<S>()),
                ("target", RepoType::of::<T>()),
                ("join table", RepoType::of::<J>()),
            ],
        );
        config.add_association(self)
    }
}

fn sort_names<C: ColumnTrait>(sort: &[(C, SortOrder)]) -> Vec<(&'static str, SortOrder)> {
    sort.iter()
        .map(|(column, order)| (column.as_str(), *order))
        .collect()
}

fn require_id_key<R: GuardedRepo>(table: &str, name: &str, what: &str) {
    let mut keys = <<R::Entity as EntityTrait>::PrimaryKey as Iterable>::iter();
    let is_id = match (keys.next(), keys.next()) {
        (Some(key), None) => key.into_column().as_str() == "id",
        _ => false,
    };
    if !is_id {
        panic!("{table}: the {what} of association {name} must have a single id column");
    }
}

/// Builds the conditions and order for the target `T` of a `load_where` closure.
///
/// The target's Policy and the `IN` condition are always applied and cannot be removed. There is no
/// `limit`, since in a contain it would apply to the whole result rather than per parent. Orders
/// are applied in the order added, followed by the target's default order.
pub struct LoadQuery<T: GuardedRepo> {
    condition: Condition,
    orders: Vec<(ColumnOf<T>, Order)>,
}

impl<T: GuardedRepo> LoadQuery<T> {
    pub(crate) fn new() -> Self {
        Self {
            condition: Condition::all(),
            orders: Vec::new(),
        }
    }

    /// Adds a condition with AND.
    pub fn filter(mut self, condition: impl IntoCondition) -> Self {
        self.condition = self.condition.add(condition.into_condition());
        self
    }

    /// Returns the condition and order as they are, for the query that reads the target.
    pub fn into_parts(self) -> (Condition, Vec<(ColumnOf<T>, Order)>) {
        (self.condition, self.orders)
    }

    /// Adds an ascending order.
    pub fn order_by_asc(mut self, column: ColumnOf<T>) -> Self {
        self.orders.push((column, Order::Asc));
        self
    }

    /// Adds a descending order.
    pub fn order_by_desc(mut self, column: ColumnOf<T>) -> Self {
        self.orders.push((column, Order::Desc));
        self
    }
}

impl<T: GuardedRepo> LoadQuery<T> {
    pub(crate) fn merge(mut self, other: Self) -> Self {
        self.condition = Condition::all().add(self.condition).add(other.condition);
        self.orders.extend(other.orders);
        self
    }
}

impl<T: GuardedRepo> Clone for LoadQuery<T> {
    fn clone(&self) -> Self {
        Self {
            condition: self.condition.clone(),
            orders: self.orders.clone(),
        }
    }
}

impl<T: GuardedRepo> Default for LoadQuery<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Loads one typed association for `RepoHandle::load` and `load_where`.
#[async_trait]
pub trait Loadable<S: GuardedRepo>: Send + Sync {
    /// Result type, decided by the kind of declaration.
    type Output: Send;

    /// The target Repo (the type of `LoadQuery`).
    type Target: GuardedRepo;

    /// Loads the association for `rows`. `query` adds conditions and order on the target.
    async fn load(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<Self::Target>,
    ) -> Result<Self::Output, RepoError>;
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey> Loadable<S> for BelongsToOf<S, T, K>
where
    <S::Entity as EntityTrait>::Column: Send + Sync,
{
    type Output = HashMap<K, ModelOf<T>>;
    type Target = T;

    async fn load(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Self::Output, RepoError> {
        let ctx = handle.context();
        let unchecked = handle.is_unchecked();
        let registered = resolve::<S, BelongsToOf<S, T, K>>(ctx, self.name)?;
        let foreign_key = registered.foreign_key().as_str();
        let target_column = registered.binding_column();
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        authorize_contain::<S>(ctx, self.name, unchecked).await?;

        let column = fk_column::<S>(self.name, foreign_key)?;
        let (condition, finder_orders) = registered.association_query(ctx).await?;
        let query = with_association::<T>(self.name, condition, finder_orders, &[], query)?;
        let keys: Vec<Option<K>> = rows.iter().map(|r| key_value(r, column)).collect();
        let found =
            read_target::<T, _>(ctx, unchecked, target_column, &unique(&keys), query).await?;
        // With several targets for one value, the first in read order is used.
        let mut out: HashMap<K, ModelOf<T>> = HashMap::new();
        for m in found {
            if let Some(id) = key_value::<K, _>(&m, target_column) {
                out.entry(id).or_insert(m);
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl<S: GuardedRepo, T: GuardedRepo, K: RowKey, C: Arity> Loadable<S> for HasOf<S, T, K, C>
where
    <T::Entity as EntityTrait>::Column: Send + Sync,
{
    type Output = C::Grouped<K, ModelOf<T>>;
    type Target = T;

    async fn load(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Self::Output, RepoError> {
        let ctx = handle.context();
        let unchecked = handle.is_unchecked();
        let registered = resolve::<S, HasOf<S, T, K, C>>(ctx, self.name)?;
        let (foreign_key, sort, parent_column) = (
            registered.foreign_key().as_str(),
            registered.sort_names(),
            registered.binding_column(),
        );
        if rows.is_empty() {
            return Ok(C::group(&[], Vec::new(), |_: &ModelOf<T>| None));
        }
        authorize_contain::<S>(ctx, self.name, unchecked).await?;

        let column = fk_column::<T>(self.name, foreign_key)?;
        let (condition, finder_orders) = registered.association_query(ctx).await?;
        let query = with_association::<T>(self.name, condition, finder_orders, &sort, query)?;
        let parents: Vec<K> = rows
            .iter()
            .filter_map(|r| key_value::<K, _>(r, parent_column))
            .collect();
        let children =
            read_target::<T, _>(ctx, unchecked, column, &unique_ids(&parents), query).await?;
        Ok(C::group(&parents, children, |m| {
            key_value::<K, _>(m, column)
        }))
    }
}

#[async_trait]
impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Loadable<S>
    for ManyToManyOf<S, J, T, K, TK>
where
    <J::Entity as EntityTrait>::Column: Send + Sync,
{
    type Output = HashMap<K, Vec<ModelOf<T>>>;
    type Target = T;

    async fn load(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<Self::Output, RepoError> {
        Ok(self
            .load_linked(handle, rows, query)
            .await?
            .into_iter()
            .map(|(parent, pairs)| (parent, pairs.into_iter().map(|(t, _)| t).collect()))
            .collect())
    }
}

pub(crate) type Linked<T, J> = (ModelOf<T>, ModelOf<J>);

impl<S: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>
    ManyToManyOf<S, J, T, K, TK>
where
    <J::Entity as EntityTrait>::Column: Send + Sync,
{
    /// Loads like `load` but returns each target with the join table row of its pair with that
    /// parent (`_joinData`). A target shared by several parents appears once per parent.
    pub(crate) async fn load_linked(
        self,
        handle: &RepoHandle<'_, S>,
        rows: &[ModelOf<S>],
        query: LoadQuery<T>,
    ) -> Result<HashMap<K, Vec<Linked<T, J>>>, RepoError> {
        let ctx = handle.context();
        let unchecked = handle.is_unchecked();
        let registered = resolve::<S, ManyToManyOf<S, J, T, K, TK>>(ctx, self.name)?;
        let (foreign_key, target_foreign_key, sort) = (
            registered.foreign_key().as_str(),
            registered.target_foreign_key().as_str(),
            registered.sort_names(),
        );
        let (own_column, pk) = (
            registered.binding_column(),
            registered.target_binding_column(),
        );
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        authorize_contain::<S>(ctx, self.name, unchecked).await?;

        let parent_column = fk_column::<J>(self.name, foreign_key)?;
        let target_column = fk_column::<J>(self.name, target_foreign_key)?;
        let (condition, finder_orders) = registered.association_query(ctx).await?;
        let query = with_association::<T>(self.name, condition, finder_orders, &sort, query)?;
        let parents: Vec<K> = rows
            .iter()
            .filter_map(|r| key_value::<K, _>(r, own_column))
            .collect();
        let links = read_in::<J, _>(
            ctx,
            unchecked,
            parent_column,
            &unique_ids(&parents),
            None,
            default_order::<J::Entity>(&ctx.registry().spec_of::<J::Entity>()),
        )
        .await?;
        let pairs: Vec<(K, TK)> = links
            .iter()
            .filter_map(|link| {
                Some((
                    key_value(link, parent_column)?,
                    key_value(link, target_column)?,
                ))
            })
            .collect();
        let target_ids = unique(
            &pairs
                .iter()
                .map(|(_, t)| Some(t.clone()))
                .collect::<Vec<_>>(),
        );
        let mut links_by_parent: HashMap<K, Vec<TK>> = HashMap::new();
        for (parent, target) in pairs {
            links_by_parent.entry(parent).or_default().push(target);
        }
        // Pair to join table row; the first row wins if a pair is duplicated.
        let mut link_of: HashMap<(K, TK), ModelOf<J>> = HashMap::new();
        for link in links {
            if let (Some(parent), Some(target)) = (
                key_value::<K, _>(&link, parent_column),
                key_value::<TK, _>(&link, target_column),
            ) {
                link_of.entry((parent, target)).or_insert(link);
            }
        }
        let ordered = !query.orders.is_empty();
        let read = read_target::<T, _>(ctx, unchecked, pk, &target_ids, query).await?;
        let position: HashMap<TK, usize> = read
            .iter()
            .enumerate()
            .filter_map(|(i, m)| key_value(m, pk).map(|id| (id, i)))
            .collect();
        let mut found: HashMap<TK, ModelOf<T>> = HashMap::new();
        for m in read {
            if let Some(id) = key_value::<TK, _>(&m, pk) {
                found.entry(id).or_insert(m);
            }
        }
        // With an explicit order, targets follow the target-side order; otherwise the join table order.
        let position = ordered.then_some(&position);
        let grouped: HashMap<K, Vec<Linked<T, J>>> = parents
            .iter()
            .map(|parent| {
                let ids = link_order(
                    &links_by_parent,
                    parent,
                    |id| found.contains_key(id),
                    position,
                );
                let models = ids
                    .iter()
                    .filter_map(|id| {
                        let target = found.get(id)?.clone();
                        let link = link_of.get(&(parent.clone(), id.clone()))?.clone();
                        Some((target, link))
                    })
                    .collect();
                (parent.clone(), models)
            })
            .collect();
        Ok(grouped)
    }
}

pub(crate) type QueryParts<T> = (Option<Condition>, Vec<(ColumnOf<T>, Order)>);

/// Builds the association condition and order from `filter` and `finder` (run with an empty
/// `LoadQuery`, also when `unchecked`). A `finder` error is returned.
pub(crate) async fn query_of<T: GuardedRepo>(
    ctx: &Context,
    filter: Option<fn() -> Condition>,
    finder: Option<Finder<T>>,
) -> Result<QueryParts<T>, RepoError> {
    let mut condition = filter.map(|f| f());
    let mut orders = Vec::new();
    if let Some(finder) = finder {
        let (found, found_orders) = finder(ctx, LoadQuery::<T>::new()).await?.into_parts();
        condition = Some(match condition {
            Some(a) => Condition::all().add(a).add(found),
            None => found,
        });
        orders = found_orders;
    }
    Ok((condition, orders))
}

/// Future returned by a `Finder`.
pub type FinderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<LoadQuery<T>, RepoError>> + Send + 'a>>;

/// Type of an association `finder`: an async function that adds conditions and order to the
/// target `LoadQuery`.
pub type Finder<T> = for<'a> fn(&'a Context, LoadQuery<T>) -> FinderFuture<'a, T>;

/// Merges the registered association condition and order into the `load_where` `query`.
/// Condition: association AND `query`. Order: `query`, then `finder`, then the association `sort`
/// (the target's default order follows). Uses the registered association rather than the const.
fn with_association<T: GuardedRepo>(
    name: &str,
    condition: Option<Condition>,
    finder_orders: Vec<(ColumnOf<T>, Order)>,
    sort: &[(&'static str, SortOrder)],
    mut query: LoadQuery<T>,
) -> Result<LoadQuery<T>, RepoError> {
    if let Some(condition) = condition {
        query.condition = Condition::all().add(condition).add(query.condition);
    }
    query.orders.extend(finder_orders);
    for (column, order) in sort {
        query
            .orders
            .push((fk_column::<T>(name, column)?, order.order()));
    }
    Ok(query)
}

pub(crate) fn resolve<'c, S: GuardedRepo, D: Association>(
    ctx: &'c Context,
    name: &str,
) -> Result<&'c D, RepoError> {
    declared::<S>(ctx, name)?
        .as_any()
        .downcast_ref::<D>()
        .ok_or_else(|| mismatch::<S>(name, "the kind or type differs"))
}

fn mismatch<S: GuardedRepo>(name: &str, reason: &str) -> RepoError {
    RepoError::Db(DbErr::Custom(format!(
        "{}: the declaration of association {name} does not match the registration ({reason})",
        <S as Repo>::repo_name()
    )))
}

fn declared<'c, S: GuardedRepo>(
    ctx: &'c Context,
    name: &str,
) -> Result<&'c Arc<dyn Association>, RepoError> {
    let relations = ctx
        .registry()
        .relations_of::<S::Entity>()
        .ok_or_else(|| RepoError::NotRegistered(<S as Repo>::repo_name().to_string()))?;
    relations
        .iter()
        .find(|a| a.name() == name)
        .ok_or_else(|| mismatch::<S>(name, "no association with that name is registered"))
}

async fn authorize_contain<S: GuardedRepo>(
    ctx: &Context,
    name: &str,
    unchecked: bool,
) -> Result<(), RepoError> {
    if unchecked {
        return Ok(());
    }
    let repo = ctx
        .registry()
        .repo::<S>()
        .ok_or_else(|| RepoError::NotRegistered(<S as Repo>::repo_name().to_string()))?;
    if resolve::can_contain(ctx, &*repo, name).await? {
        Ok(())
    } else {
        Err(RepoError::Forbidden)
    }
}

pub(crate) fn fk_column<R: GuardedRepo>(
    name: &str,
    column: &str,
) -> Result<ColumnOf<R>, RepoError> {
    column_named::<R::Entity>(column).ok_or_else(|| {
        RepoError::Db(DbErr::Custom(format!(
            "{}: association {name}: column {column} does not exist",
            <R as Repo>::repo_name()
        )))
    })
}

fn unique_ids<K: RowKey>(ids: &[K]) -> Vec<K> {
    unique(&ids.iter().map(|id| Some(id.clone())).collect::<Vec<_>>())
}

async fn read_target<T: GuardedRepo, K: RowKey>(
    ctx: &Context,
    unchecked: bool,
    column: ColumnOf<T>,
    ids: &[K],
    query: LoadQuery<T>,
) -> Result<Vec<ModelOf<T>>, RepoError> {
    let LoadQuery {
        condition,
        mut orders,
    } = query;
    orders.extend(default_order::<T::Entity>(
        &ctx.registry().spec_of::<T::Entity>(),
    ));
    read_in::<T, _>(ctx, unchecked, column, ids, Some(condition), orders).await
}
