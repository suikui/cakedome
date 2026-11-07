//! `RepoHandle`, the entry point for reading, saving and deleting rows through the Policy.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{ValueType, sea_value_to_json_value};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, DbErr, EntityTrait, IdenStatic,
    IntoActiveModel, Iterable, ModelTrait, PrimaryKeyTrait, QueryFilter, QuerySelect, Value,
};

use super::before_find::run_before_find;
use super::guard::{authorize, base_condition, registered, scoped_condition};
use super::{Find, FindOptions, GuardedRepo, Loaded, RepoEntity, RepoError, typed_decl_of};
use crate::association::{
    Association, LoadQuery, Loadable, PreparedChildren, PreparedParent, primary_column,
    row_id_of_value,
};
use crate::clock;
use crate::context::Context;
use crate::key::{Key, RowId, RowKey};
use crate::marshal::{FieldErrors, WriteMode};
use crate::messages::params;
use crate::meta::{Lifecycle, TableSpec, column_named};
use crate::observer::{Change, ChangeKind};
use crate::policy::resolve as policy_resolve;
use crate::policy::{Action, ColumnAccess};
use crate::validate::{ValidationInput, Warning};

/// Reads and writes the rows of Repo `R`'s Entity through its Policy. Created by `ctx.repo::<R>()`.
///
/// - Read: `get`, `get_detail`, `find`, `load`, `load_where`, `to_json`.
/// - Write: `save`, `insert`, `save_if_unmodified`, `save_with`, `touch`, `delete`, `link`,
///   `unlink`, `replace`, `replace_links`.
/// - Authorization: `check`, `can`, `columns`.
/// - Modes: `unchecked` skips the Policy; `option` passes a value to `before_find`.
pub struct RepoHandle<'c, R: GuardedRepo> {
    ctx: &'c Context,
    // Ties `R` to the handle. `fn() -> R` keeps `Send`/`Sync` independent of `R`.
    _repo: PhantomData<fn() -> R>,
    unchecked: bool,
    // Set only when saving from request data; `None` skips Warning checks.
    accepted_warnings: Option<Vec<String>>,
    options: FindOptions,
    // False skips `before_find` when reading before get, update, touch and delete (dependent child
    // deletes).
    before_find: bool,
}

type BelongsToRef = (Arc<dyn Association>, RowId);

