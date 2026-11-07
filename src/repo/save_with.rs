//! `RepoHandle::save_with` and its builder `SaveWith`: save a row together with its associations.
//! The writing is done by `PreparedChildren` and `RepoHandle::save_associated`; this module only
//! checks declarations against the registry and lines up the rows.

use crate::repo::GuardedRepo;
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::ValueType;
use sea_orm::{ActiveModelTrait, ActiveValue, DbErr, EntityTrait, IdenStatic};

use super::{Repo, RepoEntity, RepoError, RepoHandle};
use crate::association::{
    Association, BelongsToOf, ChildRow, ChildRows, HasManyOf, HasOneOf, LinkSpec, LinksOf,
    ManyToManyOf, ParentRow, PreparedChildren, PreparedParent, RelationCondition, SaveStrategy,
    UnlinkChildren, UnlinkLinks, fk_column, handle_of, primary_column, resolve, row_prefix,
};
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::FieldErrors;

impl<'c, R, E> RepoHandle<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    /// Saves a parent `am` together with its associations. Add associations (`replace`, `append`,
    /// `replace_links`, ..) and write with `save()`.
    ///
    /// ```text
    /// let saved: orders::Model = ctx.repo::<OrdersRepo>()
    ///     .save_with(order_am)
    ///     .replace(OrdersRepo::ITEMS, item_ams)
    ///     .replace_links(OrdersRepo::TAGS, tag_ids)
    ///     .save()
    ///     .await?;
    /// ```
    ///
    /// Pass `Nested::new(am)` as a child row to also write that row's associations; use
    /// `replace_nested` / `append_nested` for `Nested` rows. Error keys are paths such as
    /// `orders.1.items.0.quantity` or `orders.0.tags.1`. Any failure rolls everything back.
    ///
    /// - `save` opens its own transaction (a savepoint inside one), unlike `RepoHandle::save`.
    /// - `save_with` sets the child's foreign key and `seq`; a row with the foreign key `Set` is a
    ///   `ManagedColumn`. `replace` and `replace_links` renumber `seq` in the order sent; `append`
    ///   leaves it.
    /// - Children and join rows are written through their own Repo, so its permissions, `validate`
    ///   and hooks apply. From an `unchecked()` handle they are written unchecked too.
    /// - The parent's `before_save`/`after_save` run before the children's. If only children or
    ///   links changed in an update, the parent's `modified` advances last.
    /// - Errors: second-stage validation is `Validation` (keys `items.N.field`, `tags.N`), an
    ///   optimistic-lock failure `Conflict`, a missing permission `Forbidden`. A declaration that
    ///   does not match the registry, or a repeated association, is a `Db` error returned by `save`.
    pub fn save_with(&self, am: E::ActiveModel) -> SaveWith<'c, R> {
        SaveWith {
            ctx: self.context(),
            unchecked: self.is_unchecked(),
            root: Nested::new(am),
            expected: None,
        }
    }
}

/// A row with associations, passed to `save_with` as a child or target row.
///
/// Wrap a row in `Nested::new(am)` to also write its associations; `am.into()` makes a `Nested`
/// without any. Use `replace_nested` / `append_nested` for `Nested` rows, so that an empty `vec![]`
/// in `replace` needs no type annotation.
///
/// ```text
/// let saved = ctx.repo::<CustomersRepo>()
///     .save_with(customer_am)
///     .append_nested(
///         CustomersRepo::ORDERS,
///         vec![
///             Nested::new(order_am).replace(OrdersRepo::ITEMS, item_ams),
///             other_order_am.into(),
///         ],
///     )
///     .save()
///     .await?;
/// ```
///
/// Declarations are checked against the registry when passed to `save_with`; a mismatch or a
/// repeated association in one row is a `Db` error returned by `save`. Each level is written
/// through its own Repo; the parent sets foreign keys and `seq`. Error keys are paths such as
/// `orders.0.product.field`.
pub struct Nested<T: GuardedRepo> {
    am: <T::Entity as EntityTrait>::ActiveModel,
    steps: Vec<Step>,
}

type Step = Box<dyn FnOnce(&Context, &mut Prep) -> Result<(), RepoError> + Send>;

#[derive(Default)]
struct Prep {
    used: Vec<&'static str>,
    parents: Vec<Box<dyn PreparedParent>>,
    prepared: Vec<Box<dyn PreparedChildren>>,
    // Input mistakes such as a repeated id; keys are paths from this row.
    input_errors: FieldErrors,
}

