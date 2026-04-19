---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: v0.2 Backlog

Backlog source: `context/plans/v0.2-backlog.md`.

Ledger is prepend-only. Newest entries at top.

## Items

| Item | Title | Status | SHA | Notes |
|------|-------|--------|-----|-------|
| #7 | cc-hook cargo-install fallback | DONE | `45dc120` | `install_cc_hook_at` falls through to `cargo install --bin cc-hook --path <…> --root <…> --locked` when `CC_HOOK_BYTES` is empty. New `InstallOutcome::InstalledViaCargo` variant. Source resolution: `$ARK_CLAUDE_CODE_EXT_DIR` > compile-time `CARGO_MANIFEST_DIR` > run-time `$CARGO_MANIFEST_DIR`. `$ARK_CARGO_BIN` overrides cargo (shim-based tests); `$ARK_CLAUDE_CODE_NO_CARGO_FALLBACK=1` opts out. Layout gate: install path must be `<root>/bin/cc-hook` else skip. 7 new tests using sh shim; env mutations serialized via module-local `env_lock()` mutex. |
| #5 | ext_state persistence for list columns | DONE | `d91c55b` | On-disk sentinel (kit R1 option 2). Extension writes `$STATE/sessions/<sid>/ext-state/claude-code.json` atomically on every fold; `ark list` reads all `ext-state/*.json` back and overlays them onto `SessionStatus.ext_state` per row. `StateLayout::session_ext_state_{dir,path}` helpers. `CcListColumnState::write_to_file` / `read_from_file`. `ClaudeCodeExtension::configure_ext_state_path` wired at `on_session_start`. Write failures log + continue. 17 new tests (7 columns, 7 `v0_2_backlog_5_tests`, 3 cli::list + 1 end-to-end overlay). |
| #4 | `ark ext <name> <verb>` dispatcher | DONE | `4c0bffd` | New `ark ext invoke <name> <verb> [args...]` in `crates/cli/src/commands/ext/verb.rs`. New supervisor `ControlVerbDispatcher` trait + `register_control_verb_dispatcher` OnceLock in `crates/supervisor/src/ext_verb_dispatch.rs`. New `ControlVerbInvoke` command variant in `commands.rs`. Session resolution mirrors `ark bus` (--session > `$ARK_SESSION_ID` > unique-dir). No `ArkExtension` trait widening; the extension's existing verb handlers remain the fan-out target. 14 new tests (clap parse, build_request shape, resolve_active_session, render_response, end-to-end echo-supervisor success + err + missing-socket). |
| #3 | SubagentRegistry auto-wire | DONE | `8804b9f` | `Arc<SubagentRegistry>` + `rename_pane_emitter: Option<Arc<dyn Fn>>` on `ClaudeCodeExtension`; accept loop folds registry before bus forward + invokes emitter on each `RenamePaneEmission`; 7 new unit tests under `v0_2_backlog_3_tests`. |
| #2 | Stack::spawn_pane live RPC wiring | DONE | `7947e4c` | `PaneAttrs` widened to `{view_attrs: serde_json::Value}` with `#[serde(default)]` for v0.1 wire back-compat; `PaneAttrs::from_attrs<A:Serialize>` constructor; process-global `StackDispatcher` trait + `register_stack_dispatcher` (OnceLock); `Stack::spawn_pane` calls dispatcher when set, else synthetic-handle fallback. Integration test at `crates/ark-view/tests/stack_dispatcher.rs` isolated so OnceLock doesn't leak into unit tests. No protocol version bump — the `{view_attrs: null}` wire shape is strict-superset of v0.1 `{}`. |

## Design decisions

