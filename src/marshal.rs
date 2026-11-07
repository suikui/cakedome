//! Conversion between JSON and Model / ActiveModel, and the first validation stage (type, required,
//! length, writable columns).
//!
//! Uses no DB. JSON keys are column names; datetimes are UTC RFC 3339 with 6 fractional digits
//! (`2026-10-17T01:02:03.123456Z`) and dates are `YYYY-MM-DD`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, SubsecRound, Utc};
use sea_orm::prelude::Decimal;
use sea_orm::sea_query::ValueType;
use sea_orm::{ActiveModelTrait, DbErr, EntityTrait, ModelTrait, Value};
use serde::Serialize;
use serde_json::{Map, Value as Json};
use uuid::Uuid;

use crate::key::{RowId, RowKey};
use crate::messages::{Messages, params};
use crate::meta::{FieldKind, FieldMeta, column_named};

/// Whether a write is an add or an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Add,
    Edit,
}

/// Context passed to before_marshal / after_marshal.
#[derive(Clone, Debug)]
pub struct MarshalOptions {
    mode: WriteMode,
    id: Option<RowId>,
    association: Option<&'static str>,
}

impl MarshalOptions {
    pub(crate) fn new(
        mode: WriteMode,
        id: Option<RowId>,
        association: Option<&'static str>,
    ) -> Self {
        Self {
            mode,
            id,
            association,
        }
    }

    /// Returns whether this is an add or an edit.
    pub fn mode(&self) -> WriteMode {
        self.mode
    }

    /// Returns the id of the row being edited (the URL id for a parent, the body row's id for a
    /// child).
    pub fn id(&self) -> Option<&RowId> {
        self.id.as_ref()
    }

    /// Returns the association name from the parent (`"items"`) for a child row; `None` for a
    /// parent.
    pub fn association(&self) -> Option<&'static str> {
        self.association
    }
}

/// An error on one field: a machine-readable `code`, a `message` for display and the `params` used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub code: &'static str,
    /// Values used to build the message; omitted from JSON when empty.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Json>,
    pub message: String,
}

/// Errors per field name, serialized as `{"name": [{"code": .., "params": {..}, "message": ..}]}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FieldErrors(pub BTreeMap<String, Vec<FieldError>>);

impl FieldErrors {
    /// Adds an error to `field`; used by `Repo::validate`, the second validation stage.
    pub fn add(&mut self, field: &str, code: &'static str, message: &str) {
        self.add_with_params(field, code, message, Map::new());
    }

    /// Like `add`, with the `params` used to build the message.
    pub fn add_with_params(
        &mut self,
        field: &str,
        code: &'static str,
        message: &str,
        params: Map<String, Json>,
    ) {
        self.0
            .entry(field.to_string())
            .or_default()
            .push(FieldError {
                code,
                params,
                message: message.to_string(),
            });
    }
}

impl FieldErrors {
    /// Returns whether there are no errors.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds the errors of `other`, appending to existing keys.
    pub fn merge(&mut self, other: FieldErrors) {
        for (field, list) in other.0 {
            self.0.entry(field).or_default().extend(list);
        }
    }

    /// Adds the errors of `other` with `prefix.` before each key (`items.0` and `quantity` give
    /// `items.0.quantity`).
    pub fn merge_prefixed(&mut self, prefix: &str, other: FieldErrors) {
        for (field, list) in other.0 {
            self.0
                .entry(format!("{prefix}.{field}"))
                .or_default()
                .extend(list);
        }
    }
}

const TEXT_MAX_BYTES: usize = 65_535;
const DATETIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6fZ";

/// Converts a Model to JSON, leaving out hidden, internal and unreadable columns.
///
/// Datetimes are `2026-10-17T01:02:03.123456Z`, dates `2026-10-01` and NULL is null.
pub fn to_json<E: EntityTrait>(
    fields: &[FieldMeta],
    model: &E::Model,
    readable: impl Fn(&FieldMeta) -> bool,
) -> Map<String, Json> {
    let mut out = Map::new();
    for f in fields {
        if f.hidden || f.internal || !readable(f) {
            continue;
        }
        let Some(column) = column_named::<E>(f.name) else {
            continue;
        };
        out.insert(f.name.to_string(), value_to_json(f.kind, model.get(column)));
    }
    out
}

