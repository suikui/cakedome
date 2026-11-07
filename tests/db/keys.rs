//! Writing and reading UUID and string primary keys through the DB.

use cakedome::repo::{DefaultRepo, RepoError};
use sea_orm::ActiveValue::Set;

use crate::common::registry;
use crate::entities::{countries, documents};
use crate::support::{context, db, identity};

type Documents = DefaultRepo<documents::Entity>;
type Countries = DefaultRepo<countries::Entity>;

fn keys_registry() -> cakedome::registry::Registry {
    let mut r = registry();
    r.add(Documents::new());
    r.add(Countries::new());
    r
}

const ALL: &[&str] = &["entity.documents.*", "entity.countries.*"];

#[tokio::test]
async fn uuid_key_is_made_as_v7_and_read_back() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), keys_registry());
    let repo = ctx.repo::<Documents>();

    let saved = repo
        .save(documents::ActiveModel {
            title: Set("doc".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(saved.id.get_version_num(), 7);
    assert_eq!(repo.get(saved.id).await.unwrap(), saved);
}

#[tokio::test]
async fn string_key_is_inserted_and_a_second_insert_is_unique_error() {
    let db = db().await;
    let ctx = context(&db, identity(ALL), keys_registry());
    let repo = ctx.repo::<Countries>();
    let japan = || countries::ActiveModel {
        id: Set("JP".to_string()),
        name: Set("日本".to_string()),
    };

    let saved = repo.insert(japan()).await.unwrap();
    assert_eq!(repo.get("JP".to_string()).await.unwrap(), saved);

    match repo.insert(japan()).await {
        Err(RepoError::Validation(errors)) => {
            let codes: Vec<&str> = errors.0["id"].iter().map(|e| e.code).collect();
            assert_eq!(codes, ["unique"]);
        }
        other => panic!("expected unique: {other:?}"),
    }
}
