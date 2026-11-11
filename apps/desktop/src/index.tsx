import { providerPresets } from "./providers";
import { folderTree, folderLabel, type ServerFolder, type FolderNode } from "./folders";
import { render } from "solid-js/web";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  createSignal,
  createEffect,
  onCleanup,
  onMount,
  on,
  For,
  Show,
} from "solid-js";
import {
  api,
  demo,
  blankDraft,
  readerDocument,
  type Account,
  type Message,
  type Draft,
  type Event,
  type Contact,
} from "./api";
import "./style.css";
import {
  applyTheme,
  cachedTheme,
  discoverOmarchy,
  rememberTheme,
  resolveTheme,
  readerColors,
  ThemePicker,
  type Palette,
} from "./theme";
applyTheme(cachedTheme(), null);
const views = [
  ["imbox", "MeBox", "◉"],
  ["feed", "Feeds", "▤"],
  ["paper_trail", "Trails", "▧"],
  ["screener", "Buffers", "◇"],
  ["reply_later", "Round2its", "◷"],
  ["set_aside", "Asides", "▱"],
  ["drafts", "Drafts", "✎"],
  ["sent", "Sent", "↗"],
];
const stamp = (n: number) =>
  new Date(n * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
const address = (s: string) => s.match(/<([^>]+)>/)?.[1] || s;
const split = (s: string) =>
  s
    .split(/[,;]/)
    .map((x) => x.trim())
    .filter(Boolean);
function download(filename: string, bytes: number[] | string) {
  const url = URL.createObjectURL(
    new Blob([typeof bytes === "string" ? bytes : new Uint8Array(bytes)]),
  );
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function App() {
  const [omarchy, setOmarchy] = createSignal<Palette | null>(null);
  let themeTimer: number | undefined;
  let refreshingTheme = false;
  async function refreshTheme() {
    if (refreshingTheme) return;
    refreshingTheme = true;
    try {
      const next = await discoverOmarchy();
      setOmarchy((previous) =>
        JSON.stringify(previous) === JSON.stringify(next) ? previous : next,
      );
    } finally {
      refreshingTheme = false;
    }
  }
  const [serverFolders,setServerFolders]=createSignal<ServerFolder[]>([]);
  async function refreshFolders(owners=accounts()){const results=await Promise.all(owners.filter(a=>a.kind!=="jmap").map(async a=>{const r=await api("folders",{account:a.id});return (r.folders||[]).map((f:ServerFolder)=>({...f,account:a.id}));}));setServerFolders(results.flat());}
  const [ready, setReady] = createSignal(false),
    [bootError, setBootError] = createSignal(""),
    [accounts, setAccounts] = createSignal<Account[]>([]),
    [account, setAccount] = createSignal(""),
    [accountScope, setAccountScope] = createSignal(""),
    [settings, setSettings] = createSignal<Record<string, any>>({
      theme: cachedTheme() ?? "auto",
    }),
    [section, setSection] = createSignal("mail"),
    [view, setView] = createSignal("imbox"),
    [query, setQuery] = createSignal(""),
    [messages, setMessages] = createSignal<Message[]>([]),
    [total, setTotal] = createSignal(0),
    [selected, setSelected] = createSignal<number | null>(null),
    [detail, setDetail] = createSignal<any>(null),
    [busy, setBusy] = createSignal(0),
    [error, setError] = createSignal(""),
    [notice, setNotice] = createSignal("", { equals: false }),
    [modal, setModal] = createSignal(""),
    [checked, setChecked] = createSignal<number[]>([]),
    [scroll, setScroll] = createSignal(0),
    [remote, setRemote] = createSignal(false),
    [html, setHtml] = createSignal(false);
  const [noticeFading, setNoticeFading] = createSignal(false);
  const hasPendingIssues = () => /\b[1-9]\d* pending\b/.test(notice());
  createEffect(() => {
    setNoticeFading(false);
    if (!notice() || hasPendingIssues()) return;
    const fade = setTimeout(() => setNoticeFading(true), 9_500);
    const timer = setTimeout(() => setNotice(""), 10_000);
    onCleanup(() => { clearTimeout(fade); clearTimeout(timer); });
  });
  const [draft, setDraft] = createSignal<Draft>(blankDraft()),
    [draftId, setDraftId] = createSignal<number | null>(null),
    [draftRows, setDraftRows] = createSignal<any[]>([]),
    [saveState, setSaveState] = createSignal(""),
    [dirty, setDirty] = createSignal(false),
    [events, setEvents] = createSignal<Event[]>([]),
    [contacts, setContacts] = createSignal<Contact[]>([]),
    [person, setPerson] = createSignal<any>(null),
    [date, setDate] = createSignal(new Date()),
    [calendarView, setCalendarView] = createSignal("month"),
    [editEvent, setEditEvent] = createSignal<Event>(),
    [editContact, setEditContact] = createSignal<Contact>(),
    [aiKey, setAiKey] = createSignal(""),
    [davPassword, setDavPassword] = createSignal("");
  let search!: HTMLInputElement;
  let modalNode!: HTMLDivElement;
  let lastFocus: HTMLElement | null = null;
  let listRequest = 0,
    detailRequest = 0;
  let messageScroll: HTMLDivElement | undefined;
  let saving: Promise<void> | null = null;
  async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
    setBusy((n) => n + 1);
    setError("");
    try {
      return await fn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy((n) => n - 1);
    }
  }
  async function bootstrap() {
    setBootError("");
    try {
      const b = await api("bootstrap");
      setAccounts(b.accounts.map((a: Account) => ({ ...a, id: String(a.id) })));
      setAccount(String(b.accounts[0]?.id || ""));
      const s = b.settings || {};
      setSettings({
        ...s,
        theme: cachedTheme() ?? s.theme ?? "auto",
        ai_enabled: s.ai?.enabled ?? s.ai_enabled ?? false,
        consent_to_send_contents:
          s.ai?.consent_to_send_contents ?? s.consent_to_send_contents ?? false,
        ai_provider: s.ai?.provider ?? s.ai_provider ?? "anthropic",
        ai_endpoint: s.ai?.endpoint ?? s.ai_endpoint ?? "",
        ai_model: s.ai?.model ?? s.ai_model ?? "",
        caldav_url: s.dav?.calendar_url ?? s.caldav_url ?? "",
        carddav_url: s.dav?.contacts_url ?? s.carddav_url ?? "",
        dav_username: s.dav?.username ?? s.dav_username ?? "",
      });
      if(!demo) await refreshFolders(b.accounts.map((a:Account)=>({...a,id:String(a.id)})));
      setReady(true);
      if (!b.accounts.length) setModal("settings");
    } catch (e) {
      setBootError(
        `Unable to unlock your mail store. ${e instanceof Error ? e.message : String(e)}`,
      );
    }
  }
  const scopedAccounts = () => accounts().filter((a) => !accountScope() || a.id === accountScope());
  const messageAccount = () => String(detail()?.message.account ?? account());
  let accountMenu!: HTMLDivElement;
  function showAccountMenu(x: number, y: number) {
    accountMenu.showPopover();
    accountMenu.style.left = `${Math.max(8, Math.min(x, window.innerWidth - accountMenu.offsetWidth - 8))}px`;
    accountMenu.style.top = `${Math.max(8, Math.min(y, window.innerHeight - accountMenu.offsetHeight - 8))}px`;
    accountMenu.querySelector<HTMLButtonElement>("button")?.focus();
  }
  function chooseAccount(id: string) {
    if(view().startsWith("folder:"))setView("imbox");
    setAccountScope(id);
    if (id) setAccount(id);
    accountMenu.hidePopover();
  }
  async function messagePage(offset: number, ids: string[], v: string, q: string) {
    if (ids.length === 1) return api("messages", { account: ids[0], view: v, query: q, limit: 100, offset });
    const results = await Promise.all(ids.map(async (id) => {
      const rows: Message[] = [];
      let total = 0;
      // ponytail: merge account prefixes; move pagination into SQL if account volume makes this expensive.
      for (let start = 0; start <= offset; start += 100) {
        const r = await api("messages", { account: id, view: v, query: q, limit: 100, offset: start });
        rows.push(...r.messages);
        total = r.total;
        if (rows.length >= total) break;
      }
      return { rows, total };
    }));
    return {
      messages: results.flatMap((r) => r.rows).sort((a, b) => b.date - a.date || b.id - a.id).slice(offset, offset + 100),
      total: results.reduce((sum, r) => sum + r.total, 0),
    };
  }
  async function load() {
    const n = ++listRequest;
    const ids = scopedAccounts().map((a) => a.id), v = view(), q = query();
    if (!ids.length) { setMessages([]); setDraftRows([]); setTotal(0); return; }
    await run(async () => {
      if (v === "drafts") {
        const rows = await Promise.all(ids.map(async (id) => {
          const r = await api("drafts", { account: id });
          return r.drafts.map((d: any) => ({ ...d, account: id }));
        }));
        if (n === listRequest) setDraftRows(rows.flat().sort((a, b) => b.updated_at - a.updated_at || b.id - a.id));
        return;
      }
      const r = await messagePage(0, ids, v, q);
      if (n === listRequest) { setMessages(r.messages); setTotal(r.total); }
    });
  }
  async function loadMore() {
    const n = listRequest, ids = scopedAccounts().map((a) => a.id), v = view(), q = query(), offset = messages().length;
    await run(async () => {
      const r = await messagePage(offset, ids, v, q);
      if (n === listRequest) { setMessages((ms) => [...ms, ...r.messages]); setTotal(r.total); }
    });
  }
  createEffect(() => {
    accountScope();
    accounts();
    view();
    query();
    ++detailRequest;
    setSelected(null);
    setDetail(null);
    setChecked([]);
    setScroll(0);
    void load();
  });
  createEffect(() => {
    applyTheme(settings().theme, omarchy());
    document.documentElement.dataset.density =
      settings().density || "comfortable";
  });
  createEffect(() => {
    const m = modal();
    if (m) {
      lastFocus = document.activeElement as HTMLElement;
      queueMicrotask(() =>
        modalNode
          ?.querySelector<HTMLElement>("input,button,select,textarea")
          ?.focus(),
      );
    } else lastFocus?.focus();
  });
  async function openMessage(m: Message) {
    const n = ++detailRequest;
    setSelected(m.id);
    setDetail(null);
    setRemote(false);
    setHtml(false);
    await run(async () => {
      const d = await api("message", {
        account: m.account,
        id: m.id,
        remote: false,
      });
      if (n === detailRequest) {
        setDetail(d);
        setMessages((ms) =>
          ms.map((x) => (x.id === m.id ? { ...x, seen: true } : x)),
        );
      }
    });
  }
  function compose(
    d = blankDraft(accounts().find((a) => a.id === account())?.email),
  ) {
    const sender = accounts().find((a) => a.email === d.from);
    if (sender) setAccount(sender.id);
    setDraft(d);
    setDraftId(null);
    setDirty(false);
    setSaveState("");
    setModal("compose");
  }
  function changeDraft<K extends keyof Draft>(key: K, value: Draft[K]) {
    setDraft((d) => ({ ...d, [key]: value }));
    setDirty(true);
    setSaveState("Unsaved");
  }
  async function saveDraft() {
    if (saving) {
      await saving;
      if (!dirty()) return;
    }
    const snapshot = structuredClone(draft());
    setSaveState("Saving…");
    saving = (async () => {
      await run(async () => {
        const r = await api("save_draft", {
          account: account(),
          id: draftId(),
          draft: snapshot,
        });
        setDraftId(r.id);
        if (JSON.stringify(snapshot) === JSON.stringify(draft()))
          setDirty(false);
        setSaveState("Saved");
      });
    })();
    await saving;
    saving = null;
  }
  createEffect(() => {
    if (modal() === "compose" && dirty()) {
      draft();
      const timer = setTimeout(() => void saveDraft(), 1400);
      onCleanup(() => clearTimeout(timer));
    }
  });
  async function closeModal() {
    if (modal() === "compose" && dirty()) {
      await saveDraft();
      if (dirty()) return;
    }
    setModal("");
    setAiKey("");
    setDavPassword("");
  }
  async function route(destination: string) {
    await run(async () => {
      for (const a of scopedAccounts()) {
        const senders = [...new Set(messages().filter((m) => String(m.account) === a.id && checked().includes(m.id)).map((m) => address(m.from)))].map((address) => ({ address, destination }));
        if (senders.length) await api("triage", { account: a.id, senders });
      }
      setChecked([]);
      await load();
      setNotice("Sender preferences updated");
    });
  }
  async function refreshPim() {
    const center = +date() / 1000;
    await run(async () => {
      const [c, e] = await Promise.all([
        api("contacts"),
        api("events", {
          start: Math.floor(center - 183 * 86400),
          end: Math.floor(center + 183 * 86400),
        }),
      ]);
      setContacts(c.contacts);
      setEvents(e.events);
    });
  }
  createEffect(() => {
    if (section() !== "mail") void refreshPim();
  });
  async function showPerson(email: string) {
    await run(async () => {
      setPerson(await api("person", { email }));
      setModal("person");
    });
  }
  function newEvent(day = date()) {
    const start = new Date(day);
    start.setHours(10, 0, 0, 0);
    setEditEvent({
      uid: "",
      title: "",
      description: "",
      location: "",
      start: +start / 1000,
      end: +start / 1000 + 3600,
      all_day: false,
      attendees: [],
    });
    setModal("event");
  }
  function keydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      void closeModal();
      return;
    }
    if (modal()) {
      if (e.key === "Tab") {
        const nodes = Array.from(
          modalNode.querySelectorAll<HTMLElement>(
            'button,input,textarea,select,[tabindex="0"]',
          ),
        ).filter((n) => !n.hasAttribute("disabled"));
        if (e.shiftKey && document.activeElement === nodes[0]) {
          e.preventDefault();
          nodes.at(-1)?.focus();
        } else if (!e.shiftKey && document.activeElement === nodes.at(-1)) {
          e.preventDefault();
          nodes[0]?.focus();
        }
      }
      return;
    }
    if (
      (e.target as HTMLElement).closest(
        "input,textarea,select,[contenteditable]",
      ) ||
      e.metaKey ||
      e.ctrlKey ||
      e.altKey
    )
      return;
    if (e.key === "/") {
      e.preventDefault();
      search.focus();
    }
    if (e.key === "c") compose();
    if (e.key === "j" || e.key === "k") {
      e.preventDefault();
      const i = messages().findIndex((m) => m.id === selected());
      const next =
        messages()[
          Math.max(
            0,
            Math.min(messages().length - 1, i + (e.key === "j" ? 1 : -1)),
          )
        ];
      if (next) {
        void openMessage(next);
        if (messageScroll) {
          const top = messages().indexOf(next) * 100;
          if (
            top < messageScroll.scrollTop ||
            top + 100 > messageScroll.scrollTop + messageScroll.clientHeight
          )
            messageScroll.scrollTop = top;
          setScroll(messageScroll.scrollTop);
        }
      }
    }
  }
  const [syncStatus, setSyncStatus] = createSignal<Record<string, string>>({});
  let unlisten: UnlistenFn | undefined,
    disposed = false;
  onMount(() => {
    void refreshTheme();
    themeTimer = window.setInterval(() => void refreshTheme(), 2_000);
    if (!demo)
      void listen<{
        account: string | number;
        error?: string;
        result?: { fetched: number };
      }>("sync-status", (event) => {
        const status = event.payload,
          id = String(status.account);
        setSyncStatus((s) => ({
          ...s,
          [id]: status.error
            ? `Sync needs attention: ${status.error}`
            : `Synced ${new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`,
        }));
        if ((!accountScope() || id === accountScope()) && !status.error) {void load();if(!demo)void refreshFolders().catch(e=>setError(String(e)));}
      })
        .then((stop) => {
          if (disposed) stop();
          else unlisten = stop;
        })
        .catch((e) =>
          setError(`Could not listen for sync updates: ${String(e)}`),
        );
    void bootstrap();
    window.addEventListener("keydown", keydown);
  });
  onCleanup(() => {
    disposed = true;
    window.clearInterval(themeTimer);
    unlisten?.();
    window.removeEventListener("keydown", keydown);
  });
  const days = () => {
    const d = date();
    const start = new Date(
      d.getFullYear(),
      d.getMonth(),
      calendarView() === "month" ? 1 : d.getDate(),
    );
    if (calendarView() !== "day")
      start.setDate(start.getDate() - start.getDay());
    return Array.from(
      {
        length:
          calendarView() === "month" ? 42 : calendarView() === "week" ? 7 : 1,
      },
      (_, i) =>
        new Date(start.getFullYear(), start.getMonth(), start.getDate() + i),
    );
  };
  const field = (
    label: string,
    value: string,
    onInput: (s: string) => void,
    type = "text",
    required = false,
  ) => (
    <label>
      {label}
      <input
        type={type}
        value={value}
        required={required}
        onInput={(e) => onInput(e.currentTarget.value)}
      />
    </label>
  );
  const setting = (key: string, value: unknown) =>
    setSettings((s) => ({ ...s, [key]: value }));
  const toggle = (label: string, key: string) => (
    <label class="toggle">
      <input
        type="checkbox"
        checked={!!settings()[key]}
        onChange={(e) => setting(key, e.currentTarget.checked)}
      />
      {label}
    </label>
  );
  return (
    <Show
      when={ready()}
      fallback={
        <main class="boot">
          <div class="brand">
            m<span>•</span>
          </div>
          <h1>MunkiMail</h1>
          <p>{bootError() || "Opening your space…"}</p>
          <Show when={bootError()}>
            <button onClick={bootstrap}>Retry</button>
          </Show>
        </main>
      }
    >
      <div
        class="shell"
        classList={{ "reader-wide": settings().layout === "reader" }}
      >
        <aside class="sidebar">
          <div class="wordmark">
            <span class="brand">
              m<span>•</span>
            </span>{" "}
            munki<span>mail</span>
          </div>
          <button class="compose primary" onClick={() => compose()}>
            ＋ New message <kbd>C</kbd>
          </button>
          <nav aria-label="Mailboxes">
            <button class="nav-label account-scope" aria-haspopup="dialog" aria-controls="account-menu"
              onClick={(e) => { const rect = e.currentTarget.getBoundingClientRect(); showAccountMenu(rect.left, rect.bottom); }}
              onContextMenu={(e) => { e.preventDefault(); const rect = e.currentTarget.getBoundingClientRect(); showAccountMenu(e.clientX || rect.left, e.clientY || rect.bottom); }}>
              YOUR SPACE<Show when={accountScope()}><span> · {accounts().find((a) => a.id === accountScope())?.email}</span></Show>
            </button>
            <div id="account-menu" ref={accountMenu} popover="auto" role="dialog" aria-label="Filter mail accounts">
              <For each={[{ id: "", email: "All" }, ...accounts()]}>{(a) =>
                <button aria-pressed={accountScope() === a.id} onClick={() => chooseAccount(a.id)}>{a.email}</button>
              }</For>
            </div>
            <For each={views}>
              {([id, label, icon]) => (
                <button
                  classList={{ active: section() === "mail" && view() === id }}
                  onClick={() => {
                    setSection("mail");
                    setView(id);
                  }}
                >
                  <span class="nav-icon">{icon}</span>
                  {label}
                  <Show
                    when={
                      id === "imbox" &&
                      section() === "mail" &&
                      view() === "imbox"
                    }
                  >
                    <span class="count">{total()}</span>
                  </Show>
                </button>
              )}
            </For>
            <Show when={serverFolders().length || (!demo && accounts().some(a=>a.kind!=="jmap"))}>
              <div class="nav-label second">SERVER FOLDERS</div>
              <For each={scopedAccounts().filter(a=>a.kind!=="jmap")}>
                {(a)=><details class="folder-account" open><summary>{a.email}</summary>
                  <FolderBranch nodes={folderTree(serverFolders().filter(f=>f.account===a.id))} active={section()==="mail"&&accountScope()===a.id?view():""} onOpen={(f)=>{
                    setAccountScope(a.id);setAccount(a.id);setSection("mail");setQuery("");setView(`folder:${f.name}`);
                    void run(async()=>{await api("sync_folder",{account:a.id,folder:f.name});await refreshFolders();if(accountScope()===a.id&&view()===`folder:${f.name}`)await load();});
                  }}/>
                  <Show when={!serverFolders().some(f=>f.account===a.id)}><p class="quiet">Sync this account to discover folders.</p></Show>
                </details>}
              </For>
            </Show>
            <div class="nav-label second">EVERYTHING ELSE</div>
            <button
              classList={{ active: section() === "calendar" }}
              onClick={() => setSection("calendar")}
            >
              <span class="nav-icon">▦</span>Calendar
            </button>
            <button
              classList={{ active: section() === "contacts" }}
              onClick={() => setSection("contacts")}
            >
              <span class="nav-icon">♧</span>People
            </button>
          </nav>
          <div class="sidebar-bottom">
            <label class="account-label">
              ACCOUNT
              <select
                aria-label="Active account"
                value={accountScope()}
                onChange={(e) => chooseAccount(e.currentTarget.value)}
              >
                <option value="">All</option>
                <For each={accounts()}>
                  {(a) => <option value={a.id}>{a.display || a.email}</option>}
                </For>
              </select>
            </label>
            <button onClick={() => setModal("settings")}>
              ⚙ <span>Settings & accounts</span>
            </button>
            <div class="quiet" role="status">
              {syncStatus()[account()] || "A calmer corner of the internet."}
            </div>
          </div>
        </aside>
        <main class="workspace">
          <Show when={demo}>
            <div class="demo-banner">
              DEMO MODE{" "}
              <span>
                Local sample data · no mail is sent · credentials stay out
              </span>
            </div>
          </Show>
          <header class="topbar">
            <div class="breadcrumb">
              Your space <span>/</span>{" "}
              {section() === "mail"
                ? (view().startsWith("folder:") ? folderLabel(view().slice(7)) : views.find((v) => v[0] === view())?.[1])
                : section() === "calendar"
                  ? "Calendar"
                  : "People"}
            </div>
            <div class="search">
              <span>⌕</span>
              <input
                ref={search}
                aria-label="Search messages"
                placeholder="Search your mail"
                value={query()}
                onInput={(e) => {
                  setSection("mail");
                  setQuery(e.currentTarget.value);
                }}
              />
              <kbd>/</kbd>
            </div>
            <Show
              when={
                settings().ai_enabled && settings().consent_to_send_contents
              }
            >
              <button
                title="Translate search with AI"
                aria-label="Translate search with AI"
                disabled={!query() || !!busy()}
                onClick={() =>
                  run(async () => {
                    const r = await api("ai", {
                      capability: "translate_query",
                      text: query(),
                    });
                    if (r.query) setQuery(r.query);
                  })
                }
              >
                ✧
              </button>
            </Show>
            <button
              title="Sync mail"
              aria-label="Sync mail"
              disabled={!!busy() || !account()}
              onClick={() =>
                run(async () => {
                  const results = await Promise.all(scopedAccounts().map((a) => view().startsWith("folder:") ? api("sync_folder",{account:a.id,folder:view().slice(7)}) : api("sync", { account: a.id })));
                  if(!demo)await refreshFolders();
                  const r = { fetched: results.reduce((sum, r) => sum + (r.fetched || 0), 0), pending: results.reduce((sum, r) => sum + (r.pending || 0), 0) };
                  setNotice(
                    `Sync complete · ${r.fetched} fetched · ${r.pending} pending`,
                  );
                  await load();
                })
              }
            >
              ↻
            </button>
          <Show when={notice()}>
            <div class="notice" classList={{ "notice-fading": noticeFading() }} role="status">
              {notice()}
              <Show when={hasPendingIssues()}><button onClick={()=>setModal("pending")}>Pending actions</button></Show>
              <button
                aria-label="Dismiss notification"
                onClick={() => setNotice("")}
              >
                ×
              </button>
            </div>
          </Show>
          </header>
          <Show when={error()}>
            <div class="alert" role="alert">
              {error()}
              <button aria-label="Dismiss error" onClick={() => setError("")}>
                ×
              </button>
            </div>
          </Show>
          <Show when={busy()}>
            <div class="progress" role="status" aria-label="Working" />
          </Show>
          <Show when={section() === "mail"}>
            <div class="mail-layout">
              <section class="message-pane" aria-label="Message list">
                <div class="pane-heading">
                  <div>
                    <div class="eyebrow">MAKE ROOM FOR WHAT MATTERS</div>
                    <h1>{view().startsWith("folder:") ? folderLabel(view().slice(7)) : views.find((v) => v[0] === view())?.[1]}</h1>
                  </div>
                  <span class="pill">
                    {view() === "drafts" ? draftRows().length : total()}
                  </span>
                </div>
                <p class="pane-subtitle">
                  {view().startsWith("folder:") ? "Server folder · browsing preserves folders and labels. Read/unread syncs across devices." : view() === "screener"
                    ? "You decide who gets your attention."
                    : view() === "feed"
                      ? "Read something good. On your terms."
                      : view() === "paper_trail"
                        ? "Receipts, records, and the little details."
                        : "A little less noise. A little more space."}
                </p>
                <Show when={view() === "screener"}>
                  <div class="triage">
                    <label>
                      <input
                        type="checkbox"
                        aria-label="Select all senders"
                        checked={
                          messages().length > 0 &&
                          checked().length === messages().length
                        }
                        onChange={(e) =>
                          setChecked(
                            e.currentTarget.checked
                              ? messages().map((m) => m.id)
                              : [],
                          )
                        }
                      />{" "}
                      All
                    </label>
                    <For each={["imbox", "feed", "paper_trail", "dismiss"]}>
                      {(d) => (
                        <button
                          disabled={!checked().length || !!busy()}
                          onClick={() => route(d)}
                        >
                          {d === "imbox"
                            ? "Approve"
                            : d === "paper_trail"
                              ? "Trails"
                              : d === "dismiss"
                                ? "Dismiss"
                                : "Feeds"}
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
                <Show
                  when={view() === "drafts"}
                  fallback={
                    <div
                      class="message-scroll"
                      ref={messageScroll}
                      onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
                    >
                      <div
                        style={{
                          height: `${messages().length * 100}px`,
                          position: "relative",
                        }}
                      >
                        <For
                          each={messages().slice(
                            Math.max(0, Math.floor(scroll() / 100) - 4),
                            Math.floor(scroll() / 100) + 18,
                          )}
                        >
                          {(m) => (
                            <div
                              id={`message-${m.id}`}
                              class="message-row"
                              classList={{
                                selected: selected() === m.id,
                                unread: !m.seen,
                              }}
                              style={{
                                position: "absolute",
                                top: `${messages().indexOf(m) * 100}px`,
                                height: "100px",
                                width: "100%",
                              }}
                            >
                              <Show when={view() === "screener"}>
                                <input
                                  class="row-check"
                                  type="checkbox"
                                  aria-label={`Select ${m.from}`}
                                  checked={checked().includes(m.id)}
                                  onChange={(e) =>
                                    setChecked((ids) =>
                                      e.currentTarget.checked
                                        ? [...ids, m.id]
                                        : ids.filter((id) => id !== m.id),
                                    )
                                  }
                                />
                              </Show>
                              <button
                                class="message-button"
                                onClick={() => openMessage(m)}
                              >
                                <div class="message-meta">
                                  <strong>{m.from.split(" <")[0]}</strong>
                                  <time>{stamp(m.date)}</time>
                                </div>
                                <div class="message-subject">
                                  {!m.seen && <span class="unread-dot" />}
                                  {m.renamed_subject ||
                                    m.subject ||
                                    "(No subject)"}{" "}
                                  {m.has_attachment ? "⌁" : ""}
                                </div>
                                <p>
                                  {m.snippet || "Open to read this message"}
                                </p>
                              </button>
                            </div>
                          )}
                        </For>
                      </div>
                      <Show when={!messages().length && !busy()}>
                        <div class="empty">
                          <span>☀</span>
                          <h2>Room to breathe.</h2>
                          <p>
                            {query()
                              ? "No messages match your search."
                              : "You’re all caught up here."}
                          </p>
                        </div>
                      </Show>
                    </div>
                  }
                >
                  <div class="draft-list">
                    <For each={draftRows()}>
                      {(d) => (
                        <button
                          onClick={() =>
                            run(async () => {
                              const data = await api("draft", {
                                account: d.account,
                                id: d.id,
                              });
                              setAccount(String(d.account));
                              compose(data);
                              setDraftId(d.id);
                            })
                          }
                        >
                          <strong>{d.subject || "Untitled draft"}</strong>
                          <small>
                            {d.to.join(", ") || "No recipients"} · {d.state}
                          </small>
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
                <Show when={view() !== "drafts" && messages().length < total()}>
                  <button
                    class="load-more"
                    disabled={!!busy()}
                    onClick={loadMore}
                  >
                    Load more · {messages().length} of {total()}
                  </button>
                </Show>
                <footer class="list-footer">
                  <span>{messages().filter((m) => !m.seen).length} unread</span>
                  <span>
                    <kbd>J</kbd> <kbd>K</kbd> to move
                  </span>
                </footer>
              </section>
              <section class="reader" aria-label="Message reader">
                <Show
                  when={detail()}
                  fallback={
                    <div class="reader-empty">
                      <div class="sun">✳</div>
                      <div class="eyebrow">LESS NOISE, MORE MEANING</div>
                      <h2>Your attention belongs to you.</h2>
                      <p>Pick a message. Take your time.</p>
                    </div>
                  }
                >
                  {(d) => (
                    <>
                      <div class="reader-toolbar">
                        <button
                          onClick={() =>
                            run(async () => {
                              await api("set_stack", {
                                account: messageAccount(),
                                id: selected(),
                                stack: "reply_later",
                                enabled: true,
                              });
                              setNotice("Added to Round2its");
                            })
                          }
                        >
                          ◷ Round2its
                        </button>
                        <button
                          onClick={() =>
                            run(async () => {
                              await api("set_stack", {
                                account: messageAccount(),
                                id: selected(),
                                stack: "set_aside",
                                enabled: true,
                              });
                              setNotice("Added to Asides");
                            })
                          }
                        >
                          ▱ Asides
                        </button>
                        <Show
                          when={
                            view() === "reply_later" || view() === "set_aside"
                          }
                        >
                          <button
                            onClick={() =>
                              run(async () => {
                                await api("set_stack", {
                                  account: messageAccount(),
                                  id: selected(),
                                  stack: view(),
                                  enabled: false,
                                });
                                await load();
                                setDetail(null);
                              })
                            }
                          >
                            Remove
                          </button>
                        </Show>
                        <button
                          onClick={() => setModal("message-actions")}
                          aria-label="More message actions"
                        >
                          •••
                        </button>
                      </div>
                      <article>
                        <div class="eyebrow">
                          {stamp(d().message.date)} ·{" "}
                          {new Date(d().message.date * 1000).toLocaleTimeString(
                            [],
                            { hour: "2-digit", minute: "2-digit" },
                          )}
                        </div>
                        <h1>
                          {d().message.renamed_subject || d().message.subject}
                        </h1>
                        <div class="sender">
                          <div class="avatar">{d().message.from[0]}</div>
                          <div>
                            <button
                              onClick={() =>
                                showPerson(address(d().message.from))
                              }
                            >
                              <strong>{d().message.from.split(" <")[0]}</strong>
                            </button>
                            <small>{address(d().message.from)}</small>
                          </div>
                          <span class="pill">To you</span>
                        </div>
                        <Show when={d().html}>
                          <div class="html-controls">
                            <button onClick={() => setHtml(!html())}>
                              {html() ? "Plain text" : "HTML view"}
                            </button>
                            <Show when={!remote()}>
                              <button
                                onClick={() =>
                                  run(async () => {
                                    const updated = await api("message", {
                                      account: messageAccount(),
                                      id: selected(),
                                      remote: true,
                                    });
                                    setRemote(true);
                                    setDetail(updated);
                                    setHtml(true);
                                  })
                                }
                              >
                                Allow remote images for this message
                              </button>
                            </Show>
                          </div>
                        </Show>
                        <Show
                          when={html() && d().html}
                          fallback={
                            <div class="message-body">
                              {d().text ||
                                "This message has no plain text body. Use HTML view if available."}
                            </div>
                          }
                        >
                          <iframe
                            title="Message HTML"
                            sandbox=""
                            referrerpolicy="no-referrer"
                            srcdoc={readerDocument(
                              d().html,
                              remote(),
                              readerColors(settings().theme, omarchy()),
                            )}
                          />
                        </Show>
                        <For each={d().attachments}>
                          {(a: any) => (
                            <button
                              class="attachment"
                              onClick={() =>
                                run(async () => {
                                  const f = await api("download_attachment", {
                                    account: messageAccount(),
                                    id: selected(),
                                    index: a.index,
                                  });
                                  download(f.filename, f.bytes);
                                })
                              }
                            >
                              ↓ {a.filename}{" "}
                              <small>{Math.ceil(a.size / 1024)} KB</small>
                            </button>
                          )}
                        </For>
                        <Show when={d().links?.length}>
                          <details>
                            <summary>Links in this message</summary>
                            <For each={d().links}>
                              {(link: { url: string; label: string }) => (
                                <button
                                  class="external-link"
                                  onClick={() =>
                                    run(async () => {
                                      const url = new URL(link.url);
                                      if (
                                        ![
                                          "https:",
                                          "http:",
                                          "mailto:",
                                        ].includes(url.protocol)
                                      )
                                        throw Error(
                                          "Only HTTP, HTTPS, and mail links can be opened.",
                                        );
                                      await api("open_url", { url: link.url });
                                    })
                                  }
                                >
                                  {link.label || link.url} ↗
                                </button>
                              )}
                            </For>
                          </details>
                        </Show>
                        <Show
                          when={
                            d().invite ||
                            d().calendar_invite ||
                            d().attachments?.some(
                              (a: any) => a.content_type === "text/calendar",
                            )
                          }
                        >
                          <div class="rsvp">
                            <span>Calendar invitation</span>
                            <For each={["ACCEPTED", "TENTATIVE", "DECLINED"]}>
                              {(response) => (
                                <button
                                  disabled={!!busy()}
                                  onClick={() =>
                                    run(async () => {
                                      const message = d().message;
                                      await api("rsvp", {
                                        account: messageAccount(),
                                        id: selected(),
                                        response,
                                        revision: d().invite_revision,
                                      });
                                      await openMessage(message);
                                      setNotice(
                                        demo
                                          ? "Demo invitation response recorded locally. No mail was sent."
                                          : "Invitation response queued",
                                      );
                                    })
                                  }
                                >
                                  {response === "ACCEPTED"
                                    ? "Accept"
                                    : response === "TENTATIVE"
                                      ? "Maybe"
                                      : "Decline"}
                                </button>
                              )}
                            </For>
                          </div>
                        </Show>
                        <div class="reply-bar">
                          <button
                            class="primary"
                            onClick={() =>
                              run(async () =>
                                compose(
                                  await api("reply", {
                                    account: messageAccount(),
                                    id: selected(),
                                    forward: false,
                                  }),
                                ),
                              )
                            }
                          >
                            ↩ Reply
                          </button>
                          <button
                            onClick={() =>
                              run(async () =>
                                compose(
                                  await api("reply", {
                                    account: messageAccount(),
                                    id: selected(),
                                    forward: true,
                                  }),
                                ),
                              )
                            }
                          >
                            Forward ↗
                          </button>
                        </div>
                      </article>
                    </>
                  )}
                </Show>
              </section>
            </div>
          </Show>
          <Show when={section() === "calendar"}>
            <section class="pim">
              <div class="page-heading">
                <div>
                  <div class="eyebrow">SPACE FOR YOUR DAYS</div>
                  <h1>
                    {date().toLocaleDateString(undefined, {
                      month: "long",
                      year: "numeric",
                    })}
                  </h1>
                </div>
                <div class="actions">
                  <button
                    onClick={() => {
                      const d = new Date(date());
                      if (calendarView() === "month")
                        d.setMonth(d.getMonth() - 1);
                      else
                        d.setDate(
                          d.getDate() - (calendarView() === "week" ? 7 : 1),
                        );
                      setDate(d);
                    }}
                  >
                    ‹
                  </button>
                  <button onClick={() => setDate(new Date())}>Today</button>
                  <button
                    onClick={() => {
                      const d = new Date(date());
                      if (calendarView() === "month")
                        d.setMonth(d.getMonth() + 1);
                      else
                        d.setDate(
                          d.getDate() + (calendarView() === "week" ? 7 : 1),
                        );
                      setDate(d);
                    }}
                  >
                    ›
                  </button>
                  <select
                    aria-label="Calendar view"
                    value={calendarView()}
                    onChange={(e) => setCalendarView(e.currentTarget.value)}
                  >
                    <option value="month">Month</option>
                    <option value="week">Week</option>
                    <option value="day">Day</option>
                  </select>
                  <button class="primary" onClick={() => newEvent()}>
                    ＋ Event
                  </button>
                </div>
              </div>
              <div
                class="calendar"
                classList={{ single: calendarView() === "day" }}
              >
                <For each={days()}>
                  {(day) => (
                    <div
                      class="calendar-day"
                      classList={{
                        muted: day.getMonth() !== date().getMonth(),
                        today: day.toDateString() === new Date().toDateString(),
                      }}
                    >
                      <button class="day-number" onClick={() => newEvent(day)}>
                        {day.toLocaleDateString(undefined, {
                          weekday: "short",
                          day: "numeric",
                        })}
                      </button>
                      <For
                        each={events().filter((e) =>
                          e.all_day
                            ? (() => {
                                const dayString = new Date(
                                  +day - day.getTimezoneOffset() * 60000,
                                )
                                  .toISOString()
                                  .slice(0, 10);
                                return (
                                  dayString >=
                                    (e.start_date ||
                                      new Date(e.start * 1000)
                                        .toISOString()
                                        .slice(0, 10)) &&
                                  dayString <
                                    (e.end_date ||
                                      new Date(e.end * 1000)
                                        .toISOString()
                                        .slice(0, 10))
                                );
                              })()
                            : e.start <
                                +new Date(
                                  day.getFullYear(),
                                  day.getMonth(),
                                  day.getDate() + 1,
                                ) /
                                  1000 && e.end > +day / 1000,
                        )}
                      >
                        {(e) => (
                          <button
                            class="event-chip"
                            onClick={() => {
                              setEditEvent({ ...e });
                              setModal("event");
                            }}
                          >
                            {!e.all_day &&
                              new Date(e.start * 1000).toLocaleTimeString([], {
                                hour: "2-digit",
                                minute: "2-digit",
                              })}{" "}
                            {e.title}
                          </button>
                        )}
                      </For>
                    </div>
                  )}
                </For>
              </div>
            </section>
          </Show>
          <Show when={section() === "contacts"}>
            <section class="pim">
              <div class="page-heading">
                <div>
                  <div class="eyebrow">GOOD PEOPLE, ALL TOGETHER</div>
                  <h1>
                    Your people <span class="pill">{contacts().length}</span>
                  </h1>
                </div>
                <button
                  class="primary"
                  onClick={() => {
                    setEditContact({ uid: "", name: "", email: "", phone: "" });
                    setModal("contact");
                  }}
                >
                  ＋ Add person
                </button>
              </div>
              <div class="contacts-grid">
                <For each={contacts()}>
                  {(c) => (
                    <div class="contact-card">
                      <div class="avatar">{c.name[0] || "?"}</div>
                      <h2>{c.name}</h2>
                      <p>{c.email}</p>
                      <small>{c.phone}</small>
                      <div class="actions">
                        <button onClick={() => showPerson(c.email)}>
                          Conversations
                        </button>
                        <button
                          onClick={() => {
                            setEditContact({ ...c });
                            setModal("contact");
                          }}
                        >
                          Edit
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
              <Show when={!contacts().length}>
                <div class="empty">
                  <h2>Start with someone you know.</h2>
                  <p>Add a contact or sync your address book in Settings.</p>
                </div>
              </Show>
            </section>
          </Show>
        </main>
      </div>
      <Show when={modal()}>
        <div
          class="modal-backdrop"
          onClick={(e) => {
            if (e.target === e.currentTarget) void closeModal();
          }}
        >
          <div
            class="modal"
            classList={{
              large: modal() === "settings" || modal() === "compose",
            }}
            role="dialog"
            aria-modal="true"
            aria-labelledby="modal-title"
            ref={modalNode}
          >
            <header>
              <h2 id="modal-title">
                {
                  {
                    compose: "A new conversation",
                    settings: "Make yourself at home",
                    pending: "Pending server actions",
                    event: "Event details",
                    contact: "Contact details",
                    person: "A person, not an inbox",
                    "message-actions": "Message details",
                  }[modal()]
                }
              </h2>
              <button
                aria-label="Close dialog"
                onClick={() => void closeModal()}
              >
                ×
              </button>
            </header>
            <Show when={error()}>
              <div class="alert" role="alert">
                {error()}
              </div>
            </Show>
            <Show when={modal()==="pending"}>
              <PendingActions accounts={scopedAccounts()} run={run}/>
            </Show>
            <Show when={modal() === "compose"}>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void run(async () => {
                    if (!draft().to.length)
                      throw Error("Add at least one recipient.");
                    await saveDraft();
                    if (dirty() || draftId() === null)
                      throw Error("Save the draft before queueing.");
                    await api("queue_draft", {
                      account: account(),
                      id: draftId(),
                    });
                    setNotice(
                      demo
                        ? "Demo draft queued locally. No mail was sent."
                        : "Message queued for delivery.",
                    );
                    setModal("");
                    await load();
                  });
                }}
              >
                <div class="form-body">
                  <label>
                    From
                    <select
                      value={draft().from}
                      onChange={(e) =>
                        changeDraft("from", e.currentTarget.value)
                      }
                    >
                      <For each={accounts()}>
                        {(a) => <option value={a.email}>{a.email}</option>}
                      </For>
                    </select>
                  </label>
                  {field(
                    "To",
                    draft().to.join(", "),
                    (v) => changeDraft("to", split(v)),
                    "text",
                    true,
                  )}
                  <div class="form-row">
                    {field("Cc", draft().cc.join(", "), (v) =>
                      changeDraft("cc", split(v)),
                    )}
                    {field("Bcc", draft().bcc.join(", "), (v) =>
                      changeDraft("bcc", split(v)),
                    )}
                  </div>
                  {field("Subject", draft().subject, (v) =>
                    changeDraft("subject", v),
                  )}
                  <label>
                    Message
                    <textarea
                      class="compose-text"
                      value={draft().text}
                      onInput={(e) =>
                        changeDraft("text", e.currentTarget.value)
                      }
                      placeholder="Say something good…"
                    />
                  </label>
                  <details>
                    <summary>Optional HTML body</summary>
                    <label>
                      HTML
                      <textarea
                        value={draft().html || ""}
                        onInput={(e) =>
                          changeDraft("html", e.currentTarget.value || null)
                        }
                      />
                    </label>
                  </details>
                  <label class="file-label">
                    ＋ Attach files
                    <input
                      type="file"
                      multiple
                      onChange={(e) => {
                        const files = Array.from(e.currentTarget.files || []);
                        void run(async () => {
                          const added = await Promise.all(
                            files.map(async (f) => ({
                              filename: f.name,
                              content_type:
                                f.type || "application/octet-stream",
                              bytes: Array.from(
                                new Uint8Array(await f.arrayBuffer()),
                              ),
                            })),
                          );
                          changeDraft("attachments", [
                            ...draft().attachments,
                            ...added,
                          ]);
                        });
                      }}
                    />
                  </label>
                  <For each={draft().attachments}>
                    {(a, i) => (
                      <div class="attachment">
                        {a.filename}{" "}
                        <small>{Math.ceil(a.bytes.length / 1024)} KB</small>
                        <button
                          type="button"
                          aria-label={`Remove ${a.filename}`}
                          onClick={() =>
                            changeDraft(
                              "attachments",
                              draft().attachments.filter((_, j) => j !== i()),
                            )
                          }
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </For>
                  <Show
                    when={
                      settings().ai_enabled &&
                      settings().consent_to_send_contents
                    }
                  >
                    <button
                      type="button"
                      onClick={() =>
                        run(async () => {
                          const r = await api("ai", {
                            capability: "draft_reply",
                            text: draft().text,
                            subject: draft().subject,
                          });
                          if (r.text) changeDraft("text", r.text);
                        })
                      }
                    >
                      Help write with AI
                    </button>
                  </Show>
                </div>
                <footer>
                  <span aria-live="polite">
                    {saveState() || "Draft saves automatically"}
                  </span>
                  <button type="button" onClick={saveDraft} disabled={!!busy()}>
                    Save draft
                  </button>
                  <button class="primary" disabled={!!busy()}>
                    Queue message ↗
                  </button>
                </footer>
              </form>
            </Show>
            <Show when={modal() === "settings"}>
              <div class="settings-body">
                <section>
                  <h3>Look & feel</h3>
                  <div class="form-row">
                    <ThemePicker
                      value={resolveTheme(settings().theme, omarchy())}
                      omarchy={omarchy()}
                      onChange={(id) => {
                        rememberTheme(id);
                        setting("theme", id);
                      }}
                    />
                    <label>
                      Density
                      <select
                        value={settings().density || "comfortable"}
                        onChange={(e) =>
                          setting("density", e.currentTarget.value)
                        }
                      >
                        <option value="comfortable">Comfortable</option>
                        <option value="compact">Compact</option>
                      </select>
                    </label>
                    <label>
                      Layout
                      <select
                        value={settings().layout || "three-pane"}
                        onChange={(e) =>
                          setting("layout", e.currentTarget.value)
                        }
                      >
                        <option value="three-pane">Balanced panes</option>
                        <option value="reader">Wider reader</option>
                      </select>
                    </label>
                  </div>
                  {toggle("Preview attachments", "attachment_preview")}
                  <p>
                    Privacy: remote images stay blocked until you explicitly
                    allow them for a message. HTML runs without scripts or
                    navigation permissions.
                  </p>
                  {toggle("Desktop notifications", "notifications")}
                </section>
                <section>
                  <h3>Accounts</h3>
                  <Show when={!accounts().length}>
                    <p>
                      Welcome to MunkiMail. Connect your first account to get
                      started. Your inbox begins empty; only your mail belongs
                      here.
                    </p>
                  </Show>
                  <For each={accounts()}>
                    {(a) => (
                      <div class="account-item">
                        <strong>{a.display}</strong>
                        <span>{a.email}</span>
                      </div>
                    )}
                  </For>
                  <AccountForm
                    accounts={accounts()}
                    onAdded={bootstrap}
                    run={run}
                  />
                  <button onClick={()=>setModal("pending")}>View all pending actions</button>
                  <OperationReview account={account()} run={run} />
                </section>
                <section>
                  <h3>Calendar & contacts</h3>
                  <div class="form-row">
                    {field(
                      "CalDAV URL",
                      settings().caldav_url || "",
                      (v) => setting("caldav_url", v),
                      "url",
                    )}
                    {field(
                      "CardDAV URL",
                      settings().carddav_url || "",
                      (v) => setting("carddav_url", v),
                      "url",
                    )}
                  </div>
                  {field("DAV username", settings().dav_username || "", (v) =>
                    setting("dav_username", v),
                  )}
                  {field(
                    "DAV password · keychain only",
                    davPassword(),
                    setDavPassword,
                    "password",
                  )}
                  <div class="actions">
                    <button
                      onClick={() =>
                        run(async () => {
                          setNotice(JSON.stringify(await api("dav_sync")));
                          await refreshPim();
                        })
                      }
                    >
                      Sync calendars & contacts
                    </button>
                    <For each={["calendar", "contacts"]}>
                      {(kind) => (
                        <>
                          <button
                            onClick={() =>
                              run(async () => {
                                const r = await api("pim_export", { kind });
                                download(
                                  kind === "calendar"
                                    ? "calendar.ics"
                                    : "contacts.vcf",
                                  r.text,
                                );
                              })
                            }
                          >
                            Export {kind}
                          </button>
                          <label class="import">
                            Import {kind}
                            <input
                              type="file"
                              accept={kind === "calendar" ? ".ics" : ".vcf"}
                              onChange={(e) => {
                                const file = e.currentTarget.files?.[0];
                                if (file)
                                  void run(async () => {
                                    await api("pim_import", {
                                      kind,
                                      text: await file.text(),
                                    });
                                    await refreshPim();
                                    setNotice("Import complete");
                                  });
                              }}
                            />
                          </label>
                        </>
                      )}
                    </For>
                  </div>
                </section>
                <section>
                  <h3>AI, only when invited</h3>
                  <p>
                    Selected message contents may be sent to your provider when
                    you request assistance.
                  </p>
                  {toggle("Enable AI assistance", "ai_enabled")}
                  {toggle(
                    "I consent to sending selected contents to my AI provider",
                    "consent_to_send_contents",
                  )}
                  <label>
                    Provider
                    <select
                      value={settings().ai_provider || "anthropic"}
                      onChange={(e) =>
                        setting("ai_provider", e.currentTarget.value)
                      }
                    >
                      <option value="anthropic">Anthropic</option>
                      <option value="openai_compatible">
                        OpenAI-compatible
                      </option>
                    </select>
                  </label>
                  <div class="form-row">
                    {field(
                      "Endpoint",
                      settings().ai_endpoint || "",
                      (v) => setting("ai_endpoint", v),
                      "url",
                    )}
                    {field("Model", settings().ai_model || "", (v) =>
                      setting("ai_model", v),
                    )}
                  </div>
                  {field(
                    "API key · keychain only",
                    aiKey(),
                    setAiKey,
                    "password",
                  )}
                  <button
                    onClick={() =>
                      run(async () => {
                        const r = await api("ai_activity");
                        setNotice(
                          `${r.activities.length} AI activities recorded`,
                        );
                      })
                    }
                  >
                    View AI activity
                  </button>
                </section>
              </div>
              <footer>
                <span>Secrets are stored by the desktop keychain.</span>
                <button
                  class="primary"
                  disabled={!!busy()}
                  onClick={() =>
                    run(async () => {
                      const s = settings();
                      const safe: Record<string, any> = {
                        ...s,
                        ai: {
                          ...s.ai,
                          enabled: s.ai_enabled,
                          consent_to_send_contents: s.consent_to_send_contents,
                          provider: s.ai_provider,
                          endpoint: s.ai_endpoint,
                          model: s.ai_model,
                        },
                        dav: {
                          ...s.dav,
                          calendar_url: s.caldav_url,
                          contacts_url: s.carddav_url,
                          username: s.dav_username,
                        },
                      };
                      for (const key of [
                        "ai_key",
                        "dav_password",
                        "ai_enabled",
                        "consent_to_send_contents",
                        "ai_provider",
                        "ai_endpoint",
                        "ai_model",
                        "caldav_url",
                        "carddav_url",
                        "dav_username",
                      ])
                        delete safe[key];
                      await api("settings_save", {
                        settings: safe,
                        ...(aiKey() ? { ai_key: aiKey() } : {}),
                        ...(davPassword()
                          ? { dav_password: davPassword() }
                          : {}),
                      });
                      setAiKey("");
                      setDavPassword("");
                      setNotice("Settings saved");
                      setModal("");
                    })
                  }
                >
                  Save settings
                </button>
              </footer>
            </Show>
            <Show when={modal() === "event" && editEvent()}>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void run(async () => {
                    const event = editEvent()!;
                    if (event.recurring)
                      throw Error(
                        "Recurring events are read-only. Edit the full resource in your calendar provider.",
                      );
                    if (
                      event.all_day &&
                      (!event.start_date ||
                        !event.end_date ||
                        event.end_date <= event.start_date)
                    )
                      throw Error(
                        "All-day end date must follow start date (exclusive).",
                      );
                    if (event.end <= event.start)
                      throw Error("End must be after start.");
                    await api("save_event", { event });
                    await refreshPim();
                    setModal("");
                  });
                }}
              >
                <Show when={editEvent()!.recurring}>
                  <div class="notice">
                    Recurring event · read-only. Edit the full resource in your
                    calendar provider.
                  </div>
                </Show>
                <fieldset disabled={!!editEvent()!.recurring} class="form-body">
                  {field(
                    "Title",
                    editEvent()!.title,
                    (v) => setEditEvent((x) => ({ ...x!, title: v })),
                    "text",
                    true,
                  )}
                  <div class="form-row">
                    <For each={["start", "end"] as const}>
                      {(key) => {
                        const val = () => {
                          const event = editEvent()!;
                          if (event.all_day)
                            return (
                              event[
                                key === "start" ? "start_date" : "end_date"
                              ] ||
                              new Date(event[key] * 1000)
                                .toISOString()
                                .slice(0, 10)
                            );
                          const d = new Date(event[key] * 1000);
                          return new Date(+d - d.getTimezoneOffset() * 60000)
                            .toISOString()
                            .slice(0, 16);
                        };
                        return (
                          <label>
                            {key === "start"
                              ? "Starts"
                              : editEvent()!.all_day
                                ? "Ends (exclusive)"
                                : "Ends"}
                            <input
                              required
                              type={
                                editEvent()!.all_day ? "date" : "datetime-local"
                              }
                              value={val()}
                              onInput={(e) => {
                                const v = e.currentTarget.value;
                                setEditEvent((x) => ({
                                  ...x!,
                                  [key]: +new Date(v) / 1000,
                                  ...(x!.all_day
                                    ? {
                                        [key === "start"
                                          ? "start_date"
                                          : "end_date"]: v,
                                      }
                                    : {}),
                                }));
                              }}
                            />
                          </label>
                        );
                      }}
                    </For>
                  </div>
                  <label class="toggle">
                    <input
                      type="checkbox"
                      checked={editEvent()!.all_day}
                      onChange={(e) =>
                        setEditEvent((x) => ({
                          ...x!,
                          all_day: e.currentTarget.checked,
                          ...(e.currentTarget.checked
                            ? (() => {
                                const startDate = new Date(x!.start * 1000);
                                const start_date =
                                  x!.start_date ||
                                  new Date(
                                    +startDate -
                                      startDate.getTimezoneOffset() * 60000,
                                  )
                                    .toISOString()
                                    .slice(0, 10);
                                const end_date =
                                  x!.end_date && x!.end_date > start_date
                                    ? x!.end_date
                                    : new Date(
                                        +new Date(start_date + "T00:00:00Z") +
                                          86400000,
                                      )
                                        .toISOString()
                                        .slice(0, 10);
                                return {
                                  start_date,
                                  end_date,
                                  start:
                                    +new Date(start_date + "T00:00:00Z") / 1000,
                                  end:
                                    +new Date(end_date + "T00:00:00Z") / 1000,
                                };
                              })()
                            : {}),
                        }))
                      }
                    />
                    All day
                  </label>
                  {field("Location", editEvent()!.location, (v) =>
                    setEditEvent((x) => ({ ...x!, location: v })),
                  )}
                  {field(
                    "Attendees · comma separated",
                    editEvent()!.attendees.join(", "),
                    (v) =>
                      setEditEvent((x) => ({ ...x!, attendees: split(v) })),
                  )}
                  <label>
                    Description
                    <textarea
                      value={editEvent()!.description}
                      onInput={(e) =>
                        setEditEvent((x) => ({
                          ...x!,
                          description: e.currentTarget.value,
                        }))
                      }
                    />
                  </label>
                </fieldset>
                <footer>
                  <Show when={editEvent()!.uid && !editEvent()!.recurring}>
                    <button
                      type="button"
                      class="danger"
                      onClick={() =>
                        run(async () => {
                          await api("delete_event", {
                            uid: editEvent()!.uid,
                            id: editEvent()!.id,
                            revision: editEvent()!.revision,
                          });
                          await refreshPim();
                          setModal("");
                        })
                      }
                    >
                      Delete event
                    </button>
                  </Show>
                  <button
                    class="primary"
                    disabled={!!busy() || !!editEvent()!.recurring}
                  >
                    Save event
                  </button>
                </footer>
              </form>
            </Show>
            <Show when={modal() === "contact" && editContact()}>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void run(async () => {
                    await api("save_contact", { contact: editContact() });
                    await refreshPim();
                    setModal("");
                  });
                }}
              >
                <div class="form-body">
                  {field(
                    "Name",
                    editContact()!.name,
                    (v) => setEditContact((x) => ({ ...x!, name: v })),
                    "text",
                    true,
                  )}
                  {field(
                    "Email",
                    editContact()!.email,
                    (v) =>
                      setEditContact((x) => ({
                        ...x!,
                        email: v,
                        emails: [v, ...(x!.emails || []).slice(1)],
                      })),
                    "email",
                    true,
                  )}
                  {field(
                    "Phone",
                    editContact()!.phone,
                    (v) =>
                      setEditContact((x) => ({
                        ...x!,
                        phone: v,
                        phones: [v, ...(x!.phones || []).slice(1)],
                      })),
                    "tel",
                  )}
                  {field(
                    "Organization",
                    editContact()!.organization || "",
                    (v) => setEditContact((x) => ({ ...x!, organization: v })),
                  )}
                  <label>
                    Notes
                    <textarea
                      value={editContact()!.note || ""}
                      onInput={(e) =>
                        setEditContact((x) => ({
                          ...x!,
                          note: e.currentTarget.value,
                        }))
                      }
                    />
                  </label>
                </div>
                <footer>
                  <Show when={editContact()!.uid}>
                    <button
                      type="button"
                      class="danger"
                      onClick={() =>
                        run(async () => {
                          await api("delete_contact", {
                            uid: editContact()!.uid,
                            id: editContact()!.id,
                            revision: editContact()!.revision,
                          });
                          await refreshPim();
                          setModal("");
                        })
                      }
                    >
                      Delete contact
                    </button>
                  </Show>
                  <button class="primary" disabled={!!busy()}>
                    Save contact
                  </button>
                </footer>
              </form>
            </Show>
            <Show when={modal() === "person" && person()}>
              <div class="form-body">
                <h1>{person().contact?.name || "Conversations"}</h1>
                <p>{person().contact?.email}</p>
                <h3>Mail</h3>
                <For each={person().messages}>
                  {(m: Message) => (
                    <button
                      class="person-message"
                      onClick={() => {
                        setAccount(String(m.account));
                        setSection("mail");
                        setModal("");
                        void openMessage(m);
                      }}
                    >
                      {m.renamed_subject || m.subject}
                      <small>{stamp(m.date)}</small>
                    </button>
                  )}
                </For>
                <Show when={!person().messages.length}>
                  <p>No messages yet.</p>
                </Show>
                <h3>Events</h3>
                <For each={person().events}>
                  {(e: Event) => (
                    <p>
                      {e.title} · {stamp(e.start)}
                    </p>
                  )}
                </For>
              </div>
            </Show>
            <Show when={modal() === "message-actions"}>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  const data = new FormData(e.currentTarget);
                  void run(async () => {
                    await api("rename_subject", {
                      account: messageAccount(),
                      id: selected(),
                      subject: String(data.get("subject")) || null,
                    });
                    await load();
                    await openMessage(detail().message);
                    setModal("");
                  });
                }}
              >
                <div class="form-body">
                  <label>
                    Display subject
                    <input
                      name="subject"
                      value={
                        detail()?.message.renamed_subject ||
                        detail()?.message.subject
                      }
                    />
                  </label>
                  <p>Clear to restore the original subject.</p>
                </div>
                <footer>
                  <button class="primary">Save subject</button>
                </footer>
              </form>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  const target = Number(
                    new FormData(e.currentTarget).get("target"),
                  );
                  void run(async () => {
                    if (!target || target === selected())
                      throw Error("Choose a different message ID.");
                    await api("merge_threads", {
                      account: messageAccount(),
                      source: selected(),
                      target,
                    });
                    setNotice("Threads merged");
                    setModal("");
                  });
                }}
              >
                <div class="form-body">
                  <label>
                    Merge into message ID
                    <input name="target" type="number" min="1" required />
                  </label>
                  <button>Merge threads</button>
                </div>
              </form>
            </Show>
          </div>
        </div>
      </Show>
    </Show>
  );
}
function OperationReview(props: {
  account: string;
  run: <T>(fn: () => Promise<T>) => Promise<T | undefined>;
}) {
  const [ops, setOps] = createSignal<any[]>([]),
    [loaded, setLoaded] = createSignal(false);
  async function load() {
    const r = await api("review", { account: props.account });
    setOps(r.ops);
    setLoaded(true);
  }
  return (
    <section>
      <h3>Delivery & sync recovery</h3>
      <p>
        Uncertain operations need your decision. Retrying a send may deliver a
        duplicate. Mark sent only after checking with your provider. Discard
        stops further attempts.
      </p>
      <button disabled={!props.account} onClick={() => props.run(load)}>
        Review pending operations
      </button>
      <Show when={loaded() && !ops().length}>
        <p>No operations need review.</p>
      </Show>
      <For each={ops()}>
        {(op) => (
          <div class="review-op">
            <strong>
              {op.kind?.kind === "save_sent" ? "Save Sent copy" : op.kind?.kind === "submit" ? "Send message" : "Pending operation"} #{op.id}
            </strong>
            <Show when={op.kind?.kind === "save_sent"}><p>Email already sent. Only saving its Sent copy failed. Retrying will not resend it.</p></Show>
            <p>{op.last_error || op.error || op.state || "Operation needs review"}</p>
            <div class="actions">
              <For each={(op.kind?.kind === "submit" ? ["retry", "discard", "sent"] : ["retry", "discard"]) as ("retry" | "discard" | "sent")[]}>
                {(decision) => (
                  <button
                    onClick={() =>
                      props.run(async () => {
                        await api("review_resolve", {
                          account: props.account,
                          id: op.id,
                          decision,
                        });
                        await load();
                      })
                    }
                  >
                    {decision === "sent"
                      ? "Mark sent"
                      : decision === "retry"
                        ? (op.kind?.kind === "save_sent" ? "Retry Sent copy" : "Retry")
                        : (op.kind?.kind === "save_sent" ? "Stop saving copy" : "Discard")}
                  </button>
                )}
              </For>
            </div>
          </div>
        )}
      </For>
    </section>
  );
}
function AccountForm(props: {
  accounts: Account[];
  onAdded: () => Promise<void>;
  run: <T>(fn: () => Promise<T>) => Promise<T | undefined>;
}) {
  let accountForm!:HTMLFormElement;
  function fillConnection(config:Record<string,unknown>){
    config={...config,smtp_implicit:Number(config.smtp_port)===587?false:Number(config.smtp_port)===465?true:config.smtp_implicit};
    setTransport(String(config.transport||"imap"));setAuth(String(config.auth_kind||"password"));
    queueMicrotask(()=>{for(const [key,value]of Object.entries({...config,provider:config.oauth_provider,client_id:config.oauth_client_id})){
      const field=accountForm.elements.namedItem(key);if(field instanceof HTMLInputElement){if(field.type==="checkbox")field.checked=!!value;else if(value!=null)field.value=String(value);}else if(field instanceof HTMLSelectElement && value!=null)field.value=String(value);
    }});
  }
  const [editing, setEditing] = createSignal(props.accounts[0]?.id || "");
  const current = () => props.accounts.find((a) => a.id === editing());
  const [transport, setTransport] = createSignal("imap");
  const [auth, setAuth] = createSignal("password"),
    [pending, setPending] = createSignal(false),
    [oauthAccount, setOauthAccount] = createSignal("");
  createEffect(on(editing,()=>{const config=current()?.config;if(config)fillConnection(config);else{accountForm?.reset();setAuth("password");setTransport("imap");}}));
  return (
    <details open>
      <summary>Edit connection settings or add an account</summary>
      <label>
        Account to configure
        <select
          value={editing()}
          onChange={(e) => setEditing(e.currentTarget.value)}
        >
          <option value="">New account</option>
          <For each={props.accounts}>
            {(a) => <option value={a.id}>{a.email}</option>}
          </For>
        </select>
      </label>
      <Show when={editing()}>
        <p>
          Saved connection settings are shown below. Edit them, then choose Update account.
          Leave password empty to retain the saved password.
        </p>
      </Show>
      <form ref={accountForm}
        onSubmit={(e) => {
          e.preventDefault();
          const data = Object.fromEntries(new FormData(e.currentTarget));
          void props.run(async () => {
            if (demo)
              throw Error(
                "Account connection requires the desktop app. No credentials were saved.",
              );
            const result = await api(
              editing() ? "update_account" : "add_account",
              {
                ...(editing() ? { account: editing() } : {}),
                transport: transport(),
                ...data,
                ...(data.password ? {} : { password: undefined }),
                imap_port: Number(data.imap_port),
                smtp_port: Number(data.smtp_port),
                smtp_implicit: data.smtp_implicit === "true",
                auth_kind: auth(),
                oauth_provider: data.provider ?? current()?.config?.oauth_provider ?? "",
                oauth_client_id: data.client_id ?? current()?.config?.oauth_client_id ?? "",
              },
            );
            (e.target as HTMLFormElement).querySelector<HTMLInputElement>(
              '[name="password"]',
            )!.value = "";
            if (auth() === "oauth2") {
              const id =
                editing() ||
                result?.id ||
                result?.account?.id ||
                result?.account ||
                data.email;
              setOauthAccount(String(id));
              await api("auth_start", {
                account: id,
                provider: data.provider,
                client_id: data.client_id,
              });
              setPending(true);
            } else await props.onAdded();
          });
        }}
      >
        <div class="form-row">
          <label>
            Email
            <input
              name="email"
              type="email"
              value={current()?.email || ""}
              required
              autocomplete="username"
            />
          </label>
          <label>
            Display name
            <input name="display" value={current()?.display || ""} required />
          </label>
        </div>
        <Show when={!editing()}><label>Connection preset
          <select aria-label="Connection preset" onChange={e=>{const provider=e.currentTarget.value as keyof typeof providerPresets;const preset=providerPresets[provider];if(preset)fillConnection({...preset,transport:"imap",auth_kind:"oauth2",oauth_provider:provider,imap_port:993,smtp_implicit:preset.smtp_port===465,oauth_client_id:""});}}>
            <option value="">Custom IMAP / JMAP</option>
            <For each={Object.entries(providerPresets)}>{([id,preset])=><option value={id}>{preset.label}</option>}</For>
          </select>
        </label></Show>
        <label>
          Mail transport
          <select
            value={transport()}
            onChange={(e) => setTransport(e.currentTarget.value)}
          >
            <option value="imap">IMAP + SMTP</option>
            <option value="jmap">JMAP + SMTP</option>
          </select>
        </label>
        <Show when={transport() === "jmap"}>
          <label>
            JMAP session URL
            <input
              name="jmap_url"
              type="url"
              required
              placeholder="https://mail.example.com/.well-known/jmap"
            />
          </label>
        </Show>
        <div class="form-row">
          <label>
            IMAP host
            <input name="imap_host" required={transport() === "imap"} />
          </label>
          <label>
            Port
            <input
              name="imap_port"
              type="number"
              min="1"
              max="65535"
              value="993"
              required
            />
          </label>
        </div>
        <div class="form-row">
          <label>
            SMTP host
            <input name="smtp_host" required />
          </label>
          <label>
            Port
            <input
              name="smtp_port"
              onInput={e=>{const security=accountForm.elements.namedItem("smtp_implicit") as HTMLSelectElement;if(e.currentTarget.value==="587")security.value="false";else if(e.currentTarget.value==="465")security.value="true";}}
              type="number"
              min="1"
              max="65535"
              value="465"
              required
            />
          </label>
        </div>
        <label>
          SMTP security
          <select name="smtp_implicit" value="true">
            <option value="true">TLS immediately (normally port 465)</option>
            <option value="false">STARTTLS (normally port 587)</option>
          </select>
        </label>
        <label>
          Sent folder
          <input name="sent_folder" value="Sent" required />
        </label>
        <label>
          Authentication
          <select
            value={auth()}
            onChange={(e) => setAuth(e.currentTarget.value)}
          >
            <option value="password">Password / app password</option>
            <option value="oauth2">OAuth 2.0</option>
          </select>
        </label>
        <label classList={{ hidden: auth() !== "password" }}>
          Password
          <input
            name="password"
            type="password"
            autocomplete="new-password"
            required={auth() === "password" && !editing()}
          />
        </label>
        <Show when={auth() === "oauth2"}>
          <div class="form-row">
            <label>
              Provider
              <select name="provider">
                <option value="google">Google</option>
                <Show when={current()?.config?.oauth_provider==="gmail"}><option value="gmail">Google (existing registration)</option></Show>
                <Show when={current()?.config?.oauth_provider==="outlook"}><option value="outlook">Microsoft (existing registration)</option></Show>
                <option value="microsoft">Microsoft</option>
                <option value="yahoo">Yahoo</option>
              </select>
            </label>
            <label>
              OAuth client ID (registered public/native app)
              <input name="client_id" required />
            </label>
          </div>
        </Show>
        <Show when={auth()==="oauth2"}><p class="quiet">Use your registered native/public OAuth client ID. Yahoo mail access requires provider approval. MunkiMail views stay local; read/unread syncs across devices.</p></Show>
        <button type="submit">
          {auth() === "oauth2"
            ? "Connect & sign in"
            : editing()
              ? "Update account"
              : "Connect account"}
        </button>
        <Show when={pending()}>
          <p>
            Finish sign-in in your system browser, then complete authentication.
          </p>
          <button
            type="button"
            onClick={() =>
              props.run(async () => {
                await api("auth_complete", { account: oauthAccount() });
                setPending(false);
                await props.onAdded();
              })
            }
          >
            Complete sign-in
          </button>
        </Show>
      </form>
    </details>
  );
}
render(() => <App />, document.getElementById("root")!);

