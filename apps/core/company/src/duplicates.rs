//! Duplicate candidates for a company (issue 139; Person's `duplicates` is the
//! model, Bilimbi has no counterpart yet). It proposes, never chooses: a read
//! returns the existing companies a new one looks like, scored with the facts
//! that matched, and a create that goes ahead anyway carries its reason into
//! the audit trail. Matching uses the stored keys of `008-match-keys`; the
//! name similarity is computed here over a bounded preselection.
use crate::{
    contributions::LIST,
    organisation::{strip_accent, ValidCompany, COLUMNS},
    policy::Policy,
    Company, FieldError,
};
use bilibi_authz::{authorize_key, restrictions};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use tokio_postgres::GenericClient;

/// Rows the name words preselect before similarity is computed.
const PRESELECT: i64 = 200;

/// The facts a duplicate read matches on. At least one is needed; `except_id`
/// leaves out the company being edited; `limit` is 1–100 (default 10).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DuplicateQuery {
    pub name: Option<String>,
    pub legal_name: Option<String>,
    pub registration_number: Option<String>,
    pub website: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub except_id: Option<i64>,
    pub limit: i64,
}
impl Default for DuplicateQuery {
    fn default() -> Self {
        Self {
            name: None,
            legal_name: None,
            registration_number: None,
            website: None,
            email: None,
            phone: None,
            except_id: None,
            limit: 10,
        }
    }
}

/// One fact that matched and the score it alone gives.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Matched {
    /// `registration_number`, `website`, `email_domain`, `phone` or `name`.
    pub fact: &'static str,
    pub score: f64,
}
/// An existing company a new one looks like: the company as the reader may
/// see it (an archived one keeps its status), its best score and every fact
/// at or above the tenant's threshold.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub company: Company,
    pub score: f64,
    pub matched: Vec<Matched>,
}
impl Candidate {
    /// The agent and audit shape, restricted fields still carrying the mask.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.company.id, "name": self.company.name, "code": self.company.code,
            "legal_name": self.company.legal_name, "status": self.company.status,
            "archived": self.company.archived(), "registration_number": self.company.registration_number,
            "email": self.company.email, "phone": self.company.phone, "website": self.company.website,
            "score": self.score, "matched": self.matched,
            "path": format!("/companies/{}", self.company.id),
        })
    }
}

/// Why a company is created although it looks like existing ones: the
/// candidates the creator reviewed and a reason of 1–500 characters. It is
/// kept in the create's audit change.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CreateAnyway {
    pub candidate_ids: Vec<i64>,
    pub reason: String,
}

/// The facts compared, trimmed; blank is absent.
struct Criteria<'a> {
    name: Option<&'a str>,
    legal_name: Option<&'a str>,
    registration_number: Option<&'a str>,
    website: Option<&'a str>,
    email: Option<&'a str>,
    phone: Option<&'a str>,
}
fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// Candidates for a prospective or existing company (`admin.company.list`).
/// A criterion on a field the reader's role restricts is refused, never
/// searched, so a hidden value cannot be probed; restricted values in the
/// results carry the mask. Archived companies are included.
pub async fn duplicates(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    query: &DuplicateQuery,
) -> Result<Vec<Candidate>> {
    authorize_key(client, scope, LIST).await?;
    let criteria = Criteria {
        name: present(&query.name),
        legal_name: present(&query.legal_name),
        registration_number: present(&query.registration_number),
        website: present(&query.website),
        email: present(&query.email),
        phone: present(&query.phone),
    };
    let bounded = |value: Option<&str>, max: usize| {
        value.is_some_and(|v| v.chars().count() > max || v.contains('\0'))
    };
    if !(1..=100).contains(&query.limit)
        || query.except_id.is_some_and(|id| id <= 0)
        || bounded(criteria.name, 160)
        || bounded(criteria.legal_name, 255)
        || bounded(criteria.registration_number, 255)
        || bounded(criteria.website, 255)
        || bounded(criteria.email, 254)
        || bounded(criteria.phone, 64)
        || [
            criteria.name,
            criteria.legal_name,
            criteria.registration_number,
            criteria.website,
            criteria.email,
            criteria.phone,
        ]
        .iter()
        .all(Option::is_none)
    {
        return Err(Error::Invalid);
    }
    let masked = restrictions(client, scope).await?;
    for (field, value) in [
        ("registration_number", criteria.registration_number),
        ("website", criteria.website),
        ("email", criteria.email),
        ("phone", criteria.phone),
    ] {
        if value.is_some() && masked.contains(field) {
            return Err(Error::Forbidden);
        }
    }
    let policy = Policy::read(client, scope).await?;
    find(
        client,
        scope,
        &policy,
        &criteria,
        query.except_id,
        query.limit,
        &masked,
    )
    .await
}

