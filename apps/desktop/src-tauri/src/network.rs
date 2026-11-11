use crate::backend::{AccountConfig, SERVICE};
use anyhow::{bail, ensure, Context, Result};
use munki_core::{Destination, OpKind};
use munki_store::Store;
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn oauth_config(config: &AccountConfig) -> Result<munki_auth::oauth::Config> {
    let provider = match config.oauth_provider.as_str() {
        "gmail" | "google" => munki_auth::oauth::Provider::Gmail,
        "yahoo" => munki_auth::oauth::Provider::Yahoo,
        "microsoft" | "outlook" => munki_auth::oauth::Provider::Microsoft {
            tenant: "common".into(),
        },
        _ => bail!("Choose Google, Microsoft, or Yahoo OAuth"),
    };
    let scopes = if matches!(provider, munki_auth::oauth::Provider::Gmail) {
        vec!["https://mail.google.com/".into()]
    } else if matches!(provider, munki_auth::oauth::Provider::Yahoo) {
        vec!["mail-w".into()]
    } else {
        vec![
            "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
            "https://outlook.office.com/SMTP.Send".into(),
        ]
    };
    Ok(munki_auth::oauth::Config {
        provider,
        client_id: config.oauth_client_id.clone(),
        scopes,
        timeout: std::time::Duration::from_secs(180),
    })
}
/// Provider tokens may only reach the preset provider's mail endpoints.
fn validate_oauth_transport(config: &AccountConfig) -> Result<()> {
    if config.auth_kind != "oauth2" {
        return Ok(());
    }
    ensure!(
        config.transport == "imap",
        "Preset-provider OAuth does not support JMAP transport"
    );
    let (imap, smtp): (&[&str], &str) = match config.oauth_provider.as_str() {
        "gmail" | "google" => (&["imap.gmail.com"], "smtp.gmail.com"),
        "yahoo" => (&["imap.mail.yahoo.com"], "smtp.mail.yahoo.com"),
        "microsoft" | "outlook" => (
            &["outlook.office365.com", "outlook.office.com"],
            "smtp.office365.com",
        ),
        _ => bail!("Choose Google, Microsoft, or Yahoo OAuth"),
    };
    ensure!(
        imap.iter()
            .any(|host| config.imap_host.eq_ignore_ascii_case(host)),
        "OAuth IMAP host is not allowed for this provider"
    );
    ensure!(
        config.smtp_host.eq_ignore_ascii_case(smtp),
        "OAuth SMTP host is not allowed for this provider"
    );
    Ok(())
}

async fn credential(config: &AccountConfig, account: i64) -> Result<String> {
    validate_oauth_transport(config)?;
    if config.auth_kind == "oauth2" {
        let oauth = oauth_config(config)?;
        let key = format!("{}:{account}", config.oauth_provider);
        let key_load = key.clone();
        let mut tokens = tokio::task::spawn_blocking(move || {
            munki_auth::oauth::Tokens::load(&format!("{SERVICE}.oauth"), &key_load)
        })
        .await??
        .context("Sign in with your provider first")?;
        if tokens.refresh_if_needed(&oauth).await? {
            let copy = tokens.clone();
            tokio::task::spawn_blocking(move || copy.save(&format!("{SERVICE}.oauth"), &key))
                .await??;
        }
        Ok(tokens.access_token)
    } else {
        tokio::task::spawn_blocking(move || {
            munki_auth::secret::get(SERVICE, &format!("account-{account}"))
        })
        .await??
        .context("Account password missing; update the account settings")
    }
}

