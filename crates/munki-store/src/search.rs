//! Query execution over FTS5.
//!
//! The Apple Mail complaint this answers: search that is slow and fails
//! to find text it should. FTS5 indexes every message at insert time, so
//! a query is an index lookup, never a scan.

use anyhow::Result;
use munki_core::{AccountId, Message, Query};
use rusqlite::types::Value;

use crate::{dest_str, row_to_message, Store};

impl Store {
    /// Run a parsed query, newest first.
    ///
    /// Unlike `view`, this does *not* filter dismissed senders: screened
    /// mail is hidden from views but must remain findable. Hiding it from
    /// search would make the Screener a delete button, which is the thing
    /// users are rightly afraid of.
    pub fn search(&self, account: AccountId, q: &Query, limit: u32) -> Result<Vec<Message>> {
        self.search_page(account, q, limit, 0)
    }

    /// Fetch only the requested page; stable ID ties prevent duplicate date pages.
    pub fn search_page(
        &self,
        account: AccountId,
        q: &Query,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Message>> {
        let (filter, mut args) = search_filter(account, q);
        let mut sql = format!("SELECT m.id{filter}");
        // Sort only IDs and dates; load wide message rows after LIMIT.
        // ponytail: common terms still enumerate all matches. Add a date index
        // with measured date-first planning if mailbox size exceeds this budget.
        sql.push_str(" ORDER BY m.date DESC, m.id DESC LIMIT ? OFFSET ?");
        args.push(Value::from(limit));
        args.push(Value::from(offset));
        let sql = format!(
            "SELECT m.id, m.account, m.folder, m.uid, m.message_id, m.thread_id,
                    m.from_addr, m.subject, m.renamed_subject, m.date, m.seen,
                    m.has_attachment, m.destination
             FROM ({sql}) hits JOIN messages m ON m.id=hits.id
             ORDER BY m.date DESC, m.id DESC"
        );

        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), row_to_message)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Exact count without sorting or loading message rows.
    pub fn search_count(&self, account: AccountId, q: &Query) -> Result<u64> {
        let (filter, args) = search_filter(account, q);
        Ok(self.conn().query_row(
            &format!("SELECT COUNT(*){filter}"),
            rusqlite::params_from_iter(args),
            |row| row.get(0),
        )?)
    }
}

/// Shared FROM/WHERE clauses: pagination and counts always use identical filters.
fn search_filter(account: AccountId, q: &Query) -> (String, Vec<Value>) {
    let mut sql = String::from(" FROM messages m");
    let mut args = Vec::new();
    // Join the FTS table only when there is text to match; a pure
    // metadata filter should not pay for it.
    if !q.text.is_empty() {
        sql.push_str(" JOIN messages_fts f ON f.rowid = m.id");
    }
    sql.push_str(" WHERE m.account = ?");
    args.push(Value::from(account));

    if !q.text.is_empty() {
        sql.push_str(" AND messages_fts MATCH ?");
        args.push(Value::from(fts_match(&q.text)));
    }
    if let Some(folder) = &q.folder {
        sql.push_str(" AND m.folder = ?");
        args.push(Value::from(folder.clone()));
    }
    if let Some(from) = &q.from {
        sql.push_str(" AND m.from_addr = ?");
        args.push(Value::from(from.as_str().to_string()));
    }
    if let Some(d) = q.destination {
        sql.push_str(" AND m.destination = ?");
        args.push(Value::from(dest_str(d).to_string()));
    }
    if let Some(true) = q.has_attachment {
        sql.push_str(" AND m.has_attachment = 1");
    }
    if let Some(unread) = q.unread {
        sql.push_str(" AND m.seen = ?");
        args.push(Value::from(!unread));
    }
    if let Some(before) = q.before {
        sql.push_str(" AND m.date < ?");
        args.push(Value::from(before));
    }
    if let Some(after) = q.after {
        sql.push_str(" AND m.date >= ?");
        args.push(Value::from(after));
    }

    (sql, args)
}

/// Build an FTS5 MATCH expression from user terms.
///
/// Every term is quoted, which turns FTS5's operator syntax (`*`, `:`,
/// `NEAR`, `OR`, parens) into literal text. Without this, searching for
/// an email containing `AND` or a `(` is a syntax error thrown in the
/// user's face. Internal `"` is doubled, per FTS5 string escaping.
fn fts_match(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_in_user_input_are_literal() {
        // Each of these is an FTS5 syntax error unquoted.
        assert_eq!(fts_match(&["NEAR".into()]), "\"NEAR\"");
        assert_eq!(fts_match(&["a*".into(), "(b".into()]), "\"a*\" AND \"(b\"");
        assert_eq!(fts_match(&["say \"hi\"".into()]), "\"say \"\"hi\"\"\"");
    }
    #[test]
    fn limited_text_search_filters_before_limit_and_returns_newest_rows() {
        use crate::NewMessage;
        use munki_core::{Address, Destination};
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@test", "", "imap").unwrap();
        let other = store.add_account("other@test", "", "imap").unwrap();
        for (uid, owner, date, seen, attachment, destination) in [
            (1, account, 30, false, true, Destination::Feed),
            (2, account, 20, false, true, Destination::Feed),
            (3, other, 35, false, true, Destination::Feed),
            (4, account, 36, true, true, Destination::Feed),
            (5, account, 37, false, false, Destination::Feed),
            (6, account, 38, false, true, Destination::Imbox),
            (7, account, 50, false, true, Destination::Feed),
        ] {
            store
                .upsert_message(
                    &NewMessage {
                        account: owner,
                        folder: "INBOX",
                        uid,
                        message_id: &format!("{uid}@test"),
                        thread_id: "thread",
                        from: Address::new("sender@test"),
                        subject: "deployment",
                        date,
                        seen,
                        has_attachment: attachment,
                        destination,
                    },
                    "deployment details",
                    "report.pdf",
                )
                .unwrap();
        }
        let q = munki_core::query::parse(
            "deployment from:sender@test in:feed is:unread has:attachment",
        );
        let q = Query {
            after: Some(15),
            before: Some(40),
            ..q
        };
        let rows = store.search(account, &q, 2).unwrap();
        assert_eq!(rows.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(rows[0].subject, "deployment");
        assert_eq!(store.search_count(account, &q).unwrap(), 2);
        assert_eq!(store.search_page(account, &q, 1, 1).unwrap(), rows[1..]);
        assert!(store.search_page(account, &q, 2, 2).unwrap().is_empty());
        assert_eq!(store.search(account, &q, 1).unwrap(), rows[..1]);
        assert!(store.search(account, &q, 0).unwrap().is_empty());
        let metadata = Query { text: vec![], ..q };
        assert_eq!(store.search(account, &metadata, 2).unwrap(), rows);
        assert_eq!(store.search_count(account, &metadata).unwrap(), 2);
        assert_eq!(
            store.search_page(account, &metadata, 1, 1).unwrap(),
            rows[1..]
        );
    }
}
