---
created_at: "2026-10-11T02:00:54.005674719+00:00"
doc: docs/decisions/0005-no-global-request-timeout.md
id: "0005"
notes: null
status: accepted
title: No global HTTP request timeout
type: decision
updated_at: "2026-10-11T02:00:54.005677645+00:00"
verify: null
---

# No global HTTP request timeout

Date: 2026-10-11

## Status

Accepted

## Context

The panel will serve log tails, build log streams, and a web terminal. A single tower timeout around the router would cut those connections off. Ordinary JSON routes still need protection from a stuck handler.

## Decision

The HTTP router does not install a global request timeout. Streaming and terminal routes stay open until the client disconnects or the operation finishes. Short JSON routes will set their own limits when those handlers exist. Request body size limits will be applied per route group so certificate uploads and settings import can be larger than a login body.

## Alternatives Considered

1. A 60 second timeout layer on the whole router. Simple, and it breaks the terminal and live logs.
2. Disabling timeouts only after the fact with a second router. Two servers to operate.

## Consequences

Positive:

- WebSocket and SSE routes can be added on the same listener.

Tradeoffs:

- Each blocking handler must bound its own work. Database calls already use the pool acquire timeout.

## Follow-Up

- Add per-route body limits with the upload endpoints.
