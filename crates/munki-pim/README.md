# munki-pim

Local-first calendar/contact library. No UI, account access, SMTP, credential persistence, or database opening.

## Integration

```rust,ignore
let pim = munki_pim::Store::new(backend.store.conn())?;
```

`Store<'a>` borrows the application's **already-keyed** `rusqlite::Connection`. All PIM tables, indexes, email identities and conflict bodies live in that same SQLite/SQLCipher database. It never issues `PRAGMA key`, opens another database, or changes journal or busy-handler settings. Initialization enables foreign keys and creates namespaced `pim_*` tables. Parent retains connection locking and encryption ownership. Call commands outside an existing transaction: mutating multi-table operations create their own transactions. Exception: `save_raw_in_transaction` requires an active caller-owned transaction. Initialize PIM before starting that transaction. Roll back the entire caller transaction on any failure.

Use rusqlite **0.37.0**, matching munki-store. No new root workspace dependency entries are required. Root registration already supplied by parent. This crate inherits workspace anyhow/serde; dependencies otherwise remain crate-local: reqwest 0.12 (blocking, rustls), quick-xml 0.38, ical 0.11, chrono 0.4, chrono-tz 0.10, uuid 1. Dev dependency: tempfile 3.24. No independent SQLite bundling/encryption feature; parent selects SQLite/SQLCipher build features.

## Public types

All result signatures below use `anyhow::Result`. Persisted/domain types derive serde Serialize/Deserialize; Resolution is a caller-side enum.

```rust
Kind::{Calendar, AddressBook}
EventTime::{Date(String), Utc(i64)} // YYYY-MM-DD; Unix seconds
Event { uid, summary, description, location: String,
        start, end: EventTime, organizer: Option<String>,
        attendees: Vec<String>, recurring: bool }
Contact { uid, name: String, emails, phones: Vec<String>, organization, note: String }
Collection { id, name: String, kind: Kind, url: Option<String> }
Record { id, collection_id, uid, raw: String, href, etag: Option<String>,
         dirty, deleted: bool, revision: i64 }
Conflict { record_id: String, remote_raw, remote_etag: Option<String> }
Person { email: String, contacts, events: Vec<Record> }
Resolution::{KeepLocal, AcceptRemote}
SyncReport { downloaded, uploaded, deleted, conflicts: usize }
```

`Event::recurring` is derived, not an editor control. `Date` end dates are exclusive. `Utc` values represent instants; all-day range indexing uses UTC midnight. UIDs are supplied by caller (UUID strings work). Missing vCard UIDs receive UUIDs during import.

## Exact public methods

Every Store method takes `&self` after initialization. IDs mean internal Record/Collection IDs, **not UID**. Creates use `id=None, expected_revision=None`; edits require both ID and the current revision. Save resurrects a tombstone. UID/collection changes are rejected.

```rust
Store::new(db: &rusqlite::Connection) -> Result<Store<'_>>
Store::create_collection(name: &str, kind: Kind, url: Option<&str>) -> Result<Collection>
Store::collections() -> Result<Vec<Collection>>
Store::collection(id: &str) -> Result<Collection>
Store::get(id: &str) -> Result<Record>
Store::records(collection: &str) -> Result<Vec<Record>>
Store::events_in_range(collection: &str, start: i64, end: i64) -> Result<Vec<Record>>
Store::contacts(collection: &str) -> Result<Vec<Record>>
Store::person(email: &str) -> Result<Person>
Store::save_event(collection: &str, id: Option<&str>, expected_revision: Option<i64>, event: &Event) -> Result<Record>
Store::save_contact(collection: &str, id: Option<&str>, expected_revision: Option<i64>, contact: &Contact) -> Result<Record>
Store::save_raw(collection: &str, id: Option<&str>, expected_revision: Option<i64>, raw: &str) -> Result<Record>
Store::save_raw_in_transaction(collection: &str, id: Option<&str>, expected_revision: Option<i64>, raw: &str) -> Result<Record>
Store::delete(id: &str, expected_revision: i64) -> Result<()>
Store::import_ics(collection: &str, raw: &str) -> Result<Vec<Record>>
Store::import_vcf(collection: &str, raw: &str) -> Result<Vec<Record>>
Store::export(id: &str) -> Result<String>
Store::export_collection(collection: &str) -> Result<String>
Store::conflicts(collection: &str) -> Result<Vec<Conflict>>
Store::resolve(id: &str, expected_revision: i64, resolution: Resolution) -> Result<()>
Record::event(&self) -> Result<Event>
Record::contact(&self) -> Result<Contact>
EventTime::timestamp(&self) -> Result<i64>
normalize_email(input: &str) -> Result<String>
parse_event(raw: &str) -> Result<Event>
parse_contact(raw: &str) -> Result<Contact>
rsvp(raw: &str, attendee: &str, status: &str) -> Result<String>
DavClient::new() -> Result<DavClient>
DavClient::basic(self, username: impl Into<String>, password: impl Into<String>) -> DavClient
DavClient::bearer(self, token: impl Into<String>) -> DavClient
DavClient::sync(&self, store: &Store<'_>, collection_id: &str) -> Result<SyncReport>
```

