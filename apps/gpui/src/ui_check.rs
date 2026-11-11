//! Native-window smoke check; captures only this application's rendered frames.
use super::*;
use gpui::{
    point, AsyncApp, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, WindowHandle,
};
use std::time::Duration;

async fn settled(window: WindowHandle<MailApp>, cx: &mut AsyncApp) -> Result<()> {
    for _ in 0..200 {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        if !window.read_with(cx, |app, _| app.busy || app.command_busy)? {
            let error = window.read_with(cx, |app, _| app.error.clone())?;
            ensure!(error.is_empty(), "UI operation failed: {error}");
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            return Ok(());
        }
    }
    anyhow::bail!("Native mail view did not settle");
}
fn capture(window: WindowHandle<MailApp>, path: PathBuf, cx: &mut AsyncApp) -> Result<()> {
    let handle: gpui::AnyWindowHandle = window.into();
    handle.update(cx, |_, window, cx| {
        window.draw(cx).clear(cx);
        match window.render_to_image() {
            Ok(frame) => frame.save(path)?,
            Err(e) if e.to_string() == "render_to_image not implemented for this platform" => {
                eprintln!("Frame capture unavailable on this GPUI platform; native event assertions still run");
            }
            Err(e) => return Err(e),
        }
        Ok(())
    })?
}
fn click(
    window: WindowHandle<MailApp>,
    button: MouseButton,
    x: f32,
    y: f32,
    cx: &mut AsyncApp,
) -> Result<()> {
    let handle: gpui::AnyWindowHandle = window.into();
    handle.update(cx, |_, window, cx| {
        let position = point(px(x), px(y));
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers: Default::default(),
                click_count: 1,
            }),
            cx,
        );
    })
}
pub fn run(window: WindowHandle<MailApp>, dir: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        let result = async {
            std::fs::create_dir_all(&dir)?;
            settled(window, cx).await?;
            ensure!(
                window.read_with(cx, |app, _| !app.rows.is_empty())?,
                "Demo list is empty"
            );
            capture(window, dir.join("mail-list.png"), cx)?;
            // Dispatch actual GPUI pointer events through Ely's virtualized list.
            click(window, MouseButton::Left, 330., 200., cx)?;
            settled(window, cx).await?;
            ensure!(
                window.read_with(cx, |app, _| app.detail.is_some())?,
                "Message click did not open the reader"
            );
            capture(window, dir.join("mail-reader.png"), cx)?;
            click(window, MouseButton::Right, 85., 85., cx)?;
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            capture(window, dir.join("account-menu.png"), cx)?;
            click(window, MouseButton::Left, 125., 140., cx)?;
            settled(window, cx).await?;
            ensure!(
                window.read_with(cx, |app, _| app.filter.account.is_some())?,
                "Context-menu account selection did not filter mail"
            );
            window.update(cx, |app, w, cx| app.new_draft(w, cx))?;
            capture(window, dir.join("compose.png"), cx)?;
            window.update(cx, |app, _, cx| {
                let e = app.editor.as_ref().expect("compose");
                for (key, text) in [
                    ("to", "test@example.test"),
                    ("subject", "Native parity check"),
                    ("text", "Saved through the native composer."),
                ] {
                    e.fields
                        .iter()
                        .find(|f| f.spec.key == key)
                        .expect("field")
                        .input
                        .clone()
                        .update(cx, |i, cx| i.set_text(text.to_owned(), cx));
                }
            })?;
            cx.background_executor()
                .timer(Duration::from_millis(1800))
                .await;
            settled(window, cx).await?;
            ensure!(
                window.read_with(cx, |app, _| app
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.draft_id.is_some() && !e.dirty))?,
                "Draft autosave failed"
            );
            window.update(cx, |app, w, cx| {
                app.close_editor(w, cx);
                app.change_filter(None, "drafts".into(), cx);
            })?;
            settled(window, cx).await?;
            ensure!(
                window.read_with(cx, |app, _| app
                    .drafts
                    .iter()
                    .any(|d| d["subject"] == "Native parity check"))?,
                "Saved draft missing"
            );
            capture(window, dir.join("drafts.png"), cx)?;
            for view in ["calendar", "people", "settings"] {
                window.update(cx, |app, _, cx| app.change_filter(None, view.into(), cx))?;
                settled(window, cx).await?;
                capture(window, dir.join(format!("{view}.png")), cx)?;
                if view == "calendar" {
                    for mode in ["week", "day", "agenda", "month"] {
                        window.update(cx, |app, _, cx| {
                            app.calendar_mode = mode.into();
                            cx.notify();
                        })?;
                        capture(window, dir.join(format!("calendar-{mode}.png")), cx)?;
                    }
                }
            }
            for view in ["calendar", "people"] {
                window.update(cx, |app, w, cx| {
                    app.change_filter(None, view.into(), cx);
                    if view == "calendar" {
                        app.edit_event(None, w, cx);
                    } else {
                        app.edit_contact(None, w, cx);
                    }
                })?;
                window.update(cx, |app, _, cx| {
                    let e = app.editor.as_ref().expect("editor");
                    let key = if view == "calendar" { "title" } else { "name" };
                    e.fields
                        .iter()
                        .find(|f| f.spec.key == key)
                        .expect("name field")
                        .input
                        .clone()
                        .update(cx, |i, cx| {
                            i.set_text("Native workflow check".to_owned(), cx)
                        });
                })?;
                capture(window, dir.join(format!("{view}-editor.png")), cx)?;
                window.update(cx, |app, w, cx| app.save_editor(false, false, w, cx))?;
                settled(window, cx).await?;
                let item = window
                    .read_with(cx, |app, _| {
                        let (items, key) = if view == "calendar" {
                            (&app.events, "title")
                        } else {
                            (&app.contacts, "name")
                        };
                        items
                            .iter()
                            .find(|v| v[key] == "Native workflow check")
                            .cloned()
                    })?
                    .context("Saved PIM item missing")?;
                window.update(cx, |app, w, cx| {
                    app.mutate(
                        if view == "calendar" {
                            "delete_event"
                        } else {
                            "delete_contact"
                        },
                        item,
                        w,
                        cx,
                    )
                })?;
                settled(window, cx).await?;
                ensure!(
                    window.read_with(cx, |app, _| {
                        let (items, key) = if view == "calendar" {
                            (&app.events, "title")
                        } else {
                            (&app.contacts, "name")
                        };
                        !items.iter().any(|v| v[key] == "Native workflow check")
                    })?,
                    "Deleted item remains"
                );
            }
            window.update(cx, |app, w, cx| app.edit_settings(w, cx))?;
            capture(window, dir.join("preferences.png"), cx)?;
            window.update(cx, |app, w, cx| app.save_editor(false, false, w, cx))?;
            settled(window, cx).await?;
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                eprintln!("Native UI check passed");
                cx.update(|cx| cx.quit());
            }
            Err(e) => {
                eprintln!("Native UI check failed: {e:#}");
                std::process::exit(1);
            }
        }
    })
    .detach();
}
