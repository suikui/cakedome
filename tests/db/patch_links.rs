//! Many-to-many from a JSON body: target ids, `_joinData` values of the join table, and `Append`.

use std::sync::{Arc, Mutex};

use cakedome::Context;
use cakedome::association::ManyToManyOf;
use cakedome::key::RowId;
use cakedome::marshal::{FieldErrors, MarshalOptions, WriteMode};
use cakedome::patch::{PatchError, Patching};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig, SaveStrategy};
use cakedome::repo::{DefaultRepo, Repo, RepoError};
use cakedome::validate::ValidationInput;
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::ActiveValue::Set;
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{master_tags, orders, setting_order_tags};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Tags = DefaultRepo<master_tags::Entity>;
type Calls = Arc<Mutex<Vec<(&'static str, WriteMode, Option<RowId>)>>>;

// Records hook calls; a note of "ng" fails the second stage.
struct JoinRepo {
    calls: Calls,
}

#[async_trait]
impl Repo for JoinRepo {
    type Entity = setting_order_tags::Entity;

    async fn before_marshal(
        &self,
        _ctx: &Context,
        data: &mut serde_json::Map<String, Value>,
        options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        if data.get("note") == Some(&json!("boom")) {
            return Err(RepoError::Forbidden);
        }
        self.calls
            .lock()
            .unwrap()
            .push(("before", options.mode(), options.id().cloned()));
        Ok(())
    }

    async fn after_marshal(
        &self,
        _ctx: &Context,
        _am: &mut setting_order_tags::ActiveModel,
        _data: &serde_json::Map<String, Value>,
        _errors: &mut FieldErrors,
        options: &MarshalOptions,
    ) -> Result<(), RepoError> {
        self.calls
            .lock()
            .unwrap()
            .push(("after", options.mode(), options.id().cloned()));
        Ok(())
    }

    fn validate(
        &self,
        input: &ValidationInput<'_, setting_order_tags::Entity>,
        errors: &mut FieldErrors,
    ) {
        if matches!(&input.after.note, Set(Some(note)) if note == "ng") {
            errors.add("note", "invalid", "ng");
        }
    }
}

impl TablePolicy for JoinRepo {}
impl EntityPolicy for JoinRepo {}

struct OrdersRepo;

impl OrdersRepo {
    const TAGS: ManyToManyOf<Self, JoinRepo, Tags> = ManyToManyOf::new(
        "tags",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace);
    const VIEW_ONLY: ManyToManyOf<Self, JoinRepo, Tags> = ManyToManyOf::new(
        "view_only",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    );
    // Tags coded "blocked" are outside this association.
    const FILTERED: ManyToManyOf<Self, JoinRepo, Tags> = ManyToManyOf::new(
        "filtered",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Replace)
    .filter(|| Condition::all().add(master_tags::Column::Code.ne("blocked")));
    // The join row stores the target order's customer id, not the order id.
    const BY_CUSTOMER: ManyToManyOf<Self, JoinRepo, OrdersRepo, i64, i64> = ManyToManyOf::new(
        "by_customer",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .target_binding_key::<i64>(orders::Column::CustomerId)
    .save(SaveStrategy::Replace);
    const APPENDED: ManyToManyOf<Self, JoinRepo, Tags> = ManyToManyOf::new(
        "appended",
        setting_order_tags::Column::OrderId,
        setting_order_tags::Column::TagId,
    )
    .save(SaveStrategy::Append);
}

impl Repo for OrdersRepo {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::TAGS)
            .association(Self::VIEW_ONLY)
            .association(Self::FILTERED)
            .association(Self::BY_CUSTOMER)
            .association(Self::APPENDED)
    }
}

impl TablePolicy for OrdersRepo {}
impl EntityPolicy for OrdersRepo {}

const ALL: &[&str] = &[
    "entity.orders.*",
    "entity.setting_order_tags.*",
    "entity.master_tags.*",
];

