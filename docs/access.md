# Rudder Access

Rudder Access decides who may open an internal app that Rudder is hosting. It is separate from the account that signs in to the Rudder panel.

The panel account is the operator. Rudder Access people are the coworkers who use the internal apps. An email that has not been added is blocked.

## What a person sees

The portal is at `/access` on the panel, for example `https://panel.example.com/access`. You can also give it its own hostname under **Rudder Access**. Point that hostname's DNS at the server. Rudder publishes the route and serves the same portal there. The path stays `/access`.

Sign-in offers two methods:

- **Google.** One redirect URI, shown on the Rudder Access page. Put that exact URI in the Google OAuth client. Rudder accepts the Google account only when Google says the email is verified and the email is already on the allow list.
- **Email code.** Rudder emails a 6-digit code. The code expires in 10 minutes, and five wrong attempts use it up.

Both methods require a Cloudflare Turnstile token. Rudder checks the token on the server with Cloudflare siteverify. The check fails closed: the token must succeed, the action must be `access_login`, and the hostname Cloudflare returns must be one of the hostnames you saved. Do not put `localhost` in that list on a production panel.

After sign-in, the portal lists the internal apps that person may open. Choosing an app sends them to that app's domain. If they are not signed in there yet, Rudder sends them back through the portal and then sets a cookie on the app's own hostname.

## Who may open an app

On the app's **Domains & SSL** tab, turn on **Require Rudder Access** and list the email addresses. You can also allow a whole department or a rank. A department grant includes the departments nested under it. A rank grant matches that rank exactly.

Save, then add a domain if the app does not have one. Rudder stands in front of every request to that domain:

1. Traefik asks the panel's gate.
2. If there is no valid session cookie for that hostname, the browser is sent to the portal.
3. After sign-in, the browser returns to `/_rudder/callback` on the app hostname. Rudder sets an HttpOnly cookie for that hostname only and continues to the original path.
4. Later requests include the cookie. The gate checks that the person is still allowed and passes identity headers to the app:

- `X-Rudder-Access-Email`
- `X-Rudder-Access-User`
- `X-Rudder-Access-Departments` (comma-separated slugs)
- `X-Rudder-Access-Ranks` (comma-separated slugs)

Those headers are trustworthy when network isolation is on, because the app is not published on a host port and visitors cannot connect to it except through Traefik. If you turn isolation off, do not trust the headers. Use the introspect API instead.

The panel hostname and the Access portal hostname are not gated, so a rate limit or a mistake cannot lock the operator out of Rudder.

## Departments and ranks

Create them on **Rudder Access**. A department can have a parent. A rank has a level from 0 to 1000 so an app can order them.

Put a person in departments from the API (`PUT /api/v1/access/identities/{id}/memberships`). Each membership is one department and an optional rank.

The first person you mark as an Access administrator becomes the Access owner. Other administrators, including other panel administrators, cannot remove that owner or take away their administrator flag. The original Rudder panel owner can.

## Email providers

Choose one under **Rudder Access**:

| Provider | What to save |
| --- | --- |
| Resend | API key and a From address |
| SMTP | Host, and optionally port, security (`starttls`, `tls`, or `none`), username, and password |
| Cloudflare Email Sending | Account id, API token, and a From address on a domain that Cloudflare is allowed to send for |

The From address is either `login@example.com` or `Rudder Access <login@example.com>`. Secrets are encrypted at rest and are not returned by the API. Leave a secret blank when saving to keep the one already stored.

**Send a test email** delivers a short note to the panel account you are signed in as. It does not create a sign-in code.

## Connecting an app's backend

Rotate the service token on the app's **Domains & SSL** tab. Rudder shows it once and, on the next deploy, injects these environment variables unless you already set the same name yourself:

| Variable | Meaning |
| --- | --- |
| `RUDDER_ACCESS_TOKEN` | Secret the app uses to call introspect. Prefix `rdr_acc_`. |
| `RUDDER_ACCESS_URL` | Panel public URL, when `RUDDER_PUBLIC_URL` or the panel domain is set. |
| `RUDDER_ACCESS_SERVICE` | Service id. |

The app reads the visitor's `rudder_access` cookie and asks Rudder:

```http
POST /api/v1/access/introspect
Authorization: Bearer rdr_acc_…
Content-Type: application/json

{ "session": "<cookie value>" }
```

A successful response:

```json
{
  "email": "ada@example.com",
  "display_name": "Ada",
  "departments": [
    { "slug": "accounting", "name": "Accounting", "rank": "lead", "level": 40 }
  ],
  "expires_at": "2026-10-24T12:00:00Z"
}
```

Rudder accepts the session only when it was issued for a domain of that same service and the person is still allowed. Map `rank` and `slug` onto the app's own roles.

## Panel API

Panel administrators and agents with the `access` scope use:

| Method | Path | Purpose |
| --- | --- | --- |
| GET, PUT | `/api/v1/access/settings` | Turnstile, Google, email, portal hostname |
| POST | `/api/v1/access/settings/test-email` | Send a test message |
| GET, POST | `/api/v1/access/departments` | List or create departments |
| PATCH, DELETE | `/api/v1/access/departments/{id}` | Update or delete one |
| GET, POST | `/api/v1/access/ranks` | List or create ranks |
| PATCH, DELETE | `/api/v1/access/ranks/{id}` | Update or delete one |
| GET, POST | `/api/v1/access/identities` | List or allow a person |
| PATCH, DELETE | `/api/v1/access/identities/{id}` | Update or remove a person |
| GET, PUT | `/api/v1/access/identities/{id}/memberships` | Replace department memberships |
| GET, PUT | `/api/v1/access/services/{id}` | Read or set the app's allow rules |
| POST | `/api/v1/access/services/{id}/token` | Rotate the service token |

A rule is `{ "kind": "email", "email": "ada@example.com" }`, `{ "kind": "department", "department_id": "…" }`, or `{ "kind": "rank", "rank_id": "…" }`.

The sign-in, logout, portal, callback, and gate routes are for browsers and Traefik. Agents cannot call them. Introspect is for the app's service token, not for an agent token.

## Network isolation

Isolation is on unless the app turns it off. Rudder does not publish the app's ports on the server, so a second app can also listen on port 80. Each container has its own address on the `rudder-proxy` network. When you add a domain, Traefik routes that hostname to the container.

For a Compose stack, published `ports` become container-only `expose` entries on the next deploy. Turn isolation off on that app when something outside Docker must connect to a host port. Databases keep their optional public port.

`PATCH /api/v1/services/{id}/network` with `{ "isolation": false }` stores the choice. Deploy again to apply it.

## Panel owner and authenticator

The first account created at setup is the panel owner. Other administrators can do everything else, but they cannot delete the owner or change the owner's role.

The authenticator on **Account** is the second factor for the panel account. When you enable it, Rudder shows eight recovery codes shaped like `abcde-fghij`. Each code works once at sign-in, in the same field as the authenticator code. Store them somewhere safe. They are not shown again.
