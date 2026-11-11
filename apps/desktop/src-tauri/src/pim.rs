use crate::backend::{field, Backend};
use anyhow::{bail, ensure, Context, Result};
use munki_pim::{Collection, Contact, Event, EventTime, Kind, Record, Store};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub fn initialize(backend: &Backend) -> Result<()> {
    let store = Store::new(backend.store.conn())?;
    for (kind, name) in [
        (Kind::Calendar, "Personal"),
        (Kind::AddressBook, "Contacts"),
    ] {
        if !store.collections()?.iter().any(|c| c.kind == kind) {
            store.create_collection(name, kind, None)?;
        }
    }
    Ok(())
}
fn collection(store: &Store<'_>, kind: Kind) -> Result<Collection> {
    store
        .collections()?
        .into_iter()
        .find(|c| c.kind == kind)
        .context("Collection missing")
}
fn uid(backend: &Backend) -> Result<String> {
    Ok(backend.store.conn().query_row(
        "SELECT lower(hex(randomblob(16))) || '@munkimail.local'",
        [],
        |r| r.get(0),
    )?)
}
fn event_view(record: &Record) -> Result<Value> {
    let event = record.event()?;
    Ok(
        json!({"id":record.id,"revision":record.revision,"uid":event.uid,"title":event.summary,"description":event.description,"location":event.location,"start":event.start.timestamp()?,"end":event.end.timestamp()?,"all_day":matches!(event.start,EventTime::Date(_)),"attendees":event.attendees,"organizer":event.organizer,"start_date":match &event.start {EventTime::Date(date)=>Some(date.to_string()),_=>None},"end_date":match &event.end {EventTime::Date(date)=>Some(date.to_string()),_=>None},"recurring":event.recurring,"collection":record.collection_id,"dirty":record.dirty}),
    )
}
fn contact_view(record: &Record) -> Result<Value> {
    let contact = record.contact()?;
    Ok(
        json!({"id":record.id,"revision":record.revision,"uid":contact.uid,"name":contact.name,"email":contact.emails.first().cloned().unwrap_or_default(),"phone":contact.phones.first().cloned().unwrap_or_default(),"emails":contact.emails,"phones":contact.phones,"organization":contact.organization,"note":contact.note,"collection":record.collection_id,"dirty":record.dirty}),
    )
}
fn existing(store: &Store<'_>, collection: &str, p: &Value) -> Result<Option<Record>> {
    if let Some(id) = p["id"].as_str().filter(|s| !s.is_empty()) {
        let record = store.get(id)?;
        ensure!(record.collection_id == collection, "Wrong collection");
        return Ok(Some(record));
    }
    Ok(store
        .records(collection)?
        .into_iter()
        .find(|r| !r.deleted && p["uid"].as_str() == Some(r.uid.as_str())))
}
fn revision(p: &Value, record: &Option<Record>) -> Result<Option<i64>> {
    if record.is_some() {
        Ok(Some(p["revision"].as_i64().context(
            "Reload this item before editing; revision required",
        )?))
    } else {
        Ok(None)
    }
}

