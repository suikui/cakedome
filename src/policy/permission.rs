//! Matching of permission strings (`entity.orders.view`, `entity.*`, `*`).

fn is_segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn is_valid_permission(p: &str) -> bool {
    p.split('.').all(is_segment)
}

/// Returns whether `p` is a valid permission pattern: `*`, or segments with `*` only last.
pub fn is_valid_pattern(p: &str) -> bool {
    if p == "*" {
        return true;
    }
    let mut segments = p.split('.').peekable();
    while let Some(seg) = segments.next() {
        let last = segments.peek().is_none();
        if !(is_segment(seg) || (last && seg == "*")) {
            return false;
        }
    }
    true
}

fn matches(pattern: &str, permission: &str) -> bool {
    if pattern == "*" || pattern == permission {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => permission
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1),
        None => false,
    }
}

/// A set of permission patterns with an optional deny list; a deny match wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionSet {
    patterns: Vec<String>,
    denied: Vec<String>,
}

impl PermissionSet {
    /// Creates a set from `patterns`, dropping invalid ones; stored sorted and deduplicated.
    pub fn new<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut patterns: Vec<String> = patterns
            .into_iter()
            .map(Into::into)
            .filter(|p| is_valid_pattern(p))
            .collect();
        patterns.sort();
        patterns.dedup();
        Self {
            patterns,
            denied: Vec::new(),
        }
    }

    /// Replaces the deny patterns, dropping invalid ones; stored sorted and deduplicated.
    pub fn with_denied<I, S>(mut self, denied: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut denied: Vec<String> = denied
            .into_iter()
            .map(Into::into)
            .filter(|p| is_valid_pattern(p))
            .collect();
        denied.sort();
        denied.dedup();
        self.denied = denied;
        self
    }

    /// Returns a set that allows nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns whether `permission` matches a pattern and no deny pattern.
    pub fn allows(&self, permission: &str) -> bool {
        is_valid_permission(permission)
            && self.patterns.iter().any(|p| matches(p, permission))
            && !self.denied.iter().any(|p| matches(p, permission))
    }

    /// Returns the allow patterns.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Returns the deny patterns.
    pub fn denied(&self) -> &[String] {
        &self.denied
    }
}
