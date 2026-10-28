//! The holiday sets list (`/admin/holiday-sets`) and one set
//! (`/admin/holiday-sets/{code}`): its facts read first and edited in place,
//! its holidays a year at a time (`year=2026`, previous and next as links)
//! with add, rename and remove, and this tenant's calendars that include it.
//! An installation-wide set (issue 210) is read-only here except to the
//! platform operator; anyone who manages calendars may copy it into a set of
//! their own and change that instead.
use super::{
    clear_flash, flash_map, long_date, path, render, sentence, today_in, Answer, Draft, Flash,
    Pages, Standing, View, CALENDAR, SET, SETS,
};
use crate::model::{HolidaySet, HolidaySetEntry, Listing};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    lists::RecordLink,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, ActionLink, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card,
    ColumnsOutcome, CommitStates, CommitStatus, ConfirmDialog, CreateButton, Fact, Facts, Field,
    FilterToolbar, FlexRow, FlexTable, Form, Header, Html, IconButton, IconKind, InlineEdit,
    InlineField, InputKind, ListColumns, ListSpec, ListState, Modal, PageCounts, PageVariant,
    Pagination, SectionHeading, StatusKind, TableEmpty, ToolbarControl,
};
use jiff::civil::Date;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_blank: vec!["search".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("holiday sets list spec")
});

fn keeper(installation: bool) -> &'static str {
    if installation {
        "The platform operator, for every organisation"
    } else {
        "This organisation"
    }
}

/// A form field for one of the set fields the contracts name.
fn set_field(field: &str) -> Option<String> {
    ["code", "name", "country_iso", "admin1_code", "installation"]
        .contains(&field)
        .then(|| field.to_owned())
}

// ------------------------------------------------------------------ the list

pub(super) struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    framed: bool,
    rows: usize,
    flash: Flash,
    draft: Option<Draft>,
    created: Option<(String, String)>,
}

