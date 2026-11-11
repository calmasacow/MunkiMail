use munki_pim::{Contact, Event, EventTime, Kind, Store};
use rusqlite::Connection;

fn event(uid: &str) -> Event {
    Event {
        uid: uid.into(),
        summary: "Meeting".into(),
        description: "Notes".into(),
        location: "Office".into(),
        start: EventTime::Utc(100),
        end: EventTime::Utc(200),
        organizer: Some("Owner@Example.com".into()),
        attendees: vec!["Alice@Example.com".into()],
        recurring: false,
    }
}
fn contact(uid: &str) -> Contact {
    Contact {
        uid: uid.into(),
        name: "Alice".into(),
        emails: vec!["alice@example.com".into()],
        phones: vec!["+15551234".into()],
        organization: "Example".into(),
        note: "hello".into(),
    }
}
#[test]
fn shared_connection_persistence_edit_identity_and_tombstones() -> anyhow::Result<()> {
    let file = tempfile::NamedTempFile::new()?;
    let conn = Connection::open(file.path())?;
    conn.execute_batch("CREATE TABLE application_data(secret TEXT); INSERT INTO application_data VALUES('preserved');")?;
    let store = Store::new(&conn)?;
    let calendar = store.create_collection("Local", Kind::Calendar, None)?;
    let book = store.create_collection("People", Kind::AddressBook, None)?;
    let e = store.save_event(&calendar.id, None, None, &event("e"))?;
    let c = store.save_contact(&book.id, None, None, &contact("c"))?;
    assert_eq!(store.events_in_range(&calendar.id, 150, 250)?.len(), 1);
    assert!(store.events_in_range(&calendar.id, 200, 250)?.is_empty());
    assert!(store.events_in_range(&calendar.id, 0, 100)?.is_empty());
    let p = store.person(" MAILTO:ALICE@example.COM ")?;
    assert_eq!((p.contacts.len(), p.events.len()), (1, 1));
    let mut edited = e.event()?;
    edited.summary = "Updated".into();
    let edited = store.save_event(&calendar.id, Some(&e.id), Some(e.revision), &edited)?;
    assert!(store
        .save_event(&calendar.id, Some(&e.id), Some(e.revision), &event("e"))
        .is_err());
    assert!(store.delete(&e.id, e.revision).is_err());
    store.delete(&c.id, c.revision)?;
    assert!(store.contacts(&book.id)?.is_empty());
    assert!(store.person("alice@example.com")?.contacts.is_empty());
    drop(conn);
    let conn = Connection::open(file.path())?;
    let store = Store::new(&conn)?;
    assert_eq!(store.get(&edited.id)?.event()?.summary, "Updated");
    assert!(store.get(&c.id)?.deleted);
    assert_eq!(
        conn.query_row("SELECT secret FROM application_data", [], |r| r
            .get::<_, String>(0))?,
        "preserved"
    );
    Ok(())
}
#[test]
fn import_roundtrip_preserves_extensions_recurrence_and_types() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let cal = store.create_collection("Cal", Kind::Calendar, None)?;
    let ics="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:series\r\nDTSTART;TZID=America/Chicago:20261103T090000\r\nDTEND;TZID=America/Chicago:20261103T100000\r\nSUMMARY:Hello\\, world\\nSecond line\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:Alice@example.com\r\nX-MUNKI:keep\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series\r\nRECURRENCE-ID;TZID=America/Chicago:20261110T090000\r\nDTSTART;TZID=America/Chicago:20261110T110000\r\nDTEND;TZID=America/Chicago:20261110T120000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let r = store.import_ics(&cal.id, ics)?.remove(0);
    let mut e = r.event()?;
    assert!(e.recurring);
    assert_eq!(e.summary, "Hello, world\nSecond line");
    assert_eq!(
        e.start,
        EventTime::Utc(chrono::DateTime::parse_from_rfc3339("2026-11-03T15:00:00Z")?.timestamp())
    );
    e.summary = "é".repeat(90);
    let r = store.save_event(&cal.id, Some(&r.id), Some(r.revision), &e)?;
    assert!(r.raw.contains("RRULE:FREQ=WEEKLY;COUNT=3"));
    assert!(r
        .raw
        .contains("DTSTART;TZID=America/Chicago:20261103T090000"));
    assert!(r.raw.contains("RECURRENCE-ID"));
    assert!(r.raw.contains("BEGIN:VALARM"));
    assert!(r.raw.contains("PARTSTAT=ACCEPTED"));
    assert!(r.raw.contains("X-MUNKI:keep"));
    assert!(r.raw.split("\r\n").all(|line| line.len() <= 75));
    assert_eq!(r.event()?.summary, e.summary);
    let mut moved = e.clone();
    moved.start = EventTime::Utc(e.start.timestamp()? + 60);
    assert!(store
        .save_event(&cal.id, Some(&r.id), Some(r.revision), &moved)
        .is_err());
    let other = store.create_collection("Copy", Kind::Calendar, None)?;
    assert_eq!(
        store
            .import_ics(&other.id, &store.export_collection(&cal.id)?)?
            .len(),
        1
    );
    let book = store.create_collection("Book", Kind::AddressBook, None)?;
    let vcf="BEGIN:VCARD\r\nVERSION:3.0\r\nUID:alice\r\nFN:Alice\\, Example\r\nN:Example;Alice;;;\r\nitem1.EMAIL;TYPE=WORK:Alice@Example.com\r\nTEL;TYPE=CELL:+1234\r\nORG:Example;Team\r\nNOTE:Long note\r\n continued\\nnew line\r\nX-PRIVATE:keep\r\nEND:VCARD\r\n";
    let c = store.import_vcf(&book.id, vcf)?.remove(0);
    let mut model = c.contact()?;
    assert_eq!(model.note, "Long notecontinued\nnew line");
    model.name = "Changed".into();
    let c = store.save_contact(&book.id, Some(&c.id), Some(c.revision), &model)?;
    for property in [
        "item1.EMAIL;TYPE=WORK:Alice@Example.com",
        "TEL;TYPE=CELL:+1234",
        "N:Example;Alice;;;",
        "ORG:Example;Team",
        "X-PRIVATE:keep",
    ] {
        assert!(c.raw.contains(property), "{}", c.raw);
    }
    Ok(())
}
#[test]
fn all_day_points_atomic_import_and_validation() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let cal = store.create_collection("Cal", Kind::Calendar, None)?;
    let raw="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:day\r\nDTSTART;VALUE=DATE:20261103\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let r = store.import_ics(&cal.id, raw)?.remove(0);
    assert_eq!(r.event()?.end, EventTime::Date("2026-11-04".into()));
    let start = r.event()?.start.timestamp()?;
    assert_eq!(
        store.events_in_range(&cal.id, start, start + 86400)?.len(),
        1
    );
    assert!(store
        .events_in_range(&cal.id, start + 86400, start + 86401)?
        .is_empty());
    let mixed = raw.replace("UID:day", "UID:new") + raw;
    assert!(store.import_ics(&cal.id, &mixed).is_err());
    assert_eq!(store.records(&cal.id)?.len(), 1);
    assert!(store.import_ics(&cal.id, "garbage").is_err());
    assert!(store
        .save_raw(
            &cal.id,
            None,
            None,
            &raw.replace(
                "VALUE=DATE:20261103",
                "TZID=America/Chicago:20261101T013000"
            )
        )
        .is_err());
    assert!(store
        .save_raw(
            &cal.id,
            None,
            None,
            &raw.replace(";VALUE=DATE:20261103", ":20261103T090000")
        )
        .is_err());
    let mut e = event("point");
    e.end = e.start.clone();
    store.save_event(&cal.id, None, None, &e)?;
    assert_eq!(store.events_in_range(&cal.id, 100, 101)?.len(), 1);
    assert!(store.events_in_range(&cal.id, 99, 100)?.is_empty());
    let book = store.create_collection("Book", Kind::AddressBook, None)?;
    let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No UID\r\nEMAIL:a@example.com\r\nEND:VCARD\r\n";
    assert!(!store.import_vcf(&book.id, vcf)?[0].uid.is_empty());
    let mut c = contact("bad");
    c.emails = vec!["broken".into()];
    assert!(store.save_contact(&book.id, None, None, &c).is_err());
    assert!(munki_pim::normalize_email("a@example.com\r\nINJECT").is_err());
    Ok(())
}

