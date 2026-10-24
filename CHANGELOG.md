# Changelog

What changes in each Rudder release, written for the people who run it. **System → Updates** in the panel shows the entries of every release it is about to install, so each line should say what a person will notice, not which files moved.

Every change that someone can see or that changes how Rudder behaves gets a line here in the same pull request. When a release is cut, `[Unreleased]` is renamed to the version and date and a new empty `[Unreleased]` is added on top. The release workflow publishes the section of the tag as the release notes and refuses to run when it is empty.

Sections: **Added** (new things), **Changed** (existing behavior), **Fixed**, **Security**, **Removed**. Earlier releases are listed on [GitHub Releases](https://github.com/vantanminh/lunchdock/releases).

## [Unreleased]

### Added
- **Git stacks run on extra servers.** A stack whose compose file is in Git can deploy on a server that runs the agent. Rudder copies the repository's files to that server and leaves files you created there, such as a database directory mounted into the stack, in place. The agent has to be connected. A repository larger than 512 MiB is refused.
- **Cached pages can be kept on disk.** A cache policy has a disk budget. Leave it blank and caching stays in memory, as before. Set it and responses are kept on disk across a cache restart, while the busiest ones stay in memory. The Cache page shows how much disk is in use.
- **A pull request can get its own preview.** Turn on **Preview pull requests** on a service's Git source. Opening, updating, or reopening a pull request against that branch creates a copy of the service with a preview address; closing the pull request removes it. Pull requests from another repository are ignored. A GitHub App connected before this release needs the **Pull request** event turned on in the app's settings on GitHub, or previews will not start.

## [0.3.1] - 2026-10-16

### Fixed
- **Agent updates reliably start the downloaded binary.** The agent waits until the staged file is fully closed before checking its version, avoiding intermittent Linux "Text file busy" errors.
- **Complete installation and update downloads.** This release includes the installers, signed checksums, binaries and container images missing from the incomplete 0.3.0 release. It also includes the changes listed below; no manual migration is required.

### Added
- **The panel stays reachable when the server is overloaded.** A load guard reads the panel server's memory and CPU every few seconds. While the server is busy, new builds (and push deploys of stacks that build) wait in the queue; when free memory is about to run out, running builds are stopped and the reason is written to their log, so the panel, its database and Traefik keep enough room to answer. The guard is on by default and its thresholds can be changed (`busy` below 15% free memory or above 90% CPU, builds stopped below half the memory reserve). A build held only because of CPU starts anyway after 20 minutes.
- **Load page.** **Operate → Load** shows the panel server's memory, CPU, swap, load and memory pressure with the guard's thresholds drawn on the bars, why builds wait, the build queue, how the panel, its database and Traefik are protected, the guard settings, and what to do if the panel stops answering. A banner on every page says when the server is out of memory, builds are paused, or builds wait for room.
- **Search and jump from anywhere.** Press **Ctrl K** (**⌘K** on a Mac) or **/**, or use the search box in the header, to open any page or service by typing part of its name (Vietnamese works without accents) and to run common actions.
- **Connection status in the header.** A dot shows whether the panel answers normally, slowly, is busy, or is not reachable; pages retry with growing pauses instead of failing at once.
- **Pause, cancel and limit builds.** Admins can pause every new build, cancel all queued and running builds at once, and set how many builds run at the same time on one server (the API is under `/api/v1/load`).

### Changed
- **Redesigned overview.** The overview opens with a health summary that lists each problem with the button that fixes it, shows builds running and waiting next to servers, services and alerts, adds the services using the most CPU or memory with a Stop button, and shows the panel server's load and build controls beside the service list.
- **Apps are killed before the panel when memory runs out.** App, database and stack containers now start with a higher OOM score, and the panel, its database and Traefik get more CPU weight than apps (applied on every panel start). Redeploy an app to give its container the new score. New installs start the panel, Postgres and Traefik with a low OOM score; on an existing install the panel gets it with the next panel update, and Postgres and Traefik when their containers are recreated by `install.sh`.
- **A flood of anonymous requests no longer locks you out.** Requests from signed-in people and API tokens always get through; anonymous requests (webhooks, sign-in pages, scanners) are answered "busy, try again in a few seconds" when 64 are already in progress.
- **The agent on extra servers is protected the same way.** New agent installs run the agent and Traefik with a low OOM score and a higher CPU weight, so the panel can still stop apps on an overloaded server. For an agent installed earlier, add `OOMScoreAdjust=-900` and `CPUWeight=1000` with `systemctl edit rudder-agent`, then restart it.

### Fixed
- **Readable errors when a proxy answers instead of the panel.** When Traefik answers "Bad Gateway" because the panel is restarting or overloaded, pages show that error and keep retrying, instead of a JSON parse error.

## [0.2.0] - 2026-10-12

### Added
- **Watch a stack deploy while it happens.** The Deployments tab shows the output of `docker compose` (image pulls, build steps, container start-up) as it is produced, opens the running deployment's log by itself, follows the newest line, and shows the current step on the row. Before, the log appeared only when the action had finished, so a long build looked frozen.
- **Cancel a running stack action.** A Cancel button stops a deploy, build or pull that is taking too long or went wrong; the deployment ends as *cancelled* and the stack accepts new actions again.
- **Time limit for stack actions.** An action that runs longer than `RUDDER_STACK_TIMEOUT_SECS` (default one hour, `0` for none) is stopped and recorded as failed, instead of blocking the stack forever.
- **Rebuild and deploy, Pull and deploy.** Two new actions: rebuild stacks that build from source (`up --build`), or fetch newer versions of every image first (`up --pull always`). *Build images on every deploy* makes the normal Deploy rebuild too. A plain Deploy still reuses images built earlier; the stack page now says which services build from source.
- **Deploy a Git stack when you push.** *Deploy when I push to the branch* (stack source settings) lets the GitHub App webhook deploy the stack on every push to its branch.
- **Saved registry logins work for stacks.** Logins from the Registries page, such as GitHub's `ghcr.io`, are now used when a stack pulls its images, so private images no longer need `docker login` on the server by hand. A refused pull tells you which registries the stack uses. The Registries page now says so.
- **Connect GitHub with one button.** Git providers has a "Connect with GitHub" card that creates and installs a private GitHub App for you; there is no app id, installation id or private key to copy any more. Pushes to the repositories you pick build the matching services when the panel can be reached from the internet.
- **Import many environment variables at once.** *Import .env* on the Environment tab takes a pasted `.env` block or a file, previews every key as new, changed or unchanged, and saves them all in one step; if any line is wrong nothing is saved.
- **See that a variable was saved correctly.** The Environment list shows when each key was last changed and how many characters its value has, and *Check value* tells you whether a value you type matches the saved one. The saved value is still never shown.
- **Warning when the panel is opened over plain HTTP.** A banner stays on every page (and on the sign-in screen) while the address is `http://` and not this computer, saying that passwords and secrets can be read on the network, with a link to the Certificates page where the panel can get its own domain and certificate. Saving an environment variable, registry, S3 or Git credential over HTTP now asks you to confirm first (once per browser tab).
- **See what a Git stack needs before its first deploy.** *Refresh from Git* reads the compose file from the repository without deploying, so the stack page shows its services, the generated file and any warnings, and the Environment tab lists the variables the file reads that still have no value (picking one fills in its key). Before, you found out only after a failed deploy.
- **Pick the repository, branch and compose file from GitHub.** With a GitHub App provider the Source forms show the repositories the app can read, the branches of the one you chose (default first), and the compose files found on that branch, instead of free text. Saving a source now asks GitHub whether the app can read the repository and the branch, so a typo or a repository outside the installation is refused immediately rather than at the first deploy.
- **Choose a stack's compose project name.** When creating a stack, or on its source form until the first deploy, you can set the compose project name. Using the name of a project already running on the server lets the stack take over its containers and named volumes (databases keep their data) instead of getting a new random name. The guide now also explains how to dump and restore a database or copy a volume.
- **Release notes for everything an update brings in.** System → Updates now lists the notes of every release between the installed version and the newest one, and shows them before you press Update.

### Changed
- **Variables with a fallback are no longer reported as missing.** A stack that reads `${NAME:-default}` only lists `NAME` under *Not set* when the file has no fallback.
- **Long deployment logs are cut in the list.** Each deployment keeps the last 1 MiB of output; the Deployments tab shows the last 200 000 characters with a link to the whole log.

## [0.1.5] - 2026-10-12

### Changed
- **Projects page redesigned** as a workspace: projects on the left, the selected project's environments and services on the right, with search and a phone layout.
- **Service page redesigned** with clearer tabs and details.
