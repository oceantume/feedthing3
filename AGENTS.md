# Feedthing

A personal feed reader: it polls RSS/Atom/JSON feeds, keeps their items, summarises and
tags them with AI, and sends a push notification when something new comes in. One person
uses it, over Tailscale. See `README.md` for running it and the layout.

It replaces feedthing2 (`../feedthing2`, Remix on Node), whose behaviour it ports; the
data is brought over with `feedthing import` (see `docs/deploy.md`).

## Principles

- **Simplicity first.** Prefer small code we own over a dependency or an abstraction.
  Build what's needed now, not what might be needed later.
- **Measure a crate before adding it.** Check what it brings in with `cargo tree` against
  the current lockfile. Prefer a small module of our own when a crate pulls in many
  others, a native library, or has a stale upstream.
- **Crypto is built on `aws-lc-rs`**, already a dependency: ECDSA, ECDH, AES-GCM, HKDF,
  SHA-2, random numbers. Don't add other crypto crates. Web Push is written this way
  (`src/push.rs`).
- **The exceptions are earned:** the HTML sanitiser (`ammonia`), which is
  security-critical, and time zones (`jiff`), which are easy to get subtly wrong.
- **The server renders HTML.** Views are maud functions next to their feature's handlers.
  Every flow is a real `<form>` or link with post/redirect/get, and works without JS. The
  only scripts are the notification bell and the service worker, since push can't work
  without them.
- **Handler files read top-down.** `routes()` comes first, listing routes in the order
  people go through them. Handlers follow in that same order, each doc comment starting
  with its route (`` /// `POST /feeds/{id}` ``). A form struct sits right above the
  handler that reads it.
- **Forms and URL params, not JSON.** Writes are urlencoded form posts, reads use query
  params. The push routes take JSON because that's what the browser's push API hands us.
- **Domain vs. web.** Top-level modules (`feed`, `item`, `poller`, `enrich`, `push`, …) own
  their data, queries and rules, and know nothing about HTTP. `web/` turns requests into
  domain calls and renders the results; it never writes SQL.
- **Timestamps are epoch milliseconds** in INTEGER columns, as in feedthing2. `jiff` is
  only for what std can't do: local dates for display, and parsing feed dates.
- **Logs are for what happened**, not for no-ops: a poll tick logs its counts, a purge
  logs only when it deleted something.

## Guardrails

- Keep the CSRF guard (`src/web/csrf.rs`). There's no sign-in, so it's the only thing
  stopping another site from posting a delete.
- Sanitise every piece of feed HTML through `content::sanitize` before it's shown.
- Migrations only move forward and must work with the previous release: add columns and
  tables first; remove what old code reads only in a later release.

## Checks

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must
  pass.
- `cargo build` before running the app: `clippy` and `test` don't rebuild the binary.
