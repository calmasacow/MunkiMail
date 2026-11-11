use crate::{
    backend::{account, field, AccountConfig, Backend, SERVICE},
    network,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;
pub(crate) struct PendingAuthorization {
    pending: Option<munki_auth::oauth::Pending>,
    config: AccountConfig,
    credential_key: String,
    expires: tokio::time::Instant,
}

pub(crate) struct AppState {
    backend: Mutex<Option<Backend>>,
    path: PathBuf,
    demo: bool,
    oauth: Mutex<HashMap<i64, PendingAuthorization>>,
    credential_lock: Mutex<()>,
    syncing: Mutex<HashSet<i64>>,
}
impl AppState {
    pub(crate) fn new(path: PathBuf, demo: bool) -> Self {
        Self {
            backend: Mutex::new(None),
            path,
            demo,
            oauth: Mutex::new(HashMap::new()),
            credential_lock: Mutex::new(()),
            syncing: Mutex::new(HashSet::new()),
        }
    }
}
pub(crate) async fn with_backend<R: Send + 'static>(
    state: Arc<AppState>,
    f: impl FnOnce(&mut Backend) -> Result<R> + Send + 'static,
) -> Result<R> {
    tokio::task::spawn_blocking(move || {
        let mut guard = state.backend.blocking_lock();
        if guard.is_none() {
            *guard = Some(Backend::open(state.path.clone(), state.demo)?);
        }
        f(guard.as_mut().context("Mail store unavailable")?)
    })
    .await?
}

