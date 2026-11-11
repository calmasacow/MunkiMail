//! JMAP mail transport. Credentials remain in memory; no UI or credential storage.
use anyhow::{bail, ensure, Context, Result};
use munki_core::{AccountId, Address, Destination};
use munki_store::{NewMessage, Store};
use reqwest::{redirect::Policy, Url};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const CORE: &str = "urn:ietf:params:jmap:core";
const MAIL: &str = "urn:ietf:params:jmap:mail";
const JSON_LIMIT: usize = 32 * 1024 * 1024;
const MIME_LIMIT: usize = 64 * 1024 * 1024;
const SETTINGS: &str = "MunkiMail/.state";
const FOLDER: &str = "JMAP/Inbox";

/// Intentionally not Debug: callers must not log secrets.
pub enum Credentials {
    Basic { username: String, password: String },
    Bearer(String),
}

pub struct Client {
    http: reqwest::Client,
    credentials: Credentials,
    api: Url,
    download_template: Option<String>,
    upload_template: Option<String>,
    pub account_id: String,
}

fn safe_url(url: &Url) -> Result<()> {
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials forbidden"
    );
    ensure!(url.fragment().is_none(), "URL fragment forbidden");
    let loopback = match url.host() {
        Some(url::Host::Domain(h)) => h.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "HTTPS required except loopback"
    );
    Ok(())
}

impl Client {
    /// Discover at the supplied origin's `/.well-known/jmap`. All redirects
    /// are rejected; apiUrl must share that origin, including port and scheme.
    /// `remote_account` selects a mail account, otherwise primaryAccounts is used.
    pub async fn discover(
        user_url: &str,
        credentials: Credentials,
        remote_account: Option<&str>,
    ) -> Result<Self> {
        let origin = Url::parse(user_url)?;
        safe_url(&origin)?;
        let discovery = origin.join("/.well-known/jmap")?;
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let mut client = Self {
            http,
            credentials,
            api: discovery.clone(),
            download_template: None,
            upload_template: None,
            account_id: String::new(),
        };
        let response = client.auth(client.http.get(discovery)).send().await?;
        ensure!(
            response.status().is_success(),
            "JMAP discovery HTTP {}",
            response.status()
        );
        let session: Value = serde_json::from_slice(&bounded(response, JSON_LIMIT).await?)?;
        ensure!(
            session["capabilities"][CORE].is_object() && session["capabilities"][MAIL].is_object(),
            "missing JMAP mail capabilities"
        );
        let api = origin.join(string(&session, "apiUrl")?)?;
        safe_url(&api)?;
        ensure!(
            api.origin() == origin.origin(),
            "cross-origin JMAP apiUrl rejected"
        );
        let account = remote_account
            .or_else(|| session["primaryAccounts"][MAIL].as_str())
            .context("no primary mail account")?;
        ensure!(
            session["accounts"][account]["accountCapabilities"][MAIL].is_object(),
            "selected account lacks mail capability"
        );
        client.download_template = session["downloadUrl"].as_str().map(str::to_owned);
        client.upload_template = session["uploadUrl"].as_str().map(str::to_owned);
        client.api = api;
        client.account_id = account.to_owned();
        // Validate advertised endpoints before any authenticated request to them.
        for template in [&client.upload_template, &client.download_template]
            .into_iter()
            .flatten()
        {
            client.endpoint(template, "probe")?;
        }
        Ok(client)
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.credentials {
            Credentials::Basic { username, password } => {
                request.basic_auth(username, Some(password))
            }
            Credentials::Bearer(token) => request.bearer_auth(token),
        }
    }

