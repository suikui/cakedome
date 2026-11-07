//! Message tables for errors and warnings, set through `Registry::set_messages`.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// A table of message templates (message key to template) that replaces the default messages.
///
/// `{name}` in a template is replaced by the error's value. Unknown keys use the default.
#[derive(Clone, Debug, Default)]
pub struct Messages(HashMap<&'static str, &'static str>);

impl Messages {
    /// Creates a table from (key, template) pairs.
    pub fn new(entries: &[(&'static str, &'static str)]) -> Self {
        Messages(entries.iter().copied().collect())
    }

    /// Renders the template for `key` (or `default`), replacing `{name}` with the `params` values.
    pub fn render(&self, key: &str, default: &str, params: &[(&str, String)]) -> String {
        let mut text = self.0.get(key).copied().unwrap_or(default).to_string();
        for (name, value) in params {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    /// Like `render`, taking the error's values; numbers are formatted in decimal.
    pub fn render_params(&self, key: &str, default: &str, params: &Map<String, Value>) -> String {
        let values: Vec<(&str, String)> = params
            .iter()
            .map(|(name, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (name.as_str(), text)
            })
            .collect();
        self.render(key, default, &values)
    }

    pub(crate) fn text(&self, key: &str, params: &[(&str, String)]) -> String {
        self.render(key, default_of(key), params)
    }

    pub(crate) fn text_params(&self, key: &str, params: &Map<String, Value>) -> String {
        self.render_params(key, default_of(key), params)
    }
}

fn default_of(key: &str) -> &str {
    DEFAULTS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(key, |(_, template)| template)
}

/// Builds a `params` map from name and value pairs.
pub fn params<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The message keys cakedome uses and their default templates, public so an app can translate all.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("required", "This field is required"),
    ("type", "The value has an invalid format"),
    ("unknown", "This field does not exist"),
    ("read_only", "This field cannot be changed"),
    ("max_length", "Must be at most {max} characters"),
    ("max_length.bytes", "Must be at most {max} bytes"),
    ("decimal_places", "Must have at most {scale} decimal places"),
    (
        "max_digits",
        "Must have at most {digits} digits before the decimal point",
    ),
    ("unique", "This value is already in use"),
    ("duplicate", "The same row is given twice"),
    ("invalid_ref", "This value cannot be selected"),
    ("not_found", "Not found"),
    (
        "invalid.parent_key",
        "Cannot be saved because the parent's {key} is empty",
    ),
    (
        "warning.duplicate",
        "A row with the same {label} already exists",
    ),
];