impl Prep {
    fn use_name<R: Repo>(&mut self, name: &'static str) -> Result<(), RepoError> {
        if self.used.contains(&name) {
            return Err(RepoError::Db(DbErr::Custom(format!(
                "{}: association {name} is specified twice",
                R::repo_name()
            ))));
        }
        self.used.push(name);
        Ok(())
    }
}

// A blanket `From<A>` instead of `From<ActiveModel>` avoids clashing with `From<T> for T`, since
// `Nested` is not an `ActiveModelTrait`.
impl<T, A> From<A> for Nested<T>
where
    T: GuardedRepo,
    A: ActiveModelTrait<Entity = T::Entity> + Into<<T::Entity as EntityTrait>::ActiveModel>,
{
    fn from(am: A) -> Self {
        Self::new(am.into())
    }
}

impl<T: GuardedRepo> Nested<T> {
    /// Starts from a row without associations.
    pub fn new(am: <T::Entity as EntityTrait>::ActiveModel) -> Self {
        Self {
            am,
            steps: Vec::new(),
        }
    }

    /// hasMany: the rows sent are authoritative, as `SaveWith::replace`. Rows are plain
    /// `ActiveModel`s.
    pub fn replace<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<T, C, K>,
        rows: Vec<<C::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.has_many(association, wrap(rows), SaveStrategy::Replace)
    }

    /// hasMany: adds or updates only, as `SaveWith::append`. Rows are plain `ActiveModel`s.
    pub fn append<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<T, C, K>,
        rows: Vec<<C::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.has_many(association, wrap(rows), SaveStrategy::Append)
    }

    /// Like `replace`, for `Nested` rows.
    pub fn replace_nested<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<T, C, K>,
        rows: Vec<Nested<C>>,
    ) -> Self {
        self.has_many(association, rows, SaveStrategy::Replace)
    }

    /// Like `append`, for `Nested` rows.
    pub fn append_nested<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<T, C, K>,
        rows: Vec<Nested<C>>,
    ) -> Self {
        self.has_many(association, rows, SaveStrategy::Append)
    }

    /// many-to-many: relinks to the given target ids, as `SaveWith::replace_links`.
    pub fn replace_links<J: GuardedRepo, U: GuardedRepo, K: RowKey, TK: RowKey>(
        mut self,
        association: ManyToManyOf<T, J, U, K, TK>,
        target_ids: Vec<Key<U>>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_links::<T, J, U, K, TK>(ctx, prep, association, target_ids, false)
        }));
        self
    }

    /// hasOne: one target row, as `SaveWith::has_one`.
    pub fn has_one<C: GuardedRepo, K: RowKey>(
        self,
        association: HasOneOf<T, C, K>,
        row: <C::Entity as EntityTrait>::ActiveModel,
    ) -> Self {
        self.has_one_nested(association, Nested::new(row))
    }

    /// Like `has_one`, for a `Nested` target.
    pub fn has_one_nested<C: GuardedRepo, K: RowKey>(
        mut self,
        association: HasOneOf<T, C, K>,
        row: Nested<C>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_has_one::<T, C, K>(ctx, prep, association, row)
        }));
        self
    }

    /// belongsTo: saves the target first and stores its id, as `SaveWith::belongs_to`.
    pub fn belongs_to<U: GuardedRepo, K: RowKey>(
        mut self,
        association: BelongsToOf<T, U, K>,
        target: impl Into<Nested<U>>,
    ) -> Self {
        let target = target.into();
        self.steps.push(Box::new(move |ctx, prep| {
            prep_belongs_to::<T, U, K>(ctx, prep, association, target)
        }));
        self
    }

    fn has_many<C: GuardedRepo, K: RowKey>(
        mut self,
        association: HasManyOf<T, C, K>,
        rows: Vec<Nested<C>>,
        save: SaveStrategy,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_has_many::<T, C, K>(ctx, prep, association, rows, save, false)
        }));
        self
    }

    pub(crate) fn link_children<C: GuardedRepo, K: RowKey>(
        mut self,
        association: HasManyOf<T, C, K>,
        rows: Vec<<C::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_has_many::<T, C, K>(
                ctx,
                prep,
                association,
                wrap(rows),
                SaveStrategy::Append,
                true,
            )
        }));
        self
    }

    pub(crate) fn unlink_children<C: GuardedRepo, K: RowKey>(
        mut self,
        association: HasManyOf<T, C, K>,
        ids: Vec<Key<C>>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_unlink_children::<T, C, K>(ctx, prep, association, ids)
        }));
        self
    }

    pub(crate) fn link_targets<J: GuardedRepo, U: GuardedRepo, K: RowKey, TK: RowKey>(
        mut self,
        association: ManyToManyOf<T, J, U, K, TK>,
        target_ids: Vec<Key<U>>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_links::<T, J, U, K, TK>(ctx, prep, association, target_ids, true)
        }));
        self
    }

    pub(crate) fn unlink_targets<J: GuardedRepo, U: GuardedRepo, K: RowKey, TK: RowKey>(
        mut self,
        association: ManyToManyOf<T, J, U, K, TK>,
        target_ids: Vec<Key<U>>,
    ) -> Self {
        self.steps.push(Box::new(move |ctx, prep| {
            prep_unlink_links::<T, J, U, K, TK>(ctx, prep, association, target_ids)
        }));
        self
    }

    fn prepare(
        self,
        ctx: &Context,
    ) -> Result<(<T::Entity as EntityTrait>::ActiveModel, Prep), RepoError> {
        let mut prep = Prep::default();
        for step in self.steps {
            step(ctx, &mut prep)?;
        }
        Ok((self.am, prep))
    }
}