pub(crate) async fn dispatch_inner(state: Arc<AppState>, action: &str, p: Value) -> Result<Value> {
    match action {
        "sync" | "sync_folder" => {
            let folder = if action == "sync_folder" {
                let name: String = field(&p, "folder")?;
                ensure!(
                    !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control),
                    "Invalid folder name"
                );
                Some(name)
            } else {
                None
            };
            let id = account(&p)?;
            if state.demo {
                return Ok(json!({"fetched":0,"applied":0,"pending":0,"demo":true}));
            }
            {
                let mut active = state.syncing.lock().await;
                ensure!(active.insert(id), "This account is already syncing");
            }
            tokio::spawn(async move {
                let result = async {
                    let _credentials = state.credential_lock.lock().await;
                    let (path, key, config) = with_backend(state.clone(), move |b| {
                        Ok((b.path.clone(), b.key.clone(), b.config(id)?))
                    })
                    .await?;
                    network::sync(path, key, config, id, folder).await
                }
                .await;
                state.syncing.lock().await.remove(&id);
                result
            })
            .await?
        }

        "dav_sync" => {
            ensure!(!state.demo, "DAV connections are disabled in demo mode");
            {
                let mut active = state.syncing.lock().await;
                ensure!(
                    active.insert(-1),
                    "Calendar and contacts are already syncing"
                );
            }
            let copy = state.clone();
            let result = async {
                let (path, key, settings) =
                    with_backend(copy, |b| Ok((b.path.clone(), b.key.clone(), b.settings()?)))
                        .await?;
                tokio::task::spawn_blocking(move || network::dav(path, key, settings)).await?
            }
            .await;
            state.syncing.lock().await.remove(&-1);
            result
        }
        "auth_start" => {
            ensure!(!state.demo, "Account connections are disabled in demo mode");
            let id = account(&p)?;
            let mut flows = state.oauth.lock().await;
            ensure!(
                !flows.contains_key(&id),
                "Sign-in already pending for this account"
            );
            let config = with_backend(state.clone(), move |b| {
                let mut config = b.config(id)?;
                if let Some(provider) = p["provider"].as_str() {
                    config.oauth_provider = provider.into();
                }
                if let Some(client_id) = p["client_id"].as_str() {
                    config.oauth_client_id = client_id.into();
                }
                config.auth_kind = "oauth2".into();
                Ok(config)
            })
            .await?;
            let oauth = network::oauth_config(&config)?;
            let expires = tokio::time::Instant::now() + oauth.timeout;
            let (url, pending) = munki_auth::oauth::begin(oauth).await?;
            let link = url.to_string();
            tokio::task::spawn_blocking(move || open::that(link)).await??;
            flows.insert(
                id,
                PendingAuthorization {
                    pending: Some(pending),
                    credential_key: format!("{}:{id}", config.oauth_provider),
                    config,
                    expires,
                },
            );
            let cleanup = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(expires).await;
                let mut flows = cleanup.oauth.lock().await;
                if flows
                    .get(&id)
                    .is_some_and(|flow| flow.expires == expires && flow.pending.is_some())
                {
                    flows.remove(&id);
                }
            });
            Ok(json!({"url":url.to_string()}))
        }
        "auth_complete" => {
            let id = account(&p)?;
            let (pending, config, credential_key) = {
                let mut flows = state.oauth.lock().await;
                let flow = flows.get_mut(&id).context("Start sign-in first")?;
                let pending = flow
                    .pending
                    .take()
                    .context("Sign-in completion already running")?;
                (pending, flow.config.clone(), flow.credential_key.clone())
            };
            // Complete independently of the IPC caller so cancellation cannot leave
            // an account reserved or release serialization during keyring writes.
            tokio::spawn(async move {
                let result = async {
                    let tokens = pending.complete().await?;
                    let _credentials = state.credential_lock.lock().await;
                    with_backend(state.clone(), move |b| {
                        // update_account is excluded while this flow exists.
                        let tx = b.store.conn().unchecked_transaction()?;
                        ensure!(
                            tx.execute(
                                "UPDATE account_connections SET config=?2 WHERE account=?1",
                                rusqlite::params![id, serde_json::to_string(&config)?],
                            )? == 1,
                            "Account connection no longer exists"
                        );
                        tokens.save(&format!("{SERVICE}.oauth"), &credential_key)?;
                        tx.commit()?;
                        Ok(json!({"authenticated":true}))
                    })
                    .await
                }
                .await;
                state.oauth.lock().await.remove(&id);
                result
            })
            .await?
        }
        "update_account" => {
            let id = account(&p)?;
            tokio::spawn(async move {
                // Keep exclusion until blocking persistence finishes, even if IPC is cancelled.
                let flows = state.oauth.lock().await;
                ensure!(
                    !flows.contains_key(&id),
                    "Finish sign-in before updating this account"
                );
                let _credentials = state.credential_lock.lock().await;
                let result =
                    with_backend(state.clone(), move |b| b.local("update_account", p)).await;
                drop(flows);
                result
            })
            .await?
        }

        "ai" => {
            ensure!(
                !state.demo,
                "External AI requests are disabled in demo mode"
            );
            let (settings, key) = with_backend(state.clone(), |b| {
                Ok((b.settings()?, munki_auth::secret::get(SERVICE, "ai-key")?))
            })
            .await?;
            let (result, activity) = network::ai(settings, key, p).await;
            with_backend(state, move |b| {
                for entry in activity {
                    b.store.conn().execute(
                        "INSERT INTO ai_activity(entry) VALUES(?1)",
                        rusqlite::params![serde_json::to_string(&entry)?],
                    )?;
                }
                Ok(())
            })
            .await?;
            result
        }
        "open_url" => {
            let url: String = field(&p, "url")?;
            ensure!(url.len() < 4096, "URL too long");
            let parsed = url::Url::parse(&url)?;
            ensure!(
                ["https", "http", "mailto"].contains(&parsed.scheme())
                    && parsed.username().is_empty()
                    && parsed.password().is_none(),
                "Only ordinary web or email links can be opened"
            );
            tokio::task::spawn_blocking(move || open::that(url)).await??;
            Ok(json!({"ok":true}))
        }
        _ => {
            let action = action.to_owned();
            with_backend(state, move |b| b.local(&action, p)).await
        }
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[tokio::test]
    async fn pending_flow_excludes_reentry_and_update_then_releases_on_failure() {
        let state = Arc::new(AppState::new(PathBuf::new(), false));
        let config: AccountConfig = serde_json::from_value(json!({
            "email": "test@example.test", "auth_kind": "oauth2",
            "oauth_provider": "gmail", "oauth_client_id": "test-client"
        }))
        .unwrap();
        let mut oauth = network::oauth_config(&config).unwrap();
        oauth.timeout = std::time::Duration::from_millis(20);
        let (_, pending) = munki_auth::oauth::begin(oauth).await.unwrap();
        state.oauth.lock().await.insert(
            1,
            PendingAuthorization {
                pending: Some(pending),
                config,
                credential_key: "gmail:1".into(),
                expires: tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            },
        );
        for action in ["auth_start", "update_account"] {
            assert!(dispatch_inner(state.clone(), action, json!({"account":1}))
                .await
                .is_err());
        }
        // A completion marker also excludes a new sign-in until cleanup.
        let pending = state
            .oauth
            .lock()
            .await
            .get_mut(&1)
            .unwrap()
            .pending
            .take()
            .unwrap();
        assert!(
            dispatch_inner(state.clone(), "auth_start", json!({"account":1}))
                .await
                .is_err()
        );
        state.oauth.lock().await.get_mut(&1).unwrap().pending = Some(pending);
        assert!(
            dispatch_inner(state.clone(), "auth_complete", json!({"account":1}))
                .await
                .is_err()
        );
        assert!(!state.oauth.lock().await.contains_key(&1));
        assert!(state.credential_lock.try_lock().is_ok());
    }
}
