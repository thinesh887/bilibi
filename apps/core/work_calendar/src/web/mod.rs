//! The administration pages (`core/work_calendar.pages`, issue 209): the
//! calendars list, a calendar read first and edited in place, its month of
//! resolved days, and the holiday sets list and record. No Bilimbi
//! counterpart; the pages follow `base/picklists`' (lists, in-place edits and
//! confirmations through the contracts), and show what Belimbing's work
//! calendar settings show.
//!
//! Every change goes through the `core.work_calendar.*` contracts agents call
//! (`PageContracts::submit`), which recheck the capability and the version in
//! their own transaction. A control the person may not use is hidden; a
//! refused event is a flash on the page, a refused in-place edit an outcome
//! on its field. Retire and removals confirm through the shared dialog,
//! which holds what the person picked, never a client value.
mod calendar;
mod calendars;
mod month;
mod sets;
mod week;

use crate::{
    model::{Calendar, Rule},
    WorkCalendars, INSTALLATION_MANAGE, LIST, MANAGE,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    FlashMap, Html, PageVariant, StatusKind, NO_FLASH,
};
use jiff::civil::Date;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

pub(crate) const PAGES: &str = "core/work_calendar.pages";
pub(crate) const CALENDARS: &str = "/admin/work-calendars";
pub(crate) const CALENDAR: &str = "/admin/work-calendars/{code}";
pub(crate) const MONTH: &str = "/admin/work-calendars/{code}/month";
pub(crate) const SETS: &str = "/admin/holiday-sets";
pub(crate) const SET: &str = "/admin/holiday-sets/{code}";

const DENIED: &str = "You do not have permission to change work calendars.";
const STALE: &str =
    "This changed since you opened it. Reload the page to see the latest version, then try again.";
const GONE: &str = "This no longer exists; reload the page.";
const NOT_SAVED: &str =
    "The change was not saved. Try again, and tell your administrator if it keeps failing.";

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub calendars: Arc<WorkCalendars>,
    pub contracts: Arc<PageContracts>,
}

/// A contract's answer as a page reports it.
enum Answer {
    Done,
    /// The refused fields and their reasons, for the form that sent them.
    Fields(BTreeMap<String, Vec<String>>),
    /// The sentence to show.
    Refused(String),
}

impl Pages {
    async fn submit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Answer> {
        let call = Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract,
        };
        Ok(match self.contracts.submit(&call, input).await {
            Ok(_) => Answer::Done,
            Err(ContractError::InvalidInput(fields)) => {
                Answer::Fields(fields.into_iter().collect())
            }
            // Base Step-up and a lost session: the host prompts and comes back.
            Err(ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => return Err(error),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                Answer::Refused(DENIED.into())
            }
            Err(ContractError::Backend(Error::Conflict) | ContractError::StalePlan) => {
                Answer::Refused(STALE.into())
            }
            Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => {
                Answer::Refused(GONE.into())
            }
            Err(_) => Answer::Refused(NOT_SAVED.into()),
        })
    }
}

