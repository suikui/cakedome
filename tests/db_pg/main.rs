//! Runs the DB tests on PostgreSQL, sharing `tests/db`; only the DB setup (`pg.rs`) differs.
//! Built with the `test-postgres` feature; connects via `CAKEDOME_TEST_PG_URL`.

#[path = "pg.rs"]
mod backend;
#[path = "../common/mod.rs"]
mod common;
#[path = "../db/entities.rs"]
mod entities;
#[path = "../db/support.rs"]
mod support;

#[path = "../db/association_conditions.rs"]
mod association_conditions;
#[path = "../db/before_find.rs"]
mod before_find;
#[path = "../db/contain.rs"]
mod contain;
#[path = "../db/extensions.rs"]
mod extensions;
#[path = "../db/hooks.rs"]
mod hooks;
#[path = "../db/keys.rs"]
mod keys;
#[path = "../db/lookups.rs"]
mod lookups;
#[path = "../db/observers.rs"]
mod observers;
#[path = "../db/patch.rs"]
mod patch;
#[path = "../db/patch_associations.rs"]
mod patch_associations;
#[path = "../db/patch_links.rs"]
mod patch_links;
#[path = "../db/policy.rs"]
mod policy;
#[path = "../db/purge.rs"]
mod purge;
#[path = "../db/read.rs"]
mod read;
#[path = "../db/relations.rs"]
mod relations;
mod row_lock;
#[path = "../db/save_associations.rs"]
mod save_associations;
#[path = "../db/unique_and_warnings.rs"]
mod unique_and_warnings;
#[path = "../db/unordered_links.rs"]
mod unordered_links;
#[path = "../db/write.rs"]
mod write;
