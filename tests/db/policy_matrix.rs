//! Every public read and write operation against every way a Policy can refuse it, in one table.
//!
//! Each row names an operation, what is denied and the expected outcome. One test runs all rows
//! (checked, and `unchecked()` with everything denied) and reports every row that differs.

use std::sync::atomic::{AtomicUsize, Ordering};

use cakedome::Context;
use cakedome::association::{HasManyOf, ManyToManyOf};
use cakedome::patch::{PatchError, Patching};
use cakedome::policy::{Action, ColumnAccess, EntityPolicy, TablePolicy};
use cakedome::repo::{DefaultRepo, Loaded, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, IntoActiveModel};
use serde_json::json;

use crate::common::registry;
use crate::entities::{master_tags, order_items, order_tag_links, orders};
use crate::support::{context, db, identity, insert_tag, past};

type Tags = DefaultRepo<master_tags::Entity>;

/// What a case refuses. `All` refuses everything, for the `unchecked()` rows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Deny {
    None,
    Can(Action),
    Scope(Action),
    ColumnWrite,
    ColumnRead,
    EntityAdd,
    EntityEdit,
    EntityDelete,
    ChildCan(Action),
    JoinCan(Action),
    All,
}

impl Deny {
    fn can(self, own: Deny, action: Action) -> bool {
        !(self == Deny::All || self == own_with(own, action))
    }
}

fn own_with(kind: Deny, action: Action) -> Deny {
    match kind {
        Deny::Can(_) => Deny::Can(action),
        Deny::ChildCan(_) => Deny::ChildCan(action),
        Deny::JoinCan(_) => Deny::JoinCan(action),
        other => other,
    }
}

struct OrdersRepo {
    deny: Deny,
    title: String,
}

impl OrdersRepo {
    const ITEMS: HasManyOf<Self, ItemsRepo> = HasManyOf::new("items", order_items::Column::OrderId);
    const TAGS: ManyToManyOf<Self, LinksRepo, Tags> = ManyToManyOf::new(
        "tags",
        order_tag_links::Column::OrderId,
        order_tag_links::Column::TagId,
    );
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(
        e: cakedome::registry::RepoConfig<'_, orders::Entity>,
    ) -> cakedome::registry::RepoConfig<'_, orders::Entity> {
        e.association(Self::ITEMS)
            .association(Self::TAGS)
            .default_contain(["items"])
    }
}

#[async_trait]
impl TablePolicy for OrdersRepo {
    async fn can(&self, _ctx: &Context, action: Action) -> Result<bool, RepoError> {
        Ok(self.deny.can(Deny::Can(action), action))
    }

    async fn scope(&self, _ctx: &Context, action: Action) -> Result<Condition, RepoError> {
        let hidden = self.deny == Deny::All || self.deny == Deny::Scope(action);
        Ok(if hidden {
            Condition::all().add(orders::Column::Title.ne(self.title.as_str()))
        } else {
            Condition::all()
        })
    }

    async fn columns(&self, _ctx: &Context) -> Result<ColumnAccess<orders::Entity>, RepoError> {
        Ok(match self.deny {
            Deny::ColumnWrite => ColumnAccess::all().read_only(orders::Column::Title),
            Deny::ColumnRead | Deny::All => ColumnAccess::all().hide(orders::Column::Title),
            _ => ColumnAccess::all(),
        })
    }
}

#[async_trait]
impl EntityPolicy for OrdersRepo {
    async fn can_add(
        &self,
        _ctx: &Context,
        _after: &orders::ActiveModel,
    ) -> Result<bool, RepoError> {
        Ok(!matches!(self.deny, Deny::EntityAdd | Deny::All))
    }

    async fn can_edit(
        &self,
        _ctx: &Context,
        _before: &orders::Model,
        _after: &orders::ActiveModel,
    ) -> Result<bool, RepoError> {
        Ok(!matches!(self.deny, Deny::EntityEdit | Deny::All))
    }

    async fn can_delete(&self, _ctx: &Context, _model: &orders::Model) -> Result<bool, RepoError> {
        Ok(!matches!(self.deny, Deny::EntityDelete | Deny::All))
    }
}

struct ItemsRepo(Deny);

