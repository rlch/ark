---
created: "2026-04-14"
last_edited: "2026-04-14T01:00:00Z"
---

# Implementation Overview

Build site: context/plans/build-site.md (134 tasks, 7 tiers).

## Tier Progress

| Tier | Done | Total | Status |
|------|------|-------|--------|
| 0 | 17 | 17 | ✅ COMPLETE |
| 1 | 28 | 28 | ✅ COMPLETE |
| 2 | 16 | 16 | ✅ COMPLETE |
| 3 | 22 | 22 | ✅ COMPLETE |
| 4 | 0 | 10 | pending |
| 5 | 0 | 16 | pending |
| 6 | 0 | 26 | pending |

**Overall: 83/134 tasks done (62%) · 658 tests passing**

**Crate test breakdown:**
- ark-types: 85 (foundation types)
- ark-core: 34 (traits, event bus, control socket, events-log)
- ark-config: 39 (figment + schema + hooks + template)
- ark-mux-zellij: 63 (ZellijMux impl + layout templating + 6 shipped KDLs)
- ark-pane: 42 (chrome + diff + git + log widgets)
- ark-test-fixtures: 2

## Per-Domain Status

| Domain | Done | Status | Tracking File |
|--------|------|--------|---------------|
| distribution | 2 | TIER-0-DONE (T-001/002) | impl-distribution.md |
| types-state-events | 11 | TIER-0-DONE (R1-R7 foundational) | impl-types-state-events.md |
| architecture | 5 | TIER-0-DONE (R1-R6 traits + scope) | impl-architecture.md |
| config | 6 | TIER-1 DONE (T-018-T-023) | (impl-config.md pending) |
| mux-zellij | 9 | TIER-1 DONE (T-024-T-032) | (impl-mux-zellij.md pending) |
| layouts | 6 | TIER-1 DONE (T-033-T-038) | (impl-layouts.md pending) |
| pane-commands | 4 | TIER-1 DONE (T-039-T-042) | (impl-pane-commands.md pending) |
| hook-ipc | 9 | T-043-T-051 all hook-ipc tasks (primitives + skeleton + parser + JSONL + pipe + allow + fail-open) | (impl-hook-ipc.md pending) |
| engine-claude-code | 7 | T-052-T-058 full ClaudeCodeEngine building blocks (settings, transcript, permission, done, stall, handle, preflight) | (impl-engine-claude-code.md pending) |
| orchestrator-cavekit | 9 | T-075-T-083 complete (detect + run + 5 watchers + build-site extractor + done resolver) | (pending) |
| orchestrator-claude-code | 2 | T-073 detect + T-074 run DONE | (pending) |
| supervisor | 13 | all 22 supervisor/lifecycle/socket tasks — daemonize, lock, socket, commands, signals, orchestration, kill, crash, auto-close, audit log | (impl-supervisor.md pending) |
| cli | 0 | TIER-4 pending | (pending) |
| plugin-status | 0 | TIER-5 pending | (pending) |
| plugin-picker | 0 | TIER-5 pending | (pending) |
| testing | 0 | TIER-6 pending | (pending) |

## Tooling Notes

- ck:task-builder subagent broke twice (narration loop). general-purpose subagents work reliably for parallel packet execution.
- Inline execution used for small (S-effort) tasks where dispatch overhead exceeds work.
- Caveman mode active for build phase status reports (per preset config).
