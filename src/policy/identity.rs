use std::any::Any;
use std::fmt;

use super::permission::PermissionSet;
use crate::key::RowId;

/// Who is operating. Implemented by the application's user type.
pub trait Identity: Any + Send + Sync + fmt::Debug {
    /// Returns the logged-in user's id; `None` for anonymous and system.
    fn user_id(&self) -> Option<RowId>;

    /// Returns whether the identity has the permission. Only asked for logged-in users; cakedome
    /// answers for system and anonymous first.
    fn has_permission(&self, permission: &str) -> bool;

    /// Returns whether this is the system (batch, seed, scheduled jobs).
    fn is_system(&self) -> bool {
        false
    }
}

// Inherent methods of `dyn Identity`, not trait methods, so an app cannot override them and break
// "system is always allowed, anonymous never".
impl dyn Identity {
    /// Returns whether this is a logged-in user or the system.
    pub fn is_authenticated(&self) -> bool {
        self.is_system() || self.user_id().is_some()
    }

    /// Returns whether the identity has the permission: always for system, never for anonymous,
    /// otherwise `has_permission`.
    pub fn can(&self, permission: &str) -> bool {
        self.is_system() || (self.is_authenticated() && self.has_permission(permission))
    }

    /// Downcasts to the application's type.
    pub fn downcast_ref<T: Identity>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }
}

/// The default `Identity`: user id, one role and a set of permissions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicIdentity {
    /// The logged-in user's id; `None` for anonymous and system.
    pub user_id: Option<RowId>,
    pub role: Option<String>,
    pub permissions: PermissionSet,
    /// Whether this is the system. Only `BasicIdentity::system()` sets it; HTTP identities never
    /// do.
    pub system: bool,
}

impl BasicIdentity {
    /// Creates an anonymous identity.
    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            role: None,
            permissions: PermissionSet::empty(),
            system: false,
        }
    }

    /// Creates the system identity (batch, seed, scheduled jobs).
    ///
    /// It is treated as logged in and `can` is always true, so policies written with
    /// `identity.can(..)` allow everything without `unchecked()`. `user_id` is `None`, so a
    /// `CurrentUserId` default is null; policies using `user_id()` must handle the system (`None`).
    pub fn system() -> Self {
        Self {
            user_id: None,
            role: None,
            permissions: PermissionSet::empty(),
            system: true,
        }
    }

    /// Creates a logged-in user identity.
    pub fn user(
        user_id: impl Into<RowId>,
        role: Option<String>,
        permissions: PermissionSet,
    ) -> Self {
        Self {
            user_id: Some(user_id.into()),
            role,
            permissions,
            system: false,
        }
    }

    /// Delegates to `dyn Identity::is_authenticated`.
    pub fn is_authenticated(&self) -> bool {
        (self as &dyn Identity).is_authenticated()
    }

    /// Delegates to `dyn Identity::can`.
    pub fn can(&self, permission: &str) -> bool {
        (self as &dyn Identity).can(permission)
    }
}

impl Identity for BasicIdentity {
    fn user_id(&self) -> Option<RowId> {
        self.user_id.clone()
    }

    fn has_permission(&self, permission: &str) -> bool {
        self.permissions.allows(permission)
    }

    fn is_system(&self) -> bool {
        self.system
    }
}
