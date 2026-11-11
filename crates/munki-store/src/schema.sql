-- MunkiMail local store, schema v1.
--
-- SQLite is authoritative locally; the server is a replica we reconcile
-- against. Anything the user can change has a row here first.

CREATE TABLE accounts (
    id          INTEGER PRIMARY KEY,
    email       TEXT NOT NULL UNIQUE,
    display     TEXT NOT NULL DEFAULT '',
    -- Transport discriminator: 'imap' or 'jmap'. Credentials never land
    -- here; they live in the OS keyring, keyed by this id.
    kind        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

-- Per-folder sync cursors. (uidvalidity, highestmodseq) is what lets us
-- resync incrementally via CONDSTORE/QRESYNC instead of refetching.
CREATE TABLE folders (
    id             INTEGER PRIMARY KEY,
    account        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    uidvalidity    INTEGER,
    highestmodseq  INTEGER,
    UNIQUE (account, name)
);

CREATE TABLE messages (
    id               INTEGER PRIMARY KEY,
    account          INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder           TEXT NOT NULL,
    uid              INTEGER NOT NULL,
    message_id       TEXT NOT NULL,
    thread_id        TEXT NOT NULL,
    from_addr        TEXT NOT NULL,
    subject          TEXT NOT NULL DEFAULT '',
    -- User's replacement for an obscure subject line. NULL = use theirs.
    renamed_subject  TEXT,
    date             INTEGER NOT NULL,
    seen             INTEGER NOT NULL DEFAULT 0,
    has_attachment   INTEGER NOT NULL DEFAULT 0,
    destination      TEXT NOT NULL,
    -- Server identity is (account, folder, uid); that is the real key.
    UNIQUE (account, folder, uid)
);

CREATE INDEX messages_by_view   ON messages (account, destination, date DESC);
CREATE INDEX messages_by_thread ON messages (account, thread_id, date);
CREATE INDEX messages_by_sender ON messages (account, from_addr, date DESC);
CREATE INDEX messages_by_msgid  ON messages (account, message_id);

-- Standing per-sender decisions from the Screener. Dismissed senders are
-- filtered from views but never deleted, so search still finds them.
CREATE TABLE senders (
    account      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    address      TEXT NOT NULL,
    -- 'allow' or 'dismiss'
    verdict      TEXT NOT NULL,
    -- Destination to route to when verdict='allow'; NULL when dismissed.
    destination  TEXT,
    decided_at   INTEGER NOT NULL,
    PRIMARY KEY (account, address)
) WITHOUT ROWID;

-- Full-text index. External-content table: `messages` owns the metadata,
-- this owns the text, and `docid` is messages.id. Body text is kept out
-- of `messages` so list queries never touch it.
CREATE VIRTUAL TABLE messages_fts USING fts5 (
    subject,
    from_addr,
    body,
    attachment_names,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Pending actions, applied locally and replayed against the server.
CREATE TABLE outbox (
    id          INTEGER PRIMARY KEY,
    account     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    not_before  INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    -- Claimed by a drainer and in flight. Guards non-idempotent sends
    -- from being replayed after a crash without human review.
    leased_at   INTEGER,
    created_at  INTEGER NOT NULL
);

CREATE INDEX outbox_ready ON outbox (account, not_before) WHERE leased_at IS NULL;
