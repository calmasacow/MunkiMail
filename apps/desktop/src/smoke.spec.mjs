import { test, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
const screenshots = new URL("./screenshots/", import.meta.url).pathname;
async function shot(page, name) {
  await mkdir(screenshots, { recursive: true });
  await page.screenshot({ path: `${screenshots}${name}.png`, fullPage: true });
}
test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-11-03T18:00:00Z"));
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "MeBox", exact: true }),
  ).toBeVisible();
});
test("reader pagination keyboard search and stacks", async ({ page }) => {
  await expect(page.getByText("DEMO MODE", { exact: false })).toBeVisible();
  await expect(page.locator(".message-row")).toHaveCount(18);
  await page.locator(".message-button").first().click();
  await expect(page.locator(".message-body")).toContainText("Hi Alex");
  await shot(page, "mail-reader");
  await page.getByRole("button", { name: /Load more/ }).click();
  await expect(page.getByRole("button", { name: /Load more/ })).toHaveCount(0);
  await page.locator(".message-scroll").evaluate((e) => (e.scrollTop = 11000));
  await expect(page.locator("#message-111")).toBeAttached();
  expect(await page.locator(".message-row").count()).toBeLessThan(25);
  await page.locator("#message-111 .message-button").click();
  await page.keyboard.press("j");
  await expect(page.locator("#message-112")).toHaveClass(/selected/);
  await page.locator(".reader")
    .getByRole("button", { name: "◷ Round2its", exact: true })
    .click();
  await page.getByRole("navigation", { name: "Mailboxes" })
    .getByRole("button", { name: "◷ Round2its", exact: true })
    .click();
  await expect(page.locator(".message-row")).toHaveCount(1);
  await page.keyboard.press("/");
  await expect(
    page.getByRole("textbox", { name: "Search messages" }),
  ).toBeFocused();
  await page
    .getByRole("textbox", { name: "Search messages" })
    .fill("Coffee next Thursday");
  await expect(page.locator(".message-subject").first()).toContainText(
    "Coffee next Thursday",
  );
});
test("bulk sender review", async ({ page }) => {
  await page
    .getByRole("button", { name: "◇ Buffers", exact: true })
    .click();
  await page.getByRole("checkbox", { name: "Select all senders" }).check();
  await shot(page, "screener");
  await page.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.locator(".message-row")).toHaveCount(0);
  await expect(page.locator(".notice")).toContainText(
    "Sender preferences updated",
  );
});
test("draft autosave attachment reopen queue", async ({ page }) => {
  await page.keyboard.press("c");
  const dialog = page.getByRole("dialog");
  await dialog
    .getByRole("textbox", { name: "To", exact: true })
    .fill("friend@example.test");
  await dialog
    .getByRole("textbox", { name: "Subject", exact: true })
    .fill("A quiet hello");
  await dialog
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("A fully editable local draft.");
  await dialog.locator("input[type=file]").setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("Attachment check"),
  });
  await expect(dialog.getByText("Saved", { exact: true })).toBeVisible();
  await shot(page, "composer");
  await dialog.getByRole("button", { name: "Close dialog" }).click();
  await page.getByRole("button", { name: "✎ Drafts", exact: true }).click();
  await page.getByRole("button", { name: /A quiet hello/ }).click();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue("A fully editable local draft.");
  await expect(
    page.getByRole("textbox", { name: "To", exact: true }),
  ).toHaveValue("friend@example.test");
  await page
    .getByRole("button", { name: "Queue message ↗", exact: true })
    .click();
  await expect(page.locator(".notice")).toContainText("No mail was sent.");
  await expect(page.locator(".draft-list")).toContainText("queued");
});
test("calendar CRUD month week day", async ({ page }) => {
  await page.getByRole("button", { name: "▦ Calendar", exact: true }).click();
  await page.getByRole("button", { name: "＋ Event", exact: true }).click();
  let dialog = page.getByRole("dialog");
  await dialog
    .getByRole("textbox", { name: "Title", exact: true })
    .fill("Planning with Maya");
  await dialog
    .getByRole("textbox", { name: "Location", exact: true })
    .fill("Studio");
  await dialog.getByRole("button", { name: "Save event", exact: true }).click();
  await expect(
    page.getByRole("button", { name: /Planning with Maya/ }),
  ).toBeVisible();
  await shot(page, "calendar-month");
  await page
    .getByRole("combobox", { name: "Calendar view" })
    .selectOption("week");
  await expect(page.locator(".calendar-day")).toHaveCount(7);
  await page
    .getByRole("combobox", { name: "Calendar view" })
    .selectOption("day");
  await expect(page.locator(".calendar-day")).toHaveCount(1);
  await page.getByRole("button", { name: /Planning with Maya/ }).click();
  dialog = page.getByRole("dialog");
  await dialog
    .getByRole("textbox", { name: "Title", exact: true })
    .fill("Planning updated");
  await dialog.getByRole("button", { name: "Save event", exact: true }).click();
  await page.getByRole("button", { name: /Planning updated/ }).click();
  await page.getByRole("button", { name: "Delete event", exact: true }).click();
  await expect(
    page.getByRole("button", { name: /Planning updated/ }),
  ).toHaveCount(0);
});
test("contacts CRUD unified conversations", async ({ page }) => {
  await page.getByRole("button", { name: "♧ People", exact: true }).click();
  await page
    .getByRole("button", { name: "＋ Add person", exact: true })
    .click();
  let dialog = page.getByRole("dialog");
  await dialog
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Robin Taylor");
  await dialog
    .getByRole("textbox", { name: "Email", exact: true })
    .fill("robin@example.test");
  await dialog
    .getByRole("textbox", { name: "Organization", exact: true })
    .fill("Field Studio");
  await dialog
    .getByRole("textbox", { name: "Notes", exact: true })
    .fill("Met at workshop");
  await dialog
    .getByRole("button", { name: "Save contact", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Robin Taylor", exact: true }),
  ).toBeVisible();
  await shot(page, "contacts");
  await page
    .locator(".contact-card")
    .filter({ hasText: "Robin Taylor" })
    .getByRole("button", { name: "Edit", exact: true })
    .click();
  dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("textbox", { name: "Organization", exact: true }),
  ).toHaveValue("Field Studio");
  await dialog
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Robin updated");
  await dialog
    .getByRole("button", { name: "Save contact", exact: true })
    .click();
  await page
    .locator(".contact-card")
    .filter({ hasText: "Robin updated" })
    .getByRole("button", { name: "Edit", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Delete contact", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Robin updated", exact: true }),
  ).toHaveCount(0);
  await page
    .locator(".contact-card")
    .filter({ hasText: "Maya Chen" })
    .getByRole("button", { name: "Conversations", exact: true })
    .click();
  await expect(page.locator(".person-message").first()).toBeVisible();
  await page.locator(".person-message").first().click();
  await expect(page.locator(".message-body")).toContainText("Hi Alex");
});
test("settings AI consent light theme recovery", async ({ page }) => {
  await page.getByRole("button", { name: /Settings & accounts/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("checkbox", { name: "Enable AI assistance", exact: true }),
  ).not.toBeChecked();
  await expect(
    dialog.getByRole("checkbox", { name: /I consent/ }),
  ).not.toBeChecked();
  await dialog
    .getByRole("checkbox", { name: "Enable AI assistance", exact: true })
    .check();
  await expect(
    dialog.getByRole("checkbox", { name: /I consent/ }),
  ).not.toBeChecked();
  await shot(page, "settings-dark");
  await dialog.getByRole("button", { name: /^Appearance:/ }).click();
  await dialog
    .getByRole("menuitemradio", { name: "Flexoki Light", exact: true })
    .click();
  await dialog
    .getByRole("button", { name: "Review pending operations", exact: true })
    .click();
  await expect(dialog.getByText("No operations need review.")).toBeVisible();
  await dialog
    .getByRole("button", { name: "Save settings", exact: true })
    .click();
  await expect(page.locator("html")).toHaveAttribute(
    "data-theme",
    "flexoki-light",
  );
  await page.getByRole("button", { name: /Settings & accounts/ }).click();
  await expect(
    page.getByRole("checkbox", { name: "Enable AI assistance", exact: true }),
  ).toBeChecked();
  await expect(
    page.getByRole("checkbox", { name: /I consent/ }),
  ).not.toBeChecked();
  await page.getByRole("button", { name: "Close dialog" }).click();
  await shot(page, "mail-light");
});
test("responsive narrow reader composer", async ({ page }) => {
  await page.setViewportSize({ width: 430, height: 932 });
  await page.locator(".message-button").first().click();
  await expect(page.locator(".message-body")).toBeVisible();
  expect(
    await page
      .locator("body")
      .evaluate((e) => e.scrollWidth <= window.innerWidth),
  ).toBe(true);
  await shot(page, "mail-mobile");
  await page.getByRole("button", { name: /New message/ }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page
    .getByRole("textbox", { name: "To", exact: true })
    .fill("friend@example.test");
});

test("RSVP sends revision, prevents double click and refreshes reader", async ({
  page,
}) => {
  await page.locator("#message-3 .message-button").click();
  const accept = page.getByRole("button", { name: "Accept", exact: true });
  await expect(accept).toBeVisible();
  await accept.click();
  await expect(page.locator(".notice")).toContainText(
    "Demo invitation response recorded locally. No mail was sent.",
  );
  await expect(accept).toBeEnabled();
  await page.getByRole("button", { name: "Maybe", exact: true }).click();
  await expect(page.locator(".alert")).toHaveCount(0);
  await expect(page.locator(".notice")).toContainText(
    "Demo invitation response recorded locally.",
  );
});

test("all-day calendar dates save with exclusive end and reopen", async ({
  page,
}) => {
  await page.getByRole("button", { name: "▦ Calendar", exact: true }).click();
  await page.getByRole("button", { name: "＋ Event", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Title", exact: true })
    .fill("Studio holiday");
  await page.getByRole("checkbox", { name: "All day", exact: true }).check();
  await page.getByLabel("Starts", { exact: true }).fill("2026-11-03");
  await page.getByLabel("Ends (exclusive)", { exact: true }).fill("2026-11-05");
  await page.getByRole("button", { name: "Save event", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Studio holiday", exact: true }),
  ).toHaveCount(2);
  await page
    .getByRole("button", { name: "Studio holiday", exact: true })
    .first()
    .click();
  await expect(page.getByLabel("Starts", { exact: true })).toHaveValue(
    "2026-11-03",
  );
  await expect(
    page.getByLabel("Ends (exclusive)", { exact: true }),
  ).toHaveValue("2026-11-05");
  await expect(
    page.getByRole("checkbox", { name: "All day", exact: true }),
  ).toBeChecked();
});

test("all 37 static palettes work without Omarchy and selection persists", async ({
  page,
}) => {
  await page.getByRole("button", { name: /Settings & accounts/ }).click();
  const picker = page.getByRole("button", { name: /^Appearance:/ });
  await picker.click();
  await expect(page.getByRole("menuitemradio")).toHaveCount(38);
  await expect(
    page.getByRole("menuitemradio", { name: "Follow Omarchy", exact: true }),
  ).toHaveCount(0);
  // The browser imports actual generated presets, independent of system files.
  const themes = await page.evaluate(
    async () => (await import("/src/themes/omarchy.ts")).BUNDLED_THEMES,
  );
  expect(themes.filter((t) => t.origin === "stock")).toHaveLength(22);
  expect(themes.filter((t) => t.origin === "custom")).toHaveLength(15);
  for (const theme of themes) {
    await page
      .getByRole("menuitemradio", { name: theme.label, exact: true })
      .click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme.id);
    expect(
      await page.evaluate(() =>
        document.documentElement.style.getPropertyValue("--bg"),
      ),
    ).toBe(theme.colors.background);
    const contrast = await page.evaluate(() => {
      const style = getComputedStyle(document.documentElement);
      function l(h) {
        return [1, 3, 5].reduce((s, p, i) => {
          const v = parseInt(h.slice(p, p + 2), 16) / 255;
          return (
            s +
            (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4) *
              [0.2126, 0.7152, 0.0722][i]
          );
        }, 0);
      }
      const a = l(style.getPropertyValue("--on-accent").trim()),
        b = l(style.getPropertyValue("--accent").trim());
      return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
    });
    expect(contrast).toBeGreaterThanOrEqual(4.5);
    await picker.click();
  }
  await page
    .getByRole("menuitemradio", { name: "Catppuccin Latte", exact: true })
    .click();
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute(
    "data-theme",
    "catppuccin-latte",
  );
  await page.getByRole("button", { name: /Settings & accounts/ }).click();
  await picker.click();
  await expect(
    page.getByRole("menuitemradio", { name: "Catppuccin Latte", exact: true }),
  ).toHaveAttribute("aria-checked", "true");
  await shot(page, "theme-picker-light");
  await page.keyboard.press("Escape");
  await expect(picker).toBeFocused();
});

test("live Omarchy defaults, updates, and static overrides through mocked IPC", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__testPalette = {
      name: "tokyo-night",
      colors: {
        accent: "#7aa2f7",
        background: "#1a1b26",
        foreground: "#a9b1d6",
        bright_foreground: "#c0caf5",
        red: "#f7768e",
        yellow: "#e0af68",
        green: "#9ece6a",
      },
    };
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      unregisterCallback: () => {},
      invoke: async (command, args) => {
        if (command === "get_omarchy_theme")
          return structuredClone(window.__testPalette);
        if (command === "dispatch" && args.action==="folders")return {folders:[]};
        if (command === "dispatch")
          return (await import("/src/demo.ts")).dispatch(
            args.action,
            args.payload,
          );
        if (command === "plugin:event|listen") return 1;
        if (command === "plugin:event|unlisten") return;
        throw Error("Unexpected mock IPC: " + command);
      },
    };
  });
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "omarchy");
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.documentElement.style.getPropertyValue("--bg"),
      ),
    )
    .toBe("#1a1b26");
  await page.evaluate(() => {
    window.__testPalette.colors.background = "#101112";
    window.__testPalette.name = "custom-test";
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.documentElement.style.getPropertyValue("--bg"),
      ),
    )
    .toBe("#101112");
  await page.getByRole("button", { name: /Settings & accounts/ }).click();
  const picker = page.getByRole("button", { name: /^Appearance:/ });
  await picker.click();
  await expect(page.getByRole("menuitemradio")).toHaveCount(39);
  await shot(page, "theme-picker-omarchy");
  await page.getByRole("menuitemradio", { name: "Nord", exact: true }).click();
  await page.evaluate(() => {
    window.__testPalette.colors.background = "#202122";
  });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "nord");
  await page.waitForTimeout(2200);
  expect(
    await page.evaluate(() =>
      document.documentElement.style.getPropertyValue("--bg"),
    ),
  ).not.toBe("#202122");
  await picker.click();
  await page
    .getByRole("menuitemradio", { name: "Follow Omarchy", exact: true })
    .click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.documentElement.style.getPropertyValue("--bg"),
      ),
    )
    .toBe("#202122");
  await page.evaluate(() => {
    window.__testPalette = null;
  });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "original");
  await expect(page.locator("html")).not.toHaveAttribute("data-omarchy", "");
  await picker.click();
  await expect(
    page.getByRole("menuitemradio", { name: "Follow Omarchy", exact: true }),
  ).toHaveCount(0);
});

 test("account menu aggregates, filters, paginates and routes by owner", async ({ page }) => {
  await page.addInitScript(() => {
    window.__accountCalls = [];
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => 1, unregisterCallback: () => {},
      invoke: async (command, args) => {
        if (command === "get_omarchy_theme") return null;
        if (command === "plugin:event|listen") return 1;
        if (command === "plugin:event|unlisten") return;
        if (command !== "dispatch") throw Error(command);
        const { dispatch } = await import("/src/demo.ts");
        const { action, payload: p } = args;
        if(action==="folders")return {folders:[]};
        window.__accountCalls.push({ action, ...p });
        if (action === "bootstrap") {
          const b = await dispatch(action, p);
          return { ...b, accounts: [...b.accounts.map((a) => ({ ...a, id: "1" })), { id: "2", email: "second@example.test", display: "Second", kind: "imap" }] };
        }
        if (action === "messages") {
          const r = await dispatch(action, { ...p, account: "demo" });
          return { ...r, messages: r.messages.map((m) => ({ ...m, account: String(p.account), id: m.id + (String(p.account) === "2" ? 10000 : 0), subject: `${String(p.account) === "2" ? "Second" : "First"}: ${m.subject}` })) };
        }
        if (action === "message") {
          const r = await dispatch(action, { ...p, id: p.id > 10000 ? p.id - 10000 : p.id });
          return { ...r, message: { ...r.message, id: p.id, account: String(p.account) } };
        }
        if (action === "set_stack" || action === "triage") return {};
        return dispatch(action, p);
      },
    };
  });
  await page.reload();
  const scope = page.getByRole("button", { name: "YOUR SPACE", exact: true });
  await expect(page.locator(".pane-heading .pill")).toHaveText("280");
  await page.locator(".message-button").first().click();
  await page.locator(".reader").getByRole("button", { name: "◷ Round2its", exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.__accountCalls.find((c) => c.action === "set_stack")?.account)).toBe(2);
  await scope.click({ button: "right" });
  const menu = page.getByRole("dialog", { name: "Filter mail accounts" });
  await expect(menu.getByRole("button").first()).toHaveText("All");
  await expect(menu.getByRole("button")).toHaveCount(3);
  await menu.getByRole("button", { name: "second@example.test", exact: true }).click();
  await expect(page.locator(".pane-heading .pill")).toHaveText("140");
  await expect(page.locator(".message-subject").first()).toContainText("Second:");
  await page.getByRole("button", { name: /YOUR SPACE/ }).click();
  await menu.getByRole("button", { name: "All", exact: true }).click();
  await expect(page.locator(".pane-heading .pill")).toHaveText("280");
  await page.getByRole("button", { name: /Load more/ }).click();
  await expect(page.getByRole("button", { name: /Load more/ })).toContainText("200 of 280");
  await page.getByRole("button", { name: /Load more/ }).click();
  await expect(page.getByRole("button", { name: /Load more/ })).toHaveCount(0);
  await page.getByRole("textbox", { name: "Search messages" }).fill("Coffee next Thursday");
  await expect(page.locator(".pane-heading .pill")).toHaveText("80");
  await page.getByRole("textbox", { name: "Search messages" }).fill("");
  await page.getByRole("button", { name: "◇ Buffers", exact: true }).click();
  await page.getByRole("checkbox", { name: "Select all senders" }).check();
  await page.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.locator(".notice")).toContainText("Sender preferences updated");
  expect(await page.evaluate(() => window.__accountCalls.filter((c) => c.action === "triage").map((c) => String(c.account)))).toEqual(["1", "2"]);
  await page.getByRole("button", { name: /YOUR SPACE/ }).click({ button: "right" });
  await shot(page, "account-menu");
  await page.keyboard.press("Escape");
  await expect(menu).not.toBeVisible();
});

