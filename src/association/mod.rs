//! Registered associations: the `Association` trait and its typed implementations (belongsTo,
//! hasMany, hasOne, many-to-many), with the declarations in `typed` and the write side in the
//! other modules.

mod belongs_to;
mod filter;
mod has;
mod has_many;
mod many_to_many;
mod prepared;
pub(crate) mod reference;
mod typed;
pub(crate) mod util;

pub(crate) use belongs_to::ParentRow;
pub use filter::{RelationCondition, SortOrder};
pub use has::{Arity, Many, One};
pub use has_many::SaveStrategy;
pub(crate) use has_many::{ChildRow, ChildRows, UnlinkChildren, row_prefix};
pub(crate) use many_to_many::{LinkSpec, LinksOf, UnlinkLinks};
pub use prepared::{PreparedChildren, PreparedParent};
pub use typed::{
    BelongsToOf, Declaration, Finder, FinderFuture, HasManyOf, HasOf, HasOneOf, LoadQuery,
    Loadable, ManyToManyOf,
};
pub(crate) use typed::{fk_column, resolve};
pub use util::target_subquery;
pub(crate) use util::{ModelOf, handle_of, key_value, row_id_of_value};
pub(crate) use util::{primary_column, row_key};

use std::any::Any;
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use sea_orm::Condition;
use sea_orm::prelude::async_trait::async_trait;
use serde_json::Value as Json;

use crate::context::Context;
use crate::key::{KeyKind, RowId, RowKey};
use crate::marshal::FieldErrors;
use crate::meta::FieldKind;
use crate::patch::PatchError;
use crate::registry::{EntityMeta, Registry};
use crate::repo::RepoError;

pub(crate) fn unique<K: RowKey>(keys: &[Option<K>]) -> Vec<K> {
    let mut seen: HashSet<K> = HashSet::new();
    keys.iter()
        .flatten()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
}

/// Kind of an association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelationKind {
    BelongsTo,
    HasMany,
    /// The target holds the foreign key; one parent row has zero or one target row.
    HasOne,
    ManyToMany,
    /// A kind added by the application, named by the string.
    Custom(&'static str),
}

/// A value snapshot of a registered association, built from the declaration on each call.
#[derive(Clone, Debug, PartialEq)]
pub struct RelationInfo {
    /// Name used in `contain`.
    pub name: &'static str,
    pub kind: RelationKind,
    /// Name of the target Repo.
    pub target: &'static str,
    /// Join table Repo name (many-to-many).
    pub through: Option<&'static str>,
    /// Own column (belongsTo), child column (hasMany) or join table column to this entity.
    pub foreign_key: &'static str,
    /// Join table column to the target (many-to-many).
    pub target_foreign_key: Option<&'static str>,
    /// `bindingKey`: the target column belongsTo points at; `None` is the primary key.
    pub binding_key: Option<&'static str>,
    /// `bindingKey` on the target side of a many-to-many; `None` is the primary key.
    pub target_binding_key: Option<&'static str>,
    pub label: Option<&'static str>,
    /// Save strategy. A hasOne with `.save()` reports `Some(Append)`.
    pub save: Option<SaveStrategy>,
    /// Whether a `filter` is set.
    pub filtered: bool,
    /// Whether a `finder` is set.
    pub finder: bool,
    pub sort: Vec<(&'static str, SortOrder)>,
    pub dependent: bool,
    /// Whether removals and NULL updates go row by row via the target Repo (true for belongsTo).
    pub cascade_callbacks: bool,
}

/// How an association connects the two tables, so layers need not know the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssociationType {
    /// Own column points at the target (belongsTo).
    ManyToOne,
    /// Target column points at this entity; one parent has many targets (hasMany).
    OneToMany,
    /// Target column points at this entity; one parent has zero or one target (hasOne).
    OneToOne,
    /// Connected through a join table.
    ManyToMany,
}

/// Number of values an association yields per parent row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cardinality {
    /// One value, or null if absent (belongsTo, hasOne).
    One,
    /// An array.
    Many,
}

/// How deep nested associations are read when saving; `read_input` receives the child-row side.
#[derive(Clone, Debug, Default)]
pub struct NestedScope {
    /// Accepted association paths such as `orders.items`; `None` accepts all (one level in rows).
    pub associated: Option<Vec<String>>,
    /// Depth of this row; the parent row is 0.
    pub depth: usize,
    /// Id of the parent row that owns the association, set only when passed to `read_input`.
    /// `None` for rows being inserted. A hasOne uses it to read the current target before writing.
    pub owner_id: Option<RowId>,
}

impl NestedScope {
    pub(crate) fn top_names(&self) -> Option<Vec<&str>> {
        self.associated.as_ref().map(|v| {
            v.iter()
                .map(|n| n.split('.').next().unwrap_or(""))
                .collect()
        })
    }