/// The builder returned by `save_with`. Nothing touches the DB until `save`.
pub struct SaveWith<'c, R: GuardedRepo> {
    ctx: &'c Context,
    unchecked: bool,
    root: Nested<R>,
    expected: Option<DateTime<Utc>>,
}

impl<'c, R, E> SaveWith<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    fn root(mut self, f: impl FnOnce(Nested<R>) -> Nested<R>) -> Self {
        self.root = f(self.root);
        self
    }

    /// hasMany: the rows sent are authoritative. Rows with a primary key are updated, others
    /// inserted, and current children not sent are deleted. `seq` is renumbered from 1 in the order
    /// sent. Rows are plain `ActiveModel`s.
    pub fn replace<T: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, T, K>,
        rows: Vec<<T::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.root(|n| n.replace(association, rows))
    }

    /// Like `replace`, for `Nested` rows.
    pub fn replace_nested<T: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, T, K>,
        rows: Vec<Nested<T>>,
    ) -> Self {
        self.root(|n| n.replace_nested(association, rows))
    }

    /// Like `append`, for `Nested` rows.
    pub fn append_nested<T: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, T, K>,
        rows: Vec<Nested<T>>,
    ) -> Self {
        self.root(|n| n.append_nested(association, rows))
    }

    /// hasMany: adds or updates only; current children not sent stay and `seq` is not renumbered.
    pub fn append<T: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, T, K>,
        rows: Vec<<T::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.root(|n| n.append(association, rows))
    }

    /// many-to-many: relinks to the given target ids. Dropped links are deleted and `seq` follows
    /// the order sent.
    pub fn replace_links<J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
        self,
        association: ManyToManyOf<R, J, T, K, TK>,
        target_ids: Vec<Key<T>>,
    ) -> Self {
        self.root(|n| n.replace_links(association, target_ids))
    }

    pub(crate) fn link_children<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, C, K>,
        rows: Vec<<C::Entity as EntityTrait>::ActiveModel>,
    ) -> Self {
        self.root(|n| n.link_children(association, rows))
    }

    pub(crate) fn unlink_children<C: GuardedRepo, K: RowKey>(
        self,
        association: HasManyOf<R, C, K>,
        ids: Vec<Key<C>>,
    ) -> Self {
        self.root(|n| n.unlink_children(association, ids))
    }

    pub(crate) fn link_targets<J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
        self,
        association: ManyToManyOf<R, J, T, K, TK>,
        target_ids: Vec<Key<T>>,
    ) -> Self {
        self.root(|n| n.link_targets(association, target_ids))
    }

    pub(crate) fn unlink_targets<J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
        self,
        association: ManyToManyOf<R, J, T, K, TK>,
        target_ids: Vec<Key<T>>,
    ) -> Self {
        self.root(|n| n.unlink_targets(association, target_ids))
    }

    /// hasOne: saves one target row after the parent. Without a primary key it is inserted with the
    /// parent's value in the foreign key; with one it is updated. The current target is not
    /// replaced. Use `has_one_nested` to also write the target's associations.
    pub fn has_one<C: GuardedRepo, K: RowKey>(
        self,
        association: HasOneOf<R, C, K>,
        row: <C::Entity as EntityTrait>::ActiveModel,
    ) -> Self {
        self.root(|n| n.has_one(association, row))
    }

    /// Like `has_one`, for a `Nested` target.
    pub fn has_one_nested<C: GuardedRepo, K: RowKey>(
        self,
        association: HasOneOf<R, C, K>,
        row: Nested<C>,
    ) -> Self {
        self.root(|n| n.has_one_nested(association, row))
    }

    /// belongsTo: saves the target before the parent and stores its id in the parent's foreign key
    /// (a target with an id is updated). It is written through its own Repo, so its permissions,
    /// `validate` and hooks apply; its validation errors are keyed `name.field`. Pass a `Nested`
    /// to also write the target's associations.
    pub fn belongs_to<T: GuardedRepo, K: RowKey>(
        self,
        association: BelongsToOf<R, T, K>,
        target: impl Into<Nested<T>>,
    ) -> Self {
        self.root(|n| n.belongs_to(association, target))
    }

    /// Optimistic lock for updates: `Conflict` if the parent's current `modified` differs.
    pub fn if_unmodified(mut self, expected_modified: DateTime<Utc>) -> Self {
        self.expected = Some(expected_modified);
        self
    }

    /// Writes the parent, then the associations, in one transaction and returns the saved parent.
    ///
    /// Commits on success. On failure rolls back and returns the error as is (`Conflict`,
    /// `Validation`, `Forbidden`, ..). Preparation errors (declaration mismatch, `ManagedColumn`)
    /// and input mistakes are returned before any DB access, the first one first.
    pub async fn save(self) -> Result<E::Model, RepoError> {
        let SaveWith {
            ctx,
            unchecked,
            root,
            expected,
        } = self;
        let (
            am,
            Prep {
                parents,
                prepared,
                input_errors,
                ..
            },
        ) = root.prepare(ctx)?;
        if !input_errors.is_empty() {
            return Err(RepoError::Validation(input_errors));
        }
        let adding = matches!(
            am.get(crate::association::primary_column::<R>()),
            sea_orm::ActiveValue::NotSet
        );
        let tx = ctx.begin().await?;
        let result = handle_of::<R>(&tx, unchecked)
            .save_associated(am, expected, parents, prepared, adding)
            .await;
        tx.finish(result).await
    }
}

