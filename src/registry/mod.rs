//! Registration of entities and the Repo (config, Policy, validation) that serves each.
//! `EntityMeta` holds the static information, read-only after startup; `RepoConfig` writes it
//! during `Repo::initialize` with typed columns and associations, so mistakes fail to compile.

mod config;
mod effect;
mod entity_meta;

pub use crate::association::SaveStrategy;
use crate::messages::Messages;
pub use config::{FieldConfig, RepoConfig};
pub use effect::{Effect, EffectMeta, OnChange};
pub use entity_meta::EntityMeta;

use crate::repo::GuardedRepo;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::{EntityName, EntityTrait, PrimaryKeyTrait};

use crate::association::Association;
use crate::defaults::DynamicDefault;
use crate::key::{Key, KeyKind, RowKey};
use crate::meta::{FieldKind, FieldMeta, TableRules, TableSpec, spec_for};
use crate::observer::ChangeObserver;
use crate::repo::{Repo, TypedDecl, typed_decl_of};

/// The Repo type used by a typed declaration, compared by `verify` with the registered Repo type.
#[derive(Clone, Copy)]
pub(crate) struct RepoType {
    entity: TypeId,
    repo: TypeId,
    name: &'static str,
}

impl RepoType {
    pub(crate) fn of<R: GuardedRepo>() -> Self {
        Self {
            entity: TypeId::of::<R::Entity>(),
            repo: TypeId::of::<R>(),
            name: std::any::type_name::<R>(),
        }
    }
}

/// The registration of one entity; the Repo is held type-erased.
struct Entry {
    meta: EntityMeta,
    /// An `Arc<R>` boxed as `dyn Any`.
    repo: Box<dyn Any + Send + Sync>,
    dynamic_defaults: Vec<(&'static str, Arc<dyn DynamicDefault>)>,
    /// Groups added by `RepoConfig::unique`, kept when `table_spec` overrides the spec.
    own_uniques: Vec<Vec<&'static str>>,
    /// Groups added by `RepoConfig::warn_duplicate`, kept when `table_spec` overrides the spec.
    own_warn_duplicates: Vec<Vec<&'static str>>,
    /// The registered Repo type, compared by `verify` with the types of typed declarations.
    repo_type: RepoType,
    skipped_observers: Vec<&'static str>,
}

/// The registry: entity type to registration. Built at startup, then read-only (shared by `Arc`
/// through `Context`).
pub struct Registry {
    rules: Arc<dyn TableRules>,
    entries: HashMap<TypeId, Entry>,
    order: Vec<TypeId>,
    base_labels: HashMap<String, String>,
    base_entity_labels: HashMap<String, String>,
    observers: Vec<Arc<dyn ChangeObserver>>,
    messages: Messages,
}

impl Registry {
    /// Creates a registry from the table rules and two label dictionaries; pass `&[]` for none.
    ///
    /// `entity_labels` maps Repo names to entity labels (precedence: `Repo::initialize`'s `label`,
    /// this dictionary, the Repo name). `labels` maps column names to field labels (precedence:
    /// explicit in `Repo::initialize`, this dictionary, the column name), also for foreign key
    /// columns.
    ///
    /// ```text
    /// let mut registry = Registry::new(AppTableRules, ENTITY_LABELS, BASE_LABELS);
    /// registry.add(customers::CustomersRepo);
    /// registry.verify();
    /// ```
    pub fn new(
        rules: impl TableRules + 'static,
        entity_labels: &[(&str, &str)],
        labels: &[(&str, &str)],
    ) -> Self {
        let to_map = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<String, String>>()
        };
        Registry {
            rules: Arc::new(rules),
            entries: HashMap::new(),
            order: Vec::new(),
            base_labels: to_map(labels),
            base_entity_labels: to_map(entity_labels),
            observers: Vec::new(),
            messages: Messages::default(),
        }
    }

    /// Sets the message table for errors and warnings; keys not in it keep the default. A second
    /// call replaces it.
    pub fn set_messages(&mut self, entries: &[(&'static str, &'static str)]) {
        self.messages = Messages::new(entries);
    }

    /// Returns the message table.
    pub fn messages(&self) -> &Messages {
        &self.messages
    }

    /// Returns the `TableSpec` of `E`: the registered one (with `table_spec` overrides), else built
    /// from the rules. Panics if the rules return `Err` (`table: reason`).
    pub fn spec_of<E: EntityTrait>(&self) -> TableSpec {
        if let Some(e) = self.entries.get(&TypeId::of::<E>()) {
            return e.meta.spec.clone();
        }
        spec_for::<E>(&*self.rules)
            .unwrap_or_else(|reason| panic!("{}: {reason}", E::default().table_name()))
    }