    /// Returns the scope for the target rows of association `name`: paths starting with `name`
    /// with that first step removed. The default (`None`) reads one level only, so the result
    /// accepts no associations.
    pub fn child(&self, name: &str) -> NestedScope {
        NestedScope {
            associated: Some(match self.associated.as_ref() {
                Some(v) => v
                    .iter()
                    .filter_map(|n| n.strip_prefix(name)?.strip_prefix('.'))
                    .map(str::to_string)
                    .collect(),
                None => Vec::new(),
            }),
            depth: self.depth + 1,
            owner_id: None,
        }
    }

    pub(crate) fn none(&self) -> NestedScope {
        NestedScope {
            associated: Some(Vec::new()),
            depth: self.depth,
            owner_id: None,
        }
    }
}

/// Result of reading an association value from the request body.
pub enum RelationInput {
    /// Not treated as body input (belongsTo); the key stays in the parent input.
    Ignored,
    /// The key is removed from the parent input; a `read_only` error was already added.
    ReadOnly,
    /// The key is removed; the function is called at marshal time to prepare the write.
    Write(PrepareFn),
    /// A belongsTo target row: the key and the foreign key column are removed, and the function
    /// prepares a write that happens before the parent.
    WriteParent(PrepareParentFn),
}

/// Future returned by a `PrepareParentFn`.
pub type PrepareParentFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Box<dyn PreparedParent>>, PatchError>> + Send + 'a>>;

/// Prepares the write of a belongsTo target. `Ok(None)` if the target is not registered; validation
/// errors are `Invalid` with keys `name.field`.
pub type PrepareParentFn = Box<dyn for<'a> FnOnce(&'a Context) -> PrepareParentFuture<'a> + Send>;

/// Builds a `PrepareParentFn`; fixes the closure types.
pub fn prepare_parent_fn(
    f: impl for<'a> FnOnce(&'a Context) -> PrepareParentFuture<'a> + Send + 'static,
) -> PrepareParentFn {
    Box::new(f)
}

/// Future returned by a `PrepareFn`.
pub type PrepareFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Option<Box<dyn PreparedChildren>>, PatchError>> + Send + 'a>,
>;

/// Prepares a write of children or pairs. `Ok(None)` if the target is not registered; validation
/// errors (`Invalid`) are returned. Async because column permissions may need a DB read. Build it
/// with `prepare_fn`.
pub type PrepareFn = Box<dyn for<'a> FnOnce(&'a Context) -> PrepareFuture<'a> + Send>;

/// Builds a `PrepareFn`; fixes the closure lifetimes.
pub fn prepare_fn(
    f: impl for<'a> FnOnce(&'a Context) -> PrepareFuture<'a> + Send + 'static,
) -> PrepareFn {
    Box::new(f)
}