pub fn dispatch(backend: &mut Backend, action: &str, p: Value) -> Result<Value> {
    initialize(backend)?;
    let store = Store::new(backend.store.conn())?;
    match action {
        "rsvp" => respond(backend, &p),
        "events" => {
            let start = field(&p, "start")?;
            let end = field(&p, "end")?;
            ensure!(
                end > start && end - start <= 366 * 86400,
                "Choose a date range up to one year"
            );
            let records =
                store.events_in_range(&collection(&store, Kind::Calendar)?.id, start, end)?;
            Ok(json!({"events":records.iter().map(event_view).collect::<Result<Vec<_>>>()?}))
        }
        "save_event" => {
            let input = &p["event"];
            let c = collection(&store, Kind::Calendar)?;
            let old = existing(&store, &c.id, input)?;
            if let Some(ref old) = old {
                ensure!(
                    !old.event()?.recurring,
                    "Recurring events are preserved read-only; edit the source calendar"
                );
            }
            let start: i64 = field(input, "start")?;
            let end: i64 = field(input, "end")?;
            ensure!(end > start, "Event end must follow start");
            let all_day = input["all_day"].as_bool().unwrap_or(false);
            let time = |value: i64, key: &str| -> Result<EventTime> {
                if all_day {
                    if let Some(date) = input[key].as_str() {
                        return Ok(EventTime::Date(
                            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")?.to_string(),
                        ));
                    }
                    if let Some(original) = old.as_ref().map(Record::event).transpose()? {
                        let prior = if key == "start_date" {
                            original.start
                        } else {
                            original.end
                        };
                        if matches!(prior, EventTime::Date(_)) && prior.timestamp()? == value {
                            return Ok(prior);
                        }
                    }
                    bail!("All-day edits require start_date/end_date YYYY-MM-DD; end date is exclusive")
                } else {
                    Ok(EventTime::Utc(value))
                }
            };
            let event = Event {
                uid: old.as_ref().map(|r| r.uid.clone()).unwrap_or(uid(backend)?),
                summary: field(input, "title")?,
                description: input["description"].as_str().unwrap_or("").into(),
                location: input["location"].as_str().unwrap_or("").into(),
                start: time(start, "start_date")?,
                end: time(end, "end_date")?,
                organizer: old
                    .as_ref()
                    .and_then(|r| r.event().ok())
                    .and_then(|e| e.organizer),
                attendees: input["attendees"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                recurring: false,
            };
            let record = store.save_event(
                &c.id,
                old.as_ref().map(|r| r.id.as_str()),
                revision(input, &old)?,
                &event,
            )?;
            event_view(&record)
        }
        "delete_event" | "delete_contact" => {
            let kind = if action == "delete_event" {
                Kind::Calendar
            } else {
                Kind::AddressBook
            };
            let c = collection(&store, kind)?;
            let old = existing(&store, &c.id, &p)?.context("Item not found")?;
            let expected = p["revision"]
                .as_i64()
                .context("Reload this item before deleting; revision required")?;
            store.delete(&old.id, expected)?;
            Ok(json!({"ok":true}))
        }
        "contacts" => Ok(
            json!({"contacts":store.contacts(&collection(&store,Kind::AddressBook)?.id)?.iter().map(contact_view).collect::<Result<Vec<_>>>()?}),
        ),
        "save_contact" => {
            let input = &p["contact"];
            let c = collection(&store, Kind::AddressBook)?;
            let old = existing(&store, &c.id, input)?;
            let original = old.as_ref().map(Record::contact).transpose()?;
            let mut emails = original
                .as_ref()
                .map(|c| c.emails.clone())
                .unwrap_or_default();
            let mut phones = original
                .as_ref()
                .map(|c| c.phones.clone())
                .unwrap_or_default();
            let email = input["email"].as_str().unwrap_or("").trim();
            let phone = input["phone"].as_str().unwrap_or("").trim();
            if emails.is_empty() {
                if !email.is_empty() {
                    emails.push(email.into())
                }
            } else if email.is_empty() {
                emails.remove(0);
            } else {
                emails[0] = email.into();
            }
            if phones.is_empty() {
                if !phone.is_empty() {
                    phones.push(phone.into())
                }
            } else if phone.is_empty() {
                phones.remove(0);
            } else {
                phones[0] = phone.into();
            }
            let contact = Contact {
                uid: old.as_ref().map(|r| r.uid.clone()).unwrap_or(uid(backend)?),
                name: field(input, "name")?,
                emails,
                phones,
                organization: input["organization"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| original.as_ref().map(|c| c.organization.clone()))
                    .unwrap_or_default(),
                note: input["note"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| original.as_ref().map(|c| c.note.clone()))
                    .unwrap_or_default(),
            };
            let record = store.save_contact(
                &c.id,
                old.as_ref().map(|r| r.id.as_str()),
                revision(input, &old)?,
                &contact,
            )?;
            contact_view(&record)
        }
        "person" => {
            let email: String = field(&p, "email")?;
            let person = store.person(&email)?;
            let mut messages = Vec::new();
            let accounts = backend
                .store
                .conn()
                .prepare("SELECT id FROM accounts")?
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for account in accounts {
                messages.extend(backend.store.search(
                    account,
                    &munki_core::Query {
                        from: Some(munki_core::Address::new(&email)),
                        ..Default::default()
                    },
                    100,
                )?);
            }
            Ok(
                json!({"messages":messages,"events":person.events.iter().map(event_view).collect::<Result<Vec<_>>>()?,"contact":person.contacts.first().map(contact_view).transpose()?}),
            )
        }
        "pim_import" => {
            let kind: String = field(&p, "kind")?;
            let text: String = field(&p, "text")?;
            let imported = match kind.as_str() {
                "calendar" => store.import_ics(&collection(&store, Kind::Calendar)?.id, &text)?,
                "contacts" => {
                    store.import_vcf(&collection(&store, Kind::AddressBook)?.id, &text)?
                }
                _ => bail!("Unsupported import"),
            };
            Ok(json!({"imported":imported.len()}))
        }
        "pim_export" => {
            let kind = match p["kind"].as_str() {
                Some("calendar") => Kind::Calendar,
                Some("contacts") => Kind::AddressBook,
                _ => bail!("Unsupported export"),
            };
            Ok(json!({"text":store.export_collection(&collection(&store,kind)?.id)?}))
        }
        "dav_conflicts" => {
            let mut conflicts = Vec::new();
            for c in store.collections()? {
                for conflict in store.conflicts(&c.id)? {
                    let record = store.get(&conflict.record_id)?;
                    let mut value = serde_json::to_value(conflict)?;
                    value["id"] = json!(record.id);
                    value["revision"] = json!(record.revision);
                    conflicts.push(value);
                }
            }
            Ok(json!({"conflicts":conflicts}))
        }
        "dav_resolve" => {
            let resolution = match p["resolution"].as_str() {
                Some("local") => munki_pim::Resolution::KeepLocal,
                Some("remote") => munki_pim::Resolution::AcceptRemote,
                _ => bail!("Choose local or remote"),
            };
            store.resolve(
                &field::<String>(&p, "id")?,
                field(&p, "revision")?,
                resolution,
            )?;
            Ok(json!({"ok":true}))
        }
        _ => bail!("Unknown action: {action}"),
    }
}

pub fn seed(backend: &Backend) -> Result<()> {
    initialize(backend)?;
    let store = Store::new(backend.store.conn())?;
    let calendar = collection(&store, Kind::Calendar)?;
    let contacts = collection(&store, Kind::AddressBook)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    store.save_event(
        &calendar.id,
        None,
        None,
        &Event {
            uid: "demo-review@munkimail.demo".into(),
            summary: "Design review".into(),
            description: "A little room for good ideas.".into(),
            location: "Studio".into(),
            start: EventTime::Utc(now + 86400),
            end: EventTime::Utc(now + 90000),
            organizer: None,
            attendees: vec!["maya@studio.example".into()],
            recurring: false,
        },
    )?;
    store.save_contact(
        &contacts.id,
        None,
        None,
        &Contact {
            uid: "demo-maya@munkimail.demo".into(),
            name: "Maya Chen".into(),
            emails: vec!["maya@studio.example".into()],
            phones: vec!["+1 415 555 0128".into()],
            organization: "Studio".into(),
            note: "Design partner".into(),
        },
    )?;
    Ok(())
}

/// Inspect every MIME part, including inline calendar alternatives.
pub(crate) fn invitation(message: &mail_parser::Message<'_>) -> Result<String> {
    let mut candidates = Vec::new();
    for part in message.parts() {
        let Some(ct) = part.content_type() else {
            continue;
        };
        if !ct.ctype().eq_ignore_ascii_case("text")
            || !ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("calendar"))
        {
            continue;
        }
        ensure!(
            part.decoded_len() <= 16 * 1024 * 1024,
            "Invitation exceeds 16 MiB"
        );
        let bytes = part.decoded();
        let raw = std::str::from_utf8(bytes.as_ref())
            .context("Invitation must be UTF-8")?
            .to_owned();
        let method = ct.attribute("method");
        ensure!(
            method.is_none_or(|m| m.eq_ignore_ascii_case("REQUEST")),
            "Calendar part is not an invitation request"
        );
        ensure!(
            raw.lines()
                .any(|line| line.trim_end().eq_ignore_ascii_case("METHOD:REQUEST")),
            "Calendar is not METHOD:REQUEST"
        );
        munki_pim::parse_event(&raw)?;
        candidates.push(raw);
    }
    ensure!(
        candidates.len() == 1,
        "Select a message containing exactly one calendar invitation"
    );
    Ok(candidates.remove(0))
}
fn respond(backend: &Backend, p: &Value) -> Result<Value> {
    ensure!(!backend.demo, "Demo mode cannot queue invitation responses");
    let account = crate::backend::account(p)?;
    let message = backend.store.message(account, field(p, "id")?)?;
    let config = backend.config(account)?;
    let email: String = backend.store.conn().query_row(
        "SELECT email FROM accounts WHERE id=?1",
        [account],
        |r| r.get(0),
    )?;
    let attendee = munki_pim::normalize_email(&email)?;
    ensure!(
        munki_pim::normalize_email(&config.email)? == attendee,
        "Account identity mismatch"
    );
    if let Some(requested) = p["attendee"].as_str() {
        ensure!(
            munki_pim::normalize_email(requested)? == attendee,
            "Attendee must match selected account"
        );
    }
    let status = p["response"]
        .as_str()
        .or_else(|| p["status"].as_str())
        .context("RSVP status required")?;
    let raw = backend
        .store
        .raw_message(account, message.id)?
        .context("Sync invitation MIME first")?;
    let parsed = mail_parser::MessageParser::new()
        .parse(&raw)
        .context("Invalid cached MIME")?;
    let invite = invitation(&parsed)?;
    let event = munki_pim::parse_event(&invite)?;
    let reply = munki_pim::rsvp(&invite, &attendee, status)?;
    let organizer =
        munki_pim::normalize_email(event.organizer.as_deref().context("Organizer required")?)?;
    let draft = munki_core::compose::Draft {
        from: attendee,
        to: vec![organizer],
        subject: format!("{}: {}", status.to_ascii_uppercase(), event.summary),
        text: format!("{}: {}", status.to_ascii_uppercase(), event.summary),
        in_reply_to: Some(message.message_id.trim_matches(['<', '>']).to_owned()),
        attachments: vec![munki_core::compose::Attachment {
            filename: "reply.ics".into(),
            content_type: "text/calendar; method=REPLY; charset=utf-8".into(),
            bytes: reply.as_bytes().to_vec(),
        }],
        ..Default::default()
    };
    // A repeated click for this message/status must not enqueue another SMTP send.
    let duplicate:Option<i64>=backend.store.conn().query_row(
        "SELECT id FROM drafts WHERE account=?1 AND state<>'draft' AND json_extract(content,'$.in_reply_to')=?2 AND json_extract(content,'$.subject')=?3 LIMIT 1",
        rusqlite::params![account,draft.in_reply_to,draft.subject],|r|r.get(0)).optional()?;
    if let Some(draft_id) = duplicate {
        return Ok(json!({"queued":true,"draft_id":draft_id,"duplicate":true}));
    }
    let (message_id, wire) = munki_net::compose::build(&draft)?;
    let store = Store::new(backend.store.conn())?;
    let calendar = collection(&store, Kind::Calendar)?;
    let old = store
        .records(&calendar.id)?
        .into_iter()
        .find(|r| r.uid == event.uid);
    let expected = if old.is_some() {
        Some(
            p["revision"]
                .as_i64()
                .context("Reload event before responding; revision required")?,
        )
    } else {
        None
    };
    // Keep all attendees and alarms in the local event; only the reply narrows them.
    let local = munki_pim::rsvp_local(&invite, &draft.from, status)?;
    let tx = backend.store.conn().unchecked_transaction()?;
    let record = store.save_raw_in_transaction(
        &calendar.id,
        old.as_ref().map(|r| r.id.as_str()),
        expected,
        &local,
    )?;
    tx.execute("INSERT INTO drafts(account,content,state,wire,message_id,sent_folder) VALUES(?1,?2,'queued',?3,?4,?5)",rusqlite::params![account,serde_json::to_string(&draft)?,wire,message_id,config.sent_folder])?;
    let draft_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO outbox(account,kind,created_at) VALUES(?1,?2,unixepoch())",
        rusqlite::params![
            account,
            serde_json::to_string(&munki_core::OpKind::Submit { draft_id })?
        ],
    )?;
    tx.commit()?;
    Ok(json!({"queued":true,"draft_id":draft_id,"event":event_view(&record)?}))
}

