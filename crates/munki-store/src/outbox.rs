//! The pending-action queue: enqueue, lease, settle.
//!
//! Lifecycle of a row:
//!
//! ```text
//!   enqueue -> ready -> [lease] -> in flight -> [ok]   -> gone
//!                ^                            \ [fail] -> ready (backoff)
//!                |                             \ [give_up] -> needs_review
//!                +-------------------------------- (crash) -+
//! ```
//!
//! A crash while in flight leaves `leased_at` set. Recovery differs by
//! op: a flag store can be replayed blindly, an SMTP send cannot, because
//! the message may have left. `recover_leases` therefore requeues the
//! idempotent ops and parks `Send` for the user to confirm. Silently
//! replaying a send would double-mail someone; silently dropping it would
//! lose their mail. Asking is the only honest option.

use anyhow::Result;
use munki_core::{outbox::backoff_secs, AccountId, Op, OpKind};
use rusqlite::{params, OptionalExtension};

use crate::Store;

/// How long a lease may be held before `recover_leases` reclaims it.
/// Longer than any plausible IMAP/SMTP timeout, so a slow-but-alive
/// drainer is never undercut by a concurrent one.
pub const LEASE_TIMEOUT_SECS: i64 = 15 * 60;

impl Store {
    /// Queue an action. It is applied to the local DB by the caller;
    /// this only records the server-side work still owed.
    pub fn enqueue(&self, account: AccountId, kind: &OpKind) -> Result<i64> {
        let json = serde_json::to_string(kind)?;
        self.conn().execute(
            "INSERT INTO outbox (account, kind, created_at)
             VALUES (?1, ?2, unixepoch())",
            params![account, json],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Claim the oldest ready op, marking it in flight.
    ///
    /// `None` means nothing is due — either the queue is empty or every
    /// row is backing off.
    pub fn lease_next(&mut self, account: AccountId) -> Result<Option<Op>> {
        self.lease_matching(account, None)
    }

    /// Separate transports must never consume each other's work.
    pub fn lease_transport(&mut self, account: AccountId, smtp: bool) -> Result<Option<Op>> {
        self.lease_matching(account, Some(smtp))
    }

    fn lease_matching(&mut self, account: AccountId, smtp: Option<bool>) -> Result<Option<Op>> {
        let tx = self.conn.transaction()?;
        let row: Option<(i64, String, u32, i64, Option<String>)> = tx
            .query_row(
                "SELECT id, kind, attempts, not_before, last_error
                 FROM outbox
                 WHERE account = ?1 AND leased_at IS NULL AND not_before <= unixepoch()
                   AND (?2 IS NULL OR (json_extract(kind, '$.kind') = 'submit') = ?2)
                 ORDER BY id
                 LIMIT 1",
                params![account, smtp],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        let Some((id, json, attempts, not_before, last_error)) = row else {
            tx.commit()?;
            return Ok(None);
        };

        tx.execute(
            "UPDATE outbox SET leased_at = unixepoch() WHERE id = ?1",
            params![id],
        )?;
        tx.commit()?;

        Ok(Some(Op {
            id,
            account,
            kind: serde_json::from_str(&json)?,
            attempts,
            not_before,
            last_error,
        }))
    }

    /// The op succeeded on the server. Drop it.
    pub fn settle_ok(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The op failed. Release the lease and schedule a retry with backoff.
    pub fn settle_retry(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE outbox
             SET attempts = attempts + 1,
                 leased_at = NULL,
                 last_error = ?2,
                 not_before = unixepoch() + ?3
             WHERE id = ?1",
            // Backoff is computed from the count *after* this failure.
            params![id, error, backoff_secs(self.attempts(id)? + 1)],
        )?;
        Ok(())
    }

    /// The op cannot proceed without a human. Park it: keep the lease set
    /// so no drainer picks it up, and surface it in `needs_review`.
    pub fn settle_needs_review(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE outbox
             SET last_error = ?2, not_before = ?3, leased_at = unixepoch()
             WHERE id = ?1",
            params![id, error, i64::MAX],
        )?;
        Ok(())
    }

    /// Reclaim ops whose drainer died mid-flight.
    ///
    /// Idempotent ops are requeued immediately. `Send` is parked for
    /// review, because we cannot tell whether the mail went out.
    /// Returns `(requeued, parked)`.
    pub fn recover_leases(&mut self, account: AccountId) -> Result<(usize, usize)> {
        let tx = self.conn.transaction()?;
        let stale: Vec<(i64, String)> = tx
            .prepare(
                "SELECT id, kind FROM outbox
                 WHERE account = ?1
                   AND leased_at IS NOT NULL
                   AND not_before <> ?2
                   AND leased_at < unixepoch() - ?3",
            )?
            .query_map(params![account, i64::MAX, LEASE_TIMEOUT_SECS], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let (mut requeued, mut parked) = (0, 0);
        for (id, json) in stale {
            let kind: OpKind = serde_json::from_str(&json)?;
            match kind {
                OpKind::Send { .. } | OpKind::Submit { .. } => {
                    tx.execute(
                        "UPDATE outbox SET not_before = ?2, leased_at = unixepoch(),
                            last_error = ?3 WHERE id = ?1",
                        params![
                            id,
                            i64::MAX,
                            "interrupted while sending; confirm whether this message went out"
                        ],
                    )?;
                    parked += 1;
                }
                _ => {
                    tx.execute(
                        "UPDATE outbox SET leased_at = NULL, not_before = 0 WHERE id = ?1",
                        params![id],
                    )?;
                    requeued += 1;
                }
            }
        }
        tx.commit()?;
        Ok((requeued, parked))
    }

    /// Ops parked for the user to resolve.
    pub fn needs_review(&self, account: AccountId) -> Result<Vec<Op>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, kind, attempts, not_before, last_error
             FROM outbox WHERE account = ?1 AND not_before = ?2 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![account, i64::MAX], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        rows.into_iter()
            .map(|(id, json, attempts, not_before, last_error)| {
                Ok(Op {
                    id,
                    account,
                    kind: serde_json::from_str(&json)?,
                    attempts,
                    not_before,
                    last_error,
                })
            })
            .collect()
    }

    /// Count of ops still owed to the server, for the UI's sync indicator.
    pub fn pending_count(&self, account: AccountId) -> Result<u32> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM outbox WHERE account = ?1",
            params![account],
            |r| r.get(0),
        )?)
    }

    fn attempts(&self, id: i64) -> Result<u32> {
        Ok(self.conn().query_row(
            "SELECT attempts FROM outbox WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?)
    }
}
