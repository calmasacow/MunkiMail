//! Milestone 1 acceptance: insert -> index -> search -> outbox replay.
//!
//! These run against a real on-disk SQLite file, not in-memory, so the
//! WAL and transaction behaviour under test is the behaviour that ships.

use munki_core::outbox::OpKind;
use munki_core::{query, triage, Address, Destination, SenderVerdict};
use munki_store::{NewMessage, Store};

/// 2024-03-15T12:00:00Z, so date filters in tests read legibly.
const T: i64 = 1_710_504_000;

fn store() -> (Store, tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("munki.db"), Some("test-key")).unwrap();
    let acct = s.add_account("me@example.com", "Me", "imap").unwrap();
    (s, dir, acct)
}

fn msg<'a>(
    account: i64,
    uid: u32,
    from: &str,
    subject: &'a str,
    dest: Destination,
) -> NewMessage<'a> {
    NewMessage {
        account,
        folder: "INBOX",
        uid,
        message_id: subject, // unique enough for a test
        thread_id: subject,
        from: Address::new(from),
        subject,
        date: T + uid as i64,
        seen: false,
        has_attachment: false,
        destination: dest,
    }
}

#[test]
fn insert_index_search_roundtrip() {
    let (mut s, _d, a) = store();

    s.upsert_message(
        &msg(
            a,
            1,
            "jane@example.com",
            "lunch thursday",
            Destination::Imbox,
        ),
        "are you free around noon?",
        "",
    )
    .unwrap();
    s.upsert_message(
        &msg(
            a,
            2,
            "news@list.example",
            "this week in rust",
            Destination::Feed,
        ),
        "async closures stabilized",
        "",
    )
    .unwrap();

    // Body text is searchable even though it is not stored on the message.
    let hits = s.search(a, &query::parse("noon"), 50).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject, "lunch thursday");

    // Subject and sender are indexed too.
    assert_eq!(s.search(a, &query::parse("rust"), 50).unwrap().len(), 1);
    assert_eq!(
        s.search(a, &query::parse("from:jane@example.com"), 50)
            .unwrap()
            .len(),
        1
    );

    // Metadata-only queries skip the FTS join but still filter.
    assert_eq!(s.search(a, &query::parse("in:feed"), 50).unwrap().len(), 1);
    assert_eq!(
        s.search(a, &query::parse("is:unread"), 50).unwrap().len(),
        2
    );
    let after = "after:1970-01-01 before:2099-01-01";
    assert_eq!(s.search(a, &query::parse(after), 50).unwrap().len(), 2);
}

#[test]
fn resync_reinserting_a_message_updates_rather_than_duplicating() {
    let (mut s, _d, a) = store();
    let first = s
        .upsert_message(
            &msg(a, 7, "jane@example.com", "hello", Destination::Imbox),
            "v1",
            "",
        )
        .unwrap();

    // Same (account, folder, uid): a resync re-reporting a message it
    // already sent. Must be a no-op-shaped update, not a duplicate row.
    let mut m = msg(a, 7, "jane@example.com", "hello", Destination::Imbox);
    m.seen = true;
    let second = s.upsert_message(&m, "v2", "").unwrap();
    assert_eq!(first, second);

    let all = s.view(a, Destination::Imbox, 50).unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].seen);

    // The stale body must be gone from the index, not merely shadowed.
    assert!(s.search(a, &query::parse("v1"), 50).unwrap().is_empty());
    assert_eq!(s.search(a, &query::parse("v2"), 50).unwrap().len(), 1);
}

#[test]
fn approving_a_sender_retroactively_moves_their_mail() {
    let (mut s, _d, a) = store();
    for uid in 1..=3 {
        s.upsert_message(
            &msg(
                a,
                uid,
                "shop@store.example",
                "your order shipped",
                Destination::Screener,
            ),
            "on its way",
            "",
        )
        .unwrap();
    }

    let who = Address::new("shop@store.example");
    let moved = s
        .decide_sender(a, &who, SenderVerdict::Allow(Destination::PaperTrail))
        .unwrap();
    assert_eq!(moved, 3, "past mail should follow the decision");

    assert!(s.view(a, Destination::Screener, 50).unwrap().is_empty());
    assert_eq!(s.view(a, Destination::PaperTrail, 50).unwrap().len(), 3);
    assert_eq!(
        s.sender_verdict(a, &who).unwrap(),
        Some(SenderVerdict::Allow(Destination::PaperTrail))
    );
}

