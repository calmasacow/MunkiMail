#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Reuse the desktop's storage, credential and validation paths during UI migration.
mod appearance;
#[allow(dead_code)] // Transport-only fields are used by the existing desktop app.
#[path = "../../desktop/src-tauri/src/backend.rs"]
mod backend;
mod forms;
mod html;
#[path = "../../desktop/src-tauri/src/network.rs"]
mod network;
#[path = "../../desktop/src-tauri/src/pim.rs"]
mod pim;
#[path = "../../desktop/src-tauri/src/runtime.rs"]
mod runtime;
mod screens;
#[cfg(feature = "ui-check")]
mod ui_check;

use anyhow::{ensure, Context as _, Result};
use ely_core::{
    buttons::{Button, IconButton},
    forms::{InputEvent, SearchInput, TextInput},
    menus::{ContextMenu, DropdownMenu, Menu, MenuItem},
    primitives::{FocusScope, IconName},
    theme::{ActiveTheme, Mode, Theme},
    Assets,
};
use ely_mail::{Contact, Mail, MailList, MailReader, Mailbox, MailboxList, Message};
use gpui::{
    div, prelude::*, px, size, AnyElement, AnyWindowHandle, App, AppContext, Bounds, Context,
    Entity, FocusHandle, SharedString, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(feature = "ui-check")]
use std::path::PathBuf;
use std::{rc::Rc, sync::Arc};

fn line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

const VIEWS: [(&str, &str); 11] = [
    ("imbox", "MeBox"),
    ("screener", "Buffers"),
    ("feed", "Feeds"),
    ("paper_trail", "Trails"),
    ("reply_later", "Round2its"),
    ("set_aside", "Asides"),
    ("drafts", "Drafts"),
    ("sent", "Sent"),
    ("calendar", "Calendar"),
    ("people", "People"),
    ("settings", "Settings"),
];

fn mailbox_icon(view: &str) -> IconName {
    match view {
        "imbox" => IconName::CircleDot,
        "screener" => IconName::Shield,
        "feed" => IconName::Layers,
        "paper_trail" => IconName::FileText,
        "reply_later" => IconName::Clock,
        "set_aside" => IconName::Archive,
        "drafts" => IconName::Pencil,
        "sent" => IconName::Send,
        "calendar" => IconName::Calendar,
        "people" => IconName::Users,
        _ => IconName::Settings,
    }
}

