---
name: harness
description: Use the repository's 5harness CLI for project intake, durable work tracking, bounded reads, and agent handoffs.
---

# Harness

You are a coding agent. **5harness** (bin `harness`) is how you record and
retrieve work in this repo. Durable stories, decisions, intakes, backlog, and
reports are Git-backed markdown. **Do not hand-edit those files.**

## First commands

```bash
harness doctor --json
harness status --json
harness next --json
```

Do not dump `docs/HARNESS.md` or the whole docs tree first. Read with tools:

```bash
harness search "…" --json
harness get <id> --json
harness links <id> --json
harness context <id> --json
harness query matrix --json
harness query reports --json
```

## Work loop

```bash
harness intake --type <type> --summary "…" --lane tiny|normal|high-risk
harness story add --id US-… --title "…" --lane normal
harness story start US-…                 # also: --id US-…
# implement the slice, then commit it
git add <slice files> && git commit -m "feat: …"
harness story done US-…
harness story update --id US-… --status implemented --unit 1 --integration 1 --e2e 0 --platform 0
harness decision add --id … --title "…" --doc docs/decisions/….md
harness decision update --id … --status accepted --notes "…"
harness backlog add --title "…" --pain "…"
```

`story add` / `update` use `--id`. `story start` / `done` / `block` take a
positional id (`--id` also works).

## Commit after each completed slice

When a small task is done: `git add` only those files and `git commit` with a
conventional message. Do not wait for the whole epic. Do not `git push` unless
the user asked. Never commit `.5harness/`, secrets, or unrelated dirty files.
Skip if there is nothing to commit or the user forbade commits.

## Hard-fail (decision 0017)

If a harness command exits non-zero: **HARD STOP**. Recover with
`harness doctor`, `harness link`, `harness reindex`. Never bypass by editing
entity markdown.

## MCP

Prefer the CLI. If MCP is connected, use JSON tools (`harness_next`,
`harness_get`, `harness_query_*`). Project id: `harness project id`.
For an all-projects grant, send `X-Harness-Project: <id>` on every call.

Do not call unimplemented commands (decision 0023). The shipped CLI is the
contract — run `harness --help`.