/// Refused fields as one sentence, for a flash: "code is already used…".
fn sentence(fields: &BTreeMap<String, Vec<String>>) -> String {
    fields
        .iter()
        .flat_map(|(field, messages)| {
            let field = week::label(field);
            messages
                .iter()
                .map(move |message| format!("{field} {message}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// What the person may do here, read once per render. The contracts decide
/// again in their transaction; this only hides controls.
struct Standing(BTreeSet<&'static str>);
impl Standing {
    async fn load(pages: &Pages, scope: &Scope) -> Result<Self> {
        let client = pages.db.get().await?;
        Ok(Self(effective_capabilities(&**client, scope).await?))
    }
    fn manages(&self) -> bool {
        self.0.contains(MANAGE)
    }
    fn lists(&self) -> bool {
        self.0.contains(LIST)
    }
    /// Keeps installation-wide holiday sets: a platform capability, which
    /// only the platform operator's tenant holds.
    fn keeps_installation(&self) -> bool {
        self.manages() && self.0.contains(INSTALLATION_MANAGE)
    }
}

type Flash = Option<(StatusKind, String)>;
fn flash_map(flash: &Flash) -> FlashMap<'_> {
    let mut map = NO_FLASH;
    if let Some((kind, message)) = flash {
        match kind {
            StatusKind::Success => map.success = message,
            StatusKind::Error => map.error = message,
            StatusKind::Warning => map.warning = message,
            StatusKind::Info => map.info = message,
        }
    }
    map
}
/// The shared dismiss, kept for every page that shows a flash.
fn clear_flash(flash: &mut Flash, key: Option<&str>) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "work_calendar.html")]
struct View<'a> {
    id: &'a str,
    variant: PageVariant,
    flash: FlashMap<'a>,
    header: Html,
    parts: Vec<Html>,
    dialog: Html,
}
impl View<'_> {
    fn html(&self) -> Result<String> {
        self.render().map_err(|_| Error::Internal)
    }
}

/// A dialog's form as typed, with the contract's refusals by field.
#[derive(Default)]
struct Draft {
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}
impl Draft {
    fn value(&self, key: &str) -> String {
        self.values.get(key).cloned().unwrap_or_default()
    }
    fn errors(&self, key: &str) -> Vec<String> {
        self.errors.get(key).cloned().unwrap_or_default()
    }
    /// Takes a refusal: each field error lands on its form field (`key`
    /// maps a contract field to it), and the rest is the returned sentence.
    fn refuse(
        &mut self,
        fields: BTreeMap<String, Vec<String>>,
        key: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let mut rest = BTreeMap::new();
        self.errors.clear();
        for (field, messages) in fields {
            match key(&field) {
                Some(form) => self.errors.entry(form).or_default().extend(messages),
                None => {
                    rest.insert(field, messages);
                }
            }
        }
        (!rest.is_empty()).then(|| sentence(&rest))
    }
}

/// A route with its `{code}`.
fn path(route: &str, code: &str) -> String {
    route.replace("{code}", code)
}
/// Today in `zone` (a calendar's or the company's), for a default.
fn today_in(zone: &str) -> Date {
    jiff::Timestamp::now()
        .in_tz(zone)
        .unwrap_or_else(|_| jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC))
        .date()
}
/// The rule version in force on `date`: the last that started by then, or
/// the first, which also governs earlier dates.
fn in_force(rules: &[Rule], date: Date) -> Option<&Rule> {
    rules
        .iter()
        .rev()
        .find(|rule| rule.effective_from <= date)
        .or(rules.first())
}
/// Today in the calendar's own zone: the zone of the week in force.
fn calendar_today(calendar: &Calendar) -> Date {
    let zone = in_force(&calendar.rules.rows, today_in("UTC"))
        .map_or("UTC", |rule| rule.timezone.as_str());
    today_in(zone)
}
/// "Monday 31 August 2026".
fn long_date(date: Date) -> String {
    date.strftime("%A %-d %B %Y").to_string()
}
fn code(request: &PageRequest<'_, Scope>) -> Result<String> {
    request
        .path
        .get("code")
        .filter(|code| crate::model::valid_code(code))
        .cloned()
        .ok_or(Error::NotFound)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "calendars" => Some("Work calendars"),
            "sets" => Some("Holiday sets"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let (html, title) = match request.operation {
                "calendars" => (
                    calendars::Index::new(self.clone(), request.query, request.framed)?
                        .render(scope)
                        .await?,
                    None,
                ),
                "calendar" => {
                    let mut page =
                        calendar::Record::new(self.clone(), code(&request)?, request.query)?;
                    let html = page.render(scope).await?;
                    (html, Some(page.title()))
                }
                "month" => {
                    let mut page = month::Month::new(self.clone(), code(&request)?, request.query);
                    let html = page.render(scope).await?;
                    (html, Some(page.title()))
                }
                "sets" => (
                    sets::Index::new(self.clone(), request.query, request.framed)?
                        .render(scope)
                        .await?,
                    None,
                ),
                "set" => {
                    let mut page = sets::Record::new(self.clone(), code(&request)?, request.query)?;
                    let html = page.render(scope).await?;
                    (html, Some(page.title()))
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "calendars" => Box::new(calendars::Index::new(
                    self.clone(),
                    request.query,
                    request.framed,
                )?),
                "calendar" => Box::new(calendar::Record::new(
                    self.clone(),
                    code(&request)?,
                    request.query,
                )?),
                "month" => Box::new(month::Month::new(
                    self.clone(),
                    code(&request)?,
                    request.query,
                )),
                "sets" => Box::new(sets::Index::new(
                    self.clone(),
                    request.query,
                    request.framed,
                )?),
                "set" => Box::new(sets::Record::new(
                    self.clone(),
                    code(&request)?,
                    request.query,
                )?),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}
