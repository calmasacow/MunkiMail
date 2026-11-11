use super::*;
use ely_core::{
    forms::{Checkbox, Choice, Input, Select},
    overlays::Dialog,
};

#[derive(Clone)]
pub enum Kind {
    Text,
    Multi,
    Password,
    Bool,
    Choice(Vec<(String, String)>),
}
pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}
impl Spec {
    pub fn text(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: Kind::Text,
        }
    }
    pub fn multi(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: Kind::Multi,
        }
    }
    pub fn secret(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: Kind::Password,
        }
    }
    pub fn flag(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: Kind::Bool,
        }
    }
    pub fn choice(key: &'static str, label: &'static str, values: &[(&str, &str)]) -> Self {
        Self {
            key,
            label,
            kind: Kind::Choice(
                values
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
            ),
        }
    }
}
pub struct Field {
    pub spec: Spec,
    pub input: Entity<TextInput>,
}
pub struct Editor {
    pub action: &'static str,
    pub title: &'static str,
    pub base: Value,
    pub fields: Vec<Field>,
    pub revision: u64,
    pub dirty: bool,
    pub draft_id: Option<i64>,
    pub account: Option<i64>,
    pub serial: u64,
    pub _subscriptions: Vec<Subscription>,
}
impl Editor {
    pub fn values(&self, cx: &App) -> Value {
        let mut value = self.base.clone();
        for field in &self.fields {
            let keys: Vec<_> = field.spec.key.split('.').collect();
            let mut target = &mut value;
            for key in &keys[..keys.len().saturating_sub(1)] {
                if !target[*key].is_object() {
                    target[*key] = json!({});
                }
                target = &mut target[*key];
            }
            if let Some(key) = keys.last() {
                target[*key] = match field.spec.kind {
                    Kind::Bool => json!(field.input.read(cx).text() == "true"),
                    _ => json!(field.input.read(cx).text()),
                };
            }
        }
        value
    }
}
impl MailApp {
    pub fn open_editor(
        &mut self,
        action: &'static str,
        title: &'static str,
        base: Value,
        specs: Vec<Spec>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor.is_some() {
            self.error = "Save or cancel the current editor first.".into();
            cx.notify();
            return;
        }
        self.editor_serial += 1;
        let serial = self.editor_serial;
        let mut subscriptions = Vec::new();
        let fields = specs
            .into_iter()
            .map(|spec| {
                let initial = base
                    .pointer(&format!("/{}", spec.key.replace('.', "/")))
                    .cloned()
                    .unwrap_or(Value::Null);
                let text = match initial {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
                let input = cx.new(|cx| {
                    let mut input = TextInput::new(window, cx).label(spec.label).max_len(
                        if matches!(spec.kind, Kind::Multi) {
                            2_000_000
                        } else {
                            16_384
                        },
                    );
                    if matches!(spec.kind, Kind::Multi) {
                        input = input.multi_line(4, 14);
                    }
                    if matches!(spec.kind, Kind::Password) {
                        input = input.masked();
                    }
                    input.set_text(text, cx);
                    input
                });
                subscriptions.push(cx.subscribe(&input, move |this, _, event, cx| {
                    if *event != InputEvent::Changed {
                        return;
                    }
                    if let Some(editor) = this.editor.as_mut().filter(|e| e.serial == serial) {
                        editor.revision += 1;
                        editor.dirty = true;
                    }
                    this.schedule_autosave(cx);
                    cx.notify();
                }));
                Field { spec, input }
            })
            .collect();
        let account = base["account"].as_i64();
        let draft_id = base["draft_id"].as_i64();
        self.editor = Some(Editor {
            action,
            title,
            base,
            fields,
            revision: 0,
            dirty: false,
            draft_id,
            account,
            serial,
            _subscriptions: subscriptions,
        });
        self.error.clear();
        cx.notify();
    }
    pub fn schedule_autosave(&self, cx: &mut Context<Self>) {
        let Some(editor) = self
            .editor
            .as_ref()
            .filter(|e| e.action == "save_draft" && e.dirty)
        else {
            return;
        };
        let (serial, revision, handle) = (editor.serial, editor.revision, self.handle);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1400))
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ =
                    this.update(cx, |this, cx| {
                        if this.editor.as_ref().is_some_and(|e| {
                            e.serial == serial && e.revision == revision && e.dirty
                        }) && !this.command_busy
                        {
                            this.save_editor(false, false, window, cx);
                        }
                    });
            });
        })
        .detach();
    }
    pub fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_busy {
            self.error = "Wait for the current save to finish.".into();
            cx.notify();
            return;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.action == "save_draft" && (e.dirty || e.draft_id.is_none()))
        {
            self.save_editor(true, false, window, cx);
        } else {
            self.editor = None;
            self.error.clear();
            cx.notify();
        }
    }
    pub fn compose(
        &mut self,
        account: i64,
        draft: Value,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut base = draft;
        for key in ["to", "cc", "bcc"] {
            base[key] = json!(base[key]
                .as_array()
                .map(|a| a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default());
        }
        base["account"] = json!(account);
        base["draft_id"] = json!(id);
        let mut from = Spec::text("from", "From");
        from.kind = Kind::Choice(
            self.accounts
                .iter()
                .map(|a| (a.email.clone(), a.email.clone()))
                .collect(),
        );
        self.open_editor(
            "save_draft",
            "Compose message",
            base,
            vec![
                from,
                Spec::text("to", "To"),
                Spec::text("cc", "Cc"),
                Spec::text("bcc", "Bcc"),
                Spec::text("subject", "Subject"),
                Spec::multi("text", "Message"),
                Spec::multi("html", "Optional HTML body"),
            ],
            window,
            cx,
        );
    }
    pub fn new_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(a) = self
            .accounts
            .iter()
            .find(|a| Some(a.id) == self.filter.account)
            .or_else(|| self.accounts.first())
        else {
            self.error = "Connect an account in Settings first.".into();
            return;
        };
        let (account, from) = (a.id, a.email.clone());
        self.compose(account, json!({"from":from,"to":[],"cc":[],"bcc":[],"subject":"","text":"","html":null,"in_reply_to":null,"attachments":[]}), None, window, cx);
    }
    pub fn save_editor(
        &mut self,
        close: bool,
        send: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_busy {
            return;
        }
        let Some(e) = &self.editor else {
            return;
        };
        let (serial, revision, action) = (e.serial, e.revision, e.action);
        let mut value = e.values(cx);
        let result = (|| -> Result<(&'static str, Value)> {
            match action {
                "save_draft" => {
                    if value["html"] == "" {
                        value["html"] = Value::Null;
                    }
                    let from = value["from"].as_str().context("Choose a sender")?;
                    let account = self
                        .accounts
                        .iter()
                        .find(|a| a.email.eq_ignore_ascii_case(from))
                        .context("Sender account no longer exists")?
                        .id;
                    ensure!(e.draft_id.is_none() || e.account == Some(account), "A saved draft retains its sending account. Start a new message to use another account.");
                    for key in ["to", "cc", "bcc"] {
                        value[key] = json!(split(value[key].as_str().unwrap_or("")));
                    }
                    if send {
                        ensure!(!self.demo, "Demo mode does not send mail");
                        ensure!(
                            value["to"].as_array().is_some_and(|a| !a.is_empty()),
                            "Add at least one recipient"
                        );
                    }
                    Ok((
                        action,
                        json!({"account":account,"id":e.draft_id,"draft":value}),
                    ))
                }
                "save_event" => {
                    for key in ["start", "end"] {
                        let date = chrono::NaiveDateTime::parse_from_str(
                            value[key].as_str().unwrap_or(""),
                            "%Y-%m-%dT%H:%M",
                        )
                        .context("Use YYYY-MM-DDTHH:MM in UTC")?;
                        value[key] = json!(date.and_utc().timestamp());
                    }
                    value["attendees"] = json!(split(value["attendees"].as_str().unwrap_or("")));
                    if value["all_day"] == true {
                        for (time, key) in [("start", "start_date"), ("end", "end_date")] {
                            value[key] = json!(chrono::DateTime::from_timestamp(
                                value[time].as_i64().context("Invalid date")?,
                                0
                            )
                            .context("Invalid date")?
                            .format("%Y-%m-%d")
                            .to_string());
                        }
                    }
                    Ok((action, json!({"event":value})))
                }
                "save_contact" => Ok((action, json!({"contact":value}))),
                "add_account" | "update_account" => {
                    for key in ["imap_port", "smtp_port"] {
                        value[key] = json!(value[key]
                            .as_str()
                            .unwrap_or("")
                            .parse::<u16>()
                            .context("Ports must be between 1 and 65535")?);
                    }
                    Ok((action, value))
                }
                "settings_save" => {
                    let ai_key = value["ai_key"].take();
                    let dav_password = value["dav_password"].take();
                    Ok((
                        action,
                        json!({"settings":value,"ai_key":ai_key,"dav_password":dav_password}),
                    ))
                }
                "rename_subject" => {
                    if value["subject"] == "" {
                        value["subject"] = Value::Null;
                    }
                    Ok((action, value))
                }
                "merge_threads" => {
                    value["target"] = json!(value["target"]
                        .as_str()
                        .unwrap_or("")
                        .parse::<i64>()
                        .context("Enter a message ID")?);
                    Ok((action, value))
                }
                _ => Ok((action, value)),
            }
        })();
        let (action, payload) = match result {
            Ok(v) => v,
            Err(e) => {
                self.error = format!("{e:#}");
                cx.notify();
                return;
            }
        };
        let account = payload["account"].as_i64();
        self.command(
            action,
            payload,
            window,
            cx,
            move |this, result, window, cx| {
                if action == "save_draft" {
                    let id = result["id"].as_i64();
                    if let Some(editor) = this.editor.as_mut().filter(|e| e.serial == serial) {
                        editor.draft_id = id;
                        editor.account = account;
                        if editor.revision == revision {
                            editor.dirty = false;
                        }
                    }
                    let clean = this
                        .editor
                        .as_ref()
                        .is_some_and(|e| e.serial == serial && !e.dirty);
                    if clean && send {
                        this.command(
                            "queue_draft",
                            json!({"account":account,"id":id}),
                            window,
                            cx,
                            |this, _, _, cx| {
                                this.editor = None;
                                this.notice = "Message queued for delivery".into();
                                this.run(Operation::Refresh, cx);
                            },
                        );
                    } else if clean && close {
                        this.editor = None;
                    } else if !clean {
                        this.schedule_autosave(cx);
                    }
                    this.notice = "Draft saved".into();
                } else {
                    let clean = this
                        .editor
                        .as_ref()
                        .is_some_and(|e| e.serial == serial && e.revision == revision);
                    if clean {
                        this.editor = None;
                    } else if let Some(editor) = this.editor.as_mut().filter(|e| e.serial == serial)
                    {
                        for key in ["id", "revision"] {
                            if !result[key].is_null() {
                                editor.base[key] = result[key].clone();
                            }
                        }
                    }
                    this.notice = if clean {
                        "Saved"
                    } else {
                        "Saved earlier changes; newer edits remain open"
                    }
                    .into();
                }
                this.run(Operation::Refresh, cx);
            },
        );
    }
    pub fn render_editor(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let demo = self.demo;
        let editor = self.editor.as_ref()?;
        let weak = cx.entity().downgrade();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .max_h(px(600.))
            .id("editor-fields")
            .overflow_y_scroll();
        for field in &editor.fields {
            if editor.action == "readonly_event" {
                body = body
                    .child(field.spec.label)
                    .child(line(field.input.read(cx).text()));
                continue;
            }
            let state = field.input.clone();
            let control = match &field.spec.kind {
                Kind::Bool => Checkbox::new(field.spec.key, state.read(cx).text() == "true")
                    .label(field.spec.label)
                    .on_change(move |v, _, cx| {
                        state.update(cx, |input, cx| input.set_text(v.to_string(), cx))
                    })
                    .into_any_element(),
                Kind::Choice(values) if !values.is_empty() => Select::new(
                    field.spec.key,
                    values.iter().map(|(k, v)| Choice::new(k.clone(), line(v))),
                )
                .label(field.spec.label)
                .selected(state.read(cx).text().to_owned())
                .on_change(move |v, _, cx| {
                    state.update(cx, |input, cx| input.set_text(v.to_string(), cx))
                })
                .into_any_element(),
                _ => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field.spec.label)
                    .child(Input::new(&state))
                    .into_any_element(),
            };
            body = body.child(control);
        }
        if matches!(editor.action, "save_event" | "readonly_event") {
            body = body.child(
                "Times are UTC. All-day end date is exclusive. Recurring events remain read-only.",
            );
        }
        if editor.action == "save_draft" {
            for (index, a) in editor.base["attachments"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .enumerate()
            {
                let w = weak.clone();
                let label = format!(
                    "Remove {}",
                    line(a["filename"].as_str().unwrap_or("attachment"))
                );
                body = body.child(Button::new(("remove-attachment", index), label).on_click(
                    move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| {
                            if let Some(e) = this.editor.as_mut() {
                                if let Some(files) = e.base["attachments"].as_array_mut() {
                                    if index < files.len() {
                                        files.remove(index);
                                        e.dirty = true;
                                        e.revision += 1;
                                    }
                                }
                            }
                            this.schedule_autosave(cx);
                            cx.notify();
                        });
                    },
                ));
            }
            if self.settings["ai"]["enabled"] == true
                && self.settings["ai"]["consent_to_send_contents"] == true
            {
                let w = weak.clone();
                body=body.child(Button::new("ai-draft","Draft with AI").disabled(self.command_busy).on_click(move |_,window,cx|{let _=w.update(cx,|this,cx|{
                    let Some(e)=&this.editor else{return;};let (serial,revision)=(e.serial,e.revision);let value=e.values(cx);
                    this.command("ai",json!({"capability":"draft_reply","text":value["text"]}),window,cx,move|this,r,_,cx|{
                        if let Some(e)=this.editor.as_ref().filter(|e|e.serial==serial&&e.revision==revision){if let Some(field)=e.fields.iter().find(|f|f.spec.key=="text"){field.input.clone().update(cx,|input,cx|input.set_text(r["text"].as_str().unwrap_or("").to_owned(),cx));}}
                        else {this.notice="Message changed during generation. AI response was not applied.".into();}
                    });
                });}));
            }
            let w = weak.clone();
            body = body.child(Button::new("attach", "Add attachment").on_click(
                move |_, window, cx| {
                    let _ = w.update(cx, |this, cx| this.attach(window, cx));
                },
            ));
        }
        let readonly = editor.action == "readonly_event";
        let saving = self.command_busy;
        let w = weak.clone();
        let save = weak.clone();
        let send = weak.clone();
        let mut dialog = Dialog::new("editor", editor.title, move |window, cx| {
            let _ = w.update(cx, |this, cx| this.close_editor(window, cx));
        })
        .held()
        .child(body)
        .child(div().child(line(&self.error)))
        .action(move |_| {
            Button::new("save-editor", "Save")
                .disabled(readonly || saving)
                .on_click(move |_, window, cx| {
                    let _ = save.update(cx, |this, cx| this.save_editor(false, false, window, cx));
                })
        });
        if editor.action == "save_draft" {
            dialog = dialog.action(move |_| {
                Button::new("send", "Queue send")
                    .disabled(demo)
                    .on_click(move |_, window, cx| {
                        let _ =
                            send.update(cx, |this, cx| this.save_editor(true, true, window, cx));
                    })
            });
        }
        if editor.action == "save_event" && editor.base["id"].is_string() {
            let remove = weak.clone();
            let payload = editor.base.clone();
            dialog = dialog.action(move |_| {
                Button::new("delete-event", "Delete event")
                    .disabled(saving)
                    .on_click(move |_, window, cx| {
                        let _ = remove.update(cx, |this, cx| {
                            this.command(
                                "delete_event",
                                payload.clone(),
                                window,
                                cx,
                                |this, _, _, cx| {
                                    this.editor = None;
                                    this.run(Operation::Refresh, cx);
                                },
                            )
                        });
                    })
            });
        }
        Some(dialog.into_any_element())
    }
}
pub fn split(text: &str) -> Vec<String> {
    text.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