    /// Registers a Repo and returns its `RepoConfig` after applying `R::initialize` once.
    ///
    /// The returned config can be chained further (for example in tests that need a different
    /// setup). Panics if the entity or the Repo name is registered twice, the rules return `Err`,
    /// the spec is invalid, a column type is unsupported or `auto_increment` is set on a
    /// non-integer key.
    ///
    /// ```text
    /// registry.add(OrdersRepo);
    /// registry.add(CustomersRepo { credit_cap: config.credit_cap });
    /// registry.add(DefaultRepo::<master_colors::Entity>::new());
    /// ```
    pub fn add<R: GuardedRepo>(&mut self, repo: R) -> RepoConfig<'_, R::Entity> {
        let name = <R as Repo>::repo_name().to_string();
        if self.entries.values().any(|e| e.meta.name == name)
            && !self.entries.contains_key(&TypeId::of::<R::Entity>())
        {
            panic!("a Repo with the same name is registered twice: {name}");
        }
        let repo = Arc::new(repo);
        let key = <Key<R> as RowKey>::KIND;
        if key != KeyKind::BigInteger
            && <<R::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::auto_increment()
        {
            panic!(
                "{}: the primary key is {} but auto_increment is set (auto_increment is only for big_integer)",
                <R::Entity as Default>::default().table_name(),
                key.as_str()
            );
        }
        let mut meta = EntityMeta::new::<R::Entity>(
            &name,
            key,
            &*self.rules,
            &self.base_entity_labels,
            &self.base_labels,
        );
        // A string key is sent by the client, so a duplicate id becomes a 422 `unique` before the
        // DB's primary key conflict (409). The key column is always `id`; kept in `own_uniques`
        // across `table_spec`.
        let mut own_uniques = Vec::new();
        if key == KeyKind::String {
            meta.spec.uniques.push(vec!["id"]);
            own_uniques.push(vec!["id"]);
        }
        let entry = Entry {
            meta,
            repo: Box::new(repo.clone()),
            dynamic_defaults: Vec::new(),
            own_uniques,
            own_warn_duplicates: Vec::new(),
            repo_type: RepoType::of::<R>(),
            skipped_observers: Vec::new(),
        };
        if self
            .entries
            .insert(TypeId::of::<R::Entity>(), entry)
            .is_some()
        {
            panic!("an Entity is registered twice: {name}");
        }
        self.order.push(TypeId::of::<R::Entity>());
        R::initialize(RepoConfig {
            registry: self,
            _entity: PhantomData,
        })
    }

    /// Registers an observer for saves and deletes, called in registration order.
    ///
    /// Panics if the same `name()` is registered twice. May be called before or after `add` and
    /// `verify`.
    pub fn observe(&mut self, observer: impl ChangeObserver) {
        let name = observer.name();
        if self.observers.iter().any(|o| o.name() == name) {
            panic!("an observer is registered twice: {name}");
        }
        self.observers.push(Arc::new(observer));
    }

    pub(crate) fn observers_of<E: EntityTrait>(&self) -> Vec<Arc<dyn ChangeObserver>> {
        let Some(entry) = self.entries.get(&TypeId::of::<E>()) else {
            return Vec::new();
        };
        self.observers
            .iter()
            .filter(|o| !entry.skipped_observers.contains(&o.name()))
            .cloned()
            .collect()
    }

    /// Returns the static information of `E`, or `None` if unregistered.
    pub fn meta_of<E: EntityTrait>(&self) -> Option<&EntityMeta> {
        self.entries.get(&TypeId::of::<E>()).map(|e| &e.meta)
    }

    /// Returns the static information of the entity with this table name, or `None`.
    pub fn meta(&self, name: &str) -> Option<&EntityMeta> {
        self.entries
            .values()
            .map(|e| &e.meta)
            .find(|m| m.name == name)
    }

    /// Returns the static information of every entity, in registration order.
    pub fn metas_in_order(&self) -> impl Iterator<Item = &EntityMeta> {
        self.order
            .iter()
            .filter_map(|id| self.entries.get(id).map(|e| &e.meta))
    }

    /// Returns the display name of `E`, or `None` if unregistered.
    pub fn label_of<E: EntityTrait>(&self) -> Option<&str> {
        self.meta_of::<E>().map(|m| m.label.as_str())
    }

    /// Returns the fields of `E`, or `None` if unregistered.
    pub fn fields_of<E: EntityTrait>(&self) -> Option<&[FieldMeta]> {
        self.meta_of::<E>().map(|m| m.fields.as_slice())
    }

    /// Returns the associations of `E` in registration order, or `None` if unregistered.
    pub fn relations_of<E: EntityTrait>(&self) -> Option<&[Arc<dyn Association>]> {
        self.meta_of::<E>().map(|m| m.relations.as_slice())
    }

    /// Returns the `default_contain` of `E`, or `None` if unregistered.
    pub fn default_contain_of<E: EntityTrait>(&self) -> Option<&[&'static str]> {
        self.meta_of::<E>().map(|m| m.default_contain.as_slice())
    }

    /// Returns the display field column of `E`, or `None` if unregistered.
    pub fn display_field_of<E: EntityTrait>(&self) -> Option<&'static str> {
        self.meta_of::<E>().map(|m| m.display_field)
    }

    /// Checks the whole registration, to be called once after every entity is added; panics on a
    /// mistake.
    ///
    /// It checks that association targets are registered (with the table name in the message), that
    /// hasMany children have the foreign key column, the second and later levels of
    /// `default_contain`, dynamic defaults (`DynamicDefault::verify`), `skip_observer` names, the
    /// `on_change` declarations, and that the Repo types of typed declarations (including those added
    /// with `RepoConfig::add_association`) match the registered Repo types.
    pub fn verify(&self) {
        for full in self.entries.values() {
            let entry = &full.meta;
            for name in &full.skipped_observers {
                if !self.observers.iter().any(|o| o.name() == *name) {
                    panic!("{}: skip_observer {name} is not registered", entry.name);
                }
            }
            for (column, default) in &full.dynamic_defaults {
                default.verify(self, &entry.name, column);
            }
            for relation in &entry.relations {
                relation.verify(entry, self);
            }
            self.verify_on_change(entry);
            for path in &entry.default_contain {
                let mut current = entry;
                for (index, segment) in path.split('.').enumerate() {
                    let Some(relation) = current.relations.iter().find(|r| r.name() == segment)
                    else {
                        panic!(
                            "{}: default_contain {path}: {segment} is not registered on {}",
                            entry.name, current.name
                        );
                    };
                    if index + 1 == path.split('.').count() {
                        break;
                    }
                    let Some(next) = self.meta(relation.target()) else {
                        break;
                    };
                    current = next;
                }
            }
        }
    }

    /// Panics if the Repo type `R` declared by an association differs from the one registered for
    /// its Entity. An unregistered Entity is not checked.
    pub(crate) fn check_declared_repo<R: GuardedRepo>(
        &self,
        table: &str,
        association: &str,
        role: &str,
    ) {
        let declared = RepoType::of::<R>();
        if let Some(registered) = self.entries.get(&declared.entity)
            && registered.repo_type.repo != declared.repo
        {
            panic!(
                "{table}: association {association}: {role} type {} differs from the registered Repo type {}",
                declared.name, registered.repo_type.name
            );
        }
    }

    fn verify_on_change(&self, entry: &EntityMeta) {
        let kind_of = |meta: &EntityMeta, field: &str| {
            meta.fields.iter().find(|f| f.name == field).map(|f| f.kind)
        };
        // string and text are treated as the same kind.
        let same_kind = |a: FieldKind, b: FieldKind| {
            let norm = |k: FieldKind| match k.as_str() {
                "text" => "string",
                s => s,
            };
            norm(a) == norm(b)
        };
        for on_change in &entry.on_change {
            for effect in &on_change.effects {
                let EffectMeta::Copy {
                    relation,
                    target,
                    map,
                    ..
                } = effect
                else {
                    continue;
                };
                let table = &entry.name;
                let column = on_change.column;
                let Some(association) = entry
                    .relations
                    .iter()
                    .find(|r| r.name() == *relation && r.owner_foreign_key().is_some())
                else {
                    panic!(
                        "{table}.{column}: association {relation} in on_change copy is not registered as belongsTo"
                    );
                };
                if association.binding_key().is_some() {
                    panic!(
                        "{table}.{column}: association {relation} in on_change copy cannot be used because it has a bindingKey (the screen reads the target by primary key)"
                    );
                }
                let Some(other) = self.meta(association.target()) else {
                    panic!(
                        "{table}.{column}: target {} of association {relation} in on_change copy is not registered",
                        association.target()
                    );
                };
                for (own, src) in map {
                    let own_kind = kind_of(entry, own).expect("the column is in fields");
                    let Some(src_kind) = kind_of(other, src) else {
                        panic!(
                            "{table}.{column}: on_change copy source {target}.{src} does not exist"
                        );
                    };
                    if !same_kind(own_kind, src_kind) {
                        panic!(
                            "{table}.{own}: on_change copy type {} does not match the type {} of {target}.{src}",
                            own_kind.as_str(),
                            src_kind.as_str()
                        );
                    }
                }
            }
        }
        let edges = |column: &str| -> Vec<&'static str> {
            let mut next = Vec::new();
            for on_change in entry.on_change.iter().filter(|o| o.column == column) {
                for effect in &on_change.effects {
                    match effect {
                        EffectMeta::Clear { field } => next.push(*field),
                        EffectMeta::Copy { map, .. } => next.extend(map.iter().map(|(o, _)| *o)),
                    }
                }
            }
            next
        };
        fn walk<'a>(
            column: &'a str,
            edges: &dyn Fn(&str) -> Vec<&'static str>,
            path: &mut Vec<&'a str>,
            done: &mut Vec<&'a str>,
        ) -> Option<Vec<&'a str>> {
            if let Some(at) = path.iter().position(|c| *c == column) {
                let mut cycle = path[at..].to_vec();
                cycle.push(column);
                return Some(cycle);
            }
            if done.contains(&column) {
                return None;
            }
            path.push(column);
            for next in edges(column) {
                if let Some(cycle) = walk(next, edges, path, done) {
                    return Some(cycle);
                }
            }
            path.pop();
            done.push(column);
            None
        }
        let mut done = Vec::new();
        for on_change in &entry.on_change {
            if let Some(cycle) = walk(on_change.column, &edges, &mut Vec::new(), &mut done) {
                panic!(
                    "{}: on_change forms a cycle ({})",
                    entry.name,
                    cycle.join(" → ")
                );
            }
        }
    }

    /// Sets an extension of entity `E` after registration, replacing one of the same type. Panics
    /// if `E` is unregistered.
    ///
    /// Lets the app attach data that `extension` reads where only the name is known.
    pub fn set_extension<E: EntityTrait, T: Any + Send + Sync>(&mut self, value: T) {
        let entry = self.entries.get_mut(&TypeId::of::<E>()).unwrap_or_else(|| {
            panic!(
                "{} is not registered",
                <E as Default>::default().table_name()
            )
        });
        entry.meta.set_extension(value);
    }

    /// Returns the extension of type `T` of the entity with this table name, or `None`.
    pub fn extension<T: Any + Send + Sync>(&self, name: &str) -> Option<&T> {
        self.meta(name)?.extension::<T>()
    }

    pub(crate) fn meta_of_entity(&self, entity: TypeId) -> Option<&EntityMeta> {
        self.entries.get(&entity).map(|e| &e.meta)
    }

    /// Returns whether `E` is registered.
    pub fn contains<E: EntityTrait>(&self) -> bool {
        self.entries.contains_key(&TypeId::of::<E>())
    }

    /// Returns the registered name (Repo name) of `E`, or `None`.
    pub fn name_of<E: EntityTrait>(&self) -> Option<&str> {
        self.meta_of::<E>().map(|m| m.name.as_str())
    }

    /// Returns the registered Repo `R` as the type it was added with (so its startup settings can
    /// be read), or `None` if `R::Entity` is unregistered or registered with another Repo type.
    ///
    /// `RepoHandle` turns `None` into `NotRegistered`.
    pub fn repo<R: GuardedRepo>(&self) -> Option<Arc<R>> {
        let e = self.entries.get(&TypeId::of::<R::Entity>())?;
        e.repo.downcast_ref::<Arc<R>>().cloned()
    }

    pub(crate) fn typed_decl<R: GuardedRepo>(&self, name: &str) -> Option<Arc<dyn TypedDecl<R>>> {
        let relation = self
            .meta_of::<R::Entity>()?
            .relations
            .iter()
            .find(|a| a.name() == name)?;
        typed_decl_of::<R>(relation.as_ref())
    }

    pub(crate) fn dynamic_defaults<E: EntityTrait>(
        &self,
    ) -> Vec<(&'static str, Arc<dyn DynamicDefault>)> {
        self.entries
            .get(&TypeId::of::<E>())
            .map(|e| e.dynamic_defaults.clone())
            .unwrap_or_default()
    }
}
