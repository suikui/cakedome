//! The current time.

use chrono::{DateTime, SubsecRound, Utc};

/// Returns the current UTC time truncated to microseconds.
///
/// `DATETIME(6)` rounds on save, so truncating keeps the value equal to what is read back.
pub fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}
