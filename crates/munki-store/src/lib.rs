//! The local store: SQLite is the source of truth, the server is a replica.
//!
//! Everything here is synchronous. SQLite is fast enough that async adds
//! only complexity; callers run the store on a blocking task.

mod migrate;
pub mod outbox;
pub mod search;
pub mod sync;

pub use sync::Cursor;

use anyhow::{Context, Result};
use munki_core::{AccountId, Address, Destination, Message, MessageId, SenderVerdict};
use rusqlite::{params, Connection, OptionalExtension};

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Special views are local overlays. Preserve local state, retire its server writes.
    pub fn enable_local_views(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO store_preferences(key,value) VALUES('local_views',1) ON CONFLICT(key) DO UPDATE SET value=1",[])?;
        tx.execute("DELETE FROM outbox WHERE json_extract(kind,'$.kind') IN ('set_destination','set_stack','put_state','move')",[])?;
        tx.commit()?;
        Ok(())
    }
    pub fn local_views(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT coalesce((SELECT value FROM store_preferences WHERE key='local_views'),0)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Open or create the store at `path`.
    ///
    /// `key` is the SQLCipher passphrase, used only when built with the
    /// `encryption` feature; it is ignored otherwise so the dev loop can
    /// skip building OpenSSL. Pass the real key regardless — then
    /// turning the feature on is a build flag, not a code change.
    pub fn open(path: &std::path::Path, key: Option<&str>) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening store at {}", path.display()))?;
        Self::init(conn, key)
    }

    /// In-memory store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, key: Option<&str>) -> Result<Self> {
        #[cfg(feature = "encryption")]
        if let Some(key) = key {
            // Must be the first statement on the connection, before any
            // page is read, or SQLCipher will not apply it.
            conn.pragma_update(None, "key", key)
                .context("applying SQLCipher key")?;
        }
        #[cfg(not(feature = "encryption"))]
        let _ = key;

        // WAL: the sync loop writes while the UI reads. Without it every
        // background fetch would block the message list.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;

        let mut store = Store { conn };
        migrate::run(&mut store.conn)?;
        Ok(store)
    }

    /// Escape hatch for modules in this crate and for tests.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn add_account(&self, email: &str, display: &str, kind: &str) -> Result<AccountId> {
        self.conn.execute(
            "INSERT INTO accounts (email, display, kind, created_at)
             VALUES (?1, ?2, ?3, unixepoch())",
            params![email, display, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert a message and index its text in one transaction.
    ///
    /// Re-inserting the same `(account, folder, uid)` updates it rather
    /// than erroring: a resync after a dropped connection re-reports
    /// messages it already sent, and that must be a no-op, not a failure.
    pub fn upsert_message(
        &mut self,
        msg: &NewMessage<'_>,
        body: &str,
        attachment_names: &str,
    ) -> Result<MessageId> {
        let tx = self.conn.transaction()?;

        let existing: Option<MessageId> = tx
            .query_row(
                "SELECT id FROM messages WHERE account = ?1 AND folder = ?2 AND uid = ?3",
                params![msg.account, msg.folder, msg.uid],
                |r| r.get(0),
            )
            .optional()?;

        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE messages SET message_id = ?2, thread_id = ?3, from_addr = ?4,
                        subject = ?5, original_thread_id = ?3, date = ?6, seen = ?7, has_attachment = ?8,
                        destination = ?9
                     WHERE id = ?1",
                    params![
                        id,
                        msg.message_id,
                        msg.thread_id,
                        msg.from.as_str(),
                        msg.subject,
                        msg.date,
                        msg.seen,
                        msg.has_attachment,
                        dest_str(msg.destination),
                    ],
                )?;
                // External-content FTS: delete by docid, then reinsert.
                tx.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![id])?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO messages (account, folder, uid, message_id, thread_id,
                        from_addr, subject, date, seen, has_attachment, destination, original_thread_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?5)",
                    params![
                        msg.account,
                        msg.folder,
                        msg.uid,
                        msg.message_id,
                        msg.thread_id,
                        msg.from.as_str(),
                        msg.subject,
                        msg.date,
                        msg.seen,
                        msg.has_attachment,
                        dest_str(msg.destination),
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };

        tx.execute(
            "INSERT INTO messages_fts (rowid, subject, from_addr, body, attachment_names)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, msg.subject, msg.from.as_str(), body, attachment_names],
        )?;

        state::restore_message(&tx, msg.account, msg.message_id)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn cache_message(&self, id: MessageId, raw: &[u8]) -> Result<()> {
        self.conn.execute("INSERT INTO message_content(message,raw) VALUES(?1,?2) ON CONFLICT(message) DO UPDATE SET raw=?2",params![id,raw])?;
        Ok(())
    }

    pub fn raw_message(&self, account: AccountId, id: MessageId) -> Result<Option<Vec<u8>>> {
        Ok(self.conn.query_row("SELECT c.raw FROM message_content c JOIN messages m ON m.id=c.message WHERE m.account=?1 AND m.id=?2",params![account,id],|r|r.get(0)).optional()?)
    }

    pub fn message(&self, account: AccountId, id: MessageId) -> Result<Message> {
        Ok(self.conn.query_row("SELECT id,account,folder,uid,message_id,thread_id,from_addr,subject,renamed_subject,date,seen,has_attachment,destination FROM messages WHERE account=?1 AND id=?2",params![account,id],row_to_message)?)
    }

    /// Messages in a view, newest first.
    ///
    /// Dismissed senders are excluded here and *only* here: they stay in
    /// `messages` and in the FTS index, so search still finds them. That
    /// is the difference between screening and deleting.
    pub fn view(
        &self,
        account: AccountId,
        destination: Destination,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.account, m.folder, m.uid, m.message_id, m.thread_id,
                    m.from_addr, m.subject, m.renamed_subject, m.date, m.seen,
                    m.has_attachment, m.destination
             FROM messages m
             LEFT JOIN senders s ON s.account = m.account AND s.address = m.from_addr
             WHERE m.account = ?1 AND m.destination = ?2
               AND (s.verdict IS NULL OR s.verdict <> 'dismiss')
             ORDER BY m.date DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![account, dest_str(destination), limit],
                row_to_message,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Record a Screener decision and apply it to the sender's existing mail.
    ///
    /// Retroactive by design: approving a sender should pull their past
    /// messages into the right view, not just future ones. Returns the
    /// number of messages reclassified.
    pub fn decide_sender(
        &mut self,
        account: AccountId,
        address: &Address,
        verdict: SenderVerdict,
    ) -> Result<usize> {
        self.decide_sender_synced(account, address, verdict)
    }

    /// The standing decision for a sender, if any.
    pub fn sender_verdict(
        &self,
        account: AccountId,
        address: &Address,
    ) -> Result<Option<SenderVerdict>> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT verdict, destination FROM senders WHERE account = ?1 AND address = ?2",
                params![account, address.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            None => None,
            Some((v, _)) if v == "dismiss" => Some(SenderVerdict::Dismiss),
            Some((_, Some(d))) => Some(SenderVerdict::Allow(parse_dest(&d)?)),
            // verdict='allow' with no destination is corrupt; treat as
            // undecided so the message goes back through the Screener
            // rather than silently landing somewhere arbitrary.
            Some((_, None)) => None,
        })
    }

    /// Set the user's replacement subject, or clear it with `None`.
    pub fn rename_subject(&mut self, id: MessageId, subject: Option<&str>) -> Result<()> {
        self.rename_synced(id, subject)
    }
}

