---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
parent: cavekit-soul.md
phase: 1
status: ready
---

# Cavekit: Soul Phase 1 — Types

## Scope

Pure type surgery inside `crates/types/`. Covers the full set of type-shape
changes Phase 1 demands: `AgentSpec` → `SessionSpec`, `AgentId` →
`SessionId`, `AgentStatus` → `SessionStatus`, `AgentEvent` → `CoreEvent`
(with `Ext(ExtEvent)`), deletion of agent-methodology enums from core, and
the scene crate's `AgentSnapshot` → `SessionSnapshot` generalisation.

This kit does **not** describe consumers of these types — every downstream
consumer (supervisor, state layout, CLI, core consumers) is covered by
sibling Phase 1 kits that depend on this one landing first.

All decisions here are locked by the parent kit's **Resolved Decisions**
section in `cavekit-soul.md`. See in particular the decisions labelled
"State compat", "Path leaf", "No `SessionKind` discriminator",
"`Phase` + `Outcome` delete from core entirely", "Bus payload shape", and
"`SessionId::new(name)`".

## Requirements

### R1: `SessionSpec` replaces `AgentSpec`

**Description:** `crates/types/src/spec.rs` defines a `SessionSpec` struct
matching the shape in the parent kit's Layer 2 code block, and no
`AgentSpec` type remains in `ark-types`.

The required field set is (from `cavekit-soul.md` Layer 2):

```rust
struct SessionSpec {
    id: SessionId,
    name: String,
    scene_path: Option<PathBuf>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    created_at: DateTime<Utc>,
    ext_config: BTreeMap<String, serde_json::Value>,
}
```

**Acceptance Criteria:**
- [ ] `rg -n "pub struct SessionSpec" crates/types/src/spec.rs` prints exactly one hit.
- [ ] `rg -n "pub struct AgentSpec" crates/types/src/` prints zero hits.
- [ ] `rg -n "AgentSpec" crates/types/src/` prints zero hits (no residual re-exports, aliases, or doc mentions inside the crate).
- [ ] `SessionSpec` carries the exact field set (`id`, `name`, `scene_path`, `cwd`, `env`, `created_at`, `ext_config`) with the types enumerated in the parent kit's Layer 2 block, verifiable by `cargo test -p ark-types` passing a test that constructs one with every field.
- [ ] `SessionSpec` does NOT carry any of `orchestrator`, `engine`, `cmd`, `layout`, `session`, `runner_config` fields. `rg -n "orchestrator:|engine:|runner_config:|cmd:|layout:|session:" crates/types/src/spec.rs` prints zero hits that name a struct field of `SessionSpec`.
- [ ] `SessionSpec` round-trips through `serde_json` (ser → de) preserving equality, verified by a `cargo test -p ark-types` test named something like `session_spec_serde_json_roundtrip`.
- [ ] `SessionSpec::env` is a `BTreeMap<String, String>` (not `HashMap`) so serialisation order is deterministic, verified by a `cargo test -p ark-types` test asserting sorted key order in the serialized JSON.
- [ ] `SessionSpec::ext_config` iteration / serialization is deterministic (`BTreeMap<String, serde_json::Value>`), verified by a `cargo test -p ark-types` test.

**Dependencies:** none (this is the root of Phase 1).

### R2: Delete `OrchestratorSpec` alias

**Description:** The `pub type OrchestratorSpec = AgentSpec` alias in
`crates/types/src/spec.rs` is deleted. No replacement; extensions that
want an orchestrator-facing view define their own types.

**Acceptance Criteria:**
- [ ] `rg -n "OrchestratorSpec" crates/` prints zero hits.
- [ ] `cargo check --workspace` succeeds without a replacement alias.

**Dependencies:** R1.

### R3: `SessionId::new(name)` with ulid baked in

