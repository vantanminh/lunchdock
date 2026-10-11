---
created_at: "2026-10-11T03:06:30.674090646+00:00"
doc: docs/decisions/0008-traefik-file-provider-sni.md
id: "0008"
notes: null
status: accepted
title: Traefik file provider and SNI certificate matching
type: decision
updated_at: "2026-10-11T03:06:30.674091316+00:00"
verify: null
---

# Traefik file provider and SNI certificate matching

Date: 2026-10-11

## Status

Accepted

## Context

Image apps need a domain, HTTPS, and a way to move traffic to a new container without dropping the old one if the new one is unhealthy. Custom certificates include wildcards, and Traefik must pick them by SNI instead of a per-domain selector.

## Decision

Routes are rendered into one Traefik v3 file-provider document per server (`traefik/dynamic/routes.yml`). Let's Encrypt routers set `tls.certResolver: letsencrypt`. Custom-certificate routers set `tls: {}` and every uploaded certificate is listed under `tls.certificates`, so Traefik matches SAN and wildcard names by SNI. Certificate files are written mode `0600` and referenced at `/etc/rudder/traefik/certs` inside the Traefik container. The install will mount the panel data directory there.

A name matches `*.domain.com` only when the host has exactly one extra label. If a certificate has SANs, the common name is not used for matching. The private key must be PKCS#8 or PKCS#1 RSA, or P-256. A supplied chain must link by issuer name and verify. PEMs are encrypted at rest and are not returned by the API.

Zero-downtime rollout pulls the image, starts `{service}-{generation}` on the `rudder-proxy` network, waits for the health check, rewrites the Traefik file, then stops the previous container. A failed health check or a failed file write removes the new container and leaves the previous one in place. Rollback redeploys the image from a successful deployment row.

Registry passwords are encrypted. The pull uses the registry whose host matches the image (`ghcr.io/...` selects `ghcr.io`; `nginx:1` selects `docker.io`) unless the request names one. Tag polling every five minutes records the first digest and redeploys only when a later digest differs.

Remote servers keep the domain rows. The agent applies the files when it exists. Until then, deploy on a remote server is rejected.

## Consequences

Traefik's static config must define entrypoints `web` and `websecure`, the `letsencrypt` resolver, the file provider directory, and the certificate mount. P-384 and P-521 keys are rejected with a clear error. Preview hostnames use `<service>-<8 hex>.<ipv4>.sslip.io`.