impl<'c, R, E> RepoHandle<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    pub(crate) fn new(ctx: &'c Context) -> Self {
        Self {
            ctx,
            _repo: PhantomData,
            unchecked: false,
            accepted_warnings: None,
            options: FindOptions::default(),
            before_find: true,
        }
    }

    /// Passes a value to `before_find`, looked up by type. It reaches `get`, the reads before
    /// updates and deletes, and any `find` made from this handle.
    pub fn option<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.options.insert(value);
        self
    }

    pub(crate) fn without_before_find(mut self) -> Self {
        self.before_find = false;
        self
    }

    async fn before_find_condition(
        &self,
        ctx: &Context,
        repo: &R,
        action: Action,
    ) -> Result<Condition, RepoError> {
        if !self.before_find {
            return Ok(Condition::all());
        }
        Ok(run_before_find::<R>(ctx, repo, action, &self.options, true)
            .await?
            .condition)
    }

    pub(crate) fn accepting_warnings(self, accepted: Vec<String>) -> Self {
        Self {
            accepted_warnings: Some(accepted),
            ..self
        }
    }

    /// Returns a handle that skips the Policy checks, for login, seeds and batches.
    ///
    /// Skips `can`, `scope`, `can_write`, `can_add`, `can_edit` and `can_delete`, but still
    /// requires the Repo to be registered (`NotRegistered`) and marks the request as authorized.
    /// Soft-delete conditions, `created`/`modified` handling, `ManagedColumn` checks and the delete
    /// behavior per lifecycle apply as usual. `find()` inherits the mode.
    pub fn unchecked(self) -> Self {
        Self {
            unchecked: true,
            ..self
        }
    }

    /// Returns the Context, so `Loadable::load` can read targets in the same mode as
    /// `is_unchecked`.
    pub fn context(&self) -> &'c Context {
        self.ctx
    }

    /// Returns whether this handle is in `unchecked` mode.
    pub fn is_unchecked(&self) -> bool {
        self.unchecked
    }

    /// Starts a query. The Policy is checked when `all` or `count` runs.
    pub fn find(&self) -> Find<'c, R> {
        Find {
            ctx: self.ctx,
            unchecked: self.unchecked,
            condition: Condition::all(),
            orders: Vec::new(),
            limit: None,
            offset: None,
            options: self.options.clone(),
            primary: true,
        }
    }

    /// Returns one row, checking `can(View)` and `scope(View)` (and `is_active = TRUE` for soft
    /// delete).
    ///
    /// Returns `Forbidden` (`Unauthenticated` if not logged in) without permission, and `NotFound`
    /// for a missing, out-of-scope or deleted row.
    pub async fn get(&self, id: Key<R>) -> Result<E::Model, RepoError> {
        let guard = authorize::<R>(self.ctx, Action::View, self.unchecked).await?;
        let condition = base_condition::<E>(self.ctx, &guard, Action::View).await?;
        let found = self
            .before_find_condition(self.ctx, &guard.repo, Action::View)
            .await?;
        let model = E::find_by_id(id)
            .filter(condition)
            .filter(found)
            .one(&self.ctx.conn())
            .await?
            .ok_or(RepoError::NotFound)?;
        Ok(model)
    }

    /// Reads one row like `get` and contains the associations of `default_contain`.
    pub async fn get_detail(&self, id: Key<R>) -> Result<super::Loaded<E::Model>, RepoError> {
        let column = crate::association::primary_column::<R>();
        self.find()
            .filter(sea_orm::ColumnTrait::eq(&column, id))
            .contain_default()
            .all()
            .await?
            .into_iter()
            .next()
            .ok_or(RepoError::NotFound)
    }

    /// Loads an association for `rows`, reading the targets with one `IN` query.
    ///
    /// belongsTo returns target id to model (rows with a null foreign key are skipped); hasMany and
    /// many-to-many return parent id to `Vec` in the target's default order. Targets are read
    /// through their own Repo, so its `can(View)`, scope and soft delete apply (inactive
    /// `Activatable` rows are read). Returns `Forbidden` if `can_contain` is false, and `Db` if the
    /// declaration does not match the registry. Returns an empty map without a query if `rows` is
    /// empty. `unchecked` also applies to the targets. The foreign key's `can_read` is not checked.
    ///
    /// ```text
    /// let orders = ctx.repo::<OrdersRepo>().find().filter(..).all().await?;
    /// let items: HashMap<Key<OrdersRepo>, Vec<order_items::Model>> =
    ///     ctx.repo::<OrdersRepo>().load(&orders, OrdersRepo::ITEMS).await?;
    /// ```
    pub async fn load<A: Loadable<R>>(
        &self,
        rows: &[E::Model],
        association: A,
    ) -> Result<A::Output, RepoError> {
        self.load_where(rows, association, |q| q).await
    }

    /// Like `load`, with conditions and ordering on the targets built by the closure.
    ///
    /// The target's Policy and the `IN` condition cannot be removed; conditions are ANDed. There is
    /// no `limit`. Order is the given ordering, then the target's default.
    ///
    /// - hasMany: orders each parent's children.
    /// - belongsTo: the result is a map, so ordering is ignored.
    /// - many-to-many: the join table gets no conditions. Without ordering, each parent's targets
    ///   follow the join table; with it, the target read order. Targets that do not match are
    ///   omitted.
    ///
    /// ```text
    /// let items = ctx.repo::<OrdersRepo>().load_where(&orders, OrdersRepo::ITEMS, |q| {
    ///     q.filter(order_items::Column::Quantity.gte(5))
    ///         .order_by_desc(order_items::Column::Quantity)
    /// }).await?;
    /// ```
    pub async fn load_where<A, F>(
        &self,
        rows: &[E::Model],
        association: A,
        query: F,
    ) -> Result<A::Output, RepoError>
    where
        A: Loadable<R>,
        F: FnOnce(LoadQuery<A::Target>) -> LoadQuery<A::Target>,
    {
        association.load(self, rows, query(LoadQuery::new())).await
    }

    /// Inserts or updates a row and returns the saved row. A `NotSet` primary key inserts; `Set` or
    /// `Unchanged` updates.
    ///
    /// Columns written are `Set` and `Unchanged` on insert, and `Set` only on update. Returns
    /// `ManagedColumn` first if a managed column (`created`, `modified`, or `is_active` of soft
    /// delete) is written. A `NotSet` `is_active` of an `Activatable` Entity is set to TRUE.
    ///
    /// - Insert: `can(Add)`, then `can_write` per column, then `can_add`; `Forbidden` on failure.
    ///   Sets `created`, `modified` and, for soft delete, `is_active`.
    /// - Update: `can(Edit)`, then reads the current row under `scope(Edit)` (`NotFound` if
    ///   missing), then `can_write` per `Set` column (primary key excluded) and `can_edit`.
    ///   Without `Set` columns it writes nothing and returns the current row; otherwise it sets
    ///   `modified`.
    ///
    /// `Repo::validate` runs after `can_add`/`can_edit`, also when `unchecked`, and belongsTo
    /// targets are checked with `invalid_ref` (skipped when `unchecked`). Errors are returned
    /// together as `Validation`. For a caller-chosen primary key on insert, use `insert`.
    pub async fn save(&self, am: E::ActiveModel) -> Result<E::Model, RepoError> {
        let adding = matches!(am.get(primary_column::<R>()), ActiveValue::NotSet);
        self.save_inner(am, None, adding).await
    }

    /// Inserts a row whether or not the primary key is `Set`, for caller-supplied (e.g. string)
    /// keys. A `NotSet` key comes from `new_key`. A duplicate is a `unique` validation error.
    /// Otherwise it behaves like the insert path of `save`.
    pub async fn insert(&self, am: E::ActiveModel) -> Result<E::Model, RepoError> {
        self.save_inner(am, None, true).await
    }

    /// Updates a row only if its `modified` equals `expected_modified` (optimistic locking).
    ///
    /// Updates only: a `NotSet` primary key or an Entity without `modified` is a `Db(Custom)`
    /// error. Order: `can(Edit)`, read the row under `scope(Edit)` with `SELECT .. FOR UPDATE`
    /// (`NotFound` if missing), `Conflict` if `modified` differs, then as the update path of
    /// `save`.
    ///
    /// Call it inside a transaction (`ctx.begin()`) so the row lock holds. On MySQL and PostgreSQL
    /// a concurrent save waits for the first to commit, then gets `Conflict`. SQLite has no row
    /// lock: the later save fails with `Db` (`database is locked`) instead, and never overwrites.
    pub async fn save_if_unmodified(
        &self,
        am: E::ActiveModel,
        expected_modified: DateTime<Utc>,
    ) -> Result<E::Model, RepoError> {
        self.save_inner(am, Some(expected_modified), false).await
    }

    // Call inside a transaction; the caller commits or rolls back.
    pub(crate) async fn save_associated(
        &self,
        am: E::ActiveModel,
        expected_modified: Option<DateTime<Utc>>,
        parents: Vec<Box<dyn PreparedParent>>,
        prepared: Vec<Box<dyn PreparedChildren>>,
        adding: bool,
    ) -> Result<E::Model, RepoError> {
        self.save_associated_inner(am, expected_modified, parents, prepared, adding)
            .await
            .map(|(model, _)| model)
    }

    // Also returns whether children or links changed (not belongsTo targets), to decide whether the
    // parent's `modified` advances.
    pub(crate) async fn save_associated_inner(
        &self,
        am: E::ActiveModel,
        expected_modified: Option<DateTime<Utc>>,
        parents: Vec<Box<dyn PreparedParent>>,
        prepared: Vec<Box<dyn PreparedChildren>>,
        adding: bool,
    ) -> Result<(E::Model, bool), RepoError> {
        let pk = primary_column::<R>();
        let mut am = am;
        // Written before the parent so the new id can fill its foreign key.
        let mut parent_errors = FieldErrors::default();
        for one in parents {
            let column = column_named::<E>(one.foreign_key()).ok_or_else(|| {
                DbErr::Custom(format!(
                    "foreign key column {} does not exist",
                    one.foreign_key()
                ))
            })?;
            if let Some(value) = one
                .write(self.ctx, self.unchecked, &mut parent_errors)
                .await?
            {
                am.try_set(column, value)?;
            }
        }
        if !parent_errors.is_empty() {
            return Err(RepoError::Validation(parent_errors));
        }
        let updating = !adding;
        let mut model = match expected_modified {
            _ if adding => self.insert(am).await?,
            Some(expected) => self.save_if_unmodified(am, expected).await?,
            None => self.save(am).await?,
        };
        let parent_id = <Key<R> as ValueType>::try_from(model.get(pk))
            .map_err(|_| DbErr::Custom("cannot read the primary key".to_string()))?;
        let mut errors = FieldErrors::default();
        let mut changed = false;
        for one in prepared {
            // A child's foreign key may point at a `bindingKey` column other than the primary key.
            let parent_value = match one.parent_key() {
                Some(binding) => {
                    let column = column_named::<E>(binding)
                        .ok_or_else(|| DbErr::Custom(format!("column {binding} does not exist")))?;
                    row_id_of_value(model.get(column))
                }
                None => Some(parent_id.clone().into_row_id()),
            };
            changed |= one
                .write(self.ctx, parent_value, self.unchecked, &mut errors)
                .await?;
        }
        if !errors.is_empty() {
            return Err(RepoError::Validation(errors));
        }
        if changed && updating {
            model = self.touch(parent_id).await?;
        }
        Ok((model, changed))
    }

    /// Checks only registration and `can(action)`, and marks the request as authorized.
    ///
    /// For operations that read or write no row, such as a "new" form. Returns `Ok` when
    /// `unchecked`.
    pub async fn check(&self, action: Action) -> Result<(), RepoError> {
        authorize::<R>(self.ctx, action, self.unchecked).await?;
        Ok(())
    }

    /// Returns whether `action` is allowed in this Context (`TablePolicy::can`), e.g. to show a
    /// button.
    ///
    /// Does not mark the request as authorized. Returns true when `unchecked` and `NotRegistered`
    /// if the Repo is not registered.
    pub async fn can(&self, action: Action) -> Result<bool, RepoError> {
        let repo = registered::<R>(self.ctx)?;
        if self.unchecked {
            return Ok(true);
        }
        policy_resolve::can(self.ctx, &*repo, action).await
    }

    /// Returns the readable and writable columns and the per-row hiding conditions
    /// (`TablePolicy::columns`), without checking `can`. All columns when `unchecked`.
    pub async fn columns(&self) -> Result<Arc<ColumnAccess<E>>, RepoError> {
        let repo = registered::<R>(self.ctx)?;
        if self.unchecked {
            return Ok(Arc::new(ColumnAccess::all()));
        }
        policy_resolve::columns(self.ctx, &*repo).await
    }

    // Only non-null values, and on update only values changed from the current row, are checked.
    fn pre_validate(
        &self,
        repo: &R,
        mode: WriteMode,
        before: Option<&E::Model>,
        after: &E::ActiveModel,
    ) -> (FieldErrors, Vec<BelongsToRef>) {
        let registry = self.ctx.registry();
        let mut errors = FieldErrors::default();
        repo.validate(
            &ValidationInput {
                identity: self.ctx.identity(),
                mode,
                before,
                after,
            },
            &mut errors,
        );
        let mut refs = Vec::new();
        for relation in registry.relations_of::<E>().unwrap_or_default() {
            let Some(foreign_key) = relation.owner_foreign_key() else {
                continue;
            };
            let Some(column) = column_named::<E>(foreign_key) else {
                continue;
            };
            let value = after.get(column);
            if !is_written(&value, before.is_none()) {
                continue;
            }
            let Some(value) = value.into_value() else {
                continue;
            };
            let id = if relation.binding_key().is_some() {
                // Read by the foreign key column's type, which need not match the target's primary
                // key.
                row_id_of_value(value.clone())
            } else {
                let Some(decl) = typed_decl_of::<R>(relation.as_ref()) else {
                    // Cannot be checked, so report `invalid_ref` as `finish_validate` does.
                    errors.add(
                        foreign_key,
                        "invalid_ref",
                        &registry.messages().text("invalid_ref", &[]),
                    );
                    continue;
                };
                decl.target_id_from_json(&sea_value_to_json_value(&value))
            };
            let Some(id) = id else {
                continue;
            };
            if let Some(before) = before
                && before.get(column) == value
            {
                continue;
            }
            refs.push((relation.clone(), id));
        }
        (errors, refs)
    }

    async fn finish_validate(
        &self,
        errors: &mut FieldErrors,
        refs: Vec<BelongsToRef>,
    ) -> Result<(), RepoError> {
        // Unchecked (seeds, batches) leaves reference checks to the DB foreign keys.
        let refs = if self.unchecked { Vec::new() } else { refs };
        for (a, id) in refs {
            if !a.check_exists(self.ctx, &id).await?
                && let Some(column) = a.owner_foreign_key()
            {
                errors.add(
                    column,
                    "invalid_ref",
                    &self.ctx.registry().messages().text("invalid_ref", &[]),
                );
            }
        }
        Ok(())
    }

    // Reads without Policy, scope or soft delete, because DB unique constraints cover every row.
    // Runs also when `unchecked`.
    fn unique_checks(
        spec: &TableSpec,
        before: Option<&E::Model>,
        am: &E::ActiveModel,
    ) -> Vec<(&'static str, Condition)> {
        Self::set_checks(&spec.uniques, before, am)
    }

    fn set_checks(
        sets: &[Vec<&'static str>],
        before: Option<&E::Model>,
        am: &E::ActiveModel,
    ) -> Vec<(&'static str, Condition)> {
        let mut checks = Vec::new();
        for set in sets {
            let mut values = Vec::new();
            let mut changed = before.is_none();
            for name in set {
                let Some(column) = column_named::<E>(name) else {
                    break;
                };
                let current = before.map(|b| b.get(column));
                let value = match am.get(column) {
                    ActiveValue::Set(v) => {
                        if current.as_ref() != Some(&v) {
                            changed = true;
                        }
                        Some(v)
                    }
                    ActiveValue::Unchanged(v) => Some(v),
                    ActiveValue::NotSet => current,
                };
                values.push((column, value));
            }
            if values.len() != set.len() || !changed {
                continue;
            }
            let mut condition = Condition::all();
            let mut known = true;
            for (column, value) in values {
                match value {
                    Some(v) if v.as_null() != v => condition = condition.add(column.eq(v)),
                    _ => known = false,
                }
            }
            if !known {
                continue;
            }
            checks.push((set[0], condition));
        }
        checks
    }

    async fn run_unique(
        &self,
        before: Option<&E::Model>,
        checks: Vec<(&'static str, Condition)>,
        errors: &mut FieldErrors,
    ) -> Result<(), RepoError> {
        let pk = primary_column::<R>();
        for (key, condition) in checks {
            let mut select = E::find().filter(condition);
            if let Some(before) = before {
                select = select.filter(pk.ne(before.get(pk)));
            }
            if select.one(&self.ctx.conn()).await?.is_some() {
                errors.add(
                    key,
                    "unique",
                    &self.ctx.registry().messages().text("unique", &[]),
                );
            }
        }
        Ok(())
    }

    async fn run_rules(
        &self,
        repo: &R,
        spec: &TableSpec,
        mode: WriteMode,
        before: Option<&E::Model>,
        am: &mut E::ActiveModel,
    ) -> Result<(), RepoError> {
        repo.before_rules(self.ctx, am, mode, before).await?;
        let (mut errors, refs) = self.pre_validate(repo, mode, before, am);
        self.finish_validate(&mut errors, refs).await?;
        let checks = Self::unique_checks(spec, before, am);
        self.run_unique(before, checks, &mut errors).await?;
        repo.after_rules(self.ctx, am, mode, before, &mut errors)
            .await?;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(RepoError::Validation(errors))
        }
    }

    // Duplicate Warnings ignore soft-deleted rows.
    async fn check_warnings(
        &self,
        repo: &R,
        spec: &TableSpec,
        mode: WriteMode,
        before: Option<&E::Model>,
        am: &E::ActiveModel,
    ) -> Result<(), RepoError> {
        let Some(accepted) = &self.accepted_warnings else {
            return Ok(());
        };
        let pk = primary_column::<R>();
        let mut warnings = Vec::new();
        let active = (spec.lifecycle == Lifecycle::SoftDelete)
            .then(|| spec.active_column.and_then(column_named::<E>))
            .flatten();
        for (key, condition) in Self::set_checks(&spec.warn_duplicates, before, am) {
            let mut select = E::find().filter(condition);
            if let Some(before) = before {
                select = select.filter(pk.ne(before.get(pk)));
            }
            if let Some(active) = active {
                select = select.filter(active.eq(true));
            }
            if select.one(&self.ctx.conn()).await?.is_some() {
                let label = self
                    .ctx
                    .registry()
                    .meta_of::<E>()
                    .and_then(|m| m.fields.iter().find(|f| f.name == key))
                    .map_or(key, |f| f.label.as_str());
                let params = params([("label", label.into())]);
                let message = self
                    .ctx
                    .registry()
                    .messages()
                    .text_params("warning.duplicate", &params);
                warnings.push(Warning::new(key, "duplicate", &message).with_params(params));
            }
        }
        let input = ValidationInput {
            identity: self.ctx.identity(),
            mode,
            before,
            after: am,
        };
        warnings.extend(repo.warnings(self.ctx, &input).await?);
        warnings.retain(|w| !accepted.contains(&w.key));
        if warnings.is_empty() {
            Ok(())
        } else {
            Err(RepoError::Warnings(warnings))
        }
    }

    async fn save_inner(
        &self,
        mut am: E::ActiveModel,
        expected_modified: Option<DateTime<Utc>>,
        adding: bool,
    ) -> Result<E::Model, RepoError> {
        let pk = primary_column::<R>();
        let spec = self.ctx.registry().spec_of::<E>();
        let modified_column = spec.modified_column.and_then(column_named::<E>);
        if expected_modified.is_some() {
            if adding {
                return Err(
                    DbErr::Custom("save_if_unmodified only supports updates".to_string()).into(),
                );
            }
            if modified_column.is_none() {
                return Err(DbErr::Custom("the modified column does not exist".to_string()).into());
            }
        }
        for name in managed_columns(&spec) {
            if let Some(column) = column_named::<E>(name)
                && is_written(&am.get(column), adding)
            {
                return Err(RepoError::ManagedColumn(name.to_string()));
            }
        }
        let db = &self.ctx.conn();

        if adding {
            let guard = authorize::<R>(self.ctx, Action::Add, self.unchecked).await?;
            let access = guard.columns(self.ctx).await?;
            check_writable::<E>(&access, &am, pk, adding)?;
            if !guard.can_add(self.ctx, &am).await? {
                return Err(RepoError::Forbidden);
            }
            self.run_rules(&guard.repo, &spec, WriteMode::Add, None, &mut am)
                .await?;
            self.check_warnings(&guard.repo, &spec, WriteMode::Add, None, &am)
                .await?;
            guard
                .repo
                .before_save(self.ctx, &mut am, WriteMode::Add, None)
                .await?;
            let now = Value::from(clock::now());
            for name in [spec.created_column, spec.modified_column]
                .into_iter()
                .flatten()
            {
                if let Some(column) = column_named::<E>(name) {
                    am.try_set(column, now.clone())?;
                }
            }
            if let Some(column) = spec.active_column.and_then(column_named::<E>) {
                let unset = matches!(am.get(column), ActiveValue::NotSet);
                if spec.lifecycle == Lifecycle::SoftDelete || unset {
                    am.try_set(column, Value::from(true))?;
                }
            }
            // `required` when no key can be generated and the DB cannot AUTO_INCREMENT one.
            if matches!(am.get(pk), ActiveValue::NotSet) {
                match guard.repo.new_key() {
                    Some(key) => am.try_set(pk, key.into())?,
                    None if <E::PrimaryKey as PrimaryKeyTrait>::auto_increment() => {}
                    None => {
                        let mut errors = FieldErrors::default();
                        errors.add(
                            "id",
                            "required",
                            &self.ctx.registry().messages().text("required", &[]),
                        );
                        return Err(RepoError::Validation(errors));
                    }
                }
            }
            let saved = am.insert(db).await?;
            guard
                .repo
                .after_save(self.ctx, &saved, WriteMode::Add, None)
                .await?;
            let saved_id = crate::association::row_key::<R>(&saved)
                .ok_or_else(|| DbErr::Custom("the saved row has no id".to_string()))?;
            self.notify(self.ctx, ChangeKind::Add, saved_id, None, Some(&saved))
                .await?;
            self.ctx
                .after_save_commit(guard.repo.clone(), &saved, WriteMode::Add)
                .await;
            return Ok(saved);
        }

        let guard = authorize::<R>(self.ctx, Action::Edit, self.unchecked).await?;
        let id = primary_key_value::<R>(&am, pk)?;
        let condition = base_condition::<E>(self.ctx, &guard, Action::Edit).await?;
        let found = self
            .before_find_condition(self.ctx, &guard.repo, Action::Edit)
            .await?;
        let mut select = E::find_by_id(id).filter(condition).filter(found);
        if expected_modified.is_some() {
            select = select.lock_exclusive();
        }
        let before = select.one(db).await?.ok_or(RepoError::NotFound)?;
        if let (Some(expected), Some(column)) = (expected_modified, modified_column) {
            let current = <DateTime<Utc> as ValueType>::try_from(before.get(column))
                .map_err(|e| DbErr::Custom(e.to_string()))?;
            if current != expected {
                return Err(RepoError::Conflict);
            }
        }
        let access = guard.columns(self.ctx).await?;
        let changed = check_writable::<E>(&access, &am, pk, adding)?;
        if !guard.can_edit(self.ctx, &before, &am).await? {
            return Err(RepoError::Forbidden);
        }
        self.run_rules(&guard.repo, &spec, WriteMode::Edit, Some(&before), &mut am)
            .await?;
        if !changed {
            return Ok(before);
        }
        self.check_warnings(&guard.repo, &spec, WriteMode::Edit, Some(&before), &am)
            .await?;
        guard
            .repo
            .before_save(self.ctx, &mut am, WriteMode::Edit, Some(&before))
            .await?;
        if let Some(column) = modified_column {
            am.try_set(column, Value::from(clock::now()))?;
        }
        let saved = am.update(db).await?;
        guard
            .repo
            .after_save(self.ctx, &saved, WriteMode::Edit, Some(&before))
            .await?;
        let saved_id = crate::association::row_key::<R>(&saved)
            .ok_or_else(|| DbErr::Custom("the saved row has no id".to_string()))?;
        self.notify(
            self.ctx,
            ChangeKind::Edit,
            saved_id,
            Some(&before),
            Some(&saved),
        )
        .await?;
        self.ctx
            .after_save_commit(guard.repo.clone(), &saved, WriteMode::Edit)
            .await;
        Ok(saved)
    }

    /// Converts rows to JSON as the current Identity may see them.
    ///
    /// Always omits `hidden` columns and applies `can_read` and `read_scope`. Contained
    /// associations are nested, with `_label` on target rows (not the top row). Wrap a bare Model
    /// with `Loaded::from`. Output columns follow the Identity even when `unchecked`. Returns one
    /// value per row, in order.
    pub async fn to_json(
        &self,
        rows: &[Loaded<E::Model>],
    ) -> Result<Vec<serde_json::Value>, RepoError> {
        crate::json::loaded_to_json::<R>(self.ctx, rows).await
    }

    /// Converts one row like `to_json`.
    pub async fn to_json_one(
        &self,
        row: &Loaded<E::Model>,
    ) -> Result<serde_json::Value, RepoError> {
        let mut rows =
            crate::json::loaded_to_json::<R>(self.ctx, std::slice::from_ref(row)).await?;
        rows.pop()
            .ok_or_else(|| DbErr::Custom("to_json_one: no result".to_string()).into())
    }

    /// Sets `modified` to now and returns the updated row, to advance the parent's optimistic lock
    /// after only children changed.
    ///
    /// Checks `can(Edit)` and reads the row under `scope(Edit)` (`NotFound` if missing); skips
    /// `can_write` and `validate`. Writes nothing when there is no `modified` column.
    pub async fn touch(&self, id: Key<R>) -> Result<E::Model, RepoError> {
        let guard = authorize::<R>(self.ctx, Action::Edit, self.unchecked).await?;
        let spec = self.ctx.registry().spec_of::<E>();
        let condition = base_condition::<E>(self.ctx, &guard, Action::Edit).await?;
        let found = self
            .before_find_condition(self.ctx, &guard.repo, Action::Edit)
            .await?;
        let before = E::find_by_id(id)
            .filter(condition)
            .filter(found)
            .one(&self.ctx.conn())
            .await?
            .ok_or(RepoError::NotFound)?;
        let Some(modified) = spec.modified_column.and_then(column_named::<E>) else {
            return Ok(before);
        };
        let mut am = before.into_active_model();
        am.try_set(modified, Value::from(clock::now()))?;
        Ok(am.update(&self.ctx.conn()).await?)
    }

    /// Deletes a row, depending on its lifecycle.
    ///
    /// - `Activatable`: never deletes; returns `NotDeletable` before any check or DB access. Set
    ///   `is_active` to false with `save` instead.
    /// - `SoftDelete`: `can(Delete)`, reads under `scope(Delete)` (`NotFound` if missing),
    ///   `can_delete` (`Forbidden`), then sets `is_active = FALSE` and `modified`.
    /// - `HardDelete`: the same checks, then deletes the row.
    ///
    /// For associations with `dependent`, children (for many-to-many, the join rows) are deleted
    /// one by one through their own Repo's `delete` after the parent's `before_delete` and before
    /// the parent is removed, with the same `unchecked` mode. A child's error aborts everything.
    /// Such a `delete` opens its own transaction (a savepoint inside one), and commit hooks run
    /// after commit.
    pub async fn delete(&self, id: Key<R>) -> Result<(), RepoError> {
        self.remove(id, false).await
    }

    /// Physically deletes the row whatever the table lifecycle, including soft-deleted rows and
    /// `Activatable` rows that `delete` refuses.
    ///
    /// Checks run as in `delete` (`can(Delete)`, `scope(Delete)` and `before_find`, `can_delete`),
    /// but without the active-flag filter. Dependent children and join rows are purged too,
    /// including deactivated ones. Delete hooks and observers run with `ChangeKind::Delete`.
    pub async fn purge(&self, id: Key<R>) -> Result<(), RepoError> {
        self.remove(id, true).await
    }

    async fn remove(&self, id: Key<R>, purge: bool) -> Result<(), RepoError> {
        let has_dependent = self
            .ctx
            .registry()
            .relations_of::<E>()
            .unwrap_or_default()
            .iter()
            .any(|a| a.dependent());
        if !has_dependent {
            return self.delete_inner(self.ctx, id, purge).await;
        }
        let tx = self.ctx.begin().await?;
        let result = self.delete_inner(&tx, id, purge).await;
        tx.finish(result).await
    }

    async fn notify(
        &self,
        ctx: &Context,
        kind: ChangeKind,
        id: Key<R>,
        before: Option<&E::Model>,
        after: Option<&E::Model>,
    ) -> Result<(), RepoError> {
        let observers = ctx.registry().observers_of::<E>();
        if observers.is_empty() {
            return Ok(());
        }
        let meta = ctx
            .registry()
            .meta_of::<E>()
            .ok_or_else(|| RepoError::NotRegistered(std::any::type_name::<E>().to_string()))?;
        let before = before.map(|m| crate::marshal::to_json::<E>(&meta.fields, m, |_| true));
        let after = after.map(|m| crate::marshal::to_json::<E>(&meta.fields, m, |_| true));
        let change = Change {
            table: &meta.name,
            id: id.into_row_id(),
            kind,
            before: before.as_ref(),
            after: after.as_ref(),
        };
        for observer in &observers {
            observer.on_change(ctx, &change).await?;
        }
        Ok(())
    }

    async fn delete_inner(&self, ctx: &Context, id: Key<R>, purge: bool) -> Result<(), RepoError> {
        let spec = ctx.registry().spec_of::<E>();
        if !purge && spec.lifecycle == Lifecycle::Activatable {
            return Err(RepoError::NotDeletable);
        }
        let guard = authorize::<R>(ctx, Action::Delete, self.unchecked).await?;
        let condition = scoped_condition::<E>(ctx, &guard, Action::Delete, !purge).await?;
        let found = self
            .before_find_condition(ctx, &guard.repo, Action::Delete)
            .await?;
        let model = E::find_by_id(id.clone())
            .filter(condition)
            .filter(found)
            .one(&ctx.conn())
            .await?
            .ok_or(RepoError::NotFound)?;
        if !guard.can_delete(ctx, &model).await? {
            return Err(RepoError::Forbidden);
        }
        guard.repo.before_delete(ctx, &model).await?;
        self.delete_dependents(ctx, &id, &model, purge).await?;
        match (
            spec.lifecycle,
            spec.active_column.and_then(column_named::<E>),
        ) {
            (Lifecycle::SoftDelete, Some(is_active)) if !purge => {
                let mut am = model.clone().into_active_model();
                am.try_set(is_active, Value::from(false))?;
                if let Some(modified) = spec.modified_column.and_then(column_named::<E>) {
                    am.try_set(modified, Value::from(clock::now()))?;
                }
                am.update(&ctx.conn()).await?;
            }
            _ => {
                E::delete_by_id(id.clone()).exec(&ctx.conn()).await?;
            }
        }
        guard.repo.after_delete(ctx, &model).await?;
        self.notify(ctx, ChangeKind::Delete, id, Some(&model), None)
            .await?;
        ctx.after_delete_commit(guard.repo.clone(), &model).await;
        Ok(())
    }

    // A `bindingKey` association is looked up by the deleted row's value of that column; null does
    // nothing.
    async fn delete_dependents(
        &self,
        ctx: &Context,
        id: &Key<R>,
        model: &E::Model,
        purge: bool,
    ) -> Result<(), RepoError> {
        for relation in ctx.registry().relations_of::<E>().unwrap_or_default() {
            if !relation.dependent() {
                continue;
            }
            let parent = match relation.binding_key() {
                Some(binding) => {
                    let column = column_named::<E>(binding)
                        .ok_or_else(|| DbErr::Custom(format!("column {binding} does not exist")))?;
                    match row_id_of_value(model.get(column)) {
                        Some(value) => value,
                        None => continue,
                    }
                }
                None => id.clone().into_row_id(),
            };
            relation
                .delete_dependents(ctx, &parent, self.unchecked, purge)
                .await?;
        }
        Ok(())
    }
}

