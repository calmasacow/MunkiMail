use munki_core::{
    compose::{Attachment, Draft},
    OpKind,
};
use munki_net::{compose, imap, smtp};
use munki_store::Store;

fn draft() -> Draft {
    Draft {
        from: "sender@munki.test".into(),
        to: vec!["recipient@munki.test".into()],
        bcc: vec!["secret@munki.test".into()],
        subject: "Résumé & report".into(),
        text: "Hello\n> previous reply".into(),
        html: Some("<p>Hello</p><blockquote>previous reply</blockquote>".into()),
        attachments: vec![Attachment {
            filename: "report.bin".into(),
            content_type: "application/octet-stream".into(),
            bytes: vec![0, 1, 255],
        }],
        ..Default::default()
    }
}

#[test]
fn mime_roundtrip_and_header_validation() {
    let d = draft();
    let (id, wire) = compose::build(&d).unwrap();
    let p = mail_parser::MessageParser::new().parse(&wire).unwrap();
    assert_eq!(p.message_id(), Some(id.as_str()));
    assert_eq!(p.subject(), Some(d.subject.as_str()));
    assert!(p.bcc().is_none());
    assert!(p.body_text(0).unwrap().contains("> previous reply"));
    assert!(p.body_html(0).unwrap().contains("blockquote"));
    assert_eq!(
        p.attachments().next().unwrap().attachment_name(),
        Some("report.bin")
    );
    assert_eq!(
        p.attachments().next().unwrap().decoded().as_ref(),
        &[0, 1, 255]
    );
    let original = munki_net::parse::parse(&wire, "fallback", i64::MAX - 86400).unwrap();
    let reply_to = munki_net::parse::parse(
        b"From: sender@x\r\nReply-To: replies@x\r\n\r\nhello",
        "<f@x>",
        1,
    )
    .unwrap();
    assert_eq!(compose::reply("me@x", &reply_to).to, vec!["replies@x"]);
    let (_, reply) = compose::build(&compose::reply("me@munki.test", &original)).unwrap();
    let reply = mail_parser::MessageParser::new().parse(&reply).unwrap();
    assert_eq!(reply.in_reply_to().unwrap().first(), Some(id.as_str()));
    assert!(reply.body_text(0).unwrap().contains("> Hello"));
    let mut forwarded = compose::forward("me@munki.test", &original);
    forwarded.to = vec!["friend@munki.test".into()];
    assert!(compose::build(&forwarded).is_ok());
    for bad in [
        "victim@munki.test\r\nBcc: evil@munki.test",
        "a@@x",
        "a@-x",
        "a..b@x",
    ] {
        let mut bad_d = d.clone();
        bad_d.to = vec![bad.into()];
        assert!(compose::build(&bad_d).is_err());
    }
    let mut bad_d = d;
    bad_d.subject = "hi\r\nBcc: evil@x".into();
    assert!(compose::build(&bad_d).is_err());
}

