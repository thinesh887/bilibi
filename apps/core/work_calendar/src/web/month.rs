//! One calendar's month (`/admin/work-calendars/{code}/month?month=2026-11`):
//! each date resolved as a day tile, with its type, its hours and why, in
//! whole weeks from Monday. The previous and next months are links, so each
//! month has its own address and Back returns to it.
use super::{calendar_today, long_date, path, render, week, Pages, View, CALENDAR, MONTH};
use crate::model::{Day, DayType, Reason};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    ActionLink, BackLink, Card, DayKind, DayMonth, DayTile, Header, Html, PageVariant, NO_FLASH,
};
use jiff::{civil::Date, Span};
use serde_json::Value;
use std::collections::HashMap;

pub(super) struct Month {
    pages: Pages,
    code: String,
    /// The first of the month asked for; none is this month in the
    /// calendar's zone.
    asked: Option<Date>,
    /// The first of the month shown.
    first: Date,
    name: String,
}

/// `month=YYYY-MM`, if valid.
fn first_of(query: &HashMap<String, String>) -> Option<Date> {
    query.get("month").and_then(|month| {
        let (year, month) = month.split_once('-')?;
        if month.len() != 2 {
            return None;
        }
        Date::new(year.parse().ok()?, month.parse().ok()?, 1).ok()
    })
}
fn key(first: Date) -> String {
    first.strftime("%Y-%m").to_string()
}

fn kind(day_type: DayType) -> DayKind {
    match day_type {
        DayType::Working => DayKind::Working,
        DayType::Rest => DayKind::Rest,
        DayType::Off => DayKind::Off,
        DayType::Holiday => DayKind::Holiday,
    }
}

/// Why the date is what it is, in a few words: nothing for the week.
fn why(day: &Day) -> Option<String> {
    match &day.reason {
        Reason::Week => None,
        Reason::Holiday { name, .. } => Some(name.clone()),
        Reason::Substitute { name, holiday, .. } => Some(format!(
            "{name}, substitute for {}",
            holiday.strftime("%-d %b")
        )),
        Reason::Exception { name } => Some(name.clone()),
    }
}

/// The tile's lines: why, then the opening hours.
fn lines(day: &Day) -> Vec<String> {
    let mut lines: Vec<String> = why(day).into_iter().collect();
    lines.extend(day.periods.iter().map(|period| period.text()));
    lines
}

/// The whole day in words: "Monday 31 August 2026: holiday, National Day
/// (week: working)".
fn description(day: &Day) -> String {
    let mut text = format!(
        "{}: {}",
        long_date(day.date),
        week::kind(day.day_type).to_lowercase()
    );
    if !day.periods.is_empty() {
        let hours: Vec<String> = day.periods.iter().map(|period| period.text()).collect();
        text.push_str(&format!(", {}", hours.join(" and ")));
    }
    if let Some(why) = why(day) {
        text.push_str(&format!(", {why}"));
    }
    if day.weekly != day.day_type {
        text.push_str(&format!(
            " (week: {})",
            week::kind(day.weekly).to_lowercase()
        ));
    }
    text
}

impl Month {
    pub(super) fn new(pages: Pages, code: String, query: &HashMap<String, String>) -> Self {
        let asked = first_of(query);
        Self {
            pages,
            code,
            asked,
            first: asked.unwrap_or_else(|| super::today_in("UTC").first_of_month()),
            name: String::new(),
        }
    }
    pub(super) fn title(&self) -> String {
        format!(
            "{}: {}",
            if self.name.is_empty() {
                &self.code
            } else {
                &self.name
            },
            self.first.strftime("%B %Y")
        )
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let calendar = self.pages.calendars.calendar(scope, &self.code, 1).await?;
        self.name = calendar.name.clone();
        self.first = self
            .asked
            .unwrap_or_else(|| calendar_today(&calendar).first_of_month());
        let first = self.first;
        let last = first.last_of_month();
        let back = |days: i8| Span::new().days(i64::from(days));
        let start = first
            .checked_sub(back(first.weekday().to_monday_zero_offset()))
            .map_err(|_| Error::Invalid)?;
        let end = last
            .checked_add(back(6 - last.weekday().to_monday_zero_offset()))
            .map_err(|_| Error::Invalid)?;
        let days = self
            .pages
            .calendars
            .days(scope, &self.code, start, end)
            .await?;
        let mut tiles = Vec::with_capacity(days.len());
        for day in &days {
            let date = day.date.day().to_string();
            let description = description(day);
            tiles.push(render(DayTile {
                id: &format!("work-calendar-day-{}", day.date),
                kind: kind(day.day_type),
                date: &date,
                description: &description,
                outside: day.date.month() != first.month(),
                lines: lines(day),
            })?);
        }
        let month = first.strftime("%B %Y").to_string();
        let grid = render(DayMonth {
            id: "work-calendar-month-grid",
            caption: &month,
            tiles,
        })?;
        let link = |date: Date| format!("{}?month={}", path(MONTH, &self.code), key(date));
        let previous = first
            .checked_sub(Span::new().months(1))
            .map_err(|_| Error::Invalid)?;
        let next = first
            .checked_add(Span::new().months(1))
            .map_err(|_| Error::Invalid)?;
        let record = path(CALENDAR, &self.code);
        let mut actions = render(BackLink {
            href: &record,
            id: "work-calendar-month-back",
            title: "Back to the calendar",
            class: "",
        })?;
        for (id, icon, date, title) in [
            (
                "work-calendar-month-previous",
                "chevron-left",
                previous,
                "Previous month",
            ),
            (
                "work-calendar-month-next",
                "chevron-right",
                next,
                "Next month",
            ),
        ] {
            let href = link(date);
            actions.push(&render(ActionLink {
                id,
                icon,
                title,
                href: &href,
                method: "",
                body: Html::text(&date.strftime("%B %Y").to_string()),
            })?);
        }
        let zone = days.first().map_or("", |day| day.timezone.as_str());
        let subtitle = format!("{month}. Opening hours are in {zone}.");
        let header = render(Header {
            title: Html::text(&calendar.name),
            subtitle: &subtitle,
            actions,
            ..Header::default()
        })?;
        let card = render(Card {
            id: "work-calendar-month",
            title: &month,
            body: grid,
            ..Card::default()
        })?;
        View {
            id: "work-calendar-month-page",
            variant: PageVariant::Detail,
            flash: NO_FLASH,
            header,
            parts: vec![card],
            dialog: Html::EMPTY,
        }
        .html()
    }
}

impl LiveState<Scope, Error> for Month {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.asked = first_of(values),
                // The month has no controls; a dismissed flash is the only event.
                LiveInput::Event(value) => {
                    if value.get("type").and_then(Value::as_str) != Some("clear-flash") {
                        return Err(Error::Invalid);
                    }
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: HashMap::from([("month".to_owned(), key(self.first))]),
                rows: 0,
                ..LiveBody::default()
            })
        })
    }
}
