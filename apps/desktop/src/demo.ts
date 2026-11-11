import {
  blankDraft,
  type Message,
  type Event,
  type Contact,
  type Draft,
} from "./api";
const account = {
  id: "demo",
  email: "alex@example.test",
  display: "Alex Morgan",
  kind: "imap",
};
const now = 1793707200;
const names = [
  "Maya Chen <maya@example.test>",
  "The Sunday Journal <journal@example.test>",
  "Jordan Lee <jordan@example.test>",
  "Studio North <hello@example.test>",
  "Linear <updates@example.test>",
  "Sam Rivera <sam@example.test>",
];
const subjects = [
  "A little room for a big idea",
  "Your weekly dose of good things",
  "Coffee next Thursday?",
  "The new identity is ready for a look",
  "Your workspace, this week",
  "A trail worth taking",
];
let messages: Message[] = Array.from({ length: 240 }, (_, i) => ({
  id: i + 1,
  account: "demo",
  folder: "INBOX",
  uid: i + 1,
  message_id: `demo-${i}`,
  thread_id: `thread-${i}`,
  from: names[i % 6],
  subject: subjects[i % 6],
  date: now - i * 3200,
  seen: i > 3,
  has_attachment: i % 7 === 0,
  destination:
    i < 140 ? "imbox" : i < 174 ? "feed" : i < 208 ? "paper_trail" : "screener",
  snippet: [
    "I’ve been thinking about our conversation. Here’s a first pass at what we could make together.",
    "A few things to slow down with this week. Pull up a chair.",
    "Let’s find a little time to catch up.",
  ][i % 3],
}));
let settings: Record<string, any> = {
  theme: "auto",
  density: "comfortable",
  layout: "three-pane",
  attachment_preview: false,
  notifications: false,
  ai: {
    enabled: false,
    consent_to_send_contents: false,
    provider: "anthropic",
    model: "",
    endpoint: "",
  },
  dav: { calendar_url: "", contacts_url: "", username: "" },
};
let events: Event[] = [
  {
    uid: "demo-event",
    title: "Design catch-up",
    description: "Bring your ideas.",
    location: "The corner café",
    start: now + 3600,
    end: now + 7200,
    all_day: false,
    attendees: ["maya@example.test"],
  },
];
let contacts: Contact[] = [
  {
    uid: "maya",
    name: "Maya Chen",
    email: "maya@example.test",
    phone: "+1 555 010 0124",
  },
  {
    uid: "jordan",
    name: "Jordan Lee",
    email: "jordan@example.test",
    phone: "",
  },
];
const drafts = new Map<
  number,
  { draft: Draft; updated_at: number; state: string }
>();
const stacks = new Map<number, string>();
let inviteRevision=1;
// ponytail: demo state lives in this tab; use the real backend for persistence and delivery.
const escapePim = (s: string) =>
  s
    .replace(/\\/g, "\\\\")
    .replace(/\n/g, "\\n")
    .replace(/;/g, "\\;")
    .replace(/,/g, "\\,");