#[test]
fn dismissed_senders_leave_views_but_stay_searchable() {
    let (mut s, _d, a) = store();
    s.upsert_message(
        &msg(
            a,
            1,
            "spam@cold.example",
            "quick question",
            Destination::Screener,
        ),
        "buy my saas",
        "",
    )
    .unwrap();

    s.decide_sender(
        a,
        &Address::new("spam@cold.example"),
        SenderVerdict::Dismiss,
    )
    .unwrap();

    // This is the whole promise of a soft gate: hidden, never destroyed.
    assert!(s.view(a, Destination::Screener, 50).unwrap().is_empty());
    assert_eq!(s.search(a, &query::parse("saas"), 50).unwrap().len(), 1);

    // Un-dismissing restores the message to its original view, because
    // dismissal never rewrote the destination column.
    s.decide_sender(
        a,
        &Address::new("spam@cold.example"),
        SenderVerdict::Allow(Destination::Imbox),
    )
    .unwrap();
    assert_eq!(s.view(a, Destination::Imbox, 50).unwrap().len(), 1);
}

#[test]
fn renamed_subject_wins_in_display_but_original_stays_searchable() {
    let (mut s, _d, a) = store();
    let id = s
        .upsert_message(
            &msg(
                a,
                1,
                "billing@vendor.example",
                "INV-99812-A",
                Destination::PaperTrail,
            ),
            "attached",
            "invoice.pdf",
        )
        .unwrap();

    s.rename_subject(id, Some("Hosting invoice, March"))
        .unwrap();
    let m = &s.view(a, Destination::PaperTrail, 50).unwrap()[0];
    assert_eq!(m.display_subject(), "Hosting invoice, March");
    assert_eq!(m.subject, "INV-99812-A");

    // The sender's opaque original is still how you find it later.
    assert_eq!(
        s.search(a, &query::parse("INV-99812-A"), 50).unwrap().len(),
        1
    );
    // Attachment filenames are indexed.
    assert_eq!(
        s.search(a, &query::parse("invoice.pdf"), 50).unwrap().len(),
        1
    );
}

