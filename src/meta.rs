//! Column and table information: `column_named`, `TableSpec` (the result of the app's
//! `TableRules`), `FieldMeta` built from column definitions (`fields_of`) and the default order.
//! Naming and common-column conventions are the app's: cakedome knows no column or table prefixes.

use sea_orm::sea_query::{SimpleExpr, StringLen};
use sea_orm::{
    ColumnTrait, ColumnType, EntityTrait, IdenStatic, Iterable, Order, PrimaryKeyToColumn,
};
use serde_json::Value;

/// Returns the column of `E` named `name`, or `None`.
pub fn column_named<E: EntityTrait>(name: &str) -> Option<E::Column> {
    E::Column::iter().find(|c| c.as_str() == name)
}

/// What the active flag column means for a table, decided by the app's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// The flag means "usable"; inactive rows are still found, got and saved, and it is a normal
    /// writable column. Nothing is deleted.
    Activatable,
    /// The flag is a soft-delete flag managed by the Repo; reads and lists target active rows only.
    SoftDelete,
    /// No flag; delete is physical.
    HardDelete,
}

/// The result of the rules for one table, stored as `spec` in the entity meta.
/// `RepoConfig::table_spec` overrides it per entity.
#[derive(Debug, Clone, PartialEq)]
pub struct TableSpec {
    pub lifecycle: Lifecycle,
    /// The active flag column; must be `Some` unless `lifecycle` is `HardDelete` (else registration
    /// panics).
    pub active_column: Option<&'static str>,
    /// The creation time column, filled by the Repo.
    pub created_column: Option<&'static str>,
    /// The update time column, filled by the Repo and used for optimistic locking.
    pub modified_column: Option<&'static str>,
    /// The order column (first in the default order, ascending).
    pub order_column: Option<&'static str>,
    /// Defaults (column, JSON) for columns omitted on an add.
    pub defaults: Vec<(&'static str, Value)>,
    /// Groups of columns that must be unique, checked before saving (like DB UNIQUE indexes).
    pub uniques: Vec<Vec<&'static str>>,
    /// Groups of columns that raise a warning on a duplicate row, without blocking the save.
    pub warn_duplicates: Vec<Vec<&'static str>>,
}

impl TableSpec {
    /// Returns a spec with no rules: hard delete, no time or order columns, no defaults.
    pub fn plain() -> Self {
        TableSpec {
            lifecycle: Lifecycle::HardDelete,
            active_column: None,
            created_column: None,
            modified_column: None,
            order_column: None,
            defaults: Vec::new(),
            uniques: Vec::new(),
            warn_duplicates: Vec::new(),
        }
    }
}

/// The table rules, implemented by the app and passed to `Registry::new`.
pub trait TableRules: Send + Sync {
    /// Builds a `TableSpec` from the table name and column names (in definition order), or
    /// `Err(reason)` if the rules are not met (for example a required column is missing).
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String>;

    /// Returns the default column used for the `label` of choices and associations; `None` means
    /// the primary key. `Repo::initialize`'s `display_field` wins. Not part of `TableSpec`, so
    /// `table_spec` overrides do not change it. Registration panics if the column does not exist.
    fn display_field(&self, _table: &str, _columns: &[&'static str]) -> Option<&'static str> {
        None
    }
}

/// Calls `rules.spec` with the table and column names of `E`.
pub fn spec_for<E: EntityTrait>(rules: &dyn TableRules) -> Result<TableSpec, String> {
    let table = E::default().table_name().to_string();
    let columns: Vec<&'static str> = E::Column::iter().map(|c| c.as_str()).collect();
    rules.spec(&table, &columns)
}

/// The type of a field, decided by the DB column type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldKind {
    /// A string with a length limit (VARCHAR(n)).
    String {
        max_length: u32,
    },
    /// A long string (TEXT), at most 65,535 bytes.
    Text,
    /// A 32-bit integer.
    Integer,
    /// A 64-bit integer.
    BigInteger,
    /// UUID (BINARY(16)); JSON is a lowercase hyphenated string.
    Uuid,
    Boolean,
    /// A date (`YYYY-MM-DD`).
    Date,
    /// A UTC datetime with microseconds.
    DateTime,
    /// A double (f64); JSON is a number.
    Double,
    /// A JSON object or array.
    Json,
    /// DECIMAL(precision, scale); JSON is a string padded to `scale` digits.
    Decimal {
        precision: u32,
        scale: u32,
    },
}

impl FieldKind {
    /// Returns the name used as `type` in the schema.
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldKind::String { .. } => "string",
            FieldKind::Text => "text",
            FieldKind::Integer => "integer",
            FieldKind::BigInteger => "big_integer",
            FieldKind::Uuid => "uuid",
            FieldKind::Boolean => "boolean",
            FieldKind::Date => "date",
            FieldKind::DateTime => "datetime",
            FieldKind::Double => "double",
            FieldKind::Json => "json",
            FieldKind::Decimal { .. } => "decimal",
        }
    }
}

