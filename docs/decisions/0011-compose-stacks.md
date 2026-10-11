---
created_at: "2026-10-11T05:11:48.774643790+00:00"
doc: docs/decisions/0011-compose-stacks.md
id: "0011"
notes: null
status: accepted
title: Compose stacks routed by the file provider
type: decision
updated_at: "2026-10-11T05:11:48.774644427+00:00"
verify: null
---

# Compose stacks routed by the file provider
Date: 2026-10-11

## Status

Accepted

## Context

Users need to run multi-container apps from a compose file they paste or keep in Git, and one-click templates for common self-hosted apps. Stacks must share the panel's Traefik instance and later the Pingora cache, must not collide with each other on one Docker host, and must run on remote servers through the same `NodeDriver` boundary. Secrets entered in the panel must not end up in the stored compose file, the generated file, or logs.

## Decision

A stack is one `services` row with `kind = 'compose'`. Its spec keeps the source YAML as written, the generated YAML, the compose project name (service name plus six hex characters), the service list with container names, and the path of the generated file relative to the node data directory.

`rudder_core::transform_compose` parses the file with `serde_yaml_ng`, resolves merge keys, sets the top-level `name` to the project, drops `version`, prefixes custom `container_name` values with the project, adds `rudder.managed`/`rudder.project`/`rudder.service` labels, and attaches routed services to the external `rudder-proxy` network. Routed services are the stack's main service plus every compose service a domain points at (`domains.compose_service`). Adding a domain for a new service re-runs the transform and re-applies a running stack.

Routing does not use Traefik Docker labels. The panel already writes Traefik's file provider (decision 0008), and the M8 cache sits in front of the same routes, so stack domains are rendered there too, targeting `{project}-{service}-1` or the prefixed container name. One routing path covers applications, databases, and stacks, and remote servers need no Docker provider.

Environment variables from the Environment tab are decrypted only when an action runs and written as a mode 0600 `.env` next to the generated file; compose interpolates `${NAME}` from it. Values are single-quoted, or double-quoted with `"`, `\`, and `$` escaped when they contain a single quote, so compose never re-interpolates them. Logs from compose and from containers are redacted with those values.

Actions (`up -d --remove-orphans`, `down`, `restart`, `pull`, and `down --volumes --remove-orphans` on delete) run in the background against a `deployments` row; the service row moves to `deploying` atomically so only one action runs per stack. `docker compose` is invoked with an argv only, through `NodeDriver::compose`. Git stacks are cloned or fast-forwarded (`fetch` + `reset --hard`, keeping untracked bind-mount data) into `stacks/{project}/src` with the same credential handling as builds; they run on the local server only until the agent can clone.

Templates are YAML files in `templates/` embedded with rust-embed. Each declares typed variables (domain, string, email, secret, number, boolean); a unit test checks that every `${VAR}` is declared and that the file transforms with its main service routed. Deploying a template creates the stack, stores the variables encrypted, routes the domain to the main service and port, and returns generated secrets once.

## Consequences

Routed stack services share the `rudder-proxy` network, so their compose service names are visible to other routed containers on that network; a service that is both routed and resolves a common name such as `db` can, in rare cases, see another stack's routed `db`. Unrouted services stay on the stack's private network. Compose features that conflict with the proxy network (`network_mode` on a routed service) are rejected; host port publishing and replicas only produce warnings. Templates pin image tags that existed when they were added and need periodic bumps. Git-sourced stacks on remote servers are deferred to the agent milestone.