test("provider presets and account-owned server folder tree", async ({page})=>{
  await page.addInitScript(()=>{
    window.__folderCalls=[];
    window.__TAURI_INTERNALS__={transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(command,args)=>{
      if(command==="get_omarchy_theme")return null;if(command==="plugin:event|listen")return 1;if(command==="plugin:event|unlisten")return;
      const {dispatch}=await import("/src/demo.ts");const {action,payload:p}=args;window.__folderCalls.push({action,...p});
      if(action==="bootstrap"){const b=await dispatch(action,p);return {...b,demo:false,accounts:[{id:1,email:"me@example.test",display:"Me",kind:"imap"}]};}
      if(action==="folders")return {folders:[{name:"Projects",delimiter:".",selectable:false},{name:"Projects.2026",delimiter:".",selectable:true},{name:"Flat/Name",delimiter:null,selectable:true}]};
      if(action==="sync_folder")return {fetched:0,pending:0};
      if(action==="messages" && p.view.startsWith("folder:"))return {messages:[],total:0};
      return dispatch(action,p);
    }};
  });
  await page.reload();await expect(page.getByRole("heading",{name:"MeBox",exact:true})).toBeVisible();
  await page.locator(".folder-tree summary").filter({hasText:"Projects"}).click();
  await page.locator(".folder-tree button").filter({hasText:"2026"}).click();
  await expect(page.getByRole("heading",{name:"Projects.2026",exact:true})).toBeVisible();
  await expect.poll(()=>page.evaluate(()=>window.__folderCalls.find(c=>c.action==="sync_folder"))).toEqual({action:"sync_folder",account:1,folder:"Projects.2026"});
  await page.getByRole("textbox",{name:"Search messages"}).fill("invoice");
  await expect.poll(()=>page.evaluate(()=>window.__folderCalls.filter(c=>c.action==="messages").at(-1)?.view)).toBe("folder:Projects.2026");
  await page.getByRole("button",{name:/Settings & accounts/}).click();
  await page.getByRole("combobox",{name:"Account to configure"}).selectOption("");
  await page.getByRole("combobox",{name:"Connection preset"}).selectOption("yahoo");
  await expect(page.locator('[name="imap_host"]')).toHaveValue("imap.mail.yahoo.com");
  await expect(page.locator('[name="smtp_host"]')).toHaveValue("smtp.mail.yahoo.com");
  await expect(page.locator('[name="provider"]')).toHaveValue("yahoo");
  await expect(page.locator('[name="client_id"]')).toBeVisible();
});

