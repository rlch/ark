---
created: "2026-04-20"
last_edited: "2026-04-20"
supersedes_partial: "cavekit-scene.md R10 (capability/extension model — wasm-component delivery mode now governed by this kit)"
---

# Spec: Plugin Protocol — ark-Native Wasm Plugins

## Scope

ark plugins are **wasm component-model components** loaded by ark's own host runtime, not by zellij. Zellij's plugin ABI (load/update/render/pipe via zellij-tile) is no longer ark's plugin substrate; it remains the *terminal multiplexer* substrate for ark today. A future GUI ark uses the same plugins via a different host materializer.

This kit defines:
1. The wasm runtime substrate the host owns (R1).
2. The plugin contract — WIT interfaces (R2), capability declaration (R3), enforcement (R4), user grants (R5).
3. Per-view render-target classification (R6).
4. The five lifecycle hooks (R7).
5. The 3-phase loader (R8).
6. Identity (R9) and abstract widget trees (R10).
7. Intent dispatch on ark-bus (R11).
8. Distribution as bare `.wasm` (R12), dev mode (R13), ABI versioning (R14).

Out of scope for v1 (deferred until use case forces them):
- Hot reload of cap grants (wasmtime issue #3017 + design choice).
- OCI distribution / marketplace / signed publishers (no third-party shipper exists).
- Subprocess and compiled-in extension delivery modes are governed by their own kits (`cavekit-soul-phase-2-ext-surface.md`, future `cavekit-ext-subprocess.md`).
- GUI render target — `GuiView` reserved but no host materializer in v1.
- Plugin pre-shutdown / `deactivate` hooks — sudden-death-safe is the empirically correct posture (Cluster 5 §5.9).

Background research synthesised in `context/refs/plugin-protocol-survey.md` (Clusters 1-6).

## Requirements

### R1: Wasm runtime substrate

**Description:** ark-host owns its own wasm runtime, built on `wasmtime` with the **component-model** API exclusively (`wasmtime::component::*`). Plugins are `wasmtime::component::Component`s — never raw `wasmtime::Module`s, never core wasm. WASI is wired via `wasmtime_wasi::p2` (preview2) for resource-typed, default-deny capability gating. The `Engine` is process-global and shared (it is internally ref-counted, `Clone + Send + Sync`); each loaded plugin owns one `Store<PluginCtx>` containing exactly one `Instance`. Cooperative yield uses **epoch interruption** (~10% slowdown), not fuel — ark needs liveness, not determinism. Per Cluster 3 verdict (refs/plugin-protocol-survey.md "Verdict for ark — concrete API choices").

**Acceptance Criteria:**
- [ ] `ark-host` depends on `wasmtime` with `component-model` + `async` features enabled; depends on `wasmtime-wasi` for `p2` (preview2) only.
- [ ] No code path constructs `wasmtime::Module`, `wasmtime::Instance` (core), or `wasmtime::Linker<T>` (core). Only `wasmtime::component::*` types appear in plugin-loading code.
- [ ] Exactly one `Engine` per ark process. Constructed at first plugin-host access and shared by clone thereafter. Verified by an instrumentation counter on `Engine::new` that asserts ≤1 across the lifetime of an integration test that loads N plugins.
- [ ] `Engine` config asserts `wasm_component_model(true)`, `async_support(true)`, `epoch_interruption(true)` at construction. Any feature flag drift = startup panic with explanatory message.
- [ ] Each loaded plugin owns one `Store<PluginCtx>` containing one `Instance`. No `Store` is shared across plugins or threads (compile-checked: `Store` is `!Sync`).
- [ ] WASI default-deny: every `WasiCtxBuilder` constructed by `ark-host` calls `.allow_tcp(false).allow_udp(false)` *before* any cap-driven additions. Cluster 3 §3.3 footgun: defaults are TRUE — drift here = silent network exfil.
- [ ] WASI default-deny extends to: no preopens, no env vars, no args, stdio muted (or wired to ark-owned sinks), `allow_ip_name_lookup(false)`. Caps R4 may relax these per grant.
- [ ] An epoch-ticker mechanism calls `engine.increment_epoch()` once every ~50 ms for the lifetime of the process (the concrete timer implementation is implementor's choice).
- [ ] Each `Store` is configured at construction with `set_epoch_deadline(2)` + `epoch_deadline_async_yield_and_update(2)` so plugins yield cooperatively every ~100 ms.
- [ ] Fuel-based interruption (`Config::consume_fuel`) is NOT enabled. Test asserts `cfg.consume_fuel(false)` (default).
- [ ] `Component::serialize` / `Component::deserialize` AOT cache is keyed by content hash of the source `.wasm`; deserialize path is reachable from process startup without invoking Cranelift.
- [ ] No plugin instantiation path falls back to `Module::new` even on cache miss; cache miss = `Component::new` (component compile), period.
- [ ] Cross-references: introduces ark-host's *own* wasm runtime, distinct from zellij's plugin runtime. Supersedes the wasm-component branch of cavekit-scene.md R10 (capability + extension delivery model) per this kit's frontmatter and Cross-Reference Summary.

### R2: Plugin WIT world

**Description:** ark defines two WIT interfaces that together form the plugin contract: `ark:plugin/host` (what plugins receive) and `ark:plugin/guest` (what plugins must export). The guest world exports a fixed lifecycle surface (`on-install`, `load`, `update`, `render`, `pipe`). Host imports are namespaced into two trees: `ark:host/*` for unconditional services (logging, time, plugin-id introspection) and `ark:cap/*` for capability-gated services (filesystem, network, process spawn). WIT lives in `crates/ark-plugin-protocol/wit/` and is the single source of truth for both sides — guests use `wit-bindgen` to generate Rust/JS/Go bindings; the host uses `wasmtime::component::bindgen!` to produce typed host trait scaffolding (Cluster 3 §3.4).

**Acceptance Criteria:**
- [ ] WIT package declared as `package ark:plugin@0.1.0;` and lives at `crates/ark-plugin-protocol/wit/plugin.wit` (additional files permitted in same directory; `wit-deps` resolution discouraged for v1).
- [ ] One `world plugin` definition. Guest exports exactly: `on-install: func(reason: install-reason)`, `load: func()`, `update: func(event: update-event) -> bool`, `render: func(view-id: string, w: u32, h: u32) -> result<widget-tree, plugin-error>`, `pipe: func(message: pipe-message) -> bool`. Adding/removing exports = breaking change requiring world bump (and `ARK_ABI_VERSION` bump per R14).
- [ ] Host imports partition cleanly: every interface name starts with either `ark:host/` (unconditional) or `ark:cap/` (capability-gated). Any other prefix in WIT = lint failure in `ark-plugin-protocol`'s build.rs.
- [ ] Unconditional host interfaces (v1): `ark:host/log`, `ark:host/clock`, `ark:host/plugin-id`. Capability-gated interfaces (v1): `ark:cap/fs-read`, `ark:cap/fs-write`, `ark:cap/network`, `ark:cap/spawn-process`, `ark:cap/bus-send`, `ark:cap/bus-receive`. Each `ark:cap/*` interface name matches a capability identifier (R5 grant tokens).
- [ ] Guest bindings: `wit-bindgen` invocation produces compilable Rust output for the example plugin in `crates/ark-plugin-protocol/examples/echo/`.
- [ ] Host bindings: a single `wasmtime::component::bindgen!({ path: "wit", world: "plugin", async: true, trappable_imports: true, with: { ... } })` invocation in `ark-host` produces the host trait scaffolding. `with:` ascribes Rust types for any host-owned resources (Cluster 3 §3.4).
- [ ] WASI dependency: world `imports wasi:cli/environment@0.2.x` and other p2 interfaces via `wasi-deps` are excluded from v1; the host wires WASI via `wasmtime_wasi::p2::add_to_linker_async` separately, not through `ark:plugin` world re-exports.
- [ ] WIT version bumps follow semver: additive change to `ark:host/*` = minor; new `ark:cap/*` interface = minor; renaming or removing any export/import = major; the host refuses to load plugins compiled against a major-incompatible world (introspected via R3).
- [ ] An `examples/echo/` reference plugin compiles to a `.wasm` component, exports the full guest world, and imports only `ark:host/log` + `ark:cap/fs-read`. CI builds it on every push.
- [ ] Cross-references: cavekit-scene.md R10 (extension delivery modes) — this WIT world defines a *fourth* delivery mode, "wasm-component", not yet enumerated in scene R10; scene R10 update tracked in `cavekit-plugin-protocol` follow-on amendment.

### R3: Capability declaration mechanism

**Description:** Capability requirements are declared by every plugin via a **hybrid** of two authoritative sources, per Cluster 4 verdict ("Recommended mechanism: Hybrid 4.3 + 4.1"):
1. The plugin's wasm-component imports under the `ark:cap/<name>` namespace are the **authoritative requirement list** — the host enumerates them via `Component::component_type().imports(&engine)` *before* instantiation. A plugin cannot lie about what symbols it references.
2. A custom section named `ark-caps:v1` carries postcard-encoded display metadata (display name, human-readable reason, since-version) used purely for UX presentation in `ark ext list`, error messages, and future grant prompts.

The host **cross-checks** both: any `ark:cap/*` import without a matching section entry, OR any section entry without a matching import, is a load-time refusal (drift error). Both checks run on raw bytes via `wasmparser::Parser` + `Component::component_type()` — zero plugin code executes before the decision.

**Acceptance Criteria:**
- [ ] On plugin load, host calls `Component::component_type().imports(&engine)` and collects every interface name beginning with `ark:cap/`. This set is named `wanted`.
- [ ] On the same load, host calls `wasmparser::Parser::new(0).parse_all(bytes)` (streaming, no `Component::new` first) and extracts the `ark-caps:v1` custom section payload. Section absent = `error[plugin/manifest-missing]` with remediation `add an ark-caps:v1 section via the ark-plugin-sdk macro`.
- [ ] `ark-caps:v1` payload is postcard-decoded into a `CapsManifest { plugin_name: String, since_version: String, caps: Vec<CapDecl { id: String, display_name: String, reason: String }> }`. Decode failure = `error[plugin/manifest-corrupt]` with the postcard error.
- [ ] Drift check (section ⊃ imports): for each `caps[i].id` in the section, assert there exists a matching `ark:cap/<id>` import. Mismatch = `error[plugin/cap-drift-section-extra]` listing the extra section entries.
- [ ] Drift check (imports ⊃ section): for each `ark:cap/<name>` import, assert there exists a matching `caps[i].id == name`. Mismatch = `error[plugin/cap-drift-import-extra]` listing the undeclared imports.
- [ ] Both drift errors include the plugin file path and remediation: `the plugin's compiled imports do not match its ark-caps:v1 section. report this as a bug to the plugin author`.
- [ ] Section read happens *before* `Component::new` (which invokes Cranelift). Verified by benchmark: section read on a 5 MiB plugin completes in <1 ms; full compile takes >10 ms.
- [ ] Drift error refuses load: no `Store` allocated, no instance constructed.
- [ ] Section name `ark-caps:v1`. Future schema bumps use a new section name (`ark-caps:v2`) per Cluster 4 §4.1 pattern (3) — new hosts try v2 first, fall back to v1 for at least one major version.
- [ ] A single `#[derive(Plugin)]` macro in `ark-plugin-sdk` emits BOTH the `ark-caps:v1` section (R3) and the `ark-meta:v1` section (R9) at build time via `#[link_section]`. Plugin authors never hand-encode either section. The macro's input lists name, version, abi, and capabilities each with its display-name + reason string.
- [ ] `ark ext inspect <path-or-name>` CLI command prints the parsed manifest + cross-checked import list without instantiating the plugin. Path argument inspects a `.wasm` file directly; name argument resolves through the registered-plugin set in `ark.kdl`.
- [ ] Cross-references: supersedes any earlier informal "manifest = sidecar TOML" approach; cavekit-scene.md R10 capability/extension model gains a new wasm-component branch governed by R3-R5.

### R4: Capability enforcement

**Description:** Wasmtime forbids per-instance host-fn swap once a `Linker` is built (Cluster 3 §3.2 finding). The enforcement model is therefore **per-cap-profile linker variants**: at startup, the host pre-computes a `LinkerSet` containing one `Linker<PluginCtx>` per granted-cap permutation that actually appears in any plugin's user grant set. For each loaded plugin, the host caches an `InstancePre<PluginCtx>` keyed by `(component-content-hash, granted-cap-set)`. At instantiation, the host selects the linker matching the user's granted caps; the chosen `InstancePre` is reused across re-instantiations. Calls to host functions for caps not in the chosen linker variant fail at `instantiate_pre` (the import does not type-check) — *before* any guest code runs. Default-deny WASI is enforced in `WasiCtxBuilder` construction (R1) and tightened per granted cap.

**Acceptance Criteria:**
- [ ] At startup, host scans every plugin's declared caps (via R3) and computes the set of distinct cap permutations present. One `Linker<PluginCtx>` is built per permutation. The empty-cap linker (WASI default-deny + `ark:host/*` only) is always present.
- [ ] No plugin ever causes a *new* linker to be built mid-session: the LinkerSet is closed at startup. New permutations require ark restart (matches R5 "user edits ark.kdl, restarts ark").
- [ ] Per-cap-profile linker construction wires *only* the granted `ark:cap/*` interfaces. Ungranted interfaces are absent — not stubbed as traps. Cluster 3 §3.2 verdict: traps are the wrong granularity for cap gating.
- [ ] `InstancePre<PluginCtx>` cache key is `(content-hash(component-bytes), CapsKey::from(granted))` where content-hash is a host-internal collision-resistant hash. Cache lookup precedes `Linker::instantiate_pre`. Verified by integration test: instantiating the same (plugin, caps) pair twice in a row produces 1 `Linker::instantiate_pre` call total (second is cache hit).
- [ ] On instantiation: host computes `granted = user-grants ∩ wanted` (where `wanted` comes from R3 import scan); selects the matching linker variant; pre-instantiates against it; stores `InstancePre` in cache; instantiates against the active store.
- [ ] If `granted ⊊ wanted`: load is refused at the R5 cap-grant check, before the linker is selected. If somehow reached, `instantiate_pre` errors with `unknown import ark:cap/<name>` — the missing-cap path traps loud, never silently.
- [ ] WASI gating via `WasiCtxBuilder`: only granted caps add anything beyond the R1 default-deny baseline. Specifically: `fs-read` adds preopens with `DirPerms::READ + FilePerms::READ`; `fs-write` adds `DirPerms::all() + FilePerms::all()` for the same paths; `network` calls `.allow_tcp(true).allow_udp(true).allow_ip_name_lookup(true)`; `spawn-process` does not touch WasiCtx (uses ark-owned `ark:cap/spawn-process` interface, not `wasi:cli/environment`).
- [ ] Per-resource fine-grain checks (e.g., fs path allowlist within a granted preopen) live inside the host fn body using approach B from Cluster 3 §3.2 — the linker variant is the coarse gate, in-fn checks the fine gate. In-fn denial returns `wasmtime::Error`, not a trap.
- [ ] Test: a plugin declaring `ark:cap/fs-read` but granted nothing fails at `instantiate_pre` with the missing-import error (verified by integration test).
- [ ] Test: a plugin declaring `ark:cap/network` and granted `network` successfully connects to a localhost test server; the same plugin without the grant fails at instantiation, never reaches the connect call.
- [ ] Test: `LinkerSet::for_caps` is a constant-time lookup (HashMap on `CapsKey`); benchmark asserts <100 ns per lookup.
- [ ] Cluster 3 §3.2 approach C (`define_unknown_imports_as_traps`) is NOT used for cap gating. It may appear in test fixtures only; production code path forbids it (lint).
- [ ] Cross-references: cavekit-scene.md R10 capability section is superseded for the wasm-component delivery mode; the linker-variant model is wasm-specific and does not apply to compiled-in or subprocess extensions.

### R5: User capability grant in ark.kdl

**Description:** Capabilities are granted explicitly by the user in their `ark.kdl` config, inside a per-plugin `plugins {}` block. There are **no interactive prompts**, **no runtime grant requests**, **no auto-elevation**. If a plugin's required caps (R3 import scan) are not a subset of the user's granted caps, the host refuses to load with a structured, actionable error showing the missing caps and the exact text the user must add to their `ark.kdl`. The user edits the file, restarts ark, and retries. This matches the iOS/Android install-time-refusal pattern surveyed in Cluster 6 ("install-time hard refusal is the right move").

KDL schema:

```kdl
plugins {
    claude-code location="file:./plugins/claude-code.wasm" {
        capabilities {
            fs-read
            fs-write
            spawn-process
            network
        }
    }
}
```

**Acceptance Criteria:**
- [ ] `ark.kdl` admits a top-level `plugins { ... }` block. Body contains zero or more plugin entries: `<plugin-name> location="<url>" { capabilities { ... } }`.
- [ ] `<plugin-name>` is a bare identifier matching `[a-z][a-z0-9-]*`. Duplicate names = `error[ark-kdl/plugin-name-clash]`.
- [ ] `location=` is a URL with scheme `file:` (v1; `https:` and `oci:` are post-v1). The path resolves relative to the `ark.kdl` file's directory unless absolute. Missing or unreadable file at startup = `error[plugin/location-unreachable]`.
- [ ] `capabilities { ... }` body contains zero or more bare-identifier child nodes, one per granted cap. Identifiers must match the closed set of `ark:cap/*` interface names defined in R2 (v1: `fs-read`, `fs-write`, `network`, `spawn-process`, `bus-send`, `bus-receive`). Unknown cap = `error[ark-kdl/unknown-capability]` with Levenshtein suggestions from the closed set.
- [ ] Capabilities block omitted entirely = empty grant set (equivalent to `capabilities {}`).
- [ ] On plugin load: host computes `wanted = { name | ark:cap/<name> ∈ component imports }` (R3); `granted = { c | c ∈ plugins.<name>.capabilities }` (this R5). If `wanted ⊄ granted`, host refuses load with `error[plugin/cap-not-granted]`.
- [ ] The `cap-not-granted` error message lists every missing cap and the exact KDL remediation:
  ```
  ark: refused to load plugin "claude-code"
    requested capabilities not granted: spawn-process, network
    fix: add the following to your ark.kdl:
      plugins {
          claude-code {
              capabilities {
                  spawn-process
                  network
              }
          }
      }
  ```
- [ ] Refusal is hard: no `Store` allocated, no instance constructed, no plugin code runs.
- [ ] No interactive prompt path exists in the code. `ark-host` has no TTY-grant code, no socket-grant code, no future-grant-promise code. Verified by grep of source for "prompt"/"grant_request"/"ask_user".
- [ ] No runtime cap-elevation API exists. The plugin cannot call `ark:host/request-capability` — no such interface is defined in R2. Adding one = explicit kit revision.
- [ ] `ark ext doctor` CLI command lists every plugin in `ark.kdl`, its declared caps (from R3 import scan), its granted caps (from R5 KDL), and any drift; exits non-zero if any plugin would fail to load.
- [ ] `ark ext inspect <name>` shows the same data for one plugin without attempting to load others (also accepts a `.wasm` path per R3).
- [ ] User edits `ark.kdl` to add a missing cap, runs `ark` again — plugin now loads successfully (verified by integration test that mutates the KDL between runs).
- [ ] Hot-reload of `ark.kdl` cap-grant changes is OUT OF SCOPE for v1: cap changes require ark restart. Documented in this kit's "Out of Scope" preamble; not delegated to scene's hot-reload kit.
- [ ] **Per-plugin runtime config block** (used by R7 + R10 acceptance criteria): `plugins.<name>.runtime { update-failure-budget=N; render-budget-ms=M }` — both keys optional with defaults `update-failure-budget=16`, `render-budget-ms=16`. Verified by KDL parser test.
- [ ] Cross-references: cavekit-scene.md R10 capability model (this R5 supersedes the prior implicit "extensions inherit ark's full ambient authority" assumption for the wasm-component delivery mode). Hot-reload exclusion is owned by this kit, not scene's hot-reload model.

### R6: View types are typed and render-target-bound

**Description:** A **view** is what fills a pane (continuing the definition from `cavekit-scene.md` R6). Plugin-protocol introduces a NEW orthogonal axis on top of the existing `cavekit-soul-phase-2-ark-view.md` R3 trio (`View` / `CommandView` / `ZellijView`, which classify *render mode* — subprocess vs wasm): the **render-target axis** classifies which host materializer can render the view (`TerminalView` for terminal hosts, `GuiView` reserved for a future GUI host). The two axes are independent — a view is `(render-mode, render-target)`, e.g. `(ZellijView, TerminalView)` or `(CommandView, TerminalView)`.

The render-target marker traits live in the new `crates/ark-plugin-protocol/` crate (NOT in `ark-view` — that kit is locked at v0.1; the new axis lives in the new crate). The plugin's WIT world declares which view types it provides via exported types — there is no plugin-level target classification, classification lives at the view level.

A plugin loads as long as **at least one** of its declared views matches the host's active render target. Views whose target the host cannot render are simply unavailable (logged but not fatal). This supersedes Cluster 6's plugin-level `runs-on` enum: ark uses Cluster 1's per-view classification because plugins routinely mix several view types and a single plugin-level enum would force false-binary "GUI-only" labels on otherwise-mixed plugins. The host's active render target is fixed at process start and may not change for the lifetime of the session.

**Acceptance Criteria:**
- [ ] Two render-target marker traits exist in `crates/ark-plugin-protocol/`: `TerminalView: View` (v1) and `GuiView: View` (reserved, no host materializer in v1). `View` is the existing base trait re-exported from `ark-view` per `cavekit-soul-phase-2-ark-view.md` R3; the new traits refine it on the orthogonal target axis. Verified by the new crate's pub items including both traits and `ark-view` requiring no edits.
- [ ] Every view type a plugin exports is the WIT-projection of a Rust type that implements exactly one render-target marker. A view that implements none = `error[plugin/view-no-target]` at host introspection. A view that implements both = `error[plugin/view-multi-target]`. Verified by trybuild compile-fail tests.
- [ ] The plugin's component WIT world enumerates view-type exports as typed records, not opaque strings. The host extracts the export list via the component-model export-introspection API and classifies each export by its declared marker trait through the WIT type metadata.
- [ ] Loading rule: plugin loads iff `provided_views.any(|v| v.target == host.active_target)`. Zero matches = `error[plugin/no-renderable-views]` with the plugin's declared targets enumerated in the diagnostic. One or more matches = load proceeds; non-matching views are dropped silently from the plugin's view registry with a single warn-level log per view.
- [ ] There is no plugin-manifest field naming a target. The `ark-meta:v1` (R9) and `ark-caps:v1` (R3) section schemas have no target field. Target classification lives only on the per-view WIT export type. This explicitly supersedes Cluster 6 §6's `runs-on "terminal" "gui"` recommendation.
- [ ] The host's active render target is a single value, fixed at process start (not per-session, not per-tab). v1 = `Target::Terminal`. Verified by `Target` being a `Copy + Eq` enum exposed once on the `Host` handle and an integration test that asserts `host.target()` returns the same value across two arbitrary call sites in the same process.
- [ ] When a plugin's only view targets `GuiView` and the host is `Terminal`, the loader emits exactly one warn-level log line of the form `plugin "<name>" provides only GuiView; unavailable on terminal target` and refuses load. The plugin appears in `ark ext list` greyed with the same reason string (mirrors VS Code refusal UX per Cluster 2 §2.4).
- [ ] A plugin providing `(TerminalView, GuiView)` views loads on either host, with the unavailable view dropped from the registry. Scene `pane @h { foo }` referring to a dropped view = `error[scene/view-unavailable]` at `ark scene check`, NOT a runtime error.
- [ ] Adding a new render-target variant to the `Target` enum is a MAJOR-version break of `ark-plugin-protocol` (and bumps `ARK_ABI_VERSION` per R14). `Target` is `#[non_exhaustive]` so downstream materializers cannot exhaustive-match without a catch-all.
- [ ] The view-target axis is independent of `cavekit-soul-phase-2-ark-view.md` R3's `CommandView`/`ZellijView` render-mode axis. A view may be `(TerminalView, CommandView)`, `(TerminalView, ZellijView)`, `(GuiView, ZellijView)`, etc. The two axes never collapse. Verified by trybuild matrix.
- [ ] Cross-reference: this requirement supersedes the Cluster 6 §6 verdict (single `runs-on` enum on the manifest); see also `cavekit-scene.md` R6 (view-alias resolution) and `cavekit-soul-phase-2-ark-view.md` R3 (orthogonal render-mode axis — base `View` trait re-exported by ark-plugin-protocol).

### R7: Lifecycle hooks

**Description:** A plugin exports exactly five WIT-defined lifecycle functions: `on-install`, `load`, `update`, `render`, `pipe`. There is no `deactivate` / `on-unload` — every surveyed system that shipped one removed it (Chrome MV3 explicitly designed it away, Zellij never had one, wasmCloud rejected it; per Cluster 5 §5.9 "sudden death is the only safe assumption"). State must reconstruct from host-side `Resource<T>` handles or filesystem on every `load()`.

The host enforces failure policies per hook so a flaky plugin never silently crashes the session: `load()` failure unloads, every other failure logs and keeps the plugin alive. The one hook that carries a reason enum is `on-install`, mirroring Chrome's `OnInstalledReason` (the only production-shipped reason enum in the survey) plus a `Reload` arm for dev-mode (R13).

**Acceptance Criteria:**

**Hook surface (WIT exports):**
- [ ] `on-install(install-event) -> result<_, plugin-error>` — called once per (plugin-id, version, host-installation-uuid) tuple, plus once per dev-mode reload (R13). Idempotent. Fires before the `load()` of the activation that triggered it.
- [ ] `load() -> result<_, plugin-error>` — called once per instance after Component instantiation, before any `update`/`render`/`pipe`. The place to subscribe to events and pull initial state from `Resource<T>` handles.
- [ ] `update(event) -> result<bool, plugin-error>` — called once per event the plugin subscribed to. Return `true` requests a `render`; `false` is the no-op fast path (mirrors Zellij's convention per Cluster 5 §5.7).
- [ ] `render(view-id, w, h) -> result<widget-tree, plugin-error>` — called per repaint of a specific view; returns the `widget-tree` defined in R10. `view-id` selects which of the plugin's exported view types to render.
- [ ] `pipe(message) -> result<bool, plugin-error>` — called once per inbound inter-plugin pipe message. Return `true` requests a `render`. Same render-bool convention as `update`.
- [ ] No `deactivate` / `on-unload` / `pre-shutdown` export exists. Verified by `rg -n "(deactivate|on-unload|pre-shutdown|shutdown)" crates/ark-plugin-protocol/wit/` printing zero hits in the plugin-export WIT.

**install-event variant (mirrors a subset of Chrome `OnInstalledReason` + Reload for dev mode, per Cluster 5 §5.2 + §5.9 verdict):**
- [ ] `install-event` is a four-arm WIT variant whose arms are exactly: `install`, `update(from-version: string)`, `host-update(from-host-version: string)`, `reload`. The `reload` arm fires on every dev-mode reload (R13). Variant arms carry the data needed to act on them; bare-string reasons are forbidden. Chrome's `shared_module_update` arm is intentionally omitted (no shared-module concept in ark v1; revisit if/when one lands).
- [ ] `install-event` is `#[non_exhaustive]` (or the WIT equivalent via a sentinel arm) so adding a fifth reason in a future MINOR is non-breaking.
- [ ] The first three arm names match the Chrome `OnInstalledReason` enum shape modulo `chrome_update` → `host-update` rename. Verified by a `cargo test` golden against the documented Chrome enum string list (subset).

**Idempotency contract:**
- [ ] `on-install` is re-entrant: if the plugin crashes before returning `ok`, the host re-runs it on the *next activation cycle* (a fresh wasmtime instance with a fresh `Store`), prior to that cycle's `load()` call. Plugin authors are explicitly told to make every write idempotent (UPSERT, `mkdir -p`, etc.). Documented in the WIT `on-install` doc-comment with the literal string "may run again on the next activation cycle".
- [ ] `load()` may be called again after a respawn following any failure mode in this requirement or R8. No in-memory state survives across calls; the plugin re-reads from `Resource<T>` handles or filesystem. The WIT `load` doc-comment carries the literal sentence "Nothing in memory survives across calls".
- [ ] The host persists a `last-seen-version` key per (plugin-id, host-installation-uuid) tuple so `on-install` knows whether to dispatch `install` vs `update`. Persistence is NOT the plugin's responsibility for this key; the host writes it after `on-install` returns ok.
- [ ] `update`, `render`, `pipe` are individually re-entrant under failure: a failed call may be replayed by the host; plugins must dedupe via event-id or message-id where the operation is non-idempotent.

**Concurrency:**
- [ ] Hooks may be called concurrently across instances (separate plugins or separate `Store<PluginCtx>`s) but never concurrently within one instance. The single-`Store`-per-plugin invariant from Cluster 3 §3.1 enforces this without runtime locks.
- [ ] The host serialises calls into a single instance through the wasmtime `Store`'s thread-affinity invariant (Cluster 3 §3.1: `Store<T>` "NO — one thread at a time"). Verified by an integration test that fires concurrent host-driven `update` and `pipe` and asserts the plugin observes them in some serial order with no overlap.
- [ ] No hook may be re-entered from inside itself via a host call. Re-entry attempts are detected at the host boundary and return `plugin-error::reentrant-call`.

**Failure policy (mirrors Cluster 5 §5.9 verdict — "keep the plugin alive, surface the error, log"):**
- [ ] `on-install` failure = log + surface error in `ark ext list` AND in `ark doctor`. The current activation cycle proceeds — `load()` is still called immediately after; missed installation work is the plugin author's bug. The host retries `on-install` on the *next* activation cycle (next session start, next `ark ext dev` reload, next ark restart). After 3 consecutive activation cycles where `on-install` fails, the plugin is marked disabled in `ark.kdl` until the user re-enables explicitly.
- [ ] `load()` failure = unload the plugin instance immediately (drop the `Store`); log error; surface in `ark doctor`. No silent retry loop. Mirrors Zellij's load behaviour per Cluster 5 §5.7.
- [ ] `update(event)` failure = log + skip this event; keep the plugin alive. After N consecutive failures the host unloads the instance. N defaults to 16; configurable per plugin via `plugins.<name>.runtime.update-failure-budget` in `ark.kdl` (per R5 schema).
- [ ] `render(view-id, w, h)` failure = log + paint a "plugin error" placeholder widget in the pane; keep the plugin alive. Render-error logs are rate-limited to one per second per (plugin, view) pair to prevent log flooding.
- [ ] `pipe(message)` failure = log + return the `plugin-error` to the sender as a typed transport error; keep the plugin alive. The sender decides retry vs. give-up.
- [ ] WASM trap in any hook (the wasmtime `Store` is poisoned) = unload the instance; log the trap; surface in `ark doctor`. No auto-restart unless the scene declares an explicit restart policy. The trap policy mirrors systemd's `Restart=` rather than VS Code's "extension host crashed, reload?" modal.

**State reconstruction:**
- [ ] No WIT export named `serialize-state` or `restore-state` exists in v1 (the cooperative-reload path in Cluster 3 §3.8 is deferred). Reload = drop + rebuild + fresh `load()`.
- [ ] The plugin's only durable channels are (a) host-side `Resource<T>` handles whose identity the host preserves across reloads when the scene-handle-name matches (the user-close suppression policy in `cavekit-soul-phase-2-ark-view.md` R8 keys on `SceneHandleName` + `ParamsHash`, which is exactly the substrate that lets a re-spawned instance re-attach to the same logical pane), and (b) the filesystem under granted preopens.

**Cross-plugin observation (lifted from IntelliJ `DynamicPluginListener` per Cluster 5 §5.6):**
- [ ] A plugin may subscribe to a host-side `event::plugin-lifecycle { plugin-id, kind }` event delivered through the existing `update` channel. `kind` is one of `loaded`, `unloaded`, `crashed`, `upgraded { from, to }`. No new hook is added for this; it rides `update`.

**Cross-reference:**
- [ ] Cluster 5 §5.9 verdict (sudden-death-safe; one `on-install` reason enum mirroring Chrome) is the source of this design. `cavekit-soul-phase-2-ext-surface.md` lifecycle hooks for compiled-in extensions adopt the same five-hook discipline; this kit specifies the WASM-export shape only.

### R8: 3-phase loading

**Description:** A plugin file moves through three sequential phases — Inspect, Compile, Instantiate — each gated on the prior phase passing. No phase has side effects on the running session until Instantiate. This is the wasmtime-supported "introspect-then-instantiate" pattern from Cluster 3 §3.6, lifted with one extra inspection pass for ark's custom `ark-caps:v1` and `ark-meta:v1` sections (per Cluster 3 §3.5 and R9 below).

Fail-fast on each phase boundary; no partial loads, no compilation of binaries that fail meta/caps decode, no instantiation of plugins whose grants are unsatisfied or whose views can't render on the active target. Compiled `Component`s and pre-instantiated `InstancePre`s are cached per (binary, caps-key) so reload paths skip already-paid work.

**Acceptance Criteria:**

**Phase 1 — Inspect (no instantiation, no guest code, custom-section read first):**
- [ ] Read the plugin file as bytes (mmap allowed). Walk custom sections via `wasmparser::Parser::parse_all` (per Cluster 3 §3.5) BEFORE calling `Component::new`.
- [ ] Extract the `ark-caps:v1` payload and decode via `postcard`. Missing section = `error[plugin/missing-caps]`. Decode failure = `error[plugin/malformed-caps]` with the wasmparser offset surfaced for debuggability.
- [ ] Extract the `ark-meta:v1` payload and decode via `postcard`. Missing section = `error[plugin/missing-meta]`. Decode failure = `error[plugin/malformed-meta]`.
- [ ] Compute the component type via `Component::new(&engine, bytes)` and call `.component_type()`. Compilation IS allowed in Inspect because it is required to produce an audited import list; the Cluster 3 verdict treats `Component::new` as part of the inspection budget (a 5-10 MiB plugin compiles in tens of milliseconds with caching).
- [ ] Enumerate imports via `component.component_type().imports(&engine)` and exports via `.exports(&engine)`. Both lists are stored alongside the decoded `ark-caps:v1` and `ark-meta:v1` payloads as the inspection result.
- [ ] No `Store`, no `Linker::instantiate_pre`, no guest code runs in this phase. Verified by an integration test that asserts zero `PluginCtx` constructions and zero `Store::new` calls during a successful Inspect.
- [ ] Inspect failure halts loading; phases 2 and 3 are skipped. The compiled `Component` (if `Component::new` succeeded) MAY be cached for a future Inspect retry under a different meta — but not surfaced as "loaded".

**Phase 2 — Compile (verify cross-checks, pick linker variant, pre-instantiate):**
- [ ] Cross-check `ark-caps:v1` declared caps against the WIT import list from phase 1 (per the R3 caps/imports cross-check). Mismatch = `error[plugin/caps-imports-mismatch]` listing each over-claimed and under-claimed cap.
- [ ] Verify the user's grant set (from local config) covers the plugin's declared `wants` (per R5 of this kit). Insufficient grants = `error[plugin/insufficient-grants]` listing the missing caps with the exact KDL remediation block (per R5).
- [ ] Verify at least one of the plugin's declared view types matches the host's active render target (per R6 of this kit). Zero matches = `error[plugin/no-renderable-views]`.
- [ ] Verify identity invariants from R9: `ark-abi-version` matches the host (per R14); `name` does not collide with an already-loaded plugin; the plugin's WIT world name matches the `ark-meta:v1` `name` field. Each check has its own error code.
- [ ] Select the correct `LinkerSet` variant based on the granted cap set (per R4 of this kit, which lifts Cluster 3 §3.2 approach A — one Linker per capability profile, all built up-front).
- [ ] Precompile via `linker.instantiate_pre(&component) -> InstancePre<PluginCtx>`. The `InstancePre` is cached alongside the `Component` in the plugin registry per (plugin-binary, caps-key) tuple (per Cluster 3 §3.1 fast-instantiation pattern).
- [ ] Compile failure halts loading; phase 3 is skipped. The compiled `Component` may still be cached for a future load attempt with a different grant set; the failed `InstancePre` is dropped.

**Phase 3 — Instantiate (run guest code, default-deny WasiCtx, lifecycle dispatch):**
- [ ] Construct `WasiCtx` via `WasiCtxBuilder` with default-deny defaults (per Cluster 3 §3.3): `allow_tcp(false)`, `allow_udp(false)`, `inherit_stdin/out/err = false`, no preopens, empty env, no args.
- [ ] Apply per-grant capability wiring: each granted cap opens exactly one builder method on `WasiCtxBuilder` (e.g. `fs.read("/path")` → `preopened_dir(host_path, guest_path, DirPerms::READ, FilePerms::READ)`). The mapping is documented in the cap registry.
- [ ] Construct `Store<PluginCtx>` from the per-process `Engine` and the freshly built `WasiCtx`.
- [ ] Set epoch deadline + cooperative-yield: `store.set_epoch_deadline(2)` and `store.epoch_deadline_async_yield_and_update(2)` (per Cluster 3 §3.7 for ~100 ms yield windows). The per-process epoch-ticker mechanism (per R1) drives `engine.increment_epoch()` every ~50 ms.
- [ ] Instantiate via `pre.instantiate_async(&mut store).await`.
- [ ] Determine first-load vs version-drift by reading the host's `last-seen-version` key for this (plugin-id, host-installation-uuid). If absent, dispatch `on-install(install-event::install)`. If present and differs from the plugin's `version` (per R9), dispatch `on-install(install-event::update { from-version })`. Persist the new `last-seen-version` only after `on-install` returns ok.
- [ ] Always call `load()` after a successful (or skipped) `on-install`. `load()` failure unloads the instance per R7's failure policy.
- [ ] Instantiate failure unloads the instance and surfaces the error per R7. The compiled `Component` and cached `InstancePre` are NOT dropped — they remain available for a future load attempt.

**Phase boundaries and observability:**
- [ ] No "partial load" state is observable from outside the loader: a plugin is either present in the host's `loaded_plugins` map (phases 1-3 all passed) or absent. Verified by a `cargo test` that runs each phase failure path and asserts the plugin is not enumerated by `ark ext list`.
- [ ] Phase boundaries are crossed sequentially per plugin; multiple plugins may be in different phases concurrently. Verified by a parallel-load integration test.
- [ ] Each phase failure carries a stable error code (`plugin/missing-caps`, `plugin/caps-imports-mismatch`, etc.) suitable for `ark doctor` aggregation and CI assertion.
- [ ] Cross-reference: Cluster 3 §3.6 (introspect-then-instantiate pattern, extended here with the `ark-caps`/`ark-meta` inspection step), §3.1 (Engine/Store/Instance lifecycle), §3.3 (`WasiCtxBuilder` granular gating), §3.7 (epoch interruption).

### R9: Identity

**Description:** A plugin's identity — its name, version, and ABI compatibility — is baked into the wasm binary at build time via a custom section, mirroring Zed's `extension.toml` `id` field ("cannot be changed after your extension has been published", per Cluster 1 §1) and Lapce's `volt.toml` `name`. Identity lives in a dedicated `ark-meta:v1` custom section, separate from the `ark-caps:v1` section that R3 owns. There is no sidecar manifest file: the plugin file is self-describing.

The host extracts identity host-side via `wasmparser` during phase 1 (R8) and refuses to load any plugin whose declared `name` collides with an already-loaded plugin or whose `ark-abi-version` does not match the current host. Identity baking is a build-time concern handled by an `ark-plugin-sdk` proc-macro; there is no runtime identity parser in the guest, matching the wit-bindgen pattern.

**Acceptance Criteria:**

**Section layout:**
- [ ] Plugin binaries carry a custom section literally named `ark-meta:v1`. The payload is `postcard`-encoded. Verified by `rg -n "ark-meta:v1" crates/ark-plugin-sdk/` for the emit side and `rg -n "ark-meta:v1" crates/ark-host/src/load.rs` for the read side.
- [ ] The `ark-meta:v1` section is distinct from `ark-caps:v1`. Both may co-exist in the same binary; the loader reads them independently. A binary with `ark-caps:v1` but no `ark-meta:v1` = `error[plugin/missing-meta]`.
- [ ] The `ark-meta:v1` payload schema is itself versioned via the `:v1` suffix on the section name. A `v2` schema lives in a `ark-meta:v2` section; the host reads either, preferring v2 when both present. This mirrors `ark-caps:v1` from R3.

**Required fields:**
- [ ] `name: String` — snake-case identifier matching the regex `^[a-z][a-z0-9_]*$`. Validated at host-side decode; non-conforming = `error[plugin/invalid-name]`. The `name` is documented as immutable across versions of the same plugin (renaming = new plugin, fresh `on-install` dispatch).
- [ ] `version: String` — semver 2.0.0 string. Validated via the `semver` crate at decode; non-conforming = `error[plugin/invalid-version]`.
- [ ] `ark-abi-version: u32` — the integer ABI version the plugin was built against. Current value = `1` (per R14). Mismatch with the host's current ABI version = `error[plugin/abi-mismatch]` with both versions surfaced. Refuse to load.

**Build-time emit (no runtime parser in the guest):**
- [ ] Identity is BAKED at build time via the same `#[derive(Plugin)]` macro that R3 uses for caps. Single derive invocation, single attribute block:
  ```rust
  #[derive(Plugin)]
  #[plugin(
      name = "claude_code",
      version = "0.1.0",
      abi = 1,
      capabilities = [
          FsRead "Read project files",
          SpawnProcess "Spawn claude binary",
      ],
  )]
  struct MyPlugin;
  ```
  The macro emits two `#[link_section]` static byte arrays: `ark-meta:v1` (identity) and `ark-caps:v1` (caps).
- [ ] The macro derives `name` from the attribute and asserts at compile time that the WIT world name (per the plugin's `wit/world.wit`) matches the attribute `name`. Mismatch = compile error from the proc-macro with both names surfaced.
- [ ] No runtime parser in the guest: the plugin's wasm binary contains no code path that reads its own `ark-meta:v1` section. Verified by `rg -n "ark-meta" examples/plugins/*/src/` printing zero hits in plugin source code (only the derive-macro output references it, and that lives in a `crates/ark-plugin-sdk/` build dependency).
- [ ] No sidecar files. The plugin distribution is exactly one `.wasm` file. Verified by `rg -n "manifest|extension\.toml|plugin\.toml|volt\.toml" crates/ark-host/` printing zero hits.

**Host-side cross-checks (during phase 1 + phase 2 of R8):**
- [ ] The host extracts `ark-meta:v1` host-side via `wasmparser` during phase 1 (R8). No guest code runs to read identity.
- [ ] During phase 2 (R8), the host cross-checks: the plugin's declared `name` must equal the WIT world name extracted from `Component::component_type().exports(&engine)`. Mismatch = `error[plugin/world-name-mismatch]` listing both names.
- [ ] During phase 2, the host checks `name` against the loaded-plugin set: collision = `error[plugin/duplicate-name]`. The first-loaded plugin wins; the second is refused with the conflict surfaced. Multi-version same-name loading is explicitly forbidden (matches v0.1 scope per `cavekit-overview.md`).
- [ ] `ark-abi-version` is checked before any other phase-2 step: an ABI-mismatched plugin is refused before caps cross-check, before view-target check, before `instantiate_pre`. This keeps the failure cheap and the diagnostic clean.
- [ ] Plugins that declare `ark-abi-version > host_abi_version` get the same `error[plugin/abi-mismatch]` diagnostic as `<` mismatches; the host refuses to forward-guess.

**Stability guarantees:**
- [ ] `name` is documented as immutable: changing `name` between two versions of the same plugin source = a new plugin from the host's perspective, with a fresh `on-install(install-event::install)` dispatch and no version-drift detection.
- [ ] `version` MUST monotonically increase across releases of the same `name`. The host does not enforce monotonicity at load time (downgrade is allowed for development) but `on-install(install-event::update { from-version })` carries the prior version so plugins can refuse downgrades themselves.
- [ ] Bumping `ark-abi-version` is a host MAJOR-version event (per R14); the host keeps support for at most one prior ABI version.

**Cross-reference:**
- [ ] Cluster 1 §1 (Zed `extension.toml.id` immutability) and §2 (Lapce `volt.toml.name`) — both shipping editor wasm hosts use a single immutable identifier; ark's `name` field follows the same pattern with the addition of host-side cross-check against the WIT world name (a check Zed and Lapce do not perform because their plugin-discovery is filesystem-driven, not binary-driven).
- [ ] Cluster 1 §1 verdict bullet 4 ("Manifest-versioned, immutable plugin ID") is the source of this requirement.
- [ ] Cluster 3 §3.5 (`wasmparser` custom-section read) — implementation pattern for host-side identity extraction.

### R10: Abstract widget tree per view type

**Description:** Plugins emit a typed `widget-tree` (a structured data record), **not** raw ANSI bytes, **not** GPU pixels, **not** a zellij stdout stream. This is the Zed verdict from Cluster 1 §1 (Visual Extension API proposal #53403): "extensions emit data, not pixels". The widget tree's WIT type is parameterised per render target — `terminal-widget-tree` for `TerminalView`, `gui-widget-tree` reserved for `GuiView`.

The host owns a per-target **materializer** that turns the widget tree into the actual rendered surface (ANSI for terminal; egui/floem widgets for GUI when v2 lands). This insulates plugins from the render backend swap: the same `terminal-widget-tree` will compose identically whether ark's terminal frontend remains zellij or migrates to a custom renderer. v1 ships only the `TerminalView` materializer; the `GuiView` materializer is deferred along with the `GuiView` target itself (R6).

**Acceptance Criteria:**

**WIT shape:**
- [ ] The `render` hook (R7) returns `result<widget-tree, plugin-error>`. `widget-tree` is a WIT variant whose arms correspond to the host's render targets: `terminal(terminal-widget-tree)` and `gui(gui-widget-tree)`. The arm a plugin returns must match the `view-id`'s declared target (R6); mismatch = host-side `error[plugin/widget-tree-target-mismatch]` and the render is treated as a render failure per R7 (placeholder painted).
- [ ] `terminal-widget-tree` is a recursive WIT record/variant tree with at minimum these node kinds: `text(string, style)`, `row(list<terminal-widget-tree>, layout)`, `column(list<terminal-widget-tree>, layout)`, `box(terminal-widget-tree, border)`, `spacer(cells)`, `cursor(position)`. The exact WIT shape is documented in `crates/ark-plugin-protocol/wit/widget-tree.wit`.
- [ ] `terminal-widget-tree` carries no raw ANSI escape codes, no `\x1b[`-prefixed byte strings, no zellij-specific identifiers. Verified by a `cargo test` that walks every node variant and asserts string fields contain no ESC bytes (`0x1b`).
- [ ] `gui-widget-tree` exists as a WIT type but its variants are `#[non_exhaustive]` and may be empty in v1. Documented as "reserved; expected to map to a subset of egui or floem widgets in v2". The host has no `GuiView` materializer in v1.
- [ ] Style records (colours, attributes) reference theme tokens by string key, not raw RGB triples. The host theme resolves tokens at materialization time so plugins inherit the user's theme automatically (the consistency point from Cluster 1 §1 Zed proposal rationale).

**Materializer ownership:**
- [ ] One materializer per render target lives in the host, not in any plugin. v1 ships `TerminalMaterializer` only, in `crates/ark-render-terminal/`. Verified by `ls crates/ark-render-terminal/Cargo.toml`.
- [ ] The materializer is a function `fn materialize(tree: TerminalWidgetTree, w: u32, h: u32) -> AnsiBytes` whose only inputs are the tree + dimensions and which holds no `&mut World` reference. Verified by the materializer crate's public surface being a single free function (no `impl` with mutable state) and an integration test that calls it from a thread with no PluginCtx in scope, asserting it produces the same output for the same input.
- [ ] Plugins do NOT receive a zellij `Pane` handle, an ANSI writer, or any host render-backend type. The only way a plugin contributes to a frame is by returning a `widget-tree` from `render`. Verified by `rg -n "zellij|ansi|crossterm|ratatui" crates/ark-plugin-protocol/wit/` printing zero hits in the WIT export surface.
- [ ] The materializer enforces the host's frame budget: a render that takes longer than the configured threshold is logged. The plugin is not killed for slow render — the host paints the previous frame's tree as a placeholder while logging a warning. Threshold default 16 ms; configurable per plugin via `plugins.<name>.runtime.render-budget-ms` in `ark.kdl` (R5).

**Loading-time guarantees:**
- [ ] During phase 1 (R8), the host enumerates the plugin's exported view types (R6) and verifies each declared view's `render` signature returns the variant arm matching its target. A `TerminalView` whose `render` returns `gui(...)` = `error[plugin/widget-tree-target-mismatch]` at load time, NOT at first render.
- [ ] During phase 3 (R8), the host caches a per-view materializer reference so the per-frame path is one indirect call, not a target-lookup.
- [ ] Plugins exporting only `GuiView` views never produce a `widget-tree` in v1 because they fail the load-time render-target check from R6; the materializer never sees their output.

**Backend insulation:**
- [ ] Swapping the terminal render backend (e.g. zellij → custom renderer in a future ark version) is a host-internal change: zero plugin source files change, zero plugin binaries are recompiled. Verified by a `cargo test` that swaps a stub `TerminalMaterializer` for the production one and asserts the plugin's emitted `widget-tree` is byte-identical across the swap.
- [ ] The `terminal-widget-tree` WIT type is `#[non_exhaustive]` so adding a new node kind in a future MINOR is non-breaking for plugins that do not produce it.
- [ ] A new render target added in a future MAJOR (e.g. `web-widget-tree` for an in-browser ark) requires only a new materializer in the host plus a new WIT arm; existing `terminal-widget-tree`-emitting plugins are untouched.

**Failure modes:**
- [ ] A plugin returning `widget-tree::terminal(...)` larger than a host-configured node-count budget (default 10,000 nodes per frame) triggers `plugin-error::widget-tree-too-large` and the render is treated as a failure per R7. The budget is documented in the WIT `render` doc-comment.
- [ ] Materializer failure (e.g. unrenderable Unicode in a terminal target) emits a placeholder cell and a warn log; it does NOT propagate back into the plugin as a `render` failure. Materializer correctness is the host's problem; plugin correctness is the plugin's problem.
- [ ] A plugin emitting widgets whose computed layout exceeds the pane dimensions is clipped silently by the materializer, not refused. Truncation behaviour is materializer-defined and documented.

**Cross-reference:**
- [ ] Cluster 1 §1 verdict ("plugins emit abstract widget trees; host materializes") is the direct source of this requirement; the Zed Discussion #53403 RFC is the precedent for the data-not-pixels stance.
- [ ] Cluster 1 §1 "lift this" bullet 5 ("Data-only UI for v1; defer pixel-pushing") is the rationale for the v1 restriction to `TerminalView`.
- [ ] Cluster 1 §1 "avoid this" bullet 3 ("Don't promise UI rendering by extensions in v1") is the rationale for committing to a real view DSL (this widget tree) rather than a half-measure webview escape hatch.
- [ ] `cavekit-scene.md` R10 (extension delivery modes) — this requirement narrows the wasm-component delivery mode to a typed-tree contract; the compiled-in and subprocess delivery modes have analogous tree-emit contracts handled by their own kits.
- [ ] `cavekit-soul-phase-2-ark-view.md` R3-R4 (`View` / `CommandView` / `ZellijView` traits + `Pane<V>` affordances) — the typed wrapper surface that gates which views can produce widget trees vs. which run as native zellij subprocesses.

### R11: Intent dispatch via ark-bus

**Description:** Plugin↔plugin and CLI↔plugin communication rides on **ark-bus**, ark's existing intent dispatch substrate (already wired through the supervisor). An intent is a typed, named message with an optional payload, optional kv args, and a sender identity; routing is by intent type-name to all subscribers of that name. ark-bus is render-target-agnostic: when the active scene's render target is zellij, ark-bus pipes ride on top of zellij's `PipeMessage` substrate (Cluster D §5 — `PipeSource::{Cli, Plugin, Keybind}`, `name`, `payload`, `BTreeMap<String,String> args`, `is_private` for targeted-vs-broadcast); when the render target is a future GUI host, ark-bus pipes ride on a host-internal channel. The wire transport is hidden from plugin code — plugins only see `Intent` values.

A plugin emits an intent via the host import `ark:host/bus.send(intent: Intent)` and receives intents via the `pipe(message)` lifecycle hook (R7). Subscriptions are implicit: a plugin that exports `pipe` receives every intent matching the names it filters on inside that hook. Emit and receive are independently cap-gated: `ark:cap/bus-send` for `bus.send`, `ark:cap/bus-receive` for the broadcast variant of `pipe` (targeted delivery to a specific plugin URL is permitted without `bus-receive`). The user-facing emitter `ark bus intent <view> <name> [args]` (already wired via the supervisor bridge) is the CLI entry point and surfaces as `PipeSource::Cli` on the receiving plugin.

**Acceptance Criteria:**
- [ ] Host import `ark:host/bus.send(intent: Intent) -> Result<(), BusError>`. `Intent` carries `name: String`, `payload: Option<Vec<u8>>`, `args: BTreeMap<String, String>`, `target: IntentTarget` where `IntentTarget = Broadcast | Plugin(url) | Handle(@h)`.
- [ ] Plugin receives intents via the `pipe(message: PipeMessage)` lifecycle hook (R7). `PipeMessage` exposes `source`, `name`, `payload`, `args`, `is_private` — type-isomorphic to zellij's wire shape so terminal-render-target dispatch is a zero-copy passthrough.
- [ ] Routing key is `intent.name`. Intent names follow the dotted-namespace convention (e.g. `claude-code.permission.granted`, `pi.subagent.focus`). Unknown names route to no one — never an error.
- [ ] **Render-target adapter:** when scene's render target is zellij, ark-bus serializes `Intent` to a zellij `PipeMessage` and dispatches via zellij's targeted-pipe primitive (for `IntentTarget::Plugin(url)` / `IntentTarget::Handle`) or zellij's broadcast pipe (for `IntentTarget::Broadcast`). When render target is GUI host, ark-bus dispatches via host-internal channel; plugin code unchanged.
- [ ] Cap `ark:cap/bus-send` (R3) gates `ark:host/bus.send`. Absence at load time = import-resolution failure (R8 Phase 1 inspect refuses).
- [ ] Cap `ark:cap/bus-receive` gates *broadcast* `pipe` delivery only. Targeted delivery (`target = Plugin(self_url)` or `Handle(@my_handle)`) is always permitted regardless of `bus-receive` — a plugin can receive intents addressed specifically to it.
- [ ] CLI `ark bus intent <view> <name> [k=v ...] [--payload @-|<text>]` enqueues an intent with `source = PipeSource::Cli`, target resolved from `<view>` (handle or plugin URL). Already implemented per supervisor↔ark-bus bridge.
- [ ] Send-from-keybind: scene `bind` blocks compile to ark-bus `Intent` enqueues (Cluster D §5, R5 of cavekit-scene). The receiving plugin sees `source = PipeSource::Keybind`.
- [ ] Cascade depth is bounded by scene's `max-cascade-depth` (cavekit-scene R4) — an intent dispatched in response to another intent counts toward the same budget. Exceeding = `error[bus/cascade-depth]` log, drop.
- [ ] Wire format version is part of ABI (R14): a host bumps `ark-abi-version` if `Intent`'s on-wire representation changes.
- [ ] Cross-reference: cavekit-scene R4 (reactions emit intents), R5 (keybinds emit intents), R7 (`emit` op), R10 (extension protocol). Cluster D §5 (zellij pipe substrate). Memory note: ark-bus + supervisor intent dispatch bridge already done.

### R12: Distribution — bare `.wasm` v1

**Description:** A plugin ships as a single `.wasm` file. No bundles, no archives, no sidecar files, no tarballs. All metadata travels in the plugin's custom sections (R3 caps, R9 identity, R14 abi-version). Plugin identity is its **location URL**, mirroring zellij's plugin convention: `file:/abs/path/to/plugin.wasm`, `file:~/.config/ark/plugins/x.wasm`, `https://example.com/plugin.wasm`. The host downloads `https://`-scheme URLs into a content-addressed cache; `file:` URLs are loaded in place.

v1 explicitly defers OCI distribution, marketplace/registry, and code-signing trust roots — those land when a non-author shipper of plugins exists. Until then, the publisher IS the author and the user installs by URL they got from a human. `ark ext install <url-or-path>` runs the inspect-phase pipeline (R8 Phase 1 caps cross-check, R9 identity, R14 abi-version check) before the URL is added to the user's `plugins {}` config; any inspect-phase failure refuses the install with the diagnostic that R8 Phase 1 produced (no `plugins {}` mutation occurs).

**Acceptance Criteria:**
- [ ] On-disk artifact is exactly one `.wasm` file. No `.tar`, `.zip`, no `manifest.json` sibling, no `.sig` sibling. Custom sections (R3, R9) carry every byte the host needs.
- [ ] Plugin identity is its location URL. URL schemes supported: `file:` (absolute or `~`-expanded path), `https:`. `http:` refused with diagnostic. Other schemes refused.
- [ ] `https:` downloads cached at `${XDG_CACHE_HOME:-$HOME/.cache}/ark/plugins/<sha256>.wasm` where `<sha256>` is the hash of the downloaded bytes. Cache hit = skip download. Cache integrity verified on every load (rehash, mismatch = redownload).
- [ ] `file:` URLs loaded in place. No copy into cache. Mtime checked on every load for hot-reload triggering (R13 dev-mode).
- [ ] `ark ext install <url-or-path>` resolves URL → fetches if needed → runs the **inspect** phase from R8 (Phase 1): parse wasm against bytes without instantiation, read `ark-meta:v1` + `ark-caps:v1` custom sections (R3, R9), verify R14 `ark-abi-version` against host const, verify R3 caps/imports cross-check, verify R9 name immutability + collision. Any failure = refuse, no mutation to user config.
- [ ] On inspect-success, `ark ext install` mutates the user's `ark.kdl` (the same file that holds R5's `plugins {}` block) to append or replace the matching `plugins.<name> { location="<url>" capabilities { ... } }` entry. The mutation is delimited by a managed-block marker (`// >>> ark ext install: managed block`) so the user's hand-edited entries above the marker are untouched.
- [ ] `ark ext remove <url-or-name>` deletes the matching `plugins.<name>` entry from `ark.kdl`'s managed block. Cache file is NOT removed (other configs may reference it; `ark ext gc` reaps unreferenced cache entries — separate command).
- [ ] **Regression guard for the original packaging issue:** an integration test asserts `cargo tree -p <example-plugin> --target wasm32-wasip2` does NOT include `arborium-sysroot`, `facet-kdl`, or `facet-format` anywhere in the dep graph. Failure of this test = the wasm-component plugin path has regressed to pulling a parser into the guest, which was the original cause of the WASI SDK install requirement.
- [ ] `ark ext info <url-or-name>` reads the cached `.wasm` and renders the R3 caps manifest (display name, version, declared caps with reason strings, abi-version) without instantiation.
- [ ] **Explicitly out of scope for v1:** OCI registry pulls (`oci://`), marketplace search/listing, central directory, signed-publisher trust chains, auto-update on schedule. Each of these reactivates when a third-party shipper of plugins exists.
- [ ] Cross-reference: cavekit-distribution.md R3 (wasm embedding, bundled plugins). R3-this-kit (caps custom section), R8 Phase 1 (inspect phase), R9 (identity custom section), R14 (abi-version). Cluster A "Lessons for ark" §1-2 (sidecar-as-section, single-artifact-per-plugin).

### R13: Dev mode — `ark ext dev <dir>`

**Description:** Every surveyed extension system ships a "point at a directory" dev loop (Cluster A "Lessons for ark" bullet 3 — VS Code F5/`--install-extension`, JetBrains `runIde`, Chrome "Load unpacked", Firefox `web-ext run`): "ark's CLI should ship `ark ext dev <dir>` from day one; authors who must `pack` to test will fight you." `ark ext dev <path-to-crate-dir>` registers a plugin from the crate's build output, watches the build artifact, and reloads the running plugin instance on every change. The watched path is `<dir>/target/wasm32-wasip2/release/<crate-name>.wasm` by default, with `--target` and `--profile` flags overriding. Multiple dev plugins can be active at once; each is keyed by directory.

Dev-mode plugins bypass the R12 cache (load directly off disk on every reload), receive verbose logging at trace level, and fire `on-install(install-event::reload)` on every reload (matching the lifecycle reset that VS Code/JetBrains/WebExt provide). The `plugins {}` entry created by `ark ext dev` is **temporary** — held in the running supervisor's in-memory config only, not persisted to `plugins.kdl`. Restarting `ark` drops the dev registration; the author re-runs `ark ext dev <dir>` to resume. Promoting a dev plugin to a real install requires `ark ext install <built-wasm-path>`.

**Acceptance Criteria:**
- [ ] `ark ext dev <dir>` discovers the wasm build artifact for the crate at `<dir>` using cargo's standard target-dir conventions. Override via `--artifact <path>`, `--target <triple>`, `--profile <name>`.
- [ ] If artifact is missing at command time, command does NOT fail — it watches and waits, attaching once the first build produces the file.
- [ ] Watcher uses filesystem notifications (notify crate or equivalent). Debounce window 250 ms to coalesce cargo's multi-write build pattern. On debounced change: re-read file, re-run inspect phase (R8 Phase 1), reload plugin instance.
- [ ] Reload sequence: (a) drop the existing wasmtime instance + `Store` (no `unload()` lifecycle exists per R7), (b) re-run R8 phases 1-3 on new bytes, (c) call `on-install(install-event::reload)` (R7's 4th install-event arm) before `load()`. Reload is atomic to the host: intents arriving during the reload window are buffered to a per-plugin FIFO and replayed in order against the new instance after `load()` returns ok. Verified by an integration test that fires N intents during a forced reload and asserts the new instance observes all N in arrival order.
- [ ] Dev plugins skip the R12 cache. The on-disk `.wasm` is the single source of truth; no `<sha256>.wasm` is written.
- [ ] Dev plugins receive verbose logging: host emits trace-level logs for every host-import call made by the plugin, every intent received, every cap check. Non-dev plugins log at info+.
- [ ] `on-install(install-event::reload)` fires on every dev reload, distinct from `install` (first add), `update` (R12 install replacing existing version), and `host-update` (ark binary upgrade). Plugin code matches on the `install-event` arm to differentiate one-time setup from per-reload state reset.
- [ ] The temporary `plugins {}` entry added by `ark ext dev` lives in supervisor in-memory config only. `ark ext list` marks it `(dev)`. `ark` restart drops the entry.
- [ ] `ark ext dev` exits cleanly on Ctrl-C; on exit, the running plugin instance's `Store` is dropped (no `unload()` hook called per R7) and the entry is removed from the supervisor's plugin set.
- [ ] Multiple concurrent `ark ext dev` invocations supported; each keyed by canonicalized `<dir>`. Re-invoking `ark ext dev <same-dir>` while one is running = no-op (returns "already watching").
- [ ] Authors of compiled-in extensions (workspace crate path per cavekit-scene R10) use the same command — `<dir>` points at the workspace member; `cargo build` rebuilds the artifact; reload drops the in-process compiled-in instance and replaces with the wasm one for that session. (Documented edge case; default is wasm-built.)
- [ ] Cross-reference: Cluster A "Lessons for ark" §3 (point-at-a-directory norm), §4 (lifecycle reasons not install-callbacks). cavekit-scene R10 (extension delivery modes), R12-this-kit (cache bypass), R7 (lifecycle hooks — reload arm), R8 Phase 1 (inspect phase reuse).

### R14: ABI versioning

**Description:** The ark plugin ABI is versioned by a single monotonic `u32 ark-abi-version` field embedded in the `ark-meta:v1` custom section (R9). The host binary defines `ARK_ABI_VERSION: u32` as a compile-time constant — the version of the ABI this host speaks. On inspect (R8 Phase 1), the host compares the plugin's declared version to its own and decides load eligibility. Versioning is at the **ABI layer**, not at the plugin's own semver (R9.version): a plugin can ship version 7.3.1 against `ark-abi-version = 2`, and that's independent. ABI versioning catches drift in the host↔plugin contract — exports the host calls, imports the host provides, wire formats on bus, widget tree shapes for terminal views — that the plugin's own semver cannot describe.

Compatibility policy (v1 — strict equality): plugin abi == host abi → load; plugin abi > host abi → refuse with "plugin requires ark abi N, your ark speaks M, upgrade ark"; plugin abi < host abi → refuse with "plugin built against older ark, rebuild against current". Future hosts may carry a back-compat list (`SUPPORTED_PLUGIN_ABIS: &[u32] = &[5, 4, 3]`) to allow reading older plugins; this is documented per host release. A bump of `ARK_ABI_VERSION` is a **breaking change** to the host plugin protocol and tracked in plan-overview as a release-gate event.

**Acceptance Criteria:**
- [ ] `ark-meta:v1` custom section (R9) carries a `ark-abi-version: u32` field. Required, no default — missing field = inspect-phase refuse with `error[abi/missing-version]`.
- [ ] Host binary exposes a public const `pub const ARK_ABI_VERSION: u32 = N;` in `ark-types` (or equivalent foundation crate). v1 ships at `ARK_ABI_VERSION = 1`.
- [ ] Inspect phase (R8 Phase 1) compares: plugin abi == host abi → continue; plugin abi > host abi → refuse with `error[abi/host-too-old]` ("plugin requires ark abi N, your ark speaks M, upgrade ark"); plugin abi < host abi → refuse with `error[abi/plugin-too-old]` ("plugin built against older ark abi M, this ark speaks N, rebuild against current").
- [ ] Future back-compat list mechanism documented: `pub const SUPPORTED_PLUGIN_ABIS: &[u32] = &[N];` (v1 = single-element slice). When a host adds a back-compat entry, the inspect phase accepts plugin abis in the slice instead of strict equality. v1 does not exercise this path; it's a host-side knob.
- [ ] **Bump triggers** (any of the following changes to the host↔plugin contract require a `ARK_ABI_VERSION` bump): (a) a new required wasm export the host calls (the `on-install`/`load`/`update`/`render`/`pipe` set per R7); (b) an `ark:host/*` import renamed, removed, or signature-changed; (c) wire format change for `Intent` payloads on ark-bus (R11); (d) widget tree shape change for `TerminalView` rendering (R10); (e) new variant added to `Target` enum (R6).
- [ ] Adding an *optional* new export the host calls only if present is NOT a bump. Adding a new `ark:cap/*` capability is NOT a bump (cap-grant flow handles unknown caps at inspect time per R3). Adding a new optional import a plugin can request is NOT a bump.
- [ ] `ark doctor` prints `ARK_ABI_VERSION = N` (and `SUPPORTED_PLUGIN_ABIS` if length > 1) in its diagnostic header.
- [ ] `ark ext info <url-or-name>` displays the plugin's declared `ark-abi-version` alongside its semver — clearly labeled to avoid confusion.
- [ ] ABI version is independent of plugin semver (R9 `version` field). A `version: "7.3.1", ark-abi-version: 2` plugin is valid and common.
- [ ] Plan-overview tracks `ARK_ABI_VERSION` as a release-gate constant with a changelog entry per bump (date + bump trigger + migration notes).
- [ ] Cross-reference: cavekit-distribution.md R4c (ACP crate pin pattern — same shape, different surface). Cluster D §6 (zellij's weak `plugin_version` check + Cluster D Constraint 5: "ark must add its own gate"). Cluster 5 of plugin-protocol-survey (versioning analysis). R3 (caps), R7 (lifecycle exports), R9 (identity custom section), R11 (bus wire format), R8 Phase 1 (inspect phase).

## Cross-Reference Summary

| External Kit / Doc | Relationship |
|---|---|
| `cavekit-overview.md` | Add domain row "Plugin protocol (wasm)" pointing at this kit |
| `cavekit-scene.md` R10 | SUPERSEDED for the `wasm-component` delivery mode — caps/identity/lifecycle now governed by R3-R10 here. Compiled-in and subprocess delivery modes unchanged. |
| `cavekit-scene.md` R6 | Continued — view-alias resolution unchanged; this kit narrows wasm-component view types to `TerminalView`/`GuiView` (R6) |
| `cavekit-soul-phase-2-ark-view.md` R3-R4 | Continued — `View`/`CommandView`/`ZellijView` axis stays orthogonal to `TerminalView`/`GuiView` |
| `cavekit-claude-code.md` | Migration path: claude-code becomes an ark wasm-component plugin per this kit. Drop control-verb section, replace with R7 lifecycle hooks. Tracked in cavekit-claude-code amendment. |
| `cavekit-plugin-status.md` + `cavekit-plugin-picker.md` | Migration path: status + picker rebuilt as ark wasm-component plugins per this kit; current zellij-tile builds retained as transitional implementations. |
| `cavekit-distribution.md` R3 | Amendment — `.wasm` plugins ship as bare files via R12, not as zellij-tile artifacts |
| `context/refs/plugin-protocol-survey.md` | Source of all "Cluster N" references in this kit (Clusters 1-6) |
| `context/refs/extension-systems-survey.md` | Source of "Cluster A/B/C/D" references (initial survey round) |

## Migration Notes

The current in-tree zellij-tile plugins (`ark-plugin-status`, `ark-plugin-picker`) and the claude-code extension (`extensions/claude-code/`) all migrate to this protocol. Migration sequencing belongs in the build site (separate task); the migration is non-atomic across plugins (each can switch independently once the host substrate from R1-R8 lands).

`ark-ext-metadata` and `ark-ext-metadata-types` crates are deleted as part of the migration: their role (KDL-encoded metadata in a wasm custom section, parsed by a host-side facet-kdl serializer pulled INTO the wasm) is wholly replaced by R3 (caps custom section, no parser in guest) + R9 (identity custom section), both emitted by a single `#[derive(Plugin)]` macro in `ark-plugin-sdk`. The `register_extension!` macro is replaced by this single derive.

WASI SDK (the `/opt/wasi-sdk` install required by the current arborium-sysroot transitive dependency) is no longer needed once migration completes: the wasm component-model build path (`cargo build --target wasm32-wasip2`) used by `wit-bindgen`-based plugins does not pull arborium.
