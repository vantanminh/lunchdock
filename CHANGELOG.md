# Changelog

What changes in each Rudder release, written for the people who run it. **System → Updates** in the panel shows the entries of every release it is about to install, so each line should say what a person will notice, not which files moved.

Every change that someone can see or that changes how Rudder behaves gets a line here in the same pull request. When a release is cut, `[Unreleased]` is renamed to the version and date and a new empty `[Unreleased]` is added on top. The release workflow publishes the section of the tag as the release notes and refuses to run when it is empty.

Sections: **Added** (new things), **Changed** (existing behavior), **Fixed**, **Security**, **Removed**. Earlier releases are listed on [GitHub Releases](https://github.com/vantanminh/lunchdock/releases).

## [Unreleased]

### Added
- **Watch a stack deploy while it happens.** The Deployments tab shows the output of `docker compose` (image pulls, build steps, container start-up) as it is produced, opens the running deployment's log by itself, follows the newest line, and shows the current step on the row. Before, the log appeared only when the action had finished, so a long build looked frozen.
- **Cancel a running stack action.** A Cancel button stops a deploy, build or pull that is taking too long or went wrong; the deployment ends as *cancelled* and the stack accepts new actions again.
- **Time limit for stack actions.** An action that runs longer than `RUDDER_STACK_TIMEOUT_SECS` (default one hour, `0` for none) is stopped and recorded as failed, instead of blocking the stack forever.
- **Rebuild and deploy, Pull and deploy.** Two new actions: rebuild stacks that build from source (`up --build`), or fetch newer versions of every image first (`up --pull always`). *Build images on every deploy* makes the normal Deploy rebuild too. A plain Deploy still reuses images built earlier; the stack page now says which services build from source.
- **Deploy a Git stack when you push.** *Deploy when I push to the branch* (stack source settings) lets the GitHub App webhook deploy the stack on every push to its branch.
- **Saved registry logins work for stacks.** Logins from the Registries page, such as GitHub's `ghcr.io`, are now used when a stack pulls its images, so private images no longer need `docker login` on the server by hand. A refused pull tells you which registries the stack uses.
- **Connect GitHub with one button.** Git providers has a "Connect with GitHub" card that creates and installs a private GitHub App for you; there is no app id, installation id or private key to copy any more. Pushes to the repositories you pick build the matching services when the panel can be reached from the internet.
- **Import many environment variables at once.** *Import .env* on the Environment tab takes a pasted `.env` block or a file, previews every key as new, changed or unchanged, and saves them all in one step; if any line is wrong nothing is saved.
- **See that a variable was saved correctly.** The Environment list shows when each key was last changed and how many characters its value has, and *Check value* tells you whether a value you type matches the saved one. The saved value is still never shown.
- **Release notes for everything an update brings in.** System → Updates now lists the notes of every release between the installed version and the newest one, and shows them before you press Update.

### Changed
- **Variables with a fallback are no longer reported as missing.** A stack that reads `${NAME:-default}` only lists `NAME` under *Not set* when the file has no fallback.
- **Long deployment logs are cut in the list.** Each deployment keeps the last 1 MiB of output; the Deployments tab shows the last 200 000 characters with a link to the whole log.

## [0.1.5] - 2026-10-12

### Changed
- **Projects page redesigned** as a workspace: projects on the left, the selected project's environments and services on the right, with search and a phone layout.
- **Service page redesigned** with clearer tabs and details.
