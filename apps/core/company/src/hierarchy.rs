//! A company's place in its tree (issue 139): the bounded read of its
//! ancestors and descendants another module draws, and the parent check every
//! write that sets a parent runs. Bilimbi refuses only a cycle; the depth
//! limit is Bilibi's, from the tenant's `company.hierarchy.max_depth`.
use crate::{contributions::VIEW, policy::Policy, FieldError};
use bilibi_authz::authorize_key;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use serde::Serialize;
use tokio_postgres::{GenericClient, Row};

/// Most nodes one read returns, ancestors and descendants together.
const MAX_NODES: usize = 1_000;
/// Bound on any walk of `parent_id`, which the writes keep acyclic.
const WALK: i32 = 1_000;

/// One company in a hierarchy read. `level` counts from the company read:
/// its parent is -1, its children 1.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HierarchyNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub code: String,
    pub status: String,
    pub level: i64,
}
impl HierarchyNode {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            parent_id: row.get("parent_id"),
            name: row.get("name"),
            code: row.get("code"),
            status: row.get("status"),
            level: row.get::<_, i32>("level").into(),
        }
    }
}
/// A company with its ancestors (root first) and descendants (by level, then
/// name), each within `max_depth` levels and together at most 1,000 nodes;
/// `truncated` says that more exist.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hierarchy {
    pub company: HierarchyNode,
    pub ancestors: Vec<HierarchyNode>,
    pub descendants: Vec<HierarchyNode>,
    pub max_depth: i64,
    pub truncated: bool,
}

/// The hierarchy around one live company of the tenant (`admin.company.view`).
/// Soft-deleted companies are left out, and a walk stops at one.
pub async fn read(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: i64,
) -> Result<Hierarchy> {
    authorize_key(client, scope, VIEW).await?;
    if id <= 0 {
        return Err(Error::NotFound);
    }
    let max_depth = Policy::read(client, scope).await?.max_depth;
    let tenant = scope.tenant();
    let company = client
        .query_opt(
            "SELECT id,parent_id,name,code,status,0 AS level FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&tenant, &id],
        )
        .await?
        .map(|row| HierarchyNode::from_row(&row))
        .ok_or(Error::NotFound)?;
    let (ancestors, mut truncated) = ancestors(client, tenant, id, max_depth).await?;
    // One level more than a tree within the limit can have is read, to know
    // that more exist.
    let beyond = (max_depth as i32).min(WALK);
    let budget = MAX_NODES.saturating_sub(ancestors.len());
    let mut descendants: Vec<HierarchyNode> = client
        .query(
            "WITH RECURSIVE down(id, level) AS (SELECT id,0 FROM companies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT c.id,d.level+1 FROM companies c JOIN down d ON c.parent_id=d.id WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND d.level < $3) SELECT c.id,c.parent_id,c.name,c.code,c.status,d.level FROM down d JOIN companies c ON c.tenant_id=$1 AND c.id=d.id WHERE d.level > 0 ORDER BY d.level, lower(c.name), c.id LIMIT $4",
            &[&tenant, &id, &beyond, &(budget as i64 + 1)],
        )
        .await?
        .iter()
        .map(HierarchyNode::from_row)
        .collect();
    let within = descendants.len();
    descendants.retain(|node| node.level < max_depth);
    if descendants.len() < within || descendants.len() > budget {
        truncated = true;
        descendants.truncate(budget);
    }
    Ok(Hierarchy {
        company,
        ancestors,
        descendants,
        max_depth,
        truncated,
    })
}

/// A company's ancestors, root first, within the `max_depth - 1` levels a tree
/// within the limit can have above it; `true` when more exist.
pub(crate) async fn ancestors(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
    max_depth: i64,
) -> Result<(Vec<HierarchyNode>, bool)> {
    let beyond = (max_depth as i32).min(WALK);
    let mut ancestors: Vec<HierarchyNode> = client
        .query(
            "WITH RECURSIVE up(id, parent_id, level) AS (SELECT id,parent_id,0 FROM companies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT c.id,c.parent_id,u.level-1 FROM companies c JOIN up u ON c.id=u.parent_id WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND u.level > -$3::int) SELECT c.id,c.parent_id,c.name,c.code,c.status,u.level FROM up u JOIN companies c ON c.tenant_id=$1 AND c.id=u.id WHERE u.level < 0 ORDER BY u.level",
            &[&tenant, &id, &beyond],
        )
        .await?
        .iter()
        .map(HierarchyNode::from_row)
        .collect();
    let allowed = (max_depth - 1) as usize;
    let truncated = ancestors.len() > allowed;
    if truncated {
        ancestors.drain(..ancestors.len() - allowed);
    }
    Ok((ancestors, truncated))
}

/// Number of companies from the root down to `id`, both included, and whether
/// `member` is one of them.
async fn chain(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
    member: Option<i64>,
) -> Result<(i64, bool)> {
    let row = client
        .query_one(
            "WITH RECURSIVE up(id, parent_id, n) AS (SELECT id,parent_id,1 FROM companies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT c.id,c.parent_id,u.n+1 FROM companies c JOIN up u ON c.id=u.parent_id WHERE c.tenant_id=$1 AND u.n < $4) SELECT COALESCE(max(n),0)::bigint, COALESCE(bool_or(id=$3),false) FROM up",
            &[&tenant, &id, &member, &WALK],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

/// Refuses a parent that would make a loop or a tree deeper than `max_depth`
/// levels, for a new company (`moving` absent) or a company moving under
/// `parent` with its subtree. A tree already deeper stays valid: only a change
/// that takes its deepest company further down is refused. The caller holds
/// the tenant row lock, so concurrent parent changes cannot both pass.
pub(crate) async fn check_parent(
    client: &impl GenericClient,
    tenant: i64,
    moving: Option<i64>,
    parent: i64,
    max_depth: i64,
) -> Result<Option<FieldError>> {
    if moving == Some(parent) {
        return Ok(Some(FieldError::new(
            "parent_id",
            "cannot be the company itself",
        )));
    }
    let (parent_depth, cycle) = chain(client, tenant, parent, moving).await?;
    if cycle {
        return Ok(Some(FieldError::new(
            "parent_id",
            "cannot be one of its own subsidiaries",
        )));
    }
    let (height, before) = match moving {
        None => (1, 0),
        Some(id) => {
            let height: i64 = client
                .query_one(
                    "WITH RECURSIVE down(id, n) AS (SELECT id,1 FROM companies WHERE tenant_id=$1 AND id=$2 UNION ALL SELECT c.id,d.n+1 FROM companies c JOIN down d ON c.parent_id=d.id WHERE c.tenant_id=$1 AND d.n < $3) SELECT COALESCE(max(n),1)::bigint FROM down",
                    &[&tenant, &id, &WALK],
                )
                .await?
                .get(0);
            let (depth, _) = chain(client, tenant, id, None).await?;
            (height, depth + height - 1)
        }
    };
    let after = parent_depth + height;
    Ok((after > max_depth && after > before).then(|| {
        FieldError::new(
            "parent_id",
            format!("would make the company tree deeper than {max_depth} levels"),
        )
    }))
}
