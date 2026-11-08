//! Uniqueness checks before a write (`unique`, with column groups) and duplicate / Repo warnings,
//! on integer, UUID and string primary keys.

use cakedome::Context;
use cakedome::key::Key;
use cakedome::patch::{PatchError, Patching};
use cakedome::policy::{EntityPolicy, TablePolicy};
use cakedome::registry::Registry;
use cakedome::repo::{DefaultRepo, GuardedRepo, Repo, RepoError};
use cakedome::validate::{ValidationInput, Warning};
use sea_orm::ActiveValue::{self, Set, Unchanged};
use sea_orm::prelude::async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
};
use serde_json::{Value, json};

use crate::common::registry;
use crate::entities::{countries, customers, documents, master_tags, order_items};
use crate::support::{context, db, identity, insert_customer, insert_tag, past};

type Tags = DefaultRepo<master_tags::Entity>;
type Customers = DefaultRepo<customers::Entity>;
type Items = DefaultRepo<order_items::Entity>;
type Documents = DefaultRepo<documents::Entity>;
type Countries = DefaultRepo<countries::Entity>;

const ALL: &[&str] = &[
    "entity.master_tags.*",
    "entity.customers.*",
    "entity.order_items.*",
    "entity.documents.*",
    "entity.countries.*",
];

fn unique_keys<T: std::fmt::Debug>(res: Result<T, RepoError>) -> Vec<String> {
    match res {
        Err(RepoError::Validation(errors)) => errors
            .0
            .iter()
            .flat_map(|(k, v)| v.iter().filter(|e| e.code == "unique").map(|_| k.clone()))
            .collect(),
        other => panic!("expected Validation: {other:?}"),
    }
}

fn new_tag(code: &str) -> master_tags::ActiveModel {
    master_tags::ActiveModel {
        code: Set(code.to_string()),
        name: Set(format!("{code} name")),
        ..Default::default()
    }
}

async fn insert_item(
    db: &DatabaseConnection,
    order_id: i64,
    name: &str,
    active: bool,
) -> order_items::Model {
    order_items::ActiveModel {
        order_id: Set(order_id),
        name: Set(name.to_string()),
        seq: Set(0),
        is_active: Set(active),
        created: Set(past()),
        modified: Set(past()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap()
}

fn item_edit(id: i64) -> order_items::ActiveModel {
    order_items::ActiveModel {
        id: Unchanged(id),
        ..Default::default()
    }
}

// ---- unique ----

#[tokio::test]
async fn add_with_a_duplicate_code_is_a_unique_error() {
    let db = db().await;
    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(ALL), r);
    insert_tag(&db, "un_add", true).await;

    let res = ctx.repo::<Tags>().save(new_tag("un_add")).await;

    assert_eq!(unique_keys(res), ["code"]);
}

#[tokio::test]
async fn inactive_rows_count_and_unchecked_also_checks() {
    let db = db().await;
    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(ALL), r);
    insert_tag(&db, "un_inactive", false).await;

    let res = ctx.repo::<Tags>().save(new_tag("un_inactive")).await;
    assert_eq!(unique_keys(res), ["code"]);

    let res = ctx
        .repo::<Tags>()
        .unchecked()
        .save(new_tag("un_inactive"))
        .await;
    assert_eq!(unique_keys(res), ["code"]);
}

#[tokio::test]
async fn edit_checks_only_when_the_code_changes_to_another_rows_value() {
    let db = db().await;
    let mut r = registry();
    r.add(Tags::new());
    let ctx = context(&db, identity(ALL), r);
    let a = insert_tag(&db, "un_edit_a", true).await;
    insert_tag(&db, "un_edit_b", true).await;
    let edit = |code: &str, name: &str| master_tags::ActiveModel {
        id: Unchanged(a.id),
        code: Set(code.to_string()),
        name: Set(name.to_string()),
        ..Default::default()
    };

    let res = ctx.repo::<Tags>().save(edit("un_edit_b", "x")).await;
    assert_eq!(unique_keys(res), ["code"]);

    let saved = ctx
        .repo::<Tags>()
        .save(edit("un_edit_a", "only the name"))
        .await
        .unwrap();
    assert_eq!(saved.name, "only the name");
}

