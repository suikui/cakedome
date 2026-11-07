//! Primary key types: `RowKey` for typed layers, `RowId` for type-erased layers, `Key<R>` for a
//! Repo.

use std::fmt::{self, Debug};
use std::hash::Hash;

use sea_orm::sea_query::{Nullable, ValueType};
use sea_orm::{EntityTrait, PrimaryKeyTrait};

use crate::repo::Repo;

/// The kind of a primary key type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyKind {
    BigInteger,
    Uuid,
    String,
}

/// A type usable as a primary key; implemented by `i64`, `Uuid` and `String`.
pub trait RowKey:
    Clone
    + Eq
    + Hash
    + Ord
    + Debug
    + Send
    + Sync
    + 'static
    + Into<sea_orm::Value>
    + ValueType
    + Nullable
{
    const KIND: KeyKind;

    /// Converts to JSON (`i64` as a number).
    fn to_json(&self) -> serde_json::Value;
    /// Reads from JSON; `None` for a string or fraction given for `i64`, or out of range.
    fn from_json(value: &serde_json::Value) -> Option<Self>;
    /// Reads from a URL string.
    fn parse(s: &str) -> Option<Self>;
    /// Generates the key of a new row; `None` (the default) leaves it to the DB.
    fn generate() -> Option<Self> {
        None
    }
    fn into_row_id(self) -> RowId;
    fn from_row_id(id: &RowId) -> Option<Self>;
}

impl RowKey for uuid::Uuid {
    const KIND: KeyKind = KeyKind::Uuid;

    fn to_json(&self) -> serde_json::Value {
        self.to_string().into()
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Self::parse(value.as_str()?)
    }

    /// Reads only 36 characters (with hyphens) or 32 (without), in any case.
    fn parse(s: &str) -> Option<Self> {
        if s.len() != 36 && s.len() != 32 {
            return None;
        }
        uuid::Uuid::try_parse(s).ok()
    }

    fn generate() -> Option<Self> {
        Some(uuid::Uuid::now_v7())
    }

    fn into_row_id(self) -> RowId {
        RowId::Uuid(self)
    }

    fn from_row_id(id: &RowId) -> Option<Self> {
        match id {
            RowId::Uuid(v) => Some(*v),
            _ => None,
        }
    }
}

impl RowKey for String {
    const KIND: KeyKind = KeyKind::String;

    fn to_json(&self) -> serde_json::Value {
        self.clone().into()
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        value.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    }

    fn parse(s: &str) -> Option<Self> {
        (!s.is_empty()).then(|| s.to_string())
    }

    fn into_row_id(self) -> RowId {
        RowId::Str(self)
    }

    fn from_row_id(id: &RowId) -> Option<Self> {
        match id {
            RowId::Str(v) => Some(v.clone()),
            _ => None,
        }
    }
}

impl RowKey for i64 {
    const KIND: KeyKind = KeyKind::BigInteger;

    fn to_json(&self) -> serde_json::Value {
        (*self).into()
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        value.as_i64()
    }

    fn parse(s: &str) -> Option<Self> {
        s.parse::<i64>().ok()
    }

    fn into_row_id(self) -> RowId {
        RowId::Int(self)
    }

    fn from_row_id(id: &RowId) -> Option<Self> {
        match id {
            RowId::Int(v) => Some(*v),
            _ => None,
        }
    }
}

impl KeyKind {
    /// The name used as `key` in the schema.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyKind::BigInteger => "big_integer",
            KeyKind::Uuid => "uuid",
            KeyKind::String => "string",
        }
    }
}

/// A primary key in type-erased layers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowId {
    Int(i64),
    Uuid(uuid::Uuid),
    Str(String),
}

impl RowId {
    /// Converts to JSON (integers as numbers, others as strings).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            RowId::Int(v) => (*v).into(),
            RowId::Uuid(v) => v.to_string().into(),
            RowId::Str(v) => v.clone().into(),
        }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowId::Int(v) => write!(f, "{v}"),
            RowId::Uuid(v) => write!(f, "{v}"),
            RowId::Str(v) => f.write_str(v),
        }
    }
}

impl From<&RowId> for sea_orm::Value {
    /// Converts to a SQL value. A Uuid becomes a UUID value so it compares with `BINARY(16)`
    /// columns; as a string it would silently never match.
    fn from(id: &RowId) -> Self {
        match id {
            RowId::Int(v) => (*v).into(),
            RowId::Uuid(v) => (*v).into(),
            RowId::Str(v) => v.clone().into(),
        }
    }
}

impl From<i64> for RowId {
    fn from(v: i64) -> Self {
        RowId::Int(v)
    }
}

impl From<String> for RowId {
    fn from(v: String) -> Self {
        RowId::Str(v)
    }
}

impl From<uuid::Uuid> for RowId {
    fn from(v: uuid::Uuid) -> Self {
        RowId::Uuid(v)
    }
}

/// The primary key type of Repo `R`.
pub type Key<R> = <<<R as Repo>::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;
