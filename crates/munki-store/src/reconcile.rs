//! Server reconciliation checks pending edits while holding the write lock.
use crate::{dest_str, Store};
use anyhow::Result;
use munki_core::{AccountId, Destination};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct ServerFlags {
    pub uid: u32,
    pub seen: bool,
    pub destination: Option<Destination>,
    pub reply_later: bool,
    pub set_aside: bool,
}

// Includes leased, retrying, and parked edits, except operations for a retired UID epoch.
const PENDING: &str = "SELECT 1 FROM outbox o WHERE o.account=m.account
 AND NOT (o.not_before=9223372036854775807 AND coalesce(o.last_error,'') LIKE 'UIDVALIDITY%')
 AND json_extract(o.kind,'$.uid')=m.uid
 AND coalesce(json_extract(o.kind,'$.folder'),json_extract(o.kind,'$.from_folder'))=m.folder";

impl Store {
    pub fn folder_message_id(
        &self,
        account: AccountId,
        folder: &str,
        uid: u32,
    ) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE account=?1 AND folder=?2 AND uid=?3",
                params![account, folder, uid],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn highest_modseq(&self, account: AccountId, folder: &str) -> Result<Option<u64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT highestmodseq FROM folders WHERE account=?1 AND name=?2",
                params![account, folder],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// `live` is a completed UID SEARCH ALL, not a partial fetch result.
    /// Pending fields are protected independently; deletion protects the whole row.
    pub fn reconcile_folder(
        &mut self,
        account: AccountId,
        folder: &str,
        live: &HashSet<u32>,
        flags: &[ServerFlags],
        highest_modseq: Option<u64>,
    ) -> Result<usize> {
        let local_views = self.local_views()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let update = format!("UPDATE messages AS m SET
 seen=CASE WHEN EXISTS({PENDING} AND json_extract(o.kind,'$.kind')='set_seen') THEN seen ELSE ?4 END,
 destination=CASE WHEN EXISTS({PENDING} AND json_extract(o.kind,'$.kind')='set_destination') THEN destination
 ELSE coalesce((SELECT CASE WHEN verdict='dismiss' THEN 'screener' ELSE destination END FROM senders WHERE account=m.account AND address=m.from_addr),?5,'screener') END,
 reply_later=CASE WHEN EXISTS({PENDING} AND json_extract(o.kind,'$.kind')='set_stack') OR ?8 OR EXISTS(SELECT 1 FROM triage_entries WHERE account=m.account AND kind='message' AND key=m.message_id) THEN reply_later ELSE ?6 END,
 set_aside=CASE WHEN EXISTS({PENDING} AND json_extract(o.kind,'$.kind')='set_stack') OR ?8 OR EXISTS(SELECT 1 FROM triage_entries WHERE account=m.account AND kind='message' AND key=m.message_id) THEN set_aside ELSE ?7 END
 WHERE account=?1 AND folder=?2 AND uid=?3");
        for f in flags {
            tx.execute(
                &update,
                params![
                    account,
                    folder,
                    f.uid,
                    f.seen,
                    f.destination.map(dest_str),
                    f.reply_later,
                    f.set_aside,
                    local_views
                ],
            )?;
        }
        let rows: Vec<(i64,u32)> = tx.prepare(&format!("SELECT id,uid FROM messages m WHERE account=?1 AND folder=?2 AND NOT EXISTS({PENDING})"))?
            .query_map(params![account,folder], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut deleted = 0;
        for (id, uid) in rows {
            if !live.contains(&uid) {
                tx.execute("DELETE FROM messages_fts WHERE rowid=?1", [id])?;
                deleted += tx.execute("DELETE FROM messages WHERE id=?1", [id])?;
            }
        }
        // Re-fetch skipped flags after pending writes settle, even if the server modseq is unchanged.
        let pending: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM messages m WHERE account=?1 AND folder=?2 AND EXISTS({PENDING}))"),params![account,folder],|r|r.get(0))?;
        let seq = if pending {
            None
        } else {
            highest_modseq.filter(|n| *n <= i64::MAX as u64)
        };
        tx.execute(
            "UPDATE folders SET highestmodseq=?3 WHERE account=?1 AND name=?2",
            params![account, folder, seq],
        )?;
        tx.commit()?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cursor, NewMessage};
    use munki_core::{Address, OpKind, Query};
    fn seed(store: &mut Store, account: i64, uid: u32) -> i64 {
        let id = store
            .upsert_message(
                &NewMessage {
                    account,
                    folder: "INBOX",
                    uid,
                    message_id: &format!("{uid}@test"),
                    thread_id: "thread",
                    from: Address::new("a@test"),
                    subject: "subject",
                    date: 1,
                    seen: false,
                    has_attachment: false,
                    destination: Destination::Imbox,
                },
                "reconcileneedle",
                "",
            )
            .unwrap();
        store.cache_message(id, b"raw MIME").unwrap();
        id
    }
    #[test]
    fn pending_edits_survive_stale_snapshot_and_expunges_cleanup_everything() {
        let mut st = Store::open_in_memory().unwrap();
        let a = st.add_account("a@test", "", "imap").unwrap();
        let b = st.add_account("b@test", "", "imap").unwrap();
        st.set_cursor(
            a,
            "INBOX",
            Cursor {
                uidvalidity: Some(1),
                last_uid: 3,
            },
        )
        .unwrap();
        let protected = seed(&mut st, a, 1);
        let deleted = seed(&mut st, a, 2);
        let other = seed(&mut st, b, 2);
        // Snapshot arrives before the local edit, as during a network await.
        let flags = vec![ServerFlags {
            uid: 1,
            seen: false,
            destination: Some(Destination::Feed),
            reply_later: false,
            set_aside: false,
        }];
        st.mark_seen(protected, true).unwrap();
        st.enqueue(
            a,
            &OpKind::SetDestination {
                folder: "INBOX".into(),
                uid: 1,
                destination: Destination::PaperTrail,
            },
        )
        .unwrap();
        st.conn()
            .execute(
                "UPDATE messages SET destination='paper_trail' WHERE id=?1",
                [protected],
            )
            .unwrap();
        assert_eq!(
            st.reconcile_folder(a, "INBOX", &HashSet::new(), &flags, Some(10))
                .unwrap(),
            1
        );
        let row = st.message(a, protected).unwrap();
        assert!(row.seen);
        assert_eq!(row.destination, Destination::PaperTrail);
        assert!(st.raw_message(a, deleted).unwrap().is_none());
        assert!(st.raw_message(b, other).unwrap().is_some());
        let orphan_count: i64 = st
            .conn()
            .query_row(
                "SELECT count(*) FROM message_content WHERE message=?1",
                [deleted],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0);
        assert_eq!(
            st.search(
                a,
                &Query {
                    text: vec!["reconcileneedle".into()],
                    ..Default::default()
                },
                10
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(st.highest_modseq(a, "INBOX").unwrap(), None);
        while let Some(op) = st.lease_next(a).unwrap() {
            st.settle_ok(op.id).unwrap();
        }
        st.reconcile_folder(a, "INBOX", &HashSet::from([1]), &flags, Some(11))
            .unwrap();
        assert!(!st.message(a, protected).unwrap().seen);
        assert_eq!(st.highest_modseq(a, "INBOX").unwrap(), Some(11));
        st.reset_folder(a, "INBOX").unwrap();
        assert!(st
            .search(
                a,
                &Query {
                    text: vec!["reconcileneedle".into()],
                    ..Default::default()
                },
                10
            )
            .unwrap()
            .is_empty());
    }
    #[test]
    fn reset_parks_only_old_folder_uid_operations_atomically() {
        let mut st = Store::open_in_memory().unwrap();
        let a = st.add_account("a@test", "", "imap").unwrap();
        seed(&mut st, a, 1);
        st.enqueue(
            a,
            &OpKind::Move {
                from_folder: "INBOX".into(),
                uid: 1,
                to_folder: "Archive".into(),
            },
        )
        .unwrap();
        st.enqueue(
            a,
            &OpKind::SetSeen {
                folder: "Other".into(),
                uid: 1,
                seen: true,
            },
        )
        .unwrap();
        st.enqueue(
            a,
            &OpKind::Append {
                folder: "INBOX".into(),
                message_id: "id".into(),
                staged_path: "path".into(),
            },
        )
        .unwrap();
        assert_eq!(st.reset_folder(a, "INBOX").unwrap(), 1);
        assert_eq!(st.needs_review(a).unwrap().len(), 1);
        assert!(matches!(
            st.needs_review(a).unwrap()[0].kind,
            OpKind::Move { .. }
        ));
        assert_eq!(st.pending_count(a).unwrap(), 3);
        for _ in 0..2 {
            let op = st.lease_next(a).unwrap().unwrap();
            assert!(!matches!(op.kind, OpKind::Move { .. }));
            st.settle_ok(op.id).unwrap();
        }
        assert!(st.lease_next(a).unwrap().is_none());
        // Retired UID operations must not pin messages in the new epoch.
        let new_id = seed(&mut st, a, 1);
        st.reconcile_folder(a, "INBOX", &HashSet::new(), &[], None)
            .unwrap();
        assert!(st.message(a, new_id).is_err());
        assert!(
            st.conn()
                .query_row("SELECT count(*) FROM message_content", [], |r| r
                    .get::<_, i64>(0))
                .unwrap()
                == 0
        );
    }
}
