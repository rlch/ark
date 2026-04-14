---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Orchestrator — Cavekit

## Scope
The `CavekitOrchestrator` impl of the `Orchestrator` trait. Observes the external cavekit tool (Go CLI + slash commands + skills) operating inside a worktree. Spawns additional sibling tabs/panes (e.g., codex review tab) when cavekit transitions phases. No changes required to cavekit upstream — pure filesystem + process observation.

## Background
Cavekit is an external Go tool driving the Hunt lifecycle (Draft → Architect → Build → Check → Revise). The observable surface for ark is:
- `context/impl/impl-*.md` — task status table (DONE/PARTIAL/BLOCKED)
- `context/impl/loop-log.md` — iteration log
- `context/impl/dead-ends.md` — failed approaches
- `context/impl/impl-review-findings.md` — codex review output (severity-ranked)
- `context/plans/*.md` — build sites (task DAGs with tier structure)
- `context/kits/cavekit-*.md` — requirement specs
- `.claude/ralph-loop.local.md` — ralph iteration state
- `.cavekit/config` — shell key=value preset config
- git worktree state

Cavekit spawns claude agents inside the worktree; ark observes that claude session via `ClaudeCodeEngine` (hooks + transcript) simultaneously.

## Requirements

### R1: Detection
**Description:** Auto-detect that a cwd is cavekit-managed.
**Acceptance Criteria:**
- [ ] `detect(cwd)` returns true when ANY of:
  - `cwd/context/sites/*.md` or `cwd/context/plans/*.md` exists with at least one build site file
  - `cwd/.cavekit/config` exists
  - `cwd/context/kits/cavekit-*.md` exists
- [ ] Detection must not panic on missing permissions; returns false on I/O error
- [ ] Used by `--orchestrator auto` CLI flag
**Dependencies:** cavekit-cli

### R2: Engine declaration
**Description:** Uses the claude-code engine.
**Acceptance Criteria:**
- [ ] `engine()` returns `"claude-code"`
- [ ] Supervisor installs `ClaudeCodeEngine` before invoking `run`
- [ ] Orchestrator consumes engine events (Tool/Message/Stop/etc) from the shared event bus
**Dependencies:** cavekit-engine-claude-code

### R3: Builder tab + layout
**Description:** Opens the primary builder tab at spawn time using the configured layout.
**Acceptance Criteria:**
- [ ] Default layout: `config.orchestrator.cavekit.default_layout` (default `"builder"`)
- [ ] Calls `world.mux.create_tab(spec.session, "builder", layout_path)` once at start
- [ ] Layout is rendered with `cwd`, `agent_cmd`, `agent_args` substitutions
- [ ] Agent pane command = `spec.cmd` (e.g., `claude --resume`); user passes this at spawn
- [ ] Emits `TabOpened { role: Builder }` event on success
**Dependencies:** cavekit-mux-zellij, cavekit-layouts

### R4: Impl-tracking watcher
**Description:** Parse cavekit's impl-*.md status tables to emit Progress + TaskDone events.
**Acceptance Criteria:**
- [ ] `notify` crate watches `cwd/context/impl/impl-*.md` for create/modify/delete
- [ ] Parser reads markdown table rows: `| T-XXX | STATUS | notes |` where STATUS ∈ `DONE | PARTIAL | BLOCKED | IN PROGRESS | PENDING`
- [ ] Debounced at 500ms (many writes close together = single re-parse)
- [ ] Emits:
  - `TaskDone { task_id }` on STATUS → DONE
  - `Progress { done, total }` on any status change (done count = DONE+PARTIAL*0.5 per cavekit semantics, total from corresponding build site file in `context/plans/`)
- [ ] Total derived from parsing the matching build site: `context/plans/build-site.md` or `build-site-{domain}.md` by impl filename correlation
- [ ] Gracefully handles missing build-site file (total = None)
- [ ] Only active when `config.orchestrator.cavekit.watch_impl_tracking = true`
**Dependencies:** cavekit-types-state-events

### R5: Ralph loop watcher
**Description:** Parse `.claude/ralph-loop.local.md` to emit Iteration events.
**Acceptance Criteria:**
- [ ] `notify` watch on `.claude/ralph-loop.local.md`
- [ ] Parser extracts: `iteration: N`, `max_iterations: N`, `status: ...`, `started_at: ...`
- [ ] Emits `Iteration { n, max }` on change
- [ ] `PhaseTransition` when status field changes (e.g., `building` → `reviewing`)
- [ ] Only active when `config.orchestrator.cavekit.watch_ralph_loop = true`
**Dependencies:** cavekit-types-state-events

### R6: Phase detection + review tab spawn
**Description:** Detect when cavekit enters review phase and spawn a dedicated review tab.
**Acceptance Criteria:**
- [ ] Phase detection signals:
  - ralph-loop status transitions to `reviewing`
  - `context/impl/impl-review-findings.md` appears or mtime changes
