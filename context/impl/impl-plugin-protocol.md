---
created: "2026-04-20"
last_edited: "2026-04-20"
---

# Implementation Tracking: Plugin Protocol (ark-native wasm-component)

Build site: context/plans/build-site-plugin-protocol.md

Cavekit: context/kits/cavekit-plugin-protocol.md (R1-R14)

Tier start refs:
- Tier 0 start: `fac6223`
- Tier 1 start: `9b46ec3`
- Tier 2 start: `92fb67d`
- Tier 3 start: `f6f18d7`

| Task | Status | Notes |
|------|--------|-------|
| T-PP-001 | DONE | crates/ark-plugin-protocol scaffolded (9b46ec3) |
| T-PP-002 | DONE | crates/ark-plugin-sdk proc-macro skeleton (9b46ec3) |
| T-PP-003 | DONE | crates/config plugins_kdl.rs structural parse (9b46ec3) |
| T-PP-004 | DONE | crates/ark-host with 6 module stubs (9b46ec3) |
| T-PP-005 | DONE | crates/ark-render-terminal with materialize stub (9b46ec3) |
| T-PP-006 | DONE | ARK_ABI_VERSION + SUPPORTED_PLUGIN_ABIS + AbiError in ark-types (9b46ec3) |
| T-PP-007 | DONE | workspace dep pins: wasmtime/wasmtime-wasi 43, wasmparser-plugin-host 0.247, postcard 1, notify 6, sha2 0.10 (9b46ec3) |
| T-PP-008 | DONE | PluginLoadError with stable codes for R3/R5/R6/R8/R9/R12/R14 (9b46ec3) |
| T-PP-009 | DONE | Target enum `#[non_exhaustive]` + host_target() stub (9b46ec3) |
| T-PP-010 | DONE | Intent/IntentTarget/PipeMessage/BusError/PipeSource defined (9b46ec3) |
| T-PP-011 | DONE | examples/echo/ placeholder (not workspace member) (9b46ec3) |
| T-PP-012 | DONE | wit/plugin.wit + wit/widget-tree.wit (92fb67d). install-event/host-event/pipe-message in `interface types`; widget-tree in `interface widget-tree-types` — WIT disallows top-level type decls in world |
| T-PP-013 | DONE | install-event 4-arm + reserved-future(u32) sentinel (92fb67d) |
| T-PP-014 | DONE | terminal-widget-tree recursive variant (text/row/column/box-node/spacer/cursor); gui arm reserved as comment (92fb67d) |
| T-PP-015 | DONE | build.rs plain-text lint: ark:host/*, ark:cap/*, types, widget-tree-types whitelisted; no wasi:cli/environment; rerun-if-changed (92fb67d) |
| T-PP-016 | DONE | CapsManifest postcard schema + CAPS_SECTION_NAME="ark-caps:v1" + tests (92fb67d) |
| T-PP-017 | DONE | MetaManifest hand-rolled name-regex + semver + ABI check; META_SECTION_NAME="ark-meta:v1" (92fb67d) |
| T-PP-018 | PARTIAL | echo example: cdylib + wit_bindgen::generate! + 5 lifecycle stubs; `#[cfg(target_arch = "wasm32")]` gated; sections wired in T-PP-022 |
| T-PP-019 | DONE | #[derive(Plugin)] caps half — compile-time postcard bytes, #[link_section="ark-caps:v1"] static (f6f18d7) |
| T-PP-020 | DONE | #[derive(Plugin)] meta half — name regex + semver + ABI-equality compile-time enforced, #[link_section="ark-meta:v1"] static (f6f18d7) |
| T-PP-021 | DONE | WIT-world-name compile-check — reads CARGO_MANIFEST_DIR, parses `world <name>`, compile error on mismatch (f6f18d7) |
| T-PP-022 | DONE | echo wired: #[derive(Plugin)] with name="plugin" sharing wit dir; tests/echo_sections.rs #[ignore]d gate (needs wasm32-wasip2 target) (f6f18d7) |
| T-PP-023 | DONE | 4 trybuild compile-fail fixtures (invalid_name, invalid_semver, abi_mismatch, world_name_mismatch) pinned stderr (f6f18d7) |
| T-PP-024 | DONE | wit_doc_comments.rs — load + on-install literals checked, forbidden-hook grep (deactivate/on-unload/pre-shutdown) (f6f18d7) |
| T-PP-025..T-PP-040 | PENDING | Tier 3 — wasmtime substrate + capability infrastructure (16 tasks, 3 waves) |

## Deferrals recorded this tier
- ark-plugin-sdk: empty proc-macro crate, real derive logic in T-PP-019/T-PP-020
- ark-host modules: doc-stubs pointing at Tier 3 (T-PP-025..T-PP-046)
- ark-render-terminal: placeholder `materialize(&(), u16, u16) -> Vec<u8>` signature; real `TerminalWidgetTree` in T-PP-014/T-PP-047
- config/plugins_kdl: grammar-only; semantic validation (closed cap set, URL scheme, Levenshtein) deferred to T-PP-037
- wasmparser pinned as alias `wasmparser-plugin-host` 0.247 to coexist with scene's 0.246 pin; unify when scene next touches its dep

## Tier 3B Codex findings — FIXED 2026-04-20

Post-066d9b4 (Tier 3B) peer review surfaced three P1 findings against R2, R4, R14:

- **F-433 / F-436 (P1)** — `crates/ark-host/src/linker_set.rs::build_one_variant()` called the world-level `Plugin::add_to_linker`, registering all 6 cap fns in every variant. That made the deny-all + partial-cap variants still expose fs-read/fs-write/network/spawn-process/bus-send/bus-receive — Approach C by accident, not R4 Approach B. FIXED by rewriting `build_one_variant(caps: &CapsKey)` to use per-interface `add_to_linker` calls (`log::add_to_linker`, `clock::add_to_linker`, `plugin_id::add_to_linker`, `types::add_to_linker`, `widget_tree_types::add_to_linker` unconditional; each `ark:cap/*` gated on `caps.contains("<id>")`). New `crates/ark-host/tests/linker_cap_gate.rs` proves the gate: a synthetic component importing `ark:plugin/fs-read@1.0.0` fails `linker_empty.instantiate_pre(&component)` and succeeds on `linker_fs_read.instantiate_pre(&component)`. A second test proves granted caps don't leak (fs-read variant refuses a network-importing component).
- **F-434 / F-437 (P1)** — `world plugin` in `wit/plugin.wit` mandated every cap import at the world level. FIXED by splitting into `world plugin-base` (what plugin authors extend — only `ark:host/*` imports + lifecycle exports + shared type refs) and `world plugin-host` (the maximal world the host binds against — `include`s `plugin-base` + imports every `ark:cap/*`). Host `bindings.rs` switched to `world: "plugin-host"`; per-interface `add_to_linker` helpers are generated for the conditional wiring. The echo example now owns `wit/echo.wit` (`package ark:echo@0.1.0; world echo { include ark:plugin/plugin-base@1.0.0; import ark:plugin/fs-read@1.0.0; }`) and uses the canonical `wit/deps/ark-plugin/` layout to resolve the shared package. `#[derive(Plugin)]` attribute updated to `name = "echo", wit = "wit/echo.wit"`.
- **F-435 / F-438 (P1)** — the `resource terminal-node` + `list<terminal-node>` indirection in `widget-tree.wit` (forced by wit-parser 0.245's type-graph toposort rejecting `list<variant>` recursion) was a breaking change against `ark:plugin@0.1.0`. FIXED by bumping the WIT package to `ark:plugin@1.0.0` across both `plugin.wit` + `widget-tree.wit`. `ARK_ABI_VERSION` STAYS AT 1 — the on-wire widget-tree shape change is a pre-1.0 scaffolding event (no binary shipped) and R14 explicitly distinguishes WIT semver from binary ABI version. Kit R14 now carries a new acceptance criterion making the independence explicit.

Workspace tests: `cargo test --workspace -- --test-threads=1` → 2428 passing, 23 ignored (baseline 2426 → +2 for the new `linker_cap_gate.rs` tests). `cargo test -p ark-host` 26 passing (24 → 26). `cargo test -p ark-plugin-protocol` 25 passing, 1 ignored (unchanged). `cargo test -p ark-plugin-sdk` trybuild fixtures unchanged (no stderr regen needed — validator output didn't shift; the version bump is inside the package preamble, not in error messages).
