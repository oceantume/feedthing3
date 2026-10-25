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
- `src/web/`: routes, pages and the plumbing they share. `layout` is the page shell,
  `error` gives errors a page, `csrf` turns away cross-site writes, `assets` serves
  `static/` with content-hashed URLs, `health` answers `/healthz`.
- `migrations/`: SQL, compiled into the binary.
- `static/`: CSS, icons, and the browser scripts.
