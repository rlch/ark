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
| #3 | SubagentRegistry auto-wire | DONE | `8804b9f` | `Arc<SubagentRegistry>` + `rename_pane_emitter: Option<Arc<dyn Fn>>` on `ClaudeCodeExtension`; accept loop folds registry before bus forward + invokes emitter on each `RenamePaneEmission`; 7 new unit tests under `v0_2_backlog_3_tests`. |
| #2 | Stack::spawn_pane live RPC wiring | DONE | `7947e4c` | `PaneAttrs` widened to `{view_attrs: serde_json::Value}` with `#[serde(default)]` for v0.1 wire back-compat; `PaneAttrs::from_attrs<A:Serialize>` constructor; process-global `StackDispatcher` trait + `register_stack_dispatcher` (OnceLock); `Stack::spawn_pane` calls dispatcher when set, else synthetic-handle fallback. Integration test at `crates/ark-view/tests/stack_dispatcher.rs` isolated so OnceLock doesn't leak into unit tests. No protocol version bump — the `{view_attrs: null}` wire shape is strict-superset of v0.1 `{}`. |

## Design decisions

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
| `extensions/claude-code/src/lib.rs` | `8804b9f` |

## Test deltas

- `ark-view`: +8 unit tests + 1 integration test (1 fn, two dispatch assertions)
- `ark-ext-claude-code`: +7 unit tests in `v0_2_backlog_3_tests`
- Workspace: 2282 → 2296 passing (`cargo test --workspace --tests`)