test("saved DreamHost account is editable and uses STARTTLS on 587",async({page})=>{
  await page.addInitScript(()=>{
    window.__savedConnection={email:"me@example.test",display:"Me",transport:"imap",imap_host:"imap.dreamhost.com",imap_port:993,smtp_host:"smtp.dreamhost.com",smtp_port:587,smtp_implicit:true,sent_folder:"Sent",auth_kind:"password",oauth_provider:"",oauth_client_id:""};
    window.__TAURI_INTERNALS__={transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(command,args)=>{
      if(command==="get_omarchy_theme")return null;if(command==="plugin:event|listen")return 1;if(command==="plugin:event|unlisten")return;
      const {dispatch}=await import("/src/demo.ts");const {action,payload:p}=args;
      if(action==="bootstrap"){const b=await dispatch(action,p);return {...b,demo:false,accounts:[{id:1,email:"me@example.test",display:"Me",kind:"imap",config:window.__savedConnection}]};}
      if(action==="folders")return {folders:[]};
      if(action==="update_account"){window.__updatedConnection=p;return {id:1};}
      return dispatch(action,p);
    }};
  });
  await page.reload();await page.getByRole("button",{name:/Settings & accounts/}).click();
  await expect(page.locator('[name="imap_host"]')).toHaveValue("imap.dreamhost.com");
  await expect(page.locator('[name="smtp_host"]')).toHaveValue("smtp.dreamhost.com");
  await expect(page.locator('[name="smtp_port"]')).toHaveValue("587");
  await expect(page.getByRole("combobox",{name:"SMTP security"})).toHaveValue("false");
  await page.locator('[name="display"]').fill("Updated name");
  await page.getByRole("button",{name:"Update account",exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>window.__updatedConnection?.display)).toBe("Updated name");
  const saved=await page.evaluate(()=>window.__updatedConnection);
  expect(saved.smtp_implicit).toBe(false);expect(saved.password).toBeUndefined();expect(saved.account).toBe(1);expect(saved.oauth_provider).toBe("");
});

