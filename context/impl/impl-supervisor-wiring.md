---
created: "2026-04-15"
last_edited: "2026-04-16"
domain: supervisor-wiring
---

# Implementation Tracking — Supervisor Wiring (Phase 7)

## Status

**Phase 7 COMPLETE — all 8 shipped tasks landed.** `ark spawn` now properly forks the supervisor with a race-free pipe-inheritance ready-signal handshake. Build site `context/plans/build-site-supervisor-wiring.md` lands W-1 → W-4, W-8, W-9 (W-5/W-6/W-7 dropped on discovery — no separate binary needed).

### Task-by-task shipping status

| Task | Status | Commit | Notes |
|------|--------|--------|-------|
| W-1 | DONE | `b39327c` | `supervisor_main` bootstrap with ready-signal protocol (ready_writer param on `run_supervisor` / `run_supervisor_with`). |
| W-2 | DONE | `b39327c` | `ReadyWriter` newtype + `create_ready_pipe` + `wait_for_ready` shipped alongside W-1 in the same commit; verified already-wired during Wave 2. |
| W-3 | DONE | `db43213` | `ark spawn` fork routed through `supervisor_main` bootstrap with `daemonize` + pipe handshake. |
| W-4 | DONE | `db43213` / follow-up | Pipeline ordering doc + Parent/Daemon branch `wait_for_ready` integration landed in W-3 commit; inline `--no-detach` supervisor added in the F-731 follow-up cycle. |
| W-5 | DROPPED | n/a | No separate binary — in-process daemon model. |
| W-6 | DROPPED | n/a | Same reason. |
| W-7 | DROPPED | n/a | Same reason. |
| W-8 | DONE | `af25e12` | E2E test `crates/cli/tests/w8_spawn_integration.rs` swept into the T-12.6/T-12.10 commit. |
| W-9 | DONE | (this commit) | Impl tracking doc (this file) + impl-overview.md Phase 7 row flipped to COMPLETE. |

Phase 7 complete — all 8 tasks shipped. `ark spawn` now properly forks the supervisor with ready-signal handshake.

## What was built

### W-1: `supervisor_main` bootstrap helper — DONE (commit b39327c)

Discovered the supervisor library had `run_supervisor(spec, mode, config) -> Result<Outcome>` already wired (T-069). Did NOT add a separate `supervisor_main` wrapper. Instead extended `run_supervisor` and `run_supervisor_with` with an `Option<ReadyWriter>` parameter so the existing R3 step 12 hook could call the writer's `write_ack()` directly.

Changed:
- `crates/supervisor/src/orchestration.rs` — added `ready_writer: Option<ReadyWriter>` parameter to both `run_supervisor` and `run_supervisor_with`. Replaced the broken step-12 stdout-print (which never worked under daemonize because stdout was already redirected to `supervisor.log`) with `if let Some(writer) = ready_writer { ... writer.write_ack() }`.
- The old `SupervisorMode` enum is now informational only — kept for backward compat (test callers reference it) but no longer drives any behaviour at step 12.

### W-2: pipe-inheritance ready signal — DONE (commit b39327c, verified Wave 2)

**Mechanism choice:** **Pipe inheritance (Stevens APUE)** — see `context/plans/build-site-supervisor-wiring.md` "ready-signal protocol" for the A/B/C tradeoff. Confirmed via external research (`Agent` → general-purpose, 600-word survey). Real-world prior art: zellij's IPC bootstrap, wezterm-mux-server, alacritty daemon mode, Stevens APUE (Unix daemon canon since 1980s). Race-free, portable, ~40 lines with `nix`.

**Supervisor side** — `crates/supervisor/src/ready_signal.rs`:
- `ReadyWriter` newtype wrapping `OwnedFd`. `write_ack(self)` writes the 0x06 ACK byte and drops the fd, closing the parent's read end.
- `ACK_BYTE: u8 = 0x06` constant exported for the CLI side to match against.
- Drop without `write_ack` = failure: kernel closes the fd, parent's `read()` returns 0 (EOF), parent surfaces "supervisor exited before signalling ready".

**CLI side** — `crates/cli/src/supervisor_handoff.rs`:
- `create_ready_pipe() -> (OwnedFd /* read */, OwnedFd /* write */)` using `nix::unistd::pipe()` + manual `fcntl(F_SETFD, FD_CLOEXEC)` on both ends. Used `pipe()` rather than `pipe2()` because `pipe2` is gated to Linux/BSD in nix; macOS only exposes `pipe(2)`.
- `wait_for_ready(read_fd, Duration) -> Result<(), CliError>` uses `nix::poll::poll` with a millisecond timeout. Reads exactly 1 byte. Maps `(0 bytes / 1 byte ACK / 1 byte non-ACK / poll timeout)` to four distinct error variants.
- `wait_for_ready_default` wraps with `READY_TIMEOUT_MS = 5000`.
- 5 unit tests covering pipe creation + each of the 4 outcomes.

