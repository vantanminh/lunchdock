# Agent Instructions

If `.agents/skills/harness/SKILL.md` exists, use it for the work loop,
mutation rules, and tool-only durable writes. `harness init` installs that
skill.

## Optional Harness Cloud sync

If this project opts into cloud sync, use `harness login` (default
https://5harness.knotree.com) followed by one `harness sync push` so later
durable edits auto-sync. Use `harness sync pull|status` and
`harness plan get <token>` for web-AI briefs. The Cloudflare Worker is the
canonical OAuth/MCP endpoint; an optional Pages proxy may serve the dashboard.
The CLI encrypts a restore snapshot before upload; a user-scoped catalog is
stored so hosted MCP can read harness entities. Do not sync `.5harness/`,
credentials, traces, indexes, or arbitrary files. Never commit `.env`, Firebase
service-account keys, Pages secrets, or a sync passphrase. Review the project's
cloud sync runbook before enabling it.

<!-- HARNESS:BEGIN -->
<!-- harness-version: 0.30.1 -->
<!-- harness-project-id: 2fa11256f8caaa0bb46f400cdfd97cfa -->
## Harness

**5harness** (bin `harness`) is this repo's operating system for **coding
agents**. Durable work — stories, decisions, intakes, backlog, reports — is
Git-backed markdown. **You** (the agent) read and write that history through
the CLI. You do not invent commands. You do not hand-edit those files.

### First commands

```bash
npm i -g 5harness          # if `harness` is missing
harness link               # after cloning a harnessed repo
harness doctor --json
harness status --json
harness next --json
```

Do **not** start by dumping `docs/HARNESS.md`, `ARCHITECTURE.md`, or
`CONTEXT_RULES.md`. Open those only when the task changes harness or product
rules. Prefer `.agents/skills/harness/SKILL.md` when present.

### Work loop

```bash
harness intake --type <type> --summary "…" --lane tiny|normal|high-risk
harness story add --id US-… --title "…" --lane normal
harness story start US-…                 # also: --id US-…
# implement the slice
git add <slice files> && git commit -m "feat: …"
harness story done US-…                  # or: story update --id … --status implemented
harness next --json
```

`story add` / `update` use `--id`. `story start` / `done` / `block` take a
positional id (`--id` also works).

### Mutation rule (mandatory)

**Do not** create or edit operational durable markdown by hand
(stories / decisions / intakes / backlog / reports).

```bash
harness intake --type … --summary "…" --lane normal
harness story add --id US-… --title "…" --lane normal
harness story update --id US-… --status implemented --unit 1 --integration 1 --e2e 0 --platform 0
harness decision add --id … --title "…" --doc docs/decisions/….md
harness query matrix --json
```

All mutation commands auto-reindex after writing. You do NOT need to call
`harness reindex` manually after mutations.

### Commit after each completed slice (mandatory)

When a small task is actually done (code + tests/docs for that slice, or a
durable write that should travel with the repo):

1. `git add` only the files for that slice.
2. `git commit` with a conventional message (`feat:`, `fix:`, `docs:`, `chore:`).
3. Do **not** wait for the whole epic.
4. Do **not** `git push` unless the user asked.
5. Never commit `.5harness/`, secrets, `node_modules`, or unrelated dirty files.

Skip if there is nothing to commit, or the user forbade commits.

### HARD STOP — harness failure contract (decision 0017)

If the harness **CLI** or **MCP** fails, is missing, or returns a non-zero /
error result for a step you need:

1. **HARD STOP** that durable-write path. Do **not** continue as if it succeeded.
2. **Never** fall back to hand-editing story / decision / intake / backlog
   markdown to “fix” or bypass the failure.
3. **Recover**, then retry the harness command:

| Order | Command | Why |
| --- | --- | --- |
| 1 | `harness --version` | Confirm install / PATH |
| 2 | `harness doctor` or `harness doctor --json` | Workspace health |
| 3 | `harness link` | Register clone / registry pointer |
| 4 | `harness reindex` | Rebuild derived index from markdown |
| 5 | `harness status` / `harness next` | Confirm the project is usable |

**Exit codes:** `0` = success; `1` = usage / validation / operational error
(**stop**, fix, retry); `2` = reserved — treat as non-success unless that
command’s docs say otherwise. Non-zero exit is never success.

### Read with tools (prefer `--json`)

```bash
harness search "…" --json
harness get <id> --json
harness links <id> --json
harness context <id> --json
harness query matrix --json
harness query stats --json
harness query reports --json
```

Classify work with feature intake before large edits. Record durable decisions
when architecture or product rules change.

### MCP (optional)

Prefer the CLI. If MCP is connected, use JSON tools (`harness_next`,
`harness_get`, `harness_query_*`). Discover the project id with
`harness project id` or the `<!-- harness-project-id: … -->` comment.
For an all-projects grant, send `X-Harness-Project: <id>` (preferred) or
`?project=<id>` on every call. Never rely on cwd to select a project.

### Upgrade

When a newer harness CLI version is installed (`npm i -g 5harness`),
run `harness upgrade` to update the harness block in this AGENTS.md.
Only the harness-managed section (markers HARNESS:BEGIN through HARNESS:END)
is modified — all other content is preserved.
<!-- HARNESS:END -->
