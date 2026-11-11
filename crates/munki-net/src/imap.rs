//! IMAP: connect, pull new mail into the store, push queued actions.
//!
//! Generic over the stream so production uses TLS and tests use a plain
//! socket against a local server. Nothing here talks to a real mailbox
//! in CI.

use std::fmt::Debug;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_imap::types::Flag;
use async_imap::{Client, Session};
use futures::TryStreamExt;
use munki_core::{triage, triage::Stack, AccountId, Destination, OpKind};
use munki_store::{reconcile::ServerFlags, Cursor, NewMessage, Store};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::parse;

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Debug + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Debug + Send> Stream for T {}

pub enum Auth<'a> {
    OAuthBearer {
        user: &'a str,
        access_token: &'a str,
        host: &'a str,
        port: u16,
    },
    Password {
        user: &'a str,
        pass: &'a str,
    },
    /// SASL XOAUTH2, for Gmail and Microsoft. Token acquisition (the
    /// browser loopback flow) lives in the app shell, not here.
    OAuth2 {
        user: &'a str,
        access_token: &'a str,
    },
}

/// Connect with implicit TLS (port 993), verified against the Mozilla
/// root set. There is no "accept invalid certs" switch, on purpose.
pub async fn connect_tls(host: &str, port: u16) -> Result<Client<impl Stream>> {
    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connecting to {host}:{port}"))?;
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = ServerName::try_from(host.to_string())?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await.with_context(||format!("IMAP TLS handshake with {host}:{port} failed. IMAP requires implicit TLS (normally port 993); port 143 requires STARTTLS, which is not supported yet"))?;
    greet(Client::new(tls)).await
}

/// Plaintext. Test servers only; credentials cross the wire in clear.
pub async fn connect_insecure(host: &str, port: u16) -> Result<Client<impl Stream>> {
    let tcp = TcpStream::connect((host, port)).await?;
    greet(Client::new(tcp)).await
}

async fn greet<S: Stream>(mut c: Client<S>) -> Result<Client<S>> {
    c.read_response()
        .await?
        .ok_or_else(|| anyhow!("server closed before greeting"))?;
    Ok(c)
}

pub fn oauth_bearer(user: &str, token: &str, host: &str, port: u16) -> Result<String> {
    anyhow::ensure!(
        ![user, token, host]
            .iter()
            .any(|v| v.chars().any(char::is_control)),
        "Invalid OAuth SASL value"
    );
    let user = user.replace('=', "=3D").replace(',', "=2C");
    Ok(format!(
        "n,a={user},\x01host={host}\x01port={port}\x01auth=Bearer {token}\x01\x01"
    ))
}
pub async fn login<S: Stream>(c: Client<S>, auth: Auth<'_>) -> Result<Session<S>> {
    let (mechanism, blob) = match auth {
        Auth::Password { user, pass } => {
            return c.login(user, pass).await.map_err(|(e, _)| e.into())
        }
        Auth::OAuth2 { user, access_token } => {
            anyhow::ensure!(
                ![user, access_token]
                    .iter()
                    .any(|v| v.chars().any(char::is_control)),
                "Invalid OAuth SASL value"
            );
            (
                "XOAUTH2",
                format!("user={user}\x01auth=Bearer {access_token}\x01\x01"),
            )
        }
        Auth::OAuthBearer {
            user,
            access_token,
            host,
            port,
        } => ("OAUTHBEARER", oauth_bearer(user, access_token, host, port)?),
    };
    struct Response {
        first: Option<String>,
        failure: String,
    }
    impl async_imap::Authenticator for Response {
        type Response = String;
        fn process(&mut self, _: &[u8]) -> String {
            self.first.take().unwrap_or_else(|| self.failure.clone())
        }
    }
    c.authenticate(
        mechanism,
        Response {
            first: Some(blob),
            failure: if mechanism == "OAUTHBEARER" {
                "\x01".into()
            } else {
                String::new()
            },
        },
    )
    .await
    .map_err(|(e, _)| e.into())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub fetched: usize,
    /// UIDVALIDITY changed and the folder was rebuilt from scratch.
    pub reset: bool,
}

