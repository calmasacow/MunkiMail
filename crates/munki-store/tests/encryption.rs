//! At-rest encryption, verified rather than assumed.
//!
//! Only meaningful with the `encryption` feature, which is off in the
//! dev loop because it builds OpenSSL from source:
//!
//!   cargo test -p munki-store --no-default-features \
//!       --features encryption --test encryption
#![cfg(feature = "encryption")]

use munki_core::{Address, Destination};
use munki_store::{NewMessage, Store};

#[test]
fn encrypted_store_is_not_plaintext_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enc.db");

    {
        let mut s = Store::open(&path, Some("correct horse")).unwrap();
        let a = s.add_account("me@example.com", "Me", "imap").unwrap();
        s.upsert_message(
            &NewMessage {
                account: a,
                folder: "INBOX",
                uid: 1,
                message_id: "<x@y>",
                thread_id: "<x@y>",
                from: Address::new("jane@example.com"),
                subject: "SECRETSUBJECT",
                date: 0,
                seen: false,
                has_attachment: false,
                destination: Destination::Imbox,
            },
            "SECRETBODY",
            "",
        )
        .unwrap();
    }

    let bytes = std::fs::read(&path).unwrap();
    let hay = String::from_utf8_lossy(&bytes);
    // Subject and body must not be recoverable by anyone holding the file.
    assert!(!hay.contains("SECRETSUBJECT"), "subject readable on disk");
    assert!(!hay.contains("SECRETBODY"), "body readable on disk");
    assert!(
        !hay.starts_with("SQLite format 3"),
        "plaintext sqlite header"
    );

    // Reopening with the right key works; the wrong key must not.
    assert!(Store::open(&path, Some("correct horse")).is_ok());
    assert!(
        Store::open(&path, Some("wrong key")).is_err(),
        "wrong key opened the store"
    );
}
