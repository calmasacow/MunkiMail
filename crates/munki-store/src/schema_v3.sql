CREATE TABLE drafts (
    id INTEGER PRIMARY KEY,
    account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'draft' CHECK(state IN ('draft','queued','delivered','sent')),
    wire BLOB,
    message_id TEXT,
    sent_folder TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
