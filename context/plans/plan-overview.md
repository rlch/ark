---
created: "2026-04-14"
last_edited: "2026-04-16"
---

# Plan Overview

## Build Sites
| Site | File | Tasks | Done | Status |
|------|------|-------|------|--------|
| ark v1 | build-site.md | 135 | 0 | DRAFT |
| supervisor wiring (Phase 7) | build-site-supervisor-wiring.md | 6 | 0 | DRAFT — pipe-inheritance ready signal; in-process daemon model |
| mux tight-coupling revision | build-site-mux-tight-coupling.md | 13 | 0 | DRAFT — delete Multiplexer trait (no narrow-trait replacement); concretize ZellijMux; relocate status_pipe to supervisor; ZellijMux::for_test constructor; drop TmuxMux |
| Scene + Extensions (v3) | build-site-scene.md | 140 | 0 | DRAFT — regenerated 2026-04-16 from cavekit-scene v3. 16 tiers across 5 milestones: v0.1 Scene DSL + reconciler (73 tasks), v0.2 extensions (32), v0.3 ACP + composition (17), v0.4 hot reload + CLI polish (16), v1.0 freeze (5). Ark-native layout DSL, views replace plugins, Rhai expression-only mode (non-TC; CEL + minijinja both removed 2026-04-16), reconciler via override-layout + env ARK_HANDLE wrapper, extensions unified (3 delivery modes), agent-as-extension-capability, include-only composition, code-generated manifest via Rust derives. |