/// The candidates for a validated create, on the create's own connection or
/// transaction. The create already refused any field the actor may not set.
pub(crate) async fn for_company(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    policy: &Policy,
    company: &ValidCompany,
    masked: &HashSet<String>,
) -> Result<Vec<Candidate>> {
    let criteria = Criteria {
        name: Some(company.name.as_str()),
        legal_name: company.legal_name.as_deref(),
        registration_number: company.registration_number.as_deref(),
        website: company.website.as_deref(),
        email: Some(company.email.as_str()).filter(|v| !v.is_empty()),
        phone: Some(company.phone.as_str()).filter(|v| !v.is_empty()),
    };
    find(client, scope, policy, &criteria, None, 100, masked).await
}

async fn find(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    policy: &Policy,
    criteria: &Criteria<'_>,
    except: Option<i64>,
    limit: i64,
    masked: &HashSet<String>,
) -> Result<Vec<Candidate>> {
    // The criteria are normalised by the same functions as the stored keys.
    let ignored: Vec<&String> = policy.ignored_words.iter().collect();
    let keys = client
        .query_one(
            "SELECT company_match_registration($1), company_match_host($2), company_match_domain($3), company_match_digits($4), ARRAY(SELECT DISTINCT t FROM unnest(company_match_tokens(concat_ws(' ', $5::text, $6::text))) t WHERE length(t) >= 2 AND t <> ALL($7::text[]) ORDER BY t)",
            &[
                &criteria.registration_number,
                &criteria.website,
                &criteria.email,
                &criteria.phone,
                &criteria.name,
                &criteria.legal_name,
                &ignored,
            ],
        )
        .await?;
    let registration: Option<String> = keys.get(0);
    let host: Option<String> = keys.get(1);
    // Anyone can hold an address at a public mail domain: it identifies no one.
    let domain: Option<String> = keys
        .get::<_, Option<String>>(2)
        .filter(|domain| !policy.public_mail_domains.contains(domain));
    let digits: Option<String> = keys.get(3);
    let tokens: Vec<String> = keys.get(4);
    if registration.is_none()
        && host.is_none()
        && domain.is_none()
        && digits.is_none()
        && tokens.is_empty()
    {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            &format!(
                "SELECT {COLUMNS}, COALESCE(registration_key=$2,false) AS same_registration, COALESCE(website_host=$3,false) AS same_website, COALESCE(email_domain=$4,false) AS same_email_domain, COALESCE(phone_digits=$5,false) AS same_phone \
                 FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND id IS DISTINCT FROM $7 \
                 AND (registration_key=$2 OR website_host=$3 OR email_domain=$4 OR phone_digits=$5 OR name_tokens && $6::text[]) \
                 ORDER BY (COALESCE(registration_key=$2,false) OR COALESCE(website_host=$3,false) OR COALESCE(email_domain=$4,false) OR COALESCE(phone_digits=$5,false)) DESC, \
                 cardinality(ARRAY(SELECT unnest(name_tokens) INTERSECT SELECT unnest($6::text[]))) DESC, id LIMIT {PRESELECT}"
            ),
            &[
                &scope.tenant(),
                &registration,
                &host,
                &domain,
                &digits,
                &tokens,
                &except,
            ],
        )
        .await?;
    let names: Vec<&str> = [criteria.name, criteria.legal_name]
        .into_iter()
        .flatten()
        .collect();
    let mut candidates = Vec::new();
    for row in &rows {
        let company = Company::from_row(row, masked);
        let mut matched = Vec::new();
        for (column, fact, score) in [
            ("same_registration", "registration_number", 1.0),
            ("same_website", "website", 0.9),
            ("same_email_domain", "email_domain", 0.9),
            ("same_phone", "phone", 0.85),
        ] {
            if row.get::<_, bool>(column) {
                matched.push(Matched { fact, score });
            }
        }
        let name_score = names
            .iter()
            .flat_map(|query| {
                [Some(company.name.as_str()), company.legal_name.as_deref()]
                    .into_iter()
                    .flatten()
                    .map(|name| similarity(query, name, &policy.ignored_words))
            })
            .fold(0.0, f64::max);
        if name_score > 0.0 {
            matched.push(Matched {
                fact: "name",
                score: name_score,
            });
        }
        matched.retain(|fact| fact.score >= policy.threshold);
        matched.sort_by(|a, b| b.score.total_cmp(&a.score));
        if let Some(best) = matched.first() {
            candidates.push(Candidate {
                score: best.score,
                company,
                matched,
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                a.company
                    .name
                    .to_lowercase()
                    .cmp(&b.company.name.to_lowercase())
            })
            .then(a.company.id.cmp(&b.company.id))
    });
    candidates.truncate(limit as usize);
    Ok(candidates)
}

