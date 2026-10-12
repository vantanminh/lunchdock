# GitHub App

A GitHub App lets Rudder clone private repositories with short-lived tokens and receive push events, without a personal access token. Rudder creates the app for you.

## Connect GitHub

Git providers → **Connect with GitHub**.

1. Rudder sends you to GitHub with the app already described. Confirm the name and press *Create GitHub App*.
2. GitHub asks which repositories the app may read. Choose them and press *Install*.
3. You come back to Rudder. The provider is listed as *Ready* with the GitHub account that installed it.

To create the app under an organization, open *Create it under an organization* and enter its login first. An organization that requires approval shows *Waiting for approval* until an owner approves; then open **Manage repositories** on the provider to finish.

Only admins can connect GitHub. The app is private to its owner and asks for read access to repository contents and metadata, plus the `push` event.

### Public address

GitHub sends you back to, and delivers push events to, the address you used to open the panel, or `RUDDER_PUBLIC_URL` when it is set. Set it when the panel is behind a proxy so the address is the public one. If the address is private (`localhost`, a private IP, `.local`), GitHub cannot reach it: Rudder warns you before going to GitHub and creates the app without a webhook. Builds then start by hand ("Build now") or through a per-service webhook.

## Use it

Open an application → Source, choose the GitHub App provider, and enter the repository URL (`https://github.com/owner/name.git`) and branch. A provider that is not installed yet is greyed out as *needs install*.

A push to a branch starts a build of every application whose source uses this provider, the same repository (`owner/name`, case-insensitive, HTTPS or SSH URL), and that branch. The Git source card still shows a per-service webhook secret; it is not needed for apps. A compose stack with the same provider, repository and branch is deployed too, but only when *Deploy when I push to the branch* is on in its source settings (see [stacks](stacks.md#deploy-on-push)).

## Manage

- **Manage repositories** opens GitHub's installation page to add or remove repositories.
- **Finish installing** appears on an app that was created but not installed, for example if you closed the tab on GitHub.
- Deleting the provider removes it from Rudder only. Delete the app on GitHub too (Settings → Developer settings → GitHub Apps).

## Settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `RUDDER_PUBLIC_URL` | none | Public address of the panel, used for GitHub's redirects and webhook |
| `RUDDER_GITHUB_API_URL` | `https://api.github.com` | GitHub API base, for app creation, installation checks, and installation tokens |

GitHub Enterprise Server is not supported.

## API

| Method and path | Notes |
| --- | --- |
| `POST /api/v1/github-app/manifest` | Admin, write. `{"organization"?, "name"?}` → `{action_url, manifest, webhook}`; POST `manifest` as a form field to `action_url` |
| `POST /api/v1/github-app/complete` | Admin, write. `{code, state}` from GitHub's redirect → `{provider, install_url}` |
| `POST /api/v1/github-app/installed` | Admin, write. `{installation_id, state?}` from GitHub's redirect → provider. Without `state` the pending app that GitHub confirms owns the installation is used |
| `POST /api/v1/git-providers/{id}/install-link` | Admin, write. A fresh `install_url` for an existing app |
| `POST /api/v1/git-providers/{id}/webhook` | GitHub only. Verified with the app's webhook secret; starts matching builds |

`GET /api/v1/git-providers` reports `status` (`ready` or `pending_install`), `app_slug`, and `account`, never secrets. The design is in decision 0019.
