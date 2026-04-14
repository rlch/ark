---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Implementation Tracking: types-state-events

Build site: context/plans/build-site.md

| Task | Status | Notes |
|------|--------|-------|
| T-003 | DONE | AgentId(String) {orchestrator}-{name}-{ulid}; new()/from_parts()/parse(); session_name() drops ULID; state_dir(base) → base/agents/{id}/; sanitize() coerces to [a-z0-9_]; serde transparent; 13 tests cover fs-safe + URL-safe + roundtrip. crates/ark-types/src/id.rs |
| T-004 | DONE | AgentSpec serde + OrchestratorSpec alias; constructor fills env empty/layout None/session derived/created_at Utc::now/runner_config Null; 7 tests roundtrip. crates/ark-types/src/spec.rs |
| T-008 | DONE | StateLayout with XDG resolution + macOS /tmp fallback; agent_dir/spec_path/status_path/events_path/pid_path/supervisor_log_path/hooks_dir/artifacts_dir/archive_dir/lock_path/agent_socket_path; ensure_dir_0700 idempotent + mode-enforcing; 10 tests. crates/ark-types/src/state_dir.rs. T-012 refactored from_env() to delegate to EnvPaths::resolve(). |
| T-005 | DONE | AgentEvent #[non_exhaustive] + 17 variants with #[serde(tag=kind, snake_case)]. Sub-enums: TabRole (incl Custom(String)), Outcome, Severity (Hash+Ord), MessageRole, PermissionDecision, LogLevel. TabHandle placeholder defined here, refined by T-007. crates/ark-types/src/event.rs |
| T-006 | DONE | AgentStatus + Phase enum (snake_case serde) + Findings rollup struct (record(Severity), total()). AgentStatus::new(spec, supervisor_pid) → phase=Starting + empty handles. crates/ark-types/src/status.rs |
| T-007 | DONE | TabHandle Clone/Display/Hash/Eq + ::new() ctor; CancellationToken re-export from tokio_util at lib.rs root. tokio-util added to workspace deps. crates/ark-types/src/{event.rs, lib.rs} |
| T-009 | DONE | EventLogWriter::spawn(path) → EventLogHandle{sender,task}. Tokio mpsc → append+flush per event (no batching, low-volume <100/sec per kit). EventLogReader::open + read_all skips malformed lines with warn. EventRecord{ts, event}. crates/ark-core/src/events_log.rs |
| T-010 | DONE | write_status_atomic(layout, id, status): serialize → tmp + sync_all + rename. read_status returns Ok(None) on missing. crates/ark-core/src/status_writer.rs |
| T-011 | DONE | EventSink/EventReceiver type aliases. channel(capacity) clamps to >=1. default_channel() = 256. Document Lagged-handling as consumer responsibility. crates/ark-types/src/event_bus.rs |
| T-012 | DONE | EnvPaths::resolve() with Env trait DI (StdEnv/MapEnv) — zero std::env mutation in tests. ARK_STATE_DIR/ARK_RUNTIME_DIR/ARK_CONFIG_DIR > XDG_* > platform fallback. ARK_RUNTIME_DIR taken verbatim (caller-isolated); XDG_RUNTIME_DIR branch appends ark-{uid} per hook-ipc R4. Naming: ARK_CONFIG_DIR (not ARK_CONFIG_PATH per kit — single-file path is T-018's concern). agent_socket_path(id) convenience. crates/ark-types/src/env_paths.rs |
| T-017 | DONE | scope.rs constants: ENGINES_V1=[claude-code], ORCHESTRATORS_V1=[cavekit, claude-code], MUX_V1=[zellij]. is_v1_engine/orchestrator/mux helpers. NDJSON deferred to v2 per kit. crates/ark-types/src/scope.rs |
