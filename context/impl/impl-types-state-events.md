---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Implementation Tracking: types-state-events

Build site: context/plans/build-site.md

| Task | Status | Notes |
|------|--------|-------|
| T-003 | DONE | AgentId(String) {orchestrator}-{name}-{ulid}; new()/from_parts()/parse(); session_name() drops ULID; state_dir(base) → base/agents/{id}/; sanitize() coerces to [a-z0-9_]; serde transparent; 13 tests cover fs-safe + URL-safe + roundtrip. crates/ark-types/src/id.rs |
