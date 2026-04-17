---
created: "2026-04-15"
last_edited: "2026-04-15"
---

# Build Site: Supervisor Wiring (Phase 7)

## Why this exists

The supervisor library (`crates/supervisor/`) is fully implemented — `daemon.rs`, `lock.rs`, `control_socket.rs`, `commands.rs`, `signals.rs`, `kill.rs`, `crash.rs`, `auto_close.rs`, `factory.rs`, `orchestration.rs`, `audit_log.rs`, `foreground.rs`. The orchestration entry point `run_supervisor(spec, mode, config) -> Result<Outcome>` exists. T-062 through T-072 in `build-site.md` landed as library code.

What never landed: the **wiring**. `ark` warns `supervisor launch is stubbed in this build` at `crates/cli/src/commands/launch.rs` and prints `spec.json` path. No supervisor process exists, so no `events.jsonl` is written, no `status.json` gets updated, hooks never fire, `ark kill` has no PID to signal, the picker plugin sees no live agents.

> **Note (v3):** `spawn.rs` was deleted; session launch wiring now lives in `launch.rs`. Bare `ark` replaces `ark spawn`.

**Architecture clarification (post-discovery):** the `daemonize()` function in `crates/supervisor/src/daemon.rs:81` is an *in-process* double-fork. After it returns, the same `ark` binary is running in two processes — `Parent` (the original `ark` invocation) and `Daemon` (the grandchild supervisor). There is **no separate `ark-supervisor` binary** and the original kit never specified one. The Daemon path runs `run_supervisor(spec, mode, config)` directly via a tokio runtime built post-fork.

This site closes that wiring gap. F-730's lessons (zellij client needs a TTY) do **not** apply here: the supervisor is headless, so the existing `daemon.rs` double-fork + setsid + null-stdio path is correct as-is.

## Cavekit traceability

All requirements are already defined in `context/kits/cavekit-supervisor.md`:

- **R1** (fork + detach) — `daemon.rs::daemonize` exists; missing = binary calling it.
- **R3** (orchestration sequence, step 12 "signals readiness to parent CLI") — *unimplemented*. Need a ready-signal protocol.
- **R7** (control socket lifecycle) — bind code exists; readiness coupling to parent is the new wrinkle.
- Cross: `cavekit-cli.md` R2 (bare `ark` + fork supervisor + <1s parent return) — currently violated.
- Cross: `cavekit-distribution.md` R4 (binstall + brew + tarball layout) — needs `ark-supervisor` added.

No new requirements. No new kits. Pure plan-tier work.

## Launch pipeline ordering (post-F-730, in-process model)

The full launch happy path after this site lands:

1. CLI `run()` parses args, derives orchestrator + name + env + hooks.
2. CLI calls `require_zellij_on_path()` — preflight. Failure → no state mutation.
3. CLI builds `AgentSpec`, mints `AgentId`, writes `spec.json` + renders `layout.kdl` to `$STATE/agents/{id}/`.
4. **NEW**: CLI creates a `pipe2(O_CLOEXEC)` ready-pipe `(rfd, wfd)`. Both fds live in the current process.
5. **NEW**: CLI calls `daemonize(state_layout, agent_id)` (in-process double-fork). On return:
   - **Parent branch** — close `wfd`; continue at step 6.
   - **Daemon branch** — close `rfd`; build a single-threaded tokio runtime; `block_on(supervisor_main(spec, config, wfd))`; exit with `outcome_exit_code(outcome)`. Detailed below.
6. **NEW (Parent)**: poll `rfd` with 5s timeout. Success = ACK byte received. Failure = EOF before ACK / error byte / timeout. Failure path: `cleanup_agent_state` + `CliError::Internal { reason: "supervisor failed to ready within 5s" }`.
7. CLI launches zellij — inside via `switch-session`, outside via pty (per F-730).
8. CLI prints `spawned {id} -> Ctrl+o w to switch`, exits 0.

The Daemon branch (`supervisor_main`) does:

a. Run orchestration sequence per `cavekit-supervisor.md` R3 steps 1–11 (StateDir, lock, control socket bind, logging-already-installed-by-`setup_supervisor_log`, config, factory, ensure_session, preflight, consumer spawn, install_observability, Started event).
b. Write 1-byte ACK (0x06) to `wfd` + close `wfd`. Parent's `read()` unblocks with success.
c. Continue into `orchestrator.run` step 13 → drain → teardown → finalize → unlink socket → release lock → return Outcome.

