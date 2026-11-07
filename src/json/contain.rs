//! The tree of associations to include (`ContainTree`) and JSON conversion of typed `Loaded` rows.

use crate::repo::GuardedRepo;
use std::sync::Arc;

use sea_orm::DbErr;
use serde_json::{Map, Value as Json};

use super::JsonFor;
use crate::association::Association;
use crate::context::Context;
use crate::registry::EntityMeta;
use crate::repo::{Loaded, NameTree, RepoEntity, RepoError, load_related, typed_decl_of};

/// A tree of associations to include; one node per name on each level, in first-seen order.
#[derive(Clone, Debug, Default)]
pub struct ContainTree {
    pub nodes: Vec<ContainNode>,
}

/// One association in the tree and the associations to include from its target rows.
#[derive(Clone, Debug)]
pub struct ContainNode {
    pub relation: Arc<dyn Association>,
    pub children: ContainTree,
}

impl ContainTree {
    /// Returns the tree as a tree of association names, for a typed contain.
    pub fn names(&self) -> NameTree {
        NameTree {
            nodes: self
                .nodes
                .iter()
                .map(|n| (n.relation.name().to_string(), n.children.names()))
                .collect(),
        }
    }

    /// Returns the top-level associations (the `relations` of the schema), without descending.
    ///
    /// For a tree built from `[customer, items, items.product]` on orders this is `[customer,
    /// items]`; `product` belongs to order items, not orders.
    pub fn top_relations(&self) -> Vec<Arc<dyn Association>> {
        self.nodes.iter().map(|n| n.relation.clone()).collect()
    }

    /// Adds a path (associations in level order) to the tree, sharing nodes with the same name.
    ///
    /// A missing node is appended with empty children and the rest of the path is added below it,
    /// so `items` and `items.product` together load the items once, and node order is first-seen
    /// order. Each step shortens the path, so it stops even if the associations form a cycle.
    pub fn insert(&mut self, path: &[Arc<dyn Association>]) {
        let Some((first, rest)) = path.split_first() else {
            return;
        };
        let index = match self
            .nodes
            .iter()
            .position(|n| n.relation.name() == first.name())
        {
            Some(i) => i,
            None => {
                self.nodes.push(ContainNode {
                    relation: first.clone(),
                    children: ContainTree::default(),
                });
                self.nodes.len() - 1
            }
        };
        self.nodes[index].children.insert(rest);
    }
}

impl<R, E> JsonFor<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    pub(crate) async fn attach_loaded(
        ctx: &Context,
        rows: &[&Loaded<E::Model>],
        datas: &mut [Map<String, Json>],
        tree: &ContainTree,
    ) -> Result<(), RepoError> {
        for node in &tree.nodes {
            let name = node.relation.name();
            let missing = || {
                RepoError::from(DbErr::Custom(format!(
                    "association {name} was not loaded (the typed contain does not match the tree)"
                )))
            };
            let target = typed_decl_of::<R>(node.relation.as_ref())
                .ok_or_else(|| {
                    RepoError::from(DbErr::Custom(format!(
                        "cannot look up the typed declaration of association {name}"
                    )))
                })?
                .target_json(ctx)?;
            let values = rows
                .iter()
                .map(|r| r.related_raw(name).ok_or_else(missing))
                .collect::<Result<Vec<_>, _>>()?;
            let jsons = target
                .related_json(ctx, node.relation.as_ref(), &node.children, &values)
                .await?;
            for (data, json) in datas.iter_mut().zip(jsons) {
                data.insert(name.to_string(), json);
            }
        }
        Ok(())
    }
}

pub(crate) async fn loaded_to_json<R: GuardedRepo>(
    ctx: &Context,
    rows: &[Loaded<<R::Entity as sea_orm::EntityTrait>::Model>],
) -> Result<Vec<Json>, RepoError> {
    JsonFor::<R>::of(ctx)?.loaded_data(ctx, rows).await
}

/// Follows a path level by level and returns the associations found, and whether every level was
/// usable (an empty level is not).
///
/// From the second level, the target entity's associations are looked up by name and checked with
/// `Association::usable`.
pub async fn trace(
    ctx: &Context,
    meta: &EntityMeta,
    path: &str,
) -> Result<(Vec<Arc<dyn Association>>, bool), RepoError> {
    let mut found: Vec<Arc<dyn Association>> = Vec::new();
    for segment in path.split('.') {
        let relations = match found.last() {
            None => Some(&meta.relations),
            Some(previous) => ctx
                .registry()
                .meta(previous.target())
                .map(|target| &target.relations),
        };
        let next = match relations.and_then(|rs| rs.iter().find(|r| r.name() == segment)) {
            Some(r) if r.usable(ctx).await? => Some(r.clone()),
            _ => None,
        };
        match next {
            Some(relation) => found.push(relation),
            None => return Ok((found, false)),
        }
    }
    Ok((found, true))
}

/// Builds a tree from each `default_contain` path, following it until a level is unusable.
pub async fn default_contain_tree(
    ctx: &Context,
    meta: &EntityMeta,
) -> Result<ContainTree, RepoError> {
    let mut tree = ContainTree::default();
    for path in &meta.default_contain {
        let (found, _) = trace(ctx, meta, path).await?;
        tree.insert(&found);
    }
    Ok(tree)
}

impl<R, E> JsonFor<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    /// Loads the associations of `tree` for `rows` with a typed contain and puts them in `datas`
    /// (same length and order as `rows`) as JSON.
    pub async fn attach_typed(
        ctx: &Context,
        rows: Vec<E::Model>,
        datas: &mut [Map<String, Json>],
        tree: &ContainTree,
    ) -> Result<(), RepoError> {
        if tree.nodes.is_empty() {
            return Ok(());
        }
        let loaded = load_related::<R>(ctx, &tree.names(), rows).await?;
        let refs: Vec<&Loaded<E::Model>> = loaded.iter().collect();
        Self::attach_loaded(ctx, &refs, datas, tree).await
    }
}