#[tokio::test]
async fn a_column_group_compares_the_unchanged_column_with_the_current_row() {
    let db = db().await;
    let mut r = registry();
    r.add(Items::new())
        .unique([order_items::Column::OrderId, order_items::Column::Name]);
    let ctx = context(&db, identity(ALL), r);
    let repo = ctx.repo::<Items>();
    let a = insert_item(&db, 1, "a", true).await;
    let b = insert_item(&db, 1, "b", true).await;
    let c = insert_item(&db, 2, "a", true).await;
    let d = insert_item(&db, 1, "a", true).await;

    // Neither column changes, so a row that already duplicates another can still be edited.
    repo.save(order_items::ActiveModel {
        seq: Set(5),
        ..item_edit(d.id)
    })
    .await
    .unwrap();
    repo.save(order_items::ActiveModel {
        name: Set("a".to_string()),
        ..item_edit(d.id)
    })
    .await
    .unwrap();

    let res = repo
        .save(order_items::ActiveModel {
            name: Set("a".to_string()),
            ..item_edit(b.id)
        })
        .await;
    assert_eq!(unique_keys(res), ["order_id"], "name moves onto (1, a)");

    let res = repo
        .save(order_items::ActiveModel {
            order_id: Set(1),
            ..item_edit(c.id)
        })
        .await;
    assert_eq!(unique_keys(res), ["order_id"], "order_id moves onto (1, a)");

    repo.save(order_items::ActiveModel {
        name: Set("z".to_string()),
        ..item_edit(b.id)
    })
    .await
    .unwrap();
    repo.save(order_items::ActiveModel {
        order_id: Set(3),
        ..item_edit(a.id)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unique_on_uuid_and_string_keys_ignores_the_row_itself_and_finds_others() {
    let db = db().await;
    let mut r = registry();
    r.add(Documents::new()).unique([documents::Column::Title]);
    r.add(Countries::new()).unique([countries::Column::Name]);
    let ctx = context(&db, identity(ALL), r);
    let docs = ctx.repo::<Documents>();
    let new_doc = |title: &str| documents::ActiveModel {
        title: Set(title.to_string()),
        ..Default::default()
    };
    docs.save(new_doc("un_t1")).await.unwrap();
    let d2 = docs.save(new_doc("un_t2")).await.unwrap();
    let rename = |title: &str| documents::ActiveModel {
        id: Unchanged(d2.id),
        title: Set(title.to_string()),
    };

    assert_eq!(unique_keys(docs.save(new_doc("un_t1")).await), ["title"]);
    assert_eq!(unique_keys(docs.save(rename("un_t1")).await), ["title"]);
    docs.save(rename("un_t2")).await.unwrap();
    docs.save(rename("un_t3")).await.unwrap();

    let countries = ctx.repo::<Countries>();
    let country = |id: &str, name: &str| countries::ActiveModel {
        id: Set(id.to_string()),
        name: Set(name.to_string()),
    };
    countries.insert(country("JP", "un_Japan")).await.unwrap();
    countries.insert(country("US", "un_USA")).await.unwrap();
    assert_eq!(
        unique_keys(countries.insert(country("XX", "un_Japan")).await),
        ["name"]
    );
    let rename_us = |name: &str| countries::ActiveModel {
        id: Unchanged("US".to_string()),
        name: Set(name.to_string()),
    };
    assert_eq!(
        unique_keys(countries.save(rename_us("un_Japan")).await),
        ["name"]
    );
    countries.save(rename_us("un_USA")).await.unwrap();
    countries.save(rename_us("un_America")).await.unwrap();
}

// ---- warnings ----

async fn patch_save<R: GuardedRepo>(
    ctx: &Context,
    id: Option<Key<R>>,
    body: Value,
) -> Result<<R::Entity as EntityTrait>::Model, PatchError> {
    let repo = ctx.repo::<R>();
    let patch = repo.patch(id, &body).await?;
    repo.save_patch(patch).await
}

fn warning_keys<T: std::fmt::Debug>(res: Result<T, PatchError>) -> Vec<String> {
    match res {
        Err(PatchError::Warnings(w)) => w.into_iter().map(|w| w.key).collect(),
        other => panic!("expected Warnings: {other:?}"),
    }
}

async fn tags_named(db: &DatabaseConnection, name: &str) -> u64 {
    master_tags::Entity::find()
        .filter(master_tags::Column::Name.eq(name))
        .count(db)
        .await
        .unwrap()
}

fn tag_registry() -> Registry {
    let mut r = registry();
    r.add(Tags::new())
        .warn_duplicate([master_tags::Column::Name]);
    r
}

async fn tag_with_name(db: &DatabaseConnection, code: &str, name: &str) {
    master_tags::ActiveModel {
        code: Set(code.to_string()),
        name: Set(name.to_string()),
        is_active: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_duplicate_name_is_a_warning_until_accepted() {
    let db = db().await;
    tag_with_name(&db, "wn_dup_a", "wn_dup").await;
    let ctx = context(&db, identity(ALL), tag_registry());
    let body = |accept: Value| json!({"data": {"code": "wn_dup_b", "name": "wn_dup"}, "acceptWarnings": accept});

    let res = patch_save::<Tags>(&ctx, None, body(json!([]))).await;
    assert_eq!(warning_keys(res), ["name.duplicate"]);
    assert_eq!(tags_named(&db, "wn_dup").await, 1, "nothing is saved");

    let res = patch_save::<Tags>(&ctx, None, body(json!(["code.duplicate"]))).await;
    assert_eq!(warning_keys(res), ["name.duplicate"]);

    patch_save::<Tags>(&ctx, None, body(json!(["name.duplicate"])))
        .await
        .unwrap();
    assert_eq!(tags_named(&db, "wn_dup").await, 2);
}

#[tokio::test]
async fn validation_errors_come_before_warnings() {
    let db = db().await;
    tag_with_name(&db, "wn_err_a", "wn_err").await;
    let ctx = context(&db, identity(ALL), tag_registry());

    let res = patch_save::<Tags>(
        &ctx,
        None,
        json!({"data": {"code": "wn_err_a", "name": "wn_err"}}),
    )
    .await;

    match res {
        Err(PatchError::Invalid(errors)) => assert_eq!(errors.0["code"][0].code, "unique"),
        other => panic!("expected Invalid: {other:?}"),
    }
}

#[tokio::test]
async fn editing_without_changing_the_name_has_no_warning() {
    let db = db().await;
    tag_with_name(&db, "wn_edit_a", "wn_edit").await;
    tag_with_name(&db, "wn_edit_b", "wn_edit").await;
    let b = master_tags::Entity::find()
        .filter(master_tags::Column::Code.eq("wn_edit_b"))
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let ctx = context(&db, identity(ALL), tag_registry());

    let saved = patch_save::<Tags>(&ctx, Some(b.id), json!({"data": {"code": "wn_edit_c"}}))
        .await
        .unwrap();

    assert_eq!(saved.code, "wn_edit_c");
}

#[tokio::test]
async fn business_code_save_ignores_warnings() {
    let db = db().await;
    tag_with_name(&db, "wn_code_a", "wn_code").await;
    let ctx = context(&db, identity(ALL), tag_registry());

    let saved = ctx
        .repo::<Tags>()
        .save(master_tags::ActiveModel {
            code: Set("wn_code_b".to_string()),
            name: Set("wn_code".to_string()),
            ..Default::default()
        })
        .await;

    assert!(saved.is_ok(), "{saved:?}");
    assert_eq!(tags_named(&db, "wn_code").await, 2);
}

#[tokio::test]
async fn duplicate_warnings_ignore_soft_deleted_rows_but_not_inactive_masters() {
    let db = db().await;
    let mut r = registry();
    r.add(Customers::new())
        .optimistic_lock(false)
        .warn_duplicate([customers::Column::Name]);
    r.add(Tags::new())
        .warn_duplicate([master_tags::Column::Name]);
    let ctx = context(&db, identity(ALL), r);
    insert_customer(&db, "wn_soft", false).await;
    master_tags::ActiveModel {
        code: Set("wn_off".to_string()),
        name: Set("wn_off".to_string()),
        is_active: Set(false),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    patch_save::<Customers>(
        &ctx,
        None,
        json!({"data": {"name": "wn_soft"}, "acceptWarnings": []}),
    )
    .await
    .unwrap();
    let res = patch_save::<Customers>(
        &ctx,
        None,
        json!({"data": {"name": "wn_soft"}, "acceptWarnings": []}),
    )
    .await;
    assert_eq!(
        warning_keys(res),
        ["name.duplicate"],
        "the active copy counts"
    );

    let res = patch_save::<Tags>(
        &ctx,
        None,
        json!({"data": {"code": "wn_off2", "name": "wn_off"}}),
    )
    .await;
    assert_eq!(warning_keys(res), ["name.duplicate"]);
}

/// Warns for a customer whose name ends with `?`.
struct WarnCustomers;

#[async_trait]
impl Repo for WarnCustomers {
    type Entity = customers::Entity;

    async fn warnings(
        &self,
        _ctx: &Context,
        input: &ValidationInput<'_, customers::Entity>,
    ) -> Result<Vec<Warning>, RepoError> {
        let mut warnings = Vec::new();
        if let ActiveValue::Set(name) = &input.after.name
            && name.ends_with('?')
        {
            warnings.push(Warning::new("name", "unsure", "check the name"));
        }
        Ok(warnings)
    }
}

impl TablePolicy for WarnCustomers {}
impl EntityPolicy for WarnCustomers {}

#[tokio::test]
async fn repo_warnings_stop_until_accepted() {
    let db = db().await;
    let mut r = registry();
    r.add(WarnCustomers);
    let ctx = context(&db, identity(ALL), r);
    let body = |accept: Value| json!({"data": {"name": "wn_hook?"}, "acceptWarnings": accept});

    let res = patch_save::<WarnCustomers>(&ctx, None, body(json!([]))).await;
    assert_eq!(warning_keys(res), ["name.unsure"]);

    patch_save::<WarnCustomers>(&ctx, None, body(json!(["name.unsure"])))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_warning_group_compares_the_unchanged_column_with_the_current_row() {
    let db = db().await;
    let mut r = registry();
    r.add(Items::new())
        .optimistic_lock(false)
        .warn_duplicate([order_items::Column::OrderId, order_items::Column::Name]);
    let ctx = context(&db, identity(ALL), r);
    let a = insert_item(&db, 1, "a", true).await;
    let b = insert_item(&db, 1, "b", true).await;
    let c = insert_item(&db, 2, "a", true).await;
    let d = insert_item(&db, 1, "a", true).await;
    insert_item(&db, 1, "gone", false).await;
    let edit = |id: i64, data: Value| {
        let ctx = &ctx;
        async move {
            patch_save::<Items>(ctx, Some(id), json!({"data": data, "acceptWarnings": []})).await
        }
    };

    edit(d.id, json!({"seq": 5})).await.unwrap();
    edit(d.id, json!({"name": "a"})).await.unwrap();
    assert_eq!(
        warning_keys(edit(b.id, json!({"name": "a"})).await),
        ["order_id.duplicate"]
    );
    assert_eq!(
        warning_keys(edit(c.id, json!({"order_id": 1})).await),
        ["order_id.duplicate"]
    );
    edit(b.id, json!({"name": "gone"})).await.unwrap();
    edit(a.id, json!({"order_id": 3})).await.unwrap();
}

#[tokio::test]
async fn warnings_on_uuid_and_string_keys_ignore_the_row_itself_and_find_others() {
    let db = db().await;
    let mut r = registry();
    r.add(Documents::new())
        .warn_duplicate([documents::Column::Title]);
    r.add(Countries::new())
        .warn_duplicate([countries::Column::Name]);
    let ctx = context(&db, identity(ALL), r);
    let add_doc = |title: &str| {
        patch_save::<Documents>(
            &ctx,
            None,
            json!({"data": {"title": title}, "acceptWarnings": []}),
        )
    };
    add_doc("wn_t1").await.unwrap();
    let d2 = add_doc("wn_t2").await.unwrap();
    assert_eq!(warning_keys(add_doc("wn_t1").await), ["title.duplicate"]);
    let rename = |title: &str| {
        patch_save::<Documents>(
            &ctx,
            Some(d2.id),
            json!({"data": {"title": title}, "acceptWarnings": []}),
        )
    };
    assert_eq!(warning_keys(rename("wn_t1").await), ["title.duplicate"]);
    rename("wn_t2").await.unwrap();
    rename("wn_t3").await.unwrap();

    let add_country = |id: &str, name: &str| {
        patch_save::<Countries>(
            &ctx,
            None,
            json!({"data": {"id": id, "name": name}, "acceptWarnings": []}),
        )
    };
    add_country("JP", "wn_Japan").await.unwrap();
    add_country("US", "wn_USA").await.unwrap();
    assert_eq!(
        warning_keys(add_country("XX", "wn_Japan").await),
        ["name.duplicate"]
    );
    let rename_us = |name: &str| {
        patch_save::<Countries>(
            &ctx,
            Some("US".to_string()),
            json!({"data": {"name": name}, "acceptWarnings": []}),
        )
    };
    assert_eq!(
        warning_keys(rename_us("wn_Japan").await),
        ["name.duplicate"]
    );
    rename_us("wn_USA").await.unwrap();
    rename_us("wn_America").await.unwrap();
}
