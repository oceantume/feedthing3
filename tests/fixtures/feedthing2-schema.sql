create table feeds (
  id integer primary key autoincrement,
  url text not null unique,
  name text not null,
  poll_interval_minutes integer not null default 15,
  retention_days integer,
  fetch_full_content integer not null default 0,
  enabled integer not null default 1,
  next_poll_at integer,
  last_polled_at integer,
  last_error text,
  created_at integer not null
);

create table items (
  id integer primary key autoincrement,
  feed_id integer not null references feeds (id) on delete cascade,
  guid text not null,
  url text,
  title text,
  summary text,
  full_content text,
  full_content_fetched_at integer,
  full_content_error text,
  published_at integer,
  author text,
  ai_description text,
  ai_tags text,
  ai_input_tokens integer,
  ai_output_tokens integer,
  created_at integer not null,
  unique (feed_id, guid)
);

create index items_feed_id_idx on items (feed_id);
create index items_published_at_idx on items (published_at);

create table push_subscriptions (
  id integer primary key autoincrement,
  endpoint text not null unique,
  p256dh text not null,
  auth text not null,
  created_at integer not null
);
