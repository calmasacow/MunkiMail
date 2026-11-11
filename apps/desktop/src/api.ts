import { invoke } from "@tauri-apps/api/core";
export type Account = {
  id: string;
  email: string;
  display: string;
  kind: string;
  config?: Record<string, unknown>;
};
export type Message = {
  id: number;
  account: string;
  folder: string;
  uid: number;
  message_id: string;
  thread_id: string;
  from: string;
  subject: string;
  renamed_subject?: string | null;
  date: number;
  seen: boolean;
  has_attachment: boolean;
  destination: string;
  snippet?: string;
};
export type Draft = {
  from: string;
  to: string[];
  cc: string[];
  bcc: string[];
  subject: string;
  text: string;
  html: string | null;
  in_reply_to: string | null;
  attachments: { filename: string; content_type: string; bytes: number[] }[];
};
export type Event = {
  id?: string;
  revision?: number;
  recurring?: boolean;
  uid: string;
  title: string;
  description: string;
  location: string;
  start: number;
  end: number;
  all_day: boolean;
  start_date?: string;
  end_date?: string;
  attendees: string[];
};
export type Contact = {
  id?: string;
  revision?: number;
  organization?: string;
  note?: string;
  emails?: string[];
  phones?: string[];
  uid: string;
  name: string;
  email: string;
  phone: string;
};
export const demo = !(window as unknown as { __TAURI_INTERNALS__?: unknown })
  .__TAURI_INTERNALS__;
export async function api<T = any>(
  action: string,
  payload: Record<string, unknown> = {},
): Promise<T> {
  if (!demo) {
    if (typeof payload.account === "string" && /^\d+$/.test(payload.account))
      payload = { ...payload, account: Number(payload.account) };
    const result = await invoke<T | string>("dispatch", { action, payload });
    return typeof result === "string" ? JSON.parse(result) : result;
  }
  return (await import("./demo")).dispatch(action, payload) as Promise<T>;
}
export function blankDraft(from = ""): Draft {
  return {
    from,
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    text: "",
    html: null,
    in_reply_to: null,
    attachments: [],
  };
}
export function readerDocument(
  html: string,
  remote: boolean,
  appearance?: {
    text: string;
    background: string;
    accent: string;
    mode: string;
  },
) {
  const safeColor = (value: string | undefined, fallback: string) =>
    value && /^#[\da-f]{6}$/i.test(value) ? value : fallback;
  const text = safeColor(appearance?.text, "#e8e5df"),
    background = safeColor(appearance?.background, "#191917"),
    accent = safeColor(appearance?.accent, "#ee9b85");
  const mode = appearance?.mode === "light" ? "light" : "dark";
  return `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: cid:${remote ? " https:" : ""}; style-src 'unsafe-inline';"><style>body{font:16px/1.7 system-ui;color:${text};background:${background};color-scheme:${mode};padding:20px;overflow-wrap:anywhere}img{max-width:100%}a{color:${accent}}</style>${html}`;
}
