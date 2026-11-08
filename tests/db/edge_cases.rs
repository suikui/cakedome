//! Branches and public functions that no other DB test reaches: reference checks on save, unique
//! groups with NULL, a string primary key without `new_key`, empty and UUID keys, `before_find`
//! ordering, a nested many-to-many `with` and a malformed `modified`.

use cakedome::Context;
use cakedome::association::{BelongsToOf, HasManyOf, ManyToManyOf};
use cakedome::patch::{PatchError, Patching};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::{Registry, RepoConfig};
use cakedome::repo::{DefaultRepo, FindOptions, FindQuery, Repo, RepoError};
use sea_orm::ActiveValue::{Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseConnection, Schema};
use serde_json::json;

use crate::common::registry;
use crate::entities::{
    countries, customers, documents, master_tags, order_items, order_tag_links, orders,
};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

const ALL: &[&str] = &[
    "entity.customers.*",
    "entity.orders.*",
    "entity.order_items.*",
    "entity.order_tag_links.*",
    "entity.master_tags.*",
    "entity.countries.*",
    "entity.documents.*",
    "entity.doc_refs.*",
];

type Tags = DefaultRepo<master_tags::Entity>;

fn ctx_with(db: &DatabaseConnection, r: Registry) -> Context {
    context(db, identity(ALL), r)
}

fn invalid_ref(res: Result<order_items::Model, RepoError>, column: &str) {
    match res {
        Err(RepoError::Validation(errors)) => {
            assert_eq!(errors.0[column][0].code, "invalid_ref", "{errors:?}");
        }
        other => panic!("expected Validation: {other:?}"),
    }
}

// ---- reference checks on save ----

struct ItemsByTag;

impl ItemsByTag {
    const TAG: BelongsToOf<Self, Tags> = BelongsToOf::new("tag", order_items::Column::TagId);
}

impl Repo for ItemsByTag {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG)
    }
}

impl TablePolicy for ItemsByTag {}
impl EntityPolicy for ItemsByTag {}

struct ItemsByCode;

impl ItemsByCode {
    const TAG: BelongsToOf<Self, Tags, String> =
        BelongsToOf::new("tag_by_code", order_items::Column::Name)
            .binding_key(master_tags::Column::Code);
}

impl Repo for ItemsByCode {
    type Entity = order_items::Entity;