/// Pull new mail, reconcile flags and expunges, preserving pending local edits.
/// LIST reads server structure only. Complete discovery replaces the cached tree atomically.
pub async fn discover_folders<S: Stream>(
    s: &mut Session<S>,
    store: &mut Store,
    account: AccountId,
) -> Result<()> {
    let names: Vec<_> = s.list(Some(""), Some("*")).await?.try_collect().await?;
    anyhow::ensure!(names.len() <= 20_000, "Server folder list exceeds limit");
    let tx = store.conn().unchecked_transaction()?;
    tx.execute("DELETE FROM server_folders WHERE account=?1", [account])?;
    for name in names {
        anyhow::ensure!(
            name.name().len() <= 4096
                && name
                    .delimiter()
                    .is_none_or(|d| name.name().split(d).count() <= 64),
            "Server folder hierarchy exceeds limits"
        );
        if name.name() == crate::state::STATE_FOLDER {
            continue;
        }
        let selectable = !name
            .attributes()
            .contains(&async_imap::types::NameAttribute::NoSelect);
        tx.execute(
            "INSERT INTO server_folders(account,name,delimiter,selectable,special_use) VALUES(?1,?2,?3,?4,?5)",
            (account, name.name(), name.delimiter(), selectable, if name.attributes().contains(&async_imap::types::NameAttribute::Sent){Some("sent")}else{None}),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Resolve only existing folders. Never guess a new server mailbox into existence.
pub fn sent_folder(store: &Store, account: AccountId, configured: &str) -> Result<String> {
    let mut stmt = store.conn().prepare(
        "SELECT name,delimiter,special_use FROM server_folders WHERE account=?1 AND selectable=1",
    )?;
    let folders = stmt
        .query_map([account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if folders.iter().any(|(name, _, _)| name == configured) {
        return Ok(configured.into());
    }
    let sent: Vec<_> = folders
        .iter()
        .filter(|(_, _, role)| role.as_deref() == Some("sent"))
        .collect();
    if sent.len() == 1 {
        return Ok(sent[0].0.clone());
    }
    let candidates: std::collections::HashSet<_> = folders
        .iter()
        .filter(|(name, _, _)| name.eq_ignore_ascii_case("INBOX"))
        .filter_map(|(_, delimiter, _)| delimiter.as_ref().map(|d| format!("INBOX{d}{configured}")))
        .collect();
    let found: Vec<_> = folders
        .iter()
        .filter(|(name, _, _)| candidates.contains(name))
        .collect();
    anyhow::ensure!(found.len()==1,"Sent folder '{configured}' was not found. Choose an existing server Sent folder in account settings; the message will not be sent again");
    Ok(found[0].0.clone())
}
/// Only retarget already-delivered Sent copies. Submit operations are never changed.
pub fn repair_sent_copies(store: &Store, account: AccountId) -> Result<usize> {
    let rows=store.conn().prepare("SELECT id,kind FROM outbox WHERE account=?1 AND json_extract(kind,'$.kind')='save_sent'")?.query_map([account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut repaired = 0;
    for (id, json) in rows {
        let mut kind: OpKind = serde_json::from_str(&json)?;
        if let OpKind::SaveSent { draft_id, folder } = &mut kind {
            let Ok(target) = sent_folder(store, account, folder) else {
                continue;
            };
            if target == *folder {
                continue;
            }
            *folder = target.clone();
            let draft = *draft_id;
            let tx = store.conn().unchecked_transaction()?;
            tx.execute("UPDATE outbox SET kind=?3,leased_at=NULL,not_before=0,last_error=NULL WHERE id=?1 AND account=?2",(id,account,serde_json::to_string(&kind)?))?;
            tx.execute(
                "UPDATE drafts SET sent_folder=?3 WHERE id=?1 AND account=?2",
                (draft, account, target),
            )?;
            tx.commit()?;
            repaired += 1;
        }
    }
    Ok(repaired)
}

pub async fn sync_folder<S: Stream>(
    s: &mut Session<S>,
    store: &mut Store,
    account: AccountId,
    folder: &str,
) -> Result<SyncReport> {
    anyhow::ensure!(
        folder != crate::state::STATE_FOLDER,
        "settings folder is not a mail view"
    );
    if !store.local_views()? {
        crate::state::sync(s, store, account).await?;
    }
    let condstore = s.capabilities().await?.has_str("CONDSTORE");
    let mbox = if store.local_views()? {
        s.examine(folder).await
    } else if condstore {
        s.select_condstore(folder).await
    } else {
        s.select(folder).await
    }
    .with_context(|| format!("SELECT {folder}"))?;
    anyhow::ensure!(mbox.uid_validity.is_some(), "SELECT missing UIDVALIDITY");
    let mut cur = store.cursor(account, folder)?;
    let mut report = SyncReport::default();

    if cur.uidvalidity.is_some() && cur.uidvalidity != mbox.uid_validity {
        store.reset_folder(account, folder)?;
        cur = Cursor {
            uidvalidity: mbox.uid_validity,
            last_uid: 0,
        };
        report.reset = true;
    }
    cur.uidvalidity = mbox.uid_validity;
    // Keep the new epoch even if a later FETCH fails before the batch checkpoint.
    store.set_cursor(account, folder, cur)?;

    let starting_uid = cur.last_uid;
    let previous_modseq = store.highest_modseq(account, folder)?;
    let live = s.uid_search("ALL").await?;
    let mut fetches: Vec<_> = if !live.iter().any(|uid| *uid > starting_uid) {
        Vec::new()
    } else {
        s.uid_fetch(
            format!("{}:*", starting_uid.saturating_add(1)),
            "(UID FLAGS BODY.PEEK[])",
        )
        .await?
        .try_collect()
        .await?
    };
    anyhow::ensure!(fetches.iter().all(|f| f.uid.is_some()), "FETCH missing UID");
    // Only ingest the SEARCH snapshot. Later arrivals belong to the next sync.
    fetches.retain(|f| f.uid.is_some_and(|uid| live.contains(&uid)));
    let returned: std::collections::HashSet<_> = fetches.iter().filter_map(|f| f.uid).collect();
    for uid in live.iter().filter(|uid| **uid > starting_uid) {
        anyhow::ensure!(returned.contains(uid), "FETCH omitted UID {uid}");
    }
    fetches.sort_by_key(|f| f.uid);
    let now = now();
    for f in &fetches {
        let uid = f.uid.context("FETCH missing UID")?;
        // The `*` quirk again, for servers that omit UIDNEXT.
        if uid <= starting_uid {
            continue;
        }
        // A previous interrupted batch may have cached this UID already. Never upsert
        // stale flags over an intervening UI action; reconciliation handles them below.
        let raw = f
            .body()
            .with_context(|| format!("FETCH missing body for UID {uid}"))?;
        if let Some(id) = store.folder_message_id(account, folder, uid)? {
            store.cache_message(id, raw)?;
            cur.last_uid = cur.last_uid.max(uid);
            continue;
        }
        let fallback_id = format!(
            "<{folder}.{uid}.{}@munkimail.local>",
            cur.uidvalidity.unwrap_or(0)
        );
        let p = parse::parse(raw, &fallback_id, now)
            .with_context(|| format!("invalid message at UID {uid}"))?;

        let mut seen = false;
        let mut keyword = None;
        let (mut reply_later, mut set_aside) = (false, false);
        for flag in f.flags() {
            match flag {
                Flag::Seen => seen = true,
                Flag::Custom(k) => {
                    keyword = keyword.or(Destination::from_keyword(&k));
                    reply_later |= k == Stack::ReplyLater.keyword();
                    set_aside |= k == Stack::SetAside.keyword();
                }
                _ => {}
            }
        }

        let suggestion = triage::suggest(p.headers());
        let destination = store.route(
            account,
            &p.from,
            if store.local_views()? { None } else { keyword },
        )?;
        let id = store.upsert_message(
            &NewMessage {
                account,
                folder,
                uid,
                message_id: &p.message_id,
                thread_id: &p.thread_id,
                from: p.from.clone(),
                subject: &p.subject,
                date: p.date,
                seen,
                has_attachment: !p.attachment_names.is_empty(),
                destination,
            },
            &p.text,
            &p.attachment_names.join(" "),
        )?;
        if destination == Destination::Screener {
            store.set_suggestion(id, &suggestion)?;
        }
        store.cache_message(id, raw)?;
        if !store.local_views()? {
            store.server_stacks(account, id, reply_later, set_aside)?;
        }
        cur.last_uid = cur.last_uid.max(uid);
        report.fetched += 1;
    }

    let query = match (mbox.highest_modseq, previous_modseq) {
        (Some(_), Some(seq)) if condstore => format!("(UID FLAGS) (CHANGEDSINCE {seq})"),
        _ => "(UID FLAGS)".to_owned(),
    };
    let updates: Vec<_> = if live.is_empty() {
        Vec::new()
    } else {
        s.uid_fetch("1:*", query).await?.try_collect().await?
    };
    let mut flags = Vec::new();
    for f in updates {
        let uid = f.uid.context("flag FETCH missing UID")?;
        let mut server = ServerFlags {
            uid,
            seen: false,
            destination: None,
            reply_later: false,
            set_aside: false,
        };
        for flag in f.flags() {
            match flag {
                Flag::Seen => server.seen = true,
                Flag::Custom(k) => {
                    server.destination = server.destination.or(Destination::from_keyword(&k));
                    server.reply_later |= k == Stack::ReplyLater.keyword();
                    server.set_aside |= k == Stack::SetAside.keyword();
                }
                _ => (),
            }
        }
        if store.local_views()? {
            server.destination = None;
        }
        flags.push(server);
    }
    store.set_cursor(account, folder, cur)?;
    store.reconcile_folder(account, folder, &live, &flags, mbox.highest_modseq)?;
    Ok(report)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub applied: usize,
    pub retried: usize,
    pub parked: usize,
}

/// Replay queued actions against the server until the queue is empty or
/// every remaining op is backing off.
pub async fn drain_outbox<S: Stream>(
    s: &mut Session<S>,
    store: &mut Store,
    account: AccountId,
) -> Result<DrainReport> {
    // Merge before leasing: unsupported settings block stale triage writes,
    // and a retry following a dropped connection sees the latest roster.
    if !store.local_views()? {
        crate::state::sync(s, store, account).await?;
    }
    store.recover_leases(account)?;
    let mut r = DrainReport::default();
    while let Some(mut op) = store.lease_transport(account, false)? {
        if let Some(folder) = uid_folder(&op.kind) {
            let result = match s.select(folder).await {
                Ok(mailbox) => {
                    let expected = store.cursor(account, folder)?.uidvalidity;
                    if expected.is_none() || expected != mailbox.uid_validity {
                        Err(Failure::Permanent(
                            "UIDVALIDITY mismatch or unknown; old UID requires review".into(),
                        ))
                    } else {
                        Ok(())
                    }
                }
                Err(error) => Err(Failure::from(error)),
            };
            match result {
                Ok(()) => (),
                Err(Failure::Permanent(error)) => {
                    store.settle_needs_review(op.id, &error)?;
                    r.parked += 1;
                    continue;
                }
                Err(Failure::Transient(error)) => {
                    store.settle_retry(op.id, &error)?;
                    r.retried += 1;
                    break;
                }
            }
        }
        // State imported by an earlier PutState can supersede queued keywords.
        match &mut op.kind {
            OpKind::SetDestination {
                folder,
                uid,
                destination,
            } => {
                let Some(current) = store.queued_destination(account, folder, *uid)? else {
                    store.settle_ok(op.id)?;
                    continue;
                };
                *destination = current;
            }
            OpKind::SetStack {
                folder,
                uid,
                stack,
                enabled,
            } => {
                let Some(current) = store.queued_stack(account, folder, *uid, *stack)? else {
                    store.settle_ok(op.id)?;
                    continue;
                };
                *enabled = current;
            }
            _ => (),
        }
        let result = if let OpKind::SaveSent { draft_id, folder } = &op.kind {
            let (message_id, wire) = store.draft_wire(account, *draft_id)?;
            let target = if store.local_views()? {
                sent_folder(store, account, folder).map_err(|e| Failure::Permanent(e.to_string()))
            } else {
                Ok(folder.clone())
            };
            match target {
                Ok(target) => {
                    save_sent(s, &target, &message_id, &wire, !store.local_views()?).await
                }
                Err(error) => Err(error),
            }
        } else if matches!(&op.kind, OpKind::PutState { .. }) {
            // The startup merge already published the current complete snapshot.
            Ok(())
        } else {
            apply(
                s,
                &op.kind,
                store
                    .cursor(account, uid_folder(&op.kind).unwrap_or(""))?
                    .uidvalidity,
            )
            .await
        };
        match result {
            Ok(()) => {
                if let OpKind::SaveSent { draft_id, .. } = op.kind {
                    store.saved_sent(account, op.id, draft_id)?;
                } else {
                    store.settle_ok(op.id)?;
                }
                r.applied += 1;
            }
            Err(Failure::Permanent(e)) => {
                store.settle_needs_review(op.id, &e)?;
                r.parked += 1;
            }
            Err(Failure::Transient(e)) => {
                store.settle_retry(op.id, &e)?;
                r.retried += 1;
                // The connection is probably bad; stop rather than burn
                // every op's retry budget on the same dead socket.
                break;
            }
        }
    }
    Ok(r)
}

enum Failure {
    /// Server said NO/BAD: retrying the same command will not help.
    Permanent(String),
    /// Network or protocol trouble: worth another attempt later.
    Transient(String),
}

impl From<async_imap::error::Error> for Failure {
    fn from(e: async_imap::error::Error) -> Self {
        use async_imap::error::Error as E;
        match e {
            E::No(m) | E::Bad(m) => Failure::Permanent(m),
            other => Failure::Transient(other.to_string()),
        }
    }
}

async fn apply<S: Stream>(
    s: &mut Session<S>,
    op: &OpKind,
    expected_validity: Option<u32>,
) -> Result<(), Failure> {
    match op {
        OpKind::SetSeen { folder, uid, seen } => {
            let mailbox = s.select(folder).await?;
            if mailbox.uid_validity != expected_validity {
                return Err(Failure::Permanent(
                    "UIDVALIDITY changed during replay".into(),
                ));
            }

            let q = if *seen {
                "+FLAGS.SILENT (\\Seen)"
            } else {
                "-FLAGS.SILENT (\\Seen)"
            };
            drain(s.uid_store(uid.to_string(), q).await?).await
        }
        OpKind::SetStack {
            folder,
            uid,
            stack,
            enabled,
        } => {
            let mailbox = s.select(folder).await?;
            // Recheck after SELECT: the mailbox may have been recreated since the guard.
            if mailbox.uid_validity != expected_validity {
                return Err(Failure::Permanent(
                    "UIDVALIDITY changed during replay".into(),
                ));
            }
            if !supports_keyword(&mailbox, stack.keyword()) {
                return Ok(());
            }
            let sign = if *enabled { "+" } else { "-" };
            drain(
                s.uid_store(
                    uid.to_string(),
                    format!("{sign}FLAGS.SILENT ({})", stack.keyword()),
                )
                .await?,
            )
            .await
        }
        OpKind::SetDestination {
            folder,
            uid,
            destination,
        } => {
            let mailbox = s.select(folder).await?;
            // Recheck after SELECT: the mailbox may have been recreated since the guard.
            if mailbox.uid_validity != expected_validity {
                return Err(Failure::Permanent(
                    "UIDVALIDITY changed during replay".into(),
                ));
            }
            if !supports_keyword(&mailbox, destination.keyword()) {
                return Ok(());
            }
            // Remove the others first so a message never carries two
            // verdicts after a partial failure.
            let others: Vec<_> = [
                Destination::Imbox,
                Destination::Feed,
                Destination::PaperTrail,
                Destination::Screener,
            ]
            .into_iter()
            .filter(|d| d != destination)
            .map(|d| d.keyword())
            .collect();
            drain(
                s.uid_store(
                    uid.to_string(),
                    format!("-FLAGS.SILENT ({})", others.join(" ")),
                )
                .await?,
            )
            .await?;
            drain(
                s.uid_store(
                    uid.to_string(),
                    format!("+FLAGS.SILENT ({})", destination.keyword()),
                )
                .await?,
            )
            .await
        }
        // State and Sent copies are handled by the store-aware drainer.
        // Legacy file-based operations remain parked for review.
        OpKind::Submit { .. }
        | OpKind::SaveSent { .. }
        | OpKind::Move { .. }
        | OpKind::Append { .. }
        | OpKind::Send { .. }
        | OpKind::PutState { .. } => {
            Err(Failure::Permanent("not supported by this build yet".into()))
        }
    }
}

async fn drain<St>(st: St) -> Result<(), Failure>
where
    St: futures::Stream<Item = async_imap::error::Result<async_imap::types::Fetch>>,
{
    futures::pin_mut!(st);
    while st.try_next().await?.is_some() {}
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Ensure the configured Sent folder exists; deduplicate ambiguous APPEND retries.
async fn save_sent<S: Stream>(
    session: &mut Session<S>,
    folder: &str,
    message_id: &str,
    wire: &[u8],
    allow_create: bool,
) -> Result<(), Failure> {
    // Quote defensively before interpolating a SEARCH string.
    if message_id.chars().any(char::is_control) {
        return Err(Failure::Permanent("invalid Message-ID".into()));
    }
    let quoted = message_id.replace('\\', "\\\\").replace('"', "\\\"");
    match session.select(folder).await {
        Ok(_) => (),
        Err(async_imap::error::Error::No(_)) if allow_create => {
            // Another client may create it concurrently. SELECT confirms usability;
            // permission failures still prevent APPEND and park the operation.
            match session.create(folder).await {
                Ok(()) | Err(async_imap::error::Error::No(_)) => (),
                Err(error) => return Err(error.into()),
            }
            session.select(folder).await?;
        }
        Err(error) => return Err(error.into()),
    }
    if session
        .uid_search(format!("HEADER Message-ID \"<{quoted}>\""))
        .await?
        .is_empty()
    {
        session.append(folder, Some("(\\Seen)"), None, wire).await?;
    }
    Ok(())
}

fn supports_keyword(mailbox: &async_imap::types::Mailbox, keyword: &str) -> bool {
    mailbox.permanent_flags.iter().any(|flag| {
        matches!(flag, Flag::MayCreate) || matches!(flag, Flag::Custom(value) if value==keyword)
    })
}

fn uid_folder(op: &OpKind) -> Option<&str> {
    match op {
        OpKind::SetSeen { folder, .. }
        | OpKind::SetDestination { folder, .. }
        | OpKind::SetStack { folder, .. } => Some(folder),
        OpKind::Move { from_folder, .. } => Some(from_folder),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        time::Duration,
    };

    /// Real IMAP framing: reversed FETCH results, optional missing low-UID body,
    /// CONDSTORE capability, and an empty settings folder (state sync stays enabled).
    async fn scripted(
        missing_body: bool,
    ) -> (
        Session<impl Stream>,
        Arc<Mutex<Vec<String>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket.write_all(b"* OK test server\r\n").unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut settings = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                recorded.lock().unwrap().push(line.trim().to_owned());
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                if command.starts_with("LOGIN ") {
                } else if command == "CAPABILITY" {
                    socket
                        .write_all(b"* CAPABILITY IMAP4rev1 CONDSTORE\r\n")
                        .unwrap();
                } else if command.starts_with("SELECT ") {
                    settings = command.contains(".state");
                    write!(socket,"* {} EXISTS\r\n* OK [UIDVALIDITY 1] epoch\r\n* OK [UIDNEXT 3] next\r\n* OK [HIGHESTMODSEQ 9] seq\r\n",if settings {0}else{2}).unwrap();
                } else if command == "UID SEARCH ALL" {
                    socket.write_all(b"* SEARCH 1 2\r\n").unwrap();
                } else if command.starts_with("UID FETCH ") {
                    if !settings {
                        if command.contains("BODY.PEEK") {
                            for uid in [2, 1] {
                                if missing_body && uid == 1 {
                                    socket.write_all(b"* 1 FETCH (UID 1 FLAGS ())\r\n").unwrap();
                                } else {
                                    let raw=format!("From: a@test\r\nSubject: mail {uid}\r\nMessage-ID: <{uid}@test>\r\n\r\nbody {uid}\r\n");
                                    write!(socket,"* {uid} FETCH (UID {uid} FLAGS () BODY[] {{{}}}\r\n{raw})\r\n",raw.len()).unwrap();
                                }
                            }
                        } else {
                            assert!(command.contains("CHANGEDSINCE 7"), "{command}");
                            socket.write_all(b"* 1 FETCH (UID 1 FLAGS (\\Seen) MODSEQ (9))\r\n* 2 FETCH (UID 2 FLAGS () MODSEQ (8))\r\n").unwrap();
                        }
                    }
                } else {
                    panic!("unexpected command {command}");
                }
                write!(socket, "{tag} OK completed\r\n").unwrap();
            }
        });
        let session = login(
            connect_insecure("127.0.0.1", port).await.unwrap(),
            Auth::Password {
                user: "test",
                pass: "test",
            },
        )
        .await
        .unwrap();
        (session, commands, thread)
    }

    #[tokio::test]
    async fn condstore_changed_since_and_unordered_fetch_ingest_every_uid() {
        let (mut session, commands, thread) = scripted(false).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("test@test", "", "imap").unwrap();
        store
            .set_cursor(
                account,
                "INBOX",
                Cursor {
                    uidvalidity: Some(1),
                    last_uid: 0,
                },
            )
            .unwrap();
        store
            .conn()
            .execute("UPDATE folders SET highestmodseq=7", [])
            .unwrap();
        let report = sync_folder(&mut session, &mut store, account, "INBOX")
            .await
            .unwrap();
        assert_eq!(report.fetched, 2);
        assert_eq!(store.cursor(account, "INBOX").unwrap().last_uid, 2);
        assert_eq!(store.highest_modseq(account, "INBOX").unwrap(), Some(9));
        let id = store
            .folder_message_id(account, "INBOX", 1)
            .unwrap()
            .unwrap();
        assert!(store.message(account, id).unwrap().seen);
        assert!(commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("SELECT") && c.contains("INBOX") && c.contains("(CONDSTORE)")));
        drop(session);
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn missing_low_uid_body_does_not_advance_past_it() {
        let (mut session, _, thread) = scripted(true).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("test@test", "", "imap").unwrap();
        assert!(sync_folder(&mut session, &mut store, account, "INBOX")
            .await
            .is_err());
        assert_eq!(store.cursor(account, "INBOX").unwrap().last_uid, 0);
        assert!(store
            .folder_message_id(account, "INBOX", 2)
            .unwrap()
            .is_none());
        drop(session);
        thread.join().unwrap();
    }
}
