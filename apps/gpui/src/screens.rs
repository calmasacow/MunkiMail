use super::*;
use ely_calendar::{AgendaView, CalendarDayView, CalendarMonthView, CalendarWeekView};
use forms::{Kind, Spec};
use gpui::PathPromptOptions;
use std::io::{Read, Write};

impl MailApp {
    pub fn command(
        &mut self,
        action: &'static str,
        payload: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.command_busy {
            self.error = "Wait for the current operation to finish.".into();
            cx.notify();
            return;
        }
        self.command_busy = true;
        self.error.clear();
        let worker = self.worker.clone();
        let task = cx
            .background_executor()
            .spawn(async move { worker.call(action, payload) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.command_busy = false;
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(e) => {
                        this.error = format!("{e:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub fn mutate(
        &mut self,
        action: &'static str,
        payload: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command(action, payload, window, cx, |this, _, _, cx| {
            this.notice = "Updated".into();
            this.run(Operation::Refresh, cx);
        });
    }
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let serial = editor.serial;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let paths = prompt.await??;
                let Some(path) = paths.and_then(|p| p.into_iter().next()) else {
                    return anyhow::Ok(None);
                };
                let file = std::fs::File::open(&path)?;
                ensure!(
                    file.metadata()?.len() <= 32 * 1024 * 1024,
                    "Attachment exceeds 32 MiB"
                );
                let mut bytes = Vec::new();
                file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= 32 * 1024 * 1024, "Attachment exceeds 32 MiB");
                let filename = path
                    .file_name()
                    .context("Choose a named file")?
                    .to_string_lossy()
                    .to_string();
                let content_type = match path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "txt" => "text/plain",
                    "pdf" => "application/pdf",
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    _ => "application/octet-stream",
                };
                Ok(Some(
                    json!({"filename":filename,"content_type":content_type,"bytes":bytes}),
                ))
            }
            .await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(Some(file)) => {
                        if let Some(editor) = this.editor.as_mut().filter(|e| e.serial == serial) {
                            if !editor.base["attachments"].is_array() {
                                editor.base["attachments"] = json!([]);
                            }
                            if let Some(files) = editor.base["attachments"].as_array_mut() {
                                files.push(file);
                                editor.dirty = true;
                                editor.revision += 1;
                            }
                        }
                        this.schedule_autosave(cx);
                    }
                    Ok(None) => {}
                    Err(e) => this.error = format!("Attachment: {e:#}"),
                };
                cx.notify();
            });
        })
        .detach();
    }
    pub fn save_bytes(
        &mut self,
        name: String,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir);
        let safe = name
            .rsplit(['/', '\\'])
            .next()
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .unwrap_or("attachment")
            .to_owned();
        let prompt = cx.prompt_for_new_path(&directory, Some(&safe));
        cx.spawn_in(window,async move |this,cx|{
            let result=async {let Some(path)=prompt.await??else{return anyhow::Ok(false);};
                let parent=path.parent().context("Choose a file directory")?;
                let mut staged=tempfile::NamedTempFile::new_in(parent)?;staged.write_all(&bytes)?;staged.as_file().sync_all()?;
                staged.persist_noclobber(&path).map_err(|e|anyhow::anyhow!("File was not replaced; choose a new filename: {}",e.error))?;
                #[cfg(unix)]std::fs::File::open(parent)?.sync_all().context("File installed but directory durability is uncertain; inspect destination before retrying")?;
                Ok(true)
            }.await;
            let _=this.update_in(cx,|this,_,cx|{match result{Ok(true)=>this.notice="File saved".into(),Ok(false)=>{},Err(e)=>this.error=format!("{e:#}")};cx.notify();});
        }).detach();
    }
    pub fn download(
        &mut self,
        account: i64,
        id: i64,
        index: usize,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command(
            "download_attachment",
            json!({"account":account,"id":id,"index":index}),
            window,
            cx,
            move |this, r, window, cx| match serde_json::from_value::<Vec<u8>>(r["bytes"].clone()) {
                Ok(bytes) => {
                    if preview {
                        match html::image(bytes) {
                            Ok(image) => {
                                this.preview = Some(image);
                            }
                            Err(e) => this.error = format!("Preview: {e:#}"),
                        };
                    } else {
                        this.save_bytes(
                            r["filename"].as_str().unwrap_or("attachment").into(),
                            bytes,
                            window,
                            cx,
                        );
                    }
                }
                Err(e) => this.error = format!("Invalid attachment: {e}"),
            },
        );
    }
    pub fn export_pim(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.command(
            "pim_export",
            json!({"kind":kind}),
            window,
            cx,
            move |this, r, window, cx| {
                this.save_bytes(
                    if kind == "calendar" {
                        "calendar.ics"
                    } else {
                        "contacts.vcf"
                    }
                    .into(),
                    r["text"].as_str().unwrap_or("").as_bytes().to_vec(),
                    window,
                    cx,
                )
            },
        );
    }
    pub fn import_pim(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let Some(path) = prompt.await??.and_then(|p| p.into_iter().next()) else {
                    return anyhow::Ok(None);
                };
                let file = std::fs::File::open(path)?;
                ensure!(
                    file.metadata()?.len() <= 16 * 1024 * 1024,
                    "Import exceeds 16 MiB"
                );
                let mut text = String::new();
                file.take(16 * 1024 * 1024 + 1).read_to_string(&mut text)?;
                ensure!(text.len() <= 16 * 1024 * 1024, "Import exceeds 16 MiB");
                Ok(Some(text))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(Some(text)) => {
                    this.mutate("pim_import", json!({"kind":kind,"text":text}), window, cx)
                }
                Ok(None) => {}
                Err(e) => {
                    this.error = format!("Import: {e:#}");
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub fn edit_account(&mut self, id: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let base=id.and_then(|id|self.accounts.iter().find(|a|a.id==id)).map(|a|{let mut c=a.config.clone();c["account"]=json!(a.id);c}).unwrap_or_else(||json!({"email":"","display":"","transport":"imap","jmap_url":"","imap_host":"","imap_port":993,"smtp_host":"","smtp_port":587,"smtp_implicit":false,"sent_folder":"Sent","auth_kind":"password","oauth_provider":"gmail","oauth_client_id":""}));
        self.open_editor(
            if id.is_some() {
                "update_account"
            } else {
                "add_account"
            },
            "Mail account",
            base,
            vec![
                Spec::text("email", "Email"),
                Spec::text("display", "Display name"),
                Spec::choice(
                    "transport",
                    "Protocol",
                    &[("imap", "IMAP"), ("jmap", "JMAP")],
                ),
                Spec::text("imap_host", "IMAP host"),
                Spec::text("imap_port", "IMAP port"),
                Spec::text("jmap_url", "JMAP session URL"),
                Spec::text("smtp_host", "SMTP host"),
                Spec::text("smtp_port", "SMTP port"),
                Spec::flag("smtp_implicit", "SMTP implicit TLS"),
                Spec::text("sent_folder", "Sent folder"),
                Spec::choice(
                    "auth_kind",
                    "Authentication",
                    &[("password", "Password / app password"), ("oauth2", "OAuth")],
                ),
                Spec::secret("password", "Password (blank retains existing)"),
                Spec::choice(
                    "oauth_provider",
                    "OAuth provider",
                    &[("gmail", "Google"), ("microsoft", "Microsoft")],
                ),
                Spec::text("oauth_client_id", "OAuth public client ID"),
            ],
            window,
            cx,
        );
    }
    pub fn edit_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut theme = Spec::choice(
            "theme",
            "Appearance",
            &[("auto", "Follow Omarchy"), ("original", "Original")],
        );
        if let Kind::Choice(values) = &mut theme.kind {
            values.extend(
                appearance::bundled()
                    .iter()
                    .filter_map(|t| Some((t["id"].as_str()?.into(), t["label"].as_str()?.into()))),
            );
        }
        self.open_editor(
            "settings_save",
            "Preferences",
            self.settings.clone(),
            vec![
                theme,
                Spec::choice(
                    "density",
                    "Density",
                    &[("comfortable", "Comfortable"), ("compact", "Compact")],
                ),
                Spec::choice(
                    "layout",
                    "Mail layout",
                    &[("three-pane", "Three panes"), ("reader", "Wide reader")],
                ),
                Spec::flag("attachment_preview", "Image attachment previews"),
                Spec::flag("notifications", "Desktop notifications"),
                Spec::flag("ai.enabled", "Enable AI assistance"),
                Spec::flag(
                    "ai.consent_to_send_contents",
                    "Allow sending selected mail content to the AI provider",
                ),
                Spec::choice(
                    "ai.provider",
                    "AI provider",
                    &[
                        ("anthropic", "Anthropic"),
                        ("open_ai_compatible", "OpenAI compatible"),
                    ],
                ),
                Spec::text("ai.endpoint", "Trusted AI endpoint"),
                Spec::text("ai.model", "AI model"),
                Spec::secret("ai_key", "AI API key (blank retains existing)"),
                Spec::text("dav.calendar_url", "CalDAV collection URL"),
                Spec::text("dav.contacts_url", "CardDAV collection URL"),
                Spec::text("dav.username", "DAV username"),
                Spec::secret("dav_password", "DAV password (blank retains existing)"),
            ],
            window,
            cx,
        );
    }
    pub fn edit_event(
        &mut self,
        event: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut event=event.unwrap_or_else(||{let start=self.calendar_day.to_string()+"T09:00";let end=self.calendar_day.to_string()+"T10:00";json!({"title":"","description":"","location":"","start":start,"end":end,"attendees":[],"all_day":false})});

        for key in ["start", "end"] {
            if let Some(timestamp) = event[key].as_i64() {
                event[key] = json!(chrono::DateTime::from_timestamp(timestamp, 0)
                    .map(|t| t.format("%Y-%m-%dT%H:%M").to_string())
                    .unwrap_or_default());
            }
        }
        event["attendees"] = json!(event["attendees"]
            .as_array()
            .map(|a| a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "))
            .unwrap_or_default());
        let action = if event["recurring"] == true {
            "readonly_event"
        } else {
            "save_event"
        };
        self.open_editor(
            action,
            "Calendar event",
            event,
            vec![
                Spec::text("title", "Title"),
                Spec::text("start", "Start (UTC YYYY-MM-DDTHH:MM)"),
                Spec::text("end", "End (UTC YYYY-MM-DDTHH:MM)"),
                Spec::flag("all_day", "All day (end date exclusive)"),
                Spec::text("location", "Location"),
                Spec::text("attendees", "Attendee emails"),
                Spec::multi("description", "Description"),
            ],
            window,
            cx,
        );
    }
    pub fn edit_contact(
        &mut self,
        contact: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_editor(
            "save_contact",
            "Contact",
            contact.unwrap_or_else(
                || json!({"name":"","email":"","phone":"","organization":"","note":""}),
            ),
            vec![
                Spec::text("name", "Name"),
                Spec::text("email", "Email"),
                Spec::text("phone", "Phone"),
                Spec::text("organization", "Organization"),
                Spec::multi("note", "Notes"),
            ],
            window,
            cx,
        );
    }
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<_> = self
            .accounts
            .iter()
            .filter(|a| self.filter.account.is_none_or(|id| id == a.id) && !a.config.is_null())
            .map(|a| a.id)
            .collect();
        if ids.is_empty() {
            self.notice = "No connected accounts to sync.".into();
            cx.notify();
            return;
        }
        if self.command_busy {
            return;
        }
        self.command_busy = true;
        let worker = self.worker.clone();
        let task = cx.background_executor().spawn(async move {
            let mut fetched = 0;
            for account in ids {
                let result = worker.call("sync", json!({"account":account}))?;
                fetched += result["fetched"].as_u64().unwrap_or(0);
            }
            anyhow::Ok(fetched)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.command_busy = false;
                match result {
                    Ok(count) => this.notice = format!("Sync complete · {count} messages fetched"),
                    Err(e) => this.error = format!("Sync: {e:#}"),
                };
                this.run(Operation::Refresh, cx);
            });
        })
        .detach();
    }
    pub fn bulk_route(
        &mut self,
        destination: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.checked.clone();
        let mut grouped = std::collections::BTreeMap::<i64, Vec<Value>>::new();
        for row in self.rows.iter().filter(|r| selected.contains(&r.key())) {
            grouped
                .entry(row.account)
                .or_default()
                .push(json!({"address":row.from,"destination":destination}));
        }
        if grouped.is_empty() {
            return;
        }
        if self.command_busy {
            return;
        }
        self.command_busy = true;
        let worker = self.worker.clone();
        let task = cx.background_executor().spawn(async move {
            for (account, senders) in grouped {
                for chunk in senders.chunks(500) {
                    worker.call("triage", json!({"account":account,"senders":chunk}))?;
                }
            }
            anyhow::Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.command_busy = false;
                match result {
                    Ok(()) => {
                        this.checked.clear();
                        this.detail = None;
                        this.notice = "Sender preferences updated".into();
                    }
                    Err(e) => this.error = e.to_string(),
                };
                this.run(Operation::Refresh, cx);
            });
        })
        .detach();
    }
    pub fn render_settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = div()
            .id("settings-pane")
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .overflow_y_scroll();
        pane = pane
            .child(self.button(
                "preferences",
                "Appearance, privacy & sync settings",
                cx,
                |this, window, cx| this.edit_settings(window, cx),
            ))
            .child(
                self.button("add-account", "Add account", cx, |this, window, cx| {
                    this.edit_account(None, window, cx)
                }),
            )
            .child(self.button(
                "dav-sync",
                "Sync calendar & contacts",
                cx,
                |this, window, cx| this.mutate("dav_sync", json!({}), window, cx),
            ));
        for a in &self.accounts {
            let id = a.id;
            pane=pane.child(div().p_3().border_1().border_color(cx.theme().colors.border).flex().flex_col().gap_2().child(line(&a.email))
                .child(self.button(format!("edit-{id}"),"Edit connection",cx,move|this,window,cx|this.edit_account(Some(id),window,cx)))
                .child(self.button(format!("auth-start-{id}"),"Start provider sign-in",cx,move|this,window,cx|this.command("auth_start",json!({"account":id}),window,cx,|this,_,_,_|this.notice="Complete sign-in in your browser, then choose Finish sign-in.".into())))
                .child(self.button(format!("auth-end-{id}"),"Finish sign-in",cx,move|this,window,cx|this.mutate("auth_complete",json!({"account":id}),window,cx))));
        }
        for kind in ["calendar", "contacts"] {
            pane = pane
                .child(self.button(
                    format!("import-{kind}"),
                    format!("Import {kind}"),
                    cx,
                    move |this, window, cx| this.import_pim(kind, window, cx),
                ))
                .child(self.button(
                    format!("export-{kind}"),
                    format!("Export {kind}"),
                    cx,
                    move |this, window, cx| this.export_pim(kind, window, cx),
                ));
        }
        pane = pane.child(div().text_lg().child("Send recovery"));
        for op in &self.ops {
            let id = op["id"].clone();
            let account = op["account"].clone();
            pane = pane.child(line(&format!("Operation {} · {}", id, op["last_error"])));
            for (decision, label) in [
                ("retry", "Retry"),
                ("sent", "Mark delivered"),
                ("cancel", "Cancel / restore draft"),
            ] {
                let (id, account) = (id.clone(), account.clone());
                pane = pane.child(self.button(
                    format!("review-{id}-{decision}"),
                    label,
                    cx,
                    move |this, window, cx| {
                        this.mutate(
                            "review_resolve",
                            json!({"account":account,"id":id,"decision":decision}),
                            window,
                            cx,
                        )
                    },
                ));
            }
        }
        pane = pane.child(div().text_lg().child("DAV conflicts"));
        for conflict in &self.conflicts {
            let id = conflict["id"].clone();
            let revision = conflict["revision"].clone();
            pane = pane.child(line(&format!("Conflict {id}")));
            for (resolution, label) in [("local", "Keep local"), ("remote", "Accept remote")] {
                let (id, revision) = (id.clone(), revision.clone());
                pane = pane.child(self.button(
                    format!("resolve-{id}-{resolution}"),
                    label,
                    cx,
                    move |this, window, cx| {
                        this.mutate(
                            "dav_resolve",
                            json!({"id":id,"revision":revision,"resolution":resolution}),
                            window,
                            cx,
                        )
                    },
                ));
            }
        }
        pane = pane.child(div().text_lg().child("AI activity (no message contents)"));
        for activity in &self.activities {
            pane = pane.child(line(&activity.to_string()));
        }
        pane.into_any_element()
    }
    pub fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        let weak = cx.entity().downgrade();
        Button::new(id.into(), label)
            .disabled(self.command_busy)
            .on_click(move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| action(this, window, cx));
            })
    }
    pub fn render_people(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = div()
            .id("people-pane")
            .p_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                self.button("new-contact", "New contact", cx, |this, window, cx| {
                    this.edit_contact(None, window, cx)
                }),
            );
        for c in &self.contacts {
            let edit = c.clone();
            let remove = c.clone();
            let email = c["email"].clone();
            pane = pane.child(
                div()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().colors.border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(line(&format!(
                        "{} · {}",
                        c["name"].as_str().unwrap_or(""),
                        c["email"].as_str().unwrap_or("")
                    )))
                    .child(self.button(
                        format!("contact-edit-{}", c["id"]),
                        "Edit",
                        cx,
                        move |this, window, cx| this.edit_contact(Some(edit.clone()), window, cx),
                    ))
                    .child(self.button(
                        format!("contact-person-{}", c["id"]),
                        "Conversations & events",
                        cx,
                        move |this, window, cx| {
                            this.command(
                                "person",
                                json!({"email":email}),
                                window,
                                cx,
                                |this, r, _, _| this.person = Some(r),
                            )
                        },
                    ))
                    .child(self.button(
                        format!("contact-delete-{}", c["id"]),
                        "Delete contact",
                        cx,
                        move |this, window, cx| {
                            this.mutate("delete_contact", remove.clone(), window, cx)
                        },
                    )),
            );
        }
        if let Some(person) = &self.person {
            pane = pane.child(div().text_lg().child("Related conversations"));
            for m in person["messages"].as_array().unwrap_or(&vec![]) {
                let account = m["account"].as_i64().unwrap_or(0);
                let id = m["id"].as_i64().unwrap_or(0);
                pane = pane.child(self.button(
                    format!("person-message-{id}"),
                    line(m["subject"].as_str().unwrap_or("Untitled")),
                    cx,
                    move |this, _, cx| {
                        this.change_filter(Some(account), "imbox".into(), cx);
                        this.selected = Some(format!("{account}:{id}").into());
                        this.run(Operation::Read(account, id), cx);
                    },
                ));
            }
            for event in person["events"].as_array().unwrap_or(&vec![]) {
                pane = pane.child(line(event["title"].as_str().unwrap_or("Event")));
            }
        }
        pane.into_any_element()
    }
    pub fn render_calendar(&self, cx: &Context<Self>) -> AnyElement {
        let events: Vec<_> = self
            .events
            .iter()
            .filter_map(|e| calendar_event(e).ok())
            .collect();
        let weak = cx.entity().downgrade();
        let select = move |key: &SharedString, window: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |this, cx| {
                let event = this
                    .events
                    .iter()
                    .find(|e| e["id"].as_str() == Some(key.as_ref()))
                    .cloned();
                this.edit_event(event, window, cx);
            });
        };
        let mut controls = div().flex().flex_wrap().gap_2().child(self.button(
            "new-event",
            "New event",
            cx,
            |this, window, cx| this.edit_event(None, window, cx),
        ));
        for mode in ["month", "week", "day", "agenda"] {
            controls = controls.child(self.button(
                format!("calendar-{mode}"),
                mode,
                cx,
                move |this, _, cx| {
                    this.calendar_mode = mode.into();
                    cx.notify();
                },
            ));
        }
        for (step, label) in [(-1, "Previous"), (0, "Today"), (1, "Next")] {
            controls = controls.child(self.button(
                format!("calendar-step-{step}"),
                label,
                cx,
                move |this, _, cx| {
                    let span = match this.calendar_mode.as_str() {
                        "month" => jiff::Span::new().months(step),
                        "day" => jiff::Span::new().days(step),
                        _ => jiff::Span::new().days(step * 7),
                    };
                    this.calendar_day = if step == 0 {
                        jiff::Zoned::now().date()
                    } else {
                        this.calendar_day
                            .checked_add(span)
                            .unwrap_or(this.calendar_day)
                    };
                    this.filter.query = chrono::NaiveDate::parse_from_str(
                        &this.calendar_day.to_string(),
                        "%Y-%m-%d",
                    )
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|d| d.and_utc().timestamp().to_string())
                    .unwrap_or_default();
                    this.run(Operation::Refresh, cx);
                },
            ));
        }
        let weak = cx.entity().downgrade();
        let step = move |date: jiff::civil::Date, _: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |this, cx| {
                this.calendar_day = date;
                this.filter.query = date
                    .to_string()
                    .parse::<chrono::NaiveDate>()
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|d| d.and_utc().timestamp().to_string())
                    .unwrap_or_default();
                this.run(Operation::Refresh, cx);
            });
        };
        let weak = cx.entity().downgrade();
        let create = move |start: jiff::Timestamp,
                           end: jiff::Timestamp,
                           window: &mut Window,
                           cx: &mut App| {
            let _=weak.update(cx,|this,cx|this.edit_event(Some(json!({"title":"","start":start.as_second(),"end":end.as_second(),"attendees":[],"all_day":false})),window,cx));
        };
        let weak = cx.entity().downgrade();
        let resize =
            move |key: &SharedString, end: jiff::Timestamp, window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    if let Some(mut event) = this
                        .events
                        .iter()
                        .find(|e| e["id"].as_str() == Some(key.as_ref()))
                        .cloned()
                    {
                        event["end"] = json!(end.as_second());
                        this.edit_event(Some(event), window, cx);
                    }
                });
            };
        let weak = cx.entity().downgrade();
        let day = move |date: jiff::civil::Date, _: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |this, cx| {
                this.calendar_day = date;
                this.calendar_mode = "day".into();
                cx.notify();
            });
        };
        let calendar = match self.calendar_mode.as_str() {
            "week" => CalendarWeekView::new("week", self.calendar_day, events)
                .on_step(step)
                .on_create(create)
                .on_resize(resize)
                .on_event(select)
                .into_any_element(),
            "day" => CalendarDayView::new("day", self.calendar_day, events)
                .on_step(step)
                .on_create(create)
                .on_resize(resize)
                .on_event(select)
                .into_any_element(),
            "agenda" => AgendaView::new("agenda", self.calendar_day, 31, events)
                .on_event(select)
                .into_any_element(),
            _ => CalendarMonthView::new("month", self.calendar_day, events)
                .on_month(step)
                .on_day(day)
                .on_event(select)
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .child(controls)
            .child(div().flex_1().min_h_0().child(calendar))
            .into_any_element()
    }
}
fn calendar_event(e: &Value) -> Result<ely_calendar::Event> {
    let id = e["id"].as_str().context("Event ID missing")?;
    let title = line(e["title"].as_str().unwrap_or("Event"));
    if e["all_day"] == true {
        let first = e["start_date"]
            .as_str()
            .context("All-day start missing")?
            .parse::<jiff::civil::Date>()?;
        let end = e["end_date"]
            .as_str()
            .context("All-day end missing")?
            .parse::<jiff::civil::Date>()?;
        let last = end.checked_sub(jiff::Span::new().days(1))?;
        ensure!(last >= first, "All-day date range invalid");
        Ok(ely_calendar::Event::all_day(
            id.to_owned(),
            title,
            first,
            last,
            0,
        ))
    } else {
        let start =
            jiff::Timestamp::from_second(e["start"].as_i64().context("Event start missing")?)?;
        let end = jiff::Timestamp::from_second(e["end"].as_i64().context("Event end missing")?)?;
        ensure!(end > start, "Invalid event range");
        Ok(ely_calendar::Event::timed(
            id.to_owned(),
            title,
            start,
            end,
            0,
        ))
    }
}
pub fn query_text(query: &Value) -> String {
    let mut terms = Vec::new();
    for text in query["text"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(Value::as_str)
    {
        terms.push(format!("\"{}\"", text.replace('"', " ")));
    }
    if let Some(from) = query["from"].as_str() {
        terms.push(format!("from:{from}"));
    }
    if let Some(view) = query["destination"].as_str() {
        terms.push(format!("in:{view}"));
    }
    if query["has_attachment"] == true {
        terms.push("has:attachment".into());
    }
    if query["unread"] == true {
        terms.push("is:unread".into());
    }
    for key in ["before", "after"] {
        if let Some(date) = query[key]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        {
            terms.push(format!("{key}:{}", date.format("%Y-%m-%d")));
        }
    }
    terms.join(" ")
}