- **#7 cargo-install fallback chosen over real `include_bytes!`** — The
  F-709 deadlock (cargo-inside-cargo when embedding a workspace-member
  native binary via build.rs) makes the real embedding path risky.
  Cargo-install is the documented sidestep from v0.2-backlog.md and
  covers every non-distributed scenario: the developer running `cargo
  build -p ark-ext-claude-code` already has `cargo` on PATH and the
  source tree. A distributed binary without source (cargo-dist
  tarball) falls through to the manual-install hint — that path is a
  separate follow-up.
- **#7 layout gate `<root>/bin/cc-hook`** — cargo's `--root` flag
  always writes to `<root>/bin/<bin-name>`. Rather than move the
  binary post-install we reject non-matching layouts upfront with a
  clear log; the caller's `cc_hook_install_path()` already returns the
  canonical `$XDG_BIN_HOME/cc-hook` layout that matches cargo's.
- **#7 `ARK_CARGO_BIN` env override** — tests use a shell shim that
  records its argv and creates the expected output file; the shim is
  pointed to via this env var so no real `cargo install` runs during
  `cargo test`. Mirrors the test patterns already in this crate
  (`$XDG_BIN_HOME` override for path resolution).
- **#5 on-disk sentinel over supervisor write-through** — Kit R1
  offered two options: (1) supervisor reads ext_state at status-write
  time, (2) extension writes its own JSON file. Chose option 2
  because it keeps the supervisor out of the per-extension write path
  (matches the 2026-04-18 soul-phase philosophy — extensions own their
  state, supervisor observes) and mirrors the Tier 2 T-012 pattern.
  The overlay in `ark list` (`persisted_ext.insert` AFTER querying the
  supervisor) means ext-writer is authoritative, which is the
  ownership semantics we want.
- **#5 `configure_ext_state_path`** — The extension is single-session
  by contract, so a single `Option<PathBuf>` on the struct
  (Arc-wrapped for Clone-sharing) is sufficient. No session-id lookup
  at fold time; `on_session_start` knows which session is active and
  installs the one canonical path. A future multi-session extension
  would need a `HashMap<SessionId, PathBuf>` here.
- **#4 OnceLock dispatcher over trait widening** — The
  `ArkExtension` trait is frozen per the 2026-04-16 pivot (phase-2
  kits). Adding a `control_verb_invoke` method would ripple through
  every in-proc + ndjson + wasm implementor. Instead we mirror v0.2
  #2's `StackDispatcher` OnceLock pattern: the supervisor consults a
  process-global dispatcher; boot code registers one; unregistered
  returns a clear error. The dispatcher impl that fans out to each
  extension's existing verb handlers lives in supervisor-side wiring
  code (follow-up), NOT in the trait. Same call-site shape the
  StackDispatcher work pioneered.
- **#4 `--session` > `$ARK_SESSION_ID` > unique fallback** — Exact
  mirror of `ark bus` resolution (bus.rs::resolve_active_session).
  Keeps the CLI mental model consistent; a user who knows how `ark
  bus` works also knows how `ark ext invoke` works.
- **PaneAttrs shape** chose `serde_json::Value` over generic `V::Attrs`
  associated type. Associated-type approach would cascade
  `V::Attrs: Serialize + DeserializeOwned` bounds onto every `Stack<V>`
  call site including `PaneLike` dyn iteration. Value keeps the bound
  propagation free at the cost of a JSON `to_value`/`from_value`
  traversal on serialise, which is negligible next to the RPC itself.
- **Stack::spawn_pane live dispatch** chose process-global OnceLock
  (`StackDispatcher` trait + `register_stack_dispatcher`) over threading
  an `Arc<dyn ExtensionClient>` through every `Stack<V>` value. `Stack`
  is constructed from wire frames all over the place; threading an
  RPC client through would cascade to every supervisor/extension/test
  path that names `Stack<V>`. OnceLock mirrors the pattern
  `supervisor::ext_dispatch::CAP_REGISTRY` already uses — registration
  is the supervisor's job at startup. ark-view cannot depend on
  ark-ext-proto (reverse DAG violation), so the trait is abstract.