#[test]
fn rsvp_is_a_reply_for_only_the_selected_attendee() -> anyhow::Result<()> {
    let raw="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:invite\r\nSEQUENCE:4\r\nDTSTART:20261103T150000Z\r\nDTEND:20261103T160000Z\r\nORGANIZER:mailto:owner@example.com\r\nATTENDEE;CN=Alice;RSVP=TRUE;PARTSTAT=NEEDS-ACTION:mailto:Alice@example.com\r\nATTENDEE:mailto:bob@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    for status in ["accepted", "declined", "tentative"] {
        let reply = munki_pim::rsvp(raw, "ALICE@example.com", status)?;
        assert!(reply.contains("METHOD:REPLY"));
        assert!(!reply.contains("METHOD:REQUEST"));
        assert!(reply.contains(&format!("PARTSTAT={}", status.to_uppercase())));
        assert!(reply.contains("RSVP=FALSE"));
        assert!(reply.contains("SEQUENCE:4"));
        assert!(reply.contains("ORGANIZER:mailto:owner@example.com"));
        let e = munki_pim::parse_event(&reply)?;
        assert_eq!(e.uid, "invite");
        assert_eq!(e.attendees.len(), 1);
    }
    assert!(munki_pim::rsvp(raw, "stranger@example.com", "accepted").is_err());
    assert!(munki_pim::rsvp(raw, "alice@example.com", "whatever").is_err());
    assert!(raw.contains("RSVP=TRUE"));
    Ok(())
}

