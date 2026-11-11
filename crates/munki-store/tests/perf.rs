//! The "search is slow and misses things" guard.
//!
//! Ignored by default because it builds a 100k-message store. Run with:
//!   cargo test -p munki-store --release --test perf -- --ignored --nocapture
//! Release matters: a debug build measures rustc, not SQLite.

use std::time::Instant;

use munki_core::{query, Address, Destination};
use munki_store::{NewMessage, Store};

const N: u32 = 100_000;
/// p95 budget. Above this, search stops feeling instant.
const BUDGET_MS: u128 = 100;

#[test]
#[ignore = "builds a 100k-message store"]
fn search_stays_under_budget_on_a_large_mailbox() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("perf.db"), None).unwrap();
    let a = s.add_account("me@example.com", "Me", "imap").unwrap();

    let build = Instant::now();
    for uid in 1..=N {
        let from = format!("sender{}@example.com", uid % 997);
        let subject = format!("thread {} status update", uid % 5000);
        let body = format!(
            "message {uid} about the quarterly report and deployment \
             pipeline, reference {}",
            uid * 7
        );
        let dest = match uid % 4 {
            0 => Destination::Imbox,
            1 => Destination::Feed,
            2 => Destination::PaperTrail,
            _ => Destination::Screener,
        };
        s.upsert_message(
            &NewMessage {
                account: a,
                folder: "INBOX",
                uid,
                message_id: &format!("<{uid}@example.com>"),
                thread_id: &format!("<{}@example.com>", uid % 5000),
                from: Address::new(&from),
                subject: &subject,
                date: 1_700_000_000 + uid as i64,
                seen: uid % 3 == 0,
                has_attachment: uid % 11 == 0,
                destination: dest,
            },
            &body,
            if uid % 11 == 0 { "report.pdf" } else { "" },
        )
        .unwrap();
    }
    eprintln!("built {N} messages in {:?}", build.elapsed());

    // Shapes the UI actually issues: common word, rare token, phrase,
    // filter-only, and filters combined with text.
    let queries = [
        "quarterly",
        "reference 700000",
        "\"quarterly report\"",
        "in:feed",
        "is:unread has:attachment",
        "from:sender42@example.com quarterly",
        "deployment after:2023-11-01",
    ];

    let mut worst = 0u128;
    for q in queries {
        let parsed = query::parse(q);
        let mut samples = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            let hits = s.search(a, &parsed, 100).unwrap();
            samples.push(t.elapsed().as_millis());
            assert!(hits.len() <= 100);
        }
        samples.sort_unstable();
        let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
        eprintln!("{q:40} p95 {p95:>4} ms  (max {})", samples.last().unwrap());
        worst = worst.max(p95);
    }

    assert!(
        worst <= BUDGET_MS,
        "worst p95 was {worst} ms, budget {BUDGET_MS} ms"
    );
}
