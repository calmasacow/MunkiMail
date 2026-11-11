//! Atomic local triage actions, snapshot merging, and restored message settings.
use crate::{dest_str, row_to_message, Store};
use anyhow::{ensure, Context, Result};
use munki_core::{
    state::{MessageSettings, StateEntry, StateSnapshot, StateValue, MAX_STATE_BYTES},
    triage::Stack,
    AccountId, Address, Message, MessageId, OpKind, SenderVerdict,
};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) fn snapshot(conn: &Connection, account: AccountId) -> Result<StateSnapshot> {
    let mut stmt = conn.prepare(
        "SELECT key,revision,device,value FROM triage_entries WHERE account=?1 ORDER BY kind,key",
    )?;
    let rows = stmt
        .query_map(params![account], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let entries = rows
        .into_iter()
        .map(|(key, revision, device, value)| {
            Ok(StateEntry {
                key,
                revision,
                device,
                value: serde_json::from_str(&value)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(StateSnapshot { format: 1, entries })
}

fn apply(conn: &Connection, account: AccountId, entry: &StateEntry) -> Result<usize> {
    let value = serde_json::to_string(&entry.value)?;
    let changed=conn.execute("INSERT INTO triage_entries(account,kind,key,revision,device,value) VALUES(?1,?2,?3,?4,?5,?6)
        ON CONFLICT(account,kind,key) DO UPDATE SET revision=?4,device=?5,value=?6
        WHERE (excluded.revision,excluded.device,excluded.value) > (triage_entries.revision,triage_entries.device,triage_entries.value)",
        params![account,entry.value.kind(),entry.key,entry.revision,entry.device,value])?;
    if changed == 0 {
        return Ok(0);
    }
    match entry.value {
        StateValue::Sender(verdict) => {
            let (name, destination) = match verdict {
                SenderVerdict::Allow(d) => ("allow", Some(dest_str(d))),
                SenderVerdict::Dismiss => ("dismiss", None),
            };
            conn.execute("INSERT INTO senders(account,address,verdict,destination,decided_at) VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(account,address) DO UPDATE SET verdict=?3,destination=?4,decided_at=?5",params![account,entry.key,name,destination,entry.revision/1_000_000])?;
            if let Some(d) = destination {
                return Ok(conn.execute("UPDATE messages SET destination=?3 WHERE account=?1 AND from_addr=?2 AND destination<>?3",params![account,entry.key,d])?);
            }
        }
        StateValue::Message(_) => {
            restore_message(conn, account, &entry.key)?;
        }
    }
    Ok(0)
}

/// Called inside ingestion's transaction, after the FTS row exists.
pub(crate) fn restore_message(
    conn: &Connection,
    account: AccountId,
    message_id: &str,
) -> Result<()> {
    let json: Option<String> = conn
        .query_row(
            "SELECT value FROM triage_entries WHERE account=?1 AND kind='message' AND key=?2",
            params![account, message_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(json) = json {
        if let StateValue::Message(settings) = serde_json::from_str(&json)? {
            conn.execute("UPDATE messages SET renamed_subject=?3,thread_id=COALESCE(?4,original_thread_id),reply_later=?5,set_aside=?6 WHERE account=?1 AND message_id=?2",params![account,message_id,settings.subject,settings.thread_id,settings.reply_later,settings.set_aside])?;
            conn.execute("UPDATE messages_fts SET subject=(SELECT subject || ' ' || COALESCE(renamed_subject,'') FROM messages WHERE id=messages_fts.rowid) WHERE rowid IN (SELECT id FROM messages WHERE account=?1 AND message_id=?2)",params![account,message_id])?;
        }
    }
    Ok(())
}

fn enqueue(conn: &Connection, account: AccountId, op: &OpKind) -> Result<()> {
    conn.execute(
        "INSERT INTO outbox(account,kind,created_at) VALUES(?1,?2,unixepoch())",
        params![account, serde_json::to_string(op)?],
    )?;
    Ok(())
}

fn local_entry(
    conn: &Connection,
    account: AccountId,
    key: String,
    value: StateValue,
) -> Result<usize> {
    let revision:i64=conn.query_row("SELECT MAX(COALESCE(MAX(revision),0)+1,CAST((julianday('now')-2440587.5)*86400000000 AS INTEGER)) FROM triage_entries WHERE account=?1",params![account],|r|r.get(0))?;
    let device = conn.query_row("SELECT id FROM local_device", [], |r| r.get(0))?;
    let entry = StateEntry {
        key,
        revision,
        device,
        value,
    };
    let delta = StateSnapshot {
        format: 1,
        entries: vec![entry.clone()],
    };
    delta.validate().map_err(anyhow::Error::msg)?;
    let moved = apply(conn, account, &entry)?;
    ensure!(
        serde_json::to_vec(&snapshot(conn, account)?)?.len() <= MAX_STATE_BYTES,
        "triage state exceeds 8 MiB"
    );
    enqueue(
        conn,
        account,
        &OpKind::PutState {
            version: 1,
            json: serde_json::to_string(&delta)?,
        },
    )?;
    Ok(moved)
}

fn message_settings(
    conn: &Connection,
    account: AccountId,
    id: MessageId,
) -> Result<(String, MessageSettings)> {
    Ok(conn.query_row("SELECT message_id,renamed_subject,CASE WHEN thread_id<>original_thread_id THEN thread_id END,reply_later,set_aside FROM messages WHERE account=?1 AND id=?2",params![account,id],|r|Ok((r.get(0)?,MessageSettings{subject:r.get(1)?,thread_id:r.get(2)?,reply_later:r.get(3)?,set_aside:r.get(4)?})))?)
}

impl Store {
    pub fn triage_snapshot(&self, account: AccountId) -> Result<StateSnapshot> {
        snapshot(self.conn(), account)
    }

    /// Validate everything before applying anything; malformed snapshots cannot partially mutate state.
    pub fn merge_triage(&mut self, account: AccountId, incoming: &StateSnapshot) -> Result<()> {
        incoming.validate().map_err(anyhow::Error::msg)?;
        ensure!(
            serde_json::to_vec(incoming)?.len() <= MAX_STATE_BYTES,
            "triage state exceeds 8 MiB"
        );
        let tx = self.conn.transaction()?;
        for entry in &incoming.entries {
            apply(&tx, account, entry)?;
        }
        ensure!(
            serde_json::to_vec(&snapshot(&tx, account)?)?.len() <= MAX_STATE_BYTES,
            "merged triage state exceeds 8 MiB"
        );
        tx.commit()?;
        Ok(())
    }

    /// Bulk decisions, roster, local routing, and queued server work commit together.
    pub fn decide_senders(
        &mut self,
        account: AccountId,
        decisions: &[(Address, SenderVerdict)],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut moved = 0;
        for (address, verdict) in decisions {
            moved += local_entry(
                &tx,
                account,
                address.as_str().to_owned(),
                StateValue::Sender(*verdict),
            )?;
            if let SenderVerdict::Allow(destination) = verdict {
                let rows = tx
                    .prepare("SELECT folder,uid FROM messages WHERE account=?1 AND from_addr=?2")?
                    .query_map(params![account, address.as_str()], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (folder, uid) in rows {
                    enqueue(
                        &tx,
                        account,
                        &OpKind::SetDestination {
                            folder,
                            uid,
                            destination: *destination,
                        },
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    pub(crate) fn rename_synced(&mut self, id: MessageId, subject: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction()?;
        let account = tx.query_row(
            "SELECT account FROM messages WHERE id=?1",
            params![id],
            |r| r.get(0),
        )?;
        let (key, mut settings) = message_settings(&tx, account, id)?;
        settings.subject = subject.map(str::to_owned);
        local_entry(&tx, account, key, StateValue::Message(settings))?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_stack(
        &mut self,
        account: AccountId,
        id: MessageId,
        stack: Stack,
        enabled: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (key, mut settings) = message_settings(&tx, account, id)?;
        match stack {
            Stack::ReplyLater => settings.reply_later = enabled,
            Stack::SetAside => settings.set_aside = enabled,
        };
        local_entry(&tx, account, key.clone(), StateValue::Message(settings))?;
        let rows = tx
            .prepare("SELECT folder,uid FROM messages WHERE account=?1 AND message_id=?2")?
            .query_map(params![account, key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (folder, uid) in rows {
            enqueue(
                &tx,
                account,
                &OpKind::SetStack {
                    folder,
                    uid,
                    stack,
                    enabled,
                },
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Merge existing messages from the source thread into the target thread.
    pub fn merge_threads(
        &mut self,
        account: AccountId,
        source: &str,
        target: &str,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        ensure!(
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE account=?1 AND thread_id=?2)",
                params![account, target],
                |r| r.get::<_, bool>(0)
            )?,
            "target thread missing"
        );
        let ids=tx.prepare("SELECT MIN(id) FROM messages WHERE account=?1 AND thread_id=?2 GROUP BY message_id")?.query_map(params![account,source],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for id in &ids {
            let (key, mut settings) = message_settings(&tx, account, *id)?;
            settings.thread_id = Some(target.to_owned());
            local_entry(&tx, account, key, StateValue::Message(settings))?;
        }
        tx.commit()?;
        // ponytail: existing messages only; add a source-root mapping when new replies must follow a merge automatically.
        Ok(ids.len())
    }

    pub fn stack(&self, account: AccountId, stack: Stack, limit: u32) -> Result<Vec<Message>> {
        let column = match stack {
            Stack::ReplyLater => "reply_later",
            Stack::SetAside => "set_aside",
        };
        let sql=format!("SELECT m.id,m.account,m.folder,m.uid,m.message_id,m.thread_id,m.from_addr,m.subject,m.renamed_subject,m.date,m.seen,m.has_attachment,m.destination FROM messages m LEFT JOIN senders s ON s.account=m.account AND s.address=m.from_addr WHERE m.account=?1 AND m.{column}=1 AND (s.verdict IS NULL OR s.verdict<>'dismiss') ORDER BY m.date DESC LIMIT ?2");
        Ok(self
            .conn()
            .prepare(&sql)?
            .query_map(params![account, limit], row_to_message)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Server keywords supply a fallback when no message settings snapshot exists.
    pub fn server_stacks(
        &self,
        account: AccountId,
        id: MessageId,
        reply_later: bool,
        set_aside: bool,
    ) -> Result<()> {
        self.conn().execute("UPDATE messages SET reply_later=?3,set_aside=?4 WHERE account=?1 AND id=?2 AND NOT EXISTS(SELECT 1 FROM triage_entries WHERE account=?1 AND kind='message' AND key=messages.message_id)",params![account,id,reply_later,set_aside]).context("restoring stack keywords")?;
        Ok(())
    }
}