impl Repo for ItemsRepo {
    type Entity = order_items::Entity;
}

#[async_trait]
impl TablePolicy for ItemsRepo {
    async fn can(&self, _ctx: &Context, action: Action) -> Result<bool, RepoError> {
        Ok(self.0.can(Deny::ChildCan(action), action))
    }
}

impl EntityPolicy for ItemsRepo {}

struct LinksRepo(Deny);

impl Repo for LinksRepo {
    type Entity = order_tag_links::Entity;
}

#[async_trait]
impl TablePolicy for LinksRepo {
    async fn can(&self, _ctx: &Context, action: Action) -> Result<bool, RepoError> {
        Ok(self.0.can(Deny::JoinCan(action), action))
    }
}

impl EntityPolicy for LinksRepo {}

#[derive(Clone, Copy, Debug)]
enum Op {
    FindAll,
    FindCount,
    Get,
    GetDetail,
    Contain,
    ContainDefault,
    Load,
    ToJsonOne,
    SaveAdd,
    SaveEdit,
    Insert,
    SaveIfUnmodified,
    Touch,
    Delete,
    Purge,
    SaveWithChildren,
    LinkItems,
    UnlinkItems,
    ReplaceItems,
    LinkTags,
    UnlinkTags,
    ReplaceLinks,
    PatchAdd,
    PatchEdit,
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Ok,
    Empty,
    Forbidden,
    NotFound,
    /// The association was not loaded (a read that drops an unusable association).
    Dropped,
    JsonWithTitle,
    JsonWithoutTitle,
    Invalid(String),
    Other(String),
}

fn failure(e: RepoError) -> Outcome {
    match e {
        RepoError::Forbidden => Outcome::Forbidden,
        RepoError::NotFound => Outcome::NotFound,
        RepoError::Validation(errors) => invalid(&errors),
        other => Outcome::Other(format!("{other:?}")),
    }
}

fn invalid(errors: &cakedome::marshal::FieldErrors) -> Outcome {
    let codes: Vec<String> = errors
        .0
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |e| format!("{k}:{}", e.code)))
        .collect();
    Outcome::Invalid(codes.join(","))
}

fn patch_failure(e: PatchError) -> Outcome {
    match e {
        PatchError::Repo(e) => failure(e),
        PatchError::Invalid(errors) => invalid(&errors),
        other => Outcome::Other(format!("{other:?}")),
    }
}

fn done<T>(r: Result<T, RepoError>) -> Outcome {
    r.map_or_else(failure, |_| Outcome::Ok)
}

fn patched<T>(r: Result<T, PatchError>) -> Outcome {
    r.map_or_else(patch_failure, |_| Outcome::Ok)
}

fn rows_outcome(n: usize) -> Outcome {
    if n == 0 { Outcome::Empty } else { Outcome::Ok }
}

/// Rows of one case. Titles are unique per case so that `scope` can hide exactly this order.
struct Seed {
    title: String,
    order: orders::Model,
    item: order_items::Model,
    tag_a: i64,
    tag_b: i64,
}

static CASES: AtomicUsize = AtomicUsize::new(0);

