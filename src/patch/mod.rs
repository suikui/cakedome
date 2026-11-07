//! `Patching::patch` and `save_patch`: convert a JSON body, with associated rows, to an ActiveModel
//! and save it in one transaction. Layers built on top of the ORM (such as a JSON API) use these
//! two.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{DbErr, EntityTrait};
use serde_json::Value as Json;

use crate::association::{PreparedChildren, PreparedParent};
use crate::context::Context;
use crate::key::Key;
use crate::marshal::FieldErrors;
use crate::registry::EntityMeta;
use crate::repo::{GuardedRepo, RepoError, RepoHandle};

mod child;
mod save;

pub(crate) use child::{
    ChildSpec, JoinSpec, prepare_children, prepare_join, prepare_one, prepare_parent,
};

/// The error of `patch` and `save_patch`.
#[derive(Debug)]
#[non_exhaustive]
pub enum PatchError {
    /// A Repo error (permission, not found, DB, ..).
    Repo(RepoError),
    /// Input validation errors, from both stages.
    Invalid(FieldErrors),
    /// Optimistic-lock conflict. `current` is the current row's `data`, in the shape `get` returns.
    Conflict { current: serde_json::Value },
    /// The body has a wrong shape.
    BadRequest(String),
    /// There are Warnings not listed in `acceptWarnings`.
    Warnings(Vec<crate::validate::Warning>),
}

impl From<RepoError> for PatchError {
    /// `Validation` becomes `Invalid`; everything else is wrapped in `Repo`.
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::Validation(errors) => PatchError::Invalid(errors),
            RepoError::Warnings(warnings) => PatchError::Warnings(warnings),
            other => PatchError::Repo(other),
        }
    }
}

impl From<DbErr> for PatchError {
    fn from(e: DbErr) -> Self {
        PatchError::Repo(RepoError::from(e))
    }
}

pub(crate) struct Marshaller<R: GuardedRepo> {
    repo: Arc<R>,
}

impl<R: GuardedRepo> Marshaller<R> {
    pub(crate) fn meta<'a>(&self, ctx: &'a Context) -> Result<&'a EntityMeta, PatchError> {
        Ok(ctx
            .registry()
            .meta_of::<R::Entity>()
            .ok_or_else(|| RepoError::NotRegistered(R::repo_name().to_string()))?)
    }
}

pub(crate) fn marshaller_of<T: GuardedRepo>(ctx: &Context) -> Option<Marshaller<T>> {
    Some(Marshaller {
        repo: ctx.registry().repo::<T>()?,
    })
}

/// A converted body, not yet saved. Change `am` if needed, then pass it to `save_patch`.
///
/// ```text
/// let mut p = repo.patch(Some(id), &body).await?;
/// p.am.note = Set(Some("added in code".into()));
/// repo.save_patch(p).await?;
/// ```
pub struct Patch<R: GuardedRepo> {
    /// The values to write (including the primary key on update). May be changed in code.
    pub am: <R::Entity as EntityTrait>::ActiveModel,
    pub(super) id: Option<Key<R>>,
    pub(super) expected: Option<DateTime<Utc>>,
    pub(super) parents: Vec<Box<dyn PreparedParent>>,
    pub(super) prepared: Vec<Box<dyn PreparedChildren>>,
    pub(super) accepted: Vec<String>,
}

impl<R: GuardedRepo> Patch<R> {
    /// Returns the primary key of the row being updated, or `None` for an insert.
    pub fn id(&self) -> Option<&Key<R>> {
        self.id.as_ref()
    }
}

/// Options for `patch_with`.
#[derive(Clone, Debug, Default)]
pub struct PatchOptions {
    // `None` falls back to the body's `associated`, then to the default: all usable associations of
    // the parent, none inside child or target rows.
    associated: Option<Vec<String>>,
}

impl PatchOptions {
    /// Sets the association names to accept (`"items"`); keys for others are silently dropped, and
    /// an empty list accepts none. The body's `associated` is then ignored. An unregistered name is
    /// a code error, `Repo(Db)`.
    ///
    /// A nested name (`"orders.items"`) accepts `orders` at the top and only `items` inside its
    /// rows.
    pub fn associated<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.associated = Some(names.into_iter().map(Into::into).collect());
        self
    }

    pub(super) fn listed(&self) -> Option<&[String]> {
        self.associated.as_deref()
    }
}

/// Adds `patch` and `save_patch` to `RepoHandle` (`use cakedome::patch::Patching;`).
///
/// ```text
/// let repo = ctx.repo::<OrdersRepo>();
/// let mut p = repo.patch(Some(id), &body).await?;   // does not save
/// p.am.note = Set(Some("added in code".into()));
/// let saved = repo.save_patch(p).await?;            // writes the parent and associations
/// ```
///
/// The checks are the same even for an `unchecked()` handle: the mode is not inherited.
#[async_trait]
pub trait Patching<R: GuardedRepo> {
    /// Converts a request body (`data`, `modified`, `acceptWarnings`, `associated`) without saving.
    ///
    /// Accepted associations come from the body's `associated` (an array of strings; otherwise or
    /// for an unregistered name, `BadRequest`). By default only the first level is accepted: usable
    /// associations of the parent, and those inside child or target rows are silently dropped.
    /// Nested ones are written as paths (`"orders.items"`).
    ///
    /// Order: body shape (`BadRequest`), permission (Add or Edit), `before_marshal`, splitting out
    /// associations, first-stage validation, `after_marshal`. First-stage errors are `Invalid`.
    async fn patch(&self, id: Option<Key<R>>, body: &Json) -> Result<Patch<R>, PatchError> {
        self.patch_with(id, body, PatchOptions::default()).await
    }

    /// Like `patch`, with the association names to accept. An unregistered name is a code error,
    /// `Repo(Db)`.
    async fn patch_with(
        &self,
        id: Option<Key<R>>,
        body: &Json,
        options: PatchOptions,
    ) -> Result<Patch<R>, PatchError>;

    /// Saves a converted body in one transaction, checking the optimistic lock and Warnings.
    /// On a conflict it returns `Conflict` with the current row.
    async fn save_patch(
        &self,
        patch: Patch<R>,
    ) -> Result<<R::Entity as EntityTrait>::Model, PatchError>;
}

#[async_trait]
impl<'c, R: GuardedRepo> Patching<R> for RepoHandle<'c, R> {
    async fn patch_with(
        &self,
        id: Option<Key<R>>,
        body: &Json,
        options: PatchOptions,
    ) -> Result<Patch<R>, PatchError> {
        let ctx = self.context();
        let marshaller = marshaller_of::<R>(ctx)
            .ok_or_else(|| RepoError::NotRegistered(R::repo_name().to_string()))?;
        let meta = marshaller.meta(ctx)?;
        let read = marshaller
            .read_body(ctx, meta, id.as_ref(), body, &options)
            .await?;
        marshaller.marshal(ctx, meta, id.as_ref(), read).await
    }

    async fn save_patch(
        &self,
        patch: Patch<R>,
    ) -> Result<<R::Entity as EntityTrait>::Model, PatchError> {
        let ctx = self.context();
        let marshaller = marshaller_of::<R>(ctx)
            .ok_or_else(|| RepoError::NotRegistered(R::repo_name().to_string()))?;
        let meta = marshaller.meta(ctx)?;
        marshaller.persist(ctx, meta, patch).await
    }
}
