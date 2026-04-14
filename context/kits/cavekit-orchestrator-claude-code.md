---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Orchestrator — Claude Code (Passthrough)

## Scope
The `ClaudeCodeOrchestrator` — methodology-free passthrough. Opens a single builder tab running the user's `claude` command. Relies entirely on `ClaudeCodeEngine` for observability. No additional FS watchers, no review tab, no phase tracking.

## Requirements

### R1: Detection
**Description:** Default fallback — always applicable when nothing else matches.
**Acceptance Criteria:**
- [ ] `detect(cwd)` returns true when no other orchestrator detects and `claude` is on PATH (last-resort match)
- [ ] Does not self-assert when cavekit or future orchestrators match — auto-detect picks the most specific
**Dependencies:** cavekit-cli

### R2: Minimal tab graph
**Description:** One builder tab; no additional panes spawned programmatically.
**Acceptance Criteria:**
- [ ] `engine()` returns `"claude-code"`
- [ ] Default layout: `config.orchestrator.claude_code.default_layout` (default `"classic"`)
- [ ] `run` opens the builder tab and waits on engine Stop or cancel
- [ ] No side-effect watchers beyond what the engine provides
- [ ] Events forwarded as-is from engine; orchestrator adds no additional AgentEvents (except Started / TabOpened / Done that supervisor owns)
**Dependencies:** cavekit-mux-zellij, cavekit-layouts

### R3: Done and cancel
**Description:** Done is delegated entirely to the engine's Stop signal.
**Acceptance Criteria:**
- [ ] On engine `Stop` or `SessionEnd`: return `Outcome::Success { artifacts: diff_paths }` where diff_paths comes from optional git diff (tolerated missing — we don't require the cwd to be a git repo for claude-code orchestrator)
- [ ] On `world.cancel`: close tab, return `Outcome::Killed`
- [ ] Non-git cwd is valid — no assumption of `.git/`
**Dependencies:** cavekit-types-state-events

## Rationale
Provides the escape hatch for "just run claude in a tab, observe hooks + transcript, no methodology." Useful for:
- Quick one-off sessions outside cavekit workflows
- Experimenting with claude features
- Testing the engine-only observability path
- Users who don't use cavekit

## Out of Scope
- Any methodology-level features (phases, loops, reviews)
- Directing claude input programmatically
- Driving claude from a config script

## Cross-References
- cavekit-architecture.md R2
- cavekit-engine-claude-code.md — sole source of observability
- cavekit-orchestrator-cavekit.md — richer sibling using same engine
- cavekit-layouts.md — `classic.kdl` shipped default for this orchestrator
- cavekit-config.md — `[orchestrator.claude_code]`