    fn initialize(e: RepoConfig<'_, order_items::Entity>) -> RepoConfig<'_, order_items::Entity> {
        e.association(Self::TAG)
    }
}

impl TablePolicy for ItemsByCode {}
impl EntityPolicy for ItemsByCode {}

fn item(name: &str, tag_id: Option<i64>) -> order_items::ActiveModel {
    order_items::ActiveModel {
        order_id: Set(1),
        name: Set(name.to_string()),
        tag_id: Set(tag_id),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_foreign_key_with_a_binding_key_is_checked_by_the_bound_column() {
    let db = db().await;
    insert_tag(&db, "bk_ok", true).await;
    let mut r = registry();
    r.add(ItemsByCode);
    r.add(Tags::new());
    let ctx = ctx_with(&db, r);
    let repo = ctx.repo::<ItemsByCode>();

    repo.save(item("bk_ok", None)).await.unwrap();
    invalid_ref(repo.save(item("bk_missing", None)).await, "name");
}

fn tag_registry() -> Registry {
    let mut r = registry();
    r.add(ItemsByTag);
    r.add(Tags::new());
    r
}

#[tokio::test]
async fn a_null_foreign_key_is_not_checked() {
    let db = db().await;
    let ctx = ctx_with(&db, tag_registry());
    let repo = ctx.repo::<ItemsByTag>();

    repo.save(item("no tag", None)).await.unwrap();
    invalid_ref(repo.save(item("bad tag", Some(9999))).await, "tag_id");
}

#[tokio::test]
async fn an_edit_that_keeps_the_foreign_key_does_not_check_it_again() {
    let db = db().await;
    // A dangling reference, as if the target were removed after the row was written.
    let row = order_items::ActiveModel {
        tag_id: Set(Some(9999)),
        created: Set(past()),
        modified: Set(past()),
        is_active: Set(true),
        seq: Set(0),
        ..item("dangling", None)
    }
    .insert(&db)
    .await
    .unwrap();
    let ctx = ctx_with(&db, tag_registry());
    let repo = ctx.repo::<ItemsByTag>();
    let edit = |tag_id| order_items::ActiveModel {
        id: Unchanged(row.id),
        name: Set("edited".to_string()),
        tag_id,
        ..Default::default()
    };

    repo.save(edit(sea_orm::NotSet)).await.unwrap();
    repo.save(edit(Set(Some(9999)))).await.unwrap();
    invalid_ref(repo.save(edit(Set(Some(8888)))).await, "tag_id");
}

// ---- unique and primary keys ----

#[tokio::test]
async fn a_unique_group_with_a_null_column_is_not_checked() {
    let db = db().await;
    let mut r = registry();
    r.add(DefaultRepo::<order_items::Entity>::new())
        .unique([order_items::Column::OrderId, order_items::Column::TagId]);
    let ctx = ctx_with(&db, r);
    let repo = ctx.repo::<DefaultRepo<order_items::Entity>>();

    repo.save(item("a", None)).await.unwrap();
    repo.save(item("b", None)).await.unwrap();
    repo.save(item("c", Some(5))).await.unwrap();
    match repo.save(item("d", Some(5))).await {
        Err(RepoError::Validation(errors)) => assert_eq!(errors.0["order_id"][0].code, "unique"),
        other => panic!("expected Validation: {other:?}"),
    }
}

#[tokio::test]
async fn adding_without_an_id_to_a_string_key_without_new_key_is_required() {
    let db = db().await;
    let mut r = registry();
    r.add(DefaultRepo::<countries::Entity>::new());
    let ctx = ctx_with(&db, r);
    let repo = ctx.repo::<DefaultRepo<countries::Entity>>();
    let nameless = || countries::ActiveModel {
        name: Set("Japan".to_string()),
        ..Default::default()
    };

    for res in [repo.save(nameless()).await, repo.insert(nameless()).await] {
        match res {
            Err(RepoError::Validation(errors)) => {
                assert_eq!(errors.0["id"][0].code, "required", "{errors:?}");
            }
            other => panic!("expected Validation: {other:?}"),
        }
    }
    repo.insert(countries::ActiveModel {
        id: Set("JP".to_string()),
        ..nameless()
    })
    .await
    .unwrap();
}

// ---- empty and UUID keys ----

#[tokio::test]
async fn loading_a_belongs_to_where_every_foreign_key_is_null_returns_nothing() {
    let db = db().await;
    let ctx = ctx_with(&db, tag_registry());
    let repo = ctx.repo::<ItemsByTag>();
    let a = repo.save(item("a", None)).await.unwrap();
    let b = repo.save(item("b", None)).await.unwrap();

    let tags = repo.load(&[a, b], ItemsByTag::TAG).await.unwrap();

    assert!(tags.is_empty());
}

mod doc_refs {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "doc_refs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub doc_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

type Documents = DefaultRepo<documents::Entity>;

struct DocRefs;

impl DocRefs {
    const DOC: BelongsToOf<Self, Documents, uuid::Uuid> =
        BelongsToOf::new("doc", doc_refs::Column::DocId).binding_key(documents::Column::Id);
}

impl Repo for DocRefs {
    type Entity = doc_refs::Entity;

    fn initialize(e: RepoConfig<'_, doc_refs::Entity>) -> RepoConfig<'_, doc_refs::Entity> {
        e.association(Self::DOC)
    }
}

impl TablePolicy for DocRefs {}
impl EntityPolicy for DocRefs {}

#[tokio::test]
async fn a_uuid_binding_key_is_checked_against_the_target() {
    let db = db().await;
    let backend = db.get_database_backend();
    let table = backend.build(&Schema::new(backend).create_table_from_entity(doc_refs::Entity));
    db.execute_unprepared(&table.sql).await.unwrap();
    let doc = documents::ActiveModel {
        id: Set(uuid::Uuid::now_v7()),
        title: Set("doc".to_string()),
    }
    .insert(&db)
    .await
    .unwrap();
    let mut r = registry();
    r.add(DocRefs);
    r.add(Documents::new());
    let ctx = ctx_with(&db, r);
    let repo = ctx.repo::<DocRefs>();
    let reference = |doc_id| doc_refs::ActiveModel {
        doc_id: Set(doc_id),
        ..Default::default()
    };

    repo.save(reference(doc.id)).await.unwrap();
    match repo.save(reference(uuid::Uuid::now_v7())).await {
        Err(RepoError::Validation(errors)) => {
            assert_eq!(errors.0["doc_id"][0].code, "invalid_ref", "{errors:?}");
        }
        other => panic!("expected Validation: {other:?}"),
    }
}

// ---- before_find ordering ----

struct ByName;

#[async_trait]
impl Repo for ByName {
    type Entity = customers::Entity;

    async fn before_find(
        &self,
        _ctx: &Context,
        query: &mut FindQuery<customers::Entity>,
        _options: &FindOptions,
        _primary: bool,
    ) -> Result<(), RepoError> {
        query.order_by_asc(customers::Column::Name);
        Ok(())
    }
}

impl TablePolicy for ByName {}
impl EntityPolicy for ByName {}

#[tokio::test]
async fn before_find_can_order_ascending() {
    let db = db().await;
    insert_customer(&db, "b", true).await;
    insert_customer(&db, "a", true).await;
    insert_customer(&db, "c", true).await;
    let mut r = registry();
    r.add(ByName);
    let ctx = ctx_with(&db, r);

    let rows = ctx.repo::<ByName>().find().all().await.unwrap();

    let names: Vec<_> = rows.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
}

// ---- a nested many-to-many with ----

type Links = DefaultRepo<order_tag_links::Entity>;

struct NestedOrders;

impl NestedOrders {
    const TAGS: ManyToManyOf<Self, Links, NestedTags> = ManyToManyOf::new(
        "tags",
        order_tag_links::Column::OrderId,
        order_tag_links::Column::TagId,
    );
}

impl Repo for NestedOrders {
    type Entity = orders::Entity;

    fn initialize(e: RepoConfig<'_, orders::Entity>) -> RepoConfig<'_, orders::Entity> {
        e.association(Self::TAGS)
    }
}

impl TablePolicy for NestedOrders {}
impl EntityPolicy for NestedOrders {}

struct NestedTags;

impl NestedTags {
    const ITEMS: HasManyOf<Self, DefaultRepo<order_items::Entity>> =
        HasManyOf::new("items", order_items::Column::TagId);
}

impl Repo for NestedTags {
    type Entity = master_tags::Entity;

    fn initialize(e: RepoConfig<'_, master_tags::Entity>) -> RepoConfig<'_, master_tags::Entity> {
        e.association(Self::ITEMS)
    }
}

impl TablePolicy for NestedTags {}
impl EntityPolicy for NestedTags {}

#[tokio::test]
async fn a_many_to_many_can_contain_an_association_of_its_targets() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let tag = insert_tag(&db, "nested", true).await;
    let order = orders::ActiveModel {
        customer_id: Set(c.id),
        title: Set("注文".to_string()),
        is_active: Set(true),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    order_tag_links::ActiveModel {
        order_id: Set(order.id),
        tag_id: Set(tag.id),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    for name in ["x", "y"] {
        order_items::ActiveModel {
            order_id: Set(order.id),
            tag_id: Set(Some(tag.id)),
            seq: Set(0),
            is_active: Set(true),
            created: Set(past()),
            modified: Set(past()),
            ..item(name, None)
        }
        .insert(&db)
        .await
        .unwrap();
    }
    let mut r = registry();
    r.add(NestedOrders);
    r.add(NestedTags);
    r.add(Links::new());
    r.add(DefaultRepo::<order_items::Entity>::new());
    let ctx = ctx_with(&db, r);

    let rows = ctx
        .repo::<NestedOrders>()
        .find()
        .filter(sea_orm::ColumnTrait::eq(&orders::Column::Id, order.id))
        .contain(NestedOrders::TAGS.with(NestedTags::ITEMS))
        .all()
        .await
        .unwrap();

    let tags = rows[0].related(NestedOrders::TAGS).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].related(NestedTags::ITEMS).unwrap().len(), 2);
}

// ---- a malformed modified ----

#[tokio::test]
async fn a_modified_that_is_not_a_datetime_is_a_bad_request() {
    let db = db().await;
    let c = insert_customer(&db, "顧客", true).await;
    let mut r = registry();
    r.add(DefaultRepo::<customers::Entity>::new());
    let ctx = ctx_with(&db, r);
    let repo = ctx.repo::<DefaultRepo<customers::Entity>>();

    let res = repo
        .patch(
            Some(c.id),
            &json!({"data": {"name": "x"}, "modified": "yesterday"}),
        )
        .await;

    match res {
        Err(PatchError::BadRequest(message)) => assert!(message.contains("modified"), "{message}"),
        other => panic!("expected BadRequest: {:?}", other.map(|_| ())),
    }
}
