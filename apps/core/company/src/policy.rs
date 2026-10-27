//! The operator's duplicate and hierarchy settings (issue 139): how alike a
//! company must be to be proposed, whether creating one anyway needs a
//! reason, the words and mail domains that never identify a company, and how
//! deep a company tree may grow. Each is a tenant setting over a global
//! default, edited on System Settings under "Companies".
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::{ReadClient, Scope};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

pub const THRESHOLD: &str = "company.duplicates.threshold";
pub const ON_CREATE: &str = "company.duplicates.on_create";
pub const IGNORED_WORDS: &str = "company.duplicates.ignored_words";
pub const PUBLIC_MAIL_DOMAINS: &str = "company.duplicates.public_mail_domains";
pub const MAX_DEPTH: &str = "company.hierarchy.max_depth";
const KEYS: [&str; 5] = [
    THRESHOLD,
    ON_CREATE,
    IGNORED_WORDS,
    PUBLIC_MAIL_DOMAINS,
    MAX_DEPTH,
];

/// The settings store, bound once by the `core/company.companies` factory so
/// a create on any caller's transaction reads the same policy.
static SETTINGS: OnceLock<Arc<Settings>> = OnceLock::new();
pub fn bind(settings: Arc<Settings>) -> bilibi_module_registry::Result<()> {
    SETTINGS.set(settings).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate company policy binding".into())
    })
}

/// The tenant's policy as it stands on the caller's connection or transaction.
#[derive(Clone, Debug)]
pub struct Policy {
    pub threshold: f64,
    /// `require_reason`: a create that matches existing companies is refused
    /// unless a reason names every candidate.
    pub require_reason: bool,
    pub ignored_words: HashSet<String>,
    pub public_mail_domains: HashSet<String>,
    pub max_depth: i64,
}
impl Policy {
    pub async fn read(client: &dyn ReadClient, scope: &Scope) -> Result<Self> {
        let settings = SETTINGS.get().ok_or(Error::Internal)?;
        let mut values = settings
            .resolve_many(client, scope, Target::Tenant, &KEYS)
            .await?;
        let mut take = |key: &str| {
            values
                .remove(key)
                .map(|resolved| resolved.value)
                .unwrap_or(Value::Null)
        };
        let words = |value: Value| -> HashSet<String> {
            value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|word| word.trim().to_lowercase())
                .filter(|word| !word.is_empty())
                .collect()
        };
        Ok(Self {
            threshold: take(THRESHOLD).as_f64().unwrap_or(0.8).clamp(0.5, 1.0),
            require_reason: take(ON_CREATE).as_str() == Some("require_reason"),
            ignored_words: words(take(IGNORED_WORDS))
                .iter()
                .map(|word| crate::duplicates::fold(word))
                .collect(),
            public_mail_domains: words(take(PUBLIC_MAIL_DOMAINS)),
            max_depth: take(MAX_DEPTH).as_i64().unwrap_or(10).clamp(2, 20),
        })
    }
}

fn list_valid(value: &Value) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() <= 200
            && items.iter().all(|item| {
                item.as_str()
                    .is_some_and(|text| !text.trim().is_empty() && text.chars().count() <= 255)
            })
    })
}
fn on_create_valid(value: &Value) -> bool {
    matches!(value.as_str(), Some("suggest" | "require_reason"))
}
const SCOPES: &[ScopeKind] = &[ScopeKind::Global, ScopeKind::Tenant];
const fn editor(label: &'static str, help: &'static str) -> Option<Editor> {
    Some(Editor {
        group: "Companies",
        label,
        help,
        capability: None,
        reveal_duration_ms: 5_000,
    })
}
macro_rules! setting {
    ($key:expr, $definition:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, Provenance { module: "core/company", source: file!(), line: line!() }, &Setting::Definition($definition)) }
    };
}
setting!(
    THRESHOLD,
    Definition {
        value_type: ValueType::Float,
        scopes: SCOPES,
        default: || json!(0.8),
        nullable: false,
        encrypted: false,
        minimum: Some(0.5),
        maximum: Some(1.0),
        validator: None,
        editor: editor(
            "Duplicate match score",
            "How alike, from 0.5 to 1, an existing company must be to be proposed as a duplicate. The same registration number scores 1, the same website or email domain 0.9, the same phone number 0.85."
        ),
    }
);
setting!(
    ON_CREATE,
    Definition {
        value_type: ValueType::String,
        scopes: SCOPES,
        default: || json!("suggest"),
        nullable: false,
        encrypted: false,
        minimum: None,
        maximum: None,
        validator: Some(Validator {
            accepts: on_create_valid,
            message: "must be suggest or require_reason",
        }),
        editor: editor(
            "When a new company looks like an existing one",
            "suggest shows the likely duplicates and lets the company be created; require_reason refuses it until someone gives a reason to create it anyway."
        ),
    }
);
setting!(
    IGNORED_WORDS,
    Definition {
        value_type: ValueType::Array,
        scopes: SCOPES,
        default: || json!(["sdn", "bhd", "ltd", "limited", "inc", "llc", "plc", "pte", "pty", "gmbh", "corp"]),
        nullable: false,
        encrypted: false,
        minimum: None,
        maximum: None,
        validator: Some(Validator {
            accepts: list_valid,
            message: "must be a list of words, such as [\"sdn\", \"bhd\"]",
        }),
        editor: editor(
            "Words ignored when comparing names",
            "Legal suffixes and other words left out before company names are compared, for example [\"sdn\", \"bhd\"]."
        ),
    }
);
setting!(
    PUBLIC_MAIL_DOMAINS,
    Definition {
        value_type: ValueType::Array,
        scopes: SCOPES,
        default: || json!([
            "gmail.com", "googlemail.com", "yahoo.com", "ymail.com", "hotmail.com", "outlook.com",
            "live.com", "msn.com", "icloud.com", "me.com", "aol.com", "proton.me",
            "protonmail.com", "gmx.com", "mail.com", "yandex.com", "zoho.com", "qq.com",
            "163.com", "126.com"
        ]),
        nullable: false,
        encrypted: false,
        minimum: None,
        maximum: None,
        validator: Some(Validator {
            accepts: list_valid,
            message: "must be a list of domains, such as [\"gmail.com\"]",
        }),
        editor: editor(
            "Public mail domains",
            "Email domains anyone can sign up for. Two companies sharing one of these are not proposed as duplicates, for example [\"gmail.com\"]."
        ),
    }
);
setting!(
    MAX_DEPTH,
    Definition {
        value_type: ValueType::Integer,
        scopes: SCOPES,
        default: || json!(10),
        nullable: false,
        encrypted: false,
        minimum: Some(2.0),
        maximum: Some(20.0),
        validator: None,
        editor: editor(
            "Company tree depth",
            "Most levels a company tree may have, from 2 to 20. A parent change that would go deeper is refused; trees already deeper stay as they are."
        ),
    }
);