- [ ] On review-phase entry (first detection), call `world.mux.create_tab(spec.session, "review", review_layout_path)` where review_layout = `config.orchestrator.cavekit.review_layout` (default `"review"`)
- [ ] Review tab closes when review phase exits (review file stops being updated for 30s or ralph status leaves `reviewing`)
- [ ] Emits `TabOpened { role: Reviewer }` and `TabClosed` accordingly
- [ ] Only active when `config.orchestrator.cavekit.spawn_review_tab = true`
**Dependencies:** R3, cavekit-layouts

### R7: Codex findings watcher
**Description:** Parse impl-review-findings.md for ReviewComment events.
**Acceptance Criteria:**
- [ ] `notify` watch on `context/impl/impl-review-findings.md`
- [ ] Parser reads markdown table: `| Severity | File | Line | Description |`
- [ ] Each row → `ReviewComment { reviewer: codex, severity, path, line, body }`
- [ ] `reviewer` id constructed as `{run_id}:codex` (synthetic subagent id for cross-ref)
- [ ] De-dup across re-reads: track `(severity, path, line, body-hash)` set to avoid re-emitting
- [ ] Findings roll into `AgentStatus.findings` counts automatically via state writer
**Dependencies:** cavekit-types-state-events, R6

### R8: Worktree / git observation
**Description:** Watch git diff for FileEdited events (complementing engine's transcript-based ones).
**Acceptance Criteria:**
- [ ] `notify` watch on `.git/index` + poll every 5s as fallback
- [ ] Run `git diff --numstat` to get `(additions, deletions, path)` per file since last check
- [ ] Emit `FileEdited` for new entries (dedupe against transcript events by path+timestamp window of 2s)
- [ ] Ignored paths from `.gitignore` obviously excluded
**Dependencies:** cavekit-types-state-events

### R9: Done signal
**Description:** Determine when the orchestrator's work is complete.
**Acceptance Criteria:**
- [ ] Done signals, in priority order:
  1. Engine emits `Stop` or `SessionEnd` AND all observed tasks in impl-tracking are `DONE` → emit `Done { outcome: Success, artifacts }`
  2. Engine `Stop` but some tasks still PENDING → wait for potential re-activation (up to 60s), then emit `Done { outcome: Failed { reason: "incomplete" } }`
  3. Supervisor cancel (`world.cancel`): close spawned tabs, emit `Done { outcome: Killed }`, return
- [ ] `artifacts` = list of modified files (from FileEdited events) trimmed to unique set
- [ ] On done, orchestrator waits for all spawned child tabs (review) to finalize before returning
- [ ] After `run` returns, supervisor closes the builder tab per `config.defaults.auto_close_on_done|fail`
**Dependencies:** R3-R8

## Orchestrator structure sketch
```rust
#[async_trait]
impl Orchestrator for CavekitOrchestrator {
    fn name(&self) -> &'static str { "cavekit" }
    fn engine(&self) -> &'static str { "claude-code" }

    fn detect(cwd: &Path) -> bool { /* R1 */ }

    async fn run(&self, spec: OrchestratorSpec, world: World) -> Result<Outcome> {
        let builder = self.open_builder_tab(&spec, &world).await?;

        let mut tasks = JoinSet::new();
        tasks.spawn(self.watch_impl_tracking(&spec.cwd, world.events.clone()));
        tasks.spawn(self.watch_ralph_loop(&spec.cwd, world.events.clone()));
        tasks.spawn(self.watch_review_findings(&spec.cwd, world.events.clone(), spec.id.clone()));
        tasks.spawn(self.watch_phase_for_review_tab(&spec, world.clone(), builder.clone()));
        tasks.spawn(self.watch_git_diff(&spec.cwd, world.events.clone()));

        let outcome = self.await_done(&spec, &world).await?;
        self.await_children(&mut tasks, world.cancel.clone()).await?;

        Ok(outcome)
    }
}
```

## Out of Scope
- Modifying cavekit upstream — pure observation
- Driving cavekit commands (`/ck:make`) from ark — user does that inside the claude pane
- Supporting cavekit below a certain version (cutoff documented in release notes when we observe breakage)
- Ralph convergence analysis — ark surfaces signals; human decides
- Running multiple cavekit orchestrators in the same cwd — one per worktree

## Cross-References
- cavekit-architecture.md R2 — Orchestrator trait
- cavekit-engine-claude-code.md — coupled engine
- cavekit-layouts.md — `builder.kdl` and `review.kdl` shipped defaults
- cavekit-orchestrator-claude-code.md — minimal sibling; shares claude-code engine
- cavekit-config.md — `[orchestrator.cavekit]` section