pub(crate) fn invite_revision(backend: &Backend, raw: &str) -> Result<Option<i64>> {
    let event = munki_pim::parse_event(raw)?;
    let store = Store::new(backend.store.conn())?;
    let calendar = collection(&store, Kind::Calendar)?;
    Ok(store
        .records(&calendar.id)?
        .into_iter()
        .find(|r| r.uid == event.uid)
        .map(|r| r.revision))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitation_response_is_atomic_and_stale_retries_do_not_send() -> Result<()> {
        let mut backend = Backend {
            store: munki_store::Store::open_in_memory()?,
            path: Default::default(),
            key: None,
            demo: false,
        };
        initialize(&backend)?;
        let account = backend
            .store
            .add_account("alice@example.com", "Alice", "imap")?;
        backend.store.conn().execute_batch(
            "CREATE TABLE account_connections(account INTEGER PRIMARY KEY,config TEXT NOT NULL)",
        )?;
        let config = json!({"email":"alice@example.com","imap_host":"imap.example.com","smtp_host":"smtp.example.com"});
        backend.store.conn().execute(
            "INSERT INTO account_connections VALUES(?1,?2)",
            rusqlite::params![account, config.to_string()],
        )?;
        let calendar="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:invite-test\r\nDTSTART:20261103T180000Z\r\nDTEND:20261103T190000Z\r\nSUMMARY:Review\r\nORGANIZER:mailto:owner@example.com\r\nATTENDEE:mailto:alice@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let raw=format!("From: owner@example.com\r\nTo: alice@example.com\r\nMessage-ID: <invite@example.com>\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n{calendar}");
        let id = backend.store.upsert_message(
            &munki_store::NewMessage {
                account,
                folder: "INBOX",
                uid: 1,
                message_id: "invite@example.com",
                thread_id: "invite@example.com",
                from: munki_core::Address::new("owner@example.com"),
                subject: "Review",
                date: 1,
                seen: false,
                has_attachment: true,
                destination: munki_core::Destination::Imbox,
            },
            "Review",
            "",
        )?;
        backend.store.cache_message(id, raw.as_bytes())?;
        for bad in [
            json!({"account":account,"id":id,"response":"invalid"}),
            json!({"account":account,"id":id,"response":"ACCEPTED","attendee":"stranger@example.com"}),
        ] {
            assert!(respond(&backend, &bad).is_err());
        }
        let result = respond(
            &backend,
            &json!({"account":account,"id":id,"response":"ACCEPTED"}),
        )?;
        let revision = result["event"]["revision"].as_i64().unwrap();
        assert_eq!(backend.store.pending_count(account)?, 1);
        assert!(respond(
            &backend,
            &json!({"account":account,"id":id,"response":"ACCEPTED"})
        )?["duplicate"]
            .as_bool()
            .unwrap());
        assert_eq!(backend.store.pending_count(account)?, 1);
        assert_eq!(invite_revision(&backend, calendar)?, Some(revision));
        assert!(respond(
            &backend,
            &json!({"account":account,"id":id,"response":"DECLINED","revision":revision-1})
        )
        .is_err());
        assert_eq!(backend.store.pending_count(account)?, 1);
        let count: i64 =
            backend
                .store
                .conn()
                .query_row("SELECT COUNT(*) FROM drafts", [], |r| r.get(0))?;
        assert_eq!(count, 1);
        let (_, wire) = backend
            .store
            .draft_wire(account, result["draft_id"].as_i64().unwrap())?;
        let parsed = mail_parser::MessageParser::new().parse(&wire).unwrap();
        assert!(parsed.parts().any(|p| p
            .content_type()
            .is_some_and(|c| c.attribute("method") == Some("REPLY"))));
        Ok(())
    }
}