fn value_to_json(kind: FieldKind, value: Value) -> Json {
    fn opt<T>(v: Value) -> Option<T>
    where
        Option<T>: ValueType,
    {
        <Option<T> as ValueType>::try_from(v).ok().flatten()
    }
    match kind {
        FieldKind::String { .. } | FieldKind::Text => opt::<String>(value).into(),
        FieldKind::Integer => opt::<i32>(value).into(),
        FieldKind::BigInteger => opt::<i64>(value).into(),
        FieldKind::Uuid => opt::<Uuid>(value).map(|u| u.to_string()).into(),
        FieldKind::Boolean => opt::<bool>(value).into(),
        FieldKind::Date => opt::<NaiveDate>(value)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .into(),
        FieldKind::DateTime => opt::<DateTime<Utc>>(value)
            .map(|t| t.format(DATETIME_FORMAT).to_string())
            .into(),
        FieldKind::Double => opt::<f64>(value).into(),
        FieldKind::Json => opt::<Json>(value).unwrap_or(Json::Null),
        FieldKind::Decimal { scale, .. } => opt::<Decimal>(value)
            .map(|mut d| {
                d.rescale(scale);
                d.to_string()
            })
            .into(),
    }
}

pub(crate) fn decimal_text(value: &Json) -> Option<String> {
    let s = match value {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        _ => return None,
    };
    let body = s.strip_prefix('-').unwrap_or(&s);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && frac.is_none_or(digits)).then_some(s)
}

/// Returns the integer digits (without leading zeros) and fraction digits (without trailing zeros).
fn decimal_digits(s: &str) -> (usize, usize) {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    (
        int.trim_start_matches('0').len(),
        frac.trim_end_matches('0').len(),
    )
}

/// Converts JSON to an ActiveModel (first validation stage), checking every field before returning
/// all errors.
///
/// - Error codes: `unknown` for a missing or hidden column; `read_only` for a primary key (except a
///   string key on add), a managed column or a non-writable one; `required` for null in a NOT NULL
///   column (also an empty string there; for a nullable string it becomes NULL); `type` for a
///   wrong type.
/// - Integers must be JSON integers in range. Dates are `YYYY-MM-DD`; datetimes are RFC 3339 with
///   an offset, converted to UTC and truncated to microseconds. json accepts an object or array
///   only, double any JSON number, decimal `-?[0-9]+(\.[0-9]+)?` as a string or number.
/// - `decimal_places` if the fraction exceeds `scale`, `max_digits` if the integer part exceeds
///   `precision - scale` (no rounding; trailing zeros do not count). `max_length` if a string
///   exceeds its length, or TEXT exceeds 65,535 bytes.
/// - `Add` fills omitted columns with defaults (also non-writable ones) and reports `required` for
///   writable NOT NULL columns without one; `Edit` leaves omitted columns `NotSet`. Primary keys
///   are never taken from JSON.
///
/// A value that does not match the column's Rust type becomes a `type_mismatch` error under key
/// `""`; use `try_from_json` to keep that apart from input errors.
pub fn from_json<E: EntityTrait>(
    fields: &[FieldMeta],
    input: &Map<String, Json>,
    mode: WriteMode,
    writable: impl Fn(&FieldMeta) -> bool,
) -> Result<E::ActiveModel, FieldErrors> {
    match try_from_json::<E>(fields, input, mode, writable, &Messages::default()) {
        Ok(parsed) => parsed,
        Err(e) => {
            let mut errors = FieldErrors::default();
            errors.add("", "type_mismatch", &e.to_string());
            Err(errors)
        }
    }
}

