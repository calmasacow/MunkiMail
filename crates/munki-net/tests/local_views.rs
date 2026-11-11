//! The normal desktop policy must never emit metadata writes while browsing.
use munki_core::{triage::Stack, Address, Destination, SenderVerdict};
use munki_net::imap;
use munki_store::Store;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

#[tokio::test]
async fn browse_preserves_server_and_local_overlays() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"* OK test\r\n").await?;
        let raw="From: sender@example.test\r\nTo: me@example.test\r\nSubject: Folder message\r\nMessage-ID: <folder@test>\r\n\r\nBody\r\n";
        while let Some(line) = lines.next_line().await? {
            let (tag, command) = line.split_once(' ').unwrap();
            let command = command.to_uppercase();
            let response = if command.starts_with("LOGIN ") {
                String::new()
            } else if command == "CAPABILITY" {
                "* CAPABILITY IMAP4rev1\r\n".into()
            } else if command.starts_with("LIST ") {
                "* LIST (\\Noselect) \".\" \"Projects\"\r\n* LIST () \".\" \"Projects.2026\"\r\n"
                    .into()
            } else if command.starts_with("EXAMINE ") {
                "* 1 EXISTS\r\n* OK [UIDVALIDITY 42] valid\r\n* OK [UIDNEXT 2] next\r\n".into()
            } else if command == "UID SEARCH ALL" {
                "* SEARCH 1\r\n".into()
            } else if command.starts_with("UID FETCH ") && command.contains("BODY.PEEK[]") {
                format!(
                    "* 1 FETCH (UID 1 FLAGS () BODY[] {{{}}}\r\n{raw})\r\n",
                    raw.len()
                )
            } else if command.starts_with("UID FETCH ") && command.ends_with("(UID FLAGS)") {
                "* 1 FETCH (UID 1 FLAGS ())\r\n".into()
            } else if command == "LOGOUT" {
                write
                    .write_all(format!("* BYE\r\n{tag} OK logout\r\n").as_bytes())
                    .await?;
                break;
            } else {
                panic!("Browsing attempted unexpected server command: {command}")
            };
            write
                .write_all(format!("{response}{tag} OK done\r\n").as_bytes())
                .await?;
        }
        anyhow::Ok(())
    });
    let mut store = Store::open_in_memory()?;
    store.enable_local_views()?;
    let account = store.add_account("me@example.test", "Me", "imap")?;
    let client = imap::connect_insecure("127.0.0.1", port).await?;
    let mut session = imap::login(
        client,
        imap::Auth::Password {
            user: "me",
            pass: "test",
        },
    )
    .await?;
    imap::discover_folders(&mut session, &mut store, account).await?;
    assert_eq!(
        store.conn().query_row(
            "SELECT count(*) FROM server_folders WHERE account=?1",
            [account],
            |r| r.get::<_, i64>(0)
        )?,
        2
    );
    assert_eq!(
        imap::drain_outbox(&mut session, &mut store, account)
            .await?
            .applied,
        0
    );
    imap::sync_folder(&mut session, &mut store, account, "Projects.2026").await?;
    let id = store
        .folder_message_id(account, "Projects.2026", 1)?
        .unwrap();
    store.decide_senders(
        account,
        &[(
            Address::new("sender@example.test"),
            SenderVerdict::Allow(Destination::Feed),
        )],
    )?;
    store.set_stack(account, id, Stack::SetAside, true)?;
    store.rename_subject(id, Some("Local title"))?;
    assert_eq!(
        store.pending_count(account)?,
        0,
        "Local metadata must not queue server writes"
    );
    imap::sync_folder(&mut session, &mut store, account, "Projects.2026").await?;
    let message = store.message(account, id)?;
    assert_eq!(message.destination, Destination::Feed);
    assert_eq!(message.renamed_subject.as_deref(), Some("Local title"));
    assert!(store
        .conn()
        .query_row("SELECT set_aside FROM messages WHERE id=?1", [id], |r| r
            .get::<_, bool>(
            0
        ))?);
    store.mark_seen(id, true)?;
    assert_eq!(
        store.pending_count(account)?,
        1,
        "Read/unread remains synchronized"
    );
    session.logout().await?;
    server.await??;
    Ok(())
}

#[test]
fn switching_to_local_views_keeps_delivery_and_read_work() -> anyhow::Result<()> {
    use munki_core::OpKind;
    let mut store = Store::open_in_memory()?;
    let account = store.add_account("me@test", "Me", "imap")?;
    for kind in [
        OpKind::PutState {
            version: 1,
            json: "{}".into(),
        },
        OpKind::SetSeen {
            folder: "INBOX".into(),
            uid: 1,
            seen: true,
        },
        OpKind::Submit { draft_id: 7 },
    ] {
        store.enqueue(account, &kind)?;
    }
    store.enable_local_views()?;
    assert_eq!(store.pending_count(account)?, 2);
    store.enqueue(
        account,
        &OpKind::SetDestination {
            folder: "INBOX".into(),
            uid: 1,
            destination: Destination::Feed,
        },
    )?;
    assert_eq!(store.pending_count(account)?, 2);
    assert_eq!(
        imap::oauth_bearer("me@yahoo.com", "token", "imap.mail.yahoo.com", 993)?,
        "n,a=me@yahoo.com,\x01host=imap.mail.yahoo.com\x01port=993\x01auth=Bearer token\x01\x01"
    );
    assert!(imap::oauth_bearer("me", "bad\x01token", "imap.mail.yahoo.com", 993).is_err());
    Ok(())
}

#[test]
fn sent_copy_recovery_uses_existing_namespace_without_resubmitting() -> anyhow::Result<()> {
    use munki_core::OpKind;
    let store = Store::open_in_memory()?;
    let account = store.add_account("me@test", "Me", "imap")?;
    store.conn().execute("INSERT INTO server_folders(account,name,delimiter,selectable) VALUES(?1,'INBOX','.',1),(?1,'INBOX.Sent','.',1)", [account])?;
    assert_eq!(imap::sent_folder(&store, account, "Sent")?, "INBOX.Sent");
    assert_eq!(imap::sent_folder(&store, account, "INBOX")?, "INBOX");
    assert!(imap::sent_folder(&store, account, "Missing").is_err());
    store.enqueue(
        account,
        &OpKind::SaveSent {
            draft_id: 7,
            folder: "Sent".into(),
        },
    )?;
    store.enqueue(account, &OpKind::Submit { draft_id: 8 })?;
    store.conn().execute("UPDATE outbox SET not_before=9223372036854775807,last_error='bad namespace' WHERE json_extract(kind,'$.kind')='save_sent'", [])?;
    assert_eq!(imap::repair_sent_copies(&store, account)?, 1);
    let (folder, retry, error): (String,i64,Option<String>) = store.conn().query_row("SELECT json_extract(kind,'$.folder'),not_before,last_error FROM outbox WHERE json_extract(kind,'$.kind')='save_sent'", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(folder, "INBOX.Sent");
    assert_eq!(retry, 0);
    assert_eq!(error, None);
    let submit: String = store.conn().query_row(
        "SELECT kind FROM outbox WHERE json_extract(kind,'$.kind')='submit'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<OpKind>(&submit)?,
        OpKind::Submit { draft_id: 8 }
    );
    assert_eq!(imap::repair_sent_copies(&store, account)?, 0);
    store.conn().execute(
        "UPDATE server_folders SET special_use='sent' WHERE name='INBOX.Sent'",
        [],
    )?;
    assert_eq!(
        imap::sent_folder(&store, account, "Localized")?,
        "INBOX.Sent"
    );
    Ok(())
}
