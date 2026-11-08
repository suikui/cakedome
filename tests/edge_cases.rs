//! Branches and public functions that no other test reaches, without a DB: `Debug` output,
//! defaults of the `Association` and `PreparedChildren` traits, a decimal that does not parse and a
//! rules error at `Registry::add`.

mod common;

use std::any::Any;

use cakedome::DefaultRepo;
use cakedome::association::{
    Association, AssociationType, BelongsToOf, Cardinality, HasManyOf, HasOneOf, LoadQuery,
    ManyToManyOf, NestedScope, PreparedChildren, RelationCondition, RelationInfo, RelationInput,
    RelationKind,
};
use cakedome::key::RowId;
use cakedome::marshal::FieldErrors;
use cakedome::messages::Messages;
use cakedome::meta::{TableRules, TableSpec};
use cakedome::policy::{BasicIdentity, ColumnAccess, PermissionSet};
use cakedome::registry::{EntityMeta, Registry};
use cakedome::repo::RepoError;
use cakedome::{Context, layer};
use sea_orm::Condition;
use sea_orm::prelude::async_trait::async_trait;
use serde_json::json;

mod wide {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "wides")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub owner_id: i64,
        #[sea_orm(column_type = "Decimal(Some((40, 0)))")]
        pub amount: Decimal,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod owners {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "owners")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

type Wides = DefaultRepo<wide::Entity>;
type Owners = DefaultRepo<owners::Entity>;

fn context() -> Context {
    common::context(BasicIdentity::user(
        1,
        None,
        PermissionSet::new(["entity.*"]),
    ))
}

#[test]
fn debug_output_names_each_declaration() {
    const OWNER: BelongsToOf<Wides, Owners> = BelongsToOf::new("owner", wide::Column::OwnerId);
    const WIDES: HasManyOf<Owners, Wides> = HasManyOf::new("wides", wide::Column::OwnerId);
    const ONE: HasOneOf<Owners, Wides> = HasOneOf::new("one", wide::Column::OwnerId);
    const LINKS: ManyToManyOf<Owners, Wides, Wides> =
        ManyToManyOf::new("links", wide::Column::OwnerId, wide::Column::Id);

    let belongs = format!("{OWNER:?}");
    assert!(
        belongs.starts_with("BelongsToOf") && belongs.contains("owner_id"),
        "{belongs}"
    );
    let many = format!("{WIDES:?}");
    assert!(
        many.starts_with("HasManyOf") && many.contains("wides"),
        "{many}"
    );
    let one = format!("{ONE:?}");
    assert!(one.starts_with("HasOneOf") && one.contains("one"), "{one}");
    let through = format!("{LINKS:?}");
    assert!(
        through.starts_with("ManyToManyOf") && through.contains("through"),
        "{through}"
    );
    assert_eq!(
        format!("{:?}", RelationCondition::default()),
        "RelationCondition(..)"
    );
}

#[test]
fn column_access_and_entity_meta_can_be_printed_and_cloned() {
    let access = ColumnAccess::<wide::Entity>::all().hide(wide::Column::Amount);
    let copy = access.clone();
    assert!(!copy.can_read(wide::Column::Amount));
    assert!(format!("{access:?}").starts_with("ColumnAccess"));

    let mut r = common::registry();
    r.add(Wides::new());
    let meta: &EntityMeta = r.meta("wides").unwrap();
    assert!(format!("{meta:?}").contains("wides"));
}

#[test]
fn default_constructors_are_empty() {
    let (condition, orders) = LoadQuery::<Wides>::default().into_parts();
    assert_eq!(condition, Condition::all());
    assert!(orders.is_empty());
    let _ = Wides::default();
}

/// Implements only the required methods of `Association`.
#[derive(Debug)]
struct Plain;

#[async_trait]
impl Association for Plain {
    fn name(&self) -> &'static str {
        "plain"
    }

    fn target(&self) -> &'static str {
        "owners"
    }

    fn info(&self) -> RelationInfo {
        RelationInfo {
            name: "plain",
            kind: RelationKind::Custom("plain"),
            target: "owners",
            through: None,
            foreign_key: "id",
            target_foreign_key: None,
            binding_key: None,
            target_binding_key: None,
            label: None,
            save: None,
            filtered: false,
            finder: false,
            sort: Vec::new(),
            dependent: false,
            cascade_callbacks: true,
        }
    }

    fn cardinality(&self) -> Cardinality {
        Cardinality::One
    }

    fn verify(&self, _owner: &EntityMeta, _registry: &Registry) {}

    async fn usable(&self, _ctx: &Context) -> Result<bool, RepoError> {
        Ok(true)
    }

    fn association_type(&self) -> AssociationType {
        AssociationType::ManyToOne
    }

    async fn matching_condition(
        &self,
        _ctx: &Context,
        target: Condition,
    ) -> Result<Condition, RepoError> {
        Ok(target)
    }

    fn typed_contain(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn an_association_with_only_the_required_methods_gets_the_defaults() {
    let ctx = context();
    let plain = Plain;

    assert_eq!(plain.label(), None);
    assert_eq!(plain.owner_foreign_key(), None);
    assert_eq!(plain.child_foreign_key(), None);
    assert_eq!(plain.binding_key(), None);
    assert!(!plain.dependent());
    assert!(plain.cascade_callbacks());
    assert!(plain.check_exists(&ctx, &RowId::Int(1)).await.unwrap());
    plain
        .delete_dependents(&ctx, &RowId::Int(1), false, false)
        .await
        .unwrap();
    let mut errors = FieldErrors::default();
    let input = plain
        .read_input(&ctx, &json!([]), &mut errors, &NestedScope::default())
        .unwrap();
    assert!(matches!(input, RelationInput::Ignored));
    assert!(errors.is_empty());
}

struct Rows;

#[async_trait]
impl PreparedChildren for Rows {
    async fn write(
        self: Box<Self>,
        _tx: &Context,
        _parent_id: Option<RowId>,
        _unchecked: bool,
        _errors: &mut FieldErrors,
    ) -> Result<bool, RepoError> {
        Ok(false)
    }
}

#[test]
fn prepared_children_are_keyed_by_the_primary_key_by_default() {
    assert_eq!(Rows.parent_key(), None);
}

#[test]
fn a_decimal_that_the_type_cannot_hold_is_a_type_error() {
    let fields = common::fields::<wide::Entity>();
    let amount = fields.iter().find(|f| f.name == "amount").unwrap();
    let messages = Messages::default();

    // Within the column's precision (40) but beyond what `Decimal` can hold.
    let (code, _, _) = layer::json_to_value(amount, &json!("1".repeat(35)), &messages).unwrap_err();
    assert_eq!(code, "type");
    assert!(layer::json_to_value(amount, &json!("12345"), &messages).is_ok());
}

struct Broken;

impl TableRules for Broken {
    fn spec(&self, _table: &str, _columns: &[&'static str]) -> Result<TableSpec, String> {
        Err("no rules for this table".to_string())
    }
}

#[test]
#[should_panic(expected = "owners: no rules for this table")]
fn adding_an_entity_whose_rules_fail_panics_with_the_table_name() {
    let mut r = Registry::new(Broken, &[], &[]);
    r.add(Owners::new());
}