fn managed_columns(spec: &TableSpec) -> Vec<&'static str> {
    let mut columns: Vec<&'static str> = [spec.created_column, spec.modified_column]
        .into_iter()
        .flatten()
        .collect();
    if spec.lifecycle == Lifecycle::SoftDelete
        && let Some(active) = spec.active_column
    {
        columns.push(active);
    }
    columns
}

fn primary_key_value<R: GuardedRepo>(
    am: &<R::Entity as EntityTrait>::ActiveModel,
    pk: <R::Entity as EntityTrait>::Column,
) -> Result<Key<R>, RepoError> {
    let value = am
        .get(pk)
        .into_value()
        .ok_or_else(|| DbErr::Custom("the primary key is not set".to_string()))?;
    Ok(<Key<R> as ValueType>::try_from(value).map_err(|e| DbErr::Custom(e.to_string()))?)
}

// SeaORM writes `Set` and `Unchanged` on insert, but only `Set` on update.
fn is_written(value: &ActiveValue<Value>, adding: bool) -> bool {
    match value {
        ActiveValue::Set(_) => true,
        ActiveValue::Unchanged(_) => adding,
        ActiveValue::NotSet => false,
    }
}

fn check_writable<E: EntityTrait>(
    access: &ColumnAccess<E>,
    am: &E::ActiveModel,
    pk: E::Column,
    adding: bool,
) -> Result<bool, RepoError> {
    let mut any = false;
    for column in E::Column::iter() {
        if column.as_str() == pk.as_str() || !is_written(&am.get(column), adding) {
            continue;
        }
        any = true;
        if !access.can_write(column) {
            return Err(RepoError::Forbidden);
        }
    }
    Ok(any)
}
