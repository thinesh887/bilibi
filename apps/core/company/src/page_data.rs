//! Company page read models and audited writes missing from the original slice.
use crate::{
    hierarchy, organisation, policy::Policy, CompanyService, CreateOutcome, Edit, NewCompany,
    WriteError, UPDATE,
};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

impl CompanyService {
    pub async fn create_with_context(
        &self,
        scope: &Scope,
        new: &NewCompany,
        activities: Option<Value>,
        metadata: Option<Value>,
    ) -> Result<CreateOutcome> {
        if activities.as_ref().is_some_and(|v| !v.is_object())
            || metadata.as_ref().is_some_and(|v| !v.is_object())
        {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("company.create"))
            .await?;
        if let Some(parent) = new.parent_id {
            // The tree lock comes before the parent row, as a parent change takes it.
            organisation::lock_tree(tx.sql(), scope.tenant()).await?;
            organisation::authorize_target(tx.sql(), scope, parent, crate::CREATE).await?;
            organisation::require_writable(tx.sql(), scope, parent, true)
                .await
                .map_err(write_error)?;
        }
        let result = organisation::create(&mut tx, scope, new).await?;
        if let CreateOutcome::Created(company) = &result {
            let id = company.id;
            let tenant = scope.tenant();
            tx.mutate(move |sql| Box::pin(async move {
                sql.execute("UPDATE companies SET scope_activities=$3,metadata=$4 WHERE tenant_id=$1 AND id=$2", &[&tenant,&id,&activities,&metadata]).await?;
                Ok(((), vec![Change { kind:"company".into(), id, old:json!({"scope_activities":null,"metadata":null}), new:json!({"scope_activities":activities,"metadata":metadata}) }]))
            })).await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(result)
    }

    /// Saves one fact of the detail page. A parent that would make a loop or
    /// a tree deeper than the tenant's limit is `WriteError::Invalid` with the
    /// `parent_id` error that says so; every other refusal is the `Error` the
    /// page already words.
    pub async fn save_fact(
        &self,
        scope: &Scope,
        credential: &str,
        edit: &Edit,
    ) -> std::result::Result<(), WriteError> {
        if ![
            "parent_id",
            "legal_entity_type_id",
            "metadata",
            "scope_activities",
        ]
        .contains(&edit.field.as_str())
        {
            return Ok(self.save(credential, edit).await?);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("company.update"))
            .await?;
        organisation::authorize_target(tx.sql(), scope, edit.id, UPDATE).await?;
        // Parent changes serialize before the company row, matching lifecycle.
        // Otherwise two concurrent A→B / B→A changes could both pass the
        // ancestor check against an uncommitted snapshot.
        if edit.field == "parent_id" {
            organisation::lock_tree(tx.sql(), scope.tenant()).await?;
        }
        organisation::require_writable(tx.sql(), scope, edit.id, true)
            .await
            .map_err(|e| match e {
                crate::WriteError::Access(e) => e,
                _ => Error::Forbidden,
            })?;
        let row = tx
            .sql()
            .query_one(
                "SELECT version FROM companies WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &edit.id],
            )
            .await
            .map_err(Error::from)?;
        if row.get::<_, i64>(0) != edit.version {
            return Err(Error::Conflict.into());
        }
        let value = if ["metadata", "scope_activities"].contains(&edit.field.as_str()) {
            if edit.value.trim().is_empty() {
                Value::Null
            } else {
                let value: Value = serde_json::from_str(&edit.value).map_err(|_| Error::Invalid)?;
                if !(value.is_object()
                    || (edit.field == "scope_activities"
                        && value
                            .as_array()
                            .is_some_and(|a| a.iter().all(Value::is_string))))
                {
                    return Err(Error::Invalid.into());
                }
                value
            }
        } else {
            let id: Option<i64> = if edit.value.is_empty() {
                None
            } else {
                Some(edit.value.parse().map_err(|_| Error::Invalid)?)
            };
            if let Some(id) = id {
                if id <= 0 {
                    return Err(Error::Invalid.into());
                }
                if edit.field == "parent_id" {
                    organisation::authorize_target(tx.sql(), scope, id, UPDATE).await?;
                    organisation::require_writable(tx.sql(), scope, id, true)
                        .await
                        .map_err(write_error)?;
                    let max_depth = Policy::read(tx.sql(), scope).await?.max_depth;
                    if let Some(error) = hierarchy::check_parent(
                        tx.sql(),
                        scope.tenant(),
                        Some(edit.id),
                        id,
                        max_depth,
                    )
                    .await?
                    {
                        return Err(WriteError::Invalid(vec![error]));
                    }
                } else if tx
                    .sql()
                    .query_opt(
                        "SELECT id FROM company_legal_entity_types WHERE id=$1",
                        &[&id],
                    )
                    .await
                    .map_err(Error::from)?
                    .is_none()
                {
                    return Err(Error::Invalid.into());
                }
            }
            json!(id)
        };
        let field = edit.field.clone();
        let tenant = scope.tenant();
        let id = edit.id;
        tx.mutate(move |sql| Box::pin(async move {
            let old: Value = sql.query_one(&format!("SELECT to_jsonb({field}) FROM companies WHERE tenant_id=$1 AND id=$2"), &[&tenant,&id]).await?.get::<_,Option<Value>>(0).unwrap_or(Value::Null);
            if ["parent_id","legal_entity_type_id"].contains(&field.as_str()) {
                sql.execute(&format!("UPDATE companies SET {field}=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2"), &[&tenant,&id,&value.as_i64()]).await?;
            } else {
                sql.execute(&format!("UPDATE companies SET {field}=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2"), &[&tenant,&id,&Some(value.clone()).filter(|v| !v.is_null())]).await?;
            }
            Ok(((),vec![Change {kind:"company".into(),id,old:json!({&field:old}),new:json!({&field:value})}]))
        })).await?;
        Ok(tx.commit().await?)
    }
}

fn write_error(error: crate::WriteError) -> Error {
    match error {
        crate::WriteError::Access(error) => error,
        crate::WriteError::NotFound => Error::NotFound,
        _ => Error::Forbidden,
    }
}