/// Like `from_json`, but returns a type mismatch (`DbErr`) apart from input errors (`FieldErrors`).
///
/// The outer `Err` is a cakedome bug or misconfiguration; the inner `Err` is an input error.
pub fn try_from_json<E: EntityTrait>(
    fields: &[FieldMeta],
    input: &Map<String, Json>,
    mode: WriteMode,
    writable: impl Fn(&FieldMeta) -> bool,
    messages: &Messages,
) -> Result<Result<E::ActiveModel, FieldErrors>, DbErr> {
    let mut errors = FieldErrors::default();
    let mut am = E::ActiveModel::default();

    for (key, value) in input {
        let Some(f) = fields.iter().find(|f| f.name == key && !f.hidden) else {
            errors.add(key, "unknown", &messages.text("unknown", &[]));
            continue;
        };
        // A primary key is writable only as a string on add; other types come from new_key or the
        // DB.
        let writable_key = mode == WriteMode::Add && matches!(f.kind, FieldKind::String { .. });
        if (f.primary_key && !writable_key) || f.managed || !writable(f) {
            errors.add(key, "read_only", &messages.text("read_only", &[]));
            continue;
        }
        apply::<E>(&mut am, &mut errors, f, value, messages)?;
    }

    if mode == WriteMode::Add {
        for f in fields {
            if f.hidden || f.primary_key || f.managed || input.contains_key(f.name) {
                continue;
            }
            match &f.default {
                // Defaults are set by the system, so they also fill non-writable columns.
                Some(default) => apply::<E>(&mut am, &mut errors, f, default, messages)?,
                // A non-writable column cannot be fixed by the user, so it is not `required`.
                None if !f.nullable && writable(f) => {
                    errors.add(f.name, "required", &messages.text("required", &[]))
                }
                None => {}
            }
        }
    }

    Ok(if errors.0.is_empty() {
        Ok(am)
    } else {
        Err(errors)
    })
}

fn apply<E: EntityTrait>(
    am: &mut E::ActiveModel,
    errors: &mut FieldErrors,
    f: &FieldMeta,
    value: &Json,
    messages: &Messages,
) -> Result<(), DbErr> {
    match json_to_value(f, value, messages) {
        Ok(v) => {
            if let Some(column) = column_named::<E>(f.name) {
                am.try_set(column, v)?;
            }
        }
        Err((code, message, params)) => errors.add_with_params(f.name, code, &message, params),
    }
    Ok(())
}

