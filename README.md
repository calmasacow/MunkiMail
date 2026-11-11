# MunkiMail

A local-first desktop mail, calendar, and contacts client built with Rust,
Tauri 2, and SolidJS. No MunkiMail account or hosted backend is required.
Mail providers, DAV servers, and explicitly enabled AI providers still receive
requests. This is an implementation in progress, not a production security certification.

## Current implementation

- Desktop account setup, mailbox reader, search, triage, drafts, attachments,
  reply/forward, queued sending, calendar, contacts, and settings.
- IMAP over verified TLS; SMTP with implicit TLS or STARTTLS. Password/app-password
  and Google/Microsoft/Yahoo OAuth authentication, PKCE/state loopback sign-in, refresh
  tokens in the OS keyring. OAuth requires your own native/public client registration.
- Buffers, MeBox, Feeds, Trails, Round2its, Asides, sender decisions,
  subject renaming, and thread merging are local overlays. Normal desktop sync
  does not publish MunkiMail keywords or settings mailboxes. Read/unread syncs
  across devices; explicit sending submits mail and saves a Sent copy.
- An account-scoped IMAP folder tree sits below the special views. Discovery
  caches the server hierarchy; selecting a folder downloads its contents on demand.
  Folder searches remain scoped to that account and folder. No move, rename,
  delete, subscription, or server-label operations are exposed. Special destination
  views use Inbox mail so browsing Sent/Trash/Gmail labels does not duplicate or
  reroute those messages into MeBox/Feeds/Trails/Buffers.
- Persistent drafts and frozen queued MIME. Interrupted/ambiguous sends require
  review rather than automatic resend; confirmed delivery queues a separate Sent copy.
- SQLite FTS5 search; `from:`, `in:`, `has:attachment`, `is:unread`,
  `before:`, and `after:` filters.
- Desktop SQLCipher encryption always enabled. Passwords, API keys, OAuth tokens,
  and the randomly generated database key use the native OS keyring, with no
  plaintext credential fallback. Bootstrap does not expose stored credentials.
- Calendar/contact editing, ICS/VCF import/export, DAV synchronization, revision
  checks and conflict handling. Recurrence data is preserved, not expanded.
- Optional AI reply suggestions, query translation and sender suggestions. Disabled
  by default; enablement and consent are required before sending context. Review
  suggestions before acting. The desktop currently includes the AI dependency at
  build time; runtime opt-in controls requests.
- Sanitized mail HTML with scripts blocked and remote images blocked by default.
  Allowing remote images can disclose your IP and that the message was viewed.

## Setup

Install stable Rust, Node.js 22/npm, and the native Tauri prerequisites:

| OS | Build prerequisites | Credential storage |
|---|---|---|
| Linux | C/C++ toolchain, pkg-config, WebKitGTK 4.1 development files, GTK/AppIndicator, librsvg, patchelf, OpenSSL development files | Running, unlocked Secret Service (for example GNOME Keyring or compatible KWallet) |
| macOS | Xcode Command Line Tools | Keychain |
| Windows | Visual Studio C++ Build Tools, Windows SDK, WebView2, Strawberry Perl and NASM on PATH | Credential Manager |

SQLCipher builds vendored OpenSSL; Perl and a native compiler are needed.
The keyring uses native backends; Linux uses synchronous Secret Service, Rust
crypto, and vendored D-Bus. Unlock the keychain if credentials cannot be accessed.