fn context_for(db: &DatabaseConnection, patterns: &[&str]) -> (Context, Calls) {
    let calls = Calls::default();
    let mut r: Registry = registry();
    r.add(Tags::new());
    r.add(JoinRepo {
        calls: calls.clone(),
    });
    r.add(OrdersRepo);
    r.verify();
    (context(db, identity(patterns), r), calls)
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn edit(o: &orders::Model, data: Value) -> Value {
    json!({"data": data, "modified": ts(o.modified)})
}

async fn run(ctx: &Context, id: Option<i64>, body: &Value) -> Result<orders::Model, PatchError> {
    let repo = ctx.repo::<OrdersRepo>();
    let patch = repo.patch(id, body).await?;
    repo.save_patch(patch).await
}

fn invalid<T>(res: Result<T, PatchError>) -> Vec<(String, &'static str)> {
    match res {
        Err(PatchError::Invalid(errors)) => {
            let mut out: Vec<(String, &'static str)> = errors
                .0
                .iter()
                .flat_map(|(k, v)| v.iter().map(move |e| (k.clone(), e.code)))
                .collect();
            out.sort();
            out
        }
        other => panic!("expected Invalid: {:?}", other.err()),
    }
}

fn pairs(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    list.iter().map(|(k, c)| (k.to_string(), *c)).collect()
}

async fn insert_order(db: &DatabaseConnection) -> orders::Model {
    let c = insert_customer(db, "顧客", true).await;
    orders::ActiveModel {
        customer_id: Set(c.id),
        title: Set("注文".to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn insert_link(
    db: &DatabaseConnection,
    order_id: i64,
    tag_id: i64,
    seq: i32,
    note: Option<&str>,
) -> setting_order_tags::Model {
    setting_order_tags::ActiveModel {
        order_id: Set(order_id),
        tag_id: Set(tag_id),
        seq: Set(seq),
        note: Set(note.map(str::to_string)),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

async fn links_of(db: &DatabaseConnection, order_id: i64) -> Vec<setting_order_tags::Model> {
    setting_order_tags::Entity::find()
        .filter(setting_order_tags::Column::OrderId.eq(order_id))
        .order_by_asc(setting_order_tags::Column::Seq)
        .all(db)
        .await
        .unwrap()
}

async fn link_values(db: &DatabaseConnection, order_id: i64) -> Vec<(i64, i32, Option<String>)> {
    links_of(db, order_id)
        .await
        .into_iter()
        .map(|l| (l.tag_id, l.seq, l.note))
        .collect()
}

async fn modified_of(db: &DatabaseConnection, id: i64) -> DateTime<Utc> {
    orders::Entity::find_by_id(id)
        .one(db)
        .await
        .unwrap()
        .unwrap()
        .modified
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[tokio::test]
async fn adding_a_parent_numbers_the_tags_in_the_order_sent() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let saved = run(
        &ctx,
        None,
        &json!({"data": {"customer_id": c.id, "title": "注文", "tags": [b.id, a.id]}}),
    )
    .await
    .unwrap();

    assert_eq!(
        link_values(&db, saved.id).await,
        [(b.id, 1, None), (a.id, 2, None)]
    );
}

#[tokio::test]
async fn replace_adds_removes_and_reorders_tags() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    let t4 = insert_tag(&db, "t4", true).await;
    for (tag, seq) in [(&t1, 1), (&t2, 2), (&t3, 3)] {
        insert_link(&db, o.id, tag.id, seq, None).await;
    }
    let (ctx, _) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"tags": [t3.id, t4.id, t1.id]})),
    )
    .await
    .unwrap();

    assert_eq!(
        link_values(&db, o.id).await,
        [(t3.id, 1, None), (t4.id, 2, None), (t1.id, 3, None)],
        "the dropped pair is deleted"
    );
    assert!(modified_of(&db, o.id).await > o.modified);
}

#[tokio::test]
async fn unchanged_tags_write_nothing() {
    let db = db().await;
    let o = insert_order(&db).await;
    let off = insert_tag(&db, "off", false).await;
    let on = insert_tag(&db, "on", true).await;
    let first = insert_link(&db, o.id, off.id, 1, None).await;
    insert_link(&db, o.id, on.id, 2, None).await;
    let (ctx, calls) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"tags": [off.id, on.id]})),
    )
    .await
    .unwrap();

    assert_eq!(modified_of(&db, o.id).await, o.modified);
    assert_eq!(links_of(&db, o.id).await[0].id, first.id);
    assert_eq!(links_of(&db, o.id).await.len(), 2);
    assert!(
        calls.lock().unwrap().is_empty(),
        "no join hooks without _joinData"
    );
}

