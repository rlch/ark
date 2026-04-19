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
- Supervisor wiring (Phase 7): DONE 6/6
- Mux tight-coupling revision: DONE 13/13
- Scene v3 full audit: DONE 148/148 (17 pending items landed this session; 121 DONE pre-audit; 4 SUPERSEDED; 9 CUT)
- ark v1 audit: DONE 135/135 (91 DONE / 23 SUPERSEDED / 20 CUT / 1 cosmetic PARTIAL; zero pending)

**All mapped build sites CLOSED. No genuinely-pending work items remain.**

Final workspace: 2358 tests pass / 4 ignored / 0 fail. cargo fmt clean. PTY smoke green.

## v0.2 STATUS — backlog CLOSE-OUT as of 2026-04-18

7 backlog items from `context/plans/v0.2-backlog.md` closed: #1 ark-bus bridge, #2 PaneAttrs+spawn_pane RPC, #3 SubagentRegistry auto-wire, #4 `ark ext <name> <verb>` CLI, #5 ext_state persistence, #6 PTY harness (MVP scaffold), #7 cc-hook cargo-install fallback. Remaining v0.2 scope: DRAFT-site initiatives #8-#11 (all are new-scope kits, not carry-forward).

## Build Sites
| Site | File | Tasks | Done | Status |
|------|------|-------|------|--------|
| soul Phase 2 | build-site-soul-phase-2.md | 45 | 45 | DONE 2026-04-18 |
| claude-code-ext | build-site-claude-code-ext.md | 48 | 48 | DONE 2026-04-18 (T-026 deleted per R-14) |
| cleanup (Phase 4+5) | build-site-cleanup.md | 12 | 12 | DONE 2026-04-18 |
| scene 2026-04-18 | build-site-scene-2026-04-18.md | 26 | 26 | DONE 2026-04-18 (T-004 CUT per R-8) |
| ark v1 | build-site.md | 135 | 135 | DONE 2026-04-18 (audit) — `context/impl/impl-build-site-ark-v1-audit.md` resolves all 135 tasks: 91 DONE, 23 SUPERSEDED (phase-2 / claude-code-ext / cleanup / mux-tight-coupling replaced), 20 CUT (ACP, orchestrator traits, Multiplexer trait, ark-hook sidecar, ark spawn, permission module — all per 2026-04-18 pivot + Cleanup Packet A/B + mux M-7/M-8), 1 PARTIAL (T-104 picker wording drift — cosmetic). Zero genuinely-pending tasks. Audit commit `2ee9d3b`. |
| supervisor wiring (Phase 7) | build-site-supervisor-wiring.md | 6 | 6 | DONE pre-2026-04-18 — pipe-inheritance ready signal, in-process daemon model all shipped. Tracking doc: `context/impl/impl-supervisor-wiring.md` (Phase 7 COMPLETE — all 8 shipped tasks landed including W-1 supervisor_main, W-2 ReadyWriter, W-3 launch.rs daemonize fork, W-4 --no-detach variant, W-8 e2e tests, W-9 impl tracking). plan-overview row flipped 2026-04-18 after state audit confirmed. |
| mux tight-coupling revision | build-site-mux-tight-coupling.md | 13 | 13 | DONE 2026-04-18 — Multiplexer trait + mux_contract suite + status_pipe relocation + MockMux/StubMux/NoopMux helpers + TmuxMux roadmap entries all landed (12/13 pre-auto-resolved by cleanup Packet A + scene-v3 work pre-2026-04-18; M-5 kit line reworded in this audit pass). ZellijMux concrete, `for_test` constructor live (test-support feature). Audit confirmed grep-clean outside scene-local MuxHandle test seam (out of scope) and cli launch-local test seam (out of scope, documented as launch-crate-internal in `crates/cli/src/commands/launch/traits.rs`). |
| Scene + Extensions (v3 full) | build-site-scene.md | 140 (actually 148 incl. peer-review fixes T-141..T-148) | 148 | DONE 2026-04-18 — scene-v3 audit close-out (`context/impl/impl-scene-v3-audit.md`) resolved 148 task rows: 121 DONE (via phase-2 soul + scene-2026-04-18 + prior scene work), 4 SUPERSEDED (phase-2 ark-view replaced scene-local types), 9 CUT (all Tier 11 ACP per 2026-04-18 pivot), 0 PARTIAL, 0 PENDING after audit's 17 packets S-A..S-H landed (reconciler drift test, keybind MessagePlugin compile, ark-bus intent/emit rewire, reload wiring primitives, facet SHAPE migrations, v1 strict mode, layout migration, wasm transport scaffold). Ark-native layout DSL, Rhai expression-only (CEL+minijinja removed), reconciler via override-layout + env ARK_HANDLE wrapper, extensions unified 3 delivery modes, agent-as-extension-capability, include-only composition, code-generated manifest via Rust derives — all functional. |
