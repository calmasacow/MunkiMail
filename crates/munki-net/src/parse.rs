//! RFC 5322 bytes to the fields the store and classifier need.
//!
//! `mail-parser` does the hard part (MIME, charsets, RFC 2047). This
//! module only decides *which* fields matter and how to fall back when
//! a real-world message omits them.

use mail_parser::{Message, MessageParser};
use munki_core::triage::Headers;
use munki_core::Address;

/// Everything extracted from one message. Owned, because the raw bytes
/// are dropped once this is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub message_id: String,
    pub thread_id: String,
    pub from: Address,
    pub reply_to: Option<Address>,
    pub subject: String,
    pub date: i64,
    pub text: String,
    /// Raw HTML body, unsanitized. Never render this; pass it through
    /// `render::sanitize` first.
    pub html: Option<String>,
    pub attachment_names: Vec<String>,
    pub has_list_header: bool,
    pub auto_submitted: bool,
    pub bulk_precedence: bool,
}

impl Parsed {
    /// The view the triage heuristics need, borrowed from this message.
    pub fn headers(&self) -> Headers<'_> {
        Headers {
            from: self.from.as_str(),
            subject: &self.subject,
            has_list_header: self.has_list_header,
            auto_submitted: self.auto_submitted,
            bulk_precedence: self.bulk_precedence,
        }
    }
}

/// Parse a message. `fallback_id` names it when the sender omitted
/// `Message-ID` (common in automated mail); `fallback_date` is the
/// server's INTERNALDATE, used when `Date:` is missing or garbage.
pub fn parse(raw: &[u8], fallback_id: &str, fallback_date: i64) -> Option<Parsed> {
    let msg = MessageParser::new().parse(raw)?;

    let message_id = msg
        .message_id()
        .map(|id| format!("<{id}>"))
        .unwrap_or_else(|| fallback_id.to_string());

    let thread_id = thread_root(&msg).unwrap_or_else(|| message_id.clone());

    let from = msg
        .from()
        .and_then(|l| l.first())
        .and_then(|m| m.address())
        .map(Address::new)
        .unwrap_or_else(|| Address::new(""));

    let date = msg
        .date()
        .map(|d| d.to_timestamp())
        // A Date: in the far future would pin a message to the top of
        // every view forever. Trust the server's clock over the sender's.
        .filter(|&t| t > 0 && t <= fallback_date + 86_400)
        .unwrap_or(fallback_date);

    let text = msg.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
    let html = msg.body_html(0).map(|h| h.into_owned());

    let attachment_names = msg
        .attachments()
        .filter_map(|a| a.attachment_name().map(str::to_string))
        .collect();

    let h = msg.headers();
    let has_list_header = h.contains("List-Id") || h.contains("List-Unsubscribe");
    let auto_submitted = h
        .value("Auto-Submitted")
        .and_then(|v| v.as_text())
        .is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"));
    let bulk_precedence = h
        .value("Precedence")
        .and_then(|v| v.as_text())
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "bulk" | "list" | "junk"
            )
        });

    Some(Parsed {
        message_id,
        thread_id,
        from,
        reply_to: msg
            .reply_to()
            .and_then(|list| list.first())
            .and_then(|m| m.address())
            .map(Address::new),
        subject: msg.subject().unwrap_or_default().to_string(),
        date,
        text,
        html,
        attachment_names,
        has_list_header,
        auto_submitted,
        bulk_precedence,
    })
}

/// Thread root: the first `References` entry, else `In-Reply-To`.
///
/// `ponytail:` header-only threading. Misses threads whose clients drop
/// `References` (some mobile clients, some list software). Upgrade to
/// JWZ with subject fallback when users report split threads.
fn thread_root(msg: &Message<'_>) -> Option<String> {
    msg.references()
        .and_then(|r| r.first().map(str::to_string))
        .or_else(|| {
            msg.in_reply_to()
                .and_then(|r| r.first().map(str::to_string))
        })
        .map(|id| format!("<{id}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_710_504_000;

    #[test]
    fn plain_message() {
        let raw = b"From: Jane Doe <Jane@Example.com>\r\n\
            Subject: lunch\r\n\
            Message-ID: <abc@example.com>\r\n\
            Date: Fri, 15 Mar 2024 10:00:00 +0000\r\n\
            \r\n\
            free at noon?\r\n";
        let p = parse(raw, "<fallback>", NOW).unwrap();
        assert_eq!(p.from.as_str(), "jane@example.com");
        assert_eq!(p.subject, "lunch");
        assert_eq!(p.message_id, "<abc@example.com>");
        assert_eq!(p.thread_id, "<abc@example.com>", "a root threads to itself");
        assert_eq!(p.date, 1_710_496_800);
        assert!(p.text.contains("noon"));
        assert!(!p.has_list_header);
    }

    #[test]
    fn replies_thread_to_the_first_reference() {
        let raw = b"From: a@x\r\nMessage-ID: <c@x>\r\n\
            References: <root@x> <b@x>\r\nIn-Reply-To: <b@x>\r\n\r\nhi\r\n";
        assert_eq!(parse(raw, "<f>", NOW).unwrap().thread_id, "<root@x>");
    }

    #[test]
    fn missing_headers_fall_back_instead_of_failing() {
        let p = parse(b"\r\nbody only\r\n", "<uid-9@local>", NOW).unwrap();
        assert_eq!(p.message_id, "<uid-9@local>");
        assert_eq!(p.date, NOW);
        assert_eq!(p.subject, "");
    }

    #[test]
    fn future_dates_are_clamped_to_server_time() {
        let raw = b"From: a@x\r\nDate: Mon, 1 Jan 2099 00:00:00 +0000\r\n\r\nx\r\n";
        assert_eq!(parse(raw, "<f>", NOW).unwrap().date, NOW);
    }

    #[test]
    fn list_and_bulk_headers_are_detected() {
        let raw = b"From: news@list.example\r\nList-Unsubscribe: <mailto:u@x>\r\n\
            Precedence: bulk\r\nAuto-Submitted: auto-generated\r\n\r\nx\r\n";
        let p = parse(raw, "<f>", NOW).unwrap();
        assert!(p.has_list_header && p.bulk_precedence && p.auto_submitted);

        let raw = b"From: a@x\r\nAuto-Submitted: no\r\n\r\nx\r\n";
        assert!(!parse(raw, "<f>", NOW).unwrap().auto_submitted);
    }

    #[test]
    fn attachments_and_html_are_extracted() {
        let raw = concat!(
            "From: a@x\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/html\r\n\r\n<p>hello <b>there</b></p>\r\n",
            "--b\r\nContent-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\r\nJVBERi0=\r\n",
            "--b--\r\n"
        );
        let p = parse(raw.as_bytes(), "<f>", NOW).unwrap();
        assert_eq!(p.attachment_names, vec!["invoice.pdf"]);
        assert!(p.html.unwrap().contains("<b>there</b>"));
        // Text-less HTML mail still gets searchable text.
        assert!(p.text.contains("hello"), "text was {:?}", p.text);
    }
}