#[tokio::test]
async fn invalid_tags_are_rejected() {
    let db = db().await;
    let o = insert_order(&db).await;
    let on = insert_tag(&db, "on", true).await;
    let off = insert_tag(&db, "off", false).await;
    let (ctx, _) = context_for(&db, ALL);

    let repeated = run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"title": 5, "tags": [on.id, on.id]})),
    )
    .await;
    let missing = run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"tags": [on.id, off.id, 9_000_000_000_i64]})),
    )
    .await;

    assert_eq!(
        invalid(repeated),
        pairs(&[("tags.1", "duplicate"), ("title", "type")])
    );
    assert_eq!(
        invalid(missing),
        pairs(&[("tags.1", "invalid_ref"), ("tags.2", "invalid_ref")])
    );
    assert!(links_of(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn malformed_tags_and_join_data_are_bad_requests() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t = insert_tag(&db, "t", true).await;
    let (ctx, _) = context_for(&db, ALL);

    for tags in [
        json!(1),
        json!(["1"]),
        json!([{"code": "x"}]),
        json!([1.5]),
        json!([{"_joinData": {"note": "x"}}]),
        json!([{"id": "x"}]),
        json!([{"id": t.id, "_joinData": "x"}]),
        json!([{"id": t.id, "_joinData": [1]}]),
    ] {
        let res = run(&ctx, Some(o.id), &edit(&o, json!({"tags": tags.clone()}))).await;
        assert!(
            matches!(res, Err(PatchError::BadRequest(_))),
            "{tags}: {res:?}"
        );
    }
}

#[tokio::test]
async fn changing_tags_requires_the_join_table_permissions() {
    let db = db().await;
    let o = insert_order(&db).await;
    let a = insert_tag(&db, "a", true).await;
    let b = insert_tag(&db, "b", true).await;
    insert_link(&db, o.id, a.id, 1, None).await;
    let (ctx, _) = context_for(
        &db,
        &[
            "entity.orders.*",
            "entity.setting_order_tags.view",
            "entity.master_tags.view",
        ],
    );

    let add = run(&ctx, Some(o.id), &edit(&o, json!({"tags": [a.id, b.id]}))).await;
    let clear = run(&ctx, Some(o.id), &edit(&o, json!({"tags": []}))).await;

    assert!(
        matches!(add, Err(PatchError::Repo(RepoError::Forbidden))),
        "{add:?}"
    );
    assert!(
        matches!(clear, Err(PatchError::Repo(RepoError::Forbidden))),
        "{clear:?}"
    );
    assert_eq!(links_of(&db, o.id).await.len(), 1);
}

#[tokio::test]
async fn replace_writes_join_data_into_new_and_kept_links() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    let old = insert_link(&db, o.id, t1.id, 1, Some("前")).await;
    let (ctx, _) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [
                {"id": t2.id, "_joinData": {"note": "新しい"}},
                {"id": t1.id, "_joinData": {"note": "直した"}},
                t3.id,
            ]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        link_values(&db, o.id).await,
        [
            (t2.id, 1, s("新しい")),
            (t1.id, 2, s("直した")),
            (t3.id, 3, None),
        ]
    );
    assert_eq!(
        links_of(&db, o.id).await[1].id,
        old.id,
        "a kept pair is updated in place"
    );
}

#[tokio::test]
async fn a_kept_link_without_join_data_is_not_touched() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    insert_link(&db, o.id, t1.id, 1, Some("そのまま")).await;
    let (ctx, _) = context_for(&db, ALL);

    run(&ctx, Some(o.id), &edit(&o, json!({"tags": [t1.id]})))
        .await
        .unwrap();
    run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"tags": [{"id": t1.id}]})),
    )
    .await
    .unwrap();

    assert_eq!(link_values(&db, o.id).await, [(t1.id, 1, s("そのまま"))]);
    assert_eq!(modified_of(&db, o.id).await, o.modified);
}

#[tokio::test]
async fn the_same_join_data_writes_nothing_and_a_changed_one_moves_the_parent() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    insert_link(&db, o.id, t1.id, 1, Some("同じ")).await;
    let (ctx, _) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [{"id": t1.id, "_joinData": {"note": "同じ"}}]}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(modified_of(&db, o.id).await, o.modified);

    run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [{"id": t1.id, "_joinData": {"note": "後"}}]}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(link_values(&db, o.id).await, [(t1.id, 1, s("後"))]);
    assert!(modified_of(&db, o.id).await > o.modified);
}

