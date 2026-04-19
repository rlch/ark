---
created: "2026-04-14"
last_edited: "2026-04-18"
---

# Plan Overview

## v0.1 STATUS — tag-eligible as of 2026-04-18

- Phase 1 (soul foundations): DONE
- Phase 2 (soul surfaces): DONE 45/45
- claude-code-ext: DONE 48/48
- Cleanup (Phase 4+5 merged): DONE 12/12
- Scene 2026-04-18 revision: DONE 26/26

Final workspace: 2203 tests pass / 4 ignored / 0 fail. cargo fmt clean. PTY smoke green.

v0.2 carry-forward backlog captured in `context/impl/impl-claude-code-ext.md` ledger.

## Build Sites
| Site | File | Tasks | Done | Status |
|------|------|-------|------|--------|
| soul Phase 2 | build-site-soul-phase-2.md | 45 | 45 | DONE 2026-04-18 |
| claude-code-ext | build-site-claude-code-ext.md | 48 | 48 | DONE 2026-04-18 (T-026 deleted per R-14) |
| cleanup (Phase 4+5) | build-site-cleanup.md | 12 | 12 | DONE 2026-04-18 |
| scene 2026-04-18 | build-site-scene-2026-04-18.md | 26 | 26 | DONE 2026-04-18 (T-004 CUT per R-8) |
| ark v1 | build-site.md | 135 | 0 | DRAFT |
| supervisor wiring (Phase 7) | build-site-supervisor-wiring.md | 6 | 0 | DRAFT — pipe-inheritance ready signal; in-process daemon model |
| mux tight-coupling revision | build-site-mux-tight-coupling.md | 13 | 0 | DRAFT — delete Multiplexer trait (no narrow-trait replacement); concretize ZellijMux; relocate status_pipe to supervisor; ZellijMux::for_test constructor; drop TmuxMux |
| Scene + Extensions (v3 full) | build-site-scene.md | 140 | 0 | DRAFT — partially superseded by scene-2026-04-18. 16 tiers across 5 milestones: v0.1 Scene DSL + reconciler (73 tasks), v0.2 extensions (32), v0.3 ACP + composition (17), v0.4 hot reload + CLI polish (16), v1.0 freeze (5). Ark-native layout DSL, views replace plugins, Rhai expression-only mode (non-TC; CEL + minijinja both removed 2026-04-16), reconciler via override-layout + env ARK_HANDLE wrapper, extensions unified (3 delivery modes), agent-as-extension-capability, include-only composition, code-generated manifest via Rust derives. |
