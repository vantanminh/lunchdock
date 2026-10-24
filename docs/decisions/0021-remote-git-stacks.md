---
created_at: "2026-10-24T01:16:13.606892673+00:00"
doc: docs/decisions/0021-remote-git-stacks.md
id: "0021"
notes: Supersedes the local-only limit in decision 0011 for Git compose stacks.
status: accepted
title: Copy Git stack checkouts to remote servers
type: decision
updated_at: "2026-10-24T01:16:13.606894727+00:00"
verify: null
---

# Copy Git stack checkouts to remote servers

Date: 2026-10-24

## Status

Accepted

## Context

Git compose stacks were cloned on the panel host. A stack aimed at an extra server was refused with `remote_git`, because the agent has no Git binary and must not receive provider credentials. Image deploys, pasted compose files, and Git application builds (built on the panel, then streamed as an image) already run on extra servers. A compose file that builds from source or mounts a file from the repository does not.

## Decision

The panel still clones and updates the repository, with the same credentials as an application build. When the stack's server is remote and the agent is connected, the panel packs the files Git tracks and the agent extracts that tar under `stacks/<project>/src` in its data directory.

The `.git` directory is not copied, so remotes and credentials in the Git config stay on the panel. A manifest beside the checkout (`stacks/<project>/src.manifest`) lists the paths that were copied. The next copy deletes paths that were in the previous manifest and are absent now, and leaves every other file, including bind-mounted data, where it is. A checkout larger than 512 MiB is refused. Symlinks that point outside the checkout are refused.

The generated compose file and `.env` are still written after the copy, so environment values are not taken from a committed `.env`.

## Consequences

- A disconnected agent returns `remote_unavailable`, the same as any other remote action.
- Files removed from Git are removed on the server only if they were copied by an earlier deploy. Data created on the server is kept.
- The agent protocol gains an `extract_tree` upload. Older agents answer that they do not support it, and the deploy fails with that error until the agent is updated.