#[tokio::test]
async fn a_new_parent_writes_join_data_with_its_links() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let saved = run(
        &ctx,
        None,
        &json!({"data": {"customer_id": c.id, "title": "注文",
                         "tags": [{"id": t1.id, "_joinData": {"note": "最初"}}]}}),
    )
    .await
    .unwrap();

    assert_eq!(link_values(&db, saved.id).await, [(t1.id, 1, s("最初"))]);
}

#[tokio::test]
async fn join_data_errors_are_keyed_under_join_data() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    insert_link(&db, o.id, t1.id, 1, Some("前")).await;
    let (ctx, _) = context_for(&db, ALL);

    let first_stage = run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [
                {"id": t1.id, "_joinData": {"note": 123, "seq": 9}},
                {"id": t2.id, "_joinData": {"order_id": 1, "tag_id": 2}},
                {"id": t2.id + 1_000_000, "code": "x"},
            ]}),
        ),
    )
    .await;
    let second_stage = run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [
                {"id": t1.id, "_joinData": {"note": "ng"}},
                {"id": t2.id, "_joinData": {"note": "ng"}},
            ]}),
        ),
    )
    .await;

    assert_eq!(
        invalid(first_stage),
        pairs(&[
            ("tags.0._joinData.note", "type"),
            ("tags.0._joinData.seq", "read_only"),
            ("tags.1._joinData.order_id", "unknown"),
            ("tags.1._joinData.tag_id", "unknown"),
            ("tags.2.code", "unknown"),
        ])
    );
    assert_eq!(
        invalid(second_stage),
        pairs(&[
            ("tags.0._joinData.note", "invalid"),
            ("tags.1._joinData.note", "invalid"),
        ])
    );
    assert_eq!(link_values(&db, o.id).await, [(t1.id, 1, s("前"))]);
}

#[tokio::test]
async fn join_hooks_get_edit_for_kept_links_and_add_for_new_ones() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    let kept = insert_link(&db, o.id, t1.id, 1, None).await;
    let (ctx, calls) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [
                {"id": t1.id, "_joinData": {"note": "残る"}},
                {"id": t2.id, "_joinData": {"note": "足す"}},
                t3.id,
            ]}),
        ),
    )
    .await
    .unwrap();

    let kept_id = Some(RowId::Int(kept.id));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("before", WriteMode::Edit, kept_id.clone()),
            ("after", WriteMode::Edit, kept_id),
            ("before", WriteMode::Add, None),
            ("after", WriteMode::Add, None),
        ],
        "an element without _joinData is not converted"
    );
}

#[tokio::test]
async fn append_adds_new_links_and_keeps_the_unsent() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let t2 = insert_tag(&db, "t2", true).await;
    let t3 = insert_tag(&db, "t3", true).await;
    let t4 = insert_tag(&db, "t4", true).await;
    insert_link(&db, o.id, t1.id, 1, Some("前")).await;
    insert_link(&db, o.id, t2.id, 2, None).await;
    let (ctx, _) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"appended": [
                t4.id,
                {"id": t1.id, "_joinData": {"note": "直した"}},
                {"id": t3.id, "_joinData": {"note": "新しい"}},
                t2.id,
            ]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        link_values(&db, o.id).await,
        [
            (t1.id, 1, s("直した")),
            (t2.id, 2, None),
            (t4.id, 3, None),
            (t3.id, 4, s("新しい")),
        ],
        "existing pairs keep their seq, new ones follow the last in the order sent"
    );
    assert!(modified_of(&db, o.id).await > o.modified);
}

