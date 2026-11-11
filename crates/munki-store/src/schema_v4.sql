CREATE TABLE local_device (id TEXT NOT NULL);
INSERT INTO local_device VALUES (lower(hex(randomblob(16))));
CREATE TABLE triage_entries (
    account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('sender','message')),
    key TEXT NOT NULL,
    revision INTEGER NOT NULL,
    device TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY(account,kind,key)
) WITHOUT ROWID;
ALTER TABLE messages ADD COLUMN original_thread_id TEXT NOT NULL DEFAULT '';
UPDATE messages SET original_thread_id=thread_id;
ALTER TABLE messages ADD COLUMN reply_later INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN set_aside INTEGER NOT NULL DEFAULT 0;
-- Preserve decisions and renamed subjects made before state sync existed.
INSERT INTO triage_entries
SELECT account,'sender',address,decided_at * 1000000,(SELECT id FROM local_device),
    json_object('type','sender','data',json(CASE WHEN verdict='dismiss' THEN '"dismiss"' ELSE json_object('allow',destination) END))
FROM senders;
INSERT INTO triage_entries
SELECT account,'message',message_id,unixepoch() * 1000000,(SELECT id FROM local_device),
    json_object('type','message','data',json_object('subject',renamed_subject,'thread_id',NULL,'reply_later',json('false'),'set_aside',json('false')))
FROM messages WHERE renamed_subject IS NOT NULL GROUP BY account,message_id;
