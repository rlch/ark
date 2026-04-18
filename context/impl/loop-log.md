---
created: "2026-04-14"
last_edited: "2026-04-18"
---
# Loop Log

### Wave T-039 — 2026-04-18 — Tier 7 (tests R3)
- T-039 (soul phase 2 tests R3): version-mismatch matrix. DONE. 5 cells all green.
- New file `crates/ark-ext-test-support/tests/version_mismatch.rs` — drives `ExtensionClient::handshake` against `StubExtension::builder().with_protocol_version(...)` to exercise 5 (client × ext) (MAJOR.MINOR) pairings.
- Wiring touched (scope-minimal — grep-verified absent first):
  - `ark-ext-test-support/src/lib.rs`: added `initialize` override on `StubExtension::ArkExtension` impl so the stub's configured `protocol_version` + advertised bag surface in the `InitializeResponse`. Default trait body returned `method_not_found`, would have short-circuited handshake before version gate. `Capabilities::from_iter` + `to_wire` build the R10 object-of-objects shape.
  - `ark-ext-proto/src/transport/mod.rs`: `ExtensionClient::handshake` now emits one `tracing::warn!` on `target = "ark.ext_proto.handshake"` when `ext.minor > client.minor` (same MAJOR). Fields: `client_version`, `ext_version`, `ext_capabilities`, message. Honours R3 "ark logs a WARN ... the ext advertises that this ark doesn't recognize". Existing MAJOR-mismatch `UnsupportedVersion` error kept untouched.
  - `ark-ext-proto/Cargo.toml`: add `tracing = workspace` dep (was absent — no tracing usage anywhere in this crate pre-T-039).
  - `ark-ext-test-support/Cargo.toml`: add `tracing`, `tracing-subscriber`, `futures` as dev-deps for the capture-layer in the new test file.
- 5 cells (all tokio `current_thread` flavor so the thread-scoped `tracing_subscriber::with_default` sees the event — tokio multi-thread block_on swaps dispatchers):
  1. `handshake_version_match_1_1_and_1_1_ok_no_warn` — client=1.1, ext=1.1 → Ok, warns empty, session_token minted.
  2. `handshake_version_mismatch_client_newer_minor_ok_no_warn` — client=1.1, ext=1.0 → Ok, zero warns (host newer, ext older: caps simply not advertised).
  3. `handshake_version_mismatch_ext_newer_minor_ok_with_warn` — client=1.0, ext=1.1 → Ok, exactly-one WARN with both version strings + advertised capability bag.
  4. `handshake_version_mismatch_major_client_newer_rejects` — client=2.0, ext=1.1 → `ExtensionError::UnsupportedVersion(msg)` where msg contains "2.0", "1.1", "MAJOR mismatch"; stub call_log stays empty (no subsequent RPC).
  5. `handshake_version_mismatch_major_ext_newer_rejects` — client=1.1, ext=2.0 → symmetric reject; call_log empty.
- Used `futures::executor::block_on` inside the `with_default(subscriber, || …)` closure to keep the subscriber on the executing thread (same pattern as `supervisor::ext_loader::tests`). tokio `block_on` would swap the thread-local dispatcher.
- Capture layer scoped via `with_default` (not global) so prior or future tests in the same process don't leak events; `rebuild_interest_cache` on install to survive any prior global subscriber from other test suites.
- Validation: `cargo test -p ark-ext-test-support --test version_mismatch` — 5 passed / 0 failed. `cargo test -p ark-ext-test-support` 15 passed (+5 over baseline). `cargo test -p ark-ext-proto` 84 passed (existing handshake_fails_on_major_mismatch still green — no regression). `cargo test -p ark-supervisor` 121 passed. `cargo build --workspace` clean. `cargo test --workspace --lib` 1807 passed (no change — this test file is integration, not lib).
- Not touched: scene, cli, config, ark-view, supervisor prod code. ext_loader warn path untouched (complementary warn — this new one fires in `ExtensionClient::handshake` before any loader sees the capability bag).
- Next: Tier 7 continues with T-040 (capability-gate matrix), T-042 (intent + suppression integration), then Tier 8 T-043/T-044.

