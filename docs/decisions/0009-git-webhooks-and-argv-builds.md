---
created_at: "2026-10-11T03:31:49.228764956+00:00"
doc: docs/decisions/0009-git-webhooks-and-argv-builds.md
id: "0009"
notes: null
status: accepted
title: Git webhooks and argv-only builds
type: decision
updated_at: "2026-10-11T03:31:49.228767466+00:00"
verify: null
---

# Git webhooks and argv-only builds

Date: 2026-10-11

## Status

Accepted

## Context

Git apps need private clones, push-to-deploy, and three build strategies. The build must not assemble a shell command from the repository URL, branch, or Dockerfile path. Extra servers do not have an agent yet, so a remote job cannot be left running forever.

## Decision

GitHub webhooks are checked with HMAC-SHA256 over the raw body (`X-Hub-Signature-256`). GitLab webhooks are checked with a constant-time compare of `X-Gitlab-Token` to the per-service secret. A push deploys only when `ref` is `refs/heads/{branch}`. `ping` is accepted and does not deploy.

`git clone`, `docker build`, and `nixpacks build` are argument vectors. Relative build paths reject `..`, absolute paths, and shell metacharacters. A static site build writes an nginx Dockerfile and builds that. HTTPS credentials are written to a mode `0600` file and selected with `GIT_CONFIG_*`, not placed on the command line. SSH uses `GIT_SSH_COMMAND` only when the key path matches `[A-Za-z0-9._/-]`. Captured output is redacted before it is stored.

The queue claims with `FOR UPDATE SKIP LOCKED` and starts a job only when that server's running count is below `panel_settings.build_concurrency_default` (default 1). Cancelling a queued job is immediate. A running job stops between steps, and dropping the wait kills the child. Boot marks `running` builds failed and leaves `queued` builds in place.

A locally built image is rolled out with `skip_pull` so Docker does not try to pull it. GitHub App credentials mint an RS256 JWT and exchange it for an installation token; a personal access token is the simpler path. Until the agent exists, a build for a remote server fails with a clear error instead of blocking the queue.

## Consequences

The panel image needs `git`, the Docker CLI, and `nixpacks` on the target machine. A data directory path that contains spaces or shell characters cannot be used for an SSH key. Installation tokens and provider secrets are not returned by the API and are not written to deployment logs.
