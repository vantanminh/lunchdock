# Opening the panel over HTTPS

A fresh install answers on `http://<server-ip>:3000`. Over plain HTTP the sign-in password, the session cookie and every secret typed into the panel (environment variables, registry and S3 credentials, Git tokens) cross the network in clear text.

## What the panel does about it

- A warning banner stays at the top of every page, and on the sign-in and setup screens, while `location.protocol` is `http:` and the host is not loopback (`localhost`, `*.localhost`, `127.x.x.x`, `::1`). On the pages behind sign-in it links to **Certificates**.
- Saving a secret (an environment variable or an `.env` import, a registry, S3 destination or Git provider credential, and the *Check value* box) asks for confirmation first. The answer is remembered in `sessionStorage` for that browser tab only, so closing the tab asks again.
- **Certificates → Panel domain** shows the same warning with the steps below. Nothing blocks the panel: the API still accepts requests over HTTP, because an installation behind a trusted private network or a TLS-terminating proxy that forwards plain HTTP is legitimate. Use `RUDDER_COOKIE_SECURE=true` once the panel is only reachable over HTTPS.

## Give the panel its own domain

1. Point a DNS record for the panel (for example `panel.example.com`) at the server.
2. **Certificates → Panel domain**: enter the domain, pick an SSL mode (Let's Encrypt or an uploaded certificate) and save.
3. Open the panel through `https://panel.example.com` and set `RUDDER_PUBLIC_URL` to that address. The GitHub App should be created from the HTTPS address too, so its callback and webhook URLs use it.
4. Press **Check SSL** under the form. Rudder connects to the saved domain over HTTPS and reports whether the certificate is trusted, covers the domain, who issued it and the days left. Let's Encrypt can take a minute to issue: if the check says the certificate is not trusted right after saving, wait and check again. AI agents get the same result from the `get_v1_panel_ssl-check` tool.

The saved panel domain is also the address AI agents sign in at (see `docs/mcp.md`).
