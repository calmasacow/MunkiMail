//! Milestone 2 acceptance, against a disposable local IMAP server.
//!
//! Never a real mailbox. Start the server first:
//!
//!   docker run -d --name munki-greenmail -p 127.0.0.1:3143:3143 \
//!     -e GREENMAIL_OPTS='-Dgreenmail.setup.test.imap -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled' \
//!     greenmail/standalone:2.1.3
//!
//! then `cargo test -p munki-net --test greenmail -- --ignored`.
//! Each test uses its own address, so they run in parallel without
//! seeing each other's mail.

use async_imap::Session;
use futures::TryStreamExt;
use munki_core::{query, Address, Destination, SenderVerdict};
use munki_net::imap::{self, Auth, Stream};
use munki_store::Store;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 3143;

/// Unique mailbox per test run, so reruns never see stale mail.
fn user(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{n}@munki.test")
}

async fn session(user: &str) -> Session<impl Stream> {
    let c = imap::connect_insecure(HOST, PORT)
        .await
        .expect("GreenMail not running? see file header");
    imap::login(c, Auth::Password { user, pass: user })
        .await
        .unwrap()
}

/// Simulate delivery. APPEND rather than SMTP so the test controls the
/// exact bytes and does not depend on GreenMail's SMTP routing.
async fn deliver<S: Stream>(s: &mut Session<S>, raw: &str) {
    s.append("INBOX", None, None, raw.replace('\n', "\r\n"))
        .await
        .unwrap();
}

fn mail(from: &str, subject: &str, extra: &str, body: &str) -> String {
    format!(
        "From: {from}\nTo: me@munki.test\nSubject: {subject}\n\
         Message-ID: <{}@munki.test>\nDate: Fri, 15 Mar 2024 10:00:00 +0000\n{extra}\n{body}\n",
        subject.replace(' ', "-")
    )
}

