---
created_at: "2026-10-11T02:23:53.979411864+00:00"
doc: docs/decisions/0006-cookie-sessions-csrf-totp.md
id: "0006"
notes: null
status: accepted
title: "Cookie sessions, CSRF, and encrypted TOTP secrets"
type: decision
updated_at: "2026-10-11T02:23:53.979412402+00:00"
verify: null
---

# Cookie sessions, CSRF, and encrypted TOTP secrets

Date: 2026-10-11

## Status

Accepted

## Context

The panel is a browser SPA plus a token-authenticated CLI. Browser sessions need to survive reloads without putting a bearer token in JavaScript-readable storage. API tokens must work from CI without a CSRF dance. Authenticator secrets and later environment variables share one master key.

## Decision

Passwords are Argon2id with the crate default parameters. Session cookies are named `rudder_session`, HttpOnly, SameSite=Lax, and stored only as SHA-256 hashes. A second random CSRF token is returned in the JSON body and required as `x-csrf-token` on cookie-authenticated POST, PUT, PATCH, and DELETE. `Authorization: Bearer` skips CSRF. Login attempts are limited to 10 per minute per client IP, trusting `X-Forwarded-For` only when `RUDDER_TRUST_PROXY` is set. TOTP uses SHA-1, 6 digits, 30 second steps, and skew 1. The secret is encrypted with AES-256-GCM under the panel master key before it is written to `users.totp_secret`.

## Alternatives Considered

1. Store the session id in localStorage and send it as a bearer header. Simpler CSRF story, and any XSS can read the session.
2. Server-side sessions in a signed cookie that contains the user id. Revocation requires a denylist, and the cookie grows with claims.

## Consequences

Positive:

- The browser cannot read the session cookie, and a cross-site form cannot forge a write.
- API tokens and the CLI share the same user and role checks.

Tradeoffs:

- The SPA must keep the CSRF token in memory or sessionStorage and send it on writes.
- Argon2id makes login and unknown-user dummy verification intentionally slow.