**Description:** `crates/types/src/id.rs` defines `SessionId` with the
constructor `SessionId::new(name: &str) -> Self` that bakes in a freshly
generated ulid. The struct must expose both a human-friendly name and the
ulid; the path-leaf form is `<name>-<ulid>` (the `orchestrator-` prefix
from `AgentId` is gone). Per the parent kit's Layer 2 block:

```rust
struct SessionId { name: String, ulid: Ulid }
impl SessionId {
    pub fn new(name: &str) -> Self { /* ulid baked in */ }
    pub fn as_path_leaf(&self) -> String { format!("{}-{}", self.name, self.ulid) }
}
```

**Acceptance Criteria:**
- [ ] `rg -n "pub struct SessionId" crates/types/src/id.rs` prints exactly one hit.
- [ ] `rg -n "pub struct AgentId" crates/types/src/id.rs` prints zero hits.
- [ ] `rg -n "AgentId" crates/types/src/` prints zero hits.
- [ ] `SessionId::new("foo")` returns a value whose `as_path_leaf()` matches the regex `^foo-[0-9a-z]{26}$` (lowercase-ulid 26 chars), verified by a `cargo test -p ark-types` test.
- [ ] `SessionId::new("foo")` called twice returns distinct values (the ulid differs), verified by a `cargo test -p ark-types` test.
- [ ] The constructor signature `SessionId::new(name)` takes exactly one `&str` argument — NO `orchestrator` parameter. `rg -n "SessionId::new\s*\(" crates/` shows every call site passing exactly one argument.
- [ ] `SessionId` exposes `name()` and `ulid()` accessors (or equivalent public fields) so callers can read each component without parsing.
- [ ] Adversarial names (containing `/`, whitespace, control chars) sanitize to filesystem-safe runs before landing in the path-leaf — verified by a `cargo test -p ark-types` test matching the shell/fs safety properties the existing `AgentId` test suite covers (spaces, `/`, `..`, null bytes all sanitize to `_` or equivalent).
- [ ] `SessionId` serialises transparently (`#[serde(transparent)]` or serialised as a single string) so `spec.json` stays human-scannable, verified by a `cargo test -p ark-types` ser-as-string test.