/// A registered association, implemented by the typed declarations.
///
/// The Registry holds it as `Arc<dyn Association>`. An application can add its own kind by
/// implementing this trait with `Declaration`, `Related`, `Loadable` and `Containable`, and
/// registering through `RepoConfig::add_association`; `target_subquery`, `repo::typed_contain` and
/// `LoadQuery::into_parts` help with the pieces.
#[async_trait]
pub trait Association: Send + Sync + fmt::Debug + 'static {
    /// Name used in `contain` and as the JSON key.
    fn name(&self) -> &'static str;

    /// Registered name of the target (its table name).
    fn target(&self) -> &'static str;

    /// Label of the association, if given; otherwise the target entity's label.
    fn label(&self) -> Option<&'static str> {
        None
    }

    /// Returns a value snapshot of the association.
    fn info(&self) -> RelationInfo;

    /// Whether a parent row has one value (null if absent) or many (an array).
    fn cardinality(&self) -> Cardinality;

    /// Own foreign key column (belongsTo); `None` for other kinds.
    fn owner_foreign_key(&self) -> Option<&'static str> {
        None
    }

    /// Target-side foreign key column (hasMany); `None` for other kinds.
    fn child_foreign_key(&self) -> Option<&'static str> {
        None
    }

    /// Column name when the association points at a non-primary-key column (`bindingKey`).
    fn binding_key(&self) -> Option<&'static str> {
        None
    }

    /// Checks the association against `owner` after registration and panics if it is invalid.
    /// Implementations call `require_target` first.
    fn verify(&self, owner: &EntityMeta, registry: &Registry);

    /// Returns whether the viewer may use the association.
    ///
    /// Needs `can(View)` on the target (and on the join table for many-to-many) and `can_contain`
    /// on the parent; belongsTo also needs the foreign key column to be readable. Async because
    /// a Policy may read the DB.
    async fn usable(&self, ctx: &Context) -> Result<bool, RepoError>;

    /// Returns how the association connects the tables.
    fn association_type(&self) -> AssociationType;

    /// Turns a condition on the target rows into `column IN (subquery)` on the own side.
    ///
    /// The subquery carries the scope and soft delete of the target (and join table) and the
    /// association `filter` and `finder`. Parent rows are not duplicated.
    async fn matching_condition(
        &self,
        ctx: &Context,
        target: Condition,
    ) -> Result<Condition, RepoError>;

    /// Builds the typed contain, boxed as `Arc<dyn TypedDecl<S>>`; the caller downcasts it.
    fn typed_contain(&self) -> Box<dyn Any + Send + Sync>;

    /// Returns whether the foreign key target can be chosen. Called only for kinds whose
    /// `owner_foreign_key` is `Some`.
    async fn check_exists(&self, _ctx: &Context, _id: &RowId) -> Result<bool, RepoError> {
        Ok(true)
    }

    /// Reads the association value of the request body. Returns `BadRequest` for a wrong shape;
    /// `read_only` and `duplicate` errors are added to `errors`. `scope` is for the target rows.
    /// The default is `Ignored`.
    fn read_input(
        &self,
        _ctx: &Context,
        _value: &Json,
        _errors: &mut FieldErrors,
        _scope: &NestedScope,
    ) -> Result<RelationInput, PatchError> {
        Ok(RelationInput::Ignored)
    }

    /// Whether deleting the parent also deletes the children or pairs.
    fn dependent(&self) -> bool {
        false
    }

    /// Whether removals and NULL updates go through the target Repo row by row (default true);
    /// false uses one UPDATE or DELETE statement.
    fn cascade_callbacks(&self) -> bool {
        true
    }

    /// Deletes the children or pairs of `parent`. Called only when `dependent` is true.
    async fn delete_dependents(
        &self,
        _ctx: &Context,
        _parent: &RowId,
        _unchecked: bool,
        _purge: bool,
    ) -> Result<(), RepoError> {
        Ok(())
    }

    /// Returns the declaration as `Any`, so it can be downcast to its concrete type.
    fn as_any(&self) -> &dyn Any;
}

pub(crate) fn require_target<'r>(
    a: &(impl Association + ?Sized),
    owner: &EntityMeta,
    registry: &'r Registry,
) -> &'r EntityMeta {
    let Some(target) = registry.meta(a.target()) else {
        panic!(
            "{}: association {}: target {} is not registered",
            owner.name,
            a.name(),
            a.target()
        );
    };
    target
}

/// A missing column is ignored (checked elsewhere).
pub(crate) fn check_key_type(table: &str, column: &str, meta: &EntityMeta, key: KeyKind) {
    let Some(field) = meta.fields.iter().find(|f| f.name == column) else {
        return;
    };
    let matches = matches!(
        (field.kind, key),
        (FieldKind::BigInteger, KeyKind::BigInteger)
            | (FieldKind::Uuid, KeyKind::Uuid)
            | (FieldKind::String { .. }, KeyKind::String)
    );
    if !matches {
        panic!(
            "{table}.{column}: foreign key column type {} does not match the primary key type {} of the target",
            field.kind.as_str(),
            key.as_str()
        );
    }
}

pub(crate) fn check_binding_key_type(
    table: &str,
    column: &str,
    owner: &EntityMeta,
    binding: &str,
    target: &EntityMeta,
    key: KeyKind,
) {
    let Some(binding_field) = target.fields.iter().find(|f| f.name == binding) else {
        panic!(
            "{table}.{column}: bindingKey column {}.{binding} does not exist",
            target.name
        );
    };
    let Some(binding_kind) = key_kind_of(binding_field.kind) else {
        panic!(
            "{table}.{column}: bindingKey column {}.{binding} has type {}, which is not usable as a key (big_integer, uuid, string)",
            target.name,
            binding_field.kind.as_str()
        );
    };
    if binding_kind != key {
        panic!(
            "{table}.{column}: association value type {} does not match bindingKey column {}.{binding} of type {}",
            key.as_str(),
            target.name,
            binding_kind.as_str()
        );
    }
    let Some(field) = owner.fields.iter().find(|f| f.name == column) else {
        return;
    };
    if key_kind_of(field.kind) != Some(binding_kind) {
        panic!(
            "{table}.{column}: foreign key column type {} does not match bindingKey column {}.{binding} of type {}",
            field.kind.as_str(),
            target.name,
            binding_kind.as_str()
        );
    }
}

fn key_kind_of(kind: FieldKind) -> Option<KeyKind> {
    match kind {
        FieldKind::BigInteger => Some(KeyKind::BigInteger),
        FieldKind::Uuid => Some(KeyKind::Uuid),
        FieldKind::String { .. } => Some(KeyKind::String),
        _ => None,
    }
}
