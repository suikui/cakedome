//! DB-backed cakedome tests on in-memory SQLite, a fresh DB per test.
//! One binary keeps the build to a single pass; `db_pg` and `db_mysql` reuse these files.

#[path = "sqlite.rs"]
mod backend;
#[path = "../common/mod.rs"]
mod common;
mod entities;
mod support;

mod association_conditions;
mod before_find;
mod contain;
mod extensions;
mod hooks;
mod keys;
mod lookups;
mod observers;
mod patch;
mod patch_associations;
mod patch_links;
mod policy;
mod purge;
mod read;
mod relations;
mod save_associations;
mod sqlite_only;
mod unique_and_warnings;
mod unordered_links;
mod write;