test("pending actions show exact work without retrying it",async({page})=>{
  await page.addInitScript(()=>{
    window.__pendingCalls=[];
    window.__TAURI_INTERNALS__={transformCallback:()=>1,unregisterCallback:()=>{},invoke:async(command,args)=>{
      if(command==="get_omarchy_theme")return null;if(command==="plugin:event|listen")return 1;if(command==="plugin:event|unlisten")return;
      const {dispatch}=await import("/src/demo.ts");const {action,payload:p}=args;window.__pendingCalls.push(action);
      if(action==="bootstrap"){const b=await dispatch(action,p);return {...b,accounts:[{id:1,email:"me@example.test",display:"Me",kind:"imap"}]};}
      if(action==="folders")return {folders:[]};
      if(action==="sync")return {fetched:0,pending:window.__pendingCalls.filter(a=>a==="sync").length===1?1:0};
      if(action==="pending")return {total:1,ops:[{id:5,label:"Save Sent copy",status:"Waiting to retry",attempts:2,folder:"Sent",retry_at:Math.floor(Date.now()/1000)+60,last_error:"Sent folder unavailable"}]};
      return dispatch(action,p);
    }};
  });
  await page.reload();
  await page.clock.install();
  await page.getByRole("button",{name:"Sync mail",exact:true}).click();
  await expect(page.getByRole("button",{name:"Pending actions",exact:true})).toBeVisible();
  await page.clock.fastForward(11_000);
  await expect(page.locator(".topbar .notice")).toContainText("1 pending");
  await page.getByRole("button",{name:"Sync mail",exact:true}).click();
  await expect(page.locator(".notice")).toContainText("0 pending");
  await expect(page.getByRole("button",{name:"Pending actions",exact:true})).toHaveCount(0);
  await expect(page.locator(".topbar .notice")).toBeVisible();
  await page.clock.fastForward(9_000);
  await expect(page.locator(".topbar .notice")).toBeVisible();
  await page.clock.fastForward(1_000);
  await expect(page.locator(".topbar .notice")).toHaveCount(0);
  await page.getByRole("button",{name:/Settings & accounts/}).click();
  await page.getByRole("button",{name:"View all pending actions",exact:true}).click();
  const dialog=page.getByRole("dialog",{name:"Pending server actions"});
  await expect(dialog.getByText("Save Sent copy #5")).toBeVisible();
  await expect(dialog.getByText("Waiting to retry · 2 failed attempts")).toBeVisible();
  await expect(dialog.getByText("Last error: Sent folder unavailable")).toBeVisible();
  await dialog.getByRole("button",{name:"Refresh pending list"}).click();
  expect(await page.evaluate(()=>window.__pendingCalls.filter(a=>a==="review_resolve"))).toEqual([]);
});
