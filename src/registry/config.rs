//! `RepoConfig`, the registration settings of an entity, and `FieldConfig` for one field.

use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::{EntityTrait, IdenStatic};
use serde_json::Value;

use super::{Effect, EffectMeta, EntityMeta, Entry, OnChange, Registry, RepoType, TypedCheck};
use crate::association::{Association, Declaration};
use crate::defaults::DynamicDefault;
use crate::json::LABEL_KEY;
use crate::meta::{FieldMeta, TableSpec, column_named};
use crate::repo::{GuardedRepo, MAX_CONTAIN_DEPTH};

/// Settings of a registered entity, returned by `Registry::add` and received by `Repo::initialize`.
/// Methods chain.
///
/// Takes columns and associations by their `E` types and changes `EntityMeta` through its `set_*`
/// methods.
pub struct RepoConfig<'a, E: EntityTrait> {
    pub(super) registry: &'a mut Registry,
    pub(super) _entity: PhantomData<fn() -> E>,
}

impl<E: EntityTrait> RepoConfig<'_, E> {
    fn meta(&mut self) -> &mut EntityMeta {
        &mut self.entry().meta
    }

    fn entry(&mut self) -> &mut Entry {
        self.registry
            .entries
            .get_mut(&TypeId::of::<E>())
            .expect("registered by add()")
    }

    /// Sets the display name of the entity.
    pub fn label(mut self, label: &str) -> Self {
        self.meta().label = label.to_string();
        self
    }

    /// Sets a per-entity extension value, replacing one of the same type.
    ///
    /// Read it with `Registry::extension::<T>(name)` or `EntityMeta::extension`.
    pub fn extension<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.meta().set_extension(value);
        self
    }

    pub(crate) fn repo_of<R: GuardedRepo<Entity = E>>(&self) -> Arc<R> {
        self.registry.repo::<R>().expect("registered by add()")
    }

    /// Sets whether generic layers (such as a JSON API) expose the entity (default true).
    ///
    /// A hidden entity still works through the ORM (`ctx.repo`, associations, Policy, hooks); use
    /// it for tables registered only for associations, such as join tables.
    pub fn api(mut self, exposed: bool) -> Self {
        self.meta().api = exposed;
        self
    }

    /// Configures a field: `f` receives a `FieldConfig` and returns it.
    pub fn field(mut self, column: E::Column, f: impl FnOnce(FieldConfig) -> FieldConfig) -> Self {
        let name = column.as_str();
        let meta = self.meta();
        let config = f(FieldConfig(meta.field_mut(name).clone(), false));
        meta.replace_field(name, config.0, config.1);
        self
    }

    /// Overrides the rules' `TableSpec` for this entity; `f` receives it and returns the new one.
    ///
    /// The spec is checked (panics if `lifecycle` is not `HardDelete` and `active_column` is
    /// `None`) and `managed` / `internal` and rule defaults are rebuilt; labels and defaults set
    /// explicitly are kept.
    pub fn table_spec(mut self, f: impl FnOnce(TableSpec) -> TableSpec) -> Self {
        let mut spec = f(self.meta().spec.clone());
        for set in self.entry().own_uniques.clone() {
            if !spec.uniques.contains(&set) {
                spec.uniques.push(set);
            }
        }
        for set in self.entry().own_warn_duplicates.clone() {
            if !spec.warn_duplicates.contains(&set) {
                spec.warn_duplicates.push(set);
            }
        }
        self.meta().set_spec::<E>(spec);
        self
    }

    /// Sets field labels, `[(column, "label"), ..]`; other fields keep the column name.
    pub fn labels<'l>(mut self, labels: impl IntoIterator<Item = (E::Column, &'l str)>) -> Self {
        for (column, label) in labels {
            self.meta().set_label(column.as_str(), label);
        }
        self
    }

    /// Adds a group of columns that must be unique (like a DB UNIQUE index), checked before saving.
    ///
    /// A duplicate becomes a `unique` validation error on the first column. The group survives
    /// `table_spec` overrides.
    pub fn unique(mut self, columns: impl IntoIterator<Item = E::Column>) -> Self {
        let set: Vec<&'static str> = columns.into_iter().map(|c| c.as_str()).collect();
        if set.is_empty() {
            return self;
        }
        if !self.meta().spec.uniques.contains(&set) {
            self.meta().spec.uniques.push(set.clone());
        }
        let own = &mut self.entry().own_uniques;
        if !own.contains(&set) {
            own.push(set);
        }
        self
    }

    /// Adds a group of columns that raises a warning on a duplicate row (the confirming counterpart
    /// of `unique`).
    ///
    /// Only layers that check warnings enforce it; an unaccepted warning is returned as an error.
    /// The group survives `table_spec` overrides.
    pub fn warn_duplicate(mut self, columns: impl IntoIterator<Item = E::Column>) -> Self {
        let set: Vec<&'static str> = columns.into_iter().map(|c| c.as_str()).collect();
        if set.is_empty() {
            return self;
        }
        if !self.meta().spec.warn_duplicates.contains(&set) {
            self.meta().spec.warn_duplicates.push(set.clone());
        }
        let own = &mut self.entry().own_warn_duplicates;
        if !own.contains(&set) {
            own.push(set);
        }
        self
    }

    /// Skips the observer `name` for this entity (such as one that records history). `verify`
    /// panics if the name is not registered.
    pub fn skip_observer(mut self, name: &'static str) -> Self {
        let skipped = &mut self.entry().skipped_observers;
        if !skipped.contains(&name) {
            skipped.push(name);
        }
        self
    }

    /// Sets the default (JSON) used for columns omitted on an add.
    pub fn default(mut self, column: E::Column, default: Value) -> Self {
        self.meta().set_default(column.as_str(), default);
        self
    }

    /// Sets a default computed per request for omitted columns on an add and for the initial values
    /// of a new row; it wins over the fixed `default`, the later call on a column wins, and updates
    /// do not use it.
    pub fn default_from(
        mut self,
        column: E::Column,
        default: impl DynamicDefault + 'static,
    ) -> Self {
        let name = column.as_str();
        let list = &mut self.entry().dynamic_defaults;
        list.retain(|(n, _)| *n != name);
        list.push((name, Arc::new(default)));
        self
    }

    /// Hides a column: it is treated as nonexistent, never read, written or shown in the schema.
    pub fn hidden(mut self, column: E::Column) -> Self {
        self.meta().field_mut(column.as_str()).hidden = true;
        self
    }

    /// Registers an association from a typed declaration (`BelongsToOf` / `HasManyOf` /
    /// `ManyToManyOf`).
    ///
    /// A declaration of another entity than `E` fails to compile. Panics, naming the association,
    /// if the target primary key is not a single `id`, the name is reserved (`_label`) or equals a
    /// column or registered association, or a many-to-many `save` is not `Replace`.
    /// `Registry::verify` checks that targets and join tables are registered and that foreign key
    /// columns exist.
    ///
    /// ```text
    /// .association(Self::CUSTOMER)
    /// ```
    pub fn association<D: Declaration<E>>(self, declaration: D) -> Self {
        declaration.register(self)
    }

    fn check_association_name(&mut self, name: &str) {
        let table = self.meta().name.clone();
        if name == LABEL_KEY {
            panic!("{table}: association name {LABEL_KEY} is reserved");
        }
        if column_named::<E>(name).is_some() {
            panic!("{table}: association name {name} is the same as a column name");
        }
    }

    pub(crate) fn push_typed_checks(
        &mut self,
        name: &'static str,
        checks: Vec<(&'static str, RepoType)>,
    ) {
        let entry = self.entry();
        entry.typed_checks.retain(|c| c.association != name);
        for (role, declared) in checks {
            entry.typed_checks.push(TypedCheck {
                association: name,
                role,
                declared,
            });
        }
    }

    /// Adds an association, for `Declaration::register` of an app's own association kinds.
    ///
    /// Panics if the name is reserved, equals a column name or duplicates a registered association.
    pub fn add_association(mut self, association: impl Association) -> Self {
        let name = association.name();
        self.check_association_name(name);
        let association: Arc<dyn Association> = Arc::new(association);
        let meta = self.meta();
        let table = meta.name.clone();
        if meta.relations.iter().any(|r| r.name() == name) {
            panic!("{table}: association name {name} is duplicated");
        }
        meta.relations.push(association);
        self
    }

    /// Sets the associations a get includes by default. A name may be a dot-separated path
    /// (`"items.product"`).
    ///
    /// Strings are taken instead of association constants so nested ones are written the same way.
    /// Panics on an empty segment, more than `MAX_CONTAIN_DEPTH` levels or an unregistered first
    /// level; later levels belong to the target entity and `Registry::verify` checks them.
    pub fn default_contain(mut self, names: impl IntoIterator<Item = &'static str>) -> Self {
        let names: Vec<&'static str> = names.into_iter().collect();
        let entry = self.meta();
        for path in &names {
            let segments: Vec<&str> = path.split('.').collect();
            if segments.iter().any(|s| s.is_empty()) {
                panic!("{}: default_contain {path} has an empty name", entry.name);
            }
            if segments.len() > MAX_CONTAIN_DEPTH {
                panic!(
                    "{}: default_contain {path} must be at most {MAX_CONTAIN_DEPTH} levels deep",
                    entry.name
                );
            }
            let first = segments[0];
            if !entry.relations.iter().any(|r| r.name() == first) {
                panic!("{}: association {first} is not registered", entry.name);
            }
        }
        entry.default_contain = names;
        self
    }

    /// Declares the effects of a change of `column` in the UI (shown as `onChange` in the schema).
    ///
    /// The UI runs them; they are not applied on save, so enforce rules in `validate` /
    /// `before_save`. `Effect::clear(column)` clears the column; `Effect::copy(relation, [(own,
    /// target)])` copies belongsTo target values. Panics if declared twice for a column or if the
    /// foreign key of a `copy` relation is not `column`. `Registry::verify` checks associations,
    /// column types and cycles.
    ///
    /// ```text
    /// .on_change(Column::CustomerId, [Effect::clear(Column::StatusId)])
    /// ```
    pub fn on_change(
        mut self,
        column: E::Column,
        effects: impl IntoIterator<Item = Effect<E>>,
    ) -> Self {
        let name = column.as_str();
        let meta = self.meta();
        let table = meta.name.clone();
        if meta.on_change.iter().any(|o| o.column == name) {
            panic!("{table}.{name}: on_change is declared twice");
        }
        let effects: Vec<EffectMeta> = effects.into_iter().map(|e| e.meta).collect();
        for effect in &effects {
            if let EffectMeta::Copy {
                relation,
                foreign_key,
                ..
            } = effect
                && *foreign_key != name
            {
                panic!(
                    "{table}.{name}: the foreign key of association {relation} in on_change copy is {foreign_key}"
                );
            }
        }
        meta.on_change.push(OnChange {
            column: name,
            effects,
        });
        self
    }

    /// Sets the field used for the `label` of choices and associations (default: the primary key).
    pub fn display_field(mut self, column: E::Column) -> Self {
        self.meta().set_display_field(column.as_str());
        self
    }
}

/// Settings of one field, received by the closure of `RepoConfig::field`.
///
/// The second value records whether a default was set explicitly.
pub struct FieldConfig(FieldMeta, bool);

impl FieldConfig {
    /// Sets the field label.
    pub fn label(mut self, label: &str) -> Self {
        self.0.label = label.to_string();
        self
    }

    /// Sets the default (JSON) used when the field is omitted on an add.
    pub fn default(mut self, default: Value) -> Self {
        self.0.default = Some(default);
        self.1 = true;
        self
    }
}
