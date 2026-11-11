-- Desktop local-view policy, independent of server folders/labels.
CREATE TABLE store_preferences(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE server_folders(
 account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
 name TEXT NOT NULL, delimiter TEXT, selectable INTEGER NOT NULL,
 PRIMARY KEY(account,name)
);
-- Enforce local metadata at the queue boundary, including transactional writers.
CREATE TRIGGER local_view_metadata BEFORE INSERT ON outbox
WHEN coalesce((SELECT value FROM store_preferences WHERE key='local_views'),0)=1
 AND json_extract(NEW.kind,'$.kind') IN ('set_destination','set_stack','put_state','move')
BEGIN SELECT RAISE(IGNORE); END;

CREATE INDEX messages_by_folder ON messages(account,folder,date DESC,id DESC);
