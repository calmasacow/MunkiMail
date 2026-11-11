-- v2: Screener pre-sort, and per-folder UID high-water mark.

-- The heuristic's guess for a message still in the Screener, with its
-- reason. Shown in the queue row so approving is one keypress, and so
-- the guess is explainable rather than magic.
ALTER TABLE messages ADD COLUMN suggested TEXT;
ALTER TABLE messages ADD COLUMN suggested_reason TEXT;

-- Highest UID ingested. Fetch "UID n+1:*" next time instead of
-- re-listing the folder. Reset when uidvalidity changes.
ALTER TABLE folders ADD COLUMN last_uid INTEGER NOT NULL DEFAULT 0;
