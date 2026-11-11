use anyhow::{anyhow, ensure, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ical::{parser::ical::component::*, property::Property, IcalParser, VcardParser};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventTime {
    /// ISO date (YYYY-MM-DD). End dates are exclusive.
    Date(String),
    /// UTC Unix seconds.
    Utc(i64),
}
impl EventTime {
    pub fn timestamp(&self) -> Result<i64> {
        match self {
            Self::Date(s) => Ok(NaiveDate::parse_from_str(s, "%Y-%m-%d")?
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()),
            Self::Utc(t) => {
                DateTime::from_timestamp(*t, 0).context("timestamp out of range")?;
                Ok(*t)
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventTime,
    pub end: EventTime,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    /// Read-only indication: recurrence rules and exceptions stay in the source.
    pub recurring: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub uid: String,
    pub name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: String,
    pub note: String,
}
/// Trim, remove mailto:, lowercase. No provider-specific alias or dot stripping.
pub fn normalize_email(input: &str) -> Result<String> {
    let s = input.trim();
    let s = if s
        .get(..7)
        .is_some_and(|v| v.eq_ignore_ascii_case("mailto:"))
    {
        &s[7..]
    } else {
        s
    };
    ensure!(
        !s.chars().any(|c| c.is_whitespace() || c.is_control()),
        "invalid email"
    );
    let (local, domain) = s.split_once('@').context("email requires @")?;
    ensure!(
        !local.is_empty() && !domain.is_empty() && !domain.contains('@'),
        "invalid email"
    );
    Ok(s.to_lowercase())
}
fn named(p: &Property, name: &str) -> bool {
    p.name
        .rsplit('.')
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case(name))
}
fn value<'a>(p: &'a [Property], name: &str) -> Option<&'a str> {
    p.iter()
        .find(|p| named(p, name))
        .and_then(|p| p.value.as_deref())
}
fn values(p: &[Property], name: &str) -> Vec<String> {
    p.iter()
        .filter(|p| named(p, name))
        .filter_map(|p| p.value.as_deref())
        .map(unescape)
        .collect()
}
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(c) => out.push(c),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}
fn text(p: &[Property], name: &str) -> String {
    value(p, name).map(unescape).unwrap_or_default()
}
fn parse_time(p: &Property) -> Result<EventTime> {
    let s = p.value.as_deref().context("missing date value")?;
    if s.len() == 8 {
        return Ok(EventTime::Date(
            NaiveDate::parse_from_str(s, "%Y%m%d")?.to_string(),
        ));
    }
    if s.ends_with('Z') {
        return Ok(EventTime::Utc(
            NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ")?
                .and_utc()
                .timestamp(),
        ));
    }
    let tz = p
        .params
        .as_ref()
        .and_then(|p| p.iter().find(|(k, _)| k.eq_ignore_ascii_case("TZID")))
        .and_then(|(_, v)| v.first())
        .context("floating time unsupported; use UTC or IANA TZID")?;
    let tz: chrono_tz::Tz = tz
        .trim_matches('"')
        .parse()
        .map_err(|_| anyhow!("unsupported TZID: {tz}"))?;
    // Ambiguous/nonexistent wall times must never silently move an appointment.
    let dt = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S")?;
    Ok(EventTime::Utc(
        tz.from_local_datetime(&dt)
            .single()
            .context("ambiguous or nonexistent local time")?
            .timestamp(),
    ))
}
pub(crate) fn calendars(raw: &str) -> Result<Vec<IcalCalendar>> {
    ensure!(raw.len() <= 16 * 1024 * 1024, "ICS too large");
    let c: Vec<_> = IcalParser::new(Cursor::new(raw)).collect::<std::result::Result<_, _>>()?;
    ensure!(!c.is_empty(), "no VCALENDAR");
    Ok(c)
}
pub(crate) fn cards(raw: &str) -> Result<Vec<ical::parser::vcard::component::VcardContact>> {
    ensure!(raw.len() <= 16 * 1024 * 1024, "VCF too large");
    let c: Vec<_> = VcardParser::new(Cursor::new(raw)).collect::<std::result::Result<_, _>>()?;
    ensure!(!c.is_empty(), "no VCARD");
    Ok(c)
}
pub(crate) fn event(raw: &str) -> Result<Event> {
    let c = calendars(raw)?;
    ensure!(
        c.len() == 1 && !c[0].events.is_empty(),
        "expected one calendar resource"
    );
    ensure!(
        c[0].todos.is_empty()
            && c[0].journals.is_empty()
            && c[0].free_busys.is_empty()
            && c[0].alarms.is_empty(),
        "non-event calendar components unsupported"
    );
    let e = c[0]
        .events
        .iter()
        .find(|e| value(&e.properties, "RECURRENCE-ID").is_none())
        .context("recurrence master required")?;
    let p = &e.properties;
    let uid = unescape(value(p, "UID").context("event UID required")?);
    ensure!(!uid.is_empty() && c[0].events.iter().all(|e| value(&e.properties,"UID").map(unescape).as_deref()==Some(uid.as_str())), "resource must contain one UID");
    let start = parse_time(
        p.iter()
            .find(|p| named(p, "DTSTART"))
            .context("DTSTART required")?,
    )?;
    let end = if let Some(p) = p.iter().find(|p| named(p, "DTEND")) {
        parse_time(p)?
    } else {
        ensure!(
            value(p, "DURATION").is_none(),
            "DURATION unsupported; provide DTEND"
        );
        match &start {
            EventTime::Date(d) => EventTime::Date(
                NaiveDate::parse_from_str(d, "%Y-%m-%d")?
                    .succ_opt()
                    .context("date overflow")?
                    .to_string(),
            ),
            t => t.clone(),
        }
    };
    let e = Event {
        uid,
        summary: text(p, "SUMMARY"),
        description: text(p, "DESCRIPTION"),
        location: text(p, "LOCATION"),
        start,
        end,
        organizer: value(p, "ORGANIZER").map(str::to_owned),
        attendees: values(p, "ATTENDEE"),
        recurring: value(p, "RRULE").is_some()
            || value(p, "RDATE").is_some()
            || c[0].events.len() > 1,
    };
    validate_event(&e)?;
    Ok(e)
}
pub(crate) fn contact(raw: &str) -> Result<Contact> {
    let c = cards(raw)?;
    ensure!(c.len() == 1, "expected one VCARD");
    let p = &c[0].properties;
    ensure!(
        matches!(value(p, "VERSION"), Some("3.0" | "4.0")),
        "only vCard 3.0/4.0 supported"
    );
    let c = Contact {
        uid: unescape(value(p, "UID").context("contact UID required")?),
        name: text(p, "FN"),
        emails: values(p, "EMAIL"),
        phones: values(p, "TEL"),
        organization: text(p, "ORG"),
        note: text(p, "NOTE"),
    };
    validate_contact(&c)?;
    Ok(c)
}
fn validate_event(e: &Event) -> Result<()> {
    ensure!(!e.uid.is_empty(), "UID required");
    ensure!(
        std::mem::discriminant(&e.start) == std::mem::discriminant(&e.end),
        "start/end types differ"
    );
    ensure!(
        e.end.timestamp()? >= e.start.timestamp()?,
        "end before start"
    );
    if matches!(e.start, EventTime::Date(_)) {
        ensure!(
            e.end.timestamp()? > e.start.timestamp()?,
            "all-day end must be exclusive"
        );
    }
    for a in e.attendees.iter().chain(e.organizer.iter()) {
        normalize_email(a)?;
    }
    Ok(())
}
fn validate_contact(c: &Contact) -> Result<()> {
    ensure!(
        !c.uid.is_empty() && !c.name.trim().is_empty(),
        "UID and name required"
    );
    for e in &c.emails {
        normalize_email(e)?;
    }
    Ok(())
}
fn prop(name: &str, value: String) -> Property {
    Property {
        name: name.into(),
        params: None,
        value: Some(value),
    }
}
fn set(p: &mut Vec<Property>, name: &str, vals: Vec<String>) {
    p.retain(|p| !named(p, name));
    p.extend(vals.into_iter().map(|v| prop(name, v)));
}
fn set_text(p: &mut Vec<Property>, name: &str, s: &str) {
    if value(p, name).map(unescape).as_deref() == Some(s) {
        return;
    }
    if let Some(existing) = p.iter_mut().find(|p| named(p, name)) {
        existing.value = Some(escape(s));
    } else {
        p.push(prop(name, escape(s)));
    }
}
fn set_emails(p: &mut Vec<Property>, name: &str, emails: &[String], mailto: bool) -> Result<()> {
    let mut old: Vec<_> = p.iter().filter(|p| named(p, name)).cloned().collect();
    let mut new = Vec::new();
    for e in emails {
        let normalized = normalize_email(e)?;
        if let Some(i) = old.iter().position(|p| {
            p.value
                .as_deref()
                .and_then(|v| normalize_email(v).ok())
                .as_deref()
                == Some(normalized.as_str())
        }) {
            new.push(old.remove(i));
        } else {
            new.push(prop(
                name,
                if mailto {
                    format!("mailto:{normalized}")
                } else {
                    normalized
                },
            ));
        }
    }
    p.retain(|p| !named(p, name));
    p.extend(new);
    Ok(())
}
fn time_prop(name: &str, t: &EventTime) -> Result<Property> {
    Ok(match t {
        EventTime::Date(s) => Property {
            name: name.into(),
            params: Some(vec![("VALUE".into(), vec!["DATE".into()])]),
            value: Some(
                NaiveDate::parse_from_str(s, "%Y-%m-%d")?
                    .format("%Y%m%d")
                    .to_string(),
            ),
        },
        EventTime::Utc(t) => prop(
            name,
            DateTime::from_timestamp(*t, 0)
                .context("invalid timestamp")?
                .format("%Y%m%dT%H%M%SZ")
                .to_string(),
        ),
    })
}
pub(crate) fn write_event(e: &Event, source: Option<&str>) -> Result<String> {
    validate_event(e)?;
    ensure!(
        source.is_some() || !e.recurring,
        "create recurrence using save_raw/import_ics"
    );
    let mut c = if let Some(raw) = source {
        let old = event(raw)?;
        ensure!(
            !old.recurring || (old.start == e.start && old.end == e.end),
            "recurring times are read-only; use a recurrence-aware editor with save_raw"
        );
        let mut c = calendars(raw)?;
        ensure!(c.len() == 1, "expected one calendar");
        c.remove(0)
    } else {
        IcalCalendar::new()
    };
    if c.events.is_empty() {
        c.events.push(IcalEvent::new());
        c.properties = vec![
            prop("VERSION", "2.0".into()),
            prop("PRODID", "-//MunkiMail//PIM//EN".into()),
        ];
    }
    let master = c
        .events
        .iter_mut()
        .find(|e| value(&e.properties, "RECURRENCE-ID").is_none())
        .context("missing master")?;
    let p = &mut master.properties;
    if let Some(uid) = value(p, "UID") {
        ensure!(unescape(uid) == e.uid, "UID is immutable");
    }
    set_text(p, "UID", &e.uid);
    set_text(p, "SUMMARY", &e.summary);
    set_text(p, "DESCRIPTION", &e.description);
    set_text(p, "LOCATION", &e.location);
    for (name, t) in [("DTSTART", &e.start), ("DTEND", &e.end)] {
        if !p
            .iter()
            .find(|p| named(p, name))
            .and_then(|p| parse_time(p).ok())
            .is_some_and(|old| old == *t)
        {
            p.retain(|p| !named(p, name));
            p.push(time_prop(name, t)?);
        }
    }
    p.retain(|p| !named(p, "DURATION") && !named(p, "DTSTAMP"));
    p.push(time_prop(
        "DTSTAMP",
        &EventTime::Utc(Utc::now().timestamp()),
    )?);
    set_emails(p, "ATTENDEE", &e.attendees, true)?;
    set_emails(
        p,
        "ORGANIZER",
        &e.organizer.iter().cloned().collect::<Vec<_>>(),
        true,
    )?;
    let raw = emit_calendar(&c);
    event(&raw)?;
    Ok(raw)
}
pub(crate) fn write_contact(c: &Contact, source: Option<&str>) -> Result<String> {
    validate_contact(c)?;
    let mut p = if let Some(raw) = source {
        cards(raw)?.remove(0).properties
    } else {
        vec![
            prop("VERSION", "3.0".into()),
            prop("N", format!(";{};;;", escape(&c.name))),
        ]
    };
    if let Some(uid) = value(&p, "UID") {
        ensure!(unescape(uid) == c.uid, "UID is immutable");
    }
    set_text(&mut p, "UID", &c.uid);
    set_text(&mut p, "FN", &c.name);
    set_text(&mut p, "ORG", &c.organization);
    set_text(&mut p, "NOTE", &c.note);
    set_emails(&mut p, "EMAIL", &c.emails, false)?;
    if values(&p, "TEL") != c.phones {
        set(&mut p, "TEL", c.phones.iter().map(|s| escape(s)).collect());
    }
    let raw = component("VCARD", &p, "");
    contact(&raw)?;
    Ok(raw)
}
// Serialization only; all RFC content-line parsing is delegated to ical.
fn component(name: &str, p: &[Property], children: &str) -> String {
    let mut out = format!("BEGIN:{name}\r\n");
    for p in p {
        let mut line = p.name.clone();
        if let Some(params) = &p.params {
            for (key, values) in params {
                line.push(';');
                line.push_str(key);
                line.push('=');
                line.push_str(
                    &values
                        .iter()
                        .map(|v| {
                            if v.starts_with('"') && v.ends_with('"') {
                                v.clone()
                            } else if v.contains([':', ';', ',']) {
                                format!("\"{v}\"")
                            } else {
                                v.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
        }
        line.push(':');
        line.push_str(p.value.as_deref().unwrap_or(""));
        let mut width = 0;
        for ch in line.chars() {
            if width + ch.len_utf8() > 75 {
                out.push_str("\r\n ");
                width = 1;
            }
            out.push(ch);
            width += ch.len_utf8();
        }
        out.push_str("\r\n");
    }
    out.push_str(children);
    out.push_str(&format!("END:{name}\r\n"));
    out
}
pub(crate) fn emit_calendar(c: &IcalCalendar) -> String {
    let mut children = String::new();
    for tz in &c.timezones {
        let trans = tz
            .transitions
            .iter()
            .map(|t| {
                component(
                    match t.transition {
                        IcalTimeZoneTransitionType::STANDARD => "STANDARD",
                        IcalTimeZoneTransitionType::DAYLIGHT => "DAYLIGHT",
                    },
                    &t.properties,
                    "",
                )
            })
            .collect::<String>();
        children.push_str(&component("VTIMEZONE", &tz.properties, &trans));
    }
    for e in &c.events {
        let alarms = e
            .alarms
            .iter()
            .map(|a| component("VALARM", &a.properties, ""))
            .collect::<String>();
        children.push_str(&component("VEVENT", &e.properties, &alarms));
    }
    component("VCALENDAR", &c.properties, &children)
}
pub(crate) fn split_ics(raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for mut c in calendars(raw)? {
        ensure!(
            c.todos.is_empty()
                && c.journals.is_empty()
                && c.free_busys.is_empty()
                && c.alarms.is_empty(),
            "non-event components unsupported"
        );
        let mut groups: std::collections::BTreeMap<String, Vec<IcalEvent>> = Default::default();
        for e in std::mem::take(&mut c.events) {
            let uid = value(&e.properties, "UID")
                .context("event UID required")?
                .to_owned();
            groups.entry(uid).or_default().push(e);
        }
        for (_, events) in groups {
            c.events = events;
            let raw = emit_calendar(&c);
            event(&raw)?;
            out.push(raw);
        }
    }
    ensure!(!out.is_empty(), "no events");
    Ok(out)
}
pub(crate) fn split_vcf(raw: &str) -> Result<Vec<String>> {
    cards(raw)?
        .into_iter()
        .map(|mut c| {
            if value(&c.properties, "UID").is_none() {
                c.properties
                    .push(prop("UID", uuid::Uuid::new_v4().to_string()));
            }
            let raw = component("VCARD", &c.properties, "");
            contact(&raw)?;
            Ok(raw)
        })
        .collect()
}
type Metadata = (String, Option<i64>, Option<i64>, Vec<String>);
pub(crate) fn metadata(raw: &str, kind: crate::Kind) -> Result<Metadata> {
    match kind {
        crate::Kind::Calendar => {
            let e = event(raw)?;
            let emails = e
                .attendees
                .iter()
                .chain(e.organizer.iter())
                .map(|s| normalize_email(s))
                .collect::<Result<_>>()?;
            Ok((
                e.uid,
                Some(e.start.timestamp()?),
                Some(e.end.timestamp()?),
                emails,
            ))
        }
        crate::Kind::AddressBook => {
            let c = contact(raw)?;
            Ok((
                c.uid,
                None,
                None,
                c.emails
                    .iter()
                    .map(|s| normalize_email(s))
                    .collect::<Result<_>>()?,
            ))
        }
    }
}

/// Parse one calendar resource (master plus optional recurrence overrides).
pub fn parse_event(raw: &str) -> Result<Event> {
    event(raw)
}
/// Parse a single vCard 3.0/4.0 resource.
pub fn parse_contact(raw: &str) -> Result<Contact> {
    contact(raw)
}
/// Build an iTIP METHOD:REPLY for one VEVENT. The caller sends it to ORGANIZER.
/// Preserves UID, SEQUENCE, times, recurrence identity and timezone definitions.
/// Does not modify the local store or send mail. Status is accepted/declined/tentative.
pub fn rsvp(raw: &str, attendee: &str, status: &str) -> Result<String> {
    let status = match status.to_ascii_lowercase().as_str() {
        "accepted" => "ACCEPTED",
        "declined" => "DECLINED",
        "tentative" => "TENTATIVE",
        _ => return Err(anyhow!("invalid RSVP status")),
    };
    let email = normalize_email(attendee)?;
    let mut calendars = calendars(raw)?;
    ensure!(
        calendars.len() == 1 && calendars[0].events.len() == 1,
        "RSVP requires exactly one VEVENT; select an instance first"
    );
    let mut c = calendars.remove(0);
    let p = &mut c.events[0].properties;
    ensure!(
        value(p, "UID").is_some_and(|u| !u.is_empty()),
        "UID required"
    );
    ensure!(value(p, "DTSTART").is_some(), "DTSTART required");
    normalize_email(value(p, "ORGANIZER").context("ORGANIZER required")?)?;
    let mut selected = p
        .iter()
        .find(|p| {
            named(p, "ATTENDEE")
                && p.value
                    .as_deref()
                    .and_then(|v| normalize_email(v).ok())
                    .as_deref()
                    == Some(email.as_str())
        })
        .cloned()
        .context("attendee is not invited")?;
    let params = selected.params.get_or_insert_with(Vec::new);
    params.retain(|(k, _)| {
        !k.eq_ignore_ascii_case("PARTSTAT")
            && !k.eq_ignore_ascii_case("RSVP")
            && !k.eq_ignore_ascii_case("SCHEDULE-STATUS")
    });
    params.push(("PARTSTAT".into(), vec![status.into()]));
    params.push(("RSVP".into(), vec!["FALSE".into()]));
    p.retain(|p| !named(p, "ATTENDEE") && !named(p, "DTSTAMP"));
    p.push(selected);
    p.push(time_prop(
        "DTSTAMP",
        &EventTime::Utc(Utc::now().timestamp()),
    )?);
    c.events[0].alarms.clear();
    c.todos.clear();
    c.journals.clear();
    c.free_busys.clear();
    c.alarms.clear();
    set(&mut c.properties, "METHOD", vec!["REPLY".into()]);
    if value(&c.properties, "VERSION").is_none() {
        c.properties.push(prop("VERSION", "2.0".into()));
    }
    if value(&c.properties, "PRODID").is_none() {
        c.properties
            .push(prop("PRODID", "-//MunkiMail//PIM//EN".into()));
    }
    Ok(emit_calendar(&c))
}

/// Update the selected attendee locally, retaining other attendees, alarms and
/// recurrence data. Unlike the outbound reply, the stored calendar has no METHOD.
pub fn rsvp_local(raw: &str, attendee: &str, status: &str) -> Result<String> {
    let reply = rsvp(raw, attendee, status)?;
    let reply = calendars(&reply)?;
    let selected = reply[0].events[0]
        .properties
        .iter()
        .find(|p| named(p, "ATTENDEE"))
        .context("reply attendee missing")?
        .clone();
    let email = normalize_email(attendee)?;
    let mut c = calendars(raw)?.remove(0);
    for p in &mut c.events[0].properties {
        if named(p, "ATTENDEE")
            && p.value
                .as_deref()
                .and_then(|v| normalize_email(v).ok())
                .as_deref()
                == Some(email.as_str())
        {
            *p = selected.clone();
        }
    }
    c.properties.retain(|p| !named(p, "METHOD"));
    let raw = emit_calendar(&c);
    event(&raw)?;
    Ok(raw)
}