/// Information about one column (field), built by `fields_of`; registration sets `label` etc.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldMeta {
    /// The column name, also the JSON key.
    pub name: &'static str,
    /// The display name; defaults to the column name.
    pub label: String,
    pub kind: FieldKind,
    /// Whether NULL is allowed.
    pub nullable: bool,
    pub primary_key: bool,
    /// Set by the Repo: the created/modified columns and the soft-delete flag.
    pub managed: bool,
    /// The soft-delete flag: left out of output, schema and initial values, and `read_only` on
    /// input.
    pub internal: bool,
    /// Never shown or written; treated as nonexistent by the API.
    pub hidden: bool,
    /// The default for an add that omits the column (JSON).
    pub default: Option<Value>,
}

/// Builds the `FieldMeta` of every column of `E` in definition order.
///
/// The kind comes from the column type: `String(N)` or `Char(Some(n))` to `String`, `Text`,
/// `Integer`, `BigInteger`, `Uuid`, `Boolean`, `Date`, `TimestampWithTimeZone` to `DateTime`,
/// `Double`, `Json` or `JsonBinary` to `Json`, `Decimal(Some((p, s)))` to `Decimal`. `managed` and
/// `internal` come from `spec` (the soft-delete flag is both), `default` from `spec.defaults`.
///
/// Panics naming `table.column` for an unsupported type, a string without a length or a
/// `Decimal(None)`, so a misconfiguration surfaces at startup. Use `column_type =
/// "String(StringLen::N(n))"` or `"Decimal(Some((10, 2)))"` on the entity column.
pub fn fields_of<E: EntityTrait>(spec: &TableSpec) -> Vec<FieldMeta> {
    let table = E::default().table_name().to_string();
    let soft_active = if spec.lifecycle == Lifecycle::SoftDelete {
        spec.active_column
    } else {
        None
    };
    let primary: Vec<&'static str> = E::PrimaryKey::iter()
        .map(|k| k.into_column().as_str())
        .collect();
    E::Column::iter()
        .map(|column| {
            let name = column.as_str();
            let def = column.def();
            let kind = match def.get_column_type() {
                ColumnType::String(StringLen::N(n)) => FieldKind::String { max_length: *n },
                ColumnType::Char(Some(n)) => FieldKind::String { max_length: *n },
                ColumnType::Uuid => FieldKind::Uuid,
                ColumnType::Text => FieldKind::Text,
                ColumnType::Integer => FieldKind::Integer,
                ColumnType::BigInteger => FieldKind::BigInteger,
                ColumnType::Boolean => FieldKind::Boolean,
                ColumnType::Date => FieldKind::Date,
                ColumnType::TimestampWithTimeZone => FieldKind::DateTime,
                ColumnType::Double => FieldKind::Double,
                ColumnType::Json | ColumnType::JsonBinary => FieldKind::Json,
                ColumnType::Decimal(Some((p, s))) => FieldKind::Decimal {
                    precision: *p,
                    scale: *s,
                },
                ColumnType::Decimal(None) => panic!(
                    "{table}.{name}: decimal precision and scale are missing; add column_type = \"Decimal(Some((10, 2)))\" to the entity column"
                ),
                other => panic!("{table}.{name}: unsupported column type: {other:?}"),
            };
            FieldMeta {
                name,
                label: name.to_string(),
                kind,
                nullable: def.is_null(),
                primary_key: primary.contains(&name),
                managed: spec.created_column == Some(name)
                    || spec.modified_column == Some(name)
                    || soft_active == Some(name),
                internal: soft_active == Some(name),
                hidden: false,
                default: spec
                    .defaults
                    .iter()
                    .find(|(c, _)| *c == name)
                    .map(|(_, v)| v.clone()),
            }
        })
        .collect()
}