#[derive(Clone, Deserialize)]
struct Account {
    id: i64,
    email: String,
    #[serde(default)]
    config: Value,
}
#[derive(Clone, Deserialize, Serialize)]
struct Row {
    id: i64,
    account: i64,
    from: String,
    subject: String,
    renamed_subject: Option<String>,
    date: i64,
    seen: bool,
    #[serde(default)]
    snippet: String,
    has_attachment: bool,
}
impl Row {
    fn key(&self) -> SharedString {
        format!("{}:{}", self.account, self.id).into()
    }
    fn title(&self) -> &str {
        self.renamed_subject.as_deref().unwrap_or(&self.subject)
    }
    fn mail(&self) -> Mail {
        let mail = match jiff::Timestamp::from_second(self.date) {
            Ok(at) => Mail::new(self.key(), line(&self.from), line(self.title()), at),
            Err(_) => Mail::undated(self.key(), line(&self.from), line(self.title())),
        };
        mail.snippet(line(&self.snippet))
            .unread(!self.seen)
            .attachments(usize::from(self.has_attachment))
    }
}
#[derive(Clone)]
struct Filter {
    account: Option<i64>,
    view: String,
    query: String,
    limit: usize,
}
struct Snapshot {
    settings: Value,
    drafts: Vec<Value>,
    events: Vec<Value>,
    contacts: Vec<Value>,
    ops: Vec<Value>,
    conflicts: Vec<Value>,
    activities: Vec<Value>,
    accounts: Vec<Account>,
    rows: Vec<Row>,
    total: usize,
    detail: Option<Value>,
}
#[derive(Clone)]
enum Operation {
    Refresh,
    Read(i64, i64),
    #[cfg(test)]
    Local(&'static str, Value),
}

fn snapshot(b: &mut backend::Backend, filter: &Filter, operation: Operation) -> Result<Snapshot> {
    ensure!(
        (1..=100_000).contains(&filter.limit),
        "Mail page exceeds supported range"
    );
    let bootstrap = b.local("bootstrap", json!({}))?;
    let settings = bootstrap["settings"].clone();
    let accounts: Vec<Account> = serde_json::from_value(bootstrap["accounts"].clone())?;
    if let Some(id) = filter.account {
        ensure!(
            accounts.iter().any(|a| a.id == id),
            "Account no longer exists"
        );
    }
    let detail = match operation {
        Operation::Read(account, id) => {
            Some(b.local("message", json!({"account":account,"id":id,"remote":false}))?)
        }
        #[cfg(test)]
        Operation::Local(action, payload) => {
            b.local(action, payload)?;
            None
        }
        Operation::Refresh => None,
    };
    let mut drafts = Vec::new();
    let mut ops = Vec::new();
    let mut events = Vec::new();
    let mut contacts = Vec::new();
    let mut conflicts = Vec::new();
    let mut activities = Vec::new();
    let array =
        |v: Value, key: &str| -> Vec<Value> { v[key].as_array().cloned().unwrap_or_default() };
    if filter.view == "calendar" {
        let now = filter
            .query
            .parse::<i64>()
            .unwrap_or_else(|_| chrono::Utc::now().timestamp());
        events = array(
            b.local("events", json!({"start":now-180*86400,"end":now+180*86400}))?,
            "events",
        );
    }
    if filter.view == "people" {
        contacts = array(b.local("contacts", json!({}))?, "contacts");
    }
    if filter.view == "settings" {
        conflicts = array(b.local("dav_conflicts", json!({}))?, "conflicts");
        activities = array(b.local("ai_activity", json!({}))?, "activities");
    }
    let mut rows = Vec::new();
    let mut total = 0usize;
    for a in accounts
        .iter()
        .filter(|a| filter.account.is_none_or(|id| id == a.id))
    {
        if filter.view == "drafts" {
            for mut draft in array(b.local("drafts", json!({"account":a.id}))?, "drafts") {
                draft["account"] = json!(a.id);
                drafts.push(draft);
            }
            continue;
        }
        if filter.view == "settings" {
            for mut op in array(b.local("review", json!({"account":a.id}))?, "ops") {
                op["account"] = json!(a.id);
                ops.push(op);
            }
        }
        if ["calendar", "people", "settings"].contains(&filter.view.as_str()) {
            continue;
        }
        for offset in (0..filter.limit).step_by(500) {
            let result = b.local("messages", json!({"account":a.id,"view":filter.view,"query":filter.query,"limit":500.min(filter.limit-offset),"offset":offset}))?;
            let count = result["total"].as_u64().context("Invalid message count")? as usize;
            if offset == 0 {
                total = total.checked_add(count).context("Message count overflow")?;
            }
            rows.extend(serde_json::from_value::<Vec<Row>>(
                result["messages"].clone(),
            )?);
            if offset + 500 >= count {
                break;
            }
        }
    }
    rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
    rows.truncate(filter.limit);
    drafts.sort_by_key(|d| std::cmp::Reverse(d["updated_at"].as_i64().unwrap_or(0)));
    Ok(Snapshot {
        settings,
        drafts,
        events,
        contacts,
        ops,
        conflicts,
        activities,
        accounts,
        rows,
        total,
        detail,
    })
}

struct Worker {
    sync_status: std::sync::Mutex<String>,
    state: Arc<runtime::AppState>,
    runtime: Arc<tokio::runtime::Runtime>,
}
impl Worker {
    fn call(&self, action: &str, payload: Value) -> Result<Value> {
        self.runtime
            .block_on(runtime::dispatch_inner(self.state.clone(), action, payload))
    }
    fn run(&self, filter: &Filter, operation: Operation) -> Result<Snapshot> {
        let operation = match operation {
            #[cfg(test)]
            Operation::Local(action, payload) => {
                self.call(action, payload)?;
                Operation::Refresh
            }
            op => op,
        };
        let filter = filter.clone();
        self.runtime
            .block_on(runtime::with_backend(self.state.clone(), move |b| {
                snapshot(b, &filter, operation)
            }))
    }
}

struct MailApp {
    worker: Arc<Worker>,
    filter: Filter,
    accounts: Vec<Account>,
    rows: Vec<Row>,
    total: usize,
    detail: Option<Value>,
    selected: Option<SharedString>,
    search: Entity<TextInput>,
    focus: FocusHandle,
    request: u64,
    busy: bool,
    error: String,
    demo: bool,
    handle: AnyWindowHandle,
    settings: Value,
    applied_settings: String,
    drafts: Vec<Value>,
    events: Vec<Value>,
    contacts: Vec<Value>,
    ops: Vec<Value>,
    conflicts: Vec<Value>,
    activities: Vec<Value>,
    editor: Option<forms::Editor>,
    editor_serial: u64,
    command_busy: bool,
    notice: String,
    person: Option<Value>,
    calendar_mode: String,
    calendar_day: jiff::civil::Date,
    html_mode: bool,
    remote: bool,
    images: std::collections::HashMap<String, Arc<gpui::Image>>,
    checked: Vec<SharedString>,
    preview: Option<Arc<gpui::Image>>,
    _search_subscription: Subscription,
}
impl MailApp {
    fn new(worker: Arc<Worker>, demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("Search mail; Enter to search")
                .label("Search messages")
        });
        let subscription = cx.subscribe(&search, |this, input, event, cx| {
            if *event == InputEvent::Submit
                || (*event == InputEvent::Changed && input.read(cx).is_empty())
            {
                this.filter.query = input.read(cx).text().to_owned();
                this.filter.limit = 100;
                this.selected = None;
                this.detail = None;
                this.run(Operation::Refresh, cx);
            }
        });
        let mut this = Self {
            worker,
            filter: Filter {
                account: None,
                view: "imbox".into(),
                query: String::new(),
                limit: 100,
            },
            accounts: vec![],
            rows: vec![],
            total: 0,
            detail: None,
            selected: None,
            search,
            focus: cx.focus_handle(),
            request: 0,
            busy: false,
            error: String::new(),
            demo,
            handle: window.window_handle(),
            settings: json!({}),
            applied_settings: String::new(),
            drafts: vec![],
            events: vec![],
            contacts: vec![],
            ops: vec![],
            conflicts: vec![],
            activities: vec![],
            editor: None,
            editor_serial: 0,
            command_busy: false,
            notice: String::new(),
            person: None,
            calendar_mode: "month".into(),
            calendar_day: jiff::Zoned::now().date(),
            html_mode: false,
            remote: false,
            images: std::collections::HashMap::new(),
            checked: vec![],
            preview: None,
            _search_subscription: subscription,
        };
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| {
                if this.command_busy {
                    this.error = "Wait for the pending operation before closing.".into();
                    cx.notify();
                    return false;
                }
                if this.editor.is_some() {
                    this.close_editor(window, cx);
                    this.notice = "Editor saved or closed. Close the window again to quit.".into();
                    cx.notify();
                    return false;
                }
                true
            })
            .unwrap_or(true)
        });
        if !demo {
            let worker = this.worker.clone();
            let state = worker.state.clone();
            let status_worker = worker.clone();
            worker.runtime.spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                interval.tick().await;
                let mut ticks = 0u64;
                loop {
                    interval.tick().await;
                    ticks += 1;
                    let accounts =
                        runtime::with_backend(state.clone(), |b| b.local("bootstrap", json!({})))
                            .await;
                    if let Err(error) = &accounts {
                        if let Ok(mut status) = status_worker.sync_status.lock() {
                            *status = format!("Background sync: {error:#}");
                        }
                    }
                    if let Ok(bootstrap) = accounts {
                        for a in bootstrap["accounts"].as_array().unwrap_or(&vec![]) {
                            if a["config"].is_null() {
                                continue;
                            }
                            let result = runtime::dispatch_inner(
                                state.clone(),
                                "sync",
                                json!({"account":a["id"]}),
                            )
                            .await;
                            if let Err(error) = &result {
                                if let Ok(mut status) = status_worker.sync_status.lock() {
                                    *status = format!("Background sync: {error:#}");
                                }
                            }
                            if result
                                .as_ref()
                                .is_ok_and(|r| r["fetched"].as_u64().unwrap_or(0) > 0)
                                && bootstrap["settings"]["notifications"] == true
                            {
                                let _ = notify_rust::Notification::new()
                                    .summary("MunkiMail")
                                    .body("New mail is ready when you are.")
                                    .show();
                            }
                        }
                        if ticks.is_multiple_of(5) {
                            if let Err(error) =
                                runtime::dispatch_inner(state.clone(), "dav_sync", json!({})).await
                            {
                                if let Ok(mut status) = status_worker.sync_status.lock() {
                                    *status = format!("Background DAV sync: {error:#}");
                                }
                            }
                        }
                    }
                }
            });
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if !this.busy && !this.command_busy {
                            this.run(Operation::Refresh, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            })
            .detach();
        }
        this.run(Operation::Refresh, cx);
        this
    }
    fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.request += 1;
        let request = self.request;
        self.busy = true;
        self.error.clear();
        let worker = self.worker.clone();
        let filter = self.filter.clone();
        let is_read = matches!(operation, Operation::Read(..));
        let task = cx
            .background_executor()
            .spawn(async move { worker.run(&filter, operation) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if request != this.request {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(s) => {
                        if request == 1 && !this.demo {
                            eprintln!(
                                "MunkiMail opened encrypted mail store: {} accounts",
                                s.accounts.len()
                            );
                        }
                        if let Ok(status) = this.worker.sync_status.lock() {
                            if !status.is_empty() {
                                this.notice = status.clone();
                            }
                        }
                        this.settings = s.settings;
                        this.drafts = s.drafts;
                        this.events = s.events;
                        this.contacts = s.contacts;
                        this.ops = s.ops;
                        this.conflicts = s.conflicts;
                        this.activities = s.activities;
                        let appearance =
                            format!("{}:{}", this.settings["theme"], this.settings["density"]);
                        if this.applied_settings != appearance {
                            match appearance::apply(&this.settings, cx) {
                                Ok(()) => this.applied_settings = appearance,
                                Err(e) => this.error = format!("Theme: {e:#}"),
                            };
                        }
                        this.accounts = s.accounts;
                        this.rows = s.rows;
                        this.total = s.total;
                        if is_read {
                            this.detail = s.detail;
                        }
                    }
                    Err(e) => this.error = format!("{e:#}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn change_filter(&mut self, account: Option<i64>, view: String, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            self.error = "Save or close the editor first.".into();
            cx.notify();
            return;
        }
        self.checked.clear();
        self.html_mode = false;
        self.remote = false;
        self.images.clear();
        self.filter.account = account;
        self.filter.view = view;
        self.filter.limit = 100;
        self.rows.clear();
        self.total = 0;
        self.selected = None;
        self.detail = None;
        self.run(Operation::Refresh, cx);
    }
    fn account_menu(&self, cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new();
        for (id, label) in std::iter::once((None, "All".to_owned()))
            .chain(self.accounts.iter().map(|a| (Some(a.id), line(&a.email))))
        {
            let weak = cx.entity().downgrade();
            menu = menu.item(MenuItem::radio(label, self.filter.account == id).on_click(
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.change_filter(id, this.filter.view.clone(), cx)
                    });
                },
            ));
        }
        menu
    }
}
impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let weak = cx.entity().downgrade();
        let boxes = MailboxList::new("mailboxes")
            .fixed_section(
                "Mail",
                VIEWS[..8]
                    .iter()
                    .map(|(key, label)| Mailbox::new(*key, *label, mailbox_icon(key))),
            )
            .fixed_section(
                "More",
                VIEWS[8..10]
                    .iter()
                    .map(|(key, label)| Mailbox::new(*key, *label, mailbox_icon(key))),
            )
            .open(self.filter.view.clone())
            .on_open(move |key, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.change_filter(this.filter.account, key.to_string(), cx)
                });
            });
        let scope = self
            .accounts
            .iter()
            .find(|a| Some(a.id) == self.filter.account)
            .map(|a| line(&a.email))
            .unwrap_or_else(|| "All accounts".into());
        let sidebar = div()
            .w(px(230.))
            .h_full()
            .flex_none()
            .p_4()
            .bg(colors.surface)
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xl()
                    .child(
                        div()
                            .rounded_md()
                            .px_2()
                            .bg(colors.accent)
                            .text_color(colors.bg)
                            .child("m•"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child("munki")
                            .child(div().text_color(colors.accent).child("mail")),
                    ),
            )
            .child(
                ContextMenu::new("account-context", self.account_menu(cx)).child(
                    DropdownMenu::new("accounts", "YOUR SPACE", self.account_menu(cx)),
                ),
            )
            .child(div().text_sm().text_color(colors.fg_muted).child(scope))
            .child(
                self.button("compose", "New message", cx, |this, window, cx| {
                    this.new_draft(window, cx)
                }),
            )
            .child(boxes)
            .child(div().flex_1())
            .child(
                IconButton::new("settings", IconName::Settings)
                    .tooltip("Settings")
                    .on_click({
                        let weak = cx.entity().downgrade();
                        move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.change_filter(this.filter.account, "settings".into(), cx)
                            });
                        }
                    }),
            );
        let mut header = div()
            .p_3()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .child(SearchInput::new("search", &self.search)),
            )
            .child(self.button("refresh", "Refresh", cx, |this, _, cx| {
                this.run(Operation::Refresh, cx)
            }))
            .child(self.button("sync", "Sync", cx, |this, window, cx| this.sync(window, cx)));
        if self.settings["ai"]["enabled"] == true
            && self.settings["ai"]["consent_to_send_contents"] == true
        {
            header = header.child(
                self.button("ai-search", "AI search", cx, |this, window, cx| {
                    let text = this.search.read(cx).text().to_owned();
                    this.command(
                        "ai",
                        json!({"capability":"translate_query","text":text}),
                        window,
                        cx,
                        |this, r, _, cx| {
                            let query = screens::query_text(&r["query"]);
                            this.search
                                .update(cx, |input, cx| input.set_text(query.clone(), cx));
                            this.filter.query = query;
                            this.run(Operation::Refresh, cx);
                        },
                    );
                }),
            );
        }
        let content = match self.filter.view.as_str() {
            "calendar" => self.render_calendar(cx),
            "people" => self.render_people(cx),
            "settings" => self.render_settings(cx),
            "drafts" => self.render_drafts(cx),
            _ => self.render_mail(cx),
        };
        let mut workspace = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(colors.fg_muted)
                    .child(if self.demo {
                        "DEMO · synthetic unencrypted data · sending and external services disabled"
                    } else {
                        "Encrypted local mail"
                    }),
            );
        if !self.notice.is_empty() {
            workspace = workspace.child(div().px_4().py_2().child(line(&self.notice)));
        }
        if !self.error.is_empty() {
            workspace = workspace.child(
                div()
                    .px_4()
                    .py_2()
                    .text_color(colors.danger)
                    .child(line(&self.error)),
            );
        }
        workspace = workspace.child(div().flex_1().min_h_0().child(content));
        let mut root = div()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.editor.is_some()
                    || this.preview.is_some()
                    || this.search.read(cx).focus().is_focused(window)
                    || event.keystroke.modifiers.control
                    || event.keystroke.modifiers.alt
                    || event.keystroke.modifiers.platform
                {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "/" => {
                        let focus = this.search.read(cx).focus().clone();
                        window.focus(&focus, cx);
                    }
                    "c" => this.new_draft(window, cx),
                    "j" | "k" => {
                        if !this.rows.is_empty() {
                            let index = this
                                .rows
                                .iter()
                                .position(|r| Some(r.key()) == this.selected)
                                .map(|i| {
                                    if event.keystroke.key == "j" {
                                        (i + 1).min(this.rows.len() - 1)
                                    } else {
                                        i.saturating_sub(1)
                                    }
                                })
                                .unwrap_or(0);
                            let row = &this.rows[index];
                            let (account, id) = (row.account, row.id);
                            this.selected = Some(row.key());
                            this.remote = false;
                            this.images.clear();
                            this.run(Operation::Read(account, id), cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .size_full()
            .relative()
            .flex()
            .bg(colors.bg)
            .text_color(colors.fg)
            .child(sidebar)
            .child(workspace);
        if let Some(editor) = self.render_editor(cx) {
            root = root.child(editor);
        }
        if let Some(image) = &self.preview {
            let weak = cx.entity().downgrade();
            root = root.child(
                ely_core::overlays::Dialog::new(
                    "attachment-preview",
                    "Attachment preview",
                    move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.preview = None;
                            cx.notify();
                        });
                    },
                )
                .child(
                    gpui::img(image.clone())
                        .max_w_full()
                        .max_h(px(600.))
                        .object_fit(gpui::ObjectFit::Contain),
                ),
            );
        }
        let _ = window;
        FocusScope::new(&self.focus).root().child(root)
    }
}
impl MailApp {
    fn render_drafts(&self, cx: &Context<Self>) -> AnyElement {
        let mut pane = div()
            .id("drafts")
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_2();
        for draft in &self.drafts {
            let (account, id) = (
                draft["account"].as_i64().unwrap_or(0),
                draft["id"].as_i64().unwrap_or(0),
            );
            pane = pane.child(self.button(
                format!("draft-{account}-{id}"),
                format!(
                        "{} · {}",
                        line(
                            draft["subject"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or("Untitled draft")
                        ),
                        draft["state"].as_str().unwrap_or("draft")
                    ),
                cx,
                move |this, window, cx| {
                    this.command(
                        "draft",
                        json!({"account":account,"id":id}),
                        window,
                        cx,
                        move |this, r, window, cx| this.compose(account, r, Some(id), window, cx),
                    )
                },
            ));
        }
        pane.into_any_element()
    }
    fn render_mail(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors.clone();
        let weak = cx.entity().downgrade();
        let list = MailList::new(
            "messages",
            Rc::new(self.rows.iter().map(Row::mail).collect()),
        )
        .selected(self.checked.clone())
        .on_select(move |keys, _, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.checked = keys.to_vec();
                let Some(key) = keys.first() else {
                    return;
                };
                if let Some(row) = this.rows.iter().find(|r| r.key() == *key) {
                    let (account, id) = (row.account, row.id);
                    this.selected = Some(key.clone());
                    this.detail = None;
                    this.remote = false;
                    this.html_mode = false;
                    this.images.clear();
                    this.run(Operation::Read(account, id), cx);
                }
            });
        });
        let title = VIEWS
            .iter()
            .find(|(key, _)| *key == self.filter.view)
            .map(|(_, label)| *label)
            .unwrap_or("Mail");
        let width = if self.settings["layout"] == "reader" {
            280.
        } else {
            390.
        };
        let mut pane = div()
            .w(px(width))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(colors.border)
            .flex()
            .flex_col()
            .child(
                div()
                    .p_4()
                    .text_xl()
                    .child(format!("{title} · {}", self.total)),
            );
        if self.filter.view == "screener" {
            let mut bar = div().p_2().flex().flex_wrap().gap_1().child(self.button(
                "select-all",
                "Select all loaded",
                cx,
                |this, _, cx| {
                    this.checked = this.rows.iter().map(Row::key).collect();
                    cx.notify();
                },
            ));
            for (destination, label) in [
                ("imbox", "MeBox"),
                ("feed", "Feeds"),
                ("paper_trail", "Trails"),
                ("dismiss", "Dismiss"),
            ] {
                bar = bar.child(self.button(
                    format!("bulk-{destination}"),
                    label,
                    cx,
                    move |this, window, cx| this.bulk_route(destination, window, cx),
                ));
            }
            pane = pane.child(bar);
        }
        pane = pane.child(div().flex_1().min_h_0().child(list));
        if self.rows.len() < self.total && self.filter.limit < 100_000 {
            pane = pane.child(self.button("load-more", "Load more", cx, |this, _, cx| {
                this.filter.limit = (this.filter.limit + 100).min(100_000);
                this.run(Operation::Refresh, cx);
            }));
        }
        let mut reader = div()
            .id("reader")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6();
        if let Some(d) = &self.detail {
            if let Ok(row) = serde_json::from_value::<Row>(d["message"].clone()) {
                let (account, id) = (row.account, row.id);
                let mut toolbar = div().flex().flex_wrap().gap_2();
                for (stack, label) in [("reply_later", "Round2its"), ("set_aside", "Asides")] {
                    toolbar =
                        toolbar.child(self.button(stack, label, cx, move |this, window, cx| {
                            this.mutate(
                                "set_stack",
                                json!({"account":account,"id":id,"stack":stack,"enabled":true}),
                                window,
                                cx,
                            )
                        }));
                }
                if ["reply_later", "set_aside"].contains(&self.filter.view.as_str()) {
                    let stack = self.filter.view.clone();
                    toolbar = toolbar.child(self.button(
                        "remove-stack",
                        "Remove from this view",
                        cx,
                        move |this, window, cx| {
                            this.mutate(
                                "set_stack",
                                json!({"account":account,"id":id,"stack":stack,"enabled":false}),
                                window,
                                cx,
                            )
                        },
                    ));
                }
                for (forward, label) in [(false, "Reply"), (true, "Forward")] {
                    toolbar = toolbar.child(self.button(
                        if forward { "forward" } else { "reply" },
                        label,
                        cx,
                        move |this, window, cx| {
                            this.command(
                                "reply",
                                json!({"account":account,"id":id,"forward":forward}),
                                window,
                                cx,
                                move |this, r, window, cx| {
                                    this.compose(account, r, None, window, cx)
                                },
                            )
                        },
                    ));
                }
                let subject = row.title().to_owned();
                toolbar = toolbar
                    .child(
                        self.button("rename", "Rename subject", cx, move |this, window, cx| {
                            this.open_editor(
                                "rename_subject",
                                "Display subject",
                                json!({"account":account,"id":id,"subject":subject}),
                                vec![forms::Spec::text(
                                    "subject",
                                    "Subject (blank restores original)",
                                )],
                                window,
                                cx,
                            )
                        }),
                    )
                    .child(
                        self.button("merge", "Merge thread", cx, move |this, window, cx| {
                            this.open_editor(
                                "merge_threads",
                                "Merge thread",
                                json!({"account":account,"source":id,"target":""}),
                                vec![forms::Spec::text(
                                    "target",
                                    "Target message ID in the same account",
                                )],
                                window,
                                cx,
                            )
                        }),
                    )
                    .child(
                        self.button("unread", "Mark unread", cx, move |this, window, cx| {
                            this.mutate(
                                "mark_seen",
                                json!({"account":account,"id":id,"seen":false}),
                                window,
                                cx,
                            )
                        }),
                    );
                reader = reader.child(toolbar);
                if d["html"].is_string() {
                    reader = reader.child(self.button(
                        "html-toggle",
                        if self.html_mode {
                            "Plain text"
                        } else {
                            "HTML view"
                        },
                        cx,
                        |this, _, cx| {
                            this.html_mode = !this.html_mode;
                            cx.notify();
                        },
                    ));
                    if !self.remote {
                        reader = reader.child(self.button(
                            "remote-images",
                            "Allow remote images (reveals IP and opens)",
                            cx,
                            move |this, window, cx| {
                                this.command(
                                    "message",
                                    json!({"account":account,"id":id,"remote":true}),
                                    window,
                                    cx,
                                    move |this, r, _, cx| {
                                        if this.selected.as_ref().is_some_and(|key| {
                                            key.as_ref() == format!("{account}:{id}")
                                        }) {
                                            this.detail = Some(r);
                                            this.remote = true;
                                            this.html_mode = true;
                                            this.load_images(cx);
                                        }
                                    },
                                )
                            },
                        ));
                    }
                }
                let body = if self.html_mode {
                    match html::render(d["html"].as_str().unwrap_or(""), self, cx) {
                        Ok(el) => el,
                        Err(e) => div().child(line(&format!("{e:#}"))).into_any_element(),
                    }
                } else {
                    div()
                        .mt_4()
                        .flex()
                        .flex_col()
                        .children(
                            d["text"]
                                .as_str()
                                .unwrap_or("No plain-text body")
                                .lines()
                                .map(|line| div().min_h(px(22.)).child(line.to_owned())),
                        )
                        .into_any_element()
                };
                let mut message =
                    Message::undated(row.key(), Contact::new(line(&row.from), line(&row.from)));
                if let Ok(at) = jiff::Timestamp::from_second(row.date) {
                    message.at = Some(at);
                }
                reader = reader
                    .child(MailReader::new("mail-reader", line(row.title()), message).child(body));
                for (index, attachment) in d["attachments"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .enumerate()
                {
                    let index = attachment["index"]
                        .as_u64()
                        .map(|v| v as usize)
                        .unwrap_or(index);
                    let filename = attachment["filename"].as_str().unwrap_or("attachment");
                    reader = reader.child(self.button(
                        format!("download-{index}"),
                        format!("Download {}", line(filename)),
                        cx,
                        move |this, window, cx| {
                            this.download(account, id, index, false, window, cx)
                        },
                    ));
                    if self.settings["attachment_preview"] == true {
                        reader = reader.child(self.button(
                            format!("preview-{index}"),
                            "Preview image",
                            cx,
                            move |this, window, cx| {
                                this.download(account, id, index, true, window, cx)
                            },
                        ));
                    }
                }
                if d["invite"].is_string() || d["calendar_invite"] == true {
                    for response in ["ACCEPTED", "TENTATIVE", "DECLINED"] {
                        let revision = d["invite_revision"].clone();
                        reader=reader.child(self.button(format!("rsvp-{response}"),response,cx,move|this,window,cx|this.command("rsvp",json!({"account":account,"id":id,"revision":revision,"response":response}),window,cx,move|this,_,_,cx|this.run(Operation::Read(account,id),cx))));
                    }
                }
                if self.settings["ai"]["enabled"] == true
                    && self.settings["ai"]["consent_to_send_contents"] == true
                {
                    let sender = row.from.clone();
                    let subject = row.subject.clone();
                    let text = d["text"].clone();
                    reader=reader.child(self.button("ai-sender","Suggest sender destination",cx,move|this,window,cx|this.command("ai",json!({"capability":"classify_sender","sender":sender,"subject":subject,"text":text}),window,cx,|this,r,_,_|this.notice=format!("Suggestion: {} · {}",r["destination"],r["reason"]))));
                }
            }
        } else {
            reader = reader.child(div().text_color(colors.fg_muted).child(if self.busy {
                "Loading mail…"
            } else {
                "Select a message"
            }));
        }
        div()
            .size_full()
            .flex()
            .child(pane)
            .child(reader)
            .into_any_element()
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    ensure!(!demo || cfg!(feature = "ui-check"), "Synthetic data is available only in the ui-check build. Launch without --demo to use real accounts.");
    #[cfg(feature = "ui-check")]
    let capture = args
        .windows(2)
        .find(|pair| pair[0] == "--ui-check")
        .map(|pair| PathBuf::from(&pair[1]));
    #[cfg(feature = "ui-check")]
    ensure!(capture.is_none() || demo, "UI check requires --demo");
    let path = dirs::data_dir()
        .context("Cannot locate application data directory")?
        .join(backend::SERVICE)
        .join(if demo { "gpui-demo.db" } else { "mail.db" });
    std::fs::create_dir_all(path.parent().context("Missing data directory")?)?;
    let instance = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("gpui.lock"))?;
    instance
        .try_lock()
        .context("MunkiMail GPUI is already running for this database")?;
    let worker = Arc::new(Worker {
        sync_status: std::sync::Mutex::new(String::new()),
        state: Arc::new(runtime::AppState::new(path, demo)),
        runtime: Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
        ),
    });
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let start = || -> Result<()> {
                ely_core::init(cx)?;
                Theme::set_mode(Mode::Dark, cx);
                if let Some(dir) = ely_desktop::omarchy_current_dir() {
                    ely_desktop::follow_omarchy(dir, cx)?;
                }
                let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
                let window = cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(1000.), px(600.))),
                        titlebar: Some(TitlebarOptions {
                            title: Some("MunkiMail · GPUI".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| MailApp::new(worker, demo, window, cx)),
                )?;
                #[cfg(feature = "ui-check")]
                if let Some(dir) = capture {
                    ui_check::run(window, dir, cx);
                }
                #[cfg(not(feature = "ui-check"))]
                let _ = window;
                cx.on_window_closed(|cx, _| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
                .detach();
                cx.activate(true);
                Ok(())
            };
            if let Err(e) = start() {
                eprintln!("MunkiMail could not start: {e:#}");
                cx.quit();
            }
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_filter_read_and_mutate_preserve_account_ownership() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "munki-gpui-check-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let result = (|| -> Result<()> {
            let mut b = backend::Backend::open(path.clone(), true)?;
            let accounts: Vec<Account> =
                serde_json::from_value(b.local("bootstrap", json!({}))?["accounts"].clone())?;
            let first = accounts.first().context("Missing demo account")?.id;
            let second = b
                .store
                .add_account("second@example.test", "Second", "imap")?;
            let filter = Filter {
                account: None,
                view: "imbox".into(),
                query: String::new(),
                limit: 100,
            };
            let baseline = snapshot(&mut b, &filter, Operation::Refresh)?;
            assert!(!baseline.rows.is_empty());
            let empty = snapshot(
                &mut b,
                &Filter {
                    account: Some(second),
                    ..filter.clone()
                },
                Operation::Refresh,
            )?;
            assert_eq!(empty.total, 0);
            for uid in 1..=601 {
                b.store.upsert_message(
                    &munki_store::NewMessage {
                        account: second,
                        folder: "INBOX",
                        uid,
                        message_id: &format!("<gpui-{uid}@example.test>"),
                        thread_id: &format!("thread-{uid}"),
                        from: munki_core::Address::new("sender@example.test"),
                        subject: "Native aggregate fixture",
                        date: 1_900_000_000 + i64::from(uid),
                        seen: false,
                        has_attachment: false,
                        destination: munki_core::Destination::Imbox,
                    },
                    "Searchable native fixture",
                    "",
                )?;
            }
            let combined = snapshot(
                &mut b,
                &Filter {
                    limit: 650,
                    ..filter.clone()
                },
                Operation::Refresh,
            )?;
            assert_eq!(combined.total, baseline.total + 601);
            assert_eq!(combined.rows.len(), combined.total.min(650));
            assert!(combined
                .rows
                .windows(2)
                .all(|pair| pair[0].date >= pair[1].date));
            let scoped = snapshot(
                &mut b,
                &Filter {
                    account: Some(second),
                    query: "Native aggregate fixture".into(),
                    ..filter.clone()
                },
                Operation::Refresh,
            )?;
            assert_eq!(scoped.total, 601);
            assert_eq!(scoped.rows.len(), 100);
            assert!(scoped.rows.iter().all(|r| r.account == second));
            assert_eq!(line("subject\r\ncontinued"), "subject  continued");
            let row = baseline
                .rows
                .first()
                .context("Missing demo message")?
                .clone();
            assert!(snapshot(&mut b, &filter, Operation::Read(second, row.id)).is_err());
            let read = snapshot(&mut b, &filter, Operation::Read(first, row.id))?;
            assert!(read.detail.is_some());
            snapshot(
                &mut b,
                &filter,
                Operation::Local(
                    "set_stack",
                    json!({"account":first,"id":row.id,"stack":"reply_later","enabled":true}),
                ),
            )?;
            let stack = snapshot(
                &mut b,
                &Filter {
                    view: "reply_later".into(),
                    ..filter.clone()
                },
                Operation::Refresh,
            )?;
            assert!(stack.rows.iter().any(|r| r.id == row.id));
            assert!(snapshot(
                &mut b,
                &Filter {
                    account: Some(-1),
                    ..filter.clone()
                },
                Operation::Refresh
            )
            .is_err());
            assert!(snapshot(&mut b, &Filter { limit: 0, ..filter }, Operation::Refresh).is_err());
            Ok(())
        })();
        let _ = std::fs::remove_file(&path);
        result
    }
}
