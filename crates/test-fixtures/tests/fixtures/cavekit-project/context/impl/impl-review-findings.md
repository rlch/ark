---
created: "2026-04-15"
last_edited: "2026-04-15"
---

# Implementation Review Findings

Build site: context/plans/build-site.md

Sample codex adversarial findings for watcher contract tests.

## Tier 0 Gate (2026-04-15)

| Finding | Severity | File | Status | Notes |
|---------|----------|------|--------|-------|
| F-001: build-site mermaid has unreachable node (source: codex) | P1 | context/plans/build-site.md:L12 | NEW | — |
| F-002: impl-overview lacks activity-log entry for T-003 (source: codex) | P2 | context/impl/impl-overview.md:L18 | NEW | — |
| F-003: ralph-loop.md missing started_at timestamp (source: codex) | P3 | ralph-loop.md:L4 | NEW | cosmetic |

### F-001 — P1 build-site mermaid has unreachable node

**Source:** codex
**Tier:** 0
**Severity:** P1
**Status:** fixed
**Location:** context/plans/build-site.md:12

Mermaid DAG referenced `T-009` which is not declared in any tier table.
Resolution: dropped the stray edge; DAG is now connected.

### F-002 — P2 impl-overview lacks activity-log entry for T-003

**Source:** codex
**Tier:** 0
**Severity:** P2
**Status:** open

Activity log skipped T-003 completion. Watchers depend on contiguous log
lines for last-updated detection.

### F-003 — P3 ralph-loop.md missing started_at timestamp

**Source:** codex
**Tier:** 0
**Severity:** P3
**Status:** open

Cosmetic — watcher tolerates absence but the canonical shape includes it.
