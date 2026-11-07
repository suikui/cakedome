//! DB-backed cakedome tests on in-memory SQLite, a fresh DB per test.
//! One binary keeps the build to a single pass; `db_pg` and `db_mysql` reuse these files.

#[path = "sqlite.rs"]
mod backend;
#[path = "../common/mod.rs"]
mod common;
mod entities;
mod support;

mod before_find;
mod extensions;
mod hooks;
mod keys;
mod patch;
mod policy;
mod read;
mod relations;
mod sqlite_only;
mod write;