fn wrap<T: GuardedRepo>(rows: Vec<<T::Entity as EntityTrait>::ActiveModel>) -> Vec<Nested<T>> {
    rows.into_iter().map(Nested::new).collect()
}

fn prep_links<R: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: ManyToManyOf<R, J, T, K, TK>,
    target_ids: Vec<Key<T>>,
    add_only: bool,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, ManyToManyOf<R, J, T, K, TK>>(ctx, name)?;
    let (foreign_key, target_foreign_key, filter) = (
        registered.foreign_key().as_str(),
        registered.target_foreign_key().as_str(),
        registered.relation_condition(),
    );
    let mut seen: HashSet<Key<T>> = HashSet::new();
    for (position, id) in target_ids.iter().enumerate() {
        if !seen.insert(id.clone()) {
            prep.input_errors.add(
                &format!("{name}.{position}"),
                "duplicate",
                &ctx.registry().messages().text("duplicate", &[]),
            );
        }
    }
    let ids: Vec<RowId> = target_ids
        .iter()
        .cloned()
        .map(RowKey::into_row_id)
        .collect();
    let spec = LinkSpec {
        name,
        parent_column: foreign_key,
        target_column: target_foreign_key,
        binding_key: Association::binding_key(registered),
        target_binding_key: registered.target_binding_name(),
        filter,
    };
    let mut links = LinksOf::<J, T, TK>::new(&spec, &ids);
    links.add_only = add_only;
    prep.prepared.push(Box::new(links));
    Ok(())
}

fn prep_unlink_links<R: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: ManyToManyOf<R, J, T, K, TK>,
    target_ids: Vec<Key<T>>,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, ManyToManyOf<R, J, T, K, TK>>(ctx, name)?;
    let spec = LinkSpec {
        name,
        parent_column: registered.foreign_key().as_str(),
        target_column: registered.target_foreign_key().as_str(),
        binding_key: Association::binding_key(registered),
        target_binding_key: registered.target_binding_name(),
        filter: registered.relation_condition(),
    };
    let ids: Vec<RowId> = target_ids.into_iter().map(RowKey::into_row_id).collect();
    prep.prepared.push(Box::new(UnlinkLinks::<J, T>::new(
        &spec,
        Association::cascade_callbacks(registered),
        &ids,
    )));
    Ok(())
}

fn prep_unlink_children<R: GuardedRepo, T: GuardedRepo, K: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: HasManyOf<R, T, K>,
    ids: Vec<Key<T>>,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, HasManyOf<R, T, K>>(ctx, name)?;
    prep.prepared.push(Box::new(UnlinkChildren::<T> {
        name: name.to_string(),
        column: registered.foreign_key().as_str().to_string(),
        binding_key: Association::binding_key(registered),
        dependent: Association::dependent(registered),
        cascade_callbacks: Association::cascade_callbacks(registered),
        filter: registered.relation_condition(),
        ids,
    }));
    Ok(())
}