Order rationale: supervisor must bind the control socket and emit `Started` **before** signalling ready, because the picker plugin and any concurrent `ark list` / `ark kill` will look for both the socket and `status.json.phase=="Started"` as proof-of-life. Zellij can launch after the supervisor is ready, since the layout's `ark pane log --id <id>` panes tail `events.jsonl` which the supervisor is now writing to.

**`--no-detach` variant**: when `args.no_detach` is set, the CLI does NOT call `daemonize()`. Instead it builds a tokio runtime directly in the current process and runs `supervisor_main(...)` inline (still using the pipe — but trivially, since both ends are in the same process; the ack just unblocks the same task that wrote it). After ready, the CLI launches zellij in foreground (existing F-730 inherited-stdio path) and waits for both supervisor and zellij to exit. Useful for debugging and CI; never the user's default.

## Ready-signal protocol

**Decision required from user before W-2 lands.** Three viable mechanisms:

| Option | Mechanism | Pros | Cons |
|--------|-----------|------|------|
| **A. Pipe inheritance** | Parent creates `pipe2(O_CLOEXEC)`, passes write fd to supervisor via env var. Supervisor closes write fd on success, parent's read fd EOFs. Failure → supervisor writes 1 byte before exit. | Race-free. Parent blocks on `read()`, no polling. | Cross-platform fragility (works on POSIX only — fine for v1). Custom code. |
| **B. Socket file existence + bound check** | Parent polls `$XDG_RUNTIME_DIR/ark-$UID/agents/{id}.sock` until `connect()` succeeds, with 5s timeout. | Reuses socket binding as the ready signal. No new IPC. | Polling overhead (50ms ticks). Race window between bind() and listen(). |
| **C. Status.json poll** | Parent polls `$STATE/agents/{id}/status.json` until `phase == "Started"`. | Uses existing state-writer output. | Slowest readiness signal — Started event must propagate through event bus → state_writer → fsync. Worst-case 100s of ms. |

Recommend **A** for race-freedom; **B** is the reasonable fallback. Both fit `<1s parent return` (R1).

## Tasks

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| W-1 | Add `supervisor_main(spec, config, ready_writer) -> Outcome` helper in `crates/supervisor/src/lib.rs` (or new `crates/supervisor/src/bootstrap.rs`): builds engine/orch/mux factories, runs orchestration steps 1–11, signals ready via the writer, then runs orchestrator.run + finalize. Wraps `run_supervisor` from `orchestration.rs` with the ready-signal hook. | supervisor | R1, R3 step 12 | none | M |
| W-2 | Pipe-inheritance ready-signal helpers. **Supervisor side** (`crates/supervisor/src/ready_signal.rs`): `ReadyWriter` newtype wrapping the `wfd`; `write_ack()` writes 0x06 + closes; `Drop` closes without ack (failure signal). **CLI side** (`crates/cli/src/supervisor_handoff.rs`): `create_ready_pipe() -> (ReadFd, WriteFd)` via `nix::unistd::pipe2(O_CLOEXEC)`; `wait_for_ready(rfd, Duration) -> Result<(), CliError>` uses `nix::poll::poll` with timeout, reads 1 byte, distinguishes ACK / EOF / error byte / timeout. Default `READY_TIMEOUT_MS = 5000`. | supervisor, cli | R1, R3 step 12 | W-1 | M |
| W-3 | Wire `ark` launch fork at `launch.rs`. Replace stub with: create ready pipe; call `daemonize(state_layout, agent_id)`. Match outcome — Parent: close wfd, `wait_for_ready(rfd, 5s)`. Daemon: close rfd, build single-thread tokio runtime, `block_on(supervisor_main(spec, config, ReadyWriter::new(wfd)))`, `std::process::exit(outcome_exit_code(outcome))`. Cleanup state on ready timeout (mirror F-528 invariant). | cli, supervisor | R1, R3, cavekit-cli R2 | W-1, W-2 | L |
| W-4 | Pipeline ordering: in `launch.rs::run()`, sequence is (a) preflight zellij (b) write spec.json + render layout (c) `daemonize` + Daemon-branch supervisor / Parent-branch wait_for_ready (W-3) (d) inside-zellij switch-session OR outside pty spawn (post-F-730 paths) (e) print id + return. Document order in module comment. `--no-detach` variant: skip `daemonize()`, run `supervisor_main` inline in current process via tokio `LocalSet`, then foreground zellij. | cli, supervisor | R1, F-730 | W-3 | M |
| W-8 | Integration test `crates/cli/tests/launch_e2e.rs::launch_creates_supervisor_artifacts`: tempdir as `$ARK_STATE_DIR`; invoke `ark --no-detach --scene claude-code --cwd <tmp>`; within 5s assert `$STATE/agents/{id}/spec.json` exists, `$STATE/agents/{id}/status.json` exists with `phase == "Started"`, control socket file at `$XDG_RUNTIME_DIR/ark-$UID/agents/{id}.sock`, PID file `$STATE/agents/{id}/pid` alive (`kill(pid, 0)` returns Ok). Then `ark kill <id>` and assert all four cleaned within 10s. Gate behind zellij-on-PATH. | cli, supervisor, testing | R1, R3, R4 | W-3, W-4 | L |
| W-9 | Document the wiring + ready-signal mechanism in a new `context/impl/impl-supervisor-wiring.md` (Tier 4 tracking doc): pipe-inheritance choice + rationale (research summary, prior art in zellij/wezterm), in-process daemon model rather than separate binary, pipeline order, tests, F-731 follow-up (`crates/mux/zellij/src/mux.rs:281` external-setsid). | impl-tracking | n/a (Tier 4) | W-8 | S |