- **Registry emitter** — `Arc<dyn Fn + Send + Sync>` (not FnMut, not
  a channel): the accept loop is a single tokio task but may fire
  emissions from different frames concurrently if ever restructured.
  Fn + Sync composes with any imaginable routing shape (channel send,
  direct RPC dispatch, log-only). Callback-over-channel means the
  emitter surface is test-friendly: unit tests capture emissions into
  an `Arc<Mutex<Vec<...>>>` without a tokio runtime.
- **Backwards compat** for PaneAttrs: `#[serde(default)]` means a v0.1
  peer that sent `{}` decodes cleanly into `PaneAttrs { view_attrs:
  Value::Null }`. No protocol version bump; no
  `CURRENT_PROTOCOL_VERSION` change.

## Follow-ups (unblocked by this packet)

- **Supervisor-side** `StackDispatcher` impl — the supervisor's
  `on_session_start` handler should build a `StackDispatcher` that
  funnels `spawn_pane` through the `stack/spawn_pane` RPC to the
  appropriate extension via the per-session `ExtensionClient` handle.
  Once installed via `register_stack_dispatcher` at supervisor init,
  every `Stack<V>::spawn_pane` call in any extension lights up.
- **Supervisor-side** `rename_pane_emitter` installation — the
  supervisor's `ClaudeCodeExtension` construction path should chain a
  `.with_rename_pane_emitter(...)` call whose closure maps
  `RenamePaneEmission.id` → stack-child `Pane<ClaudeCodeSubagent>`
  handle and dispatches `pane/emit(payload)` against it.
- **S-H (T-084)** scene-v3 wasm transport audit — per the packet
  brief this was the last audit blocker on v0.2 #2 + #3; both are now
  closed, so S-H can proceed.

## Files touched

| Path | Commit(s) |
|------|-----------|
| `crates/ark-view/src/typed.rs` | `7947e4c` |
| `crates/ark-view/src/lib.rs` | `7947e4c` |
| `crates/ark-view/tests/stack_dispatcher.rs` | `7947e4c` (new) |
| `crates/ark-ext-proto/src/lib.rs` | `7947e4c` (doc only) |
| `extensions/claude-code/src/lib.rs` | `8804b9f`, `d91c55b` |
| `crates/cli/src/commands/ext/mod.rs` | `4c0bffd` |
| `crates/cli/src/commands/ext/verb.rs` | `4c0bffd` (new) |
| `crates/supervisor/src/commands.rs` | `4c0bffd` |
| `crates/supervisor/src/lib.rs` | `4c0bffd` |
| `crates/supervisor/src/ext_verb_dispatch.rs` | `4c0bffd` (new) |
| `crates/cli/src/commands/list.rs` | `d91c55b` |
| `crates/types/src/state_dir.rs` | `d91c55b` |
| `extensions/claude-code/src/columns.rs` | `d91c55b` |
| `extensions/claude-code/src/settings_json.rs` | `45dc120` |
| `extensions/claude-code/tests/settings_reconcile.rs` | `45dc120` |

## Test deltas

- `ark-view`: +8 unit tests + 1 integration test (1 fn, two dispatch assertions) — #2
- `ark-ext-claude-code`: +7 unit tests in `v0_2_backlog_3_tests` — #3
- `ark-cli`: +14 tests in `commands::ext::verb` — #4
- `ark-supervisor`: +4 tests in `ext_verb_dispatch::tests` — #4
- `ark-ext-claude-code`: +7 in `columns::tests`, +7 in `v0_2_backlog_5_tests` — #5
- `ark-cli`: +4 in `commands::list::tests` (3 read_persisted + 1 end-to-end overlay) — #5
- `ark-ext-claude-code`: +7 in `settings_json::tests` — #7
- Workspace: 2282 → 2296 (#2+#3) → 2314 (#4) → 2331 (#5) → 2338 (#7)
