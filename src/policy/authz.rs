//! A mark that an authorization check passed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A mark that a request passed an authorization check.
///
/// One mark is created per request and shared (`Arc<AtomicBool>`) with the code that checks, which
/// calls `mark()` when it allows; the creator keeps a clone and reads `is_marked()` afterwards.
/// `SeqCst` is the safest ordering; the cost is negligible at one or two operations per request.
#[derive(Clone, Default)]
pub struct AuthzMark(Arc<AtomicBool>);

impl AuthzMark {
    /// Records that a check allowed the request.
    pub fn mark(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns whether any check allowed the request.
    pub fn is_marked(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