`records` includes tombstones; `contacts`, date queries, person lookup and collection export exclude them. Range queries use half-open overlap `[start,end)` and include zero-duration events whose start lies inside that range. Person normalizes email by trimming, removing `mailto:`, lowercasing; queries every contact email and event organizer/attendee. It does not infer aliases or strip plus tags. Parent supplies message results for its unified person view.

Imports validate before writing, group recurrence overrides by UID, preserve extensions/alarms/timezones and commit atomically. Duplicate UIDs reject the entire import instead of replacing local edits. ICS collection exports contain concatenated complete VCALENDAR objects; VCF exports concatenate VCARDs. Typed edits preserve unmodeled properties and matching email/attendee parameters. Editing modeled phone lists replaces their parameters; structured vCard `N` stays intact on imported contacts (edit through `save_raw` to change it).

`rsvp` accepts case-insensitive `accepted`, `declined`, `tentative`. It requires exactly one VEVENT with UID, DTSTART, ORGANIZER and the selected invited attendee; returns a new METHOD:REPLY resource, updates DTSTAMP, keeps UID/SEQUENCE/time/recurrence identity/timezones, includes only the selected attendee with PARTSTAT and RSVP=FALSE, removes alarms. It does not save, transmit mail, or change the original. Parent builds the SMTP calendar MIME part and sends it to the organizer. `parse_event` expects a recurrence master; an override-only reply can still be generated by `rsvp`.

## DAV safety and limitations

Sync is **blocking**; run on a worker thread. Serialize sync calls per collection. Local writes may occur while HTTP is in flight: revision-aware acknowledgements keep newer edits dirty. Reports describe that invocation; `conflicts()` is the authoritative unresolved list. HTTP/network errors return Err, leaving pending writes dirty; earlier successful operations can remain applied. Retry is safe. A lost PUT response can conservatively surface a conflict on retry.

Supply a concrete CalDAV calendar/CardDAV address-book URL. No principal discovery, collection creation, sync-token REPORT, OAuth refresh, redirect following, or background scheduler. Full Depth:1 PROPFIND scans require successful getetag/resourcetype responses and the collection's own response; malformed or incomplete inventories fail closed. Changed resources are GET-fetched/validated before interpreting absent hrefs as remote deletions. A valid server listing is assumed complete; no DAV inventory can eliminate all concurrent remote scan races.

Existing writes use `If-Match` with a strong ETag; new writes use `If-None-Match: *`. 409/412 creates a persisted remote snapshot without overwriting local edits. A remote edit/deletion against dirty local data also creates a conflict. KeepLocal rebases onto the conflict ETag and queues another conditional write; AcceptRemote explicitly replaces/deletes local data. Subsequent remote changes still conflict. Unresolved conflicts skip upload. Clean remote deletions remove local records; local deletes remain tombstones until acknowledged. No forced unconditional writes. If PUT omits an ETag, a GET must byte-match the uploaded body to acknowledge it; server transformations can therefore produce a conservative conflict.

Cross-origin/out-of-collection hrefs, credentials in URLs, ambiguous encoded paths, response bodies over 16 MiB, missing ETags and weak ETags for writes are rejected. Credentials live only in the DavClient. Basic auth over HTTP is appropriate only for trusted local deployments/tests; use HTTPS for accounts.

Recurrence rules/overrides are preserved, **not expanded**. Range queries index only the master's stored interval. Typed recurring start/end edits are rejected; use a recurrence-aware editor plus `save_raw` for those changes. UTC, DATE and unambiguous IANA TZID values are supported. Floating times, custom VTIMEZONE-only TZIDs, ambiguous/nonexistent DST times and DURATION-based events are rejected rather than guessed. Non-event calendar components are rejected. vCard 3.0/4.0 text properties are supported; legacy vCard 2.1, quoted-printable conversion and binary/photo editing are not implemented. No attachment downloading or invitation scheduling side effects.

## Validation

```sh
cargo test -p munki-pim
cargo clippy -p munki-pim --all-targets -- -D warnings
```

Offline parser/persistence tests and a stdlib loopback mock DAV server cover local edits/revisions, normalized people, ranges, import rollback, preserved recurrence/timezone/extensions, RSVP, conditional headers, 412 races, conflict resolutions, tombstones, remote deletion, malformed inventories, missing PUT ETags and changes made during network requests. No external accounts accessed.
