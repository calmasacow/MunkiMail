//! Domain types shared across MunkiMail. No I/O, no protocol, no UI.

pub mod outbox;
pub mod query;
pub mod triage;

pub use outbox::{Op, OpKind};
pub use query::Query;
pub use triage::{Destination, SenderVerdict};

use serde::{Deserialize, Serialize};

/// Stable identity for an account, assigned by the store on first login.
pub type AccountId = i64;
/// Store-local message rowid. Server identity is `(folder, uid)`.
pub type MessageId = i64;

/// An email address, normalized for joins across mail/calendar/contacts.
///
/// Normalization is lowercase + trim only. Deliberately *not* doing
/// gmail-style dot/plus stripping: that is provider-specific and would
/// merge genuinely distinct addresses on other hosts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address(String);

impl Address {
    pub fn new(raw: &str) -> Self {
        Address(raw.trim().to_lowercase())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Domain part, or `None` if the address has no `@`.
    pub fn domain(&self) -> Option<&str> {
        self.0.rsplit_once('@').map(|(_, d)| d)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A message as the store holds it. Body text lives in the FTS index,
/// not here, so list views stay cheap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub account: AccountId,
    pub folder: String,
    pub uid: u32,
    /// RFC 5322 Message-ID, used to stitch threads across folders.
    pub message_id: String,
    pub thread_id: String,
    pub from: Address,
    pub subject: String,
    /// Subject the user typed to replace an obscure one, if any.
    pub renamed_subject: Option<String>,
    pub date: i64,
    pub seen: bool,
    pub has_attachment: bool,
    pub destination: Destination,
}

impl Message {
    /// What the UI should show: the user's rename wins over the sender's subject.
    pub fn display_subject(&self) -> &str {
        self.renamed_subject.as_deref().unwrap_or(&self.subject)
    }
}

pub mod compose;

pub mod state;
