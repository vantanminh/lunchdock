# Compose stacks and templates

A compose stack is a service of kind `compose` that runs one `docker compose` project on a server.

## Creating a stack

- **Paste YAML**: Projects → choose an environment → Service → kind *compose stack*, then paste the file. API: `POST /api/v1/environments/{id}/stacks` with `source: "inline"` and `compose_yaml`.
- **Git**: choose *Git repository*, a Git provider, the repository, branch, and the compose file path inside it. The repository is cloned on the first deploy into `<data dir>/stacks/<project>/src` and updated with `git fetch` + `git reset --hard` afterwards, so untracked files such as `./data` bind mounts survive. Git stacks run on the local server only for now.
- **Template**: Templates → pick an app → fill in the domain and any options → Deploy.

Set the *main service* to the compose service that serves HTTP. New domains target it unless you choose another compose service in the domain form.

## What Rudder changes in your file

The original file is stored unchanged. The generated file, shown on the stack's General tab and written to `stacks/<project>/rudder-compose.yml`, differs as follows:

- the top-level `name` becomes the project name (`<service name>-<6 hex>`) and `version` is dropped;
- `container_name` values gain the `<project>-` prefix;
- every service gets `rudder.managed`, `rudder.project`, and `rudder.service` labels;
- the main service and every service with a domain join the external `rudder-proxy` network next to their own networks.

Traefik routes are written by the panel's file provider (see decision 0011), not by Docker labels, so you do not need to add Traefik labels. Routed services must not use `network_mode`. Published host ports and `replicas` produce warnings.

## Environment

Reference values as `${NAME}` (or `${NAME:-default}`) in the compose file and set them on the Environment tab. They are stored encrypted and written to a mode 0600 `.env` next to the generated file each time the stack is deployed; values are quoted so compose does not expand `$` inside them. The stack page lists referenced variables that have no value yet. Use `$$` for a literal dollar sign in the compose file. Container and compose output is masked with these values before it is stored or shown.

## Actions

| Action | Command |
| --- | --- |
| Deploy | `docker compose -p <project> -f rudder-compose.yml up -d --remove-orphans` |
| Restart | `… restart` |
| Pull images | `… pull` |
| Stop | `… down` |
| Delete | `… down` (or `down --volumes --remove-orphans` when you also delete volumes), then the `.env` is emptied and the service removed |

Actions run in the background and appear under Deployments. Only one runs per stack at a time. Logs for each compose service are on the Logs tab.

## Templates

Templates live in [`templates/`](../templates) and are compiled into the binary. Shipped: Ghost, Gitea, Metabase, MinIO (community build `pgsty/minio`), n8n, Plausible, RustFS, Umami, Uptime Kuma, Vaultwarden, WordPress + MariaDB.

A template is YAML:

```yaml
slug: demo              # file must be demo.yml
name: Demo
description: One line.
category: Testing
tags: [example]
website: https://example.com
main_service: web       # receives the domain
port: 80                # container port the domain routes to
notes: Shown after deploy.
variables:
  - name: DOMAIN
    kind: domain        # domain, string, email, secret, number, boolean
  - name: SECRET_KEY
    kind: secret        # generated when empty
    length: 48
compose: |
  services:
    web:
      image: nginx:1.27
      environment:
        URL: https://${DOMAIN}
        KEY: ${SECRET_KEY}
```

Every `${VAR}` must be declared. `cargo test -p rudder-server templates` checks each file, and `RUDDER_DOCKER_TESTS=1 cargo test -p rudder-server --test stacks` also runs `docker compose config` on each. Generated secrets are returned once by the deploy call and shown on screen; afterwards they are only in the encrypted environment.
