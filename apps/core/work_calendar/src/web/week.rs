//! The week a calendar's rule holds, as a form: for each weekday its type
//! and, for a working day, its opening periods ("09:00-13:00, 14:00-18:00").
//! The contract reads the week as `core.work_calendar.create` and
//! `.rule.set` take it; a refusal on `week.monday` lands on Monday's hours.
use super::{render, Draft};
use crate::model::{DayType, Week, WeekDay, WEEKDAYS};
use bilibi_database::Result;
use bilibi_ui::{Choice, Field, Html, InputKind};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

const NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// A contract field in words, for a sentence: `week.monday` is "Monday",
/// `effective_from` "effective from".
pub(super) fn label(field: &str) -> String {
    match field.strip_prefix("week.") {
        Some(day) => WEEKDAYS
            .iter()
            .position(|name| *name == day)
            .map_or_else(|| day.to_owned(), |at| NAMES[at].to_owned()),
        None if field == "week" => "The week".into(),
        None => field.replace('_', " "),
    }
}

/// The form field a contract refusal belongs to: `week.monday` is Monday's
/// hours (or its type, when it is not a working day).
pub(super) fn field(field: &str, values: &HashMap<String, String>) -> Option<String> {
    let day = field.strip_prefix("week.")?;
    WEEKDAYS.contains(&day).then(|| {
        if values.get(&format!("{day}_type")).map(String::as_str) == Some("working") {
            format!("{day}_periods")
        } else {
            format!("{day}_type")
        }
    })
}

/// Monday to Friday 09:00-18:00, Saturday off, Sunday rest.
pub(super) fn defaults(values: &mut HashMap<String, String>) {
    for (at, day) in WEEKDAYS.iter().enumerate() {
        let (kind, periods) = match at {
            5 => ("off", ""),
            6 => ("rest", ""),
            _ => ("working", "09:00-18:00"),
        };
        values.insert(format!("{day}_type"), kind.into());
        values.insert(format!("{day}_periods"), periods.into());
    }
}

/// The form's values for an existing week.
pub(super) fn values(week: &Week, values: &mut HashMap<String, String>) {
    for (day, rule) in WEEKDAYS.iter().zip(&week.0) {
        values.insert(format!("{day}_type"), rule.day_type.as_str().into());
        values.insert(format!("{day}_periods"), periods(rule));
    }
}

/// "09:00-13:00, 14:00-18:00", or nothing for a day that does not open.
pub(super) fn periods(day: &WeekDay) -> String {
    day.periods
        .iter()
        .map(|period| period.text())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One weekday in a list cell: its hours, or the day type.
pub(super) fn summary(day: &WeekDay) -> String {
    match day.day_type {
        DayType::Working => periods(day),
        other => kind(other).into(),
    }
}

pub(super) fn kind(day_type: DayType) -> &'static str {
    match day_type {
        DayType::Working => "Working",
        DayType::Rest => "Rest",
        DayType::Off => "Off",
        DayType::Holiday => "Holiday",
    }
}

/// The hours a person typed, as the contract's list of periods.
pub(super) fn split(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|period| !period.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The week as the contracts take it. Hours typed for a day that does not
/// open are left out, so changing Saturday to Off needs no clearing.
pub(super) fn input(posted: &HashMap<String, String>) -> Value {
    let mut week = Map::new();
    for day in WEEKDAYS {
        let kind = posted
            .get(&format!("{day}_type"))
            .cloned()
            .unwrap_or_default();
        let mut entry = json!({ "type": kind });
        if kind == "working" {
            entry["periods"] = json!(split(
                posted
                    .get(&format!("{day}_periods"))
                    .map_or("", String::as_str)
            ));
        }
        week.insert(day.into(), entry);
    }
    Value::Object(week)
}

/// The fourteen fields, Monday to Sunday: each day's type, then its hours.
pub(super) fn fields(form: &str, id: &str, draft: &Draft) -> Result<Html> {
    let mut html = Html::EMPTY;
    for (day, name) in WEEKDAYS.iter().zip(NAMES) {
        let kind = format!("{day}_type");
        html.push(&render(
            Field::in_form(form, &kind)
                .id(format!("{id}-{day}-type"))
                .label(name)
                .kind(InputKind::Select)
                .options(vec![
                    Choice::new("Working", "working"),
                    Choice::new("Off", "off"),
                    Choice::new("Rest", "rest"),
                ])
                .value(draft.value(&kind))
                .errors(draft.errors(&kind))
                .required(),
        )?);
        let hours = format!("{day}_periods");
        let mut field = Field::in_form(form, &hours)
            .id(format!("{id}-{day}-periods"))
            .label(format!("{name} hours"))
            .value(draft.value(&hours))
            .attr("maxlength", "60")
            .errors(draft.errors(&hours));
        if *day == "monday" {
            field = field.hint(
                "Working days only: up to four periods, HH:MM-HH:MM, separated by commas. An end at or before the start is the next day.",
            );
        }
        html.push(&render(field)?);
    }
    Ok(html)
}
