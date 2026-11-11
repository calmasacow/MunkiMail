//! Store operations the sync loop needs: folder cursors, routing, and
//! local application of user actions.

use anyhow::Result;
use munki_core::triage::Suggestion;
use munki_core::{AccountId, Address, Destination, MessageId, OpKind, SenderVerdict};
use rusqlite::{params, OptionalExtension};

use crate::{dest_str, parse_dest, Store};

/// Per-folder sync position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub uidvalidity: Option<u32>,
    pub last_uid: u32,
}

impl Store {
    pub fn cursor(&self, account: AccountId, folder: &str) -> Result<Cursor> {
        Ok(self
            .conn()
            .query_row(
                "SELECT uidvalidity, last_uid FROM folders WHERE account = ?1 AND name = ?2",
                params![account, folder],
                |r| {
                    Ok(Cursor {
                        uidvalidity: r.get(0)?,
                        last_uid: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn set_cursor(&self, account: AccountId, folder: &str, c: Cursor) -> Result<()> {
        self.conn().execute(
            "INSERT INTO folders (account, name, uidvalidity, last_uid) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, name) DO UPDATE SET uidvalidity = ?3, last_uid = ?4",
            params![account, folder, c.uidvalidity, c.last_uid],
        )?;
        Ok(())
    }

    /// UIDVALIDITY changed: every UID we hold for this folder now names a
    /// different message, or none. Drop them and refetch.
    ///
    /// Safe because the server is the replica of record for message
    /// *content*. Triage verdicts are not lost: they live on the sender
    /// (`senders` table) and in server keywords, and both re-apply on
    /// refetch.
    pub fn reset_folder(&mut self, account: AccountId, folder: &str) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("UPDATE outbox SET last_error='UIDVALIDITY changed; old UID requires review', not_before=?3, leased_at=unixepoch()
            WHERE account=?1 AND json_extract(kind,'$.uid') IS NOT NULL
            AND coalesce(json_extract(kind,'$.folder'),json_extract(kind,'$.from_folder'))=?2", params![account,folder,i64::MAX])?;
        tx.execute(
            "DELETE FROM messages_fts WHERE rowid IN
               (SELECT id FROM messages WHERE account = ?1 AND folder = ?2)",
            params![account, folder],
        )?;
        let n = tx.execute(
            "DELETE FROM messages WHERE account=?1 AND folder=?2",
            params![account, folder],
        )?;
        tx.execute("UPDATE folders SET uidvalidity=NULL,last_uid=0,highestmodseq=NULL WHERE account=?1 AND name=?2",params![account,folder])?;
        tx.commit()?;
        Ok(n)
    }

    /// Where a newly arrived message goes.
    ///
    /// Precedence, highest first:
    /// 1. The user's synced standing verdict on the sender.
    /// 2. A destination keyword, when no roster decision exists.
    /// 3. Otherwise the Screener, carrying the heuristic's suggestion.
    ///
    /// Dismissed senders route to the Screener too; `view` hides them.
    /// Keeping them in a real destination column means an un-dismiss
    /// drops them back exactly where approval would have put them.
    pub fn route(
        &self,
        account: AccountId,
        from: &Address,
        server_keyword: Option<Destination>,
    ) -> Result<Destination> {
        Ok(match self.sender_verdict(account, from)? {
            Some(SenderVerdict::Allow(d)) => d,
            Some(SenderVerdict::Dismiss) => Destination::Screener,
            None => server_keyword.unwrap_or(Destination::Screener),
        })
    }

    pub fn set_suggestion(&self, id: MessageId, s: &Suggestion) -> Result<()> {
        self.conn().execute(
            "UPDATE messages SET suggested = ?2, suggested_reason = ?3 WHERE id = ?1",
            params![id, dest_str(s.destination), s.reason],
        )?;
        Ok(())
    }

    pub fn suggestion(&self, id: MessageId) -> Result<Option<Suggestion>> {
        let row: Option<(Option<String>, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT suggested, suggested_reason FROM messages WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((Some(d), reason)) => Ok(Some(Suggestion {
                destination: parse_dest(&d)?,
                reason: reason.unwrap_or_default(),
            })),
            _ => Ok(None),
        }
    }

    /// Mark read/unread: write locally, queue for the server, atomically.
    ///
    /// The UI calls this and returns immediately. Offline or not, the
    /// change is visible at once and owed to the server until drained.
    pub fn mark_seen(&mut self, id: MessageId, seen: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (account, folder, uid): (AccountId, String, u32) = tx.query_row(
            "SELECT account, folder, uid FROM messages WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        tx.execute(
            "UPDATE messages SET seen = ?2 WHERE id = ?1",
            params![id, seen],
        )?;
        let op = serde_json::to_string(&OpKind::SetSeen { folder, uid, seen })?;
        tx.execute(
            "INSERT INTO outbox (account, kind, created_at) VALUES (?1, ?2, unixepoch())",
            params![account, op],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Screener decision plus the server work it implies: one
    /// `SetDestination` per message moved, so the verdict reaches every
    /// other device through keywords.
    pub fn decide_sender_synced(
        &mut self,
        account: AccountId,
        address: &Address,
        verdict: SenderVerdict,
    ) -> Result<usize> {
        self.decide_senders(account, &[(address.clone(), verdict)])
    }

    pub fn queued_destination(
        &self,
        account: AccountId,
        folder: &str,
        uid: u32,
    ) -> Result<Option<Destination>> {
        let value: Option<String> = self
            .conn()
            .query_row(
                "SELECT destination FROM messages WHERE account=?1 AND folder=?2 AND uid=?3",
                params![account, folder, uid],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse_dest(&s)).transpose()
    }

    pub fn queued_stack(
        &self,
        account: AccountId,
        folder: &str,
        uid: u32,
        stack: munki_core::triage::Stack,
    ) -> Result<Option<bool>> {
        let column = match stack {
            munki_core::triage::Stack::ReplyLater => "reply_later",
            munki_core::triage::Stack::SetAside => "set_aside",
        };
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {column} FROM messages WHERE account=?1 AND folder=?2 AND uid=?3"),
                params![account, folder, uid],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Message id by server identity.
    pub fn find(&self, account: AccountId, folder: &str, uid: u32) -> Result<Option<MessageId>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE account = ?1 AND folder = ?2 AND uid = ?3",
                params![account, folder, uid],
                |r| r.get(0),
            )
            .optional()?)
    }
}
