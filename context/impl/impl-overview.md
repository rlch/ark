---
created: "2026-04-14"
last_edited: "2026-04-15T04:00:00Z"
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
| 4 | 10 | 10 | ✅ COMPLETE (post-gate) |
| 5 | 16 | 16 | ✅ COMPLETE (post-gate, 16 findings fixed across 5 cycles) |
| 6 | 26 | 26 | ✅ COMPLETE (post-gate, 15 findings fixed across 8 cycles) |

**Overall: 134/134 tasks done (100%) · 279 ark-cli tests + workspace green · ALL TIERS COMPLETE**

Tier 6 landed 2026-04-15 across 16 build commits + 8 codex gate cycles. Build order: testing fixtures (T-110–T-113), contract suites (T-114/T-115/T-116), per-crate unit coverage (T-117–T-125), mock-claude (T-126), e2e scenarios (T-127/T-128), CI workflow (T-129), build orchestration (T-130), wasm release profile (T-131), CI size delta (T-132), cargo-dist (T-133), homebrew/binstall (T-134), standalone wasm assets (T-135). Gate fixed F-700 through F-714 (workflow_dispatch tag handling, flaky test, status plugin permission split, build.rs incremental, --no-detach state cleanup, doctor exit-code, dead-code warnings, manifest mtimes, brew tap owner). TIER_6_START_REF = 314cfbf → final HEAD = fef98dd.

Tier 4 landed 2026-04-15 across commits 3e681da → 1a03779 (build) + 10 codex gate cycles 3e681da → 0fc47dd. Build order: T-084 scaffold, T-086 (pre-existing), T-085 exit-codes, T-085-fdn CliError expansion (foundation), T-092 pane routing, T-093 env-vars, T-089 kill, T-090 config, T-088 list, T-091 doctor, T-087 spawn. Codex gate fixed 30 findings (F-500–F-529) across 10 cycles. Cycle 10 returned zero P1s — gate closed. TIER_4_START_REF = 538fa42.

Deferrals from T-087 spawn (noted in commit bodies, picked up in Tier 5/6): supervisor-binary launch (waits on T-062/T-069 binary target), --no-detach log-tail (waits on supervisor), file lock $STATE/locks/{id}.lock (T-064). Layout KDL rendering IS implemented (F-525). Zellij session creation IS implemented (F-516).

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
| cli | 10 | TIER-4 COMPLETE (pre-gate) — T-084 scaffold, T-085 exit-codes, T-086 id-resolver, T-087 spawn (partial, supervisor-launch stubbed), T-088 list, T-089 kill, T-090 config, T-091 doctor, T-092 pane routing, T-093 env-vars | (pending) |
| plugin-status | 5 | TIER-5 COMPLETE (pre-gate) — T-094 scaffold, T-095 ingest+cache, T-096 chip render, T-097 fs fallback, T-098 distribution | (pending) |
| plugin-picker | 11 | TIER-5 COMPLETE (pre-gate) — T-099 scaffold, T-100 state model, T-101 bootstrap, T-102 list, T-103 detail, T-104 new-agent, T-105 kill/rename/forget, T-106 resurrect, T-107 switch_session, T-108 keymap+help, T-109 distribution | (pending) |
| testing | 16 | TIER-6 COMPLETE — fixtures (T-110–T-113), contract suites (T-114/T-115/T-116), unit coverage per crate (T-117–T-125), mock-claude (T-126), e2e (T-127/T-128) | (pending) |
| distribution | 6 | TIER-6 COMPLETE — CI workflow (T-129), build.rs wasm orchestration (T-130), release profile + wasm-opt (T-131), CI size watch (T-132), cargo-dist (T-133), homebrew/binstall (T-134), standalone wasm assets (T-135). Wasm sizes: status 873KB→748KB w/ wasm-opt; picker 997KB→861KB. | (pending) |

## Tooling Notes

- ck:task-builder subagent broke twice (narration loop). general-purpose subagents work reliably for parallel packet execution.
- Inline execution used for small (S-effort) tasks where dispatch overhead exceeds work.
- Caveman mode active for build phase status reports (per preset config).
