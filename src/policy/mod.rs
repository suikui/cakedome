//! Permission checks, the request's `Identity` and the kind of operation (`Action`).

mod authz;
pub(crate) mod columns;
mod identity;
mod permission;
pub(crate) mod resolve;
mod table;
mod traits;

pub use authz::AuthzMark;
pub use columns::ColumnAccess;
pub(crate) use columns::{table_readable, table_writable};
pub use identity::{BasicIdentity, Identity};
pub use permission::{PermissionSet, is_valid_pattern};
pub use table::{Action, entity_permission};
pub use traits::{EntityPolicy, TablePolicy};