#[tokio::test]
async fn append_checks_new_targets_and_duplicates() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let repeated = run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"appended": [t1.id, {"id": t1.id}]})),
    )
    .await;
    let missing = run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"appended": [t1.id, 999_999_999]})),
    )
    .await;

    assert_eq!(invalid(repeated), pairs(&[("appended.1", "duplicate")]));
    assert_eq!(invalid(missing), pairs(&[("appended.1", "invalid_ref")]));
    assert!(links_of(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn append_with_nothing_new_writes_nothing() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t1 = insert_tag(&db, "t1", true).await;
    insert_link(&db, o.id, t1.id, 1, None).await;
    let (ctx, _) = context_for(&db, ALL);

    run(&ctx, Some(o.id), &edit(&o, json!({"appended": [t1.id]})))
        .await
        .unwrap();
    run(&ctx, Some(o.id), &edit(&o, json!({"appended": []})))
        .await
        .unwrap();

    assert_eq!(link_values(&db, o.id).await, [(t1.id, 1, None)]);
    assert_eq!(modified_of(&db, o.id).await, o.modified);
}

#[test]
#[should_panic(expected = "AppendWithDelete")]
fn append_with_delete_is_rejected_for_many_to_many() {
    struct Bad;

    impl Bad {
        const TAGS: ManyToManyOf<Self, JoinRepo, Tags> = ManyToManyOf::new(
            "bad",
            setting_order_tags::Column::OrderId,
            setting_order_tags::Column::TagId,
        )
        .save(SaveStrategy::AppendWithDelete);
    }

    impl Repo for Bad {
        type Entity = orders::Entity;

        fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
            e.association(Self::TAGS)
        }
    }

    impl TablePolicy for Bad {}
    impl EntityPolicy for Bad {}

    let mut r: Registry = registry();
    r.add(Tags::new());
    r.add(JoinRepo {
        calls: Calls::default(),
    });
    r.add(Bad);
    r.verify();
}

#[tokio::test]
async fn a_many_to_many_without_save_is_read_only() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t = insert_tag(&db, "t", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let res = run(&ctx, Some(o.id), &edit(&o, json!({"view_only": [t.id]}))).await;

    assert_eq!(invalid(res), pairs(&[("view_only", "read_only")]));
    assert!(links_of(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn a_tag_outside_the_association_condition_is_an_invalid_ref() {
    let db = db().await;
    let o = insert_order(&db).await;
    let fine = insert_tag(&db, "fine", true).await;
    let blocked = insert_tag(&db, "blocked", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let res = run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"filtered": [fine.id, blocked.id]})),
    )
    .await;

    assert_eq!(invalid(res), pairs(&[("filtered.1", "invalid_ref")]));
    assert!(links_of(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn an_error_from_a_join_hook_is_returned_as_it_is() {
    let db = db().await;
    let o = insert_order(&db).await;
    let t = insert_tag(&db, "t", true).await;
    let (ctx, _) = context_for(&db, ALL);

    let res = run(
        &ctx,
        Some(o.id),
        &edit(
            &o,
            json!({"tags": [{"id": t.id, "_joinData": {"note": "boom"}}]}),
        ),
    )
    .await;

    assert!(
        matches!(res, Err(PatchError::Repo(RepoError::Forbidden))),
        "{res:?}"
    );
    assert!(links_of(&db, o.id).await.is_empty());
}

#[tokio::test]
async fn an_unchecked_handle_links_a_tag_the_caller_could_not_choose() {
    let db = db().await;
    let o = insert_order(&db).await;
    let off = insert_tag(&db, "off", false).await;
    let (ctx, _) = context_for(&db, ALL);

    let checked = ctx
        .repo::<OrdersRepo>()
        .replace_links(OrdersRepo::TAGS, &o, vec![off.id])
        .await;
    ctx.repo::<OrdersRepo>()
        .unchecked()
        .replace_links(OrdersRepo::TAGS, &o, vec![off.id])
        .await
        .unwrap();

    assert!(
        matches!(checked, Err(RepoError::Validation(_))),
        "{checked:?}"
    );
    assert_eq!(link_values(&db, o.id).await, [(off.id, 1, None)]);
}

#[tokio::test]
async fn a_target_binding_key_stores_the_bound_column_in_the_join_row() {
    let db = db().await;
    let o = insert_order(&db).await;
    let a = insert_order(&db).await;
    let b = insert_order(&db).await;
    let (ctx, _) = context_for(&db, ALL);

    run(
        &ctx,
        Some(o.id),
        &edit(&o, json!({"by_customer": [a.id, b.id]})),
    )
    .await
    .unwrap();
    assert_eq!(
        link_values(&db, o.id).await,
        [(a.customer_id, 1, None), (b.customer_id, 2, None)]
    );

    let fresh = orders::Entity::find_by_id(o.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let missing = run(
        &ctx,
        Some(o.id),
        &edit(&fresh, json!({"by_customer": [a.id, 999_999_999]})),
    )
    .await;
    assert_eq!(invalid(missing), pairs(&[("by_customer.1", "invalid_ref")]));

    ctx.repo::<OrdersRepo>()
        .unlink(OrdersRepo::BY_CUSTOMER, &o, vec![a.id])
        .await
        .unwrap();
    assert_eq!(link_values(&db, o.id).await, [(b.customer_id, 2, None)]);
}