**Tasks deleted from original plan:**

- ~~W-5 (separate binary manifest)~~ — no separate binary; in-process daemon model.
- ~~W-6 (release.yml + brew for `ark-supervisor`)~~ — same reason.
- ~~W-7 (`ark doctor` check for `ark-supervisor`)~~ — same reason.

## Tier ordering

```
W-1 (supervisor_main helper)
  └── W-2 (pipe ready-signal helpers)
        └── W-3 (daemonize fork in spawn.rs)
              └── W-4 (pipeline order doc + --no-detach)
                    └── W-8 (e2e test)
                          └── W-9 (impl tracking)
```

Strictly serial. Each task touches code the next depends on; no parallelism worth orchestrating.

## Out of scope for this site

- **`crates/mux/zellij/src/mux.rs:281` external-setsid invocation** — flagged as F-730 follow-up. The supervisor doesn't trigger this code path (orchestration uses `ZellijMux::create_tab`, which only calls `switch-session` inside-zellij or the outside-setsid path). Will need fixing before the supervisor's outside-zellij `ensure_session` actually runs in production. Treat as a separate F-731.
- **Replacing `setsid + null stdio`** in `daemon.rs` — supervisor is headless (no TUI, no pty). The current daemon mechanics are correct. Do not apply F-730's pty fix here.
- **Picker → `ark` exec model** (`cavekit-overview.md` principle 5) — the picker subprocess-execs `ark` to create new agents. That path now needs the wired supervisor to be useful, but the picker itself doesn't change.
- **Multi-supervisor reaping** — out of scope; each supervisor is independent per kakoune model.

## Open question

W-2 needs a ready-signal mechanism choice (A/B/C above). The plan as written assumes the user picks one before W-2 starts.

## Verification

After W-1 → W-9 land:

1. `cargo build --workspace` produces three binaries: `ark`, `ark-hook`, `ark-supervisor`.
2. `cargo test --workspace -- --test-threads=1` green including W-8.
3. Manual: `ark --scene claude-code` from a bare shell — within 1s the shell prompt returns; `ark list` shows the agent in `Started` phase; `$XDG_RUNTIME_DIR/ark-$UID/agents/{id}.sock` exists; `Ctrl+o w` from any zellij client reaches the new session.
4. `ark kill <id>` — within 10s, control socket gone, PID gone, `status.json.phase == "Killed"`.
5. `ark doctor` reports `ark-supervisor: ok`.
6. `cargo dist build` (or whatever cargo-dist's local equivalent is) produces a tarball containing `ark-supervisor`.
