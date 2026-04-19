---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: Scene v3 pending tasks

Build site: context/plans/build-site-scene.md
Audit: context/impl/impl-scene-v3-audit.md

Ledger append-only. Newest entries at top.

## Task Status

| Task | Packet | Status | SHA | Notes |
|------|--------|--------|-----|-------|
| T-044 | S-F | DONE | `bd38e35` | reconciler drift-tolerance integration tests (4 added): retain-flags guarantee, quiet-pass reports `predicates_changed=false`, predicate-flip forces convergence, mode-switch preserves retain flags. Uses MockMux-equivalent `RecordingApplier` — no real zellij. |
| T-065 | S-A | DONE | `bd38e35` | `compile::keybinds` module emits `keybinds { shared { bind "<chord>" { MessagePlugin "ark-bus" { name "ark-intent"; payload "<JSON>" } } } }` block. No `clear-defaults=true` (additive merge). `inject_keybinds_if_needed(doc, ir)` injector mirrors `inject_ark_bus_if_needed` contract. 15 unit tests + snapshot fixture + end-to-end KDL re-parse. Also side-effect: `auto_mount.rs` newly registered in `compile/mod.rs` (was compiled-dead). |