Ubuntu 24.04 build dependencies:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf libssl-dev libdbus-1-dev
```

Run the desktop from the repository root:

```sh
cd apps/desktop
npm ci
npm run tauri -- dev
```

`apps/desktop/package-lock.json` locks frontend dependencies; `Cargo.lock` locks
Rust dependencies. Keep both lockfiles committed and aligned with their manifests.

For seeded native demo data, use `npm run tauri -- dev -- --demo`. Demo mode uses
a separate **unencrypted** `demo.db`; external account connections, sending, DAV,
and AI requests are disabled. `npm run dev` opens only the browser UI/demo, not
the native backend, and cannot connect real accounts.

Right-click **YOUR SPACE** to choose **All** (combined mail) or a configured account.
The filter applies to mailbox views, search and drafts; message actions retain the
owning account. Clicking the heading also opens this menu for keyboard access.

### Experimental GPUI application (development paused)

Tauri is the primary interface for continued desktop/mobile development.
The retained experimental native Rust interface is built with
[Ely GPUI Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components),
pinned to `9efab23a1db2f54c327a150e6870db65e3a8adfd` and GPUI `0.3.11`.
Rust 1.98 or newer is required.

```sh
cargo run -p munkimail-gpui
```

Launch opens the existing encrypted `mail.db` through the same backend and
OS keyring used by the Tauri application. No data conversion is performed.
Normal builds do not permit synthetic demo mode. Synthetic data is restricted
to the explicit `ui-check` build below.

The native interface includes account-scoped mail/search, multi-select sender routing,
read status, Round2its/Asides, subject renaming and thread merging. Compose supports
reply/forward, draft autosave, attachments and queued sending. Account connection,
provider OAuth/sync, recovery decisions, calendar/contact editing and import/export,
DAV sync/conflicts, RSVP, appearance/privacy preferences and opt-in AI use the same
validated dispatcher as Tauri. Keyboard shortcuts: `c` compose, `/` search, `j`/`k`
message navigation. Calendar times in the editor are UTC; all-day ends are exclusive.
Recurring events remain read-only.

HTML renders with native GPUI elements from backend-sanitized markup; browser CSS
layout is not reproduced. Remote images require explicit consent. Provider sign-in,
real mail delivery, DAV servers and OS file dialogs still need live integration
verification. The Tauri application remains available during migration.

```sh
cargo test -p munkimail-gpui --locked
cargo clippy -p munkimail-gpui --all-targets --features ui-check --locked -- -D warnings
cargo run -p munkimail-gpui --features ui-check -- --demo --ui-check /tmp/munkimail-gpui-shots
```

The UI check requires a desktop display. It dispatches pointer events through
Ely's native list, captures this app's frames where GPUI supports capture, then
exits. GPUI's Linux backend currently reports capture unsupported; native event
assertions still run there. Storage
checks cover two accounts, aggregate paging, search and account ownership. Native
workflow assertions cover draft autosave, calendar/contact create-delete, and
preferences saving. Linux frame capture is unavailable; these checks do not
provide visual screenshots.

Ely is MIT licensed; its embedded Lucide icons are ISC licensed and its fonts
use SIL OFL 1.1. Upstream notices remain in the pinned dependency: see
[Ely LICENSE](https://github.com/ELY-Open-Source/Ely-GPUI-Components/blob/9efab23a1db2f54c327a150e6870db65e3a8adfd/LICENSE)
and the font/icon notices in that source. Retain them with packaged builds.

### Connect accounts

Use the desktop account form with your email, transport, server hosts/ports,
and authentication method. Password authentication may require a provider-issued
app password. Google, Microsoft and Yahoo presets fill the standard server
settings; enter the client ID from your own provider registration. Configure
the intended Sent folder (provider-localized names can differ from the preset). After confirmed SMTP delivery, IMAP creates a missing
configured Sent folder and reselects it before saving; JMAP lazily creates the
mailbox path before importing the Sent copy. Permission failures remain actionable
errors rather than triggering SMTP resubmission.

For Google/Microsoft/Yahoo OAuth, register a native/public application, configure its
client ID, and enable loopback redirects to
`http://127.0.0.1:{port}/oauth/callback`. No embedded client secret is assumed.
Provider registration/consent policies still apply. Finish sign-in in the system
browser, then complete sign-in in the app within three minutes.

