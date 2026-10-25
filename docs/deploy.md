# Deploying

The app runs on the box, reachable only over the tailnet at
`https://box.<tailnet>.ts.net:8421`. It runs in two slots behind Caddy: one serves, the other
keeps the previous release for rollback. `tailscale serve` terminates HTTPS with the box's
Tailscale certificate and hands requests to Caddy on `127.0.0.1:19921`, like Netdata's
dashboard. HTTPS isn't optional: browsers only allow service workers, and so push
notifications, on it. The SQLite database is in `/var/lib/apps/feedthing` on the box, mounted
at `/data`.

The box itself (Flatcar, Tailscale, Caddy, the slot machinery) is set up from the `infra`
repo; this repo holds what's specific to Feedthing:

- `Dockerfile`: the image.
- `deploy/app.conf`: how the box runs it (port, slot ports 4021/4022, health check, memory).
- `deploy/site.caddy`: its site in Caddy, on 127.0.0.1 only.
- `deploy/env`: settings that aren't secret; `deploy/secrets.enc.yaml`: the secrets (sops).
- `bin/deploy`, `bin/rollback`, `bin/status`: copies of `infra/app-kit/bin/`.

- [Before the first deploy](#before-the-first-deploy)
- [Secrets](#secrets)
- [Deploying](#deploying)
- [Logs](#logs)
- [Backups](#backups)
- [Moving from Fly](#moving-from-fly)

## Before the first deploy

You need Docker, [sops](https://github.com/getsops/sops), and your machine on the tailnet.
Your key, `~/.ssh/id_ed25519_infra`, logs into the box and decrypts the secrets: load it in
your SSH agent (`ssh-add ~/.ssh/id_ed25519_infra`) and point sops at it:

```sh
export SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/id_ed25519_infra
```

The box publishes the site to the tailnet; see "Tailnet-only apps" in the infra repo's
`docs/operations.md`. On a box installed before that was added, once:

```sh
ssh core@box sudo tailscale serve --bg --https=8421 http://127.0.0.1:19921
```

## Secrets

`deploy/secrets.enc.yaml` holds the values, encrypted: names are readable, values aren't.
All are optional; the app runs without them, minus the feature each turns on.

```yaml
ANTHROPIC_API_KEY: sk-ant-...   # AI summaries
VAPID_PUBLIC_KEY: B...          # push notifications: both keys, base64url
VAPID_PRIVATE_KEY: ...
VAPID_SUBJECT: mailto:...       # how push services can reach you
```

Edit them with `sops deploy/secrets.enc.yaml`, commit, and deploy. Keep the VAPID keys:
browsers subscribed with them, and new keys mean subscribing every browser again.

## Deploying

```sh
bin/deploy      # the last commit, live once healthy
bin/rollback    # back to the previous release, in seconds
bin/status      # the two slots: which is live, their commits, their health
```

`bin/deploy` builds the image from the working tree, which must be committed: the image is
tagged with the commit. It ships the image over SSH and has the box start it in the idle
slot. Once that slot answers `/healthz`, traffic moves to it and the old one stops. The
scheduler runs in whichever slot is live; a poll the old slot had in progress is dropped,
and the feed is polled again on its next turn.

- **Migrations must work with the previous version.** The new release runs them at startup,
  while the old one still serves on the same database, and a rollback runs the old code on
  the migrated database. Add columns and tables first; remove what old code reads only in a
  later release.
- **Both slots poll for a moment** during a switch. That's safe: a feed is claimed before
  it's polled, so only one of them polls it.

## Logs

```sh
ssh core@box journalctl -t feedthing.a -f           # the app, slot a (or .b: see bin/status)
ssh core@box journalctl -u app@feedthing.a          # its starts, stops, crashes
ssh core@box tail -f /var/log/caddy/feedthing.log   # requests, as JSON
```

Each poll tick logs one line with how many feeds it checked, what came in, and what failed.

## Backups

There are none yet: the database exists only on the box, until the infra repo adds
Litestream. Take a copy by hand with SQLite's `.backup`, which is safe while the app writes:

```sh
ssh core@box docker run --rm --user 65532 -v /var/lib/apps/feedthing:/data keinos/sqlite3:3.54.1 \
  sqlite3 /data/feedthing.db "'.backup /data/copy.db'"
ssh core@box sudo cat /var/lib/apps/feedthing/copy.db > prod-copy.db
ssh core@box sudo rm /var/lib/apps/feedthing/copy.db
DATABASE_URL=sqlite://prod-copy.db cargo run
```

## Moving from Fly

One time, to replace feedthing2 (Fly app `feedthing`) with this app on the box. feedthing2
keeps running on Fly until the last step.

1. **Copy the secrets** from the running Fly machine without showing them (Fly only lists
   names). Keeping the VAPID keys means the browsers feedthing2 subscribed keep getting
   notifications. `AI_MODEL` stays behind on purpose: feedthing2's may not support the
   structured outputs summaries now use, and the default is current.

   ```sh
   fly ssh console -a feedthing -C printenv 2>/dev/null | tr -d '\r' \
     | grep -E '^(ANTHROPIC_API_KEY|VAPID_PUBLIC_KEY|VAPID_PRIVATE_KEY|VAPID_SUBJECT)=' \
     | sed -E 's/^([A-Z_]+)=(.*)$/\1: "\2"/' \
     | sops encrypt --filename-override deploy/secrets.enc.yaml \
         --input-type yaml --output-type yaml /dev/stdin > deploy/secrets.enc.yaml
   grep -E '^[A-Z]' deploy/secrets.enc.yaml | cut -c1-40   # names, values ENC[...]
   git add deploy/secrets.enc.yaml && git commit -m "Add the secrets from Fly"
   ```

2. **Deploy to the box** (`bin/deploy`) and check it on its own, before moving any data: open
   `https://box.<tailnet>.ts.net:8421`, add a feed, see it polled.

3. **Copy and import the database.** Items that come in on Fly after the copy are fetched
   again by the box's first polls, as long as they're still in their feeds. feedthing2's
   image has Node but no `sqlite3`; `VACUUM INTO` makes a consistent copy while it runs.
   Check first that `/data/app.db` is the file in use (feedthing2's `DATABASE_PATH`; the
   older Prisma app's `feedthing.db` may sit next to it).

   ```sh
   fly ssh console -a feedthing -C "ls -la /data"
   fly ssh console -a feedthing -C \
     "node -e \"new (require('node:sqlite').DatabaseSync)('/data/app.db').exec(\\\"VACUUM INTO '/tmp/copy.db'\\\")\""
   fly ssh sftp get -a feedthing /tmp/copy.db feedthing2-prod.db
   fly scale count 0 -a feedthing

   rm -f feedthing.db*
   DATABASE_URL=sqlite://feedthing.db cargo run --release -- import feedthing2-prod.db

   ssh core@box 'sudo systemctl stop app@feedthing.$(cat /etc/apps/feedthing/active)'
   ssh core@box 'sudo sh -c "rm -f /var/lib/apps/feedthing/feedthing.db* && cat > /var/lib/apps/feedthing/feedthing.db && chown 65532:65532 /var/lib/apps/feedthing/feedthing.db"' < feedthing.db
   ssh core@box 'sudo systemctl start app@feedthing.$(cat /etc/apps/feedthing/active)'
   bin/status
   ```

4. **Subscribe each browser again** from the bell at the box's address. Until then, the
   subscriptions copied from feedthing2 keep notifying through feedthing2's service worker
   (same VAPID keys), but tapping one that isn't about an item opens feedthing2's dead
   address. Once every browser has subscribed here, drop the copied ones, or each
   notification comes twice:

   ```sh
   ssh core@box docker run --rm --user 65532 -v /var/lib/apps/feedthing:/data keinos/sqlite3:3.54.1 \
     sqlite3 /data/feedthing.db "'DELETE FROM push_subscriptions WHERE id <= <last imported id>'"
   ```

   `<last imported id>` is the highest id right after step 3's import, before anything
   subscribed here: `sqlite3 feedthing.db 'SELECT max(id) FROM push_subscriptions'` on the
   local copy.

5. **Retire Fly** once everything works on the box: keep the volume a while as a backup
   (`fly scale count 0` already stopped the app), then `fly apps destroy feedthing`.
