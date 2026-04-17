---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
parent: cavekit-soul.md
phase: 1
status: ready
---

# Cavekit: Soul Phase 1 — State Layout

## Scope

Covers the `$STATE/agents/` → `$STATE/sessions/` path-leaf rename inside
`crates/types/src/state_dir.rs` (the `StateLayout` struct + accessor
methods), and the supervisor-boot-time nuke of any legacy
`$STATE/agents/` directory that was left over from the pre-Phase-1
ark build.

Per the parent kit's Resolved Decisions:
- **State compat:** nuke `$STATE` on first boot after Phase 1 lands. No
  migration code.
- **Path leaf:** `$STATE/agents/` renames to `$STATE/sessions/`. One-time
  rename alongside type migration; no symlink bridge.

## Requirements

### R1: `StateLayout::sessions_root()` replaces `agents_root()`

**Description:** `crates/types/src/state_dir.rs` renames the method
`agents_root()` → `sessions_root()` and the path it returns from
`$base/agents` → `$base/sessions`.

**Acceptance Criteria:**
- [ ] `rg -n "fn agents_root" crates/types/src/state_dir.rs` prints zero hits.
- [ ] `rg -n "fn sessions_root" crates/types/src/state_dir.rs` prints exactly one hit.
- [ ] `StateLayout::new("/state".into(), ..., ...).sessions_root()` returns `/state/sessions` (not `/state/agents`), verified by a `cargo test -p ark-types` test.
- [ ] `rg -n '"agents"' crates/types/src/state_dir.rs` prints zero hits (no residual `"agents"` path segment literal).
- [ ] `rg -n "agents_root\(\)" crates/` prints zero hits (no caller still names the old accessor).

**Dependencies:** `cavekit-soul-phase-1-types.md` R3 (the path leaf format for each session directory is `<name>-<ulid>`, driven by `SessionId::as_path_leaf()`).

### R2: Per-session path accessors renamed from `agent_*` to `session_*`

**Description:** Every per-agent accessor on `StateLayout` is renamed:
- `agent_dir` → `session_dir`
- `spec_path` stays (file name unchanged)
- `status_path` stays (file name unchanged)
- `events_path` stays (file name unchanged)
- `pid_path` stays
- `supervisor_log_path` stays
- `hooks_dir` stays
- `artifacts_dir` stays
- `lock_path` stays (file-name schema unchanged)
- `agent_socket_path` → `session_socket_path` (runtime socket path changes
  its dir segment from `runtime/agents/<id>.sock` to
  `runtime/sessions/<id>.sock`)

Every accessor that previously took an `&AgentId` now takes a
`&SessionId`.

**Acceptance Criteria:**
- [ ] `rg -n "fn agent_dir" crates/types/src/state_dir.rs` prints zero hits.
- [ ] `rg -n "fn session_dir" crates/types/src/state_dir.rs` prints exactly one hit.
- [ ] `rg -n "fn agent_socket_path" crates/types/src/state_dir.rs` prints zero hits.
- [ ] `rg -n "fn session_socket_path" crates/types/src/state_dir.rs` prints exactly one hit.
- [ ] `StateLayout::session_dir(&id)` returns `<base>/sessions/<id.as_path_leaf()>`, verified by a `cargo test -p ark-types` test against a fixed-ulid `SessionId`.
- [ ] `StateLayout::session_socket_path(&id)` returns `<runtime>/sessions/<id.as_path_leaf()>.sock`, verified by a `cargo test -p ark-types` test.
- [ ] Every surviving accessor (`spec_path`, `status_path`, `events_path`, `pid_path`, `supervisor_log_path`, `hooks_dir`, `artifacts_dir`, `lock_path`) takes `&SessionId` and returns a path under the new `sessions/` leaf, verified by a `cargo test -p ark-types` round of per-accessor tests.
- [ ] `rg -n "&AgentId" crates/types/src/state_dir.rs` prints zero hits (the module no longer references the deleted type).

**Dependencies:** R1, `cavekit-soul-phase-1-types.md` R3.

### R3: Archive path stays `archive/<date>/<id>` with sessions leaf format

