//! Business-rule validation, the second stage after `marshal::from_json` has checked types,
//! required and length.

use sea_orm::EntityTrait;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::marshal::WriteMode;
use crate::policy::Identity;

/// The content about to be saved, passed to `Repo::validate`.
pub struct ValidationInput<'a, E: EntityTrait> {
    pub identity: &'a dyn Identity,
    /// Add or update.
    pub mode: WriteMode,
    /// The current row for an update; `None` for an add.
    pub before: Option<&'a E::Model>,
    /// The ActiveModel about to be written (`Set` columns are the values to write).
    pub after: &'a E::ActiveModel,
}

/// Something that can be saved but a person should confirm.
///
/// A save with unaccepted warnings is returned without saving; `key` is the acceptance token, sent
/// back in `acceptWarnings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// `{field}.{code}`.
    pub key: String,
    pub field: String,
    pub code: String,
    /// Values used to build the message; omitted from JSON when empty.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    pub message: String,
}

impl Warning {
    /// Creates a warning for `field` with `code`; the key is `{field}.{code}`.
    pub fn new(field: &str, code: &str, message: &str) -> Self {
        Self {
            key: format!("{field}.{code}"),
            field: field.to_string(),
            code: code.to_string(),
            params: Map::new(),
            message: message.to_string(),
        }
    }

    /// Sets the values used to build the message.
    pub fn with_params(mut self, params: Map<String, Value>) -> Self {
        self.params = params;
        self
    }
}
