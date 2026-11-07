//! `RepoHandle::link`, `unlink`, `replace` and `replace_links`: connect or detach associated rows
//! without saving the parent's own columns. They run the same way as `save_with`.

use sea_orm::IntoActiveModel;

use super::{GuardedRepo, RepoEntity, RepoError, RepoHandle, SaveWith};
use crate::association::{HasManyOf, ManyToManyOf};
use crate::key::{Key, RowKey};

/// An association declaration (hasMany or many-to-many) that `link` and `unlink` accept.
pub trait Linkable<R: GuardedRepo>: Sized {
    /// The row passed to `link`: the child ActiveModel for hasMany, the target key for
    /// many-to-many.
    type LinkRow: Send;
    /// The row passed to `unlink`: the child key for hasMany, the target key for many-to-many.
    type UnlinkRow: Send;

    #[doc(hidden)]
    fn link_into<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::LinkRow>) -> SaveWith<'c, R>;

    #[doc(hidden)]
    fn unlink_from<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::UnlinkRow>) -> SaveWith<'c, R>;
}

impl<R: GuardedRepo, C: GuardedRepo, K: RowKey> Linkable<R> for HasManyOf<R, C, K>
where
    R::Entity: RepoEntity,
{
    type LinkRow = <C::Entity as sea_orm::EntityTrait>::ActiveModel;
    type UnlinkRow = Key<C>;

    fn link_into<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::LinkRow>) -> SaveWith<'c, R> {
        with.link_children(self, rows)
    }

    fn unlink_from<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::UnlinkRow>) -> SaveWith<'c, R> {
        with.unlink_children(self, rows)
    }
}

impl<R: GuardedRepo, J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey> Linkable<R>
    for ManyToManyOf<R, J, T, K, TK>
where
    R::Entity: RepoEntity,
{
    type LinkRow = Key<T>;
    type UnlinkRow = Key<T>;

    fn link_into<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::LinkRow>) -> SaveWith<'c, R> {
        with.link_targets(self, rows)
    }

    fn unlink_from<'c>(self, with: SaveWith<'c, R>, rows: Vec<Self::UnlinkRow>) -> SaveWith<'c, R> {
        with.unlink_targets(self, rows)
    }
}

impl<'c, R, E> RepoHandle<'c, R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    /// Connects rows to `parent` through `association`.
    ///
    /// - hasMany: sets the child's foreign key to the parent's `bindingKey` value and saves it
    ///   through the child's Repo (insert without a primary key, update with one, so a child of
    ///   another parent is moved). Existing children are kept.
    /// - many-to-many: adds join rows only for targets not yet linked, after the current last
    ///   `seq`. New targets are checked against `selectable` and the association's condition; a
    ///   failure adds `invalid_ref` under `name.index` and writes nothing.
    ///
    /// Checks the parent's `can(Edit)` (unless `unchecked`) and runs in one transaction (a
    /// savepoint inside an existing one). Advances the parent's `modified` when children or links
    /// change. A null `bindingKey` value adds `invalid` under `name`.
    pub async fn link<D: Linkable<R>>(
        &self,
        association: D,
        parent: &E::Model,
        rows: Vec<D::LinkRow>,
    ) -> Result<(), RepoError> {
        let with = self.save_with(parent.clone().into_active_model());
        association.link_into(with, rows).save().await?;
        Ok(())
    }

    /// Detaches rows from `parent` through `association`.
    ///
    /// - hasMany: detaches the given children whose foreign key equals the parent's value and that
    ///   match the association's condition, the same way `Replace` drops children (set the key to
    ///   NULL if it is nullable and not `dependent`, otherwise delete). Other keys are ignored.
    /// - many-to-many: deletes the links to the given targets; the target rows stay.
    pub async fn unlink<D: Linkable<R>>(
        &self,
        association: D,
        parent: &E::Model,
        rows: Vec<D::UnlinkRow>,
    ) -> Result<(), RepoError> {
        let with = self.save_with(parent.clone().into_active_model());
        association.unlink_from(with, rows).save().await?;
        Ok(())
    }

    /// Replaces the parent's hasMany children with `rows`, whatever the declaration's save mode.
    ///
    /// Children not in `rows` get a NULL foreign key if it is nullable and not `dependent`, and are
    /// deleted otherwise.
    pub async fn replace<C: GuardedRepo, K: RowKey>(
        &self,
        association: HasManyOf<R, C, K>,
        parent: &E::Model,
        rows: Vec<<C::Entity as sea_orm::EntityTrait>::ActiveModel>,
    ) -> Result<(), RepoError> {
        self.save_with(parent.clone().into_active_model())
            .replace(association, rows)
            .save()
            .await?;
        Ok(())
    }

    /// Relinks the parent's many-to-many targets to exactly `target_ids`, in that order.
    pub async fn replace_links<J: GuardedRepo, T: GuardedRepo, K: RowKey, TK: RowKey>(
        &self,
        association: ManyToManyOf<R, J, T, K, TK>,
        parent: &E::Model,
        target_ids: Vec<Key<T>>,
    ) -> Result<(), RepoError> {
        self.save_with(parent.clone().into_active_model())
            .replace_links(association, target_ids)
            .save()
            .await?;
        Ok(())
    }
}