Google requests `https://mail.google.com/`; Microsoft requests IMAP/SMTP scopes
plus offline access. Yahoo requests `mail-w` and uses PKCE with OAUTHBEARER
for mail connections. Yahoo mail access is approval-gated; an ordinary sign-in
client ID alone is insufficient. See [Yahoo developer access](https://senders.yahooinc.com/developer/developer-access/)
and [public-client OAuth guidance](https://developer.yahoo.com/sign-in-with-yahoo/).
Use `imap.gmail.com` / `smtp.gmail.com`, or
`outlook.office365.com` (also `outlook.office.com`) / `smtp.office365.com`.
Yahoo uses `imap.mail.yahoo.com` / `smtp.mail.yahoo.com`.
Sync validates both provider mail hosts before loading credentials; these OAuth
presets reject JMAP. Sign-in setup can still accept invalid server configuration,
but sync rejects it before credential access or transmission. JMAP accounts use
password/app-password credentials, including SMTP credentials for queued sending.

DAV requires concrete HTTPS calendar/address-book collection URLs and credentials;
principal discovery is not implemented. Settings URLs and AI endpoints are trust
choices: configure only services intended to receive the corresponding data.

### Storage and recovery

The native app stores `mail.db` in Tauri's platform-specific application data
directory. Calendar and contacts share that database. Encryption keys stay in the
OS keyring; retaining only the database is insufficient for recovery. Keep keychain
access and a consistent database backup, including committed WAL contents. Do not
copy only a live `mail.db` while ignoring its WAL.

The database-key entry currently incorporates the database path. Moving/restoring
its location requires preserving the matching key association; do not delete keys
as a troubleshooting step. Wrong-key/open failures must not trigger database deletion.
The desktop enables SQLCipher directly on its storage dependency; no desktop
`encrypted` feature flag is needed. Storage-library-only builds can use plaintext
SQLite; they do not provide the desktop's encryption guarantee.

Desktop schema v6 enables local-only views and retires queued destination, stack,
settings-publication and move operations while retaining their local decisions.
Read/unread and delivery work remain queued. Existing server-side MunkiMail
folders/keywords from earlier versions are left untouched; no cleanup deletes
server content. Older binaries cannot open the migrated schema. Library-only
legacy metadata replication remains for compatibility tests, outside the normal
desktop sync path.

## Themes and native Wayland

Theme behavior matches WaveSink: **Follow Omarchy** on Omarchy installations,
**Original** elsewhere, plus portable snapshots of this machine's **22 stock and
15 custom themes**. Explicit choices persist immediately per device. The swatch
picker appears under Settings → Look & feel → Appearance. Existing light-mode
choices migrate to Flexoki Light. Omarchy changes update within two seconds; choosing a
static preset stops following desktop changes. No system config or hooks changed.

Normal builds include every palette; non-Omarchy Linux, Windows and macOS need no
Omarchy installation. [Theme inventory, source mapping and regeneration](apps/desktop/src/themes/README.md).

On Linux with `WAYLAND_DISPLAY`, GTK uses Wayland directly. Startup applies the
same WebKit DMA-BUF renderer workaround as WaveSink, preserving an explicit
`WEBKIT_DISABLE_DMABUF_RENDERER` override. This avoids the observed compositor
protocol crash without switching to X11/XWayland. Native operation was verified
on this Hyprland session with `xwayland: false`.

## Validation and packaging

From the repository root:

```sh
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run --locked -p munkimail -- --self-check
cargo test -p munki-store --no-default-features --features encryption --test encryption
```

Workspace checks include the native desktop and require its OS dependencies.
`--self-check` uses temporary seeded demo data; it does not verify provider OAuth,
a real keychain, or encrypted production database recovery. The separate SQLCipher
test checks ciphertext and wrong-key rejection. OAuth/DAV/JMAP/AI unit tests use
local mocks; they do not certify external-provider interoperability.

Local Linux verification (2026-11-03): 88 workspace tests passed; the 11 additional
mail-server integration tests passed against disposable GreenMail; eleven browser
demo scenarios passed, including all 37 presets and live-theme IPC simulation. Rustfmt and strict Clippy passed. The 100,000-message
release search benchmark measured worst p95 25 ms on this machine. Native and
packaged AppImage self-checks passed; these cover backend operations without
opening a native window. Native Wayland launch was additionally verified on Hyprland.
Real provider sign-in, broader native window interactions,
macOS/Windows builds and clean-machine installation remain unverified locally.

Frontend checks:

```sh
cd apps/desktop
npm test
npm run build
npx playwright install --with-deps chromium
npm run test:ui
```

[Screenshots](apps/desktop/src/screenshots/README.md) document the synthetic demo.

Build a native package on the target OS from `apps/desktop`:

```sh
npm run tauri -- build --ci --no-sign -- --locked
```

Artifacts are under the repository's `target/release/bundle/`. The
[desktop workflow](.github/workflows/desktop.yml) runs checks and creates Linux
DEB/AppImage, macOS DMG plus a tarred `.app` bundle, and Windows NSIS packages on native hosted runners. It
uploads workflow artifacts, including for pull requests; it does not publish
GitHub releases. Builds target the runner's architecture, not universal binaries.
Packages are unsigned and macOS builds are not notarized. Platform trust prompts
are expected; signing/notarization and install/upgrade testing remain release work.
No signing secrets or provider credentials are needed by CI.

Optional local-server and performance checks:

```sh
docker run -d --name munki-greenmail -p 127.0.0.1:3143:3143 -p 127.0.0.1:3025:3025 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.imap -Dgreenmail.setup.test.smtp -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled' \
  greenmail/standalone:2.1.3
cargo test -p munki-net --test greenmail --test compose -- --ignored
docker rm -f munki-greenmail
cargo test -p munki-store --release --test perf -- --ignored --nocapture
```

Use throwaway local test mail only. Performance target: search p95 below 100 ms
on 100,000 messages; run the benchmark to establish results on your hardware.

## Product plan

The product promise: **calm mail, dependable scheduling, useful people context,
on every device, without changing your email address.** Start with individuals
and small teams using existing providers. Enterprise migration comes later.

| Feedback | Product decision | Success measure |
|---|---|---|
| HEY reduces clutter but feels rigid | Optional Buffers, reversible sender routing, familiar mail terminology, configurable views | First-time users can find an expected message and reverse a decision without help |
| Outlook feels heavy and loses offline features | Local database, durable action queue, small shared Rust core | Cached mail/calendar/contacts remain readable and editable offline; reconnect preserves edits |
| Apple Mail search and sync feel unreliable | Indexed search, visible sync status, explicit recovery queue | No silent missing results in fixture tests; every failed send has an actionable state |
| Formatting and attachments are unpredictable | Plain-text default, restrained HTML, attachment list and optional previews | Recipient tests preserve paragraphs, links, filenames and Bcc privacy |
| Notifications and tracking interrupt users | Notifications off, remote images blocked, no ads or content-based telemetry | Fresh installation makes no optional AI requests or remote image requests |

Primary navigation: **Mail · Calendar · People**. Mail provides MeBox, Buffers,
Feeds, Trails, Round2its, Asides, Drafts and search. A standard inbox
mode should remain available for users who prefer their provider workflow.
Sender decisions explain their effect; dismissal hides mail without deleting it.
Feeds should offer a continuous reading view; Trails should emphasize
receipts, files and retrieval. These richer views remain design backlog.

Calendar should support agenda/day/week/month, timezone-aware scheduling,
invitation responses, recurrence and shared availability. People should combine
editable contacts with related conversations and events, with explicit duplicate
merging. Shared calendars, availability and contact merging remain backlog.

Platform scope: Linux, macOS and Windows desktop first; iOS and Android next.
Share domain/storage/sync logic, adapt navigation, accessibility and background
execution to each OS. Mobile requires its own secure-storage, notification,
battery and suspended-sync validation. A browser companion is a separate product
phase: this repository's browser demo is not a hosted mail client.

Release sequence: (1) reliable desktop private alpha; (2) provider interoperability,
accessibility and signed desktop beta; (3) mobile apps with tested offline parity;
(4) shared mailboxes/calendars, delegated access and audited automation if demand
justifies them. Defer COM/VBA compatibility, PST editing, a plugin marketplace and
a custom mail service. Support migration through validated import/export before
promising legacy compatibility. Use a paid, ad-free model; validate willingness
to pay before setting pricing. Keep data export available without subscription.

Validate the design with five realistic tasks: connect an account; recover an
expected screened message; compose offline then reconnect; respond to an invite;
find a person and their last conversation. Measure completion, errors and recovery
on each platform before expanding scope. Track crash-free sessions, sync lag,
search latency and duplicate sends using opt-in diagnostics without mail content.

This plan preserves the requested local-first product alongside implementation.
Priorities reflect the stated requirements and security review; external user
research or acceptance testing has not been established in this repository.
Implemented code is not equivalent to an accepted milestone.

| Priority / milestone | Current state | Acceptance criteria |
|---|---|---|
| P0 — Safe account and encrypted storage foundation | Implemented; native verification remains a gate | Native keyring works on Linux/macOS/Windows; locked/unavailable storage fails without plaintext fallback; correct key reopens data and wrong key fails; restart preserves accounts without exposing credentials to the frontend |
| P0 — OAuth trust and recovery | PKCE/state, host binding, pending-flow identity and serialized token writes implemented; provider/native verification pending | Provider tokens reach only permitted provider mail servers; pending flows remain bound to originating configuration; overlapping sign-in/refresh cannot overwrite newer tokens; cancellation/failure preserves working account configuration |
| P1 — Dependable everyday mail | IMAP/JMAP, SMTP sending, Sent copies and local triage implemented | Connect a throwaway account, sync, search, triage, restart and send; verify Sent copy; simulate lost SMTP acknowledgement and confirm review rather than duplicate send; verify replies/attachments in Gmail and Outlook |
| P1 — Usable desktop workflow | Tauri UI and demo implemented | Complete account setup, reader, paging, search, compose, attachment download and error recovery on each OS; keyboard-only navigation and readable layouts at supported window sizes |
| P2 — Calendar and contacts | Local editing/import/export and DAV implemented | Round-trip supported ICS/VCF without losing preserved fields; reject stale edits; demonstrate DAV conflict resolution without silent overwrite; communicate recurrence limits in the UI |
| P2 — Optional AI assistance | Runtime opt-in implementation | No provider request before enablement and consent; revocation stops requests; suggestions stay reviewable; activity omits message contents and secrets |
| P3 — Distribution and release readiness | CI packaging configured; hosted runs/install checks pending | Three OS jobs pass; install, launch, upgrade and reopen encrypted data on clean machines; sign Windows/macOS packages and notarize macOS; document recovery before public release |

### Security review report and implemented fixes

Review scope: desktop `main.rs`, `backend.rs`, `network.rs` and the relevant
keyring, SQLCipher and transport boundaries. Static verification of current code;
real-provider sign-in and native keychain interoperability still require testing.

1. **P1: Provider-token destination binding — implemented.** Sync and credential
   loading validate both IMAP and SMTP against Google/Microsoft host allowlists
   before keyring access or token refresh. These provider presets reject JMAP.
   Local tests cover foreign hosts and unsupported transport. Sign-in setup does
   not validate hosts itself: an invalid configuration can be activated, but sync
   rejects it before credentials are loaded or transmitted.
2. **P2: Authorization/configuration and token-write races — implemented.** Pending
   authorization retains proposed configuration and provider credential identity;
   sign-in start leaves the active configuration untouched. Overlapping sign-ins
   and account updates during a pending flow are rejected. Completion persists
   tokens before committing configuration. A shared credential mutex serializes
   sync, completion persistence and account updates across accounts, without
   holding it during browser authorization. Abandoned pending listeners expire.
   Detached sync/completion/update tasks retain exclusion when IPC disconnects;
   ordinary completion/error paths release reservations. A focused local test
   covers overlap rejection and cleanup after authorization timeout.

Lock ordering has no cycle in these paths: completion drops the credential mutex
before reacquiring the pending-flow map for cleanup. Account updates may hold
that map while waiting for credentials; completion does not wait for it while
holding credentials. Unexpected task panics are outside the ordinary cleanup guarantee.

The encrypted build applies the SQLCipher key before database access, keeps
secrets out of bootstrap, and has no plaintext keyring fallback. The single-instance
plugin addresses cross-process initial key provisioning. Database transactions and
OS keyring writes are separate persistence systems, not an atomic combined commit;
a database commit failure after a keyring write can require sign-in recovery.
These implemented fixes do not replace real-provider, native-keychain and recovery tests.

### Remaining backlog and explicit limits

- Provider registration walkthroughs and real Gmail/Microsoft/Yahoo interoperability;
  safe key backup/restore/migration; native keychain tests; signing and notarization.
- JMAP currently indexes Inbox. Desktop integration sends through configured SMTP,
  imports Sent copies through JMAP; desktop triage stays local.
  Native JMAP submission, broader mailbox indexing and push remain backlog;
  unsupported queued operations surface for review rather than disappearing.
- Local triage metadata is capped at 8 MiB per account. Cross-device MunkiMail
  view synchronization is deliberately disabled to preserve server structure.
- Thread merging covers existing messages; future replies retain their original
  root. Custom automation and DKIM-based suggestions remain pending. Untrusted
  authentication headers are not proof of identity.
- Mail addresses are ASCII; forwarded attachments need explicit reattachment.
  JMAP folder-tree browsing, push/background delivery guarantees, and broader
  mailbox coverage need further work.
- Recurrence expansion/editing, DAV discovery, richer timezone handling and
  provider-specific invitation interoperability remain limited. See
  [PIM limitations](crates/munki-pim/README.md).
- Clean-machine packaging/upgrade checks, accessibility review and performance
  validation on representative user hardware remain release gates.

## Repository layout

| Path | Responsibility |
|---|---|
| `apps/desktop` | SolidJS UI and Tauri native application |
| `crates/munki-core` | Domain types, triage and query grammar |
| `crates/munki-store` | SQLite/SQLCipher, migrations, FTS and outbox |
| `crates/munki-net` | IMAP/SMTP, MIME, composition and HTML sanitization |
| [munki-auth](crates/munki-auth/README.md) | OS secrets and native OAuth |
| [munki-pim](crates/munki-pim/README.md) | Calendar, contacts and DAV |
| [munki-jmap](crates/munki-jmap/README.md) | JMAP client and mapping |
| [munki-ai](crates/munki-ai/README.md) | Opt-in suggestion APIs |