/// Returns the default order of `E`: `spec.order_column` (ascending) then the primary key, or the
/// primary key alone.
pub fn default_order<E: EntityTrait>(spec: &TableSpec) -> Vec<(E::Column, Order)> {
    let pk = E::PrimaryKey::iter()
        .next()
        .expect("the entity has a primary key")
        .into_column();
    match spec.order_column.and_then(column_named::<E>) {
        Some(order) => vec![(order, Order::Asc), (pk, Order::Asc)],
        None => vec![(pk, Order::Asc)],
    }
}

pub(crate) fn active_filter<E: EntityTrait>(spec: &TableSpec) -> Option<SimpleExpr> {
    if spec.lifecycle == Lifecycle::Activatable {
        spec.active_column
            .and_then(column_named::<E>)
            .map(|c| c.eq(true))
    } else {
        None
    }
}

/// A sample non-null value of the Rust type cakedome writes for each kind, and the name of the Rust
/// type the column must use. SeaORM's `Value` does not change kind on null.
fn sample_of(kind: FieldKind) -> (sea_orm::Value, &'static str) {
    use chrono::{DateTime, NaiveDate, Utc};
    match kind {
        FieldKind::String { .. } | FieldKind::Text => (String::new().into(), "String"),
        FieldKind::Integer => (0_i32.into(), "i32"),
        FieldKind::BigInteger => (0_i64.into(), "i64"),
        FieldKind::Uuid => (uuid::Uuid::nil().into(), "Uuid"),
        FieldKind::Boolean => (false.into(), "bool"),
        FieldKind::Date => (NaiveDate::default().into(), "NaiveDate"),
        FieldKind::DateTime => (DateTime::<Utc>::default().into(), "DateTime<Utc>"),
        FieldKind::Double => (0.0_f64.into(), "f64"),
        FieldKind::Json => (serde_json::Value::Null.into(), "Json"),
        FieldKind::Decimal { .. } => (sea_orm::prelude::Decimal::ZERO.into(), "Decimal"),
    }
}

/// Run at registration because SeaORM's `ActiveModel::set` panics on a type mismatch at runtime.
pub(crate) fn check_value_types<E: EntityTrait>(
    table: &str,
    fields: &[FieldMeta],
    spec: &TableSpec,
) {
    use sea_orm::ActiveModelTrait;

    let roles: [(Option<&'static str>, FieldKind, &str); 4] = [
        (spec.created_column, FieldKind::DateTime, "DateTime"),
        (spec.modified_column, FieldKind::DateTime, "DateTime"),
        (spec.active_column, FieldKind::Boolean, "Boolean"),
        (spec.order_column, FieldKind::Integer, "Integer"),
    ];
    for (column, expected, label) in roles {
        let Some(column) = column else { continue };
        if let Some(field) = fields.iter().find(|f| f.name == column)
            && std::mem::discriminant(&field.kind) != std::mem::discriminant(&expected)
        {
            panic!(
                "{table}.{column}: the rule column type must be {label} (found {})",
                field.kind.as_str()
            );
        }
    }

    for field in fields {
        let Some(column) = column_named::<E>(field.name) else {
            continue;
        };
        let (sample, rust_type) = sample_of(field.kind);
        let mut am = <E::ActiveModel as ActiveModelTrait>::default();
        if let Err(e) = am.try_set(column, sample) {
            let option = if field.nullable {
                format!("(nullable, so Option<{rust_type}>)")
            } else {
                String::new()
            };
            panic!(
                "{table}.{}: the Rust type does not match the value type cakedome writes (use {rust_type} for a {} column) {option}: {e}",
                field.name,
                field.kind.as_str()
            );
        }
    }
}
