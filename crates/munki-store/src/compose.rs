//! Draft content and queued MIME bytes share the encrypted store.
use crate::Store;
use anyhow::{ensure, Result};
use munki_core::{compose::Draft, AccountId, OpKind};
use rusqlite::params;

impl Store {
    pub fn save_draft(&self, account: AccountId, id: Option<i64>, draft: &Draft) -> Result<i64> {
        let content = serde_json::to_string(draft)?;
        if let Some(id) = id {
            ensure!(self.conn().execute(
                "UPDATE drafts SET content=?3, updated_at=unixepoch() WHERE id=?1 AND account=?2 AND state='draft'",
                params![id, account, content],
            )? == 1, "draft missing or already queued");
            Ok(id)
        } else {
            self.conn().execute(
                "INSERT INTO drafts(account,content) VALUES(?1,?2)",
                params![account, content],
            )?;
            Ok(self.conn().last_insert_rowid())
        }
    }

    pub fn draft(&self, account: AccountId, id: i64) -> Result<Draft> {
        let json: String = self.conn().query_row(
            "SELECT content FROM drafts WHERE id=?1 AND account=?2",
            params![id, account],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn draft_wire(&self, account: AccountId, id: i64) -> Result<(String, Vec<u8>)> {
        Ok(self.conn().query_row(
            "SELECT message_id,wire FROM drafts WHERE id=?1 AND account=?2 AND state<>'draft'",
            params![id, account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Freeze the exact MIME bytes and enqueue atomically. A second click cannot send twice.
    pub fn queue_draft(
        &mut self,
        account: AccountId,
        id: i64,
        message_id: &str,
        wire: &[u8],
        sent_folder: &str,
    ) -> Result<()> {
        ensure!(
            !sent_folder.is_empty() && !sent_folder.chars().any(char::is_control),
            "invalid Sent folder"
        );
        let tx = self.conn.transaction()?;
        ensure!(tx.execute("UPDATE drafts SET state='queued',wire=?3,message_id=?4,sent_folder=?5 WHERE id=?1 AND account=?2 AND state='draft'",params![id,account,wire,message_id,sent_folder])?==1,"draft missing or already queued");
        tx.execute(
            "INSERT INTO outbox(account,kind,created_at) VALUES(?1,?2,unixepoch())",
            params![
                account,
                serde_json::to_string(&OpKind::Submit { draft_id: id })?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// SMTP acknowledged delivery: the same op becomes a Sent-copy operation.
    pub fn delivered(&mut self, account: AccountId, op_id: i64, draft_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let folder: String = tx.query_row(
            "SELECT sent_folder FROM drafts WHERE id=?1 AND account=?2 AND state='queued'",
            params![draft_id, account],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE drafts SET state='delivered' WHERE id=?1",
            params![draft_id],
        )?;
        ensure!(tx.execute("UPDATE outbox SET kind=?3,leased_at=NULL,not_before=0,last_error=NULL WHERE id=?1 AND account=?2",params![op_id,account,serde_json::to_string(&OpKind::SaveSent{draft_id,folder})?])?==1,"send operation missing");
        tx.commit()?;
        Ok(())
    }

    pub fn saved_sent(&mut self, account: AccountId, op_id: i64, draft_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE drafts SET state='sent' WHERE id=?1 AND account=?2",
            params![draft_id, account],
        )?;
        tx.execute(
            "DELETE FROM outbox WHERE id=?1 AND account=?2",
            params![op_id, account],
        )?;
        tx.commit()?;
        Ok(())
    }
}
