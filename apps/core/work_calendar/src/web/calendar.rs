//! One calendar (`/admin/work-calendars/{code}`), read first and edited in
//! place: its name and substitute rule, its weekly rule versions, the holiday
//! sets it includes and its dated exceptions. Retiring it and removing an
//! exception confirm; reactivating it and leaving a set out do not, since
//! both are undone as easily. Every write names the version this page read,
//! so a change made elsewhere since is refused with the reload message.
use super::{
    calendar_today,
    calendars::{status_badge, substitutes},
    clear_flash, flash_map, in_force, long_date, path, render, today_in, week, Answer, Draft,
    Flash, Pages, Standing, View, CALENDARS, MONTH, SET,
};
use crate::model::{
    Calendar, DayType, Exception, HolidaySetEntry, Listing, MAX_PAGE_SIZE, MAX_ROWS,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    lists::RecordLink,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, ActionLink, BackLink, BuiltinColumn, Button, ButtonVariant, Card, Choice,
    ColumnsOutcome, CommitStates, CommitStatus, ConfirmDialog, Fact, Facts, Field, FlexRow,
    FlexTable, Form, Header, Html, IconButton, IconKind, InlineChoice, InlineEdit, InlineField,
    InputKind, ListColumns, Modal, ModalWidth, PageCounts, PageVariant, Pagination, SectionHeading,
    StatusKind, TableEmpty,
};
use serde_json::{json, Value};
use std::collections::HashMap;

const EXCEPTION_PAGE: [u64; 1] = [MAX_ROWS as u64];

fn rule_columns() -> Vec<BuiltinColumn> {
    let mut columns = vec![
        BuiltinColumn::new("from", "From"),
        BuiltinColumn::new("zone", "Time zone"),
    ];
    for (day, name) in [
        ("monday", "Monday"),
        ("tuesday", "Tuesday"),
        ("wednesday", "Wednesday"),
        ("thursday", "Thursday"),
        ("friday", "Friday"),
        ("saturday", "Saturday"),
        ("sunday", "Sunday"),
    ] {
        columns.push(BuiltinColumn::new(day, name));
    }
    columns
}
fn set_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("set", "Holiday set"),
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("keeper", "Kept by"),
    ]
}
fn exception_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("name", "Exception"),
        BuiltinColumn::new("from", "From"),
        BuiltinColumn::new("to", "To"),
        BuiltinColumn::new("type", "Day type"),
        BuiltinColumn::new("hours", "Hours"),
    ]
}

/// One write the page sends: its event, the contract it calls, the input and
/// the message for success.
struct Request {
    event: &'static str,
    contract: &'static str,
    input: Value,
    done: String,
}
/// A dialog's form and where each contract field lands on it.
type Refusals = (Draft, fn(&str, &HashMap<String, String>) -> Option<String>);

/// A form dialog's frame.
struct Frame<'a> {
    id: &'a str,
    title: &'a str,
    description: &'a str,
    event: &'a str,
    submit: &'a str,
    width: ModalWidth,
}

enum Dialog {
    Rule(Draft),
    /// The draft and every set the calendar may include, read when opened.
    Include(Draft, Vec<HolidaySetEntry>),
    Exception(Draft),
    Retire,
    Remove(Exception),
}

pub(super) struct Record {
    pages: Pages,
    code: String,
    /// The page of exceptions shown, `MAX_ROWS` to a page.
    page: u64,
    /// The calendar as last read: its version fences every write.
    calendar: Option<Calendar>,
    rules: ListColumns,
    sets: ListColumns,
    exceptions: ListColumns,
    rows_changed: bool,
    statuses: CommitStates,
    notice: String,
    flash: Flash,
    dialog: Option<Dialog>,
}

