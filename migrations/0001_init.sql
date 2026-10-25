-- Same shape as feedthing2's schema, so `feedthing import` copies rows as
-- they are. Timestamps are epoch milliseconds; booleans are 0/1.

CREATE TABLE feeds (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  url TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  poll_interval_minutes INTEGER NOT NULL DEFAULT 15,
  retention_days INTEGER,
  fetch_full_content INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  next_poll_at INTEGER,
  last_polled_at INTEGER,
  last_error TEXT,
  -- Sent back on the next poll, so an unchanged feed answers 304.
  etag TEXT,
  last_modified TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  feed_id INTEGER NOT NULL REFERENCES feeds (id) ON DELETE CASCADE,
  -- The feed's guid, or the item's URL when it has none.
  guid TEXT NOT NULL,
  url TEXT,
  title TEXT,
  summary TEXT,
  full_content TEXT,
  full_content_fetched_at INTEGER,
  full_content_error TEXT,
  published_at INTEGER,
  author TEXT,
  ai_description TEXT,
  ai_tags TEXT CHECK (ai_tags IS NULL OR json_valid(ai_tags)),
  ai_input_tokens INTEGER,
  ai_output_tokens INTEGER,
  created_at INTEGER NOT NULL,
  UNIQUE (feed_id, guid)
);

CREATE INDEX items_feed_id_idx ON items (feed_id);
CREATE INDEX items_published_at_idx ON items (published_at);

CREATE TABLE push_subscriptions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  endpoint TEXT NOT NULL UNIQUE,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