/// A failed conversion: error code, message, and the values used in the message.
pub(crate) type Rejected = (&'static str, String, Map<String, Json>);

pub(crate) fn json_to_value(
    f: &FieldMeta,
    value: &Json,
    messages: &Messages,
) -> Result<Value, Rejected> {
    let invalid = || ("type", messages.text("type", &[]), Map::new());
    let required = || ("required", messages.text("required", &[]), Map::new());

    let is_text = matches!(f.kind, FieldKind::String { .. } | FieldKind::Text);
    let is_empty_text = is_text && value.as_str() == Some("");
    if value.is_null() || is_empty_text {
        if !f.nullable {
            return Err(required());
        }
        return Ok(null_value(f.kind));
    }

    match f.kind {
        FieldKind::String { max_length } => {
            let s = value.as_str().ok_or_else(invalid)?;
            if s.chars().count() > max_length as usize {
                return Err(rejected(
                    messages,
                    "max_length",
                    "max_length",
                    params([("max", max_length.into()), ("unit", "chars".into())]),
                ));
            }
            Ok(text_value(f, s))
        }
        FieldKind::Text => {
            let s = value.as_str().ok_or_else(invalid)?;
            if s.len() > TEXT_MAX_BYTES {
                return Err(rejected(
                    messages,
                    "max_length",
                    "max_length.bytes",
                    params([("max", TEXT_MAX_BYTES.into()), ("unit", "bytes".into())]),
                ));
            }
            Ok(text_value(f, s))
        }
        FieldKind::Integer => {
            let n = value.as_i64().ok_or_else(invalid)?;
            let n = <i32 as TryFrom<i64>>::try_from(n).map_err(|_| invalid())?;
            Ok(if f.nullable {
                Value::from(Some(n))
            } else {
                Value::from(n)
            })
        }
        FieldKind::BigInteger => {
            let n = value.as_i64().ok_or_else(invalid)?;
            Ok(if f.nullable {
                Value::from(Some(n))
            } else {
                Value::from(n)
            })
        }
        FieldKind::Uuid => {
            let u = Uuid::from_json(value).ok_or_else(invalid)?;
            Ok(if f.nullable {
                Value::from(Some(u))
            } else {
                Value::from(u)
            })
        }
        FieldKind::Boolean => {
            let b = value.as_bool().ok_or_else(invalid)?;
            Ok(if f.nullable {
                Value::from(Some(b))
            } else {
                Value::from(b)
            })
        }
        FieldKind::Date => {
            let s = value.as_str().ok_or_else(invalid)?;
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| invalid())?;
            if d.format("%Y-%m-%d").to_string() != s {
                return Err(invalid());
            }
            Ok(if f.nullable {
                Value::from(Some(d))
            } else {
                Value::from(d)
            })
        }
        FieldKind::DateTime => {
            let s = value.as_str().ok_or_else(invalid)?;
            let t = DateTime::parse_from_rfc3339(s).map_err(|_| invalid())?;
            let t = t.with_timezone(&Utc).trunc_subsecs(6);
            Ok(if f.nullable {
                Value::from(Some(t))
            } else {
                Value::from(t)
            })
        }
        FieldKind::Json => {
            if !(value.is_object() || value.is_array()) {
                return Err(invalid());
            }
            Ok(if f.nullable {
                Value::from(Some(value.clone()))
            } else {
                Value::from(value.clone())
            })
        }
        FieldKind::Double => {
            let n = value.as_f64().ok_or_else(invalid)?;
            Ok(if f.nullable {
                Value::from(Some(n))
            } else {
                Value::from(n)
            })
        }
        FieldKind::Decimal { precision, scale } => {
            let s = decimal_text(value).ok_or_else(invalid)?;
            let (int_digits, frac_digits) = decimal_digits(&s);
            if frac_digits > scale as usize {
                return Err(rejected(
                    messages,
                    "decimal_places",
                    "decimal_places",
                    params([("scale", scale.into())]),
                ));
            }
            let max_int = precision.saturating_sub(scale);
            if int_digits > max_int as usize {
                return Err(rejected(
                    messages,
                    "max_digits",
                    "max_digits",
                    params([("digits", max_int.into())]),
                ));
            }
            let d = s.parse::<Decimal>().map_err(|_| invalid())?;
            Ok(if f.nullable {
                Value::from(Some(d))
            } else {
                Value::from(d)
            })
        }
    }
}

fn rejected(
    messages: &Messages,
    code: &'static str,
    key: &str,
    params: Map<String, Json>,
) -> Rejected {
    (code, messages.text_params(key, &params), params)
}

fn text_value(f: &FieldMeta, s: &str) -> Value {
    if f.nullable {
        Value::from(Some(s.to_string()))
    } else {
        Value::from(s.to_string())
    }
}

fn null_value(kind: FieldKind) -> Value {
    match kind {
        FieldKind::String { .. } | FieldKind::Text => Value::from(None::<String>),
        FieldKind::Integer => Value::from(None::<i32>),
        FieldKind::BigInteger => Value::from(None::<i64>),
        FieldKind::Uuid => Value::from(None::<Uuid>),
        FieldKind::Boolean => Value::from(None::<bool>),
        FieldKind::Date => Value::from(None::<NaiveDate>),
        FieldKind::DateTime => Value::from(None::<DateTime<Utc>>),
        FieldKind::Double => Value::from(None::<f64>),
        FieldKind::Json => Value::from(None::<Json>),
        FieldKind::Decimal { .. } => Value::from(None::<Decimal>),
    }
}

/// Returns the initial values of a new row: defaults or null, without managed, hidden and
/// unreadable columns.
///
/// A primary key is left out (it is generated), except a string one, which the input decides.
pub fn defaults_json(
    fields: &[FieldMeta],
    readable: impl Fn(&FieldMeta) -> bool,
) -> Map<String, Json> {
    fields
        .iter()
        .filter(|f| {
            (!f.primary_key || matches!(f.kind, FieldKind::String { .. }))
                && !f.managed
                && !f.hidden
                && readable(f)
        })
        .map(|f| (f.name.to_string(), f.default.clone().unwrap_or(Json::Null)))
        .collect()
}
