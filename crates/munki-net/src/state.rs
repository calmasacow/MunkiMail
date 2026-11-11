//! Immutable snapshots avoid overwriting concurrent appends by another device.
use crate::imap::Stream;
use anyhow::{ensure, Context, Result};
use async_imap::Session;
use futures::TryStreamExt;
use mail_builder::MessageBuilder;
use munki_core::{
    state::{StateSnapshot, MAX_STATE_BYTES},
    AccountId,
};
use munki_store::Store;

pub const STATE_FOLDER: &str = "MunkiMail/.state";

pub async fn sync<S: Stream>(
    session: &mut Session<S>,
    store: &mut Store,
    account: AccountId,
) -> Result<bool> {
    let folder = open_folder(session).await?;
    // ponytail: retain all snapshots, merge on read. Add cursor-based fetching
    // and coordinated compaction when snapshot history affects sync latency.
    let messages: Vec<_> = session
        .uid_fetch("1:*", "BODY.PEEK[]")
        .await?
        .try_collect()
        .await?;
    let mut remote = StateSnapshot::default();
    for message in messages {
        let Some(raw) = message.body() else { continue };
        ensure!(
            raw.len() <= MAX_STATE_BYTES * 2,
            "oversized triage settings message"
        );
        let parsed = mail_parser::MessageParser::new()
            .parse(raw)
            .context("invalid settings MIME")?;
        let Some(marker) = parsed.headers().get("X-MunkiMail-State") else {
            continue;
        };
        ensure!(
            marker.value().as_text() == Some("1"),
            "unsupported triage settings message version"
        );
        let text = parsed
            .body_text(0)
            .context("settings message has no JSON body")?;
        ensure!(text.len() <= MAX_STATE_BYTES, "triage state exceeds 8 MiB");
        let snapshot: StateSnapshot = serde_json::from_str(&text).context("invalid triage JSON")?;
        snapshot.validate().map_err(anyhow::Error::msg)?;
        remote.merge(snapshot);
        ensure!(
            serde_json::to_vec(&remote)?.len() <= MAX_STATE_BYTES,
            "merged triage state exceeds 8 MiB"
        );
    }
    store.merge_triage(account, &remote)?;
    let mut merged = store.triage_snapshot(account)?;
    merged.merge(StateSnapshot::default());
    if merged == remote {
        return Ok(false);
    }
    let json = serde_json::to_string(&merged)?;
    ensure!(json.len() <= MAX_STATE_BYTES, "triage state exceeds 8 MiB");
    let wire = MessageBuilder::new()
        .from("settings@munkimail.local")
        .to("settings@munkimail.local")
        .subject("MunkiMail settings")
        .header(
            "X-MunkiMail-State",
            mail_builder::headers::raw::Raw::new("1"),
        )
        .text_body(json)
        .write_to_vec()?;
    session
        .append(&folder, Some("(\\Seen)"), None, wire)
        .await?;
    Ok(true)
}