#[test]
fn caller_transaction_atomically_saves_event_identity_and_outbox() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    let store = Store::new(&conn)?;
    let calendar = store.create_collection("Invites", Kind::Calendar, None)?;
    conn.execute_batch("CREATE TABLE test_outbox(id TEXT PRIMARY KEY,body TEXT NOT NULL)")?;
    let raw="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:atomic\r\nDTSTART:20261103T150000Z\r\nDTEND:20261103T160000Z\r\nATTENDEE:mailto:alice@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert!(store
        .save_raw_in_transaction(&calendar.id, None, None, raw)
        .is_err());
    {
        let tx = conn.unchecked_transaction()?;
        store.save_raw_in_transaction(&calendar.id, None, None, raw)?;
        tx.execute("INSERT INTO test_outbox VALUES('draft','reply')", [])?;
        assert_eq!(store.person("alice@example.com")?.events.len(), 1);
        tx.rollback()?;
    }
    assert!(store.records(&calendar.id)?.is_empty());
    assert!(store.person("alice@example.com")?.events.is_empty());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM test_outbox", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    {
        let tx = conn.unchecked_transaction()?;
        store.save_raw_in_transaction(&calendar.id, None, None, raw)?;
        tx.execute("INSERT INTO test_outbox VALUES('draft','reply')", [])?;
        tx.commit()?;
    }
    assert_eq!(store.records(&calendar.id)?.len(), 1);
    assert_eq!(store.person("alice@example.com")?.events.len(), 1);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM test_outbox", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn local_rsvp_retains_other_attendees_and_alarms() -> anyhow::Result<()> {
    let raw="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:local-rsvp\r\nSEQUENCE:2\r\nDTSTART:20261103T150000Z\r\nDTEND:20261103T160000Z\r\nORGANIZER:mailto:owner@example.com\r\nATTENDEE;CN=Alice;PARTSTAT=NEEDS-ACTION:mailto:alice@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let local = munki_pim::rsvp_local(raw, "alice@example.com", "tentative")?;
    assert!(!local.contains("METHOD:"));
    assert!(local.contains("BEGIN:VALARM"));
    assert!(local.contains("PARTSTAT=TENTATIVE"));
    assert!(local.contains("PARTSTAT=ACCEPTED:mailto:bob@example.com"));
    assert_eq!(munki_pim::parse_event(&local)?.attendees.len(), 2);
    Ok(())
}