impl Record {
    pub(super) fn new(pages: Pages, code: String, query: &HashMap<String, String>) -> Result<Self> {
        let mount = |id, columns| ListColumns::mount(id, columns).map_err(|_| Error::Internal);
        Ok(Self {
            pages,
            code,
            page: page(query),
            calendar: None,
            rules: mount("work-calendar-rules", rule_columns())?,
            sets: mount("work-calendar-sets", set_columns())?,
            exceptions: mount("work-calendar-exceptions", exception_columns())?,
            rows_changed: true,
            statuses: CommitStates::default(),
            notice: String::new(),
            flash: None,
            dialog: None,
        })
    }
    pub(super) fn title(&self) -> String {
        self.calendar
            .as_ref()
            .map_or_else(|| self.code.clone(), |calendar| calendar.name.clone())
    }
    fn version(&self) -> i64 {
        self.calendar
            .as_ref()
            .map_or(0, |calendar| calendar.version)
    }

    /// A write from a dialog or a button: its outcome is the page's flash,
    /// and a refused form keeps its dialog open with each field's reason.
    async fn write(
        &mut self,
        scope: &Scope,
        credential: &str,
        request: Request,
        draft: Option<Refusals>,
    ) -> Result<()> {
        let Request {
            event,
            contract,
            input,
            done,
        } = request;
        match self
            .pages
            .submit(scope, credential, event, contract, input)
            .await?
        {
            Answer::Done => {
                self.dialog = None;
                self.flash = Some((StatusKind::Success, done));
            }
            Answer::Fields(fields) => match draft {
                Some((mut draft, key)) => {
                    let values = draft.values.clone();
                    let rest = draft.refuse(fields, |field| key(field, &values));
                    self.flash = rest.map(|message| (StatusKind::Error, message));
                    self.dialog = Some(match (event, self.dialog.take()) {
                        ("set_rule", _) => Dialog::Rule(draft),
                        ("include", Some(Dialog::Include(_, sets))) => Dialog::Include(draft, sets),
                        _ => Dialog::Exception(draft),
                    });
                }
                None => {
                    self.dialog = None;
                    self.flash = Some((StatusKind::Error, super::sentence(&fields)));
                }
            },
            Answer::Refused(message) => {
                self.flash = Some((StatusKind::Error, message));
                self.dialog = match (draft, event, self.dialog.take()) {
                    (Some((draft, _)), "set_rule", _) => Some(Dialog::Rule(draft)),
                    (Some((draft, _)), "include", Some(Dialog::Include(_, sets))) => {
                        Some(Dialog::Include(draft, sets))
                    }
                    (Some((draft, _)), _, _) => Some(Dialog::Exception(draft)),
                    (None, _, _) => None,
                };
            }
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let code = self.code.clone();
        let version = self.version();
        self.notice.clear();
        let grid = |columns: &mut ListColumns| -> Result<()> {
            match columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => Ok(()),
            }
        };
        match get("type").ok_or(Error::Invalid)? {
            "page" => self.page = params::positive_id(get("page")).map_or(1, |page| page as u64),
            "grid_rules" => {
                grid(&mut self.rules)?;
                self.rows_changed = false;
            }
            "grid_sets" => {
                grid(&mut self.sets)?;
                self.rows_changed = false;
            }
            "grid_exceptions" => {
                grid(&mut self.exceptions)?;
                self.rows_changed = false;
            }
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            "cancel_dialog" => self.dialog = None,
            "save_field" => {
                let field = get("field").unwrap_or_default();
                let label = match field {
                    "name" => "Name",
                    "substitute" => "Holiday substitutes",
                    _ => return Err(Error::Invalid),
                };
                let submitted = get("value").unwrap_or_default();
                let read: i64 = get("version")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                let mut input = json!({"code": code, "version": read});
                input[field] = json!(submitted.trim());
                let status = match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "save_field",
                        "core.work_calendar.update",
                        input,
                    )
                    .await?
                {
                    Answer::Done => CommitStatus::Saved,
                    Answer::Fields(fields) => {
                        let reasons: Vec<String> = fields.into_values().flatten().collect();
                        let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                        CommitStatus::refusal(label, submitted, &reasons)
                    }
                    Answer::Refused(message) => {
                        CommitStatus::refusal(label, submitted, &[&message])
                    }
                };
                self.statuses.put(field, status);
            }
            "request_retire" => {
                self.flash = None;
                self.dialog = Some(Dialog::Retire);
            }
            "retire" | "reactivate" => {
                let retire = get("type") == Some("retire");
                if retire && !matches!(self.dialog, Some(Dialog::Retire)) {
                    return Ok(());
                }
                let name = self.title();
                let (event, status, done) = if retire {
                    ("retire", "retired", format!("“{name}” was retired."))
                } else {
                    ("reactivate", "active", format!("“{name}” is active again."))
                };
                self.write(
                    scope,
                    credential,
                    Request {
                        event,
                        contract: "core.work_calendar.update",
                        input: json!({"code": code, "version": version, "status": status}),
                        done,
                    },
                    None,
                )
                .await?;
                self.dialog = None;
            }
            "new_rule" => {
                let mut draft = Draft::default();
                let today = self
                    .calendar
                    .as_ref()
                    .map_or_else(|| today_in("UTC"), calendar_today);
                let rule = self
                    .calendar
                    .as_ref()
                    .and_then(|calendar| in_force(&calendar.rules.rows, today));
                draft
                    .values
                    .insert("effective_from".into(), today.to_string());
                match rule {
                    Some(rule) => {
                        draft
                            .values
                            .insert("timezone".into(), rule.timezone.clone());
                        week::values(&rule.week, &mut draft.values);
                    }
                    None => week::defaults(&mut draft.values),
                }
                self.flash = None;
                self.dialog = Some(Dialog::Rule(draft));
            }
            "set_rule" => {
                let posted = params::form_values_checked(value, "rule", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let from = posted
                    .get("effective_from")
                    .map_or("", |v| v.trim())
                    .to_owned();
                let mut input = json!({
                    "code": code, "version": version, "effective_from": from,
                    "week": week::input(&posted),
                });
                if let Some(zone) = posted
                    .get("timezone")
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                {
                    input["timezone"] = json!(zone);
                }
                self.write(
                    scope,
                    credential,
                    Request {
                        event: "set_rule",
                        contract: "core.work_calendar.rule.set",
                        input,
                        done: format!("The week from {from} was saved."),
                    },
                    Some((
                        Draft {
                            values: posted,
                            ..Draft::default()
                        },
                        |field, values| {
                            ["effective_from", "timezone"]
                                .contains(&field)
                                .then(|| field.to_owned())
                                .or_else(|| week::field(field, values))
                        },
                    )),
                )
                .await?;
            }
            "new_include" => {
                // Every set, a page at a time: the tenant's and the
                // installation-wide ones.
                let mut sets = Vec::new();
                for page in 1.. {
                    let found = self
                        .pages
                        .calendars
                        .holiday_sets(
                            scope,
                            &Listing {
                                search: String::new(),
                                page,
                                page_size: MAX_PAGE_SIZE,
                            },
                        )
                        .await?;
                    sets.extend(found.rows);
                    if page * MAX_PAGE_SIZE >= found.total {
                        break;
                    }
                }
                self.flash = None;
                self.dialog = Some(Dialog::Include(Draft::default(), sets));
            }
            "include" => {
                let posted = params::form_values_checked(value, "include", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let set = posted.get("set").cloned().unwrap_or_default();
                self.write(
                    scope,
                    credential,
                    Request {
                        event: "include",
                        contract: "core.work_calendar.include",
                        input: json!({"code": code, "version": version, "holiday_set": set}),
                        done: format!("Holiday set {set} is included."),
                    },
                    Some((
                        Draft {
                            values: posted,
                            ..Draft::default()
                        },
                        |field, _| (field == "holiday_set").then(|| "set".to_owned()),
                    )),
                )
                .await?;
            }
            "leave_out" => {
                let set = get("key").unwrap_or_default().to_owned();
                self.write(
                    scope,
                    credential,
                    Request {
                        event: "leave_out",
                        contract: "core.work_calendar.include",
                        input: json!({"code": code, "version": version, "holiday_set": set, "included": false}),
                        done: format!("Holiday set {set} is left out."),
                    },
                    None,
                )
                .await?;
            }
            "new_exception" => {
                let mut draft = Draft::default();
                let today = self
                    .calendar
                    .as_ref()
                    .map_or_else(|| today_in("UTC"), calendar_today);
                draft.values.insert("starts_on".into(), today.to_string());
                draft.values.insert("type".into(), "off".into());
                self.flash = None;
                self.dialog = Some(Dialog::Exception(draft));
            }
            "add_exception" => {
                let posted = params::form_values_checked(value, "exception", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).map_or("", |v| v.trim()).to_owned();
                let name = field("name");
                let kind = field("type");
                let mut input = json!({
                    "code": code, "version": version, "starts_on": field("starts_on"),
                    "name": name, "type": kind,
                });
                if !field("ends_on").is_empty() {
                    input["ends_on"] = json!(field("ends_on"));
                }
                if kind == "working" {
                    input["periods"] = json!(week::split(&field("periods")));
                }
                self.write(
                    scope,
                    credential,
                    Request {
                        event: "add_exception",
                        contract: "core.work_calendar.exception.add",
                        input,
                        done: format!("The exception “{name}” was added."),
                    },
                    Some((
                        Draft {
                            values: posted,
                            ..Draft::default()
                        },
                        // The day's own refusals are `exception.type` and
                        // `exception.periods`.
                        |field, _| match field.strip_prefix("exception.").unwrap_or(field) {
                            field @ ("name" | "starts_on" | "ends_on" | "type" | "periods") => {
                                Some(field.to_owned())
                            }
                            "exception" => Some("type".to_owned()),
                            _ => None,
                        },
                    )),
                )
                .await?;
            }
            "request_remove" => {
                let id = params::positive_id(get("key"));
                let found = self.calendar.as_ref().and_then(|calendar| {
                    calendar
                        .exceptions
                        .rows
                        .iter()
                        .find(|exception| Some(exception.id) == id)
                        .cloned()
                });
                if let Some(exception) = found {
                    self.flash = None;
                    self.dialog = Some(Dialog::Remove(exception));
                }
            }
            "remove_exception" => {
                let Some(Dialog::Remove(exception)) = self.dialog.take() else {
                    return Ok(());
                };
                self.write(
                    scope,
                    credential,
                    Request {
                        event: "remove_exception",
                        contract: "core.work_calendar.exception.remove",
                        input: json!({"code": code, "version": version, "exception_id": exception.id}),
                        done: format!("The exception “{}” was removed.", exception.name),
                    },
                    None,
                )
                .await?;
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let standing = Standing::load(&self.pages, scope).await?;
        let calendar = self
            .pages
            .calendars
            .calendar(scope, &self.code, self.page as i64)
            .await?;
        let edits = standing.manages();
        // The included sets' names and keepers.
        let named = self
            .pages
            .calendars
            .named_sets(scope, &calendar.holiday_sets.rows)
            .await?;
        let header = self.header(&calendar, edits)?;
        let mut parts = vec![self.details(&calendar, edits)?];
        parts.extend(self.rules_section(&calendar, edits)?);
        parts.extend(self.sets_section(&calendar, &named, edits && standing.lists())?);
        parts.extend(self.exceptions_section(&calendar, edits)?);
        let dialog = self.dialog(&calendar)?;
        let html = View {
            id: "work-calendar-page",
            variant: PageVariant::Detail,
            flash: flash_map(&self.flash),
            header,
            parts,
            dialog,
        }
        .html()?;
        self.calendar = Some(calendar);
        Ok(html)
    }

    fn header(&self, calendar: &Calendar, edits: bool) -> Result<Html> {
        let mut actions = render(BackLink {
            href: CALENDARS,
            id: "work-calendar-back",
            title: "All calendars",
            class: "",
        })?;
        let month = path(MONTH, &calendar.code);
        actions.push(&render(ActionLink {
            id: "work-calendar-month",
            icon: "calendar-days",
            title: "Month view: each date resolved",
            href: &month,
            method: "",
            body: Html::text("Month view"),
        })?);
        if edits {
            actions.push(&render(if calendar.status == "active" {
                Button {
                    id: "work-calendar-retire",
                    variant: ButtonVariant::Danger,
                    event: "request_retire",
                    button_type: "button",
                    body: Html::text("Retire"),
                    ..Button::default()
                }
            } else {
                Button {
                    id: "work-calendar-reactivate",
                    event: "reactivate",
                    button_type: "button",
                    working: "Reactivating…",
                    body: Html::text("Reactivate"),
                    ..Button::default()
                }
            })?);
        }
        render(Header {
            title: Html::text(&calendar.name),
            subtitle: if calendar.status == "active" {
                "Pickers offer it and records may be assigned it."
            } else {
                "Retired: pickers no longer offer it, and records that hold it still compute with it."
            },
            title_actions: status_badge(&calendar.status)?,
            actions,
        })
    }

    fn details(&self, calendar: &Calendar, edits: bool) -> Result<Html> {
        let mut name = InlineField::new("work-calendar-name", "name", "Name", &calendar.name)
            .version(calendar.version)
            .status(self.statuses.get("name"));
        let substitute = calendar.substitute.as_str();
        let mut choice = InlineField::new(
            "work-calendar-substitute",
            "substitute",
            "Holiday substitutes",
            substitute,
        )
        .version(calendar.version)
        .status(self.statuses.get("substitute"));
        if !edits {
            name = name.readonly();
            choice = choice.readonly();
        }
        let label = substitutes()
            .into_iter()
            .find(|option| option.value == substitute)
            .map(|option| option.label)
            .unwrap_or_default();
        let editor = render(
            Field::named("substitute")
                .id("work-calendar-substitute-select")
                .label_class("sr-only")
                .label("Holiday substitutes")
                .kind(InputKind::Select)
                .value(substitute)
                .options(substitutes()),
        )?;
        let zone = in_force(&calendar.rules.rows, calendar_today(calendar))
            .map_or("—", |rule| rule.timezone.as_str());
        let facts = render(Facts {
            id: "work-calendar-facts",
            items: vec![
                Fact {
                    title: "Name",
                    id: "work-calendar-fact-name",
                    value: render(InlineEdit { field: name })?,
                },
                Fact {
                    title: "Code",
                    id: "work-calendar-fact-code",
                    value: Html::text(&calendar.code),
                },
                Fact {
                    title: "Holiday substitutes",
                    id: "work-calendar-fact-substitute",
                    value: render(InlineChoice {
                        field: choice,
                        display: Html::text(&label),
                        editor,
                    })?,
                },
                Fact {
                    title: "Time zone today",
                    id: "work-calendar-fact-zone",
                    value: Html::text(zone),
                },
            ],
        })?;
        render(Card {
            id: "work-calendar-details",
            body: facts,
            ..Card::default()
        })
    }

    fn section(
        id: &str,
        title: &str,
        description: &str,
        count: usize,
        actions: Html,
        table: FlexTable<'_>,
        footer: Html,
    ) -> Result<Vec<Html>> {
        let heading = render(SectionHeading {
            title,
            id,
            class: "mt-6",
            count: Some(count),
            description,
            actions,
            ..SectionHeading::default()
        })?;
        let mut body = render(table)?;
        body.push(&footer);
        Ok(vec![
            heading,
            render(Card {
                id: &format!("{id}-card"),
                inner_class: "p-0",
                body,
                ..Card::default()
            })?,
        ])
    }

    fn rules_section(&mut self, calendar: &Calendar, edits: bool) -> Result<Vec<Html>> {
        if self.rows_changed {
            self.rules
                .load(&calendar.rules.rows, |rule| rule.effective_from.to_string());
        }
        let mut rows = Vec::new();
        for rule in &calendar.rules.rows {
            let key = rule.effective_from.to_string();
            let mut row = FlexRow::new(key.clone())
                .slot("from", Html::text(&key))
                .slot("zone", Html::text(&rule.timezone));
            for (day, weekday) in crate::model::WEEKDAYS.iter().zip(&rule.week.0) {
                row = row.slot(day, Html::text(&week::summary(weekday)));
            }
            row.dom_id = Some(format!("work-calendar-rule-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("work-calendar-rules", self.rules.flex_columns(), rows);
        table.event = "grid_rules";
        table.zoom = self.rules.zoom;
        table.suggestions = self.rules.flex_suggestions();
        table.add_query = &self.rules.add_query;
        table.caption = "Weekly rules";
        table.framed = false;
        let actions = if edits {
            render(Button {
                id: "work-calendar-new-rule",
                event: "new_rule",
                button_type: "button",
                body: Html::text("Set the week from a date"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        Self::section(
            "work-calendar-rules-section",
            "Weekly rules",
            "Each version holds the week from its date until the next one; earlier dates keep their rules, and the first version also governs every date before it.",
            calendar.rules.total as usize,
            actions,
            table,
            Html::EMPTY,
        )
    }

    /// `includes`: the reader may include and leave out sets (it manages
    /// calendars and may list the sets).
    fn sets_section(
        &mut self,
        calendar: &Calendar,
        named: &[HolidaySetEntry],
        includes: bool,
    ) -> Result<Vec<Html>> {
        if self.rows_changed {
            self.sets
                .load(&calendar.holiday_sets.rows, |code| code.clone());
        }
        let mut rows = Vec::new();
        for code in &calendar.holiday_sets.rows {
            let known = named.iter().find(|set| &set.code == code);
            let href = path(SET, code);
            let mut row = FlexRow::new(code.clone())
                .slot(
                    "set",
                    render(RecordLink {
                        workspace: false,
                        kind: "core/work_calendar.holiday_set",
                        record_id: code,
                        href: &href,
                        class: "",
                        body: Html::text(code),
                    })?,
                )
                .slot(
                    "name",
                    Html::text(known.map_or("—", |set| set.name.as_str())),
                )
                .slot(
                    "keeper",
                    Html::text(match known {
                        Some(set) if set.installation => "The platform operator",
                        Some(_) => "This organisation",
                        None => "—",
                    }),
                );
            if includes {
                row.actions = render(IconButton {
                    id: &format!("work-calendar-leave-out-{code}"),
                    event: "leave_out",
                    name: "key",
                    value: code,
                    ..IconButton::new("minus-circle", "Leave this set out")
                })?;
            }
            row.dom_id = Some(format!("work-calendar-set-{code}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("work-calendar-sets", self.sets.flex_columns(), rows);
        table.event = "grid_sets";
        table.zoom = self.sets.zoom;
        table.suggestions = self.sets.flex_suggestions();
        table.add_query = &self.sets.add_query;
        table.caption = "Included holiday sets";
        table.framed = false;
        table.actions = includes;
        if calendar.holiday_sets.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No holiday set is included: only the week and exceptions decide its days.",
            )));
        }
        let actions = if includes {
            render(Button {
                id: "work-calendar-new-include",
                event: "new_include",
                button_type: "button",
                body: Html::text("Include a holiday set"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        Self::section(
            "work-calendar-sets-section",
            "Holiday sets",
            "A holiday from an included set closes the day. With a substitute rule, a holiday on a rest day moves to the next working day.",
            calendar.holiday_sets.total as usize,
            actions,
            table,
            Html::EMPTY,
        )
    }

    fn exceptions_section(&mut self, calendar: &Calendar, edits: bool) -> Result<Vec<Html>> {
        let shown = &calendar.exceptions.rows;
        if self.rows_changed {
            self.exceptions
                .load(shown, |exception| exception.id.to_string());
        }
        let mut rows = Vec::new();
        for exception in shown {
            let mut row = FlexRow::new(exception.id.to_string())
                .slot("name", Html::text(&exception.name))
                .slot("from", Html::text(&exception.starts_on.to_string()))
                .slot("to", Html::text(&exception.ends_on.to_string()))
                .slot("type", Html::text(week::kind(exception.day.day_type)))
                .slot(
                    "hours",
                    Html::text(&if exception.day.day_type == DayType::Working {
                        week::periods(&exception.day)
                    } else {
                        "—".to_owned()
                    }),
                );
            if edits {
                let id = exception.id.to_string();
                row.actions = render(IconButton {
                    id: &format!("work-calendar-exception-{id}-remove"),
                    kind: IconKind::Danger,
                    event: "request_remove",
                    name: "key",
                    value: &id,
                    ..IconButton::new("trash", "Remove this exception")
                })?;
            }
            row.dom_id = Some(format!("work-calendar-exception-{}", exception.id));
            rows.push(row);
        }
        let mut table = FlexTable::new(
            "work-calendar-exceptions",
            self.exceptions.flex_columns(),
            rows,
        );
        table.event = "grid_exceptions";
        table.zoom = self.exceptions.zoom;
        table.suggestions = self.exceptions.flex_suggestions();
        table.add_query = &self.exceptions.add_query;
        table.caption = "Exceptions";
        table.framed = false;
        table.actions = edits;
        if calendar.exceptions.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text("No dated exception.")));
        }
        let counts = PageCounts::new(
            self.page,
            EXCEPTION_PAGE[0],
            calendar.exceptions.total as u64,
        );
        let footer = if counts.total_pages > 1 {
            render(Pagination::new(
                "work-calendar-exceptions-pagination",
                counts,
                &EXCEPTION_PAGE,
            ))?
        } else {
            Html::EMPTY
        };
        let actions = if edits {
            render(Button {
                id: "work-calendar-new-exception",
                event: "new_exception",
                button_type: "button",
                body: Html::text("Add an exception"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        Self::section(
            "work-calendar-exceptions-section",
            "Exceptions",
            "A dated exception wins over holidays and the week: a shutdown, a special working day, a half day.",
            calendar.exceptions.total as usize,
            actions,
            table,
            footer,
        )
    }

    fn dialog(&self, calendar: &Calendar) -> Result<Html> {
        let Some(dialog) = &self.dialog else {
            return Ok(Html::EMPTY);
        };
        match dialog {
            Dialog::Retire => render(ConfirmDialog {
                id: "work-calendar-retire-confirm",
                consequence: &format!("Retire “{}”?", calendar.name),
                detail: "Pickers stop offering it and new records cannot be assigned it. Records that hold it keep computing with it, and you can reactivate it later.",
                confirm: "Retire",
                working: "Retiring…",
                confirm_event: "retire",
                cancel_event: "cancel_dialog",
            }),
            Dialog::Remove(exception) => render(ConfirmDialog {
                id: "work-calendar-remove-confirm",
                consequence: &format!("Remove the exception “{}”?", exception.name),
                detail: &format!(
                    "{} to {} resolve from holidays and the week again.",
                    long_date(exception.starts_on),
                    long_date(exception.ends_on)
                ),
                confirm: "Remove",
                working: "Removing…",
                confirm_event: "remove_exception",
                cancel_event: "cancel_dialog",
            }),
            Dialog::Rule(draft) => {
                let mut body = render(
                    Field::in_form("rule", "effective_from")
                        .id("work-calendar-rule-from")
                        .label("From")
                        .kind(InputKind::Date)
                        .value(draft.value("effective_from"))
                        .hint("The week holds from this date until the next version. A version already on this date is replaced.")
                        .errors(draft.errors("effective_from"))
                        .required(),
                )?;
                body.push(&render(
                    Field::in_form("rule", "timezone")
                        .id("work-calendar-rule-timezone")
                        .label("Time zone")
                        .value(draft.value("timezone"))
                        .attr("maxlength", "64")
                        .errors(draft.errors("timezone")),
                )?);
                body.push(&week::fields("rule", "work-calendar-rule", draft)?);
                self.modal(
                    Frame {
                        id: "work-calendar-rule",
                        title: "Set the week from a date",
                        description: "Earlier dates keep their rules.",
                        event: "set_rule",
                        submit: "Save the week",
                        width: ModalWidth::Wide,
                    },
                    body,
                )
            }
            Dialog::Include(draft, offered) => {
                let options: Vec<Choice> = offered
                    .iter()
                    .filter(|set| !calendar.holiday_sets.rows.contains(&set.code))
                    .map(|set| {
                        let keeper = if set.installation {
                            ", installation-wide"
                        } else {
                            ""
                        };
                        Choice::new(format!("{} ({}{keeper})", set.name, set.code), set.code.clone())
                    })
                    .collect();
                let body = render(
                    Field::in_form("include", "set")
                        .id("work-calendar-include-set")
                        .label("Holiday set")
                        .kind(InputKind::Select)
                        .prompt("Choose…")
                        .options(options)
                        .value(draft.value("set"))
                        .errors(draft.errors("set"))
                        .required(),
                )?;
                self.modal(
                    Frame {
                        id: "work-calendar-include",
                        title: "Include a holiday set",
                        description: "Its holidays close this calendar's days from now on.",
                        event: "include",
                        submit: "Include",
                        width: ModalWidth::Narrow,
                    },
                    body,
                )
            }
            Dialog::Exception(draft) => {
                let mut body = render(
                    Field::in_form("exception", "name")
                        .id("work-calendar-exception-name")
                        .label("Name")
                        .value(draft.value("name"))
                        .hint("Why: Year-end shutdown, Special working Saturday, Half day.")
                        .attr("maxlength", "120")
                        .errors(draft.errors("name"))
                        .required(),
                )?;
                body.push(&render(
                    Field::in_form("exception", "starts_on")
                        .id("work-calendar-exception-from")
                        .label("From")
                        .kind(InputKind::Date)
                        .value(draft.value("starts_on"))
                        .errors(draft.errors("starts_on"))
                        .required(),
                )?);
                body.push(&render(
                    Field::in_form("exception", "ends_on")
                        .id("work-calendar-exception-to")
                        .label("To")
                        .kind(InputKind::Date)
                        .value(draft.value("ends_on"))
                        .hint("Leave empty for one day. At most 366 days.")
                        .errors(draft.errors("ends_on")),
                )?);
                body.push(&render(
                    Field::in_form("exception", "type")
                        .id("work-calendar-exception-type")
                        .label("Day type")
                        .kind(InputKind::Select)
                        .options(vec![
                            Choice::new("Off: closed", "off"),
                            Choice::new("Holiday", "holiday"),
                            Choice::new("Rest", "rest"),
                            Choice::new("Working: open with its own hours", "working"),
                        ])
                        .value(draft.value("type"))
                        .errors(draft.errors("type"))
                        .required(),
                )?);
                body.push(&render(
                    Field::in_form("exception", "periods")
                        .id("work-calendar-exception-periods")
                        .label("Hours")
                        .value(draft.value("periods"))
                        .hint("Working only: HH:MM-HH:MM, separated by commas. A short day is a half day.")
                        .attr("maxlength", "60")
                        .errors(draft.errors("periods")),
                )?);
                self.modal(
                    Frame {
                        id: "work-calendar-exception",
                        title: "Add an exception",
                        description: "It wins over holidays and the week on its dates.",
                        event: "add_exception",
                        submit: "Add exception",
                        width: ModalWidth::Narrow,
                    },
                    body,
                )
            }
        }
    }

    fn modal(&self, frame: Frame<'_>, body: Html) -> Result<Html> {
        let Frame {
            id,
            title,
            description,
            event,
            submit,
            width,
        } = frame;
        let mut actions = render(Button {
            event: "cancel_dialog",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            variant: ButtonVariant::Primary,
            button_type: "submit",
            working: "Saving…",
            body: Html::text(submit),
            ..Button::default()
        })?);
        let form = render(Form {
            id: &format!("{id}-form"),
            event,
            body,
            actions,
            ..Form::default()
        })?;
        render(Modal {
            id: &format!("{id}-dialog"),
            title,
            description,
            cancel_event: "cancel_dialog",
            width,
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

fn page(query: &HashMap<String, String>) -> u64 {
    params::positive_id(query.get("page").map(String::as_str)).map_or(1, |page| page as u64)
}

impl LiveState<Scope, Error> for Record {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.page = page(values),
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.render(request.scope).await?;
            let mut query = HashMap::new();
            if self.page > 1 {
                query.insert("page".into(), self.page.to_string());
            }
            Ok(LiveBody {
                html,
                query,
                rows: self
                    .calendar
                    .as_ref()
                    .map_or(0, |calendar| calendar.exceptions.rows.len()),
                notice: self.notice.clone(),
                ..LiveBody::default()
            })
        })
    }
}
