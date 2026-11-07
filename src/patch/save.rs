//! The save stages: read the body, convert the input, persist.

use crate::repo::{GuardedRepo, MAX_CONTAIN_DEPTH};
use chrono::{DateTime, SubsecRound, Utc};
use sea_orm::{ActiveModelTrait, DbErr};
use serde_json::{Map, Value as Json};

use super::{Marshaller, Patch, PatchError, PatchOptions};
use crate::association::{
    NestedScope, PrepareFn, PrepareParentFn, PreparedChildren, PreparedParent, RelationInput,
    primary_column,
};
use crate::context::Context;
use crate::json::{JsonFor, default_contain_tree};
use crate::key::{Key, RowKey};
use crate::marshal::{FieldErrors, MarshalOptions, WriteMode, try_from_json};
use crate::policy::{self, Action, resolve};
use crate::registry::EntityMeta;
use crate::repo::RepoEntity;
use crate::repo::RepoError;

pub(super) struct SaveBody {
    // The whole `data` after `before_marshal`, association keys included.
    data: Map<String, Json>,
    expected: Option<DateTime<Utc>>,
    scope: NestedScope,
    accepted: Vec<String>,
}

pub(super) struct RowSpec<'a, R: GuardedRepo> {
    pub(super) mode: WriteMode,
    pub(super) id: Option<Key<R>>,
    pub(super) association: Option<&'static str>,
    // Columns hidden from input: a hasMany child's foreign key (two for a many-to-many
    // `_joinData`).
    pub(super) hidden_columns: &'a [&'a str],
    // Columns input cannot write: the sequence column of a `Replace` child.
    pub(super) read_only_column: Option<&'a str>,
    pub(super) defaults: bool,
    // False for rows with a delete mark.
    pub(super) run_after: bool,
    pub(super) scope: &'a NestedScope,
}

pub(super) struct MarshaledRow<R: GuardedRepo> {
    pub(super) am: Option<<R::Entity as sea_orm::EntityTrait>::ActiveModel>,
    pub(super) parents: Vec<Box<dyn PreparedParent>>,
    pub(super) prepared: Vec<Box<dyn PreparedChildren>>,
}

