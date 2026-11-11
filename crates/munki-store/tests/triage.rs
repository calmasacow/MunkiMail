use munki_core::{
    state::{StateEntry, StateSnapshot, StateValue},
    triage::Stack,
    Address, Destination, SenderVerdict,
};
use munki_store::{NewMessage, Store};

fn setup() -> (Store, i64) {
    let s = Store::open_in_memory().unwrap();
    let a = s.add_account("me@example.com", "", "imap").unwrap();
    (s, a)
}
fn insert(s: &mut Store, a: i64, uid: u32, from: &str) -> i64 {
    s.upsert_message(
        &NewMessage {
            account: a,
            folder: "INBOX",
            uid,
            message_id: &format!("<{uid}@x>"),
            thread_id: &format!("<{uid}@x>"),
            from: Address::new(from),
            subject: "opaque subject",
            date: uid as i64,
            seen: false,
            has_attachment: false,
            destination: Destination::Screener,
        },
        "searchable body",
        "",
    )
    .unwrap()
}

#[test]
fn actions_are_atomic_and_metadata_survives_reingestion() {
    let (mut s, a) = setup();
    let first = insert(&mut s, a, 1, "jane@x");
    insert(&mut s, a, 2, "friend@x");
    s.conn().execute_batch("CREATE TRIGGER fail_queue BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT,'simulated disk failure'); END;").unwrap();
    assert!(s
        .decide_senders(
            a,
            &[(
                Address::new("jane@x"),
                SenderVerdict::Allow(Destination::Imbox)
            )]
        )
        .is_err());
    assert!(s
        .sender_verdict(a, &Address::new("jane@x"))
        .unwrap()
        .is_none());
    assert!(s.triage_snapshot(a).unwrap().entries.is_empty());
    assert_eq!(s.view(a, Destination::Screener, 10).unwrap().len(), 2);
    s.conn().execute_batch("DROP TRIGGER fail_queue").unwrap();
    assert!(s
        .decide_senders(
            a,
            &[
                (
                    Address::new("jane@x"),
                    SenderVerdict::Allow(Destination::Imbox)
                ),
                (Address::new("invalid"), SenderVerdict::Dismiss)
            ]
        )
        .is_err());
    assert!(
        s.triage_snapshot(a).unwrap().entries.is_empty(),
        "bulk validation rollback"
    );
    s.decide_senders(
        a,
        &[
            (
                Address::new("jane@x"),
                SenderVerdict::Allow(Destination::Imbox),
            ),
            (
                Address::new("friend@x"),
                SenderVerdict::Allow(Destination::Feed),
            ),
        ],
    )
    .unwrap();
    s.rename_subject(first, Some("Travel details")).unwrap();
    s.set_stack(a, first, Stack::ReplyLater, true).unwrap();
    s.set_stack(a, first, Stack::SetAside, true).unwrap();
    assert_eq!(s.merge_threads(a, "<1@x>", "<2@x>").unwrap(), 1);
    // Incoming message bytes must not overwrite restored presentation settings.
    insert(&mut s, a, 1, "jane@x");
    let hit = s.stack(a, Stack::ReplyLater, 10).unwrap().remove(0);
    assert_eq!(hit.display_subject(), "Travel details");
    assert_eq!(hit.thread_id, "<2@x>");
    assert_eq!(s.stack(a, Stack::SetAside, 10).unwrap().len(), 1);
    assert_eq!(
        s.search(a, &munki_core::query::parse("Travel"), 10)
            .unwrap()
            .len(),
        1
    );
    s.rename_subject(first, None).unwrap();
    s.set_stack(a, first, Stack::ReplyLater, false).unwrap();
    assert!(s.stack(a, Stack::ReplyLater, 10).unwrap().is_empty());
    assert!(s
        .search(a, &munki_core::query::parse("Travel"), 10)
        .unwrap()
        .is_empty());
}

#[test]
fn merge_is_order_independent_and_invalid_snapshots_change_nothing() {
    let (mut a, account) = setup();
    let (mut b, other) = setup();
    let entry = |key: &str, device: &str, value| StateEntry {
        key: key.into(),
        revision: 42,
        device: device.repeat(32),
        value: StateValue::Sender(value),
    };
    let one = StateSnapshot {
        format: 1,
        entries: vec![
            entry("shared@x", "a", SenderVerdict::Dismiss),
            entry("one@x", "a", SenderVerdict::Allow(Destination::Feed)),
        ],
    };
    let two = StateSnapshot {
        format: 1,
        entries: vec![
            entry("shared@x", "b", SenderVerdict::Allow(Destination::Imbox)),
            entry("two@x", "b", SenderVerdict::Dismiss),
        ],
    };
    a.merge_triage(account, &one).unwrap();
    a.merge_triage(account, &two).unwrap();
    b.merge_triage(other, &two).unwrap();
    b.merge_triage(other, &one).unwrap();
    assert_eq!(
        a.triage_snapshot(account).unwrap(),
        b.triage_snapshot(other).unwrap()
    );
    assert_eq!(
        a.sender_verdict(account, &Address::new("shared@x"))
            .unwrap(),
        Some(SenderVerdict::Allow(Destination::Imbox))
    );
    let before = a.triage_snapshot(account).unwrap();
    let mut malformed = one.clone();
    malformed.entries[1].key = "bad\nkey".into();
    assert!(a.merge_triage(account, &malformed).is_err());
    malformed = one;
    malformed.format = 2;
    assert!(a.merge_triage(account, &malformed).is_err());
    assert_eq!(a.triage_snapshot(account).unwrap(), before);
    // Explicitly clearing a value is a newer record, not removal of history.
    a.decide_sender(account, &Address::new("shared@x"), SenderVerdict::Dismiss)
        .unwrap();
    a.merge_triage(account, &before).unwrap();
    assert_eq!(
        a.sender_verdict(account, &Address::new("shared@x"))
            .unwrap(),
        Some(SenderVerdict::Dismiss)
    );
}

#[test]
fn migration_preserves_preexisting_sender_and_subject_decisions() {
    let dir = std::env::temp_dir().join(format!("munki-state-migration-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let conn = rusqlite::Connection::open(&dir).unwrap();
    conn.execute_batch(include_str!("../src/schema.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/schema_v2.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/schema_v3.sql"))
        .unwrap();
    conn.execute_batch("PRAGMA user_version=3; INSERT INTO accounts(id,email,kind,created_at) VALUES(1,'me@x','imap',1); INSERT INTO senders VALUES(1,'old@x','allow','feed',1); INSERT INTO senders VALUES(1,'blocked@x','dismiss',NULL,1); INSERT INTO messages(account,folder,uid,message_id,thread_id,from_addr,subject,renamed_subject,date,destination) VALUES(1,'INBOX',1,'<1@x>','<1@x>','old@x','original','Readable',1,'feed');").unwrap();
    drop(conn);
    let s = Store::open(&dir, None).unwrap();
    let snapshot = s.triage_snapshot(1).unwrap();
    snapshot.validate().unwrap();
    assert_eq!(snapshot.entries.len(), 3);
    assert!(snapshot.entries.iter().any(
        |e| matches!(&e.value,StateValue::Message(m) if m.subject.as_deref()==Some("Readable"))
    ));
    drop(s);
    std::fs::remove_file(dir).unwrap();
}
