-- Full MIME remains local and encrypted, including attachments and invites.
CREATE TABLE message_content (
    message INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    raw BLOB NOT NULL
);