    async fn call(&self, method: &str, mut args: Value) -> Result<Value> {
        args["accountId"] = json!(self.account_id);
        let response = self
            .auth(self.http.post(self.api.clone()))
            .json(&json!({
                "using": [CORE, MAIL], "methodCalls": [[method, args, "m"]]
            }))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "JMAP HTTP {}",
            response.status()
        );
        let value: Value = serde_json::from_slice(&bounded(response, JSON_LIMIT).await?)?;
        let calls = value["methodResponses"]
            .as_array()
            .context("missing methodResponses")?;
        ensure!(
            calls.len() == 1 && calls[0][2] == "m",
            "invalid method response"
        );
        if calls[0][0] == "error" {
            bail!("JMAP method error: {}", calls[0][1]["type"]);
        }
        ensure!(
            calls[0][0] == method && calls[0][1]["accountId"] == self.account_id,
            "unexpected method/account response"
        );
        Ok(calls[0][1].clone())
    }

    async fn download(&self, email: &Value) -> Result<Option<Vec<u8>>> {
        let Some(template) = &self.download_template else {
            return Ok(None);
        };
        let url = self.endpoint(template, string(email, "blobId")?)?;
        let response = self.auth(self.http.get(url)).send().await?;
        ensure!(
            response.status().is_success(),
            "JMAP download HTTP {}",
            response.status()
        );
        Ok(Some(bounded(response, MIME_LIMIT).await?))
    }

    fn endpoint(&self, template: &str, blob_id: &str) -> Result<Url> {
        let encode = |s: &str| {
            url::form_urlencoded::byte_serialize(s.as_bytes())
                .collect::<String>()
                .replace('+', "%20")
        };
        let expanded = template
            .replace("{accountId}", &encode(&self.account_id))
            .replace("{blobId}", &encode(blob_id))
            .replace("{name}", "message.eml")
            .replace("{type}", "message%2Frfc822");
        ensure!(
            !expanded.contains(['{', '}']),
            "unsupported JMAP URL template"
        );
        let url = self.api.join(&expanded)?;
        safe_url(&url)?;
        ensure!(
            url.origin() == self.api.origin(),
            "cross-origin JMAP endpoint rejected"
        );
        Ok(url)
    }

    async fn mailboxes(&self) -> Result<Vec<Value>> {
        let result = self
            .call(
                "Mailbox/get",
                json!({"ids":null,"properties":["id","name","parentId","role"]}),
            )
            .await?;
        Ok(result["list"]
            .as_array()
            .context("missing mailbox list")?
            .clone())
    }

    async fn mailbox(&self, name: &str, create: bool) -> Result<Option<String>> {
        ensure!(
            !name.is_empty() && !name.chars().any(char::is_control),
            "invalid mailbox name"
        );
        let mut boxes = self.mailboxes().await?;
        if let Some(id) = mailbox_path(&boxes, name)? {
            return Ok(Some(id));
        }
        if !create {
            return Ok(None);
        }
        let mut parent: Option<String> = None;
        for component in name.split('/') {
            ensure!(!component.is_empty(), "empty mailbox path component");
            let matches: Vec<_> = boxes
                .iter()
                .filter(|m| m["name"] == component && m["parentId"].as_str() == parent.as_deref())
                .collect();
            ensure!(matches.len() <= 1, "ambiguous mailbox path");
            if let Some(existing) = matches.first() {
                parent = Some(string(existing, "id")?.to_owned());
                continue;
            }
            let result = self
                .call(
                    "Mailbox/set",
                    json!({"create":{"mailbox":{"name":component,"parentId":parent}}}),
                )
                .await?;
            let id = result["created"]["mailbox"]["id"]
                .as_str()
                .with_context(|| {
                    format!(
                        "Mailbox/set creation not confirmed: {}",
                        result["notCreated"]["mailbox"]["type"]
                    )
                })?
                .to_owned();
            boxes.push(json!({"id":id,"name":component,"parentId":parent}));
            parent = Some(id);
        }
        Ok(parent)
    }

    async fn query_ids(&self, filter: Value) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut query_state = None;
        loop {
            let page = self
                .call(
                    "Email/query",
                    json!({"filter":filter,"position":ids.len(),"limit":256,"calculateTotal":true}),
                )
                .await?;
            let current = string(&page, "queryState")?.to_owned();
            if let Some(previous) = &query_state {
                ensure!(
                    previous == &current,
                    "query changed during pagination; retry"
                );
            }
            query_state = Some(current);
            let batch = strings(&page, "ids")?;
            let empty = batch.is_empty();
            ids.extend(batch);
            if ids.len() >= page["total"].as_u64().context("missing query total")? as usize {
                return Ok(ids);
            }
            ensure!(!empty, "query pagination made no progress");
        }
    }

    /// Download reserved settings MIME, without indexing or creating mailboxes.
    pub async fn load_settings(&self) -> Result<Vec<Vec<u8>>> {
        ensure!(
            self.download_template.is_some(),
            "JMAP downloadUrl required for settings"
        );
        let Some(mailbox) = self.mailbox(SETTINGS, false).await? else {
            return Ok(Vec::new());
        };
        let ids = self.query_ids(json!({"inMailbox":mailbox})).await?;
        let mut messages = Vec::new();
        for batch in ids.chunks(64) {
            let result = self
                .call(
                    "Email/get",
                    json!({"ids":batch,"properties":["id","blobId"]}),
                )
                .await?;
            let list = result["list"].as_array().context("missing settings list")?;
            ensure!(
                strings(&result, "notFound")?.is_empty(),
                "settings changed during fetch; retry"
            );
            let mut returned = BTreeSet::new();
            for email in list {
                let id = string(email, "id")?;
                ensure!(
                    batch.iter().any(|v| v == id) && returned.insert(id),
                    "unexpected settings email"
                );
                messages.push(
                    self.download(email)
                        .await?
                        .context("JMAP downloadUrl required")?,
                );
            }
            ensure!(returned.len() == batch.len(), "incomplete settings fetch");
        }
        Ok(messages)
    }

    /// Upload MIME then import into a lazily-created mailbox. Serialize appends
    /// per account: Message-ID lookup makes retries after ambiguous import safe.
    pub async fn append_to_mailbox(&self, name: &str, message_id: &str, raw: &[u8]) -> Result<()> {
        ensure!(
            !raw.is_empty() && raw.len() <= MIME_LIMIT,
            "MIME must be 1..=64 MiB"
        );
        let message_id = message_id.trim_matches(['<', '>']);
        ensure!(
            !message_id.is_empty()
                && !message_id
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || c == '<' || c == '>'),
            "invalid Message-ID"
        );
        let parsed = mail_parser::MessageParser::new()
            .parse(raw)
            .context("invalid MIME message")?;
        ensure!(
            parsed.message_id() == Some(message_id),
            "MIME Message-ID does not match append identity"
        );
        let template = self
            .upload_template
            .as_deref()
            .context("JMAP uploadUrl required for append")?;
        let url = self.endpoint(template, "")?;
        let mailbox = self
            .mailbox(name, true)
            .await?
            .context("mailbox creation failed")?;
        if !self
            .query_ids(
                json!({"inMailbox":mailbox,"header":["Message-ID",format!("<{message_id}>")]}),
            )
            .await?
            .is_empty()
        {
            return Ok(());
        }
        let response = self
            .auth(self.http.post(url))
            .header(reqwest::header::CONTENT_TYPE, "message/rfc822")
            .body(raw.to_vec())
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "JMAP upload HTTP {}",
            response.status()
        );
        let uploaded: Value = serde_json::from_slice(&bounded(response, JSON_LIMIT).await?)?;
        ensure!(
            uploaded["accountId"] == self.account_id,
            "upload account mismatch"
        );
        let blob = string(&uploaded, "blobId")?;
        let mut mailbox_ids = serde_json::Map::new();
        mailbox_ids.insert(mailbox, json!(true));
        let result = self.call("Email/import",json!({"emails":{"message":{"blobId":blob,"mailboxIds":mailbox_ids,"keywords":{"$seen":true}}}})).await?;
        ensure!(
            result["created"]["message"]["id"].is_string(),
            "Email/import not confirmed: {}",
            result["notCreated"]["message"]["type"]
        );
        Ok(())
    }

    /// Bind before reading settings or replaying mutations, preventing provider rebinding.
    pub fn bind_store(&self, store: &Store, account: AccountId) -> Result<()> {
        schema(store)?;
        let binding = format!("{}#{}", self.api, self.account_id);
        store.conn().execute(
            "INSERT OR IGNORE INTO jmap_state(account,provider) VALUES(?1,?2)",
            params![account, binding],
        )?;
        let provider: String = store.conn().query_row(
            "SELECT provider FROM jmap_state WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        ensure!(
            provider == binding,
            "local account bound to another JMAP provider/account"
        );
        Ok(())
    }

    /// Force a full enumeration after cannotCalculateChanges. Keeps stable mappings
    /// and local edits; the next sync also reconciles vanished server IDs.
    pub fn reset_cursor(store: &Store, account: AccountId) -> Result<()> {
        schema(store)?;
        store.conn().execute(
            "UPDATE jmap_state SET cursor=NULL WHERE account=?1",
            [account],
        )?;
        Ok(())
    }

    /// Resolve a local row for parent-owned outbound queue processing.
    pub fn server_id(store: &Store, account: AccountId, local_id: i64) -> Result<Option<String>> {
        schema(store)?;
        Ok(store
            .conn()
            .query_row(
                "SELECT jmap_id FROM jmap_messages WHERE account=?1 AND local_id=?2",
                params![account, local_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Patch only specified keywords, preserving unrelated server keywords.
    /// A caller draining Store's outbox must settle only confirmed successes.
    pub async fn set_keywords(&self, jmap_id: &str, keywords: &[(&str, bool)]) -> Result<()> {
        let mut patch = serde_json::Map::new();
        for (keyword, enabled) in keywords {
            ensure!(
                !keyword.is_empty() && !keyword.chars().any(char::is_control),
                "invalid keyword"
            );
            let escaped = keyword.replace('~', "~0").replace('/', "~1");
            patch.insert(
                format!("keywords/{escaped}"),
                if *enabled { json!(true) } else { Value::Null },
            );
        }
        let mut update = serde_json::Map::new();
        update.insert(jmap_id.to_owned(), Value::Object(patch));
        let result = self.call("Email/set", json!({"update": update})).await?;
        ensure!(
            result["updated"].get(jmap_id).is_some(),
            "Email/set not confirmed: {}",
            result["notUpdated"][jmap_id]["type"]
        );
        Ok(())
    }

    /// Synchronize this remote account into one Store account. Serialize calls per
    /// local account. Pending local work conservatively defers updates/deletions.
    pub async fn sync(&self, store: &mut Store, account: AccountId) -> Result<SyncReport> {
        let boxes = self.mailboxes().await?;
        let inboxes: Vec<_> = boxes.iter().filter(|m| m["role"] == "inbox").collect();
        ensure!(
            inboxes.len() == 1,
            "JMAP account must expose exactly one inbox-role mailbox"
        );
        let inbox = string(inboxes[0], "id")?;
        let settings = mailbox_path(&boxes, SETTINGS)?;
        self.bind_store(store, account)?;
        let cursor: Option<String> = store.conn().query_row(
            "SELECT cursor FROM jmap_state WHERE account=?1",
            [account],
            |r| r.get(0),
        )?;
        let pending = store.pending_count(account)? > 0;
        let mut report = SyncReport::default();
        let mut state;
        let mut ids = BTreeSet::new();
        let mut removed = BTreeSet::new();
        if let Some(cursor) = &cursor {
            state = cursor.clone();
            loop {
                let changes = self
                    .call(
                        "Email/changes",
                        json!({"sinceState":state,"maxChanges":256}),
                    )
                    .await?;
                ensure!(
                    string(&changes, "oldState")? == state,
                    "changes state mismatch"
                );
                for key in ["created", "updated"] {
                    ids.extend(strings(&changes, key)?);
                }
                removed.extend(strings(&changes, "destroyed")?);
                let next = string(&changes, "newState")?.to_owned();
                ensure!(
                    changes["hasMoreChanges"] != true || next != state,
                    "changes made no progress"
                );
                state = next;
                if !changes["hasMoreChanges"]
                    .as_bool()
                    .context("missing hasMoreChanges")?
                {
                    break;
                }
            }
        } else {
            // Snapshot state before enumeration: concurrent changes replay next sync.
            state = string(
                &self
                    .call("Email/get", json!({"ids":[],"properties":["id"]}))
                    .await?,
                "state",
            )?
            .to_owned();
            let filter = match &settings {
                Some(settings) => {
                    json!({"operator":"AND","conditions":[{"inMailbox":inbox},{"operator":"NOT","conditions":[{"inMailbox":settings}]}]})
                }
                None => json!({"inMailbox":inbox}),
            };
            ids.extend(self.query_ids(filter).await?);
        }
        if cursor.is_none() {
            let known = store
                .conn()
                .prepare(
                    "SELECT jmap_id FROM jmap_messages WHERE account=?1 AND local_id IS NOT NULL",
                )?
                .query_map([account], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            removed.extend(known.into_iter().filter(|id| !ids.contains(id)));
        }
        for id in &removed {
            ids.remove(id);
        }
        let ids: Vec<_> = ids.into_iter().collect();
        for batch in ids.chunks(64) {
            let result = self.call("Email/get", json!({"ids":batch,"properties":["id","blobId","mailboxIds","messageId","threadId","from","to","cc","subject","receivedAt","keywords","hasAttachment","textBody","htmlBody","bodyValues"],"fetchAllBodyValues":true,"maxBodyValueBytes":1048576})).await?;
            removed.extend(strings(&result, "notFound")?);
            let list = result["list"].as_array().context("missing email list")?;
            let mut returned = BTreeSet::new();
            for email in list {
                let id = string(email, "id")?;
                ensure!(
                    batch.iter().any(|requested| requested == id) && returned.insert(id.to_owned()),
                    "unrequested/duplicate email"
                );
                let membership = email["mailboxIds"]
                    .as_object()
                    .context("missing mailboxIds")?;
                if membership.get(inbox) != Some(&json!(true))
                    || settings
                        .as_ref()
                        .is_some_and(|id| membership.get(id) == Some(&json!(true)))
                {
                    removed.insert(id.to_owned());
                    continue;
                }
                let existing: Option<i64> = store.conn().query_row("SELECT m.id FROM jmap_messages j JOIN messages m ON m.account=j.account AND m.folder='JMAP/Inbox' AND m.uid=j.uid WHERE j.account=?1 AND j.jmap_id=?2",params![account,id],|r|r.get(0)).optional()?;
                if existing.is_some() && pending {
                    report.deferred += 1;
                    continue;
                }
                let raw = self.download(email).await?;
                import(store, account, email, raw.as_deref())?;
                report.imported += 1;
            }
            for id in strings(&result, "notFound")? {
                ensure!(batch.contains(&id), "unrequested notFound");
                returned.insert(id);
            }
            ensure!(
                returned.len() == batch.len(),
                "incomplete Email/get response"
            );
        }
        for id in removed {
            let exists: bool = store.conn().query_row("SELECT EXISTS(SELECT 1 FROM jmap_messages j JOIN messages m ON m.account=j.account AND m.folder='JMAP/Inbox' AND m.uid=j.uid WHERE j.account=?1 AND j.jmap_id=?2)",params![account,id],|r|r.get(0))?;
            if !exists {
                continue;
            }
            if pending {
                report.deferred += 1;
                continue;
            }
            let tx = store.conn().unchecked_transaction()?;
            tx.execute("DELETE FROM messages_fts WHERE rowid IN (SELECT m.id FROM jmap_messages j JOIN messages m ON m.account=j.account AND m.folder='JMAP/Inbox' AND m.uid=j.uid WHERE j.account=?1 AND j.jmap_id=?2)",params![account,id])?;
            report.deleted += tx.execute("DELETE FROM messages WHERE account=?1 AND id IN (SELECT m.id FROM jmap_messages j JOIN messages m ON m.account=j.account AND m.folder='JMAP/Inbox' AND m.uid=j.uid WHERE j.account=?1 AND j.jmap_id=?2)",params![account,id])?;
            tx.execute(
                "UPDATE jmap_messages SET local_id=NULL WHERE account=?1 AND jmap_id=?2",
                params![account, id],
            )?;
            tx.commit()?;
        }
        if report.deferred == 0 {
            store.conn().execute(
                "UPDATE jmap_state SET cursor=?2 WHERE account=?1",
                params![account, state],
            )?;
        }
        Ok(report)
    }
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub imported: usize,
    pub deleted: usize,
    pub deferred: usize,
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn strings(v: &Value, key: &str) -> Result<Vec<String>> {
    v[key]
        .as_array()
        .with_context(|| format!("missing array {key}"))?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("invalid ID"))
        .collect()
}
fn schema(store: &Store) -> Result<()> {
    store.conn().execute_batch("CREATE TABLE IF NOT EXISTS jmap_messages(account INTEGER NOT NULL,jmap_id TEXT NOT NULL,local_id INTEGER,uid INTEGER NOT NULL,PRIMARY KEY(account,jmap_id),UNIQUE(account,uid));
    CREATE TABLE IF NOT EXISTS jmap_state(account INTEGER PRIMARY KEY,provider TEXT NOT NULL,cursor TEXT,next_uid INTEGER NOT NULL DEFAULT 1);")?;
    Ok(())
}
fn import(store: &mut Store, account: AccountId, email: &Value, raw: Option<&[u8]>) -> Result<()> {
    let id = string(email, "id")?;
    // Reserve UID before ingestion: a crash reuses the same Store identity.
    let tx = store.conn().unchecked_transaction()?;
    tx.execute("INSERT OR IGNORE INTO jmap_messages(account,jmap_id,uid) SELECT ?1,?2,next_uid FROM jmap_state WHERE account=?1",params![account,id])?;
    tx.execute("UPDATE jmap_state SET next_uid=MAX(next_uid,(SELECT MAX(uid)+1 FROM jmap_messages WHERE account=?1)) WHERE account=?1",[account])?;
    let uid: u32 = tx.query_row(
        "SELECT uid FROM jmap_messages WHERE account=?1 AND jmap_id=?2",
        params![account, id],
        |r| r.get(0),
    )?;
    tx.commit()?;
    let keywords = email["keywords"].as_object().context("missing keywords")?;
    let has = |k: &str| keywords.get(k).and_then(Value::as_bool) == Some(true);
    let destination = keywords.keys().find_map(|k| {
        if has(k) {
            Destination::from_keyword(k)
        } else {
            None
        }
    });
    let from = Address::new(email["from"][0]["email"].as_str().unwrap_or(""));
    let destination = store.route(
        account,
        &from,
        if store.local_views()? {
            None
        } else {
            destination
        },
    )?;
    let parts = email["textBody"]
        .as_array()
        .filter(|a| !a.is_empty())
        .or_else(|| email["htmlBody"].as_array())
        .context("missing body parts")?;
    let mut body = String::new();
    for part in parts {
        let value = &email["bodyValues"][string(part, "partId")?];
        ensure!(
            !value["isTruncated"].as_bool().unwrap_or(false)
                && !value["isEncodingProblem"].as_bool().unwrap_or(false),
            "incomplete body; cursor retained"
        );
        body.push_str(string(value, "value")?);
        body.push('\n');
    }
    let fallback = format!("<jmap:{id}@munkimail.local>");
    let message_id = email["messageId"][0].as_str().unwrap_or(&fallback);
    let date = chrono::DateTime::parse_from_rfc3339(string(email, "receivedAt")?)?.timestamp();
    let local = store.upsert_message(
        &NewMessage {
            account,
            folder: FOLDER,
            uid,
            message_id,
            thread_id: string(email, "threadId")?,
            from,
            subject: string(email, "subject")?,
            date,
            seen: has("$seen"),
            has_attachment: email["hasAttachment"]
                .as_bool()
                .context("missing attachment flag")?,
            destination,
        },
        &body,
        "",
    )?;
    let generated;
    let mime = if let Some(raw) = raw {
        raw
    } else {
        generated = mime_message(email, message_id, date)?;
        &generated
    };
    store.cache_message(local, mime)?;
    if !store.local_views()? {
        store.server_stacks(
            account,
            local,
            has(munki_core::triage::Stack::ReplyLater.keyword()),
            has(munki_core::triage::Stack::SetAside.keyword()),
        )?;
    }
    store.conn().execute(
        "UPDATE jmap_messages SET local_id=?3 WHERE account=?1 AND jmap_id=?2",
        params![account, id, local],
    )?;
    Ok(())
}

// ponytail: reconstructed MIME omits attachments; session downloadUrl supplies full originals.
fn mime_message(email: &Value, message_id: &str, date: i64) -> Result<Vec<u8>> {
    let addresses = |key: &str| -> Vec<String> {
        email[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["email"].as_str().map(str::to_owned))
            .collect()
    };
    for key in ["from", "to", "cc"] {
        for address in addresses(key) {
            ensure!(
                !address.chars().any(char::is_control),
                "invalid address header"
            );
        }
    }
    let subject = string(email, "subject")?;
    ensure!(
        !subject.chars().any(char::is_control) && !message_id.chars().any(char::is_control),
        "invalid MIME header"
    );
    let mut builder = mail_builder::MessageBuilder::new()
        .from(addresses("from"))
        .to(addresses("to"))
        .cc(addresses("cc"))
        .subject(subject)
        .message_id(message_id.trim_matches(['<', '>']))
        .date(date);
    for key in ["textBody", "htmlBody"] {
        let mut text = String::new();
        for part in email[key].as_array().into_iter().flatten() {
            text.push_str(string(
                &email["bodyValues"][string(part, "partId")?],
                "value",
            )?);
            text.push('\n');
        }
        if key == "textBody" {
            builder = builder.text_body(text);
        } else if !text.is_empty() {
            builder = builder.html_body(text);
        }
    }
    Ok(builder.write_to_vec()?)
}

async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "JMAP response exceeds {limit} bytes"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= limit - body.len(),
            "JMAP response exceeds {limit} bytes"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn mailbox_path(boxes: &[Value], name: &str) -> Result<Option<String>> {
    let direct: Vec<_> = boxes
        .iter()
        .filter(|m| m["name"] == name && m["parentId"].is_null())
        .collect();
    ensure!(direct.len() <= 1, "ambiguous mailbox name");
    if let Some(m) = direct.first() {
        return Ok(Some(string(m, "id")?.to_owned()));
    }
    let mut parent: Option<String> = None;
    for component in name.split('/') {
        let matches: Vec<_> = boxes
            .iter()
            .filter(|m| m["name"] == component && m["parentId"].as_str() == parent.as_deref())
            .collect();
        ensure!(matches.len() <= 1, "ambiguous mailbox path");
        let Some(m) = matches.first() else {
            return Ok(None);
        };
        parent = Some(string(m, "id")?.to_owned());
    }
    Ok(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[tokio::test]
    async fn chunked_response_limit_enforced() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n5\r\n67890\r\n0\r\n\r\n").unwrap();
        });
        let response = reqwest::get(endpoint).await.unwrap();
        assert!(bounded(response, 8)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
        server.join().unwrap();
    }
}
