# Command line client

`rudder` is a single static binary, `rudder-x86_64-unknown-linux-musl` or `rudder-aarch64-unknown-linux-musl` on each release. It talks to the panel's REST API with a scoped API token.

## Log in

Create a token on the **API tokens** page. `deploy` and `cache purge` need the `write` scope; `services` and `logs` work with `read`. Then run:

```sh
rudder login --url https://panel.example.com
# API token: rdr_...
```

The token is read from `RUDDER_TOKEN` or from stdin, never from an argument, so it stays out of shell history and `ps`. `login` checks the token with `GET /api/v1/me` before saving anything. It saves the URL and token to `~/.config/rudder/config.json` with mode 0600 (the directory gets 0700). You can change the location with `RUDDER_CONFIG` or `XDG_CONFIG_HOME`.

`RUDDER_URL` and `RUDDER_TOKEN` override the saved values for one command, which is how CI uses it:

```sh
RUDDER_URL=https://panel.example.com RUDDER_TOKEN=$RUDDER_DEPLOY_TOKEN rudder deploy shop/web
```

`rudder logout` deletes the saved file. The token stays valid until you revoke it in the panel.

## Choosing a service

Commands take a service as its id, its name, `project/name`, or `project/environment/name`. When a name matches more than one service, the CLI lists the matches and asks for a longer form. `rudder services` prints every service:

```text
ID                                    NAME            KIND         STATUS
6c1f…                                 shop/prod/web   application  running
91aa…                                 shop/prod/blog  compose      running
```

## Deploy

```sh
rudder deploy shop/web                                     # redeploy the current image, or build from Git
rudder deploy shop/web --image ghcr.io/acme/web:1.4.2       # deploy a specific image
rudder deploy shop/prod/blog                               # docker compose up for a stack
rudder deploy shop/web --no-wait                           # queue and return
```

- **Applications with a Git source** get a build (`POST /services/{id}/build`). The CLI follows the deployment, printing its log as it grows, until it finishes.
- **Image applications** deploy synchronously (`POST /services/{id}/deploy`), and the log is printed at the end.
- **Compose stacks** run `up` (`POST /services/{id}/stack/up`) and are followed the same way as builds.
- **Databases** are not deployed; start or restart them from the panel.

The exit status is 0 only when the deployment `succeeded`. `--timeout SECS` (default 1800) limits how long the CLI waits.

## Logs

```sh
rudder logs shop/web                       # last 100 lines, oldest first
rudder logs shop/web -n 500 --since 2h
rudder logs shop/web --level error,warn --source app --grep timeout
rudder logs shop/web -f                    # keep following
```

The lines come from the panel's central log store (`GET /api/v1/logs`), so they include app, proxy, and cache lines for the service. `--since` takes `30s`, `15m`, `2h`, `7d`, or an RFC 3339 time. `-f` polls every 2 seconds and prints lines it has not shown yet.

## Purge the cache

```sh
rudder cache purge shop/web --all
rudder cache purge shop/web --path /index.html --path /app.css
rudder cache purge shop/web --prefix /blog/ [--domain <domain id>]
```

This purges every cache-enabled domain of the service, or only `--domain`. The CLI prints how many objects were removed.

## Decrypt a backup

```sh
RUDDER_BACKUP_PASSPHRASE=... rudder backup decrypt --in db.dump.zst.enc --out db.dump.zst
```

This works offline and needs no panel (see [backups](backups.md)).
