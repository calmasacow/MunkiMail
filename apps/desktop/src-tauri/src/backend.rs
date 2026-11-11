use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use munki_core::{compose::Draft, query, triage::Stack, Address, Destination, SenderVerdict};
use munki_store::{NewMessage, Store};
use rusqlite::{params, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const SERVICE: &str = "com.munkimail.desktop";

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub display: String,
    #[serde(default = "imap_kind")]
    pub transport: String,
    #[serde(default)]
    pub jmap_url: String,
    #[serde(default)]
    pub imap_host: String,
    #[serde(default = "imap_port")]
    pub imap_port: u16,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_implicit: bool,
    #[serde(default = "sent_folder")]
    pub sent_folder: String,
    #[serde(default = "password_kind")]
    pub auth_kind: String,
    #[serde(default)]
    pub oauth_provider: String,
    #[serde(default)]
    pub oauth_client_id: String,
}
fn imap_kind() -> String {
    "imap".into()
}
fn password_kind() -> String {
    "password".into()
}
fn imap_port() -> u16 {
    993
}
fn smtp_port() -> u16 {
    587
}
fn sent_folder() -> String {
    "Sent".into()
}

pub fn field<T: DeserializeOwned>(p: &Value, key: &str) -> Result<T> {
    serde_json::from_value(
        p.get(key)
            .cloned()
            .with_context(|| format!("Missing {key}"))?,
    )
    .with_context(|| format!("Invalid {key}"))
}
pub fn account(p: &Value) -> Result<i64> {
    let id = field(p, "account")?;
    ensure!(id > 0, "Select an account");
    Ok(id)
}
pub fn settings_default() -> Value {
    json!({"theme":"auto","density":"comfortable","layout":"three-pane","attachment_preview":false,"notifications":false,"ai":{"enabled":false,"consent_to_send_contents":false,"provider":"anthropic","endpoint":"https://api.anthropic.com/v1/messages","model":"claude-sonnet-4-5"},"dav":{"calendar_url":"","contacts_url":"","username":""}})
}

