use crate::{format, Contact, Event};
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Calendar,
    AddressBook,
}
impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::AddressBook => "contacts",
        }
    }
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Calendar => "ics",
            Self::AddressBook => "vcf",
        }
    }
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Calendar => "text/calendar; charset=utf-8",
            Self::AddressBook => "text/vcard; charset=utf-8",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub collection_id: String,
    pub uid: String,
    pub raw: String,
    pub href: Option<String>,
    pub etag: Option<String>,
    pub dirty: bool,
    pub deleted: bool,
    pub revision: i64,
}
impl Record {
    pub fn event(&self) -> Result<Event> {
        format::event(&self.raw)
    }
    pub fn contact(&self) -> Result<Contact> {
        format::contact(&self.raw)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub record_id: String,
    /// None means the server deleted the resource.
    pub remote_raw: Option<String>,
    pub remote_etag: Option<String>,
}
#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    KeepLocal,
    AcceptRemote,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub email: String,
    pub contacts: Vec<Record>,
    pub events: Vec<Record>,
}

pub struct Store<'a> {
    pub(crate) db: &'a Connection,
}
const SELECT: &str =
    "SELECT id,collection_id,uid,raw,href,etag,dirty,deleted,revision FROM pim_records";
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        id: r.get(0)?,
        collection_id: r.get(1)?,
        uid: r.get(2)?,
        raw: r.get(3)?,
        href: r.get(4)?,
        etag: r.get(5)?,
        dirty: r.get(6)?,
        deleted: r.get(7)?,
        revision: r.get(8)?,
    })
}
impl<'a> Store<'a> {
    /// Borrow the application's already-open, already-keyed SQLite/SQLCipher connection.
    /// Never opens a database, sets a key, or changes journal/encryption settings.
    pub fn new(db: &'a Connection) -> Result<Self> {
        db.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS pim_collections(id TEXT PRIMARY KEY,name TEXT NOT NULL,kind TEXT NOT NULL,url TEXT UNIQUE);
        CREATE TABLE IF NOT EXISTS pim_records(
          id TEXT PRIMARY KEY,collection_id TEXT NOT NULL REFERENCES pim_collections(id),uid TEXT NOT NULL,raw TEXT NOT NULL,
          href TEXT,etag TEXT,dirty INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1,
          start INTEGER,end INTEGER,UNIQUE(collection_id,uid),UNIQUE(collection_id,href));
        CREATE INDEX IF NOT EXISTS pim_range ON pim_records(collection_id,start,end);
        CREATE TABLE IF NOT EXISTS pim_emails(record_id TEXT NOT NULL REFERENCES pim_records(id) ON DELETE CASCADE,email TEXT NOT NULL,PRIMARY KEY(record_id,email));
        CREATE INDEX IF NOT EXISTS pim_email ON pim_emails(email);
        CREATE TABLE IF NOT EXISTS pim_conflicts(record_id TEXT PRIMARY KEY REFERENCES pim_records(id) ON DELETE CASCADE,remote_raw TEXT,remote_etag TEXT);")?;
        Ok(Self { db })
    }
    pub fn create_collection(
        &self,
        name: &str,
        kind: Kind,
        url: Option<&str>,
    ) -> Result<Collection> {
        ensure!(!name.trim().is_empty(), "collection name required");
        let url = url
            .map(crate::dav::collection_url)
            .transpose()?
            .map(|u| u.to_string());
        let c = Collection {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            kind,
            url,
        };
        self.db.execute(
            "INSERT INTO pim_collections VALUES(?1,?2,?3,?4)",
            params![c.id, c.name, c.kind.as_str(), c.url],
        )?;
        Ok(c)
    }
    pub fn collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self
            .db
            .prepare("SELECT id,name,kind,url FROM pim_collections ORDER BY name,id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Collection {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: if r.get::<_, String>(2)? == "calendar" {
                    Kind::Calendar
                } else {
                    Kind::AddressBook
                },
                url: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn collection(&self, id: &str) -> Result<Collection> {
        self.collections()?
            .into_iter()
            .find(|c| c.id == id)
            .context("collection not found")
    }
    pub fn get(&self, id: &str) -> Result<Record> {
        self.db
            .query_row(&format!("{SELECT} WHERE id=?1"), [id], row)
            .optional()?
            .context("record not found")
    }
    /// Includes tombstones, so callers can display pending deletions.
    pub fn records(&self, collection: &str) -> Result<Vec<Record>> {
        self.collection(collection)?;
        let mut stmt = self
            .db
            .prepare(&format!("{SELECT} WHERE collection_id=?1 ORDER BY uid"))?;
        let records = stmt
            .query_map([collection], row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(records)
    }
    /// Half-open overlap [start,end), Unix seconds. Point events match start <= t < end.
    /// All-day dates use UTC midnight for indexing; recurrences are not expanded.
    // ponytail: index master intervals only; add an RFC recurrence engine for occurrence queries.
    pub fn events_in_range(&self, collection: &str, start: i64, end: i64) -> Result<Vec<Record>> {
        ensure!(
            self.collection(collection)?.kind == Kind::Calendar,
            "not a calendar"
        );
        ensure!(start < end, "invalid range");
        let mut stmt=self.db.prepare(&format!("{SELECT} WHERE collection_id=?1 AND deleted=0 AND start<?3 AND (end>?2 OR (start=end AND start>=?2)) ORDER BY start,uid"))?;
        let records = stmt
            .query_map(params![collection, start, end], row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(records)
    }
    pub fn contacts(&self, collection: &str) -> Result<Vec<Record>> {
        ensure!(
            self.collection(collection)?.kind == Kind::AddressBook,
            "not an address book"
        );
        Ok(self
            .records(collection)?
            .into_iter()
            .filter(|r| !r.deleted)
            .collect())
    }
    /// Unified identity across every collection; aliases are not guessed.
    pub fn person(&self, email: &str) -> Result<Person> {
        let email = format::normalize_email(email)?;
        let mut stmt=self.db.prepare(&format!("{SELECT} WHERE deleted=0 AND id IN (SELECT record_id FROM pim_emails WHERE email=?1) ORDER BY collection_id,uid"))?;
        let records = stmt
            .query_map([&email], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut p = Person {
            email,
            contacts: vec![],
            events: vec![],
        };
        for r in records {
            if self.collection(&r.collection_id)?.kind == Kind::Calendar {
                p.events.push(r);
            } else {
                p.contacts.push(r);
            }
        }
        Ok(p)
    }
    /// New records: id/revision None. Edits require the revision returned by get/list.
    pub fn save_event(
        &self,
        collection: &str,
        id: Option<&str>,
        expected_revision: Option<i64>,
        event: &Event,
    ) -> Result<Record> {
        ensure!(
            self.collection(collection)?.kind == Kind::Calendar,
            "not a calendar"
        );
        let source = id.map(|id| self.get(id)).transpose()?;
        let raw = format::write_event(event, source.as_ref().map(|r| r.raw.as_str()))?;
        self.save_raw(collection, id, expected_revision, &raw)
    }
    pub fn save_contact(
        &self,
        collection: &str,
        id: Option<&str>,
        expected_revision: Option<i64>,
        contact: &Contact,
    ) -> Result<Record> {
        ensure!(
            self.collection(collection)?.kind == Kind::AddressBook,
            "not an address book"
        );
        let source = id.map(|id| self.get(id)).transpose()?;
        let raw = format::write_contact(contact, source.as_ref().map(|r| r.raw.as_str()))?;
        self.save_raw(collection, id, expected_revision, &raw)
    }
    /// Advanced editor: replaces the complete resource after validation.
    pub fn save_raw(
        &self,
        collection: &str,
        id: Option<&str>,
        expected_revision: Option<i64>,
        raw: &str,
    ) -> Result<Record> {
        let kind = self.collection(collection)?.kind;
        let tx = self.db.unchecked_transaction()?;
        let record = self.save_on(collection, kind, id, expected_revision, raw)?;
        tx.commit()?;
        Ok(record)
    }
    /// Participates in the caller's transaction, without BEGIN/COMMIT/savepoints.
    /// The caller must roll back the entire transaction if this operation fails.
    pub fn save_raw_in_transaction(
        &self,
        collection: &str,
        id: Option<&str>,
        expected_revision: Option<i64>,
        raw: &str,
    ) -> Result<Record> {
        ensure!(!self.db.is_autocommit(), "caller transaction required");
        let kind = self.collection(collection)?.kind;
        self.save_on(collection, kind, id, expected_revision, raw)
    }
    fn save_on(
        &self,
        collection: &str,
        kind: Kind,
        id: Option<&str>,
        expected: Option<i64>,
        raw: &str,
    ) -> Result<Record> {
        let (uid, start, end, emails) = format::metadata(raw, kind)?;
        let id = if let Some(id) = id {
            let old = self.get(id)?;
            ensure!(
                old.collection_id == collection && old.uid == uid,
                "collection and UID are immutable"
            );
            ensure!(expected == Some(old.revision), "stale revision");
            let count=self.db.execute("UPDATE pim_records SET raw=?2,start=?3,end=?4,dirty=1,deleted=0,revision=revision+1 WHERE id=?1 AND revision=?5",params![id,raw,start,end,old.revision])?;
            ensure!(count == 1, "stale revision");
            id.to_owned()
        } else {
            ensure!(expected.is_none(), "revision without id");
            let id = uuid::Uuid::new_v4().to_string();
            self.db.execute("INSERT INTO pim_records(id,collection_id,uid,raw,dirty,start,end) VALUES(?1,?2,?3,?4,1,?5,?6)",params![id,collection,uid,raw,start,end])?;
            id
        };
        self.index_emails(&id, emails)?;
        self.get(&id)
    }
    fn index_emails(&self, id: &str, emails: Vec<String>) -> Result<()> {
        self.db
            .execute("DELETE FROM pim_emails WHERE record_id=?1", [id])?;
        for email in emails.into_iter().collect::<BTreeSet<_>>() {
            self.db
                .execute("INSERT INTO pim_emails VALUES(?1,?2)", params![id, email])?;
        }
        Ok(())
    }
    pub fn delete(&self, id: &str, expected_revision: i64) -> Result<()> {
        let n=self.db.execute("UPDATE pim_records SET deleted=1,dirty=1,revision=revision+1 WHERE id=?1 AND revision=?2",params![id,expected_revision])?;
        ensure!(n == 1, "record missing or stale revision");
        Ok(())
    }
    /// Atomic import. Existing UIDs reject the whole import; never overwrite local edits.
    pub fn import_ics(&self, collection: &str, raw: &str) -> Result<Vec<Record>> {
        self.import(collection, Kind::Calendar, format::split_ics(raw)?)
    }
    pub fn import_vcf(&self, collection: &str, raw: &str) -> Result<Vec<Record>> {
        self.import(collection, Kind::AddressBook, format::split_vcf(raw)?)
    }
    fn import(&self, collection: &str, kind: Kind, resources: Vec<String>) -> Result<Vec<Record>> {
        ensure!(
            self.collection(collection)?.kind == kind,
            "wrong collection kind"
        );
        let tx = self.db.unchecked_transaction()?;
        let records = resources
            .iter()
            .map(|raw| self.save_on(collection, kind, None, None, raw))
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(records)
    }
    /// Returns a self-contained resource, including recurrence exceptions and extensions.
    pub fn export(&self, id: &str) -> Result<String> {
        Ok(self.get(id)?.raw)
    }
    /// ICS exports concatenate complete VCALENDAR objects; VCF exports concatenate VCARDs.
    pub fn export_collection(&self, collection: &str) -> Result<String> {
        Ok(self
            .records(collection)?
            .into_iter()
            .filter(|r| !r.deleted)
            .map(|r| r.raw.trim_end().to_owned() + "\r\n")
            .collect())
    }
    pub fn conflicts(&self, collection: &str) -> Result<Vec<Conflict>> {
        self.collection(collection)?;
        let mut stmt=self.db.prepare("SELECT record_id,remote_raw,remote_etag FROM pim_conflicts WHERE record_id IN (SELECT id FROM pim_records WHERE collection_id=?1) ORDER BY record_id")?;
        let records = stmt
            .query_map([collection], |r| {
                Ok(Conflict {
                    record_id: r.get(0)?,
                    remote_raw: r.get(1)?,
                    remote_etag: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(records)
    }
    /// Requires the current local revision. KeepLocal rebases the next conditional write;
    /// AcceptRemote discards local edits only upon this explicit call.
    pub fn resolve(&self, id: &str, expected_revision: i64, resolution: Resolution) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let r = self.get(id)?;
        ensure!(r.revision == expected_revision, "stale revision");
        let (raw, etag): (Option<String>, Option<String>) = self
            .db
            .query_row(
                "SELECT remote_raw,remote_etag FROM pim_conflicts WHERE record_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context("no conflict")?;
        match resolution {
            Resolution::KeepLocal => {
                if raw.is_none() && r.deleted {
                    self.db
                        .execute("DELETE FROM pim_records WHERE id=?1", [id])?;
                } else {
                    self.db.execute(
                        "UPDATE pim_records SET etag=?2,dirty=1,revision=revision+1 WHERE id=?1",
                        params![id, etag],
                    )?;
                }
            }
            Resolution::AcceptRemote => {
                if let Some(raw) = raw {
                    self.replace_remote(&r, &raw, etag.as_deref().context("remote ETag missing")?)?;
                } else {
                    self.db
                        .execute("DELETE FROM pim_records WHERE id=?1", [id])?;
                }
            }
        }
        self.db
            .execute("DELETE FROM pim_conflicts WHERE record_id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    fn replace_remote(&self, r: &Record, raw: &str, etag: &str) -> Result<()> {
        let (uid, start, end, emails) =
            format::metadata(raw, self.collection(&r.collection_id)?.kind)?;
        ensure!(uid == r.uid, "remote UID changed at existing href");
        self.db.execute("UPDATE pim_records SET raw=?2,etag=?3,dirty=0,deleted=0,start=?4,end=?5,revision=revision+1 WHERE id=?1",params![r.id,raw,etag,start,end])?;
        self.index_emails(&r.id, emails)
    }
    pub(crate) fn conflict(&self, id: &str, raw: Option<&str>, etag: Option<&str>) -> Result<()> {
        self.db.execute("INSERT INTO pim_conflicts VALUES(?1,?2,?3) ON CONFLICT(record_id) DO UPDATE SET remote_raw=excluded.remote_raw,remote_etag=excluded.remote_etag",params![id,raw,etag])?;
        Ok(())
    }
    pub(crate) fn has_conflict(&self, id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pim_conflicts WHERE record_id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn remote(&self, c: &Collection, href: &str, raw: &str, etag: &str) -> Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let (uid, _, _, _) = format::metadata(raw, c.kind)?;
        let existing = self
            .db
            .query_row(
                &format!("{SELECT} WHERE collection_id=?1 AND (href=?2 OR uid=?3)"),
                params![c.id, href, uid],
                row,
            )
            .optional()?;
        let conflicted = if let Some(r) = existing {
            if r.href.is_none() {
                self.assign_href(&r.id, href)?;
                self.conflict(&r.id, Some(raw), Some(etag))?;
                tx.commit()?;
                return Ok(true);
            }
            if r.href.as_deref() != Some(href) {
                bail!("remote UID collides with a different local href; no data overwritten");
            }
            if r.etag.as_deref() == Some(etag) {
                false
            } else if r.dirty || self.has_conflict(&r.id)? {
                self.conflict(&r.id, Some(raw), Some(etag))?;
                true
            } else {
                self.replace_remote(&r, raw, etag)?;
                false
            }
        } else {
            let r = self.save_on(&c.id, c.kind, None, None, raw)?;
            self.db.execute(
                "UPDATE pim_records SET href=?2,etag=?3,dirty=0 WHERE id=?1",
                params![r.id, href, etag],
            )?;
            false
        };
        tx.commit()?;
        Ok(conflicted)
    }
    pub(crate) fn remote_missing(&self, id: &str) -> Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let r = self.get(id)?;
        let conflict = if r.dirty && !r.deleted || self.has_conflict(id)? {
            self.conflict(id, None, None)?;
            true
        } else {
            self.db
                .execute("DELETE FROM pim_records WHERE id=?1", [id])?;
            false
        };
        tx.commit()?;
        Ok(conflict)
    }
    pub(crate) fn assign_href(&self, id: &str, href: &str) -> Result<()> {
        self.db.execute(
            "UPDATE pim_records SET href=?2 WHERE id=?1 AND href IS NULL",
            params![id, href],
        )?;
        Ok(())
    }
    // A concurrent editor must stay dirty after a successful network write.
    pub(crate) fn acknowledge(&self, r: &Record, etag: &str) -> Result<()> {
        self.db.execute("UPDATE pim_records SET etag=?2,dirty=CASE WHEN revision=?3 THEN 0 ELSE 1 END WHERE id=?1",params![r.id,etag,r.revision])?;
        Ok(())
    }
    pub(crate) fn acknowledge_delete(&self, r: &Record) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        if self.get(&r.id)?.revision == r.revision {
            self.db
                .execute("DELETE FROM pim_records WHERE id=?1", [&r.id])?;
        } else {
            self.db.execute(
                "UPDATE pim_records SET etag=NULL,dirty=1 WHERE id=?1",
                [&r.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_acknowledgements_preserve_edits_made_during_request() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        let store = Store::new(&conn)?;
        let c = store.create_collection("Book", Kind::AddressBook, None)?;
        let mut contact = Contact {
            uid: "c".into(),
            name: "Before".into(),
            emails: vec![],
            phones: vec![],
            organization: String::new(),
            note: String::new(),
        };
        let snapshot = store.save_contact(&c.id, None, None, &contact)?;
        contact.name = "During request".into();
        let edited =
            store.save_contact(&c.id, Some(&snapshot.id), Some(snapshot.revision), &contact)?;
        store.acknowledge(&snapshot, "\"server-version\"")?;
        let r = store.get(&snapshot.id)?;
        assert!(r.dirty);
        assert_eq!(r.revision, edited.revision);
        assert_eq!(r.contact()?.name, "During request");
        store.delete(&r.id, r.revision)?;
        let deleted = store.get(&r.id)?;
        store.save_contact(&c.id, Some(&r.id), Some(deleted.revision), &contact)?;
        store.acknowledge_delete(&deleted)?;
        let r = store.get(&r.id)?;
        assert!(r.dirty);
        assert!(!r.deleted);
        assert!(r.etag.is_none());
        Ok(())
    }
}