/// Preserve existing snapshots first, then use the server's actual hierarchy.
async fn open_folder<S: Stream>(session: &mut Session<S>) -> Result<String> {
    // Keep the established path on servers that already support it, including
    // older MunkiMail installations. Only discover alternatives after rejection.
    let mut last_error = match ensure_folder(session, "MunkiMail", STATE_FOLDER).await {
        Ok(()) => return Ok(STATE_FOLDER.into()),
        Err(error @ async_imap::error::Error::No(_)) => error,
        Err(error) => return Err(error.into()),
    };
    let names: Vec<_> = session
        .list(Some(""), Some("INBOX"))
        .await?
        .try_collect()
        .await?;
    let inbox = names
        .iter()
        .find(|n| n.name().eq_ignore_ascii_case("INBOX"))
        .context("IMAP server did not list INBOX for mailbox hierarchy discovery")?;
    let delimiter = inbox.delimiter();
    let leaf = if delimiter == Some(".") {
        "state"
    } else {
        ".state"
    };
    let folders = if let Some(separator) = delimiter {
        vec![
            (
                "MunkiMail".to_owned(),
                format!("MunkiMail{separator}{leaf}"),
            ),
            (
                format!("INBOX{separator}MunkiMail"),
                format!("INBOX{separator}MunkiMail{separator}{leaf}"),
            ),
        ]
    } else {
        vec![(String::new(), "MunkiMail-state".to_owned())]
    };
    for (parent, folder) in folders
        .into_iter()
        .filter(|(_, folder)| folder != STATE_FOLDER)
    {
        match ensure_folder(session, &parent, &folder).await {
            Ok(()) => return Ok(folder),
            Err(error @ async_imap::error::Error::No(_)) => last_error = error,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow::Error::from(last_error)
        .context("opening triage settings folder in the server namespace"))
}

async fn ensure_folder<S: Stream>(
    session: &mut Session<S>,
    parent: &str,
    folder: &str,
) -> std::result::Result<(), async_imap::error::Error> {
    match session.select(folder).await {
        Ok(_) => return Ok(()),
        Err(async_imap::error::Error::No(_)) => (),
        Err(error) => return Err(error),
    }
    if !parent.is_empty() {
        match session.create(parent).await {
            Ok(()) | Err(async_imap::error::Error::No(_)) => (),
            Err(error) => return Err(error),
        }
    }
    match session.create(folder).await {
        Ok(()) | Err(async_imap::error::Error::No(_)) => (),
        Err(error) => return Err(error),
    }
    session.select(folder).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        time::Duration,
    };

    #[tokio::test]
    async fn inbox_namespace_settings_roundtrip_does_not_block_mail() {
        for separator in [".", "/"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let leaf = if separator == "." { "state" } else { ".state" };
            let expected = format!("INBOX{separator}MunkiMail{separator}{leaf}");
            let expected_server = expected.clone();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                socket.write_all(b"* OK namespace test\r\n").unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut folders = HashSet::from(["INBOX".to_owned()]);
                let mut selected = String::new();
                let mut snapshots: Vec<Vec<u8>> = Vec::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    let (tag, command) = line.trim_end().split_once(' ').unwrap();
                    let mut ok = true;
                    if command.starts_with("LOGIN ") {
                    } else if command == "CAPABILITY" {
                        socket.write_all(b"* CAPABILITY IMAP4rev1\r\n").unwrap();
                    } else if command.starts_with("LIST ") {
                        write!(socket, "* LIST () \"{separator}\" \"INBOX\"\r\n").unwrap();
                    } else if let Some(name) = command.strip_prefix("SELECT ") {
                        let name = name.trim_matches('"');
                        if folders.contains(name) {
                            selected = name.into();
                            write!(socket,"* {} EXISTS\r\n* OK [UIDVALIDITY 1] epoch\r\n* OK [UIDNEXT 2] next\r\n",if name=="INBOX"{1}else{snapshots.len()}).unwrap();
                        } else {
                            ok = false;
                        }
                    } else if let Some(name) = command.strip_prefix("CREATE ") {
                        let name = name.trim_matches('"');
                        if name.starts_with(&format!("INBOX{separator}")) {
                            folders.insert(name.into());
                        } else {
                            ok = false;
                        }
                    } else if command.starts_with("APPEND ") {
                        assert!(
                            command.starts_with(&format!("APPEND \"{expected_server}\" ")),
                            "{command}"
                        );
                        let size: usize = command
                            .rsplit_once('{')
                            .unwrap()
                            .1
                            .trim_end_matches('}')
                            .parse()
                            .unwrap();
                        socket.write_all(b"+ continue\r\n").unwrap();
                        let mut raw = vec![0; size];
                        reader.read_exact(&mut raw).unwrap();
                        let mut end = [0; 2];
                        reader.read_exact(&mut end).unwrap();
                        assert_eq!(&end, b"\r\n");
                        snapshots.push(raw);
                    } else if command == "UID SEARCH ALL" {
                        socket.write_all(b"* SEARCH 1\r\n").unwrap();
                    } else if command.starts_with("UID FETCH ") {
                        if selected == "INBOX" {
                            if command.contains("BODY.PEEK") {
                                let raw=b"From: person@example.test\r\nMessage-ID: <namespace@example.test>\r\nSubject: Namespace mail\r\n\r\nHello\r\n";
                                write!(
                                    socket,
                                    "* 1 FETCH (UID 1 FLAGS () BODY[] {{{}}}\r\n",
                                    raw.len()
                                )
                                .unwrap();
                                socket.write_all(raw).unwrap();
                                socket.write_all(b")\r\n").unwrap();
                            } else {
                                socket.write_all(b"* 1 FETCH (UID 1 FLAGS ())\r\n").unwrap();
                            }
                        } else {
                            for (i, raw) in snapshots.iter().enumerate() {
                                write!(
                                    socket,
                                    "* {} FETCH (UID {} BODY[] {{{}}}\r\n",
                                    i + 1,
                                    i + 1,
                                    raw.len()
                                )
                                .unwrap();
                                socket.write_all(raw).unwrap();
                                socket.write_all(b")\r\n").unwrap();
                            }
                        }
                    } else {
                        panic!("unexpected {command}");
                    }
                    if ok {
                        write!(socket, "{tag} OK completed\r\n").unwrap();
                    } else {
                        write!(socket,"{tag} NO Client tried to access nonexistent namespace; use INBOX{separator}\r\n").unwrap();
                    }
                }
                assert_eq!(snapshots.len(), 1, "subsequent sync must reuse saved state");
            });
            let client = crate::imap::connect_insecure("127.0.0.1", port)
                .await
                .unwrap();
            let mut session = crate::imap::login(
                client,
                crate::imap::Auth::Password {
                    user: "test",
                    pass: "test",
                },
            )
            .await
            .unwrap();
            let mut store = Store::open_in_memory().unwrap();
            let account = store.add_account("test@example.test", "", "imap").unwrap();
            store
                .decide_sender(
                    account,
                    &munki_core::Address::new("person@example.test"),
                    munki_core::SenderVerdict::Allow(munki_core::Destination::Imbox),
                )
                .unwrap();
            assert!(sync(&mut session, &mut store, account).await.unwrap());
            let report = crate::imap::sync_folder(&mut session, &mut store, account, "INBOX")
                .await
                .unwrap();
            assert_eq!(report.fetched, 1);
            let messages = store
                .search(account, &munki_core::query::parse("Namespace"), 10)
                .unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].destination, munki_core::Destination::Imbox);
            drop(session);
            server.join().unwrap();
        }
    }
}
