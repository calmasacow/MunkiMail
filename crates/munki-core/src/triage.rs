//! Where a message belongs, and the heuristics that guess it.
//!
//! Heuristics only. The AI layer (milestone 7) refines a `Suggestion`,
//! it never replaces this: header-based rules are free, offline, and
//! explainable to the user, which matters for a feature that hides mail.

use serde::{Deserialize, Serialize};

use crate::Address;

/// The four views a message can land in. Stored as a column, not a
/// folder, so reclassifying is a local write rather than a server move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// Real mail from a human you screened in.
    Imbox,
    /// Newsletters and updates, read on your schedule.
    Feed,
    /// Receipts, shipping, invoices. Quiet and searchable.
    PaperTrail,
    /// Awaiting a screening decision.
    Screener,
}

impl Destination {
    /// IMAP/JMAP keyword this destination mirrors to, so a second
    /// machine recovers the verdict without any server of ours.
    pub fn keyword(self) -> &'static str {
        match self {
            Destination::Imbox => "$MunkiImbox",
            Destination::Feed => "$MunkiFeed",
            Destination::PaperTrail => "$MunkiPaper",
            Destination::Screener => "$MunkiScreener",
        }
    }

    pub fn from_keyword(kw: &str) -> Option<Self> {
        match kw {
            "$MunkiImbox" => Some(Destination::Imbox),
            "$MunkiFeed" => Some(Destination::Feed),
            "$MunkiPaper" => Some(Destination::PaperTrail),
            "$MunkiScreener" => Some(Destination::Screener),
            _ => None,
        }
    }
}

/// A standing decision about a sender, set by the user in the Screener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderVerdict {
    /// Approved: route to this destination from now on.
    Allow(Destination),
    /// Screened out. Filtered from views, never deleted, always searchable.
    Dismiss,
}

/// The headers the classifier needs. A borrowed view so callers can
/// build it from `mail-parser` output without allocating a copy.
#[derive(Debug, Default, Clone, Copy)]
pub struct Headers<'a> {
    pub from: &'a str,
    pub subject: &'a str,
    /// Any `List-Id` / `List-Unsubscribe` present.
    pub has_list_header: bool,
    /// `Auto-Submitted:` present and not `no`.
    pub auto_submitted: bool,
    /// `Precedence:` is `bulk`, `list`, or `junk`.
    pub bulk_precedence: bool,
}

/// A guess, with the reason, so the Screener can explain itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub destination: Destination,
    /// Human-readable justification shown in the Screener row.
    pub reason: String,
}

/// Subject/sender fragments that reliably mark transactional paperwork.
/// Matched case-insensitively against the subject.
const PAPER_TRAIL_SUBJECT: &[&str] = &[
    "receipt",
    "invoice",
    "order confirmation",
    "your order",
    "has shipped",
    "shipping confirmation",
    "tracking number",
    "payment received",
    "payment confirmation",
    "statement is ready",
    "booking confirmation",
    "itinerary",
];

/// Localparts used by no-reply automation. A sender here is never a
/// human waiting on a reply, so it must not reach the Imbox.
const NOREPLY_LOCALPARTS: &[&str] = &[
    "no-reply",
    "noreply",
    "donotreply",
    "do-not-reply",
    "bounce",
    "mailer-daemon",
    "postmaster",
];

/// Guess a destination for a message from an unknown sender.
///
/// Order matters: paperwork is checked before bulk, because a shipping
/// notice usually carries list headers too and belongs in Paper Trail,
/// not the Feed.
pub fn suggest(h: Headers<'_>) -> Suggestion {
    let subject = h.subject.to_lowercase();
    let from = Address::new(h.from);

    if let Some(hit) = PAPER_TRAIL_SUBJECT.iter().find(|p| subject.contains(**p)) {
        return Suggestion {
            destination: Destination::PaperTrail,
            reason: format!("subject mentions \"{hit}\""),
        };
    }

    if h.has_list_header {
        return Suggestion {
            destination: Destination::Feed,
            reason: "has mailing-list headers".into(),
        };
    }

    if h.bulk_precedence {
        return Suggestion {
            destination: Destination::Feed,
            reason: "sent with bulk precedence".into(),
        };
    }

    let localpart = from.as_str().split('@').next().unwrap_or("");
    if NOREPLY_LOCALPARTS.iter().any(|n| localpart.contains(n)) {
        return Suggestion {
            destination: Destination::Feed,
            reason: "sent from a no-reply address".into(),
        };
    }

    if h.auto_submitted {
        return Suggestion {
            destination: Destination::Feed,
            reason: "automatically generated".into(),
        };
    }

    Suggestion {
        destination: Destination::Imbox,
        reason: "looks like a person".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h<'a>(from: &'a str, subject: &'a str) -> Headers<'a> {
        Headers {
            from,
            subject,
            ..Default::default()
        }
    }

    #[test]
    fn plain_human_mail_reaches_imbox() {
        assert_eq!(
            suggest(h("jane@example.com", "lunch thursday?")).destination,
            Destination::Imbox
        );
    }

    #[test]
    fn receipts_beat_list_headers() {
        // A shipping notice carries List-Unsubscribe but is paperwork,
        // not a newsletter. Regression guard on the ordering in suggest().
        let hs = Headers {
            has_list_header: true,
            ..h("ship@store.example", "Your order has shipped!")
        };
        assert_eq!(suggest(hs).destination, Destination::PaperTrail);
    }

    #[test]
    fn newsletters_go_to_feed() {
        let hs = Headers {
            has_list_header: true,
            ..h("news@substack.example", "This week in Rust")
        };
        assert_eq!(suggest(hs).destination, Destination::Feed);
    }

    #[test]
    fn noreply_never_reaches_imbox() {
        for lp in NOREPLY_LOCALPARTS {
            let from = format!("{lp}@bank.example");
            let got = suggest(h(&from, "hello")).destination;
            assert_ne!(got, Destination::Imbox, "{from} was routed to Imbox");
        }
    }

    #[test]
    fn keyword_roundtrips_every_destination() {
        for d in [
            Destination::Imbox,
            Destination::Feed,
            Destination::PaperTrail,
            Destination::Screener,
        ] {
            assert_eq!(Destination::from_keyword(d.keyword()), Some(d));
        }
    }

    #[test]
    fn unknown_keyword_is_not_a_destination() {
        assert_eq!(Destination::from_keyword("$Forwarded"), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stack {
    ReplyLater,
    SetAside,
}
impl Stack {
    pub fn keyword(self) -> &'static str {
        match self {
            Self::ReplyLater => "$MunkiReplyLater",
            Self::SetAside => "$MunkiSetAside",
        }
    }
}
