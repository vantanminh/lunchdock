---
created_at: "2026-10-12T02:17:14.028047600+00:00"
doc: docs/decisions/0019-github-app-manifest.md
id: "0019"
notes: null
status: accepted
title: GitHub App is created from a manifest and installed in two redirects
type: decision
updated_at: "2026-10-12T02:17:14.028052800+00:00"
verify: null
---

# GitHub App is created from a manifest and installed in two redirects

Date: 2026-10-12

## Status

Accepted

## Context

A GitHub App is the right way to clone private repositories and receive push events: its token is short-lived, scoped to the repositories the owner chose, and not tied to a person. The panel already mints installation tokens, but the app had to be created by hand. The operator filled in GitHub's form (permissions, events, webhook URL and secret), generated a private key, found the installation id, and pasted three values into the panel. Every step is a chance to get a permission wrong or paste the wrong id.

GitHub's manifest flow removes that: the panel describes the app, GitHub creates it after one confirmation, and hands the panel the app id, private key, and webhook secret.

## Decision

The panel creates and installs the app in two browser round trips.

1. `POST /api/v1/github-app/manifest` returns the manifest and the github.com address for it. The web UI submits it as a form POST to GitHub. The app asks for read access to repository contents, read access to metadata, and the `push` event. It is private (`public: false`).
2. GitHub sends the browser back to `{panel}/git/github?code=&state=`. The web page calls `POST /api/v1/github-app/complete`, which exchanges the one-time code at `POST /app-manifests/{code}/conversions`, checks that the returned key can sign a JWT, and saves the app as a `github_app` git provider with its secrets encrypted. It has no installation yet, so it is `pending_install`. The browser is sent to `https://github.com/apps/{slug}/installations/new`.
3. After the installation GitHub sends the browser to the same page with `installation_id`. `POST /api/v1/github-app/installed` asks GitHub, signed with the app's JWT, for that installation, checks that it belongs to this app, and stores the installation id and account. The provider becomes `ready`. GitHub returns `state` only for a first install started from the panel's link. After an install made on GitHub, or a change to an existing installation, it does not, so without `state` the panel tries each pending app; GitHub's answer to the app-signed request still decides which one the installation belongs to.

Both redirects land on the web page, not on an API endpoint, so the calls that handle secrets are ordinary authenticated requests with the session cookie and CSRF token. There is no unauthenticated callback.

`state` is a blob sealed with the master key (AES-256-GCM) holding the step (`manifest` or `install`), the admin's user id, the provider id, and an expiry one hour out. A forged blob fails to decrypt. A blob from one step is refused at the other. A blob started by one admin is refused for another. The provider id is chosen in step 1 so the webhook URL in the manifest can name it, and a second completion with the same state is a conflict, so a reused code is never exchanged twice.

The address GitHub returns to is `RUDDER_PUBLIC_URL`, else the browser's `Origin`, else the `Host` header. If that address is loopback, private, or `.local`, GitHub cannot deliver webhooks there, so the manifest leaves the webhook out and the UI says so before sending the browser away.

The app's webhook is `POST /api/v1/git-providers/{id}/webhook`. It checks `X-Hub-Signature-256` against the app's webhook secret, then starts a build for every application whose source uses this provider, whose repository (`owner/name`, from an HTTPS or SSH URL) matches the event, and whose branch matches the pushed ref. Per-service webhook secrets stay for personal tokens and GitLab.

A pending app cannot be chosen as a service source, and a build that reaches one says the app is not installed yet. `GET /api/v1/git-providers` reports `status`, `app_slug`, and `account`, never secrets. `POST /api/v1/git-providers/{id}/install-link` gives a fresh GitHub link to finish or change an installation.

The GitHub API base is `RUDDER_GITHUB_API_URL` (default `https://api.github.com`), so tests run against a fake. GitHub Enterprise Server is not supported: the github.com web addresses are fixed.

## Alternatives Considered

1. A callback endpoint on the API as the manifest `redirect_url`, as some panels do. It must be reachable without a session or rely on `SameSite=Lax` to carry one, and it would handle the private key outside the CSRF-protected path.
2. Keep the pending flow in a database table. The sealed `state` carries everything needed and needs no cleanup job or migration.
3. OAuth or personal tokens only. User tokens expire or tie deploys to a person, and cover every repository the person can read.
4. A webhook per service, as for tokens. A GitHub App has one webhook for all its repositories, so a second configuration step per service is avoidable.

## Consequences

Positive:

- No ids, keys, or permissions to type. The app has the least access that still clones and gets push events.
- Push-to-deploy needs no per-repository webhook setup for apps.
- Secrets never pass through a URL the panel serves, and the code exchange is single use.

Tradeoffs:

- The private key and webhook secret are stored like other provider secrets: encrypted with the master key, included in panel backups.
- Deleting a provider does not delete the app on GitHub; the owner removes it there.
- The panel must reach `api.github.com`, and GitHub must reach the panel for push events.
- A manifest code and the install redirect are good for about an hour; after that the flow starts again from the Git providers page, and an unfinished app can be finished with "Finish installing".
- GitHub Enterprise Server is not covered.

## Follow-Up

- List the repositories of an installation (`GET /installation/repositories`) so a service can pick one instead of typing a URL.
- Pull request previews from `pull_request` events.
