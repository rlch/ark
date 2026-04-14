---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Loop Log

### Iteration 1 — 2026-04-14
- T-001+T-002: scaffold workspace + pin deps — DONE. Files: Cargo.toml, rust-toolchain.toml, .gitignore, crates/*/Cargo.toml, crates/*/src/{lib,main}.rs (12 crates). Build P, fmt P. Commit f20942a. Notes: ck:task-builder subagent failed twice (narration loop + 0 tool uses); fell back to inline (parent=opus matches EXECUTION_MODEL). Next: T-003 (AgentId) — only T-003 unblocked in Tier 0 by T-001 done.

### Iteration 2 — 2026-04-14
- T-003: AgentId type + ULID + session-name + state-dir helpers — DONE. Files: crates/ark-types/{Cargo.toml, src/lib.rs, src/id.rs}. Build P, Tests 13/13 P. Commit 1908e63. Inline (small, fast). Next: T-004+T-008 packet (both deps T-003 satisfied; both live in ark-types).

### Iteration 3 — 2026-04-14
- T-004+T-008: AgentSpec + StateLayout — DONE. Files: spec.rs, state_dir.rs (+chrono/nix/tempfile deps). Build P, Tests 29/29 P (12 id + 7 spec + 10 state_dir). Commit 965c284. Subagent: general-purpose worked (vs ck:task-builder which crashed). NOTE: ARK_STATE_DIR override deferred to T-012. Next: T-005 (AgentEvent — big enum) + T-012 (env paths) + T-017 (scope-lock) all unblocked.

### Iteration 4 — 2026-04-14
- T-005 + T-012 + T-017 — ALL DONE in parallel. Two general-purpose subagents (T-005, T-012) + inline (T-017). Files: event.rs (17 variants + 6 sub-enums + TabHandle placeholder), env_paths.rs (EnvPaths Env-trait DI + ARK_*/XDG resolver + agent_socket_path helper), state_dir.rs (refactored to delegate to EnvPaths — single source of truth), scope.rs (V1 slug constants). Commits d77c5b4, d86a166, 5083934. Tests 68/68 P (id 13 + spec 7 + state_dir 10 + event 24 + env_paths 11 + scope 3). Next: T-006, T-007, T-011, T-013, T-014 unblocked (T-009 too: deps T-005/T-008 met).

### Iteration 5 — 2026-04-14
- T-006 + T-007 + T-011 (ark-types) + T-009 + T-013 + T-014 (ark-core) — ALL DONE in 2 parallel general-purpose packets. Files: status.rs (AgentStatus+Phase+Findings), tab refinements + CancellationToken re-export, event_bus.rs (EventSink+channel factories), events_log.rs (EventLogWriter spawn pattern + EventLogReader corruption-tolerant), engine.rs (Engine trait + ApprovalPolicy + EngineHandle Box<dyn Any>), orchestrator.rs (Orchestrator trait + stub World). Commits e487b38, 80f9173. Tests: ark-types 84/84 + ark-core 9/9. Next: T-010, T-015, T-016 close Tier 0.

### Iteration 6 — 2026-04-14
- T-010 + T-015 + T-016 — DONE in single general-purpose packet. Files: status_writer.rs (atomic tmp+sync_all+rename), config.rs (placeholder until T-018), multiplexer.rs (Multiplexer trait), orchestrator.rs (World replaces stub with full fields). Commit 19f75f1. Tests ark-core 18/18 (was 9). **TIER 0 COMPLETE: 17/17 tasks done, 102 tests passing across ark-types + ark-core.** Next: Tier 1 (28 tasks, frontier of 7 ready).

### Iteration 7 — 2026-04-14 (Tier 0 codex review + fixes)
- Codex adversarial review vs c25b545. Findings: F-036 (P2 AgentId parse accepts extra hyphens) FIXED; F-037 (P2 EventSink drop+warn) DEFERRED to consumer tasks T-059/60/61; F-038 (P3 EngineHandle pub fields) FIXED; F-035 (P1 zmij dep) DISMISSED as false positive (zmij is legit dtolnay upstream in serde_json 1.0.149). Commit e0bc149. Gate: PROCEED.

### Iteration 8 — 2026-04-14 (Tier 1 W1)
- 4 parallel general-purpose packets: T-018 (ark-config figment), T-024+T-029 (mux executor + layout resolver), T-039 (pane chrome), T-043+T-045 (control-socket + agents-dir). All COMPLETE. Commits 3b798da, 75270d0 (+scope issues from parallel `git add -A` despite scoped instructions — sandbox blocks git reset). Tests: 226 workspace.

### Iteration 9 — 2026-04-14 (Tier 1 W2)
- 3 parallel packets: T-019-T-023 (ark-config tail), T-025-T-028+T-032 (ZellijMux impl + preflight), T-041+T-042 (pane git + log). All COMPLETE. Commits 771e7e4/5971925, af429f4, 78ba8f2 (more commit interleaving). Plus T-044 inline (stale-socket GC + ENOTSOCK portability handling) → commit 1fb2747. Tests: 229 workspace.

### Iteration 10 — 2026-04-14 (Tier 1 W3 — closeout)
- 2 parallel packets: T-030+T-031+T-033+T-034+T-035+T-036+T-037+T-038 (layout templating + KDL writer + 6 shipped layouts + precedence + list + validation + docs) and T-040 (ark pane diff with delta + ansi-to-tui). All COMPLETE. Commits ecda420, 8020b64. **TIER 1 COMPLETE: 28/28 tasks done. 265 tests passing across 6 crates.**

Overall: 45/134 (34%). Next: Tier 2 (engine-claude-code + event-bus consumers, 16 tasks).
