//! Build once, persist once, send the exact stored bytes.
use anyhow::{ensure, Result};
use mail_builder::{headers::content_type::ContentType, MessageBuilder};
use munki_core::{compose::Draft, AccountId};
use munki_store::Store;

fn mailbox(value: &str) -> Result<()> {
    // ponytail: ASCII dot-atom mailboxes only; add SMTPUTF8/quoted localparts
    // together with server capability negotiation when required.
    let (local, domain) = value.split_once('@').unwrap_or(("", ""));
    ensure!(
        !local.is_empty() && local.len() <= 64 && !domain.is_empty() && value.len() <= 254,
        "invalid email address"
    );
    ensure!(
        !local.starts_with('.') && !local.ends_with('.') && !local.contains(".."),
        "invalid localpart"
    );
    ensure!(
        local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b)),
        "unsupported localpart"
    );
    ensure!(
        domain.split('.').all(|label| !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')),
        "invalid email domain"
    );
    Ok(())
}

fn attachment_content_type(value: &str) -> Result<ContentType<'_>> {
    ensure!(
        !value.chars().any(char::is_control),
        "invalid attachment content type"
    );
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    let mut parts = value.split(';');
    let media = parts.next().unwrap_or_default().trim();
    let (major, minor) = media.split_once('/').unwrap_or(("", ""));
    ensure!(
        token(major) && token(minor),
        "invalid attachment content type"
    );
    let mut content_type = ContentType::new(media);
    // ponytail: token-valued MIME parameters only. Add quoted-string parsing
    // when an attachment needs values containing spaces or MIME separators.
    for parameter in parts {
        let (key, value) = parameter
            .trim()
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid MIME parameter"))?;
        let (key, value) = (key.trim(), value.trim());
        ensure!(token(key) && token(value), "invalid MIME parameter");
        ensure!(
            !content_type
                .attributes
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(key)),
            "duplicate MIME parameter"
        );
        content_type = content_type.attribute(key, value);
    }
    Ok(content_type)
}

pub fn build(draft: &Draft) -> Result<(String, Vec<u8>)> {
    mailbox(&draft.from)?;
    ensure!(
        !draft.to.is_empty() || !draft.cc.is_empty() || !draft.bcc.is_empty(),
        "recipient required"
    );
    for address in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        mailbox(address)?;
    }
    ensure!(
        !draft.subject.chars().any(char::is_control),
        "invalid subject"
    );
    let mut m = MessageBuilder::new()
        .from(draft.from.as_str())
        .subject(draft.subject.as_str())
        .text_body(draft.text.as_str());
    if !draft.to.is_empty() {
        m = m.to(draft.to.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if !draft.cc.is_empty() {
        m = m.cc(draft.cc.iter().map(String::as_str).collect::<Vec<_>>());
    }
    // Bcc belongs exclusively to the SMTP envelope, never the wire headers.
    if let Some(html) = &draft.html {
        m = m.html_body(html.as_str());
    }
    if let Some(id) = &draft.in_reply_to {
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>'),
            "invalid reply Message-ID"
        );
        m = m.in_reply_to(id.as_str()).references(id.as_str());
    }
    for a in &draft.attachments {
        ensure!(
            !a.filename.is_empty() && !a.filename.chars().any(char::is_control),
            "invalid attachment filename"
        );
        m = m.attachment(
            attachment_content_type(&a.content_type)?,
            a.filename.as_str(),
            a.bytes.as_slice(),
        );
    }
    let wire = m.write_to_vec()?;
    let parsed = mail_parser::MessageParser::new()
        .parse(&wire)
        .ok_or_else(|| anyhow::anyhow!("built message cannot be parsed"))?;
    let id = parsed
        .message_id()
        .ok_or_else(|| anyhow::anyhow!("built message has no Message-ID"))?
        .to_string();
    Ok((id, wire))
}

pub fn queue(
    store: &mut Store,
    account: AccountId,
    draft_id: i64,
    sent_folder: &str,
) -> Result<()> {
    let draft = store.draft(account, draft_id)?;
    let (id, wire) = build(&draft)?;
    store.queue_draft(account, draft_id, &id, &wire, sent_folder)
}

/// Plain-text quoting remains readable across Gmail and Outlook.
pub fn reply(from: &str, original: &crate::parse::Parsed) -> Draft {
    Draft {
        from: from.into(),
        to: vec![original
            .reply_to
            .as_ref()
            .unwrap_or(&original.from)
            .to_string()],
        subject: if original.subject.to_ascii_lowercase().starts_with("re:") {
            original.subject.clone()
        } else {
            format!("Re: {}", original.subject)
        },
        text: format!(
            "\n\nOn the previous message, {} wrote:\n{}",
            original.from,
            original
                .text
                .lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        in_reply_to: Some(
            original
                .message_id
                .trim_start_matches('<')
                .trim_end_matches('>')
                .into(),
        ),
        ..Default::default()
    }
}

pub fn forward(from: &str, original: &crate::parse::Parsed) -> Draft {
    Draft {
        from: from.into(),
        subject: format!("Fwd: {}", original.subject),
        text: format!(
            "\n\n---------- Forwarded message ----------\nFrom: {}\nSubject: {}\n\n{}",
            original.from, original.subject, original.text
        ),
        // ponytail: forwards quote text only; attach original MIME when forwarding attachments is requested.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munki_core::compose::Attachment;

    #[test]
    fn calendar_reply_preserves_mime_parameters_and_rejects_injection() {
        let calendar = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n";
        let mut draft = Draft {
            from: "me@example.com".into(),
            to: vec!["organizer@example.com".into()],
            text: "Accepted".into(),
            attachments: vec![Attachment {
                filename: "reply.ics".into(),
                content_type: "text/calendar; method=REPLY; charset=utf-8".into(),
                bytes: calendar.to_vec(),
            }],
            ..Default::default()
        };
        let (_, wire) = build(&draft).unwrap();
        let parsed = mail_parser::MessageParser::new().parse(&wire).unwrap();
        let part = parsed.attachments().next().unwrap();
        let content_type = part.content_type().unwrap();
        assert_eq!(content_type.ctype(), "text");
        assert_eq!(content_type.subtype(), Some("calendar"));
        assert_eq!(content_type.attribute("method"), Some("REPLY"));
        assert_eq!(content_type.attribute("charset"), Some("utf-8"));
        assert_eq!(part.attachment_name(), Some("reply.ics"));
        assert_eq!(part.decoded().as_ref(), calendar);
        for bad in [
            "text/calendar; method=REPLY\r\nBcc: victim@example.com",
            "text/calendar; method=REPLY; METHOD=REQUEST",
            "text/calendar; method",
            "text/calendar; method=",
            "text/calendar; =REPLY",
            "text/calendar; method=REPLY/REQUEST",
            "text/calendar/extra",
            "text/calendar; method=\"REPLY\"",
        ] {
            draft.attachments[0].content_type = bad.into();
            assert!(build(&draft).is_err(), "{bad:?}");
        }
    }
}
