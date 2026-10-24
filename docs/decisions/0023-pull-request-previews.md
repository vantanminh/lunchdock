---
created_at: "2026-10-24T01:36:59.044766161+00:00"
doc: docs/decisions/0023-pull-request-previews.md
id: "0023"
notes: Same-repository pull requests only. Opt in per service.
status: accepted
title: Preview a pull request with a copy of the service
type: decision
updated_at: "2026-10-24T01:36:59.044766877+00:00"
verify: null
---

# Preview a pull request with a copy of the service

Date: 2026-10-24

## Status

Accepted

## Context

Push-to-deploy and preview hostnames already exist. A pull request still had no environment of its own. Decision 0019 listed pull request previews as follow-up work.

## Decision

A service opts in with `preview_pull_requests`. The GitHub App webhook handles `pull_request` for `opened`, `synchronize`, `reopened`, and `closed`.

For each opted-in application or Git stack whose repository and branch match the pull request base, Rudder keeps one copy named `{service}-pr-{number}`. The copy uses the head branch of the same repository, the parent's environment variables, and a preview address. Updating the pull request deploys that copy again. Closing it removes the copy. A head repository that is not the base repository is ignored.

New GitHub Apps subscribe to `push` and `pull_request`. An app created earlier does not receive pull request events until that event is enabled on GitHub.

## Consequences

- Preview copies are ordinary services. They are not deployed by a push to their branch; the pull request event is what updates them.
- A fork pull request does not receive a preview, because the panel may not be allowed to clone it.
