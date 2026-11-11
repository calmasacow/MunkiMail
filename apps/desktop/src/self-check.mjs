import assert from "node:assert/strict";
import { createServer } from "vite";
const server = await createServer({
  cacheDir: "node_modules/.vite-self-check",
  optimizeDeps: { noDiscovery: true, entries: [], include: [] },
  server: { middlewareMode: true },
});
globalThis.window = {};
try {
  const { dispatch } = await server.ssrLoadModule("/src/demo.ts");
  const { blankDraft, readerDocument } =
    await server.ssrLoadModule("/src/api.ts");
  const {folderTree,folderLabel}=await server.ssrLoadModule("/src/folders.ts");
  const folders=[{account:"1",name:"Projects.2026",delimiter:".",selectable:true},{account:"1",name:"Projects",delimiter:".",selectable:false},{account:"1",name:"Flat/Name",delimiter:null,selectable:true}];
  const tree=folderTree(folders);assert.equal(tree.length,2);const projects=tree.find(n=>n.name==="Projects");assert.equal(projects.folder.selectable,false);assert.equal(projects.children[0].folder.name,"Projects.2026");assert.equal(folderLabel("&U,BTFw- &-"),"台北 &");assert.equal(folderLabel("&invalid-"),"&invalid-");
  const b = await dispatch("bootstrap", {});
  assert.equal(b.demo, true);
  assert.equal(b.settings.ai.enabled, false);
  assert.equal(b.settings.ai.consent_to_send_contents, false);
  const first = await dispatch("messages", {
    account: "demo",
    view: "imbox",
    limit: 100,
    offset: 0,
  });
  const second = await dispatch("messages", {
    account: "demo",
    view: "imbox",
    limit: 100,
    offset: 100,
  });
  assert.equal(first.total, 140);
  assert.equal(second.messages.length, 40);
  assert(
    !first.messages.some((m) => second.messages.some((n) => n.id === m.id)),
  );
  const draft = {
    ...blankDraft("alex@example.test"),
    to: ["friend@example.test"],
    subject: "Contract check",
    text: "Hello",
    attachments: [
      { filename: "notes.txt", content_type: "text/plain", bytes: [72, 105] },
    ],
  };
  const saved = await dispatch("save_draft", {
    account: "demo",
    id: null,
    draft,
  });
  assert.deepEqual(
    await dispatch("draft", { account: "demo", id: saved.id }),
    draft,
  );
  await dispatch("queue_draft", { account: "demo", id: saved.id });
  assert.equal((await dispatch("drafts", {})).drafts[0].state, "queued");
  assert.equal(
    (await dispatch("messages", { account: "demo", view: "sent" })).total,
    0,
  );
  await dispatch("triage", {
    account: "demo",
    senders: [{ address: "maya@example.test", destination: "feed" }],
  });
  assert(
    (
      await dispatch("messages", {
        account: "demo",
        view: "feed",
        query: "Maya",
        limit: 500,
      })
    ).messages.every((m) => m.destination === "feed"),
  );
  const c = await dispatch("save_contact", {
    contact: {
      uid: "",
      name: "Test",
      email: "test@example.test",
      phone: "",
      emails: ["test@example.test", "other@example.test"],
      organization: "Studio",
      note: "Keep this",
    },
  });
  const updated = await dispatch("save_contact", {
    contact: { ...c, name: "Updated" },
  });
  assert.equal(updated.organization, "Studio");
  assert.equal(updated.emails.length, 2);
  await assert.rejects(dispatch("save_contact", { contact: c }), /changed/);
  const e = await dispatch("save_event", {
    event: {
      uid: "",
      title: "Review",
      start: 100,
      end: 200,
      description: "",
      location: "",
      attendees: [],
      all_day: false,
    },
  });
  const e2 = await dispatch("save_event", {
    event: { ...e, title: "Review updated" },
  });
  assert.equal(e2.revision, 2);
  await assert.rejects(dispatch("save_event", { event: e }), /changed/);
  const exported = await dispatch("pim_export", { kind: "calendar" });
  assert(exported.text.includes("BEGIN:VEVENT"));
  assert(
    (await dispatch("pim_import", { kind: "calendar", text: exported.text }))
      .imported > 0,
  );
  const cards = await dispatch("pim_export", { kind: "contacts" });
  assert(
    (await dispatch("pim_import", { kind: "contacts", text: cards.text }))
      .imported > 0,
  );
  const blocked = readerDocument('<img src="https://example.test/a">', false);
  assert(blocked.includes("default-src 'none'"));
  assert(blocked.includes("img-src data: cid:;"));
  assert(!blocked.includes("script-src 'unsafe-inline'"));
  assert(readerDocument("", true).includes("img-src data: cid: https:;"));
  await assert.rejects(
    dispatch("add_account", { password: "never-store-this" }),
    /desktop app/,
  );
  console.log(
    "Contract checks passed: paging, triage, draft queue, revisions, extras, CSP, credential isolation.",
  );
} finally {
  delete globalThis.window;
  await server.close();
}
