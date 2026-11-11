//! Pending user actions, queued locally and replayed against the server.
//!
//! Every mutating action goes here first and is applied to the local DB
//! immediately. The sync loop drains the queue with retry/backoff. Kill
//! the app mid-flight and nothing is lost -- that is what makes offline
//! mode real rather than a cache that forgets.

use serde::{Deserialize, Serialize};

use crate::{AccountId, Destination};

/// A single replayable action. Each variant must be *idempotent* on the
/// server: replay after an ambiguous failure has to be safe, because we
/// cannot distinguish "never arrived" from "arrived, ack lost".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpKind {
    /// Set or clear `\Seen`. Idempotent: a flag store, not a toggle.
    SetSeen {
        folder: String,
        uid: u32,
        seen: bool,
    },
    /// Mirror a triage verdict into the account's keywords.
    SetDestination {
        folder: String,
        uid: u32,
        destination: Destination,
    },
    /// Move to another folder. Carries the source so a replay after a
    /// partial move can detect the message is already gone and succeed.
    Move {
        from_folder: String,
        uid: u32,
        to_folder: String,
    },
    /// Append a built message to a folder (Sent, Drafts).
    ///
    /// `message_id` is the RFC 5322 Message-ID we generated. The sync
    /// loop searches for it before appending, so a replay cannot
    /// produce a duplicate in Sent.
    Append {
        folder: String,
        message_id: String,
        /// Path to the staged RFC 5322 bytes on disk. Not inlined:
        /// attachments make these rows large and the queue is read often.
        staged_path: String,
    },
    /// Hand a staged message to SMTP.
    ///
    /// Sending is the one action that is *not* naturally idempotent, so
    /// the store guards it: the row is marked `sending` before the SMTP
    /// dialogue and only a confirmed 5xx rewinds it. See
    /// `munki-store::outbox` for the state machine.
    Send {
        message_id: String,
        staged_path: String,
    },
    /// Submit a frozen draft. Ambiguous delivery requires human review.
    Submit { draft_id: i64 },
    /// Save a delivered draft without submitting it again.
    SaveSent { draft_id: i64, folder: String },
    /// Mirror a stack flag. Full settings also survive in the state folder.
    SetStack {
        folder: String,
        uid: u32,
        stack: crate::triage::Stack,
        enabled: bool,
    },
    /// Replace the triage state blob in `MunkiMail/.state`.
    PutState { version: u64, json: String },
}

/// A queued op with its replay bookkeeping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub id: i64,
    pub account: AccountId,
    pub kind: OpKind,
    /// Replay attempts so far. Drives exponential backoff.
    pub attempts: u32,
    /// Unix seconds; the op is invisible to the drainer until then.
    pub not_before: i64,
    /// Last failure, kept for the UI so a stuck op is explainable.
    pub last_error: Option<String>,
}

/// Backoff before the next attempt: 2^attempts seconds, capped at an
/// hour. Capped so a mailbox that was offline overnight recovers
/// promptly instead of sleeping for days.
pub fn backoff_secs(attempts: u32) -> i64 {
    const CAP: i64 = 3600;
    if attempts >= 12 {
        return CAP;
    }
    (1i64 << attempts).min(CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_secs(0), 1);
        assert_eq!(backoff_secs(4), 16);
        assert_eq!(backoff_secs(11), 2048);
        assert_eq!(backoff_secs(12), 3600);
        // Never regresses, never exceeds the cap, for any plausible count.
        let mut prev = 0;
        for a in 0..64 {
            let b = backoff_secs(a);
            assert!(b >= prev && b <= 3600, "attempts={a} gave {b}");
            prev = b;
        }
    }

    #[test]
    fn op_kind_survives_json_roundtrip() {
        // The queue is persisted as JSON; a schema drift here would
        // silently drop a user's pending action.
        let ops = vec![
            OpKind::SetSeen {
                folder: "INBOX".into(),
                uid: 7,
                seen: true,
            },
            OpKind::SetDestination {
                folder: "INBOX".into(),
                uid: 7,
                destination: Destination::PaperTrail,
            },
            OpKind::Move {
                from_folder: "INBOX".into(),
                uid: 7,
                to_folder: "Archive".into(),
            },
            OpKind::Append {
                folder: "Sent".into(),
                message_id: "<a@b>".into(),
                staged_path: "/tmp/a.eml".into(),
            },
            OpKind::Send {
                message_id: "<a@b>".into(),
                staged_path: "/tmp/a.eml".into(),
            },
            OpKind::PutState {
                version: 3,
                json: "{}".into(),
            },
        ];
        for op in ops {
            let s = serde_json::to_string(&op).unwrap();
            assert_eq!(serde_json::from_str::<OpKind>(&s).unwrap(), op);
        }
    }
}