impl Index {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount(
                "holiday-sets",
                vec![
                    BuiltinColumn::new("name", "Holiday set"),
                    BuiltinColumn::new("code", "Code"),
                    BuiltinColumn::new("country", "Country"),
                    BuiltinColumn::new("division", "Division"),
                    BuiltinColumn::new("keeper", "Kept by"),
                ],
            )
            .map_err(|_| Error::Internal)?,
            rows_changed: true,
            framed,
            rows: 0,
            flash: None,
            draft: None,
            created: None,
        })
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self.state.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            "new" => self.draft = Some(Draft::default()),
            "close_new" => self.draft = None,
            "create_set" => {
                let posted = params::form_values_checked(value, "set", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).map_or("", |v| v.trim()).to_owned();
                let (code, name) = (field("code"), field("name"));
                let mut input = json!({"code": code, "name": name});
                for key in ["country_iso", "admin1_code"] {
                    if !field(key).is_empty() {
                        input[key] = json!(field(key));
                    }
                }
                if field("installation") == "true" {
                    input["installation"] = json!(true);
                }
                let answer = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "create_set",
                        "core.work_calendar.holiday_set.create",
                        input,
                    )
                    .await?;
                let mut draft = Draft {
                    values: posted,
                    ..Draft::default()
                };
                match answer {
                    Answer::Done => {
                        self.draft = None;
                        self.created = Some((
                            path(SET, &code),
                            format!("Holiday set “{name}” was created."),
                        ));
                    }
                    Answer::Fields(fields) => {
                        let rest = draft.refuse(fields, set_field);
                        self.flash = rest.map(|message| (StatusKind::Error, message));
                        self.draft = Some(draft);
                    }
                    Answer::Refused(message) => {
                        self.flash = Some((StatusKind::Error, message));
                        self.draft = Some(draft);
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let standing = Standing::load(&self.pages, scope).await?;
        let listing = |state: &ListState| Listing {
            search: state.search.clone(),
            page: state.page as i64,
            page_size: state.page_size as i64,
        };
        let mut found = self
            .pages
            .calendars
            .holiday_sets(scope, &listing(&self.state))
            .await?;
        let mut counts = PageCounts::new(self.state.page, self.state.page_size, found.total as u64);
        if self.state.clamp_to_last_page(counts.total_pages, true) {
            found = self
                .pages
                .calendars
                .holiday_sets(scope, &listing(&self.state))
                .await?;
            counts = PageCounts::new(self.state.page, self.state.page_size, found.total as u64);
        }
        self.rows = found.rows.len();
        if self.rows_changed {
            self.columns.load(&found.rows, |set| set.code.clone());
        }
        let actions = if standing.manages() {
            render(CreateButton {
                noun: "Holiday set",
                id: "holiday-set-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Holiday sets"),
            subtitle: "Public holidays by country or state. A calendar includes the sets that apply to it.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "holiday-sets-filters",
            vec![ToolbarControl::search(
                "holiday-sets-search",
                "search",
                "Search holiday sets",
                &form["search"],
                "Search by code or name...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(found.rows.len());
        for set in &found.rows {
            rows.push(self.row(set)?);
        }
        let mut table = FlexTable::new("holiday-sets", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Holiday sets";
        table.framed = false;
        if found.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                if self.state.search.is_empty() {
                    "No holiday set yet. Nothing ships: enter the holidays your government announces."
                } else {
                    "No holiday set matches this search."
                },
            )));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "holiday-sets-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "holiday-sets-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.draft {
            Some(draft) => self.new_dialog(draft, standing.keeps_installation())?,
            None => Html::EMPTY,
        };
        View {
            id: "holiday-sets-page",
            variant: PageVariant::List,
            flash: flash_map(&self.flash),
            header,
            parts: vec![toolbar, card],
            dialog,
        }
        .html()
    }

    fn row(&self, set: &HolidaySetEntry) -> Result<FlexRow> {
        let href = path(SET, &set.code);
        let mut row = FlexRow::new(set.code.clone())
            .slot(
                "name",
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/work_calendar.holiday_set",
                    record_id: &set.code,
                    href: &href,
                    class: "",
                    body: Html::text(&set.name),
                })?,
            )
            .slot("code", Html::text(&set.code))
            .slot(
                "country",
                Html::text(set.country_iso.as_deref().unwrap_or("—")),
            )
            .slot(
                "division",
                Html::text(set.admin1_code.as_deref().unwrap_or("—")),
            )
            .slot("keeper", Html::text(keeper(set.installation)));
        row.dom_id = Some(format!("holiday-set-{}", set.code));
        Ok(row)
    }

    fn new_dialog(&self, draft: &Draft, installation: bool) -> Result<Html> {
        let mut body = render(
            Field::in_form("set", "code")
                .id("holiday-set-new-code")
                .label("Code")
                .value(draft.value("code"))
                .hint("Lowercase letters, digits, _ and -; it never changes.")
                .attr("maxlength", "64")
                .errors(draft.errors("code"))
                .required(),
        )?;
        body.push(&render(
            Field::in_form("set", "name")
                .id("holiday-set-new-name")
                .label("Name")
                .value(draft.value("name"))
                .attr("maxlength", "120")
                .errors(draft.errors("name"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("set", "country_iso")
                .id("holiday-set-new-country")
                .label("Country")
                .value(draft.value("country_iso"))
                .hint("Optional ISO code, such as MY; checked against the imported countries.")
                .attr("maxlength", "2")
                .errors(draft.errors("country_iso")),
        )?);
        body.push(&render(
            Field::in_form("set", "admin1_code")
                .id("holiday-set-new-division")
                .label("Division")
                .value(draft.value("admin1_code"))
                .hint("Optional state or division of that country, such as MY.14.")
                .attr("maxlength", "20")
                .errors(draft.errors("admin1_code")),
        )?);
        if installation {
            body.push(&render(
                Field::in_form("set", "installation")
                    .id("holiday-set-new-installation")
                    .label("Installation-wide: every organisation may include it, and only the platform operator changes it")
                    .kind(InputKind::Checkbox)
                    .checked(draft.value("installation") == "true")
                    .errors(draft.errors("installation")),
            )?);
        }
        let mut actions = render(Button {
            event: "close_new",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            variant: ButtonVariant::Primary,
            button_type: "submit",
            working: "Creating…",
            body: Html::text("Create holiday set"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "holiday-set-new-form",
            event: "create_set",
            body,
            actions,
            ..Form::default()
        })?;
        render(Modal {
            id: "holiday-set-new-dialog",
            title: "New holiday set",
            description: "Enter its holidays on its page, a year at a time.",
            cancel_event: "close_new",
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            if let Some((redirect, message)) = self.created.take() {
                return Ok(LiveBody {
                    redirect: Some(redirect),
                    flash: Some((StatusKind::Success, message)),
                    ..LiveBody::default()
                });
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.rows,
                ..LiveBody::default()
            })
        })
    }
}

// ---------------------------------------------------------------- the record

enum Dialog {
    Add(Draft),
    Copy(Draft),
    Remove(Date, String),
}

pub(super) struct Record {
    pages: Pages,
    code: String,
    /// The year asked for; none is this year in the company's zone.
    year: Option<i16>,
    /// The year shown.
    shown: i16,
    /// The set as last read: its version fences every write.
    set: Option<HolidaySet>,
    holidays: ListColumns,
    calendars: ListColumns,
    rows_changed: bool,
    statuses: CommitStates,
    notice: String,
    flash: Flash,
    dialog: Option<Dialog>,
    created: Option<(String, String)>,
}

fn year(query: &HashMap<String, String>) -> Option<i16> {
    query
        .get("year")
        .and_then(|year| year.parse::<i16>().ok())
        .filter(|year| (1..=9998).contains(year))
}

impl Record {
    pub(super) fn new(pages: Pages, code: String, query: &HashMap<String, String>) -> Result<Self> {
        let mount = |id, columns| ListColumns::mount(id, columns).map_err(|_| Error::Internal);
        Ok(Self {
            pages,
            code,
            year: year(query),
            shown: today_in("UTC").year(),
            set: None,
            holidays: mount(
                "holiday-set-holidays",
                vec![
                    BuiltinColumn::new("date", "Date"),
                    BuiltinColumn::new("day", "Day"),
                    BuiltinColumn::new("name", "Holiday"),
                ],
            )?,
            calendars: mount(
                "holiday-set-calendars",
                vec![BuiltinColumn::new("calendar", "Calendar")],
            )?,
            rows_changed: true,
            statuses: CommitStates::default(),
            notice: String::new(),
            flash: None,
            dialog: None,
            created: None,
        })
    }
    pub(super) fn title(&self) -> String {
        self.set
            .as_ref()
            .map_or_else(|| self.code.clone(), |set| set.name.clone())
    }
    fn version(&self) -> i64 {
        self.set.as_ref().map_or(0, |set| set.version)
    }

    /// A one-click or confirmed write: its outcome is the page's flash.
    async fn write(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &str,
        input: Value,
        done: String,
    ) -> Result<()> {
        self.flash = Some(
            match self
                .pages
                .submit(
                    scope,
                    credential,
                    event,
                    "core.work_calendar.holiday_set.holidays",
                    input,
                )
                .await?
            {
                Answer::Done => (StatusKind::Success, done),
                Answer::Fields(fields) => (StatusKind::Error, sentence(&fields)),
                Answer::Refused(message) => (StatusKind::Error, message),
            },
        );
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
            "grid_holidays" => {
                grid(&mut self.holidays)?;
                self.rows_changed = false;
            }
            "grid_calendars" => {
                grid(&mut self.calendars)?;
                self.rows_changed = false;
            }
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            "cancel_dialog" => self.dialog = None,
            "save_set_field" => {
                let field = get("field").unwrap_or_default();
                let label = match field {
                    "name" => "Name",
                    "country_iso" => "Country",
                    "admin1_code" => "Division",
                    _ => return Err(Error::Invalid),
                };
                let submitted = get("value").unwrap_or_default().trim().to_owned();
                let read: i64 = get("version")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                let country = self
                    .set
                    .as_ref()
                    .and_then(|set| set.country_iso.clone())
                    .unwrap_or_default();
                let input = match field {
                    "name" => json!({"code": code, "version": read, "name": submitted}),
                    // A new country clears the division, which belonged to the old one.
                    "country_iso" => {
                        json!({"code": code, "version": read, "country_iso": submitted, "admin1_code": ""})
                    }
                    _ => {
                        json!({"code": code, "version": read, "country_iso": country, "admin1_code": submitted})
                    }
                };
                let status = match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "save_set_field",
                        "core.work_calendar.holiday_set.update",
                        input,
                    )
                    .await?
                {
                    Answer::Done => CommitStatus::Saved,
                    Answer::Fields(fields) => {
                        let reasons: Vec<String> = fields.into_values().flatten().collect();
                        let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                        CommitStatus::refusal(label, &submitted, &reasons)
                    }
                    Answer::Refused(message) => {
                        CommitStatus::refusal(label, &submitted, &[&message])
                    }
                };
                self.statuses.put(field, status);
            }
            "rename_holiday" => {
                let date = get("id").unwrap_or_default().to_owned();
                let submitted = get("value").unwrap_or_default().trim().to_owned();
                let read: i64 = get("version")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                let status = match self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "rename_holiday",
                        "core.work_calendar.holiday_set.holidays",
                        json!({"code": code, "version": read, "add": [{"date": date, "name": submitted}]}),
                    )
                    .await?
                {
                    Answer::Done => CommitStatus::Saved,
                    Answer::Fields(fields) => {
                        let reasons: Vec<String> = fields.into_values().flatten().collect();
                        let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                        CommitStatus::refusal("Holiday", &submitted, &reasons)
                    }
                    Answer::Refused(message) => {
                        CommitStatus::refusal("Holiday", &submitted, &[&message])
                    }
                };
                self.statuses.put(format!("{date}-name"), status);
            }
            "new_holiday" => {
                let mut draft = Draft::default();
                let first = Date::new(self.shown, 1, 1).map_err(|_| Error::Invalid)?;
                let today = today_in(&self.pages.calendars.company_zone(scope).await?);
                let day = if today.year() == self.shown {
                    today
                } else {
                    first
                };
                draft.values.insert("date".into(), day.to_string());
                self.flash = None;
                self.dialog = Some(Dialog::Add(draft));
            }
            "add_holiday" => {
                let posted = params::form_values_checked(value, "holiday", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).map_or("", |v| v.trim()).to_owned();
                let (date, name) = (field("date"), field("name"));
                let answer = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "add_holiday",
                        "core.work_calendar.holiday_set.holidays",
                        json!({"code": code, "version": version, "add": [{"date": date, "name": name}]}),
                    )
                    .await?;
                let mut draft = Draft {
                    values: posted,
                    ..Draft::default()
                };
                match answer {
                    Answer::Done => {
                        self.dialog = None;
                        self.flash = Some((
                            StatusKind::Success,
                            format!("“{name}” on {date} was added."),
                        ));
                        // Show the year the holiday is in.
                        if let Ok(added) = date.parse::<Date>() {
                            self.year = Some(added.year());
                        }
                    }
                    Answer::Fields(fields) => {
                        let rest = draft.refuse(fields, |field| {
                            field.strip_prefix("add").map(|rest| {
                                if rest.ends_with("name") {
                                    "name"
                                } else {
                                    "date"
                                }
                                .to_owned()
                            })
                        });
                        self.flash = rest.map(|message| (StatusKind::Error, message));
                        self.dialog = Some(Dialog::Add(draft));
                    }
                    Answer::Refused(message) => {
                        self.flash = Some((StatusKind::Error, message));
                        self.dialog = Some(Dialog::Add(draft));
                    }
                }
            }
            "request_remove" => {
                let date = get("key").unwrap_or_default();
                let found = self.set.as_ref().and_then(|set| {
                    set.holidays
                        .rows
                        .iter()
                        .find(|holiday| holiday.date.to_string() == date)
                        .map(|holiday| (holiday.date, holiday.name.clone()))
                });
                if let Some((date, name)) = found {
                    self.flash = None;
                    self.dialog = Some(Dialog::Remove(date, name));
                }
            }
            "remove_holiday" => {
                let Some(Dialog::Remove(date, name)) = self.dialog.take() else {
                    return Ok(());
                };
                self.write(
                    scope,
                    credential,
                    "remove_holiday",
                    json!({"code": code, "version": version, "remove": [date.to_string()]}),
                    format!("“{name}” on {date} was removed."),
                )
                .await?;
            }
            "new_copy" => {
                let mut draft = Draft::default();
                if let Some(set) = &self.set {
                    draft
                        .values
                        .insert("name".into(), format!("{} (copy)", set.name));
                }
                self.flash = None;
                self.dialog = Some(Dialog::Copy(draft));
            }
            "copy_set" => {
                let posted = params::form_values_checked(value, "copy", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).map_or("", |v| v.trim()).to_owned();
                let (new, name) = (field("code"), field("name"));
                let mut input = json!({"code": new, "name": name, "source": code});
                for key in ["from", "to"] {
                    if !field(key).is_empty() {
                        input[key] = json!(field(key));
                    }
                }
                let answer = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "copy_set",
                        "core.work_calendar.holiday_set.copy",
                        input,
                    )
                    .await?;
                let mut draft = Draft {
                    values: posted,
                    ..Draft::default()
                };
                match answer {
                    Answer::Done => {
                        self.dialog = None;
                        self.created = Some((
                            path(SET, &new),
                            format!(
                                "“{name}” holds a copy of the holidays and is yours to change."
                            ),
                        ));
                    }
                    Answer::Fields(fields) => {
                        let rest = draft.refuse(fields, |field| {
                            ["code", "name", "from", "to"]
                                .contains(&field)
                                .then(|| field.to_owned())
                        });
                        self.flash = rest.map(|message| (StatusKind::Error, message));
                        self.dialog = Some(Dialog::Copy(draft));
                    }
                    Answer::Refused(message) => {
                        self.flash = Some((StatusKind::Error, message));
                        self.dialog = Some(Dialog::Copy(draft));
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let standing = Standing::load(&self.pages, scope).await?;
        self.shown = match self.year {
            Some(year) => year,
            None => today_in(&self.pages.calendars.company_zone(scope).await?).year(),
        };
        let first = Date::new(self.shown, 1, 1).map_err(|_| Error::Invalid)?;
        let last = Date::new(self.shown, 12, 31).map_err(|_| Error::Invalid)?;
        let set = self
            .pages
            .calendars
            .holiday_set(scope, &self.code, Some(first), Some(last), 1)
            .await?;
        // An installation-wide set is the platform operator's to change.
        let edits = standing.manages() && (!set.installation || standing.keeps_installation());
        let header = self.header(&set, standing.manages())?;
        let mut parts = vec![self.details(&set, edits)?];
        parts.extend(self.holidays_section(&set, edits)?);
        parts.extend(self.calendars_section(&set)?);
        let dialog = self.dialog()?;
        let html = View {
            id: "holiday-set-page",
            variant: PageVariant::Detail,
            flash: flash_map(&self.flash),
            header,
            parts,
            dialog,
        }
        .html()?;
        self.set = Some(set);
        Ok(html)
    }

    fn header(&self, set: &HolidaySet, copies: bool) -> Result<Html> {
        let mut actions = render(BackLink {
            href: SETS,
            id: "holiday-set-back",
            title: "All holiday sets",
            class: "",
        })?;
        if copies {
            actions.push(&render(Button {
                id: "holiday-set-copy",
                event: "new_copy",
                button_type: "button",
                body: Html::text("Copy into a new set"),
                ..Button::default()
            })?);
        }
        render(Header {
            title: Html::text(&set.name),
            subtitle: if set.installation {
                "Installation-wide: the platform operator keeps it for every organisation. Copy it to change it for yours."
            } else {
                "Kept by this organisation."
            },
            title_actions: if set.installation {
                render(Badge {
                    kind: BadgeKind::Plain,
                    dot: false,
                    class: "",
                    body: Html::text("Installation-wide"),
                })?
            } else {
                Html::EMPTY
            },
            actions,
        })
    }

    fn details(&self, set: &HolidaySet, edits: bool) -> Result<Html> {
        let inline = |id: &str, name: &str, label: &str, value: &str| {
            let mut field = InlineField::new(id, name, label, value)
                .version(set.version)
                .status(self.statuses.get(name));
            field.save_event = "save_set_field".into();
            if name != "name" {
                field = field.allow_empty();
            }
            if !edits {
                field = field.readonly();
            }
            field
        };
        let hint = |field: InlineField, hint: &str| if edits { field.hint(hint) } else { field };
        let country = set.country_iso.as_deref().unwrap_or("");
        let division = set.admin1_code.as_deref().unwrap_or("");
        let facts = render(Facts {
            id: "holiday-set-facts",
            items: vec![
                Fact {
                    title: "Name",
                    id: "holiday-set-fact-name",
                    value: render(InlineEdit {
                        field: inline("holiday-set-name", "name", "Name", &set.name),
                    })?,
                },
                Fact {
                    title: "Code",
                    id: "holiday-set-fact-code",
                    value: Html::text(&set.code),
                },
                Fact {
                    title: "Country",
                    id: "holiday-set-fact-country",
                    value: render(InlineEdit {
                        field: hint(
                            inline("holiday-set-country", "country_iso", "Country", country),
                            "ISO code, such as MY. A new country clears the division.",
                        ),
                    })?,
                },
                Fact {
                    title: "Division",
                    id: "holiday-set-fact-division",
                    value: render(InlineEdit {
                        field: hint(
                            inline("holiday-set-division", "admin1_code", "Division", division),
                            "A state of the country, such as MY.14.",
                        ),
                    })?,
                },
                Fact {
                    title: "Kept by",
                    id: "holiday-set-fact-keeper",
                    value: Html::text(keeper(set.installation)),
                },
            ],
        })?;
        render(Card {
            id: "holiday-set-details",
            body: facts,
            ..Card::default()
        })
    }

    fn holidays_section(&mut self, set: &HolidaySet, edits: bool) -> Result<Vec<Html>> {
        if self.rows_changed {
            self.holidays
                .load(&set.holidays.rows, |holiday| holiday.date.to_string());
        }
        let mut rows = Vec::new();
        for holiday in &set.holidays.rows {
            let date = holiday.date.to_string();
            let mut name = InlineField::new(
                format!("holiday-set-holiday-{date}-name"),
                "name",
                "Holiday",
                &holiday.name,
            )
            .version(set.version)
            .status(self.statuses.get(&format!("{date}-name")));
            name.id_value = date.clone();
            name.save_event = "rename_holiday".into();
            if !edits {
                name = name.readonly();
            }
            let mut row = FlexRow::new(date.clone())
                .slot("date", Html::text(&date))
                .slot("day", Html::text(&holiday.date.strftime("%A").to_string()))
                .slot("name", render(InlineEdit { field: name })?);
            if edits {
                row.actions = render(IconButton {
                    id: &format!("holiday-set-holiday-{date}-remove"),
                    kind: IconKind::Danger,
                    event: "request_remove",
                    name: "key",
                    value: &date,
                    ..IconButton::new("trash", "Remove this holiday")
                })?;
            }
            row.dom_id = Some(format!("holiday-set-holiday-{date}"));
            rows.push(row);
        }
        let caption = format!("Holidays in {}", self.shown);
        let mut table = FlexTable::new("holiday-set-holidays", self.holidays.flex_columns(), rows);
        table.event = "grid_holidays";
        table.zoom = self.holidays.zoom;
        table.suggestions = self.holidays.flex_suggestions();
        table.add_query = &self.holidays.add_query;
        table.caption = &caption;
        table.framed = false;
        table.actions = edits;
        if set.holidays.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(&format!(
                "No holiday in {}.",
                self.shown
            ))));
        }
        let mut actions = Html::EMPTY;
        let here = path(SET, &set.code);
        for (id, icon, year, title) in [
            (
                "holiday-set-previous-year",
                "chevron-left",
                self.shown - 1,
                "Previous year",
            ),
            (
                "holiday-set-next-year",
                "chevron-right",
                self.shown + 1,
                "Next year",
            ),
        ] {
            let href = format!("{here}?year={year}");
            actions.push(&render(ActionLink {
                id,
                icon,
                title,
                href: &href,
                method: "",
                body: Html::text(&year.to_string()),
            })?);
        }
        if edits {
            actions.push(&render(Button {
                id: "holiday-set-new-holiday",
                event: "new_holiday",
                button_type: "button",
                body: Html::text("Add a holiday"),
                ..Button::default()
            })?);
        }
        let heading = render(SectionHeading {
            title: &caption,
            id: "holiday-set-holidays-section",
            class: "mt-6",
            count: Some(set.holidays.total as usize),
            description: "Whole days. Every calendar that includes this set closes on them.",
            actions,
            ..SectionHeading::default()
        })?;
        Ok(vec![
            heading,
            render(Card {
                id: "holiday-set-holidays-card",
                inner_class: "p-0",
                body: render(table)?,
                ..Card::default()
            })?,
        ])
    }

    fn calendars_section(&mut self, set: &HolidaySet) -> Result<Vec<Html>> {
        if self.rows_changed {
            self.calendars
                .load(&set.calendars.rows, |code| code.clone());
        }
        let mut rows = Vec::new();
        for code in &set.calendars.rows {
            let href = path(CALENDAR, code);
            let mut row = FlexRow::new(code.clone()).slot(
                "calendar",
                render(RecordLink {
                    workspace: false,
                    kind: "core/work_calendar.calendar",
                    record_id: code,
                    href: &href,
                    class: "",
                    body: Html::text(code),
                })?,
            );
            row.dom_id = Some(format!("holiday-set-calendar-{code}"));
            rows.push(row);
        }
        let mut table =
            FlexTable::new("holiday-set-calendars", self.calendars.flex_columns(), rows);
        table.event = "grid_calendars";
        table.zoom = self.calendars.zoom;
        table.suggestions = self.calendars.flex_suggestions();
        table.add_query = &self.calendars.add_query;
        table.caption = "Calendars that include it";
        table.framed = false;
        if set.calendars.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No calendar of this organisation includes it.",
            )));
        }
        let heading = render(SectionHeading {
            title: "Calendars that include it",
            id: "holiday-set-calendars-section",
            class: "mt-6",
            count: Some(set.calendars.total as usize),
            description: "This organisation's calendars only.",
            ..SectionHeading::default()
        })?;
        Ok(vec![
            heading,
            render(Card {
                id: "holiday-set-calendars-card",
                inner_class: "p-0",
                body: render(table)?,
                ..Card::default()
            })?,
        ])
    }

    fn dialog(&self) -> Result<Html> {
        let Some(dialog) = &self.dialog else {
            return Ok(Html::EMPTY);
        };
        let (id, title, description, event, submit, body) = match dialog {
            Dialog::Remove(date, name) => {
                return render(ConfirmDialog {
                    id: "holiday-set-remove-confirm",
                    consequence: &format!("Remove “{name}”?"),
                    detail: &format!(
                        "{} stops being a holiday in every calendar that includes this set.",
                        long_date(*date)
                    ),
                    confirm: "Remove",
                    working: "Removing…",
                    confirm_event: "remove_holiday",
                    cancel_event: "cancel_dialog",
                })
            }
            Dialog::Add(draft) => {
                let mut body = render(
                    Field::in_form("holiday", "date")
                        .id("holiday-set-holiday-date")
                        .label("Date")
                        .kind(InputKind::Date)
                        .value(draft.value("date"))
                        .hint("A date the set already holds is renamed.")
                        .errors(draft.errors("date"))
                        .required(),
                )?;
                body.push(&render(
                    Field::in_form("holiday", "name")
                        .id("holiday-set-holiday-name")
                        .label("Name")
                        .value(draft.value("name"))
                        .attr("maxlength", "120")
                        .errors(draft.errors("name"))
                        .required(),
                )?);
                (
                    "holiday-set-holiday",
                    "Add a holiday",
                    "A whole day. Calendars that include this set close on it.",
                    "add_holiday",
                    "Add holiday",
                    body,
                )
            }
            Dialog::Copy(draft) => {
                let mut body = render(
                    Field::in_form("copy", "code")
                        .id("holiday-set-copy-code")
                        .label("Code")
                        .value(draft.value("code"))
                        .hint("The new set's own code; lowercase letters, digits, _ and -.")
                        .attr("maxlength", "64")
                        .errors(draft.errors("code"))
                        .required(),
                )?;
                body.push(&render(
                    Field::in_form("copy", "name")
                        .id("holiday-set-copy-name")
                        .label("Name")
                        .value(draft.value("name"))
                        .attr("maxlength", "120")
                        .errors(draft.errors("name"))
                        .required(),
                )?);
                body.push(&render(
                    Field::in_form("copy", "from")
                        .id("holiday-set-copy-from")
                        .label("From")
                        .kind(InputKind::Date)
                        .value(draft.value("from"))
                        .hint("Optional: copy holidays from this date. A copy takes at most 400.")
                        .errors(draft.errors("from")),
                )?);
                body.push(&render(
                    Field::in_form("copy", "to")
                        .id("holiday-set-copy-to")
                        .label("To")
                        .kind(InputKind::Date)
                        .value(draft.value("to"))
                        .hint("Optional: copy holidays up to this date.")
                        .errors(draft.errors("to")),
                )?);
                (
                    "holiday-set-copy",
                    "Copy into a new set",
                    "The copy is this organisation's and no longer follows the original. Include it in a calendar in place of the original.",
                    "copy_set",
                    "Copy",
                    body,
                )
            }
        };
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
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

impl LiveState<Scope, Error> for Record {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.year = year(values),
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            if let Some((redirect, message)) = self.created.take() {
                return Ok(LiveBody {
                    redirect: Some(redirect),
                    flash: Some((StatusKind::Success, message)),
                    ..LiveBody::default()
                });
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: HashMap::from([("year".to_owned(), self.shown.to_string())]),
                rows: self.set.as_ref().map_or(0, |set| set.holidays.rows.len()),
                notice: self.notice.clone(),
                ..LiveBody::default()
            })
        })
    }
}
