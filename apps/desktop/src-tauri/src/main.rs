#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod backend;
mod network;
mod pim;
mod runtime;
mod theme;
use anyhow::{ensure, Context, Result};
use backend::Backend;
use runtime::{dispatch_inner, with_backend, AppState};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
#[tauri::command]
async fn dispatch(
    state: tauri::State<'_, Arc<AppState>>,
    action: String,
    payload: Option<Value>,
) -> std::result::Result<Value, String> {
    dispatch_inner(
        state.inner().clone(),
        &action,
        payload.unwrap_or_else(|| json!({})),
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        // Match WaveSink: avoid WebKitGTK's DMA-BUF protocol crash on Wayland.
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            std::env::set_var("GDK_BACKEND", "wayland");
        }
    }
    if std::env::args().any(|a| a == "--self-check") {
        if let Err(error) = self_check() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    let demo = std::env::args().any(|a| a == "--demo");
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(move |app| {
            let path = app
                .path()
                .app_data_dir()?
                .join(if demo { "demo.db" } else { "mail.db" });
            let state = Arc::new(AppState::new(path, demo));
            app.manage(state.clone());
            if !demo {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    interval.tick().await;
                    let mut ticks = 0u64;
                    loop {
                        interval.tick().await;
                        ticks += 1;
                        let accounts = with_backend(state.clone(), |b| {
                            let ids = b
                                .store
                                .conn()
                                .prepare("SELECT account FROM account_connections")?
                                .query_map([], |r| r.get::<_, i64>(0))?
                                .collect::<rusqlite::Result<Vec<_>>>()?;
                            Ok(ids)
                        })
                        .await
                        .unwrap_or_default();
                        for account in accounts {
                            let result =
                                dispatch_inner(state.clone(), "sync", json!({"account":account}))
                                    .await;
                            if result
                                .as_ref()
                                .is_ok_and(|value| value["fetched"].as_u64().unwrap_or(0) > 0)
                            {
                                let notify = with_backend(state.clone(), |b| {
                                    Ok(b.settings()?["notifications"].as_bool().unwrap_or(false))
                                })
                                .await
                                .unwrap_or(false);
                                if notify {
                                    let _ = handle
                                        .notification()
                                        .builder()
                                        .title("MunkiMail")
                                        .body("New mail is ready when you are.")
                                        .show();
                                }
                            }
                            let status = match result {
                                Ok(result) => json!({"account":account,"result":result}),
                                Err(error) => json!({"account":account,"error":error.to_string()}),
                            };
                            let _ = handle.emit("sync-status", status);
                        }
                        if ticks.is_multiple_of(5) {
                            let _ = dispatch_inner(state.clone(), "dav_sync", json!({})).await;
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch, theme::get_omarchy_theme])
        .run(tauri::generate_context!())
        .expect("MunkiMail could not start");
}

fn self_check() -> Result<()> {
    let directory = std::env::temp_dir().join(format!("munkimail-check-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let result = (|| {
        let mut backend = Backend::open(directory.join("demo.db"), true)?;
        let bootstrap = backend.local("bootstrap", json!({}))?;
        let account = bootstrap["accounts"][0]["id"]
            .as_i64()
            .context("Demo account missing")?;
        let list = backend.local("messages", json!({"account":account,"view":"imbox"}))?;
        ensure!(list["total"].as_u64().unwrap_or(0) > 0, "Messages missing");
        let id = list["messages"][0]["id"].as_i64().unwrap();
        ensure!(
            backend.local("message", json!({"account":account,"id":id}))?["text"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "Reader body missing"
        );
        backend.local(
            "rename_subject",
            json!({"account":account,"id":id,"subject":"Smoke check"}),
        )?;
        ensure!(
            backend.local("messages", json!({"account":account,"query":"Smoke"}))?["total"] == 1,
            "Renamed search failed"
        );
        let contacts = backend.local("contacts", json!({}))?;
        ensure!(
            !contacts["contacts"].as_array().unwrap().is_empty(),
            "Contacts missing"
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        ensure!(
            !backend.local("events", json!({"start":now,"end":now+7*86400}))?["events"]
                .as_array()
                .unwrap()
                .is_empty(),
            "Calendar missing"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(directory)?;
    result?;
    println!("MunkiMail self-check passed: message reader, triage search, calendar, contacts.");
    Ok(())
}