fn store(u: &str) -> (Store, i64) {
    let s = Store::open_in_memory().unwrap();
    let a = s.add_account(u, "", "imap").unwrap();
    (s, a)
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn first_sync_ingests_and_presorts_the_screener() {
    let u = user("first");
    let mut s = session(&u).await;
    deliver(
        &mut s,
        &mail("jane@example.com", "lunch thursday", "", "free at noon?"),
    )
    .await;
    deliver(
        &mut s,
        &mail(
            "ship@store.example",
            "Your order has shipped",
            "List-Unsubscribe: <mailto:u@x>\n",
            "tracking 1Z999",
        ),
    )
    .await;
    deliver(
        &mut s,
        &mail(
            "news@list.example",
            "weekly digest",
            "List-Id: <weekly.list.example>\n",
            "top stories",
        ),
    )
    .await;

    let (mut st, a) = store(&u);
    let r = imap::sync_folder(&mut s, &mut st, a, "INBOX")
        .await
        .unwrap();
    assert_eq!(r.fetched, 3);

    // Unknown senders all wait in the Screener -- the soft gate.
    let q = st.view(a, Destination::Screener, 50).unwrap();
    assert_eq!(q.len(), 3);

    // ...each pre-sorted with a reason, so approving is one keypress.
    let sug = |subj: &str| {
        let m = q.iter().find(|m| m.subject == subj).unwrap();
        st.suggestion(m.id).unwrap().unwrap()
    };
    assert_eq!(sug("lunch thursday").destination, Destination::Imbox);
    assert_eq!(
        sug("Your order has shipped").destination,
        Destination::PaperTrail
    );
    assert_eq!(sug("weekly digest").destination, Destination::Feed);
    assert!(sug("weekly digest").reason.contains("mailing-list"));

    // Body text came over the wire and into the search index.
    assert_eq!(st.search(a, &query::parse("1Z999"), 10).unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn second_sync_fetches_only_new_mail() {
    let u = user("incr");
    let mut s = session(&u).await;
    deliver(&mut s, &mail("a@example.com", "one", "", "x")).await;

    let (mut st, a) = store(&u);
    assert_eq!(
        imap::sync_folder(&mut s, &mut st, a, "INBOX")
            .await
            .unwrap()
            .fetched,
        1
    );

    // Unchanged folder: zero, not the `UID n+1:*` off-by-one that
    // returns the last message again.
    assert_eq!(
        imap::sync_folder(&mut s, &mut st, a, "INBOX")
            .await
            .unwrap()
            .fetched,
        0
    );

    deliver(&mut s, &mail("a@example.com", "two", "", "y")).await;
    assert_eq!(
        imap::sync_folder(&mut s, &mut st, a, "INBOX")
            .await
            .unwrap()
            .fetched,
        1
    );
    assert_eq!(
        st.search(a, &query::parse("from:a@example.com"), 10)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn offline_actions_replay_to_the_server() {
    let u = user("outbox");
    let mut s = session(&u).await;
    deliver(
        &mut s,
        &mail("shop@store.example", "receipt 42", "", "thanks"),
    )
    .await;

    let (mut st, a) = store(&u);
    imap::sync_folder(&mut s, &mut st, a, "INBOX")
        .await
        .unwrap();
    let id = st.view(a, Destination::Screener, 10).unwrap()[0].id;

    // User acts while "offline": local state changes now, server later.
    st.mark_seen(id, true).unwrap();
    st.decide_sender_synced(
        a,
        &Address::new("shop@store.example"),
        SenderVerdict::Allow(Destination::PaperTrail),
    )
    .unwrap();
    assert_eq!(st.pending_count(a).unwrap(), 3);
    assert_eq!(st.view(a, Destination::PaperTrail, 10).unwrap().len(), 1);

    // Back online.
    let r = imap::drain_outbox(&mut s, &mut st, a).await.unwrap();
    assert_eq!((r.applied, r.retried, r.parked), (3, 0, 0));
    assert_eq!(st.pending_count(a).unwrap(), 0);

    // The server now carries both the read flag and the verdict keyword.
    s.select("INBOX").await.unwrap();
    let f: Vec<_> = s
        .uid_fetch("1:*", "FLAGS")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let flags: Vec<String> = f[0].flags().map(|f| format!("{f:?}")).collect();
    assert!(flags.iter().any(|f| f == "Seen"), "{flags:?}");
    assert!(flags.iter().any(|f| f.contains("$MunkiPaper")), "{flags:?}");
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn a_fresh_install_recovers_verdicts_from_server_keywords() {
    // Device A decides; device B (empty store) must agree without any
    // server of ours -- the keyword *is* the sync channel.
    let u = user("restore");
    let mut s = session(&u).await;
    deliver(
        &mut s,
        &mail("billing@vendor.example", "invoice march", "", "due"),
    )
    .await;

    let (mut dev_a, a) = store(&u);
    imap::sync_folder(&mut s, &mut dev_a, a, "INBOX")
        .await
        .unwrap();
    dev_a
        .decide_sender_synced(
            a,
            &Address::new("billing@vendor.example"),
            SenderVerdict::Allow(Destination::PaperTrail),
        )
        .unwrap();
    imap::drain_outbox(&mut s, &mut dev_a, a).await.unwrap();

    // Exercise keyword fallback even when the settings folder is unavailable on reinstall.
    s.delete(munki_net::state::STATE_FOLDER).await.unwrap();
    let (mut dev_b, b) = store(&u);
    imap::sync_folder(&mut s, &mut dev_b, b, "INBOX")
        .await
        .unwrap();
    assert!(dev_b.view(b, Destination::Screener, 10).unwrap().is_empty());
    assert_eq!(dev_b.view(b, Destination::PaperTrail, 10).unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn server_refusal_parks_the_op_instead_of_retrying_forever() {
    let u = user("park");
    let mut s = session(&u).await;
    let (mut st, a) = store(&u);

    // Folder does not exist: server says NO. Retrying cannot fix that.
    st.enqueue(
        a,
        &munki_core::OpKind::SetSeen {
            folder: "Nope".into(),
            uid: 1,
            seen: true,
        },
    )
    .unwrap();
    let r = imap::drain_outbox(&mut s, &mut st, a).await.unwrap();
    assert_eq!((r.applied, r.retried, r.parked), (0, 0, 1));
    assert_eq!(st.needs_review(a).unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn state_restores_sender_rules_metadata_and_future_routing() {
    use munki_core::triage::Stack;
    let u = user("state");
    let mut session = session(&u).await;
    deliver(&mut session, &mail("jane@x", "trip", "", "tickets")).await;
    deliver(&mut session, &mail("friend@x", "followup", "", "hotel")).await;
    deliver(&mut session, &mail("cold@x", "offer", "", "hiddenneedle")).await;
    let (mut a, account) = store(&u);
    imap::sync_folder(&mut session, &mut a, account, "INBOX")
        .await
        .unwrap();
    let rows = a.view(account, Destination::Screener, 10).unwrap();
    let trip = rows.iter().find(|m| m.subject == "trip").unwrap();
    let followup = rows.iter().find(|m| m.subject == "followup").unwrap();
    a.decide_senders(
        account,
        &[
            (
                Address::new("jane@x"),
                SenderVerdict::Allow(Destination::Imbox),
            ),
            (
                Address::new("friend@x"),
                SenderVerdict::Allow(Destination::Imbox),
            ),
            (Address::new("cold@x"), SenderVerdict::Dismiss),
        ],
    )
    .unwrap();
    a.rename_subject(trip.id, Some("Travel details")).unwrap();
    a.set_stack(account, trip.id, Stack::ReplyLater, true)
        .unwrap();
    a.set_stack(account, followup.id, Stack::SetAside, true)
        .unwrap();
    a.merge_threads(account, &followup.thread_id, &trip.thread_id)
        .unwrap();
    let report = imap::drain_outbox(&mut session, &mut a, account)
        .await
        .unwrap();
    assert_eq!((report.retried, report.parked), (0, 0));
    assert_eq!(a.pending_count(account).unwrap(), 0);

    let (mut b, other) = store(&u);
    imap::sync_folder(&mut session, &mut b, other, "INBOX")
        .await
        .unwrap();
    assert_eq!(
        a.triage_snapshot(account).unwrap(),
        b.triage_snapshot(other).unwrap()
    );
    assert!(b.view(other, Destination::Screener, 10).unwrap().is_empty());
    assert_eq!(b.view(other, Destination::Imbox, 10).unwrap().len(), 2);
    assert_eq!(
        b.stack(other, Stack::ReplyLater, 10).unwrap()[0].display_subject(),
        "Travel details"
    );
    assert_eq!(
        b.stack(other, Stack::SetAside, 10).unwrap()[0].thread_id,
        trip.thread_id
    );
    assert_eq!(
        b.search(other, &query::parse("hiddenneedle"), 10)
            .unwrap()
            .len(),
        1
    );
    deliver(
        &mut session,
        &mail("jane@x", "next trip", "", "new tickets"),
    )
    .await;
    deliver(
        &mut session,
        &mail("cold@x", "new offer", "", "anotherhiddenneedle"),
    )
    .await;
    imap::sync_folder(&mut session, &mut b, other, "INBOX")
        .await
        .unwrap();
    assert_eq!(b.view(other, Destination::Imbox, 10).unwrap().len(), 3);
    assert!(b.view(other, Destination::Screener, 10).unwrap().is_empty());
    assert!(
        !munki_net::state::sync(&mut session, &mut b, other)
            .await
            .unwrap(),
        "unchanged sync must not append snapshots"
    );
    let restored = b.stack(other, Stack::ReplyLater, 10).unwrap().remove(0);
    b.rename_subject(restored.id, None).unwrap();
    b.set_stack(other, restored.id, Stack::ReplyLater, false)
        .unwrap();
    imap::drain_outbox(&mut session, &mut b, other)
        .await
        .unwrap();
    munki_net::state::sync(&mut session, &mut a, account)
        .await
        .unwrap();
    assert!(a.stack(account, Stack::ReplyLater, 10).unwrap().is_empty());
    assert_eq!(
        a.search(account, &query::parse("from:jane@x"), 10).unwrap()[0].display_subject(),
        "trip"
    );
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn concurrent_snapshots_converge_and_future_formats_cannot_overwrite_local_state() {
    use mail_builder::{headers::raw::Raw, MessageBuilder};
    let u = user("concurrent");
    let mut session = session(&u).await;
    let (mut a, account) = store(&u);
    let (mut b, other) = store(&u);
    a.decide_sender(
        account,
        &Address::new("one@x"),
        SenderVerdict::Allow(Destination::Feed),
    )
    .unwrap();
    b.decide_sender(other, &Address::new("two@x"), SenderVerdict::Dismiss)
        .unwrap();
    a.decide_sender(account, &Address::new("shared@x"), SenderVerdict::Dismiss)
        .unwrap();
    b.decide_sender(
        other,
        &Address::new("shared@x"),
        SenderVerdict::Allow(Destination::Imbox),
    )
    .unwrap();
    session.create("MunkiMail").await.unwrap();
    session
        .create(munki_net::state::STATE_FOLDER)
        .await
        .unwrap();
    // Both devices appended after reading the same old state: neither snapshot includes the other.
    for snapshot in [
        a.triage_snapshot(account).unwrap(),
        b.triage_snapshot(other).unwrap(),
    ] {
        let wire = MessageBuilder::new()
            .header("X-MunkiMail-State", Raw::new("1"))
            .text_body(serde_json::to_string(&snapshot).unwrap())
            .write_to_vec()
            .unwrap();
        session
            .append(munki_net::state::STATE_FOLDER, None, None, wire)
            .await
            .unwrap();
    }
    munki_net::state::sync(&mut session, &mut a, account)
        .await
        .unwrap();
    munki_net::state::sync(&mut session, &mut b, other)
        .await
        .unwrap();
    assert_eq!(
        a.triage_snapshot(account).unwrap(),
        b.triage_snapshot(other).unwrap()
    );
    assert_eq!(a.triage_snapshot(account).unwrap().entries.len(), 3);
    let before = a.triage_snapshot(account).unwrap();
    let count = session
        .select(munki_net::state::STATE_FOLDER)
        .await
        .unwrap()
        .exists;
    let wire = MessageBuilder::new()
        .header("X-MunkiMail-State", Raw::new("1"))
        .text_body("{\"format\":2,\"entries\":[]}")
        .write_to_vec()
        .unwrap();
    session
        .append(munki_net::state::STATE_FOLDER, None, None, wire)
        .await
        .unwrap();
    assert!(munki_net::state::sync(&mut session, &mut a, account)
        .await
        .is_err());
    assert_eq!(a.triage_snapshot(account).unwrap(), before);
    assert_eq!(
        session
            .select(munki_net::state::STATE_FOLDER)
            .await
            .unwrap()
            .exists,
        count + 1,
        "unsupported state must not be overwritten"
    );
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn newer_roster_overrides_an_older_queued_keyword() {
    let u = user("stale-keyword");
    let mut session = session(&u).await;
    deliver(
        &mut session,
        &mail("shared@x", "shared thread", "", "hello"),
    )
    .await;
    let (mut a, account) = store(&u);
    let (mut b, other) = store(&u);
    imap::sync_folder(&mut session, &mut a, account, "INBOX")
        .await
        .unwrap();
    a.decide_sender(
        account,
        &Address::new("shared@x"),
        SenderVerdict::Allow(Destination::Imbox),
    )
    .unwrap();
    // Publish A's roster, leaving its keyword write pending.
    munki_net::state::sync(&mut session, &mut a, account)
        .await
        .unwrap();
    imap::sync_folder(&mut session, &mut b, other, "INBOX")
        .await
        .unwrap();
    b.decide_sender(
        other,
        &Address::new("shared@x"),
        SenderVerdict::Allow(Destination::Feed),
    )
    .unwrap();
    imap::drain_outbox(&mut session, &mut b, other)
        .await
        .unwrap();
    imap::drain_outbox(&mut session, &mut a, account)
        .await
        .unwrap();
    assert_eq!(a.view(account, Destination::Feed, 10).unwrap().len(), 1);
    session.select("INBOX").await.unwrap();
    let messages: Vec<_> = session
        .uid_fetch("1:*", "FLAGS")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let flags = messages[0]
        .flags()
        .map(|f| format!("{f:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(flags.contains("$MunkiFeed"), "{flags}");
    assert!(!flags.contains("$MunkiImbox"), "{flags}");
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn second_client_flags_and_expunge_reconcile_without_new_mail() {
    let u = user("reconcile");
    let mut first = session(&u).await;
    deliver(&mut first, &mail("a@x", "keep", "", "keptneedle")).await;
    deliver(&mut first, &mail("b@x", "remove", "", "removedneedle")).await;
    let (mut st, a) = store(&u);
    imap::sync_folder(&mut first, &mut st, a, "INBOX")
        .await
        .unwrap();
    let rows = st.view(a, Destination::Screener, 10).unwrap();
    let keep = rows.iter().find(|m| m.subject == "keep").unwrap();
    let remove = rows.iter().find(|m| m.subject == "remove").unwrap();
    let mut second = session(&u).await;
    second.select("INBOX").await.unwrap();
    let _: Vec<_> = second
        .uid_store(keep.uid.to_string(), "+FLAGS.SILENT (\\Seen $MunkiFeed)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let _: Vec<_> = second
        .uid_store(remove.uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let _: Vec<_> = second.expunge().await.unwrap().try_collect().await.unwrap();
    let report = imap::sync_folder(&mut first, &mut st, a, "INBOX")
        .await
        .unwrap();
    assert_eq!(report.fetched, 0);
    let row = st.message(a, keep.id).unwrap();
    assert!(row.seen);
    assert_eq!(row.destination, Destination::Feed);
    assert!(st.message(a, remove.id).is_err());
    assert!(st.raw_message(a, remove.id).unwrap().is_none());
    assert!(st
        .search(a, &query::parse("removedneedle"), 10)
        .unwrap()
        .is_empty());
    // A stale server snapshot must not reverse queued UI work.
    st.mark_seen(keep.id, false).unwrap();
    imap::sync_folder(&mut first, &mut st, a, "INBOX")
        .await
        .unwrap();
    assert!(!st.message(a, keep.id).unwrap().seen);
    imap::drain_outbox(&mut first, &mut st, a).await.unwrap();
    imap::sync_folder(&mut first, &mut st, a, "INBOX")
        .await
        .unwrap();
    assert!(!st.message(a, keep.id).unwrap().seen);
}

#[tokio::test]
#[ignore = "needs local GreenMail on 127.0.0.1:3143"]
async fn uidvalidity_mismatch_parks_old_uid_without_writing_server() {
    let u = user("uidvalidity");
    let mut s = session(&u).await;
    deliver(&mut s, &mail("a@x", "untouched", "", "body")).await;
    let (mut st, a) = store(&u);
    imap::sync_folder(&mut s, &mut st, a, "INBOX")
        .await
        .unwrap();
    let row = st.view(a, Destination::Screener, 10).unwrap().remove(0);
    st.mark_seen(row.id, true).unwrap();
    let mut cursor = st.cursor(a, "INBOX").unwrap();
    cursor.uidvalidity = Some(cursor.uidvalidity.unwrap().wrapping_add(1));
    st.set_cursor(a, "INBOX", cursor).unwrap();
    let report = imap::drain_outbox(&mut s, &mut st, a).await.unwrap();
    assert_eq!(report.parked, 1);
    assert_eq!(st.needs_review(a).unwrap().len(), 1);
    s.select("INBOX").await.unwrap();
    let flags: Vec<_> = s
        .uid_fetch(row.uid.to_string(), "(UID FLAGS)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(!flags[0]
        .flags()
        .any(|f| matches!(f, async_imap::types::Flag::Seen)));
    let report = imap::sync_folder(&mut s, &mut st, a, "INBOX")
        .await
        .unwrap();
    assert!(report.reset);
    assert_eq!(st.needs_review(a).unwrap().len(), 1);
    assert!(!st.view(a, Destination::Screener, 10).unwrap()[0].seen);
}
