# munki-jmap

No UI, settings configuration, secret persistence, or workspace registration.
Parent supplies a Tokio runtime and serializes sync per Store account.

```rust,ignore
use munki_jmap::{Client, Credentials};

// config.transport == "jmap"; config.jmap_url is a user-supplied origin URL.
let credentials = match config.auth_kind.as_str() {
    "password" => Credentials::Basic {
        username: config.email.clone(), password: secret,
    },
    "oauth" => Credentials::Bearer(secret), // access token, refreshed by parent
    _ => anyhow::bail!("unsupported auth kind"),
};
let client = Client::discover(&config.jmap_url, credentials, None).await?;
let report = client.sync(&mut store, account).await?;
```

Public APIs:

- `Client::discover(&str, Credentials, Option<&str>) -> async Result<Client>`:
  optional remote mail account ID; otherwise primary mail account.
- `Client::sync(&self, &mut Store, AccountId) -> async Result<SyncReport>`:
  `imported`, `deleted`, `deferred` counts. Uses `NewMessage`, caches MIME.
- `Client::load_settings(&self) -> async Result<Vec<Vec<u8>>>`: raw MIME from
  `MunkiMail/.state`; missing mailbox returns empty without creating it.
  Parent parses MIME, extracts and merges StateSnapshot before regular sync.
- `Client::append_to_mailbox(&self, name: &str, message_id: &str, raw: &[u8]) -> async Result<()>`:
  lazily resolves/creates mailbox paths; Message-ID must match MIME. Checks
  `inMailbox` plus Message-ID header before upload/import. Use for state MIME
  and SMTP `SaveSent`. Retry after ambiguous failure runs the lookup again.
  Serialize appends per account; cross-device concurrent appends can race.
- `Client::server_id(&Store, AccountId, local_id: i64) -> Result<Option<String>>`.
- `Client::set_keywords(&self, jmap_id: &str, &[(&str, bool)]) -> async Result<()>`:
  `$seen`, `Destination::keyword()`, `Stack::keyword()`; false removes keyword.
  Destination changes must remove other destination keywords in the same patch.
- `Client::reset_cursor(&Store, AccountId) -> Result<()>`: call after
  `cannotCalculateChanges`; next sync fully enumerates and reconciles deletions.

Parent owns outbox selection, leasing, retry, and settlement. Resolve a queued
folder/UID via `Store::find`, then `server_id`, patch keywords, settle only after
success. Unsupported operations must remain queued or be explicitly surfaced.
Do not let IMAP drain a JMAP account's queue. Any pending account outbox work
conservatively defers updates to existing messages and all deletions, retaining
the cursor for replay. New messages still import. Local triage/settings survive
through Store routing and ingestion APIs.

Discovery always uses the supplied origin's `/.well-known/jmap`; its path is
ignored. HTTPS required except literal loopback IPs/localhost. All redirects
are rejected, including same-origin redirects. apiUrl, uploadUrl, downloadUrl must
share the discovery origin. Credentials exist only inside Client memory.

Inbox sync uses the server's `role=inbox` mailbox; its local folder remains
`JMAP/Inbox`. Full query filters inbox membership and excludes reserved settings.
Incremental gets recheck membership: messages moved out or into settings are
removed locally, subject to pending-edit protection. Settings never enter FTS.
Exactly one inbox-role mailbox is required. Other user mailboxes aren't indexed.

Limits: query pages 256, get batches 64, body values capped at 1 MiB per part;
truncated/encoding-error bodies fail without advancing the cursor. Servers with
smaller method limits need smaller batches. Cursor expiry requires explicit
`reset_cursor`. No push, automatic OAuth refresh, submission, or move API.
Full downloads cache original MIME and attachments when downloadUrl exists;
otherwise reconstructed MIME contains from/to/cc, subject, date, Message-ID,
text/HTML only, without attachments. HTML-only FTS content remains HTML.
Missing settings download/upload support and unconfirmed mailbox creation/import
return explicit errors; parent must preserve queued operations on failure.
JSON/session/upload-response bodies cap at 32 MiB, MIME download/upload at 64 MiB.
Limits apply while streaming, even without Content-Length. Settings returns all
messages; parent owns snapshot retention/compaction. SMTP remains parent-owned.
Provider binding includes API URL and remote account ID; switching either needs
a separate local account. Mapping tombstones retain stable UIDs.

Validation: `cargo test -p munki-jmap` after parent registers the workspace member.
Mock HTTP tests assert auth, discovery path, JMAP request bodies, incremental
cursor replay, stable IDs/no duplicates, pending edits/deletions, MIME fallback,
original download caching, keyword patching, redirect/cross-origin rejection,
settings exclusion (even if also in inbox), lazy mailbox creation, MIME upload,
import confirmation/failure, idempotent append replay, settings downloads,
Content-Length and chunked response limits.
