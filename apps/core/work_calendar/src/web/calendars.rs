//! The calendars list (`/admin/work-calendars`) and its new-calendar dialog.
//! A new calendar starts in the current company's `localization.timezone`
//! and goes to its record once created.
use super::{
    clear_flash, flash_map, path, render, today_in, week, Answer, Draft, Flash, Pages, Standing,
    View, CALENDAR,
};
use crate::{model::Listing, store::IndexRow};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    lists::RecordLink,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome,
    CreateButton, Field, FilterToolbar, FlexRow, FlexTable, Form, Header, Html, InputKind,
    ListColumns, ListSpec, ListState, Modal, ModalWidth, PageCounts, PageVariant, Pagination,
    StatusKind, TableEmpty, ToolbarControl,
};
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
    .expect("work calendars list spec")
});

fn columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("name", "Calendar"),
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("status", "Status"),
        BuiltinColumn::new("zone", "Time zone"),
        BuiltinColumn::new("sets", "Holiday sets"),
    ]
}

pub(super) fn status_badge(status: &str) -> Result<Html> {
    render(if status == "active" {
        Badge::status(BadgeKind::Success, "Active")
    } else {
        Badge::status(BadgeKind::Neutral, "Retired")
    })
}

pub(super) fn substitutes() -> Vec<Choice> {
    vec![
        Choice::new("No substitute", "none"),
        Choice::new("A holiday on a rest day moves", "rest"),
        Choice::new("A holiday on a rest or off day moves", "rest_and_off"),
    ]
}

pub(super) struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    framed: bool,
    rows: usize,
    flash: Flash,
    draft: Option<Draft>,
    /// The new calendar's record and its message, once created.
    created: Option<(String, String)>,
}

