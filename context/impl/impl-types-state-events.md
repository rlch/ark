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
| T-008 | DONE | StateLayout with XDG resolution + macOS /tmp fallback; agent_dir/spec_path/status_path/events_path/pid_path/supervisor_log_path/hooks_dir/artifacts_dir/archive_dir/lock_path/agent_socket_path; ensure_dir_0700 idempotent + mode-enforcing; 10 tests. crates/ark-types/src/state_dir.rs. NOTE: ARK_STATE_DIR env override for from_env() not yet wired — T-012 will integrate. |