const icsDate = (n: number) =>
  new Date(n * 1000)
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}/, "");
function parseRecords(text: string, kind: string) {
  // ponytail: demo imports unzoned UTC/basic records; native backend preserves full DAV resources.
  const name = kind === "contacts" ? "VCARD" : "VEVENT";
  const records = [
    ...text
      .replace(/\r\n[ \t]/g, "")
      .matchAll(new RegExp(`BEGIN:${name}([\\s\\S]*?)END:${name}`, "g")),
  ];
  if (!records.length) throw Error("No supported records found.");
  return records.map((match) => {
    const fields: Record<string, string[]> = {};
    for (const line of match[1].split(/\r?\n/)) {
      const colon = line.indexOf(":");
      if (colon < 0) continue;
      const key = line.slice(0, colon).split(";")[0];
      (fields[key] ??= []).push(
        line
          .slice(colon + 1)
          .replace(/\\n/gi, "\n")
          .replace(/\\([,;\\])/g, "$1"),
      );
    }
    return fields;
  });
}
export async function dispatch(action: string, p: any): Promise<any> {
  switch (action) {
    case "bootstrap":
      return { accounts: [account], demo: true, settings };
    case "messages": {
      let rows = messages.filter(
        (m) =>
          m.account === p.account &&
          (p.query
            ? true
            : p.view === "reply_later" || p.view === "set_aside"
              ? stacks.get(m.id) === p.view
              : p.view === "sent"
                ? false
                : m.destination === p.view),
      );
      if (p.query)
        rows = rows.filter((m) =>
          `${m.from} ${m.subject} ${m.snippet}`
            .toLowerCase()
            .includes(p.query.toLowerCase()),
        );
      return {
        messages: rows.slice(p.offset || 0, (p.offset || 0) + (p.limit || 100)),
        total: rows.length,
      };
    }
    case "message": {
      const m = messages.find((m) => m.id === p.id);
      if (!m) throw Error("Message not found");
      m.seen = true;
      return {
        message: { ...m },
        ...(m.id===3?{invite:'Demo calendar invitation',invite_revision:inviteRevision}:{}),
        text: `Hi Alex,\n\n${m.snippet}\n\nThere’s something special about making space for the things that matter. Take a look when you have a quiet moment. I’d love to hear what you think.\n\nTalk soon,\n${m.from.split(" <")[0]}`,
        html: null,
        blocked: false,
        attachments: m.has_attachment
          ? [
              {
                index: 0,
                filename: "project-notes.txt",
                content_type: "text/plain",
                size: 27,
              },
            ]
          : [],
      };
    }
    case "rsvp":
      if(p.id!==3)throw Error('No invitation found.');
      if(p.revision!==inviteRevision)throw Error('Invitation changed. Reload before responding.');
      if(!['ACCEPTED','DECLINED','TENTATIVE'].includes(p.response))throw Error('Invalid invitation response.');
      inviteRevision++;return {queued:true};
    case "triage":
      for (const s of p.senders)
        messages = messages.map((m) =>
          m.from.includes(s.address) ? { ...m, destination: s.destination } : m,
        );
      return {};
    case "set_stack":
      p.enabled ? stacks.set(p.id, p.stack) : stacks.delete(p.id);
      return {};
    case "rename_subject": {
      const m = messages.find((m) => m.id === p.id);
      if (m) m.renamed_subject = p.subject;
      return {};
    }
    case "merge_threads": {
      const target = messages.find((m) => m.id === Number(p.target));
      messages
        .filter((m) => m.id === p.source)
        .forEach((m) => (m.thread_id = target?.thread_id || m.thread_id));
      return {};
    }
    case "reply": {
      const m = messages.find((m) => m.id === p.id)!;
      return {
        ...blankDraft(account.email),
        to: p.forward ? [] : [m.from],
        subject: `${p.forward ? "Fwd" : "Re"}: ${m.subject}`,
        in_reply_to: m.message_id,
      };
    }
    case "drafts":
      return {
        drafts: [...drafts].map(([id, d]) => ({
          id,
          subject: d.draft.subject,
          to: d.draft.to,
          updated_at: d.updated_at,
          state: d.state,
        })),
      };
    case "draft":
      return drafts.get(p.id)?.draft;
    case "save_draft": {
      const id = p.id ?? Date.now();
      drafts.set(id, {
        draft: structuredClone(p.draft),
        updated_at: Date.now() / 1000,
        state: "draft",
      });
      return { id };
    }
    case "queue_draft": {
      const d = drafts.get(p.id);
      if (d) d.state = "queued";
      return { queued: true };
    }
    case "settings_get":
      return settings;
    case "settings_save":
      settings = { ...p.settings };
      return {};
    case "sync":
      return { fetched: 0, applied: 0, pending: 0 };
    case "events":
      return {
        events: events.filter((e) => e.start < p.end && e.end > p.start),
      };
    case "save_event": {
      if(p.event.all_day){if(!/^\d{4}-\d{2}-\d{2}$/.test(p.event.start_date||'')||!/^\d{4}-\d{2}-\d{2}$/.test(p.event.end_date||'')||p.event.end_date<=p.event.start_date)throw Error('Valid exclusive all-day dates required.');p.event={...p.event,start:+new Date(p.event.start_date+'T00:00:00Z')/1000,end:+new Date(p.event.end_date+'T00:00:00Z')/1000};}
      const old = events.find((x) => x.uid === p.event.uid);
      if (old?.recurring) throw Error("Recurring events are read-only");
      if (old && old.revision !== p.event.revision)
        throw Error("Event changed. Reload before saving.");
      const e = {
        ...p.event,
        uid: p.event.uid || crypto.randomUUID(),
        revision: (old?.revision || 0) + 1,
      };
      events = [...events.filter((x) => x.uid !== e.uid), e];
      return e;
    }
    case "delete_event":
      events = events.filter((e) => e.uid !== p.uid);
      return {};
    case "contacts":
      return { contacts };
    case "save_contact": {
      const old = contacts.find((x) => x.uid === p.contact.uid);
      if (old && old.revision !== p.contact.revision)
        throw Error("Contact changed. Reload before saving.");
      const c = {
        ...p.contact,
        uid: p.contact.uid || crypto.randomUUID(),
        revision: (old?.revision || 0) + 1,
      };
      contacts = [...contacts.filter((x) => x.uid !== c.uid), c];
      return c;
    }
    case "delete_contact":
      contacts = contacts.filter((c) => c.uid !== p.uid);
      return {};
    case "person":
      return {
        contact: contacts.find((c) => c.email === p.email),
        messages: messages.filter((m) => m.from.includes(p.email)),
        events: events.filter((e) => e.attendees.includes(p.email)),
      };
    case "download_attachment":
      return {
        filename: "project-notes.txt",
        bytes: Array.from(
          new TextEncoder().encode("A little space for good ideas."),
        ),
      };
    case "dav_sync":
      return { status: "Demo: local calendar and contacts only" };
    case "pim_export": {
      if (p.kind === "contacts")
        return {
          text: contacts
            .map(
              (c) =>
                `BEGIN:VCARD\r\nVERSION:3.0\r\nUID:${escapePim(c.uid)}\r\nFN:${escapePim(c.name)}\r\nEMAIL:${escapePim(c.email)}\r\nTEL:${escapePim(c.phone)}\r\nORG:${escapePim(c.organization || "")}\r\nNOTE:${escapePim(c.note || "")}\r\nEND:VCARD`,
            )
            .join("\r\n"),
        };
      return {
        text: `BEGIN:VCALENDAR\r\nVERSION:2.0\r\n${events.map((e) => `BEGIN:VEVENT\r\nUID:${escapePim(e.uid)}\r\nSUMMARY:${escapePim(e.title)}\r\nDTSTART:${icsDate(e.start)}\r\nDTEND:${icsDate(e.end)}\r\nDESCRIPTION:${escapePim(e.description)}\r\nLOCATION:${escapePim(e.location)}\r\nEND:VEVENT`).join("\r\n")}\r\nEND:VCALENDAR`,
      };
    }
    case "pim_import": {
      const records = parseRecords(p.text, p.kind);
      if (p.kind === "contacts") {
        const added = records.map((f) => ({
          uid: f.UID?.[0] || crypto.randomUUID(),
          name: f.FN?.[0] || "",
          email: f.EMAIL?.[0] || "",
          phone: f.TEL?.[0] || "",
          emails: f.EMAIL || [],
          phones: f.TEL || [],
          organization: f.ORG?.[0] || "",
          note: f.NOTE?.[0] || "",
          revision: 1,
        }));
        contacts = [
          ...contacts.filter((c) => !added.some((a) => a.uid === c.uid)),
          ...added,
        ];
      } else {
        const parse = (value: string) => {
          if (!/^\d{8}T\d{6}Z$/.test(value || ""))
            throw Error(
              "Demo imports UTC event times only. Use desktop for timezone or recurring calendars.",
            );
          return (
            Date.UTC(
              +value.slice(0, 4),
              +value.slice(4, 6) - 1,
              +value.slice(6, 8),
              +value.slice(9, 11),
              +value.slice(11, 13),
              +value.slice(13, 15),
            ) / 1000
          );
        };
        const added = records.map((f) => {
          if (f.RRULE)
            throw Error("Recurring imports require the desktop backend.");
          const start = parse(f.DTSTART?.[0]),
            end = parse(f.DTEND?.[0]);
          if (end <= start) throw Error("Event end must follow start.");
          return {
            uid: f.UID?.[0] || crypto.randomUUID(),
            title: f.SUMMARY?.[0] || "Untitled",
            description: f.DESCRIPTION?.[0] || "",
            location: f.LOCATION?.[0] || "",
            start,
            end,
            all_day: false,
            attendees: [],
            revision: 1,
          };
        });
        events = [
          ...events.filter((e) => !added.some((a) => a.uid === e.uid)),
          ...added,
        ];
      }
      return { imported: records.length };
    }
    case "ai_activity":
      return { activities: [] };
    case "ai":
      throw Error(
        "AI is unavailable in browser demo mode. Configure a provider in the desktop app.",
      );
    case "auth_start":
    case "auth_complete":
    case "update_account":
    case "add_account":
      throw Error(
        "Connect accounts in the desktop app. Credentials are never stored in this demo.",
      );
    case "review_resolve":
      return {};
    case "review":
      return { ops: [] };
    default:
      throw Error(`Unsupported action: ${action}`);
  }
}