/// A message to insert. Borrowed, so callers can hand over `mail-parser`
/// output without copying it first.
#[derive(Debug, Clone)]
pub struct NewMessage<'a> {
    pub account: AccountId,
    pub folder: &'a str,
    pub uid: u32,
    pub message_id: &'a str,
    pub thread_id: &'a str,
    pub from: Address,
    pub subject: &'a str,
    pub date: i64,
    pub seen: bool,
    pub has_attachment: bool,
    pub destination: Destination,
}

fn dest_str(d: Destination) -> &'static str {
    match d {
        Destination::Imbox => "imbox",
        Destination::Feed => "feed",
        Destination::PaperTrail => "paper_trail",
        Destination::Screener => "screener",
    }
}

fn parse_dest(s: &str) -> Result<Destination> {
    Ok(match s {
        "imbox" => Destination::Imbox,
        "feed" => Destination::Feed,
        "paper_trail" => Destination::PaperTrail,
        "screener" => Destination::Screener,
        other => anyhow::bail!("unknown destination in store: {other:?}"),
    })
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let dest: String = r.get(12)?;
    Ok(Message {
        id: r.get(0)?,
        account: r.get(1)?,
        folder: r.get(2)?,
        uid: r.get(3)?,
        message_id: r.get(4)?,
        thread_id: r.get(5)?,
        from: Address::new(&r.get::<_, String>(6)?),
        subject: r.get(7)?,
        renamed_subject: r.get(8)?,
        date: r.get(9)?,
        seen: r.get(10)?,
        has_attachment: r.get(11)?,
        destination: parse_dest(&dest).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, e.into())
        })?,
    })
}

pub mod compose;

pub mod state;

pub mod reconcile;
