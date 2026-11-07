//! Permission string matching (allow / deny) and Identity. No DB.

use cakedome::policy::{BasicIdentity, PermissionSet, is_valid_pattern};

fn set(patterns: &[&str]) -> PermissionSet {
    PermissionSet::new(patterns.iter().copied())
}

#[test]
fn exact_pattern_matches_only_itself() {
    let p = set(&["entity.orders.view"]);

    assert!(p.allows("entity.orders.view"));
    assert!(!p.allows("entity.orders.edit"));
    assert!(!p.allows("entity.orders"));
    assert!(!p.allows("entity.orders.view.extra"));
}

#[test]
fn trailing_wildcard_matches_one_or_more_segments_below() {
    let p = set(&["entity.orders.*"]);

    assert!(p.allows("entity.orders.view"));
    assert!(p.allows("entity.orders.items.view"));
    assert!(!p.allows("entity.orders"), "does not match the bare prefix");
    assert!(!p.allows("entity.ordersx.view"), "does not cut mid-segment");
    assert!(!p.allows("entity.customers.view"));
}

#[test]
fn star_alone_matches_everything() {
    let p = set(&["*"]);

    assert!(p.allows("entity.orders.view"));
    assert!(p.allows("route.admin"));
    assert!(p.allows("x"));
}

#[test]
fn matching_is_case_sensitive() {
    let p = set(&["entity.orders.view"]);

    assert!(!p.allows("Entity.Orders.View"));
}

#[test]
fn invalid_patterns_never_match() {
    let p = set(&[
        "entity.*.view",
        "entity.ord*",
        "",
        "entity..view",
        ".entity",
        "entity.",
        "*.view",
        "entity.orders.view ",
        "entity.注文.view",
    ]);

    for required in [
        "entity.orders.view",
        "entity.ord",
        "entity.orders",
        "entity..view",
        "x.view",
    ] {
        assert!(!p.allows(required), "{required}");
    }
    assert!(p.patterns().is_empty(), "invalid strings are dropped");
}

#[test]
fn required_permission_with_wildcard_is_never_allowed() {
    let p = set(&["*"]);

    assert!(!p.allows("entity.*"));
    assert!(!p.allows("*"));
    assert!(!p.allows(""));
    assert!(!p.allows("entity..view"));
}

#[test]
fn validates_patterns() {
    for ok in [
        "*",
        "entity.orders.view",
        "entity.orders.*",
        "route.admin-users.view",
        "entity.order_items.view",
        "A1",
    ] {
        assert!(is_valid_pattern(ok), "{ok}");
    }
    for ng in [
        "",
        "entity.*.view",
        "entity.ord*",
        "**",
        "entity..view",
        "entity.",
        "entity orders",
        "entity.注文",
    ] {
        assert!(!is_valid_pattern(ng), "{ng}");
    }
}

#[test]
fn patterns_are_sorted_and_deduplicated() {
    let p = set(&["b.view", "a.*", "b.view", "bad.*.x", "*"]);

    assert_eq!(p.patterns(), ["*", "a.*", "b.view"]);
}

#[test]
fn empty_set_allows_nothing() {
    let p = PermissionSet::empty();

    assert!(!p.allows("entity.orders.view"));
    assert!(p.patterns().is_empty());
    assert_eq!(p, set(&[]));
}

#[test]
fn deny_wins_over_allow() {
    let p = set(&["entity.orders.*"]).with_denied(["entity.orders.delete"]);

    assert!(p.allows("entity.orders.view"));
    assert!(p.allows("entity.orders.edit"));
    assert!(!p.allows("entity.orders.delete"));
}

#[test]
fn deny_wildcard_removes_part_of_star() {
    let p = set(&["*"]).with_denied(["entity.*"]);

    assert!(!p.allows("entity.orders.view"));
    assert!(p.allows("route.admin"));
}

#[test]
fn narrower_allow_does_not_override_broader_deny() {
    let p = set(&["entity.orders.view"]).with_denied(["entity.*"]);

    assert!(!p.allows("entity.orders.view"));
}

#[test]
fn deny_star_removes_everything() {
    let p = set(&["*"]).with_denied(["*"]);

    assert!(!p.allows("entity.orders.view"));
    assert!(!p.allows("route.admin"));
}

#[test]
fn denied_patterns_are_sorted_deduplicated_and_validated() {
    let p = set(&["*"]).with_denied(["b.view", "a.*", "b.view", "bad.*.x"]);

    assert_eq!(p.denied(), ["a.*", "b.view"]);
    assert_eq!(p.patterns(), ["*"]);
}

#[test]
fn invalid_deny_pattern_denies_nothing() {
    let p = set(&["entity.orders.view"]).with_denied(["entity.*.view", ""]);

    assert!(p.allows("entity.orders.view"));
    assert!(p.denied().is_empty());
}

#[test]
fn empty_deny_list_changes_nothing() {
    let empty: [&str; 0] = [];
    assert_eq!(set(&["a.view"]).with_denied(empty), set(&["a.view"]));
}

#[test]
fn anonymous_identity_cannot_do_anything() {
    let id = BasicIdentity::anonymous();

    assert_eq!(id.user_id, None);
    assert!(!id.is_authenticated());
    assert_eq!(id.role, None);
    assert!(!id.can("entity.orders.view"));
}

#[test]
fn user_identity_checks_its_permissions() {
    let id = BasicIdentity::user(
        7,
        Some("user".to_string()),
        set(&["entity.customers.*"]).with_denied(["entity.customers.delete"]),
    );

    assert_eq!(id.user_id, Some(cakedome::key::RowId::Int(7)));
    assert!(id.is_authenticated());
    assert_eq!(id.role.as_deref(), Some("user"));
    assert!(id.can("entity.customers.view"));
    assert!(!id.can("entity.customers.delete"));
}

#[test]
fn user_without_role_is_still_authenticated() {
    let id = BasicIdentity::user(8, None, PermissionSet::empty());

    assert!(id.is_authenticated());
    assert_eq!(id.role, None);
    assert!(!id.can("entity.customers.view"));
}
