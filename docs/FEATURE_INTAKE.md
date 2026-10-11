# Feature Intake

Every implementation prompt enters the intake gate before code changes.

## Lanes

### Tiny

Low-risk docs, copy, or narrow edits. Record intent, patch directly, run quick
checks. No full story packet required.

### Normal

Story-sized behavior with bounded blast radius. Record it with
`harness story add` (do not create `docs/stories/*.md` by hand), link product
docs, define validation, implement the smallest vertical slice.

### High-Risk

Touches security, data model, multi-role contracts, or large scope. Record the
story with `harness story add --lane high-risk`, require explicit proof, and
record durable decisions with `harness decision add`. Do not hand-create story
files.

## Checklist (escalate if any apply)

- Auth / sessions
- Authorization / tenancy
- Data model / migrations
- Audit / sensitive data
- External providers
- Public API contracts