### Wave T-038 — 2026-04-18 — Tier 7 (tests R2)
- T-038 (soul phase 2 tests R2): NDJSON subprocess stub. DONE.
- New bin `ark-stub-ext` in crate `ark-ext-test-support` — `src/bin/ark-stub-ext.rs`. Bin wrap R1 StubExtension in `ark_ext_proto::transport::ndjson::NdjsonServer::serve`, pump stdin→stub→stdout. `tokio::main(flavor = "current_thread")` keep dep-budget tight. No new workspace dep needed — all pieces already in `ark-ext-proto`.
- Config via env vars (caveman pick option-a from task spec — shell-scriptable, matches real ext pattern). Surface: `ARK_STUB_VERSION`, `ARK_STUB_CAPABILITIES`, `ARK_STUB_INTENTS`, `ARK_STUB_VIEW_TYPES` (csv `name:kind`), `ARK_STUB_METHODS` (csv of method names to register default-response handler for), `ARK_STUB_METHOD_NOT_FOUND_LIST` (csv of advertised-but-unimplemented markers). Documented at top of `src/bin/ark-stub-ext.rs`.
- `register_default_handler(builder, method)` match-table maps 12 Phase-2 method names onto `with_method(name, |req| Ok(<Resp>::default()))` closures. Unknown methods fall through — trait default returns `method_not_found`, which is correct parity behaviour against in-process stub.
- StackSpawnPaneResponse has no `Default` (carries HandleId) — construct with fixed `"stub-spawned"` handle. Other Phase-2 responses all derive Default.
- Parity test `crates/ark-ext-test-support/tests/subprocess_parity.rs`:
  - Build matching in-proc StubExtension via same manifest + capability + handler set.
  - Spawn `ark-stub-ext` child with piped stdin/stdout, `stderr = inherit` so bin panics surface in test output.
  - `rpc(stdin, stdout, id, method, params)` helper sends one JSON-RPC 2.0 request line, reads exactly one response line back. `NdjsonServer::serve` already flushes each response — no extra buffering hacks needed.
  - 4-step scripted sequence: `pane/emit`, `pane/close`, `on_session_end` (all implemented on both legs) + `stack/clear` (unimplemented on both legs). Assert (a) subprocess `result` == `serde_json::to_value(in_proc_typed_response)` for the first 3; (b) subprocess error code == `-32601` for the 4th AND in-proc returns `ExtensionError::MethodNotFound`.
  - Shutdown: drop stdin → child sees EOF → `NdjsonServer::serve` returns → process exits 0. `child.wait()` asserts `status.success()`.
- GOTCHA: `CARGO_BIN_EXE_ark-stub-ext` was expected per cargo docs but cargo 1.93.0 on this workspace does not set it for integration tests (even with `cargo test -p <crate>`, no `CARGO_BIN_EXE_*` vars were present). `locate_bin()` falls back to `current_exe()` two-parents-up (`target/<profile>/deps/<testname>-<hash>` → `target/<profile>/ark-stub-ext`) with a clear diagnostic if the bin is missing. Panic message tells caller to run `cargo test -p ark-ext-test-support` (which builds bins) or explicit `cargo build -p ark-ext-test-support --bin ark-stub-ext`.
- Not touched: supervisor, scene, cli, config, or any other crate. Only `crates/ark-ext-test-support/**` + 2 ledgers.
- Validation: `cargo build -p ark-ext-test-support --bin ark-stub-ext` clean; `cargo test -p ark-ext-test-support --test subprocess_parity` 1/1 pass; `cargo test -p ark-ext-test-support` 8 unit + 1 integration + 1 doctest all green; `cargo build --workspace` clean; `cargo test --workspace --lib` all green (no regressions, tests count steady).