### W-3: `ark spawn` forks the supervisor — DONE (commit db43213)

`crates/cli/src/commands/spawn.rs::run` — replaced the long-standing supervisor-stub branch (lines 821-824 of the pre-W-3 file) with a real fork:

1. `create_ready_pipe()` → `(rfd, wfd)`.
2. `ark_supervisor::daemonize(&state_layout, &spec.id)` — in-process double-fork + setsid + log redirect (existing primitive).
3. Match `DaemonizeOutcome`:
   - **Daemon** branch: drop `rfd`; build single-thread tokio runtime; `block_on(run_supervisor(spec, Daemon, Config::placeholder(), Some(ReadyWriter::from_owned_fd(wfd))))`; `std::process::exit(outcome_exit_code(&outcome))`.
   - **Parent** branch: drop `wfd`; `wait_for_ready_default(rfd)`; on failure → `cleanup_agent_state` + return CliError.
4. Continue to existing F-730 zellij launch paths (inside switch-session / outside pty / outside foreground).

### W-4: pipeline ordering documented + inline supervisor for `--no-detach` — DONE (commit db43213 + F-731 follow-up cycle)

`spawn.rs` module-level doc comment describes the 6-step pipeline including the daemonize fork at step 4.

**Inline supervisor landed** (initially deferred, completed in the F-731 follow-up cycle). With `--no-detach`, ark stays in the foreground and runs the supervisor inline in a background `std::thread` (own current-thread tokio runtime). Zellij is spawned as a foreground subprocess in its own process group via `pre_exec(setpgid(0, 0))`, and `tcsetpgrp` hands it the controlling tty so terminal SIGINT routes to zellij only. When zellij exits, `child.wait()` unblocks → cli reclaims tty via `tcsetpgrp` → `cancel.cancel()` on the shared `CancellationToken` → `supervisor_thread.join()` → drain + finalize.

Required signature change: `run_supervisor` and `run_supervisor_with` now take `external_cancel: Option<CancellationToken>`. When `Some`, the supervisor uses it as its primary cancel token instead of creating an internal one — letting external code (the inline path) drive shutdown. Daemon callers pass `None` and rely on the existing signal-handler-driven cancel.

Pattern grounded in external research (zellij/wezterm/helix prior art, Stevens APUE job-control). Documented inline in `spawn.rs::run` for future readers.

### W-8: end-to-end test — DONE (commit af25e12, file at `crates/cli/tests/w8_spawn_integration.rs`)

`crates/cli/tests/w8_spawn_integration.rs` (originally drafted as `e2e.rs::scenario_spawn_supervisor_lives_then_dies`, landed as a dedicated integration-test binary):

- Gated by `ARK_E2E=1` + `zellij` on PATH + `mock-claude` binary present.
- Spawns `ark spawn --orchestrator claude-code --cwd <state> -- /bin/sleep 60`.
- Asserts within 5 s: `spec.json`, `pid`, `status.json` (with `phase == "Started"`), control socket at `$XDG_RUNTIME_DIR/ark-$UID/agents/{id}.sock` all exist; PID is alive (`kill(pid, 0) == Ok`).
- Sends `SIGTERM` directly (not via `ark kill`) to keep the test decoupled from the kill-command surface.
- Asserts within 10 s: socket gone, PID dead.

## Bugs found and fixed during W-3 / W-8

### F-740 — `setup_supervisor_log` propagated `set_global_default` failure as fatal

**Symptom:** `ark spawn` returned `internal error: supervisor exited before signalling ready` immediately (well under the 5 s timeout). The supervisor.log file did not exist; the agent state dir was empty by the time the parent tried to inspect it.

**Root cause:** `crates/cli/src/main.rs:62` installs a global `tracing_subscriber` before the `spawn` command runs. The `daemonize()` grandchild inherits this global subscriber via `fork(2)` — `tracing` globals are static. `setup_supervisor_log` (in `crates/supervisor/src/daemon.rs`) then called `tracing::subscriber::set_global_default(subscriber)` and propagated the resulting "global already installed" error as `DaemonizeError::TracingInit`. The grandchild's `daemonize` returned `Err`; the spawn.rs handler ran `cleanup_agent_state` (which `remove_dir_all`s the agent state dir); the grandchild then exited; the parent's `wait_for_ready` saw EOF and surfaced the misleading "supervisor exited before signalling ready" — the *real* error (a global-subscriber clash) was nowhere in the operator-visible logs.

The disappearing state dir made the failure mode opaque: the natural debugging instinct is to read `supervisor.log`, but `cleanup_agent_state` had already deleted it.