impl Index {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("work-calendars", columns())
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
            "new" => {
                let mut draft = Draft::default();
                let zone = self.pages.calendars.company_zone(scope).await?;
                draft
                    .values
                    .insert("effective_from".into(), today_in(&zone).to_string());
                draft.values.insert("timezone".into(), zone);
                draft.values.insert("substitute".into(), "none".into());
                week::defaults(&mut draft.values);
                self.draft = Some(draft);
            }
            "close_new" => self.draft = None,
            "create" => {
                let posted = params::form_values_checked(value, "calendar", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).map_or("", |v| v.trim()).to_owned();
                let code = field("code");
                let name = field("name");
                let input = json!({
                    "code": code,
                    "name": name,
                    "timezone": field("timezone"),
                    "substitute": field("substitute"),
                    "effective_from": field("effective_from"),
                    "week": week::input(&posted),
                });
                let answer = self
                    .pages
                    .submit(
                        scope,
                        credential,
                        "create",
                        "core.work_calendar.create",
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
                            path(CALENDAR, &code),
                            format!("Calendar “{name}” was created."),
                        ));
                    }
                    Answer::Fields(fields) => {
                        let values = draft.values.clone();
                        let rest = draft.refuse(fields, |field| {
                            ["code", "name", "timezone", "substitute", "effective_from"]
                                .contains(&field)
                                .then(|| field.to_owned())
                                .or_else(|| week::field(field, &values))
                        });
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
            .calendar_index(scope, &listing(&self.state))
            .await?;
        let mut counts = PageCounts::new(self.state.page, self.state.page_size, found.total as u64);
        if self.state.clamp_to_last_page(counts.total_pages, true) {
            found = self
                .pages
                .calendars
                .calendar_index(scope, &listing(&self.state))
                .await?;
            counts = PageCounts::new(self.state.page, self.state.page_size, found.total as u64);
        }
        self.rows = found.rows.len();
        if self.rows_changed {
            self.columns.load(&found.rows, |row| row.code.clone());
        }
        let actions = if standing.manages() {
            render(CreateButton {
                noun: "Calendar",
                id: "work-calendar-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Work calendars"),
            subtitle: "When the organisation does business: working hours, rest days and holidays.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "work-calendars-filters",
            vec![ToolbarControl::search(
                "work-calendars-search",
                "search",
                "Search calendars",
                &form["search"],
                "Search by code or name...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(found.rows.len());
        for calendar in &found.rows {
            rows.push(self.row(calendar)?);
        }
        let mut table = FlexTable::new("work-calendars", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Work calendars";
        table.framed = false;
        if found.total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                if self.state.search.is_empty() {
                    "No work calendar yet."
                } else {
                    "No calendar matches this search."
                },
            )));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "work-calendars-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "work-calendars-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.draft {
            Some(draft) => self.new_dialog(draft)?,
            None => Html::EMPTY,
        };
        View {
            id: "work-calendars-page",
            variant: PageVariant::List,
            flash: flash_map(&self.flash),
            header,
            parts: vec![toolbar, card],
            dialog,
        }
        .html()
    }

    fn row(&self, calendar: &IndexRow) -> Result<FlexRow> {
        let href = path(CALENDAR, &calendar.code);
        let mut row = FlexRow::new(calendar.code.clone())
            .slot(
                "name",
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/work_calendar.calendar",
                    record_id: &calendar.code,
                    href: &href,
                    class: "",
                    body: Html::text(&calendar.name),
                })?,
            )
            .slot("code", Html::text(&calendar.code))
            .slot("status", status_badge(&calendar.status)?)
            .slot("zone", Html::text(calendar.zone.as_deref().unwrap_or("—")))
            .slot(
                "sets",
                Html::text(&if calendar.sets.is_empty() {
                    "—".to_owned()
                } else {
                    calendar.sets.join(", ")
                }),
            );
        row.dom_id = Some(format!("work-calendar-{}", calendar.code));
        Ok(row)
    }

    fn new_dialog(&self, draft: &Draft) -> Result<Html> {
        let mut body = Html::EMPTY;
        body.push(&render(
            Field::in_form("calendar", "code")
                .id("work-calendar-new-code")
                .label("Code")
                .value(draft.value("code"))
                .hint("Lowercase letters, digits, _ and -. Other modules store it, so it never changes.")
                .attr("maxlength", "64")
                .errors(draft.errors("code"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("calendar", "name")
                .id("work-calendar-new-name")
                .label("Name")
                .value(draft.value("name"))
                .attr("maxlength", "120")
                .errors(draft.errors("name"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("calendar", "timezone")
                .id("work-calendar-new-timezone")
                .label("Time zone")
                .value(draft.value("timezone"))
                .hint("An IANA zone, such as Asia/Kuala_Lumpur. Opening hours are read in it.")
                .attr("maxlength", "64")
                .errors(draft.errors("timezone"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("calendar", "substitute")
                .id("work-calendar-new-substitute")
                .label("Holiday substitutes")
                .kind(InputKind::Select)
                .options(substitutes())
                .value(draft.value("substitute"))
                .errors(draft.errors("substitute")),
        )?);
        body.push(&render(
            Field::in_form("calendar", "effective_from")
                .id("work-calendar-new-effective-from")
                .label("Week starts on")
                .kind(InputKind::Date)
                .value(draft.value("effective_from"))
                .hint("The week also governs any earlier date.")
                .errors(draft.errors("effective_from"))
                .required(),
        )?);
        body.push(&week::fields("calendar", "work-calendar-new", draft)?);
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
            body: Html::text("Create calendar"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "work-calendar-new-form",
            event: "create",
            body,
            actions,
            ..Form::default()
        })?;
        render(Modal {
            id: "work-calendar-new-dialog",
            title: "New work calendar",
            description: "Its week, from a date. Add holiday sets and exceptions on its page.",
            cancel_event: "close_new",
            width: ModalWidth::Wide,
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

impl LiveState<Scope, bilibi_database::Error> for Index {
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