function FolderBranch(props:{nodes:FolderNode[];active:string;onOpen:(folder:ServerFolder)=>void}){
  return <ul class="folder-tree"><For each={props.nodes}>{node=><li>
    <Show when={node.children.length} fallback={<button disabled={!node.folder?.selectable} classList={{active:props.active===`folder:${node.name}`}} onClick={()=>node.folder&&props.onOpen(node.folder)}><span aria-hidden="true">▱</span> {node.label}</button>}>
      <details><summary><span>{node.label}</span></summary>
        <Show when={node.folder?.selectable}><button classList={{active:props.active===`folder:${node.name}`}} onClick={()=>node.folder&&props.onOpen(node.folder)}>Open {node.label}</button></Show>
        <FolderBranch nodes={node.children} active={props.active} onOpen={props.onOpen}/>
      </details>
    </Show>
  </li>}</For></ul>;
}

function PendingActions(props:{accounts:Account[];run:<T>(fn:()=>Promise<T>)=>Promise<T|undefined>}){
  const [groups,setGroups]=createSignal<{account:Account;ops:any[];total:number}[]>([]),[loaded,setLoaded]=createSignal(false);
  async function load(){const results=await Promise.all(props.accounts.map(async account=>({account,...await api("pending",{account:account.id})})));setGroups(results);setLoaded(true);}
  onMount(()=>void props.run(load));
  return <div class="form-body">
    <p>Actions still owed to your mail server. Loading this list does not retry or send anything.</p>
    <button onClick={()=>void props.run(load)}>Refresh pending list</button>
    <Show when={loaded()&&!groups().some(g=>g.total)}><p>No pending server actions.</p></Show>
    <For each={groups().filter(g=>g.total)}>{group=><section>
      <h3>{group.account.email} · {group.total} pending</h3>
      <Show when={group.total>group.ops.length}><p>Showing the first {group.ops.length} actions.</p></Show>
      <For each={group.ops}>{op=><article class="review-op">
        <strong>{op.label} #{op.id}</strong>
        <p>{op.status} · {op.attempts} failed attempts</p>
        <Show when={op.folder}><p>Folder: {op.folder}</p></Show>
        <Show when={op.retry_at}><p>Next retry after {new Date(op.retry_at*1000).toLocaleString()}, during sync.</p></Show>
        <Show when={op.last_error}><p>Last error: {op.last_error}</p></Show>
        <Show when={op.status==="Needs review"}><p>Open Settings → Delivery & sync recovery to resolve this action.</p></Show>
      </article>}</For>
    </section>}</For>
  </div>;
}