impl<R, E> Marshaller<R>
where
    R: GuardedRepo<Entity = E>,
    E: RepoEntity,
{
    pub(super) async fn read_body(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        id: Option<&Key<R>>,
        body: &Json,
        options: &PatchOptions,
    ) -> Result<SaveBody, PatchError> {
        let (associated, from_body) = match options.listed() {
            Some(list) => (Some(list.to_vec()), false),
            None => match body.get("associated") {
                None => (None, true),
                Some(value) => (
                    Some(
                        value
                            .as_array()
                            .and_then(|a| {
                                a.iter()
                                    .map(|v| v.as_str().map(str::to_string))
                                    .collect::<Option<Vec<_>>>()
                            })
                            .ok_or_else(|| {
                                PatchError::BadRequest(
                                    "associated is not an array of strings".into(),
                                )
                            })?,
                    ),
                    true,
                ),
            },
        };
        // Check every level of every path up front, so typos are not silently dropped.
        if let Some(paths) = &associated {
            self.check_paths(ctx, meta, paths, from_body)?;
        }
        let scope = NestedScope {
            associated,
            depth: 0,
            owner_id: None,
        };
        let accepted_names = scope.top_names();
        let listed = |name: &str| {
            accepted_names
                .as_ref()
                .is_none_or(|names| names.contains(&name))
        };
        let original = body
            .get("data")
            .and_then(Json::as_object)
            .ok_or_else(|| PatchError::BadRequest("data is not an object".into()))?;
        let expected = match id {
            Some(_) => {
                let modified = ctx.registry().spec_of::<E>().modified_column;
                Some(parse_modified(body, modified)?)
            }
            None => None,
        };
        let mode = if id.is_some() {
            WriteMode::Edit
        } else {
            WriteMode::Add
        };
        // Shape errors come before permission errors, but hooks run after permission, so only the
        // shape is checked here and the results are dropped.
        for association in &meta.relations {
            if !listed(association.name()) {
                continue;
            }
            let Some(value) = original.get(association.name()) else {
                continue;
            };
            if !association.usable(ctx).await? {
                continue;
            }
            let mut discarded = FieldErrors::default();
            if let Err(e @ PatchError::BadRequest(_)) =
                association.read_input(ctx, value, &mut discarded, &scope.child(association.name()))
            {
                return Err(e);
            }
        }
        // Hooks must not run for a caller without permission.
        ctx.repo::<R>()
            .check(match mode {
                WriteMode::Edit => Action::Edit,
                WriteMode::Add => Action::Add,
            })
            .await?;
        let mut data = original.clone();
        let options = MarshalOptions::new(mode, id.map(|k| k.clone().into_row_id()), None);
        self.repo.before_marshal(ctx, &mut data, &options).await?;

        let accepted = match body.get("acceptWarnings") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    PatchError::BadRequest("acceptWarnings is not an array of strings".into())
                })?,
        };

        Ok(SaveBody {
            data,
            expected,
            scope,
            accepted,
        })
    }

    // A missing name is `BadRequest` from a body, and a code error `Repo(Db)` from code.
    fn check_paths(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        paths: &[String],
        from_body: bool,
    ) -> Result<(), PatchError> {
        for path in paths {
            let mut current = meta;
            for name in path.split('.') {
                let next = current
                    .relations
                    .iter()
                    .find(|a| a.name() == name)
                    .and_then(|a| ctx.registry().meta(a.target()));
                match next {
                    Some(next) => current = next,
                    None if from_body => {
                        return Err(PatchError::BadRequest(format!(
                            "associated {path} is not registered"
                        )));
                    }
                    None => {
                        return Err(PatchError::from(DbErr::Custom(format!(
                            "{}: association {name} is not registered",
                            R::repo_name()
                        ))));
                    }
                }
            }
        }
        Ok(())
    }

    // Errors are keyed relative to the row; the caller adds the prefix (`orders.0`).
    pub(super) async fn marshal_row(
        &self,
        ctx: &Context,
        hooked: &Map<String, Json>,
        input: Map<String, Json>,
        spec: RowSpec<'_, R>,
        errors: &mut FieldErrors,
    ) -> Result<MarshaledRow<R>, PatchError> {
        if spec.scope.depth > MAX_CONTAIN_DEPTH {
            return Err(PatchError::BadRequest("nesting is too deep".into()));
        }
        let meta = self.meta(ctx)?;
        let accepted_names = spec.scope.top_names();
        let listed = |name: &str| {
            accepted_names
                .as_ref()
                .is_none_or(|names| names.contains(&name))
        };
        // Usable association keys are taken out of the row; unusable names are left to `unknown`.
        // belongsTo keys stay in the input.
        let mut rest = input;
        let mut prepares: Vec<PrepareFn> = Vec::new();
        let mut parent_prepares: Vec<PrepareParentFn> = Vec::new();
        let mut parent_columns: Vec<&'static str> = Vec::new();
        for association in &meta.relations {
            if !listed(association.name()) {
                rest.remove(association.name());
                continue;
            }
            let Some(value) = rest.get(association.name()).cloned() else {
                continue;
            };
            if !association.usable(ctx).await? {
                continue;
            }
            let mut child_scope = spec.scope.child(association.name());
            child_scope.owner_id = spec.id.clone().map(RowKey::into_row_id);
            match association.read_input(ctx, &value, errors, &child_scope)? {
                RelationInput::Ignored => {}
                RelationInput::ReadOnly => {
                    rest.remove(association.name());
                }
                RelationInput::Write(prepare) => {
                    rest.remove(association.name());
                    prepares.push(prepare);
                }
                RelationInput::WriteParent(prepare) => {
                    rest.remove(association.name());
                    if let Some(column) = association.owner_foreign_key() {
                        // The target row wins; the foreign key value is dropped.
                        rest.remove(column);
                        parent_columns.push(column);
                    }
                    parent_prepares.push(prepare);
                }
            }
        }

        let access = resolve::columns(ctx, &*self.repo).await?;
        let mut input_fields = meta.fields.clone();
        for f in &mut input_fields {
            if !policy::table_readable::<E>(&access, f) || spec.hidden_columns.contains(&f.name) {
                f.hidden = true;
            }
            // Foreign keys filled by a target row need no required check.
            if parent_columns.contains(&f.name) {
                f.nullable = true;
            }
        }
        // On insert, omitted columns get dynamic defaults, even unwritable ones.
        if spec.defaults {
            for (name, default) in ctx.registry().dynamic_defaults::<E>() {
                if rest.contains_key(name) {
                    continue;
                }
                if let Some(f) = input_fields.iter_mut().find(|f| f.name == name) {
                    f.default = Some(default.value(ctx).await?);
                }
            }
        }
        let parsed = try_from_json::<E>(
            &input_fields,
            &rest,
            spec.mode,
            |f| policy::table_writable::<E>(&access, f) && spec.read_only_column != Some(f.name),
            ctx.registry().messages(),
        )?;
        let mut parents: Vec<Box<dyn PreparedParent>> = Vec::new();
        for prepare in parent_prepares {
            match prepare(ctx).await {
                Ok(Some(one)) => parents.push(one),
                Ok(None) => {}
                Err(PatchError::Invalid(e)) => errors.merge(e),
                Err(e) => return Err(e),
            }
        }
        let mut prepared: Vec<Box<dyn PreparedChildren>> = Vec::new();
        for prepare in prepares {
            match prepare(ctx).await {
                Ok(Some(one)) => prepared.push(one),
                Ok(None) => {}
                Err(PatchError::Invalid(e)) => errors.merge(e),
                Err(e) => return Err(e),
            }
        }
        // `after_marshal` runs once the row is read, even if children had errors.
        let am = match parsed {
            Ok(mut am) => {
                if spec.run_after {
                    let options = MarshalOptions::new(
                        spec.mode,
                        spec.id.clone().map(RowKey::into_row_id),
                        spec.association,
                    );
                    self.repo
                        .after_marshal(ctx, &mut am, hooked, errors, &options)
                        .await?;
                }
                if let Some(id) = spec.id {
                    am.try_set(primary_column::<R>(), id.into())?;
                }
                Some(am)
            }
            Err(e) => {
                errors.merge(e);
                None
            }
        };
        Ok(MarshaledRow {
            am,
            parents,
            prepared,
        })
    }

    pub(super) async fn marshal(
        &self,
        ctx: &Context,
        _meta: &EntityMeta,
        id: Option<&Key<R>>,
        body: SaveBody,
    ) -> Result<Patch<R>, PatchError> {
        let SaveBody {
            data,
            expected,
            scope,
            accepted,
        } = body;
        let mode = if id.is_some() {
            WriteMode::Edit
        } else {
            WriteMode::Add
        };
        let mut errors = FieldErrors::default();
        let row = self
            .marshal_row(
                ctx,
                &data,
                data.clone(),
                RowSpec {
                    mode,
                    id: id.cloned(),
                    association: None,
                    hidden_columns: &[],
                    read_only_column: None,
                    defaults: id.is_none(),
                    run_after: true,
                    scope: &scope,
                },
                &mut errors,
            )
            .await?;
        let MarshaledRow {
            am,
            parents,
            prepared,
        } = row;
        let Some(am) = am else {
            return Err(PatchError::Invalid(errors));
        };
        if !errors.is_empty() {
            return Err(PatchError::Invalid(errors));
        }
        Ok(Patch {
            am,
            id: id.cloned(),
            expected,
            parents,
            prepared,
            accepted,
        })
    }

    pub(super) async fn persist(
        &self,
        ctx: &Context,
        meta: &EntityMeta,
        patch: Patch<R>,
    ) -> Result<E::Model, PatchError> {
        let Patch {
            am,
            id,
            expected,
            parents,
            prepared,
            accepted,
        } = patch;
        let tx = ctx.begin().await?;
        let written = tx
            .repo::<R>()
            .accepting_warnings(accepted)
            .save_associated(am, expected, parents, prepared, id.is_none())
            .await;
        match (tx.finish(written).await, id) {
            (Ok(model), _) => Ok(model),
            (Err(RepoError::Conflict), Some(id)) => Err(self.conflict(ctx, meta, id).await),
            (Err(e), _) => Err(e.into()),
        }
    }

    async fn conflict(&self, ctx: &Context, meta: &EntityMeta, id: Key<R>) -> PatchError {
        let tree = match default_contain_tree(ctx, meta).await {
            Ok(tree) => tree,
            Err(e) => return e.into(),
        };
        let current = match ctx.repo::<R>().get(id).await {
            Ok(current) => current,
            Err(e) => return e.into(),
        };
        let json = JsonFor::<R> {
            repo: self.repo.clone(),
        };
        let readability = match json
            .row_readability(ctx, meta, std::slice::from_ref(&current))
            .await
        {
            Ok(readability) => readability,
            Err(e) => return e.into(),
        };
        let mut data = json.data_json(meta, &current, &readability);
        if let Err(e) = JsonFor::<R>::attach_typed(
            ctx,
            vec![current.clone()],
            std::slice::from_mut(&mut data),
            &tree,
        )
        .await
        {
            return e.into();
        }
        PatchError::Conflict {
            current: Json::Object(data),
        }
    }
}

// An Entity without the column gets the Repo's error.
fn parse_modified(body: &Json, modified_column: Option<&str>) -> Result<DateTime<Utc>, PatchError> {
    let key = modified_column
        .ok_or_else(|| DbErr::Custom("the modified column does not exist".to_string()))?;
    let bad = || PatchError::BadRequest(format!("{key} is required (an RFC 3339 string)"));
    let s = body.get(key).and_then(Json::as_str).ok_or_else(bad)?;
    let t = DateTime::parse_from_rfc3339(s).map_err(|_| bad())?;
    Ok(t.with_timezone(&Utc).trunc_subsecs(6))
}