**Dependencies:** none (independent of R1, but R1's `SessionSpec.id: SessionId` uses this type).

### R4: `SessionStatus` replaces `AgentStatus`

**Description:** `crates/types/src/status.rs` defines `SessionStatus`
matching the parent kit's Layer 2 block:

```rust
struct SessionStatus {
    id: SessionId,
    started_at: DateTime<Utc>,
    terminated_at: Option<DateTime<Utc>>,
    ext_state: BTreeMap<String, serde_json::Value>,
}
```

Per-extension status rollup data lives in `ext_state` under the ext name.
Core writes nothing into those buckets.

**Acceptance Criteria:**
- [ ] `rg -n "pub struct SessionStatus" crates/types/src/status.rs` prints exactly one hit.
- [ ] `rg -n "pub struct AgentStatus" crates/types/src/` prints zero hits.
- [ ] `rg -n "AgentStatus" crates/types/src/` prints zero hits.
- [ ] `SessionStatus` carries exactly these fields: `id`, `started_at`, `terminated_at`, `ext_state`. No `phase`, `progress`, `last_event_at`, `last_event_summary`, `tab_handles`, `supervisor_pid`, `stalled_since`, `findings`, `hide`, or `spec` fields. `rg -n "phase:|progress:|last_event_|tab_handles:|supervisor_pid:|stalled_since:|findings:|hide:" crates/types/src/status.rs` prints zero hits.
- [ ] `SessionStatus` round-trips through `serde_json`, verified by a `cargo test -p ark-types` test.
- [ ] `ext_state` is a `BTreeMap<String, serde_json::Value>` and its serialisation is deterministic.

**Dependencies:** R3.

### R5: Delete `Phase`, `Outcome`, `Findings` from core

**Description:** Remove `Phase` (enum), `Outcome` (enum), and `Findings`
(struct) from `ark-types` entirely. These are methodology concepts; they
re-home inside extensions in Phase 4, not Phase 1.

**Acceptance Criteria:**
- [ ] `rg -n "pub enum Phase" crates/types/src/` prints zero hits.
- [ ] `rg -n "pub enum Outcome" crates/types/src/` prints zero hits.
- [ ] `rg -n "pub struct Findings" crates/types/src/` prints zero hits.
- [ ] `rg -n "Severity" crates/types/src/` has zero hits OR every remaining hit is inside a module that is not re-exported from `ark_types` (since `Severity` was an input to `Findings.record`, it becomes orphaned; deleting it is acceptable and preferred).
- [ ] No re-exports of `Phase`, `Outcome`, `Findings` from `ark_types::lib`. `rg -n "pub use .*(Phase|Outcome|Findings)" crates/types/src/lib.rs` prints zero hits.

**Dependencies:** none (orthogonal to R1/R3/R4 at the module level).

### R6: `CoreEvent` with shrunk variant list + `Ext(ExtEvent)`

**Description:** `crates/types/src/event.rs` defines a `CoreEvent` enum
matching the parent kit's Layer 3 block:

```rust
enum CoreEvent {
    Log { level, message, target },
    Error { error },
    SessionStarted { spec: SessionSpec },
    SessionEnded { terminated_at: DateTime<Utc> },
    Ext(ExtEvent),
}
```

And an `ExtEvent` struct:

```rust
struct ExtEvent {
    ext: String,
    kind: String,
    payload: serde_json::Value,
}
```

The old `AgentEvent` enum with its ten agent-specific variants
(`Started`, `TabOpened`, `TabClosed`, `Progress`, `TaskDone`, `Iteration`,
`PhaseTransition`, `ToolUse`, `Message`, `FileEdited`, `ReviewComment`,
`PermissionAsked`, `PermissionResolved`, `Stall`, `UserEvent`, `Done`) is
deleted in full. `Log` and `Error` survive (shape may evolve to drop the
`id: AgentId` field since sessions broadcast on a per-session bus).

**Acceptance Criteria:**
- [ ] `rg -n "pub enum CoreEvent" crates/types/src/event.rs` prints exactly one hit.
- [ ] `rg -n "pub enum AgentEvent" crates/types/src/` prints zero hits.
- [ ] `rg -n "AgentEvent" crates/types/src/` prints zero hits.
- [ ] The `CoreEvent` enum has exactly five variants: `Log`, `Error`, `SessionStarted`, `SessionEnded`, `Ext`. Extra variants fail a `cargo test -p ark-types` exhaustiveness test that pattern-matches every variant.
- [ ] None of these variants exist in `CoreEvent`: `Started`, `TabOpened`, `TabClosed`, `Progress`, `TaskDone`, `Iteration`, `PhaseTransition`, `ToolUse`, `Message`, `FileEdited`, `ReviewComment`, `PermissionAsked`, `PermissionResolved`, `Stall`, `UserEvent`, `Done`. `rg -n "TaskDone\|Iteration\|PhaseTransition\|ToolUse\|FileEdited\|ReviewComment\|PermissionAsked\|PermissionResolved\|Stall\|UserEvent" crates/types/src/event.rs` prints zero hits.
- [ ] `pub struct ExtEvent { ext: String, kind: String, payload: serde_json::Value }` exists in `crates/types/src/event.rs` (or equivalent path) with public fields of exactly these names + types, verified by a `cargo test -p ark-types` construction test.
- [ ] `CoreEvent::SessionStarted` carries a `spec: SessionSpec` field (not an `AgentSpec`).
- [ ] `CoreEvent::SessionEnded` carries `terminated_at: DateTime<Utc>` and does NOT carry an `outcome` field.
- [ ] `CoreEvent` round-trips through `serde_json` for every variant, verified by a `cargo test -p ark-types` per-variant roundtrip test.
- [ ] `CoreEvent::Ext(ExtEvent { ext: "acp-client", kind: "permission.asked", payload: json })` serialises in a form that preserves `ext` and `kind` as tagged strings and round-trips cleanly.
- [ ] Supplementary enums that existed only to support deleted variants (`TabRole`, `TabHandle`, `MessageRole`, `PermissionDecision`, `Severity`) are either deleted from `ark-types` or moved into the one remaining core variant that needs them. `rg -n "pub (enum|struct) (TabRole|TabHandle|MessageRole|PermissionDecision)" crates/types/src/` prints zero hits. (`LogLevel` survives if `CoreEvent::Log.level: LogLevel` uses it.)

**Dependencies:** R1, R3 (variants reference `SessionSpec` / `SessionId`).

### R7: `FlatEvent` shim with `Into` conversions from both core and ext events

**Description:** Scene-script convenience shim, per the parent kit's
Layer 3 block:

```rust
struct FlatEvent { name: String, payload: serde_json::Value }
impl From<&CoreEvent> for FlatEvent { /* core events → "ark.core.*" */ }
impl From<&ExtEvent>  for FlatEvent { /* "<ext>.<kind>" */ }
```

Rhai selectors match on the flat `name` without needing to pattern-match
the enum.

**Acceptance Criteria:**
- [ ] `pub struct FlatEvent { pub name: String, pub payload: serde_json::Value }` (or equivalent with pub accessors) exists in `crates/types/src/`.
- [ ] `From<&CoreEvent> for FlatEvent` is implemented, verified by a `cargo test -p ark-types` test asserting `CoreEvent::Log { … }` → `FlatEvent { name: "ark.core.log", … }` (or `ark.core.log`-prefixed value per the parent kit's scene KDL example).
- [ ] `From<&ExtEvent> for FlatEvent` is implemented, verified by a `cargo test -p ark-types` test asserting `ExtEvent { ext: "claude-code", kind: "tool.use", payload: p }` → `FlatEvent { name: "claude-code.tool.use", payload: p }`.
- [ ] For every `CoreEvent` variant, the produced `FlatEvent.name` starts with `"ark.core."`, verified by an exhaustive per-variant test.
- [ ] `FlatEvent` is `Clone + Debug + PartialEq` and serialises through `serde_json`, verified by a roundtrip test.

**Dependencies:** R6.

### R8: Delete `ENGINES_V1`, `ORCHESTRATORS_V1`, `is_v1_engine`, `is_v1_orchestrator`

**Description:** Remove engine/orchestrator scope-lock constants and
predicates from `crates/types/src/scope.rs`. `MUX_V1` and `is_v1_mux`
survive (mux is in-core for v1).

**Acceptance Criteria:**
- [ ] `rg -n "ENGINES_V1" crates/` prints zero hits.
- [ ] `rg -n "ORCHESTRATORS_V1" crates/` prints zero hits.
- [ ] `rg -n "is_v1_engine" crates/` prints zero hits.
- [ ] `rg -n "is_v1_orchestrator" crates/` prints zero hits.
- [ ] `rg -n "MUX_V1" crates/types/src/scope.rs` prints at least one hit (the const survives).
- [ ] `rg -n "is_v1_mux" crates/types/src/scope.rs` prints at least one hit (the predicate survives).
- [ ] `cargo test -p ark-types` passes.

**Dependencies:** none.

### R9: `SessionSnapshot` replaces `AgentSnapshot` with extensions map

**Description:** `crates/scene/src/context.rs` replaces the local
placeholder `AgentSnapshot` with `SessionSnapshot` (generalised per the
parent kit):

```rust
struct SessionSnapshot {
    id: String,
    name: String,
    extensions: BTreeMap<String, serde_json::Value>,
}
```

Scene Rhai scope exposes `session.*` + `session.extensions.<name>.*`
instead of `agent.*`. The existing `session` binding stays; the old
`agent` binding is removed.

**Acceptance Criteria:**
- [ ] `rg -n "pub struct AgentSnapshot" crates/scene/` prints zero hits.
- [ ] `rg -n "pub struct SessionSnapshot" crates/scene/src/context.rs` prints exactly one hit.
- [ ] `SessionSnapshot` carries a public `extensions` field of type `BTreeMap<String, serde_json::Value>` (or equivalent map-of-json type), verified by a `cargo test -p ark-scene` construction test.
- [ ] The `build_event_scope` function signature no longer takes an `agent: &AgentSnapshot` parameter; it takes `session: &SessionSnapshot` (and its other arguments). `rg -n "fn build_event_scope" crates/scene/src/context.rs` shows the new signature.
- [ ] The Rhai event scope exposes an `agent` binding NOT at all, OR exposes only a backwards-compat `agent` binding that is documented as deprecated and populated identically to `session` for this phase. Verified by a `cargo test -p ark-scene` test evaluating a Rhai predicate against `session.name`, `session.id`, and `session.extensions["some-ext"]`.
- [ ] The Rhai event scope exposes `session.extensions` as a map indexable by ext name, returning each ext's JSON-shaped state as a Rhai-accessible value, verified by a `cargo test -p ark-scene` test that sets `extensions["acp"]` and reads it back through a Rhai predicate like `session.extensions["acp"]["some_field"] == "some_value"`.
- [ ] `cargo test -p ark-scene` passes.

**Dependencies:** R3, R4.

## Out of Scope

- Consumers of the new types — supervisor loop, CLI commands, core
  consumers. Those are covered by
  `cavekit-soul-phase-1-supervisor.md` and
  `cavekit-soul-phase-1-cli-and-launch.md`.
- State-layout path rename and boot-time nuke of legacy `$STATE/agents/`.
  Covered by `cavekit-soul-phase-1-state-layout.md`.
- Per-extension status rollup logic. Phase 1 only provides the
  `ext_state` / `ext_config` / `ExtEvent` buckets; actual extension
  writers arrive in Phase 2+.
- Re-homing `Phase` / `Outcome` / `Findings` inside
  `extensions/claude-code/` or `extensions/cavekit/`. Those are Phase 4
  moves.
- `crates/types/src/permission.rs` (`PermissionPolicy`, `READ_ONLY_TOOLS`,
  `POLICY_FILE_NAME`). Stays in core for Phase 1; Phase 4 moves it to
  `extensions/claude-code/`. Phase 1 type-churn against that file is
  limited to whatever changes are forced by `AgentEvent` → `CoreEvent`
  (it may need a tolerant stub or parked behind `#[cfg(feature = "legacy-
  hooks")]` — exact shape is a Phase-1 compile-glue concern, not a
  requirement here).
- Scene KDL grammar changes. The `on "claude-code.tool.use" { … }` string
  syntax documented in the parent kit's Layer 3 block works via the
  `FlatEvent` shim; no grammar changes required.
- Subscription-side bus infrastructure. `EventSink` / `EventReceiver` /
  `event_bus` stay; only the payload type changes from `AgentEvent` to
  `CoreEvent`.

## Cross-References

- Parent spec: `cavekit-soul.md` (see Resolved Decisions + Phase 1).
- See also: `cavekit-soul-phase-1-state-layout.md` (consumes R3 — `SessionId` path-leaf form drives the on-disk layout).
- See also: `cavekit-soul-phase-1-supervisor.md` (consumes R1, R4, R6 for supervisor signature + lifecycle events).
- See also: `cavekit-soul-phase-1-cli-and-launch.md` (consumes R1, R4, R6 for list / state_writer / launch).
- Downstream (Phase 4): `extensions/claude-code/` re-homes `PermissionPolicy`, tool taxonomy, and any Phase/Outcome equivalents.

## Changelog

(empty)