pub async fn sync(
    path: PathBuf,
    key: Option<String>,
    mut config: AccountConfig,
    account: i64,
    folder: Option<String>,
) -> Result<Value> {
    validate_oauth_transport(&config)?;
    let secret = credential(&config, account).await?;
    let mut store =
        tokio::task::spawn_blocking(move || Store::open(&path, key.as_deref())).await??;
    store.enable_local_views()?;
    if config.transport == "jmap" {
        ensure!(
            folder.is_none(),
            "Server folder browsing currently requires IMAP"
        );
        return sync_jmap(&mut store, &config, account, &secret).await;
    }
    let client = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        munki_net::imap::connect_tls(&config.imap_host, config.imap_port),
    )
    .await
    .context("IMAP connection timed out")??;
    let auth = if config.auth_kind == "oauth2" && config.oauth_provider == "yahoo" {
        munki_net::imap::Auth::OAuthBearer {
            user: &config.email,
            access_token: &secret,
            host: &config.imap_host,
            port: config.imap_port,
        }
    } else if config.auth_kind == "oauth2" {
        munki_net::imap::Auth::OAuth2 {
            user: &config.email,
            access_token: &secret,
        }
    } else {
        munki_net::imap::Auth::Password {
            user: &config.email,
            pass: &secret,
        }
    };
    let mut session = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        munki_net::imap::login(client, auth),
    )
    .await
    .context("IMAP sign-in timed out")??;
    let result=tokio::time::timeout(std::time::Duration::from_secs(180),async {
        // Stage local server mutations before reconciling fetched flags.
        munki_net::imap::discover_folders(&mut session,&mut store,account).await?;
        munki_net::imap::repair_sent_copies(&store,account)?;
        if let Ok(target)=munki_net::imap::sent_folder(&store,account,&config.sent_folder) {
            if target!=config.sent_folder {
                config.sent_folder=target;
                store.conn().execute("UPDATE account_connections SET config=?2 WHERE account=?1",rusqlite::params![account,serde_json::to_string(&config)?])?;
            }
        }
        let first=munki_net::imap::drain_outbox(&mut session,&mut store,account).await?;
        let target=folder.as_deref().unwrap_or("INBOX");
        let selectable:bool=store.conn().query_row("SELECT EXISTS(SELECT 1 FROM server_folders WHERE account=?1 AND name=?2 AND selectable=1)",rusqlite::params![account,target],|r|r.get(0))?;
        ensure!(selectable,"Server folder is unavailable or not selectable");
        let report=munki_net::imap::sync_folder(&mut session,&mut store,account,target).await?;
        let mut applied=first.applied;
        let pending_sends:i64=store.conn().query_row("SELECT COUNT(*) FROM outbox WHERE account=?1 AND json_extract(kind,'$.kind')='submit' AND leased_at IS NULL AND not_before<=unixepoch()",rusqlite::params![account],|r|r.get(0))?;
        if pending_sends>0 {
            let credentials=if config.auth_kind=="oauth2"&&config.oauth_provider=="yahoo"{mail_send::Credentials::new_oauth(munki_net::imap::oauth_bearer(&config.email,&secret,&config.smtp_host,config.smtp_port)?)}else if config.auth_kind=="oauth2"{mail_send::Credentials::new_xoauth2(config.email.clone(),secret.clone())}else{mail_send::Credentials::new(config.email.clone(),secret.clone())};
            let mut smtp=munki_net::smtp::connect(&config.smtp_host,config.smtp_port,config.smtp_implicit,credentials).await?;
            applied+=munki_net::smtp::drain_outbox(&mut smtp,&mut store,account).await?.applied;
            let _=smtp.quit().await;
            applied+=munki_net::imap::drain_outbox(&mut session,&mut store,account).await?.applied;
        }
        // Sent is optional on accounts that have never sent mail.
        if folder.is_none() && config.sent_folder!="INBOX" && session.examine(&config.sent_folder).await.is_ok() {
            munki_net::imap::sync_folder(&mut session,&mut store,account,&config.sent_folder).await?;
        }
        Ok::<_,anyhow::Error>(json!({"fetched":report.fetched,"applied":applied,"pending":store.pending_count(account)?}))
    }).await.context("Mail sync timed out; queued actions remain saved")?;
    let _ = session.logout().await;
    result
}