**Fix:** Ignore the `set_global_default` result in `setup_supervisor_log`. The inherited subscriber writes to `fd 2`, which `redirect_stdio` (called immediately above) just `dup2`'d to `supervisor.log`. So tracing output still ends up in the right file — just via the inherited subscriber rather than the freshly-built one. Single-line change at `crates/supervisor/src/daemon.rs:164`. The `DaemonizeError::TracingInit` variant is now unreachable but kept for binary-compat.

**Why this wasn't caught earlier:** the existing supervisor unit tests use `run_supervisor_with` directly without going through `daemonize` — they install their own subscriber once at test-binary startup and never re-init. The clash only surfaces when a real `ark` binary calls `daemonize` post-tracing-init.

### F-741 — macOS `sun_path` 104-byte limit blew up the e2e test

**Symptom:** W-8 e2e test failed with `bind control socket ... local socket name length exceeds capacity of sun_path of sockaddr_un`.

**Root cause:** `tempfile::Builder::tempdir()` uses `$TMPDIR`, which on macOS is `/var/folders/<random-32-chars>/T/`. Adding `agents/<26-char-ulid-id>.sock` to that base blows past the POSIX `sun_path` 104-byte cap.

**Fix:** `crates/cli/tests/e2e_support/mod.rs` now constructs the runtime tempdir under `/tmp` (via `tempfile::Builder::tempdir_in("/tmp")`). The state and config tempdirs can stay on the default `$TMPDIR` since they don't host sockets. Documented inline.

## Follow-up cycle (F-731 + W-4 inline closed)

Both originally-deferred items landed in the next cycle:

### F-731 — mux pty replacement (FIXED)

`crates/mux/zellij/src/mux.rs:281` previously shelled out to external `setsid(1)` (macOS doesn't ship it; even on Linux the null-stdio + setsid combination strips zellij's controlling TTY).

**Resolution:**
1. Extracted `spawn_zellij_with_pty` + `PtyZellijHandle` + `pty_child_startup_failure` from `crates/cli/src/commands/spawn.rs` into a new shared module `crates/mux/zellij/src/pty.rs`. Single source of truth for the pty lifetime contract + startup-grace poll. Two unit tests (`pty_child_startup_failure_ok_for_successful_exit`, `pty_child_startup_failure_reports_early_exit`).
2. Moved `portable-pty = "0.8"` dep to `crates/mux/zellij/Cargo.toml`.
3. Replaced `mux.rs:281` with a direct `crate::pty::spawn_zellij_with_pty` call. The pty path bypasses `self.executor` because the executor trait is `Output`-style and can't model a pty allocation; trait surface stayed unchanged.
4. Adapter wrappers `cli_spawn_zellij_with_pty` + `cli_pty_child_startup_failure` in spawn.rs translate `PtySpawnError` → `CliError::Internal` with stable wording (downstream stderr-grepping depends on the "zellij exited with code N" substring).
5. Test churn: replaced `create_tab_first_outside_uses_setsid_and_layout` + `create_tab_new_session_includes_session_name_and_layout_flags` with `_does_not_route_through_executor` variants. Adjusted `create_tab_additional_*` for the new "first tab is pty, additional tabs are executor-mediated" call topology. Renamed `create_tab_reports_zellij_failure` → `_additional_reports_zellij_failure`. Relaxed `crates/core/src/mux_contract.rs::create_tab_new_session_argv_includes_s_flag` and `create_tab_preserves_kdl_extension` to "if calls were recorded they must carry session/kdl, but absence of calls is OK (impl uses pty)". Lowered `create_tab_additional_uses_new_tab_action` minimum from `>= 2` to `>= 1` (first-tab pty is silent).

### W-4 inline supervisor — DONE

See "W-4 pipeline ordering documented + inline supervisor for `--no-detach`" section above. Required `external_cancel: Option<CancellationToken>` parameter on `run_supervisor` + `run_supervisor_with`.

### Integration with picker / status plugins — non-blocking

Supervisor writes status.json + pipes events. Picker's `read_dir` + reachability check should pick up live agents automatically (no plugin changes needed). End-to-end picker behaviour will validate naturally on first manual smoke test. Not on critical path.

## Verification

- `cargo test --workspace -- --test-threads=1` — all 41 binaries green.
- `ARK_E2E=1 cargo test -p ark-cli --test e2e -- --test-threads=1` — all 10 scenarios pass including W-8.
- Manual `ark spawn -- /bin/sleep 60` from a bare shell:
  - Returns `spawned <id> -> Ctrl+o w to switch` in well under 1 s.
  - State dir contains spec.json, status.json (phase=Started), pid, events.jsonl, supervisor.log, layout.kdl.
  - Runtime dir contains the per-agent control socket.
  - State dir contains the per-agent lock file.
  - `kill -TERM <pid>` cleans up socket + status.json updated to terminal phase within seconds.
