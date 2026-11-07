//! An ORM layer on SeaORM that runs every read and write of an entity through a Repo with a
//! Policy.
//!
//! Implement `Repo`, `TablePolicy` and `EntityPolicy` for each entity, register them in a
//! `Registry`, and call `ctx.repo::<R>()` on a `Context` made per request. `prelude` imports the
//! common items; `layer` holds building blocks for layers on top of the ORM, such as a JSON API.

pub mod association;
pub mod clock;
pub mod context;
pub mod defaults;
pub(crate) mod json;
pub mod key;
pub mod layer;
pub mod marshal;
pub mod messages;
pub mod meta;
pub mod observer;
pub mod patch;
pub mod policy;
pub mod prelude;
pub mod registry;
pub mod repo;
pub mod validate;

pub use context::{Context, Transaction};
pub use repo::{DefaultRepo, GuardedRepo, Repo, RepoHandle};