#[test]
fn drafts_survive_restart_and_send_freezes_content() {
    let dir = std::env::temp_dir().join(format!("munki-draft-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("draft.db");
    let _ = std::fs::remove_file(&path);
    let mut store = Store::open(&path, None).unwrap();
    let account = store.add_account("me@munki.test", "", "imap").unwrap();
    let id = store.save_draft(account, None, &draft()).unwrap();
    let mut edited = draft();
    edited.text = "saved edit".into();
    store.save_draft(account, Some(id), &edited).unwrap();
    drop(store);
    store = Store::open(&path, None).unwrap();
    assert_eq!(store.draft(account, id).unwrap(), edited);
    compose::queue(&mut store, account, id, "Sent").unwrap();
    assert!(compose::queue(&mut store, account, id, "Sent").is_err());
    assert!(store.save_draft(account, Some(id), &draft()).is_err());
    assert!(store.lease_transport(account, false).unwrap().is_none());
    let op = store.lease_transport(account, true).unwrap().unwrap();
    assert_eq!(op.kind, OpKind::Submit { draft_id: id });
    store
        .conn()
        .execute("UPDATE outbox SET leased_at=0", [])
        .unwrap();
    assert_eq!(store.recover_leases(account).unwrap(), (0, 1));
    assert_eq!(store.needs_review(account).unwrap().len(), 1);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn lost_smtp_ack_requires_review_without_resending() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (client, server) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        let mut stream = BufReader::new(server);
        let mut line = String::new();
        loop {
            line.clear();
            if stream.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            if line.starts_with("DATA") {
                stream.write_all(b"354 send data\r\n").await.unwrap();
                loop {
                    line.clear();
                    stream.read_line(&mut line).await.unwrap();
                    if line == ".\r\n" {
                        return;
                    }
                }
            }
            stream.write_all(b"250 OK\r\n").await.unwrap();
        }
    });
    let mut client = mail_send::SmtpClient {
        stream: client,
        timeout: std::time::Duration::from_secs(2),
    };
    let mut store = Store::open_in_memory().unwrap();
    let a = store.add_account("sender@munki.test", "", "imap").unwrap();
    let id = store.save_draft(a, None, &draft()).unwrap();
    compose::queue(&mut store, a, id, "Sent").unwrap();
    let result = smtp::drain_outbox(&mut client, &mut store, a)
        .await
        .unwrap();
    assert_eq!(result.parked, 1);
    assert_eq!(store.needs_review(a).unwrap().len(), 1);
    assert_eq!(
        smtp::drain_outbox(&mut client, &mut store, a)
            .await
            .unwrap()
            .applied,
        0
    );
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "needs GreenMail SMTP 3025 and IMAP 3143"]
async fn smtp_delivery_bcc_and_sent_copy_are_durable() {
    use futures::TryStreamExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut d = draft();
    d.from = format!("sender-{nonce}@munki.test");
    d.to = vec![format!("recipient-{nonce}@munki.test")];
    d.bcc = vec![format!("secret-{nonce}@munki.test")];
    let c = imap::connect_insecure("127.0.0.1", 3143).await.unwrap();
    let mut session = imap::login(
        c,
        imap::Auth::Password {
            user: &d.from,
            pass: &d.from,
        },
    )
    .await
    .unwrap();
    assert!(session.select("Sent Archive").await.is_err());
    let mut store = Store::open_in_memory().unwrap();
    let a = store.add_account(&d.from, "", "imap").unwrap();
    let id = store.save_draft(a, None, &d).unwrap();
    compose::queue(&mut store, a, id, "Sent Archive").unwrap();
    let mut client = mail_send::SmtpClientBuilder::new("127.0.0.1", 3025)
        .unwrap()
        .connect_plain()
        .await
        .unwrap();
    assert_eq!(
        smtp::drain_outbox(&mut client, &mut store, a)
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(
        smtp::drain_outbox(&mut client, &mut store, a)
            .await
            .unwrap()
            .applied,
        0
    );
    let (message_id, _) = store.draft_wire(a, id).unwrap();
    assert_eq!(
        imap::drain_outbox(&mut session, &mut store, a)
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(session.select("Sent Archive").await.unwrap().exists, 1);
    assert_eq!(store.pending_count(a).unwrap(), 0);
    // Replay after a lost APPEND acknowledgement must find the existing copy.
    store
        .enqueue(
            a,
            &OpKind::SaveSent {
                draft_id: id,
                folder: "Sent Archive".into(),
            },
        )
        .unwrap();
    assert_eq!(
        imap::drain_outbox(&mut session, &mut store, a)
            .await
            .unwrap()
            .applied,
        1
    );
    assert_eq!(session.select("Sent Archive").await.unwrap().exists, 1);
    for recipient in d.to.iter().chain(&d.bcc) {
        let c = imap::connect_insecure("127.0.0.1", 3143).await.unwrap();
        let mut inbox = imap::login(
            c,
            imap::Auth::Password {
                user: recipient,
                pass: recipient,
            },
        )
        .await
        .unwrap();
        assert_eq!(inbox.select("INBOX").await.unwrap().exists, 1);
        let messages: Vec<_> = inbox
            .uid_fetch("1:*", "BODY.PEEK[]")
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let parsed = mail_parser::MessageParser::new()
            .parse(messages[0].body().unwrap())
            .unwrap();
        assert_eq!(parsed.message_id(), Some(message_id.as_str()));
        assert!(parsed.bcc().is_none());
    }
}