/// A character lower-cased and stripped of its Latin accent, as names compare.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(strip_accent)
        .collect()
}
/// The words of a name that identify it: letters and digits, folded, without
/// the tenant's ignored words.
fn words(name: &str, ignored: &HashSet<String>) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(fold)
        .filter(|word| !ignored.contains(word))
        .collect()
}
/// `pg_trgm`'s similarity: each word padded with two spaces before and one
/// after, the shared trigrams over all trigrams of both, rounded to 0.01.
fn similarity(a: &str, b: &str, ignored: &HashSet<String>) -> f64 {
    let trigrams = |name: &str| -> HashSet<[char; 3]> {
        let mut set = HashSet::new();
        for word in words(name, ignored) {
            let padded: Vec<char> = "  ".chars().chain(word.chars()).chain([' ']).collect();
            for window in padded.windows(3) {
                set.insert([window[0], window[1], window[2]]);
            }
        }
        set
    };
    let (a, b) = (trigrams(a), trigrams(b));
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    (a.intersection(&b).count() as f64 / union as f64 * 100.0).round() / 100.0
}

/// Serializes creates of the same normalised name within a tenant for the
/// rest of the transaction, so a second one sees the first as a candidate.
pub(crate) async fn lock_name(
    client: &impl GenericClient,
    tenant: i64,
    policy: &Policy,
    name: &str,
) -> Result<()> {
    let mut key = words(name, &policy.ignored_words).join(" ");
    if key.is_empty() {
        key = fold(name.trim());
    }
    client
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('company-name-' || $1::text || ':' || $2, 0))",
            &[&tenant.to_string(), &key],
        )
        .await?;
    Ok(())
}

/// The create-anyway as the audit keeps it: a trimmed reason and the
/// candidate ids sorted once each, or the field error that refuses it.
pub(crate) fn normalise(anyway: &CreateAnyway) -> std::result::Result<CreateAnyway, FieldError> {
    let reason = anyway.reason.trim().to_owned();
    if reason.is_empty() || reason.chars().count() > 500 || reason.contains('\0') {
        return Err(FieldError::new(
            "create_anyway",
            "needs a reason of 1 to 500 characters",
        ));
    }
    let ids: BTreeSet<i64> = anyway.candidate_ids.iter().copied().collect();
    if ids.is_empty() || ids.len() > 100 || ids.iter().any(|id| *id <= 0) {
        return Err(FieldError::new(
            "create_anyway",
            "must name 1 to 100 candidate companies by id",
        ));
    }
    Ok(CreateAnyway {
        candidate_ids: ids.into_iter().collect(),
        reason,
    })
}

/// `require_reason`: a create that matches existing companies goes ahead only
/// with a reason naming every candidate. The refusal names the candidates'
/// ids only to a reader who may list companies.
pub(crate) fn reason_missing(
    candidates: &[Candidate],
    anyway: Option<&CreateAnyway>,
    can_list: bool,
) -> Option<FieldError> {
    if candidates.is_empty() {
        return None;
    }
    let named: HashSet<i64> = anyway
        .map(|anyway| anyway.candidate_ids.iter().copied().collect())
        .unwrap_or_default();
    let missing: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.company.id)
        .filter(|id| !named.contains(id))
        .map(|id| id.to_string())
        .collect();
    if missing.is_empty() && anyway.is_some() {
        return None;
    }
    Some(FieldError::new(
        "create_anyway",
        match (anyway.is_some(), can_list) {
            (false, true) => format!(
                "needs a reason, because this company looks like existing companies {}",
                missing.join(", ")
            ),
            (false, false) => {
                "needs a reason, because this company looks like an existing company".into()
            }
            (true, true) => format!(
                "must name every likely duplicate; also review companies {}",
                missing.join(", ")
            ),
            (true, false) => "must name every likely duplicate".into(),
        },
    ))
}