fn prep_belongs_to<R: GuardedRepo, T: GuardedRepo, K: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: BelongsToOf<R, T, K>,
    target: Nested<T>,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, BelongsToOf<R, T, K>>(ctx, name)?;
    let foreign_key = registered.foreign_key().as_str();
    let binding_key = Association::binding_key(registered);
    let (am, inner) = target.prepare(ctx)?;
    prep.input_errors.merge_prefixed(name, inner.input_errors);
    prep.parents.push(Box::new(ParentRow::<T> {
        name,
        foreign_key,
        binding_key,
        am,
        parents: inner.parents,
        prepared: inner.prepared,
    }));
    Ok(())
}

fn prep_has_many<R: GuardedRepo, T: GuardedRepo, K: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: HasManyOf<R, T, K>,
    rows: Vec<Nested<T>>,
    save: SaveStrategy,
    link: bool,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, HasManyOf<R, T, K>>(ctx, name)?;
    let (foreign_key, filter) = (
        registered.foreign_key().as_str(),
        registered.relation_condition(),
    );
    let binding_key = Association::binding_key(registered);
    prep_child_rows::<T>(
        ctx,
        prep,
        ChildTarget {
            name,
            foreign_key,
            binding_key,
            filter,
            dependent: Association::dependent(registered),
            cascade_callbacks: Association::cascade_callbacks(registered),
            link,
            single: false,
        },
        rows,
        save,
    )
}

fn prep_has_one<R: GuardedRepo, T: GuardedRepo, K: RowKey>(
    ctx: &Context,
    prep: &mut Prep,
    association: HasOneOf<R, T, K>,
    row: Nested<T>,
) -> Result<(), RepoError> {
    let name = association.name();
    prep.use_name::<R>(name)?;
    let registered = resolve::<R, HasOneOf<R, T, K>>(ctx, name)?;
    let target = ChildTarget {
        name,
        foreign_key: registered.foreign_key().as_str(),
        binding_key: Association::binding_key(registered),
        filter: registered.relation_condition(),
        dependent: false,
        cascade_callbacks: true,
        link: false,
        single: true,
    };
    prep_child_rows::<T>(ctx, prep, target, vec![row], SaveStrategy::Append)
}

struct ChildTarget {
    name: &'static str,
    foreign_key: &'static str,
    binding_key: Option<&'static str>,
    filter: RelationCondition,
    dependent: bool,
    cascade_callbacks: bool,
    link: bool,
    // A hasOne row; error keys carry no row index.
    single: bool,
}

fn prep_child_rows<T: GuardedRepo>(
    ctx: &Context,
    prep: &mut Prep,
    target: ChildTarget,
    rows: Vec<Nested<T>>,
    save: SaveStrategy,
) -> Result<(), RepoError> {
    let ChildTarget {
        name,
        foreign_key,
        binding_key,
        filter,
        dependent,
        cascade_callbacks,
        link,
        single,
    } = target;
    let fk = fk_column::<T>(name, foreign_key)?;
    let pk = primary_column::<T>();
    let mut seen: HashSet<Key<T>> = HashSet::new();
    let mut child_rows = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        let (am, inner) = row.prepare(ctx)?;
        if matches!(am.get(fk), ActiveValue::Set(_)) {
            return Err(RepoError::ManagedColumn(foreign_key.to_string()));
        }
        let id = match am.get(pk) {
            ActiveValue::Set(v) | ActiveValue::Unchanged(v) => Some(
                <Key<T> as ValueType>::try_from(v)
                    .map_err(|_| DbErr::Custom("cannot read the primary key".to_string()))?,
            ),
            ActiveValue::NotSet => None,
        };
        if let Some(id) = &id
            && !seen.insert(id.clone())
        {
            prep.input_errors.add(
                &format!("{}.id", row_prefix(name, index, single)),
                "duplicate",
                &ctx.registry().messages().text("duplicate", &[]),
            );
        }
        prep.input_errors
            .merge_prefixed(&row_prefix(name, index, single), inner.input_errors);
        child_rows.push(ChildRow {
            index,
            id,
            delete: false,
            am: Some(am),
            parents: inner.parents,
            prepared: inner.prepared,
        });
    }
    prep.prepared.push(Box::new(ChildRows::<T> {
        name: name.to_string(),
        column: foreign_key.to_string(),
        binding_key,
        dependent,
        cascade_callbacks,
        link,
        save,
        filter,
        single,
        rows: child_rows,
    }));
    Ok(())
}