pub struct Backend {
    pub store: Store,
    pub path: PathBuf,
    pub key: Option<String>,
    pub demo: bool,
}
impl Backend {
    pub fn open(path: PathBuf, demo: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = if demo {
            None
        } else {
            let bytes=munki_auth::secret::database_key(SERVICE,&format!("database:{}",path.display())).context("Unlock your OS keychain, then retry. An unlocked Secret Service is required on Linux.")?;
            Some(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
        };
        let mut store = Store::open(&path, key.as_deref())?;
        store.enable_local_views()?;
        store.conn().execute_batch("CREATE TABLE IF NOT EXISTS desktop_settings(id INTEGER PRIMARY KEY CHECK(id=1),json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS account_connections(account INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,config TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS ai_activity(id INTEGER PRIMARY KEY,entry TEXT NOT NULL);")?;
        let mut backend = Self {
            store,
            path,
            key,
            demo,
        };
        crate::pim::initialize(&backend)?;
        if demo {
            backend.seed()?;
        }
        Ok(backend)
    }
    pub fn settings(&self) -> Result<Value> {
        let saved: Option<String> = self
            .store
            .conn()
            .query_row("SELECT json FROM desktop_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(saved
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_else(settings_default))
    }
    pub fn config(&self, id: i64) -> Result<AccountConfig> {
        let config: String = self
            .store
            .conn()
            .query_row(
                "SELECT config FROM account_connections WHERE account=?1",
                params![id],
                |r| r.get(0),
            )
            .context("This account has no connection settings")?;
        Ok(serde_json::from_str(&config)?)
    }
    pub fn local(&mut self, action: &str, p: Value) -> Result<Value> {
        match action {
            "bootstrap" => {
                let mut stmt=self.store.conn().prepare("SELECT a.id,a.email,a.display,a.kind,c.config FROM accounts a LEFT JOIN account_connections c ON c.account=a.id ORDER BY a.id")?;
                let accounts=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(|(id,email,display,kind,config)|json!({"id":id,"email":email,"display":display,"kind":kind,"config":config.and_then(|s|serde_json::from_str::<Value>(&s).ok())})).collect::<Vec<_>>();
                Ok(json!({"accounts":accounts,"demo":self.demo,"settings":self.settings()?}))
            }
            "folders" => {
                let owner = account(&p)?;
                let rows=self.store.conn().prepare("SELECT name,delimiter,selectable FROM server_folders WHERE account=?1 ORDER BY name COLLATE NOCASE")?.query_map([owner],|r|Ok(json!({"account":owner,"name":r.get::<_,String>(0)?,"delimiter":r.get::<_,Option<String>>(1)?,"selectable":r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(json!({"folders":rows}))
            }
            "messages" => self.messages(&p),
            "message" => self.read(&p),
            "triage" => {
                let account = account(&p)?;
                let choices: Vec<Value> = field(&p, "senders")?;
                ensure!(choices.len() <= 500, "Screen at most 500 senders at once");
                let decisions = choices
                    .iter()
                    .map(|choice| {
                        let address: String = field(choice, "address")?;
                        let destination: String = field(choice, "destination")?;
                        let verdict = if destination == "dismiss" {
                            SenderVerdict::Dismiss
                        } else {
                            SenderVerdict::Allow(serde_json::from_value(Value::String(
                                destination,
                            ))?)
                        };
                        Ok((Address::new(&address), verdict))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(json!({"moved":self.store.decide_senders(account,&decisions)?}))
            }
            "mark_seen" => {
                let account = account(&p)?;
                let id = field(&p, "id")?;
                self.store.message(account, id)?;
                self.store
                    .mark_seen(id, p["seen"].as_bool().unwrap_or(true))?;
                Ok(json!({"ok":true}))
            }
            "set_stack" => {
                let stack: Stack = field(&p, "stack")?;
                self.store.set_stack(
                    account(&p)?,
                    field(&p, "id")?,
                    stack,
                    field(&p, "enabled")?,
                )?;
                Ok(json!({"ok":true}))
            }
            "rename_subject" => {
                let id = field(&p, "id")?;
                self.store.message(account(&p)?, id)?;
                let subject: Option<String> = field(&p, "subject")?;
                self.store.rename_subject(id, subject.as_deref())?;
                Ok(json!({"ok":true}))
            }
            "merge_threads" => {
                let account = account(&p)?;
                let source = if let Some(id) = p["source"].as_i64() {
                    self.store.message(account, id)?.thread_id
                } else {
                    field(&p, "source")?
                };
                let target = if let Some(id) = p["target"].as_i64() {
                    self.store.message(account, id)?.thread_id
                } else {
                    field(&p, "target")?
                };
                Ok(json!({"moved":self.store.merge_threads(account,&source,&target)?}))
            }
            "reply" => {
                let account = account(&p)?;
                let id = field(&p, "id")?;
                let m = self.store.message(account, id)?;
                let raw = self
                    .store
                    .raw_message(account, id)?
                    .context("Message has no cached MIME; sync the account first")?;
                let parsed = munki_net::parse::parse(&raw, &m.message_id, m.date)
                    .context("Cannot parse this message")?;
                let from: String = self.store.conn().query_row(
                    "SELECT email FROM accounts WHERE id=?1",
                    params![account],
                    |r| r.get(0),
                )?;
                Ok(serde_json::to_value(
                    if p["forward"].as_bool().unwrap_or(false) {
                        munki_net::compose::forward(&from, &parsed)
                    } else {
                        munki_net::compose::reply(&from, &parsed)
                    },
                )?)
            }
            "drafts" => {
                let mut stmt=self.store.conn().prepare("SELECT id,content,state,updated_at FROM drafts WHERE account=?1 ORDER BY updated_at DESC")?;
                let rows=stmt.query_map(params![account(&p)?],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(|(id,s,state,updated_at)|{let draft:Draft=serde_json::from_str(&s)?;Ok(json!({"id":id,"subject":draft.subject,"to":draft.to,"updated_at":updated_at,"state":state}))}).collect::<Result<Vec<_>>>()?;
                Ok(json!({"drafts":rows}))
            }
            "draft" => Ok(serde_json::to_value(
                self.store.draft(account(&p)?, field(&p, "id")?)?,
            )?),
            "save_draft" => {
                let account = account(&p)?;
                let draft: Draft = field(&p, "draft")?;
                ensure!(
                    serde_json::to_vec(&draft)?.len() <= 32 * 1024 * 1024,
                    "Draft exceeds 32 MiB"
                );
                let email: String = self.store.conn().query_row(
                    "SELECT email FROM accounts WHERE id=?1",
                    params![account],
                    |r| r.get(0),
                )?;
                ensure!(
                    draft.from.eq_ignore_ascii_case(&email),
                    "Sender must match the selected account"
                );
                Ok(json!({"id":self.store.save_draft(account,p["id"].as_i64(),&draft)?}))
            }
            "queue_draft" => {
                let account = account(&p)?;
                ensure!(!self.demo, "Demo mode saves drafts but cannot send email");
                let config = self.config(account)?;
                munki_net::compose::queue(
                    &mut self.store,
                    account,
                    field(&p, "id")?,
                    &config.sent_folder,
                )?;
                Ok(json!({"queued":true}))
            }
            "download_attachment" => {
                let account = account(&p)?;
                let id = field(&p, "id")?;
                let index: usize = field(&p, "index")?;
                let raw = self
                    .store
                    .raw_message(account, id)?
                    .context("No cached attachments")?;
                let parsed = mail_parser::MessageParser::new()
                    .parse(&raw)
                    .context("Invalid MIME")?;
                let part = parsed
                    .attachments()
                    .nth(index)
                    .context("Attachment not found")?;
                ensure!(
                    part.decoded_len() <= 32 * 1024 * 1024,
                    "Attachment exceeds 32 MiB"
                );
                let filename = part
                    .attachment_name()
                    .unwrap_or("attachment")
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("attachment");
                Ok(json!({"filename":filename,"bytes":part.decoded().as_ref()}))
            }
            "settings_get" => self.settings(),
            "settings_save" => {
                let mut settings: Value = field(&p, "settings")?;
                ensure!(settings.is_object(), "Invalid settings");
                ensure!(
                    serde_json::to_vec(&settings)?.len() <= 64 * 1024,
                    "Settings too large"
                );
                // Credentials are accepted only through dedicated keychain fields.
                for name in ["password", "api_key", "ai_key", "dav_password"] {
                    settings.as_object_mut().unwrap().remove(name);
                }
                if let Some(ai) = settings.get_mut("ai").and_then(Value::as_object_mut) {
                    for name in ["key", "api_key", "password"] {
                        ai.remove(name);
                    }
                }
                if let Some(dav) = settings.get_mut("dav").and_then(Value::as_object_mut) {
                    dav.remove("password");
                }
                if !self.demo {
                    for (field, key) in [("ai_key", "ai-key"), ("dav_password", "dav-password")] {
                        if let Some(secret) = p[field].as_str().filter(|s| !s.is_empty()) {
                            munki_auth::secret::set(SERVICE, key, secret)?;
                        }
                    }
                }
                self.store.conn().execute("INSERT INTO desktop_settings(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=?1",params![serde_json::to_string(&settings)?])?;
                Ok(json!({"ok":true}))
            }
            "add_account" => self.add_account(p),
            "update_account" => self.update_account(p),
            "pending" => {
                let account = account(&p)?;
                let mut stmt=self.store.conn().prepare("SELECT id,json_extract(kind,'$.kind'),json_extract(kind,'$.seen'),json_extract(kind,'$.folder'),attempts,not_before,leased_at,last_error FROM outbox WHERE account=?1 ORDER BY id LIMIT 1000")?;
                let now = chrono::Utc::now().timestamp();
                let rows=stmt.query_map([account],|r|{
                    let kind:String=r.get(1)?;let seen:Option<bool>=r.get(2)?;let next:i64=r.get(5)?;let leased:Option<i64>=r.get(6)?;
                    let label=match kind.as_str(){"set_seen"=>if seen==Some(true){"Mark message read"}else{"Mark message unread"},"submit"|"send"=>"Send queued message","save_sent"=>"Save Sent copy","append"=>"Save message to server folder","move"=>"Move message","set_destination"=>"Update destination","set_stack"=>"Update stack","put_state"=>"Publish settings",_=>"Server action"};
                    let status=if next==i64::MAX{"Needs review"}else if leased.is_some(){"In progress"}else if next>now{"Waiting to retry"}else{"Queued"};
                    Ok(json!({"id":r.get::<_,i64>(0)?,"action":kind,"label":label,"folder":r.get::<_,Option<String>>(3)?,"attempts":r.get::<_,u32>(4)?,"status":status,"retry_at":if next>now&&next!=i64::MAX {Some(next)}else{None},"last_error":r.get::<_,Option<String>>(7)?}))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(json!({"ops":rows,"total":self.store.pending_count(account)?}))
            }
            "review" => Ok(json!({"ops":self.store.needs_review(account(&p)?)?})),
            "review_resolve" => {
                let account = account(&p)?;
                let id = field(&p, "id")?;
                let decision: String = field(&p, "decision")?;
                ensure!(
                    ["retry", "discard", "sent"].contains(&decision.as_str()),
                    "Invalid review decision"
                );
                let op = self
                    .store
                    .needs_review(account)?
                    .into_iter()
                    .find(|op| op.id == id)
                    .context("Review item not found")?;
                if decision == "retry" {
                    self.store.conn().execute(
                        "UPDATE outbox SET leased_at=NULL,not_before=0 WHERE id=?1 AND account=?2",
                        params![id, account],
                    )?;
                } else if decision == "sent" {
                    if let munki_core::OpKind::Submit { draft_id } = op.kind {
                        self.store.delivered(account, id, draft_id)?;
                    } else {
                        bail!("Only a send can be marked delivered");
                    }
                } else {
                    let tx = self.store.conn().unchecked_transaction()?;
                    if let munki_core::OpKind::Submit { draft_id } = op.kind {
                        ensure!(tx.execute("UPDATE drafts SET state='draft',wire=NULL,message_id=NULL,sent_folder=NULL,updated_at=unixepoch() WHERE id=?1 AND account=?2 AND state='queued'",params![draft_id,account])?==1,"Queued draft missing");
                    }
                    tx.execute(
                        "DELETE FROM outbox WHERE id=?1 AND account=?2",
                        params![id, account],
                    )?;
                    tx.commit()?;
                }
                Ok(json!({"ok":true}))
            }
            "ai_activity" => {
                let mut stmt = self
                    .store
                    .conn()
                    .prepare("SELECT entry FROM ai_activity ORDER BY id DESC LIMIT 100")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(
                    json!({"activities":rows.into_iter().map(|s|serde_json::from_str::<Value>(&s)).collect::<serde_json::Result<Vec<_>>>()?}),
                )
            }
            _ => crate::pim::dispatch(self, action, p),
        }
    }
    fn add_account(&mut self, p: Value) -> Result<Value> {
        ensure!(!self.demo, "Account connections are disabled in demo mode");
        let mut config: AccountConfig = serde_json::from_value(p.clone())?;
        validate_account(&mut config)?;
        let password = p["password"].as_str().unwrap_or("");
        if config.auth_kind == "password" {
            ensure!(!password.is_empty(), "Password or app password required");
        }
        let tx = self.store.conn().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO accounts(email,display,kind,created_at) VALUES(?1,?2,?3,unixepoch())",
            params![config.email, config.display, config.transport],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO account_connections VALUES(?1,?2)",
            params![id, serde_json::to_string(&config)?],
        )?;
        if !password.is_empty() {
            munki_auth::secret::set(SERVICE, &format!("account-{id}"), password)?;
        }
        tx.commit()?;
        Ok(json!({"id":id}))
    }
    fn update_account(&mut self, p: Value) -> Result<Value> {
        ensure!(!self.demo, "Account connections are disabled in demo mode");
        let id = account(&p)?;
        let old = self.config(id)?;
        let mut config: AccountConfig = serde_json::from_value(p.clone())?;
        validate_account(&mut config)?;
        ensure!(
            config.email.eq_ignore_ascii_case(&old.email),
            "Reconnect must retain the account email; add a new account for another identity"
        );
        ensure!(
            config.transport == old.transport,
            "Changing transport requires a new account"
        );
        ensure!(config.auth_kind==old.auth_kind && config.oauth_provider==old.oauth_provider && config.oauth_client_id==old.oauth_client_id,"Change authentication through provider sign-in; reconnect cannot replace OAuth identity settings");
        let password = p["password"].as_str().unwrap_or("");
        let tx = self.store.conn().unchecked_transaction()?;
        ensure!(
            tx.execute(
                "UPDATE accounts SET display=?2 WHERE id=?1",
                params![id, config.display]
            )? == 1,
            "Account not found"
        );
        ensure!(
            tx.execute(
                "UPDATE account_connections SET config=?2 WHERE account=?1",
                params![id, serde_json::to_string(&config)?]
            )? == 1,
            "Connection not found"
        );
        // Empty password intentionally retains the existing keychain entry.
        if !password.is_empty() {
            munki_auth::secret::set(SERVICE, &format!("account-{id}"), password)?;
        }
        tx.commit()?;
        Ok(json!({"id":id,"updated":true}))
    }
    fn messages(&self, p: &Value) -> Result<Value> {
        let account = account(p)?;
        let view = p["view"].as_str().unwrap_or("imbox");
        let limit = p["limit"].as_u64().unwrap_or(200).clamp(1, 500) as usize;
        let offset = p["offset"].as_u64().unwrap_or(0).min(100_000) as usize;
        let query = p["query"].as_str().unwrap_or("");
        let (messages, total) = if !query.trim().is_empty() || view.starts_with("folder:") {
            let mut q = query::parse(query);
            q.folder = view.strip_prefix("folder:").map(str::to_owned);
            let total = self.store.search_count(account, &q)? as usize;
            (
                self.store
                    .search_page(account, &q, limit as u32, offset as u32)?,
                total,
            )
        } else {
            let filter=match view {"reply_later"=>"m.reply_later=1".to_string(),"set_aside"=>"m.set_aside=1".to_string(),"sent"=>"m.folder IN (SELECT json_extract(config,'$.sent_folder') FROM account_connections WHERE account=m.account)".to_string(),"all"=>"1=1".to_string(),"imbox"|"feed"|"paper_trail"|"screener"=>format!("m.folder='INBOX' AND m.destination='{view}'"),_=>bail!("Unknown mail view")};
            let where_clause=format!("FROM messages m LEFT JOIN senders s ON s.account=m.account AND s.address=m.from_addr WHERE m.account=?1 AND {filter} AND (s.verdict IS NULL OR s.verdict<>'dismiss')");
            let total = self.store.conn().query_row(
                &format!("SELECT COUNT(*) {where_clause}"),
                params![account],
                |r| r.get::<_, usize>(0),
            )?;
            let ids = self
                .store
                .conn()
                .prepare(&format!(
                    "SELECT m.id {where_clause} ORDER BY m.date DESC,m.id DESC LIMIT ?2 OFFSET ?3"
                ))?
                .query_map(params![account, limit, offset], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            (
                ids.into_iter()
                    .map(|id| self.store.message(account, id))
                    .collect::<Result<Vec<_>>>()?,
                total,
            )
        };
        let messages = messages
            .into_iter()
            .map(|message| {
                let snippet: String = self
                    .store
                    .conn()
                    .query_row(
                        "SELECT substr(body,1,180) FROM messages_fts WHERE rowid=?1",
                        params![message.id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or_default();
                let suggestion = self.store.suggestion(message.id)?;
                let mut value = serde_json::to_value(message)?;
                value["snippet"] = json!(snippet);
                value["suggestion"] = serde_json::to_value(suggestion)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"messages":messages,"total":total}))
    }
    fn read(&mut self, p: &Value) -> Result<Value> {
        let account = account(p)?;
        let id = field(p, "id")?;
        let mut message = self.store.message(account, id)?;
        if !message.seen {
            self.store.mark_seen(id, true)?;
            message.seen = true;
        }
        let raw = self.store.raw_message(account, id)?;
        let fallback: String = self
            .store
            .conn()
            .query_row(
                "SELECT body FROM messages_fts WHERE rowid=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default();
        let Some(raw) = raw else {
            return Ok(
                json!({"message":message,"text":fallback,"html":null,"blocked":0,"attachments":[]}),
            );
        };
        let parsed = mail_parser::MessageParser::new()
            .parse(&raw)
            .context("Invalid cached message")?;
        let text = parsed
            .body_text(0)
            .map(|s| s.into_owned())
            .unwrap_or(fallback);
        let mut html = parsed.body_html(0).map(|s| s.into_owned());
        let attachments=parsed.attachments().enumerate().map(|(index,part)|json!({"index":index,"filename":part.attachment_name().unwrap_or("attachment"),"content_type":part.content_type().map(mime_type),"size":part.decoded_len()})).collect::<Vec<_>>();
        if let Some(ref mut html) = html {
            for part in parsed.parts() {
                if let (Some(cid), Some(ct)) = (part.content_id(), part.content_type()) {
                    let ct = mime_type(ct);
                    if ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&ct.as_str())
                        && part.decoded_len() <= 4 * 1024 * 1024
                    {
                        let data = base64::engine::general_purpose::STANDARD.encode(part.decoded());
                        *html = html
                            .replace(&format!("cid:{cid}"), &format!("data:{ct};base64,{data}"));
                    }
                }
            }
        }
        let remote = if p["remote"].as_bool().unwrap_or(false) {
            munki_net::render::Remote::Allow
        } else {
            munki_net::render::Remote::Block
        };
        let rendered = html.map(|h| munki_net::render::sanitize(&h, remote));
        let invite = crate::pim::invitation(&parsed).ok();
        let invite_revision = invite
            .as_deref()
            .map(|raw| crate::pim::invite_revision(self, raw))
            .transpose()?
            .flatten();
        Ok(
            json!({"message":message,"text":text,"html":rendered.as_ref().map(|r|&r.html),"blocked":rendered.map(|r|r.blocked).unwrap_or(0),"attachments":attachments,"calendar_invite":invite.is_some(),"invite":invite,"invite_revision":invite_revision}),
        )
    }
    fn seed(&mut self) -> Result<()> {
        let count: i64 = self
            .store
            .conn()
            .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
        if count > 0 {
            return Ok(());
        }
        let account = self
            .store
            .add_account("alex@munkimail.demo", "Alex Morgan", "demo")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        for (index,(from,subject,body,destination)) in [
            ("maya@studio.example","A little room to breathe","Hey Alex,\n\nThe new direction feels right. I've left a few notes in the deck. Let's catch up Thursday at 10?\n\nMaya",Destination::Imbox),
            ("oliver@studio.example","Thursday's design review","The team is ready for the next design review. Bring your sketches and your strongest opinions.\n\nSee you there,\nOliver",Destination::Imbox),
            ("hello@fieldnotes.example","Field Notes — things worth noticing","This week: quieter software, better questions, and the joy of getting outside. A few good things to read when you have a moment.",Destination::Feed),
            ("orders@atelier.example","Your order is on its way","Order #AT-2048 has shipped. Expected delivery: Friday. Your receipt and tracking details are ready.",Destination::PaperTrail),
            ("sam@northstar.example","An introduction from Maya","Maya suggested we connect about the autumn project. I'd love to hear what you're working on.\n\nSam",Destination::Screener),
            ("news@designweekly.example","A weekly dose of good design","A curated digest for curious people. Read it whenever the time feels right.",Destination::Feed),
        ].into_iter().enumerate(){
            let uid=index as u32+1;let message_id=format!("<demo-{uid}@munkimail.demo>");
            let raw=format!("From: {from}\r\nTo: alex@munkimail.demo\r\nSubject: {subject}\r\nMessage-ID: {message_id}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}");
            let id=self.store.upsert_message(&NewMessage{account,folder:"INBOX",uid,message_id:&message_id,thread_id:&message_id,from:Address::new(from),subject,date:now-index as i64*3600,seen:index>1,has_attachment:false,destination},body,"")?;
            self.store.cache_message(id,raw.as_bytes())?;
            if destination==Destination::Screener {self.store.set_suggestion(id,&munki_core::triage::suggest(munki_core::triage::Headers {from,subject,..Default::default()}))?;}
        }
        crate::pim::seed(self)?;
        Ok(())
    }
}

fn mime_type(ct: mail_parser::ContentType<'_>) -> String {
    ct.subtype()
        .map(|sub| format!("{}/{sub}", ct.ctype()))
        .unwrap_or_else(|| ct.ctype().to_owned())
}

fn validate_account(config: &mut AccountConfig) -> Result<()> {
    config.email = munki_pim::normalize_email(&config.email)?;
    munki_net::compose::build(&Draft {
        from: config.email.clone(),
        to: vec![config.email.clone()],
        ..Default::default()
    })?;
    ensure!(
        ["imap", "jmap"].contains(&config.transport.as_str()),
        "Unsupported transport"
    );
    ensure!(
        ["password", "oauth2"].contains(&config.auth_kind.as_str()),
        "Unsupported authentication"
    );
    ensure!(
        !config.display.chars().any(char::is_control),
        "Invalid display name"
    );
    for host in [&config.smtp_host, &config.imap_host] {
        if !host.is_empty() {
            url::Host::parse(host).context("Invalid mail server hostname")?;
            ensure!(
                !host.chars().any(|c| c.is_whitespace() || c.is_control()),
                "Invalid mail server hostname"
            );
        }
    }
    ensure!(!config.smtp_host.is_empty(), "SMTP host required");
    if config.transport == "imap" {
        ensure!(!config.imap_host.is_empty(), "IMAP host required");
    } else {
        let u = url::Url::parse(&config.jmap_url).context("Invalid JMAP URL")?;
        let local = u.host_str() == Some("localhost")
            || u.host_str()
                .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
        ensure!(
            u.host_str().is_some()
                && (u.scheme() == "https" || (u.scheme() == "http" && local))
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none(),
            "JMAP requires HTTPS without URL credentials"
        );
    }
    config.smtp_implicit = match config.smtp_port {
        465 => true,
        587 => false,
        _ => config.smtp_implicit,
    };
    ensure!(
        config.smtp_port > 0 && config.imap_port > 0,
        "Invalid server port"
    );
    ensure!(
        !config.sent_folder.trim().is_empty() && !config.sent_folder.chars().any(char::is_control),
        "Invalid Sent folder"
    );
    if config.auth_kind == "oauth2" {
        ensure!(
            ["gmail", "google", "microsoft", "outlook", "yahoo"]
                .contains(&config.oauth_provider.as_str())
                && !config.oauth_client_id.trim().is_empty(),
            "OAuth provider and client ID required"
        );
    }
    Ok(())
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    #[test]
    fn folder_queries_keep_account_ownership_and_inbox_views_separate() -> Result<()> {
        let mut store = Store::open_in_memory()?;
        store.enable_local_views()?;
        let first = store.add_account("first@test", "First", "imap")?;
        let second = store.add_account("second@test", "Second", "imap")?;
        for account in [first, second] {
            for (uid, folder) in [(1, "INBOX"), (2, "Projects.2026")] {
                store.upsert_message(
                    &munki_store::NewMessage {
                        account,
                        folder,
                        uid,
                        message_id: &format!("{account}.{uid}@test"),
                        thread_id: "thread",
                        from: Address::new("sender@test"),
                        subject: "invoice",
                        date: uid as i64,
                        seen: false,
                        has_attachment: false,
                        destination: Destination::Feed,
                    },
                    "invoice body",
                    "",
                )?;
            }
        }
        let mut backend = Backend {
            store,
            path: PathBuf::new(),
            key: None,
            demo: true,
        };
        let folder = backend.local(
            "messages",
            json!({"account":first,"view":"folder:Projects.2026","query":"invoice"}),
        )?;
        assert_eq!(folder["total"], 1);
        assert_eq!(folder["messages"][0]["account"], first);
        assert_eq!(folder["messages"][0]["folder"], "Projects.2026");
        assert_eq!(
            backend.local("messages", json!({"account":first,"view":"feed"}))?["total"],
            1
        );
        assert_eq!(
            backend.local(
                "messages",
                json!({"account":first,"view":"folder:Projects' OR 1=1--"})
            )?["total"],
            0
        );
        backend.local(
            "triage",
            json!({"account":first,"senders":[{"address":"sender@test","destination":"imbox"}]}),
        )?;
        assert_eq!(backend.store.pending_count(first)?, 0);
        let operation = backend.store.enqueue(
            first,
            &munki_core::OpKind::Append {
                folder: "Sent".into(),
                message_id: "private-message-id".into(),
                staged_path: "private-file-path".into(),
            },
        )?;
        backend
            .store
            .settle_retry(operation, "Sent folder unavailable")?;
        backend
            .store
            .enqueue(second, &munki_core::OpKind::Submit { draft_id: 99 })?;
        let pending = backend.local("pending", json!({"account":first}))?;
        assert_eq!(pending["total"], 1);
        assert_eq!(pending["ops"][0]["label"], "Save message to server folder");
        assert_eq!(pending["ops"][0]["status"], "Waiting to retry");
        assert_eq!(pending["ops"][0]["last_error"], "Sent folder unavailable");
        assert!(!pending.to_string().contains("private-"));

        assert_eq!(
            backend.local("messages", json!({"account":second,"view":"feed"}))?["total"],
            1
        );
        Ok(())
    }
}
