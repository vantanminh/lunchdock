# Harness

The harness is the repo-level operating system for humans and coding agents.

The app is what users touch. The harness is what agents touch.

## Mental Model

```text
Human intent
  -> Feature intake (tiny | normal | high-risk)
  -> Story packet (when needed)
  -> Agent work loop
  -> Product delta + proof
  -> Harness delta (docs, decisions, backlog)
```

Every task may produce:

1. **Product delta** — application code, tests, product docs.
2. **Harness delta** — operating docs, templates, decisions that help the next agent.

## Install

```bash
# preferred: global CLI for multi-project work
npm i -g 5harness
harness --help

# or one-shot
npx 5harness --help
```

## Durable layer (markdown SoT)

Operational entities live as **git-committed markdown** with YAML frontmatter:

| Type | Path |
| --- | --- |
| Story | `docs/stories/<id>.md` |
| Decision | `docs/decisions/<id>.md` |
| Intake | `docs/intakes/IN-###.md` |
| Backlog | `docs/backlog/BL-###.md` |

Machine-local derived data (gitignored):

- `.5harness/index/` — agent search/link index (`harness reindex`)
- `.5harness/local/` — traces and other non-shared state

```bash
harness init          # scaffold + register this project
harness link          # after clone: register + reindex
harness story add …
harness query matrix
harness search "…"
harness get US-001
```

**Agents must not hand-edit operational entity files.** Use write commands only.

## Collaborator path

```text
git clone → npm i -g 5harness → harness link → harness reindex
→ same markdown history; dashboard/list via registry
```