async fn sync_jmap(
    store: &mut Store,
    config: &AccountConfig,
    account: i64,
    secret: &str,
) -> Result<Value> {
    let credentials = if config.auth_kind == "oauth2" {
        munki_jmap::Credentials::Bearer(secret.into())
    } else {
        munki_jmap::Credentials::Basic {
            username: config.email.clone(),
            password: secret.into(),
        }
    };
    let client = munki_jmap::Client::discover(&config.jmap_url, credentials, None).await?;
    client.bind_store(store, account)?;
    store.recover_leases(account)?;
    // Release only operations parked by the earlier, temporary transport limits.
    store.conn().execute("UPDATE outbox SET leased_at=NULL,not_before=0,last_error=NULL WHERE account=?1 AND
        ((json_extract(kind,'$.kind')='put_state' AND last_error='Cross-device settings snapshots currently require IMAP; local decisions remain saved') OR
         (json_extract(kind,'$.kind')='save_sent' AND last_error='This operation requires IMAP transport'))",[account])?;
    // Merge remote decisions before replaying local destination/stack keywords.
    if !store.local_views()? {
        let remote = decode_jmap_settings(&client.load_settings().await?)?;
        store.merge_triage(account, &remote)?;
    }
    let mut applied = drain_jmap(&client, store, account).await?;
    let pending_sends:i64=store.conn().query_row("SELECT COUNT(*) FROM outbox WHERE account=?1 AND json_extract(kind,'$.kind')='submit' AND leased_at IS NULL AND not_before<=unixepoch()",[account],|r|r.get(0))?;
    if pending_sends > 0 {
        ensure!(
            !config.smtp_host.is_empty(),
            "Configure SMTP to send queued JMAP drafts"
        );
        let credentials = if config.auth_kind == "oauth2" {
            mail_send::Credentials::new_xoauth2(config.email.clone(), secret.to_owned())
        } else {
            mail_send::Credentials::new(config.email.clone(), secret.to_owned())
        };
        let mut smtp = munki_net::smtp::connect(
            &config.smtp_host,
            config.smtp_port,
            config.smtp_implicit,
            credentials,
        )
        .await?;
        let delivered = munki_net::smtp::drain_outbox(&mut smtp, store, account).await;
        let _ = smtp.quit().await;
        applied += delivered?.applied;
        // SMTP acknowledgement atomically transforms Submit into SaveSent.
        applied += drain_jmap(&client, store, account).await?;
    }
    let report = client.sync(store, account).await?;
    Ok(
        json!({"fetched":report.imported,"deleted":report.deleted,"applied":applied,"pending":store.pending_count(account)?}),
    )
}

fn decode_jmap_settings(messages: &[Vec<u8>]) -> Result<munki_core::state::StateSnapshot> {
    use munki_core::state::{StateSnapshot, MAX_STATE_BYTES};
    let mut remote = StateSnapshot::default();
    for raw in messages {
        ensure!(
            raw.len() <= MAX_STATE_BYTES * 2,
            "oversized triage settings message"
        );
        let parsed = mail_parser::MessageParser::new()
            .parse(raw)
            .context("invalid settings MIME")?;
        let Some(marker) = parsed.headers().get("X-MunkiMail-State") else {
            continue;
        };
        ensure!(
            marker.value().as_text() == Some("1"),
            "unsupported triage settings message version"
        );
        let text = parsed
            .body_text(0)
            .context("settings message has no JSON body")?;
        ensure!(text.len() <= MAX_STATE_BYTES, "triage state exceeds 8 MiB");
        let snapshot: StateSnapshot = serde_json::from_str(&text).context("invalid triage JSON")?;
        snapshot.validate().map_err(anyhow::Error::msg)?;
        remote.merge(snapshot);
        ensure!(
            serde_json::to_vec(&remote)?.len() <= MAX_STATE_BYTES,
            "merged triage state exceeds 8 MiB"
        );
    }
    Ok(remote)
}

fn jmap_settings_message(
    store: &Store,
    account: i64,
    op_id: i64,
    revision: i64,
) -> Result<(String, Vec<u8>)> {
    use base64::Engine;
    let snapshot = store.triage_snapshot(account)?;
    snapshot.validate().map_err(anyhow::Error::msg)?;
    let json = serde_json::to_vec(&snapshot)?;
    ensure!(
        json.len() <= munki_core::state::MAX_STATE_BYTES,
        "triage state exceeds 8 MiB"
    );
    let device: String = store
        .conn()
        .query_row("SELECT id FROM local_device", [], |r| r.get(0))?;
    ensure!(
        device.len() == 32 && device.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid settings device ID"
    );
    // Stable per durable operation, including after an ambiguous import outcome.
    let id = format!("state.{device}.{account}.{op_id}.{revision}@munkimail.local");
    let mut mime=format!("From: settings@munkimail.local\r\nTo: settings@munkimail.local\r\nSubject: MunkiMail settings\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\nX-MunkiMail-State: 1\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n");
    let encoded = base64::engine::general_purpose::STANDARD.encode(json);
    for line in encoded.as_bytes().chunks(76) {
        mime.push_str(std::str::from_utf8(line)?);
        mime.push_str("\r\n");
    }
    Ok((id, mime.into_bytes()))
}

async fn drain_jmap(client: &munki_jmap::Client, store: &mut Store, account: i64) -> Result<usize> {
    let mut applied = 0;
    while let Some(op) = store.lease_transport(account, false)? {
        if !matches!(
            op.kind,
            OpKind::SetSeen { .. }
                | OpKind::SetDestination { .. }
                | OpKind::SetStack { .. }
                | OpKind::PutState { .. }
                | OpKind::SaveSent { .. }
        ) {
            store.settle_needs_review(
                op.id,
                "This queued operation is not supported by JMAP transport",
            )?;
            continue;
        }
        // Every error after leasing releases the lease with durable backoff.
        let result = {
            let store = &mut *store;
            let op = &op;
            async move {
                match &op.kind {
                    OpKind::PutState { version, json } => {
                        ensure!(*version == 1, "unsupported queued settings version");
                        ensure!(
                            json.len() <= munki_core::state::MAX_STATE_BYTES,
                            "oversized queued settings"
                        );
                        let queued: munki_core::state::StateSnapshot = serde_json::from_str(json)?;
                        queued.validate().map_err(anyhow::Error::msg)?;
                        let (id, wire) = jmap_settings_message(
                            store,
                            account,
                            op.id,
                            queued.entries.iter().map(|e| e.revision).max().unwrap_or(0),
                        )?;
                        client
                            .append_to_mailbox("MunkiMail/.state", &id, &wire)
                            .await?;
                    }
                    OpKind::SaveSent { draft_id, folder } => {
                        let (id, wire) = store.draft_wire(account, *draft_id)?;
                        client.append_to_mailbox(folder, &id, &wire).await?;
                    }
                    OpKind::SetSeen { folder, uid, .. }
                    | OpKind::SetDestination { folder, uid, .. }
                    | OpKind::SetStack { folder, uid, .. } => {
                        let local_id = store
                            .find(account, folder, *uid)?
                            .context("queued JMAP message missing locally; operation retained")?;
                        let id = munki_jmap::Client::server_id(store, account, local_id)?
                            .context("JMAP mapping missing; operation retained")?;
                        let keywords = match &op.kind {
                            OpKind::SetSeen { seen, .. } => vec![("$seen", *seen)],
                            OpKind::SetDestination { destination, .. } => {
                                let d = store
                                    .queued_destination(account, folder, *uid)?
                                    .unwrap_or(*destination);
                                [
                                    Destination::Imbox,
                                    Destination::Feed,
                                    Destination::PaperTrail,
                                    Destination::Screener,
                                ]
                                .into_iter()
                                .map(|other| (other.keyword(), other == d))
                                .collect()
                            }
                            OpKind::SetStack { stack, enabled, .. } => vec![(
                                stack.keyword(),
                                store
                                    .queued_stack(account, folder, *uid, *stack)?
                                    .unwrap_or(*enabled),
                            )],
                            _ => unreachable!(),
                        };
                        client.set_keywords(&id, &keywords).await?;
                    }
                    _ => unreachable!(),
                }
                Ok::<(), anyhow::Error>(())
            }
        }
        .await;
        match result {
            Ok(()) => {
                if let OpKind::SaveSent { draft_id, .. } = op.kind {
                    store.saved_sent(account, op.id, draft_id)?;
                } else {
                    store.settle_ok(op.id)?;
                }
                applied += 1;
            }
            Err(error) => {
                store.settle_retry(op.id, &error.to_string())?;
                return Err(error);
            }
        }
    }
    Ok(applied)
}

pub fn dav(path: PathBuf, key: Option<String>, settings: Value) -> Result<Value> {
    let store = Store::open(&path, key.as_deref())?;
    let pim = munki_pim::Store::new(store.conn())?;
    let dav = &settings["dav"];
    let username = dav["username"].as_str().unwrap_or("");
    let password = munki_auth::secret::get(SERVICE, "dav-password")?.unwrap_or_default();
    let client = munki_pim::DavClient::new()?.basic(username, password);
    let mut reports = Vec::new();
    for (kind, field) in [
        (munki_pim::Kind::Calendar, "calendar_url"),
        (munki_pim::Kind::AddressBook, "contacts_url"),
    ] {
        if let Some(url) = dav[field].as_str().filter(|s| !s.is_empty()) {
            let collection = pim
                .collections()?
                .into_iter()
                .find(|c| c.kind == kind)
                .context("Collection missing")?;
            let parsed = url::Url::parse(url)?;
            ensure!(parsed.scheme() == "https", "DAV requires HTTPS");
            // Rebinding populated collections would silently upload private data to another server.
            if let Some(existing) = &collection.url {
                ensure!(
                    existing.trim_end_matches('/') == url.trim_end_matches('/'),
                    "Create a separate collection to change DAV server"
                );
            } else {
                store.conn().execute(
                    "UPDATE pim_collections SET url=?2 WHERE id=?1",
                    rusqlite::params![collection.id, url],
                )?;
            }
            reports.push(serde_json::to_value(client.sync(&pim, &collection.id)?)?);
        }
    }
    ensure!(
        !reports.is_empty(),
        "Configure a CalDAV or CardDAV collection URL in Settings"
    );
    Ok(json!({"reports":reports}))
}

pub async fn ai(
    settings: Value,
    key: Option<String>,
    p: Value,
) -> (Result<Value>, Vec<munki_ai::Activity>) {
    let cfg = &settings["ai"];
    let provider = if cfg["provider"] == "anthropic" {
        munki_ai::Provider::Anthropic
    } else {
        munki_ai::Provider::OpenAiCompatible
    };
    let mut config = munki_ai::Config::new(
        provider,
        cfg["endpoint"].as_str().unwrap_or(""),
        cfg["model"].as_str().unwrap_or(""),
    );
    config.enabled = cfg["enabled"].as_bool().unwrap_or(false);
    config.consent_to_send_contents = cfg["consent_to_send_contents"].as_bool().unwrap_or(false);
    if !config.enabled {
        return (
            Err(anyhow::anyhow!("Enable AI in Settings first")),
            Vec::new(),
        );
    }
    let client = match munki_ai::AiClient::new(config, key.as_deref()) {
        Ok(c) => c,
        Err(e) => return (Err(e.into()), Vec::new()),
    };
    let text = p["text"].as_str().unwrap_or("");
    let result = match p["capability"].as_str() {
        Some("draft_reply") => client
            .draft_reply(text)
            .await
            .map(|text| json!({"text":text})),
        Some("translate_query") => client
            .translate_query(text)
            .await
            .map(|query| json!({"query":query})),
        Some("classify_sender") => client
            .classify_sender(
                p["sender"].as_str().unwrap_or(""),
                p["subject"].as_str().unwrap_or(""),
                text,
            )
            .await
            .map(|s| json!({"destination":s.destination,"reason":s.reason})),
        _ => return (Err(anyhow::anyhow!("Unknown AI capability")), Vec::new()),
    };
    (result.map_err(Into::into), client.take_activity())
}

#[cfg(test)]
mod tests {
    use super::*;
    use munki_core::{Address, SenderVerdict};

    #[tokio::test]
    async fn oauth_rejects_foreign_hosts_before_credentials() -> Result<()> {
        let config: AccountConfig = serde_json::from_value(json!({
            "auth_kind":"oauth2", "oauth_provider":"gmail", "transport":"imap",
            "imap_host":"imap.gmail.com", "smtp_host":"smtp.gmail.com"
        }))?;
        validate_oauth_transport(&config)?;
        for (field, value) in [
            ("imap_host", "foreign.invalid"),
            ("smtp_host", "smtp.gmail.com.foreign.invalid"),
            ("transport", "jmap"),
            ("imap_host", "imap.gmail.com@foreign.invalid"),
        ] {
            let mut value_config = serde_json::to_value(&config)?;
            value_config[field] = json!(value);
            let invalid: AccountConfig = serde_json::from_value(value_config)?;
            let expected = validate_oauth_transport(&invalid).unwrap_err().to_string();
            // credential validates before keyring loads or refresh; sync before Store open.
            assert_eq!(
                credential(&invalid, -1).await.unwrap_err().to_string(),
                expected
            );
            assert_eq!(
                sync(
                    PathBuf::from("/nonexistent/oauth-test/store"),
                    None,
                    invalid,
                    -1,
                    None
                )
                .await
                .unwrap_err()
                .to_string(),
                expected
            );
        }
        let mut yahoo = config.clone();
        yahoo.oauth_provider = "yahoo".into();
        yahoo.imap_host = "imap.mail.yahoo.com".into();
        yahoo.smtp_host = "smtp.mail.yahoo.com".into();
        validate_oauth_transport(&yahoo)?;
        assert_eq!(oauth_config(&yahoo)?.scopes, vec!["mail-w"]);
        yahoo.imap_host = "imap.gmail.com".into();
        assert!(validate_oauth_transport(&yahoo).is_err());
        let mut microsoft = config.clone();
        microsoft.oauth_provider = "microsoft".into();
        microsoft.smtp_host = "smtp.office365.com".into();
        for host in [
            "outlook.office365.com",
            "outlook.office.com",
            "OUTLOOK.OFFICE365.COM",
        ] {
            microsoft.imap_host = host.into();
            validate_oauth_transport(&microsoft)?;
        }
        microsoft.smtp_host = "smtp.gmail.com".into();
        assert!(validate_oauth_transport(&microsoft).is_err());
        Ok(())
    }

    #[test]
    fn jmap_settings_validate_merge_and_write_full_snapshot() -> Result<()> {
        let mut first = Store::open_in_memory()?;
        let account = first.add_account("me@example.org", "Me", "jmap")?;
        first.decide_sender(
            account,
            &Address::new("remote@example.org"),
            SenderVerdict::Allow(Destination::Feed),
        )?;
        let (id, wire) = jmap_settings_message(&first, account, 7, 1)?;
        assert_eq!(jmap_settings_message(&first, account, 7, 1)?.0, id);
        let parsed = mail_parser::MessageParser::new()
            .parse(&wire)
            .context("test MIME")?;
        assert_eq!(parsed.message_id(), Some(id.as_str()));
        let remote = decode_jmap_settings(std::slice::from_ref(&wire))?;
        assert_eq!(remote, first.triage_snapshot(account)?);

        let mut second = Store::open_in_memory()?;
        let second_account = second.add_account("me@example.org", "Me", "jmap")?;
        second.decide_sender(
            second_account,
            &Address::new("local@example.org"),
            SenderVerdict::Allow(Destination::Imbox),
        )?;
        second.merge_triage(second_account, &remote)?;
        let (_, merged_wire) = jmap_settings_message(&second, second_account, 8, 2)?;
        let merged = decode_jmap_settings(&[merged_wire])?;
        assert_eq!(merged.entries.len(), 2);
        assert_eq!(merged, second.triage_snapshot(second_account)?);
        first.merge_triage(account, &merged)?;
        assert_eq!(first.triage_snapshot(account)?, merged);

        let unsupported = String::from_utf8(wire)?
            .replace("X-MunkiMail-State: 1", "X-MunkiMail-State: 2")
            .into_bytes();
        assert!(decode_jmap_settings(&[unsupported]).is_err());
        let invalid_snapshot = b"X-MunkiMail-State: 1\r\nContent-Type: text/plain\r\n\r\n{\"format\":2,\"entries\":[]}".to_vec();
        assert!(decode_jmap_settings(&[invalid_snapshot]).is_err());
        assert!(
            decode_jmap_settings(&[b"Subject: ordinary mail\r\n\r\nignored".to_vec()])?
                .entries
                .is_empty()
        );
        Ok(())
    }
}
