//! `EntityMeta`, the static information of an entity, and how it is built and changed.
//!
//! Only `EntityMeta::new` builds it and only the `set_*` methods change it, which keeps the marks
//! of explicit defaults inside.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use sea_orm::{EntityTrait, IdenStatic, Iterable, PrimaryKeyToColumn};
use serde_json::Value;

use super::OnChange;
use crate::association::Association;
use crate::json::LABEL_KEY;
use crate::key::KeyKind;
use crate::meta::{
    FieldMeta, Lifecycle, TableRules, TableSpec, check_value_types, fields_of, spec_for,
};

#[derive(Clone, Default)]
struct Extensions(HashMap<TypeId, Arc<dyn Any + Send + Sync>>);

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Extensions({})", self.0.len())
    }
}

/// Static information of an entity, built at registration and independent of Identity.
///
/// `spec` holds the result of the rules; `RepoConfig` changes it during registration and it is
/// read-only afterwards. Applications attach per-entity data by type (`RepoConfig::extension`,
/// `Registry::set_extension`) and can read it by name (`Registry::extension::<T>("orders")`,
/// `EntityMeta::extension`).
#[derive(Debug, Clone)]
pub struct EntityMeta {
    /// The table name (such as `"customers"`), used to find an entity by name.
    pub name: String,
    /// The display name; defaults to the entity label dictionary given to `Registry::new`, else the
    /// table name.
    pub label: String,
    /// The kind of the primary key.
    pub key: KeyKind,
    /// The result of the rules (`TableRules`); `table_spec` overrides it.
    pub spec: TableSpec,
    /// The fields, built from column definitions and adjusted by `RepoConfig`.
    pub fields: Vec<FieldMeta>,
    /// The registered associations, in registration order.
    pub relations: Vec<Arc<dyn Association>>,
    /// The association names a get includes by default.
    pub default_contain: Vec<&'static str>,
    /// The column used for the `label` of choices and associations; defaults to the rules'
    /// `display_field`, else the primary key. `RepoConfig::display_field` overrides it.
    pub display_field: &'static str,
    /// Whether the entity is exposed to generic layers (such as a JSON API); false for tables
    /// registered only for associations. Set with `RepoConfig::api`.
    pub api: bool,
    /// Whether saves from request data check the optimistic lock (default true). Only tables with a
    /// modified column are checked.
    pub optimistic_lock: bool,
    /// The effects of a field change (`RepoConfig::on_change`), in declaration order.
    pub on_change: Vec<OnChange>,
    /// Columns whose default was set explicitly; `set_spec` does not overwrite them with rule
    /// defaults.
    explicit_defaults: HashSet<&'static str>,
    /// Writable during registration only.
    extensions: Extensions,
}

impl EntityMeta {
    /// Builds the meta from the rules and dictionaries.
    ///
    /// Panics if the rules return `Err` (`table: reason`), the spec is invalid or a column type is
    /// unsupported.
    pub(crate) fn new<E: EntityTrait>(
        name: &str,
        key: KeyKind,
        rules: &dyn TableRules,
        entity_labels: &HashMap<String, String>,
        labels: &HashMap<String, String>,
    ) -> Self {
        let name = name.to_string();
        let table = E::default().table_name();
        let spec = spec_for::<E>(rules).unwrap_or_else(|reason| panic!("{name}: {reason}"));
        check_spec(&name, &spec);
        let mut fields = fields_of::<E>(&spec);
        if fields.iter().any(|f| f.name == LABEL_KEY) {
            panic!("{name}: column name {LABEL_KEY} is reserved");
        }
        check_value_types::<E>(&name, &fields, &spec);
        for field in &mut fields {
            if let Some(label) = labels.get(field.name) {
                field.label = label.clone();
            }
        }
        let label = entity_labels
            .get(&name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        let columns: Vec<&'static str> = E::Column::iter().map(|c| c.as_str()).collect();
        let display_field = match rules.display_field(table, &columns) {
            Some(column) if columns.contains(&column) => column,
            Some(column) => panic!("{name}: display field column {column} does not exist"),
            None => primary_key_of::<E>(),
        };
        EntityMeta {
            name,
            label,
            key,
            spec,
            fields,
            relations: Vec::new(),
            default_contain: Vec::new(),
            display_field,
            api: true,
            optimistic_lock: true,
            on_change: Vec::new(),
            explicit_defaults: HashSet::new(),
            extensions: Extensions::default(),
        }
    }

    /// Returns the extension of type `T`, if set.
    pub fn extension<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.extensions
            .0
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    pub(crate) fn set_extension<T: Any + Send + Sync>(&mut self, value: T) {
        self.extensions.0.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Replaces `spec`, checking it and rebuilding `managed` / `internal` and rule defaults of the
    /// fields (explicit defaults are kept).
    pub(crate) fn set_spec<E: EntityTrait>(&mut self, spec: TableSpec) {
        check_spec(&self.name, &spec);
        check_value_types::<E>(&self.name, &self.fields, &spec);
        let rebuilt = fields_of::<E>(&spec);
        for field in &mut self.fields {
            let new = rebuilt
                .iter()
                .find(|r| r.name == field.name)
                .expect("a column of the same entity");
            field.managed = new.managed;
            field.internal = new.internal;
            if !self.explicit_defaults.contains(field.name) {
                field.default = new.default.clone();
            }
        }
        self.spec = spec;
    }

    pub(crate) fn set_label(&mut self, column: &'static str, label: &str) {
        self.field_mut(column).label = label.to_string();
    }

    pub(crate) fn set_default(&mut self, column: &'static str, value: Value) {
        self.field_mut(column).default = Some(value);
        self.explicit_defaults.insert(column);
    }

    pub(crate) fn set_display_field(&mut self, column: &'static str) {
        self.display_field = column;
    }

    pub(crate) fn field_mut(&mut self, column: &'static str) -> &mut FieldMeta {
        self.fields
            .iter_mut()
            .find(|m| m.name == column)
            .expect("the column is in fields_of")
    }

    /// Puts back a field edited by a `FieldConfig` closure, marking the default explicit if it was
    /// set.
    pub(crate) fn replace_field(
        &mut self,
        column: &'static str,
        field: FieldMeta,
        default_set: bool,
    ) {
        *self.field_mut(column) = field;
        if default_set {
            self.explicit_defaults.insert(column);
        }
    }
}

fn primary_key_of<E: EntityTrait>() -> &'static str {
    E::PrimaryKey::iter()
        .next()
        .expect("the entity has a primary key")
        .into_column()
        .as_str()
}

fn check_spec(table: &str, spec: &TableSpec) {
    if spec.lifecycle != Lifecycle::HardDelete && spec.active_column.is_none() {
        panic!(
            "{table}: lifecycle is {:?} but active_column is missing",
            spec.lifecycle
        );
    }
}