### Wave T-041 — 2026-04-18 — Tier 7 (tests R5)
- T-041 (soul phase 2 tests R5): KDL-level view-type compile-error goldens. DONE.
- New crate: `crates/scene-macros/` (proc-macro, `proc-macro = true`); workspace member. Deps: `syn 2`, `quote 1`, `proc-macro2 1`, `kdl 6.5`, `ark-ext-metadata-types` (path). No `ark-scene` dep — breaks would-be cycle via duplicate-~30-LOC ViewTypeTable slice (documented at top of `src/lib.rs`).
- `ark_scene_macros::validate_scene!` proc-macro: structured input (`manifests: [...]`, `scene_path: "..."`, `scene: "..."`). Parses inline manifest + scene KDL via `kdl` crate directly (bypasses facet-kdl's bare-`item` Vec<T> disambiguation issue). Walks scene nodes, calls internal validator mirroring `validate_view_reference`, emits `compile_error!("<scene_path>:<line>:<col>: <plain-English>")` on any violation. Converts byte offset → 1-indexed line:col via newline-count.
- Re-exported as `ark_scene::validate_scene` — fixtures write `use ark_scene::validate_scene;`.
- Four R5 compile-fail fixtures + stderr goldens under `crates/scene/tests/ui/`: undeclared_view_type, view_type_mismatch_on_handle_attr, stack_child_under_non_stack_parent, handle_typed_attr_takes_non_handle. Each stderr has 1× `.kdl:line:col` hit (grep-asserted per kit R5).
- Two compile-pass fixtures (valid_pane_and_stack_decls, cross_ext_view_reference) extended with a green-path `validate_scene!` invocation so the macro's happy path is pinned.
- Existing 2 Rust-level compile-fail fixtures (view_decl_wrong_field_type, metadata_missing_views_field) KEPT — per kit instructions.
- `view_types_trybuild.rs` harness updated: 6 compile-fail + 2 compile-pass; doc block points at `TRYBUILD=overwrite` for blessing.
- Green: ark-scene 597 tests P, workspace `cargo test --workspace --lib` 1807 P, `cargo check --workspace --tests` 0 errors.
- Next: T-042 integration + T-043 proto bump + T-044 CI gate.

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

### Iteration 22 — 2026-04-15 (Tier 3 Codex tier-gate)
- Codex review vs 5ac148f. Cycle 1 surfaced 4 real findings (F-085 P1 engine install_observability fabricated AgentId; F-086 P1 kill_handler subscribes too late; F-087 P1 run_supervisor_with never installs SIGTERM handlers; F-088 P2 finalize_state maps Killed/Timeout → Done). Fixed commit 6e2c3da: widened Engine::install_observability signature to accept AgentId; TabRegistry shared state fed by bus task; install_signal_handlers wired; Phase::Killed + Phase::Timeout added. Cycle 2 surfaced 2 real findings (F-423 P1 review_tab emits TabClosed on close_tab error; F-424 P2 git_diff dedupe broken on revert cycle). Fixed commit 4ec7c90: guard TabClosed on Ok, HashSet-retain dedupe. Workspace 695/695 pass --test-threads=1 (6 real fixes total this gate). Parser garbage F-064-F-084 + F-402-F-422 ignored (codex-findings.sh parsing bug — emits old findings from fixtures). Known flaky test logged in dead-ends.md. Gate complete per 2-cycle policy. Advance to Tier 4.

### Iteration 21 — 2026-04-14 (Tier 2 deferred sweep)
- Swept the three Tier 2 deferred codex findings (F-058, F-059, F-060) in one commit. F-058 (P1 command injection): added `cmd_argv` field to HookEntry + RenderedCommand enum, dispatcher now prefers direct-exec via `Command::new(argv[0]).args(&argv[1..])` when cmd_argv is set; shell (cmd) path now runs every interpolated value through `shlex::try_quote` before substitution so metachars become literal quoted tokens; one-shot warn emitted first time a shell-form hook with `{{var}}` templates fires. Added `shlex = "1"` workspace dep. F-059 (P1 data loss): `restore_settings` no longer deletes live settings.local.json when no backup exists — logs a warn and returns Ok, preserving user data. Updated the existing test that had encoded the wrong contract. F-060 (P2 pair invariant): new `emit_permission_pair_synthetic` helper fires Asked + Resolved in order for stdin-read-error / empty-stdin branches; malformed-JSON branch already correct, locked with regression test. Tests: +12 f058 in ark-config, +6 f058 in ark-core, +3 f059 in ark-engines-claude-code, +6 f060 in ark-hook = **+27 new tests, workspace 686/686 passing** (was 658). Build P, fmt P, clippy unchanged (one pre-existing warning in ark-types/permission.rs:108 unrelated to sweep). dead-ends.md updated to mark all three resolved.

### Iteration 20 — 2026-04-15 (Tier 3 W5 close)
- Single task wave. T-083 (cavekit done-signal resolver, commit 6467b97) wires all 5 watchers into CavekitOrchestrator::run via JoinSet, adds ImplTrackingSnapshot via tokio watch channel, resolve_done_outcome implements R9 cases: all-DONE → Success, pending → 60s grace → Success-if-transition-else-Failed, no-build-site → Success empty, cancel → Killed with child-tab cascade. trim_artifacts dedupes/sorts/caps at 100. factory.rs swapped CavekitOrchestratorStub → real CavekitOrchestrator. 8 new tests incl. timeout path via tokio::time::pause. **TIER 3 COMPLETE 22/22.** Workspace 658/658 pass (+111 vs T2 close). Next: Codex tier-gate review vs 5ac148f.

### Iteration 19 — 2026-04-15 (Tier 3 W4)
- 2 parallel packets. T-078 (build-site total extractor for Progress events, commit dad48bd, 14 tests, strict regex excludes coverage-matrix rows, domain-filename correlation) and T-080 (review tab spawn/close on PhaseTransition, commit 9b1e837, 11 tests, default matcher "review"/"check"/"inspect"). Then T-081 (codex findings watcher, commit 60d7bc3, 17 tests, synthetic codex reviewer AgentId, F-ID dedupe, NO_FINDINGS sentinel skip).

### Iteration 18 — 2026-04-15 (Tier 3 W3)
- 2 parallel packets. T-072 (auto-close policy on outcome with StubMux tests, commit ac910ca, 14 tests) and T-077+T-079+T-082 (3 cavekit watchers: impl_tracking 500ms debounce + Progress/TaskDone, ralph_loop PhaseTransition+Iteration, git_diff numstat with dedupe, commit 1ecb197, 45 tests). Workspace 633/633.

### Iteration 17 — 2026-04-15 (Tier 3 W2)
- 2 parallel packets. T-066+T-067 (SupervisorCommandHandler with Status/Kill/ForceKill/Rename/Forget/Ping, signal_hook SIGTERM/SIGINT cleanup + ControlSocketGuard, AgentStatus.hide field added, commit 01a1c47, 17 tests). T-069 (supervisor R3 full 18-step boot sequence — state→lock→socket→logging→config→factory→ensure_session→preflight→consumers→install_observability→Started→orchestrator.run→drain→teardown→finalize→exit, commit 932eba1, factory.rs + orchestration.rs + minimal ClaudeCodeEngine Engine impl in engines/claude-code). Workspace 535/535. Also ran T-063+T-065 (foreground mode + control-socket bind, commit 86666bb, 14 tests) in prior wave.

### Iteration 16 — 2026-04-15 (Tier 3 W1 + restructure)
- 2 parallel packets. T-062+T-064 (new crates/supervisor crate — daemonize double-fork+setsid + acquire_lock fd-lock + process-local registry for same-proc reacquire, commit ed85ca9, 9 tests + 1 ignored integ) and T-073+T-074+T-075 (ClaudeCodeOrchestrator detect+run with git-diff artifact + CavekitOrchestrator detect 4-heuristic, commit 471fa23, 26 tests). Then user-directed restructure: flatten crate paths, drop ark- prefix, nest multi-member families (engines/, orchestrators/, mux/, plugins/). git mv preserves 100% rename fidelity. Commit b996e5a. Workspace 481/481 pass (+36 vs T2 close). TIER_3_START_REF=5ac148f. Next: T-063 (supervisor --no-detach) + T-065 (control-socket bind) — only 2 unblocked, chain heavy.

### Iteration 15 — 2026-04-14 (Tier 2 Codex tier-gate)
- Codex review vs a083009. Cycle 1: F-044 (P1 ark-hook bypasses permission_policy — security) + F-045/F-046/F-047 (P2s). Fixed in commits 3f17fd1 (F-044: promoted PermissionPolicy to ark-types, maybe_emit_permission_decision gates stdout per policy) + dd393ca (F-045 both-targets pipe, F-046 strip all ark hooks on re-inject, F-047 lazy status bootstrap). Cycle 2: F-053 (P1 missing PermissionResolved pair) + F-054 (P2 late-Started phase regression). Fixed in c9411f2 + 1849b67. Cycle 3 verify surfaced 3 NEW findings: F-058/F-061 (P1 command injection in hook_dispatcher sh -c render, latent from T-061), F-059/F-062 (P1 restore_settings deletes user-managed settings.local.json when no backup, latent from T-052), F-060/F-063 (P2 regression from F-053 fix: stdin-read-fail + empty-stdin branches emit Resolved without Asked). **Deferred by user decision — advance to Tier 3 per 2-cycle policy.** F-058, F-059, F-060 tracked in dead-ends.md for Tier 3+ sweep. Tier 2 complete: 16/16 tasks DONE, 445/445 workspace tests, gate-ADVISORY.

### Iteration 14 — 2026-04-14 (Tier 2 W4 — closeout)
- 2 parallel packets. T-051 (ark-hook fail-open-for-permission invariant audit + 8 regression tests, commit fa50341, now 55/55 in crate; ensure_permission_allow helper routes every fail-open branch), T-054 (permission policy enum + decide + emit_permission_events + policy file read/write, commit 7ff4aaf, 16 new tests in ark-engines-claude-code; no ark-config dep, callers parse String at boundary). **TIER 2 COMPLETE 16/16.** Workspace 405/405 pass (+140 vs Tier 1 close). Next: Codex tier-gate review vs a083009.

### Iteration 13 — 2026-04-14 (Tier 2 W3)
- 2 parallel packets. T-048+T-049+T-050 (ark-hook writer.rs/pipe.rs/allow.rs + run.rs rewrite taking &mut impl Write for stdout, commit bc4b144, 47 tests in crate; state-root injection via run_with_state test seam; pipe_with fn injection for zellij-free tests; ALLOW_PAYLOAD_JSON const locked byte-equal), T-056+T-057 (stall_watcher + EngineHandle with JoinSet+restore_settings teardown, commit e3b545c, 50/50 in crate; chrono + tokio test-util added). Workspace 381/381. Next: W4 close tier with T-051+T-054.

### Iteration 12 — 2026-04-14 (Tier 2 W2)
- 3 parallel packets. T-047 (ark-hook typed payload parser + translator, commit 8ac7df2, 17 new tests = 29 total in crate), T-053+T-055 (transcript tailer + done watcher, commit 69c3445, notify-based inotify + JSONL parser, DoneSignal enum + mpsc done_watcher; assumed encoded-cwd = "/"→"-" overridable via ARK_CLAUDE_PROJECTS_DIR), T-058 (preflight with injectable test fn, commit d0bfec6, 7 tests, no `which` dep — uses env PATH walk + HOME). Workspace 352/352 pass. Next: Tier 2 W3 = T-048/T-049/T-050 (hook JSONL + pipe + allow payload) and T-056/T-057 (stall watcher + EngineHandle).

### Iteration 11 — 2026-04-14 (Tier 2 W1)
- 3 parallel packets. T-046 (ark-hook skeleton, commit a7a289c, 12 tests, 6 files), T-052 (settings.local.json injection, commit 9370e71, 12 tests, sha256 checksum + .ark-backup + deep-merge), T-059+T-060+T-061 (consumers state_writer/status_pipe/hook_dispatcher, commit 2c43e37, 10 tests, ark-core+=ark-config dep no cycle, F-037 closed Lagged(n) warn-log in every recv loop). Packet A paused for solution-set fork on clap exit-2 behavior; user picked B (keep Cli::parse, exit 2 only on arg-validation = loud setup bug; all runtime errors still fail-open). Build P, tests 308/308 workspace. Next: T-047 (hook payload parser, deps T-046+T-005 met) + T-053 (transcript tailer, deps T-052+T-005 met) + T-058 (engine preflight, deps T-046 met).

### Iteration Tier-0 — 2026-04-18 — phase-2
- Wave 1 (Tier 0): 4 parallel general-purpose opus agents. T-001/T-002/T-003/T-045 all COMPLETE. Commits: 20c21e6, b8e07ab, 8547655, 1544dab. Build P, Tests P (1648).
- Codex tier-gate: 3 findings. F-003 fix 3133529. F-001 deferred→T-046 (facet-kdl limitation, pre-existing). F-002 deferred→T-043 sequenced.
- Next: Tier 1 — T-004..T-007 (ark-view type primitives: HandleKind, HandleId, View traits, InvalidationCause).

### Iteration Tier-1 — 2026-04-18 — phase-2
- Wave 2 (Tier 1): 3 parallel general-purpose opus agents. T-004+T-005/T-006/T-007 all COMPLETE. Commits: 6f31378, e913cb5, 541db89. Build P, Tests P (1666).
- Codex tier-gate: 3×P2 (all P2, gate PROCEED). F-004/F-005/F-006 fixed inline: dc90de0. HandleId internals now private; doc-comments no longer overclaim wire-compat from #[non_exhaustive].
- Next: Tier 2 — T-008..T-013 (Pane<V>, Stack<V>, TabHandle, PaneLike, marker-gated impls, ParamsHash, SuppressionPolicy).

### Iteration Tier-2 — 2026-04-18 — phase-2
- Wave 3a: T-008 (0ffc222) + T-012 (5b33711) parallel. Wave 3b: T-009/T-010/T-011 bundle (a904a98 + trybuild) + T-013 (cc5c02f) parallel. Build P, Tests P (1700).
- Codex tier-gate: 1×P2 (F-007 spawn_pane aliasing). Fixed inline. Gate PROCEED.
- Next: Tier 3 — T-014..T-017 (HandleGone ExtensionError, handle.invalidated event wire, SessionHandles, cross-crate exports).

### Iteration Tier-3 — 2026-04-18 — phase-2
- Wave 4a: T-014 (bcd38e2) + T-015 (09676d1) + T-016 (753f91f) parallel. Wave 4b: T-017 (5d157e3). Build P, Tests P (1714).
- Codex tier-gate: 2 findings. F-008 [P1] NDJSON HandleGone decoder regression + F-009 [P2] SessionHandles view-type enforcement. Both fixed (1094a0a). Gate PROCEED.
- Next: Tier 4 — T-018..T-023 (6 RPC pairs + lifecycle hooks + feature-group hooks + ViewDecl extension).

### Iteration Tier-4 — 2026-04-18 — phase-2
- Wave 5: T-018+T-019+T-021+T-022 (ad001b7) + T-023 (d6a67bf); T-020 unblocked (7a24239); doctest fix (8815508). Build P, Tests P (1721).
- Codex tier-gate: 2 findings. F-010 [P1] kind unconsumed — warn-log added + T-034 deferral. F-011 [P2] spec gen tooling — deferred. Gate: accept deferrals, advance.
- Next: Tier 5 — T-024..T-027 (capability taxonomy + #[derive(View/CommandView/ZellijView)] + auto-advertise on Extension derive).

### Iteration Tier-5 — 2026-04-18 — phase-2
- Wave 6a: T-024 (117af17) + T-025 (380700c) parallel. Wave 6b: T-026 + T-027 (d691db7) bundled. Build P, Tests P (1723).
- Codex tier-gate: 3 findings. F-012/F-014 fixed inline (bfde279 — generics via split_for_impl, attribute-rename doc). F-013 accepted — theoretical breakage, no production users.
- Next: Tier 6 — T-028..T-036 (supervisor-side: capability dispatcher + load sequence + list/doctor + config + scene validator + reload gate + suppression map — 9 tasks, host-dispatch kit).

### Iteration Tier-6 — 2026-04-18 — phase-2
- Waves 7a-e, 9 tasks. Serial after 2 git-race collisions (race pattern saved: feedback_parallel_git_collision.md).
- Commits: c5dbf78 (T-028), a949d40 (T-029+T-033 bundled), 5d742b7 (T-030+T-031+T-032 bundled), afa46d3 (T-034), 890dd17 (T-035), 59c1293 (T-036). Build P, Tests P (1799).
- Codex tier-gate: F-015 [P1] fixed (4a594b7) — dispatcher opt-out on method_not_found. F-016 [P2] deferred to post-stub-harness.
- Next: Tier 7 — T-037..T-042 (stub harness + matrices + trybuild + integration tests).

### Iteration Tier-7-partial — 2026-04-18 — phase-2
- Wave 8a (serial): T-037 (3863b1a) ark-ext-test-support stub crate. 9 tests green.
- 7 tasks remain (T-038..T-044). Handoff written: handoff-2026-04-18-phase-2-tier-7-mid.md.
- User rejected T-041 dispatch (trybuild deferral language) — next session should consider option (b) full KDL-level validator, or coordinate with user before defaulting to option (a).