async fn seed(db: &DatabaseConnection) -> Seed {
    let n = CASES.fetch_add(1, Ordering::SeqCst);
    let title = format!("case{n}");
    let order = orders::ActiveModel {
        customer_id: Set(1),
        title: Set(title.clone()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    let item = order_items::ActiveModel {
        order_id: Set(order.id),
        name: Set("item".to_string()),
        seq: Set(0),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    let tag_a = insert_tag(db, &format!("{title}a"), true).await.id;
    let tag_b = insert_tag(db, &format!("{title}b"), true).await.id;
    order_tag_links::ActiveModel {
        order_id: Set(order.id),
        tag_id: Set(tag_a),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    Seed {
        title,
        order,
        item,
        tag_a,
        tag_b,
    }
}

fn new_item() -> order_items::ActiveModel {
    order_items::ActiveModel {
        name: Set("new item".to_string()),
        ..Default::default()
    }
}

async fn run(db: &DatabaseConnection, op: Op, deny: Deny, unchecked: bool) -> Outcome {
    let s = seed(db).await;
    let mut r = registry();
    r.add(OrdersRepo {
        deny,
        title: s.title.clone(),
    });
    r.add(ItemsRepo(deny));
    r.add(LinksRepo(deny));
    r.add(Tags::new());
    r.verify();
    let ctx = context(
        db,
        identity(&[
            "entity.orders.*",
            "entity.order_items.*",
            "entity.order_tag_links.*",
            "entity.master_tags.*",
        ]),
        r,
    );
    let handle = ctx.repo::<OrdersRepo>();
    let repo = if unchecked {
        handle.unchecked()
    } else {
        handle
    };
    let id = s.order.id;
    let title = orders::Column::Title.eq(s.title.as_str());
    let add_am = || orders::ActiveModel {
        customer_id: Set(1),
        title: Set("added".to_string()),
        ..Default::default()
    };
    let edit_am = || orders::ActiveModel {
        id: Unchanged(id),
        title: Set("edited".to_string()),
        ..Default::default()
    };

    match op {
        Op::FindAll => repo
            .find()
            .filter(title)
            .all()
            .await
            .map_or_else(failure, |rows| rows_outcome(rows.len())),
        Op::FindCount => repo
            .find()
            .filter(title)
            .count()
            .await
            .map_or_else(failure, |n| rows_outcome(n as usize)),
        Op::Get => match repo.get(id).await {
            Ok(model) if model.title == s.title => Outcome::Ok,
            Ok(model) => Outcome::Other(format!("{model:?}")),
            Err(e) => failure(e),
        },
        Op::GetDetail => done(repo.get_detail(id).await),
        Op::Contain => match repo
            .find()
            .filter(title)
            .contain(OrdersRepo::ITEMS)
            .all()
            .await
        {
            Err(e) => failure(e),
            Ok(rows) if rows.is_empty() => Outcome::Empty,
            Ok(rows) => match rows[0].related(OrdersRepo::ITEMS) {
                Ok(items) if items.len() == 1 => Outcome::Ok,
                Ok(items) => Outcome::Other(format!("{} items", items.len())),
                Err(_) => Outcome::Dropped,
            },
        },
        Op::ContainDefault => match repo.find().filter(title).contain_default().all().await {
            Err(e) => failure(e),
            Ok(rows) if rows.is_empty() => Outcome::Empty,
            Ok(rows) => match rows[0].related(OrdersRepo::ITEMS) {
                Ok(items) if items.len() == 1 => Outcome::Ok,
                Ok(items) => Outcome::Other(format!("{} items", items.len())),
                Err(_) => Outcome::Dropped,
            },
        },
        Op::Load => match repo
            .load(std::slice::from_ref(&s.order), OrdersRepo::ITEMS)
            .await
        {
            Err(e) => failure(e),
            Ok(map) => rows_outcome(map.len()),
        },
        Op::ToJsonOne => match repo.to_json_one(&Loaded::from(s.order.clone())).await {
            Err(e) => failure(e),
            Ok(json) if json.get("title").is_some() => Outcome::JsonWithTitle,
            Ok(_) => Outcome::JsonWithoutTitle,
        },
        Op::SaveAdd => done(repo.save(add_am()).await),
        Op::Insert => done(repo.insert(add_am()).await),
        Op::SaveEdit => done(repo.save(edit_am()).await),
        Op::SaveIfUnmodified => done(repo.save_if_unmodified(edit_am(), s.order.modified).await),
        Op::Touch => done(repo.touch(id).await),
        Op::Delete => done(repo.delete(id).await),
        Op::Purge => done(repo.purge(id).await),
        Op::SaveWithChildren => done(
            repo.save_with(s.order.clone().into_active_model())
                .replace(OrdersRepo::ITEMS, vec![new_item()])
                .save()
                .await,
        ),
        Op::LinkItems => done(
            repo.link(OrdersRepo::ITEMS, &s.order, vec![new_item()])
                .await,
        ),
        Op::UnlinkItems => done(
            repo.unlink(OrdersRepo::ITEMS, &s.order, vec![s.item.id])
                .await,
        ),
        Op::ReplaceItems => done(
            repo.replace(OrdersRepo::ITEMS, &s.order, vec![new_item()])
                .await,
        ),
        Op::LinkTags => done(repo.link(OrdersRepo::TAGS, &s.order, vec![s.tag_b]).await),
        Op::UnlinkTags => done(repo.unlink(OrdersRepo::TAGS, &s.order, vec![s.tag_a]).await),
        Op::ReplaceLinks => done(
            repo.replace_links(OrdersRepo::TAGS, &s.order, vec![s.tag_b])
                .await,
        ),
        Op::PatchAdd => {
            let body = json!({"data": {"customer_id": 1, "title": "patched"}});
            match repo.patch(None, &body).await {
                Ok(patch) => patched(repo.save_patch(patch).await),
                Err(e) => patch_failure(e),
            }
        }
        Op::PatchEdit => {
            let modified = s
                .order
                .modified
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
            let body = json!({"data": {"title": "patched"}, "modified": modified});
            match repo.patch(Some(id), &body).await {
                Ok(patch) => patched(repo.save_patch(patch).await),
                Err(e) => patch_failure(e),
            }
        }
    }
}

fn rows() -> Vec<(Op, Deny, Outcome)> {
    use Action::{Add, Edit, View};
    use Deny::{
        Can, ChildCan, ColumnRead, ColumnWrite, EntityAdd, EntityDelete, EntityEdit, JoinCan, Scope,
    };
    use Op::*;
    let mut t: Vec<(Op, Deny, Outcome)> = Vec::new();
    let mut row = |op, deny, outcome| t.push((op, deny, outcome));

    for op in [FindAll, FindCount, Get, GetDetail, Contain, ContainDefault] {
        row(op, Deny::None, Outcome::Ok);
        row(op, Can(View), Outcome::Forbidden);
        row(op, Can(Edit), Outcome::Ok);
        let hidden = match op {
            Get | GetDetail => Outcome::NotFound,
            _ => Outcome::Empty,
        };
        row(op, Scope(View), hidden);
        row(op, Scope(Edit), Outcome::Ok);
    }
    row(Get, ColumnRead, Outcome::Ok);
    row(Contain, ChildCan(View), Outcome::Forbidden);
    row(ContainDefault, ChildCan(View), Outcome::Dropped);
    row(GetDetail, ChildCan(View), Outcome::Ok);
    row(Load, Deny::None, Outcome::Ok);
    row(Load, ChildCan(View), Outcome::Forbidden);
    row(ToJsonOne, Deny::None, Outcome::JsonWithTitle);
    row(ToJsonOne, ColumnWrite, Outcome::JsonWithTitle);
    row(ToJsonOne, ColumnRead, Outcome::JsonWithoutTitle);

    for op in [SaveAdd, Insert, PatchAdd] {
        row(op, Deny::None, Outcome::Ok);
        row(op, Can(Add), Outcome::Forbidden);
        row(op, Can(Edit), Outcome::Ok);
        row(op, Scope(Add), Outcome::Ok);
        row(op, EntityAdd, Outcome::Forbidden);
        row(op, EntityEdit, Outcome::Ok);
    }
    for op in [SaveAdd, Insert] {
        row(op, ColumnWrite, Outcome::Forbidden);
        row(op, ColumnRead, Outcome::Forbidden);
    }
    row(
        PatchAdd,
        ColumnWrite,
        Outcome::Invalid("title:read_only".into()),
    );
    row(
        PatchAdd,
        ColumnRead,
        Outcome::Invalid("title:unknown".into()),
    );

    for op in [SaveEdit, SaveIfUnmodified, PatchEdit] {
        row(op, Deny::None, Outcome::Ok);
        row(op, Can(Edit), Outcome::Forbidden);
        row(op, Can(Add), Outcome::Ok);
        row(op, Scope(Edit), Outcome::NotFound);
        row(op, Scope(View), Outcome::Ok);
        row(op, EntityEdit, Outcome::Forbidden);
        row(op, EntityAdd, Outcome::Ok);
    }
    for op in [SaveEdit, SaveIfUnmodified] {
        row(op, ColumnWrite, Outcome::Forbidden);
        row(op, ColumnRead, Outcome::Forbidden);
    }
    row(
        PatchEdit,
        ColumnWrite,
        Outcome::Invalid("title:read_only".into()),
    );
    row(
        PatchEdit,
        ColumnRead,
        Outcome::Invalid("title:unknown".into()),
    );

    row(Touch, Deny::None, Outcome::Ok);
    row(Touch, Can(Edit), Outcome::Forbidden);
    row(Touch, Scope(Edit), Outcome::NotFound);
    row(Touch, ColumnWrite, Outcome::Ok);
    row(Touch, ColumnRead, Outcome::Ok);
    row(Touch, EntityEdit, Outcome::Ok);

    for op in [Delete, Purge] {
        row(op, Deny::None, Outcome::Ok);
        row(op, Can(Action::Delete), Outcome::Forbidden);
        row(op, Can(Edit), Outcome::Ok);
        row(op, Scope(Action::Delete), Outcome::NotFound);
        row(op, Scope(Edit), Outcome::Ok);
        row(op, EntityDelete, Outcome::Forbidden);
        row(op, EntityEdit, Outcome::Ok);
    }

    for op in [
        SaveWithChildren,
        LinkItems,
        UnlinkItems,
        ReplaceItems,
        LinkTags,
        UnlinkTags,
        ReplaceLinks,
    ] {
        row(op, Deny::None, Outcome::Ok);
        row(op, Can(Edit), Outcome::Forbidden);
        row(op, Can(Action::Delete), Outcome::Ok);
        row(op, Scope(Edit), Outcome::NotFound);
        row(op, EntityEdit, Outcome::Forbidden);
    }
    row(SaveWithChildren, ChildCan(Add), Outcome::Forbidden);
    row(
        SaveWithChildren,
        ChildCan(Action::Delete),
        Outcome::Forbidden,
    );
    row(LinkItems, ChildCan(Add), Outcome::Forbidden);
    row(UnlinkItems, ChildCan(Action::Delete), Outcome::Forbidden);
    row(ReplaceItems, ChildCan(Add), Outcome::Forbidden);
    row(ReplaceItems, ChildCan(Action::Delete), Outcome::Forbidden);
    row(LinkTags, JoinCan(Add), Outcome::Forbidden);
    row(UnlinkTags, JoinCan(Action::Delete), Outcome::Forbidden);
    row(ReplaceLinks, JoinCan(Add), Outcome::Forbidden);
    row(ReplaceLinks, JoinCan(Action::Delete), Outcome::Forbidden);
    t
}

const ALL_OPS: [Op; 24] = [
    Op::FindAll,
    Op::FindCount,
    Op::Get,
    Op::GetDetail,
    Op::Contain,
    Op::ContainDefault,
    Op::Load,
    Op::ToJsonOne,
    Op::SaveAdd,
    Op::SaveEdit,
    Op::Insert,
    Op::SaveIfUnmodified,
    Op::Touch,
    Op::Delete,
    Op::Purge,
    Op::SaveWithChildren,
    Op::LinkItems,
    Op::UnlinkItems,
    Op::ReplaceItems,
    Op::LinkTags,
    Op::UnlinkTags,
    Op::ReplaceLinks,
    Op::PatchAdd,
    Op::PatchEdit,
];

#[tokio::test]
async fn each_policy_part_refuses_the_operations_it_covers() {
    let db = db().await;
    let mut wrong = Vec::new();

    for (op, deny, expected) in rows() {
        let actual = run(&db, op, deny, false).await;
        if actual != expected {
            wrong.push(format!(
                "{op:?} / {deny:?}: expected {expected:?}, got {actual:?}"
            ));
        }
    }

    // Everything is refused, yet an unchecked handle goes through; only the JSON columns still
    // follow the viewer's permission.
    for op in ALL_OPS {
        let expected = match op {
            Op::ToJsonOne => Outcome::JsonWithoutTitle,
            // `patch` and `save_patch` do not inherit `unchecked()`.
            Op::PatchAdd | Op::PatchEdit => Outcome::Forbidden,
            _ => Outcome::Ok,
        };
        let actual = run(&db, op, Deny::All, true).await;
        if actual != expected {
            wrong.push(format!(
                "{op:?} / unchecked: expected {expected:?}, got {actual:?}"
            ));
        }
    }

    assert!(
        wrong.is_empty(),
        "{} rows differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
