//! Per-column access (`ColumnAccess`, returned by `TablePolicy::columns`).
//!
//! Columns are held by name so the value can be carried across `.await` without requiring
//! `Entity::Column` to be `Send + Sync`.

use std::collections::HashMap;
use std::marker::PhantomData;

use sea_orm::{Condition, EntityTrait, IdenStatic};

use crate::meta::{FieldMeta, column_named};

#[derive(Clone, Debug)]
struct Rule {
    readable: bool,
    writable: bool,
    read_where: Option<Condition>,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            readable: true,
            writable: true,
            read_where: None,
        }
    }
}

/// The columns the viewer can read and write (table level) and the conditions that hide a column
/// per row. Returned in one call by `TablePolicy::columns`.
///
/// Unrestricted columns are readable and writable. Start from `all()` and add restrictions:
///
/// ```text
/// Ok(ColumnAccess::all().hide(Column::CreditLimit).read_only(Column::Note))
/// ```
///
/// Restrictions on the same column combine (two `read_where` calls are ANDed).
pub struct ColumnAccess<E: EntityTrait> {
    rules: HashMap<String, Rule>,
    _entity: PhantomData<fn() -> E>,
}

impl<E: EntityTrait> Clone for ColumnAccess<E> {
    fn clone(&self) -> Self {
        Self {
            rules: self.rules.clone(),
            _entity: PhantomData,
        }
    }
}

impl<E: EntityTrait> std::fmt::Debug for ColumnAccess<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnAccess")
            .field("rules", &self.rules)
            .finish()
    }
}

impl<E: EntityTrait> ColumnAccess<E> {
    /// Returns an access that allows reading and writing every column.
    pub fn all() -> Self {
        Self {
            rules: HashMap::new(),
            _entity: PhantomData,
        }
    }

    fn rule_mut(&mut self, column: E::Column) -> &mut Rule {
        self.rules.entry(column.as_str().to_string()).or_default()
    }

    /// Makes `column` unreadable and unwritable (absent from output, schema and search; `unknown`
    /// on input).
    pub fn hide(mut self, column: E::Column) -> Self {
        let rule = self.rule_mut(column);
        rule.readable = false;
        rule.writable = false;
        self
    }

    /// Makes `column` readable but not writable (`readOnly` in the schema, `read_only` on input).
    pub fn read_only(mut self, column: E::Column) -> Self {
        self.rule_mut(column).writable = false;
        self
    }

    /// Makes `column` readable only in rows matching `condition`.
    ///
    /// The condition (SQL) applies to output, display names, search and the `q` of choices, not to
    /// sorting. A column hidden at table level stays unreadable.
    pub fn read_where(mut self, column: E::Column, condition: Condition) -> Self {
        let rule = self.rule_mut(column);
        rule.read_where = Some(match rule.read_where.take() {
            Some(old) => Condition::all().add(old).add(condition),
            None => condition,
        });
        self
    }

    /// Returns whether `column` is readable at table level.
    pub fn can_read(&self, column: E::Column) -> bool {
        self.can_read_named(column.as_str())
    }

    /// Returns whether `column` is writable.
    pub fn can_write(&self, column: E::Column) -> bool {
        self.can_write_named(column.as_str())
    }

    /// Returns the condition of rows where `column` is readable; `None` means all rows.
    pub fn read_scope(&self, column: E::Column) -> Option<Condition> {
        self.read_scope_named(column.as_str())
    }

    /// Like `can_read`, by column name.
    pub fn can_read_named(&self, name: &str) -> bool {
        self.rules.get(name).is_none_or(|r| r.readable)
    }

    /// Like `can_write`, by column name.
    pub fn can_write_named(&self, name: &str) -> bool {
        self.rules.get(name).is_none_or(|r| r.writable)
    }

    /// Like `read_scope`, by column name.
    pub fn read_scope_named(&self, name: &str) -> Option<Condition> {
        self.rules.get(name).and_then(|r| r.read_where.clone())
    }
}

/// Returns whether the field is readable at table level, reading a resolved `ColumnAccess`.
pub fn table_readable<E: EntityTrait>(access: &ColumnAccess<E>, f: &FieldMeta) -> bool {
    column_named::<E>(f.name).is_some() && access.can_read_named(f.name)
}

/// Returns whether the field is writable at table level, reading a resolved `ColumnAccess`.
pub fn table_writable<E: EntityTrait>(access: &ColumnAccess<E>, f: &FieldMeta) -> bool {
    column_named::<E>(f.name).is_some() && access.can_write_named(f.name)
}
