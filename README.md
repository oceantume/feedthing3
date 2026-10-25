# Feedthing

A personal feed reader. It polls feeds in the background, keeps their items for as long
as you ask, summarises and tags new items with AI, and can send a push notification when
a feed has something new.

## Running it

```sh
cargo run
```

It listens on http://127.0.0.1:3000 and keeps its data in `feedthing.db`. Migrations run
on startup.

| Variable | Default | |
|---|---|---|
| `BIND` | `127.0.0.1:3000` | Address to listen on. |
| `DATABASE_URL` | `sqlite://feedthing.db` | |
| `FETCH_TIMEOUT_MS` | `10000` | For each call to a feed or an article page. |
| `TZ` | `America/Toronto` | Where "today" is, for showing times. |
| `ANTHROPIC_API_KEY` | | Turns on AI summaries of new items, and the backfill button. |
| `AI_MODEL` | `claude-opus-5-5` | Must support structured outputs. |
| `AI_SYSTEM_PROMPT` | see `src/enrich.rs` | Replaces the instructions for summaries and tags. |
| `RUST_LOG` | `feedthing=debug,tower_http=info` | |

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Layout

- `src/main.rs`: startup, shutdown.
- `src/config.rs`: settings from the environment.
- `src/http_client.rs`: the client for every outgoing call.
- `src/feed.rs`, `src/item.rs`: feeds and their items, and the SQL for them.
- `src/parse.rs`: reads RSS 2.0, RSS 1.0, Atom and JSON Feed on top of quick-xml, with
  dates read the forgiving way feeds need.
- `src/content.rs`: sanitises feed HTML, and cuts an article page down to the article.
- `src/enrich.rs`: AI descriptions and tags, from Anthropic's Messages API over plain HTTP.
- `src/poller.rs`: polls one feed, with conditional GET; `src/scheduler.rs` polls what's
  due every 30 seconds and purges expired items daily.
- `src/web/`: routes, pages and the plumbing they share. `layout` is the page shell,
  `error` gives errors a page, `csrf` turns away cross-site writes, `assets` serves
  `static/` with content-hashed URLs, `health` answers `/healthz`.
- `migrations/`: SQL, compiled into the binary.
- `static/`: CSS, icons, and the browser scripts.