#[test]
fn search_survives_fts5_operators_in_user_input() {
    let (mut s, _d, a) = store();
    s.upsert_message(
        &msg(a, 1, "jane@example.com", "re: terms", Destination::Imbox),
        "the NEAR clause and the (parenthetical) one",
        "",
    )
    .unwrap();

    // Each of these is an FTS5 syntax error if passed through raw.
    for q in [
        "NEAR",
        "(parenthetical)",
        "AND",
        "terms*",
        "\"NEAR clause\"",
    ] {
        let r = s.search(a, &query::parse(q), 50);
        assert!(r.is_ok(), "query {q:?} errored: {:?}", r.err());
    }
    assert_eq!(
        s.search(a, &query::parse("\"NEAR clause\""), 50)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn outbox_replays_each_op_exactly_once() {
    let (mut s, _d, a) = store();
    let ops = [
        OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
        OpKind::SetDestination {
            folder: "INBOX".into(),
            uid: 1,
            destination: Destination::Feed,
        },
        OpKind::Move {
            from_folder: "INBOX".into(),
            uid: 2,
            to_folder: "Archive".into(),
        },
    ];
    for op in &ops {
        s.enqueue(a, op).unwrap();
    }
    assert_eq!(s.pending_count(a).unwrap(), 3);

    // Drain in FIFO order, settling each one.
    let mut drained = Vec::new();
    while let Some(op) = s.lease_next(a).unwrap() {
        drained.push(op.kind.clone());
        s.settle_ok(op.id).unwrap();
    }
    assert_eq!(drained, ops);
    assert_eq!(s.pending_count(a).unwrap(), 0);
}

#[test]
fn a_leased_op_is_not_handed_out_twice() {
    let (mut s, _d, a) = store();
    s.enqueue(
        a,
        &OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
    )
    .unwrap();

    let first = s.lease_next(a).unwrap().expect("one ready op");
    // Concurrent drainer, or the same one re-entering: must see nothing.
    assert!(s.lease_next(a).unwrap().is_none());

    s.settle_ok(first.id).unwrap();
}

#[test]
fn a_failed_op_backs_off_then_returns() {
    let (mut s, _d, a) = store();
    s.enqueue(
        a,
        &OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
    )
    .unwrap();

    let op = s.lease_next(a).unwrap().unwrap();
    s.settle_retry(op.id, "connection reset").unwrap();

    // Backoff after one failure is 2s, so it is not due yet.
    assert!(s.lease_next(a).unwrap().is_none());

    // Pull it forward rather than sleeping: we are testing the schedule,
    // not the clock.
    s.conn()
        .execute("UPDATE outbox SET not_before = 0", [])
        .unwrap();
    let again = s.lease_next(a).unwrap().expect("op should come back");
    assert_eq!(again.attempts, 1);
    assert_eq!(again.last_error.as_deref(), Some("connection reset"));
}

#[test]
fn a_crash_requeues_flag_ops_but_parks_an_interrupted_send() {
    let (mut s, _d, a) = store();
    s.enqueue(
        a,
        &OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
    )
    .unwrap();
    s.enqueue(
        a,
        &OpKind::Send {
            message_id: "<x@example.com>".into(),
            staged_path: "/tmp/x.eml".into(),
        },
    )
    .unwrap();

    // Simulate a crash: both ops leased, process dies, leases go stale.
    let flag = s.lease_next(a).unwrap().unwrap();
    let send = s.lease_next(a).unwrap().unwrap();
    s.conn()
        .execute("UPDATE outbox SET leased_at = unixepoch() - 86400", [])
        .unwrap();

    let (requeued, parked) = s.recover_leases(a).unwrap();
    assert_eq!((requeued, parked), (1, 1));

    // The idempotent op comes back on its own.
    let back = s.lease_next(a).unwrap().expect("flag op requeued");
    assert_eq!(back.id, flag.id);

    // The send does not: we cannot tell whether it left, so a human decides.
    let review = s.needs_review(a).unwrap();
    assert_eq!(review.len(), 1);
    assert_eq!(review[0].id, send.id);
    assert!(review[0].last_error.as_deref().unwrap().contains("confirm"));
}

#[test]
fn parked_ops_are_never_leased() {
    let (mut s, _d, a) = store();
    s.enqueue(
        a,
        &OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
    )
    .unwrap();
    let op = s.lease_next(a).unwrap().unwrap();
    s.settle_needs_review(op.id, "mailbox is read-only")
        .unwrap();

    assert!(s.lease_next(a).unwrap().is_none());
    assert_eq!(s.needs_review(a).unwrap().len(), 1);
    // Still owed, so the UI keeps showing it as unfinished work.
    assert_eq!(s.pending_count(a).unwrap(), 1);

    // And a later recovery pass must not resurrect it.
    s.conn()
        .execute("UPDATE outbox SET leased_at = unixepoch() - 86400", [])
        .unwrap();
    assert_eq!(s.recover_leases(a).unwrap(), (0, 0));
    assert!(s.lease_next(a).unwrap().is_none());
}

#[test]
fn heuristics_and_store_agree_on_where_mail_lands() {
    // The classifier decides, the store records it. This is the seam
    // milestone 4 builds on, so it is worth pinning now.
    let (mut s, _d, a) = store();

    let cases: &[(&str, &str, bool)] = &[
        ("jane@example.com", "lunch thursday?", false),
        ("ship@store.example", "Your order has shipped", true),
        ("news@list.example", "weekly digest", true),
        ("no-reply@bank.example", "statement", false),
    ];

    for (i, (from, subject, list)) in cases.iter().enumerate() {
        let sug = triage::suggest(triage::Headers {
            from,
            subject,
            has_list_header: *list,
            ..Default::default()
        });
        let m = msg(a, i as u32 + 1, from, subject, sug.destination);
        s.upsert_message(&m, "", "").unwrap();
    }

    assert_eq!(s.view(a, Destination::Imbox, 50).unwrap().len(), 1);
    assert_eq!(s.view(a, Destination::PaperTrail, 50).unwrap().len(), 1);
    assert_eq!(s.view(a, Destination::Feed, 50).unwrap().len(), 2);
}
