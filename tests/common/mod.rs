//! Table rules used by cakedome tests.

#![allow(dead_code)]

use cakedome::meta::FieldMeta;
use cakedome::meta::{Lifecycle, TableRules, TableSpec, spec_for};
use cakedome::registry::Registry;
use sea_orm::EntityTrait;
use serde_json::json;

pub struct TestRules;

impl TableRules for TestRules {
    fn spec(&self, table: &str, columns: &[&'static str]) -> Result<TableSpec, String> {
        let has = |name: &str| columns.iter().find(|c| **c == name).copied();
        if table.starts_with("master_") {
            for required in ["id", "code", "name", "is_active"] {
                if has(required).is_none() {
                    return Err(format!("master_* requires a {required} column"));
                }
            }
        }
        if table.starts_with("setting_") && has("seq").is_none() {
            return Err("setting_* requires a seq column".to_string());
        }
        let active_column = has("is_active");
        let lifecycle = match active_column {
            None => Lifecycle::HardDelete,
            Some(_) if table.starts_with("master_") || table == "users" => Lifecycle::Activatable,
            Some(_) => Lifecycle::SoftDelete,
        };
        let mut defaults = Vec::new();
        if let (Lifecycle::Activatable, Some(c)) = (lifecycle, active_column) {
            defaults.push((c, json!(true)));
        }
        if let (true, Some(c)) = (table.starts_with("setting_"), has("seq")) {
            defaults.push((c, json!(0)));
        }
        Ok(TableSpec {
            lifecycle,
            active_column,
            created_column: has("created"),
            modified_column: has("modified"),
            order_column: has("seq"),
            defaults,
            uniques: if table.starts_with("master_") {
                vec![vec!["code"]]
            } else {
                Vec::new()
            },
            warn_duplicates: Vec::new(),
        })
    }
}

pub fn registry() -> Registry {
    Registry::new(TestRules, &[], &[])
}

/// Context without a DB (for Policy-only tests; reading the DB fails).
pub fn context(identity: impl cakedome::policy::Identity) -> cakedome::Context {
    cakedome::Context::new(
        identity,
        sea_orm::DatabaseConnection::default(),
        cakedome::policy::AuthzMark::default(),
        std::sync::Arc::new(registry()),
    )
}

/// Rule result for `E` (panics with the reason if the rules reject it).
pub fn spec<E: EntityTrait>() -> TableSpec {
    spec_for::<E>(&TestRules).unwrap_or_else(|e| panic!("{}: {e}", E::default().table_name()))
}

pub fn fields<E: EntityTrait>() -> Vec<FieldMeta> {
    cakedome::meta::fields_of::<E>(&spec::<E>())
}