**Description:** `StateLayout::archive_dir(date, &SessionId)` continues
to return `<base>/archive/YYYY-MM-DD/<session-path-leaf>` (the `archive/`
segment does not get renamed — sessions are archived flat under date).
The id path-leaf format follows from `SessionId::as_path_leaf()`.

**Acceptance Criteria:**
- [ ] `rg -n "archive_dir" crates/types/src/state_dir.rs` prints at least one hit and the implementation produces paths under `<base>/archive/` (not `<base>/archived/` or `<base>/sessions/archive/`), verified by a `cargo test -p ark-types` test.
- [ ] `archive_dir` accepts a `&SessionId` (not `&AgentId`).

**Dependencies:** R1, `cavekit-soul-phase-1-types.md` R3.

### R4: Supervisor boot deletes legacy `$STATE/agents/` directory

**Description:** On every supervisor boot, before creating any new
state directories, the supervisor checks for the existence of
`<state_layout.base()>/agents/` and, if present, deletes it recursively.
A single INFO-level tracing log line is emitted recording that the legacy
directory was nuked. No error is surfaced if the legacy directory is
absent (steady state).

This implements the **State compat: nuke** decision from the parent
kit's Resolved Decisions — no migration.

**Acceptance Criteria:**
- [ ] The supervisor's boot path (before spec-writing / lock / state-dir setup — before any creation under `<base>/sessions/`) invokes a nuke step against `<base>/agents/`.
- [ ] Calling the supervisor boot path against a state dir that contains a seeded `<base>/agents/somefile` results in `<base>/agents/` being removed entirely, verified by an integration-level `cargo test --workspace` test that seeds the directory, drives boot, and asserts the directory is gone afterwards.
- [ ] Calling the supervisor boot path against a state dir that has no `<base>/agents/` directory succeeds without error and does NOT create one, verified by an integration-level `cargo test --workspace` test.
- [ ] The nuke emits exactly one `tracing::info!` record naming the path that was removed, verified by a `cargo test --workspace` test with a capturing subscriber asserting the record's target + level + message substring (e.g. `"legacy $STATE/agents"`).
- [ ] No symlink-bridge or data-migration code exists. `rg -n "symlink|migrate|legacy_agents" crates/supervisor/src/ crates/types/src/state_dir.rs` shows at most tracing-message strings and no migration logic.

**Dependencies:** R1 (the `sessions/` root must exist so the nuke is unambiguous — legacy `agents/` is always nuked, sessions live under the new root).

### R5: No symlink bridge between `agents/` and `sessions/`

**Description:** The kit's parent decision says "no symlink bridge."
Explicit negative requirement so reviewers can confirm.

**Acceptance Criteria:**
- [ ] `rg -n "symlink" crates/types/src/state_dir.rs crates/supervisor/src/` prints zero hits that create or dereference a path named `agents` (matches against unrelated symlink use are acceptable).
- [ ] No new file named anything like `state_dir_compat.rs` / `legacy_paths.rs` / `migrate.rs` exists under `crates/types/src/` or `crates/supervisor/src/`.

**Dependencies:** R4.

## Out of Scope

- Renaming session-data contents (spec, status, events). Those files keep
  their names; only the directory leaf changes.
- Data migration between the legacy and new layout (none — Phase 1 nukes).
- The `SessionId` constructor / path-leaf format itself. That is
  `cavekit-soul-phase-1-types.md` R3.
- CLI `id_resolver.rs` changes. Covered by
  `cavekit-soul-phase-1-cli-and-launch.md`.
- `$XDG_CONFIG_DIR` / `$XDG_RUNTIME_DIR` / `$ARK_*` env var precedence.
  `env_paths.rs` and `from_env()` stay unchanged.

## Cross-References

- Parent spec: `cavekit-soul.md` (Resolved Decisions — "State compat", "Path leaf").
- Depends on: `cavekit-soul-phase-1-types.md` R3 (`SessionId::as_path_leaf()` format).
- Consumed by: `cavekit-soul-phase-1-supervisor.md` (supervisor boot calls the nuke), `cavekit-soul-phase-1-cli-and-launch.md` (CLI walks `sessions_root()` in list + resolver).

## Changelog

(empty)
